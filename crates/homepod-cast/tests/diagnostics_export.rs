//! Contract tests for the versioned, support-safe diagnostics export.
//!
//! Covers Task 11 of the diagnostics/calibration plan: the allow-listed
//! export DTO, per-export receiver aliases, exact metric semantics, gap and
//! producer-drop disclosure, and the copy summary built from the same
//! redaction.
//!
//! The central test is [`no_hostile_token_survives_into_the_exported_bytes`]:
//! it feeds a deliberately hostile fixture -- names, MACs, IPv4/IPv6, ports,
//! serials, group/pairing/PTP/SSRC/RTSP identities, a Windows user path, an
//! authorization header, a key, a nonce, a tag, and packet hex -- through
//! every string-carrying field the domain has, then scans the produced bytes
//! for each of them. It is modelled on
//! `device_resilience::no_diagnostic_carries_a_secret_or_an_address`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use airplay_audio::diagnostics::{
    AudioDiagnosticsSnapshot, CaptureQueueSnapshot, RetransmitSnapshot, SchedulerJitterSnapshot,
    SenderBufferSnapshot, TargetTransportSnapshot,
};
use airplay_client::{
    ClientDiagnosticKind, ClientDiagnosticsSource, DeviceId, FeedbackResultKind, FeedbackSnapshot,
};
use homepod_cast::backend::{DiagnosticPayload, SystemTransition};
use homepod_cast::calibration::EffectiveCalibration;
use homepod_cast::diagnostics::{
    build_support_export, build_support_summary, CalibrationApplyState, DefiniteCounterKind,
    DiagnosticComponent, DiagnosticError, DiagnosticErrorCode, DiagnosticEvent, DiagnosticSeverity,
    DiagnosticsSnapshot, EventCursor, EventGap, ExportEventState, Health,
    ReceiverDiagnosticsSnapshot, ReceiverSessionKey, ReceiverTimingSnapshot, ReceiverTimingSource,
    ReceiverTransportSnapshot, Recoverability, SessionDiagnosticsSnapshot, SessionId,
    StructuredDiagnosticPayload, SupportAudioConfiguration, SupportCalibrationContext,
    SupportExportContext, SupportExportInput, DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
    SUPPORT_EXPORT_MEDIA_TYPE, SUPPORT_EXPORT_SCHEMA, SUPPORT_EXPORT_SCHEMA_VERSION,
};
use homepod_cast::{
    AudioSourceState, DiscoveryPhase, PersistenceSnapshot, ReceiverId, ReceiverLifecycle,
    ReceiverRole, RestartReason, SessionPhase, SetupPhase, UserFacingError,
};

// ===========================================================================
// The hostile fixture
// ===========================================================================

/// Every token below is planted somewhere in the fixture and must not appear
/// in the exported document, in any casing.
const HOSTILE_TOKENS: &[&str] = &[
    "Lisas Schlafzimmer",
    "Thorsten",
    "5855CA1AE288",
    "58:55:CA:1A:E2:88",
    "192.168.178.42",
    "10.0.0.7",
    "fe80::1c2d:3e4f:5a6b:7c8d",
    "C:\\Users\\Thorsten\\AppData\\Roaming\\OpenAirCast\\state-v1.json",
    "FRITZ!Box 7590 Gastnetz",
    "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9",
    "pin=8421",
    "shk=0f1e2d3c4b5a69788796a5b4c3d2e1f0",
    "SERIAL-F9X2K7QP",
    "group-uuid-1f6b0a3c",
    "clock-id 5855ca.fffe.1ae288",
    "ssrc=0xDEADBEEF",
    "rtsp://192.168.178.42:7000/9876543210",
    "nonce=00000000000000000000000b",
    "tag=9f8e7d6c5b4a39281706f5e4d3c2b1a0",
    "41 4c 41 43 00 00 01 60",
    "\\Device\\HarddiskVolume4\\Render\\{0.0.0.00000000}",
];

/// JSON keys the export must never contain, either because they carry
/// identity or because they would claim something the sender cannot measure.
const FORBIDDEN_KEYS: &[&str] = &[
    "\"loss_percent\"",
    "\"receiver_buffer\"",
    "\"network_jitter\"",
    "\"ptp_rtt\"",
    "\"clock_accuracy\"",
    "\"acoustic_latency\"",
    "\"packet_loss\"",
    "\"device_id\"",
    "\"receiver_id\"",
    "\"session_id\"",
    "\"mac\"",
    "\"ip\"",
    "\"address\"",
    "\"hostname\"",
    "\"port\"",
    "\"path\"",
    "\"name\"",
    "\"ssrc\"",
    "\"serial\"",
    "\"endpoint\"",
];

fn hostile(index: usize) -> String {
    HOSTILE_TOKENS[index].to_owned()
}

/// `5855CA1AE288` -- the exact MAC the token list scans for.
fn identified_receiver() -> ReceiverId {
    ReceiverId::from_storage_key("5855CA1AE288").expect("fixed storage key is valid")
}

fn other_receiver() -> ReceiverId {
    ReceiverId::from_storage_key("0011223344AA").expect("fixed storage key is valid")
}

/// A receiver that only ever shows up inside an event payload, never in the
/// snapshot -- the alias table has to reach it anyway.
fn payload_only_receiver() -> ReceiverId {
    ReceiverId::from_storage_key("FFEEDDCCBB99").expect("fixed storage key is valid")
}

fn device(seed: u8) -> DeviceId {
    DeviceId([0x58, 0x55, 0xCA, 0x1A, 0xE2, seed])
}

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn hostile_error(key: ReceiverSessionKey) -> DiagnosticError {
    DiagnosticError {
        code: DiagnosticErrorCode::PairingFailure,
        component: DiagnosticComponent::Pairing,
        severity: DiagnosticSeverity::Error,
        recoverability: Recoverability::UserAction,
        operation: "pair_verify_m3",
        receiver_session: Some(key),
        // Nominally "pre-redacted"; the export must not trust that claim.
        public_message: format!("{} rejected {}", hostile(0), hostile(11)),
        technical_detail: Some(format!(
            "{} / {} / {}",
            hostile(9),
            hostile(12),
            hostile(19)
        )),
    }
}

fn hostile_audio() -> AudioDiagnosticsSnapshot {
    AudioDiagnosticsSnapshot {
        sender_buffer: SenderBufferSnapshot {
            queued_frames: 11,
            capacity_frames: 64,
            queued_samples_per_channel: 3_872,
            buffered_ns: 87_800_453,
            fill_ratio: 0.171_875,
            samples_written_total: 441_000,
            samples_read_total: 437_128,
            underrun_events_total: 3,
        },
        capture_queue: CaptureQueueSnapshot {
            queued_frames: 2,
            capacity_frames: 32,
            submitted_frames_total: 1_253,
            submitted_samples_per_channel_total: 441_056,
            full_queue_drops_total: 7,
            disconnected_drops_total: 0,
        },
        scheduler: SchedulerJitterSnapshot {
            sample_count: 1_250,
            last_signed_jitter_ns: Some(-412_000),
            mean_absolute_jitter_ns: Some(311_000),
            p95_absolute_jitter_ns: Some(902_000),
            maximum_absolute_jitter_ns: Some(4_110_000),
            deadline_resets_total: 1,
        },
        rtp_frames_prepared_total: 1_250,
        targets: vec![TargetTransportSnapshot {
            data_datagrams_attempted_total: 1_250,
            data_datagrams_accepted_local_total: 1_248,
            data_send_failures_total: 2,
            data_bytes_sent_total: 1_800_000,
            sync_datagrams_attempted_total: 42,
            sync_datagrams_accepted_local_total: 42,
            sync_send_failures_total: 0,
            sync_bytes_sent_total: 1_176,
        }],
        retransmit: RetransmitSnapshot {
            retransmit_request_datagrams_total: 9,
            retransmit_packet_slots_requested_total: 21,
            retransmit_datagrams_accepted_local_total: 18,
            retransmit_history_misses_total: 3,
            retransmit_send_failures_total: 0,
        },
    }
}

fn hostile_snapshot(session_id: SessionId) -> DiagnosticsSnapshot {
    let identified = identified_receiver();
    let other = other_receiver();

    let members: BTreeMap<ReceiverId, DeviceId> =
        [(identified, device(0x88)), (other, device(0xAA))]
            .into_iter()
            .collect();

    let rows = vec![
        ReceiverDiagnosticsSnapshot {
            key: ReceiverSessionKey {
                session_id,
                receiver_id: identified,
            },
            lifecycle: ReceiverLifecycle::Failed {
                retryable: false,
                // The backend calls this "pre-redacted". It is not.
                error: UserFacingError::new(format!("{} at {}", hostile(0), hostile(4))),
            },
            timing: ReceiverTimingSnapshot {
                source: ReceiverTimingSource::PtpMeasuredAgainstThisMaster,
                independently_measured: true,
                offset_local_minus_master_ns: Some(-183_412),
                mean_path_delay_ns: Some(412_889),
                drift_ppm: Some(-2.75),
                sample_age_ns: Some(96_000_000),
                stale: false,
            },
            transport: ReceiverTransportSnapshot {
                data_datagrams_attempted_total: 1_250,
                data_datagrams_accepted_local_total: 1_248,
                data_datagram_send_failures_total: 2,
                data_bytes_accepted_local_total: 1_800_000,
                sync_datagrams_attempted_total: 42,
                sync_datagrams_accepted_local_total: 42,
                sync_datagram_send_failures_total: 0,
                sync_bytes_accepted_local_total: 1_176,
                retransmit_slots_requested_total: 21,
                retransmit_slots_accepted_local_total: 18,
                recovery_demand_numerator_slots: 21,
                recovery_demand_denominator_shared_frames: 1_250,
                recovery_demand_ratio: Some(0.016_8),
            },
            feedback: FeedbackSnapshot {
                attempts_total: 30,
                successes_total: 28,
                protocol_failures_total: 1,
                transport_failures_total: 0,
                timeouts_total: 1,
                last_result: Some(FeedbackResultKind::Timeout),
                last_transaction_duration_ns: Some(2_000_000_000),
                last_success_elapsed_ns: Some(58_000_000_000),
            },
            last_error: Some(hostile_error(ReceiverSessionKey {
                session_id,
                receiver_id: identified,
            })),
        },
        ReceiverDiagnosticsSnapshot {
            key: ReceiverSessionKey {
                session_id,
                receiver_id: other,
            },
            lifecycle: ReceiverLifecycle::Streaming {
                role: ReceiverRole::Secondary,
            },
            timing: ReceiverTimingSnapshot {
                source: ReceiverTimingSource::PtpSharedFromPrimary {
                    source_receiver_id: Some(identified),
                },
                independently_measured: false,
                offset_local_minus_master_ns: None,
                mean_path_delay_ns: None,
                drift_ppm: None,
                sample_age_ns: None,
                stale: false,
            },
            transport: ReceiverTransportSnapshot::default(),
            feedback: FeedbackSnapshot::default(),
            last_error: None,
        },
    ];

    DiagnosticsSnapshot {
        schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
        snapshot_sequence: 4_211,
        captured_at_utc: at(1_774_000_000),
        process_elapsed_ns: 61_500_000_000,
        active_session_id: Some(session_id),
        health: Health::Attention,
        session: Some(SessionDiagnosticsSnapshot {
            session_id,
            started_elapsed_ns: 1_500_000_000,
            finished_elapsed_ns: None,
            stop_reason: None,
            phase: Some(SessionPhase::Failed {
                generation: 7,
                error: UserFacingError::new(hostile(7)),
            }),
            primary: Some(identified),
            members,
            audio: Some(hostile_audio()),
        }),
        receivers: rows,
        diagnostics_events_dropped_total: 12,
    }
}

fn client_event(kind: ClientDiagnosticKind, device_id: DeviceId) -> StructuredDiagnosticPayload {
    let source = ClientDiagnosticsSource::test_new_empty();
    source.test_register_connection(device_id.clone(), Some(0));
    source.test_record_event(Some(device_id), kind);
    let mut drained = source.test_drain_events(4);
    assert_eq!(drained.len(), 1, "the test seam records exactly one event");
    StructuredDiagnosticPayload::Client(drained.remove(0))
}

fn hostile_events(session_id: SessionId) -> Vec<DiagnosticEvent> {
    let identified = identified_receiver();
    let key = ReceiverSessionKey {
        session_id,
        receiver_id: identified,
    };

    let payloads: Vec<StructuredDiagnosticPayload> = vec![
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::Message(format!(
            "{} :: {} :: {}",
            hostile(3),
            hostile(6),
            hostile(20)
        ))),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::CommandResult {
            id: 91,
            error: Some(UserFacingError::new(hostile(16))),
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::DiscoveryReceiver {
            // The only place this receiver ever appears.
            receiver: payload_only_receiver(),
            present: true,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::DiscoveryGeneration {
            generation: 3,
            phase: DiscoveryPhase::Running,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::ReceiverTransition {
            receiver: identified,
            to: ReceiverLifecycle::Failed {
                retryable: true,
                error: UserFacingError::new(hostile(13)),
            },
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::QueueLag { dropped: 12 }),
        StructuredDiagnosticPayload::Error(hostile_error(key.clone())),
        StructuredDiagnosticPayload::DefiniteCounterTransition {
            counter: DefiniteCounterKind::UdpSendFailures,
            previous_total: 0,
            current_total: 2,
        },
        StructuredDiagnosticPayload::CalibrationState(CalibrationApplyState::PendingRestart),
        StructuredDiagnosticPayload::ExportState(ExportEventState::Started),
        client_event(ClientDiagnosticKind::SetupFailure, device(0x88)),
    ];

    payloads
        .into_iter()
        .enumerate()
        .map(|(index, payload)| DiagnosticEvent {
            cursor: EventCursor(u64::try_from(index).expect("small index") + 41),
            occurred_at_utc: at(1_774_000_000 + u64::try_from(index).expect("small index")),
            process_elapsed_ns: 60_000_000_000 + u64::try_from(index).expect("small index"),
            receiver_session: Some(key.clone()),
            severity: DiagnosticSeverity::Warning,
            component: DiagnosticComponent::UdpTransport,
            code: DiagnosticErrorCode::UdpSendFailure,
            // Free-form and hostile on purpose.
            public_message: format!("{} -> {}", hostile(2), hostile(15)),
            payload,
        })
        .collect()
}

fn hostile_input() -> SupportExportInput {
    let session_id = SessionId::generate();
    SupportExportInput {
        latest_snapshot: hostile_snapshot(session_id),
        retained_events: hostile_events(session_id),
        event_gap: Some(EventGap {
            requested_cursor: EventCursor(5),
            resumed_at_cursor: EventCursor(41),
            overwritten_events: 36,
        }),
        producer_drops_total: 12,
    }
}

fn context() -> SupportExportContext {
    SupportExportContext {
        exported_at_utc: at(1_774_000_600),
        application_version: "0.1.0",
        build_profile: "test",
        platform_os: "windows",
        platform_arch: "x86_64",
        windows_build: Some(19_045),
        audio: SupportAudioConfiguration {
            sample_rate_hz: 44_100,
            channels: 2,
            frame_samples_per_channel: 352,
            capture_queue_capacity_frames: 32,
        },
        calibration: Some(SupportCalibrationContext {
            reference_receiver: Some(identified_receiver()),
            effective: EffectiveCalibration {
                minimum_requested_ns: -500_000,
                effective_delay_ns: [(identified_receiver(), 0), (other_receiver(), 700_000)]
                    .into_iter()
                    .collect(),
            },
            apply_state: Some(CalibrationApplyState::Applied),
        }),
    }
}

fn export_text(input: &SupportExportInput, context: &SupportExportContext) -> String {
    let artifact = build_support_export(input, context).expect("the hostile fixture must export");
    assert_eq!(artifact.media_type, SUPPORT_EXPORT_MEDIA_TYPE);
    String::from_utf8(artifact.bytes).expect("the export is UTF-8 JSON")
}

fn export_json(input: &SupportExportInput, context: &SupportExportContext) -> serde_json::Value {
    serde_json::from_str(&export_text(input, context)).expect("the export parses as JSON")
}

// ===========================================================================
// Redaction
// ===========================================================================

#[test]
fn no_hostile_token_survives_into_the_exported_bytes() {
    let input = hostile_input();
    let text = export_text(&input, &context());
    let lowered = text.to_lowercase();

    for token in HOSTILE_TOKENS {
        assert!(
            !lowered.contains(&token.to_lowercase()),
            "the export carried {token:?}"
        );
    }

    // The MAC bytes must not survive in any of their usual renderings either.
    for form in [
        "5855ca1ae288",
        "58-55-ca-1a-e2-88",
        "58:55:ca:1a:e2:88",
        "0011223344aa",
        "ffeeddccbb99",
    ] {
        assert!(
            !lowered.contains(form),
            "the export carried the MAC {form:?}"
        );
    }
}

#[test]
fn the_copy_summary_carries_no_hostile_token_either() {
    let input = hostile_input();
    let summary = build_support_summary(&input);
    let lowered = summary.to_lowercase();

    assert!(!summary.is_empty(), "the summary must say something");
    for token in HOSTILE_TOKENS {
        assert!(
            !lowered.contains(&token.to_lowercase()),
            "the copy summary carried {token:?}"
        );
    }
    for form in ["5855ca1ae288", "0011223344aa", "ffeeddccbb99"] {
        assert!(
            !lowered.contains(form),
            "the copy summary carried the MAC {form:?}"
        );
    }
}

#[test]
fn the_export_never_uses_a_prohibited_key_name() {
    let input = hostile_input();
    let text = export_text(&input, &context());

    for key in FORBIDDEN_KEYS {
        assert!(
            !text.contains(key),
            "the export used the forbidden key {key}"
        );
    }
}

#[test]
fn the_export_declares_that_it_has_no_unredacted_mode() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    let redaction = &json["redaction"];
    assert_eq!(redaction["policy"], "allow_list");
    assert_eq!(redaction["unredacted_mode_available"], false);
    assert_eq!(redaction["receiver_identity"], "per_export_alias");
    let omitted = redaction["omitted"]
        .as_array()
        .expect("the omitted classes are a list");
    for class in [
        "receiver_and_device_names",
        "receiver_and_device_identifiers",
        "network_addresses_and_ports",
        "file_system_paths_and_user_names",
        "free_form_error_and_protocol_text",
        "pairing_secrets_and_keys",
        "audio_payload_and_packet_bytes",
    ] {
        assert!(
            omitted.iter().any(|entry| entry == class),
            "the redaction descriptor does not name {class}"
        );
    }
}

// ===========================================================================
// Schema
// ===========================================================================

#[test]
fn the_export_names_its_schema_version_and_media_type() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    assert_eq!(
        SUPPORT_EXPORT_SCHEMA,
        "openaircast.diagnostics.support-export"
    );
    assert_eq!(SUPPORT_EXPORT_SCHEMA_VERSION, 1);
    assert_eq!(
        SUPPORT_EXPORT_MEDIA_TYPE,
        "application/vnd.openaircast.diagnostics+json"
    );

    assert_eq!(json["schema"], SUPPORT_EXPORT_SCHEMA);
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["exported_at_utc"], "2026-03-20T09:56:40.000000000Z");
    assert_eq!(
        json["latest_snapshot"]["captured_at_utc"],
        "2026-03-20T09:46:40.000000000Z"
    );
    assert_eq!(json["latest_snapshot"]["snapshot_schema_version"], 1);
    assert_eq!(json["latest_snapshot"]["snapshot_sequence"], 4_211);
    assert_eq!(json["application"]["product"], "OpenAirCast");
    assert_eq!(json["application"]["version"], "0.1.0");
    assert_eq!(json["application"]["platform_os"], "windows");
    assert_eq!(json["application"]["windows_build"], 19_045);
}

#[test]
fn wall_clock_stamps_are_rendered_as_zero_padded_utc() {
    let session_id = SessionId::generate();
    let mut snapshot = hostile_snapshot(session_id);
    snapshot.captured_at_utc = SystemTime::UNIX_EPOCH;
    let input = SupportExportInput {
        latest_snapshot: snapshot,
        retained_events: Vec::new(),
        event_gap: None,
        producer_drops_total: 0,
    };
    let mut context = context();
    // Windows `SystemTime` ticks in 100 ns steps, so the sub-second part is a
    // multiple of 100; the point here is the zero padding, not the resolution.
    context.exported_at_utc = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_000_000_100);

    let json = export_json(&input, &context);
    assert_eq!(
        json["latest_snapshot"]["captured_at_utc"],
        "1970-01-01T00:00:00.000000000Z"
    );
    assert_eq!(json["exported_at_utc"], "1970-01-01T00:00:01.000000100Z");
}

// ===========================================================================
// Aliases
// ===========================================================================

#[test]
fn one_receiver_keeps_one_alias_for_the_whole_export() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    // Sorted ReceiverIds: 0011223344AA < 5855CA1AE288 < FFEEDDCCBB99.
    let rows = json["latest_snapshot"]["receivers"]
        .as_array()
        .expect("receiver rows are a list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["receiver_alias"], "R-001");
    assert_eq!(rows[1]["receiver_alias"], "R-002");

    // The primary is the second-sorted receiver, and every reference to it --
    // session primary, shared-timing source, calibration -- uses that one
    // alias.
    let session = &json["latest_snapshot"]["session"];
    assert_eq!(session["primary_receiver_alias"], "R-002");
    assert_eq!(rows[0]["timing"]["shared_from_receiver_alias"], "R-002");
    assert_eq!(
        json["configuration"]["calibration"]["reference_receiver_alias"],
        "R-002"
    );
    assert_eq!(
        session["member_receiver_aliases"]
            .as_array()
            .expect("members are a list"),
        &vec![
            serde_json::Value::from("R-001"),
            serde_json::Value::from("R-002")
        ]
    );

    // A receiver that only occurs inside one event payload still gets a
    // numbered alias -- the alias walk reaches payloads, not just rows.
    let text = export_text(&input, &context());
    assert!(
        text.contains("R-003"),
        "the payload-only receiver never got an alias: {text}"
    );
    assert!(
        !text.contains("R-unknown"),
        "some receiver reached the export without an alias: {text}"
    );
}

#[test]
fn each_export_builds_its_own_alias_table() {
    // An export whose only receiver is the one that sorted last above must
    // start counting at R-001 again instead of inheriting R-003.
    let session_id = SessionId::generate();
    let key = ReceiverSessionKey {
        session_id,
        receiver_id: payload_only_receiver(),
    };
    let snapshot = DiagnosticsSnapshot {
        schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
        snapshot_sequence: 1,
        captured_at_utc: at(1_774_000_000),
        process_elapsed_ns: 1,
        active_session_id: Some(session_id),
        health: Health::Running,
        session: Some(SessionDiagnosticsSnapshot {
            session_id,
            started_elapsed_ns: 0,
            finished_elapsed_ns: None,
            stop_reason: None,
            phase: Some(SessionPhase::Streaming { generation: 1 }),
            primary: None,
            members: [(payload_only_receiver(), device(0x99))]
                .into_iter()
                .collect(),
            audio: None,
        }),
        receivers: vec![ReceiverDiagnosticsSnapshot {
            key,
            lifecycle: ReceiverLifecycle::Streaming {
                role: ReceiverRole::Single,
            },
            timing: ReceiverTimingSnapshot {
                source: ReceiverTimingSource::SenderReference,
                independently_measured: false,
                offset_local_minus_master_ns: None,
                mean_path_delay_ns: None,
                drift_ppm: None,
                sample_age_ns: None,
                stale: false,
            },
            transport: ReceiverTransportSnapshot::default(),
            feedback: FeedbackSnapshot::default(),
            last_error: None,
        }],
        diagnostics_events_dropped_total: 0,
    };
    let input = SupportExportInput {
        latest_snapshot: snapshot,
        retained_events: Vec::new(),
        event_gap: None,
        producer_drops_total: 0,
    };

    let mut context = context();
    context.calibration = None;
    let json = export_json(&input, &context);
    assert_eq!(
        json["latest_snapshot"]["receivers"][0]["receiver_alias"],
        "R-001"
    );
}

#[test]
fn repeated_exports_of_the_same_input_are_byte_identical() {
    let input = hostile_input();
    let first = export_text(&input, &context());
    let second = export_text(&input, &context());
    assert_eq!(first, second, "the export must be deterministic");
}

// ===========================================================================
// Exact semantics
// ===========================================================================

#[test]
fn metrics_keep_their_exact_unit_and_semantics_names() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    let primary = &json["latest_snapshot"]["receivers"][1];
    assert_eq!(
        primary["timing"]["source"],
        "ptp_measured_against_this_master"
    );
    assert_eq!(primary["timing"]["offset_local_minus_master_ns"], -183_412);
    assert_eq!(primary["timing"]["mean_path_delay_ns"], 412_889);
    assert_eq!(
        primary["timing"]["estimated_relative_clock_drift_ppm"],
        -2.75
    );
    assert_eq!(primary["timing"]["sample_age_ns"], 96_000_000);

    let transport = &primary["transport"];
    assert_eq!(transport["data_datagrams_accepted_local_total"], 1_248);
    assert_eq!(transport["data_datagram_send_failures_total"], 2);
    assert_eq!(transport["recovery_demand_numerator_slots"], 21);
    assert_eq!(
        transport["recovery_demand_denominator_shared_frames"],
        1_250
    );
    assert_eq!(
        transport["local_os_udp_acceptance"],
        "accepted_local counts datagrams the local operating system accepted; \
         it is not receiver delivery and not packet loss"
    );

    let scheduler = &json["latest_snapshot"]["session"]["audio"]["scheduler"];
    assert_eq!(
        scheduler["mean_absolute_scheduler_dispatch_jitter_ns"],
        311_000
    );
    assert_eq!(
        scheduler["last_signed_scheduler_dispatch_jitter_ns"],
        -412_000
    );

    let configuration = &json["configuration"];
    assert_eq!(configuration["sample_rate_hz"], 44_100);
    assert_eq!(configuration["frame_samples_per_channel"], 352);
    assert_eq!(configuration["capture_queue_capacity_frames"], 32);
    assert_eq!(configuration["requested_latency_not_measured"], true);
    assert_eq!(configuration["event_ring_capacity"], 4_096);
}

#[test]
fn an_unavailable_measurement_stays_null_and_is_never_zeroed() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    let secondary = &json["latest_snapshot"]["receivers"][0];
    assert_eq!(secondary["timing"]["source"], "ptp_shared_from_primary");
    assert_eq!(secondary["timing"]["independently_measured"], false);
    assert!(secondary["timing"]["offset_local_minus_master_ns"].is_null());
    assert!(secondary["timing"]["mean_path_delay_ns"].is_null());
    assert!(secondary["timing"]["estimated_relative_clock_drift_ppm"].is_null());
    assert!(secondary["timing"]["sample_age_ns"].is_null());
}

/// A NaN or infinity must read as "unavailable", not as zero and not as a
/// failed export. `serde_json` writes a non-finite float as `null`, which is
/// exactly what the diagnostics domain means by unavailable; this test pins
/// that down so a serializer change cannot turn a garbage fit into a
/// plausible-looking number.
#[test]
fn a_non_finite_measurement_exports_as_unavailable_instead_of_failing() {
    let session_id = SessionId::generate();
    let mut snapshot = hostile_snapshot(session_id);
    snapshot.receivers[0].timing.drift_ppm = Some(f64::NAN);
    snapshot.receivers[0].timing.offset_local_minus_master_ns = Some(7);
    snapshot.receivers[1].transport.recovery_demand_ratio = Some(f64::INFINITY);
    let input = SupportExportInput {
        latest_snapshot: snapshot,
        retained_events: Vec::new(),
        event_gap: None,
        producer_drops_total: 0,
    };

    let json = export_json(&input, &context());
    let rows = json["latest_snapshot"]["receivers"]
        .as_array()
        .expect("rows are a list");
    // Rows are ordered by alias, so the fixture's first row (the identified
    // receiver, `5855CA1AE288`) sorts second.
    assert!(rows[1]["timing"]["estimated_relative_clock_drift_ppm"].is_null());
    assert_eq!(rows[1]["timing"]["offset_local_minus_master_ns"], 7);
    assert!(rows[0]["transport"]["recovery_demand_ratio"].is_null());
}

#[test]
fn calibration_effective_delays_are_exported_by_alias() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    let calibration = &json["configuration"]["calibration"];
    assert_eq!(calibration["apply_state"], "applied");
    assert_eq!(calibration["minimum_requested_ns"], -500_000);
    assert_eq!(
        calibration["effective_delay_ns_by_receiver"]["R-001"],
        700_000
    );
    assert_eq!(calibration["effective_delay_ns_by_receiver"]["R-002"], 0);
    assert_eq!(calibration["entries_omitted_for_unknown_receivers"], 0);
}

#[test]
fn a_calibration_entry_for_an_unobserved_receiver_is_dropped_and_counted() {
    let input = hostile_input();
    let mut context = context();
    let stray = ReceiverId::from_storage_key("AAAAAAAAAAAA").expect("valid key");
    context
        .calibration
        .as_mut()
        .expect("the fixture carries calibration")
        .effective
        .effective_delay_ns
        .insert(stray, 1_234_000);

    let json = export_json(&input, &context);
    let calibration = &json["configuration"]["calibration"];
    assert_eq!(calibration["entries_omitted_for_unknown_receivers"], 1);
    assert_eq!(
        calibration["effective_delay_ns_by_receiver"]
            .as_object()
            .expect("delays are an object")
            .len(),
        2
    );
    assert!(!export_text(&input, &context)
        .to_lowercase()
        .contains("aaaaaaaaaaaa"));
}

// ===========================================================================
// Events, gaps, drops
// ===========================================================================

#[test]
fn events_carry_stable_codes_and_typed_payload_kinds_only() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    let events = json["events"].as_array().expect("events are a list");
    assert_eq!(events.len(), 11, "every retained event is exported");
    assert_eq!(events[0]["cursor"], 41);
    assert_eq!(events[0]["severity"], "warning");
    assert_eq!(events[0]["component"], "udp_transport");
    assert_eq!(events[0]["code"], "udp_send_failure");
    assert_eq!(events[0]["receiver_alias"], "R-002");
    assert_eq!(
        events[0]["occurred_at_utc"],
        "2026-03-20T09:46:40.000000000Z"
    );
    assert!(
        events[0].get("public_message").is_none(),
        "free-form event text must not be exported at all"
    );

    let kinds: Vec<&str> = events
        .iter()
        .map(|event| {
            event["payload"]["kind"]
                .as_str()
                .expect("every payload names its kind")
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "backend_message",
            "command_result",
            "discovery_receiver",
            "discovery_generation",
            "receiver_transition",
            "diagnostic_feed_lag",
            "error",
            "definite_counter_transition",
            "calibration_state",
            "export_state",
            "client_event",
        ]
    );

    // A free-form payload is reported as withheld, never guessed at.
    assert_eq!(events[0]["payload"]["free_form_text_omitted"], true);
    assert_eq!(events[1]["payload"]["failed"], true);
    assert_eq!(events[1]["payload"]["free_form_text_omitted"], true);
    assert_eq!(events[2]["payload"]["receiver_alias"], "R-003");
    assert_eq!(events[4]["payload"]["lifecycle"]["state"], "failed");
    assert_eq!(events[4]["payload"]["lifecycle"]["retryable"], true);
    assert_eq!(events[5]["payload"]["dropped_events"], 12);
    assert_eq!(events[6]["payload"]["error"]["code"], "pairing_failure");
    assert_eq!(events[6]["payload"]["error"]["operation"], "pair_verify_m3");
    assert!(
        events[6]["payload"]["error"]
            .get("technical_detail")
            .is_none(),
        "raw technical detail has no field to live in"
    );
    assert_eq!(events[7]["payload"]["counter"], "udp_send_failures");
    assert_eq!(events[8]["payload"]["state"], "pending_restart");
    assert_eq!(events[9]["payload"]["state"], "started");
    assert_eq!(events[10]["payload"]["client_kind"], "setup_failure");
}

#[test]
fn the_export_discloses_overwritten_events_and_producer_drops() {
    let input = hostile_input();
    let json = export_json(&input, &context());

    assert_eq!(json["event_gap"]["requested_cursor"], 5);
    assert_eq!(json["event_gap"]["resumed_at_cursor"], 41);
    assert_eq!(json["event_gap"]["overwritten_events"], 36);
    assert_eq!(json["producer_drops_total"], 12);
    assert_eq!(json["events_retained"], 11);
    assert_eq!(
        json["latest_snapshot"]["diagnostics_events_dropped_total"],
        12
    );
}

#[test]
fn an_export_without_a_gap_says_so_explicitly() {
    let mut input = hostile_input();
    input.event_gap = None;
    input.producer_drops_total = 0;

    let json = export_json(&input, &context());
    assert!(json["event_gap"].is_null());
    assert_eq!(json["producer_drops_total"], 0);
}

// ===========================================================================
// Copy summary
// ===========================================================================

#[test]
fn the_copy_summary_repeats_the_export_facts_in_plain_text() {
    let input = hostile_input();
    let summary = build_support_summary(&input);

    for expected in [
        "OpenAirCast diagnostics summary",
        "schema openaircast.diagnostics.support-export v1",
        "health: attention",
        "R-001",
        "R-002",
        "events retained: 11",
        "events overwritten before this read: 36",
        "diagnostic events dropped by producer lag: 12",
        "no names, addresses, paths, identifiers, or keys are included",
    ] {
        assert!(
            summary.contains(expected),
            "the summary is missing {expected:?}:\n{summary}"
        );
    }
}

#[test]
fn the_copy_summary_agrees_with_the_exported_aliases() {
    let input = hostile_input();
    let summary = build_support_summary(&input);
    let json = export_json(&input, &context());

    let rows = json["latest_snapshot"]["receivers"]
        .as_array()
        .expect("rows are a list");
    for row in rows {
        let alias = row["receiver_alias"].as_str().expect("alias is a string");
        assert!(
            summary.contains(alias),
            "the summary omits {alias}:\n{summary}"
        );
    }
    assert!(!summary.contains("R-004"), "the summary invented an alias");
}

// ===========================================================================
// Registry seam
// ===========================================================================

#[test]
fn the_registry_hands_out_a_consistent_export_input() {
    use std::sync::Arc;

    use homepod_cast::diagnostics::{
        DiagnosticsClock, DiagnosticsRegistry, SessionRegistration, SessionStopReason,
    };

    struct FixedClock;
    impl DiagnosticsClock for FixedClock {
        fn now_utc(&self) -> SystemTime {
            at(1_774_000_000)
        }
        fn elapsed_ns(&self) -> u64 {
            5_000_000_000
        }
    }

    let (_tx, rx) = DiagnosticsRegistry::test_diagnostic_feed(16);
    let (registry, handle) = DiagnosticsRegistry::start(rx, Arc::new(FixedClock));

    let session_id = SessionId::generate();
    registry.register_session(SessionRegistration {
        session_id,
        started_elapsed_ns: 7,
        primary: None,
        members: [(identified_receiver(), device(0x88))]
            .into_iter()
            .collect(),
        source: ClientDiagnosticsSource::test_new_empty(),
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if handle.snapshot().active_session_id == Some(session_id) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the registry never registered the session"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let input = registry.support_export_input();
    assert_eq!(input.latest_snapshot.active_session_id, Some(session_id));
    assert_eq!(input.latest_snapshot.schema_version, 1);
    assert_eq!(input.producer_drops_total, 0);

    // The input is exportable and stays redacted through the registry path.
    let text = export_text(&input, &context());
    assert!(!text.to_lowercase().contains("5855ca1ae288"));
    assert!(text.contains("R-001"));

    registry.finish_session(session_id, SessionStopReason::Stopped);
    drop(registry);
}

#[test]
fn the_export_is_one_self_contained_json_object() {
    let input = hostile_input();
    let text = export_text(&input, &context());
    assert!(text.starts_with('{'), "the export is one JSON object");
    assert!(text.trim_end().ends_with('}'), "the export is complete");
    assert!(
        text.len() < 4 * 1024 * 1024,
        "the export grew past four megabytes"
    );
}

// ===========================================================================
// Structural seal of the alias newtypes
// ===========================================================================

/// Source text of `diagnostics.rs`, split at the private `alias` submodule.
///
/// Returns `(inside, outside)`. The module is a top-level item, so its closing
/// brace is the first `}` in column zero after the header.
fn diagnostics_source_split_at_alias_module() -> (String, String) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/diagnostics.rs");
    let source = std::fs::read_to_string(path)
        .expect("the diagnostics module source is readable from the test")
        .replace("\r\n", "\n");

    let header = "\nmod alias {\n";
    let start = source
        .find(header)
        .expect("the alias newtypes must live in a private `mod alias` submodule");
    let body_start = start + header.len();
    let end = source[body_start..]
        .find("\n}\n")
        .map(|offset| body_start + offset + 2)
        .expect("the `mod alias` block must close at column zero");

    let inside = source[body_start..end].to_owned();
    let mut outside = source[..start].to_owned();
    outside.push_str(&source[end..]);
    (inside, outside)
}

/// Rust visibility is per module, not per type. `diagnostics.rs` is one module
/// that holds both the alias newtypes and every converter that redacts domain
/// values, so a private tuple field alone does not stop those converters from
/// wrapping a receiver name in an alias -- it only stops other files. The
/// submodule is the boundary that makes the guarantee hold where it matters.
#[test]
fn alias_newtypes_are_sealed_in_their_own_module() {
    let (inside, outside) = diagnostics_source_split_at_alias_module();

    for declaration in [
        "pub struct ReceiverAlias(String);",
        "pub struct SessionAlias(String);",
    ] {
        assert!(
            inside.contains(declaration),
            "`{declaration}` must be declared inside `mod alias`, not next to the converters"
        );
        assert!(
            !outside.contains(declaration),
            "`{declaration}` is still declared outside `mod alias`"
        );
    }

    // Only `AliasTable` -- which lives in the submodule -- may mint an alias.
    for construction in ["ReceiverAlias(", "SessionAlias("] {
        assert!(
            !outside.contains(construction),
            "`{construction}` appears outside `mod alias`: the export converters can mint \
             an alias from arbitrary text again"
        );
    }

    // The minting constructors stay private to the submodule.
    for constructor in ["fn numbered(", "fn unresolved("] {
        assert!(
            inside.contains(constructor),
            "the alias submodule must own `{constructor}`"
        );
        assert!(
            !inside.contains(&format!("pub {constructor}")),
            "`{constructor}` must stay private to the alias submodule"
        );
    }
}

// ===========================================================================
// Schema drift guard
//
// `SUPPORT_EXPORT_SCHEMA_VERSION` is hand-maintained. Without a pin on the
// document shape a renamed, removed, or added field changes the format while
// the version stays at 1 and every support tool silently reads null. The two
// tests below are that pin: one over the full set of JSON key paths, one over
// the payload discriminators, both fed by a fixture that exercises every
// `SupportEventPayload` variant.
// ===========================================================================

/// Every `kind` discriminator the export can emit.
const ALL_SUPPORT_EVENT_PAYLOAD_KINDS: &[&str] = &[
    "backend_message",
    "calibration_state",
    "capture_drop",
    "capture_transition",
    "client_event",
    "command_coalesced",
    "command_result",
    "definite_counter_transition",
    "diagnostic_feed_lag",
    "discovery_generation",
    "discovery_receiver",
    "error",
    "export_state",
    "persistence",
    "probe",
    "queue_watermark",
    "receiver_transition",
    "retry",
    "session_restart",
    "session_transition",
    "setup_phase_duration",
    "silence_bridge",
    "system_transition",
    "worker_shutdown",
];

/// The complete, sorted set of JSON key paths the export document may contain.
///
/// Object keys are dotted; `[]` marks an array level. If this list changes,
/// `SUPPORT_EXPORT_SCHEMA_VERSION` must be raised: a renamed or dropped field
/// is an incompatible change for every tool that parses the previous version,
/// and this list is the only place that notices.
const PINNED_EXPORT_KEY_PATHS: &[&str] = &[
    "application",
    "application.build_profile",
    "application.platform_arch",
    "application.platform_os",
    "application.process_elapsed_ns",
    "application.product",
    "application.version",
    "application.windows_build",
    "configuration",
    "configuration.calibration",
    "configuration.calibration.apply_state",
    "configuration.calibration.effective_delay_ns_by_receiver",
    "configuration.calibration.effective_delay_ns_by_receiver.R-001",
    "configuration.calibration.effective_delay_ns_by_receiver.R-002",
    "configuration.calibration.entries_omitted_for_unknown_receivers",
    "configuration.calibration.minimum_requested_ns",
    "configuration.calibration.reference_receiver_alias",
    "configuration.capture_queue_capacity_frames",
    "configuration.channels",
    "configuration.event_ring_capacity",
    "configuration.frame_samples_per_channel",
    "configuration.max_event_read_limit",
    "configuration.requested_latency_not_measured",
    "configuration.sample_rate_hz",
    "event_gap",
    "event_gap.overwritten_events",
    "event_gap.requested_cursor",
    "event_gap.resumed_at_cursor",
    "events",
    "events[]",
    "events[].code",
    "events[].component",
    "events[].cursor",
    "events[].occurred_at_utc",
    "events[].payload",
    "events[].payload.active_members",
    "events[].payload.attempt",
    "events[].payload.capacity",
    "events[].payload.client_kind",
    "events[].payload.control_request_duration_ms",
    "events[].payload.counter",
    "events[].payload.current_total",
    "events[].payload.depth",
    "events[].payload.dropped_events",
    "events[].payload.dropped_frames",
    "events[].payload.duration_ns",
    "events[].payload.elapsed_ns",
    "events[].payload.error",
    "events[].payload.error.code",
    "events[].payload.error.component",
    "events[].payload.error.free_form_text_omitted",
    "events[].payload.error.operation",
    "events[].payload.error.receiver_alias",
    "events[].payload.error.recoverability",
    "events[].payload.error.severity",
    "events[].payload.failed",
    "events[].payload.free_form_text_omitted",
    "events[].payload.generation",
    "events[].payload.healthy",
    "events[].payload.kind",
    "events[].payload.lifecycle",
    "events[].payload.lifecycle.attempt",
    "events[].payload.lifecycle.free_form_text_omitted",
    "events[].payload.lifecycle.retryable",
    "events[].payload.lifecycle.role",
    "events[].payload.lifecycle.setup_phase",
    "events[].payload.lifecycle.state",
    "events[].payload.local_binding_changed",
    "events[].payload.outcome",
    "events[].payload.phase",
    "events[].payload.phase.free_form_text_omitted",
    "events[].payload.phase.generation",
    "events[].payload.phase.phase",
    "events[].payload.phase.restart_reason",
    "events[].payload.present",
    "events[].payload.previous_total",
    "events[].payload.reason",
    "events[].payload.receiver_alias",
    "events[].payload.state",
    "events[].payload.superseded",
    "events[].payload.timed_out",
    "events[].payload.transition",
    "events[].process_elapsed_ns",
    "events[].receiver_alias",
    "events[].session_alias",
    "events[].severity",
    "events_retained",
    "exported_at_utc",
    "latest_snapshot",
    "latest_snapshot.active_session_alias",
    "latest_snapshot.captured_at_utc",
    "latest_snapshot.diagnostics_events_dropped_total",
    "latest_snapshot.health",
    "latest_snapshot.process_elapsed_ns",
    "latest_snapshot.receivers",
    "latest_snapshot.receivers[]",
    "latest_snapshot.receivers[].feedback",
    "latest_snapshot.receivers[].feedback.attempts_total",
    "latest_snapshot.receivers[].feedback.last_control_request_duration_ns",
    "latest_snapshot.receivers[].feedback.last_result",
    "latest_snapshot.receivers[].feedback.last_success_elapsed_ns",
    "latest_snapshot.receivers[].feedback.protocol_failures_total",
    "latest_snapshot.receivers[].feedback.successes_total",
    "latest_snapshot.receivers[].feedback.timeouts_total",
    "latest_snapshot.receivers[].feedback.transport_failures_total",
    "latest_snapshot.receivers[].last_error",
    "latest_snapshot.receivers[].last_error.code",
    "latest_snapshot.receivers[].last_error.component",
    "latest_snapshot.receivers[].last_error.free_form_text_omitted",
    "latest_snapshot.receivers[].last_error.operation",
    "latest_snapshot.receivers[].last_error.receiver_alias",
    "latest_snapshot.receivers[].last_error.recoverability",
    "latest_snapshot.receivers[].last_error.severity",
    "latest_snapshot.receivers[].lifecycle",
    "latest_snapshot.receivers[].lifecycle.attempt",
    "latest_snapshot.receivers[].lifecycle.free_form_text_omitted",
    "latest_snapshot.receivers[].lifecycle.retryable",
    "latest_snapshot.receivers[].lifecycle.role",
    "latest_snapshot.receivers[].lifecycle.setup_phase",
    "latest_snapshot.receivers[].lifecycle.state",
    "latest_snapshot.receivers[].receiver_alias",
    "latest_snapshot.receivers[].timing",
    "latest_snapshot.receivers[].timing.estimated_relative_clock_drift_ppm",
    "latest_snapshot.receivers[].timing.independently_measured",
    "latest_snapshot.receivers[].timing.mean_path_delay_ns",
    "latest_snapshot.receivers[].timing.offset_local_minus_master_ns",
    "latest_snapshot.receivers[].timing.sample_age_ns",
    "latest_snapshot.receivers[].timing.shared_from_receiver_alias",
    "latest_snapshot.receivers[].timing.source",
    "latest_snapshot.receivers[].timing.stale",
    "latest_snapshot.receivers[].transport",
    "latest_snapshot.receivers[].transport.data_bytes_accepted_local_total",
    "latest_snapshot.receivers[].transport.data_datagram_send_failures_total",
    "latest_snapshot.receivers[].transport.data_datagrams_accepted_local_total",
    "latest_snapshot.receivers[].transport.data_datagrams_attempted_total",
    "latest_snapshot.receivers[].transport.local_os_udp_acceptance",
    "latest_snapshot.receivers[].transport.recovery_demand_denominator_shared_frames",
    "latest_snapshot.receivers[].transport.recovery_demand_numerator_slots",
    "latest_snapshot.receivers[].transport.recovery_demand_ratio",
    "latest_snapshot.receivers[].transport.retransmit_slots_accepted_local_total",
    "latest_snapshot.receivers[].transport.retransmit_slots_requested_total",
    "latest_snapshot.receivers[].transport.sync_bytes_accepted_local_total",
    "latest_snapshot.receivers[].transport.sync_datagram_send_failures_total",
    "latest_snapshot.receivers[].transport.sync_datagrams_accepted_local_total",
    "latest_snapshot.receivers[].transport.sync_datagrams_attempted_total",
    "latest_snapshot.session",
    "latest_snapshot.session.audio",
    "latest_snapshot.session.audio.capture_queue",
    "latest_snapshot.session.audio.capture_queue.capacity_frames",
    "latest_snapshot.session.audio.capture_queue.disconnected_drops_total",
    "latest_snapshot.session.audio.capture_queue.full_queue_drops_total",
    "latest_snapshot.session.audio.capture_queue.queued_frames",
    "latest_snapshot.session.audio.capture_queue.submitted_frames_total",
    "latest_snapshot.session.audio.capture_queue.submitted_samples_per_channel_total",
    "latest_snapshot.session.audio.retransmit",
    "latest_snapshot.session.audio.retransmit.retransmit_datagrams_accepted_local_total",
    "latest_snapshot.session.audio.retransmit.retransmit_history_misses_total",
    "latest_snapshot.session.audio.retransmit.retransmit_packet_slots_requested_total",
    "latest_snapshot.session.audio.retransmit.retransmit_request_datagrams_total",
    "latest_snapshot.session.audio.retransmit.retransmit_send_failures_total",
    "latest_snapshot.session.audio.rtp_frames_prepared_total",
    "latest_snapshot.session.audio.scheduler",
    "latest_snapshot.session.audio.scheduler.deadline_resets_total",
    "latest_snapshot.session.audio.scheduler.last_signed_scheduler_dispatch_jitter_ns",
    "latest_snapshot.session.audio.scheduler.maximum_absolute_scheduler_dispatch_jitter_ns",
    "latest_snapshot.session.audio.scheduler.mean_absolute_scheduler_dispatch_jitter_ns",
    "latest_snapshot.session.audio.scheduler.p95_absolute_scheduler_dispatch_jitter_ns",
    "latest_snapshot.session.audio.scheduler.sample_count",
    "latest_snapshot.session.audio.sender_buffer",
    "latest_snapshot.session.audio.sender_buffer.buffered_ns",
    "latest_snapshot.session.audio.sender_buffer.capacity_frames",
    "latest_snapshot.session.audio.sender_buffer.fill_ratio",
    "latest_snapshot.session.audio.sender_buffer.queued_frames",
    "latest_snapshot.session.audio.sender_buffer.queued_samples_per_channel",
    "latest_snapshot.session.audio.sender_buffer.samples_read_total",
    "latest_snapshot.session.audio.sender_buffer.samples_written_total",
    "latest_snapshot.session.audio.sender_buffer.underrun_events_total",
    "latest_snapshot.session.audio.target_count",
    "latest_snapshot.session.finished_elapsed_ns",
    "latest_snapshot.session.member_count",
    "latest_snapshot.session.member_receiver_aliases",
    "latest_snapshot.session.member_receiver_aliases[]",
    "latest_snapshot.session.phase",
    "latest_snapshot.session.phase.free_form_text_omitted",
    "latest_snapshot.session.phase.generation",
    "latest_snapshot.session.phase.phase",
    "latest_snapshot.session.phase.restart_reason",
    "latest_snapshot.session.primary_receiver_alias",
    "latest_snapshot.session.session_alias",
    "latest_snapshot.session.started_elapsed_ns",
    "latest_snapshot.session.stop_reason",
    "latest_snapshot.snapshot_schema_version",
    "latest_snapshot.snapshot_sequence",
    "producer_drops_total",
    "redaction",
    "redaction.omitted",
    "redaction.omitted[]",
    "redaction.policy",
    "redaction.receiver_identity",
    "redaction.unredacted_mode_available",
    "schema",
    "schema_version",
];

/// The payload variants `hostile_events` does not already cover.
fn remaining_payloads() -> Vec<StructuredDiagnosticPayload> {
    vec![
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::CommandCoalesced { dropped: 3 }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::SetupPhaseDuration {
            receiver: identified_receiver(),
            phase: SetupPhase::Pair,
            duration: Duration::from_millis(120),
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::SessionTransition {
            phase: SessionPhase::Restarting {
                generation: 8,
                reason: RestartReason::MembershipChange,
            },
            active_members: 2,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::SessionRestart {
            generation: 8,
            reason: RestartReason::SystemResume,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::Retry {
            receiver: Some(identified_receiver()),
            attempt: 2,
            next_at: at(1_774_000_100),
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::CaptureTransition {
            state: AudioSourceState::Recovering,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::PcmDrop { dropped: 4 }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::SilenceBridge {
            duration: Duration::from_millis(250),
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::Probe {
            healthy: true,
            latency_ms: 12,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::Persistence {
            outcome: PersistenceSnapshot::Healthy,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::QueueWatermark {
            depth: 5,
            capacity: 32,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::WorkerShutdown {
            duration: Duration::from_secs(1),
            timed_out: false,
        }),
        StructuredDiagnosticPayload::Backend(DiagnosticPayload::SystemTransition {
            transition: SystemTransition::NetworkChanged {
                local_binding_changed: true,
            },
        }),
    ]
}

/// The hostile fixture plus one event per payload variant it does not reach,
/// so the pinned key-path set covers the whole document shape.
fn pinned_input() -> SupportExportInput {
    let mut input = hostile_input();
    let session_id = input
        .latest_snapshot
        .active_session_id
        .expect("the hostile fixture runs a session");
    let key = ReceiverSessionKey {
        session_id,
        receiver_id: identified_receiver(),
    };
    let mut cursor = input
        .retained_events
        .last()
        .map(|event| event.cursor.0)
        .unwrap_or(0);

    for payload in remaining_payloads() {
        cursor += 1;
        input.retained_events.push(DiagnosticEvent {
            cursor: EventCursor(cursor),
            occurred_at_utc: at(1_774_000_200 + cursor),
            process_elapsed_ns: 61_000_000_000 + cursor,
            receiver_session: Some(key.clone()),
            severity: DiagnosticSeverity::Info,
            component: DiagnosticComponent::Backend,
            code: DiagnosticErrorCode::EventChannelFailure,
            public_message: String::new(),
            payload,
        });
    }
    input
}

fn collect_key_paths(value: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.insert(path.clone());
                collect_key_paths(child, &path, out);
            }
        }
        serde_json::Value::Array(items) => {
            let path = format!("{prefix}[]");
            out.insert(path.clone());
            for item in items {
                collect_key_paths(item, &path, out);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

#[test]
fn the_pinned_fixture_exercises_every_event_payload_variant() {
    let json = export_json(&pinned_input(), &context());
    let observed: BTreeSet<String> = json["events"]
        .as_array()
        .expect("events are a list")
        .iter()
        .map(|event| {
            event["payload"]["kind"]
                .as_str()
                .expect("every payload names its kind")
                .to_owned()
        })
        .collect();
    let expected: BTreeSet<String> = ALL_SUPPORT_EVENT_PAYLOAD_KINDS
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect();

    assert_eq!(
        observed, expected,
        "the pinned fixture no longer covers exactly the known payload kinds; a new \
         variant needs a fixture event and a schema-version decision"
    );
}

#[test]
fn the_exported_key_paths_are_pinned_to_the_schema_version() {
    let json = export_json(&pinned_input(), &context());
    let mut observed = BTreeSet::new();
    collect_key_paths(&json, "", &mut observed);

    let expected: BTreeSet<String> = PINNED_EXPORT_KEY_PATHS
        .iter()
        .map(|path| (*path).to_owned())
        .collect();

    let added: Vec<&String> = observed.difference(&expected).collect();
    let removed: Vec<&String> = expected.difference(&observed).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "the exported field set changed -- raise SUPPORT_EXPORT_SCHEMA_VERSION and update \
         PINNED_EXPORT_KEY_PATHS.\nadded: {added:#?}\nremoved: {removed:#?}"
    );
}
