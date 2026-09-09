# OpenAirCast Diagnostics and Calibration Control Center Design

Date: 2026-08-22
Status: Approved design for subproject 3
Predecessors: subproject 1, Windows Calm application shell; subproject 2,
receiver lifecycle and resilience

## Purpose and scope

Subproject 3 adds a complete, truthful diagnostics and manual calibration
control center to the Windows application. It makes the sender's actual state
observable without adding work to real-time audio or timing paths, gives each
receiver a stable diagnostic identity across discovery and reconnects, and
supports deliberate per-receiver presentation delay while preserving the
shared AirPlay clock and RTP media timeline.

The deliverable contains five Windows Calm pages: Overview, Speakers,
Calibration, Events, and Advanced. It also supplies a versioned, redacted
support export. Collection is always available while the process runs; opening
the window only starts UI polling and does not enable a different streaming
code path.

This subproject does not implement automatic acoustic measurement, infer
receiver-side buffer occupancy, or turn retransmit requests into a claimed
packet-loss rate. It does not replace the receiver state machine, reconnect
policy, or main-window navigation owned by subprojects 1 and 2.

## Dependencies on subprojects 1 and 2

Subproject 1 owns the Windows Calm top-level window, navigation, typography,
spacing, color tokens, focus behavior, high-contrast behavior, reduced-motion
behavior, and the rule that closing the window leaves the tray process running.
Subproject 3 registers its five pages in that shell and uses the existing Inter
Variable, pale-surface, indigo, and teal visual vocabulary. It does not add a
second visual system or an independent nested application shell.

Subproject 2 owns the authoritative receiver lifecycle. It must provide:

- an opaque `ReceiverId` which remains stable across discovery refreshes,
  address changes, reconnects, and application restarts;
- a fresh `SessionId` for every group start or restart, never reused by a later
  session;
- authoritative discovery, pairing, setup, ready, streaming, degraded,
  reconnecting, failed, and stopped state;
- receiver-scoped error categories and the existing controlled full-group
  restart used when membership or a restart-only setting changes.

Its `DeviceBackendUpdates::diagnostics` source is consumed by one application
`DiagnosticsRegistry`. The shell reducer may copy bounded health summaries into
`UiSnapshot`, but views never receive `DeviceBackendHandle` or `DeviceSnapshot`.
Subproject 3 extends the existing root `AppHandle` with a read-only
`diagnostics_handle()` accessor; the returned `DiagnosticsHandle` is scoped to
the diagnostics pages and cannot issue transport commands.

Diagnostics consume those contracts. They must not derive identity from a
receiver's vector index, display name, current IP address, or PTP role. Every
receiver observation is keyed by `ReceiverSessionKey { session_id,
receiver_id }`. Events outside a streaming session, such as discovery failure
or export failure, have no `ReceiverSessionKey`.

Calibration uses the controlled restart entry point from subproject 2. It does
not stop, reconnect, or partially rejoin receivers directly. If subproject 2
reports a receiver as reconnecting or failed, the corresponding diagnostic row
shows that authoritative state and does not invent a transport state from
missing packets.

## Source-level audit and metric truth model

### Timing

The active multi-receiver path creates one PTP coordinator on the primary
`Connection`. `run_bmca_yield_flow` publishes continuously updated
`ClockOffset` values through a Tokio watch channel. Secondary connections copy
that same channel; they do not make independent timing measurements. The
current `Connection::timing_offset()` is only the initial value, while
`timing_rx()` carries live updates.

The PTP four-timestamp calculation is defined as:

```text
t1 = master transmit time from Follow_Up
t2 = local receive time for Sync
t3 = local transmit time for Delay_Req
t4 = master receive time from Delay_Resp

offset_local_minus_master_ns = ((t2 - t1) + (t3 - t4)) / 2
mean_path_delay_ns           = ((t2 - t1) - (t3 - t4)) / 2
```

`offset_local_minus_master_ns` is signed nanoseconds. A positive value means
the local sender clock is ahead of the PTP master. Conversion from local time
to master time therefore subtracts this offset. This explicit name replaces
the ambiguous legacy `ClockOffset` sign description at diagnostic boundaries.
The implementation must lock this convention with calculation and conversion
tests before changing any production timestamp application.

`mean_path_delay_ns` is a software-timestamped, symmetric-path estimate. The
legacy PTP code stores it in `ClockOffset.rtt_ns`; the UI and export must not
call it RTT. The legacy `error_ns = mean_path_delay / 2` assignment is not a
validated measurement uncertainty and is not shown as clock accuracy.

PTP drift is derived rather than received. The collector keeps at most 256
valid offset samples from the latest 30 seconds, each stamped with a monotonic
local observation time. With at least eight samples spanning at least two
seconds, ordinary least-squares slope produces:

```text
drift_ppm = slope(offset_local_minus_master_ns / elapsed_local_ns) * 1_000_000
```

The snapshot also reports sample count, window duration, and residual RMS in
nanoseconds. The label is **estimated relative clock drift**, never hardware
clock drift or servo accuracy. Network asymmetry, Windows scheduling, and
software receive/transmit timestamping contribute to both offset and drift.
The metric is unavailable rather than zero when the sample requirements are
not met.

For a single receiver, OpenAirCast runs `NtpTimingServer` and declares the
sender to be the time reference. It does not run `NtpTimingClient`. The
receiver owns the fourth timestamp required to calculate its offset and RTT,
so the application reports `SenderReference` and leaves offset, path delay,
RTT, and drift unavailable. A displayed zero would falsely imply measurement.

Each timing snapshot has a monotonically increasing measurement sequence and
sample age derived from a monotonic clock. Secondary receiver rows identify
the primary timing source and set `independently_measured` to false. They do
not repeat the shared estimate as if it were a per-speaker measurement.

### Scheduler jitter

The existing dedicated sender thread already measures the interval immediately
before dispatching an RTP burst. For a burst containing `n` audio frames:

```text
target_interval_ns = frames_per_packet * 1_000_000_000 / sample_rate * n
jitter_ns          = actual_dispatch_interval_ns - target_interval_ns
```

At the current 352 frames and 44,100 Hz, one target interval is approximately
7.9819 ms, not exactly 8 ms. The first dispatch and the first dispatch after a
pause, buffer stall, or deadline reset are excluded because they have no
continuous predecessor.

The session snapshot contains sample count, last signed jitter, mean absolute
jitter, nearest-rank p95 absolute jitter, maximum absolute jitter, and deadline
reset count, all in nanoseconds. A bounded 1,024-sample rolling window is used
for p95; cumulative count, mean, and maximum cover the current session. This is
local sender dispatch jitter before `send_to`, not network arrival jitter,
receiver jitter, or acoustic skew.

The existing 1 ms and 2 ms trace thresholds are debugging heuristics. They are
not product health thresholds. Historical observations about a 70 ms HomePod
buffer are not receiver telemetry and must not drive a red or green badge.

### Sender buffer and capture queue

`AudioBuffer` is the shared decoded PCM buffer before encoding. Its truthful
snapshot is:

```rust
pub struct SenderBufferSnapshot {
    pub queued_frames: u32,
    pub capacity_frames: u32,
    pub queued_samples_per_channel: u64,
    pub buffered_ns: u64,
    pub fill_ratio: f32,
    pub samples_written_total: u64,
    pub samples_read_total: u64,
    pub underrun_events_total: u64,
}
```

`buffered_ns` is derived from actual queued samples and sample rate, not from a
claimed receiver buffer. `fill_ratio` is in the inclusive range 0.0 through
1.0. The current approximately two-second capacity and the 50%, 40%, and 10%
startup/refill state thresholds may be drawn as implementation markers, but
they are not universal safe limits.

WASAPI capture feeds a bounded `LiveFrameSender` queue with capacity 32 in the
current application. Every non-blocking submission records queue length,
capacity, submitted frames and samples, full-queue drops, and disconnected
drops. HomePodCast currently ignores the false return from `try_send`; this
subproject makes those losses visible without changing the non-blocking
contract. Capture queue depth is distinct from sender PCM buffer depth.

### RTP, datagrams, retransmits, and feedback

One encoded RTP media frame is shared across the group, while every receiver
has its own encryption, UDP socket, and packet history. Counter names must
reflect those layers:

- `rtp_frames_prepared_total`: shared audio frames encoded and prepared once;
- `data_datagrams_attempted_total`: receiver-specific UDP audio send attempts;
- `data_datagrams_accepted_local_total`: receiver-specific `send_to` calls
  accepted by the local operating system;
- `data_datagram_send_failures_total`: receiver-specific `send_to` failures;
- `data_bytes_accepted_local_total`: bytes returned as sent by successful local
  `send_to` calls;
- equivalent `sync_datagrams_*` counters for PT=84/PT=87 control sends.

`rtp_frames_prepared_total` is not a wire packet count. A successful local UDP
send is not proof that the receiver received or rendered the packet. The legacy
`AudioStreamer::packets_sent()` increments before real sends on the normal
sender-thread path and therefore remains only a compatibility value; new UI and
export use the explicit counters.

Retransmit accounting is receiver-specific:

- `retransmit_request_datagrams_total`: valid PT=85 requests received;
- `retransmit_packet_slots_requested_total`: sum of every request's `count`;
- `retransmit_datagrams_accepted_local_total`: requested history packets whose
  PT=86 response was accepted by the local OS;
- `retransmit_history_misses_total`: requested sequence slots no longer or not
  yet present in that receiver's history;
- `retransmit_send_failures_total`: replies that failed at `send_to`.

Requests are not deduplicated because a receiver may request the same sequence
again. `requested slots / shared RTP frames prepared while the receiver was in
the session` may be shown as a **recovery-demand ratio** with that denominator
spelled out. It is never packet loss. Aggregate group demand is not divided by
the one shared frame count because that would scale with group size and could
exceed 100%.

Feedback accounting records attempts, successful RTSP responses, protocol or
transport failures, timeouts, last result, and last transaction duration in
nanoseconds. The existing two-second timeout must remain distinguishable from
success even though feedback timeout does not tear down a session by itself.
Transaction duration includes TCP, RTSP handling, scheduling, and receiver
processing; it is labelled control-request duration, not network RTT.

### State and errors

The snapshot carries the authoritative subproject 2 lifecycle state plus RTSP
session state, playback state, streamer state, timing role, and last successful
feedback time. It does not reduce all failures to `Streaming` or `Stopped`.

Diagnostic errors are structured at the point where context is still known:

```rust
pub struct DiagnosticError {
    pub code: DiagnosticErrorCode,
    pub component: DiagnosticComponent,
    pub severity: DiagnosticSeverity,
    pub recoverability: Recoverability,
    pub operation: &'static str,
    pub receiver_session: Option<ReceiverSessionKey>,
    pub public_message: String,
    pub technical_detail: Option<String>,
}

pub enum DiagnosticComponent {
    Discovery, Pairing, Rtsp, Timing, Capture, Buffer, Encoder,
    Scheduler, UdpTransport, Retransmit, Feedback, Calibration, Export,
}

pub enum DiagnosticSeverity { Info, Warning, Error }

pub enum Recoverability {
    Transient, AutomaticRetry, RequiresSessionRestart, UserAction, Fatal,
}
```

`DiagnosticErrorCode` has stable variants for discovery failure, pairing
failure, setup rejection, setup timeout, event-channel failure, PTP bind
fallback, PTP sample stale, capture failure, capture queue full, buffer
underrun, encode failure, sender queue disconnected, UDP send failure,
retransmit history miss, feedback failure, feedback timeout, teardown timeout,
calibration apply failure, and export failure. Display and export switch on the
code. They do not parse free-form tracing text.

## Diagnostics collection contract

The public read contract is synchronous and cloneable:

```rust
#[derive(Clone)]
pub struct DiagnosticsHandle { /* shared read handle */ }

impl DiagnosticsHandle {
    pub fn snapshot(&self) -> DiagnosticsSnapshot;
    pub fn events_since(&self, cursor: EventCursor, limit: usize) -> EventBatch;
}

pub struct DiagnosticsSnapshot {
    pub schema_version: u16,
    pub snapshot_sequence: u64,
    pub captured_at_utc: SystemTime,
    pub process_elapsed_ns: u64,
    pub active_session_id: Option<SessionId>,
    pub session: Option<SessionDiagnosticsSnapshot>,
    pub receivers: Vec<ReceiverDiagnosticsSnapshot>,
    pub diagnostics_events_dropped_total: u64,
}

pub struct ReceiverDiagnosticsSnapshot {
    pub key: ReceiverSessionKey,
    pub lifecycle: ReceiverLifecycleState,
    pub timing: ReceiverTimingSnapshot,
    pub transport: ReceiverTransportSnapshot,
    pub feedback: FeedbackSnapshot,
    pub last_error: Option<DiagnosticError>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventCursor(pub u64);

pub struct EventBatch {
    pub events: Vec<DiagnosticEvent>,
    pub next_cursor: EventCursor,
    pub oldest_available_cursor: EventCursor,
    pub gap: Option<EventGap>,
}

pub struct EventGap {
    pub requested_cursor: EventCursor,
    pub resumed_at_cursor: EventCursor,
    pub overwritten_events: u64,
}
```

`schema_version` is 1 for this contract. `snapshot_sequence` and event cursors
increase for the lifetime of the process and never move backwards. Receiver
vectors are sorted by stable `ReceiverId`, not current sender index.

The event ring holds the latest 4,096 significant events across current and
recent sessions. `events_since` clamps `limit` to 1 through 512. It returns
events with cursor strictly greater than the supplied cursor and sets
`next_cursor` to the last returned cursor, or leaves it unchanged if no event
was available. If the caller is behind the oldest retained event, the batch
starts at the oldest retained event and returns an explicit `EventGap`; gaps
are never silent. Timing samples and normal per-packet sends are aggregated
metrics, not events.

Real-time writers update atomics or bounded accumulators. Significant events
use a bounded non-blocking producer path; a full diagnostic queue increments
`diagnostics_events_dropped_total` and drops the diagnostic event rather than
waiting. No audio sender, capture, PTP, retransmit, or feedback path awaits a UI
consumer, performs file I/O, serializes JSON, or acquires an application-window
lock.

The visible control center uses one 250 ms timer, so it polls at no more than
4 Hz. One tick reads one snapshot and one event batch. Hidden, minimized, and
closed diagnostics pages do not poll. Counter collection continues because it
is part of the transport objects, not the UI. Rendering must coalesce stale
ticks rather than queue updates.

## Windows Calm information architecture

The Diagnostics navigation item opens the Overview page. A secondary page bar
contains Overview, Speakers, Calibration, Events, and Advanced. The page bar
uses the shell's existing navigation and keyboard/focus behavior. Technical
numbers use the shell's monospaced metric style; body copy and actions use Inter
Variable.

The overall status badge is conservative:

- Error when an authoritative receiver/session state is failed or the current
  streamer has a terminal error.
- Attention when new capture drops, buffer underruns, UDP send failures,
  retransmit history misses, feedback failures, or a stale PTP source exist.
- Running when the stream is active and no definite problem is present.
- Unknown when no current session or sufficient measurement exists.

It must say `PTP clock active` or `shared PTP source`, not `synchronized`,
because the application has no acoustic measurement.

### Overview

Overview answers whether the current session is running and where a definite
problem occurred. It shows active session duration and receiver count, shared
sender-buffer depth, capture queue and drops, local scheduler jitter, shared RTP
frames prepared, receiver-specific local UDP failure total, retransmit recovery
demand, and the latest actionable structured error. A compact signal-path row
labels each boundary: WASAPI capture, capture queue, sender PCM buffer, ALAC,
local scheduler, and receiver transports.

The page uses plain-language labels first. Hover/help text gives the exact
metric definition and unit. A value without a valid source is `Not measured`,
not zero. `Copy summary` creates the same redacted representation used by the
export.

### Speakers

Speakers presents one stable row per receiver in the active or most recent
session. Columns are receiver name, authoritative lifecycle state, timing
source, local data datagrams accepted/failed, recovery demand and fulfilment,
feedback result, and last error. Expanding a row shows exact counters and the
receiver's `ReceiverSessionKey` alias.

For the PTP primary, the row says `Measured against this PTP master` and exposes
the latest sample age. Secondary rows say `Uses shared primary measurement` and
name the source receiver. They do not display separate offset or drift values.
Single-receiver NTP says `Sender is time reference; receiver offset not
measured`.

### Calibration

Calibration is enabled only for an active selection of at least two receivers.
The user selects a reference receiver, adjusts signed relative delay in
milliseconds with 0.1 ms display precision, runs a low-level shared click test,
and reviews the effective non-negative delays before applying. Values are saved
by stable `ReceiverId`, never by speaker name, IP address, or list position.

The click test is four shared PCM clicks, 500 ms apart, at a conservative fixed
digital level. It traverses the same shared encode and RTP fan-out path as
normal audio. The UI states that the user is listening for repeated/echoed
onsets and that room reflections limit judgment. Test samples and captured
audio are never logged or exported.

`Apply` records the calibration profile and requests one controlled full-group
restart. The page shows `Pending restart`, `Applied to session <alias>`, or a
structured apply failure. There is no live retiming slider. Reset sets all
relative values to zero and also applies through a controlled restart.

### Events

Events reads the bounded cursor stream and shows UTC time, session-relative
time, severity, component, receiver alias, stable code, and public message.
Filters operate locally by severity, component, and receiver. A visible gap row
reports overwritten or producer-dropped events. Clearing the view only moves
the page cursor; it does not mutate transport counters or the process event
ring.

### Advanced

Advanced exposes protocol and format, PTP role, explicit offset convention,
mean path-delay estimate, offset sample age, drift estimate and fit quality,
scheduler distribution, requested latency range, configured render lead,
sender and capture queue capacities, counter start time, and diagnostic event
drop count. Requested latency values are labelled sender requests, not measured
receiver latency. This page contains no packet payload, cryptographic value, or
raw RTSP body.

## Manual presentation-time calibration

Calibration preserves the PTP clock and RTP content timeline. For active
receiver `i`, let `requested_i` be the user's signed relative delay. Before a
session starts:

```text
minimum_requested = min(requested_i for every active receiver)
effective_delay_i  = requested_i - minimum_requested

presentation_time_i = common_master_time
                    + global_render_lead
                    + effective_delay_i
```

Every `effective_delay_i` is non-negative and pairwise differences are
preserved. Changing group membership may add a common shift after
renormalization, but relative alignment among remaining receivers is stable.
The preview shows both requested relative and effective added delay.

The shared ALAC payload, RTP sequence, current RTP timestamp, next RTP
timestamp, PTP master identity, and measured PTP offset remain identical in
meaning across receivers. Only the presentation clock written into that
receiver's PT=84 or PT=87 sync packet changes. Audio datagrams continue to be
dispatched on the common sender schedule; delaying UDP transmission itself is
forbidden because it would consume recovery headroom and create avoidable
packet lateness.

The current sender prepares one sync packet from the first `RtpSender` and
broadcasts it. Calibration changes the sender message from one optional shared
sync payload to a vector with one optional sync payload per target. Each
`RtpSender` prepares its own sync packet so its sync sequence and first-sync
extension state advance correctly. The sender thread sends each sync only to
the corresponding target.

Calibration is restart-only. A full setup/FLUSH/RECORD boundary establishes a
new presentation mapping and correct first audio marker/sync extension state.
No calibration value modifies `ClockOffset`, BMCA, Delay_Req/Delay_Resp,
sample-rate conversion, audio content, packet-history indexing, or reconnect
policy.

The application validates checked arithmetic and refuses a mapping whose
presentation time is not in the future at session start. Negotiated
`latencyMin` and `latencyMax` are displayed as requests and may inform a warning,
but they are not treated as proof of HomePod buffer capacity. The design makes
no universal safe offset claim. Hardware validation determines practical
limits for supported receiver/firmware combinations.

Automatic acoustic calibration is outside scope. It would require microphone
selection and permission, acoustic isolation of each speaker, emitted-signal
correlation, room-reflection handling, and a separate privacy design. Manual
settings compensate perceived arrival only to the quality the user or an
external recorder can verify.

## Structured logging and support export

Significant diagnostic events use structured fields matching
`DiagnosticEvent`; human tracing text is secondary. Existing diagnostic traces
that include PCM samples, ALAC bytes, packet hex, nonce/tag bytes, IP addresses,
or clock identities are not support-safe and must be removed from normal builds
or gated behind an explicit developer-only capture mode unavailable from the
standard UI.

`Export diagnostics` writes one UTF-8 JSON document with media type
`application/vnd.openaircast.diagnostics+json` and this top-level shape:

```json
{
  "schema": "openaircast.diagnostics",
  "schema_version": 1,
  "exported_at_utc": "RFC3339 timestamp",
  "redaction": { "mode": "support-safe", "receiver_aliases": "per-export" },
  "application": {},
  "configuration": {},
  "latest_snapshot": {},
  "events": [],
  "event_gap": null
}
```

The export contains application version/build, Windows version, audio format,
timing mode, requested latency, configured render lead, calibration effective
delays, exact metric units/semantics, latest snapshot, and all retained events.
It records whether a cursor gap or producer drop occurred.

Support export is always redacted; the standard UI has no unredacted option.
Each encountered receiver becomes `R-001`, `R-002`, and so on for that export.
The file omits raw `ReceiverId`, device name, MAC, IP address, serial number,
pairing and group identities, PTP clock identity, SSRC, RTSP session ID, local
username, filesystem path, and network port. It also omits RTSP headers and
bodies, pairing material, cryptographic keys, payloads, PCM samples, encoded
audio, packet hex, nonces, and authentication tags. Model and firmware version
may remain because they are needed for compatibility diagnosis.

`technical_detail` is sanitized before export and may not contain a raw error
chain when that chain embeds an address, identity, or path. `public_message`,
stable error code, operation, component, recoverability, counters, relative
times, and units remain. Export performs all serialization and file I/O away
from capture, sender, PTP, retransmit, and feedback threads.

## Health interpretation and prohibited claims

Definite events such as a capture drop, buffer underrun, UDP send failure,
retransmit history miss, setup failure, or terminal streamer error may create
an Attention or Error state. PTP offset magnitude, estimated drift, mean path
delay, scheduler p95, and recovery-demand ratio remain informational until
repeatable hardware baselines establish receiver-specific limits.

A timing source becomes stale when no new sample arrives for the greater of two
seconds or five times the rolling median valid sample interval. Staleness says
only that sender-side timing observations stopped. It does not prove that audio
is currently out of sync.

OpenAirCast and its export must not claim:

- measured packet loss or confirmed receiver delivery;
- receiver-side or HomePod buffer occupancy;
- network or receiver jitter from sender dispatch jitter;
- true PTP RTT from the current mean-path-delay field;
- PTP or clock accuracy from `error_ns`;
- an independent offset or drift for secondary speakers;
- measured end-to-end, capture-to-speaker, or acoustic latency;
- measured acoustic inter-speaker skew or guaranteed synchronization;
- that requested latency or render lead equals actual receiver latency;
- automatic acoustic calibration;
- that a successful UDP `send_to` means the receiver rendered the packet.

The existing three-HomePod, 50-second validation demonstrates real capture,
shared encoding, PTP participation, local scheduling, and encrypted fan-out. It
does not provide acoustic-skew, long-duration, per-receiver latency, or
receiver-delivery evidence.

## File and interface map

The implementation plan for this design should use these boundaries:

- Create `crates/airplay-timing/src/diagnostics.rs` for
  `TimingMeasurement`, rolling drift calculation, sample age, and explicit PTP
  semantics. Modify `ptp.rs`, `ntp.rs`, `clock.rs`, and `lib.rs` to publish and
  export the timing handle.
- Create `crates/airplay-audio/src/diagnostics.rs` for audio atomics, bounded
  jitter accumulation, sender-buffer and capture-queue snapshots, and
  per-target transport counters. Modify `buffer.rs`, `live_decoder.rs`,
  `rtp.rs`, `streamer.rs`, and `lib.rs` at their existing measurement points.
- Create `crates/airplay-client/src/diagnostics.rs` for cloneable
  `ClientDiagnosticsSource` values, typed client errors, per-connection transport
  observations, and mapping sender indices back to stable `DeviceId`. Modify
  `stats.rs`, `events.rs`, `connection.rs`, `client.rs`, and `lib.rs`. Keep the
  existing public stats API compatible, but do not use `loss_percent()` in the
  control center.
- Create `crates/homepod-cast/src/diagnostics.rs` for `DiagnosticsRegistry`, the
  UI-facing `DiagnosticsHandle`, `DiagnosticsSnapshot`, `ReceiverSessionKey`,
  the 4,096-event ring, cursor batches, aggregation of registered timing/audio/
  client sources, the application view model, health interpretation, redaction,
  copy summary, and JSON export.
- Create `crates/homepod-cast/src/calibration.rs` for stable persisted profiles,
  normalization, validation, restart request, and click-test generation.
- Create `crates/homepod-cast/src/diagnostics_window.rs` for the Windows Calm
  Overview, Speakers, Calibration, Events, and Advanced pages and their single
  250 ms visible-window timer. Modify the shell integration established by
  subproject 1, plus current `main.rs`, `cast.rs`, and `tray.rs`, without moving
  transport ownership onto the UI thread.
- Modify `crates/homepod-cast/Cargo.toml` only for serialization or Windows UI
  dependencies not already supplied by the shell. Persist calibration through
  the device-state repository established by subproject 2, using a versioned
  schema migration under `%APPDATA%\OpenAirCast`; never duplicate it in the
  shell's `settings.json`.

`Connection` and `AirPlayClient` each expose a cloneable internal
`diagnostics_source()`. `cast::Session` retains and registers that source when choosing
either single or group transport. The application registry exposes exactly one
read-only `DiagnosticsHandle` through `AppHandle::diagnostics_handle()`, so the
UI consumes one application-level contract. Diagnostics do not expose RTP
senders, sockets, packet histories, cryptographic state, or mutable transport
internals.

## Automated verification

Unit and deterministic integration tests must cover:

- PTP formula, explicit sign convention, and local-to-master conversion;
- PTP mean path delay not being serialized or labelled as RTT;
- drift unavailable below eight samples or two seconds, zero for constant
  offset, and correct ppm for a known linear offset slope;
- stale timing based on `max(2 s, 5 * median interval)` and recovery on a new
  sample;
- scheduler target interval at 352/44,100, signed jitter, rolling nearest-rank
  p95, mean absolute, maximum, and reset exclusions;
- sender buffer frames, actual sample duration, fill ratio, and underrun count;
- capture queue submission, full drop, disconnected drop, and non-blocking
  behavior;
- one shared RTP frame producing one prepared-frame count and one local
  datagram result per target;
- UDP success/failure and byte counters without claiming receiver delivery;
- retransmit request datagrams, requested slots, repeated requests, fulfilment,
  history miss, and send failure;
- feedback success, protocol failure, timeout, and transaction duration;
- receiver mapping by stable `ReceiverId` when discovery order changes;
- secondary timing marked shared and single NTP marked sender-reference with
  unavailable offset/RTT;
- event cursor ordering, 4,096-event overwrite gap, 512-result clamp, producer
  drop counter, session keying, and stopped-session retention;
- calibration normalization preserving pairwise differences and producing only
  non-negative effective delays;
- calibration changing only per-target sync presentation time while preserving
  common RTP timestamps, PTP offset, clock identity, and audio payload;
- calibration apply and reset requesting one controlled full restart;
- export schema version, exact units, stable per-export aliases, and removal of
  every prohibited identifier/content field from snapshot, event, and error
  detail fixtures;
- view-model states for no session, single NTP, healthy group PTP, shared
  secondary timing, stale timing, capture drop, UDP failure, reconnecting, and
  terminal error;
- a timer/polling test proving the visible window never exceeds 4 Hz and hidden
  windows do not poll.

Repository verification runs `cargo fmt --all -- --check`,
`cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets
-- -D warnings`, and `cargo build -p homepod-cast --release` on the supported
Windows MSVC toolchain.

## Hardware acceptance

Hardware acceptance uses at least three discovered HomePods and retains raw
test notes outside the support-safe export when network identity is required.
The release gate is:

1. A single-receiver session shows NTP sender-reference semantics and never
   displays offset, RTT, drift, or a synchronization claim.
2. A three-receiver group runs for at least 30 minutes while the diagnostics
   window remains open. The primary produces fresh PTP measurements, both
   secondary rows point to the same source as shared measurements, UI polling
   remains at or below 4 Hz, and opening/closing the window creates no audible
   discontinuity or new underrun.
3. Real Windows audio and silence exercise capture queue, sender buffer, ALAC,
   and per-target datagram counters. Counts obey their defined layer; shared
   frame count is not multiplied by receiver count and per-target datagrams are.
4. A controlled interruption of one receiver produces receiver-scoped
   transport/feedback errors and the subproject 2 lifecycle transition without
   falsely declaring group packet loss or acoustic desynchronization. Recovery
   retains `ReceiverId` and uses a new `SessionId` when the resilience policy
   restarts the session.
5. Applying a clearly audible test offset to one receiver, verified by a human
   listener and an external recording, moves that receiver's click onset in the
   requested direction. Reset restores the baseline mapping. This validates the
   mechanism only; no acoustic accuracy tolerance is advertised without a
   separate repeated measurement study.
6. Calibration survives application restart under the same stable
   `ReceiverId`, and a changed discovery order does not exchange profiles.
7. A support export made after timing, retransmit, feedback, calibration, and
   induced error events validates against schema version 1 and contains no
   device name, address, stable identity, path, packet/audio content, pairing
   material, or cryptographic value.

Completion of these checks permits the control center to report sender-side
transport health and manual presentation offsets. It does not permit a product
claim of measured packet delivery, measured end-to-end latency, or guaranteed
acoustic synchronization.
