use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use homepod_cast::backend::model::{AudioEndpointPreference, LatencyPreset};
use homepod_cast::backend::{BackendCommand, BackendSendError, DeviceBackendHandle};
use homepod_cast::{DeviceSnapshot, ReceiverId, RunIntent, Volume};
use tokio::sync::Notify;

use crate::app::{
    AudioEndpointRequest, ControllerEvent, ControllerPort, ControllerSendError, DeviceCommand,
    GenerationId, LatencyChoice,
};

use super::projection::{EndpointDirectory, SessionEcho};

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
    pub(super) fn answered_by(self, echo: &SessionEcho) -> bool {
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
    pub(super) fn observe_listening_target(
        &self,
        snapshot: &DeviceSnapshot,
        events: &[ControllerEvent],
    ) {
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

    pub(super) fn listening_events(
        &self,
        snapshot: &DeviceSnapshot,
        closed: bool,
    ) -> Vec<ControllerEvent> {
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

    pub(super) fn volume_events(
        &self,
        snapshot: &DeviceSnapshot,
        closed: bool,
    ) -> Vec<ControllerEvent> {
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
    pub(super) fn group_events(
        &self,
        snapshot: &DeviceSnapshot,
        closed: bool,
    ) -> Vec<ControllerEvent> {
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
    pub(super) fn record_session(&self, generation: GenerationId, request: SessionEdge) {
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
    pub(super) handle: DeviceBackendHandle,
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

    pub(super) fn send(&self, command: BackendCommand) -> Result<(), ControllerSendError> {
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
