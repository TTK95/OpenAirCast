# OpenAirCast Device Resilience and Group State Design

Date: 2026-08-22
Status: Approved design for subproject 2
Predecessor: subproject 1, the egui/eframe control-center shell
Successor: subproject 3, diagnostics and operational visibility

## Purpose

Subproject 2 turns the current one-shot tray controller into a UI-independent
backend that continuously discovers receivers, owns desired and active group
state, persists saved groups and volume preferences, and recovers from receiver,
audio-device, network, and sleep/wake failures.

The backend presents one stable command/state/event contract to the egui shell.
The shell renders state and stages user edits; it never owns an AirPlay
`Connection`, a discovery `Device`, a WASAPI capture thread, retry timers, or
group reconciliation logic.

This design deliberately keeps the existing synchronized fan-out architecture:
one Windows loopback capture, one live PCM buffer, one ALAC encoder, one RTP
timeline, and receiver-specific encryption/sockets. It does not claim seamless
dynamic AirPlay joining. With the current group streamer, membership changes and
receiver rejoin are coordinated full-session restarts.

## Scope

This subproject includes:

- stable receiver identity and continuous `_airplay._tcp` plus `_raop._tcp`
  discovery;
- backend commands, snapshots, lifecycle events, cancellation, and bounded
  queues;
- staged and atomically applied membership changes;
- desired versus active membership and partial-success startup;
- saved groups, receiver levels, master volume, mute, schema migration, and
  atomic persistence;
- system-default or explicitly selected Windows render endpoint, Low/Normal/Stable
  latency presets, optional auto-connect, and start-with-Windows integration;
- primary and secondary failure isolation, reconnect policy, and synchronized
  rejoin semantics;
- supervised WASAPI capture with silence bridging and default-device recovery;
- network and Windows suspend/resume recovery;
- lifecycle data required by the later diagnostics subproject.

This subproject does not build diagnostics screens, packet charts, log export,
seamless live target insertion, acoustic skew measurement, or a new pairing
credential store. The existing shell remains responsible for presentation,
window/tray behavior, hotkeys, and forwarding Windows power notifications.

## Current constraints

The implementation begins from these verified repository constraints:

- `homepod-cast` performs one three-second scan and stores receiver vector
  indices in `GroupSelection`.
- `cast::Session` owns one capture thread and either one `Connection` or one
  `AirPlayClient` group.
- `AirPlayClient::connect_group()` connects and sets up receivers serially,
  fixes the first receiver as primary, and returns no partial-success report.
- `AudioStreamer` has an immutable vector of RTP targets for its lifetime.
- UDP send errors are already isolated per target, and retransmit history is
  already receiver-specific.
- `DeviceGroup::add_to_group()`, `remove_from_group()`, and
  `set_member_volume()` are metadata operations; they do not mutate a running
  stream or send a targeted volume command.
- the current control-listener loops do not have deterministic cancellation,
  which is why the application forces runtime and process shutdown.
- `ClientBuilder::auto_reconnect()` has no runtime effect.

The design below does not expose these limitations to the shell as accidental
behavior. It models them as explicit lifecycle transitions and restart reasons.

## Architecture

The backend is a single-owner actor running on its own control thread and Tokio
runtime. Commands, discovery events, capture events, transport results, power
events, and retry deadlines all enter that actor. Only the actor mutates domain
state and publishes `DeviceSnapshot` revisions.

```text
egui/eframe shell
    | bounded Command queue
    v
Device backend actor ----> watch<DeviceSnapshot> ----> shell reducer/effects
    |        |       |
    |        |       +----> PersistenceStore
    |        +------------> CaptureSupervisor ----> stable live PCM bridge
    +---------------------> DiscoverySupervisor
    +---------------------> SessionSupervisor ----> airplay-client
    |
    +---- bounded BackendEvent + DiagnosticEvent feeds
```

Each long-running subsystem has one cancellation token and owned join handles.
No worker changes UI-visible state directly. Results carry a session or
discovery generation, allowing the actor to ignore stale work safely.

## Stable receiver identity

The application uses a validated value type instead of discovery indices,
addresses, or display names:

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReceiverId([u8; 6]);
```

`ReceiverId` converts losslessly to and from `airplay_core::DeviceId`. Its
persistent representation is exactly twelve uppercase hexadecimal characters,
for example `5855CA1AE288`; its display form is colon separated. Deserialization
rejects any other length or non-hexadecimal byte.

Receiver IP addresses and names are mutable discovery attributes. An address or
name change updates the existing receiver record. Saved groups and desired
membership therefore remain stable across DHCP, mDNS rename, list reordering,
and application restart.

## Shell/backend contract

Subproject 1 consumes the following public library surface from
`homepod-cast`:

```rust
pub fn start_device_backend(
    config: BackendConfig,
) -> anyhow::Result<(DeviceBackendHandle, DeviceBackendUpdates)>;

#[derive(Clone)]
pub struct DeviceBackendHandle {
    command_tx: tokio::sync::mpsc::Sender<CommandEnvelope>,
}

pub struct DeviceBackendUpdates {
    pub state: tokio::sync::watch::Receiver<Arc<DeviceSnapshot>>,
    pub events: tokio::sync::broadcast::Receiver<BackendEvent>,
    pub diagnostics: DiagnosticEventReceiver,
}

pub struct CommandEnvelope {
    pub id: u64,
    pub command: BackendCommand,
}
```

`DeviceBackendHandle::try_send()` is non-blocking for the shell effect executor
and returns a typed `BackendBusy` or `BackendClosed` error.
`DeviceBackendHandle::send()` is async for non-UI callers. Command IDs are
allocated by the handle and echoed in completion or failure events. Views, tray,
and hotkey never receive this handle directly; they keep using subproject 1's
`AppHandle` and `UiSnapshot`.

The command enum is:

```rust
pub enum BackendCommand {
    SetDesiredMembers {
        members: BTreeSet<ReceiverId>,
    },
    SetRunIntent(RunIntent),
    SetMasterVolume(Volume),
    SetMuted(bool),
    SetReceiverLevel {
        receiver: ReceiverId,
        level: Volume,
    },
    SetAudioEndpoint(AudioEndpointPreference),
    SetLatencyPreset(LatencyPreset),
    SetAutoConnect(bool),
    SaveGroup {
        id: Option<SavedGroupId>,
        name: String,
        members: Vec<SavedGroupMember>,
    },
    DeleteGroup(SavedGroupId),
    ActivateSavedGroup {
        id: SavedGroupId,
        start: bool,
    },
    RetryReceiver(ReceiverId),
    NotifySystem(SystemEvent),
    Shutdown,
}

pub enum SystemEvent {
    Suspending,
    Resumed,
    NetworkChanged,
}
```

`Volume` is a finite validated value in `0.0..=1.0`. NaN and infinity are
rejected. `AudioEndpointPreference` is either `SystemDefault` or a stable Windows
endpoint ID with a last-known display name. `LatencyPreset` is `Low`, `Normal`,
or `Stable` and resolves to one versioned sender-buffer/render-lead configuration;
the UI never exposes unvalidated free-form timing values. `SavedGroupId` wraps a
UUID. Saved-group names are trimmed, contain 1–64 Unicode scalar values, and are
unique case-insensitively.

### Authoritative snapshot

The watch channel holds the latest complete render model:

```rust
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

Receiver and group vectors are sorted for deterministic rendering: receivers by
case-folded display name and then `ReceiverId`, saved groups by case-folded name
and then ID. The snapshot contains no `Device`, socket, cryptographic key,
`Connection`, or join handle.

`BackendEvent` contains low-volume shell notifications:

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

Snapshots are authoritative if the event receiver lags. Event overflow is
reported by Tokio's `Lagged` result, after which the shell refreshes from the
watch snapshot. User-facing error strings are prepared by the backend so the
shell does not interpret protocol errors.

## Approved staged-membership UX

Membership editing is deliberately separate from applying a group change:

1. The shell owns a local `staged_members: BTreeSet<ReceiverId>`, initialized
   from `DeviceSnapshot::desired_members`.
2. Checking or unchecking receiver rows changes only `staged_members`. It sends
   no backend command, does not interrupt playback, and can include an offline
   saved receiver.
3. While the staged set differs from the snapshot's desired set, the shell
   shows a persistent change bar with **Apply speaker changes** and **Discard**.
   It also states that applying while audio is running briefly restarts the
   group.
4. **Apply speaker changes** sends exactly one
   `SetDesiredMembers { members: staged_members.clone() }` command. The backend
   treats the supplied set as a complete replacement, increments
   `desired_revision` once, persists it, and performs at most one reconciliation
   against the latest set.
5. **Discard** restores the stage from the newest snapshot and sends nothing.
6. If discovery changes while edits are staged, existing staged IDs are kept.
   Offline members receive an offline badge; they are not silently removed.
   Newly discovered receivers appear unchecked.
7. If another action changes `desired_revision` while the stage is dirty—for
   example activating a saved group—the shell does not overwrite it
   automatically. It marks the stage as based on an older revision and asks the
   user to discard/reload or apply the staged set explicitly.
8. Activating a saved group is an explicit atomic action. `start: true` replaces
   desired membership and sets `RunIntent::Running`; `start: false` only loads
   the saved group as the committed desired set. Editing a saved group in the UI
   still uses the same staged apply bar.

The shell renders both committed and actual state: “Selected” comes from
`desired_members`; “Playing” comes from active session members. During degraded
operation, a selected offline receiver stays selected but is not shown as
playing.

## Desired, active, and staged membership

There are three intentionally distinct sets:

- **staged**: shell-local uncommitted edits;
- **desired**: the backend's committed receiver set, persisted across launches;
- **active**: receivers with successfully negotiated connections in the current
  session generation.

The application starts with persisted desired membership restored. Auto-connect
defaults to false, which leaves `RunIntent::Stopped`. When the user explicitly
enables auto-connect, startup waits for initial discovery stability and then sets
`RunIntent::Running` once at least one desired receiver is available. Failure is
visible and follows normal retry policy; it never selects an unrelated receiver.

An offline desired receiver remains desired. Partial startup can therefore
produce `desired={A,B,C}` and `active={A,C}`. Successful rediscovery of B starts
rejoin policy; it does not mutate desired membership.

## Generation and command coalescing

The actor maintains `desired_revision`, `session_generation`,
`discovery_generation`, and `capture_generation` counters.

- Every committed membership or run-intent change increments
  `session_generation` before cancellation of the prior reconciliation.
- Transport and capture results include the generation that created them.
- A result with an older generation is disposed and cannot alter the snapshot.
- Before starting expensive reconciliation, the actor drains immediately ready
  commands and coalesces them: the newest desired set, run intent, and master
  volume win; receiver level updates coalesce independently by `ReceiverId`.
- If a new membership command arrives during setup, setup is cancelled,
  partially created connections are boundedly disconnected, and only the latest
  desired set is attempted.
- Explicit Apply from the shell means normal membership editing creates one
  generation and one restart, not one restart per checkbox.

Queue capacities are fixed:

- backend commands: 64;
- backend events: 256;
- diagnostic events: 2,048;
- supervisor-to-actor events: 256 per supervisor;
- live PCM input: 32 frames;
- state watch: one latest snapshot.

No producer waits indefinitely. PCM uses non-blocking newest-data behavior and
counts dropped frames. Lifecycle queues reject or report overload; they never
grow without bound. Shutdown and cancellation signals use watch/cancellation
tokens rather than competing for ordinary queue capacity.

## Receiver and session lifecycle

Receiver snapshots use these states:

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

pub enum ReceiverRole {
    Single,
    Primary,
    Secondary,
}
```

`SetupPhase` has `Connect`, `Pair`, `PrimaryTiming`, `RtspSetup`, `SetPeers`,
`BuildSender`, and `StartAudio`. These phases are reported by the group
transport adapter, not inferred by the shell.

Session snapshots use:

```rust
pub enum SessionPhase {
    Stopped,
    Starting { generation: u64 },
    Streaming { generation: u64 },
    Degraded { generation: u64 },
    Restarting { generation: u64, reason: RestartReason },
    Stopping { generation: u64 },
    Failed { generation: u64, error: UserFacingError },
}
```

`SessionSnapshot` identifies desired, active, failed, and retry-waiting members,
the current primary, and whether audio is live or silence-bridged. The session
is `Streaming` only when every desired member is active. It is `Degraded` when
at least one receiver remains active but a desired receiver or the capture
source is unavailable. It is `Failed` only when run intent is Running and no
receiver can be kept active after best-effort recovery.

## Continuous dual-service discovery

`airplay-discovery` changes from an unannotated event stream to:

```rust
pub type BrowseStream = Pin<
    Box<dyn Stream<Item = Result<BrowseEvent, DiscoveryError>> + Send>
>;

pub enum BrowseEvent {
    Added(Device),
    Updated(Device),
    Removed(DeviceId),
}
```

`ServiceBrowser` tracks both service instance identity and receiver identity:

```text
(service kind, full service name) -> DeviceId
DeviceId -> { airplay instance, optional RAOP instance, merged Device }
```

Resolving either service updates the mapping before emitting an event. Removing
RAOP data must not remove a still-present AirPlay receiver. Removing the
AirPlay instance makes an AirPlay 2 receiver unavailable, while its stable
record and RAOP metadata may remain cached for presentation. A receiver is
castable only when a resolved AirPlay service supplies a supported AirPlay 2
configuration and a usable IPv4 address.

The browser stream owns its browse registrations. Dropping or cancelling it
stops both registrations. A scan cannot stop an independent browse. Cache state
is scoped to one browser generation and is cleared when a failed daemon is
recreated.

The discovery supervisor restarts a failed daemon with exponential delays of
1, 2, 4, 8, 16, and 30 seconds, each with deterministic ±20% jitter. Network
change or resume cancels the current delay and creates a fresh discovery
generation after a two-second interface-settle period. Discovery failure changes
availability to Unknown and never directly tears down a healthy RTSP/RTP
session.

## Saved groups and persistence

Application state is stored at
`%APPDATA%\OpenAirCast\state-v1.json`:

```rust
pub struct PersistedStateV1 {
    pub version: u32,                       // exactly 1
    pub master_volume: Volume,
    pub muted: bool,
    pub receiver_levels: BTreeMap<ReceiverId, Volume>,
    pub saved_groups: Vec<SavedGroup>,
    pub last_desired_members: BTreeSet<ReceiverId>,
    pub audio_endpoint: AudioEndpointPreference,
    pub latency_preset: LatencyPreset,
    pub auto_connect: bool,
}

pub struct SavedGroup {
    pub id: SavedGroupId,
    pub name: String,
    pub members: Vec<SavedGroupMember>,
}

pub struct SavedGroupMember {
    pub receiver: ReceiverId,
    pub last_known_name: String,
    pub level: Volume,
}
```

Groups contain at least one unique receiver. Offline members remain stored and
visible. Saving a group writes the currently staged member order chosen by the
shell, but runtime identity comparisons use sets.

Writes use a temporary file in the same directory, flush file data, then replace
the destination atomically with Windows replace/write-through semantics. The
backend never truncates the last known-good file in place. A malformed state
file is renamed with a `.corrupt-<UTC timestamp>` suffix, defaults are loaded,
and a persistence notice is emitted.

On the first launch without `state-v1.json`, the backend reads the current
`%APPDATA%\HomePodCast\volume.txt`, validates and clamps it, writes it as
`master_volume`, and leaves the legacy file intact. Hotkey migration belongs to
subproject 1 because the shell owns hotkeys. Upstream AirPlay pairing identity
files are not copied into this state file and never appear in snapshots or
diagnostic events.

Persistence writes are coalesced for 250 ms. Shutdown performs one bounded
two-second final flush. A failed write leaves in-memory state active, reports
`PersistenceSnapshot::Error`, and retries after five seconds without blocking
audio.

This device-state file is the sole owner of playback preferences. The shell's
separate `%APPDATA%\OpenAirCast\settings.json` owns theme, hotkey, window
geometry, navigation, close-to-tray behavior, and start-with-Windows preference;
it never duplicates volume, mute, desired membership, endpoint, latency, groups,
or receiver levels. Start-with-Windows registration is performed by the shell's
Windows platform service and displayed beside auto-connect as a distinct setting:
one starts the process at login, the other opts into starting audio after safe
initial discovery.

## Volume semantics

Master and receiver values are both linear values in `0.0..=1.0`:

```text
effective_volume(receiver) = if muted {
    0.0
} else {
    clamp(master_volume * receiver_level, 0.0, 1.0)
}
```

The default master is `0.25`; the default receiver level is `1.0`. Changing the
master sends a targeted effective volume to every active receiver. Changing one
receiver level sends only that receiver's effective volume. Failures are
reported per receiver and do not prevent updates to later receivers.

Volume changes never restart the group. A disconnected receiver receives its
current effective volume after SETUP and before the first audio packet. Receiver
levels are persisted globally and copied into saved-group members so restoring a
saved group also restores its intended balance.

Mute preserves master and receiver values and applies zero to every active
receiver without restarting. Unmute reapplies each computed effective volume.

## Audio endpoint and latency settings

`AudioSourceSnapshot` contains all active render endpoints, the persisted
preference, the currently captured endpoint, availability, and capture-recovery
state. `SystemDefault` follows Windows default-render changes. An explicitly
selected endpoint stays selected by stable endpoint ID; if it disappears, the
capture supervisor bridges with silence and reports it unavailable rather than
silently switching sources. Selecting another endpoint restarts only WASAPI
capture and retains the AirPlay session generation.

The three latency presets are product labels for versioned sender-side
configurations, not measured receiver latency. `Normal` preserves the currently
validated two-second sender buffer and existing render-lead behavior. `Low`
reduces buffering only after hardware acceptance proves stable playback;
`Stable` increases recovery headroom only within receiver-negotiated limits.
Until those two mappings pass the same hardware gate, they remain disabled with
an explanatory label rather than guessing unsafe timing values. Changing an
enabled preset is restart-only and requests exactly one controlled full-group
restart. Diagnostics exports the resolved numeric configuration and labels all
receiver latency bounds as requested, not measured.

## Best-effort setup and upstream transport contract

`airplay-client` must expose receiver-addressable group results instead of one
all-or-nothing error:

```rust
pub async fn connect_group_best_effort(
    &mut self,
    devices: &[Device],
    preferred_primary: Option<&DeviceId>,
) -> Result<GroupConnectReport>;

pub struct GroupConnectReport {
    pub primary: Option<DeviceId>,
    pub connected: Vec<DeviceId>,
    pub failures: Vec<MemberFailure>,
}

pub async fn probe_members(&mut self) -> Vec<MemberResult<Health>>;

pub async fn set_member_volume(
    &mut self,
    id: &DeviceId,
    volume: f32,
) -> MemberResult<()>;
```

Every `MemberFailure` includes receiver ID, `SetupPhase`, retryability, and the
source error. Failed setup explicitly disconnects all partially created
connections and cancels timing/control tasks.

Primary selection is deterministic: keep the current healthy primary when it
remains desired and castable; otherwise select the lexicographically smallest
PTP-capable `ReceiverId`. If primary setup fails, clean up that attempt, elect
the next capable receiver, and rebuild group setup. A primary timing identity is
never reused across a failed primary attempt.

Secondary setup is best effort. A failed secondary is omitted from SETPEERS and
the RTP target set while later secondaries are still attempted. If at least two
receivers succeed, start the PTP group. If exactly one succeeds, tear down the
partial PTP attempt and reconnect that receiver through the established
single-receiver NTP path. If none succeeds, publish `SessionPhase::Failed` and
retain desired membership for retries.

The existing metadata-only `add_to_group()` and `remove_from_group()` are not
used for active reconciliation.

## Failure handling and rejoin

### Secondary receiver failure

A discovery removal alone marks the receiver unavailable but does not stop
survivors. Runtime health is established from RTSP feedback/probes plus sender
errors; UDP silence alone is not treated as proof of receiver health.

When a secondary fails, the session becomes Degraded and the remaining targets
continue. Reconnect attempts wait for the receiver to be rediscovered, then use
1, 2, 4, 8, 16, and 30-second backoff with ±20% jitter. A receiver that streams
for 60 continuous seconds resets its attempt count.

Because the current streamer cannot insert a target, a successful secondary
rejoin requires one full group restart. The backend waits until the receiver has
been continuously discoverable for two seconds, then performs one coordinated
restart against the complete newest desired set. Automatic rejoin restarts are
rate-limited to one per 30 seconds so a flapping receiver cannot repeatedly
interrupt healthy members. If that restart still excludes the receiver, the
successful subset resumes and backoff continues.

### Primary receiver failure

The primary supplies the PTP clock identity and timing updates shared by every
secondary. Its failure therefore invalidates group timing. The backend cancels
the current generation, elects a new primary from online desired members, and
performs an immediate full restart. If one receiver remains, it falls back to
single-receiver NTP. Primary loss cannot be isolated without renegotiation and
must never be presented as seamless.

### User stop and retry

`RunIntent::Stopped` cancels retry timers, stops capture after switching the PCM
bridge to silence, and boundedly disconnects every connection. It preserves
desired membership. Returning to Running creates a new session generation.
`RetryReceiver` clears that receiver's backoff only; reconciliation still
follows the primary/secondary rules above.

## Capture and default-audio-device recovery

The session no longer hands `LiveFrameSender` directly to a single WASAPI
thread. A `CaptureSupervisor` owns a stable PCM bridge for the whole session
generation. A replaceable WASAPI worker sends normalized 44.1 kHz, stereo,
16-bit PCM frames into it.

The bridge paces silence when the worker has not supplied enough frames. It
therefore keeps the live decoder, AirPlay buffers, feedback, and RTP timeline
alive while Windows is silent, the endpoint changes, or capture restarts.

Capture startup has a readiness handshake. The session does not report live
audio until WASAPI initialization succeeds or the bridge explicitly enters
silence-recovery mode. Capture errors and default-device invalidation terminate
only the worker. The supervisor retries device enumeration after 250 ms, 500 ms,
1 s, 2 s, and then every 5 s. Recovery to the same normalized format does not
restart AirPlay. `AudioSourceSnapshot` distinguishes Capturing, SilentSystem,
Recovering, Unavailable, and Failed while the session may remain Degraded.

`LiveFrameSender::try_send` outcomes are counted. When the bounded PCM queue is
full, the oldest pending frame is discarded in favor of current audio so stale
latency cannot accumulate.

## Network and sleep/wake recovery

Subproject 1 forwards `SystemEvent::Suspending` and `Resumed` from its Windows
window/message integration. The backend also receives interface changes from a
network monitor; either source may emit `NetworkChanged`.

On suspend:

- set a suspending flag and cancel discovery/reconnect delays;
- switch capture to silence, then stop capture;
- boundedly disconnect active AirPlay sessions for at most four seconds total;
- publish Stopped-with-resume-pending while retaining run intent and desired
  membership.

On resume:

- wait two seconds for interfaces and the default endpoint to settle;
- recreate discovery and capture generations;
- treat all pre-suspend RTSP, RTP, and PTP state as invalid;
- if pre-suspend run intent was Running, perform best-effort setup against the
  newest desired set.

On ordinary network loss, healthy audio is not declared healthy solely because
UDP sends succeed. Once probes fail or the interface disappears, the session is
degraded/failed and retries pause until a usable interface returns. A new local
address always rebuilds RTSP and PTP state; socket destinations are never patched
in place.

## Full-restart matrix

These operations require a full active-session restart with the current
upstream group API:

- applying any added or removed desired member;
- activating a saved group with a different member set;
- reconnecting or rejoining a secondary receiver;
- removing a failed target from the immutable streamer;
- primary loss or primary replacement;
- an active receiver address change;
- a local network-interface/address change;
- transition between one receiver/NTP and two-or-more receivers/PTP;
- stream format, timing mode, or SETUP-negotiated latency changes;
- applying an enabled Low/Normal/Stable latency preset;
- Windows resume after suspend;
- recovery after dead RTSP sessions.

These operations do not require a group restart:

- master-volume, mute, or receiver-level changes;
- receiver display-name/model metadata updates;
- saved-group create, rename, update, or delete;
- temporary discovery failure while probes and sessions remain healthy;
- default or explicitly selected Windows audio endpoint loss, replacement, or
  user selection when the normalized PCM bridge remains alive;
- a secondary discovery disappearance before transport health has failed.

## Cancellation and shutdown

Discovery, capture, current session, each connection's timing task, and each
control listener have explicit cancellation tokens. Owners retain join handles.

Normal cancellation order is:

1. invalidate generation and signal cancellation;
2. stop new capture production and enable silence if the session will continue;
3. stop the streamer/sender thread;
4. abort and join control/timing workers;
5. send RTSP TEARDOWN concurrently to receivers;
6. close sockets and publish the final snapshot.

Per-receiver TEARDOWN is limited to two seconds and total group shutdown to four
seconds. One blocked receiver cannot delay later receivers. Every start/stop
cycle returns worker counts to baseline; process exit and runtime
`shutdown_timeout` are no longer lifecycle mechanisms.

## Diagnostics output for subproject 3

Subproject 2 defines and emits the bounded `DiagnosticEventReceiver` source feed
but does not render it. The source is consumed by subproject 3's registry, whose
UI-facing `DiagnosticsHandle` owns aggregation, cursoring, and redaction. Events
contain monotonic timestamp, wall-clock timestamp, session generation, optional
receiver ID, severity, category, and typed payload.

Required payloads are:

- discovery generation start/stop/error and receiver add/update/remove;
- command accepted/coalesced/completed/failed;
- receiver lifecycle transition and setup-phase duration;
- session start, active member set, degraded reason, restart reason, and stop;
- retry scheduled/attempted/reset;
- capture state, dropped frame count, silence duration, and device recovery;
- feedback/probe result per receiver;
- persistence load/migration/write/corruption recovery;
- queue high-water and lag notifications;
- cancellation and worker shutdown duration.

No pairing keys, PINs, cryptographic material, full network payloads, or Windows
user paths appear in diagnostic events. The diagnostics receiver may lag and
drop old entries without blocking audio; a lag event records the number lost.
Subproject 3 may add aggregation, charts, export, and redaction UI without
changing backend state ownership or the shell contract.

## File layout

Create in `homepod-cast`:

- `src/lib.rs` — public backend entry point and exported contract;
- `src/backend/mod.rs` — module assembly;
- `src/backend/model.rs` — IDs, volumes, lifecycle, snapshot, saved-group types;
- `src/backend/command.rs` — command envelope and handle;
- `src/backend/event.rs` — shell and diagnostic events;
- `src/backend/controller.rs` — single-owner actor, revisions, coalescing;
- `src/backend/discovery.rs` — discovery supervisor and `Device` mapping;
- `src/backend/session.rs` — reconciliation and restart policy;
- `src/backend/capture.rs` — WASAPI worker and stable silence bridge;
- `src/backend/persistence.rs` — schema, migration, validation, atomic writes;
- `src/backend/recovery.rs` — retry policy, jitter, rate limits, fake clock hooks;
- `tests/backend_lifecycle.rs`;
- `tests/persistence.rs`;
- `tests/recovery.rs`.

Modify in `homepod-cast`:

- `Cargo.toml` — serialization, cancellation, UUID, and test dependencies;
- `src/cast.rs` — reduce to transport/capture adapters or remove after extraction;
- `src/main.rs` — start the library backend through the subproject 1 shell.

Subproject 1 integration modifies its shell actor/effect executor to hold
`DeviceBackendHandle` and `DeviceBackendUpdates`; its reducer owns staged
membership and staged base revision, then publishes the combined `UiSnapshot`
through `AppHandle`. The device backend does not depend on egui or eframe.

Modify in `airplay-discovery`:

- `src/traits.rs` — fallible owned browse stream;
- `src/browser.rs` — dual-service instance mapping and independent lifecycle;
- `src/lib.rs` — export the revised stream types;
- `Cargo.toml` — cancellation/stream support if required by the implementation.

Create/modify in `airplay-client`:

- create `src/group_transport.rs` — addressable connections and best-effort
  report;
- modify `src/client.rs` — delegate group setup, feedback, and member volume;
- modify `src/connection.rs` — cancellation, bounded teardown, setup phases;
- modify `src/group.rs` — stable member lookup and explicit metadata role;
- modify `src/events.rs` and `src/lib.rs` — typed public results/exports;
- create `tests/group_lifecycle.rs`.

`airplay-audio/src/streamer.rs` does not need mutable target insertion for this
subproject. Changes there are limited to cancellable sender lifecycle and PCM
queue/drop observability if those cannot be implemented in the app adapter.

## Automated test requirements

### Domain and shell contract

- receiver identity parsing, formatting, ordering, and JSON round trip;
- selection remains correct across discovery reorder, rename, and IP change;
- staged checkbox edits produce no backend command until Apply;
- Apply sends one complete member set and causes one desired revision;
- Discard restores the latest desired set;
- dirty staging survives discovery changes and detects external desired revision;
- desired members remain selected while offline; active members do not;
- stale generation results cannot overwrite a newer snapshot;
- rapid desired/run commands coalesce to the last values;
- bounded queue full/closed behavior is typed and non-blocking.

### Discovery

- AirPlay then RAOP resolution emits one stable receiver with merged metadata;
- RAOP removal does not remove a present AirPlay receiver;
- AirPlay removal makes the receiver uncastable while retaining stable identity;
- address update emits Updated for the same ID;
- dropped browse stream stops both registrations;
- scan and browse do not stop each other;
- daemon error reaches the supervisor and triggers deterministic backoff;
- network generation replacement rejects old discovery events.

### Persistence and volume

- complete schema round trip and deterministic ordering;
- invalid IDs, volumes, duplicate members, and invalid group names are rejected;
- corrupt-file rename and default recovery preserve evidence;
- temporary write failure leaves the previous file valid;
- legacy volume migration occurs once and leaves the old file untouched;
- startup restores desired members and remains stopped when auto-connect is off;
- explicit auto-connect begins only after safe initial discovery and never picks
  an unrelated receiver;
- effective volume accounts for master, receiver level, and mute;
- endpoint preference, latency preset, mute, and auto-connect round trip;
- targeted receiver volume failure does not prevent other updates.

### Group lifecycle

- secondary setup failure allows later receivers and returns member context;
- primary setup failure cleans up and elects the next capable receiver;
- one successful PTP candidate is reconnected through NTP;
- no successful candidate produces Failed while preserving desired membership;
- every partial setup path cancels and joins created workers;
- secondary runtime failure leaves survivors active and marks Degraded;
- primary runtime failure creates a new generation and full restart;
- rejoin waits for discovery stability and observes the 30-second restart limit;
- a new Apply during setup cancels the old generation and starts only the latest;
- repeated start/stop returns thread/task/socket counts to baseline.

### Capture and platform recovery

- capture readiness precedes Capturing state;
- worker failure switches to correctly paced silence without ending the session;
- replacement worker resumes PCM without changing session generation;
- default endpoint follows Windows changes; explicit endpoint loss stays visible
  and does not silently select another source;
- endpoint selection restarts capture without changing session generation;
- an enabled latency-preset change requests exactly one session restart;
- full PCM queue drops stale data and records the count;
- suspend performs bounded shutdown and retains desired/run intent;
- resume invalidates old network/session work and restores a running intent;
- interface change forces new discovery and session generations;
- persistence or discovery failure never blocks the audio actor.

All unit and integration tests use fake clock, discovery, capture, persistence,
and receiver transports. Hardware-independent CI must run with no AirPlay
receiver, audio device, network transition, or interactive window.

## Hardware acceptance

Release acceptance requires Windows 10 or 11 and at least three physical
AirPlay 2 receivers, including two HomePods:

1. Save a three-receiver group, restart OpenAirCast with auto-connect disabled,
   and verify the group, endpoint preference, latency preset, mute state, and
   per-receiver levels return while playback remains stopped. Then enable
   auto-connect and verify a later launch starts only the saved desired group
   after initial discovery.
2. Stage three checkbox changes, verify current playback is uninterrupted, then
   Apply and observe exactly one group restart.
3. Stream real Windows audio to three receivers for 30 minutes with no process
   restart, runaway worker growth, or receiver session timeout.
4. Power off a secondary. Other receivers must continue; the shell must show
   Degraded within 10 seconds and preserve the offline receiver as selected.
5. Power the secondary on. It must be rediscovered and rejoin through one
   announced full restart within 60 seconds. Healthy receivers may experience
   that one restart but no repeated restart loop.
6. Power off the primary. A new primary or single-receiver fallback must resume
   audio within 60 seconds and expose the restart reason.
7. Change the Windows default render endpoint during playback. AirPlay sessions
   must remain in the same generation, silence must bridge the gap, and audio
   must resume within 10 seconds after the new endpoint becomes usable.
8. Disable and re-enable the active network interface. The application must
   recover the desired group within 90 seconds without process restart.
9. Suspend Windows for at least 60 seconds and resume. Old sessions must not be
   reused; the desired group must recover within 90 seconds when run intent was
   Running.
10. Change master and individual receiver levels during playback and verify only
    intended receivers change, without group restart.
11. Toggle mute and change between default and an explicit render endpoint;
    volume state must return correctly and capture must recover without a group
    restart. Apply every enabled latency preset and verify one announced restart
    per change with no unsupported synchronization or latency claim.
12. Enable start-with-Windows, sign out and back in, and verify exactly one hidden
    or visible OpenAirCast process starts according to the shell preference;
    auto-connect independently determines whether audio starts.
13. Repeat start/stop and two-to-three-member Apply cycles 25 times. Worker and
    handle counts after the final stop must return to the initial stopped
    baseline, allowing only the permanent shell/backend/discovery workers.

Logs and diagnostic events must identify every restart reason, primary election,
failed member, retry, capture recovery, and queue lag during these runs.

## Dependencies and risks

Subproject 2 depends on subproject 1 providing:

- the eframe application lifetime and non-blocking polling of watch/broadcast
  updates;
- the receiver checklist, persistent Apply/Discard change bar, saved-group UI,
  desired-versus-playing presentation, and user-visible notices;
- tray/hotkey commands mapped to `RunIntent` without mutating membership;
- Windows suspend/resume forwarding from the shell's native window integration;
- orderly backend shutdown before process exit.

The principal protocol dependency is the current upstream group API. Safe
partial setup, receiver-addressable volume/probe results, and deterministic
control-task shutdown must land before best-effort recovery can be claimed.
Dynamic target insertion remains absent; therefore secondary rejoin and all
membership changes retain the explicit full-restart behavior.

The PTP primary remains a shared failure domain. UDP sends cannot establish
receiver reachability, Windows firewall/interface behavior varies, and HomePod
setup duration can exceed local test timings. Recovery decisions therefore use
typed RTSP probes, discovery, interface state, conservative backoff, and hardware
acceptance rather than invented health or synchronization metrics.

Plaintext upstream persistent pairing identities are a separate security concern.
This subproject neither expands their scope nor places them in application state
or diagnostics.

## Completion criteria

Subproject 2 is complete when the public backend contract is integrated with the
subproject 1 shell, all automated tests above pass, the forced process-exit and
leaked-listener workarounds are removed, persisted state is migration-safe, and
the hardware acceptance sequence passes with diagnostic evidence. Subproject 3
can then build diagnostics entirely from the defined snapshot and diagnostic
feeds without reaching into transport internals.
