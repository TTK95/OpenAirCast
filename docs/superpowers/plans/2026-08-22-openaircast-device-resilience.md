# OpenAirCast Device Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the UI-independent device backend that continuously discovers AirPlay receivers, persists desired and saved groups, applies staged membership atomically, isolates per-receiver failures, and recovers capture, network, and Windows suspend/resume state behind the control-center shell contract.

**Architecture:** A single Tokio-backed device actor owns all mutable device-domain state and publishes immutable `DeviceSnapshot` revisions through a watch channel. Discovery, capture, persistence, connection, and recovery supervisors report generation-tagged results through bounded channels; the subproject 1 reducer/effect executor is the only shell integration point and preserves `AppHandle`/`UiSnapshot` semantics.

**Tech Stack:** Rust 2021, Tokio 1.x, tokio-util cancellation tokens, futures, async-trait, serde/serde_json, uuid, crossbeam-channel, mdns-sd 0.11, wasapi 0.23, windows-sys 0.59, existing AirPlay crates, egui/eframe `=0.36.1` shell.

**Spec:** `docs/superpowers/specs/2026-08-22-openaircast-device-resilience-design.md`

## Global Constraints

- Execute this plan only on a revision containing the completed subproject 1 shell from `docs/superpowers/specs/2026-08-22-openaircast-control-center-shell-design.md`; the current planning checkout still contains the pre-shell tray implementation.
- Preserve `AppHandle::dispatch`, `AppHandle::snapshot`, `AppHandle::drain_ui_effects`, and `AppHandle::install_waker` exactly; views, tray, and hotkey never receive `DeviceBackendHandle`.
- Keep `DeviceSnapshot` internal to the shell reducer/effect executor; no AirPlay `Device`, `Connection`, socket, key, Tokio handle, mutable collection, or raw error chain crosses into UI code.
- Keep egui/eframe pinned to `=0.36.1` with `wgpu` and AccessKit, and retain the single self-contained Windows executable and existing GPL-2.0 licensing.
- Identify receivers only by validated `ReceiverId([u8; 6])`; persist exactly twelve uppercase hexadecimal characters and never persist discovery indices, IP addresses, or display names as identity.
- The shell owns staged membership. Checkbox changes perform no backend operation; **Apply speaker changes** sends one complete `SetDesiredMembers` command and **Discard** sends none.
- Keep staged, desired, and active membership distinct. Offline desired members remain desired; only confirmed transport results populate active membership.
- Start with persisted desired membership restored. Auto-connect defaults to false; without explicit auto-connect, every process launch starts with `RunIntent::Stopped`.
- Keep one capture, one live PCM timeline, one ALAC encoder, and receiver-specific RTP encryption/sockets. Do not implement seamless target insertion.
- Treat membership changes, saved-group activation with different membership, secondary reconnect/rejoin, rebuilding without a failed immutable target, primary replacement, active receiver or local-interface address change, one-to-many timing transition, stream-format/timing/SETUP-latency change, enabled latency-preset change, resume, and dead RTSP recovery as announced full-session restarts.
- Master volume, mute, receiver level, saved-group edits, display metadata, and normalized endpoint recovery do not restart the AirPlay session.
- Use fixed queue capacities: commands 64, backend events 256, diagnostics 2,048, supervisor events 256 per supervisor, live PCM 32 frames, and one latest watch snapshot.
- Supervisor producers use non-blocking `try_send`; a full lifecycle queue emits a non-blocking queue diagnostic, cancels that generation, and resynchronizes from a fresh complete supervisor state instead of dropping an unreported state edge or waiting indefinitely.
- Use explicit cancellation and owned join handles. Per-receiver teardown is limited to two seconds and total group teardown to four seconds; no process exit or runtime timeout is a normal lifecycle mechanism.
- Persist device state only at `%APPDATA%\OpenAirCast\state-v1.json`; shell-only settings remain in `%APPDATA%\OpenAirCast\settings.json`.
- Migrate legacy `%APPDATA%\HomePodCast\volume.txt` once without deleting it. Do not copy pairing identities, hotkeys, window state, secrets, or protocol payloads into device state.
- `Normal` is the only initially enabled latency preset and preserves the validated 2,000 ms sender buffer plus current 200 ms render lead. `Low` and `Stable` remain disabled until their hardware gate passes.
- Hardware-independent tests must run without a receiver, audio device, network transition, interactive window, or administrator rights.
- Make each checklist step one focused 2–5 minute action. Run the named focused test after each implementation slice and commit only after its task-wide verification passes.

## File Structure and Dependency Order

Create or modify these units in this order:

| Unit | Responsibility | Primary files |
|---|---|---|
| Domain contract | Stable IDs, volumes, settings, lifecycle, snapshots | `homepod-cast/src/backend/model.rs`, `lib.rs` |
| Transport contract | Bounded commands, shell events, diagnostic feed | `backend/command.rs`, `backend/event.rs` |
| Persistence | Versioned state, migration, atomic replace | `backend/persistence.rs` |
| Recovery policy | Fake-clock backoff, discovery stability, restart limits | `backend/recovery.rs` |
| Discovery | Fallible stream, dual-service cache, supervisor | `airplay-discovery/src/{traits,browser}.rs`, `backend/discovery.rs` |
| Live PCM | Replace-oldest queue and supervised silence bridge | `airplay-audio/src/live_decoder.rs`, `backend/capture.rs` |
| AirPlay lifecycle | Cancellable workers, member reports, best-effort setup | `airplay-client/src/{connection,group,client,events}.rs` |
| Session reconciliation | One/many transport adapter and restart classification | `backend/session.rs` |
| Device actor | Generations, command coalescing, snapshot publication | `backend/controller.rs`, `backend/mod.rs`, `lib.rs` |
| Platform recovery | Interface and suspend/resume events | `backend/network.rs`, shell platform modules |
| Shell integration | Effect adapter, staged UX, groups/audio/settings controls | subproject 1 `app`, `ui`, `tray`, `platform` modules |
| Acceptance | End-to-end fakes, leak regression, Windows hardware record | integration tests, `README.md`, `docs/testing/device-resilience-hardware.md` |

---

### Task 1: Domain Value Types and Device Snapshot

**Files:**
- Create: `crates/homepod-cast/src/lib.rs`
- Create: `crates/homepod-cast/src/backend/mod.rs`
- Create: `crates/homepod-cast/src/backend/model.rs`
- Create: `crates/homepod-cast/tests/support/mod.rs`
- Modify: `crates/homepod-cast/Cargo.toml`
- Test: `crates/homepod-cast/src/backend/model.rs`

**Interfaces:**
- Consumes: `airplay_core::DeviceId`, subproject 1's presentation-safe snapshot rule.
- Produces: `ReceiverId`, `Volume`, `SavedGroupId`, `SavedGroup`, `SavedGroupMember`, `RunIntent`, `ReceiverLifecycle`, `ReceiverRole`, `SetupPhase`, `SessionPhase`, `DeviceSnapshot`, `AudioEndpointPreference`, `LatencyPreset`, and `LatencyConfig`.

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReceiverId([u8; 6]);
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume(f32);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SavedGroupId(uuid::Uuid);

pub enum AudioEndpointPreference {
    SystemDefault,
    Explicit { id: String, last_known_name: String },
}

pub enum LatencyPreset { Low, Normal, Stable }

pub struct LatencyConfig {
    pub sender_buffer_ms: u32,
    pub render_delay_ms: u32,
}

impl LatencyPreset {
    pub fn enabled_config(self) -> Option<LatencyConfig>;
}
```

- [ ] **Step 1: Add serialization and cancellation dependencies**

Add workspace-backed `serde`, `serde_json`, `uuid`, `async-trait`, `futures`, `tokio-stream`, `crossbeam-channel = "0.5"`, and `tokio-util = { version = "0.7", features = ["rt"] }` to `homepod-cast`; add `tempfile = "3"` under dev-dependencies.

```toml
serde.workspace = true
serde_json = "1.0"
uuid.workspace = true
async-trait.workspace = true
futures.workspace = true
tokio-stream.workspace = true
crossbeam-channel = "0.5"
tokio-util = { version = "0.7", features = ["rt"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write failing identity and volume tests**

```rust
#[test]
fn receiver_id_has_stable_storage_and_display_forms() {
    let id = ReceiverId::from_storage_key("5855CA1AE288").unwrap();
    assert_eq!(id.storage_key(), "5855CA1AE288");
    assert_eq!(id.to_string(), "58:55:CA:1A:E2:88");
    assert!(ReceiverId::from_storage_key("58:55:CA:1A:E2:88").is_err());
    assert!(ReceiverId::from_storage_key("5855CA1AE28Z").is_err());
}

#[test]
fn volume_rejects_non_finite_and_out_of_range_values() {
    assert_eq!(Volume::new(0.25).unwrap().get(), 0.25);
    assert!(Volume::new(f32::NAN).is_err());
    assert!(Volume::new(-0.01).is_err());
    assert!(Volume::new(1.01).is_err());
}
```

- [ ] **Step 3: Run the focused model tests to establish red state**

Run: `cargo test -p homepod-cast backend::model::tests --lib`

Expected: compilation fails because `ReceiverId` and `Volume` are not defined.

- [ ] **Step 4: Implement validated `ReceiverId` and custom serde**

```rust
impl ReceiverId {
    pub fn from_storage_key(value: &str) -> Result<Self, ModelError> {
        if value.len() != 12 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ModelError::InvalidReceiverId(value.to_owned()));
        }
        let mut bytes = [0_u8; 6];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| ModelError::InvalidReceiverId(value.to_owned()))?;
        }
        Ok(Self(bytes))
    }

    pub fn storage_key(self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }
}

impl From<airplay_core::DeviceId> for ReceiverId {
    fn from(value: airplay_core::DeviceId) -> Self { Self(value.0) }
}

impl From<ReceiverId> for airplay_core::DeviceId {
    fn from(value: ReceiverId) -> Self { Self(value.0) }
}
```

Implement `Serialize` as `storage_key()` and `Deserialize` through `from_storage_key`; implement `Display` as six uppercase colon-separated bytes.

- [ ] **Step 5: Implement `Volume`, endpoint preference, and latency catalog**

```rust
impl Volume {
    pub const DEFAULT_MASTER: Self = Self(0.25);
    pub const UNITY: Self = Self(1.0);
    pub const MUTED: Self = Self(0.0);

    pub fn new(value: f32) -> Result<Self, ModelError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ModelError::InvalidVolume)
        }
    }

    pub fn get(self) -> f32 { self.0 }
}

impl LatencyPreset {
    pub fn enabled_config(self) -> Option<LatencyConfig> {
        match self {
            Self::Normal => Some(LatencyConfig {
                sender_buffer_ms: 2_000,
                render_delay_ms: 200,
            }),
            Self::Low | Self::Stable => None,
        }
    }
}
```

Implement custom `Serialize`/`Deserialize` for `Volume` so deserialization always calls `Volume::new`; deriving transparent deserialization is forbidden because it could construct NaN, infinity, or an out-of-range value.

- [ ] **Step 6: Add lifecycle and snapshot structs exactly matching the spec**

Define `ReceiverLifecycle`, `ReceiverRole`, `SetupPhase`, `SessionPhase`, `DiscoverySnapshot`, `ReceiverSnapshot`, `SavedGroupSnapshot`, `SessionSnapshot`, `AudioSourceSnapshot`, `PersistenceSnapshot`, `UserFacingError`, and `DeviceSnapshot`. Use `BTreeSet<ReceiverId>` for desired/active/failed/retry sets and `Arc<[T]>` only at the shell mapping layer; backend snapshots own sorted `Vec<T>`.

```rust
pub enum ReceiverLifecycle {
    Discovered,
    Connecting { attempt: u32 },
    SettingUp { role: ReceiverRole, phase: SetupPhase },
    Ready { role: ReceiverRole },
    Streaming { role: ReceiverRole },
    RetryWaiting { attempt: u32, retry_at: SystemTime },
    Unavailable,
    Failed { retryable: bool, error: UserFacingError },
}

pub enum ReceiverRole { Single, Primary, Secondary }

pub enum SessionPhase {
    Stopped,
    Starting { generation: u64 },
    Streaming { generation: u64 },
    Degraded { generation: u64 },
    Restarting { generation: u64, reason: RestartReason },
    Stopping { generation: u64 },
    Failed { generation: u64, error: UserFacingError },
}

pub struct DeviceSnapshot {
    pub revision: u64,
    pub desired_revision: u64,
    pub discovery: DiscoverySnapshot,
    pub receivers: Vec<ReceiverSnapshot>,
    pub saved_groups: Vec<SavedGroupSnapshot>,
    pub desired_members: BTreeSet<ReceiverId>,
    pub run_intent: RunIntent,
    pub session: SessionSnapshot,
    pub audio_source: AudioSourceSnapshot,
    pub master_volume: Volume,
    pub muted: bool,
    pub latency_preset: LatencyPreset,
    pub auto_connect: bool,
    pub persistence: PersistenceSnapshot,
}
```

`SessionSnapshot` stores desired, active, failed, and retry-waiting sets, primary, and `AudioFlow::{Live,SilenceBridged}`. Derive `Streaming` only when all desired members are active; derive `Degraded` when at least one receiver remains active but a desired receiver or capture is unavailable; derive `Failed` only while Running with no active receiver after best-effort recovery.

- [ ] **Step 7: Test deterministic receiver and group ordering**

```rust
#[test]
fn snapshot_lists_are_sorted_for_rendering() {
    let mut snapshot = DeviceSnapshot::default();
    snapshot.receivers = vec![receiver_snapshot(2, "wohnzimmer"), receiver_snapshot(1, "Büro")];
    snapshot.saved_groups = vec![saved_group_snapshot(2, "Unten"), saved_group_snapshot(1, "Abends")];
    snapshot.sort_for_publication();
    assert_eq!(snapshot.receivers.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Büro", "wohnzimmer"]);
    assert_eq!(snapshot.saved_groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), vec!["Abends", "Unten"]);
}
```

Implement the two constructors used above in `tests/support/mod.rs` with fixed `ReceiverId`/`SavedGroupId` inputs, and compare case-folded name then stable ID so equal names never depend on discovery order.

- [ ] **Step 8: Run model tests and crate check**

Run: `cargo test -p homepod-cast backend::model::tests --lib`

Expected: all model tests pass.

Run: `cargo check -p homepod-cast --all-targets`

Expected: success with no new warnings.

- [ ] **Step 9: Commit the domain contract**

```powershell
git add crates/homepod-cast/Cargo.toml Cargo.lock crates/homepod-cast/src/lib.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/src/backend/model.rs crates/homepod-cast/tests/support/mod.rs
git commit -m "feat: define resilient device backend model"
```

### Task 2: Bounded Commands, Backend Events, and Diagnostic Feed

**Files:**
- Create: `crates/homepod-cast/src/backend/command.rs`
- Create: `crates/homepod-cast/src/backend/event.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/src/lib.rs`
- Test: `crates/homepod-cast/src/backend/command.rs`
- Test: `crates/homepod-cast/src/backend/event.rs`

**Interfaces:**
- Consumes: Task 1 domain types.
- Produces: `BackendCommand`, `CommandEnvelope`, `DeviceBackendHandle`, `DeviceBackendUpdates`, `BackendEvent`, `DiagnosticEvent`, `DiagnosticPayload`, `DiagnosticEventReceiver`, and queue constants.

```rust
pub const COMMAND_CAPACITY: usize = 64;
pub const BACKEND_EVENT_CAPACITY: usize = 256;
pub const DIAGNOSTIC_CAPACITY: usize = 2_048;
pub const SUPERVISOR_CAPACITY: usize = 256;
pub const PCM_CAPACITY: usize = 32;

pub struct DeviceBackendUpdates {
    pub state: tokio::sync::watch::Receiver<Arc<DeviceSnapshot>>,
    pub events: tokio::sync::broadcast::Receiver<BackendEvent>,
    pub diagnostics: DiagnosticEventReceiver,
}

impl DeviceBackendHandle {
    pub fn try_send(&self, command: BackendCommand) -> Result<u64, BackendSendError>;
    pub async fn send(&self, command: BackendCommand) -> Result<u64, BackendSendError>;
}
```

- [ ] **Step 1: Write failing bounded-command tests**

```rust
#[test]
fn try_send_reports_full_and_closed_without_blocking() {
    let (handle, mut rx) = DeviceBackendHandle::test_channel(1);
    let first = handle.try_send(BackendCommand::SetRunIntent(RunIntent::Running)).unwrap();
    assert_eq!(first, 1);
    assert_eq!(handle.try_send(BackendCommand::SetRunIntent(RunIntent::Stopped)), Err(BackendSendError::Busy));
    assert_eq!(rx.blocking_recv().unwrap().id, 1);
    drop(rx);
    assert_eq!(handle.try_send(BackendCommand::Shutdown), Err(BackendSendError::Closed));
}
```

- [ ] **Step 2: Run command tests to verify the missing contract**

Run: `cargo test -p homepod-cast backend::command::tests --lib`

Expected: compilation fails because `DeviceBackendHandle` is absent.

- [ ] **Step 3: Implement the exact command enum and handle**

```rust
#[derive(Debug)]
pub struct CommandEnvelope {
    pub id: u64,
    pub command: BackendCommand,
}

#[derive(Clone)]
pub struct DeviceBackendHandle {
    command_tx: tokio::sync::mpsc::Sender<CommandEnvelope>,
    next_id: Arc<AtomicU64>,
}

impl DeviceBackendHandle {
    pub fn try_send(&self, command: BackendCommand) -> Result<u64, BackendSendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.command_tx.try_send(CommandEnvelope { id, command }).map_err(|error| {
            match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => BackendSendError::Busy,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => BackendSendError::Closed,
            }
        })?;
        Ok(id)
    }
}
```

Add the exact command surface; do not add transport-shaped commands:

```rust
pub enum BackendCommand {
    SetDesiredMembers { members: BTreeSet<ReceiverId> },
    SetRunIntent(RunIntent),
    SetMasterVolume(Volume),
    SetMuted(bool),
    SetReceiverLevel { receiver: ReceiverId, level: Volume },
    SetAudioEndpoint(AudioEndpointPreference),
    SetLatencyPreset(LatencyPreset),
    SetAutoConnect(bool),
    SaveGroup {
        id: Option<SavedGroupId>,
        name: String,
        members: Vec<SavedGroupMember>,
    },
    DeleteGroup(SavedGroupId),
    ActivateSavedGroup { id: SavedGroupId, start: bool },
    RetryReceiver(ReceiverId),
    NotifySystem(SystemEvent),
    Shutdown,
}

pub enum SystemEvent { Suspending, Resumed, NetworkChanged }

impl BackendCommand {
    pub(crate) fn is_edge(&self) -> bool {
        matches!(self,
            Self::SaveGroup { .. } | Self::DeleteGroup(_) | Self::ActivateSavedGroup { .. }
            | Self::RetryReceiver(_) | Self::NotifySystem(_) | Self::Shutdown)
    }
}
```

- [ ] **Step 4: Write failing diagnostic redaction and lag tests**

```rust
#[tokio::test]
async fn diagnostics_are_bounded_and_resubscribable() {
    let (sender, receiver) = diagnostic_channel(2);
    sender.send(diagnostic("one")).unwrap();
    let mut copy = receiver.resubscribe();
    sender.send(diagnostic("two")).unwrap();
    assert_eq!(copy.recv().await.unwrap().payload, DiagnosticPayload::Message("two".into()));
}

#[tokio::test]
async fn diagnostic_lag_becomes_a_typed_loss_event() {
    let (sender, mut receiver) = diagnostic_channel(2);
    for value in 0..3 { sender.send(diagnostic(value.to_string())).unwrap(); }
    assert!(matches!(receiver.recv().await.unwrap().payload, DiagnosticPayload::QueueLag { dropped: 1 }));
}

#[test]
fn diagnostic_debug_output_contains_no_secret_fields() {
    let event = diagnostic_for_receiver(receiver_id(1), DiagnosticPayload::Probe { healthy: false, latency_ms: 2_000 });
    let rendered = format!("{event:?}");
    assert!(!rendered.contains("pin"));
    assert!(!rendered.contains("key"));
    assert!(!rendered.contains("payload_bytes"));
}
```

- [ ] **Step 5: Implement shell and diagnostic event types**

```rust
pub struct DiagnosticEvent {
    pub monotonic_ns: u64,
    pub wall_time: SystemTime,
    pub session_generation: u64,
    pub discovery_generation: u64,
    pub capture_generation: u64,
    pub receiver: Option<ReceiverId>,
    pub severity: Severity,
    pub category: DiagnosticCategory,
    pub payload: DiagnosticPayload,
}

pub struct DiagnosticEventReceiver {
    inner: tokio::sync::broadcast::Receiver<DiagnosticEvent>,
}

impl DiagnosticEventReceiver {
    pub async fn recv(&mut self) -> Result<DiagnosticEvent, DiagnosticClosed> {
        match self.inner.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(dropped)) => Ok(DiagnosticEvent::queue_lag(dropped)),
            Err(broadcast::error::RecvError::Closed) => Err(DiagnosticClosed),
        }
    }
    pub fn try_recv(&mut self) -> Result<DiagnosticEvent, DiagnosticTryRecvError>;
    pub fn resubscribe(&self) -> Self { Self { inner: self.inner.resubscribe() } }
}
```

Implement `try_recv` with the same typed `QueueLag { dropped }` conversion plus only `Empty`/`Closed` wrapper errors. The synthetic lag event uses the receiver's current wall/monotonic time, zero generation values, no receiver ID, warning severity, and `DiagnosticCategory::Queue`.

Define the low-volume shell edge exactly:

```rust
pub enum BackendEvent {
    CommandCompleted { id: u64 },
    CommandFailed { id: u64, error: UserFacingError },
    Notice {
        severity: Severity,
        code: NoticeCode,
        scope: ErrorScope,
        message: String,
    },
}
```

Define `DiagnosticPayload` with these exhaustive typed variants: `Message`, `DiscoveryGeneration`, `DiscoveryReceiver`, `CommandResult`, `CommandCoalesced`, `ReceiverTransition`, `SetupPhaseDuration`, `SessionTransition`, `SessionRestart`, `Retry`, `CaptureTransition`, `PcmDrop`, `SilenceBridge`, `Probe`, `Persistence`, `QueueWatermark`, `QueueLag`, and `WorkerShutdown`. `Message` accepts only a pre-redacted user-safe summary. Every variant stores only stable IDs, enum states, counts, durations, retry attempt/deadline, resolved latency numbers, and redacted error summaries. Implement `DiagnosticPayload::kind() -> DiagnosticKind` for Subproject 3/tests; raw pairing material, protocol payload bytes, credentials, and Windows user paths have no fields in these types.

- [ ] **Step 6: Run the focused command/event tests**

Run: `cargo test -p homepod-cast backend::command::tests backend::event::tests --lib`

Expected: all command and event tests pass.

- [ ] **Step 7: Commit the bounded public contract**

```powershell
git add crates/homepod-cast/src/backend/command.rs crates/homepod-cast/src/backend/event.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/src/lib.rs
git commit -m "feat: add bounded device backend contract"
```

### Task 3: Versioned Device-State Persistence and Legacy Migration

**Files:**
- Create: `crates/homepod-cast/src/backend/persistence.rs`
- Create: `crates/homepod-cast/tests/persistence.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/Cargo.toml`

**Interfaces:**
- Consumes: Task 1 persistence-safe model types and `%APPDATA%` layout.
- Produces: `PersistedStateV1`, `StateStore`, `JsonStateStore`, `LoadOutcome`, `PersistError`, `effective_volume`.

```rust
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load(&self) -> Result<LoadOutcome, PersistError>;
    async fn save(&self, state: &PersistedStateV1) -> Result<(), PersistError>;
}

pub fn effective_volume(master: Volume, level: Volume, muted: bool) -> Volume;
```

- [ ] **Step 1: Write failing schema and effective-volume tests**

```rust
#[test]
fn schema_round_trip_includes_all_device_preferences() {
    let state = sample_state();
    let json = serde_json::to_string_pretty(&state).unwrap();
    let decoded: PersistedStateV1 = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, state);
    assert!(json.contains("\"version\": 1"));
    assert!(json.contains("\"audio_endpoint\""));
    assert!(json.contains("\"auto_connect\""));
}

#[test]
fn mute_preserves_preferences_and_applies_zero() {
    assert_eq!(effective_volume(Volume::new(0.5).unwrap(), Volume::new(0.4).unwrap(), false).get(), 0.2);
    assert_eq!(effective_volume(Volume::new(0.5).unwrap(), Volume::new(0.4).unwrap(), true), Volume::MUTED);
}
```

- [ ] **Step 2: Run persistence tests to establish red state**

Run: `cargo test -p homepod-cast --test persistence schema_round_trip_includes_all_device_preferences`

Expected: compilation fails because `PersistedStateV1` is absent.

- [ ] **Step 3: Implement and validate schema version 1**

```rust
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PersistedStateV1 {
    pub version: u32,
    pub master_volume: Volume,
    pub muted: bool,
    pub receiver_levels: BTreeMap<ReceiverId, Volume>,
    pub saved_groups: Vec<SavedGroup>,
    pub last_desired_members: BTreeSet<ReceiverId>,
    pub audio_endpoint: AudioEndpointPreference,
    pub latency_preset: LatencyPreset,
    pub auto_connect: bool,
}
```

Validate `version == 1`, group names after trim, case-insensitive uniqueness, non-empty member lists, unique receiver IDs per group, and enabled latency preset. Defaults are master `0.25`, unmuted, no group, system default endpoint, Normal latency, and auto-connect false.

- [ ] **Step 4: Write failing atomic-replace and corruption tests**

```rust
#[tokio::test]
async fn failed_replace_leaves_previous_json_valid() {
    let dir = tempfile::tempdir().unwrap();
    let working_store = JsonStateStore::for_test_default(dir.path());
    working_store.save(&sample_state()).await.unwrap();
    let store = JsonStateStore::for_test(dir.path(), FailingReplacer::after_temp_sync());
    let mut changed = sample_state();
    changed.muted = true;
    assert!(store.save(&changed).await.is_err());
    let on_disk: PersistedStateV1 = serde_json::from_slice(&std::fs::read(store.state_path()).unwrap()).unwrap();
    assert!(!on_disk.muted);
}

#[tokio::test]
async fn corrupt_json_is_renamed_and_defaults_are_returned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state-v1.json"), b"not-json").unwrap();
    let outcome = JsonStateStore::for_test_default(dir.path()).load().await.unwrap();
    assert!(matches!(outcome, LoadOutcome::RecoveredCorrupt { .. }));
    assert_eq!(dir.path().read_dir().unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).count(), 1);
}
```

- [ ] **Step 5: Implement same-directory temp write and Windows atomic replacement**

Enable `windows-sys` feature `Win32_Storage_FileSystem`. Write JSON to `state-v1.json.tmp`, call `File::sync_all`, and replace with `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` on Windows. On replace failure remove only that exact temp file and leave the prior destination untouched. The test replacer performs `std::fs::rename` after removing only the exact destination file inside the temporary test directory.

```rust
#[async_trait]
impl StateStore for JsonStateStore {
    async fn save(&self, state: &PersistedStateV1) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(state)?;
        let state_path = self.state_path.clone();
        let replacer = self.replacer.clone();
        tokio::task::spawn_blocking(move || {
            let temp = state_path.with_extension("json.tmp");
            let mut file = OpenOptions::new().create(true).truncate(true).write(true).open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            replacer.replace(&temp, &state_path)
        }).await.map_err(PersistError::Join)?
    }
}
```

Run `load`, corrupt-file quarantine, and legacy import through the same `spawn_blocking` boundary; no filesystem call runs on the controller's async executor thread.

- [ ] **Step 6: Write and implement legacy volume migration**

```rust
#[tokio::test]
async fn legacy_volume_migrates_once_without_deleting_source() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("HomePodCast").join("volume.txt");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "0.375").unwrap();
    let store = JsonStateStore::for_test_paths(dir.path().join("OpenAirCast"), legacy.clone());
    let first = store.load().await.unwrap().into_state();
    assert_eq!(first.master_volume.get(), 0.375);
    assert!(legacy.exists());
    let second = store.load().await.unwrap().into_state();
    assert_eq!(second.master_volume.get(), 0.375);
}
```

Remove `load_volume`/`save_volume` ownership from `cast.rs`; keep hotkey migration untouched because it belongs to the shell repository.

- [ ] **Step 7: Test malformed values and secret exclusion**

```rust
#[test]
fn serialized_state_contains_no_shell_or_protocol_state() {
    let json = serde_json::to_string(&sample_state()).unwrap();
    for forbidden in ["hotkey", "window", "theme", "ip_address", "pin", "ed25519", "pairing"] {
        assert!(!json.contains(forbidden), "unexpected field {forbidden}");
    }
}
```

- [ ] **Step 8: Run all persistence tests**

Run: `cargo test -p homepod-cast --test persistence`

Expected: schema, validation, corruption recovery, atomic replacement, legacy migration, and volume tests pass.

- [ ] **Step 9: Commit persistence**

```powershell
git add crates/homepod-cast/src/backend/persistence.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/Cargo.toml crates/homepod-cast/tests/persistence.rs
git commit -m "feat: persist resilient device state"
```

### Task 4: Deterministic Backoff and Restart Policy

**Files:**
- Create: `crates/homepod-cast/src/backend/recovery.rs`
- Create: `crates/homepod-cast/tests/recovery.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`

**Interfaces:**
- Consumes: `ReceiverId`, monotonic fake clock.
- Produces: `RetryPolicy`, `RetryState`, `RejoinRestartLimiter`, `Clock`.

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct RetryPolicy {
    pub base: [Duration; 6],
    pub jitter_percent: u8,
    pub healthy_reset_after: Duration,
    pub discovery_stable_for: Duration,
}
```

- [ ] **Step 1: Write failing deterministic-delay tests**

```rust
#[test]
fn retry_schedule_caps_at_thirty_seconds_and_is_deterministic() {
    let policy = RetryPolicy::default();
    let id = receiver_id(7);
    let first = (0..8).map(|attempt| policy.delay(id, attempt)).collect::<Vec<_>>();
    let second = (0..8).map(|attempt| policy.delay(id, attempt)).collect::<Vec<_>>();
    assert_eq!(first, second);
    assert!(first[0] >= Duration::from_millis(800) && first[0] <= Duration::from_millis(1_200));
    assert!(first[7] >= Duration::from_secs(24) && first[7] <= Duration::from_secs(36));
}
```

- [ ] **Step 2: Run recovery tests to establish red state**

Run: `cargo test -p homepod-cast --test recovery retry_schedule_caps_at_thirty_seconds_and_is_deterministic`

Expected: compilation fails because `RetryPolicy` is absent.

- [ ] **Step 3: Implement delay sequence and stable jitter**

```rust
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: [1, 2, 4, 8, 16, 30].map(Duration::from_secs),
            jitter_percent: 20,
            healthy_reset_after: Duration::from_secs(60),
            discovery_stable_for: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    pub fn delay(&self, receiver: ReceiverId, attempt: u32) -> Duration {
        let base = self.base[usize::min(attempt as usize, self.base.len() - 1)];
        let hash = receiver.storage_key().bytes().fold(attempt as u64, |value, byte| value.wrapping_mul(1099511628211).wrapping_add(byte as u64));
        let signed = (hash % 41) as i64 - 20;
        base.mul_f64(1.0 + signed as f64 / 100.0)
    }

    pub fn discovery_delay(&self, generation: u64, attempt: u32) -> Duration {
        let base = self.base[usize::min(attempt as usize, self.base.len() - 1)];
        let hash = generation.to_le_bytes().into_iter().fold(attempt as u64, |value, byte| value.wrapping_mul(1099511628211).wrapping_add(byte as u64));
        let signed = (hash % 41) as i64 - 20;
        base.mul_f64(1.0 + signed as f64 / 100.0)
    }
}
```

Test `discovery_delay` twice with the same generation/attempt and assert equality plus the same ±20% bounds.

- [ ] **Step 4: Add fake-clock tests for stability and rate limiting**

```rust
#[test]
fn rejoin_requires_two_seconds_online_and_one_restart_per_thirty_seconds() {
    let clock = FakeClock::at_zero();
    let mut limiter = RejoinRestartLimiter::new(clock.clone());
    limiter.note_discovered(receiver_id(2));
    clock.advance(Duration::from_millis(1_999));
    assert!(!limiter.may_rejoin(receiver_id(2)));
    clock.advance(Duration::from_millis(1));
    assert!(limiter.may_rejoin(receiver_id(2)));
    limiter.note_restart();
    assert!(!limiter.may_restart_group());
    clock.advance(Duration::from_secs(30));
    assert!(limiter.may_restart_group());
}
```

- [ ] **Step 5: Implement healthy reset and manual retry**

`RetryState::note_streaming(now)` resets attempts only after 60 continuous seconds. `RetryState::manual_retry()` clears the deadline for the named receiver without changing desired membership or bypassing primary/secondary reconciliation.

```rust
pub fn manual_retry(&mut self, receiver: ReceiverId) {
    if let Some(entry) = self.receivers.get_mut(&receiver) {
        entry.next_retry = None;
    }
}
```

- [ ] **Step 6: Run recovery suite and commit**

Run: `cargo test -p homepod-cast --test recovery`

Expected: deterministic jitter, cap, healthy reset, discovery stability, manual retry, and restart limiter tests pass.

```powershell
git add crates/homepod-cast/src/backend/recovery.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/tests/recovery.rs
git commit -m "feat: add deterministic recovery policy"
```

### Task 5: Fallible Independent Discovery Streams

**Files:**
- Modify: `crates/airplay-discovery/src/traits.rs`
- Modify: `crates/airplay-discovery/src/lib.rs`
- Modify: `crates/airplay-discovery/src/browser.rs`
- Modify: `crates/airplay-discovery/examples/debug_devices.rs`
- Test: `crates/airplay-discovery/src/traits.rs`
- Test: `crates/airplay-discovery/src/browser.rs`

**Interfaces:**
- Consumes: existing `BrowseEvent`, `DiscoveryError`, `mdns_sd::Receiver::recv_async`.
- Produces: `BrowseStream`, fallible `Discovery::browse`, browse-owned registration lifecycle.

```rust
pub type BrowseStream = Pin<Box<dyn Stream<Item = Result<BrowseEvent, DiscoveryError>> + Send>>;

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn browse(&self) -> Result<BrowseStream>;
    async fn scan(&self, timeout: Duration) -> Result<Vec<Device>>;
    async fn get_device(&self, id: &DeviceId) -> Option<Device>;
    async fn get_all_devices(&self) -> Vec<Device>;
}
```

- [ ] **Step 1: Change mock tests to require a fallible stream**

```rust
#[tokio::test]
async fn mock_browse_can_surface_daemon_failure() {
    let mut mock = MockDiscovery::new();
    mock.expect_browse().return_once(|| {
        let error = DiscoveryError::Daemon("closed".into());
        Box::pin(async move { Ok(Box::pin(tokio_stream::iter([Err(error)])) as BrowseStream) })
    });
    let mut stream = mock.browse().await.unwrap();
    assert!(stream.next().await.unwrap().is_err());
}
```

- [ ] **Step 2: Run discovery trait tests to verify red state**

Run: `cargo test -p airplay-discovery traits::tests --lib`

Expected: type mismatch because browse items are currently plain `BrowseEvent`.

- [ ] **Step 3: Introduce `BrowseStream` and update consumers**

Remove `Unpin` from the public trait object and pin at call sites. Update `debug_devices` to handle `Ok(BrowseEvent)` and print an error before ending on `Err`.

```rust
while let Some(item) = stream.next().await {
    match item {
        Ok(event) => println!("{event:?}"),
        Err(error) => {
            eprintln!("discovery stopped: {error}");
            break;
        }
    }
}
```

- [ ] **Step 4: Give each browse/scan operation its own daemon registration guard**

Create a fresh `ServiceDaemon` inside `browse()`. Capture a `BrowseRegistration` in the async stream; its `Drop` calls `stop_browse` for both service types and `shutdown`. Implement `scan()` by consuming its own `browse()` stream under `tokio::time::timeout`, so it cannot stop a concurrent browse.

```rust
struct BrowseRegistration {
    daemon: ServiceDaemon,
}

impl Drop for BrowseRegistration {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(AIRPLAY_SERVICE_TYPE);
        let _ = self.daemon.stop_browse(RAOP_SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
```

- [ ] **Step 5: Replace blocking receive calls with async selection**

```rust
let stream = async_stream::stream! {
    let _registration = registration;
    loop {
        let result = tokio::select! {
            event = airplay_receiver.recv_async() => event.map(|value| (ServiceKind::AirPlay, value)),
            event = raop_receiver.recv_async() => event.map(|value| (ServiceKind::Raop, value)),
        };
        match result {
            Ok((kind, event)) => {
                if let Some(event) = index.handle(kind, event).await { yield Ok(event); }
            }
            Err(error) => {
                yield Err(DiscoveryError::Daemon(error.to_string()));
                break;
            }
        }
    }
};
```

- [ ] **Step 6: Test scan/browse independence and stream drop**

Add a daemon factory seam used by tests. Hold one fake browse, run a fake scan, drop the scan, and assert only its registration stopped. Drop the browse and assert both of its service registrations stopped exactly once.

```rust
#[tokio::test]
async fn scan_drop_does_not_stop_existing_browse() {
    let factory = FakeDaemonFactory::new();
    let browser = ServiceBrowser::with_factory(factory.clone());
    let browse = browser.browse().await.unwrap();
    browser.scan(Duration::from_millis(1)).await.unwrap();
    assert_eq!(factory.active_registration_sets(), 1);
    drop(browse);
    assert_eq!(factory.active_registration_sets(), 0);
}
```

- [ ] **Step 7: Run discovery tests and commit**

Run: `cargo test -p airplay-discovery`

Expected: all trait, browser, and parser tests pass.

```powershell
git add crates/airplay-discovery/src/traits.rs crates/airplay-discovery/src/lib.rs crates/airplay-discovery/src/browser.rs crates/airplay-discovery/examples/debug_devices.rs
git commit -m "feat: make discovery streams fallible and independent"
```

### Task 6: Correct Dual-Service Receiver Bookkeeping

**Files:**
- Modify: `crates/airplay-discovery/src/browser.rs`
- Test: `crates/airplay-discovery/src/browser.rs`

**Interfaces:**
- Consumes: Task 5 `ServiceKind`, resolved full service names, `TxtRecordParser::merge_device_info`.
- Produces: private `ServiceKey`, `ReceiverServices`, `ServiceIndex::resolve`, `ServiceIndex::remove`.

```rust
struct ServiceKey { kind: ServiceKind, fullname: String }
struct ReceiverServices { airplay: Option<Device>, raop: Option<Device> }
struct ServiceIndex {
    service_to_receiver: HashMap<ServiceKey, DeviceId>,
    receivers: HashMap<DeviceId, ReceiverServices>,
}
```

- [ ] **Step 1: Write failing resolve/remove state-machine tests**

```rust
#[test]
fn removing_raop_keeps_present_airplay_receiver() {
    let mut index = ServiceIndex::default();
    index.resolve(ServiceKind::AirPlay, "Living._airplay._tcp.local.", airplay_device(1));
    index.resolve(ServiceKind::Raop, "AA@Living._raop._tcp.local.", raop_device(1));
    let event = index.remove(ServiceKind::Raop, "AA@Living._raop._tcp.local.").unwrap();
    assert!(matches!(event, BrowseEvent::Updated(ref device) if device.id == device_id(1)));
    assert!(index.is_castable(&device_id(1)));
}

#[test]
fn removing_airplay_marks_receiver_unavailable_even_if_raop_remains() {
    let mut index = ServiceIndex::default();
    index.resolve(ServiceKind::Raop, "AA@Living._raop._tcp.local.", raop_device(1));
    index.resolve(ServiceKind::AirPlay, "Living._airplay._tcp.local.", airplay_device(1));
    assert!(matches!(index.remove(ServiceKind::AirPlay, "Living._airplay._tcp.local."), Some(BrowseEvent::Removed(id)) if id == device_id(1)));
    assert!(!index.is_castable(&device_id(1)));
    assert!(index.contains(&device_id(1)));
}
```

- [ ] **Step 2: Run the focused browser tests to establish red state**

Run: `cargo test -p airplay-discovery browser::tests::service_index --lib`

Expected: compilation fails because `ServiceIndex` is absent.

- [ ] **Step 3: Implement keyed resolution and merge**

`resolve` records the `(kind, fullname) -> DeviceId` mapping before replacing that service slot. Emit `Added` when a receiver first becomes known and `Updated` for subsequent service/address/name changes. Prefer AirPlay fields, enrich with RAOP fields, and retain the latest service-specific records separately.

```rust
fn merged(services: &ReceiverServices) -> Option<Device> {
    match (&services.airplay, &services.raop) {
        (Some(airplay), Some(raop)) => Some(TxtRecordParser::merge_device_info(airplay, raop)),
        (Some(airplay), None) => Some(airplay.clone()),
        (None, Some(raop)) => Some(raop.clone()),
        (None, None) => None,
    }
}
```

- [ ] **Step 4: Implement service-specific removal**

Use the recorded full-name mapping for AirPlay removals instead of parsing the service name. Clearing RAOP emits `Updated` when AirPlay remains; clearing AirPlay emits `Removed` while retaining a RAOP-only stable cache record.

```rust
match key.kind {
    ServiceKind::Raop => services.raop = None,
    ServiceKind::AirPlay => services.airplay = None,
}
match (&services.airplay, key.kind) {
    (Some(_), _) => merged(services).map(BrowseEvent::Updated),
    (None, ServiceKind::AirPlay) => Some(BrowseEvent::Removed(receiver)),
    (None, ServiceKind::Raop) => None,
}
```

- [ ] **Step 5: Add address-update and duplicate-resolution tests**

```rust
#[test]
fn resolving_new_address_updates_same_receiver_id() {
    let mut index = ServiceIndex::default();
    index.resolve(ServiceKind::AirPlay, "Living._airplay._tcp.local.", airplay_device_at(1, [192, 168, 1, 2]));
    let event = index.resolve(ServiceKind::AirPlay, "Living._airplay._tcp.local.", airplay_device_at(1, [192, 168, 1, 44]));
    assert!(matches!(event, BrowseEvent::Updated(ref device) if device.addresses[0].to_string() == "192.168.1.44"));
    assert_eq!(index.receiver_count(), 1);
}
```

- [ ] **Step 6: Run browser and workspace discovery tests**

Run: `cargo test -p airplay-discovery browser::tests --lib`

Expected: all dual-service and registration tests pass.

Run: `cargo test -p airplay-discovery --all-targets`

Expected: library and example targets compile and pass.

- [ ] **Step 7: Commit dual-service discovery**

```powershell
git add crates/airplay-discovery/src/browser.rs
git commit -m "fix: track AirPlay and RAOP services independently"
```

### Task 7: Continuous Discovery Supervisor

**Files:**
- Create: `crates/homepod-cast/src/backend/discovery.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/tests/support/mod.rs`
- Test: `crates/homepod-cast/src/backend/discovery.rs`

**Interfaces:**
- Consumes: Task 4 `RetryPolicy`, Task 5 `BrowseStream`, Task 6 stable discovery events.
- Produces: `DiscoverySource`, `DiscoverySupervisor`, `DiscoveryControl`, and generation-tagged `SupervisorEvent::Discovery`.

```rust
#[async_trait]
pub trait DiscoverySource: Send + Sync {
    async fn browse(&self) -> Result<airplay_discovery::BrowseStream, UserFacingError>;
}

pub enum DiscoveryControl { RestartAfterNetworkChange, Shutdown }
pub enum DiscoveryUpdate {
    Started { generation: u64 },
    Receiver { generation: u64, event: BrowseEvent },
    Failed { generation: u64, error: UserFacingError, retry_at: Instant },
}
```

- [ ] **Step 1: Write failing supervisor retry and stale-generation tests**

```rust
#[tokio::test(start_paused = true)]
async fn daemon_failure_restarts_with_backoff() {
    let source = FakeDiscoverySource::sequence([Err(discovery_error("down")), Ok(stream_of([added(1)]))]);
    let mut harness = DiscoveryHarness::start(source);
    assert!(matches!(harness.recv().await, DiscoveryUpdate::Failed { generation: 1, .. }));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(harness.recv().await, DiscoveryUpdate::Started { generation: 2 }));
}

#[test]
fn old_generation_update_cannot_change_inventory() {
    let mut state = DiscoveryState::at_generation(2);
    assert!(!state.apply(DiscoveryUpdate::Receiver { generation: 1, event: added(1) }));
    assert!(state.receivers().is_empty());
}
```

- [ ] **Step 2: Run focused tests to establish red state**

Run: `cargo test -p homepod-cast backend::discovery::tests --lib`

Expected: compilation fails because the discovery supervisor is absent.

- [ ] **Step 3: Implement the cancellable supervisor loop**

Use a command watch channel for restart/shutdown and a bounded 256-event sender. Each daemon recreation increments generation and creates its own cancellation child token. Convert `DeviceId` to `ReceiverId`; filter castability with AirPlay 2 support and a usable IPv4 address while retaining non-castable records for display.

```rust
loop {
    generation += 1;
    match updates.try_send(DiscoveryUpdate::Started { generation }) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            diagnostics.send(queue_overload("discovery", generation));
            continue;
        }
        Err(mpsc::error::TrySendError::Closed(_)) => break,
    }
    match run_generation(source.as_ref(), generation, &updates, cancel.child_token()).await {
        GenerationExit::RestartNow => continue,
        GenerationExit::Retry(attempt) => clock.sleep(policy.discovery_delay(generation, attempt)).await,
        GenerationExit::Shutdown => break,
    }
}
```

Map `TrySendError::Full` to `GenerationExit::Overloaded`: emit a diagnostic high-water event, cancel the browse, and retry with a fresh generation after the actor drains. Map `Closed` to shutdown. Apply the same non-blocking producer rule to capture and session updates.

- [ ] **Step 4: Implement network restart semantics**

`RestartAfterNetworkChange` cancels the active browse and pending delay, sleeps exactly two seconds through the injected clock, clears generation-local availability to Unknown, and starts a fresh generation. It does not emit a session-stop command.

```rust
DiscoveryControl::RestartAfterNetworkChange => {
    generation_cancel.cancel();
    inventory.mark_all_unknown();
    clock.sleep(Duration::from_secs(2)).await;
    GenerationExit::RestartNow
}
```

- [ ] **Step 5: Test no-receiver versus discovery-error snapshots**

```rust
#[test]
fn empty_running_discovery_differs_from_failed_discovery() {
    assert!(matches!(DiscoverySnapshot::running_empty().phase, DiscoveryPhase::Running));
    assert!(matches!(DiscoverySnapshot::failed(user_error("mdns")).phase, DiscoveryPhase::Retrying { .. }));
}
```

- [ ] **Step 6: Run supervisor tests and commit**

Run: `cargo test -p homepod-cast backend::discovery::tests --lib`

Expected: restart, stale generation, castability, empty, failure, network settle, and shutdown tests pass.

```powershell
git add crates/homepod-cast/src/backend/discovery.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/tests/support/mod.rs
git commit -m "feat: supervise continuous receiver discovery"
```

### Task 8: Replace-Oldest Live PCM Delivery

**Files:**
- Modify: `crates/airplay-audio/src/live_decoder.rs`
- Modify: `crates/airplay-audio/src/lib.rs`
- Test: `crates/airplay-audio/src/live_decoder.rs`

**Interfaces:**
- Consumes: existing `LiveFrameSender::try_send` and crossbeam bounded channel.
- Produces: `LiveSendOutcome`, `LiveFrameSender::try_send_latest` while preserving existing methods.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveSendOutcome { Enqueued, ReplacedOldest, Disconnected }

impl LiveFrameSender {
    pub fn try_send_latest(&mut self, frame: LivePcmFrame) -> LiveSendOutcome;
}
```

- [ ] **Step 1: Write a failing replace-oldest queue test**

```rust
#[test]
fn latest_send_replaces_oldest_frame_when_full() {
    let (mut sender, mut decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);
    assert_eq!(sender.try_send_latest(frame_with_value(1)), LiveSendOutcome::Enqueued);
    assert_eq!(sender.try_send_latest(frame_with_value(2)), LiveSendOutcome::Enqueued);
    assert_eq!(sender.try_send_latest(frame_with_value(3)), LiveSendOutcome::ReplacedOldest);
    assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 2);
    assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 3);
}
```

- [ ] **Step 2: Run the focused audio test to establish red state**

Run: `cargo test -p airplay-audio live_decoder::tests::latest_send_replaces_oldest_frame_when_full --lib`

Expected: compilation fails because `LiveSendOutcome` is absent.

- [ ] **Step 3: Retain a receiver clone in `LiveFrameSender` and implement latest send**

```rust
pub struct LiveFrameSender {
    tx: Sender<LivePcmFrame>,
    eviction_rx: Receiver<LivePcmFrame>,
}

pub fn try_send_latest(&mut self, mut frame: LivePcmFrame) -> LiveSendOutcome {
    match self.tx.try_send(frame) {
        Ok(()) => LiveSendOutcome::Enqueued,
        Err(TrySendError::Disconnected(_)) => LiveSendOutcome::Disconnected,
        Err(TrySendError::Full(returned)) => {
            frame = returned;
            let _ = self.eviction_rx.try_recv();
            match self.tx.try_send(frame) {
                Ok(()) => LiveSendOutcome::ReplacedOldest,
                Err(TrySendError::Disconnected(_)) => LiveSendOutcome::Disconnected,
                Err(TrySendError::Full(_)) => unreachable!("exclusive producer cannot refill after eviction"),
            }
        }
    }
}
```

Keep `try_send` behavior and return type unchanged for existing callers. Do not implement `Clone` for `LiveFrameSender`; `PcmBridge` is the sole owner of its mutable latest-data send path, which makes eviction plus replacement atomic with respect to producers while the decoder may continue consuming.

- [ ] **Step 4: Add concurrent producer/consumer and disconnect tests**

Verify `try_send_latest` never blocks, reports `Disconnected` after decoder drop, and leaves frame ordering monotonic under one producer/one consumer stress loop.

```rust
#[test]
fn latest_sender_reports_decoder_disconnect() {
    let (mut sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);
    drop(decoder);
    assert_eq!(sender.try_send_latest(frame_with_value(9)), LiveSendOutcome::Disconnected);
}
```

- [ ] **Step 5: Run audio crate tests and commit**

Run: `cargo test -p airplay-audio live_decoder --lib`

Expected: all existing live-decoder tests and new latest-delivery tests pass.

```powershell
git add crates/airplay-audio/src/live_decoder.rs crates/airplay-audio/src/lib.rs
git commit -m "feat: keep latest live PCM frames"
```

### Task 9: Stable PCM Bridge and Capture Supervisor

**Files:**
- Create: `crates/homepod-cast/src/backend/capture.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/tests/support/mod.rs`
- Test: `crates/homepod-cast/src/backend/capture.rs`

**Interfaces:**
- Consumes: Task 8 `try_send_latest`, Task 4 clock, normalized 44.1 kHz stereo PCM.
- Produces: `CaptureSource`, `CaptureSupervisor`, `PcmBridge`, `CaptureControl`, `CaptureUpdate`, `AudioEndpoint`.

```rust
#[async_trait]
pub trait CaptureSource: Send + Sync {
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError>;
    async fn open(&self, preference: &AudioEndpointPreference) -> Result<Box<dyn CaptureWorker>, CaptureError>;
}

pub trait CaptureWorker: Send {
    fn run(
        self: Box<Self>,
        frames: crossbeam_channel::Sender<LivePcmFrame>,
        ready: tokio::sync::oneshot::Sender<Result<AudioEndpoint, UserFacingError>>,
        cancel: CancellationToken,
    ) -> Result<(), CaptureError>;
}
```

- [ ] **Step 1: Write failing readiness and silence-bridge tests**

```rust
#[tokio::test(start_paused = true)]
async fn capture_failure_keeps_decoder_alive_with_paced_silence() {
    let source = FakeCaptureSource::fails_once_then_streams();
    let (mut supervisor, mut decoder, mut updates) = CaptureHarness::start(source);
    assert!(matches!(updates.recv().await.unwrap(), CaptureUpdate::Recovering { generation: 1, .. }));
    tokio::time::advance(Duration::from_millis(50)).await;
    let frame = decoder.decode_frame().unwrap().unwrap();
    assert!(frame.samples.iter().all(|sample| *sample == 0));
    assert!(!decoder.is_eof());
    supervisor.shutdown().await.unwrap();
}
```

- [ ] **Step 2: Run capture tests to establish red state**

Run: `cargo test -p homepod-cast backend::capture::tests --lib`

Expected: compilation fails because `CaptureSupervisor` is absent.

- [ ] **Step 3: Implement one stable decoder/bridge pair**

Create the `LiveAudioDecoder` pair once per session generation with capacity 32. A bridge thread receives real frames from a bounded internal queue and sends a 50 ms silence frame when no real frame arrives before its deadline. Count `ReplacedOldest` outcomes as dropped frames.

```rust
const RATE: u32 = 44_100;
const CHANNELS: u8 = 2;
const SILENCE_PERIOD: Duration = Duration::from_millis(50);

fn silence_frame() -> LivePcmFrame {
    LivePcmFrame {
        samples: vec![0; (RATE as usize * CHANNELS as usize) / 20],
        channels: CHANNELS,
        sample_rate: RATE,
    }
}
```

- [ ] **Step 4: Implement readiness and retry schedule**

The worker sends `Ready { endpoint }` only after WASAPI-equivalent initialization. On error, retain the bridge, publish `Recovering`, and retry after 250 ms, 500 ms, 1 s, 2 s, then 5 s repeatedly. Replacing a capture worker increments capture generation but leaves session generation unchanged.

```rust
const CAPTURE_RETRY: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
let delay = CAPTURE_RETRY[attempt.min(4) as usize];
```

- [ ] **Step 5: Add silent-system versus recovery tests**

```rust
#[test]
fn audio_snapshot_distinguishes_silence_from_failed_capture() {
    assert!(matches!(AudioSourceSnapshot::silent_system().state, AudioSourceState::SilentSystem));
    assert!(matches!(AudioSourceSnapshot::recovering(user_error("endpoint lost")).state, AudioSourceState::Recovering));
}
```

- [ ] **Step 6: Test endpoint preference behavior with fakes**

Verify SystemDefault follows a fake default change; Explicit retains its stable ID and becomes Unavailable when absent; changing preference cancels/reopens capture without replacing the decoder or session generation.

```rust
#[tokio::test]
async fn explicit_loss_bridges_silence_without_session_restart() {
    let rig = CaptureRig::explicit("endpoint-a").await;
    let session_generation = rig.session_generation();
    rig.remove_endpoint("endpoint-a").await;
    assert!(matches!(rig.audio_state(), AudioSourceState::Unavailable));
    assert_eq!(rig.session_generation(), session_generation);
    assert!(rig.next_frame().await.samples.iter().all(|sample| *sample == 0));
}
```

- [ ] **Step 7: Run capture tests and commit**

Run: `cargo test -p homepod-cast backend::capture::tests --lib`

Expected: readiness, pacing, retry, drop count, default, explicit-unavailable, endpoint switch, and shutdown tests pass.

```powershell
git add crates/homepod-cast/src/backend/capture.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/tests/support/mod.rs
git commit -m "feat: supervise a stable Windows audio bridge"
```

### Task 10: WASAPI Endpoint Enumeration and Replaceable Capture Worker

**Files:**
- Modify: `crates/homepod-cast/src/backend/capture.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/Cargo.toml`
- Test: `crates/homepod-cast/src/backend/capture.rs`
- Test: `crates/homepod-cast/tests/backend_lifecycle.rs`

**Interfaces:**
- Consumes: wasapi 0.23 `DeviceEnumerator::{get_default_device,get_device,get_device_collection}`, `Device::{get_id,get_friendlyname}` and existing loopback conversion.
- Produces: `WasapiCaptureSource`, `WasapiCaptureWorker`; `cast.rs` retains only diagnostic-compatible session adapters after extraction.

- [ ] **Step 1: Write endpoint-selection characterization tests through an enumerator seam**

```rust
#[test]
fn explicit_endpoint_uses_id_and_never_falls_back_to_default() {
    let api = FakeWasapiApi::with_default("default").with_endpoint("chosen", "USB DAC");
    let source = WasapiCaptureSource::with_api(api.clone());
    source.resolve(&AudioEndpointPreference::Explicit { id: "chosen".into(), last_known_name: "USB DAC".into() }).unwrap();
    assert_eq!(api.requested_ids(), vec!["chosen"]);
    assert_eq!(api.default_requests(), 0);
}
```

- [ ] **Step 2: Run focused endpoint tests to establish red state**

Run: `cargo test -p homepod-cast backend::capture::tests::explicit_endpoint_uses_id_and_never_falls_back_to_default --lib`

Expected: compilation fails because `WasapiCaptureSource` is absent.

- [ ] **Step 3: Implement endpoint enumeration using stable Windows IDs**

```rust
fn enumerate_render_endpoints(enumerator: &DeviceEnumerator) -> anyhow::Result<Vec<AudioEndpoint>> {
    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let mut endpoints = Vec::new();
    for item in &collection {
        let device = item?;
        endpoints.push(AudioEndpoint {
            id: device.get_id()?,
            name: device.get_friendlyname()?,
        });
    }
    endpoints.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    Ok(endpoints)
}
```

- [ ] **Step 4: Move current loopback initialization into `WasapiCaptureWorker`**

Reuse the existing shared/event-driven loopback path: Render endpoint, Capture direction, `StreamMode::EventsShared { autoconvert: true }`, 44.1 kHz stereo f32 conversion to i16. Resolve SystemDefault with `get_default_device`; resolve Explicit only with `get_device(id)`.

```rust
let device = match preference {
    AudioEndpointPreference::SystemDefault => enumerator.get_default_device(&Direction::Render)?,
    AudioEndpointPreference::Explicit { id, .. } => enumerator.get_device(id)?,
};
```

- [ ] **Step 5: Report worker readiness before entering the read loop**

After `initialize_client`, event-handle creation, capture-client creation, and `start_stream`, send the resolved endpoint through the readiness oneshot. On read failure, stop the stream and return the error to the supervisor.

```rust
client.start_stream()?;
if ready.send(Ok(resolved_endpoint.clone())).is_err() {
    client.stop_stream()?;
    return Ok(());
}
let result = read_loop(&client, frames, &cancel);
let _ = client.stop_stream();
result
```

- [ ] **Step 6: Preserve diagnostic CLI routing**

Keep `--list`, `--selftest`, and `--selftest-group` using the same production discovery/session adapters. Add a compile test that calls the argument router without launching eframe or requiring an endpoint.

```rust
#[test]
fn diagnostic_routes_do_not_start_gui() {
    assert_eq!(route_args(["openaircast", "--list"]), LaunchRoute::List);
    assert_eq!(route_args(["openaircast", "--selftest", "Kitchen"]), LaunchRoute::SelfTest("Kitchen".into()));
}
```

- [ ] **Step 7: Run capture and diagnostic routing tests**

Run: `cargo test -p homepod-cast backend::capture --lib`

Expected: endpoint and worker seam tests pass.

Run: `cargo test -p homepod-cast --test backend_lifecycle diagnostic_routes_do_not_start_gui`

Expected: diagnostic arguments select their preserved routes.

- [ ] **Step 8: Commit WASAPI extraction**

```powershell
git add crates/homepod-cast/src/backend/capture.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/Cargo.toml crates/homepod-cast/tests/backend_lifecycle.rs
git commit -m "feat: recover selected WASAPI endpoints"
```

### Task 11: Cancellable AirPlay Connections and Owned Control Workers

**Files:**
- Modify: `crates/airplay-client/src/connection.rs`
- Modify: `crates/airplay-client/src/raop_connection.rs`
- Modify: `crates/airplay-client/src/group.rs`
- Modify: `crates/airplay-client/src/events.rs`
- Modify: `crates/airplay-client/src/lib.rs`
- Modify: `crates/airplay-client/Cargo.toml`
- Test: `crates/airplay-client/tests/connection_lifecycle.rs`

**Interfaces:**
- Consumes: current `Connection`, RTSP teardown, feedback requests, event/group listeners.
- Produces: `Connection::probe(&mut self, timeout) -> Result<Health>`, idempotent `Connection::disconnect(&mut self)`, and internally owned `CancellationToken` plus joined worker handles.

- [ ] **Step 1: Add lifecycle tests with a fake transport**

```rust
#[tokio::test]
async fn disconnect_cancels_and_joins_every_connection_worker() {
    let workers = WorkerLedger::default();
    let mut connection = test_connection(workers.clone()).await;
    assert_eq!(workers.running(), 3);
    connection.disconnect().await.unwrap();
    connection.disconnect().await.unwrap();
    assert_eq!(workers.running(), 0);
}

#[tokio::test]
async fn feedback_timeout_is_unhealthy_not_success() {
    let mut connection = test_connection_with_feedback(FakeFeedback::Never).await;
    assert_eq!(
        connection.probe(Duration::from_millis(10)).await.unwrap(),
        Health::TimedOut,
    );
}
```

- [ ] **Step 2: Run the lifecycle test to establish red state**

Run: `cargo test -p airplay-client --test connection_lifecycle`

Expected: compilation fails because owned worker lifecycle and `probe` do not exist.

- [ ] **Step 3: Introduce explicit health and worker ownership**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Health { Healthy, TimedOut }

struct ConnectionWorkers {
    cancel: CancellationToken,
    tokio_joins: Vec<tokio::task::JoinHandle<()>>,
    thread_joins: Vec<std::thread::JoinHandle<()>>,
}
```

Create the token before feedback/event/group workers are spawned. Every loop selects between its protocol operation and `cancel.cancelled()`; no detached Tokio task or unowned standard thread remains.

- [ ] **Step 4: Make disconnect bounded and idempotent**

Stop the connection's streamer, cancel and join its control/timing workers, then send RTSP teardown once and close sockets even when teardown fails. A second `disconnect()` returns success without another protocol request; `Drop` may only cancel and must not spawn work or panic.

```rust
pub async fn disconnect(&mut self) -> airplay_core::Result<()> {
    if self.disconnected { return Ok(()); }
    self.disconnected = true;
    let stop = self.stop_streamer().await;
    self.workers.cancel.cancel();
    for join in self.workers.tokio_joins.drain(..) {
        let _ = tokio::time::timeout(Duration::from_secs(2), join).await;
    }
    let threads = std::mem::take(&mut self.workers.thread_joins);
    let _ = tokio::time::timeout(Duration::from_secs(2), tokio::task::spawn_blocking(move || {
        for join in threads { let _ = join.join(); }
    })).await;
    let teardown = match tokio::time::timeout(Duration::from_secs(2), self.teardown_once()).await {
        Ok(result) => result,
        Err(_) => Err(airplay_core::Error::Rtsp(airplay_core::RtspError::SetupFailed(
            "TEARDOWN timed out after 2 seconds".to_owned(),
        ))),
    };
    self.close_sockets();
    match (stop, teardown) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
```

- [ ] **Step 5: Convert feedback into a real probe**

Return `Healthy` only after a valid response. Map timeout to `TimedOut`; preserve transport/protocol errors as `Err` so the supervisor can classify them.

```rust
pub async fn probe(&mut self, timeout: Duration) -> airplay_core::Result<Health> {
    match tokio::time::timeout(timeout, self.send_feedback_request()).await {
        Ok(Ok(response)) if response.status_code == 200 => Ok(Health::Healthy),
        Ok(Ok(response)) => Err(airplay_core::Error::Rtsp(airplay_core::RtspError::SetupFailed(
            format!("feedback returned RTSP {}", response.status_code),
        ))),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(Health::TimedOut),
    }
}
```

- [ ] **Step 6: Prove repeated start/stop does not leak workers**

Add a 25-cycle fake-transport test and assert the worker ledger reaches zero after every cycle and after the test runtime drains.

```rust
#[tokio::test]
async fn repeated_disconnect_returns_workers_to_baseline() {
    let workers = WorkerLedger::default();
    for _ in 0..25 {
        let mut connection = test_connection(workers.clone()).await;
        connection.disconnect().await.unwrap();
        assert_eq!(workers.running(), 0);
    }
}
```

- [ ] **Step 7: Run client tests and commit**

Run: `cargo test -p airplay-client --all-targets`

Expected: all AirPlay client lifecycle and existing protocol tests pass.

```powershell
git add crates/airplay-client/src/connection.rs crates/airplay-client/src/raop_connection.rs crates/airplay-client/src/group.rs crates/airplay-client/src/events.rs crates/airplay-client/src/lib.rs crates/airplay-client/Cargo.toml crates/airplay-client/tests/connection_lifecycle.rs
git commit -m "fix: own and cancel AirPlay control workers"
```

### Task 12: Typed Group Transport and Deterministic Primary Selection

**Files:**
- Modify: `crates/airplay-client/src/client.rs`
- Modify: `crates/airplay-client/src/group.rs`
- Modify: `crates/airplay-client/src/lib.rs`
- Test: `crates/airplay-client/tests/group_connect.rs`

**Interfaces:**
- Consumes: existing `AirPlayClient::connect_group`, `Connection`, `Device`, and live decoder.
- Produces exactly: `connect_group_best_effort`, `GroupConnectReport`, `MemberFailure`, `MemberResult<T>`, `Health`, `SetupPhase`, `probe_members`, and receiver-addressable `set_member_volume`; the current public convenience API delegates to this implementation.

- [ ] **Step 1: Write deterministic-primary tests**

```rust
#[tokio::test]
async fn preferred_healthy_member_is_primary_regardless_of_input_order() {
    let mut client = fake_client_with_healthy_group(&["c", "a"]);
    let report = client.connect_group_best_effort(&devices(&["b", "a", "c"]), Some(&device_id("c"))).await.unwrap();
    assert_eq!(report.primary, Some(device_id("c")));
}

#[tokio::test]
async fn fallback_primary_is_lowest_ptp_capable_stable_id() {
    let mut client = fake_client().mark_not_ptp_capable("a");
    let report = client.connect_group_best_effort(&devices(&["c", "a", "b"]), None).await.unwrap();
    assert_eq!(report.primary, Some(device_id("b")));
}
```

- [ ] **Step 2: Run the primary tests to establish red state**

Run: `cargo test -p airplay-client --test group_connect preferred_healthy_member_is_primary_regardless_of_input_order`

Expected: compilation fails because `connect_group_best_effort` and its typed report are absent.

- [ ] **Step 3: Define exact group startup types**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupPhase { Connect, Pair, PrimaryTiming, RtspSetup, SetPeers, BuildSender, StartAudio }

impl SetupPhase {
    pub const ALL: [Self; 7] = [Self::Connect, Self::Pair, Self::PrimaryTiming, Self::RtspSetup, Self::SetPeers, Self::BuildSender, Self::StartAudio];
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberFailure {
    pub receiver: DeviceId,
    pub phase: SetupPhase,
    pub retryable: bool,
    pub source: airplay_core::Error,
}

pub struct MemberResult<T> {
    pub receiver: DeviceId,
    pub result: std::result::Result<T, MemberFailure>,
}

pub struct GroupConnectReport {
    pub primary: Option<DeviceId>,
    pub connected: Vec<DeviceId>,
    pub failures: Vec<MemberFailure>,
}

impl AirPlayClient {
    pub async fn connect_group_best_effort(
        &mut self,
        devices: &[Device],
        preferred_primary: Option<&DeviceId>,
    ) -> airplay_core::Result<GroupConnectReport>;

    pub async fn probe_members(&mut self) -> Vec<MemberResult<Health>>;

    pub async fn set_member_volume(
        &mut self,
        id: &DeviceId,
        volume: f32,
    ) -> MemberResult<()>;
}
```

Keep the `ConnectionFactory` trait private to `client.rs` as the deterministic test seam; public callers see only the approved methods and report types above.

- [ ] **Step 4: Centralize stable primary choice**

Normalize devices by stable `DeviceId`, reject duplicate IDs, retain the current healthy primary when desired/castable, otherwise honor an available PTP-capable preferred primary, otherwise choose the lexicographically smallest PTP-capable stable ID. Do not derive primary from discovery/input order and never reuse timing identity from a failed attempt.

```rust
fn choose_primary<'a>(candidates: &'a [Device], preferred: Option<&DeviceId>) -> Option<&'a Device> {
    preferred.and_then(|id| candidates.iter().find(|device| &device.id == id && is_ptp_capable(device)))
        .or_else(|| candidates.iter().filter(|device| is_ptp_capable(device)).min_by_key(|device| device.id.0))
}
```

- [ ] **Step 5: Adapt the current public client API**

Keep source compatibility for existing examples by making `AirPlayClient::connect_group` call `connect_group_best_effort`, return success only when its legacy all-members expectation is met, and leave the typed report API available to the resilience adapter. Remove duplicated startup ordering from the convenience path.

```rust
pub async fn connect_group(&mut self, devices: &[Device]) -> airplay_core::Result<()> {
    let report = self.connect_group_best_effort(devices, None).await?;
    if report.connected.len() == devices.len() {
        Ok(())
    } else {
        Err(airplay_core::Error::Rtsp(airplay_core::RtspError::SetupFailed(
            format!("{} group member(s) failed", report.failures.len()),
        )))
    }
}
```

- [ ] **Step 6: Run group tests and commit**

Run: `cargo test -p airplay-client --test group_connect`

Expected: current-primary retention, PTP-capability filtering, deterministic selection, duplicate rejection, and legacy adapter tests pass.

```powershell
git add crates/airplay-client/src/client.rs crates/airplay-client/src/group.rs crates/airplay-client/src/lib.rs crates/airplay-client/tests/group_connect.rs
git commit -m "refactor: type AirPlay group startup"
```

### Task 13: Best-Effort Group Setup, Member Volume, and One Primary Fallback

**Files:**
- Modify: `crates/airplay-client/src/client.rs`
- Modify: `crates/airplay-client/src/group.rs`
- Modify: `crates/airplay-client/src/playback.rs`
- Modify: `crates/airplay-client/src/events.rs`
- Test: `crates/airplay-client/tests/group_connect.rs`
- Test: `crates/airplay-client/tests/group_volume.rs`

**Interfaces:**
- Consumes: Task 12 group interfaces and Task 11 probes/disconnect.
- Produces: phase-classified partial `GroupConnectReport`, independent member control/probe, one deterministic primary-candidate fallback, and the required one-survivor NTP fallback.

- [ ] **Step 1: Write partial-secondary and primary-fallback tests**

```rust
#[tokio::test]
async fn secondary_failure_keeps_other_members_playing() {
    let mut client = fake_client().fail("b", SetupPhase::RtspSetup);
    let report = client.connect_group_best_effort(&devices(&["a", "b", "c"]), Some(&device_id("a"))).await.unwrap();
    assert_eq!(report.connected, vec![device_id("a"), device_id("c")]);
    assert_eq!(report.failures[0].receiver, device_id("b"));
}

#[tokio::test]
async fn failed_primary_gets_one_deterministic_fallback_attempt() {
    let mut client = fake_client().fail("a", SetupPhase::PrimaryTiming);
    let report = client.connect_group_best_effort(&devices(&["a", "b", "c"]), Some(&device_id("a"))).await.unwrap();
    assert_eq!(report.primary, Some(device_id("b")));
    assert_eq!(client.primary_attempts(), vec![device_id("a"), device_id("b")]);
}
```

- [ ] **Step 2: Run both tests to establish red state**

Run: `cargo test -p airplay-client --test group_connect secondary_failure_keeps_other_members_playing failed_primary_gets_one_deterministic_fallback_attempt`

Expected: tests fail because setup is serial/all-or-nothing.

- [ ] **Step 3: Split setup into primary-critical and member-local phases**

Connect/pair candidates independently with bounded concurrency. Run timing on the selected primary. Record each receiver's failing `SetupPhase`; disconnect only that receiver on member-local failures. `SetPeers`, sender construction, and audio start operate on the surviving set.

```rust
const MAX_PARALLEL_SETUPS: usize = 4;
let attempts = futures::stream::iter(devices.iter().cloned().map(|device| connect_and_pair(device)))
    .buffer_unordered(MAX_PARALLEL_SETUPS)
    .collect::<Vec<_>>().await;
let mut survivors = attempts.into_iter().filter_map(record_failure_or_connection).collect::<Vec<_>>();
survivors.retain_mut(|member| run_member_setup(member, &mut failures).is_ok());
```

- [ ] **Step 4: Enforce the one-fallback rule**

When the selected primary fails a primary-critical phase, disconnect it, discard its timing identity, choose the lowest remaining PTP-capable stable ID, and retry the primary-critical path once. If that fallback candidate fails, return a report with no primary/connected members and both phase-classified failures; do not attempt a third primary.

```rust
for primary_attempt in 0..=1 {
    let Some(primary) = choose_primary(&survivors, preferred.as_ref()) else { break };
    match setup_primary(primary).await {
        Ok(identity) => break 'primary Ok((primary.id.clone(), identity)),
        Err(failure) => { failures.push(failure); remove_and_disconnect(primary).await; }
    }
    preferred = None;
    debug_assert!(primary_attempt < 1 || survivors.is_empty());
}
```

- [ ] **Step 5: Add truly per-member control and probe**

```rust
pub async fn set_member_volume(&mut self, id: &DeviceId, volume: f32) -> MemberResult<()> {
    let result = match self.connection_for_mut(id) {
        Some(connection) => connection.set_volume(volume).await.map_err(|source| MemberFailure {
            receiver: id.clone(),
            phase: SetupPhase::StartAudio,
            retryable: true,
            source,
        }),
        None => Err(MemberFailure {
            receiver: id.clone(),
            phase: SetupPhase::StartAudio,
            retryable: true,
            source: airplay_core::Error::Rtsp(airplay_core::RtspError::NoSession),
        }),
    };
    MemberResult { receiver: id.clone(), result }
}
```

Do not route volume or feedback through the primary connection. Add tests where the primary succeeds and one secondary returns a control error; other member calls must still execute.

Have playback fan-out attach `DeviceId` to target send failures and deliver `GroupRuntimeEvent::TargetSendFailed { receiver, source }` through an internal bounded 256-event channel owned by the client/session adapter. A full event channel increments an atomic loss counter; it never blocks RTP. Runtime health still requires RTSP probe/feedback—successful UDP send alone never marks a member healthy.

- [ ] **Step 6: Write and implement one-survivor and zero-survivor behavior**

```rust
#[tokio::test]
async fn one_successful_ptp_candidate_is_reconnected_through_ntp() {
    let mut client = fake_client().fail("b", SetupPhase::RtspSetup);
    let report = client.connect_group_best_effort(&devices(&["a", "b"]), Some(&device_id("a"))).await.unwrap();
    assert_eq!(report.connected, vec![device_id("a")]);
    assert_eq!(client.timing_attempts("a"), vec![TimingProtocol::Ptp, TimingProtocol::Ntp]);
    assert_eq!(client.ptp_workers_running(), 0);
}
```

If exactly one receiver survives PTP setup, tear down the partial PTP attempt and reconnect it through the established single-receiver NTP path. If none survives, return `primary: None`, an empty `connected`, and all failures so the backend can publish `SessionPhase::Failed` without losing desired membership.

- [ ] **Step 7: Verify failed members are torn down**

For every phase failure, assert the fake connection's teardown/cancellation ledger reaches zero while successful members remain active until group stop.

```rust
for phase in SetupPhase::ALL {
    let mut client = fake_client().fail("b", phase);
    let _ = client.connect_group_best_effort(&devices(&["a", "b", "c"]), Some(&device_id("a"))).await;
    assert_eq!(client.worker_ledger("b"), 0, "phase {phase:?}");
    assert!(client.worker_ledger("a") > 0);
}
```

- [ ] **Step 8: Run AirPlay client suite and commit**

Run: `cargo test -p airplay-client --all-targets`

Expected: all group, lifecycle, example compile, and legacy adapter tests pass.

```powershell
git add crates/airplay-client/src/client.rs crates/airplay-client/src/group.rs crates/airplay-client/src/playback.rs crates/airplay-client/src/events.rs crates/airplay-client/tests/group_connect.rs crates/airplay-client/tests/group_volume.rs
git commit -m "feat: connect AirPlay groups best effort"
```

### Task 14: Resilient Session Supervisor and Restart Classifier

**Files:**
- Create: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Test: `crates/homepod-cast/src/backend/session.rs`
- Test: `crates/homepod-cast/tests/session_recovery.rs`

**Interfaces:**
- Consumes: Task 12/13 `AirPlayClient` best-effort/member APIs, stable bridge decoder, desired members, retry policy, generation commands.
- Produces: private `SessionTransportFactory`/`SessionTransport` adapter seams, `SessionSupervisor`, `SessionRequest`, `SessionUpdate`, and `ReconfigureCause::requires_full_restart()`.

- [ ] **Step 1: Write the complete restart-matrix test**

```rust
#[test]
fn restart_classifier_matches_the_contract() {
    assert!(ReconfigureCause::Membership.requires_full_restart());
    assert!(ReconfigureCause::PrimaryFailure.requires_full_restart());
    assert!(ReconfigureCause::LatencyChanged.requires_full_restart());
    assert!(ReconfigureCause::ProtocolDesync.requires_full_restart());
    assert!(ReconfigureCause::NetworkRebind.requires_full_restart());
    assert!(ReconfigureCause::ReceiverAddressChanged.requires_full_restart());
    assert!(ReconfigureCause::TimingModeChanged.requires_full_restart());
    assert!(ReconfigureCause::StreamFormatChanged.requires_full_restart());
    assert!(ReconfigureCause::Resume.requires_full_restart());
    assert!(ReconfigureCause::SecondaryRejoin.requires_full_restart());
    assert!(ReconfigureCause::RemoveFailedTarget.requires_full_restart());
    assert!(!ReconfigureCause::MasterVolume.requires_full_restart());
    assert!(!ReconfigureCause::ReceiverVolume.requires_full_restart());
    assert!(!ReconfigureCause::Mute.requires_full_restart());
    assert!(!ReconfigureCause::CaptureEndpoint.requires_full_restart());
    assert!(!ReconfigureCause::SecondaryFailure.requires_full_restart());
    assert!(!ReconfigureCause::DiscoveryMetadata.requires_full_restart());
    assert!(!ReconfigureCause::SavedGroupEdited.requires_full_restart());
}
```

- [ ] **Step 2: Run classifier test to establish red state**

Run: `cargo test -p homepod-cast backend::session::tests::restart_classifier_matches_the_contract --lib`

Expected: compilation fails because the session module is absent.

- [ ] **Step 3: Define supervisor messages and state**

```rust
#[async_trait]
trait SessionTransportFactory: Send + Sync {
    async fn start(
        &self,
        desired: Vec<Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        latency: LatencyConfig,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure>;
}

#[async_trait]
trait SessionTransport: Send {
    fn primary(&self) -> Option<ReceiverId>;
    fn active_members(&self) -> BTreeSet<ReceiverId>;
    fn setup_failures(&self) -> Vec<MemberFailure>;
    fn try_runtime_event(&mut self) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag>;
    async fn probe_members(&mut self) -> Vec<MemberResult<Health>>;
    async fn set_member_volume(&mut self, receiver: ReceiverId, volume: f32) -> MemberResult<()>;
    async fn stop(&mut self) -> anyhow::Result<()>;
}

pub enum SessionRequest {
    Reconcile { generation: u64, desired: BTreeSet<ReceiverId>, preferred_primary: Option<ReceiverId>, latency: LatencyPreset },
    SetVolume { receiver: ReceiverId, volume: f32 },
    Probe,
    NetworkChanged,
    Suspend,
    Resume,
}

pub enum SessionUpdate {
    Starting { generation: u64 },
    Active { generation: u64, primary: ReceiverId, members: BTreeSet<ReceiverId>, partial_failures: Vec<MemberFailure> },
    MemberFailed { generation: u64, receiver: ReceiverId, primary: bool, error: UserFacingError },
    Recovering { generation: u64, reason: RecoveryReason, attempt: u32 },
    Stopped { generation: u64 },
}
```

The production adapter maps `airplay_client::SetupPhase` exhaustively to the identically named backend snapshot phase and converts `GroupConnectReport.connected` into stable `ReceiverId` values. The request channel is bounded at 256; shutdown uses the supervisor's separate `CancellationToken`, never that ordinary queue. Every async completion carries `generation`; stale completions are ignored and immediately cleaned up.

- [ ] **Step 4: Implement latest-generation reconciliation**

One supervisor owns at most one active transport and one in-flight start. A higher generation cancels the in-flight start, stops any obsolete transport, and starts only from the newest complete desired set. Volume/mute/capture changes do not increment session generation.

```rust
if request.generation > current_generation {
    start_cancel.cancel();
    if let Some(mut transport) = active.take() { transport.stop().await?; }
    current_generation = request.generation;
    start_cancel = session_cancel.child_token();
    in_flight = Some(spawn_start(request, start_cancel.clone()));
}
```

- [ ] **Step 5: Bound stop ordering**

Use the exact order: invalidate generation/signal cancellation; stop capture production and enable silence if continuing; stop streamer/sender; abort and join control/timing workers; start RTSP TEARDOWN futures concurrently with a two-second per-receiver timeout and four-second group deadline; close sockets; publish `Stopped`. All blocking loops use finite cancellation-aware receives so their owned handles join; a lifecycle test fails if the worker ledger does not return to baseline.

```rust
let group_stop = async {
    transport.stop_sender().await?;
    transport.cancel_and_join_workers().await?;
    futures::future::join_all(transport.members_mut().map(|member| async move {
        tokio::time::timeout(Duration::from_secs(2), member.teardown()).await
    })).await;
    transport.close_sockets();
    Ok::<(), SessionStopError>(())
};
tokio::time::timeout(Duration::from_secs(4), group_stop).await??;
```

- [ ] **Step 6: Test coalescing and stale completion cleanup**

```rust
#[tokio::test]
async fn rapid_generations_start_only_the_latest_membership() {
    let rig = SessionRig::paused();
    rig.reconcile(4, receivers(&["a"])).await;
    rig.reconcile(5, receivers(&["a", "b"])).await;
    rig.reconcile(6, receivers(&["c"])).await;
    rig.release_start().await;
    assert_eq!(rig.factory.started_member_sets(), vec![receivers(&["c"])]);
    assert_eq!(rig.leaked_transports(), 0);
}
```

- [ ] **Step 7: Run session tests and commit**

Run: `cargo test -p homepod-cast backend::session --lib`

Run: `cargo test -p homepod-cast --test session_recovery rapid_generations_start_only_the_latest_membership`

Expected: classifier, generation, cancellation, partial-start, and bounded-stop tests pass.

```powershell
git add crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/tests/session_recovery.rs
git commit -m "feat: supervise resilient AirPlay sessions"
```

### Task 15: Device Backend Controller, Snapshot Publication, and Coalesced Commands

**Files:**
- Create: `crates/homepod-cast/src/backend/controller.rs`
- Modify: `crates/homepod-cast/src/backend/command.rs`
- Modify: `crates/homepod-cast/src/backend/mod.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/Cargo.toml`
- Test: `crates/homepod-cast/src/backend/controller.rs`
- Test: `crates/homepod-cast/tests/backend_lifecycle.rs`

**Interfaces:**
- Consumes: domain, persistence, discovery, capture, session, diagnostics, and shell `AppHandle` effect executor.
- Produces exactly: `start_device_backend(config: BackendConfig) -> anyhow::Result<(DeviceBackendHandle, DeviceBackendUpdates)>` where updates contain `watch::Receiver<Arc<DeviceSnapshot>>`, `broadcast::Receiver<BackendEvent>`, and `DiagnosticEventReceiver`.

- [ ] **Step 1: Write public-contract compilation and initial-snapshot tests**

```rust
#[tokio::test]
async fn startup_returns_handle_and_all_three_update_feeds() {
    let (handle, mut updates) = start_device_backend(BackendConfig::for_test()).unwrap();
    let first = updates.state.borrow().clone();
    assert_eq!(first.revision, 0);
    assert!(matches!(updates.events.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    let id = handle.send(BackendCommand::Shutdown).await.unwrap();
    assert!(matches!(updates.events.recv().await.unwrap(), BackendEvent::CommandCompleted { id: completed } if completed == id));
}
```

- [ ] **Step 2: Run contract test to establish red state**

Run: `cargo test -p homepod-cast --test backend_lifecycle startup_returns_handle_and_all_three_update_feeds`

Expected: compilation fails because the controller/start function is absent.

- [ ] **Step 3: Construct exact bounded channels**

```rust
pub struct BackendConfig {
    pub state_directory: PathBuf,
    pub legacy_volume_path: PathBuf,
}

impl BackendConfig {
    pub fn for_current_user() -> anyhow::Result<Self> {
        let roaming = PathBuf::from(std::env::var_os("APPDATA")
            .ok_or_else(|| anyhow!("APPDATA is unavailable"))?);
        Ok(Self {
            state_directory: roaming.join("OpenAirCast"),
            legacy_volume_path: roaming.join("HomePodCast").join("volume.txt"),
        })
    }
}

pub fn start_device_backend(
    config: BackendConfig,
) -> anyhow::Result<(DeviceBackendHandle, DeviceBackendUpdates)> {
    let (commands_tx, commands_rx) = mpsc::channel(64);
    let (state_tx, state_rx) = watch::channel(Arc::new(DeviceSnapshot::initial()));
    let (events_tx, events_rx) = broadcast::channel(256);
    let (diagnostics_tx, diagnostics_rx) = diagnostic_channel(2048);
    let lifecycle = ControllerLifecycle::spawn(config, commands_rx, state_tx, events_tx.clone(), diagnostics_tx)?;
    Ok((DeviceBackendHandle::new(commands_tx, lifecycle), DeviceBackendUpdates {
        state: state_rx,
        events: events_rx,
        diagnostics: diagnostics_rx,
    }))
}
```

`ControllerLifecycle::spawn` creates the named control thread and its current-thread Tokio runtime, stores the `JoinHandle` in a private guard shared by handle clones, and uses a one-shot synchronous readiness result so `start_device_backend` returns only after path/state-store/runtime validation. Add that private lifecycle guard to `DeviceBackendHandle` without changing its public methods. Start supervisors after readiness succeeds. Use supervisor channels of 256, the stable PCM channel of 32, and watch semantics for latest state. Startup errors join the thread and return `Err` with no workers; the shutdown command joins all supervisors before completion, and the guard joins the already-finished control thread when the final handle is dropped.

- [ ] **Step 4: Coalesce intent, never edges**

Before reconciliation, drain immediately available commands and retain the last `SetDesiredMembers`, `SetRunIntent`, master, mute, endpoint, latency, and auto-connect value; coalesce receiver levels independently by `ReceiverId`. Preserve `SaveGroup`, `DeleteGroup`, `ActivateSavedGroup`, `RetryReceiver`, `NotifySystem`, and `Shutdown` in arrival order.

```rust
while let Ok(envelope) = commands_rx.try_recv() {
    match envelope.command {
        BackendCommand::SetDesiredMembers { members } => pending.desired = Some((envelope.id, members)),
        BackendCommand::SetReceiverLevel { receiver, level } => { pending.levels.insert(receiver, (envelope.id, level)); }
        command if command.is_edge() => pending.edges.push_back(CommandEnvelope { id: envelope.id, command }),
        command => pending.replace_scalar(envelope.id, command),
    }
}
```

- [ ] **Step 5: Centralize revision and desired-revision rules**

Every published semantic state change increments `revision`. Only accepted desired-membership changes increment `desired_revision`. Every committed membership or run-intent change increments `session_generation` before cancelling prior reconciliation. Duplicate complete desired sets and duplicate run intent are no-ops. `ActivateSavedGroup` replaces the full desired set atomically, increments `desired_revision` once when that set differs, applies `start` to run intent, and creates at most one new session generation after both values are committed.

At load, restore `last_desired_members` before discovery and publish them as selected/unavailable rows. Use saved-group `last_known_name` when available and the colon-form stable ID otherwise. Discovery updates merge mutable name/model/address into those rows without changing identity or desired membership.

```rust
fn publish(&mut self) {
    self.snapshot.revision = self.snapshot.revision.checked_add(1).expect("snapshot revision overflow");
    self.snapshot.sort_for_publication();
    self.state_tx.send_replace(Arc::new(self.snapshot.clone()));
}
```

- [ ] **Step 6: Implement saved-group validation and activation transaction**

Trim names, require 1–64 Unicode scalar values, reject case-insensitive duplicates and duplicate/empty member lists, and preserve `SavedGroupId` on edits. Save/delete never changes desired membership or session generation. Activation atomically copies member IDs into desired membership and each saved member level into global `receiver_levels`; `start: true` also sets Running, while `start: false` changes only committed desired/balance intent. Persist the complete transaction before one publication/reconciliation.

```rust
#[tokio::test]
async fn saved_group_activation_commits_members_levels_and_start_once() {
    let rig = BackendRig::with_saved_group(group("Evening", &[("a", 0.4), ("b", 0.8)]));
    rig.activate_group("Evening", true).await;
    assert_eq!(rig.snapshot().desired_members, receivers(&["a", "b"]));
    assert_eq!(rig.persisted_levels(), levels(&[("a", 0.4), ("b", 0.8)]));
    assert_eq!(rig.snapshot().run_intent, RunIntent::Running);
    assert_eq!(rig.session_generation_delta(), 1);
}
```

- [ ] **Step 7: Persist accepted durable intent before publication**

For durable commands, validate, save via Task 3's crash-safe store, then publish. On save failure, retain the previous durable value, set `persistence.last_error`, emit `BackendEvent::CommandFailed` for the originating command plus `BackendEvent::Notice { code: NoticeCode::PersistenceWrite, scope: ErrorScope::Persistence, .. }`, and publish only the changed error state.

```rust
match self.store.save(&candidate).await {
    Ok(()) => { self.persisted = candidate; self.apply_validated(command); self.complete(id); }
    Err(error) => { self.note_persistence_failure(id, error); }
}
self.publish();
```

- [ ] **Step 8: Test generation and coalescing**

```rust
#[tokio::test]
async fn three_membership_commands_publish_only_the_last_generation() {
    let rig = BackendRig::paused();
    rig.send_members(&["a"]).await;
    rig.send_members(&["a", "b"]).await;
    rig.send_members(&["c"]).await;
    rig.release_controller().await;
    assert_eq!(rig.snapshot().desired_members, receivers(&["c"]));
    assert_eq!(rig.session_reconciles(), vec![receivers(&["c"])]);
}
```

- [ ] **Step 9: Run controller tests and commit**

Run: `cargo test -p homepod-cast backend::controller --lib`

Run: `cargo test -p homepod-cast --test backend_lifecycle`

Expected: public contract, coalescing, revision, persistence failure, and graceful shutdown tests pass.

```powershell
git add crates/homepod-cast/src/backend/controller.rs crates/homepod-cast/src/backend/command.rs crates/homepod-cast/src/backend/mod.rs crates/homepod-cast/src/main.rs crates/homepod-cast/Cargo.toml crates/homepod-cast/tests/backend_lifecycle.rs
git commit -m "feat: expose the resilient device backend"
```

### Task 16: Receiver Health, Failure Isolation, Retry, and Rejoin

**Files:**
- Modify: `crates/homepod-cast/src/backend/controller.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/backend/model.rs`
- Test: `crates/homepod-cast/tests/session_recovery.rs`

**Interfaces:**
- Consumes: session member updates, continuous discovery availability, explicit `RetryReceiver`, retry policy.
- Produces: independent receiver lifecycle snapshots and `ReceiverFailed`, `ReceiverRetryScheduled`, `ReceiverRejoined`, `PrimaryChanged` events.

- [ ] **Step 1: Write secondary-isolation and primary-restart tests**

```rust
#[tokio::test]
async fn secondary_failure_does_not_restart_healthy_members() {
    let rig = active_rig(&["a", "b", "c"], "a").await;
    rig.fail_probe("b").await;
    assert_eq!(rig.active_members(), receivers(&["a", "c"]));
    assert_eq!(rig.full_restart_count(), 0);
    assert!(matches!(rig.receiver_state("b"), ReceiverLifecycle::RetryWaiting { .. }));
}

#[tokio::test]
async fn primary_failure_restarts_the_survivors_once() {
    let rig = active_rig(&["a", "b", "c"], "a").await;
    rig.fail_probe("a").await;
    assert_eq!(rig.full_restart_count(), 1);
    assert_eq!(rig.primary(), Some(receiver("b")));
}
```

- [ ] **Step 2: Run recovery tests to establish red state**

Run: `cargo test -p homepod-cast --test session_recovery secondary_failure_does_not_restart_healthy_members primary_failure_restarts_the_survivors_once`

Expected: tests fail because receiver-level lifecycle and recovery are incomplete.

- [ ] **Step 3: Implement the receiver state machine**

Permit only:

```text
Discovered -> Connecting { attempt } -> SettingUp { role, Connect|Pair|PrimaryTiming|RtspSetup|SetPeers|BuildSender|StartAudio }
SettingUp -> Ready { role } -> Streaming { role }
Connecting|SettingUp|Ready|Streaming -> Failed { retryable, error }
Failed { retryable: true, .. } -> RetryWaiting { attempt, retry_at } -> Connecting { attempt }
Any state -> Unavailable when discovery no longer has a castable AirPlay service
Unavailable -> Discovered when the receiver becomes castable again
```

Store attempt, next retry instant, last error, active generation, service availability, and role per receiver. A non-desired receiver may be removed from the snapshot only after it is unavailable; removal is inventory cleanup, not a `ReceiverLifecycle` variant. Invalid transitions fail tests in debug builds and emit an invariant diagnostic in release builds.

- [ ] **Step 4: Apply the exact retry policy**

Use 1, 2, 4, 8, 16, then 30 seconds with deterministic ±20% injected jitter. Reset attempts after 60 continuously healthy seconds. `RetryReceiver` clears only that receiver's pending deadline; the ensuing reconciliation still obeys the full-restart matrix, so retrying a secondary may cause the required announced group restart but does not create an extra restart outside reconciliation.

Probe every active member independently every two seconds. Treat a hard RTSP/protocol close as immediate failure; require two consecutive `Health::TimedOut` results before failure, resetting the streak on `Healthy`. A target UDP send error schedules an immediate probe and becomes failure only if that probe is timed out/error. This reaches Degraded within 10 seconds while never treating successful UDP sends as proof of health.

```rust
match probe.result {
    Ok(Health::Healthy) => receiver.timeout_streak = 0,
    Ok(Health::TimedOut) if receiver.timeout_streak == 0 => receiver.timeout_streak = 1,
    Ok(Health::TimedOut) | Err(_) => self.fail_receiver(probe.receiver),
}
```

- [ ] **Step 5: Rejoin secondaries through one rate-limited full restart**

Require two seconds of continuous castable discovery before an automatic rejoin. Because the current group streamer has no mutable target insertion, increment session generation and perform one coordinated full restart against the complete newest desired set. Rate-limit automatic rejoin restarts to one per 30 seconds. If best-effort setup still omits the receiver, resume the successful subset and continue only that receiver's backoff; never call metadata-only `add_to_group()`/`remove_from_group()` for active reconciliation.

```rust
if stability.online_for(receiver) >= Duration::from_secs(2) && rejoin_limiter.allow(now) {
    rejoin_limiter.record(now);
    self.restart_session(RestartReason::SecondaryRejoin(receiver));
}
```

- [ ] **Step 6: Handle primary loss deterministically**

Primary failure increments session generation and rebuilds with surviving desired/available receivers. Prefer the prior primary only when healthy; otherwise choose the lexicographically lowest PTP-capable stable ID. If exactly one receiver survives, reconnect it through NTP. Do not wait for unrelated unavailable members before starting survivors.

```rust
if failed == self.snapshot.session.primary {
    let survivors = self.desired_available_except(failed);
    self.increment_session_generation(RestartReason::PrimaryLost(failed));
    self.reconcile_now(survivors, None);
}
```

- [ ] **Step 7: Test failure storms and isolation**

With fake paused time, fail three secondaries at different phases and assert the primary/unaffected sender remain active before rejoin. Rediscover one for two seconds, assert exactly one announced full rejoin restart, then flap it again inside 30 seconds and assert no second automatic restart. Finally fail the primary and assert one immediate new full restart and no leaked worker.

```rust
#[tokio::test(start_paused = true)]
async fn rejoin_storm_is_rate_limited_but_primary_loss_is_immediate() {
    let rig = RecoveryRig::active(&["a", "b", "c", "d"], "a").await;
    rig.fail_secondaries(&["b", "c", "d"]).await;
    rig.rediscover_for("b", Duration::from_secs(2)).await;
    assert_eq!(rig.rejoin_restarts(), 1);
    rig.flap_for("b", Duration::from_secs(29)).await;
    assert_eq!(rig.rejoin_restarts(), 1);
    rig.fail_primary("a").await;
    assert_eq!(rig.primary_loss_restarts(), 1);
}
```

- [ ] **Step 8: Run recovery suite and commit**

Run: `cargo test -p homepod-cast --test session_recovery`

Expected: isolation, primary rebuild, stable rejoin, retry jitter, healthy reset, manual retry, and leak tests pass.

```powershell
git add crates/homepod-cast/src/backend/controller.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/backend/model.rs crates/homepod-cast/tests/session_recovery.rs
git commit -m "feat: isolate and recover receiver failures"
```

### Task 17: Network, Suspend/Resume, Auto-Connect, Mute, and Latency Recovery

**Files:**
- Create: `crates/homepod-cast/src/backend/network.rs`
- Modify: `crates/homepod-cast/src/backend/controller.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/backend/model.rs`
- Modify: `crates/homepod-cast/src/platform/mod.rs`
- Modify: `crates/homepod-cast/Cargo.toml`
- Test: `crates/homepod-cast/tests/system_recovery.rs`

**Interfaces:**
- Consumes: Windows IP interface notifications, shell-forwarded `NotifySystem::{Suspending,Resumed}`, discovery stability, capture endpoint state, persisted auto-connect/mute/latency.
- Produces: `NetworkMonitor`, recovery events/state, enabled-latency validation, correct effective volume.

- [ ] **Step 1: Write system-event policy tests**

```rust
#[tokio::test]
async fn resume_restarts_discovery_and_reconciles_only_latest_intent() {
    let rig = SystemRig::active(&["a", "b"]).await;
    rig.notify(SystemEvent::Suspending).await;
    rig.set_desired(&["c"]).await;
    rig.notify(SystemEvent::Resumed).await;
    rig.discover_stably("c", Duration::from_secs(2)).await;
    assert_eq!(rig.started_member_sets(), vec![receivers(&["a", "b"]), receivers(&["c"])]);
}
```

- [ ] **Step 2: Run system recovery test to establish red state**

Run: `cargo test -p homepod-cast --test system_recovery resume_restarts_discovery_and_reconciles_only_latest_intent`

Expected: compilation fails because system recovery adapters are absent.

- [ ] **Step 3: Implement cancellable network monitoring**

Wrap `NotifyIpInterfaceChange`/`CancelMibChangeNotify2` behind a trait for fake tests and retain the returned notification handle until cancellation. Coalesce one callback burst into one `NetworkChanged` edge, cancel the subscription on shutdown, wait the specified two-second interface-settle period, restart both discovery streams, invalidate stale addresses, and request a full session restart when an active local binding/address changed. The 30-second limiter applies to automatic secondary rejoin only, not to a distinct network recovery.

Enable only `windows-sys` features `Win32_NetworkManagement_IpHelper` and `Win32_Networking_WinSock` for the monitor and address records.

```rust
pub trait NetworkChangeSource: Send + Sync {
    fn subscribe(&self, tx: mpsc::Sender<SystemEvent>) -> Result<Box<dyn NetworkSubscription>, NetworkError>;
}
pub trait NetworkSubscription: Send { fn cancel(&mut self); }
```

- [ ] **Step 4: Implement suspend/resume ordering**

On suspend, set resume-pending, cancel discovery/retry delays, switch capture to silence, stop capture and the session within the four-second shutdown budget, retain desired/run intent, and publish Stopped-with-resume-pending. On resume, wait exactly two seconds for interfaces/default endpoint to settle, recreate discovery and capture generations, invalidate every pre-suspend RTSP/RTP/PTP object, and best-effort reconcile the newest desired set only when the retained run intent is Running.

```rust
SystemEvent::Resumed => {
    clock.sleep(Duration::from_secs(2)).await;
    self.restart_discovery_and_capture();
    if self.snapshot.run_intent == RunIntent::Running { self.restart_session(RestartReason::Resume); }
}
```

- [ ] **Step 5: Implement optional auto-connect without broadening membership**

At process startup only, persisted auto-connect waits for initial discovery stability and changes run intent from Stopped to Running once at least one `last_desired_members` receiver is available. Changing `SetAutoConnect` persists policy for later launches and does not itself change current run intent. Never select a newly discovered unrelated receiver; an explicit user Stop remains stopped for the rest of that process unless the user explicitly starts again.

```rust
if startup_auto_connect_armed && desired.iter().any(|id| discovery.is_stable_castable(id)) {
    startup_auto_connect_armed = false;
    self.apply_run_intent(RunIntent::Running);
}
```

- [ ] **Step 6: Apply mute and effective volume without restart**

```rust
fn effective_volume(master: Volume, receiver: Volume, muted: bool) -> Volume {
    if muted { Volume::MUTED } else { Volume::new(master.get() * receiver.get()).expect("validated factors stay in range") }
}
```

Pass each changed effective value's `f32` to `AirPlayClient::set_member_volume`, which performs the protocol dB conversion. Send independently to active receivers; one control failure updates/retries that receiver and does not prevent later sends. Mute, master, and receiver changes never increment session generation.

- [ ] **Step 7: Enforce latency availability and restart semantics**

Expose Low and Stable in snapshots but mark them disabled with a reason until their hardware matrix passes. Normal uses the existing 2000 ms buffer and 200 ms render lead. Reject selection of disabled presets with `CommandRejected`; an accepted enabled preset change persists first and requires one full restart.

```rust
match preset.enabled_config() {
    None => self.fail_command(id, UserFacingError::disabled_latency(preset)),
    Some(_) if preset == self.snapshot.latency_preset => self.complete(id),
    Some(_) => self.persist_then_restart(id, preset, RestartReason::LatencyPresetChanged),
}
```

- [ ] **Step 8: Test network storms, suspend, mute, and disabled latency**

Use fake time and adapters to assert 20 callbacks in one interface-change burst coalesce to one discovery/session generation after the two-second settle, a later distinct binding change can recover immediately, suspend/resume has no leaks, endpoint recovery does not restart AirPlay, mute sends zero effective volume to every active receiver, and disabled presets do not mutate state.

```rust
#[tokio::test(start_paused = true)]
async fn callback_burst_coalesces_but_later_binding_change_recovers() {
    let rig = SystemRig::running().await;
    rig.network_callbacks(20).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(rig.network_restart_count(), 1);
    rig.change_active_binding().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(rig.network_restart_count(), 2);
}
```

- [ ] **Step 9: Run system recovery suite and commit**

Run: `cargo test -p homepod-cast --test system_recovery`

Expected: network, suspend/resume, auto-connect, volume isolation, capture independence, and latency validation tests pass.

```powershell
git add crates/homepod-cast/src/backend/network.rs crates/homepod-cast/src/backend/controller.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/backend/model.rs crates/homepod-cast/src/platform/mod.rs crates/homepod-cast/Cargo.toml crates/homepod-cast/tests/system_recovery.rs
git commit -m "feat: recover backend across system changes"
```

### Task 18: Shell Integration and Approved Staged-Membership UX

**Files:**
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`
- Modify: `crates/homepod-cast/src/app/snapshot.rs`
- Modify: `crates/homepod-cast/src/app_handle.rs`
- Modify: `crates/homepod-cast/src/ui/home.rs`
- Modify: `crates/homepod-cast/src/ui/pages.rs`
- Modify: `crates/homepod-cast/src/preferences.rs`
- Modify: `crates/homepod-cast/Cargo.toml`
- Create: `crates/homepod-cast/src/platform/startup.rs`
- Modify: `crates/homepod-cast/src/platform/mod.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Test: `crates/homepod-cast/src/app/reducer.rs`
- Test: `crates/homepod-cast/tests/shell_backend_contract.rs`

**Interfaces:**
- Consumes: merged Subproject 1 shell and only its stable view boundary `AppHandle::{dispatch,snapshot,drain_ui_effects,install_waker}` plus Task 15 backend contract.
- Produces: reducer-owned staged membership, backend-to-shell effect adapter, saved-group/audio/volume controls, shell-owned start-with-Windows setting. `DeviceSnapshot` remains internal to the reducer/effect executor and is never read directly by views, tray, or hotkey code.

- [ ] **Step 1: Add a hard prerequisite contract test**

```rust
fn view_boundary_is_still_stable(handle: &AppHandle) {
    let _ = handle.dispatch(AppEvent::ShowMainWindow);
    let _: Arc<UiSnapshot> = handle.snapshot();
    let _: Vec<UiEffect> = handle.drain_ui_effects();
    handle.install_waker(Arc::new(|| {}));
}
```

Run: `cargo test -p homepod-cast --test shell_backend_contract view_boundary_is_still_stable`

Expected before integration: this compiles only after Subproject 1 has merged. Stop implementation at this task if the shell modules or these four methods are absent; do not recreate a parallel shell.

- [ ] **Step 2: Write reducer tests for staging, apply, discard, and staleness**

```rust
#[test]
fn receiver_checkboxes_stage_locally_and_apply_once() {
    let mut state = app_state_with_desired(7, &["a"]);
    let staged = reduce(&mut state, AppEvent::ToggleStagedReceiver(receiver("b")));
    assert!(staged.effects.is_empty());
    let applied = reduce(&mut state, AppEvent::ApplyStagedReceivers);
    assert_eq!(applied.effects, vec![AppEffect::SendDeviceCommand(BackendCommand::SetDesiredMembers {
        members: receivers(&["a", "b"]),
    })]);
}

#[test]
fn external_desired_revision_marks_dirty_stage_stale_without_overwriting_it() {
    let mut state = dirty_stage(7, &["a", "b"]);
    reduce(&mut state, AppEvent::DeviceSnapshot(backend_snapshot_with_desired(8, &["c"])));
    assert!(state.staged_membership.is_stale);
    assert_eq!(state.staged_membership.members, receivers(&["a", "b"]));
}
```

- [ ] **Step 3: Implement the approved staging model exactly**

Migrate Subproject 1's temporary shell `DeviceId` value losslessly to `ReceiverId` (or a transparent alias) while keeping the same 12-hex storage/egui identity. Checkboxes modify only `AppState.staged_membership.members`. `ApplyStagedReceivers` emits one complete `SetDesiredMembers`; `DiscardStagedReceivers` copies current backend desired members and emits nothing. Discovery additions/removals never overwrite a dirty stage. Track `staged_base_revision`; if backend `desired_revision` changes externally, show stale state and require Apply or Discard. Activating a saved group emits one `ActivateSavedGroup`; after confirmation, refresh a clean stage, but retain and mark a dirty stage stale exactly as for every external desired-revision change.

- [ ] **Step 4: Extend `UiSnapshot` without changing established semantics**

Add immutable rows for discovery/service status, receiver lifecycle/error/retry, primary, desired/active flags, effective volume, saved groups, staged dirty/stale state, audio endpoints/source state, master/mute, latency availability, auto-connect, and persistence error. Preserve Subproject 1's existing close, visibility, tray, status, and navigation fields.

- [ ] **Step 5: Route backend state and edges through effects**

Add `AppEffect::SendDeviceCommand(BackendCommand)` and `AppEvent::{DeviceSnapshot(Arc<DeviceSnapshot>),BackendEvent(BackendEvent)}` inside the shell boundary. The effect executor owns `DeviceBackendUpdates`, translates watch changes and low-volume broadcast edges to those events, and calls the installed waker. Backend-event lag triggers a watch resync and local warning; it never fabricates state from missed edges. `DiagnosticEventReceiver` converts its own lag into typed `QueueLag` events. Retain that receiver in the backend-service adapter without mapping it into `UiSnapshot`; provide a crate-private take/resubscribe point for Subproject 3's registry.

- [ ] **Step 6: Connect all controls to commands**

Map Apply, Start/Stop, master, mute, per-receiver level, endpoint, enabled latency preset, auto-connect, save/delete/activate group, retry receiver, suspend, resume, and shutdown. Debounce slider dispatches but always send the final release value. Display user-facing persistence/receiver errors without blocking healthy controls.

- [ ] **Step 7: Keep start-with-Windows in shell persistence**

Implement `StartupRegistration` under `platform/startup.rs` using the current executable and `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run`. Store the boolean in shell `settings.json`, not backend `state-v1.json`; apply registry changes as a shell effect and roll back the UI value on failure.

Enable `windows-sys` feature `Win32_System_Registry`; write/delete only the `OpenAirCast` value, quote the executable path, and treat an existing exact value as idempotent success.

- [ ] **Step 8: Add UI rendering tests for mixed health and staged changes**

Render snapshots containing an active primary, failed secondary, unavailable desired receiver, newly discovered unselected receiver, dirty/stale stage, recovering endpoint, and disabled latency rows. Assert Apply is a single action, Discard has no backend effect, and healthy receiver controls remain enabled.

- [ ] **Step 9: Run shell and integration tests and commit**

Run: `cargo test -p homepod-cast app:: --lib`

Run: `cargo test -p homepod-cast --test shell_backend_contract`

Expected: stable boundary, reducer, staging, backend adapter, startup registration seam, and UI state tests pass.

```powershell
git add crates/homepod-cast/src/app crates/homepod-cast/src/app_handle.rs crates/homepod-cast/src/ui crates/homepod-cast/src/preferences.rs crates/homepod-cast/src/platform crates/homepod-cast/src/main.rs crates/homepod-cast/Cargo.toml crates/homepod-cast/tests/shell_backend_contract.rs
git commit -m "feat: connect the control center to resilient devices"
```

### Task 19: End-to-End Diagnostics, Cleanup, and Failure-Storm Tests

**Files:**
- Modify: `crates/homepod-cast/src/backend/controller.rs`
- Modify: `crates/homepod-cast/src/backend/discovery.rs`
- Modify: `crates/homepod-cast/src/backend/capture.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Test: `crates/homepod-cast/tests/device_resilience.rs`
- Test: `crates/homepod-cast/tests/backend_lifecycle.rs`

**Interfaces:**
- Consumes: all supervisor updates, queue/drop counters, retry decisions, backend shutdown.
- Produces: complete diagnostic stream required by Subproject 3 and deterministic graceful process exit.

- [ ] **Step 1: Write a diagnostic correlation test**

```rust
#[tokio::test]
async fn one_failed_receiver_has_a_correlated_diagnostic_timeline() {
    let rig = ResilienceRig::active(&["a", "b"], "a").await;
    rig.fail_secondary("b", SetupPhase::RtspSetup).await;
    rig.advance_to_retry("b").await;
    rig.recover("b").await;
    let events = rig.diagnostics_for("b");
    assert_eq!(events.iter().map(|e| e.payload.kind()).collect::<Vec<_>>(), vec![
        DiagnosticKind::ReceiverSetupFailed,
        DiagnosticKind::RetryScheduled,
        DiagnosticKind::RetryStarted,
        DiagnosticKind::ReceiverRejoined,
    ]);
    assert!(events.windows(2).all(|pair| pair[0].session_generation <= pair[1].session_generation));
    assert!(events.last().unwrap().session_generation > events.first().unwrap().session_generation);
}
```

- [ ] **Step 2: Run the diagnostic test to establish red state**

Run: `cargo test -p homepod-cast --test device_resilience one_failed_receiver_has_a_correlated_diagnostic_timeline`

Expected: assertion fails until every transition emits the required correlation fields.

- [ ] **Step 3: Complete the diagnostic feed**

Emit timestamp, severity, component, event kind, receiver ID when applicable, session/capture/discovery generation, attempt, duration, queue depth/drop delta, user-safe message, and error-chain summary for discovery restarts, capture loss/readiness, session start/stop/restart, every receiver phase/failure/retry/rejoin, network/sleep transitions, persistence migration/save failure, broadcast lag, and shutdown timeout. Do not include pairing secrets, keys, or raw credentials.

- [ ] **Step 4: Exercise independent failure domains end to end**

In one fake-time test: flap RAOP only, restore it, fail capture while the AirPlay session stays active, overflow PCM, fail a secondary, change master volume, then fail the primary. Assert discovery continues, silence bridges capture recovery, healthy members survive secondary loss, only primary loss restarts the group, and queue sizes never exceed their declared bounds.

- [ ] **Step 5: Replace forced process termination**

Remove every normal `std::process::exit` and `runtime.shutdown_timeout` path. Window/tray exit dispatches backend shutdown; the controller switches/stops capture, stops session, cancels discovery/network workers, joins them within the four-second total session budget, emits shutdown completion, and returns its runtime/control thread naturally before the shell exits.

- [ ] **Step 6: Add 50-cycle leak and stale-generation tests**

Repeatedly start/stop, switch endpoint, trigger discovery errors, reconnect, and suspend/resume. Assert fake task/thread/transport ledgers are zero, state revision is monotonic, no stale generation becomes active, and shutdown completes within fake four-second budget.

- [ ] **Step 7: Run end-to-end and workspace tests and commit**

Run: `cargo test -p homepod-cast --test device_resilience -- --test-threads=1`

Run: `cargo test -p homepod-cast --test backend_lifecycle -- --test-threads=1`

Run: `cargo test --workspace --all-targets`

Expected: all resilience, lifecycle, AirPlay, discovery, shell, and legacy tests pass with no leaked-worker assertions.

```powershell
git add crates/homepod-cast/src/backend crates/homepod-cast/src/main.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/tests/device_resilience.rs crates/homepod-cast/tests/backend_lifecycle.rs
git commit -m "test: verify device resilience end to end"
```

### Task 20: Release Verification and Hardware Acceptance Gates

**Files:**
- Modify: `README.md`
- Create: `docs/testing/device-resilience-hardware.md`
- Test: workspace and Windows hardware acceptance commands below.

**Interfaces:**
- Consumes: completed Subproject 2 and merged Subproject 1.
- Produces: verified release behavior and a concrete diagnostics contract/acceptance record for Subproject 3; no latency preset becomes enabled from documentation alone.

- [ ] **Step 1: Run formatting, lint, and complete automated suite**

Run: `cargo fmt --all -- --check`

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Run: `cargo test --workspace --all-targets --all-features -- --test-threads=1`

Expected: all commands exit 0. Fix failures in the owning task's files and rerun that task's focused red/green tests before rerunning the workspace command.

- [ ] **Step 2: Run diagnostic CLI compatibility checks on Windows**

Run: `cargo run -p homepod-cast -- --list`

Run: `$firstReceiverName = Read-Host 'Enter one unique receiver name from --list'; cargo run -p homepod-cast -- --selftest $firstReceiverName`

Run: `$secondReceiverName = Read-Host 'Enter a second unique receiver name from --list'; cargo run -p homepod-cast -- --selftest-group $firstReceiverName $secondReceiverName`

Expected: list remains continuous for its documented interval; each self-test reports discovery/setup/stream/teardown phases and exits without opening the GUI. Ambiguous names are rejected with matching stable IDs displayed.

- [ ] **Step 3: Execute the required real-hardware resilience matrix**

On Windows 10 or 11 with at least three physical AirPlay 2 receivers including two HomePods, record timestamped pass/fail evidence for: persisted three-receiver group with auto-connect off/on; three staged checkbox changes followed by one Apply/one restart; 30-minute three-receiver stream; secondary loss degraded within 10 seconds; secondary rediscovery/rejoin through one announced restart within 60 seconds; primary loss recovery within 60 seconds; default endpoint recovery within 10 seconds without session-generation change; explicit endpoint disappearance/return without fallback; network disable/enable recovery within 90 seconds; 60-second suspend and recovery within 90 seconds; master/receiver/mute changes without restart; every enabled latency preset with exactly one restart; start-with-Windows after sign-out/sign-in; corrupted-state quarantine; and 25 start/stop plus two-to-three-member Apply cycles. Each case records desired/active membership, primary, time to recover, audible continuity of unaffected receivers, queue/drop counters, session generations/restart reasons, and leaked worker/handle count after stop.

- [ ] **Step 4: Gate latency presets with measured criteria**

For Normal, verify the existing 2000 ms buffer/200 ms render lead across the matrix. For Low and Stable, collect startup success, ten-minute underrun/drop rate, synchronization drift, reconnect behavior, and suspend/resume behavior on every supported receiver class. Enable a preset in `LatencyPresetSnapshot` only in a follow-up code change after every criterion in the hardware document passes; otherwise keep it visible and disabled with its reason.

- [ ] **Step 5: Verify persistence and migration on an installed-style profile**

Use a disposable Windows user profile or redirected `APPDATA`; confirm atomic writes leave no temp file, corrupt JSON is quarantined, v1 round-trips, legacy volume imports once without deleting the legacy source, `state-v1.json` and shell `settings.json` remain separate, and start-with-Windows points to the current executable.

- [ ] **Step 6: Document the Subproject 3 diagnostic handoff**

List every `DiagnosticKind`, field, capacity (2048), lag behavior, redaction rule, and correlation key in `docs/testing/device-resilience-hardware.md`. State that diagnostics may subscribe and export but may not own receiver/session lifecycle or bypass `AppHandle`.

- [ ] **Step 7: Review full-restart evidence against the contract**

Confirm traces show full restart for applied membership change, saved-group activation with different membership, secondary reconnect/rejoin, rebuilding without a failed immutable target, primary loss/replacement, active receiver address change, local interface/address change, one-receiver NTP versus multi-receiver PTP transition, stream-format/timing/SETUP-latency change, enabled latency preset, Windows resume, and dead RTSP recovery. Confirm master volume, receiver level, mute, endpoint change/recovery, discovery display metadata, saved-group edits, and the initial act of detecting a secondary failure do not restart healthy sessions. The current upstream API provides no supported restart-free secondary rejoin path.

- [ ] **Step 8: Commit documentation and acceptance record**

```powershell
git add README.md docs/testing/device-resilience-hardware.md
git commit -m "docs: record device resilience acceptance"
```

The implementation series is complete only after automated checks pass and the hardware record contains a result for every matrix row. Low and Stable remain disabled unless their complete hardware gates pass.
