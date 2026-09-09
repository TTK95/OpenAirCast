# OpenAirCast Windows Native Capability Pages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connect the Windows Native shell to the resilient device backend and deliver fully functional Speakers, Groups, Audio, and Settings pages without duplicating backend behavior or leaking device-domain objects into views.

**Architecture:** Replace the shell-integration work defined by device-resilience Task 18 with the approved presentation architecture. One adapter privately consumes backend snapshots/edges and maps them into presentation-safe runtime events before the reducer sees them. Public widget `AppEvent` values and renderer inputs contain no backend enums; a crate-private device request/effect boundary maps validated presentation values to `BackendCommand` and executes them through `DeviceBackendHandle::try_send`. Pure presentation mappers turn the expanded `UiSnapshot` into page models, and page renderers reuse Subproject-1 components.

**Tech Stack:** Rust 2021, Tokio watch/broadcast, bounded backend commands, egui/eframe 0.36.1, AccessKit, egui_kittest, serde, ArcSwap.

**Spec:** `docs/superpowers/specs/2026-08-24-openaircast-windows-native-redesign-design.md`

## Global Constraints

- Subproject 1 must be complete and approved before this plan starts.
- Device-resilience Task 10 and Tasks 15, 16, and 17 must be committed, independently reviewed, and green. Task 18's shell-integration intent is implemented by Tasks 1–2 here; do not create a second backend or device service.
- `DeviceSnapshot`, `BackendEvent`, `BackendSendError`, and `BackendCommand` stay private to `app/device_bridge.rs` and the service executor. Public widget `AppEvent`, `UiSnapshot`, presentation models, and renderers contain only presentation-safe app-owned types.
- The current dirty backend/controller/calibration work belongs to its existing task owners. Do not begin this plan until those task boundaries are committed or explicitly handed off.
- Preserve the four frozen `AppHandle` methods, local staged-membership semantics, stable `ReceiverId`, one snapshot per frame, generation safety, bounded queues, and non-blocking UI behavior.
- Receiver-level UI is blocked until the backend publishes authoritative configured/effective receiver-level values. If the prerequisite snapshot lacks them, stop at the prerequisite gate and finish the owning resilience task; never display a guessed default.
- Only latency presets reported as available/enabled may be interactive. Unsupported Low/Stable values remain absent or explicitly unavailable according to the backend snapshot.
- Raw `UserFacingError`, `BackendEvent::CommandFailed.error`, and `BackendEvent::Notice.message` never become visible copy. Map command kind, typed notice code/scope, lifecycle, and safe names/counts to localization keys.
- Each task uses TDD, exact-path commits, and independent review. No visual token/layout redesign occurs here; consume Subproject-1 resources.
- `app` and `ui` are private modules of the `openaircast` binary. Tests that inspect their bridge, reducer, presentation, or renderer internals live under `src/app` or `src/ui/tests` and run with `cargo test -p homepod-cast --bin openaircast` plus the exact module filter shown in each task. Keep `crates/homepod-cast/tests` only for genuine integration tests against the public library API.

## Prerequisite Gate

Before Task 1, run:

```powershell
& $cargo test -p homepod-cast backend::controller --lib
& $cargo test -p homepod-cast --test backend_lifecycle
& $cargo test -p homepod-cast --test session_recovery
& $cargo test -p homepod-cast --test system_recovery
```

Inspect the integrated `DeviceSnapshot`. It must expose desired revision/membership, run intent, full `SessionPhase`, receiver lifecycle/retry state, presentation-safe device class/model, saved groups, audio source/endpoints, master/mute, an explicit latency catalog with enabled/disabled reason, auto-connect, persistence health, and authoritative configured/effective receiver levels. Receiver rows must contain exact configured and effective values; the UI may not read persistence state or infer a default. If one field is missing, stop and complete the owning resilience Task 10 or Task 15–17 change and its backend tests; this GUI plan does not invent backend state.

## File Structure

```text
crates/homepod-cast/src/app/
├── device_bridge.rs       # sole backend-type boundary, endpoint-key map, one-shot diagnostics split
├── state.rs               # authoritative shell state and pending command contexts
├── event.rs               # public widget AppEvent plus crate-private RuntimeEvent
├── effect.rs              # crate-private DeviceRequest and existing shell effects
├── reducer.rs             # pure lifecycle/capability transitions
├── snapshot.rs            # presentation-safe immutable projection
└── shell_backend_contract_tests.rs # binary-internal bridge contract tests

crates/homepod-cast/src/ui/pages/
├── speakers.rs
├── groups.rs
├── audio.rs
└── settings.rs

crates/homepod-cast/src/ui/tests/
├── mod.rs
├── windows_native_speakers.rs
├── windows_native_groups.rs
├── windows_native_audio.rs
├── windows_native_settings.rs
└── gui_capabilities.rs
```

Add narrowly scoped components beside the owning page; do not create generic abstractions used by only one page.

---

### Task 1: Build the bounded resilient-device shell bridge

**Files:**
- Create: `crates/homepod-cast/src/app/device_bridge.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/snapshot.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app_handle.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Create: `crates/homepod-cast/src/app/shell_backend_contract_tests.rs`

**Interfaces:**
- Consumes privately inside `app/device_bridge.rs`: `DeviceBackendHandle`, `DeviceBackendUpdates::{state,events,diagnostics}`, `DeviceSnapshot`, `BackendEvent`, `BackendSendError`, and `BackendCommand`.
- Produces: widget-safe `AppEvent`, crate-private `RuntimeEvent`, `AppEffect::SendDeviceRequest`, complete presentation-safe `DeviceViewState`, a one-shot `DeviceBridgeParts::diagnostics` handoff, and an unchanged public `AppHandle` method surface.

Keep every existing public widget/preference `AppEvent` variant. Do not add a
`DeviceSnapshot`, `BackendEvent`, `BackendCommand`, or other backend-owned
variant. Extend the existing private `AppEffect` enum with exactly this variant:

```rust
pub(crate) enum AppEffect {
    SendDeviceRequest {
        request: DeviceRequest,
        context: PendingCommandKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingCommandKind {
    Membership, RunIntent, MasterVolume, Mute, ReceiverLevel,
    AudioEndpoint, LatencyPreset, AutoConnect,
    SaveGroup, DeleteGroup, ActivateGroup, RetryReceiver,
}

pub(crate) enum RuntimeEvent {
    Widget(AppEvent),
    DeviceSnapshot(std::sync::Arc<DeviceViewState>),
    DeviceEdge(DeviceEdgeView),
    DeviceCommandAccepted { id: u64, kind: PendingCommandKind },
    DeviceCommandRejected { kind: PendingCommandKind, reason: DeviceQueueFailure },
}

pub(crate) enum DeviceRequest {
    SetMembership(std::collections::BTreeSet<ReceiverId>),
    SetRunIntent(RunIntentView),
    SetMasterVolume(f32),
    SetMuted(bool),
    SetReceiverLevel { receiver: ReceiverId, level: f32 },
    SetAudioEndpoint(AudioEndpointKey),
    SetLatencyPreset(LatencyChoice),
    SetAutoConnect(bool),
    SaveGroup(GroupSaveRequest),
    DeleteGroup(SavedGroupId),
    ActivateGroup { id: SavedGroupId, start: bool },
    RetryReceiver(ReceiverId),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AudioEndpointKey(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatencyChoice { Normal, Low, Stable }

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupMemberRequest {
    pub receiver: ReceiverId,
    pub level: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupSaveRequest {
    pub id: Option<SavedGroupId>,
    pub name: String,
    pub members: Vec<GroupMemberRequest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceQueueFailure { Busy, Closed }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SeverityView { Info, Warning, Error }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoticeCodeView {
    DiscoveryFailed, SessionFailed, CaptureUnavailable, PersistenceWrite, QueueOverloaded,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorScopeView { Discovery, Session, Capture, Persistence, Queue }

pub(crate) enum DeviceEdgeView {
    CommandCompleted { id: u64 },
    CommandFailed { id: u64 },
    Notice { severity: SeverityView, code: NoticeCodeView, scope: ErrorScopeView },
    EventLagged,
}

pub(crate) struct DeviceBridgeParts {
    pub commands: DeviceBackendHandle,
    pub runtime: DeviceBridgeRuntime,
    pub diagnostics: DiagnosticEventReceiver,
}

pub(crate) struct DeviceBridgeRuntime {
    join: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestartReasonView {
    MembershipChange, SavedGroupActivated, SecondaryRejoin,
    FailedTargetRemoved, PrimaryReplaced, ReceiverAddressChanged,
    LocalInterfaceChanged, TimingModeTransition, StreamFormatChanged,
    LatencyPresetChanged, SystemResume, DeadRtspRecovered,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionPhaseView {
    Stopped,
    Starting { generation: u64 },
    Streaming { generation: u64 },
    Degraded { generation: u64 },
    Restarting { generation: u64, reason: RestartReasonView },
    Stopping { generation: u64 },
    Failed { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunIntentView { Stopped, Running }

pub(crate) fn split_device_backend(
    handle: DeviceBackendHandle,
    updates: DeviceBackendUpdates,
    runtime_events: std::sync::mpsc::SyncSender<RuntimeEvent>,
) -> DeviceBridgeParts;
```

`split_device_backend` destructures `DeviceBackendUpdates` exactly once. The bridge runtime owns only `state` and `events`; application composition owns `diagnostics` without cloning/resubscribing it and moves that same receiver into the single diagnostics registry in Subproject 3.

`AppHandle::dispatch(AppEvent)` wraps the widget event as `RuntimeEvent::Widget` before the bounded actor queue. Backend bridge producers can send only crate-private `RuntimeEvent`; renderers cannot construct device snapshot/edge variants.

Define the app-owned snapshot rows completely. `ReceiverViewState` contains stable `ReceiverId`, localized-safe name, device-class/model, lifecycle/retry facts, desired/active/primary flags, and authoritative configured/effective levels. `AudioEndpointViewState` contains only an app-owned `AudioEndpointKey`, display name, availability, and selected/default flags. `LatencyOptionViewState` contains `LatencyChoice`, enabled state, and a typed unavailability reason. Saved-group rows contain stable group/receiver IDs, safe names, and configured levels. No row contains a raw Windows endpoint ID, backend error text, address, service record, transport, or diagnostics event.

- [ ] **Step 1: Write the frozen-boundary and lag-resync tests**

Declare `#[cfg(test)] mod shell_backend_contract_tests;` in `app/mod.rs` so
the test module is compiled only as part of the private `openaircast` binary.

```rust
#[test]
fn view_boundary_remains_the_four_frozen_methods(handle: &AppHandle) {
    let _ = handle.dispatch(AppEvent::ShowMainWindow);
    let _: Arc<UiSnapshot> = handle.snapshot();
    let _: Vec<UiEffect> = handle.drain_ui_effects();
    let _subscription = handle.install_waker(Arc::new(|| {}));
}

#[test]
fn backend_event_lag_resyncs_from_watch_without_fabricating_edges() {
    let mut rig = bridge_rig();
    rig.lag_events_then_publish(authoritative_snapshot(42));
    assert_eq!(rig.ui_snapshot().device_revision, 42);
    assert!(rig.ui_snapshot().notice_is_local_queue_warning());
}
```

- [ ] **Step 2: Run focused tests to verify RED**

Run `& $cargo test -p homepod-cast --bin openaircast app::shell_backend_contract_tests`.

Expected: FAIL because the resilient adapter/events/effect do not exist.

- [ ] **Step 3: Implement snapshot and event ingestion**

Consume the watch receiver as the authoritative state source. Inside `device_bridge.rs`, convert each complete backend snapshot to `Arc<DeviceViewState>` before sending `RuntimeEvent::DeviceSnapshot`; convert low-volume edges to `DeviceEdgeView` without copying free-form error/message text. Broadcast lag triggers a watch resync plus a local localized warning. Return the original `DeviceBackendUpdates::diagnostics` receiver through `DeviceBridgeParts` exactly once; it never enters `UiSnapshot` or a renderer.

- [ ] **Step 4: Implement non-blocking command execution**

`ServiceExecutor` passes `DeviceRequest` to `device_bridge.rs`, which validates/maps it to the exact `BackendCommand` and calls only `DeviceBackendHandle::try_send`. On success, send `RuntimeEvent::DeviceCommandAccepted { id, kind }`; map Busy/Closed to the app-owned `DeviceQueueFailure` before sending `RuntimeEvent::DeviceCommandRejected`. Never await capacity on the reducer or eframe thread.

- [ ] **Step 5: Project presentation-safe device state**

Add immutable sorted rows for receiver name/device class/model/lifecycle/retry/desired/active/primary/configured/effective levels, saved groups, desired revision, run intent/session phase, audio endpoints/source, master/mute, explicit latency availability, auto-connect, and persistence health. Raw endpoint identifiers remain in a process-lifetime bridge table and are represented to the shell by monotonically allocated opaque `AudioEndpointKey(u64)` values; keys are never rendered, logged, persisted, or exported. Do not copy addresses, service records, error strings, transports, or diagnostics events.

- [ ] **Step 6: Run bridge and regression tests**

```powershell
& $cargo test -p homepod-cast --bin openaircast app::shell_backend_contract_tests
& $cargo test -p homepod-cast --bin openaircast app::
& $cargo test -p homepod-cast --test backend_lifecycle
```

Expected: PASS for frozen boundary, sorted projection, lag resync, non-blocking Busy/Closed handling, and unchanged backend lifecycle.

- [ ] **Step 7: Commit**

```powershell
git diff --check
git add crates/homepod-cast/src/app/device_bridge.rs crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/effect.rs crates/homepod-cast/src/app/reducer.rs crates/homepod-cast/src/app/snapshot.rs crates/homepod-cast/src/app/mod.rs crates/homepod-cast/src/app/shell_backend_contract_tests.rs crates/homepod-cast/src/app_handle.rs crates/homepod-cast/src/main.rs
git commit -m "feat(app): bridge resilient devices into shell snapshots"
```

### Task 2: Map complete lifecycle, actions, and localized errors

**Files:**
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/snapshot.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Test: `crates/homepod-cast/src/ui/presentation.rs`
- Test: `crates/homepod-cast/src/app/reducer.rs`

**Interfaces:**
- Consumes: Task 1 full phase/run-intent snapshot, command ID contexts, backend notice code/scope.
- Produces: exhaustive `SessionPresentation`, safe `NoticeModel`, and valid primary/secondary actions for every phase.

```rust
pub struct SessionPresentation {
    pub phase: SessionVisualPhase,
    pub label: TextKey,
    pub primary: Option<LifecycleAction>,
    pub secondary: Vec<CorrectiveAction>,
    pub retain_live_receivers: bool,
}

pub fn map_session(
    phase: &SessionPhaseView,
    run_intent: RunIntentView,
) -> SessionPresentation;
```

- [ ] **Step 1: Write the exhaustive phase/intent table test**

Cover Stopped, Starting, Streaming, Degraded, Restarting, Stopping, Failed and impossible pairs. Assert Degraded/Restarting/Failed-running keep Stop/Stop trying filled; Starting maps Cancel to stop intent; Stopping dispatches nothing; retry never hides an active safety action.

- [ ] **Step 2: Write raw-error exclusion tests**

Feed sentinel strings through `CommandFailed` and Notice messages. Assert rendered/accessibility/clipboard presentation models contain only locale keys plus safe receiver/group names and never the sentinels.

- [ ] **Step 3: Run focused tests to verify RED**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::presentation::tests
& $cargo test -p homepod-cast --bin openaircast app::reducer::tests
```

Expected: FAIL on new phase/action/error mappings.

- [ ] **Step 4: Implement exhaustive mapping and command contexts**

Map Start/Stop to `DeviceRequest::SetRunIntent`, receiver retry to `DeviceRequest::RetryReceiver`, and command failures by retained `PendingCommandKind`. Unknown IDs produce a generic scoped notice without echoing the raw reason. Clear pending state only for matching completion/failure IDs; snapshot truth always wins.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::presentation::tests
& $cargo test -p homepod-cast --bin openaircast app::reducer::tests
& $cargo test -p homepod-cast --bin openaircast app::shell_backend_contract_tests
git diff --check
git add crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/reducer.rs crates/homepod-cast/src/app/snapshot.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/i18n.rs
git commit -m "feat(app): map resilient lifecycle and safe errors"
```

### Task 3: Implement the functional Speakers page

**Files:**
- Modify: `crates/homepod-cast/src/ui/pages/speakers.rs`
- Modify: `crates/homepod-cast/src/ui/components/receiver_card.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/windows_native_speakers.rs`

**Interfaces:**
- Consumes: authoritative receiver device-class/model/lifecycle/retry/configured/effective-level projection and Task-1 `DeviceRequest::{SetReceiverLevel,RetryReceiver}` mapping.
- Produces: `SpeakerPageModel`, receiver-level/retry events, and an accessible functional Speakers page.

Extend the existing `AppEvent` enum with exactly these variants without
removing or renaming any prior variant:

```rust
pub enum AppEvent {
    ReceiverLevelChanged { receiver: ReceiverId, level: f32 },
    RetryReceiverRequested(ReceiverId),
}
```

Declare `#[cfg(test)] mod tests;` once in `ui/mod.rs`, then declare
`mod windows_native_speakers;` in `ui/tests/mod.rs`.

- [ ] **Step 1: Write mixed-health presentation and interaction tests**

Cover available, unavailable, retry-waiting, active primary, active secondary, failed desired, and unknown/stale facts. Assert stable IDs, Standard versus Advanced details in identical positions, authoritative configured/effective level, slider keyboard behavior, and Retry only for eligible receivers.

- [ ] **Step 2: Run the integration test to verify RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_speakers`.

Expected: FAIL because functional level/retry interactions and page model are absent.

- [ ] **Step 3: Implement the page model and reducer events**

Map receiver facts without raw errors or addresses. Validate the finite `0.0..=1.0` shell value in the reducer and emit app-owned `DeviceRequest::SetReceiverLevel`; only `device_bridge.rs` converts it to backend `Volume`/`BackendCommand`. Dispatch Retry through the same request boundary. Slider UI remains snapshot-authoritative; intermediate values may coalesce, final release is dispatched once more. Render all Standard device facts and the device-backed Advanced facts here; Subproject 3 Task 3 owns the later diagnostics-backed sync-reference/freshness, calibration-applied, and diagnostic-age additions to this same main Speakers page.

- [ ] **Step 4: Render with existing receiver components**

Keep one 56-point minimum card/row, accessible names/descriptions, text plus icon for lifecycle, and Advanced facts below the same Standard row. Healthy controls remain enabled during another receiver's failure.

- [ ] **Step 5: Run tests and commit**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_speakers
& $cargo test -p homepod-cast --bin openaircast ui::pages::speakers::tests
git diff --check
git add crates/homepod-cast/src/ui/pages/speakers.rs crates/homepod-cast/src/ui/components/receiver_card.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/mod.rs crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/windows_native_speakers.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/reducer.rs
git commit -m "feat(ui): add functional resilient speakers"
```

### Task 4: Implement saved-group Create, Edit, Delete, and Activate

**Files:**
- Create: `crates/homepod-cast/src/ui/components/group_editor.rs`
- Modify: `crates/homepod-cast/src/ui/components/mod.rs`
- Modify: `crates/homepod-cast/src/ui/pages/groups.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/windows_native_groups.rs`

**Interfaces:**
- Consumes: saved-group snapshots, desired/active membership, receiver-level rows, and typed group backend commands.
- Produces: transient `GroupDraft`, explicit save/delete/activate events, and a fully functional Groups page.

Extend the existing `AppEvent` enum with exactly these variants without
removing or renaming any prior variant:

```rust
pub enum AppEvent {
    SaveGroupRequested {
        id: Option<SavedGroupId>,
        name: String,
        members: Vec<GroupMemberDraft>,
    },
    DeleteGroupRequested(SavedGroupId),
    ActivateGroupRequested { id: SavedGroupId, start: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupMemberDraft {
    pub receiver: ReceiverId,
    pub level: f32,
}
```

- [ ] **Step 1: Write editor validation and command tests**

Declare `mod windows_native_groups;` in `ui/tests/mod.rs`. Assert trimmed non-empty case-insensitively unique names, at least one member, stable group/receiver identity, explicit Cancel, one Save dispatch, explicit destructive confirmation, Activate-only versus Activate-and-start, and no optimistic saved-group mutation before authoritative snapshot confirmation.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_groups`.

Expected: FAIL because the editor and typed UI events do not exist.

- [ ] **Step 3: Implement transient editor and reducer effects**

Keep drafts in page-local egui temporary state keyed by stable group ID; views never persist directly and never construct backend `SavedGroupMember`. Validate every finite level/name/membership in the reducer and emit app-owned `DeviceRequest::{SaveGroup,DeleteGroup,ActivateGroup}`; only `device_bridge.rs` constructs backend group members/commands. On confirmed desired-revision change, refresh a clean stage; preserve and mark a dirty stage stale.

- [ ] **Step 4: Render page and run GREEN**

Render current group, sorted saved groups, one contextual primary action, member summaries, Edit/Delete quiet actions, and honest empty state only when the authoritative list is empty.

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_groups
& $cargo test -p homepod-cast --bin openaircast ui::pages::groups::tests
```

- [ ] **Step 5: Commit**

```powershell
git diff --check
git add crates/homepod-cast/src/ui/components/group_editor.rs crates/homepod-cast/src/ui/components/mod.rs crates/homepod-cast/src/ui/pages/groups.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/windows_native_groups.rs crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/reducer.rs
git commit -m "feat(ui): add saved group management"
```

### Task 5: Implement functional Audio controls

**Files:**
- Modify: `crates/homepod-cast/src/ui/pages/audio.rs`
- Create: `crates/homepod-cast/src/ui/components/audio_source_picker.rs`
- Modify: `crates/homepod-cast/src/ui/components/mod.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/windows_native_audio.rs`

**Interfaces:**
- Consumes: audio-source snapshot, endpoint list/preference, capture health, master/mute, latency availability.
- Produces: mute/endpoint/latency events and the functional Audio page.

Add the exact widget-safe variants `AppEvent::MutedChanged(bool)`,
`AppEvent::AudioEndpointChanged(AudioEndpointSelection)`, and
`AppEvent::LatencyPresetChanged(LatencyChoice)` without changing the existing
variants. `AudioEndpointKey` and `LatencyChoice` are the app-owned Task-1 types:

```rust
pub enum AudioEndpointSelection {
    Default,
    Specific(AudioEndpointKey),
}
```

- [ ] **Step 1: Write capability/availability tests**

Declare `mod windows_native_audio;` in `ui/tests/mod.rs`. Cover default endpoint, explicit endpoint, endpoint removal/recovery, capturing/silent/recovering/unavailable/failed, mute preserving volume, and enabled/disabled latency presets. Assert unsupported controls are absent or explicitly unavailable and never clickable.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_audio`.

Expected: FAIL because typed audio controls are absent.

- [ ] **Step 3: Implement events/effects and page model**

Map presentation-safe values to `DeviceRequest::{SetMuted,SetAudioEndpoint,SetLatencyPreset}` inside the reducer/effect boundary. `device_bridge.rs` resolves `AudioEndpointKey` through its process-lifetime table and rejects unknown/stale keys locally; only it constructs `AudioEndpointPreference` and other backend enums. Do not restart or infer session behavior in the UI. The authoritative follow-up snapshot supplies selected value and capture state.

- [ ] **Step 4: Render and verify**

Reuse Audio Dock for master volume, add a labeled mute switch, endpoint radio/list picker, capture health notice, and available latency options with concise restart consequence text from the catalog.

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_audio
& $cargo test -p homepod-cast --bin openaircast ui::pages::audio::tests
```

- [ ] **Step 5: Commit**

```powershell
git diff --check
git add crates/homepod-cast/src/ui/pages/audio.rs crates/homepod-cast/src/ui/components/audio_source_picker.rs crates/homepod-cast/src/ui/components/mod.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/windows_native_audio.rs crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/reducer.rs
git commit -m "feat(ui): add native audio source controls"
```

### Task 6: Complete Settings with auto-connect and existing shell preferences

**Files:**
- Modify: `crates/homepod-cast/src/ui/pages/settings.rs`
- Modify: `crates/homepod-cast/src/ui/presentation.rs`
- Modify: `crates/homepod-cast/src/ui/i18n.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/windows_native_settings.rs`

**Interfaces:**
- Consumes: Subproject-1 theme/locale/Advanced/hotkey preferences and backend auto-connect snapshot/command.
- Produces: complete ordered Settings sections, `AutoConnectChanged(bool)`, and a truthful About section that preserves Subproject-1's real localized embedded-text Open/Copy actions for the license and third-party notices. Version, license, and notices remain read-only embedded build content; this task adds no decorative action.

- [ ] **Step 1: Write complete Settings tests**

Declare `mod windows_native_settings;` in `ui/tests/mod.rs`. Assert section order Appearance, Language, Behavior, Connection, Keyboard, Advanced information, About; System/Light/Dark; System/Deutsch/English; auto-connect; existing close/tray behavior only when real; valid hotkey editing; Advanced toggle; version/license/notices content; and the working, localized Subproject-1 About Open/Copy actions. Assert Open displays the embedded text and Copy places the same full text on the clipboard without exposing a relative or absolute path. Test failed preference save keeps active memory with scoped Retry and failed auto-connect waits for authoritative state.

- [ ] **Step 2: Run RED**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_settings`.

Expected: FAIL on the ordered functional settings surface.

- [ ] **Step 3: Implement auto-connect event/effect and page sections**

Map `AutoConnectChanged(bool)` to `DeviceRequest::SetAutoConnect`. Reuse Subproject-1 theme/locale/Advanced events and current hotkey validation. Keep High Contrast controlled by Windows and show it as current system state, not a manual radio option. Preserve the working embedded About Open/Copy actions and their localized accessible labels. Do not expose repository or user paths and do not replace the real actions with inert controls.

- [ ] **Step 4: Run GREEN and regressions**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::windows_native_settings
& $cargo test -p homepod-cast --bin openaircast ui::pages::settings::tests
& $cargo test -p homepod-cast --bin openaircast preferences::tests
```

- [ ] **Step 5: Commit**

```powershell
git diff --check
git add crates/homepod-cast/src/ui/pages/settings.rs crates/homepod-cast/src/ui/presentation.rs crates/homepod-cast/src/ui/i18n.rs crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/windows_native_settings.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/reducer.rs
git commit -m "feat(ui): complete Windows Native settings"
```

### Task 7: Verify all non-diagnostics capabilities end to end

**Files:**
- Modify: `crates/homepod-cast/src/ui/tests/mod.rs`
- Create: `crates/homepod-cast/src/ui/tests/gui_capabilities.rs`
- Modify: `crates/homepod-cast/src/ui/pages/overview.rs`
- Modify: `crates/homepod-cast/src/ui/components/change_bar.rs`
- Modify: `docs/VALIDATION.md`

**Interfaces:**
- Consumes: Tasks 1–6 and the real fakeable backend composition.
- Produces: widget-to-command-to-authoritative-snapshot evidence for every non-diagnostics Required capability.

- [ ] **Step 1: Write one full capability-flow test**

Declare `mod gui_capabilities;` in `ui/tests/mod.rs`. Drive staged membership, Apply/Discard/stale, Start/Cancel/Stop, master volume, mute, receiver level, endpoint, latency, auto-connect, save/edit/delete/activate group, and receiver Retry. Assert exact backend commands, command IDs, completion/failure mapping, and new snapshot values. Include a partial-active Degraded snapshot and assert Stop remains filled while Apply/Retry are quiet.

- [ ] **Step 2: Run RED and close integration gaps**

Run `& $cargo test -p homepod-cast --bin openaircast ui::tests::gui_capabilities`.

Expected first run: FAIL on any missing end-to-end connection. Return each failure to its owning Task 1–6 implementation/review boundary, fix it there with an exact-path fix commit, and rerun that focused task before resuming this integration gate; Task 7 does not absorb unbounded adapter/page changes.

- [ ] **Step 3: Run Subproject-2 verification**

```powershell
& $cargo test -p homepod-cast --bin openaircast ui::tests::gui_capabilities
& $cargo test -p homepod-cast --bin openaircast app::shell_backend_contract_tests
& $cargo test -p homepod-cast --bin openaircast ui::
& $cargo test -p homepod-cast
& $cargo check --workspace --all-targets
rg -n "UserFacingError|\.message|\.summary" crates/homepod-cast/src/ui
git diff --check
```

Expected: PASS; raw backend string access has no renderer matches.

- [ ] **Step 4: Record evidence and commit**

Update `docs/VALIDATION.md` with exact commands/results and capability rows.

```powershell
git add crates/homepod-cast/src/ui/tests/mod.rs crates/homepod-cast/src/ui/tests/gui_capabilities.rs crates/homepod-cast/src/ui/pages/overview.rs crates/homepod-cast/src/ui/components/change_bar.rs docs/VALIDATION.md
git commit -m "test(ui): verify native capability flows"
```

### Task 8: Review and checkpoint Subproject 2

**Files:**
- Modify: `docs/NEXT_STEPS.md`

**Interfaces:**
- Consumes: complete Task-1 through Task-7 diff and verification evidence.
- Produces: review-approved capability checkpoint for Subproject 3.

- [ ] **Step 1: Run final non-diagnostics smoke verification**

Build release and manually verify Speakers, Groups, Audio, Settings, partial receiver failure, endpoint recovery, and system theme/language changes. Record only observed results.

- [ ] **Step 2: Request independent full review**

Require Spec and Code verdicts across the Subproject-2 base-to-HEAD diff. Resolve and re-review every Critical/Important finding, rerunning `gui_capabilities`, package tests, and workspace check after fixes.

- [ ] **Step 3: Update checkpoint and commit**

Record task commits, test evidence, review verdict, and the remaining diagnostics dependency gates in `docs/NEXT_STEPS.md`.

```powershell
git add docs/NEXT_STEPS.md
git commit -m "docs: checkpoint Windows Native capability pages"
```

## Subproject 2 Spec Coverage Check

- Sections 7.2–7.4 and 7.6 plus every non-diagnostics Required capability row: Tasks 1–7.
- Exhaustive Degraded/Restarting/Failed action mapping and partial success truth: Task 2.
- Standard/device-backed Advanced details without separate navigation or moved controls: Tasks 3–6. Diagnostics-backed sync-reference/freshness, calibration-applied, and diagnostic-age details on the main Speakers page are explicitly owned by Subproject 3 Task 3.
- Typed, bounded, generation-safe commands and raw-error localization: Tasks 1, 2, and 7.
- Diagnostics views, registry, copy/export, 4 Hz polling, calibration, and final release matrix remain exclusively in Subproject 3.
