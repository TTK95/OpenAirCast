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
mod tests;

#[cfg(test)]
mod command_home_tests;
