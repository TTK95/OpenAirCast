//! Contract tests for the client diagnostics source (Task 6).
//!
//! All tests are hardware-independent: they drive the diagnostics machinery
//! through documented test seams (`#[doc(hidden)]` constructors/recorders)
//! without requiring an AirPlay receiver, RTSP server, or network I/O.

use airplay_client::{
    ClientDiagnosticKind, ClientDiagnosticsSnapshot, ClientDiagnosticsSource, FeedbackRecorder,
    FeedbackResultKind, FeedbackSnapshot,
};
use airplay_core::DeviceId;

fn fixture_id(last_byte: u8) -> DeviceId {
    DeviceId([0x02, 0x11, 0x22, 0x33, 0x44, last_byte])
}

/// Feeding one Success, one ProtocolFailure, one TransportFailure and one
/// Timeout into the feedback recorder must yield totals 1/1/1/1 with the
/// timeout as the last result and durations preserved.
#[test]
fn feedback_snapshot_classifies_all_four_results() {
    let recorder: FeedbackRecorder = ClientDiagnosticsSource::test_feedback_recorder();
    recorder.record(FeedbackResultKind::Success, 1_000, 100);
    recorder.record(FeedbackResultKind::ProtocolFailure, 2_000, 200);
    recorder.record(FeedbackResultKind::TransportFailure, 3_000, 300);
    recorder.record(FeedbackResultKind::Timeout, 4_000, 400);

    let snap = recorder.snapshot();
    assert_eq!(snap.attempts_total, 4);
    assert_eq!(snap.successes_total, 1);
    assert_eq!(snap.protocol_failures_total, 1);
    assert_eq!(snap.transport_failures_total, 1);
    assert_eq!(snap.timeouts_total, 1);
    assert_eq!(snap.last_result, Some(FeedbackResultKind::Timeout));
    assert_eq!(snap.last_transaction_duration_ns, Some(4_000));
    assert_eq!(snap.last_success_elapsed_ns, Some(100));
}

/// Registering the same two receivers in different insertion orders must
/// produce identical, DeviceId-sorted connection sequences in snapshots.
#[test]
fn receiver_mapping_is_stable_across_insertion_order() {
    let a = fixture_id(0x0A);
    let b = fixture_id(0x0B);
    let expected: Vec<DeviceId> = vec![a.clone(), b.clone()];

    let discovery_order_ab = ClientDiagnosticsSource::test_new_empty();
    discovery_order_ab.test_register_connection(a.clone(), Some(0));
    discovery_order_ab.test_register_connection(b.clone(), Some(1));

    let discovery_order_ba = ClientDiagnosticsSource::test_new_empty();
    discovery_order_ba.test_register_connection(b.clone(), Some(0));
    discovery_order_ba.test_register_connection(a.clone(), Some(1));

    let snap_ab: ClientDiagnosticsSnapshot = discovery_order_ab.snapshot(0);
    let snap_ba = discovery_order_ba.snapshot(0);

    let ids_ab: Vec<DeviceId> = snap_ab
        .connections
        .iter()
        .map(|c| c.device_id.clone())
        .collect();
    let ids_ba: Vec<DeviceId> = snap_ba
        .connections
        .iter()
        .map(|c| c.device_id.clone())
        .collect();

    assert_eq!(
        ids_ab, expected,
        "A/B registration must sort to stable order"
    );
    assert_eq!(
        ids_ba, expected,
        "B/A registration must sort to the same stable order"
    );

    // The stable mapping must also carry the original target indices.
    assert_eq!(
        snap_ab
            .connections
            .iter()
            .map(|c| c.sender_target_index)
            .collect::<Vec<_>>(),
        vec![Some(0), Some(1)]
    );
    assert_eq!(
        snap_ba
            .connections
            .iter()
            .map(|c| c.sender_target_index)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(0)],
        "indices follow the device, not the vector position"
    );
}

/// Draining the bounded event ring must yield events strictly FIFO with
/// exactly-once delivery across successive limited drains.
#[test]
fn drain_events_fifo_respects_limit() {
    let source = ClientDiagnosticsSource::test_new_empty();
    source.test_register_connection(fixture_id(0x01), None);

    let pushed_kinds = [
        ClientDiagnosticKind::Feedback,
        ClientDiagnosticKind::RuntimeWarning,
        ClientDiagnosticKind::SetupFailure,
        ClientDiagnosticKind::Feedback,
        ClientDiagnosticKind::RuntimeWarning,
        ClientDiagnosticKind::SetupFailure,
        ClientDiagnosticKind::Feedback,
    ];
    for (i, kind) in pushed_kinds.iter().enumerate() {
        source.test_record_event(Some(fixture_id(i as u8 + 1)), *kind);
    }

    let first = source.test_drain_events(3);
    assert_eq!(first.len(), 3);
    let first_kinds: Vec<ClientDiagnosticKind> = first.iter().map(|e| e.kind).collect();
    assert_eq!(first_kinds, pushed_kinds[..3]);
    let first_devices: Vec<Option<DeviceId>> = first.iter().map(|e| e.device_id.clone()).collect();
    assert_eq!(
        first_devices,
        vec![
            Some(fixture_id(1)),
            Some(fixture_id(2)),
            Some(fixture_id(3))
        ]
    );

    let second = source.test_drain_events(3);
    assert_eq!(second.len(), 3);
    let second_kinds: Vec<ClientDiagnosticKind> = second.iter().map(|e| e.kind).collect();
    assert_eq!(second_kinds, pushed_kinds[3..6]);

    let third = source.test_drain_events(3);
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].kind, pushed_kinds[6]);
    assert_eq!(third[0].device_id, Some(fixture_id(7)));

    let fourth = source.test_drain_events(3);
    assert!(
        fourth.is_empty(),
        "exactly-once: nothing left after full drain"
    );
}

/// A brand-new source must report truthfully-empty values: no connections,
/// no audio half, zero drop counter, no fabricated numbers anywhere.
#[test]
fn snapshot_defaults_are_truthfully_empty() {
    let source = ClientDiagnosticsSource::test_new_empty();
    let snap = source.snapshot(123_456);
    assert_eq!(snap.captured_elapsed_ns, 123_456);
    assert!(snap.connections.is_empty());
    assert!(snap.audio.is_none());
    assert_eq!(snap.client_events_dropped_total, 0);

    let fresh_feedback: FeedbackSnapshot =
        ClientDiagnosticsSource::test_feedback_recorder().snapshot();
    assert_eq!(fresh_feedback, FeedbackSnapshot::default());
    assert_eq!(fresh_feedback.attempts_total, 0);
    assert_eq!(fresh_feedback.successes_total, 0);
    assert_eq!(fresh_feedback.protocol_failures_total, 0);
    assert_eq!(fresh_feedback.transport_failures_total, 0);
    assert_eq!(fresh_feedback.timeouts_total, 0);
    assert_eq!(fresh_feedback.last_result, None);
    assert_eq!(fresh_feedback.last_transaction_duration_ns, None);
    assert_eq!(fresh_feedback.last_success_elapsed_ns, None);
}
