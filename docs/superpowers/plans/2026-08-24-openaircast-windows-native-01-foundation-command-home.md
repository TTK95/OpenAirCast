# OpenAirCast Windows Native Foundation and Command Home Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` task by task. Use `superpowers:test-driven-development` for each behavior change and `superpowers:verification-before-completion` before every completion claim.

**Goal:** Deliver a startable bilingual Windows Native OpenAirCast GUI with exact Light, Dark, and Windows High Contrast behavior; responsive navigation; accessible controls; the complete receiver, master-volume, and start/stop Overview; and real Appearance, Language, Advanced-information, and shortcut controls in Settings.

**Architecture:** Preserve the frozen `AppHandle` boundary. App state owns locale and preference semantics, the platform layer owns Windows settings and font I/O, the UI catalog owns visible copy, presentation maps one immutable `UiSnapshot` per frame, and the App Shell owns navigation, scrolling, and the sticky change bar. Subproject 1 uses the integrated shell state; resilient backend projection remains Subproject 2.

**Stack:** Rust 2021/MSRV 1.95, egui/eframe 0.36.1, wgpu, AccessKit, egui_kittest, serde/serde_json, windows-sys 0.59, raw-window-handle 0.6, ArcSwap, Windows Segoe UI Variable, embedded Inter Variable, and embedded IBM Plex Mono.

**Approved spec:** `docs/superpowers/specs/2026-08-24-openaircast-windows-native-redesign-design.md`

## Global constraints

- Preserve `AppHandle::{dispatch,snapshot,drain_ui_effects,install_waker}`, `Page::Home`, typed reducer events, stable receiver IDs, one snapshot per frame, and non-blocking rendering.
- Use exact spec colors, geometry, typography, action priority, 900 by 600 minimum, 1120 by 720 default, 1000-point navigation breakpoint, High Contrast mapping, and Reduced Motion behavior.
- Never render raw backend/persistence summaries, paths, addresses, or protocol errors. Presentation maps typed context and `NoticeCode` to localized generic copy; concrete detail is logged only.
- Do not implement the resilient controller, diagnostics registry, calibration, export, or later backend projections in this subproject.
- Start every task with focused failing tests, finish with focused/regression verification, stage only listed paths, and make a scoped commit. Review fixes may add scoped commits.
- Stop after Task 10. Do not start Subproject 2 without a new explicit user instruction.

Run from the implementation worktree:

```powershell
$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) { (Get-Command cargo).Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
$rustfmt = if (Get-Command rustfmt -ErrorAction SilentlyContinue) { (Get-Command rustfmt).Source } else { Join-Path $env:USERPROFILE '.cargo\bin\rustfmt.exe' }
& $cargo --version
& $rustfmt --version
```

## Target structure

```text
crates/homepod-cast/src/platform/
├── appearance.rs
├── fonts.rs
├── hotkey.rs
├── mod.rs
└── windows_settings.rs
crates/homepod-cast/src/ui/
├── acceptance.rs
├── mod.rs
├── theme.rs
├── i18n.rs
├── layout.rs
├── presentation.rs
├── components/
│   ├── mod.rs
│   ├── app_shell.rs
│   ├── command_action.rs
│   ├── route_ribbon.rs
│   ├── receiver_card.rs
│   ├── audio_dock.rs
│   ├── notice.rs
│   ├── empty_state.rs
│   ├── metric_tile.rs
│   └── change_bar.rs
└── pages/
    ├── mod.rs
    ├── overview.rs
    ├── speakers.rs
    ├── groups.rs
    ├── audio.rs
    ├── diagnostics.rs
    └── settings.rs
```

`ui/home.rs`, `ui/navigation.rs`, and `ui/pages.rs` remain compilable until Task 8 replaces them. Task 8 removes all three while adding `ui/pages/mod.rs`; Rust must never see `pages.rs` and `pages/mod.rs` together.

---

### Task 1: Establish one exact theme, motion, and font contract

**Files:** Modify `crates/homepod-cast/src/ui/theme.rs`, `crates/homepod-cast/src/ui/mod.rs`, `crates/homepod-cast/src/ui/home.rs`, `crates/homepod-cast/src/ui/navigation.rs`, `crates/homepod-cast/src/ui/pages.rs`, `crates/homepod-cast/src/platform/mod.rs`, `crates/homepod-cast/src/platform/hotkey.rs`, `crates/homepod-cast/src/app/mod.rs`, `crates/homepod-cast/Cargo.toml`, `Cargo.lock`; create `crates/homepod-cast/src/platform/appearance.rs`, `crates/homepod-cast/src/platform/fonts.rs`.

**Exact interfaces:**

```rust
pub struct SystemColors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub highlight: [u8; 3],
    pub highlight_text: [u8; 3],
    pub disabled_text: [u8; 3],
    pub link: [u8; 3],
}

pub struct SystemAppearance {
    pub client_animation_enabled: bool,
    pub high_contrast: bool,
    pub colors: SystemColors,
}

pub enum ResolvedTheme { Light, Dark, HighContrast }

pub struct ResolvedThemeStyle {
    pub theme: ResolvedTheme,
    pub tokens: ThemeTokens,
    pub transition_seconds: f32,
}

pub fn resolve_theme(
    preference: ThemePreference,
    system_theme: Option<egui::Theme>,
    appearance: SystemAppearance,
) -> ResolvedThemeStyle;

pub fn apply_theme(ctx: &egui::Context, style: &ResolvedThemeStyle);
pub fn load_segoe_ui_variable() -> Option<std::sync::Arc<[u8]>>;
```

`ThemeTokens` contains every spec role: `canvas`, `surface`, `surface_subtle`, `ink`, `ink_muted`, `border`, `route`, `on_route`, `route_soft`, `live`, `live_soft`, `warning`, `warning_soft`, `fault`, `fault_soft`, `focus`, `disabled`, `link`, and `focus_width`.

- [ ] Write RED tests for every exact Light/Dark token, all text pairs at 4.5 to 1, all non-text pairs at 3 to 1, disabled/link, two-point normal focus, three-point High Contrast focus, and zero duration with client animation disabled.
- [ ] Test precedence: High Contrast overrides all preferences; otherwise explicit Light/Dark overrides the OS; System uses `ctx.system_theme()`; missing OS theme falls back to Light.
- [ ] In `platform/fonts.rs`, test pure Windows-directory-to-`Fonts\SegUIVar.ttf` path construction, missing-file fallback, and `ControlCenterApp::new(None)` without filesystem I/O. Update the old hotkey smoke test from `MotionPreferences` to `SystemAppearance`.
- [ ] Run RED:

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::theme::tests
& $cargo test -p homepod-cast --bin openaircast platform::fonts::tests
& $cargo test -p homepod-cast --bin openaircast platform::appearance::tests
```

- [ ] Replace `MotionPreferences` with `SystemAppearance`. Read `SPI_GETCLIENTAREAANIMATION`, `SPI_GETHIGHCONTRAST`, and `GetSysColor` values for `COLOR_WINDOW`, `COLOR_WINDOWTEXT`, `COLOR_HIGHLIGHT`, `COLOR_HIGHLIGHTTEXT`, `COLOR_GRAYTEXT`, and `COLOR_HOTLIGHT`.
- [ ] High Contrast maps background to canvas/surfaces/soft roles, foreground to ink/border/live/warning/fault, highlight to route/focus, highlight text to on-route, gray text to disabled, and hotlight to link. No hard-coded application accent survives. Transition duration is 0.12 seconds when enabled and 0 otherwise. `apply_theme` never resolves preferences again.
- [ ] Implement `load_segoe_ui_variable` with `GetWindowsDirectoryW`, add `Win32_System_SystemInformation`, read before `run_native`, and pass `Option<Arc<[u8]>>` into `ControlCenterApp::new`. Install Segoe first when present, embedded Inter next, IBM Plex Mono first for measurements. No frame-time font I/O.
- [ ] Install approved typography 28/34, 20/26, 16/22, 14/20, button 14/18, 12/17, 11/15, mono 12/16; spacing/radii/borders/control minimums from the spec.
- [ ] Remove all active `LIGHT_PALETTE` use from `ui/mod.rs`, `home.rs`, `navigation.rs`, and `pages.rs`. Cache `(ThemePreference, ctx.system_theme(), SystemAppearance)` and reapply only when it changes.
- [ ] Verify, format listed Rust files, run `git diff --check`, stage only listed files, and commit `feat(ui): add Windows Native semantic themes`.

---

### Task 2: Add the complete German and English catalog

**Files:** Modify `crates/homepod-cast/src/app/state.rs`, `crates/homepod-cast/src/ui/mod.rs`; create `crates/homepod-cast/src/ui/i18n.rs`.

**App-owned locale types:**

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalePreference { #[default] System, German, English }

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResolvedLocale { German, #[default] English }

impl LocalePreference {
    pub const fn resolve(self, windows: ResolvedLocale) -> ResolvedLocale {
        match self {
            Self::System => windows,
            Self::German => ResolvedLocale::German,
            Self::English => ResolvedLocale::English,
        }
    }
}
```

**Complete visible Subproject-1 `TextKey` inventory:**

```rust
pub enum TextKey {
    Overview, Speakers, Groups, Audio, Diagnostics, Settings,
    Status, Receivers, AudioRoute, SelectedChanges,
    Ready, Connecting, Streaming, Reconnecting, Stopping, NeedsAttention,
    StartStreaming, StopStreaming, Cancel, StopTrying, Retry, Refresh,
    ApplyChanges, Discard, Dismiss,
    WindowsAudio, RouteIdle, RoutePending, RouteLive, RouteFault,
    RouteToSelectedReceivers, AvailableReceivers, DiscoveringReceivers,
    RefreshReceivers, NoReceiversTitle, NoReceiversBody,
    ReceiverAvailable, ReceiverUnavailable, ReceiverSelected, ReceiverActive,
    ReceiverSelectedAndActive, SelectReceiver, DeselectReceiver,
    MasterVolume, MasterVolumeAccessibility, ChangeSummary,
    SpeakersEmptyTitle, SpeakersEmptyBody, GroupsEmptyTitle, GroupsEmptyBody,
    AudioEmptyTitle, AudioEmptyBody, DiagnosticsEmptyTitle, DiagnosticsEmptyBody,
    AdvancedMetricsUnavailable,
    Appearance, AppearanceDescription, ThemeSystem, ThemeLight, ThemeDark,
    HighContrastControlledByWindows,
    Language, LanguageDescription, LanguageSystem, LanguageGerman, LanguageEnglish,
    Keyboard, GlobalShortcut, GlobalShortcutDescription, ShortcutEnabled,
    ShortcutControl, ShortcutAlt, ShortcutShift, ShortcutWindows,
    ShortcutKeyCode, ShortcutKeyCodeAccessibility, ShortcutKeyCodeTooltip,
    ApplyShortcut, ActiveShortcut, ShortcutDisabled, ShortcutChangePending,
    ShortcutNeedsModifier, ShortcutInvalidKey,
    AdvancedInformation, AdvancedInformationDescription, On, Off,
    About, AboutDescription, Version, License, ThirdPartyNotices,
    OpenLicense, OpenThirdPartyNotices, CopyLicenseText, CopyNoticesText,
    CloseAboutViewer, CopiedToClipboard,
    Available, Unavailable, Unknown, Stale, Selected, Active,
    PreferenceSaveFailed, PreferenceSaveConsequence, PreferenceRetry,
    NoReceiverAvailableNotice, DiscoveryFailedNotice, SessionFailedNotice,
    ControllerUnavailableNotice, HotkeyFailedNotice, InvalidInputNotice,
    CurrentSettingsRemainActive, TryRefreshNext, TryRetryNext, OpenSettingsNext,
    CompactDestinationTooltip, NavigationDestinationAccessibility,
    RouteSummaryAccessibility, ReceiverSelectionAccessibility,
    VolumeAccessibility, StatusFooterAccessibility,
}
```

`TextKey::ALL` contains each variant exactly once in declaration order. `Catalog` owns only `ResolvedLocale`. Dynamic sentences use exact named structs: `ReceiverCountArgs { count }`, `ChangeSummaryArgs { added, removed }`, `RouteSummaryArgs { selected, active }`, `PercentageArgs { percent }`, `DurationArgs { milliseconds }`, and `NamedValueArgs { name, value }`. Exact methods are `new`, `locale`, `text`, `receiver_count`, `change_summary`, `route_summary`, `percentage`, `duration`, and `named_value`.

- [ ] Write RED tests that every key is nonblank in both locales, coverage is identical, `ALL` has no duplicates, core approved vocabulary is exact, and zero/singular/plural/percentage/duration formatting is locale-correct.
- [ ] Test copy safety with raw path/address/error sentinels; notice formatting has no argument capable of accepting raw error text. Mixed German/English is forbidden except AirPlay, HomePod, PTP, WASAPI, OpenAirCast, Ctrl, Alt, Shift, and Win.
- [ ] Run `& $cargo test -p homepod-cast --bin openaircast ui::i18n::tests` to verify RED.
- [ ] Implement exhaustive `(ResolvedLocale, TextKey)` matches without wildcard arms. Components/pages receive keys or resolved copy and never embed language-specific sentences.
- [ ] Verify, format, `git diff --check`, stage the three files, and commit `feat(ui): add complete German and English catalogs`.

---

### Task 3: Migrate shell preferences and state to schema v2

This task owns migration and optimistic state only. Completion feedback and Retry belong to Task 4.

**Files:** Modify `crates/homepod-cast/src/app/state.rs`, `crates/homepod-cast/src/app/event.rs`, `crates/homepod-cast/src/app/effect.rs`, `crates/homepod-cast/src/app/reducer.rs`, `crates/homepod-cast/src/app/snapshot.rs`, `crates/homepod-cast/src/preferences.rs`, `crates/homepod-cast/src/app/mod.rs`.

```rust
pub const PREFERENCES_SCHEMA_VERSION: u32 = 2;

pub struct Preferences {
    pub schema_version: u32,
    pub hotkey: HotkeyBinding,
    pub theme: ThemePreference,
    pub locale: LocalePreference,
    pub advanced_information: bool,
    pub window: WindowGeometry,
    pub last_page: Page,
    pub close_to_tray: bool,
    pub launch_at_startup: bool,
}
```

Add `locale_preference`, internal `windows_display_locale`, `resolved_locale`, and `advanced_information` to `AppState`. Publish only `locale_preference`, `resolved_locale`, and `advanced_information` in `UiSnapshot`. Do not add a separate persistence-status field. Add these exact variants without renaming existing ones:

```rust
LocaleChanged(LocalePreference),
AdvancedInformationChanged(bool),
WindowsDisplayLanguageChanged(ResolvedLocale),
```

- [ ] Write RED repository tests using literal v1 JSON with every old field. A private `SchemaProbe { schema_version }` branches to private `PreferencesV1` for version 1 or public `Preferences` for version 2; every other version is rejected. Migration preserves hotkey, theme, geometry, `Page::Home`, close-to-tray, and launch-at-startup and defaults locale to System and Advanced to false.
- [ ] Assert load performs no immediate rewrite, next successful save writes deterministic v2, v2 round-trips, and `save_atomic` returns typed `InvalidSchemaVersion` before filesystem mutation for a non-v2 public value.
- [ ] Write RED reducer/snapshot tests: locale and Advanced changes update memory and schedule the complete v2 value; System resolves cached Windows locale; OS-language changes always update the internal cache but change published locale only under System and never persist; repeated values are no-ops; snapshot fields are exhaustive; startup honors `loaded.value.launch_at_startup` instead of forcing false.
- [ ] Run:

```powershell
& $cargo test -p homepod-cast --bin openaircast preferences::tests
& $cargo test -p homepod-cast --bin openaircast app::reducer::tests
& $cargo test -p homepod-cast --bin openaircast app::snapshot::tests
& $cargo test -p homepod-cast --bin openaircast app::tests::lifecycle
```

- [ ] Implement one helper that builds the whole current v2 preference value so no change drops unrelated fields. Write only v2 atomically.
- [ ] Verify, format listed files, `git diff --check`, stage only listed files, and commit `feat(app): migrate shell preferences to version two`.

---

### Task 4: Add generation-safe preference outcomes and scoped Retry

**Files:** Modify `crates/homepod-cast/src/app/state.rs`, `crates/homepod-cast/src/app/event.rs`, `crates/homepod-cast/src/app/reducer.rs`, `crates/homepod-cast/src/app/mod.rs`, `crates/homepod-cast/src/preferences.rs`, `crates/homepod-cast/src/ui/home.rs`.

Keep existing `CorrectiveAction::Retry` unchanged. Add only `CorrectiveAction::RetryPreferencesPersistence` and `AppEvent::RetryPreferencesPersistence`. Outcomes are `PreferencesEvent::Persisted { generation }` and `PreferencesEvent::PersistFailed { generation }`; raw summaries are removed. Exact worker signature:

```rust
pub(crate) fn spawn_preferences_worker(
    repository: PreferencesRepository,
    events: crate::app_handle::AppEventSender,
) -> PreferencesWorkerHandle;
```

- [ ] Write RED reducer tests for matching failure, stale failure, stale success, matching success clearing only a scoped persistence notice, current-full-v2 Retry at the next generation, and retained optimistic in-memory values/prior durable file. Retry must never dispatch Start, Refresh, or controller work.
- [ ] Write RED worker/executor tests for write success, write failure, best-effort event-send failure, executor Busy, executor Closed, flush/shutdown, and lower-generation debounce. Busy/Closed emit preference failure at the attempted generation, never `ControllerEvent::ChannelClosed`. A lower offer preserves both the newer pending generation and its value.
- [ ] Update legacy Home kittest: scoped preference Retry dispatches `RetryPreferencesPersistence`; existing lifecycle Retry retains its existing behavior.
- [ ] Run:

```powershell
& $cargo test -p homepod-cast --bin openaircast preferences::tests
& $cargo test -p homepod-cast --bin openaircast app::reducer::tests
& $cargo test -p homepod-cast --bin openaircast app::tests::lifecycle
& $cargo test -p homepod-cast --bin openaircast ui::home::tests
```

- [ ] Pass `service_events.clone()` to the worker. Log concrete I/O errors there, then emit best-effort typed outcomes. App/reducer/preferences/platform code never imports `TextKey`. Use `state.generations.preferences` as the sole authoritative whole-preferences generation.
- [ ] Verify, format, `git diff --check`, stage only listed files, and commit `feat(app): report preference persistence outcomes`.

---

### Task 5: Observe Windows locale, contrast, colors, and motion without polling

**Files:** Create `crates/homepod-cast/src/platform/windows_settings.rs`; modify `crates/homepod-cast/src/platform/mod.rs`, `crates/homepod-cast/src/platform/appearance.rs`, `crates/homepod-cast/src/app/mod.rs`, `crates/homepod-cast/src/ui/mod.rs`, `crates/homepod-cast/Cargo.toml`, and `Cargo.lock`.

```rust
pub struct WindowsSettingsSnapshot {
    pub locale: ResolvedLocale,
    pub appearance: SystemAppearance,
}

pub struct WindowsSettingsMonitor {
    current: std::sync::Arc<arc_swap::ArcSwap<WindowsSettingsSnapshot>>,
}

impl WindowsSettingsMonitor {
    pub fn current(&self) -> std::sync::Arc<WindowsSettingsSnapshot>;
}

pub fn read_windows_settings() -> WindowsSettingsSnapshot;

pub fn attach_windows_settings_monitor(
    window_handle: raw_window_handle::RawWindowHandle,
    initial: WindowsSettingsSnapshot,
    events: crate::app_handle::AppEventSender,
    request_repaint: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Result<WindowsSettingsMonitor, WindowsSettingsError>;
```

The production monitor may add a private Windows attachment field. Tests use a private injected reader.

- [ ] RED pure tests: `GetUserPreferredUILanguages(MUI_LANGUAGE_NAME)` first tag `de`, `de-DE`, or `de-AT` maps German; any other tag, empty response, or failure maps English. Test SPI animation/contrast and all six colors through injection. Tracker tests cover unchanged, locale-only, appearance-only, combined, exactly one cache swap, locale event only on locale change, one repaint per complete change, and none after detach.
- [ ] Add `#[cfg(windows)] #[ignore]` test `windows_real_hwnd_attach_dispatches_once_and_detaches`. Register a minimal Win32 window, create a real HWND, attach with deterministic reader, send `WM_SETTINGCHANGE`, assert one update/dispatch, drop on the window thread, send again, assert no callback, then destroy and unregister.
- [ ] Run RED:

```powershell
& $cargo test -p homepod-cast --bin openaircast platform::windows_settings::tests
& $cargo test -p homepod-cast --bin openaircast platform::windows_settings::tests::windows_real_hwnd_attach_dispatches_once_and_detaches -- --ignored --exact
```

- [ ] Read initial Windows settings after preferences load but before AppState, workers, actor, or first paint. Initialize cached Windows locale, resolved locale, and appearance from it. Persisted explicit language overrides Windows; unsupported languages fall back to English.
- [ ] In the eframe creation closure, use `raw_window_handle::HasWindowHandle` on `CreationContext`, attach to the real HWND, and retain the monitor in `ControlCenterApp` for its full lifetime.
- [ ] Use `SetWindowSubclass` for `WM_SETTINGCHANGE` and `WM_THEMECHANGED`. Callback catches panics, calls `DefSubclassProc`, keeps state alive, and shares an exactly-once detach/free guard between `WM_NCDESTROY` and `Drop`. Removal/free occurs on the owning UI thread; failed attach frees state; detached callbacks cannot access it.
- [ ] UI cache key is `(snapshot.theme, ctx.system_theme(), monitor.current().appearance)`. High Contrast overrides all; Reduced Motion updates live. No timer, per-frame Win32 read, persisted OS color, or actor appearance event.
- [ ] Add `raw-window-handle = "0.6"`, `Win32_Globalization`, and `Win32_UI_Shell`; retain `Win32_System_SystemInformation` and existing features.
- [ ] Verify focused tests plus theme and app lifecycle; format, `git diff --check`, stage listed files, commit `feat(platform): observe Windows display settings`.

---

### Task 6: Build reusable accessible control primitives

**Files:** Create `crates/homepod-cast/src/ui/presentation.rs`, `crates/homepod-cast/src/ui/components/mod.rs`, `crates/homepod-cast/src/ui/components/command_action.rs`, `crates/homepod-cast/src/ui/components/notice.rs`, `crates/homepod-cast/src/ui/components/empty_state.rs`, `crates/homepod-cast/src/ui/components/metric_tile.rs`, `crates/homepod-cast/src/ui/components/change_bar.rs`; modify `crates/homepod-cast/src/ui/mod.rs`.

```rust
pub enum LifecycleAction { Start, Cancel, Stop, StopTrying, Retry, DisabledStopping }
pub enum FilledActionOwner { Lifecycle, Apply, None }
pub struct ChangeBarModel {
    pub summary: String,
    pub apply_filled: bool,
    pub apply_enabled: bool,
}
pub fn filled_action_owner(
    lifecycle: Option<LifecycleAction>,
    staged_dirty: bool,
    apply_safe: bool,
) -> FilledActionOwner;
```

- [ ] RED egui_kittest tests: 40-point commands, visible disabled state using semantic disabled token, keyboard activation, accessible names, Notice severity symbol/text, at most one real Empty State action, Metric Tile label/value/unit/freshness/combined phrase, and text-plus-symbol Unknown/Stale.
- [ ] Test action priority: Start/Cancel/Stop/StopTrying/Retry owns the sole filled action whenever an enabled lifecycle command is present; Apply becomes filled only when dirty, stopped/safe, and no lifecycle command owns priority; Stopping has no enabled filled command.
- [ ] Test Change Bar dispatches exactly Apply or Discard but owns no panel, fixed position, scroll region, or page selection. Shell placement belongs to Task 7.
- [ ] Run `& $cargo test -p homepod-cast --bin openaircast ui::components` to verify RED.
- [ ] Implement pure models/renderers. Map `NoticeCode` and corrective action to catalog keys without `UserNotice.summary`; preference failure maps to failure, consequence, and scoped Retry keys. Components accept tokens/catalog/model/typed event sink only.
- [ ] Verify components and presentation tests, format, `git diff --check`, stage listed files, commit `feat(ui): add accessible Windows Native controls`.

---

### Task 7: Build the responsive App Shell and exhaustive dispatcher

**Files:** Create `crates/homepod-cast/src/ui/layout.rs`, `crates/homepod-cast/src/ui/components/app_shell.rs`, `crates/homepod-cast/src/ui/pages/overview.rs`, `crates/homepod-cast/src/ui/pages/speakers.rs`, `crates/homepod-cast/src/ui/pages/groups.rs`, `crates/homepod-cast/src/ui/pages/audio.rs`, `crates/homepod-cast/src/ui/pages/diagnostics.rs`, `crates/homepod-cast/src/ui/pages/settings.rs`; modify `crates/homepod-cast/src/ui/components/mod.rs`, `crates/homepod-cast/src/ui/pages.rs`, `crates/homepod-cast/src/ui/mod.rs`.

```rust
pub enum RailMode { Expanded, Compact }
pub struct LayoutMetrics {
    pub rail_mode: RailMode,
    pub rail_width: f32,
    pub page_inset: f32,
    pub receiver_columns: usize,
}
pub struct UiResources<'a> { pub tokens: &'a ThemeTokens, pub catalog: Catalog }
pub fn show(
    page: Page,
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
);
pub fn show_shell(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    change_bar: Option<&ChangeBarModel>,
    emit: &mut dyn FnMut(AppEvent),
);
```

- [ ] RED tests: rail 176/64, breakpoint 1000, insets 24/16, columns two/one, six fixed localized destinations, 44-point targets, correct Home title, one exhaustive renderer per `Page`, and containment at both target sizes.
- [ ] Shell-ownership test: dirty membership yields one fixed bottom Change Bar on every destination, after scrolling, with reserved height and Apply/Discard last in accessibility/tab order. No page calls `change_bar::show`.
- [ ] Run layout, `components::app_shell::tests`, and `ui::pages::tests` RED.
- [ ] Implement semantic navigation, compact localized tooltips/names, true status footer, native title bar, page header outside scroll, and Shell-owned Change Bar.
- [ ] Keep `ui/pages.rs` as sole module root in this task and declare child modules. Overview may delegate to legacy Home. Move truthful theme/hotkey/About Settings content; Task 9 adds locale/Advanced. Other pages show localized honest empty states only; no fake saved groups, endpoint, calibration, export, metric, or connection controls.
- [ ] `ControlCenterApp::ui` drains effects, takes one snapshot, resolves theme/catalog once, derives shell chrome once, and calls shell once. Pages receive no handle.
- [ ] Verify focused and existing navigation tests, format, `git diff --check`, stage listed files, commit `feat(ui): add the responsive Windows Native shell`.

---

### Task 8: Build Route Ribbon, Receiver Card, Audio Dock, and Command Home

**Files:** Modify `crates/homepod-cast/src/ui/presentation.rs`, `crates/homepod-cast/src/ui/components/mod.rs`, `crates/homepod-cast/src/ui/components/app_shell.rs`, `crates/homepod-cast/src/ui/pages/overview.rs`, `crates/homepod-cast/src/ui/mod.rs`; create `crates/homepod-cast/src/ui/components/route_ribbon.rs`, `crates/homepod-cast/src/ui/components/receiver_card.rs`, `crates/homepod-cast/src/ui/components/audio_dock.rs`, `crates/homepod-cast/src/ui/pages/mod.rs`; delete `crates/homepod-cast/src/ui/home.rs`, `crates/homepod-cast/src/ui/navigation.rs`, `crates/homepod-cast/src/ui/pages.rs` after parity.

```rust
pub enum SessionVisualPhase {
    Ready, Connecting, Streaming, Degraded, Restarting, Stopping, Failed,
}
pub struct OverviewModel {
    pub phase: SessionVisualPhase,
    pub title: String,
    pub explanation: String,
    pub primary: Option<LifecycleAction>,
    pub route: RouteRibbonModel,
    pub receivers: Vec<ReceiverCardModel>,
    pub audio: AudioDockModel,
    pub notice: Option<NoticeModel>,
    pub advanced_metrics: Vec<MetricTileModel>,
}
impl OverviewModel {
    pub fn from_snapshot(snapshot: &UiSnapshot, catalog: Catalog) -> Self;
}
```

Overview never owns the sticky Change Bar.

- [ ] RED pure mapping tests cover the currently representable Stopped/Starting/Streaming/Stopping/Failed states, selected/active/unavailable receivers, dirty stage, volume, all notices, both locales, and Standard/Advanced. Degraded/Restarting and running-intent Failed fixtures remain reserved for Subproject 2 because Subproject 1 has no authoritative run-intent projection. Raw summaries never enter models; Unknown never becomes zero.
- [ ] RED component/page tests cover accessible Route summary and `3 + N` collapse, stable receiver ID across reorder, checkbox semantics/states, authoritative master volume and keyboard increments, exact Overview order, one/two columns, and exact typed dispatch for Start/Cancel/Stop/stopped Retry/Refresh/receiver/volume plus shell-owned Apply/Discard.
- [ ] Run presentation, three component, and Overview tests RED.
- [ ] Implement pure mapping without I/O/clock. The currently representable stopped Failed state maps Retry to `StartRequested`. Running-intent Failed/StopTrying, Degraded, and Restarting remain reserved for Subproject 2.
- [ ] Allow only one bounded 120 ms success sweep when motion is enabled; no indefinite animation or repeating timer. High Contrast retains text/symbol/line distinctions.
- [ ] Atomically replace module roots: delete `pages.rs`, add `pages/mod.rs`, then delete legacy Home/navigation only after parity tests.
- [ ] Verify all `ui::` tests, format, `git diff --check`, stage exact files including deletions, commit `feat(ui): build the Windows Native Command Home`.

---

### Task 9: Complete Settings with Appearance, Language, Advanced information, and shortcut controls

**Files:** Modify `crates/homepod-cast/src/ui/pages/settings.rs`, `crates/homepod-cast/src/ui/presentation.rs`, `crates/homepod-cast/src/ui/i18n.rs`, `crates/homepod-cast/src/ui/components/notice.rs`.

- Appearance offers System/Light/Dark and dispatches `ThemeChanged`; Windows High Contrast is localized read-only explanation, never a fourth preference.
- Language offers System/Deutsch/English and dispatches `LocaleChanged`; selection uses `locale_preference`, copy uses `resolved_locale`.
- Advanced information is a persistent switch dispatching `AdvancedInformationChanged`.
- About embeds the repository files at compile time with `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../LICENSE"))` and `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../THIRD_PARTY_NOTICES.md"))`; it never copies or renders a build/user path. Open actions show the full embedded text in a keyboard-dismissible in-app viewer, and Copy actions place that same embedded text on the clipboard. Every shown action is real, localized, and keyboard accessible.
- Do not add Behavior or Connection controls without a working typed shell event/effect end to end.

- [ ] RED interaction tests in both locales click every Appearance/Language choice and Advanced toggle and assert exactly one typed event. System remains selected when it resolves German; explicit English remains selected after Windows changes German.
- [ ] RED accessibility tests cover localized groups, radio roles/states, switch state, High Contrast explanation, stable keyboard order, 40-point targets, both sizes, shortcut validation/Escape/apply, embedded About Open/Copy for both license and notices without paths, and scoped persistence Retry dispatch.
- [ ] Run settings and i18n tests RED.
- [ ] Implement groups in supported spec order: Appearance, Language, Keyboard, Advanced information, About. Use catalog copy only, stable egui draft IDs, and snapshot reconciliation. No repository, Windows API, or handle access.
- [ ] Verify settings/i18n/notice/reducer tests, format, `git diff --check`, stage listed files, commit `feat(ui): complete localized Windows Native settings`.

---

### Task 10: Verify the private binary UI, launch, document, review, and stop

**Files:** Create `crates/homepod-cast/src/ui/acceptance.rs`; modify `crates/homepod-cast/src/ui/mod.rs`, `docs/VALIDATION.md`, `docs/NEXT_STEPS.md`; modify task-owned code only when a failing acceptance assertion identifies the owner.

Add `#[cfg(test)] mod acceptance;` inside private `ui/mod.rs`. Do not create an integration test under `crates/homepod-cast/tests`: binary-private app/UI modules are not accessible there.

- [ ] Add table-driven egui_kittest coverage for Light/Dark, German/English, Standard/Advanced, 1120 by 720/900 by 600, all six pages, current Home lifecycle states, and Settings Theme/Locale/Advanced/shortcut/scoped Retry events.
- [ ] Every case asserts six localized destinations, correct title, at most one filled action, no overflow/change-bar overlap, correct AccessKit names/roles/states/tab order, stable receiver identity, no raw sentinel/path/address/summary, Shell-only Change Bar, no Standard/Advanced action-position drift, and no mixed language.
- [ ] Run `& $cargo test -p homepod-cast --bin openaircast ui::acceptance::tests` RED, fix only owning Subproject-1 code, rerun its focused tests and matrix.
- [ ] Commit acceptance files as `test(ui): add private Windows Native acceptance matrix`.
- [ ] Run asserting release verification:

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::acceptance::tests
if ($LASTEXITCODE -ne 0) { throw 'private UI acceptance failed' }
& $cargo test -p homepod-cast
if ($LASTEXITCODE -ne 0) { throw 'package tests failed' }
& $cargo check --workspace --all-targets
if ($LASTEXITCODE -ne 0) { throw 'workspace check failed' }
& $cargo clippy -p homepod-cast --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'clippy failed' }
& $cargo build -p homepod-cast --release
if ($LASTEXITCODE -ne 0) { throw 'release build failed' }

$forbidden = rg -n 'LIGHT_PALETTE|System volume|Systemlautstärke' crates/homepod-cast/src/ui
if ($LASTEXITCODE -eq 0) { $forbidden; throw 'forbidden legacy palette or volume wording remains' }
if ($LASTEXITCODE -ne 1) { throw 'forbidden-copy scan failed' }

$rawCopy = rg -n 'notice\.summary|summary\.clone\(\)' crates/homepod-cast/src/ui
if ($LASTEXITCODE -eq 0) { $rawCopy; throw 'raw notice copy enters UI code' }
if ($LASTEXITCODE -ne 1) { throw 'raw-copy scan failed' }

$repaints = rg -n 'request_repaint_after|request_repaint\(\)' crates/homepod-cast/src
if ($LASTEXITCODE -gt 1) { throw 'repaint scan failed' }
if (-not $repaints) { throw 'expected bounded repaint sources were not found' }
$unexpected = $repaints | Where-Object {
    $_ -notmatch 'src[\\/]ui[\\/]mod\.rs' -and
    $_ -notmatch 'src[\\/]platform[\\/]windows_settings\.rs' -and
    $_ -notmatch 'src[\\/]ui[\\/]components[\\/]route_ribbon\.rs'
}
if ($unexpected) { $unexpected; throw 'unexpected repaint source found' }

git diff --check
if ($LASTEXITCODE -ne 0) { throw 'git diff check failed' }
$appExe = (Resolve-Path 'target\release\openaircast.exe').Path
$appHash = (Get-FileHash -Algorithm SHA256 $appExe).Hash
if ($appHash -notmatch '^[0-9A-F]{64}$') { throw 'release SHA256 is invalid' }
Write-Output $appExe
Write-Output "SHA256 $appHash"
```

- [ ] Run the real-HWND test and launch:

```powershell
& $cargo test -p homepod-cast --bin openaircast platform::windows_settings::tests::windows_real_hwnd_attach_dispatches_once_and_detaches -- --ignored --exact
if ($LASTEXITCODE -ne 0) { throw 'real HWND monitor test failed' }
Start-Process -FilePath $appExe
```

- [ ] Manually verify both sizes/locales; System/Light/Dark/High Contrast; Reduced Motion; keyboard receiver/Apply/Discard/volume/Start/Stop/Retry/Language/Theme/Advanced/shortcut; Narrator; compact tooltips; tray hide/restore; native title bar. Record only observed passes in `docs/VALIDATION.md`; mark unperformed Windows/hardware combinations unverified.
- [ ] Request independent read-only Spec and Code review from the parent of the Task-1 font creation commit through current HEAD. Critical/Important findings return to owning focused tests, full verification, scoped fix commit, and re-review.
- [ ] Derive the base and list every Subproject-1 implementation, acceptance, and review-fix commit through the reviewed pre-evidence HEAD; never assume a fixed count. The later evidence commit is reported separately and is not self-listed:

```powershell
$taskOneCommit = git log --diff-filter=A --format='%H' -- crates/homepod-cast/src/platform/fonts.rs | Select-Object -Last 1
if (-not $taskOneCommit) { throw 'Task 1 creation commit was not found' }
$subprojectBase = git rev-parse "$taskOneCommit^"
if ($LASTEXITCODE -ne 0) { throw 'Subproject 1 base could not be resolved' }
$subprojectCommits = git log --reverse --format='%H %s' "$subprojectBase..HEAD"
if (-not $subprojectCommits) { throw 'Subproject 1 commit list is empty' }
$subprojectCommits
```

- [ ] Update `docs/NEXT_STEPS.md` with base, the complete pre-evidence commit list, reviewed pre-evidence HEAD, exact commands/results, actual manual results, remaining dirty backend paths, executable path/hash, deferred hardware checks, and exact line `STOP: Windows Native Subproject 1 complete; Subproject 2 not started.`
- [ ] Commit evidence as `docs(ui): record Windows Native validation evidence`. Record that evidence commit ID in the final report separately. Re-run `git diff --check`, resolve release path, recompute SHA256, and assert it equals the documented `$appHash`.
- [ ] Report reviewed HEAD, executable, SHA256, tests, actual manual passes, and deferrals. Do not dispatch an agent, edit a file, or run an implementation command for Subproject 2.

## Subproject 1 coverage

- Theme, High Contrast, typography, Windows font, geometry, Reduced Motion: Tasks 1 and 5.
- Complete localization, first-run/live Windows locale: Tasks 2, 3, and 5.
- v2 migration, optimistic locale/Advanced state, outcomes, scoped Retry: Tasks 3 and 4.
- Accessible controls, responsive six-page shell, Shell-owned Change Bar: Tasks 6 and 7.
- Route Ribbon, receiver selection, master volume, lifecycle actions, Command Home: Task 8.
- Appearance, Language, Advanced, shortcut, High Contrast explanation, About: Task 9.
- Private-binary matrix, real HWND, release/hash, manual evidence, review, full commit list, hard stop: Task 10.
- Saved groups, resilient receiver lifecycle, endpoint, mute, latency, auto-connect, per-receiver level/Retry, and full backend phase mapping remain Subproject 2.
- Diagnostics detail, calibration, copy/export, and bounded diagnostics polling remain Subproject 3.

STOP: Windows Native Subproject 1 complete; Subproject 2 not started.
