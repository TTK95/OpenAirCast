//! Per-target presentation clocks over one shared RTP stream.
//!
//! Every group member renders the *same* RTP stream: identical payload,
//! identical RTP timestamps, identical marker meaning. A calibration delay
//! must therefore never reach the audio packets — it only moves the
//! RTP-timestamp-to-clock anchor a target is told to render at, i.e. the
//! presentation time inside that target's own PT=84/PT=87 sync packet.
//!
//! The first test in this file is the load-bearing one: if a calibration ever
//! leaks into the shared RTP timestamp, group synchronisation collapses and
//! nothing in a test run would notice — only a listener in the room would.

use std::net::SocketAddr;

use airplay_audio::{
    checked_presentation_time_ns, prepare_frame_for_targets, FramePlan, PrepareFrameError,
    PresentationTimeError, RtpSender, TargetTransportCounters,
};

const COMMON_RTP_TS: u32 = 1_234_560;
const NEXT_RTP_TS: u32 = 1_234_912;
const CLOCK_ID: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
const PAYLOAD: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
/// Anchor far enough from the epoch that no test needs saturating arithmetic.
const ANCHOR_NS: u64 = 1_700_000_000_000_000_000;
const RENDER_LEAD_NS: u64 = 200_000_000;
const DELAY_NS: u64 = 5_000_000;

/// A sender that can serialize packets without touching the network: the
/// `prepare_*` family never needs a bound socket, only a non-zero control
/// destination port.
fn sender_fixture(ssrc: u32) -> RtpSender {
    let data: SocketAddr = "127.0.0.1:6000".parse().unwrap();
    let control: SocketAddr = "127.0.0.1:6001".parse().unwrap();
    let mut sender = RtpSender::new(data, ssrc);
    sender.set_control_dest(control);
    sender
}

fn plan(sync_due: bool, use_ptp_sync: bool) -> FramePlan<'static> {
    FramePlan {
        payload_type: 96,
        rtp_timestamp: COMMON_RTP_TS,
        next_rtp_timestamp: NEXT_RTP_TS,
        payload: PAYLOAD,
        marker: true,
        sync_due,
        use_ptp_sync,
        ptp_master_clock_id: CLOCK_ID,
        common_master_time_ns: ANCHOR_NS,
        global_render_lead_ns: RENDER_LEAD_NS,
        reference_master_time_ns: ANCHOR_NS - 1,
    }
}

/// Decode the presentation time out of a PT=87 sync packet (28 bytes).
fn ptp_presentation_ns(packet: &[u8]) -> u64 {
    assert_eq!(packet.len(), 28, "PT=87 sync packets are 28 bytes");
    assert_eq!(packet[1] & 0x7F, 87, "payload type must be 87");
    let secs = u32::from_be_bytes(packet[8..12].try_into().unwrap()) as u64;
    let frac = u32::from_be_bytes(packet[12..16].try_into().unwrap()) as u64;
    secs * 1_000_000_000 + ((frac * 1_000_000_000) >> 32)
}

/// Decode the presentation time out of a PT=84 sync packet (20 bytes).
fn ntp_presentation_ns(packet: &[u8]) -> u64 {
    assert_eq!(packet.len(), 20, "PT=84 sync packets are 20 bytes");
    assert_eq!(packet[1] & 0x7F, 84, "payload type must be 84");
    let ntp = u64::from_be_bytes(packet[8..16].try_into().unwrap());
    airplay_timing::ntp_to_unix(ntp)
}

fn rtp_timestamp_of(wire: &[u8]) -> u32 {
    u32::from_be_bytes(wire[4..8].try_into().unwrap())
}

fn assert_close(actual: u64, expected: u64, tolerance_ns: u64, what: &str) {
    let diff = actual.abs_diff(expected);
    assert!(
        diff <= tolerance_ns,
        "{what}: {actual} differs from {expected} by {diff} ns (tolerance {tolerance_ns})"
    );
}

// ===========================================================================
// The guarantee: calibration never touches the shared RTP stream
// ===========================================================================

#[test]
fn calibration_leaves_the_shared_audio_packets_byte_identical() {
    // Same SSRC, no cipher, same sequence start: two targets that differ in
    // nothing but their calibration delay must produce identical audio bytes.
    let mut senders = vec![sender_fixture(0xDEAD_BEEF), sender_fixture(0xDEAD_BEEF)];
    let delays = [0u64, DELAY_NS];

    let frame = prepare_frame_for_targets(&mut senders, &delays, &plan(true, true))
        .expect("frame must be preparable");

    assert_eq!(frame.wire_packets.len(), 2);
    assert_eq!(
        frame.wire_packets[0], frame.wire_packets[1],
        "a calibration delay must not change one single byte of the shared audio packet"
    );
    assert_eq!(rtp_timestamp_of(&frame.wire_packets[0]), COMMON_RTP_TS);
    assert_eq!(rtp_timestamp_of(&frame.wire_packets[1]), COMMON_RTP_TS);
    assert_eq!(&frame.wire_packets[0][12..], PAYLOAD);
    assert_eq!(&frame.wire_packets[1][12..], PAYLOAD);
    // Marker meaning is shared too.
    assert_eq!(frame.wire_packets[0][1] >> 7, 1);
    assert_eq!(frame.wire_packets[1][1] >> 7, 1);
}

#[test]
fn calibrated_and_uncalibrated_frames_carry_the_same_rtp_timestamp() {
    let mut uncalibrated = vec![sender_fixture(1), sender_fixture(1)];
    let flat = prepare_frame_for_targets(&mut uncalibrated, &[0, 0], &plan(true, true)).unwrap();

    let mut calibrated = vec![sender_fixture(1), sender_fixture(1)];
    let shifted =
        prepare_frame_for_targets(&mut calibrated, &[0, DELAY_NS], &plan(true, true)).unwrap();

    assert_eq!(
        flat.wire_packets, shifted.wire_packets,
        "the shared RTP stream must be bit-identical with and without calibration"
    );

    // The PT=87 anchor of the *uncalibrated* target is unchanged as well.
    let flat_sync = flat.sync_packets[0].as_ref().unwrap();
    let shifted_sync = shifted.sync_packets[0].as_ref().unwrap();
    assert_eq!(flat_sync, shifted_sync);
}

// ===========================================================================
// Per-target presentation time
// ===========================================================================

#[test]
fn ptp_sync_presentation_time_differs_by_the_effective_delay() {
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];
    let frame = prepare_frame_for_targets(&mut senders, &[0, DELAY_NS], &plan(true, true)).unwrap();

    let first = frame.sync_packets[0].as_ref().expect("sync due");
    let second = frame.sync_packets[1].as_ref().expect("sync due");

    assert_close(
        ptp_presentation_ns(first),
        ANCHOR_NS + RENDER_LEAD_NS,
        2,
        "uncalibrated target keeps anchor + global render lead",
    );
    assert_close(
        ptp_presentation_ns(second),
        ANCHOR_NS + RENDER_LEAD_NS + DELAY_NS,
        2,
        "calibrated target is delayed by exactly its effective delay",
    );

    // Everything else in the sync packet stays shared.
    assert_eq!(first[4..8], second[4..8], "current RTP timestamp is shared");
    assert_eq!(
        first[16..20],
        second[16..20],
        "next RTP timestamp is shared"
    );
    assert_eq!(
        first[20..28],
        second[20..28],
        "PTP clock identity is shared"
    );
    assert_eq!(
        u32::from_be_bytes(first[4..8].try_into().unwrap()),
        COMMON_RTP_TS
    );
    assert_eq!(
        u32::from_be_bytes(first[16..20].try_into().unwrap()),
        NEXT_RTP_TS
    );
}

#[test]
fn ntp_sync_presentation_time_differs_by_the_effective_delay() {
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];
    let frame =
        prepare_frame_for_targets(&mut senders, &[0, DELAY_NS], &plan(true, false)).unwrap();

    let first = frame.sync_packets[0].as_ref().expect("sync due");
    let second = frame.sync_packets[1].as_ref().expect("sync due");

    assert_close(
        ntp_presentation_ns(first),
        ANCHOR_NS + RENDER_LEAD_NS,
        2,
        "uncalibrated PT=84 anchor",
    );
    assert_close(
        ntp_presentation_ns(second),
        ANCHOR_NS + RENDER_LEAD_NS + DELAY_NS,
        2,
        "calibrated PT=84 anchor",
    );

    assert_eq!(first[4..8], second[4..8], "playback position is shared");
    assert_eq!(first[16..20], second[16..20], "RTP timestamp is shared");
}

// ===========================================================================
// Per-target sync state
// ===========================================================================

#[test]
fn every_target_advances_its_own_sync_sequence_and_extension_state() {
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];

    let first = prepare_frame_for_targets(&mut senders, &[0, DELAY_NS], &plan(true, true)).unwrap();
    let second =
        prepare_frame_for_targets(&mut senders, &[0, DELAY_NS], &plan(true, true)).unwrap();

    for target in 0..2 {
        let a = first.sync_packets[target].as_ref().unwrap();
        let b = second.sync_packets[target].as_ref().unwrap();
        assert_eq!(
            a[0], 0x90,
            "target {target}: first sync must carry the extension bit"
        );
        assert_eq!(b[0], 0x80, "target {target}: later syncs must not");
        assert_eq!(
            u16::from_be_bytes(a[2..4].try_into().unwrap()),
            0,
            "target {target}: sync sequence starts at 0"
        );
        assert_eq!(
            u16::from_be_bytes(b[2..4].try_into().unwrap()),
            1,
            "target {target}: sync sequence advances per target"
        );
    }
}

#[test]
fn sync_state_stays_per_target_when_one_target_joins_late() {
    // A freshly reset sender must re-emit the extension bit without dragging
    // the already-running target back to its first sync.
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];
    let _ = prepare_frame_for_targets(&mut senders, &[0, 0], &plan(true, true)).unwrap();
    senders[1].reset_sync_state();

    let frame = prepare_frame_for_targets(&mut senders, &[0, 0], &plan(true, true)).unwrap();
    assert_eq!(frame.sync_packets[0].as_ref().unwrap()[0], 0x80);
    assert_eq!(frame.sync_packets[1].as_ref().unwrap()[0], 0x90);
}

#[test]
fn sync_and_wire_vectors_always_match_the_target_count() {
    for target_count in 1..=4usize {
        let mut senders: Vec<RtpSender> = (0..target_count)
            .map(|i| sender_fixture(i as u32))
            .collect();
        let delays = vec![0u64; target_count];

        let due = prepare_frame_for_targets(&mut senders, &delays, &plan(true, true)).unwrap();
        assert_eq!(due.wire_packets.len(), target_count);
        assert_eq!(due.sync_packets.len(), target_count);
        assert!(due.sync_packets.iter().all(|s| s.is_some()));

        let quiet = prepare_frame_for_targets(&mut senders, &delays, &plan(false, true)).unwrap();
        assert_eq!(quiet.wire_packets.len(), target_count);
        assert_eq!(quiet.sync_packets.len(), target_count);
        assert!(
            quiet.sync_packets.iter().all(|s| s.is_none()),
            "no sync due means an empty slot per target, never a shorter vector"
        );
    }
}

#[test]
fn a_delay_vector_of_the_wrong_length_is_rejected() {
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];
    let err = prepare_frame_for_targets(&mut senders, &[0], &plan(true, true))
        .expect_err("one delay for two targets must be rejected");
    assert!(matches!(
        err,
        PrepareFrameError::TargetCountMismatch {
            targets: 2,
            delays: 1
        }
    ));
}

// ===========================================================================
// Checked time validation
// ===========================================================================

#[test]
fn checked_presentation_time_adds_lead_and_effective_delay() {
    assert_eq!(checked_presentation_time_ns(1_000, 200, 50, 999), Ok(1_250));
    // Zero calibration is a valid request, not a missing one.
    assert_eq!(checked_presentation_time_ns(1_000, 0, 0, 999), Ok(1_000));
}

#[test]
fn checked_presentation_time_rejects_overflow() {
    assert_eq!(
        checked_presentation_time_ns(u64::MAX, 1, 0, 0),
        Err(PresentationTimeError::Overflow)
    );
    assert_eq!(
        checked_presentation_time_ns(u64::MAX - 10, 5, 6, 0),
        Err(PresentationTimeError::Overflow)
    );
}

#[test]
fn checked_presentation_time_rejects_non_future_times() {
    // Exactly "now" is not a future presentation time.
    assert_eq!(
        checked_presentation_time_ns(1_000, 0, 0, 1_000),
        Err(PresentationTimeError::NotInFuture)
    );
    assert_eq!(
        checked_presentation_time_ns(1_000, 100, 100, 5_000),
        Err(PresentationTimeError::NotInFuture)
    );
    // One nanosecond of future is enough.
    assert_eq!(checked_presentation_time_ns(1_000, 0, 1, 1_000), Ok(1_001));
}

#[test]
fn an_unrepresentable_presentation_time_leaves_every_sender_untouched() {
    let counters = [
        std::sync::Arc::new(TargetTransportCounters::new()),
        std::sync::Arc::new(TargetTransportCounters::new()),
    ];
    let mut senders = vec![sender_fixture(1), sender_fixture(2)];
    for (sender, counter) in senders.iter_mut().zip(counters.iter()) {
        sender.set_transport_counters(std::sync::Arc::clone(counter));
    }

    let err = prepare_frame_for_targets(&mut senders, &[0, u64::MAX], &plan(true, true))
        .expect_err("an overflowing delay must be rejected");
    assert!(matches!(
        err,
        PrepareFrameError::PresentationTime {
            index: 1,
            source: PresentationTimeError::Overflow
        }
    ));

    // No datagram was attempted for any target...
    for counter in &counters {
        let snapshot = counter.snapshot();
        assert_eq!(snapshot.data_datagrams_attempted_total, 0);
        assert_eq!(snapshot.sync_datagrams_attempted_total, 0);
        assert_eq!(snapshot.data_send_failures_total, 0);
        assert_eq!(snapshot.sync_send_failures_total, 0);
    }

    // ...and no sender consumed a sequence number or its first-sync extension.
    let recovered = prepare_frame_for_targets(&mut senders, &[0, 0], &plan(true, true))
        .expect("a valid frame after a rejection must still be the first one");
    for target in 0..2 {
        let sync = recovered.sync_packets[target].as_ref().unwrap();
        assert_eq!(sync[0], 0x90, "target {target}: extension bit unspent");
        assert_eq!(u16::from_be_bytes(sync[2..4].try_into().unwrap()), 0);
        assert_eq!(
            u16::from_be_bytes(recovered.wire_packets[target][2..4].try_into().unwrap()),
            0,
            "target {target}: RTP sequence unspent"
        );
    }
}

#[test]
fn a_backwards_master_clock_is_rejected_instead_of_anchored_in_the_past() {
    let mut senders = vec![sender_fixture(1)];
    let mut backwards = plan(true, true);
    // The reference already lies beyond anchor + lead: the clock went back.
    backwards.reference_master_time_ns = ANCHOR_NS + RENDER_LEAD_NS + 1;

    let err = prepare_frame_for_targets(&mut senders, &[0], &backwards)
        .expect_err("a non-advancing master clock must be rejected");
    assert!(matches!(
        err,
        PrepareFrameError::PresentationTime {
            index: 0,
            source: PresentationTimeError::NotInFuture
        }
    ));
}
