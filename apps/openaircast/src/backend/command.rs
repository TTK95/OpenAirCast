//! Bounded command transport between the shell effect executor and the device
//! backend actor.
//!
//! Producers never block indefinitely: [`DeviceBackendHandle::try_send`]
//! rejects with a typed error when the fixed-capacity queue is full, and
//! [`DeviceBackendHandle::send`] awaits capacity for non-UI callers. Command
//! IDs are allocated by the handle before enqueueing so completion and failure
//! events can echo them even under contention.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::backend::model::{
    AudioEndpointPreference, LatencyPreset, ReceiverId, RunIntent, SavedGroupId, SavedGroupMember,
    Volume,
};
use crate::calibration::CalibrationCommand;

/// Fixed capacity of the shell-to-actor command queue.
pub const COMMAND_CAPACITY: usize = 64;

/// A shell-issued command paired with its handle-allocated identity.
#[derive(Debug)]
pub struct CommandEnvelope {
    /// Monotonically increasing allocation order; echoed in completion events.
    pub id: u64,
    /// The command itself.
    pub command: BackendCommand,
    /// Optional exact receipt; dropping unexecuted/coalesced work closes it.
    pub confirmation: Option<oneshot::Sender<CommandConfirmation>>,
}

/// Exact terminal result, readable only after the accompanying state is projected.
#[derive(Clone, Copy, Debug)]
pub struct CommandConfirmation {
    /// First snapshot revision guaranteed to include this operation's state.
    pub revision: u64,
    /// None means the command succeeded, not that playback has started.
    pub error: Option<crate::backend::model::CommandFailure>,
    /// Earliest backend session attempt that may answer a successful group start.
    /// Assigned after batch reconciliation; durable success alone is not session evidence.
    pub session_generation_floor: Option<u64>,
}

/// Bounded, one-result receipt independent of allocation order and broadcast lag.
#[derive(Debug)]
pub struct CommandReceipt {
    receiver: oneshot::Receiver<CommandConfirmation>,
    confirmed: Option<CommandConfirmation>,
}

/// Current state of an exact receipt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReceiptState {
    /// Still executing, or its state revision has not yet been observed.
    Pending,
    /// Definitive completion after state publication.
    Completed(Option<crate::backend::model::CommandFailure>),
    /// The producer disappeared without a result; execution is uncertain.
    Lost,
}

impl CommandReceipt {
    /// Causal session floor, available only once this receipt's state is observable.
    pub fn session_generation_floor(&self, projected_revision: u64) -> Option<u64> {
        self.confirmed
            .filter(|confirmation| confirmation.revision <= projected_revision)
            .and_then(|confirmation| confirmation.session_generation_floor)
    }

    /// Polls without blocking and retains the result across repeated polls.
    pub fn poll(&mut self, projected_revision: u64) -> ReceiptState {
        if self.confirmed.is_none() {
            match self.receiver.try_recv() {
                Ok(confirmation) => self.confirmed = Some(confirmation),
                Err(oneshot::error::TryRecvError::Empty) => return ReceiptState::Pending,
                Err(oneshot::error::TryRecvError::Closed) => return ReceiptState::Lost,
            }
        }
        let confirmation = self.confirmed.expect("confirmation was just observed");
        if projected_revision < confirmation.revision {
            ReceiptState::Pending
        } else {
            ReceiptState::Completed(confirmation.error)
        }
    }
}

/// Every operation the shell may request from the device backend actor.
///
/// Deliberately transport-free: no connection handles, addresses, or protocol
/// values appear here. Membership is always a complete replacement set.
#[derive(Clone, Debug)]
pub enum BackendCommand {
    /// One finite test tone, bound to the exact already-connected configuration.
    RunListeningTest(super::listening_check::ListeningTestGuard),
    /// Explicit selection apply, retained in arrival order with an outcome.
    /// Unlike slider-style desired updates this command cannot be coalesced away.
    ApplyDesiredMembers {
        /// Complete membership, including offline receivers.
        members: BTreeSet<ReceiverId>,
    },
    /// Replaces the committed desired membership as a complete set.
    SetDesiredMembers {
        /// The new desired receiver set.
        members: BTreeSet<ReceiverId>,
    },
    /// Sets whether audio should run; independent of desired membership.
    SetRunIntent(RunIntent),
    /// Sets the validated master volume.
    SetMasterVolume(Volume),
    /// Mutes or unmutes while preserving configured volumes.
    SetMuted(bool),
    /// Sets one receiver's balance within the group.
    SetReceiverLevel {
        /// Target receiver.
        receiver: ReceiverId,
        /// Validated per-receiver level.
        level: Volume,
    },
    /// Selects the Windows render endpoint preference for capture.
    SetAudioEndpoint(AudioEndpointPreference),
    /// Applies an enabled latency preset.
    SetLatencyPreset(LatencyPreset),
    /// Enables or disables auto-connect after initial discovery stability.
    SetAutoConnect(bool),
    /// Creates or updates a saved group in one durable step.
    SaveGroup {
        /// Existing group ID when editing, or none to allocate a fresh one.
        id: Option<SavedGroupId>,
        /// Trimmed display name, unique case-insensitively.
        name: String,
        /// Complete member list with intended levels.
        members: Vec<SavedGroupMember>,
    },
    /// Deletes one saved group; desired membership stays untouched.
    DeleteGroup(SavedGroupId),
    /// Atomically loads a saved group as the desired set, optionally running.
    ActivateSavedGroup {
        /// Group to activate.
        id: SavedGroupId,
        /// Whether to also set [`RunIntent::Running`].
        start: bool,
    },
    /// Clears backoff state for one receiver and retries immediately.
    RetryReceiver(ReceiverId),
    /// Reports a platform edge discovered outside the backend.
    NotifySystem(SystemEvent),
    /// Applies, resets, or exercises the manual presentation calibration.
    ///
    /// An edge, never a coalesced scalar: each apply is guarded by the
    /// desired revision the caller decided against, and each accepted one
    /// costs exactly one controlled full-group restart. Merging two applies
    /// would silently discard the guard of the one that lost.
    Calibration(CalibrationCommand),
    /// Requests the bounded full teardown of the backend.
    Shutdown,
}

/// Platform edges reported through [`BackendCommand::NotifySystem`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemEvent {
    /// Windows is about to suspend.
    Suspending,
    /// Windows resumed from suspend.
    Resumed,
    /// One coalesced local network-interface change was observed.
    NetworkChanged {
        /// Whether an active local binding/address actually moved.
        ///
        /// The monitor compares successive address tables and publishes only
        /// this verdict; no address ever crosses this boundary.
        local_binding_changed: bool,
    },
}

impl BackendCommand {
    /// Whether this command marks a lifecycle edge the shell should observe
    /// even across event lag, as opposed to continuously coalesced settings.
    #[allow(dead_code)] // consumed by controller event classification in Task 9.
    pub(crate) fn is_edge(&self) -> bool {
        matches!(
            self,
            Self::ApplyDesiredMembers { .. }
                | Self::SaveGroup { .. }
                | Self::DeleteGroup(_)
                | Self::ActivateSavedGroup { .. }
                | Self::RetryReceiver(_)
                | Self::NotifySystem(_)
                | Self::Calibration(_)
                | Self::RunListeningTest(_)
                | Self::Shutdown
        )
    }
}

/// Typed failure of a non-blocking or awaiting command send.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BackendSendError {
    /// The fixed-capacity command queue is currently full.
    #[error("backend queue is busy")]
    Busy,
    /// The backend actor is gone and can no longer receive commands.
    #[error("backend is closed")]
    Closed,
}

/// Cloneable producer side of the bounded command queue.
///
/// IDs are allocated atomically before enqueueing, so a rejected send may skip
/// an ID but never reuses or reorders allocations that were accepted. Every
/// clone shares one optional lifecycle guard; dropping the last guard joins
/// the backend control thread.
#[derive(Clone, Debug)]
pub struct DeviceBackendHandle {
    command_tx: mpsc::Sender<CommandEnvelope>,
    next_id: Arc<AtomicU64>,
    // Owned purely for its Drop side effect: the last handle clone joins the
    // backend control thread.
    #[allow(dead_code)]
    lifecycle: Option<LifecycleGuard>,
}

/// Private guard owning the backend control thread's join handle.
///
/// Shared by every [`DeviceBackendHandle`] clone; when the final clone is
/// dropped the already-finished control thread is joined so no worker leaks.
#[derive(Clone, Debug, Default)]
pub(crate) struct LifecycleGuard(Arc<LifecycleInner>);

#[derive(Debug, Default)]
struct LifecycleInner {
    join: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LifecycleGuard {
    /// Stores the control thread's join handle exactly once.
    pub(crate) fn set_join(&self, join: std::thread::JoinHandle<()>) {
        *self.0.join.lock().expect("lifecycle join mutex poisoned") = Some(join);
    }
}

impl Drop for LifecycleInner {
    fn drop(&mut self) {
        if let Some(join) = self
            .join
            .lock()
            .expect("lifecycle join mutex poisoned")
            .take()
        {
            let _ = join.join();
        }
    }
}

impl DeviceBackendHandle {
    /// Wraps an existing queue sender; the device actor constructs handles.
    ///
    /// Public so a shell can build a handle over its own bounded queue and
    /// assert the real `Busy`/`Closed` behaviour without starting a backend
    /// thread. Such a handle carries no lifecycle guard and therefore joins
    /// no control thread on drop.
    pub fn new(command_tx: mpsc::Sender<CommandEnvelope>) -> Self {
        Self {
            command_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            lifecycle: None,
        }
    }

    /// Wraps a queue sender plus the shared lifecycle guard of its actor.
    pub(crate) fn with_lifecycle(
        command_tx: mpsc::Sender<CommandEnvelope>,
        lifecycle: LifecycleGuard,
    ) -> Self {
        Self {
            command_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            lifecycle: Some(lifecycle),
        }
    }

    /// Whether the actor is gone and no further command can be accepted.
    ///
    /// A shell command that translates into no backend command at all -- a
    /// discovery refresh, since the backend browses as a daemon -- has no
    /// send to fail, and would report success against a dead backend. This
    /// lets the caller observe the same loss without inventing a probe
    /// command that would have a real effect.
    pub fn is_closed(&self) -> bool {
        self.command_tx.is_closed()
    }

    /// Non-blocking send for the shell effect executor.
    ///
    /// Returns the allocated command ID, or a typed error when the queue is
    /// full ([`BackendSendError::Busy`]) or closed
    /// ([`BackendSendError::Closed`]).
    pub fn try_send(&self, command: BackendCommand) -> Result<u64, BackendSendError> {
        self.try_send_envelope(command, None)
    }

    /// Sends with a bounded receipt for edges and the latest confirmed master
    /// request in a batch. An unconfirmed Start preserves that master receipt
    /// until the winning value commits. Superseded receipts report `Lost`.
    pub fn try_send_confirmed(
        &self,
        command: BackendCommand,
    ) -> Result<(u64, CommandReceipt), BackendSendError> {
        let (sender, receiver) = oneshot::channel();
        let id = self.try_send_envelope(command, Some(sender))?;
        Ok((
            id,
            CommandReceipt {
                receiver,
                confirmed: None,
            },
        ))
    }

    fn try_send_envelope(
        &self,
        command: BackendCommand,
        confirmation: Option<oneshot::Sender<CommandConfirmation>>,
    ) -> Result<u64, BackendSendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.command_tx
            .try_send(CommandEnvelope {
                id,
                command,
                confirmation,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => BackendSendError::Busy,
                mpsc::error::TrySendError::Closed(_) => BackendSendError::Closed,
            })?;
        Ok(id)
    }

    /// Reserves capacity for every command before enqueueing any of them.
    ///
    /// A capacity refusal admits none of the batch. Accepted commands retain
    /// their order and receive distinct IDs, but this is not a backend
    /// transaction: the consumer and other producers can run between sends.
    pub fn try_send_batch<const N: usize>(
        &self,
        commands: [BackendCommand; N],
    ) -> Result<[u64; N], BackendSendError> {
        let permits = self
            .command_tx
            .try_reserve_many(N)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => BackendSendError::Busy,
                mpsc::error::TrySendError::Closed(_) => BackendSendError::Closed,
            })?;
        let mut ids = [0; N];
        for ((id, command), permit) in ids.iter_mut().zip(commands).zip(permits) {
            *id = self.next_id.fetch_add(1, Ordering::Relaxed);
            permit.send(CommandEnvelope {
                id: *id,
                command,
                confirmation: None,
            });
        }
        Ok(ids)
    }

    /// Awaiting send for non-UI callers; waits for queue capacity instead of
    /// failing with [`BackendSendError::Busy`].
    pub async fn send(&self, command: BackendCommand) -> Result<u64, BackendSendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.command_tx
            .send(CommandEnvelope {
                id,
                command,
                confirmation: None,
            })
            .await
            .map_err(|_| BackendSendError::Closed)?;
        Ok(id)
    }
}

#[cfg(test)]
impl DeviceBackendHandle {
    /// Builds a handle over a fresh queue with the requested capacity.
    fn test_channel(capacity: usize) -> (Self, mpsc::Receiver<CommandEnvelope>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self::new(tx), rx)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::backend::model::{
        AudioEndpointPreference, LatencyPreset, ReceiverId, RunIntent, SavedGroupId, Volume,
    };

    fn receiver(seed: u8) -> ReceiverId {
        ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
            .expect("fixed seed forms a valid storage key")
    }

    mod handle {
        use super::*;

        #[test]
        fn exact_receipt_waits_for_projection_and_remembers_its_terminal_result() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(1);
            let (id, mut receipt) = handle
                .try_send_confirmed(BackendCommand::DeleteGroup(SavedGroupId::generate()))
                .unwrap();
            let envelope = rx.try_recv().unwrap();
            assert_eq!(envelope.id, id);
            assert_eq!(receipt.poll(u64::MAX), ReceiptState::Pending);
            envelope
                .confirmation
                .unwrap()
                .send(CommandConfirmation {
                    session_generation_floor: None,
                    revision: 8,
                    error: Some(crate::backend::model::CommandFailure::Persistence),
                })
                .unwrap();
            assert_eq!(receipt.poll(7), ReceiptState::Pending);
            let expected =
                ReceiptState::Completed(Some(crate::backend::model::CommandFailure::Persistence));
            assert_eq!(receipt.poll(8), expected);
            assert_eq!(receipt.poll(900), expected);
        }

        #[test]
        fn session_floor_obeys_the_exact_receipts_revision_barrier() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(1);
            let (_, mut receipt) = handle
                .try_send_confirmed(BackendCommand::ActivateSavedGroup {
                    id: SavedGroupId::generate(),
                    start: true,
                })
                .unwrap();
            rx.try_recv()
                .unwrap()
                .confirmation
                .unwrap()
                .send(CommandConfirmation {
                    revision: 8,
                    error: None,
                    session_generation_floor: Some(31),
                })
                .unwrap();
            assert_eq!(receipt.poll(7), ReceiptState::Pending);
            assert_eq!(receipt.session_generation_floor(7), None);
            assert_eq!(receipt.poll(8), ReceiptState::Completed(None));
            assert_eq!(receipt.session_generation_floor(8), Some(31));
        }

        #[test]
        fn exact_receipt_reports_lost_only_when_its_own_producer_disappears() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(1);
            let (_, mut receipt) = handle
                .try_send_confirmed(BackendCommand::DeleteGroup(SavedGroupId::generate()))
                .unwrap();
            let envelope = rx.try_recv().unwrap();
            for _ in 0..130 {
                handle.try_send(BackendCommand::Shutdown).unwrap();
                drop(rx.try_recv().unwrap());
            }
            assert_eq!(receipt.poll(u64::MAX), ReceiptState::Pending);
            drop(envelope);
            assert_eq!(receipt.poll(u64::MAX), ReceiptState::Lost);
            drop(rx);
            assert!(matches!(
                handle.try_send_confirmed(BackendCommand::Shutdown),
                Err(BackendSendError::Closed)
            ));
        }

        #[test]
        fn batch_admission_reserves_every_slot_and_preserves_command_ids() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(3);
            let first = handle.try_send(BackendCommand::Shutdown).unwrap();
            assert_eq!(
                handle.try_send_batch([
                    BackendCommand::SetMuted(true),
                    BackendCommand::SetMasterVolume(Volume::DEFAULT_MASTER),
                    BackendCommand::SetRunIntent(RunIntent::Running),
                ]),
                Err(BackendSendError::Busy)
            );
            assert_eq!(rx.try_recv().unwrap().id, first);
            assert!(rx.try_recv().is_err(), "a rejected batch enqueues nothing");
            let ids = handle
                .try_send_batch([
                    BackendCommand::SetMuted(true),
                    BackendCommand::SetMuted(false),
                ])
                .unwrap();
            assert!(first < ids[0] && ids[0] < ids[1]);
            assert_eq!(rx.try_recv().unwrap().id, ids[0]);
            assert_eq!(rx.try_recv().unwrap().id, ids[1]);
            assert!(handle.try_send(BackendCommand::Shutdown).unwrap() > ids[1]);
            drop(rx);
            assert_eq!(
                handle.try_send_batch([BackendCommand::Shutdown]),
                Err(BackendSendError::Closed)
            );
        }

        #[tokio::test]
        async fn try_send_allocates_strictly_increasing_ids_before_enqueueing() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(COMMAND_CAPACITY);
            assert_eq!(handle.try_send(BackendCommand::Shutdown).unwrap(), 1);
            assert_eq!(
                handle
                    .try_send(BackendCommand::SetRunIntent(RunIntent::Running))
                    .unwrap(),
                2
            );
            assert_eq!(rx.recv().await.unwrap().id, 1);
            assert_eq!(rx.recv().await.unwrap().id, 2);
        }

        #[tokio::test]
        async fn try_send_reports_full_without_blocking() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(1);
            assert_eq!(handle.try_send(BackendCommand::Shutdown).unwrap(), 1);
            assert_eq!(
                handle.try_send(BackendCommand::Shutdown),
                Err(BackendSendError::Busy)
            );
            assert_eq!(rx.recv().await.unwrap().id, 1);
        }

        #[tokio::test]
        async fn try_send_reports_closed_after_receiver_drop() {
            let (handle, rx) = DeviceBackendHandle::test_channel(4);
            drop(rx);
            assert_eq!(
                handle.try_send(BackendCommand::Shutdown),
                Err(BackendSendError::Closed)
            );
        }

        #[tokio::test]
        async fn send_waits_for_capacity_and_keeps_ids_increasing() {
            let (handle, mut rx) = DeviceBackendHandle::test_channel(2);
            let first = handle.try_send(BackendCommand::Shutdown).unwrap();
            let second = handle.try_send(BackendCommand::Shutdown).unwrap();
            let pending = tokio::spawn({
                let handle = handle.clone();
                async move { handle.send(BackendCommand::Shutdown).await }
            });
            tokio::task::yield_now().await;
            assert!(
                !pending.is_finished(),
                "saturated queue must hold the sender"
            );
            assert_eq!(rx.recv().await.unwrap().id, first);
            assert_eq!(rx.recv().await.unwrap().id, second);
            assert_eq!(pending.await.unwrap().unwrap(), second + 1);
            assert_eq!(rx.recv().await.unwrap().id, second + 1);
        }

        #[tokio::test]
        async fn send_reports_closed_after_receiver_drop() {
            let (handle, rx) = DeviceBackendHandle::test_channel(4);
            drop(rx);
            assert_eq!(
                handle.send(BackendCommand::Shutdown).await,
                Err(BackendSendError::Closed)
            );
        }
    }

    mod edge_classification {
        use super::*;

        #[test]
        fn edge_commands_are_classified_exactly() {
            let group = SavedGroupId::generate();
            let cases = vec![
                (
                    BackendCommand::SetDesiredMembers {
                        members: BTreeSet::new(),
                    },
                    false,
                ),
                (BackendCommand::SetRunIntent(RunIntent::Stopped), false),
                (BackendCommand::SetMasterVolume(Volume::UNITY), false),
                (BackendCommand::SetMuted(true), false),
                (
                    BackendCommand::SetReceiverLevel {
                        receiver: receiver(1),
                        level: Volume::UNITY,
                    },
                    false,
                ),
                (
                    BackendCommand::SetAudioEndpoint(AudioEndpointPreference::SystemDefault),
                    false,
                ),
                (
                    BackendCommand::SetLatencyPreset(LatencyPreset::Normal),
                    false,
                ),
                (BackendCommand::SetAutoConnect(false), false),
                (
                    BackendCommand::SaveGroup {
                        id: None,
                        name: "Abends".to_owned(),
                        members: Vec::new(),
                    },
                    true,
                ),
                (BackendCommand::DeleteGroup(group), true),
                (
                    BackendCommand::ActivateSavedGroup {
                        id: group,
                        start: true,
                    },
                    true,
                ),
                (BackendCommand::RetryReceiver(receiver(2)), true),
                (BackendCommand::NotifySystem(SystemEvent::Suspending), true),
                (BackendCommand::NotifySystem(SystemEvent::Resumed), true),
                (
                    BackendCommand::NotifySystem(SystemEvent::NetworkChanged {
                        local_binding_changed: true,
                    }),
                    true,
                ),
                (
                    BackendCommand::Calibration(CalibrationCommand::ResetCalibration {
                        expected_desired_revision: 0,
                    }),
                    true,
                ),
                (
                    BackendCommand::Calibration(CalibrationCommand::RunCalibrationClickTest),
                    true,
                ),
                (BackendCommand::Shutdown, true),
            ];
            assert_eq!(cases.len(), 18);
            for (command, expected) in &cases {
                assert_eq!(
                    command.is_edge(),
                    *expected,
                    "wrong edge classification for {command:?}"
                );
            }
        }
    }
}
