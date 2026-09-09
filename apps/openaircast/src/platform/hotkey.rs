//! Global hotkey service on a dedicated Win32 thread.
//!
//! `RegisterHotKey`/`UnregisterHotKey` and the `GetMessageW` pump live entirely
//! on the service thread; the UI thread only posts configuration commands.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::{AppEvent, GenerationId, HotkeyBinding, PreferencesEvent};
use crate::app_handle::AppEventSender;

/// Wakes the service thread to drain queued configuration commands.
pub const WM_APP_CONFIGURE: u32 = 0x8001; // WM_APP + 1
/// Asks the service thread to unregister and exit.
pub const WM_APP_SHUTDOWN: u32 = 0x8002; // WM_APP + 2

const WM_HOTKEY: u32 = 0x0312;
const HOTKEY_ID: i32 = 1;
/// Capacity of the bounded command queue.
const COMMAND_CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HotkeyError {
    #[error("hotkey queue is busy")]
    Busy,
    #[error("hotkey service is closed")]
    Closed,
    #[error("could not spawn the hotkey thread")]
    Spawn,
}

/// Registration seam so the transactional state machine is testable without
/// touching machine-global hotkey state.
pub(crate) trait HotkeyRegistrar: Send + 'static {
    /// Registers an enabled chord; fails with a human-readable reason.
    fn register(&mut self, binding: &HotkeyBinding) -> Result<(), String>;
    fn unregister(&mut self);
}

struct RealRegistrar;

impl HotkeyRegistrar for RealRegistrar {
    #[cfg(windows)]
    fn register(&mut self, binding: &HotkeyBinding) -> Result<(), String> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::RegisterHotKey;

        let registered = unsafe {
            RegisterHotKey(
                std::ptr::null_mut(),
                HOTKEY_ID,
                binding.modifiers,
                binding.virtual_key,
            )
        };
        if registered == 0 {
            Err("the chord could not be registered".into())
        } else {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    fn register(&mut self, _binding: &HotkeyBinding) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn unregister(&mut self) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;

        unsafe {
            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
        }
    }

    #[cfg(not(windows))]
    fn unregister(&mut self) {}
}

pub(crate) enum Command {
    Configure {
        generation: GenerationId,
        binding: HotkeyBinding,
    },
    Shutdown,
}

/// Transactional application state over any registrar.
pub(crate) struct HotkeyEngine<R: HotkeyRegistrar> {
    registrar: R,
    applied: HotkeyBinding,
    registered: bool,
}

impl<R: HotkeyRegistrar> HotkeyEngine<R> {
    pub(crate) fn new(registrar: R, initial: HotkeyBinding) -> Self {
        let mut engine = Self {
            registrar,
            applied: initial,
            registered: false,
        };
        let (applied, _failure) = engine.apply(initial);
        engine.applied = applied;
        engine
    }

    /// The binding that is currently in force.
    #[allow(dead_code)] // read by engine tests; production reads the event payload.
    pub(crate) fn applied(&self) -> HotkeyBinding {
        self.applied
    }

    /// Applies a replacement transactionally: disable/unregister first, try
    /// the new chord, and on failure restore the previous chord — reporting
    /// the actually-applied disabled state when even the restore fails.
    pub(crate) fn apply(&mut self, requested: HotkeyBinding) -> (HotkeyBinding, Option<String>) {
        if !requested.enabled {
            self.registrar.unregister();
            self.registered = false;
            self.applied = requested;
            return (requested, None);
        }

        self.registrar.unregister();
        self.registered = false;
        match self.registrar.register(&requested) {
            Ok(()) => {
                self.registered = true;
                self.applied = requested;
                (requested, None)
            }
            Err(failure) => {
                if self.registrar.register(&self.applied).is_ok() {
                    self.registered = true;
                    (self.applied, Some(failure))
                } else {
                    let disabled = HotkeyBinding {
                        enabled: false,
                        ..self.applied
                    };
                    self.applied = disabled;
                    (
                        disabled,
                        Some(format!(
                            "{failure}; the previous chord could not be restored either"
                        )),
                    )
                }
            }
        }
    }

    /// Unregisters exactly once; safe to call again during loop teardown.
    pub(crate) fn shutdown(&mut self) {
        if self.registered {
            self.registrar.unregister();
            self.registered = false;
        }
        self.applied.enabled = false;
    }

    /// Handles one pump message. Returns `true` to leave the loop.
    pub(crate) fn process_message(
        &mut self,
        message: u32,
        wparam: usize,
        commands: &std::sync::mpsc::Receiver<Command>,
        events: &AppEventSender,
    ) -> bool {
        match message {
            WM_HOTKEY if wparam == HOTKEY_ID as usize => {
                let _ = events.try_send(AppEvent::GlobalHotkeyPressed);
                false
            }
            WM_APP_CONFIGURE => {
                while let Ok(command) = commands.try_recv() {
                    match command {
                        Command::Configure {
                            generation,
                            binding,
                        } => {
                            let (applied, failure) = self.apply(binding);
                            let _ = events.try_send(AppEvent::Preferences(
                                PreferencesEvent::HotkeyReconfigured {
                                    generation,
                                    applied,
                                    failure,
                                },
                            ));
                        }
                        Command::Shutdown => return true,
                    }
                }
                false
            }
            WM_APP_SHUTDOWN => true,
            _ => false,
        }
    }
}

/// Handle onto the dedicated registration thread.
#[derive(Clone)]
pub struct HotkeyServiceHandle {
    cmd_tx: SyncSender<Command>,
    thread_id: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
}

impl HotkeyServiceHandle {
    /// Queues a transactional chord replacement without blocking.
    pub fn try_reconfigure(
        &self,
        generation: GenerationId,
        binding: HotkeyBinding,
    ) -> Result<(), HotkeyError> {
        let posted = self.cmd_tx.try_send(Command::Configure {
            generation,
            binding,
        });
        match posted {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(HotkeyError::Busy),
            Err(TrySendError::Disconnected(_)) => return Err(HotkeyError::Closed),
        }
        #[cfg(windows)]
        self.wake_thread(WM_APP_CONFIGURE);
        Ok(())
    }

    /// Requests shutdown and waits up to `timeout`; reports whether the
    /// service unregistered and exited in time.
    pub fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.cmd_tx.try_send(Command::Shutdown) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => break,
                Err(TrySendError::Full(_)) => {
                    if Instant::now() >= deadline {
                        return self.done.load(Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        #[cfg(windows)]
        self.wake_thread(WM_APP_SHUTDOWN);

        while Instant::now() < deadline {
            if self.done.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.done.load(Ordering::SeqCst)
    }

    /// Win32 thread id of the service; zero until the thread stored it.
    pub fn thread_id(&self) -> u32 {
        self.thread_id.load(Ordering::SeqCst)
    }

    #[cfg(windows)]
    fn wake_thread(&self, message: u32) {
        use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

        let tid = self.thread_id();
        if tid != 0 {
            unsafe {
                PostThreadMessageW(tid, message, 0, 0);
            }
        }
    }
}

/// Spawns the service thread registering `initial` right away.
pub fn spawn_hotkey_service(
    initial: HotkeyBinding,
    events: AppEventSender,
) -> Result<HotkeyServiceHandle, HotkeyError> {
    spawn_with_registrar(initial, events, RealRegistrar)
}

pub(crate) fn spawn_with_registrar<R: HotkeyRegistrar>(
    initial: HotkeyBinding,
    events: AppEventSender,
    registrar: R,
) -> Result<HotkeyServiceHandle, HotkeyError> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::sync_channel::<Command>(COMMAND_CAPACITY);
    let thread_id = Arc::new(AtomicU32::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let stored_tid = Arc::clone(&thread_id);
    let done_flag = Arc::clone(&done);

    std::thread::Builder::new()
        .name("openaircast-hotkey".into())
        .spawn(move || {
            run_message_loop(initial, events, cmd_rx, stored_tid, done_flag, registrar);
        })
        .map_err(|_| HotkeyError::Spawn)?;

    Ok(HotkeyServiceHandle {
        cmd_tx,
        thread_id,
        done,
    })
}

#[cfg(windows)]
fn run_message_loop<R: HotkeyRegistrar>(
    initial: HotkeyBinding,
    events: AppEventSender,
    commands: std::sync::mpsc::Receiver<Command>,
    thread_id: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
    registrar: R,
) {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG};

    thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    let mut engine = HotkeyEngine::new(registrar, initial);

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // Drain queued commands before blocking so configure/shutdown never
        // waits behind an idle pump.
        if engine.process_message(WM_APP_CONFIGURE, 0, &commands, &events) {
            break;
        }

        let result = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        if engine.process_message(msg.message, msg.wParam, &commands, &events) {
            break;
        }
    }

    engine.shutdown();
    done.store(true, Ordering::SeqCst);
}

/// Non-Windows fallback loop used by cross builds and unit tests only.
#[cfg(not(windows))]
fn run_message_loop<R: HotkeyRegistrar>(
    initial: HotkeyBinding,
    events: AppEventSender,
    commands: std::sync::mpsc::Receiver<Command>,
    _thread_id: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
    registrar: R,
) {
    let mut engine = HotkeyEngine::new(registrar, initial);
    while let Ok(command) = commands.recv() {
        let stop = match command {
            Command::Shutdown => true,
            Command::Configure { .. } => false,
        };
        let _ = &mut engine;
        if stop {
            break;
        }
    }
    let _ = events;
    engine.shutdown();
    done.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        spawn_with_registrar, Command, HotkeyEngine, HotkeyRegistrar, COMMAND_CAPACITY, HOTKEY_ID,
        WM_HOTKEY,
    };
    use crate::app::{AppEvent, GenerationId, HotkeyBinding, MOD_ALT, MOD_CONTROL};
    use crate::app_handle::AppEventSender;
    use crate::platform::SystemAppearance;

    fn ctrl_alt(vk: u32) -> HotkeyBinding {
        HotkeyBinding {
            enabled: true,
            modifiers: MOD_CONTROL | MOD_ALT,
            virtual_key: vk,
        }
    }

    #[derive(Default)]
    struct RegistrarState {
        registrations: Vec<(u32, u32)>,
        unregister_count: u32,
        fail_queue: std::collections::VecDeque<String>,
    }

    /// Shared-state fake so clones observe every mutation the engine performs.
    #[derive(Clone, Default)]
    struct ScriptedRegistrar {
        state: Arc<std::sync::Mutex<RegistrarState>>,
    }

    impl ScriptedRegistrar {
        fn fail_next(&self, reason: &str) {
            self.state
                .lock()
                .unwrap()
                .fail_queue
                .push_back(reason.into());
        }

        fn registrations(&self) -> Vec<(u32, u32)> {
            self.state.lock().unwrap().registrations.clone()
        }

        fn unregister_count(&self) -> u32 {
            self.state.lock().unwrap().unregister_count
        }
    }

    impl HotkeyRegistrar for ScriptedRegistrar {
        fn register(&mut self, binding: &HotkeyBinding) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = state.fail_queue.pop_front() {
                return Err(reason);
            }
            state
                .registrations
                .push((binding.modifiers, binding.virtual_key));
            Ok(())
        }

        fn unregister(&mut self) {
            self.state.lock().unwrap().unregister_count += 1;
        }
    }

    #[test]
    fn engine_starts_by_registering_the_initial_binding() {
        let registrar = ScriptedRegistrar::default();
        let engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));

        assert_eq!(engine.applied(), ctrl_alt(0x48));
        assert_eq!(registrar.registrations().len(), 1);
        assert_eq!(registrar.unregister_count(), 1);
    }

    #[test]
    fn runtime_replacement_swaps_to_the_new_chord_transactionally() {
        let registrar = ScriptedRegistrar::default();
        let mut engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));
        let before_unregisters = registrar.unregister_count();

        let (applied, failure) = engine.apply(ctrl_alt(0x50));

        assert_eq!(applied, ctrl_alt(0x50));
        assert_eq!(failure, None);
        assert_eq!(
            registrar.registrations().last(),
            Some(&(MOD_CONTROL | MOD_ALT, 0x50))
        );
        assert!(registrar.unregister_count() > before_unregisters);
    }

    #[test]
    fn disabling_the_binding_unregisters_without_registering_anything() {
        let registrar = ScriptedRegistrar::default();
        let mut engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));
        let registrations_before = registrar.registrations().len();
        let unregisters_before = registrar.unregister_count();
        let disabled = HotkeyBinding {
            enabled: false,
            ..ctrl_alt(0x48)
        };

        let (applied, failure) = engine.apply(disabled);

        assert!(!applied.enabled);
        assert_eq!(failure, None);
        assert_eq!(registrar.registrations().len(), registrations_before);
        assert!(registrar.unregister_count() > unregisters_before);
    }

    #[test]
    fn failed_replacement_restores_the_previous_chord_and_reports_once() {
        let registrar = ScriptedRegistrar::default();
        let mut engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));

        registrar.fail_next("chord already taken");
        let (applied, failure) = engine.apply(ctrl_alt(0x50));

        assert_eq!(applied, ctrl_alt(0x48), "previous binding stays in force");
        assert_eq!(failure.as_deref(), Some("chord already taken"));
        assert!(
            registrar.registrations().iter().any(|&(_, vk)| vk == 0x48),
            "previous chord was re-registered"
        );
    }

    #[test]
    fn failed_restore_reports_the_actually_applied_disabled_state() {
        let registrar = ScriptedRegistrar::default();
        let mut engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));

        registrar.fail_next("first failure");
        registrar.fail_next("restore failure");
        let (applied, failure) = engine.apply(ctrl_alt(0x50));

        assert!(!applied.enabled, "service reports the real state");
        assert!(
            failure.unwrap().contains("could not be restored"),
            "the failure explains the degraded state"
        );
    }

    #[test]
    fn hotkey_press_dispatches_exactly_one_global_event() {
        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let events = AppEventSender::new(events_tx);
        let (_cmd_tx, cmd_rx) = mpsc::sync_channel::<Command>(COMMAND_CAPACITY);
        let mut engine = HotkeyEngine::new(ScriptedRegistrar::default(), ctrl_alt(0x48));

        engine.process_message(WM_HOTKEY, HOTKEY_ID as usize, &cmd_rx, &events);
        drop(engine);

        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(1)),
            Ok(AppEvent::GlobalHotkeyPressed)
        ));
    }

    #[test]
    fn bounded_shutdown_unregisters_at_most_once() {
        let registrar = ScriptedRegistrar::default();
        let mut engine = HotkeyEngine::new(registrar.clone(), ctrl_alt(0x48));
        let after_start = registrar.unregister_count();

        engine.shutdown();
        engine.shutdown();

        assert_eq!(
            registrar.unregister_count() - after_start,
            1,
            "teardown never double-unregisters"
        );
        assert!(!engine.applied().enabled);
    }

    #[test]
    fn spawned_service_shuts_down_within_bounds() {
        let (events_tx, _events_rx) = mpsc::sync_channel(8);
        let handle = spawn_with_registrar(
            ctrl_alt(0x39),
            AppEventSender::new(events_tx),
            ScriptedRegistrar::default(),
        )
        .unwrap();

        assert!(handle
            .try_reconfigure(GenerationId(1), ctrl_alt(0x37))
            .is_ok());
        assert!(handle.shutdown_and_wait(Duration::from_secs(2)));
    }

    #[test]
    fn system_appearance_is_available_without_blocking() {
        let appearance = SystemAppearance::read();
        let _ = (
            appearance.client_animation_enabled,
            appearance.high_contrast,
            appearance.colors,
        );
    }

    /// Hardware test: registers a non-default chord for real, presses it via a
    /// synthetic thread message, expects exactly one dispatch, then shuts down.
    /// Run explicitly with:
    /// `cargo test -p homepod-cast platform::hotkey::tests::windows_message_loop_round_trip -- --ignored --exact`
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn windows_message_loop_round_trip() {
        use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let handle =
            super::spawn_hotkey_service(ctrl_alt(0x39), AppEventSender::new(events_tx)).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while handle.thread_id() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let tid = handle.thread_id();
        assert_ne!(tid, 0);

        let posted = unsafe { PostThreadMessageW(tid, WM_HOTKEY, HOTKEY_ID as usize, 0) };
        assert_ne!(posted, 0, "synthetic press must reach the pump");

        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(2)),
            Ok(AppEvent::GlobalHotkeyPressed)
        ));

        assert!(handle.shutdown_and_wait(Duration::from_secs(2)));
    }
}
