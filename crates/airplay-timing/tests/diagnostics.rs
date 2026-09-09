//! Integration tests for the timing diagnostics module.
//!
//! These tests pin down the NTP/PTP standard math with the explicit sign
//! convention: `offset = local - master`. The offset is a *rechnungsgrenze*
//! (explicit computational boundary): every conversion goes through checked
//! arithmetic and returns `None` instead of wrapping or panicking.

use airplay_timing::diagnostics::{local_to_master_ns, ptp_measurement, TimingMeasurement};

#[test]
fn timing_formula_uses_local_minus_master_sign() {
    // Asymmetric wire delays prove the sign:
    //   t1 = master TX, t2 = local RX  (+100 ns wire delay to us)
    //   t3 = local TX, t4 = master RX  (+200 ns wire delay from us)
    let t1: u64 = 1_000_000_000;
    let t2: u64 = 1_000_000_100;
    let t3: u64 = 1_000_000_300;
    let t4: u64 = 1_000_000_500;

    // offset = ((t2 - t1) + (t3 - t4)) / 2 = (100 + (-200)) / 2 = -50
    let expected_offset = ((t2 as i128 - t1 as i128) + (t3 as i128 - t4 as i128)) / 2;
    assert_eq!(expected_offset, -50);

    // mean_path_delay = ((t4 - t1) - (t3 - t2)) / 2 = (500 - 200) / 2 = 150
    let expected_delay = ((t4 - t1) - (t3 - t2)) / 2;
    assert_eq!(expected_delay, 150);

    let m = ptp_measurement(0, 0, t1, t2, t3, t4)
        .expect("measurement with sane timestamps must be Some");

    assert_eq!(
        m.offset_local_minus_master_ns(),
        -50,
        "sign must be local minus master (negative => local ahead)"
    );
    assert_eq!(m.mean_path_delay_ns(), 150);

    // Converting local -> master with offset = -50:
    // master = local - offset = 1e9 - (-50) = 1e9 + 50
    assert_eq!(local_to_master_ns(1_000_000_000, -50), Some(1_000_000_050));

    // Roundtrip sanity through the measurement itself:
    assert_eq!(
        local_to_master_ns(t2, m.offset_local_minus_master_ns()),
        Some(t2 + 50)
    );
}

#[test]
fn checked_conversion_rejects_underflow() {
    // Case 1: mean path delay impossible because t4 < t1.
    let t1: u64 = 1_000_000_500;
    let t2: u64 = 1_000_000_600;
    let t3: u64 = 1_000_000_700;
    let t4: u64 = 1_000_000_000; // master RX before master TX
    assert_eq!(ptp_measurement(1, 0, t1, t2, t3, t4), None);

    // Case 2: local_to_master would go negative.
    assert_eq!(local_to_master_ns(10, 20), None);

    // Case 3: i128 offset math overflows i64 when narrowed.
    // offset = ((t2 - t1) + (t3 - t4)) / 2
    //        = (u64::MAX + 1) / 2 = i64::MAX + 1  => does not fit into i64.
    // Note: u64::MAX / 2 == i64::MAX exactly, so we need one extra step above it.
    let max = u64::MAX;
    let t1: u64 = 0;
    let t2: u64 = max; // t2 - t1 = u64::MAX = 2 * i64::MAX + 1
    let t3: u64 = max; // t3 - t4 = 1
    let t4: u64 = max - 1;

    // mean_path_delay = ((t4 - t1) - (t3 - t2)) / 2 = (max-1 - 0) / 2, no underflow,
    // so ONLY the i64-narrowing of the offset can fail => None.
    assert_eq!(ptp_measurement(2, 0, t1, t2, t3, t4), None);
}

#[test]
fn sequence_and_elapsed_are_carried_through() {
    let t1: u64 = 10_000;
    let t2: u64 = 10_050;
    let t3: u64 = 10_100;
    let t4: u64 = 10_150;

    let m =
        ptp_measurement(42, 987_654_321, t1, t2, t3, t4).expect("sane timestamps must yield Some");

    assert_eq!(m.measurement_sequence(), 42);
    assert_eq!(m.observed_elapsed_ns(), 987_654_321);
}

#[test]
fn zero_offset_identity() {
    // With offset 0 the conversion must be the identity.
    assert_eq!(local_to_master_ns(0, 0), Some(0));
    assert_eq!(local_to_master_ns(u64::MAX, 0), Some(u64::MAX));
}

#[test]
fn a_positive_offset_moves_the_timestamp_backwards() {
    // Positive offset = the local clock reads ahead of the master, so the
    // same instant carries a *smaller* number on the master timeline. This is
    // the direction every stream timestamp boundary has to use.
    assert_eq!(local_to_master_ns(10_000, 50), Some(9_950));
    assert_eq!(local_to_master_ns(10_000, -50), Some(10_050));
}

#[test]
fn master_conversion_roundtrips_through_the_inverse() {
    // master = local - offset, so local = master + offset. Both directions
    // together must be the identity for every representable offset sign.
    for offset in [-1_000_000i64, -1, 0, 1, 5_000_000] {
        let local: u64 = 12_345_678_901;
        let master = local_to_master_ns(local, offset).expect("representable");
        let back = (master as i128 + offset as i128) as u64;
        assert_eq!(back, local, "roundtrip broken for offset {offset}");
    }
}

// ---------------------------------------------------------------------------
// Bounded drift / age / staleness
//
// Synthetic exchanges are built with symmetric wire delay D and reply gap R:
//   t2 = t1 + D + o   (local RX: true delay D plus clock offset o)
//   t3 = t2 + R       (local processing)
//   t4 = t1 + 2D + R  (master RX after the return trip)
// which yields offset = o and mean path delay = D exactly.
// ---------------------------------------------------------------------------

use airplay_timing::diagnostics::{timing_diagnostics_pair, TimingReferenceKind};

const WIRE_DELAY_NS: i64 = 100_000;
const REPLY_GAP_NS: i64 = 200_000;

fn synth(seq: u64, elapsed_ns: u64, offset_ns: i64) -> TimingMeasurement {
    let t1: i64 = 1_000_000_000;
    let t2 = t1 + WIRE_DELAY_NS + offset_ns;
    let t3 = t2 + REPLY_GAP_NS;
    let t4 = t1 + 2 * WIRE_DELAY_NS + REPLY_GAP_NS;
    let conv = |v: i64| u64::try_from(v).expect("synthetic timestamps stay non-negative");
    ptp_measurement(seq, elapsed_ns, conv(t1), conv(t2), conv(t3), conv(t4))
        .expect("synthetic exchange must satisfy wire constraints")
}

#[test]
fn drift_estimate_requires_eight_samples_and_two_seconds() {
    // 7 samples spread over many seconds: window is long enough, sample count is not.
    let (_recorder, handle) = timing_diagnostics_pair(TimingReferenceKind::PtpMeasured);
    for i in 0..7u64 {
        handle.record(synth(0, i * 1_000_000_000, i as i64 * 50_000));
    }
    let snap = handle.snapshot(7 * 1_000_000_000);
    assert!(
        snap.drift.is_none(),
        "7 samples must not produce a drift estimate"
    );

    // 8 samples crammed below two seconds: sample count is enough, window is not.
    let (_recorder, short_window) = timing_diagnostics_pair(TimingReferenceKind::PtpMeasured);
    for i in 0..8u64 {
        short_window.record(synth(0, i * 100_000_000, i as i64 * 5_000));
    }
    let snap = short_window.snapshot(8 * 100_000_000);
    assert!(
        snap.drift.is_none(),
        "a sub-two-second window must not produce a drift estimate"
    );

    // 8 samples spanning >= 2 s on an exact +50 ppm ramp: drift becomes Some.
    let (_recorder, full) = timing_diagnostics_pair(TimingReferenceKind::PtpMeasured);
    for i in 0..8u64 {
        full.record(synth(0, i * 500_000_000, i as i64 * 25_000));
    }
    let snap = full.snapshot(8 * 500_000_000);
    let drift = snap
        .drift
        .expect("8 samples over 3.5 s must yield a drift estimate");
    assert_eq!(drift.sample_count, 8);
    assert_eq!(drift.window_ns, 7 * 500_000_000);
    assert!(
        (drift.drift_ppm - 50.0).abs() < 1e-6,
        "expected ~50 ppm, got {}",
        drift.drift_ppm
    );
    assert!(
        drift.residual_rms_ns < 1e-6,
        "a perfect line must leave no residual, got {}",
        drift.residual_rms_ns
    );
}

#[test]
fn drift_window_is_bounded_to_256_samples() {
    let (recorder, handle) = timing_diagnostics_pair(TimingReferenceKind::SenderReference);
    for i in 0..300u64 {
        recorder.record(synth(999, i * 10_000_000, 0));
    }

    let snap = handle.snapshot(300 * 10_000_000);
    let drift = snap
        .drift
        .expect("256 retained samples span 2.55 s, enough for drift");
    assert_eq!(
        drift.sample_count, 256,
        "ring buffer must cap at 256 samples"
    );
    assert_eq!(
        snap.latest
            .expect("newest measurement must be kept")
            .measurement_sequence(),
        300,
        "sequence keeps counting beyond ring capacity"
    );
}

#[test]
fn staleness_uses_max_of_two_seconds_and_five_median_intervals() {
    // Fast cadence (100 ms): five-median bound 0.5 s < 2 s floor, floor decides.
    let (_recorder, fast) = timing_diagnostics_pair(TimingReferenceKind::PtpMeasured);
    for i in 0..5u64 {
        fast.record(synth(0, i * 100_000_000, 0));
    }
    let latest_elapsed = 4 * 100_000_000;

    let snap = fast.snapshot(latest_elapsed + 1_900_000_000);
    assert!(!snap.stale, "just under the two-second floor");
    let snap = fast.snapshot(latest_elapsed + 2_500_000_000);
    assert!(snap.stale, "past the two-second floor");

    // Slow cadence (600 ms): five-median bound 3 s > 2 s floor, median term decides.
    let (_recorder, slow) = timing_diagnostics_pair(TimingReferenceKind::SenderReference);
    for i in 0..5u64 {
        slow.record(synth(0, i * 600_000_000, 0));
    }
    let latest_elapsed = 4 * 600_000_000;

    let snap = slow.snapshot(latest_elapsed + 2_100_000_000);
    assert!(
        !snap.stale,
        "older than the 2 s floor but still within 5x median interval"
    );
    let snap = slow.snapshot(latest_elapsed + 3_100_000_000);
    assert!(snap.stale, "beyond 5x median interval");
}

#[test]
fn sample_age_and_latest_are_reported() {
    let (_recorder, handle) = timing_diagnostics_pair(TimingReferenceKind::PtpMeasured);

    let empty = handle.snapshot(12_345);
    assert!(empty.latest.is_none());
    assert_eq!(empty.sample_age_ns, None);
    assert!(!empty.stale, "no samples means never stale");
    assert!(empty.drift.is_none());

    handle.record(synth(7, 1_000_000_000, -42));

    let snap = handle.snapshot(1_000_001_234);
    assert_eq!(snap.reference, TimingReferenceKind::PtpMeasured);
    let latest = snap.latest.expect("latest present after record");
    assert_eq!(latest.offset_local_minus_master_ns(), -42);
    assert_eq!(latest.mean_path_delay_ns(), 100_000);
    assert_eq!(snap.sample_age_ns, Some(1_234));

    // A reader clock behind the newest observation saturates at zero age.
    let snap = handle.snapshot(0);
    assert_eq!(snap.sample_age_ns, Some(0));
}

#[test]
fn recorder_handle_roundtrip_preserves_sequence_order() {
    let (recorder, handle) = timing_diagnostics_pair(TimingReferenceKind::SenderReference);

    for i in 0..4u64 {
        recorder.record(synth(999, i * 1_000_000_000, i as i64 * 50_000));

        let snap = handle.snapshot(i * 1_000_000_000);
        let latest = snap.latest.expect("each record becomes the newest sample");
        assert_eq!(
            latest.measurement_sequence(),
            i + 1,
            "sequences start at 1 and increment per record"
        );
        assert_eq!(latest.observed_elapsed_ns(), i * 1_000_000_000);
        assert_eq!(latest.offset_local_minus_master_ns(), i as i64 * 50_000);
    }

    handle.record(synth(999, 4 * 1_000_000_000, 200_000));
    let snap = handle.snapshot(4 * 1_000_000_000);
    assert_eq!(
        snap.latest
            .expect("handle.record feeds the same history")
            .measurement_sequence(),
        5,
        "both writer entry points share one monotone sequence"
    );
}
