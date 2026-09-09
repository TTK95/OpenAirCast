// ===========================================================================
// Task 8: single registry, immutable snapshots, conservative health
//
// Design: exactly one dedicated consumer thread owns the event ring, session
// registrations, definite-counter baselines, and snapshot sequence. It reads
// the backend diagnostic feed through a blocking `DiagnosticEventReceiver`
// (driven by a tiny current-thread Tokio runtime so lag surfaces as typed
// QueueLag payloads) multiplexed with a bounded lifecycle command channel.
// Every consumed change rebuilds one owned `DiagnosticsSnapshot` and swaps it
// into an `ArcSwap`, so readers (`DiagnosticsHandle::snapshot`) are never
// blocked by writers. `events_since` delegates to the same shared ring.
// ===========================================================================

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{sync_channel, Receiver as CommandReceiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use airplay_client::{
    ClientDiagnosticError as ClientRowError, ClientDiagnosticErrorKind, ClientDiagnosticsSource,
    ClientTimingRole, DeviceId,
};
use arc_swap::ArcSwap;
use tokio::sync::{broadcast, Notify};

use crate::backend::event::{
    DiagnosticCategory as BackendCategory, DiagnosticEvent as BackendFeedEvent,
    DiagnosticEventReceiver, Severity as BackendSeverity,
};
use crate::backend::model::{ReceiverId, ReceiverLifecycle, SessionPhase};

use airplay_client::ClientDiagnosticKind;

use super::export::SupportExportInput;
use super::ring::DiagnosticsRing;
use super::snapshot::{
    BackendDiagnosticPayload, DefiniteCounterKind, DiagnosticComponent, DiagnosticError,
    DiagnosticErrorCode, DiagnosticEventDraft, DiagnosticSeverity, DiagnosticsClock,
    DiagnosticsSnapshot, EventBatch, EventCursor, Health, ReceiverDiagnosticsSnapshot,
    ReceiverSessionKey, ReceiverTimingSnapshot, ReceiverTimingSource, ReceiverTransportSnapshot,
    Recoverability, SessionDiagnosticsSnapshot, SessionId, SessionRegistration, SessionStopReason,
    StructuredDiagnosticPayload, DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
};

struct HandleInner {
    current: ArcSwap<DiagnosticsSnapshot>,
    ring: Arc<DiagnosticsRing>,
}

/// Cloneable, synchronous, read-only diagnostics surface for the UI.
#[derive(Clone)]
pub struct DiagnosticsHandle {
    inner: Arc<HandleInner>,
}

impl DiagnosticsHandle {
    /// Loads the latest published immutable snapshot without blocking.
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        Arc::unwrap_or_clone(self.inner.current.load_full())
    }

    /// Reads up to the clamped limit of events after `cursor` from the same
    /// ring the registry ingests into.
    pub fn events_since(&self, cursor: EventCursor, limit: usize) -> EventBatch {
        self.inner.ring.events_since(cursor, limit)
    }
}

enum Command {
    Register(SessionRegistration),
    Finish(SessionId, SessionStopReason),
}

/// Sole application-level diagnostics aggregator.
///
/// Spawns exactly one background consumer that owns all mutable registry
/// state; public methods only enqueue lifecycle commands. Dropping the
/// registry shuts the consumer down after draining pending commands.
pub struct DiagnosticsRegistry {
    commands: SyncSender<Command>,
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Read-only view of the same published snapshot and ring the consumer
    /// owns, so [`Self::support_export_input`] never has to ask the worker.
    shared: Arc<HandleInner>,
}

impl DiagnosticsRegistry {
    /// Starts the registry consumer thread.
    ///
    /// Returns the registry handle for lifecycle commands plus the shared
    /// read handle. The initial empty snapshot is published synchronously so
    /// the returned handle is immediately usable.
    pub fn start(
        backend_events: DiagnosticEventReceiver,
        clock: Arc<dyn DiagnosticsClock>,
    ) -> (Self, DiagnosticsHandle) {
        let shared = Arc::new(HandleInner {
            current: ArcSwap::from_pointee(DiagnosticsSnapshot {
                schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
                snapshot_sequence: 0,
                captured_at_utc: clock.now_utc(),
                process_elapsed_ns: clock.elapsed_ns(),
                active_session_id: None,
                health: Health::Unknown,
                session: None,
                receivers: Vec::new(),
                diagnostics_events_dropped_total: 0,
            }),
            ring: Arc::new(DiagnosticsRing::new()),
        });
        let (commands, command_rx) = sync_channel::<Command>(256);
        let wake = Arc::new(Notify::new());
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut state = WorkerState::new(Arc::clone(&clock), Arc::clone(&shared));
        state.publish();

        let worker_wake = Arc::clone(&wake);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = std::thread::Builder::new()
            .name("diagnostics-registry".to_owned())
            .spawn(move || {
                run_registry_worker(
                    state,
                    backend_events,
                    command_rx,
                    worker_wake,
                    worker_shutdown,
                )
            })
            .expect("spawning the diagnostics registry thread must succeed");

        (
            Self {
                commands,
                wake,
                shutdown,
                worker: Mutex::new(Some(worker)),
                shared: Arc::clone(&shared),
            },
            DiagnosticsHandle { inner: shared },
        )
    }

    /// Copies everything one support export needs in one short read.
    ///
    /// The latest published snapshot comes from the same `ArcSwap` the UI
    /// reads, and the retained events plus their overwrite gap come from one
    /// critical section on the shared ring, so no writer is ever blocked and
    /// no partial page can be stitched together across ticks.
    pub fn support_export_input(&self) -> SupportExportInput {
        let latest_snapshot = Arc::unwrap_or_clone(self.shared.current.load_full());
        let (retained_events, event_gap) = self.shared.ring.export_batch();
        let producer_drops_total = latest_snapshot.diagnostics_events_dropped_total;
        SupportExportInput {
            latest_snapshot,
            retained_events,
            event_gap,
            producer_drops_total,
        }
    }

    /// Registers the active session, replacing any previous registration
    /// (the resilience layer always finishes before registering anew).
    ///
    /// Best-effort: if the registry is shutting down the call is a no-op.
    pub fn register_session(&self, registration: SessionRegistration) {
        self.send(Command::Register(registration));
    }

    /// Marks the session finished and retains its final view for Speakers.
    pub fn finish_session(&self, session_id: SessionId, reason: SessionStopReason) {
        self.send(Command::Finish(session_id, reason));
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.wake.notify_one();
        }
    }

    /// Test seam: builds a bounded backend feed pair without touching the
    /// backend module's own constructors.
    #[doc(hidden)]
    pub fn test_diagnostic_feed(
        capacity: usize,
    ) -> (broadcast::Sender<BackendFeedEvent>, DiagnosticEventReceiver) {
        let (tx, rx) = broadcast::channel(capacity.max(1));
        (tx, DiagnosticEventReceiver::new(rx))
    }
}

impl Drop for DiagnosticsRegistry {
    fn drop(&mut self) {
        self.shutdown.store(true, AtomicOrdering::Relaxed);
        self.wake.notify_one();
        if let Some(worker) = self.worker.lock().expect("registry worker lock").take() {
            let _ = worker.join();
        }
    }
}

fn run_registry_worker(
    mut state: WorkerState,
    mut feed: DiagnosticEventReceiver,
    commands: CommandReceiver<Command>,
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async {
        let mut feed_closed = false;
        loop {
            // Drain every pending lifecycle command first, then publish once.
            let mut applied = false;
            while let Ok(command) = commands.try_recv() {
                state.apply_command(command);
                applied = true;
            }
            if applied {
                state.publish();
            }
            if shutdown.load(AtomicOrdering::Relaxed) {
                break;
            }

            tokio::select! {
                _ = wake.notified() => {}
                received = async {
                    if feed_closed {
                        std::future::pending::<Result<BackendFeedEvent, crate::backend::event::DiagnosticRecvError>>().await
                    } else {
                        feed.recv().await
                    }
                } => match received {
                    Ok(event) => {
                        state.ingest_backend_event(event);
                        state.publish();
                    }
                    Err(crate::backend::event::DiagnosticRecvError::Closed) => {
                        // Keep serving snapshots and commands without a feed.
                        feed_closed = true;
                    }
                },
            }
        }
        // Final drain so shutdown never loses a just-sent command.
        while let Ok(command) = commands.try_recv() {
            state.apply_command(command);
        }
        state.publish();
    });
}

/// Definite-counter last-seen values observed for the active session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DefiniteBaseline {
    capture_queue_full_drops: u64,
    buffer_underruns: u64,
    udp_send_failures: u64,
    retransmit_history_misses: u64,
}

struct ActiveSession {
    registration: SessionRegistration,
}

/// Retained view of the most recently completed session.
struct RetainedSession {
    session: SessionDiagnosticsSnapshot,
    receivers: Vec<ReceiverDiagnosticsSnapshot>,
}

/// All mutable registry state; owned exclusively by the consumer thread.
struct WorkerState {
    clock: Arc<dyn DiagnosticsClock>,
    shared: Arc<HandleInner>,
    sequence: u64,
    dropped_total: u64,
    active: Option<ActiveSession>,
    retained: Option<RetainedSession>,
    lifecycle: BTreeMap<ReceiverId, ReceiverLifecycle>,
    phase: Option<SessionPhase>,
    attention_definite: bool,
    feedback_attention: bool,
    pcm_drop_accumulator: u64,
    definite_baseline: Option<DefiniteBaseline>,
}

impl WorkerState {
    fn new(clock: Arc<dyn DiagnosticsClock>, shared: Arc<HandleInner>) -> Self {
        Self {
            clock,
            shared,
            sequence: 0,
            dropped_total: 0,
            active: None,
            retained: None,
            lifecycle: BTreeMap::new(),
            phase: None,
            attention_definite: false,
            feedback_attention: false,
            pcm_drop_accumulator: 0,
            definite_baseline: None,
        }
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            Command::Register(registration) => {
                self.active = Some(ActiveSession { registration });
                self.retained = None;
                self.lifecycle.clear();
                self.phase = None;
                self.attention_definite = false;
                self.feedback_attention = false;
                self.pcm_drop_accumulator = 0;
                self.definite_baseline = None;
            }
            Command::Finish(session_id, reason) => {
                let matches_active = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.registration.session_id == session_id);
                if !matches_active {
                    return;
                }
                let active = self.active.take().expect("checked above");
                let finished_elapsed_ns = self.clock.elapsed_ns();
                let client = active.registration.source.snapshot(self.clock.elapsed_ns());
                let receivers = self.build_receiver_rows(&active.registration, &client);
                let session = SessionDiagnosticsSnapshot {
                    session_id: active.registration.session_id,
                    started_elapsed_ns: active.registration.started_elapsed_ns,
                    finished_elapsed_ns: Some(finished_elapsed_ns),
                    stop_reason: Some(reason),
                    phase: self.phase.clone(),
                    primary: active.registration.primary,
                    members: active.registration.members.clone(),
                    audio: client.audio.clone(),
                };
                self.retained = Some(RetainedSession { session, receivers });
            }
        }
    }

    fn ingest_backend_event(&mut self, event: BackendFeedEvent) {
        let BackendFeedEvent {
            monotonic_ns,
            wall_time,
            ref receiver,
            ref severity,
            ref category,
            ref payload,
            ..
        } = event;

        if let BackendDiagnosticPayload::QueueLag { dropped } = payload {
            self.dropped_total += dropped;
        }
        if let BackendDiagnosticPayload::PcmDrop { dropped } = payload {
            let previous = self.pcm_drop_accumulator;
            self.pcm_drop_accumulator = previous.saturating_add(*dropped);
            if self.active.is_some() && self.pcm_drop_accumulator > previous {
                self.record_definite_transition(
                    DefiniteCounterKind::CaptureDrops,
                    DiagnosticComponent::Capture,
                    DiagnosticErrorCode::CaptureQueueFull,
                    previous,
                    self.pcm_drop_accumulator,
                );
                self.attention_definite = true;
            }
        }
        if let BackendDiagnosticPayload::ReceiverTransition {
            receiver: changed,
            to,
        } = payload
        {
            if self.is_active_member(changed) {
                self.lifecycle.insert(*changed, to.clone());
            }
        }
        if let BackendDiagnosticPayload::SessionTransition { phase, .. } = payload {
            self.phase = Some(phase.clone());
        }

        let receiver_session = receiver.as_ref().and_then(|id| self.session_key_for(id));
        self.shared.ring.push(DiagnosticEventDraft {
            occurred_at_utc: wall_time,
            process_elapsed_ns: monotonic_ns,
            receiver_session,
            severity: map_backend_severity(*severity),
            component: backend_component_for(*category, receiver.is_some()),
            code: backend_code_for(payload),
            public_message: backend_public_message(payload),
            // Calibration has its own structured variant; wrapping it in
            // `Backend(..)` would hide a first-class lifecycle edge inside a
            // generic envelope.
            payload: match payload {
                BackendDiagnosticPayload::CalibrationState(state) => {
                    StructuredDiagnosticPayload::CalibrationState(*state)
                }
                other => StructuredDiagnosticPayload::Backend(other.clone()),
            },
        });
    }

    /// Drains the registered client source's bounded event feed into the
    /// shared ring, exactly once per event.
    fn drain_client_events(&mut self, source: &ClientDiagnosticsSource) {
        for _ in 0..16 {
            let drained = source.drain_events(512);
            if drained.is_empty() {
                return;
            }
            for event in drained {
                let receiver_session = event
                    .device_id
                    .as_ref()
                    .and_then(|device| self.device_key(device));
                let (severity, component, code, attention, message) = match event.kind {
                    ClientDiagnosticKind::Feedback => (
                        DiagnosticSeverity::Info,
                        DiagnosticComponent::Feedback,
                        DiagnosticErrorCode::Notice,
                        false,
                        "feedback transaction recorded",
                    ),
                    ClientDiagnosticKind::SetupFailure => (
                        DiagnosticSeverity::Warning,
                        DiagnosticComponent::Rtsp,
                        DiagnosticErrorCode::SetupRejected,
                        false,
                        "receiver setup failed",
                    ),
                    ClientDiagnosticKind::RuntimeWarning => (
                        DiagnosticSeverity::Warning,
                        DiagnosticComponent::Retransmit,
                        DiagnosticErrorCode::Notice,
                        true,
                        "recoverable transport problem",
                    ),
                };
                if attention {
                    self.attention_definite = true;
                }
                self.shared.ring.push(DiagnosticEventDraft {
                    occurred_at_utc: self.clock.now_utc(),
                    process_elapsed_ns: event.elapsed_ns,
                    receiver_session,
                    severity,
                    component,
                    code,
                    public_message: message.to_owned(),
                    payload: StructuredDiagnosticPayload::Client(event),
                });
            }
        }
    }

    /// Observes the client's shared audio counters against the last-seen
    /// baseline, recording one transition event per increased counter.
    fn observe_definite_counters(&mut self, client: &airplay_client::ClientDiagnosticsSnapshot) {
        let Some(audio) = client.audio.as_ref() else {
            return;
        };
        let current = DefiniteBaseline {
            capture_queue_full_drops: audio.capture_queue.full_queue_drops_total,
            buffer_underruns: audio.sender_buffer.underrun_events_total,
            udp_send_failures: audio
                .targets
                .iter()
                .map(|target| target.data_send_failures_total)
                .fold(0_u64, u64::saturating_add),
            retransmit_history_misses: audio.retransmit.retransmit_history_misses_total,
        };
        let Some(baseline) = self.definite_baseline else {
            // First observation seeds the baseline silently so pre-session
            // totals are never fabricated into transitions.
            self.definite_baseline = Some(current);
            return;
        };

        let increases = [
            (
                baseline.capture_queue_full_drops,
                current.capture_queue_full_drops,
                DefiniteCounterKind::CaptureDrops,
                DiagnosticComponent::Capture,
                DiagnosticErrorCode::CaptureQueueFull,
            ),
            (
                baseline.buffer_underruns,
                current.buffer_underruns,
                DefiniteCounterKind::BufferUnderruns,
                DiagnosticComponent::Buffer,
                DiagnosticErrorCode::BufferUnderrun,
            ),
            (
                baseline.udp_send_failures,
                current.udp_send_failures,
                DefiniteCounterKind::UdpSendFailures,
                DiagnosticComponent::UdpTransport,
                DiagnosticErrorCode::UdpSendFailure,
            ),
            (
                baseline.retransmit_history_misses,
                current.retransmit_history_misses,
                DefiniteCounterKind::RetransmitHistoryMisses,
                DiagnosticComponent::Retransmit,
                DiagnosticErrorCode::RetransmitHistoryMiss,
            ),
        ];
        for (previous, now, kind, component, code) in increases {
            if now > previous && self.active.is_some() {
                self.record_definite_transition(kind, component, code, previous, now);
                self.attention_definite = true;
            }
        }
        self.definite_baseline = Some(current);
    }

    fn record_definite_transition(
        &mut self,
        counter: DefiniteCounterKind,
        component: DiagnosticComponent,
        code: DiagnosticErrorCode,
        previous_total: u64,
        current_total: u64,
    ) {
        self.shared.ring.push(DiagnosticEventDraft {
            occurred_at_utc: self.clock.now_utc(),
            process_elapsed_ns: self.clock.elapsed_ns(),
            receiver_session: None,
            severity: DiagnosticSeverity::Warning,
            component,
            code,
            public_message: format!("{counter:?} total advanced"),
            payload: StructuredDiagnosticPayload::DefiniteCounterTransition {
                counter,
                previous_total,
                current_total,
            },
        });
    }

    fn build_receiver_rows(
        &self,
        registration: &SessionRegistration,
        client: &airplay_client::ClientDiagnosticsSnapshot,
    ) -> Vec<ReceiverDiagnosticsSnapshot> {
        let shared_frames_prepared = client
            .audio
            .as_ref()
            .map(|audio| audio.rtp_frames_prepared_total)
            .unwrap_or(0);

        registration
            .members
            .iter()
            .map(|(receiver_id, device_id)| {
                let key = ReceiverSessionKey {
                    session_id: registration.session_id,
                    receiver_id: *receiver_id,
                };
                let conn = client
                    .connections
                    .iter()
                    .find(|conn| &conn.device_id == device_id);

                let timing = match conn.map(|conn| conn.timing_role) {
                    Some(ClientTimingRole::PtpPrimary) => {
                        let values = conn.expect("role implies a connection").timing.clone();
                        ReceiverTimingSnapshot {
                            source: ReceiverTimingSource::PtpMeasuredAgainstThisMaster,
                            independently_measured: true,
                            offset_local_minus_master_ns: values
                                .latest
                                .as_ref()
                                .map(|m| m.offset_local_minus_master_ns()),
                            mean_path_delay_ns: values
                                .latest
                                .as_ref()
                                .map(|m| m.mean_path_delay_ns()),
                            drift_ppm: values.drift.as_ref().map(|d| d.drift_ppm),
                            sample_age_ns: values.sample_age_ns,
                            stale: values.stale,
                        }
                    }
                    Some(ClientTimingRole::PtpSecondary) => ReceiverTimingSnapshot {
                        source: ReceiverTimingSource::PtpSharedFromPrimary {
                            source_receiver_id: registration.primary,
                        },
                        independently_measured: false,
                        offset_local_minus_master_ns: None,
                        mean_path_delay_ns: None,
                        drift_ppm: None,
                        sample_age_ns: None,
                        stale: false,
                    },
                    role => ReceiverTimingSnapshot {
                        source: if conn.is_some() && matches!(role, Some(ClientTimingRole::Single))
                        {
                            ReceiverTimingSource::SenderReference
                        } else {
                            ReceiverTimingSource::Unavailable
                        },
                        independently_measured: false,
                        offset_local_minus_master_ns: None,
                        mean_path_delay_ns: None,
                        drift_ppm: None,
                        sample_age_ns: None,
                        stale: false,
                    },
                };

                let transport_row = conn.map(|conn| conn.transport).unwrap_or_default();
                let retransmit = conn.map(|conn| conn.retransmit).unwrap_or_default();
                let requested_slots = retransmit.retransmit_packet_slots_requested_total;
                let transport = ReceiverTransportSnapshot {
                    data_datagrams_attempted_total: transport_row.data_datagrams_attempted_total,
                    data_datagrams_accepted_local_total: transport_row
                        .data_datagrams_accepted_local_total,
                    data_datagram_send_failures_total: transport_row.data_send_failures_total,
                    data_bytes_accepted_local_total: transport_row.data_bytes_sent_total,
                    sync_datagrams_attempted_total: transport_row.sync_datagrams_attempted_total,
                    sync_datagrams_accepted_local_total: transport_row
                        .sync_datagrams_accepted_local_total,
                    sync_datagram_send_failures_total: transport_row.sync_send_failures_total,
                    sync_bytes_accepted_local_total: transport_row.sync_bytes_sent_total,
                    retransmit_slots_requested_total: requested_slots,
                    retransmit_slots_accepted_local_total: retransmit
                        .retransmit_datagrams_accepted_local_total,
                    recovery_demand_numerator_slots: requested_slots,
                    recovery_demand_denominator_shared_frames: shared_frames_prepared,
                    recovery_demand_ratio: (shared_frames_prepared > 0)
                        .then(|| requested_slots as f64 / shared_frames_prepared as f64),
                };

                let last_error = conn.and_then(|conn| {
                    conn.last_error
                        .as_ref()
                        .map(|error| client_error_to_diagnostic(error, key.clone()))
                });

                ReceiverDiagnosticsSnapshot {
                    key,
                    lifecycle: self
                        .lifecycle
                        .get(receiver_id)
                        .cloned()
                        .unwrap_or(ReceiverLifecycle::Discovered),
                    timing,
                    transport,
                    feedback: conn.map(|conn| conn.feedback.clone()).unwrap_or_default(),
                    last_error,
                }
            })
            .collect()
    }

    fn compute_health(&self) -> Health {
        if self.active.is_none() {
            // Spec health naming: no current session maps to Unknown, even
            // though a stopped session stays retained for Speakers.
            return Health::Unknown;
        }
        let authoritative_failure = self
            .lifecycle
            .values()
            .any(|state| matches!(state, ReceiverLifecycle::Failed { .. }))
            || matches!(self.phase, Some(SessionPhase::Failed { .. }));
        if authoritative_failure {
            return Health::Error;
        }
        let degraded = matches!(self.phase, Some(SessionPhase::Degraded { .. }))
            || self.attention_definite
            || self.feedback_attention;
        if degraded {
            return Health::Attention;
        }
        Health::Running
    }

    fn publish(&mut self) {
        let captured_at_utc = self.clock.now_utc();
        let process_elapsed_ns = self.clock.elapsed_ns();

        let client_source = self
            .active
            .as_ref()
            .map(|active| active.registration.source.clone());
        let mut receivers = Vec::new();
        let mut session = None;

        if let Some(source) = &client_source {
            self.drain_client_events(source);
            let client = source.snapshot(process_elapsed_ns);
            self.observe_definite_counters(&client);
            let feedback_failed_now = client.connections.iter().any(|conn| {
                matches!(
                    conn.feedback.last_result,
                    Some(
                        airplay_client::FeedbackResultKind::ProtocolFailure
                            | airplay_client::FeedbackResultKind::TransportFailure
                            | airplay_client::FeedbackResultKind::Timeout
                    )
                )
            });
            if feedback_failed_now {
                self.feedback_attention = true;
            }
            if let Some(active) = self.active.as_ref() {
                receivers = self.build_receiver_rows(&active.registration, &client);
                session = Some(SessionDiagnosticsSnapshot {
                    session_id: active.registration.session_id,
                    started_elapsed_ns: active.registration.started_elapsed_ns,
                    finished_elapsed_ns: None,
                    stop_reason: None,
                    phase: self.phase.clone(),
                    primary: active.registration.primary,
                    members: active.registration.members.clone(),
                    audio: client.audio.clone(),
                });
            }
        } else if let Some(retained) = &self.retained {
            receivers = retained.receivers.clone();
            session = Some(retained.session.clone());
        }

        let health = self.compute_health();
        self.sequence += 1;
        self.shared.current.store(Arc::new(DiagnosticsSnapshot {
            schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
            snapshot_sequence: self.sequence,
            captured_at_utc,
            process_elapsed_ns,
            active_session_id: self
                .active
                .as_ref()
                .map(|active| active.registration.session_id),
            health,
            session,
            receivers,
            diagnostics_events_dropped_total: self.dropped_total,
        }));
    }

    fn is_active_member(&self, receiver_id: &ReceiverId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.registration.members.contains_key(receiver_id))
    }

    fn session_key_for(&self, receiver_id: &ReceiverId) -> Option<ReceiverSessionKey> {
        self.active.as_ref().and_then(|active| {
            if active.registration.members.contains_key(receiver_id) {
                Some(ReceiverSessionKey {
                    session_id: active.registration.session_id,
                    receiver_id: *receiver_id,
                })
            } else {
                None
            }
        })
    }

    fn device_key(&self, device_id: &DeviceId) -> Option<ReceiverSessionKey> {
        self.active.as_ref().and_then(|active| {
            active
                .registration
                .members
                .iter()
                .find(|(_, member_device)| *member_device == device_id)
                .map(|(receiver_id, _)| ReceiverSessionKey {
                    session_id: active.registration.session_id,
                    receiver_id: *receiver_id,
                })
        })
    }
}

fn map_backend_severity(severity: BackendSeverity) -> DiagnosticSeverity {
    match severity {
        BackendSeverity::Info => DiagnosticSeverity::Info,
        BackendSeverity::Warning => DiagnosticSeverity::Warning,
        BackendSeverity::Error => DiagnosticSeverity::Error,
    }
}

/// Coarse component routing for wrapped backend events. The wrapped payload
/// stays authoritative; this only labels the Events page filter column.
fn backend_component_for(category: BackendCategory, receiver_scoped: bool) -> DiagnosticComponent {
    match category {
        BackendCategory::Discovery => DiagnosticComponent::Discovery,
        BackendCategory::Capture => DiagnosticComponent::Capture,
        BackendCategory::Probe => DiagnosticComponent::Feedback,
        BackendCategory::Retry => {
            if receiver_scoped {
                DiagnosticComponent::Rtsp
            } else {
                DiagnosticComponent::Discovery
            }
        }
        BackendCategory::Receiver | BackendCategory::Session => DiagnosticComponent::Rtsp,
        BackendCategory::Command
        | BackendCategory::General
        | BackendCategory::Persistence
        | BackendCategory::Queue
        | BackendCategory::Worker => DiagnosticComponent::Backend,
    }
}

/// Coarse failure-class routing for wrapped backend events. Only payloads
/// whose class is unambiguous get a specific code; everything else carries
/// [`DiagnosticErrorCode::Notice`] so no failure is claimed that the payload
/// does not state.
fn backend_code_for(payload: &BackendDiagnosticPayload) -> DiagnosticErrorCode {
    match payload {
        BackendDiagnosticPayload::PcmDrop { .. } => DiagnosticErrorCode::CaptureQueueFull,
        BackendDiagnosticPayload::QueueLag { .. } => DiagnosticErrorCode::EventChannelFailure,
        BackendDiagnosticPayload::Probe { healthy: false, .. } => {
            DiagnosticErrorCode::FeedbackFailure
        }
        _ => DiagnosticErrorCode::Notice,
    }
}

/// User-safe message derived from the typed backend payload.
///
/// Exhaustive on purpose: a catch-all would let a new payload variant reach
/// the export as "backend diagnostic", which says nothing and is therefore
/// worse than no record. Every arm restates only what its own payload holds,
/// so none of them can name an address, a path, or a credential.
fn backend_public_message(payload: &BackendDiagnosticPayload) -> String {
    match payload {
        BackendDiagnosticPayload::Message(text) => text.clone(),
        BackendDiagnosticPayload::DiscoveryGeneration { generation, phase } => {
            format!("discovery generation {generation} is {phase:?}")
        }
        BackendDiagnosticPayload::DiscoveryReceiver { present, .. } => {
            if *present {
                "a receiver appeared in discovery".to_owned()
            } else {
                "a receiver disappeared from discovery".to_owned()
            }
        }
        BackendDiagnosticPayload::CommandResult { id, error } => match error {
            Some(error) => format!("command {id} failed: {error}"),
            None => format!("command {id} completed"),
        },
        BackendDiagnosticPayload::CommandCoalesced { dropped } => {
            format!("{dropped} superseded commands were merged away")
        }
        BackendDiagnosticPayload::ReceiverTransition { to, .. } => {
            format!("receiver entered {to:?}")
        }
        BackendDiagnosticPayload::SetupPhaseDuration {
            phase, duration, ..
        } => format!("setup phase {phase:?} took {} ms", duration.as_millis()),
        BackendDiagnosticPayload::SessionTransition {
            phase,
            active_members,
        } => format!("session entered {phase:?} with {active_members} active receivers"),
        BackendDiagnosticPayload::SessionRestart { generation, reason } => {
            format!("session generation {generation} restarts: {reason:?}")
        }
        BackendDiagnosticPayload::Retry {
            receiver, attempt, ..
        } => match receiver {
            Some(_) => format!("reconnect attempt {attempt} scheduled for a receiver"),
            None => format!("retry attempt {attempt} scheduled"),
        },
        BackendDiagnosticPayload::CaptureTransition { state } => {
            format!("audio capture is {state:?}")
        }
        BackendDiagnosticPayload::PcmDrop { dropped } => {
            format!("capture bridge dropped {dropped} frames")
        }
        BackendDiagnosticPayload::QueueLag { dropped } => {
            format!("diagnostic feed lagged; {dropped} events lost")
        }
        BackendDiagnosticPayload::SilenceBridge { duration } => {
            format!("bridge paced silence for {} ms", duration.as_millis())
        }
        BackendDiagnosticPayload::QueueWatermark { depth, capacity } => {
            format!("queue reached {depth}/{capacity} entries")
        }
        BackendDiagnosticPayload::Probe {
            healthy,
            latency_ms,
        } => {
            if *healthy {
                format!("probe answered in {latency_ms} ms")
            } else {
                "probe failed".to_owned()
            }
        }
        BackendDiagnosticPayload::Persistence { outcome } => {
            format!("saving device state is {outcome:?}")
        }
        BackendDiagnosticPayload::WorkerShutdown {
            duration,
            timed_out,
        } => {
            if *timed_out {
                format!(
                    "shutdown ran out of its budget after {} ms",
                    duration.as_millis()
                )
            } else {
                format!("shutdown completed in {} ms", duration.as_millis())
            }
        }
        BackendDiagnosticPayload::SystemTransition { transition } => {
            format!("platform edge applied: {transition:?}")
        }
        BackendDiagnosticPayload::CalibrationState(state) => {
            format!("speaker calibration is {state:?}")
        }
    }
}

/// Maps a classified client error onto the structured application error.
///
/// The classification today comes from feedback transactions and connection
/// failures, so the coarse codes reflect those producers; the original
/// message text is preserved verbatim.
fn client_error_to_diagnostic(error: &ClientRowError, key: ReceiverSessionKey) -> DiagnosticError {
    let (code, component) = match error.kind {
        ClientDiagnosticErrorKind::Timeout => (
            DiagnosticErrorCode::FeedbackTimeout,
            DiagnosticComponent::Feedback,
        ),
        ClientDiagnosticErrorKind::ProtocolFailure => (
            DiagnosticErrorCode::FeedbackFailure,
            DiagnosticComponent::Feedback,
        ),
        ClientDiagnosticErrorKind::TransportFailure => (
            DiagnosticErrorCode::UdpSendFailure,
            DiagnosticComponent::UdpTransport,
        ),
        ClientDiagnosticErrorKind::Other => {
            (DiagnosticErrorCode::Notice, DiagnosticComponent::Backend)
        }
    };
    DiagnosticError {
        code,
        component,
        severity: DiagnosticSeverity::Error,
        recoverability: Recoverability::AutomaticRetry,
        operation: "client_connection",
        receiver_session: Some(key),
        public_message: error.public_message.clone(),
        technical_detail: None,
    }
}
