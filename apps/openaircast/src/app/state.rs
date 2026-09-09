use std::collections::HashSet;

use airplay_core::DeviceId;

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct GenerationId(pub u64);

/// Stable saved-template identity; independent of device and endpoint identities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GroupId(pub uuid::Uuid);

/// Stored member data used when creating or editing a template.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupMember {
    /// Stable receiver identity, including speakers currently offline.
    pub receiver: DeviceId,
    /// Last-known display name saved with the template.
    pub name: String,
    /// Intended balance, finite and within 0..=1.
    pub level: f32,
}

/// One confirmed saved template. Availability is current, never persisted.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedGroupReading {
    /// Persisted template identity.
    pub id: GroupId,
    /// Backend-confirmed display name.
    pub name: String,
    /// Complete confirmed membership and balances in stored order.
    pub members: Vec<GroupMember>,
    /// Members currently discoverable; absence means offline, not deleted.
    pub available_members: std::collections::BTreeSet<DeviceId>,
}

/// Explicit template operations; applying alone preserves current run intent.
#[derive(Clone, Debug, PartialEq)]
pub enum GroupCommand {
    /// Create (no ID) or replace a template without changing playback.
    Save {
        /// None for a create; retain the confirmed ID when editing.
        id: Option<GroupId>,
        /// The backend trims and validates uniqueness and length.
        name: String,
        /// Complete replacement membership and intended balances.
        members: Vec<GroupMember>,
    },
    /// Delete the template while preserving current selection and playback.
    Delete(GroupId),
    /// Atomically apply membership and levels, optionally requesting playback.
    Apply {
        /// Confirmed template to activate.
        id: GroupId,
        /// False applies only; true also requests running audio.
        start: bool,
    },
    /// Commit the shared selection draft, including offline members.
    ApplySelection {
        /// The shared draft's complete membership.
        receiver_ids: Vec<DeviceId>,
    },
}

/// Presentation codes; backend error text and paths never enter the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupFailure {
    /// The backend command queue refused admission.
    Busy,
    /// The backend is no longer available.
    Closed,
    /// The requested group or member values were rejected.
    Validation,
    /// The durable write failed; the old state remains authoritative.
    Persistence,
    /// The result aged out; inspect confirmed state before retrying a create.
    ConfirmationLost,
}

/// State of the latest explicitly requested operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupOperationStatus {
    /// Waiting for a retained backend outcome.
    Pending,
    /// The backend accepted the operation; durable data still comes from readings.
    Succeeded,
    /// Refused, failed, or unable to recover a definitive confirmation.
    Failed(GroupFailure),
}

/// The latest request and its correlated outcome; only one may be pending.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupOperation {
    /// Monotonic shell request identity used to ignore stale outcomes.
    pub request: u64,
    /// Requested action and target, retained for contextual feedback.
    pub command: GroupCommand,
    /// Pending or terminal outcome of this exact request.
    pub status: GroupOperationStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Home,
    Speakers,
    Groups,
    Audio,
    Diagnostics,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// The persisted language choice. `System` follows the Windows display
/// language; an explicit choice ignores later OS-language changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalePreference {
    #[default]
    System,
    German,
    English,
}

/// The language the UI actually renders in. Windows display languages that
/// OpenAirCast does not ship fall back to English.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResolvedLocale {
    German,
    #[default]
    English,
}

impl LocalePreference {
    /// Resolves the preference against the current Windows display language.
    ///
    /// The `windows` argument is the already-mapped display language, so this
    /// stays a pure function: no OS call, no I/O, no clock.
    pub const fn resolve(self, windows: ResolvedLocale) -> ResolvedLocale {
        match self {
            Self::System => windows,
            Self::German => ResolvedLocale::German,
            Self::English => ResolvedLocale::English,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HotkeyBinding {
    pub enabled: bool,
    pub modifiers: u32,
    pub virtual_key: u32,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self {
            enabled: true,
            modifiers: MOD_CONTROL | MOD_ALT,
            virtual_key: 0x48,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WindowGeometry {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl WindowGeometry {
    pub fn validate(self) -> Option<Self> {
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.x.is_some_and(|x| !x.is_finite())
            || self.y.is_some_and(|y| !y.is_finite())
        {
            return None;
        }

        Some(Self {
            width: self.width.max(900.0),
            height: self.height.max(600.0),
            ..self
        })
    }
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: 1120.0,
            height: 720.0,
            maximized: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub visible: bool,
    pub geometry: WindowGeometry,
    pub close_to_tray: bool,
    pub launch_at_startup: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            visible: true,
            geometry: WindowGeometry::default(),
            close_to_tray: true,
            launch_at_startup: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoveryState {
    Idle,
    Discovering { generation: GenerationId },
    Ready,
    Failed { summary: String },
}

impl Default for DiscoveryState {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverState {
    pub id: DeviceId,
    pub name: String,
    pub model: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamState {
    Stopped,
    Starting {
        generation: GenerationId,
    },
    Streaming {
        generation: GenerationId,
    },
    /// Audio is reaching some members but not all of them, or the capture
    /// source is missing.
    ///
    /// A running state, not a failure: the session is alive and stoppable,
    /// and the applied membership still says which receivers were asked for.
    /// It exists because folding it into `Streaming` made the window claim a
    /// complete delivery it could not observe.
    Degraded {
        generation: GenerationId,
    },
    /// The backend is rebuilding the whole group inside the running session.
    ///
    /// Distinct from `Starting`, which is the answer to the user pressing
    /// Start, and from `Stopping`, which ends the session. This one is the
    /// backend healing itself -- a member rejoining, the clock primary being
    /// replaced, the local interface changing -- and the user did not ask for
    /// it. Showing it is what gives an interruption an explanation.
    Restarting {
        generation: GenerationId,
    },
    Stopping {
        generation: GenerationId,
    },
    Failed {
        generation: GenerationId,
        summary: String,
    },
}

impl Default for StreamState {
    fn default() -> Self {
        Self::Stopped
    }
}

impl StreamState {
    /// Whether a session exists that the user can end.
    ///
    /// Written out by hand with no rest pattern, and used by every place that
    /// asks the question -- the Stop command, the guard on applying a staged
    /// membership, the shutdown path, and the snapshot's `can_stop`. A new
    /// phase is therefore a compile error here instead of quietly defaulting
    /// to "not running", which is how a running session loses its Stop
    /// button and its bounded teardown at the same time.
    pub const fn is_running(&self) -> bool {
        match self {
            Self::Starting { .. }
            | Self::Streaming { .. }
            | Self::Degraded { .. }
            | Self::Restarting { .. } => true,
            Self::Stopped | Self::Stopping { .. } | Self::Failed { .. } => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Audio: the three capabilities the Audio destination controls
// ---------------------------------------------------------------------------

/// The latency the user asks the backend for.
///
/// The shell's own three words, written out rather than re-exported: the
/// backend may one day offer a fourth profile that this window has no control
/// for, and when it does the compiler has to ask in `backend_bridge` instead
/// of letting an unnamed profile render as whichever arm sat nearest.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LatencyChoice {
    Low,
    #[default]
    Normal,
    Stable,
}

impl LatencyChoice {
    /// Every choice in presentation order.
    pub const ALL: [Self; 3] = [Self::Low, Self::Normal, Self::Stable];
}

/// Why the backend refuses a latency choice, as a code the catalog owns.
///
/// The backend states its reason as free-form text. That text is never read,
/// never carried, and never painted; the bridge learns only *that* a choice
/// is refused and this code is what crosses instead, exactly as
/// `NoticeCode` does for a failed discovery.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LatencyUnavailable {
    /// Offered so the user can see it exists, held back until the hardware
    /// gate passes.
    NotValidated,
}

/// One latency choice paired with whether it can be selected.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LatencyOption {
    pub choice: LatencyChoice,
    /// `None` while the choice is selectable.
    pub unavailable: Option<LatencyUnavailable>,
}

/// Everything the shell knows about latency after at least one reading.
///
/// Absence is the `Option` around this struct, never a default inside it:
/// "Normal is selected" and "nothing has reported yet" are different facts
/// and the Audio destination draws them differently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatencyReading {
    pub selected: LatencyChoice,
    pub options: Vec<LatencyOption>,
}

/// The shell's opaque handle for one Windows capture endpoint.
///
/// A counter allocated by the bridge, with no arithmetic relationship to the
/// raw Windows endpoint ID it stands for. The ID has the shape
/// `{0.0.0.00000000}.{guid}`, identifies the machine's hardware, and must
/// never reach a snapshot, an event, a presentation model, or a painted
/// string; the translation back happens in `backend_bridge` and nowhere else.
/// It is deliberately not a truncation and not a hash of the ID: neither
/// could be shown to be irreversible, and a counter needs no argument.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AudioEndpointKey(pub u64);

/// One selectable capture endpoint.
///
/// The name is the one Windows itself shows the user in its own volume
/// mixer -- the same class of datum as a receiver name, which already
/// crosses this boundary -- so it is carried. The ID is not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioEndpointChoice {
    pub key: AudioEndpointKey,
    pub name: String,
}

/// Which endpoint the user has asked capture to follow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioEndpointSelection {
    /// Follow whatever Windows currently calls the default.
    SystemDefault,
    /// One named endpoint that is being offered right now.
    Chosen(AudioEndpointKey),
    /// One named endpoint that is *not* being offered right now.
    ///
    /// The stored preference stands -- the backend does not silently switch
    /// away from it -- but it cannot be met, and paragraph 7.4 requires the
    /// window to say so. The name is the one saved with the choice, so it may
    /// be older than the device list; the window never presents it as a
    /// present device.
    ChosenButMissing { name: String },
    /// The inventory has not verified this preference; capture may still prove it live.
    ChosenUnverified { name: String },
}

/// What capture is doing right now.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureState {
    Capturing,
    SilentSystem,
    Recovering,
    Unavailable,
    Failed,
}

/// Everything the shell knows about the capture source after one reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioSourceReading {
    /// Whether at least one endpoint scan succeeded, including an empty scan.
    pub endpoints_known: bool,
    /// Sanitized scan failure; offered endpoints may still hold the last success.
    pub refresh_failed: bool,
    /// Endpoints from the last successful Windows playback-device scan.
    /// Failed refreshes preserve these choices; `endpoints_known` distinguishes
    /// a successful empty scan from a scan that has never succeeded.
    pub endpoints: Vec<AudioEndpointChoice>,
    pub selection: AudioEndpointSelection,
    /// Opaque identity of the source actually captured, independent of preference.
    /// Raw Windows IDs remain in the bridge and this key is never exported in reports.
    pub captured_key: Option<AudioEndpointKey>,
    /// The endpoint capture is actually on, or `None` while it is on none.
    pub captured_name: Option<String>,
    pub state: CaptureState,
}

/// What the window asks capture to follow.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AudioEndpointRequest {
    SystemDefault,
    Endpoint(AudioEndpointKey),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeCode {
    NoReceiverAvailable,
    DiscoveryFailed,
    SessionFailed,
    ControllerUnavailable,
    PreferencesFailed,
    HotkeyFailed,
    InvalidInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrectiveAction {
    Refresh,
    Retry,
    OpenSettings,
    /// Re-offer the settings file, and nothing else.
    ///
    /// Deliberately separate from [`CorrectiveAction::Retry`], which retries
    /// the *session*. One button reading "Retry" for both would mean the user
    /// pressing it after a failed save could start streaming instead.
    RetryPreferencesPersistence,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserNotice {
    pub severity: Severity,
    pub code: NoticeCode,
    pub summary: String,
    pub action: Option<CorrectiveAction>,
}

/// How the diagnostics registry currently judges the whole application.
///
/// The shell's own four-word vocabulary, not the registry's enum. It happens
/// to have the same shape today, and that is the point of writing it out
/// here: the registry may grow a fifth state that the window has no word for,
/// and when it does, the compiler asks in `backend_bridge` rather than
/// letting an unnamed state render as whatever the nearest arm said.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum DiagnosticsHealth {
    /// Nothing is running, or too little has been measured to judge.
    #[default]
    Unknown,
    /// A stream is active and no definite problem has been observed.
    Running,
    /// Something recoverable is wrong and the user should know.
    Attention,
    /// A receiver or the session itself is in a failed state.
    Error,
}

impl DiagnosticsHealth {
    /// Every state, in the order the registry's own badge escalates.
    ///
    /// Written out rather than derived, and bound to the enum by the
    /// wildcard-free roster in the page tests: a fifth state that nobody adds
    /// here fails there instead of quietly rendering as one of these four.
    pub const ALL: &'static [DiagnosticsHealth] = &[
        DiagnosticsHealth::Unknown,
        DiagnosticsHealth::Running,
        DiagnosticsHealth::Attention,
        DiagnosticsHealth::Error,
    ];
}

/// Everything the shell knows about the backend's diagnostics registry.
///
/// Three values, and the shape of the type is the privacy boundary: it is
/// `Copy`, holds no `String`, no identifier, no collection, and no clock, so
/// there is nothing here that *could* carry a path, an address, a hardware
/// identity, or backend prose even if a future projection tried. Which fields
/// of the registry's snapshot become these three -- and, field by field, why
/// the rest stay behind -- is decided in
/// [`crate::backend_bridge::diagnostics_reading`], the only place in the
/// shell that may look at the registry at all.
///
/// Absence is carried by the `Option` around this struct in [`AppState`] and
/// [`super::UiSnapshot`], never by a zero inside it: "the registry has not
/// reported yet" and "the registry reported zero" are different facts and the
/// window shows them differently.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct DiagnosticsReading {
    /// The registry's conservative overall judgement.
    pub health: DiagnosticsHealth,
    /// Diagnostic events lost because a producer outran the feed.
    ///
    /// A real measurement from the moment the registry starts, so zero here
    /// means "none lost", not "not counted".
    pub events_dropped_total: u64,
    /// How many receivers the registry currently holds measurements for.
    pub measured_receivers: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Generations {
    pub discovery: GenerationId,
    pub session: GenerationId,
    pub volume: GenerationId,
    pub preferences: GenerationId,
    pub hotkey: GenerationId,
}

/// The only schema version this build writes. Older files are migrated on
/// load; anything else is rejected rather than guessed at.
pub const PREFERENCES_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub hotkey: HotkeyBinding,
    pub theme: ThemePreference,
    pub locale: LocalePreference,
    pub advanced_information: bool,
    pub window: WindowGeometry,
    pub last_page: Page,
    pub close_to_tray: bool,
    pub launch_at_startup: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            hotkey: HotkeyBinding::default(),
            theme: ThemePreference::default(),
            locale: LocalePreference::default(),
            advanced_information: false,
            window: WindowGeometry::default(),
            last_page: Page::default(),
            close_to_tray: true,
            launch_at_startup: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppState {
    pub revision: u64,
    /// Confirmed backend templates, unknown until the first projection.
    pub saved_groups: Option<Vec<SavedGroupReading>>,
    /// Latest request and outcome; never a replacement for confirmed groups.
    pub group_operation: Option<GroupOperation>,
    pub(crate) next_group_request: u64,
    /// Backend revision zero is a valid first persisted-selection reading.
    pub(crate) desired_known: bool,
    pub(crate) controller_closed: bool,
    pub desired_revision: u64,
    pub page: Page,
    pub window: WindowState,
    pub discovery: DiscoveryState,
    pub receivers: Vec<ReceiverState>,
    pub desired_receivers: HashSet<DeviceId>,
    pub staged_receivers: HashSet<DeviceId>,
    pub staged_base_revision: u64,
    pub active_receivers: HashSet<DeviceId>,
    pub stream: StreamState,
    pub master_volume: f32,
    /// A drag draft remains visible until its exact backend result arrives.
    pub(crate) volume_pending: bool,
    /// Last backend-confirmed value, independent of a pending drag draft.
    pub(crate) confirmed_master_volume: Option<f32>,
    /// Durable per-receiver balance; unknown until the backend reports.
    pub receiver_levels: Option<std::collections::BTreeMap<DeviceId, f32>>,
    pub theme: ThemePreference,
    /// The persisted language choice.
    pub locale_preference: LocalePreference,
    /// The last Windows display language observed.
    ///
    /// Internal on purpose: it is an input to resolution, not a presentation
    /// value, so it never reaches a snapshot. It is cached even while an
    /// explicit language is chosen, so switching back to `System` resolves
    /// against what Windows renders in *now*, not against a stale reading.
    pub(crate) windows_display_locale: ResolvedLocale,
    /// The language the UI actually renders in, kept in step with
    /// `locale_preference` and `windows_display_locale`.
    pub resolved_locale: ResolvedLocale,
    /// Whether the shell reveals advanced diagnostic detail.
    pub advanced_information: bool,
    pub hotkey: HotkeyBinding,
    pub notice: Option<UserNotice>,
    /// Whether the backend reports the group muted, or `None` while it has
    /// never said.
    ///
    /// The `Option` is the honest empty state, exactly as for `diagnostics`:
    /// a switch drawn over a value nobody measured would claim "not muted"
    /// on a build whose backend never answers.
    pub muted: Option<bool>,
    /// The newest latency reading, or `None` while nothing has reported.
    pub latency: Option<LatencyReading>,
    /// The newest capture-source reading, or `None` while nothing has
    /// reported.
    pub audio_source: Option<AudioSourceReading>,
    /// The newest reading of the diagnostics registry, or `None` while the
    /// registry has never reported.
    ///
    /// The `Option` is the honest empty state and is not interchangeable with
    /// a zeroed [`DiagnosticsReading`]: a shell that has heard nothing must
    /// not draw a measurement, and the Diagnostics page renders these two
    /// cases differently.
    pub diagnostics: Option<DiagnosticsReading>,
    pub live_diagnostics: Option<super::LiveDiagnosticsReading>,
    pub hardware_check: super::hardware_check::HardwareCheck,
    pub shutting_down: bool,
    pub generations: Generations,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            revision: 0,
            saved_groups: None,
            group_operation: None,
            next_group_request: 0,
            desired_known: false,
            controller_closed: false,
            desired_revision: 0,
            page: Page::default(),
            window: WindowState::default(),
            discovery: DiscoveryState::default(),
            receivers: Vec::new(),
            desired_receivers: HashSet::new(),
            staged_receivers: HashSet::new(),
            staged_base_revision: 0,
            active_receivers: HashSet::new(),
            stream: StreamState::default(),
            master_volume: 0.25,
            volume_pending: false,
            confirmed_master_volume: None,
            receiver_levels: None,
            theme: ThemePreference::default(),
            locale_preference: LocalePreference::default(),
            windows_display_locale: ResolvedLocale::default(),
            resolved_locale: ResolvedLocale::default(),
            advanced_information: false,
            hotkey: HotkeyBinding::default(),
            notice: None,
            muted: None,
            latency: None,
            audio_source: None,
            diagnostics: None,
            live_diagnostics: None,
            hardware_check: super::hardware_check::HardwareCheck::default(),
            shutting_down: false,
            generations: Generations::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_default_is_enabled_ctrl_alt_h() {
        let hotkey = HotkeyBinding::default();

        assert!(hotkey.enabled);
        assert_eq!(hotkey.modifiers, MOD_CONTROL | MOD_ALT);
        assert_eq!(hotkey.virtual_key, 0x48);
    }

    #[test]
    fn window_geometry_default_is_safe_and_validation_clamps_minimum_size() {
        let default_geometry = WindowGeometry::default();
        assert_eq!(
            default_geometry,
            WindowGeometry {
                x: None,
                y: None,
                width: 1120.0,
                height: 720.0,
                maximized: false,
            }
        );

        assert_eq!(
            WindowGeometry {
                x: Some(-10.0),
                y: Some(20.0),
                width: 640.0,
                height: 480.0,
                maximized: false,
            }
            .validate(),
            Some(WindowGeometry {
                x: Some(-10.0),
                y: Some(20.0),
                width: 900.0,
                height: 600.0,
                maximized: false,
            })
        );
    }

    #[test]
    fn window_geometry_validation_rejects_non_finite_dimensions_and_coordinates() {
        for geometry in [
            WindowGeometry {
                x: None,
                y: None,
                width: f32::NAN,
                height: 720.0,
                maximized: false,
            },
            WindowGeometry {
                x: Some(f32::INFINITY),
                y: None,
                width: 1120.0,
                height: 720.0,
                maximized: false,
            },
        ] {
            assert_eq!(geometry.validate(), None);
        }
    }
}
