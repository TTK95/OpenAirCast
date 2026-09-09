# OpenAirCast Windows Native Redesign — Design Specification

**Date:** 2026-08-24

**Status:** Design approved; implementation plan pending

**Scope:** Complete visual and structural redesign of the native Windows GUI while preserving backend contracts and core behaviors

## 1. Purpose and precedence

This specification replaces the visual language, information hierarchy, layout, and component guidance of the earlier Control Center shell design. The state machine, stable receiver identity, bounded command handling, generation safety, persistence guarantees, and frozen `AppHandle` boundary remain authoritative.

When this document conflicts with the earlier shell design on appearance or page composition, this document wins. When it conflicts with the device-resilience or diagnostics plans on domain behavior, those backend plans win.

The redesign has one product job:

> A Windows user can select HomePods, understand the current audio route, set the master volume, and start or stop streaming without leaving the Overview page.

The primary audience is deliberately dual-layered:

- Standard mode serves people who want a clear household control surface.
- Advanced mode adds truthful connection, synchronization, and diagnostic facts for enthusiasts and support work.
- Both modes use the same pages, actions, terminology, and control positions.

## 2. Approved design decisions

The following decisions are fixed:

| Axis | Decision |
|---|---|
| Visual direction | Windows Native |
| Home hierarchy | Command Home |
| Change depth | Visual structure, navigation hierarchy, components, and copy may change; backend contracts remain stable |
| Theme | Follow the Windows setting by default; Light, Dark, and High Contrast are equal products |
| Information depth | Standard mode plus a persistent Advanced-information preference |
| Languages | German and English; Windows display language on first run, manual override in Settings |
| Signature element | Route Ribbon showing the real path from Windows audio to selected receivers |
| Window frame | Native Windows title bar and controls; no custom-drawn window chrome |
| Minimum window | 900 × 600 logical points |
| Default window | 1120 × 720 logical points |

## 3. Diagnosis of the existing GUI

The current implementation is not a viable visual baseline. The redesign must correct these causes rather than adjust isolated colors:

1. The navigation rail hard-codes the light palette while the content can use the dark theme, producing two unrelated themes in one window.
2. Several Home components hard-code `LIGHT_PALETTE`, so dark text, surfaces, and state colors become illegible in Dark mode.
3. The central panel does not establish a dependable active-theme canvas.
4. The route/status block consumes excessive height without communicating a useful hierarchy.
5. Receiver selection, status, volume, and the primary action do not read as one workflow.
6. Spacing follows widget defaults more often than an intentional page rhythm.
7. Navigation is visually wide and dominant while the daily content is sparse.
8. The current root dispatcher renders Home but does not consistently route the completed destination renderers.
9. English-only copy and implementation-oriented labels do not match the selected German/English product direction.
10. Placeholder destinations and future-capability descriptions do not share one purposeful empty-state system.

## 4. Design principles

### 4.1 Familiar interaction, specific identity

Controls behave like a modern Windows application: native window frame, conventional left navigation, clear page titles, rectangular command buttons, predictable keyboard behavior, and restrained surfaces. OpenAirCast becomes recognizable through the Route Ribbon, receiver geometry, live-state colors, and audio-specific copy rather than ornamental branding.

### 4.2 One dominant action per context

Each page exposes at most one filled primary button. On Overview it is Start, Stop, Cancel, or Retry according to the authoritative stream state. Secondary actions use quiet buttons or links. Safety and lifecycle actions take priority over committing staged changes: while run intent is active or a start/stop transition is in flight, Stop or Cancel remains filled and available and the sticky bar's Apply action is quiet. Apply becomes the page's only filled action only in a stopped/safe state; Start remains quiet or unavailable until the staged changes are applied or discarded.

### 4.3 Structure does not jump with state

Ready, Connecting, Streaming, and Attention use the same page geometry. Text, Route Ribbon, state iconography, and the contextual action change together. Stopping uses the Connecting geometry with stopping copy and disabled conflicting actions.

### 4.4 Truth before decoration

Every status, metric, route segment, receiver badge, and progress label comes from a real snapshot field. Missing, stale, unsupported, or not-yet-measured data is labeled explicitly. Zero is never used as a substitute for unknown.

### 4.5 Progressive disclosure, not a second application

Advanced mode adds facts below or within the same standard components. It does not reorder navigation, rename actions, add a separate dashboard, or change the meaning of a standard control.

### 4.6 Accessibility is a visual constraint

Focus, selection, availability, activity, warning, and failure remain distinguishable without color. High Contrast uses Windows system colors rather than approximating the Light or Dark palette.

## 5. Visual foundation

### 5.1 Semantic color roles

Components consume semantic tokens from the active `ThemeTokens`. They never import a Light- or Dark-specific constant directly.

#### Light theme

| Token | Value | Use |
|---|---:|---|
| `canvas` | `#F4F6FB` | Main application background |
| `surface` | `#FFFFFF` | Cards, command areas, selected navigation surface |
| `surface_subtle` | `#E9EDF5` | Navigation rail, audio dock, quiet grouped controls |
| `ink` | `#172033` | Primary text and high-emphasis icons |
| `ink_muted` | `#657084` | Secondary text and inactive icons |
| `border` | `#DCE2ED` | Quiet one-pixel separators and card borders |
| `route` | `#3168E8` | Primary action, focus, selection, source node |
| `on_route` | `#FFFFFF` | Text and icons on filled Route controls |
| `route_soft` | `#E8EFFF` | Hover and selected quiet backgrounds |
| `live` | `#157766` | Streaming, healthy connection, active route |
| `live_soft` | `#DFF4EF` | Healthy quiet backgrounds |
| `warning` | `#A9570D` | Recoverable attention state |
| `warning_soft` | `#FFF3E4` | Warning background |
| `fault` | `#B94B5B` | Failed route, destructive/failure emphasis |
| `fault_soft` | `#FFF0F2` | Failure background |

#### Dark theme

| Token | Value | Use |
|---|---:|---|
| `canvas` | `#10151F` | Main application background |
| `surface` | `#18202D` | Cards and command areas |
| `surface_subtle` | `#222C3A` | Navigation and quiet grouped controls |
| `ink` | `#F4F7FB` | Primary text |
| `ink_muted` | `#9BA7BA` | Secondary text |
| `border` | `#2F3A49` | Quiet separators and card borders |
| `route` | `#7EA6FF` | Primary action, focus, selection, source node |
| `on_route` | `#10151F` | Text and icons on filled Route controls |
| `route_soft` | `#243657` | Hover and selected quiet backgrounds |
| `live` | `#60D6C2` | Streaming, healthy connection, active route |
| `live_soft` | `#183D3A` | Healthy quiet backgrounds |
| `warning` | `#FFB45C` | Recoverable attention state |
| `warning_soft` | `#3E2E1D` | Warning background |
| `fault` | `#FF8B9A` | Failed route and failure emphasis |
| `fault_soft` | `#43252D` | Failure background |

#### High Contrast

High Contrast resolves foreground, background, selection, disabled, link, and focus colors from Windows. It uses:

- system background for `canvas` and `surface`;
- system foreground for all text and neutral strokes;
- system highlight and highlight-text for selection and primary actions;
- a three-pixel system-colored focus outline with a one-pixel offset;
- borders, text, and symbols instead of quiet tonal surface differences.

Hard-coded Route, Live, Warning, or Fault hues do not override a system High Contrast color.

### 5.2 Typography

The application requests `Segoe UI Variable` from Windows. Embedded Inter Variable remains the metrically stable fallback and supports test environments. IBM Plex Mono remains embedded for measurements, identifiers, percentages, timestamps, and technical values.

| Role | Family | Size / line height | Weight |
|---|---|---:|---:|
| Page title | Segoe UI Variable / Inter | 28 / 34 | 700 |
| Hero title | Segoe UI Variable / Inter | 20 / 26 | 700 |
| Section title | Segoe UI Variable / Inter | 16 / 22 | 650 |
| Body | Segoe UI Variable / Inter | 14 / 20 | 400 |
| Button | Segoe UI Variable / Inter | 14 / 18 | 600 |
| Secondary | Segoe UI Variable / Inter | 12 / 17 | 400 |
| Eyebrow/status label | Segoe UI Variable / Inter | 11 / 15 | 700, uppercase only where the phrase is a true state/category |
| Measurement | IBM Plex Mono | 12 / 16 | 500–700 |

Copy uses sentence case. All-caps is limited to short technical state/category labels; page titles and commands are never all-caps.

### 5.3 Geometry and spacing

The base unit is four logical points. Page-level structure uses an eight-point rhythm.

| Element | Value |
|---|---:|
| Standard page inset | 24 |
| Compact page inset | 16 |
| Section gap | 16 |
| Card internal padding | 16 |
| Dense metric padding | 12 |
| Control minimum height | 40 |
| Receiver row/card minimum height | 56 |
| Navigation destination minimum height | 44 |
| Card radius | 12 |
| Button/input radius | 8 |
| Small badge radius | 6 |
| Quiet border | 1 |
| Keyboard focus | 2 plus 2-point offset; 3 in High Contrast |

Diffuse drop shadows, gradients, frosted-glass layers, and decorative background shapes are excluded. Only selected floating surfaces such as a menu may use a small Windows-like elevation shadow.

### 5.4 Icons

Icons are deterministic vector shapes rendered by egui or embedded open-source SVG paths converted reproducibly at build time. The family uses 20-point canvases, rounded two-point strokes, and no filled multicolor illustrations. Every icon has a textual accessible name.

## 6. App shell and responsive layout

### 6.1 Window structure

OpenAirCast keeps the native Windows frame. The client area contains:

```text
┌──────────────── native Windows title bar ────────────────┐
│ ┌──────── navigation ───────┬──────── page ────────────┐ │
│ │ destination list          │ page header + command    │ │
│ │                           │                          │ │
│ │                           │ page content             │ │
│ │                           │                          │ │
│ │ status footer             │ sticky changes/notice   │ │
│ └───────────────────────────┴──────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

### 6.2 Navigation

The six stable destinations remain:

1. Overview / Übersicht (`Page::Home` remains the internal enum variant and persisted value; only its visible label is localized)
2. Speakers / Lautsprecher
3. Groups / Gruppen
4. Audio
5. Diagnostics / Diagnose
6. Settings / Einstellungen

At 1000 logical points and above, the rail is 176 points wide and shows icon plus label. Below 1000, it is 64 points wide and shows centered icons with accessible labels and hover tooltips. Settings remains with the main destination set but may be visually separated near the bottom. The true application state is pinned below the destinations.

Navigation selection uses a quiet surface and a three-point Route-colored leading indicator. It never uses a fully saturated rectangular fill behind both icon and text.

### 6.3 Page content

The content column fills the active-theme canvas. Standard pages have a 920-point maximum readable width but remain left-aligned; the Overview receiver grid may use the full available width. Vertical scrolling is page-owned. The navigation rail never scrolls with page content.

### 6.4 Responsive behavior

At 1120 × 720:

- expanded navigation;
- two receiver columns;
- Route Ribbon and hero copy share one card row;
- page inset is 24 points.

At 900 × 600:

- compact navigation;
- one receiver column when two columns would violate minimum card width;
- shorter copy labels selected from localization resources, not clipped text;
- page inset is 16 points;
- the primary command remains visible in the page header;
- page content scrolls without moving the command bar or navigation.

The window supports 100–200% scale and movement between monitors without persisting impossible geometry.

## 7. Information architecture

### 7.1 Overview / Übersicht — Command Home

Overview is the daily control surface. Its fixed order is:

1. Page header with localized greeting/context and the one contextual primary action.
2. Status hero with phase, concise explanation, and Route Ribbon.
3. Receiver section with count, selection cards, availability, and active state.
4. Audio Dock with master volume and exact percentage.
5. One inline notice when user action is required.
6. Advanced metrics strip only when Advanced mode is enabled and real data exists.
7. Sticky staged-change bar when membership differs from the applied selection; it is pinned to the bottom of the page viewport and remains last in visual, accessibility, and tab order.

The user can select receivers, apply or discard receiver changes, adjust volume, and start or stop streaming without navigating away.

### 7.2 Speakers / Lautsprecher

Standard mode displays:

- room/device name;
- device class/model in user-facing form;
- Available, Unavailable, Unknown, or Retrying;
- Selected and Streaming states;
- last user-actionable problem and Retry/Refresh when supported.

Advanced mode additionally displays support-safe values already present in snapshots:

- health and retry state;
- current session membership;
- configured per-receiver level, with an editable slider once the existing typed backend command and value are projected through the shell;
- sync reference kind and freshness;
- calibration value and whether it is applied;
- diagnostic age/staleness.

Network addresses, socket identifiers, secrets, pairing material, raw service records, and internal connection objects never enter the view.

### 7.3 Groups / Gruppen

The page explains the currently selected receiver combination and active session group. Saved-group Create/Edit/Delete/Activate controls are in scope because typed backend commands and durable snapshots exist. They appear only after those existing contracts are projected through `UiSnapshot`, `AppEvent`, and the effect executor; until that bridge is integrated, the page uses the approved Empty State and directs the user to select receivers on Overview. Disabled fake CRUD buttons are prohibited.

### 7.4 Audio

Audio displays only capabilities supplied by the backend snapshot. The first redesign release includes the shell projection for the already-typed mute, endpoint-selection, and latency-preset capabilities:

- active Windows capture endpoint;
- selectable endpoint list;
- master volume;
- mute state;
- capture health and source replacement status;
- latency preset.

Unavailable planned controls are not shown. A read-only default endpoint is labeled as such.

### 7.5 Diagnostics / Diagnose

Standard mode opens on a compact health summary with plain-language status and recommended action. Advanced mode exposes the planned subpages:

- Overview / Übersicht
- Speakers / Lautsprecher
- Calibration / Kalibrierung
- Events / Ereignisse
- Advanced / Erweitert

The Diagnostics area is the only place for dense metrics and event history. Copy summary and support-safe export are explicit commands. The UI polls diagnostics at no more than 4 Hz while this area is visible and the window is neither hidden nor minimized.

### 7.6 Settings / Einstellungen

Settings is grouped in this order:

1. Appearance: System, Light, Dark; High Contrast remains controlled by Windows.
2. Language: System, Deutsch, English.
3. Behavior: launch/close/tray behavior only when supported.
4. Connection: auto-connect using the existing typed backend capability.
5. Keyboard: global streaming shortcut and validation.
6. Advanced information: persistent on/off switch with a concise explanation.
7. About: version, license, notices, and copy/open actions that really work.

### 7.7 Capability scope for this redesign

The frozen `AppHandle` method surface does not mean the old shell snapshot is the final feature boundary. The redesign may extend `UiSnapshot`, `AppEvent`, and internal effects while preserving the four `AppHandle` methods and keeping all device objects out of the UI.

| Capability | Existing authoritative basis | Redesign scope |
|---|---|---|
| Navigation, staged membership, Apply/Discard, Start/Stop, master volume, theme, hotkey | Existing shell snapshot/events | Required |
| Locale preference and Advanced-information preference | New shell-only presentation preferences | Required, including schema migration described in section 14 |
| Saved-group Create/Edit/Delete/Activate | Existing `DeviceSnapshot` plus typed `BackendCommand` variants | Required shell projection and UI |
| Mute, endpoint selection, latency preset, auto-connect | Existing `DeviceSnapshot` plus typed `BackendCommand` variants | Required shell projection and UI |
| Per-receiver level and receiver Retry | Existing typed backend commands; presentation-safe values must be projected | Required on Speakers after the safe projection exists |
| Diagnostics details, copy summary, and support-safe export | Approved read-only diagnostics snapshot/event boundary | Required shell integration and a bounded off-UI-thread export derived only from allowlisted presentation-safe fields |
| New transport behavior, calibration algorithms, or unsupported automatic measurement | No approved presentation-safe command/snapshot contract | Out of scope |

An implementation checkpoint may temporarily show an Empty State while a listed projection is unfinished, but the final redesign acceptance build must not mark an in-scope row complete until its end-to-end command, snapshot, error, accessibility, and localization tests pass.

## 8. Standard and Advanced modes

The mode is a presentation preference, not an authorization or backend mode.

### Standard mode

Standard mode uses household language:

- receiver name and availability;
- Ready, Connecting, Streaming, Reconnecting, Stopping, or Needs attention;
- clear corrective actions;
- no packet, queue, clock, generation, or transport terminology.

### Advanced mode

Advanced mode adds measured facts in place without moving standard controls:

- receiver health and retry state;
- timing reference and freshness;
- group latency and calibration;
- scheduler jitter;
- capture and sender queue depth;
- transport/retransmit counters;
- structured recent events.

Every measurement includes its unit and, when relevant, age or stale state. Advanced mode does not expose unsafe data.

## 9. Localization and copy

### 9.1 Locale selection

On first run, OpenAirCast maps the Windows display language to German or English; unsupported languages fall back to English. The user can select System, Deutsch, or English in Settings. The choice is persisted atomically with the other shell preferences.

### 9.2 String ownership

All visible strings use stable localization keys. Components receive resolved strings or a locale/catalog handle; they do not embed language-specific sentences. Dynamic sentences use named arguments and locale-aware plural templates.

No page may display a mixture of German and English except technical protocol/product names such as AirPlay, HomePod, PTP, WASAPI, or OpenAirCast.

### 9.3 Core vocabulary

| Concept | German | English |
|---|---|---|
| Overview | Übersicht | Overview |
| Speakers | Lautsprecher | Speakers |
| Ready | Bereit | Ready |
| Connecting | Verbindung wird aufgebaut | Connecting |
| Streaming | Streaming | Streaming |
| Reconnecting | Verbindung wird wiederhergestellt | Reconnecting |
| Stopping | Streaming wird beendet | Stopping |
| Needs attention | Aufmerksamkeit erforderlich | Needs attention |
| Start streaming | Streaming starten | Start streaming |
| Stop streaming | Streaming stoppen | Stop streaming |
| Cancel | Abbrechen | Cancel |
| Stop trying | Vorgang beenden | Stop trying |
| Retry | Erneut versuchen | Retry |
| Apply changes | Änderungen anwenden | Apply changes |
| Discard | Verwerfen | Discard |
| Master volume | Gesamtlautstärke | Master volume |
| Advanced information | Erweiterte Informationen | Advanced information |
| Unknown | Unbekannt | Unknown |
| Stale | Veraltet | Stale |

Errors avoid apology and vague failure language. They state:

1. what happened;
2. what remains active or preserved;
3. what the user can do next.

### 9.4 Backend-error localization

Existing backend `UserFacingError` strings and `CommandFailed` reasons are pre-redacted but are not localization resources. They are never rendered directly. The presentation mapper resolves a stable localization key from typed context already available at the boundary:

- the originating command kind retained by command ID;
- `NoticeCode` and error scope when supplied;
- session, discovery, capture, receiver, or persistence phase;
- the typed corrective action.

Known contexts use a specific localized message. An unmapped error uses a generic localized message for its scope, preserves the authoritative state, and offers only a valid typed action. Named arguments are restricted to presentation-safe receiver/group names, counts, percentages, and durations. The original backend string is logging/diagnostic input only and does not cross into visible copy, accessibility labels, clipboard text, or support export. A future backend error-code extension may improve specificity but is not required to keep the first redesign release safe and fully localized.

## 10. Component contracts

### 10.1 App Shell

Owns active theme tokens, locale, responsive rail mode, page scrolling boundary, and status footer. It does not derive transport state itself.

### 10.2 Route Ribbon

Inputs are presentation-safe source status, route phase, ordered receiver nodes, per-node availability/activity, and optional failure location. It renders:

- a rounded square source node for Windows audio;
- a neutral, pending, live, or failed route segment;
- circular receiver nodes in stable display order;
- a complete textual summary in the accessibility tree.

The Ribbon never implies active streaming when the authoritative session is stopped. More than four receivers collapse visually into three nodes plus a `+N` node while the accessible summary names/counts all selected receivers.

### 10.3 Receiver Card

Minimum height is 56 points. The entire card is one selection target on Overview. It contains:

- accessible checkbox semantics;
- receiver name;
- concise device class/model;
- availability text and icon;
- selected and streaming states;
- optional advanced details below the standard row.

Selection, focus, hover, active streaming, and unavailable are separate visual states.

### 10.4 Command Action

The primary action is filled Route blue in Light, the Dark Route token in Dark, and system Highlight in High Contrast. Its foreground uses `on_route` in Light and Dark and system Highlight Text in High Contrast. It has a minimum 40-point height, a visible disabled state, and a verb that matches the resulting announcement.

### 10.5 Audio Dock

Displays `Master volume` / `Gesamtlautstärke`, an accessible slider, and a monospace percentage. The label deliberately avoids implying control of the Windows system volume. Arrow keys make deterministic increments. The percentage reflects the authoritative snapshot after reducer round-trip rather than optimistic local state.

### 10.6 Inline Notice

Contains severity icon/text, summary, optional consequence, and no more than one primary corrective action. A secondary dismiss action appears only for dismissible informational notices.

### 10.7 Empty State

Contains a purpose-specific title, explanation, and one real next action when available. It never describes vague future features as if controls existed.

### 10.8 Metric Tile

Contains a plain label, monospace value, unit, health/freshness label, and accessible combined phrase. Unknown and stale use text plus symbol. A metric tile is not interactive unless it navigates to a clearly named detail.

### 10.9 Sticky change bar

When staged receiver membership differs from applied membership, a bar remains pinned to the bottom of the page viewport across all destinations. It is painted after the scrollable page content and is last in visual, accessibility, and tab order. The scroll region reserves bottom space so the bar never obscures the page header, page content, or Audio Dock. It contains the localized changed-state summary, Apply, and Discard. Apply is filled only while the authoritative run intent is stopped and no lifecycle transition is in flight; otherwise Apply is quiet so Stop or Cancel remains the single filled safety action.

## 11. State presentation

The presentation mapper exhaustively converts the authoritative session phase and run intent. No renderer independently chooses the primary action.

| Session phase | Standard label | Route treatment | Filled action | Typed dispatch | Secondary corrective action |
|---|---|---|---|---|---|
| `Stopped` with stopped intent | Ready / Bereit | Neutral | Start streaming | `StartRequested` | Refresh only when discovery can run |
| `Starting` with running intent | Connecting / Verbindung wird aufgebaut | Pending source-to-receivers | Cancel | `StopRequested` | None |
| `Streaming` with running intent | Streaming | Live to every active receiver | Stop streaming | `StopRequested` | None |
| `Degraded` with running intent | Needs attention / Aufmerksamkeit erforderlich | Live to active receivers; fault/pending to missing receivers | Stop streaming | `StopRequested` | Retry on an affected receiver when valid |
| `Restarting` with running intent | Reconnecting / Verbindung wird wiederhergestellt | Existing live route retained where truthful; replacement segments pending | Stop streaming | `StopRequested` | None; the reason is Advanced information |
| `Stopping` with stopped intent | Stopping / Streaming wird beendet | Stopping treatment | Disabled Stopping label | None | None |
| `Failed` with running intent | Needs attention / Aufmerksamkeit erforderlich | Fault at the known segment; no fabricated live route | Stop trying / Vorgang beenden | `StopRequested` | Retry receiver or Refresh when valid |

Impossible or temporarily inconsistent phase/intent pairs map to a generic localized Needs-attention state, retain any truthfully active receiver nodes, and expose no command that is invalid for the snapshot. The legacy failed projection may use Retry as the filled action only when its authoritative run intent is already stopped; that action dispatches `StartRequested` and never hides a still-running session.

### Ready

- Hero label: Ready / Bereit.
- Route is neutral with selected receiver outlines.
- Primary action: Start streaming.
- Receiver selection and volume remain enabled.

### Connecting

- Hero names the current stage or receiver when available.
- Route progresses from source toward receiver nodes.
- Primary contextual action: Cancel.
- Progress has a textual equivalent and never depends on indefinite animation.

### Streaming

- Hero label: Live / Streaming.
- Active Route Ribbon segments and active receiver nodes use `live`.
- Primary action: Stop streaming.
- Receiver cards distinguish selected from currently active.

### Degraded

- The hero states that some receivers are still streaming.
- Active receiver nodes remain `live`; failed or retrying nodes use text, symbol, and Fault/Warning treatment.
- Stop remains the filled action. Per-receiver Retry is secondary and never replaces the ability to stop.

### Restarting

- The stable hero geometry uses Reconnecting copy and a non-looping pending treatment.
- Any receiver that remains authoritatively active stays visibly live.
- Stop remains the filled action; the restart reason appears only in Advanced information.

### Stopping

- Uses the Connecting geometry with Stopping copy.
- Conflicting actions are disabled.
- Stop is not dispatched twice.

### Attention / failure

- Hero names the affected receiver or subsystem in user language.
- The Ribbon identifies the failed segment/node when known.
- Remaining active receivers stay visibly live.
- When audio or run intent remains active, Stop/Stop trying remains the filled action and Retry, Refresh, or Open Settings is secondary. Retry becomes filled only when the authoritative run intent is stopped and no stream remains active.
- Staged/desired selection remains visible and preserved.

## 12. Interaction and motion

- Tab order follows visual reading order: navigation, page header command, page content, sticky action bar.
- Enter and Space activate focused buttons and selectable receiver cards.
- Arrow keys adjust sliders and navigate appropriate radio groups.
- Escape discards only the active transient editor; it never discards staged membership implicitly.
- A two-point focus ring plus two-point offset distinguishes focus from selection.
- Hover, focus, active, selected, disabled, and unavailable have separate component states.
- Normal transitions complete within 120 ms.
- The Route Ribbon may perform one 120 ms successful connection sweep.
- No component pulses or animates continuously.
- With Windows Reduced Motion, all transitions are immediate.
- Hidden or minimized windows run no decorative animation or repaint timer.

## 13. Accessibility requirements

- All actionable controls expose correct AccessKit roles, names, states, and disabled values.
- Receiver cards expose checkbox semantics and include availability in their accessible description.
- Current page is marked selected/current.
- Streaming phase, failure, Retry result, applied membership, and volume changes generate polite live announcements once per semantic change.
- Color is never the sole state carrier.
- Body text meets WCAG AA contrast; non-text focus/state indicators meet 3:1 against adjacent colors.
- High Contrast uses Windows system colors and remains usable without quiet surface differences.
- German and English labels fit at 100–200% DPI in both target window sizes.
- Screenreader order matches visual order.
- The UI is manually accepted with Accessibility Insights, Narrator, and NVDA.

## 14. Presentation architecture and data flow

The frozen shell boundary remains:

```rust
AppHandle::{dispatch, snapshot, drain_ui_effects, install_waker}
```

The approved diagnostics design may add its read-only diagnostics accessor. Views never receive a backend handle.

```text
device backend + diagnostics registry
                │
                ▼
       immutable domain snapshots
                │
                ▼
 locale + preferences + presentation mapping
                │
                ▼
       immutable page/component models
                │
                ▼
          pure egui renderers
                │
                ▼
            typed AppEvent
                │
                ▼
          reducer / backend effects
```

### 14.1 Shell-preference schema migration

The redesign upgrades the current shell `Preferences` schema from version 1 to version 2. Version 2 adds exactly two presentation fields:

| Field | Type | Version-1 migration default |
|---|---|---|
| `locale` | `System`, `German`, or `English` | `System` |
| `advanced_information` | boolean | `false` |

`System` resolves from the current Windows display language whenever preferences load and whenever Windows reports a relevant settings change; unsupported languages resolve to English. Explicit German or English choices ignore later OS-language changes.

The shell adds typed `LocaleChanged`, `AdvancedInformationChanged`, `WindowsDisplayLanguageChanged`, and `RetryPreferencesPersistence` events. It stores both preferences plus the resolved locale in `AppState` and publishes the preference plus resolved locale in `UiSnapshot`. `WindowsDisplayLanguageChanged` updates the resolved locale and snapshot only when the preference is `System`; it neither rewrites the preference nor persists an OS-derived value. `RetryPreferencesPersistence` increments the existing whole-preferences generation and asks the atomic worker to persist the current complete version-2 value; stale `Persisted` or `PersistFailed` completions are ignored by generation. Its corrective-action variant is scoped to preferences and can never dispatch a stream Start/Retry event. Both new preference fields use the existing whole-preferences persistence generation and atomic worker; they do not enter device-backend persistence. A version-1 document is decoded by an explicit migration path, receives the defaults above, preserves all existing fields including persisted `Page::Home`, and is written as version 2 on the next successful save.

Preference changes apply optimistically in the current process. If the save fails, the selected theme, locale, Advanced mode, hotkey, or geometry remains active in memory; the prior file remains durable, and a localized notice states that the change may be lost after restart and offers a real Retry action. This shell behavior is intentionally separate from device-state commands that use the backend's persist-before-publish contract.

Rules:

- The normal UI takes at most one `AppHandle::snapshot()` per frame.
- Presentation mapping is deterministic and has no I/O, sleep, clock read, socket access, or channel receive.
- Renderers consume immutable models and emit typed events through a supplied sink.
- Widget IDs derive from page variants, localization-independent component IDs, and stable receiver/session identity.
- Diagnostics polling occurs at no more than 4 Hz only while a diagnostics view is visible and the window is visible and restored.
- Theme, locale, and Advanced mode are persisted shell preferences.
- The active theme is resolved before any panel or component paints.

## 15. Error handling and data truthfulness

- Backend error codes map to localized presentation keys and typed corrective actions.
- Raw addresses, paths, secrets, payloads, packet contents, internal errors, and pairing data never become user-visible copy or support export fields.
- An empty discovery result differs from discovery failure and retrying discovery.
- A stale measurement includes a stale label and age when available.
- Partial group success names active and failed receivers without claiming full-group success.
- Export/copy operations run off the UI thread and publish completion/failure through normal events.
- If persistence of theme, language, Advanced mode, hotkey, or geometry fails, the active in-memory preference stays visible, the prior on-disk value remains durable, and the UI displays the settings-scoped notice defined in section 14.1.
- Unknown values are rendered as `Unknown` / `Unbekannt`, not `0`, an empty string, or a neutral healthy state.

## 16. Performance and repaint contract

- Discovery, endpoint enumeration, session setup, calibration apply, support export, and shutdown never block the eframe thread.
- Snapshot publication wakes one frame per new revision.
- User input wakes normal immediate frames.
- Diagnostics uses a coalesced 250 ms schedule only while visible.
- No page uses a continuous repaint loop for ambient animation.
- Receiver lists and event lists are bounded and virtualized/scrolled without cloning transport objects.
- Hidden-to-tray operation performs no visual polling.
- Release acceptance retains the visible and hidden idle CPU target below 1% over 60 seconds on the reference machine.

## 17. Source organization

The redesign targets these UI boundaries:

```text
crates/homepod-cast/src/ui/
├── mod.rs                 # eframe lifecycle, one dispatcher, waker/effects
├── theme.rs               # ThemeTokens, typography, fonts, contrast helpers
├── layout.rs              # rail mode, page insets, responsive helpers
├── i18n.rs                # Locale, keys, catalogs, plural/template formatting
├── components/
│   ├── mod.rs
│   ├── app_shell.rs
│   ├── route_ribbon.rs
│   ├── receiver_card.rs
│   ├── command_action.rs
│   ├── audio_dock.rs
│   ├── notice.rs
│   ├── empty_state.rs
│   └── metric_tile.rs
└── pages/
    ├── mod.rs             # exactly one complete Page dispatcher
    ├── overview.rs
    ├── speakers.rs
    ├── groups.rs
    ├── audio.rs
    ├── diagnostics.rs
    └── settings.rs
```

The current `home.rs`, `navigation.rs`, and monolithic `pages.rs` are migrated into these boundaries, then removed only after parity tests are green. `ui/mod.rs` must call the complete page dispatcher for every `Page` variant.

The design does not require a WebView, HTML runtime, Qt, hosted service, proprietary SDK, or per-seat dependency.

## 18. Verification matrix

### Automated

1. Token tests verify exact semantic values and active-theme resolution.
2. Contrast tests cover body text, secondary text on both canvas and surface, filled-action foregrounds, focus, selection, Live, Warning, and Fault roles in Light and Dark.
3. High Contrast tests verify system-color mapping and non-color state distinctions.
4. Localization tests verify every key exists in German and English, templates accept the same named arguments, and no rendered page mixes catalogs.
5. Presentation-mapping tests exhaustively cover Stopped, Starting, Streaming, Degraded, Restarting, Stopping, and Failed with their valid run intents, plus partial failure, empty discovery, retrying discovery, stale diagnostics, unknown values, and impossible phase/intent fallback.
6. Layout tests render all pages at 1120 × 720 and 900 × 600.
7. Theme/language coverage renders representative pages for Light/Dark × German/English × Standard/Advanced.
8. Receiver reorder tests verify stable widget identity and selection.
9. Keyboard tests verify tab order, Enter/Space activation, Escape scope, slider keys, and radio navigation.
10. AccessKit tests verify roles, selected/current/disabled states, labels, and live announcements.
11. Repaint tests verify no diagnostics or animation polling while hidden/minimized and a maximum 4 Hz diagnostics schedule while visible.
12. Reducer/actor regression tests verify each visible command remains typed, bounded, generation-safe, and non-blocking.
13. Preference tests migrate version 1 to version 2, verify System/German/English resolution, persist Advanced mode, and exercise failed-save/retry semantics without rolling back active memory state.
14. Capability bridge tests cover saved-group CRUD/activation, mute, endpoint selection, latency, auto-connect, receiver level, Retry, diagnostics snapshot/event integration, copy summary, and bounded support export from widget event through the effect executor to a new authoritative snapshot or completion event.
15. Error-presentation tests prove free-form backend error strings never appear in rendered text, accessibility output, clipboard content, or support export.

### Manual visual and product acceptance

- Light, Dark, and Windows High Contrast on Windows 10 and 11.
- German and English at 100%, 125%, 150%, 175%, and 200% scale.
- Default 1120 × 720 and minimum 900 × 600.
- Mixed-DPI monitor movement without clipping or geometry jump.
- Keyboard-only completion of receiver selection, Apply, volume, Start, Stop, Retry, language, theme, and Advanced mode.
- Accessibility Insights, Narrator, and NVDA.
- Reduced Motion on/off.
- Remote Desktop wgpu rendering.
- Visible and hidden idle CPU measurement.
- Tray hide/restore preserves page, focus target when appropriate, theme, language, and mode.
- Real receiver discovery, group start/stop, partial failure, retry, suspend/resume, and network interruption.

## 19. Migration constraints

- Preserve `AppHandle::{dispatch,snapshot,drain_ui_effects,install_waker}`.
- Preserve every `Page` identity, including internal/persisted `Page::Home`, and stable receiver IDs; visible localized labels may change.
- Preserve typed `AppEvent` dispatch and reducer ownership of state.
- Do not place device addresses, sockets, sessions, secrets, or mutable backend handles in UI snapshots or components.
- Do not invent controls for saved groups, endpoint selection, calibration, exports, or metrics before their typed backend contract exists.
- Do not keep direct `LIGHT_PALETTE` use in any component.
- Do not perform a single monolithic rewrite without component/state regression coverage.
- Do not remove old renderers until the new dispatcher and parity tests cover every destination.
- Keep the application fully native and portable.

## 20. Explicit non-goals

This redesign does not itself add:

- saved-group persistence;
- new AirPlay transport behavior;
- new calibration algorithms;
- automatic acoustic measurement;
- new per-receiver DSP or volume semantics beyond the existing typed receiver-level capability;
- custom Windows title-bar controls;
- animated visualizers without truthful audio data;
- background imagery, gradients, glass effects, or decorative waveform noise;
- a second advanced navigation tree;
- web content or network-fetched fonts/assets.

## 21. Definition of design completion

The redesign is implemented only when:

- every page uses the active semantic theme;
- Overview supports the complete daily workflow without navigation;
- all six destinations render through one dispatcher;
- Light, Dark, and High Contrast meet the approved roles;
- German and English are complete and switchable;
- Standard and Advanced modes keep identical actions and positions;
- every Required row in the capability matrix works end to end;
- all state, responsive, accessibility, localization, and repaint gates pass;
- the manual acceptance matrix is recorded against the final integrated binary;
- no placeholder control, fabricated metric, or support-unsafe field remains.
