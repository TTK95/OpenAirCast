//! Low-volume shell events and the bounded diagnostic feed.
//!
//! [`BackendEvent`] carries only shell-visible edges; snapshots stay
//! authoritative when its broadcast lags. [`DiagnosticEvent`] is the typed,
//! redaction-safe feed consumed by the diagnostics registry, and
//! [`DiagnosticEventReceiver`] converts its own lag into a synthetic
//! `QueueLag` loss event instead of surfacing transport errors.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{broadcast, watch};

use crate::diagnostics::{CalibrationApplyState, SessionDiagnosticsState};

use crate::backend::model::{
    AudioSourceState, DeviceSnapshot, DiscoveryPhase, PersistenceSnapshot, ReceiverId,
    ReceiverLifecycle, RestartReason, SessionPhase, SetupPhase, UserFacingError,
};

/// Fixed capacity of the low-volume shell event broadcast.
pub const BACKEND_EVENT_CAPACITY: usize = 256;
/// Fixed capacity of the diagnostic ring consumed by subproject 3.
pub const DIAGNOSTIC_CAPACITY: usize = 2_048;
/// Fixed per-supervisor capacity for supervisor-to-actor events.
pub const SUPERVISOR_CAPACITY: usize = 256;
/// Fixed capacity of the newest-data live PCM bridge in frames.
pub const PCM_CAPACITY: usize = 32;

/// Presentation severity of a backend notice or diagnostic event.
///
/// This is the backend-owned copy of the concept; the shell's UI types are
/// intentionally separate so backend semantics never leak into app state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Purely informational.
    Info,
    /// Degraded but recovering; user attention optional.
    Warning,
    /// An operation failed in a way the user should know about.
    Error,
}

/// Stable machine-readable code carried by [`BackendEvent::Notice`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeCode {
    /// Discovery supervision could not maintain a browse generation.
    DiscoveryFailed,
    /// The session could not be kept running after best-effort recovery.
    SessionFailed,
    /// No usable capture source is currently available.
    CaptureUnavailable,
    /// Persisting device state failed and the change was therefore not
    /// applied; the previous durable and in-memory state both stand.
    PersistenceWrite,
    /// A lifecycle queue rejected work because it is overloaded.
    QueueOverloaded,
}

/// Backend domain a notice belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorScope {
    /// Discovery supervision and receiver inventory.
    Discovery,
    /// AirPlay session setup, streaming, and recovery.
    Session,
    /// Windows capture and the live PCM bridge.
    Capture,
    /// Durable device-state persistence.
    Persistence,
    /// Internal lifecycle queues.
    Queue,
}

/// Low-volume shell notifications; snapshots stay authoritative if this feed
/// lags and the shell refreshes from the watch channel instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendEvent {
    /// The command with this ID was applied successfully.
    CommandCompleted {
        /// Handle-allocated ID echoed from the command.
        id: u64,
    },
    /// The command with this ID was rejected or failed.
    CommandFailed {
        /// Handle-allocated ID echoed from the command.
        id: u64,
        /// Pre-redacted, shell-safe reason.
        error: UserFacingError,
    },
    /// A user-visible condition that is not tied to one command.
    Notice {
        /// Presentation severity.
        severity: Severity,
        /// Stable machine-readable code.
        code: NoticeCode,
        /// Backend domain responsible for the notice.
        scope: ErrorScope,
        /// Pre-redacted human-readable message.
        message: String,
    },
}

/// Coarse domain of a diagnostic event, derived from its payload kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCategory {
    /// Free-form redacted summaries.
    General,
    /// Discovery supervision and inventory changes.
    Discovery,
    /// Command acceptance, coalescing, completion, and failure.
    Command,
    /// Per-receiver lifecycle and setup progress.
    Receiver,
    /// Whole-session phases and restarts.
    Session,
    /// Scheduled retries for receivers or discovery.
    Retry,
    /// Capture state, PCM drops, and silence bridging.
    Capture,
    /// Per-receiver feedback probe results.
    Probe,
    /// Durable state load, write, migration, and recovery.
    Persistence,
    /// Queue watermarks and lag losses.
    Queue,
    /// Worker cancellations and shutdown durations.
    Worker,
}

/// Fine-grained identity of a [`DiagnosticPayload`] variant.
///
/// One-to-one with payload variants so consumers can filter without matching
/// payload contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    /// Pre-redacted free-form summary.
    Message,
    /// Discovery generation start/stop/error edge.
    DiscoveryGeneration,
    /// Receiver added to or removed from discovery inventory.
    DiscoveryReceiver,
    /// Command completed or failed.
    CommandResult,
    /// Commands merged away during pre-setup coalescing.
    CommandCoalesced,
    /// Receiver lifecycle transition destination.
    ReceiverTransition,
    /// Measured duration of one setup phase.
    SetupPhaseDuration,
    /// Whole-session phase transition destination.
    SessionTransition,
    /// Announced full-session restart.
    SessionRestart,
    /// Retry scheduled for a receiver or discovery generation.
    Retry,
    /// Capture availability transition.
    CaptureTransition,
    /// Live PCM frames dropped by the newest-data bridge.
    PcmDrop,
    /// Silence paced by the bridge instead of live capture.
    SilenceBridge,
    /// Per-receiver feedback probe result.
    Probe,
    /// Persistence operation outcome.
    Persistence,
    /// Observed queue high-water mark.
    QueueWatermark,
    /// Typed gap after dropped diagnostic events.
    QueueLag,
    /// Worker cancellation shutdown duration.
    WorkerShutdown,
    /// Platform sleep or network transition applied by the controller.
    SystemTransition,
    /// Manual calibration apply lifecycle edge.
    CalibrationState,
}

/// Platform edge the controller acted on.
///
/// Carries verdicts, never addresses: the network monitor is the only place
/// that sees local IP addresses and it does not export them.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SystemTransition {
    /// Suspend accepted; capture and the session are being stopped.
    Suspending,
    /// The resume interface-settle period elapsed and generations were
    /// recreated.
    Resumed,
    /// One coalesced network-change burst was applied.
    NetworkChanged {
        /// Whether an active local binding/address moved.
        local_binding_changed: bool,
    },
}

/// Typed, presentation-safe diagnostic content.
///
/// Every variant stores only stable IDs, enum states, counts, durations,
/// retry attempt/deadline values, resolved latency numbers, and pre-redacted
/// error summaries. Raw pairing material, protocol payload bytes, credentials,
/// and Windows user paths have no fields here by construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticPayload {
    /// Pre-redacted user-safe summary text only.
    Message(String),
    /// Discovery supervisor generation lifecycle edge.
    DiscoveryGeneration {
        /// Generation the edge belongs to.
        generation: u64,
        /// Supervisor phase reached.
        phase: DiscoveryPhase,
    },
    /// A receiver entered or left the discovery inventory.
    DiscoveryReceiver {
        /// Stable receiver identity.
        receiver: ReceiverId,
        /// Whether the receiver is now present.
        present: bool,
    },
    /// Command completed or failed; `None` marks success.
    CommandResult {
        /// Handle-allocated command ID.
        id: u64,
        /// Pre-redacted failure summary, if any.
        error: Option<UserFacingError>,
    },
    /// Commands merged away during coalescing before reconciliation.
    CommandCoalesced {
        /// How many commands were superseded.
        dropped: u32,
    },
    /// Receiver lifecycle transition destination.
    ReceiverTransition {
        /// Stable receiver identity.
        receiver: ReceiverId,
        /// Lifecycle state entered.
        to: ReceiverLifecycle,
    },
    /// Measured duration of one transport-reported setup phase.
    SetupPhaseDuration {
        /// Stable receiver identity.
        receiver: ReceiverId,
        /// Phase that completed.
        phase: SetupPhase,
        /// Time spent in the phase.
        duration: Duration,
    },
    /// Whole-session phase transition destination.
    SessionTransition {
        /// Phase entered, including its generation tag.
        phase: SessionPhase,
        /// Receivers active when the transition committed.
        active_members: u32,
    },
    /// Announced full-session restart.
    SessionRestart {
        /// Generation being torn down and replaced.
        generation: u64,
        /// Matrix reason driving the restart.
        reason: RestartReason,
    },
    /// Retry scheduled for a receiver or for a discovery generation.
    Retry {
        /// Target receiver, or none for discovery-level retries.
        receiver: Option<ReceiverId>,
        /// Zero-based attempt number that will be performed.
        attempt: u32,
        /// Wall-clock deadline of the pending retry.
        next_at: SystemTime,
    },
    /// Capture availability transition destination.
    CaptureTransition {
        /// Availability/capture-recovery state entered.
        state: AudioSourceState,
    },
    /// Live PCM frames dropped by the newest-data bridge.
    PcmDrop {
        /// Number of frames dropped since the last report.
        dropped: u64,
    },
    /// Duration the bridge paced silence instead of live capture.
    SilenceBridge {
        /// Accumulated silence interval.
        duration: Duration,
    },
    /// Per-receiver feedback probe result.
    Probe {
        /// Whether the receiver answered within bounds.
        healthy: bool,
        /// Resolved control-request latency in milliseconds.
        latency_ms: u64,
    },
    /// Persistence operation outcome.
    Persistence {
        /// Health of the latest persistence cycle.
        outcome: PersistenceSnapshot,
    },
    /// Observed queue high-water mark.
    QueueWatermark {
        /// Deepest observed occupancy.
        depth: u32,
        /// Fixed capacity of the observed queue.
        capacity: u32,
    },
    /// Typed gap replacing events lost to receiver lag.
    QueueLag {
        /// Number of diagnostic events dropped.
        dropped: u64,
    },
    /// Worker cancellation shutdown duration.
    WorkerShutdown {
        /// Time from cancellation signal to joined workers.
        duration: Duration,
        /// Whether at least one bounded drain hit its budget instead of
        /// finishing. A duration alone cannot say this: a teardown that ran
        /// into two four-second budgets and one that simply took long look
        /// identical without it.
        timed_out: bool,
    },
    /// Platform sleep or network transition applied by the controller.
    SystemTransition {
        /// Edge that was applied.
        transition: SystemTransition,
    },
    /// Manual calibration apply lifecycle edge.
    ///
    /// Carries the lifecycle state only. The profile, its receivers, and its
    /// values are user configuration, not diagnostic material, and never
    /// enter a record.
    CalibrationState(CalibrationApplyState),
}

impl DiagnosticPayload {
    /// Returns the fine-grained kind of this payload.
    pub fn kind(&self) -> DiagnosticKind {
        match self {
            Self::Message(_) => DiagnosticKind::Message,
            Self::DiscoveryGeneration { .. } => DiagnosticKind::DiscoveryGeneration,
            Self::DiscoveryReceiver { .. } => DiagnosticKind::DiscoveryReceiver,
            Self::CommandResult { .. } => DiagnosticKind::CommandResult,
            Self::CommandCoalesced { .. } => DiagnosticKind::CommandCoalesced,
            Self::ReceiverTransition { .. } => DiagnosticKind::ReceiverTransition,
            Self::SetupPhaseDuration { .. } => DiagnosticKind::SetupPhaseDuration,
            Self::SessionTransition { .. } => DiagnosticKind::SessionTransition,
            Self::SessionRestart { .. } => DiagnosticKind::SessionRestart,
            Self::Retry { .. } => DiagnosticKind::Retry,
            Self::CaptureTransition { .. } => DiagnosticKind::CaptureTransition,
            Self::PcmDrop { .. } => DiagnosticKind::PcmDrop,
            Self::SilenceBridge { .. } => DiagnosticKind::SilenceBridge,
            Self::Probe { .. } => DiagnosticKind::Probe,
            Self::Persistence { .. } => DiagnosticKind::Persistence,
            Self::QueueWatermark { .. } => DiagnosticKind::QueueWatermark,
            Self::QueueLag { .. } => DiagnosticKind::QueueLag,
            Self::WorkerShutdown { .. } => DiagnosticKind::WorkerShutdown,
            Self::SystemTransition { .. } => DiagnosticKind::SystemTransition,
            Self::CalibrationState(_) => DiagnosticKind::CalibrationState,
        }
    }
}

/// Coarse domain associated with a diagnostic kind.
fn category_of_kind(kind: DiagnosticKind) -> DiagnosticCategory {
    match kind {
        DiagnosticKind::Message => DiagnosticCategory::General,
        DiagnosticKind::DiscoveryGeneration | DiagnosticKind::DiscoveryReceiver => {
            DiagnosticCategory::Discovery
        }
        DiagnosticKind::CommandResult | DiagnosticKind::CommandCoalesced => {
            DiagnosticCategory::Command
        }
        DiagnosticKind::ReceiverTransition | DiagnosticKind::SetupPhaseDuration => {
            DiagnosticCategory::Receiver
        }
        DiagnosticKind::SessionTransition | DiagnosticKind::SessionRestart => {
            DiagnosticCategory::Session
        }
        DiagnosticKind::Retry => DiagnosticCategory::Retry,
        DiagnosticKind::CaptureTransition
        | DiagnosticKind::PcmDrop
        | DiagnosticKind::SilenceBridge => DiagnosticCategory::Capture,
        DiagnosticKind::Probe => DiagnosticCategory::Probe,
        DiagnosticKind::Persistence => DiagnosticCategory::Persistence,
        DiagnosticKind::QueueWatermark | DiagnosticKind::QueueLag => DiagnosticCategory::Queue,
        DiagnosticKind::WorkerShutdown | DiagnosticKind::SystemTransition => {
            DiagnosticCategory::Worker
        }
        // Calibration is a session-setup input and its apply is a session
        // edge; it needs no category of its own.
        DiagnosticKind::CalibrationState => DiagnosticCategory::Session,
    }
}

/// Process-relative monotonic timestamp in nanoseconds.
fn monotonic_ns_now() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The three generation counters that correlate one diagnostic record with
/// the browse, the capture worker, and the session it was written under.
///
/// Zero is not a generation: it means "none was live here". Discovery and
/// capture generations start at one, the session generation at one, so a zero
/// in a record says the emitting component genuinely had nothing to point at
/// -- never that the value was unavailable to it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Generations {
    /// Session generation live at emission, or zero while none runs.
    ///
    /// A stopped session returns this to zero; the controller's monotonic
    /// counter keeps its value underneath so the next session mints a
    /// strictly newer number. "Written under generation 5" and "written after
    /// generation 5 ended" are therefore distinguishable.
    pub session: u64,
    /// Discovery browse generation live at emission, or zero before the
    /// first browse started.
    pub discovery: u64,
    /// Capture generation live at emission, or zero while none runs.
    pub capture: u64,
}

/// Shared, lock-free view of the live generation counters.
///
/// Each counter is written by the component that owns it -- the controller
/// publishes session and capture, the discovery supervisor publishes its own
/// browse generation -- and read by everyone else, so a record can name the
/// counters it does not own. Without it a capture drop and a receiver failure
/// that happened in the same session would carry different-looking
/// correlation keys, and no consumer could line the two up.
///
/// Ownership decides the writer for a reason: routing the browse generation
/// through the controller would leave every record written before the actor
/// loop processed `Started` claiming that no browse was running.
#[derive(Debug, Default)]
pub struct GenerationCell {
    session: AtomicU64,
    discovery: AtomicU64,
    capture: AtomicU64,
}

impl GenerationCell {
    /// A cell with every counter at zero, i.e. nothing running yet.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The generations most recently published by their owners.
    pub fn get(&self) -> Generations {
        Generations {
            session: self.session.load(Ordering::Relaxed),
            discovery: self.discovery.load(Ordering::Relaxed),
            capture: self.capture.load(Ordering::Relaxed),
        }
    }

    /// Publishes the two counters the controller owns.
    ///
    /// `generations.discovery` is ignored: that field belongs to
    /// [`Self::publish_discovery`], and writing the controller's mirror of it
    /// here would push the value backwards for as long as a browse start has
    /// not reached the actor loop yet.
    pub fn publish_controller(&self, generations: Generations) {
        self.session.store(generations.session, Ordering::Relaxed);
        self.capture.store(generations.capture, Ordering::Relaxed);
    }

    /// Publishes the browse generation the discovery supervisor just opened.
    ///
    /// Called from the supervisor itself so the value is live from the
    /// instant the generation exists, not from the instant the controller
    /// heard about it.
    pub fn publish_discovery(&self, generation: u64) {
        self.discovery.store(generation, Ordering::Relaxed);
    }
}

/// One redaction-safe diagnostic record for subproject 3's registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticEvent {
    /// Process-relative monotonic timestamp in nanoseconds.
    pub monotonic_ns: u64,
    /// Wall-clock timestamp.
    pub wall_time: SystemTime,
    /// Session generation active at emission.
    pub session_generation: u64,
    /// Discovery generation active at emission.
    pub discovery_generation: u64,
    /// Capture generation active at emission.
    pub capture_generation: u64,
    /// Affected receiver, if the event is receiver-scoped.
    pub receiver: Option<ReceiverId>,
    /// Presentation severity.
    pub severity: Severity,
    /// Coarse domain of the event.
    pub category: DiagnosticCategory,
    /// Typed, redaction-safe content.
    pub payload: DiagnosticPayload,
}

impl DiagnosticEvent {
    /// Builds the synthetic loss event substituted when the feed lags.
    ///
    /// Uses current wall/monotonic time, zero generation values, no receiver
    /// ID, warning severity, and the queue category.
    pub fn queue_lag(dropped: u64) -> Self {
        Self {
            monotonic_ns: monotonic_ns_now(),
            wall_time: SystemTime::now(),
            session_generation: 0,
            discovery_generation: 0,
            capture_generation: 0,
            receiver: None,
            severity: Severity::Warning,
            category: category_of_kind(DiagnosticKind::QueueLag),
            payload: DiagnosticPayload::QueueLag { dropped },
        }
    }
}

/// Error of the awaiting diagnostic receive path.
///
/// Lag never surfaces here; it is converted into a typed
/// [`DiagnosticPayload::QueueLag`] event by [`DiagnosticEventReceiver::recv`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticRecvError {
    /// All senders are gone and the backlog is drained.
    #[error("diagnostic feed is closed")]
    Closed,
}

/// Error of the non-blocking diagnostic receive path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticTryRecvError {
    /// No event is currently buffered.
    #[error("no diagnostic event available")]
    Empty,
    /// All senders are gone and the backlog is drained.
    #[error("diagnostic feed is closed")]
    Closed,
}

/// Consumer side of the bounded diagnostic broadcast.
///
/// Deliberately not `Clone`; use [`DiagnosticEventReceiver::resubscribe`] to
/// obtain an additional independent receiver.
#[derive(Debug)]
pub struct DiagnosticEventReceiver {
    inner: broadcast::Receiver<DiagnosticEvent>,
}

impl DiagnosticEventReceiver {
    /// Wraps an existing broadcast receiver; the backend constructs these.
    #[allow(dead_code)] // the Task 9 controller publishes updates at startup.
    pub(crate) fn new(inner: broadcast::Receiver<DiagnosticEvent>) -> Self {
        Self { inner }
    }

    /// Awaits the next diagnostic event.
    ///
    /// If this receiver lagged behind the ring, the loss is reported as a
    /// synthetic typed [`DiagnosticPayload::QueueLag`] gap event carrying the
    /// number of dropped entries instead of returning an error.
    pub async fn recv(&mut self) -> Result<DiagnosticEvent, DiagnosticRecvError> {
        match self.inner.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                Ok(DiagnosticEvent::queue_lag(dropped))
            }
            Err(broadcast::error::RecvError::Closed) => Err(DiagnosticRecvError::Closed),
        }
    }

    /// Non-blocking receive with the same typed lag conversion as `recv`.
    pub fn try_recv(&mut self) -> Result<DiagnosticEvent, DiagnosticTryRecvError> {
        match self.inner.try_recv() {
            Ok(event) => Ok(event),
            Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                Ok(DiagnosticEvent::queue_lag(dropped))
            }
            Err(broadcast::error::TryRecvError::Empty) => Err(DiagnosticTryRecvError::Empty),
            Err(broadcast::error::TryRecvError::Closed) => Err(DiagnosticTryRecvError::Closed),
        }
    }

    /// Starts a fresh subscription from the oldest retained event, healing a
    /// lagged receiver without losing future events.
    pub fn resubscribe(&self) -> Self {
        Self {
            inner: self.inner.resubscribe(),
        }
    }
}

/// Shell-side bundle of every backend subscription surface.
#[derive(Debug)]
pub struct DeviceBackendUpdates {
    /// Latest complete snapshot; authoritative whenever other feeds lag.
    pub state: watch::Receiver<Arc<DeviceSnapshot>>,
    /// Low-volume shell edges.
    pub events: broadcast::Receiver<BackendEvent>,
    /// Bounded diagnostics source retained outside the UI snapshot.
    pub diagnostics: DiagnosticEventReceiver,
    /// Latest complete generation-scoped live client diagnostics binding.
    pub session_diagnostics: watch::Receiver<SessionDiagnosticsState>,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use tokio::sync::broadcast;

    use super::*;
    use crate::backend::model::{
        AudioSourceState, DiscoveryPhase, PersistenceSnapshot, ReceiverId, ReceiverLifecycle,
        RestartReason, SessionPhase, SetupPhase,
    };

    fn receiver(seed: u8) -> ReceiverId {
        ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
            .expect("fixed seed forms a valid storage key")
    }

    fn diagnostic(payload: DiagnosticPayload) -> DiagnosticEvent {
        DiagnosticEvent {
            monotonic_ns: 0,
            wall_time: SystemTime::UNIX_EPOCH,
            session_generation: 0,
            discovery_generation: 0,
            capture_generation: 0,
            receiver: None,
            severity: Severity::Info,
            category: category_of_kind(payload.kind()),
            payload,
        }
    }

    fn diagnostic_for_receiver(
        receiver: ReceiverId,
        payload: DiagnosticPayload,
    ) -> DiagnosticEvent {
        let mut event = diagnostic(payload);
        event.receiver = Some(receiver);
        event
    }

    fn diagnostic_channel(
        capacity: usize,
    ) -> (broadcast::Sender<DiagnosticEvent>, DiagnosticEventReceiver) {
        let (sender, receiver) = broadcast::channel(capacity);
        (sender, DiagnosticEventReceiver::new(receiver))
    }

    mod backend_events {
        use super::*;

        #[tokio::test]
        async fn backend_events_fan_out_to_every_subscriber() {
            let (sender, first) = broadcast::channel(BACKEND_EVENT_CAPACITY);
            let mut second = sender.subscribe();
            let mut first = first;
            let event = BackendEvent::CommandCompleted { id: 7 };
            sender.send(event.clone()).unwrap();
            assert_eq!(first.recv().await.unwrap(), event);
            assert_eq!(second.recv().await.unwrap(), event);
        }
    }

    mod diagnostic_feed {
        use super::*;

        #[tokio::test]
        async fn diagnostics_are_bounded_and_resubscribable() {
            let (sender, receiver) = diagnostic_channel(2);
            sender
                .send(diagnostic(DiagnosticPayload::Message("one".into())))
                .unwrap();
            let mut copy = receiver.resubscribe();
            sender
                .send(diagnostic(DiagnosticPayload::Message("two".into())))
                .unwrap();
            assert_eq!(
                copy.recv().await.unwrap().payload,
                DiagnosticPayload::Message("two".into())
            );
        }

        #[tokio::test]
        async fn diagnostic_lag_becomes_a_typed_loss_event() {
            let (sender, mut receiver) = diagnostic_channel(2);
            for value in 0..3 {
                sender
                    .send(diagnostic(DiagnosticPayload::Message(value.to_string())))
                    .unwrap();
            }
            let gap = receiver.recv().await.unwrap();
            assert_eq!(gap.payload, DiagnosticPayload::QueueLag { dropped: 1 });
            assert_eq!(gap.severity, Severity::Warning);
            assert_eq!(gap.category, DiagnosticCategory::Queue);
            assert_eq!(gap.receiver, None);
            assert_eq!(gap.session_generation, 0);
            assert_eq!(gap.discovery_generation, 0);
            assert_eq!(gap.capture_generation, 0);
        }

        #[tokio::test]
        async fn resubscribe_heals_after_lag() {
            let (sender, mut receiver) = diagnostic_channel(2);
            for value in 0..3 {
                sender
                    .send(diagnostic(DiagnosticPayload::Message(value.to_string())))
                    .unwrap();
            }
            assert_eq!(
                receiver.recv().await.unwrap().payload,
                DiagnosticPayload::QueueLag { dropped: 1 }
            );
            assert_eq!(
                receiver.recv().await.unwrap().payload,
                DiagnosticPayload::Message("1".into())
            );
            assert_eq!(
                receiver.recv().await.unwrap().payload,
                DiagnosticPayload::Message("2".into())
            );
            let mut healed = receiver.resubscribe();
            sender
                .send(diagnostic(DiagnosticPayload::Message("fresh".into())))
                .unwrap();
            assert_eq!(
                healed.recv().await.unwrap().payload,
                DiagnosticPayload::Message("fresh".into())
            );
        }

        #[tokio::test]
        async fn try_recv_reports_empty_before_events_and_gap_after_loss() {
            let (sender, mut receiver) = diagnostic_channel(2);
            assert_eq!(receiver.try_recv(), Err(DiagnosticTryRecvError::Empty));
            for value in 0..3 {
                sender
                    .send(diagnostic(DiagnosticPayload::Message(value.to_string())))
                    .unwrap();
            }
            assert_eq!(
                receiver.try_recv().unwrap().payload,
                DiagnosticPayload::QueueLag { dropped: 1 }
            );
        }

        #[tokio::test]
        async fn closed_feed_is_reported_by_both_receive_paths() {
            let (sender, mut receiver) = diagnostic_channel(2);
            drop(sender);
            assert_eq!(receiver.try_recv(), Err(DiagnosticTryRecvError::Closed));
            assert_eq!(receiver.recv().await, Err(DiagnosticRecvError::Closed));
        }

        #[test]
        fn diagnostic_debug_output_contains_no_secret_fields() {
            let event = diagnostic_for_receiver(
                receiver(1),
                DiagnosticPayload::Probe {
                    healthy: false,
                    latency_ms: 2_000,
                },
            );
            let rendered = format!("{event:?}");
            assert!(!rendered.contains("pin"));
            assert!(!rendered.contains("key"));
            assert!(!rendered.contains("payload_bytes"));
        }
    }

    mod payload_kinds {
        use super::*;

        #[test]
        fn payload_kind_maps_every_variant_without_wildcard() {
            let samples: Vec<(DiagnosticPayload, DiagnosticKind)> = vec![
                (
                    DiagnosticPayload::Message("ok".into()),
                    DiagnosticKind::Message,
                ),
                (
                    DiagnosticPayload::DiscoveryGeneration {
                        generation: 1,
                        phase: DiscoveryPhase::Running,
                    },
                    DiagnosticKind::DiscoveryGeneration,
                ),
                (
                    DiagnosticPayload::DiscoveryReceiver {
                        receiver: receiver(1),
                        present: true,
                    },
                    DiagnosticKind::DiscoveryReceiver,
                ),
                (
                    DiagnosticPayload::CommandResult { id: 1, error: None },
                    DiagnosticKind::CommandResult,
                ),
                (
                    DiagnosticPayload::CommandCoalesced { dropped: 2 },
                    DiagnosticKind::CommandCoalesced,
                ),
                (
                    DiagnosticPayload::ReceiverTransition {
                        receiver: receiver(1),
                        to: ReceiverLifecycle::Discovered,
                    },
                    DiagnosticKind::ReceiverTransition,
                ),
                (
                    DiagnosticPayload::SetupPhaseDuration {
                        receiver: receiver(1),
                        phase: SetupPhase::Pair,
                        duration: Duration::from_millis(5),
                    },
                    DiagnosticKind::SetupPhaseDuration,
                ),
                (
                    DiagnosticPayload::SessionTransition {
                        phase: SessionPhase::Stopped,
                        active_members: 0,
                    },
                    DiagnosticKind::SessionTransition,
                ),
                (
                    DiagnosticPayload::SessionRestart {
                        generation: 1,
                        reason: RestartReason::MembershipChange,
                    },
                    DiagnosticKind::SessionRestart,
                ),
                (
                    DiagnosticPayload::Retry {
                        receiver: None,
                        attempt: 1,
                        next_at: SystemTime::UNIX_EPOCH,
                    },
                    DiagnosticKind::Retry,
                ),
                (
                    DiagnosticPayload::CaptureTransition {
                        state: AudioSourceState::Capturing,
                    },
                    DiagnosticKind::CaptureTransition,
                ),
                (
                    DiagnosticPayload::PcmDrop { dropped: 3 },
                    DiagnosticKind::PcmDrop,
                ),
                (
                    DiagnosticPayload::SilenceBridge {
                        duration: Duration::from_millis(10),
                    },
                    DiagnosticKind::SilenceBridge,
                ),
                (
                    DiagnosticPayload::Probe {
                        healthy: true,
                        latency_ms: 4,
                    },
                    DiagnosticKind::Probe,
                ),
                (
                    DiagnosticPayload::Persistence {
                        outcome: PersistenceSnapshot::Healthy,
                    },
                    DiagnosticKind::Persistence,
                ),
                (
                    DiagnosticPayload::QueueWatermark {
                        depth: 8,
                        capacity: 64,
                    },
                    DiagnosticKind::QueueWatermark,
                ),
                (
                    DiagnosticPayload::QueueLag { dropped: 9 },
                    DiagnosticKind::QueueLag,
                ),
                (
                    DiagnosticPayload::WorkerShutdown {
                        duration: Duration::from_millis(12),
                        timed_out: false,
                    },
                    DiagnosticKind::WorkerShutdown,
                ),
            ];
            assert_eq!(samples.len(), 18);
            for (payload, expected) in samples {
                assert_eq!(
                    payload.kind(),
                    expected,
                    "kind mapping drifted for {expected:?}"
                );
            }
        }
    }
}
