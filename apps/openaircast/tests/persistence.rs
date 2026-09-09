//! Integration tests for versioned device-state persistence and legacy
//! volume migration (`backend::persistence`).
//!
//! All filesystem access runs through the store's `spawn_blocking` boundary;
//! tests only inject temporary directories and a failing replace seam.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use homepod_cast::backend::model::{
    AudioEndpointPreference, LatencyPreset, ReceiverId, SavedGroup, SavedGroupMember, Volume,
};
use homepod_cast::backend::persistence::{
    effective_volume, FileStore, LoadOutcome, PersistError, PersistedStateV1, StateReplacer,
    StateStore,
};

/// Deterministic receiver identity for the given seed byte.
fn receiver_id(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}")).expect("valid storage key")
}

fn sample_state() -> PersistedStateV1 {
    let mut receiver_levels = BTreeMap::new();
    receiver_levels.insert(receiver_id(1), Volume::new(0.4).unwrap());
    receiver_levels.insert(receiver_id(2), Volume::new(0.8).unwrap());
    PersistedStateV1 {
        version: 1,
        master_volume: Volume::new(0.25).unwrap(),
        muted: false,
        receiver_levels,
        saved_groups: vec![SavedGroup {
            id: homepod_cast::backend::model::SavedGroupId::generate(),
            name: "Abends".into(),
            members: vec![SavedGroupMember {
                receiver: receiver_id(1),
                last_known_name: "Wohnzimmer".into(),
                level: Volume::UNITY,
            }],
        }],
        last_desired_members: BTreeSet::from([receiver_id(1), receiver_id(2)]),
        audio_endpoint: AudioEndpointPreference::Explicit {
            id: "{0.0.0.00000000}".into(),
            last_known_name: "Lautsprecher".into(),
        },
        latency_preset: LatencyPreset::Normal,
        auto_connect: false,
    }
}

fn state_file(dir: &Path) -> std::path::PathBuf {
    dir.join("state-v1.json")
}

fn corrupt_entries(dir: &Path) -> usize {
    dir.read_dir()
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
        .count()
}

#[test]
fn schema_round_trip_includes_all_device_preferences() {
    let state = sample_state();
    let json = serde_json::to_string_pretty(&state).unwrap();
    let decoded: PersistedStateV1 = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, state);
    assert!(json.contains("\"version\": 1"));
    assert!(json.contains("\"audio_endpoint\""));
    assert!(json.contains("\"auto_connect\""));
}

#[test]
fn serialized_state_contains_no_shell_or_protocol_state() {
    let json = serde_json::to_string(&sample_state())
        .unwrap()
        .to_lowercase();
    for forbidden in [
        "hotkey",
        "window",
        "theme",
        "ip_address",
        "pin",
        "ed25519",
        "pairing",
    ] {
        assert!(!json.contains(forbidden), "unexpected field {forbidden}");
    }
}

#[test]
fn effective_volume_formula_cases() {
    // Unmute multiplies master by receiver level.
    assert_eq!(
        effective_volume(Volume::new(0.5).unwrap(), Volume::new(0.4).unwrap(), false).get(),
        0.2
    );
    // Mute preserves stored values but applies zero.
    assert_eq!(
        effective_volume(Volume::new(0.5).unwrap(), Volume::new(0.4).unwrap(), true),
        Volume::MUTED
    );
    // Unity master keeps the receiver level; zero master silences.
    assert_eq!(
        effective_volume(Volume::UNITY, Volume::new(0.3).unwrap(), false),
        Volume::new(0.3).unwrap()
    );
    assert_eq!(
        effective_volume(Volume::MUTED, Volume::UNITY, false),
        Volume::MUTED
    );
}

#[tokio::test]
async fn missing_files_return_defaults_without_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().join("OpenAirCast"), None);

    let outcome = store.load().await.unwrap();

    assert_eq!(outcome, LoadOutcome::FreshDefaults);
    assert!(!dir
        .path()
        .join("OpenAirCast")
        .join("state-v1.json")
        .exists());
}

#[tokio::test]
async fn save_then_load_preserves_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().to_path_buf(), None);
    let state = sample_state();

    store.save(&state).await.unwrap();

    let outcome = store.load().await.unwrap();
    assert_eq!(outcome.into_state(), state);
}

#[tokio::test]
async fn corrupt_json_is_renamed_and_defaults_are_returned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(state_file(dir.path()), b"not-json").unwrap();

    let outcome = FileStore::new(dir.path().to_path_buf(), None)
        .load()
        .await
        .unwrap();

    match &outcome {
        LoadOutcome::RecoveredCorrupt { quarantined_path } => {
            assert!(quarantined_path.starts_with(dir.path()));
            assert!(quarantined_path.exists(), "evidence file is preserved");
        }
        other => panic!("expected RecoveredCorrupt, got {other:?}"),
    }
    assert_eq!(outcome.clone().into_state(), PersistedStateV1::default());
    assert_eq!(corrupt_entries(dir.path()), 1);
    assert!(!state_file(dir.path()).exists(), "original name vacated");
}

#[tokio::test]
async fn invalid_state_values_are_rejected_without_quarantine() {
    struct Case {
        name: &'static str,
        json: String,
    }
    let group_template = |id: &str, name: &str, members: &str| {
        format!(r#"{{"id":"{id}","name":"{name}","members":{members}}}"#)
    };
    let member = |key: &str| format!(r#"{{"receiver":"{key}","last_known_name":"X","level":1.0}}"#);
    let members = |keys: &[&str]| {
        format!(
            "[{}]",
            keys.iter()
                .map(|key| member(key))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let base_with = |saved_groups: &str, latency: &str, version: u32| {
        format!(
            r#"{{"version":{version},"master_volume":0.25,"muted":false,"receiver_levels":{{}},"saved_groups":[{saved_groups}],"last_desired_members":[],"audio_endpoint":"SystemDefault","latency_preset":"{latency}","auto_connect":false}}"#
        )
    };
    // Every case here is a document this build can *parse* and then finds
    // unusable. A version it cannot parse at all is a different question and
    // is covered in `tests/calibration.rs`: those are quarantined so the
    // launch still completes.
    let cases = vec![
        Case {
            name: "disabled latency preset",
            json: base_with("", "Low", 1),
        },
        Case {
            name: "case-insensitive duplicate group names",
            json: base_with(
                &format!(
                    "{},{}",
                    group_template(
                        "00000000-0000-0000-0000-000000000001",
                        "Abends",
                        &members(&["000000000001"]),
                    ),
                    group_template(
                        "00000000-0000-0000-0000-000000000002",
                        "abends",
                        &members(&["000000000002"]),
                    ),
                ),
                "Normal",
                1,
            ),
        },
        Case {
            name: "group without members",
            json: base_with(
                &group_template("00000000-0000-0000-0000-000000000003", "Solo", "[]"),
                "Normal",
                1,
            ),
        },
        Case {
            name: "duplicate receivers inside one group",
            json: base_with(
                &group_template(
                    "00000000-0000-0000-0000-000000000004",
                    "Doppelt",
                    &members(&["000000000001", "000000000001"]),
                ),
                "Normal",
                1,
            ),
        },
        Case {
            name: "untrimmed group name",
            json: base_with(
                &group_template(
                    "00000000-0000-0000-0000-000000000005",
                    " Abends",
                    &members(&["000000000001"]),
                ),
                "Normal",
                1,
            ),
        },
    ];

    for case in cases {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(state_file(dir.path()), case.json.as_bytes()).unwrap();

        let outcome = FileStore::new(dir.path().to_path_buf(), None).load().await;

        assert!(
            matches!(outcome, Err(PersistError::Invalid(_))),
            "{} must be rejected as Invalid, got {:?}",
            case.name,
            outcome
        );
        assert!(
            state_file(dir.path()).exists(),
            "{} stays in place",
            case.name
        );
        assert_eq!(
            corrupt_entries(dir.path()),
            0,
            "{} is not quarantined",
            case.name
        );
    }
}

#[tokio::test]
async fn legacy_volume_seeds_master_once() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("HomePodCast").join("volume.txt");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "0.375").unwrap();
    let store = FileStore::for_app_data_root(dir.path());

    let first = store.load().await.unwrap();
    assert_eq!(first.clone().into_state().master_volume.get(), 0.375);
    assert!(
        matches!(&first, LoadOutcome::MigratedLegacyVolume { legacy_source, .. }
            if legacy_source == &legacy),
        "first load marks the migration: {first:?}"
    );
    assert!(legacy.exists(), "legacy file is kept untouched");
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "0.375");

    // Migration-once semantics: the seeded state was persisted under the new
    // schema, so the second load reads it back instead of migrating again.
    let second = store.load().await.unwrap();
    assert!(matches!(second, LoadOutcome::Loaded(_)), "got {second:?}");
    assert_eq!(second.into_state().master_volume.get(), 0.375);
    assert!(legacy.exists());
}

#[tokio::test]
async fn legacy_value_is_validated_then_clamped() {
    for (raw, expected) in [("7", 1.0_f32), ("-3", 0.0_f32)] {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("HomePodCast").join("volume.txt");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, raw).unwrap();

        let outcome = FileStore::for_app_data_root(dir.path())
            .load()
            .await
            .unwrap();

        assert_eq!(outcome.into_state().master_volume.get(), expected);
        assert!(legacy.exists());
    }

    // Unparseable content never migrates and leaves defaults in place.
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("HomePodCast").join("volume.txt");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "garbage").unwrap();

    let outcome = FileStore::for_app_data_root(dir.path())
        .load()
        .await
        .unwrap();

    assert_eq!(outcome, LoadOutcome::FreshDefaults);
    assert!(!state_file(&dir.path().join("OpenAirCast")).exists());
}

/// Replace seam that always fails after the temporary file has been written
/// and synced — simulates a failed atomic replacement.
struct FailingReplacer;

impl StateReplacer for FailingReplacer {
    fn replace(&self, _temporary: &Path, _destination: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("simulated replace failure"))
    }
}

#[tokio::test]
async fn save_is_atomic_replace_survives_simulated_failure() {
    let dir = tempfile::tempdir().unwrap();
    let working = FileStore::new(dir.path().to_path_buf(), None);
    working.save(&sample_state()).await.unwrap();

    let failing =
        FileStore::with_replacer(dir.path().to_path_buf(), None, Arc::new(FailingReplacer));
    let mut changed = sample_state();
    changed.muted = true;

    let outcome = failing.save(&changed).await;

    assert!(
        matches!(outcome, Err(PersistError::Replace(_))),
        "{outcome:?}"
    );
    let on_disk: PersistedStateV1 =
        serde_json::from_slice(&std::fs::read(state_file(dir.path())).unwrap()).unwrap();
    assert!(!on_disk.muted, "previous valid state survives untouched");
    let leftovers = dir
        .path()
        .read_dir()
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0, "failed save removes only its temp file");
}
