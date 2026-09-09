//! Windows display settings observed by message, never by polling.
//!
//! Windows announces language, contrast, colour, and animation changes with
//! `WM_SETTINGCHANGE`, `WM_THEMECHANGED`, and `WM_SYSCOLORCHANGE`. A window
//! subclass turns those broadcasts into exactly one fresh reading, one cache
//! swap, and one repaint request, so the shell never runs a timer and never
//! queries Win32 per frame.
//!
//! Everything decidable without an operating system lives in pure functions and
//! in [`SettingsTracker`], which takes its reading through an injected closure.
//! The Win32 half is confined to the attachment: extracting the `HWND`,
//! installing the subclass, and removing it again exactly once.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::app::ResolvedLocale;
use crate::platform::SystemAppearance;

/// One coherent reading of every Windows setting the shell renders from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowsSettingsSnapshot {
    pub locale: ResolvedLocale,
    pub appearance: SystemAppearance,
}

/// Why a monitor could not be attached to a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowsSettingsError {
    /// The window handle does not name a Win32 window.
    #[error("the window handle is not a Win32 window handle")]
    UnsupportedWindowHandle,
    /// `SetWindowSubclass` refused the installation.
    #[error("Windows refused to install the display-settings subclass")]
    SubclassRejected,
}

/// Receives Windows display-language changes.
///
/// The monitor owns no knowledge of the app's event vocabulary: it reports a
/// change through this seam, which keeps the tracker testable without an actor
/// and keeps the Win32 half free of reducer types.
pub trait WindowsSettingsEvents: Send + Sync {
    /// Called once per actual change of the Windows display language.
    fn windows_display_language_changed(&self, locale: ResolvedLocale);
}

// ===========================================================================
// Pure language mapping
// ===========================================================================

/// Maps the ordered Windows preferred UI language tags to a shipped locale.
///
/// Only the first tag decides, because that is the language Windows actually
/// renders in. Anything OpenAirCast does not ship -- including an empty list
/// from a failed call -- falls back to English.
pub(crate) fn map_preferred_ui_languages<S: AsRef<str>>(tags: &[S]) -> ResolvedLocale {
    match tags.first() {
        Some(tag) if is_german_tag(tag.as_ref()) => ResolvedLocale::German,
        _ => ResolvedLocale::English,
    }
}

/// True when the BCP-47 tag's primary language subtag is German.
///
/// Comparing the subtag rather than a prefix keeps `deu`, `des`, and `nds-DE`
/// out: only `de` and its regional variants are the German UI.
fn is_german_tag(tag: &str) -> bool {
    let primary = tag.split(['-', '_']).next().unwrap_or_default();
    primary.eq_ignore_ascii_case("de")
}

/// Splits the double-null-terminated UTF-16 multi-string that
/// `GetUserPreferredUILanguages` writes into individual BCP-47 tags.
pub(crate) fn parse_language_multi_string(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|unit| *unit == 0)
        .take_while(|tag| !tag.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

// ===========================================================================
// Message classification and the exactly-once detach gate
// ===========================================================================

#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    WM_NCDESTROY, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED,
};

#[cfg(not(windows))]
const WM_NCDESTROY: u32 = 0x0082;
#[cfg(not(windows))]
const WM_SYSCOLORCHANGE: u32 = 0x0015;
#[cfg(not(windows))]
const WM_SETTINGCHANGE: u32 = 0x001A;
#[cfg(not(windows))]
const WM_THEMECHANGED: u32 = 0x031A;

/// True for the three broadcasts that can change what this module reads.
///
/// `WM_SYSCOLORCHANGE` is not redundant next to `WM_THEMECHANGED`: editing the
/// colours of the *active* contrast theme changes the `GetSysColor` values that
/// the entire High Contrast token set is derived from, while the theme itself
/// stays put -- so that edit arrives as a colour broadcast and nothing else.
pub(crate) const fn message_requires_reading(message: u32) -> bool {
    matches!(
        message,
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE
    )
}

/// Shared exactly-once gate between `WM_NCDESTROY` and `Drop`.
///
/// Both paths can run, in either order, for the same attachment. Whoever wins
/// removes the subclass and frees the callback's reference; the loser must do
/// nothing at all, or the reference would be released twice.
#[derive(Debug, Default)]
pub(crate) struct DetachGuard {
    claimed: AtomicBool,
}

impl DetachGuard {
    /// Returns true for the single caller that is allowed to detach.
    pub(crate) fn claim(&self) -> bool {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.claimed.load(Ordering::Acquire)
    }
}

// ===========================================================================
// The tracker: everything a Windows message causes, without Windows
// ===========================================================================

type SettingsReader = Arc<dyn Fn() -> WindowsSettingsSnapshot + Send + Sync>;

/// What one reading changed. Returned so tests can count effects exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SettingsUpdate {
    pub cache_swapped: bool,
    pub locale_dispatched: bool,
    pub repaint_requested: bool,
}

pub(crate) struct SettingsTracker {
    current: Arc<ArcSwap<WindowsSettingsSnapshot>>,
    read: SettingsReader,
    events: Arc<dyn WindowsSettingsEvents>,
    request_repaint: Arc<dyn Fn() + Send + Sync>,
    detach: DetachGuard,
}

impl SettingsTracker {
    fn new(
        current: Arc<ArcSwap<WindowsSettingsSnapshot>>,
        read: SettingsReader,
        events: Arc<dyn WindowsSettingsEvents>,
        request_repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            current,
            read,
            events,
            request_repaint,
            detach: DetachGuard::default(),
        }
    }

    /// Takes one fresh reading and publishes it if anything changed.
    ///
    /// Windows repeats `WM_SETTINGCHANGE` freely -- one user action can raise
    /// several -- so the comparison against the published reading is what
    /// turns a burst of broadcasts into a single swap, a single language
    /// event, and a single repaint.
    pub(crate) fn refresh(&self) -> SettingsUpdate {
        if self.detach.is_claimed() {
            return SettingsUpdate::default();
        }

        let published = self.current.load();
        let fresh = (self.read)();
        if **published == fresh {
            return SettingsUpdate::default();
        }

        let locale_changed = published.locale != fresh.locale;
        self.current.store(Arc::new(fresh));
        if locale_changed {
            self.events.windows_display_language_changed(fresh.locale);
        }
        (self.request_repaint)();

        SettingsUpdate {
            cache_swapped: true,
            locale_dispatched: locale_changed,
            repaint_requested: true,
        }
    }

    /// [`Self::refresh`] with unwinding contained.
    ///
    /// This runs inside a Win32 window procedure, and unwinding across that
    /// boundary is undefined behaviour. A panic is therefore absorbed here and
    /// reported; the window keeps working with the previous reading.
    pub(crate) fn guarded_refresh(&self) -> SettingsUpdate {
        match std::panic::catch_unwind(AssertUnwindSafe(|| self.refresh())) {
            Ok(update) => update,
            Err(_) => {
                tracing::error!(
                    "reading the Windows display settings panicked; keeping the previous reading"
                );
                SettingsUpdate::default()
            }
        }
    }

    /// Handles one window message; returns what it changed.
    pub(crate) fn handle_message(&self, message: u32) -> SettingsUpdate {
        if message_requires_reading(message) {
            self.guarded_refresh()
        } else {
            SettingsUpdate::default()
        }
    }
}

// ===========================================================================
// Reading Windows
// ===========================================================================

/// Reads the Windows display language and appearance in one go.
pub fn read_windows_settings() -> WindowsSettingsSnapshot {
    WindowsSettingsSnapshot {
        locale: read_windows_display_locale(),
        appearance: SystemAppearance::read(),
    }
}

#[cfg(windows)]
fn read_windows_display_locale() -> ResolvedLocale {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut count = 0u32;
    let mut units = 0u32;
    let sized = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut units,
        )
    };
    if sized == 0 || units == 0 {
        return ResolvedLocale::English;
    }

    let mut buffer = vec![0u16; units as usize];
    let filled = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            buffer.as_mut_ptr(),
            &mut units,
        )
    };
    if filled == 0 {
        return ResolvedLocale::English;
    }

    map_preferred_ui_languages(&parse_language_multi_string(&buffer))
}

#[cfg(not(windows))]
fn read_windows_display_locale() -> ResolvedLocale {
    ResolvedLocale::English
}

// ===========================================================================
// The monitor
// ===========================================================================

/// Publishes the newest Windows settings reading to the UI thread.
pub struct WindowsSettingsMonitor {
    current: Arc<ArcSwap<WindowsSettingsSnapshot>>,
    #[cfg(windows)]
    _attachment: Option<SubclassAttachment>,
}

impl WindowsSettingsMonitor {
    /// The newest complete reading. Cheap enough to call once per frame.
    pub fn current(&self) -> Arc<WindowsSettingsSnapshot> {
        self.current.load_full()
    }

    /// A monitor with no OS attachment.
    ///
    /// `current()` keeps returning the reading it was built from. Used when
    /// attachment fails -- a stale-but-coherent appearance beats no UI -- and
    /// by tests that build the shell without a real window.
    pub fn detached(initial: WindowsSettingsSnapshot) -> Self {
        Self {
            current: Arc::new(ArcSwap::from_pointee(initial)),
            #[cfg(windows)]
            _attachment: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn publish(&self, next: WindowsSettingsSnapshot) {
        self.current.store(Arc::new(next));
    }
}

/// Attaches a settings monitor to the application window.
pub fn attach_windows_settings_monitor(
    window_handle: raw_window_handle::RawWindowHandle,
    initial: WindowsSettingsSnapshot,
    events: Arc<dyn WindowsSettingsEvents>,
    request_repaint: Arc<dyn Fn() + Send + Sync>,
) -> Result<WindowsSettingsMonitor, WindowsSettingsError> {
    attach_with_reader(
        window_handle,
        initial,
        events,
        request_repaint,
        Arc::new(read_windows_settings),
    )
}

/// Distinguishes this subclass from every other one on the same window.
#[cfg(windows)]
const SUBCLASS_ID: usize = 0x4f41_4353; // "OACS"

/// The live Win32 half of a monitor.
///
/// `hwnd` and `callback_reference` are kept as integers rather than pointers
/// so the monitor stays `Send`; thread affinity is enforced explicitly in
/// [`Drop`] instead of by the type system, which also produces a diagnosable
/// error rather than a silent cross-thread Win32 call.
#[cfg(windows)]
struct SubclassAttachment {
    hwnd: isize,
    /// The monitor's own reference, so the tracker outlives any in-flight
    /// callback even if the window is destroyed first.
    tracker: Arc<SettingsTracker>,
    /// The reference handed to Windows as `dwRefData`, owned by the callback
    /// and released exactly once by whoever claims the detach gate.
    callback_reference: usize,
    owner_thread: std::thread::ThreadId,
}

#[cfg(windows)]
impl Drop for SubclassAttachment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;

        if std::thread::current().id() != self.owner_thread {
            tracing::error!(
                "the Windows settings monitor was dropped off its window thread; \
                 leaking the subclass instead of removing it across threads"
            );
            return;
        }
        if !self.tracker.detach.claim() {
            // `WM_NCDESTROY` already removed the subclass and released the
            // callback's reference; doing it again would free it twice.
            return;
        }
        unsafe {
            RemoveWindowSubclass(self.hwnd as HWND, Some(settings_subclass_proc), SUBCLASS_ID);
            drop(Arc::from_raw(
                self.callback_reference as *const SettingsTracker,
            ));
        }
    }
}

/// The subclass procedure Windows calls for every message the window receives.
///
/// Three properties make this sound: nothing unwinds out of it, everything it
/// does is bounded and non-blocking, and the unhandled remainder always goes
/// on to [`DefSubclassProc`] so the rest of the chain still sees the message.
#[cfg(windows)]
unsafe extern "system" fn settings_subclass_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    reference: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};

    // Unwinding across an `extern "system"` boundary is undefined behaviour.
    // The reading is already guarded; this second net covers the rest.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if reference == 0 {
            return;
        }
        // Borrowed, not owned: an ordinary message must not release the
        // callback's reference.
        let tracker = std::mem::ManuallyDrop::new(unsafe {
            Arc::from_raw(reference as *const SettingsTracker)
        });

        // The same entry point the tracker tests drive, so production takes no
        // second, untested path through the message filter.
        tracker.handle_message(message);

        if message == WM_NCDESTROY && tracker.detach.claim() {
            // The window is going away under us. Detach here so a later
            // `Drop` finds the gate claimed and leaves the reference alone.
            unsafe {
                RemoveWindowSubclass(hwnd, Some(settings_subclass_proc), SUBCLASS_ID);
            }
            // Nothing may touch `tracker` past this line.
            drop(std::mem::ManuallyDrop::into_inner(tracker));
        }
    }));

    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(windows)]
fn attach_with_reader(
    window_handle: raw_window_handle::RawWindowHandle,
    initial: WindowsSettingsSnapshot,
    events: Arc<dyn WindowsSettingsEvents>,
    request_repaint: Arc<dyn Fn() + Send + Sync>,
    read: SettingsReader,
) -> Result<WindowsSettingsMonitor, WindowsSettingsError> {
    let raw_window_handle::RawWindowHandle::Win32(win32) = window_handle else {
        return Err(WindowsSettingsError::UnsupportedWindowHandle);
    };
    let hwnd = win32.hwnd.get();

    attach_with_installer(initial, events, request_repaint, read, move |tracker| {
        install_subclass(hwnd, tracker)
    })
}

/// Installs the subclass and hands back the live attachment.
///
/// Separated from [`attach_with_installer`] so the sequencing around it can be
/// tested without a window: this is the only part that needs Win32.
#[cfg(windows)]
fn install_subclass(
    hwnd: isize,
    tracker: &Arc<SettingsTracker>,
) -> Result<Option<SubclassAttachment>, WindowsSettingsError> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;

    let callback_reference = Arc::into_raw(Arc::clone(tracker)) as usize;
    let installed = unsafe {
        SetWindowSubclass(
            hwnd as HWND,
            Some(settings_subclass_proc),
            SUBCLASS_ID,
            callback_reference,
        )
    };
    if installed == 0 {
        // Nothing is registered, so this reference can never be reached
        // again: release it here rather than leak it.
        unsafe {
            drop(Arc::from_raw(callback_reference as *const SettingsTracker));
        }
        return Err(WindowsSettingsError::SubclassRejected);
    }

    Ok(Some(SubclassAttachment {
        hwnd,
        tracker: Arc::clone(tracker),
        callback_reference,
        owner_thread: std::thread::current().id(),
    }))
}

/// Builds the tracker, installs the subclass through `install`, then takes one
/// reconciling reading.
///
/// The order is the whole point. The startup reading is taken before the window
/// exists, and the window plus renderer initialisation that follows can take
/// seconds; every settings broadcast in that gap is delivered to a window that
/// nobody is listening on. Reading once *after* the subclass is in place closes
/// the gap from both ends: what changed during initialisation is picked up here,
/// and what changes from here on reaches the callback. [`SettingsTracker::refresh`]
/// compares against the published reading, so a broadcast that already arrived
/// costs nothing but the read.
#[cfg(windows)]
fn attach_with_installer<I>(
    initial: WindowsSettingsSnapshot,
    events: Arc<dyn WindowsSettingsEvents>,
    request_repaint: Arc<dyn Fn() + Send + Sync>,
    read: SettingsReader,
    install: I,
) -> Result<WindowsSettingsMonitor, WindowsSettingsError>
where
    I: FnOnce(&Arc<SettingsTracker>) -> Result<Option<SubclassAttachment>, WindowsSettingsError>,
{
    let current = Arc::new(ArcSwap::from_pointee(initial));
    let tracker = Arc::new(SettingsTracker::new(
        Arc::clone(&current),
        read,
        events,
        request_repaint,
    ));

    let attachment = install(&tracker)?;
    tracker.guarded_refresh();

    Ok(WindowsSettingsMonitor {
        current,
        _attachment: attachment,
    })
}

#[cfg(not(windows))]
fn attach_with_reader(
    _window_handle: raw_window_handle::RawWindowHandle,
    _initial: WindowsSettingsSnapshot,
    _events: Arc<dyn WindowsSettingsEvents>,
    _request_repaint: Arc<dyn Fn() + Send + Sync>,
    _read: SettingsReader,
) -> Result<WindowsSettingsMonitor, WindowsSettingsError> {
    Err(WindowsSettingsError::UnsupportedWindowHandle)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    use super::*;
    use crate::platform::SystemColors;

    const LIGHT_COLORS: SystemColors = SystemColors {
        background: [255, 255, 255],
        foreground: [0, 0, 0],
        highlight: [0, 120, 215],
        highlight_text: [255, 255, 255],
        disabled_text: [128, 128, 128],
        link: [0, 102, 204],
    };

    /// Every one of the six colours differs from [`LIGHT_COLORS`], so a test
    /// that drops any single channel fails instead of passing by coincidence.
    const CONTRAST_COLORS: SystemColors = SystemColors {
        background: [1, 2, 3],
        foreground: [4, 5, 6],
        highlight: [7, 8, 9],
        highlight_text: [10, 11, 12],
        disabled_text: [13, 14, 15],
        link: [16, 17, 18],
    };

    const CALM_LIGHT: WindowsSettingsSnapshot = WindowsSettingsSnapshot {
        locale: ResolvedLocale::English,
        appearance: SystemAppearance {
            client_animation_enabled: true,
            high_contrast: false,
            colors: LIGHT_COLORS,
        },
    };

    const CALM_LIGHT_GERMAN: WindowsSettingsSnapshot = WindowsSettingsSnapshot {
        locale: ResolvedLocale::German,
        ..CALM_LIGHT
    };

    const HIGH_CONTRAST_REDUCED_MOTION: WindowsSettingsSnapshot = WindowsSettingsSnapshot {
        locale: ResolvedLocale::English,
        appearance: SystemAppearance {
            client_animation_enabled: false,
            high_contrast: true,
            colors: CONTRAST_COLORS,
        },
    };

    const HIGH_CONTRAST_GERMAN: WindowsSettingsSnapshot = WindowsSettingsSnapshot {
        locale: ResolvedLocale::German,
        ..HIGH_CONTRAST_REDUCED_MOTION
    };

    #[derive(Default)]
    struct RecordedLocales(Mutex<Vec<ResolvedLocale>>);

    impl WindowsSettingsEvents for RecordedLocales {
        fn windows_display_language_changed(&self, locale: ResolvedLocale) {
            self.0.lock().unwrap().push(locale);
        }
    }

    /// A tracker whose reading, dispatches, and repaints are all observable.
    struct TrackerUnderTest {
        tracker: SettingsTracker,
        cache: Arc<ArcSwap<WindowsSettingsSnapshot>>,
        reading: Arc<Mutex<WindowsSettingsSnapshot>>,
        locales: Arc<RecordedLocales>,
        repaints: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
    }

    impl TrackerUnderTest {
        fn new(initial: WindowsSettingsSnapshot) -> Self {
            Self::with_reader(initial, Arc::new(Mutex::new(initial)))
        }

        fn with_reader(
            initial: WindowsSettingsSnapshot,
            reading: Arc<Mutex<WindowsSettingsSnapshot>>,
        ) -> Self {
            let cache = Arc::new(ArcSwap::from_pointee(initial));
            let locales = Arc::new(RecordedLocales::default());
            let repaints = Arc::new(AtomicUsize::new(0));
            let reads = Arc::new(AtomicUsize::new(0));

            let read_source = Arc::clone(&reading);
            let read_counter = Arc::clone(&reads);
            let repaint_counter = Arc::clone(&repaints);
            let tracker = SettingsTracker::new(
                Arc::clone(&cache),
                Arc::new(move || {
                    read_counter.fetch_add(1, Ordering::SeqCst);
                    *read_source.lock().unwrap()
                }),
                Arc::clone(&locales) as Arc<dyn WindowsSettingsEvents>,
                Arc::new(move || {
                    repaint_counter.fetch_add(1, Ordering::SeqCst);
                }),
            );

            Self {
                tracker,
                cache,
                reading,
                locales,
                repaints,
                reads,
            }
        }

        fn set_reading(&self, next: WindowsSettingsSnapshot) {
            *self.reading.lock().unwrap() = next;
        }

        fn dispatched(&self) -> Vec<ResolvedLocale> {
            self.locales.0.lock().unwrap().clone()
        }

        fn repaints(&self) -> usize {
            self.repaints.load(Ordering::SeqCst)
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        fn cached(&self) -> Arc<WindowsSettingsSnapshot> {
            self.cache.load_full()
        }
    }

    mod language_mapping {
        use super::*;

        #[test]
        fn a_leading_german_tag_maps_german() {
            for tag in ["de", "de-DE", "de-AT", "de-CH", "de-LI"] {
                assert_eq!(
                    map_preferred_ui_languages(&[tag]),
                    ResolvedLocale::German,
                    "{tag} is a German Windows display language"
                );
            }
        }

        #[test]
        fn any_other_leading_tag_maps_english() {
            for tag in ["en-US", "en-GB", "fr-FR", "nl-NL", "nds-DE", "deu", "des"] {
                assert_eq!(
                    map_preferred_ui_languages(&[tag]),
                    ResolvedLocale::English,
                    "{tag} is not a language OpenAirCast ships"
                );
            }
        }

        #[test]
        fn an_empty_language_list_maps_english() {
            let none: [&str; 0] = [];

            assert_eq!(map_preferred_ui_languages(&none), ResolvedLocale::English);
        }

        #[test]
        fn only_the_first_tag_decides() {
            assert_eq!(
                map_preferred_ui_languages(&["en-US", "de-DE"]),
                ResolvedLocale::English,
                "the fallback languages behind the display language must not win"
            );
            assert_eq!(
                map_preferred_ui_languages(&["de-DE", "en-US"]),
                ResolvedLocale::German
            );
        }

        #[test]
        fn tags_are_matched_without_regard_to_case() {
            assert_eq!(
                map_preferred_ui_languages(&["DE-de"]),
                ResolvedLocale::German
            );
        }

        #[test]
        fn the_language_buffer_is_split_at_its_nulls() {
            let buffer: Vec<u16> = "de-DE\0en-US\0\0".encode_utf16().collect();

            assert_eq!(parse_language_multi_string(&buffer), ["de-DE", "en-US"]);
        }

        #[test]
        fn an_empty_language_buffer_yields_no_tags() {
            let buffer: Vec<u16> = "\0\0".encode_utf16().collect();

            assert!(parse_language_multi_string(&buffer).is_empty());
        }

        #[test]
        fn a_failed_read_leaves_the_ui_in_english() {
            // The Win32 call reports failure by writing nothing; the parser
            // then sees an all-zero buffer and the mapping falls back.
            let buffer = vec![0u16; 16];

            assert_eq!(
                map_preferred_ui_languages(&parse_language_multi_string(&buffer)),
                ResolvedLocale::English
            );
        }
    }

    mod tracking {
        use super::*;

        #[test]
        fn an_unchanged_reading_swaps_nothing_dispatches_nothing_and_repaints_nothing() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            let before = harness.cached();

            let update = harness.tracker.handle_message(WM_SETTINGCHANGE);

            assert_eq!(update, SettingsUpdate::default());
            assert!(
                Arc::ptr_eq(&before, &harness.cached()),
                "an unchanged reading must not replace the published snapshot"
            );
            assert!(harness.dispatched().is_empty());
            assert_eq!(harness.repaints(), 0);
        }

        #[test]
        fn a_language_change_swaps_once_dispatches_once_and_repaints_once() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(CALM_LIGHT_GERMAN);

            let update = harness.tracker.handle_message(WM_SETTINGCHANGE);

            assert_eq!(
                update,
                SettingsUpdate {
                    cache_swapped: true,
                    locale_dispatched: true,
                    repaint_requested: true,
                }
            );
            assert_eq!(*harness.cached(), CALM_LIGHT_GERMAN);
            assert_eq!(harness.dispatched(), [ResolvedLocale::German]);
            assert_eq!(harness.repaints(), 1);
        }

        #[test]
        fn an_appearance_change_swaps_and_repaints_without_a_language_event() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_REDUCED_MOTION);

            let update = harness.tracker.handle_message(WM_THEMECHANGED);

            assert_eq!(
                update,
                SettingsUpdate {
                    cache_swapped: true,
                    locale_dispatched: false,
                    repaint_requested: true,
                }
            );
            assert!(
                harness.dispatched().is_empty(),
                "the language did not change, so nothing may reach the reducer"
            );
            assert_eq!(harness.repaints(), 1);
        }

        #[test]
        fn a_combined_change_still_swaps_dispatches_and_repaints_exactly_once() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_GERMAN);

            let update = harness.tracker.handle_message(WM_SETTINGCHANGE);

            assert_eq!(
                update,
                SettingsUpdate {
                    cache_swapped: true,
                    locale_dispatched: true,
                    repaint_requested: true,
                }
            );
            assert_eq!(*harness.cached(), HIGH_CONTRAST_GERMAN);
            assert_eq!(harness.dispatched(), [ResolvedLocale::German]);
            assert_eq!(
                harness.repaints(),
                1,
                "one complete change is one repaint, not one per changed field"
            );
        }

        #[test]
        fn the_cache_carries_animation_contrast_and_all_six_system_colors() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_REDUCED_MOTION);

            harness.tracker.handle_message(WM_SETTINGCHANGE);

            let published = harness.cached();
            assert!(!published.appearance.client_animation_enabled);
            assert!(published.appearance.high_contrast);
            assert_eq!(published.appearance.colors, CONTRAST_COLORS);
        }

        #[test]
        fn a_settled_reading_is_swapped_exactly_once_however_many_messages_arrive() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_GERMAN);

            let first = harness.tracker.handle_message(WM_SETTINGCHANGE);
            let published = harness.cached();
            let second = harness.tracker.handle_message(WM_SETTINGCHANGE);
            let third = harness.tracker.handle_message(WM_THEMECHANGED);

            assert!(first.cache_swapped);
            assert_eq!(second, SettingsUpdate::default());
            assert_eq!(third, SettingsUpdate::default());
            assert!(
                Arc::ptr_eq(&published, &harness.cached()),
                "Windows repeats these broadcasts; only a real change may swap"
            );
            assert_eq!(harness.dispatched(), [ResolvedLocale::German]);
            assert_eq!(harness.repaints(), 1);
        }

        /// Editing the active contrast theme changes the `GetSysColor` values
        /// without changing the theme, so Windows announces it with
        /// `WM_SYSCOLORCHANGE` alone -- no `WM_THEMECHANGED`, not necessarily
        /// a `WM_SETTINGCHANGE`. Two thirds of a reading are those colours, so
        /// ignoring this broadcast would paint High Contrast in colours
        /// Windows has stopped using.
        #[test]
        fn a_system_color_change_swaps_the_palette_without_a_language_event() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_REDUCED_MOTION);

            let update = harness.tracker.handle_message(WM_SYSCOLORCHANGE);

            assert_eq!(
                update,
                SettingsUpdate {
                    cache_swapped: true,
                    locale_dispatched: false,
                    repaint_requested: true,
                }
            );
            assert_eq!(harness.cached().appearance.colors, CONTRAST_COLORS);
            assert_eq!(harness.reads(), 1);
            assert!(
                harness.dispatched().is_empty(),
                "a colour edit is not a language change"
            );
            assert_eq!(harness.repaints(), 1);
        }

        #[test]
        fn only_the_three_settings_broadcasts_cause_a_reading() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_GERMAN);

            for message in [0x0001u32, 0x000F, 0x0005, 0x0014, WM_NCDESTROY] {
                assert_eq!(
                    harness.tracker.handle_message(message),
                    SettingsUpdate::default(),
                    "message {message:#06x} must not reach Win32"
                );
            }

            assert_eq!(
                harness.reads(),
                0,
                "an unrelated message must not cost a settings read"
            );
            assert!(message_requires_reading(WM_SETTINGCHANGE));
            assert!(message_requires_reading(WM_THEMECHANGED));
            assert!(message_requires_reading(WM_SYSCOLORCHANGE));
        }

        /// Detaching has to silence a tracker that demonstrably still worked
        /// one message earlier, or the assertions below would hold for a
        /// tracker that never did anything in the first place.
        #[test]
        fn nothing_happens_after_the_monitor_is_detached() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            harness.set_reading(HIGH_CONTRAST_GERMAN);
            assert!(
                harness
                    .tracker
                    .handle_message(WM_SETTINGCHANGE)
                    .cache_swapped,
                "the tracker has to be live before detaching proves anything"
            );
            harness.set_reading(CALM_LIGHT_GERMAN);
            let reads_before = harness.reads();
            let published = harness.cached();

            assert!(harness.tracker.detach.claim());
            let update = harness.tracker.handle_message(WM_SETTINGCHANGE);

            assert_eq!(update, SettingsUpdate::default());
            assert_eq!(
                harness.reads(),
                reads_before,
                "a detached tracker must not even read Windows"
            );
            assert!(Arc::ptr_eq(&published, &harness.cached()));
            assert_eq!(harness.dispatched(), [ResolvedLocale::German]);
            assert_eq!(harness.repaints(), 1);
        }

        #[test]
        fn a_panicking_reading_cannot_unwind_across_the_callback_boundary() {
            let cache = Arc::new(ArcSwap::from_pointee(CALM_LIGHT));
            let reads = Arc::new(AtomicUsize::new(0));
            let read_counter = Arc::clone(&reads);
            let tracker = SettingsTracker::new(
                Arc::clone(&cache),
                Arc::new(move || {
                    read_counter.fetch_add(1, Ordering::SeqCst);
                    panic!("the settings read failed");
                }),
                Arc::new(RecordedLocales::default()),
                Arc::new(|| {}),
            );

            let update = tracker.handle_message(WM_SETTINGCHANGE);

            assert_eq!(
                reads.load(Ordering::SeqCst),
                1,
                "the panic must come from a reading that actually ran"
            );
            assert_eq!(
                update,
                SettingsUpdate::default(),
                "a panic must be absorbed, not carried into the window procedure"
            );
            assert_eq!(*cache.load_full(), CALM_LIGHT);

            // The tracker survives its own accident: the next good reading is
            // still published.
            let healthy = SettingsTracker::new(
                Arc::clone(&cache),
                Arc::new(|| HIGH_CONTRAST_GERMAN),
                Arc::new(RecordedLocales::default()),
                Arc::new(|| {}),
            );
            assert!(healthy.handle_message(WM_THEMECHANGED).cache_swapped);
        }
    }

    mod detaching {
        use super::*;

        #[test]
        fn the_detach_gate_admits_exactly_one_claimer() {
            let guard = DetachGuard::default();

            assert!(!guard.is_claimed());
            assert!(guard.claim(), "the first claimer detaches");
            assert!(!guard.claim(), "a second claim would free the state twice");
            assert!(!guard.claim());
            assert!(guard.is_claimed());
        }
    }

    mod attaching {
        use super::*;

        #[test]
        fn a_window_handle_that_is_not_a_win32_window_is_rejected_without_attaching() {
            let handle =
                raw_window_handle::RawWindowHandle::Web(raw_window_handle::WebWindowHandle::new(1));

            let outcome = attach_windows_settings_monitor(
                handle,
                CALM_LIGHT,
                Arc::new(RecordedLocales::default()),
                Arc::new(|| {}),
            );

            match outcome {
                Err(error) => assert_eq!(error, WindowsSettingsError::UnsupportedWindowHandle),
                Ok(_) => panic!("a non-Win32 handle has no window to subclass"),
            }
        }

        #[test]
        fn a_detached_monitor_keeps_publishing_its_startup_reading() {
            let monitor = WindowsSettingsMonitor::detached(HIGH_CONTRAST_REDUCED_MOTION);

            assert_eq!(*monitor.current(), HIGH_CONTRAST_REDUCED_MOTION);
        }

        /// The startup reading is taken before the window exists; the whole
        /// window and renderer initialisation happens between it and the
        /// attachment, and every broadcast in that gap reaches nobody. So the
        /// attachment has to reconcile -- and it has to do so *after* the
        /// subclass is installed, because only then can no further broadcast
        /// slip past unobserved.
        #[cfg(windows)]
        #[test]
        fn attaching_publishes_a_reading_taken_after_the_window_started_listening() {
            let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
            let read_order = Arc::clone(&order);
            let install_order = Arc::clone(&order);
            let reader: SettingsReader = Arc::new(move || {
                read_order.lock().unwrap().push("read");
                HIGH_CONTRAST_GERMAN
            });
            let locales = Arc::new(RecordedLocales::default());
            let repaints = Arc::new(AtomicUsize::new(0));
            let repaint_counter = Arc::clone(&repaints);

            let monitor = attach_with_installer(
                CALM_LIGHT,
                Arc::clone(&locales) as Arc<dyn WindowsSettingsEvents>,
                Arc::new(move || {
                    repaint_counter.fetch_add(1, Ordering::SeqCst);
                }),
                reader,
                move |_tracker| {
                    install_order.lock().unwrap().push("install");
                    Ok(None)
                },
            )
            .expect("the stand-in installation succeeds");

            assert_eq!(
                *order.lock().unwrap(),
                ["install", "read"],
                "reading before the subclass is in place would reopen the gap"
            );
            assert_eq!(
                *monitor.current(),
                HIGH_CONTRAST_GERMAN,
                "the monitor must publish the reading it took, not the startup snapshot"
            );
            assert_eq!(locales.0.lock().unwrap().clone(), [ResolvedLocale::German]);
            assert_eq!(repaints.load(Ordering::SeqCst), 1);
        }

        #[cfg(windows)]
        #[test]
        fn a_refused_installation_reads_nothing_and_reports_the_refusal() {
            let reads = Arc::new(AtomicUsize::new(0));
            let read_counter = Arc::clone(&reads);
            let reader: SettingsReader = Arc::new(move || {
                read_counter.fetch_add(1, Ordering::SeqCst);
                HIGH_CONTRAST_GERMAN
            });

            let outcome = attach_with_installer(
                CALM_LIGHT,
                Arc::new(RecordedLocales::default()),
                Arc::new(|| {}),
                reader,
                |_tracker| Err(WindowsSettingsError::SubclassRejected),
            );

            match outcome {
                Err(error) => assert_eq!(error, WindowsSettingsError::SubclassRejected),
                Ok(_) => panic!("a refused subclass leaves no monitor to return"),
            }
            assert_eq!(
                reads.load(Ordering::SeqCst),
                0,
                "with nothing listening, the reconciling read would only go stale again"
            );
        }
    }

    mod theming {
        use super::*;
        use crate::app::ThemePreference;
        use crate::ui::theme::{resolve_theme, ResolvedTheme};

        /// The point of the whole module: a contrast or motion change taken
        /// from Windows has to reach the resolved style with no persistence,
        /// no approximation, and no second reading.
        #[test]
        fn high_contrast_and_reduced_motion_reach_the_resolved_theme_live() {
            let harness = TrackerUnderTest::new(CALM_LIGHT);
            let before = resolve_theme(
                ThemePreference::Dark,
                Some(egui::Theme::Dark),
                harness.cached().appearance,
            );
            assert_eq!(before.theme, ResolvedTheme::Dark);
            assert!(before.transition_seconds > 0.0);

            harness.set_reading(HIGH_CONTRAST_REDUCED_MOTION);
            harness.tracker.handle_message(WM_SETTINGCHANGE);

            let after = resolve_theme(
                ThemePreference::Dark,
                Some(egui::Theme::Dark),
                harness.cached().appearance,
            );
            assert_eq!(
                after.theme,
                ResolvedTheme::HighContrast,
                "High Contrast outranks every theme preference"
            );
            assert_eq!(
                after.transition_seconds, 0.0,
                "Reduced Motion has to take effect without a restart"
            );
            assert_eq!(after.tokens.canvas, egui::Color32::from_rgb(1, 2, 3));
            assert_eq!(after.tokens.ink, egui::Color32::from_rgb(4, 5, 6));
            assert_eq!(after.tokens.route, egui::Color32::from_rgb(7, 8, 9));
            assert_eq!(after.tokens.on_route, egui::Color32::from_rgb(10, 11, 12));
            assert_eq!(after.tokens.disabled, egui::Color32::from_rgb(13, 14, 15));
            assert_eq!(after.tokens.link, egui::Color32::from_rgb(16, 17, 18));
        }
    }

    /// Everything below needs a real window, a real message pump, and a real
    /// `comctl32` subclass chain, so it is opt-in.
    #[cfg(windows)]
    mod real_window {
        use super::*;

        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SendMessageW,
            UnregisterClassW, HWND_MESSAGE, WNDCLASSW, WS_OVERLAPPED,
        };

        unsafe extern "system" fn host_window_proc(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }

        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }

        #[test]
        #[ignore = "needs a real Win32 window and the comctl32 subclass chain"]
        fn windows_real_hwnd_attach_dispatches_once_and_detaches() {
            let class_name = wide("OpenAirCastSettingsMonitorTestWindow");
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(host_window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            let atom = unsafe { RegisterClassW(&class) };
            assert_ne!(atom, 0, "the test window class must register");

            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    class_name.as_ptr(),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "the test window must be created");

            let reads = Arc::new(AtomicUsize::new(0));
            let read_counter = Arc::clone(&reads);
            // Starts out matching the startup snapshot so the reconciling read
            // at attach time changes nothing; the broadcast below is then the
            // only thing that can move the cache.
            let reading = Arc::new(Mutex::new(CALM_LIGHT));
            let read_source = Arc::clone(&reading);
            let reader: SettingsReader = Arc::new(move || {
                read_counter.fetch_add(1, Ordering::SeqCst);
                *read_source.lock().unwrap()
            });
            let locales = Arc::new(RecordedLocales::default());
            let repaints = Arc::new(AtomicUsize::new(0));
            let repaint_counter = Arc::clone(&repaints);

            let handle = raw_window_handle::RawWindowHandle::Win32(
                raw_window_handle::Win32WindowHandle::new(
                    std::num::NonZeroIsize::new(hwnd as isize).expect("a created window is not 0"),
                ),
            );
            let monitor = attach_with_reader(
                handle,
                CALM_LIGHT,
                Arc::clone(&locales) as Arc<dyn WindowsSettingsEvents>,
                Arc::new(move || {
                    repaint_counter.fetch_add(1, Ordering::SeqCst);
                }),
                reader,
            )
            .expect("the subclass must install on a real window");

            assert_eq!(
                reads.load(Ordering::SeqCst),
                1,
                "attaching reconciles once against whatever happened during startup"
            );
            assert_eq!(*monitor.current(), CALM_LIGHT);

            *reading.lock().unwrap() = HIGH_CONTRAST_GERMAN;
            unsafe { SendMessageW(hwnd, WM_SETTINGCHANGE, 0, 0) };

            assert_eq!(reads.load(Ordering::SeqCst), 2, "one broadcast, one read");
            assert_eq!(*monitor.current(), HIGH_CONTRAST_GERMAN);
            assert_eq!(locales.0.lock().unwrap().clone(), [ResolvedLocale::German]);
            assert_eq!(repaints.load(Ordering::SeqCst), 1);

            // The owning thread is this one, so the subclass is removed here.
            drop(monitor);

            unsafe { SendMessageW(hwnd, WM_SETTINGCHANGE, 0, 0) };
            assert_eq!(
                reads.load(Ordering::SeqCst),
                2,
                "a detached callback must never run again"
            );

            assert_ne!(unsafe { DestroyWindow(hwnd) }, 0);
            assert_ne!(
                unsafe { UnregisterClassW(class_name.as_ptr(), instance) },
                0
            );
        }
    }
}
