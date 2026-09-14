//! Localization keys, German and English catalogs, and the locale-aware
//! templates for the few sentences that carry numbers.
//!
//! Rules this module enforces by construction:
//!
//! * Every visible string is addressed by a [`TextKey`]. Components receive
//!   keys or already-resolved copy; they never embed a language-specific
//!   sentence.
//! * [`Catalog::text`] resolves through exhaustive matches without a wildcard
//!   arm, so a new key cannot compile until both languages have copy for it.
//! * Catalog copy is `&'static str` with no placeholder syntax at all. There
//!   is therefore no seam through which a backend string -- a
//!   `UserFacingError`, a `CommandFailed` reason, a path, or an address --
//!   could reach visible copy, an accessibility label, the clipboard, or a
//!   support export.
//! * The only dynamic sentences take the typed argument structs below, whose
//!   fields are counts, percentages, durations, and already-presentation-safe
//!   name/value pairs.

use crate::app::ResolvedLocale;

/// Declares the key enum and `TextKey::ALL` from one list, so `ALL` contains
/// every variant exactly once in declaration order by construction.
macro_rules! text_keys {
    ($($key:ident),+ $(,)?) => {
        /// A stable identifier for one visible string.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum TextKey {
            $($key),+
        }

        impl TextKey {
            /// Every key in declaration order.
            pub const ALL: &'static [TextKey] = &[$(TextKey::$key),+];
        }
    };
}

text_keys! {
    Overview, Speakers, Groups, Audio, Diagnostics, Settings,
    HardwareCheck, HardwarePrepare, HardwareRequirements, HardwareLower, HardwareStart,
    HardwareHeard, HardwareNotHeard, HardwarePending, HardwarePass, HardwareFail,
    HardwareInvalid, HardwareLimitations, HardwareReport, HardwareSnapshot, HardwareEventNote,
    HardwareTitle, HardwareBack, HardwareHelp, HardwareHideHelp, HardwarePreparation,
    HardwareListening,
    HardwareSourceNotice, HardwareSilent, HardwareUnavailable, HardwareBusy,
    HardwareSelection, HardwareSelectionPending, HardwareOffline, HardwareLevels,
    HardwareMuted, HardwareLatency, HardwareVolumePending, HardwareVolume,
    HardwareSource, HardwareConfirmation, HardwareReady, HardwareBackActive,
    Status, Receivers, AudioRoute, SelectedChanges,
    Ready, Connecting, Streaming, Reconnecting, Stopping, NeedsAttention, Degraded,
    ReadyExplanation, ConnectingExplanation, StreamingExplanation, DegradedExplanation,
    RestartingExplanation, StoppingExplanation, FailedExplanation,
    StartStreaming, StopStreaming, Cancel, StopTrying, Retry, Refresh,
    ApplyChanges, Discard, Dismiss,
    SelectionApplying, OperationPending, SelectionBusy, SelectionInvalid,
    SelectionSaveFailed, SelectionClosed, SelectionUnconfirmed,
    WindowsAudio, RouteIdle, RoutePending, RouteLive, RouteFault,
    RouteToSelectedReceivers, AvailableReceivers, DiscoveringReceivers,
    RefreshReceivers, NoReceiversTitle, NoReceiversBody,
    ReceiverAvailable, ReceiverUnavailable, ReceiverSelected, ReceiverActive,
    ReceiverSelectedAndActive, SelectReceiver, DeselectReceiver, ReceiverUnnamed,
    DeviceClassHomePod, DeviceClassHomePodMini, DeviceClassAppleTv, DeviceClassSpeaker,
    MasterVolume, MasterVolumeAccessibility, ReceiverLevel, ReceiverLevelAccessibility, ReceiverLevelHelp, ChangeSummary,
    SpeakersEmptyTitle, SpeakersEmptyBody, GroupsEmptyTitle, GroupsEmptyBody,
    GroupsLoading, CreateGroup, EditGroup, DeleteGroup, ApplyGroup, ApplyGroupAndStart,
    SaveGroup, GroupName, GroupMembers, GroupMemberOffline, GroupNameRequired,
    GroupMemberRequired, GroupNameTooLong, GroupNameDuplicate, GroupOperationPending,
    GroupSaveSucceeded, GroupDeleteSucceeded, GroupApplySucceeded, GroupApplyAndStartSucceeded,
    GroupBusy, GroupValidationFailed, GroupPersistenceFailed, GroupClosed, GroupUnconfirmed,
    KeepSelectionDraft, DiscardSelectionAndApply, GroupApplySelectionConflict,
    ConfirmDelete, DismissGroupResult,
    AudioEmptyTitle, AudioEmptyBody,
    AudioMute, AudioMuteDescription,
    AudioLatency, AudioLatencyDescription,
    LatencyLow, LatencyNormal, LatencyStable, LatencyNotValidated,
    AudioCaptureSource, AudioCaptureSourceDescription, AudioCaptureSystemDefault,
    AudioCaptureActiveDevice, AudioCaptureStatus, AudioCaptureSelectionMissing,
    AudioCaptureSourceFixed, AudioCaptureInventoryUnknown, AudioCaptureSelectionUnverified,
    AudioCaptureInventoryFailed, AudioCaptureRefreshFailed,
    CaptureCapturing, CaptureSilentSystem, CaptureRecovering,
    CaptureUnavailable, CaptureFailed,
    DiagnosticsEmptyTitle, DiagnosticsEmptyBody,
    AdvancedMetricsUnavailable,
    DiagnosticsMeasurements, DiagnosticsOverallHealth, DiagnosticsAudioSource,
    DiagnosticsHealthRunning, DiagnosticsHealthError,
    DiagnosticsEventsLost, DiagnosticsMeasuredReceivers,
    LiveDiagnostics, DiagnosticsIdle, DiagnosticsNoSignal, DiagnosticsSignalUnknown,
    DiagnosticsHistoricalErrors, DiagnosticsHistoricalCapture, DiagnosticsHistoricalBuffer,
    DiagnosticsCurrentFailure, DiagnosticsCurrentRecovering,
    DiagnosticsCurrentConnecting, DiagnosticsLocalHealthy, DiagnosticsInputSignal,
    DiagnosticsAwaitingSend, DiagnosticsSourceUnavailable, DiagnosticsSourceRecovering,
    DiagnosticsShowDetails, DiagnosticsHideDetails, DiagnosticsBuffer, DiagnosticsBufferFill,
    DiagnosticsBufferedAudio, DiagnosticsBufferUnderruns, DiagnosticsCaptureDrops,
    DiagnosticsReceivers, DiagnosticsPacketsAccepted, DiagnosticsBytesAccepted,
    DiagnosticsSendFailures, DiagnosticsRetransmitRequests, DiagnosticsCounterScope,
    DiagnosticsRetransmitDisclaimer, DiagnosticsLocalOnly, Frames,
    DiagnosticsIssuePairing, DiagnosticsIssueSetup, DiagnosticsIssueCapture,
    DiagnosticsIssueTiming, DiagnosticsIssueTransport, DiagnosticsIssueFeedback,
    DiagnosticsIssueTeardown, DiagnosticsIssueOther,
    Appearance, AppearanceDescription, ThemeSystem, ThemeLight, ThemeDark,
    HighContrastControlledByWindows,
    Language, LanguageDescription, LanguageSystem, LanguageGerman, LanguageEnglish,
    Keyboard, GlobalShortcut, GlobalShortcutDescription, ShortcutEnabled,
    ShortcutControl, ShortcutAlt, ShortcutShift, ShortcutWindows,
    ShortcutKey, ShortcutKeyAccessibility, ShortcutKeyTooltip,
    ApplyShortcut, ActiveShortcut, ShortcutDisabled, ShortcutChangePending,
    ShortcutNeedsModifier, ShortcutInvalidKey,
    AdvancedInformation, AdvancedInformationDescription, On, Off,
    About, AboutDescription, Version, License, ThirdPartyNotices,
    OpenLicense, OpenThirdPartyNotices, CopyLicenseText, CopyNoticesText,
    CloseAboutViewer, CopiedToClipboard,
    SupportProject, SupportProjectDescription, SupportWithPaypal, SupportBrowserHint,
    Available, Unavailable, Unknown, Stale, Selected, Active,
    SessionMembershipIncluded, SessionMembershipExcluded,
    PreferenceSaveFailed, PreferenceSaveConsequence, PreferenceRetry,
    NoReceiverAvailableNotice, DiscoveryFailedNotice, SessionFailedNotice,
    ControllerUnavailableNotice, HotkeyFailedNotice, InvalidInputNotice,
    CurrentSettingsRemainActive, TryRefreshNext, TryRetryNext, OpenSettingsNext,
    CompactDestinationTooltip, NavigationDestinationAccessibility,
    RouteSummaryAccessibility, ReceiverSelectionAccessibility,
    VolumeAccessibility, StatusFooterAccessibility,
}

/// How many receivers a list or count line is talking about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiverCountArgs {
    pub count: usize,
}

/// How the staged selection differs from the applied one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChangeSummaryArgs {
    pub added: usize,
    pub removed: usize,
}

/// How many receivers are selected and how many actually carry audio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSummaryArgs {
    pub selected: usize,
    pub active: usize,
}

/// A whole-number percentage, already rounded by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PercentageArgs {
    pub percent: u8,
}

/// A signal peak in tenths of one percent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalPercentageArgs {
    pub per_mille: u16,
}

/// A measured duration in milliseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurationArgs {
    pub milliseconds: u64,
}

/// A label/value pair for Advanced information.
///
/// Both halves must already be presentation-safe: a localized label from this
/// catalog or a receiver/group name, and a formatted measurement. Backend
/// error text, paths, and addresses never qualify.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamedValueArgs<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Resolves keys and templates for one locale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Catalog {
    locale: ResolvedLocale,
}

impl Catalog {
    /// Creates the catalog for an already-resolved locale.
    pub const fn new(locale: ResolvedLocale) -> Self {
        Self { locale }
    }

    /// The locale this catalog renders.
    pub const fn locale(self) -> ResolvedLocale {
        self.locale
    }

    /// The copy for one key.
    pub const fn text(self, key: TextKey) -> &'static str {
        match self.locale {
            ResolvedLocale::German => german(key),
            ResolvedLocale::English => english(key),
        }
    }

    /// "3 receivers" / "3 Empfänger", with the locale's zero and singular
    /// forms. German keeps one noun form for both numbers.
    pub fn receiver_count(self, args: ReceiverCountArgs) -> String {
        let ReceiverCountArgs { count } = args;
        match (self.locale, count) {
            (ResolvedLocale::German, 0) => "Keine Empfänger".to_owned(),
            (ResolvedLocale::German, _) => format!("{count} Empfänger"),
            (ResolvedLocale::English, 0) => "No receivers".to_owned(),
            (ResolvedLocale::English, 1) => "1 receiver".to_owned(),
            (ResolvedLocale::English, _) => format!("{count} receivers"),
        }
    }

    /// A destructive confirmation that names the exact saved template.
    pub fn group_delete_confirmation(self, name: &str) -> String {
        match self.locale {
            ResolvedLocale::German => format!("Gruppe \"{name}\" wirklich löschen?"),
            ResolvedLocale::English => format!("Delete group \"{name}\"?"),
        }
    }

    /// Counts currently available members without hiding stored offline members.
    pub fn group_availability(self, available: usize, total: usize) -> String {
        match self.locale {
            ResolvedLocale::German => format!("{available} von {total} verfügbar"),
            ResolvedLocale::English => format!("{available} of {total} available"),
        }
    }

    /// What Apply would change about the current selection.
    pub fn change_summary(self, args: ChangeSummaryArgs) -> String {
        let ChangeSummaryArgs { added, removed } = args;
        match (added, removed) {
            (0, 0) => match self.locale {
                ResolvedLocale::German => "Keine Änderungen".to_owned(),
                ResolvedLocale::English => "No changes".to_owned(),
            },
            (_, 0) => self.added_phrase(added),
            (0, _) => self.removed_phrase(removed),
            _ => format!(
                "{}, {}",
                self.added_phrase(added),
                self.removed_phrase(removed)
            ),
        }
    }

    fn added_phrase(self, count: usize) -> String {
        match self.locale {
            ResolvedLocale::German => format!("{count} Lautsprecher hinzugefügt"),
            ResolvedLocale::English => format!("{count} {} added", speaker_noun(count)),
        }
    }

    fn removed_phrase(self, count: usize) -> String {
        match self.locale {
            ResolvedLocale::German => format!("{count} Lautsprecher entfernt"),
            ResolvedLocale::English => format!("{count} {} removed", speaker_noun(count)),
        }
    }

    /// How many receivers are selected and how many are streaming.
    pub fn route_summary(self, args: RouteSummaryArgs) -> String {
        let RouteSummaryArgs { selected, active } = args;
        match (self.locale, selected) {
            (ResolvedLocale::German, 0) => "Keine Lautsprecher ausgewählt".to_owned(),
            (ResolvedLocale::German, _) => {
                format!("{selected} ausgewählt, {active} im Streaming")
            }
            (ResolvedLocale::English, 0) => "No speakers selected".to_owned(),
            (ResolvedLocale::English, _) => format!("{selected} selected, {active} streaming"),
        }
    }

    /// A percentage in the locale's typography: German sets a space before the
    /// sign, English does not.
    pub fn percentage(self, args: PercentageArgs) -> String {
        let PercentageArgs { percent } = args;
        match self.locale {
            ResolvedLocale::German => format!("{percent} %"),
            ResolvedLocale::English => format!("{percent}%"),
        }
    }

    /// A measured signal peak with one decimal only when it is needed.
    pub fn signal_percentage(self, args: SignalPercentageArgs) -> String {
        let per_mille = args.per_mille.min(1_000);
        let (whole, fraction) = (per_mille / 10, per_mille % 10);
        match (self.locale, fraction) {
            (ResolvedLocale::German, 0) => format!("{whole} %"),
            (ResolvedLocale::German, _) => format!("{whole},{fraction} %"),
            (ResolvedLocale::English, 0) => format!("{whole}%"),
            (ResolvedLocale::English, _) => format!("{whole}.{fraction}%"),
        }
    }

    /// A duration with its unit and the locale's decimal separator.
    ///
    /// Below one second the value stays in whole milliseconds; above it the
    /// value is rounded to one decimal with integer arithmetic, so the output
    /// is bit-identical on every machine.
    pub fn duration(self, args: DurationArgs) -> String {
        let DurationArgs { milliseconds } = args;
        if milliseconds < 1_000 {
            return format!("{milliseconds} ms");
        }
        let tenths = (milliseconds + 50) / 100;
        let (whole, fraction) = (tenths / 10, tenths % 10);
        match self.locale {
            ResolvedLocale::German => format!("{whole},{fraction} s"),
            ResolvedLocale::English => format!("{whole}.{fraction} s"),
        }
    }

    /// A label and its already-safe value.
    ///
    /// This joins; it never reformats. Callers must pass presentation-safe
    /// halves -- catalog copy, a receiver or group name, or a value produced
    /// by [`Catalog::percentage`] or [`Catalog::duration`].
    pub fn named_value(self, args: NamedValueArgs<'_>) -> String {
        let NamedValueArgs { name, value } = args;
        format!("{name}: {value}")
    }

    /// Joins already-presentation-safe names with the locale's conjunction.
    ///
    /// Like [`Catalog::named_value`] this joins and never reformats, so the
    /// caller stays responsible for handing over receiver or group names
    /// rather than backend text.
    pub fn name_list(self, names: &[&str]) -> String {
        let conjunction = match self.locale {
            ResolvedLocale::German => "und",
            ResolvedLocale::English => "and",
        };
        match names {
            [] => String::new(),
            [single] => (*single).to_owned(),
            [head @ .., last] => format!("{} {conjunction} {last}", head.join(", ")),
        }
    }

    /// What the collapsed `+N` route node stands for.
    pub fn more_receivers(self, args: ReceiverCountArgs) -> String {
        let ReceiverCountArgs { count } = args;
        match (self.locale, count) {
            (ResolvedLocale::German, 1) => "1 weiterer Empfänger".to_owned(),
            (ResolvedLocale::German, _) => format!("{count} weitere Empfänger"),
            (ResolvedLocale::English, 1) => "1 more receiver".to_owned(),
            (ResolvedLocale::English, _) => format!("{count} more receivers"),
        }
    }

    /// The short marker painted inside the collapsed route node.
    ///
    /// A sign and a number, identical in both locales: the node is a few
    /// points wide, and the sentence it stands for lives in
    /// [`Catalog::more_receivers`].
    pub fn overflow_marker(self, args: ReceiverCountArgs) -> String {
        let ReceiverCountArgs { count } = args;
        let _ = self.locale;
        format!("+{count}")
    }
}

const fn speaker_noun(count: usize) -> &'static str {
    if count == 1 {
        "speaker"
    } else {
        "speakers"
    }
}

const fn german(key: TextKey) -> &'static str {
    match key {
        TextKey::HardwareCheck => "Hardwareprüfung öffnen",
        TextKey::HardwareTitle => "Hardwareprüfung",
        TextKey::HardwareBack => "Zurück zu Audio",
        TextKey::HardwareHelp => "Hilfe anzeigen",
        TextKey::HardwareHideHelp => "Hilfe ausblenden",
        TextKey::HardwarePreparation => "Hörtest vorbereiten",
        TextKey::HardwareListening => {
            "Testton angefordert. War er auf allen ausgewählten Boxen hörbar?"
        }
        TextKey::HardwareSourceNotice => "Der leise Testton endet nach etwa 2 Sekunden. Danach läuft Windows-Audio weiter; die Sitzung bleibt bis Gehört, Nicht gehört oder Abbrechen geöffnet.",
        TextKey::HardwareSilent => "Windows meldet eine stille Audioquelle. Prüfe, ob Musik oder ein Video auf dem gewählten Gerät läuft.",
        TextKey::HardwareUnavailable => "Die App ist momentan nicht bereit. Prüfe den Sitzungsstatus in der Übersicht.",
        TextKey::HardwareBusy => "Streaming läuft bereits. Stoppe es zuerst; danach kannst du den Hörtest starten.",
        TextKey::HardwareSelection => "Wähle mindestens einen Lautsprecher aus und übernimm die Auswahl.",
        TextKey::HardwareSelectionPending => "Übernimm offene Änderungen an der Lautsprecherauswahl und warte auf die Bestätigung.",
        TextKey::HardwareOffline => "Mindestens ein ausgewählter Lautsprecher ist offline. Prüfe die Auswahl unter Lautsprecher.",
        TextKey::HardwareLevels => "Die Einzelpegel müssen bekannt und größer als null sein. Prüfe sie unter Lautsprecher.",
        TextKey::HardwareMuted => "Schalte die Stummschaltung unter Audio aus und warte auf die Bestätigung.",
        TextKey::HardwareLatency => "Wähle unter Audio das verfügbare Latenzprofil Normal.",
        TextKey::HardwareVolumePending => "Die Lautstärkeänderung wird noch bestätigt. Bitte kurz warten.",
        TextKey::HardwareVolume => "Setze die Masterlautstärke auf 10 Prozent und warte auf die Bestätigung.",
        TextKey::HardwareSource => "Die Windows-Audioquelle ist nicht bereit. Prüfe das Wiedergabegerät unter Audio.",
        TextKey::HardwareConfirmation => "Bestätige zuerst die geprüfte Auswahl und die niedrige Lautstärke.",
        TextKey::HardwareReady => "Bereit für den Hörtest. Die Wiedergabe beginnt erst nach deinem Klick.",
        TextKey::HardwareBackActive => "Zurück navigiert nur. Zum Beenden des laufenden Hörtests wähle Abbrechen.",
        TextKey::HardwarePrepare => "Auswahl und niedrige Lautstärke geprüft",
        TextKey::HardwareRequirements => "Vorbereitung: Auswahl vollständig übernehmen und alle Boxen online halten; Einzelpegel über 0, Normal-Profil und nicht stumm. Masterlautstärke bestätigt über 0 und höchstens 15 Prozent. Der Testton läuft etwa 2 Sekunden mit niedrigem Pegel.",
        TextKey::HardwareLower => "Masterlautstärke auf 10 Prozent setzen",
        TextKey::HardwareStart => "Hörtest mit Testton starten",
        TextKey::HardwareHeard => "Alle ausgewählten Boxen gehört – stoppen",
        TextKey::HardwareNotHeard => "Nicht alle Boxen gehört – stoppen",
        TextKey::HardwarePending => "Stopp ausstehend – Backend-Bestätigung abwarten. Test noch nicht abgeschlossen.",
        TextKey::HardwarePass => "Hören aller ausgewählten Boxen bestätigt; Wiedergabe gestoppt.",
        TextKey::HardwareFail => "Nicht alle ausgewählten Boxen gehört; Wiedergabe gestoppt.",
        TextKey::HardwareInvalid => "Versuch abgebrochen oder durch Änderungen ungültig; kein bestätigtes Ergebnis.",
        TextKey::HardwareLimitations => "Manueller Hörtest ohne Mikrofonmessung. Der Testton misst weder Schall noch akustischen Versatz oder Audiopaketverlust und schaltet keine Profile dauerhaft frei.",
        TextKey::HardwareReport => "Diagnose-Momentaufnahme kopieren",
        TextKey::HardwareSnapshot => "Aktuelle Momentaufnahme – keine dauerhafte Hardwarevalidierung",
        TextKey::HardwareEventNote => "Verlorene Diagnoseereignisse sind keine verlorenen Audiopakete.",
        TextKey::Overview => "Übersicht",
        TextKey::Speakers => "Lautsprecher",
        TextKey::Groups => "Gruppen",
        TextKey::Audio => "Audio",
        TextKey::Diagnostics => "Diagnose",
        TextKey::Settings => "Einstellungen",
        TextKey::Status => "Status",
        TextKey::Receivers => "Empfänger",
        TextKey::AudioRoute => "Audiopfad",
        TextKey::SelectedChanges => "Ausgewählte Änderungen",
        TextKey::Ready => "Bereit",
        TextKey::Connecting => "Verbindung wird aufgebaut",
        TextKey::Streaming => "Streaming",
        TextKey::Reconnecting => "Verbindung wird wiederhergestellt",
        TextKey::Stopping => "Streaming wird beendet",
        TextKey::NeedsAttention => "Aufmerksamkeit erforderlich",
        TextKey::Degraded => "Eingeschränkt",
        TextKey::ReadyExplanation => "Lautsprecher auswählen, danach das Streaming starten.",
        TextKey::ConnectingExplanation => {
            "Die Verbindung zu den ausgewählten Lautsprechern wird aufgebaut."
        }
        TextKey::StreamingExplanation => {
            "Der Ton dieses PCs läuft auf den ausgewählten Lautsprechern."
        }
        TextKey::DegradedExplanation => {
            "Die Wiedergabe läuft weiter, aber nicht jeder ausgewählte Lautsprecher \
             erhält Ton."
        }
        TextKey::RestartingExplanation => "Die Verbindung ist abgerissen und wird neu aufgebaut.",
        TextKey::StoppingExplanation => "Die Sitzung wird beendet.",
        TextKey::FailedExplanation => {
            "Das Streaming wurde beendet. Die getroffene Auswahl bleibt erhalten."
        }
        TextKey::StartStreaming => "Streaming starten",
        TextKey::StopStreaming => "Streaming stoppen",
        TextKey::Cancel => "Abbrechen",
        TextKey::StopTrying => "Vorgang beenden",
        TextKey::Retry => "Erneut versuchen",
        TextKey::Refresh => "Aktualisieren",
        TextKey::ApplyChanges => "Änderungen anwenden",
        TextKey::SelectionApplying => "Auswahl wird angewendet …",
        TextKey::OperationPending => "Vorgang läuft …",
        TextKey::SelectionBusy => "Belegt. Erneut versuchen.",
        TextKey::SelectionInvalid => "Auswahl abgelehnt. Bitte prüfen.",
        TextKey::SelectionSaveFailed => "Speichern fehlgeschlagen. Erneut versuchen.",
        TextKey::SelectionClosed => "Verbindung beendet. Ergebnis unbestätigt.",
        TextKey::SelectionUnconfirmed => "Ergebnis unbestätigt. Auswahl prüfen.",
        TextKey::Discard => "Verwerfen",
        TextKey::Dismiss => "Ausblenden",
        TextKey::WindowsAudio => "Windows-Audio",
        TextKey::RouteIdle => "Pfad inaktiv",
        TextKey::RoutePending => "Pfad wird aufgebaut",
        TextKey::RouteLive => "Pfad aktiv",
        TextKey::RouteFault => "Pfad gestört",
        TextKey::RouteToSelectedReceivers => "Von diesem PC zu den ausgewählten Lautsprechern",
        TextKey::AvailableReceivers => "Verfügbare Empfänger",
        TextKey::DiscoveringReceivers => "Empfänger werden gesucht",
        TextKey::RefreshReceivers => "Empfängerliste aktualisieren",
        TextKey::NoReceiversTitle => "Keine Empfänger gefunden",
        TextKey::NoReceiversBody => {
            "Die Lautsprecher müssen eingeschaltet und im selben Netzwerk erreichbar sein. \
             Danach erneut suchen."
        }
        TextKey::ReceiverAvailable => "Lautsprecher verfügbar",
        TextKey::ReceiverUnavailable => "Lautsprecher nicht verfügbar",
        TextKey::ReceiverSelected => "Für das Streaming ausgewählt",
        TextKey::ReceiverActive => "Streaming läuft",
        TextKey::ReceiverSelectedAndActive => "Ausgewählt, Streaming läuft",
        TextKey::DeviceClassHomePod => "HomePod",
        TextKey::DeviceClassHomePodMini => "HomePod mini",
        TextKey::DeviceClassAppleTv => "Apple TV",
        // The honest fallback: an AirPlay receiver whose hardware identifier
        // names no class we recognize is still a speaker, and saying so beats
        // guessing a product name.
        TextKey::DeviceClassSpeaker => "Lautsprecher",
        // A receiver the app knows it must show but has no name for: a
        // selected speaker restored from the last session that discovery has
        // not seen yet, typically because it is switched off. Its stable
        // identity is a MAC address and must never stand in for a name.
        TextKey::ReceiverUnnamed => "Unbenannter Lautsprecher",
        TextKey::SelectReceiver => "Lautsprecher auswählen",
        TextKey::DeselectReceiver => "Auswahl aufheben",
        TextKey::MasterVolume => "Gesamtlautstärke",
        TextKey::MasterVolumeAccessibility => "Gesamtlautstärke für alle ausgewählten Lautsprecher",
        TextKey::ReceiverLevel => "Lautstärke",
        TextKey::ReceiverLevelAccessibility => "Relative Lautstärke",
        TextKey::ReceiverLevelHelp => "Anteil an der Gesamtlautstärke",
        TextKey::ChangeSummary => "Zusammenfassung der Änderungen",
        TextKey::SpeakersEmptyTitle => "Noch keine Lautsprecher bekannt",
        TextKey::SpeakersEmptyBody => {
            "Sobald AirPlay-Lautsprecher im Netzwerk antworten, erscheinen sie hier."
        }
        TextKey::GroupsEmptyTitle => "Noch keine Gruppen gespeichert",
        TextKey::GroupsEmptyBody => {
            "Speichere häufig genutzte Lautsprecherkombinationen mit ihren Pegeln."
        }
        TextKey::GroupsLoading => "Gespeicherte Gruppen werden geladen…",
        TextKey::CreateGroup => "Gruppe erstellen",
        TextKey::EditGroup => "Bearbeiten",
        TextKey::DeleteGroup => "Löschen",
        TextKey::ApplyGroup => "Übernehmen",
        TextKey::ApplyGroupAndStart => "Übernehmen und starten",
        TextKey::SaveGroup => "Gruppe speichern",
        TextKey::GroupName => "Name der Gruppe",
        TextKey::GroupMembers => "Mitglieder",
        TextKey::GroupMemberOffline => "Nicht verfügbar",
        TextKey::GroupNameRequired => "Gib einen Gruppennamen ein.",
        TextKey::GroupMemberRequired => "Wähle mindestens einen Lautsprecher.",
        TextKey::GroupNameTooLong => "Der Gruppenname darf höchstens 64 Zeichen haben.",
        TextKey::GroupNameDuplicate => "Dieser Gruppenname wird bereits verwendet.",
        TextKey::GroupOperationPending => "Gruppenaktion wird ausgeführt…",
        TextKey::GroupSaveSucceeded => "Gruppe gespeichert.",
        TextKey::GroupDeleteSucceeded => "Gruppe gelöscht.",
        TextKey::GroupApplySucceeded => "Gruppe übernommen.",
        TextKey::GroupApplyAndStartSucceeded => "Gruppe übernommen; Wiedergabe wurde angefordert.",
        TextKey::GroupBusy => "Beschäftigt. Erneut versuchen.",
        TextKey::GroupValidationFailed => "Gruppe wurde abgelehnt. Eingaben prüfen.",
        TextKey::GroupPersistenceFailed => "Speichern fehlgeschlagen. Erneut versuchen.",
        TextKey::GroupClosed => "Verbindung geschlossen. Ergebnis nicht bestätigt.",
        TextKey::GroupUnconfirmed => "Ergebnis nicht bestätigt. Gespeicherte Gruppen prüfen.",
        TextKey::KeepSelectionDraft => "Auswahlentwurf behalten",
        TextKey::DiscardSelectionAndApply => "Entwurf verwerfen und übernehmen",
        TextKey::GroupApplySelectionConflict => "Ein Auswahlentwurf ist noch nicht angewendet.",
        TextKey::ConfirmDelete => "Löschen bestätigen",
        TextKey::DismissGroupResult => "Hinweis schließen",
        TextKey::AudioEmptyTitle => "Windows-Audio hat noch nichts übermittelt",
        TextKey::AudioEmptyBody => {
            "Aufnahmegerät, Stummschaltung und Latenzvorgabe erscheinen hier, sobald die \
             Audiosteuerung ihren Zustand übermittelt. Die Gesamtlautstärke liegt auf \
             der Übersicht."
        }
        TextKey::AudioMute => "Stummschaltung",
        TextKey::AudioMuteDescription => {
            "Schaltet die Wiedergabe stumm, ohne die eingestellten Pegel zu ändern."
        }
        TextKey::AudioLatency => "Latenzvorgabe",
        TextKey::AudioLatencyDescription => "Bestimmt, wie viel Vorlauf der Sender puffert.",
        TextKey::LatencyLow => "Niedrig",
        TextKey::LatencyNormal => "Normal",
        TextKey::LatencyStable => "Stabil",
        TextKey::LatencyNotValidated => "Noch nicht freigegeben; der manuelle Hörtest schaltet dieses Profil nicht frei",
        TextKey::AudioCaptureSource => "Windows-Wiedergabegerät",
        TextKey::AudioCaptureSourceDescription => {
            "Überträgt den Systemton dieses Windows-Wiedergabegeräts per Loopback."
        }
        TextKey::AudioCaptureSystemDefault => "Windows-Standard",
        TextKey::AudioCaptureActiveDevice => "Aktuell erfasstes Wiedergabegerät",
        TextKey::AudioCaptureStatus => "Aufnahmezustand",
        TextKey::AudioCaptureSelectionMissing => "Das gewählte Gerät fehlt",
        TextKey::AudioCaptureSourceFixed => {
            "Windows hat bei der letzten Abfrage kein Wiedergabegerät gemeldet."
        }
        TextKey::AudioCaptureInventoryFailed => "Die Windows-Wiedergabegeräte konnten nicht ermittelt werden.",
        TextKey::AudioCaptureRefreshFailed => "Die Geräteliste konnte nicht aktualisiert werden. Angezeigt wird die letzte erfolgreiche Abfrage.",
        TextKey::AudioCaptureInventoryUnknown => {
            "Die Windows-Wiedergabegeräte wurden noch nicht ermittelt."
        }
        TextKey::AudioCaptureSelectionUnverified => {
            "Geräteauswahl noch nicht durch die Geräteliste bestätigt"
        }
        TextKey::CaptureCapturing => "Nimmt auf",
        TextKey::CaptureSilentSystem => "System ohne Ton",
        TextKey::CaptureRecovering => "Wiederherstellung läuft",
        TextKey::CaptureUnavailable => "Keine Aufnahmequelle",
        TextKey::CaptureFailed => "Aufnahme fehlgeschlagen",
        TextKey::DiagnosticsEmptyTitle => "Noch keine Diagnosedaten",
        TextKey::DiagnosticsEmptyBody => "Messwerte erscheinen, sobald eine Sitzung läuft.",
        TextKey::AdvancedMetricsUnavailable => {
            "Für diesen Bereich liegen derzeit keine erweiterten Messwerte vor."
        }
        TextKey::DiagnosticsMeasurements => "Messwerte",
        TextKey::DiagnosticsOverallHealth => "Gesamtzustand",
        TextKey::DiagnosticsAudioSource => "Ausgewählte oder erfasste Windows-Audioquelle",
        TextKey::DiagnosticsHealthRunning => "Läuft",
        TextKey::DiagnosticsHealthError => "Fehler",
        TextKey::DiagnosticsEventsLost => "Verlorene Diagnoseereignisse",
        TextKey::DiagnosticsMeasuredReceivers => "Empfänger mit Messwerten",
        TextKey::LiveDiagnostics => "Aktuelle Diagnose",
        TextKey::DiagnosticsIdle => "Starte das Streaming, um Transportdiagnosen zu sehen.",
        TextKey::DiagnosticsNoSignal => "Derzeit wird kein Windows-Eingangssignal gemessen. Starte Ton auf der ausgewählten Quelle.",
        TextKey::DiagnosticsSignalUnknown => "Noch keine Eingangsmessung. Lass Audio laufen und prüfe erneut.",
        TextKey::DiagnosticsHistoricalErrors => "In dieser Sitzung wurden lokale Transportfehler aufgezeichnet. Falls der Ton aussetzt, prüfe die Lautsprecherverbindung.",
        TextKey::DiagnosticsHistoricalCapture => "Seit dem Programmstart wurden PCM-Frames verworfen. Falls der Ton aussetzt, prüfe die Windows-Audioquelle.",
        TextKey::DiagnosticsHistoricalBuffer => "In dieser Sitzung wurden Pufferunterläufe aufgezeichnet. Falls der Ton aussetzt, versuche das Latenzprofil Normal.",
        TextKey::DiagnosticsCurrentFailure => "Mindestens ein Lautsprecher ist derzeit offline oder ausgefallen. Prüfe Verbindung und Auswahl.",
        TextKey::DiagnosticsCurrentRecovering => "Mindestens eine Lautsprecherverbindung wird gerade wiederhergestellt. Warte kurz und prüfe erneut.",
        TextKey::DiagnosticsCurrentConnecting => "Die Lautsprecherverbindung wird aufgebaut. Warte kurz und prüfe erneut.",
        TextKey::DiagnosticsLocalHealthy => "Windows-Eingang wird gemessen und lokale Sendepakete wurden aufgezeichnet. Akustische Wiedergabe ist nicht bestätigt.",
        TextKey::DiagnosticsAwaitingSend => "Windows-Eingangssignal ist vorhanden. Lokale Sendemesswerte stehen noch aus.",
        TextKey::DiagnosticsSourceUnavailable => "Die Windows-Audioquelle ist nicht verfügbar. Prüfe das gewählte Wiedergabegerät unter Audio.",
        TextKey::DiagnosticsSourceRecovering => "Die Windows-Audioquelle wird wiederhergestellt. Warte kurz und prüfe erneut.",
        TextKey::DiagnosticsInputSignal => "Eingangssignal",
        TextKey::DiagnosticsShowDetails => "Detaillierte Zähler anzeigen",
        TextKey::DiagnosticsHideDetails => "Detaillierte Zähler ausblenden",
        TextKey::DiagnosticsBuffer => "Audiopuffer",
        TextKey::DiagnosticsBufferFill => "Pufferfüllung",
        TextKey::DiagnosticsBufferedAudio => "Gepuffertes Audio",
        TextKey::DiagnosticsBufferUnderruns => "Pufferunterläufe seit Sitzungsstart",
        TextKey::DiagnosticsCaptureDrops => "Verworfene PCM-Frames seit Programmstart",
        TextKey::DiagnosticsReceivers => "Lautsprechertransport",
        TextKey::DiagnosticsPacketsAccepted => "Vom lokalen Betriebssystem angenommene Pakete",
        TextKey::DiagnosticsBytesAccepted => "Vom lokalen Betriebssystem angenommene Bytes",
        TextKey::DiagnosticsSendFailures => "Lokale Sendefehler",
        TextKey::DiagnosticsRetransmitRequests => "Angeforderte Wiederholungsplätze",
        TextKey::DiagnosticsCounterScope => "Puffer- und Transportzähler gelten seit Sitzungsstart; verworfene PCM-Frames seit Programmstart.",
        TextKey::DiagnosticsRetransmitDisclaimer => "Angeforderte Wiederholungsplätze sind Anfragen, kein gemessener Paketverlust.",
        TextKey::DiagnosticsLocalOnly => "Lokale Annahme bestätigt weder Wiedergabe noch Hörbarkeit oder akustische Latenz.",
        TextKey::Frames => "Frames",
        TextKey::DiagnosticsIssuePairing => "Kopplungsproblem. Verbinde den Lautsprecher neu.",
        TextKey::DiagnosticsIssueSetup => "Sitzungsaufbau fehlgeschlagen. Prüfe die Auswahl und versuche es erneut.",
        TextKey::DiagnosticsIssueCapture => "Aufnahmeproblem. Prüfe die ausgewählte Windows-Audioquelle.",
        TextKey::DiagnosticsIssueTiming => "Zeitabgleich gestört. Warte auf die Wiederherstellung oder starte die Sitzung neu.",
        TextKey::DiagnosticsIssueTransport => "Transportproblem. Prüfe die Lautsprecherverbindung und starte die Sitzung bei Bedarf neu.",
        TextKey::DiagnosticsIssueFeedback => "Rückmeldung des Lautsprechers fehlt. Prüfe die Netzwerkverbindung.",
        TextKey::DiagnosticsIssueTeardown => "Die vorige Sitzung wurde nicht sauber beendet. Starte die Verbindung erneut.",
        TextKey::DiagnosticsIssueOther => "Unbekanntes Sitzungsproblem. Beende die Sitzung und versuche es erneut.",
        TextKey::Appearance => "Darstellung",
        TextKey::AppearanceDescription => {
            "Legt fest, ob OpenAirCast hell, dunkel oder wie Windows dargestellt wird."
        }
        TextKey::ThemeSystem => "System",
        TextKey::ThemeLight => "Hell",
        TextKey::ThemeDark => "Dunkel",
        TextKey::HighContrastControlledByWindows => "Hoher Kontrast wird von Windows gesteuert.",
        TextKey::Language => "Sprache",
        TextKey::LanguageDescription => {
            "Legt die Sprache der Oberfläche fest. System folgt der Anzeigesprache von Windows."
        }
        TextKey::LanguageSystem => "System",
        TextKey::LanguageGerman => "Deutsch",
        TextKey::LanguageEnglish => "English",
        TextKey::Keyboard => "Tastatur",
        TextKey::GlobalShortcut => "Globales Tastenkürzel",
        TextKey::GlobalShortcutDescription => {
            "Startet und beendet das Streaming, auch wenn OpenAirCast nicht im Vordergrund ist."
        }
        TextKey::ShortcutEnabled => "Tastenkürzel aktiv",
        TextKey::ShortcutControl => "Ctrl",
        TextKey::ShortcutAlt => "Alt",
        TextKey::ShortcutShift => "Shift",
        TextKey::ShortcutWindows => "Win",
        TextKey::ShortcutKey => "Taste",
        TextKey::ShortcutKeyAccessibility => {
            "Taste des Tastenkürzels, ein Buchstabe, \
         eine Ziffer oder F1 bis F24"
        }
        TextKey::ShortcutKeyTooltip => {
            "Ein Buchstabe, eine Ziffer oder eine Funktionstaste, \
         zum Beispiel H oder F5."
        }
        TextKey::ApplyShortcut => "Tastenkürzel übernehmen",
        TextKey::ActiveShortcut => "Aktives Tastenkürzel",
        TextKey::ShortcutDisabled => "Tastenkürzel abgeschaltet",
        TextKey::ShortcutChangePending => "Änderung noch nicht übernommen",
        TextKey::ShortcutNeedsModifier => "Mindestens eine Zusatztaste ist erforderlich.",
        TextKey::ShortcutInvalidKey => {
            "Diese Taste ist unbekannt. Ein Buchstabe, eine Ziffer \
         oder F1 bis F24 wird erkannt."
        }
        TextKey::AdvancedInformation => "Erweiterte Informationen",
        TextKey::AdvancedInformationDescription => {
            "Zeigt die angewandte Auswahl auf jeder Empfängerzeile der Übersicht und unter \
             Lautsprecher, dazu Zählwerte für Aktiv und Verfügbar."
        }
        TextKey::On => "Ein",
        TextKey::Off => "Aus",
        TextKey::About => "Über OpenAirCast",
        TextKey::AboutDescription => "Version, Lizenz und Hinweise zu verwendeter Fremdsoftware.",
        TextKey::Version => "Version",
        TextKey::License => "Lizenz",
        TextKey::ThirdPartyNotices => "Hinweise zu Fremdsoftware",
        TextKey::OpenLicense => "Lizenz anzeigen",
        TextKey::OpenThirdPartyNotices => "Hinweise anzeigen",
        TextKey::CopyLicenseText => "Lizenztext kopieren",
        TextKey::CopyNoticesText => "Hinweistext kopieren",
        TextKey::CloseAboutViewer => "Ansicht schließen",
        TextKey::CopiedToClipboard => "In die Zwischenablage kopiert",
        TextKey::SupportProject => "OpenAirCast unterstützen",
        TextKey::SupportProjectDescription => {
            "Gefällt dir die App? Unterstütze die Weiterentwicklung mit einem Kaffee."
        }
        TextKey::SupportWithPaypal => "Mit PayPal unterstützen",
        TextKey::SupportBrowserHint => "Öffnet paypal.me/ttk95 im Standardbrowser. Freiwillig, ohne festen Betrag.",
        TextKey::Available => "Verfügbar",
        TextKey::Unavailable => "Nicht verfügbar",
        TextKey::Unknown => "Unbekannt",
        TextKey::Stale => "Veraltet",
        TextKey::Selected => "Ausgewählt",
        TextKey::Active => "Aktiv",
        TextKey::SessionMembershipIncluded => "Teil der laufenden Auswahl",
        TextKey::SessionMembershipExcluded => "Nicht Teil der laufenden Auswahl",
        TextKey::PreferenceSaveFailed => "Die Einstellung wurde nicht gespeichert.",
        TextKey::PreferenceSaveConsequence => {
            "Sie bleibt bis zum Beenden aktiv; auf dem Datenträger bleibt der zuletzt \
             gespeicherte Stand erhalten."
        }
        TextKey::PreferenceRetry => "Speichern erneut versuchen",
        TextKey::NoReceiverAvailableNotice => "Derzeit ist kein AirPlay-Empfänger erreichbar.",
        TextKey::DiscoveryFailedNotice => {
            "Die Suche nach Empfängern wurde unterbrochen. Bereits gefundene Lautsprecher \
             bleiben sichtbar."
        }
        TextKey::SessionFailedNotice => {
            "Die Sitzung wurde beendet. Die Auswahl der Lautsprecher bleibt erhalten."
        }
        TextKey::ControllerUnavailableNotice => {
            "Die Audiosteuerung antwortet nicht. Bereits laufende Wiedergabe bleibt \
             unverändert."
        }
        TextKey::HotkeyFailedNotice => {
            "Das globale Tastenkürzel wurde nicht registriert. Eine andere Anwendung belegt \
             diese Tastenkombination."
        }
        TextKey::InvalidInputNotice => {
            "Die Eingabe wurde nicht übernommen. Der zuletzt gültige Wert bleibt aktiv."
        }
        TextKey::CurrentSettingsRemainActive => "Die aktuellen Einstellungen bleiben aktiv.",
        TextKey::TryRefreshNext => "Nächster Schritt: Empfängerliste aktualisieren.",
        TextKey::TryRetryNext => "Nächster Schritt: Vorgang erneut versuchen.",
        TextKey::OpenSettingsNext => "Nächster Schritt: Einstellungen öffnen.",
        TextKey::CompactDestinationTooltip => "Diesen Bereich öffnen",
        TextKey::NavigationDestinationAccessibility => "Navigationsziel",
        TextKey::RouteSummaryAccessibility => "Zusammenfassung des Audiopfads",
        TextKey::ReceiverSelectionAccessibility => "Auswahl der Lautsprecher für das Streaming",
        TextKey::VolumeAccessibility => "Regler für die Gesamtlautstärke",
        TextKey::StatusFooterAccessibility => "Aktueller Zustand der Anwendung",
    }
}

const fn english(key: TextKey) -> &'static str {
    match key {
        TextKey::HardwareCheck => "Open hardware check",
        TextKey::HardwareTitle => "Hardware check",
        TextKey::HardwareBack => "Back to Audio",
        TextKey::HardwareHelp => "Show help",
        TextKey::HardwareHideHelp => "Hide help",
        TextKey::HardwarePreparation => "Prepare listening check",
        TextKey::HardwareListening => {
            "Test tone requested. Was it audible on every selected speaker?"
        }
        TextKey::HardwareSourceNotice => "The quiet test tone ends after about 2 seconds. Windows audio then resumes; the session stays open until Heard, Not heard, or Cancel.",
        TextKey::HardwareSilent => "Windows reports a silent audio source. Check that music or a video is playing on the selected device.",
        TextKey::HardwareUnavailable => "The app is not ready right now. Check the session status in Overview.",
        TextKey::HardwareBusy => "Streaming is already running. Stop it first; then you can start the listening check.",
        TextKey::HardwareSelection => "Select at least one speaker and apply the selection.",
        TextKey::HardwareSelectionPending => "Apply pending speaker selection changes and wait for confirmation.",
        TextKey::HardwareOffline => "At least one selected speaker is offline. Check the selection in Speakers.",
        TextKey::HardwareLevels => "Individual levels must be known and above zero. Check them in Speakers.",
        TextKey::HardwareMuted => "Turn mute off in Audio and wait for confirmation.",
        TextKey::HardwareLatency => "Select the available Normal latency profile in Audio.",
        TextKey::HardwareVolumePending => "The volume change is awaiting confirmation. Please wait briefly.",
        TextKey::HardwareVolume => "Set master volume to 10 percent and wait for confirmation.",
        TextKey::HardwareSource => "The Windows audio source is not ready. Check the playback device in Audio.",
        TextKey::HardwareConfirmation => "Confirm that you checked the selection and low volume first.",
        TextKey::HardwareReady => "Ready for the listening check. Playback starts only after your click.",
        TextKey::HardwareBackActive => "Back only navigates. To end the running listening check, choose Cancel.",
        TextKey::HardwarePrepare => "Selection and low volume checked",
        TextKey::HardwareRequirements => "Prepare: apply the complete selection and keep every speaker online; individual levels above 0, Normal profile, and unmuted. Confirmed master volume above 0 and at most 15 percent. The test tone runs for about 2 seconds at a low level.",
        TextKey::HardwareLower => "Set master volume to 10 percent",
        TextKey::HardwareStart => "Start listening check with test tone",
        TextKey::HardwareHeard => "Heard every selected speaker – stop",
        TextKey::HardwareNotHeard => "Did not hear every speaker – stop",
        TextKey::HardwarePending => "Stop pending – waiting for backend confirmation. Check is not complete.",
        TextKey::HardwarePass => "Hearing every selected speaker confirmed; playback stopped.",
        TextKey::HardwareFail => "Not every selected speaker was heard; playback stopped.",
        TextKey::HardwareInvalid => "Attempt cancelled or invalidated by changes; no confirmed result.",
        TextKey::HardwareLimitations => "Manual listening check without microphone measurement. The tone does not measure sound, acoustic offset, or audio packet loss and does not permanently unlock profiles.",
        TextKey::HardwareReport => "Copy diagnostics snapshot",
        TextKey::HardwareSnapshot => "Current snapshot – not permanent hardware validation",
        TextKey::HardwareEventNote => "Lost diagnostic events are not lost audio packets.",
        TextKey::Overview => "Overview",
        TextKey::Speakers => "Speakers",
        TextKey::Groups => "Groups",
        TextKey::Audio => "Audio",
        TextKey::Diagnostics => "Diagnostics",
        TextKey::Settings => "Settings",
        TextKey::Status => "Status",
        TextKey::Receivers => "Receivers",
        TextKey::AudioRoute => "Audio route",
        TextKey::SelectedChanges => "Selected changes",
        TextKey::Ready => "Ready",
        TextKey::Connecting => "Connecting",
        TextKey::Streaming => "Streaming",
        TextKey::Reconnecting => "Reconnecting",
        TextKey::Stopping => "Stopping",
        TextKey::NeedsAttention => "Needs attention",
        TextKey::Degraded => "Reduced",
        TextKey::ReadyExplanation => "Choose speakers, then start streaming.",
        TextKey::ConnectingExplanation => "The connection to the chosen speakers is being set up.",
        TextKey::StreamingExplanation => {
            "The sound from this PC is playing on the chosen speakers."
        }
        TextKey::DegradedExplanation => {
            "Playback continues, but not every chosen speaker is receiving sound."
        }
        TextKey::RestartingExplanation => "The connection dropped and is being rebuilt.",
        TextKey::StoppingExplanation => "The session is being closed.",
        TextKey::FailedExplanation => "Streaming ended. The chosen selection is kept.",
        TextKey::StartStreaming => "Start streaming",
        TextKey::StopStreaming => "Stop streaming",
        TextKey::Cancel => "Cancel",
        TextKey::StopTrying => "Stop trying",
        TextKey::Retry => "Retry",
        TextKey::Refresh => "Refresh",
        TextKey::ApplyChanges => "Apply changes",
        TextKey::SelectionApplying => "Applying selection…",
        TextKey::OperationPending => "Operation in progress…",
        TextKey::SelectionBusy => "Busy. Try again.",
        TextKey::SelectionInvalid => "Selection rejected. Please check it.",
        TextKey::SelectionSaveFailed => "Save failed. Try again.",
        TextKey::SelectionClosed => "Connection closed. Result unconfirmed.",
        TextKey::SelectionUnconfirmed => "Result unconfirmed. Check selection.",
        TextKey::Discard => "Discard",
        TextKey::Dismiss => "Dismiss",
        TextKey::WindowsAudio => "Windows audio",
        TextKey::RouteIdle => "Route idle",
        TextKey::RoutePending => "Route connecting",
        TextKey::RouteLive => "Route live",
        TextKey::RouteFault => "Route fault",
        TextKey::RouteToSelectedReceivers => "From this PC to the selected speakers",
        TextKey::AvailableReceivers => "Available receivers",
        TextKey::DiscoveringReceivers => "Looking for receivers",
        TextKey::RefreshReceivers => "Refresh the receiver list",
        TextKey::NoReceiversTitle => "No receivers found",
        TextKey::NoReceiversBody => {
            "The speakers must be switched on and reachable on the same network. Then search \
             again."
        }
        TextKey::ReceiverAvailable => "Speaker available",
        TextKey::ReceiverUnavailable => "Speaker unavailable",
        TextKey::ReceiverSelected => "Selected for streaming",
        TextKey::ReceiverActive => "Streaming now",
        TextKey::ReceiverSelectedAndActive => "Selected, streaming now",
        TextKey::DeviceClassHomePod => "HomePod",
        TextKey::DeviceClassHomePodMini => "HomePod mini",
        TextKey::DeviceClassAppleTv => "Apple TV",
        TextKey::DeviceClassSpeaker => "Speaker",
        TextKey::ReceiverUnnamed => "Unnamed speaker",
        TextKey::SelectReceiver => "Select speaker",
        TextKey::DeselectReceiver => "Deselect speaker",
        TextKey::MasterVolume => "Master volume",
        TextKey::MasterVolumeAccessibility => "Master volume for every selected speaker",
        TextKey::ReceiverLevel => "Speaker level",
        TextKey::ReceiverLevelAccessibility => "Speaker level relative to master volume",
        TextKey::ReceiverLevelHelp => "Relative to master volume",
        TextKey::ChangeSummary => "Summary of changes",
        TextKey::SpeakersEmptyTitle => "No speakers known yet",
        TextKey::SpeakersEmptyBody => {
            "Speakers appear here as soon as AirPlay devices answer on the network."
        }
        TextKey::GroupsEmptyTitle => "No saved groups yet",
        TextKey::GroupsEmptyBody => "Save frequently used speaker combinations with their levels.",
        TextKey::GroupsLoading => "Loading saved groups…",
        TextKey::CreateGroup => "Create group",
        TextKey::EditGroup => "Edit",
        TextKey::DeleteGroup => "Delete",
        TextKey::ApplyGroup => "Apply",
        TextKey::ApplyGroupAndStart => "Apply and start",
        TextKey::SaveGroup => "Save group",
        TextKey::GroupName => "Group name",
        TextKey::GroupMembers => "Members",
        TextKey::GroupMemberOffline => "Unavailable",
        TextKey::GroupNameRequired => "Enter a group name.",
        TextKey::GroupMemberRequired => "Select at least one speaker.",
        TextKey::GroupNameTooLong => "The group name must be at most 64 characters.",
        TextKey::GroupNameDuplicate => "That group name already exists.",
        TextKey::GroupOperationPending => "Group operation in progress…",
        TextKey::GroupSaveSucceeded => "Group saved.",
        TextKey::GroupDeleteSucceeded => "Group deleted.",
        TextKey::GroupApplySucceeded => "Group applied.",
        TextKey::GroupApplyAndStartSucceeded => "Group applied; playback was requested.",
        TextKey::GroupBusy => "Busy. Try again.",
        TextKey::GroupValidationFailed => "Group rejected. Check the entries.",
        TextKey::GroupPersistenceFailed => "Save failed. Try again.",
        TextKey::GroupClosed => "Connection closed. Result unconfirmed.",
        TextKey::GroupUnconfirmed => "Result unconfirmed. Check saved groups.",
        TextKey::KeepSelectionDraft => "Keep selection draft",
        TextKey::DiscardSelectionAndApply => "Discard draft and apply",
        TextKey::GroupApplySelectionConflict => "A speaker selection draft is still unapplied.",
        TextKey::ConfirmDelete => "Confirm delete",
        TextKey::DismissGroupResult => "Dismiss notice",
        TextKey::AudioEmptyTitle => "Windows audio has sent nothing yet",
        TextKey::AudioEmptyBody => {
            "Capture device, mute, and latency profile appear here as soon as the audio \
             control sends its state. Master volume lives on the Overview page."
        }
        TextKey::AudioMute => "Mute",
        TextKey::AudioMuteDescription => {
            "Silences playback without changing the configured levels."
        }
        TextKey::AudioLatency => "Latency profile",
        TextKey::AudioLatencyDescription => "Sets how much lead the sender buffers.",
        TextKey::LatencyLow => "Low",
        TextKey::LatencyNormal => "Normal",
        TextKey::LatencyStable => "Stable",
        TextKey::LatencyNotValidated => "Not yet approved; the manual listening check does not unlock this profile",
        TextKey::AudioCaptureSource => "Windows playback device",
        TextKey::AudioCaptureSourceDescription => {
            "Streams system audio from this Windows playback device using loopback."
        }
        TextKey::AudioCaptureSystemDefault => "Windows default",
        TextKey::AudioCaptureActiveDevice => "Currently captured playback device",
        TextKey::AudioCaptureStatus => "Capture state",
        TextKey::AudioCaptureSelectionMissing => "The chosen endpoint is absent",
        TextKey::AudioCaptureSourceFixed => {
            "Windows reported no playback devices in the last scan."
        }
        TextKey::AudioCaptureInventoryFailed => "Windows playback devices could not be enumerated.",
        TextKey::AudioCaptureRefreshFailed => {
            "The device list could not be refreshed. Showing the last successful scan."
        }
        TextKey::AudioCaptureInventoryUnknown => {
            "Windows playback devices have not been enumerated yet."
        }
        TextKey::AudioCaptureSelectionUnverified => {
            "Device selection has not been verified by the device list"
        }
        TextKey::CaptureCapturing => "Capturing",
        TextKey::CaptureSilentSystem => "System without sound",
        TextKey::CaptureRecovering => "Recovering",
        TextKey::CaptureUnavailable => "No capture source",
        TextKey::CaptureFailed => "Capture failed",
        TextKey::DiagnosticsEmptyTitle => "No diagnostics yet",
        TextKey::DiagnosticsEmptyBody => "Measurements appear once a session is running.",
        TextKey::AdvancedMetricsUnavailable => {
            "No advanced measurements are available for this area right now."
        }
        TextKey::DiagnosticsMeasurements => "Measurements",
        TextKey::DiagnosticsOverallHealth => "Overall health",
        TextKey::DiagnosticsAudioSource => "Selected or captured Windows audio source",
        TextKey::DiagnosticsHealthRunning => "Running",
        TextKey::DiagnosticsHealthError => "Error",
        TextKey::DiagnosticsEventsLost => "Diagnostic events lost",
        TextKey::DiagnosticsMeasuredReceivers => "Receivers with measurements",
        TextKey::LiveDiagnostics => "Live diagnostics",
        TextKey::DiagnosticsIdle => "Start streaming to see transport diagnostics.",
        TextKey::DiagnosticsNoSignal => "No Windows input signal is currently measured. Start audio on the selected source.",
        TextKey::DiagnosticsSignalUnknown => "No input measurement yet. Keep audio playing and check again.",
        TextKey::DiagnosticsHistoricalErrors => "Local transport errors were recorded in this session. If audio drops out, check the speaker connection.",
        TextKey::DiagnosticsHistoricalCapture => "Capture drops were recorded since app start. If audio drops out, check the Windows source.",
        TextKey::DiagnosticsHistoricalBuffer => "Buffer underruns were recorded in this session. If audio drops out, try the Normal latency profile.",
        TextKey::DiagnosticsCurrentFailure => "At least one speaker is currently offline or failed. Check its connection and the selection.",
        TextKey::DiagnosticsCurrentRecovering => "At least one speaker connection is recovering. Wait briefly and check again.",
        TextKey::DiagnosticsCurrentConnecting => "The speaker connection is being established. Wait briefly and check again.",
        TextKey::DiagnosticsLocalHealthy => "Windows input is measured and local send packets are recorded. Acoustic playback is not verified.",
        TextKey::DiagnosticsAwaitingSend => "Windows input signal is present. Waiting for local send measurements.",
        TextKey::DiagnosticsSourceUnavailable => "The Windows audio source is unavailable. Check the selected playback device in Audio.",
        TextKey::DiagnosticsSourceRecovering => "The Windows audio source is recovering. Wait briefly and check again.",
        TextKey::DiagnosticsInputSignal => "Input signal",
        TextKey::DiagnosticsShowDetails => "Show detailed counters",
        TextKey::DiagnosticsHideDetails => "Hide detailed counters",
        TextKey::DiagnosticsBuffer => "Audio buffer",
        TextKey::DiagnosticsBufferFill => "Buffer fill",
        TextKey::DiagnosticsBufferedAudio => "Buffered audio",
        TextKey::DiagnosticsBufferUnderruns => "Buffer underruns since session start",
        TextKey::DiagnosticsCaptureDrops => "Capture drops since app start",
        TextKey::DiagnosticsReceivers => "Speaker transport",
        TextKey::DiagnosticsPacketsAccepted => "Packets accepted by local OS",
        TextKey::DiagnosticsBytesAccepted => "Bytes accepted by local OS",
        TextKey::DiagnosticsSendFailures => "Local send errors",
        TextKey::DiagnosticsRetransmitRequests => "Retransmit slots requested",
        TextKey::DiagnosticsCounterScope => "Buffer and transport counters are since session start; capture drops are since app start.",
        TextKey::DiagnosticsRetransmitDisclaimer => "Retransmit slots are requests, not measured packet loss.",
        TextKey::DiagnosticsLocalOnly => "Local acceptance does not confirm playback, audibility, or acoustic latency.",
        TextKey::Frames => "frames",
        TextKey::DiagnosticsIssuePairing => "Pairing problem. Pair the speaker again.",
        TextKey::DiagnosticsIssueSetup => "Session setup failed. Check the selection and try again.",
        TextKey::DiagnosticsIssueCapture => "Capture problem. Check the selected Windows audio source.",
        TextKey::DiagnosticsIssueTiming => "Timing synchronization is disrupted. Wait for recovery or restart the session.",
        TextKey::DiagnosticsIssueTransport => "Transport problem. Check the speaker connection and restart the session if needed.",
        TextKey::DiagnosticsIssueFeedback => "Speaker feedback is missing. Check the network connection.",
        TextKey::DiagnosticsIssueTeardown => "The previous session did not close cleanly. Start the connection again.",
        TextKey::DiagnosticsIssueOther => "Unknown session problem. Stop the session and try again.",
        TextKey::Appearance => "Appearance",
        TextKey::AppearanceDescription => {
            "Sets whether OpenAirCast follows Windows or uses a light or dark theme."
        }
        TextKey::ThemeSystem => "System",
        TextKey::ThemeLight => "Light",
        TextKey::ThemeDark => "Dark",
        TextKey::HighContrastControlledByWindows => "High contrast is controlled by Windows.",
        TextKey::Language => "Language",
        TextKey::LanguageDescription => {
            "Sets the interface language. System follows the Windows display language."
        }
        TextKey::LanguageSystem => "System",
        TextKey::LanguageGerman => "Deutsch",
        TextKey::LanguageEnglish => "English",
        TextKey::Keyboard => "Keyboard",
        TextKey::GlobalShortcut => "Global shortcut",
        TextKey::GlobalShortcutDescription => {
            "Starts and stops streaming even when OpenAirCast is not in the foreground."
        }
        TextKey::ShortcutEnabled => "Shortcut enabled",
        TextKey::ShortcutControl => "Ctrl",
        TextKey::ShortcutAlt => "Alt",
        TextKey::ShortcutShift => "Shift",
        TextKey::ShortcutWindows => "Win",
        TextKey::ShortcutKey => "Key",
        TextKey::ShortcutKeyAccessibility => "Shortcut key: a letter, a digit, or F1 to F24",
        TextKey::ShortcutKeyTooltip => {
            "A letter, a digit, or a function key, \
         for example H or F5."
        }
        TextKey::ApplyShortcut => "Apply shortcut",
        TextKey::ActiveShortcut => "Active shortcut",
        TextKey::ShortcutDisabled => "Shortcut turned off",
        TextKey::ShortcutChangePending => "Change not applied yet",
        TextKey::ShortcutNeedsModifier => "At least one modifier key is required.",
        TextKey::ShortcutInvalidKey => {
            "That key is unknown. A letter, a digit, \
         or F1 to F24 is recognized."
        }
        TextKey::AdvancedInformation => "Advanced information",
        TextKey::AdvancedInformationDescription => {
            "Shows the applied selection on every receiver row on Overview and Speakers, plus \
             Active and Available counts."
        }
        TextKey::On => "On",
        TextKey::Off => "Off",
        TextKey::About => "About OpenAirCast",
        TextKey::AboutDescription => {
            "Version, license, and notices for the third-party software in use."
        }
        TextKey::Version => "Version",
        TextKey::License => "License",
        TextKey::ThirdPartyNotices => "Third-party notices",
        TextKey::OpenLicense => "Show license",
        TextKey::OpenThirdPartyNotices => "Show notices",
        TextKey::CopyLicenseText => "Copy license text",
        TextKey::CopyNoticesText => "Copy notice text",
        TextKey::CloseAboutViewer => "Close view",
        TextKey::CopiedToClipboard => "Copied to the clipboard",
        TextKey::SupportProject => "Support OpenAirCast",
        TextKey::SupportProjectDescription => {
            "Enjoying the app? Support its development with a coffee."
        }
        TextKey::SupportWithPaypal => "Support with PayPal",
        TextKey::SupportBrowserHint => {
            "Opens paypal.me/ttk95 in your default browser. Optional, with no fixed amount."
        }
        TextKey::Available => "Available",
        TextKey::Unavailable => "Unavailable",
        TextKey::Unknown => "Unknown",
        TextKey::Stale => "Stale",
        TextKey::Selected => "Selected",
        TextKey::Active => "Active",
        TextKey::SessionMembershipIncluded => "Part of the applied selection",
        TextKey::SessionMembershipExcluded => "Not part of the applied selection",
        TextKey::PreferenceSaveFailed => "The setting was not saved.",
        TextKey::PreferenceSaveConsequence => {
            "It stays active until the app closes; the last saved state remains on disk."
        }
        TextKey::PreferenceRetry => "Try saving again",
        TextKey::NoReceiverAvailableNotice => "No AirPlay receiver is reachable right now.",
        TextKey::DiscoveryFailedNotice => {
            "The search for receivers was interrupted. Speakers already found stay visible."
        }
        TextKey::SessionFailedNotice => "The session ended. The speaker selection is preserved.",
        TextKey::ControllerUnavailableNotice => {
            "The audio control is not responding. Playback that is already running is \
             unchanged."
        }
        TextKey::HotkeyFailedNotice => {
            "The global shortcut was not registered. Another application is using that key \
             combination."
        }
        TextKey::InvalidInputNotice => {
            "The entry was not applied. The last valid value stays active."
        }
        TextKey::CurrentSettingsRemainActive => "The current settings stay active.",
        TextKey::TryRefreshNext => "Next step: refresh the receiver list.",
        TextKey::TryRetryNext => "Next step: try the operation again.",
        TextKey::OpenSettingsNext => "Next step: open Settings.",
        TextKey::CompactDestinationTooltip => "Open this area",
        TextKey::NavigationDestinationAccessibility => "Navigation destination",
        TextKey::RouteSummaryAccessibility => "Audio route summary",
        TextKey::ReceiverSelectionAccessibility => "Speaker selection for streaming",
        TextKey::VolumeAccessibility => "Master volume slider",
        TextKey::StatusFooterAccessibility => "Current application state",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const LOCALES: [ResolvedLocale; 2] = [ResolvedLocale::German, ResolvedLocale::English];

    fn catalog(locale: ResolvedLocale) -> Catalog {
        Catalog::new(locale)
    }

    /// Product and protocol names that are allowed to appear in both
    /// languages, plus the language endonyms the settings page shows
    /// unchanged in either catalog.
    const CROSS_LANGUAGE_TERMS: &[&str] = &[
        "airplay",
        "homepod",
        "ptp",
        "wasapi",
        "openaircast",
        "windows",
        "ctrl",
        "alt",
        "shift",
        "win",
        "audio",
        "status",
        "system",
        "streaming",
        "version",
        "deutsch",
        "english",
    ];

    /// Words that would only ever appear in English copy.
    const ENGLISH_MARKERS: &[&str] = &[
        "the",
        "and",
        "with",
        "for",
        "your",
        "please",
        "settings",
        "speaker",
        "speakers",
        "receiver",
        "receivers",
        "volume",
        "available",
        "unavailable",
        "unknown",
        "stale",
        "selected",
        "active",
        "apply",
        "changes",
        "discard",
        "retry",
        "refresh",
        "cancel",
        "start",
        "stop",
        "shortcut",
        "keyboard",
        "appearance",
        "light",
        "dark",
        "overview",
        "groups",
        "diagnostics",
        "master",
        "clipboard",
        "notices",
        "license",
        "page",
        "search",
        "network",
        "device",
        "devices",
        "measurements",
        "session",
    ];

    /// Words that would only ever appear in German copy.
    const GERMAN_MARKERS: &[&str] = &[
        "der",
        "die",
        "das",
        "und",
        "nicht",
        "wird",
        "werden",
        "kein",
        "keine",
        "ist",
        "sind",
        "auf",
        "mit",
        "von",
        "zum",
        "bleibt",
        "bleiben",
        "einstellungen",
        "lautsprecher",
        "bereit",
        "verbindung",
        "erneut",
        "verwerfen",
        "abbrechen",
        "sprache",
        "darstellung",
        "tastatur",
        "unbekannt",
        "veraltet",
        "aktualisieren",
        "hell",
        "dunkel",
        "lizenz",
        "hinweise",
    ];

    fn words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| word.to_lowercase())
            .collect()
    }

    fn every_string(locale: ResolvedLocale) -> Vec<(TextKey, &'static str)> {
        TextKey::ALL
            .iter()
            .map(|&key| (key, catalog(locale).text(key)))
            .collect()
    }

    mod keys {
        use super::*;

        #[test]
        fn all_lists_each_key_once() {
            let unique: HashSet<TextKey> = TextKey::ALL.iter().copied().collect();
            assert_eq!(
                unique.len(),
                TextKey::ALL.len(),
                "TextKey::ALL must not repeat a key"
            );
        }

        #[test]
        fn all_starts_and_ends_in_declaration_order() {
            assert_eq!(TextKey::ALL.first(), Some(&TextKey::Overview));
            assert_eq!(
                TextKey::ALL.last(),
                Some(&TextKey::StatusFooterAccessibility)
            );
        }

        #[test]
        fn every_key_is_nonblank_in_both_locales() {
            for locale in LOCALES {
                for (key, text) in every_string(locale) {
                    assert!(!text.trim().is_empty(), "{key:?} is blank in {locale:?}");
                    assert_eq!(text, text.trim(), "{key:?} has stray padding in {locale:?}");
                }
            }
        }

        /// No sentence carries a run of whitespace a typesetter did not put
        /// there.
        ///
        /// Every long string in this file is wrapped with a backslash
        /// continuation, and the continuation is the one thing about it that
        /// can be lost silently: without the backslash the source still
        /// compiles, the string still says the right words, every language
        /// guard still passes -- and the user reads a fourteen-space hole in
        /// the middle of a sentence. Nothing else here looks at the space
        /// between words, so this is the only place that can see it.
        /// What is wrong with the way `text` is set, if anything.
        ///
        /// The judgement lives in a function rather than inside the loop so
        /// that the guard and the test which proves the guard can fail run
        /// the *same* code. A falsification test that restates the condition
        /// on a planted string proves only that the planted string is what it
        /// is: disable the guard and such a test still passes, which is
        /// exactly what happened here.
        fn typesetting_fault(text: &str) -> Option<&'static str> {
            if text.contains("  ") {
                return Some("a run of spaces");
            }
            if text.contains('\n') || text.contains('\t') {
                return Some("a line break or tab");
            }
            None
        }

        #[test]
        fn no_string_carries_a_run_of_whitespace() {
            for locale in LOCALES {
                for (key, text) in every_string(locale) {
                    assert_eq!(
                        typesetting_fault(text),
                        None,
                        "{key:?} is badly set in {locale:?}: {text:?}"
                    );
                }
            }
        }

        /// The guard has to be able to fail -- and to stay quiet on real copy,
        /// or it would be a guard that fails at everything.
        #[test]
        fn the_whitespace_guard_flags_a_lost_continuation_and_clears_real_copy() {
            let planted = "eine Fassung bietet noch              nicht an";
            assert_eq!(typesetting_fault(planted), Some("a run of spaces"));
            assert_eq!(
                typesetting_fault("erste Zeile\nzweite Zeile"),
                Some("a line break or tab")
            );
            for locale in LOCALES {
                let text = Catalog::new(locale).text(TextKey::ReadyExplanation);
                assert_eq!(typesetting_fault(text), None, "{locale:?}: {text:?}");
            }
        }

        #[test]
        fn coverage_is_identical_across_locales() {
            let german: HashSet<TextKey> = every_string(ResolvedLocale::German)
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            let english: HashSet<TextKey> = every_string(ResolvedLocale::English)
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            assert_eq!(german, english);
            assert_eq!(german.len(), TextKey::ALL.len());
        }

        #[test]
        fn translations_differ_where_the_languages_differ() {
            // Product names, "Audio", "Status", and the language endonyms are
            // legitimately identical; everything else must be translated.
            let shared_by_design = [
                TextKey::Audio,
                // "Normal" is the same word in both languages; naming the
                // middle latency profile anything else in German would
                // invent a distinction the backend does not make.
                TextKey::LatencyNormal,
                TextKey::Status,
                TextKey::Streaming,
                TextKey::Version,
                TextKey::ThemeSystem,
                TextKey::LanguageSystem,
                TextKey::LanguageGerman,
                TextKey::LanguageEnglish,
                TextKey::ShortcutControl,
                TextKey::ShortcutAlt,
                TextKey::ShortcutShift,
                TextKey::ShortcutWindows,
                // Apple's product names, which are not translated in either
                // language. The fallback class beside them is.
                TextKey::DeviceClassHomePod,
                TextKey::DeviceClassHomePodMini,
                TextKey::DeviceClassAppleTv,
            ];
            for &key in TextKey::ALL {
                if shared_by_design.contains(&key) {
                    continue;
                }
                assert_ne!(
                    catalog(ResolvedLocale::German).text(key),
                    catalog(ResolvedLocale::English).text(key),
                    "{key:?} is not translated"
                );
            }
        }
    }

    mod vocabulary {
        use super::*;

        #[test]
        fn german_matches_the_approved_core_vocabulary() {
            let de = catalog(ResolvedLocale::German);
            assert_eq!(de.text(TextKey::Overview), "Übersicht");
            assert_eq!(de.text(TextKey::Speakers), "Lautsprecher");
            assert_eq!(de.text(TextKey::Ready), "Bereit");
            assert_eq!(de.text(TextKey::Connecting), "Verbindung wird aufgebaut");
            assert_eq!(de.text(TextKey::Streaming), "Streaming");
            assert_eq!(
                de.text(TextKey::Reconnecting),
                "Verbindung wird wiederhergestellt"
            );
            assert_eq!(de.text(TextKey::Stopping), "Streaming wird beendet");
            assert_eq!(
                de.text(TextKey::NeedsAttention),
                "Aufmerksamkeit erforderlich"
            );
            assert_eq!(de.text(TextKey::StartStreaming), "Streaming starten");
            assert_eq!(de.text(TextKey::StopStreaming), "Streaming stoppen");
            assert_eq!(de.text(TextKey::Cancel), "Abbrechen");
            assert_eq!(de.text(TextKey::StopTrying), "Vorgang beenden");
            assert_eq!(de.text(TextKey::Retry), "Erneut versuchen");
            assert_eq!(de.text(TextKey::ApplyChanges), "Änderungen anwenden");
            assert_eq!(de.text(TextKey::Discard), "Verwerfen");
            assert_eq!(de.text(TextKey::MasterVolume), "Gesamtlautstärke");
            assert_eq!(
                de.text(TextKey::AdvancedInformation),
                "Erweiterte Informationen"
            );
            assert_eq!(de.text(TextKey::Unknown), "Unbekannt");
            assert_eq!(de.text(TextKey::Stale), "Veraltet");
        }

        #[test]
        fn english_matches_the_approved_core_vocabulary() {
            let en = catalog(ResolvedLocale::English);
            assert_eq!(en.text(TextKey::Overview), "Overview");
            assert_eq!(en.text(TextKey::Speakers), "Speakers");
            assert_eq!(en.text(TextKey::Ready), "Ready");
            assert_eq!(en.text(TextKey::Connecting), "Connecting");
            assert_eq!(en.text(TextKey::Streaming), "Streaming");
            assert_eq!(en.text(TextKey::Reconnecting), "Reconnecting");
            assert_eq!(en.text(TextKey::Stopping), "Stopping");
            assert_eq!(en.text(TextKey::NeedsAttention), "Needs attention");
            assert_eq!(en.text(TextKey::StartStreaming), "Start streaming");
            assert_eq!(en.text(TextKey::StopStreaming), "Stop streaming");
            assert_eq!(en.text(TextKey::Cancel), "Cancel");
            assert_eq!(en.text(TextKey::StopTrying), "Stop trying");
            assert_eq!(en.text(TextKey::Retry), "Retry");
            assert_eq!(en.text(TextKey::ApplyChanges), "Apply changes");
            assert_eq!(en.text(TextKey::Discard), "Discard");
            assert_eq!(en.text(TextKey::MasterVolume), "Master volume");
            assert_eq!(
                en.text(TextKey::AdvancedInformation),
                "Advanced information"
            );
            assert_eq!(en.text(TextKey::Unknown), "Unknown");
            assert_eq!(en.text(TextKey::Stale), "Stale");
        }

        #[test]
        fn commands_and_titles_use_sentence_case() {
            for locale in LOCALES {
                for (key, text) in every_string(locale) {
                    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
                    let shouting = letters.len() > 3
                        && letters.iter().all(|c| c.is_uppercase())
                        && !CROSS_LANGUAGE_TERMS.contains(&text.to_lowercase().as_str());
                    assert!(!shouting, "{key:?} is all-caps in {locale:?}: {text}");
                }
            }
        }

        #[test]
        fn the_language_guards_flag_planted_mixtures() {
            // Without this, the two guards below would still pass if their
            // word lists silently stopped matching anything.
            let english_in_german = words("Verbindung wird via network aufgebaut");
            assert!(english_in_german
                .iter()
                .any(|word| ENGLISH_MARKERS.contains(&word.as_str())));

            let german_in_english = words("The route is nicht available");
            assert!(german_in_english
                .iter()
                .any(|word| GERMAN_MARKERS.contains(&word.as_str())));

            // Product names stay exempt in both directions.
            for word in words("AirPlay HomePod PTP WASAPI OpenAirCast Ctrl Alt Shift Win") {
                assert!(CROSS_LANGUAGE_TERMS.contains(&word.as_str()), "{word}");
            }
        }

        #[test]
        fn german_copy_has_no_english_words() {
            for (key, text) in every_string(ResolvedLocale::German) {
                for word in words(text) {
                    if CROSS_LANGUAGE_TERMS.contains(&word.as_str()) {
                        continue;
                    }
                    assert!(
                        !ENGLISH_MARKERS.contains(&word.as_str()),
                        "{key:?} mixes English into German: {text}"
                    );
                }
            }
        }

        #[test]
        fn english_copy_has_no_german_words_or_umlauts() {
            for (key, text) in every_string(ResolvedLocale::English) {
                assert!(
                    !text.contains(['ä', 'ö', 'ü', 'Ä', 'Ö', 'Ü', 'ß']),
                    "{key:?} carries German diacritics into English: {text}"
                );
                for word in words(text) {
                    if CROSS_LANGUAGE_TERMS.contains(&word.as_str()) {
                        continue;
                    }
                    assert!(
                        !GERMAN_MARKERS.contains(&word.as_str()),
                        "{key:?} mixes German into English: {text}"
                    );
                }
            }
        }
    }

    mod formatting {
        use super::*;

        #[test]
        fn receiver_count_uses_locale_plurals() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);

            assert_eq!(
                de.receiver_count(ReceiverCountArgs { count: 0 }),
                "Keine Empfänger"
            );
            assert_eq!(
                de.receiver_count(ReceiverCountArgs { count: 1 }),
                "1 Empfänger"
            );
            assert_eq!(
                de.receiver_count(ReceiverCountArgs { count: 4 }),
                "4 Empfänger"
            );

            assert_eq!(
                en.receiver_count(ReceiverCountArgs { count: 0 }),
                "No receivers"
            );
            assert_eq!(
                en.receiver_count(ReceiverCountArgs { count: 1 }),
                "1 receiver"
            );
            assert_eq!(
                en.receiver_count(ReceiverCountArgs { count: 4 }),
                "4 receivers"
            );
        }

        #[test]
        fn change_summary_covers_zero_singular_and_plural() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);

            assert_eq!(
                de.change_summary(ChangeSummaryArgs {
                    added: 0,
                    removed: 0
                }),
                "Keine Änderungen"
            );
            assert_eq!(
                de.change_summary(ChangeSummaryArgs {
                    added: 1,
                    removed: 0
                }),
                "1 Lautsprecher hinzugefügt"
            );
            assert_eq!(
                de.change_summary(ChangeSummaryArgs {
                    added: 0,
                    removed: 3
                }),
                "3 Lautsprecher entfernt"
            );
            assert_eq!(
                de.change_summary(ChangeSummaryArgs {
                    added: 2,
                    removed: 1
                }),
                "2 Lautsprecher hinzugefügt, 1 Lautsprecher entfernt"
            );

            assert_eq!(
                en.change_summary(ChangeSummaryArgs {
                    added: 0,
                    removed: 0
                }),
                "No changes"
            );
            assert_eq!(
                en.change_summary(ChangeSummaryArgs {
                    added: 1,
                    removed: 0
                }),
                "1 speaker added"
            );
            assert_eq!(
                en.change_summary(ChangeSummaryArgs {
                    added: 0,
                    removed: 3
                }),
                "3 speakers removed"
            );
            assert_eq!(
                en.change_summary(ChangeSummaryArgs {
                    added: 2,
                    removed: 1
                }),
                "2 speakers added, 1 speaker removed"
            );
        }

        #[test]
        fn route_summary_separates_selected_from_streaming() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);

            assert_eq!(
                de.route_summary(RouteSummaryArgs {
                    selected: 0,
                    active: 0
                }),
                "Keine Lautsprecher ausgewählt"
            );
            assert_eq!(
                de.route_summary(RouteSummaryArgs {
                    selected: 3,
                    active: 2
                }),
                "3 ausgewählt, 2 im Streaming"
            );

            assert_eq!(
                en.route_summary(RouteSummaryArgs {
                    selected: 0,
                    active: 0
                }),
                "No speakers selected"
            );
            assert_eq!(
                en.route_summary(RouteSummaryArgs {
                    selected: 3,
                    active: 2
                }),
                "3 selected, 2 streaming"
            );
        }

        #[test]
        fn percentage_uses_locale_typography() {
            assert_eq!(
                catalog(ResolvedLocale::German).percentage(PercentageArgs { percent: 42 }),
                "42 %"
            );
            assert_eq!(
                catalog(ResolvedLocale::English).percentage(PercentageArgs { percent: 42 }),
                "42%"
            );
            assert_eq!(
                catalog(ResolvedLocale::German).percentage(PercentageArgs { percent: 0 }),
                "0 %"
            );
            assert_eq!(
                catalog(ResolvedLocale::English).percentage(PercentageArgs { percent: 100 }),
                "100%"
            );

            assert_eq!(
                catalog(ResolvedLocale::German)
                    .signal_percentage(SignalPercentageArgs { per_mille: 375 }),
                "37,5 %"
            );
            assert_eq!(
                catalog(ResolvedLocale::English)
                    .signal_percentage(SignalPercentageArgs { per_mille: 375 }),
                "37.5%"
            );
            assert_eq!(
                catalog(ResolvedLocale::English)
                    .signal_percentage(SignalPercentageArgs { per_mille: 0 }),
                "0%"
            );
        }

        #[test]
        fn duration_switches_unit_and_decimal_separator() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);

            assert_eq!(de.duration(DurationArgs { milliseconds: 0 }), "0 ms");
            assert_eq!(de.duration(DurationArgs { milliseconds: 820 }), "820 ms");
            assert_eq!(
                de.duration(DurationArgs {
                    milliseconds: 1_250
                }),
                "1,3 s"
            );
            assert_eq!(
                de.duration(DurationArgs {
                    milliseconds: 90_000
                }),
                "90,0 s"
            );

            assert_eq!(en.duration(DurationArgs { milliseconds: 0 }), "0 ms");
            assert_eq!(en.duration(DurationArgs { milliseconds: 820 }), "820 ms");
            assert_eq!(
                en.duration(DurationArgs {
                    milliseconds: 1_250
                }),
                "1.3 s"
            );
            assert_eq!(
                en.duration(DurationArgs {
                    milliseconds: 90_000
                }),
                "90.0 s"
            );
        }

        #[test]
        fn named_value_joins_a_label_and_its_value() {
            for locale in LOCALES {
                assert_eq!(
                    catalog(locale).named_value(NamedValueArgs {
                        name: catalog(locale).text(TextKey::Version),
                        value: "0.1.0",
                    }),
                    format!("{}: 0.1.0", catalog(locale).text(TextKey::Version))
                );
            }
        }

        /// The Route Ribbon's accessible summary names every selected
        /// receiver, so the list itself needs the locale's conjunction.
        #[test]
        fn name_list_uses_the_locale_conjunction_for_every_length() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);
            assert_eq!(de.name_list(&[]), "");
            assert_eq!(en.name_list(&[]), "");
            assert_eq!(de.name_list(&["Küche"]), "Küche");
            assert_eq!(en.name_list(&["Kitchen"]), "Kitchen");
            assert_eq!(de.name_list(&["Küche", "Büro"]), "Küche und Büro");
            assert_eq!(en.name_list(&["Kitchen", "Office"]), "Kitchen and Office");
            assert_eq!(
                de.name_list(&["Küche", "Büro", "Studio"]),
                "Küche, Büro und Studio"
            );
            assert_eq!(
                en.name_list(&["Kitchen", "Office", "Studio"]),
                "Kitchen, Office and Studio"
            );
        }

        /// The collapsed `+N` route node announces what it stands for.
        #[test]
        fn more_receivers_counts_the_collapsed_route_nodes() {
            let de = catalog(ResolvedLocale::German);
            let en = catalog(ResolvedLocale::English);
            assert_eq!(
                de.more_receivers(ReceiverCountArgs { count: 1 }),
                "1 weiterer Empfänger"
            );
            assert_eq!(
                en.more_receivers(ReceiverCountArgs { count: 1 }),
                "1 more receiver"
            );
            assert_eq!(
                de.more_receivers(ReceiverCountArgs { count: 4 }),
                "4 weitere Empfänger"
            );
            assert_eq!(
                en.more_receivers(ReceiverCountArgs { count: 4 }),
                "4 more receivers"
            );
        }

        /// The node itself carries the short marker; it must stay a number
        /// with a sign rather than a word, in both locales.
        #[test]
        fn overflow_marker_is_the_same_short_token_in_both_locales() {
            for locale in LOCALES {
                assert_eq!(
                    catalog(locale).overflow_marker(ReceiverCountArgs { count: 7 }),
                    "+7"
                );
            }
        }
    }

    mod copy_safety {
        use super::*;

        /// Fragments that only ever come from a path, an address, or a raw
        /// backend error. None of them may appear in catalog copy.
        const SENTINELS: &[&str] = &[
            "\\",
            "://",
            "c:",
            "0x",
            "@",
            "os error",
            "errno",
            "panic",
            "unwrap",
            "backtrace",
            "exception",
            "socket",
            "rtsp",
            "http",
            "tcp",
            "udp",
            "port ",
            "127.0",
            "::1",
            "appdata",
            "roaming",
            "userfacingerror",
            "commandfailed",
        ];

        fn looks_like_an_address(text: &str) -> bool {
            let mut groups = 0;
            let mut digits_in_group = 0;
            for c in text.chars() {
                if c.is_ascii_digit() {
                    digits_in_group += 1;
                } else if c == '.' && digits_in_group > 0 {
                    groups += 1;
                    digits_in_group = 0;
                } else {
                    groups = 0;
                    digits_in_group = 0;
                }
                if groups >= 3 && digits_in_group > 0 {
                    return true;
                }
            }
            false
        }

        fn contains_fragment(lowered: &str, sentinel: &str) -> bool {
            if sentinel == "port " {
                // A network port is a word, not the suffix of "support".
                lowered
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|word| word == "port")
            } else {
                lowered.contains(sentinel)
            }
        }

        #[test]
        fn the_sentinels_flag_planted_backend_text() {
            // Proves the scanner below is capable of failing.
            let planted = [
                r"C:\Users\someone\AppData\Roaming\openaircast\preferences.json",
                "rtsp://192.168.0.42:7000",
                "os error 10054",
                "thread panicked while sending to 0x1f4",
                "connection failed on port 7000",
            ];
            for sample in planted {
                let lowered = sample.to_lowercase();
                assert!(
                    SENTINELS.iter().any(|s| contains_fragment(&lowered, s))
                        || looks_like_an_address(sample),
                    "the scanner would have let {sample} through"
                );
            }
            assert!(looks_like_an_address("192.168.0.42"));
            assert!(!looks_like_an_address("Version 0.1.0"));
            assert!(!looks_like_an_address("Wert zwischen 1 und 254"));
            for legitimate in ["support openaircast", "transport controls"] {
                assert!(!SENTINELS.iter().any(|s| contains_fragment(legitimate, s)));
            }
        }

        #[test]
        fn catalog_copy_carries_no_path_address_or_error_fragments() {
            for locale in LOCALES {
                for (key, text) in every_string(locale) {
                    let lowered = text.to_lowercase();
                    for sentinel in SENTINELS {
                        assert!(
                            !contains_fragment(&lowered, sentinel),
                            "{key:?} in {locale:?} contains the unsafe fragment {sentinel:?}: {text}"
                        );
                    }
                    assert!(
                        !looks_like_an_address(text),
                        "{key:?} in {locale:?} looks like an address: {text}"
                    );
                }
            }
        }

        #[test]
        fn catalog_copy_has_no_substitution_points() {
            // No placeholder syntax means no seam for a raw backend string to
            // enter visible copy, an accessibility label, the clipboard, or a
            // support export.
            for locale in LOCALES {
                for (key, text) in every_string(locale) {
                    for forbidden in ['{', '}', '%', '$'] {
                        assert!(
                            !text.contains(forbidden),
                            "{key:?} in {locale:?} has a substitution point: {text}"
                        );
                    }
                }
            }
        }

        #[test]
        fn notices_are_complete_sentences_without_arguments() {
            // Every notice resolves through `text` alone: none of the typed
            // argument structs reaches a notice, so a backend reason cannot be
            // interpolated into one.
            let notices = [
                TextKey::PreferenceSaveFailed,
                TextKey::PreferenceSaveConsequence,
                TextKey::NoReceiverAvailableNotice,
                TextKey::DiscoveryFailedNotice,
                TextKey::SessionFailedNotice,
                TextKey::ControllerUnavailableNotice,
                TextKey::HotkeyFailedNotice,
                TextKey::InvalidInputNotice,
                TextKey::CurrentSettingsRemainActive,
                TextKey::TryRefreshNext,
                TextKey::TryRetryNext,
                TextKey::OpenSettingsNext,
            ];
            for locale in LOCALES {
                for key in notices {
                    let text = catalog(locale).text(key);
                    assert!(
                        text.ends_with('.'),
                        "{key:?} in {locale:?} is not a complete sentence: {text}"
                    );
                }
            }
        }

        #[test]
        fn dynamic_sentences_only_ever_carry_numbers_or_safe_names() {
            // A raw error handed to the only free-text template still cannot
            // widen the surface: the template joins, it never reformats, so
            // callers are the ones held to presentation-safe input and the
            // catalog itself stays clean.
            let raw = r"C:\Users\someone\AppData\Roaming\openaircast: os error 2";
            let joined = catalog(ResolvedLocale::English).named_value(NamedValueArgs {
                name: "Endpoint",
                value: raw,
            });
            assert_eq!(joined, format!("Endpoint: {raw}"));
            for locale in LOCALES {
                for (_, text) in every_string(locale) {
                    assert!(!text.contains(raw));
                }
            }
        }
    }

    mod locale_resolution {
        use super::*;
        use crate::app::LocalePreference;

        #[test]
        fn system_follows_windows_and_explicit_choices_do_not() {
            assert_eq!(
                LocalePreference::System.resolve(ResolvedLocale::German),
                ResolvedLocale::German
            );
            assert_eq!(
                LocalePreference::System.resolve(ResolvedLocale::English),
                ResolvedLocale::English
            );
            assert_eq!(
                LocalePreference::German.resolve(ResolvedLocale::English),
                ResolvedLocale::German
            );
            assert_eq!(
                LocalePreference::English.resolve(ResolvedLocale::German),
                ResolvedLocale::English
            );
        }

        #[test]
        fn defaults_are_system_and_english() {
            assert_eq!(LocalePreference::default(), LocalePreference::System);
            assert_eq!(ResolvedLocale::default(), ResolvedLocale::English);
            assert_eq!(
                catalog(ResolvedLocale::German).locale(),
                ResolvedLocale::German
            );
        }
    }
}
