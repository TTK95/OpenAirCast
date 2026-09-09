//! High-level audio streaming orchestrator.

use crate::diagnostics::{
    AudioDiagnosticsSource, RetransmitCounters, RetransmitOutcome, SchedulerJitterRecorder,
    TargetTransportCounters,
};
use crate::encoder::{create_encoder, AudioEncoder};
use crate::eq::{EqConfig, EqParams, Equalizer};
use crate::{AudioBuffer, AudioDecoder, LiveAudioDecoder, RtpSender};
use airplay_core::{error::Result, StreamConfig};
use airplay_timing::{local_to_master_ns, unix_to_ntp, Clock, ClockOffset, PtpClockState};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::net::{SocketAddr, UdpSocket};
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    Arc, RwLock,
};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration, Instant};

/// Budget for joining the dedicated RT sender thread during [`AudioStreamer::stop`].
///
/// Kept far below every caller's teardown budget so stopping a streamer can
/// never be the reason a disconnect misses its own deadline.
const SENDER_THREAD_JOIN_BUDGET: Duration = Duration::from_millis(500);

/// Streaming state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamerState {
    /// Not started.
    Idle,
    /// Buffering initial audio.
    Buffering,
    /// Actively streaming.
    Streaming,
    /// Paused.
    Paused,
    /// Stopped.
    Stopped,
    /// Error occurred.
    Error,
}

/// Try to set real-time priority for the current thread (Linux only).
#[cfg(target_os = "linux")]
fn set_realtime_priority() {
    use std::mem;
    unsafe {
        let param: libc::sched_param = mem::zeroed();
        let mut param = param;
        param.sched_priority = 50; // RT priority 50 (1-99 scale)

        let result = libc::sched_setscheduler(
            0, // current thread
            libc::SCHED_FIFO,
            &param as *const _,
        );

        if result == 0 {
            tracing::info!("Set real-time priority (SCHED_FIFO, priority 50)");
        } else {
            tracing::warn!(
                "Failed to set RT priority (need CAP_SYS_NICE or root): errno={}",
                *libc::__errno_location()
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn set_realtime_priority() {
    tracing::debug!("RT priority not supported on this platform");
}

/// Disable WiFi power save to prevent packet loss from radio sleep (Linux only).
///
/// WiFi power management causes the radio to periodically sleep, which drops
/// outgoing packets during sleep windows. For real-time audio streaming this
/// is a major source of packet loss, especially on Raspberry Pi.
#[cfg(target_os = "linux")]
fn disable_wifi_power_save() {
    use std::process::Command;
    // Try common wireless interface names
    for iface in &["wlan0", "wlp2s0", "wlp3s0"] {
        match Command::new("iw")
            .args([*iface, "set", "power_save", "off"])
            .output()
        {
            Ok(output) if output.status.success() => {
                tracing::info!("Disabled WiFi power save on {}", iface);
                return;
            }
            _ => {}
        }
    }
    tracing::debug!(
        "Could not disable WiFi power save (no wireless interface found or no permissions)"
    );
}

#[cfg(not(target_os = "linux"))]
fn disable_wifi_power_save() {}

/// Message sent from the async producer to the dedicated sender thread.
enum SenderMessage {
    /// A fully serialized packet ready for timed transmission.
    Packet {
        /// Pre-serialized wire bytes per target (one entry per SendTarget).
        /// Single-device: 1 entry. Group: N entries (each encrypted with device-specific cipher).
        wire_packets: Vec<Vec<u8>>,
        /// Pre-serialized sync packet bytes **per target**, in the same order
        /// as `wire_packets` and always of the same length. `None` means no
        /// sync was due (or the target does not use a control port) — never
        /// "reuse a neighbour's packet".
        ///
        /// Sync packets carry the per-target presentation time, so they can
        /// no longer be shared: each entry belongs to exactly one target and
        /// is dispatched only to that target's control destination.
        sync_packets: Vec<Option<Vec<u8>>>,
    },
    /// Pause: sender thread should stop advancing deadlines and wait for Resume.
    Pause,
    /// Resume: reset deadline to now and resume sending.
    Resume,
    /// Stop: sender thread should exit.
    Stop,
}

// ===========================================================================
// Per-target presentation clocks over one shared RTP stream
// ===========================================================================

/// Why a target's presentation time could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PresentationTimeError {
    /// The sum of anchor, render lead and calibration delay leaves the
    /// representable nanosecond range.
    #[error("presentation time does not fit into 64 bits of nanoseconds")]
    Overflow,
    /// The computed time does not lie strictly after the reference master
    /// time — telling a receiver to render at or before a moment that has
    /// already passed is not a presentation time.
    #[error("presentation time does not lie after the reference master time")]
    NotInFuture,
}

/// Why a whole frame could not be prepared for its targets.
#[derive(Debug, thiserror::Error)]
pub enum PrepareFrameError {
    /// One presentation delay per target is required; zero is a valid delay,
    /// a missing entry is not.
    #[error("{delays} presentation delay(s) supplied for {targets} target(s)")]
    TargetCountMismatch {
        /// Number of RTP senders in the fan-out.
        targets: usize,
        /// Number of supplied delays.
        delays: usize,
    },
    /// A target's presentation time could not be computed. Reported before
    /// any sender is mutated, so the frame is rejected as a whole.
    #[error("target {index}: {source}")]
    PresentationTime {
        /// Index of the offending target.
        index: usize,
        /// The rejected computation.
        source: PresentationTimeError,
    },
    /// Serializing a packet failed (encryption, missing encoder state, ...).
    #[error(transparent)]
    Packet(#[from] airplay_core::error::Error),
}

impl From<PrepareFrameError> for airplay_core::error::Error {
    fn from(value: PrepareFrameError) -> Self {
        match value {
            PrepareFrameError::Packet(inner) => inner,
            other => airplay_core::error::Error::Streaming(
                airplay_core::error::StreamingError::Encoding(other.to_string()),
            ),
        }
    }
}

/// Compute one target's presentation time with checked arithmetic.
///
/// ```text
/// presentation = common_master_time + global_render_lead + effective_delay
/// ```
///
/// `common_master_time_ns` is the anchor shared by every group member — it is
/// derived from the shared RTP stream and never carries calibration.
/// `effective_delay_ns` is that target's normalized, non-negative calibration
/// delay (the earliest speaker keeps zero). `now_master_time_ns` is a master
/// time already known to have passed; the result must lie strictly after it,
/// which also catches a master clock that stopped advancing or jumped back.
pub fn checked_presentation_time_ns(
    common_master_time_ns: u64,
    global_render_lead_ns: u64,
    effective_delay_ns: u64,
    now_master_time_ns: u64,
) -> std::result::Result<u64, PresentationTimeError> {
    let presentation = common_master_time_ns
        .checked_add(global_render_lead_ns)
        .and_then(|value| value.checked_add(effective_delay_ns))
        .ok_or(PresentationTimeError::Overflow)?;

    if presentation <= now_master_time_ns {
        return Err(PresentationTimeError::NotInFuture);
    }

    Ok(presentation)
}

/// Convert the sender's local wall clock to the group's master timeline.
///
/// Every producer of a [`ClockOffset`] in this workspace measures it as
/// `local - master` — `PtpClient::calculate_offset` and the BMCA yield flow
/// both compute `((t2 - t1) + (t3 - t4)) / 2` with `t1`/`t4` taken from the
/// master's wire timestamps and `t2`/`t3` from the local clock. Converting a
/// local timestamp to master time therefore **subtracts** the offset.
///
/// This anchor is what a receiver reads out of the PT=84/PT=87 sync packet to
/// decide when to render, so the sign is directly audible. Without a measured
/// offset — NTP mode and PTP-master mode both report zero — and for a
/// conversion that would leave the representable range, the local time is used
/// unchanged rather than wrapping.
fn master_time_for_anchor_ns(local_wall_ns: u64, offset: Option<ClockOffset>) -> u64 {
    match offset {
        Some(offset) => {
            local_to_master_ns(local_wall_ns, offset.offset_ns).unwrap_or(local_wall_ns)
        }
        None => local_wall_ns,
    }
}

/// Everything one encoded frame contributes to **every** target.
///
/// All fields describe the shared stream. Nothing here is per-target: the
/// only per-target quantity is the calibration delay passed alongside this
/// plan, and it may only reach the sync packet.
pub struct FramePlan<'a> {
    /// RTP payload type of the audio packet.
    pub payload_type: u8,
    /// Shared RTP timestamp of this frame — identical for every target.
    pub rtp_timestamp: u32,
    /// Shared RTP timestamp announced as "next" in PT=87 sync packets.
    pub next_rtp_timestamp: u32,
    /// Shared encoded audio payload (one encode for the whole group).
    pub payload: &'a [u8],
    /// Shared RTP marker bit.
    pub marker: bool,
    /// Whether a sync packet is due for this frame (shared cadence).
    pub sync_due: bool,
    /// Emit PT=87 PTP sync instead of PT=84 NTP sync.
    pub use_ptp_sync: bool,
    /// Shared PTP master clock identity carried in PT=87 packets.
    pub ptp_master_clock_id: [u8; 8],
    /// Shared anchor on the master timeline, in nanoseconds.
    pub common_master_time_ns: u64,
    /// Render lead applied to every target, in nanoseconds.
    pub global_render_lead_ns: u64,
    /// A master time already known to have passed; presentation times must
    /// lie strictly after it.
    pub reference_master_time_ns: u64,
}

/// One frame serialized for every target.
///
/// Both vectors have exactly `targets.len()` entries, so the sender thread
/// can zip target, audio packet and sync packet by index without a length
/// check of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFrame {
    /// Audio wire bytes per target (per-device encryption, shared content).
    pub wire_packets: Vec<Vec<u8>>,
    /// Sync wire bytes per target, `None` where no sync was emitted.
    pub sync_packets: Vec<Option<Vec<u8>>>,
}

/// Serialize one shared frame for every target, applying per-target
/// presentation delays to the sync packets only.
///
/// The audio packets are built from the same `payload`, the same
/// `rtp_timestamp` and the same marker for every target — a calibration delay
/// can therefore never desynchronize the shared RTP stream. It moves only the
/// clock anchor inside each target's own PT=84/PT=87 sync packet, which is
/// exactly the value the receiver uses to decide *when* to render the samples
/// it is given.
///
/// Every presentation time is computed and validated **before** the first
/// sender is touched, so a rejected frame leaves all sequence numbers,
/// first-sync extension flags, packet histories and transport counters
/// untouched.
pub fn prepare_frame_for_targets(
    senders: &mut [RtpSender],
    presentation_delays_ns: &[u64],
    plan: &FramePlan<'_>,
) -> std::result::Result<PreparedFrame, PrepareFrameError> {
    if senders.len() != presentation_delays_ns.len() {
        return Err(PrepareFrameError::TargetCountMismatch {
            targets: senders.len(),
            delays: presentation_delays_ns.len(),
        });
    }

    // Phase 1: validate. Nothing is mutated while this can still fail.
    let mut presentation_times = Vec::with_capacity(senders.len());
    if plan.sync_due {
        for (index, delay_ns) in presentation_delays_ns.iter().enumerate() {
            let presentation_ns = checked_presentation_time_ns(
                plan.common_master_time_ns,
                plan.global_render_lead_ns,
                *delay_ns,
                plan.reference_master_time_ns,
            )
            .map_err(|source| PrepareFrameError::PresentationTime { index, source })?;
            presentation_times.push(presentation_ns);
        }
    }

    // Phase 2: serialize. Each target owns its sync sequence and first-sync
    // extension state, so every target builds its own packet.
    let mut sync_packets = Vec::with_capacity(senders.len());
    for (index, sender) in senders.iter_mut().enumerate() {
        if !plan.sync_due {
            sync_packets.push(None);
            continue;
        }
        let presentation_ns = presentation_times[index];
        let packet = if plan.use_ptp_sync {
            sender.prepare_ptp_sync(
                plan.rtp_timestamp,
                presentation_ns,
                plan.next_rtp_timestamp,
                &plan.ptp_master_clock_id,
            )?
        } else {
            sender.prepare_sync(plan.rtp_timestamp, unix_to_ntp(presentation_ns))?
        };
        sync_packets.push(packet);
    }

    let mut wire_packets = Vec::with_capacity(senders.len());
    for sender in senders.iter_mut() {
        wire_packets.push(sender.prepare_audio(
            plan.payload_type,
            plan.rtp_timestamp,
            plan.payload,
            plan.marker,
        )?);
    }

    Ok(PreparedFrame {
        wire_packets,
        sync_packets,
    })
}

/// Sleep until an absolute deadline using the best available method.
///
/// On Linux with SCHED_FIFO, `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)`
/// provides ~10-50μs precision regardless of CONFIG_HZ. On other platforms,
/// falls back to spin_sleep.
#[cfg(target_os = "linux")]
fn precise_sleep_until(deadline_ns: u64) {
    use std::mem;

    let ts = libc::timespec {
        tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
    };

    unsafe {
        // TIMER_ABSTIME = 1, CLOCK_MONOTONIC = 1
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            1, // TIMER_ABSTIME
            &ts as *const libc::timespec,
            std::ptr::null_mut(),
        );
    }
}

/// Get current CLOCK_MONOTONIC time in nanoseconds.
#[cfg(target_os = "linux")]
fn monotonic_now_ns() -> u64 {
    use std::mem;
    unsafe {
        let mut ts: libc::timespec = mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts as *mut _);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

/// A send target for the dedicated sender thread.
///
/// Each target has its own data socket and destination, plus optional control
/// socket/dest for sync packets. In single-device mode there is one target;
/// in group mode there is one per device.
struct SendTarget {
    data_socket: UdpSocket,
    data_dest: SocketAddr,
    control_socket: Option<UdpSocket>,
    control_dest: Option<SocketAddr>,
    /// Per-target transport diagnostics counters (shared with the snapshot).
    counters: Arc<TargetTransportCounters>,
    /// Position of the [`RtpSender`] this target was built from.
    ///
    /// `prepare_frame_for_targets` produces one audio and one sync packet per
    /// *RTP sender*, while a sender without a data socket produces no target
    /// at all. Addressing the packet vectors by the target's own position
    /// would therefore hand every later target its predecessor's packets —
    /// including its presentation time, which is audible in the room but
    /// invisible in a test run.
    sender_index: usize,
}

impl SendTarget {
    /// Pick this target's entry out of a vector indexed by RTP sender.
    fn packet_for<'a, T>(&self, packets: &'a [T]) -> Option<&'a T> {
        packets.get(self.sender_index)
    }
}

/// Build one [`SendTarget`] per RTP sender that owns a data socket, plus the
/// per-sender transport counters in RTP-sender order.
///
/// Counters are registered for **every** sender, so the diagnostics registry
/// stays aligned with `rtp_senders`. Targets exist only for senders that can
/// actually transmit; each one records where it came from in `sender_index`.
fn build_send_targets(
    senders: &mut [RtpSender],
) -> (Vec<SendTarget>, Vec<Arc<TargetTransportCounters>>) {
    let mut targets = Vec::new();
    let mut counters_registry = Vec::with_capacity(senders.len());

    for (sender_index, sender) in senders.iter_mut().enumerate() {
        // Register per-target transport counters (RTP-sender order) and
        // share them with both send paths.
        let counters = Arc::new(TargetTransportCounters::default());
        sender.set_transport_counters(Arc::clone(&counters));
        counters_registry.push(Arc::clone(&counters));

        if let Ok(Some((data_socket, data_dest))) = sender.clone_data_socket() {
            let (control_socket, control_dest) = match sender.clone_control_socket() {
                Ok(Some((socket, dest))) => (Some(socket), Some(dest)),
                _ => (None, None),
            };
            targets.push(SendTarget {
                data_socket,
                data_dest,
                control_socket,
                control_dest,
                counters,
                sender_index,
            });
        }
    }

    (targets, counters_registry)
}

/// Dedicated OS thread for precise packet timing.
///
/// On Linux, uses `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` with
/// SCHED_FIFO for ~10-50μs precision. On other platforms, falls back to
/// `spin_sleep` hybrid kernel-sleep + spin-wait.
///
/// When `burst_size > 1`, packets are buffered and sent in bursts to reduce
/// WiFi/Bluetooth interference on shared-radio devices (e.g., Pi Zero 2 W).
/// For example, burst_size=4 sends 4 packets rapidly, then waits 4*frame_duration.
fn sender_thread_main(
    rx: Receiver<SenderMessage>,
    targets: Vec<SendTarget>,
    frame_duration: std::time::Duration,
    burst_size: usize,
    jitter: SchedulerJitterRecorder,
) {
    set_realtime_priority();
    disable_wifi_power_save();

    let burst_size = burst_size.max(1); // Minimum 1
    let frame_duration_ns = frame_duration.as_nanos() as u64;
    let burst_duration_ns = frame_duration_ns * burst_size as u64;
    #[cfg(target_os = "linux")]
    let mut next_deadline_ns = monotonic_now_ns();
    #[cfg(not(target_os = "linux"))]
    let sleeper = spin_sleep::SpinSleeper::default();
    #[cfg(not(target_os = "linux"))]
    let mut next_deadline = std::time::Instant::now();

    let mut last_send = std::time::Instant::now();
    let mut started = false;
    let mut packet_count: u64 = 0;
    let mut max_jitter_ms: f64 = 0.0;
    let mut jitter_sum_ms: f64 = 0.0;
    let mut jitter_exceed_count: u64 = 0;

    // Burst buffer for WiFi/BT coexistence
    // Each entry: (wire_packets per target, sync packets per target)
    #[allow(clippy::type_complexity)]
    let mut burst_buffer: Vec<(Vec<Vec<u8>>, Vec<Option<Vec<u8>>>)> =
        Vec::with_capacity(burst_size);

    let target_count = targets.len();
    tracing::info!(
        "Sender thread started: {} target(s), frame_duration={:.3}ms, burst_size={}, timing={}",
        target_count,
        frame_duration.as_secs_f64() * 1000.0,
        burst_size,
        if cfg!(target_os = "linux") {
            "clock_nanosleep(TIMER_ABSTIME)"
        } else {
            "spin_sleep"
        }
    );

    loop {
        // Receive next message (blocking with timeout for clean shutdown detection)
        let msg = match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(msg) => msg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                tracing::info!("Sender thread: channel disconnected, exiting");
                break;
            }
        };

        match msg {
            SenderMessage::Stop => {
                tracing::info!("Sender thread: received Stop, exiting");
                break;
            }
            SenderMessage::Pause => {
                tracing::debug!("Sender thread: paused");
                loop {
                    match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(SenderMessage::Resume) => {
                            tracing::debug!("Sender thread: resumed");
                            #[cfg(target_os = "linux")]
                            {
                                next_deadline_ns = monotonic_now_ns();
                            }
                            #[cfg(not(target_os = "linux"))]
                            {
                                next_deadline = std::time::Instant::now();
                            }
                            jitter.record_deadline_reset();
                            break;
                        }
                        Ok(SenderMessage::Stop) => {
                            tracing::info!("Sender thread: received Stop while paused, exiting");
                            return;
                        }
                        _ => continue,
                    }
                }
                continue;
            }
            SenderMessage::Resume => {
                #[cfg(target_os = "linux")]
                {
                    next_deadline_ns = monotonic_now_ns();
                }
                #[cfg(not(target_os = "linux"))]
                {
                    next_deadline = std::time::Instant::now();
                }
                continue;
            }
            SenderMessage::Packet {
                wire_packets,
                sync_packets,
            } => {
                // Buffer packet for burst sending
                burst_buffer.push((wire_packets, sync_packets));

                // Only send when we have a full burst (or first packet to initialize timing)
                if burst_buffer.len() < burst_size && started {
                    continue;
                }

                if !started {
                    #[cfg(target_os = "linux")]
                    {
                        next_deadline_ns = monotonic_now_ns();
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        next_deadline = std::time::Instant::now();
                    }
                    last_send = std::time::Instant::now();
                    started = true;
                } else {
                    // Wait until the precise deadline for this burst
                    #[cfg(target_os = "linux")]
                    {
                        precise_sleep_until(next_deadline_ns);
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let now = std::time::Instant::now();
                        if next_deadline > now {
                            sleeper.sleep(next_deadline - now);
                        }
                    }
                }

                // Measure actual interval and jitter (for burst timing)
                let send_time = std::time::Instant::now();
                let actual_interval = send_time.duration_since(last_send);
                let interval_ms = actual_interval.as_secs_f64() * 1000.0;
                let target_ms = frame_duration.as_secs_f64() * 1000.0 * burst_buffer.len() as f64;
                let jitter_ms = interval_ms - target_ms;
                // Exact signed nanoseconds vs. the nominal schedule, feeding
                // the diagnostics recorder (same measurement as jitter_ms).
                let target_ns = frame_duration_ns * burst_buffer.len() as u64;
                let jitter_ns = actual_interval.as_nanos() as i64 - target_ns as i64;

                // Track jitter stats (per burst)
                packet_count += burst_buffer.len() as u64;
                if packet_count > burst_size as u64 {
                    // Feed the bounded recorder alongside the legacy stats.
                    jitter.record_jitter(jitter_ns);

                    let abs_jitter = jitter_ms.abs();
                    if abs_jitter > max_jitter_ms {
                        max_jitter_ms = abs_jitter;
                    }
                    jitter_sum_ms += abs_jitter;
                    if abs_jitter > 1.0 * burst_size as f64 {
                        jitter_exceed_count += 1;
                    }

                    // Log burst with significant jitter
                    if abs_jitter > 2.0 * burst_size as f64 {
                        tracing::warn!(
                            "JITTER burst#{}: interval={:.3}ms target={:.3}ms jitter={:+.3}ms",
                            packet_count / burst_size as u64,
                            interval_ms,
                            target_ms,
                            jitter_ms
                        );
                    }

                    // Log stats every 500 packets (~4s)
                    if packet_count % 500 < burst_size as u64 {
                        let bursts = packet_count / burst_size as u64;
                        let avg_jitter = if bursts > 1 {
                            jitter_sum_ms / (bursts - 1) as f64
                        } else {
                            0.0
                        };
                        tracing::info!(
                            "TIMING STATS after {} pkts ({} bursts): avg_jitter={:.3}ms max_jitter={:.3}ms exceeds={}",
                            packet_count, bursts, avg_jitter, max_jitter_ms, jitter_exceed_count
                        );
                    }
                }

                last_send = send_time;

                // Send all buffered packets
                for (wire_packets, sync_packets) in burst_buffer.drain(..) {
                    // Each sync packet carries its own target's presentation
                    // time, so it goes to that target's control dest only.
                    // The packet vectors are indexed by RTP sender, so the
                    // target resolves them through its own `sender_index`.
                    for target in targets.iter() {
                        let Some(Some(sync)) = target.packet_for(&sync_packets) else {
                            continue;
                        };
                        let sock = target
                            .control_socket
                            .as_ref()
                            .unwrap_or(&target.data_socket);
                        let dest = target.control_dest.unwrap_or(target.data_dest);
                        target.counters.bump_sync_attempted();
                        let sent = sock.send_to(sync, dest);
                        if let Err(e) = &sent {
                            tracing::error!("Failed to send sync packet: {}", e);
                        }
                        target.counters.record_sync_outcome(&sent);
                    }

                    // Send per-target audio packets
                    for target in targets.iter() {
                        if let Some(wire_data) = target.packet_for(&wire_packets) {
                            target.counters.bump_data_attempted();
                            let sent = target.data_socket.send_to(wire_data, target.data_dest);
                            if let Err(e) = &sent {
                                tracing::error!("Failed to send audio packet: {}", e);
                            }
                            target.counters.record_data_outcome(&sent);
                        }
                    }
                }

                // Advance deadline by burst duration
                #[cfg(target_os = "linux")]
                {
                    next_deadline_ns += burst_duration_ns;
                    // Catch up if we've fallen behind by more than one burst
                    let now_ns = monotonic_now_ns();
                    if now_ns > next_deadline_ns + burst_duration_ns {
                        tracing::warn!(
                            "Sender thread fell behind by {:.2}ms, resetting deadline",
                            (now_ns - next_deadline_ns) as f64 / 1_000_000.0
                        );
                        next_deadline_ns = now_ns;
                        jitter.record_deadline_reset();
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    next_deadline += frame_duration * burst_size as u32;
                    let now = std::time::Instant::now();
                    if now > next_deadline + frame_duration * burst_size as u32 {
                        tracing::warn!(
                            "Sender thread fell behind by {:.2}ms, resetting deadline",
                            (now - next_deadline).as_secs_f64() * 1000.0
                        );
                        next_deadline = now;
                        jitter.record_deadline_reset();
                    }
                }
            }
        }
    }

    tracing::info!("Sender thread exiting");
}

struct StreamerInner {
    state: StreamerState,
    config: StreamConfig,
    buffer: AudioBuffer,
    rtp_senders: Vec<RtpSender>,
    current_timestamp: u64,
    last_sync_rtp: u32,
    clock: Clock,
    clock_offset: Option<ClockOffset>,
    timing_rx: Option<watch::Receiver<ClockOffset>>,
    ptp_clock_state: Option<PtpClockState>,
    ptp_clock_rx: Option<watch::Receiver<Option<PtpClockState>>>,
    decoder: Option<AudioDecoder>,
    /// Live audio decoder for streaming from external sources (e.g., Bluetooth).
    live_decoder: Option<LiveAudioDecoder>,
    encoder: Option<Box<dyn AudioEncoder>>,
    /// Audio equalizer for processing audio before encoding.
    equalizer: Option<Equalizer>,
    /// Track whether first audio packet has been sent (requires marker bit)
    first_packet_sent: bool,
    /// Render delay in nanoseconds added to NTP timestamps in sync packets.
    /// Tells the receiver to render audio this far in the future, giving more
    /// time for retransmit recovery of lost packets.
    render_delay_ns: u64,
    /// Whether to use PTP-mode sync packets (PT=87) instead of NTP (PT=84).
    use_ptp_sync: bool,
    /// PTP master clock identity (from BMCA, used in PT=87 packets).
    ptp_master_clock_id: [u8; 8],
    /// Normalized per-target presentation delay in nanoseconds, in the same
    /// order as `rtp_senders`. One entry per target; zero is a valid delay.
    ///
    /// Applied to the target's sync packet only — never to the shared RTP
    /// timestamp, payload or marker.
    target_presentation_delays_ns: Vec<u64>,
    /// Master-time anchor of the previous frame; the next frame's
    /// presentation times must lie strictly after it.
    last_master_anchor_ns: u64,
}

struct PreparedStreamFrame {
    prepared: PreparedFrame,
    sync_emitted: bool,
    master_anchor_ns: Option<u64>,
}

fn install_ptp_clock_state(inner: &mut StreamerInner, state: Option<PtpClockState>) {
    let previous_id = inner.ptp_clock_state.map(|current| current.master_clock_id);
    let next_id = state.map(|next| next.master_clock_id);
    let generation_changed = previous_id != next_id;

    inner.ptp_clock_state = state;
    match state {
        Some(state) => {
            inner.clock_offset = Some(state.offset);
            inner.ptp_master_clock_id = state.master_clock_id;
        }
        None => {
            inner.clock_offset = None;
            inner.ptp_master_clock_id = [0; 8];
        }
    }

    if generation_changed {
        inner.last_master_anchor_ns = 0;
        inner.last_sync_rtp = 0;
        for sender in &mut inner.rtp_senders {
            sender.reset_sync_state();
        }
    }
}

fn consume_ptp_clock_updates(inner: &mut StreamerInner) {
    let latest = inner
        .ptp_clock_rx
        .as_mut()
        .map(|rx| *rx.borrow_and_update());
    if let Some(latest) = latest {
        let changed = match (inner.ptp_clock_state, latest) {
            (Some(current), Some(next)) => {
                current.master_clock_id != next.master_clock_id
                    || current.offset.offset_ns != next.offset.offset_ns
                    || current.offset.error_ns != next.offset.error_ns
                    || current.offset.rtt_ns != next.offset.rtt_ns
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            install_ptp_clock_state(inner, latest);
        }
    }
}

fn prepare_stream_frame(
    inner: &mut StreamerInner,
    payload_type: u8,
    rtp_timestamp: u32,
    next_rtp_timestamp: u32,
    payload: &[u8],
    marker: bool,
    local_wall_ns: u64,
) -> std::result::Result<PreparedStreamFrame, PrepareFrameError> {
    consume_ptp_clock_updates(inner);

    let master_anchor_ns = master_time_for_anchor_ns(local_wall_ns, inner.clock_offset);
    let coherent_ptp_ready = inner.ptp_clock_rx.is_none() || inner.ptp_clock_state.is_some();
    let clock_ready = !inner.use_ptp_sync || coherent_ptp_ready;
    let sample_rate = inner.config.audio_format.sample_rate.as_hz();
    let sync_due = clock_ready
        && (marker
            || inner.last_sync_rtp == 0
            || rtp_timestamp.wrapping_sub(inner.last_sync_rtp) >= sample_rate);
    let mut plan = FramePlan {
        payload_type,
        rtp_timestamp,
        next_rtp_timestamp,
        payload,
        marker,
        sync_due,
        use_ptp_sync: inner.use_ptp_sync,
        ptp_master_clock_id: inner.ptp_master_clock_id,
        common_master_time_ns: master_anchor_ns,
        global_render_lead_ns: inner.render_delay_ns,
        reference_master_time_ns: inner.last_master_anchor_ns,
    };

    let prepared = match prepare_frame_for_targets(
        &mut inner.rtp_senders,
        &inner.target_presentation_delays_ns,
        &plan,
    ) {
        Ok(prepared) => prepared,
        Err(PrepareFrameError::PresentationTime { index, source }) => {
            tracing::warn!(
                "Skipping sync for this frame (target {}): {}",
                index,
                source
            );
            plan.sync_due = false;
            prepare_frame_for_targets(
                &mut inner.rtp_senders,
                &inner.target_presentation_delays_ns,
                &plan,
            )?
        }
        Err(other) => return Err(other),
    };

    Ok(PreparedStreamFrame {
        prepared,
        sync_emitted: plan.sync_due,
        master_anchor_ns: clock_ready.then_some(master_anchor_ns),
    })
}

/// High-level audio streamer.
pub struct AudioStreamer {
    inner: Arc<Mutex<StreamerInner>>,
    task: Option<JoinHandle<Result<()>>>,
    state_cache: Arc<AtomicU8>,
    timestamp_cache: Arc<AtomicU64>,
    /// Total audio packets sent (lock-free, updated by run_streamer).
    packets_sent: Arc<AtomicU64>,
    /// Buffer underruns: times the buffer was empty when a packet was due.
    underruns: Arc<AtomicU64>,
    /// Dedicated sender thread for precise packet timing.
    sender_thread: Option<std::thread::JoinHandle<()>>,
    /// Channel to send packets to the sender thread.
    sender_tx: Option<Sender<SenderMessage>>,
    /// Scheduler jitter recorder fed by the sender thread / stream loop.
    scheduler_jitter: SchedulerJitterRecorder,
    /// Per-target transport counters, registered in target-index order.
    target_counters: Arc<RwLock<Vec<Arc<TargetTransportCounters>>>>,
    /// Cumulative retransmit-handling counters (shared with the diagnostics
    /// source; bumped once per handled retransmit request).
    retransmit_counters: Arc<RetransmitCounters>,
    /// Buffer-backed diagnostics source captured at construction (the buffer
    /// lives behind an async mutex, so its counters are captured eagerly).
    buffer_diagnostics: AudioDiagnosticsSource,
}

impl Clone for AudioStreamer {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            task: None, // Can't clone JoinHandle, new clone doesn't own the task
            state_cache: Arc::clone(&self.state_cache),
            timestamp_cache: Arc::clone(&self.timestamp_cache),
            packets_sent: Arc::clone(&self.packets_sent),
            underruns: Arc::clone(&self.underruns),
            sender_thread: None, // Can't clone JoinHandle
            sender_tx: self.sender_tx.clone(),
            scheduler_jitter: self.scheduler_jitter.clone(),
            target_counters: Arc::clone(&self.target_counters),
            retransmit_counters: Arc::clone(&self.retransmit_counters),
            buffer_diagnostics: self.buffer_diagnostics.clone(),
        }
    }
}

/// Cancels the streaming task when the owning streamer goes away.
///
/// The task is spawned, so simply dropping its [`JoinHandle`] would detach
/// it: it would keep holding the live decoder and keep pulling PCM frames out
/// of the capture queue that the NEXT session generation also drains. Every
/// path that abandons a streamer without awaiting [`AudioStreamer::stop`] —
/// a teardown that ran out of its time budget, an aborted in-flight start —
/// therefore relies on this.
///
/// Only the owning instance cancels: [`Clone`] hands out handle-less views
/// (`task: None`), so dropping a clone — the control listener holds one —
/// never touches the running stream. Cancellation is asynchronous by design;
/// [`Drop`] must not block.
impl Drop for AudioStreamer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            self.state_cache
                .store(StreamerState::Stopped as u8, Ordering::Relaxed);
            task.abort();
        }
    }
}

impl AudioStreamer {
    /// Create new streamer.
    pub fn new(config: StreamConfig) -> Self {
        let audio_format = config.audio_format.clone();
        let buffer = AudioBuffer::new(audio_format, config.sender_buffer.millis());
        let buffer_diagnostics = buffer.diagnostics_source(audio_format.sample_rate.as_hz());
        Self {
            inner: Arc::new(Mutex::new(StreamerInner {
                state: StreamerState::Idle,
                config,
                buffer,
                rtp_senders: Vec::new(),
                current_timestamp: 0,
                last_sync_rtp: 0,
                clock: Clock::new(audio_format.sample_rate.as_hz()),
                clock_offset: None,
                timing_rx: None,
                ptp_clock_state: None,
                ptp_clock_rx: None,
                decoder: None,
                live_decoder: None,
                encoder: None,
                equalizer: None,
                first_packet_sent: false,
                render_delay_ns: 0,
                use_ptp_sync: false,
                ptp_master_clock_id: [0u8; 8],
                target_presentation_delays_ns: Vec::new(),
                last_master_anchor_ns: 0,
            })),
            task: None,
            state_cache: Arc::new(AtomicU8::new(StreamerState::Idle as u8)),
            timestamp_cache: Arc::new(AtomicU64::new(0)),
            packets_sent: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            sender_thread: None,
            sender_tx: None,
            scheduler_jitter: SchedulerJitterRecorder::new(),
            target_counters: Arc::new(RwLock::new(Vec::new())),
            retransmit_counters: Arc::new(RetransmitCounters::default()),
            buffer_diagnostics,
        }
    }

    /// Configure RTP sender (single device, backward compatible).
    ///
    /// Resets any configured presentation delays, because they are positional.
    pub async fn set_rtp_sender(&mut self, sender: RtpSender) {
        let mut inner = self.inner.lock().await;
        inner.rtp_senders = vec![sender];
        inner.target_presentation_delays_ns = vec![0];
    }

    /// Configure multiple RTP senders for group streaming.
    ///
    /// Each sender targets a different device with its own encryption key.
    /// All senders share the same RTP sequence/timestamp space.
    ///
    /// Presentation delays are positional, so this resets them to zero for
    /// every target; call [`Self::set_target_presentation_delays_ns`]
    /// afterwards.
    pub async fn set_rtp_senders(&mut self, senders: Vec<RtpSender>) {
        let mut inner = self.inner.lock().await;
        inner.target_presentation_delays_ns = vec![0; senders.len()];
        inner.rtp_senders = senders;
    }

    /// Set the normalized per-target presentation delays, in sender order.
    ///
    /// One non-negative delay per configured RTP sender; zero is a valid
    /// request, a missing entry is not. Delays are part of session setup and
    /// are refused once streaming has begun — this is a restart-only control,
    /// not a live retiming slider.
    ///
    /// The delay is added to that target's sync-packet presentation time only.
    /// The shared RTP timestamps, payload and marker are untouched, so group
    /// synchronization of the audio stream itself cannot be affected.
    ///
    /// Deviation from the plan's sketch: this is `async` like every other
    /// streamer setter, because validating against the configured sender
    /// count requires the same lock.
    pub async fn set_target_presentation_delays_ns(&mut self, delays: Vec<u64>) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.state != StreamerState::Idle {
            return Err(airplay_core::error::Error::Streaming(
                airplay_core::error::StreamingError::Encoding(
                    "presentation delays can only be set before streaming starts".into(),
                ),
            ));
        }
        if delays.len() != inner.rtp_senders.len() {
            return Err(PrepareFrameError::TargetCountMismatch {
                targets: inner.rtp_senders.len(),
                delays: delays.len(),
            }
            .into());
        }
        inner.target_presentation_delays_ns = delays;
        Ok(())
    }

    /// Enable PTP-mode sync packets (PT=87) instead of NTP sync (PT=84).
    ///
    /// When enabled, sync packets include PTP clock timestamps and the master
    /// clock identity instead of NTP epoch timestamps.
    pub async fn set_ptp_sync_mode(&mut self, clock_id: [u8; 8]) {
        let mut inner = self.inner.lock().await;
        inner.use_ptp_sync = true;
        if inner.ptp_clock_rx.is_none() {
            inner.ptp_master_clock_id = clock_id;
        }
        tracing::info!("PTP sync mode enabled");
    }

    /// Subscribe to coherent PTP clock generations.
    ///
    /// The receiver's current value is consumed immediately so a late
    /// subscriber does not wait for another network packet before it can emit
    /// a valid PT=87 sync. `None` keeps audio flowing but suppresses PT=87.
    pub async fn set_ptp_clock_updates(&mut self, rx: watch::Receiver<Option<PtpClockState>>) {
        let mut inner = self.inner.lock().await;
        inner.use_ptp_sync = true;
        inner.timing_rx = None;
        inner.ptp_clock_rx = Some(rx);
        inner.ptp_clock_state = None;
        inner.clock_offset = None;
        inner.ptp_master_clock_id = [0; 8];
        inner.last_master_anchor_ns = 0;
        inner.last_sync_rtp = 0;
        for sender in &mut inner.rtp_senders {
            sender.reset_sync_state();
        }
        consume_ptp_clock_updates(&mut inner);
        tracing::info!("Coherent PTP clock updates enabled");
    }

    /// Set render delay in milliseconds.
    ///
    /// This shifts NTP timestamps in sync packets into the future, telling
    /// the receiver to buffer audio longer before rendering. This gives more
    /// time for retransmit recovery of lost packets.
    pub async fn set_render_delay_ms(&mut self, delay_ms: u32) {
        let delay_ns = delay_ms as u64 * 1_000_000;
        self.inner.lock().await.render_delay_ns = delay_ns;
        tracing::info!("Render delay set to {}ms ({}ns)", delay_ms, delay_ns);
    }

    /// Set up the equalizer with shared parameters.
    ///
    /// The EQ will be initialized with the given configuration and parameters.
    /// The parameters can be updated atomically from another thread (e.g., the UI).
    pub async fn set_eq_params(&mut self, config: EqConfig, params: Arc<EqParams>) {
        let mut inner = self.inner.lock().await;
        let sample_rate = inner.config.audio_format.sample_rate.as_hz();
        inner.equalizer = Some(Equalizer::new(config, params, sample_rate));
        tracing::info!(
            "Equalizer enabled with {} bands",
            inner.equalizer.as_ref().unwrap().config().num_bands()
        );
    }

    /// Get a clone of the EQ params if the equalizer is set up.
    pub async fn eq_params(&self) -> Option<Arc<EqParams>> {
        let inner = self.inner.lock().await;
        inner.equalizer.as_ref().map(|eq| Arc::clone(eq.params()))
    }

    /// Handle retransmit request from control channel (delegates to first sender).
    pub async fn handle_retransmit(
        &self,
        request: &crate::rtp::RetransmitRequest,
    ) -> airplay_core::error::Result<RetransmitOutcome> {
        self.handle_retransmit_for_target(0, request).await
    }

    /// Handle retransmit request for a specific target device.
    ///
    /// Each RtpSender has its own packet history ring buffer, so retransmit
    /// responses are per-device (encrypted with the correct device key). Every
    /// requested slot is classified truthfully in the returned outcome and
    /// folded into the cumulative counters exposed via
    /// [`Self::diagnostics_source_with_transport`]. A send error on one slot
    /// never aborts the remaining slots.
    pub async fn handle_retransmit_for_target(
        &self,
        index: usize,
        request: &crate::rtp::RetransmitRequest,
    ) -> airplay_core::error::Result<RetransmitOutcome> {
        let inner = self.inner.lock().await;
        let outcome = match inner.rtp_senders.get(index) {
            Some(sender) => sender.handle_retransmit(request),
            // Unknown target: the slots were requested but nothing could
            // serve them; they count as requested only.
            None => RetransmitOutcome {
                requested_slots: request.count,
                ..RetransmitOutcome::default()
            },
        };
        drop(inner);
        self.retransmit_counters.record_request(&outcome);
        Ok(outcome)
    }

    /// Get the total number of audio packets sent.
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent.load(Ordering::Relaxed)
    }

    /// Build a diagnostics source carrying streamer-side telemetry.
    ///
    /// The snapshot's `scheduler`, `rtp_frames_prepared_total`, `targets` and
    /// `retransmit` fields are filled from live streamer state; the
    /// sender-buffer half is live too (this streamer's own buffer, baked with
    /// its configured sample rate). `sample_rate_hz` is currently unused
    /// because of that baked-in rate and reserved for future recomputation of
    /// the buffer half.
    ///
    /// Note: `rtp_frames_prepared_total` mirrors [`Self::packets_sent`] — a
    /// frame counts once it was encoded and handed to the transport path.
    pub fn diagnostics_source_with_transport(
        &self,
        _sample_rate_hz: u32,
    ) -> AudioDiagnosticsSource {
        AudioDiagnosticsSource::with_retransmit_counters(
            AudioDiagnosticsSource::with_stream_telemetry(
                self.buffer_diagnostics.clone(),
                self.scheduler_jitter.clone(),
                Arc::clone(&self.packets_sent),
                Arc::clone(&self.target_counters),
            ),
            Arc::clone(&self.retransmit_counters),
        )
    }

    /// Get the count of buffer underruns (buffer empty when a packet was due).
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Get current state.
    pub fn state(&self) -> StreamerState {
        match self.state_cache.load(Ordering::Relaxed) {
            0 => StreamerState::Idle,
            1 => StreamerState::Buffering,
            2 => StreamerState::Streaming,
            3 => StreamerState::Paused,
            4 => StreamerState::Stopped,
            _ => StreamerState::Error,
        }
    }

    /// Start streaming from decoder.
    pub async fn start(&mut self, decoder: AudioDecoder) -> Result<()> {
        let frame_duration_ns;
        {
            let mut inner = self.inner.lock().await;
            inner.decoder = Some(decoder);
            inner.encoder = Some(create_encoder(inner.config.audio_format.clone())?);
            inner.state = StreamerState::Buffering;
            frame_duration_ns = inner.config.audio_format.frames_per_packet as u64
                * 1_000_000_000u64
                / inner.config.audio_format.sample_rate.as_hz() as u64;
        }
        self.state_cache
            .store(StreamerState::Buffering as u8, Ordering::Relaxed);

        // Prime the buffer
        self.decode_some().await?;
        let level = self.buffer_level().await;
        if level > 10.0 {
            self.inner.lock().await.state = StreamerState::Streaming;
            self.state_cache
                .store(StreamerState::Streaming as u8, Ordering::Relaxed);
        }

        if self.task.is_none() {
            // Set up the dedicated sender thread with cloned sockets
            let (tx, rx) = bounded::<SenderMessage>(8);
            let frame_duration = std::time::Duration::from_nanos(frame_duration_ns);
            let jitter_recorder = self.scheduler_jitter.clone();

            {
                let mut inner = self.inner.lock().await;
                let (targets, counters_registry) = build_send_targets(&mut inner.rtp_senders);
                let sender_count = inner.rtp_senders.len();
                if let Ok(mut registry) = self.target_counters.write() {
                    *registry = counters_registry;
                }

                // A sender without a socket simply sends nothing; every
                // remaining target still resolves its own packets through
                // `sender_index`, so nothing is shifted onto a neighbour.
                if targets.len() != sender_count {
                    tracing::warn!(
                        "{} of {} RTP sender(s) have no socket and will not be served",
                        sender_count - targets.len(),
                        sender_count
                    );
                }

                if !targets.is_empty() {
                    // Burst size for WiFi/BT coexistence: send N packets rapidly, then wait.
                    // Creates gaps for Bluetooth to transmit cleanly.
                    // - burst_size=1: no bursting (original behavior)
                    // - burst_size=2: 16ms gaps
                    // - burst_size=4: 32ms gaps (may cause burst packet loss)
                    let burst_size = 1;

                    let thread = std::thread::Builder::new()
                        .name("rt-sender".into())
                        .spawn(move || {
                            sender_thread_main(
                                rx,
                                targets,
                                frame_duration,
                                burst_size,
                                jitter_recorder,
                            );
                        })
                        .expect("Failed to spawn sender thread");
                    self.sender_thread = Some(thread);
                    self.sender_tx = Some(tx);
                } else {
                    tracing::warn!("No RTP senders have sockets, falling back to async timing");
                }
            }

            let inner = self.inner.clone();
            let state_cache = self.state_cache.clone();
            let timestamp_cache = self.timestamp_cache.clone();
            let packets_sent = self.packets_sent.clone();
            let underruns = self.underruns.clone();
            let sender_tx = self.sender_tx.clone();
            let jitter_recorder = self.scheduler_jitter.clone();
            self.task = Some(tokio::spawn(async move {
                match run_streamer(
                    inner.clone(),
                    state_cache.clone(),
                    timestamp_cache,
                    packets_sent,
                    underruns,
                    sender_tx,
                    jitter_recorder,
                )
                .await
                {
                    Ok(()) => tracing::debug!("Streaming task completed normally"),
                    Err(e) => {
                        tracing::error!("Streaming task error: {}", e);
                        state_cache.store(StreamerState::Error as u8, Ordering::Relaxed);
                        if let Ok(mut guard) = inner.try_lock() {
                            guard.state = StreamerState::Error;
                        }
                    }
                }
                Ok(())
            }));
        }

        Ok(())
    }

    /// Start streaming from live audio source (e.g., Bluetooth capture).
    ///
    /// This is similar to `start()` but uses a `LiveAudioDecoder` that receives
    /// PCM frames from a channel, enabling streaming from external sources like
    /// Bluetooth audio capture.
    pub async fn start_live(&mut self, live_decoder: LiveAudioDecoder) -> Result<()> {
        self.start_live_gated(live_decoder, None).await
    }

    /// Prepares live audio, holding all dispatch until `release` succeeds.
    /// Dropping the release sender cancels dispatch; buffering never sends RTP.
    pub async fn start_live_gated(
        &mut self,
        live_decoder: LiveAudioDecoder,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Result<()> {
        let frame_duration_ns;
        {
            let mut inner = self.inner.lock().await;
            inner.live_decoder = Some(live_decoder);
            inner.decoder = None; // Clear file decoder if any
            inner.encoder = Some(create_encoder(inner.config.audio_format.clone())?);
            inner.state = StreamerState::Buffering;
            frame_duration_ns = inner.config.audio_format.frames_per_packet as u64
                * 1_000_000_000u64
                / inner.config.audio_format.sample_rate.as_hz() as u64;
        }
        self.state_cache
            .store(StreamerState::Buffering as u8, Ordering::Relaxed);

        // For live streaming, wait for initial buffer fill before streaming.
        // This prevents startup artifacts from sending packets before we have
        // enough audio data buffered. Target ~500ms of buffer (about 60 packets
        // at 352 frames/packet, 44.1kHz).
        tracing::info!("Live streaming: waiting for initial buffer fill...");
        let buffer_start = std::time::Instant::now();
        let max_wait = std::time::Duration::from_secs(5);
        let target_fill_pct = 50.0; // Wait for 50% of 2000ms buffer = 1000ms

        loop {
            // Try to decode some frames into the buffer
            {
                let mut guard = self.inner.lock().await;
                decode_some_inner(&mut guard)?;
                let fill_pct = guard.buffer.fill_percentage();
                let frame_count = guard.buffer.len();

                if fill_pct >= target_fill_pct {
                    tracing::info!(
                        "Live streaming: buffer ready at {:.1}% ({} frames), starting playback",
                        fill_pct,
                        frame_count
                    );
                    break;
                }

                if buffer_start.elapsed() > max_wait {
                    tracing::warn!(
                        "Live streaming: buffer timeout at {:.1}% ({} frames), starting anyway",
                        fill_pct,
                        frame_count
                    );
                    break;
                }

                if buffer_start.elapsed().as_millis() % 500 == 0 {
                    tracing::debug!(
                        "Live streaming: buffering {:.1}% ({} frames)...",
                        fill_pct,
                        frame_count
                    );
                }
            }
            // Small delay before retry
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        if release.is_none() {
            // Ungated callers retain the original immediate state contract.
            self.inner.lock().await.state = StreamerState::Streaming;
            self.state_cache
                .store(StreamerState::Streaming as u8, Ordering::Relaxed);
        }

        if self.task.is_none() {
            // Set up the dedicated sender thread with cloned sockets
            let (tx, rx) = bounded::<SenderMessage>(8);
            let frame_duration = std::time::Duration::from_nanos(frame_duration_ns);
            let jitter_recorder = self.scheduler_jitter.clone();

            {
                let mut inner = self.inner.lock().await;
                let (targets, counters_registry) = build_send_targets(&mut inner.rtp_senders);
                let sender_count = inner.rtp_senders.len();
                if let Ok(mut registry) = self.target_counters.write() {
                    *registry = counters_registry;
                }

                // A sender without a socket simply sends nothing; every
                // remaining target still resolves its own packets through
                // `sender_index`, so nothing is shifted onto a neighbour.
                if targets.len() != sender_count {
                    tracing::warn!(
                        "{} of {} RTP sender(s) have no socket and will not be served",
                        sender_count - targets.len(),
                        sender_count
                    );
                }

                if !targets.is_empty() {
                    let burst_size = 1;

                    let thread = std::thread::Builder::new()
                        .name("rt-sender".into())
                        .spawn(move || {
                            sender_thread_main(
                                rx,
                                targets,
                                frame_duration,
                                burst_size,
                                jitter_recorder,
                            );
                        })
                        .expect("Failed to spawn sender thread");
                    self.sender_thread = Some(thread);
                    self.sender_tx = Some(tx);
                } else {
                    tracing::warn!("No RTP senders have sockets, falling back to async timing");
                }
            }

            let inner = self.inner.clone();
            let state_cache = self.state_cache.clone();
            let timestamp_cache = self.timestamp_cache.clone();
            let packets_sent = self.packets_sent.clone();
            let underruns = self.underruns.clone();
            let sender_tx = self.sender_tx.clone();
            let jitter_recorder = self.scheduler_jitter.clone();
            self.task = Some(tokio::spawn(async move {
                if let Some(release) = release {
                    if release.await.is_err() {
                        return Ok(());
                    }
                }
                inner.lock().await.state = StreamerState::Streaming;
                state_cache.store(StreamerState::Streaming as u8, Ordering::Relaxed);
                match run_streamer(
                    inner.clone(),
                    state_cache.clone(),
                    timestamp_cache,
                    packets_sent,
                    underruns,
                    sender_tx,
                    jitter_recorder,
                )
                .await
                {
                    Ok(()) => tracing::debug!("Live streaming task completed normally"),
                    Err(e) => {
                        tracing::error!("Live streaming task error: {}", e);
                        state_cache.store(StreamerState::Error as u8, Ordering::Relaxed);
                        if let Ok(mut guard) = inner.try_lock() {
                            guard.state = StreamerState::Error;
                        }
                    }
                }
                Ok(())
            }));
        }

        Ok(())
    }

    /// Pause streaming.
    pub async fn pause(&mut self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.state == StreamerState::Streaming {
            if let Some(ref tx) = self.sender_tx {
                let _ = tx.try_send(SenderMessage::Pause);
            }
            inner.state = StreamerState::Paused;
            self.state_cache
                .store(StreamerState::Paused as u8, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Resume streaming.
    pub async fn resume(&mut self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.state == StreamerState::Paused {
            if let Some(ref tx) = self.sender_tx {
                let _ = tx.try_send(SenderMessage::Resume);
            }
            inner.state = StreamerState::Streaming;
            self.state_cache
                .store(StreamerState::Streaming as u8, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Reset state after FLUSH so the next audio/sync packets have correct
    /// marker and extension bits set, as required by the AirPlay spec.
    pub async fn reset_after_flush(&mut self) {
        let mut inner = self.inner.lock().await;
        inner.first_packet_sent = false;
        for sender in &mut inner.rtp_senders {
            sender.reset_sync_state();
        }
    }

    /// Stop streaming.
    pub async fn stop(&mut self) -> Result<()> {
        // Set state to Stopped FIRST so run_streamer breaks out of its loop
        // and stops producing into the bounded channel.
        self.state_cache
            .store(StreamerState::Stopped as u8, Ordering::Relaxed);

        // Signal sender thread to stop
        if let Some(ref tx) = self.sender_tx {
            let _ = tx.try_send(SenderMessage::Stop);
        }
        // Drop sender_tx to disconnect the channel — guarantees the sender
        // thread's rx.recv() returns Err and exits, even if the Stop message
        // couldn't be delivered (channel was full).
        self.sender_tx = None;

        // Now safe to join — sender thread will exit from channel disconnect.
        // Bounded: a `Clone` of this streamer may still hold a sender handle,
        // so the disconnect is not guaranteed to be observed. Callers run
        // this inside a teardown budget, so an unbounded join here would wedge
        // the whole teardown; give up the join instead and let the thread
        // finish on its own.
        if let Some(handle) = self.sender_thread.take() {
            let join = tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            });
            if tokio::time::timeout(SENDER_THREAD_JOIN_BUDGET, join)
                .await
                .is_err()
            {
                tracing::warn!(
                    "sender thread did not exit within {:?}; abandoning the join",
                    SENDER_THREAD_JOIN_BUDGET
                );
            }
        }

        // Cancel the run_streamer task if still running
        if let Some(task) = self.task.take() {
            task.abort();
        }

        let mut inner = self.inner.lock().await;
        inner.state = StreamerState::Stopped;
        inner.buffer.flush();
        inner.decoder = None;
        inner.live_decoder = None;
        Ok(())
    }

    /// Seek to position in samples.
    pub async fn seek(&mut self, position_samples: u64) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.buffer.flush();
        if let Some(ref mut decoder) = inner.decoder {
            decoder.seek(position_samples)?;
        }
        // Reset EQ filter state to avoid artifacts from previous audio
        if let Some(ref mut eq) = inner.equalizer {
            eq.reset();
        }
        inner.current_timestamp = position_samples;
        self.timestamp_cache
            .store(position_samples, Ordering::Relaxed);
        Ok(())
    }

    /// Get current playback position in samples.
    pub fn position(&self) -> u64 {
        self.timestamp_cache.load(Ordering::Relaxed)
    }

    /// Get buffer fill level percentage.
    pub async fn buffer_level(&self) -> f32 {
        self.inner.lock().await.buffer.fill_percentage()
    }

    /// Set volume (0.0 to 1.0).
    pub async fn set_volume(&mut self, _volume: f32) -> Result<()> {
        // Volume is set via RTSP SET_PARAMETER, not in audio stream
        // This is a no-op placeholder
        Ok(())
    }

    /// Set timing offset from sync protocol.
    pub async fn set_timing_offset(&mut self, offset: ClockOffset) {
        let mut inner = self.inner.lock().await;
        if inner.ptp_clock_rx.is_none() {
            inner.clock_offset = Some(offset);
        }
    }

    /// Subscribe to timing updates.
    pub async fn set_timing_updates(&mut self, rx: watch::Receiver<ClockOffset>) {
        let mut inner = self.inner.lock().await;
        if inner.ptp_clock_rx.is_none() {
            inner.timing_rx = Some(rx);
        }
    }

    /// Internal: decode some audio into buffer.
    async fn decode_some(&mut self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        decode_some_inner(&mut inner)?;
        Ok(())
    }
}

fn decode_some_inner(inner: &mut StreamerInner) -> Result<()> {
    let format = inner.config.audio_format.clone();
    let frames_per_packet = format.frames_per_packet as usize;
    let is_live = inner.live_decoder.is_some();

    // Decode 3 frames per batch to minimize blocking in send loop.
    // Very small batches ensure minimal interference with precise timing.
    // With 2ms timeout per frame, worst case is ~6ms blocking.
    for _ in 0..3 {
        // Try live decoder first (for Bluetooth/external sources), then file decoder
        let frame = if let Some(ref mut live_decoder) = inner.live_decoder {
            live_decoder.decode_resampled(&format, frames_per_packet)?
        } else if let Some(ref mut decoder) = inner.decoder {
            decoder.decode_resampled(&format, frames_per_packet)?
        } else {
            break;
        };

        if let Some(frame) = frame {
            let audio_frame = crate::AudioFrame::new(frame.samples, frame.timestamp);
            inner
                .buffer
                .push(audio_frame)
                .map_err(|_| airplay_core::error::StreamingError::BufferOverflow)?;
        } else if is_live {
            // For live streams, None means timeout (no data yet), not EOF.
            // Don't break - just return and try again later.
            // This prevents the streamer from stopping on temporary data gaps.
            break;
        } else {
            // For file decoders, None means EOF
            break;
        }
    }
    Ok(())
}

async fn run_streamer(
    inner: Arc<Mutex<StreamerInner>>,
    state_cache: Arc<AtomicU8>,
    timestamp_cache: Arc<AtomicU64>,
    packets_sent_counter: Arc<AtomicU64>,
    underrun_counter: Arc<AtomicU64>,
    sender_tx: Option<Sender<SenderMessage>>,
    jitter: SchedulerJitterRecorder,
) -> Result<()> {
    // Compute frame duration once (constant for the session)
    let frame_duration_ns = {
        let guard = inner.lock().await;
        guard.config.audio_format.frames_per_packet as u64 * 1_000_000_000u64
            / guard.config.audio_format.sample_rate.as_hz() as u64
    };
    let frame_duration = Duration::from_nanos(frame_duration_ns);

    let has_sender_thread = sender_tx.is_some();

    // Only set RT priority if we're NOT using the dedicated sender thread
    // (the sender thread sets its own RT priority)
    if !has_sender_thread {
        set_realtime_priority();
    }

    // Use absolute deadline scheduling so processing time doesn't cause drift
    let mut next_deadline = Instant::now();

    loop {
        {
            let state = inner.lock().await.state;
            if state == StreamerState::Stopped || state == StreamerState::Error {
                break;
            }
            if state == StreamerState::Paused {
                sleep(Duration::from_millis(10)).await;
                // Reset deadline after pause so we don't burst-send
                next_deadline = Instant::now();
                jitter.record_deadline_reset();
                continue;
            }
        }

        {
            let mut guard = inner.lock().await;
            consume_ptp_clock_updates(&mut guard);
            if guard.ptp_clock_rx.is_none() {
                if let Some(rx) = guard.timing_rx.as_mut() {
                    if rx.has_changed().unwrap_or(false) {
                        let latest = *rx.borrow_and_update();
                        guard.clock_offset = Some(latest);
                    }
                }
            }
            // Keep buffer above 40% but don't decode too aggressively
            // to avoid blocking the send loop with decode operations.
            // With 50% initial fill, we have plenty of headroom.
            if guard.buffer.fill_percentage() < 40.0 {
                let decode_start = Instant::now();
                decode_some_inner(&mut guard)?;
                let decode_elapsed = decode_start.elapsed();
                if decode_elapsed.as_millis() > 10 {
                    tracing::warn!(
                        "Decode took {:.2}ms (blocking send loop!)",
                        decode_elapsed.as_secs_f64() * 1000.0
                    );
                }
            }

            // Check for recovery from Buffering state
            if guard.state == StreamerState::Buffering {
                if guard.buffer.fill_percentage() > 10.0 {
                    guard.state = StreamerState::Streaming;
                    state_cache.store(StreamerState::Streaming as u8, Ordering::Relaxed);
                } else {
                    // Check if decoder(s) are exhausted
                    let decoder_eof = guard.decoder.as_ref().map_or(true, |d| d.is_eof());
                    let live_decoder_eof = guard.live_decoder.as_ref().map_or(true, |d| d.is_eof());
                    let has_any_decoder = guard.decoder.is_some() || guard.live_decoder.is_some();

                    // Only stop if we have a decoder and it's exhausted with empty buffer
                    // For live decoders, we keep waiting unless explicitly marked EOF
                    if has_any_decoder && decoder_eof && live_decoder_eof && guard.buffer.is_empty()
                    {
                        // Decoder exhausted and buffer empty - playback complete
                        tracing::info!("Decoder EOF and buffer empty - stopping");
                        guard.state = StreamerState::Stopped;
                        state_cache.store(StreamerState::Stopped as u8, Ordering::Relaxed);
                        break;
                    } else {
                        // Still buffering, wait and retry
                        drop(guard);
                        sleep(Duration::from_millis(10)).await;
                        // Reset deadline after buffering stall
                        next_deadline = Instant::now();
                        jitter.record_deadline_reset();
                        continue;
                    }
                }
            }

            let frame = guard.buffer.pop();
            if let Some(frame) = frame {
                // Monotonic counter gating the occasional structured-safe
                // logs below (durations/counts only, never packet content).
                static DIAG_COUNT: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(0);
                let diag = DIAG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                let samples_to_encode: Vec<i16> = if let Some(ref mut eq) = guard.equalizer {
                    let mut samples = (*frame.samples).clone();
                    eq.process(&mut samples);
                    samples
                } else {
                    (*frame.samples).clone()
                };

                // Encode synchronously
                let encode_start = Instant::now();
                let encoder = guard.encoder.as_mut().ok_or_else(|| {
                    airplay_core::error::StreamingError::Encoding("Encoder missing".into())
                })?;
                let packet = encoder.encode(&samples_to_encode)?;
                let encode_elapsed = encode_start.elapsed();

                let payload_type = guard.config.stream_type as u8;
                let sample_rate = guard.config.audio_format.sample_rate.as_hz();

                // Get current time and convert it to master time.
                let local_wall = guard.clock.now_wall_ns();
                let adjusted = master_time_for_anchor_ns(local_wall, guard.clock_offset);
                if diag < 3 {
                    match guard.clock_offset {
                        Some(offset) => tracing::info!(
                            "CLOCK OFFSET: offset_local_minus_master_ns={}, local_wall={}, master={}",
                            offset.offset_ns,
                            local_wall,
                            adjusted
                        ),
                        None if guard.ptp_clock_rx.is_some() => tracing::debug!(
                            "CLOCK OFFSET: PTP clock transition in progress; suppressing PT=87"
                        ),
                        None => tracing::warn!(
                            "CLOCK OFFSET: None - using local time directly (may cause timing issues!)"
                        ),
                    }
                }

                // Set marker bit on first audio packet (required by some receivers)
                let first_packet = !guard.first_packet_sent;
                let marker = first_packet;
                if marker {
                    tracing::info!("Sending first audio packet with marker bit set");
                }

                let rtp_ts = packet.timestamp as u32;

                if !guard.rtp_senders.is_empty() {
                    // One shared encode, one shared RTP timestamp, one common
                    // dispatch deadline — only the sync packets differ, by
                    // exactly each target's normalized calibration delay.
                    let next_rtp_ts = rtp_ts.wrapping_add(sample_rate / 44100 * 352);
                    let frame = prepare_stream_frame(
                        &mut guard,
                        payload_type,
                        rtp_ts,
                        next_rtp_ts,
                        &packet.data,
                        marker,
                        local_wall,
                    )?;
                    let sync_emitted = frame.sync_emitted;
                    let master_anchor_ns = frame.master_anchor_ns;
                    let PreparedFrame {
                        wire_packets,
                        sync_packets,
                    } = frame.prepared;

                    if let Some(ref tx) = sender_tx {
                        if diag < 5 || diag % 500 == 0 {
                            tracing::info!(
                                "DIAG timing #{}: encode={:.2}ms, targets={} (sender thread handles send timing)",
                                diag,
                                encode_elapsed.as_secs_f64() * 1000.0,
                                wire_packets.len(),
                            );
                        }

                        // Update state BEFORE sending to channel
                        if sync_emitted {
                            guard.last_sync_rtp = rtp_ts;
                        }
                        if first_packet {
                            guard.first_packet_sent = true;
                        }
                        if let Some(master_anchor_ns) = master_anchor_ns {
                            guard.last_master_anchor_ns = master_anchor_ns;
                        }
                        guard.current_timestamp = packet.timestamp + packet.samples as u64;
                        timestamp_cache.store(guard.current_timestamp, Ordering::Relaxed);
                        packets_sent_counter.fetch_add(1, Ordering::Relaxed);

                        // Drop the mutex guard first so other async tasks can proceed
                        drop(guard);
                        let tx_clone = tx.clone();
                        let msg = SenderMessage::Packet {
                            wire_packets,
                            sync_packets,
                        };
                        let send_result =
                            tokio::task::spawn_blocking(move || tx_clone.send(msg)).await;
                        match send_result {
                            Ok(Ok(())) => {}
                            _ => {
                                tracing::error!("Sender thread disconnected");
                                state_cache.store(StreamerState::Error as u8, Ordering::Relaxed);
                                break;
                            }
                        }
                        continue;
                    } else {
                        // Fallback: direct send (no sender thread). Same
                        // per-target packets, sent inline instead of on the
                        // RT thread.
                        let send_start = Instant::now();
                        for (index, sender) in guard.rtp_senders.iter().enumerate() {
                            if let Some(Some(sync)) = sync_packets.get(index) {
                                sender.send_prepared_sync(sync)?;
                            }
                        }
                        for (index, sender) in guard.rtp_senders.iter().enumerate() {
                            if let Some(wire) = wire_packets.get(index) {
                                sender.send_prepared_audio(wire)?;
                            }
                        }
                        let send_elapsed = send_start.elapsed();

                        if diag < 5 || diag % 500 == 0 {
                            tracing::info!(
                                "DIAG timing #{}: encode={:.2}ms, send={:.2}ms, targets={}, total={:.2}ms",
                                diag,
                                encode_elapsed.as_secs_f64() * 1000.0,
                                send_elapsed.as_secs_f64() * 1000.0,
                                guard.rtp_senders.len(),
                                (encode_elapsed + send_elapsed).as_secs_f64() * 1000.0
                            );
                        }

                        if sync_emitted {
                            guard.last_sync_rtp = rtp_ts;
                        }
                        if first_packet {
                            guard.first_packet_sent = true;
                        }
                        if let Some(master_anchor_ns) = master_anchor_ns {
                            guard.last_master_anchor_ns = master_anchor_ns;
                        }
                        guard.current_timestamp = packet.timestamp + packet.samples as u64;
                        timestamp_cache.store(guard.current_timestamp, Ordering::Relaxed);
                        packets_sent_counter.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                let count = underrun_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if count <= 5 || count % 50 == 0 {
                    tracing::warn!("Buffer underrun #{} (buffer empty, packet skipped)", count);
                }
                guard.state = StreamerState::Buffering;
                state_cache.store(StreamerState::Buffering as u8, Ordering::Relaxed);
            }
        }

        if has_sender_thread {
            // With sender thread: no sleep needed here. The bounded channel
            // provides natural backpressure - when it's full, the blocking send
            // (via spawn_blocking) throttles us to match the sender thread's
            // consumption rate. This keeps the channel maximally filled so the
            // sender thread never starves.
            //
            // Yield to let other Tokio tasks run (NTP, control, etc.)
            tokio::task::yield_now().await;
        } else {
            // Without sender thread: use deadline-based timing (original behavior)
            next_deadline += frame_duration;
            let now = Instant::now();
            if next_deadline > now {
                tokio::time::sleep(next_deadline - now).await;
            }
        }
    }

    // Signal sender thread to stop when streaming loop exits
    if let Some(ref tx) = sender_tx {
        let _ = tx.try_send(SenderMessage::Stop);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::{SenderBufferCapacity, StreamConfig};

    fn test_config() -> StreamConfig {
        StreamConfig::default()
    }

    mod sender_buffer_capacity {
        use super::*;

        #[tokio::test]
        async fn configures_the_actual_local_buffer_without_changing_render_lead() {
            for (millis, capacity_frames) in [(500, 62), (2_000, 250), (4_000, 500)] {
                let mut config = test_config();
                config.sender_buffer = SenderBufferCapacity::from_millis(millis).unwrap();
                let streamer = AudioStreamer::new(config);
                let inner = streamer.inner.lock().await;

                assert_eq!(inner.buffer.capacity(), capacity_frames);
                assert_eq!(inner.render_delay_ns, 0);
            }
        }
    }

    mod state_machine {
        use super::*;

        #[test]
        fn starts_idle() {
            let streamer = AudioStreamer::new(test_config());
            assert_eq!(streamer.state(), StreamerState::Idle);
        }

        #[tokio::test]
        async fn start_transitions_to_buffering() {
            // Can't easily test without real audio file
            // Test that we start in Idle
            let streamer = AudioStreamer::new(test_config());
            assert_eq!(streamer.state(), StreamerState::Idle);
        }

        #[tokio::test]
        async fn buffering_transitions_to_streaming() {
            // Would need mock decoder for proper testing
            let streamer = AudioStreamer::new(test_config());
            assert_eq!(streamer.state(), StreamerState::Idle);
        }

        #[tokio::test]
        async fn pause_transitions_to_paused() {
            let mut streamer = AudioStreamer::new(test_config());
            {
                let mut inner = streamer.inner.lock().await;
                inner.state = StreamerState::Streaming;
            }
            streamer
                .state_cache
                .store(StreamerState::Streaming as u8, Ordering::Relaxed);
            streamer.pause().await.unwrap();
            assert_eq!(streamer.state(), StreamerState::Paused);
        }

        #[tokio::test]
        async fn resume_transitions_to_streaming() {
            let mut streamer = AudioStreamer::new(test_config());
            {
                let mut inner = streamer.inner.lock().await;
                inner.state = StreamerState::Paused;
            }
            streamer
                .state_cache
                .store(StreamerState::Paused as u8, Ordering::Relaxed);
            streamer.resume().await.unwrap();
            assert_eq!(streamer.state(), StreamerState::Streaming);
        }

        #[tokio::test]
        async fn stop_transitions_to_stopped() {
            let mut streamer = AudioStreamer::new(test_config());
            {
                let mut inner = streamer.inner.lock().await;
                inner.state = StreamerState::Streaming;
            }
            streamer
                .state_cache
                .store(StreamerState::Streaming as u8, Ordering::Relaxed);
            streamer.stop().await.unwrap();
            assert_eq!(streamer.state(), StreamerState::Stopped);
        }
    }

    mod streaming {
        use super::*;

        #[tokio::test]
        async fn decodes_and_buffers_audio() {
            // Would need mock decoder
            let streamer = AudioStreamer::new(test_config());
            assert_eq!(streamer.buffer_level().await, 0.0);
        }

        #[tokio::test]
        async fn sends_rtp_packets() {
            // Would need mock RTP sender
            let streamer = AudioStreamer::new(test_config());
            assert!(streamer.inner.lock().await.rtp_senders.is_empty());
        }

        #[tokio::test]
        async fn respects_buffer_level() {
            let streamer = AudioStreamer::new(test_config());
            let level = streamer.buffer_level().await;
            assert!(level >= 0.0 && level <= 100.0);
        }

        // End-of-stream handling requires a mock decoder to trigger EOF.
    }

    mod lifecycle {
        use super::*;
        use crate::live_decoder::{LiveAudioDecoder, LivePcmFrame, LiveSendOutcome};

        fn pcm_frame() -> LivePcmFrame {
            LivePcmFrame {
                samples: vec![0i16; 2048],
                channels: 2,
                sample_rate: 44_100,
            }
        }

        #[tokio::test]
        async fn first_audio_dispatch_waits_for_release_and_cancel_never_dispatches() {
            for cancel in [false, true] {
                for target_count in [1, 2] {
                    let (mut sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 128);
                    for _ in 0..128 {
                        sender.try_send(pcm_frame());
                    }
                    let mut streamer = AudioStreamer::new(test_config());
                    let mut receivers = Vec::new();
                    let mut targets = Vec::new();
                    for index in 0..target_count {
                        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
                        receiver.set_nonblocking(true).unwrap();
                        let mut target = RtpSender::new(receiver.local_addr().unwrap(), index);
                        target.bind(0).unwrap();
                        targets.push(target);
                        receivers.push(receiver);
                    }
                    streamer.set_rtp_senders(targets).await;
                    let (release, ready) = tokio::sync::oneshot::channel();
                    streamer
                        .start_live_gated(decoder, Some(ready))
                        .await
                        .unwrap();
                    // The production dispatch loop must remain parked after the
                    // real buffer preparation, including across scheduler polls.
                    sleep(Duration::from_millis(40)).await;
                    assert_eq!(
                        streamer.packets_sent(),
                        0,
                        "prepared audio escaped the barrier"
                    );
                    let mut packet = [0u8; 4096];
                    for receiver in &receivers {
                        assert_eq!(
                            receiver.recv(&mut packet).unwrap_err().kind(),
                            std::io::ErrorKind::WouldBlock
                        );
                    }
                    assert_eq!(streamer.state(), StreamerState::Buffering);
                    if cancel {
                        drop(release);
                        sleep(Duration::from_millis(40)).await;
                        assert_eq!(streamer.packets_sent(), 0, "failed barrier released audio");
                        for receiver in &receivers {
                            assert_eq!(
                                receiver.recv(&mut packet).unwrap_err().kind(),
                                std::io::ErrorKind::WouldBlock
                            );
                        }
                    } else {
                        release.send(()).unwrap();
                        for receiver in &receivers {
                            tokio::time::timeout(Duration::from_secs(1), async {
                                loop {
                                    if let Ok(bytes) = receiver.recv(&mut packet) {
                                        assert!(bytes >= 12);
                                        break;
                                    }
                                    sleep(Duration::from_millis(2)).await;
                                }
                            })
                            .await
                            .expect("first real RTP datagram arrives after release");
                        }
                    }
                    streamer.stop().await.unwrap();
                }
            }
        }

        #[tokio::test]
        async fn unguarded_live_start_preserves_immediate_streaming_state() {
            let (mut sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 128);
            for _ in 0..128 {
                sender.try_send(pcm_frame());
            }
            let mut streamer = AudioStreamer::new(test_config());
            streamer.start_live(decoder).await.unwrap();
            assert_eq!(streamer.state(), StreamerState::Streaming);
            streamer.stop().await.unwrap();
        }

        /// A dropped streamer must let go of the live capture queue.
        ///
        /// Its streaming task is spawned, not owned by the future that
        /// started it: without an explicit abort, dropping the streamer
        /// merely detaches the task and it keeps pulling PCM frames. Since
        /// consumers of one capture bridge now share a single queue, such a
        /// leftover task splits the stream with the next session generation
        /// instead of quietly idling.
        #[tokio::test]
        async fn dropping_a_live_streamer_releases_the_capture_queue() {
            let (mut sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 128);
            for _ in 0..128 {
                sender.try_send(pcm_frame());
            }

            let mut streamer = AudioStreamer::new(test_config());
            streamer.start_live(decoder).await.unwrap();

            drop(streamer);

            // Cancellation lands at the task's next await point, so poll for
            // the release instead of guessing a fixed delay.
            let mut released = false;
            for _ in 0..200 {
                if sender.try_send_latest(pcm_frame()) == LiveSendOutcome::Disconnected {
                    released = true;
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }

            assert!(
                released,
                "a dropped streamer must not keep a live consumer on the capture queue"
            );
        }
    }

    mod seeking {
        use super::*;

        #[tokio::test]
        async fn seek_clears_buffer() {
            let mut streamer = AudioStreamer::new(test_config());
            streamer.seek(1000).await.unwrap();
            assert_eq!(streamer.buffer_level().await, 0.0);
        }

        #[tokio::test]
        async fn seek_updates_position() {
            let mut streamer = AudioStreamer::new(test_config());
            streamer.seek(44100).await.unwrap();
            assert_eq!(streamer.position(), 44100);
        }

        #[tokio::test]
        async fn seek_while_paused() {
            let mut streamer = AudioStreamer::new(test_config());
            {
                let mut inner = streamer.inner.lock().await;
                inner.state = StreamerState::Paused;
            }
            streamer
                .state_cache
                .store(StreamerState::Paused as u8, Ordering::Relaxed);
            streamer.seek(22050).await.unwrap();
            assert_eq!(streamer.position(), 22050);
            assert_eq!(streamer.state(), StreamerState::Paused);
        }
    }

    mod timing {
        use super::*;

        #[tokio::test]
        async fn timestamps_increment_correctly() {
            let streamer = AudioStreamer::new(test_config());
            assert_eq!(streamer.position(), 0);
        }

        #[tokio::test]
        async fn position_tracks_playback() {
            let streamer = AudioStreamer::new(test_config());
            let pos = streamer.position();
            assert_eq!(pos, 0);
        }
    }

    mod coherent_ptp_clock {
        use super::*;
        use std::net::{IpAddr, Ipv4Addr};

        fn state(id: u8, offset_ns: i64) -> PtpClockState {
            PtpClockState {
                master_clock_id: [id; 8],
                offset: ClockOffset {
                    offset_ns,
                    error_ns: 0,
                    rtt_ns: 0,
                },
            }
        }

        fn sender(port: u16, ssrc: u32) -> RtpSender {
            RtpSender::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), ssrc)
        }

        fn packet_rtp_timestamp(packet: &[u8]) -> u32 {
            u32::from_be_bytes(packet[4..8].try_into().unwrap())
        }

        #[tokio::test]
        async fn transition_never_prepares_a_torn_pt87_and_backward_epoch_forces_every_target_sync()
        {
            let (tx, rx) = watch::channel(Some(state(0xAA, 1_000)));
            let mut streamer = AudioStreamer::new(test_config());
            streamer
                .set_rtp_senders(vec![sender(6101, 1), sender(6102, 2)])
                .await;
            streamer.set_ptp_clock_updates(rx).await;

            let mut inner = streamer.inner.lock().await;
            inner.render_delay_ns = 10_000;
            let a = prepare_stream_frame(&mut inner, 96, 44_100, 44_452, &[1, 2, 3], true, 20_000)
                .expect("A frame");
            let a_anchor = a.master_anchor_ns.expect("A clock is ready");
            assert_eq!(a_anchor, 19_000);
            for sync in &a.prepared.sync_packets {
                let sync = sync.as_ref().expect("A sync");
                assert_eq!(&sync[20..28], &[0xAA; 8]);
                assert_eq!(packet_rtp_timestamp(sync), 44_100);
            }
            assert_eq!(
                packet_rtp_timestamp(&a.prepared.wire_packets[0]),
                packet_rtp_timestamp(&a.prepared.wire_packets[1])
            );
            inner.last_master_anchor_ns = a_anchor;
            inner.last_sync_rtp = 44_100;

            tx.send_replace(None);
            let transitioning =
                prepare_stream_frame(&mut inner, 96, 44_452, 44_804, &[4, 5, 6], false, 21_000)
                    .expect("audio continues while clock transitions");
            assert!(transitioning
                .prepared
                .sync_packets
                .iter()
                .all(Option::is_none));
            assert_eq!(transitioning.master_anchor_ns, None);
            assert_eq!(inner.last_master_anchor_ns, 0);

            tx.send_replace(Some(state(0xBB, 8_000)));
            let b = prepare_stream_frame(&mut inner, 96, 44_804, 45_156, &[7, 8, 9], false, 21_000)
                .expect("backward B epoch is accepted after reset");
            let b_anchor = b.master_anchor_ns.expect("B clock is ready");
            assert!(b_anchor < a_anchor, "the new clock may move backward");
            for sync in &b.prepared.sync_packets {
                let sync = sync.as_ref().expect("B forces sync for every target");
                assert_eq!(sync[0], 0x90, "new generation restores first-sync flag");
                assert_eq!(&sync[20..28], &[0xBB; 8]);
                assert_eq!(packet_rtp_timestamp(sync), 44_804);
            }
            assert_eq!(
                packet_rtp_timestamp(&b.prepared.wire_packets[0]),
                packet_rtp_timestamp(&b.prepared.wire_packets[1])
            );
        }

        #[tokio::test]
        async fn legacy_timing_setters_do_not_overwrite_a_live_coherent_clock() {
            let (_tx, rx) = watch::channel(Some(state(0xCC, 321)));
            let mut streamer = AudioStreamer::new(test_config());
            streamer.set_ptp_clock_updates(rx).await;
            streamer
                .set_timing_offset(ClockOffset {
                    offset_ns: 999,
                    error_ns: 0,
                    rtt_ns: 0,
                })
                .await;

            let inner = streamer.inner.lock().await;
            assert_eq!(inner.ptp_clock_state.unwrap().master_clock_id, [0xCC; 8]);
            assert_eq!(inner.clock_offset.unwrap().offset_ns, 321);
        }
    }

    mod send_target_indexing {
        use super::*;
        use std::net::{IpAddr, Ipv4Addr};

        fn dest(port: u16) -> SocketAddr {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
        }

        /// Three senders, the middle one without a data socket. The two
        /// surviving targets must still name the position of the RTP sender
        /// they were built from — otherwise every later target inherits its
        /// predecessor's packets.
        #[test]
        fn a_sender_without_a_socket_does_not_shift_later_targets() {
            let mut senders = vec![
                RtpSender::new(dest(6001), 1),
                RtpSender::new(dest(6002), 2),
                RtpSender::new(dest(6003), 3),
            ];
            senders[0].bind(0).expect("bind first sender");
            // senders[1] deliberately keeps no data socket.
            senders[2].bind(0).expect("bind third sender");

            let (targets, counters) = build_send_targets(&mut senders);

            assert_eq!(targets.len(), 2, "only senders with a socket send");
            assert_eq!(counters.len(), 3, "counters stay in RTP-sender order");
            assert_eq!(targets[0].sender_index, 0);
            assert_eq!(
                targets[1].sender_index, 2,
                "the third sender must not be addressed as the second"
            );
        }

        /// The sync packet carrying a target's own presentation time — and
        /// the audio packet encrypted for it — must be the ones built from
        /// that target's RTP sender, not a neighbour's.
        #[test]
        fn every_target_reads_the_packet_of_its_own_sender() {
            let mut senders = vec![
                RtpSender::new(dest(6011), 1),
                RtpSender::new(dest(6012), 2),
                RtpSender::new(dest(6013), 3),
            ];
            senders[0].bind(0).expect("bind first sender");
            senders[2].bind(0).expect("bind third sender");

            let (targets, _counters) = build_send_targets(&mut senders);

            // One entry per RTP sender, as `prepare_frame_for_targets` builds
            // them: index 2 carries the calibrated presentation time.
            let sync_packets = vec![Some(vec![0u8]), Some(vec![1u8]), Some(vec![2u8])];
            let wire_packets = vec![vec![10u8], vec![11u8], vec![12u8]];

            assert_eq!(targets[0].packet_for(&sync_packets), Some(&Some(vec![0u8])));
            assert_eq!(targets[0].packet_for(&wire_packets), Some(&vec![10u8]));
            assert_eq!(
                targets[1].packet_for(&sync_packets),
                Some(&Some(vec![2u8])),
                "the calibrated sync packet must reach its own speaker"
            );
            assert_eq!(targets[1].packet_for(&wire_packets), Some(&vec![12u8]));
        }
    }

    mod master_clock_anchor {
        use super::*;

        fn offset(offset_ns: i64) -> ClockOffset {
            ClockOffset {
                offset_ns,
                error_ns: 0,
                rtt_ns: 0,
            }
        }

        /// `ClockOffset::offset_ns` is measured as `local - master`, so a
        /// local clock running 2 ms ahead of the master anchors 2 ms
        /// *earlier* on the master timeline.
        #[test]
        fn a_local_clock_ahead_of_the_master_anchors_earlier() {
            assert_eq!(
                master_time_for_anchor_ns(10_000_000_000, Some(offset(2_000_000))),
                9_998_000_000
            );
        }

        #[test]
        fn a_local_clock_behind_the_master_anchors_later() {
            assert_eq!(
                master_time_for_anchor_ns(10_000_000_000, Some(offset(-2_000_000))),
                10_002_000_000
            );
        }

        #[test]
        fn without_a_measured_offset_the_local_clock_is_the_anchor() {
            assert_eq!(
                master_time_for_anchor_ns(10_000_000_000, None),
                10_000_000_000
            );
        }

        /// A conversion that would leave the representable range must not
        /// wrap: the local time is used unchanged.
        #[test]
        fn an_unrepresentable_conversion_falls_back_to_local_time() {
            assert_eq!(master_time_for_anchor_ns(10, Some(offset(i64::MAX))), 10);
            assert_eq!(
                master_time_for_anchor_ns(u64::MAX, Some(offset(i64::MIN))),
                u64::MAX
            );
        }
    }
}
