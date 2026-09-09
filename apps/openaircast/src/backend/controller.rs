//! Single-owner device backend controller (Task 15).
//!
//! [`start_device_backend`] spawns one named control thread running a
//! current-thread Tokio runtime. The controller is the only mutator of device
//! domain state: it consumes bounded command envelopes, coalesces scalars
//! (newest wins; receiver levels independently per receiver) while executing
//! edge commands strictly in arrival order, persists every accepted durable
//! intent through the crash-safe store BEFORE publishing, and publishes
//! immutable `Arc<DeviceSnapshot>` revisions through a watch channel.
//!
//! Supervisor wiring: each supervisor lives behind an
//! `Arc<tokio::sync::Mutex<_>>`, and its bounded stream of generation-tagged
//! updates is taken out of the handle once at startup, so the actor owns the
//! receiving end outright and feeds it straight into its select loop. The
//! actor is then the only taker of those mutexes, and no wait for an update
//! ever happens under one.
//!
//! Wiring status: every seam runs over its production implementation —
//! discovery over the real mDNS browser, capture over the real WASAPI surface,
//! persistence over [`FileStore`], and the session transport over
//! [`AirPlaySessionTransportFactory`], which drives
//! `airplay_client::connect_group_best_effort` and streams the capture
//! supervisor's stable decoder pair. Tests inject fakes for every seam through
//! the doc-hidden [`BackendOverrides`] bundle.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use airplay_client::MemberFailure;
use airplay_core::Device;
use airplay_discovery::{BrowseStream, Discovery as BrowserDiscovery};
use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::backend::capture::{
    CaptureConfig, CaptureControl, CaptureSource, CaptureSupervisor, CaptureUpdate,
    LiveDecoderFactory, CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE,
};
use crate::backend::command::{
    BackendCommand, CommandEnvelope, DeviceBackendHandle, LifecycleGuard, SystemEvent,
    COMMAND_CAPACITY,
};
use crate::backend::discovery::{
    DiscoveryConfig, DiscoveryControl, DiscoverySource, DiscoverySupervisor, DiscoveryUpdate,
    Timer, TokioTimer,
};
use crate::backend::event::{
    BackendEvent, DeviceBackendUpdates, DiagnosticCategory, DiagnosticEvent,
    DiagnosticEventReceiver, DiagnosticKind, DiagnosticPayload, ErrorScope, GenerationCell,
    Generations, NoticeCode, Severity, SystemTransition, BACKEND_EVENT_CAPACITY,
    DIAGNOSTIC_CAPACITY,
};
use crate::backend::model::{
    AudioEndpointPreference, AudioFlow, AudioSourceSnapshot, AudioSourceState, DeviceSnapshot,
    DiscoveryPhase, LatencyConfig, PersistenceSnapshot, ReceiverId, ReceiverLifecycle,
    ReceiverRole, ReceiverSnapshot, RestartReason, RunIntent, SavedGroup, SavedGroupId,
    SavedGroupMember, SavedGroupSnapshot, SessionPhase, UserFacingError, Volume,
};
use crate::backend::network::{
    platform_sources, LocalBindingSource, NetworkChangeSource, NetworkMonitor,
};
use crate::backend::persistence::{
    effective_volume, CalibrationStateV2, FileStore, LoadOutcome, PersistError, PersistedStateV1,
    StateStore,
};
use crate::backend::recovery::{
    Clock, InvalidTransition, ReceiverMachine, RejoinRestartLimiter, RetryPolicy, RetryState,
    SystemClock,
};
use crate::backend::session::{
    snapshot_phase, ReconfigureCause, SessionConfig, SessionDecoderSource, SessionRequest,
    SessionSendError, SessionSupervisor, SessionTransportFactory, SessionUpdate, PROBE_INTERVAL,
};
use crate::backend::transport::AirPlaySessionTransportFactory;
use crate::calibration::{
    calibration_click_pattern, normalize_calibration, CalibrationCommand, CalibrationProfile,
};
use crate::diagnostics::{CalibrationApplyState, SessionDiagnosticsState};

/// What the controller keeps for itself on top of the session teardown it
/// contains: signalling three supervisors and draining three update streams.
const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_secs(1);

/// Total teardown budget honored while draining supervisors at shutdown.
///
/// Strictly larger than the session supervisor's own
/// [`crate::backend::session::TEARDOWN_GROUP`], because it *contains* it: the
/// session's update channel closes only after the supervisor has finished its
/// final bounded `stop`, which may legitimately use that whole budget against
/// an unreachable receiver. The two used to be the same four seconds, so an
/// exit with a wedged receiver was guaranteed to report a timeout and then
/// drop the supervisor -- whose `Drop` aborts the join handle, cutting the
/// TEARDOWN it was in the middle of sending. The exit budget has to be the
/// larger of the two for the bounded disconnect to ever be allowed to finish.
pub const SHUTDOWN_GROUP_BUDGET: Duration =
    crate::backend::session::TEARDOWN_GROUP.saturating_add(SHUTDOWN_JOIN_SLACK);

/// Interface/default-endpoint settle period observed after a system resume.
///
/// Windows hands the session back before the network stack has finished
/// rebinding: reconnecting inside this window reliably fails on addresses that
/// are about to change again.
pub const RESUME_SETTLE: Duration = Duration::from_secs(2);

/// Longest a suspend may last without a resume broadcast before the backend
/// concludes on its own that the machine is awake again.
///
/// The suspend gate must not have a single exit. `PBT_APMRESUMEAUTOMATIC` is
/// not guaranteed to arrive -- Windows drops resume broadcasts in hybrid-sleep
/// and fast-startup scenarios -- and with `SystemEvent::Resumed` as the only
/// way out the backend would sit there wanting to run and refusing to, for the
/// rest of the process's life: every automatic path and every user command
/// that reconciles is gated on the same flag. This deadline is the second
/// exit; passing it does exactly what a resume would have done.
///
/// Across S3/S4 the process is frozen and cannot advance a monotonic clock, so
/// an ordinary sleep never reaches this deadline no matter how long it lasts.
/// A throttled-but-running modern-standby machine can reach it; the cost there
/// is one rebuild that finds the network down, backs off, and is corrected by
/// the real resume -- which is strictly better than a backend that never
/// starts anything again.
pub const SUSPEND_WATCHDOG: Duration = Duration::from_secs(300);

/// How long after launch the persisted auto-connect policy may still act.
///
/// Without an absolute bound the arm is not startup-only at all: it is bound
/// to the first moment some desired receiver becomes stably castable, however
/// late that is. A launch that finds its receivers switched off would keep the
/// window open for the rest of the process, and an unrelated membership edit
/// hours later would spend it. The window is generous enough to cover the
/// realistic case it exists for -- receivers that answer mDNS a few seconds
/// after the app -- and short enough that everything past it is unambiguously
/// a decision made in a running process.
pub const AUTO_CONNECT_WINDOW: Duration = Duration::from_secs(60);

/// Bounded queue carrying platform edges from the network monitor.
const SYSTEM_EVENT_CAPACITY: usize = 16;

/// Poll interval of the test start gate.
const GATE_POLL: Duration = Duration::from_millis(5);
/// Maximum publication cadence for Windows-input capture telemetry.
const CAPTURE_TELEMETRY_PERIOD: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Public configuration and entry points
// ---------------------------------------------------------------------------

/// Filesystem layout used by the backend's durable state.
#[derive(Clone, Debug)]
pub struct BackendConfig {
    /// Directory holding `state-v1.json` (`%APPDATA%\OpenAirCast`).
    pub state_directory: PathBuf,
    /// Legacy `%APPDATA%\HomePodCast\volume.txt` migration source.
    pub legacy_volume_path: PathBuf,
}

impl BackendConfig {
    /// Standard per-user layout rooted at `%APPDATA%`.
    ///
    /// When the variable is unavailable the OS temporary directory is used so
    /// state never silently resolves relative to the working directory.
    pub fn for_current_user() -> Self {
        let root = std::env::var_os("APPDATA")
            .as_deref()
            .map_or_else(std::env::temp_dir, PathBuf::from);
        Self {
            state_directory: root.join("OpenAirCast"),
            legacy_volume_path: root.join("HomePodCast").join("volume.txt"),
        }
    }
}

/// Source of independent browse generations; public mirror of the supervisor's
/// internal seam so integration tests can inject fakes without exposing the
/// crate-private trait hierarchy.
#[doc(hidden)]
#[async_trait]
pub trait BackendDiscoverySource: Send + Sync {
    /// Starts one fallible browse stream; each call owns its own daemon.
    async fn browse(&self) -> Result<BrowseStream, UserFacingError>;
}

/// Asynchronous delay seam behind the controller's rejoin/backoff arm.
///
/// The backend owns a private thread with its own Tokio runtime, so a test's
/// paused Tokio clock cannot govern it. Injecting this seam together with
/// [`Clock`] is therefore the only way to make the two-second discovery window
/// and the thirty-second rejoin spacing exact rather than load-dependent.
#[doc(hidden)]
#[async_trait]
pub trait BackendTimer: Send + Sync {
    /// Completes once `duration` has elapsed on the matching clock.
    async fn sleep(&self, duration: Duration);
}

/// Production delay seam backed by [`tokio::time::sleep`].
#[derive(Clone, Copy, Debug, Default)]
struct TokioBackendTimer;

#[async_trait]
impl BackendTimer for TokioBackendTimer {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Synchronous gate parked on by the controller before it starts consuming
/// commands. Tests close the gate to enqueue complete scenarios first,
/// eliminating scheduler races around coalescing batches.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct TestStartGate {
    open: AtomicBool,
}

impl TestStartGate {
    /// Creates a closed gate.
    pub fn closed() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Opens the gate; the controller proceeds.
    pub fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
}

/// Injection points for the controller's subsystem seams.
///
/// `None` selects the production wiring (real mDNS browse, real WASAPI
/// capture, stubbed AirPlay transport factory, [`FileStore`]). Tests override
/// individual entries with deterministic fakes.
#[doc(hidden)]
#[derive(Default)]
pub struct BackendOverrides {
    /// Replacement discovery source.
    pub discovery: Option<Arc<dyn BackendDiscoverySource>>,
    /// Replacement capture source.
    pub capture: Option<Arc<dyn CaptureSource>>,
    /// Replacement session transport factory.
    pub session_factory: Option<Arc<dyn SessionTransportFactory>>,
    /// Replacement per-generation decoder source.
    pub decoders: Option<Arc<dyn SessionDecoderSource>>,
    /// Replacement durable store.
    pub store: Option<Arc<dyn StateStore>>,
    /// Optional gate keeping the actor from consuming commands.
    pub start_gate: Option<Arc<TestStartGate>>,
    /// Health-probe cadence of the session supervisor; `None` keeps the
    /// production two-second interval.
    pub probe_interval: Option<Duration>,
    /// Monotonic time source behind backoff, discovery stability, and rejoin
    /// spacing; `None` selects [`SystemClock`].
    pub clock: Option<Arc<dyn Clock>>,
    /// Delay seam of the rejoin/backoff arm; `None` selects real sleeps.
    /// Must be driven by the same time source as `clock`. When present it also
    /// governs the discovery and capture supervisors' internal delays, so a
    /// test owns every settle period in the backend rather than only some.
    pub timer: Option<Arc<dyn BackendTimer>>,
    /// Interface-change notification seam; `None` selects the platform source.
    pub network: Option<Arc<dyn NetworkChangeSource>>,
    /// Local-binding sampler paired with `network`; `None` selects the
    /// platform source.
    pub local_bindings: Option<Arc<dyn LocalBindingSource>>,
}

/// Bridges the public [`BackendTimer`] seam onto the supervisors' internal
/// delay trait.
///
/// Without it a test could pin the controller's own deadlines to fake time
/// while the discovery settle period and the capture ladder stayed on the wall
/// clock -- which is exactly the split that makes a suspend/resume test
/// load-dependent.
struct SupervisorTimer(Arc<dyn BackendTimer>);

impl Timer for SupervisorTimer {
    fn sleep(
        &self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move { inner.sleep(duration).await })
    }
}

/// Starts the resilient device backend with production wiring.
pub fn start_device_backend(
    config: BackendConfig,
) -> anyhow::Result<(DeviceBackendHandle, DeviceBackendUpdates)> {
    start_device_backend_with(config, BackendOverrides::default())
}

/// Starts the backend with injected seams (tests and future shells).
pub fn start_device_backend_with(
    config: BackendConfig,
    overrides: BackendOverrides,
) -> anyhow::Result<(DeviceBackendHandle, DeviceBackendUpdates)> {
    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (state_tx, state_rx) = watch::channel(Arc::new(DeviceSnapshot::default()));
    let (events_tx, events_rx) = broadcast::channel(BACKEND_EVENT_CAPACITY);
    let (diagnostics_tx, diagnostics_rx) = broadcast::channel(DIAGNOSTIC_CAPACITY);
    let (session_diagnostics_tx, session_diagnostics_rx) =
        watch::channel(SessionDiagnosticsState::default());
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<anyhow::Result<()>>(1);

    let guard = LifecycleGuard::default();
    let join_guard = guard.clone();
    let thread_events_tx = events_tx.clone();
    let thread_diagnostics_tx = diagnostics_tx.clone();
    let join = std::thread::Builder::new()
        .name("openaircast-backend".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(anyhow::anyhow!("backend runtime failed: {error}")));
                    return;
                }
            };
            let result = runtime.block_on(run_controller(
                config,
                overrides,
                commands_rx,
                state_tx,
                thread_events_tx,
                thread_diagnostics_tx,
                session_diagnostics_tx,
                ready_tx,
            ));
            if let Err(error) = result {
                tracing::error!("device backend stopped with error: {error:#}");
            }
            // Every worker this controller owns was signalled and drained
            // above, inside `SHUTDOWN_GROUP_BUDGET`, so nothing the caller is
            // waiting for is abandoned here. What can remain is a
            // `spawn_blocking` task the AirPlay streamer leaves behind when its
            // sender thread outlives the join budget, and a blocking task
            // cannot be aborted -- so dropping the runtime normally would block
            // this thread forever. That is not a leak but a hang: the
            // `LifecycleGuard` joins this thread when the last handle clone is
            // dropped, and a join that never returns holds the whole process
            // exit open until the shell's exit valve kills it. Releasing the
            // runtime into the background returns at once and without a
            // deadline, so this thread ends and the join completes.
            //
            // `device_service::run_service` and `main.rs --selftest` do exactly
            // this at the same point, for the same reason.
            runtime.shutdown_background();
        })?;

    // Readiness handshake: startup returns only after runtime/store/path
    // validation; failures join the thread and leave no workers behind.
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = join.join();
            return Err(error);
        }
        Err(_) => {
            let _ = join.join();
            anyhow::bail!("backend control thread exited before reporting readiness");
        }
    }

    join_guard.set_join(join);
    Ok((
        DeviceBackendHandle::with_lifecycle(commands_tx, guard),
        DeviceBackendUpdates {
            state: state_rx,
            events: events_rx,
            diagnostics: DiagnosticEventReceiver::new(diagnostics_rx),
            session_diagnostics: session_diagnostics_rx,
        },
    ))
}

// ---------------------------------------------------------------------------
// Production adapters
// ---------------------------------------------------------------------------

/// Wraps the real mDNS service browser in the public source seam.
struct MdnsDiscoverySource(airplay_discovery::ServiceBrowser);

#[async_trait]
impl BackendDiscoverySource for MdnsDiscoverySource {
    async fn browse(&self) -> Result<BrowseStream, UserFacingError> {
        self.0
            .browse()
            .await
            .map_err(|error| UserFacingError::new(format!("mDNS discovery failed: {error}")))
    }
}

/// Adapts the public seam onto the supervisor's internal trait.
struct OverseenDiscoverySource(Arc<dyn BackendDiscoverySource>);

#[async_trait]
impl DiscoverySource for OverseenDiscoverySource {
    async fn browse(&self) -> Result<BrowseStream, UserFacingError> {
        self.0.browse().await
    }
}

// ---------------------------------------------------------------------------
// Controller internals
// ---------------------------------------------------------------------------

/// One receiver record mirrored from discovery events.
struct InventoryEntry {
    name: String,
    castable: bool,
    available: bool,
    device: Option<Device>,
}

/// Why the next reconciliation generation was requested.
#[derive(Clone, Copy)]
enum ReconcileCause {
    Membership,
    SavedGroup,
    LatencyPreset,
    Retry,
    /// The PTP primary was lost; group timing is invalidated.
    PrimaryFailure,
    /// Windows resumed; every pre-suspend transport object is invalid.
    Resume,
    /// An active local binding/address moved.
    NetworkRebind,
    /// A manual calibration profile was applied or reset.
    CalibrationChanged,
}

impl ReconcileCause {
    fn reason(self) -> RestartReason {
        match self {
            Self::Membership => RestartReason::MembershipChange,
            Self::SavedGroup => RestartReason::SavedGroupActivated,
            Self::LatencyPreset => RestartReason::LatencyPresetChanged,
            Self::Retry => RestartReason::SecondaryRejoin,
            Self::PrimaryFailure => RestartReason::PrimaryReplaced,
            Self::Resume => RestartReason::SystemResume,
            Self::NetworkRebind => RestartReason::LocalInterfaceChanged,
            Self::CalibrationChanged => RestartReason::CalibrationChanged,
        }
    }

    /// The full-restart matrix row this cause belongs to.
    fn matrix_row(self) -> ReconfigureCause {
        match self {
            Self::Membership | Self::SavedGroup => ReconfigureCause::Membership,
            Self::LatencyPreset => ReconfigureCause::LatencyChanged,
            Self::Retry => ReconfigureCause::SecondaryRejoin,
            Self::PrimaryFailure => ReconfigureCause::PrimaryFailure,
            Self::Resume => ReconfigureCause::Resume,
            Self::NetworkRebind => ReconfigureCause::NetworkRebind,
            Self::CalibrationChanged => ReconfigureCause::CalibrationChanged,
        }
    }
}

/// Latest-wins scalar slots drained between awaited command receives.
///
/// Scalar commands overwrite their slot so only the newest value survives one
/// drain cycle; receiver levels coalesce independently per identity; edge
/// commands queue in strict arrival order.
#[derive(Default)]
struct PendingBatch {
    desired_members: Option<CommandEnvelope>,
    run_intent: Option<CommandEnvelope>,
    master_volume: Option<CommandEnvelope>,
    master_receipt: Option<tokio::sync::oneshot::Sender<super::command::CommandConfirmation>>,
    muted: Option<CommandEnvelope>,
    endpoint: Option<CommandEnvelope>,
    latency_preset: Option<CommandEnvelope>,
    auto_connect: Option<CommandEnvelope>,
    levels: BTreeMap<ReceiverId, CommandEnvelope>,
    edges: VecDeque<CommandEnvelope>,
    drained_total: u32,
}

impl PendingBatch {
    fn insert(&mut self, envelope: CommandEnvelope) {
        self.drained_total += 1;
        match envelope.command {
            BackendCommand::SetDesiredMembers { .. } => self.desired_members = Some(envelope),
            BackendCommand::SetRunIntent(intent) => {
                if intent == RunIntent::Stopped {
                    // Scalars run before edges. A later Stop must cancel the
                    // start side effect of earlier group edges in this batch,
                    // while retaining their ordered durable selection changes.
                    for edge in &mut self.edges {
                        if let BackendCommand::ActivateSavedGroup { start, .. } = &mut edge.command
                        {
                            *start = false;
                        }
                    }
                }
                self.run_intent = Some(envelope);
            }
            BackendCommand::SetMasterVolume(_) => {
                let mut envelope = envelope;
                if let Some(receipt) = envelope.confirmation.take() {
                    self.master_receipt = Some(receipt);
                }
                self.master_volume = Some(envelope);
            }
            BackendCommand::SetMuted(_) => self.muted = Some(envelope),
            BackendCommand::SetReceiverLevel { receiver, .. } => {
                self.levels.insert(receiver, envelope);
            }
            BackendCommand::SetAudioEndpoint(_) => self.endpoint = Some(envelope),
            BackendCommand::SetLatencyPreset(_) => self.latency_preset = Some(envelope),
            BackendCommand::SetAutoConnect(_) => self.auto_connect = Some(envelope),
            other => {
                debug_assert!(
                    other.is_edge(),
                    "unclassified command reached the edge queue"
                );
                self.edges.push_back(CommandEnvelope {
                    id: envelope.id,
                    command: other,
                    confirmation: envelope.confirmation,
                });
            }
        }
    }

    /// Number of envelopes that survive the drain as distinct work items.
    fn winner_count(&self) -> u32 {
        let scalar_slots = [
            self.desired_members.is_some(),
            self.run_intent.is_some(),
            self.master_volume.is_some(),
            self.muted.is_some(),
            self.endpoint.is_some(),
            self.latency_preset.is_some(),
            self.auto_connect.is_some(),
        ]
        .into_iter()
        .filter(|held| *held)
        .count() as u32;
        scalar_slots + self.levels.len() as u32 + self.edges.len() as u32
    }
}

/// Everything the actor interacts with besides its own domain state.
struct ActorContext<'a> {
    commands_rx: mpsc::Receiver<CommandEnvelope>,
    events_tx: broadcast::Sender<BackendEvent>,
    diagnostics: broadcast::Sender<DiagnosticEvent>,
    store: Arc<dyn StateStore>,
    session_requests: Option<crate::backend::session::SessionSupervisorHandle>,
    discovery: &'a Arc<tokio::sync::Mutex<DiscoverySupervisor>>,
    capture: &'a Arc<tokio::sync::Mutex<CaptureSupervisor>>,
    discovery_updates: mpsc::Receiver<DiscoveryUpdate>,
    capture_updates: mpsc::Receiver<CaptureUpdate>,
    endpoint_scanner: crate::backend::capture::EndpointScanner,
    session_updates: mpsc::Receiver<SessionUpdate>,
}

/// Events driving the actor's main loop.
enum Event {
    Command(CommandEnvelope),
    #[allow(clippy::large_enum_variant)] // plan-fixed supervisor payload shape
    Discovery(Box<DiscoveryUpdate>),
    Capture(CaptureUpdate),
    CaptureTelemetry,
    Endpoints(
        Result<Vec<crate::backend::model::AudioEndpoint>, crate::backend::capture::CaptureError>,
    ),
    Session(SessionUpdate),
    /// At least one receiver's backoff deadline has passed.
    RetryDue,
    /// A platform edge arrived from the network monitor.
    System(SystemEvent),
    /// The post-resume interface-settle period elapsed.
    ResumeSettled,
    /// A desired receiver has been castable long enough for the persisted
    /// auto-connect policy to act on this launch.
    AutoConnectDue,
}

/// The single-owner actor state.
struct ControllerActor {
    listening_test: Option<(
        super::listening_check::ListeningTestGuard,
        CancellationToken,
    )>,
    last_listening_test_generation: Option<u64>,
    state_tx: Option<watch::Sender<Arc<DeviceSnapshot>>>,
    snapshot: DeviceSnapshot,
    persisted: PersistedStateV1,
    /// Durable manual calibration intent.
    ///
    /// Deliberately NOT part of [`DeviceSnapshot`]: the shell renders it from
    /// its own authoring state, and a session-setup input has no business in
    /// the published presentation snapshot.
    calibration: CalibrationProfile,
    /// Session generation minted for a calibration change that has not yet
    /// been seen to carry a live session.
    ///
    /// The apply itself only persists the profile and asks for a restart, so
    /// on its own it can never say more than "pending". This is what closes
    /// the lifecycle: when a session at or after that generation reports
    /// itself active, the delays it was built with are the ones now playing,
    /// and the edge becomes `Applied`.
    calibration_pending_generation: Option<u64>,
    inventory: BTreeMap<ReceiverId, InventoryEntry>,
    saved_names: HashMap<ReceiverId, String>,
    session_generation: u64,
    /// Generation of the session that is actually live, or `None` while none
    /// is.
    ///
    /// Separate from `session_generation`, which is the monotonic mint and
    /// must keep its value across a stop so the next reconcile hands out a
    /// strictly newer number. Diagnostics need the other question answered --
    /// "was a session running when this was written?" -- and would otherwise
    /// stamp every record after the first run with the number of a session
    /// that had already ended.
    live_session_generation: Option<u64>,
    /// Newest capture generation the supervisor reported, or zero while none
    /// runs. Mirrored here only so every diagnostic can name the capture
    /// worker it was written under; nothing else reads it.
    capture_generation: u64,
    /// Shared publication of the correlation counters. This actor writes the
    /// session and capture fields and reads the discovery field back from the
    /// supervisor that owns it. `None` in unit tests that build an actor
    /// without supervisors.
    generations_cell: Option<Arc<GenerationCell>>,
    /// The same feed [`ActorContext::diagnostics`] carries.
    ///
    /// Held here as well because the publish hook is the only place that can
    /// see *every* session-phase change, and it runs from call sites that do
    /// not carry a context -- `apply_capture_update` among them.
    diagnostics: Option<broadcast::Sender<DiagnosticEvent>>,
    primary: Option<ReceiverId>,
    active: BTreeSet<ReceiverId>,
    discovery_failures: u32,
    seen_discovery_generation: u64,
    /// One lifecycle machine per known receiver; the sole author of the
    /// `ReceiverLifecycle` values published in the snapshot.
    machines: BTreeMap<ReceiverId, ReceiverMachine>,
    /// Attempt counters, backoff deadlines, and healthy-streaming windows.
    retry: RetryState,
    /// Retry ladder (1, 2, 4, 8, 16, 30 s, deterministic +/-20% jitter).
    policy: RetryPolicy,
    /// Discovery-stability window and spacing gate of automatic rejoins.
    rejoin: RejoinRestartLimiter,
    /// Monotonic time seam behind every retry decision.
    clock: Arc<dyn Clock>,
    /// Desired set of the reconcile currently in flight.
    pending_desired: BTreeSet<ReceiverId>,
    /// Whether the platform announced a suspend that no resume has ended yet.
    ///
    /// `RunIntent` deliberately survives a suspend -- the user did not stop
    /// anything -- so it cannot express "the machine is going to sleep". Every
    /// automatic restart path has to read this flag instead, or it would build
    /// the session back up while the supervisor is tearing it down.
    suspended: bool,
    /// Deadline at which the suspend ends, whatever raised it.
    ///
    /// A resume is not an instant: `suspended` stays raised until this passes,
    /// so nothing rebuilds against addresses Windows is still rebinding. The
    /// suspend itself arms the same field with [`SUSPEND_WATCHDOG`], so a
    /// resume broadcast that never arrives cannot wedge the backend.
    resume_at: Option<Instant>,
    /// Instant past which the persisted auto-connect policy may no longer act,
    /// or `None` once this process has settled the question for good.
    ///
    /// Startup-only by construction, and the deadline is what makes that true:
    /// the arm is seeded from the loaded state with [`AUTO_CONNECT_WINDOW`]
    /// counted from launch, and can only ever be cleared. Without the bound it
    /// would be tied not to the launch but to the first moment some desired
    /// receiver turns castable, so a launch whose receivers were switched off
    /// would leave the window open indefinitely and let an unrelated
    /// membership edit hours later start playback. An explicit run intent --
    /// in either direction -- clears it too, which is what makes a Stop stay
    /// stopped for the rest of the process.
    startup_auto_connect_until: Option<Instant>,
}

// This is the controller thread's single wiring boundary; each argument is an
// independently owned port or startup dependency with a distinct lifetime.
#[allow(clippy::too_many_arguments)]
async fn run_controller(
    config: BackendConfig,
    overrides: BackendOverrides,
    commands_rx: mpsc::Receiver<CommandEnvelope>,
    state_tx: watch::Sender<Arc<DeviceSnapshot>>,
    events_tx: broadcast::Sender<BackendEvent>,
    diagnostics_tx: broadcast::Sender<DiagnosticEvent>,
    session_diagnostics_tx: watch::Sender<SessionDiagnosticsState>,
    ready_tx: std::sync::mpsc::SyncSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // 1. Validate paths and load durable state before announcing readiness.
    let store: Arc<dyn StateStore> = match overrides.store.clone() {
        Some(store) => store,
        None => Arc::new(FileStore::new(
            config.state_directory.clone(),
            Some(config.legacy_volume_path.clone()),
        )),
    };
    let outcome = store
        .load()
        .await
        .map_err(|error| anyhow::anyhow!("device state could not be loaded: {error}"))?;
    let corrupted = matches!(outcome, LoadOutcome::RecoveredCorrupt { .. });
    let migrated = matches!(outcome, LoadOutcome::MigratedLegacyVolume { .. });
    let persisted = outcome.into_state();
    let clock: Arc<dyn Clock> = overrides
        .clock
        .clone()
        .unwrap_or_else(|| Arc::new(SystemClock));
    let retry_timer: Arc<dyn BackendTimer> = overrides
        .timer
        .clone()
        .unwrap_or_else(|| Arc::new(TokioBackendTimer));
    // Read once, next to the rest of the durable state, and never written
    // back on load: a launch is a read.
    //
    // A calibration that could not be read is not a reason to refuse to
    // launch -- a backend that does not start is not an error message the
    // user can act on. The section stays on disk untouched (no write path
    // drops one it could not read), so the next launch restores it; this one
    // runs unaligned and says so.
    let (restored_calibration, calibration_unreadable) = match store.load_calibration().await {
        Ok(section) => (section, false),
        Err(_) => (None, true),
    };
    let mut actor = ControllerActor::from_persisted(persisted, clock);
    actor.calibration = restored_calibration
        .as_ref()
        .map(CalibrationProfile::from)
        .unwrap_or_default();
    // One shared correlation key for the whole backend. Each counter is
    // written by whoever owns it -- the actor publishes session and capture,
    // the discovery supervisor its own browse generation -- and read by the
    // others, so that every record names the same three generations.
    let generations_cell = GenerationCell::new();
    actor.generations_cell = Some(Arc::clone(&generations_cell));
    actor.diagnostics = Some(diagnostics_tx.clone());
    actor.state_tx = Some(state_tx.clone());
    // Publish the restored snapshot before anything else can observe it.
    state_tx.send_replace(Arc::new(actor.snapshot.clone()));

    // 2. Announce readiness; supervisors start afterwards per plan Step 3.
    let _ = ready_tx.send(Ok(()));

    // 3. Optional test gate: park before consuming any command.
    if let Some(gate) = overrides.start_gate.as_ref() {
        while !gate.is_open() {
            tokio::time::sleep(GATE_POLL).await;
        }
    }

    emit_diagnostics(
        &diagnostics_tx,
        actor.generations(),
        if corrupted {
            Severity::Warning
        } else {
            Severity::Info
        },
        DiagnosticPayload::Persistence {
            outcome: PersistenceSnapshot::Healthy,
        },
    );
    // The outcome itself is only Healthy/Error, so what *kind* of load this
    // was has to be said separately -- otherwise a quarantined state file and
    // a clean one leave identical records. Neither message names a path.
    if corrupted {
        emit_diagnostics(
            &diagnostics_tx,
            actor.generations(),
            Severity::Warning,
            DiagnosticPayload::Message(
                "stored device state was unreadable; it was set aside and defaults restored"
                    .to_owned(),
            ),
        );
    }
    if calibration_unreadable {
        // Names no path, no receiver and no delay -- only that this session
        // is running without the alignment the user stored.
        emit_diagnostics(
            &diagnostics_tx,
            actor.generations(),
            Severity::Warning,
            DiagnosticPayload::Message(
                "the stored speaker alignment could not be read; this session runs without it"
                    .to_owned(),
            ),
        );
    }
    if migrated {
        emit_diagnostics(
            &diagnostics_tx,
            actor.generations(),
            Severity::Info,
            DiagnosticPayload::Message(
                "master volume was imported once from the previous release's settings".to_owned(),
            ),
        );
    }
    if corrupted {
        let _ = events_tx.send(BackendEvent::Notice {
            severity: Severity::Warning,
            code: NoticeCode::PersistenceWrite,
            scope: ErrorScope::Persistence,
            message: "the previous state file was damaged; defaults were restored".to_owned(),
        });
    }
    if calibration_unreadable {
        let _ = events_tx.send(BackendEvent::Notice {
            severity: Severity::Warning,
            code: NoticeCode::PersistenceWrite,
            scope: ErrorScope::Persistence,
            message: "the saved speaker alignment could not be read and is not in use".to_owned(),
        });
    }

    // 4. Start supervisors now that readiness succeeded, taking each one's
    // bounded update stream out of its handle so the actor owns it outright.
    let timer: Arc<dyn Timer> = match overrides.timer.clone() {
        Some(injected) => Arc::new(SupervisorTimer(injected)),
        None => Arc::new(TokioTimer),
    };

    let discovery_impl: Arc<dyn BackendDiscoverySource> = match overrides.discovery.clone() {
        Some(source) => source,
        None => {
            let browser = airplay_discovery::ServiceBrowser::new()
                .map_err(|error| anyhow::anyhow!("mDNS service browser unavailable: {error}"))?;
            Arc::new(MdnsDiscoverySource(browser))
        }
    };
    let discovery = Arc::new(tokio::sync::Mutex::new(DiscoverySupervisor::start(
        Arc::new(OverseenDiscoverySource(discovery_impl)),
        DiscoveryConfig {
            policy: RetryPolicy::default(),
            clock: Arc::new(SystemClock),
            timer: timer.clone(),
            diagnostics: Some(diagnostics_tx.clone()),
            generations: Some(Arc::clone(&generations_cell)),
        },
    )));
    let discovery_updates = detach_supervisor_updates(&discovery, |supervisor| {
        supervisor
            .take_updates()
            .expect("a freshly started supervisor still owns its update stream")
    })
    .await;

    let capture_impl: Arc<dyn CaptureSource> = overrides
        .capture
        .clone()
        .unwrap_or_else(|| Arc::new(crate::backend::capture::WasapiCaptureSource::new()));
    let endpoint_scanner =
        crate::backend::capture::EndpointScanner::start(Arc::clone(&capture_impl))?;
    let capture = Arc::new(tokio::sync::Mutex::new(CaptureSupervisor::start(
        capture_impl,
        Arc::new(LiveDecoderFactory),
        CaptureConfig {
            timer,
            diagnostics: Some(diagnostics_tx.clone()),
            generations: Some(Arc::clone(&generations_cell)),
        },
    )));
    // The restored preference has to reach the supervisor before its first
    // generation opens a worker, or a persisted explicit endpoint would be
    // silently ignored until the user re-selected it.
    set_capture_preference(&capture, actor.persisted.audio_endpoint.clone()).await;
    let capture_updates = detach_supervisor_updates(&capture, |supervisor| {
        supervisor
            .take_updates()
            .expect("a freshly started supervisor still owns its update stream")
    })
    .await;

    // Production transport: one `AirPlayClient` per session generation, fed by
    // the capture supervisor's stable decoder pair so the AirPlay timeline
    // never notices a capture worker restart or an endpoint swap.
    let factory: Arc<dyn SessionTransportFactory> = overrides
        .session_factory
        .clone()
        .unwrap_or_else(|| Arc::new(AirPlaySessionTransportFactory::new()));
    let decoders: Arc<dyn SessionDecoderSource> = match overrides.decoders.clone() {
        Some(decoders) => decoders,
        None => capture.lock().await.decoder_source(),
    };
    let session_config = SessionConfig {
        probe_interval: overrides.probe_interval.unwrap_or(PROBE_INTERVAL),
    };
    let session = Arc::new(tokio::sync::Mutex::new(
        SessionSupervisor::start_with_diagnostics_config(
            factory,
            decoders,
            session_config,
            session_diagnostics_tx,
        ),
    ));
    let session_requests = session.lock().await.handle();
    let session_updates = detach_supervisor_updates(&session, |supervisor| {
        supervisor
            .take_updates()
            .expect("a freshly started supervisor still owns its update stream")
    })
    .await;
    let events_tx_for_notice = events_tx.clone();

    // Platform edges: one coalescing monitor per backend, owning the OS
    // notification handle until shutdown hands it back.
    let (system_tx, mut system_rx) = mpsc::channel::<SystemEvent>(SYSTEM_EVENT_CAPACITY);
    let network_pair = match (overrides.network.clone(), overrides.local_bindings.clone()) {
        (Some(source), Some(bindings)) => Some((source, bindings)),
        (None, None) => platform_sources(),
        // A half-configured override is a test wiring mistake, not a runtime
        // condition: honouring it would silently sample the real machine's
        // addresses from a fake notification source.
        _ => anyhow::bail!("network override requires both a source and a binding sampler"),
    };
    let network_monitor = match network_pair {
        Some((source, bindings)) => {
            let seam: Arc<dyn BackendTimer> = overrides
                .timer
                .clone()
                .unwrap_or_else(|| Arc::new(TokioBackendTimer));
            match NetworkMonitor::start(source, bindings, seam, system_tx) {
                Ok(monitor) => Some(monitor),
                Err(error) => {
                    // Losing interface notifications degrades recovery; it must
                    // not keep the backend from starting.
                    let _ = events_tx_for_notice.send(BackendEvent::Notice {
                        severity: Severity::Warning,
                        code: NoticeCode::DiscoveryFailed,
                        scope: ErrorScope::Discovery,
                        message: format!("network change monitoring is unavailable: {error}"),
                    });
                    None
                }
            }
        }
        None => None,
    };

    let mut ctx = ActorContext {
        commands_rx,
        events_tx,
        diagnostics: diagnostics_tx,
        store,
        session_requests: Some(session_requests),
        discovery: &discovery,
        capture: &capture,
        discovery_updates,
        capture_updates,
        endpoint_scanner,
        session_updates,
    };

    // 5. Main loop: one awaited receive, then a bounded coalescing drain.
    let mut discovery_open = true;
    let mut capture_open = true;
    let mut endpoints_open = true;
    let mut session_open = true;
    let mut system_open = true;
    let mut shutdown_requested = false;
    let mut capture_telemetry_tick = tokio::time::interval(CAPTURE_TELEMETRY_PERIOD);
    capture_telemetry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    capture_telemetry_tick.tick().await;
    while !shutdown_requested {
        // The backoff ladder only becomes policy once something consumes its
        // deadlines. This arm is that consumer; it sits last so a busy event
        // stream is never starved by it, and it is disabled outright while no
        // deadline is pending.
        let retry_in = actor.next_retry_delay();
        let resume_in = actor.resume_delay();
        let auto_connect_in = actor.auto_connect_delay();
        let event = tokio::select! {
            biased;
            command = ctx.commands_rx.recv() => match command {
                Some(envelope) => Event::Command(envelope),
                None => break,
            },
            _ = capture_telemetry_tick.tick() => Event::CaptureTelemetry,
            update = ctx.discovery_updates.recv(), if discovery_open => match update {
                Some(update) => Event::Discovery(Box::new(update)),
                None => { discovery_open = false; continue; }
            },
            update = ctx.capture_updates.recv(), if capture_open => match update {
                Some(update) => Event::Capture(update),
                None => { capture_open = false; continue; }
            },
            result = ctx.endpoint_scanner.results.recv(), if endpoints_open => match result {
                Some(result) => Event::Endpoints(result),
                None => {
                    endpoints_open = false;
                    // Runtime construction failure and unexpected worker exit
                    // both close this channel. Publish one sanitized failure.
                    Event::Endpoints(Err(crate::backend::capture::CaptureError::Endpoints(
                        "endpoint scanner terminated".into(),
                    )))
                }
            },
            update = ctx.session_updates.recv(), if session_open => match update {
                Some(update) => Event::Session(update),
                None => { session_open = false; continue; }
            },
            edge = system_rx.recv(), if system_open => match edge {
                Some(edge) => Event::System(edge),
                // The monitor stopped; platform edges can still arrive through
                // `NotifySystem`, so the loop keeps running without this arm.
                None => { system_open = false; continue; }
            },
            () = retry_timer.sleep(resume_in.unwrap_or_default()), if resume_in.is_some() => {
                Event::ResumeSettled
            }
            () = retry_timer.sleep(retry_in.unwrap_or_default()), if retry_in.is_some() => {
                Event::RetryDue
            }
            // Last arm and armed at most once per process: a launch decision
            // must never compete with live traffic for the loop.
            () = retry_timer.sleep(auto_connect_in.unwrap_or_default()),
                if auto_connect_in.is_some() =>
            {
                Event::AutoConnectDue
            }
        };

        match event {
            Event::Command(envelope) => {
                let mut pending = PendingBatch::default();
                pending.insert(envelope);
                while let Ok(next) = ctx.commands_rx.try_recv() {
                    pending.insert(next);
                }
                let dropped = pending.drained_total.saturating_sub(pending.winner_count());
                if dropped > 0 {
                    emit_diagnostics(
                        &ctx.diagnostics,
                        actor.generations(),
                        Severity::Info,
                        DiagnosticPayload::CommandCoalesced { dropped },
                    );
                }
                shutdown_requested |= actor.process_batch(pending, &mut ctx).await;
            }
            Event::Discovery(update) => actor.apply_discovery_update(*update, &ctx),
            Event::Capture(update) => {
                if matches!(
                    &update,
                    CaptureUpdate::Ready { .. } | CaptureUpdate::Recovering { .. }
                ) {
                    ctx.endpoint_scanner.request();
                }
                actor.apply_capture_update(update);
            }
            Event::CaptureTelemetry => {
                let telemetry = ctx.capture.lock().await.take_input_telemetry();
                actor.apply_capture_telemetry(telemetry);
            }
            Event::Endpoints(result) => actor.apply_endpoints(result),
            Event::Session(update) => actor.apply_session_update(update, &ctx),
            Event::RetryDue => actor.fire_due_retries(&ctx),
            Event::System(edge) => actor.handle_system_event(edge, &ctx).await,
            Event::ResumeSettled => {
                ctx.endpoint_scanner.request();
                actor.finish_resume(&ctx).await;
            }
            Event::AutoConnectDue => actor.apply_startup_auto_connect(&ctx),
        }
        actor.publish_generations();
    }

    // 6. Graceful shutdown: end every worker, then join what was ended --
    // all of it inside ONE total budget rather than one budget per stage.
    //
    // Signalling comes first and completely: a drain can only finish because
    // the worker feeding it was told to stop, so anything left unsignalled
    // turns its drain into a guaranteed timeout. The budget is a bound on a
    // teardown that is expected to finish, not the mechanism that ends it.
    let started = Instant::now();
    let deadline = started + SHUTDOWN_GROUP_BUDGET;
    // Every bounded step reports whether it finished or hit its budget, so
    // the shutdown record can say which of the two happened instead of
    // leaving the reader to guess from a duration.
    let mut timed_out = false;
    if let Some(monitor) = network_monitor {
        // Hands the OS notification handle back before anything else is torn
        // down, so no callback can fire into a half-stopped backend. Bounded
        // like every other step below: this is the first teardown step and
        // nothing after it may be hostage to it.
        timed_out |= tokio::time::timeout(remaining_budget(deadline), monitor.shutdown())
            .await
            .is_err();
    }
    // Capture first: it owns the WASAPI worker and the PCM bridge, and the
    // session's decoder consumers hang off it.
    timed_out |= signal_within(&capture, deadline, "capture", |supervisor| {
        supervisor.request_control(CaptureControl::Stop);
    })
    .await;
    // Then the session. Dropping the request handle alone leaves the
    // supervisor's own producer clone behind, so its loop would never see the
    // queue close; the cancellation token is what actually ends it.
    timed_out |= signal_within(&session, deadline, "session", |supervisor| {
        supervisor.request_shutdown();
    })
    .await;
    drop(ctx.session_requests.take());
    timed_out |= signal_within(&discovery, deadline, "discovery", |supervisor| {
        supervisor.request_control(DiscoveryControl::Shutdown);
    })
    .await;
    timed_out |= tokio::time::timeout(remaining_budget(deadline), async {
        while ctx.discovery_updates.recv().await.is_some() {}
    })
    .await
    .is_err();
    timed_out |= tokio::time::timeout(remaining_budget(deadline), async {
        while ctx.capture_updates.recv().await.is_some() {}
    })
    .await
    .is_err();
    timed_out |= tokio::time::timeout(remaining_budget(deadline), async {
        while ctx.session_updates.recv().await.is_some() {}
    })
    .await
    .is_err();
    emit_diagnostics(
        &ctx.diagnostics,
        actor.generations(),
        if timed_out {
            Severity::Warning
        } else {
            Severity::Info
        },
        DiagnosticPayload::WorkerShutdown {
            duration: started.elapsed(),
            timed_out,
        },
    );
    Ok(())
}

/// Time left before the shared teardown deadline, never negative.
///
/// One deadline for the whole exit rather than a fresh budget per stage: four
/// stages with four independent four-second budgets is a sixteen-second exit
/// wearing a four-second label.
fn remaining_budget(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Takes a supervisor's lock inside the shared teardown deadline and signals
/// it; reports whether the budget ran out before the lock could be taken.
///
/// Every signalling step used to `lock().await` outright. Back when a relay
/// task held the same mutex across a receive window, that was the one wait on
/// this path with no upper bound of its own, and the teardown could overrun
/// the budget it advertises while still reporting `timed_out == false`,
/// because no `timeout` had been wrapped around the step that overran. The
/// relay is gone and the actor is now the only taker of these mutexes, so the
/// lock is expected to be free -- but "expected to be free" is an assumption
/// about the rest of this file, and on the teardown path a wrong assumption
/// costs the exit. A lock that cannot be taken in time is exactly as much a
/// budget overrun as a drain that does not finish, and is recorded as one.
async fn signal_within<T>(
    supervisor: &Arc<tokio::sync::Mutex<T>>,
    deadline: Instant,
    what: &str,
    signal: impl FnOnce(&mut T),
) -> bool {
    match tokio::time::timeout(remaining_budget(deadline), supervisor.lock()).await {
        Ok(mut guard) => {
            signal(&mut guard);
            false
        }
        Err(_) => {
            tracing::warn!("could not reach the {what} supervisor inside the shutdown budget");
            true
        }
    }
}

/// Hands one supervisor's bounded update stream to the actor, once, at
/// startup, so nothing ever waits for an update while holding the lock that
/// control requests need.
///
/// This used to be a relay task per supervisor: it took the supervisor mutex,
/// waited up to a quarter second for an update, gave the lock back and took it
/// again, copying every update into a second queue of the same size. The
/// reason was a borrow split -- `updates_mut(&mut self)` needs `&mut`, while
/// `request_control(&self)` does not, and the mutex bridged the two. The
/// price was paid by every control request: `request_capture_control`,
/// `set_capture_preference`, `request_discovery_control` and the
/// suspend/resume paths all queued behind a wait for something that had not
/// happened yet, so a single endpoint change or resume cost several lock
/// handovers of 250 ms each. In the other direction the same lock let a
/// control request stall the update stream for as long as it ran.
///
/// Taking the receiver out of the supervisor once removes the split instead
/// of pricing it: the channel is created once per supervisor and is never
/// replaced -- restarts swap the child worker, not the queue -- so nothing
/// needs it back. The actor then owns the stream outright, the supervisor
/// mutex is only ever taken by the actor itself, and the second queue, the
/// receive window, the retry loop and the whole "relay waits on a full queue
/// while holding the lock" deadlock class disappear with the task.
async fn detach_supervisor_updates<T, U>(
    supervisor: &Arc<tokio::sync::Mutex<T>>,
    take: impl FnOnce(&mut T) -> mpsc::Receiver<U>,
) -> mpsc::Receiver<U> {
    take(&mut *supervisor.lock().await)
}

async fn request_discovery_control(
    supervisor: &Arc<tokio::sync::Mutex<DiscoverySupervisor>>,
    control: DiscoveryControl,
) {
    supervisor.lock().await.request_control(control);
}

async fn request_capture_control(
    supervisor: &Arc<tokio::sync::Mutex<CaptureSupervisor>>,
    control: CaptureControl,
) {
    supervisor.lock().await.request_control(control);
}

/// Hands the durable endpoint preference to the capture supervisor.
///
/// The supervisor reads it at the start of every capture generation, so this
/// must happen BEFORE the restart that is supposed to apply it -- otherwise
/// the restart reopens the endpoint the user just moved away from.
async fn set_capture_preference(
    supervisor: &Arc<tokio::sync::Mutex<CaptureSupervisor>>,
    preference: AudioEndpointPreference,
) {
    supervisor
        .lock()
        .await
        .lock_state()
        .set_preference(preference);
}

// ---------------------------------------------------------------------------
// Actor implementation
// ---------------------------------------------------------------------------

impl Drop for ControllerActor {
    fn drop(&mut self) {
        if let Some((_, token)) = self.listening_test.take() {
            token.cancel();
        }
    }
}

impl ControllerActor {
    /// Builds initial state from loaded persisted values: desired members are
    /// restored as selected/unavailable rows before any discovery arrives.
    fn from_persisted(persisted: PersistedStateV1, clock: Arc<dyn Clock>) -> Self {
        let saved_names: HashMap<ReceiverId, String> = persisted
            .saved_groups
            .iter()
            .flat_map(|group| {
                group
                    .members
                    .iter()
                    .map(|member| (member.receiver, member.last_known_name.clone()))
            })
            .collect();
        let saved_groups = persisted
            .saved_groups
            .iter()
            .map(|group| SavedGroupSnapshot {
                id: group.id,
                name: group.name.clone(),
                members: group.members.clone(),
            })
            .collect();
        // Read before `persisted` moves into the struct below. The window is
        // measured from here -- the launch -- and not from the first castable
        // sighting, so the policy cannot outlive the start it belongs to.
        let startup_auto_connect_until = persisted
            .auto_connect
            .then(|| clock.now() + AUTO_CONNECT_WINDOW);
        let mut actor = Self {
            listening_test: None,
            last_listening_test_generation: None,
            state_tx: None,
            snapshot: DeviceSnapshot {
                master_volume: persisted.master_volume,
                receiver_levels: persisted.receiver_levels.clone(),
                muted: persisted.muted,
                latency_preset: persisted.latency_preset,
                auto_connect: persisted.auto_connect,
                audio_source: AudioSourceSnapshot {
                    preference: persisted.audio_endpoint.clone(),
                    ..AudioSourceSnapshot::default()
                },
                saved_groups,
                desired_members: persisted.last_desired_members.clone(),
                ..DeviceSnapshot::default()
            },
            persisted,
            calibration: CalibrationProfile::default(),
            calibration_pending_generation: None,
            inventory: BTreeMap::new(),
            saved_names,
            session_generation: 0,
            live_session_generation: None,
            capture_generation: 0,
            generations_cell: None,
            diagnostics: None,
            primary: None,
            active: BTreeSet::new(),
            discovery_failures: 0,
            seen_discovery_generation: 0,
            machines: BTreeMap::new(),
            retry: RetryState::new(),
            policy: RetryPolicy::default(),
            rejoin: RejoinRestartLimiter::new(Arc::clone(&clock)),
            clock,
            pending_desired: BTreeSet::new(),
            suspended: false,
            resume_at: None,
            startup_auto_connect_until,
        };
        let mut restored = actor.snapshot.clone();
        actor.rebuild_receiver_rows(&mut restored);
        actor.snapshot = restored;
        actor.snapshot.sort_for_publication();
        actor
    }

    /// The three generation counters every diagnostic is correlated by.
    ///
    /// Read from the actor's own view rather than from the snapshot: the
    /// snapshot is the *published* state and lags by one publication during
    /// the window in which most failure diagnostics are written.
    fn generations(&self) -> Generations {
        Generations {
            session: self.live_session_generation.unwrap_or(0),
            // The browse counter is published by the supervisor that owns it,
            // which is ahead of `seen_discovery_generation` for as long as a
            // `Started` update is still in flight. The larger of the two is
            // the one that was live at emission.
            discovery: self
                .generations_cell
                .as_ref()
                .map_or(0, |cell| cell.get().discovery)
                .max(self.seen_discovery_generation),
            capture: self.capture_generation,
        }
    }

    /// Hands the counters this actor owns to the supervisors' shared view.
    ///
    /// Called at every site that changes one of them *and* once at the end of
    /// each main-loop iteration. The loop-end call is the backstop that no
    /// new assignment can forget; the assignment-site calls are what keep the
    /// window short, because a single iteration can await a disk write and a
    /// supervisor emits into that window.
    fn publish_generations(&self) {
        if let Some(cell) = self.generations_cell.as_ref() {
            cell.publish_controller(self.generations());
        }
    }

    /// Publishes the current snapshot as a new revision.
    fn publish(&mut self) {
        if self
            .listening_test
            .as_ref()
            .is_some_and(|(guard, _)| !guard.matches(&self.snapshot))
        {
            if let Some((_, token)) = self.listening_test.take() {
                token.cancel();
            }
        }
        self.snapshot.revision = self
            .snapshot
            .revision
            .checked_add(1)
            .expect("snapshot revision overflow");
        self.snapshot.sort_for_publication();
        if let Some(state_tx) = self.state_tx.as_ref() {
            let _ = state_tx.send_replace(Arc::new(self.snapshot.clone()));
        }
    }

    /// Applies a supervisor-driven change only when it alters visible state.
    fn maybe_publish(&mut self, next: DeviceSnapshot) {
        if next == self.snapshot {
            return;
        }
        // Every session start, degrade, restart, and stop passes through
        // here, so this is the one place where none of them can be
        // forgotten by a new call site.
        let phase_changed = next.session.phase != self.snapshot.session.phase;
        let phase = next.session.phase.clone();
        let active_members = u32::try_from(next.session.active.len()).unwrap_or(u32::MAX);
        self.snapshot = next;
        self.publish();
        if !phase_changed {
            return;
        }
        let Some(diagnostics) = self.diagnostics.clone() else {
            return;
        };
        let severity = match phase {
            SessionPhase::Failed { .. } => Severity::Error,
            SessionPhase::Degraded { .. } | SessionPhase::Restarting { .. } => Severity::Warning,
            _ => Severity::Info,
        };
        emit_diagnostics(
            &diagnostics,
            self.generations(),
            severity,
            DiagnosticPayload::SessionTransition {
                phase,
                active_members,
            },
        );
    }

    /// Rebuilds receiver rows from inventory plus desired-member bookkeeping.
    ///
    /// Desired members stay visible as unavailable rows even before discovery
    /// knows them; names prefer discovery metadata, then saved-group labels,
    /// and are otherwise empty -- the stable identity is never used as a
    /// display name.
    fn rebuild_receiver_rows(&mut self, snapshot: &mut DeviceSnapshot) {
        let desired = self.snapshot.desired_members.clone();
        // Inventory cleanup, not a lifecycle variant: a receiver that is no
        // longer desired disappears only once it is actually unavailable.
        self.inventory
            .retain(|id, entry| desired.contains(id) || (entry.available && entry.castable));
        let mut ids: BTreeSet<ReceiverId> = self.inventory.keys().copied().collect();
        ids.extend(desired.iter().copied());
        for id in &ids {
            let castable = self.castable(id);
            self.machines
                .entry(*id)
                .or_insert_with(|| ReceiverMachine::new(castable));
        }
        self.machines.retain(|id, _| ids.contains(id));
        // Symmetric generation boundary. A receiver dropped from the desired
        // set is released here, so `snapshot.session.active` and its row can
        // never disagree about whether it still streams -- `leave_session()`
        // alone would only close that boundary when the whole session stops.
        for (id, machine) in self.machines.iter_mut() {
            if !desired.contains(id) {
                machine.release();
                self.retry.clear_deadline(*id);
            }
        }
        snapshot.receivers = ids
            .into_iter()
            .map(|id| {
                let entry = self.inventory.get(&id);
                let name = entry
                    .and_then(|entry| (!entry.name.is_empty()).then_some(entry.name.clone()))
                    .or_else(|| self.saved_names.get(&id).cloned())
                    // Deliberately *not* `id.to_string()`. That renders the
                    // stable identity as `AA:BB:CC:DD:EE:FF`, and this arm is
                    // the common case rather than the exotic one: every
                    // desired member restored from persisted state is a row
                    // before discovery has seen it, and stays one for as long
                    // as the speaker is switched off. Naming a receiver after
                    // its MAC address puts a hardware identifier on screen,
                    // which is precisely what the snapshot boundary forbids.
                    // "No name known" is the truth, and only the presentation
                    // layer can say it in the user's language.
                    .unwrap_or_default();
                let model = entry
                    .and_then(|entry| entry.device.as_ref())
                    .map(|device| device.model.clone())
                    .unwrap_or_default();
                let lifecycle = self
                    .machines
                    .get(&id)
                    .map_or(ReceiverLifecycle::Unavailable, |machine| {
                        machine.state().clone()
                    });
                ReceiverSnapshot {
                    id,
                    name,
                    model,
                    lifecycle,
                }
            })
            .collect();
        // The backoff set is derived, never maintained separately, so row and
        // session views can never disagree.
        snapshot.session.retry_waiting = self
            .machines
            .iter()
            .filter(|(_, machine)| {
                matches!(machine.state(), ReceiverLifecycle::RetryWaiting { .. })
            })
            .map(|(id, _)| *id)
            .collect();
    }

    /// Whether discovery currently offers a castable AirPlay service.
    fn castable(&self, id: &ReceiverId) -> bool {
        self.inventory
            .get(id)
            .is_some_and(|entry| entry.available && entry.castable)
    }

    /// Applies one lifecycle transition, reporting rejections truthfully.
    ///
    /// Rejected transitions panic in debug builds (a test fails immediately)
    /// and emit a presentation-safe invariant diagnostic in release builds.
    fn drive(
        &mut self,
        receiver: ReceiverId,
        ctx: &ActorContext<'_>,
        apply: impl FnOnce(&mut ReceiverMachine) -> Result<(), InvalidTransition>,
    ) {
        let castable = self.castable(&receiver);
        let machine = self
            .machines
            .entry(receiver)
            .or_insert_with(|| ReceiverMachine::new(castable));
        let before = machine.state().clone();
        let outcome = apply(machine);
        let state = machine.state().clone();
        // An accepted transition that lands where it started is not a
        // transition. Reporting it would put a second `Connecting` record in
        // every retry timeline -- once for the retry, once for the generation
        // that adopts it -- and a reader cannot tell such a pair from a real
        // second attempt.
        if outcome.is_ok() && state == before {
            return;
        }
        match outcome {
            Ok(()) => emit_receiver_diagnostic(
                &ctx.diagnostics,
                self.generations(),
                receiver,
                Severity::Info,
                DiagnosticPayload::ReceiverTransition {
                    receiver,
                    to: state,
                },
            ),
            Err(rejected) => emit_receiver_diagnostic(
                &ctx.diagnostics,
                self.generations(),
                receiver,
                Severity::Warning,
                DiagnosticPayload::Message(format!("receiver invariant violated: {rejected}")),
            ),
        }
    }

    /// Refreshes the rows from the machines and publishes when anything moved.
    fn republish_rows(&mut self) {
        let mut next = self.snapshot.clone();
        self.rebuild_receiver_rows(&mut next);
        self.maybe_publish(next);
    }

    /// Registers newly discovered receivers, mirrors service availability into
    /// their machines, and publishes the resulting rows once.
    fn refresh_receivers(&mut self, ctx: &ActorContext<'_>) {
        let mut scratch = self.snapshot.clone();
        // Creates a machine for every id currently in scope, so the
        // availability sync below sees all of them.
        self.rebuild_receiver_rows(&mut scratch);
        self.sync_service_availability(ctx);
        self.republish_rows();
    }

    /// Mirrors discovery availability into every machine.
    fn sync_service_availability(&mut self, ctx: &ActorContext<'_>) {
        let states: Vec<(ReceiverId, bool)> = self
            .machines
            .keys()
            .map(|id| (*id, self.castable(id)))
            .collect();
        for (id, castable) in states {
            // The discoverability window is a discovery fact and must be
            // maintained even when the machine's own state does not move --
            // otherwise a receiver that flaps while a generation owns it
            // would keep a window it never actually held.
            if castable {
                self.rejoin.note_discovered(id);
            } else {
                self.rejoin.note_lost(id);
            }
            if self
                .machines
                .get(&id)
                .is_some_and(|machine| machine.service_available() == castable)
            {
                continue;
            }
            self.drive(id, ctx, |machine| machine.set_service_available(castable));
        }
    }

    // -- command batch -------------------------------------------------------

    /// Processes one drained batch: scalars first (each slot independently),
    /// then edges strictly in arrival order. Returns whether shutdown ran.
    async fn process_batch(&mut self, pending: PendingBatch, ctx: &mut ActorContext<'_>) -> bool {
        let mut reconcile: Option<ReconcileCause> = None;
        let mut volumes_changed = false;
        let mut shutdown_requested = false;
        let mut group_start_confirmations = Vec::new();

        if let Some(envelope) = pending.desired_members {
            let members = match &envelope.command {
                BackendCommand::SetDesiredMembers { members } => members.clone(),
                _ => unreachable!("slot holds only its own command"),
            };
            if members != self.persisted.last_desired_members {
                let for_persisted = members.clone();
                let for_announce = members.clone();
                let applied = self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.last_desired_members = for_persisted)
                    .announce(move |snapshot| {
                        snapshot.desired_members = for_announce;
                        snapshot.desired_revision = snapshot.desired_revision.saturating_add(1);
                    })
                    .then_refresh_rows()
                    .run()
                    .await
                    .applied();
                if applied {
                    reconcile = Some(ReconcileCause::Membership);
                }
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        if let Some(envelope) = pending.run_intent {
            let intent = match &envelope.command {
                BackendCommand::SetRunIntent(intent) => *intent,
                _ => unreachable!(),
            };
            // The user answered the question auto-connect exists to answer,
            // so the launch policy is spent -- including when the answer
            // matches the current intent. A Stop that only holds until the
            // next discovery sighting is not a Stop.
            self.startup_auto_connect_until = None;
            if intent != self.snapshot.run_intent {
                self.snapshot.run_intent = intent;
                // Deliberately not part of the durable schema: every launch
                // starts Stopped unless auto-connect applies.
                self.complete(envelope.id, ctx);
                self.publish();
                reconcile = Some(ReconcileCause::Membership);
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        if let Some(envelope) = pending.master_volume {
            let value = match &envelope.command {
                BackendCommand::SetMasterVolume(value) => *value,
                _ => unreachable!(),
            };
            if value != self.persisted.master_volume {
                volumes_changed |= self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.master_volume = value)
                    .announce(move |snapshot| snapshot.master_volume = value)
                    .run()
                    .await
                    .applied();
            } else {
                self.complete(envelope.id, ctx);
            }
            if let Some(confirmation) = pending.master_receipt {
                if let Some((_, error)) = self
                    .snapshot
                    .command_outcomes
                    .recent
                    .iter()
                    .find(|(id, _)| *id == envelope.id)
                {
                    let result = super::command::CommandConfirmation {
                        revision: self.snapshot.revision + 1,
                        error: *error,
                        session_generation_floor: None,
                    };
                    let _ = confirmation.send(result);
                }
                self.publish();
            }
        }

        if let Some(envelope) = pending.muted {
            let muted = match &envelope.command {
                BackendCommand::SetMuted(muted) => *muted,
                _ => unreachable!(),
            };
            if muted != self.persisted.muted {
                volumes_changed |= self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.muted = muted)
                    .announce(move |snapshot| snapshot.muted = muted)
                    .run()
                    .await
                    .applied();
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        if let Some(envelope) = pending.endpoint {
            let preference = match &envelope.command {
                BackendCommand::SetAudioEndpoint(preference) => preference.clone(),
                _ => unreachable!(),
            };
            if preference != self.persisted.audio_endpoint {
                let for_persisted = preference.clone();
                let for_announce = preference.clone();
                let applied = self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.audio_endpoint = for_persisted)
                    .announce(move |snapshot| {
                        snapshot.audio_source.preference = for_announce;
                    })
                    .run()
                    .await
                    .applied();
                if applied {
                    ctx.endpoint_scanner.request();
                    set_capture_preference(&Arc::clone(ctx.capture), preference).await;
                    request_capture_control(
                        &Arc::clone(ctx.capture),
                        CaptureControl::RequestEndpointChange,
                    )
                    .await;
                }
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        if let Some(envelope) = pending.latency_preset {
            let preset = match &envelope.command {
                BackendCommand::SetLatencyPreset(preset) => *preset,
                _ => unreachable!(),
            };
            if let Some(reason) = preset.disabled_reason() {
                // Same text the snapshot row carries, from the same source:
                // a rejection that reads differently from the greyed-out
                // option next to it is a bug report waiting to happen.
                self.fail(envelope.id, reason.as_str(), ctx);
            } else if preset != self.persisted.latency_preset {
                let applied = self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.latency_preset = preset)
                    .announce(move |snapshot| snapshot.latency_preset = preset)
                    .run()
                    .await
                    .applied();
                if applied {
                    reconcile = Some(ReconcileCause::LatencyPreset);
                }
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        if let Some(envelope) = pending.auto_connect {
            let enabled = match &envelope.command {
                BackendCommand::SetAutoConnect(enabled) => *enabled,
                _ => unreachable!(),
            };
            // Policy for later launches only: enabling it deliberately does
            // NOT arm the running process, and disabling it cancels a start
            // this launch has not spent yet -- a setting the user just
            // cleared may not get one last turn.
            if !enabled {
                self.startup_auto_connect_until = None;
            }
            if enabled != self.persisted.auto_connect {
                self.commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| state.auto_connect = enabled)
                    .announce(move |snapshot| snapshot.auto_connect = enabled)
                    .run()
                    .await
                    .applied();
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        for (receiver, envelope) in pending.levels {
            let level = match &envelope.command {
                BackendCommand::SetReceiverLevel { level, .. } => *level,
                _ => unreachable!(),
            };
            if self.persisted.receiver_levels.get(&receiver) != Some(&level) {
                volumes_changed |= self
                    .commit_durable(ctx, envelope.id)
                    .mutate_persisted(move |state| {
                        state.receiver_levels.insert(receiver, level);
                    })
                    .run()
                    .await
                    .applied();
            } else {
                self.complete(envelope.id, ctx);
            }
        }

        // Edges execute strictly in arrival order after all scalars.
        for envelope in pending.edges {
            let starts_group = matches!(
                &envelope.command,
                BackendCommand::ActivateSavedGroup { start: true, .. }
            );
            match envelope.command {
                BackendCommand::ApplyDesiredMembers { members } => {
                    if members != self.persisted.last_desired_members {
                        let for_announce = members.clone();
                        if self
                            .commit_durable(ctx, envelope.id)
                            .mutate_persisted(move |state| state.last_desired_members = members)
                            .announce(move |snapshot| {
                                snapshot.desired_members = for_announce;
                                snapshot.desired_revision =
                                    snapshot.desired_revision.saturating_add(1);
                            })
                            .then_refresh_rows()
                            .run()
                            .await
                            .applied()
                        {
                            reconcile = Some(ReconcileCause::Membership);
                        }
                    } else {
                        self.complete(envelope.id, ctx);
                    }
                }
                BackendCommand::SaveGroup { id, name, members } => {
                    self.handle_save_group(envelope.id, id, name, members, ctx)
                        .await;
                }
                BackendCommand::DeleteGroup(group_id) => {
                    self.handle_delete_group(envelope.id, group_id, ctx).await;
                }
                BackendCommand::ActivateSavedGroup { id, start } => {
                    if self
                        .handle_activate_group(envelope.id, id, start, ctx)
                        .await
                        && reconcile.is_none()
                    {
                        reconcile = Some(ReconcileCause::SavedGroup);
                    }
                }
                BackendCommand::RetryReceiver(receiver) => {
                    // Clears exactly this receiver's pending deadline; the
                    // reconciliation below still obeys the full-restart
                    // matrix and produces no additional restart.
                    self.retry.manual_retry(receiver);
                    self.drive(receiver, ctx, |machine| machine.retry_now());
                    self.republish_rows();
                    reconcile = Some(ReconcileCause::Retry);
                    self.complete(envelope.id, ctx);
                }
                BackendCommand::NotifySystem(event) => {
                    self.handle_system_event(event, ctx).await;
                    self.complete(envelope.id, ctx);
                }
                BackendCommand::Calibration(command) => {
                    if self.handle_calibration(envelope.id, command, ctx).await {
                        // Overrides any weaker cause queued in this batch:
                        // the calibration row is a full restart either way,
                        // and one generation applies both changes at once.
                        reconcile = Some(ReconcileCause::CalibrationChanged);
                    }
                }
                BackendCommand::RunListeningTest(guard) => {
                    if !guard.matches(&self.snapshot)
                        || self.last_listening_test_generation == Some(guard.generation())
                    {
                        self.fail(
                            envelope.id,
                            "the listening test configuration changed or was already used",
                            ctx,
                        );
                    } else {
                        let token = CancellationToken::new();
                        if ctx.capture.lock().await.start_listening_tone(token.clone()) {
                            self.last_listening_test_generation = Some(guard.generation());
                            self.listening_test = Some((guard, token));
                            self.complete(envelope.id, ctx);
                        } else {
                            token.cancel();
                            self.fail(envelope.id, "the listening test audio source is busy", ctx);
                        }
                    }
                }
                BackendCommand::Shutdown => {
                    if let Some((_, token)) = self.listening_test.take() {
                        token.cancel();
                    }
                    self.complete(envelope.id, ctx);
                    shutdown_requested = true;
                }
                other => unreachable!("non-edge command reached the edge queue: {other:?}"),
            }
            if let Some(confirmation) = envelope.confirmation {
                // Exactly this edge just ran. Keep its result in its own slot,
                // not a global allocation-ID watermark. Publish after filling
                // the slot so even a bridge woken immediately sees the result.
                if let Some((_, error)) = self
                    .snapshot
                    .command_outcomes
                    .recent
                    .iter()
                    .find(|(id, _)| *id == envelope.id)
                {
                    if starts_group && error.is_none() {
                        // The durable/run-intent result precedes reconciliation.
                        // Keep this exact sender until the batch owns its actual
                        // session generation; the old phase is not its answer.
                        group_start_confirmations.push(confirmation);
                    } else {
                        let _ = confirmation.send(super::command::CommandConfirmation {
                            revision: self.snapshot.revision + 1,
                            error: *error,
                            session_generation_floor: None,
                        });
                    }
                }
                self.publish();
            }
            if shutdown_requested {
                break;
            }
        }

        if volumes_changed {
            self.remember_effective_volumes(ctx);
            self.forward_effective_volumes(ctx);
        }
        if let Some(cause) = reconcile {
            self.request_reconcile(cause, ctx);
        }
        if !group_start_confirmations.is_empty() {
            // A no-op may truthfully reuse the current attempt. A suspended
            // start cannot: resume must mint the next generation first. The
            // supervisor may subsequently advance beyond this floor itself.
            let floor = if self.suspended || ctx.session_requests.is_none() {
                self.session_generation.saturating_add(1)
            } else {
                self.session_generation
            };
            for confirmation in group_start_confirmations {
                let _ = confirmation.send(super::command::CommandConfirmation {
                    revision: self.snapshot.revision + 1,
                    error: None,
                    session_generation_floor: Some(floor),
                });
            }
            self.publish();
        }
        // No-op and validation failures also need an authoritative result,
        // even when the command changed no durable state to publish.
        let outcomes_unpublished = self
            .state_tx
            .as_ref()
            .is_some_and(|tx| tx.borrow().command_outcomes != self.snapshot.command_outcomes);
        if outcomes_unpublished {
            self.publish();
        }
        shutdown_requested
    }

    // -- durable commit pipeline ---------------------------------------------

    /// Opens the persist-before-publish transaction for one durable command.
    ///
    /// Flow per plan Step 7: candidate ÃƒÂ¢Ã¢â‚¬Â Ã¢â‚¬â„¢ validate ÃƒÂ¢Ã¢â‚¬Â Ã¢â‚¬â„¢ save ÃƒÂ¢Ã¢â‚¬Â Ã¢â‚¬â„¢ on success commit
    /// the durable value, announce the snapshot change, complete; on failure
    /// retain the previous durable value, flag the persistence error, fail +
    /// notice. Either way at most one publication leaves the actor.
    fn commit_durable<'a>(&'a mut self, ctx: &'a ActorContext<'a>, id: u64) -> DurableCommit<'a> {
        DurableCommit {
            actor: self,
            ctx,
            id,
            mutate_persisted: None,
            announce: None,
            refresh_rows: false,
            sync_names: false,
        }
    }

    fn complete(&mut self, id: u64, ctx: &ActorContext<'_>) {
        self.snapshot.command_outcomes.record(id, None);
        let _ = ctx.events_tx.send(BackendEvent::CommandCompleted { id });
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Info,
            DiagnosticPayload::CommandResult { id, error: None },
        );
    }

    fn fail(&mut self, id: u64, message: &str, ctx: &ActorContext<'_>) {
        self.fail_with_kind(id, message, super::model::CommandFailure::Rejected, ctx);
    }

    fn fail_with_kind(
        &mut self,
        id: u64,
        message: &str,
        kind: super::model::CommandFailure,
        ctx: &ActorContext<'_>,
    ) {
        let error = UserFacingError::new(message.to_owned());
        self.snapshot.command_outcomes.record(id, Some(kind));
        let _ = ctx.events_tx.send(BackendEvent::CommandFailed {
            id,
            error: error.clone(),
        });
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Warning,
            DiagnosticPayload::CommandResult {
                id,
                error: Some(error),
            },
        );
    }

    fn note_persistence_failure(&mut self, id: u64, error: &PersistError, ctx: &ActorContext<'_>) {
        // Full details go to logs only; feeds carry pre-redacted summaries so
        // no Windows user path ever crosses them.
        tracing::warn!("persistence save failed for command {id}: {error}");
        self.snapshot.persistence = PersistenceSnapshot::Error;
        let _ = ctx.events_tx.send(BackendEvent::Notice {
            severity: Severity::Error,
            code: NoticeCode::PersistenceWrite,
            scope: ErrorScope::Persistence,
            // Truthful: `DurableCommit::run` skips the announce step on a
            // failed save, so the change is not applied anywhere -- neither
            // on disk nor in memory. Callers that carry an in-memory side
            // effect alongside the transaction owe the same rule and gate it
            // on `CommitOutcome::state_is_intended`; `handle_activate_group`
            // is the one that does.
            message: "saving device state failed; the change was not applied".to_owned(),
        });
        self.fail_with_kind(
            id,
            "the change could not be saved to disk",
            super::model::CommandFailure::Persistence,
            ctx,
        );
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Error,
            DiagnosticPayload::Persistence {
                outcome: PersistenceSnapshot::Error,
            },
        );
    }

    // -- edges ---------------------------------------------------------------

    /// Applies, resets, or exercises the manual calibration.
    ///
    /// Returns whether the caller owes exactly one controlled full-group
    /// restart. Order is persist-then-apply: an alignment that could not be
    /// written must not reach the speakers, or the next launch would silently
    /// play a different one than the user just heard.
    async fn handle_calibration(
        &mut self,
        id: u64,
        command: CalibrationCommand,
        ctx: &ActorContext<'_>,
    ) -> bool {
        // A reset is the recovery action, so it carries no profile to
        // validate: an empty alignment needs no reference receiver and
        // nothing to be relative to.
        let (expected, profile, clearing) = match command {
            CalibrationCommand::ApplyCalibrationProfile {
                expected_desired_revision,
                profile,
            } => (expected_desired_revision, profile, false),
            CalibrationCommand::ResetCalibration {
                expected_desired_revision,
            } => (
                expected_desired_revision,
                CalibrationProfile::default(),
                true,
            ),
            CalibrationCommand::RunCalibrationClickTest => {
                return self.handle_calibration_click_test(id, ctx).await;
            }
        };

        // Lost-update guard: the caller authored against one selection, and
        // an apply that landed after the selection moved would attach delays
        // to receivers the user never compared.
        if expected != self.snapshot.desired_revision {
            self.fail(
                id,
                "the receiver selection changed while calibrating; review it and apply again",
                ctx,
            );
            self.note_calibration_state(CalibrationApplyState::Failed, ctx);
            return false;
        }

        // Both guards below judge a profile, so both are skipped when there
        // is none. Gating the clear on the selection size would withhold the
        // one action that undoes a bad alignment at exactly the moment the
        // user shrank the selection to get out of it -- and the section would
        // stay on disk and come back at every later launch.
        if !clearing {
            // Calibration is relative alignment; one speaker has nothing to be
            // aligned against.
            let active: BTreeSet<ReceiverId> = self.snapshot.desired_members.clone();
            if active.len() < 2 {
                self.fail(id, "calibration needs at least two selected receivers", ctx);
                self.note_calibration_state(CalibrationApplyState::Failed, ctx);
                return false;
            }

            // Validate before writing: the reference must belong to the
            // selection the user judged against, and the shifted values must
            // fit.
            if let Err(error) = normalize_calibration(&active, &profile) {
                tracing::warn!("calibration {id} could not be normalized: {error}");
                self.fail(id, "the calibration values could not be applied", ctx);
                self.note_calibration_state(CalibrationApplyState::Failed, ctx);
                return false;
            }
        }

        let section = (!clearing).then(|| CalibrationStateV2::from(&profile));
        if let Err(error) = ctx.store.save_calibration(section).await {
            self.note_persistence_failure(id, &error, ctx);
            self.note_calibration_state(CalibrationApplyState::Failed, ctx);
            self.publish();
            return false;
        }

        self.calibration = profile;
        self.complete(id, ctx);
        self.note_calibration_state(CalibrationApplyState::PendingRestart, ctx);
        true
    }

    /// Rejects or accepts the shared four-click alignment test.
    ///
    /// The pattern itself carries nothing receiver-specific and travels the
    /// normal shared encode/RTP fan-out, so what a listener hears is only the
    /// per-target presentation clock.
    async fn handle_calibration_click_test(&mut self, id: u64, ctx: &ActorContext<'_>) -> bool {
        if self.active.len() < 2 {
            self.fail(
                id,
                "the alignment test needs at least two connected receivers",
                ctx,
            );
            return false;
        }
        let pattern = calibration_click_pattern(CAPTURE_SAMPLE_RATE, CAPTURE_CHANNELS);
        // Sample values and byte counts are audio material and never reach a
        // record; only the fact that a test was injected does. The lock is
        // held across one synchronous, non-blocking enqueue and nothing else.
        ctx.capture.lock().await.inject_pattern(pattern);
        self.complete(id, ctx);
        // The click test changes no setup input, so it costs no restart.
        false
    }

    /// Closes an open calibration lifecycle once a session at or after the
    /// generation minted for it reports itself live.
    ///
    /// A newer generation counts: every reconcile hands the same stored
    /// profile down, so whatever superseded the calibration restart is
    /// carrying the same delays. Nothing is emitted while no calibration is
    /// waiting, which is the ordinary case for every other restart cause.
    ///
    /// A session that stops before ever becoming active deliberately does not
    /// clear the mark: the profile is persisted and still pending, and a
    /// later Start is exactly the restart it was waiting for.
    fn settle_calibration_generation(&mut self, generation: u64, ctx: &ActorContext<'_>) {
        let Some(pending) = self.calibration_pending_generation else {
            return;
        };
        if generation < pending {
            return;
        }
        self.calibration_pending_generation = None;
        self.note_calibration_state(CalibrationApplyState::Applied, ctx);
    }

    /// Records one calibration lifecycle edge for the diagnostics registry.
    fn note_calibration_state(&self, state: CalibrationApplyState, ctx: &ActorContext<'_>) {
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            match state {
                CalibrationApplyState::Failed => Severity::Warning,
                _ => Severity::Info,
            },
            DiagnosticPayload::CalibrationState(state),
        );
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_save_group(
        &mut self,
        id: u64,
        group_id: Option<SavedGroupId>,
        name: String,
        members: Vec<SavedGroupMember>,
        ctx: &ActorContext<'_>,
    ) {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 64 {
            self.fail(id, "group names need 1-64 characters", ctx);
            return;
        }
        let name_taken = self.persisted.saved_groups.iter().any(|existing| {
            existing.name.eq_ignore_ascii_case(trimmed)
                && group_id.is_none_or(|edit_id| existing.id != edit_id)
        });
        if name_taken {
            self.fail(id, "a group with that name already exists", ctx);
            return;
        }
        let mut seen = BTreeSet::new();
        for member in &members {
            if !seen.insert(member.receiver) {
                self.fail(id, "each receiver may appear once per group", ctx);
                return;
            }
        }
        if members.is_empty() {
            self.fail(id, "a saved group needs at least one receiver", ctx);
            return;
        }

        let resolved_id = group_id.unwrap_or_else(SavedGroupId::generate);
        let group_name = trimmed.to_owned();
        let stored = SavedGroup {
            id: resolved_id,
            name: group_name.clone(),
            members: members.clone(),
        };
        // Saving or editing groups must not touch desired membership or the
        // session generation (spec non-restart row).
        self.commit_durable(ctx, id)
            .mutate_persisted(move |state| {
                state
                    .saved_groups
                    .retain(|existing| existing.id != resolved_id);
                state.saved_groups.push(stored);
                state.saved_groups.sort_by(|a, b| {
                    a.name
                        .to_lowercase()
                        .cmp(&b.name.to_lowercase())
                        .then_with(|| a.id.cmp(&b.id))
                });
            })
            .announce(move |snapshot| {
                snapshot
                    .saved_groups
                    .retain(|existing| existing.id != resolved_id);
                snapshot.saved_groups.push(SavedGroupSnapshot {
                    id: resolved_id,
                    name: group_name,
                    members,
                });
            })
            .then_sync_saved_names()
            .run()
            .await;
    }

    async fn handle_delete_group(
        &mut self,
        id: u64,
        group_id: SavedGroupId,
        ctx: &ActorContext<'_>,
    ) {
        if !self.persisted.saved_groups.iter().any(|g| g.id == group_id) {
            self.complete(id, ctx);
            return;
        }
        self.commit_durable(ctx, id)
            .mutate_persisted(move |state| {
                state
                    .saved_groups
                    .retain(|existing| existing.id != group_id);
            })
            .announce(move |snapshot| {
                snapshot
                    .saved_groups
                    .retain(|existing| existing.id != group_id);
            })
            .then_sync_saved_names()
            .run()
            .await;
    }

    /// Atomic activation transaction: members AND levels are written as ONE
    /// durable candidate; publication happens only after the save succeeded.
    ///
    /// Returns whether the transaction committed (driving at most one new
    /// session generation afterwards).
    async fn handle_activate_group(
        &mut self,
        id: u64,
        group_id: SavedGroupId,
        start: bool,
        ctx: &ActorContext<'_>,
    ) -> bool {
        let Some(group) = self
            .persisted
            .saved_groups
            .iter()
            .find(|group| group.id == group_id)
            .cloned()
        else {
            self.fail(id, "that saved group no longer exists", ctx);
            return false;
        };
        let member_ids: BTreeSet<ReceiverId> =
            group.members.iter().map(|member| member.receiver).collect();
        let levels: Vec<(ReceiverId, Volume)> = group
            .members
            .iter()
            .map(|member| (member.receiver, member.level))
            .collect();
        let membership_differs = member_ids != self.persisted.last_desired_members;
        let for_announce = member_ids.clone();

        let outcome = self
            .commit_durable(ctx, id)
            .mutate_persisted(move |state| {
                state.last_desired_members = member_ids;
                for (receiver, level) in &levels {
                    state.receiver_levels.insert(*receiver, *level);
                }
            })
            .announce(move |snapshot| {
                snapshot.desired_members = for_announce;
                if membership_differs {
                    snapshot.desired_revision = snapshot.desired_revision.saturating_add(1);
                }
            })
            .then_refresh_rows()
            .run()
            .await;

        // Activating a group and declining to start it is the user answering
        // the exact question auto-connect exists to answer. Leaving the launch
        // arm up here would let the backend start, seconds later, precisely
        // the playback this command refused.
        //
        // Gated on the outcome, and deliberately not on `committed`: a group
        // already identical to the stored document commits nothing and still
        // has to disarm the launch. Only a *failed* save must not, because
        // that path tells the user the change was applied neither on disk nor
        // in memory -- and disarming here would be exactly such an in-memory
        // change.
        if !start && outcome.state_is_intended() {
            self.startup_auto_connect_until = None;
        }

        // Deliberately outside the durable transaction. Run intent is not part
        // of the persisted schema (see the note at `SetRunIntent`), and a
        // group whose membership and levels are already exactly what is on
        // disk produces a candidate equal to the stored document -- so
        // `DurableCommit::run` completes the command and returns before it
        // ever reaches `announce`. Carrying the intent in there would turn
        // "start the group I am already pointing at" into a silent no-op
        // reported as success -- and since the stop gate in
        // `request_reconcile`, this is the only path that can start it.
        //
        // A successful write or an accepted identical candidate permits start.
        // Matching IDs alone do not: different levels may have failed to save.
        if start && outcome.state_is_intended() && self.snapshot.run_intent != RunIntent::Running {
            self.snapshot.run_intent = RunIntent::Running;
            self.publish();
            return true;
        }
        outcome.applied()
    }

    /// Applies one platform edge in the order the design mandates.
    async fn handle_system_event(&mut self, event: SystemEvent, ctx: &ActorContext<'_>) {
        match event {
            SystemEvent::Suspending => self.begin_suspend(ctx).await,
            SystemEvent::Resumed => self.begin_resume(),
            SystemEvent::NetworkChanged {
                local_binding_changed,
            } => self.apply_network_change(local_binding_changed, ctx).await,
        }
    }

    /// Suspend: stop producing and streaming, keep wanting to.
    ///
    /// Order is load-bearing. The flag rises before anything is requested, so
    /// the automatic rejoin arm is dead for the whole window in which the
    /// supervisors tear down -- not only from the moment they report back.
    /// Capture is silenced first so the bounded session teardown streams
    /// silence rather than a truncated buffer, and desired membership plus run
    /// intent are deliberately retained: the user stopped nothing, so the
    /// resume has to know what to bring back.
    async fn begin_suspend(&mut self, ctx: &ActorContext<'_>) {
        self.suspended = true;
        // A resume that never completed is void; the machine is going back to
        // sleep. What replaces it is the watchdog: the suspend gate closes
        // every automatic path and every reconciling user command, so it may
        // never depend on a broadcast that Windows is free to swallow. An
        // arriving `Resumed` simply overwrites this deadline with its own,
        // much shorter, interface settle.
        self.resume_at = Some(self.clock.now() + SUSPEND_WATCHDOG);
        request_capture_control(&Arc::clone(ctx.capture), CaptureControl::Suspend).await;
        if let Some(requests) = ctx.session_requests.as_ref() {
            // The supervisor honours `TEARDOWN_PER_RECEIVER`/`TEARDOWN_GROUP`
            // for this request, so the four-second budget is enforced where
            // the transports actually live.
            ignore_send_error(requests.try_send(SessionRequest::Suspend));
        }
        let mut next = self.snapshot.clone();
        next.session.resume_pending = true;
        next.session.audio_flow = AudioFlow::SilenceBridged;
        self.maybe_publish(next);
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Info,
            DiagnosticPayload::SystemTransition {
                transition: SystemTransition::Suspending,
            },
        );
    }

    /// Resume: arm the interface settle; the suspend gate stays shut until it
    /// elapses.
    fn begin_resume(&mut self) {
        self.resume_at = Some(self.clock.now() + RESUME_SETTLE);
    }

    /// Time left on the post-resume interface settle, if one is armed.
    fn resume_delay(&self) -> Option<Duration> {
        let now = self.clock.now();
        self.resume_at
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// The interface settle elapsed: rebuild everything that slept.
    ///
    /// Every pre-suspend RTSP/RTP/PTP object is already gone -- the suspend
    /// tore the transport down -- and the discovery and capture generations
    /// are replaced here, so nothing that observed the old addresses can
    /// survive. The rebuild targets the NEWEST desired set, not whatever the
    /// session supervisor retained when the machine went to sleep.
    async fn finish_resume(&mut self, ctx: &ActorContext<'_>) {
        self.resume_at = None;
        self.suspended = false;
        request_discovery_control(ctx.discovery, DiscoveryControl::RestartAfterNetworkChange).await;
        request_capture_control(&Arc::clone(ctx.capture), CaptureControl::Resume).await;
        // Backoff rungs are policy for a running machine; a sleep is not a
        // failed attempt, so nothing may be held back from the rebuild.
        self.release_backoff(ctx);
        let mut next = self.snapshot.clone();
        next.session.resume_pending = false;
        self.maybe_publish(next);
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Info,
            DiagnosticPayload::SystemTransition {
                transition: SystemTransition::Resumed,
            },
        );
        if self.snapshot.run_intent == RunIntent::Running {
            // This rebuild carries the whole desired set, so it *is* the
            // restart the rejoin spacing counts -- otherwise the arm would
            // spend a second generation on receivers this one already brings.
            self.rejoin.note_restart();
            self.request_reconcile(ReconcileCause::Resume, ctx);
        }
    }

    // -- startup auto-connect (Task 17 Step 5) --------------------------------

    /// Time left until the persisted auto-connect policy could act.
    ///
    /// `None` disables the arm: either the policy is spent, or no desired
    /// receiver currently offers a castable service, in which case no amount
    /// of waiting would change anything and only a new discovery sighting can
    /// re-open the window.
    fn auto_connect_delay(&self) -> Option<Duration> {
        let until = self.startup_auto_connect_until?;
        if self.suspended {
            return None;
        }
        let now = self.clock.now();
        if now > until {
            return None;
        }
        self.snapshot
            .desired_members
            .iter()
            .filter_map(|receiver| self.rejoin.rejoin_eligible_at(*receiver))
            .min()
            // A window that would close before the receiver settles is not a
            // shorter wait, it is no wait at all: waking up for it could only
            // ever hit the expiry branch below.
            .filter(|at| *at <= until)
            .map(|at| at.saturating_duration_since(now))
    }

    /// Spends the one automatic start this process owes its persisted policy.
    ///
    /// Membership is deliberately untouched: the only state this changes is
    /// the run intent, so a receiver the user never selected can never be
    /// pulled in by a launch. The desired set is whatever the previous run
    /// committed, and it stays that way.
    fn apply_startup_auto_connect(&mut self, ctx: &ActorContext<'_>) {
        let Some(until) = self.startup_auto_connect_until else {
            return;
        };
        if self.suspended {
            return;
        }
        if self.clock.now() > until {
            // The launch this policy belonged to is over. Clearing rather
            // than returning is what keeps the promise in the field's own
            // documentation: past the window there is no arm left to spend.
            self.startup_auto_connect_until = None;
            return;
        }
        if self.snapshot.run_intent != RunIntent::Stopped {
            // Something already answered the question this arm exists to
            // answer. Disarming rather than returning matters: the deadline
            // that woke us has passed, so leaving the arm up would re-arm it
            // with zero delay on every loop iteration and spin the backend.
            self.startup_auto_connect_until = None;
            return;
        }
        let ready = self
            .snapshot
            .desired_members
            .iter()
            .any(|receiver| self.rejoin.may_rejoin(*receiver));
        if !ready {
            return;
        }
        self.startup_auto_connect_until = None;
        self.snapshot.run_intent = RunIntent::Running;
        self.publish();
        self.request_reconcile(ReconcileCause::Membership, ctx);
    }

    /// Applies one coalesced network edge.
    ///
    /// Discovery always restarts: a new generation is how stale addresses are
    /// invalidated. The AirPlay group is rebuilt only when an active local
    /// binding actually moved, and that rebuild is a distinct network
    /// recovery, so the thirty-second automatic-rejoin spacing never delays
    /// it.
    async fn apply_network_change(&mut self, local_binding_changed: bool, ctx: &ActorContext<'_>) {
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Info,
            DiagnosticPayload::SystemTransition {
                transition: SystemTransition::NetworkChanged {
                    local_binding_changed,
                },
            },
        );
        if self.suspended {
            // The resume path owns the rebuild; doing it here would race the
            // teardown still in flight.
            return;
        }
        request_discovery_control(ctx.discovery, DiscoveryControl::RestartAfterNetworkChange).await;
        if local_binding_changed && self.snapshot.run_intent == RunIntent::Running {
            self.release_backoff(ctx);
            self.rejoin.note_restart();
            self.request_reconcile(ReconcileCause::NetworkRebind, ctx);
        }
    }

    /// Frees every receiver serving a backoff rung so a platform-level
    /// recovery rebuilds the complete desired set instead of a subset.
    fn release_backoff(&mut self, ctx: &ActorContext<'_>) {
        let waiting: Vec<ReceiverId> = self
            .machines
            .iter()
            .filter(|(_, machine)| machine.in_backoff())
            .map(|(id, _)| *id)
            .collect();
        for receiver in waiting {
            self.retry.clear_deadline(receiver);
            self.drive(receiver, ctx, |machine| machine.retry_now());
        }
    }

    // -- reconciliation and volume fan-out ------------------------------------

    fn request_reconcile(&mut self, cause: ReconcileCause, ctx: &ActorContext<'_>) {
        let Some(requests) = ctx.session_requests.as_ref() else {
            return;
        };
        debug_assert!(
            cause.matrix_row().requires_full_restart(),
            "reconciliation was requested for a non-restart matrix row"
        );
        // A sleeping machine reconciles nothing. Membership may still change
        // while the lid is shut -- the shell keeps running -- but building a
        // session against a torn-down network stack would only fight the
        // teardown the suspend just requested. `finish_resume` rebuilds
        // against the newest desired set once the interfaces have settled.
        if self.suspended {
            return;
        }
        // A stopped app builds nothing, whatever the cause. Every row of the
        // restart matrix reaches this point -- a tick in the receiver list, a
        // saved group activated without starting it, a manual retry, a
        // latency preset, a lost primary, an interface that moved -- and none
        // of them is a reason to put audio back on a receiver the user
        // stopped. The selection itself is untouched: it was persisted and
        // published before this call and stays visible, so pressing Start
        // later builds against the newest set. What the stopped intent
        // resolves to is the desired set handed DOWNWARD: empty, which is
        // also how the Stop that caused it tears the live session down.
        let stopped = self.snapshot.run_intent == RunIntent::Stopped;
        if stopped && self.active.is_empty() && self.pending_desired.is_empty() {
            // Nothing is up and nothing may be built: holding the supervisor
            // at "no session" it already occupies would only spend a
            // generation and announce a restart that never happened.
            return;
        }
        self.session_generation = self.session_generation.saturating_add(1);
        self.live_session_generation = Some(self.session_generation);
        // The generation is minted here, so this is the only place that can
        // name the session a calibration change is waiting on.
        if matches!(cause, ReconcileCause::CalibrationChanged) {
            self.calibration_pending_generation = Some(self.session_generation);
        }
        self.publish_generations();
        // Receivers serving their backoff stay out until their deadline (or a
        // manual retry) releases them, and an undiscoverable receiver cannot
        // be connected at all.
        let desired_devices: Vec<Device> = if stopped {
            Vec::new()
        } else {
            self.snapshot
                .desired_members
                .iter()
                .filter(|id| {
                    self.castable(id)
                        && !self
                            .machines
                            .get(id)
                            .is_some_and(|machine| machine.in_backoff())
                })
                .filter_map(|id| {
                    self.inventory
                        .get(id)
                        .and_then(|entry| entry.device.clone())
                })
                .collect()
        };
        self.pending_desired = desired_devices
            .iter()
            .map(|device| ReceiverId::from(device.id.clone()))
            .collect();
        // Seed the supervisor's latest-value source before the connect task
        // is admitted. This is also safe for a reconnect: the source remains
        // alive across generations and is refreshed after every durable save.
        self.remember_effective_volumes(ctx);
        let preferred_primary = self
            .primary
            .filter(|primary| self.snapshot.desired_members.contains(primary));
        let latency = self
            .persisted
            .latency_preset
            .enabled_config()
            .unwrap_or(LatencyConfig {
                sender_buffer_ms: 2_000,
                render_delay_ms: 200,
            });
        emit_diagnostics(
            &ctx.diagnostics,
            self.generations(),
            Severity::Info,
            DiagnosticPayload::SessionRestart {
                generation: self.session_generation,
                reason: cause.reason(),
            },
        );
        ignore_send_error(requests.try_send(SessionRequest::Reconcile {
            generation: self.session_generation,
            desired: desired_devices,
            preferred_primary,
            latency,
            calibration: self.calibration.clone(),
            // The intent travels with the set instead of being inferred from
            // its size: only a stopped app means "no session wanted". An
            // empty set produced by the backoff/castable filters above still
            // belongs to an app that wants to run.
            teardown_only: stopped,
        }));
    }

    /// Receivers an automatic rejoin may pull back in at `now`, plus the
    /// earliest instant at which the next one could become eligible.
    ///
    /// One predicate serves both the timer arm and the firing path, so the
    /// arm can never wake on a deadline the firing path then declines --
    /// which on a monotonic clock is what keeps it from spinning.
    ///
    /// Three gates apply together, and the latest of them wins:
    ///  * the receiver's own backoff rung, so a failing receiver is not
    ///    hammered;
    ///  * two continuous seconds of castable discovery, so a flapping mDNS
    ///    record cannot pass for a recovered receiver;
    ///  * thirty seconds since the last rejoin restart, so no single receiver
    ///    can keep interrupting the healthy members.
    ///
    /// A suspend closes the arm outright. `SessionUpdate::Stopped` returns
    /// every streaming machine to `Discovered` while `RunIntent` stays
    /// `Running` -- without this gate the whole desired set would come up as
    /// due the instant the machine goes to sleep, and the controller would
    /// rebuild the session the supervisor is tearing down.
    fn rejoin_candidates(&self, now: Instant) -> (Vec<ReceiverId>, Option<Instant>) {
        if self.suspended || self.snapshot.run_intent != RunIntent::Running {
            return (Vec::new(), None);
        }
        let group_allowed_at = self.rejoin.group_allowed_at();
        let mut due = Vec::new();
        let mut next: Option<Instant> = None;
        for receiver in &self.snapshot.desired_members {
            if !self
                .machines
                .get(receiver)
                .is_some_and(ReceiverMachine::awaiting_rejoin)
            {
                continue;
            }
            // No castable service means no eligibility instant at all; the
            // arm stays disabled until discovery reports the receiver again.
            let Some(mut at) = self.rejoin.rejoin_eligible_at(*receiver) else {
                continue;
            };
            if let Some(retry_at) = self.retry.next_retry(*receiver) {
                at = at.max(retry_at);
            }
            if let Some(allowed) = group_allowed_at {
                at = at.max(allowed);
            }
            if at <= now {
                due.push(*receiver);
            } else {
                next = Some(next.map_or(at, |earliest: Instant| earliest.min(at)));
            }
        }
        (due, next)
    }

    /// Time left until the next automatic rejoin becomes possible.
    ///
    /// `None` disables the arm entirely, so an idle backend parks on its
    /// event sources instead of waking on a timer that has nothing to do.
    fn next_retry_delay(&self) -> Option<Duration> {
        let now = self.clock.now();
        let (due, next) = self.rejoin_candidates(now);
        if !due.is_empty() {
            return Some(Duration::ZERO);
        }
        next.map(|at| at.saturating_duration_since(now))
    }

    /// Rejoins every eligible receiver through ONE coordinated full restart.
    ///
    /// The running group streamer has no mutable target insertion, so a
    /// rejoin is a new session generation against the complete newest desired
    /// set -- never a metadata-only `add_to_group()`. One request per due
    /// batch, never one per receiver: should best-effort setup omit a
    /// receiver again, only that receiver continues its own backoff while the
    /// successful subset keeps streaming.
    fn fire_due_retries(&mut self, ctx: &ActorContext<'_>) {
        let now = self.clock.now();
        let (due, _) = self.rejoin_candidates(now);
        if due.is_empty() {
            return;
        }
        for receiver in due {
            self.retry.clear_deadline(receiver);
            self.drive(receiver, ctx, |machine| machine.retry_now());
        }
        self.rejoin.note_restart();
        self.republish_rows();
        self.request_reconcile(ReconcileCause::Retry, ctx);
    }

    fn forward_effective_volumes(&self, ctx: &ActorContext<'_>) {
        let Some(requests) = ctx.session_requests.as_ref() else {
            return;
        };
        for receiver in &self.active {
            let level = self
                .persisted
                .receiver_levels
                .get(receiver)
                .copied()
                .unwrap_or(Volume::UNITY);
            let volume =
                effective_volume(self.persisted.master_volume, level, self.persisted.muted);
            ignore_send_error(requests.try_send(SessionRequest::SetVolume {
                receiver: *receiver,
                volume: volume.get(),
            }));
        }
    }

    /// Refreshes the supervisor's setup-time levels without touching the
    /// running stream. Every desired receiver is included so the future
    /// connection barrier observes durable changes made while it is pairing.
    fn remember_effective_volumes(&self, ctx: &ActorContext<'_>) {
        let Some(requests) = ctx.session_requests.as_ref() else {
            return;
        };
        for receiver in &self.snapshot.desired_members {
            let level = self
                .persisted
                .receiver_levels
                .get(receiver)
                .copied()
                .unwrap_or(Volume::UNITY);
            let volume =
                effective_volume(self.persisted.master_volume, level, self.persisted.muted);
            requests.remember_volume(*receiver, volume.get());
        }
    }

    fn sync_saved_names(&mut self) {
        self.saved_names = self
            .persisted
            .saved_groups
            .iter()
            .flat_map(|group| {
                group
                    .members
                    .iter()
                    .map(|member| (member.receiver, member.last_known_name.clone()))
            })
            .collect();
    }

    // -- supervisor updates -----------------------------------------------------

    fn apply_discovery_update(&mut self, update: DiscoveryUpdate, ctx: &ActorContext<'_>) {
        match update {
            DiscoveryUpdate::Started { generation } => {
                self.seen_discovery_generation = generation.max(self.seen_discovery_generation);
                self.publish_generations();
                let mut next = self.snapshot.clone();
                next.discovery.generation = generation;
                next.discovery.phase = DiscoveryPhase::Running;
                self.maybe_publish(next);
            }
            DiscoveryUpdate::Receiver { generation, event } => {
                if generation < self.seen_discovery_generation {
                    return;
                }
                match event {
                    airplay_discovery::BrowseEvent::Added(device)
                    | airplay_discovery::BrowseEvent::Updated(device) => {
                        let id = ReceiverId::from(device.id.clone());
                        let castable = device.supports_airplay2()
                            && device.socket_addr().is_some_and(|addr| addr.is_ipv4());
                        let entry = self.inventory.entry(id).or_insert_with(|| InventoryEntry {
                            name: String::new(),
                            castable: false,
                            available: true,
                            device: None,
                        });
                        entry.name = device.name.clone();
                        entry.castable = castable;
                        entry.available = true;
                        entry.device = Some(device);
                    }
                    airplay_discovery::BrowseEvent::Removed(device_id) => {
                        let key = ReceiverId::from(device_id);
                        if let Some(entry) = self.inventory.get_mut(&key) {
                            entry.available = false;
                        }
                    }
                }
                self.refresh_receivers(ctx);
            }
            DiscoveryUpdate::Failed { generation, .. } => {
                if generation < self.seen_discovery_generation {
                    return;
                }
                self.discovery_failures = self.discovery_failures.saturating_add(1);
                let attempt = self.discovery_failures;
                // Wall-clock approximation of the monotonic deadline; exact
                // retry scheduling stays inside the discovery supervisor.
                let retry_at = SystemTime::now();
                let mut next = self.snapshot.clone();
                next.discovery.phase = DiscoveryPhase::Retrying { attempt, retry_at };
                let changed = next != self.snapshot;
                self.maybe_publish(next);
                if changed {
                    let _ = ctx.events_tx.send(BackendEvent::Notice {
                        severity: Severity::Warning,
                        code: NoticeCode::DiscoveryFailed,
                        scope: ErrorScope::Discovery,
                        message: "receiver discovery is retrying in the background".to_owned(),
                    });
                }
            }
        }
    }

    fn apply_endpoints(
        &mut self,
        result: Result<
            Vec<crate::backend::model::AudioEndpoint>,
            crate::backend::capture::CaptureError,
        >,
    ) {
        match result {
            Ok(mut endpoints) => {
                crate::backend::capture::sort_endpoints(&mut endpoints);
                let mut next = self.snapshot.clone();
                next.audio_source.active_endpoints = Some(endpoints);
                next.audio_source.refresh_failed = false;
                self.maybe_publish(next);
            }
            Err(_) => {
                let mut next = self.snapshot.clone();
                next.audio_source.refresh_failed = true;
                self.maybe_publish(next);
                tracing::warn!(
                    "Windows playback device refresh failed; retaining last successful inventory"
                );
            }
        }
    }

    fn apply_capture_update(&mut self, update: CaptureUpdate) {
        // Every capture edge except the final stop names its generation; the
        // newest one wins so a late edge from a cancelled worker cannot pull
        // the correlation key backwards.
        match &update {
            CaptureUpdate::Started {
                capture_generation, ..
            }
            | CaptureUpdate::Ready {
                capture_generation, ..
            }
            | CaptureUpdate::FrameDrop {
                capture_generation, ..
            }
            | CaptureUpdate::Recovering {
                capture_generation, ..
            }
            | CaptureUpdate::Suspended {
                capture_generation, ..
            } => self.capture_generation = self.capture_generation.max(*capture_generation),
            // A stop ends the last generation; nothing is live afterwards.
            CaptureUpdate::Stopped { .. } => self.capture_generation = 0,
        }
        self.publish_generations();
        match update {
            CaptureUpdate::Ready { endpoint, .. } => {
                let mut next = self.snapshot.clone();
                next.audio_source.state = AudioSourceState::Capturing;
                next.audio_source.captured_endpoint = Some(endpoint);
                next.audio_source.windows_input_peak_permille = None;
                next.session.audio_flow = if self.active.is_empty() {
                    AudioFlow::SilenceBridged
                } else {
                    AudioFlow::Live
                };
                self.maybe_publish(next);
            }
            CaptureUpdate::Suspended { .. } => {
                // Not `Recovering`: nothing is being retried, the machine is
                // asleep. The stable bridge keeps pacing silence underneath.
                let mut next = self.snapshot.clone();
                next.audio_source.state = AudioSourceState::Unavailable;
                next.audio_source.captured_endpoint = None;
                next.audio_source.windows_input_peak_permille = None;
                next.session.audio_flow = AudioFlow::SilenceBridged;
                self.maybe_publish(next);
            }
            CaptureUpdate::Recovering { .. } => {
                let mut next = self.snapshot.clone();
                next.audio_source.state = AudioSourceState::Recovering;
                next.audio_source.captured_endpoint = None;
                next.audio_source.windows_input_peak_permille = None;
                next.session.audio_flow = AudioFlow::SilenceBridged;
                self.maybe_publish(next);
            }
            CaptureUpdate::Stopped { .. } => {
                let mut next = self.snapshot.clone();
                next.audio_source.state = AudioSourceState::Unavailable;
                next.audio_source.captured_endpoint = None;
                next.audio_source.windows_input_peak_permille = None;
                next.session.audio_flow = AudioFlow::SilenceBridged;
                self.maybe_publish(next);
            }
            CaptureUpdate::Started { .. } => {
                let mut next = self.snapshot.clone();
                next.audio_source.windows_input_peak_permille = None;
                self.maybe_publish(next);
            }
            CaptureUpdate::FrameDrop { dropped, .. } => {
                let mut next = self.snapshot.clone();
                next.audio_source.pcm_frames_dropped_total =
                    next.audio_source.pcm_frames_dropped_total.max(dropped);
                self.maybe_publish(next);
            }
        }
    }

    fn apply_capture_telemetry(&mut self, telemetry: (Option<u16>, u64)) {
        let mut next = self.snapshot.clone();
        next.audio_source.windows_input_peak_permille = telemetry.0;
        next.audio_source.pcm_frames_dropped_total =
            next.audio_source.pcm_frames_dropped_total.max(telemetry.1);
        self.maybe_publish(next);
    }

    /// Records one receiver-local failure: terminal state, backoff deadline,
    /// and the scheduled-retry diagnostic. Never restarts anything by itself.
    fn note_receiver_failure(
        &mut self,
        receiver: ReceiverId,
        retryable: bool,
        error: UserFacingError,
        ctx: &ActorContext<'_>,
    ) {
        let failure = error.clone();
        self.drive(receiver, ctx, move |machine| {
            machine.fail(retryable, failure)
        });
        if !retryable {
            return;
        }
        let now = self.clock.now();
        // Continuous health is exactly the span between this receiver's
        // `Active` edge and this failure: had a probe failed in between, the
        // failure would have arrived earlier. Evaluating the sixty-second
        // window here therefore resets the ladder without needing a periodic
        // "still healthy" edge on the update queue.
        self.retry.note_streaming(&self.policy, now, receiver);
        let deadline = self.retry.note_failure(&self.policy, now, receiver);
        let attempt = self.retry.attempt(receiver);
        // The ladder is monotonic; the published deadline is its wall-clock
        // projection so the shell can render a countdown.
        let retry_at = SystemTime::now() + deadline.saturating_duration_since(now);
        self.drive(receiver, ctx, move |machine| {
            machine.wait_retry(attempt, retry_at)
        });
        emit_receiver_diagnostic(
            &ctx.diagnostics,
            self.generations(),
            receiver,
            Severity::Info,
            DiagnosticPayload::Retry {
                receiver: Some(receiver),
                attempt,
                next_at: retry_at,
            },
        );
    }

    fn apply_session_update(&mut self, update: SessionUpdate, ctx: &ActorContext<'_>) {
        match update {
            SessionUpdate::Starting { generation } => {
                // The supervisor mints its own generation for platform edges
                // (`NetworkChanged`, `Resume`). Adopting it here keeps the two
                // counters aligned; otherwise the next controller-side
                // reconcile would re-send an already-accepted number and the
                // supervisor's strictly-newer gate would silently drop it.
                self.session_generation = self.session_generation.max(generation);
                self.live_session_generation = Some(self.session_generation);
                self.publish_generations();
                for receiver in self.pending_desired.clone() {
                    let attempt = self.retry.attempt(receiver);
                    self.drive(receiver, ctx, move |machine| {
                        machine.begin_generation(generation, attempt)
                    });
                }
                let mut next = self.snapshot.clone();
                next.session.phase = SessionPhase::Starting { generation };
                next.session.desired = next.desired_members.clone();
                next.session.failed.clear();
                self.rebuild_receiver_rows(&mut next);
                self.maybe_publish(next);
            }
            SessionUpdate::Active {
                generation,
                primary,
                members,
                partial_failures,
            } => {
                self.primary = Some(primary);
                self.active = members.clone();
                self.forward_effective_volumes(ctx);
                self.settle_calibration_generation(generation, ctx);
                let single = members.len() <= 1;
                for receiver in &members {
                    let role = if single {
                        ReceiverRole::Single
                    } else {
                        role_of(Some(&primary), receiver)
                    };
                    self.drive(*receiver, ctx, move |machine| machine.enter_streaming(role));
                }
                let now = self.clock.now();
                for receiver in &members {
                    // Arms (or advances) the sixty-second healthy window that
                    // resets this receiver's attempt counter.
                    self.retry.note_streaming(&self.policy, now, *receiver);
                }
                let failed: BTreeSet<ReceiverId> = partial_failures
                    .iter()
                    .map(|failure| ReceiverId::from(failure.receiver.clone()))
                    .collect();
                for failure in &partial_failures {
                    let receiver = ReceiverId::from(failure.receiver.clone());
                    let retryable = failure.retryable;
                    let error = setup_failure_error(failure);
                    self.note_receiver_failure(receiver, retryable, error, ctx);
                }
                // Closing the generation boundary from the other side: a
                // receiver this generation was asked to connect that turns up
                // in neither list would otherwise sit in `Connecting` forever.
                let missing: Vec<ReceiverId> = self
                    .pending_desired
                    .iter()
                    .copied()
                    .filter(|receiver| {
                        !self.active.contains(receiver) && !failed.contains(receiver)
                    })
                    .collect();
                for receiver in missing {
                    self.note_receiver_failure(
                        receiver,
                        true,
                        UserFacingError::new("the receiver did not join the group"),
                        ctx,
                    );
                }
                let mut next = self.snapshot.clone();
                next.session.phase =
                    derive_phase(&next.desired_members, &members, next.run_intent, generation);
                next.session.desired = next.desired_members.clone();
                next.session.active = members;
                next.session.failed = failed;
                next.session.primary = Some(primary);
                next.session.audio_flow = if next.audio_source.state == AudioSourceState::Capturing
                {
                    AudioFlow::Live
                } else {
                    AudioFlow::SilenceBridged
                };
                self.rebuild_receiver_rows(&mut next);
                self.maybe_publish(next);
            }
            SessionUpdate::MemberControlFailed {
                generation,
                receiver,
                error,
            } => {
                // The stream is untouched, so there is nothing to publish and
                // nothing to reconcile -- only something to say. A report from
                // a generation the controller has already replaced describes a
                // session that no longer exists and is dropped.
                if generation < self.session_generation {
                    return;
                }
                emit_receiver_diagnostic(
                    &ctx.diagnostics,
                    self.generations(),
                    receiver,
                    Severity::Warning,
                    DiagnosticPayload::Message(error.as_str().to_owned()),
                );
            }
            SessionUpdate::MemberFailed {
                generation,
                receiver,
                primary,
                error,
            } => {
                self.active.remove(&receiver);
                if primary && self.primary == Some(receiver) {
                    self.primary = None;
                }
                self.note_receiver_failure(receiver, true, error, ctx);
                let mut next = self.snapshot.clone();
                next.session.phase = derive_phase(
                    &next.desired_members,
                    &self.active,
                    next.run_intent,
                    generation,
                );
                next.session.active = self.active.clone();
                next.session.failed.insert(receiver);
                if primary {
                    next.session.primary = None;
                }
                self.rebuild_receiver_rows(&mut next);
                self.maybe_publish(next);

                // Full-restart matrix decides, nothing else: losing the PTP
                // primary invalidates group timing, while a secondary failure
                // must leave the survivors streaming.
                let row = if primary {
                    ReconfigureCause::PrimaryFailure
                } else {
                    ReconfigureCause::SecondaryFailure
                };
                if row.requires_full_restart() {
                    // The rebuild below takes the whole desired set with it,
                    // so it *is* the restart the spacing gate counts. Without
                    // this the rejoin arm would still see its candidates as
                    // due -- `begin_generation` only reaches them when
                    // `Starting` comes back -- and would immediately spend a
                    // second generation on receivers this one already carries.
                    self.rejoin.note_restart();
                    self.request_reconcile(ReconcileCause::PrimaryFailure, ctx);
                }
            }
            SessionUpdate::Recovering {
                generation, reason, ..
            } => {
                self.session_generation = self.session_generation.max(generation);
                self.live_session_generation = Some(self.session_generation);
                self.publish_generations();
                let mut next = self.snapshot.clone();
                next.session.phase = SessionPhase::Restarting { generation, reason };
                self.maybe_publish(next);
            }
            SessionUpdate::Stopped { generation } => {
                // Nothing is live afterwards, so nothing may claim to have
                // been written under a session -- the same rule capture
                // already follows for its own counter.
                self.live_session_generation = None;
                self.publish_generations();
                self.active.clear();
                self.primary = None;
                for machine in self.machines.values_mut() {
                    machine.leave_session();
                }
                let mut next = self.snapshot.clone();
                next.session.phase = SessionPhase::Stopped;
                next.session.active.clear();
                next.session.primary = None;
                next.session.audio_flow = AudioFlow::SilenceBridged;
                let _ = generation;
                self.rebuild_receiver_rows(&mut next);
                self.maybe_publish(next);
            }
        }
    }
}

/// Renders one best-effort setup failure as a pre-redacted summary.
fn setup_failure_error(failure: &MemberFailure) -> UserFacingError {
    UserFacingError::new(format!(
        "receiver rejected {:?} during setup",
        snapshot_phase(failure.phase)
    ))
}

/// Emits one receiver-scoped diagnostic under the live generations.
fn emit_receiver_diagnostic(
    sender: &broadcast::Sender<DiagnosticEvent>,
    generations: Generations,
    receiver: ReceiverId,
    severity: Severity,
    payload: DiagnosticPayload,
) {
    let category = category_of(payload.kind());
    let _ = sender.send(DiagnosticEvent {
        monotonic_ns: monotonic_ns_now(),
        wall_time: SystemTime::now(),
        session_generation: generations.session,
        discovery_generation: generations.discovery,
        capture_generation: generations.capture,
        receiver: Some(receiver),
        severity,
        category,
        payload,
    });
}

fn role_of(primary: Option<&ReceiverId>, id: &ReceiverId) -> ReceiverRole {
    match primary {
        Some(primary) if primary == id => ReceiverRole::Primary,
        _ => ReceiverRole::Secondary,
    }
}

/// Derives Streaming/Degraded/Stopped from desired versus active membership.
///
/// Task 15 scope: Connecting/SettingUp/RetryWaiting row states and terminal
/// Failed classification land with Task 16 health wiring; during setup the
/// supervisor-reported phase stands unchanged.
fn derive_phase(
    desired: &BTreeSet<ReceiverId>,
    active: &BTreeSet<ReceiverId>,
    intent: RunIntent,
    generation: u64,
) -> SessionPhase {
    if intent == RunIntent::Stopped {
        return SessionPhase::Stopped;
    }
    if !desired.is_empty() && active == desired {
        SessionPhase::Streaming { generation }
    } else if !active.is_empty() {
        SessionPhase::Degraded { generation }
    } else {
        SessionPhase::Starting { generation }
    }
}

/// Builder-style persist-before-publish transaction assembled by
/// [`ControllerActor::commit_durable`].
#[allow(clippy::type_complexity)] // boxed transaction hooks read clearer than type aliases here
struct DurableCommit<'a> {
    actor: &'a mut ControllerActor,
    ctx: &'a ActorContext<'a>,
    id: u64,
    mutate_persisted: Option<Box<dyn FnOnce(&mut PersistedStateV1) + 'a>>,
    announce: Option<Box<dyn FnOnce(&mut DeviceSnapshot) + 'a>>,
    refresh_rows: bool,
    sync_names: bool,
}

/// How one [`DurableCommit`] ended.
///
/// The three non-`Applied` outcomes are deliberately distinct: a caller that
/// has to undo an in-memory side effect must be able to tell "the save blew
/// up" from "there was nothing to save", because only the first one is
/// reported to the user as *the change was not applied*.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitOutcome {
    /// The candidate was written and announced.
    Applied,
    /// The candidate equalled the stored document, or there was none.
    Unchanged,
    /// The candidate failed validation; nothing was written.
    Rejected,
    /// The store rejected the write; nothing was written or announced.
    Failed,
}

impl CommitOutcome {
    /// Whether a semantic change was applied.
    fn applied(self) -> bool {
        self == Self::Applied
    }

    /// Whether the durable state is exactly what the caller asked for --
    /// either because it was written now or because it already was.
    ///
    /// This is the gate for in-memory side effects that belong to the same
    /// user intent: they may run when nothing needed writing, but not when a
    /// write was attempted and failed.
    fn state_is_intended(self) -> bool {
        matches!(self, Self::Applied | Self::Unchanged)
    }
}

impl<'a> DurableCommit<'a> {
    fn mutate_persisted(mut self, mutate: impl FnOnce(&mut PersistedStateV1) + 'a) -> Self {
        self.mutate_persisted = Some(Box::new(mutate));
        self
    }

    fn announce(mut self, announce: impl FnOnce(&mut DeviceSnapshot) + 'a) -> Self {
        self.announce = Some(Box::new(announce));
        self
    }

    fn then_refresh_rows(mut self) -> Self {
        self.refresh_rows = true;
        self
    }

    fn then_sync_saved_names(mut self) -> Self {
        self.sync_names = true;
        self
    }

    /// Runs the transaction and reports how it ended.
    async fn run(self) -> CommitOutcome {
        let Self {
            actor,
            ctx,
            id,
            mutate_persisted,
            announce,
            refresh_rows,
            sync_names,
        } = self;
        let Some(mutate_persisted) = mutate_persisted else {
            actor.complete(id, ctx);
            return CommitOutcome::Unchanged;
        };
        let mut candidate = actor.persisted.clone();
        mutate_persisted(&mut candidate);
        if let Err(error) = candidate.validate() {
            tracing::warn!("command {id} produced invalid state: {error}");
            actor.fail(id, "the requested change could not be applied", ctx);
            return CommitOutcome::Rejected;
        }
        if candidate == actor.persisted {
            // Duplicate complete values are accepted no-ops.
            actor.complete(id, ctx);
            return CommitOutcome::Unchanged;
        }
        match ctx.store.save(&candidate).await {
            Ok(()) => {
                actor.persisted = candidate;
                actor.snapshot.receiver_levels = actor.persisted.receiver_levels.clone();
                if let Some(announce) = announce {
                    announce(&mut actor.snapshot);
                }
                if refresh_rows {
                    let mut next = actor.snapshot.clone();
                    actor.rebuild_receiver_rows(&mut next);
                    actor.snapshot = next;
                }
                if sync_names {
                    actor.sync_saved_names();
                }
                actor.complete(id, ctx);
                actor.publish();
                CommitOutcome::Applied
            }
            Err(error) => {
                actor.note_persistence_failure(id, &error, ctx);
                actor.publish();
                CommitOutcome::Failed
            }
        }
    }
}

fn ignore_send_error(result: Result<(), SessionSendError>) {
    if let Err(SessionSendError::Closed) = result {
        tracing::warn!("session supervisor is no longer accepting work");
    }
}

/// Emits one session-scoped diagnostic under the live generations.
fn emit_diagnostics(
    sender: &broadcast::Sender<DiagnosticEvent>,
    generations: Generations,
    severity: Severity,
    payload: DiagnosticPayload,
) {
    let category = category_of(payload.kind());
    let _ = sender.send(DiagnosticEvent {
        monotonic_ns: monotonic_ns_now(),
        wall_time: SystemTime::now(),
        session_generation: generations.session,
        discovery_generation: generations.discovery,
        capture_generation: generations.capture,
        receiver: None,
        severity,
        category,
        payload,
    });
}

fn category_of(kind: DiagnosticKind) -> DiagnosticCategory {
    match kind {
        DiagnosticKind::Persistence => DiagnosticCategory::Persistence,
        DiagnosticKind::CommandResult | DiagnosticKind::CommandCoalesced => {
            DiagnosticCategory::Command
        }
        DiagnosticKind::SessionRestart | DiagnosticKind::SessionTransition => {
            DiagnosticCategory::Session
        }
        DiagnosticKind::WorkerShutdown => DiagnosticCategory::Worker,
        _ => DiagnosticCategory::General,
    }
}

fn monotonic_ns_now() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn coalescing_master_into_start_keeps_the_drag_receipt_pending() {
        let (send, mut receipt) = tokio::sync::oneshot::channel();
        let mut batch = super::PendingBatch::default();
        batch.insert(super::CommandEnvelope {
            id: 1,
            command: super::BackendCommand::SetMasterVolume(super::Volume::DEFAULT_MASTER),
            confirmation: Some(send),
        });
        batch.insert(super::CommandEnvelope {
            id: 2,
            command: super::BackendCommand::SetMasterVolume(super::Volume::UNITY),
            confirmation: None,
        });
        assert!(
            matches!(
                receipt.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "a coalesced drag must not settle from an older snapshot before the winner commits"
        );
    }
    use super::*;

    #[test]
    fn pcm_drop_total_stays_monotonic_across_samples_and_delayed_edges() {
        let mut actor =
            ControllerActor::from_persisted(PersistedStateV1::default(), Arc::new(SystemClock));

        actor.apply_capture_telemetry((Some(250), 12));
        actor.apply_capture_update(CaptureUpdate::FrameDrop {
            decoder_generation: 1,
            capture_generation: 1,
            dropped: 8,
        });
        assert_eq!(
            actor.snapshot.audio_source.pcm_frames_dropped_total, 12,
            "a delayed queue edge must not overwrite a newer sampled lifetime total"
        );

        actor.apply_capture_update(CaptureUpdate::FrameDrop {
            decoder_generation: 1,
            capture_generation: 1,
            dropped: 15,
        });
        actor.apply_capture_telemetry((Some(300), 14));
        assert_eq!(
            actor.snapshot.audio_source.pcm_frames_dropped_total, 15,
            "an older periodic sample must not overwrite a newer queue edge"
        );
    }

    #[test]
    fn listening_test_is_cancelled_by_stop_config_change_and_actor_drop() {
        for change in [
            |s: &mut DeviceSnapshot| s.run_intent = RunIntent::Stopped,
            |s: &mut DeviceSnapshot| s.muted = true,
            |s: &mut DeviceSnapshot| s.master_volume = Volume::new(0.12).unwrap(),
            |s: &mut DeviceSnapshot| {
                s.session.phase = SessionPhase::Restarting {
                    generation: 7,
                    reason: RestartReason::CalibrationChanged,
                }
            },
        ] {
            let mut actor =
                ControllerActor::from_persisted(PersistedStateV1::default(), Arc::new(SystemClock));
            actor.snapshot.run_intent = RunIntent::Running;
            actor.snapshot.master_volume = Volume::new(0.1).unwrap();
            let id = ReceiverId::from(airplay_core::DeviceId([1; 6]));
            actor.snapshot.desired_members.insert(id);
            actor.snapshot.session.active.insert(id);
            actor.snapshot.session.phase = SessionPhase::Streaming { generation: 7 };
            actor.snapshot.audio_source.state = AudioSourceState::SilentSystem;
            let guard = super::super::listening_check::ListeningTestGuard::capture(&actor.snapshot)
                .unwrap();
            let token = CancellationToken::new();
            actor.listening_test = Some((guard.clone(), token.clone()));
            actor.publish();
            assert!(
                !token.is_cancelled(),
                "unchanged publication must preserve the tone"
            );
            change(&mut actor.snapshot);
            actor.publish();
            assert!(token.is_cancelled());
            let on_drop = CancellationToken::new();
            actor.listening_test = Some((guard, on_drop.clone()));
            drop(actor);
            assert!(on_drop.is_cancelled());
        }
    }

    #[test]
    fn endpoint_inventory_failure_keeps_unknown_or_last_success_and_recovery_can_be_empty() {
        let mut actor =
            ControllerActor::from_persisted(PersistedStateV1::default(), Arc::new(SystemClock));
        let fail = || {
            Err(crate::backend::capture::CaptureError::Endpoints(
                "unavailable".into(),
            ))
        };
        actor.apply_endpoints(fail());
        assert!(
            actor.snapshot.audio_source.refresh_failed,
            "initial scan failure must be visible"
        );
        assert_eq!(actor.snapshot.audio_source.active_endpoints, None);
        let endpoints = vec![crate::backend::model::AudioEndpoint {
            id: "speakers".into(),
            name: "Speakers".into(),
        }];
        actor.apply_endpoints(Ok(endpoints.clone()));
        assert!(
            !actor.snapshot.audio_source.refresh_failed,
            "success clears the scan failure"
        );
        assert_eq!(
            actor.snapshot.audio_source.active_endpoints,
            Some(endpoints.clone())
        );
        actor.apply_endpoints(fail());
        assert!(
            actor.snapshot.audio_source.refresh_failed,
            "failed refresh must be visible beside the retained inventory"
        );
        assert_eq!(
            actor.snapshot.audio_source.active_endpoints,
            Some(endpoints)
        );
        actor.apply_endpoints(Ok(Vec::new()));
        assert!(
            !actor.snapshot.audio_source.refresh_failed,
            "even an empty successful scan clears failure"
        );
        assert_eq!(
            actor.snapshot.audio_source.active_endpoints,
            Some(Vec::new())
        );
    }

    mod receiver_rows {
        use super::*;

        fn identity(last: u8) -> ReceiverId {
            ReceiverId::from(airplay_core::DeviceId([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, last]))
        }

        /// A desired member restored from persisted state is a row before
        /// discovery has ever seen it, and stays one for as long as the
        /// speaker is switched off -- so this is the ordinary case, not an
        /// exotic one. Filling the display name from the stable identity put
        /// a MAC address in front of the user on every launch after the first
        /// session; the snapshot boundary forbids exactly that.
        #[test]
        fn a_receiver_without_a_known_name_is_never_named_after_its_identity() {
            let id = identity(0x01);
            let persisted = PersistedStateV1 {
                last_desired_members: BTreeSet::from([id]),
                ..PersistedStateV1::default()
            };

            let actor = ControllerActor::from_persisted(persisted, Arc::new(SystemClock));

            let row = actor
                .snapshot
                .receivers
                .iter()
                .find(|receiver| receiver.id == id)
                .expect("a restored desired member is a row before discovery");
            assert_eq!(
                row.name, "",
                "the stable identity was used as a display name"
            );
            assert!(
                !format!("{:?}", actor.snapshot.receivers).contains(&id.to_string()),
                "the identity reached the snapshot in display form"
            );
        }

        /// The gap the row leaves is the *name*, not the row: a receiver the
        /// user selected stays visible while it is unreachable, or its
        /// selection would silently disappear.
        #[test]
        fn a_restored_desired_member_is_still_a_row() {
            let persisted = PersistedStateV1 {
                last_desired_members: BTreeSet::from([identity(0x02)]),
                ..PersistedStateV1::default()
            };

            let actor = ControllerActor::from_persisted(persisted, Arc::new(SystemClock));

            assert_eq!(actor.snapshot.receivers.len(), 1);
            assert_eq!(
                actor.snapshot.receivers[0].lifecycle,
                ReceiverLifecycle::Unavailable
            );
        }
    }

    mod teardown_budget {
        use super::*;
        use crate::backend::session::TEARDOWN_GROUP;

        /// The exit budget contains the session teardown; it cannot equal it.
        ///
        /// After the cancel, the session supervisor still runs its own
        /// bounded `stop`, and only when that returns does its update channel
        /// close and the controller's session drain finish. With both budgets
        /// at four seconds, an exit that met a wedged receiver was guaranteed
        /// to report a timeout no matter how healthy the rest of the backend
        /// was -- and the supervisor was then dropped, whose `Drop` aborts the
        /// join handle mid-TEARDOWN. The bounded disconnect the design
        /// mandates can only ever complete if the budget around it is larger.
        #[test]
        fn the_exit_budget_outlasts_the_session_teardown_it_contains() {
            assert!(
                SHUTDOWN_GROUP_BUDGET > TEARDOWN_GROUP,
                "the exit budget ({SHUTDOWN_GROUP_BUDGET:?}) must leave room for the session \
                 teardown it waits on ({TEARDOWN_GROUP:?}) plus the drains that follow it"
            );
            assert_eq!(
                SHUTDOWN_GROUP_BUDGET,
                Duration::from_secs(5),
                "the shipped exit budget changed; confirm the shell's device budget still covers it"
            );
        }
    }

    mod teardown_signalling {
        use super::*;

        /// A hang detector, not a timing assertion: both cases below either
        /// complete immediately or never, and the value under test is the
        /// returned flag, never an elapsed time.
        const NEVER: Duration = Duration::from_secs(10);

        /// Signalling is the only part of the teardown that has to take a
        /// lock, and it used to take it without a bound.
        ///
        /// Whatever makes a supervisor unreachable -- once it was the relay
        /// task holding the same mutex across its receive window -- the
        /// teardown must not overrun the budget it advertises while still
        /// recording `timed_out == false`, which is what an unbounded
        /// `lock().await` on this path produced.
        #[tokio::test]
        async fn a_supervisor_lock_that_outlasts_the_budget_is_recorded_as_an_overrun() {
            let supervisor = Arc::new(tokio::sync::Mutex::new(0_u32));
            let held = Arc::clone(&supervisor).lock_owned().await;
            // Already spent, so the outcome is the policy and not a race.
            let deadline = Instant::now();

            let timed_out = tokio::time::timeout(
                NEVER,
                signal_within(&supervisor, deadline, "test", |signalled| *signalled += 1),
            )
            .await
            .expect("an unreachable supervisor must not stall the teardown");

            assert!(
                timed_out,
                "a supervisor that could not be reached inside the budget must be reported"
            );
            assert_eq!(
                *held, 0,
                "the signal must not be delivered after the budget is gone"
            );
        }

        /// The ordinary case: the lock is free, the worker is told to stop,
        /// and nothing is reported against the budget.
        #[tokio::test]
        async fn a_reachable_supervisor_is_signalled_and_costs_no_overrun() {
            let supervisor = Arc::new(tokio::sync::Mutex::new(0_u32));
            let deadline = Instant::now() + SHUTDOWN_GROUP_BUDGET;

            let timed_out = tokio::time::timeout(
                NEVER,
                signal_within(&supervisor, deadline, "test", |signalled| *signalled += 1),
            )
            .await
            .expect("a free lock must be taken at once");

            assert!(!timed_out, "a signalled worker is not a budget overrun");
            assert_eq!(
                *supervisor.lock().await,
                1,
                "the worker was never signalled"
            );
        }
    }

    mod supervisor_updates {
        use super::*;
        // The bound the supervisors publish under; the wiring no longer adds
        // a queue of its own, so this appears here and nowhere else.
        use crate::backend::event::SUPERVISOR_CAPACITY;

        /// A hang detector, not a timing assertion. Every test below runs on
        /// a paused clock, so this bound is reached only by a wiring that
        /// truly never delivers -- and then it is reached at once, in virtual
        /// time, instead of stalling the suite.
        const NEVER: Duration = Duration::from_secs(10);

        /// Minimal stand-in for a supervisor: all the wiring ever touches is
        /// its update receiver behind the mutex.
        struct FakeSupervisor {
            updates: Option<mpsc::Receiver<u8>>,
        }

        fn fake(updates: mpsc::Receiver<u8>) -> Arc<tokio::sync::Mutex<FakeSupervisor>> {
            Arc::new(tokio::sync::Mutex::new(FakeSupervisor {
                updates: Some(updates),
            }))
        }

        /// The update stream must not be hostage to the supervisor lock.
        ///
        /// The actor takes that lock for every control request -- endpoint
        /// preference, capture control, discovery control, the suspend and
        /// resume paths. While the stream is read through the same lock, each
        /// of those requests has to wait for whoever is reading, and the
        /// stream has to wait for the request. Detaching the receiver once
        /// removes both waits: a held lock is invisible to the stream.
        #[tokio::test(start_paused = true)]
        async fn a_held_supervisor_lock_does_not_stall_the_update_stream() {
            let (updates_tx, updates) = mpsc::channel(SUPERVISOR_CAPACITY);
            let supervisor = fake(updates);
            let mut stream = detach_supervisor_updates(&supervisor, |supervisor| {
                supervisor
                    .updates
                    .take()
                    .expect("the fixture hands the stream over exactly once")
            })
            .await;

            // A control request in flight: the actor owns the supervisor for
            // as long as the request takes.
            let held = Arc::clone(&supervisor).lock_owned().await;
            // A full supervisor queue, so nothing here depends on how fast
            // either side is scheduled.
            for index in 0..SUPERVISOR_CAPACITY {
                updates_tx
                    .try_send((index % 256) as u8)
                    .expect("the queue was sized for exactly this many items");
            }

            for index in 0..SUPERVISOR_CAPACITY {
                let item = tokio::time::timeout(NEVER, stream.recv())
                    .await
                    .expect("a detached stream must not wait for the supervisor lock")
                    .expect("the supervisor is still running");
                assert_eq!(
                    item,
                    (index % 256) as u8,
                    "updates must arrive in emission order"
                );
            }
            drop(held);
        }

        /// The control lock must be free at any moment, not once per window.
        ///
        /// Measured on the paused clock rather than the wall clock: a wiring
        /// that parks on the update stream while holding the lock can only
        /// give it back when a timer fires, and the auto-advancing test clock
        /// makes exactly that timer visible as elapsed virtual time. A wiring
        /// that never takes the lock costs zero, under any machine load.
        #[tokio::test(start_paused = true)]
        async fn an_idle_update_stream_costs_a_control_request_no_time() {
            let (_updates_tx, updates) = mpsc::channel::<u8>(SUPERVISOR_CAPACITY);
            let supervisor = fake(updates);
            let _stream = detach_supervisor_updates(&supervisor, |supervisor| {
                supervisor
                    .updates
                    .take()
                    .expect("the fixture hands the stream over exactly once")
            })
            .await;
            // Give anything the wiring may have spawned its first poll.
            tokio::task::yield_now().await;

            let started = tokio::time::Instant::now();
            let guard = tokio::time::timeout(NEVER, supervisor.lock())
                .await
                .expect("a control request must reach an idle supervisor");
            let waited = started.elapsed();
            drop(guard);

            assert_eq!(
                waited,
                Duration::ZERO,
                "a control request waited {waited:?} on the update stream; the supervisor lock \
                 must never be held across a wait for an update"
            );
        }

        /// A supervisor that stops cleanly ends the stream, which is how the
        /// actor's select arm and the teardown drain both learn to stop.
        #[tokio::test(start_paused = true)]
        async fn a_cleanly_stopped_supervisor_ends_the_update_stream() {
            let (updates_tx, updates) = mpsc::channel::<u8>(SUPERVISOR_CAPACITY);
            let supervisor = fake(updates);
            let mut stream = detach_supervisor_updates(&supervisor, |supervisor| {
                supervisor
                    .updates
                    .take()
                    .expect("the fixture hands the stream over exactly once")
            })
            .await;

            updates_tx.try_send(7).expect("the queue is empty");
            drop(updates_tx); // the supervisor task ended

            assert_eq!(
                tokio::time::timeout(NEVER, stream.recv())
                    .await
                    .expect("a queued update outlives the sender"),
                Some(7),
                "updates queued before the stop must still be delivered"
            );
            assert_eq!(
                tokio::time::timeout(NEVER, stream.recv())
                    .await
                    .expect("a closed stream ends instead of hanging"),
                None,
                "a cleanly stopped supervisor must end the stream"
            );
        }
    }
}
