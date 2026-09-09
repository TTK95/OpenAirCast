//! Explicit timing conventions as computational boundaries.
//!
//! All conversions in this module use the NTP/PTP standard math with one
//! explicit sign convention, treated as a hard rechnungsgrenze (computation
//! boundary): **offset = local minus master**.
//!
//! * `offset_local_minus_master_ns > 0` means the local clock is behind the
//!   master (master time is ahead).
//! * `offset_local_minus_master_ns < 0` means the local clock is ahead of the
//!   master.
//!
//! Converting a local timestamp to master time therefore subtracts the
//! offset. Every arithmetic step is checked; impossible or overflowing
//! inputs yield `None` instead of wrapping.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

/// One PTP-style timing observation with all derived quantities resolved.
///
/// The four wire timestamps are reduced to the signed clock offset and the
/// mean path delay at construction time so that later consumers cannot mix
/// up the sign convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingMeasurement {
    measurement_sequence: u64,
    observed_elapsed_ns: u64,
    offset_local_minus_master_ns: i64,
    mean_path_delay_ns: u64,
}

impl TimingMeasurement {
    /// Sequence number carried through unchanged from the observation.
    pub fn measurement_sequence(&self) -> u64 {
        self.measurement_sequence
    }

    /// Observed elapsed nanoseconds carried through unchanged (diagnostics payload).
    pub fn observed_elapsed_ns(&self) -> u64 {
        self.observed_elapsed_ns
    }

    /// Signed clock offset in nanoseconds, sign convention: local minus master.
    pub fn offset_local_minus_master_ns(&self) -> i64 {
        self.offset_local_minus_master_ns
    }

    /// Estimated mean path delay in nanoseconds.
    pub fn mean_path_delay_ns(&self) -> u64 {
        self.mean_path_delay_ns
    }

    /// Convert a local timestamp to master time using this measurement's offset.
    pub fn local_to_master(&self, local_ns: u64) -> Option<u64> {
        local_to_master_ns(local_ns, self.offset_local_minus_master_ns)
    }
}

/// Compute a timing measurement from PTP/NTP wire timestamps.
///
/// Standard NTP/PTP math with timestamps in nanoseconds:
///
/// ```text
/// t1 = master TX, t2 = local RX, t3 = local TX, t4 = master RX
/// offset = ((t2 - t1) + (t3 - t4)) / 2      (sign: local minus master)
/// delay  = ((t4 - t1) - (t3 - t2)) / 2
/// ```
///
/// Returns `None` when any intermediate subtraction underflows (e.g. `t4 <
/// t1`) or when the offset does not fit into an [`i64`].
pub fn ptp_measurement(
    measurement_sequence: u64,
    observed_elapsed_ns: u64,
    t1_master_tx_ns: u64,
    t2_local_rx_ns: u64,
    t3_local_tx_ns: u64,
    t4_master_rx_ns: u64,
) -> Option<TimingMeasurement> {
    // Offset math in i128 to absorb any u64 differences, then narrow with a range check.
    let rx_leg = t2_local_rx_ns as i128 - t1_master_tx_ns as i128;
    let tx_leg = t3_local_tx_ns as i128 - t4_master_rx_ns as i128;
    let offset_i128 = (rx_leg + tx_leg) / 2;
    let offset_local_minus_master_ns = i64::try_from(offset_i128).ok()?;

    // Delay math stays in u64: every subtraction must be physically possible.
    let uplink = t4_master_rx_ns.checked_sub(t1_master_tx_ns)?;
    let downlink = t3_local_tx_ns.checked_sub(t2_local_rx_ns)?;
    let mean_path_delay_ns = uplink.checked_sub(downlink)? / 2;

    Some(TimingMeasurement {
        measurement_sequence,
        observed_elapsed_ns,
        offset_local_minus_master_ns,
        mean_path_delay_ns,
    })
}

/// Convert a local timestamp to master time.
///
/// `master = local - offset`. Returns `None` if the result would be negative
/// or exceed [`u64::MAX`] (checked conversion, no wraparound).
pub fn local_to_master_ns(local_ns: u64, offset_local_minus_master_ns: i64) -> Option<u64> {
    let master = local_ns as i128 - offset_local_minus_master_ns as i128;
    if master < 0 || master > u64::MAX as i128 {
        return None;
    }
    Some(master as u64)
}

// ===========================================================================
// Bounded drift / age / staleness
// ===========================================================================

/// Which time reference a diagnostics stream describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingReferenceKind {
    /// The sender's own reference clock (offsets are sender-internal).
    SenderReference,
    /// Offsets measured against a remote PTP/NTP master.
    PtpMeasured,
}

/// Least-squares drift estimate over the retained sample window.
#[derive(Clone, Debug, PartialEq)]
pub struct DriftEstimate {
    /// Estimated clock drift in parts per million (least-squares slope × 1e6).
    pub drift_ppm: f64,
    /// Number of retained samples the estimate is based on.
    pub sample_count: u16,
    /// Elapsed span from oldest to newest retained sample.
    pub window_ns: u64,
    /// Root-mean-square of the least-squares residuals in nanoseconds.
    pub residual_rms_ns: f64,
}

/// Point-in-time view of a timing source's health.
#[derive(Clone, Debug, PartialEq)]
pub struct TimingDiagnosticsSnapshot {
    /// Which reference this stream tracks.
    pub reference: TimingReferenceKind,
    /// Newest retained measurement, if any.
    pub latest: Option<TimingMeasurement>,
    /// Age of the newest measurement relative to the reader's clock.
    pub sample_age_ns: Option<u64>,
    /// True when the newest sample is older than the staleness threshold.
    pub stale: bool,
    /// Drift estimate when enough samples over a long-enough window exist.
    pub drift: Option<DriftEstimate>,
}

/// Ring buffer capacity for retained measurements.
const MAX_SAMPLES: usize = 256;
/// Minimum retained samples before a drift estimate is attempted.
const MIN_DRIFT_SAMPLES: usize = 8;
/// Minimum retained window before a drift estimate is attempted.
const MIN_DRIFT_WINDOW_NS: u64 = 2_000_000_000;
/// Absolute staleness floor.
const STALENESS_FLOOR_NS: u64 = 2_000_000_000;
/// Staleness multiplier applied to the median consecutive sampling interval.
const STALENESS_MEDIAN_FACTOR: u64 = 5;

struct SharedSamples {
    reference: TimingReferenceKind,
    samples: VecDeque<TimingMeasurement>,
    next_sequence: u64,
}

impl SharedSamples {
    fn push(&mut self, measurement: TimingMeasurement) {
        // The shared counter owns sequencing: it starts at 1 and increments
        // once per accepted record regardless of which entry point wrote it.
        let stamped = TimingMeasurement {
            measurement_sequence: self.next_sequence,
            ..measurement
        };
        self.next_sequence += 1;
        self.samples.push_back(stamped);
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    fn drift(&self) -> Option<DriftEstimate> {
        let oldest = self.samples.front()?;
        let newest = self.samples.back()?;
        let window_ns = newest
            .observed_elapsed_ns()
            .checked_sub(oldest.observed_elapsed_ns())?;
        if self.samples.len() < MIN_DRIFT_SAMPLES || window_ns < MIN_DRIFT_WINDOW_NS {
            return None;
        }

        let n = self.samples.len() as f64;
        // Center x at the oldest sample for numerical stability.
        let x_origin = oldest.observed_elapsed_ns() as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xx = 0.0;
        let mut sum_xy = 0.0;
        for sample in &self.samples {
            let x = sample.observed_elapsed_ns() as f64 - x_origin;
            let y = sample.offset_local_minus_master_ns() as f64;
            sum_x += x;
            sum_y += y;
            sum_xx += x * x;
            sum_xy += x * y;
        }

        let denominator = n * sum_xx - sum_x * sum_x;
        let slope = if denominator == 0.0 {
            0.0
        } else {
            (n * sum_xy - sum_x * sum_y) / denominator
        };
        let intercept = (sum_y - slope * sum_x) / n;

        let mut residual_sq_sum = 0.0;
        for sample in &self.samples {
            let x = sample.observed_elapsed_ns() as f64 - x_origin;
            let y = sample.offset_local_minus_master_ns() as f64;
            let residual = y - (slope * x + intercept);
            residual_sq_sum += residual * residual;
        }

        Some(DriftEstimate {
            drift_ppm: slope * 1e6,
            sample_count: self.samples.len() as u16,
            window_ns,
            residual_rms_ns: (residual_sq_sum / n).sqrt(),
        })
    }

    fn median_interval_ns(&self) -> Option<u64> {
        if self.samples.len() < 2 {
            return None;
        }
        let mut gaps: Vec<u64> = self
            .samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .map(|(older, newer)| {
                newer
                    .observed_elapsed_ns()
                    .saturating_sub(older.observed_elapsed_ns())
            })
            .collect();
        gaps.sort_unstable();
        Some(gaps[gaps.len() / 2])
    }

    fn staleness_threshold_ns(&self) -> u64 {
        match self.median_interval_ns() {
            Some(median) => STALENESS_FLOOR_NS.max(STALENESS_MEDIAN_FACTOR.saturating_mul(median)),
            None => STALENESS_FLOOR_NS,
        }
    }
}

fn lock_shared(state: &Mutex<SharedSamples>) -> MutexGuard<'_, SharedSamples> {
    // A poisoned lock must never wedge producers or readers; recover the data.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Producer-side writer half of a diagnostics pair.
///
/// Cloned [`TimingDiagnosticsHandle`]s observe everything recorded here.
pub struct TimingDiagnosticsRecorder {
    state: Arc<Mutex<SharedSamples>>,
}

impl TimingDiagnosticsRecorder {
    /// Record one measurement into the shared bounded ring.
    ///
    /// Never blocks on readers beyond the tiny critical section that appends.
    pub fn record(&self, measurement: TimingMeasurement) {
        lock_shared(&self.state).push(measurement);
    }
}

/// Reader half of a diagnostics pair; cheap to clone.
#[derive(Clone)]
pub struct TimingDiagnosticsHandle {
    state: Arc<Mutex<SharedSamples>>,
}

impl TimingDiagnosticsHandle {
    /// Take a point-in-time view of the shared history.
    ///
    /// `now_elapsed_ns` must come from the same monotone clock that stamped
    /// the measurements' `observed_elapsed_ns`.
    pub fn snapshot(&self, now_elapsed_ns: u64) -> TimingDiagnosticsSnapshot {
        let guard = lock_shared(&self.state);
        let latest = guard.samples.back().copied();
        let sample_age_ns = latest.map(|m| now_elapsed_ns.saturating_sub(m.observed_elapsed_ns()));
        let stale = match sample_age_ns {
            Some(age) => age > guard.staleness_threshold_ns(),
            None => false,
        };
        TimingDiagnosticsSnapshot {
            reference: guard.reference,
            latest,
            sample_age_ns,
            stale,
            drift: guard.drift(),
        }
    }

    /// Record one measurement from the reader side (same shared ring).
    pub fn record(&self, measurement: TimingMeasurement) {
        lock_shared(&self.state).push(measurement);
    }
}

/// Build a matched recorder/handle pair sharing one bounded ring buffer.
pub fn timing_diagnostics_pair(
    reference: TimingReferenceKind,
) -> (TimingDiagnosticsRecorder, TimingDiagnosticsHandle) {
    let state = Arc::new(Mutex::new(SharedSamples {
        reference,
        samples: VecDeque::new(),
        next_sequence: 1,
    }));
    (
        TimingDiagnosticsRecorder {
            state: Arc::clone(&state),
        },
        TimingDiagnosticsHandle { state },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    mod ptp_measurement {
        use super::*;

        #[test]
        fn symmetric_delays_yield_zero_offset() {
            let m = ptp_measurement(7, 1, 1_000, 1_100, 1_200, 1_300).unwrap();
            assert_eq!(m.offset_local_minus_master_ns(), 0);
            assert_eq!(m.mean_path_delay_ns(), 100);
        }
    }
}
