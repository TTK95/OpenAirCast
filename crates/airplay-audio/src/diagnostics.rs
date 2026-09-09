//! Truthful runtime diagnostics for the audio pipeline.
//!
//! Two halves exist, and a snapshot always fills exactly one of them with
//! real state while the other half stays at all-zero defaults:
//!
//! - **Sender buffer** ([`crate::buffer::AudioBuffer`]): ring-buffer fill,
//!   monotonic write/read totals, underrun events.
//! - **Capture queue** ([`crate::live_decoder::LiveFrameSender`] →
//!   [`crate::live_decoder::LiveAudioDecoder`]): submission/drop counters and
//!   current queue length.
//!
//! Every number comes from actual pipeline state (atomics maintained at the
//! exact mutation sites); nothing is estimated or invented.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// Snapshot of scheduler jitter as observed by the packet sender loop.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SchedulerJitterSnapshot {
    pub sample_count: u64,
    pub last_signed_jitter_ns: Option<i64>,
    pub mean_absolute_jitter_ns: Option<u64>,
    pub p95_absolute_jitter_ns: Option<u64>,
    pub maximum_absolute_jitter_ns: Option<u64>,
    pub deadline_resets_total: u64,
}

/// Snapshot of one send target's UDP transport activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TargetTransportSnapshot {
    pub data_datagrams_attempted_total: u64,
    pub data_datagrams_accepted_local_total: u64,
    pub data_send_failures_total: u64,
    pub data_bytes_sent_total: u64,
    pub sync_datagrams_attempted_total: u64,
    pub sync_datagrams_accepted_local_total: u64,
    pub sync_send_failures_total: u64,
    pub sync_bytes_sent_total: u64,
}

/// Cumulative snapshot of retransmit-request handling at streamer level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RetransmitSnapshot {
    /// Retransmit requests (PT=85 datagrams) handled, never deduplicated.
    pub retransmit_request_datagrams_total: u64,
    /// Sum of requested sequence slots across all handled requests.
    pub retransmit_packet_slots_requested_total: u64,
    /// Slots served from packet history whose response datagram the local OS
    /// accepted.
    pub retransmit_datagrams_accepted_local_total: u64,
    /// Requested sequences absent (or stale) in the per-target history ring.
    pub retransmit_history_misses_total: u64,
    /// Served slots whose response datagram send returned an error.
    pub retransmit_send_failures_total: u64,
}

/// Per-request truth about how each requested sequence slot was handled.
///
/// Every requested slot lands in exactly one of `accepted_local`,
/// `history_misses`, or `send_failures`; the three always sum to
/// `requested_slots`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RetransmitOutcome {
    /// Total slots in the request.
    pub requested_slots: u16,
    /// Slots served from history and locally accepted by the OS.
    pub accepted_local: u16,
    /// Requested sequences not present in the history ring.
    pub history_misses: u16,
    /// Served slots whose send returned an error.
    pub send_failures: u16,
}

/// Snapshot of the sender-side ring buffer.
///
/// Produced by an [`AudioDiagnosticsSource`] created from
/// [`crate::buffer::AudioBuffer::diagnostics_source`].
#[derive(Clone, Debug, PartialEq)]
pub struct SenderBufferSnapshot {
    /// Frames currently queued in the ring buffer.
    pub queued_frames: u32,
    /// Total ring capacity in frames.
    pub capacity_frames: u32,
    /// PCM samples per channel currently queued (exact mirror, updated on
    /// every push/pop/clear/flush).
    pub queued_samples_per_channel: u64,
    /// Queued audio duration in nanoseconds, computed as
    /// `queued_samples_per_channel * 1_000_000_000 / sample_rate_hz` using
    /// exact integer math (u128 intermediate). Preferred over deriving from
    /// the millisecond-rounded `buffered_ms()`.
    pub buffered_ns: u64,
    /// `queued_frames / capacity_frames`, clamped to `0.0..=1.0`.
    pub fill_ratio: f32,
    /// Monotonic total of samples-per-channel successfully pushed.
    pub samples_written_total: u64,
    /// Monotonic total of samples-per-channel returned by `pop()`.
    /// Frames removed via `clear()`/`flush()` leave the queue without
    /// counting as reads (pre-existing semantics).
    pub samples_read_total: u64,
    /// Monotonic count of reads that hit an empty buffer (`pop()` returning
    /// `None`). Non-consuming checks (`peek`, `is_empty`) do not count.
    pub underrun_events_total: u64,
}

/// Snapshot of the capture queue feeding a [`crate::live_decoder::LiveAudioDecoder`].
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureQueueSnapshot {
    /// Frames currently in the channel (mirror counter: incremented on each
    /// successful send, decremented on each received frame; approximate under
    /// concurrency, never blocking).
    pub queued_frames: u32,
    /// Channel capacity in frames as configured at pair creation.
    pub capacity_frames: u32,
    /// Monotonic total of submit attempts (successes AND drops).
    pub submitted_frames_total: u64,
    /// Monotonic total samples-per-channel of every attempted frame
    /// (successes AND drops), keeping this consistent with
    /// `submitted_frames_total`.
    pub submitted_samples_per_channel_total: u64,
    /// Attempts rejected because the bounded channel was full.
    pub full_queue_drops_total: u64,
    /// Attempts rejected because the receiver side was dropped.
    pub disconnected_drops_total: u64,
}

/// Combined point-in-time diagnostics snapshot.
///
/// Exactly one half carries live values; the other half is all-zero
/// ("not applicable"), depending on which pipeline component produced the
/// source. See [`AudioDiagnosticsSource`].
#[derive(Clone, Debug, PartialEq)]
pub struct AudioDiagnosticsSnapshot {
    /// Ring-buffer half (zeroed for capture-queue-backed sources).
    pub sender_buffer: SenderBufferSnapshot,
    /// Capture-queue half (zeroed for AudioBuffer-backed sources).
    pub capture_queue: CaptureQueueSnapshot,
    /// Scheduler jitter statistics (zeroed default when the source has no
    /// streamer-side handle attached).
    pub scheduler: SchedulerJitterSnapshot,
    /// Frames pulled from the buffer, encoded and handed to the transport.
    /// Mirrors [`crate::streamer::AudioStreamer::packets_sent`] (the
    /// "prepared" counter is the same atomic as the "sent" one; a frame counts
    /// once it was encoded and queued/sent towards the targets).
    pub rtp_frames_prepared_total: u64,
    /// Per-target transport counters in target-index order (same order as the
    /// configured RTP senders). Empty when no targets are registered.
    pub targets: Vec<TargetTransportSnapshot>,
    /// Cumulative retransmit-handling counters (zeroed for sources that have
    /// no streamer-side retransmit handle attached).
    pub retransmit: RetransmitSnapshot,
}

/// Shared counters maintained by [`crate::buffer::AudioBuffer`] at the exact
/// mutation sites (push success, pop success, pop-on-empty, clear, flush).
#[derive(Default)]
pub(crate) struct SenderBufferCounters {
    pub(crate) queued_frames: AtomicU32,
    pub(crate) queued_samples_per_channel: AtomicU64,
    pub(crate) samples_written_total: AtomicU64,
    pub(crate) samples_read_total: AtomicU64,
    pub(crate) underrun_events_total: AtomicU64,
}

/// Shared counters maintained by [`crate::live_decoder::LiveFrameSender`] at
/// the exact submit sites.
///
/// The current queue length is intentionally NOT mirrored here:
/// [`crossbeam_channel::Sender::len`] already reports the true channel
/// occupancy without blocking, which stays truthful even when frames are
/// consumed by a separately constructed receiver.
pub(crate) struct CaptureQueueCounters {
    /// Channel capacity in frames as configured at pair creation (fixed).
    pub(crate) capacity_frames: u32,
    pub(crate) submitted_frames_total: AtomicU64,
    pub(crate) submitted_samples_per_channel_total: AtomicU64,
    pub(crate) full_queue_drops_total: AtomicU64,
    pub(crate) disconnected_drops_total: AtomicU64,
}

impl CaptureQueueCounters {
    pub(crate) fn with_capacity(capacity_frames: usize) -> Self {
        Self {
            capacity_frames: u32::try_from(capacity_frames).unwrap_or(u32::MAX),
            submitted_frames_total: AtomicU64::new(0),
            submitted_samples_per_channel_total: AtomicU64::new(0),
            full_queue_drops_total: AtomicU64::new(0),
            disconnected_drops_total: AtomicU64::new(0),
        }
    }
}

/// Samples per channel of a PCM frame; frames with zero channels count as
/// zero samples instead of panicking on division.
pub(crate) fn pcm_frame_samples_per_channel(frame: &super::LivePcmFrame) -> u64 {
    if frame.channels == 0 {
        return 0;
    }
    (frame.samples.len() / frame.channels as usize) as u64
}

fn samples_to_ns(samples: u64, sample_rate_hz: u32) -> u64 {
    if sample_rate_hz == 0 {
        return 0;
    }
    // Exact integer math; the u128 intermediate makes overflow impossible for
    // any realistic queue depth (would need > ~4 days of continuous audio).
    ((samples as u128 * 1_000_000_000u128) / sample_rate_hz as u128) as u64
}

fn fill_ratio(queued_frames: u32, capacity_frames: u32) -> f32 {
    if capacity_frames == 0 {
        return 0.0;
    }
    (queued_frames as f32 / capacity_frames as f32).min(1.0)
}

impl SenderBufferSnapshot {
    fn zeroed() -> Self {
        Self {
            queued_frames: 0,
            capacity_frames: 0,
            queued_samples_per_channel: 0,
            buffered_ns: 0,
            fill_ratio: 0.0,
            samples_written_total: 0,
            samples_read_total: 0,
            underrun_events_total: 0,
        }
    }
}

impl CaptureQueueSnapshot {
    fn zeroed() -> Self {
        Self {
            queued_frames: 0,
            capacity_frames: 0,
            submitted_frames_total: 0,
            submitted_samples_per_channel_total: 0,
            full_queue_drops_total: 0,
            disconnected_drops_total: 0,
        }
    }
}

/// Handle that produces [`AudioDiagnosticsSnapshot`]s from live pipeline
/// state.
///
/// The source is cheap to clone and keeps only `Arc` handles to shared
/// atomic counters, so snapshots never block the audio path.
///
/// # Half asymmetry
///
/// A source built from an [`crate::buffer::AudioBuffer`] reports live values
/// in `sender_buffer` and all-zero defaults in `capture_queue` (the ring has
/// no capture queue — "not applicable", not fabricated numbers).
/// A source built from a [`crate::live_decoder::LiveFrameSender`] does the
/// opposite: live `capture_queue`, zeroed `sender_buffer`.
#[derive(Clone)]
pub struct AudioDiagnosticsSource {
    build_snapshot: Arc<dyn Fn() -> AudioDiagnosticsSnapshot + Send + Sync>,
}

impl AudioDiagnosticsSource {
    pub(crate) fn new(
        build_snapshot: Arc<dyn Fn() -> AudioDiagnosticsSnapshot + Send + Sync>,
    ) -> Self {
        Self { build_snapshot }
    }

    /// Build an [`AudioDiagnosticsSource`] backed by sender-ring counters.
    pub(crate) fn sender_buffer(
        counters: Arc<SenderBufferCounters>,
        capacity_frames: u32,
        sample_rate_hz: u32,
    ) -> Self {
        Self::new(Arc::new(move || {
            let queued_frames = counters.queued_frames.load(Ordering::Relaxed);
            let queued_samples = counters.queued_samples_per_channel.load(Ordering::Relaxed);
            AudioDiagnosticsSnapshot {
                sender_buffer: SenderBufferSnapshot {
                    queued_frames,
                    capacity_frames,
                    queued_samples_per_channel: queued_samples,
                    buffered_ns: samples_to_ns(queued_samples, sample_rate_hz),
                    fill_ratio: fill_ratio(queued_frames, capacity_frames),
                    samples_written_total: counters.samples_written_total.load(Ordering::Relaxed),
                    samples_read_total: counters.samples_read_total.load(Ordering::Relaxed),
                    underrun_events_total: counters.underrun_events_total.load(Ordering::Relaxed),
                },
                capture_queue: CaptureQueueSnapshot::zeroed(),
                scheduler: SchedulerJitterSnapshot::default(),
                rtp_frames_prepared_total: 0,
                targets: Vec::new(),
                retransmit: RetransmitSnapshot::default(),
            }
        }))
    }

    /// Build an [`AudioDiagnosticsSource`] backed by capture-queue counters.
    ///
    /// `current_queued_frames` is read live from the channel on every
    /// snapshot (non-blocking), so it is passed as a closure input rather
    /// than stored.
    pub(crate) fn capture_queue(
        counters: Arc<CaptureQueueCounters>,
        current_queued_frames: impl Fn() -> usize + Send + Sync + 'static,
    ) -> Self {
        Self::new(Arc::new(move || AudioDiagnosticsSnapshot {
            sender_buffer: SenderBufferSnapshot::zeroed(),
            capture_queue: CaptureQueueSnapshot {
                queued_frames: u32::try_from(current_queued_frames()).unwrap_or(u32::MAX),
                capacity_frames: counters.capacity_frames,
                submitted_frames_total: counters.submitted_frames_total.load(Ordering::Relaxed),
                submitted_samples_per_channel_total: counters
                    .submitted_samples_per_channel_total
                    .load(Ordering::Relaxed),
                full_queue_drops_total: counters.full_queue_drops_total.load(Ordering::Relaxed),
                disconnected_drops_total: counters.disconnected_drops_total.load(Ordering::Relaxed),
            },
            scheduler: SchedulerJitterSnapshot::default(),
            rtp_frames_prepared_total: 0,
            targets: Vec::new(),
            retransmit: RetransmitSnapshot::default(),
        }))
    }

    /// Take a point-in-time snapshot of the underlying counters.
    ///
    /// Individual fields are loaded independently with relaxed ordering, so a
    /// snapshot under concurrent mutation may mix slightly different instants;
    /// every value is nonetheless a real counter/queue value, never invented.
    pub fn snapshot(&self) -> AudioDiagnosticsSnapshot {
        (self.build_snapshot)()
    }

    /// Wrap an existing source so that every snapshot additionally carries
    /// streamer-side telemetry: scheduler jitter, the prepared-frames total
    /// and per-target transport counters. The base source's halves stay live.
    pub(crate) fn with_stream_telemetry(
        base: AudioDiagnosticsSource,
        jitter: SchedulerJitterRecorder,
        frames_prepared: Arc<AtomicU64>,
        targets: Arc<RwLock<Vec<Arc<TargetTransportCounters>>>>,
    ) -> Self {
        Self::new(Arc::new(move || {
            let mut snap = base.snapshot();
            snap.scheduler = jitter.snapshot();
            snap.rtp_frames_prepared_total = frames_prepared.load(Ordering::Relaxed);
            snap.targets = match targets.read() {
                Ok(registry) => registry.iter().map(|c| c.snapshot()).collect(),
                Err(poisoned) => poisoned.into_inner().iter().map(|c| c.snapshot()).collect(),
            };
            snap
        }))
    }

    /// Wrap an existing source so that every snapshot additionally carries the
    /// cumulative retransmit counters (mirrors
    /// [`Self::with_stream_telemetry`]). The wrapped source's fields stay live.
    pub(crate) fn with_retransmit_counters(
        base: AudioDiagnosticsSource,
        counters: Arc<RetransmitCounters>,
    ) -> Self {
        Self::new(Arc::new(move || {
            let mut snap = base.snapshot();
            snap.retransmit = counters.snapshot();
            snap
        }))
    }
}

/// Capacity of the bounded scheduler-jitter ring: only the most recent 1024
/// absolute jitter samples participate in mean/p95/max statistics.
const SCHEDULER_JITTER_RING_CAPACITY: usize = 1024;

/// Lock-free bounded ring of recent scheduler-jitter samples.
///
/// All state lives in atomics touched with relaxed ordering by a single
/// producer thread (the packet sender loop); consumers never lock or block
/// the producer. Under concurrent access a snapshot is allowed to mix
/// slightly different instants (every value stays a real recorded sample).
impl Default for SchedulerJitterRing {
    fn default() -> Self {
        Self {
            absolute_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            write_index: AtomicUsize::new(0),
            event_count: AtomicU64::new(0),
            jitter_count: AtomicU64::new(0),
            last_signed_ns: AtomicI64::new(0),
            deadline_resets_total: AtomicU64::new(0),
        }
    }
}

struct SchedulerJitterRing {
    /// Absolute (`|ns|`) jitter samples indexed by `write_index % capacity`.
    absolute_ns: [AtomicU64; SCHEDULER_JITTER_RING_CAPACITY],
    /// Next ring slot (monotonic; wraps modulo capacity on use).
    write_index: AtomicUsize,
    /// Total recorded events: jitter samples AND deadline resets. Exposed as
    /// `sample_count` in the snapshot.
    event_count: AtomicU64,
    /// Total jitter samples ever recorded (drives ring validity and whether
    /// signed/percentile statistics are `Some`).
    jitter_count: AtomicU64,
    /// Signed jitter of the most recently recorded sample.
    last_signed_ns: AtomicI64,
    deadline_resets_total: AtomicU64,
}

impl std::fmt::Debug for SchedulerJitterRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerJitterRing")
            .field("capacity", &SCHEDULER_JITTER_RING_CAPACITY)
            .finish_non_exhaustive()
    }
}

/// Records scheduler jitter and deadline re-anchoring events for diagnostics.
///
/// Cheap to clone (shared `Arc` ring); recording never allocates or blocks.
#[derive(Clone, Debug, Default)]
pub struct SchedulerJitterRecorder(Arc<SchedulerJitterRing>);

impl SchedulerJitterRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one signed jitter observation (nanoseconds vs. nominal
    /// schedule). Only the absolute value enters the bounded statistics ring;
    /// the signed value is kept as `last_signed_jitter_ns`.
    pub fn record_jitter(&self, signed_ns: i64) {
        let ring = &*self.0;
        let slot =
            ring.write_index.fetch_add(1, Ordering::Relaxed) % SCHEDULER_JITTER_RING_CAPACITY;
        ring.absolute_ns[slot].store(signed_ns.unsigned_abs(), Ordering::Relaxed);
        ring.last_signed_ns.store(signed_ns, Ordering::Relaxed);
        ring.jitter_count.fetch_add(1, Ordering::Relaxed);
        ring.event_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that the send-schedule deadline was reset/re-anchored (e.g.
    /// after resume or when the sender fell behind).
    pub fn record_deadline_reset(&self) {
        self.0.deadline_resets_total.fetch_add(1, Ordering::Relaxed);
        self.0.event_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Current statistics over the recorded events.
    ///
    /// `mean`/`p95`/`max` cover only the up-to-1024 most recent jitter
    /// samples; `p95` is the `ceil(0.95*n)`-th smallest of a sorted copy.
    pub fn snapshot(&self) -> SchedulerJitterSnapshot {
        let ring = &*self.0;
        let jitter_total = ring.jitter_count.load(Ordering::Relaxed);
        let valid = (jitter_total as usize).min(SCHEDULER_JITTER_RING_CAPACITY);

        let mut sorted = Vec::with_capacity(valid);
        let head = ring.write_index.load(Ordering::Relaxed);
        let start = head + SCHEDULER_JITTER_RING_CAPACITY - valid;
        for k in 0..valid {
            let slot = (start + k) % SCHEDULER_JITTER_RING_CAPACITY;
            sorted.push(ring.absolute_ns[slot].load(Ordering::Relaxed));
        }
        sorted.sort_unstable();

        let stats = if valid == 0 {
            (None, None, None)
        } else {
            let sum: u128 = sorted.iter().map(|v| *v as u128).sum();
            let mean = Some((sum / valid as u128) as u64);
            // ceil(0.95 * n) as a 1-based rank, converted to a 0-based index.
            let rank = (19 * valid).div_ceil(20);
            (mean, Some(sorted[rank - 1]), Some(*sorted.last().unwrap()))
        };

        SchedulerJitterSnapshot {
            sample_count: ring.event_count.load(Ordering::Relaxed),
            last_signed_jitter_ns: if jitter_total > 0 {
                Some(ring.last_signed_ns.load(Ordering::Relaxed))
            } else {
                None
            },
            mean_absolute_jitter_ns: stats.0,
            p95_absolute_jitter_ns: stats.1,
            maximum_absolute_jitter_ns: stats.2,
            deadline_resets_total: ring.deadline_resets_total.load(Ordering::Relaxed),
        }
    }

    /// Test helper: build a recorder pre-filled with the given signed jitter
    /// samples (as if they had been recorded in order).
    #[doc(hidden)]
    pub fn from_samples(samples: &[i64]) -> Self {
        let recorder = Self::new();
        for &signed_ns in samples {
            recorder.record_jitter(signed_ns);
        }
        recorder
    }
}

/// Per-target UDP transport counters maintained at the actual socket-send
/// sites (`attempted` is bumped before the syscall, outcome afterwards).
#[derive(Debug, Default)]
pub struct TargetTransportCounters {
    data_attempted_total: AtomicU64,
    data_accepted_local_total: AtomicU64,
    data_send_failures_total: AtomicU64,
    data_bytes_sent_total: AtomicU64,
    sync_attempted_total: AtomicU64,
    sync_accepted_local_total: AtomicU64,
    sync_send_failures_total: AtomicU64,
    sync_bytes_sent_total: AtomicU64,
}

impl TargetTransportCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bump_data_attempted(&self) {
        self.data_attempted_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Account one data-datagram send result: `Ok(len>0)` counts as locally
    /// accepted plus byte total, `Err` as failure. `Ok(0)` counts as neither.
    pub(crate) fn record_data_outcome(&self, sent: &std::io::Result<usize>) {
        match sent {
            Ok(len) if *len > 0 => {
                self.data_accepted_local_total
                    .fetch_add(1, Ordering::Relaxed);
                self.data_bytes_sent_total
                    .fetch_add(*len as u64, Ordering::Relaxed);
            }
            Err(_) => {
                self.data_send_failures_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
        }
    }

    pub(crate) fn bump_sync_attempted(&self) {
        self.sync_attempted_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Account one sync/timing-datagram send result (same semantics as
    /// [`Self::record_data_outcome`]).
    pub(crate) fn record_sync_outcome(&self, sent: &std::io::Result<usize>) {
        match sent {
            Ok(len) if *len > 0 => {
                self.sync_accepted_local_total
                    .fetch_add(1, Ordering::Relaxed);
                self.sync_bytes_sent_total
                    .fetch_add(*len as u64, Ordering::Relaxed);
            }
            Err(_) => {
                self.sync_send_failures_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
        }
    }

    /// Point-in-time snapshot (relaxed loads; fields may mix instants under
    /// concurrent sends but each value is real).
    pub fn snapshot(&self) -> TargetTransportSnapshot {
        TargetTransportSnapshot {
            data_datagrams_attempted_total: self.data_attempted_total.load(Ordering::Relaxed),
            data_datagrams_accepted_local_total: self
                .data_accepted_local_total
                .load(Ordering::Relaxed),
            data_send_failures_total: self.data_send_failures_total.load(Ordering::Relaxed),
            data_bytes_sent_total: self.data_bytes_sent_total.load(Ordering::Relaxed),
            sync_datagrams_attempted_total: self.sync_attempted_total.load(Ordering::Relaxed),
            sync_datagrams_accepted_local_total: self
                .sync_accepted_local_total
                .load(Ordering::Relaxed),
            sync_send_failures_total: self.sync_send_failures_total.load(Ordering::Relaxed),
            sync_bytes_sent_total: self.sync_bytes_sent_total.load(Ordering::Relaxed),
        }
    }
}

/// Cumulative retransmit counters maintained by the streamer at the exact
/// site where a retransmit request is handled (relaxed atomics only; the hot
/// control path never blocks or locks for diagnostics).
#[derive(Debug, Default)]
pub(crate) struct RetransmitCounters {
    pub(crate) request_datagrams_total: AtomicU64,
    pub(crate) packet_slots_requested_total: AtomicU64,
    pub(crate) datagrams_accepted_local_total: AtomicU64,
    pub(crate) history_misses_total: AtomicU64,
    pub(crate) send_failures_total: AtomicU64,
}

impl RetransmitCounters {
    /// Fold one handled request (with its per-slot outcome) into the totals.
    pub(crate) fn record_request(&self, outcome: &RetransmitOutcome) {
        self.request_datagrams_total.fetch_add(1, Ordering::Relaxed);
        self.packet_slots_requested_total
            .fetch_add(u64::from(outcome.requested_slots), Ordering::Relaxed);
        self.datagrams_accepted_local_total
            .fetch_add(u64::from(outcome.accepted_local), Ordering::Relaxed);
        self.history_misses_total
            .fetch_add(u64::from(outcome.history_misses), Ordering::Relaxed);
        self.send_failures_total
            .fetch_add(u64::from(outcome.send_failures), Ordering::Relaxed);
    }

    /// Point-in-time snapshot of the cumulative counters.
    pub fn snapshot(&self) -> RetransmitSnapshot {
        RetransmitSnapshot {
            retransmit_request_datagrams_total: self
                .request_datagrams_total
                .load(Ordering::Relaxed),
            retransmit_packet_slots_requested_total: self
                .packet_slots_requested_total
                .load(Ordering::Relaxed),
            retransmit_datagrams_accepted_local_total: self
                .datagrams_accepted_local_total
                .load(Ordering::Relaxed),
            retransmit_history_misses_total: self.history_misses_total.load(Ordering::Relaxed),
            retransmit_send_failures_total: self.send_failures_total.load(Ordering::Relaxed),
        }
    }
}
