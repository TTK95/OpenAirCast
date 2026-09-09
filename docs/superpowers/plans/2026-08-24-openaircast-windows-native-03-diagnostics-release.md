# OpenAirCast Windows Native Diagnostics and Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate the approved diagnostics/calibration contracts into the Windows Native shell, expose support-safe copy/export, and verify one accessible, performant, production-oriented Windows release.

**Architecture:** This plan integrates/replaces diagnostics Tasks 12–15 without creating a parallel registry, metrics model, calibration algorithm, alias algorithm, or export schema. Exactly one `DiagnosticsHandle` reaches `AppHandle`, a page-local state object polls immutable diagnostics at no more than 4 Hz only while visible/restored, and UI commands dispatch the typed summary/export contracts already completed by diagnostics Task 11. A presentation-safe calibration shell bridge maps widget events onto Task-9 backend commands without owning calibration math. Final tasks exercise the entire application matrix and record evidence against one integrated binary.

**Tech Stack:** Rust 2021, existing `DiagnosticsRegistry`/`DiagnosticsHandle`, egui/eframe 0.36.1, AccessKit, egui_kittest, serde JSON support export, wgpu, Windows MSVC packaging.

**Spec:** `docs/superpowers/specs/2026-08-24-openaircast-windows-native-redesign-design.md`

## Global Constraints

- Subprojects 1 and 2 must be complete, reviewed, and green.
- Diagnostics/calibration plan Tasks 9, 10, and 11 must be complete and reviewed. They own persisted manual calibration, applied presentation delays, support-alias construction, `build_support_summary`, the versioned support-safe export, typed Copy/Export application events, save/clipboard destinations, off-thread build/write execution, and structured completion results. Task 1 here is the sole integration/replacement for diagnostics Task 13; Tasks 2–7 integrate/replace diagnostics Tasks 12, 14, and 15.
- The UI never derives transport truth, serializes domain structs, creates receiver aliases, redacts free text, chooses raw export fields, writes export files, or reads registry internals. It consumes approved immutable diagnostics types, Task-11 `SupportAliasTable`, and typed Copy/Export effect results.
- Support export has no unsafe mode. It excludes real receiver/device names and IDs, MAC/IP/ports, serial/group/pairing/PTP/SSRC/RTSP identity, user paths, headers/bodies, keys, payload/audio bytes, packet hex, nonces, tags, and raw error chains.
- Scheduled diagnostics reads occur at most once per 250 ms due tick while the Diagnostics destination is visible and the main window is visible/restored. The main Speakers Advanced section may perform one immutable snapshot read only on page entry, Advanced enablement, or explicit Refresh; it has no timer. No catch-up burst, hidden/minimized poll, background Speakers poll, or ambient animation is allowed.
- Standard mode uses a compact plain-language health summary. Advanced mode adds Overview, Speakers, Calibration, Events, and Advanced diagnostics views without changing global navigation or daily-control actions.
- All metrics include units and freshness/Unknown/Stale semantics. Do not claim receiver rendering, acoustic measurement, packet loss, guaranteed synchronization, or timing precision beyond the approved diagnostics truth model.
- Every task is TDD, ends in an exact-path commit, and receives independent review. Final completion requires automated and manual evidence; unchecked hardware observations remain explicitly open.
- `app` and `ui` are private modules of the `openaircast` binary. Registry/bridge tests live under `src/app`; renderer and full-shell tests live under `src/ui/tests`; both run with `cargo test -p homepod-cast --bin openaircast` plus the exact module filter shown in each task. Existing `crates/homepod-cast/tests` suites remain only when they exercise public library contracts such as calibration, diagnostics builders/schema, or backend lifecycle.

## Prerequisite Gate

```powershell
& $cargo test -p homepod-cast --test calibration
& $cargo test -p homepod-cast --test diagnostics_export
& $cargo test -p homepod-cast --bin openaircast ui::tests::gui_capabilities
```

Expected: these three prerequisite suites PASS. The binary-internal `app::diagnostics_registry_tests` module is deliberately not a prerequisite; Task 1 creates/runs it RED then GREEN. Confirm `DiagnosticsHandle::{snapshot,events_since}`, `build_support_summary`, `build_support_export`, `SupportAliasTable`, `AppEvent::{RequestDiagnosticsSummaryCopy,RequestDiagnosticsExport}`, and their structured completion events/effects are already provided by Task 11. Confirm Task 9 publishes requested/effective/applied calibration state and `BackendCommand::Calibration`. Stop if any contract is absent; finish its owning Task 9–11 implementation instead of recreating it in UI code.

Task 11 owns this presentation-safe alias source, reused by export and UI:

```rust
pub struct SupportAliasTable {
    session_aliases: std::collections::HashMap<SessionId, String>,
    receiver_aliases: std::collections::HashMap<ReceiverSessionKey, String>,
}

impl SupportAliasTable {
    pub fn for_diagnostics(
        snapshot: &DiagnosticsSnapshot,
        events: &[DiagnosticEvent],
    ) -> Self;
    pub fn session_alias(&self, session: SessionId) -> Option<&str>;
    pub fn receiver_alias(&self, key: &ReceiverSessionKey) -> Option<&str>;
}
```

Alias construction stays in the diagnostics module. UI state stores only the returned table/aliases and never formats a `ReceiverId`, `DeviceId`, or `SessionId`.

## File Structure

```text
crates/homepod-cast/src/
├── diagnostics_window.rs          # page-local cursor/cache/4 Hz state
├── app/
│   └── diagnostics_registry_tests.rs # binary-internal registry/composition tests
└── ui/pages/diagnostics/
    ├── mod.rs                      # Standard/Advanced page dispatcher
    ├── overview.rs
    ├── speakers.rs
    ├── calibration.rs
    ├── events.rs
    └── advanced.rs

crates/homepod-cast/src/ui/tests/
├── diagnostics_ui.rs              # private renderer/shell tests
├── windows_native_speakers.rs     # extended from Subproject 2
└── windows_native_release.rs      # private full-binary contract

crates/homepod-cast/tests/
└── diagnostics_truthfulness.rs    # public diagnostics summary/export only
```

---

### Task 1: Wire exactly one DiagnosticsHandle through AppHandle

**Files:**
- Modify: `crates/homepod-cast/src/app_handle.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/src/tray.rs`
- Create: `crates/homepod-cast/src/app/diagnostics_registry_tests.rs`

**Interfaces:**
- Consumes: diagnostics plan registry, the exact one-shot `DeviceBridgeParts::diagnostics` receiver from Subproject 2, single/group client diagnostics sources, session lifecycle, and the existing shell composition.
- Produces: one read-only handle on `AppHandle`, deterministic lifecycle registration, and no diagnostics mutation methods in views.

```rust
impl AppHandle {
    pub fn diagnostics_handle(&self) -> DiagnosticsHandle;
}
```

- [ ] **Step 1: Write the one-registry full-flow test**

Declare `#[cfg(test)] mod diagnostics_registry_tests;` in `app/mod.rs`. In that binary-internal module, start fake application composition, create a two-receiver session, publish timing/audio/client/backend observations, and assert the handle reports one active session and two stable presentation-safe rows. Assert a restarted session receives a fresh session alias and late old-generation observations cannot enter the new session.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast app::diagnostics_registry_tests`.

Expected: FAIL until the application owns one registry and exposes its read handle through `AppHandle`.

- [ ] **Step 3: Compose and retain the registry once**

Move `DeviceBridgeParts::diagnostics` exactly once into the registry; never call `resubscribe` to manufacture a replacement owner. Give only the read handle to `AppHandle`. Wrap registry lifecycle commands in a crate-private `DiagnosticsSessionPort` exposing only `register_session` and `finish_session`, and give only that port to session/backend integration. Keep the registry owner in application composition for Task-11 Copy/Export effects. Do not map diagnostics events into `UiSnapshot` and do not add command methods to `DiagnosticsHandle`.

```rust
pub(crate) trait DiagnosticsSessionPort: Send + Sync {
    fn register_session(&self, registration: SessionRegistration);
    fn finish_session(&self, session: SessionId, reason: SessionStopReason);
}

impl cast::Session {
    pub(crate) fn diagnostics_source(&self) -> ClientDiagnosticsSource;
}
```

- [ ] **Step 4: Preserve lifecycle and tray boundaries**

Register both single-receiver `Connection` and group `AirPlayClient` sources before streaming publication. Finish old registrations after worker cancellation on stop/restart/failure, preserve retained history, reject stale generations, and make tray Diagnostics navigation open the existing destination without reading metrics.

- [ ] **Step 5: Run integration/lifecycle tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast app::diagnostics_registry_tests
& $cargo test -p homepod-cast --test backend_lifecycle
& $cargo test -p homepod-cast --test recovery
git diff --check
git add crates/homepod-cast/src/app_handle.rs crates/homepod-cast/src/app/mod.rs crates/homepod-cast/src/app/effect.rs crates/homepod-cast/src/app/diagnostics_registry_tests.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/src/main.rs crates/homepod-cast/src/tray.rs
git commit -m "feat(app): wire native diagnostics handle"
```

### Task 2: Add bounded diagnostics window state and page navigation

**Files:**
- Create: `crates/homepod-cast/src/diagnostics_window.rs`
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/mod.rs`
- Delete after replacement tests are ready: `crates/homepod-cast/src/ui/pages/diagnostics.rs`
- Modify: `crates/homepod-cast/src/ui/pages/mod.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/diagnostics_ui.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Consumes: Task 1 read handle, Subproject-1 resources, window visibility/minimized state, monotonic elapsed input supplied by the shell.
- Produces: `DiagnosticsWindowState`, local subpage/cursor/cache state, and the Standard/Advanced diagnostics dispatcher.

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiagnosticsPage {
    #[default] Overview, Speakers, Calibration, Events, Advanced,
}

pub struct DiagnosticsWindowState {
    pub page: DiagnosticsPage,
    pub event_cursor: EventCursor,
    pub last_poll_elapsed_ns: Option<u64>,
    pub cached_snapshot: DiagnosticsSnapshot,
    pub cached_events: Vec<DiagnosticEvent>,
    pub latest_gap: Option<EventGap>,
    pub aliases: SupportAliasTable,
    pub main_speakers_snapshot_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsVisibility { VisibleRestored, Hidden, Minimized, OtherPage }

impl DiagnosticsWindowState {
    pub fn poll_if_due(
        &mut self,
        handle: &DiagnosticsHandle,
        now_elapsed_ns: u64,
        visibility: DiagnosticsVisibility,
    ) -> bool;
}
```

- [ ] **Step 1: Write 4 Hz and visibility tests**

Declare `mod diagnostics_ui;` in the existing `ui/tests/mod.rs`. In that binary-internal module, simulate 10 seconds at 1 ms render cadence and assert at most 40 polls; one snapshot plus one event read per due tick; `last_poll = now` with no catch-up burst; zero polls while hidden, minimized, closed, or another destination is selected.

- [ ] **Step 2: Write diagnostics page-bar accessibility tests**

Assert Standard mode shows only the compact health summary. Advanced mode exposes five localized subpages, keyboard arrow navigation, selected/current state, 44-point targets, and stable local filters/cursor across app snapshot revisions.

- [ ] **Step 3: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui`.

Expected: FAIL because bounded window state and subpage dispatcher do not exist.

- [ ] **Step 4: Implement one due timer and dispatcher**

Use `now - last_poll >= 250_000_000`, assign `last_poll = now`, perform both reads once, rebuild aliases through Task-11 `SupportAliasTable::for_diagnostics`, and request repaint after 250 ms only while Diagnostics is visible and restored. Store UI-local selection/filter state outside `UiSnapshot`; never mutate registry data when filters or clear-cursor change. Declare `mod diagnostics_window;` in `main.rs`. Atomically replace `ui/pages/diagnostics.rs` with `ui/pages/diagnostics/mod.rs`; never leave both module-root forms present.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
git diff --check
git add crates/homepod-cast/src/diagnostics_window.rs crates/homepod-cast/src/main.rs crates/homepod-cast/src/ui/pages/diagnostics.rs crates/homepod-cast/src/ui/pages/diagnostics/mod.rs crates/homepod-cast/src/ui/pages/mod.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/diagnostics_ui.rs
git commit -m "feat(ui): add bounded native diagnostics shell"
```

### Task 3: Implement Diagnostics Overview and Speakers

**Files:**
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/overview.rs`
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/speakers.rs`
- Modify: `crates/homepod-cast/src/ui/pages/speakers.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/ui/components/metric_tile.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/ui/tests/diagnostics_ui.rs`
- Modify: `crates/homepod-cast/src/ui/tests/windows_native_speakers.rs`

**Interfaces:**
- Consumes: cached diagnostics snapshot/events, conservative health model, Task-11 support aliases, semantic tokens and Metric Tile.
- Produces: truthful compact/advanced Diagnostics Overview, stable per-speaker diagnostics rows, and the approved diagnostics-backed Advanced facts on the main Speakers page.

- [ ] **Step 1: Add view-model fixture tests**

Cover no session, one NTP receiver, group PTP primary/shared secondary, stale timing, capture drop, UDP failure, reconnecting, and terminal error. Assert Unknown/Not measured/Stale with units and ages; Task-11 support aliases rather than receiver identity; local versus shared metric separation; and no prohibited claims. On the main Speakers page, assert Advanced mode adds sync-reference kind/freshness, requested/effective/applied calibration, and diagnostic age without moving or replacing the Standard controls.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui`.

Expected: FAIL because the two renderers are absent.

- [ ] **Step 3: Implement Standard health summary and Advanced Overview**

Standard renders plain-language health plus one real recommended action. Advanced Overview renders signal path, session duration/count, shared buffer/capture/scheduler/prepared-frame facts, summed receiver-local failure/recovery demand, and latest actionable typed error. Missing values remain Unknown/Not measured.

- [ ] **Step 4: Implement stable Speakers diagnostics**

Render lifecycle, local datagram/retransmit/feedback counters, timing-source kind, sample age/freshness, calibration applied state, and support alias. Stable row expansion uses the existing stable receiver identity internally but never displays it. Extend the main Speakers page with the approved diagnostics-backed Advanced facts. It performs one synchronous immutable `DiagnosticsHandle::snapshot()` only when entering main Speakers with Advanced enabled, when Advanced becomes enabled on that page, or on explicit Refresh; it stores the presentation-safe result/alias table in `DiagnosticsWindowState`, schedules no timer there, and performs no periodic/background read outside the Diagnostics destination.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_speakers
git diff --check
git add crates/homepod-cast/src/ui/pages/diagnostics/overview.rs crates/homepod-cast/src/ui/pages/diagnostics/speakers.rs crates/homepod-cast/src/ui/pages/speakers.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/mod.rs crates/homepod-cast/src/ui/components/metric_tile.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/diagnostics_ui.rs crates/homepod-cast/src/ui/tests/windows_native_speakers.rs
git commit -m "feat(ui): add truthful native diagnostics views"
```

### Task 4: Implement Calibration, Events, and Advanced diagnostics

**Files:**
- Modify: `crates/homepod-cast/src/app/device_bridge.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/snapshot.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/calibration.rs`
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/events.rs`
- Create: `crates/homepod-cast/src/ui/pages/diagnostics/advanced.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/app/shell_backend_contract_tests.rs`
- Modify: `crates/homepod-cast/src/ui/tests/diagnostics_ui.rs`

**Interfaces:**
- Consumes: Task-9 `BackendCommand::Calibration`, authoritative requested/effective/applied calibration state, structured bounded events/gaps, and exact advanced metric semantics.
- Produces: widget-safe calibration events, app-owned `CalibrationRequestView`, `PendingCommandKind::Calibration`, `RestartReasonView::CalibrationChanged`, local event filters, and truthful dense Advanced presentation. Only `device_bridge.rs` constructs backend calibration types.

Extend the existing public widget-safe `AppEvent` enum with exactly these
variants without removing or renaming any prior variant:

```rust
pub enum AppEvent {
    ApplyCalibrationRequested {
        expected_desired_revision: u64,
        reference: Option<ReceiverId>,
        requested_relative_delay_ns: std::collections::BTreeMap<ReceiverId, i64>,
    },
    ResetCalibrationRequested { expected_desired_revision: u64 },
    RunCalibrationClickTestRequested,
}

pub(crate) enum CalibrationRequestView {
    Apply {
        expected_desired_revision: u64,
        reference: Option<ReceiverId>,
        requested_relative_delay_ns: std::collections::BTreeMap<ReceiverId, i64>,
    },
    Reset { expected_desired_revision: u64 },
    RunClickTest,
}
```

Extend the existing private enums with the exact variants
`DeviceRequest::Calibration(CalibrationRequestView)`,
`PendingCommandKind::Calibration`, and
`RestartReasonView::CalibrationChanged`; update every exhaustive match in the
same task.

- [ ] **Step 1: Write interaction and truthfulness tests**

Assert calibration disabled below two active receivers, shared reference selector, signed 0.1 ms controls restricted to exact 100,000 ns steps, requested/effective preview, four-click action, Reset/Apply, matching command completion/failure, `CalibrationChanged` restart presentation, and pending-restart/applied/failure states. Events cover UTC/session-relative time, local filters, and clear-cursor only; Advanced covers exact units/limitations and gap/drop disclosures.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui`.

Expected: FAIL because the three renderers are absent.

- [ ] **Step 3: Implement calibration with typed events only**

Validate membership, finite/bounded signed nanoseconds, exact 100,000 ns display steps, and expected desired revision in the reducer, then emit `DeviceRequest::Calibration`. `device_bridge.rs` alone converts the request to Task-9 `CalibrationCommand`/`BackendCommand`; it maps calibration restart state to `RestartReasonView::CalibrationChanged` and strips raw backend error strings from failures. Track the request by `PendingCommandKind::Calibration`; snapshot truth supplies requested/effective/applied state. Do not expose live retiming or automatic acoustic claims. Render room-reflection/manual-listening disclosure and authoritative state.

- [ ] **Step 4: Implement Events and Advanced**

Filters change only page-local presentation. Clear advances the local cursor without deleting registry history. Advanced labels explicitly distinguish path delay, estimated relative clock drift/fit, control-request duration, scheduler distribution, requested latency/render lead, capacities, counter start, and event drops.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
& $cargo test -p homepod-cast --test calibration
& $cargo test -p homepod-cast --bin openaircast app::shell_backend_contract_tests
git diff --check
git add crates/homepod-cast/src/app/device_bridge.rs crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/effect.rs crates/homepod-cast/src/app/reducer.rs crates/homepod-cast/src/app/snapshot.rs crates/homepod-cast/src/app/shell_backend_contract_tests.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/pages/diagnostics/calibration.rs crates/homepod-cast/src/ui/pages/diagnostics/events.rs crates/homepod-cast/src/ui/pages/diagnostics/advanced.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/diagnostics_ui.rs
git commit -m "feat(ui): complete native diagnostics pages"
```

### Task 5: Connect the existing Task-11 Copy summary and Export commands

**Files:**
- Modify: `crates/homepod-cast/src/ui/pages/diagnostics/mod.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/ui/tests/diagnostics_ui.rs`

**Interfaces:**
- Consumes unchanged from completed diagnostics Task 11: `AppEvent::{RequestDiagnosticsSummaryCopy,RequestDiagnosticsExport}`, typed progress/completion state, `support_export_input`, `build_support_summary`, `build_support_export`, and artifact media type/schema.
- Produces: accessible buttons and localized presentation of the existing typed progress/completion state. This task owns no DTO, alias/redaction logic, clipboard/file destination, serialization, or off-thread effect.

- [ ] **Step 1: Write end-to-end copy/export tests**

Assert each button dispatches exactly the existing Task-11 event, duplicate clicks obey the existing bounded in-progress state, and Task-11 success/cancel/failure results render localized scoped notices without a path. Retain Task-11 regression assertions that both operations execute off the eframe thread, use the same allowlisted DTO representation, and exclude hostile identity/path/error sentinels from clipboard bytes, artifact bytes, and accessibility text.

- [ ] **Step 2: Run RED**

```powershell
& $cargo test -p homepod-cast --test diagnostics_export
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
```

Expected: Task-11 export tests already PASS; only the UI interaction test is RED until the buttons connect to those committed events.

- [ ] **Step 3: Connect UI to the committed typed effects**

Dispatch only `RequestDiagnosticsSummaryCopy` and `RequestDiagnosticsExport`. Read only Task-11 typed progress/completion presentation state. Do not modify/reimplement the reducer, effect executor, save dialog, clipboard service, registry export input, builders, aliases, schema, redaction, or file writing. If one of those committed contracts is absent, stop and return to diagnostics Task 11.

- [ ] **Step 4: Render commands and completion notices**

Copy summary and Export are explicit accessible commands. Keep one filled page action according to context; operation progress is textual and bounded. File-dialog cancellation is informational, not failure.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --test diagnostics_export
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
git diff --check
git add crates/homepod-cast/src/ui/pages/diagnostics/mod.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/diagnostics_ui.rs
git commit -m "feat(ui): connect support-safe diagnostics export"
```

### Task 6: Add localization, accessibility, truthfulness, and repaint gates

**Files:**
- Create: `crates/homepod-cast/tests/diagnostics_truthfulness.rs`
- Modify: `crates/homepod-cast/src/ui/tests/diagnostics_ui.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `docs/VALIDATION.md`

**Interfaces:**
- Consumes: complete diagnostics UI and export bridge.
- Produces: automated cross-cutting gates for the final release task.

- [ ] **Step 1: Add the prohibited-language/redaction test**

Keep `tests/diagnostics_truthfulness.rs` library-only: walk the public support summary and serialized export builders and reject raw identity/path/audio/packet/crypto fixtures and prohibited claims unless inside explicit negating limitation copy defined by the diagnostics spec. In binary-internal `ui::tests::diagnostics_ui`, separately walk rendered labels, help, and accessibility strings against the same fixtures; do not import private `app` or `ui` modules from the integration test.

- [ ] **Step 2: Add the complete UI matrix test**

Render Standard/Advanced × German/English × Light/Dark × 1120 × 720/900 × 600. Assert roles/names/selected/current/disabled/live status, no mixed language, no overflow, correct tab order, Unknown/Stale labels, and no unlabeled icon action.

- [ ] **Step 3: Add repaint/hidden-window tests**

Assert at most 4 Hz scheduled polling while Diagnostics is visible/restored, zero scheduled polling/repaint-after while hidden/minimized/another page, exactly one permitted main-Speakers read per entry/Advanced-enable/explicit-Refresh trigger, no background Speakers reads, no catch-up burst, and no continuous component animation.

- [ ] **Step 4: Run cross-cutting tests**

```powershell
& $cargo test -p homepod-cast --test diagnostics_truthfulness
& $cargo test -p homepod-cast --bin openaircast ui::tests::diagnostics_ui
& $cargo test -p homepod-cast --test diagnostics_export
rg -n "request_repaint_after" crates/homepod-cast/src
```

Expected: PASS; the one diagnostics schedule is guarded by visible/restored Diagnostics state.

- [ ] **Step 5: Record and commit evidence**

```powershell
git add crates/homepod-cast/tests/diagnostics_truthfulness.rs crates/homepod-cast/src/ui/tests/diagnostics_ui.rs crates/homepod-cast/src/ui/i18n.rs docs/VALIDATION.md
git commit -m "test(ui): gate native diagnostics truthfulness"
```

### Task 7: Build and accept the final Windows release

**Files:**
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/windows_native_release.rs`
- Modify: `docs/VALIDATION.md`
- Modify: `docs/NEXT_STEPS.md`
- Modify as required by existing packaging: `build.ps1`
- Modify as required by final user docs: `README.md`

**Interfaces:**
- Consumes: all three redesign subprojects, existing packaging, hardware plan, and release acceptance matrix.
- Produces: one reviewed binary plus complete automated/manual evidence and an explicit list of unperformed hardware-only checks.

- [ ] **Step 1: Add the final contract test**

Declare `mod windows_native_release;` in `ui/tests/mod.rs`. In that binary-internal module, exercise all six pages, every Required capability, seven session phases, staged action priority, two locales, two presentation modes, theme preference plus High Contrast mapping, diagnostics/export, hide/restore, and safe shutdown using fakeable boundaries.

- [ ] **Step 2: Run full automated verification**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_release
& $cargo test -p homepod-cast
& $cargo test --workspace
& $cargo check --workspace --all-targets
& $cargo build -p homepod-cast --release
& $cargo fmt --all -- --check
git diff --check
```

Expected: all task-owned tests/builds PASS. Record known immutable baseline formatting issues separately; do not hide new task-owned drift.

- [ ] **Step 3: Package and hash one binary**

Run the existing packaging path, verify license/notices/fonts/icon/version metadata, and record executable/package SHA-256 hashes. Do not include source worktrees, logs, diagnostics artifacts, or assistant files.

- [ ] **Step 4: Execute manual visual/accessibility matrix**

Verify Windows 10/11; Light/Dark/High Contrast; German/English; 100/125/150/175/200% scale; 1120 × 720 and 900 × 600; mixed-DPI movement; keyboard-only flows; Accessibility Insights, Narrator, NVDA; Reduced Motion; Remote Desktop; tray restore; visible/hidden idle CPU; real discovery/group start-stop/partial failure/retry/suspend/network interruption. Mark only observed cases passed.

- [ ] **Step 5: Execute approved hardware gates**

Run real HomePod/multiroom/calibration/export checks from the resilience and diagnostics hardware plans. Identity-bearing lab notes stay outside support export. If hardware is unavailable, keep those rows open and do not claim production acceptance complete.

- [ ] **Step 6: Request independent release review**

Review the complete final diff, dependency/license changes, redaction tests, package contents, and recorded evidence. Fix/retest/re-review every Critical/Important finding.

- [ ] **Step 7: Update docs and commit the release gate**

```powershell
git add crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/windows_native_release.rs docs/VALIDATION.md docs/NEXT_STEPS.md build.ps1 README.md
git commit -m "release: verify Windows Native OpenAirCast"
```

## Subproject 3 Spec Coverage Check

- Diagnostics IA, five Advanced views, 4 Hz behavior, Metric Tile truth, Task-11 aliases/copy/export wiring, calibration shell bridge, events, and the diagnostics-backed Advanced facts on the main Speakers page: Tasks 1–6.
- Error safety, redaction, Unknown/Stale, units/age, non-blocking export: Tasks 3–6.
- Full automated verification matrix: Tasks 6–7.
- Manual Windows, theme, locale, DPI, keyboard, screen reader, motion, Remote Desktop, CPU, tray, hardware, and package acceptance: Task 7.
- Final design completion from spec section 21 is claimed only when every automated gate passes and every required manual/hardware row is either passed or explicitly reported open.
