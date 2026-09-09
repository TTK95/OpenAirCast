use airplay_core::DeviceId;

use super::{
    AudioEndpointRequest, AudioSourceReading, DiagnosticsReading, GenerationId, HotkeyBinding,
    LatencyChoice, LatencyReading, LocalePreference, Page, Preferences, ReceiverState,
    ResolvedLocale, ThemePreference, UserNotice, WindowGeometry,
};

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    /// Explicit listening-check gestures; opening never starts playback.
    HardwareCheck(super::hardware_check::HardwareCheckAction),
    /// Create, edit, delete or explicitly apply a saved template.
    GroupRequested(super::GroupCommand),
    Navigate(Page),
    MainWindowCloseRequested,
    ShowMainWindow,
    ToggleStagedReceiver(DeviceId),
    ApplyStagedReceivers,
    DiscardStagedReceivers,
    StartRequested,
    StopRequested,
    GlobalHotkeyPressed,
    RefreshRequested,
    MasterVolumeChanged(f32),
    /// Set one receiver's balance relative to the master volume.
    ReceiverLevelRequested {
        receiver: DeviceId,
        level: f32,
    },
    /// The user asked for the group to be muted or unmuted.
    ///
    /// Carries the wanted value rather than "toggle", so the request cannot
    /// depend on what the window happened to be showing when the click
    /// landed. The switch renderer is built for exactly this: the page reads
    /// the applied state and says what the opposite of it would be.
    MuteChangeRequested(bool),
    /// The user picked a latency profile. Only a profile the newest reading
    /// reports as selectable is acted on.
    LatencyChoiceRequested(LatencyChoice),
    /// The user picked a capture source.
    AudioEndpointRequested(AudioEndpointRequest),
    ThemeChanged(ThemePreference),
    /// The user chose a language, or chose to follow Windows again.
    LocaleChanged(LocalePreference),
    /// The user revealed or hid advanced diagnostic detail.
    AdvancedInformationChanged(bool),
    /// Windows reported a new display language. Not a user choice, and never
    /// persisted: it only feeds the cached input to language resolution.
    WindowsDisplayLanguageChanged(ResolvedLocale),
    /// The user asked for the settings file to be written again after a failed
    /// save. Scoped to persistence: it never touches the session.
    RetryPreferencesPersistence,
    HotkeyChanged(HotkeyBinding),
    WindowGeometryChanged(WindowGeometry),
    Controller(ControllerEvent),
    Preferences(PreferencesEvent),
    QuitRequested,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ControllerEvent {
    /// The complete confirmed template list and current member availability.
    SavedGroupsChanged {
        /// Backend data projected into independent shell value types.
        groups: Vec<super::SavedGroupReading>,
    },
    /// A terminal result retained until the shell can accept it.
    GroupOperationFinished {
        /// Shell request identity, not the backend command identifier.
        request: u64,
        /// Successful, refused, failed, or explicitly unconfirmed.
        status: super::GroupOperationStatus,
    },
    /// Successfully persisted receiver balance settings.
    ReceiverLevelsChanged {
        levels: std::collections::BTreeMap<DeviceId, f32>,
    },
    DiscoveryCompleted {
        generation: GenerationId,
        receivers: Vec<ReceiverState>,
    },
    DiscoveryFailed {
        generation: GenerationId,
        summary: String,
    },
    DesiredReceiversChanged {
        desired_revision: u64,
        receiver_ids: Vec<DeviceId>,
    },
    SessionStarted {
        generation: GenerationId,
        active_receiver_ids: Vec<DeviceId>,
    },
    /// The session is running but is not delivering to every member it was
    /// asked to, or has lost its capture source.
    SessionDegraded {
        generation: GenerationId,
        active_receiver_ids: Vec<DeviceId>,
    },
    /// The controller is rebuilding the whole group inside the running
    /// session, without the user having asked for it.
    ///
    /// Carries no active membership on purpose: during a full-group restart
    /// there is none, and the reason the restart was needed stays with the
    /// controller -- see `backend_bridge::session_echo`.
    SessionRestarting {
        generation: GenerationId,
    },
    SessionStopped {
        generation: GenerationId,
    },
    SessionFailed {
        generation: GenerationId,
        summary: String,
    },
    VolumeApplied {
        generation: GenerationId,
        volume: f32,
    },
    /// The backend's own mute state.
    ///
    /// Carries no generation, for the reason `DiagnosticsUpdated` states: it
    /// answers no press. It is the settled state of a backend setting, and
    /// the newest one always wins -- which is also what repairs a request the
    /// backend refused, without the shell having to model a refusal.
    MuteChanged {
        muted: bool,
    },
    /// The backend's selected latency profile and the profiles it offers.
    ///
    /// The reason a profile is refused crosses as
    /// [`super::LatencyUnavailable`], never as the backend's own sentence;
    /// see `backend_bridge::latency_echo`.
    LatencyChanged {
        reading: LatencyReading,
    },
    /// The backend's capture source: what is offered, what is chosen, what is
    /// actually being captured, and what capture is doing.
    ///
    /// Raw Windows endpoint IDs stay in `backend_bridge`, which hands out
    /// opaque [`super::AudioEndpointKey`]s instead and translates them back
    /// on the way out.
    AudioSourceChanged {
        reading: AudioSourceReading,
    },
    /// A fresh reading of the backend's diagnostics registry.
    ///
    /// Carries no generation, and that is not an omission. Every other event
    /// here answers something the user pressed and has to be matched against
    /// the press that asked for it; a diagnostics reading answers nothing.
    /// It is the newest state of a thing that runs on its own, and the newest
    /// one always wins.
    ///
    /// What a [`DiagnosticsReading`] may contain, and what the registry keeps
    /// to itself, is decided in `backend_bridge::diagnostics_reading`.
    DiagnosticsUpdated {
        reading: DiagnosticsReading,
    },
    LiveDiagnosticsUpdated {
        reading: super::LiveDiagnosticsReading,
    },
    ChannelClosed {
        summary: String,
    },
}

/// Typed outcomes of the settings file.
///
/// The write outcomes carry a generation and nothing else. The concrete I/O
/// error is logged in the worker where it happens, so no path, handle, or
/// operating-system message can travel into app state, a snapshot, or a
/// renderer by way of an event.
#[derive(Clone, Debug, PartialEq)]
pub enum PreferencesEvent {
    Loaded {
        value: Preferences,
        notice: Option<UserNotice>,
    },
    LoadFailed {
        summary: String,
    },
    Persisted {
        generation: GenerationId,
    },
    PersistFailed {
        generation: GenerationId,
    },
    HotkeyReconfigured {
        generation: GenerationId,
        applied: HotkeyBinding,
        failure: Option<String>,
    },
}
