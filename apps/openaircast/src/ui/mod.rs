//! Control Center application shell: owns the eframe integration, drains UI
//! effects, installs the snapshot waker, keeps the AccessKit live-status node
//! fresh, and routes the six destinations.

#[cfg(test)]
mod acceptance;
pub mod components;
pub mod i18n;
pub mod layout;
pub mod pages;
pub mod presentation;
pub mod theme;

use crate::app::{AppEvent, UiEffect, UiSnapshot};
use crate::app_handle::{AppHandle, SnapshotSubscription};
use crate::platform::{SystemAppearance, WindowsSettingsMonitor};

type ThemeCacheKey = (
    crate::app::ThemePreference,
    Option<egui::Theme>,
    SystemAppearance,
);

pub struct ControlCenterApp {
    handle: AppHandle,
    _subscription: SnapshotSubscription,
    /// The live Windows reading. Held for the whole app lifetime: dropping it
    /// removes the window subclass that keeps it fresh.
    windows_settings: WindowsSettingsMonitor,
    last_applied_theme: Option<ThemeCacheKey>,
    live_status: String,
    tray: Option<crate::tray::TrayController>,
    dispatched_initial_refresh: bool,
    /// Set once the shutdown coordinator has asked for the window to close.
    /// Until then every close request is cancelled and handed to the app.
    close_granted: bool,
    /// The last geometry handed to the app, so a window that is standing still
    /// dispatches nothing.
    last_reported_geometry: Option<crate::app::WindowGeometry>,
}

/// What the shell does with a close request from the window manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseResponse {
    /// Nothing was requested.
    Ignore,
    /// The bounded shutdown already ran; let the event loop end.
    Proceed,
    /// Closing means hiding: the app keeps running in the tray.
    HideToTray,
    /// Closing means quitting: hand it to the shutdown coordinator.
    BeginShutdown,
}

/// Decides what a close request means.
///
/// The X button used to end the eframe event loop directly. Nothing on that
/// path stopped the device service, so a running AirPlay session was dropped
/// rather than torn down, the settings were never flushed, and the exit valve
/// was never armed. Every close now goes through the app: it is cancelled
/// first, and only the `CloseApplication` the coordinator sends at the end of
/// its bounded teardown grants it.
pub(crate) fn respond_to_close_request(
    requested: bool,
    close_to_tray: bool,
    close_granted: bool,
) -> CloseResponse {
    match (requested, close_granted, close_to_tray) {
        (false, _, _) => CloseResponse::Ignore,
        // Never cancel the coordinator's own close: that would be a window
        // that can never be shut, with the shutdown latch already closed
        // against a second attempt.
        (true, true, _) => CloseResponse::Proceed,
        (true, false, true) => CloseResponse::HideToTray,
        (true, false, false) => CloseResponse::BeginShutdown,
    }
}

impl ControlCenterApp {
    /// Installs the snapshot-derived tray; called from the eframe creation
    /// closure so the icon lives on the event-loop thread.
    pub fn install_tray(&mut self, tray: crate::tray::TrayController) {
        self.tray = Some(tray);
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        handle: AppHandle,
        windows_settings: WindowsSettingsMonitor,
        segoe_ui_variable: Option<std::sync::Arc<[u8]>>,
    ) -> Self {
        let initial = handle.snapshot();
        let appearance = windows_settings.current().appearance;
        theme::install_fonts(&cc.egui_ctx, segoe_ui_variable);
        let system_theme = cc.egui_ctx.system_theme();
        let resolved = theme::resolve_theme(initial.theme, system_theme, appearance);
        theme::apply_theme(&cc.egui_ctx, &resolved);

        // The only repaint source besides real OS input events: one published
        // revision wakes exactly one frame.
        let waker_ctx = cc.egui_ctx.clone();
        let subscription =
            handle.install_waker(std::sync::Arc::new(move || waker_ctx.request_repaint()));

        Self {
            handle,
            _subscription: subscription,
            windows_settings,
            last_applied_theme: Some((initial.theme, system_theme, appearance)),
            live_status: String::new(),
            tray: None,
            dispatched_initial_refresh: false,
            close_granted: false,
            last_reported_geometry: None,
        }
    }

    /// Hands the current snapshot to the tray controller, if one is installed.
    ///
    /// The controller decides whether anything has to be rebuilt; this is the
    /// call that was missing entirely, which left the icon with the empty
    /// menu it was constructed with.
    fn sync_tray(&mut self) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        tray.sync(&self.handle.snapshot());
    }

    /// Answers a pending close request; see [`respond_to_close_request`].
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|input| input.viewport().close_requested());
        let close_to_tray = self.handle.snapshot().window.close_to_tray;
        let event = match respond_to_close_request(requested, close_to_tray, self.close_granted) {
            CloseResponse::Ignore | CloseResponse::Proceed => return,
            CloseResponse::HideToTray => AppEvent::MainWindowCloseRequested,
            CloseResponse::BeginShutdown => AppEvent::QuitRequested,
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        let _ = self.handle.dispatch(event);
    }

    /// Tells the app how large the window actually is, once per size it
    /// reaches.
    ///
    /// This was the missing half of a feature that was otherwise complete.
    /// `AppEvent::WindowGeometryChanged` had a reducer arm, the reducer
    /// persisted through it, and the settings worker carries a debounce rule
    /// written specifically for geometry churn -- and nothing in the whole
    /// binary ever sent the event. The window was therefore reopened at the
    /// built-in size no matter what size it was last left at, and the
    /// persisted value could only ever be the default that was written once.
    ///
    /// Gated on the value and not on the frame: the guard is what keeps a
    /// drag-resize from queueing one event per frame, and the reducer's own
    /// equality check is the second line of defence rather than the first.
    fn report_window_geometry(&mut self, ctx: &egui::Context) {
        let Some(geometry) = observed_geometry(ctx) else {
            return;
        };
        // Validated here as well as in the reducer, so what is compared
        // against the previous report is the same clamped value the app would
        // store -- otherwise a below-minimum reading would be dispatched over
        // and over, each time clamping to a state that never matches it.
        let Some(geometry) = geometry.validate() else {
            return;
        };
        if self.last_reported_geometry == Some(geometry) {
            return;
        }
        // Recorded only once the event is actually on the queue. A dispatch
        // that failed on a full queue and was still recorded would make this
        // size the one the shell believes it has already reported, and the
        // next frame -- which sees no further change -- would never send it.
        if self
            .handle
            .dispatch(AppEvent::WindowGeometryChanged(geometry))
            .is_ok()
        {
            self.last_reported_geometry = Some(geometry);
        }
    }

    fn consume_window_effects(&mut self, ctx: &egui::Context) {
        for effect in self.handle.drain_ui_effects() {
            match effect {
                UiEffect::ShowMainWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                UiEffect::HideMainWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                UiEffect::FocusMainWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                UiEffect::CloseApplication => {
                    // The bounded teardown is over; this is the one close the
                    // shell must not cancel.
                    self.close_granted = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                UiEffect::Announce(message) => {
                    if message != self.live_status {
                        self.live_status = message.clone();
                        publish_live_status_node(ctx, &message);
                    }
                }
                UiEffect::DropTray => {
                    // Dropping the controller drops the icon with it, so the
                    // shell does not leave a dead tray entry behind.
                    self.tray = None;
                }
            }
        }
    }

    /// Reapplies the style only when the cache key moved.
    ///
    /// The appearance comes from the monitor's published reading, not from a
    /// Win32 call: this is an atomic load of a value that only a
    /// `WM_SETTINGCHANGE` or `WM_THEMECHANGED` can have replaced, so High
    /// Contrast and Reduced Motion take effect on the very next frame without
    /// a timer and without a per-frame system query.
    /// Returns the style in force for this frame, so the shell resolves the
    /// theme exactly once and every renderer below it reads the same tokens.
    fn sync_theme(
        &mut self,
        ctx: &egui::Context,
        snapshot: &UiSnapshot,
    ) -> theme::ResolvedThemeStyle {
        let appearance = self.windows_settings.current().appearance;
        let key = (snapshot.theme, ctx.system_theme(), appearance);
        let resolved = theme::resolve_theme(snapshot.theme, key.1, appearance);
        if self
            .last_applied_theme
            .is_none_or(|previous| theme_reapply_needed(previous, key))
        {
            theme::apply_theme(ctx, &resolved);
            self.last_applied_theme = Some(key);
        }
        resolved
    }
}

fn theme_reapply_needed(previous: ThemeCacheKey, next: ThemeCacheKey) -> bool {
    previous != next
}

/// The window's real geometry, or `None` while the platform has reported none.
///
/// `maximized` is read separately from the rectangle on purpose: a maximized
/// window still has an inner rectangle, and storing the rectangle without the
/// flag would restore a maximized window as a merely very large one.
fn observed_geometry(ctx: &egui::Context) -> Option<crate::app::WindowGeometry> {
    ctx.input(|input| {
        let viewport = input.viewport();
        let rect = viewport.inner_rect?;
        Some(crate::app::WindowGeometry {
            x: Some(rect.min.x),
            y: Some(rect.min.y),
            width: rect.width(),
            height: rect.height(),
            maximized: viewport.maximized.unwrap_or(false),
        })
    })
}

fn current_live_status(snapshot: &UiSnapshot) -> String {
    let catalog = i18n::Catalog::new(snapshot.resolved_locale);
    // The pre-redacted backend summary never reaches the live region: the
    // notice code resolves to catalog copy, exactly as it does on the page.
    match &snapshot.notice {
        Some(notice) => catalog
            .text(presentation::message_key(notice.code))
            .to_owned(),
        None => catalog.text(pages::status_key(snapshot)).to_owned(),
    }
}

fn publish_live_status_node(ctx: &egui::Context, label: &str) {
    let label = label.to_owned();
    ctx.accesskit_node_builder(egui::Id::new("openaircast-live-status"), move |node| {
        node.set_role(accesskit::Role::Status);
        node.set_live(accesskit::Live::Polite);
        node.set_label(label);
    });
}

impl eframe::App for ControlCenterApp {
    /// Runs before every frame -- and, unlike [`Self::ui`], also while the
    /// window is hidden in the tray. Window effects and close requests are
    /// handled here for exactly that reason: a Quit chosen from the tray of a
    /// hidden window would otherwise queue a `CloseApplication` that no frame
    /// ever consumes, leaving a process with no window and no way out.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.consume_window_effects(ctx);
        self.handle_close_request(ctx);
        // Here rather than in `ui` for the same reason as the tray: a resize
        // that is followed by a hide to the tray still has to be recorded, and
        // `ui` does not run once the window is hidden.
        self.report_window_geometry(ctx);
        // Here rather than in `ui`: the tray is the only surface a hidden
        // window still has, and `ui` does not run while it is hidden. The
        // controller gates on the revision, so a frame that changed nothing
        // costs one integer comparison.
        self.sync_tray();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let snapshot = self.handle.snapshot();
        let style = self.sync_theme(&ctx, &snapshot);
        let status = current_live_status(&snapshot);
        if status != self.live_status {
            self.live_status = status.clone();
            publish_live_status_node(&ctx, &status);
        }

        let handle = self.handle.clone();
        let group_ctx = ctx.clone();
        let mut discard_refused = None;
        let mut emit = move |event: AppEvent| {
            let group_request = matches!(event, AppEvent::GroupRequested(_));
            if group_request {
                if let Some(error) = discard_refused.take() {
                    pages::groups::admission_failed(&group_ctx, error);
                    return;
                }
            }
            let discard = matches!(event, AppEvent::DiscardStagedReceivers);
            if let Err(error) = handle.dispatch(event) {
                if discard {
                    discard_refused = Some(error);
                }
                if group_request {
                    pages::groups::admission_failed(&group_ctx, error);
                }
            }
        };

        if !self.dispatched_initial_refresh {
            self.dispatched_initial_refresh = true;
            let _ = self.handle.dispatch(AppEvent::RefreshRequested);
        }

        // One snapshot, one resolved style, one catalog, one derived chrome,
        // one shell call. Nothing below this line sees the handle.
        let resources = layout::UiResources {
            tokens: &style.tokens,
            catalog: i18n::Catalog::new(snapshot.resolved_locale),
        };
        let change_bar = components::app_shell::change_bar_model(&snapshot, resources.catalog);
        components::app_shell::show_shell(
            ui,
            &snapshot,
            &resources,
            change_bar.as_ref(),
            &mut emit,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::{AppEffect, AppState, WindowState};
    use crate::app_handle::{AppFeedback, EffectExecutor};
    use crate::platform::{SystemColors, WindowsSettingsSnapshot};

    const APPEARANCE: SystemAppearance = SystemAppearance {
        client_animation_enabled: true,
        high_contrast: false,
        colors: SystemColors {
            background: [1, 2, 3],
            foreground: [4, 5, 6],
            highlight: [7, 8, 9],
            highlight_text: [10, 11, 12],
            disabled_text: [13, 14, 15],
            link: [16, 17, 18],
        },
    };

    /// What Windows publishes after the user turns on a High Contrast theme
    /// and switches animations off.
    const CONTRAST_APPEARANCE: SystemAppearance = SystemAppearance {
        client_animation_enabled: false,
        high_contrast: true,
        colors: SystemColors {
            background: [20, 21, 22],
            foreground: [23, 24, 25],
            highlight: [26, 27, 28],
            highlight_text: [29, 30, 31],
            disabled_text: [32, 33, 34],
            link: [35, 36, 37],
        },
    };

    const SETTINGS: WindowsSettingsSnapshot = WindowsSettingsSnapshot {
        locale: crate::app::ResolvedLocale::English,
        appearance: APPEARANCE,
    };

    /// Stands in for the production effect executor, and for the shutdown
    /// coordinator it starts: `BeginShutdown` answers with the
    /// `CloseApplication` the real coordinator queues once its bounded
    /// teardown is done, so the whole round trip can be driven here.
    #[derive(Clone, Default)]
    struct RecordingExecutor(Arc<Mutex<Vec<AppEffect>>>);

    impl EffectExecutor for RecordingExecutor {
        fn execute(&mut self, effect: AppEffect, feedback: &AppFeedback) {
            self.0.lock().unwrap().push(effect.clone());
            match effect {
                AppEffect::ShowMainWindow => feedback.ui_effects.send(UiEffect::ShowMainWindow),
                AppEffect::HideMainWindow => feedback.ui_effects.send(UiEffect::HideMainWindow),
                AppEffect::BeginShutdown => {
                    feedback.ui_effects.send(UiEffect::DropTray);
                    feedback.ui_effects.send(UiEffect::CloseApplication);
                }
                _ => {}
            }
        }
    }

    /// The actor runs on its own thread, so an effect appears some time after
    /// the frame that dispatched it. The deadline is a failure bound, not an
    /// expected duration: nothing here is asserted about how long it takes.
    fn wait_for(effects: &Arc<Mutex<Vec<AppEffect>>>, wanted: &AppEffect, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if effects.lock().unwrap().contains(wanted) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what} never reached the effect executor: {effects:?}");
    }

    fn request_window_close(harness: &mut egui_kittest::Harness<'_, ControlCenterApp>) {
        harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
    }

    fn close_was_cancelled(harness: &egui_kittest::Harness<'_, ControlCenterApp>) -> bool {
        harness
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| {
                viewport
                    .commands
                    .contains(&egui::ViewportCommand::CancelClose)
            })
    }

    /// One menu exactly as the surface received it: tooltip and rows.
    type RecordedMenu = (String, Vec<crate::tray::TrayEntry>);

    /// A tray surface with no Windows behind it: it records every menu the
    /// controller hands it, so a test can state what the user's tray would
    /// actually show.
    #[derive(Clone, Default)]
    struct RecordingTray(Arc<Mutex<Vec<RecordedMenu>>>);

    impl crate::tray::TraySurface for RecordingTray {
        fn apply(&mut self, tooltip: &str, entries: &[crate::tray::TrayEntry]) {
            self.0
                .lock()
                .unwrap()
                .push((tooltip.to_owned(), entries.to_vec()));
        }
    }

    /// Whether the recorded menu's checkable receiver row is usable, or
    /// `None` if it has no such row at all.
    fn receiver_row(entry: &RecordedMenu) -> Option<bool> {
        entry.1.iter().find_map(|row| match row {
            crate::tray::TrayEntry::Command {
                checked: Some(_),
                enabled,
                ..
            } => Some(*enabled),
            _ => None,
        })
    }

    /// The labels of the rows the last recorded menu carries.
    fn menu_labels(entry: &RecordedMenu) -> Vec<String> {
        entry
            .1
            .iter()
            .filter_map(|row| match row {
                crate::tray::TrayEntry::Separator => None,
                crate::tray::TrayEntry::Command { label, .. } => Some(label.clone()),
            })
            .collect()
    }

    /// How many frames a shell test will pump while another thread catches
    /// up before it calls the shell broken.
    const MAX_FRAMES: usize = 5_000;

    /// Steps the shell until `done` holds, or fails after [`MAX_FRAMES`].
    ///
    /// The bound is a frame count and not a duration, and that is the whole
    /// point of it. The actor and the shutdown coordinator run on their own
    /// threads, so what this waits for is those threads being scheduled --
    /// and a wall-clock deadline tightens precisely when they are slowest to
    /// be scheduled, which is how a busy machine turns a correct shell into
    /// a red test. A frame budget does not tighten under load: the loaded
    /// machine runs the same frames, each of them yielding, and merely takes
    /// longer over them. Nothing here asserts how long anything takes.
    ///
    /// Nothing that is not genuinely asynchronous belongs in here. The first
    /// menu is stepped exactly once at the call site instead: a controller
    /// that has never seen a revision has to be handed one by the very first
    /// frame, and there is nothing there to wait for.
    fn step_until<'a>(
        harness: &mut egui_kittest::Harness<'a, ControlCenterApp>,
        what: &str,
        mut done: impl FnMut(&egui_kittest::Harness<'a, ControlCenterApp>) -> bool,
    ) {
        for _ in 0..MAX_FRAMES {
            if done(harness) {
                return;
            }
            harness.step();
            std::thread::yield_now();
        }
        panic!("{what} never happened in {MAX_FRAMES} frames");
    }

    struct ShellUnderTest {
        harness: egui_kittest::Harness<'static, ControlCenterApp>,
        effects: Arc<Mutex<Vec<AppEffect>>>,
        _runtime: crate::app_handle::AppRuntime,
    }

    /// Builds the real `ControlCenterApp` on the real `eframe::App` hooks, so
    /// the close path is exercised through the same calls the event loop
    /// makes rather than through a copy of its logic.
    fn shell_with_close_to_tray(close_to_tray: bool) -> ShellUnderTest {
        shell_with_state(AppState {
            window: WindowState {
                close_to_tray,
                ..WindowState::default()
            },
            ..AppState::default()
        })
    }

    fn shell_with_state(initial: AppState) -> ShellUnderTest {
        let effects = Arc::new(Mutex::new(Vec::new()));
        let executor = RecordingExecutor(Arc::clone(&effects));
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel(256);
        let (handle, runtime) = crate::app_handle::start_app_with_channel(
            initial,
            Box::new(executor),
            event_tx,
            event_rx,
        );
        let harness = egui_kittest::Harness::builder().build_eframe(move |cc| {
            ControlCenterApp::new(
                cc,
                handle.clone(),
                WindowsSettingsMonitor::detached(SETTINGS),
                None,
            )
        });
        ShellUnderTest {
            harness,
            effects,
            _runtime: runtime,
        }
    }

    /// The device the tray fixture discovers. A MAC-derived id, like the
    /// real one, so nothing downstream can depend on a placeholder shape.
    fn receiver_id() -> airplay_core::DeviceId {
        airplay_core::DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, 0x01])
    }

    /// A shell whose snapshot already carries a selected, available receiver,
    /// so the tray menu under test has a receiver row to show.
    fn shell_with_receivers() -> ShellUnderTest {
        let mut state = AppState {
            window: WindowState {
                close_to_tray: true,
                ..WindowState::default()
            },
            receivers: vec![crate::app::ReceiverState {
                id: receiver_id(),
                name: "Kitchen".into(),
                model: "AudioAccessory5,1".into(),
                availability: crate::app::Availability::Available,
            }],
            ..AppState::default()
        };
        state.desired_receivers.insert(receiver_id());
        state.staged_receivers.insert(receiver_id());
        shell_with_state(state)
    }

    /// The window's X must not be a second, unguarded exit.
    ///
    /// It used to end the event loop directly: the device service was never
    /// asked to stop, so a live AirPlay session was dropped instead of torn
    /// down, the settings were never flushed, and the exit valve was never
    /// armed. The close is now cancelled and handed to the coordinator, and
    /// only the coordinator's own `CloseApplication` lets it through -- the
    /// second half matters just as much, since a shell that cancelled that
    /// one too would be a window that can never be closed at all.
    #[test]
    fn closing_the_window_quits_through_the_bounded_shutdown() {
        let mut shell = shell_with_close_to_tray(false);
        shell.harness.step();

        request_window_close(&mut shell.harness);
        shell.harness.step();

        assert!(
            close_was_cancelled(&shell.harness),
            "the close must be held back until the bounded teardown is done"
        );
        wait_for(&shell.effects, &AppEffect::BeginShutdown, "BeginShutdown");

        // The coordinator has finished and queued its close; the shell now
        // has to let the next request through.
        shell.harness.step();
        request_window_close(&mut shell.harness);
        shell.harness.step();
        assert!(
            !close_was_cancelled(&shell.harness),
            "the shutdown coordinator's own close must not be cancelled"
        );
    }

    /// With close-to-tray on, the same X means hide, not quit -- and it must
    /// still not reach the event loop.
    #[test]
    fn closing_the_window_with_close_to_tray_hides_it_without_shutting_down() {
        let mut shell = shell_with_close_to_tray(true);
        shell.harness.step();

        request_window_close(&mut shell.harness);
        shell.harness.step();

        assert!(
            close_was_cancelled(&shell.harness),
            "closing to the tray must not close the window"
        );
        wait_for(&shell.effects, &AppEffect::HideMainWindow, "HideMainWindow");
        assert!(
            !shell
                .effects
                .lock()
                .unwrap()
                .contains(&AppEffect::BeginShutdown),
            "closing to the tray must not start a shutdown"
        );
    }

    #[test]
    fn a_close_request_is_answered_once_per_meaning() {
        assert_eq!(
            respond_to_close_request(false, true, false),
            CloseResponse::Ignore
        );
        assert_eq!(
            respond_to_close_request(true, true, false),
            CloseResponse::HideToTray
        );
        assert_eq!(
            respond_to_close_request(true, false, false),
            CloseResponse::BeginShutdown
        );
        assert_eq!(
            respond_to_close_request(true, false, true),
            CloseResponse::Proceed,
            "the coordinator's own close ends the loop"
        );
        assert_eq!(
            respond_to_close_request(true, true, true),
            CloseResponse::Proceed,
            "close-to-tray must not outrank a granted shutdown"
        );
    }

    #[test]
    fn theme_cache_reuses_the_startup_appearance_snapshot_between_frames() {
        let previous = (crate::app::ThemePreference::System, None, APPEARANCE);

        assert!(!theme_reapply_needed(previous, previous));
        assert!(theme_reapply_needed(
            previous,
            (crate::app::ThemePreference::Dark, None, APPEARANCE)
        ));
        assert!(
            theme_reapply_needed(
                previous,
                (
                    crate::app::ThemePreference::System,
                    None,
                    CONTRAST_APPEARANCE
                )
            ),
            "an appearance the OS changed under us has to invalidate the cache"
        );
    }

    /// The shell must not freeze the startup reading. A Windows message
    /// republishes the monitor's snapshot, and the very next frame has to
    /// reapply the style from it -- no restart, no timer, no per-frame Win32
    /// call.
    #[test]
    fn the_theme_follows_the_monitor_reading_on_the_next_frame() {
        let mut shell = shell_with_close_to_tray(false);
        shell.harness.step();
        assert_eq!(
            shell
                .harness
                .state()
                .last_applied_theme
                .expect("the shell applies a style at construction")
                .2,
            APPEARANCE
        );

        shell
            .harness
            .state()
            .windows_settings
            .publish(WindowsSettingsSnapshot {
                appearance: CONTRAST_APPEARANCE,
                ..SETTINGS
            });
        shell.harness.step();

        assert_eq!(
            shell
                .harness
                .state()
                .last_applied_theme
                .expect("a style stays applied")
                .2,
            CONTRAST_APPEARANCE,
            "High Contrast and Reduced Motion have to reach the style live"
        );
    }

    /// The tray menu has to be driven by the shell, not merely constructed.
    ///
    /// The controller was created in the launch path, handed to the shell,
    /// and then never asked for anything: `install_tray` stored it in a field
    /// nothing read, so the icon kept the empty `Menu::new()` it was built
    /// with and a right click showed nothing at all. `menu_items` was fully
    /// tested throughout -- as a model no surface ever received.
    ///
    /// One frame, not a wait: a controller that has never seen a revision
    /// has to be handed a menu by the very first `logic`, and no amount of
    /// load on the machine can change that.
    #[test]
    fn the_shell_hands_the_tray_a_menu_built_from_the_published_snapshot() {
        let mut shell = shell_with_receivers();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        shell
            .harness
            .state_mut()
            .install_tray(crate::tray::TrayController::with_surface(Box::new(
                RecordingTray(Arc::clone(&recorded)),
            )));

        shell.harness.step();

        let first = recorded
            .lock()
            .unwrap()
            .first()
            .expect("the first frame drove no tray menu at all")
            .clone();
        assert!(
            !first.1.is_empty(),
            "the tray was handed an empty menu: {first:?}"
        );
        let labels = menu_labels(&first);
        assert!(
            labels.iter().any(|label| label == "Open") && labels.iter().any(|l| l == "Quit"),
            "the menu is missing its fixed commands: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label.starts_with("Kitchen")),
            "the menu does not carry the snapshot's receivers: {labels:?}"
        );
        assert!(
            !first.0.is_empty(),
            "the tooltip was never set: {:?}",
            first.0
        );
    }

    /// A revision the user caused has to reach the menu.
    ///
    /// Stated as "some menu the shell handed over says so" rather than as
    /// "the last one does": the actor publishes on its own thread and a
    /// discovery pass can publish behind the edit, so reading only the newest
    /// recording would make the assertion depend on machine timing rather
    /// than on the shell.
    #[test]
    fn a_revision_the_user_caused_reaches_the_menu() {
        let mut shell = shell_with_receivers();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        shell
            .harness
            .state_mut()
            .install_tray(crate::tray::TrayController::with_surface(Box::new(
                RecordingTray(Arc::clone(&recorded)),
            )));
        shell.harness.step();
        assert!(
            !recorded.lock().unwrap().is_empty(),
            "the first frame drove no tray menu at all"
        );
        assert!(
            recorded
                .lock()
                .unwrap()
                .iter()
                .all(|menu| receiver_row(menu).is_some_and(|enabled| enabled)),
            "the receiver row started out inert"
        );

        // Staging a membership change makes the tray rows inert: the tray
        // must not commit an edit the window has not applied.
        let _ = shell
            .harness
            .state()
            .handle
            .dispatch(AppEvent::ToggleStagedReceiver(receiver_id()));

        step_until(
            &mut shell.harness,
            "the staged edit reaching the menu",
            |_| {
                recorded
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|menu| receiver_row(menu) == Some(false))
            },
        );
    }

    /// The tray icon has to go when the coordinator says so.
    ///
    /// `UiEffect::DropTray` was consumed and discarded, so the icon outlived
    /// the bounded teardown that asked for it to be removed.
    #[test]
    fn the_shutdown_coordinators_drop_takes_the_tray_away() {
        let mut shell = shell_with_receivers();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        shell
            .harness
            .state_mut()
            .install_tray(crate::tray::TrayController::with_surface(Box::new(
                RecordingTray(Arc::clone(&recorded)),
            )));
        shell.harness.step();
        assert!(
            shell.harness.state().tray.is_some() && !recorded.lock().unwrap().is_empty(),
            "the tray was never there to be taken away"
        );

        let _ = shell
            .harness
            .state()
            .handle
            .dispatch(AppEvent::QuitRequested);
        // Clockless: the coordinator's teardown is bounded by its own logic,
        // not by how quickly this machine gets round to it.
        step_until(
            &mut shell.harness,
            "the tray going away with the teardown that asked for it",
            |harness| harness.state().tray.is_none(),
        );
    }
}
