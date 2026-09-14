//! Versioned device-state persistence: schema v1, schema v2 (calibration),
//! legacy volume migration, validation, deterministic serialization, and
//! atomic replacement.
//!
//! This module owns `%APPDATA%\OpenAirCast\state-v1.json` exclusively. Shell
//! presentation preferences stay in the shell's own settings store; pairing
//! identities, hotkeys, window state, secrets, and protocol payloads never
//! enter this schema. Every filesystem call runs behind a
//! [`tokio::task::spawn_blocking`] boundary so no async executor thread ever
//! blocks on disk I/O.
//!
//! Schema migration: calibration is carried by a full [`PersistedStateV2`]
//! document (`version: 2`) that adds exactly one required `calibration` key
//! ([`CalibrationStateV2`]) on top of every schema-v1 field. Schema-v1
//! documents load unchanged ([`FileStore::load_calibration`] reports `None`);
//! writing a calibration section upgrades the document to version 2 through
//! the same temporary-file flush-and-replace machinery, preserving all
//! predecessor values. A document this build cannot parse -- an unknown
//! higher version, or a version-2 document that violates its own schema -- is
//! quarantined beside the state file and reported as recovered defaults, the
//! same treatment unrecognizable content gets: a launch that refuses to
//! complete would refuse identically every time afterwards. Version 1 never
//! carries a calibration section. Because [`PersistedStateV1`]
//! itself stays byte-for-byte identical to its schema-v1 contract, existing
//! readers of the version-1 shape keep working; [`FileStore::load`] projects
//! a version-2 document back onto that shape (normalized to `version: 1`),
//! while [`FileStore::load_calibration`] exposes the added section.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::backend::model::{
    AudioEndpointPreference, LatencyPreset, ReceiverId, SavedGroup, Volume,
};

/// Name of the persisted device-state file inside the state directory.
const STATE_FILE_NAME: &str = "state-v1.json";

/// Errors raised while loading or saving device state.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// The state directory could not be created.
    #[error("could not create the state directory: {0}")]
    CreateDirectory(std::io::Error),
    /// The state could not be serialized to JSON.
    #[error("could not serialize device state: {0}")]
    Serialize(serde_json::Error),
    /// The temporary file could not be written.
    #[error("could not write the temporary state file: {0}")]
    WriteTemporary(std::io::Error),
    /// The temporary file could not be flushed to disk.
    #[error("could not flush the temporary state file: {0}")]
    Sync(std::io::Error),
    /// The atomic replacement of the state file failed.
    #[error("could not replace the state file: {0}")]
    Replace(String),
    /// Persisted content violated schema-v1 invariants.
    #[error("persisted state is invalid: {0}")]
    Invalid(String),
    /// The state file could not be read or quarantined.
    #[error("could not read or quarantine the state file: {0}")]
    Io(std::io::Error),
    /// A blocking persistence worker panicked before completing.
    #[error("persistence worker failed: {0}")]
    Join(tokio::task::JoinError),
}

impl From<serde_json::Error> for PersistError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialize(value)
    }
}

impl From<tokio::task::JoinError> for PersistError {
    fn from(value: tokio::task::JoinError) -> Self {
        Self::Join(value)
    }
}

/// Calibration section persisted inside the device-state document
/// (introduced by schema version 2).
///
/// Stored values are the raw user-requested signed relative delays keyed by
/// stable [`ReceiverId`]; validation against an active selection happens at
/// apply time, not at load time.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalibrationStateV2 {
    /// Reference receiver the relative delays are judged against, if chosen.
    #[serde(default)]
    pub reference: Option<ReceiverId>,
    /// Signed requested relative delay per receiver in nanoseconds.
    #[serde(default)]
    pub delays_ns: BTreeMap<ReceiverId, i64>,
}

/// The complete persisted device state, schema version 1.
///
/// Field order and names are part of the on-disk contract; new fields require
/// a new schema version instead of mutating this one. The schema-v2
/// calibration addition therefore lives in [`PersistedStateV2`] only.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersistedStateV1 {
    /// Schema version; exactly `1`.
    pub version: u32,
    /// Validated master volume.
    pub master_volume: Volume,
    /// Whether output is muted; volumes are preserved while muted.
    pub muted: bool,
    /// Per-receiver balance levels keyed by stable identity.
    pub receiver_levels: BTreeMap<ReceiverId, Volume>,
    /// User-saved receiver groups.
    pub saved_groups: Vec<SavedGroup>,
    /// Committed desired membership restored on launch.
    pub last_desired_members: BTreeSet<ReceiverId>,
    /// Windows render endpoint preference.
    pub audio_endpoint: AudioEndpointPreference,
    /// Selected latency preset; must resolve to an enabled configuration.
    pub latency_preset: LatencyPreset,
    /// Whether startup may begin streaming after safe initial discovery.
    pub auto_connect: bool,
}

impl Default for PersistedStateV1 {
    fn default() -> Self {
        Self {
            version: 1,
            master_volume: Volume::DEFAULT_MASTER,
            muted: false,
            receiver_levels: BTreeMap::new(),
            saved_groups: Vec::new(),
            last_desired_members: BTreeSet::new(),
            audio_endpoint: AudioEndpointPreference::default(),
            latency_preset: LatencyPreset::default(),
            auto_connect: false,
        }
    }
}

/// The complete persisted device state, schema version 2.
///
/// Adds exactly one required field to the schema-v1 document: the manual
/// calibration section. Version 1 migrates to an empty/zero profile; every
/// predecessor field keeps its name, order, and value semantics.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersistedStateV2 {
    /// Schema version; exactly `2`.
    pub version: u32,
    /// Validated master volume.
    pub master_volume: Volume,
    /// Whether output is muted; volumes are preserved while muted.
    pub muted: bool,
    /// Per-receiver balance levels keyed by stable identity.
    pub receiver_levels: BTreeMap<ReceiverId, Volume>,
    /// User-saved receiver groups.
    pub saved_groups: Vec<SavedGroup>,
    /// Committed desired membership restored on launch.
    pub last_desired_members: BTreeSet<ReceiverId>,
    /// Windows render endpoint preference.
    pub audio_endpoint: AudioEndpointPreference,
    /// Selected latency preset; must resolve to an enabled configuration.
    pub latency_preset: LatencyPreset,
    /// Whether startup may begin streaming after safe initial discovery.
    pub auto_connect: bool,
    /// Manual calibration profile (required in schema-v2 documents).
    pub calibration: CalibrationStateV2,
}

impl Default for PersistedStateV2 {
    fn default() -> Self {
        Self {
            version: 2,
            master_volume: Volume::DEFAULT_MASTER,
            muted: false,
            receiver_levels: BTreeMap::new(),
            saved_groups: Vec::new(),
            last_desired_members: BTreeSet::new(),
            audio_endpoint: AudioEndpointPreference::default(),
            latency_preset: LatencyPreset::default(),
            auto_connect: false,
            calibration: CalibrationStateV2::default(),
        }
    }
}

impl From<&PersistedStateV1> for PersistedStateV2 {
    fn from(state: &PersistedStateV1) -> Self {
        Self {
            version: 2,
            master_volume: state.master_volume,
            muted: state.muted,
            receiver_levels: state.receiver_levels.clone(),
            saved_groups: state.saved_groups.clone(),
            last_desired_members: state.last_desired_members.clone(),
            audio_endpoint: state.audio_endpoint.clone(),
            latency_preset: state.latency_preset,
            auto_connect: state.auto_connect,
            calibration: CalibrationStateV2::default(),
        }
    }
}

impl PersistedStateV2 {
    /// Projects a schema-v2 document onto the predecessor-compatible
    /// schema-v1 shape so existing [`LoadOutcome`] consumers stay unchanged;
    /// the calibration section is read separately via
    /// [`FileStore::load_calibration`].
    pub fn into_v1_projection(self) -> PersistedStateV1 {
        PersistedStateV1 {
            version: 1,
            master_volume: self.master_volume,
            muted: self.muted,
            receiver_levels: self.receiver_levels,
            saved_groups: self.saved_groups,
            last_desired_members: self.last_desired_members,
            audio_endpoint: self.audio_endpoint,
            latency_preset: self.latency_preset,
            auto_connect: self.auto_connect,
        }
    }

    /// Checks every schema-v2 invariant beyond what value types enforce.
    pub fn validate(&self) -> Result<(), PersistError> {
        if self.version != 2 {
            return Err(PersistError::Invalid(format!(
                "unsupported schema version {}",
                self.version
            )));
        }
        validate_preferences(self.latency_preset, &self.saved_groups)
    }
}

/// Validates the preferences shared by both schema versions.
fn validate_preferences(
    latency_preset: LatencyPreset,
    saved_groups: &[SavedGroup],
) -> Result<(), PersistError> {
    if latency_preset.enabled_config().is_none() {
        return Err(PersistError::Invalid(
            "latency preset is not enabled".to_owned(),
        ));
    }

    let mut seen_names = std::collections::HashSet::new();
    for group in saved_groups {
        let trimmed = group.name.trim();
        if trimmed.is_empty() || trimmed != group.name {
            return Err(PersistError::Invalid(format!(
                "group name must be trimmed and non-empty: {:?}",
                group.name
            )));
        }
        if !seen_names.insert(trimmed.to_lowercase()) {
            return Err(PersistError::Invalid(format!(
                "duplicate group name: {trimmed}"
            )));
        }
        if group.members.is_empty() {
            return Err(PersistError::Invalid(format!(
                "group {trimmed} has no members"
            )));
        }
        let mut seen_members = std::collections::HashSet::new();
        for member in &group.members {
            if !seen_members.insert(member.receiver) {
                return Err(PersistError::Invalid(format!(
                    "group {trimmed} lists receiver {} more than once",
                    member.receiver
                )));
            }
        }
    }
    Ok(())
}

impl PersistedStateV1 {
    /// Checks every schema-v1 invariant beyond what value types enforce.
    ///
    /// Violations mean the file was written by an incompatible producer;
    /// callers must not silently repair them.
    pub fn validate(&self) -> Result<(), PersistError> {
        if self.version != 1 {
            return Err(PersistError::Invalid(format!(
                "unsupported schema version {}",
                self.version
            )));
        }
        validate_preferences(self.latency_preset, &self.saved_groups)
    }
}

/// Outcome of [`StateStore::load`].
#[derive(Clone, Debug, PartialEq)]
pub enum LoadOutcome {
    /// The state file existed and passed validation.
    Loaded(PersistedStateV1),
    /// No state exists yet; defaults apply.
    FreshDefaults,
    /// The state file was corrupt or unreadable and was renamed with a
    /// `.corrupt-<unix_ms>` suffix; defaults apply.
    RecoveredCorrupt {
        /// Preserved evidence file next to the original location.
        quarantined_path: PathBuf,
    },
    /// No state file existed and the legacy `volume.txt` seeded
    /// `master_volume`; the legacy file is kept untouched and the seeded
    /// snapshot was written under the new schema so migration happens once.
    MigratedLegacyVolume {
        /// Defaults with the seeded master volume.
        state: PersistedStateV1,
        /// Legacy source file that remains in place.
        legacy_source: PathBuf,
    },
}

impl LoadOutcome {
    /// Consumes the outcome and yields the effective persisted state.
    pub fn into_state(self) -> PersistedStateV1 {
        match self {
            Self::Loaded(state) | Self::MigratedLegacyVolume { state, .. } => state,
            Self::FreshDefaults | Self::RecoveredCorrupt { .. } => PersistedStateV1::default(),
        }
    }
}

/// Storage backend for validated device state.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Loads persisted state, migrating or recovering as needed.
    ///
    /// Returns an error only for unrecoverable conditions; corrupt files are
    /// quarantined and reported as defaults through the outcome.
    async fn load(&self) -> Result<LoadOutcome, PersistError>;

    /// Atomically persists the given validated state.
    async fn save(&self, state: &PersistedStateV1) -> Result<(), PersistError>;

    /// Loads the persisted calibration section, or `None` when the document
    /// predates schema version 2.
    ///
    /// Part of the trait rather than of one implementation: the backend actor
    /// only ever sees `dyn StateStore`, and a store that silently answered
    /// "no calibration" would let a persisted alignment disappear on the next
    /// launch without anything failing.
    ///
    /// For the same reason `None` means only "this document carries no
    /// calibration". A document that could not be read is an error, which the
    /// caller turns into a launch that runs unaligned *and says so* rather
    /// than one that quietly forgets.
    async fn load_calibration(&self) -> Result<Option<CalibrationStateV2>, PersistError>;

    /// Atomically persists (or clears) the calibration section, promoting the
    /// document to schema version 2 and preserving every predecessor field.
    ///
    /// Refuses rather than substituting defaults when the current document
    /// cannot be read: rewriting one blind would replace every predecessor
    /// field -- volume, groups, selection, endpoint, latency, auto-connect --
    /// with defaults.
    async fn save_calibration(
        &self,
        calibration: Option<CalibrationStateV2>,
    ) -> Result<(), PersistError>;
}

/// Replaces `temporary` with `destination` atomically.
///
/// Injection seam for tests and future backends; production uses
/// [`OsStateReplacer`].
pub trait StateReplacer: Send + Sync {
    /// Moves the fully synced temporary file onto the destination.
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()>;
}

/// Production replacer: `MoveFileExW` with replace-existing and write-through
/// semantics on Windows, plain rename elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsStateReplacer;

impl StateReplacer for OsStateReplacer {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()> {
        replace_file(temporary, destination)
    }
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let wide_from: Vec<u16> = from
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let wide_to: Vec<u16> = to
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let replaced = unsafe {
            MoveFileExW(
                wide_from.as_ptr(),
                wide_to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

/// File-backed [`StateStore`] with injected locations.
///
/// All filesystem work happens synchronously inside dedicated blocking tasks;
/// the async methods only marshal results.
#[derive(Clone)]
pub struct FileStore {
    state_directory: PathBuf,
    legacy_volume_path: Option<PathBuf>,
    replacer: std::sync::Arc<dyn StateReplacer>,
}

impl FileStore {
    /// Creates a store rooted at `state_directory` with optional legacy
    /// migration source, using the platform atomic replacer.
    pub fn new(state_directory: PathBuf, legacy_volume_path: Option<PathBuf>) -> Self {
        Self {
            state_directory,
            legacy_volume_path,
            replacer: std::sync::Arc::new(OsStateReplacer),
        }
    }

    /// Creates a store with an injected replacement strategy.
    pub fn with_replacer(
        state_directory: PathBuf,
        legacy_volume_path: Option<PathBuf>,
        replacer: std::sync::Arc<dyn StateReplacer>,
    ) -> Self {
        Self {
            state_directory,
            legacy_volume_path,
            replacer,
        }
    }

    /// Builds the standard layout beneath an application-data root:
    /// `<root>/OpenAirCast` for state and `<root>/HomePodCast/volume.txt` as
    /// the legacy migration source.
    pub fn for_app_data_root(app_data_root: &Path) -> Self {
        Self::new(
            app_data_root.join("OpenAirCast"),
            Some(app_data_root.join("HomePodCast").join("volume.txt")),
        )
    }

    /// Builds the production store from `%APPDATA%`, if that variable is set.
    pub fn from_environment() -> Option<Self> {
        std::env::var_os("APPDATA")
            .as_deref()
            .map(PathBuf::from)
            .map(|root| Self::for_app_data_root(&root))
    }

    /// Full path of the schema-v1 state file.
    pub fn state_file(&self) -> PathBuf {
        self.state_directory.join(STATE_FILE_NAME)
    }

    /// Reads the state document, separating "there is none" from "it could
    /// not be read".
    ///
    /// One place makes that distinction for all four callers. It used to be
    /// made four times over with three different answers -- the load path
    /// quarantined, the calibration read reported "no calibration", and both
    /// write paths substituted a default document -- so what an unreadable
    /// file meant depended on which caller reached it first, and two of those
    /// answers silently discarded durable state.
    fn read_state_bytes(&self) -> Result<Option<Vec<u8>>, std::io::Error> {
        match std::fs::read(self.state_file()) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Synchronous load implementation executed on the blocking pool.
    fn load_blocking(&self) -> Result<LoadOutcome, PersistError> {
        match self.read_state_bytes() {
            Ok(Some(bytes)) => match parse_state_document(&bytes) {
                Ok(StateDocument::V1(state)) => {
                    state.validate()?;
                    Ok(LoadOutcome::Loaded(state))
                }
                Ok(StateDocument::V2(state)) => {
                    state.validate()?;
                    Ok(LoadOutcome::Loaded(state.into_v1_projection()))
                }
                // Neither a document from a newer build nor a version-2
                // document that violates its own schema can be read here, and
                // a document this loader cannot read must not become a
                // launch that never completes: the app would refuse to start
                // again on every subsequent try, with no way out of the app
                // itself. Quarantining moves the bytes aside under a
                // `.corrupt-<millis>` name -- nothing is deleted, so the
                // evidence survives for a newer build or a bug report -- and
                // reports defaults, exactly as unrecognizable content does.
                Err(ParseStateError::UnknownVersion(_) | ParseStateError::MalformedSchemaV2(_))
                | Err(ParseStateError::Unrecognizable) => self.quarantine_corrupt(),
            },
            Ok(None) => Ok(self.load_missing()),
            // Unreadable counts as corrupt: quarantine and fall back.
            Err(_) => self.quarantine_corrupt(),
        }
    }

    /// Synchronous calibration-section read executed on the blocking pool.
    ///
    /// Read-only: this path never quarantines. A document that parses but
    /// predates schema version 2 reports no persisted calibration (`None`).
    ///
    /// A file that could not be read at all is reported as an error, not as
    /// `None`: "there is no calibration" and "the calibration could not be
    /// read" lead to different handling, and collapsing them here would let a
    /// persisted alignment vanish from a launch with nothing to show for it.
    /// Only a missing file is genuinely the first case. Content this loader
    /// cannot parse cannot reach here in the startup sequence -- [`Self::load`]
    /// runs first and quarantines it.
    fn load_calibration_blocking(&self) -> Result<Option<CalibrationStateV2>, PersistError> {
        let bytes = match self.read_state_bytes().map_err(PersistError::Io)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        match parse_state_document(&bytes) {
            Ok(StateDocument::V2(state)) => Ok(Some(state.calibration)),
            _ => Ok(None),
        }
    }

    /// Synchronous calibration-section write executed on the blocking pool.
    ///
    /// Reads the current base fields from disk (schema-v1 or schema-v2,
    /// defaults when absent), merges in `calibration`, and replaces the
    /// document with a validated schema-v2 version through the same atomic
    /// temporary-file flush-and-replace machinery as [`FileStore::save`].
    fn save_calibration_blocking(
        &self,
        calibration: Option<CalibrationStateV2>,
    ) -> Result<(), PersistError> {
        // Only a missing file means "there is nothing to merge into". Every
        // other read failure is reported: defaulting on a file that exists
        // but could not be read would replace master volume, mute,
        // per-receiver levels, saved groups, the last desired selection, the
        // audio endpoint, the latency preset and auto-connect with defaults
        // -- a total loss of device state bought by one calibration apply. A
        // refused write is already handled cleanly by the caller, which
        // reports it and applies nothing.
        let mut migrated = match self.read_state_bytes().map_err(PersistError::Io)? {
            Some(bytes) => match parse_state_document(&bytes) {
                Ok(StateDocument::V1(state)) => PersistedStateV2::from(&state),
                Ok(StateDocument::V2(state)) => state,
                Err(ParseStateError::UnknownVersion(version)) => {
                    return Err(PersistError::Invalid(format!(
                        "unsupported schema version {version}"
                    )));
                }
                Err(ParseStateError::MalformedSchemaV2(error)) => {
                    return Err(PersistError::Invalid(format!(
                        "schema-v2 state is invalid: {error}"
                    )));
                }
                // Unrecognizable content is quarantined before being replaced
                // so the evidence survives the upgrade.
                Err(ParseStateError::Unrecognizable) => {
                    self.quarantine_corrupt()?;
                    PersistedStateV2::default()
                }
            },
            None => PersistedStateV2::default(),
        };
        migrated.calibration = calibration.unwrap_or_default();
        self.write_atomic_sync_v2(&migrated)
    }

    /// Handles the missing-state case: legacy volume seeding or fresh
    /// defaults.
    fn load_missing(&self) -> LoadOutcome {
        let legacy_path = match &self.legacy_volume_path {
            Some(path) => path,
            None => return LoadOutcome::FreshDefaults,
        };
        let raw = match std::fs::read_to_string(legacy_path) {
            Ok(raw) => raw,
            Err(_) => return LoadOutcome::FreshDefaults,
        };
        let volume = match parse_legacy_volume(&raw) {
            Some(volume) => volume,
            None => return LoadOutcome::FreshDefaults,
        };

        let state = PersistedStateV1 {
            master_volume: volume,
            ..PersistedStateV1::default()
        };
        // Migration-once semantics: persist the seeded snapshot immediately so
        // subsequent launches load schema-v1 instead of migrating again. The
        // legacy file itself is never modified or deleted.
        if let Err(error) = self.write_atomic_sync(&state) {
            tracing::warn!("could not persist migrated legacy volume: {error}");
        }
        LoadOutcome::MigratedLegacyVolume {
            state,
            legacy_source: legacy_path.clone(),
        }
    }

    /// Renames the unreadable/corrupt state file aside and reports defaults.
    fn quarantine_corrupt(&self) -> Result<LoadOutcome, PersistError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let source = self.state_file();
        let quarantined = source.with_extension(format!("json.corrupt-{stamp}"));
        std::fs::rename(&source, &quarantined).map_err(PersistError::Io)?;
        Ok(LoadOutcome::RecoveredCorrupt {
            quarantined_path: quarantined,
        })
    }

    /// Synchronous atomic write used by both `save` and legacy migration.
    ///
    /// If the file on disk currently carries a schema-v2 calibration section,
    /// it is carried forward into the written document so ordinary volume or
    /// group saves never drop persisted calibration.
    fn write_atomic_sync(&self, state: &PersistedStateV1) -> Result<(), PersistError> {
        state.validate()?;
        // A file that exists but could not be read is NOT evidence that there
        // is no calibration section. Treating it as such would write a
        // schema-v1 document over it, so an unrelated volume or group save
        // after one transient read failure would silently erase the alignment
        // the user tuned. Abort before writing anything; the caller has a
        // clean failure path and the document stays as it was.
        let carried_calibration = match self.read_state_bytes().map_err(PersistError::Io)? {
            Some(bytes) => match parse_state_document(&bytes) {
                Ok(StateDocument::V2(existing)) => Some(existing.calibration),
                _ => None,
            },
            None => None,
        };
        let bytes = match carried_calibration {
            Some(calibration) => {
                let mut upgraded = PersistedStateV2::from(state);
                upgraded.calibration = calibration;
                serde_json::to_vec_pretty(&upgraded)?
            }
            None => serde_json::to_vec_pretty(state)?,
        };
        self.replace_atomically(&bytes)
    }

    /// Synchronous atomic write of a full schema-v2 document.
    fn write_atomic_sync_v2(&self, state: &PersistedStateV2) -> Result<(), PersistError> {
        state.validate()?;
        let bytes = serde_json::to_vec_pretty(state)?;
        self.replace_atomically(&bytes)
    }

    /// Writes `bytes` to a synced temporary file and atomically replaces the
    /// state file with it.
    fn replace_atomically(&self, bytes: &[u8]) -> Result<(), PersistError> {
        std::fs::create_dir_all(&self.state_directory).map_err(PersistError::CreateDirectory)?;
        let destination = self.state_file();
        let temporary = destination.with_extension("json.tmp");
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)
                .map_err(PersistError::WriteTemporary)?;
            std::io::Write::write_all(&mut file, bytes).map_err(PersistError::WriteTemporary)?;
            std::io::Write::flush(&mut file).map_err(PersistError::Sync)?;
            file.sync_all().map_err(PersistError::Sync)?;
        }

        if let Err(error) = self.replacer.replace(&temporary, &destination) {
            // Remove only the exact temp file; the previous destination stays
            // untouched as the last known-good copy.
            let _ = std::fs::remove_file(&temporary);
            return Err(PersistError::Replace(error.to_string()));
        }
        Ok(())
    }
}

/// On-disk device-state document shape detected while loading.
enum StateDocument {
    V1(PersistedStateV1),
    V2(PersistedStateV2),
}

/// Failure modes of [`parse_state_document`].
enum ParseStateError {
    /// The bytes are not JSON or do not name any schema version.
    Unrecognizable,
    /// The document names a version this build does not know.
    UnknownVersion(u32),
    /// The document names schema version 2 but violates its shape.
    MalformedSchemaV2(serde_json::Error),
}

#[derive(serde::Deserialize)]
struct VersionProbe {
    version: u32,
}

/// Detects the schema version first, then parses into the matching document
/// type so unknown versions stay distinguishable from random corruption.
fn parse_state_document(bytes: &[u8]) -> Result<StateDocument, ParseStateError> {
    let probe = serde_json::from_slice::<VersionProbe>(bytes)
        .map_err(|_| ParseStateError::Unrecognizable)?;
    match probe.version {
        1 => serde_json::from_slice::<PersistedStateV1>(bytes)
            .map(StateDocument::V1)
            .map_err(|_| ParseStateError::Unrecognizable),
        2 => serde_json::from_slice::<PersistedStateV2>(bytes)
            .map(StateDocument::V2)
            .map_err(ParseStateError::MalformedSchemaV2),
        other => Err(ParseStateError::UnknownVersion(other)),
    }
}

#[async_trait]
impl StateStore for FileStore {
    async fn load(&self) -> Result<LoadOutcome, PersistError> {
        let this = self.clone();
        let result = tokio::task::spawn_blocking(move || this.load_blocking()).await?;
        result
    }

    async fn save(&self, state: &PersistedStateV1) -> Result<(), PersistError> {
        let this = self.clone();
        let owned = state.clone();
        let result = tokio::task::spawn_blocking(move || this.write_atomic_sync(&owned)).await?;
        result
    }

    /// Returns `None` unless the state file is a valid schema-v2 document;
    /// this read-only path never quarantines or mutates anything. Calibration
    /// lives outside shell settings and only inside this device-state file.
    async fn load_calibration(&self) -> Result<Option<CalibrationStateV2>, PersistError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.load_calibration_blocking()).await?
    }

    /// The predecessor device-state fields are read from disk, merged with
    /// the given section, validated, and written as a schema-v2 document via
    /// the same temporary-file flush-and-replace machinery as
    /// [`StateStore::save`]. `None` stores an empty/zero profile — the
    /// migration target of every version-1 file.
    async fn save_calibration(
        &self,
        calibration: Option<CalibrationStateV2>,
    ) -> Result<(), PersistError> {
        let this = self.clone();
        let owned = calibration;
        tokio::task::spawn_blocking(move || this.save_calibration_blocking(owned)).await?
    }
}

/// Parses and clamps a legacy `volume.txt` value.
///
/// Returns `None` for unparseable or non-finite content; otherwise clamps
/// into `0.0..=1.0` and validates as a [`Volume`].
fn parse_legacy_volume(raw: &str) -> Option<Volume> {
    let parsed: f32 = raw.trim().parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    Volume::new(parsed.clamp(0.0, 1.0)).ok()
}

/// Computes the effective linear volume for one receiver.
///
/// `muted` applies silence while preserving stored values; otherwise the
/// master and receiver level are multiplied and clamped to `0.0..=1.0`.
pub fn effective_volume(master: Volume, level: Volume, muted: bool) -> Volume {
    if muted {
        return Volume::MUTED;
    }
    Volume::new(master.get() * level.get()).unwrap_or(Volume::MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod default_state {
        use super::*;

        #[test]
        fn defaults_match_the_specified_baseline() {
            let state = PersistedStateV1::default();
            assert_eq!(state.version, 1);
            assert_eq!(state.master_volume, Volume::DEFAULT_MASTER);
            assert!(!state.muted);
            assert!(state.receiver_levels.is_empty());
            assert!(state.saved_groups.is_empty());
            assert!(state.last_desired_members.is_empty());
            assert_eq!(state.audio_endpoint, AudioEndpointPreference::SystemDefault);
            assert_eq!(state.latency_preset, LatencyPreset::Normal);
            assert!(!state.auto_connect);
            assert!(state.validate().is_ok());
        }
    }

    mod validation {
        use super::*;
        use crate::backend::model::{SavedGroupId, SavedGroupMember};

        fn valid_group(name: &str) -> SavedGroup {
            SavedGroup {
                id: SavedGroupId::generate(),
                name: name.to_owned(),
                members: vec![SavedGroupMember {
                    receiver: ReceiverId::from_storage_key("000000000001").unwrap(),
                    last_known_name: "Wohnzimmer".into(),
                    level: Volume::UNITY,
                }],
            }
        }

        fn base_with(mutate: impl FnOnce(&mut PersistedStateV1)) -> PersistError {
            let mut state = PersistedStateV1::default();
            state.saved_groups.push(valid_group("Abends"));
            mutate(&mut state);
            state.validate().expect_err("mutation must break validity")
        }

        #[test]
        fn rejects_wrong_schema_version() {
            let state = PersistedStateV1 {
                version: 2,
                ..PersistedStateV1::default()
            };
            assert!(matches!(
                state.validate(),
                Err(PersistError::Invalid(message)) if message.contains("version")
            ));
        }

        #[test]
        fn rejects_disabled_latency_preset() {
            assert!(matches!(
                base_with(|state| state.latency_preset = LatencyPreset::Low),
                PersistError::Invalid(_)
            ));
        }

        #[test]
        fn rejects_untrimmed_blank_and_duplicate_group_names() {
            assert!(matches!(
                base_with(|state| state.saved_groups.push(valid_group(" Abends"))),
                PersistError::Invalid(_)
            ));
            assert!(matches!(
                base_with(|state| state.saved_groups.push(valid_group("  "))),
                PersistError::Invalid(_)
            ));
            assert!(matches!(
                base_with(|state| state.saved_groups.push(valid_group("abends"))),
                PersistError::Invalid(_)
            ));
        }

        #[test]
        fn rejects_empty_and_duplicate_member_lists() {
            let empty = SavedGroup {
                id: SavedGroupId::generate(),
                name: "Leer".into(),
                members: Vec::new(),
            };
            assert!(matches!(
                base_with(|state| state.saved_groups.push(empty)),
                PersistError::Invalid(_)
            ));

            let duplicate_member = SavedGroup {
                id: SavedGroupId::generate(),
                name: "Doppelt".into(),
                members: vec![
                    SavedGroupMember {
                        receiver: ReceiverId::from_storage_key("000000000001").unwrap(),
                        last_known_name: "A".into(),
                        level: Volume::UNITY,
                    },
                    SavedGroupMember {
                        receiver: ReceiverId::from_storage_key("000000000001").unwrap(),
                        last_known_name: "B".into(),
                        level: Volume::UNITY,
                    },
                ],
            };
            assert!(matches!(
                base_with(|state| state.saved_groups.push(duplicate_member)),
                PersistError::Invalid(_)
            ));
        }
    }

    mod legacy {
        use super::*;

        #[test]
        fn parses_finite_values_and_clamps_into_range() {
            assert_eq!(parse_legacy_volume("0.375").unwrap().get(), 0.375);
            assert_eq!(parse_legacy_volume("7").unwrap().get(), 1.0);
            assert_eq!(parse_legacy_volume("-3").unwrap().get(), 0.0);
            assert_eq!(parse_legacy_volume(" 0.5\n").unwrap().get(), 0.5);
        }

        #[test]
        fn rejects_garbage_and_non_finite_content() {
            assert_eq!(parse_legacy_volume("garbage"), None);
            assert_eq!(parse_legacy_volume(""), None);
            assert_eq!(parse_legacy_volume("nan"), None);
            assert_eq!(parse_legacy_volume("inf"), None);
        }
    }

    mod effective {
        use super::*;

        #[test]
        fn mutes_to_silence_and_multiplies_when_live() {
            let master = Volume::new(0.5).unwrap();
            let level = Volume::new(0.4).unwrap();
            assert_eq!(effective_volume(master, level, false).get(), 0.2);
            assert_eq!(effective_volume(master, level, true), Volume::MUTED);
            assert_eq!(effective_volume(Volume::UNITY, level, false), level);
        }
    }
}
