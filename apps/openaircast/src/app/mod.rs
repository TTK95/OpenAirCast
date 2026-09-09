mod effect;
mod event;
pub mod hardware_check;
mod live_diagnostics;
mod reducer;
mod snapshot;
mod state;

#[allow(unused_imports)]
pub use effect::*;
#[allow(unused_imports)]
pub use event::*;
pub use live_diagnostics::*;
#[allow(unused_imports)]
pub use reducer::*;
#[allow(unused_imports)]
pub use snapshot::*;
#[allow(unused_imports)]
pub use state::*;

// ===========================================================================
// Production launch path and service assembly (Task 14).
// ===========================================================================

/// Scheduling seam for the preference worker so tests can fake it.
pub(crate) trait PreferenceSink: Send + Sync {
    fn try_schedule(
        &self,
        generation: GenerationId,
        value: Preferences,
    ) -> Result<(), crate::preferences::PreferencesUnavailable>;
}

impl PreferenceSink for crate::preferences::PreferencesWorkerHandle {
    fn try_schedule(
        &self,
        generation: GenerationId,
        value: Preferences,
    ) -> Result<(), crate::preferences::PreferencesUnavailable> {
        crate::preferences::PreferencesWorkerHandle::try_schedule(self, generation, value)
    }
}

/// Reconfiguration seam for the hotkey service so tests can fake it.
pub(crate) trait HotkeySink: Send + Sync {
    fn try_reconfigure(
        &self,
        generation: GenerationId,
        binding: HotkeyBinding,
    ) -> Result<(), crate::platform::hotkey::HotkeyError>;
}

impl HotkeySink for crate::platform::hotkey::HotkeyServiceHandle {
    fn try_reconfigure(
        &self,
        generation: GenerationId,
        binding: HotkeyBinding,
    ) -> Result<(), crate::platform::hotkey::HotkeyError> {
        crate::platform::hotkey::HotkeyServiceHandle::try_reconfigure(self, generation, binding)
    }
}

/// Bounded cleanup seams consumed by the shutdown coordinator.
pub(crate) trait BoundedShutdown: Send + Sync {
    fn shutdown_device(&self, timeout: std::time::Duration) -> bool;
    fn shutdown_hotkey(&self, timeout: std::time::Duration) -> bool;
    fn flush_preferences(&self) -> bool;
}

/// The device stage of the bounded exit, whichever service owns it.
///
/// Named as its own seam because the shell has two implementations of it now
/// -- the legacy service and the resilience backend -- and the coordinator
/// must not be able to tell them apart. Kept free of every backend name so
/// this file stays on the shell's side of the boundary the bridge enforces.
pub(crate) trait DeviceShutdown: Send + Sync {
    /// Requests shutdown, waits up to `timeout`, and reports whether the
    /// device stage finished within it.
    fn shutdown_and_wait(&self, timeout: std::time::Duration) -> bool;
}

/// Everything `run()` needs from whichever device stage it was handed.
///
/// The budget travels with the implementation on purpose. The coordinator's
/// device stage used to be one constant for one service; a second service
/// with a longer teardown of its own would otherwise be cut short by a number
/// written for the first one, on every exit, with nothing on screen saying so.
pub(crate) struct DeviceSeam {
    /// Where reducer effects are sent.
    pub(crate) controller: std::sync::Arc<dyn ControllerPort>,
    /// The device stage of the bounded exit.
    pub(crate) shutdown: std::sync::Arc<dyn DeviceShutdown>,
    /// How long this implementation needs to tear down.
    pub(crate) budget: std::time::Duration,
}

/// Which implementation `run()` opens the device stage on.
///
/// The choice used to be the identity of a function named inline in `run()`,
/// which made it the one decision in the shell that no test could see: the
/// whole suite stayed green with the app wired back to the legacy service,
/// because every test builds its seam directly. A value can be asserted; a
/// call site cannot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceStage {
    /// The resilience backend, through `backend_bridge`.
    Backend,
    /// The legacy device service. The way back, kept reachable on purpose.
    Legacy,
}

impl DeviceStage {
    /// Starts this stage and returns the seam `run()` consumes.
    ///
    /// Both arms have the same signature and produce the same seam, budget
    /// included, so switching is this one value and nothing else.
    pub(crate) fn open(
        self,
        events: crate::app_handle::AppEventSender,
        appdata_dir: std::path::PathBuf,
    ) -> anyhow::Result<DeviceSeam> {
        match self {
            DeviceStage::Backend => crate::backend_bridge::backend_device_seam(events, appdata_dir),
            DeviceStage::Legacy => crate::device_service::legacy_device_seam(events, appdata_dir),
        }
    }
}

/// The switch. Changing this constant is the entire way back to the legacy
/// service; a test in this file fails when it moves, so the change cannot be
/// made silently.
pub(crate) const ACTIVE_DEVICE_STAGE: DeviceStage = DeviceStage::Backend;

pub(crate) struct RealBoundedShutdown {
    pub device: std::sync::Arc<dyn DeviceShutdown>,
    pub hotkey: crate::platform::hotkey::HotkeyServiceHandle,
    pub preferences: crate::preferences::PreferencesWorkerHandle,
}

impl BoundedShutdown for RealBoundedShutdown {
    fn shutdown_device(&self, timeout: std::time::Duration) -> bool {
        self.device.shutdown_and_wait(timeout)
    }

    fn shutdown_hotkey(&self, timeout: std::time::Duration) -> bool {
        self.hotkey.shutdown_and_wait(timeout)
    }

    fn flush_preferences(&self) -> bool {
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel::<()>(1);
        self.preferences.try_flush_and_shutdown(ack_tx).is_ok()
            && ack_rx.recv_timeout(PREFERENCE_FLUSH_BUDGET).is_ok()
    }
}

/// How long the coordinator waits for the settings file to be written.
///
/// Named rather than spelled into the `recv_timeout` above because the exit
/// valve is sized from it: a flush the valve does not cover would be cut in
/// the middle of writing the user's settings.
pub(crate) const PREFERENCE_FLUSH_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// What the shell still has to do after the coordinator queues its close:
/// drain the effect, tear the viewport down, and drop the actor thread.
const VIEWPORT_TEARDOWN_SLACK: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Copy)]
pub(crate) struct ShutdownBudgets {
    pub device: std::time::Duration,
    pub hotkey: std::time::Duration,
}

impl Default for ShutdownBudgets {
    fn default() -> Self {
        Self {
            device: std::time::Duration::from_secs(3),
            hotkey: std::time::Duration::from_secs(1),
        }
    }
}

impl ShutdownBudgets {
    /// Longest the coordinator can run before it queues `CloseApplication`:
    /// every stage is sequential, so the budgets add up.
    pub(crate) fn worst_case(&self) -> std::time::Duration {
        self.device
            .saturating_add(self.hotkey)
            .saturating_add(PREFERENCE_FLUSH_BUDGET)
    }

    /// Budget for the exit valve, derived from the budgets actually in force
    /// rather than stated as a second, independent number.
    ///
    /// Both values used to be literals in unrelated lines, and nothing tied
    /// them together: raising the device budget past the valve's would have
    /// let the valve fire in the middle of the preference flush -- settings
    /// lost, and no failure reported anywhere. Deriving it means a longer
    /// teardown moves the valve with it.
    pub(crate) fn exit_valve(&self) -> std::time::Duration {
        self.worst_case().saturating_add(VIEWPORT_TEARDOWN_SLACK)
    }
}

/// Wires reducer effects onto the real services; every operation returns
/// after queueing work and never performs blocking work inline.
pub(crate) struct ServiceExecutor {
    pub controller: std::sync::Arc<dyn ControllerPort>,
    pub preferences: std::sync::Arc<dyn PreferenceSink>,
    pub hotkeys: std::sync::Arc<dyn HotkeySink>,
    pub shutdown: std::sync::Arc<dyn BoundedShutdown>,
    pub budgets: ShutdownBudgets,
    /// Which valve the shutdown coordinator arms; see [`ExitValvePolicy`].
    pub exit_valve: ExitValvePolicy,
    pub begin_shutdown_started: std::sync::atomic::AtomicBool,
}

impl ServiceExecutor {
    fn report_channel_loss(&self, feedback: &crate::app_handle::AppFeedback, summary: &str) {
        let _ = feedback
            .events
            .try_send(AppEvent::Controller(ControllerEvent::ChannelClosed {
                summary: summary.to_string(),
            }));
    }
}

impl crate::app_handle::EffectExecutor for ServiceExecutor {
    fn execute(&mut self, effect: AppEffect, feedback: &crate::app_handle::AppFeedback) {
        match effect {
            AppEffect::PlayListeningTone { generation } => {
                if self
                    .controller
                    .try_send(DeviceCommand::PlayListeningTone { generation })
                    .is_err()
                {
                    let _ = feedback.events.try_send(AppEvent::Controller(
                        ControllerEvent::SessionFailed {
                            generation,
                            summary: "The listening test tone could not be started.".into(),
                        },
                    ));
                }
            }
            AppEffect::Group { .. }
            | AppEffect::Discover { .. }
            | AppEffect::StartSession { .. }
            | AppEffect::StopSession { .. }
            | AppEffect::ApplyVolume { .. }
            | AppEffect::ApplyMute(_)
            | AppEffect::ApplyReceiverLevel { .. }
            | AppEffect::ApplyLatency(_)
            | AppEffect::ApplyAudioEndpoint(_) => {
                if let Some(command) = crate::device_service::effect_to_command(&effect) {
                    match self.controller.try_send(command) {
                        Ok(()) => {}
                        Err(ControllerSendError::Busy) => {
                            tracing::warn!("device service queue busy; command dropped");
                        }
                        Err(ControllerSendError::Closed) => {
                            self.report_channel_loss(feedback, "the audio controller stopped");
                        }
                    }
                }
            }
            AppEffect::PersistPreferences { generation, value } => {
                if let Err(error) = self.preferences.try_schedule(generation, value) {
                    // Busy and Closed alike: the settings file was not
                    // written, and the shell is entitled to say so about that
                    // write specifically. Reporting a lost audio controller
                    // here would blame streaming for a full preferences queue
                    // and leave the failed save invisible.
                    tracing::warn!(?generation, "could not schedule preferences: {error}");
                    let _ = feedback.events.try_send(AppEvent::Preferences(
                        PreferencesEvent::PersistFailed { generation },
                    ));
                }
            }
            AppEffect::ReconfigureHotkey { generation, value } => {
                match self.hotkeys.try_reconfigure(generation, value) {
                    Ok(()) => {}
                    Err(error) => {
                        tracing::warn!("could not reconfigure hotkey: {error}");
                        if error == crate::platform::hotkey::HotkeyError::Closed {
                            self.report_channel_loss(feedback, "the shortcut service stopped");
                        }
                    }
                }
            }
            AppEffect::ShowMainWindow => {
                feedback.ui_effects.send(UiEffect::ShowMainWindow);
            }
            AppEffect::HideMainWindow => {
                feedback.ui_effects.send(UiEffect::HideMainWindow);
            }
            AppEffect::BeginShutdown => {
                self.begin_bounded_shutdown(feedback);
            }
        }
    }
}

impl ServiceExecutor {
    fn begin_bounded_shutdown(&mut self, feedback: &crate::app_handle::AppFeedback) {
        use std::sync::atomic::Ordering;
        if self.begin_shutdown_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let shutdown = std::sync::Arc::clone(&self.shutdown);
        let budgets = self.budgets;
        let exit_valve = self.exit_valve.clone();
        let ui_effects = feedback.ui_effects.clone();

        let spawned = std::thread::Builder::new()
            .name("openaircast-shutdown".into())
            .spawn(move || {
                run_shutdown_coordinator(shutdown, budgets, ui_effects, move |budget| {
                    exit_valve.arm(budget)
                });
            });
        if let Err(error) = spawned {
            // Same rule as the valve below: a thread the OS will not give us
            // must not turn Quit into a window that never closes. The workers
            // then go down with the process instead of before it.
            tracing::error!("could not start the shutdown coordinator ({error}); closing anyway");
            feedback.ui_effects.send(UiEffect::DropTray);
            feedback.ui_effects.send(UiEffect::CloseApplication);
        }
    }
}

/// The bounded exit, as run by the coordinator thread.
///
/// `arm_valve` is a seam: production hands it [`ExitWatchdog::arm`], tests
/// hand it a failure so the coordinator's behaviour without a valve can be
/// asserted. It receives the budget rather than reading a constant, which is
/// what keeps the valve tied to the budgets actually in force.
fn run_shutdown_coordinator(
    shutdown: std::sync::Arc<dyn BoundedShutdown>,
    budgets: ShutdownBudgets,
    ui_effects: crate::app_handle::UiEffectSender,
    arm_valve: impl FnOnce(std::time::Duration) -> std::io::Result<ExitWatchdog>,
) {
    // Armed for the whole exit, including the viewport teardown and the
    // runtime drops that happen after this thread is gone. It is deliberately
    // never disarmed: a shell that gets all the way out ends the process,
    // which ends this thread with it. See `ExitWatchdog` for why the valve
    // exists.
    //
    // A valve that cannot be armed is logged and skipped. Arming is a thread
    // spawn and can fail; letting that failure end this thread would take the
    // whole teardown with it -- no worker stopped, no settings written, and
    // `CloseApplication` never queued, so the window would stay open forever
    // with the shutdown latch already closed against a second attempt. Losing
    // the valve costs the last resort; panicking here costs the exit itself.
    let _watchdog = match arm_valve(budgets.exit_valve()) {
        Ok(watchdog) => Some(watchdog),
        Err(error) => {
            tracing::error!("exit valve unavailable ({error}); shutting down without it");
            None
        }
    };

    // Tray disappears immediately so Quit feels responsive.
    ui_effects.send(UiEffect::DropTray);

    let device_done = shutdown.shutdown_device(budgets.device);
    if !device_done {
        tracing::warn!("device service cleanup exceeded its budget");
    }
    let hotkey_done = shutdown.shutdown_hotkey(budgets.hotkey);
    if !hotkey_done {
        tracing::warn!("hotkey cleanup exceeded its budget");
    }
    let prefs_done = shutdown.flush_preferences();
    if !prefs_done {
        tracing::warn!("preference flush exceeded its budget");
    }

    // CloseApplication is queued regardless of acknowledgements.
    ui_effects.send(UiEffect::CloseApplication);
}

/// Bounded last-resort exit valve.
///
/// The shell's exit path is a sequence of joins: the coordinator stops the
/// device service, the hotkey service, and the preference worker, the viewport
/// closes, `run()` returns, and the process ends because `main` returned. That
/// is the whole normal path and it contains no forced termination.
///
/// This exists for the case where that path does not complete. The AirPlay
/// stack this shell drives is alpha: `Streamer::stop` gives up on joining its
/// sender thread after a budget and leaves a `spawn_blocking` task behind, and
/// a blocking task cannot be aborted -- so a Tokio runtime that still owns one
/// can block forever on drop. An app that hangs after the user clicked Quit is
/// worse than one that is killed a few seconds late, so the valve stays. What
/// changed is when it opens: it is armed by the shutdown request and only ever
/// fires after the entire bounded teardown has already failed to finish, where
/// previously the process was killed on every single exit, successful or not.
///
/// The watchdog owns exactly one parked thread and no other state; arming it
/// costs nothing while the app runs, because it is armed only at shutdown.
pub(crate) struct ExitWatchdog {
    disarm: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ExitWatchdog {
    /// Arms the valve for `budget`, running `force` if it expires first.
    ///
    /// `force` is injected rather than hard-coded so the policy can be
    /// asserted without killing the test process. The spawn failure is
    /// returned rather than raised: the caller is the shutdown coordinator,
    /// and an exit path that dies while arming its own last resort is worse
    /// than one that runs without it.
    pub(crate) fn arm(
        budget: std::time::Duration,
        force: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<Self> {
        let disarm = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let watched = std::sync::Arc::clone(&disarm);
        let thread = std::thread::Builder::new()
            .name("openaircast-exit-watchdog".into())
            .spawn(move || {
                let (flag, signal) = &*watched;
                let guard = flag.lock().expect("exit watchdog flag poisoned");
                // `wait_timeout_while` absorbs spurious wakeups and keeps the
                // total wait bounded by `budget`, so the valve cannot open
                // early and cannot be re-armed by one.
                let (disarmed, outcome) = signal
                    .wait_timeout_while(guard, budget, |disarmed| !*disarmed)
                    .expect("exit watchdog flag poisoned");
                let expired = outcome.timed_out() && !*disarmed;
                // Never hold the lock across the valve: `force` does not
                // return in production.
                drop(disarmed);
                if expired {
                    force();
                }
            })?;
        Ok(Self {
            disarm,
            thread: std::sync::Mutex::new(Some(thread)),
        })
    }

    /// Closes the valve for good; safe to call after it has already opened.
    pub(crate) fn disarm(&self) {
        let (flag, signal) = &*self.disarm;
        *flag.lock().expect("exit watchdog flag poisoned") = true;
        signal.notify_all();
    }

    /// Waits for the watchdog thread to end, leaving the value usable.
    #[cfg(test)]
    fn wait_for_thread(&self) {
        let handle = self
            .thread
            .lock()
            .expect("exit watchdog handle poisoned")
            .take();
        if let Some(handle) = handle {
            handle.join().expect("exit watchdog thread panicked");
        }
    }

    /// Consuming [`Self::wait_for_thread`].
    #[cfg(test)]
    fn join(self) {
        self.wait_for_thread();
    }
}

/// How the shutdown coordinator arms its last-resort exit valve.
///
/// Production hands this [`Self::terminate_process`], which ends the process
/// when the bounded teardown fails to finish. That action is deliberately
/// never disarmed (see [`run_shutdown_coordinator`]) -- correct for the shell,
/// fatal for a test binary: a test that reaches this path with the production
/// valve installed does not fail. It terminates the whole test run once the
/// budget expires, with exit code 0 and no `test result` line, so every test
/// scheduled after that point silently never runs.
///
/// Making the policy a field of [`ServiceExecutor`] rather than a literal at
/// the call site means every construction site has to say which valve it
/// wants, and the compiler is the one asking.
#[derive(Clone)]
pub(crate) struct ExitValvePolicy(
    std::sync::Arc<dyn Fn(std::time::Duration) -> std::io::Result<ExitWatchdog> + Send + Sync>,
);

impl ExitValvePolicy {
    /// The shell's valve: end the process once the budget expires.
    pub(crate) fn terminate_process() -> Self {
        Self(std::sync::Arc::new(|budget| {
            ExitWatchdog::arm(budget, || std::process::exit(0))
        }))
    }

    /// Records the budget it was armed for and does nothing when it opens.
    ///
    /// The watchdog is disarmed before it is handed back, so a test that
    /// drives the real shutdown path leaves no parked thread behind.
    #[cfg(test)]
    pub(crate) fn recording() -> (
        Self,
        std::sync::Arc<std::sync::Mutex<Vec<std::time::Duration>>>,
    ) {
        let armed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&armed);
        let policy = Self(std::sync::Arc::new(move |budget| {
            recorder
                .lock()
                .expect("exit valve record poisoned")
                .push(budget);
            let watchdog = ExitWatchdog::arm(budget, || {})?;
            watchdog.disarm();
            Ok(watchdog)
        }));
        (policy, armed)
    }

    fn arm(&self, budget: std::time::Duration) -> std::io::Result<ExitWatchdog> {
        (self.0)(budget)
    }
}

/// Carries Windows display-language changes into the app as reducer input.
///
/// The send is bounded and non-blocking, because this runs on the window
/// procedure: a full or closed queue costs a log line, never a stalled UI
/// thread. Dropping one report is survivable -- the monitor's cache still
/// holds the current reading, and the next change re-reports it.
struct WindowsLocaleBridge {
    events: crate::app_handle::AppEventSender,
}

impl crate::platform::WindowsSettingsEvents for WindowsLocaleBridge {
    fn windows_display_language_changed(&self, locale: ResolvedLocale) {
        if let Err(error) = self
            .events
            .try_send(AppEvent::WindowsDisplayLanguageChanged(locale))
        {
            tracing::warn!(?locale, "display-language change not delivered: {error}");
        }
    }
}

/// Builds the first `AppState` from what was stored and what Windows reports.
///
/// Pure and total: no I/O, no clock, no services, so the startup contract is
/// checkable without an event loop. Every stored preference is honoured --
/// including launch-at-startup, which an earlier build silently forced off on
/// every launch -- and the geometry is validated once so a bad stored value
/// cannot produce an unusable window.
fn initial_state(
    loaded: crate::preferences::PreferencesLoad,
    windows_display_locale: ResolvedLocale,
) -> AppState {
    let mut state = AppState {
        hotkey: loaded.value.hotkey,
        theme: loaded.value.theme,
        page: loaded.value.last_page,
        locale_preference: loaded.value.locale,
        windows_display_locale,
        resolved_locale: loaded.value.locale.resolve(windows_display_locale),
        advanced_information: loaded.value.advanced_information,
        window: WindowState {
            visible: true,
            geometry: loaded.value.window,
            close_to_tray: loaded.value.close_to_tray,
            launch_at_startup: loaded.value.launch_at_startup,
        },
        notice: loaded.notice,
        ..AppState::default()
    };
    state.window.geometry = state.window.geometry.validate().unwrap_or_default();

    state
}

/// Subclasses the real application window so Windows setting changes arrive
/// as messages.
///
/// A failure here is not fatal: the shell falls back to the reading taken at
/// startup, which is coherent, merely no longer live.
fn attach_settings_monitor(
    cc: &eframe::CreationContext<'_>,
    initial: crate::platform::WindowsSettingsSnapshot,
    events: crate::app_handle::AppEventSender,
) -> crate::platform::WindowsSettingsMonitor {
    use raw_window_handle::HasWindowHandle;

    let repaint_ctx = cc.egui_ctx.clone();
    let request_repaint: std::sync::Arc<dyn Fn() + Send + Sync> =
        std::sync::Arc::new(move || repaint_ctx.request_repaint());

    let attached = match cc.window_handle() {
        Ok(window) => crate::platform::attach_windows_settings_monitor(
            window.as_raw(),
            initial,
            std::sync::Arc::new(WindowsLocaleBridge { events }),
            request_repaint,
        ),
        Err(error) => {
            tracing::warn!("no window handle for the settings monitor: {error}");
            return crate::platform::WindowsSettingsMonitor::detached(initial);
        }
    };

    match attached {
        Ok(monitor) => monitor,
        Err(error) => {
            tracing::warn!("windows settings monitor unavailable: {error}");
            crate::platform::WindowsSettingsMonitor::detached(initial)
        }
    }
}

/// The window the app opens, built from the geometry it was last left at.
///
/// The size was persisted, loaded, validated, and put into `AppState` -- and
/// then never used: the viewport was built from a hard-coded 1120 x 720, so a
/// user who resized the window found it back at the built-in size on every
/// launch. `WindowGeometry::validate` guarantees the minimum, so the clamp
/// below only restates it to `eframe`.
///
/// The position is deliberately *not* restored. It is persisted, because the
/// monitor arrangement needed to judge it is a later task's; restoring an x/y
/// from a monitor that is no longer attached puts the window somewhere the
/// user cannot reach it, and that is worse than opening centred.
pub(crate) fn main_viewport(geometry: WindowGeometry) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("OpenAirCast")
        .with_inner_size([geometry.width, geometry.height])
        .with_min_inner_size([900.0, 600.0])
        .with_maximized(geometry.maximized)
}

/// Normal launch path: load preferences, start workers, start the actor, and
/// run the eframe event loop until Quit closes the viewport.
pub fn run() -> anyhow::Result<()> {
    let appdata = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .map_err(|_| anyhow::anyhow!("APPDATA is not set; cannot resolve settings folder"))?;

    let paths = crate::preferences::PreferencesPaths::new(appdata.clone());
    let repository = crate::preferences::PreferencesRepository::new(paths);
    let loaded = repository.load();

    // Read Windows once, before any state, worker, actor, or frame exists, so
    // the first painted frame already has the real contrast, colours, motion
    // preference, and display language. Everything after this point is driven
    // by `WM_SETTINGCHANGE` and `WM_THEMECHANGED`, never by a poll.
    let initial_settings = crate::platform::read_windows_settings();

    let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
    let service_events = crate::app_handle::AppEventSender::new(event_tx.clone());

    // ---------------------------------------------------------------
    // The switch. `ACTIVE_DEVICE_STAGE` decides which device stage the
    // app runs on, and this is the only line in the shell that opens
    // one. Back out by changing that constant, not this call: two tests
    // below pin the constant and pin that this line still goes through
    // it. Nothing else in this function, in the executor, or in the
    // coordinator refers to either implementation.
    // ---------------------------------------------------------------
    let device = ACTIVE_DEVICE_STAGE.open(service_events.clone(), appdata)?;

    let hotkey_service =
        crate::platform::hotkey::spawn_hotkey_service(loaded.value.hotkey, service_events.clone())?;
    let preferences_worker =
        crate::preferences::spawn_preferences_worker(repository.clone(), service_events.clone());

    let initial = initial_state(loaded, initial_settings.locale);

    let executor = ServiceExecutor {
        controller: std::sync::Arc::clone(&device.controller),
        preferences: std::sync::Arc::new(preferences_worker.clone()),
        hotkeys: std::sync::Arc::new(hotkey_service.clone()),
        shutdown: std::sync::Arc::new(RealBoundedShutdown {
            device: std::sync::Arc::clone(&device.shutdown),
            hotkey: hotkey_service.clone(),
            preferences: preferences_worker.clone(),
        }),
        // Taken from the seam, not from the default: the device stage is the
        // one budget that depends on which implementation is behind it, and
        // `exit_valve` is derived from these, so the valve moves with it.
        budgets: ShutdownBudgets {
            device: device.budget,
            ..ShutdownBudgets::default()
        },
        exit_valve: ExitValvePolicy::terminate_process(),
        begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
    };

    let (handle, runtime) =
        crate::app_handle::start_app_with_channel(initial, Box::new(executor), event_tx, event_rx);

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: main_viewport(handle.snapshot().window.geometry),
        ..Default::default()
    };

    let app_handle_for_ui = handle.clone();
    let locale_events_for_ui = service_events.clone();
    let segoe_ui_variable = crate::ui::theme::load_segoe_ui_variable();
    let run_result = eframe::run_native(
        "OpenAirCast",
        options,
        Box::new(move |cc| {
            let monitor =
                attach_settings_monitor(cc, initial_settings, locale_events_for_ui.clone());
            let mut application = crate::ui::ControlCenterApp::new(
                cc,
                app_handle_for_ui.clone(),
                monitor,
                segoe_ui_variable.clone(),
            );
            match crate::tray::TrayController::new(app_handle_for_ui.clone()) {
                Ok(tray) => application.install_tray(tray),
                Err(error) => tracing::warn!("tray unavailable: {error}"),
            }
            Ok(Box::new(application))
        }),
    );

    // The actor stops when the runtime drops; workers were already shut down
    // by the coordinator during BeginShutdown.
    drop(runtime);

    run_result.map_err(|error| anyhow::anyhow!("GUI loop failed: {error}"))?;
    // No forced exit: every worker was signalled and joined by the shutdown
    // coordinator, the actor thread was joined above, and returning from here
    // returns from `main`, which ends the process. The valve armed at
    // `BeginShutdown` covers the case where one of those joins does not
    // finish; on the path taken here it never fires.
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The switch itself: which device stage the shipped app opens, and that
    /// `run()` still asks the switch instead of naming a constructor.
    ///
    /// `run()` cannot be called from a test -- it opens sockets, a window, and
    /// the audio endpoint -- so these two are what is checkable about it. They
    /// exist because reverting the switch by hand left all 557 tests in this
    /// binary green: every other test builds its seam directly and none of
    /// them reaches this decision.
    mod switch {
        use super::super::{DeviceStage, ACTIVE_DEVICE_STAGE};

        #[test]
        fn the_shipped_app_opens_the_resilience_backend() {
            assert_eq!(
                ACTIVE_DEVICE_STAGE,
                DeviceStage::Backend,
                "the app is wired back to the legacy device service; if that is \
                 intended, this assertion is the one place that has to say so"
            );
        }

        #[test]
        fn run_opens_the_device_stage_through_the_switch() {
            let source = include_str!("mod.rs");
            let line = source
                .lines()
                .find(|line| line.trim_start().starts_with("let device ="))
                .expect("`run()` still binds the device seam to `device`");

            assert!(
                line.contains("ACTIVE_DEVICE_STAGE.open("),
                "`run()` must open the device stage through the switch so the \
                 choice stays assertable, got: {line}"
            );
        }
    }

    mod lifecycle {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{
            BoundedShutdown, ExitValvePolicy, HotkeySink, PreferenceSink, ServiceExecutor,
            ShutdownBudgets,
        };
        use crate::app::{
            AppEvent, AppState, ControllerPort, ControllerSendError, DeviceCommand, GenerationId,
            HotkeyBinding, Page, Preferences, ResolvedLocale, StreamSnapshot, UiEffect,
        };
        use crate::app_handle::start_app_with_channel;

        #[derive(Default)]
        struct FakeControllerInner {
            commands: Vec<DeviceCommand>,
            closed: bool,
        }

        #[derive(Clone, Default)]
        struct FakeController(Arc<Mutex<FakeControllerInner>>);

        impl ControllerPort for FakeController {
            fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError> {
                let mut inner = self.0.lock().unwrap();
                if inner.closed {
                    return Err(ControllerSendError::Closed);
                }
                inner.commands.push(command);
                Ok(())
            }
        }

        #[derive(Default)]
        struct FakePrefsInner {
            saves: Vec<(GenerationId, Preferences)>,
            closed: bool,
        }

        #[derive(Clone, Default)]
        struct FakePrefs(Arc<Mutex<FakePrefsInner>>);

        impl PreferenceSink for FakePrefs {
            fn try_schedule(
                &self,
                generation: GenerationId,
                value: Preferences,
            ) -> Result<(), crate::preferences::PreferencesUnavailable> {
                let mut inner = self.0.lock().unwrap();
                if inner.closed {
                    return Err(crate::preferences::PreferencesUnavailable::Closed);
                }
                inner.saves.push((generation, value));
                Ok(())
            }
        }

        #[derive(Clone, Default)]
        struct FakeHotkeys {
            calls: Arc<Mutex<Vec<HotkeyBinding>>>,
        }

        impl HotkeySink for FakeHotkeys {
            fn try_reconfigure(
                &self,
                _generation: GenerationId,
                binding: HotkeyBinding,
            ) -> Result<(), crate::platform::hotkey::HotkeyError> {
                self.calls.lock().unwrap().push(binding);
                Ok(())
            }
        }

        #[derive(Clone, Default)]
        pub(super) struct FakeShutdown {
            pub(super) order: Arc<Mutex<Vec<&'static str>>>,
            pub(super) block_device_until: Option<std::time::Instant>,
            pub(super) completed: Arc<std::sync::atomic::AtomicBool>,
        }

        impl BoundedShutdown for FakeShutdown {
            fn shutdown_device(&self, timeout: Duration) -> bool {
                self.order.lock().unwrap().push("device:start");
                let deadline = std::time::Instant::now() + timeout;
                if let Some(until) = self.block_device_until {
                    while std::time::Instant::now() < until && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                self.order.lock().unwrap().push("device:end");
                true
            }

            fn shutdown_hotkey(&self, _timeout: Duration) -> bool {
                self.order.lock().unwrap().push("hotkey");
                true
            }

            fn flush_preferences(&self) -> bool {
                self.order.lock().unwrap().push("prefs");
                self.completed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                true
            }
        }

        fn make_executor() -> (
            crate::app_handle::AppHandle,
            FakeController,
            FakePrefs,
            FakeShutdown,
            std::thread::JoinHandle<()>,
        ) {
            let controller = FakeController::default();
            let prefs = FakePrefs::default();
            let shutdown = FakeShutdown::default();

            let executor = ServiceExecutor {
                controller: Arc::new(controller.clone()),
                preferences: Arc::new(prefs.clone()),
                hotkeys: Arc::new(FakeHotkeys::default()),
                shutdown: Arc::new(shutdown.clone()),
                budgets: ShutdownBudgets {
                    device: Duration::from_millis(300),
                    hotkey: Duration::from_millis(100),
                },
                exit_valve: ExitValvePolicy::recording().0,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            };

            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
            let (handle, _runtime_keepalive) =
                start_app_with_channel(AppState::default(), Box::new(executor), event_tx, event_rx);
            // Keep the actor running for the test by leaking the runtime join;
            // the AppRuntime drop would stop it mid-test otherwise.
            std::mem::forget(_runtime_keepalive);

            (
                handle,
                controller,
                prefs,
                shutdown,
                std::thread::spawn(|| {}),
            )
        }

        fn wait_for(handle: &crate::app_handle::AppHandle, target: u64) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if handle.snapshot().revision >= target {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "revision {target} never published"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        #[test]
        fn initial_snapshot_publishes_immediately_without_events() {
            let (handle, _c, _p, _s, _join) = make_executor();
            assert_eq!(handle.snapshot().revision, 0);
            assert!(matches!(handle.snapshot().stream, StreamSnapshot::Stopped));
        }

        #[test]
        fn startup_refresh_dispatches_discover_to_the_controller() {
            let (handle, controller, _p, _s, _join) = make_executor();
            assert!(handle.dispatch(AppEvent::RefreshRequested).is_ok());
            wait_for(&handle, 1);

            assert!(matches!(
                controller.0.lock().unwrap().commands.first(),
                Some(DeviceCommand::Discover {
                    generation: GenerationId(1)
                })
            ));
        }

        #[test]
        fn main_window_close_hides_only_and_keeps_page() {
            let (handle, _c, _p, _s, _join) = make_executor();
            assert!(handle.dispatch(AppEvent::Navigate(Page::Speakers)).is_ok());
            wait_for(&handle, 1);
            assert!(handle.dispatch(AppEvent::MainWindowCloseRequested).is_ok());
            wait_for(&handle, 2);

            assert_eq!(handle.drain_ui_effects(), vec![UiEffect::HideMainWindow]);
            assert_eq!(handle.snapshot().page, Page::Speakers);
        }

        #[test]
        fn tray_show_produces_show_focus_window_effects_preserving_page() {
            let (handle, _c, _p, _s, _join) = make_executor();
            assert!(handle.dispatch(AppEvent::Navigate(Page::Audio)).is_ok());
            wait_for(&handle, 1);
            assert!(handle.dispatch(AppEvent::ShowMainWindow).is_ok());
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let effects = handle.drain_ui_effects();
                if !effects.is_empty() {
                    assert_eq!(effects, vec![UiEffect::ShowMainWindow]);
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "show effect never arrived"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(handle.snapshot().page, Page::Audio);
        }

        #[test]
        fn the_shutdown_valve_is_the_one_the_executor_was_built_with() {
            // The production valve ends the process, and it is deliberately
            // never disarmed. A test that reaches this path with the
            // production valve installed does not fail -- it kills the whole
            // test binary once the budget expires, mid-run, with exit code 0
            // and no `test result` line. Every test scheduled after that
            // point silently never runs.
            //
            // So the valve has to be chosen where the executor is built,
            // where a test can see it and put something harmless in its
            // place. This asserts the arming, not the firing: waiting out a
            // six-second budget would tie the test to the wall clock.
            let (valve, armed) = ExitValvePolicy::recording();

            let executor = ServiceExecutor {
                controller: Arc::new(FakeController::default()),
                preferences: Arc::new(FakePrefs::default()),
                hotkeys: Arc::new(FakeHotkeys::default()),
                shutdown: Arc::new(FakeShutdown::default()),
                budgets: ShutdownBudgets {
                    device: Duration::from_millis(20),
                    hotkey: Duration::from_millis(10),
                },
                exit_valve: valve,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            };

            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
            let (handle, _runtime) =
                start_app_with_channel(AppState::default(), Box::new(executor), event_tx, event_rx);
            std::mem::forget(_runtime);

            assert!(handle.dispatch(AppEvent::QuitRequested).is_ok());

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if !armed.lock().unwrap().is_empty() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the shutdown coordinator never armed the executor's valve"
                );
                std::thread::sleep(Duration::from_millis(5));
            }

            let budgets = armed.lock().unwrap().clone();
            assert_eq!(
                budgets,
                vec![
                    Duration::from_millis(30)
                        + super::super::PREFERENCE_FLUSH_BUDGET
                        + super::super::VIEWPORT_TEARDOWN_SLACK
                ],
                "the valve must be armed once, for the budgets actually in force"
            );
        }

        #[test]
        fn explicit_quit_runs_persistence_stop_hide_then_bounded_cleanup() {
            let initial = AppState {
                stream: crate::app::StreamState::Streaming {
                    generation: GenerationId(4),
                },
                generations: crate::app::Generations {
                    session: GenerationId(4),
                    ..Default::default()
                },
                desired_receivers: std::collections::HashSet::from_iter([airplay_core::DeviceId(
                    [1u8, 0, 0, 0, 0, 0],
                )]),
                ..AppState::default()
            };

            let controller = FakeController::default();
            let prefs = FakePrefs::default();
            let shutdown = FakeShutdown::default();

            let executor = ServiceExecutor {
                controller: Arc::new(controller.clone()),
                preferences: Arc::new(prefs.clone()),
                hotkeys: Arc::new(FakeHotkeys::default()),
                shutdown: Arc::new(shutdown.clone()),
                budgets: ShutdownBudgets {
                    device: Duration::from_secs(1),
                    hotkey: Duration::from_millis(100),
                },
                exit_valve: ExitValvePolicy::recording().0,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            };

            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
            let (handle, _runtime) =
                start_app_with_channel(initial, Box::new(executor), event_tx, event_rx);
            std::mem::forget(_runtime);

            assert!(handle.dispatch(AppEvent::QuitRequested).is_ok());
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !shutdown.completed.load(std::sync::atomic::Ordering::SeqCst) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "bounded cleanup never finished"
                );
                std::thread::sleep(Duration::from_millis(5));
            }

            let effects = handle.drain_ui_effects();
            assert!(
                effects.contains(&UiEffect::HideMainWindow),
                "quit must hide first, got {effects:?}"
            );
            assert!(
                effects.contains(&UiEffect::CloseApplication),
                "quit must end with CloseApplication, got {effects:?}"
            );

            let order = shutdown.order.lock().unwrap().clone();
            let device_start = order.iter().position(|step| *step == "device:start");
            let device_end = order.iter().position(|step| *step == "device:end");
            let hotkey_pos = order.iter().position(|step| *step == "hotkey");
            let prefs_pos = order.iter().position(|step| *step == "prefs");
            assert!(
                device_start < device_end && device_end < hotkey_pos && hotkey_pos < prefs_pos,
                "cleanup order wrong: {order:?}"
            );

            assert!(controller
                .0
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|command| matches!(command, DeviceCommand::StopSession { .. })));
            assert!(
                !prefs.0.lock().unwrap().saves.is_empty(),
                "quit must persist preferences"
            );
        }

        #[test]
        fn controller_channel_loss_publishes_needs_attention_but_quit_still_works() {
            let initial = AppState {
                receivers: vec![crate::app::ReceiverState {
                    id: airplay_core::DeviceId([1, 0, 0, 0, 0, 0]),
                    name: "Kitchen".into(),
                    model: "HomePod".into(),
                    availability: crate::app::Availability::Available,
                }],
                desired_receivers: std::collections::HashSet::from_iter([airplay_core::DeviceId(
                    [1, 0, 0, 0, 0, 0],
                )]),
                staged_receivers: std::collections::HashSet::from_iter([airplay_core::DeviceId([
                    1, 0, 0, 0, 0, 0,
                ])]),
                ..AppState::default()
            };

            let controller = FakeController::default();
            controller.0.lock().unwrap().closed = true;

            let executor = ServiceExecutor {
                controller: Arc::new(controller.clone()),
                preferences: Arc::new(FakePrefs::default()),
                hotkeys: Arc::new(FakeHotkeys::default()),
                shutdown: Arc::new(FakeShutdown::default()),
                budgets: ShutdownBudgets::default(),
                exit_valve: ExitValvePolicy::recording().0,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            };

            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
            let (handle, _runtime) =
                start_app_with_channel(initial, Box::new(executor), event_tx, event_rx);
            std::mem::forget(_runtime);

            assert!(handle.dispatch(AppEvent::StartRequested).is_ok());
            wait_for(&handle, 1);
            assert!(matches!(
                handle.snapshot().notice,
                Some(crate::app::UserNotice {
                    code: crate::app::NoticeCode::ControllerUnavailable,
                    ..
                })
            ));

            assert!(handle.dispatch(AppEvent::QuitRequested).is_ok());
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let final_snapshot = handle.snapshot();
                if final_snapshot.shutting_down {
                    assert!(
                        matches!(
                            final_snapshot.notice,
                            Some(crate::app::UserNotice {
                                code: crate::app::NoticeCode::ControllerUnavailable,
                                ..
                            })
                        ),
                        "needs attention must survive quit"
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "quit never completed: rev={} stream={:?}",
                    final_snapshot.revision,
                    final_snapshot.stream
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn cleanup_deadline_still_queues_close_application_when_a_port_never_finishes() {
            let controller = FakeController::default();
            let prefs = FakePrefs::default();
            let stuck_until = std::time::Instant::now() + Duration::from_secs(30);
            let shutdown = FakeShutdown {
                order: Arc::default(),
                block_device_until: Some(stuck_until),
                completed: Arc::default(),
            };

            let executor = ServiceExecutor {
                controller: Arc::new(controller),
                preferences: Arc::new(prefs),
                hotkeys: Arc::new(FakeHotkeys::default()),
                shutdown: Arc::new(shutdown.clone()),
                budgets: ShutdownBudgets {
                    device: Duration::from_millis(150),
                    hotkey: Duration::from_millis(50),
                },
                exit_valve: ExitValvePolicy::recording().0,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            };

            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
            let (handle, _runtime) =
                start_app_with_channel(AppState::default(), Box::new(executor), event_tx, event_rx);
            std::mem::forget(_runtime);

            assert!(handle.dispatch(AppEvent::QuitRequested).is_ok());
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                let effects = handle.drain_ui_effects();
                if effects.contains(&UiEffect::CloseApplication) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "CloseApplication must arrive despite a stuck port"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn startup_state_honours_every_loaded_preference_including_launch_at_startup() {
            let loaded = crate::preferences::PreferencesLoad {
                value: Preferences {
                    schema_version: crate::app::PREFERENCES_SCHEMA_VERSION,
                    hotkey: HotkeyBinding {
                        enabled: false,
                        modifiers: 5,
                        virtual_key: 0x50,
                    },
                    theme: crate::app::ThemePreference::Dark,
                    locale: crate::app::LocalePreference::System,
                    advanced_information: true,
                    window: crate::app::WindowGeometry {
                        x: Some(20.0),
                        y: Some(-8.0),
                        width: 1024.0,
                        height: 768.0,
                        maximized: true,
                    },
                    last_page: Page::Diagnostics,
                    close_to_tray: false,
                    launch_at_startup: true,
                },
                notice: None,
            };

            let state = super::super::initial_state(loaded, ResolvedLocale::German);

            assert!(
                state.window.launch_at_startup,
                "a stored launch-at-startup choice must survive a restart"
            );
            assert_eq!(state.page, Page::Diagnostics);
            assert_eq!(state.theme, crate::app::ThemePreference::Dark);
            assert!(!state.window.close_to_tray);
            assert!(state.advanced_information);
            assert_eq!(
                state.locale_preference,
                crate::app::LocalePreference::System
            );
            assert_eq!(state.windows_display_locale, ResolvedLocale::German);
            assert_eq!(
                state.resolved_locale,
                ResolvedLocale::German,
                "System resolves through the language Windows was read as at startup"
            );
            assert_eq!(state.hotkey.virtual_key, 0x50);
            assert!(state.window.visible);
            assert_eq!(state.notice, None);
        }

        #[test]
        fn startup_state_clamps_an_out_of_range_stored_geometry() {
            let loaded = crate::preferences::PreferencesLoad {
                value: Preferences {
                    window: crate::app::WindowGeometry {
                        x: Some(0.0),
                        y: Some(0.0),
                        width: f32::NAN,
                        height: 400.0,
                        maximized: false,
                    },
                    ..Preferences::default()
                },
                notice: None,
            };

            let state = super::super::initial_state(loaded, ResolvedLocale::English);

            assert_eq!(state.window.geometry, crate::app::WindowGeometry::default());
        }

        #[test]
        fn startup_state_keeps_an_explicit_language_regardless_of_windows() {
            let loaded = crate::preferences::PreferencesLoad {
                value: Preferences {
                    locale: crate::app::LocalePreference::English,
                    ..Preferences::default()
                },
                notice: None,
            };

            let state = super::super::initial_state(loaded, ResolvedLocale::German);

            assert_eq!(state.resolved_locale, ResolvedLocale::English);
            assert_eq!(
                state.windows_display_locale,
                ResolvedLocale::German,
                "the OS reading is still cached for a later return to System"
            );
        }

        #[test]
        fn the_windows_locale_bridge_dispatches_the_change_as_an_app_event() {
            use crate::platform::WindowsSettingsEvents as _;

            let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(1);
            let bridge = super::super::WindowsLocaleBridge {
                events: crate::app_handle::AppEventSender::new(tx),
            };

            bridge.windows_display_language_changed(ResolvedLocale::German);

            assert_eq!(
                rx.try_recv().unwrap(),
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German)
            );
        }

        #[test]
        fn the_windows_locale_bridge_stays_bounded_when_the_actor_cannot_keep_up() {
            use crate::platform::WindowsSettingsEvents as _;

            let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(1);
            let bridge = super::super::WindowsLocaleBridge {
                events: crate::app_handle::AppEventSender::new(tx),
            };

            bridge.windows_display_language_changed(ResolvedLocale::German);
            bridge.windows_display_language_changed(ResolvedLocale::English);

            assert_eq!(
                rx.try_recv().unwrap(),
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German)
            );
            assert!(
                rx.try_recv().is_err(),
                "the dropped report must not have been queued or blocked on"
            );
        }
    }

    mod exit {
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{run_shutdown_coordinator, ExitWatchdog, ShutdownBudgets};
        use super::lifecycle::FakeShutdown;
        use crate::app::UiEffect;
        use crate::app_handle::{UiEffectSender, WakerRegistry};

        /// A UI-effect sink that keeps what the coordinator queued, without
        /// starting an actor: the coordinator is the subject here, and it
        /// only ever talks to the shell through this one queue.
        fn ui_effect_probe() -> (UiEffectSender, Arc<Mutex<VecDeque<UiEffect>>>) {
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            let sender = UiEffectSender::new(
                Arc::clone(&queue),
                Arc::new(AtomicBool::new(true)),
                Arc::new(WakerRegistry::default()),
            );
            (sender, queue)
        }

        fn queued(queue: &Arc<Mutex<VecDeque<UiEffect>>>) -> Vec<UiEffect> {
            queue.lock().unwrap().iter().cloned().collect()
        }

        /// The valve is the last resort, not a precondition for the exit.
        ///
        /// Arming it is a thread spawn, and a spawn can fail. It used to fail
        /// into an `expect` on the coordinator's first line, which killed the
        /// coordinator thread before it had stopped a single worker or queued
        /// `CloseApplication` -- and because the shutdown latch had already
        /// been set, a second Quit from the tray did nothing at all. Thread
        /// exhaustion is exactly the state in which the user wants out.
        #[test]
        fn a_coordinator_that_cannot_arm_its_valve_still_closes_the_window() {
            let shutdown = FakeShutdown::default();
            let (ui_effects, queue) = ui_effect_probe();

            run_shutdown_coordinator(
                Arc::new(shutdown.clone()),
                ShutdownBudgets::default(),
                ui_effects,
                |_budget| Err(std::io::Error::other("no thread available for the valve")),
            );

            assert_eq!(
                shutdown.order.lock().unwrap().as_slice(),
                ["device:start", "device:end", "hotkey", "prefs"],
                "every worker must still be stopped in order without a valve"
            );
            assert_eq!(
                queued(&queue),
                vec![UiEffect::DropTray, UiEffect::CloseApplication],
                "the window must still be told to close without a valve"
            );
        }

        /// The valve has to outlast the teardown it guards, whatever that
        /// teardown is budgeted at.
        ///
        /// The two numbers used to be unrelated literals: three plus one plus
        /// two seconds of coordinator against a ten-second constant nothing
        /// referred to. Raising one budget would have moved the coordinator's
        /// worst case past the valve, and the valve would then have fired in
        /// the middle of the preference flush -- settings lost, with the
        /// forced exit reporting nothing. Budgets far larger than the shipped
        /// ones are used here precisely so a re-hardcoded constant fails.
        #[test]
        fn the_coordinator_arms_its_valve_beyond_its_own_worst_case() {
            let budgets = ShutdownBudgets {
                device: Duration::from_secs(7),
                hotkey: Duration::from_secs(5),
            };
            let armed_for = Arc::new(Mutex::new(None));
            let recorder = Arc::clone(&armed_for);
            let (ui_effects, _queue) = ui_effect_probe();

            run_shutdown_coordinator(
                Arc::new(FakeShutdown::default()),
                budgets,
                ui_effects,
                move |budget| {
                    *recorder.lock().unwrap() = Some(budget);
                    Err(std::io::Error::other("the valve itself is not the subject"))
                },
            );

            let armed_for = armed_for
                .lock()
                .unwrap()
                .expect("the coordinator must arm a valve before it starts stopping workers");
            let worst_case = budgets.worst_case();
            assert!(
                armed_for > worst_case,
                "the valve was armed for {armed_for:?} but the teardown it guards may take {worst_case:?}"
            );
            assert_eq!(
                ShutdownBudgets::default().exit_valve(),
                Duration::from_secs(10),
                "the shipped valve budget changed; confirm the shell still fits inside it"
            );
        }

        /// The normal path: the shell finishes its bounded teardown and the
        /// valve never opens. This is the case that must hold on every quit,
        /// so it is asserted without any wall-clock dependency -- the budget
        /// is far longer than the test could ever take, and the assertion is
        /// made only after the watchdog thread has actually ended.
        #[test]
        fn a_disarmed_exit_watchdog_never_forces_the_process_down() {
            let forced = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&forced);
            let watchdog = ExitWatchdog::arm(Duration::from_secs(3_600), move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("the test host can spawn one thread");

            watchdog.disarm();
            watchdog.join();

            assert_eq!(
                forced.load(Ordering::SeqCst),
                0,
                "a shell that finished its teardown must not be killed"
            );
        }

        /// The last resort: once the budget is gone the valve opens exactly
        /// once. A zero budget makes this deterministic -- `wait_timeout`
        /// with no time left reports a timeout immediately -- so the test
        /// states the policy rather than racing a sleep.
        #[test]
        fn an_expired_exit_watchdog_forces_the_process_down_once() {
            let forced = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&forced);
            let watchdog = ExitWatchdog::arm(Duration::ZERO, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("the test host can spawn one thread");

            watchdog.join();

            assert_eq!(
                forced.load(Ordering::SeqCst),
                1,
                "an app wedged past its exit budget must be forced down, once"
            );
        }

        /// A disarm that arrives after the valve already opened must not open
        /// it a second time, and must not panic on a thread that is gone.
        #[test]
        fn disarming_after_the_valve_opened_changes_nothing() {
            let forced = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&forced);
            let watchdog = ExitWatchdog::arm(Duration::ZERO, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("the test host can spawn one thread");
            watchdog.wait_for_thread();

            watchdog.disarm();

            assert_eq!(forced.load(Ordering::SeqCst), 1);
        }

        /// The token this guard counts, composed so that naming it here does
        /// not make the guard count itself.
        ///
        /// The previous version cut this file at its first `cfg`-test marker
        /// and searched only what came before. That marker sits inside
        /// `ExitWatchdog`, hundreds of lines above `run()`, so the whole
        /// launch path -- the place the forced exit used to live -- was
        /// outside the searched region: a re-added kill there would have left
        /// the count untouched. Counting over the entire file removes both
        /// that blind spot and the false alarm a new test helper above the
        /// coordinator would have caused.
        const FORCED_EXIT: &str = concat!("process", "::exit");

        /// Structural guard over the shipping shell.
        ///
        /// Steps 1-4 of this plan task made the backend end its workers and
        /// join them; this asserts the other half -- that no file on the
        /// shipping exit path short-circuits that with a forced process death
        /// or a runtime shutdown deadline. The one permitted forced exit is
        /// the watchdog's valve above, which only ever runs after the whole
        /// bounded teardown has already failed to finish.
        #[test]
        fn the_shipping_shell_keeps_no_unconditional_forced_exit() {
            const SHELL_SOURCES: [(&str, &str); 3] = [
                ("main.rs", include_str!("../main.rs")),
                ("cast.rs", include_str!("../cast.rs")),
                ("device_service.rs", include_str!("../device_service.rs")),
            ];
            for (name, source) in SHELL_SOURCES {
                assert!(
                    !source.contains("shutdown_timeout"),
                    "{name} still ends a Tokio runtime on a deadline instead of letting it finish"
                );
                assert!(
                    !source.contains(FORCED_EXIT),
                    "{name} still terminates the process instead of returning"
                );
            }

            // The whole file, launch path included -- see `FORCED_EXIT`.
            let coordinator = include_str!("mod.rs");
            assert_eq!(
                coordinator.matches(FORCED_EXIT).count(),
                1,
                "the exit watchdog's valve is the only forced exit the shell may contain"
            );
        }
    }

    /// What the executor does when the settings queue will not take the write.
    ///
    /// A refused settings write is a settings failure. Reporting it as a lost
    /// audio controller -- which is what `ChannelClosed` means -- would put a
    /// "streaming is paused" notice on screen because a preferences thread was
    /// busy, and would leave the actual save silently unreported.
    mod persistence_reports {
        use std::collections::VecDeque;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{
            BoundedShutdown, ExitValvePolicy, HotkeySink, PreferenceSink, ServiceExecutor,
            ShutdownBudgets,
        };
        use crate::app::{
            AppEffect, AppEvent, ControllerPort, ControllerSendError, DeviceCommand, GenerationId,
            HotkeyBinding, Preferences, PreferencesEvent,
        };
        use crate::app_handle::{
            AppEventSender, AppFeedback, EffectExecutor, UiEffectSender, WakerRegistry,
        };
        use crate::preferences::PreferencesUnavailable;

        /// Fails the test loudly rather than silently recording: no preference
        /// outcome has any business reaching the audio controller.
        struct UnusedController;

        impl ControllerPort for UnusedController {
            fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError> {
                panic!("a preference write must not reach the audio controller: {command:?}");
            }
        }

        struct RefusingPreferences(PreferencesUnavailable);

        impl PreferenceSink for RefusingPreferences {
            fn try_schedule(
                &self,
                _generation: GenerationId,
                _value: Preferences,
            ) -> Result<(), PreferencesUnavailable> {
                Err(self.0)
            }
        }

        struct UnusedHotkeys;

        impl HotkeySink for UnusedHotkeys {
            fn try_reconfigure(
                &self,
                _generation: GenerationId,
                _binding: HotkeyBinding,
            ) -> Result<(), crate::platform::hotkey::HotkeyError> {
                Ok(())
            }
        }

        struct UnusedShutdown;

        impl BoundedShutdown for UnusedShutdown {
            fn shutdown_device(&self, _timeout: Duration) -> bool {
                true
            }

            fn shutdown_hotkey(&self, _timeout: Duration) -> bool {
                true
            }

            fn flush_preferences(&self) -> bool {
                true
            }
        }

        fn executor_refusing(unavailable: PreferencesUnavailable) -> ServiceExecutor {
            ServiceExecutor {
                controller: Arc::new(UnusedController),
                preferences: Arc::new(RefusingPreferences(unavailable)),
                hotkeys: Arc::new(UnusedHotkeys),
                shutdown: Arc::new(UnusedShutdown),
                budgets: ShutdownBudgets::default(),
                exit_valve: ExitValvePolicy::recording().0,
                begin_shutdown_started: std::sync::atomic::AtomicBool::new(false),
            }
        }

        /// The executor's feedback path without an actor behind it, so what it
        /// reported can be read back exactly.
        fn feedback_probe() -> (AppFeedback, std::sync::mpsc::Receiver<AppEvent>) {
            let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AppEvent>(8);
            let feedback = AppFeedback {
                events: AppEventSender::new(event_tx),
                ui_effects: UiEffectSender::new(
                    Arc::new(Mutex::new(VecDeque::new())),
                    Arc::new(AtomicBool::new(true)),
                    Arc::new(WakerRegistry::default()),
                ),
            };
            (feedback, event_rx)
        }

        #[test]
        fn a_settings_queue_that_refuses_the_write_reports_the_attempted_generation() {
            // Both refusals are collected before anything is asserted, so a
            // regression in either one is visible in the same failure rather
            // than hidden behind the other.
            let reported: Vec<(&str, Vec<AppEvent>)> = [
                ("busy", PreferencesUnavailable::Busy),
                ("closed", PreferencesUnavailable::Closed),
            ]
            .into_iter()
            .map(|(case, unavailable)| {
                let mut executor = executor_refusing(unavailable);
                let (feedback, events) = feedback_probe();

                executor.execute(
                    AppEffect::PersistPreferences {
                        generation: GenerationId(4),
                        value: Preferences::default(),
                    },
                    &feedback,
                );

                (case, events.try_iter().collect::<Vec<_>>())
            })
            .collect();

            let expected = vec![AppEvent::Preferences(PreferencesEvent::PersistFailed {
                generation: GenerationId(4),
            })];
            assert_eq!(
                reported,
                vec![("busy", expected.clone()), ("closed", expected)],
                "a refused settings write is reported as that write failing, \
                 never as a lost audio controller and never as silence"
            );
        }
    }
}
