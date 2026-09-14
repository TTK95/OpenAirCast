//! Continuous dual-service discovery supervisor (Task 7).
//!
//! Owns one cancellable browse generation at a time, restarts failed daemons
//! with deterministic [`RetryPolicy::discovery_delay`] backoff, settles for
//! exactly two seconds after network changes while marking the inventory
//! unknown, resynchronizes with a fresh generation when its bounded update
//! queue overflows, and publishes generation-tagged [`DiscoveryUpdate`]s.
//!
//! All time flows through injected seams: [`Clock`] for deadlines and
//! [`Timer`] for sleeps, so every decision is reproducible without real
//! waiting.

// The supervisor surface is consumed by the device controller in a later
// plan task; until that wiring lands, non-test builds see it as unused.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use airplay_core::Device;
use airplay_discovery::{BrowseEvent, BrowseStream};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::backend::event::{
    DiagnosticCategory, DiagnosticEvent, DiagnosticKind, DiagnosticPayload, GenerationCell,
    Severity, SUPERVISOR_CAPACITY,
};
use crate::backend::model::{
    DiscoveryPhase, DiscoverySnapshot, ReceiverId, ReceiverLifecycle, ReceiverSnapshot,
    UserFacingError,
};
use crate::backend::recovery::{Clock, RetryPolicy};

/// Interface-settle period observed after a network change before a fresh
/// discovery generation starts; exactly two seconds per the design.
const NETWORK_SETTLE: Duration = Duration::from_secs(2);

/// Injected async sleep seam.
///
/// Production uses [`TokioTimer`]; tests record requested durations and
/// delegate to the real Tokio timer so paused runtimes auto-advance.
pub(crate) trait Timer: Send + Sync + 'static {
    /// Completes after `duration`; never panics.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Production timer backed by [`tokio::time::sleep`].
///
/// Not yet constructed inside this crate: the device controller wires it in
/// a later task of the plan.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TokioTimer;

impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Source of independent browse generations; the injection seam for mDNS.
#[async_trait]
pub(crate) trait DiscoverySource: Send + Sync {
    /// Starts one fallible browse stream; each call is an own daemon.
    async fn browse(&self) -> Result<BrowseStream, UserFacingError>;
}

/// Supervisor control edges sent through a Tokio watch channel so the newest
/// request always wins and pending delays can be cancelled promptly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryControl {
    /// Cancel the active browse, mark all known receivers unknown, settle
    /// exactly two seconds, then start a fresh generation.
    RestartAfterNetworkChange,
    /// Stop the child token and close the update channel.
    Shutdown,
}

/// Generation-tagged supervisor output published on a bounded
/// [`SUPERVISOR_CAPACITY`] queue via non-blocking `try_send`.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // plan-fixed shape: `event: BrowseEvent` by value
pub enum DiscoveryUpdate {
    /// A new browse generation started.
    Started {
        /// Generation that just started.
        generation: u64,
    },
    /// A receiver entered, changed, or left the discovery inventory.
    Receiver {
        /// Owning generation; older events are stale.
        generation: u64,
        /// The underlying browse event.
        event: BrowseEvent,
    },
    /// The daemon failed; a retry is scheduled via policy backoff.
    Failed {
        /// Generation that failed.
        generation: u64,
        /// Pre-redacted failure summary.
        error: UserFacingError,
        /// Monotonic deadline of the scheduled restart.
        retry_at: Instant,
    },
}

/// Construction parameters of [`DiscoverySupervisor`].
pub struct DiscoveryConfig {
    /// Deterministic discovery-restart backoff policy.
    pub policy: RetryPolicy,
    /// Monotonic time source for retry deadlines.
    pub clock: Arc<dyn Clock>,
    /// Sleep seam for settle periods and backoff delays.
    pub(crate) timer: Arc<dyn Timer>,
    /// Optional broadcast feed emitting `DiscoveryGeneration`,
    /// `DiscoveryReceiver`, `Retry`, and `QueueWatermark` payloads.
    pub diagnostics: Option<broadcast::Sender<DiagnosticEvent>>,
    /// Shared view of the counters this supervisor does not own, so its
    /// records name the same session and capture generation the controller's
    /// records do. `None` leaves those two at zero.
    pub generations: Option<Arc<GenerationCell>>,
}

/// One known receiver retained by the internal inventory map.
///
/// Non-castable records stay cached for display; availability flips to
/// unknown on removals and network changes instead of dropping the row.
#[derive(Debug)]
struct KnownReceiver {
    name: String,
    /// Service-record hardware identifier; empty until a sighting carries one.
    model: String,
    castable: bool,
    available: bool,
}

/// Internal supervisor state shared with the handle side.
///
/// Feeds `DiscoverySnapshot` values and the receiver rows rendered by the
/// shell; updates from older generations cannot alter newer state.
#[derive(Debug, Default)]
pub(crate) struct DiscoveryState {
    generation: u64,
    phase: DiscoveryPhase,
    receivers: BTreeMap<ReceiverId, KnownReceiver>,
    child: Option<CancellationToken>,
}

impl DiscoveryState {
    /// Creates state anchored at the given generation.
    pub(crate) fn at_generation(generation: u64) -> Self {
        Self {
            generation,
            ..Self::default()
        }
    }

    /// Applies a supervisor update as the canonical reducer.
    ///
    /// Returns false when the update belongs to an older generation and was
    /// discarded; stale updates never change the inventory or phase.
    pub(crate) fn apply_update(&mut self, update: &DiscoveryUpdate) -> bool {
        match update {
            DiscoveryUpdate::Started { generation } => {
                if *generation < self.generation {
                    return false;
                }
                self.generation = *generation;
                self.phase = DiscoveryPhase::Running;
                true
            }
            DiscoveryUpdate::Receiver { generation, event } => {
                if *generation < self.generation {
                    return false;
                }
                self.apply_event(*generation, event)
            }
            DiscoveryUpdate::Failed { generation, .. } => *generation >= self.generation,
        }
    }

    /// Folds one browse event into the inventory; returns false when stale.
    fn apply_event(&mut self, generation: u64, event: &BrowseEvent) -> bool {
        if generation < self.generation {
            return false;
        }
        match event {
            BrowseEvent::Added(device) | BrowseEvent::Updated(device) => {
                let id = ReceiverId::from(device.id.clone());
                let entry = self.receivers.entry(id).or_insert_with(|| KnownReceiver {
                    name: String::new(),
                    model: String::new(),
                    castable: false,
                    available: false,
                });
                entry.name = device.name.clone();
                entry.model = device.model.clone();
                entry.castable = is_castable(device);
                entry.available = true;
            }
            BrowseEvent::Removed(id) => {
                let key = ReceiverId::from(id.clone());
                if let Some(entry) = self.receivers.get_mut(&key) {
                    entry.available = false;
                }
            }
        }
        true
    }

    /// Network-change semantics: keep every record but mark it unknown.
    pub(crate) fn mark_all_unknown(&mut self) {
        for entry in self.receivers.values_mut() {
            entry.available = false;
        }
    }

    /// Daemon-recreation semantics: cache state is scoped to one generation.
    pub(crate) fn clear_inventory(&mut self) {
        self.receivers.clear();
    }

    /// Records a scheduled discovery restart for snapshot rendering.
    pub(crate) fn note_retrying(&mut self, attempt: u32, retry_at: SystemTime) {
        self.phase = DiscoveryPhase::Retrying { attempt, retry_at };
    }

    /// Records that supervision stopped and cancels the live child token.
    pub(crate) fn note_stopped(&mut self) {
        self.phase = DiscoveryPhase::Stopped;
        if let Some(child) = self.child.take() {
            child.cancel();
        }
    }

    /// Receiver rows sorted by stable identity for rendering.
    pub(crate) fn receivers(&self) -> Vec<ReceiverSnapshot> {
        self.receivers
            .iter()
            .map(|(id, known)| ReceiverSnapshot {
                id: *id,
                name: known.name.clone(),
                model: known.model.clone(),
                lifecycle: if known.available && known.castable {
                    ReceiverLifecycle::Discovered
                } else {
                    ReceiverLifecycle::Unavailable
                },
            })
            .collect()
    }

    /// Current `DiscoverySnapshot` view (generation plus supervisor phase).
    pub(crate) fn snapshot(&self) -> DiscoverySnapshot {
        let mut snapshot = match self.phase {
            DiscoveryPhase::Running => DiscoverySnapshot::running_empty(),
            DiscoveryPhase::Retrying { attempt, retry_at } => {
                DiscoverySnapshot::failed(attempt, retry_at)
            }
            DiscoveryPhase::Stopped => DiscoverySnapshot::default(),
        };
        snapshot.generation = self.generation;
        snapshot
    }

    /// Clone of the live generation's cancellation token, if any.
    pub(crate) fn child_token(&self) -> Option<CancellationToken> {
        self.child.clone()
    }
}

impl DiscoverySnapshot {
    /// Snapshot of a running browse generation with an empty inventory.
    pub(crate) fn running_empty() -> Self {
        Self {
            generation: 0,
            phase: DiscoveryPhase::Running,
        }
    }

    /// Snapshot of a failed daemon waiting to restart after `retry_at`.
    pub(crate) fn failed(attempt: u32, retry_at: SystemTime) -> Self {
        Self {
            generation: 0,
            phase: DiscoveryPhase::Retrying { attempt, retry_at },
        }
    }
}

/// A receiver is castable only with AirPlay 2 support and a usable IPv4
/// address; non-castable records remain visible for presentation.
fn is_castable(device: &Device) -> bool {
    device.supports_airplay2() && device.socket_addr().is_some_and(|addr| addr.is_ipv4())
}

/// Outcome of a non-blocking update send.
enum Flow {
    Sent,
    Overloaded,
    Closed,
}

/// Exit reason of one browse generation.
enum GenerationExit {
    /// Control requested shutdown.
    Shutdown,
    /// Control requested a network-change restart.
    NetworkChanged,
    /// The update queue overflowed; resynchronize from a fresh generation.
    Overloaded,
    /// A daemon failure's backoff delay elapsed uneventfully.
    BackoffElapsed,
}

/// Result of the interruptible two-second network-settle wait.
enum Settle {
    Elapsed,
    Shutdown,
    RestartAgain,
}

/// Long-running supervisor task handle plus its control/state surfaces.
pub struct DiscoverySupervisor {
    control_tx: watch::Sender<Option<DiscoveryControl>>,
    /// `None` once [`DiscoverySupervisor::take_updates`] handed the receive
    /// half to an owner outside this handle. The channel itself is created
    /// once in [`DiscoverySupervisor::start`] and is never replaced -- a
    /// browse restart is an internal child restart, not a new queue -- so
    /// handing the receiver out once loses nothing.
    updates: Option<mpsc::Receiver<DiscoveryUpdate>>,
    shared: Arc<Mutex<DiscoveryState>>,
    join: Option<JoinHandle<()>>,
}

impl DiscoverySupervisor {
    /// Spawns the supervisor loop over the given source and configuration.
    pub(crate) fn start(source: Arc<dyn DiscoverySource>, config: DiscoveryConfig) -> Self {
        let (control_tx, control_rx) = watch::channel(None);
        let (updates_tx, updates) = mpsc::channel(SUPERVISOR_CAPACITY);
        let shared = Arc::new(Mutex::new(DiscoveryState::default()));
        let join = tokio::spawn(run_supervisor(
            source,
            config,
            control_rx,
            updates_tx,
            Arc::clone(&shared),
        ));
        Self {
            control_tx,
            updates: Some(updates),
            shared,
            join: Some(join),
        }
    }

    /// Publishes a control edge; the newest value wins (watch semantics).
    pub fn request_control(&self, control: DiscoveryControl) {
        let _ = self.control_tx.send(Some(control));
    }

    /// Receive half of the bounded supervisor update queue.
    ///
    /// # Panics
    ///
    /// Panics once [`Self::take_updates`] has moved the receiver out. Owning
    /// the stream and borrowing it through the handle are mutually exclusive
    /// by construction; a caller doing both has two consumers of one queue.
    pub fn updates_mut(&mut self) -> &mut mpsc::Receiver<DiscoveryUpdate> {
        self.updates
            .as_mut()
            .expect("the update receiver was taken out of this supervisor")
    }

    /// Moves the receive half out of the handle, once.
    ///
    /// Callers that own the stream outright never need the supervisor lock to
    /// read it, so nothing has to wait on an update while holding a lock that
    /// control requests need. Returns `None` on every call after the first.
    pub fn take_updates(&mut self) -> Option<mpsc::Receiver<DiscoveryUpdate>> {
        self.updates.take()
    }

    /// Locks the internal discovery state for inspection or snapshots.
    pub(crate) fn lock_state(&self) -> MutexGuard<'_, DiscoveryState> {
        lock(&self.shared)
    }

    /// Awaits clean task completion after a shutdown control edge.
    pub async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        self.join
            .take()
            .expect("join handle is only consumed once")
            .await
    }
}

impl Drop for DiscoverySupervisor {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

fn lock(shared: &Mutex<DiscoveryState>) -> MutexGuard<'_, DiscoveryState> {
    shared.lock().expect("discovery state mutex poisoned")
}

async fn run_supervisor(
    source: Arc<dyn DiscoverySource>,
    config: DiscoveryConfig,
    mut control_rx: watch::Receiver<Option<DiscoveryControl>>,
    updates_tx: mpsc::Sender<DiscoveryUpdate>,
    shared: Arc<Mutex<DiscoveryState>>,
) {
    control_rx.borrow_and_update();
    let mut generation: u64 = 0;
    let mut failures: u32 = 0;
    // After a network change the marked-unknown inventory must survive the
    // next generation start instead of being cleared like a daemon recreation.
    let mut preserve_unknown = false;

    'outer: loop {
        generation = generation.saturating_add(1);
        // Published before anything can be emitted under it: this supervisor
        // owns the browse counter, and a record written by the capture
        // supervisor in the same instant has to be able to name the browse
        // that is running rather than wait for the actor loop to relay it.
        if let Some(cell) = config.generations.as_ref() {
            cell.publish_discovery(generation);
        }
        let child = CancellationToken::new();
        {
            let mut state = lock(&shared);
            state.apply_update(&DiscoveryUpdate::Started { generation });
            if preserve_unknown {
                state.mark_all_unknown();
            } else {
                state.clear_inventory();
            }
            state.child = Some(child.clone());
        }
        preserve_unknown = false;

        match push_update(
            &config,
            &updates_tx,
            generation,
            &child,
            DiscoveryUpdate::Started { generation },
        ) {
            Flow::Sent => {}
            Flow::Overloaded => {
                tokio::task::yield_now().await;
                continue;
            }
            Flow::Closed => break 'outer,
        }
        emit(
            &config,
            generation,
            None,
            Severity::Info,
            DiagnosticPayload::DiscoveryGeneration {
                generation,
                phase: DiscoveryPhase::Running,
            },
        );

        match drive_generation(
            source.as_ref(),
            &config,
            &mut control_rx,
            &updates_tx,
            &shared,
            generation,
            child.clone(),
            &mut failures,
        )
        .await
        {
            GenerationExit::Shutdown => break 'outer,
            GenerationExit::NetworkChanged => {
                // Order per design: cancel, mark all known unknown, settle
                // exactly two seconds, then start a fresh generation.
                child.cancel();
                lock(&shared).mark_all_unknown();
                failures = 0;
                preserve_unknown = true;
                match settle(&config, &mut control_rx).await {
                    Settle::Elapsed | Settle::RestartAgain => {}
                    Settle::Shutdown => break 'outer,
                }
            }
            GenerationExit::Overloaded => {
                tokio::task::yield_now().await;
            }
            GenerationExit::BackoffElapsed => {}
        }
    }

    emit(
        &config,
        generation,
        None,
        Severity::Info,
        DiagnosticPayload::DiscoveryGeneration {
            generation,
            phase: DiscoveryPhase::Stopped,
        },
    );
    lock(&shared).note_stopped();
}

/// Runs one browse generation until control, failure, or overload ends it.
#[allow(clippy::too_many_arguments)]
async fn drive_generation(
    source: &dyn DiscoverySource,
    config: &DiscoveryConfig,
    control_rx: &mut watch::Receiver<Option<DiscoveryControl>>,
    updates_tx: &mpsc::Sender<DiscoveryUpdate>,
    shared: &Arc<Mutex<DiscoveryState>>,
    generation: u64,
    child: CancellationToken,
    failures: &mut u32,
) -> GenerationExit {
    let mut stream = match source.browse().await {
        Ok(stream) => stream,
        Err(error) => {
            return schedule_failure(
                config, control_rx, updates_tx, shared, generation, child, failures, error,
            )
            .await
        }
    };

    loop {
        tokio::select! {
            control = next_control(control_rx) => {
                break on_control(control);
            }
            item = stream.next() => match item {
                Some(Ok(event)) => {
                    let accepted = lock(shared).apply_update(&DiscoveryUpdate::Receiver {
                        generation,
                        event: event.clone(),
                    });
                    if !accepted {
                        continue;
                    }
                    let present = !matches!(event, BrowseEvent::Removed(_));
                    emit(
                        config,
                        generation,
                        event_receiver(&event),
                        Severity::Info,
                        DiagnosticPayload::DiscoveryReceiver {
                            receiver: event_receiver(&event)
                                .expect("accepted receiver events carry an identity"),
                            present,
                        },
                    );
                    match push_update(
                        config,
                        updates_tx,
                        generation,
                        &child,
                        DiscoveryUpdate::Receiver { generation, event },
                    ) {
                        Flow::Sent => {}
                        Flow::Overloaded => break GenerationExit::Overloaded,
                        Flow::Closed => break GenerationExit::Shutdown,
                    }
                }
                Some(Err(error)) => {
                    break schedule_failure(
                        config,
                        control_rx,
                        updates_tx,
                        shared,
                        generation,
                        child,
                        failures,
                        redact_discovery_error(&error),
                    )
                    .await;
                }
                None => {
                    break schedule_failure(
                        config,
                        control_rx,
                        updates_tx,
                        shared,
                        generation,
                        child,
                        failures,
                        UserFacingError::new("discovery stream ended unexpectedly"),
                    )
                    .await;
                }
            }
        }
    }
}

/// Emits `Failed` plus a retry diagnostic, then sleeps policy backoff while
/// remaining interruptible by control edges.
#[allow(clippy::too_many_arguments)]
async fn schedule_failure(
    config: &DiscoveryConfig,
    control_rx: &mut watch::Receiver<Option<DiscoveryControl>>,
    updates_tx: &mpsc::Sender<DiscoveryUpdate>,
    shared: &Arc<Mutex<DiscoveryState>>,
    generation: u64,
    child: CancellationToken,
    failures: &mut u32,
    error: UserFacingError,
) -> GenerationExit {
    let delay = config.policy.discovery_delay(generation, *failures);
    let retry_at = config.clock.now() + delay;
    let wall_deadline = SystemTime::now() + delay;
    lock(shared).note_retrying(*failures, wall_deadline);
    match push_update(
        config,
        updates_tx,
        generation,
        &child,
        DiscoveryUpdate::Failed {
            generation,
            error,
            retry_at,
        },
    ) {
        Flow::Sent => {}
        Flow::Overloaded => return GenerationExit::Overloaded,
        Flow::Closed => return GenerationExit::Shutdown,
    }
    emit(
        config,
        generation,
        None,
        Severity::Warning,
        DiagnosticPayload::Retry {
            receiver: None,
            attempt: *failures,
            next_at: wall_deadline,
        },
    );
    *failures += 1;

    tokio::select! {
        _ = config.timer.sleep(delay) => GenerationExit::BackoffElapsed,
        control = next_control(control_rx) => on_control(control),
    }
}

/// Waits exactly two seconds for interfaces to settle, staying interruptible.
async fn settle(
    config: &DiscoveryConfig,
    control_rx: &mut watch::Receiver<Option<DiscoveryControl>>,
) -> Settle {
    tokio::select! {
        _ = config.timer.sleep(NETWORK_SETTLE) => Settle::Elapsed,
        control = next_control(control_rx) => match control {
            Some(DiscoveryControl::RestartAfterNetworkChange) => Settle::RestartAgain,
            _ => Settle::Shutdown,
        },
    }
}

/// Resolves the latest control edge; `None` means the channel closed.
async fn next_control(
    control_rx: &mut watch::Receiver<Option<DiscoveryControl>>,
) -> Option<DiscoveryControl> {
    let _ = control_rx.changed().await;
    *control_rx.borrow_and_update()
}

fn on_control(control: Option<DiscoveryControl>) -> GenerationExit {
    match control {
        Some(DiscoveryControl::RestartAfterNetworkChange) => GenerationExit::NetworkChanged,
        Some(DiscoveryControl::Shutdown) | None => GenerationExit::Shutdown,
    }
}

fn push_update(
    config: &DiscoveryConfig,
    updates_tx: &mpsc::Sender<DiscoveryUpdate>,
    generation: u64,
    child: &CancellationToken,
    update: DiscoveryUpdate,
) -> Flow {
    match updates_tx.try_send(update) {
        Ok(()) => Flow::Sent,
        Err(mpsc::error::TrySendError::Full(_)) => {
            emit(
                config,
                generation,
                None,
                Severity::Warning,
                DiagnosticPayload::QueueWatermark {
                    depth: SUPERVISOR_CAPACITY as u32,
                    capacity: SUPERVISOR_CAPACITY as u32,
                },
            );
            child.cancel();
            Flow::Overloaded
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Flow::Closed,
    }
}

fn event_receiver(event: &BrowseEvent) -> Option<ReceiverId> {
    match event {
        BrowseEvent::Added(device) | BrowseEvent::Updated(device) => {
            Some(ReceiverId::from(device.id.clone()))
        }
        // A removal names the service that expired, so it carries an identity
        // exactly like the other two; returning `None` here made the caller's
        // `expect` fire and killed the discovery worker on the first mDNS TTL
        // expiry.
        BrowseEvent::Removed(id) => Some(ReceiverId::from(id.clone())),
    }
}

fn redact_discovery_error(error: &airplay_core::error::DiscoveryError) -> UserFacingError {
    UserFacingError::new(format!("mDNS discovery failed: {error}"))
}

fn emit(
    config: &DiscoveryConfig,
    generation: u64,
    receiver: Option<ReceiverId>,
    severity: Severity,
    payload: DiagnosticPayload,
) {
    let Some(sender) = config.diagnostics.as_ref() else {
        return;
    };
    let category = diagnostic_category(payload.kind());
    // This supervisor owns the discovery counter and reads the other two.
    let mut correlation = config
        .generations
        .as_ref()
        .map(|cell| cell.get())
        .unwrap_or_default();
    correlation.discovery = generation;
    let _ = sender.send(DiagnosticEvent {
        monotonic_ns: monotonic_ns_now(),
        wall_time: SystemTime::now(),
        session_generation: correlation.session,
        discovery_generation: correlation.discovery,
        capture_generation: correlation.capture,
        receiver,
        severity,
        category,
        payload,
    });
}

fn diagnostic_category(kind: DiagnosticKind) -> DiagnosticCategory {
    match kind {
        DiagnosticKind::DiscoveryGeneration | DiagnosticKind::DiscoveryReceiver => {
            DiagnosticCategory::Discovery
        }
        DiagnosticKind::Retry => DiagnosticCategory::Retry,
        DiagnosticKind::QueueWatermark => DiagnosticCategory::Queue,
        _ => DiagnosticCategory::General,
    }
}

fn monotonic_ns_now() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

// The shared fixtures file is intentionally included once per test module
// (model already includes it), so this duplicate registration is deliberate.
#[allow(clippy::duplicate_mod)]
#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use tokio::sync::broadcast;

    use super::support::{added, removed, BrowseItem, FakeDiscoverySource, FixedClock};
    use super::*;

    /// Timer seam recorder: captures every requested delay and completes
    /// instantly so tests never wait real time; the recorded durations are
    /// the observable timing contract.
    #[derive(Default)]
    struct RecordingTimer {
        recorded: Mutex<Vec<Duration>>,
        calls: AtomicUsize,
    }

    impl RecordingTimer {
        fn recorded(&self) -> Vec<Duration> {
            self.recorded.lock().expect("timer mutex poisoned").clone()
        }
    }

    impl Timer for RecordingTimer {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            self.recorded
                .lock()
                .expect("timer mutex poisoned")
                .push(duration);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }

    struct DiscoveryHarness {
        supervisor: DiscoverySupervisor,
        source: Arc<FakeDiscoverySource>,
        timer: Arc<RecordingTimer>,
        diagnostics_rx: broadcast::Receiver<DiagnosticEvent>,
        clock: Arc<FixedClock>,
    }

    impl DiscoveryHarness {
        fn start(source: FakeDiscoverySource) -> Self {
            Self::start_with_diagnostics(source, false)
        }

        fn start_with_diagnostics(source: FakeDiscoverySource, with_diagnostics: bool) -> Self {
            Self::start_with(source, with_diagnostics, None)
        }

        fn start_with(
            source: FakeDiscoverySource,
            with_diagnostics: bool,
            generations: Option<Arc<GenerationCell>>,
        ) -> Self {
            let source = Arc::new(source);
            let timer = Arc::new(RecordingTimer::default());
            let clock = Arc::new(FixedClock::new());
            let (diagnostics_tx, diagnostics_rx) = broadcast::channel(2_048);
            let config = DiscoveryConfig {
                generations,
                policy: RetryPolicy::default(),
                clock: clock.clone(),
                timer: timer.clone() as Arc<dyn Timer>,
                diagnostics: with_diagnostics.then_some(diagnostics_tx),
            };
            let supervisor = DiscoverySupervisor::start(source.clone(), config);
            Self {
                supervisor,
                source,
                timer,
                diagnostics_rx,
                clock,
            }
        }

        async fn recv_update(&mut self) -> DiscoveryUpdate {
            self.supervisor
                .updates_mut()
                .recv()
                .await
                .expect("supervisor update channel stays open until shutdown")
        }

        /// Requests a clean shutdown and awaits task completion.
        async fn shutdown_and_join(self) {
            self.supervisor.request_control(DiscoveryControl::Shutdown);
            self.supervisor.join().await.expect("task ends cleanly");
        }

        fn drain_diagnostics(&mut self) -> Vec<DiagnosticPayload> {
            let mut payloads = Vec::new();
            loop {
                match self.diagnostics_rx.try_recv() {
                    Ok(event) => payloads.push(event.payload),
                    Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                        panic!("diagnostic receiver lagged by {dropped}");
                    }
                    Err(broadcast::error::TryRecvError::Empty)
                    | Err(broadcast::error::TryRecvError::Closed) => break,
                }
            }
            payloads
        }
    }

    mod generation_lifecycle {
        use super::*;

        /// The browse counter is this supervisor's own, and the shared cell
        /// has to carry it from the instant the generation exists.
        ///
        /// The assertion is taken at the one moment that proves it: the
        /// `Started` update has just been pulled off the queue, so no actor
        /// loop can have processed it yet. A cell that only the controller
        /// writes still reads zero here -- and every record another
        /// supervisor emits in this window would claim that no browse was
        /// running, which is what zero is reserved to mean.
        #[tokio::test]
        async fn a_browse_generation_reaches_the_shared_cell_before_any_consumer() {
            let source = FakeDiscoverySource::new();
            source.push_stream(Vec::new());
            let cell = GenerationCell::new();
            let mut harness = DiscoveryHarness::start_with(source, false, Some(Arc::clone(&cell)));

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            assert_eq!(
                cell.get().discovery,
                1,
                "the browse was open but the shared cell still said none was"
            );

            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn generation_increments_per_daemon_recreation() {
            let source = FakeDiscoverySource::new();
            // Generations 1 and 2 fail immediately; generation 3 stays idle-live.
            source.push_browse_error("daemon gone");
            source.push_browse_error("daemon gone");
            source.push_stream(Vec::new());
            let mut harness = DiscoveryHarness::start(source);

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            assert!(
                matches!(
                    harness.recv_update().await,
                    DiscoveryUpdate::Failed { generation: 1, .. }
                ),
                "browse failure reports Failed before retrying"
            );
            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 2 }
            ));
            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Failed { generation: 2, .. }
            ));
            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 3 }
            ));

            assert_eq!(harness.source.calls(), 3, "one browse per generation");
            assert_eq!(harness.supervisor.lock_state().snapshot().generation, 3);
            // The idle third generation would exhaust the script on its next
            // recreation; stop cleanly instead of dropping mid-flight.
            harness.shutdown_and_join().await;
        }

        #[test]
        fn old_generation_update_cannot_change_inventory() {
            let mut state = DiscoveryState::at_generation(2);
            assert!(!state.apply_update(&DiscoveryUpdate::Receiver {
                generation: 1,
                event: added(1),
            }));
            assert!(state.receivers().is_empty());
        }
    }

    mod network_restart {
        use super::*;
        use crate::backend::model::ReceiverLifecycle;

        #[tokio::test]
        async fn network_change_cancels_marks_unknown_and_restarts_after_two_seconds() {
            let source = FakeDiscoverySource::new();
            source.push_stream(vec![Ok(added(1))]);
            source.push_stream(Vec::new());
            let mut harness = DiscoveryHarness::start(source);

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Receiver { generation: 1, .. }
            ));
            let child = harness
                .supervisor
                .lock_state()
                .child_token()
                .expect("generation child token stored");
            assert!(!child.is_cancelled());

            harness
                .supervisor
                .request_control(DiscoveryControl::RestartAfterNetworkChange);

            assert!(
                matches!(
                    harness.recv_update().await,
                    DiscoveryUpdate::Started { generation: 2 }
                ),
                "fresh generation starts after the settle period"
            );

            assert!(child.is_cancelled(), "old generation token is cancelled");
            assert_eq!(
                harness.timer.recorded(),
                vec![Duration::from_secs(2)],
                "settle waits exactly two seconds via the injected timer"
            );
            {
                let state = harness.supervisor.lock_state();
                let receivers = state.receivers();
                assert_eq!(receivers.len(), 1, "record retained for display");
                assert_eq!(receivers[0].lifecycle, ReceiverLifecycle::Unavailable);
                assert_eq!(state.snapshot().generation, 2);
            }
            assert_eq!(harness.source.calls(), 2);
            harness.shutdown_and_join().await;
        }
    }

    mod queue_overload {
        use super::*;

        #[tokio::test]
        async fn full_update_queue_triggers_watermark_and_generation_resync() {
            let source = FakeDiscoverySource::new();
            // Far more events than SUPERVISOR_CAPACITY; a second idle stream
            // serves the fresh resynchronization generation.
            let preloaded: Vec<BrowseItem> = (0..400).map(|seed| Ok(added(seed as u8))).collect();
            source.push_stream(preloaded);
            source.push_stream(Vec::new());
            let mut harness = DiscoveryHarness::start_with_diagnostics(source, true);

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            let child = harness
                .supervisor
                .lock_state()
                .child_token()
                .expect("generation child token stored");

            // Phase one: do not drain yet so the bounded queue provably
            // overflows; the first watermark marks that moment.
            loop {
                match harness.diagnostics_rx.try_recv() {
                    Ok(event)
                        if matches!(event.payload, DiagnosticPayload::QueueWatermark { .. }) =>
                    {
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                        panic!("diagnostic receiver lagged by {dropped}");
                    }
                    _ => tokio::task::yield_now().await,
                }
            }
            assert!(
                child.is_cancelled(),
                "overflow cancels the overloaded generation"
            );

            // Phase two: drain; the overloaded supervisor keeps yielding until
            // space frees up, then emits a fresh Started generation.
            let mut drained = Vec::new();
            let resync_generation = loop {
                match harness.supervisor.updates_mut().try_recv() {
                    Ok(DiscoveryUpdate::Started { generation }) if generation >= 2 => {
                        break generation;
                    }
                    Ok(update) => drained.push(update),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        tokio::task::yield_now().await;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        panic!("update channel closed during overload resync");
                    }
                }
            };

            assert!(resync_generation >= 2, "resync starts a fresh generation");
            assert!(
                drained.len() <= SUPERVISOR_CAPACITY,
                "queue never grows beyond capacity"
            );
            assert!(
                drained.iter().all(|update| matches!(
                    update,
                    DiscoveryUpdate::Receiver { generation: 1, .. }
                )),
                "only current-generation updates are delivered"
            );

            let payloads = harness.drain_diagnostics();
            assert!(
                payloads
                    .iter()
                    .any(|payload| matches!(payload, DiagnosticPayload::QueueWatermark { .. })),
                "a QueueWatermark diagnostic is emitted on overflow"
            );

            harness.shutdown_and_join().await;
        }
    }

    mod failure_retry {
        use super::*;

        #[tokio::test]
        async fn browse_failure_schedules_retry_via_policy() {
            let expected_delay = RetryPolicy::default().discovery_delay(1, 0);

            let source = FakeDiscoverySource::new();
            source.push_browse_error("mdns down");
            source.push_stream(Vec::new());

            let mut harness = DiscoveryHarness::start(source);
            // The fake clock is frozen, so this equals the deadline computed
            // inside the supervisor at any later point.
            let retry_deadline = harness.clock.now_instant() + expected_delay;

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            match harness.recv_update().await {
                DiscoveryUpdate::Failed {
                    generation,
                    error,
                    retry_at,
                } => {
                    assert_eq!(generation, 1);
                    assert!(
                        error.as_str().contains("mdns down"),
                        "failure carries a redacted reason"
                    );
                    assert_eq!(
                        retry_at, retry_deadline,
                        "retry_at comes from RetryPolicy::discovery_delay via the injected clock"
                    );
                }
                other => panic!("expected Failed, got {other:?}"),
            }

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 2 }
            ));
            assert_eq!(harness.source.calls(), 2);
            harness.shutdown_and_join().await;
        }
    }

    mod shutdown {
        use super::*;
        use crate::backend::model::DiscoveryPhase;

        #[tokio::test]
        async fn shutdown_stops_child_and_channel() {
            let source = FakeDiscoverySource::new();
            source.push_stream(Vec::new());
            let mut harness = DiscoveryHarness::start(source);

            assert!(matches!(
                harness.recv_update().await,
                DiscoveryUpdate::Started { generation: 1 }
            ));
            let child = harness
                .supervisor
                .lock_state()
                .child_token()
                .expect("generation child token stored");
            assert_eq!(harness.source.calls(), 1);

            harness
                .supervisor
                .request_control(DiscoveryControl::Shutdown);

            assert!(
                harness.supervisor.updates_mut().recv().await.is_none(),
                "shutdown closes the update channel"
            );
            assert!(child.is_cancelled(), "shutdown stops the child token");
            assert!(matches!(
                harness.supervisor.lock_state().snapshot().phase,
                DiscoveryPhase::Stopped
            ));
            harness.shutdown_and_join().await;
        }
    }

    mod snapshot_constructors {
        use super::*;
        use crate::backend::model::{DiscoveryPhase, DiscoverySnapshot};

        #[test]
        fn empty_running_discovery_differs_from_failed_discovery() {
            assert!(matches!(
                DiscoverySnapshot::running_empty().phase,
                DiscoveryPhase::Running
            ));
            assert!(matches!(
                DiscoverySnapshot::failed(2, std::time::SystemTime::UNIX_EPOCH).phase,
                DiscoveryPhase::Retrying { attempt: 2, .. }
            ));
        }

        #[test]
        fn removed_event_marks_known_receiver_unavailable_but_retained() {
            let mut state = DiscoveryState::at_generation(1);
            assert!(state.apply_update(&DiscoveryUpdate::Receiver {
                generation: 1,
                event: added(4),
            }));
            assert!(state.apply_update(&DiscoveryUpdate::Receiver {
                generation: 1,
                event: removed(4),
            }));
            let receivers = state.receivers();
            assert_eq!(receivers.len(), 1);
            assert!(matches!(
                receivers[0].lifecycle,
                ReceiverLifecycle::Unavailable
            ));
        }
    }
}
