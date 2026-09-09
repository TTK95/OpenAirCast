//! Contract tests for the structured diagnostics domain and bounded cursor ring.
//!
//! Covers Task 7 of the diagnostics/calibration plan: monotonic cursors from
//! one, clamped read limits, explicit overwrite gaps, head-anchored empty
//! reads, non-blocking burst ingestion, and stable error-code surface.
//!
//! The `registry_*` and `health_*` sections cover Task 8: the single
//! background registry consumer, immutable schema-version-1 snapshots,
//! truthful lag accounting, monotonically increasing snapshot sequences,
//! exactly-once event round-trips, and the conservative
//! Unknown/Running/Attention/Error health model.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use airplay_client::{ClientDiagnosticsSource, DeviceId};
use tokio::sync::broadcast;

use homepod_cast::backend::{
    DiagnosticCategory, DiagnosticEvent as BackendFeedEvent, DiagnosticPayload, Severity,
};
use homepod_cast::diagnostics::{
    CalibrationApplyState, DefiniteCounterKind, DiagnosticComponent, DiagnosticError,
    DiagnosticErrorCode, DiagnosticEventDraft, DiagnosticSeverity, DiagnosticsClock,
    DiagnosticsHandle, DiagnosticsRegistry, DiagnosticsRing, EventCursor, EventGap,
    ExportEventState, Health, ReceiverSessionKey, Recoverability, SessionId, SessionRegistration,
    SessionStopReason, StructuredDiagnosticPayload, EVENT_RING_CAPACITY, MAX_EVENT_READ_LIMIT,
};
use homepod_cast::{ReceiverId, ReceiverLifecycle, UserFacingError};

fn receiver(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
        .expect("fixed test storage key is valid")
}

fn message_draft(tag: &str) -> DiagnosticEventDraft {
    DiagnosticEventDraft {
        occurred_at_utc: SystemTime::UNIX_EPOCH + Duration::from_secs(7),
        process_elapsed_ns: 42,
        receiver_session: None,
        severity: DiagnosticSeverity::Info,
        component: DiagnosticComponent::Export,
        code: DiagnosticErrorCode::ExportFailure,
        public_message: tag.to_owned(),
        payload: StructuredDiagnosticPayload::Backend(DiagnosticPayload::Message(tag.into())),
    }
}

#[test]
fn event_cursor_assigns_monotonic_sequence_from_one() {
    let ring = DiagnosticsRing::new();

    let first = SessionId::generate();
    let second = SessionId::generate();
    assert_ne!(first, second, "every session start must mint a fresh id");

    let mut keyed = message_draft("keyed");
    keyed.component = DiagnosticComponent::Pairing;
    keyed.code = DiagnosticErrorCode::PairingFailure;
    keyed.severity = DiagnosticSeverity::Error;
    keyed.receiver_session = Some(ReceiverSessionKey {
        session_id: first,
        receiver_id: receiver(1),
    });

    let c1 = ring.push(keyed);
    let c2 = ring.push(message_draft("b"));
    let c3 = ring.push(message_draft("c"));

    assert_eq!(c1, EventCursor(1));
    assert_eq!(c2, EventCursor(2));
    assert_eq!(c3, EventCursor(3));

    let batch = ring.events_since(EventCursor(0), 512);
    assert_eq!(batch.events.len(), 3);
    assert_eq!(
        batch.events.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        vec![EventCursor(1), EventCursor(2), EventCursor(3)]
    );
    let head = &batch.events[0];
    assert_eq!(head.code, DiagnosticErrorCode::PairingFailure);
    assert_eq!(head.component, DiagnosticComponent::Pairing);
    assert_eq!(head.severity, DiagnosticSeverity::Error);
    assert_eq!(head.public_message, "keyed");
    assert_eq!(head.process_elapsed_ns, 42);
    let key = head
        .receiver_session
        .as_ref()
        .expect("keyed draft keeps its key");
    assert_eq!(key.receiver_id, receiver(1));
    assert_eq!(key.session_id, first);
}

#[test]
fn read_limit_clamps_to_512_and_returns_exact_batch_window() {
    let ring = DiagnosticsRing::new();
    for index in 0..600 {
        ring.push(message_draft(&format!("e{index}")));
    }

    let zero_limit = ring.events_since(EventCursor(0), 0);
    assert_eq!(zero_limit.events.len(), 1);
    assert_eq!(zero_limit.events[0].cursor, EventCursor(1));

    let huge_limit = ring.events_since(EventCursor(0), 999);
    assert_eq!(huge_limit.events.len(), MAX_EVENT_READ_LIMIT);
    assert_eq!(huge_limit.events.len(), 512);
    let cursors: Vec<u64> = huge_limit.events.iter().map(|e| e.cursor.0).collect();
    let expected: Vec<u64> = (1..=512).collect();
    assert_eq!(cursors, expected);
    assert_eq!(huge_limit.next_cursor, EventCursor(512));
    assert_eq!(huge_limit.oldest_available_cursor, EventCursor(1));
    assert_eq!(huge_limit.gap, None);

    let remainder = ring.events_since(EventCursor(512), 512);
    assert_eq!(remainder.events.len(), 88);
    assert_eq!(remainder.events[0].cursor, EventCursor(513));
    assert_eq!(remainder.events[87].cursor, EventCursor(600));
    assert_eq!(remainder.next_cursor, EventCursor(600));
    assert_eq!(remainder.gap, None);

    let caught_up = ring.events_since(EventCursor(600), 512);
    assert!(caught_up.events.is_empty());
    assert_eq!(caught_up.next_cursor, EventCursor(600));
}

#[test]
fn overwrite_produces_explicit_gap_with_truthful_counts() {
    let ring = DiagnosticsRing::new();
    assert_eq!(EVENT_RING_CAPACITY, 4096);

    for index in 0..4_100_u64 {
        ring.push(message_draft(&format!("g{index}")));
    }

    let behind = ring.events_since(EventCursor(1), 10);
    assert_eq!(
        behind.gap,
        Some(EventGap {
            requested_cursor: EventCursor(1),
            resumed_at_cursor: EventCursor(5),
            overwritten_events: 4,
        }),
        "cursor 1..=4 were overwritten; the gap must say so explicitly"
    );
    assert_eq!(behind.oldest_available_cursor, EventCursor(5));
    assert_eq!(behind.events.len(), 10);
    assert_eq!(behind.events[0].cursor, EventCursor(5));
    assert_eq!(behind.events[9].cursor, EventCursor(14));
    assert_eq!(behind.next_cursor, EventCursor(14));

    let at_boundary = ring.events_since(EventCursor(5), 10);
    assert_eq!(
        at_boundary.gap, None,
        "requesting exactly the oldest retained cursor is not a gap"
    );
    assert_eq!(at_boundary.events[0].cursor, EventCursor(6));
}

#[test]
fn empty_range_yields_next_cursor_at_head_without_gap() {
    let ring = DiagnosticsRing::new();

    let fresh = ring.events_since(EventCursor(0), 512);
    assert!(fresh.events.is_empty());
    assert_eq!(fresh.next_cursor, EventCursor(0));
    assert_eq!(fresh.gap, None);

    for index in 0..3 {
        ring.push(message_draft(&format!("h{index}")));
    }

    let drained = ring.events_since(EventCursor(0), 512);
    assert_eq!(drained.events.len(), 3);
    assert_eq!(drained.next_cursor, EventCursor(3));

    let caught_up = ring.events_since(drained.next_cursor, 512);
    assert!(caught_up.events.is_empty());
    assert_eq!(
        caught_up.next_cursor,
        EventCursor(3),
        "empty reads must keep next_cursor pinned at the newest assigned cursor"
    );
    assert_eq!(caught_up.oldest_available_cursor, EventCursor(1));
    assert_eq!(caught_up.gap, None);
}

#[test]
fn producer_push_never_blocks_under_burst() {
    let ring = DiagnosticsRing::new();

    let mut cursors = Vec::with_capacity(10_000);
    for index in 0..10_000 {
        cursors.push(ring.push(message_draft(&format!("burst-{index}"))));
    }

    assert_eq!(cursors.first(), Some(&EventCursor(1)));
    assert_eq!(cursors.last(), Some(&EventCursor(10_000)));
    let unique: BTreeSet<u64> = cursors.iter().map(|c| c.0).collect();
    assert_eq!(unique.len(), 10_000, "cursors must stay unique under burst");
    assert_eq!(*unique.iter().next().expect("non-empty set"), 1);
    assert_eq!(*unique.last().expect("non-empty set"), 10_000);

    let batch = ring.events_since(EventCursor(0), 512);
    assert_eq!(batch.events.len(), 512);
    assert_eq!(batch.events[0].cursor, EventCursor(5_905));
    assert_eq!(batch.events[511].cursor, EventCursor(6_416));
    assert_eq!(batch.oldest_available_cursor, EventCursor(5_905));
    assert_eq!(
        batch.gap,
        Some(EventGap {
            requested_cursor: EventCursor(0),
            resumed_at_cursor: EventCursor(5_905),
            overwritten_events: 5_905,
        })
    );

    let one_behind = ring.events_since(EventCursor(5_904), 512);
    assert_eq!(
        one_behind.gap,
        Some(EventGap {
            requested_cursor: EventCursor(5_904),
            resumed_at_cursor: EventCursor(5_905),
            overwritten_events: 1,
        })
    );
}

#[test]
fn error_code_variants_are_exhaustive_and_stable_count_19() {
    const ALL_CODES: [DiagnosticErrorCode; 19] = [
        DiagnosticErrorCode::DiscoveryFailure,
        DiagnosticErrorCode::PairingFailure,
        DiagnosticErrorCode::SetupRejected,
        DiagnosticErrorCode::SetupTimeout,
        DiagnosticErrorCode::EventChannelFailure,
        DiagnosticErrorCode::PtpBindFallback,
        DiagnosticErrorCode::PtpSampleStale,
        DiagnosticErrorCode::CaptureFailure,
        DiagnosticErrorCode::CaptureQueueFull,
        DiagnosticErrorCode::BufferUnderrun,
        DiagnosticErrorCode::EncodeFailure,
        DiagnosticErrorCode::SenderQueueDisconnected,
        DiagnosticErrorCode::UdpSendFailure,
        DiagnosticErrorCode::RetransmitHistoryMiss,
        DiagnosticErrorCode::FeedbackFailure,
        DiagnosticErrorCode::FeedbackTimeout,
        DiagnosticErrorCode::TeardownTimeout,
        DiagnosticErrorCode::CalibrationApplyFailure,
        DiagnosticErrorCode::ExportFailure,
    ];

    fn label(code: DiagnosticErrorCode) -> &'static str {
        match code {
            DiagnosticErrorCode::DiscoveryFailure => "discovery-failure",
            DiagnosticErrorCode::PairingFailure => "pairing-failure",
            DiagnosticErrorCode::SetupRejected => "setup-rejected",
            DiagnosticErrorCode::SetupTimeout => "setup-timeout",
            DiagnosticErrorCode::EventChannelFailure => "event-channel-failure",
            DiagnosticErrorCode::PtpBindFallback => "ptp-bind-fallback",
            DiagnosticErrorCode::PtpSampleStale => "ptp-sample-stale",
            DiagnosticErrorCode::CaptureFailure => "capture-failure",
            DiagnosticErrorCode::CaptureQueueFull => "capture-queue-full",
            DiagnosticErrorCode::BufferUnderrun => "buffer-underrun",
            DiagnosticErrorCode::EncodeFailure => "encode-failure",
            DiagnosticErrorCode::SenderQueueDisconnected => "sender-queue-disconnected",
            DiagnosticErrorCode::UdpSendFailure => "udp-send-failure",
            DiagnosticErrorCode::RetransmitHistoryMiss => "retransmit-history-miss",
            DiagnosticErrorCode::FeedbackFailure => "feedback-failure",
            DiagnosticErrorCode::FeedbackTimeout => "feedback-timeout",
            DiagnosticErrorCode::TeardownTimeout => "teardown-timeout",
            DiagnosticErrorCode::CalibrationApplyFailure => "calibration-apply-failure",
            DiagnosticErrorCode::ExportFailure => "export-failure",
            // Task 8 appended this neutral marker; it is not one of the 19
            // failure classes pinned above and never claims a failure the
            // wrapped payload does not state.
            DiagnosticErrorCode::Notice => "notice",
        }
    }

    assert_eq!(ALL_CODES.len(), 19);
    let labels: BTreeSet<&'static str> = ALL_CODES.iter().map(|code| label(*code)).collect();
    assert_eq!(labels.len(), 19, "every code must render a distinct label");

    // Exhaustive payload coverage without constructing foreign types.
    fn payload_label(payload: &StructuredDiagnosticPayload) -> &'static str {
        match payload {
            StructuredDiagnosticPayload::Backend(_) => "backend",
            StructuredDiagnosticPayload::Client(_) => "client",
            StructuredDiagnosticPayload::Error(_) => "error",
            StructuredDiagnosticPayload::DefiniteCounterTransition { .. } => "counter",
            StructuredDiagnosticPayload::CalibrationState(_) => "calibration",
            StructuredDiagnosticPayload::ExportState(_) => "export",
        }
    }

    let counter = StructuredDiagnosticPayload::DefiniteCounterTransition {
        counter: DefiniteCounterKind::BufferUnderruns,
        previous_total: 1,
        current_total: 2,
    };
    let calibration =
        StructuredDiagnosticPayload::CalibrationState(CalibrationApplyState::PendingRestart);
    let export = StructuredDiagnosticPayload::ExportState(ExportEventState::Completed);
    let error = DiagnosticError {
        code: DiagnosticErrorCode::CalibrationApplyFailure,
        component: DiagnosticComponent::Calibration,
        severity: DiagnosticSeverity::Error,
        recoverability: Recoverability::UserAction,
        operation: "apply_calibration",
        receiver_session: None,
        public_message: "profile rejected".to_owned(),
        technical_detail: None,
    };
    assert_eq!(payload_label(&counter), "counter");
    assert_eq!(payload_label(&calibration), "calibration");
    assert_eq!(payload_label(&export), "export");
    assert_eq!(
        payload_label(&StructuredDiagnosticPayload::Error(error)),
        "error"
    );
}

// ===========================================================================
// Task 8: registry, immutable snapshots, and conservative health
// ===========================================================================

mod task8_support {
    use super::*;

    /// Settable fake clock for deterministic registry tests.
    #[derive(Clone)]
    pub(crate) struct FakeClock {
        inner: Arc<Mutex<(SystemTime, u64)>>,
    }

    impl FakeClock {
        pub(crate) fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new((
                    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
                    5_000_000_000,
                ))),
            }
        }
    }

    impl DiagnosticsClock for FakeClock {
        fn now_utc(&self) -> SystemTime {
            self.inner.lock().expect("fake clock lock").0
        }

        fn elapsed_ns(&self) -> u64 {
            self.inner.lock().expect("fake clock lock").1
        }
    }

    pub(crate) struct RunningRegistry {
        pub(crate) tx: broadcast::Sender<BackendFeedEvent>,
        #[allow(dead_code)]
        pub(crate) registry: DiagnosticsRegistry,
        pub(crate) handle: DiagnosticsHandle,
    }

    /// Starts a registry on a private feed with a fake clock.
    pub(crate) fn start_registry(feed_capacity: usize) -> RunningRegistry {
        let (tx, rx) = DiagnosticsRegistry::test_diagnostic_feed(feed_capacity);
        let clock = FakeClock::new();
        let (registry, handle) = DiagnosticsRegistry::start(rx, Arc::new(clock.clone()));
        RunningRegistry {
            tx,
            registry,
            handle,
        }
    }

    /// Builds a backend feed event with explicit category.
    pub(crate) fn backend_event(
        category: DiagnosticCategory,
        receiver: Option<ReceiverId>,
        payload: DiagnosticPayload,
    ) -> BackendFeedEvent {
        BackendFeedEvent {
            monotonic_ns: 77,
            wall_time: SystemTime::UNIX_EPOCH + Duration::from_secs(9),
            session_generation: 1,
            discovery_generation: 2,
            capture_generation: 3,
            receiver,
            severity: Severity::Info,
            category,
            payload,
        }
    }

    pub(crate) fn registration(
        session_id: SessionId,
        primary: Option<ReceiverId>,
        members: &[(ReceiverId, DeviceId)],
    ) -> SessionRegistration {
        SessionRegistration {
            session_id,
            started_elapsed_ns: 123,
            primary,
            members: members
                .iter()
                .map(|(r, d)| (*r, d.clone()))
                .collect::<BTreeMap<_, _>>(),
            source: ClientDiagnosticsSource::test_new_empty(),
        }
    }

    /// Polls until `cond` holds; the test harness is the only sleeper here.
    pub(crate) fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if cond() {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Waits until sequence, dropped total, and ring head all stop moving so
    /// assertions read a quiesced registry rather than racing the consumer.
    pub(crate) fn wait_for_quiet(handle: &DiagnosticsHandle) {
        let mut previous = (0_u64, 0_u64, EventCursor(0));
        wait_until(
            || {
                let snapshot = handle.snapshot();
                let batch = handle.events_since(EventCursor(0), MAX_EVENT_READ_LIMIT);
                let current = (
                    snapshot.snapshot_sequence,
                    snapshot.diagnostics_events_dropped_total,
                    batch.next_cursor,
                );
                let quiet = current == previous;
                previous = current;
                quiet
            },
            "the registry consumer to go quiet",
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    /// Sums every QueueLag gap payload currently retained in the ring.
    pub(crate) fn retained_lag_dropped_total(handle: &DiagnosticsHandle) -> u64 {
        let mut cursor = EventCursor(0);
        let mut total = 0;
        loop {
            let batch = handle.events_since(cursor, MAX_EVENT_READ_LIMIT);
            if batch.events.is_empty() {
                return total;
            }
            for event in &batch.events {
                if let StructuredDiagnosticPayload::Backend(DiagnosticPayload::QueueLag {
                    dropped,
                }) = &event.payload
                {
                    total += dropped;
                }
            }
            cursor = batch.next_cursor;
        }
    }

    pub(crate) fn member_pair(seed: u8) -> (ReceiverId, DeviceId) {
        (
            receiver(seed),
            DeviceId([0x50 + seed, 0xAA, 0xBB, 0xCC, 0xDD, seed]),
        )
    }
}

use task8_support::{
    backend_event, member_pair, registration, retained_lag_dropped_total, start_registry,
    wait_for_quiet, wait_until, RunningRegistry,
};

#[test]
fn registry_start_returns_handle_with_schema_v1_snapshot() {
    let RunningRegistry { handle, .. } = start_registry(16);

    // The initial snapshot is published synchronously during `start` using
    // the injected clock's current reading.
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.schema_version, 1, "snapshot schema version is 1");
    assert_eq!(
        snapshot.captured_at_utc,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
    );
    assert_eq!(snapshot.process_elapsed_ns, 5_000_000_000);
    assert_eq!(snapshot.active_session_id, None);
    assert!(snapshot.session.is_none(), "no session is registered yet");
    assert!(snapshot.receivers.is_empty());
    assert_eq!(snapshot.diagnostics_events_dropped_total, 0);
    assert_eq!(snapshot.health, Health::Unknown);
    assert!(
        snapshot.snapshot_sequence >= 1,
        "the initial publication must carry sequence >= 1"
    );

    // A second read without any consumed change must be stable and cheap.
    let again = handle.snapshot();
    assert_eq!(again.snapshot_sequence, snapshot.snapshot_sequence);
    assert_eq!(again.active_session_id, None);

    // The shared ring starts empty but readable through the same handle.
    let batch = handle.events_since(EventCursor(0), 512);
    assert!(batch.events.is_empty());
    assert_eq!(batch.gap, None);
}

#[test]
fn register_then_finish_session_transitions_health_unknown_running_to_stopped_state() {
    let RunningRegistry {
        registry, handle, ..
    } = start_registry(16);

    assert_eq!(handle.snapshot().health, Health::Unknown);

    let session_id = SessionId::generate();
    let (a_rx, a_dev) = member_pair(1);
    let (b_rx, b_dev) = member_pair(2);
    registry.register_session(registration(
        session_id,
        Some(a_rx),
        &[(a_rx, a_dev), (b_rx, b_dev)],
    ));

    wait_until(
        || {
            let snapshot = handle.snapshot();
            snapshot.health == Health::Running && snapshot.active_session_id == Some(session_id)
        },
        "health to become Running after registration",
    );

    let active = handle.snapshot();
    let session = active.session.as_ref().expect("active session present");
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.stop_reason, None);
    assert_eq!(session.finished_elapsed_ns, None);
    assert_eq!(
        session.started_elapsed_ns, 123,
        "registration carries its truthful start stamp"
    );
    assert_eq!(session.members.len(), 2);
    assert_eq!(active.receivers.len(), 2, "one row per stable receiver id");
    assert!(
        active
            .receivers
            .windows(2)
            .all(|w| w[0].key.receiver_id < w[1].key.receiver_id),
        "receiver rows are sorted by stable ReceiverId"
    );
    for row in &active.receivers {
        assert_eq!(row.key.session_id, session_id);
    }

    registry.finish_session(session_id, SessionStopReason::Stopped);

    wait_until(
        || {
            let snapshot = handle.snapshot();
            // Spec health naming: no current session maps to Unknown even
            // though the stopped session stays retained for Speakers.
            snapshot.health == Health::Unknown && snapshot.active_session_id.is_none()
        },
        "finished session to map back to the stopped/no-current-session health",
    );

    let finished = handle.snapshot();
    let retained = finished.session.as_ref().expect("retained session present");
    assert_eq!(retained.stop_reason, Some(SessionStopReason::Stopped));
    assert!(retained.finished_elapsed_ns.is_some());
    assert_eq!(
        finished.receivers.len(),
        2,
        "stopped-session rows stay retained for Speakers"
    );
    assert_eq!(finished.diagnostics_events_dropped_total, 0);
}

#[test]
fn lagged_diagnostic_feed_counts_into_dropped_total_truthfully() {
    let running = start_registry(8);
    let RunningRegistry { tx, handle, .. } = running;

    for index in 0..64_u32 {
        tx.send(backend_event(
            DiagnosticCategory::General,
            None,
            DiagnosticPayload::Message(format!("lag-flood-{index}")),
        ))
        .expect("feed open during flood");
    }

    wait_until(
        || handle.snapshot().diagnostics_events_dropped_total > 0,
        "lagged feed losses to surface in diagnostics_events_dropped_total",
    );
    wait_for_quiet(&handle);

    let reported = handle.snapshot().diagnostics_events_dropped_total;
    let retained_sum = retained_lag_dropped_total(&handle);
    assert!(
        (1..=64).contains(&reported),
        "drop counter must reflect real losses, got {reported}"
    );
    assert_eq!(
        reported, retained_sum,
        "counter must equal exactly the dropped counts carried by the \
         synthetic QueueLag events it ingested"
    );
}

#[test]
fn snapshot_sequence_monotonically_increases_per_published_change() {
    let RunningRegistry {
        tx,
        registry,
        handle,
        ..
    } = start_registry(16);

    wait_for_quiet(&handle);
    let initial = handle.snapshot().snapshot_sequence;

    // No pending change: repeated reads keep the sequence pinned.
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(handle.snapshot().snapshot_sequence, initial);

    let session_id = SessionId::generate();
    let (a_rx, a_dev) = member_pair(1);
    registry.register_session(registration(session_id, Some(a_rx), &[(a_rx, a_dev)]));
    wait_until(
        || handle.snapshot().snapshot_sequence > initial,
        "registration to publish a new snapshot",
    );
    let after_register = handle.snapshot().snapshot_sequence;

    for index in 0..3_u32 {
        tx.send(backend_event(
            DiagnosticCategory::General,
            None,
            DiagnosticPayload::Message(format!("seq-{index}")),
        ))
        .unwrap();
    }
    wait_until(
        || handle.snapshot().snapshot_sequence > after_register,
        "ingested events to publish new snapshots",
    );
    let after_events = handle.snapshot().snapshot_sequence;

    registry.finish_session(session_id, SessionStopReason::Restarted);
    wait_until(
        || handle.snapshot().snapshot_sequence > after_events,
        "finish to publish a new snapshot",
    );

    let final_sequence = handle.snapshot().snapshot_sequence;
    assert!(initial < after_register);
    assert!(after_register < after_events);
    assert!(after_events < final_sequence);
}

#[test]
fn events_since_roundtrips_ingested_events_exactly_once() {
    let RunningRegistry {
        tx,
        registry: _registry,
        handle,
        ..
    } = start_registry(64);

    let sent: Vec<String> = (0..5).map(|index| format!("roundtrip-{index}")).collect();
    for message in &sent {
        tx.send(backend_event(
            DiagnosticCategory::General,
            None,
            DiagnosticPayload::Message(message.clone()),
        ))
        .unwrap();
    }

    wait_until(
        || {
            let batch = handle.events_since(EventCursor(0), MAX_EVENT_READ_LIMIT);
            batch.events.len() == sent.len()
        },
        "every fed event to be ingested into the shared ring",
    );
    wait_for_quiet(&handle);

    let full = handle.events_since(EventCursor(0), MAX_EVENT_READ_LIMIT);
    let messages: Vec<&str> = full
        .events
        .iter()
        .map(|event| event.public_message.as_str())
        .collect();
    let expected: Vec<&str> = sent.iter().map(String::as_str).collect();
    assert_eq!(messages, expected, "FIFO order preserved exactly once");

    let cursors: Vec<u64> = full.events.iter().map(|e| e.cursor.0).collect();
    assert!(cursors.windows(2).all(|w| w[0] < w[1]), "strictly ordered");

    // Clamped paged reads reconstruct exactly the same set without repeats.
    let mut paged = Vec::new();
    let mut cursor = EventCursor(0);
    loop {
        let batch = handle.events_since(cursor, 2);
        let newly: Vec<u64> = batch.events.iter().map(|e| e.cursor.0).collect();
        assert!(!newly.iter().any(|c| paged.contains(c)), "no duplicates");
        paged.extend(newly);
        cursor = batch.next_cursor;
        if batch.events.is_empty() {
            break;
        }
    }
    assert_eq!(paged, cursors);

    // Draining from the head returns nothing new and pins next_cursor.
    let caught_up = handle.events_since(full.next_cursor, MAX_EVENT_READ_LIMIT);
    assert!(caught_up.events.is_empty());
    assert_eq!(caught_up.next_cursor, full.next_cursor);
}

#[test]
fn health_definite_capture_drop_yields_attention_not_error() {
    let RunningRegistry {
        tx,
        registry,
        handle,
        ..
    } = start_registry(32);

    let session_id = SessionId::generate();
    let (a_rx, a_dev) = member_pair(1);
    registry.register_session(registration(session_id, Some(a_rx), &[(a_rx, a_dev)]));
    wait_until(
        || handle.snapshot().health == Health::Running,
        "clean active stream to report Running",
    );

    tx.send(backend_event(
        DiagnosticCategory::Capture,
        None,
        DiagnosticPayload::PcmDrop { dropped: 3 },
    ))
    .unwrap();

    wait_until(
        || handle.snapshot().health == Health::Attention,
        "a definite capture drop to degrade health to Attention",
    );

    // The transition was recorded as a structured definite-counter event.
    wait_until(
        || {
            handle
                .events_since(EventCursor(0), MAX_EVENT_READ_LIMIT)
                .events
                .iter()
                .any(|event| {
                    matches!(
                        event.payload,
                        StructuredDiagnosticPayload::DefiniteCounterTransition {
                            counter: DefiniteCounterKind::CaptureDrops,
                            previous_total: 0,
                            current_total: 3,
                        }
                    )
                })
        },
        "the capture-drop counter transition to be recorded truthfully",
    );

    let attention = handle.snapshot();
    assert_ne!(attention.health, Health::Error, "Attention is not Error");
    assert_eq!(attention.active_session_id, Some(session_id));

    // Informational quantities alone must never move health: an unrelated
    // info event keeps Attention and never escalates it.
    tx.send(backend_event(
        DiagnosticCategory::General,
        None,
        DiagnosticPayload::Message("informational".into()),
    ))
    .unwrap();
    wait_for_quiet(&handle);
    assert_eq!(handle.snapshot().health, Health::Attention);
}

#[test]
fn health_authoritative_receiver_failure_yields_error() {
    let RunningRegistry {
        tx,
        registry,
        handle,
        ..
    } = start_registry(32);

    let session_id = SessionId::generate();
    let (a_rx, a_dev) = member_pair(1);
    let (b_rx, b_dev) = member_pair(2);
    registry.register_session(registration(
        session_id,
        Some(a_rx),
        &[(a_rx, a_dev.clone()), (b_rx, b_dev)],
    ));
    wait_until(
        || handle.snapshot().health == Health::Running,
        "clean active stream to report Running",
    );

    tx.send(backend_event(
        DiagnosticCategory::Receiver,
        Some(b_rx),
        DiagnosticPayload::ReceiverTransition {
            receiver: b_rx,
            to: ReceiverLifecycle::Failed {
                retryable: false,
                error: UserFacingError::new("receiver failed"),
            },
        },
    ))
    .unwrap();

    wait_until(
        || handle.snapshot().health == Health::Error,
        "an authoritative failed receiver state to escalate health to Error",
    );

    let errored = handle.snapshot();
    let row = errored
        .receivers
        .iter()
        .find(|row| row.key.receiver_id == b_rx)
        .expect("failed member still has its stable row");
    assert!(matches!(row.lifecycle, ReceiverLifecycle::Failed { .. }));
    assert_eq!(errored.active_session_id, Some(session_id));

    registry.finish_session(session_id, SessionStopReason::Failed);
    wait_until(
        || handle.snapshot().health == Health::Unknown,
        "session end to fall back to Unknown",
    );
    assert_eq!(
        handle.snapshot().session.unwrap().stop_reason,
        Some(SessionStopReason::Failed)
    );
}
