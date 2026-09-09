# OpenAirCast Diagnostics and Calibration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a truthful, non-blocking diagnostics and manual per-receiver presentation-time calibration control center for OpenAirCast, including bounded cursor events, five Windows Calm pages, and a versioned support-safe export.

**Architecture:** Transport and timing crates publish typed, cloneable read sources backed by atomics and bounded accumulators; one application-level `DiagnosticsRegistry` consumes the subproject 2 diagnostic feed, registers session sources by stable identity, publishes immutable snapshots, and owns the bounded event ring. Calibration is persisted by `ReceiverId`, normalized to non-negative per-target presentation delays, and applied only through the resilience backend's controlled full-group restart while shared RTP audio and PTP meaning remain unchanged.

**Tech Stack:** Rust 2021, Tokio, crossbeam-channel, serde/serde_json, arc-swap, eframe/egui `=0.36.1` with wgpu, Windows MSVC, existing OpenAirCast backend and Windows Calm shell.

**Spec:** `docs/superpowers/specs/2026-08-22-openaircast-diagnostics-calibration-design.md`

## Global Constraints

- Execute this plan only after the control-center shell and device-resilience subprojects are present and green. The prerequisite files are `crates/homepod-cast/src/app_handle.rs`, `crates/homepod-cast/src/app/mod.rs`, `crates/homepod-cast/src/backend/model.rs`, `crates/homepod-cast/src/backend/event.rs`, `crates/homepod-cast/src/backend/session.rs`, and `crates/homepod-cast/src/backend/persistence.rs`. If any are absent, stop and implement the predecessor plans; do not invent a parallel shell, backend, receiver ID, session generation, persistence file, or restart path.
- Preserve the four existing `AppHandle` methods and add only the read-only `diagnostics_handle()` accessor. Diagnostics views never receive `DeviceBackendHandle`, sockets, RTP senders, packet histories, cryptographic state, or mutable transport state.
- Key every in-session observation by `ReceiverSessionKey { session_id, receiver_id }`. Never key or persist diagnostics/calibration by sender index, discovery order, display name, IP address, or PTP role. Sender indices may exist only inside the client/audio adapter and must be mapped to stable identity before crossing into application diagnostics.
- Use nanoseconds for stored durations and signed timing differences. Use exact 352/44,100 arithmetic, explicit unavailable values, checked integer conversion, and monotonically increasing process-lifetime snapshot/event sequences. Never encode unavailable measurements as zero.
- Real-time audio, PTP, retransmit, feedback, and capture writers may perform relaxed atomic updates, bounded in-memory accumulation, or non-blocking `try_send`; they may not wait for a diagnostics consumer, serialize JSON, perform file I/O, acquire a UI/application registry lock, or call egui.
- Keep the event ring at 4,096 entries, clamp public event reads to 1 through 512, make overwrite gaps explicit, and increment producer-drop counters instead of blocking. Timing samples and ordinary packet sends are metrics, not events.
- The UI performs at most one `snapshot()` and one `events_since()` call per 250 ms while the Diagnostics area is visible and the native window is neither hidden nor minimized. It performs neither call when hidden, minimized, or closed and coalesces elapsed ticks.
- Treat PTP offset, estimated drift, mean path delay, scheduler jitter, recovery demand, requested latency, and render lead as informational. Definite events may affect health; inferred thresholds may not. Use `PTP clock active`, `shared PTP source`, and `Not measured`; never claim synchronization, packet loss, receiver delivery, receiver buffer occupancy, network jitter, PTP accuracy, measured end-to-end/acoustic latency, or automatic acoustic calibration.
- Support export is always support-safe. It has no unredacted mode and never serializes raw domain structs. Build a dedicated allow-listed export DTO that omits receiver/device names and IDs, MAC/IP/ports, serial/group/pairing/PTP/SSRC/RTSP identities, paths/usernames, headers/bodies, keys, payload/audio bytes, packet hex, nonces, and authentication tags.
- Persist calibration by migrating the device-state schema at `%APPDATA%\OpenAirCast\state-v1.json`; do not add calibration to the shell's `settings.json`. Applying or resetting a profile persists first and requests exactly one controlled full-group restart.
- Preserve the capture bridge's specified 32-frame bounded queue unless a separately approved latency configuration changes it; report that actual capacity in snapshots and exports.
- Calibration changes only each target's PT=84/PT=87 presentation clock. It must not delay UDP data dispatch, change shared audio/RTP timestamps, mutate `ClockOffset`/BMCA/delay exchange, alter sample rate/content/history, or implement a live retiming slider.
- Follow red-green-refactor for every behavior below. Run the narrow failing test before production changes, make the smallest implementation pass, rerun the affected crate, then commit only the paths named by the task. Do not stage unrelated working-tree changes.
- All hardware-independent tests run without an AirPlay receiver, audio device, network transition, or visible interactive window. Use fake monotonic/wall clocks, local UDP fixtures, fake backend transports, and deterministic window visibility inputs.

---

### Task 1: Lock the explicit timing convention and calculation boundary

**Files:**
- Create: `crates/airplay-timing/src/diagnostics.rs`
- Create: `crates/airplay-timing/tests/diagnostics.rs`
- Modify: `crates/airplay-timing/src/clock.rs`
- Modify: `crates/airplay-timing/src/lib.rs`

**Interfaces:**
- Consumes: legacy `ClockOffset { offset_ns, error_ns, rtt_ns }` and the four PTP timestamps already parsed in `ptp.rs`.
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingMeasurement {
    pub measurement_sequence: u64,
    pub observed_elapsed_ns: u64,
    pub offset_local_minus_master_ns: i64,
    pub mean_path_delay_ns: u64,
}

pub fn ptp_measurement(
    measurement_sequence: u64,
    observed_elapsed_ns: u64,
    t1_master_tx_ns: u64,
    t2_local_rx_ns: u64,
    t3_local_tx_ns: u64,
    t4_master_rx_ns: u64,
) -> Option<TimingMeasurement>;

pub fn local_to_master_ns(
    local_ns: u64,
    offset_local_minus_master_ns: i64,
) -> Option<u64>;
```

- `ClockOffset.rtt_ns` remains source-compatible but is documented as the legacy storage slot for PTP mean path delay; `error_ns` is documented as legacy and never exported as accuracy.

- [ ] **Step 1 (2 min): Verify the predecessor gate.** Run:

```powershell
$required = @(
  'crates/homepod-cast/src/app_handle.rs',
  'crates/homepod-cast/src/app/mod.rs',
  'crates/homepod-cast/src/backend/model.rs',
  'crates/homepod-cast/src/backend/event.rs',
  'crates/homepod-cast/src/backend/session.rs',
  'crates/homepod-cast/src/backend/persistence.rs'
)
$missing = $required | Where-Object { -not (Test-Path -LiteralPath $_) }
if ($missing) { throw "Predecessor contracts missing: $($missing -join ', ')" }
cargo test -p homepod-cast --tests
```

- [ ] **Step 2 (4 min): Write failing sign and formula tests.** Add exact symmetric and non-zero-offset fixtures:

```rust
#[test]
fn timing_formula_uses_local_minus_master_sign() {
    let sample = ptp_measurement(7, 3_000_000_000, 1_000, 1_150, 1_250, 1_300).unwrap();
    assert_eq!(sample.offset_local_minus_master_ns, 50);
    assert_eq!(sample.mean_path_delay_ns, 100);
    assert_eq!(local_to_master_ns(10_000, 50), Some(9_950));
}

#[test]
fn checked_conversion_rejects_underflow() {
    assert_eq!(local_to_master_ns(10, 50), None);
}
```

- [ ] **Step 3 (2 min): Prove the tests fail for missing explicit APIs.** Run `cargo test -p airplay-timing --test diagnostics timing_formula_uses_local_minus_master_sign -- --exact` and confirm a compile failure naming `ptp_measurement`/`local_to_master_ns`.
- [ ] **Step 4 (5 min): Implement checked `i128` math.** Compute both formulas before narrowing and return `None` for negative path delay, timestamp subtraction overflow, or values that do not fit the public types:

```rust
let a = i128::from(t2_local_rx_ns) - i128::from(t1_master_tx_ns);
let b = i128::from(t3_local_tx_ns) - i128::from(t4_master_rx_ns);
let offset = (a + b) / 2;
let delay = (a - b) / 2;
```

- [ ] **Step 5 (3 min): Export the truthful API and correct only documentation at this boundary.** Add `pub mod diagnostics;` and re-exports in `lib.rs`; do not yet change stream timestamp application.
- [ ] **Step 6 (3 min): Run the focused and crate suites.** Run `cargo test -p airplay-timing --test diagnostics` and `cargo test -p airplay-timing`.
- [ ] **Step 7 (2 min): Commit the timing truth boundary.** Run:

```powershell
git add crates/airplay-timing/src/diagnostics.rs crates/airplay-timing/tests/diagnostics.rs crates/airplay-timing/src/clock.rs crates/airplay-timing/src/lib.rs
git commit -m "feat(timing): define truthful PTP measurements"
```

### Task 2: Add bounded drift, age, and staleness collection to timing sources

**Files:**
- Modify: `crates/airplay-timing/src/diagnostics.rs`
- Modify: `crates/airplay-timing/tests/diagnostics.rs`
- Modify: `crates/airplay-timing/src/ptp.rs`
- Modify: `crates/airplay-timing/src/ntp.rs`
- Modify: `crates/airplay-timing/src/lib.rs`

**Interfaces:**
- Consumes: Task 1's `ptp_measurement(...) -> Option<TimingMeasurement>` and `local_to_master_ns(u64, i64) -> Option<u64>`, plus the existing `ClockOffset` watch publication points in `ptp.rs` and the one-receiver `NtpTimingServer` path.
- Produces: `TimingDiagnosticsHandle::snapshot(now_elapsed_ns: u64) -> TimingDiagnosticsSnapshot` and the crate-private `timing_diagnostics_pair(reference: TimingReferenceKind) -> (TimingDiagnosticsRecorder, TimingDiagnosticsHandle)` shared by primary and secondary connections.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingReferenceKind { SenderReference, PtpMeasured }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriftEstimate {
    pub drift_ppm: f64,
    pub sample_count: u16,
    pub window_ns: u64,
    pub residual_rms_ns: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimingDiagnosticsSnapshot {
    pub reference: TimingReferenceKind,
    pub latest: Option<TimingMeasurement>,
    pub sample_age_ns: Option<u64>,
    pub stale: bool,
    pub drift: Option<DriftEstimate>,
}

#[derive(Clone)]
pub struct TimingDiagnosticsHandle { /* Arc-backed read source */ }

impl TimingDiagnosticsHandle {
    pub fn snapshot(&self, now_elapsed_ns: u64) -> TimingDiagnosticsSnapshot;
}

#[derive(Clone)]
pub(crate) struct TimingDiagnosticsRecorder { /* bounded writer */ }

pub(crate) fn timing_diagnostics_pair(
    reference: TimingReferenceKind,
) -> (TimingDiagnosticsRecorder, TimingDiagnosticsHandle);
```

- The recorder retains at most 256 valid samples and evicts observations older than 30 seconds. Drift requires at least 8 samples spanning at least 2 seconds. Stale is `age > max(2_000_000_000, 5 * rolling_median_interval_ns)`.

- [ ] **Step 1 (5 min): Add failing drift tests.** Cover fewer than eight samples, less than two seconds, constant offset, and a +10 ppm linear slope using injected elapsed nanoseconds; assert `sample_count`, `window_ns`, and residual RMS.
- [ ] **Step 2 (3 min): Add failing age/staleness tests.** Record intervals `[400, 500, 600] ms`, assert the median is 500 ms and staleness begins after 2.5 s; record a new sample and assert recovery.
- [ ] **Step 3 (2 min): Run the focused failures.** Run `cargo test -p airplay-timing --test diagnostics drift_` and `cargo test -p airplay-timing --test diagnostics stale_`; confirm missing collector behavior.
- [ ] **Step 4 (5 min): Implement the bounded sample window and ordinary least squares.** Center elapsed time at the first retained sample and use `f64` only for the regression/result:

```rust
let slope = sum_dx_dy / sum_dx2;
let drift_ppm = slope * 1_000_000.0;
let residual_rms_ns = (sum_squared_residuals / count as f64).sqrt();
```

Use nearest middle element of the sorted interval copy for an odd count and the integer midpoint for an even count.
- [ ] **Step 5 (4 min): Publish every valid PTP four-timestamp result.** Replace duplicated formula blocks in `PtpClient`, `PtpMaster`, `run_bmca_yield_flow`, and `run_ptp_slave` with `ptp_measurement`, increment the measurement sequence once per valid result, update the legacy `ClockOffset` watch value, and record the same sample in the timing recorder. Rename the current contradictory `master ahead/behind` unit tests to assert the explicit local-minus-master convention without changing the fixture timestamps.
- [ ] **Step 6 (3 min): Mark the one-receiver NTP server as `SenderReference`.** Its snapshot must keep `latest`, `sample_age_ns`, and `drift` as `None` and `stale` as false; do not manufacture a zero sample.
- [ ] **Step 7 (3 min): Expose a cloneable timing handle from the existing PTP/NTP owners.** The PTP coordinator and every copied secondary watch must point at the same timing handle; do not construct a recorder for each secondary.
- [ ] **Step 8 (3 min): Run timing tests.** Run `cargo test -p airplay-timing --test diagnostics` and `cargo test -p airplay-timing`.
- [ ] **Step 9 (2 min): Commit timing collection.** Run:

```powershell
git add crates/airplay-timing/src/diagnostics.rs crates/airplay-timing/tests/diagnostics.rs crates/airplay-timing/src/ptp.rs crates/airplay-timing/src/ntp.rs crates/airplay-timing/src/lib.rs
git commit -m "feat(timing): collect drift and sample freshness"
```

### Task 3: Make sender-buffer and capture-queue depth truthful

**Files:**
- Create: `crates/airplay-audio/src/diagnostics.rs`
- Create: `crates/airplay-audio/tests/diagnostics.rs`
- Modify: `crates/airplay-audio/src/buffer.rs`
- Modify: `crates/airplay-audio/src/live_decoder.rs`
- Modify: `crates/airplay-audio/src/lib.rs`

**Interfaces:**
- Consumes: existing `AudioBuffer`, `AudioFrame`, `AudioFormat`, and `LiveAudioDecoder::create_pair(sample_rate: u32, channels: u8, capacity: usize) -> (LiveFrameSender, LiveAudioDecoder)` behavior, including subproject 2's non-blocking capture-queue outcome contract.
- Produces: `AudioDiagnosticsSource::snapshot() -> AudioDiagnosticsSnapshot`, `LiveFrameSender::diagnostics_source() -> AudioDiagnosticsSource`, and exact `SenderBufferSnapshot`/`CaptureQueueSnapshot` values used by Tasks 4, 6, 8, 11, and 12.

```rust
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureQueueSnapshot {
    pub queued_frames: u32,
    pub capacity_frames: u32,
    pub submitted_frames_total: u64,
    pub submitted_samples_per_channel_total: u64,
    pub full_queue_drops_total: u64,
    pub disconnected_drops_total: u64,
}

#[derive(Clone)]
pub struct AudioDiagnosticsSource { /* Arc-backed atomics/accumulators */ }

impl AudioDiagnosticsSource {
    pub fn snapshot(&self) -> AudioDiagnosticsSnapshot;
}

impl LiveFrameSender {
    pub fn diagnostics_source(&self) -> AudioDiagnosticsSource;
}
```

- `LiveFrameSender::try_send` keeps its existing non-blocking outcome contract from subproject 2. Successful submission counts the frame and samples per channel; full and disconnected outcomes increment different counters. Queue length is sampled after the outcome and never inferred from submitted minus dropped totals.

- [ ] **Step 1 (4 min): Add failing `AudioBuffer` snapshot tests.** Push two frames with different actual samples-per-channel, pop one, then assert exact `queued_samples_per_channel`, `buffered_ns`, totals, capacity, and a fill ratio in `0.0..=1.0`.
- [ ] **Step 2 (4 min): Add failing queue-outcome tests.** With capacity one, assert one success, one full drop, then drop the decoder and assert one disconnected drop. Assert the calls return immediately without a blocking send.
- [ ] **Step 3 (2 min): Run the focused failures.** Run `cargo test -p airplay-audio --test diagnostics sender_buffer_` and `cargo test -p airplay-audio --test diagnostics capture_queue_`.
- [ ] **Step 4 (5 min): Implement shared audio diagnostics state.** Use `AtomicU64` counters and capacity metadata. Update queue length from `Sender::len()` and compute submitted samples as `frame.samples.len() / frame.channels`, rejecting zero channels from diagnostics accounting without panicking.
- [ ] **Step 5 (4 min): Add `AudioBuffer::diagnostics_snapshot()`.** Sum `AudioFrame::samples_per_channel(format.channels)` across queued frames and compute:

```rust
let buffered_ns = queued_samples_per_channel
    .saturating_mul(1_000_000_000)
    / u64::from(sample_rate);
let fill_ratio = if capacity_frames == 0 { 0.0 } else {
    (queued_frames as f32 / capacity_frames as f32).clamp(0.0, 1.0)
};
```

- [ ] **Step 6 (3 min): Wire the source through `LiveAudioDecoder::create_pair`.** Sender and decoder share the same source without changing the pair's public return shape; the decoder exposes it internally for streamer registration.
- [ ] **Step 7 (3 min): Run audio tests.** Run `cargo test -p airplay-audio --test diagnostics` and `cargo test -p airplay-audio`.
- [ ] **Step 8 (2 min): Commit buffer and queue diagnostics.** Run:

```powershell
git add crates/airplay-audio/src/diagnostics.rs crates/airplay-audio/tests/diagnostics.rs crates/airplay-audio/src/buffer.rs crates/airplay-audio/src/live_decoder.rs crates/airplay-audio/src/lib.rs
git commit -m "feat(audio): expose sender and capture queue metrics"
```

### Task 4: Instrument scheduler jitter and truthful per-target UDP counters

**Files:**
- Modify: `crates/airplay-audio/src/diagnostics.rs`
- Modify: `crates/airplay-audio/tests/diagnostics.rs`
- Modify: `crates/airplay-audio/src/streamer.rs`
- Modify: `crates/airplay-audio/src/rtp.rs`
- Modify: `crates/airplay-audio/src/lib.rs`

**Interfaces:**
- Consumes: Task 3's shared `AudioDiagnosticsSource`, `SenderBufferSnapshot`, and `CaptureQueueSnapshot`, plus existing `AudioStreamer`, `SenderMessage`, `RtpSender`, and per-target `UdpSocket::send_to` dispatch points.
- Produces: `SchedulerJitterSnapshot`, per-target `TargetTransportSnapshot`, and `AudioDiagnosticsSnapshot { sender_buffer, capture_queue, scheduler, rtp_frames_prepared_total, targets }` with local-OS acceptance semantics.

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerJitterSnapshot {
    pub sample_count: u64,
    pub last_signed_jitter_ns: Option<i64>,
    pub mean_absolute_jitter_ns: Option<u64>,
    pub p95_absolute_jitter_ns: Option<u64>,
    pub maximum_absolute_jitter_ns: Option<u64>,
    pub deadline_resets_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetTransportSnapshot {
    pub data_datagrams_attempted_total: u64,
    pub data_datagrams_accepted_local_total: u64,
    pub data_datagram_send_failures_total: u64,
    pub data_bytes_accepted_local_total: u64,
    pub sync_datagrams_attempted_total: u64,
    pub sync_datagrams_accepted_local_total: u64,
    pub sync_datagram_send_failures_total: u64,
    pub sync_bytes_accepted_local_total: u64,
}

pub struct AudioDiagnosticsSnapshot {
    pub sender_buffer: SenderBufferSnapshot,
    pub capture_queue: CaptureQueueSnapshot,
    pub scheduler: SchedulerJitterSnapshot,
    pub rtp_frames_prepared_total: u64,
    pub targets: Vec<TargetTransportSnapshot>,
}
```

- [ ] **Step 1 (5 min): Add failing deterministic jitter tests.** Record exact dispatch intervals around `target_interval_ns(352, 44_100, burst_frames)` and assert signed last, cumulative mean absolute, maximum, nearest-rank p95 over a 1,024-value rolling window, and reset exclusion.
- [ ] **Step 2 (4 min): Add failing UDP result tests.** Use two localhost receivers plus an injected failing `DatagramSender` test double. One shared frame must increment `rtp_frames_prepared_total` once and produce one attempted/result count for each target; bytes are only the `send_to` return value.
- [ ] **Step 3 (2 min): Run focused failures.** Run `cargo test -p airplay-audio --test diagnostics scheduler_` and `cargo test -p airplay-audio --test diagnostics target_datagram_`.
- [ ] **Step 4 (5 min): Implement `SchedulerJitterAccumulator`.** Keep a `VecDeque<u64>` capped at 1,024 only for p95; keep cumulative count, absolute sum in `u128`, and max for the session. Nearest-rank p95 uses `ceil(0.95 * n) - 1` after sorting a bounded copy.
- [ ] **Step 5 (4 min): Instrument immediately before burst dispatch.** Compute target interval with integer nanoseconds:

```rust
let target_ns = u64::from(frames_per_packet)
    .saturating_mul(1_000_000_000)
    .saturating_mul(burst_frame_count as u64)
    / u64::from(sample_rate);
```

Exclude the first dispatch and first dispatch after Pause, Resume, buffer stall, Stop, sender-channel disconnect, or a deadline reset; increment `deadline_resets_total` once for each reset.
- [ ] **Step 6 (5 min): Instrument both sender-thread and direct-send paths.** Increment attempt before `send_to`, accepted/bytes only on `Ok(n)`, and failure only on `Err`. Apply the equivalent rule to PT=84/PT=87 sync sends. Never increment from packet preparation.
- [ ] **Step 7 (3 min): Move shared prepared-frame accounting to successful encoded-frame preparation.** Increment once after ALAC output is accepted for fan-out and before per-target serialization; keep legacy `packets_sent()` untouched for compatibility and do not use it in the new snapshot.
- [ ] **Step 8 (3 min): Run audio tests.** Run `cargo test -p airplay-audio --test diagnostics` and `cargo test -p airplay-audio`.
- [ ] **Step 9 (2 min): Commit scheduler and datagram metrics.** Run:

```powershell
git add crates/airplay-audio/src/diagnostics.rs crates/airplay-audio/tests/diagnostics.rs crates/airplay-audio/src/streamer.rs crates/airplay-audio/src/rtp.rs crates/airplay-audio/src/lib.rs
git commit -m "feat(audio): measure scheduler and local datagram results"
```

### Task 5: Account for retransmit outcomes and remove support-unsafe diagnostic traces

**Files:**
- Modify: `crates/airplay-audio/src/diagnostics.rs`
- Modify: `crates/airplay-audio/tests/diagnostics.rs`
- Modify: `crates/airplay-audio/src/rtp.rs`
- Modify: `crates/airplay-audio/src/streamer.rs`
- Modify: `crates/airplay-client/src/client.rs`
- Modify: `crates/airplay-client/src/connection.rs`
- Modify: `crates/airplay-timing/src/ptp.rs`

**Interfaces:**
- Consumes: Task 4's target-indexed transport accumulators, existing `RetransmitRequest { first_sequence, count }`, each `RtpSender` packet-history ring, and the single/group PT=85 control listeners.
- Produces: `AudioStreamer::handle_retransmit_for_target(index, request) -> Result<RetransmitOutcome>`, per-target `RetransmitSnapshot`, and normal-build tracing that contains no packet/audio/identity material.

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetransmitSnapshot {
    pub retransmit_request_datagrams_total: u64,
    pub retransmit_packet_slots_requested_total: u64,
    pub retransmit_datagrams_accepted_local_total: u64,
    pub retransmit_history_misses_total: u64,
    pub retransmit_send_failures_total: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetransmitOutcome {
    pub requested_slots: u16,
    pub accepted_local: u16,
    pub history_misses: u16,
    pub send_failures: u16,
}

impl AudioStreamer {
    pub async fn handle_retransmit_for_target(
        &self,
        target_index: usize,
        request: &RetransmitRequest,
    ) -> Result<RetransmitOutcome>;
}
```

- [ ] **Step 1 (5 min): Add failing retransmit tests.** Cover repeated identical PT=85 requests, a request spanning present and absent history entries, successful PT=86 local acceptance, and injected send failure. Assert request datagrams and requested slots are not deduplicated.
- [ ] **Step 2 (3 min): Add a failing source-safety check.** Assert normal-build source no longer contains the current `DIAG PCM`, `DIAG ALAC`, first-packet hex, nonce/tag, destination address, or clock-ID logging blocks.
- [ ] **Step 3 (2 min): Run focused failures.** Run `cargo test -p airplay-audio --test diagnostics retransmit_` and confirm the outcome granularity is absent.
- [ ] **Step 4 (5 min): Return an outcome for every requested sequence slot.** Treat a missing/wrong history slot as one history miss, each `send_to` error as one send failure, and each successful local send as one accepted datagram. Preserve wrapping `u16` sequence traversal.
- [ ] **Step 5 (3 min): Update client control listeners.** Record one request datagram before handling and merge the returned per-target outcome. Do not compute a loss percentage and do not aggregate group requested slots over a single shared-frame denominator.
- [ ] **Step 6 (5 min): Remove unsafe normal-build tracing.** Delete PCM samples/energy, ALAC bytes/hex, RTP header hex, nonce/tag, SSRC, raw destination, PTP clock identity, and sync-packet dumps. Retain only structured-safe fields such as component, operation, target index local to the session, byte count, duration, and `io::ErrorKind`.
- [ ] **Step 7 (3 min): Verify the unsafe patterns are absent.** Run:

```powershell
rg -n "DIAG PCM|DIAG ALAC|first_4|first_8|nonce=|tag_first|header_first|pkt=|clock_id=|dest=|ssrc=" crates/airplay-audio/src crates/airplay-client/src crates/airplay-timing/src
if ($LASTEXITCODE -eq 0) { throw 'support-unsafe normal-build trace remains' }
```

- [ ] **Step 8 (3 min): Run affected suites.** Run `cargo test -p airplay-audio` and `cargo test -p airplay-client`.
- [ ] **Step 9 (2 min): Commit retransmit truth and trace safety.** Run:

```powershell
git add crates/airplay-audio/src/diagnostics.rs crates/airplay-audio/tests/diagnostics.rs crates/airplay-audio/src/rtp.rs crates/airplay-audio/src/streamer.rs crates/airplay-client/src/client.rs crates/airplay-client/src/connection.rs crates/airplay-timing/src/ptp.rs
git commit -m "feat(audio): report retransmit outcomes safely"
```

### Task 6: Publish stable client diagnostics, feedback outcomes, and receiver mapping

**Files:**
- Create: `crates/airplay-client/src/diagnostics.rs`
- Create: `crates/airplay-client/tests/diagnostics.rs`
- Modify: `crates/airplay-client/src/stats.rs`
- Modify: `crates/airplay-client/src/events.rs`
- Modify: `crates/airplay-client/src/connection.rs`
- Modify: `crates/airplay-client/src/client.rs`
- Modify: `crates/airplay-client/src/lib.rs`

**Interfaces:**
- Consumes: Task 2's `TimingDiagnosticsHandle`, Tasks 3–5's `AudioDiagnosticsSource`/transport/retransmit snapshots, stable `DeviceId`, existing RTSP feedback transactions, and the sender-index mapping created during single/group setup.
- Produces: cloneable `Connection::diagnostics_source() -> ClientDiagnosticsSource` and `AirPlayClient::diagnostics_source() -> ClientDiagnosticsSource`, whose `snapshot(now_elapsed_ns)` and bounded `drain_events(limit)` are the only client inputs to the application registry.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackResultKind { Success, ProtocolFailure, TransportFailure, Timeout }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedbackSnapshot {
    pub attempts_total: u64,
    pub successes_total: u64,
    pub protocol_failures_total: u64,
    pub transport_failures_total: u64,
    pub timeouts_total: u64,
    pub last_result: Option<FeedbackResultKind>,
    pub last_transaction_duration_ns: Option<u64>,
    pub last_success_elapsed_ns: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ConnectionDiagnosticsSnapshot {
    pub device_id: DeviceId,
    pub sender_target_index: Option<usize>,
    pub rtsp_state: RtspSessionState,
    pub playback_state: PlaybackState,
    pub timing_role: ClientTimingRole,
    pub timing: TimingDiagnosticsSnapshot,
    pub transport: TargetTransportSnapshot,
    pub retransmit: RetransmitSnapshot,
    pub feedback: FeedbackSnapshot,
    pub last_error: Option<ClientDiagnosticError>,
}

#[derive(Clone, Debug)]
pub struct ClientDiagnosticsSnapshot {
    pub captured_elapsed_ns: u64,
    pub audio: Option<AudioDiagnosticsSnapshot>,
    pub connections: Vec<ConnectionDiagnosticsSnapshot>,
    pub client_events_dropped_total: u64,
}

#[derive(Clone)]
pub struct ClientDiagnosticsSource { /* shared read source + bounded event feed */ }

impl ClientDiagnosticsSource {
    pub fn snapshot(&self, now_elapsed_ns: u64) -> ClientDiagnosticsSnapshot;
    pub(crate) fn drain_events(&self, limit: usize) -> Vec<ClientDiagnosticEvent>;
}

impl Connection {
    pub(crate) fn diagnostics_source(&self) -> ClientDiagnosticsSource;
}

impl AirPlayClient {
    pub(crate) fn diagnostics_source(&self) -> ClientDiagnosticsSource;
}
```

- `ClientDiagnosticError` is typed by operation/component/outcome and contains no packet/body/secret material. `StreamStats`, `StatsSnapshot`, `packets_sent`, and `loss_percent()` stay source-compatible; the new source never calls `loss_percent()`.

- [ ] **Step 1 (4 min): Add failing feedback tests.** Use a fake RTSP connection/clock to assert success, protocol rejection, transport failure, and the existing two-second timeout each update different counters and preserve transaction duration. Timeout remains non-fatal to the session.
- [ ] **Step 2 (5 min): Add failing receiver-order tests.** Build connections in discovery order A/B, reorder discovery B/A, and assert target counter snapshots still map to the original stable `DeviceId`; reconnect uses the same ID with a fresh source/session registration later.
- [ ] **Step 3 (3 min): Add timing-role tests.** Primary is `PtpPrimaryMeasured`; secondaries are `PtpSecondaryShared { source: primary_id }` with `independently_measured == false`; one receiver is `NtpSenderReference` with unavailable offset/path delay/drift.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p airplay-client --test diagnostics feedback_` and `cargo test -p airplay-client --test diagnostics receiver_mapping_`.
- [ ] **Step 5 (5 min): Implement the cloneable source and typed snapshots.** Store `(DeviceId, target_index)` when group senders are constructed, sort snapshots by `DeviceId`, and join target metrics by that mapping rather than current connection vector position. Significant client errors/results use a bounded 2,048-entry `try_send` feed drained only by the registry; a full feed increments `client_events_dropped_total`.
- [ ] **Step 6 (4 min): Instrument feedback at the RTSP transaction boundary.** Capture monotonic start immediately before the request; classify timeout separately from protocol and transport errors; record duration on every attempted terminal result.
- [ ] **Step 7 (4 min): Attach the one primary timing source correctly.** Primary and secondary connection snapshots reference the same `TimingDiagnosticsHandle`; only the primary publishes its latest values. Secondaries publish the source ID and no duplicated measurement values.
- [ ] **Step 8 (3 min): Preserve compatibility explicitly.** Add a regression test that old `StatsSnapshot::loss_percent()` compiles while `ClientDiagnosticsSnapshot` has no field or method named loss.
- [ ] **Step 9 (3 min): Run client tests.** Run `cargo test -p airplay-client --test diagnostics` and `cargo test -p airplay-client`.
- [ ] **Step 10 (2 min): Commit client diagnostics.** Run:

```powershell
git add crates/airplay-client/src/diagnostics.rs crates/airplay-client/tests/diagnostics.rs crates/airplay-client/src/stats.rs crates/airplay-client/src/events.rs crates/airplay-client/src/connection.rs crates/airplay-client/src/client.rs crates/airplay-client/src/lib.rs
git commit -m "feat(client): publish receiver-mapped diagnostics"
```

### Task 7: Define structured application diagnostics and the bounded cursor ring

**Files:**
- Create: `crates/homepod-cast/src/diagnostics.rs`
- Create: `crates/homepod-cast/tests/diagnostics_contract.rs`
- Modify: `crates/homepod-cast/src/lib.rs`
- Modify: `crates/homepod-cast/Cargo.toml`

**Interfaces:**
- Consumes: subproject 2's stable `ReceiverId`, fresh `SessionId`, `ReceiverLifecycleState`, and `BackendDiagnosticPayload`, plus Task 6's `ClientDiagnosticEvent` and typed client snapshots.
- Produces: the synchronous cloneable `DiagnosticsHandle::{snapshot, events_since}`, structured `DiagnosticError`/`DiagnosticEvent` types, `ReceiverSessionKey`, and the 4,096-entry `EventCursor`/`EventBatch`/`EventGap` contract used by every later application task.

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReceiverSessionKey {
    pub session_id: SessionId,
    pub receiver_id: ReceiverId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticComponent {
    Discovery, Pairing, Rtsp, Timing, Capture, Buffer, Encoder,
    Scheduler, UdpTransport, Retransmit, Feedback, Calibration, Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity { Info, Warning, Error }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recoverability {
    Transient, AutomaticRetry, RequiresSessionRestart, UserAction, Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticErrorCode {
    DiscoveryFailure, PairingFailure, SetupRejected, SetupTimeout,
    EventChannelFailure, PtpBindFallback, PtpSampleStale, CaptureFailure,
    CaptureQueueFull, BufferUnderrun, EncodeFailure, SenderQueueDisconnected,
    UdpSendFailure, RetransmitHistoryMiss, FeedbackFailure, FeedbackTimeout,
    TeardownTimeout, CalibrationApplyFailure, ExportFailure,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EventCursor(pub u64);

#[derive(Clone, Debug)]
pub struct DiagnosticEvent {
    pub cursor: EventCursor,
    pub occurred_at_utc: SystemTime,
    pub process_elapsed_ns: u64,
    pub receiver_session: Option<ReceiverSessionKey>,
    pub severity: DiagnosticSeverity,
    pub component: DiagnosticComponent,
    pub code: Option<DiagnosticErrorCode>,
    pub public_message: String,
    pub payload: StructuredDiagnosticPayload,
}

#[derive(Clone, Debug)]
pub enum StructuredDiagnosticPayload {
    Backend(BackendDiagnosticPayload),
    Client(ClientDiagnosticEvent),
    Error(DiagnosticError),
    DefiniteCounterTransition {
        counter: DefiniteCounterKind,
        previous_total: u64,
        current_total: u64,
    },
    CalibrationState(CalibrationApplyState),
    ExportState(ExportEventState),
}

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

- [ ] **Step 1 (5 min): Add failing domain tests.** Verify error-code stability, optional receiver session keying, `SessionId` freshness across restart fixtures, and receiver sorting by `ReceiverId` rather than name/index.
- [ ] **Step 2 (5 min): Add failing cursor tests.** Append 4,100 numbered events, then assert capacity 4,096, strict `cursor > supplied`, `limit=0` clamps to one, `limit=999` clamps to 512, `next_cursor` remains unchanged on empty reads, and an old cursor returns exact `EventGap.overwritten_events`.
- [ ] **Step 3 (3 min): Add a failing non-blocking producer test.** Fill the bounded ingress, call `try_record` from a test thread, assert prompt return and `diagnostics_events_dropped_total == 1`.
- [ ] **Step 4 (2 min): Run the focused failures.** Run `cargo test -p homepod-cast --test diagnostics_contract event_cursor_` and `cargo test -p homepod-cast --test diagnostics_contract producer_`.
- [ ] **Step 5 (5 min): Implement the ring and read handle.** Use a process-lifetime cursor counter starting at one and a `VecDeque` capped at 4,096. `events_since` must calculate gaps before applying the limit.
- [ ] **Step 6 (4 min): Implement bounded ingress.** Writers receive a cloneable `DiagnosticIngress` whose `try_record` only uses `try_send`; a full channel increments an atomic drop counter. The registry consumer, not the writer, assigns event cursors.
- [ ] **Step 7 (3 min): Add only required dependencies.** Add `serde.workspace = true`, `serde_json = "1.0"`, `arc-swap = "1.7"`, and `time = { version = "0.3", features = ["formatting"] }` if the predecessor manifests do not already provide them.
- [ ] **Step 8 (3 min): Run contract tests.** Run `cargo test -p homepod-cast --test diagnostics_contract` and `cargo test -p homepod-cast --lib`.
- [ ] **Step 9 (2 min): Commit the diagnostics contract.** Run:

```powershell
git add crates/homepod-cast/src/diagnostics.rs crates/homepod-cast/tests/diagnostics_contract.rs crates/homepod-cast/src/lib.rs crates/homepod-cast/Cargo.toml
git commit -m "feat(app): add bounded diagnostics read contract"
```

### Task 8: Build the single registry, immutable snapshots, and conservative health model

**Files:**
- Modify: `crates/homepod-cast/src/diagnostics.rs`
- Modify: `crates/homepod-cast/tests/diagnostics_contract.rs`
- Modify: `crates/homepod-cast/src/backend/event.rs`

**Interfaces:**
- Consumes: Task 7's diagnostics domain/ring/ingress, Task 6's `ClientDiagnosticsSource`, and the sole subproject 2 `DiagnosticEventReceiver` together with authoritative session/receiver lifecycle values.
- Produces: `DiagnosticsRegistry::{start, register_session, finish_session}`, immutable schema-version-1 `DiagnosticsSnapshot` publication, retained stopped-session data, and conservative `Unknown`/`Running`/`Attention`/`Error` health values.

```rust
#[derive(Clone)]
pub struct DiagnosticsHandle { /* read-only Arc state */ }

impl DiagnosticsHandle {
    pub fn snapshot(&self) -> DiagnosticsSnapshot;
    pub fn events_since(&self, cursor: EventCursor, limit: usize) -> EventBatch;
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct ReceiverDiagnosticsSnapshot {
    pub key: ReceiverSessionKey,
    pub lifecycle: ReceiverLifecycleState,
    pub timing: ReceiverTimingSnapshot,
    pub transport: ReceiverTransportSnapshot,
    pub feedback: FeedbackSnapshot,
    pub last_error: Option<DiagnosticError>,
}

pub struct SessionRegistration {
    pub session_id: SessionId,
    pub started_elapsed_ns: u64,
    pub primary: Option<ReceiverId>,
    pub members: BTreeMap<ReceiverId, DeviceId>,
    pub source: ClientDiagnosticsSource,
}

impl DiagnosticsRegistry {
    pub fn start(
        backend_events: DiagnosticEventReceiver,
        clock: Arc<dyn DiagnosticsClock>,
    ) -> (Self, DiagnosticsHandle);
    pub fn register_session(&self, registration: SessionRegistration);
    pub fn finish_session(&self, session_id: SessionId, reason: SessionStopReason);
}
```

- The registry has one background consumer. It publishes owned snapshots through `ArcSwap`, retains the last completed session snapshot for Speakers, and drains backend/client significant events into the one ring. Snapshot schema version is exactly 1.

- [ ] **Step 1 (5 min): Add failing aggregation tests.** Register a session with two receiver mappings, mutate client atomics, advance a fake clock, run one registry aggregation, and assert one sorted receiver row per stable ID with the correct session key.
- [ ] **Step 2 (4 min): Add failing timing truth tests.** Assert primary values are present, secondary values are absent with `shared_source_receiver_id`, and NTP uses `SenderReference` with offset/path delay/drift absent.
- [ ] **Step 3 (5 min): Add failing health-state tests.** Cover no session (`Unknown`), active clean stream (`Running`), authoritative failed/terminal state (`Error`), and new capture drop/underrun/UDP failure/history miss/feedback failure/stale PTP (`Attention`). Assert offset magnitude, drift, mean path delay, p95 jitter, and recovery-demand ratio alone never change health.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p homepod-cast --test diagnostics_contract registry_` and `cargo test -p homepod-cast --test diagnostics_contract health_`.
- [ ] **Step 5 (5 min): Implement one registry consumer and immutable publication.** The consumer owns source cursors, session registrations, the event ring, snapshot sequence, and last-seen definite counter values. Hot-path sources never touch `ArcSwap` or the ring lock.
- [ ] **Step 6 (4 min): Map backend diagnostics structurally.** Convert subproject 2 discovery/lifecycle/restart/capture/persistence/queue events by typed payload and stable IDs. Do not parse tracing strings. Events outside a session keep `receiver_session: None`.
- [ ] **Step 7 (4 min): Implement recovery-demand values without a loss name.** Compute per receiver only as `requested_slots / shared_frames_prepared_while_member`, retain numerator and denominator, and use `None` when the denominator is zero. Do not produce a group ratio.
- [ ] **Step 8 (3 min): Implement PTP staleness events without a sync claim.** Emit one `PtpSampleStale` transition event when stale begins and one informational recovery event when a new sample arrives; staleness alone means observations stopped.
- [ ] **Step 9 (3 min): Run app diagnostics tests.** Run `cargo test -p homepod-cast --test diagnostics_contract` and `cargo test -p homepod-cast --lib`.
- [ ] **Step 10 (2 min): Commit registry aggregation.** Run:

```powershell
git add crates/homepod-cast/src/diagnostics.rs crates/homepod-cast/tests/diagnostics_contract.rs crates/homepod-cast/src/backend/event.rs
git commit -m "feat(app): aggregate session diagnostics truthfully"
```

### Task 9: Persist and apply manual calibration through the resilience backend

**Files:**
- Create: `crates/homepod-cast/src/calibration.rs`
- Create: `crates/homepod-cast/tests/calibration.rs`
- Modify: `crates/homepod-cast/src/lib.rs`
- Modify: `crates/homepod-cast/src/backend/model.rs`
- Modify: `crates/homepod-cast/src/backend/command.rs`
- Modify: `crates/homepod-cast/src/backend/controller.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/backend/capture.rs`
- Modify: `crates/homepod-cast/src/backend/persistence.rs`
- Modify: `crates/homepod-cast/tests/persistence.rs`
- Modify: `crates/homepod-cast/tests/backend_lifecycle.rs`

**Interfaces:**
- Consumes: subproject 2's `ReceiverId`, desired revision, backend actor, `CaptureSupervisor`, atomic device-state persistence, and controlled full-group `RestartReason` path; Task 7 supplies structured calibration errors/events.
- Produces: `CalibrationProfile`, `normalize_calibration(active, profile) -> Result<EffectiveCalibration, CalibrationError>`, `CalibrationCommand`, V1-to-V2 persisted-state migration, and a shared four-click PCM injection command.

```rust
pub const CALIBRATION_DISPLAY_STEP_NS: i64 = 100_000;
pub const CALIBRATION_CLICK_AMPLITUDE: i16 = 4_096;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationProfile {
    pub reference_receiver: Option<ReceiverId>,
    pub requested_relative_delay_ns: BTreeMap<ReceiverId, i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveCalibration {
    pub minimum_requested_ns: i64,
    pub effective_delay_ns: BTreeMap<ReceiverId, u64>,
}

pub fn normalize_calibration(
    active: &BTreeSet<ReceiverId>,
    profile: &CalibrationProfile,
) -> Result<EffectiveCalibration, CalibrationError>;

pub enum CalibrationCommand {
    ApplyCalibrationProfile {
        expected_desired_revision: u64,
        profile: CalibrationProfile,
    },
    ResetCalibration {
        expected_desired_revision: u64,
    },
    RunCalibrationClickTest,
}

// Add this exact variant to the predecessor enum:
// BackendCommand::Calibration(CalibrationCommand)
```

- Persistence migrates version 1 to `PersistedStateV2 { version: 2, calibration: CalibrationProfile, .. }` in the same device-state path. Version 1 migrates to an empty/zero profile, and all predecessor fields remain byte-for-byte equivalent after round trip.

- [ ] **Step 1 (5 min): Add failing normalization tests.** For requested delays `A=-2.0 ms`, `B=0.0 ms`, `C=3.5 ms`, assert effective `A=0`, `B=2.0 ms`, `C=5.5 ms` and identical pairwise differences. Cover membership renormalization and `i64`/`u64` overflow rejection.
- [ ] **Step 2 (4 min): Add failing persistence migration tests.** Load a complete V1 fixture, assert V2 version and zero profile, write/read it, and assert all volume/group/desired/endpoint/latency/autoconnect values remain intact. Verify profiles follow `ReceiverId` after discovery reorder.
- [ ] **Step 3 (5 min): Add failing restart tests.** Applying and resetting while a multi-receiver session is active each persist once and enqueue exactly one `RestartReason::CalibrationChanged`; a stale desired revision, fewer than two active receivers, persistence failure, or invalid arithmetic produces `CalibrationApplyFailure` and no restart.
- [ ] **Step 4 (4 min): Add failing click-pattern tests.** Generate 44.1 kHz stereo PCM with four 2 ms clicks starting 500 ms apart, fixed amplitude 4,096, zeros elsewhere, and no receiver-specific content.
- [ ] **Step 5 (2 min): Run focused failures.** Run `cargo test -p homepod-cast --test calibration` and `cargo test -p homepod-cast --test persistence calibration_`.
- [ ] **Step 6 (5 min): Implement normalization with checked arithmetic.** Require the reference receiver and every applied key to belong to the active set; omitted active members have requested zero. Use `i128` subtraction before narrowing to `u64`.
- [ ] **Step 7 (5 min): Implement V1-to-V2 migration and atomic persistence.** Keep the existing corrupt-file recovery, write coalescing, temporary-file flush/replace, and shutdown flush semantics. Calibration never enters shell preferences.
- [ ] **Step 8 (5 min): Route apply/reset through the backend actor.** Persist the validated profile, emit structured pending/applied/failure events, then request the existing full-group reconciliation once. Do not expose restart authority on `DiagnosticsHandle`.
- [ ] **Step 9 (4 min): Route the click test through `CaptureSupervisor`.** Inject the shared PCM pattern into the stable live bridge so it uses the normal shared ALAC/RTP fan-out. The command is rejected when fewer than two receivers are active. Do not record sample values or bytes in events/tracing.
- [ ] **Step 10 (3 min): Run persistence/lifecycle tests.** Run `cargo test -p homepod-cast --test calibration`, `cargo test -p homepod-cast --test persistence`, and `cargo test -p homepod-cast --test backend_lifecycle calibration_`.
- [ ] **Step 11 (2 min): Commit calibration state and restart policy.** Run:

```powershell
git add crates/homepod-cast/src/calibration.rs crates/homepod-cast/tests/calibration.rs crates/homepod-cast/src/lib.rs crates/homepod-cast/src/backend/model.rs crates/homepod-cast/src/backend/command.rs crates/homepod-cast/src/backend/controller.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/backend/capture.rs crates/homepod-cast/src/backend/persistence.rs crates/homepod-cast/tests/persistence.rs crates/homepod-cast/tests/backend_lifecycle.rs
git commit -m "feat(app): persist restart-only speaker calibration"
```

### Task 10: Apply per-target presentation clocks without changing shared RTP

**Files:**
- Create: `crates/airplay-audio/tests/presentation_calibration.rs`
- Modify: `crates/airplay-timing/src/clock.rs`
- Modify: `crates/airplay-timing/src/ptp.rs`
- Modify: `crates/airplay-timing/tests/diagnostics.rs`
- Modify: `crates/airplay-audio/src/streamer.rs`
- Modify: `crates/airplay-audio/src/rtp.rs`
- Modify: `crates/airplay-client/src/diagnostics.rs`
- Modify: `crates/airplay-client/src/client.rs`
- Modify: `crates/airplay-client/src/connection.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/tests/calibration.rs`

**Interfaces:**
- Consumes: Task 1's explicit `local_to_master_ns`, Tasks 4–5's per-target `RtpSender`/transport counters, Task 6's stable `DeviceId` target mapping, and Task 9's normalized `EffectiveCalibration` supplied only during controlled session setup.
- Produces: `SenderMessage::Packet { wire_packets, sync_packets }`, `AudioStreamer::set_target_presentation_delays_ns(Vec<u64>)`, checked future presentation times, and `AirPlayClient::start_live_streaming_to_group_with_presentation_delays(...)` without changing shared RTP/audio/PTP meaning.

```rust
enum SenderMessage {
    Packet {
        wire_packets: Vec<Vec<u8>>,
        sync_packets: Vec<Option<Vec<u8>>>,
    },
    Pause,
    Resume,
    Stop,
}

impl AudioStreamer {
    pub fn set_target_presentation_delays_ns(
        &mut self,
        delays: Vec<u64>,
    ) -> Result<()>;
}

impl AirPlayClient {
    pub async fn start_live_streaming_to_group_with_presentation_delays(
        &mut self,
        decoder: LiveAudioDecoder,
        effective_delay_ns: &BTreeMap<DeviceId, u64>,
    ) -> Result<()>;
}

pub fn checked_presentation_time_ns(
    common_master_time_ns: u64,
    global_render_lead_ns: u64,
    effective_delay_ns: u64,
    now_master_time_ns: u64,
) -> Result<u64, CalibrationError>;
```

- [ ] **Step 1 (5 min): Add failing packet-invariant tests.** With two deterministic `RtpSender` fixtures, assert both audio packets have the same payload, RTP timestamp, current/next RTP timestamp, marker meaning, and shared PTP clock identity while PT=87 presentation time differs by the requested effective delay.
- [ ] **Step 2 (4 min): Add failing per-target sync-state tests.** Assert both senders independently advance sync sequence and first-sync extension state, and `sync_packets.len() == wire_packets.len() == target_count`.
- [ ] **Step 3 (4 min): Add failing time-validation tests.** Cover checked overflow and rejection when `common + lead + effective <= now_master`; assert no socket/send/counter mutation on rejection.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p airplay-audio --test presentation_calibration`.
- [ ] **Step 5 (5 min): Replace shared sync payload construction.** For each target index, call that target's `RtpSender::prepare_sync` or `prepare_ptp_sync` with the common RTP values and target presentation time. Preserve one shared encode and common sender dispatch deadline.
- [ ] **Step 6 (4 min): Send each sync only to its matching target.** Zip target, wire packet, and optional sync packet by validated equal lengths. Record each target's sync attempt/result in its existing transport counters.
- [ ] **Step 7 (4 min): Apply the explicit local-to-master convention.** Make `PtpClient::local_to_master`, its `TimingProtocol::local_to_remote` implementation, and the stream timestamp boundary use `local_to_master_ns`; positive local-minus-master offset must subtract. Keep the PTP watch/BMCA values unchanged, preserve NTP sender-reference behavior, and deprecate the ambiguous `Clock::apply_offset` call site. Run the Task 1 sign and round-trip tests before and after this change.
- [ ] **Step 8 (5 min): Map stable calibration IDs to sender target order in `airplay-client`.** Require an entry for every connected target (zero is valid), reject extra/unknown IDs, and set delays only before streaming begins. Reconnect/restart reconstructs the vector from the persisted profile.
- [ ] **Step 9 (4 min): Integrate the resilience session.** On full setup, normalize the active profile, map `ReceiverId` losslessly to `DeviceId`, and pass effective delays before RECORD/audio start. FLUSH/RECORD and `reset_sync_state()` establish first marker/sync state.
- [ ] **Step 10 (3 min): Run affected tests.** Run `cargo test -p airplay-timing --test diagnostics`, `cargo test -p airplay-audio --test presentation_calibration`, `cargo test -p airplay-client --test diagnostics`, and `cargo test -p homepod-cast --test calibration`.
- [ ] **Step 11 (2 min): Commit calibrated sync fan-out.** Run:

```powershell
git add crates/airplay-audio/tests/presentation_calibration.rs crates/airplay-timing/src/clock.rs crates/airplay-timing/src/ptp.rs crates/airplay-timing/tests/diagnostics.rs crates/airplay-audio/src/streamer.rs crates/airplay-audio/src/rtp.rs crates/airplay-client/src/diagnostics.rs crates/airplay-client/src/client.rs crates/airplay-client/src/connection.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/tests/calibration.rs
git commit -m "feat(streaming): apply per-target presentation delays"
```

### Task 11: Build the versioned support-safe export and copy summary

**Files:**
- Create: `crates/homepod-cast/tests/diagnostics_export.rs`
- Modify: `crates/homepod-cast/src/diagnostics.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/app/reducer.rs`

**Interfaces:**
- Consumes: Task 8's registry-owned latest snapshot and all retained structured events/gaps/drop counts, Task 9's effective calibration values, application/build/Windows/audio configuration metadata, and the existing shell effect worker for off-thread file output.
- Produces: `DiagnosticsRegistry::support_export_input() -> SupportExportInput`, `build_support_export(...) -> Result<SupportExportArtifact, DiagnosticError>`, and `build_support_summary(...) -> String`, both derived from the same support-safe allow-listed DTOs.

```rust
pub const SUPPORT_EXPORT_MEDIA_TYPE: &str =
    "application/vnd.openaircast.diagnostics+json";

#[derive(Serialize)]
pub struct SupportExportV1 {
    pub schema: &'static str,
    pub schema_version: u16,
    pub exported_at_utc: String,
    pub redaction: RedactionDescriptor,
    pub application: SupportApplication,
    pub configuration: SupportConfiguration,
    pub latest_snapshot: SupportSnapshot,
    pub events: Vec<SupportEvent>,
    pub event_gap: Option<SupportEventGap>,
}

pub struct SupportExportArtifact {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

pub(crate) struct SupportExportInput {
    pub latest_snapshot: DiagnosticsSnapshot,
    pub retained_events: Vec<DiagnosticEvent>,
    pub event_gap: Option<EventGap>,
    pub producer_drops_total: u64,
}

impl DiagnosticsRegistry {
    pub(crate) fn support_export_input(&self) -> SupportExportInput;
}

pub fn build_support_export(
    input: &SupportExportInput,
    context: &SupportExportContext,
) -> Result<SupportExportArtifact, DiagnosticError>;

pub fn build_support_summary(
    input: &SupportExportInput,
) -> String;
```

- [ ] **Step 1 (5 min): Add a complete hostile fixture.** Include raw receiver/name/MAC/IPv4/IPv6/port/serial/group/pairing/PTP/SSRC/RTSP IDs, username/path, headers/body, key, payload, PCM/ALAC/hex, nonce/tag, and an error chain containing several of them.
- [ ] **Step 2 (5 min): Add failing schema and redaction tests.** Assert schema/name/version/RFC3339/media type, per-export aliases `R-001..`, exact metric unit/semantics fields, calibration effective delays, event gap/drop disclosure, and byte-for-byte absence of every hostile token and prohibited field name.
- [ ] **Step 3 (3 min): Add failing alias tests.** The same receiver gets one alias throughout one export; a new export starts a new alias table; alias assignment is deterministic from sorted encountered `ReceiverId` values and never leaks the ID.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p homepod-cast --test diagnostics_export`.
- [ ] **Step 5 (5 min): Implement dedicated allow-listed DTOs.** Under one short registry read, `support_export_input()` copies the latest snapshot and all retained events (up to 4,096), including an existing overwrite gap and producer-drop total. Convert that input field by field. Do not derive `Serialize` for transport/domain types solely to export them. Omit `technical_detail` unless it was constructed from a typed support-safe detail; raw error chains always become `None`.
- [ ] **Step 6 (4 min): Encode exact truthful semantics.** Include unit descriptors such as `offset_local_minus_master_ns`, `mean_path_delay_ns`, `scheduler_dispatch_jitter_ns`, `local_os_udp_acceptance`, and `requested_latency_not_measured`; never emit keys named `loss_percent`, `receiver_buffer`, `network_jitter`, `ptp_rtt`, `clock_accuracy`, or `acoustic_latency`.
- [ ] **Step 7 (4 min): Reuse the same redaction conversion for Copy Summary.** Produce plain text from the support-safe DTO only; never interpolate a receiver display name, raw ID, address, or path.
- [ ] **Step 8 (5 min): Route export away from hot/UI paths.** `AppEvent::RequestDiagnosticsExport` emits an effect that obtains a save destination through the existing platform layer; a worker builds and writes bytes, then dispatches structured success/failure. Never log or include the chosen path. Cancellation writes no partial document.
- [ ] **Step 9 (3 min): Run export and reducer tests.** Run `cargo test -p homepod-cast --test diagnostics_export` and `cargo test -p homepod-cast app::reducer::tests::diagnostics_export`.
- [ ] **Step 10 (2 min): Commit support-safe export.** Run:

```powershell
git add crates/homepod-cast/tests/diagnostics_export.rs crates/homepod-cast/src/diagnostics.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/effect.rs crates/homepod-cast/src/app/reducer.rs
git commit -m "feat(app): add redacted diagnostics export"
```

### Task 12: Implement the Windows Calm diagnostics pages and 4 Hz polling rule

**Files:**
- Create: `crates/homepod-cast/src/diagnostics_window.rs`
- Create: `crates/homepod-cast/tests/diagnostics_ui.rs`
- Modify: `crates/homepod-cast/src/ui/mod.rs`
- Modify: `crates/homepod-cast/src/ui/navigation.rs`
- Modify: `crates/homepod-cast/src/ui/pages.rs`
- Modify: `crates/homepod-cast/src/app/state.rs`
- Modify: `crates/homepod-cast/src/app/event.rs`
- Modify: `crates/homepod-cast/src/app/snapshot.rs`

**Interfaces:**
- Consumes: Task 7's read-only `DiagnosticsHandle`, Task 8's health/view data, Tasks 9 and 11's typed calibration/copy/export app events, and subproject 1's Windows Calm tokens, Inter/IBM Plex fonts, navigation, focus, visibility, and repaint contracts.
- Produces: `DiagnosticsWindowState::poll_if_due(...) -> bool`, the `DiagnosticsPage` state machine, and accessible Overview/Speakers/Calibration/Events/Advanced renderers that issue commands only through typed `AppEvent` values.

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticsPage { #[default] Overview, Speakers, Calibration, Events, Advanced }

pub struct DiagnosticsWindowState {
    pub page: DiagnosticsPage,
    pub event_cursor: EventCursor,
    pub last_poll_elapsed_ns: Option<u64>,
    pub cached_snapshot: DiagnosticsSnapshot,
    pub cached_events: Vec<DiagnosticEvent>,
    pub latest_gap: Option<EventGap>,
}

impl DiagnosticsWindowState {
    pub fn poll_if_due(
        &mut self,
        handle: &DiagnosticsHandle,
        now_elapsed_ns: u64,
        visibility: DiagnosticsVisibility,
    ) -> bool;
}
```

- [ ] **Step 1 (5 min): Add failing polling tests.** Simulate 10 seconds at 1 ms render cadence and assert at most 40 polls when visible, exactly one snapshot plus one event read per due tick, no catch-up burst after a delayed frame, and zero polls while hidden/minimized/closed.
- [ ] **Step 2 (5 min): Add failing view-model tests.** Cover no session, one NTP receiver, healthy group PTP primary/shared secondary, stale timing, capture drop, UDP failure, reconnecting, and terminal error. Assert `Not measured` for unavailable values and exact allowed timing phrases.
- [ ] **Step 3 (4 min): Add failing interaction tests.** Page-bar keyboard navigation, event local filters/clear-cursor behavior, stable speaker expansion, disabled calibration below two active receivers, signed 0.1 ms controls, effective-delay preview, pending/applied/failure states, click-test action, reset/apply, copy, and export must emit typed `AppEvent` values only.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p homepod-cast --test diagnostics_ui`.
- [ ] **Step 5 (5 min): Implement the one timer.** Use `now - last_poll >= 250_000_000`, set `last_poll = now` rather than adding missed intervals, call both handle methods once, and request `egui::Context::request_repaint_after(Duration::from_millis(250))` only while visible and not minimized.
- [ ] **Step 6 (5 min): Build Overview and Speakers.** Overview shows signal path, session duration/count, shared buffer/capture/scheduler/prepared-frame metrics, summed receiver-local failures/recovery demand, and latest actionable error. Speakers uses stable rows, authoritative lifecycle, exact local datagram/retransmit/feedback counters, truthful timing-source text, sample age, and an expanded support-safe session alias rather than exposing raw identity.
- [ ] **Step 7 (5 min): Build Calibration.** Use one shared reference selector, signed 0.1 ms display, requested/effective preview, room-reflection/manual-listening disclosure, four-click action, and restart-only `Pending restart`, `Applied to session <alias>`, and structured failure states. Do not render a live retiming control or acoustic result.
- [ ] **Step 8 (5 min): Build Events and Advanced.** Events shows UTC time, session-relative time, severity, component, receiver alias, stable code, and public message; it uses local severity/component/receiver filters and explicit overwritten/producer-drop gap rows, while clearing only advances the page cursor. Advanced labels mean path delay, estimated relative clock drift and fit quality, feedback control-request duration, scheduler distribution, requested latency/render lead, capacities, counter start, and event drops with units and limitations.
- [ ] **Step 9 (4 min): Apply shell accessibility rules.** Use Inter Variable/body and IBM Plex Mono/metrics from the shell, existing Windows Calm tokens, ≥44 pt targets, focus order, high contrast, reduced motion, mixed-DPI-safe layout, and virtualized/scroll-clipped event rows.
- [ ] **Step 10 (3 min): Run UI tests.** Run `cargo test -p homepod-cast --test diagnostics_ui` and existing shell tests.
- [ ] **Step 11 (2 min): Commit the diagnostics UI.** Run:

```powershell
git add crates/homepod-cast/src/diagnostics_window.rs crates/homepod-cast/tests/diagnostics_ui.rs crates/homepod-cast/src/ui/mod.rs crates/homepod-cast/src/ui/navigation.rs crates/homepod-cast/src/ui/pages.rs crates/homepod-cast/src/app/state.rs crates/homepod-cast/src/app/event.rs crates/homepod-cast/src/app/snapshot.rs
git commit -m "feat(ui): add Windows Calm diagnostics pages"
```

### Task 13: Wire one registry through sessions, backend updates, and `AppHandle`

**Files:**
- Create: `crates/homepod-cast/tests/diagnostics_integration.rs`
- Modify: `crates/homepod-cast/src/app_handle.rs`
- Modify: `crates/homepod-cast/src/app/mod.rs`
- Modify: `crates/homepod-cast/src/app/effect.rs`
- Modify: `crates/homepod-cast/src/backend/session.rs`
- Modify: `crates/homepod-cast/src/cast.rs`
- Modify: `crates/homepod-cast/src/main.rs`
- Modify: `crates/homepod-cast/src/tray.rs`

**Interfaces:**
- Consumes: Task 8's single `DiagnosticsRegistry`, Task 6's single/group `ClientDiagnosticsSource`, subproject 2's `DeviceBackendUpdates::diagnostics`/session lifecycle, and subproject 1's existing `AppHandle` construction and shell effect executor.
- Produces: exactly one `AppHandle::diagnostics_handle() -> DiagnosticsHandle`, `cast::Session::diagnostics_source() -> ClientDiagnosticsSource`, deterministic session register/finish wiring, and fresh-session/stale-generation integration guarantees.

```rust
impl AppHandle {
    pub fn diagnostics_handle(&self) -> DiagnosticsHandle;
}

impl cast::Session {
    pub fn diagnostics_source(&self) -> ClientDiagnosticsSource;
}
```

- Application composition consumes `DeviceBackendUpdates::diagnostics` exactly once, starts one `DiagnosticsRegistry`, gives its read handle to `AppHandle`, and gives only registration capability to the backend session adapter.

- [ ] **Step 1 (5 min): Add a failing full-flow test.** Start fake shell/backend composition, start a two-receiver session, publish timing/audio/client/backend observations, and assert the handle produces one active session and two stable rows without exposing command methods.
- [ ] **Step 2 (4 min): Add failing restart retention tests.** A controlled calibration/recovery restart closes session A, retains its events/last speaker snapshot, creates fresh session B keys for the same receiver IDs, and never attaches late A observations to B.
- [ ] **Step 3 (4 min): Add failing lifecycle ownership tests.** Reconnecting/failed UI state comes from the backend snapshot even when UDP counters continue; diagnostics cannot change lifecycle, desired membership, retry, or restart policy.
- [ ] **Step 4 (2 min): Run focused failures.** Run `cargo test -p homepod-cast --test diagnostics_integration`.
- [ ] **Step 5 (5 min): Compose the registry once in startup.** Move `DeviceBackendUpdates::diagnostics` into the registry, retain the other state/events receivers for the shell, and pass the read handle into the existing `AppHandle` constructor.
- [ ] **Step 6 (4 min): Register both transport choices.** `cast::Session` retains the `Connection` source for one receiver and `AirPlayClient` source for a group; backend session setup registers exact `SessionId`, primary, and `ReceiverId`/`DeviceId` map before streaming state publication.
- [ ] **Step 7 (4 min): Close registrations deterministically.** On stop/restart/setup failure, finish the old registration after worker cancellation, preserve its last snapshot/events, and reject stale-generation updates. A new setup always allocates a fresh `SessionId`.
- [ ] **Step 8 (3 min): Keep tray/CLI boundaries safe.** Tray opens the existing Diagnostics navigation page. CLI self-test may read the same structured source but must not dump packet/audio/identity content; release GUI subsystem behavior remains unchanged.
- [ ] **Step 9 (3 min): Run integration and lifecycle tests.** Run `cargo test -p homepod-cast --test diagnostics_integration`, `cargo test -p homepod-cast --test backend_lifecycle`, and `cargo test -p homepod-cast --test recovery`.
- [ ] **Step 10 (2 min): Commit application wiring.** Run:

```powershell
git add crates/homepod-cast/tests/diagnostics_integration.rs crates/homepod-cast/src/app_handle.rs crates/homepod-cast/src/app/mod.rs crates/homepod-cast/src/app/effect.rs crates/homepod-cast/src/backend/session.rs crates/homepod-cast/src/cast.rs crates/homepod-cast/src/main.rs crates/homepod-cast/src/tray.rs
git commit -m "feat(app): wire the diagnostics control center"
```

### Task 14: Add truthfulness, redaction, and non-blocking regression gates

**Files:**
- Create: `crates/homepod-cast/tests/diagnostics_truthfulness.rs`
- Modify: `crates/homepod-cast/tests/diagnostics_export.rs`
- Modify: `crates/homepod-cast/tests/diagnostics_integration.rs`
- Modify: `crates/airplay-audio/tests/diagnostics.rs`
- Modify: `crates/airplay-client/tests/diagnostics.rs`

**Interfaces:**
- Consumes: all public snapshots, UI labels, support export bytes, bounded producers, and fake session flows completed above.
- Produces: no new production interface; this task makes prohibited claims and blocking regressions test failures.

- [ ] **Step 1 (5 min): Add a prohibited-language test.** Walk every diagnostics view-model label/help string and serialized export key/value; reject `packet loss`, `receiver buffer`, `network jitter`, `PTP RTT`, `clock accuracy`, `acoustic latency`, `automatically calibrated`, `guaranteed sync`, and `receiver delivered/rendered` except inside explicit negating limitation copy.
- [ ] **Step 2 (5 min): Add layer-invariant tests.** Three targets and ten shared frames must yield ten prepared frames and thirty target data attempts; local accepted counts may differ by target; repeated retransmit demand may exceed the shared frame count without being clamped or called loss.
- [ ] **Step 3 (4 min): Add unavailable-value tests.** NTP offset/path delay/drift, immature drift, missing feedback duration, and zero-denominator recovery demand serialize/render as absent or `Not measured`, never numeric zero.
- [ ] **Step 4 (4 min): Add non-blocking stress tests.** Fill diagnostic/capture/event channels, run 100,000 atomic counter updates, and assert producers complete without awaiting a UI/registry consumer; verify exact dropped-event/drop-frame counts after the consumer resumes.
- [ ] **Step 5 (5 min): Extend redaction fixtures across every event/error variant.** For each `DiagnosticErrorCode` and backend/client event payload, inject forbidden identities/content and assert the support document contains only stable code, safe operation, public message, aliases, counters, relative times, and units.
- [ ] **Step 6 (3 min): Run regression tests.** Run:

```powershell
cargo test -p airplay-audio --test diagnostics
cargo test -p airplay-client --test diagnostics
cargo test -p homepod-cast --test diagnostics_truthfulness
cargo test -p homepod-cast --test diagnostics_export
cargo test -p homepod-cast --test diagnostics_integration
```

- [ ] **Step 7 (2 min): Commit diagnostic safeguards.** Run:

```powershell
git add crates/homepod-cast/tests/diagnostics_truthfulness.rs crates/homepod-cast/tests/diagnostics_export.rs crates/homepod-cast/tests/diagnostics_integration.rs crates/airplay-audio/tests/diagnostics.rs crates/airplay-client/tests/diagnostics.rs
git commit -m "test(diagnostics): enforce truth and redaction"
```

### Task 15: Run repository verification and hardware release acceptance

**Files:**
- Verify only; no source change or commit is authorized by this task.

**Interfaces:**
- Consumes: the completed implementation and the hardware acceptance protocol from the binding spec.
- Produces: command output and external hardware notes; hardware notes containing network/device identity stay outside the support-safe export.

- [ ] **Step 1 (3 min): Scan for unfinished markers and forbidden new UI/export calls.** Run:

```powershell
rg -n "unimplemented!|panic!\(\"not implemented|loss_percent\(" crates/airplay-timing/src crates/airplay-audio/src crates/airplay-client/src crates/homepod-cast/src
rg -n "send_to|UdpSocket|RtpSender|packet_history|DeviceBackendHandle" crates/homepod-cast/src/diagnostics_window.rs crates/homepod-cast/src/ui
```

Review every match: the legacy compatible `loss_percent()` may remain only in `airplay-client/src/stats.rs`; diagnostics UI/export and incomplete production code must have no matches.
- [ ] **Step 2 (3 min): Verify formatting.** Run `cargo fmt --all -- --check`.
- [ ] **Step 3 (5 min): Run the complete automated suite.** Run `cargo test --workspace --all-targets`.
- [ ] **Step 4 (5 min): Run lint as an error.** Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 5 (5 min): Build the supported release binary.** Run `cargo build -p homepod-cast --release` on Windows MSVC.
- [ ] **Step 6 (5 min setup, 30 min observation): Validate single and three-receiver truth.** One receiver must show NTP sender-reference with no offset/path delay/drift/sync claim. Three HomePods must run for at least 30 minutes with fresh primary PTP measurement, shared secondary source rows, UI polling ≤4 Hz, no audible discontinuity/underrun from opening or closing Diagnostics, and shared/per-target counters obeying their layers.
- [ ] **Step 7 (5 min): Validate receiver interruption and identity continuity.** Interrupt one receiver; require receiver-scoped transport/feedback errors plus authoritative resilience lifecycle, no group packet-loss/acoustic claim, stable `ReceiverId`, and a fresh `SessionId` after any controlled restart.
- [ ] **Step 8 (10 min): Validate manual calibration direction and reset.** Apply a clearly audible offset, use the shared four-click test, verify onset direction by human listening and an external recording, then reset. Record mechanism success only; do not record or advertise an accuracy tolerance.
- [ ] **Step 9 (5 min): Validate calibration persistence.** Restart the application and reorder discovery; the same stable receiver retains its profile and profiles do not exchange.
- [ ] **Step 10 (5 min): Validate the support export.** Generate timing, retransmit, feedback, calibration, and induced-error events; export schema version 1; inspect the bytes for absence of device name/address/identity/path, packet/audio content, pairing material, and cryptographic values, and confirm gap/drop disclosure.
- [ ] **Step 11 (2 min): Confirm a clean implementation boundary.** Run `git status --short` and confirm only intentionally committed implementation remains. Do not create a verification-only commit.

## Spec Coverage Check

- Timing tasks 1–2 lock local-minus-master sign, mean path delay naming, live measurement sequence/age, bounded OLS drift, rolling-median staleness, shared secondary semantics, and NTP sender-reference unavailability.
- Audio tasks 3–5 cover actual sender/capture depth, non-blocking drops, exact scheduler interval/distribution/reset exclusions, shared prepared frames, per-target local UDP results/bytes, retransmit slots/outcomes, and unsafe trace removal.
- Client task 6 covers stable target mapping, timing roles, RTSP/playback state, structured errors, feedback result/duration, and compatibility without using the legacy loss calculation.
- Application tasks 7–8 cover `ReceiverSessionKey`, schema/sequence snapshots, exactly one registry, 4,096-event ring, cursor limit/gap semantics, producer drops, current/recent sessions, authoritative lifecycle, and conservative health.
- Calibration tasks 9–10 cover stable persistence/migration, manual clicks, normalization, checked/future time, one controlled restart on apply/reset, per-target PT=84/PT=87 sync, independent sync state, and preservation of the shared RTP/PTP/audio timeline.
- Export task 11 covers media type/top-level schema, RFC3339 time, exact semantics, per-export aliases, allow-listed redaction, safe summary reuse, event gap/drop disclosure, and off-hot-path serialization/file I/O.
- UI task 12 covers Overview, Speakers, Calibration, Events, Advanced, Windows Calm/accessibility, truthful labels, local filters, and visible-only coalesced 4 Hz polling.
- Integration and gate tasks 13–15 cover predecessor ownership, one read-only `AppHandle` accessor, session registration/retention, stale generation rejection, all prohibited claims, full repository verification, and every hardware acceptance item.
