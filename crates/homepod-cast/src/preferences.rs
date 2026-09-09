//! Schema-v2 shell preferences: path policy, schema migration, legacy hotkey
//! migration, deterministic serialization, atomic replacement, and a bounded
//! debounced persistence worker.
//!
//! Only shell presentation preferences are stored here. Device volume,
//! receiver membership, stream state, addresses, generations, logs, and
//! secrets never enter `settings.json`; the legacy device volume stays owned
//! by the device service.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::app::{
    AppEvent, CorrectiveAction, GenerationId, HotkeyBinding, LocalePreference, NoticeCode, Page,
    Preferences, PreferencesEvent, Severity, ThemePreference, UserNotice, WindowGeometry,
    PREFERENCES_SCHEMA_VERSION,
};

/// Delay before flushing an ordinary preference change.
pub(crate) const STANDARD_DEBOUNCE_MS: u64 = 500;
/// Extra delay that keeps rapid window-geometry churn from hammering the disk.
pub(crate) const GEOMETRY_DEBOUNCE_MS: u64 = 1000;
/// Capacity of the worker command queue.
const COMMAND_CHANNEL_CAPACITY: usize = 16;

/// Resolves canonical and legacy file locations from one injected app-data
/// root; production resolves `%APPDATA%` exactly once before startup.
#[derive(Clone, Debug)]
pub struct PreferencesPaths {
    app_data_root: PathBuf,
}

impl PreferencesPaths {
    pub fn new(app_data_root: PathBuf) -> Self {
        Self { app_data_root }
    }

    pub fn settings_dir(&self) -> PathBuf {
        self.app_data_root.join("OpenAirCast")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.settings_dir().join("settings.json")
    }

    pub fn legacy_hotkey_dir(&self) -> PathBuf {
        self.app_data_root.join("HomePodCast")
    }

    pub fn legacy_hotkey_file(&self) -> PathBuf {
        self.legacy_hotkey_dir().join("hotkey.txt")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreferencesError {
    /// Refused before any filesystem work: only the current schema is written.
    #[error("refusing to write preferences with schema version {found}")]
    InvalidSchemaVersion { found: u32 },
    #[error("could not create the settings directory: {0}")]
    CreateDirectory(std::io::Error),
    #[error("could not serialize preferences: {0}")]
    Serialize(serde_json::Error),
    #[error("could not write the temporary settings file: {0}")]
    WriteTemporary(std::io::Error),
    #[error("could not flush the temporary settings file: {0}")]
    Sync(std::io::Error),
    #[error("could not replace the settings file: {0}")]
    Replace(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreferencesUnavailable {
    #[error("preferences queue is busy")]
    Busy,
    #[error("preferences worker is closed")]
    Closed,
}

/// Load outcome: validated preferences plus at most one non-fatal notice.
pub struct PreferencesLoad {
    pub value: Preferences,
    pub notice: Option<UserNotice>,
}

fn invalid_preferences_notice(summary: &str) -> UserNotice {
    UserNotice {
        severity: Severity::Warning,
        code: NoticeCode::PreferencesFailed,
        summary: summary.to_string(),
        action: Some(CorrectiveAction::OpenSettings),
    }
}

const LEGACY_INVALID_NOTICE: &str =
    "The previously saved shortcut was ignored because it is invalid.";
const CORRUPT_JSON_NOTICE: &str =
    "Saved settings could not be read and were reset to their defaults.";

/// Parses the legacy `hotkey.txt` format: two comma-separated decimal `u32`
/// values (modifiers, virtual key) with an enabled non-zero key.
pub(crate) fn parse_legacy_hotkey(contents: &str) -> Option<HotkeyBinding> {
    let mut parts = contents.trim().split(',');
    let modifiers = parts.next()?.trim().parse::<u32>().ok()?;
    let virtual_key = parts.next()?.trim().parse::<u32>().ok()?;
    if parts.next().is_some() || virtual_key == 0 {
        return None;
    }

    Some(HotkeyBinding {
        enabled: true,
        modifiers,
        virtual_key,
    })
}

/// Reads nothing but the version tag, so the file can be routed to the right
/// record before anything else about its shape is assumed.
///
/// Deliberately tolerant of unknown fields: every stored schema has fields this
/// probe does not know, and refusing them here would reject files that are
/// perfectly readable one step later.
#[derive(serde::Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// The retired version-one record, kept only so its files can be migrated.
///
/// Strict about unknown fields for the same reason [`Preferences`] is: a file
/// claiming version one but carrying newer fields is not a version-one file,
/// and guessing at it would silently discard whatever it really held.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferencesV1 {
    #[allow(dead_code)] // matched by the probe; kept so the record stays exact.
    schema_version: u32,
    hotkey: HotkeyBinding,
    theme: ThemePreference,
    window: WindowGeometry,
    last_page: Page,
    close_to_tray: bool,
    launch_at_startup: bool,
}

impl PreferencesV1 {
    /// Carries every version-one field forward unchanged and gives the two
    /// fields version two adds their documented starting values: the language
    /// follows Windows, and advanced information stays hidden.
    fn migrate(self) -> Preferences {
        Preferences {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            hotkey: self.hotkey,
            theme: self.theme,
            locale: LocalePreference::System,
            advanced_information: false,
            window: self.window,
            last_page: self.last_page,
            close_to_tray: self.close_to_tray,
            launch_at_startup: self.launch_at_startup,
        }
    }
}

/// Routes a settings file by its declared version.
///
/// `None` means "do not use this file": unreadable JSON, a version this build
/// has no record for, or a body that does not match the version it claims.
fn parse_settings(raw: &str) -> Option<Preferences> {
    let probe = serde_json::from_str::<SchemaProbe>(raw).ok()?;

    match probe.schema_version {
        1 => serde_json::from_str::<PreferencesV1>(raw)
            .ok()
            .map(PreferencesV1::migrate),
        PREFERENCES_SCHEMA_VERSION => serde_json::from_str::<Preferences>(raw).ok(),
        _ => None,
    }
}

/// Synchronous repository used by startup loading, tests, and the worker.
#[derive(Clone)]
pub struct PreferencesRepository {
    paths: PreferencesPaths,
}

impl PreferencesRepository {
    pub fn new(paths: PreferencesPaths) -> Self {
        Self { paths }
    }

    /// Loads the settings file when it carries a schema this build understands,
    /// else migrates the legacy hotkey file, else returns defaults with at most
    /// one non-fatal notice.
    ///
    /// Loading never writes. A migrated value stays in memory until the next
    /// ordinary save carries it to disk, so a read-only or otherwise unwritable
    /// profile still starts, and a crash right after startup leaves the old
    /// file intact for the previous build to read.
    pub fn load(&self) -> PreferencesLoad {
        if let Ok(raw) = std::fs::read_to_string(self.paths.settings_file()) {
            return match parse_settings(&raw) {
                Some(value) => PreferencesLoad {
                    value,
                    notice: None,
                },
                None => PreferencesLoad {
                    value: Preferences::default(),
                    notice: Some(invalid_preferences_notice(CORRUPT_JSON_NOTICE)),
                },
            };
        }

        if let Ok(raw) = std::fs::read_to_string(self.paths.legacy_hotkey_file()) {
            return match parse_legacy_hotkey(&raw) {
                Some(hotkey) => PreferencesLoad {
                    value: Preferences {
                        hotkey,
                        ..Preferences::default()
                    },
                    notice: None,
                },
                None => PreferencesLoad {
                    value: Preferences::default(),
                    notice: Some(invalid_preferences_notice(LEGACY_INVALID_NOTICE)),
                },
            };
        }

        PreferencesLoad {
            value: Preferences::default(),
            notice: None,
        }
    }

    /// Serializes to `settings.json.tmp`, flushes and syncs it, then replaces
    /// the final file atomically (`MoveFileExW` with replace + write-through
    /// on Windows). Any failure keeps the previous valid file in place.
    ///
    /// Only the current schema is ever written. A value carrying any other
    /// version is refused here, before a directory is created, a temporary file
    /// is opened, or the previous file is touched.
    pub fn save_atomic(&self, value: &Preferences) -> Result<(), PreferencesError> {
        if value.schema_version != PREFERENCES_SCHEMA_VERSION {
            return Err(PreferencesError::InvalidSchemaVersion {
                found: value.schema_version,
            });
        }

        std::fs::create_dir_all(self.paths.settings_dir())
            .map_err(PreferencesError::CreateDirectory)?;

        let serialized =
            serde_json::to_string_pretty(value).map_err(PreferencesError::Serialize)? + "\n";
        let temporary_path = self.paths.settings_dir().join("settings.json.tmp");
        {
            let mut temporary =
                std::fs::File::create(&temporary_path).map_err(PreferencesError::WriteTemporary)?;
            temporary
                .write_all(serialized.as_bytes())
                .map_err(PreferencesError::WriteTemporary)?;
            temporary.flush().map_err(PreferencesError::Sync)?;
            temporary.sync_all().map_err(PreferencesError::Sync)?;
        }

        replace_file(&temporary_path, &self.paths.settings_file())
    }
}

fn replace_file(from: &Path, to: &Path) -> Result<(), PreferencesError> {
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
            let _ = std::fs::remove_file(from);
            return Err(PreferencesError::Replace(format!(
                "MoveFileExW failed for {}",
                to.display()
            )));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        std::fs::rename(from, to).map_err(|error| PreferencesError::Replace(error.to_string()))
    }
}

/// Pure debounce bookkeeping over a caller-supplied millisecond timeline so
/// deadline math is testable without sleeping.
#[derive(Default)]
pub(crate) struct DebounceState {
    pending: Option<PendingSave>,
}

struct PendingSave {
    deadline_ms: u64,
    generation: GenerationId,
    value: Preferences,
}

impl DebounceState {
    /// Offers the newest value; returns the absolute millisecond deadline.
    ///
    /// A change that only moves window geometry relative to the pending or
    /// last committed value uses the long churn delay and restarts its timer.
    ///
    /// An offer whose generation is older than what is already pending is
    /// dropped whole -- value and timer alike. Keeping the newer generation
    /// while writing the older value would report the outcome of one change
    /// for the content of another, which is precisely the confusion the
    /// generation exists to prevent.
    pub(crate) fn offer(
        &mut self,
        now_ms: u64,
        generation: GenerationId,
        value: Preferences,
        reference: Option<&Preferences>,
    ) -> u64 {
        if let Some(pending) = self.pending.as_ref() {
            if generation.0 < pending.generation.0 {
                return pending.deadline_ms;
            }
        }

        let geometry_only = match self.pending.as_ref().map(|pending| &pending.value) {
            Some(pending_value) => differs_only_in_window(pending_value, &value),
            None => reference.is_some_and(|reference| differs_only_in_window(reference, &value)),
        };
        let delay = if geometry_only {
            GEOMETRY_DEBOUNCE_MS
        } else {
            STANDARD_DEBOUNCE_MS
        };
        let deadline_ms = now_ms + delay;
        self.pending = Some(PendingSave {
            deadline_ms,
            generation,
            value,
        });

        deadline_ms
    }

    pub(crate) fn peek_deadline(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.deadline_ms)
    }

    pub(crate) fn peek_pending(&self) -> Option<Preferences> {
        self.pending.as_ref().map(|pending| pending.value.clone())
    }

    pub(crate) fn take_due(&mut self, now_ms: u64) -> Option<(GenerationId, Preferences)> {
        let due = self
            .pending
            .as_ref()
            .is_some_and(|p| p.deadline_ms <= now_ms);
        if due {
            self.pending
                .take()
                .map(|pending| (pending.generation, pending.value))
        } else {
            None
        }
    }

    pub(crate) fn take_pending(&mut self) -> Option<(GenerationId, Preferences)> {
        self.pending
            .take()
            .map(|pending| (pending.generation, pending.value))
    }
}

fn differs_only_in_window(previous: &Preferences, next: &Preferences) -> bool {
    previous.hotkey == next.hotkey
        && previous.theme == next.theme
        && previous.locale == next.locale
        && previous.advanced_information == next.advanced_information
        && previous.last_page == next.last_page
        && previous.close_to_tray == next.close_to_tray
        && previous.launch_at_startup == next.launch_at_startup
}

pub(crate) enum PreferencesCommand {
    Save {
        generation: GenerationId,
        value: Preferences,
    },
    FlushAndShutdown {
        acknowledgement: SyncSender<()>,
    },
}

/// Bounded, non-blocking handle onto the debounced persistence worker.
#[derive(Clone)]
pub struct PreferencesWorkerHandle {
    tx: SyncSender<PreferencesCommand>,
    #[allow(dead_code)] // drained by join(); shutdown coordinator uses acks instead.
    join_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl PreferencesWorkerHandle {
    pub fn try_schedule(
        &self,
        generation: GenerationId,
        value: Preferences,
    ) -> Result<(), PreferencesUnavailable> {
        self.tx
            .try_send(PreferencesCommand::Save { generation, value })
            .map_err(map_unavailable)
    }

    /// Flushes the newest pending value synchronously on the worker thread and
    /// acknowledges completion through `acknowledgement`.
    pub fn try_flush_and_shutdown(
        &self,
        acknowledgement: SyncSender<()>,
    ) -> Result<(), PreferencesUnavailable> {
        self.tx
            .try_send(PreferencesCommand::FlushAndShutdown { acknowledgement })
            .map_err(map_unavailable)
    }

    /// Waits until the worker thread has exited.
    pub fn join(&self) {
        if let Ok(mut guard) = self.join_handle.lock() {
            if let Some(join_handle) = guard.take() {
                let _ = join_handle.join();
            }
        }
    }
}

fn map_unavailable(error: TrySendError<PreferencesCommand>) -> PreferencesUnavailable {
    match error {
        TrySendError::Full(_) => PreferencesUnavailable::Busy,
        TrySendError::Disconnected(_) => PreferencesUnavailable::Closed,
    }
}

/// Starts the debounced persistence worker.
///
/// `events` is the same bounded actor queue every other service reports on.
/// The worker owns the only place the concrete I/O failure exists: it logs it
/// and sends a typed outcome carrying nothing but the generation.
pub(crate) fn spawn_preferences_worker(
    repository: PreferencesRepository,
    events: crate::app_handle::AppEventSender,
) -> PreferencesWorkerHandle {
    let (tx, rx) = std::sync::mpsc::sync_channel::<PreferencesCommand>(COMMAND_CHANNEL_CAPACITY);
    let join_handle = Arc::new(Mutex::new(None));
    let join_slot = Arc::clone(&join_handle);

    if let Ok(handle) = std::thread::Builder::new()
        .name("openaircast-preferences".into())
        .spawn(move || run_worker(repository, events, rx))
    {
        if let Ok(mut slot) = join_slot.lock() {
            *slot = Some(handle);
        }
    }

    PreferencesWorkerHandle { tx, join_handle }
}

fn run_worker(
    repository: PreferencesRepository,
    events: crate::app_handle::AppEventSender,
    commands: Receiver<PreferencesCommand>,
) {
    let started = Instant::now();
    let mut debounce = DebounceState::default();
    let mut last_committed: Option<Preferences> = None;

    loop {
        let deadline_ms = debounce.peek_deadline();
        let now_ms = || started.elapsed().as_millis() as u64;

        let outcome = match deadline_ms {
            None => match commands.recv() {
                Ok(command) => Ok(command),
                Err(_) => Err(()),
            },
            Some(deadline_ms) => {
                let now = now_ms();
                if now >= deadline_ms {
                    if let Some((generation, value)) = debounce.take_due(now) {
                        if commit(&repository, &events, generation, &value) {
                            last_committed = Some(value);
                        }
                    }
                    continue;
                }
                match commands.recv_timeout(Duration::from_millis(deadline_ms - now)) {
                    Ok(command) => Ok(command),
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some((generation, value)) = debounce.take_due(now_ms()) {
                            if commit(&repository, &events, generation, &value) {
                                last_committed = Some(value);
                            }
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => Err(()),
                }
            }
        };

        match outcome {
            Ok(PreferencesCommand::Save { generation, value }) => {
                let reference = debounce.peek_pending().or_else(|| last_committed.clone());
                debounce.offer(now_ms(), generation, value, reference.as_ref());
            }
            Ok(PreferencesCommand::FlushAndShutdown { acknowledgement }) => {
                if let Some((generation, value)) = debounce.take_pending() {
                    commit(&repository, &events, generation, &value);
                }
                let _ = acknowledgement.send(());
                break;
            }
            Err(()) => {
                if let Some((generation, value)) = debounce.take_pending() {
                    commit(&repository, &events, generation, &value);
                }
                break;
            }
        }
    }
}

/// Writes one value and reports the typed outcome; returns whether the file
/// on disk now holds `value`.
///
/// This is the boundary the concrete failure never crosses. The
/// [`PreferencesError`] -- with its path, its operating-system message, its
/// error number -- is logged here and then dropped. What travels onward is a
/// generation and nothing else, so there is no route by which an I/O string
/// could turn up in a snapshot or on screen.
///
/// The report is best effort by design: the worker runs on its own thread and
/// the actor queue is bounded, so a full or closed queue costs a log line. It
/// must never cost the write, and it must never block the flush the shutdown
/// coordinator is waiting on.
fn commit(
    repository: &PreferencesRepository,
    events: &crate::app_handle::AppEventSender,
    generation: GenerationId,
    value: &Preferences,
) -> bool {
    let written = match repository.save_atomic(value) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(?generation, "could not persist shell preferences: {error}");
            false
        }
    };

    let outcome = if written {
        PreferencesEvent::Persisted { generation }
    } else {
        PreferencesEvent::PersistFailed { generation }
    };
    if let Err(error) = events.try_send(AppEvent::Preferences(outcome)) {
        tracing::warn!(?generation, "preference outcome not delivered: {error}");
    }

    written
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use crate::app::{
        AppEvent, CorrectiveAction, GenerationId, HotkeyBinding, LocalePreference, NoticeCode,
        Page, Preferences, PreferencesEvent, Severity, ThemePreference, UserNotice, WindowGeometry,
        PREFERENCES_SCHEMA_VERSION,
    };
    use crate::app_handle::AppEventSender;
    use crate::preferences::{
        spawn_preferences_worker, DebounceState, PreferencesError, PreferencesPaths,
        PreferencesRepository,
    };

    /// A complete schema-v1 file exactly as an earlier build wrote it: every
    /// field it knew about, and none of the fields version two adds.
    const SCHEMA_V1_SETTINGS_JSON: &str = r#"{
  "schema_version": 1,
  "hotkey": {
    "enabled": false,
    "modifiers": 5,
    "virtual_key": 80
  },
  "theme": "dark",
  "window": {
    "x": 20.0,
    "y": -8.0,
    "width": 1024.0,
    "height": 768.0,
    "maximized": true
  },
  "last_page": "home",
  "close_to_tray": false,
  "launch_at_startup": true
}
"#;

    fn temp_root() -> (TempDir, PreferencesPaths) {
        let dir = TempDir::new().expect("temp dir");
        let paths = PreferencesPaths::new(dir.path().to_path_buf());
        (dir, paths)
    }

    fn repository(paths: &PreferencesPaths) -> PreferencesRepository {
        PreferencesRepository::new(paths.clone())
    }

    fn write_settings(paths: &PreferencesPaths, raw: &str) {
        std::fs::create_dir_all(paths.settings_dir()).unwrap();
        std::fs::write(paths.settings_file(), raw).unwrap();
    }

    fn custom_preferences() -> Preferences {
        Preferences {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            hotkey: HotkeyBinding {
                enabled: false,
                modifiers: 5,
                virtual_key: 0x50,
            },
            theme: ThemePreference::Dark,
            locale: LocalePreference::German,
            advanced_information: true,
            window: WindowGeometry {
                x: Some(20.0),
                y: Some(-8.0),
                width: 1024.0,
                height: 768.0,
                maximized: true,
            },
            last_page: Page::Diagnostics,
            close_to_tray: false,
            launch_at_startup: true,
        }
    }

    fn expected_invalid_notice(summary: &str) -> Option<UserNotice> {
        Some(UserNotice {
            severity: Severity::Warning,
            code: NoticeCode::PreferencesFailed,
            summary: summary.into(),
            action: Some(CorrectiveAction::OpenSettings),
        })
    }

    #[test]
    fn missing_files_load_defaults_without_notice() {
        let (_dir, paths) = temp_root();

        let load = repository(&paths).load();

        assert_eq!(load.value, Preferences::default());
        assert_eq!(load.notice, None);
    }

    #[test]
    fn valid_legacy_hotkey_file_migrates_into_the_current_schema_and_is_preserved() {
        let (_dir, paths) = temp_root();
        std::fs::create_dir_all(paths.legacy_hotkey_dir()).unwrap();
        std::fs::write(paths.legacy_hotkey_file(), "5,80").unwrap();

        let load = repository(&paths).load();

        assert_eq!(
            load.value,
            Preferences {
                hotkey: HotkeyBinding {
                    enabled: true,
                    modifiers: 5,
                    virtual_key: 80,
                },
                ..Preferences::default()
            }
        );
        assert_eq!(load.notice, None);
        assert!(paths.legacy_hotkey_file().exists(), "legacy file is kept");
    }

    #[test]
    fn invalid_legacy_hotkey_falls_back_with_one_notice() {
        for (case, contents) in [
            ("not numbers", "abc"),
            ("single part", "3"),
            ("zero key", "3,0"),
            ("too many parts", "3,72,9"),
            ("empty", ""),
        ] {
            let (_dir, paths) = temp_root();
            std::fs::create_dir_all(paths.legacy_hotkey_dir()).unwrap();
            std::fs::write(paths.legacy_hotkey_file(), contents).unwrap();

            let load = repository(&paths).load();

            assert_eq!(load.value, Preferences::default(), "{case}");
            assert_eq!(
                load.notice,
                expected_invalid_notice(
                    "The previously saved shortcut was ignored because it is invalid.",
                ),
                "{case}"
            );
        }
    }

    #[test]
    fn corrupt_settings_json_falls_back_to_defaults_without_blocking_startup() {
        let (_dir, paths) = temp_root();
        std::fs::create_dir_all(paths.settings_dir()).unwrap();
        std::fs::write(paths.settings_file(), "{ definitely not json").unwrap();

        let load = repository(&paths).load();

        assert_eq!(load.value, Preferences::default());
        assert_eq!(
            load.notice,
            expected_invalid_notice(
                "Saved settings could not be read and were reset to their defaults.",
            )
        );
    }

    #[test]
    fn schema_v1_settings_migrate_into_version_two_preserving_every_old_field() {
        let (_dir, paths) = temp_root();
        write_settings(&paths, SCHEMA_V1_SETTINGS_JSON);

        let load = repository(&paths).load();

        assert_eq!(load.notice, None, "a readable old file is not a problem");
        assert_eq!(
            load.value,
            Preferences {
                schema_version: PREFERENCES_SCHEMA_VERSION,
                hotkey: HotkeyBinding {
                    enabled: false,
                    modifiers: 5,
                    virtual_key: 80,
                },
                theme: ThemePreference::Dark,
                locale: LocalePreference::System,
                advanced_information: false,
                window: WindowGeometry {
                    x: Some(20.0),
                    y: Some(-8.0),
                    width: 1024.0,
                    height: 768.0,
                    maximized: true,
                },
                last_page: Page::Home,
                close_to_tray: false,
                launch_at_startup: true,
            }
        );
    }

    #[test]
    fn schema_v1_last_page_survives_migration_instead_of_falling_back_to_the_default() {
        let (_dir, paths) = temp_root();
        write_settings(
            &paths,
            &SCHEMA_V1_SETTINGS_JSON.replace("\"last_page\": \"home\"", "\"last_page\": \"audio\""),
        );

        let load = repository(&paths).load();

        assert_eq!(load.value.last_page, Page::Audio);
        assert_eq!(load.notice, None);
    }

    #[test]
    fn migrating_a_load_leaves_the_old_file_untouched() {
        let (_dir, paths) = temp_root();
        write_settings(&paths, SCHEMA_V1_SETTINGS_JSON);
        let before = std::fs::read(paths.settings_file()).unwrap();

        let load = repository(&paths).load();

        assert_eq!(load.value.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert_eq!(
            load.value.hotkey.virtual_key, 80,
            "the value really came from the old file, not from defaults"
        );
        assert_eq!(
            std::fs::read(paths.settings_file()).unwrap(),
            before,
            "loading must not rewrite the file"
        );
        assert!(
            !paths.settings_dir().join("settings.json.tmp").exists(),
            "loading must not leave a temporary file behind"
        );
    }

    #[test]
    fn the_next_successful_save_after_migration_writes_deterministic_version_two() {
        let (_dir, paths) = temp_root();
        write_settings(&paths, SCHEMA_V1_SETTINGS_JSON);
        let repo = repository(&paths);
        let migrated = repo.load().value;

        repo.save_atomic(&migrated).unwrap();
        let first = std::fs::read_to_string(paths.settings_file()).unwrap();
        repo.save_atomic(&migrated).unwrap();
        let second = std::fs::read_to_string(paths.settings_file()).unwrap();

        assert_eq!(first, second, "the written bytes are deterministic");
        let written: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(written["schema_version"], 2);
        assert_eq!(written["locale"], "system");
        assert_eq!(written["advanced_information"], false);
        assert_eq!(written["launch_at_startup"], true);

        let reloaded = repo.load();
        assert_eq!(reloaded.value, migrated, "version two round-trips");
        assert_eq!(reloaded.notice, None);
    }

    #[test]
    fn unsupported_schema_versions_are_rejected_with_one_notice() {
        for version in ["0", "3", "99"] {
            let (_dir, paths) = temp_root();
            write_settings(
                &paths,
                &SCHEMA_V1_SETTINGS_JSON.replace(
                    "\"schema_version\": 1",
                    &format!("\"schema_version\": {version}"),
                ),
            );

            let load = repository(&paths).load();

            assert_eq!(load.value, Preferences::default(), "version {version}");
            assert_eq!(
                load.notice,
                expected_invalid_notice(
                    "Saved settings could not be read and were reset to their defaults.",
                ),
                "version {version}"
            );
        }
    }

    #[test]
    fn save_atomic_rejects_a_foreign_schema_version_before_touching_the_filesystem() {
        let (_dir, paths) = temp_root();
        let stale = Preferences {
            schema_version: 1,
            ..custom_preferences()
        };

        let outcome = repository(&paths).save_atomic(&stale);

        assert!(
            matches!(
                outcome,
                Err(PreferencesError::InvalidSchemaVersion { found: 1 })
            ),
            "expected a typed rejection, got {outcome:?}"
        );
        assert!(
            !paths.settings_dir().exists(),
            "the rejection happens before any directory, temporary file, or replacement"
        );
    }

    #[test]
    fn valid_settings_json_wins_over_legacy_migration() {
        let (_dir, paths) = temp_root();
        std::fs::create_dir_all(paths.legacy_hotkey_dir()).unwrap();
        std::fs::write(paths.legacy_hotkey_file(), "5,80").unwrap();
        let saved = custom_preferences();
        repository(&paths).save_atomic(&saved).unwrap();

        let load = repository(&paths).load();

        assert_eq!(load.value, saved);
        assert_eq!(load.notice, None);
    }

    #[test]
    fn complete_schema_round_trips_every_field() {
        let (_dir, paths) = temp_root();
        let saved = custom_preferences();

        repository(&paths).save_atomic(&saved).unwrap();
        let load = repository(&paths).load();

        assert_eq!(load.value, saved);
        assert_eq!(load.notice, None);
    }

    #[test]
    fn serialized_json_is_deterministic_and_domain_clean() {
        let (_dir, _paths) = temp_root();
        let value = custom_preferences();

        let first = serde_json::to_string_pretty(&value).unwrap();
        let second = serde_json::to_string_pretty(&value).unwrap();
        assert_eq!(first, second);

        let lower = first.to_lowercase();
        for forbidden in [
            "volume",
            "receiver",
            "stream",
            "\"ip\"",
            "address",
            "generation",
            "secret",
            "token",
            "\"log\"",
        ] {
            assert!(
                !lower.contains(forbidden),
                "json must not contain {forbidden}"
            );
        }

        for required in [
            "schema_version",
            "hotkey",
            "enabled",
            "modifiers",
            "virtual_key",
            "theme",
            "window",
            "maximized",
            "locale",
            "advanced_information",
            "last_page",
            "close_to_tray",
            "launch_at_startup",
        ] {
            assert!(lower.contains(required), "json must contain {required}");
        }
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)] // the test restores writability
    fn failed_replacement_preserves_the_previous_valid_file() {
        let (_dir, paths) = temp_root();
        let repo = repository(&paths);
        let first = custom_preferences();
        repo.save_atomic(&first).unwrap();
        let settings_path = paths.settings_file();
        let mut permissions = std::fs::metadata(&settings_path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&settings_path, permissions).unwrap();

        let outcome = repo.save_atomic(&Preferences::default());

        let mut permissions = std::fs::metadata(&settings_path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&settings_path, permissions).unwrap();

        assert!(outcome.is_err());
        let load = repo.load();
        assert_eq!(load.value, first, "previous valid settings survive");
    }

    #[test]
    fn debounce_state_uses_500ms_for_ordinary_changes() {
        let mut state = DebounceState::default();
        let value = Preferences::default();

        let deadline = state.offer(1_000, GenerationId(1), value.clone(), None);

        assert_eq!(deadline, 1_500);
        assert_eq!(state.peek_deadline(), Some(1_500));
        assert_eq!(state.take_due(1_499), None);
        assert_eq!(
            state.take_due(1_500),
            Some((GenerationId(1), value.clone()))
        );
        assert_eq!(state.peek_deadline(), None);
    }

    #[test]
    fn debounce_state_extends_geometry_only_churn_to_one_second_and_keeps_newest_generation() {
        let mut state = DebounceState::default();
        let base = Preferences::default();
        let mut moved = base.clone();
        moved.window.width = 1001.0;

        let first = state.offer(1_000, GenerationId(1), base, None);
        assert_eq!(first, 1_500);

        let second = state.offer(1_100, GenerationId(2), moved.clone(), None);
        assert_eq!(
            second, 2_100,
            "geometry-only churn uses the one-second delay"
        );

        let mut retinted = moved;
        retinted.theme = ThemePreference::Light;
        let third = state.offer(1_200, GenerationId(3), retinted.clone(), None);
        assert_eq!(third, 1_700, "ordinary changes return to the short delay");

        assert_eq!(state.take_due(1_699), None);
        let (generation, value) = state.take_due(1_700).unwrap();
        assert_eq!(generation, GenerationId(3));
        assert_eq!(value, retinted);
    }

    #[test]
    fn debounce_state_treats_locale_and_advanced_changes_as_ordinary_not_geometry_churn() {
        for (case, changed) in [
            (
                "locale",
                Preferences {
                    locale: LocalePreference::German,
                    ..Preferences::default()
                },
            ),
            (
                "advanced information",
                Preferences {
                    advanced_information: true,
                    ..Preferences::default()
                },
            ),
        ] {
            let mut state = DebounceState::default();
            let base = Preferences::default();

            let deadline = state.offer(1_000, GenerationId(1), changed, Some(&base));

            assert_eq!(deadline, 1_500, "{case} must use the short delay");
        }
    }

    #[test]
    fn worker_debounces_then_flushes_the_newest_value_on_shutdown() {
        let (_dir, paths) = temp_root();
        let repo = repository(&paths);
        let (report_tx, _reports) = mpsc::sync_channel::<AppEvent>(64);
        let worker = spawn_preferences_worker(repo.clone(), AppEventSender::new(report_tx));

        worker
            .try_schedule(
                GenerationId(1),
                Preferences {
                    theme: ThemePreference::Dark,
                    ..Preferences::default()
                },
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(repo.load().value.theme, ThemePreference::Dark);

        for width in [1001.0f32, 1002.0, 1010.0] {
            worker
                .try_schedule(
                    GenerationId(10),
                    Preferences {
                        window: WindowGeometry {
                            x: None,
                            y: None,
                            width,
                            height: 720.0,
                            maximized: false,
                        },
                        ..Preferences {
                            theme: ThemePreference::Dark,
                            ..Preferences::default()
                        }
                    },
                )
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            repo.load().value.window.width,
            1120.0,
            "geometry churn must not have flushed yet"
        );

        worker
            .try_schedule(
                GenerationId(20),
                Preferences {
                    hotkey: HotkeyBinding {
                        enabled: false,
                        modifiers: 3,
                        virtual_key: 0x48,
                    },
                    theme: ThemePreference::Dark,
                    window: WindowGeometry {
                        x: None,
                        y: None,
                        width: 1010.0,
                        height: 720.0,
                        maximized: false,
                    },
                    ..Preferences::default()
                },
            )
            .unwrap();

        let (ack_tx, ack_rx) = mpsc::sync_channel::<()>(1);
        let started = Instant::now();
        worker.try_flush_and_shutdown(ack_tx).unwrap();
        ack_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("shutdown acknowledgement");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "final flush completes promptly"
        );

        let final_value = repo.load().value;
        assert!(!final_value.hotkey.enabled);
        assert_eq!(final_value.theme, ThemePreference::Dark);
        assert_eq!(final_value.window.width, 1010.0);
        assert_eq!(final_value.window.height, 720.0);

        worker.join();
    }

    #[test]
    fn worker_accepts_a_burst_without_blocking_and_shuts_down_on_disconnect() {
        let (_dir, paths) = temp_root();
        let (report_tx, _reports) = mpsc::sync_channel::<AppEvent>(64);
        let worker = spawn_preferences_worker(repository(&paths), AppEventSender::new(report_tx));

        for generation in 1..=64u64 {
            let _ = worker.try_schedule(GenerationId(generation), Preferences::default());
        }
        drop(worker);
    }

    /// Waits for the worker's own completion signal rather than for a clock.
    ///
    /// The flush is synchronous on the worker thread and acknowledges only
    /// after the write, so the timeout is a hang guard, never a schedule the
    /// test depends on.
    fn flush_and_wait(worker: &super::PreferencesWorkerHandle) {
        let (ack_tx, ack_rx) = mpsc::sync_channel::<()>(1);
        worker.try_flush_and_shutdown(ack_tx).unwrap();
        ack_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker acknowledged its flush");
    }

    #[test]
    fn a_successful_write_reports_the_generation_it_actually_wrote() {
        let (_dir, paths) = temp_root();
        let repo = repository(&paths);
        let (tx, rx) = mpsc::sync_channel::<AppEvent>(8);
        let worker = spawn_preferences_worker(repo.clone(), AppEventSender::new(tx));

        worker
            .try_schedule(GenerationId(7), custom_preferences())
            .unwrap();
        flush_and_wait(&worker);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![AppEvent::Preferences(PreferencesEvent::Persisted {
                generation: GenerationId(7),
            })],
            "the outcome is queued before the flush is acknowledged"
        );
        assert_eq!(repo.load().value, custom_preferences());

        worker.join();
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)] // the test restores writability
    fn a_failed_write_reports_the_attempted_generation_and_keeps_the_previous_file() {
        let (_dir, paths) = temp_root();
        let repo = repository(&paths);
        let previous = custom_preferences();
        repo.save_atomic(&previous).unwrap();

        let settings_path = paths.settings_file();
        let mut permissions = std::fs::metadata(&settings_path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&settings_path, permissions).unwrap();

        let (tx, rx) = mpsc::sync_channel::<AppEvent>(8);
        let worker = spawn_preferences_worker(repo.clone(), AppEventSender::new(tx));
        worker
            .try_schedule(GenerationId(12), Preferences::default())
            .unwrap();
        flush_and_wait(&worker);
        let reported = rx.try_iter().collect::<Vec<_>>();

        let mut permissions = std::fs::metadata(&settings_path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&settings_path, permissions).unwrap();

        assert_eq!(
            reported,
            vec![AppEvent::Preferences(PreferencesEvent::PersistFailed {
                generation: GenerationId(12),
            })],
            "the failure names the write that was attempted, and carries no error text"
        );
        assert_eq!(
            repo.load().value,
            previous,
            "an atomic write that failed replaced nothing"
        );

        worker.join();
    }

    #[test]
    fn a_report_nobody_can_receive_still_leaves_the_file_written_and_the_worker_shut_down() {
        let (_dir, paths) = temp_root();
        let repo = repository(&paths);
        let (tx, rx) = mpsc::sync_channel::<AppEvent>(1);
        let worker = spawn_preferences_worker(repo.clone(), AppEventSender::new(tx));
        // The actor is gone: every outcome send from here on fails.
        drop(rx);

        worker
            .try_schedule(GenerationId(3), custom_preferences())
            .unwrap();
        flush_and_wait(&worker);

        assert_eq!(
            repo.load().value,
            custom_preferences(),
            "reporting is best effort; writing the file is not"
        );

        worker.join();
    }

    #[test]
    fn a_lower_generation_offer_never_displaces_the_newer_pending_value() {
        let mut state = DebounceState::default();
        let newer = Preferences {
            theme: ThemePreference::Dark,
            ..Preferences::default()
        };
        let older = Preferences {
            theme: ThemePreference::Light,
            ..Preferences::default()
        };

        let deadline = state.offer(1_000, GenerationId(5), newer.clone(), None);
        assert_eq!(deadline, 1_500);

        let after_stale = state.offer(1_200, GenerationId(3), older, None);

        assert_eq!(
            state.peek_pending(),
            Some(newer.clone()),
            "the superseded value must not become the one that gets written"
        );
        assert_eq!(
            after_stale, 1_500,
            "a superseded offer must not restart the timer either"
        );
        assert_eq!(
            state.take_due(1_500),
            Some((GenerationId(5), newer)),
            "the generation reported and the value written must be the same change"
        );
    }

    #[test]
    fn paths_resolve_canonical_and_legacy_locations_from_the_injected_root() {
        let root = PathBuf::from("R").join("appdata");
        let paths = PreferencesPaths::new(root.clone());

        assert_eq!(paths.settings_dir(), root.join("OpenAirCast"));
        assert_eq!(
            paths.settings_file(),
            root.join("OpenAirCast").join("settings.json")
        );
        assert_eq!(
            paths.legacy_hotkey_file(),
            root.join("HomePodCast").join("hotkey.txt")
        );
    }
}
