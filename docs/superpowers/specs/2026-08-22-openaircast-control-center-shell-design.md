# OpenAirCast Control Center Shell Design

Date: 2026-08-22
Status: Approved for implementation planning
Subproject: 1 of the OpenAirCast control-center program

## Summary

OpenAirCast will evolve from a tray-only Windows utility into a dedicated control
center with a persistent system-tray companion. The application uses `egui` and
`eframe` as a fully Rust-native GUI stack under their MIT license option. The main
window presents daily controls first, while a stable navigation shell reserves
clear homes for speaker management, audio configuration, diagnostics, and
settings.

The GUI is a projection of one centralized application state. Main-window input,
tray input, the global hotkey, persistence results, and device-backend results
all enter the same reducer as `AppEvent` values. The reducer changes state and
emits explicit `AppEffect` values. The UI consumes immutable `UiSnapshot` values
and communicates through a cloneable `AppHandle`; it never owns an AirPlay
session, Tokio runtime, discovery browser, or settings file.

This boundary deliberately anticipates subproject 2. The first subproject adapts
the existing control loop behind a private device-service boundary. The second
replaces that adapter with `DeviceBackendHandle` and `DeviceSnapshot` while the
public `AppHandle`/`UiSnapshot` contract used by views, tray, and hotkey remains
unchanged.

## Repository Context

The current `homepod-cast` application already has a useful separation at the
protocol boundary:

- `main.rs` keeps the `--list`, `--selftest`, and `--selftest-group` diagnostic
  paths separate from normal GUI startup.
- `cast.rs` owns UI-independent discovery, WASAPI loopback, and one active
  `Session` containing either the single-receiver or group transport.
- `group_state.rs` is a pure selection model, but its receiver indices are not
  stable enough for background rediscovery.
- `tray.rs` currently combines the Win32 pump, tray presentation, optimistic UI
  state, channels, global hotkey, Tokio runtime, discovery, session control,
  keepalive, and shutdown in one 371-line module.
- `settings_window.rs` is a separate 333-line raw-Win32 UI running on its own
  thread, with fixed pixel geometry and callback-based state propagation.
- `cast.rs` also contains volume and hotkey file persistence, even though those
  settings are application preferences rather than casting concerns.
- Shutdown currently ends with `process::exit(0)` because an upstream blocking
  task can outlive the runtime. The shell must preserve bounded termination while
  making the fallback explicit.

The existing multi-room architecture remains authoritative: the control side
owns Tokio and one `cast::Session`; the UI sends complete desired receiver sets;
membership changes currently renegotiate the session; one receiver uses NTP and
multiple receivers use the shared PTP group path.

## Goals

1. Provide one dedicated, resizable Windows 10/11 main window and one persistent
   notification-area icon in the same process.
2. Make the Home page sufficient for the everyday loop: inspect actual status,
   choose receivers, start or stop streaming, and adjust master volume.
3. Establish the complete control-center navigation shell without prematurely
   implementing the deeper features assigned to later subprojects.
4. Maintain one authoritative `AppState` and make desired state, in-progress
   operations, and confirmed active state visibly distinct.
5. Define a non-blocking `AppHandle`/`UiSnapshot` boundary that subproject 2
   can extend without changing view code.
6. Preserve the existing CLI diagnostics, Windows GUI subsystem release behavior,
   portable single-executable build, multi-room transport, and hardware-tested
   audio path.
7. Meet Windows expectations for keyboard use, screen readers, mixed-DPI
   monitors, system light/dark preference, reduced motion, and low idle resource
   use.
8. Remove the raw-Win32 settings UI and reduce direct Win32 code to narrowly
   scoped platform services such as the global hotkey.

## Non-goals

This subproject does not implement:

- continuous discovery, automatic receiver rejoin, or partial group recovery;
- dynamic insertion or removal of a receiver from a running upstream streamer;
- saved groups, per-speaker volume, output-device selection, latency presets,
  equalization, or acoustic synchronization measurement;
- a full log viewer, packet inspector, or long-run diagnostics engine;
- pairing-secret persistence or Windows Credential Manager integration;
- installer, auto-update, or Microsoft Store packaging; startup registration is
  deferred to subproject 2's Settings integration;
- protocol changes in the `airplay-*` crates;
- cross-platform visual parity outside Windows 10/11;
- multiple simultaneous OpenAirCast processes or sessions.

Navigation destinations for later features are real pages with explanatory empty
states, not enabled controls that pretend unavailable operations work.

## Fixed Technology Decisions

### GUI toolkit

Use `egui`/`eframe` `=0.36.1` with the `wgpu` renderer and AccessKit enabled.
Applications render directly through Rust and native graphics APIs; no HTML,
JavaScript, WebView2, Electron runtime, Qt DLL, or external UI resource directory
is introduced. Keep the exact versions in `Cargo.toml` and `Cargo.lock` for the
full subproject because egui APIs may change between releases.

The app keeps the existing `tray-icon` 0.24 integration. The tray icon is created
from the eframe/winit event-loop thread and retained for the process lifetime.
Tray and menu event handlers send `AppEvent` values through `AppHandle` and
request an egui repaint; the UI does not poll them every frame.

### Fonts and icons

Bundle these open fonts into the executable with `include_bytes!` and register
them in `egui::FontDefinitions` before the first frame:

- Inter Variable for headings, body text, navigation, buttons, and form labels;
- IBM Plex Mono Regular for receiver identifiers, addresses, timing values, and
  diagnostic data only.

Both font families are distributed under SIL Open Font License 1.1. Their license
texts live next to the source assets and are named in `THIRD_PARTY_NOTICES.md`.
Do not use IBM Plex Mono for ordinary labels or paragraphs.

Use vector paths or embedded SVG/PNG data for in-window icons. The Windows tray
asset contains at least 16, 20, 24, and 32 pixel variants so DPI scaling does not
blur a single raster source.

## Visual Direction: Windows Calm

The design is quiet, spacious, and operational rather than decorative. It borrows
Windows 11's clear hierarchy and soft grouping without attempting to clone system
Settings. It uses one expressive device—the signal route—and keeps the remaining
surface restrained.

### Approved anchor tokens

| Token | Value | Use |
|---|---:|---|
| Canvas | `#F5F7FB` | Main light-theme background and high-emphasis dark-theme text |
| Quiet surface | `#E9EDF7` | Sidebar, grouped controls, selected-row wash |
| Ink | `#20283B` | Primary light-theme text and dark-theme canvas |
| Route blue | `#5E72D6` | Primary action, focus, PC/source portion of signal route |
| Streaming teal | `#51B9AF` | Confirmed active state and receiver portion of signal route |
| Fault red | `#C6535F` | Errors and destructive emphasis only |

These six colors are the brand anchors. White, black, and alpha blends may be used
only as neutral contrast helpers. Dark-theme surfaces are derived by mixing Ink
with Canvas at fixed semantic levels; they are not additional brand colors.
Route blue, Streaming teal, and Fault red never carry meaning by color alone:
each occurrence also has text, an icon, shape, or state label.

All text/background and control-state combinations must meet WCAG 2.2 AA contrast:
4.5:1 for ordinary text and 3:1 for large text, focus indicators, and meaningful
non-text graphics. If an approved accent does not meet contrast at a given size,
use Ink text on an accent tint or the accent as a border/icon instead of placing
small white text directly on it.

### Geometry and type

- Initial main-window client size: 1120 by 720 logical points.
- Minimum client size: 900 by 600 logical points.
- Navigation rail: 224 logical points wide at 1120 points and above; below that,
  collapse to a 64-point icon rail with accessible tooltips and names.
- Outer content padding: 32 points; compact layout padding: 24 points.
- Spacing uses a 4-point base and an 8-point primary rhythm.
- Cards use 12-point corner radii, one-point quiet borders, and no large diffuse
  shadows.
- Interactive rows and buttons are at least 44 points high.
- Body text is 15 points; secondary text 13; page title 28/34; section title
  18/24; data labels in IBM Plex Mono are 12 or 13 points.
- The native title bar remains enabled. Custom title-bar painting is outside this
  subproject.

### Signature signal route

The first Home-page card contains a horizontal route from **This PC** to the
selected receivers. A Route-blue source node and line lead to one or more receiver
nodes. Desired but not active nodes are outlined and labeled **Selected**;
confirmed active nodes use Streaming teal and **Streaming**; unavailable nodes
use a neutral dashed outline and **Unavailable**. During connection, a short
120-millisecond directional transition may move once along the route. There is no
continuous ambient animation.

The card also exposes an equivalent accessible text summary, for example:
"This PC is streaming to Wohnzimmer and Büro" or "Three speakers selected;
streaming is stopped." The diagram itself is not the only status source.

## Information Architecture and Layout

### Persistent navigation

The left rail contains these destinations in this order:

1. **Home** — daily controls and current route.
2. **Speakers** — discovery inventory and group composition.
3. **Groups** — saved receiver groups and one-action activation.
4. **Audio** — source, volume, and later processing controls.
5. **Diagnostics** — connection, timing, buffer, and validation information.
6. **Settings** — appearance, hotkey, startup behavior when implemented, About,
   licenses, and version information.

OpenAirCast name and mark sit at the top. A compact actual-state indicator sits
above Settings at the bottom: **Stopped**, **Connecting**, **Streaming to N**, or
**Needs attention**. Navigation changes `Page` locally through the reducer and
does not perform controller work.

### Home page

The approved desktop layout is:

```text
┌──────── navigation ────────┬──────────────────────────────────────────────┐
│ OpenAirCast                │ Home                    [status] [Start/Stop]│
│                            ├──────────────────────────────────────────────┤
│ Home                       │ This PC ───── selected/active receivers      │
│ Speakers                   │ signature signal-route card                 │
│ Groups                     │                                              │
│ Audio                      ├───────────────────────────┬──────────────────┤
│ Diagnostics                │ Speakers                 │ Master volume    │
│                            │ selectable receiver rows │ slider + percent │
│                            │ refresh / empty states   │ session summary  │
│ state                      │                           │                  │
│ Settings                   ├───────────────────────────┴──────────────────┤
└────────────────────────────┴─ inline notice / next corrective action ────┘
```

The header always shows the actual session state. Its primary button reads
**Start streaming**, **Stop streaming**, **Connecting…**, or **Stopping…**.
Connecting and stopping disable duplicate activation but keep navigation,
receiver inspection, window movement, and cancellation through Stop responsive.

The Speakers card lists discovered receivers in stable display-name order while
identifying them internally by `DeviceId`. Each row has a checkbox for staged
membership, name, model, availability, and actual-state label. Checkbox edits
never interrupt playback. While the staged set differs from committed desired
membership, a persistent change bar offers **Apply speaker changes** and
**Discard**. Apply commits the complete set and causes at most one group
renegotiation; Discard restores the latest committed set.

The right card contains master volume, numeric percent, and a concise session
summary. Slider movement updates the snapshot immediately, coalesces backend
volume commands, and schedules debounced persistence. It does not synchronously
write a file on the UI thread.

The approved high-fidelity reference is the end-state control center: its quick
group actions and receiver-level controls become functional in subproject 2.
Subproject 1 preserves their intended hierarchy through the Groups destination,
session summary, and receiver-row trailing-action space, but shows a clear
purpose-specific empty state instead of clickable placeholders. Subproject 2
fills those reserved areas without changing the Home page's route-first layout.

Errors appear as a concise inline notice with a concrete corrective action.
Starting failure returns actual state to Stopped while retaining desired speaker
selection for retry.

### Other pages in subproject 1

Each destination has final navigation, page title, keyboard focus target, and a
purpose-specific empty state:

- Speakers explains that discovered devices and group details appear here and
  mirrors the current receiver inventory read-only beyond Home's selection.
- Groups explains saved groups and one-action activation. Subproject 2 replaces
  the empty state with functional create, rename, delete, reorder, and activate
  flows.
- Audio shows current master volume and identifies output-device, latency, and
  processing controls as subsequent capabilities without interactive fake inputs.
- Diagnostics shows current application/session state and links the user to the
  existing documented validation/log workflow; invented sync metrics are never
  displayed.
- Settings contains functional theme selection, hotkey editing, and About/license
  information. Options not implemented in this subproject are omitted.

## State, Events, and Effects

### Stable identities and generations

Receiver selection uses `airplay_core::DeviceId`, never a vector index. Every
operation that can complete asynchronously carries a monotonically increasing
`GenerationId(u64)`. Reducer state stores the latest generation for discovery,
session, volume, and preference writes where ordering matters. Completion events
whose generation is older than the stored generation are ignored and logged at
debug level.

### Authoritative state

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
```

`staged_receivers` contains shell-local checkbox edits. `desired_receivers` is
the last committed group and `active_receivers` is populated only by a successful
device-backend result. When staged and desired differ, the change bar remains
visible across navigation. Stop clears active membership but preserves desired
and staged membership. A failed start also clears actual membership without
destroying either selection.

When staged membership is clean, any newer desired revision refreshes the stage
and base revision automatically. When it is dirty, an external desired change
keeps the user's stage intact and marks it stale; Apply deliberately replaces the
complete committed set, while Discard loads the newest desired set. This is the
same conflict rule used by saved-group activation in subproject 2.

`StreamState` is one of `Stopped`, `Starting { generation }`,
`Streaming { generation }`, `Stopping { generation }`, or
`Failed { generation, summary }`. The UI never infers Streaming merely because a
Start command was accepted.

### Event input

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
```

Window widgets, tray callbacks, and the hotkey service only create these events.
They do not directly modify menu checks, streaming labels, or persistence files.

### Pure transition

```rust
pub struct Transition {
    pub effects: Vec<AppEffect>,
    pub snapshot_changed: bool,
}

pub fn reduce(state: &mut AppState, event: AppEvent) -> Transition;
```

The reducer is deterministic and performs no I/O, blocking work, logging setup,
clock reads, or GUI calls. Generation IDs and timestamps are supplied in events
or by the effect executor so reducer tests remain deterministic.

### Effects

```rust
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
```

The effect executor maps controller effects to the private device-service
adapter, preferences effects to the shell settings repository, hotkey effects to
the platform service, and window effects to queued `UiEffect` values. Applying
staged membership first commits the complete desired set and then emits at most
one controller reconciliation effect. Completion returns as a new event through
the reducer.

## AppHandle and UiSnapshot Contract

`AppHandle` is the only top-level gateway shared with the eframe application,
tray event handlers, and hotkey callback. It is cheap to clone, `Send + Sync`,
and never blocks on network, audio, disk, or thread shutdown. Later subprojects
may add read-only scoped accessors, such as `diagnostics_handle()`, without
exposing device commands or mutable transport state.

```rust
#[derive(Clone)]
pub struct AppHandle { /* private channels and snapshot publication */ }

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

The eframe app installs a waker that calls `egui::Context::request_repaint()`.
Publishing a changed snapshot or a `UiEffect` invokes it once. Dropping the
returned `SnapshotSubscription` unregisters the callback. The callback performs
no rendering and may be invoked from a backend thread.

`UiSnapshot` is immutable and contains only presentation-safe owned values:

```rust
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
```

`AppHandle::snapshot()` returns the latest `Arc` without waiting for new data.
`revision` increases exactly once per published semantic state change. Receiver
arrays are already sorted for display. No `cast::Session`, `Device` socket data,
Tokio handle, channel receiver, secret, or mutable collection crosses into the UI.

Subproject 2 may add fields to `DeviceEvent`, internal `AppState`, and
`UiSnapshot` while preserving these four `AppHandle` methods and the meaning of
existing snapshot fields. Subproject 3 may add a read-only scoped diagnostics
accessor. `DeviceSnapshot` remains an internal input to the shell reducer/effect
executor rather than a second presentation model. Long-running operations run
outside the reducer actor and report generation-tagged results.

## Tray, Hotkey, Window, and Shutdown Behavior

### Tray menu

Right-clicking the tray icon opens this order:

1. **Open OpenAirCast**
2. separator
3. **Start streaming** or **Stop streaming**, based on actual/in-progress state
4. **Speakers** submenu with desired receiver checks and availability labels
5. separator
6. **Settings**
7. separator
8. **Quit OpenAirCast**

Left-clicking the icon dispatches `ShowMainWindow`. The tooltip reflects actual
state: `OpenAirCast — Stopped`, `OpenAirCast — Connecting`, or
`OpenAirCast — Streaming to N speakers`. Menu checks represent desired membership;
the action label and tooltip represent actual state. The menu is rebuilt from the
latest snapshot before display or on snapshot change, never optimistically in the
event handler.

### Window close

The main window is visible on normal startup. The title-bar Close button dispatches
`MainWindowCloseRequested`, which reduces to `HideMainWindow`; it does not stop an
active session and does not quit the process. Opening from tray restores and
focuses the existing window, preserving page, scroll, and selection state.

### Global hotkey

The default remains Ctrl+Alt+H unless migrated preferences specify another
binding. A dedicated platform thread owns `RegisterHotKey`, its Win32 message
queue, re-registration, and shutdown. A press dispatches `GlobalHotkeyPressed`:

- actual Streaming or Starting requests Stop;
- Stopped with a non-empty desired group requests Start;
- Stopped with no desired group selects the first available receiver, records it
  as desired, and requests Start;
- no available receiver leaves the app stopped and publishes a corrective notice.

Registration failure preserves the previous working binding when possible and
appears in Settings as a specific error. Raw `WM_HOTKEY` handling is not attached
to eframe's winit loop.

### Quit

Quit disables new commands, hides tray and main window, asks the controller to
stop the session, shuts down the hotkey service, flushes the latest preferences,
and waits for a bounded controller acknowledgement. If the known leaked upstream
task prevents normal runtime destruction after that bounded sequence, the existing
forced process exit remains the final fallback and is logged before invocation.

## Persistence Ownership and Migration

All shell preference I/O belongs to `preferences.rs` behind a
`PreferencesRepository`. Neither views, tray code, `cast.rs`, nor the reducer
reads or writes files. Device-domain persistence is a separate service boundary:
subproject 1's legacy adapter temporarily owns volume and committed membership,
and subproject 2 replaces it with `%APPDATA%\OpenAirCast\state-v1.json`.

The canonical shell file is `%APPDATA%\OpenAirCast\settings.json`, versioned with
`"schema_version": 1`. It stores:

- hotkey binding or explicit disabled state;
- `System`, `Light`, or `Dark` theme preference;
- last main-window size, position, and maximized state;
- last selected navigation page;
- close-to-tray behavior and, once implemented, start-with-Windows preference.

It does not store actual streaming state, transient notices, IP addresses,
discovery availability, operation generations, logs, or pairing secrets.

On first load without the canonical file, migrate the existing
`%APPDATA%\HomePodCast\hotkey.txt`. Validate modifier/key values, retain the
original legacy file, and atomically write the new JSON through a same-directory
temporary file plus replace. Legacy `volume.txt` migration belongs to the
device-state adapter/backend. Malformed preferences produce defaults and a
non-fatal notice rather than preventing app startup.

Volume commands are coalesced while dragging, with the final value always sent on
drag release; device-state persistence is scheduled outside the shell preference
repository. Shell preference writes are debounced by 500 milliseconds and
flushed on orderly quit. Window geometry writes are debounced by one second.

## Accessibility, DPI, Theme, and Motion

### Accessibility

- Enable eframe's AccessKit feature in release and debug builds.
- Every icon-only control has a stable accessible name and visible tooltip.
- Each input has an explicit label relationship; speaker rows expose checkbox,
  name, availability, desired state, and actual state in a predictable order.
- Keyboard order follows visual order: navigation, header action, signal summary,
  receiver rows, volume, notice action.
- Enter activates focused buttons; Space toggles checkboxes; arrow keys adjust the
  focused volume slider; Escape closes transient overlays but never the app.
- Focus uses a two-point Route-blue outline plus a neutral contrast edge and is
  visible in both themes.
- Stream changes and errors are exposed as polite live announcements. A connection
  failure announces once and does not repeat on every frame.
- The signal-route diagram has an equivalent text node and is never the sole
  representation of topology or status.
- Minimum pointer target size is 44 by 44 logical points.
- Narrator and NVDA behavior is part of acceptance, not inferred solely from
  AccessKit tree construction.

### High DPI

All layout dimensions are logical egui points. Font glyphs are rendered at the
current viewport scale; window and tray raster assets provide suitable source
sizes. The app must tolerate live movement among 100%, 125%, 150%, 175%, and 200%
scaled monitors without rebuilding state, clipping content, or changing the
saved logical window size.

### Light, dark, and high contrast

`ThemePreference::System` follows the current OS light/dark setting and responds
while running. Explicit Light or Dark overrides only the app. Both variants derive
all component colors from the approved semantic anchors. Windows high-contrast
mode disables nonessential fills and shadows, strengthens borders, and keeps all
state labels visible; color is never the only differentiator.

### Reduced motion

Read the Windows client-animation preference at startup and when settings change.
When animations are disabled, set egui animation time to zero and render every
state transition immediately. Otherwise transitions are at most 120 milliseconds.
There are no looping gradients, pulsing status dots, continuous route motion, or
parallax effects.

## Repaint and Performance Policy

The eframe `update` method does not call unconditional `request_repaint()` or
`request_repaint_after()`. Repaint occurs only for:

- OS/window input;
- publication of a new snapshot or `UiEffect` through the installed waker;
- an active, bounded transition of at most 120 milliseconds;
- a native window expose, resize, DPI, or theme event.

The UI performs no discovery, file I/O, network call, blocking receive, thread
join, or Tokio `block_on`. Receiver rows reuse stable egui IDs derived from
`DeviceId`. Large diagnostic collections introduced later must be virtualized;
subproject 1 keeps only bounded summaries in snapshots.

While visible and unchanged for 60 seconds, the app must not generate a periodic
frame stream. While hidden in the tray, no egui animation or polling timer remains
active. Backend keepalive work may continue independently and must not publish a
new snapshot unless presentation-relevant state changes.

## Error Handling

- GUI bootstrap errors return from `main` with context; release mode records them
  before showing a native fatal dialog.
- Backend channel closure changes the snapshot to **Needs attention**, disables
  Start, retains desired selection, and keeps Quit available.
- Discovery failure distinguishes no receivers found from a scan error.
- Start failure includes the affected receiver names, returns actual state to
  Stopped, and retains desired membership for retry.
- Volume application failure retains the chosen preference, marks it unapplied in
  the session summary, and avoids snapping the slider repeatedly.
- Hotkey failure is localized to the hotkey service and never terminates streaming.
- Rendering never exposes raw `anyhow` chains, IP secrets, keys, or protocol dumps;
  user notices contain a short summary and one actionable next step while full
  context remains in tracing logs.

## File and Module Migration

### Create

- `crates/homepod-cast/src/app/mod.rs` — backend actor bootstrap and lifecycle.
- `crates/homepod-cast/src/app/state.rs` — authoritative mutable `AppState`.
- `crates/homepod-cast/src/app/event.rs` — `AppEvent` and service result events.
- `crates/homepod-cast/src/app/effect.rs` — `AppEffect`, executor, and `UiEffect`.
- `crates/homepod-cast/src/app/reducer.rs` — pure transition function and tests.
- `crates/homepod-cast/src/app/snapshot.rs` — immutable presentation models.
- `crates/homepod-cast/src/app_handle.rs` — public `AppHandle` contract.
- `crates/homepod-cast/src/ui/mod.rs` — eframe `OpenAirCastApp` and font setup.
- `crates/homepod-cast/src/ui/theme.rs` — tokens, light/dark/high-contrast styles.
- `crates/homepod-cast/src/ui/navigation.rs` — persistent rail and page routing.
- `crates/homepod-cast/src/ui/home.rs` — signal route and daily controls.
- `crates/homepod-cast/src/ui/pages.rs` — Speakers, Groups, Audio, Diagnostics,
  Settings.
- `crates/homepod-cast/src/preferences.rs` — repository, migration, atomic writes.
- `crates/homepod-cast/src/platform/mod.rs` — Windows platform-service boundary.
- `crates/homepod-cast/src/platform/hotkey.rs` — dedicated hotkey thread.
- `crates/homepod-cast/assets/fonts/InterVariable.ttf` and OFL text.
- `crates/homepod-cast/assets/fonts/IBMPlexMono-Regular.ttf` and OFL text.
- `crates/homepod-cast/assets/openaircast.ico` — window/tray icon variants.

### Modify

- `crates/homepod-cast/src/main.rs` — preserve diagnostic branches and launch
  eframe only for normal application mode.
- `crates/homepod-cast/src/tray.rs` — reduce to the eframe-compatible tray adapter
  backed entirely by snapshots and events.
- `crates/homepod-cast/src/cast.rs` — remove hotkey persistence and expose only
  controller-facing discovery/session behavior; keep device-state migration
  behind the temporary adapter until subproject 2 replaces it.
- `crates/homepod-cast/src/group_state.rs` — migrate tests into the DeviceId-based
  reducer, then remove the index-based module.
- `crates/homepod-cast/Cargo.toml` — add eframe/egui `=0.36.1` with `wgpu` and
  `accesskit`, serialization for settings, and matching dev-test support; retain
  `tray-icon`, Tokio, and narrowly featured `windows-sys`.
- `build.ps1` — continue producing `dist\OpenAirCast.exe` and verify no runtime
  asset directory is required.
- `README.md` — replace tray-only instructions with main-window/tray behavior.
- `THIRD_PARTY_NOTICES.md` — record egui/eframe and font licenses.

### Remove after parity

- `crates/homepod-cast/src/settings_window.rs` and its `mod` declaration.

Removal happens only after volume, theme, hotkey editing, window-close behavior,
tray actions, and persistence migration pass their acceptance tests.

## Tests

### Reducer unit tests

1. Selecting a receiver changes staged membership without changing desired or
   actual membership.
2. Start creates a new generation and enters Starting without claiming Streaming.
3. A matching Started event installs active membership and Streaming state.
4. Started or Failed from an older generation has no semantic effect.
5. Start failure clears active membership, returns to Stopped, and retains desired
   membership.
6. Stop clears active membership and preserves the last non-empty desired group.
7. The global hotkey stops active streaming and resumes the desired group when
   stopped.
8. Hotkey activation with no desired group selects the first available receiver;
   with no receiver it emits a notice and no Start effect.
9. A disappeared receiver remains visibly unavailable in desired membership but
   cannot enter the next controller Start target.
10. Editing membership while Streaming emits no controller effect; one Apply
    commits the complete set and creates exactly one newer restart generation,
    while Discard creates none.
11. Volume is clamped, emits immediate ApplyVolume, and schedules debounced
    persistence without synchronous I/O.
12. Main-window close emits HideMainWindow, never StopSession or BeginShutdown.
13. Quit emits bounded shutdown effects once and rejects subsequent Start.
14. Page and theme changes publish exactly one new snapshot revision.

### Backend contract tests

- `dispatch` is non-blocking when fake discovery/start operations are held.
- Snapshot publication is atomic: readers observe either the old or complete new
  revision, never partially updated collections.
- One semantic transition invokes the installed waker once.
- Dropping `SnapshotSubscription` prevents later wake callbacks.
- `drain_ui_effects` returns each window effect exactly once.
- Fake controller results with reversed generation order leave the newest state.
- Controller-channel loss publishes Needs attention and leaves Quit usable.

### Preferences tests

- A valid legacy hotkey file migrates to shell schema version 1; legacy volume is
  not serialized into shell settings.
- Invalid legacy values fall back safely and produce one notice.
- Corrupt JSON does not prevent startup.
- Atomic replacement leaves either the previous valid file or the new valid file.
- Debounced changes flush final geometry, hotkey, startup, and theme on quit.
- Actual stream state, IP addresses, and secrets are absent from serialized JSON.

### UI and tray tests

- A headless egui harness renders every navigation page at minimum and initial
  sizes in light and dark themes without overflow.
- Home receiver rows keep stable IDs when display order changes.
- Every icon-only action has an accessible name.
- Start/Stop text derives from actual/in-progress snapshot state.
- Signal-route desired, active, unavailable, and error variants each expose an
  equivalent textual summary.
- Tray menu labels, desired checks, enabled state, tooltip, and receiver submenu
  derive from one supplied snapshot.
- Tray events dispatch the expected `AppEvent` without mutating a local mirror.

### Existing regression tests

The following remain green:

```powershell
cargo test -p homepod-cast
cargo test --workspace --all-targets
cargo build -p homepod-cast --release
```

The diagnostic behavior of `openaircast.exe --list`, `--selftest`, and
`--selftest-group` remains unchanged apart from module routing.

## Acceptance Criteria

1. Normal launch shows one 1120x720 main window on-screen and exactly one tray
   icon; release launch shows no console window.
2. Home exposes actual state, signal route, staged receiver selection with one
   Apply/Discard bar, Start/Stop, and master volume without visiting another
   page.
3. Clicking Close hides the window without stopping audio; tray left-click restores
   and focuses the same window and page.
4. Tray and main window show identical desired and actual state after success,
   connection failure, stop, and rapid membership changes.
5. A delayed result from an older generation cannot overwrite newer selection or
   session state.
6. Global hotkey behavior matches the specified stopped/starting/streaming cases;
   changing it does not require restarting the app.
7. Discovery and connection may take several seconds without freezing window
   movement, navigation, Close, or Stop.
8. Keyboard-only use reaches every daily control in visual order with visible
   focus and expected Enter, Space, arrow, and Escape behavior.
9. Accessibility Insights reports no critical errors for Home and Settings;
   Narrator and NVDA announce receiver checkboxes, volume value, actual stream
   changes, and one failure notice correctly.
10. Light, dark, and Windows high-contrast presentations retain readable text,
    visible focus, and non-color state distinctions.
11. Moving the running window among 100%, 125%, 150%, 175%, and 200% monitors
    causes no clipping, blurred text, duplicated state, or geometry jump.
12. With Windows animations disabled, all transitions are immediate. When enabled,
    no animation exceeds 120 milliseconds or loops indefinitely.
13. After 60 seconds visible and idle, there is no continuous frame stream and
    average process CPU remains below 1% on the validation machine. Hidden-to-tray
    UI activity remains event-driven.
14. `build.ps1` produces one self-contained `dist\OpenAirCast.exe`; fonts, icons,
    and GUI resources require no adjacent runtime files, Qt, or WebView2. Windows
    system DLLs are the only dynamic runtime dependencies.
15. Quit stops the active session when possible and terminates within the bounded
    shutdown interval even when the documented upstream task leak requires the
    final forced-exit fallback.
16. The existing real-audio, one-capture, single-/multi-receiver paths compile and
    the recorded three-HomePod validation procedure remains executable.

## Dependencies for Later Subprojects

Subproject 2 may rely on these stable outputs:

- `AppHandle::dispatch`, `snapshot`, `drain_ui_effects`, and `install_waker`;
- immutable, revisioned `UiSnapshot` publication;
- stable `DeviceId` receiver identity;
- separate desired and active membership;
- generation-tagged controller operations and completion events;
- pure reducer plus explicit effect execution;
- `PreferencesRepository` as the only shell-settings persistence owner and a
  separate device-state boundary for playback preferences;
- view and tray code that never owns protocol sessions.

Subproject 2 is responsible for richer continuous discovery, lifecycle,
reconnect, partial-failure, stats, and cancellation behavior behind those
interfaces. New snapshot fields must remain presentation-safe and bounded. It may
extend enums using new variants but must preserve the existing semantics: only a
confirmed controller result changes actual receiver membership to Streaming.

## Risks and Mitigations

- **egui API movement:** Pin `=0.36.1` for the subproject and isolate widget styling
  and tray/winit hooks in `ui` and `tray` modules.
- **Immediate-mode overpainting:** Use immutable snapshots, stable IDs, event-driven
  repaint, and bounded models; never clone protocol objects into each frame.
- **Windows Calm becoming a generic dashboard:** Enforce the approved palette,
  typography, daily-control hierarchy, and single signal-route signature. Avoid
  stat-card grids, gradients, excessive pills, and ambient motion.
- **Tray/winit thread affinity:** Construct and retain the tray icon inside eframe
  initialization on the event-loop thread; cover hide/restore and Explorer restart
  in Windows acceptance testing.
- **Hidden-window wake behavior:** The snapshot waker must wake the eframe event
  loop even while the viewport is hidden; verify a tray click can restore it after
  extended idle time.
- **Accessibility gaps in custom painting:** Pair every custom route or icon with
  standard interactive widgets and explicit AccessKit labels; validate with real
  screen readers.
- **Renderer/driver variability:** Validate wgpu on Windows 10, Windows 11, Remote
  Desktop, and the project's validation machine. A renderer fallback is a release
  decision only if it still preserves one executable and the same UI contract.
- **State races:** Route every result through generation-aware reduction; views and
  tray never patch actual state locally.
- **Persistence corruption:** Version, validate, and atomically replace settings;
  keep legacy files during migration and make failure non-fatal.
- **Shutdown leak:** Keep shutdown bounded and retain forced exit only as the final
  documented fallback after session stop, hotkey teardown, and preference flush.
- **Binary growth:** Record release size before and after eframe. Do not trade away
  accessibility, embedded fonts, or one-file deployment solely for size.

## License Constraints

OpenAirCast remains GPL-2.0 as currently declared by the workspace and upstream
notices. Select the MIT option for dual-licensed egui, eframe, wgpu, tray-icon,
and compatible transitive components; do not describe the combined application
as Apache-2.0. Bundle the complete SIL OFL 1.1 texts for Inter and IBM Plex Mono
and preserve their font names and copyright notices. Update
`THIRD_PARTY_NOTICES.md` before release.

No dependency may require a proprietary runtime, account, hosted service, or
per-seat license to build or run the GUI. The release artifact remains
redistributable with source under the project's existing GPL obligations and runs
offline apart from OpenAirCast's local-network AirPlay behavior.
