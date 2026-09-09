//! Resilient session supervisor and restart classifier (Task 14).
//!
//! One supervisor owns at most one active AirPlay transport and one in-flight
//! start per session generation. Requests arrive through a bounded queue fed
//! exclusively by non-blocking producers; a newer generation always wins and
//! older reconciles are dropped as stale. Every lifecycle edge published to
//! the shell carries its generation so the controller can ignore stale
//! completions safely.
//!
//! All timing flows through injected seams ([`Timer`]); no test sleeps for
//! real. Teardown honors the spec budgets: two seconds per receiver and four
//! seconds total per group, enforced through the injected timer.

// The transport seams are wired to `backend::transport`; a few
// helper surfaces still await their controller call sites, so non-test builds
// would otherwise warn on them.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::future::pending;
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use airplay_audio::LiveAudioDecoder;
use airplay_client::{
    ClientDiagnosticsSource, GroupRuntimeEvent, Health, MemberFailure, MemberResult,
};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::backend::discovery::{Timer, TokioTimer};
use crate::backend::event::SUPERVISOR_CAPACITY;
use crate::backend::model::{
    LatencyConfig, ReceiverId, RestartReason, SetupPhase as SnapshotSetupPhase, UserFacingError,
};
use crate::calibration::CalibrationProfile;
use crate::diagnostics::{
    DiagnosticsClock, SessionDiagnosticsState, SessionStopReason, SystemDiagnosticsClock,
};

/// Fixed capacity of the controller-to-supervisor request queue.
///
/// Matches the global bounded-capacity rule (256 entries); producers use
/// non-blocking `try_send` and treat [`SessionSendError::Busy`] as
/// backpressure.
pub const SESSION_REQUEST_CAPACITY: usize = 256;

/// Cadence at which every active member is probed independently.
///
/// Two seconds plus the two-consecutive-timeout rule below reaches Degraded
/// within the mandated ten seconds without failing on a single lost answer.
/// The budget holds only because the transport fans its probes out across
/// members (see `AirPlayClient::probe_members`): a cycle costs one probe
/// timeout, not one per unresponsive member.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Consecutive [`Health::TimedOut`] answers required before a member fails.
///
/// A single missed probe is noise; `Health::Healthy` resets the streak.
pub const TIMEOUT_STREAK_TO_FAIL: u32 = 2;

/// Per-receiver RTSP TEARDOWN budget mandated by the design.
pub const TEARDOWN_PER_RECEIVER: Duration = Duration::from_secs(2);

/// Total group teardown budget mandated by the design.
pub const TEARDOWN_GROUP: Duration = Duration::from_secs(4);

/// Why a session reconfiguration was requested.
///
/// Variants mirror the approved full-restart matrix verbatim: exactly the
/// eleven rows below require tearing down and rebuilding the whole group,
/// while volume, mute, capture-endpoint, secondary-failure, metadata, and
/// saved-group edits must never interrupt a healthy session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReconfigureCause {
    /// Applying any added or removed desired member.
    Membership,
    /// Primary loss or primary replacement; group timing is invalidated.
    PrimaryFailure,
    /// Applying an enabled latency preset.
    LatencyChanged,
    /// Recovery after dead RTSP sessions or protocol desync.
    ProtocolDesync,
    /// A local network-interface or local address change.
    NetworkRebind,
    /// An active receiver changed its address.
    ReceiverAddressChanged,
    /// Transition between one-receiver/NTP and multi-receiver/PTP timing.
    TimingModeChanged,
    /// Stream format or SETUP-negotiated latency change.
    StreamFormatChanged,
    /// Windows resume after suspend.
    Resume,
    /// A previously failed secondary rejoined; the immutable streamer forces
    /// one coordinated restart instead of a live target insertion.
    SecondaryRejoin,
    /// Removing a failed target from the immutable streamer.
    RemoveFailedTarget,
    /// Master volume change; applied inline without restarting.
    MasterVolume,
    /// Per-receiver level change; applied inline without restarting.
    ReceiverVolume,
    /// Mute toggle; preserves volumes while applying zero downstream.
    Mute,
    /// Capture endpoint loss/replacement/selection with a live PCM bridge.
    CaptureEndpoint,
    /// A secondary failed; survivors continue degraded without a restart.
    SecondaryFailure,
    /// Discovery display-name/model metadata update only.
    DiscoveryMetadata,
    /// Saved-group create/rename/update/delete without activation.
    SavedGroupEdited,
    /// A manual calibration profile was applied or reset.
    CalibrationChanged,
}

impl ReconfigureCause {
    /// Whether this cause mandates a full active-session restart.
    ///
    /// The mapping is the approved matrix and nothing else; adding rows
    /// requires a matching row in the design document.
    pub fn requires_full_restart(self) -> bool {
        matches!(
            self,
            Self::Membership
                | Self::PrimaryFailure
                | Self::LatencyChanged
                | Self::ProtocolDesync
                | Self::NetworkRebind
                | Self::ReceiverAddressChanged
                | Self::TimingModeChanged
                | Self::StreamFormatChanged
                | Self::Resume
                | Self::SecondaryRejoin
                | Self::RemoveFailedTarget
                | Self::CalibrationChanged
        )
    }

    /// The snapshot-facing restart reason, if this cause restarts at all.
    pub fn restart_reason(self) -> Option<RestartReason> {
        if !self.requires_full_restart() {
            return None;
        }
        Some(match self {
            Self::Membership => RestartReason::MembershipChange,
            Self::PrimaryFailure => RestartReason::PrimaryReplaced,
            Self::LatencyChanged => RestartReason::LatencyPresetChanged,
            Self::ProtocolDesync => RestartReason::DeadRtspRecovered,
            Self::NetworkRebind => RestartReason::LocalInterfaceChanged,
            Self::ReceiverAddressChanged => RestartReason::ReceiverAddressChanged,
            Self::TimingModeChanged => RestartReason::TimingModeTransition,
            Self::StreamFormatChanged => RestartReason::StreamFormatChanged,
            Self::Resume => RestartReason::SystemResume,
            Self::SecondaryRejoin => RestartReason::SecondaryRejoin,
            Self::RemoveFailedTarget => RestartReason::FailedTargetRemoved,
            Self::CalibrationChanged => RestartReason::CalibrationChanged,
            // Non-restart rows are rejected above.
            Self::MasterVolume
            | Self::ReceiverVolume
            | Self::Mute
            | Self::CaptureEndpoint
            | Self::SecondaryFailure
            | Self::DiscoveryMetadata
            | Self::SavedGroupEdited => unreachable!("non-restart cause has no restart reason"),
        })
    }
}

/// Maps an upstream setup phase onto the identically named snapshot phase.
///
/// The production adapter performs this conversion exhaustively so a new
/// upstream variant is a compile error instead of a silently unmapped state.
pub(crate) fn snapshot_phase(phase: airplay_client::SetupPhase) -> SnapshotSetupPhase {
    match phase {
        airplay_client::SetupPhase::Connect => SnapshotSetupPhase::Connect,
        airplay_client::SetupPhase::Pair => SnapshotSetupPhase::Pair,
        airplay_client::SetupPhase::PrimaryTiming => SnapshotSetupPhase::PrimaryTiming,
        airplay_client::SetupPhase::RtspSetup => SnapshotSetupPhase::RtspSetup,
        airplay_client::SetupPhase::SetPeers => SnapshotSetupPhase::SetPeers,
        airplay_client::SetupPhase::BuildSender => SnapshotSetupPhase::BuildSender,
        airplay_client::SetupPhase::StartAudio => SnapshotSetupPhase::StartAudio,
    }
}

/// Failure payload of a best-effort session start.
#[derive(Debug)]
pub struct SessionStartFailure {
    /// Pre-redacted user-safe summary of why no session could be established.
    pub error: UserFacingError,
    /// Per-member failures from the underlying best-effort setup.
    pub failures: Vec<MemberFailure>,
}

/// Truthful loss notice emitted when the transport's runtime-event ring
/// overflowed and events had to be dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeEventLag {
    /// Number of events dropped by the bounded fan-out feed.
    pub dropped: u64,
}

/// One live AirPlay transport for a session generation.
///
/// Adapter seam: the production implementation wraps
/// `airplay_client::AirPlayClient`; tests script it locally.
#[doc(hidden)]
#[async_trait]
pub trait SessionTransport: Send {
    /// Completes the final level barrier and releases prepared audio at adoption.
    /// The supervisor must not consume live controls between this and adoption.
    async fn activate(&mut self) -> Result<(), UserFacingError> {
        Ok(())
    }
    /// Read-only diagnostics backed by this live transport, when available.
    fn diagnostics_source(&self) -> Option<ClientDiagnosticsSource> {
        None
    }
    /// Current PTP primary, if any.
    fn primary(&self) -> Option<ReceiverId>;
    /// Receivers with successfully negotiated connections.
    fn active_members(&self) -> BTreeSet<ReceiverId>;
    /// Setup-phase-classified failures of the last best-effort start.
    ///
    /// Takes them: `MemberFailure` owns a non-cloneable `airplay_core::Error`,
    /// so a production transport cannot hand the same list out twice. The
    /// supervisor consumes it exactly once, at adoption.
    fn setup_failures(&mut self) -> Vec<MemberFailure>;
    /// Pops one queued runtime event; `Err` reports truthful lag instead of
    /// silently skipping.
    fn try_runtime_event(&mut self) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag>;
    /// Probes every live member's health independently.
    async fn probe_members(&mut self) -> Vec<MemberResult<Health>>;
    /// Sets the volume of exactly one member without touching others.
    async fn set_member_volume(&mut self, receiver: &ReceiverId, volume: f32) -> MemberResult<()>;
    /// Bounded best-effort stop; must never block indefinitely.
    async fn stop(&mut self) -> anyhow::Result<()>;
}

/// Establishes one session generation from the newest complete desired set.
///
/// Adapter seam: production wraps `connect_group_best_effort`; tests script
/// outcomes without network.
#[doc(hidden)]
#[async_trait]
pub trait SessionTransportFactory: Send + Sync {
    /// Starts the group against `desired`, honoring the preferred primary
    /// when healthy and feeding the stable live decoder into the streamer.
    async fn start(
        &self,
        desired: Vec<airplay_core::Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        latency: LatencyConfig,
        calibration: CalibrationProfile,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure>;

    /// Starts with the current effective receiver levels. Test factories that
    /// model only lifecycle behavior retain the simpler [`Self::start`]
    /// contract; the production adapter overrides this before it starts RTP.
    async fn start_with_volumes(
        &self,
        desired: Vec<airplay_core::Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        latency: LatencyConfig,
        calibration: CalibrationProfile,
        _volumes: EffectiveVolumeState,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
        self.start(desired, preferred_primary, decoder, latency, calibration)
            .await
    }
}

/// Latest durable effective levels shared between the actor and an in-flight
/// start. The setup barrier reads this only after the receiver connection has
/// completed, so a slider change during pairing cannot be overwritten by an
/// earlier setup snapshot.
#[derive(Clone, Debug, Default)]
pub struct EffectiveVolumeState {
    values: Arc<RwLock<VersionedEffectiveVolumes>>,
}

#[derive(Debug, Default)]
struct VersionedEffectiveVolumes {
    version: u64,
    values: BTreeMap<ReceiverId, f32>,
}

/// Coherent effective-volume snapshot used by one setup pass.
#[derive(Clone, Debug)]
pub struct EffectiveVolumeSnapshot {
    version: u64,
    values: BTreeMap<ReceiverId, f32>,
}

impl EffectiveVolumeState {
    /// Stores one already-derived effective level for both live control and a
    /// future setup barrier.
    pub fn set(&self, receiver: ReceiverId, volume: f32) {
        let mut state = self
            .values
            .write()
            .expect("effective-volume state lock is not poisoned");
        let volume = volume.clamp(0.0, 1.0);
        if state.values.insert(receiver, volume) != Some(volume) {
            state.version = state.version.wrapping_add(1);
        }
    }

    /// Returns the newest effective level for `receiver`; no persisted level
    /// intentionally means the product default of unity.
    pub fn get(&self, receiver: ReceiverId) -> f32 {
        self.values
            .read()
            .expect("effective-volume state lock is not poisoned")
            .values
            .get(&receiver)
            .copied()
            .unwrap_or(1.0)
    }

    /// Captures all effective levels and their revision for a bounded setup
    /// pass. A caller must revalidate before starting audio.
    pub fn snapshot(&self) -> EffectiveVolumeSnapshot {
        let state = self
            .values
            .read()
            .expect("effective-volume state lock is not poisoned");
        EffectiveVolumeSnapshot {
            version: state.version,
            values: state.values.clone(),
        }
    }

    /// Whether no durable effective-volume update happened after `snapshot`.
    pub fn is_current(&self, snapshot: &EffectiveVolumeSnapshot) -> bool {
        self.values
            .read()
            .expect("effective-volume state lock is not poisoned")
            .version
            == snapshot.version
    }

    /// Linearizes release with synchronous durable level updates.
    pub(crate) fn release_if_current(
        &self,
        snapshot: &EffectiveVolumeSnapshot,
        release: &mut Option<tokio::sync::oneshot::Sender<()>>,
    ) -> bool {
        let state = self
            .values
            .read()
            .expect("effective-volume state lock is not poisoned");
        if state.version != snapshot.version {
            return false;
        }
        release
            .take()
            .is_some_and(|release| release.send(()).is_ok())
    }
}

impl EffectiveVolumeSnapshot {
    /// Returns the captured level, defaulting only an absent persisted value
    /// to unity.
    pub fn get(&self, receiver: ReceiverId) -> f32 {
        self.values.get(&receiver).copied().unwrap_or(1.0)
    }
}

/// Hands out the stable live decoder for one session generation.
///
/// Production wraps the capture supervisor's decoder-pair seam (Task 15);
/// tests create throwaway pairs.
#[doc(hidden)]
pub trait SessionDecoderSource: Send + Sync {
    /// Takes the live decoder half of the stable PCM bridge pair.
    fn take_decoder(&self) -> LiveAudioDecoder;
}

/// Requests the session supervisor may receive.
///
/// Producers are non-blocking; the queue is bounded at
/// [`SESSION_REQUEST_CAPACITY`].
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // plan-fixed shape: complete desired set by value
pub enum SessionRequest {
    /// Reconciles the session towards a complete desired set under a
    /// monotonically increasing generation; older generations lose.
    Reconcile {
        /// Monotonic session generation owning this desired set.
        generation: u64,
        /// Complete newest desired membership as resolved devices.
        desired: Vec<airplay_core::Device>,
        /// Preferred primary when healthy and PTP-capable.
        preferred_primary: Option<ReceiverId>,
        /// Resolved latency configuration for this generation.
        latency: LatencyConfig,
        /// Manual calibration intent for this generation.
        ///
        /// Travels with the desired set because it IS a setup input: the
        /// normalized delays are handed to the targets while the group is
        /// built, so a changed profile can only take effect through a new
        /// generation. Keyed by stable receiver identity, never by index.
        calibration: CalibrationProfile,
        /// Whether this generation exists only to tear the session down.
        ///
        /// Set when the controller's run intent is `Stopped`, which is the
        /// one reason an empty desired set means "no session wanted". An
        /// empty set that arises any other way -- every desired receiver
        /// serving its backoff, or none of them castable yet -- is a running
        /// app with nothing to connect *right now*, and must not be reported
        /// as a stop.
        teardown_only: bool,
    },
    /// Changes one member's level inline; never bumps the generation.
    SetVolume {
        /// Target receiver.
        receiver: ReceiverId,
        /// Linear volume within `0.0..=1.0`.
        volume: f32,
    },
    /// Probes every live member's health now.
    Probe,
    /// A local network-interface change was observed.
    NetworkChanged,
    /// Windows is about to suspend; boundedly disconnect.
    Suspend,
    /// Windows resumed; rebuild the retained session as a fresh generation.
    Resume,
}

impl SessionRequest {
    /// Whether handling this request may end or replace the current session
    /// generation. Volume changes deliberately never do.
    pub(crate) fn affects_session_generation(&self) -> bool {
        matches!(
            self,
            Self::Reconcile { .. } | Self::NetworkChanged | Self::Suspend | Self::Resume
        )
    }
}

/// Generation-tagged lifecycle edges published to the device actor.
#[derive(Debug)]
pub enum SessionUpdate {
    /// A generation began best-effort setup.
    Starting {
        /// Generation performing the setup.
        generation: u64,
    },
    /// The generation holds a live session.
    Active {
        /// Generation holding the stream.
        generation: u64,
        /// Elected PTP primary.
        primary: ReceiverId,
        /// Live members including the primary.
        members: BTreeSet<ReceiverId>,
        /// Desired receivers whose setup failed best-effort.
        partial_failures: Vec<MemberFailure>,
    },
    /// A member refused one out-of-band control request -- today only a
    /// volume set -- while its stream kept running.
    ///
    /// Deliberately distinct from [`Self::MemberFailed`]: a rejected
    /// SET_PARAMETER response says something about that one request and
    /// nothing about the group's PTP time base or the RTP flow carrying the
    /// audio. Routed through the member-failure path it would be classified
    /// against the full-restart matrix, and on the primary that classification
    /// is `PrimaryFailure` -- a whole-group teardown, with the audible gap
    /// that implies, bought by a single drag of a volume slider. A receiver
    /// that is genuinely gone is removed by the health probe, which is the
    /// mechanism that actually establishes it.
    MemberControlFailed {
        /// Owning generation; older arrivals are stale and dropped.
        generation: u64,
        /// Receiver that refused the request.
        receiver: ReceiverId,
        /// Pre-redacted presentation-safe summary.
        error: UserFacingError,
    },
    /// Exactly one member failed at runtime; survivors continue.
    MemberFailed {
        /// Owning generation.
        generation: u64,
        /// Failed receiver.
        receiver: ReceiverId,
        /// Whether the failed receiver was the group primary.
        primary: bool,
        /// Pre-redacted presentation-safe summary.
        error: UserFacingError,
    },
    /// An announced full-session restart is in flight.
    Recovering {
        /// Generation undergoing recovery.
        generation: u64,
        /// Matrix reason driving the restart.
        reason: RestartReason,
        /// Zero-based attempt counter for repeated recovery.
        attempt: u32,
    },
    /// The generation was boundedly torn down.
    Stopped {
        /// Generation that was stopped.
        generation: u64,
    },
}

/// Typed failure of a non-blocking request send.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SessionSendError {
    /// The fixed-capacity request queue is currently full.
    #[error("session queue is busy")]
    Busy,
    /// The supervisor is gone and can no longer receive requests.
    #[error("session supervisor is closed")]
    Closed,
}

/// Typed outcome of the bounded group teardown helper.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SessionStopError {
    /// The transport exceeded the four-second group budget.
    #[error("group teardown exceeded the {TEARDOWN_GROUP:?} budget")]
    Timeout,
    /// The transport reported a failure while stopping.
    #[error("transport stop failed: {0}")]
    Failed(String),
}

/// Computes `(per-receiver deadline, group deadline)` from one instant.
///
/// Pure function over the injected clock so tests assert both budgets
/// without sleeping; the per-receiver budget applies inside the production
/// adapter (TEARDOWN runs concurrently across members).
pub(crate) fn teardown_deadlines(now: Instant) -> (Instant, Instant) {
    (now + TEARDOWN_PER_RECEIVER, now + TEARDOWN_GROUP)
}

/// Runs `stop` under the four-second group budget using the injected timer.
///
/// Never blocks indefinitely: whichever side completes first wins, so a
/// wedged transport is abandoned after exactly [`TEARDOWN_GROUP`].
pub(crate) async fn bounded_stop(
    timer: Arc<dyn Timer>,
    stop: impl Future<Output = anyhow::Result<()>>,
) -> Result<(), SessionStopError> {
    tokio::select! {
        result = stop => result.map_err(|error| SessionStopError::Failed(error.to_string())),
        () = timer.sleep(TEARDOWN_GROUP) => Err(SessionStopError::Timeout),
    }
}

/// Latest-generation gate shared by the supervisor loop.
///
/// A reconcile is admitted only when strictly newer than every generation
/// accepted before; equal and older arrivals are stale and dropped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GenerationGate {
    accepted: u64,
}

impl GenerationGate {
    /// Admits `generation` when newer than everything accepted so far.
    fn admit(&mut self, generation: u64) -> bool {
        if generation > self.accepted {
            self.accepted = generation;
            true
        } else {
            false
        }
    }

    /// Highest accepted generation.
    fn current(self) -> u64 {
        self.accepted
    }
}

/// Parameters retained between reconciles so platform edges (network change,
/// suspend/resume) can rebuild against the newest complete desired set.
#[derive(Clone)]
struct ReconcileParams {
    generation: u64,
    desired: Vec<airplay_core::Device>,
    preferred_primary: Option<ReceiverId>,
    latency: LatencyConfig,
    /// See [`SessionRequest::Reconcile::calibration`].
    calibration: CalibrationProfile,
    /// See [`SessionRequest::Reconcile::teardown_only`].
    teardown_only: bool,
}

/// In-flight best-effort start awaiting adoption.
///
/// The task reports its outcome through a cancel-safe oneshot so the loop can
/// wait on it inside `select!` while keeping the join handle available to
/// abort superseded attempts deterministically.
struct StartAttempt {
    generation: u64,
    join: JoinHandle<()>,
    outcome: oneshot::Receiver<Result<Box<dyn SessionTransport>, SessionStartFailure>>,
}

type StartOutcome = Result<Box<dyn SessionTransport>, SessionStartFailure>;

/// Cloneable producer half of the bounded request queue.
#[derive(Clone, Debug)]
pub struct SessionSupervisorHandle {
    requests_tx: mpsc::Sender<SessionRequest>,
    volumes: EffectiveVolumeState,
}

impl SessionSupervisorHandle {
    /// Non-blocking send; rejects with [`SessionSendError`] instead of ever
    /// waiting for capacity.
    pub fn try_send(&self, request: SessionRequest) -> Result<(), SessionSendError> {
        if let SessionRequest::SetVolume { receiver, volume } = &request {
            self.remember_volume(*receiver, *volume);
        }
        self.requests_tx
            .try_send(request)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SessionSendError::Busy,
                mpsc::error::TrySendError::Closed(_) => SessionSendError::Closed,
            })
    }

    /// Stores a durable effective volume directly in the shared latest-value
    /// source. This intentionally bypasses the bounded request queue so a
    /// full queue can never make a later reconnect fall back to unity.
    pub fn remember_volume(&self, receiver: ReceiverId, volume: f32) {
        self.volumes.set(receiver, volume);
    }
}

/// Tunables of one supervisor instance.
///
/// Only the probe cadence is configurable; tests compress it so the
/// production two-second rhythm does not stretch their budget. Everything
/// else stays fixed by the design.
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
    /// Cadence at which every active member is probed independently.
    pub probe_interval: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            probe_interval: PROBE_INTERVAL,
        }
    }
}

/// Long-running session supervisor handle plus its update surface.
pub struct SessionSupervisor {
    handle: SessionSupervisorHandle,
    /// `None` once [`SessionSupervisor::take_updates`] handed the receive half
    /// to an owner outside this handle. The channel is created once in
    /// [`SessionSupervisor::start_with`] and outlives every session
    /// generation -- a reconfigure or restart replaces the transport, never
    /// the queue -- so handing the receiver out once loses nothing.
    updates: Option<mpsc::Receiver<SessionUpdate>>,
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
}

impl SessionSupervisor {
    /// Spawns the supervisor over the given factory and decoder source.
    ///
    /// The timer seam is the production Tokio timer internally; paused test
    /// runtimes auto-advance it, and lifecycle decisions never depend on real
    /// wall-clock delays.
    pub fn start(
        factory: Arc<dyn SessionTransportFactory>,
        decoders: Arc<dyn SessionDecoderSource>,
    ) -> Self {
        Self::start_with(factory, decoders, SessionConfig::default())
    }

    /// Spawns the supervisor with an explicit configuration.
    pub fn start_with(
        factory: Arc<dyn SessionTransportFactory>,
        decoders: Arc<dyn SessionDecoderSource>,
        config: SessionConfig,
    ) -> Self {
        let (diagnostics, _unused) = watch::channel(SessionDiagnosticsState::default());
        Self::start_with_diagnostics_config(factory, decoders, config, diagnostics)
    }

    /// Spawns the supervisor with a generation-safe diagnostics state port.
    #[doc(hidden)]
    pub fn start_with_diagnostics(
        factory: Arc<dyn SessionTransportFactory>,
        decoders: Arc<dyn SessionDecoderSource>,
        diagnostics: watch::Sender<SessionDiagnosticsState>,
    ) -> Self {
        Self::start_with_diagnostics_config(
            factory,
            decoders,
            SessionConfig::default(),
            diagnostics,
        )
    }

    /// Spawns the supervisor with explicit runtime and diagnostics settings.
    #[doc(hidden)]
    pub fn start_with_diagnostics_config(
        factory: Arc<dyn SessionTransportFactory>,
        decoders: Arc<dyn SessionDecoderSource>,
        config: SessionConfig,
        diagnostics: watch::Sender<SessionDiagnosticsState>,
    ) -> Self {
        let (requests_tx, requests_rx) = mpsc::channel(SESSION_REQUEST_CAPACITY);
        let (updates_tx, updates) = mpsc::channel(SUPERVISOR_CAPACITY);
        let cancel = CancellationToken::new();
        let volumes = EffectiveVolumeState::default();
        let join = tokio::spawn(run_supervisor(
            factory,
            decoders,
            config,
            requests_rx,
            updates_tx,
            cancel.clone(),
            volumes.clone(),
            diagnostics,
        ));
        Self {
            handle: SessionSupervisorHandle {
                requests_tx,
                volumes,
            },
            updates: Some(updates),
            cancel,
            join: Some(join),
        }
    }

    /// Cloneable producer handle for the bounded request queue.
    pub fn handle(&self) -> SessionSupervisorHandle {
        self.handle.clone()
    }

    /// Receive half of the bounded update queue.
    ///
    /// # Panics
    ///
    /// Panics once [`Self::take_updates`] has moved the receiver out. Owning
    /// the stream and borrowing it through the handle are mutually exclusive
    /// by construction; a caller doing both has two consumers of one queue.
    pub fn updates_mut(&mut self) -> &mut mpsc::Receiver<SessionUpdate> {
        self.updates
            .as_mut()
            .expect("the update receiver was taken out of this supervisor")
    }

    /// Moves the receive half out of the handle, once.
    ///
    /// Callers that own the stream outright never need the supervisor lock to
    /// read it, so nothing has to wait on an update while holding a lock that
    /// control requests need. Returns `None` on every call after the first.
    pub fn take_updates(&mut self) -> Option<mpsc::Receiver<SessionUpdate>> {
        self.updates.take()
    }

    /// Signals the supervisor to end, without taking ownership of it.
    ///
    /// The controller holds this supervisor behind a shared mutex for the
    /// whole run, so it can never reclaim ownership for [`Self::shutdown`].
    /// Dropping the request handle is not a substitute:
    /// the supervisor keeps a producer clone of that queue itself, so the
    /// `recv()` arm never returns `None` and the loop runs on. Without this
    /// the update channel stays open until the supervisor is *dropped*, which
    /// happens after the controller's drains -- so every exit spent the full
    /// teardown budget waiting for a worker nobody had told to stop.
    pub fn request_shutdown(&self) {
        self.cancel.cancel();
    }

    /// Cancels supervision and awaits the bounded final teardown.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

impl Drop for SessionSupervisor {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

/// Publishes one lifecycle edge; a full queue logs instead of blocking.
fn push_update(updates: &mpsc::Sender<SessionUpdate>, update: SessionUpdate) {
    if let Err(error) = updates.try_send(update) {
        tracing::warn!("session update queue unavailable ({error}); edge dropped");
    }
}

/// Publishes the complete active diagnostics binding after transport adoption.
fn publish_active_diagnostics(
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
    generation: u64,
    primary: ReceiverId,
    members: &BTreeSet<ReceiverId>,
    source: ClientDiagnosticsSource,
) {
    let primary = (members.len() > 1).then_some(primary);
    let members = members
        .iter()
        .copied()
        .map(|receiver| (receiver, receiver.into()))
        .collect();
    diagnostics.send_replace(SessionDiagnosticsState::Active {
        generation,
        started_elapsed_ns: SystemDiagnosticsClock::new().elapsed_ns(),
        primary,
        members,
        source,
    });
}

/// Tombstones the currently published active generation.
fn publish_inactive_diagnostics(
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
    reason: SessionStopReason,
) {
    let generation = match &*diagnostics.borrow() {
        SessionDiagnosticsState::Inactive { generation, .. }
        | SessionDiagnosticsState::Active { generation, .. } => *generation,
    };
    diagnostics.send_replace(SessionDiagnosticsState::Inactive { generation, reason });
}

/// Stops the transport under the group budget via the injected timer.
async fn stop_bounded(
    timer: Arc<dyn Timer>,
    transport: &mut dyn SessionTransport,
) -> Result<(), SessionStopError> {
    // The two-second per-receiver budget applies inside the production
    // adapter's concurrent TEARDOWN fan-out; the supervisor bounds the whole
    // group here so one wedged member can delay nothing else.
    bounded_stop(timer, transport.stop()).await
}

/// Waits for an already-aborted start attempt to actually end, bounded by the
/// group budget through the injected timer.
///
/// `JoinHandle::abort` only marks a task; it dies at its next yield point and
/// keeps whatever it holds until then. That is tolerable when a newer
/// generation supersedes an older one -- the machine keeps running and the
/// drop follows immediately -- but not across a system suspend, where the
/// mark and the death are separated by the sleep itself. A start task parked
/// in a TCP connect or a TEARDOWN round trip would carry its half-built
/// sockets into the sleep and could still finish afterwards, handing back a
/// transport built against a network stack that no longer exists and that
/// nobody owns.
async fn bounded_join(timer: Arc<dyn Timer>, join: JoinHandle<()>) {
    tokio::select! {
        _ = join => {}
        () = timer.sleep(TEARDOWN_GROUP) => {
            tracing::warn!("an aborted session start outlived the teardown budget");
        }
    }
}

/// Aborts any superseded start and boundedly stops any obsolete transport.
async fn supersede(
    in_flight: &mut Option<StartAttempt>,
    active: &mut Option<Box<dyn SessionTransport>>,
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
    reason: SessionStopReason,
    timer: &Arc<dyn Timer>,
) {
    if let Some(attempt) = in_flight.take() {
        attempt.join.abort();
    }
    if let Some(mut transport) = active.take() {
        publish_inactive_diagnostics(diagnostics, reason);
        let _ = stop_bounded(Arc::clone(timer), transport.as_mut()).await;
    }
}
/// Spawns one best-effort start for `params`, reporting via cancel-safe
/// oneshot so the loop can race it against newer requests.
fn spawn_start(
    factory: &Arc<dyn SessionTransportFactory>,
    decoders: &Arc<dyn SessionDecoderSource>,
    params: ReconcileParams,
    volumes: EffectiveVolumeState,
) -> StartAttempt {
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let factory = Arc::clone(factory);
    let decoders = Arc::clone(decoders);
    let join = tokio::spawn(async move {
        let decoder = decoders.take_decoder();
        let outcome = factory
            .start_with_volumes(
                params.desired,
                params.preferred_primary,
                decoder,
                params.latency,
                params.calibration,
                volumes,
            )
            .await;
        // A missing consumer means a newer generation already won.
        let _ = outcome_tx.send(outcome);
    });
    StartAttempt {
        generation: params.generation,
        join,
        outcome: outcome_rx,
    }
}

/// Admits and dispatches one reconcile generation.
///
/// Older generations lose here too: the queue-level coalescing keeps only the
/// newest arrival, and this gate additionally rejects anything equal to or
/// below the accepted generation.
#[allow(clippy::too_many_arguments)] // explicit disjoint-state threading (capture.rs precedent)
async fn start_generation(
    params: ReconcileParams,
    gate: &mut GenerationGate,
    in_flight: &mut Option<StartAttempt>,
    active: &mut Option<Box<dyn SessionTransport>>,
    retained: &mut Option<ReconcileParams>,
    health: &mut HealthState,
    updates: &mpsc::Sender<SessionUpdate>,
    factory: &Arc<dyn SessionTransportFactory>,
    decoders: &Arc<dyn SessionDecoderSource>,
    timer: &Arc<dyn Timer>,
    volumes: &EffectiveVolumeState,
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
) {
    if !gate.admit(params.generation) {
        return; // stale-generation rejection
    }
    let stop_reason = if params.teardown_only {
        SessionStopReason::Stopped
    } else {
        SessionStopReason::Restarted
    };
    supersede(in_flight, active, diagnostics, stop_reason, timer).await;
    // Health bookkeeping is per generation; nothing carries over.
    health.reset();
    let generation = params.generation;
    let nothing_desired = params.desired.is_empty();
    *retained = Some(params.clone());
    if nothing_desired {
        // Nothing to connect, so nothing that could fail: spawning a start
        // here would report "no selected receiver could be reached" for a
        // state nobody can fix by retrying. The teardown above has already
        // happened; what this generation still owes is the truthful reason.
        //
        // Which reason that is comes from the request, not from the size of
        // the set. `teardown_only` is the app not wanting a session -- the
        // user stopped it, or deselected the last receiver -- and that is a
        // stop. Everything else is a running app whose desired receivers are
        // all currently out (serving a backoff, or not yet castable): the
        // rejoin arm will build against them again, so this is a restart in
        // progress. Announcing `Stopped` there would publish
        // `SessionPhase::Stopped` under `RunIntent::Running`, a pair
        // `derive_phase` never produces, and would erase the only signal
        // that says the app is still trying.
        let update = if params.teardown_only {
            SessionUpdate::Stopped { generation }
        } else {
            SessionUpdate::Recovering {
                generation,
                reason: RestartReason::DeadRtspRecovered,
                attempt: 0,
            }
        };
        push_update(updates, update);
        return;
    }
    push_update(updates, SessionUpdate::Starting { generation });
    *in_flight = Some(spawn_start(factory, decoders, params, volumes.clone()));
}

/// Announces recovery and rebuilds against the retained desired set.
///
/// Used by `NetworkChanged` ([`RestartReason::LocalInterfaceChanged`]) and
/// `Resume` ([`RestartReason::SystemResume`]): both are matrix rows that
/// invalidate all pre-existing RTSP/RTP/PTP state.
#[allow(clippy::too_many_arguments)] // explicit disjoint-state threading (capture.rs precedent)
async fn rebuild_for_cause(
    reason: RestartReason,
    gate: &mut GenerationGate,
    in_flight: &mut Option<StartAttempt>,
    active: &mut Option<Box<dyn SessionTransport>>,
    retained: &mut Option<ReconcileParams>,
    health: &mut HealthState,
    updates: &mpsc::Sender<SessionUpdate>,
    factory: &Arc<dyn SessionTransportFactory>,
    decoders: &Arc<dyn SessionDecoderSource>,
    timer: &Arc<dyn Timer>,
    volumes: &EffectiveVolumeState,
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
) {
    let Some(previous) = retained.as_ref().map(|params| ReconcileParams {
        generation: params.generation.wrapping_add(1),
        desired: params.desired.clone(),
        preferred_primary: params.preferred_primary,
        latency: params.latency,
        calibration: params.calibration.clone(),
        teardown_only: params.teardown_only,
    }) else {
        return; // nothing ever started; nothing to rebuild
    };
    push_update(
        updates,
        SessionUpdate::Recovering {
            generation: gate.current(),
            reason,
            attempt: 0,
        },
    );
    start_generation(
        previous,
        gate,
        in_flight,
        active,
        retained,
        health,
        updates,
        factory,
        decoders,
        timer,
        volumes,
        diagnostics,
    )
    .await;
}

/// Per-generation health bookkeeping backing the probe policy.
///
/// A successful UDP send is deliberately absent here: it proves nothing about
/// receiver health, so only probe answers can clear or raise a streak.
#[derive(Default)]
struct HealthState {
    /// Consecutive [`Health::TimedOut`] answers per receiver.
    streak: BTreeMap<ReceiverId, u32>,
    /// Receivers already reported failed in this generation.
    failed: BTreeSet<ReceiverId>,
    /// Receivers whose next probe answers for an observed UDP send error and
    /// therefore fails on the first timeout instead of the second.
    urgent: BTreeSet<ReceiverId>,
}

impl HealthState {
    /// Clears everything; called whenever a new generation takes over.
    fn reset(&mut self) {
        self.streak.clear();
        self.failed.clear();
        self.urgent.clear();
    }
}

/// Drains the transport's runtime-event feed.
///
/// A `TargetSendFailed` is NOT a failure by itself: per the design a UDP send
/// error only schedules an immediate probe, and the receiver fails solely when
/// that probe times out or errors. Returns whether any receiver was armed.
fn drain_runtime_events(
    transport: &mut dyn SessionTransport,
    urgent: &mut BTreeSet<ReceiverId>,
) -> bool {
    let before = urgent.len();
    for _ in 0..SESSION_REQUEST_CAPACITY {
        match transport.try_runtime_event() {
            Ok(Some(GroupRuntimeEvent::TargetSendFailed { receiver, source })) => {
                tracing::debug!("target send failed; scheduling an immediate probe: {source}");
                urgent.insert(ReceiverId::from(receiver));
            }
            Ok(None) => break,
            Err(lag) => {
                tracing::warn!(
                    "runtime event feed lagged; {} event(s) dropped",
                    lag.dropped
                );
                break;
            }
        }
    }
    urgent.len() > before
}

/// Probes every live member and applies the failure policy.
///
/// The independence is the transport's: `probe_members` answers for all
/// members at once, so one cycle costs one probe timeout regardless of how
/// many members are silent. Everything below then classifies those answers:
///
/// * hard transport/protocol errors fail immediately,
/// * two consecutive timeouts fail, a single one only arms the streak,
/// * `Health::Healthy` resets the streak,
/// * a receiver armed by a UDP send error fails on its first timeout.
async fn run_probe_cycle(
    active: &mut Option<Box<dyn SessionTransport>>,
    health: &mut HealthState,
    gate: &GenerationGate,
    updates: &mpsc::Sender<SessionUpdate>,
) {
    let Some(transport) = active.as_mut() else {
        return;
    };
    drain_runtime_events(transport.as_mut(), &mut health.urgent);
    let primary = transport.primary();
    let results = transport.probe_members().await;
    let urgent = std::mem::take(&mut health.urgent);

    for result in results {
        let receiver = ReceiverId::from(result.receiver);
        if health.failed.contains(&receiver) {
            continue; // already reported; survivors continue undisturbed
        }
        let failure = match result.result {
            Ok(Health::Healthy) => {
                health.streak.remove(&receiver);
                None
            }
            Ok(Health::TimedOut) => {
                let streak = health.streak.entry(receiver).or_insert(0);
                *streak = streak.saturating_add(1);
                let armed = urgent.contains(&receiver);
                (*streak >= TIMEOUT_STREAK_TO_FAIL || armed)
                    .then(|| UserFacingError::new("the receiver stopped answering health probes"))
            }
            Err(failure) => Some(member_failed_error(&failure)),
        };
        if let Some(error) = failure {
            health.streak.remove(&receiver);
            health.failed.insert(receiver);
            push_update(
                updates,
                SessionUpdate::MemberFailed {
                    generation: gate.current(),
                    receiver,
                    primary: primary == Some(receiver),
                    error,
                },
            );
        }
    }
}

/// Renders one member-local failure as a pre-redacted presentation summary.
fn member_failed_error(failure: &MemberFailure) -> UserFacingError {
    UserFacingError::new(format!(
        "receiver rejected {:?} request",
        snapshot_phase(failure.phase)
    ))
}

/// Renders one refused control request as a pre-redacted summary.
///
/// Worded as the control-plane event it is, so the diagnostic feed cannot be
/// mistaken for the member failure this path deliberately does not raise.
fn control_failed_error(failure: &MemberFailure) -> UserFacingError {
    UserFacingError::new(format!(
        "receiver refused a {:?} control request; it keeps streaming",
        snapshot_phase(failure.phase)
    ))
}

/// Applies one non-reconcile request against the current state.
#[allow(clippy::too_many_arguments)]
async fn apply_inline_request(
    request: SessionRequest,
    gate: &mut GenerationGate,
    in_flight: &mut Option<StartAttempt>,
    active: &mut Option<Box<dyn SessionTransport>>,
    retained: &mut Option<ReconcileParams>,
    health: &mut HealthState,
    updates: &mpsc::Sender<SessionUpdate>,
    factory: &Arc<dyn SessionTransportFactory>,
    decoders: &Arc<dyn SessionDecoderSource>,
    timer: &Arc<dyn Timer>,
    volumes: &EffectiveVolumeState,
    diagnostics: &watch::Sender<SessionDiagnosticsState>,
) {
    match request {
        SessionRequest::Reconcile { .. } => {
            // Older arrivals are dropped; the newest is dispatched separately.
        }
        SessionRequest::SetVolume { receiver, .. } => {
            // `try_send` records the newest value synchronously before this
            // bounded queue is touched. A stale dequeued request must neither
            // replace that state nor forward its obsolete payload live.
            let volume = volumes.get(receiver);
            if let Some(transport) = active.as_mut() {
                if !transport.active_members().contains(&receiver) {
                    return;
                }
                let result = transport.set_member_volume(&receiver, volume).await;
                if let Err(failure) = result.result {
                    // Control-plane only: the member keeps its place in the
                    // session and the group keeps streaming. See the variant's
                    // documentation for why this must not become a member
                    // failure, on the primary least of all.
                    push_update(
                        updates,
                        SessionUpdate::MemberControlFailed {
                            generation: gate.current(),
                            receiver,
                            error: control_failed_error(&failure),
                        },
                    );
                }
            }
        }
        SessionRequest::Probe => {
            run_probe_cycle(active, health, gate, updates).await;
        }
        SessionRequest::NetworkChanged => {
            rebuild_for_cause(
                RestartReason::LocalInterfaceChanged,
                gate,
                in_flight,
                active,
                retained,
                health,
                updates,
                factory,
                decoders,
                timer,
                volumes,
                diagnostics,
            )
            .await;
        }
        SessionRequest::Suspend => {
            let had_session = in_flight.is_some() || active.is_some();
            if let Some(attempt) = in_flight.take() {
                // Unlike `supersede`, this abort is followed by a sleep, so
                // the attempt has to be provably over before the branch
                // returns -- not merely scheduled to die.
                attempt.join.abort();
                bounded_join(Arc::clone(timer), attempt.join).await;
            }
            if let Some(mut transport) = active.take() {
                publish_inactive_diagnostics(diagnostics, SessionStopReason::Restarted);
                let _ = stop_bounded(Arc::clone(timer), transport.as_mut()).await;
            }
            health.reset();
            // Desired membership stays retained so resume can rebuild.
            if had_session {
                push_update(
                    updates,
                    SessionUpdate::Stopped {
                        generation: gate.current(),
                    },
                );
            }
        }
        SessionRequest::Resume => {
            rebuild_for_cause(
                RestartReason::SystemResume,
                gate,
                in_flight,
                active,
                retained,
                health,
                updates,
                factory,
                decoders,
                timer,
                volumes,
                diagnostics,
            )
            .await;
        }
    }
}

enum SupervisorStep {
    Adopt(u64, Result<StartOutcome, oneshot::error::RecvError>),
    Request(Option<SessionRequest>),
    /// The independent per-member health probe is due.
    Probe,
}

enum ActivationStep {
    Ready(Result<(), UserFacingError>),
    Superseded(Option<SessionRequest>),
    Shutdown,
}

/// Keeps the prepared transport cancellable while its final volume I/O waits.
/// Volume values are already stored synchronously by the producer; consuming
/// their queue notifications here cannot bypass activation's final revalidation.
async fn activate_prepared(
    transport: &mut dyn SessionTransport,
    requests: &mut mpsc::Receiver<SessionRequest>,
    cancel: &CancellationToken,
    generation: u64,
) -> ActivationStep {
    let activation = transport.activate();
    let deadline = tokio::time::sleep(TEARDOWN_GROUP);
    tokio::pin!(activation, deadline);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return ActivationStep::Shutdown,
            _ = &mut deadline => return ActivationStep::Ready(Err(UserFacingError::new("safe audio activation timed out"))),
            request = requests.recv() => match request {
                Some(SessionRequest::SetVolume { .. } | SessionRequest::Probe) => {},
                Some(SessionRequest::Reconcile { generation: newer, .. }) if newer <= generation => {},
                request => return ActivationStep::Superseded(request),
            },
            result = &mut activation => return ActivationStep::Ready(result),
        }
    }
}

/// Supervisor main loop: waits on cancellation, in-flight starts, and new
/// requests; adopts completions tagged with the accepted generation only.
// This is the single wiring boundary for the supervisor's owned channels and
// runtime services; grouping them would only hide the lifecycle dependencies.
#[allow(clippy::too_many_arguments)]
async fn run_supervisor(
    factory: Arc<dyn SessionTransportFactory>,
    decoders: Arc<dyn SessionDecoderSource>,
    config: SessionConfig,
    mut requests_rx: mpsc::Receiver<SessionRequest>,
    updates_tx: mpsc::Sender<SessionUpdate>,
    cancel: CancellationToken,
    volumes: EffectiveVolumeState,
    diagnostics: watch::Sender<SessionDiagnosticsState>,
) {
    let timer: Arc<dyn Timer> = Arc::new(TokioTimer);
    let mut gate = GenerationGate::default();
    let mut in_flight: Option<StartAttempt> = None;
    let mut active: Option<Box<dyn SessionTransport>> = None;
    let mut retained: Option<ReconcileParams> = None;
    let mut health = HealthState::default();
    // Probing is deadline-driven and the deadline is re-anchored AFTER each
    // cycle, so a cycle that outruns the interval still leaves a full interval
    // of slack before the next one. Anchoring it before the cycle would make
    // the probe arm permanently ready and, under `biased`, starve every other
    // arm. Requests are polled ahead of probes for the same reason: they are
    // edge-triggered and finite, while probes regenerate themselves forever.
    let mut next_probe = tokio::time::Instant::now() + config.probe_interval;
    let mut pending_step = None;

    'supervise: loop {
        if cancel.is_cancelled() {
            break;
        }
        let step = if let Some(step) = pending_step.take() {
            step
        } else if in_flight.is_some() {
            // Cancel-safe wait: the oneshot keeps the outcome if another arm
            // wins, so the attempt is restored intact for the next round.
            let mut attempt = in_flight.take().expect("checked above");
            let generation = attempt.generation;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    in_flight = Some(attempt);
                    break 'supervise;
                }
                outcome = &mut attempt.outcome => SupervisorStep::Adopt(generation, outcome),
                request = requests_rx.recv() => {
                    in_flight = Some(attempt);
                    SupervisorStep::Request(request)
                }
                () = tokio::time::sleep_until(next_probe) => {
                    in_flight = Some(attempt);
                    SupervisorStep::Probe
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break 'supervise,
                request = requests_rx.recv() => SupervisorStep::Request(request),
                () = tokio::time::sleep_until(next_probe) => SupervisorStep::Probe,
            }
        };

        match step {
            SupervisorStep::Probe => {
                run_probe_cycle(&mut active, &mut health, &gate, &updates_tx).await;
                next_probe = tokio::time::Instant::now() + config.probe_interval;
            }
            SupervisorStep::Adopt(generation, outcome) => {
                in_flight = None;
                match outcome {
                    Ok(Ok(mut transport)) if generation == gate.current() => {
                        let activation = match activate_prepared(
                            transport.as_mut(),
                            &mut requests_rx,
                            &cancel,
                            generation,
                        )
                        .await
                        {
                            ActivationStep::Shutdown => {
                                let _ = stop_bounded(Arc::clone(&timer), transport.as_mut()).await;
                                break 'supervise;
                            }
                            ActivationStep::Superseded(request) => {
                                // Drop the activation future before bounded
                                // teardown, then replay the request through the
                                // existing coalescing/generation path.
                                let _ = stop_bounded(Arc::clone(&timer), transport.as_mut()).await;
                                pending_step = Some(SupervisorStep::Request(request));
                                continue;
                            }
                            ActivationStep::Ready(result) => result,
                        };
                        if let Err(error) = activation {
                            let _ = stop_bounded(Arc::clone(&timer), transport.as_mut()).await;
                            tracing::warn!("safe audio activation failed: {error}");
                            push_update(
                                &updates_tx,
                                SessionUpdate::Recovering {
                                    generation,
                                    reason: RestartReason::DeadRtspRecovered,
                                    attempt: 0,
                                },
                            );
                            continue;
                        }
                        let primary = transport.primary();
                        let members = transport.active_members();
                        let failures = transport.setup_failures();
                        match primary {
                            Some(primary_id) => {
                                if let Some(source) = transport.diagnostics_source() {
                                    publish_active_diagnostics(
                                        &diagnostics,
                                        generation,
                                        primary_id,
                                        &members,
                                        source,
                                    );
                                }
                                push_update(
                                    &updates_tx,
                                    SessionUpdate::Active {
                                        generation,
                                        primary: primary_id,
                                        members,
                                        partial_failures: failures,
                                    },
                                );
                                // A send error observed during setup arms an
                                // immediate probe instead of failing outright.
                                if drain_runtime_events(transport.as_mut(), &mut health.urgent) {
                                    next_probe = tokio::time::Instant::now();
                                }
                                active = Some(transport);
                            }
                            None => {
                                // Contract violation: success without a
                                // primary cannot hold a session.
                                let _ = stop_bounded(Arc::clone(&timer), transport.as_mut()).await;
                                push_update(
                                    &updates_tx,
                                    SessionUpdate::Recovering {
                                        generation,
                                        reason: RestartReason::DeadRtspRecovered,
                                        attempt: 0,
                                    },
                                );
                            }
                        }
                    }
                    Ok(Ok(mut stale_transport)) => {
                        // Stale completion: ignored and immediately cleaned up.
                        let _ = stop_bounded(Arc::clone(&timer), stale_transport.as_mut()).await;
                    }
                    Ok(Err(failure)) if generation == gate.current() => {
                        push_update(
                            &updates_tx,
                            SessionUpdate::Recovering {
                                generation,
                                reason: RestartReason::DeadRtspRecovered,
                                attempt: 0,
                            },
                        );
                        tracing::warn!(
                            "session generation {generation} failed best-effort start: {}",
                            failure.error
                        );
                    }
                    // Failed attempts of superseded generations vanish.
                    _ => {}
                }
            }
            SupervisorStep::Request(request) => {
                let Some(request) = request else {
                    break 'supervise; // all producer handles dropped
                };
                let (inline, newest_reconcile) = coalesce(&mut requests_rx, request);
                for request in inline {
                    apply_inline_request(
                        request,
                        &mut gate,
                        &mut in_flight,
                        &mut active,
                        &mut retained,
                        &mut health,
                        &updates_tx,
                        &factory,
                        &decoders,
                        &timer,
                        &volumes,
                        &diagnostics,
                    )
                    .await;
                }
                if let Some(params) = newest_reconcile {
                    start_generation(
                        params,
                        &mut gate,
                        &mut in_flight,
                        &mut active,
                        &mut retained,
                        &mut health,
                        &updates_tx,
                        &factory,
                        &decoders,
                        &timer,
                        &volumes,
                        &diagnostics,
                    )
                    .await;
                }
            }
        }
    }

    // Bounded final teardown: no leaked tasks or transports may survive.
    if let Some(attempt) = in_flight.take() {
        attempt.join.abort();
    }
    if let Some(mut transport) = active.take() {
        publish_inactive_diagnostics(&diagnostics, SessionStopReason::Stopped);
        let _ = stop_bounded(timer, transport.as_mut()).await;
    }
}

/// Folds the still-queued requests around the popped one: the newest
/// reconcile wins outright (older ones are dropped as stale), while
/// non-reconcile requests execute first in arrival order.
fn coalesce(
    rx: &mut mpsc::Receiver<SessionRequest>,
    first: SessionRequest,
) -> (Vec<SessionRequest>, Option<ReconcileParams>) {
    let mut pending = vec![first];
    while let Ok(next) = rx.try_recv() {
        pending.push(next);
    }
    let mut inline = Vec::new();
    let mut newest: Option<ReconcileParams> = None;
    for request in pending {
        match request {
            SessionRequest::Reconcile {
                generation,
                desired,
                preferred_primary,
                latency,
                calibration,
                teardown_only,
            } => {
                let supersedes = newest
                    .as_ref()
                    .is_none_or(|current| generation > current.generation);
                if supersedes {
                    newest = Some(ReconcileParams {
                        generation,
                        desired,
                        preferred_primary,
                        latency,
                        calibration,
                        teardown_only,
                    });
                }
            }
            other => inline.push(other),
        }
    }
    (inline, newest)
}

/// Placeholder used by tests to keep an unresolved future value handy.
#[cfg(test)]
fn never<T>() -> Pin<Box<dyn Future<Output = T> + Send>>
where
    T: Send + 'static,
{
    Box::pin(pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(seed: u8) -> ReceiverId {
        ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
            .expect("fixed seed forms a valid storage key")
    }

    #[test]
    fn restart_classifier_matches_the_contract() {
        let cases = [
            (ReconfigureCause::Membership, true),
            (ReconfigureCause::PrimaryFailure, true),
            (ReconfigureCause::LatencyChanged, true),
            (ReconfigureCause::ProtocolDesync, true),
            (ReconfigureCause::NetworkRebind, true),
            (ReconfigureCause::ReceiverAddressChanged, true),
            (ReconfigureCause::TimingModeChanged, true),
            (ReconfigureCause::StreamFormatChanged, true),
            (ReconfigureCause::Resume, true),
            (ReconfigureCause::SecondaryRejoin, true),
            (ReconfigureCause::RemoveFailedTarget, true),
            (ReconfigureCause::CalibrationChanged, true),
            (ReconfigureCause::MasterVolume, false),
            (ReconfigureCause::ReceiverVolume, false),
            (ReconfigureCause::Mute, false),
            (ReconfigureCause::CaptureEndpoint, false),
            (ReconfigureCause::SecondaryFailure, false),
            (ReconfigureCause::DiscoveryMetadata, false),
            (ReconfigureCause::SavedGroupEdited, false),
        ];
        assert_eq!(cases.len(), 19, "the matrix has exactly nineteen rows");
        for (cause, restarts) in cases {
            assert_eq!(cause.requires_full_restart(), restarts, "{cause:?}");
            if restarts {
                assert!(cause.restart_reason().is_some(), "{cause:?}");
            } else {
                assert!(cause.restart_reason().is_none(), "{cause:?}");
            }
        }
    }

    mod generation_gate {
        use super::*;

        #[test]
        fn stale_reconcile_generation_is_dropped() {
            let mut gate = GenerationGate::default();
            assert_eq!(gate.current(), 0);
            assert!(gate.admit(5), "the first generation is always admitted");
            assert_eq!(gate.current(), 5);
            // An older reconcile arriving after a newer one is dropped.
            assert!(!gate.admit(4));
            // A duplicate of the accepted generation is stale too.
            assert!(!gate.admit(5));
            assert_eq!(gate.current(), 5, "stale arrivals never move the gate");
            assert!(gate.admit(6));
            assert!(!gate.admit(5));
            assert_eq!(gate.current(), 6);
        }

        #[test]
        fn volume_request_does_not_bump_generation() {
            let mut gate = GenerationGate::default();
            assert!(gate.admit(3));
            let request = SessionRequest::SetVolume {
                receiver: rid(1),
                volume: 0.5,
            };
            assert!(!request.affects_session_generation());
            match request {
                SessionRequest::SetVolume { .. } => {
                    // Forwarded inline to the active transport; no start.
                }
                other => panic!("unexpected classification: {other:?}"),
            }
            assert_eq!(gate.current(), 3, "volume must not bump the generation");
            // The same generation stays authoritative afterwards.
            assert!(!gate.admit(3));
            assert_eq!(gate.current(), 3);
        }

        #[tokio::test]
        async fn stale_queued_volume_cannot_replace_a_newer_muted_setup_value() {
            struct RefusingFactory;

            #[async_trait]
            impl SessionTransportFactory for RefusingFactory {
                async fn start(
                    &self,
                    _desired: Vec<airplay_core::Device>,
                    _preferred_primary: Option<ReceiverId>,
                    _decoder: LiveAudioDecoder,
                    _latency: LatencyConfig,
                    _calibration: CalibrationProfile,
                ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
                    Err(SessionStartFailure {
                        error: UserFacingError::new("not used for inline volume handling"),
                        failures: Vec::new(),
                    })
                }
            }

            struct UnusedDecoders;

            impl SessionDecoderSource for UnusedDecoders {
                fn take_decoder(&self) -> LiveAudioDecoder {
                    let (_sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 8);
                    decoder
                }
            }

            struct UnusedTimer;

            impl Timer for UnusedTimer {
                fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    never()
                }
            }

            let (requests_tx, mut requests_rx) = mpsc::channel(1);
            let volumes = EffectiveVolumeState::default();
            let handle = SessionSupervisorHandle {
                requests_tx,
                volumes: volumes.clone(),
            };

            // An old live request occupies the only queue slot. The newer
            // persisted mute gets into the durable source but its live echo
            // cannot be enqueued.
            assert_eq!(
                handle.try_send(SessionRequest::SetVolume {
                    receiver: rid(1),
                    volume: 0.4,
                }),
                Ok(())
            );
            handle.remember_volume(rid(1), 0.0);
            assert_eq!(volumes.get(rid(1)), 0.0);
            assert_eq!(
                handle.try_send(SessionRequest::SetVolume {
                    receiver: rid(1),
                    volume: 0.0,
                }),
                Err(SessionSendError::Busy)
            );

            let stale = requests_rx
                .try_recv()
                .expect("the old live volume request is queued");
            let mut gate = GenerationGate::default();
            let mut in_flight = None;
            let mut active = None;
            let mut retained = None;
            let mut health = HealthState::default();
            let (updates, _updates_rx) = mpsc::channel(SUPERVISOR_CAPACITY);
            let factory: Arc<dyn SessionTransportFactory> = Arc::new(RefusingFactory);
            let decoders: Arc<dyn SessionDecoderSource> = Arc::new(UnusedDecoders);
            let timer: Arc<dyn Timer> = Arc::new(UnusedTimer);
            let (diagnostics, _diagnostics_rx) = watch::channel(SessionDiagnosticsState::default());
            apply_inline_request(
                stale,
                &mut gate,
                &mut in_flight,
                &mut active,
                &mut retained,
                &mut health,
                &updates,
                &factory,
                &decoders,
                &timer,
                &volumes,
                &diagnostics,
            )
            .await;

            assert!(matches!(
                handle.try_send(SessionRequest::Reconcile {
                    generation: 1,
                    desired: Vec::new(),
                    preferred_primary: None,
                    latency: LatencyConfig {
                        sender_buffer_ms: 2_000,
                        render_delay_ms: 200,
                    },
                    calibration: CalibrationProfile::default(),
                    teardown_only: false,
                }),
                Ok(())
            ));
            assert_eq!(
                volumes.get(rid(1)),
                0.0,
                "a stale dequeued control request cannot replace the newer muted setup value"
            );
        }
    }

    mod teardown_budgets {
        use super::*;

        /// Timer double completing instantly; records requested durations.
        #[derive(Default)]
        struct InstantTimer {
            recorded: std::sync::Mutex<Vec<Duration>>,
        }

        impl InstantTimer {
            fn recorded(&self) -> Vec<Duration> {
                self.recorded.lock().expect("timer mutex poisoned").clone()
            }
        }

        impl Timer for InstantTimer {
            fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                self.recorded
                    .lock()
                    .expect("timer mutex poisoned")
                    .push(duration);
                Box::pin(std::future::ready(()))
            }
        }

        /// Timer double that never resolves; models undisturbed time.
        #[derive(Default)]
        struct FrozenTimer;

        impl Timer for FrozenTimer {
            fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                never()
            }
        }

        #[test]
        fn teardown_deadlines_match_the_spec_budgets() {
            let now = Instant::now();
            let (per_receiver, group) = teardown_deadlines(now);
            assert_eq!(per_receiver, now + TEARDOWN_PER_RECEIVER);
            assert_eq!(group, now + TEARDOWN_GROUP);
            assert_eq!(TEARDOWN_PER_RECEIVER, Duration::from_secs(2));
            assert_eq!(TEARDOWN_GROUP, Duration::from_secs(4));
            assert!(group > per_receiver, "group budget exceeds per-receiver");
        }

        #[tokio::test]
        async fn bounded_group_teardown_times_out_via_injected_timer() {
            struct WedgedTransport;
            #[async_trait]
            impl SessionTransport for WedgedTransport {
                fn primary(&self) -> Option<ReceiverId> {
                    None
                }
                fn active_members(&self) -> BTreeSet<ReceiverId> {
                    BTreeSet::new()
                }
                fn setup_failures(&mut self) -> Vec<MemberFailure> {
                    Vec::new()
                }
                fn try_runtime_event(
                    &mut self,
                ) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag> {
                    Ok(None)
                }
                async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
                    Vec::new()
                }
                async fn set_member_volume(
                    &mut self,
                    _receiver: &ReceiverId,
                    _volume: f32,
                ) -> MemberResult<()> {
                    MemberResult {
                        receiver: airplay_core::DeviceId([0; 6]),
                        result: Ok(()),
                    }
                }
                async fn stop(&mut self) -> anyhow::Result<()> {
                    never::<anyhow::Result<()>>().await
                }
            }

            let timer = Arc::new(InstantTimer::default());
            let mut transport = WedgedTransport;
            let outcome = stop_bounded(timer.clone(), &mut transport).await;
            assert_eq!(outcome, Err(SessionStopError::Timeout));
            // The four-second budget was requested through the seam, not slept.
            assert_eq!(timer.recorded(), vec![TEARDOWN_GROUP]);
        }

        #[tokio::test]
        async fn bounded_group_teardown_passes_when_stop_finishes_first() {
            struct QuickTransport;
            #[async_trait]
            impl SessionTransport for QuickTransport {
                fn primary(&self) -> Option<ReceiverId> {
                    None
                }
                fn active_members(&self) -> BTreeSet<ReceiverId> {
                    BTreeSet::new()
                }
                fn setup_failures(&mut self) -> Vec<MemberFailure> {
                    Vec::new()
                }
                fn try_runtime_event(
                    &mut self,
                ) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag> {
                    Ok(None)
                }
                async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
                    Vec::new()
                }
                async fn set_member_volume(
                    &mut self,
                    _receiver: &ReceiverId,
                    _volume: f32,
                ) -> MemberResult<()> {
                    MemberResult {
                        receiver: airplay_core::DeviceId([0; 6]),
                        result: Ok(()),
                    }
                }
                async fn stop(&mut self) -> anyhow::Result<()> {
                    Ok(())
                }
            }

            let mut transport = QuickTransport;
            let outcome = stop_bounded(Arc::new(FrozenTimer), &mut transport).await;
            assert_eq!(outcome, Ok(()));
        }

        /// The suspend path aborts an in-flight start and must not return
        /// before that start is actually over: between the abort and the
        /// task's next yield point lies the system sleep, and a start task
        /// that survives it carries pre-sleep sockets into a network stack
        /// that no longer exists.
        ///
        /// The frozen timer removes the budget as an escape hatch, so the only
        /// way this can return is by having awaited the handle.
        #[tokio::test]
        async fn bounded_join_returns_only_once_the_aborted_start_is_over() {
            let guard = Arc::new(());
            let carried = Arc::clone(&guard);
            let join = tokio::spawn(async move {
                let _carried = carried;
                never::<()>().await;
            });
            // Let the task reach its await, so the abort interrupts a parked
            // start rather than an unpolled one.
            tokio::task::yield_now().await;
            assert_eq!(
                Arc::strong_count(&guard),
                2,
                "the start task should still hold its resources here"
            );

            join.abort();
            bounded_join(Arc::new(FrozenTimer), join).await;

            assert_eq!(
                Arc::strong_count(&guard),
                1,
                "the suspend returned while the aborted start still held its transport"
            );
        }

        /// A start that refuses to die must not hold the suspend hostage
        /// either: the same four-second group budget applies, requested
        /// through the injected seam rather than slept.
        #[tokio::test]
        async fn bounded_join_gives_up_after_the_group_budget() {
            // Never aborted, so only the budget can end this wait.
            let join = tokio::spawn(async { never::<()>().await });
            let timer = Arc::new(InstantTimer::default());

            bounded_join(timer.clone(), join).await;

            assert_eq!(timer.recorded(), vec![TEARDOWN_GROUP]);
        }
    }

    mod probe_policy {
        use super::*;
        use std::collections::VecDeque;

        /// One scripted probe answer per member and cycle.
        #[derive(Clone, Copy)]
        enum Answer {
            Healthy,
            TimedOut,
            /// Hard RTSP/protocol close.
            Hard,
        }

        struct ProbeTransport {
            members: Vec<u8>,
            script: VecDeque<Vec<Answer>>,
            events: VecDeque<GroupRuntimeEvent>,
        }

        fn device_id(seed: u8) -> airplay_core::DeviceId {
            airplay_core::DeviceId([0, 0, 0, 0, 0, seed])
        }

        #[async_trait]
        impl SessionTransport for ProbeTransport {
            fn primary(&self) -> Option<ReceiverId> {
                self.members.first().copied().map(rid)
            }
            fn active_members(&self) -> BTreeSet<ReceiverId> {
                self.members.iter().copied().map(rid).collect()
            }
            fn setup_failures(&mut self) -> Vec<MemberFailure> {
                Vec::new()
            }
            fn try_runtime_event(&mut self) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag> {
                Ok(self.events.pop_front())
            }
            async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
                let answers = self
                    .script
                    .pop_front()
                    .unwrap_or_else(|| vec![Answer::Healthy; self.members.len()]);
                self.members
                    .iter()
                    .zip(answers)
                    .map(|(seed, answer)| MemberResult {
                        receiver: device_id(*seed),
                        result: match answer {
                            Answer::Healthy => Ok(Health::Healthy),
                            Answer::TimedOut => Ok(Health::TimedOut),
                            Answer::Hard => Err(MemberFailure {
                                receiver: device_id(*seed),
                                phase: airplay_client::SetupPhase::RtspSetup,
                                retryable: true,
                                source: airplay_core::Error::Rtsp(
                                    airplay_core::error::RtspError::SetupFailed(
                                        "scripted hard close".into(),
                                    ),
                                ),
                            }),
                        },
                    })
                    .collect()
            }
            async fn set_member_volume(
                &mut self,
                receiver: &ReceiverId,
                _volume: f32,
            ) -> MemberResult<()> {
                MemberResult {
                    receiver: (*receiver).into(),
                    result: Ok(()),
                }
            }
            async fn stop(&mut self) -> anyhow::Result<()> {
                Ok(())
            }
        }

        /// Rig running probe cycles against a scripted transport.
        struct ProbeRig {
            active: Option<Box<dyn SessionTransport>>,
            health: HealthState,
            gate: GenerationGate,
            updates: mpsc::Sender<SessionUpdate>,
            inbox: mpsc::Receiver<SessionUpdate>,
        }

        impl ProbeRig {
            fn new(
                members: &[u8],
                script: Vec<Vec<Answer>>,
                events: Vec<GroupRuntimeEvent>,
            ) -> Self {
                let (updates, inbox) = mpsc::channel(SUPERVISOR_CAPACITY);
                let mut gate = GenerationGate::default();
                assert!(gate.admit(1));
                Self {
                    active: Some(Box::new(ProbeTransport {
                        members: members.to_vec(),
                        script: script.into(),
                        events: events.into(),
                    })),
                    health: HealthState::default(),
                    gate,
                    updates,
                    inbox,
                }
            }

            async fn cycle(&mut self) {
                run_probe_cycle(
                    &mut self.active,
                    &mut self.health,
                    &self.gate,
                    &self.updates,
                )
                .await;
            }

            /// Receivers reported failed so far, in emission order.
            fn failures(&mut self) -> Vec<ReceiverId> {
                let mut failed = Vec::new();
                while let Ok(update) = self.inbox.try_recv() {
                    if let SessionUpdate::MemberFailed { receiver, .. } = update {
                        failed.push(receiver);
                    }
                }
                failed
            }
        }

        #[tokio::test]
        async fn a_single_timeout_never_fails_and_healthy_resets_the_streak() {
            let mut rig = ProbeRig::new(
                &[1],
                vec![
                    vec![Answer::TimedOut],
                    vec![Answer::Healthy],
                    vec![Answer::TimedOut],
                ],
                Vec::new(),
            );
            for _ in 0..3 {
                rig.cycle().await;
            }
            assert!(
                rig.failures().is_empty(),
                "a healthy answer between two timeouts resets the streak"
            );
        }

        #[tokio::test]
        async fn two_consecutive_timeouts_fail_the_member_exactly_once() {
            let mut rig = ProbeRig::new(
                &[1, 2],
                vec![
                    vec![Answer::Healthy, Answer::TimedOut],
                    vec![Answer::Healthy, Answer::TimedOut],
                    vec![Answer::Healthy, Answer::TimedOut],
                ],
                Vec::new(),
            );
            rig.cycle().await;
            assert!(rig.failures().is_empty(), "one timeout is not a failure");
            rig.cycle().await;
            assert_eq!(rig.failures(), vec![rid(2)]);
            rig.cycle().await;
            assert!(
                rig.failures().is_empty(),
                "an already failed member is not reported again"
            );
        }

        #[tokio::test]
        async fn hard_protocol_close_fails_immediately() {
            let mut rig = ProbeRig::new(
                &[1, 2],
                vec![vec![Answer::Healthy, Answer::Hard]],
                Vec::new(),
            );
            rig.cycle().await;
            assert_eq!(rig.failures(), vec![rid(2)]);
        }

        #[tokio::test]
        async fn udp_send_error_alone_is_no_failure_but_arms_the_next_probe() {
            // First cycle: send error plus a healthy answer -> still healthy.
            let mut rig = ProbeRig::new(
                &[1, 2],
                vec![
                    vec![Answer::Healthy, Answer::Healthy],
                    vec![Answer::Healthy, Answer::TimedOut],
                ],
                vec![GroupRuntimeEvent::TargetSendFailed {
                    receiver: device_id(2),
                    source: "scripted send error".to_owned(),
                }],
            );
            rig.cycle().await;
            assert!(
                rig.failures().is_empty(),
                "a send error is never a failure by itself"
            );

            // Second cycle: the arming is consumed, so this behaves like an
            // ordinary first timeout again.
            rig.cycle().await;
            assert!(rig.failures().is_empty());
        }

        #[tokio::test]
        async fn an_armed_receiver_fails_on_its_first_timing_out_probe() {
            let mut rig = ProbeRig::new(
                &[1, 2],
                vec![vec![Answer::Healthy, Answer::TimedOut]],
                vec![GroupRuntimeEvent::TargetSendFailed {
                    receiver: device_id(2),
                    source: "scripted send error".to_owned(),
                }],
            );
            rig.cycle().await;
            assert_eq!(
                rig.failures(),
                vec![rid(2)],
                "the probe triggered by the send error decides, not the send"
            );
        }
    }

    mod snapshot_phase_mapping {
        use super::*;

        #[test]
        fn maps_every_upstream_variant_exhaustively() {
            for phase in airplay_client::SetupPhase::ALL {
                let mapped = snapshot_phase(phase);
                // Identical names imply identical discriminant order; verify
                // each upstream variant lands on its same-named counterpart.
                let expected = match phase {
                    airplay_client::SetupPhase::Connect => SnapshotSetupPhase::Connect,
                    airplay_client::SetupPhase::Pair => SnapshotSetupPhase::Pair,
                    airplay_client::SetupPhase::PrimaryTiming => SnapshotSetupPhase::PrimaryTiming,
                    airplay_client::SetupPhase::RtspSetup => SnapshotSetupPhase::RtspSetup,
                    airplay_client::SetupPhase::SetPeers => SnapshotSetupPhase::SetPeers,
                    airplay_client::SetupPhase::BuildSender => SnapshotSetupPhase::BuildSender,
                    airplay_client::SetupPhase::StartAudio => SnapshotSetupPhase::StartAudio,
                };
                assert_eq!(mapped, expected, "{phase:?}");
            }
        }
    }

    /// An empty desired set is the absence of a session, not a session that
    /// failed to come up.
    mod empty_desired_set {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Factory that counts every start it is asked for and refuses it the
        /// way the real one refuses an unreachable group.
        #[derive(Default)]
        struct CountingFactory {
            starts: AtomicUsize,
        }

        #[async_trait]
        impl SessionTransportFactory for CountingFactory {
            async fn start(
                &self,
                _desired: Vec<airplay_core::Device>,
                _preferred_primary: Option<ReceiverId>,
                _decoder: LiveAudioDecoder,
                _latency: LatencyConfig,
                _calibration: CalibrationProfile,
            ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
                self.starts.fetch_add(1, Ordering::SeqCst);
                Err(SessionStartFailure {
                    error: UserFacingError::new("no selected receiver could be reached"),
                    failures: Vec::new(),
                })
            }
        }

        /// Timer double for a path that never sleeps: nothing is active, so
        /// no teardown budget can be requested here.
        struct NoopTimer;

        impl Timer for NoopTimer {
            fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(std::future::ready(()))
            }
        }

        struct ThrowawayDecoders;

        impl SessionDecoderSource for ThrowawayDecoders {
            fn take_decoder(&self) -> LiveAudioDecoder {
                let (_sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 8);
                decoder
            }
        }

        fn params(generation: u64, teardown_only: bool) -> ReconcileParams {
            ReconcileParams {
                generation,
                desired: Vec::new(),
                preferred_primary: None,
                latency: LatencyConfig {
                    sender_buffer_ms: 2_000,
                    render_delay_ms: 200,
                },
                calibration: CalibrationProfile::default(),
                teardown_only,
            }
        }

        /// Drives one empty generation and returns the single update it made.
        async fn announce_of(params: ReconcileParams) -> (SessionUpdate, usize) {
            let factory: Arc<CountingFactory> = Arc::new(CountingFactory::default());
            let as_factory: Arc<dyn SessionTransportFactory> = factory.clone();
            let decoders: Arc<dyn SessionDecoderSource> = Arc::new(ThrowawayDecoders);
            let timer: Arc<dyn Timer> = Arc::new(NoopTimer);
            let (updates, mut inbox) = mpsc::channel(SUPERVISOR_CAPACITY);
            let (diagnostics, _diagnostics_rx) = watch::channel(SessionDiagnosticsState::default());
            let mut gate = GenerationGate::default();
            let mut in_flight = None;
            let mut active: Option<Box<dyn SessionTransport>> = None;
            let mut retained = None;
            let mut health = HealthState::default();

            start_generation(
                params,
                &mut gate,
                &mut in_flight,
                &mut active,
                &mut retained,
                &mut health,
                &updates,
                &as_factory,
                &decoders,
                &timer,
                &EffectiveVolumeState::default(),
                &diagnostics,
            )
            .await;

            assert!(
                in_flight.is_none(),
                "an empty desired set left a start attempt in flight"
            );
            let update = inbox.try_recv().expect("the generation announces itself");
            assert!(
                inbox.try_recv().is_err(),
                "an empty desired set announced more than one update"
            );
            (update, factory.starts.load(Ordering::SeqCst))
        }

        #[tokio::test]
        async fn a_teardown_only_generation_stops_instead_of_starting() {
            let (update, starts) = announce_of(params(1, true)).await;

            assert_eq!(
                starts, 0,
                "an empty desired set was handed to the transport factory"
            );
            assert!(
                matches!(update, SessionUpdate::Stopped { generation: 1 }),
                "a teardown-only generation announced {update:?} instead of Stopped"
            );
        }

        /// An empty set that is *not* a teardown is the app still wanting to
        /// run while every desired receiver is out (backoff, or not yet
        /// castable). Announcing `Stopped` there would publish
        /// `SessionPhase::Stopped` under `RunIntent::Running` -- a combination
        /// `derive_phase` never produces -- and would erase the only signal
        /// that says "the app is still trying".
        #[tokio::test]
        async fn an_empty_generation_that_still_wants_to_run_announces_recovery() {
            let (update, starts) = announce_of(params(1, false)).await;

            assert_eq!(
                starts, 0,
                "an empty desired set was handed to the transport factory"
            );
            assert!(
                matches!(
                    update,
                    SessionUpdate::Recovering {
                        generation: 1,
                        reason: RestartReason::DeadRtspRecovered,
                        ..
                    }
                ),
                "an empty generation under a running intent announced {update:?}, not Recovering"
            );
        }
    }
}
