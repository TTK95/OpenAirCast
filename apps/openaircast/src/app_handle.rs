//! Non-blocking control-center actor.
//!
//! The actor thread owns the authoritative [`AppState`], reduces incoming
//! [`AppEvent`]s, executes resulting [`AppEffect`]s through a non-blocking
//! executor port, and publishes complete immutable [`UiSnapshot`] revisions to
//! an `ArcSwap` slot. UI threads observe either the old or the new revision,
//! never a partial update.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use arc_swap::ArcSwap;

use crate::app::{reduce, AppEffect, AppEvent, AppState, UiEffect, UiSnapshot};

/// Capacity of the bounded application event queue.
#[allow(dead_code)] // start_app convenience wrapper keeps it for tests/subproject 2.
const EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AppUnavailable {
    #[error("application event queue is busy")]
    Busy,
    #[error("application actor is closed")]
    Closed,
}

pub(crate) trait EffectExecutor: Send + 'static {
    fn execute(&mut self, effect: AppEffect, feedback: &AppFeedback);
}

#[derive(Clone)]
pub(crate) struct AppFeedback {
    pub events: AppEventSender,
    pub ui_effects: UiEffectSender,
}

#[derive(Clone)]
pub(crate) struct AppEventSender {
    tx: SyncSender<AppEvent>,
}

impl AppEventSender {
    pub(crate) fn new(tx: SyncSender<AppEvent>) -> Self {
        Self { tx }
    }

    /// Non-blocking bounded send used by worker threads to report results.
    pub(crate) fn try_send(&self, event: AppEvent) -> Result<(), AppUnavailable> {
        self.tx.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => AppUnavailable::Busy,
            TrySendError::Disconnected(_) => AppUnavailable::Closed,
        })
    }
}

#[derive(Clone)]
pub(crate) struct UiEffectSender {
    queue: Arc<Mutex<VecDeque<UiEffect>>>,
    wake_gate: Arc<AtomicBool>,
    wakers: Arc<WakerRegistry>,
}

impl UiEffectSender {
    pub(crate) fn new(
        queue: Arc<Mutex<VecDeque<UiEffect>>>,
        wake_gate: Arc<AtomicBool>,
        wakers: Arc<WakerRegistry>,
    ) -> Self {
        Self {
            queue,
            wake_gate,
            wakers,
        }
    }

    /// Queues one window/UI effect without blocking and schedules a frame if
    /// none is pending yet.
    pub(crate) fn send(&self, effect: UiEffect) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(effect);
        }
        schedule_frame(&self.wake_gate, &self.wakers);
    }
}

type WakerCallbacks = Vec<(u64, Arc<dyn Fn() + Send + Sync>)>;

pub(crate) struct WakerRegistry {
    callbacks: Mutex<WakerCallbacks>,
    next_id: AtomicU64,
}

impl Default for WakerRegistry {
    fn default() -> Self {
        Self {
            callbacks: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl WakerRegistry {
    fn register(self: &Arc<Self>, waker: Arc<dyn Fn() + Send + Sync>) -> SnapshotSubscription {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut callbacks) = self.callbacks.lock() {
            callbacks.push((id, waker));
        }
        SnapshotSubscription {
            registry: Arc::clone(self),
            id,
        }
    }

    fn invoke_all(&self) {
        let callbacks: Vec<Arc<dyn Fn() + Send + Sync>> = match self.callbacks.lock() {
            Ok(guard) => guard
                .iter()
                .map(|(_, callback)| Arc::clone(callback))
                .collect(),
            Err(_) => return,
        };
        for callback in callbacks {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(move || callback()));
            if outcome.is_err() {
                tracing::warn!("a snapshot waker panicked; the actor continues");
            }
        }
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut callbacks) = self.callbacks.lock() {
            callbacks.retain(|(registered, _)| *registered != id);
        }
    }
}

/// Keeps one waker registered until dropped; dropping removes only its own
/// callback and leaves other subscriptions untouched.
pub struct SnapshotSubscription {
    registry: Arc<WakerRegistry>,
    id: u64,
}

impl Drop for SnapshotSubscription {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}

#[derive(Clone)]
pub struct AppHandle {
    event_tx: SyncSender<AppEvent>,
    snapshot: Arc<ArcSwap<UiSnapshot>>,
    ui_effects: Arc<Mutex<VecDeque<UiEffect>>>,
    wakers: Arc<WakerRegistry>,
    wake_gate: Arc<AtomicBool>,
}

impl AppHandle {
    /// Non-blocking bounded dispatch; fails with [`AppUnavailable::Busy`]
    /// instead of blocking when the 256-entry queue is full.
    pub fn dispatch(&self, event: AppEvent) -> Result<(), AppUnavailable> {
        self.event_tx.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => AppUnavailable::Busy,
            TrySendError::Disconnected(_) => AppUnavailable::Closed,
        })
    }

    /// Loads one complete snapshot revision; readers never see partial state.
    pub fn snapshot(&self) -> Arc<UiSnapshot> {
        self.snapshot.load_full()
    }

    /// Returns every queued UI effect exactly once in FIFO order and re-arms
    /// the wake gate so a later publication schedules a fresh frame.
    pub fn drain_ui_effects(&self) -> Vec<UiEffect> {
        let mut queue = match self.ui_effects.lock() {
            Ok(queue) => queue,
            Err(_) => return Vec::new(),
        };
        self.wake_gate.store(false, Ordering::SeqCst);
        queue.drain(..).collect()
    }

    /// Registers a repaint waker invoked at most once per scheduled frame.
    pub fn install_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) -> SnapshotSubscription {
        self.wakers.register(waker)
    }
}

/// Join handle plus lifetime owner of the actor thread. Dropping the runtime
/// stops the actor gracefully and waits for its thread to finish; the process
/// itself is never terminated by dropping handles or runtimes.
pub struct AppRuntime {
    join_handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

#[derive(Clone, Default)]
struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Starts the actor with `initial` state. The initial snapshot is published
/// before the first event is processed.
#[allow(dead_code)] // tests and subproject 2 use the channel-less variant.
pub fn start_app(initial: AppState, executor: Box<dyn EffectExecutor>) -> (AppHandle, AppRuntime) {
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(EVENT_CHANNEL_CAPACITY);
    start_app_with_channel(initial, executor, event_tx, event_rx)
}

/// Like [`start_app`] but reuses a pre-built channel so worker services can
/// hold [`AppEventSender`] clones from before the actor exists.
pub fn start_app_with_channel(
    initial: AppState,
    executor: Box<dyn EffectExecutor>,
    event_tx: SyncSender<AppEvent>,
    event_rx: Receiver<AppEvent>,
) -> (AppHandle, AppRuntime) {
    let mut executor = executor;
    let snapshot = Arc::new(ArcSwap::from_pointee(UiSnapshot::from_state(&initial)));
    let ui_effects = Arc::new(Mutex::new(VecDeque::new()));
    let wakers = Arc::new(WakerRegistry::default());
    let wake_gate = Arc::new(AtomicBool::new(false));

    let handle = AppHandle {
        event_tx: event_tx.clone(),
        snapshot: Arc::clone(&snapshot),
        ui_effects: Arc::clone(&ui_effects),
        wakers: Arc::clone(&wakers),
        wake_gate: Arc::clone(&wake_gate),
    };
    let feedback = AppFeedback {
        events: AppEventSender::new(event_tx),
        ui_effects: UiEffectSender::new(
            Arc::clone(&ui_effects),
            Arc::clone(&wake_gate),
            Arc::clone(&wakers),
        ),
    };

    let stop_signal = StopSignal::default();
    let runtime_stop = Arc::clone(&stop_signal.0);

    let join_handle = std::thread::Builder::new()
        .name("openaircast-app-actor".into())
        .spawn(move || {
            run_actor(
                initial,
                event_rx,
                ActorContext {
                    executor: executor.as_mut(),
                    feedback: &feedback,
                    snapshot: &snapshot,
                    wake_gate: &wake_gate,
                    wakers: &wakers,
                    stop: &stop_signal,
                },
            );
        })
        .expect("spawn control center actor thread");

    (
        handle,
        AppRuntime {
            join_handle: Some(join_handle),
            stop: runtime_stop,
        },
    )
}

struct ActorContext<'a> {
    executor: &'a mut dyn EffectExecutor,
    feedback: &'a AppFeedback,
    snapshot: &'a ArcSwap<UiSnapshot>,
    wake_gate: &'a AtomicBool,
    wakers: &'a WakerRegistry,
    stop: &'a StopSignal,
}

impl ActorContext<'_> {
    fn publish(&self, state: &AppState) {
        self.snapshot.store(Arc::new(UiSnapshot::from_state(state)));
        schedule_frame(self.wake_gate, self.wakers);
    }
}

fn run_actor(mut state: AppState, events: Receiver<AppEvent>, context: ActorContext<'_>) {
    context.publish(&state);

    loop {
        if context.stop.requested() {
            break;
        }
        let event = match events.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(event) => event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let is_controller_result = matches!(event, AppEvent::Controller(_));
        let transition = reduce(&mut state, event);

        for effect in transition.effects {
            context.executor.execute(effect, context.feedback);
        }

        if transition.snapshot_changed {
            context.publish(&state);
        } else if is_controller_result {
            tracing::debug!(
                revision = state.revision,
                "ignored stale or non-matching controller result"
            );
        }
    }
}

/// Flips the shared wake gate false→true exactly once per scheduled frame so
/// bursts of publications or queued effects issue a single wake.
fn schedule_frame(wake_gate: &AtomicBool, wakers: &WakerRegistry) {
    if wake_gate
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        wakers.invoke_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use airplay_core::DeviceId;

    use super::*;
    use crate::app::{
        AppEvent, AppState, Availability, ControllerEvent, GenerationId, NoticeCode, Page,
        ReceiverState, Severity, StreamState, UiEffect, UserNotice,
    };
    use std::time::{Duration, Instant};

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn receiver_state(last: u8, name: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "HomePod".into(),
            availability,
        }
    }

    #[derive(Clone, Default)]
    struct WakeCounter(Arc<std::sync::Mutex<u32>>);

    impl WakeCounter {
        fn into_waker(self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(move || {
                *self.0.lock().unwrap() += 1;
            })
        }

        fn count(&self) -> u32 {
            *self.0.lock().unwrap()
        }
    }

    #[derive(Clone, Default)]
    struct EffectLog(Arc<std::sync::Mutex<Vec<AppEffect>>>);

    impl EffectLog {
        fn recorded(&self) -> Vec<AppEffect> {
            self.0.lock().unwrap().clone()
        }

        fn wait_for_len(&self, target: usize) -> Vec<AppEffect> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let current = self.recorded();
                if current.len() >= target {
                    return current;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {target} executed effects"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    struct MirroringExecutor {
        log: EffectLog,
    }

    impl EffectExecutor for MirroringExecutor {
        fn execute(&mut self, effect: AppEffect, feedback: &AppFeedback) {
            match &effect {
                AppEffect::ShowMainWindow => feedback.ui_effects.send(UiEffect::ShowMainWindow),
                AppEffect::HideMainWindow => feedback.ui_effects.send(UiEffect::HideMainWindow),
                _ => {}
            }
            self.log.0.lock().unwrap().push(effect);
        }
    }

    fn wait_for_revision(handle: &AppHandle, target: u64) -> Arc<UiSnapshot> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = handle.snapshot();
            if snapshot.revision >= target {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for revision {target}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Waits until at least `target` wakes have been observed.
    ///
    /// `publish` stores the snapshot revision *before* it calls
    /// `schedule_frame`, so a reader that observed a revision has proven
    /// nothing about the waker having run yet.
    ///
    /// Exact wake counts are only deterministic when every publication's wake
    /// is delivered *before* the drain that re-arms the gate. A drain that
    /// overtakes its own `schedule_frame` leaves the gate armed inside the
    /// next publication's window, and that publication then coalesces into a
    /// frame nobody is waiting for. Call this before each `drain_ui_effects`,
    /// not only before the final assertion.
    fn wait_for_wakes(counter: &WakeCounter, target: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = counter.count();
            if seen >= target {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {target} wakes; observed {seen}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn dispatch_is_bounded_non_blocking_and_reports_closure() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
        let handle = AppHandle {
            event_tx: tx,
            snapshot: Arc::new(ArcSwap::from_pointee(UiSnapshot::from_state(
                &AppState::default(),
            ))),
            ui_effects: Default::default(),
            wakers: Default::default(),
            wake_gate: Default::default(),
        };

        for volume in 0..256 {
            assert!(handle
                .dispatch(AppEvent::MasterVolumeChanged(volume as f32 / 512.0))
                .is_ok());
        }
        assert_eq!(
            handle.dispatch(AppEvent::MasterVolumeChanged(1.0)),
            Err(AppUnavailable::Busy)
        );

        drop(rx);
        assert_eq!(
            handle.dispatch(AppEvent::QuitRequested),
            Err(AppUnavailable::Closed)
        );
    }

    #[test]
    fn readers_only_observe_complete_old_or_new_revisions() {
        let executor = MirroringExecutor {
            log: Default::default(),
        };
        let (handle, _runtime) = start_app(AppState::default(), Box::new(executor));

        let violations = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut readers = Vec::new();
        for reader_index in 0..4 {
            let handle = handle.clone();
            let violations = violations.clone();
            let stop = stop.clone();
            readers.push(std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let snapshot = handle.snapshot();
                    let expected = if snapshot.revision % 2 == 0 {
                        Page::Home
                    } else {
                        Page::Speakers
                    };
                    if snapshot.page != expected {
                        violations.lock().unwrap().push(format!(
                            "reader {reader_index}: revision {} had page {:?}",
                            snapshot.revision, snapshot.page
                        ));
                    }
                    std::thread::yield_now();
                }
            }));
        }

        for round in 1..=30u64 {
            assert!(handle.dispatch(AppEvent::Navigate(Page::Speakers)).is_ok());
            wait_for_revision(&handle, round * 2 - 1);
            assert!(handle.dispatch(AppEvent::Navigate(Page::Home)).is_ok());
            wait_for_revision(&handle, round * 2);
        }

        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        for reader in readers {
            reader.join().expect("reader thread");
        }
        assert!(violations.lock().unwrap().is_empty());
        assert_eq!(handle.snapshot().revision, 60);
    }

    #[test]
    fn one_semantic_transition_wakes_once_and_bursts_coalesce_until_drain() {
        let counter = WakeCounter::default();
        let (handle, _runtime) = start_app(
            AppState::default(),
            Box::new(MirroringExecutor {
                log: Default::default(),
            }),
        );
        let subscription = handle.install_waker(counter.clone().into_waker());

        for value in [0.1f32, 0.2, 0.3, 0.4, 0.5] {
            assert!(handle
                .dispatch(AppEvent::MasterVolumeChanged(value))
                .is_ok());
        }
        wait_for_revision(&handle, 5);
        wait_for_wakes(&counter, 1);
        assert_eq!(
            counter.count(),
            1,
            "bursting publications wake exactly once"
        );

        assert!(handle.drain_ui_effects().is_empty());
        assert!(handle.dispatch(AppEvent::MasterVolumeChanged(0.6)).is_ok());
        wait_for_revision(&handle, 6);
        wait_for_wakes(&counter, 2);
        assert_eq!(
            counter.count(),
            2,
            "post-drain publication schedules a fresh frame"
        );

        drop(subscription);
    }

    #[test]
    fn dropped_subscription_stops_receiving_wakes() {
        let kept = WakeCounter::default();
        let dropped_counter = WakeCounter::default();
        let (handle, _runtime) = start_app(
            AppState::default(),
            Box::new(MirroringExecutor {
                log: Default::default(),
            }),
        );

        let kept_subscription = handle.install_waker(kept.clone().into_waker());
        let dropped_subscription = handle.install_waker(dropped_counter.clone().into_waker());
        drop(dropped_subscription);

        assert!(handle.dispatch(AppEvent::MasterVolumeChanged(0.5)).is_ok());
        wait_for_revision(&handle, 1);
        wait_for_wakes(&kept, 1);

        assert_eq!(kept.count(), 1);
        assert_eq!(dropped_counter.count(), 0);
        drop(kept_subscription);
    }

    #[test]
    fn ui_effects_drain_exactly_once_in_fifo_order_and_rearm_the_gate() {
        let log = EffectLog::default();
        let (handle, _runtime) = start_app(
            AppState::default(),
            Box::new(MirroringExecutor { log: log.clone() }),
        );

        assert!(handle.dispatch(AppEvent::ShowMainWindow).is_ok());
        assert!(handle.dispatch(AppEvent::MainWindowCloseRequested).is_ok());
        wait_for_revision(&handle, 1);
        assert!(handle.dispatch(AppEvent::ShowMainWindow).is_ok());
        wait_for_revision(&handle, 2);

        assert_eq!(
            handle.drain_ui_effects(),
            vec![
                UiEffect::ShowMainWindow,
                UiEffect::HideMainWindow,
                UiEffect::ShowMainWindow,
            ]
        );
        assert!(handle.drain_ui_effects().is_empty());

        assert!(handle.dispatch(AppEvent::MainWindowCloseRequested).is_ok());
        let _ = log.wait_for_len(4);
        assert_eq!(handle.drain_ui_effects(), vec![UiEffect::HideMainWindow]);
    }

    #[test]
    fn reversed_controller_generations_leave_only_the_newest_state() {
        let mut initial = AppState::default();
        initial
            .receivers
            .push(receiver_state(1, "Kitchen", Availability::Available));
        initial.desired_receivers.insert(rid(1));
        initial.staged_receivers.insert(rid(1));
        initial.stream = StreamState::Starting {
            generation: GenerationId(10),
        };
        initial.generations.session = GenerationId(10);

        let counter = WakeCounter::default();
        let (handle, _runtime) = start_app(
            initial,
            Box::new(MirroringExecutor {
                log: Default::default(),
            }),
        );
        let subscription = handle.install_waker(counter.clone().into_waker());

        assert!(handle
            .dispatch(AppEvent::Controller(ControllerEvent::SessionStarted {
                generation: GenerationId(9),
                active_receiver_ids: vec![rid(1)],
            }))
            .is_ok());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(handle.snapshot().revision, 0, "stale started is a noop");

        assert!(handle
            .dispatch(AppEvent::Controller(ControllerEvent::SessionStarted {
                generation: GenerationId(10),
                active_receiver_ids: vec![rid(1)],
            }))
            .is_ok());
        wait_for_revision(&handle, 1);
        wait_for_wakes(&counter, 1);
        handle.drain_ui_effects();

        assert!(handle.dispatch(AppEvent::StopRequested).is_ok());
        wait_for_revision(&handle, 2);
        wait_for_wakes(&counter, 2);
        handle.drain_ui_effects();

        assert!(handle
            .dispatch(AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: GenerationId(9),
            }))
            .is_ok());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(handle.snapshot().revision, 2, "stale stopped is a noop");

        assert!(handle
            .dispatch(AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: GenerationId(11),
            }))
            .is_ok());
        wait_for_revision(&handle, 3);
        wait_for_wakes(&counter, 3);
        handle.drain_ui_effects();

        let final_snapshot = handle.snapshot();
        assert_eq!(final_snapshot.stream, crate::app::StreamSnapshot::Stopped);
        assert_eq!(final_snapshot.revision, 3);
        assert_eq!(counter.count(), 3, "only semantic transitions woke the UI");
        drop(subscription);
    }

    #[test]
    fn controller_channel_closure_publishes_needs_attention_while_quit_stays_dispatchable() {
        let log = EffectLog::default();
        let (handle, runtime) = start_app(
            AppState::default(),
            Box::new(MirroringExecutor { log: log.clone() }),
        );

        assert!(handle
            .dispatch(AppEvent::Controller(ControllerEvent::ChannelClosed {
                summary: "device service stopped unexpectedly".into(),
            }))
            .is_ok());
        let snapshot = wait_for_revision(&handle, 1);
        assert_eq!(
            snapshot.notice,
            Some(UserNotice {
                severity: Severity::Error,
                code: NoticeCode::ControllerUnavailable,
                summary: "device service stopped unexpectedly".into(),
                action: None,
            })
        );
        assert!(!snapshot.shutting_down);

        assert_eq!(handle.dispatch(AppEvent::QuitRequested), Ok(()));
        wait_for_revision(&handle, 2);
        let effects = log.wait_for_len(3);
        assert!(effects.contains(&AppEffect::BeginShutdown));
        assert!(handle.snapshot().shutting_down);

        drop(handle);
        drop(runtime);
    }

    #[test]
    fn unknown_sender_errors_map_to_typed_unavailable_reasons() {
        assert_eq!(
            AppUnavailable::Busy.to_string(),
            "application event queue is busy"
        );
        assert_eq!(
            AppUnavailable::Closed.to_string(),
            "application actor is closed"
        );
    }
}
