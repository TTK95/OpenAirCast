//! The single seam between the egui shell and the resilience backend library.
//!
//! The shell's binary module tree and the backend library are two disjoint
//! module trees over one directory: nothing under `src/app`, `src/ui`, or
//! `src/tray` can name a backend type, and nothing under `src/backend` can
//! name a shell type. This file is the one deliberate exception, and a test
//! in it enforces that it stays the only one. That is what makes "no address,
//! socket, secret, raw endpoint ID, device, connection, session, or backend
//! diagnostic object reaches a snapshot or a renderer" checkable by reading
//! one file rather than auditing the whole shell.
//!
//! Two pure halves and the thread that joins them:
//!
//! * downward, [`BackendPort`] adapts the shell effect executor's
//!   [`ControllerPort`] onto [`BackendCommand`];
//! * upward, [`Projection::project`] derives [`ControllerEvent`]s from one
//!   [`DeviceSnapshot`], and [`project_event`] translates the low-volume
//!   [`BackendEvent`] feed;
//! * between them, [`spawn_backend_bridge`] owns one thread and one
//!   `current_thread` runtime, and holds no lock across an `await`.
//!
//! [`backend_device_seam`] ties all three to a started backend. Which
//! implementation the shell opens is one value,
//! [`crate::app::ACTIVE_DEVICE_STAGE`], with
//! [`crate::device_service::legacy_device_seam`] as the other arm and the way
//! back.
//!
//! The upward half is level-driven, not transactional. Every pass re-derives
//! the whole answer from the newest snapshot, and an event the shell's bounded
//! queue refuses is retained and offered again rather than dropped, so the
//! window cannot end up permanently out of step with a backend that has
//! settled and has no reason to publish again.
//!
//! What the shell still does not learn from the backend is listed field by
//! field in [`Projection::project`] and [`session_echo`], each with the reason
//! it stays behind. Nothing is omitted silently.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airplay_core::DeviceId;
use homepod_cast::backend::controller::SHUTDOWN_GROUP_BUDGET;
use homepod_cast::backend::event::{
    BackendEvent, DiagnosticEventReceiver, NoticeCode as BackendNoticeCode,
};
use homepod_cast::backend::model::{
    AudioEndpoint, AudioEndpointPreference, AudioSourceSnapshot, AudioSourceState,
    DiscoverySnapshot, LatencyPreset, LatencyPresetOption, ReceiverSnapshot, SessionSnapshot,
};
use homepod_cast::backend::{
    start_device_backend, BackendCommand, BackendConfig, BackendSendError, DeviceBackendHandle,
    DeviceBackendUpdates,
};
use homepod_cast::{
    DeviceSnapshot, DiagnosticsClock, DiagnosticsHandle, DiagnosticsRegistry,
    DiagnosticsSessionTracker, DiagnosticsSnapshot, DiscoveryPhase, Health,
    ReceiverDiagnosticsSnapshot, ReceiverId, ReceiverLifecycle, RunIntent, SessionDiagnosticsState,
    SessionPhase, SessionStopReason, SystemDiagnosticsClock, Volume,
};
use tokio::sync::{broadcast, watch, Notify};

use crate::app::{
    AppEvent, AudioEndpointChoice, AudioEndpointKey, AudioEndpointRequest, AudioEndpointSelection,
    AudioSourceReading, Availability, CaptureState, ControllerEvent, ControllerPort,
    ControllerSendError, DeviceCommand, DeviceSeam, DeviceShutdown, DiagnosticsHealth,
    DiagnosticsReading, GenerationId, LatencyChoice, LatencyOption, LatencyReading,
    LatencyUnavailable, ReceiverState,
};
use crate::app_handle::{AppEventSender, AppUnavailable};

/// Joins only current-session numeric observations to the known receiver inventory.
/// No backend text, endpoint identity, absolute clock or protocol object crosses.
fn project_live_diagnostics(
    backend: &DeviceSnapshot,
    registry: Option<&DiagnosticsSnapshot>,
    generation: GenerationId,
    registration: Option<(u64, homepod_cast::SessionId)>,
) -> crate::app::LiveDiagnosticsReading {
    use crate::app::{
        BufferDiagnosticsReading, DiagnosticReceiverState as S, LiveDiagnosticsReading,
        ReceiverDiagnosticsReading, TransportDiagnosticsReading,
    };
    let current = registry.filter(|registry| {
        backend.run_intent == RunIntent::Running && registry.session.as_ref().is_some_and(|session| {
            registry.active_session_id == Some(session.session_id)
                && session.finished_elapsed_ns.is_none()
                && matches!((&registration, &backend.session.phase),
                    (Some((a, id)), SessionPhase::Streaming { generation: b } | SessionPhase::Degraded { generation: b })
                    if a == b && *id == session.session_id)
        })
    });
    let buffer = current
        .and_then(|r| r.session.as_ref())
        .and_then(|s| s.audio.as_ref())
        .filter(|audio| audio.sender_buffer.capacity_frames > 0)
        .map(|audio| BufferDiagnosticsReading {
            queued_frames: audio.sender_buffer.queued_frames,
            capacity_frames: audio.sender_buffer.capacity_frames,
            buffered_ms: audio.sender_buffer.buffered_ns / 1_000_000,
            underruns: audio.sender_buffer.underrun_events_total,
        });
    let receivers = backend
        .receivers
        .iter()
        .map(|receiver| {
            let measured = current.and_then(|r| {
                r.receivers.iter().find(|row| {
                    Some(row.key.session_id) == r.active_session_id
                        && row.key.receiver_id == receiver.id
                        && r.session.as_ref().is_some_and(|s| {
                            s.members.get(&receiver.id) == Some(&DeviceId::from(receiver.id))
                        })
                })
            });
            // A registry lifecycle row can exist before any client observation.
            let observed = measured.filter(|row| {
                row.timing.source != homepod_cast::ReceiverTimingSource::Unavailable
                    || row.transport.data_datagrams_attempted_total > 0
                    || row.feedback.attempts_total > 0
            });
            ReceiverDiagnosticsReading {
                id: DeviceId::from(receiver.id),
                state: match receiver.lifecycle {
                    ReceiverLifecycle::Discovered | ReceiverLifecycle::Ready { .. } => S::Ready,
                    ReceiverLifecycle::Connecting { .. } | ReceiverLifecycle::SettingUp { .. } => {
                        S::Connecting
                    }
                    ReceiverLifecycle::Streaming { .. } => S::Streaming,
                    ReceiverLifecycle::RetryWaiting { .. } => S::Recovering,
                    ReceiverLifecycle::Unavailable => S::Offline,
                    ReceiverLifecycle::Failed { .. } => S::Failed,
                },
                transport: observed.map(|row| TransportDiagnosticsReading {
                    packets_accepted: row.transport.data_datagrams_accepted_local_total,
                    bytes_accepted: row.transport.data_bytes_accepted_local_total,
                    send_failures: row.transport.data_datagram_send_failures_total,
                    retransmit_requests: row.transport.retransmit_slots_requested_total,
                }),
                issue: measured
                    .and_then(|row| row.last_error.as_ref())
                    .map(|error| diagnostic_issue(error.code)),
            }
        })
        .collect();
    LiveDiagnosticsReading {
        generation,
        input_peak_per_mille: match backend.audio_source.state {
            AudioSourceState::Capturing | AudioSourceState::SilentSystem => {
                backend.audio_source.windows_input_peak_permille
            }
            _ => None,
        },
        capture_drops_total: backend.audio_source.pcm_frames_dropped_total,
        buffer,
        receivers,
    }
}

fn diagnostic_issue(code: homepod_cast::DiagnosticErrorCode) -> crate::app::DiagnosticIssue {
    use crate::app::DiagnosticIssue as I;
    use homepod_cast::DiagnosticErrorCode as C;
    match code {
        C::PairingFailure => I::Pairing,
        C::SetupRejected | C::SetupTimeout => I::Setup,
        C::CaptureFailure | C::CaptureQueueFull | C::BufferUnderrun | C::EncodeFailure => {
            I::Capture
        }
        C::PtpBindFallback | C::PtpSampleStale => I::Timing,
        C::SenderQueueDisconnected | C::UdpSendFailure | C::RetransmitHistoryMiss => I::Transport,
        C::FeedbackFailure | C::FeedbackTimeout => I::Feedback,
        C::TeardownTimeout => I::Teardown,
        C::DiscoveryFailure
        | C::EventChannelFailure
        | C::CalibrationApplyFailure
        | C::ExportFailure
        | C::Notice => I::Other,
    }
}

#[cfg(test)]
mod live_diagnostics_projection_tests {
    use super::*;
    use homepod_cast::{ReceiverRole, SessionDiagnosticsSnapshot, SessionId};

    fn fixture() -> (DeviceSnapshot, DiagnosticsSnapshot) {
        let id = ReceiverId::from(DeviceId([7; 6]));
        let mut backend = DeviceSnapshot::default();
        backend.run_intent = RunIntent::Running;
        backend.desired_members.insert(id);
        backend.session.active.insert(id);
        backend.session.phase = SessionPhase::Streaming { generation: 19 };
        backend.receivers.push(ReceiverSnapshot {
            id,
            name: "Living room".into(),
            model: "HomePod".into(),
            lifecycle: ReceiverLifecycle::Streaming {
                role: ReceiverRole::Primary,
            },
        });
        let session_id = SessionId::generate();
        let buffer = airplay_audio::AudioBuffer::new(airplay_core::AudioFormat::default(), 2_000);
        let mut audio = buffer.diagnostics_source(44_100).snapshot();
        audio.sender_buffer.queued_frames = 4;
        audio.sender_buffer.buffered_ns = 80_000_000;
        audio.sender_buffer.underrun_events_total = 2;
        let registry = DiagnosticsSnapshot {
            schema_version: homepod_cast::DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
            snapshot_sequence: 1,
            captured_at_utc: std::time::SystemTime::UNIX_EPOCH,
            process_elapsed_ns: 0,
            active_session_id: Some(session_id),
            health: Health::Running,
            session: Some(SessionDiagnosticsSnapshot {
                session_id,
                started_elapsed_ns: 0,
                finished_elapsed_ns: None,
                stop_reason: None,
                phase: Some(SessionPhase::Streaming { generation: 19 }),
                primary: Some(id),
                members: BTreeMap::from([(id, DeviceId([7; 6]))]),
                audio: Some(audio),
            }),
            receivers: vec![homepod_cast::ReceiverDiagnosticsSnapshot {
                key: homepod_cast::ReceiverSessionKey {
                    session_id,
                    receiver_id: id,
                },
                lifecycle: ReceiverLifecycle::Streaming {
                    role: ReceiverRole::Primary,
                },
                timing: homepod_cast::ReceiverTimingSnapshot {
                    source: homepod_cast::ReceiverTimingSource::SenderReference,
                    independently_measured: false,
                    offset_local_minus_master_ns: None,
                    mean_path_delay_ns: None,
                    drift_ppm: None,
                    sample_age_ns: None,
                    stale: false,
                },
                transport: homepod_cast::ReceiverTransportSnapshot {
                    data_datagrams_attempted_total: 103,
                    data_datagrams_accepted_local_total: 100,
                    data_datagram_send_failures_total: 3,
                    data_bytes_accepted_local_total: 80_000,
                    retransmit_slots_requested_total: 6,
                    ..Default::default()
                },
                feedback: Default::default(),
                last_error: None,
            }],
            diagnostics_events_dropped_total: 0,
        };
        (backend, registry)
    }

    #[test]
    fn live_diagnostics_projects_actual_local_counters_and_current_buffer() {
        let (backend, mut registry) = fixture();
        // Register may follow the lossy phase event; the authoritative identity still matches.
        registry.session.as_mut().unwrap().phase = None;
        let reading = project_live_diagnostics(
            &backend,
            Some(&registry),
            GenerationId(3),
            Some((19, registry.active_session_id.unwrap())),
        );
        assert_eq!(reading.generation, GenerationId(3));
        let buffer = reading.buffer.unwrap();
        assert_eq!(
            (buffer.queued_frames, buffer.buffered_ms, buffer.underruns),
            (4, 80, 2)
        );
        let counters = reading.receivers[0].transport.unwrap();
        assert_eq!(
            (
                counters.packets_accepted,
                counters.bytes_accepted,
                counters.send_failures,
                counters.retransmit_requests
            ),
            (100, 80_000, 3, 6)
        );
    }

    #[test]
    fn live_diagnostics_rejects_retained_mismatched_and_unready_measurements() {
        let (mut backend, mut registry) = fixture();
        let registration = Some((19, registry.active_session_id.unwrap()));
        assert!(project_live_diagnostics(
            &backend,
            Some(&registry),
            GenerationId(3),
            Some((19, SessionId::generate()))
        )
        .buffer
        .is_none());
        assert!(
            project_live_diagnostics(&backend, Some(&registry), GenerationId(3), None)
                .buffer
                .is_none()
        );
        registry.active_session_id = None;
        let stopped =
            project_live_diagnostics(&backend, Some(&registry), GenerationId(3), registration);
        assert!(stopped.buffer.is_none());
        assert!(stopped.receivers[0].transport.is_none());
        registry.active_session_id = registry.session.as_ref().map(|s| s.session_id);
        backend.session.phase = SessionPhase::Streaming { generation: 20 };
        assert!(
            project_live_diagnostics(&backend, Some(&registry), GenerationId(3), registration)
                .buffer
                .is_none()
        );
        backend.session.phase = SessionPhase::Stopped;
        assert!(
            project_live_diagnostics(&backend, Some(&registry), GenerationId(3), registration)
                .receivers[0]
                .transport
                .is_none()
        );
    }
}

/// Shell generations the upward bridge has to stamp its events with,
/// published from the shell thread and consumed by the bridge thread.
///
/// The session generation and admission outcome are copied under one short
/// lock, so a projection never combines different requests. No lock is held
/// across the `await` in [`GenerationEcho::wait`].
///
/// A discovery refresh needs no separate "pending" flag. Every refresh raises
/// the discovery generation, and the projection republishes whenever the
/// generation it last answered under differs from the current one, so the
/// generation *is* the pending request. A second encoding of the same fact
/// would be a branch no caller could ever exercise.
#[derive(Debug, Default)]
pub(crate) struct GenerationEcho {
    /// Backend binding published before the corresponding SessionStarted event.
    listening_target: Mutex<
        Option<(
            GenerationId,
            homepod_cast::backend::listening_check::ListeningTestGuard,
        )>,
    >,
    listening_pending: Mutex<Option<VolumePending>>,
    /// Only the latest drag needs a receipt; older drafts are superseded.
    volume: Mutex<Option<VolumePending>>,
    /// One admitted shell transaction; admission and result are read under one lock.
    group: Mutex<Option<GroupPending>>,
    /// Newest generation the shell asked a discovery refresh under.
    discovery: AtomicU64,
    /// Newest session request and its admission outcome, sampled together.
    session: Mutex<(GenerationId, SessionEdge)>,
    /// Wakes the bridge; `notify_one` stores a permit when nobody waits, so
    /// work raised before the bridge parks is not lost.
    wake: Notify,
}

#[derive(Debug)]
struct VolumePending {
    generation: GenerationId,
    receipt: Option<homepod_cast::backend::command::CommandReceipt>,
}

#[derive(Debug)]
struct GroupPending {
    request: u64,
    generation: Option<GenerationId>,
    admission:
        Result<(u64, homepod_cast::backend::command::CommandReceipt), crate::app::GroupFailure>,
}

/// Which session edge the shell most recently asked for.
///
/// Named for the edge rather than the request because `backend::session`
/// already has a `SessionRequest` meaning something else entirely; these two
/// module trees share one crate and must not share vocabulary by accident.
///
/// Carried next to the session generation because the bridge has to tell
/// "the backend already stands where the shell asked it to stand" from "the
/// backend has not moved yet", and the generation alone cannot say which.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SessionEdge {
    /// No session has been started or stopped yet in this run.
    #[default]
    Untouched,
    /// The newest session edge was a start.
    Start,
    /// A group start awaits its exact command receipt before accepting session state.
    PendingGroupStart,
    /// Durable intent confirmed; only this backend attempt or a successor may answer.
    ConfirmedGroupStart { backend_floor: u64 },
    /// The newest session edge was a stop.
    Stop,
    /// No lifecycle command was admitted; republish the real session phase.
    Rejected,
}

impl SessionEdge {
    /// Whether an unchanged backend answer already settles this request.
    ///
    /// This is the whole difference between a discovery generation and a
    /// session generation. A refresh is always answered from the current
    /// inventory, because "browse again" has no wrong answer. A start or a
    /// stop does: the command itself wakes the bridge, so the first pass
    /// after it still sees the pre-command snapshot, and answering the raised
    /// generation from that snapshot would report the *opposite* of what the
    /// user just pressed -- with exactly the generation the reducer is
    /// waiting for, so its guard would let it through and the window would
    /// visibly snap back to streaming on Stop and to stopped on Start.
    ///
    /// So a stale answer settles a raised generation only when it already
    /// says what was asked for:
    ///
    /// * a stop against a backend that is already stopped -- otherwise the
    ///   window waits forever, because a run intent that does not change
    ///   produces no reconcile and therefore no further snapshot;
    /// * a start against a backend that is already streaming;
    /// * a retry of a failure the backend will not re-derive, for the same
    ///   run-intent reason as the stop.
    ///
    /// A failure never settles a *stop*: a stopped run intent tears the
    /// session down, so the truthful answer is on its way, and reporting the
    /// old failure would put an error notice on screen for a deliberate stop.
    /// A rejected enqueue changes no backend intent and therefore accepts any
    /// actual phase as its answer, including a rebuild already in progress.
    fn answered_by(self, echo: &SessionEcho) -> bool {
        match (self, echo) {
            (Self::PendingGroupStart | Self::ConfirmedGroupStart { .. }, _) => false,
            (Self::Rejected, _) => true,
            (
                Self::Start,
                SessionEcho::Active(_) | SessionEcho::Degraded(_) | SessionEcho::Failed(_),
            ) => true,
            (Self::Stop, SessionEcho::Stopped) => true,
            (Self::Start, SessionEcho::Stopped)
            | (
                Self::Stop,
                SessionEcho::Active(_) | SessionEcho::Degraded(_) | SessionEcho::Failed(_),
            )
            // A rebuild in flight answers neither press: it is the controller
            // moving on its own, and the edge the user asked for is still
            // outstanding on the other side of it.
            | (_, SessionEcho::Restarting)
            | (Self::Untouched, _) => false,
        }
    }
}

impl GenerationEcho {
    fn observe_listening_target(&self, snapshot: &DeviceSnapshot, events: &[ControllerEvent]) {
        let mut target = self
            .listening_target
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(generation) = events.iter().find_map(|e| match e {
            ControllerEvent::SessionStarted { generation, .. } => Some(*generation),
            _ => None,
        }) {
            *target = homepod_cast::backend::listening_check::ListeningTestGuard::capture(snapshot)
                .map(|guard| (generation, guard));
        } else if !matches!(
            snapshot.session.phase,
            homepod_cast::backend::model::SessionPhase::Streaming { .. }
        ) {
            *target = None;
        }
    }

    fn listening_events(&self, snapshot: &DeviceSnapshot, closed: bool) -> Vec<ControllerEvent> {
        use homepod_cast::backend::command::ReceiptState;
        let mut pending = self
            .listening_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(request) = pending.as_mut() else {
            return vec![];
        };
        let status = request.receipt.as_mut().map(|r| r.poll(snapshot.revision));
        if !closed && status == Some(ReceiptState::Pending) {
            return vec![];
        }
        let failed = closed || !matches!(status, Some(ReceiptState::Completed(None)));
        let generation = request.generation;
        *pending = None;
        if failed {
            vec![ControllerEvent::SessionFailed {
                generation,
                summary: "The listening test tone could not be started.".into(),
            }]
        } else {
            vec![]
        }
    }

    fn volume_events(&self, snapshot: &DeviceSnapshot, closed: bool) -> Vec<ControllerEvent> {
        use homepod_cast::backend::command::ReceiptState;
        let mut pending = self.volume.lock().unwrap_or_else(|e| e.into_inner());
        let Some(VolumePending {
            generation,
            receipt,
        }) = pending.as_mut()
        else {
            return vec![];
        };
        let settled = receipt.as_mut().is_none_or(|receipt| {
            receipt.poll(snapshot.revision) != ReceiptState::Pending || closed
        });
        if !settled {
            return vec![];
        }
        let event = ControllerEvent::VolumeApplied {
            generation: *generation,
            volume: snapshot.master_volume.get(),
        };
        *pending = None;
        vec![event]
    }
    /// Resolve exact receipts after state projection, independently of history eviction.
    fn group_events(&self, snapshot: &DeviceSnapshot, closed: bool) -> Vec<ControllerEvent> {
        use crate::app::{GroupFailure, GroupOperationStatus};
        let mut guard = self.group.lock().unwrap_or_else(|error| error.into_inner());
        let Some(pending) = guard.as_mut() else {
            return vec![];
        };
        let status = match &mut pending.admission {
            Err(failure) => Some(GroupOperationStatus::Failed(*failure)),
            Ok((_, receipt)) => {
                use homepod_cast::backend::command::ReceiptState;
                let result = receipt.poll(snapshot.revision);
                if let ReceiptState::Completed(error) = result {
                    Some(match error {
                        None => GroupOperationStatus::Succeeded,
                        Some(homepod_cast::backend::model::CommandFailure::Persistence) => {
                            GroupOperationStatus::Failed(GroupFailure::Persistence)
                        }
                        Some(homepod_cast::backend::model::CommandFailure::Rejected) => {
                            GroupOperationStatus::Failed(GroupFailure::Validation)
                        }
                    })
                } else if closed {
                    Some(GroupOperationStatus::Failed(GroupFailure::Closed))
                } else if result == ReceiptState::Lost {
                    Some(GroupOperationStatus::Failed(GroupFailure::ConfirmationLost))
                } else {
                    None
                }
            }
        };
        match status {
            Some(status) => {
                let request = pending.request;
                if let Some(generation) = pending.generation {
                    let mut session = self
                        .session
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if session.0 == generation {
                        // The exact receipt already obeys its snapshot revision
                        // barrier. Keep the backend attempt identity after
                        // success: the phase can still belong to a predecessor.
                        session.1 = if matches!(status, GroupOperationStatus::Failed(_)) {
                            SessionEdge::Rejected
                        } else {
                            match &pending.admission {
                                Ok((_, receipt)) => receipt
                                    .session_generation_floor(snapshot.revision)
                                    .map(|backend_floor| SessionEdge::ConfirmedGroupStart {
                                        backend_floor,
                                    })
                                    .unwrap_or(SessionEdge::PendingGroupStart),
                                Err(_) => unreachable!("a refused group cannot succeed"),
                            }
                        };
                        self.wake.notify_one();
                    }
                }
                *guard = None;
                vec![ControllerEvent::GroupOperationFinished { request, status }]
            }
            None => vec![],
        }
    }
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Newest generation a discovery refresh was requested under.
    pub(crate) fn discovery(&self) -> GenerationId {
        GenerationId(self.discovery.load(Ordering::SeqCst))
    }

    /// Newest generation a session start or stop was requested under.
    pub(crate) fn session(&self) -> GenerationId {
        self.session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
    }

    /// Which edge raised the newest session generation.
    #[cfg(test)]
    pub(crate) fn session_edge(&self) -> SessionEdge {
        self.session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1
    }

    /// Parks until the shell raises new work. Holds nothing while parked.
    pub(crate) async fn wait(&self) {
        self.wake.notified().await;
    }

    fn record_discovery(&self, generation: GenerationId) {
        self.discovery.store(generation.0, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Records one complete admission outcome before waking the bridge.
    fn record_session(&self, generation: GenerationId, request: SessionEdge) {
        *self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = (generation, request);
        self.wake.notify_one();
    }

    /// Reads the shell generations for one projection pass.
    pub(crate) fn sample(&self) -> GenerationsSeen {
        let discovery = self.discovery();
        let (session, session_edge) = *self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        GenerationsSeen {
            discovery,
            session,
            session_edge,
        }
    }
}

/// One pass's view of what the shell most recently asked for.
///
/// Backend snapshots carry the backend's own generation counters, which are
/// unrelated to the shell's. Every event the bridge emits is stamped from
/// here instead, because the reducer's guards compare against the shell's.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct GenerationsSeen {
    /// Generation the newest discovery refresh was requested under.
    pub(crate) discovery: GenerationId,
    /// Generation the newest session start or stop was requested under.
    pub(crate) session: GenerationId,
    /// Which edge raised that session generation.
    pub(crate) session_edge: SessionEdge,
}

/// What the bridge last told the reducer, so a pass emits only differences.
///
/// Level-driven rather than transactional: each pass re-derives the whole
/// answer from the newest snapshot and compares it with this. An event lost
/// to a full shell queue is therefore repaired by the next revision instead
/// of leaving the window permanently out of step with the backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Projection {
    master_volume: Option<f32>,
    desired: Option<(u64, Vec<DeviceId>)>,
    groups: Option<Vec<crate::app::SavedGroupReading>>,
    /// Newest discovery answer published, with the generation it was
    /// published under. `None` until the daemon has produced one.
    discovery: Option<(GenerationId, DiscoveryEcho)>,
    /// Newest session answer published, with its shell generation.
    session: Option<(GenerationId, SessionEcho)>,
    /// Newest mute answer published. `None` until the first pass.
    mute: Option<bool>,
    /// Last durable per-receiver balances published to the shell.
    receiver_levels: Option<BTreeMap<DeviceId, f32>>,
    /// Newest latency answer published.
    latency: Option<LatencyReading>,
    /// Newest capture-source answer published.
    audio: Option<AudioSourceReading>,
    /// Which opaque key stands for which Windows endpoint ID.
    ///
    /// Part of the projection rather than a side table because `project` is
    /// pure: a pass that meets a new endpoint returns a table with one more
    /// entry, and the caller decides what to do with it. The bridge thread
    /// publishes it into the shared [`EndpointDirectory`] the command half
    /// reads.
    endpoints: EndpointTable,
}

/// The allocation of opaque keys to raw Windows endpoint IDs.
///
/// Two rules, and both are safety rather than tidiness:
///
/// * **A key is a counter.** It is not a truncation of the ID and not a hash
///   of it, so no argument about irreversibility is needed: there is nothing
///   in the key derived from the ID at all.
/// * **An entry is never removed and a key is never reused.** Endpoints come
///   and go -- a headset is unplugged, a monitor sleeps -- and a key that
///   came to mean a *different* device would turn a stale click into the
///   silent selection of the wrong microphone. Retiring an entry instead
///   would reintroduce the same race between the frame that painted a key and
///   the command that carries it. The table therefore grows by one per
///   distinct endpoint ID seen in the process lifetime, which on any real
///   machine is a handful.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EndpointTable {
    /// Next key to hand out. Monotonic for the life of the process.
    next: u64,
    /// Every endpoint ID this run has ever offered, in allocation order,
    /// with the newest display name seen for it.
    known: Vec<EndpointEntry>,
}

#[derive(Clone, PartialEq)]
struct EndpointEntry {
    key: AudioEndpointKey,
    /// The raw Windows endpoint ID. This field is the reason this type is
    /// private to the bridge.
    id: String,
    name: String,
}

/// Written by hand so the raw endpoint ID is not printable.
///
/// [`Projection`] is `Debug` and holds the table; one `tracing::debug!` line
/// in this file naming it would otherwise put every `{0.0.0.00000000}.{guid}`
/// on the machine into a log, with no compile error and no test to catch it,
/// because the shell-side canaries only ever read what the *window* says.
/// Redacting at the only place the ID lives closes that door for every future
/// caller instead of asking each one to remember.
impl std::fmt::Debug for EndpointEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EndpointEntry")
            .field("key", &self.key)
            .field("id", &"<redacted>")
            .field("name", &self.name)
            .finish()
    }
}

impl EndpointTable {
    /// The key for `endpoint`, allocating one the first time it is seen and
    /// refreshing the display name every time after.
    fn key_for(&mut self, endpoint: &AudioEndpoint) -> AudioEndpointKey {
        if let Some(entry) = self.known.iter_mut().find(|entry| entry.id == endpoint.id) {
            entry.name.clone_from(&endpoint.name);
            return entry.key;
        }

        let key = AudioEndpointKey(self.next);
        self.next += 1;
        self.known.push(EndpointEntry {
            key,
            id: endpoint.id.clone(),
            name: endpoint.name.clone(),
        });
        key
    }

    /// The backend preference one key stands for, or `None` for a key this
    /// process never allocated.
    ///
    /// `None` is the only safe answer to an unrecognised key, and the command
    /// half turns it into no command at all rather than into a guess.
    fn preference(&self, key: AudioEndpointKey) -> Option<AudioEndpointPreference> {
        self.known
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| AudioEndpointPreference::Explicit {
                id: entry.id.clone(),
                last_known_name: entry.name.clone(),
            })
    }
}

/// The key table as the command half sees it.
///
/// One `Mutex` around a table the bridge thread writes after each projection
/// pass and the shell thread reads when it sends a selection. No lock is ever
/// held across an `await`: the publish is a single synchronous statement in
/// the bridge loop, and the read happens on the shell thread, which has no
/// runtime at all.
#[derive(Debug, Default)]
pub(crate) struct EndpointDirectory {
    table: std::sync::Mutex<EndpointTable>,
}

impl EndpointDirectory {
    fn publish(&self, table: &EndpointTable) {
        let mut guard = self
            .table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard != *table {
            guard.clone_from(table);
        }
    }

    fn preference(&self, key: AudioEndpointKey) -> Option<AudioEndpointPreference> {
        self.table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .preference(key)
    }
}

/// The shell-visible content of one discovery answer.
#[derive(Clone, Debug, PartialEq)]
enum DiscoveryEcho {
    Inventory(Vec<ReceiverState>),
    Failed(String),
}

/// The shell-visible content of one session answer.
#[derive(Clone, Debug, PartialEq)]
enum SessionEcho {
    Stopped,
    Active(Vec<DeviceId>),
    /// Running, delivering to the receivers named here, but not to every one
    /// that was asked for -- or without a capture source.
    Degraded(Vec<DeviceId>),
    /// Running, with the whole group being rebuilt by the controller. Carries
    /// no membership because during the rebuild there is none.
    Restarting,
    Failed(String),
}

impl Projection {
    /// Derives the shell events one backend snapshot implies.
    ///
    /// Pure and total: no clock, no I/O, no channel. Every field of
    /// [`DeviceSnapshot`] is destructured by name without a `..` rest
    /// pattern, so a new backend field is a compile error here until someone
    /// decides in this file what the shell may learn about it.
    pub(crate) fn project(
        &self,
        snapshot: &DeviceSnapshot,
        seen: GenerationsSeen,
    ) -> (Self, Vec<ControllerEvent>) {
        let DeviceSnapshot {
            // The backend's own snapshot counter; the shell has its own.
            revision: _,
            desired_revision,
            discovery,
            receivers,
            saved_groups,
            desired_members,
            // Correlated separately by GenerationEcho after this state projection.
            command_outcomes: _,
            // The shell derives running/stopped from the session phase, which
            // is the settled fact rather than the wish.
            run_intent: _,
            session,
            // Forwarded, but never verbatim: this is where the raw Windows
            // endpoint IDs live. `audio_echo` hands out opaque keys for them
            // and keeps every ID inside this file.
            audio_source,
            master_volume,
            receiver_levels,
            muted,
            latency_preset,
            latency_presets,
            // No shell vocabulary yet: auto-connect.
            auto_connect: _,
            // No shell vocabulary yet: device-state persistence health. Not
            // to be confused with the settings file, which the shell already
            // reports through `PreferencesEvent`.
            persistence: _,
        } = snapshot;

        let mut next = self.clone();
        let mut events = Vec::new();
        if next.master_volume != Some(master_volume.get()) {
            next.master_volume = Some(master_volume.get());
            events.push(ControllerEvent::VolumeApplied {
                generation: GenerationId(0),
                volume: master_volume.get(),
            });
        }

        let desired = (
            *desired_revision,
            desired_members
                .iter()
                .map(|id| DeviceId::from(*id))
                .collect::<Vec<_>>(),
        );
        if next.desired.as_ref() != Some(&desired) {
            events.push(ControllerEvent::DesiredReceiversChanged {
                desired_revision: desired.0,
                receiver_ids: desired.1.clone(),
            });
            next.desired = Some(desired);
        }
        let groups = saved_groups
            .iter()
            .map(|group| crate::app::SavedGroupReading {
                id: crate::app::GroupId(group.id.as_uuid()),
                name: group.name.clone(),
                members: group
                    .members
                    .iter()
                    .map(|member| crate::app::GroupMember {
                        receiver: DeviceId::from(member.receiver),
                        name: member.last_known_name.clone(),
                        level: member.level.get(),
                    })
                    .collect(),
                available_members: group
                    .members
                    .iter()
                    .filter(|member| {
                        receivers.iter().any(|row| {
                            row.id == member.receiver
                                && availability_of(&row.lifecycle) == Availability::Available
                        })
                    })
                    .map(|member| DeviceId::from(member.receiver))
                    .collect(),
            })
            .collect::<Vec<_>>();
        if next.groups.as_ref() != Some(&groups) {
            events.push(ControllerEvent::SavedGroupsChanged {
                groups: groups.clone(),
            });
            next.groups = Some(groups);
        }

        if let Some(echo) = discovery_echo(discovery, receivers) {
            let stamped = (seen.discovery, echo);
            if next.discovery.as_ref() != Some(&stamped) {
                events.push(match &stamped.1 {
                    DiscoveryEcho::Inventory(receivers) => ControllerEvent::DiscoveryCompleted {
                        generation: stamped.0,
                        receivers: receivers.clone(),
                    },
                    DiscoveryEcho::Failed(summary) => ControllerEvent::DiscoveryFailed {
                        generation: stamped.0,
                        summary: summary.clone(),
                    },
                });
                next.discovery = Some(stamped);
            }
        }

        if let Some(echo) = session_echo(session) {
            // Deliberately *not* the discovery rule. A raised discovery
            // generation is always answered from the current inventory; a
            // raised session generation is answered from an unchanged
            // snapshot only when that snapshot already says what the shell
            // asked for. See `SessionEdge::answered_by` for why copying
            // the discovery rule here would make every Start and Stop press
            // visibly snap back.
            // A predecessor can change phase after the shell requested a new
            // generation. Changed content is not proof that it answers that
            // newer request. Group starts also wait for their exact receipt,
            // so an old Active/Failed snapshot cannot settle them prematurely.
            let publish = if let SessionEdge::ConfirmedGroupStart { backend_floor } =
                seen.session_edge
            {
                let backend_generation = match session.phase {
                    homepod_cast::backend::model::SessionPhase::Starting { generation }
                    | homepod_cast::backend::model::SessionPhase::Streaming { generation }
                    | homepod_cast::backend::model::SessionPhase::Degraded { generation }
                    | homepod_cast::backend::model::SessionPhase::Restarting {
                        generation, ..
                    }
                    | homepod_cast::backend::model::SessionPhase::Stopping { generation }
                    | homepod_cast::backend::model::SessionPhase::Failed { generation, .. } => {
                        Some(generation)
                    }
                    homepod_cast::backend::model::SessionPhase::Stopped => None,
                };
                let settled = next
                    .session
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == seen.session);
                // Recovering is a real outcome too (empty castable set or
                // preflight failure), and keeps Cancel available. Once settled,
                // spontaneous stop/health updates retain their usual semantics.
                backend_generation.map_or(settled, |generation| generation >= backend_floor)
                    && next.session.as_ref() != Some(&(seen.session, echo.clone()))
            } else if seen.session_edge == SessionEdge::PendingGroupStart {
                false
            } else {
                match next.session.as_ref() {
                    None if seen.session_edge != SessionEdge::Untouched => {
                        seen.session_edge.answered_by(&echo)
                    }
                    None => true,
                    Some((generation, _)) if *generation != seen.session => {
                        seen.session_edge.answered_by(&echo)
                    }
                    Some((_, published)) => published != &echo,
                }
            };
            let stamped = (seen.session, echo);
            if publish {
                events.push(match &stamped.1 {
                    SessionEcho::Stopped => ControllerEvent::SessionStopped {
                        generation: stamped.0,
                    },
                    SessionEcho::Active(active) => ControllerEvent::SessionStarted {
                        generation: stamped.0,
                        active_receiver_ids: active.clone(),
                    },
                    SessionEcho::Degraded(active) => ControllerEvent::SessionDegraded {
                        generation: stamped.0,
                        active_receiver_ids: active.clone(),
                    },
                    SessionEcho::Restarting => ControllerEvent::SessionRestarting {
                        generation: stamped.0,
                    },
                    SessionEcho::Failed(summary) => ControllerEvent::SessionFailed {
                        generation: stamped.0,
                        summary: summary.clone(),
                    },
                });
                next.session = Some(stamped);
            }
        }

        if next.mute != Some(*muted) {
            next.mute = Some(*muted);
            events.push(ControllerEvent::MuteChanged { muted: *muted });
        }

        let levels = receiver_levels
            .iter()
            .map(|(receiver, level)| (DeviceId::from(*receiver), level.get()))
            .collect::<BTreeMap<_, _>>();
        if next.receiver_levels.as_ref() != Some(&levels) {
            next.receiver_levels = Some(levels.clone());
            events.push(ControllerEvent::ReceiverLevelsChanged { levels });
        }

        let latency = latency_echo(*latency_preset, latency_presets);
        if next.latency.as_ref() != Some(&latency) {
            next.latency = Some(latency.clone());
            events.push(ControllerEvent::LatencyChanged { reading: latency });
        }

        let audio = audio_echo(audio_source, &mut next.endpoints);
        if next.audio.as_ref() != Some(&audio) {
            next.audio = Some(audio.clone());
            events.push(ControllerEvent::AudioSourceChanged { reading: audio });
        }

        (next, events)
    }
}

/// The shell's word for one backend latency profile.
///
/// Written out without a rest pattern so a fourth backend profile stops this
/// file compiling until someone decides here whether the window may offer it.
fn latency_choice(preset: LatencyPreset) -> LatencyChoice {
    match preset {
        LatencyPreset::Low => LatencyChoice::Low,
        LatencyPreset::Normal => LatencyChoice::Normal,
        LatencyPreset::Stable => LatencyChoice::Stable,
    }
}

/// What one latency offer means to the shell.
///
/// The backend states its refusal as a `UserFacingError`, which is a newtype
/// over free-form text: it has no variants to match, and its sentence is
/// composed at the point of failure. So the text is not narrowed here -- it
/// is not read at all. The bridge learns only *whether* a profile is refused
/// and substitutes the shell's own code, exactly as `DiscoveryFailed` is
/// turned into `DISCOVERY_FAILED_SUMMARY` on the presentation side. The
/// `Option` is matched without a wildcard, so if `UserFacingError` ever
/// becomes an enum whose cases the window should tell apart, this is the arm
/// that has to grow.
fn latency_echo(selected: LatencyPreset, offered: &[LatencyPresetOption]) -> LatencyReading {
    LatencyReading {
        selected: latency_choice(selected),
        options: offered
            .iter()
            .map(|option| LatencyOption {
                choice: latency_choice(option.preset),
                unavailable: latency_unavailable(option),
            })
            .collect(),
    }
}

/// Whether one offered profile is refused, as a shell code.
///
/// The offer is destructured by name without a rest pattern, so a second
/// reason the backend might one day attach to a profile is a compile error
/// here rather than a fact the window silently drops. The refusal itself is
/// bound and never read: it is free-form text composed at the point of
/// failure, and the catalog that owns every visible string in this shell
/// holds `&'static str` only.
fn latency_unavailable(option: &LatencyPresetOption) -> Option<LatencyUnavailable> {
    let LatencyPresetOption {
        preset: _,
        disabled_reason,
    } = option;

    disabled_reason
        .as_ref()
        .map(|_refused| LatencyUnavailable::NotValidated)
}

/// The shell's word for what capture is doing.
fn capture_state(state: AudioSourceState) -> CaptureState {
    match state {
        AudioSourceState::Capturing => CaptureState::Capturing,
        AudioSourceState::SilentSystem => CaptureState::SilentSystem,
        AudioSourceState::Recovering => CaptureState::Recovering,
        AudioSourceState::Unavailable => CaptureState::Unavailable,
        AudioSourceState::Failed => CaptureState::Failed,
    }
}

/// What one capture-source snapshot means to the shell.
///
/// The one function in the shell that ever sees a Windows endpoint ID, and it
/// keeps every one of them: the offered endpoints cross as
/// `(opaque key, display name)` pairs, and the stored preference crosses as
/// the key of the endpoint it names *while that endpoint is on offer* and as
/// a bare saved name otherwise. That second case is paragraph 7.4's
/// "unfulfillable preference", and it is read off the snapshot rather than
/// guessed: the ID is either in the offered list or it is not.
///
/// Display names are carried. A name is what Windows itself shows the user in
/// its own volume mixer, it is the same class of datum as the receiver names
/// that already cross this boundary, and without it the list would be a
/// column of numbers nobody could choose from.
fn audio_echo(source: &AudioSourceSnapshot, table: &mut EndpointTable) -> AudioSourceReading {
    let AudioSourceSnapshot {
        windows_input_peak_permille: _,
        pcm_frames_dropped_total: _,
        refresh_failed,
        active_endpoints,
        preference,
        captured_endpoint,
        state,
    } = source;

    let endpoints: Vec<AudioEndpointChoice> = active_endpoints
        .iter()
        .flatten()
        .map(|endpoint| AudioEndpointChoice {
            key: table.key_for(endpoint),
            name: endpoint.name.clone(),
        })
        .collect();

    let selection = match preference {
        AudioEndpointPreference::SystemDefault => AudioEndpointSelection::SystemDefault,
        AudioEndpointPreference::Explicit {
            id,
            last_known_name,
        } => match active_endpoints
            .iter()
            .flatten()
            .position(|endpoint| &endpoint.id == id)
        {
            Some(index) => AudioEndpointSelection::Chosen(endpoints[index].key),
            None if active_endpoints.is_none()
                || captured_endpoint
                    .as_ref()
                    .is_some_and(|endpoint| &endpoint.id == id) =>
            {
                AudioEndpointSelection::ChosenUnverified {
                    name: captured_endpoint
                        .as_ref()
                        .filter(|endpoint| &endpoint.id == id)
                        .map_or_else(|| last_known_name.clone(), |endpoint| endpoint.name.clone()),
                }
            }
            None => AudioEndpointSelection::ChosenButMissing {
                name: last_known_name.clone(),
            },
        },
    };

    AudioSourceReading {
        refresh_failed: *refresh_failed,
        endpoints_known: active_endpoints.is_some(),
        endpoints,
        selection,
        captured_key: captured_endpoint
            .as_ref()
            .map(|endpoint| table.key_for(endpoint)),
        captured_name: captured_endpoint
            .as_ref()
            .map(|endpoint| endpoint.name.clone()),
        state: capture_state(*state),
    }
}

/// What one discovery snapshot means to the shell, or `None` while the daemon
/// has nothing to say.
fn discovery_echo(
    discovery: &DiscoverySnapshot,
    receivers: &[ReceiverSnapshot],
) -> Option<DiscoveryEcho> {
    let DiscoverySnapshot {
        // The backend's browse generation, not the shell's refresh counter.
        generation: _,
        phase,
    } = discovery;

    match phase {
        // Suspend and shutdown stop the daemon. An empty inventory published
        // here would tell the user their receivers had disappeared, so the
        // last answer stands until the daemon runs again.
        DiscoveryPhase::Stopped => None,
        DiscoveryPhase::Running => Some(DiscoveryEcho::Inventory(inventory(receivers))),
        // A supervised restart with backoff is the closest thing the daemon
        // has to the legacy one-shot browse failing. `retry_at` is a
        // wall-clock instant and deliberately stays behind.
        DiscoveryPhase::Retrying {
            attempt: _,
            retry_at: _,
        } => Some(DiscoveryEcho::Failed(DISCOVERY_FAILED_SUMMARY.to_owned())),
    }
}

/// What the shell is told when discovery cannot be maintained.
///
/// Owned here rather than taken from the backend, and that ownership is the
/// point. `Notice.message` is the one free-form string the backend can put in
/// front of the bridge, and it is not always a constant: the network-monitor
/// arm interpolates a `NetworkError` into it. Today that error renders no
/// address, but "today it happens to be safe" is not a boundary. Every
/// discovery failure the shell sees is therefore this sentence, from both the
/// snapshot's `Retrying` phase and [`project_event`]'s notice arm, which
/// keeps the redaction promise checkable by reading this file.
const DISCOVERY_FAILED_SUMMARY: &str = "Devices could not be searched for.";

/// Projects the receiver inventory into presentation-safe records, ordered by
/// stable identity so the comparison against the previous pass cannot depend
/// on the backend's display-name sort.
fn inventory(receivers: &[ReceiverSnapshot]) -> Vec<ReceiverState> {
    let mut records: Vec<ReceiverState> = receivers
        .iter()
        .map(|receiver| {
            let ReceiverSnapshot {
                id,
                name,
                model,
                lifecycle,
            } = receiver;
            ReceiverState {
                id: DeviceId::from(*id),
                name: display_name(*id, name),
                model: model.clone(),
                availability: availability_of(lifecycle),
            }
        })
        .collect();
    records.sort_by_key(|record| record.id.0);
    records
}

/// Passes a receiver's display name through, unless it *is* the identity.
///
/// The backend no longer synthesizes a name from [`ReceiverId`], so in a
/// consistent tree this returns `name` unchanged. It stays because the bridge
/// is where "no raw endpoint ID reaches the window" is enforced and checked,
/// and a name field is the one string on this path that crosses verbatim: the
/// leak canary cannot distinguish a MAC-shaped name from a legitimate one,
/// because it depends on names crossing. A rule the boundary owns outlives
/// the backend arm that happened to need it.
fn display_name(id: ReceiverId, name: &str) -> String {
    if name == id.to_string() {
        tracing::warn!("a receiver display name matched its identity and was suppressed");
        return String::new();
    }
    name.to_owned()
}

/// Collapses the eight-state receiver lifecycle onto the shell's two-state
/// availability.
///
/// Only "not currently discoverable" becomes `Unavailable`. Every other state
/// is a session concern the backend owns, and reporting one of them as
/// unavailable would drop the receiver out of the next start's member set --
/// and therefore out of the backend's own retry and rejoin policy.
fn availability_of(lifecycle: &ReceiverLifecycle) -> Availability {
    match lifecycle {
        ReceiverLifecycle::Unavailable => Availability::Unavailable,
        ReceiverLifecycle::Discovered
        | ReceiverLifecycle::Connecting { attempt: _ }
        | ReceiverLifecycle::SettingUp { role: _, phase: _ }
        | ReceiverLifecycle::Ready { role: _ }
        | ReceiverLifecycle::Streaming { role: _ }
        // `retry_at` is a wall-clock deadline and must not reach the shell.
        | ReceiverLifecycle::RetryWaiting {
            attempt: _,
            retry_at: _,
        }
        | ReceiverLifecycle::Failed {
            retryable: _,
            error: _,
        } => Availability::Available,
    }
}

/// What one session snapshot means to the shell, or `None` while a phase is
/// in flight and the window should keep showing what it already shows.
fn session_echo(session: &SessionSnapshot) -> Option<SessionEcho> {
    let SessionSnapshot {
        phase,
        // The committed wish, not the settled fact; see `desired_revision`.
        desired: _,
        active,
        // No shell vocabulary yet: per-receiver failure and retry state. The
        // whole-session phase now distinguishes a reduced delivery from a
        // complete one, but which receiver is waiting on which attempt is a
        // further step, and the inventory's availability is what the window
        // renders per row today.
        failed: _,
        retry_waiting: _,
        // No shell vocabulary yet: which receiver holds the PTP clock.
        primary: _,
        // No shell vocabulary yet: live versus silence-bridged audio.
        audio_flow: _,
        // No shell vocabulary yet: "stopped because Windows suspended" as
        // distinct from "stopped because the user stopped it".
        resume_pending: _,
    } = session;

    match phase {
        SessionPhase::Stopped => Some(SessionEcho::Stopped),
        // In flight, and the shell already stands where its own press put it.
        // Reporting either as a stop would flip the window to "stopped" and
        // back for every start and every teardown.
        SessionPhase::Starting { generation: _ } | SessionPhase::Stopping { generation: _ } => None,
        // A restart, by contrast, is *not* something the shell asked for: the
        // controller rebuilds the group after a membership change, a primary
        // replacement, a network rebind, or a resume. Reporting it as a stop
        // would still be wrong -- the session never ended -- but reporting
        // nothing at all left an audible interruption with no explanation on
        // screen, which is what this variant fixes.
        //
        // `reason` stays behind. It is presentation-safe, but the window has
        // no vocabulary for thirteen distinct causes yet, and inventing one
        // sentence for all of them would say less than the phase already does.
        SessionPhase::Restarting {
            generation: _,
            reason: _,
        } => Some(SessionEcho::Restarting),
        SessionPhase::Streaming { generation: _ } => Some(SessionEcho::Active(
            active.iter().map(|id| DeviceId::from(*id)).collect(),
        )),
        // Reported as its own phase rather than as an ordinary stream. The
        // active set says which receivers really carry audio, but only the
        // phase can say that the set is smaller than the one that was asked
        // for -- the shell cannot derive that, because its own desired set is
        // not the committed membership of this generation.
        SessionPhase::Degraded { generation: _ } => Some(SessionEcho::Degraded(
            active.iter().map(|id| DeviceId::from(*id)).collect(),
        )),
        SessionPhase::Failed {
            generation: _,
            error,
        } => Some(SessionEcho::Failed(error.as_str().to_owned())),
    }
}

// ---------------------------------------------------------------------------
// The diagnostics registry, projected
// ---------------------------------------------------------------------------

/// Derives the shell's [`DiagnosticsReading`] from one registry snapshot.
///
/// The most dangerous projection in the shell, and the reason it is written
/// like the device one: pure, total, and destructured by name without a `..`
/// rest pattern at every level it looks at, so a new field in
/// [`DiagnosticsSnapshot`] or in a receiver row stops this file compiling
/// until someone decides here whether the window may learn about it.
///
/// What crosses, and what does not:
///
/// * `schema_version` -- the registry's own contract number. It says nothing
///   about this installation that the build version does not already say.
/// * `snapshot_sequence` -- a cache-invalidation counter. It moves on every
///   publication, so putting it in the reading would make every reading
///   different from the last one and turn the shell's "changed?" comparison
///   into "always", which is exactly the check that keeps the window from
///   repainting for nothing.
/// * `captured_at_utc` -- **an absolute wall-clock instant, refused.** It is
///   the same class as `DiscoveryPhase::Retrying`'s `retry_at` and
///   `ReceiverLifecycle::RetryWaiting`'s, both of which this file already
///   holds back. A window value that changes with the calendar cannot be
///   asserted without asserting the clock, and a page that showed it would be
///   showing the reader their own system time back.
/// * `process_elapsed_ns` -- **the age of this process, refused,** for the
///   same reason: it moves on every publication and measures the app rather
///   than the audio.
/// * `active_session_id` -- a UUID used only to gate whether retained receiver
///   rows are current. The support export only ever emits it behind a
///   per-export alias; a page has no use for either form, because the window
///   already names the session by what it is doing.
/// * `health` -- crosses, as the shell's own four-word vocabulary.
/// * `session` -- held back whole. Its members are `ReceiverId`/`DeviceId`
///   pairs, its `phase` carries free-form `reason` text, and its `audio` half
///   is a foreign crate's snapshot; none of that has a word on this page yet,
///   and a partial session row would be the least honest thing here.
/// * `receivers` -- **counted, never forwarded.** See [`measured_receivers`].
/// * `diagnostics_events_dropped_total` -- crosses. A plain counter of what
///   the diagnostics feed itself lost, and the one number on this page that
///   is a real measurement from the first moment the registry runs.
pub(crate) fn diagnostics_reading(snapshot: &DiagnosticsSnapshot) -> DiagnosticsReading {
    let DiagnosticsSnapshot {
        schema_version: _,
        snapshot_sequence: _,
        captured_at_utc: _,
        process_elapsed_ns: _,
        active_session_id,
        health,
        session: _,
        receivers,
        diagnostics_events_dropped_total,
    } = snapshot;

    DiagnosticsReading {
        health: health_of(*health),
        events_dropped_total: *diagnostics_events_dropped_total,
        measured_receivers: if active_session_id.is_some() {
            measured_receivers(receivers)
        } else {
            0
        },
    }
}

/// Translates the registry's health badge into the shell's own word.
///
/// A wildcard-free match, so a fifth registry state cannot arrive silently as
/// one of these four.
fn health_of(health: Health) -> DiagnosticsHealth {
    match health {
        Health::Unknown => DiagnosticsHealth::Unknown,
        Health::Running => DiagnosticsHealth::Running,
        Health::Attention => DiagnosticsHealth::Attention,
        Health::Error => DiagnosticsHealth::Error,
    }
}

/// How many receivers the registry currently holds measurements for.
///
/// A count, and the destructuring above it is the point rather than a
/// formality. Every field of a receiver row is named and refused here, which
/// is what makes "the window learns the number of rows and nothing else"
/// something the compiler keeps true: a seventh field added to
/// [`ReceiverDiagnosticsSnapshot`] does not compile until it appears in this
/// pattern, and whoever adds it has to decide in this file whether the window
/// may see it.
///
/// Every one of the six is refused today, and none of them for a small
/// reason:
///
/// * `key` -- a session UUID and a MAC-derived `ReceiverId`. Raw identity.
/// * `lifecycle` -- carries a `UserFacingError` whose text is free-form.
/// * `timing`, `transport`, `feedback` -- presentation-safe numbers, and the
///   natural next step, but a per-receiver row needs a receiver *name* to sit
///   next to, and the name lives in the device snapshot rather than here.
///   Half a row -- numbers with no receiver against them -- would be worse
///   than none.
/// * `last_error` -- classification is safe, but `public_message` and
///   `technical_detail` are unbounded strings and the support export withholds
///   both. What that export refuses to put in a file a user sends to support,
///   this page does not put on screen permanently.
fn measured_receivers(receivers: &[ReceiverDiagnosticsSnapshot]) -> usize {
    receivers
        .iter()
        .filter(|receiver| {
            let ReceiverDiagnosticsSnapshot {
                key: _,
                lifecycle: _,
                timing: _,
                transport: _,
                feedback: _,
                last_error: _,
            } = receiver;
            true
        })
        .count()
}

/// Translates one backend event into the shell's vocabulary, or drops it.
///
/// The snapshot is authoritative for state, so almost everything here is
/// deliberately dropped. The one exception is the discovery notice: the
/// snapshot's `Retrying` phase says discovery is failing, but a notice can
/// also arrive while the phase is `Running` -- the network-change monitor
/// refusing to register is reported that way -- and that condition would
/// otherwise never reach the shell at all.
///
/// The path is complete: `reducer` answers
/// [`ControllerEvent::DiscoveryFailed`] with a `DiscoveryFailed` notice, so
/// what leaves the bridge here is what the window shows. It was not always
/// so -- the reducer discarded this event in a catch-all until that arm was
/// added -- and the note survives because it names where the other half of
/// this path lives.
///
/// No string on this path is forwarded. `message` is free-form backend prose
/// and at least one emitter interpolates an error into it, so what the shell
/// learns is the code, rendered as the bridge's own
/// [`DISCOVERY_FAILED_SUMMARY`]. `severity` is dropped with it: the shell's
/// `DiscoveryFailed` has one severity, and a warning that is worth telling
/// the user about is worth telling them plainly.
pub(crate) fn project_event(
    event: &BackendEvent,
    seen: GenerationsSeen,
) -> Option<ControllerEvent> {
    match event {
        // Correlated group outcomes are delivered by GenerationEcho from the
        // retained snapshot history, so broadcast lag cannot lose a result.
        // A completed coalesced setting such as SetMasterVolume is not news.
        BackendEvent::CommandCompleted { id: _ } => None,
        BackendEvent::CommandFailed { id: _, error: _ } => None,
        BackendEvent::Notice {
            severity: _,
            code,
            scope: _,
            message: _,
        } => match code {
            BackendNoticeCode::DiscoveryFailed => Some(ControllerEvent::DiscoveryFailed {
                generation: seen.discovery,
                summary: DISCOVERY_FAILED_SUMMARY.to_owned(),
            }),
            // Session failure is taken from `SessionPhase::Failed`, which
            // carries its own redacted reason and cannot disagree with the
            // phase the window is showing.
            BackendNoticeCode::SessionFailed => None,
            // No shell vocabulary yet: capture loss, device-state write
            // failure, and internal queue overload.
            BackendNoticeCode::CaptureUnavailable
            | BackendNoticeCode::PersistenceWrite
            | BackendNoticeCode::QueueOverloaded => None,
        },
    }
}

/// Adapts the shell's private controller seam onto the backend command queue.
///
/// Translation is not one-to-one, and the differences are the point:
///
/// * A discovery refresh is not a command at all. The backend browses
///   continuously under its own supervisor, so a refresh only records the
///   generation the current inventory must be answered under.
/// * One `StartSession` is three backend settings, because the backend
///   separates membership, volume, and run intent and coalesces each.
/// * All three queue slots are reserved before any command is sent, so Busy
///   cannot leave a partially admitted start. Run intent is still sent last.
pub(crate) struct BackendPort {
    handle: DeviceBackendHandle,
    echo: Arc<GenerationEcho>,
    /// The other half of the endpoint-key translation, shared with the bridge
    /// thread that fills it. Without it a capture selection could not leave
    /// the window at all, because the window is never told the ID.
    endpoints: Arc<EndpointDirectory>,
}

impl BackendPort {
    pub(crate) fn new(
        handle: DeviceBackendHandle,
        echo: Arc<GenerationEcho>,
        endpoints: Arc<EndpointDirectory>,
    ) -> Self {
        Self {
            handle,
            echo,
            endpoints,
        }
    }

    /// Translates one capture request back into a backend preference.
    ///
    /// `None` only for a key the directory does not know, which the caller
    /// turns into no command at all.
    fn preference_of(&self, request: AudioEndpointRequest) -> Option<AudioEndpointPreference> {
        match request {
            AudioEndpointRequest::SystemDefault => Some(AudioEndpointPreference::SystemDefault),
            AudioEndpointRequest::Endpoint(key) => self.endpoints.preference(key),
        }
    }

    fn send(&self, command: BackendCommand) -> Result<(), ControllerSendError> {
        self.handle
            .try_send(command)
            .map(|_id| ())
            .map_err(|error| match error {
                BackendSendError::Busy => ControllerSendError::Busy,
                BackendSendError::Closed => ControllerSendError::Closed,
            })
    }
}

/// The backend profile one shell latency choice names.
///
/// Written out without a rest pattern, like `latency_choice` going the other
/// way, so the two directions cannot drift apart silently.
fn latency_preset(choice: LatencyChoice) -> LatencyPreset {
    match choice {
        LatencyChoice::Low => LatencyPreset::Low,
        LatencyChoice::Normal => LatencyPreset::Normal,
        LatencyChoice::Stable => LatencyPreset::Stable,
    }
}

/// Total conversion of a shell volume into a validated backend volume.
///
/// The reducer already clamps and rejects non-finite input, so this is
/// defence in depth -- but it must stay total. Dropping the command on a
/// value the backend refuses would leave the group at a level the user did
/// not choose, with nothing on screen saying the change was lost.
fn volume_of(value: f32) -> Volume {
    if !value.is_finite() {
        tracing::warn!("non-finite volume replaced by the default");
        return Volume::DEFAULT_MASTER;
    }
    Volume::new(value.clamp(0.0, 1.0)).unwrap_or(Volume::DEFAULT_MASTER)
}

impl ControllerPort for BackendPort {
    fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError> {
        match command {
            DeviceCommand::PlayListeningTone { generation } => {
                let guard = self
                    .echo
                    .listening_target
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .filter(|(g, _)| *g == generation && self.echo.session() == generation)
                    .map(|(_, guard)| guard.clone());
                let mut pending = self
                    .echo
                    .listening_pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if pending.is_some() {
                    return Err(ControllerSendError::Busy);
                }
                let receipt = guard.and_then(|guard| {
                    self.handle
                        .try_send_confirmed(BackendCommand::RunListeningTest(guard))
                        .ok()
                        .map(|(_, receipt)| receipt)
                });
                *pending = Some(VolumePending {
                    generation,
                    receipt,
                });
                drop(pending);
                self.echo.wake.notify_one();
                Ok(())
            }
            DeviceCommand::Group {
                request,
                generation,
                command,
            } => {
                use crate::app::{GroupCommand, GroupFailure};
                use homepod_cast::backend::model::{SavedGroupId, SavedGroupMember};
                let mut pending = self
                    .echo
                    .group
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if pending.is_some() {
                    return Err(ControllerSendError::Busy);
                }
                let translated = match command {
                    GroupCommand::Save { id, name, members } => members
                        .into_iter()
                        .map(|member| {
                            Volume::new(member.level).map(|level| SavedGroupMember {
                                receiver: ReceiverId::from(member.receiver),
                                last_known_name: member.name,
                                level,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|members| BackendCommand::SaveGroup {
                            id: id.map(|id| SavedGroupId::from_uuid(id.0)),
                            name,
                            members,
                        })
                        .map_err(|_| GroupFailure::Validation),
                    GroupCommand::Delete(id) => {
                        Ok(BackendCommand::DeleteGroup(SavedGroupId::from_uuid(id.0)))
                    }
                    GroupCommand::Apply { id, start } => Ok(BackendCommand::ActivateSavedGroup {
                        id: SavedGroupId::from_uuid(id.0),
                        start,
                    }),
                    GroupCommand::ApplySelection { receiver_ids } => {
                        Ok(BackendCommand::ApplyDesiredMembers {
                            members: receiver_ids.into_iter().map(ReceiverId::from).collect(),
                        })
                    }
                };
                let admission = translated.and_then(|command| {
                    self.handle
                        .try_send_confirmed(command)
                        .map_err(|error| match error {
                            BackendSendError::Busy => GroupFailure::Busy,
                            BackendSendError::Closed => GroupFailure::Closed,
                        })
                });
                if let Some(generation) = generation {
                    self.echo.record_session(
                        generation,
                        if admission.is_ok() {
                            SessionEdge::PendingGroupStart
                        } else {
                            SessionEdge::Rejected
                        },
                    );
                }
                *pending = Some(GroupPending {
                    request,
                    generation,
                    admission,
                });
                drop(pending);
                self.echo.wake.notify_one();
                // Even a refusal is retained for the bridge to deliver reliably.
                Ok(())
            }
            DeviceCommand::Discover { generation } => {
                // No backend command exists for this: discovery is a daemon.
                // Liveness therefore has to be asked for rather than inferred
                // from a send, or a refresh would report success against a
                // dead backend and the window would spin forever.
                if self.handle.is_closed() {
                    return Err(ControllerSendError::Closed);
                }
                self.echo.record_discovery(generation);
                Ok(())
            }
            DeviceCommand::StartSession {
                generation,
                receiver_ids,
                volume,
            } => {
                let members: BTreeSet<ReceiverId> =
                    receiver_ids.into_iter().map(ReceiverId::from).collect();
                let outcome = self
                    .handle
                    .try_send_batch([
                        BackendCommand::SetDesiredMembers { members },
                        BackendCommand::SetMasterVolume(volume_of(volume)),
                        BackendCommand::SetRunIntent(RunIntent::Running),
                    ])
                    .map(|_| ())
                    .map_err(|error| match error {
                        BackendSendError::Busy => ControllerSendError::Busy,
                        BackendSendError::Closed => ControllerSendError::Closed,
                    });
                self.echo.record_session(
                    generation,
                    if outcome == Err(ControllerSendError::Busy) {
                        SessionEdge::Rejected
                    } else {
                        SessionEdge::Start
                    },
                );
                outcome
            }
            DeviceCommand::StopSession { generation } => {
                let outcome = self.send(BackendCommand::SetRunIntent(RunIntent::Stopped));
                self.echo.record_session(
                    generation,
                    if outcome == Err(ControllerSendError::Busy) {
                        SessionEdge::Rejected
                    } else {
                        SessionEdge::Stop
                    },
                );
                outcome
            }
            DeviceCommand::ApplyVolume { generation, volume } => {
                let result = self
                    .handle
                    .try_send_confirmed(BackendCommand::SetMasterVolume(volume_of(volume)));
                let mut pending = self.echo.volume.lock().unwrap_or_else(|e| e.into_inner());
                match result {
                    Ok((_, receipt)) => {
                        *pending = Some(VolumePending {
                            generation,
                            receipt: Some(receipt),
                        });
                        self.echo.wake.notify_one();
                        Ok(())
                    }
                    Err(error) => {
                        *pending = Some(VolumePending {
                            generation,
                            receipt: None,
                        });
                        self.echo.wake.notify_one();
                        Err(match error {
                            BackendSendError::Busy => ControllerSendError::Busy,
                            BackendSendError::Closed => ControllerSendError::Closed,
                        })
                    }
                }
            }
            // Other coalesced settings are displayed from confirmed snapshots.
            DeviceCommand::ApplyMute(muted) => self.send(BackendCommand::SetMuted(muted)),
            DeviceCommand::ApplyReceiverLevel { receiver, level } => {
                self.send(BackendCommand::SetReceiverLevel {
                    receiver: ReceiverId::from(receiver),
                    level: volume_of(level),
                })
            }
            DeviceCommand::ApplyLatency(choice) => {
                self.send(BackendCommand::SetLatencyPreset(latency_preset(choice)))
            }
            DeviceCommand::ApplyAudioEndpoint(request) => {
                let Some(preference) = self.preference_of(request) else {
                    // A key this process never handed out. Unreachable from
                    // the window -- the directory keeps every key it ever
                    // allocated, so a key that is on screen resolves -- and
                    // the only safe answer to it is no command at all.
                    // Selecting *some* endpoint here would mean silently
                    // capturing a device the user did not name, which is the
                    // one outcome the opaque key exists to prevent. The
                    // window keeps showing the backend's last confirmed
                    // selection until an accepted change is reported.
                    tracing::warn!("an unknown capture endpoint was selected; ignored");
                    return Ok(());
                };
                self.send(BackendCommand::SetAudioEndpoint(preference))
            }
            DeviceCommand::Shutdown => self.send(BackendCommand::Shutdown),
        }
    }
}

// ---------------------------------------------------------------------------
// The bridge thread
// ---------------------------------------------------------------------------

/// How long a refused shell event waits before the bridge offers it again.
///
/// Needed because the two halves are driven by different things. A refusal is
/// a fact about the *shell's* queue, and nothing on either backend feed has to
/// happen afterwards: a backend that has settled publishes no further
/// revision, so without a tick of its own the refused event would wait for an
/// edge that never comes and the window would stay out of step for the rest of
/// the run. Short enough to be invisible, long enough that a shell which is
/// genuinely behind is not hammered while it catches up.
const REPAIR_INTERVAL: Duration = Duration::from_millis(100);

/// How often the bridge re-reads the diagnostics registry.
///
/// The one permanent timer in this file, and it needs the justification
/// [`repair_delay`] refuses to give itself. The registry publishes into an
/// `ArcSwap` and offers its readers no wakeup at all, so a poll is the only
/// way its snapshot reaches the shell -- unlike a repair tick, which has
/// something to do only while something is retained. Twice a second is a
/// cadence for a page a human is reading, not a measurement rate, and a wake
/// that finds nothing new costs one atomic load and a comparison of three
/// scalars.
const DIAGNOSTICS_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Spacing of [`BridgeHandle::wait_until_done`]'s poll; matches the legacy
/// device service's own shutdown poll so the exit path has one cadence.
const DONE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Handle on the bridge thread, held by the shell for exactly one question.
///
/// [`Self::is_done`] is the shell's only evidence that the backend has
/// finished tearing down. Both feeds close when the backend control thread's
/// future is dropped -- it owns the only `watch::Sender` and the last
/// `broadcast::Sender` -- so "both feeds closed" is precisely "the controller
/// returned", which is what the shutdown coordinator's device stage waits for.
///
/// Deliberately not a join handle. The bridge thread ends on its own when both
/// feeds close, and a `Drop` that joined it would turn a backend that failed
/// to close its feeds into a process that never exits -- the very failure the
/// exit valve exists to bound.
pub(crate) struct BridgeHandle {
    done: Arc<AtomicBool>,
}

impl BridgeHandle {
    /// Whether both backend feeds have closed, i.e. the backend is gone.
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Waits up to `timeout` for [`Self::is_done`], reporting what it found.
    ///
    /// Returns as soon as the flag is set; the timeout only bounds the wait so
    /// a backend that never finishes cannot hold the exit open.
    pub(crate) fn wait_until_done(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.is_done() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return self.is_done();
            }
            std::thread::sleep(DONE_POLL_INTERVAL);
        }
    }
}

/// Starts the thread that joins the two halves of this module.
///
/// Built like [`crate::diagnostics::run_registry_worker`]: one named std
/// thread owning a `current_thread` runtime and a single `tokio::select!`
/// loop. Nothing here holds a lock across an `await` -- the shell generations
/// are three atomics plus a `Notify`, and the newest snapshot is cloned out of
/// the watch guard in one statement that ends before the next `await`.
pub(crate) fn spawn_backend_bridge(
    updates: DeviceBackendUpdates,
    echo: Arc<GenerationEcho>,
    endpoints: Arc<EndpointDirectory>,
    events: AppEventSender,
) -> BridgeHandle {
    let DeviceBackendUpdates {
        state,
        events: backend_events,
        // The support export's feed. It goes to the registry the bridge
        // thread owns -- never to the window, which was the condition written
        // here while this receiver was still being dropped. That condition
        // still holds: what the shell learns from the registry is three
        // scalars chosen one at a time in `diagnostics_reading`, and no value
        // carried on this feed reaches a renderer.
        diagnostics,
        session_diagnostics,
    } = updates;

    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let spawned = std::thread::Builder::new()
        .name("openaircast-backend-bridge".to_owned())
        .spawn(move || {
            run_bridge(
                state,
                backend_events,
                diagnostics,
                session_diagnostics,
                echo,
                endpoints,
                events,
                thread_done,
            )
        });

    if let Err(error) = spawned {
        // Same rule the shutdown coordinator applies to its own valve: a
        // thread the OS refuses must not turn Quit into a window that never
        // closes. Without a bridge nothing translates either way, so the
        // window will report a lost controller on the first command; reporting
        // the backend as already finished keeps the exit bounded instead of
        // spending the whole device budget waiting for a thread that does not
        // exist.
        tracing::error!("could not start the backend bridge ({error}); the window will report a lost controller");
        done.store(true, Ordering::SeqCst);
    }

    BridgeHandle { done }
}

/// Hands as much of the outbox to the shell as its bounded queue accepts.
///
/// `Busy` leaves the remainder in place. The shell queue is 256 deep, so a
/// refusal means the actor is momentarily behind rather than gone -- and
/// dropping the event would leave the window out of step with a backend that
/// has no reason to publish again.
///
/// `Closed` means there is no window left to be out of step with. The outbox
/// is discarded and the caller stops deriving; the loop keeps running only so
/// that `done` still reports the backend truthfully.
fn flush(out: &AppEventSender, outbox: &mut VecDeque<ControllerEvent>, shell_gone: &mut bool) {
    while let Some(event) = outbox.front() {
        match out.try_send(AppEvent::Controller(event.clone())) {
            Ok(()) => {
                outbox.pop_front();
            }
            Err(AppUnavailable::Busy) => break,
            Err(AppUnavailable::Closed) => {
                *shell_gone = true;
                outbox.clear();
                break;
            }
        }
    }
}

/// How long the bridge may park before it offers a retained event again.
///
/// `None` while nothing is retained: a timer that ran on an empty outbox would
/// wake the thread ten times a second for the life of the app to discover it
/// has nothing to do.
///
/// Separated from the loop for the same reason as [`finished`]: forcing the
/// shell's queue to refuse a delivery from a *test* means winning a race
/// against the test's own reader, so a bridge that had lost the retry
/// altogether would still pass a thread-level test most of the time.
fn repair_delay(retained: usize) -> Option<Duration> {
    (retained > 0).then_some(REPAIR_INTERVAL)
}

/// Whether the bridge has nothing left to do, as a decision separate from the
/// schedule that produces it.
///
/// It is here rather than spelled into the loop because the loop cannot prove
/// it. `tokio::select!` picks at random among simultaneously ready branches,
/// so a bridge that stopped one feed too early would still pass a
/// thread-level test most of the time -- and "most of the time" is exactly
/// what a shutdown contract must not be. The rule itself is total and has
/// four cases, so it is checked as four cases.
///
/// One surviving feed is not a finished backend: the shell's exit path reads
/// this through `done` to learn that the backend control thread has returned,
/// and it returns only when it has dropped *both* senders.
fn finished(state_closed: bool, events_closed: bool) -> bool {
    state_closed && events_closed
}

/// Converts the last retained session binding into a fail-closed tombstone.
fn failed_session_state(state: &SessionDiagnosticsState) -> SessionDiagnosticsState {
    let generation = match state {
        SessionDiagnosticsState::Inactive { generation, .. }
        | SessionDiagnosticsState::Active { generation, .. } => *generation,
    };
    SessionDiagnosticsState::Inactive {
        generation,
        reason: SessionStopReason::Failed,
    }
}

/// Starts the diagnostics registry, or reports that it could not be started.
///
/// The registry `expect`s its own thread spawn, and a panic here would leave
/// `done` unset for the whole device budget on every exit -- the exact
/// failure [`spawn_backend_bridge`] already bounds for its own thread, with
/// the same reasoning. So the panic is caught and the bridge runs on without
/// a registry; the Diagnostics page then draws every registry tile marked
/// unmeasured wherever it draws them at all, which is what "no measurements
/// exist" is supposed to look like. (While nothing is running it draws its
/// empty state instead, as it does with a healthy registry -- that is the
/// page's answer to an idle window, not a claim about the registry.)
fn start_registry(
    feed: DiagnosticEventReceiver,
) -> Option<(DiagnosticsRegistry, DiagnosticsHandle)> {
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let clock: Arc<dyn DiagnosticsClock> = Arc::new(SystemDiagnosticsClock::new());
        DiagnosticsRegistry::start(feed, clock)
    }));
    match started {
        Ok(started) => Some(started),
        Err(_) => {
            tracing::error!(
                "the diagnostics registry could not be started; the Diagnostics page \
                 will report that nothing has been measured"
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_bridge(
    mut state: watch::Receiver<Arc<DeviceSnapshot>>,
    mut backend_events: broadcast::Receiver<BackendEvent>,
    diagnostics: DiagnosticEventReceiver,
    mut session_diagnostics: watch::Receiver<SessionDiagnosticsState>,
    echo: Arc<GenerationEcho>,
    endpoints: Arc<EndpointDirectory>,
    out: AppEventSender,
    done: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Same reasoning as the failed spawn above.
            tracing::error!("backend bridge runtime unavailable: {error}");
            done.store(true, Ordering::SeqCst);
            return;
        }
    };

    // Owned by this thread, and therefore dropped when the block below
    // returns -- which is what shuts the registry down: its `Drop` signals
    // the worker and joins it. Started after the runtime, so a runtime that
    // could not be built leaves no registry thread behind.
    let registry = start_registry(diagnostics);

    runtime.block_on(async move {
        let diagnostics = registry.as_ref().map(|(_, handle)| handle.clone());
        let mut diagnostics_sessions = DiagnosticsSessionTracker::default();
        // The newest reading offered to the shell. The registry republishes
        // on every diagnostic event it ingests and almost none of them move
        // these three values, so an unchanged reading is not published.
        let mut published: Option<DiagnosticsReading> = None;
        let mut published_live: Option<crate::app::LiveDiagnosticsReading> = None;
        let mut next_live_poll = std::time::Instant::now();
        let mut registry_stamp: Option<(u64, std::time::Instant)> = None;
        let mut projection = Projection::default();
        // At most one pass's worth of events: a new pass is derived only once
        // the previous one has been delivered, so this cannot grow with a
        // shell that stays behind.
        let mut outbox: VecDeque<ControllerEvent> = VecDeque::new();
        let mut state_closed = false;
        let mut events_closed = false;
        let mut session_diagnostics_closed = false;
        let mut closure_reported = false;
        let mut shell_gone = false;

        loop {
            flush(&out, &mut outbox, &mut shell_gone);

            if !session_diagnostics_closed {
                if let Some((registry, _)) = registry.as_ref() {
                    let state = session_diagnostics.borrow_and_update().clone();
                    diagnostics_sessions.reconcile(registry, &state);
                }
            }

            // Level-driven: every pass re-derives the whole answer from the
            // newest snapshot. Held back while the outbox is not empty, which
            // is what keeps a refusal from turning into a duplicate.
            if outbox.is_empty() && !shell_gone {
                // One statement: the watch guard is a read lock and is gone
                // before the next `await`.
                let snapshot = Arc::clone(&state.borrow_and_update());
                let (next, produced) = projection.project(&snapshot, echo.sample());
                echo.observe_listening_target(&snapshot, &produced);
                projection = next;
                // Published before the events that mention the keys, not
                // after: the window may act on an endpoint the moment it sees
                // it, and a key the command half cannot resolve yet would be
                // dropped. One synchronous statement, no `await` inside it.
                endpoints.publish(&projection.endpoints);
                outbox.extend(produced);
                outbox.extend(echo.group_events(&snapshot, state_closed));
                outbox.extend(echo.volume_events(&snapshot, state_closed));
                outbox.extend(echo.listening_events(&snapshot, state_closed));
                if state_closed && !closure_reported {
                    outbox.push_back(ControllerEvent::ChannelClosed {
                        summary: "the audio controller stopped".into(),
                    });
                    closure_reported = true;
                }
                flush(&out, &mut outbox, &mut shell_gone);
            }

            // Level-driven for the same reason as the pass above: the answer
            // is whatever the registry has published, re-derived each time,
            // and held back while the outbox is not empty so a refusal cannot
            // turn into a duplicate here either.
            if outbox.is_empty() && !shell_gone {
                if let Some(handle) = &diagnostics {
                    let registry_snapshot = handle.snapshot();
                    let reading = diagnostics_reading(&registry_snapshot);
                    if published != Some(reading) {
                        published = Some(reading);
                        outbox.push_back(ControllerEvent::DiagnosticsUpdated { reading });
                        flush(&out, &mut outbox, &mut shell_gone);
                    }
                    let now = std::time::Instant::now();
                    let seen = echo.sample();
                    if now >= next_live_poll
                        || published_live.as_ref().map(|r| r.generation) != Some(seen.session)
                    {
                        next_live_poll = now + Duration::from_millis(250);
                        if registry_stamp.as_ref().map(|(sequence, _)| *sequence)
                            != Some(registry_snapshot.snapshot_sequence)
                        {
                            registry_stamp = Some((registry_snapshot.snapshot_sequence, now));
                        }
                        let fresh = registry_stamp.is_some_and(|(_, stamp)| {
                            now.duration_since(stamp) < Duration::from_secs(2)
                        });
                        let backend = Arc::clone(&state.borrow());
                        let ready = !state_closed
                            && projection
                                .session
                                .as_ref()
                                .is_some_and(|(generation, status)| {
                                    *generation == seen.session
                                        && matches!(
                                            status,
                                            SessionEcho::Active(_) | SessionEcho::Degraded(_)
                                        )
                                });
                        let registration = ready
                            .then(|| diagnostics_sessions.active_registration())
                            .flatten();
                        let live = project_live_diagnostics(
                            &backend,
                            fresh.then_some(&registry_snapshot),
                            seen.session,
                            registration,
                        );
                        if published_live.as_ref() != Some(&live) {
                            published_live = Some(live.clone());
                            outbox.push_back(ControllerEvent::LiveDiagnosticsUpdated {
                                reading: live,
                            });
                            flush(&out, &mut outbox, &mut shell_gone);
                        }
                    }
                }
            }

            if finished(state_closed, events_closed) {
                // Backend teardown is done even if its last shell notification
                // must still wait for space. Keep repairing that delivery.
                done.store(true, Ordering::SeqCst);
                if outbox.is_empty() || shell_gone {
                    break;
                }
            }

            // Copies rather than the flags themselves: the branch handlers
            // below write the flags, and a branch future borrowing them would
            // not compile.
            let state_live = !state_closed;
            let events_live = !events_closed;
            let session_diagnostics_live = !session_diagnostics_closed;
            let repair = repair_delay(outbox.len());
            // Only while there is a registry to read. Without one this branch
            // is `pending` and the thread parks exactly as it did before.
            let registry_poll = diagnostics.as_ref().map(|_| DIAGNOSTICS_POLL_INTERVAL);

            tokio::select! {
                changed = async {
                    if state_live {
                        state.changed().await
                    } else {
                        std::future::pending::<Result<(), watch::error::RecvError>>().await
                    }
                } => {
                    if changed.is_err() {
                        state_closed = true;
                    }
                }
                received = async {
                    if events_live {
                        backend_events.recv().await
                    } else {
                        std::future::pending::<
                            Result<BackendEvent, broadcast::error::RecvError>,
                        >().await
                    }
                } => match received {
                    Ok(event) => {
                        if let Some(translated) = project_event(&event, echo.sample()) {
                            outbox.push_back(translated);
                        }
                    }
                    // Bounded and lossy by construction, and the snapshot is
                    // authoritative for state: a gap costs the entries, never
                    // the bridge. Treating it as a closed feed would silently
                    // stop translating notices for the rest of the run.
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "the backend event feed lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        events_closed = true;
                    }
                },
                changed = async {
                    if session_diagnostics_live {
                        session_diagnostics.changed().await
                    } else {
                        std::future::pending::<Result<(), watch::error::RecvError>>().await
                    }
                } => {
                    if changed.is_err() {
                        session_diagnostics_closed = true;
                        if let Some((registry, _)) = registry.as_ref() {
                            let failed = failed_session_state(&session_diagnostics.borrow());
                            diagnostics_sessions.reconcile(registry, &failed);
                        }
                    }
                }
                // A shell command that moves no backend state -- a refresh, a
                // start, a stop -- reaches the bridge only this way.
                () = echo.wait() => {}
                // Offers a retained event again; see `repair_delay`.
                () = async {
                    match repair {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {}
                // Re-reads the registry; see `DIAGNOSTICS_POLL_INTERVAL`.
                () = async {
                    match registry_poll {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {}
            }
        }

        done.store(true, Ordering::SeqCst);
    });
}

// ---------------------------------------------------------------------------
// The switch
// ---------------------------------------------------------------------------

/// What the shell's shutdown coordinator must allow its device stage once the
/// backend is behind it.
///
/// Derived, never stated. The backend gives its own teardown
/// [`SHUTDOWN_GROUP_BUDGET`] -- itself derived from the session supervisor's
/// group teardown -- and only *then* returns from its control thread and drops
/// the feeds this bridge watches. A device stage shorter than that would
/// report a timeout on every exit with one unreachable receiver and walk on
/// while the backend was still disconnecting; the coordinator's `exit_valve`
/// is derived from the budgets in force, so raising this raises the valve with
/// it rather than leaving the valve to fire inside the teardown it guards.
///
/// The margin on top is not a second guess at the teardown. The backend's
/// budget is measured from the moment it *begins* tearing down, and two things
/// happen outside it: getting the shutdown command into a queue that may
/// briefly be full, and one bridge wakeup afterwards to observe both feeds
/// closed and publish `done`. Neither is bounded by the backend's number, so
/// neither may be taken out of it.
const SWITCH_SHUTDOWN_SLACK: Duration = Duration::from_secs(1);

pub(crate) const DEVICE_SHUTDOWN_BUDGET: Duration =
    SHUTDOWN_GROUP_BUDGET.saturating_add(SWITCH_SHUTDOWN_SLACK);

/// The device stage of the bounded exit, with the backend behind it.
struct BackendShutdown {
    handle: DeviceBackendHandle,
    bridge: BridgeHandle,
}

impl DeviceShutdown for BackendShutdown {
    fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;

        // `Busy` is not a refusal: the actor drains its whole queue on every
        // wake, so the command lands as soon as there is room. Retried rather
        // than reported, because a shutdown that was merely queued behind a
        // slider burst is not a shutdown that failed.
        loop {
            match self.handle.try_send(BackendCommand::Shutdown) {
                // Closed: the actor is already gone, so there is nothing left
                // to ask. Whether it *finished* is still the bridge's answer.
                Ok(_) | Err(BackendSendError::Closed) => break,
                Err(BackendSendError::Busy) => {
                    if std::time::Instant::now() >= deadline {
                        return self.bridge.is_done();
                    }
                    std::thread::sleep(DONE_POLL_INTERVAL);
                }
            }
        }

        // Waits for evidence, not for a duration: both feeds close when the
        // backend control thread returns, which is the only thing that proves
        // its supervisors were torn down rather than abandoned.
        self.bridge
            .wait_until_done(deadline.saturating_duration_since(std::time::Instant::now()))
    }
}

/// Wires one already-started backend onto the shell's device seam.
///
/// Split out from [`backend_device_seam`] so the wiring can be asserted
/// against hand-built channels: everything below is pure plumbing, and the
/// only thing the caller adds is a backend that opens sockets, browses the
/// network, and takes the audio endpoint.
///
/// One [`GenerationEcho`] is shared by both halves, and that sharing is the
/// whole switch: the port records what the user asked for, the bridge answers
/// under exactly that generation, and the reducer's guards line up.
fn device_seam_from(
    handle: DeviceBackendHandle,
    updates: DeviceBackendUpdates,
    events: AppEventSender,
) -> DeviceSeam {
    let echo = Arc::new(GenerationEcho::new());
    let endpoints = Arc::new(EndpointDirectory::default());
    let bridge = spawn_backend_bridge(updates, Arc::clone(&echo), Arc::clone(&endpoints), events);
    DeviceSeam {
        controller: Arc::new(BackendPort::new(handle.clone(), echo, endpoints)),
        shutdown: Arc::new(BackendShutdown { handle, bridge }),
        budget: DEVICE_SHUTDOWN_BUDGET,
    }
}

/// Starts the resilience backend and hands the shell its device seam.
///
/// Reached through [`crate::app::DeviceStage::Backend`];
/// [`crate::device_service::legacy_device_seam`] is the other arm and the way
/// back. Which one the app runs is [`crate::app::ACTIVE_DEVICE_STAGE`], and
/// two tests there pin it.
/// Picks the legacy volume file this installation actually has.
///
/// Two of them shipped, and naming either one alone loses the other:
///
/// * `%APPDATA%\HomePodCast\volume.txt` -- written by `cast.rs::volume_file`
///   in every build up to `33521c2`;
/// * `%APPDATA%\volume.txt` -- written by
///   [`crate::device_service::legacy_volume_file`] since that refactor, which
///   moved the file up one directory while its own doc comment kept claiming
///   the old location.
///
/// This is the last launch that can read either. The backend migrates once
/// and then owns volume in `state-v1.json`, so a source that resolves to a
/// file the user never had silently hands them the default volume and makes
/// that permanent on the first backend write.
///
/// The newer writer wins when both exist: it is the one the last launch
/// updated, so the older file is by construction stale. When neither exists
/// the newer path is returned unchanged -- the migration finds nothing either
/// way, and the backend falls back to its own default.
pub(crate) fn legacy_volume_source(appdata_dir: &std::path::Path) -> std::path::PathBuf {
    let current = crate::device_service::legacy_volume_file(appdata_dir);
    if current.exists() {
        return current;
    }

    let historical = appdata_dir.join("HomePodCast").join("volume.txt");
    if historical.exists() {
        return historical;
    }

    current
}

/// The filesystem layout the shell hands the backend.
///
/// Split out of [`backend_device_seam`] so the one decision in it that can be
/// wrong without anything failing -- which file the volume migration reads --
/// is reachable from a test. The seam itself starts sockets and takes the
/// audio endpoint, so nothing below it can be asserted.
///
/// The same layout as `BackendConfig::for_current_user`, but rooted at the
/// folder `run()` has already validated rather than read from the environment
/// a second time: that constructor falls back to the OS temporary directory
/// when `APPDATA` is missing, which would silently put device state somewhere
/// the next launch does not look.
pub(crate) fn backend_config_for(appdata_dir: &std::path::Path) -> BackendConfig {
    BackendConfig {
        state_directory: appdata_dir.join("OpenAirCast"),
        legacy_volume_path: legacy_volume_source(appdata_dir),
    }
}

pub(crate) fn backend_device_seam(
    events: AppEventSender,
    appdata_dir: std::path::PathBuf,
) -> anyhow::Result<DeviceSeam> {
    let (handle, updates) = start_device_backend(backend_config_for(&appdata_dir))?;
    Ok(device_seam_from(handle, updates, events))
}

#[cfg(test)]
mod tests {
    fn confirm_group(
        envelope: homepod_cast::backend::command::CommandEnvelope,
        snapshot: &mut DeviceSnapshot,
        error: Option<homepod_cast::backend::model::CommandFailure>,
    ) {
        confirm_group_attempt(envelope, snapshot, error, 1);
    }

    fn confirm_group_attempt(
        envelope: homepod_cast::backend::command::CommandEnvelope,
        snapshot: &mut DeviceSnapshot,
        error: Option<homepod_cast::backend::model::CommandFailure>,
        backend_floor: u64,
    ) {
        snapshot.revision += 1;
        snapshot.command_outcomes.record(envelope.id, error);
        envelope
            .confirmation
            .unwrap()
            .send(homepod_cast::backend::command::CommandConfirmation {
                revision: snapshot.revision,
                error,
                session_generation_floor: error.is_none().then_some(backend_floor),
            })
            .unwrap();
    }
    #[test]
    fn saved_group_hotkey_start_cannot_forge_a_backend_desired_revision() {
        use crate::app::{reduce, AppEvent, AppState, Availability};
        let mut state = AppState::default();
        let (_, events) =
            Projection::default().project(&DeviceSnapshot::default(), GenerationsSeen::default());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        state.receivers = vec![crate::app::ReceiverState {
            id: DeviceId([1; 6]),
            name: "Online".into(),
            model: "HomePod".into(),
            availability: Availability::Available,
        }];
        let transition = reduce(&mut state, AppEvent::GlobalHotkeyPressed);
        assert_eq!(transition.effects.len(), 1);
        assert_eq!(
            state.desired_revision, 0,
            "only the backend may increment its revision"
        );
        assert!(
            state.desired_receivers.is_empty(),
            "the first automatic choice awaits persistence"
        );
    }
    #[test]
    fn saved_group_result_retry_and_closure_cannot_leave_or_settle_the_wrong_request() {
        use crate::app::{
            reduce, AppEvent, AppState, GroupCommand, GroupFailure, GroupId, GroupOperationStatus,
            UiSnapshot,
        };
        let mut state = AppState::default();
        let command = GroupCommand::Delete(GroupId(uuid::Uuid::nil()));
        reduce(&mut state, AppEvent::GroupRequested(command.clone()));
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request: 1,
                status: GroupOperationStatus::Failed(GroupFailure::Busy),
            }),
        );
        reduce(&mut state, AppEvent::GroupRequested(command.clone()));
        assert_eq!(state.group_operation.as_ref().unwrap().request, 2);
        let stale = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request: 1,
                status: GroupOperationStatus::Succeeded,
            }),
        );
        assert!(!stale.snapshot_changed);
        assert_eq!(
            state.group_operation.as_ref().unwrap().status,
            GroupOperationStatus::Pending
        );
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::ChannelClosed {
                summary: "closed".into(),
            }),
        );
        assert_eq!(
            UiSnapshot::from_state(&state)
                .group_operation
                .unwrap()
                .status,
            GroupOperationStatus::Failed(GroupFailure::Closed)
        );
        let after_close = reduce(&mut state, AppEvent::GroupRequested(command));
        assert!(after_close.effects.is_empty());
        assert_eq!(
            state.group_operation.as_ref().unwrap().status,
            GroupOperationStatus::Failed(GroupFailure::Closed)
        );
        state.shutting_down = true;
        let before = state.clone();
        reduce(
            &mut state,
            AppEvent::GroupRequested(GroupCommand::ApplySelection {
                receiver_ids: vec![],
            }),
        );
        assert_eq!(state, before);
    }

    #[test]
    fn saved_group_pending_result_survives_a_full_shell_queue() {
        use crate::app::{AppEvent, GroupCommand, GroupId, GroupOperationStatus};
        let (port, mut rx, echo) = port(2);
        port.try_send(DeviceCommand::Group {
            request: 7,
            generation: None,
            command: GroupCommand::Delete(GroupId(uuid::Uuid::nil())),
        })
        .unwrap();
        let envelope = rx.try_recv().unwrap();
        let mut snapshot = DeviceSnapshot::default();
        confirm_group(envelope, &mut snapshot, None);
        let mut outbox = std::collections::VecDeque::from(echo.group_events(&snapshot, false));
        let (tx, shell) = std::sync::mpsc::sync_channel(1);
        let out = crate::app_handle::AppEventSender::new(tx);
        out.try_send(AppEvent::RefreshRequested).unwrap();
        let mut gone = false;
        super::flush(&out, &mut outbox, &mut gone);
        assert_eq!(outbox.len(), 1);
        assert!(super::repair_delay(outbox.len()).is_some());
        shell.try_recv().unwrap();
        super::flush(&out, &mut outbox, &mut gone);
        assert!(outbox.is_empty());
        assert_eq!(
            shell.try_recv().unwrap(),
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request: 7,
                status: GroupOperationStatus::Succeeded
            })
        );
    }
    #[test]
    fn saved_group_projection_preserves_ids_offline_members_and_backend_echo() {
        use crate::app::{reduce, AppEvent, AppState, UiSnapshot};
        use homepod_cast::backend::model::{SavedGroupId, SavedGroupMember, SavedGroupSnapshot};
        let group = SavedGroupId::generate();
        let mut snapshot = DeviceSnapshot {
            desired_members: BTreeSet::from([ReceiverId::from(DeviceId([2; 6]))]),
            saved_groups: vec![SavedGroupSnapshot {
                id: group,
                name: "Evening".into(),
                members: vec![SavedGroupMember {
                    receiver: ReceiverId::from(DeviceId([2; 6])),
                    last_known_name: "Offline".into(),
                    level: Volume::new(0.7).unwrap(),
                }],
            }],
            ..Default::default()
        };
        let mut state = AppState::default();
        let (projection, events) =
            Projection::default().project(&snapshot, GenerationsSeen::default());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        let view = UiSnapshot::from_state(&state);
        assert_eq!(
            view.desired_receivers.as_ref(),
            &[DeviceId([2; 6])],
            "startup revision zero restores applied membership"
        );
        let saved = &view.saved_groups.unwrap()[0];
        assert_eq!(saved.id.0, group.as_uuid());
        assert_eq!(saved.members[0].name, "Offline");
        assert_eq!(saved.members[0].level, 0.7);
        assert!(saved.available_members.is_empty());
        assert!(projection
            .project(&snapshot, GenerationsSeen::default())
            .1
            .is_empty());
        snapshot.saved_groups.clear();
        let (_, events) = projection.project(&snapshot, GenerationsSeen::default());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert!(state.saved_groups.unwrap().is_empty());
        assert!(
            state.desired_receivers.contains(&DeviceId([2; 6])),
            "deleting a template retains applied selection"
        );
    }

    #[test]
    fn saved_group_out_of_order_eviction_does_not_release_a_running_request() {
        use crate::app::{GroupCommand, GroupId, GroupOperationStatus};
        let (port, mut rx, echo) = port(2);
        let command = GroupCommand::Delete(GroupId(uuid::Uuid::nil()));
        port.try_send(DeviceCommand::Group {
            request: 40,
            generation: None,
            command: command.clone(),
        })
        .unwrap();
        let envelope = rx.try_recv().unwrap();
        let mut snapshot = DeviceSnapshot::default();
        // A producer can allocate this ID, pause, then enqueue after these
        // later allocations have completed. None is evidence about this ID.
        for id in envelope.id + 1..envelope.id + 131 {
            snapshot.command_outcomes.record(id, None);
        }
        assert!(echo.group_events(&snapshot, false).is_empty());
        assert_eq!(
            port.try_send(DeviceCommand::Group {
                request: 41,
                generation: None,
                command
            }),
            Err(ControllerSendError::Busy)
        );
        let old_snapshot = snapshot.clone();
        let id = envelope.id;
        confirm_group(envelope, &mut snapshot, None);
        // The exact receipt cannot overtake its durable state publication.
        assert!(echo.group_events(&old_snapshot, false).is_empty());
        for completed in id + 131..id + 262 {
            snapshot.command_outcomes.record(completed, None);
        }
        assert!(!snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(completed, _)| *completed == id));
        assert_eq!(
            echo.group_events(&snapshot, false),
            vec![ControllerEvent::GroupOperationFinished {
                request: 40,
                status: GroupOperationStatus::Succeeded,
            }]
        );
        assert!(echo.group_events(&snapshot, false).is_empty());
    }

    #[test]
    fn saved_group_admission_failures_and_channel_loss_always_settle_the_request() {
        use crate::app::{GroupCommand, GroupFailure, GroupId, GroupOperationStatus};
        for (capacity, close, expected) in [
            (1, false, GroupFailure::Busy),
            (2, true, GroupFailure::Closed),
        ] {
            let (port, rx, echo) = port(capacity);
            port.handle
                .try_send(BackendCommand::SetMuted(true))
                .unwrap();
            if close {
                drop(rx);
            }
            port.try_send(DeviceCommand::Group {
                request: 9,
                generation: None,
                command: GroupCommand::Delete(GroupId(uuid::Uuid::nil())),
            })
            .unwrap();
            assert_eq!(
                echo.group_events(&DeviceSnapshot::default(), close),
                vec![ControllerEvent::GroupOperationFinished {
                    request: 9,
                    status: GroupOperationStatus::Failed(expected)
                }]
            );
        }
        let (port, mut rx, echo) = port(2);
        port.try_send(DeviceCommand::Group {
            request: 12,
            generation: None,
            command: GroupCommand::Delete(GroupId(uuid::Uuid::nil())),
        })
        .unwrap();
        let envelope = rx.try_recv().unwrap();
        let id = envelope.id;
        assert!(echo
            .group_events(&DeviceSnapshot::default(), false)
            .is_empty());
        let mut snapshot = DeviceSnapshot::default();
        for completed in id..id + 129 {
            snapshot.command_outcomes.record(completed, None);
        }
        assert_eq!(snapshot.command_outcomes.recent.len(), 128);
        assert!(echo.group_events(&snapshot, false).is_empty());
        // Actual loss of this command's producer, not an unrelated watermark.
        drop(envelope);
        assert_eq!(
            echo.group_events(&snapshot, false),
            vec![ControllerEvent::GroupOperationFinished {
                request: 12,
                status: GroupOperationStatus::Failed(GroupFailure::ConfirmationLost)
            }]
        );
        port.try_send(DeviceCommand::Group {
            request: 13,
            generation: None,
            command: GroupCommand::Delete(GroupId(uuid::Uuid::nil())),
        })
        .unwrap();
        assert_eq!(
            echo.group_events(&DeviceSnapshot::default(), true),
            vec![ControllerEvent::GroupOperationFinished {
                request: 13,
                status: GroupOperationStatus::Failed(GroupFailure::Closed)
            }]
        );
    }

    #[test]
    fn saved_group_apply_start_and_delete_are_distinct_single_commands() {
        use crate::app::{GroupCommand, GroupId};
        let group = GroupId(uuid::Uuid::new_v4());
        for start in [false, true] {
            let (port, mut rx, _) = port(4);
            port.try_send(DeviceCommand::Group {
                request: 3,
                generation: None,
                command: GroupCommand::Apply { id: group, start },
            })
            .unwrap();
            assert!(
                matches!(rx.try_recv().unwrap().command, BackendCommand::ActivateSavedGroup { id, start: actual } if id.as_uuid() == group.0 && actual == start)
            );
            assert!(rx.try_recv().is_err());
        }
        let (port, mut rx, _) = port(4);
        port.try_send(DeviceCommand::Group {
            request: 4,
            generation: None,
            command: GroupCommand::Delete(group),
        })
        .unwrap();
        assert!(
            matches!(rx.try_recv().unwrap().command, BackendCommand::DeleteGroup(id) if id.as_uuid() == group.0)
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn release_group_start_from_idle_is_cancellable_before_backend_activation() {
        use crate::app::{
            reduce, AppEvent, AppState, GroupCommand, GroupId, StreamState, UiSnapshot,
        };
        let (port, mut rx, echo) = port(8);
        let mut state = AppState::default();
        let backend = DeviceSnapshot::default();
        let (projection, _) = Projection::default().project(&backend, echo.sample());
        let transition = reduce(
            &mut state,
            AppEvent::GroupRequested(GroupCommand::Apply {
                id: GroupId(uuid::Uuid::new_v4()),
                start: true,
            }),
        );
        assert!(matches!(state.stream, StreamState::Starting { .. }));
        assert!(UiSnapshot::from_state(&state).can_stop);
        for effect in &transition.effects {
            port.try_send(crate::device_service::effect_to_command(effect).unwrap())
                .unwrap();
        }
        let (_, stale) = projection.project(&backend, echo.sample());
        for event in stale {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert!(matches!(state.stream, StreamState::Starting { .. }));
        let stop = reduce(&mut state, AppEvent::StopRequested);
        assert!(matches!(state.stream, StreamState::Stopping { .. }));
        for effect in &stop.effects {
            port.try_send(crate::device_service::effect_to_command(effect).unwrap())
                .unwrap();
        }
        assert!(matches!(
            rx.try_recv().unwrap().command,
            BackendCommand::ActivateSavedGroup { start: true, .. }
        ));
        assert!(matches!(
            rx.try_recv().unwrap().command,
            BackendCommand::SetRunIntent(RunIntent::Stopped)
        ));
        let (_, settled) = projection.project(&backend, echo.sample());
        for event in settled {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert_eq!(state.stream, StreamState::Stopped);
    }

    #[test]
    fn release_group_start_refusal_reprojects_actual_idle_without_overriding_later_stop() {
        use crate::app::{
            reduce, AppEvent, AppState, GroupCommand, GroupId, GroupOperationStatus, StreamState,
        };
        use homepod_cast::backend::model::CommandFailure;
        for cancel_first in [false, true] {
            let (port, mut rx, echo) = port(8);
            let mut state = AppState::default();
            let mut backend = DeviceSnapshot::default();
            let (projection, _) = Projection::default().project(&backend, echo.sample());
            let transition = reduce(
                &mut state,
                AppEvent::GroupRequested(GroupCommand::Apply {
                    id: GroupId(uuid::Uuid::new_v4()),
                    start: true,
                }),
            );
            port.try_send(
                crate::device_service::effect_to_command(&transition.effects[0]).unwrap(),
            )
            .unwrap();
            let group = rx.try_recv().unwrap();
            if cancel_first {
                let stop = reduce(&mut state, AppEvent::StopRequested);
                port.try_send(crate::device_service::effect_to_command(&stop.effects[0]).unwrap())
                    .unwrap();
            }
            confirm_group(group, &mut backend, Some(CommandFailure::Persistence));
            for event in echo.group_events(&backend, false) {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert!(matches!(
                state.group_operation.as_ref().unwrap().status,
                GroupOperationStatus::Failed(_)
            ));
            assert_eq!(
                echo.session_edge(),
                if cancel_first {
                    SessionEdge::Stop
                } else {
                    SessionEdge::Rejected
                }
            );
            let (_, events) = projection.project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(state.stream, StreamState::Stopped);
        }
    }

    #[test]
    fn release_group_start_success_waits_for_actual_stream_and_ignores_old_generation() {
        use crate::app::{reduce, AppEvent, AppState, GroupCommand, GroupId, StreamState};
        let (port, mut rx, echo) = port(8);
        let mut state = AppState::default();
        let mut backend = DeviceSnapshot::default();
        let (projection, _) = Projection::default().project(&backend, echo.sample());
        let transition = reduce(
            &mut state,
            AppEvent::GroupRequested(GroupCommand::Apply {
                id: GroupId(uuid::Uuid::new_v4()),
                start: true,
            }),
        );
        port.try_send(crate::device_service::effect_to_command(&transition.effects[0]).unwrap())
            .unwrap();
        let started_under = state.generations.session;
        confirm_group(rx.try_recv().unwrap(), &mut backend, None);
        for event in echo.group_events(&backend, false) {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert!(
            matches!(state.stream, StreamState::Starting { .. }),
            "a durable operation success is not audio delivery"
        );
        backend.session.phase =
            homepod_cast::backend::model::SessionPhase::Streaming { generation: 1 };
        let (_, events) = projection.project(&backend, echo.sample());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert!(matches!(state.stream, StreamState::Streaming { .. }));
        reduce(&mut state, AppEvent::StopRequested);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation: started_under,
                active_receiver_ids: vec![],
            }),
        );
        assert!(matches!(state.stream, StreamState::Stopping { .. }));
    }

    #[test]
    fn release_group_start_queue_refusal_restores_idle() {
        use crate::app::{
            reduce, AppEvent, AppState, GroupCommand, GroupFailure, GroupId, GroupOperationStatus,
            StreamState,
        };
        let (port, _rx, echo) = port(1);
        port.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
            .unwrap();
        let mut state = AppState::default();
        let backend = DeviceSnapshot::default();
        let (projection, _) = Projection::default().project(&backend, echo.sample());
        let transition = reduce(
            &mut state,
            AppEvent::GroupRequested(GroupCommand::Apply {
                id: GroupId(uuid::Uuid::new_v4()),
                start: true,
            }),
        );
        port.try_send(crate::device_service::effect_to_command(&transition.effects[0]).unwrap())
            .unwrap();
        let (_, events) = projection.project(&backend, echo.sample());
        for event in events.into_iter().chain(echo.group_events(&backend, false)) {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert_eq!(state.stream, StreamState::Stopped);
        assert_eq!(
            state.group_operation.unwrap().status,
            GroupOperationStatus::Failed(GroupFailure::Busy)
        );
    }

    #[test]
    fn release_group_start_does_not_adopt_the_previous_stops_backend_completion() {
        use crate::app::{
            reduce, AppEvent, AppState, GenerationId, GroupCommand, GroupId, StreamState,
            UiSnapshot,
        };
        use homepod_cast::backend::model::SessionPhase;

        fn project(
            projection: &mut Projection,
            backend: &DeviceSnapshot,
            echo: &GenerationEcho,
            state: &mut AppState,
        ) {
            let (next, events) = projection.project(backend, echo.sample());
            *projection = next;
            for event in events {
                reduce(state, AppEvent::Controller(event));
            }
        }

        for confirm_before_old_stop in [false, true] {
            let (port, mut rx, echo) = port(8);
            let mut state = AppState::default();
            state.generations.session = GenerationId(6);
            echo.record_session(GenerationId(6), SessionEdge::Start);
            let mut backend = DeviceSnapshot::default();
            backend.session.phase = SessionPhase::Streaming { generation: 30 };
            let mut projection = Projection::default();
            project(&mut projection, &backend, &echo, &mut state);
            let stop = reduce(&mut state, AppEvent::StopRequested);
            port.try_send(crate::device_service::effect_to_command(&stop.effects[0]).unwrap())
                .unwrap();
            rx.try_recv().unwrap();
            backend.session.phase = SessionPhase::Stopping { generation: 30 };
            project(&mut projection, &backend, &echo, &mut state);
            let start = reduce(
                &mut state,
                AppEvent::GroupRequested(GroupCommand::Apply {
                    id: GroupId(uuid::Uuid::new_v4()),
                    start: true,
                }),
            );
            port.try_send(crate::device_service::effect_to_command(&start.effects[0]).unwrap())
                .unwrap();
            let mut group = Some(rx.try_recv().unwrap());
            assert_eq!(
                state.stream,
                StreamState::Starting {
                    generation: GenerationId(8)
                }
            );
            if confirm_before_old_stop {
                confirm_group_attempt(group.take().unwrap(), &mut backend, None, 31);
                for event in echo.group_events(&backend, false) {
                    reduce(&mut state, AppEvent::Controller(event));
                }
            }
            backend.session.phase = SessionPhase::Stopped;
            project(&mut projection, &backend, &echo, &mut state);
            assert_eq!(
                state.stream,
                StreamState::Starting {
                    generation: GenerationId(8)
                },
                "a changed backend phase from the prior Stop is not the new Start's answer"
            );
            assert!(UiSnapshot::from_state(&state).can_stop);
            backend.session.phase = SessionPhase::Starting { generation: 31 };
            project(&mut projection, &backend, &echo, &mut state);
            assert!(UiSnapshot::from_state(&state).can_stop);
            if let Some(group) = group {
                confirm_group_attempt(group, &mut backend, None, 31);
                for event in echo.group_events(&backend, false) {
                    reduce(&mut state, AppEvent::Controller(event));
                }
            }
            assert_eq!(
                state.stream,
                StreamState::Starting {
                    generation: GenerationId(8)
                },
                "durable success is not audio delivery"
            );
            backend.session.phase = SessionPhase::Streaming { generation: 31 };
            project(&mut projection, &backend, &echo, &mut state);
            assert_eq!(
                state.stream,
                StreamState::Streaming {
                    generation: GenerationId(8)
                }
            );
            let cancel = reduce(&mut state, AppEvent::StopRequested);
            port.try_send(crate::device_service::effect_to_command(&cancel.effects[0]).unwrap())
                .unwrap();
            project(&mut projection, &backend, &echo, &mut state);
            assert_eq!(
                state.stream,
                StreamState::Stopping {
                    generation: GenerationId(9)
                }
            );
            backend.session.phase = SessionPhase::Stopped;
            project(&mut projection, &backend, &echo, &mut state);
            assert_eq!(state.stream, StreamState::Stopped);
        }
    }

    #[test]
    fn release_group_start_does_not_accept_active_or_failed_before_its_exact_receipt() {
        use crate::app::{
            reduce, AppEvent, AppState, GenerationId, GroupCommand, GroupId, StreamState,
        };
        use homepod_cast::backend::model::{SessionPhase, UserFacingError};
        for previous_phase in [
            SessionPhase::Streaming { generation: 30 },
            SessionPhase::Failed {
                generation: 30,
                error: UserFacingError::new("previous session failed"),
            },
        ] {
            let (port, mut rx, echo) = port(8);
            let mut state = AppState::default();
            state.generations.session = GenerationId(6);
            echo.record_session(GenerationId(6), SessionEdge::Start);
            let mut backend = DeviceSnapshot::default();
            backend.session.phase = SessionPhase::Streaming { generation: 30 };
            let (projection, events) = Projection::default().project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            let stop = reduce(&mut state, AppEvent::StopRequested);
            port.try_send(crate::device_service::effect_to_command(&stop.effects[0]).unwrap())
                .unwrap();
            rx.try_recv().unwrap();
            let start = reduce(
                &mut state,
                AppEvent::GroupRequested(GroupCommand::Apply {
                    id: GroupId(uuid::Uuid::new_v4()),
                    start: true,
                }),
            );
            port.try_send(crate::device_service::effect_to_command(&start.effects[0]).unwrap())
                .unwrap();
            let group = rx.try_recv().unwrap();
            backend.session.phase = previous_phase;
            let (_, events) = projection.project(&backend, echo.sample());
            for event in events.into_iter().chain(echo.group_events(&backend, false)) {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Starting {
                    generation: GenerationId(8)
                }
            );
            assert!(state.notice.is_none());
            drop(group);
        }
    }

    use homepod_cast::backend::model::SessionPhase;

    fn assert_post_receipt_predecessor_is_not_new_group_start(previous_phase: SessionPhase) {
        use crate::app::{
            reduce, AppEvent, AppState, GenerationId, GroupCommand, GroupId, StreamState,
        };
        let (port, mut rx, echo) = port(8);
        let mut state = AppState::default();
        state.generations.session = GenerationId(6);
        echo.record_session(GenerationId(6), SessionEdge::Start);
        let mut backend = DeviceSnapshot::default();
        backend.session.phase = SessionPhase::Streaming { generation: 30 };
        let (projection, events) = Projection::default().project(&backend, echo.sample());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        let stop = reduce(&mut state, AppEvent::StopRequested);
        port.try_send(crate::device_service::effect_to_command(&stop.effects[0]).unwrap())
            .unwrap();
        rx.try_recv().unwrap();
        let start = reduce(
            &mut state,
            AppEvent::GroupRequested(GroupCommand::Apply {
                id: GroupId(uuid::Uuid::new_v4()),
                start: true,
            }),
        );
        port.try_send(crate::device_service::effect_to_command(&start.effects[0]).unwrap())
            .unwrap();
        confirm_group_attempt(rx.try_recv().unwrap(), &mut backend, None, 31);
        for event in echo.group_events(&backend, false) {
            reduce(&mut state, AppEvent::Controller(event));
        }
        backend.session.phase = previous_phase;
        let (_, events) = projection.project(&backend, echo.sample());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert_eq!(
            state.stream,
            StreamState::Starting {
                generation: GenerationId(8)
            }
        );
        assert!(
            state.notice.is_none(),
            "a predecessor failure cannot become this start's failure"
        );
    }

    #[test]
    fn release_group_start_post_receipt_rejects_predecessor_active() {
        assert_post_receipt_predecessor_is_not_new_group_start(SessionPhase::Streaming {
            generation: 30,
        });
    }

    #[test]
    fn release_group_start_post_receipt_rejects_predecessor_failed() {
        assert_post_receipt_predecessor_is_not_new_group_start(SessionPhase::Failed {
            generation: 30,
            error: homepod_cast::backend::model::UserFacingError::new("previous session failed"),
        });
    }

    #[test]
    fn release_group_start_receipt_cannot_replace_a_newer_cancel() {
        use crate::app::{reduce, AppEvent, AppState, GroupCommand, GroupId, StreamState};
        for cancel_before_receipt in [false, true] {
            let (port, mut rx, echo) = port(8);
            let mut state = AppState::default();
            let mut backend = DeviceSnapshot::default();
            let (projection, _) = Projection::default().project(&backend, echo.sample());
            let start = reduce(
                &mut state,
                AppEvent::GroupRequested(GroupCommand::Apply {
                    id: GroupId(uuid::Uuid::new_v4()),
                    start: true,
                }),
            );
            port.try_send(crate::device_service::effect_to_command(&start.effects[0]).unwrap())
                .unwrap();
            let mut envelope = Some(rx.try_recv().unwrap());
            if !cancel_before_receipt {
                confirm_group_attempt(envelope.take().unwrap(), &mut backend, None, 31);
                for event in echo.group_events(&backend, false) {
                    reduce(&mut state, AppEvent::Controller(event));
                }
            }
            let stop = reduce(&mut state, AppEvent::StopRequested);
            port.try_send(crate::device_service::effect_to_command(&stop.effects[0]).unwrap())
                .unwrap();
            if let Some(envelope) = envelope {
                confirm_group_attempt(envelope, &mut backend, None, 31);
                for event in echo.group_events(&backend, false) {
                    reduce(&mut state, AppEvent::Controller(event));
                }
            }
            assert_eq!(echo.session_edge(), SessionEdge::Stop);
            backend.session.phase = SessionPhase::Streaming { generation: 31 };
            let (projection, events) = projection.project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert!(matches!(state.stream, StreamState::Stopping { .. }));
            backend.session.phase = SessionPhase::Stopped;
            let (_, events) = projection.project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(state.stream, StreamState::Stopped);
        }
    }

    #[test]
    fn release_group_start_accepts_its_early_recovery_and_failure_without_starting_snapshot() {
        use crate::app::{
            reduce, AppEvent, AppState, GenerationId, GroupCommand, GroupId, StreamState,
            UiSnapshot,
        };
        use homepod_cast::backend::model::{RestartReason, UserFacingError};
        for phase in [
            SessionPhase::Restarting {
                generation: 31,
                reason: RestartReason::DeadRtspRecovered,
            },
            SessionPhase::Failed {
                generation: 31,
                error: UserFacingError::new("new attempt failed"),
            },
        ] {
            let (port, mut rx, echo) = port(8);
            let mut state = AppState::default();
            let mut backend = DeviceSnapshot::default();
            let (projection, _) = Projection::default().project(&backend, echo.sample());
            let start = reduce(
                &mut state,
                AppEvent::GroupRequested(GroupCommand::Apply {
                    id: GroupId(uuid::Uuid::new_v4()),
                    start: true,
                }),
            );
            port.try_send(crate::device_service::effect_to_command(&start.effects[0]).unwrap())
                .unwrap();
            confirm_group_attempt(rx.try_recv().unwrap(), &mut backend, None, 31);
            for event in echo.group_events(&backend, false) {
                reduce(&mut state, AppEvent::Controller(event));
            }
            backend.session.phase = phase.clone();
            let (projection, events) = projection.project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            if matches!(phase, SessionPhase::Failed { .. }) {
                assert_eq!(state.stream, StreamState::Stopped);
                assert!(state.notice.is_some());
            } else {
                assert_eq!(
                    state.stream,
                    StreamState::Restarting {
                        generation: GenerationId(1)
                    }
                );
                assert!(UiSnapshot::from_state(&state).can_stop);
                assert!(state.notice.is_none());
                // Normal same-attempt health and spontaneous stop updates still
                // work after the new attempt, without another command receipt.
                backend.session.phase = SessionPhase::Streaming { generation: 31 };
                let (projection, events) = projection.project(&backend, echo.sample());
                for event in events {
                    reduce(&mut state, AppEvent::Controller(event));
                }
                assert!(matches!(state.stream, StreamState::Streaming { .. }));
                backend.session.phase = SessionPhase::Stopped;
                let (_, events) = projection.project(&backend, echo.sample());
                for event in events {
                    reduce(&mut state, AppEvent::Controller(event));
                }
                assert_eq!(state.stream, StreamState::Stopped);
            }
        }
    }

    #[test]
    fn saved_group_stopped_selection_is_durable_and_keeps_offline_members_on_start() {
        use crate::app::{reduce, AppEvent, AppState, Availability, GroupOperationStatus};
        let mut state = AppState::default();
        let (_, events) =
            Projection::default().project(&DeviceSnapshot::default(), GenerationsSeen::default());
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        state.receivers = vec![crate::app::ReceiverState {
            id: DeviceId([1; 6]),
            name: "Online".into(),
            model: "HomePod".into(),
            availability: Availability::Available,
        }];
        reduce(&mut state, AppEvent::ToggleStagedReceiver(DeviceId([1; 6])));
        reduce(&mut state, AppEvent::ToggleStagedReceiver(DeviceId([2; 6])));
        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);
        assert!(state.desired_receivers.is_empty());
        assert_eq!(state.desired_revision, 0);
        let (port, mut rx, echo) = port(4);
        port.try_send(crate::device_service::effect_to_command(&transition.effects[0]).unwrap())
            .unwrap();
        let sent = rx.try_recv().unwrap();
        assert!(
            matches!(&sent.command, BackendCommand::ApplyDesiredMembers { members } if members.len() == 2)
        );
        assert!(
            rx.try_recv().is_err(),
            "selection apply alone must never start"
        );
        let mut snapshot = DeviceSnapshot {
            desired_revision: 1,
            desired_members: BTreeSet::from([
                ReceiverId::from(DeviceId([1; 6])),
                ReceiverId::from(DeviceId([2; 6])),
            ]),
            ..Default::default()
        };
        confirm_group(sent, &mut snapshot, None);
        let (_, events) = Projection::default().project(&snapshot, GenerationsSeen::default());
        for event in events
            .into_iter()
            .chain(echo.group_events(&snapshot, false))
        {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert_eq!(
            state.group_operation.as_ref().unwrap().status,
            GroupOperationStatus::Succeeded
        );
        assert_eq!(state.desired_revision, 1);
        let transition = reduce(&mut state, AppEvent::StartRequested);
        assert!(
            matches!(&transition.effects[0], crate::app::AppEffect::StartSession { receiver_ids, .. } if receiver_ids.len() == 2)
        );
    }
    #[test]
    fn saved_group_contract_roundtrip_waits_for_echo_and_correlates_results() {
        use crate::app::{
            reduce, AppEvent, AppState, GroupCommand, GroupFailure, GroupMember,
            GroupOperationStatus,
        };
        let (port, mut rx, echo) = port(4);
        let mut state = AppState::default();
        let command = GroupCommand::Save {
            id: None,
            name: "Evening".into(),
            members: vec![GroupMember {
                receiver: DeviceId([2; 6]),
                name: "Offline".into(),
                level: 0.7,
            }],
        };
        let transition = reduce(&mut state, AppEvent::GroupRequested(command.clone()));
        assert_eq!(
            state.group_operation.as_ref().unwrap().status,
            GroupOperationStatus::Pending
        );
        assert!(state.saved_groups.is_none());
        assert!(reduce(&mut state, AppEvent::GroupRequested(command))
            .effects
            .is_empty());
        port.try_send(crate::device_service::effect_to_command(&transition.effects[0]).unwrap())
            .unwrap();
        let envelope = rx.try_recv().unwrap();
        match &envelope.command {
            BackendCommand::SaveGroup { id, name, members } => {
                assert_eq!(*id, None);
                assert_eq!(name, "Evening");
                assert_eq!(members[0].receiver, ReceiverId::from(DeviceId([2; 6])));
                assert_eq!(members[0].level.get(), 0.7);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "save must never enqueue playback");
        let mut snapshot = DeviceSnapshot::default();
        confirm_group(
            envelope,
            &mut snapshot,
            Some(homepod_cast::backend::model::CommandFailure::Persistence),
        );
        let events = echo.group_events(&snapshot, false);
        assert_eq!(events.len(), 1);
        for event in events {
            reduce(&mut state, AppEvent::Controller(event));
        }
        assert_eq!(
            state.group_operation.unwrap().status,
            GroupOperationStatus::Failed(GroupFailure::Persistence)
        );
        assert!(state.saved_groups.is_none());
        assert!(echo.group_events(&snapshot, false).is_empty());
    }
    #[test]
    fn saved_group_applied_revision_updates_clean_draft_and_preserves_dirty_draft() {
        use crate::app::{reduce, AppEvent, AppState, UiSnapshot};
        let first = DeviceId([1; 6]);
        let second = DeviceId([2; 6]);
        let draft = DeviceId([3; 6]);
        let snapshot = DeviceSnapshot {
            desired_revision: 8,
            desired_members: BTreeSet::from([ReceiverId::from(second.clone())]),
            ..Default::default()
        };
        for dirty in [false, true] {
            let mut state = AppState {
                desired_revision: 7,
                desired_receivers: [first.clone()].into_iter().collect(),
                staged_receivers: [if dirty { draft.clone() } else { first.clone() }]
                    .into_iter()
                    .collect(),
                staged_base_revision: 7,
                ..Default::default()
            };
            let (_, events) = Projection::default().project(&snapshot, GenerationsSeen::default());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(state.desired_revision, 8);
            assert!(state.desired_receivers.contains(&second));
            assert!(state
                .staged_receivers
                .contains(if dirty { &draft } else { &second }));
            assert_eq!(
                UiSnapshot::from_state(&state).staged_membership_stale,
                dirty
            );
        }
    }
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use airplay_core::DeviceId;
    use homepod_cast::backend::{BackendCommand, CommandEnvelope, DeviceBackendHandle};
    use homepod_cast::{ReceiverId, RunIntent, Volume};
    use tokio::sync::mpsc;

    use super::{
        BackendPort, EndpointDirectory, GenerationEcho, GenerationsSeen, Projection, SessionEdge,
    };
    use crate::app::{
        ControllerEvent, ControllerPort, ControllerSendError, DeviceCommand, GenerationId,
    };
    use homepod_cast::DeviceSnapshot;

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    #[test]
    fn persisted_master_is_restored_into_the_shell_before_start() {
        use crate::app::{reduce, AppEvent, AppState};
        let backend = DeviceSnapshot {
            master_volume: Volume::new(0.8).unwrap(),
            ..DeviceSnapshot::default()
        };
        let mut shell = AppState::default();
        let (_, events) = Projection::default().project(&backend, GenerationsSeen::default());
        for event in events {
            reduce(&mut shell, AppEvent::Controller(event));
        }
        assert_eq!(
            shell.master_volume, 0.8,
            "relaunch must show the master that GroupStart uses"
        );
        shell.receivers = vec![crate::app::ReceiverState {
            id: rid(1),
            name: "Office".into(),
            model: "HomePod".into(),
            availability: crate::app::Availability::Available,
        }];
        shell.desired_receivers.insert(rid(1));
        shell.staged_receivers.insert(rid(1));
        for group_start in [false, true] {
            let mut shell = shell.clone();
            let (port, mut rx, _) = port(8);
            let event = if group_start {
                AppEvent::GroupRequested(crate::app::GroupCommand::Apply {
                    id: crate::app::GroupId(uuid::Uuid::nil()),
                    start: true,
                })
            } else {
                AppEvent::StartRequested
            };
            for effect in reduce(&mut shell, event).effects {
                if let Some(command) = crate::device_service::effect_to_command(&effect) {
                    port.try_send(command).unwrap();
                }
            }
            let commands = drain(&mut rx);
            if group_start {
                assert!(matches!(
                    commands.as_slice(),
                    [BackendCommand::ActivateSavedGroup { start: true, .. }]
                ));
                assert_eq!(shell.master_volume, backend.master_volume.get());
            } else {
                assert!(commands.iter().any(|command| matches!(command, BackendCommand::SetMasterVolume(value) if value.get() == 0.8)));
            }
        }
    }

    #[test]
    fn master_receipt_projects_rejected_persisted_and_rapid_edits_after_revision_barrier() {
        use crate::app::{reduce, AppEvent, AppState};
        for failure in [
            None,
            Some(homepod_cast::backend::model::CommandFailure::Persistence),
        ] {
            let (port, mut rx, echo) = port(8);
            let mut backend = DeviceSnapshot {
                master_volume: Volume::new(0.8).unwrap(),
                ..DeviceSnapshot::default()
            };
            let mut shell = AppState::default();
            let (_, events) = Projection::default().project(&backend, echo.sample());
            for event in events {
                reduce(&mut shell, AppEvent::Controller(event));
            }
            for volume in [0.4, 0.6] {
                for effect in reduce(&mut shell, AppEvent::MasterVolumeChanged(volume)).effects {
                    port.try_send(crate::device_service::effect_to_command(&effect).unwrap())
                        .unwrap();
                }
            }
            let old = rx.try_recv().unwrap();
            let latest = rx.try_recv().unwrap();
            drop(old);
            assert!(echo.volume_events(&backend, false).is_empty());
            assert_eq!(shell.master_volume, 0.6);
            latest
                .confirmation
                .unwrap()
                .send(homepod_cast::backend::command::CommandConfirmation {
                    revision: 5,
                    error: failure,
                    session_generation_floor: None,
                })
                .unwrap();
            assert!(echo.volume_events(&backend, false).is_empty());
            backend.revision = 5;
            if failure.is_none() {
                backend.master_volume = Volume::new(0.6).unwrap();
            }
            for event in echo.volume_events(&backend, false) {
                reduce(&mut shell, AppEvent::Controller(event));
            }
            assert_eq!(
                shell.master_volume,
                if failure.is_some() { 0.8 } else { 0.6 }
            );
            assert!(!shell.volume_pending);
        }
        let (port, mut rx, echo) = port(1);
        port.try_send(DeviceCommand::ApplyMute(true)).unwrap();
        let mut shell = AppState::default();
        let effect = reduce(&mut shell, AppEvent::MasterVolumeChanged(0.6))
            .effects
            .remove(0);
        assert_eq!(
            port.try_send(crate::device_service::effect_to_command(&effect).unwrap()),
            Err(ControllerSendError::Busy)
        );
        let backend = DeviceSnapshot {
            master_volume: Volume::new(0.8).unwrap(),
            ..DeviceSnapshot::default()
        };
        for event in echo.volume_events(&backend, false) {
            reduce(&mut shell, AppEvent::Controller(event));
        }
        assert_eq!(shell.master_volume, 0.8);
        assert!(!shell.volume_pending);
        assert!(matches!(
            rx.try_recv().unwrap().command,
            BackendCommand::SetMuted(true)
        ));
    }

    /// Builds a port over a real bounded queue: no thread, no runtime, no
    /// hardware, and the real `Busy`/`Closed` behaviour of the shipping
    /// handle rather than a hand-written stand-in.
    fn port(
        capacity: usize,
    ) -> (
        BackendPort,
        mpsc::Receiver<CommandEnvelope>,
        Arc<GenerationEcho>,
    ) {
        let (tx, rx) = mpsc::channel(capacity);
        let echo = Arc::new(GenerationEcho::new());
        let port = BackendPort::new(
            DeviceBackendHandle::new(tx),
            Arc::clone(&echo),
            Arc::new(EndpointDirectory::default()),
        );
        (port, rx, echo)
    }

    fn drain(rx: &mut mpsc::Receiver<CommandEnvelope>) -> Vec<BackendCommand> {
        let mut sent = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            sent.push(envelope.command);
        }
        sent
    }

    #[test]
    fn listening_tone_is_bound_to_backend_generation_and_reports_lost_receipt() {
        let (port, mut rx, echo) = port(8);
        let id = ReceiverId::from(DeviceId([1; 6]));
        let mut snapshot = DeviceSnapshot::default();
        snapshot.run_intent = RunIntent::Running;
        snapshot.master_volume = Volume::new(0.1).unwrap();
        snapshot.desired_members.insert(id);
        snapshot.session.active.insert(id);
        snapshot.session.phase =
            homepod_cast::backend::model::SessionPhase::Streaming { generation: 19 };
        snapshot.audio_source.state = homepod_cast::backend::model::AudioSourceState::SilentSystem;
        let generation = GenerationId(3);
        echo.record_session(generation, SessionEdge::Start);
        echo.observe_listening_target(
            &snapshot,
            &[ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![DeviceId([1; 6])],
            }],
        );
        port.try_send(DeviceCommand::PlayListeningTone { generation })
            .unwrap();
        let command = rx.try_recv().unwrap();
        match &command.command {
            BackendCommand::RunListeningTest(guard) => {
                assert_eq!(
                    guard.generation(),
                    19,
                    "shell and backend generations are distinct"
                );
                assert!(guard.matches(&snapshot));
                snapshot.muted = true;
                assert!(!guard.matches(&snapshot));
            }
            other => panic!("expected bound tone command, got {other:?}"),
        }
        assert!(echo.listening_events(&snapshot, false).is_empty());
        drop(command);
        assert!(matches!(
            echo.listening_events(&snapshot, false).as_slice(),
            [ControllerEvent::SessionFailed {
                generation: GenerationId(3),
                ..
            }]
        ));
        assert!(
            echo.listening_events(&snapshot, false).is_empty(),
            "failure delivered once"
        );
        port.try_send(DeviceCommand::PlayListeningTone {
            generation: GenerationId(2),
        })
        .unwrap();
        assert!(
            rx.try_recv().is_err(),
            "stale shell request cannot start audio"
        );
        assert!(matches!(
            echo.listening_events(&snapshot, false).as_slice(),
            [ControllerEvent::SessionFailed {
                generation: GenerationId(2),
                ..
            }]
        ));
    }

    fn members(command: &BackendCommand) -> &BTreeSet<ReceiverId> {
        match command {
            BackendCommand::SetDesiredMembers { members } => members,
            other => panic!("expected a membership replacement, got {other:?}"),
        }
    }

    fn master_volume(command: &BackendCommand) -> Volume {
        match command {
            BackendCommand::SetMasterVolume(volume) => *volume,
            other => panic!("expected a master volume, got {other:?}"),
        }
    }

    #[test]
    fn receiver_level_command_targets_one_receiver_without_session_edge() {
        let (port, mut rx, echo) = port(4);
        let before = echo.sample();
        let effect = crate::app::AppEffect::ApplyReceiverLevel {
            receiver: DeviceId([2; 6]),
            level: 0.5,
        };
        port.try_send(crate::device_service::effect_to_command(&effect).unwrap())
            .unwrap();
        let commands = drain(&mut rx);
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            BackendCommand::SetReceiverLevel { receiver, level } => {
                assert_eq!(*receiver, ReceiverId::from(DeviceId([2; 6])));
                assert_eq!(level.get(), 0.5);
            }
            other => panic!("wrong receiver command: {other:?}"),
        }
        assert_eq!(echo.sample(), before);
    }

    #[test]
    fn receiver_level_projection_restores_durable_levels_and_tracks_changes() {
        let mut snapshot = DeviceSnapshot::default();
        snapshot.receiver_levels.insert(
            ReceiverId::from(DeviceId([2; 6])),
            Volume::new(0.3).unwrap(),
        );
        let (projection, events) =
            Projection::default().project(&snapshot, GenerationsSeen::default());
        let expected = std::collections::BTreeMap::from([(DeviceId([2; 6]), 0.3)]);
        assert!(events.contains(&ControllerEvent::ReceiverLevelsChanged { levels: expected }));
        let (_, unchanged) = projection.project(&snapshot, GenerationsSeen::default());
        assert!(!unchanged
            .iter()
            .any(|e| matches!(e, ControllerEvent::ReceiverLevelsChanged { .. })));
        snapshot.receiver_levels.clear();
        let (_, changed) = projection.project(&snapshot, GenerationsSeen::default());
        assert!(changed.contains(&ControllerEvent::ReceiverLevelsChanged {
            levels: Default::default()
        }));
    }

    #[test]
    fn receiver_level_backpressure_does_not_change_session_generation() {
        let (port, mut rx, echo) = port(1);
        let before = echo.sample();
        let command = DeviceCommand::ApplyReceiverLevel {
            receiver: rid(1),
            level: 0.2,
        };
        port.try_send(command.clone()).unwrap();
        assert_eq!(
            port.try_send(command.clone()),
            Err(ControllerSendError::Busy)
        );
        assert_eq!(drain(&mut rx).len(), 1);
        assert_eq!(echo.sample(), before);
        drop(rx);
        assert_eq!(port.try_send(command), Err(ControllerSendError::Closed));
        assert_eq!(echo.sample(), before);
    }

    fn run_intent(command: &BackendCommand) -> RunIntent {
        match command {
            BackendCommand::SetRunIntent(intent) => *intent,
            other => panic!("expected a run intent, got {other:?}"),
        }
    }

    /// A port whose endpoint directory the test can fill, and the directory.
    fn port_with_directory(
        capacity: usize,
    ) -> (
        BackendPort,
        mpsc::Receiver<CommandEnvelope>,
        Arc<EndpointDirectory>,
    ) {
        let (tx, rx) = mpsc::channel(capacity);
        let endpoints = Arc::new(EndpointDirectory::default());
        let port = BackendPort::new(
            DeviceBackendHandle::new(tx),
            Arc::new(GenerationEcho::new()),
            Arc::clone(&endpoints),
        );
        (port, rx, endpoints)
    }

    mod translation {
        use super::*;

        /// The audio half of the seam, in both directions at once.
        ///
        /// The command tests here do not invent an endpoint key. They fill the
        /// directory the way the running bridge fills it -- by projecting a
        /// backend snapshot and publishing the table that pass produced -- so
        /// what is asserted is that the key the *window* was handed reaches
        /// the *backend* as the endpoint it stands for. A key made up in a
        /// test would prove only that the map works, not that the two halves
        /// agree about it.
        mod audio {
            use homepod_cast::backend::model::{
                AudioEndpoint, AudioEndpointPreference, AudioSourceSnapshot,
            };
            use homepod_cast::{DeviceSnapshot, LatencyPreset};

            use super::super::super::{GenerationsSeen, Projection};
            use super::*;
            use crate::app::{
                AudioEndpointKey, AudioEndpointRequest, ControllerEvent, LatencyChoice,
            };

            const HEADSET: &str = "{0.0.0.00000000}.{aaaa}";
            const SPEAKERS: &str = "{0.0.0.00000000}.{bbbb}";

            fn endpoint(id: &str, name: &str) -> AudioEndpoint {
                AudioEndpoint {
                    id: id.to_owned(),
                    name: name.to_owned(),
                }
            }

            /// Publishes the key table one projection pass produces, and
            /// hands back the keys the shell would have been given.
            fn publish_two_endpoints(
                directory: &EndpointDirectory,
            ) -> (Projection, Vec<AudioEndpointKey>) {
                let snapshot = DeviceSnapshot {
                    audio_source: AudioSourceSnapshot {
                        refresh_failed: false,
                        active_endpoints: Some(vec![
                            endpoint(HEADSET, "Arctis 7 Chat"),
                            endpoint(SPEAKERS, "Realtek HDMI"),
                        ]),
                        preference: AudioEndpointPreference::SystemDefault,
                        captured_endpoint: None,
                        state: homepod_cast::AudioSourceState::Capturing,
                        ..Default::default()
                    },
                    ..DeviceSnapshot::default()
                };
                let (projection, events) =
                    Projection::default().project(&snapshot, GenerationsSeen::default());
                directory.publish(&projection.endpoints);
                let keys = events
                    .into_iter()
                    .find_map(|event| match event {
                        ControllerEvent::AudioSourceChanged { reading } => Some(
                            reading
                                .endpoints
                                .into_iter()
                                .map(|choice| choice.key)
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .expect("the pass publishes a capture reading");
                (projection, keys)
            }

            #[test]
            fn mute_reaches_the_backend_as_one_set_muted_command() {
                let (port, mut rx, _directory) = port_with_directory(8);

                port.try_send(DeviceCommand::ApplyMute(true)).unwrap();

                match drain(&mut rx).as_slice() {
                    [BackendCommand::SetMuted(true)] => {}
                    other => panic!("expected one mute command, got {other:?}"),
                }
            }

            #[test]
            fn a_latency_choice_reaches_the_backend_as_its_own_preset() {
                for (choice, preset) in [
                    (LatencyChoice::Low, LatencyPreset::Low),
                    (LatencyChoice::Normal, LatencyPreset::Normal),
                    (LatencyChoice::Stable, LatencyPreset::Stable),
                ] {
                    let (port, mut rx, _directory) = port_with_directory(8);

                    port.try_send(DeviceCommand::ApplyLatency(choice)).unwrap();

                    match drain(&mut rx).as_slice() {
                        [BackendCommand::SetLatencyPreset(sent)] => assert_eq!(*sent, preset),
                        other => panic!("expected one latency command, got {other:?}"),
                    }
                }
            }

            #[test]
            fn the_windows_default_capture_source_needs_no_key_at_all() {
                let (port, mut rx, _directory) = port_with_directory(8);

                port.try_send(DeviceCommand::ApplyAudioEndpoint(
                    AudioEndpointRequest::SystemDefault,
                ))
                .unwrap();

                match drain(&mut rx).as_slice() {
                    [BackendCommand::SetAudioEndpoint(AudioEndpointPreference::SystemDefault)] => {}
                    other => panic!("expected the system default, got {other:?}"),
                }
            }

            /// The claim the opaque key exists to make: the key the window was
            /// handed reaches the backend as exactly the endpoint it stands
            /// for, and never as the other one.
            #[test]
            fn a_published_key_reaches_the_backend_as_its_own_endpoint() {
                let (port, mut rx, directory) = port_with_directory(8);
                let (_projection, keys) = publish_two_endpoints(&directory);

                for (key, id, name) in [
                    (keys[0], HEADSET, "Arctis 7 Chat"),
                    (keys[1], SPEAKERS, "Realtek HDMI"),
                ] {
                    port.try_send(DeviceCommand::ApplyAudioEndpoint(
                        AudioEndpointRequest::Endpoint(key),
                    ))
                    .unwrap();

                    match drain(&mut rx).as_slice() {
                        [BackendCommand::SetAudioEndpoint(AudioEndpointPreference::Explicit {
                            id: sent,
                            last_known_name,
                        })] => {
                            assert_eq!(sent, id);
                            assert_eq!(last_known_name, name);
                        }
                        other => panic!("expected {id} to be selected, got {other:?}"),
                    }
                }
            }

            /// An endpoint that has gone away keeps its key, so a click that
            /// raced a device change still means the device the user pointed
            /// at -- and never a different one.
            #[test]
            fn a_key_survives_its_endpoint_disappearing_from_the_offer() {
                let (port, mut rx, directory) = port_with_directory(8);
                let (published, keys) = publish_two_endpoints(&directory);

                // A later pass in which the headset is gone. Carried forward
                // from the pass that published the keys, exactly as the bridge
                // thread carries one projection for its whole life -- which is
                // what makes the guarantee below true at all.
                let snapshot = DeviceSnapshot {
                    audio_source: AudioSourceSnapshot {
                        refresh_failed: false,
                        active_endpoints: Some(vec![endpoint(SPEAKERS, "Realtek HDMI")]),
                        ..AudioSourceSnapshot::default()
                    },
                    ..DeviceSnapshot::default()
                };
                let (projection, _) = published.project(&snapshot, GenerationsSeen::default());
                directory.publish(&projection.endpoints);

                port.try_send(DeviceCommand::ApplyAudioEndpoint(
                    AudioEndpointRequest::Endpoint(keys[0]),
                ))
                .unwrap();

                match drain(&mut rx).as_slice() {
                    [BackendCommand::SetAudioEndpoint(AudioEndpointPreference::Explicit {
                        id,
                        ..
                    })] => assert_eq!(
                        id, HEADSET,
                        "the key must still mean the endpoint it was published for"
                    ),
                    other => panic!("expected the headset, got {other:?}"),
                }
            }

            /// A key this process never handed out selects nothing at all.
            ///
            /// Not a fallback and not a guess: choosing *some* endpoint here
            /// would mean silently capturing a device the user never named,
            /// which is the one outcome the opaque key exists to prevent.
            #[test]
            fn a_key_the_bridge_never_published_selects_nothing() {
                let (port, mut rx, directory) = port_with_directory(8);
                let _ = publish_two_endpoints(&directory);

                assert_eq!(
                    port.try_send(DeviceCommand::ApplyAudioEndpoint(
                        AudioEndpointRequest::Endpoint(AudioEndpointKey(u64::MAX)),
                    )),
                    Ok(())
                );

                assert!(
                    drain(&mut rx).is_empty(),
                    "an unknown key must not select any endpoint"
                );
            }
        }

        /// Discovery stopped being a request. The backend browses as a
        /// supervised daemon, so the shell's "Refresh" only records the
        /// generation the bridge must answer the current inventory under.
        #[test]
        fn discover_records_the_generation_and_sends_no_backend_command() {
            let (port, mut rx, echo) = port(8);

            assert_eq!(
                port.try_send(DeviceCommand::Discover {
                    generation: GenerationId(7),
                }),
                Ok(())
            );

            assert!(
                drain(&mut rx).is_empty(),
                "discovery is a backend daemon, not a command"
            );
            assert_eq!(echo.discovery(), GenerationId(7));
            assert_eq!(
                echo.session(),
                GenerationId(0),
                "a refresh is not a session edge"
            );
        }

        /// The newest refresh wins, so a burst of clicks costs one answer.
        #[test]
        fn a_second_refresh_before_the_bridge_wakes_keeps_only_the_newest_generation() {
            let (port, _rx, echo) = port(8);

            port.try_send(DeviceCommand::Discover {
                generation: GenerationId(4),
            })
            .unwrap();
            port.try_send(DeviceCommand::Discover {
                generation: GenerationId(5),
            })
            .unwrap();

            assert_eq!(echo.discovery(), GenerationId(5));
        }

        #[test]
        fn start_session_sets_members_then_volume_then_run_intent() {
            let (port, mut rx, echo) = port(8);

            port.try_send(DeviceCommand::StartSession {
                generation: GenerationId(3),
                receiver_ids: vec![rid(2), rid(1)],
                volume: 0.5,
            })
            .unwrap();

            let sent = drain(&mut rx);
            assert_eq!(sent.len(), 3, "one start is three backend settings");
            assert_eq!(
                members(&sent[0]),
                &BTreeSet::from([ReceiverId::from(rid(1)), ReceiverId::from(rid(2))])
            );
            assert_eq!(master_volume(&sent[1]).get(), 0.5);
            assert_eq!(run_intent(&sent[2]), RunIntent::Running);
            assert_eq!(echo.session(), GenerationId(3));
            assert_eq!(
                echo.discovery(),
                GenerationId(0),
                "starting a session is not a discovery refresh"
            );
        }

        /// A refusal must leave membership and volume unchanged as well as
        /// run intent, even when two of the three settings would fit.
        #[test]
        fn start_session_never_reaches_the_backend_as_a_run_intent_when_the_queue_fills() {
            let (port, mut rx, _echo) = port(2);

            assert_eq!(
                port.try_send(DeviceCommand::StartSession {
                    generation: GenerationId(3),
                    receiver_ids: vec![rid(1)],
                    volume: 0.5,
                }),
                Err(ControllerSendError::Busy)
            );

            let sent = drain(&mut rx);
            assert!(sent.is_empty(), "a refused start admits no settings");
            assert!(
                !sent
                    .iter()
                    .any(|command| matches!(command, BackendCommand::SetRunIntent(_))),
                "a partially accepted start must never carry a run intent"
            );
        }

        #[test]
        fn start_session_stops_at_the_first_refusal() {
            let (port, mut rx, _echo) = port(1);

            assert_eq!(
                port.try_send(DeviceCommand::StartSession {
                    generation: GenerationId(3),
                    receiver_ids: vec![rid(1)],
                    volume: 0.5,
                }),
                Err(ControllerSendError::Busy)
            );

            let sent = drain(&mut rx);
            assert!(
                sent.is_empty(),
                "even membership needs the whole reservation"
            );
        }

        /// The echo records what the shell *asked* for, not what the queue
        /// happened to accept. A refused start still leaves the window in
        /// `Starting` under the new generation, and the bridge has to be able
        /// to answer that generation with whatever the backend really is --
        /// otherwise the reducer's guard drops every later event and the
        /// window says "starting" forever.
        #[test]
        fn a_refused_start_still_lets_the_bridge_answer_under_the_new_generation() {
            let (port, _rx, echo) = port(2);

            assert_eq!(
                port.try_send(DeviceCommand::StartSession {
                    generation: GenerationId(7),
                    receiver_ids: vec![rid(1)],
                    volume: 0.5,
                }),
                Err(ControllerSendError::Busy)
            );

            assert_eq!(echo.session(), GenerationId(7));
        }

        /// A session generation is not a self-describing request the way a
        /// discovery generation is: "answer generation 4" means the opposite
        /// thing after Start than after Stop, and the bridge has to be able
        /// to tell them apart to avoid answering a press with its opposite.
        #[test]
        fn a_start_and_a_stop_record_which_edge_the_shell_asked_for() {
            let (port, _rx, echo) = port(8);
            assert_eq!(echo.session_edge(), SessionEdge::Untouched);

            port.try_send(DeviceCommand::StartSession {
                generation: GenerationId(3),
                receiver_ids: vec![rid(1)],
                volume: 0.5,
            })
            .unwrap();
            assert_eq!(echo.session_edge(), SessionEdge::Start);

            port.try_send(DeviceCommand::StopSession {
                generation: GenerationId(4),
            })
            .unwrap();
            assert_eq!(echo.session_edge(), SessionEdge::Stop);

            port.try_send(DeviceCommand::ApplyVolume {
                generation: GenerationId(5),
                volume: 0.5,
            })
            .unwrap();
            assert_eq!(
                echo.session_edge(),
                SessionEdge::Stop,
                "volume is not a session edge"
            );
        }

        #[test]
        fn stop_session_sends_exactly_one_stopped_run_intent() {
            let (port, mut rx, echo) = port(8);

            port.try_send(DeviceCommand::StopSession {
                generation: GenerationId(9),
            })
            .unwrap();

            let sent = drain(&mut rx);
            assert_eq!(sent.len(), 1);
            assert_eq!(run_intent(&sent[0]), RunIntent::Stopped);
            assert_eq!(echo.session(), GenerationId(9));
        }

        /// Volume is coalesced backend state, not a lifecycle edge: it must
        /// not disturb the session generation the bridge stamps its session
        /// events with.
        #[test]
        fn apply_volume_sends_one_master_volume_and_touches_no_generation() {
            let (port, mut rx, echo) = port(8);

            port.try_send(DeviceCommand::ApplyVolume {
                generation: GenerationId(11),
                volume: 0.75,
            })
            .unwrap();

            let sent = drain(&mut rx);
            assert_eq!(sent.len(), 1);
            assert_eq!(master_volume(&sent[0]).get(), 0.75);
            assert_eq!(echo.session(), GenerationId(0));
            assert_eq!(echo.discovery(), GenerationId(0));
        }

        #[test]
        fn shutdown_forwards_exactly_one_backend_shutdown() {
            let (port, mut rx, _echo) = port(8);

            port.try_send(DeviceCommand::Shutdown).unwrap();

            let sent = drain(&mut rx);
            assert_eq!(sent.len(), 1);
            assert!(matches!(sent[0], BackendCommand::Shutdown));
        }

        /// The reducer already clamps and rejects non-finite input, so this is
        /// defence in depth. It must be total: dropping the command instead
        /// would leave the backend at a volume the user did not choose, with
        /// nothing on screen saying so.
        #[test]
        fn volume_is_clamped_and_a_non_finite_value_falls_back_to_the_default() {
            let (port, mut rx, _echo) = port(8);

            for value in [1.5_f32, -0.5, f32::NAN] {
                port.try_send(DeviceCommand::ApplyVolume {
                    generation: GenerationId(1),
                    volume: value,
                })
                .unwrap();
            }

            let sent = drain(&mut rx);
            assert_eq!(sent.len(), 3);
            assert_eq!(master_volume(&sent[0]).get(), 1.0);
            assert_eq!(master_volume(&sent[1]).get(), 0.0);
            assert_eq!(
                master_volume(&sent[2]).get(),
                Volume::DEFAULT_MASTER.get(),
                "a non-finite volume falls back rather than vanishing"
            );
        }
    }

    mod failure_reporting {
        use super::*;

        /// `Closed` is what makes the shell say the audio controller stopped;
        /// `Busy` only logs. Confusing the two either hides a dead backend or
        /// puts a permanent error on screen for a full queue.
        #[test]
        fn a_full_queue_is_busy_and_a_dropped_backend_is_closed() {
            let (busy_port, _busy_rx, _busy_echo) = port(1);
            busy_port.try_send(DeviceCommand::Shutdown).unwrap();
            assert_eq!(
                busy_port.try_send(DeviceCommand::Shutdown),
                Err(ControllerSendError::Busy)
            );

            let (closed_port, closed_rx, _closed_echo) = port(4);
            drop(closed_rx);
            for command in [
                DeviceCommand::Discover {
                    generation: GenerationId(1),
                },
                DeviceCommand::StartSession {
                    generation: GenerationId(1),
                    receiver_ids: vec![rid(1)],
                    volume: 0.5,
                },
                DeviceCommand::StopSession {
                    generation: GenerationId(1),
                },
                DeviceCommand::ApplyVolume {
                    generation: GenerationId(1),
                    volume: 0.5,
                },
                DeviceCommand::Shutdown,
            ] {
                assert_eq!(
                    closed_port.try_send(command.clone()),
                    Err(ControllerSendError::Closed),
                    "a dead backend must be reported for {command:?}"
                );
            }
        }

        /// Discovery sends nothing, so it would report success against a dead
        /// backend and leave the window spinning forever. The port checks
        /// liveness instead of assuming it.
        #[test]
        fn a_refresh_against_a_dead_backend_records_no_work_for_the_bridge() {
            let (port, rx, echo) = port(4);
            drop(rx);

            assert_eq!(
                port.try_send(DeviceCommand::Discover {
                    generation: GenerationId(2),
                }),
                Err(ControllerSendError::Closed)
            );
            assert_eq!(
                echo.discovery(),
                GenerationId(0),
                "a refusal must not leave work queued for the bridge"
            );
        }
    }

    mod wakeup {
        use super::*;

        /// The bridge waits on the echo instead of polling it. Without a
        /// wakeup a refresh would only be answered once the backend happened
        /// to publish a new snapshot.
        #[tokio::test]
        async fn waiting_on_the_echo_wakes_on_the_next_shell_command() {
            let (port, _rx, echo) = port(8);

            let waiter = tokio::spawn({
                let echo = Arc::clone(&echo);
                async move { echo.wait().await }
            });
            tokio::task::yield_now().await;
            assert!(
                !waiter.is_finished(),
                "an idle echo must not wake the bridge"
            );

            port.try_send(DeviceCommand::Discover {
                generation: GenerationId(1),
            })
            .unwrap();

            // Bounded on purpose: on a current-thread runtime one yield is
            // enough for a woken task to finish, so a lost wakeup fails here
            // instead of hanging the suite on an `await` that never returns.
            tokio::task::yield_now().await;
            assert!(
                waiter.is_finished(),
                "a raised refresh must wake the parked bridge"
            );
            waiter.await.expect("the woken waiter completes");
        }
    }

    mod boundary {
        /// Structural guard over the crate boundary.
        ///
        /// The shell's binary module tree and the backend library are two
        /// disjoint trees over one directory; today nothing in the shell can
        /// name a backend type. This file is the one exception, which is what
        /// makes "no backend material reaches the UI" checkable by reading
        /// one file instead of auditing the whole shell.
        #[test]
        fn the_shell_reaches_the_backend_library_only_through_this_bridge() {
            let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
            // The exemption is this file, not its name. A second
            // `backend_bridge.rs` anywhere else under `src/` would otherwise
            // inherit the licence to name the backend library, which is the
            // one licence this crate hands out exactly once.
            let bridge = src.join("backend_bridge.rs");
            assert!(
                bridge.is_file(),
                "the exempt file is not where this guard looks for it: {}",
                bridge.display()
            );
            let mut scanned = 0_usize;
            let mut offenders = Vec::new();
            visit(&src, &mut |path: &std::path::Path| {
                scanned += 1;
                if path == bridge {
                    return;
                }
                let source = std::fs::read_to_string(path).expect("source file is readable");
                if source.contains("homepod_cast::") {
                    offenders.push(path.display().to_string());
                }
            });

            assert!(
                scanned > 20,
                "the scan found {scanned} files and would pass vacuously"
            );
            assert!(
                offenders.is_empty(),
                "only the bridge may name the backend library; found: {offenders:?}"
            );
        }

        fn visit(dir: &std::path::Path, seen: &mut impl FnMut(&std::path::Path)) {
            for entry in std::fs::read_dir(dir).expect("source directory is readable") {
                let path = entry.expect("directory entry is readable").path();
                if path.is_dir() {
                    visit(&path, seen);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    seen(&path);
                }
            }
        }
    }

    /// The second, later, and far more dangerous projection: the diagnostics
    /// registry's own snapshot.
    ///
    /// Everything the device projection guards against is here too, and two
    /// things besides: an absolute wall-clock stamp and a monotonic process
    /// age, neither of which the shell has any business holding.
    mod diagnostics {
        use std::collections::BTreeMap;
        use std::time::{Duration, SystemTime};

        use airplay_client::FeedbackSnapshot;
        use homepod_cast::{
            DiagnosticComponent, DiagnosticError, DiagnosticErrorCode, DiagnosticSeverity,
            DiagnosticsSnapshot, Health, ReceiverDiagnosticsSnapshot, ReceiverId,
            ReceiverLifecycle, ReceiverSessionKey, ReceiverTimingSnapshot, ReceiverTimingSource,
            ReceiverTransportSnapshot, Recoverability, SessionDiagnosticsSnapshot, SessionId,
            SessionStopReason, UserFacingError, DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
        };

        use super::super::diagnostics_reading;
        use super::rid;
        use crate::app::{DiagnosticsHealth, DiagnosticsReading};

        /// The free-form backend text every row in these tests carries.
        ///
        /// The registry's error record holds two unbounded strings of its
        /// own. A projection that forwarded a row instead of reading its
        /// fields would put one of these into shell state, one step from a
        /// renderer.
        const POISON: &str = concat!(
            "connect failed: os error 10061 (0x80070005) to 192.168.178.44:7000 ",
            r"while writing C:\Users\Thorsten\AppData\Roaming\OpenAirCast"
        );

        fn empty(health: Health, dropped: u64) -> DiagnosticsSnapshot {
            DiagnosticsSnapshot {
                schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
                snapshot_sequence: 9,
                captured_at_utc: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                process_elapsed_ns: 12_345_678,
                active_session_id: None,
                health,
                session: None,
                receivers: Vec::new(),
                diagnostics_events_dropped_total: dropped,
            }
        }

        /// One receiver row with every free-form string it can carry filled in.
        fn poisoned_row(last: u8, session_id: SessionId) -> ReceiverDiagnosticsSnapshot {
            let receiver_id = ReceiverId::from(rid(last));
            ReceiverDiagnosticsSnapshot {
                key: ReceiverSessionKey {
                    session_id,
                    receiver_id,
                },
                lifecycle: ReceiverLifecycle::Failed {
                    retryable: true,
                    error: UserFacingError::new(POISON),
                },
                timing: ReceiverTimingSnapshot {
                    source: ReceiverTimingSource::PtpSharedFromPrimary {
                        source_receiver_id: Some(receiver_id),
                    },
                    independently_measured: false,
                    offset_local_minus_master_ns: Some(-4_200),
                    mean_path_delay_ns: Some(1_100),
                    drift_ppm: Some(0.5),
                    sample_age_ns: Some(900),
                    stale: true,
                },
                transport: ReceiverTransportSnapshot::default(),
                feedback: FeedbackSnapshot::default(),
                last_error: Some(DiagnosticError {
                    code: DiagnosticErrorCode::UdpSendFailure,
                    component: DiagnosticComponent::UdpTransport,
                    severity: DiagnosticSeverity::Error,
                    recoverability: Recoverability::AutomaticRetry,
                    operation: "send_to",
                    receiver_session: Some(ReceiverSessionKey {
                        session_id,
                        receiver_id,
                    }),
                    public_message: POISON.to_owned(),
                    technical_detail: Some(POISON.to_owned()),
                }),
            }
        }

        #[test]
        fn the_reading_carries_health_dropped_events_and_the_measured_receiver_count() {
            let session_id = SessionId::generate();
            let mut snapshot = empty(Health::Attention, 17);
            snapshot.active_session_id = Some(session_id);
            snapshot.receivers = vec![poisoned_row(1, session_id), poisoned_row(2, session_id)];

            assert_eq!(
                diagnostics_reading(&snapshot),
                DiagnosticsReading {
                    health: DiagnosticsHealth::Attention,
                    events_dropped_total: 17,
                    measured_receivers: 2,
                }
            );
        }

        #[test]
        fn retained_receiver_rows_are_not_current_without_an_active_session() {
            let session_id = SessionId::generate();
            let mut active = empty(Health::Running, 0);
            active.active_session_id = Some(session_id);
            active.receivers = vec![poisoned_row(1, session_id), poisoned_row(2, session_id)];

            assert_eq!(diagnostics_reading(&active).measured_receivers, 2);

            let mut finished = active.clone();
            finished.active_session_id = None;
            finished.health = Health::Unknown;

            assert_eq!(finished.receivers.len(), 2);
            assert_eq!(diagnostics_reading(&finished).measured_receivers, 0);
        }

        /// Every health state the registry can publish arrives as its own
        /// shell value, and no two of them collapse onto one.
        #[test]
        fn every_health_state_arrives_as_its_own_shell_value() {
            let states = [
                (Health::Unknown, DiagnosticsHealth::Unknown),
                (Health::Running, DiagnosticsHealth::Running),
                (Health::Attention, DiagnosticsHealth::Attention),
                (Health::Error, DiagnosticsHealth::Error),
            ];
            let mut seen = Vec::new();
            for (backend, shell) in states {
                let reading = diagnostics_reading(&empty(backend, 0));
                assert_eq!(reading.health, shell, "{backend:?}");
                seen.push(reading.health);
            }
            seen.dedup();
            assert_eq!(seen.len(), 4, "two health states collapsed onto one word");
        }

        /// The wall-clock class, refused.
        ///
        /// `captured_at_utc` is an absolute time and `process_elapsed_ns` is
        /// the age of this process; both move on every publication for
        /// reasons that have nothing to do with what the user is looking at.
        /// Two snapshots that differ in nothing else must project to the same
        /// reading, or every assertion downstream of here becomes a clock
        /// assertion.
        #[test]
        fn two_snapshots_differing_only_in_the_clock_project_to_one_reading() {
            let early = empty(Health::Running, 3);
            let mut late = empty(Health::Running, 3);
            late.captured_at_utc = early.captured_at_utc + Duration::from_secs(86_400);
            late.process_elapsed_ns = early.process_elapsed_ns + 999_999_999;

            assert_eq!(diagnostics_reading(&early), diagnostics_reading(&late));
        }

        /// Which class of raw registry material a string carries, if any.
        ///
        /// A near-copy of the shape guard in `crate::ui::acceptance`, and the
        /// duplication is deliberate rather than an oversight. That one lives
        /// on the far side of privacy boundary 1: the window may not import
        /// anything from this file, so a check that has to run in both places
        /// has to exist in both places.
        ///
        /// It has to run *here*, and that is the whole reason this function
        /// exists. The window's copy is applied to a page built from a
        /// [`DiagnosticsReading`] the window's own tests construct by hand, so
        /// a field added to that struct is filled with a placeholder there and
        /// the page then faithfully draws the nothing the test put in. The
        /// window's guard proves the *copy* is clean; only a guard applied to
        /// the output of [`diagnostics_reading`] proves the *projection* is.
        ///
        /// Shapes rather than sentences, because a fixed fragment list only
        /// catches text some test happened to plant, while the registry's
        /// snapshot is full of things with recognisable form: session UUIDs,
        /// MAC-derived receiver identities, socket addresses and filesystem
        /// paths inside two unbounded error strings. Wall-clock values are the
        /// one class with no shape to recognise, and they are pinned by
        /// [`two_snapshots_differing_only_in_the_clock_project_to_one_reading`]
        /// instead.
        fn backend_shape(text: &str) -> Option<&'static str> {
            if text.contains("://") {
                return Some("a URL scheme");
            }
            // A drive letter, a UNC prefix, or a home directory. `Debug`
            // doubles backslashes, which leaves both of the first two intact.
            if text.contains(":\\")
                || text.contains("\\\\")
                || text.contains("/Users/")
                || text.contains("/home/")
            {
                return Some("a filesystem path");
            }
            if looks_like_an_address(text) {
                return Some("an IPv4 address");
            }
            if looks_like_a_uuid(text) {
                return Some("a UUID");
            }
            if looks_like_a_byte_address(text) {
                return Some("a hardware address");
            }
            for token in text.split_whitespace() {
                if let Some(shape) = colon_shape(token) {
                    return Some(shape);
                }
            }
            None
        }

        /// What a single whitespace-free token's colons make it look like:
        /// a hardware address (and therefore a `ReceiverId`), an IPv6
        /// address, or a socket.
        ///
        /// Deliberately tolerant of `field: value`, which is how `Debug`
        /// renders every struct, so the guard cannot fire on its own input
        /// shape and get deleted for it.
        fn colon_shape(token: &str) -> Option<&'static str> {
            let trimmed = token.trim_matches(|c: char| {
                matches!(c, ',' | '.' | ';' | '"' | '(' | ')' | '{' | '}' | '[' | ']')
            });
            if !trimmed.contains(':') {
                return None;
            }
            let parts: Vec<&str> = trimmed.split(':').collect();
            let hex = |part: &&str| part.len() <= 4 && part.chars().all(|c| c.is_ascii_hexdigit());
            if parts.len() == 6 && parts.iter().all(|part| part.len() == 2 && hex(part)) {
                return Some("a hardware address");
            }
            if parts.len() >= 3 && parts.iter().all(|part| part.is_empty() || hex(part)) {
                return Some("an IPv6 address");
            }
            if parts.len() == 2
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && parts
                    .iter()
                    .all(|part| part.chars().all(|c| c.is_ascii_digit()))
            {
                return Some("a socket");
            }
            None
        }

        /// Four dotted octets anywhere in the text, however it is punctuated
        /// around them.
        fn looks_like_an_address(text: &str) -> bool {
            text.split(|c: char| !c.is_ascii_digit() && c != '.')
                .any(|candidate| {
                    let parts: Vec<&str> = candidate.split('.').collect();
                    parts.len() == 4
                        && parts
                            .iter()
                            .all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
                })
        }

        /// The 8-4-4-4-12 hexadecimal shape of a `SessionId`, anywhere in the
        /// text.
        ///
        /// The one shape the acceptance guard's fragment list cannot catch by
        /// content: a session UUID is different on every run, so nothing can
        /// be planted for it and only its form gives it away.
        fn looks_like_a_uuid(text: &str) -> bool {
            text.split(|c: char| !c.is_ascii_hexdigit() && c != '-')
                .any(|candidate| {
                    let groups: Vec<&str> = candidate.split('-').collect();
                    groups.len() == 5
                        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, width)| {
                            group.len() == width && group.chars().all(|c| c.is_ascii_hexdigit())
                        })
                })
        }

        /// A `ReceiverId` rendered as the array it is.
        ///
        /// `ReceiverId` is `[u8; 6]` with a derived `Debug`, so a field that
        /// forwarded one would not print the colon-separated display form at
        /// all -- it would print `[0, 0, 0, 0, 0, 1]`, which every colon rule
        /// above sails straight past. Six bracketed octets is the shape that
        /// catches it, and a shell value that legitimately holds six numbers
        /// in a row is the kind of thing that should have to argue its case
        /// here anyway.
        fn looks_like_a_byte_address(text: &str) -> bool {
            text.split('[').skip(1).any(|tail| {
                let Some(inside) = tail.split(']').next() else {
                    return false;
                };
                let octets: Vec<&str> = inside.split(',').map(str::trim).collect();
                octets.len() == 6 && octets.iter().all(|octet| octet.parse::<u8>().is_ok())
            })
        }

        /// The shape guard has to be able to fail, or it proves nothing.
        ///
        /// Both halves matter. A guard that flags nothing is decoration; a
        /// guard that flags a correct reading gets deleted the first time it
        /// fires, which comes to the same thing one release later.
        ///
        /// The two identity shapes are taken from the real types rather than
        /// written out, because what has to be caught is how `SessionId` and
        /// `ReceiverId` actually render when a future field forwards one --
        /// not how a test author remembers them rendering.
        #[test]
        fn the_projection_shape_guard_flags_registry_material_and_spares_a_clean_reading() {
            let real_session = format!("{:?}", SessionId::generate());
            let real_receiver = format!("{:?}", ReceiverId::from(rid(1)));
            for planted in [
                real_session.as_str(),
                real_receiver.as_str(),
                "00:00:00:00:00:01",
                "fe80::1ff:fe23:4567:890a",
                "7000:1",
                "192.168.178.44:7000",
                "30a6c3dd-ef9e-456e-84c6-2fc015ee9051",
                r"C:\Users\Thorsten\AppData\Roaming\OpenAirCast",
                "rtsp://192.168.178.44",
                POISON,
            ] {
                assert!(
                    backend_shape(planted).is_some(),
                    "the guard let {planted:?} through"
                );
            }

            let clean = format!(
                "{:?}",
                DiagnosticsReading {
                    health: DiagnosticsHealth::Attention,
                    events_dropped_total: u64::MAX,
                    measured_receivers: usize::MAX,
                }
            );
            assert_eq!(
                backend_shape(&clean),
                None,
                "the guard fires on a correct reading: {clean}"
            );
        }

        /// A receiver row is counted, never forwarded.
        ///
        /// The whole shell value is `Debug`-rendered and searched, so this
        /// fails on any field that carried a row's text along -- including a
        /// field added later, which is the case a hand-written list misses.
        #[test]
        fn a_receiver_row_is_counted_and_none_of_its_text_travels_with_it() {
            let session_id = SessionId::generate();
            let mut snapshot = empty(Health::Error, 0);
            snapshot.active_session_id = Some(session_id);
            snapshot.session = Some(SessionDiagnosticsSnapshot {
                session_id,
                started_elapsed_ns: 1,
                finished_elapsed_ns: Some(2),
                stop_reason: Some(SessionStopReason::Failed),
                phase: None,
                primary: Some(ReceiverId::from(rid(1))),
                members: BTreeMap::from([(ReceiverId::from(rid(1)), rid(1))]),
                audio: None,
            });
            snapshot.receivers = vec![poisoned_row(1, session_id)];

            let reading = diagnostics_reading(&snapshot);
            let rendered = format!("{reading:?}");

            assert_eq!(reading.measured_receivers, 1);
            for fragment in ["os error", "0x", "192.168.", ":7000", r"C:\", "AppData"] {
                assert!(
                    !rendered.contains(fragment),
                    "backend text reached the shell reading ({fragment:?}): {rendered}"
                );
            }
            assert!(
                !rendered.contains("00:00:00:00:00:01"),
                "a hardware identifier reached the shell reading: {rendered}"
            );
            // The fragment list above only knows the text this test planted.
            // The shape guard is what covers the material nobody can plant --
            // the session UUID, which differs on every run -- and, more to the
            // point, a field added to `DiagnosticsReading` later that carries
            // any of these forms out of the registry.
            assert_eq!(
                backend_shape(&rendered),
                None,
                "the shell reading carries {} : {rendered}",
                backend_shape(&rendered).unwrap_or("nothing")
            );
        }
    }

    mod projection {
        use std::collections::BTreeSet;
        use std::time::{Duration, SystemTime};

        use homepod_cast::backend::model as backend_model;
        use homepod_cast::{
            AudioEndpointPreference, AudioSourceSnapshot, DeviceSnapshot, DiscoveryPhase,
            LatencyPreset, ReceiverId, ReceiverLifecycle, ReceiverRole, RestartReason,
            SessionPhase, SetupPhase, UserFacingError,
        };

        use super::super::{GenerationsSeen, Projection, SessionEdge};
        use super::rid;
        use crate::app::{
            AudioEndpointSelection, Availability, CaptureState, ControllerEvent, GenerationId,
            LatencyChoice, LatencyOption, LatencyUnavailable, ReceiverState,
        };

        #[test]
        fn busy_start_restores_the_authoritative_phase_and_allows_retry() {
            busy_lifecycle_restores_the_authoritative_phase_and_allows_retry(false);
        }

        #[test]
        fn busy_stop_restores_the_authoritative_phase_and_allows_retry() {
            busy_lifecycle_restores_the_authoritative_phase_and_allows_retry(true);
        }

        fn busy_lifecycle_restores_the_authoritative_phase_and_allows_retry(stopping: bool) {
            use crate::app::{reduce, AppEvent, AppState, ControllerPort, StreamState};
            let (port, mut queue, echo) = super::port(3);
            for _ in 0..3 {
                port.try_send(crate::app::DeviceCommand::ApplyMute(false))
                    .unwrap();
            }
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![known(1, "Kitchen", ReceiverLifecycle::Discovered)];
            snapshot.desired_members = BTreeSet::from([ReceiverId::from(rid(1))]);
            if stopping {
                snapshot.session.phase = SessionPhase::Streaming { generation: 8 };
                snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            }
            let mut state = AppState::default();
            state.desired_receivers.insert(rid(1));
            let (projection, initial) = Projection::default().project(&snapshot, echo.sample());
            for event in initial {
                reduce(&mut state, AppEvent::Controller(event));
            }
            let request = if stopping {
                AppEvent::StopRequested
            } else {
                AppEvent::StartRequested
            };
            let transition = reduce(&mut state, request.clone());
            assert_eq!(transition.effects.len(), 1);
            assert_eq!(
                port.try_send(
                    crate::device_service::effect_to_command(&transition.effects[0]).unwrap()
                ),
                Err(crate::app::ControllerSendError::Busy)
            );
            let (projection, repaired) = projection.project(&snapshot, echo.sample());
            for event in repaired {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                if stopping {
                    StreamState::Streaming {
                        generation: GenerationId(1),
                    }
                } else {
                    StreamState::Stopped
                },
                "rejected stop={stopping} must settle from backend truth"
            );
            assert_eq!(state.active_receivers.contains(&rid(1)), stopping);
            assert_eq!(state.generations.session, GenerationId(1));
            assert!(projection.project(&snapshot, echo.sample()).1.is_empty());
            super::drain(&mut queue);
            let retry = reduce(&mut state, request);
            assert_eq!(
                retry.effects.len(),
                1,
                "the real phase must offer another attempt"
            );
            port.try_send(crate::device_service::effect_to_command(&retry.effects[0]).unwrap())
                .unwrap();
            assert_eq!(echo.session(), GenerationId(2));
        }

        #[test]
        fn busy_start_admission_never_changes_only_some_settings() {
            use crate::app::{ControllerPort, ControllerSendError, DeviceCommand};
            for available in 0..=3 {
                let (port, mut queue, _) = super::port(3);
                for _ in available..3 {
                    port.try_send(DeviceCommand::ApplyMute(false)).unwrap();
                }
                let outcome = port.try_send(DeviceCommand::StartSession {
                    generation: GenerationId(1),
                    receiver_ids: vec![rid(1)],
                    volume: 0.4,
                });
                let sent = super::drain(&mut queue);
                if available < 3 {
                    assert_eq!(outcome, Err(ControllerSendError::Busy));
                    assert_eq!(
                        sent.len(),
                        3 - available,
                        "rejected start with {available} slots must enqueue no partial settings"
                    );
                    assert!(sent.iter().all(|command| matches!(
                        command,
                        homepod_cast::backend::BackendCommand::SetMuted(false)
                    )));
                } else {
                    assert!(outcome.is_ok());
                    assert_eq!(sent.len(), 3);
                }
            }
        }

        #[test]
        fn busy_membership_change_preserves_actual_receivers_and_the_requested_selection() {
            use crate::app::{
                reduce, AppEvent, AppState, ControllerPort, DeviceCommand, StreamState,
            };
            let (port, mut queue, echo) = super::port(3);
            for _ in 0..3 {
                port.try_send(DeviceCommand::ApplyMute(false)).unwrap();
            }
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![
                known(1, "Kitchen", ReceiverLifecycle::Discovered),
                known(2, "Office", ReceiverLifecycle::Discovered),
            ];
            snapshot.session.phase = SessionPhase::Streaming { generation: 8 };
            snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            snapshot.desired_members = BTreeSet::from([ReceiverId::from(rid(1))]);
            let mut state = AppState::default();
            state.desired_receivers.insert(rid(1));
            state.staged_receivers.insert(rid(2));
            let (projection, events) = Projection::default().project(&snapshot, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);
            assert_eq!(transition.effects.len(), 1);
            assert_eq!(
                port.try_send(
                    crate::device_service::effect_to_command(&transition.effects[0]).unwrap()
                ),
                Ok(())
            );
            let (_, repaired) = projection.project(&snapshot, echo.sample());
            for event in repaired
                .into_iter()
                .chain(echo.group_events(&snapshot, false))
            {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Streaming {
                    generation: GenerationId(0)
                }
            );
            assert_eq!(
                state.active_receivers,
                std::collections::HashSet::from([rid(1)])
            );
            assert_eq!(
                state.desired_receivers,
                std::collections::HashSet::from([rid(1)])
            );
            assert_eq!(
                state.staged_receivers,
                std::collections::HashSet::from([rid(2)])
            );
            assert_eq!(
                state.group_operation.unwrap().status,
                crate::app::GroupOperationStatus::Failed(crate::app::GroupFailure::Busy)
            );
            assert_eq!(
                super::drain(&mut queue).len(),
                3,
                "no membership/volume/start admitted"
            );
        }

        #[test]
        fn busy_lifecycle_feedback_survives_a_full_queue_and_cannot_settle_a_newer_request() {
            use crate::app::{
                reduce, AppEvent, AppState, ControllerPort, DeviceCommand, StreamState,
            };
            use crate::app_handle::AppEventSender;
            for stopping in [false, true] {
                let (port, mut commands, echo) = super::port(3);
                for _ in 0..3 {
                    port.try_send(DeviceCommand::ApplyMute(false)).unwrap();
                }
                let mut snapshot = DeviceSnapshot::default();
                snapshot.discovery.phase = DiscoveryPhase::Running;
                snapshot.receivers = vec![known(1, "Kitchen", ReceiverLifecycle::Discovered)];
                snapshot.desired_members = BTreeSet::from([ReceiverId::from(rid(1))]);
                if stopping {
                    snapshot.session.phase = SessionPhase::Streaming { generation: 8 };
                    snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
                }
                let mut state = AppState::default();
                state.desired_receivers.insert(rid(1));
                let (projection, initial) = Projection::default().project(&snapshot, echo.sample());
                for event in initial {
                    reduce(&mut state, AppEvent::Controller(event));
                }
                let request = if stopping {
                    AppEvent::StopRequested
                } else {
                    AppEvent::StartRequested
                };
                let transition = reduce(&mut state, request.clone());
                assert_eq!(
                    port.try_send(
                        crate::device_service::effect_to_command(&transition.effects[0]).unwrap()
                    ),
                    Err(crate::app::ControllerSendError::Busy)
                );
                let (projection, repair) = projection.project(&snapshot, echo.sample());
                let mut outbox = std::collections::VecDeque::from(repair);
                assert_eq!(outbox.len(), 1, "one actual phase repairs a rejected edge");
                let old_answer = outbox[0].clone();
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                let feedback = AppEventSender::new(tx);
                feedback.try_send(AppEvent::RefreshRequested).unwrap();
                let mut gone = false;
                super::super::flush(&feedback, &mut outbox, &mut gone);
                assert_eq!(outbox.len(), 1, "busy feedback must retain the repair");
                assert!(super::super::repair_delay(outbox.len()).is_some());
                assert!(!gone);
                assert_eq!(rx.try_recv().unwrap(), AppEvent::RefreshRequested);
                super::super::flush(&feedback, &mut outbox, &mut gone);
                assert!(outbox.is_empty());
                reduce(&mut state, rx.try_recv().unwrap());
                assert_eq!(
                    state.stream,
                    if stopping {
                        StreamState::Streaming {
                            generation: GenerationId(1),
                        }
                    } else {
                        StreamState::Stopped
                    }
                );
                super::drain(&mut commands);
                let retry = reduce(&mut state, request);
                port.try_send(crate::device_service::effect_to_command(&retry.effects[0]).unwrap())
                    .unwrap();
                let pending = state.clone();
                let stale = reduce(&mut state, AppEvent::Controller(old_answer));
                assert!(!stale.snapshot_changed);
                assert_eq!(
                    state, pending,
                    "old retained repair must not settle the accepted retry"
                );
                assert!(
                    projection.project(&snapshot, echo.sample()).1.is_empty(),
                    "rejected-edge behavior must not carry over into a successful request"
                );
            }
        }

        #[test]
        fn rejected_audio_changes_keep_authoritative_controls_without_projection_loops() {
            use crate::app::{reduce, AppEvent, AppState, AudioEndpointKey, AudioEndpointRequest};

            let endpoint = backend_model::AudioEndpoint {
                id: "test-endpoint".into(),
                name: "Headphones".into(),
            };
            let mut snapshot = DeviceSnapshot {
                latency_preset: LatencyPreset::Stable,
                ..DeviceSnapshot::default()
            };
            snapshot.audio_source.active_endpoints = Some(vec![endpoint.clone()]);
            let (projection, events) = Projection::default().project(&snapshot, seen(0, 0));
            let mut state = AppState::default();
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            for request in [
                AppEvent::MuteChangeRequested(true),
                AppEvent::LatencyChoiceRequested(LatencyChoice::Normal),
                AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(
                    0,
                ))),
            ] {
                let transition = reduce(&mut state, request);
                assert_eq!(transition.effects.len(), 1);
            }

            // A refused durable write republishes persistence health, but no
            // audio value changes. The same applies to an unaccepted send.
            snapshot.persistence = backend_model::PersistenceSnapshot::Error;
            snapshot.revision += 1;
            let (projection, events) = projection.project(&snapshot, seen(0, 0));
            assert!(events.is_empty());
            assert_eq!(state.muted, Some(false));
            assert_eq!(
                state.latency.as_ref().unwrap().selected,
                LatencyChoice::Stable
            );
            assert_eq!(
                state.audio_source.as_ref().unwrap().selection,
                AudioEndpointSelection::SystemDefault
            );

            // Only accepted backend values change the controls, and their
            // projection never sends another command back to the backend.
            snapshot.muted = true;
            snapshot.latency_preset = LatencyPreset::Normal;
            snapshot.audio_source.preference = AudioEndpointPreference::Explicit {
                id: endpoint.id,
                last_known_name: endpoint.name,
            };
            let (projection, events) = projection.project(&snapshot, seen(0, 0));
            assert_eq!(events.len(), 3);
            for event in events {
                assert!(reduce(&mut state, AppEvent::Controller(event))
                    .effects
                    .is_empty());
            }
            assert_eq!(state.muted, Some(true));
            assert_eq!(
                state.latency.as_ref().unwrap().selected,
                LatencyChoice::Normal
            );
            assert_eq!(
                state.audio_source.as_ref().unwrap().selection,
                AudioEndpointSelection::Chosen(AudioEndpointKey(0))
            );
            assert!(projection.project(&snapshot, seen(0, 0)).1.is_empty());
        }

        /// Whether one event belongs to the audio triple.
        ///
        /// An exhaustive match rather than a `matches!`, so a new
        /// [`ControllerEvent`] variant has to be classified here before this
        /// file compiles instead of quietly joining the device half.
        fn is_audio(event: &ControllerEvent) -> bool {
            match event {
                ControllerEvent::MuteChanged { .. }
                | ControllerEvent::VolumeApplied { .. }
                | ControllerEvent::ReceiverLevelsChanged { .. }
                | ControllerEvent::LatencyChanged { .. }
                | ControllerEvent::AudioSourceChanged { .. } => true,
                ControllerEvent::DiscoveryCompleted { .. }
                | ControllerEvent::SavedGroupsChanged { .. }
                | ControllerEvent::GroupOperationFinished { .. }
                | ControllerEvent::DiscoveryFailed { .. }
                | ControllerEvent::DesiredReceiversChanged { .. }
                | ControllerEvent::SessionStarted { .. }
                | ControllerEvent::SessionDegraded { .. }
                | ControllerEvent::SessionRestarting { .. }
                | ControllerEvent::SessionStopped { .. }
                | ControllerEvent::SessionFailed { .. }
                | ControllerEvent::DiagnosticsUpdated { .. }
                | ControllerEvent::LiveDiagnosticsUpdated { .. }
                | ControllerEvent::ChannelClosed { .. } => false,
            }
        }

        /// What one pass says about discovery and the session.
        ///
        /// Every pass also republishes the audio triple whenever it differs
        /// from the last one, and the very first pass over any snapshot
        /// therefore carries three audio events. Those have their own tests;
        /// the ones that follow are about session and discovery answers, and
        /// asserting the audio events' position inside them would be
        /// asserting a fact nothing promises.
        fn device_events(events: Vec<ControllerEvent>) -> Vec<ControllerEvent> {
            events
                .into_iter()
                .filter(|event| {
                    !is_audio(event)
                        && !matches!(
                            event,
                            ControllerEvent::SavedGroupsChanged { .. }
                                | ControllerEvent::GroupOperationFinished { .. }
                                | ControllerEvent::DesiredReceiversChanged { .. }
                        )
                })
                .collect()
        }

        /// The capture reading one pass published, or a panic naming what it
        /// published instead.
        fn capture_reading(events: Vec<ControllerEvent>) -> crate::app::AudioSourceReading {
            let audio: Vec<ControllerEvent> = events.into_iter().filter(is_audio).collect();
            audio
                .iter()
                .find_map(|event| match event {
                    ControllerEvent::AudioSourceChanged { reading } => Some(reading.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected a capture reading, got {audio:?}"))
        }

        /// Shell generations with no session edge pressed yet, which is what
        /// every pass that is not about a start or a stop looks like.
        fn seen(discovery: u64, session: u64) -> GenerationsSeen {
            GenerationsSeen {
                discovery: GenerationId(discovery),
                session: GenerationId(session),
                session_edge: SessionEdge::Untouched,
            }
        }

        /// Shell generations one press after `seen`.
        fn pressed(session: u64, session_edge: SessionEdge) -> GenerationsSeen {
            GenerationsSeen {
                discovery: GenerationId(0),
                session: GenerationId(session),
                session_edge,
            }
        }

        fn known(
            last: u8,
            name: &str,
            lifecycle: ReceiverLifecycle,
        ) -> backend_model::ReceiverSnapshot {
            backend_model::ReceiverSnapshot {
                id: ReceiverId::from(rid(last)),
                name: name.to_owned(),
                model: "AudioAccessory5,1".to_owned(),
                lifecycle,
            }
        }

        /// The shell starts stopped and so does an untouched backend, so the
        /// first pass may only confirm that -- never invent an inventory the
        /// discovery daemon has not produced yet.
        #[test]
        fn the_first_pass_over_an_idle_backend_reports_only_a_stopped_session() {
            let (_next, events) =
                Projection::default().project(&DeviceSnapshot::default(), seen(0, 0));

            assert_eq!(
                device_events(events),
                vec![ControllerEvent::SessionStopped {
                    generation: GenerationId(0)
                }]
            );
        }

        #[test]
        fn a_running_daemon_publishes_the_whole_inventory_under_the_shell_generation() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![
                known(2, "Kueche", ReceiverLifecycle::Discovered),
                known(1, "Wohnzimmer", ReceiverLifecycle::Discovered),
            ];

            let (_next, events) = Projection::default().project(&snapshot, seen(4, 0));

            assert_eq!(
                device_events(events).first(),
                Some(&ControllerEvent::DiscoveryCompleted {
                    generation: GenerationId(4),
                    receivers: vec![
                        ReceiverState {
                            id: rid(1),
                            name: "Wohnzimmer".to_owned(),
                            // The service-record identifier the shell resolves
                            // to a localized device class. Blanking it here
                            // would call every HomePod a generic speaker.
                            model: "AudioAccessory5,1".to_owned(),
                            availability: Availability::Available,
                        },
                        ReceiverState {
                            id: rid(2),
                            name: "Kueche".to_owned(),
                            model: "AudioAccessory5,1".to_owned(),
                            availability: Availability::Available,
                        },
                    ],
                })
            );
        }

        /// A receiver the backend has no name for crosses as an empty name,
        /// not as an invented one.
        ///
        /// This is the common case rather than the exotic one: every desired
        /// member restored from persisted state is a row before discovery has
        /// seen it, and stays one for as long as the speaker is switched off.
        /// The only identifier the layers below hold is the MAC-derived
        /// identity, so the gap is left open here on purpose and closed by
        /// the presentation layer, which is the only one that can localize.
        #[test]
        fn a_receiver_without_a_name_crosses_with_an_empty_one() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![known(1, "", ReceiverLifecycle::Unavailable)];

            let (_next, events) = Projection::default().project(&snapshot, seen(1, 0));
            let events = device_events(events);
            let ControllerEvent::DiscoveryCompleted { receivers, .. } = &events[0] else {
                panic!("a running daemon publishes an inventory");
            };

            assert_eq!(receivers[0].name, "");
        }

        /// The boundary rule, independent of the backend arm that needed it.
        ///
        /// `ReceiverId` renders as `AA:BB:CC:DD:EE:FF`, and a name is the one
        /// string on this path that crosses verbatim -- which is exactly why
        /// the leak canary cannot catch this: that test depends on names
        /// crossing. So the bridge refuses a name that *is* the identity,
        /// whatever produced it.
        #[test]
        fn an_identity_shaped_name_never_reaches_the_shell() {
            let identity = ReceiverId::from(rid(1)).to_string();
            assert_eq!(
                identity, "00:00:00:00:00:01",
                "the guard has a MAC to catch"
            );
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![known(1, &identity, ReceiverLifecycle::Unavailable)];

            let (_next, events) = Projection::default().project(&snapshot, seen(1, 0));
            let rendered = format!("{events:?}");

            assert!(
                !rendered.contains(&identity),
                "a hardware identifier reached the shell as a name: {rendered}"
            );
        }

        /// The eight-state lifecycle collapses onto the shell's two-state
        /// availability. Only "not currently discoverable" may become
        /// `Unavailable`: every other state is a session concern the backend
        /// owns, and marking it unavailable would drop the receiver out of the
        /// next start's member set and out of the backend's own retry.
        #[test]
        fn only_an_undiscoverable_receiver_is_reported_unavailable() {
            let lifecycles = [
                (ReceiverLifecycle::Discovered, Availability::Available),
                (
                    ReceiverLifecycle::Connecting { attempt: 2 },
                    Availability::Available,
                ),
                (
                    ReceiverLifecycle::SettingUp {
                        role: ReceiverRole::Primary,
                        phase: SetupPhase::RtspSetup,
                    },
                    Availability::Available,
                ),
                (
                    ReceiverLifecycle::Ready {
                        role: ReceiverRole::Secondary,
                    },
                    Availability::Available,
                ),
                (
                    ReceiverLifecycle::Streaming {
                        role: ReceiverRole::Single,
                    },
                    Availability::Available,
                ),
                (
                    ReceiverLifecycle::RetryWaiting {
                        attempt: 3,
                        retry_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_234_567_890),
                    },
                    Availability::Available,
                ),
                (ReceiverLifecycle::Unavailable, Availability::Unavailable),
                (
                    ReceiverLifecycle::Failed {
                        retryable: false,
                        error: UserFacingError::new("setup did not complete"),
                    },
                    Availability::Available,
                ),
            ];
            assert_eq!(lifecycles.len(), 8, "every lifecycle state is classified");

            for (lifecycle, expected) in lifecycles {
                let mut snapshot = DeviceSnapshot::default();
                snapshot.discovery.phase = DiscoveryPhase::Running;
                snapshot.receivers = vec![known(1, "Wohnzimmer", lifecycle.clone())];

                let (_next, events) = Projection::default().project(&snapshot, seen(1, 0));
                let events = device_events(events);
                let ControllerEvent::DiscoveryCompleted { receivers, .. } = &events[0] else {
                    panic!("a running daemon publishes an inventory");
                };
                assert_eq!(
                    receivers[0].availability, expected,
                    "wrong availability for {lifecycle:?}"
                );
            }
        }

        #[test]
        fn an_unchanged_inventory_is_not_republished() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![known(1, "Wohnzimmer", ReceiverLifecycle::Discovered)];

            let (next, first) = Projection::default().project(&snapshot, seen(4, 0));
            let (_next, second) = next.project(&snapshot, seen(4, 0));

            assert_eq!(
                device_events(first).len(),
                2,
                "the first pass publishes inventory and session"
            );
            assert!(second.is_empty(), "an unchanged backend produces no events");
        }

        /// Discovery is a daemon, so "Refresh" cannot be a command. The window
        /// would stay in `Discovering` until the inventory happened to change
        /// if a refresh did not force a fresh answer.
        #[test]
        fn a_refresh_republishes_the_unchanged_inventory_under_the_new_generation() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![known(1, "Wohnzimmer", ReceiverLifecycle::Discovered)];

            let (next, _first) = Projection::default().project(&snapshot, seen(4, 0));
            let (_next, events) = next.project(&snapshot, seen(5, 0));

            assert!(matches!(
                events.as_slice(),
                [ControllerEvent::DiscoveryCompleted {
                    generation: GenerationId(5),
                    ..
                }]
            ));
        }

        #[test]
        fn a_retrying_daemon_is_reported_as_a_discovery_failure() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 3,
                retry_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_234_567_890),
            };

            let (_next, events) = Projection::default().project(&snapshot, seen(4, 0));

            assert!(matches!(
                device_events(events).first(),
                Some(ControllerEvent::DiscoveryFailed {
                    generation: GenerationId(4),
                    ..
                })
            ));
        }

        /// Retry deadlines are wall-clock instants. A projection that let one
        /// through would make every downstream assertion depend on the clock.
        #[test]
        fn a_retry_deadline_never_changes_what_the_shell_is_told() {
            let mut early = DeviceSnapshot::default();
            early.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 3,
                retry_at: SystemTime::UNIX_EPOCH,
            };
            let mut late = DeviceSnapshot::default();
            late.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 3,
                retry_at: SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000_000),
            };

            let (_a, from_early) = Projection::default().project(&early, seen(4, 0));
            let (_b, from_late) = Projection::default().project(&late, seen(4, 0));

            assert_eq!(from_early, from_late);
        }

        /// Suspend and shutdown stop the daemon. Publishing an empty inventory
        /// then would tell the user their speakers had vanished.
        #[test]
        fn a_stopped_daemon_publishes_no_inventory_at_all() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Stopped;
            snapshot.receivers = vec![known(1, "Wohnzimmer", ReceiverLifecycle::Discovered)];

            let (_next, events) = Projection::default().project(&snapshot, seen(4, 0));

            assert!(!events
                .iter()
                .any(|event| matches!(event, ControllerEvent::DiscoveryCompleted { .. })));
        }

        /// The capability the legacy service could not provide: it reported
        /// every requested receiver as active. The backend knows which ones
        /// actually negotiated.
        #[test]
        fn streaming_reports_the_receivers_that_actually_negotiated() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.session.phase = SessionPhase::Streaming { generation: 99 };
            snapshot.session.desired = BTreeSet::from([
                ReceiverId::from(rid(1)),
                ReceiverId::from(rid(2)),
                ReceiverId::from(rid(3)),
            ]);
            snapshot.session.active =
                BTreeSet::from([ReceiverId::from(rid(1)), ReceiverId::from(rid(3))]);

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 7));

            assert_eq!(
                device_events(events),
                vec![ControllerEvent::SessionStarted {
                    generation: GenerationId(7),
                    active_receiver_ids: vec![rid(1), rid(3)],
                }]
            );
        }

        /// A reduced delivery is its own answer, not a streaming one.
        ///
        /// It used to cross as `SessionStarted`, on the grounds that the
        /// active set already said which receivers carried audio. It does not
        /// say enough: the shell's desired set is not the committed
        /// membership of this generation, so from an active set alone the
        /// window cannot tell a complete delivery from an incomplete one, and
        /// it presented both as healthy.
        #[test]
        fn a_degraded_session_reports_its_own_phase_with_the_receivers_that_carry_audio() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.session.phase = SessionPhase::Degraded { generation: 99 };
            snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 7));

            assert_eq!(
                device_events(events),
                vec![ControllerEvent::SessionDegraded {
                    generation: GenerationId(7),
                    active_receiver_ids: vec![rid(1)],
                }]
            );
        }

        /// A restart is announced and bounded, and it is still not a stop:
        /// flipping the window to "stopped" and back for every membership
        /// change, network rebind, or primary replacement would be wrong.
        /// Publishing nothing was wrong too -- the audio really does break off
        /// while the group is rebuilt, and the window had no way to say why.
        #[test]
        fn an_announced_restart_publishes_a_rebuild_and_the_next_stream_publishes_the_new_set() {
            let mut streaming = DeviceSnapshot::default();
            streaming.session.phase = SessionPhase::Streaming { generation: 99 };
            streaming.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            let (after_start, _) = Projection::default().project(&streaming, seen(0, 7));

            let mut restarting = streaming.clone();
            restarting.session.phase = SessionPhase::Restarting {
                generation: 100,
                reason: RestartReason::MembershipChange,
            };
            let (after_restart, during) = after_start.project(&restarting, seen(0, 7));
            assert_eq!(
                during,
                vec![ControllerEvent::SessionRestarting {
                    generation: GenerationId(7),
                }],
                "a restart in flight is a rebuild, neither a stop nor silence"
            );

            let mut restarted = streaming.clone();
            restarted.session.phase = SessionPhase::Streaming { generation: 100 };
            restarted.session.active =
                BTreeSet::from([ReceiverId::from(rid(1)), ReceiverId::from(rid(2))]);
            let (_next, after) = after_restart.project(&restarted, seen(0, 7));

            assert_eq!(
                after,
                vec![ControllerEvent::SessionStarted {
                    generation: GenerationId(7),
                    active_receiver_ids: vec![rid(1), rid(2)],
                }]
            );
        }

        #[test]
        fn transient_setup_and_teardown_publish_nothing() {
            for phase in [
                SessionPhase::Starting { generation: 99 },
                SessionPhase::Stopping { generation: 99 },
            ] {
                let mut snapshot = DeviceSnapshot::default();
                snapshot.session.phase = phase.clone();

                let (_next, events) = Projection::default().project(&snapshot, seen(0, 7));

                assert!(
                    device_events(events).is_empty(),
                    "{phase:?} must not move the window"
                );
            }
        }

        #[test]
        fn a_failed_session_carries_the_backends_redacted_reason() {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.session.phase = SessionPhase::Failed {
                generation: 99,
                error: UserFacingError::new("no receiver could be kept active"),
            };

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 7));

            assert_eq!(
                device_events(events),
                vec![ControllerEvent::SessionFailed {
                    generation: GenerationId(7),
                    summary: "no receiver could be kept active".to_owned(),
                }]
            );
        }

        /// The backend counts its own session generations and the shell counts
        /// its own. Stamping shell events with the backend's number would make
        /// the reducer's generation guard drop every one of them.
        #[test]
        fn the_backend_generation_never_reaches_the_shell() {
            let mut low = DeviceSnapshot::default();
            low.session.phase = SessionPhase::Streaming { generation: 1 };
            low.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            let mut high = low.clone();
            high.session.phase = SessionPhase::Streaming {
                generation: 4_000_000,
            };

            let (_a, from_low) = Projection::default().project(&low, seen(0, 7));
            let (_b, from_high) = Projection::default().project(&high, seen(0, 7));

            assert_eq!(from_low, from_high);
            assert_eq!(
                device_events(from_low),
                vec![ControllerEvent::SessionStarted {
                    generation: GenerationId(7),
                    active_receiver_ids: vec![rid(1)],
                }]
            );
        }

        /// The reported defect, in one pass.
        ///
        /// Pressing Stop raises the session generation *and* wakes the bridge
        /// from the command itself, so the very first pass afterwards still
        /// sees a `Streaming` snapshot. Answering the raised generation from
        /// it would emit `SessionStarted` under exactly the generation the
        /// reducer is waiting for -- so the guard would pass it -- and the
        /// window would snap straight back to streaming.
        #[test]
        fn a_stop_press_is_not_answered_by_the_stream_it_is_about_to_end() {
            let mut streaming = DeviceSnapshot::default();
            streaming.session.phase = SessionPhase::Streaming { generation: 99 };
            streaming.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);

            let (after_start, _) = Projection::default().project(&streaming, seen(0, 3));
            let (_next, after_stop) =
                after_start.project(&streaming, pressed(4, SessionEdge::Stop));

            assert!(
                after_stop.is_empty(),
                "a stop must not be answered with the stream it ends: {after_stop:?}"
            );
        }

        /// The mirror image: pressing Start against a not-yet-moved backend
        /// must not report the stop it is replacing.
        #[test]
        fn a_start_press_is_not_answered_by_the_stopped_session_it_replaces() {
            let stopped = DeviceSnapshot::default();

            let (after_stop, _) = Projection::default().project(&stopped, seen(0, 3));
            let (_next, after_start) = after_stop.project(&stopped, pressed(4, SessionEdge::Start));

            assert!(
                after_start.is_empty(),
                "a start must not be answered with the stop it replaces: {after_start:?}"
            );
        }

        /// The other half of the same rule, and the reason it cannot simply
        /// be "publish only on a content change": a run intent that does not
        /// change produces no reconcile and therefore no further snapshot, so
        /// an unanswered stop would leave the window in `Stopping` forever.
        #[test]
        fn a_stop_against_an_already_stopped_backend_is_answered_at_once() {
            let stopped = DeviceSnapshot::default();

            let (first, _) = Projection::default().project(&stopped, seen(0, 3));
            let (_next, events) = first.project(&stopped, pressed(4, SessionEdge::Stop));

            assert_eq!(
                events,
                vec![ControllerEvent::SessionStopped {
                    generation: GenerationId(4)
                }]
            );
        }

        #[test]
        fn a_start_against_an_already_streaming_backend_is_answered_at_once() {
            let mut streaming = DeviceSnapshot::default();
            streaming.session.phase = SessionPhase::Streaming { generation: 99 };
            streaming.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);

            let (first, _) = Projection::default().project(&streaming, seen(0, 3));
            let (_next, events) = first.project(&streaming, pressed(4, SessionEdge::Start));

            assert_eq!(
                events,
                vec![ControllerEvent::SessionStarted {
                    generation: GenerationId(4),
                    active_receiver_ids: vec![rid(1)],
                }]
            );
        }

        /// Retrying a failed start with the same members changes neither the
        /// backend's run intent nor its desired set, so the backend has
        /// nothing to re-derive. The failure is the answer, and it has to
        /// carry the new generation or the window stays in `Starting`.
        #[test]
        fn a_retry_against_an_unchanged_failure_is_answered_at_once() {
            let mut failed = DeviceSnapshot::default();
            failed.session.phase = SessionPhase::Failed {
                generation: 99,
                error: UserFacingError::new("no receiver could be kept active"),
            };

            let (first, _) = Projection::default().project(&failed, seen(0, 3));
            let (_next, events) = first.project(&failed, pressed(4, SessionEdge::Start));

            assert_eq!(
                events,
                vec![ControllerEvent::SessionFailed {
                    generation: GenerationId(4),
                    summary: "no receiver could be kept active".to_owned(),
                }]
            );
        }

        /// A stopped run intent tears the session down, so the truthful
        /// answer is already on its way. Re-reporting the old failure would
        /// put an error notice on screen for a deliberate stop.
        #[test]
        fn a_stop_is_never_answered_with_a_stale_failure() {
            let mut failed = DeviceSnapshot::default();
            failed.session.phase = SessionPhase::Failed {
                generation: 99,
                error: UserFacingError::new("no receiver could be kept active"),
            };

            let (first, _) = Projection::default().project(&failed, seen(0, 3));
            let (_next, events) = first.project(&failed, pressed(4, SessionEdge::Stop));

            assert!(events.is_empty(), "a stop is not a failure: {events:?}");
        }

        /// The level-driven repair: the pass that withholds an answer is not
        /// the last one. As soon as the backend actually moves, the raised
        /// generation is answered from the settled snapshot.
        #[test]
        fn the_backend_settling_answers_the_generation_the_stale_pass_withheld() {
            let mut streaming = DeviceSnapshot::default();
            streaming.session.phase = SessionPhase::Streaming { generation: 99 };
            streaming.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);

            let (started, _) = Projection::default().project(&streaming, seen(0, 3));
            let stop = pressed(4, SessionEdge::Stop);
            let (stale, withheld) = started.project(&streaming, stop);
            assert!(withheld.is_empty());

            let mut tearing_down = streaming.clone();
            tearing_down.session.phase = SessionPhase::Stopping { generation: 99 };
            let (in_flight, during) = stale.project(&tearing_down, stop);
            assert!(during.is_empty(), "a teardown in flight is not an answer");

            let (_next, settled) = in_flight.project(&DeviceSnapshot::default(), stop);
            assert_eq!(
                settled,
                vec![ControllerEvent::SessionStopped {
                    generation: GenerationId(4)
                }]
            );
        }

        /// Backend fields the shell still has no vocabulary for are not merely
        /// unused -- they must not change what the shell is told, or an
        /// auto-connect flag or a persistence error would silently restart the
        /// session display.
        ///
        /// Mute, latency, and the capture source used to be on this list. They
        /// have their own events now, and the test directly below is the
        /// counterpart: those three *must* change what the shell is told.
        #[test]
        fn settings_the_shell_cannot_render_yet_change_nothing() {
            let mut plain = DeviceSnapshot::default();
            plain.session.phase = SessionPhase::Streaming { generation: 1 };
            plain.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);

            let mut decorated = plain.clone();
            decorated.auto_connect = true;
            decorated.persistence = homepod_cast::PersistenceSnapshot::Error;
            decorated.session.audio_flow = homepod_cast::AudioFlow::SilenceBridged;
            decorated.session.resume_pending = true;
            decorated.session.primary = Some(ReceiverId::from(rid(1)));

            let (_a, from_plain) = Projection::default().project(&plain, seen(0, 7));
            let (_b, from_decorated) = Projection::default().project(&decorated, seen(0, 7));

            assert_eq!(from_plain, from_decorated);
        }

        /// One capture endpoint, named the way Windows names one.
        fn endpoint(id: &str, name: &str) -> backend_model::AudioEndpoint {
            backend_model::AudioEndpoint {
                id: id.to_owned(),
                name: name.to_owned(),
            }
        }

        #[test]
        fn endpoint_inventory_unknown_empty_capture_proof_and_recovery_remain_distinct() {
            let mut source = AudioSourceSnapshot {
                preference: AudioEndpointPreference::Explicit {
                    id: "usb".into(),
                    last_known_name: "USB DAC".into(),
                },
                ..AudioSourceSnapshot::default()
            };
            let mut table = super::super::EndpointTable::default();
            let unknown = super::super::audio_echo(&source, &mut table);
            assert!(!unknown.endpoints_known);
            assert_eq!(
                unknown.selection,
                AudioEndpointSelection::ChosenUnverified {
                    name: "USB DAC".into()
                }
            );
            source.active_endpoints = Some(Vec::new());
            let empty = super::super::audio_echo(&source, &mut table);
            assert!(empty.endpoints_known);
            assert_eq!(
                empty.selection,
                AudioEndpointSelection::ChosenButMissing {
                    name: "USB DAC".into()
                }
            );
            source.captured_endpoint = Some(endpoint("usb", "Live DAC"));
            let live = super::super::audio_echo(&source, &mut table);
            assert_eq!(
                live.selection,
                AudioEndpointSelection::ChosenUnverified {
                    name: "Live DAC".into()
                }
            );
            assert_eq!(live.captured_name.as_deref(), Some("Live DAC"));
            source.active_endpoints = Some(vec![endpoint("usb", "Live DAC")]);
            let restored = super::super::audio_echo(&source, &mut table);
            assert_eq!(
                restored.selection,
                AudioEndpointSelection::Chosen(restored.endpoints[0].key)
            );
            source.refresh_failed = true;
            let failed = super::super::audio_echo(&source, &mut table);
            assert!(
                failed.refresh_failed,
                "refresh failure must reach the shell without raw error text"
            );
            assert_eq!(failed.endpoints, restored.endpoints);
            assert_eq!(failed.selection, restored.selection);
            source.refresh_failed = false;
            assert!(!super::super::audio_echo(&source, &mut table).refresh_failed);
        }

        /// A backend snapshot whose audio half is worth reading.
        fn with_audio(source: AudioSourceSnapshot) -> DeviceSnapshot {
            DeviceSnapshot {
                audio_source: source,
                ..DeviceSnapshot::default()
            }
        }

        #[test]
        fn mute_crosses_as_its_own_event_and_only_while_it_moves() {
            let muted = DeviceSnapshot {
                muted: true,
                ..DeviceSnapshot::default()
            };

            let (first, events) = Projection::default().project(&muted, seen(0, 0));
            assert!(
                events.contains(&ControllerEvent::MuteChanged { muted: true }),
                "the first pass has to say what mute is: {events:?}"
            );

            let (second, again) = first.project(&muted, seen(0, 0));
            assert!(
                !again
                    .iter()
                    .any(|event| matches!(event, ControllerEvent::MuteChanged { .. })),
                "an unchanged mute is not republished: {again:?}"
            );

            let mut unmuted = muted.clone();
            unmuted.muted = false;
            let (_third, moved) = second.project(&unmuted, seen(0, 0));
            assert!(
                moved.contains(&ControllerEvent::MuteChanged { muted: false }),
                "a mute that moved has to cross: {moved:?}"
            );
        }

        /// The gated profiles cross as a code, and the backend's own sentence
        /// for the same refusal never leaves this file.
        #[test]
        fn a_gated_latency_profile_crosses_as_a_code_and_never_as_the_backends_sentence() {
            const REASON: &str = "the Low latency profile is disabled until validation passes";
            let snapshot = DeviceSnapshot {
                latency_preset: LatencyPreset::Normal,
                latency_presets: vec![
                    backend_model::LatencyPresetOption {
                        preset: LatencyPreset::Low,
                        disabled_reason: Some(UserFacingError::new(REASON)),
                    },
                    backend_model::LatencyPresetOption {
                        preset: LatencyPreset::Normal,
                        disabled_reason: None,
                    },
                ],
                ..DeviceSnapshot::default()
            };

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 0));
            let reading = events
                .iter()
                .find_map(|event| match event {
                    ControllerEvent::LatencyChanged { reading } => Some(reading),
                    _ => None,
                })
                .expect("the first pass publishes the latency reading");

            assert_eq!(reading.selected, LatencyChoice::Normal);
            assert_eq!(
                reading.options,
                vec![
                    LatencyOption {
                        choice: LatencyChoice::Low,
                        unavailable: Some(LatencyUnavailable::NotValidated),
                    },
                    LatencyOption {
                        choice: LatencyChoice::Normal,
                        unavailable: None,
                    },
                ]
            );
            assert!(
                !format!("{events:?}").contains("disabled until"),
                "the backend's own sentence crossed: {events:?}"
            );
        }

        /// The whole point of the opaque key.
        ///
        /// The Windows endpoint ID identifies the machine's hardware. It may
        /// not appear in an event, and the key that stands for it may not be
        /// derived from it either -- a truncation or a hash would put the
        /// question "can this be reversed?" where a counter needs no answer.
        #[test]
        fn a_capture_endpoint_crosses_under_a_key_that_holds_no_part_of_its_id() {
            let id = "{0.0.0.00000000}.{d3f1b2a4-0c55-4f2e-9a11-7c6b5e4d3a21}";
            let snapshot = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint(id, "Arctis 7 Chat")]),
                preference: AudioEndpointPreference::SystemDefault,
                captured_endpoint: Some(endpoint(id, "Arctis 7 Chat")),
                state: homepod_cast::AudioSourceState::Capturing,
                ..Default::default()
            });

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 0));
            let reading = events
                .iter()
                .find_map(|event| match event {
                    ControllerEvent::AudioSourceChanged { reading } => Some(reading),
                    _ => None,
                })
                .expect("the first pass publishes the capture reading");

            assert_eq!(reading.endpoints.len(), 1);
            assert_eq!(reading.endpoints[0].name, "Arctis 7 Chat");
            assert_eq!(reading.captured_name.as_deref(), Some("Arctis 7 Chat"));
            assert_eq!(reading.state, CaptureState::Capturing);
            assert_eq!(reading.selection, AudioEndpointSelection::SystemDefault);

            let published = format!("{events:?}");
            assert!(
                !published.contains("0.0.0.00000000") && !published.contains("d3f1b2a4"),
                "a raw Windows endpoint ID crossed: {published}"
            );
        }

        #[test]
        fn hardware_check_actual_source_keys_distinguish_duplicate_display_names() {
            use crate::backend_bridge::{audio_echo, EndpointTable};
            let mut table = EndpointTable::default();
            let mut source = AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![
                    endpoint("private-id-A", "Same name"),
                    endpoint("private-id-B", "Same name"),
                ]),
                preference: AudioEndpointPreference::SystemDefault,
                captured_endpoint: Some(endpoint("private-id-A", "Same name")),
                state: homepod_cast::AudioSourceState::Capturing,
                ..Default::default()
            };
            let first = audio_echo(&source, &mut table);
            source.captured_endpoint = Some(endpoint("private-id-B", "Same name"));
            let second = audio_echo(&source, &mut table);
            assert_eq!(first.captured_name, second.captured_name);
            assert_ne!(first.captured_key, second.captured_key);
            assert_eq!(first.captured_key, Some(first.endpoints[0].key));
            assert_eq!(second.captured_key, Some(second.endpoints[1].key));
            assert!(!format!("{second:?}").contains("private-id"));
        }

        /// Endpoints come and go. A key must stay with its endpoint while it
        /// is there, and must never come to mean a different one afterwards --
        /// otherwise a click that raced a device change would silently select
        /// the wrong microphone.
        #[test]
        fn a_key_stays_with_its_endpoint_and_is_never_reused_after_it_disappears() {
            let headset = "{0.0.0.00000000}.{aaaa}";
            let speakers = "{0.0.0.00000000}.{bbbb}";

            let both = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![
                    endpoint(headset, "Arctis 7 Chat"),
                    endpoint(speakers, "Realtek HDMI"),
                ]),
                ..AudioSourceSnapshot::default()
            });
            let (after_both, _) = Projection::default().project(&both, seen(0, 0));
            let keys = |projection: &Projection, snapshot: &DeviceSnapshot| {
                let (_next, events) = projection.project(snapshot, seen(0, 0));
                capture_reading(events).endpoints
            };

            let first = keys(&Projection::default(), &both);
            assert_ne!(first[0].key, first[1].key, "two endpoints, two keys");

            // The headset is unplugged, then plugged back in.
            let without = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint(speakers, "Realtek HDMI")]),
                ..AudioSourceSnapshot::default()
            });
            let (after_loss, _) = after_both.project(&without, seen(0, 0));
            let gone = keys(&after_both, &without);
            assert_eq!(gone.len(), 1);
            assert_eq!(
                gone[0].key, first[1].key,
                "a surviving endpoint keeps the key it was published under"
            );

            let returned = keys(&after_loss, &both);
            assert_eq!(
                returned[0].key, first[0].key,
                "an endpoint that comes back is the same endpoint, so it is the same key"
            );

            // A genuinely new endpoint gets a key of its own, never a retired
            // one.
            let third = "{0.0.0.00000000}.{cccc}";
            let with_third = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint(third, "Monitor")]),
                ..AudioSourceSnapshot::default()
            });
            let fresh = keys(&after_loss, &with_third);
            assert!(
                fresh[0].key != first[0].key && fresh[0].key != first[1].key,
                "a new endpoint must never inherit a key that already means another one"
            );
        }

        /// The projection is `Debug`, and the endpoint IDs it holds are not.
        ///
        /// Everything else in this module guards the *published* side: what
        /// the shell is handed, what the window says. This guards the side
        /// nobody publishes on purpose. One `tracing::debug!("{projection:?}")`
        /// in this file -- a plausible line to write while chasing a capture
        /// bug -- would otherwise put every raw `{0.0.0.00000000}.{guid}` on
        /// the machine into a log, and no compile error and no shell-side
        /// canary would notice, because none of them read this file's own
        /// formatting.
        #[test]
        fn the_projections_own_debug_output_holds_no_endpoint_id() {
            let secret = "{0.0.0.00000000}.{9f2c-DEBUG-CANARY}";
            let snapshot = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint(secret, "Arctis 7 Chat")]),
                preference: AudioEndpointPreference::Explicit {
                    id: secret.to_owned(),
                    last_known_name: "Arctis 7 Chat".into(),
                },
                ..AudioSourceSnapshot::default()
            });

            let (projection, _events) = Projection::default().project(&snapshot, seen(0, 0));
            let printed = format!("{projection:?}");

            assert!(
                printed.contains("EndpointEntry"),
                "the table is not in the output at all, so this passes vacuously: \
                 {printed}"
            );
            assert!(
                !printed.contains("DEBUG-CANARY") && !printed.contains("0.0.0.00000000"),
                "the raw endpoint ID is printable: {printed}"
            );
            assert!(
                printed.contains("Arctis 7 Chat"),
                "the display name is not the secret and stays legible: {printed}"
            );
        }

        /// Paragraph 7.4's unfulfillable preference, read off the snapshot
        /// rather than guessed: the stored ID is simply not among the offered
        /// ones.
        #[test]
        fn a_preference_whose_endpoint_is_absent_crosses_as_missing_with_its_saved_name() {
            let snapshot = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint("{0.0.0.00000000}.{bbbb}", "Realtek HDMI")]),
                preference: AudioEndpointPreference::Explicit {
                    id: "{0.0.0.00000000}.{aaaa}".to_owned(),
                    last_known_name: "Arctis 7 Chat".to_owned(),
                },
                captured_endpoint: None,
                state: homepod_cast::AudioSourceState::Unavailable,
                ..Default::default()
            });

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 0));
            let reading = capture_reading(events);

            assert_eq!(
                reading.selection,
                AudioEndpointSelection::ChosenButMissing {
                    name: "Arctis 7 Chat".to_owned(),
                }
            );
            assert_eq!(reading.captured_name, None, "nothing is being captured");
        }

        /// The same preference, while its endpoint *is* offered, resolves to
        /// the key of that endpoint -- and to nothing else.
        #[test]
        fn a_preference_whose_endpoint_is_offered_crosses_as_that_endpoints_key() {
            let id = "{0.0.0.00000000}.{aaaa}";
            let snapshot = with_audio(AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![
                    endpoint("{0.0.0.00000000}.{bbbb}", "Realtek HDMI"),
                    endpoint(id, "Arctis 7 Chat"),
                ]),
                preference: AudioEndpointPreference::Explicit {
                    id: id.to_owned(),
                    last_known_name: "an older name nobody should read".to_owned(),
                },
                captured_endpoint: Some(endpoint(id, "Arctis 7 Chat")),
                state: homepod_cast::AudioSourceState::Capturing,
                ..Default::default()
            });

            let (_next, events) = Projection::default().project(&snapshot, seen(0, 0));
            let reading = capture_reading(events);

            assert_eq!(
                reading.selection,
                AudioEndpointSelection::Chosen(reading.endpoints[1].key)
            );
            assert!(
                !format!("{reading:?}").contains("older name"),
                "the saved name is not read while the endpoint is present: {reading:?}"
            );
        }
    }

    mod event_projection {
        use homepod_cast::backend::event::{
            BackendEvent, ErrorScope, NoticeCode as BackendNoticeCode, Severity as BackendSeverity,
        };
        use homepod_cast::UserFacingError;

        use super::super::{project_event, GenerationsSeen, SessionEdge};
        use crate::app::{ControllerEvent, GenerationId};

        fn seen() -> GenerationsSeen {
            GenerationsSeen {
                discovery: GenerationId(4),
                session: GenerationId(7),
                session_edge: SessionEdge::Untouched,
            }
        }

        /// Free-form backend prose in the one field that could carry it.
        const CANARY: &str = "LEAK-CANARY";

        fn notice(code: BackendNoticeCode) -> BackendEvent {
            BackendEvent::Notice {
                severity: BackendSeverity::Error,
                code,
                scope: ErrorScope::Discovery,
                message: format!("devices could not be searched for: {CANARY}"),
            }
        }

        /// A notice can arrive while the snapshot's discovery phase is still
        /// `Running` -- the network-change monitor refusing to register is
        /// reported exactly that way -- so this is the one backend event
        /// worth translating today. What crosses is the *code*, never the
        /// message.
        #[test]
        fn a_discovery_notice_becomes_a_discovery_failure_under_the_shell_generation() {
            assert_eq!(
                project_event(&notice(BackendNoticeCode::DiscoveryFailed), seen()),
                Some(ControllerEvent::DiscoveryFailed {
                    generation: GenerationId(4),
                    summary: "Devices could not be searched for.".to_owned(),
                })
            );
        }

        /// The canary the projection tests cannot carry: `Notice.message` is
        /// the only free-form string a backend author can put in front of the
        /// bridge, and at least one emitter interpolates an error into it.
        /// Nothing on this path may be forwarded verbatim.
        #[test]
        fn no_backend_prose_survives_an_event_translation() {
            for code in [
                BackendNoticeCode::DiscoveryFailed,
                BackendNoticeCode::SessionFailed,
                BackendNoticeCode::CaptureUnavailable,
                BackendNoticeCode::PersistenceWrite,
                BackendNoticeCode::QueueOverloaded,
            ] {
                let rendered = format!("{:?}", project_event(&notice(code), seen()));
                assert!(
                    !rendered.contains(CANARY),
                    "backend prose reached the shell for {code:?}: {rendered}"
                );
            }
        }

        /// Severity is dropped with the message, so a warning and an error
        /// with the same code cannot become two different shell events.
        #[test]
        fn the_notice_severity_never_changes_what_the_shell_is_told() {
            let event = |severity| BackendEvent::Notice {
                severity,
                code: BackendNoticeCode::DiscoveryFailed,
                scope: ErrorScope::Discovery,
                message: String::new(),
            };

            assert_eq!(
                project_event(&event(BackendSeverity::Warning), seen()),
                project_event(&event(BackendSeverity::Error), seen())
            );
        }

        /// Everything else is deliberately dropped: the snapshot is the
        /// authority on session state, and the remaining notices name
        /// conditions the shell has no vocabulary for yet.
        #[test]
        fn every_other_backend_event_is_dropped_on_purpose() {
            let dropped = [
                BackendEvent::CommandCompleted { id: 1 },
                BackendEvent::CommandFailed {
                    id: 2,
                    error: UserFacingError::new("rejected"),
                },
                notice(BackendNoticeCode::SessionFailed),
                notice(BackendNoticeCode::CaptureUnavailable),
                notice(BackendNoticeCode::PersistenceWrite),
                notice(BackendNoticeCode::QueueOverloaded),
            ];
            assert_eq!(dropped.len(), 6);

            for event in &dropped {
                assert_eq!(
                    project_event(event, seen()),
                    None,
                    "{event:?} must not reach the shell yet"
                );
            }
        }
    }

    mod privacy {
        use std::collections::BTreeSet;

        use homepod_cast::backend::model as backend_model;
        use homepod_cast::{
            AudioEndpointPreference, AudioSourceSnapshot, DeviceSnapshot, DiscoveryPhase,
            LatencyPreset, ReceiverId, ReceiverLifecycle, SavedGroupId, SavedGroupMember,
            SessionPhase, UserFacingError, Volume,
        };

        use super::super::{GenerationsSeen, Projection, SessionEdge};
        use super::rid;
        use crate::app::{reduce, AppEvent, AppState, GenerationId, UiSnapshot};

        const CANARY: &str = "LEAK-CANARY";

        /// Every backend field that must never reach the shell, stuffed with
        /// one marker. The receiver name is the one string that legitimately
        /// crosses, so it carries a different, expected value.
        fn poisoned() -> DeviceSnapshot {
            // The endpoint's *name* is what Windows shows the user in its
            // own volume mixer, and it crosses on purpose -- the same
            // decision the receiver name above records. So it carries a real
            // name, and the ID beside it carries the marker.
            let endpoint = backend_model::AudioEndpoint {
                id: format!("{{0.0.0.00000000}}.{{{CANARY}}}"),
                name: "Kopfhoerer".to_owned(),
            };
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.generation = 4_000_000;
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![backend_model::ReceiverSnapshot {
                id: ReceiverId::from(rid(1)),
                name: "Wohnzimmer".to_owned(),
                model: "AudioAccessory5,1".to_owned(),
                lifecycle: ReceiverLifecycle::Failed {
                    retryable: true,
                    error: UserFacingError::new(CANARY),
                },
            }];
            snapshot.saved_groups = vec![backend_model::SavedGroupSnapshot {
                id: SavedGroupId::generate(),
                name: "Evening".to_owned(),
                members: vec![SavedGroupMember {
                    receiver: ReceiverId::from(rid(1)),
                    last_known_name: "Wohnzimmer".to_owned(),
                    level: Volume::UNITY,
                }],
            }];
            snapshot.audio_source = AudioSourceSnapshot {
                refresh_failed: false,
                active_endpoints: Some(vec![endpoint.clone()]),
                preference: AudioEndpointPreference::Explicit {
                    id: format!("{{0.0.0.00000000}}.{{{CANARY}}}"),
                    last_known_name: CANARY.to_owned(),
                },
                captured_endpoint: Some(endpoint),
                state: homepod_cast::AudioSourceState::Capturing,
                ..Default::default()
            };
            snapshot.latency_presets = vec![backend_model::LatencyPresetOption {
                preset: LatencyPreset::Low,
                disabled_reason: Some(UserFacingError::new(CANARY)),
            }];
            snapshot.session.phase = SessionPhase::Streaming { generation: 1 };
            snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            snapshot
        }

        fn seen() -> GenerationsSeen {
            GenerationsSeen {
                discovery: GenerationId(4),
                session: GenerationId(7),
                session_edge: SessionEdge::Untouched,
            }
        }

        #[test]
        fn no_backend_material_reaches_a_controller_event() {
            let (_next, events) = Projection::default().project(&poisoned(), seen());

            let rendered = format!("{events:?}");
            assert!(
                rendered.contains("Wohnzimmer"),
                "the test is vacuous if nothing crossed at all"
            );
            assert!(
                !rendered.contains(CANARY),
                "backend material reached a shell event: {rendered}"
            );
        }

        /// The end of the chain, not the middle: whatever the projection
        /// emitted is fed through the real reducer and rendered into the real
        /// `UiSnapshot` a renderer would read.
        #[test]
        fn no_backend_material_reaches_the_ui_snapshot() {
            let (_next, events) = Projection::default().project(&poisoned(), seen());

            let mut state = AppState::default();
            state.generations.discovery = GenerationId(4);
            state.generations.session = GenerationId(7);
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            let rendered = format!("{:?}", UiSnapshot::from_state(&state));
            assert!(
                rendered.contains("Wohnzimmer"),
                "the test is vacuous if the events never landed"
            );
            assert!(
                !rendered.contains(CANARY),
                "backend material reached the UI snapshot: {rendered}"
            );
        }
    }

    /// The reported defect end to end: the real reducer, the real port, the
    /// real effect-to-command mapping, and the real projection, with nothing
    /// stubbed but the backend snapshot.
    mod round_trip {
        use std::collections::BTreeSet;

        use homepod_cast::{
            DeviceSnapshot, DiscoveryPhase, ReceiverId, RestartReason, SessionPhase,
        };

        use super::super::{BackendPort, GenerationEcho, Projection};
        use super::{port, rid};
        use crate::app::{
            reduce, AppEffect, AppEvent, AppState, Availability, ControllerPort, CorrectiveAction,
            DiscoverySnapshot, GenerationId, ReceiverState, ResolvedLocale, StreamState,
            UiSnapshot,
        };
        use crate::device_service::effect_to_command;
        use crate::ui::i18n::{Catalog, TextKey};
        use crate::ui::presentation::{
            LifecycleAction, MetricFreshness, OverviewModel, RoutePhase, SessionVisualPhase,
        };

        /// Shell state with one available, selected receiver and nothing else.
        fn ready_to_start() -> AppState {
            let mut state = AppState {
                receivers: vec![ReceiverState {
                    id: rid(1),
                    name: "Wohnzimmer".to_owned(),
                    model: "AudioAccessory5,1".to_owned(),
                    availability: Availability::Available,
                }],
                ..AppState::default()
            };
            state.staged_receivers.insert(rid(1));
            state.desired_receivers.insert(rid(1));
            state
        }

        fn streaming_backend() -> DeviceSnapshot {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.session.phase = SessionPhase::Streaming { generation: 99 };
            snapshot.session.active = BTreeSet::from([ReceiverId::from(rid(1))]);
            snapshot
        }

        /// Pressing Stop must not put the window back into `Streaming`.
        ///
        /// The whole chain matters here. The stop wakes the bridge through
        /// the very command that requested it, so the first projection pass
        /// runs against a backend that is still streaming; the reducer's
        /// generation guard cannot help, because the stale answer would carry
        /// exactly the generation the guard is waiting for.
        #[test]
        fn pressing_stop_leaves_the_window_stopping_until_the_backend_stops() {
            let (port, _rx, echo) = port(16);
            let mut state = ready_to_start();
            let mut projection = Projection::default();
            let backend = streaming_backend();

            for effect in reduce(&mut state, AppEvent::StartRequested).effects {
                let command = effect_to_command(&effect).expect("a start is a device command");
                port.try_send(command).expect("the queue has room");
            }
            let (next, events) = projection.project(&backend, echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Streaming {
                    generation: GenerationId(1)
                }
            );

            let stop = reduce(&mut state, AppEvent::StopRequested);
            assert_eq!(
                stop.effects,
                vec![AppEffect::StopSession {
                    generation: GenerationId(2)
                }]
            );
            for effect in stop.effects {
                let command = effect_to_command(&effect).expect("a stop is a device command");
                port.try_send(command).expect("the queue has room");
            }

            // The backend has not moved yet -- this is the pass the stop
            // itself woke.
            let (next, events) = projection.project(&backend, echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Stopping {
                    generation: GenerationId(2)
                },
                "the window snapped back to the stream the user just ended"
            );

            let (_next, events) = projection.project(&DeviceSnapshot::default(), echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(state.stream, StreamState::Stopped);
            assert!(state.active_receivers.is_empty());
        }

        /// The mirror image, for the same reason.
        #[test]
        fn pressing_start_leaves_the_window_starting_until_the_backend_streams() {
            let (port, _rx, echo) = port(16);
            let mut state = ready_to_start();
            let mut projection = Projection::default();
            let stopped = DeviceSnapshot {
                desired_members: BTreeSet::from([ReceiverId::from(rid(1))]),
                ..Default::default()
            };

            // The first pass settles the window on the idle backend.
            let (next, events) = projection.project(&stopped, echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(state.stream, StreamState::Stopped);

            for effect in reduce(&mut state, AppEvent::StartRequested).effects {
                let command = effect_to_command(&effect).expect("a start is a device command");
                port.try_send(command).expect("the queue has room");
            }

            let (next, events) = projection.project(&stopped, echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Starting {
                    generation: GenerationId(1)
                },
                "the window fell back to the stop the user just replaced"
            );

            let (_next, events) = projection.project(&streaming_backend(), echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert_eq!(
                state.stream,
                StreamState::Streaming {
                    generation: GenerationId(1)
                }
            );
        }

        /// The gap the reducer's silent catch-all left open.
        ///
        /// A discovery daemon that cannot be maintained already produced a
        /// `DiscoveryFailed` here, and the reducer discarded it without a
        /// compile error and without a failing test: the window kept
        /// presenting the last inventory as if it were still being measured.
        /// The chain is walked end to end on purpose -- backend phase,
        /// projection, reducer, snapshot, rendered model -- because a test
        /// that stops at the reducer cannot tell a wired-up capability from
        /// a dead one.
        #[test]
        fn a_discovery_daemon_that_cannot_be_maintained_reaches_the_window() {
            let (port, _rx, echo) = port(16);
            let mut state = ready_to_start();
            state.advanced_information = true;

            for effect in reduce(&mut state, AppEvent::RefreshRequested).effects {
                let command = effect_to_command(&effect).expect("a refresh is a device command");
                port.try_send(command).expect("the queue has room");
            }

            let mut backend = DeviceSnapshot::default();
            backend.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 3,
                retry_at: std::time::SystemTime::UNIX_EPOCH,
            };

            let (_next, events) = Projection::default().project(&backend, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            let snapshot = UiSnapshot::from_state(&state);
            assert!(
                matches!(snapshot.discovery, DiscoverySnapshot::Failed { .. }),
                "the window still presents discovery as healthy: {:?}",
                snapshot.discovery
            );

            let catalog = Catalog::new(ResolvedLocale::English);
            let overview = OverviewModel::from_snapshot(&snapshot, catalog);
            let notice = overview
                .notice
                .expect("a search that cannot run is worth telling the user about");
            assert_eq!(notice.message, catalog.text(TextKey::DiscoveryFailedNotice));
            assert_eq!(
                notice.action.map(|action| action.corrective),
                Some(CorrectiveAction::Refresh),
                "the one thing the user can do about it is search again"
            );
            assert!(
                overview
                    .advanced_metrics
                    .iter()
                    .any(|tile| tile.freshness == MetricFreshness::Stale),
                "the receiver count is no longer being measured and must say so"
            );
        }

        /// The other half of the same capability: the warning has to go away
        /// again.
        ///
        /// `DiscoveryPhase::Retrying` is the daemon's ordinary supervised
        /// backoff, not a terminal state, so the shell sees it on transient
        /// stumbles as readily as on a real outage. Reporting it without ever
        /// withdrawing it parks a permanent accusation in a window where
        /// everything works -- and a warning the user has learned to ignore
        /// is worth nothing on the day the daemon really is gone.
        ///
        /// Walked over the bridge rather than in the reducer alone, because
        /// what has to be true is that the *backend's* return to health
        /// clears it, not that some event exists which would.
        #[test]
        fn a_daemon_that_recovers_takes_its_warning_off_the_window() {
            let (port, _rx, echo) = port(16);
            let mut state = ready_to_start();
            state.advanced_information = true;
            let mut projection = Projection::default();

            for effect in reduce(&mut state, AppEvent::RefreshRequested).effects {
                let command = effect_to_command(&effect).expect("a refresh is a device command");
                port.try_send(command).expect("the queue has room");
            }

            let mut stumbling = DeviceSnapshot::default();
            stumbling.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 1,
                retry_at: std::time::SystemTime::UNIX_EPOCH,
            };
            let (next, events) = projection.project(&stumbling, echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            assert!(
                state.notice.is_some(),
                "the stumble has to be reported before its withdrawal means anything"
            );

            let mut browsing = DeviceSnapshot::default();
            browsing.discovery.phase = DiscoveryPhase::Running;
            let (_next, events) = projection.project(&browsing, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            let snapshot = UiSnapshot::from_state(&state);
            let overview =
                OverviewModel::from_snapshot(&snapshot, Catalog::new(ResolvedLocale::English));

            assert_eq!(
                snapshot.discovery,
                DiscoverySnapshot::Ready,
                "the daemon is browsing again"
            );
            assert!(
                overview.notice.is_none(),
                "the daemon is browsing again, yet the window still accuses it: {:?}",
                overview.notice
            );
            assert!(
                overview
                    .advanced_metrics
                    .iter()
                    .all(|tile| tile.freshness != MetricFreshness::Stale),
                "the receiver count is being measured again and must stop saying otherwise"
            );
        }

        /// Drives the window to a live session over one backend snapshot.
        fn started(port: &BackendPort, echo: &GenerationEcho) -> (AppState, Projection) {
            let mut state = ready_to_start();
            let mut projection = Projection::default();

            let (next, events) = projection.project(&DeviceSnapshot::default(), echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }
            for effect in reduce(&mut state, AppEvent::StartRequested).effects {
                let command = effect_to_command(&effect).expect("a start is a device command");
                port.try_send(command).expect("the queue has room");
            }
            let (next, events) = projection.project(&streaming_backend(), echo.sample());
            projection = next;
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            (state, projection)
        }

        /// A session carrying audio to some but not all of its members is the
        /// state the resilience backend exists to survive, and the window
        /// presented it as an ordinary healthy stream.
        #[test]
        fn a_partly_delivered_session_reaches_the_window_as_reduced() {
            let (port, _rx, echo) = port(16);
            let (mut state, projection) = started(&port, &echo);

            let mut degraded = streaming_backend();
            degraded.session.phase = SessionPhase::Degraded { generation: 99 };
            degraded.session.desired =
                BTreeSet::from([ReceiverId::from(rid(1)), ReceiverId::from(rid(2))]);

            let (_next, events) = projection.project(&degraded, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            let snapshot = UiSnapshot::from_state(&state);
            let catalog = Catalog::new(ResolvedLocale::English);
            let overview = OverviewModel::from_snapshot(&snapshot, catalog);

            assert_eq!(
                overview.phase,
                SessionVisualPhase::Degraded,
                "the window claims a healthy stream while a member is missing"
            );
            assert_eq!(overview.title, catalog.text(TextKey::Degraded));
            assert_eq!(
                overview.explanation,
                catalog.text(TextKey::DegradedExplanation)
            );
            assert_eq!(crate::ui::pages::status_key(&snapshot), TextKey::Degraded);
            assert_eq!(
                overview.primary,
                Some(LifecycleAction::Stop),
                "a reduced session is still running and must stay stoppable"
            );
            let tray = crate::tray::TrayModel::from_snapshot(&snapshot);
            assert!(
                tray.tooltip.contains("Reduced"),
                "the tray says the same thing the window says"
            );
            assert_eq!(
                tray.session_action,
                crate::tray::TraySessionAction::Stop { enabled: true },
                "the tray is the only surface reachable from a hidden window, \
                 so it is the last place that may lose the Stop command"
            );
        }

        /// The rejoin the backend performs on its own -- a member reconnecting,
        /// the primary being replaced, the local interface changing -- is a
        /// full-group restart. The window showed an uninterrupted stream
        /// through it, which is why a stall had no explanation on screen.
        #[test]
        fn a_self_healing_restart_reaches_the_window_as_reconnecting() {
            let (port, _rx, echo) = port(16);
            let (mut state, projection) = started(&port, &echo);

            let mut restarting = streaming_backend();
            restarting.session.phase = SessionPhase::Restarting {
                generation: 100,
                reason: RestartReason::SecondaryRejoin,
            };

            let (_next, events) = projection.project(&restarting, echo.sample());
            for event in events {
                reduce(&mut state, AppEvent::Controller(event));
            }

            let snapshot = UiSnapshot::from_state(&state);
            let catalog = Catalog::new(ResolvedLocale::English);
            let overview = OverviewModel::from_snapshot(&snapshot, catalog);

            assert_eq!(
                overview.phase,
                SessionVisualPhase::Restarting,
                "the window shows an unbroken stream while the group is being rebuilt"
            );
            assert_eq!(overview.title, catalog.text(TextKey::Reconnecting));
            assert_eq!(
                overview.explanation,
                catalog.text(TextKey::RestartingExplanation)
            );
            assert_eq!(
                crate::ui::pages::status_key(&snapshot),
                TextKey::Reconnecting
            );
            assert_eq!(
                overview.route.phase,
                RoutePhase::Pending,
                "nothing is carrying audio while the group is torn down"
            );
            assert!(
                snapshot.active_receivers.is_empty(),
                "a rebuilt group has no active members yet, and claiming otherwise \
                 is a fact the window invented"
            );
            // The restart is the backend healing itself; the user never asked
            // for it and must not have to wait it out. A `DisabledStopping`
            // here would paint a dead button over a running session and leave
            // no way to end playback at all.
            assert_eq!(
                overview.primary,
                Some(LifecycleAction::Stop),
                "a rebuild the user did not ask for took away their Stop command"
            );
            let tray = crate::tray::TrayModel::from_snapshot(&snapshot);
            assert!(
                tray.tooltip.contains("Reconnecting"),
                "the tray says the same thing the window says"
            );
            assert_eq!(
                tray.session_action,
                crate::tray::TraySessionAction::Stop { enabled: true },
                "the tray is the only surface reachable from a hidden window, \
                 so it is the last place that may lose the Stop command"
            );
        }
    }

    mod sampling {
        use super::port;
        use crate::app::{ControllerPort, DeviceCommand, GenerationId};

        /// The bridge samples both counters in one place, so a projection
        /// pass cannot stamp its discovery and session events from two
        /// different moments.
        #[test]
        fn sampling_reports_both_shell_generations() {
            let (port, _rx, echo) = port(8);
            port.try_send(DeviceCommand::Discover {
                generation: GenerationId(5),
            })
            .unwrap();
            port.try_send(DeviceCommand::StopSession {
                generation: GenerationId(9),
            })
            .unwrap();

            let seen = echo.sample();

            assert_eq!(seen.discovery, GenerationId(5));
            assert_eq!(seen.session, GenerationId(9));
            assert_eq!(echo.sample(), seen, "sampling does not consume anything");
        }
    }

    /// The bridge thread: the only place the two halves above are joined.
    ///
    /// Every test here runs against a hand-built `watch` + `broadcast` pair --
    /// the exact channel types [`DeviceBackendUpdates`] carries -- so the
    /// thread is proved without a backend, a runtime of the backend's, a
    /// receiver, or a note of audio.
    mod bridge_thread {
        use std::sync::mpsc::Receiver;
        use std::sync::Arc;
        use std::time::Duration;

        use homepod_cast::backend::event::{
            BackendEvent, DiagnosticEvent as BackendDiagnosticEvent, ErrorScope,
            NoticeCode as BackendNoticeCode, Severity,
        };
        use homepod_cast::backend::{DeviceBackendHandle, DeviceBackendUpdates};
        use homepod_cast::{
            DeviceSnapshot, DiagnosticsRegistry, DiscoveryPhase, ReceiverId, ReceiverLifecycle,
            SessionDiagnosticsState, SessionPhase,
        };
        use tokio::sync::{broadcast, watch};

        use super::super::{
            spawn_backend_bridge, BackendPort, BridgeHandle, EndpointDirectory, GenerationEcho,
            DISCOVERY_FAILED_SUMMARY,
        };
        use super::rid;
        use crate::app::{
            AppEvent, ControllerEvent, ControllerPort, DeviceCommand, DiagnosticsHealth,
            DiagnosticsReading, GenerationId,
        };
        use crate::app_handle::AppEventSender;

        /// Deadline, never a delay: every wait below returns as soon as the
        /// bridge has done its work, and this only bounds a hang so a broken
        /// bridge fails the suite instead of stopping it.
        pub(super) const SETTLE: Duration = Duration::from_secs(5);

        /// The sending ends of a hand-built backend feed pair.
        pub(super) struct Feeds {
            pub(super) state: watch::Sender<Arc<DeviceSnapshot>>,
            pub(super) events: broadcast::Sender<BackendEvent>,
            /// The feed the registry the bridge starts consumes. Held so it
            /// stays open, and driven by the test that proves a reading
            /// reaches the shell; closing it must never be what ends the
            /// bridge.
            pub(super) diagnostics: broadcast::Sender<BackendDiagnosticEvent>,
        }

        pub(super) fn feeds(events_capacity: usize) -> (Feeds, DeviceBackendUpdates) {
            let (state, state_rx) = watch::channel(Arc::new(DeviceSnapshot::default()));
            let (events, events_rx) = broadcast::channel(events_capacity);
            let (diagnostics_tx, diagnostics) = DiagnosticsRegistry::test_diagnostic_feed(8);
            let (_session_diagnostics_tx, session_diagnostics) =
                watch::channel(SessionDiagnosticsState::default());
            (
                Feeds {
                    state,
                    events,
                    diagnostics: diagnostics_tx,
                },
                DeviceBackendUpdates {
                    state: state_rx,
                    events: events_rx,
                    diagnostics,
                    session_diagnostics,
                },
            )
        }

        fn start(
            updates: DeviceBackendUpdates,
            echo: &Arc<GenerationEcho>,
            shell_capacity: usize,
        ) -> (BridgeHandle, Receiver<AppEvent>) {
            let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(shell_capacity);
            let handle = spawn_backend_bridge(
                updates,
                Arc::clone(echo),
                Arc::new(EndpointDirectory::default()),
                AppEventSender::new(tx),
            );
            (handle, rx)
        }

        /// The next event the bridge publishes about the *device* half.
        ///
        /// Diagnostics readings are skipped rather than asserted around. The
        /// bridge now joins two independent sources onto one queue, and
        /// `tokio::select!` gives no order between them, so a test about a
        /// snapshot revision that also asserted the position of a registry
        /// reading would be asserting a coin toss. The registry half has its
        /// own test below, which skips device events for the same reason.
        fn next(shell: &Receiver<AppEvent>) -> ControllerEvent {
            let deadline = std::time::Instant::now() + SETTLE;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match shell.recv_timeout(left) {
                    Ok(AppEvent::Controller(ControllerEvent::DiagnosticsUpdated {
                        reading: _,
                    })) => continue,
                    Ok(AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated {
                        ..
                    })) => continue,
                    // The audio triple is republished by the same pass that
                    // answers a session or a discovery edge, and has its own
                    // tests. A test about a session edge that also asserted
                    // the position of a capture reading would be asserting an
                    // interleaving nothing promises.
                    Ok(AppEvent::Controller(
                        ControllerEvent::SavedGroupsChanged { .. }
                        | ControllerEvent::DesiredReceiversChanged { .. }
                        | ControllerEvent::GroupOperationFinished { .. }
                        | ControllerEvent::VolumeApplied { .. }
                        | ControllerEvent::MuteChanged { .. }
                        | ControllerEvent::ReceiverLevelsChanged { .. }
                        | ControllerEvent::LatencyChanged { .. }
                        | ControllerEvent::AudioSourceChanged { .. },
                    )) => continue,
                    Ok(AppEvent::Controller(event)) => return event,
                    Ok(other) => {
                        panic!("the bridge may only publish controller events, got {other:?}")
                    }
                    Err(error) => panic!("the bridge published nothing within {SETTLE:?}: {error}"),
                }
            }
        }

        fn streaming() -> DeviceSnapshot {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.session.phase = SessionPhase::Streaming { generation: 4 };
            snapshot.session.active = std::collections::BTreeSet::from([ReceiverId::from(rid(1))]);
            snapshot
        }

        #[test]
        fn busy_lifecycle_reaches_the_shell_without_another_backend_snapshot() {
            for stopping in [false, true] {
                let echo = Arc::new(GenerationEcho::new());
                let (feeds, updates) = feeds(8);
                let initial = if stopping {
                    streaming()
                } else {
                    DeviceSnapshot::default()
                };
                feeds.state.send(Arc::new(initial)).unwrap();
                // A single event slot also makes the initial audio readings
                // and lifecycle repair share the retained delivery path.
                let (bridge, shell) = start(updates, &echo, 1);
                let first = next(&shell);
                assert!(matches!(
                    first,
                    ControllerEvent::SessionStarted {
                        generation: GenerationId(0),
                        ..
                    } | ControllerEvent::SessionStopped {
                        generation: GenerationId(0)
                    }
                ));
                let (tx, _commands) = tokio::sync::mpsc::channel(3);
                let port = BackendPort::new(
                    DeviceBackendHandle::new(tx),
                    Arc::clone(&echo),
                    Arc::new(EndpointDirectory::default()),
                );
                for _ in 0..3 {
                    port.try_send(DeviceCommand::ApplyMute(false)).unwrap();
                }
                let request = if stopping {
                    DeviceCommand::StopSession {
                        generation: GenerationId(1),
                    }
                } else {
                    DeviceCommand::StartSession {
                        generation: GenerationId(1),
                        receiver_ids: vec![rid(1)],
                        volume: 0.4,
                    }
                };
                assert_eq!(
                    port.try_send(request),
                    Err(crate::app::ControllerSendError::Busy)
                );
                assert_eq!(
                    next(&shell),
                    if stopping {
                        ControllerEvent::SessionStarted {
                            generation: GenerationId(1),
                            active_receiver_ids: vec![rid(1)],
                        }
                    } else {
                        ControllerEvent::SessionStopped {
                            generation: GenerationId(1),
                        }
                    }
                );
                drop(feeds);
                assert!(bridge.wait_until_done(SETTLE));
            }
        }

        pub(super) fn discovered() -> DeviceSnapshot {
            let mut snapshot = DeviceSnapshot::default();
            snapshot.discovery.phase = DiscoveryPhase::Running;
            snapshot.receivers = vec![homepod_cast::backend::model::ReceiverSnapshot {
                id: ReceiverId::from(rid(1)),
                name: "Wohnzimmer".to_owned(),
                model: "AudioAccessory5,1".to_owned(),
                lifecycle: ReceiverLifecycle::Discovered,
            }];
            snapshot
        }

        #[test]
        fn live_diagnostics_repairs_bounded_delivery_without_changing_audio_configuration() {
            let (feeds, updates) = feeds(8);
            let echo = Arc::new(GenerationEcho::new());
            let mut snapshot = discovered();
            snapshot.audio_source.state = homepod_cast::backend::model::AudioSourceState::Capturing;
            snapshot.audio_source.windows_input_peak_permille = Some(250);
            snapshot.audio_source.pcm_frames_dropped_total = 7;
            feeds.state.send(Arc::new(snapshot.clone())).unwrap();
            let (bridge, shell) = start(updates, &echo, 1);
            for (index, peak) in [Some(250), Some(0), None].into_iter().enumerate() {
                if index > 0 {
                    snapshot.audio_source.windows_input_peak_permille = peak;
                    feeds.state.send(Arc::new(snapshot.clone())).unwrap();
                }
                let deadline = std::time::Instant::now() + SETTLE;
                loop {
                    let event = shell
                        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                        .unwrap();
                    match event {
                        AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated {
                            reading,
                        }) if reading.input_peak_per_mille == peak => {
                            assert_eq!(reading.capture_drops_total, 7);
                            assert!(reading.buffer.is_none());
                            break;
                        }
                        AppEvent::Controller(ControllerEvent::AudioSourceChanged { .. })
                            if index > 0 =>
                        {
                            panic!("a meter update must not mutate the hearing-test source binding")
                        }
                        _ => {}
                    }
                }
            }
            drop(feeds);
            drop(shell);
            assert!(bridge.wait_until_done(SETTLE));
        }

        /// The reading the shell shows comes from a registry the bridge
        /// starts, feeds, and shuts down.
        ///
        /// Everything here is real except the backend: the registry is the
        /// shipping one, the feed is the exact channel type the backend
        /// hands over, and the loss the test plants is the same typed
        /// `QueueLag` event a producer that outran the feed would produce.
        /// What is asserted is the whole chain -- backend diagnostic feed,
        /// registry, `diagnostics_reading`, bridge, shell queue -- because
        /// each half of it was already provable on its own and neither half
        /// proves the app shows anything.
        #[test]
        fn a_registry_reading_reaches_the_shell_and_the_backend_feed_moves_it() {
            let echo = Arc::new(GenerationEcho::new());
            let (feeds, updates) = feeds(8);
            let (handle, shell) = start(updates, &echo, 256);

            let first = next_reading(&shell);
            assert_eq!(
                first.health,
                DiagnosticsHealth::Unknown,
                "no session is registered, so the registry cannot judge"
            );
            assert_eq!(first.events_dropped_total, 0);
            assert_eq!(first.measured_receivers, 0);

            feeds
                .diagnostics
                .send(BackendDiagnosticEvent::queue_lag(3))
                .expect("the registry is subscribed to the diagnostics feed");

            let after = next_changed_reading(&shell, first);
            assert_eq!(
                after,
                DiagnosticsReading {
                    health: DiagnosticsHealth::Unknown,
                    events_dropped_total: 3,
                    measured_receivers: 0,
                },
                "a producer that outran the feed is a measurement, and the only \
                 one this page can make before a session registers"
            );

            drop(feeds);
            assert!(
                handle.wait_until_done(SETTLE),
                "the bridge has to finish once both feeds close, registry and all"
            );
        }

        /// The next diagnostics reading the shell is offered.
        ///
        /// The bridge publishes device events on the same queue, so this
        /// skips whatever else is in front of it rather than asserting an
        /// order the `select!` does not promise.
        fn next_reading(shell: &Receiver<AppEvent>) -> DiagnosticsReading {
            let deadline = std::time::Instant::now() + SETTLE;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match shell.recv_timeout(left) {
                    Ok(AppEvent::Controller(ControllerEvent::DiagnosticsUpdated { reading })) => {
                        return reading
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        panic!(
                            "no diagnostics reading reached the shell within {SETTLE:?}: {error}"
                        )
                    }
                }
            }
        }

        /// The next reading that says something different from `previous`.
        fn next_changed_reading(
            shell: &Receiver<AppEvent>,
            previous: DiagnosticsReading,
        ) -> DiagnosticsReading {
            loop {
                let reading = next_reading(shell);
                if reading != previous {
                    return reading;
                }
            }
        }

        /// A notice whose free-form prose carries exactly the kind of material
        /// the boundary exists to keep off the screen.
        fn leaky_notice() -> BackendEvent {
            BackendEvent::Notice {
                severity: Severity::Warning,
                code: BackendNoticeCode::DiscoveryFailed,
                scope: ErrorScope::Discovery,
                message: "network change monitoring is unavailable: 192.168.178.42:7000".to_owned(),
            }
        }

        /// Nothing has to happen on either feed for the window to learn where
        /// the backend stands: the bridge projects once before it ever parks.
        #[test]
        fn the_first_pass_reaches_the_shell_before_any_feed_moves() {
            let echo = Arc::new(GenerationEcho::new());
            let (_feeds, updates) = feeds(8);
            let (_handle, shell) = start(updates, &echo, 64);

            assert_eq!(
                next(&shell),
                ControllerEvent::SessionStopped {
                    generation: GenerationId(0)
                }
            );
        }

        #[test]
        fn a_new_snapshot_revision_reaches_the_shell() {
            let echo = Arc::new(GenerationEcho::new());
            let (feeds, updates) = feeds(8);
            let (_handle, shell) = start(updates, &echo, 64);
            let _ = next(&shell);

            feeds
                .state
                .send(Arc::new(streaming()))
                .expect("the bridge holds the receiving end");

            assert_eq!(
                next(&shell),
                ControllerEvent::SessionStarted {
                    generation: GenerationId(0),
                    active_receiver_ids: vec![rid(1)],
                }
            );
        }

        /// A refresh moves no backend state at all, so only the shell-side
        /// wakeup can carry it. Without it the window would spin until the
        /// discovery daemon happened to publish a new revision.
        #[test]
        fn a_refresh_is_answered_from_the_current_inventory_without_a_new_snapshot() {
            let echo = Arc::new(GenerationEcho::new());
            let (feeds, updates) = feeds(8);
            feeds
                .state
                .send(Arc::new(discovered()))
                .expect("the receiver is still in the updates bundle");
            let (_handle, shell) = start(updates, &echo, 64);
            let (command_tx, _command_rx) = tokio::sync::mpsc::channel(8);
            let port = BackendPort::new(
                DeviceBackendHandle::new(command_tx),
                Arc::clone(&echo),
                Arc::new(EndpointDirectory::default()),
            );

            // The first pass answers under generation 0.
            assert!(matches!(
                next(&shell),
                ControllerEvent::DiscoveryCompleted {
                    generation: GenerationId(0),
                    ..
                }
            ));
            assert_eq!(
                next(&shell),
                ControllerEvent::SessionStopped {
                    generation: GenerationId(0)
                }
            );

            port.try_send(DeviceCommand::Discover {
                generation: GenerationId(7),
            })
            .expect("the port accepts a refresh");

            match next(&shell) {
                ControllerEvent::DiscoveryCompleted {
                    generation,
                    receivers,
                } => {
                    assert_eq!(generation, GenerationId(7));
                    assert_eq!(receivers.len(), 1);
                }
                other => panic!("a refresh must be answered with an inventory, got {other:?}"),
            }
        }

        #[test]
        fn a_backend_notice_reaches_the_shell_without_its_prose() {
            let echo = Arc::new(GenerationEcho::new());
            let (feeds, updates) = feeds(8);
            let (_handle, shell) = start(updates, &echo, 64);
            let _ = next(&shell);

            feeds
                .events
                .send(leaky_notice())
                .expect("the bridge is subscribed");

            assert_eq!(
                next(&shell),
                ControllerEvent::DiscoveryFailed {
                    generation: GenerationId(0),
                    summary: DISCOVERY_FAILED_SUMMARY.to_owned(),
                }
            );
        }

        /// The event feed is bounded and lossy by construction. Losing entries
        /// must cost the entries, not the bridge: a `Lagged` treated as a
        /// closed feed would silently stop translating notices for the rest of
        /// the run, with the window none the wiser.
        #[test]
        fn a_lagged_event_feed_neither_ends_the_bridge_nor_stops_the_snapshots() {
            let echo = Arc::new(GenerationEcho::new());
            // Capacity one, three sends before the bridge ever polls: the very
            // first `recv` it issues can only return `Lagged`.
            let (feeds, updates) = feeds(1);
            for _ in 0..3 {
                feeds
                    .events
                    .send(leaky_notice())
                    .expect("a receiver exists");
            }
            let (handle, shell) = start(updates, &echo, 64);
            let _ = next(&shell);

            assert_eq!(
                next(&shell),
                ControllerEvent::DiscoveryFailed {
                    generation: GenerationId(0),
                    summary: DISCOVERY_FAILED_SUMMARY.to_owned(),
                },
                "the surviving notice must still be translated"
            );

            feeds
                .state
                .send(Arc::new(streaming()))
                .expect("the bridge holds the receiving end");
            assert_eq!(
                next(&shell),
                ControllerEvent::SessionStarted {
                    generation: GenerationId(0),
                    active_receiver_ids: vec![rid(1)],
                }
            );
            assert!(!handle.is_done(), "a lagged feed is not a closed feed");
        }

        /// The rule itself, over all four combinations.
        ///
        /// Deliberately *not* asserted by closing one feed on a running
        /// bridge and reading `is_done` afterwards. `tokio::select!` chooses
        /// at random among ready branches, so a bridge that stopped on one
        /// closed feed would pass such a test whenever the other branch
        /// happened to win the race -- which is how a shutdown contract
        /// quietly stops holding. The decision is separated from the schedule
        /// so it can be checked exhaustively instead of statistically.
        #[test]
        fn only_both_feeds_closing_finishes_the_bridge() {
            assert!(!super::super::finished(false, false));
            assert!(
                !super::super::finished(true, false),
                "a live event feed still has notices to translate"
            );
            assert!(
                !super::super::finished(false, true),
                "a live snapshot feed still has revisions to project"
            );
            assert!(super::super::finished(true, true));
        }

        #[test]
        fn closed_session_diagnostics_feed_tombstones_its_last_generation() {
            let active = SessionDiagnosticsState::Active {
                generation: 17,
                started_elapsed_ns: 1,
                primary: None,
                members: std::collections::BTreeMap::new(),
                source: airplay_client::ClientDiagnosticsSource::test_new_empty(),
            };

            assert!(matches!(
                super::super::failed_session_state(&active),
                SessionDiagnosticsState::Inactive {
                    generation: 17,
                    reason: homepod_cast::SessionStopReason::Failed,
                }
            ));
        }

        /// `done` is what the shell's exit path reads to learn that the
        /// backend control thread has returned. It must not be set on a
        /// backend that is still publishing, and it must be set once both
        /// senders are gone.
        #[test]
        fn done_is_reported_only_after_the_backend_is_gone() {
            let echo = Arc::new(GenerationEcho::new());
            let (feeds, updates) = feeds(8);
            let Feeds {
                state,
                events,
                diagnostics: _diagnostics,
            } = feeds;
            let (handle, shell) = start(updates, &echo, 64);
            let _ = next(&shell);

            assert!(
                !handle.is_done(),
                "both feeds are open; the backend has not gone anywhere"
            );

            drop(state);
            drop(events);
            assert!(
                handle.wait_until_done(SETTLE),
                "both feeds closed but the bridge never reported done"
            );
        }

        /// The shell's event queue is bounded, and a refusal is not a licence
        /// to leave the window permanently out of step with the backend.
        ///
        /// Asserted against `flush` directly rather than through the thread:
        /// forcing a refusal from a test means winning a race against the
        /// test's own reader, and a bridge that dropped refused events would
        /// pass a thread-level version whenever the reader happened to be
        /// quick. Here the queue is full by construction.
        #[test]
        fn a_refused_event_is_retained_and_delivered_on_the_next_attempt() {
            let (tx, shell) = std::sync::mpsc::sync_channel::<AppEvent>(1);
            let out = AppEventSender::new(tx);
            let first = ControllerEvent::SessionStopped {
                generation: GenerationId(1),
            };
            let second = ControllerEvent::SessionStopped {
                generation: GenerationId(2),
            };
            let mut outbox = std::collections::VecDeque::from([first.clone(), second.clone()]);
            let mut shell_gone = false;

            super::super::flush(&out, &mut outbox, &mut shell_gone);
            assert_eq!(outbox.len(), 1, "the refused event must be kept");
            assert!(!shell_gone, "a full queue is not a lost shell");
            assert_eq!(shell.recv(), Ok(AppEvent::Controller(first)));

            super::super::flush(&out, &mut outbox, &mut shell_gone);
            assert!(outbox.is_empty());
            assert_eq!(shell.recv(), Ok(AppEvent::Controller(second)));
        }

        /// A retained event has to be offered again by something, and nothing
        /// on either backend feed has to happen after a refusal.
        #[test]
        fn a_retained_event_schedules_a_retry_and_an_empty_outbox_does_not() {
            assert_eq!(
                super::super::repair_delay(0),
                None,
                "an idle bridge must park on its feeds, not on a timer"
            );
            assert!(super::super::repair_delay(1).is_some());
            assert!(super::super::repair_delay(2).is_some());
        }

        /// A shell that is gone is not a shell that is behind: keeping the
        /// event would retry it against a closed queue for the rest of the run.
        #[test]
        fn a_lost_shell_discards_the_outbox_instead_of_retrying_it() {
            let (tx, shell) = std::sync::mpsc::sync_channel::<AppEvent>(1);
            drop(shell);
            let out = AppEventSender::new(tx);
            let mut outbox = std::collections::VecDeque::from([ControllerEvent::SessionStopped {
                generation: GenerationId(1),
            }]);
            let mut shell_gone = false;

            super::super::flush(&out, &mut outbox, &mut shell_gone);

            assert!(shell_gone);
            assert!(outbox.is_empty());
        }
    }

    /// The switch: the one place the running app changes.
    ///
    /// Everything here is asserted against [`device_seam_from`], the wiring
    /// half of the switch, over hand-built channels. [`backend_device_seam`]
    /// adds exactly one thing on top -- it starts a real backend -- and that
    /// is the part a test must not run: it opens sockets, browses the network,
    /// and takes the audio endpoint.
    mod switch {
        use std::sync::Arc;
        use std::time::Duration;

        use homepod_cast::backend::controller::SHUTDOWN_GROUP_BUDGET;
        use homepod_cast::backend::{BackendCommand, CommandEnvelope, DeviceBackendHandle};
        use homepod_cast::{DeviceSnapshot, DiscoveryPhase, RunIntent};
        use tokio::sync::mpsc;

        use super::super::{device_seam_from, DEVICE_SHUTDOWN_BUDGET};
        use super::bridge_thread::{discovered, feeds, Feeds, SETTLE};
        use super::rid;
        use crate::app::{
            ControllerEvent, DeviceCommand, DeviceSeam, GenerationId, ShutdownBudgets,
        };
        use crate::app_handle::AppEventSender;

        /// Everything the switch produces, over channels a test owns.
        struct Wired {
            seam: DeviceSeam,
            feeds: Feeds,
            commands: mpsc::Receiver<CommandEnvelope>,
            shell: std::sync::mpsc::Receiver<crate::app::AppEvent>,
        }

        fn wire(initial: DeviceSnapshot) -> Wired {
            let (feeds, updates) = feeds(8);
            feeds
                .state
                .send(Arc::new(initial))
                .expect("the receiver is still in the updates bundle");
            let (command_tx, commands) = mpsc::channel(16);
            let (shell_tx, shell) = std::sync::mpsc::sync_channel(64);
            let seam = device_seam_from(
                DeviceBackendHandle::new(command_tx),
                updates,
                AppEventSender::new(shell_tx),
            );
            Wired {
                seam,
                feeds,
                commands,
                shell,
            }
        }

        fn drain(commands: &mut mpsc::Receiver<CommandEnvelope>) -> Vec<BackendCommand> {
            let mut sent = Vec::new();
            while let Ok(envelope) = commands.try_recv() {
                sent.push(envelope.command);
            }
            sent
        }

        /// The next event the seam publishes about the *device* half; see
        /// `bridge_thread::next` for why the registry half is skipped rather
        /// than ordered against.
        fn next(shell: &std::sync::mpsc::Receiver<crate::app::AppEvent>) -> ControllerEvent {
            let deadline = std::time::Instant::now() + SETTLE;
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match shell.recv_timeout(left) {
                    Ok(crate::app::AppEvent::Controller(ControllerEvent::DiagnosticsUpdated {
                        reading: _,
                    })) => continue,
                    Ok(crate::app::AppEvent::Controller(
                        ControllerEvent::LiveDiagnosticsUpdated { .. },
                    )) => continue,
                    // See `bridge_thread::next`.
                    Ok(crate::app::AppEvent::Controller(
                        ControllerEvent::SavedGroupsChanged { .. }
                        | ControllerEvent::DesiredReceiversChanged { .. }
                        | ControllerEvent::GroupOperationFinished { .. }
                        | ControllerEvent::VolumeApplied { .. }
                        | ControllerEvent::MuteChanged { .. }
                        | ControllerEvent::ReceiverLevelsChanged { .. }
                        | ControllerEvent::LatencyChanged { .. }
                        | ControllerEvent::AudioSourceChanged { .. },
                    )) => continue,
                    Ok(crate::app::AppEvent::Controller(event)) => return event,
                    Ok(other) => {
                        panic!("the seam may only publish controller events, got {other:?}")
                    }
                    Err(error) => panic!("nothing reached the window within {SETTLE:?}: {error}"),
                }
            }
        }

        /// The first of the two traps handed over with the design.
        ///
        /// The coordinator's device stage was three seconds while the backend
        /// gives itself five for its own teardown. Left alone, every exit with
        /// one wedged receiver was guaranteed to report a timeout and walk on
        /// while the backend was still disconnecting.
        #[test]
        fn the_device_budget_covers_the_backends_own_teardown() {
            assert!(
                DEVICE_SHUTDOWN_BUDGET >= SHUTDOWN_GROUP_BUDGET,
                "the shell allows the device stage {DEVICE_SHUTDOWN_BUDGET:?} but the backend \
                 gives itself {SHUTDOWN_GROUP_BUDGET:?} to tear down"
            );
            assert!(
                DEVICE_SHUTDOWN_BUDGET > ShutdownBudgets::default().device,
                "the legacy service's budget was smaller; the switch has to raise it"
            );
        }

        /// The valve is derived from the budgets in force, so a longer device
        /// stage must move it rather than be cut short by it.
        #[test]
        fn the_longer_device_budget_carries_the_exit_valve_with_it() {
            let legacy = ShutdownBudgets::default();
            let switched = ShutdownBudgets {
                device: DEVICE_SHUTDOWN_BUDGET,
                ..legacy
            };

            assert!(switched.exit_valve() > legacy.exit_valve());
            assert!(
                switched.exit_valve() > switched.worst_case(),
                "the valve would fire inside the teardown it guards"
            );
        }

        #[test]
        fn the_switch_hands_that_budget_to_the_coordinator() {
            assert_eq!(
                wire(DeviceSnapshot::default()).seam.budget,
                DEVICE_SHUTDOWN_BUDGET
            );
        }

        /// Capability: starting a session.
        #[test]
        fn a_start_press_becomes_membership_then_volume_then_run_intent() {
            let mut wired = wire(DeviceSnapshot::default());

            wired
                .seam
                .controller
                .try_send(DeviceCommand::StartSession {
                    generation: GenerationId(1),
                    receiver_ids: vec![rid(1)],
                    volume: 0.25,
                })
                .expect("the queue has room");

            let sent = drain(&mut wired.commands);
            assert!(
                matches!(
                    sent.as_slice(),
                    [
                        BackendCommand::SetDesiredMembers { .. },
                        BackendCommand::SetMasterVolume(_),
                        BackendCommand::SetRunIntent(RunIntent::Running),
                    ]
                ),
                "a start must reach the backend as membership, volume, run intent, got {sent:?}"
            );
        }

        /// Capability: stopping a session.
        #[test]
        fn a_stop_press_becomes_a_stopped_run_intent() {
            let mut wired = wire(DeviceSnapshot::default());

            wired
                .seam
                .controller
                .try_send(DeviceCommand::StopSession {
                    generation: GenerationId(2),
                })
                .expect("the queue has room");

            assert!(matches!(
                drain(&mut wired.commands).as_slice(),
                [BackendCommand::SetRunIntent(RunIntent::Stopped)]
            ));
        }

        /// Capability: master volume, outside a session as well as inside one.
        #[test]
        fn a_volume_change_becomes_a_master_volume() {
            let mut wired = wire(DeviceSnapshot::default());

            wired
                .seam
                .controller
                .try_send(DeviceCommand::ApplyVolume {
                    generation: GenerationId(3),
                    volume: 0.75,
                })
                .expect("the queue has room");

            match drain(&mut wired.commands).as_slice() {
                [BackendCommand::SetMasterVolume(volume)] => {
                    assert!((volume.get() - 0.75).abs() < f32::EPSILON);
                }
                other => panic!("a volume change must reach the backend, got {other:?}"),
            }
        }

        /// Capability: discovery. The whole way round -- shell command in,
        /// window event out -- with no backend behind it.
        #[test]
        fn a_refresh_is_answered_from_the_backends_inventory() {
            let wired = wire(discovered());

            // The first pass answers under generation 0.
            assert!(matches!(
                next(&wired.shell),
                ControllerEvent::DiscoveryCompleted { .. }
            ));
            assert!(matches!(
                next(&wired.shell),
                ControllerEvent::SessionStopped { .. }
            ));

            wired
                .seam
                .controller
                .try_send(DeviceCommand::Discover {
                    generation: GenerationId(9),
                })
                .expect("a refresh is accepted while the backend lives");

            match next(&wired.shell) {
                ControllerEvent::DiscoveryCompleted {
                    generation,
                    receivers,
                } => {
                    assert_eq!(generation, GenerationId(9));
                    assert_eq!(receivers.len(), 1);
                }
                other => panic!("a refresh must be answered with an inventory, got {other:?}"),
            }
        }

        /// Capability: discovery *with retry*. The legacy service reported a
        /// one-shot browse failure; the backend keeps a supervised daemon and
        /// backs off, and that state has to leave the seam as the failure it
        /// is rather than as an empty room.
        ///
        /// The seam is as far as this goes, and the name says so. What the
        /// reducer then makes of the event is its own test's business; this
        /// one owns the claim that the failure leaves the seam as a failure,
        /// and asserting "reaches the window" here would put two layers'
        /// behaviour behind one name.
        #[test]
        fn a_retrying_discovery_daemon_leaves_the_seam_as_a_failure() {
            let wired = wire(discovered());
            let _ = next(&wired.shell);
            let _ = next(&wired.shell);

            let mut retrying = discovered();
            retrying.discovery.phase = DiscoveryPhase::Retrying {
                attempt: 3,
                retry_at: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(30),
            };
            wired
                .feeds
                .state
                .send(Arc::new(retrying))
                .expect("the bridge holds the receiving end");

            assert!(matches!(
                next(&wired.shell),
                ControllerEvent::DiscoveryFailed { .. }
            ));
        }

        /// The device stage of the bounded exit: ask the backend to stop, then
        /// wait for evidence that it actually did.
        #[test]
        fn the_device_stage_asks_the_backend_to_stop_and_waits_for_evidence() {
            let mut wired = wire(DeviceSnapshot::default());
            let Feeds {
                state,
                events,
                diagnostics: _diagnostics,
            } = wired.feeds;

            // Both feeds still open: the backend has not finished.
            drop(state);
            drop(events);

            assert!(
                wired.seam.shutdown.shutdown_and_wait(SETTLE),
                "the backend's feeds closed but the device stage reported a timeout"
            );
            assert!(
                matches!(
                    drain(&mut wired.commands).as_slice(),
                    [BackendCommand::Shutdown]
                ),
                "the device stage must actually ask the backend to shut down"
            );
        }
    }

    /// Where the backend looks for the volume the user last set.
    ///
    /// The only decision in `backend_device_seam` that can be wrong without
    /// anything failing: a source pointing at a file the installation never
    /// had migrates nothing, hands the user the default volume, and the first
    /// backend write to `state-v1.json` makes that permanent.
    mod volume_migration {
        use super::super::{backend_config_for, legacy_volume_source};

        fn historical(root: &std::path::Path) -> std::path::PathBuf {
            root.join("HomePodCast").join("volume.txt")
        }

        fn write(path: &std::path::Path, contents: &str) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the temporary root is writable");
            }
            std::fs::write(path, contents).expect("the temporary root is writable");
        }

        /// Builds up to `33521c2` wrote this file and nothing has moved it, so
        /// a user who last ran one of those must still be found.
        #[test]
        fn a_volume_left_by_the_original_layout_is_found() {
            let dir = tempfile::TempDir::new().unwrap();
            write(&historical(dir.path()), "0.400");

            assert_eq!(
                legacy_volume_source(dir.path()),
                historical(dir.path()),
                "the only volume file present was ignored"
            );
        }

        /// The refactored legacy service moved the file up one directory.
        #[test]
        fn a_volume_left_by_the_legacy_service_is_found() {
            let dir = tempfile::TempDir::new().unwrap();
            let current = crate::device_service::legacy_volume_file(dir.path());
            write(&current, "0.600");

            assert_eq!(
                legacy_volume_source(dir.path()),
                current,
                "the only volume file present was ignored"
            );
        }

        /// Both on disk means the user ran both builds; the later writer holds
        /// the volume they actually last set.
        #[test]
        fn the_newer_writers_file_wins_when_both_exist() {
            let dir = tempfile::TempDir::new().unwrap();
            let current = crate::device_service::legacy_volume_file(dir.path());
            write(&historical(dir.path()), "0.400");
            write(&current, "0.600");

            assert_eq!(legacy_volume_source(dir.path()), current);
        }

        /// A fresh profile has neither, and that must resolve rather than
        /// panic or point outside the profile.
        #[test]
        fn a_profile_with_neither_file_resolves_inside_it() {
            let dir = tempfile::TempDir::new().unwrap();

            assert_eq!(
                legacy_volume_source(dir.path()),
                crate::device_service::legacy_volume_file(dir.path())
            );
        }

        /// The rest of the layout, so a change to it is visible here too.
        #[test]
        fn the_backend_state_directory_sits_under_the_validated_profile() {
            let dir = tempfile::TempDir::new().unwrap();
            let config = backend_config_for(dir.path());

            assert_eq!(config.state_directory, dir.path().join("OpenAirCast"));
            assert_eq!(config.legacy_volume_path, legacy_volume_source(dir.path()));
        }
    }
}
