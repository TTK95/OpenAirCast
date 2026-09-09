//! Integration tests for manual presentation-time calibration: profile
//! normalization, click-test/display constants, and the schema-v1 to schema-v2
//! device-state migration that carries the calibration section.
//!
//! Calibration itself lives outside shell settings; persistence goes through
//! the existing atomic `%APPDATA%\OpenAirCast\state-v1.json` flow.

use std::collections::{BTreeMap, BTreeSet};

use airplay_core::DeviceId;
use homepod_cast::backend::model::ReceiverId;
use homepod_cast::backend::persistence::{CalibrationStateV2, FileStore, LoadOutcome, StateStore};
use homepod_cast::calibration::{
    calibration_click_pattern, effective_delays_by_device, normalize_calibration, CalibrationError,
    CalibrationProfile, CALIBRATION_CLICK_AMPLITUDE, CALIBRATION_DISPLAY_STEP_NS,
};

const MS_NS: i64 = 1_000_000;

/// Deterministic receiver identity for the given seed byte.
fn receiver_id(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}")).expect("valid storage key")
}

#[test]
fn normalize_shifts_minimum_to_zero_and_keeps_relative_order() {
    // Non-negative requested values already start at zero: unchanged.
    let active = BTreeSet::from([receiver_id(1), receiver_id(2), receiver_id(3)]);
    let profile = CalibrationProfile {
        reference_receiver: Some(receiver_id(1)),
        requested_relative_delay_ns: BTreeMap::from([
            (receiver_id(1), 0),
            (receiver_id(2), 50 * MS_NS),
            (receiver_id(3), 120 * MS_NS),
        ]),
    };

    let effective = normalize_calibration(&active, &profile).expect("normalizable profile");

    assert_eq!(effective.minimum_requested_ns, 0);
    assert_eq!(
        effective.effective_delay_ns,
        BTreeMap::from([
            (receiver_id(1), 0_u64),
            (receiver_id(2), 50 * MS_NS as u64),
            (receiver_id(3), 120 * MS_NS as u64),
        ])
    );

    // Negative minimums shift every speaker up so the earliest stays put;
    // pairwise differences are preserved exactly.
    let pair_active = BTreeSet::from([receiver_id(1), receiver_id(2)]);
    let shifted = CalibrationProfile {
        reference_receiver: Some(receiver_id(1)),
        requested_relative_delay_ns: BTreeMap::from([
            (receiver_id(1), -30 * MS_NS),
            (receiver_id(2), 0),
        ]),
    };

    let effective = normalize_calibration(&pair_active, &shifted).expect("normalizable profile");

    assert_eq!(effective.minimum_requested_ns, -30 * MS_NS);
    assert_eq!(
        effective.effective_delay_ns,
        BTreeMap::from([(receiver_id(1), 0_u64), (receiver_id(2), 30 * MS_NS as u64),])
    );
    let requested_gap = 0 - (-30 * MS_NS);
    let effective_gap = effective.effective_delay_ns[&receiver_id(2)] as i128
        - effective.effective_delay_ns[&receiver_id(1)] as i128;
    assert_eq!(i128::from(requested_gap), effective_gap);
}

#[test]
fn normalize_treats_omitted_active_members_as_zero_request() {
    let active = BTreeSet::from([receiver_id(1), receiver_id(2)]);
    let profile = CalibrationProfile {
        reference_receiver: Some(receiver_id(1)),
        requested_relative_delay_ns: BTreeMap::from([(receiver_id(2), -5 * MS_NS)]),
    };

    let effective = normalize_calibration(&active, &profile).expect("normalizable profile");

    // Receiver 1 is absent from the map and therefore requests zero.
    assert_eq!(effective.minimum_requested_ns, -5 * MS_NS);
    assert_eq!(
        effective.effective_delay_ns,
        BTreeMap::from([(receiver_id(1), 5 * MS_NS as u64), (receiver_id(2), 0_u64),])
    );
}

#[test]
fn normalize_ignores_profile_entries_not_in_active_selection() {
    let active = BTreeSet::from([receiver_id(1), receiver_id(2)]);
    let profile = CalibrationProfile {
        reference_receiver: Some(receiver_id(1)),
        requested_relative_delay_ns: BTreeMap::from([
            (receiver_id(1), 10 * MS_NS),
            (receiver_id(2), 40 * MS_NS),
            // Stale member from a previous group: ignored entirely, so it
            // neither shifts the minimum nor appears in the output.
            (receiver_id(9), -500 * MS_NS),
        ]),
    };

    let effective = normalize_calibration(&active, &profile).expect("normalizable profile");

    assert_eq!(effective.minimum_requested_ns, 10 * MS_NS);
    assert_eq!(
        effective.effective_delay_ns,
        BTreeMap::from([(receiver_id(1), 0_u64), (receiver_id(2), 30 * MS_NS as u64),])
    );
}

#[test]
fn normalize_rejects_reference_outside_selection_and_empty_selection() {
    let active = BTreeSet::from([receiver_id(1)]);
    let outsider_reference = CalibrationProfile {
        reference_receiver: Some(receiver_id(2)),
        requested_relative_delay_ns: BTreeMap::new(),
    };
    assert!(matches!(
        normalize_calibration(&active, &outsider_reference),
        Err(CalibrationError::ReferenceNotInSelection)
    ));

    let empty = BTreeSet::new();
    assert!(matches!(
        normalize_calibration(&empty, &CalibrationProfile::default()),
        Err(CalibrationError::EmptySelection)
    ));
}

#[test]
fn display_step_constant_is_hundred_microseconds() {
    assert_eq!(CALIBRATION_DISPLAY_STEP_NS, 100_000_i64);
}

#[test]
fn click_amplitude_constant_matches_spec() {
    assert_eq!(CALIBRATION_CLICK_AMPLITUDE, 4_096_i16);
}

#[tokio::test]
async fn calibration_roundtrips_through_persistence_v2_and_v1_files_still_load() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state_file = dir.path().join("state-v1.json");

    // A hand-written schema-v1 document without a calibration section still
    // loads and reports no persisted calibration.
    let v1_document = format!(
        r#"{{"version":1,"master_volume":0.25,"muted":false,"receiver_levels":{{}},"saved_groups":[],"last_desired_members":["{key}"],"audio_endpoint":"SystemDefault","latency_preset":"Normal","auto_connect":false}}"#,
        key = receiver_id(1).storage_key(),
    );
    std::fs::write(&state_file, v1_document).expect("write v1 fixture");

    let outcome = store.load().await.expect("loadable v1 fixture");
    let loaded = match &outcome {
        LoadOutcome::Loaded(state) => state.clone(),
        other => panic!("expected Loaded, got {other:?}"),
    };
    assert_eq!(loaded.version, 1);
    assert_eq!(store.load_calibration().await.expect("readable"), None);

    // Persisting calibration upgrades the document to schema version 2 under
    // the same atomic temporary-file flush-and-replace machinery.
    let section = CalibrationStateV2 {
        reference: Some(receiver_id(2)),
        delays_ns: BTreeMap::from([(receiver_id(2), 0), (receiver_id(3), 250 * MS_NS)]),
    };
    store
        .save_calibration(Some(section.clone()))
        .await
        .expect("saveable calibration");

    let raw = std::fs::read_to_string(&state_file).expect("read written state");
    assert!(raw.contains("\"version\": 2"), "{raw}");
    assert!(raw.contains("\"calibration\""), "{raw}");

    // Reloading restores the section exactly and every predecessor field.
    assert_eq!(
        store.load_calibration().await.expect("readable"),
        Some(section)
    );
    let reloaded = store
        .load()
        .await
        .expect("reloadable upgraded document")
        .into_state();
    assert_eq!(reloaded.master_volume, loaded.master_volume);
    assert_eq!(reloaded.muted, loaded.muted);
    assert_eq!(reloaded.receiver_levels, loaded.receiver_levels);
    assert_eq!(reloaded.saved_groups, loaded.saved_groups);
    assert_eq!(reloaded.last_desired_members, loaded.last_desired_members);
    assert_eq!(reloaded.audio_endpoint, loaded.audio_endpoint);
    assert_eq!(reloaded.latency_preset, loaded.latency_preset);
    assert_eq!(reloaded.auto_connect, loaded.auto_connect);

    // Clearing persists an empty/zero profile — the migration target of
    // every version-1 file.
    store
        .save_calibration(None)
        .await
        .expect("clearable calibration");
    assert_eq!(
        store.load_calibration().await.expect("readable"),
        Some(CalibrationStateV2::default())
    );
}

// ---------------------------------------------------------------------------
// Task 9: the one ReceiverId -> DeviceId bridge
// ---------------------------------------------------------------------------

#[test]
fn effective_delays_convert_to_the_client_device_keying_losslessly() {
    let active = BTreeSet::from([receiver_id(1), receiver_id(2), receiver_id(3)]);
    let profile = CalibrationProfile {
        reference_receiver: Some(receiver_id(1)),
        requested_relative_delay_ns: BTreeMap::from([
            (receiver_id(1), -2 * MS_NS),
            (receiver_id(2), 0),
            (receiver_id(3), 7 * MS_NS / 2),
        ]),
    };
    let effective = normalize_calibration(&active, &profile).expect("normalizable profile");

    let by_device = effective_delays_by_device(&effective);

    // One entry per active receiver, same values, keyed by the client's own
    // stable identity. Nothing is dropped and nothing is invented.
    assert_eq!(by_device.len(), effective.effective_delay_ns.len());
    for (receiver, delay) in &effective.effective_delay_ns {
        let device: DeviceId = (*receiver).into();
        assert_eq!(by_device.get(&device), Some(delay), "{receiver} lost");
    }
    assert_eq!(
        by_device.values().copied().collect::<Vec<u64>>(),
        vec![0, 2 * MS_NS as u64, 5_500_000],
    );
}

// ---------------------------------------------------------------------------
// Task 9: shared four-click alignment pattern
// ---------------------------------------------------------------------------

#[test]
fn the_click_pattern_places_four_fixed_amplitude_clicks_in_silence() {
    let pattern = calibration_click_pattern(44_100, 2);

    let frame_of = |ms: u32| (44_100_u64 * u64::from(ms) / 1_000) as usize;
    let click_frames = frame_of(2);
    let spacing_frames = frame_of(500);
    assert_eq!(click_frames, 88);
    assert_eq!(spacing_frames, 22_050);
    // Three gaps between four clicks, plus the trailing click itself.
    let total_frames = 3 * spacing_frames + click_frames;
    assert_eq!(pattern.len(), total_frames * 2, "stereo interleaved");

    for (frame, samples) in pattern.as_chunks::<2>().0.iter().enumerate() {
        let offset = frame % spacing_frames;
        let inside_click = frame < total_frames && offset < click_frames;
        let expected = if inside_click {
            CALIBRATION_CLICK_AMPLITUDE
        } else {
            0
        };
        assert_eq!(*samples, [expected, expected], "frame {frame}");
    }

    // Exactly four clicks, all identical: nothing here is receiver specific.
    let click_samples = pattern.iter().filter(|s| **s != 0).count();
    assert_eq!(click_samples, 4 * click_frames * 2);
}

// ---------------------------------------------------------------------------
// Task 9: domain profile <-> persisted section
// ---------------------------------------------------------------------------

#[test]
fn the_domain_profile_and_the_persisted_section_are_one_shape() {
    let profile = CalibrationProfile {
        reference_receiver: Some(receiver_id(2)),
        requested_relative_delay_ns: BTreeMap::from([
            (receiver_id(2), 0),
            (receiver_id(3), -12 * MS_NS),
        ]),
    };

    let section = CalibrationStateV2::from(&profile);
    assert_eq!(section.reference, profile.reference_receiver);
    assert_eq!(section.delays_ns, profile.requested_relative_delay_ns);
    assert_eq!(CalibrationProfile::from(&section), profile);

    // The empty/zero profile is the migration target of every version-1 file
    // and survives the round trip as itself.
    assert_eq!(
        CalibrationProfile::from(&CalibrationStateV2::default()),
        CalibrationProfile::default()
    );
}

// ---------------------------------------------------------------------------
// Task 9: migration is a promotion, never a guess
// ---------------------------------------------------------------------------

/// A complete schema-v1 document with a non-default value in *every* field, so
/// a migration that silently substitutes a default is caught.
fn complete_v1_document() -> String {
    format!(
        concat!(
            r#"{{"version":1,"master_volume":0.375,"muted":true,"#,
            r#""receiver_levels":{{"{one}":0.5,"{two}":0.875}},"#,
            r#""saved_groups":[{{"id":"{group}","name":"Abends","#,
            r#""members":[{{"receiver":"{one}","last_known_name":"Kueche","#,
            r#""level":0.5}},{{"receiver":"{two}","last_known_name":"Bad","#,
            r#""level":0.875}}]}}],"#,
            r#""last_desired_members":["{one}","{two}"],"#,
            r#""audio_endpoint":{{"Explicit":{{"id":"ep-1","last_known_name":"Speakers"}}}},"#,
            r#""latency_preset":"Normal","auto_connect":true}}"#
        ),
        one = receiver_id(1).storage_key(),
        two = receiver_id(2).storage_key(),
        group = uuid::Uuid::nil(),
    )
}

#[tokio::test]
async fn loading_a_version_one_file_neither_rewrites_it_nor_invents_a_profile() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state_file = dir.path().join("state-v1.json");
    let original = complete_v1_document();
    std::fs::write(&state_file, &original).expect("write v1 fixture");

    let loaded = match store.load().await.expect("loadable v1 fixture") {
        LoadOutcome::Loaded(state) => state,
        other => panic!("expected Loaded, got {other:?}"),
    };

    // A load is a read. Migration-on-read would rewrite a file the user may
    // still want to open with the previous build.
    assert_eq!(
        std::fs::read_to_string(&state_file).expect("state file still there"),
        original,
        "loading must not rewrite the state file"
    );
    // No calibration existed, and none was invented.
    assert_eq!(store.load_calibration().await.expect("readable"), None);

    // Every predecessor value survived the read verbatim.
    assert_eq!(loaded.master_volume.get(), 0.375);
    assert!(loaded.muted);
    assert_eq!(loaded.receiver_levels.len(), 2);
    assert_eq!(loaded.saved_groups.len(), 1);
    assert_eq!(loaded.saved_groups[0].name, "Abends");
    assert_eq!(loaded.saved_groups[0].members.len(), 2);
    assert_eq!(loaded.last_desired_members.len(), 2);
    assert!(loaded.auto_connect);

    // Writing calibration promotes the document; nothing else moves.
    store
        .save_calibration(Some(CalibrationStateV2 {
            reference: Some(receiver_id(1)),
            delays_ns: BTreeMap::from([(receiver_id(2), 3 * MS_NS)]),
        }))
        .await
        .expect("saveable calibration");
    let promoted = store
        .load()
        .await
        .expect("reloadable upgraded document")
        .into_state();
    assert_eq!(promoted.master_volume, loaded.master_volume);
    assert_eq!(promoted.muted, loaded.muted);
    assert_eq!(promoted.receiver_levels, loaded.receiver_levels);
    assert_eq!(promoted.saved_groups, loaded.saved_groups);
    assert_eq!(promoted.last_desired_members, loaded.last_desired_members);
    assert_eq!(promoted.audio_endpoint, loaded.audio_endpoint);
    assert_eq!(promoted.latency_preset, loaded.latency_preset);
    assert_eq!(promoted.auto_connect, loaded.auto_connect);
}

/// Reads the single `.corrupt-<millis>` sibling the quarantine leaves behind.
fn quarantined_content(directory: &std::path::Path) -> String {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
        .expect("readable state directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.starts_with("corrupt-"))
        })
        .collect();
    assert_eq!(found.len(), 1, "expected exactly one quarantined file");
    std::fs::read_to_string(found.pop().expect("one path")).expect("readable quarantine")
}

#[tokio::test]
async fn a_document_from_a_newer_build_is_set_aside_rather_than_blocking_the_launch() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state_file = dir.path().join("state-v1.json");
    // Shape-compatible with schema v1 in every field a future reader would
    // recognize -- only the version says "written by a newer build".
    let future = complete_v1_document().replace(r#""version":1"#, r#""version":3"#);
    std::fs::write(&state_file, &future).expect("write v3 fixture");

    // A version this build cannot read must not become a launch that never
    // completes: refusing outright would fail identically on every later
    // start, with no way out of the app itself.
    let outcome = store
        .load()
        .await
        .expect("an unreadable version still loads");
    assert!(
        matches!(outcome, LoadOutcome::RecoveredCorrupt { .. }),
        "expected RecoveredCorrupt, got {outcome:?}"
    );
    // Defaults, not the newer build's values guessed at.
    let restored = outcome.into_state();
    assert_eq!(
        restored.master_volume,
        homepod_cast::backend::model::Volume::DEFAULT_MASTER
    );
    assert!(!restored.muted);
    assert!(restored.saved_groups.is_empty());
    assert!(restored.last_desired_members.is_empty());
    // Set aside, not destroyed: the bytes survive verbatim for a newer build
    // or a bug report.
    assert!(!state_file.exists(), "the unreadable document was moved");
    assert_eq!(quarantined_content(dir.path()), future);
}

#[tokio::test]
async fn a_damaged_version_two_document_is_set_aside_rather_than_blocking_the_launch() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state_file = dir.path().join("state-v1.json");

    // Write a genuine schema-v2 document first, then damage one calibration
    // value so it parses as JSON but violates the version-2 schema. This is
    // the shape Task 9 itself put on disk, so it is the one that would strand
    // a real installation.
    store
        .save_calibration(Some(CalibrationStateV2 {
            reference: Some(receiver_id(1)),
            delays_ns: BTreeMap::from([(receiver_id(2), 3 * MS_NS)]),
        }))
        .await
        .expect("saveable calibration");
    let damaged = std::fs::read_to_string(&state_file)
        .expect("written v2 document")
        .replace(&format!("{}", 3 * MS_NS), r#""three milliseconds""#);
    std::fs::write(&state_file, &damaged).expect("write damaged fixture");

    let outcome = store.load().await.expect("a damaged document still loads");
    assert!(
        matches!(outcome, LoadOutcome::RecoveredCorrupt { .. }),
        "expected RecoveredCorrupt, got {outcome:?}"
    );
    assert!(!state_file.exists(), "the damaged document was moved");
    assert_eq!(quarantined_content(dir.path()), damaged);
    // The quarantine ran, so nothing unreadable is left for the calibration
    // read to trip over either.
    assert_eq!(store.load_calibration().await.expect("readable"), None);
}

#[tokio::test]
async fn the_calibration_write_refuses_a_document_it_cannot_read() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state_file = dir.path().join("state-v1.json");
    let future = complete_v1_document().replace(r#""version":1"#, r#""version":3"#);
    std::fs::create_dir_all(dir.path()).expect("state directory");
    std::fs::write(&state_file, &future).expect("write v3 fixture");

    // The write path is not the launch path: refusing here loses nothing,
    // because the caller reports the failure and applies nothing, whereas
    // overwriting would destroy a document written by a build that can still
    // read it.
    let error = store
        .save_calibration(Some(CalibrationStateV2::default()))
        .await
        .expect_err("unknown versions are refused");
    assert!(
        error.to_string().contains("unsupported schema version 3"),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&state_file).expect("state file untouched"),
        future
    );
}

#[tokio::test]
async fn an_ordinary_save_carries_the_calibration_section_forward() {
    let dir = tempfile::tempdir().expect("temp directory");
    let store = FileStore::new(dir.path().to_path_buf(), None);
    std::fs::create_dir_all(dir.path()).expect("state directory");
    std::fs::write(dir.path().join("state-v1.json"), complete_v1_document())
        .expect("write v1 fixture");

    let section = CalibrationStateV2 {
        reference: Some(receiver_id(1)),
        delays_ns: BTreeMap::from([(receiver_id(2), -4 * MS_NS)]),
    };
    store
        .save_calibration(Some(section.clone()))
        .await
        .expect("saveable calibration");

    // A plain volume save goes through the schema-v1 shape; the calibration
    // section is not part of it and must not be dropped by it.
    let mut state = store.load().await.expect("loadable").into_state();
    state.muted = false;
    store.save(&state).await.expect("saveable state");

    assert_eq!(
        store.load_calibration().await.expect("readable"),
        Some(section)
    );
}

// ---------------------------------------------------------------------------
// Task 9: an unreadable document is never mistaken for an empty one
// ---------------------------------------------------------------------------

/// Puts a directory where the state file belongs.
///
/// A portable way to make `std::fs::read` fail with something other than
/// `NotFound`, which is the distinction the store has to keep: "there is no
/// state" and "the state could not be read" call for opposite handling, and
/// only the first of them may lead to defaults being written.
fn unreadable_state_file(directory: &std::path::Path) {
    std::fs::create_dir_all(directory.join("state-v1.json")).expect("blocking directory");
}

#[tokio::test]
async fn an_unreadable_document_is_not_reported_as_an_absent_calibration() {
    let dir = tempfile::tempdir().expect("temp directory");
    unreadable_state_file(dir.path());
    let store = FileStore::new(dir.path().to_path_buf(), None);

    let error = store
        .load_calibration()
        .await
        .expect_err("an unreadable document is an error, not an empty one");
    assert!(
        matches!(
            error,
            homepod_cast::backend::persistence::PersistError::Io(_)
        ),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn an_unreadable_document_stops_an_ordinary_save_before_it_drops_the_calibration() {
    let dir = tempfile::tempdir().expect("temp directory");
    unreadable_state_file(dir.path());
    let store = FileStore::new(dir.path().to_path_buf(), None);

    // The carry-forward read is what decides whether the written document
    // keeps its calibration section. A read failure treated as "no section"
    // would write a schema-v1 document -- so the save has to fail on the
    // read, before any bytes are produced, not later at the replace.
    let error = store
        .save(&Default::default())
        .await
        .expect_err("an unreadable document refuses the save");
    assert!(
        matches!(
            error,
            homepod_cast::backend::persistence::PersistError::Io(_)
        ),
        "expected the failure to come from the read, got: {error}"
    );
}

#[tokio::test]
async fn an_unreadable_document_stops_a_calibration_write_before_it_defaults_everything() {
    let dir = tempfile::tempdir().expect("temp directory");
    unreadable_state_file(dir.path());
    let store = FileStore::new(dir.path().to_path_buf(), None);

    // Substituting a default document here would replace master volume,
    // mute, receiver levels, saved groups, the desired selection, the audio
    // endpoint, the latency preset and auto-connect all at once.
    let error = store
        .save_calibration(Some(CalibrationStateV2::default()))
        .await
        .expect_err("an unreadable document refuses the write");
    assert!(
        matches!(
            error,
            homepod_cast::backend::persistence::PersistError::Io(_)
        ),
        "expected the failure to come from the read, got: {error}"
    );
}
