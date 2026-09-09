# OpenAirCast Control Center Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the tray-only/raw-Win32 shell with the approved, accessible egui/eframe Control Center while preserving the proven AirPlay capture and transport paths and keeping tray, hotkey, persistence, and shutdown behavior driven by one authoritative application state.

**Architecture:** A single reducer actor owns `AppState`; widgets, tray callbacks, and the Win32 hotkey thread dispatch `AppEvent`; the pure reducer returns `AppEffect`; non-blocking ports execute those effects outside the UI/reducer thread; immutable `Arc<UiSnapshot>` revisions feed both eframe and tray. The existing Tokio/AirPlay loop moves behind a private legacy device-service adapter shaped so subproject 2 can replace it with `DeviceBackendHandle`/`DeviceSnapshot` without changing the public `AppHandle` contract.

**Tech Stack:** Rust 2021 (MSRV 1.95), egui/eframe 0.36.1 with wgpu and AccessKit, tray-icon 0.24.0, raw Win32 only for the isolated global-hotkey/motion-preference services, Tokio for the private AirPlay controller thread, serde/serde_json for versioned settings, ArcSwap for atomic snapshots, egui_kittest for headless accessibility tests, Inter Variable and IBM Plex Mono under SIL OFL 1.1.

**Spec:** `docs/superpowers/specs/2026-08-22-openaircast-control-center-shell-design.md`

## Global Constraints

- The linked design spec is authoritative. Subproject 1 implements the complete shell, Home daily controls, all six destinations, tray coexistence, settings, accessibility, and lifecycle behavior; it does not implement continuous discovery, reconnect, saved-group CRUD, per-speaker volume, selectable audio endpoints, latency presets, or invented diagnostics.
- Preserve `openaircast.exe --list`, `--selftest`, and `--selftest-group` behavior and the existing one-capture single-/multi-receiver path. Do not change protocol, pairing, capture, timing, or packet behavior in this plan.
- Use stable `airplay_core::DeviceId` values everywhere in shell state. No new vector-index identity may cross `device_service`, reducer, snapshot, UI, or tray boundaries.
- The eframe thread performs rendering and tray object mutation only. It never performs filesystem, discovery, network, audio, thread-join, sleep, or `Runtime::block_on` work.
- The reducer is deterministic and I/O-free. Only matching generation results may change discovery, session, volume, hotkey, or persistence state.
- `AppHandle::dispatch`, `snapshot`, `drain_ui_effects`, and `install_waker` are the stable subproject boundary. View and tray modules receive no controller handle, session, socket, Tokio handle, mutable domain collection, address, or secret.
- Keep shell preferences in `%APPDATA%\OpenAirCast\settings.json`. Keep temporary legacy device-volume ownership behind `device_service`; never serialize volume, receiver membership, actual stream state, receiver IP addresses, diagnostics, generations, logs, or secrets into shell settings.
- Run every behavioral task test-first: add the focused test, run it and observe the stated RED reason, add only the implementation needed, then run the focused GREEN command. A compilation failure caused by the deliberately missing symbol counts as RED only when the task explicitly says so.
- Run `cargo fmt --all -- --check` before each task commit. Do not weaken an assertion merely to make a regression green.
- Pin `eframe`, `egui`, and `egui_kittest` to `=0.36.1`; use `default-features = false` plus `accesskit`, `default_fonts`, and `wgpu` for eframe so Windows builds do not acquire Wayland/X11/Web dependencies.
- Keep AccessKit enabled in debug and release. Do not place the feature behind `cfg(debug_assertions)`.
- No unconditional `request_repaint`, `request_repaint_after`, timer-driven frame loop, hidden-window polling, or 50 ms tray pump. The installed snapshot waker, OS input/expose/theme/DPI events, and at most one 120 ms transition are the only repaint sources.
- All interactive rows and buttons are at least 44 logical points high. All custom-painted information has a textual equivalent and a standard accessible control or AccessKit node.
- OpenAirCast remains GPL-2.0. For every dual MIT/Apache dependency, record and distribute under the MIT option; preserve full Inter and IBM Plex Mono SIL OFL 1.1 notices and font names.
- Source edits use `apply_patch`; generated/downloaded binary font and icon assets must come from the pinned sources or reproducible generator described in Task 1.
- Each task below ends in one commit during implementation. Do not combine task commits, and do not commit generated `.new.png`, `.diff.png`, temp settings, logs, or `dist` output.

Run commands from `C:\Users\Thorsten\Documents\Claude\Projects\HomePodCast`. In a fresh PowerShell session, establish the tested Cargo command once:

```powershell
$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) {
    (Get-Command cargo).Source
} else {
    Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
}
& $cargo --version
```

Expected prerequisite: Cargo/Rust 1.95 or newer. The audited machine has Cargo 1.98.0. Before implementation, the verified baseline is 7 passing `homepod-cast` tests, a passing workspace/all-targets suite, and a passing release build; the current release executable is 4,932,096 bytes. Existing upstream compiler warnings are baseline noise, not permission to add shell warnings.

## Contract Freeze and File Map

Create these modules and keep the listed responsibilities narrow:

- `crates/homepod-cast/src/app/mod.rs` — actor startup, service wiring types, exports.
- `crates/homepod-cast/src/app/state.rs` — authoritative domain state and validated shell value types.
- `crates/homepod-cast/src/app/event.rs` — all reducer inputs, including generation-tagged controller/preferences results.
- `crates/homepod-cast/src/app/effect.rs` — reducer effects, UI effects, non-blocking effect-executor port.
- `crates/homepod-cast/src/app/reducer.rs` — pure transition logic and reducer tests.
- `crates/homepod-cast/src/app/snapshot.rs` — deterministic presentation-only projection.
- `crates/homepod-cast/src/app_handle.rs` — bounded dispatch, ArcSwap publication, UI-effect drain, waker subscriptions.
- `crates/homepod-cast/src/device_service.rs` — temporary controller thread/Tokio runtime, `DeviceId -> Device` ownership, current `cast::Session`, and legacy volume file.
- `crates/homepod-cast/src/preferences.rs` — schema-v1 shell preferences, path policy, legacy-hotkey migration, debounce, atomic write worker.
- `crates/homepod-cast/src/platform/mod.rs` — Windows platform exports and motion/high-contrast query.
- `crates/homepod-cast/src/platform/hotkey.rs` — dedicated `RegisterHotKey` message-loop thread.
- `crates/homepod-cast/src/ui/mod.rs` — `ControlCenterApp`, update/event routing, window commands, six-page dispatcher.
- `crates/homepod-cast/src/ui/theme.rs` — fixed Windows Calm tokens, fonts, typography, theme and motion policy.
- `crates/homepod-cast/src/ui/navigation.rs` — 224/64-point navigation rail and actual-state footer.
- `crates/homepod-cast/src/ui/home.rs` — route-first Home layout and daily controls.
- `crates/homepod-cast/src/ui/pages.rs` — Speakers, Groups, Audio, Diagnostics, and Settings.
- `crates/homepod-cast/examples/generate_icon.rs` — deterministic project-owned icon generator.
- `crates/homepod-cast/build.rs` — embed the multi-resolution ICO and Windows metadata.
- `crates/homepod-cast/assets/fonts/*`, `crates/homepod-cast/assets/licenses/*`, `crates/homepod-cast/assets/openaircast.ico`, and `crates/homepod-cast/assets/openaircast.png` — embedded resources only.

Modify `crates/homepod-cast/src/main.rs`, `cast.rs`, `tray.rs`, `Cargo.toml`, workspace `Cargo.lock`, root `build.ps1`, `README.md`, and `THIRD_PARTY_NOTICES.md`. Delete `settings_window.rs` and `group_state.rs` only in Task 14 after their behavior has migrated and all replacement tests are green.

The public shell contract must retain these exact names and method meanings:

```rust
#[derive(Clone)]
pub struct AppHandle {
    event_tx: SyncSender<AppEvent>,
    snapshot: Arc<ArcSwap<UiSnapshot>>,
    ui_effects: Arc<Mutex<VecDeque<UiEffect>>>,
    wakers: Arc<WakerRegistry>,
}

impl AppHandle {
    pub fn dispatch(&self, event: AppEvent) -> Result<(), AppUnavailable>;
    pub fn snapshot(&self) -> Arc<UiSnapshot>;
    pub fn drain_ui_effects(&self) -> Vec<UiEffect>;
    pub fn install_waker(
        &self,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> SnapshotSubscription;
}
```

The authoritative state and presentation projection are fixed as follows:

```rust
pub struct AppState {
    pub revision: u64,
    pub desired_revision: u64,
    pub page: Page,
    pub window: WindowState,
    pub discovery: DiscoveryState,
    pub receivers: Vec<ReceiverState>,
    pub desired_receivers: HashSet<DeviceId>,
    pub staged_receivers: HashSet<DeviceId>,
    pub staged_base_revision: u64,
    pub active_receivers: HashSet<DeviceId>,
    pub stream: StreamState,
    pub master_volume: f32,
    pub theme: ThemePreference,
    pub hotkey: HotkeyBinding,
    pub notice: Option<UserNotice>,
    pub shutting_down: bool,
    pub generations: Generations,
}

pub struct UiSnapshot {
    pub revision: u64,
    pub desired_revision: u64,
    pub page: Page,
    pub window: WindowSnapshot,
    pub discovery: DiscoverySnapshot,
    pub receivers: Arc<[ReceiverSnapshot]>,
    pub desired_receivers: Arc<[DeviceId]>,
    pub staged_receivers: Arc<[DeviceId]>,
    pub staged_membership_dirty: bool,
    pub staged_membership_stale: bool,
    pub active_receivers: Arc<[DeviceId]>,
    pub stream: StreamSnapshot,
    pub master_volume: f32,
    pub theme: ThemePreference,
    pub hotkey: HotkeyBinding,
    pub notice: Option<UserNotice>,
    pub can_start: bool,
    pub can_stop: bool,
    pub shutting_down: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub visible: bool,
    pub geometry: WindowGeometry,
    pub close_to_tray: bool,
    pub launch_at_startup: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowSnapshot {
    pub visible: bool,
    pub geometry: WindowGeometry,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoveryState {
    Idle,
    Discovering { generation: GenerationId },
    Ready,
    Failed { summary: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoverySnapshot {
    Idle,
    Discovering,
    Ready,
    Failed { summary: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability { Available, Unavailable }

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverState {
    pub id: DeviceId,
    pub name: String,
    pub model: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverSnapshot {
    pub id: DeviceId,
    pub name: String,
    pub model: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamState {
    Stopped,
    Starting { generation: GenerationId },
    Streaming { generation: GenerationId },
    Stopping { generation: GenerationId },
    Failed { generation: GenerationId, summary: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamSnapshot {
    Stopped,
    Starting { generation: GenerationId },
    Streaming { generation: GenerationId },
    Stopping { generation: GenerationId },
    Failed { generation: GenerationId, summary: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity { Info, Warning, Error }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeCode {
    NoReceiverAvailable,
    DiscoveryFailed,
    SessionFailed,
    ControllerUnavailable,
    PreferencesFailed,
    HotkeyFailed,
    InvalidInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrectiveAction { Refresh, Retry, OpenSettings }

#[derive(Clone, Debug, PartialEq)]
pub struct UserNotice {
    pub severity: Severity,
    pub code: NoticeCode,
    pub summary: String,
    pub action: Option<CorrectiveAction>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Generations {
    pub discovery: GenerationId,
    pub session: GenerationId,
    pub volume: GenerationId,
    pub preferences: GenerationId,
    pub hotkey: GenerationId,
}
```

Use these concrete internal result payloads in subproject 1:

```rust
pub enum ControllerEvent {
    DiscoveryCompleted {
        generation: GenerationId,
        receivers: Vec<ReceiverState>,
    },
    DiscoveryFailed { generation: GenerationId, summary: String },
    DesiredReceiversChanged {
        desired_revision: u64,
        receiver_ids: Vec<DeviceId>,
    },
    SessionStarted {
        generation: GenerationId,
        active_receiver_ids: Vec<DeviceId>,
    },
    SessionStopped { generation: GenerationId },
    SessionFailed { generation: GenerationId, summary: String },
    VolumeApplied { generation: GenerationId, volume: f32 },
    ChannelClosed { summary: String },
}

pub enum PreferencesEvent {
    Loaded { value: Preferences, notice: Option<UserNotice> },
    LoadFailed { summary: String },
    Persisted { generation: GenerationId },
    PersistFailed { generation: GenerationId, summary: String },
    HotkeyReconfigured {
        generation: GenerationId,
        applied: HotkeyBinding,
        failure: Option<String>,
    },
}

pub enum UiEffect {
    ShowMainWindow,
    HideMainWindow,
    FocusMainWindow,
    Announce(String),
    DropTray,
    CloseApplication,
}
```

`DesiredReceiversChanged` is unused by today's one-shot legacy adapter but is
tested now as the conflict input for subproject 2 saved-group/device-snapshot
updates. A `desired_revision` not newer than the current revision is ignored.

Use this reducer/effect surface. Private event payload structs may grow in later subprojects, but these existing variants and semantics remain stable:

```rust
pub enum AppEvent {
    Navigate(Page),
    MainWindowCloseRequested,
    ShowMainWindow,
    ToggleStagedReceiver(DeviceId),
    ApplyStagedReceivers,
    DiscardStagedReceivers,
    StartRequested,
    StopRequested,
    GlobalHotkeyPressed,
    RefreshRequested,
    MasterVolumeChanged(f32),
    ThemeChanged(ThemePreference),
    HotkeyChanged(HotkeyBinding),
    WindowGeometryChanged(WindowGeometry),
    Controller(ControllerEvent),
    Preferences(PreferencesEvent),
    QuitRequested,
}

pub enum AppEffect {
    Discover { generation: GenerationId },
    StartSession {
        generation: GenerationId,
        receiver_ids: Vec<DeviceId>,
        volume: f32,
    },
    StopSession { generation: GenerationId },
    ApplyVolume { generation: GenerationId, volume: f32 },
    PersistPreferences { generation: GenerationId, value: Preferences },
    ReconfigureHotkey { generation: GenerationId, value: HotkeyBinding },
    ShowMainWindow,
    HideMainWindow,
    BeginShutdown,
}

pub struct Transition {
    pub effects: Vec<AppEffect>,
    pub snapshot_changed: bool,
}

pub fn reduce(state: &mut AppState, event: AppEvent) -> Transition;
```

Define the temporary controller seam so Task 8 can adapt today's code and subproject 2 can replace it without leaking into views:

```rust
pub(crate) enum DeviceCommand {
    Discover { generation: GenerationId },
    StartSession {
        generation: GenerationId,
        receiver_ids: Vec<DeviceId>,
        volume: f32,
    },
    StopSession { generation: GenerationId },
    ApplyVolume { generation: GenerationId, volume: f32 },
    Shutdown,
}

pub(crate) trait ControllerPort: Send + Sync {
    fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError>;
}
```

`EffectExecutor` maps controller, preference, hotkey, window, and shutdown effects to ports using `try_send` or worker scheduling only. Subproject 2 replaces the concrete `ControllerPort` with `DeviceBackendHandle::try_send` and maps `DeviceSnapshot`/`BackendEvent` into `ControllerEvent`; UI, tray, hotkey, reducer call sites, and all four `AppHandle` methods remain unchanged.

---

### Task 1: Pin GUI dependencies and embed reproducible visual assets

**Files:**
- Modify: `crates/homepod-cast/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/homepod-cast/src/main.rs`
- Create: `crates/homepod-cast/src/ui/mod.rs`
- Create: `crates/homepod-cast/src/ui/theme.rs`
- Create: `crates/homepod-cast/examples/generate_icon.rs`
- Create: `crates/homepod-cast/build.rs`
- Create: `crates/homepod-cast/assets/fonts/InterVariable.ttf`
- Create: `crates/homepod-cast/assets/fonts/IBMPlexMono-Regular.ttf`
- Create: `crates/homepod-cast/assets/licenses/Inter-OFL-1.1.txt`
- Create: `crates/homepod-cast/assets/licenses/IBM-Plex-Mono-OFL-1.1.txt`
- Create: `crates/homepod-cast/assets/openaircast.ico`
- Create: `crates/homepod-cast/assets/openaircast.png`

**Interfaces:**
- Consumes: Existing binary entry point `fn main() -> anyhow::Result<()>`, `crates/homepod-cast/Cargo.toml`, and the workspace `Cargo.lock`.
- Produces: `ui::theme::{INTER_VARIABLE, IBM_PLEX_MONO, APP_ICON_PNG}: &'static [u8]`, eframe/egui/egui_kittest `=0.36.1`, embedded `assets/openaircast.ico` plus `assets/openaircast.png`, and Windows resource compilation consumed by Tasks 10 and 14.

- [ ] Add `mod ui;` to `main.rs`, create `ui/mod.rs`, and add this deliberately asset-dependent test to `ui/theme.rs` before the assets exist:

```rust
pub const INTER_VARIABLE: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
pub const IBM_PLEX_MONO: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/openaircast.png");

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    #[test]
    fn embedded_assets_are_present_and_icon_has_required_sizes() {
        assert!(super::INTER_VARIABLE.len() > 800_000);
        assert!(super::IBM_PLEX_MONO.len() > 100_000);
        let dir = ico::IconDir::read(Cursor::new(include_bytes!(
            "../../assets/openaircast.ico"
        )))
        .expect("valid embedded ICO");
        let mut sizes = dir
            .entries()
            .iter()
            .map(|entry| (entry.width(), entry.height()))
            .collect::<Vec<_>>();
        sizes.sort_unstable();
        assert_eq!(sizes, [(16, 16), (20, 20), (24, 24), (32, 32)]);
    }
}
```

- [ ] Add the dependency block exactly, preserving the existing AirPlay/wasapi/tracing dependencies and selecting the MIT option in notices later:

```toml
rust-version = "1.95"

[dependencies]
eframe = { version = "=0.36.1", default-features = false, features = ["accesskit", "default_fonts", "wgpu"] }
egui = "=0.36.1"
arc-swap = "=1.9.2"
serde = { workspace = true }
serde_json = "=1.0.149"
thiserror = { workspace = true }
image = { version = "=0.25.10", default-features = false, features = ["png"] }
tray-icon = "=0.24.0"

[dev-dependencies]
egui_kittest = "=0.36.1"
ico = "=0.5.0"
tempfile = "=3.27.0"

[build-dependencies]
winresource = "=0.1.31"
```

- [ ] Run `& $cargo test -p homepod-cast ui::theme::tests::embedded_assets_are_present_and_icon_has_required_sizes --no-run`. Expected RED: compilation reports that one or more `assets` paths do not exist.

- [ ] Download the approved font bytes and license texts from the pinned Google Fonts tree, then verify the exact SHA-256 values:

```powershell
$fontBase = "https://raw.githubusercontent.com/google/fonts/ec626514f79f831f1ab848a82114a0ce7e2d6372/ofl"
New-Item -ItemType Directory -Force crates/homepod-cast/assets/fonts | Out-Null
New-Item -ItemType Directory -Force crates/homepod-cast/assets/licenses | Out-Null
Invoke-WebRequest "$fontBase/inter/Inter%5Bopsz,wght%5D.ttf" -OutFile crates/homepod-cast/assets/fonts/InterVariable.ttf
Invoke-WebRequest "$fontBase/ibmplexmono/IBMPlexMono-Regular.ttf" -OutFile crates/homepod-cast/assets/fonts/IBMPlexMono-Regular.ttf
Invoke-WebRequest "$fontBase/inter/OFL.txt" -OutFile crates/homepod-cast/assets/licenses/Inter-OFL-1.1.txt
Invoke-WebRequest "$fontBase/ibmplexmono/OFL.txt" -OutFile crates/homepod-cast/assets/licenses/IBM-Plex-Mono-OFL-1.1.txt
Get-FileHash -Algorithm SHA256 crates/homepod-cast/assets/fonts/InterVariable.ttf, crates/homepod-cast/assets/fonts/IBMPlexMono-Regular.ttf, crates/homepod-cast/assets/licenses/Inter-OFL-1.1.txt, crates/homepod-cast/assets/licenses/IBM-Plex-Mono-OFL-1.1.txt
```

Expected hashes, in command order: `29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031`, `6a3412f058c7d8dfd9170c41e85ade48e5156ecb89356110ca57a0a27734af46`, `5b9321a4298cfeb6b34354164a1c3afc3db114569984c502b9b35d988fd58c57`, and `7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da`.

- [ ] Implement `examples/generate_icon.rs` as a deterministic 4x-supersampled renderer: transparent square; Route-blue `#5E72D6` source circle centered at 27%/50%; a rounded 8%-height route line from 39% to 62%; Streaming-teal `#51B9AF` receiver circle centered at 73%/50%; two Canvas `#F5F7FB` concentric signal arcs cut into the receiver node; Lanczos downsample to 16, 20, 24, and 32 for the four ICO entries and render a separate 256x256 PNG from the same vector geometry. Its two positional arguments are the ICO output path followed by the PNG output path, and it exits non-zero on any other argument count.

- [ ] Run the generator and inspect both assets at 100%, 125%, 150%, 175%, and 200% zoom; the route remains legible, uses no text, and contains no detail thinner than one final pixel:

```powershell
& $cargo run -p homepod-cast --example generate_icon -- crates/homepod-cast/assets/openaircast.ico crates/homepod-cast/assets/openaircast.png
```

- [ ] Add `build.rs` with `cargo:rerun-if-changed=assets/openaircast.ico` and, on Windows only, configure `winresource::WindowsResource` with that ICO, product name `OpenAirCast`, file description `OpenAirCast Control Center`, and original filename `OpenAirCast.exe`. Keep the normal native title bar.

- [ ] Run `& $cargo test -p homepod-cast ui::theme::tests::embedded_assets_are_present_and_icon_has_required_sizes`. Expected GREEN: one test passes and the ICO size vector matches exactly.

- [ ] Run `& $cargo tree -p homepod-cast -e features | Select-String 'eframe|accesskit|wgpu|glow|wayland|x11|web_screen_reader'`. Confirm eframe, AccessKit, and wgpu are present and glow/Wayland/X11/web screen-reader features are absent.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/Cargo.toml Cargo.lock crates/homepod-cast/build.rs crates/homepod-cast/examples/generate_icon.rs crates/homepod-cast/src/main.rs crates/homepod-cast/src/ui crates/homepod-cast/assets
git commit -m "build: pin control center GUI assets and dependencies"
```

### Task 2: Define authoritative state and deterministic presentation snapshots

**Files:**
- Create: `crates/homepod-cast/src/app/mod.rs`
- Create: `crates/homepod-cast/src/app/state.rs`
- Create: `crates/homepod-cast/src/app/event.rs`
- Create: `crates/homepod-cast/src/app/effect.rs`
- Create: `crates/homepod-cast/src/app/snapshot.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Consumes: `airplay_core::DeviceId` and Task 1's compilable `ui` module/resource constants; it imports no `Device`, `cast::Session`, channel receiver, address, or secret.
- Produces: The frozen `AppState`, `AppEvent`, `AppEffect`, `ControllerEvent`, `PreferencesEvent`, and `UiSnapshot` types plus `UiSnapshot::from_state(state: &AppState) -> UiSnapshot`, consumed by Tasks 3–15.

- [ ] Add `mod app;` in `main.rs`, declare the five new submodules in `app/mod.rs`, and first add snapshot tests that construct receivers named `zeta`, `Alpha`, and `alpha`, with intentionally unsorted IDs/sets. Assert case-folded-name-then-ID receiver order, ID-sorted membership arrays, dirty/stale flags, actual-state-derived `can_start`/`can_stop`, and absence of any address-bearing field.

- [ ] Run `& $cargo test -p homepod-cast app::snapshot::tests --no-run`. Expected RED: missing `AppState`, `ReceiverState`, `UiSnapshot`, and associated value types.

- [ ] Implement the state types with derives needed by reducer assertions. Use the approved `AppState` fields verbatim and these supporting semantics:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct GenerationId(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page { #[default] Home, Speakers, Groups, Audio, Diagnostics, Settings }

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference { #[default] System, Light, Dark }

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HotkeyBinding { pub enabled: bool, pub modifiers: u32, pub virtual_key: u32 }

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowGeometry {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}
```

`HotkeyBinding::default()` is enabled Ctrl+Alt+H (`MOD_CONTROL | MOD_ALT`, virtual key `0x48`). `WindowGeometry::default()` is 1120x720, non-maximized, with no saved position. Validation clamps size to at least 900x600 and rejects non-finite dimensions/coordinates.

- [ ] Add `WindowState`, `DiscoveryState`, `Availability`, `ReceiverState`, `StreamState`, `Severity`, `UserNotice`, and `Generations`. `ReceiverState` contains only `DeviceId`, name, model, availability, and actual-state label inputs; `Device` and addresses stay in `device_service`. `StreamState` is exactly `Stopped`, `Starting { generation }`, `Streaming { generation }`, `Stopping { generation }`, or `Failed { generation, summary }`.

- [ ] Define `ControllerEvent` with `DiscoveryCompleted`, `DiscoveryFailed`, `SessionStarted`, `SessionStopped`, `SessionFailed`, `VolumeApplied`, and `ChannelClosed`; every async completion carries the relevant `GenerationId`. Define `PreferencesEvent` with `Loaded`, `LoadFailed`, `Persisted`, `PersistFailed`, and `HotkeyReconfigured`; ordered completions carry their preference/hotkey generation.

- [ ] Implement `UiSnapshot::from_state(&AppState)` with all spec fields. Sort receivers by `name.to_lowercase()` and `DeviceId.0`; sort desired/staged/active arrays by `DeviceId.0`. Set `staged_membership_dirty = staged != desired`, `staged_membership_stale = dirty && staged_base_revision < desired_revision`, `can_start` only for Stopped/Failed with at least one available desired receiver, and `can_stop` for Starting/Streaming.

- [ ] Run `& $cargo test -p homepod-cast app::snapshot::tests`. Expected GREEN: deterministic projection tests pass.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/main.rs crates/homepod-cast/src/app
git commit -m "feat: define control center state and snapshot contract"
```

### Task 3: Implement staged membership and discovery conflict reduction

**Files:**
- Create: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`

**Interfaces:**
- Consumes: Task 2's `AppState`, `AppEvent::{ToggleStagedReceiver, ApplyStagedReceivers, DiscardStagedReceivers, Controller}`, `ControllerEvent::{DiscoveryCompleted, DesiredReceiversChanged}`, and `AppEffect::{StartSession, StopSession}`.
- Produces: `pub fn reduce(state: &mut AppState, event: AppEvent) -> Transition` with stable-ID discovery merge and staged/desired conflict semantics, consumed and extended by Tasks 4–6.

- [ ] Add reducer tests for: checkbox toggles stage only; no effect while streaming; Apply commits the complete set once; Discard emits no controller effect; clean staging auto-refreshes on a newer discovery/desired result; dirty staging survives that result and becomes stale; applying stale staging deliberately replaces the complete desired set; a missing desired receiver remains in inventory as Unavailable and is excluded from start targets.

Use assertions shaped like:

```rust
let before_active = state.active_receivers.clone();
let transition = reduce(&mut state, AppEvent::ToggleStagedReceiver(kitchen.clone()));
assert!(state.staged_receivers.contains(&kitchen));
assert!(!state.desired_receivers.contains(&kitchen));
assert_eq!(state.active_receivers, before_active);
assert!(transition.effects.is_empty());
assert_eq!(state.revision, 1);
```

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::staged`. Expected RED: `reduce` is missing.

- [ ] Implement `reduce` with small branch helpers and one `finish` helper that increments `state.revision` exactly once when `snapshot_changed` is true. No branch increments `revision` directly.

- [ ] Implement discovery merge by `DeviceId`: update discovered records, retain last-known desired/staged records as Unavailable, remove missing records only when they are neither desired nor staged, and never add an unavailable ID to a `StartSession.receiver_ids` vector.

- [ ] Implement Apply/Discard. Apply increments `desired_revision` once, sets `staged_base_revision` to it, replaces all desired IDs, and emits at most one effect: `StartSession` with a new session generation when Starting/Streaming and at least one selected receiver is available, or `StopSession` with one new session generation when the applied set is empty during Starting/Streaming. Applying while Stopped commits without starting playback. Discard loads the latest desired set and base revision without a controller effect.

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::staged; & $cargo test -p homepod-cast app::reducer::tests::discovery`. Expected GREEN: all staging, merge, and conflict tests pass.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/app
git commit -m "feat: reduce staged speaker membership by stable identity"
```

### Task 4: Implement generation-safe session lifecycle reduction

**Files:**
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`

**Interfaces:**
- Consumes: Task 3's `reduce(&mut AppState, AppEvent) -> Transition`, stable desired/staged sets, `Generations::session`, and `ControllerEvent::{SessionStarted, SessionStopped, SessionFailed}`.
- Produces: Generation-safe `reduce` behavior for `AppEvent::{StartRequested, StopRequested, Controller}` and matching `AppEffect::{StartSession, StopSession}`, consumed by Tasks 5, 6, 8, 11, and 13.

- [ ] Add tests for Start entering Starting without active membership; matching SessionStarted entering Streaming with exactly the confirmed active IDs; stale Started/Failed/Stopped events producing no semantic revision; matching start failure returning Stopped, clearing active, retaining desired, and showing one corrective notice; Stop entering Stopping and clearing active while preserving desired/staged; matching SessionStopped entering Stopped.

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::session`. Expected RED: Start/Stop/controller branches return no effects or wrong state.

- [ ] Implement `StartRequested`: reject while shutting down or without an available desired target; otherwise increment `generations.session`, enter `Starting`, clear stale active membership, and emit exactly one sorted `StartSession` using the current clamped volume.

- [ ] Implement matching controller completion branches. Compare the result generation to `generations.session` before any mutation; stale events return `Transition { effects: vec![], snapshot_changed: false }` and are debug-logged by the actor, not by the reducer.

- [ ] Implement `StopRequested`: for Starting/Streaming increment the session generation, enter Stopping, clear active membership, and emit one `StopSession`; for Stopped/Stopping it is idempotent. A matching failure produces a concise `UserNotice` with retry/refresh guidance and returns actual state to Stopped.

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::session`. Expected GREEN: matching results win, older results cannot overwrite them, and desired/staged state survives stop/failure.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/app
git commit -m "feat: add generation-safe streaming transitions"
```

### Task 5: Implement shell, hotkey, volume, preference, and quit reduction

**Files:**
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`

**Interfaces:**
- Consumes: Task 4's generation-safe `reduce` lifecycle, Task 2's `Preferences`-bearing `AppEffect`, and `WindowState`/`HotkeyBinding`/`ThemePreference` value types.
- Produces: Complete pure `reduce(state: &mut AppState, event: AppEvent) -> Transition` coverage for navigation, window, hotkey, volume, preferences, and idempotent quit, consumed by the Task 6 actor and all Task 10–14 adapters.

- [ ] Add tests for the exact global-hotkey matrix: Starting/Streaming emits Stop; Stopped with desired emits Start; Stopped without desired selects the first available receiver in display order then starts; no available receiver emits a notice and no controller effect.

- [ ] Add tests that finite volume is clamped to 0.0..=1.0 and emits immediate `ApplyVolume`; NaN/infinity keep the current volume and show one notice; Navigate, ThemeChanged, and valid WindowGeometryChanged each increment revision once and schedule shell preference persistence; hotkey reconfiguration failure preserves/reports the applied prior binding.

- [ ] Add tests that `MainWindowCloseRequested` emits only `HideMainWindow`; `ShowMainWindow` emits only `ShowMainWindow`; first Quit marks `shutting_down`, emits preference persistence, optional session stop, and one `BeginShutdown`; second Quit and later Start are effect-free.

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::shell`. Expected RED: the new event cases are unimplemented.

- [ ] Implement the hotkey branches by delegating to the same tested `start_requested`/`stop_requested` helpers used by buttons and tray. Select the first available receiver from the already display-sorted `state.receivers`, commit it to staged and desired membership, increment `desired_revision`, then start once.

- [ ] Implement volume/theme/page/window/hotkey branches. A changed `MasterVolumeChanged` publishes the clamped value immediately and emits `ApplyVolume`; the same finite value repeated on slider release emits `ApplyVolume` with `snapshot_changed = false`, so the adapter receives the authoritative final drag value without a false revision. The legacy device adapter performs backend coalescing and volume-file debounce. Theme/page/window changes emit one `PersistPreferences` built from the new state. Persist the requested hotkey only after a matching successful `HotkeyReconfigured` event; on failure retain the previously applied value and publish a notice.

- [ ] Implement quit ordering as effects `[PersistPreferences, StopSession when needed, HideMainWindow, BeginShutdown]`; increment the session/preference generations before building those effects. `BeginShutdown` appears once and owns bounded cleanup outside the reducer.

- [ ] Run `& $cargo test -p homepod-cast app::reducer::tests::shell`. Expected GREEN: hotkey, volume, revision, close, and idempotent quit tests pass.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/app
git commit -m "feat: reduce control center shell actions"
```

### Task 6: Build the non-blocking AppHandle actor and publication contract

**Files:**
- Create: `crates/homepod-cast/src/app_handle.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Consumes: Task 5's complete `reduce(&mut AppState, AppEvent) -> Transition`, Task 2's `UiSnapshot::from_state`, and `AppEffect`/`UiEffect` values.
- Produces: `start_app(initial: AppState, executor: Box<dyn EffectExecutor>) -> (AppHandle, AppRuntime)`, `EffectExecutor::execute(&mut self, AppEffect, &AppFeedback)`, and the four frozen `AppHandle` methods consumed by Tasks 7–14.

- [ ] Add contract tests for bounded non-blocking dispatch while a fake effect is held; atomic old-or-new snapshot observation from multiple reader threads; one waker call for one semantic transition; no call after subscription drop; exact-once FIFO UI-effect drain; reversed controller generations; closed controller producing Needs attention while Quit remains dispatchable.

- [ ] Run `& $cargo test -p homepod-cast app_handle::tests --no-run`. Expected RED: `AppHandle`, `AppUnavailable`, `SnapshotSubscription`, and runtime startup are missing.

- [ ] Implement a 256-entry `sync_channel<AppEvent>` and typed dispatch errors:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AppUnavailable {
    #[error("application event queue is busy")]
    Busy,
    #[error("application actor is closed")]
    Closed,
}

pub fn dispatch(&self, event: AppEvent) -> Result<(), AppUnavailable> {
    self.event_tx.try_send(event).map_err(|error| match error {
        TrySendError::Full(_) => AppUnavailable::Busy,
        TrySendError::Disconnected(_) => AppUnavailable::Closed,
    })
}
```

- [ ] Store the latest complete snapshot in `ArcSwap<UiSnapshot>`. After each `snapshot_changed` transition, build one complete `UiSnapshot` and store it once. Clone the registered waker list under its mutex, release the mutex, and invoke callbacks outside locks. Catch a callback panic so one integration cannot kill the actor.

- [ ] Implement subscription IDs with an atomic counter and a drop guard that removes only its callback. Implement UI effects as `Mutex<VecDeque<UiEffect>>` and share an `AtomicBool wake_pending` between snapshot publication and UI-effect insertion. The first publication/insertion after a UI drain flips false to true and invokes each waker once; more state/effects before that scheduled frame do not issue redundant wakes. `drain_ui_effects` clears the gate before draining each queued value exactly once, so a concurrent later publication schedules a fresh frame.

- [ ] Define the executor boundary used by later tasks:

```rust
pub(crate) trait EffectExecutor: Send + 'static {
    fn execute(&mut self, effect: AppEffect, feedback: &AppFeedback);
}

#[derive(Clone)]
pub(crate) struct AppFeedback {
    pub events: AppEventSender,
    pub ui_effects: UiEffectSender,
}
```

Both feedback senders use bounded/non-blocking sends. The actor may call `execute`, but every concrete implementation must return after queueing work; it may not perform the work inline.

- [ ] Implement actor exit only after `BeginShutdown` is accepted and the shutdown coordinator queues `UiEffect::CloseApplication`; do not make dropping an arbitrary `AppHandle` stop the process.

- [ ] Run `& $cargo test -p homepod-cast app_handle::tests`. Expected GREEN: all seven contract categories pass, including exact waker/unsubscribe/drain behavior.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/app_handle.rs crates/homepod-cast/src/app crates/homepod-cast/src/main.rs
git commit -m "feat: publish immutable control center snapshots"
```

### Task 7: Implement schema-v1 shell preferences, migration, debounce, and atomic replace

**Files:**
- Create: `crates/homepod-cast/src/preferences.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/Cargo.toml`

**Interfaces:**
- Consumes: Task 6's `AppEventSender`/`AppFeedback`, `AppEffect::PersistPreferences { generation, value }`, `PreferencesEvent`, and Task 2's `Page`, `ThemePreference`, `HotkeyBinding`, and `WindowGeometry`.
- Produces: `PreferencesRepository::{load(&self) -> PreferencesLoad, save_atomic(&self, value: &Preferences) -> Result<(), PreferencesError>}` and `PreferencesWorkerHandle::{try_schedule(&self, generation: GenerationId, value: Preferences) -> Result<(), PreferencesUnavailable>, try_flush_and_shutdown(&self, acknowledgement: SyncSender<()>) -> Result<(), PreferencesUnavailable>}`, consumed by Task 14's production executor/shutdown coordinator.

- [ ] Add tests using `tempfile::TempDir` and injected `PreferencesPaths`: valid `%APPDATA%\HomePodCast\hotkey.txt` migration; invalid legacy value notice; corrupt JSON fallback; complete schema round trip; deterministic JSON; replacement failure preserving old valid JSON; 500 ms standard/1 s geometry debounce with final flush containing the newest geometry, hotkey, launch-at-startup, and theme; absence of volume, desired/active membership, stream, IP, generation, log, and secret keys.

- [ ] Run `& $cargo test -p homepod-cast preferences::tests --no-run`. Expected RED: repository, schema, migration parser, and worker do not exist.

- [ ] Define the persisted shape exactly:

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub hotkey: HotkeyBinding,
    pub theme: ThemePreference,
    pub window: WindowGeometry,
    pub last_page: Page,
    pub close_to_tray: bool,
    pub launch_at_startup: bool,
}
```

`schema_version` must equal 1; defaults are Ctrl+Alt+H enabled, System theme, 1120x720, Home, close-to-tray true, and launch-at-startup false. The field is persisted now so future startup integration is schema-stable, but Settings does not show a non-functional control.

- [ ] Define `PreferencesRepository: Send + Sync` with `load() -> PreferencesLoad` and `save_atomic(&Preferences)`. `PreferencesLoad` returns validated preferences plus at most one non-fatal `UserNotice`. Resolve canonical and legacy paths from an injected app-data root; production uses `APPDATA` once before worker startup.

- [ ] Parse legacy hotkey only as two comma-separated decimal `u32` values with an enabled non-zero key, preserve the legacy file, and never read `volume.txt` in this repository. Prefer valid schema-v1 JSON over legacy migration.

- [ ] Implement atomic save in the settings directory: serialize to `settings.json.tmp`, `flush` and `sync_all`, then call Windows `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`. Add `Win32_Storage_FileSystem` to the existing `windows-sys` feature list. On any failure, keep the last valid `settings.json` and return a typed failure event.

- [ ] Implement a bounded preference worker. Coalesce ordinary changes to the newest value at 500 ms and geometry-only churn to 1 s; a newer generation wins; `FlushAndShutdown` synchronously drains the final value on the worker thread and acknowledges completion to the shutdown coordinator. Factor deadline selection into a pure `DebounceState` tested without sleeping.

- [ ] Run `& $cargo test -p homepod-cast preferences::tests`. Expected GREEN: all six spec categories pass with deterministic, domain-clean JSON.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/preferences.rs crates/homepod-cast/src/app crates/homepod-cast/Cargo.toml Cargo.lock
git commit -m "feat: persist shell preferences atomically"
```

### Task 8: Extract the current AirPlay loop behind the private device-service port

**Files:**
- Create: `crates/homepod-cast/src/device_service.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Consumes: Task 6's `AppEventSender`, frozen `ControllerPort::try_send(&self, DeviceCommand) -> Result<(), ControllerSendError>`, Task 2's `ControllerEvent`, and today's `cast::{discover, Session}` APIs.
- Produces: `spawn_device_service(events: AppEventSender, appdata_dir: PathBuf) -> anyhow::Result<DeviceServiceHandle>`, `impl ControllerPort for DeviceServiceHandle`, and `DeviceServiceHandle::shutdown_and_wait(timeout: Duration) -> bool`, consumed by Task 14.

- [ ] Add fake-port tests that each `AppEffect` maps to one `DeviceCommand`; queue full/closed is typed and publishes one Needs attention notice; result mapping retains generation and confirmed active IDs; volume commands coalesce to the newest value and persist only the newest legacy volume; shutdown requests Stop before runtime shutdown.

- [ ] Add pure tests that a device map resolves `receiver_ids` by `DeviceId`, not discovery position, ignores unavailable IDs, and returns selected devices in the reducer-provided order even after discovery reorder.

- [ ] Run `& $cargo test -p homepod-cast device_service::tests --no-run`. Expected RED: `DeviceServiceHandle`, command mapping, and identity resolver are missing.

- [ ] Move `tray.rs::control_loop` into `device_service.rs` as a dedicated thread owning one Tokio runtime, current discovered `HashMap<DeviceId, Device>`, optional `cast::Session`, keepalive timing, and a bounded 64-entry command receiver. Startup returns immediately; discovery happens only after `Discover` and reports `ControllerEvent` asynchronously.

- [ ] Implement `ControllerPort::try_send` on a cheap cloneable handle. `Discover` uses the current scan and maps `Device` to presentation-safe receiver records; `StartSession` resolves IDs inside the private map; `SessionStarted.active_receivers` is populated only after `cast::Session::start` succeeds; stop/failure results are generation-tagged.

- [ ] Move `cast::load_volume`/`save_volume` behavior to a private `LegacyDevicePreferences` in `device_service.rs`, retaining `%APPDATA%\HomePodCast\volume.txt`. Coalesce volume application during rapid slider input, send the latest value to the current session, and debounce the legacy file write; flush the newest volume during service shutdown. Do not copy volume into shell `Preferences`.

- [ ] Keep `cast::Session`, `discover`, timing-protocol tests, WASAPI capture, and group transport unchanged except imports/visibility needed by the adapter. Do not add continuous browsing or reconnect logic.

- [ ] Run `& $cargo test -p homepod-cast device_service::tests; & $cargo test -p homepod-cast cast::tests`. Expected GREEN: adapter tests and all three existing cast tests pass.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/device_service.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/src/app crates/homepod-cast/src/main.rs
git commit -m "refactor: isolate the legacy AirPlay controller service"
```

### Task 9: Isolate global hotkey registration on a dedicated Win32 thread

**Files:**
- Create: `crates/homepod-cast/src/platform/mod.rs`
- Create: `crates/homepod-cast/src/platform/hotkey.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/Cargo.toml`

**Interfaces:**
- Consumes: Task 6's `AppEventSender`, `AppEffect::ReconfigureHotkey`, `PreferencesEvent::HotkeyReconfigured`, and Task 2's `GenerationId`/`HotkeyBinding`.
- Produces: `HotkeyServiceHandle::{spawn(initial: HotkeyBinding, events: AppEventSender) -> Result<Self, HotkeyError>, try_reconfigure(&self, generation: GenerationId, binding: HotkeyBinding) -> Result<(), HotkeyError>, shutdown_and_wait(&self, timeout: Duration) -> bool}` and `MotionPreferences::read() -> MotionPreferences`, consumed by Tasks 10 and 14.

- [ ] Add a fake `HotkeyRegistrar` state-machine test for initial Ctrl+Alt+H registration, runtime replacement, disabled binding, failed replacement restoring the prior binding when possible, failed restore reporting the actually applied disabled state, hotkey press dispatching `GlobalHotkeyPressed`, and bounded shutdown unregistering once.

- [ ] Run `& $cargo test -p homepod-cast platform::hotkey::tests --no-run`. Expected RED: hotkey service and registrar seam are missing.

- [ ] Implement `HotkeyServiceHandle` with a dedicated thread and Win32 `GetMessageW` loop. Use `WM_APP + 1` to wake configuration commands from a bounded queue, `WM_HOTKEY` to dispatch `GlobalHotkeyPressed`, and `WM_APP + 2` for shutdown; keep `RegisterHotKey`/`UnregisterHotKey` entirely off the eframe event loop.

- [ ] Apply replacements transactionally: unregister current, try new, and on failure re-register current. Report `PreferencesEvent::HotkeyReconfigured { generation, applied, failure }`; the reducer persists only `applied` and announces the failure once.

- [ ] Add the required `windows-sys` threading/message features without broadening to the high-level `windows` crate. Add a platform helper that reads `SPI_GETCLIENTAREAANIMATION` and `SPI_GETHIGHCONTRAST` at app creation; return conservative reduced-motion/high-contrast values when the API query fails.

- [ ] Run `& $cargo test -p homepod-cast platform::hotkey::tests`. Expected GREEN: replacement/restore and event routing pass without registering a real test-wide hotkey.

- [ ] On Windows, add one ignored hardware test that registers a non-default test chord, posts a synthetic thread message through the service seam, verifies one dispatch, and unregisters. Run it explicitly once with `& $cargo test -p homepod-cast platform::hotkey::tests::windows_message_loop_round_trip -- --ignored --exact`.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/platform crates/homepod-cast/src/app crates/homepod-cast/src/main.rs crates/homepod-cast/Cargo.toml Cargo.lock
git commit -m "feat: isolate the Windows global hotkey service"
```

### Task 10: Build the Windows Calm eframe shell, fonts, themes, and navigation

**Files:**
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/ui/theme.rs`
- Create: `crates/homepod-cast/src/ui/navigation.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`

**Interfaces:**
- Consumes: Task 1's embedded font/icon constants, Task 6's `AppHandle`/`SnapshotSubscription`, Task 9's `MotionPreferences`, and Task 2's immutable `UiSnapshot`, `UiEffect`, `Page`, and `AppEvent`.
- Produces: `ControlCenterApp::new(cc: &eframe::CreationContext<'_>, handle: AppHandle, motion: MotionPreferences) -> Self` and `navigation::show(ui: &mut egui::Ui, snapshot: &UiSnapshot, emit: &mut dyn FnMut(AppEvent))`, consumed by Tasks 11–14.

- [ ] Add headless harness tests at 1120x720 and 900x600 for a six-item navigation rail; assert 224-point expanded and 64-point compact modes, destination order, visible actual-state label, 44-point hit heights, icon-only compact labels/tooltips, keyboard activation dispatching one `Navigate`, and no horizontal overflow.

- [ ] Add light/dark token tests for the six exact anchors and contrast tests: body text/background at least 4.5:1, focus/non-text states at least 3:1. Test animation time is 0 when client animation is disabled and at most 0.120 seconds otherwise.

- [ ] Run `& $cargo test -p homepod-cast ui::navigation::tests; & $cargo test -p homepod-cast ui::theme::tests`. Expected RED: theme installer, navigation renderer, and `ControlCenterApp` shell are missing.

- [ ] Implement `install_fonts` using `FontData::from_static(INTER_VARIABLE)` as the first proportional family and IBM Plex Mono Regular as the first monospace family, keeping egui default fonts as Unicode fallback. Set Body 15, secondary 13, page title 28/34, section title 18/24, and mono data 12/13.

- [ ] Implement exact light anchors: Canvas `#F5F7FB`, Quiet surface `#E9EDF7`, Ink `#20283B`, Route blue `#5E72D6`, Streaming teal `#51B9AF`, Fault red `#C6535F`. Derive dark surfaces only by fixed Ink/Canvas mixes: canvas 92% Ink + 8% Canvas, quiet surface 84% Ink + 16% Canvas, primary text 8% Ink + 92% Canvas. Use accent tints with Ink text when direct small accent text fails AA. High contrast uses Windows foreground/background and border/shape distinctions, with no decorative wash.

- [ ] Set 32-point outer padding (24 compact), 4-point base/8-point primary spacing, 12-point card radius, one-point quiet borders, no gradients or diffuse shadows, and a two-point Route-blue focus outline with one-point neutral halo.

- [ ] Implement `ControlCenterApp::new` to install fonts/styles once and install a waker closure calling only `egui::Context::request_repaint()`. Retain `SnapshotSubscription` for the app lifetime. Map app `ThemePreference` to `egui::ThemePreference`; System uses live `Context::system_theme`, Light/Dark override it, and native title-bar theme remains synchronized.

- [ ] Define UI effects exactly as `ShowMainWindow`, `HideMainWindow`, `FocusMainWindow`, `Announce(String)`, `DropTray`, and `CloseApplication`. In `update`, drain once and map window effects to `ViewportCommand::Visible`, `Focus`, and `Close`; no window command is issued by worker threads. `Announce` updates one stable `openaircast-live-status` AccessKit node with `Role::Status`, `Live::Polite`, and the new label, so screen readers announce a changed status once rather than once per frame.

- [ ] Implement navigation with stable IDs from `Page`, the approved top mark, Home/Speakers/Groups/Audio/Diagnostics/Settings order, and bottom actual-state text: Stopped, Connecting, Streaming to N, or Needs attention. Navigation only dispatches `Navigate`.

- [ ] Run `& $cargo test -p homepod-cast ui::navigation::tests; & $cargo test -p homepod-cast ui::theme::tests`. Expected GREEN: both sizes/themes pass accessible semantic assertions and contrast thresholds.

- [ ] Run `rg -n "request_repaint_after|loop \{|sleep\(" crates/homepod-cast/src/ui`. Expected: no matches. The sole `request_repaint()` match is the installed waker.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/ui crates/homepod-cast/src/app
git commit -m "feat: add the Windows Calm control center shell"
```

### Task 11: Build the route-first Home page and daily controls

**Files:**
- Create: `crates/homepod-cast/src/ui/home.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/ui/theme.rs`

**Interfaces:**
- Consumes: Task 10's `ControlCenterApp`, Windows Calm theme helpers, immutable `UiSnapshot`, and `emit: &mut dyn FnMut(AppEvent)` rendering boundary.
- Produces: `home::show(ui: &mut egui::Ui, snapshot: &UiSnapshot, emit: &mut dyn FnMut(AppEvent))` and `home::signal_route_summary(snapshot: &UiSnapshot) -> String`, consumed by Task 14 and exercised by Task 15 acceptance.

- [ ] Add egui_kittest tests for all header labels and button states: Stopped/Start streaming, Starting/Connecting plus responsive Stop action, Streaming/Stop streaming, Stopping/Stopping disabled, and failure/Needs attention. Assert events are derived from the supplied immutable snapshot.

- [ ] Add receiver-row interaction tests: stable egui IDs from `DeviceId.0` across reorder; checkbox changes dispatch only `ToggleStagedReceiver`; dirty change bar persists in a non-Home snapshot and dispatches Apply/Discard; unavailable desired receivers remain checked and labeled Unavailable; refresh dispatches `RefreshRequested`.

- [ ] Add signal-route tests for selected-not-active, active, unavailable, connecting, and error states. Assert each exposes a complete text summary such as `This PC is streaming to Wohnzimmer and Büro` or `Three speakers selected; streaming is stopped`, independent of painted colors.

- [ ] Add volume tests that the slider has an accessible label/value, shows a numeric percent, dispatches `MasterVolumeChanged`, uses no direct persistence/controller handle, and retains a 44-point keyboard target.

- [ ] Run `& $cargo test -p homepod-cast ui::home::tests`. Expected RED: Home renderer and signal route do not exist.

- [ ] Implement the desktop hierarchy exactly: page header with actual status/actions; first full-width signal-route card; lower Speakers card plus Master volume/session-summary card; bottom inline notice/corrective action. Use 32/24 padding and collapse lower cards vertically before content clips at 900x600.

- [ ] Paint the route with one Route-blue source node/line and receiver nodes: desired inactive outlined and labeled Selected; confirmed active Streaming-teal and labeled Streaming; unavailable neutral dashed outline and labeled Unavailable. During a transition, call `animate_value_with_time` once with 0 or 0.120 seconds according to platform motion preference; do not restart it for an unchanged generation and do not animate an idle route.

- [ ] Give every receiver row this stable scope, then render name, model, availability, actual label, reserved trailing-action space, and checkbox inside `render_receiver_row`:

```rust
ui.push_id(("receiver", receiver.id.0), |ui| {
    render_receiver_row(ui, receiver, snapshot, emit);
});
```

No click changes desired/active directly. Render the persistent Apply/Discard bar whenever `staged_membership_dirty`, including outside the Home renderer through `ui/mod.rs`.

- [ ] Implement volume as immediate snapshot-driven input. Dispatch on every finite changed value; the adapter coalesces backend/persistence work. On drag release, dispatch the final displayed value once more so the final queue value is authoritative even if intermediate device commands were coalesced.

- [ ] Render one concise notice with severity text/icon/shape and a concrete action (`Refresh`, `Retry`, or `Open Settings`) derived from notice code. Queue `UiEffect::Announce` only when notice identity or actual stream state changes, never once per frame.

- [ ] Run `& $cargo test -p homepod-cast ui::home::tests`. Expected GREEN: actual-state, stable-ID, staging, route-summary, volume, and accessibility tests pass at both sizes.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/ui
git commit -m "feat: build the daily control Home page"
```

### Task 12: Complete Speakers, Groups, Audio, Diagnostics, and Settings pages

**Files:**
- Create: `crates/homepod-cast/src/ui/pages.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/ui/navigation.rs`

**Interfaces:**
- Consumes: Task 10's page dispatcher/navigation contract, Task 11's persistent Home/change-bar conventions, Task 2's `Page`/`UiSnapshot`, and the same `FnMut(AppEvent)` sink.
- Produces: `pages::show(page: Page, ui: &mut egui::Ui, snapshot: &UiSnapshot, emit: &mut dyn FnMut(AppEvent))` with functional Settings events and five non-fabricated destination views, consumed by Task 14.

- [ ] Add a parameterized headless test over all six `Page` values, both themes, and 1120x720/900x600. Assert one page title/focus target, no overflow, a predictable keyboard sequence, and no unlabeled icon-only action.

- [ ] Add page-specific tests: Speakers mirrors inventory read-only beyond Home selection; Groups has a saved-group purpose empty state and no clickable fake CRUD; Audio shows real master volume and names later controls as explanatory text only; Diagnostics shows actual app/session state and the existing `--list`/selftest/log workflow with no fabricated metrics.

- [ ] Add Settings tests for functional System/Light/Dark radio controls, editable enabled hotkey chord, validation/reconfiguration feedback, About/version/GPL text, and links or copyable paths to `LICENSE`/`THIRD_PARTY_NOTICES.md`; launch-at-startup is omitted because the integration is not implemented.

- [ ] Run `& $cargo test -p homepod-cast ui::pages::tests`. Expected RED: the five destination renderers and Settings actions are missing.

- [ ] Implement all page renderers using `UiSnapshot` plus an `emit: &mut dyn FnMut(AppEvent)` callback only. No renderer reads global state, disk, environment variables, logs, device transport, or time.

- [ ] Implement Settings hotkey editing with modifier checkboxes and a single virtual-key capture field. Reject no-modifier, modifier-only, and reserved/unknown key combinations before dispatch; disabling remains valid. Dispatch exactly one `HotkeyChanged` after explicit Apply, retaining the prior binding until the hotkey service confirms.

- [ ] Keep the dirty membership change bar above page content for every destination. Escape discards only an active transient hotkey edit; it never silently discards staged speaker membership.

- [ ] Run `& $cargo test -p homepod-cast ui::pages::tests`. Expected GREEN: all 24 page/size/theme combinations and specific semantic assertions pass.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/ui
git commit -m "feat: complete control center destinations and settings"
```

### Task 13: Rebuild tray behavior as a pure snapshot adapter

**Files:**
- Modify: `crates/homepod-cast/src/tray.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`

**Interfaces:**
- Consumes: Task 6's `AppHandle`, Task 2's immutable `UiSnapshot`/`AppEvent`, and Task 10's `ControlCenterApp` event-loop ownership.
- Produces: `TrayModel::from_snapshot(snapshot: &UiSnapshot) -> TrayModel`, `TrayController::{new(handle: AppHandle) -> anyhow::Result<Self>, sync(&mut self, snapshot: &UiSnapshot)}`, and `ControlCenterApp::install_tray(&mut self, tray: TrayController)`, consumed by Task 14.

- [ ] First add a pure `TrayModel::from_snapshot` test matrix for Stopped, Starting, Streaming, Stopping, and Needs attention. Assert Open; separator; actual Start/Stop/progress label and enabled state; Speakers submenu with desired checks and availability; separator; Settings; separator; Quit; plus actual-state tooltip.

- [ ] Add event-mapping tests: tray left-button Up dispatches `ShowMainWindow`; Open dispatches Show; Settings dispatches Navigate(Settings) then Show; Start/Stop/Quit dispatch their exact events; a clean enabled receiver item dispatches Toggle then Apply in FIFO order; unavailable items are disabled; while staged membership is dirty, receiver mutation is disabled so tray cannot silently commit window edits.

- [ ] Run `& $cargo test -p homepod-cast tray::tests::snapshot_model`. Expected RED: the current tray owns index-based `GroupSelection`, an optimistic Status mirror, and has no snapshot model.

- [ ] Implement immutable model types:

```rust
pub(crate) struct TrayModel {
    pub tooltip: String,
    pub session_action: TrayAction,
    pub receivers: Vec<TrayReceiver>,
    pub has_pending_membership: bool,
}

pub(crate) enum TrayDispatch {
    One(AppEvent),
    Batch(Vec<AppEvent>),
}
```

Build labels/checks/enabled state only from one supplied `UiSnapshot`; never patch checks when a click occurs.

- [ ] Add `TrayController` beside the legacy `run` temporarily. Construct it in eframe initialization on the event-loop thread, load the icon with `tray_icon::Icon::from_resource(1, Some((32, 32)))`, retain `TrayIcon`, and rebuild/set its menu only when `snapshot.revision` changes.

- [ ] Use `TrayIconEvent::set_event_handler` and `MenuEvent::set_event_handler` to dispatch through cloned `AppHandle`; handlers do no tray mutation. Maintain a revision-local `MenuId -> TrayDispatch` map behind a short mutex. `ControlCenterApp::update` calls `tray.sync(&snapshot)` on the eframe thread after a waker-triggered revision.

- [ ] Use these actual-state rules: Starting offers enabled Stop streaming; Streaming offers enabled Stop streaming; Stopping shows disabled Stopping…; Stopped/Failed offers Start streaming enabled only when `can_start`. Tooltip names actual state and active count, never merely desired checks.

- [ ] Run `& $cargo test -p homepod-cast tray::tests`. Expected GREEN: exact menu ordering/model and event mapping pass with no local state mirror.

- [ ] Run `rg -n "GroupSelection|thread::sleep|try_recv\(\).*50|Status::Streaming" crates/homepod-cast/src/tray.rs`. Expected: matches occur only in the explicitly marked legacy block that Task 14 will remove.

- [ ] Run `& $cargo fmt --all -- --check; & $cargo test -p homepod-cast` and commit:

```powershell
git add crates/homepod-cast/src/tray.rs crates/homepod-cast/src/ui/mod.rs
git commit -m "feat: derive tray controls from app snapshots"
```

### Task 14: Wire normal launch, close/restore, bounded shutdown, and remove Raw-Win32 settings

**Files:**
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/tray.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Delete: `crates/homepod-cast/src/settings_window.rs`
- Delete: `crates/homepod-cast/src/group_state.rs`

**Interfaces:**
- Consumes: Task 6's `start_app`/`AppHandle`/`AppRuntime`, Task 7's preference repository/worker handle, Task 8's `DeviceServiceHandle`, Task 9's `HotkeyServiceHandle`, Task 10's `ControlCenterApp`, and Task 13's `TrayController`.
- Produces: `app::run() -> anyhow::Result<()>` as the normal-launch path called by `main()`, with `--list`, `--selftest`, and `--selftest-group` continuing through their existing signatures; also produces bounded shutdown and one-window/one-tray runtime behavior consumed by Task 15 validation.

- [ ] Add integration-style module tests with fake device/preferences/hotkey ports for startup order; immediate initial snapshot; startup Discover dispatch; main close Hide only; tray Show producing Show+Focus UI effects and preserving page; explicit Quit effect order; controller channel loss retaining working Quit; cleanup deadlines producing CloseApplication even when one fake port never acknowledges.

- [ ] Run `& $cargo test -p homepod-cast app::tests::lifecycle`. Expected RED: the production service assembly and bounded shutdown coordinator are missing.

- [ ] Implement normal startup in this order: initialize tracing; preserve diagnostic argument branches exactly; resolve/load shell preferences; start preference/device/hotkey workers; create initial `AppState`; start actor/effect executor; call `eframe::run_native`; inside the creation closure install theme/fonts, construct exactly one tray icon, retain `ControlCenterApp`, and dispatch `RefreshRequested` after the window is visible.

- [ ] Configure native options exactly:

```rust
let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default()
        .with_title("OpenAirCast")
        .with_inner_size([1120.0, 720.0])
        .with_min_inner_size([900.0, 600.0])
        .with_icon(load_embedded_icon()),
    ..Default::default()
};
```

Restore validated saved geometry only when its rectangle intersects a current monitor; otherwise use the centered 1120x720 default. Let eframe/winit apply logical DPI scaling.

- [ ] Intercept `ctx.input(|i| i.viewport().close_requested())`, send `ViewportCommand::CancelClose`, and dispatch `MainWindowCloseRequested`. Consume Hide with `ViewportCommand::Visible(false)`; consume Show with Visible(true) then Focus. Never create a second viewport or settings window.

- [ ] Implement production `EffectExecutor`: controller effects call `ControllerPort::try_send`; preference effects schedule the worker; hotkey effects queue the hotkey service; window effects queue `UiEffect`; failures dispatch typed events/notices. It returns after queueing each operation.

- [ ] Implement `BeginShutdown` once on a coordinator thread. The reducer's preceding `HideMainWindow` effect hides the viewport; the coordinator queues DropTray immediately, enqueues device Stop/Shutdown and waits at most 3 seconds, stops/unregisters hotkey and waits at most 1 second, flushes the preference worker and waits at most 2 seconds, then queues CloseApplication regardless of acknowledgements. Log timeout categories without secrets.

- [ ] After `run_native` returns from explicit Quit, call `std::process::exit(0)` only as the documented final fallback for the known upstream infinite `spawn_blocking` leak, after the bounded cleanup sequence above has run. Do not use forced exit for diagnostic modes before their existing runtime shutdown path completes.

- [ ] Remove the legacy `tray::run`, `Cmd`, `Status`, 50 ms pump, inline `RegisterHotKey`, `control_loop`, and hand-painted tray icon. Remove `settings_window.rs`, its module declaration, and all callbacks. Remove `group_state.rs` and port/delete its index-based tests because reducer identity/staging tests now cover the behavior.

- [ ] Remove `cast::load_hotkey`/`save_hotkey` after confirming migration reads the same legacy path; remove `cast::load_volume`/`save_volume` after confirming `LegacyDevicePreferences` owns the same path and default 0.25 behavior. Keep all audio/session tests unchanged.

- [ ] Run `& $cargo test -p homepod-cast app::tests::lifecycle`. Expected GREEN: launch/close/show/quit and deadline tests pass.

- [ ] Run `rg -n "settings_window|GroupSelection|RegisterHotKey|thread::sleep\(Duration::from_millis\(50\)\)|Runtime::block_on|block_on\(" crates/homepod-cast/src`. Expected: `RegisterHotKey` only in `platform/hotkey.rs`; `block_on` only in diagnostic `main.rs` branches and `device_service.rs`; no other pattern matches.

- [ ] Run the full automated gates:

```powershell
& $cargo fmt --all -- --check
& $cargo test -p homepod-cast
& $cargo test --workspace --all-targets
& $cargo build -p homepod-cast --release
```

Expected GREEN: all commands exit 0; the three original cast tests still pass; release uses the Windows GUI subsystem.

- [ ] Commit the integration boundary:

```powershell
git add crates/homepod-cast/src/main.rs crates/homepod-cast/src/app crates/homepod-cast/src/ui crates/homepod-cast/src/tray.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/src/device_service.rs crates/homepod-cast/src/preferences.rs crates/homepod-cast/src/platform crates/homepod-cast/src/settings_window.rs crates/homepod-cast/src/group_state.rs
git commit -m "refactor: launch the unified OpenAirCast control center"
```

### Task 15: Update notices/docs, package one executable, and complete acceptance

**Files:**
- Modify: `README.md`
- Modify: `THIRD_PARTY_NOTICES.md`
- Modify: `build.ps1`
- Modify: `docs/VALIDATION.md`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: Task 14's `app::run() -> anyhow::Result<()>`, release `target\release\openaircast.exe`, the three frozen diagnostic CLI modes, and automated gates `cargo test -p homepod-cast`, `cargo test --workspace --all-targets`, and `cargo build -p homepod-cast --release`.
- Produces: Self-contained `dist\OpenAirCast.exe`, updated GPL/MIT/SIL-OFL notices and user guidance, plus `docs/VALIDATION.md` evidence for acceptance items 1–16; no later code task depends on an undocumented interface.

- [ ] Run this focused documentation assertion before editing README and observe RED for the old tray-only usage text; rerun it after the edit and require GREEN:

```powershell
$readme = Get-Content -Raw README.md
$required = @("main window", "notification area", "Apply speaker changes", "Discard", "close", "hotkey", "theme", "--list", "--selftest-group", "Windows Firewall")
$missing = $required | Where-Object { $readme -notmatch [regex]::Escape($_) }
if ($missing.Count -ne 0) { throw "README is missing: $($missing -join ', ')" }
```

- [ ] Rewrite README current features/usage/limitations to match the shipped shell. State that Speakers/Groups/Audio/Diagnostics destinations intentionally expose current data and purpose-specific future-capability explanations; do not claim saved groups, reconnect, endpoint selection, per-speaker controls, or measured sync diagnostics.

- [ ] Add Inter Variable, IBM Plex Mono, egui/eframe/wgpu/AccessKit, tray-icon, ArcSwap, image/ico, serde/serde_json, and winresource to `THIRD_PARTY_NOTICES.md` with upstream project URL, exact locked version or pinned font commit, and the MIT or SIL OFL option. Keep GPL-2.0 application/upstream notices intact and do not describe the combined application as Apache-2.0.

- [ ] Update `.gitignore` for `**/tests/snapshots/**/*.new.png` and `**/tests/snapshots/**/*.diff.png`. Update `build.ps1` to use the existing Cargo fallback, run the release build, copy only `target\release\openaircast.exe` to `dist\OpenAirCast.exe`, and fail if the embedded-font/icon asset tests did not run successfully in the same invocation.

- [ ] Run packaging and prove resources are embedded:

```powershell
./build.ps1
Get-Item dist/OpenAirCast.exe | Select-Object FullName,Length,LastWriteTime
Get-FileHash -Algorithm SHA256 dist/OpenAirCast.exe
Get-ChildItem dist -File
```

Expected: `dist\OpenAirCast.exe` is the only file in `dist`. Copy that executable alone to an otherwise empty temporary directory and launch it there; fonts, icon, and UI load with no adjacent DLL, font, WebView2, Qt, or other runtime file.

- [ ] Compare the release size with the audited 4,932,096-byte baseline and record the new byte count plus renderer/features in `docs/VALIDATION.md`; size growth is reported, not hidden by dropping accessibility or embedded assets.

- [ ] Complete automated release verification:

```powershell
& $cargo fmt --all -- --check
& $cargo test -p homepod-cast
& $cargo test --workspace --all-targets
& $cargo build -p homepod-cast --release
```

- [ ] On Windows 10 and Windows 11, verify acceptance 1–7: exactly one visible 1120x720 window and tray icon; all Home controls; close hides without stopping; left-click restores/focuses the same page; window/tray agree after success/failure/stop/rapid Apply; stale results cannot overwrite; runtime hotkey replacement; multi-second fake/real discovery does not freeze movement/navigation/Close/Stop.

- [ ] Verify acceptance 8–10 with Accessibility Insights plus Narrator and NVDA: keyboard reaches controls in visual order with visible focus and expected Enter/Space/arrows/Escape; Home/Settings have no critical errors; receiver checkbox, volume value, actual state change, and one failure notice are announced once; light/dark/high-contrast retain AA contrast and non-color distinctions.

- [ ] Verify acceptance 11–13: move the running window across 100%, 125%, 150%, 175%, and 200% monitors with no clip/blur/duplicate/jump; toggle Windows client animations and observe immediate versus at-most-120 ms one-shot transitions; exercise wgpu under Remote Desktop; hide for at least 10 minutes and verify a tray click still wakes/restores; restart Windows Explorer and verify exactly one working tray icon is recreated; measure visible idle and hidden-to-tray for 60 seconds each with no continuous frames and average process CPU below 1% on the validation machine.

- [ ] Verify acceptance 14–16: one portable executable; explicit Quit stops when possible and terminates within the 6-second bounded cleanup plus immediate forced-exit fallback; run `--list`, `--selftest`, and a three-HomePod `--selftest-group` using the existing validation procedure and confirm one capture and single-/multi-receiver paths remain executable.

- [ ] Record OS build, GPU/driver, Remote Desktop wgpu result, DPI monitors, screen readers/versions, CPU samples, shutdown time, binary size/hash, receiver models, and pass/fail for all 16 acceptance items in `docs/VALIDATION.md`. A failing manual item blocks release; it is not converted into a documentation caveat.

- [ ] Run the final scope/license/source audit:

```powershell
rg -n "WebView2|Qt|proprietary|per-seat|hosted service" crates/homepod-cast README.md THIRD_PARTY_NOTICES.md
rg -n "request_repaint_after|thread::sleep\(Duration::from_millis\(50\)\)|GroupSelection|settings_window" crates/homepod-cast/src
git status --short
git diff --check
```

Expected: no proprietary/runtime dependency claim; no polling/index/raw-settings match; status contains only intentional subproject files; `git diff --check` is silent.

- [ ] Commit documentation and release evidence:

```powershell
git add README.md THIRD_PARTY_NOTICES.md build.ps1 docs/VALIDATION.md .gitignore
git commit -m "docs: validate and document the OpenAirCast control center"
```

## Complete Spec Self-Check

- [ ] Goals/non-goals: dedicated window plus tray, daily-control Home, six final destinations, functional settings, preserved AirPlay path, and explicitly deferred device resilience/richer controls are represented in Tasks 8–15.
- [ ] Visual system: all six anchor colors, fixed light/dark/high-contrast derivation, Inter/IBM Plex Mono, geometry, type sizes, spacing, card radius, focus, native title bar, and route signature are implemented/tested in Tasks 1, 10, and 11.
- [ ] State semantics: stable `DeviceId`, staged/desired/active separation, dirty/stale conflict rule, desired/active truth, generation IDs, matching-result rule, and exact-once revision publication are covered by Tasks 2–6.
- [ ] Contracts: all four `AppHandle` methods, immutable bounded `UiSnapshot`, non-blocking ports, atomic publication, waker lifecycle, UI-effect draining, and future `DeviceBackendHandle`/`DeviceSnapshot` replacement seam are fixed in Tasks 2, 6, and 8.
- [ ] Window/tray/hotkey/quit: visible startup, close-to-tray, restore/focus same page, snapshot-derived menu/tooltip, dedicated hotkey thread and matrix, bounded cleanup, and forced-exit fallback are covered by Tasks 5, 9, 13, and 14.
- [ ] Persistence: schema-v1 canonical path, legacy hotkey-only migration, domain separation, validation, atomic replace, debounce, geometry restore, final flush, and non-fatal failures are covered by Tasks 5, 7, 8, and 14.
- [ ] Accessibility/DPI/theme/motion: AccessKit in release, names/labels/tab order/44-point targets/focus/live announcements, logical mixed DPI, live System theme, explicit overrides, high contrast, Windows reduced motion, and screen-reader validation are covered by Tasks 9–12 and 15.
- [ ] Repaint/performance: no UI blocking, polling, unconditional repaint, continuous animation, unstable row IDs, or backend keepalive presentation churn; hidden wake/restore and CPU validation are covered by Tasks 6, 10, 11, 13, and 15.
- [ ] Migration: `tray.rs` becomes an adapter, `settings_window.rs` and `group_state.rs` are removed after parity, `cast.rs` retains transport/capture, `main.rs` retains diagnostic routing, and packaging/docs/notices are updated in Tasks 8, 13–15.
- [ ] Automated tests: all 14 reducer behaviors, seven AppHandle contract categories, six preference categories, seven UI/tray categories, original package tests, workspace/all-targets, and release build have explicit RED/GREEN commands and final gates.
- [ ] Acceptance: Task 15 exercises and records every numbered acceptance item 1–16, including Windows versions, real accessibility tools, DPI matrix, idle CPU, one-file packaging, bounded Quit, and three-HomePod validation.
- [ ] License constraints: exact GPL-2.0/MIT/SIL OFL treatment, pinned font provenance/hashes, no proprietary runtime/service, and notices audit are covered by Tasks 1 and 15.
- [ ] Type consistency: `DeviceId` remains the current non-`Copy` `airplay_core` type and is cloned at ownership boundaries; `GenerationId` is `Copy`; only presentation-safe owned data enters `UiSnapshot`; `Preferences` contains shell fields only; device commands carry complete sorted receiver IDs and generations.
- [ ] Implementation readiness: every changed/created/deleted file is named, every behavioral task starts RED and ends GREEN, commands are PowerShell-compatible in the audited environment, each task has one commit boundary, and no unresolved design decision remains.
