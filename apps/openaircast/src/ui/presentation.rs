//! Pure presentation models and colour-role resolution for the Windows Native
//! controls.
//!
//! Nothing in this module touches egui, the clock, or the backend. It answers
//! three questions that the renderers are deliberately not allowed to answer
//! themselves:
//!
//! * **Which control owns the single filled primary action?**
//!   [`filled_action_owner`] is the one place that decides. Renderers ask for
//!   an [`Emphasis`]; they never compare lifecycle state to staged state.
//! * **Which colour plays which role?** [`action_colors`], [`notice_colors`],
//!   and [`focus_ring`] resolve roles from [`ThemeTokens`] only. No component
//!   ever sees a palette, so High Contrast keeps using Windows system colours
//!   without a single approximation.
//! * **Which localized key describes this state?** Every model is built from
//!   [`Catalog`] copy. `UserNotice::summary` -- the pre-redacted backend
//!   string -- is never read here, so it cannot reach visible text, an
//!   accessibility label, the clipboard, or a support export.
//!
//! Two contracts are deliberately absent rather than faked:
//!
//! * A notice carries no Dismiss action, because no typed dismiss event
//!   exists yet. An always-disabled Dismiss button would be exactly the fake
//!   control the specification forbids.
//! * The Speakers, Groups, Audio, and Diagnostics empty states carry no
//!   action, because no typed command reaches those pages yet.

use egui::Color32;

use airplay_core::DeviceId;

use crate::app::{
    AppEvent, AudioEndpointRequest, AudioEndpointSelection, AudioSourceReading, Availability,
    CaptureState, CorrectiveAction, DiagnosticsHealth, DiagnosticsReading, DiscoverySnapshot,
    LatencyChoice, LatencyReading, LatencyUnavailable, NoticeCode, Severity, StreamSnapshot,
    UiSnapshot, UserNotice,
};
use crate::ui::i18n::{
    Catalog, ChangeSummaryArgs, NamedValueArgs, PercentageArgs, ReceiverCountArgs,
    RouteSummaryArgs, TextKey,
};
use crate::ui::theme::ThemeTokens;

/// Minimum height of a command button, in logical points.
pub const CONTROL_MIN_HEIGHT: f32 = 40.0;
/// Corner radius of buttons and inputs.
pub const CONTROL_RADIUS: f32 = 8.0;
/// Corner radius of cards and notice surfaces.
pub const CARD_RADIUS: f32 = 12.0;
/// Internal padding of a card.
pub const CARD_PADDING: f32 = 16.0;
/// Internal padding of a dense metric surface.
pub const DENSE_PADDING: f32 = 12.0;
/// Gap between sections.
pub const SECTION_GAP: f32 = 16.0;
/// Distance between a control's edge and its focus ring.
pub const FOCUS_RING_OFFSET: f32 = 2.0;

/// The lifecycle command a context offers, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    Start,
    Cancel,
    Stop,
    StopTrying,
    Retry,
    /// A stopped session with nothing startable shows Start in a visibly
    /// disabled state, for the same reason [`LifecycleAction::DisabledStopping`]
    /// does: the page geometry must not depend on the state. It is never
    /// activatable, and it says why through [`LifecycleAction::disabled_reason`].
    DisabledStart,
    /// A failed session with nothing startable shows Retry in a visibly
    /// disabled state. Offering it enabled was the same defect as hiding
    /// Start, one turn worse: Retry holds the filled slot, so the page's
    /// dominant command dispatched `StartRequested` into a reducer that drops
    /// it for the very reason the command was invalid.
    DisabledRetry,
    /// The Stopping phase shows the command in a visibly disabled state so the
    /// page geometry does not jump; it is never activatable.
    DisabledStopping,
}

impl LifecycleAction {
    /// Every variant, for exhaustive tests.
    pub const ALL: &'static [LifecycleAction] = &[
        LifecycleAction::Start,
        LifecycleAction::Cancel,
        LifecycleAction::Stop,
        LifecycleAction::StopTrying,
        LifecycleAction::Retry,
        LifecycleAction::DisabledStart,
        LifecycleAction::DisabledRetry,
        LifecycleAction::DisabledStopping,
    ];

    /// Whether the command can actually be activated.
    pub const fn is_enabled(self) -> bool {
        !matches!(
            self,
            LifecycleAction::DisabledStart
                | LifecycleAction::DisabledRetry
                | LifecycleAction::DisabledStopping
        )
    }

    /// Whether the command stops or abandons something that is running.
    ///
    /// A safety command never yields the filled slot: hiding Stop behind a
    /// quiet treatment while audio is live is the one failure mode this rule
    /// exists to prevent.
    pub const fn is_safety(self) -> bool {
        matches!(
            self,
            LifecycleAction::Cancel | LifecycleAction::Stop | LifecycleAction::StopTrying
        )
    }

    /// The catalog key for the button label.
    pub const fn label(self) -> TextKey {
        match self {
            LifecycleAction::Start | LifecycleAction::DisabledStart => TextKey::StartStreaming,
            LifecycleAction::Cancel => TextKey::Cancel,
            LifecycleAction::Stop => TextKey::StopStreaming,
            LifecycleAction::StopTrying => TextKey::StopTrying,
            LifecycleAction::Retry | LifecycleAction::DisabledRetry => TextKey::Retry,
            LifecycleAction::DisabledStopping => TextKey::Stopping,
        }
    }

    /// Why an inert command cannot be activated, when its own verb does not
    /// already say so.
    ///
    /// A button that is drawn but dead has to name its precondition, or it is
    /// indistinguishable from a broken one. `Stopping` needs no sentence --
    /// its label *is* the reason -- while a greyed "Start streaming" needs
    /// the sentence the Command Home already states above it, which is why
    /// this reuses [`TextKey::ReadyExplanation`] rather than inventing a
    /// second phrasing of one fact. A greyed "Retry" is blocked by the same
    /// precondition and the same missing receiver, so it says the same
    /// sentence: the user has one thing to do, and a second wording of it
    /// would only make the window look like it knows two different problems.
    pub const fn disabled_reason(self) -> Option<TextKey> {
        match self {
            LifecycleAction::DisabledStart | LifecycleAction::DisabledRetry => {
                Some(TextKey::ReadyExplanation)
            }
            LifecycleAction::Start
            | LifecycleAction::Cancel
            | LifecycleAction::Stop
            | LifecycleAction::StopTrying
            | LifecycleAction::Retry
            | LifecycleAction::DisabledStopping => None,
        }
    }
}

/// Which control is allowed to be the one filled button in a context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilledActionOwner {
    Lifecycle,
    Apply,
    None,
}

/// Decides which control owns the single filled primary action.
///
/// * `apply_safe` means the authoritative run intent is stopped and no
///   start/stop transition is in flight.
/// * A safety command (Cancel, Stop, Stop trying) always wins while it is
///   offered and enabled.
/// * While the session is not safe, the lifecycle command keeps the filled
///   slot and Apply goes quiet.
/// * In a stopped, safe state with staged changes, Apply becomes the sole
///   filled action and Start or Retry goes quiet, so a stale selection cannot
///   be started by accident.
pub fn filled_action_owner(
    lifecycle: Option<LifecycleAction>,
    staged_dirty: bool,
    apply_safe: bool,
) -> FilledActionOwner {
    let enabled = lifecycle.filter(|action| action.is_enabled());
    match enabled {
        Some(action) if action.is_safety() => FilledActionOwner::Lifecycle,
        _ if !apply_safe => {
            enabled.map_or(FilledActionOwner::None, |_| FilledActionOwner::Lifecycle)
        }
        _ if staged_dirty => FilledActionOwner::Apply,
        _ => enabled.map_or(FilledActionOwner::None, |_| FilledActionOwner::Lifecycle),
    }
}

/// How the lifecycle command is drawn, or `None` when no command is offered.
pub fn lifecycle_emphasis(
    owner: FilledActionOwner,
    lifecycle: Option<LifecycleAction>,
) -> Option<Emphasis> {
    lifecycle.map(|_| {
        if owner == FilledActionOwner::Lifecycle {
            Emphasis::Filled
        } else {
            Emphasis::Quiet
        }
    })
}

/// How the Apply command in the change bar is drawn.
pub fn apply_emphasis(owner: FilledActionOwner) -> Emphasis {
    if owner == FilledActionOwner::Apply {
        Emphasis::Filled
    } else {
        Emphasis::Quiet
    }
}

/// Visual weight of a command button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Emphasis {
    Filled,
    Quiet,
}

/// One command button, already localized.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandActionModel {
    /// The visible verb.
    pub label: String,
    /// The accessible name, which may add context the visible label omits.
    pub accessible_name: String,
    pub emphasis: Emphasis,
    pub enabled: bool,
}

/// One persistent on/off switch, already localized.
///
/// The state word is a model field rather than something the switch derives,
/// for the same reason the percentage on the Audio Dock is: the word a user
/// reads and the state a screen reader is told have to come from the same
/// place, and that place is the snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchModel {
    /// The setting's own name, which is what the control announces.
    pub accessible_name: String,
    /// The word beside the track, so the state survives without colour.
    pub state_label: String,
    pub on: bool,
}

impl SwitchModel {
    pub fn new(
        accessible_name: impl Into<String>,
        state_label: impl Into<String>,
        on: bool,
    ) -> Self {
        Self {
            accessible_name: accessible_name.into(),
            state_label: state_label.into(),
            on,
        }
    }
}

impl CommandActionModel {
    /// A command whose accessible name is its visible label.
    pub fn new(label: impl Into<String>, emphasis: Emphasis, enabled: bool) -> Self {
        let label = label.into();
        Self {
            accessible_name: label.clone(),
            label,
            emphasis,
            enabled,
        }
    }

    /// A command whose accessible name carries more than the visible label.
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    /// The lifecycle command for `action`, drawn according to `owner`.
    pub fn lifecycle(catalog: Catalog, action: LifecycleAction, owner: FilledActionOwner) -> Self {
        let emphasis = lifecycle_emphasis(owner, Some(action)).unwrap_or(Emphasis::Quiet);
        let label = catalog.text(action.label());
        let model = Self::new(label, emphasis, action.is_enabled());
        match action.disabled_reason() {
            // The visible label stays the bare verb -- widening the button
            // with a sentence would move every other control in the header --
            // and the precondition travels in the announced name, where a
            // screen reader reaches it.
            Some(reason) => model.with_accessible_name(catalog.named_value(NamedValueArgs {
                name: label,
                value: catalog.text(reason),
            })),
            None => model,
        }
    }
}

/// The sticky change bar's localized model.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeBarModel {
    pub summary: String,
    pub apply_filled: bool,
    pub apply_enabled: bool,
    /// Separate from retry: a terminal error may be dismissed without applying.
    pub discard_enabled: bool,
}

impl ChangeBarModel {
    /// Builds the bar from the staged difference and the owning context.
    ///
    /// Apply stays enabled while there is a difference to commit; the running
    /// session only takes its *emphasis* away, never the ability to act.
    pub fn new(catalog: Catalog, changes: ChangeSummaryArgs, owner: FilledActionOwner) -> Self {
        Self {
            summary: catalog.change_summary(changes),
            apply_filled: owner == FilledActionOwner::Apply,
            apply_enabled: changes.added + changes.removed > 0,
            discard_enabled: changes.added + changes.removed > 0,
        }
    }

    /// The emphasis the Apply button is drawn with.
    pub fn apply_emphasis(&self) -> Emphasis {
        if self.apply_filled {
            Emphasis::Filled
        } else {
            Emphasis::Quiet
        }
    }
}

/// The corrective action a notice offers, already localized.
#[derive(Clone, Debug, PartialEq)]
pub struct NoticeActionModel {
    pub label: String,
    /// The typed action the reducer authorised. The caller maps it to an
    /// [`AppEvent`]; the notice component never invents one.
    pub corrective: CorrectiveAction,
}

/// A notice, built entirely from catalog copy.
#[derive(Clone, Debug, PartialEq)]
pub struct NoticeModel {
    pub severity: Severity,
    /// Text equivalent of the severity, so severity survives without colour.
    pub symbol: &'static str,
    pub message: String,
    pub consequence: Option<String>,
    pub action: Option<NoticeActionModel>,
}

impl NoticeModel {
    /// Maps the authoritative notice onto localized copy.
    ///
    /// `notice.summary` is deliberately not read.
    pub fn from_notice(notice: &UserNotice, catalog: Catalog) -> Self {
        let message = catalog.text(message_key(notice.code));
        let consequence = consequence_key(notice.code, notice.action).map(|key| catalog.text(key));
        let action = notice.action.map(|corrective| NoticeActionModel {
            label: catalog.text(action_key(notice.code, corrective)).to_owned(),
            corrective,
        });
        Self {
            severity: notice.severity,
            symbol: severity_symbol(notice.severity),
            message: message.to_owned(),
            consequence: consequence.map(str::to_owned),
            action,
        }
    }
}

/// The localized sentence for a notice code.
pub const fn message_key(code: NoticeCode) -> TextKey {
    match code {
        NoticeCode::NoReceiverAvailable => TextKey::NoReceiverAvailableNotice,
        NoticeCode::DiscoveryFailed => TextKey::DiscoveryFailedNotice,
        NoticeCode::SessionFailed => TextKey::SessionFailedNotice,
        NoticeCode::ControllerUnavailable => TextKey::ControllerUnavailableNotice,
        NoticeCode::PreferencesFailed => TextKey::PreferenceSaveFailed,
        NoticeCode::HotkeyFailed => TextKey::HotkeyFailedNotice,
        NoticeCode::InvalidInput => TextKey::InvalidInputNotice,
    }
}

/// What stays true after the failure, or what to do next.
pub const fn consequence_key(
    code: NoticeCode,
    action: Option<CorrectiveAction>,
) -> Option<TextKey> {
    match code {
        NoticeCode::PreferencesFailed => Some(TextKey::PreferenceSaveConsequence),
        _ => match action {
            Some(CorrectiveAction::Refresh) => Some(TextKey::TryRefreshNext),
            Some(CorrectiveAction::Retry | CorrectiveAction::RetryPreferencesPersistence) => {
                Some(TextKey::TryRetryNext)
            }
            Some(CorrectiveAction::OpenSettings) => Some(TextKey::OpenSettingsNext),
            None => Some(TextKey::CurrentSettingsRemainActive),
        },
    }
}

/// The button label for a corrective action.
///
/// A failed preference write gets the scoped label, because a bare "Retry"
/// would not say what is being retried.
pub const fn action_key(code: NoticeCode, action: CorrectiveAction) -> TextKey {
    match (code, action) {
        (NoticeCode::PreferencesFailed, _) | (_, CorrectiveAction::RetryPreferencesPersistence) => {
            TextKey::PreferenceRetry
        }
        (_, CorrectiveAction::Refresh) => TextKey::Refresh,
        (_, CorrectiveAction::Retry) => TextKey::Retry,
        (_, CorrectiveAction::OpenSettings) => TextKey::Settings,
    }
}

/// The textual severity marker.
pub const fn severity_symbol(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "i",
        Severity::Warning => "!",
        Severity::Error => "\u{00d7}",
    }
}

/// An empty state's single real next action.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyStateActionModel {
    pub label: String,
    pub event: AppEvent,
}

/// A purpose-specific empty state.
///
/// The action is an `Option`, not a list: "at most one real action" is a type
/// invariant here rather than a rule a renderer has to remember.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyStateModel {
    pub title: String,
    pub body: String,
    pub action: Option<EmptyStateActionModel>,
}

impl EmptyStateModel {
    fn plain(catalog: Catalog, title: TextKey, body: TextKey) -> Self {
        Self {
            title: catalog.text(title).to_owned(),
            body: catalog.text(body).to_owned(),
            action: None,
        }
    }

    /// Discovery found nothing. Refresh is a real typed command.
    pub fn no_receivers(catalog: Catalog) -> Self {
        Self {
            title: catalog.text(TextKey::NoReceiversTitle).to_owned(),
            body: catalog.text(TextKey::NoReceiversBody).to_owned(),
            action: Some(EmptyStateActionModel {
                label: catalog.text(TextKey::RefreshReceivers).to_owned(),
                event: AppEvent::RefreshRequested,
            }),
        }
    }

    pub fn speakers(catalog: Catalog) -> Self {
        Self::plain(
            catalog,
            TextKey::SpeakersEmptyTitle,
            TextKey::SpeakersEmptyBody,
        )
    }

    pub fn groups(catalog: Catalog) -> Self {
        Self::plain(catalog, TextKey::GroupsEmptyTitle, TextKey::GroupsEmptyBody)
    }

    pub fn audio(catalog: Catalog) -> Self {
        Self::plain(catalog, TextKey::AudioEmptyTitle, TextKey::AudioEmptyBody)
    }

    pub fn diagnostics(catalog: Catalog) -> Self {
        Self::plain(
            catalog,
            TextKey::DiagnosticsEmptyTitle,
            TextKey::DiagnosticsEmptyBody,
        )
    }
}

/// What the Diagnostics destination draws from the registry.
///
/// Three tiles in a fixed order -- overall health, diagnostic events lost,
/// receivers with measurements -- or, while nothing runs and nothing is
/// wrong, the page's empty state instead.
///
/// The freshness of the tiles is the whole design. A reading the shell has
/// actually received produces measured tiles, zeros included, because a
/// registry that counts zero lost events has measured that. No reading at all
/// produces [`MetricFreshness::Unknown`] tiles, whose value slot carries the
/// localized word rather than a number. Those two cases must not look alike:
/// this page is where a user goes to find out whether something is wrong, and
/// a fabricated `0` there is worse than no page.
///
/// There is a third case, and it is the one the receiver tile turns on: a
/// reading that arrived but cannot mean anything. See [`registry_tiles`].
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticsModel {
    /// The section title above the tiles.
    pub title: String,
    /// The three registry tiles, always all three.
    pub registry: Vec<MetricTileModel>,
    /// The page-level empty state, drawn instead of everything else while
    /// nothing runs and nothing is wrong.
    pub empty: Option<EmptyStateModel>,
}

impl DiagnosticsModel {
    /// Derives the page from one snapshot.
    pub fn from_snapshot(snapshot: &UiSnapshot, catalog: Catalog) -> Self {
        // Nothing running and nothing wrong: this page's own empty-state copy
        // promises that measurements appear once a session is running, and
        // that is exactly what the registry can do -- it answers
        // `Health::Unknown` and publishes no receiver rows until a session is
        // registered with it.
        //
        // The registry reading is deliberately *not* part of this condition.
        // The backend bridge is opened unconditionally at start-up and
        // publishes its first reading in the first pass of its loop, before
        // it ever parks, so `snapshot.diagnostics` is `Some` within a
        // fraction of a second of launch and never returns to `None`. A
        // condition that also asked for `diagnostics.is_none()` would
        // therefore be unreachable for the whole life of the process, and
        // this empty state -- which is the page's behaviour whenever nothing
        // is running -- would silently never be drawn again.
        let silent =
            matches!(snapshot.stream, StreamSnapshot::Stopped) && snapshot.notice.is_none();

        Self {
            title: catalog.text(TextKey::DiagnosticsMeasurements).to_owned(),
            registry: registry_tiles(snapshot.diagnostics, catalog),
            empty: silent.then(|| EmptyStateModel::diagnostics(catalog)),
        }
    }
}

/// The three tiles, measured from a reading or marked unknown without one.
///
/// "Unknown" covers two different absences here, and both have to look the
/// same to the reader because both mean *nobody measured this*:
///
/// * no reading at all -- the shell has never heard from the registry, either
///   because the bridge is not running or because the registry refused to
///   start;
/// * a reading whose `health` is [`DiagnosticsHealth::Unknown`] -- the
///   registry is running and is telling the window it has no session to judge.
///   Its receiver rows are built exclusively from that same absent session, so
///   the count it reports then is not a measurement of zero receivers, it is
///   the absence of a measurement. Drawn as a measurement it would say
///   "Receivers with measurements: 0" while two receivers are streaming, and
///   that sentence is worse than no tile.
///
/// The other two tiles stay measured in that case, and for different reasons.
/// `health` *is* the registry's answer -- "unknown" is what it measured, not
/// a gap in what it reported. `events_dropped_total` counts what the
/// diagnostics feed itself lost and is a real measurement from the moment the
/// registry starts, session or no session; it is the one number on this page
/// that moves today.
fn registry_tiles(reading: Option<DiagnosticsReading>, catalog: Catalog) -> Vec<MetricTileModel> {
    let health_label = catalog.text(TextKey::DiagnosticsOverallHealth);
    let lost_label = catalog.text(TextKey::DiagnosticsEventsLost);
    let receivers_label = catalog.text(TextKey::DiagnosticsMeasuredReceivers);

    let Some(reading) = reading else {
        return vec![
            MetricTileModel::unknown(catalog, health_label),
            MetricTileModel::unknown(catalog, lost_label),
            MetricTileModel::unknown(catalog, receivers_label),
        ];
    };

    let DiagnosticsReading {
        health,
        events_dropped_total,
        measured_receivers,
    } = reading;

    vec![
        MetricTileModel::measured(
            catalog,
            health_label,
            catalog.text(health_key(health)),
            None,
        ),
        MetricTileModel::measured(catalog, lost_label, events_dropped_total.to_string(), None),
        // Wildcard-free, so a fifth health state has to decide here whether
        // the receiver count means anything under it.
        match health {
            DiagnosticsHealth::Unknown => MetricTileModel::unknown(catalog, receivers_label),
            DiagnosticsHealth::Running
            | DiagnosticsHealth::Attention
            | DiagnosticsHealth::Error => MetricTileModel::measured(
                catalog,
                receivers_label,
                measured_receivers.to_string(),
                None,
            ),
        },
    ]
}

/// The word for one health state.
///
/// Wildcard-free, so a fifth state cannot borrow a fourth state's word.
/// `Unknown` and `Attention` reuse the catalog entries the rest of the window
/// already uses for the same two facts; one concept keeps one word.
const fn health_key(health: DiagnosticsHealth) -> TextKey {
    match health {
        DiagnosticsHealth::Unknown => TextKey::Unknown,
        DiagnosticsHealth::Running => TextKey::DiagnosticsHealthRunning,
        DiagnosticsHealth::Attention => TextKey::NeedsAttention,
        DiagnosticsHealth::Error => TextKey::DiagnosticsHealthError,
    }
}

// How trustworthy a metric currently is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricFreshness {
    /// Measured now.
    Live,
    /// Last known value, no longer being updated.
    Stale,
    /// Never measured. The value slot shows the localized word, never a zero.
    Unknown,
}

impl MetricFreshness {
    pub const ALL: &'static [MetricFreshness] = &[
        MetricFreshness::Live,
        MetricFreshness::Stale,
        MetricFreshness::Unknown,
    ];

    /// The textual marker, so freshness survives without colour.
    pub const fn symbol(self) -> Option<&'static str> {
        match self {
            MetricFreshness::Live => None,
            MetricFreshness::Stale => Some("\u{2013}"),
            MetricFreshness::Unknown => Some("?"),
        }
    }

    pub const fn label_key(self) -> Option<TextKey> {
        match self {
            MetricFreshness::Live => None,
            MetricFreshness::Stale => Some(TextKey::Stale),
            MetricFreshness::Unknown => Some(TextKey::Unknown),
        }
    }
}

// ---------------------------------------------------------------------------
// The Audio destination
// ---------------------------------------------------------------------------

/// The three groups of the Audio destination, each present only once the
/// backend has actually reported it.
///
/// `None` is not "off" and not "empty": it is *unmeasured*, and the page draws
/// nothing at all for a group in that state rather than a switch resting on a
/// value nobody produced. Master volume is deliberately absent -- it lives in
/// the Overview Audio Dock, and a second copy here would be a second authority
/// over one number.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPageModel {
    pub capture: Option<CaptureGroupModel>,
    pub mute: Option<MuteGroupModel>,
    pub latency: Option<LatencyGroupModel>,
}

/// The mute group: one switch over one measured bit.
#[derive(Clone, Debug, PartialEq)]
pub struct MuteGroupModel {
    pub title: String,
    pub description: String,
    pub switch: SwitchModel,
    /// What the switch dispatches: the opposite of the applied state, decided
    /// here so the switch renderer cannot invent a direction.
    pub toggle: AppEvent,
}

/// One latency profile as the page draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct LatencyRowModel {
    pub label: String,
    pub accessible_name: String,
    pub selected: bool,
    /// The whole sentence a refused profile is drawn as: its name and the
    /// reason it is offered and refused. `None` while the profile is
    /// selectable.
    ///
    /// Resolved from [`LatencyUnavailable`], which is a shell code -- the
    /// backend's own sentence for the same refusal is never read.
    pub unavailable_line: Option<String>,
    /// What the row dispatches, or `None` when the row is not selectable.
    ///
    /// A row without a request is not drawn as a control at all: it becomes a
    /// line of text. A greyed-out radio would still be a node in the
    /// accessibility tree that announces itself as a choice and answers to
    /// nothing, which is the dead control this project treats as its worst
    /// defect. Absent from the tab order is the honest shape.
    pub request: Option<AppEvent>,
}

/// The latency group.
#[derive(Clone, Debug, PartialEq)]
pub struct LatencyGroupModel {
    pub title: String,
    pub description: String,
    pub rows: Vec<LatencyRowModel>,
}

/// One capture source as the page draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct EndpointRowModel {
    pub label: String,
    pub accessible_name: String,
    pub selected: bool,
    /// What the row dispatches. Always present: every offered endpoint is
    /// selectable, and the Windows default row always is.
    pub request: AppEvent,
}

/// How the capture source is put to the reader: as a choice, or as a fact.
///
/// The distinction is not cosmetic. A radio group whose only row is the one
/// already in force is a control that announces a choice, takes the caret,
/// accepts a click, dispatches an event -- and cannot change anything. That
/// is the dead control this project treats as its worst defect, and it is
/// invisible to `no_reachable_control_is_inert`, whose bar is that *an* event
/// is sent. So the model decides here, once, on the only question that
/// matters: is there a row naming something other than what is already true?
///
/// Paragraph 7.4 provides for exactly this case -- "a read-only default
/// endpoint is labeled as such" -- and [`Self::Fixed`] is that label. It is
/// reached today on every real machine, because no backend stage publishes an
/// endpoint list yet; see the note on [`crate::app::AudioSourceReading`].
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureSourceModel {
    /// At least one row names a source other than the one in force.
    Offered(Vec<EndpointRowModel>),
    /// Nothing else is on offer. Both strings are drawn as text: the first
    /// names the source in force, the second says why it cannot be changed.
    Fixed { line: String, note: String },
}

/// The capture-source group.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureGroupModel {
    pub title: String,
    pub description: String,
    pub source: CaptureSourceModel,
    /// Which endpoint is actually being captured, or the word for "not
    /// measured" -- never a fabricated name.
    pub captured: MetricTileModel,
    /// What capture is doing.
    pub state: MetricTileModel,
    /// Present exactly when the stored preference names an endpoint that is
    /// not on offer, which is paragraph 7.4's unfulfillable preference.
    pub missing_selection: Option<String>,
    /// Localized scan failure; independent of whether capture is still running.
    pub refresh_error: Option<String>,
}

/// The localized name of one latency profile.
fn latency_label(catalog: Catalog, choice: LatencyChoice) -> &'static str {
    catalog.text(match choice {
        LatencyChoice::Low => TextKey::LatencyLow,
        LatencyChoice::Normal => TextKey::LatencyNormal,
        LatencyChoice::Stable => TextKey::LatencyStable,
    })
}

/// The localized reason one latency profile is refused.
///
/// The one place a [`LatencyUnavailable`] becomes words, and the reason that
/// type exists: the backend states its refusal as free-form text, and free
/// text has no place in a catalog whose every string is `&'static str`.
fn latency_unavailable_text(catalog: Catalog, reason: LatencyUnavailable) -> &'static str {
    catalog.text(match reason {
        LatencyUnavailable::NotValidated => TextKey::LatencyNotValidated,
    })
}

/// The localized word for what capture is doing.
fn capture_state_text(catalog: Catalog, state: CaptureState) -> &'static str {
    catalog.text(match state {
        CaptureState::Capturing => TextKey::CaptureCapturing,
        CaptureState::SilentSystem => TextKey::CaptureSilentSystem,
        CaptureState::Recovering => TextKey::CaptureRecovering,
        CaptureState::Unavailable => TextKey::CaptureUnavailable,
        CaptureState::Failed => TextKey::CaptureFailed,
    })
}

impl AudioPageModel {
    /// Builds the destination from one snapshot.
    pub fn from_snapshot(snapshot: &UiSnapshot, catalog: Catalog) -> Self {
        Self {
            capture: snapshot
                .audio_source
                .as_ref()
                .map(|reading| CaptureGroupModel::new(reading, catalog)),
            mute: snapshot
                .muted
                .map(|muted| MuteGroupModel::new(muted, catalog)),
            latency: snapshot
                .latency
                .as_ref()
                .map(|reading| LatencyGroupModel::new(reading, catalog)),
        }
    }

    /// Whether nothing has been measured, so the page shows its empty state.
    pub fn is_empty(&self) -> bool {
        self.capture.is_none() && self.mute.is_none() && self.latency.is_none()
    }
}

impl MuteGroupModel {
    fn new(muted: bool, catalog: Catalog) -> Self {
        let title = catalog.text(TextKey::AudioMute);
        Self {
            title: title.to_owned(),
            description: catalog.text(TextKey::AudioMuteDescription).to_owned(),
            switch: SwitchModel::new(
                title,
                catalog.text(if muted { TextKey::On } else { TextKey::Off }),
                muted,
            ),
            toggle: AppEvent::MuteChangeRequested(!muted),
        }
    }
}

impl LatencyGroupModel {
    fn new(reading: &LatencyReading, catalog: Catalog) -> Self {
        let group = catalog.text(TextKey::AudioLatency);
        Self {
            title: group.to_owned(),
            description: catalog.text(TextKey::AudioLatencyDescription).to_owned(),
            rows: reading
                .options
                .iter()
                .map(|option| {
                    let label = latency_label(catalog, option.choice);
                    let reason = option
                        .unavailable
                        .map(|reason| latency_unavailable_text(catalog, reason));
                    let named = catalog.named_value(NamedValueArgs {
                        name: group,
                        value: label,
                    });
                    LatencyRowModel {
                        label: label.to_owned(),
                        accessible_name: named,
                        selected: reading.selected == option.choice,
                        unavailable_line: reason.map(|reason| {
                            catalog.named_value(NamedValueArgs {
                                name: label,
                                value: reason,
                            })
                        }),
                        request: reason
                            .is_none()
                            .then_some(AppEvent::LatencyChoiceRequested(option.choice)),
                    }
                })
                .collect(),
        }
    }
}

impl CaptureGroupModel {
    fn new(reading: &AudioSourceReading, catalog: Catalog) -> Self {
        let group = catalog.text(TextKey::AudioCaptureSource);
        let row = |label: &str, selected: bool, request: AppEvent| EndpointRowModel {
            label: label.to_owned(),
            accessible_name: catalog.named_value(NamedValueArgs {
                name: group,
                value: label,
            }),
            selected,
            request,
        };

        let default_row = row(
            catalog.text(TextKey::AudioCaptureSystemDefault),
            reading.selection == AudioEndpointSelection::SystemDefault,
            AppEvent::AudioEndpointRequested(AudioEndpointRequest::SystemDefault),
        );
        // Held before the row moves into the list, so the read-only line
        // below never has to look a row up again -- and never has to invent a
        // name for the case where the lookup found nothing.
        let default_name = default_row.accessible_name.clone();

        let mut rows = vec![default_row];
        rows.extend(reading.endpoints.iter().map(|endpoint| {
            row(
                &endpoint.name,
                reading.selection == AudioEndpointSelection::Chosen(endpoint.key),
                AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(endpoint.key)),
            )
        }));

        // The one question: does any row name something other than what is
        // already in force? If not, there is no choice here, only a fact --
        // and paragraph 7.4 asks for the fact to be labeled read-only.
        let source = if rows.iter().any(|row| !row.selected) {
            CaptureSourceModel::Offered(rows)
        } else {
            // Every row is the row already in force. Since the list always
            // opens with the Windows-default row, that is the only row there
            // is, and its name is the one held above.
            CaptureSourceModel::Fixed {
                line: default_name,
                note: catalog
                    .text(if reading.endpoints_known {
                        TextKey::AudioCaptureSourceFixed
                    } else {
                        TextKey::AudioCaptureInventoryUnknown
                    })
                    .to_owned(),
            }
        };

        Self {
            title: group.to_owned(),
            description: catalog
                .text(TextKey::AudioCaptureSourceDescription)
                .to_owned(),
            source,
            captured: match &reading.captured_name {
                Some(name) => MetricTileModel::measured(
                    catalog,
                    catalog.text(TextKey::AudioCaptureActiveDevice),
                    name.clone(),
                    None,
                ),
                // The word, never a blank or an invented name: nothing is
                // being captured, and the tile says so in the same vocabulary
                // the Diagnostics tiles use.
                None => MetricTileModel::unknown(
                    catalog,
                    catalog.text(TextKey::AudioCaptureActiveDevice),
                ),
            },
            state: MetricTileModel::measured(
                catalog,
                catalog.text(TextKey::AudioCaptureStatus),
                capture_state_text(catalog, reading.state),
                None,
            ),
            missing_selection: match &reading.selection {
                AudioEndpointSelection::ChosenUnverified { name } => {
                    Some(catalog.named_value(NamedValueArgs {
                        name: catalog.text(TextKey::AudioCaptureSelectionUnverified),
                        value: name,
                    }))
                }
                AudioEndpointSelection::ChosenButMissing { name } => {
                    Some(catalog.named_value(NamedValueArgs {
                        name: catalog.text(TextKey::AudioCaptureSelectionMissing),
                        value: name,
                    }))
                }
                AudioEndpointSelection::SystemDefault | AudioEndpointSelection::Chosen(_) => None,
            },
            refresh_error: reading.refresh_failed.then(|| {
                catalog
                    .text(if reading.endpoints_known {
                        TextKey::AudioCaptureRefreshFailed
                    } else {
                        TextKey::AudioCaptureInventoryFailed
                    })
                    .to_owned()
            }),
        }
    }
}

/// One metric tile. Never interactive in this subproject.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricTileModel {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
    pub freshness: MetricFreshness,
    pub freshness_label: Option<String>,
    pub symbol: Option<&'static str>,
}

impl MetricTileModel {
    fn build(
        catalog: Catalog,
        label: String,
        value: String,
        unit: Option<String>,
        freshness: MetricFreshness,
    ) -> Self {
        Self {
            label,
            value,
            unit,
            freshness,
            freshness_label: freshness
                .label_key()
                .map(|key| catalog.text(key).to_owned()),
            symbol: freshness.symbol(),
        }
    }

    /// A currently measured value.
    pub fn measured(
        catalog: Catalog,
        label: impl Into<String>,
        value: impl Into<String>,
        unit: Option<String>,
    ) -> Self {
        Self::build(
            catalog,
            label.into(),
            value.into(),
            unit,
            MetricFreshness::Live,
        )
    }

    /// The last known value, explicitly marked as no longer updating.
    pub fn stale(
        catalog: Catalog,
        label: impl Into<String>,
        value: impl Into<String>,
        unit: Option<String>,
    ) -> Self {
        Self::build(
            catalog,
            label.into(),
            value.into(),
            unit,
            MetricFreshness::Stale,
        )
    }

    /// Never measured. The value is the localized word, not a fabricated zero.
    pub fn unknown(catalog: Catalog, label: impl Into<String>) -> Self {
        Self::build(
            catalog,
            label.into(),
            catalog.text(TextKey::Unknown).to_owned(),
            None,
            MetricFreshness::Unknown,
        )
    }

    /// The single sentence a screen reader announces for the tile.
    pub fn accessible_phrase(&self) -> String {
        let mut phrase = format!("{}: {}", self.label, self.value);
        if let Some(unit) = &self.unit {
            phrase.push(' ');
            phrase.push_str(unit);
        }
        if let Some(freshness) = self
            .freshness_label
            .as_ref()
            .filter(|word| **word != self.value || self.unit.is_some())
        {
            phrase.push_str(" (");
            phrase.push_str(freshness);
            phrase.push(')');
        }
        phrase
    }
}

/// The colours one command button paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionColors {
    /// `None` for quiet buttons, which sit directly on the page surface.
    pub fill: Option<Color32>,
    pub foreground: Color32,
    pub border: Color32,
}

/// Resolves a command button's colours from tokens only.
pub fn action_colors(tokens: &ThemeTokens, emphasis: Emphasis, enabled: bool) -> ActionColors {
    match (emphasis, enabled) {
        (Emphasis::Filled, true) => ActionColors {
            fill: Some(tokens.route),
            foreground: tokens.on_route,
            border: tokens.route,
        },
        (Emphasis::Filled, false) => ActionColors {
            fill: Some(tokens.surface_subtle),
            foreground: tokens.disabled,
            border: tokens.disabled,
        },
        (Emphasis::Quiet, true) => ActionColors {
            fill: None,
            foreground: tokens.ink,
            border: tokens.ink_muted,
        },
        (Emphasis::Quiet, false) => ActionColors {
            fill: None,
            foreground: tokens.disabled,
            border: tokens.disabled,
        },
    }
}

/// The colours one notice paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoticeColors {
    pub background: Color32,
    /// The severity symbol and rule.
    pub accent: Color32,
    pub foreground: Color32,
}

/// Resolves a notice's colours from tokens only.
///
/// In High Contrast every severity resolves to the same system foreground, by
/// design: the symbol and the sentence carry the severity there.
pub fn notice_colors(tokens: &ThemeTokens, severity: Severity) -> NoticeColors {
    let (background, accent) = match severity {
        Severity::Info => (tokens.route_soft, tokens.route),
        Severity::Warning => (tokens.warning_soft, tokens.warning),
        Severity::Error => (tokens.fault_soft, tokens.fault),
    };
    NoticeColors {
        background,
        accent,
        foreground: tokens.ink,
    }
}

/// The keyboard focus indicator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRing {
    pub width: f32,
    pub offset: f32,
    pub color: Color32,
}

/// Resolves the focus ring. High Contrast widens it to three points.
pub fn focus_ring(tokens: &ThemeTokens) -> FocusRing {
    FocusRing {
        width: tokens.focus_width,
        offset: FOCUS_RING_OFFSET,
        color: tokens.focus,
    }
}

/// Minimum height of one receiver card, in logical points.
pub const RECEIVER_CARD_MIN_HEIGHT: f32 = 56.0;
/// How many receiver nodes the Route Ribbon paints before it collapses.
pub const ROUTE_VISIBLE_NODES: usize = 3;
/// The largest node count that still fits without collapsing.
pub const ROUTE_COLLAPSE_THRESHOLD: usize = 4;
/// One deterministic arrow-key increment of the master volume.
pub const VOLUME_STEP: f32 = 0.05;
/// Duration of the single bounded sweep that acknowledges a live route.
pub const ROUTE_SWEEP_SECONDS: f32 = 0.12;

/// The lifecycle command the authoritative phase offers, if any.
///
/// `Stopped` always offers Start, and offers it *inert* when there is nothing
/// to start. Returning `None` instead was the worse of the two mistakes it
/// was meant to avoid: the Shell header draws the command or nothing at all,
/// so an unstartable Command Home had no primary command beside its title --
/// a page in breach of "exactly one filled primary command per page" and a
/// header whose geometry changed with the state. A drawn, disabled command
/// that names its precondition promises nothing; an absent one hides the fact
/// that starting is what this page is for.
///
/// `Failed` currently always means a stopped session -- `UiSnapshot::can_stop`
/// is false for it -- so its command is Retry; the running-intent `StopTrying`
/// needs an authoritative run intent that Subproject 1 does not project. It
/// is offered inert on exactly the same condition as Start, because retrying
/// *is* starting: `AppEvent::StartRequested` is what it dispatches, and the
/// reducer discards that whenever no applied receiver is available. An
/// enabled Retry there was a filled button that could not act.
pub fn lifecycle_action(snapshot: &UiSnapshot) -> Option<LifecycleAction> {
    match &snapshot.stream {
        StreamSnapshot::Stopped => Some(if snapshot.can_start {
            LifecycleAction::Start
        } else {
            LifecycleAction::DisabledStart
        }),
        StreamSnapshot::Starting { .. } => Some(LifecycleAction::Cancel),
        // All three are running sessions, and the command a running session
        // offers is the one that ends it. A restart the controller started on
        // its own is not a reason to take the Stop button away from the user.
        StreamSnapshot::Streaming { .. }
        | StreamSnapshot::Degraded { .. }
        | StreamSnapshot::Restarting { .. } => Some(LifecycleAction::Stop),
        StreamSnapshot::Stopping { .. } => Some(LifecycleAction::DisabledStopping),
        StreamSnapshot::Failed { .. } => Some(if snapshot.can_stop {
            LifecycleAction::StopTrying
        } else if snapshot.can_start {
            LifecycleAction::Retry
        } else {
            LifecycleAction::DisabledRetry
        }),
    }
}

/// How the session presents itself on the Command Home.
///
/// Every variant is reachable: `Degraded` and `Restarting` are the controller's
/// own session phases, carried through
/// [`crate::app::ControllerEvent::SessionDegraded`] and
/// [`crate::app::ControllerEvent::SessionRestarting`]. They were declared here
/// before anything could produce them, which was the honest half of the gap --
/// the Overview never invented a state it could not observe, but it also could
/// not report the two the resilience backend exists to survive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionVisualPhase {
    Ready,
    Connecting,
    Streaming,
    Degraded,
    Restarting,
    Stopping,
    Failed,
}

impl SessionVisualPhase {
    /// Every variant, for exhaustive tests.
    pub const ALL: &'static [SessionVisualPhase] = &[
        SessionVisualPhase::Ready,
        SessionVisualPhase::Connecting,
        SessionVisualPhase::Streaming,
        SessionVisualPhase::Degraded,
        SessionVisualPhase::Restarting,
        SessionVisualPhase::Stopping,
        SessionVisualPhase::Failed,
    ];

    /// The phase the authoritative stream state presents as.
    pub const fn from_stream(stream: &StreamSnapshot) -> Self {
        match stream {
            StreamSnapshot::Stopped => Self::Ready,
            StreamSnapshot::Starting { .. } => Self::Connecting,
            StreamSnapshot::Streaming { .. } => Self::Streaming,
            StreamSnapshot::Degraded { .. } => Self::Degraded,
            StreamSnapshot::Restarting { .. } => Self::Restarting,
            StreamSnapshot::Stopping { .. } => Self::Stopping,
            StreamSnapshot::Failed { .. } => Self::Failed,
        }
    }

    /// The one word that names the phase.
    pub const fn title_key(self) -> TextKey {
        match self {
            Self::Ready => TextKey::Ready,
            Self::Connecting => TextKey::Connecting,
            Self::Streaming => TextKey::Streaming,
            Self::Degraded => TextKey::Degraded,
            Self::Restarting => TextKey::Reconnecting,
            Self::Stopping => TextKey::Stopping,
            Self::Failed => TextKey::NeedsAttention,
        }
    }

    /// The sentence that explains what the phase means for the user.
    pub const fn explanation_key(self) -> TextKey {
        match self {
            Self::Ready => TextKey::ReadyExplanation,
            Self::Connecting => TextKey::ConnectingExplanation,
            Self::Streaming => TextKey::StreamingExplanation,
            Self::Degraded => TextKey::DegradedExplanation,
            Self::Restarting => TextKey::RestartingExplanation,
            Self::Stopping => TextKey::StoppingExplanation,
            Self::Failed => TextKey::FailedExplanation,
        }
    }

    /// The textual marker, so the phase survives without colour.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ready => "\u{25cb}",
            Self::Connecting => "\u{25d4}",
            Self::Streaming => "\u{25cf}",
            Self::Degraded => "\u{25d1}",
            Self::Restarting => "\u{21bb}",
            Self::Stopping => "\u{25a1}",
            Self::Failed => "\u{00d7}",
        }
    }

    /// How the route segment reads while the session is in this phase.
    pub const fn route_phase(self) -> RoutePhase {
        match self {
            Self::Ready => RoutePhase::Neutral,
            Self::Connecting | Self::Restarting | Self::Stopping => RoutePhase::Pending,
            Self::Streaming | Self::Degraded => RoutePhase::Live,
            Self::Failed => RoutePhase::Failed,
        }
    }
}

/// The state of the segment between the source and the receivers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutePhase {
    Neutral,
    Pending,
    Live,
    Failed,
}

impl RoutePhase {
    pub const ALL: &'static [RoutePhase] = &[
        RoutePhase::Neutral,
        RoutePhase::Pending,
        RoutePhase::Live,
        RoutePhase::Failed,
    ];

    pub const fn label_key(self) -> TextKey {
        match self {
            Self::Neutral => TextKey::RouteIdle,
            Self::Pending => TextKey::RoutePending,
            Self::Live => TextKey::RouteLive,
            Self::Failed => TextKey::RouteFault,
        }
    }

    /// The textual marker, so the segment survives without colour.
    ///
    /// The resting mark is the same empty ring the resting node and the
    /// resting receiver use, so "nothing is happening here" reads the same on
    /// every axis. It used to be an en dash, which turned the ribbon's own
    /// headline into a sentence with a stray leading dash.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Neutral => "\u{25cb}",
            Self::Pending => "\u{2026}",
            Self::Live => "\u{25b6}",
            Self::Failed => "\u{00d7}",
        }
    }
}

/// What one receiver node on the ribbon says about itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteNodeState {
    Selected,
    Active,
    Unavailable,
}

impl RouteNodeState {
    pub const ALL: &'static [RouteNodeState] = &[
        RouteNodeState::Selected,
        RouteNodeState::Active,
        RouteNodeState::Unavailable,
    ];

    pub const fn label_key(self) -> TextKey {
        match self {
            Self::Selected => TextKey::ReceiverSelected,
            Self::Active => TextKey::ReceiverActive,
            Self::Unavailable => TextKey::ReceiverUnavailable,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Selected => "\u{25cb}",
            Self::Active => "\u{25cf}",
            Self::Unavailable => "\u{2717}",
        }
    }
}

/// Where a failed route broke.
///
/// Only [`RouteFailureLocation::Segment`] is producible in Subproject 1: a
/// stopped `Failed` session says the connection did not come up, and nothing
/// in the snapshot attributes the failure to the capture source or to one
/// receiver. The other two locations are declared, never guessed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteFailureLocation {
    Source,
    Segment,
    Receiver,
}

/// One receiver node on the ribbon.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteNodeModel {
    pub name: String,
    pub state: RouteNodeState,
    pub state_label: String,
    pub symbol: &'static str,
    /// The sentence this node announces on its own.
    pub accessible_name: String,
}

/// The collapsed `+N` node.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteOverflowModel {
    pub count: usize,
    /// The short token painted in the node.
    pub marker: String,
    /// The sentence the node announces.
    pub accessible_name: String,
}

/// The Route Ribbon's presentation-safe input.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteRibbonModel {
    /// The localized name of the capture source.
    pub source_label: String,
    pub phase: RoutePhase,
    pub phase_label: String,
    /// The visible count line: how many receivers are selected and how many
    /// carry audio.
    pub counts_label: String,
    /// The nodes that are actually painted: every node, or the first
    /// [`ROUTE_VISIBLE_NODES`] when the route collapses.
    pub nodes: Vec<RouteNodeModel>,
    pub overflow: Option<RouteOverflowModel>,
    /// How many receivers the route really has, collapsed or not.
    pub total_nodes: usize,
    pub failure: Option<RouteFailureLocation>,
    /// The complete sentence the accessibility tree receives. It names every
    /// receiver on the route, including the ones the collapse hides.
    pub summary: String,
}

/// What a receiver card says about itself, beyond its name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverVisualState {
    Available,
    Unavailable,
    Selected,
    Streaming,
    SelectedAndStreaming,
}

impl ReceiverVisualState {
    pub const ALL: &'static [ReceiverVisualState] = &[
        ReceiverVisualState::Available,
        ReceiverVisualState::Unavailable,
        ReceiverVisualState::Selected,
        ReceiverVisualState::Streaming,
        ReceiverVisualState::SelectedAndStreaming,
    ];

    /// The state a staged selection, a live session, and an availability
    /// reading add up to.
    ///
    /// "Selected" is the staged set and nothing else: that is the set the
    /// checkbox writes and the set Apply commits, so binding the box to the
    /// applied set would make it spring back on the next frame.
    ///
    /// This is the *selection* axis only. `Available` and `Unavailable` are
    /// its resting values, so a selected or streaming receiver reports the
    /// selection here and its availability is carried separately -- by
    /// [`ReceiverCardModel::availability`], `availability_symbol`,
    /// `availability_label`, and by the card's accessible sentence. Folding
    /// availability into this enum is what made a staged receiver that went
    /// offline announce itself as merely "Selected".
    pub const fn of(selected: bool, streaming: bool, availability: Availability) -> Self {
        match (selected, streaming) {
            (true, true) => Self::SelectedAndStreaming,
            (false, true) => Self::Streaming,
            (true, false) => Self::Selected,
            (false, false) => match availability {
                Availability::Available => Self::Available,
                Availability::Unavailable => Self::Unavailable,
            },
        }
    }

    pub const fn label_key(self) -> TextKey {
        match self {
            Self::Available => TextKey::ReceiverAvailable,
            Self::Unavailable => TextKey::ReceiverUnavailable,
            Self::Selected => TextKey::ReceiverSelected,
            Self::Streaming => TextKey::ReceiverActive,
            Self::SelectedAndStreaming => TextKey::ReceiverSelectedAndActive,
        }
    }

    /// The textual marker, so selection and streaming survive without colour.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Available => "\u{25cb}",
            Self::Unavailable => "\u{2717}",
            Self::Selected => "\u{2713}",
            Self::Streaming => "\u{25b6}",
            Self::SelectedAndStreaming => "\u{25cf}",
        }
    }

    /// Whether the painted card owes this state a row of its own.
    ///
    /// Deliberately narrower than [`Self::adds_to_availability`], which
    /// governs the announced sentence. The card has two carriers the
    /// sentence does not: the check mark -- which *is* the control -- and the
    /// border. Between them `Selected` is already fully stated, and
    /// `Available` and `Unavailable` are the availability word printed one
    /// line above. Live audio is the one fact no control on the card can
    /// carry, so it is the one state that earns a row.
    ///
    /// Nothing is lost by the states that do not: every one of them is still
    /// in the sentence the card announces, which is what a screen reader
    /// receives.
    pub const fn shows_state_row(self) -> bool {
        matches!(self, Self::Streaming | Self::SelectedAndStreaming)
    }

    /// Whether this state says anything the availability word does not.
    ///
    /// `Available` and `Unavailable` *are* the availability word. Announcing
    /// them next to it would make the card state the same fact twice, so the
    /// sentence names the state only for the three selection states.
    pub const fn adds_to_availability(self) -> bool {
        matches!(
            self,
            Self::Selected | Self::Streaming | Self::SelectedAndStreaming
        )
    }
}

/// The availability marker, on its own axis.
///
/// Section 10.3 of the design specification lists "availability text and
/// icon" beside -- not inside -- "selected and streaming states", because
/// selecting an unreachable speaker does not make it reachable. This symbol
/// is therefore keyed off [`Availability`] alone and is painted whatever the
/// selection says.
pub const fn availability_symbol(availability: Availability) -> &'static str {
    match availability {
        Availability::Available => "\u{25cb}",
        Availability::Unavailable => "\u{2717}",
    }
}

/// The name to show for a receiver, or a localized stand-in when none is known.
///
/// An empty name is a real state rather than a defect: a selected receiver
/// restored from the last session is a row before discovery has ever seen it,
/// and stays one for as long as the speaker is switched off. The layers below
/// deliberately do not fill that gap -- the only identifier they hold is the
/// MAC-derived identity, and putting that on screen is exactly the leak this
/// module's own rules forbid. Naming it is a presentation decision because
/// only presentation can say it in the user's language.
pub fn receiver_display_name(catalog: Catalog, name: &str) -> &str {
    if name.trim().is_empty() {
        catalog.text(TextKey::ReceiverUnnamed)
    } else {
        name
    }
}

/// The device class a `model=` record names, as a catalog key.
///
/// The mDNS TXT record carries Apple's hardware identifier -- `AppleTV5,3`,
/// `AudioAccessory5,1` -- and nothing else. That string is a build artefact,
/// not copy: it is not localized, it is not a name any user has seen on a
/// box, and it changes with hardware revisions the Command Home has no
/// reason to distinguish.
///
/// The mapping is deliberately narrow. A prefix this function does not know,
/// a missing revision number, or an empty record all resolve to
/// [`TextKey::DeviceClassSpeaker`]: an AirPlay receiver whose class cannot be
/// read is still a speaker, and saying that is honest where inventing a
/// product name would not be.
pub fn device_class_key(model: &str) -> TextKey {
    let model = model.trim();
    let split = model
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(model.len());
    let (family, revision) = model.split_at(split);
    // The major revision: everything before the comma, when there is one.
    let major: Option<u32> = revision
        .split(',')
        .next()
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| digits.parse().ok());

    match (family.to_ascii_lowercase().as_str(), major) {
        // AudioAccessory5,x is the mini; every other known generation of the
        // family is a full-size HomePod.
        ("audioaccessory", Some(5)) => TextKey::DeviceClassHomePodMini,
        ("audioaccessory", Some(_)) => TextKey::DeviceClassHomePod,
        ("appletv", Some(_)) => TextKey::DeviceClassAppleTv,
        _ => TextKey::DeviceClassSpeaker,
    }
}

/// One receiver card, already localized.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverCardModel {
    /// The typed identity the toggle dispatches. It never reaches visible
    /// copy or an accessibility label; the renderer uses it as an egui id
    /// salt and as the payload of [`AppEvent::ToggleStagedReceiver`].
    pub id: DeviceId,
    pub name: String,
    /// The device class in user-facing form.
    pub device_class: String,
    pub availability: Availability,
    /// The availability word on its own, independent of selection.
    pub availability_label: String,
    /// The availability marker, independent of selection, so the fact
    /// survives without colour.
    pub availability_symbol: &'static str,
    pub selected: bool,
    pub streaming: bool,
    pub state: ReceiverVisualState,
    pub state_label: String,
    pub state_symbol: &'static str,
    /// The sentence the card announces.
    pub accessible_name: String,
    /// What activating the toggle would do.
    pub toggle_label: String,
    /// Advanced-mode detail below the standard row, or `None` in Standard
    /// mode.
    pub advanced_details: Option<String>,
}

/// The Audio Dock's master volume.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioDockModel {
    pub label: String,
    pub accessible_name: String,
    /// The authoritative value from the snapshot, clamped to the track.
    pub value: f32,
    pub percent: u8,
    /// The monospace percentage, in the locale's typography.
    pub percent_text: String,
}

/// One receiver's durable level, expressed as a multiplier of master volume.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverLevelModel {
    pub receiver: DeviceId,
    pub name: String,
    pub label: String,
    pub help: String,
    pub accessible_name: String,
    pub value: f32,
    pub percent: u8,
    pub percent_text: String,
}

/// Maps the backend's optional level registry to a control model.
///
/// `None` means the backend has not reported levels yet and must not grow
/// invented controls. A known registry omitting a discovered receiver means
/// the receiver's durable default: unity.
pub fn receiver_level_model(
    snapshot: &UiSnapshot,
    receiver: &DeviceId,
    catalog: Catalog,
) -> Option<ReceiverLevelModel> {
    let levels = snapshot.receiver_levels.as_ref()?;
    let raw_name = snapshot
        .receivers
        .iter()
        .find(|candidate| candidate.id == *receiver)
        .map(|candidate| candidate.name.clone())?;
    let name = receiver_display_name(catalog, &raw_name).to_owned();
    let value = levels.get(receiver).copied().unwrap_or(1.0).clamp(0.0, 1.0);
    let percent = (f64::from(value) * 100.0).round() as u8;
    Some(ReceiverLevelModel {
        receiver: receiver.clone(),
        name: name.clone(),
        label: catalog.text(TextKey::ReceiverLevel).to_owned(),
        help: catalog.text(TextKey::ReceiverLevelHelp).to_owned(),
        accessible_name: format!("{}: {}", catalog.text(TextKey::ReceiverLevel), name),
        value,
        percent,
        percent_text: catalog.percentage(PercentageArgs { percent }),
    })
}

impl AudioDockModel {
    /// Reads the authoritative volume. Never an optimistic local value: the
    /// percentage a user reads has to be the one the reducer published.
    pub fn from_snapshot(snapshot: &UiSnapshot, catalog: Catalog) -> Self {
        let value = snapshot.master_volume.clamp(0.0, 1.0);
        let percent = (f64::from(value) * 100.0).round() as u8;
        Self {
            label: catalog.text(TextKey::MasterVolume).to_owned(),
            accessible_name: catalog.text(TextKey::MasterVolumeAccessibility).to_owned(),
            value,
            percent,
            percent_text: catalog.percentage(PercentageArgs { percent }),
        }
    }
}

/// The whole Command Home, mapped once per frame from one snapshot.
///
/// Overview never owns the sticky Change Bar: the Shell places it, so the
/// single filled primary action stays arbitrated in one place.
#[derive(Clone, Debug, PartialEq)]
pub struct OverviewModel {
    pub phase: SessionVisualPhase,
    pub title: String,
    pub explanation: String,
    /// The lifecycle command the phase offers. The Shell header is the one
    /// place that draws it; the field exists so the body and the header
    /// cannot disagree about which command the phase has.
    pub primary: Option<LifecycleAction>,
    pub route: RouteRibbonModel,
    pub receivers: Vec<ReceiverCardModel>,
    pub audio: AudioDockModel,
    pub notice: Option<NoticeModel>,
    pub advanced_metrics: Vec<MetricTileModel>,
}

impl OverviewModel {
    /// Maps the snapshot onto localized copy. Pure: no clock, no disk, no
    /// frame, and `UserNotice::summary` is never read.
    pub fn from_snapshot(snapshot: &UiSnapshot, catalog: Catalog) -> Self {
        let phase = SessionVisualPhase::from_stream(&snapshot.stream);
        let primary = lifecycle_action(snapshot);
        Self {
            phase,
            title: catalog.text(phase.title_key()).to_owned(),
            explanation: catalog.text(phase.explanation_key()).to_owned(),
            primary,
            route: route_ribbon_model(snapshot, catalog, phase),
            receivers: receiver_card_models(snapshot, catalog),
            audio: AudioDockModel::from_snapshot(snapshot, catalog),
            notice: snapshot.notice.as_ref().map(|notice| {
                let mut model = NoticeModel::from_notice(notice, catalog);
                if notice_action_repeats_the_command(notice, primary) {
                    model.action = None;
                }
                model
            }),
            advanced_metrics: advanced_metric_models(snapshot, catalog),
        }
    }
}

/// Whether the notice's corrective button would carry the very word the page
/// header's lifecycle command already carries.
///
/// A failed session puts `Retry` in the header *and* `Retry` on the notice, so
/// the Command Home offered two buttons announcing the identical name and
/// dispatching the identical event. A screen reader says "Retry, button" twice
/// with nothing to tell them apart, and a sighted user is asked to choose
/// between two spellings of one command.
///
/// The comparison is on the resolved catalog key rather than on the event:
/// a notice whose corrective action means something the header does not offer
/// -- Refresh beside Start, or the scoped settings retry beside anything --
/// keeps its button, and only the literal duplicate is dropped. The
/// consequence sentence stays either way, so the notice still says what to do
/// next; it simply stops drawing a second copy of the control that does it.
fn notice_action_repeats_the_command(
    notice: &UserNotice,
    command: Option<LifecycleAction>,
) -> bool {
    let Some(corrective) = notice.action else {
        return false;
    };
    let Some(command) = command else {
        return false;
    };
    action_key(notice.code, corrective) == command.label()
}

/// Builds the ribbon from the *applied* selection.
///
/// The applied set is the route; the staged set is a proposal. Painting the
/// staged set would draw an audio path that does not exist and cannot be
/// started -- `UiSnapshot::can_start` reads the applied set too -- while the
/// Change Bar is the control that states the difference.
fn route_ribbon_model(
    snapshot: &UiSnapshot,
    catalog: Catalog,
    phase: SessionVisualPhase,
) -> RouteRibbonModel {
    let route_phase = phase.route_phase();
    let live = route_phase == RoutePhase::Live;
    let all_nodes: Vec<RouteNodeModel> = snapshot
        .receivers
        .iter()
        .filter(|receiver| snapshot.desired_receivers.contains(&receiver.id))
        .map(|receiver| {
            // Never "active" while the authoritative session is not live: a
            // stale membership set must not imply audio is flowing.
            let state = if live && snapshot.active_receivers.contains(&receiver.id) {
                RouteNodeState::Active
            } else if receiver.availability == Availability::Unavailable {
                RouteNodeState::Unavailable
            } else {
                RouteNodeState::Selected
            };
            let state_label = catalog.text(state.label_key()).to_owned();
            RouteNodeModel {
                accessible_name: catalog.named_value(NamedValueArgs {
                    name: &receiver.name,
                    value: &state_label,
                }),
                name: receiver.name.clone(),
                state,
                state_label,
                symbol: state.symbol(),
            }
        })
        .collect();

    let total_nodes = all_nodes.len();
    let active = all_nodes
        .iter()
        .filter(|node| node.state == RouteNodeState::Active)
        .count();
    let names: Vec<&str> = all_nodes.iter().map(|node| node.name.as_str()).collect();
    let summary = route_summary(catalog, route_phase, &names, total_nodes, active);

    let (nodes, overflow) = if total_nodes > ROUTE_COLLAPSE_THRESHOLD {
        let hidden = total_nodes - ROUTE_VISIBLE_NODES;
        let mut visible = all_nodes;
        visible.truncate(ROUTE_VISIBLE_NODES);
        (
            visible,
            Some(RouteOverflowModel {
                count: hidden,
                marker: catalog.overflow_marker(ReceiverCountArgs { count: hidden }),
                accessible_name: catalog.more_receivers(ReceiverCountArgs { count: hidden }),
            }),
        )
    } else {
        (all_nodes, None)
    };

    RouteRibbonModel {
        source_label: catalog.text(TextKey::WindowsAudio).to_owned(),
        phase: route_phase,
        phase_label: catalog.text(route_phase.label_key()).to_owned(),
        counts_label: catalog.route_summary(RouteSummaryArgs {
            selected: total_nodes,
            active,
        }),
        nodes,
        overflow,
        total_nodes,
        failure: (route_phase == RoutePhase::Failed).then_some(RouteFailureLocation::Segment),
        summary,
    }
}

/// The complete sentence the ribbon puts into the accessibility tree.
fn route_summary(
    catalog: Catalog,
    phase: RoutePhase,
    names: &[&str],
    selected: usize,
    active: usize,
) -> String {
    let mut body = format!(
        "{}. {}",
        catalog.text(phase.label_key()),
        catalog.text(TextKey::WindowsAudio)
    );
    if !names.is_empty() {
        body.push_str(". ");
        body.push_str(&catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::RouteToSelectedReceivers),
            value: &catalog.name_list(names),
        }));
    }
    body.push_str(". ");
    body.push_str(&catalog.route_summary(RouteSummaryArgs { selected, active }));
    catalog.named_value(NamedValueArgs {
        name: catalog.text(TextKey::RouteSummaryAccessibility),
        value: &body,
    })
}

/// The one sentence a card announces: who it is, what it is, whether it can
/// be reached, what state it is in, and -- in Advanced mode -- the applied
/// membership.
///
/// The card is a single target, so everything it shows has to reach the
/// accessibility tree through this one name; painted sub-lines are decoration
/// on top of it, never the only carrier of a fact. That is why the
/// availability word is unconditional here: it used to ride on the state
/// word, and a staged receiver that went offline reported "Selected" with
/// nothing to say it could no longer be reached.
fn receiver_card_sentence(
    catalog: Catalog,
    name: &str,
    device_class: &str,
    availability_label: &str,
    state: ReceiverVisualState,
    state_label: &str,
    advanced_details: Option<&str>,
) -> String {
    let mut sentence = catalog.named_value(NamedValueArgs {
        name,
        value: device_class,
    });
    sentence.push_str(". ");
    sentence.push_str(availability_label);
    if state.adds_to_availability() {
        sentence.push_str(". ");
        sentence.push_str(state_label);
    }
    if let Some(details) = advanced_details {
        sentence.push_str(". ");
        sentence.push_str(details);
    }
    sentence
}

pub(crate) fn receiver_card_models(
    snapshot: &UiSnapshot,
    catalog: Catalog,
) -> Vec<ReceiverCardModel> {
    snapshot
        .receivers
        .iter()
        .map(|receiver| {
            let device_class = catalog.text(device_class_key(&receiver.model)).to_owned();
            let name = receiver_display_name(catalog, &receiver.name).to_owned();
            let selected = snapshot.staged_receivers.contains(&receiver.id);
            let streaming = snapshot.active_receivers.contains(&receiver.id);
            let state = ReceiverVisualState::of(selected, streaming, receiver.availability);
            let state_label = catalog.text(state.label_key()).to_owned();
            let availability_label = catalog
                .text(match receiver.availability {
                    Availability::Available => TextKey::ReceiverAvailable,
                    Availability::Unavailable => TextKey::ReceiverUnavailable,
                })
                .to_owned();
            // The applied membership is the one fact the standard row does
            // not already carry: the row shows the staged selection, because
            // that is what the checkbox writes.
            let advanced_details = snapshot.advanced_information.then(|| {
                catalog
                    .text(if snapshot.desired_receivers.contains(&receiver.id) {
                        TextKey::SessionMembershipIncluded
                    } else {
                        TextKey::SessionMembershipExcluded
                    })
                    .to_owned()
            });
            ReceiverCardModel {
                id: receiver.id.clone(),
                device_class: device_class.clone(),
                availability: receiver.availability,
                availability_symbol: availability_symbol(receiver.availability),
                selected,
                streaming,
                state,
                accessible_name: receiver_card_sentence(
                    catalog,
                    &name,
                    &device_class,
                    &availability_label,
                    state,
                    &state_label,
                    advanced_details.as_deref(),
                ),
                availability_label,
                state_label,
                state_symbol: state.symbol(),
                toggle_label: catalog
                    .text(if selected {
                        TextKey::DeselectReceiver
                    } else {
                        TextKey::SelectReceiver
                    })
                    .to_owned(),
                advanced_details,
                name,
            }
        })
        .collect()
}

/// The two counts Subproject 1 really measures.
///
/// Session membership is authoritative in every snapshot, so the active count
/// is always live. The availability count is only as good as the last
/// discovery pass: it is unknown before the first one and stale after a
/// failed one, and in neither case does it become a fabricated zero.
fn advanced_metric_models(snapshot: &UiSnapshot, catalog: Catalog) -> Vec<MetricTileModel> {
    if !snapshot.advanced_information {
        return Vec::new();
    }
    let available = snapshot
        .receivers
        .iter()
        .filter(|receiver| receiver.availability == Availability::Available)
        .count();
    let availability_label = catalog.text(TextKey::Available);
    vec![
        MetricTileModel::measured(
            catalog,
            catalog.text(TextKey::Active),
            snapshot.active_receivers.len().to_string(),
            None,
        ),
        match snapshot.discovery {
            DiscoverySnapshot::Idle => MetricTileModel::unknown(catalog, availability_label),
            DiscoverySnapshot::Failed { .. } => {
                MetricTileModel::stale(catalog, availability_label, available.to_string(), None)
            }
            DiscoverySnapshot::Discovering | DiscoverySnapshot::Ready => {
                MetricTileModel::measured(catalog, availability_label, available.to_string(), None)
            }
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::{ResolvedLocale, ThemePreference};
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::theme::{contrast_ratio, resolve_theme};

    const SEVERITIES: &[Severity] = &[Severity::Info, Severity::Warning, Severity::Error];
    const CODES: &[NoticeCode] = &[
        NoticeCode::NoReceiverAvailable,
        NoticeCode::DiscoveryFailed,
        NoticeCode::SessionFailed,
        NoticeCode::ControllerUnavailable,
        NoticeCode::PreferencesFailed,
        NoticeCode::HotkeyFailed,
        NoticeCode::InvalidInput,
    ];
    const CORRECTIVES: &[Option<CorrectiveAction>] = &[
        None,
        Some(CorrectiveAction::Refresh),
        Some(CorrectiveAction::Retry),
        Some(CorrectiveAction::OpenSettings),
    ];
    const LOCALES: &[ResolvedLocale] = &[ResolvedLocale::German, ResolvedLocale::English];

    fn normal_appearance() -> SystemAppearance {
        SystemAppearance {
            client_animation_enabled: false,
            high_contrast: false,
            colors: SystemColors {
                background: [255, 255, 255],
                foreground: [0, 0, 0],
                highlight: [0, 120, 215],
                highlight_text: [255, 255, 255],
                disabled_text: [128, 128, 128],
                link: [0, 102, 204],
            },
        }
    }

    /// The real Windows "Contrast Black" and "Contrast White" themes. Using
    /// invented colours here would make the contrast assertions self-fulfilling.
    fn contrast_black() -> SystemAppearance {
        SystemAppearance {
            client_animation_enabled: false,
            high_contrast: true,
            colors: SystemColors {
                background: [0x00, 0x00, 0x00],
                foreground: [0xff, 0xff, 0xff],
                highlight: [0x1a, 0xeb, 0xff],
                highlight_text: [0x00, 0x00, 0x00],
                disabled_text: [0x3f, 0xf2, 0x3f],
                link: [0xff, 0xff, 0x00],
            },
        }
    }

    fn contrast_white() -> SystemAppearance {
        SystemAppearance {
            client_animation_enabled: false,
            high_contrast: true,
            colors: SystemColors {
                background: [0xff, 0xff, 0xff],
                foreground: [0x00, 0x00, 0x00],
                highlight: [0x37, 0x00, 0x6e],
                highlight_text: [0xff, 0xff, 0xff],
                disabled_text: [0x60, 0x00, 0x00],
                link: [0x00, 0x00, 0xff],
            },
        }
    }

    /// Every theme the product ships, named for assertion messages.
    fn all_token_sets() -> Vec<(&'static str, ThemeTokens)> {
        vec![
            (
                "Light",
                resolve_theme(ThemePreference::Light, None, normal_appearance()).tokens,
            ),
            (
                "Dark",
                resolve_theme(ThemePreference::Dark, None, normal_appearance()).tokens,
            ),
            (
                "Contrast Black",
                resolve_theme(ThemePreference::System, None, contrast_black()).tokens,
            ),
            (
                "Contrast White",
                resolve_theme(ThemePreference::System, None, contrast_white()).tokens,
            ),
        ]
    }

    mod one_filled_action {
        use super::*;

        /// Enumerates every context the rule can be asked about.
        fn every_context() -> Vec<(Option<LifecycleAction>, bool, bool)> {
            let mut contexts = Vec::new();
            let mut lifecycles: Vec<Option<LifecycleAction>> = vec![None];
            lifecycles.extend(LifecycleAction::ALL.iter().copied().map(Some));
            for lifecycle in lifecycles {
                for staged_dirty in [false, true] {
                    for apply_safe in [false, true] {
                        contexts.push((lifecycle, staged_dirty, apply_safe));
                    }
                }
            }
            contexts
        }

        /// Counts filled buttons the way a rendered page would: the lifecycle
        /// command plus the change bar's Apply, when the bar is on screen.
        fn filled_button_count(
            lifecycle: Option<LifecycleAction>,
            staged_dirty: bool,
            apply_safe: bool,
        ) -> usize {
            let owner = filled_action_owner(lifecycle, staged_dirty, apply_safe);
            let lifecycle_filled = lifecycle_emphasis(owner, lifecycle) == Some(Emphasis::Filled);
            let mut count = 0;
            if lifecycle_filled {
                count += 1;
            }
            if staged_dirty && apply_emphasis(owner) == Emphasis::Filled {
                count += 1;
            }
            count
        }

        #[test]
        fn no_context_ever_shows_two_filled_actions() {
            let mut filled_somewhere = 0;
            for (lifecycle, staged_dirty, apply_safe) in every_context() {
                let count = filled_button_count(lifecycle, staged_dirty, apply_safe);
                assert!(
                    count <= 1,
                    "{lifecycle:?} dirty={staged_dirty} safe={apply_safe} showed {count} filled actions"
                );
                filled_somewhere += count;
            }
            // A rule that fills nothing anywhere would satisfy the bound above
            // without being a rule at all.
            assert!(
                filled_somewhere > 0,
                "not a single context produced a filled action"
            );
        }

        #[test]
        fn every_context_with_an_enabled_command_has_exactly_one_filled_action() {
            for (lifecycle, staged_dirty, apply_safe) in every_context() {
                let has_enabled_lifecycle =
                    lifecycle.map(LifecycleAction::is_enabled).unwrap_or(false);
                // Apply only claims the slot in a safe state: while a session
                // runs or a transition is in flight it stays quiet, so a
                // context offering no lifecycle command fills nothing at all.
                let expected = usize::from(has_enabled_lifecycle || (staged_dirty && apply_safe));
                assert_eq!(
                    filled_button_count(lifecycle, staged_dirty, apply_safe),
                    expected,
                    "{lifecycle:?} dirty={staged_dirty} safe={apply_safe}"
                );
            }
        }

        #[test]
        fn a_running_session_keeps_stop_filled_and_makes_apply_quiet() {
            let owner = filled_action_owner(Some(LifecycleAction::Stop), true, false);
            assert_eq!(owner, FilledActionOwner::Lifecycle);
            assert_eq!(apply_emphasis(owner), Emphasis::Quiet);
        }

        #[test]
        fn a_transition_in_flight_keeps_cancel_and_stop_trying_filled() {
            for action in [LifecycleAction::Cancel, LifecycleAction::StopTrying] {
                assert_eq!(
                    filled_action_owner(Some(action), true, false),
                    FilledActionOwner::Lifecycle,
                    "{action:?}"
                );
            }
        }

        #[test]
        fn a_stopped_context_with_staged_changes_gives_apply_the_filled_slot() {
            for action in [LifecycleAction::Start, LifecycleAction::Retry] {
                let owner = filled_action_owner(Some(action), true, true);
                assert_eq!(owner, FilledActionOwner::Apply, "{action:?}");
                assert_eq!(
                    lifecycle_emphasis(owner, Some(action)),
                    Some(Emphasis::Quiet),
                    "{action:?}"
                );
            }
        }

        #[test]
        fn a_stopped_context_without_staged_changes_gives_start_the_filled_slot() {
            let owner = filled_action_owner(Some(LifecycleAction::Start), false, true);
            assert_eq!(owner, FilledActionOwner::Lifecycle);
            assert_eq!(
                lifecycle_emphasis(owner, Some(LifecycleAction::Start)),
                Some(Emphasis::Filled)
            );
        }

        #[test]
        fn stopping_owns_no_filled_command_and_stays_disabled() {
            for (dirty, safe) in [(false, false), (true, false), (false, true), (true, true)] {
                let owner =
                    filled_action_owner(Some(LifecycleAction::DisabledStopping), dirty, safe);
                if dirty && safe {
                    assert_eq!(owner, FilledActionOwner::Apply);
                } else {
                    assert_eq!(owner, FilledActionOwner::None, "dirty={dirty} safe={safe}");
                }
                assert_eq!(
                    lifecycle_emphasis(owner, Some(LifecycleAction::DisabledStopping)),
                    Some(Emphasis::Quiet)
                );
                assert!(!LifecycleAction::DisabledStopping.is_enabled());
            }
        }

        #[test]
        fn the_change_bar_model_mirrors_the_owner_and_stays_actionable() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let changes = ChangeSummaryArgs {
                added: 1,
                removed: 0,
            };
            let running = ChangeBarModel::new(
                catalog,
                changes,
                filled_action_owner(Some(LifecycleAction::Stop), true, false),
            );
            assert!(!running.apply_filled);
            assert!(running.apply_enabled);
            assert_eq!(running.apply_emphasis(), Emphasis::Quiet);

            let stopped = ChangeBarModel::new(
                catalog,
                changes,
                filled_action_owner(Some(LifecycleAction::Start), true, true),
            );
            assert!(stopped.apply_filled);
            assert_eq!(stopped.apply_emphasis(), Emphasis::Filled);

            let unchanged = ChangeBarModel::new(
                catalog,
                ChangeSummaryArgs {
                    added: 0,
                    removed: 0,
                },
                FilledActionOwner::None,
            );
            assert!(!unchanged.apply_enabled);
        }
    }

    mod contrast {
        use super::*;

        /// WCAG AA for body text.
        const TEXT_MINIMUM: f32 = 4.5;
        /// WCAG 1.4.11 for non-text state indicators.
        const NON_TEXT_MINIMUM: f32 = 3.0;

        fn assert_text(name: &str, theme: &str, foreground: Color32, background: Color32) {
            let ratio = contrast_ratio(foreground, background);
            assert!(
                ratio >= TEXT_MINIMUM,
                "{theme}: {name} is {ratio:.2}:1, below {TEXT_MINIMUM}:1"
            );
        }

        fn assert_non_text(name: &str, theme: &str, indicator: Color32, adjacent: Color32) {
            let ratio = contrast_ratio(indicator, adjacent);
            assert!(
                ratio >= NON_TEXT_MINIMUM,
                "{theme}: {name} is {ratio:.2}:1, below {NON_TEXT_MINIMUM}:1"
            );
        }

        #[test]
        fn every_enabled_command_foreground_meets_body_text_contrast() {
            for (theme, tokens) in all_token_sets() {
                let filled = action_colors(&tokens, Emphasis::Filled, true);
                assert_text(
                    "filled command label",
                    theme,
                    filled.foreground,
                    filled.fill.expect("a filled command has a fill"),
                );

                let quiet = action_colors(&tokens, Emphasis::Quiet, true);
                assert!(quiet.fill.is_none(), "{theme}: a quiet command has no fill");
                for (surface_name, surface) in
                    [("canvas", tokens.canvas), ("surface", tokens.surface)]
                {
                    assert_text(
                        &format!("quiet command label on {surface_name}"),
                        theme,
                        quiet.foreground,
                        surface,
                    );
                }
            }
        }

        #[test]
        fn every_command_border_meets_non_text_contrast() {
            for (theme, tokens) in all_token_sets() {
                for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                    let colors = action_colors(&tokens, emphasis, true);
                    for (surface_name, surface) in
                        [("canvas", tokens.canvas), ("surface", tokens.surface)]
                    {
                        assert_non_text(
                            &format!("{emphasis:?} command border on {surface_name}"),
                            theme,
                            colors.border,
                            surface,
                        );
                    }
                }
            }
        }

        #[test]
        fn the_disabled_treatment_stays_legible_and_uses_the_disabled_token() {
            for (theme, tokens) in all_token_sets() {
                for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                    let colors = action_colors(&tokens, emphasis, false);
                    assert_eq!(
                        colors.foreground, tokens.disabled,
                        "{theme}: {emphasis:?} disabled must use the semantic disabled token"
                    );
                    let background = colors.fill.unwrap_or(tokens.surface);
                    assert_non_text(
                        &format!("{emphasis:?} disabled label"),
                        theme,
                        colors.foreground,
                        background,
                    );
                }
            }
        }

        #[test]
        fn every_notice_severity_meets_text_and_indicator_contrast() {
            for (theme, tokens) in all_token_sets() {
                for &severity in SEVERITIES {
                    let colors = notice_colors(&tokens, severity);
                    assert_text(
                        &format!("{severity:?} notice text"),
                        theme,
                        colors.foreground,
                        colors.background,
                    );
                    assert_non_text(
                        &format!("{severity:?} notice accent"),
                        theme,
                        colors.accent,
                        colors.background,
                    );
                }
            }
        }

        #[test]
        fn the_focus_ring_meets_indicator_contrast_against_every_neighbour() {
            for (theme, tokens) in all_token_sets() {
                let ring = focus_ring(&tokens);
                for (surface_name, surface) in [
                    ("canvas", tokens.canvas),
                    ("surface", tokens.surface),
                    ("surface_subtle", tokens.surface_subtle),
                ] {
                    assert_non_text(
                        &format!("focus ring on {surface_name}"),
                        theme,
                        ring.color,
                        surface,
                    );
                }
            }
        }

        #[test]
        fn body_text_meets_contrast_on_every_page_surface() {
            for (theme, tokens) in all_token_sets() {
                for (surface_name, surface) in [
                    ("canvas", tokens.canvas),
                    ("surface", tokens.surface),
                    ("surface_subtle", tokens.surface_subtle),
                ] {
                    assert_text(
                        &format!("body text on {surface_name}"),
                        theme,
                        tokens.ink,
                        surface,
                    );
                }
            }
        }

        /// Secondary copy has to hold up on every surface it can land on, and
        /// `surface_subtle` is one of them whether a component chooses it or
        /// not: `apply_colors` wires `ink_muted` into egui's `weak_text_color`
        /// and `surface_subtle` into both backdrop fills, so egui pairs them by
        /// itself for hint text and striped rows.
        #[test]
        fn secondary_text_meets_contrast_on_every_page_surface() {
            for (theme, tokens) in all_token_sets() {
                for (surface_name, surface) in [
                    ("canvas", tokens.canvas),
                    ("surface", tokens.surface),
                    ("surface_subtle", tokens.surface_subtle),
                ] {
                    assert_text(
                        &format!("secondary text on {surface_name}"),
                        theme,
                        tokens.ink_muted,
                        surface,
                    );
                }
            }
        }

        /// A guard against a self-fulfilling suite: the helpers must be able
        /// to fail. Grey on grey is below both thresholds.
        #[test]
        fn the_contrast_helpers_reject_an_indistinguishable_pair() {
            let grey = Color32::from_rgb(0x80, 0x80, 0x80);
            let nearly = Color32::from_rgb(0x86, 0x86, 0x86);
            let ratio = contrast_ratio(grey, nearly);
            assert!(ratio < NON_TEXT_MINIMUM, "ratio was {ratio:.2}:1");
            assert!(ratio < TEXT_MINIMUM);
        }
    }

    mod without_color {
        use super::*;

        #[test]
        fn every_severity_has_its_own_text_marker() {
            let mut seen: Vec<&'static str> = Vec::new();
            for &severity in SEVERITIES {
                let symbol = severity_symbol(severity);
                assert!(!symbol.trim().is_empty(), "{severity:?} has no marker");
                assert!(
                    !seen.contains(&symbol),
                    "{severity:?} reuses the marker {symbol:?}"
                );
                seen.push(symbol);
            }
        }

        #[test]
        fn high_contrast_severities_share_a_colour_so_only_the_marker_separates_them() {
            let tokens = resolve_theme(ThemePreference::System, None, contrast_black()).tokens;
            let warning = notice_colors(&tokens, Severity::Warning);
            let error = notice_colors(&tokens, Severity::Error);
            assert_eq!(
                warning.accent, error.accent,
                "High Contrast must not approximate a warning hue"
            );
            assert_ne!(
                severity_symbol(Severity::Warning),
                severity_symbol(Severity::Error)
            );
        }

        #[test]
        fn unknown_and_stale_carry_both_a_marker_and_a_word() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                for &freshness in MetricFreshness::ALL {
                    let has_marker = freshness.symbol().is_some();
                    let has_word = freshness.label_key().is_some();
                    assert_eq!(has_marker, has_word, "{freshness:?} in {locale:?}");
                    if let Some(key) = freshness.label_key() {
                        assert!(!catalog.text(key).trim().is_empty());
                    }
                }
                assert_ne!(
                    MetricFreshness::Stale.symbol(),
                    MetricFreshness::Unknown.symbol()
                );
            }
        }

        #[test]
        fn the_focus_ring_is_two_points_with_a_two_point_offset_and_three_in_high_contrast() {
            for (theme, tokens) in all_token_sets() {
                let ring = focus_ring(&tokens);
                let expected = if theme.starts_with("Contrast") {
                    3.0
                } else {
                    2.0
                };
                assert_eq!(ring.width, expected, "{theme}");
                assert_eq!(ring.offset, 2.0, "{theme}");
            }
        }

        #[test]
        fn a_disabled_command_differs_from_an_enabled_one_beyond_colour() {
            for (theme, tokens) in all_token_sets() {
                let enabled = action_colors(&tokens, Emphasis::Filled, true);
                let disabled = action_colors(&tokens, Emphasis::Filled, false);
                assert_ne!(enabled.fill, disabled.fill, "{theme}");
            }
        }
    }

    mod notices {
        use super::*;

        fn notice(code: NoticeCode, action: Option<CorrectiveAction>) -> UserNotice {
            UserNotice {
                severity: Severity::Error,
                code,
                summary: "os error 10054 at C:\\Users\\x\\preferences.json".into(),
                action,
            }
        }

        #[test]
        fn no_notice_model_ever_carries_the_backend_summary() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                for &code in CODES {
                    for &action in CORRECTIVES {
                        let source = notice(code, action);
                        let model = NoticeModel::from_notice(&source, catalog);
                        let mut rendered = model.message.clone();
                        if let Some(consequence) = &model.consequence {
                            rendered.push_str(consequence);
                        }
                        if let Some(action) = &model.action {
                            rendered.push_str(&action.label);
                        }
                        // An empty model would trivially contain no sentinel.
                        assert!(
                            rendered.len() > 20,
                            "{code:?}/{action:?} in {locale:?} rendered almost nothing: {rendered:?}"
                        );
                        let lowered = rendered.to_lowercase();
                        for sentinel in ["os error", "c:\\", "preferences.json", "10054"] {
                            assert!(
                                !lowered.contains(sentinel),
                                "{code:?}/{action:?} in {locale:?} leaked {sentinel:?}: {rendered}"
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn every_notice_model_is_complete_in_both_locales() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                for &code in CODES {
                    for &action in CORRECTIVES {
                        let model = NoticeModel::from_notice(&notice(code, action), catalog);
                        assert!(
                            !model.message.trim().is_empty(),
                            "{code:?} has no message in {locale:?}"
                        );
                        assert!(
                            model
                                .consequence
                                .as_deref()
                                .is_some_and(|c| !c.trim().is_empty()),
                            "{code:?}/{action:?} has no consequence in {locale:?}"
                        );
                        assert_eq!(
                            model.action.is_some(),
                            action.is_some(),
                            "{code:?}/{action:?} in {locale:?}"
                        );
                        if let Some(rendered) = &model.action {
                            assert!(!rendered.label.trim().is_empty());
                            assert_eq!(Some(rendered.corrective), action);
                        }
                    }
                }
            }
        }

        #[test]
        fn a_failed_preference_write_uses_the_scoped_keys() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let model = NoticeModel::from_notice(
                &notice(NoticeCode::PreferencesFailed, Some(CorrectiveAction::Retry)),
                catalog,
            );
            assert_eq!(model.message, catalog.text(TextKey::PreferenceSaveFailed));
            assert_eq!(
                model.consequence.as_deref(),
                Some(catalog.text(TextKey::PreferenceSaveConsequence))
            );
            assert_eq!(
                model.action.as_ref().map(|a| a.label.as_str()),
                Some(catalog.text(TextKey::PreferenceRetry)),
                "a bare Retry would not say what is being retried"
            );
        }

        #[test]
        fn each_notice_code_maps_to_its_own_sentence() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let mut seen: Vec<String> = Vec::new();
            for &code in CODES {
                let message = catalog.text(message_key(code)).to_owned();
                assert!(!seen.contains(&message), "{code:?} reuses {message:?}");
                seen.push(message);
            }
        }

        #[test]
        fn the_severity_marker_follows_the_authoritative_severity() {
            let catalog = Catalog::new(ResolvedLocale::German);
            for &severity in SEVERITIES {
                let source = UserNotice {
                    severity,
                    code: NoticeCode::SessionFailed,
                    summary: String::new(),
                    action: None,
                };
                let model = NoticeModel::from_notice(&source, catalog);
                assert_eq!(model.severity, severity);
                // Comparing two empty markers would pass without a marker
                // existing at all.
                assert!(
                    !model.symbol.trim().is_empty(),
                    "{severity:?} has no marker"
                );
                assert_eq!(model.symbol, severity_symbol(severity));
            }
        }
    }

    mod empty_states_and_metrics {
        use super::*;

        #[test]
        fn an_empty_state_offers_at_most_one_action_and_only_a_real_one() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let with_action = EmptyStateModel::no_receivers(catalog);
                assert_eq!(
                    with_action.action.as_ref().map(|a| a.event.clone()),
                    Some(AppEvent::RefreshRequested)
                );

                for model in [
                    EmptyStateModel::speakers(catalog),
                    EmptyStateModel::groups(catalog),
                    EmptyStateModel::audio(catalog),
                    EmptyStateModel::diagnostics(catalog),
                ] {
                    assert!(
                        model.action.is_none(),
                        "a page without a typed command must not offer one in {locale:?}"
                    );
                    assert!(!model.title.trim().is_empty());
                    assert!(!model.body.trim().is_empty());
                }
            }
        }

        #[test]
        fn an_unknown_metric_shows_the_word_not_a_fabricated_zero() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = MetricTileModel::unknown(catalog, "Latency");
                assert_eq!(model.value, catalog.text(TextKey::Unknown));
                assert_ne!(model.value, "0");
                assert!(model.unit.is_none());
                assert_eq!(model.symbol, Some("?"));
            }
        }

        #[test]
        fn the_accessible_phrase_joins_label_value_unit_and_freshness() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let live = MetricTileModel::measured(catalog, "Latency", "12", Some("ms".into()));
            assert_eq!(live.accessible_phrase(), "Latency: 12 ms");

            let stale = MetricTileModel::stale(catalog, "Latency", "12", Some("ms".into()));
            assert_eq!(stale.accessible_phrase(), "Latency: 12 ms (Stale)");

            let unknown = MetricTileModel::unknown(catalog, "Latency");
            assert_eq!(unknown.accessible_phrase(), "Latency: Unknown");
        }

        #[test]
        fn the_spec_metrics_are_the_values_the_renderers_use() {
            assert_eq!(CONTROL_MIN_HEIGHT, 40.0);
            assert_eq!(CONTROL_RADIUS, 8.0);
            assert_eq!(CARD_RADIUS, 12.0);
            assert_eq!(CARD_PADDING, 16.0);
            assert_eq!(DENSE_PADDING, 12.0);
            assert_eq!(SECTION_GAP, 16.0);
            assert_eq!(FOCUS_RING_OFFSET, 2.0);
        }
    }
}

#[cfg(test)]
mod command_home_tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;

    use super::*;
    use crate::app::{
        AppState, Availability, DiscoveryState, GenerationId, ReceiverState, ResolvedLocale,
        StreamState, UiSnapshot,
    };
    use crate::ui::i18n::{PercentageArgs, ReceiverCountArgs, RouteSummaryArgs};

    const LOCALES: &[ResolvedLocale] = &[ResolvedLocale::German, ResolvedLocale::English];

    fn rid(last: u8) -> DeviceId {
        DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last])
    }

    fn receiver(last: u8, name: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            // The identifier form a real `model=` record carries. A friendly
            // product name here would have hidden the very defect the
            // device-class mapping exists for.
            model: "AudioAccessory5,1".into(),
            availability,
        }
    }

    /// Two known receivers, both applied and staged, discovery ready.
    fn base_state() -> AppState {
        AppState {
            receivers: vec![
                receiver(1, "Kitchen", Availability::Available),
                receiver(2, "Office", Availability::Unavailable),
            ],
            desired_receivers: HashSet::from_iter([rid(1), rid(2)]),
            staged_receivers: HashSet::from_iter([rid(1), rid(2)]),
            discovery: DiscoveryState::Ready,
            master_volume: 0.6,
            ..AppState::default()
        }
    }

    fn snapshot_of(state: &AppState) -> UiSnapshot {
        UiSnapshot::from_state(state)
    }

    fn overview(state: &AppState, locale: ResolvedLocale) -> OverviewModel {
        OverviewModel::from_snapshot(&snapshot_of(state), Catalog::new(locale))
    }

    /// Every state `StreamSnapshot` can represent, with the phase it presents
    /// as.
    ///
    /// No count in the name or the sentence: the list gained two entries the
    /// moment the resilience backend's phases reached the shell, and a
    /// comment saying "the five states" is how the gap stayed invisible.
    /// `representable_states_covers_every_visual_phase` is what keeps it
    /// honest now.
    fn representable_states() -> Vec<(StreamState, SessionVisualPhase)> {
        vec![
            (StreamState::Stopped, SessionVisualPhase::Ready),
            (
                StreamState::Starting {
                    generation: GenerationId(1),
                },
                SessionVisualPhase::Connecting,
            ),
            (
                StreamState::Streaming {
                    generation: GenerationId(2),
                },
                SessionVisualPhase::Streaming,
            ),
            (
                StreamState::Degraded {
                    generation: GenerationId(5),
                },
                SessionVisualPhase::Degraded,
            ),
            (
                StreamState::Restarting {
                    generation: GenerationId(6),
                },
                SessionVisualPhase::Restarting,
            ),
            (
                StreamState::Stopping {
                    generation: GenerationId(3),
                },
                SessionVisualPhase::Stopping,
            ),
            (
                StreamState::Failed {
                    generation: GenerationId(4),
                    summary: "Authentication failed at 192.168.1.44:7000".into(),
                },
                SessionVisualPhase::Failed,
            ),
        ]
    }

    mod phase {
        use super::*;

        /// The fixture has to cover the phases, not a remembered subset.
        ///
        /// Every test built on `representable_states` inherits its blind
        /// spots, and `the_primary_command_agrees_with_the_shell_header_for_
        /// every_state` promises "every state" in its own name. Anchoring on
        /// [`SessionVisualPhase::ALL`] turns the next phase somebody declares
        /// into a red test here instead of a silent hole there.
        #[test]
        fn representable_states_covers_every_visual_phase() {
            let covered = representable_states()
                .into_iter()
                .map(|(_, phase)| phase)
                .collect::<Vec<_>>();

            let missing = SessionVisualPhase::ALL
                .iter()
                .filter(|phase| !covered.contains(phase))
                .collect::<Vec<_>>();

            assert!(
                missing.is_empty(),
                "these phases are rendered by the window and asserted by nothing: \
                 {missing:?}"
            );
        }

        #[test]
        fn every_representable_stream_state_maps_to_its_visual_phase() {
            for (stream, expected) in representable_states() {
                let mut state = base_state();
                state.stream = stream.clone();
                assert_eq!(
                    overview(&state, ResolvedLocale::English).phase,
                    expected,
                    "{stream:?}"
                );
            }
        }

        #[test]
        fn every_phase_has_a_distinct_localized_title_and_explanation() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let titles: HashSet<&str> = SessionVisualPhase::ALL
                    .iter()
                    .map(|phase| catalog.text(phase.title_key()))
                    .collect();
                assert_eq!(titles.len(), SessionVisualPhase::ALL.len(), "{locale:?}");
                let explanations: HashSet<&str> = SessionVisualPhase::ALL
                    .iter()
                    .map(|phase| catalog.text(phase.explanation_key()))
                    .collect();
                assert_eq!(
                    explanations.len(),
                    SessionVisualPhase::ALL.len(),
                    "{locale:?}"
                );
            }
        }

        /// A state marker is a mark, never punctuation.
        ///
        /// `RoutePhase::Neutral` carried an en dash, so the ribbon's headline
        /// read "- Pfad inaktiv": a sentence with a stray leading dash, which
        /// looks like a template that failed to fill rather than like a
        /// state. Every other marker on the surface is a glyph that stands
        /// for something on its own.
        #[test]
        fn no_state_marker_is_a_dash_or_a_bare_piece_of_punctuation() {
            const PUNCTUATION: [&str; 6] = [
                "-", "\u{2010}", // hyphen
                "\u{2012}", // figure dash
                "\u{2013}", // en dash
                "\u{2014}", // em dash
                "_",
            ];
            let markers = SessionVisualPhase::ALL
                .iter()
                .map(|phase| ("session phase", phase.symbol()))
                .chain(
                    RoutePhase::ALL
                        .iter()
                        .map(|phase| ("route phase", phase.symbol())),
                )
                .chain(
                    RouteNodeState::ALL
                        .iter()
                        .map(|state| ("route node", state.symbol())),
                )
                .chain(
                    ReceiverVisualState::ALL
                        .iter()
                        .map(|state| ("receiver", state.symbol())),
                );
            for (axis, symbol) in markers {
                assert!(
                    !PUNCTUATION.contains(&symbol),
                    "a {axis} marker is the punctuation {symbol:?}"
                );
                assert!(!symbol.is_empty(), "a {axis} marker is empty");
            }
        }

        /// Colour is never the only carrier of a phase.
        #[test]
        fn every_phase_and_route_phase_has_a_distinct_text_symbol() {
            let phase_symbols: HashSet<&str> = SessionVisualPhase::ALL
                .iter()
                .map(|phase| phase.symbol())
                .collect();
            assert_eq!(phase_symbols.len(), SessionVisualPhase::ALL.len());
            let route_symbols: HashSet<&str> =
                RoutePhase::ALL.iter().map(|phase| phase.symbol()).collect();
            assert_eq!(route_symbols.len(), RoutePhase::ALL.len());
        }

        #[test]
        fn the_title_and_explanation_come_from_the_catalog_in_both_locales() {
            let mut state = base_state();
            state.stream = StreamState::Streaming {
                generation: GenerationId(9),
            };
            let de = overview(&state, ResolvedLocale::German);
            let en = overview(&state, ResolvedLocale::English);
            assert_eq!(
                de.explanation,
                Catalog::new(ResolvedLocale::German).text(TextKey::StreamingExplanation)
            );
            assert_eq!(
                en.explanation,
                Catalog::new(ResolvedLocale::English).text(TextKey::StreamingExplanation)
            );
            assert_ne!(de.explanation, en.explanation);
        }

        /// Named one by one rather than derived, so the mapping is pinned to
        /// the specification instead of to whatever the code happens to do.
        #[test]
        fn every_representable_state_names_its_primary_command_explicitly() {
            let expected = [
                (StreamState::Stopped, Some(LifecycleAction::Start)),
                (
                    StreamState::Starting {
                        generation: GenerationId(1),
                    },
                    Some(LifecycleAction::Cancel),
                ),
                (
                    StreamState::Streaming {
                        generation: GenerationId(2),
                    },
                    Some(LifecycleAction::Stop),
                ),
                (
                    StreamState::Stopping {
                        generation: GenerationId(3),
                    },
                    Some(LifecycleAction::DisabledStopping),
                ),
                (
                    StreamState::Failed {
                        generation: GenerationId(4),
                        summary: "Could not connect".into(),
                    },
                    Some(LifecycleAction::Retry),
                ),
            ];
            for (stream, command) in expected {
                let mut state = base_state();
                state.stream = stream.clone();
                assert_eq!(
                    overview(&state, ResolvedLocale::English).primary,
                    command,
                    "{stream:?}"
                );
            }
        }

        /// The Overview's statement of the phase command and the command the
        /// Shell header actually draws must be the same command; two answers
        /// would be two primary actions.
        #[test]
        fn the_primary_command_agrees_with_the_shell_header_for_every_state() {
            for (stream, _) in representable_states() {
                let mut state = base_state();
                state.stream = stream.clone();
                let snapshot = snapshot_of(&state);
                assert_eq!(
                    overview(&state, ResolvedLocale::English).primary,
                    crate::ui::components::app_shell::lifecycle_action(&snapshot),
                    "{stream:?}"
                );
            }
        }

        /// The only Failed state Subproject 1 can reach is a stopped one, and
        /// its command is Retry. `StopTrying` needs an authoritative run
        /// intent that no projection carries yet.
        #[test]
        fn the_currently_representable_failed_state_offers_retry() {
            let mut state = base_state();
            state.stream = StreamState::Failed {
                generation: GenerationId(7),
                summary: "Could not connect".into(),
            };
            let model = overview(&state, ResolvedLocale::English);
            assert_eq!(model.primary, Some(LifecycleAction::Retry));
            assert!(!snapshot_of(&state).can_stop);
        }

        /// A stopped session that cannot start still draws its command.
        ///
        /// Returning `None` here took the whole primary command off the page:
        /// the Shell header returns before it draws anything, so beside the
        /// page title stood nothing at all. That is a context without a
        /// primary command, and a header whose geometry jumps the moment a
        /// selection is applied. The command takes the route
        /// `DisabledStopping` already takes -- drawn, inert, reported
        /// disabled -- and, because a dead button has to say why it is dead,
        /// its accessible name carries the precondition the page states
        /// anyway.
        #[test]
        fn a_stopped_session_without_a_startable_selection_still_shows_an_inert_start() {
            let mut state = base_state();
            state.receivers[0].availability = Availability::Unavailable;
            assert!(
                !snapshot_of(&state).can_start,
                "the fixture has to be unstartable for this to mean anything"
            );

            let action = overview(&state, ResolvedLocale::English)
                .primary
                .expect("a stopped session must still name its command");
            assert_eq!(action.label(), TextKey::StartStreaming);
            assert!(!action.is_enabled(), "there is nothing to start");

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = CommandActionModel::lifecycle(
                    catalog,
                    action,
                    filled_action_owner(Some(action), false, true),
                );
                assert!(!model.enabled, "{locale:?}");
                assert_eq!(model.emphasis, Emphasis::Quiet, "{locale:?}");
                assert_eq!(
                    model.label,
                    catalog.text(TextKey::StartStreaming),
                    "{locale:?}"
                );
                assert!(
                    model
                        .accessible_name
                        .contains(catalog.text(TextKey::ReadyExplanation)),
                    "{locale:?}: an inert command has to say why, and said only {:?}",
                    model.accessible_name
                );
            }
        }

        /// A failed session with nothing to retry shows its command inert.
        ///
        /// This was the worse half of the defect the inert Start removed.
        /// `Failed` handed back an *enabled* Retry no matter whether anything
        /// could be started, and Retry takes the filled slot on the Command
        /// Home -- so the page's dominant command was a button that sends
        /// `StartRequested` into a reducer that drops it, because
        /// `available_selected_ids` applies the very predicate `can_start`
        /// reports. Paragraph 11 forbids exposing a command that is invalid
        /// for the snapshot, and a filled dead button breaks that more
        /// visibly than an absent one.
        #[test]
        fn a_failed_session_without_a_startable_selection_still_shows_an_inert_retry() {
            let mut state = base_state();
            state.receivers[0].availability = Availability::Unavailable;
            state.stream = StreamState::Failed {
                generation: GenerationId(9),
                summary: "Could not connect".into(),
            };
            let snapshot = snapshot_of(&state);
            assert!(
                !snapshot.can_start && !snapshot.can_stop,
                "the fixture has to be an unretryable failure for this to mean anything"
            );

            let action = overview(&state, ResolvedLocale::English)
                .primary
                .expect("a failed session must still name its command");
            assert_eq!(action.label(), TextKey::Retry);
            assert!(!action.is_enabled(), "there is nothing to retry");

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = CommandActionModel::lifecycle(
                    catalog,
                    action,
                    filled_action_owner(Some(action), false, true),
                );
                assert!(!model.enabled, "{locale:?}");
                assert_eq!(
                    model.emphasis,
                    Emphasis::Quiet,
                    "{locale:?}: a dead command must not hold the filled slot"
                );
                assert_eq!(model.label, catalog.text(TextKey::Retry), "{locale:?}");
                assert!(
                    model
                        .accessible_name
                        .contains(catalog.text(TextKey::ReadyExplanation)),
                    "{locale:?}: an inert command has to say why, and said only {:?}",
                    model.accessible_name
                );
            }
        }

        /// An enabled command says its verb and nothing else: the
        /// precondition sentence belongs to the state that cannot act.
        #[test]
        fn a_startable_session_announces_the_bare_verb() {
            let action = overview(&base_state(), ResolvedLocale::English)
                .primary
                .expect("a startable session offers Start");
            assert!(action.is_enabled());
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = CommandActionModel::lifecycle(
                    catalog,
                    action,
                    filled_action_owner(Some(action), false, true),
                );
                assert_eq!(model.accessible_name, model.label, "{locale:?}");
            }
        }
    }

    mod route {
        use super::*;

        #[test]
        fn nodes_follow_the_applied_selection_in_display_order() {
            let mut state = base_state();
            state
                .receivers
                .push(receiver(3, "Attic", Availability::Available));
            state.desired_receivers = HashSet::from_iter([rid(1), rid(3)]);
            state.staged_receivers = state.desired_receivers.clone();

            let route = overview(&state, ResolvedLocale::English).route;
            assert_eq!(
                route
                    .nodes
                    .iter()
                    .map(|node| node.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Attic", "Kitchen"],
                "nodes follow the snapshot's name order, not the id order"
            );
        }

        /// A staged-but-unapplied selection is not a route. Apply is what
        /// makes it one, and the Change Bar is what says so.
        #[test]
        fn a_staged_only_selection_does_not_become_a_route() {
            let mut state = base_state();
            state.desired_receivers = HashSet::new();
            state.staged_receivers = HashSet::from_iter([rid(1)]);

            let route = overview(&state, ResolvedLocale::English).route;
            assert!(route.nodes.is_empty());
            assert_eq!(route.phase, RoutePhase::Neutral);
        }

        #[test]
        fn no_node_is_active_while_the_authoritative_session_is_stopped() {
            let mut state = base_state();
            // A stale active set must not survive into the ribbon.
            state.active_receivers = HashSet::from_iter([rid(1)]);
            state.stream = StreamState::Stopped;

            let route = overview(&state, ResolvedLocale::English).route;
            assert!(route
                .nodes
                .iter()
                .all(|node| node.state != RouteNodeState::Active));
            assert_eq!(route.phase, RoutePhase::Neutral);
        }

        #[test]
        fn a_streaming_session_marks_exactly_the_active_receivers_live() {
            let mut state = base_state();
            state.receivers[1].availability = Availability::Available;
            state.stream = StreamState::Streaming {
                generation: GenerationId(5),
            };
            state.active_receivers = HashSet::from_iter([rid(1)]);

            let route = overview(&state, ResolvedLocale::English).route;
            assert_eq!(route.phase, RoutePhase::Live);
            let states: Vec<(&str, RouteNodeState)> = route
                .nodes
                .iter()
                .map(|node| (node.name.as_str(), node.state))
                .collect();
            assert_eq!(
                states,
                vec![
                    ("Kitchen", RouteNodeState::Active),
                    ("Office", RouteNodeState::Selected),
                ]
            );
        }

        #[test]
        fn an_unavailable_receiver_keeps_its_node_and_says_so() {
            let route = overview(&base_state(), ResolvedLocale::English).route;
            let office = route
                .nodes
                .iter()
                .find(|node| node.name == "Office")
                .expect("the unavailable receiver keeps its node");
            assert_eq!(office.state, RouteNodeState::Unavailable);
            assert_eq!(
                office.state_label,
                Catalog::new(ResolvedLocale::English).text(TextKey::ReceiverUnavailable)
            );
        }

        #[test]
        fn four_receivers_still_show_four_nodes() {
            let mut state = base_state();
            state.receivers[1].availability = Availability::Available;
            for index in 3..=4u8 {
                let name = format!("Room {index}");
                state
                    .receivers
                    .push(receiver(index, &name, Availability::Available));
            }
            state.desired_receivers = (1..=4u8).map(rid).collect();
            state.staged_receivers = state.desired_receivers.clone();

            let route = overview(&state, ResolvedLocale::English).route;
            assert_eq!(route.nodes.len(), 4);
            assert!(route.overflow.is_none());
        }

        #[test]
        fn more_than_four_receivers_collapse_into_three_nodes_plus_a_counted_one() {
            let mut state = base_state();
            state.receivers[1].availability = Availability::Available;
            for index in 3..=6u8 {
                let name = format!("Room {index}");
                state
                    .receivers
                    .push(receiver(index, &name, Availability::Available));
            }
            state.desired_receivers = (1..=6u8).map(rid).collect();
            state.staged_receivers = state.desired_receivers.clone();

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let route = overview(&state, locale).route;
                assert_eq!(route.nodes.len(), 3, "{locale:?}");
                assert_eq!(route.total_nodes, 6, "{locale:?}");
                let overflow = route.overflow.as_ref().expect("a counted node");
                assert_eq!(overflow.count, 3, "{locale:?}");
                assert_eq!(overflow.marker, "+3", "{locale:?}");
                assert_eq!(
                    overflow.accessible_name,
                    catalog.more_receivers(ReceiverCountArgs { count: 3 }),
                    "{locale:?}"
                );
                // The collapse is visual only: every name still reaches the
                // accessibility tree through the summary.
                for name in ["Kitchen", "Office", "Room 3", "Room 4", "Room 5", "Room 6"] {
                    assert!(route.summary.contains(name), "{locale:?} lost {name}");
                }
            }
        }

        #[test]
        fn the_summary_is_a_complete_localized_sentence_in_both_locales() {
            let mut state = base_state();
            state.receivers[1].availability = Availability::Available;
            state.stream = StreamState::Streaming {
                generation: GenerationId(2),
            };
            state.active_receivers = HashSet::from_iter([rid(1)]);

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let route = overview(&state, locale).route;
                assert!(route
                    .summary
                    .starts_with(catalog.text(TextKey::RouteSummaryAccessibility)));
                assert!(route.summary.contains(catalog.text(TextKey::RouteLive)));
                assert!(route.summary.contains(catalog.text(TextKey::WindowsAudio)));
                assert!(route
                    .summary
                    .contains(&catalog.name_list(&["Kitchen", "Office"])));
                assert!(route
                    .summary
                    .contains(&catalog.route_summary(RouteSummaryArgs {
                        selected: 2,
                        active: 1
                    })));
            }
        }

        #[test]
        fn a_failed_session_locates_the_failure_on_the_segment() {
            let mut state = base_state();
            state.stream = StreamState::Failed {
                generation: GenerationId(3),
                summary: "Could not connect".into(),
            };
            let route = overview(&state, ResolvedLocale::English).route;
            assert_eq!(route.phase, RoutePhase::Failed);
            assert_eq!(route.failure, Some(RouteFailureLocation::Segment));
        }

        #[test]
        fn a_healthy_route_locates_no_failure() {
            assert_eq!(
                overview(&base_state(), ResolvedLocale::English)
                    .route
                    .failure,
                None
            );
        }
    }

    mod receivers {
        use super::*;

        /// The identifier-to-class mapping, stated against the identifiers
        /// Apple actually ships rather than against the match arms.
        #[test]
        fn a_hardware_identifier_resolves_to_the_class_its_family_names() {
            for (identifier, key) in [
                ("AudioAccessory1,1", TextKey::DeviceClassHomePod),
                ("AudioAccessory1,2", TextKey::DeviceClassHomePod),
                ("AudioAccessory6,1", TextKey::DeviceClassHomePod),
                ("AudioAccessory5,1", TextKey::DeviceClassHomePodMini),
                ("AppleTV5,3", TextKey::DeviceClassAppleTv),
                ("AppleTV6,2", TextKey::DeviceClassAppleTv),
                ("AppleTV11,1", TextKey::DeviceClassAppleTv),
            ] {
                assert_eq!(device_class_key(identifier), key, "{identifier}");
            }
        }

        /// Everything the mapping cannot read has to say "speaker" rather
        /// than guess a product. An empty `model=` is the ordinary case for a
        /// third-party receiver, not an exotic one.
        #[test]
        fn an_unreadable_identifier_falls_back_to_the_plain_speaker_class() {
            for unknown in [
                "",
                "   ",
                "AudioAccessory",
                "AppleTV",
                "Shairport",
                "iPhone14,2",
                "Mac15,3",
                "42",
            ] {
                assert_eq!(
                    device_class_key(unknown),
                    TextKey::DeviceClassSpeaker,
                    "{unknown:?} was given a product name it did not earn"
                );
            }
        }

        /// The class reaches the user in the language of the window, and the
        /// fallback is the half that has to be translated.
        #[test]
        fn the_device_class_is_localized_and_the_fallback_is_translated() {
            let de = Catalog::new(ResolvedLocale::German);
            let en = Catalog::new(ResolvedLocale::English);
            assert_eq!(
                de.text(device_class_key("AudioAccessory5,1")),
                "HomePod mini"
            );
            assert_eq!(
                en.text(device_class_key("AudioAccessory5,1")),
                "HomePod mini"
            );
            assert_eq!(de.text(device_class_key("AppleTV11,1")), "Apple TV");
            assert_eq!(de.text(device_class_key("Shairport")), "Lautsprecher");
            assert_eq!(en.text(device_class_key("Shairport")), "Speaker");
        }

        /// `model=` in the mDNS record is Apple's hardware identifier, and
        /// the card used to print it verbatim: real windows showed
        /// "AudioAccessory5,1" and "AppleTV11,1" where a device class
        /// belongs. Nothing an Apple engineer types into a build script may
        /// reach the Command Home.
        #[test]
        fn no_hardware_identifier_from_the_mdns_record_reaches_a_card() {
            let identifiers = [
                "AudioAccessory5,1",
                "AudioAccessory1,1",
                "AppleTV11,1",
                "AppleTV5,3",
            ];
            for &locale in LOCALES {
                for identifier in identifiers {
                    let mut state = base_state();
                    state.receivers[0].model = identifier.into();
                    let card = &overview(&state, locale).receivers[0];
                    assert!(
                        !card.device_class.contains(identifier),
                        "{locale:?}: the card printed the raw identifier {identifier:?} \
                         as its device class: {:?}",
                        card.device_class
                    );
                    assert!(
                        !card.accessible_name.contains(identifier),
                        "{locale:?}: {identifier:?} reached the announced sentence: {:?}",
                        card.accessible_name
                    );
                }
            }
        }

        /// A receiver with no known name is a real state, not a defect: a
        /// selected speaker restored from the last session is a row before
        /// discovery has ever seen it, and stays one while it is switched
        /// off. The layers below leave the name empty on purpose, because the
        /// only identifier they hold is a MAC address, so the card is where a
        /// name has to be found -- and it must be found in both languages.
        #[test]
        fn a_receiver_with_no_known_name_gets_a_localized_stand_in() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut state = base_state();
                state.receivers[0].name = String::new();

                let card = &overview(&state, locale).receivers[0];

                assert_eq!(
                    card.name,
                    catalog.text(TextKey::ReceiverUnnamed),
                    "{locale:?}"
                );
                assert!(
                    card.accessible_name
                        .contains(catalog.text(TextKey::ReceiverUnnamed)),
                    "{locale:?}: the announced sentence lost the receiver: {:?}",
                    card.accessible_name
                );
            }
        }

        /// The stand-in is for an absent name only; a real one is never
        /// replaced, and a name is never invented for a receiver that has one.
        #[test]
        fn a_receiver_that_has_a_name_keeps_it() {
            let card = &overview(&base_state(), ResolvedLocale::German).receivers[0];
            assert_eq!(card.name, "Kitchen");
        }

        #[test]
        fn a_card_reports_the_staged_selection_not_the_applied_one() {
            let mut state = base_state();
            state.staged_receivers = HashSet::from_iter([rid(2)]);

            let model = overview(&state, ResolvedLocale::English);
            let kitchen = &model.receivers[0];
            let office = &model.receivers[1];
            assert_eq!(kitchen.name, "Kitchen");
            assert!(!kitchen.selected, "Kitchen was unstaged");
            assert!(office.selected, "Office is staged");
        }

        #[test]
        fn a_card_carries_name_device_class_and_availability_in_both_locales() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = overview(&base_state(), locale);
                let office = &model.receivers[1];
                assert_eq!(
                    office.device_class,
                    catalog.text(TextKey::DeviceClassHomePodMini)
                );
                assert_eq!(office.availability, Availability::Unavailable);
                assert_eq!(
                    office.availability_label,
                    catalog.text(TextKey::ReceiverUnavailable),
                    "{locale:?}"
                );
                assert!(office.accessible_name.contains("Office"), "{locale:?}");
            }
        }

        /// Selecting a speaker does not make it reachable.
        ///
        /// The card folds the staged selection and the session into one
        /// `ReceiverVisualState`, and that enum has no room for "selected and
        /// unreachable" -- it reports `Selected`. So the availability fact
        /// has to travel on its own, or a speaker the user checked and that
        /// then went offline announces itself as merely "Selected" and reads
        /// identically to a speaker that is about to play.
        #[test]
        fn a_selected_receiver_that_went_offline_still_says_it_is_unreachable() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                // `base_state` stages Office and marks it unavailable.
                let office = &overview(&base_state(), locale).receivers[1];
                assert!(office.selected, "{locale:?}: the fixture stages Office");
                assert_eq!(office.availability, Availability::Unavailable);
                assert_eq!(
                    office.state,
                    ReceiverVisualState::Selected,
                    "{locale:?}: the selection axis reports the selection"
                );

                assert!(
                    office
                        .accessible_name
                        .contains(catalog.text(TextKey::ReceiverUnavailable)),
                    "{locale:?}: a staged offline receiver hid its availability: {}",
                    office.accessible_name
                );
                assert!(
                    office
                        .accessible_name
                        .contains(catalog.text(TextKey::ReceiverSelected)),
                    "{locale:?}: it also has to keep saying it is selected: {}",
                    office.accessible_name
                );
                assert_eq!(
                    office.availability_symbol,
                    availability_symbol(Availability::Unavailable),
                    "{locale:?}"
                );
            }
        }

        /// Every combination the two axes can take reaches the sentence.
        ///
        /// Written as a loop over the whole matrix rather than over the one
        /// case that regressed, because the enum cannot represent the matrix
        /// and a second fold-in would pass a single-case test.
        #[test]
        fn every_selection_and_availability_combination_announces_both_facts() {
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                for availability in [Availability::Available, Availability::Unavailable] {
                    for staged in [false, true] {
                        for active in [false, true] {
                            let mut state = base_state();
                            state.receivers[1].availability = availability;
                            state.staged_receivers = if staged {
                                HashSet::from_iter([rid(2)])
                            } else {
                                HashSet::new()
                            };
                            state.active_receivers = HashSet::new();
                            if active {
                                state.stream = StreamState::Streaming {
                                    generation: GenerationId(9),
                                };
                                state.active_receivers = HashSet::from_iter([rid(2)]);
                            }

                            let office = &overview(&state, locale).receivers[1];
                            let announced = &office.accessible_name;
                            let context =
                                format!("{locale:?} {availability:?} staged={staged} active={active}: {announced}");

                            assert_eq!(
                                office.availability_label,
                                catalog.text(match availability {
                                    Availability::Available => TextKey::ReceiverAvailable,
                                    Availability::Unavailable => TextKey::ReceiverUnavailable,
                                }),
                                "the availability word is not the catalog's -- {context}"
                            );
                            assert!(
                                announced.contains(&office.availability_label),
                                "availability missing -- {context}"
                            );
                            if office.state.adds_to_availability() {
                                assert!(
                                    announced.contains(&office.state_label),
                                    "state missing -- {context}"
                                );
                            } else {
                                // The resting states *are* the availability
                                // word; saying it twice is a defect too.
                                assert_eq!(
                                    announced.matches(&office.availability_label).count(),
                                    1,
                                    "the availability word is repeated -- {context}"
                                );
                            }
                        }
                    }
                }
            }
        }

        #[test]
        fn streaming_and_selection_are_separate_states_with_separate_words() {
            let mut state = base_state();
            state.receivers[1].availability = Availability::Available;
            state.stream = StreamState::Streaming {
                generation: GenerationId(4),
            };
            state.active_receivers = HashSet::from_iter([rid(1)]);

            let catalog = Catalog::new(ResolvedLocale::English);
            let model = overview(&state, ResolvedLocale::English);
            assert!(model.receivers[0].streaming);
            assert!(!model.receivers[1].streaming);
            assert_eq!(
                model.receivers[0].state_label,
                catalog.text(TextKey::ReceiverSelectedAndActive)
            );
            assert_eq!(
                model.receivers[1].state_label,
                catalog.text(TextKey::ReceiverSelected)
            );
        }

        /// The toggle names what activating it would do, so a screen reader
        /// user hears the outcome rather than the current state twice.
        #[test]
        fn the_toggle_label_names_the_outcome_in_both_locales() {
            let mut state = base_state();
            state.staged_receivers = HashSet::from_iter([rid(1)]);
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = overview(&state, locale);
                assert_eq!(
                    model.receivers[0].toggle_label,
                    catalog.text(TextKey::DeselectReceiver),
                    "{locale:?}"
                );
                assert_eq!(
                    model.receivers[1].toggle_label,
                    catalog.text(TextKey::SelectReceiver),
                    "{locale:?}"
                );
            }
        }

        #[test]
        fn advanced_details_appear_only_in_advanced_mode_and_state_applied_membership() {
            let mut state = base_state();
            state.desired_receivers = HashSet::from_iter([rid(1)]);
            state.staged_receivers = HashSet::from_iter([rid(1), rid(2)]);

            assert!(overview(&state, ResolvedLocale::English)
                .receivers
                .iter()
                .all(|card| card.advanced_details.is_none()));

            state.advanced_information = true;
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let model = overview(&state, locale);
                assert_eq!(
                    model.receivers[0].advanced_details.as_deref(),
                    Some(catalog.text(TextKey::SessionMembershipIncluded)),
                    "{locale:?}"
                );
                assert_eq!(
                    model.receivers[1].advanced_details.as_deref(),
                    Some(catalog.text(TextKey::SessionMembershipExcluded)),
                    "{locale:?}"
                );
            }
        }

        #[test]
        fn a_card_keeps_the_typed_identity_its_toggle_needs() {
            let model = overview(&base_state(), ResolvedLocale::English);
            assert_eq!(
                model
                    .receivers
                    .iter()
                    .map(|card| card.id.clone())
                    .collect::<Vec<_>>(),
                vec![rid(1), rid(2)]
            );
        }
    }

    mod audio {
        use super::*;

        #[test]
        fn the_dock_reports_the_authoritative_volume_as_a_localized_percentage() {
            let mut state = base_state();
            state.master_volume = 0.735;
            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let audio = overview(&state, locale).audio;
                assert_eq!(audio.value, 0.735);
                assert_eq!(
                    audio.percent, 74,
                    "the percentage rounds, it never truncates"
                );
                assert_eq!(
                    audio.percent_text,
                    catalog.percentage(PercentageArgs { percent: 74 }),
                    "{locale:?}"
                );
                assert_eq!(
                    audio.label,
                    catalog.text(TextKey::MasterVolume),
                    "{locale:?}"
                );
                assert_eq!(
                    audio.accessible_name,
                    catalog.text(TextKey::MasterVolumeAccessibility),
                    "{locale:?}"
                );
            }
        }

        #[test]
        fn the_dock_clamps_an_out_of_range_reading_instead_of_painting_past_the_track() {
            let mut state = base_state();
            state.master_volume = 1.4;
            assert_eq!(overview(&state, ResolvedLocale::English).audio.value, 1.0);
            state.master_volume = -0.2;
            assert_eq!(overview(&state, ResolvedLocale::English).audio.value, 0.0);
        }

        /// The specification asks for deterministic arrow-key increments. The
        /// dock shows whole percentage points, so a step that did not divide
        /// the range would move the visible number by four points here and
        /// five points there, depending on where the slider stood.
        #[test]
        fn the_keyboard_step_lands_on_whole_percentage_points() {
            let percent_step = VOLUME_STEP * 100.0;
            assert!(
                (percent_step - percent_step.round()).abs() < 1e-4,
                "one step moves the percentage by {percent_step}"
            );
            let steps = 1.0 / VOLUME_STEP;
            assert!(
                (steps - steps.round()).abs() < 1e-4,
                "{VOLUME_STEP} leaves a remainder"
            );
            assert_eq!(
                steps.round(),
                20.0,
                "silence and full scale must both be reachable"
            );
        }
    }

    mod notices_and_metrics {
        use super::*;

        #[test]
        fn every_notice_reaches_the_overview_as_catalog_copy() {
            for code in [
                crate::app::NoticeCode::NoReceiverAvailable,
                crate::app::NoticeCode::DiscoveryFailed,
                crate::app::NoticeCode::SessionFailed,
                crate::app::NoticeCode::ControllerUnavailable,
                crate::app::NoticeCode::PreferencesFailed,
                crate::app::NoticeCode::HotkeyFailed,
                crate::app::NoticeCode::InvalidInput,
            ] {
                let mut state = base_state();
                state.notice = Some(UserNotice {
                    severity: Severity::Error,
                    code,
                    summary: "C:\\Users\\someone\\openaircast: os error 2".into(),
                    action: Some(CorrectiveAction::Retry),
                });
                for &locale in LOCALES {
                    let catalog = Catalog::new(locale);
                    let notice = overview(&state, locale).notice.expect("a notice");
                    assert_eq!(notice.message, catalog.text(message_key(code)), "{code:?}");
                }
            }
        }

        /// Every string the Overview model can put on screen or into the
        /// accessibility tree.
        fn model_strings(model: &OverviewModel) -> Vec<String> {
            let mut strings = vec![
                model.title.clone(),
                model.explanation.clone(),
                model.route.source_label.clone(),
                model.route.phase_label.clone(),
                model.route.summary.clone(),
                model.audio.label.clone(),
                model.audio.accessible_name.clone(),
                model.audio.percent_text.clone(),
            ];
            for node in &model.route.nodes {
                strings.push(node.name.clone());
                strings.push(node.state_label.clone());
            }
            if let Some(overflow) = &model.route.overflow {
                strings.push(overflow.marker.clone());
                strings.push(overflow.accessible_name.clone());
            }
            for card in &model.receivers {
                strings.push(card.name.clone());
                strings.push(card.device_class.clone());
                strings.push(card.availability_label.clone());
                strings.push(card.state_label.clone());
                strings.push(card.accessible_name.clone());
                strings.push(card.toggle_label.clone());
                strings.extend(card.advanced_details.clone());
            }
            if let Some(notice) = &model.notice {
                strings.push(notice.message.clone());
                strings.extend(notice.consequence.clone());
                strings.extend(notice.action.as_ref().map(|action| action.label.clone()));
            }
            for tile in &model.advanced_metrics {
                strings.push(tile.accessible_phrase());
            }
            strings
        }

        #[test]
        fn no_model_string_ever_carries_the_backend_summary_or_the_device_address() {
            let mut state = base_state();
            state.advanced_information = true;
            state.stream = StreamState::Failed {
                generation: GenerationId(11),
                summary: "Authentication failed at 192.168.1.44:7000".into(),
            };
            state.notice = Some(UserNotice {
                severity: Severity::Error,
                code: crate::app::NoticeCode::SessionFailed,
                summary: "Authentication failed at 192.168.1.44:7000".into(),
                action: Some(CorrectiveAction::Retry),
            });

            for &locale in LOCALES {
                let model = overview(&state, locale);
                for text in model_strings(&model) {
                    assert!(!text.contains("192.168"), "{locale:?}: {text}");
                    assert!(
                        !text.contains("Authentication failed"),
                        "{locale:?}: {text}"
                    );
                    assert!(
                        !text.to_lowercase().contains("a0b1c2"),
                        "{locale:?} leaks the device address: {text}"
                    );
                }
            }
        }

        #[test]
        fn standard_mode_shows_no_advanced_metrics_at_all() {
            assert!(overview(&base_state(), ResolvedLocale::English)
                .advanced_metrics
                .is_empty());
        }

        #[test]
        fn advanced_mode_measures_the_counts_the_snapshot_really_carries() {
            let mut state = base_state();
            state.advanced_information = true;
            state.stream = StreamState::Streaming {
                generation: GenerationId(6),
            };
            state.active_receivers = HashSet::from_iter([rid(1)]);

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let metrics = overview(&state, locale).advanced_metrics;
                assert_eq!(metrics.len(), 2, "{locale:?}");
                assert_eq!(
                    metrics[0].label,
                    catalog.text(TextKey::Active),
                    "{locale:?}"
                );
                assert_eq!(metrics[0].value, "1", "{locale:?}");
                assert_eq!(metrics[0].freshness, MetricFreshness::Live, "{locale:?}");
                assert_eq!(
                    metrics[1].label,
                    catalog.text(TextKey::Available),
                    "{locale:?}"
                );
                assert_eq!(metrics[1].value, "1", "{locale:?}");
                assert_eq!(metrics[1].freshness, MetricFreshness::Live, "{locale:?}");
            }
        }

        /// Discovery that has never run has not measured a zero.
        #[test]
        fn an_unmeasured_availability_count_is_unknown_and_never_zero() {
            let mut state = base_state();
            state.advanced_information = true;
            state.discovery = DiscoveryState::Idle;

            for &locale in LOCALES {
                let catalog = Catalog::new(locale);
                let metrics = overview(&state, locale).advanced_metrics;
                assert_eq!(metrics[1].freshness, MetricFreshness::Unknown, "{locale:?}");
                assert_eq!(
                    metrics[1].value,
                    catalog.text(TextKey::Unknown),
                    "{locale:?}"
                );
                assert_ne!(metrics[1].value, "0", "{locale:?}");
            }
        }

        #[test]
        fn a_failed_discovery_marks_the_last_known_count_stale() {
            let mut state = base_state();
            state.advanced_information = true;
            state.discovery = DiscoveryState::Failed {
                summary: "Network unavailable".into(),
            };

            let metrics = overview(&state, ResolvedLocale::English).advanced_metrics;
            assert_eq!(metrics[1].freshness, MetricFreshness::Stale);
            assert_eq!(metrics[1].value, "1");
        }
    }

    mod purity {
        /// The mapping must stay a pure function of snapshot and catalog: a
        /// clock or an I/O call would make the Overview untestable without a
        /// frame and unreproducible in a support export.
        const SOURCE: &str = include_str!("presentation.rs");

        fn body() -> &'static str {
            SOURCE
                .split("#[cfg(test)]")
                .next()
                .expect("the models precede their tests")
        }

        fn impurities(text: &str) -> Vec<&'static str> {
            [
                concat!("Instant::", "now"),
                concat!("SystemTime::", "now"),
                concat!("std::", "fs"),
                concat!("std::", "thread"),
                concat!("egui::", "Context"),
            ]
            .into_iter()
            .filter(|needle| text.contains(needle))
            .collect()
        }

        #[test]
        fn the_models_read_no_clock_no_disk_and_no_frame() {
            assert!(impurities(body()).is_empty(), "{:?}", impurities(body()));
        }

        #[test]
        fn the_purity_guard_flags_a_planted_call() {
            assert_eq!(
                impurities(&format!("let t = {}();", concat!("Instant::", "now"))).len(),
                1
            );
        }
    }
}
