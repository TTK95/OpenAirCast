use std::collections::HashSet;

#[cfg(test)]
mod receiver_level_tests;

use airplay_core::DeviceId;

use super::{
    AppEffect, AppEvent, AppState, AudioEndpointRequest, AudioEndpointSelection,
    AudioSourceReading, Availability, ControllerEvent, CorrectiveAction, DiagnosticsReading,
    DiscoveryState, GenerationId, HotkeyBinding, LatencyChoice, LatencyReading, LocalePreference,
    NoticeCode, Page, Preferences, PreferencesEvent, ReceiverState, ResolvedLocale, Severity,
    StreamState, ThemePreference, UserNotice, WindowGeometry, PREFERENCES_SCHEMA_VERSION,
};

/// What the shell says when the settings file could not be written.
///
/// Owned here, not by the worker: the outcome event carries a generation and
/// no text at all, so the concrete failure stays in the log and this sentence
/// is the only thing the user ever sees.
const PREFERENCES_PERSIST_FAILED_SUMMARY: &str = "Your settings could not be saved.";

#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub effects: Vec<AppEffect>,
    pub snapshot_changed: bool,
}

/// The one place an [`AppEvent`] turns into state.
///
/// `unreachable_patterns` is denied here rather than merely warned: a
/// catch-all placed *before* the named arms would swallow events exactly like
/// the old wildcard did, and a warning in a workspace this size is a warning
/// nobody reads. Placed *after* the named arms it is caught instead by
/// `tests::the_event_dispatch_has_no_wildcard_arm`, which reads this
/// function's own source. Between them there is no position where a catch-all
/// survives.
#[deny(unreachable_patterns)]
pub fn reduce(state: &mut AppState, event: AppEvent) -> Transition {
    if state.hardware_check.phase == super::hardware_check::HardwareCheckPhase::StopPending
        && matches!(
            event,
            AppEvent::StartRequested
                | AppEvent::GlobalHotkeyPressed
                | AppEvent::GroupRequested(super::GroupCommand::Apply { start: true, .. })
        )
    {
        return finish(state, vec![], false);
    }
    let check_before = super::hardware_check::HardwareCheckSnapshot::from_state(state);
    let observed_event = HardwareCheckObservation::from_event(&event);
    let (effects, snapshot_changed) = match event {
        AppEvent::HardwareCheck(action) => hardware_check_requested(state, action),
        AppEvent::GroupRequested(command) => group_requested(state, command),
        AppEvent::Controller(ControllerEvent::SavedGroupsChanged { groups }) => {
            let changed = state.saved_groups.as_ref() != Some(&groups);
            state.saved_groups = Some(groups);
            (vec![], changed)
        }
        AppEvent::Controller(ControllerEvent::GroupOperationFinished { request, status }) => {
            if let Some(operation) = state.group_operation.as_mut().filter(|operation| {
                operation.request == request
                    && operation.status == super::GroupOperationStatus::Pending
            }) {
                operation.status = status;
                (vec![], true)
            } else {
                (vec![], false)
            }
        }
        AppEvent::ToggleStagedReceiver(receiver_id) => toggle_staged(state, receiver_id),
        AppEvent::ApplyStagedReceivers => apply_staged(state),
        AppEvent::DiscardStagedReceivers => discard_staged(state),
        AppEvent::StartRequested => start_requested(state),
        AppEvent::StopRequested => stop_requested(state),
        AppEvent::Navigate(page) => navigate(state, page),
        AppEvent::MainWindowCloseRequested => main_window_close_requested(state),
        AppEvent::ShowMainWindow => show_main_window(state),
        AppEvent::GlobalHotkeyPressed => global_hotkey_pressed(state),
        AppEvent::RefreshRequested => refresh_requested(state),
        AppEvent::MasterVolumeChanged(volume) => master_volume_changed(state, volume),
        AppEvent::ReceiverLevelRequested { receiver, level } => {
            if state.shutting_down
                || state.receiver_levels.is_none()
                || !level.is_finite()
                || !state.receivers.iter().any(|row| row.id == receiver)
            {
                (vec![], false)
            } else {
                // Do not claim the value was saved before the backend confirms it.
                // Re-sending an unchanged level permits retry after queue pressure.
                (
                    vec![AppEffect::ApplyReceiverLevel {
                        receiver,
                        level: level.clamp(0.0, 1.0),
                    }],
                    false,
                )
            }
        }
        AppEvent::Controller(ControllerEvent::ReceiverLevelsChanged { levels }) => {
            let changed = state.receiver_levels.as_ref() != Some(&levels);
            state.receiver_levels = Some(levels);
            (vec![], changed)
        }
        AppEvent::MuteChangeRequested(muted) => mute_change_requested(state, muted),
        AppEvent::LatencyChoiceRequested(choice) => latency_choice_requested(state, choice),
        AppEvent::AudioEndpointRequested(request) => audio_endpoint_requested(state, request),
        AppEvent::ThemeChanged(theme) => theme_changed(state, theme),
        AppEvent::LocaleChanged(preference) => locale_changed(state, preference),
        AppEvent::AdvancedInformationChanged(enabled) => {
            advanced_information_changed(state, enabled)
        }
        AppEvent::WindowsDisplayLanguageChanged(locale) => {
            windows_display_language_changed(state, locale)
        }
        AppEvent::HotkeyChanged(binding) => hotkey_changed(state, binding),
        AppEvent::WindowGeometryChanged(geometry) => window_geometry_changed(state, geometry),
        AppEvent::Preferences(PreferencesEvent::HotkeyReconfigured {
            generation,
            applied,
            failure,
        }) => hotkey_reconfigured(state, generation, applied, failure),
        AppEvent::Preferences(PreferencesEvent::Persisted { generation }) => {
            preferences_persisted(state, generation)
        }
        AppEvent::Preferences(PreferencesEvent::PersistFailed { generation }) => {
            preferences_persist_failed(state, generation)
        }
        AppEvent::RetryPreferencesPersistence => retry_preferences_persistence(state),
        AppEvent::QuitRequested => quit_requested(state),
        AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
            generation,
            receivers,
        }) => merge_discovery(state, generation, receivers),
        AppEvent::Controller(ControllerEvent::DesiredReceiversChanged {
            desired_revision,
            receiver_ids,
        }) => update_desired(state, desired_revision, receiver_ids),
        AppEvent::Controller(ControllerEvent::SessionStarted {
            generation,
            active_receiver_ids,
        }) => session_started(state, generation, active_receiver_ids),
        AppEvent::Controller(ControllerEvent::SessionDegraded {
            generation,
            active_receiver_ids,
        }) => session_degraded(state, generation, active_receiver_ids),
        AppEvent::Controller(ControllerEvent::SessionRestarting { generation }) => {
            session_restarting(state, generation)
        }
        AppEvent::Controller(ControllerEvent::SessionStopped { generation }) => {
            session_stopped(state, generation)
        }
        AppEvent::Controller(ControllerEvent::SessionFailed {
            generation,
            summary,
        }) => session_failed(state, generation, summary),
        AppEvent::Controller(ControllerEvent::DiscoveryFailed {
            generation,
            summary,
        }) => discovery_failed(state, generation, summary),
        AppEvent::Controller(ControllerEvent::ChannelClosed { summary }) => {
            channel_closed(state, summary)
        }
        AppEvent::Controller(ControllerEvent::DiagnosticsUpdated { reading }) => {
            diagnostics_updated(state, reading)
        }
        AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated { reading }) => {
            if state.controller_closed
                || reading.generation != state.generations.session
                || state.live_diagnostics.as_ref() == Some(&reading)
            {
                (Vec::new(), false)
            } else {
                state.live_diagnostics = Some(reading);
                (Vec::new(), true)
            }
        }
        AppEvent::Controller(ControllerEvent::MuteChanged { muted }) => mute_changed(state, muted),
        AppEvent::Controller(ControllerEvent::LatencyChanged { reading }) => {
            latency_changed(state, reading)
        }
        AppEvent::Controller(ControllerEvent::AudioSourceChanged { reading }) => {
            audio_source_changed(state, reading)
        }
        AppEvent::Controller(ControllerEvent::VolumeApplied { generation, volume }) => {
            if volume.is_finite()
                && (generation == GenerationId(0) || generation == state.generations.volume)
            {
                state.confirmed_master_volume = Some(volume);
            }
            if volume.is_finite()
                && (generation == state.generations.volume
                    || (generation == GenerationId(0) && !state.volume_pending))
            {
                let changed = state.master_volume != volume;
                state.master_volume = volume;
                if generation != GenerationId(0) {
                    state.volume_pending = false;
                }
                (Vec::new(), changed)
            } else {
                (Vec::new(), false)
            }
        }
        // Deliberately not handled: the settings file is read once, before
        // the event loop exists, and `initial_state` folds the outcome --
        // value *and* notice -- straight into the first `AppState`. There is
        // therefore no moment at which either variant could arrive, and an
        // arm that wrote them into a running state would be a second,
        // unreachable startup path. If a reload ever becomes asynchronous,
        // this is the arm that has to grow a body.
        AppEvent::Preferences(
            PreferencesEvent::Loaded {
                value: _,
                notice: _,
            }
            | PreferencesEvent::LoadFailed { summary: _ },
        ) => (Vec::new(), false),
    };

    let mut effects = effects;
    let check_changed = observe_hardware_check(state, observed_event, &mut effects);
    finish(
        state,
        effects,
        snapshot_changed
            || check_changed
            || check_before != super::hardware_check::HardwareCheckSnapshot::from_state(state),
    )
}

fn hardware_check_requested(
    state: &mut AppState,
    action: super::hardware_check::HardwareCheckAction,
) -> (Vec<AppEffect>, bool) {
    use super::hardware_check::{
        HardwareCheckAction as A, HardwareCheckPhase as P, HardwareCheckSnapshot,
    };
    if state.shutting_down {
        return (vec![], false);
    }
    match action {
        A::Open => {
            state.page = Page::Diagnostics;
            (vec![], true)
        }
        A::Prepare(value) => {
            state.hardware_check.prepared = value;
            (vec![], true)
        }
        A::LowerVolume => master_volume_changed(state, 0.10),
        A::Start => {
            if !HardwareCheckSnapshot::from_state(state).ready || !state.hardware_check.prepared {
                return (vec![], false);
            }
            let result = start_requested(state);
            let mut check = std::mem::take(&mut state.hardware_check);
            check.bind(state);
            state.hardware_check = check;
            result
        }
        A::Heard(heard) => {
            if !HardwareCheckSnapshot::from_state(state).can_confirm {
                return (vec![], false);
            }
            let result = stop_requested(state);
            state.hardware_check.phase = P::StopPending;
            state.hardware_check.generation = Some(state.generations.session);
            state.hardware_check.outcome = Some(heard);
            result
        }
        A::Cancel => {
            if !matches!(state.hardware_check.phase, P::Connecting | P::Listening) {
                return (vec![], false);
            }
            let result = stop_requested(state);
            state.hardware_check.phase = P::StopPending;
            state.hardware_check.generation = Some(state.generations.session);
            state.hardware_check.outcome = None;
            result
        }
    }
}

#[derive(Clone, Copy)]
struct HardwareCheckObservation {
    configuration_request: bool,
    fault_generation: Option<GenerationId>,
    stopped_generation: Option<GenerationId>,
}

impl HardwareCheckObservation {
    fn from_event(event: &AppEvent) -> Self {
        use super::hardware_check::HardwareCheckAction as A;
        Self {
            configuration_request: matches!(
                event,
                AppEvent::MasterVolumeChanged(_)
                    | AppEvent::ReceiverLevelRequested { .. }
                    | AppEvent::MuteChangeRequested(_)
                    | AppEvent::LatencyChoiceRequested(_)
                    | AppEvent::AudioEndpointRequested(_)
                    | AppEvent::GroupRequested(_)
                    | AppEvent::ToggleStagedReceiver(_)
                    | AppEvent::HardwareCheck(A::LowerVolume)
            ),
            fault_generation: match event {
                AppEvent::Controller(
                    ControllerEvent::SessionFailed { generation, .. }
                    | ControllerEvent::SessionRestarting { generation }
                    | ControllerEvent::SessionDegraded { generation, .. },
                ) => Some(*generation),
                _ => None,
            },
            stopped_generation: match event {
                AppEvent::Controller(ControllerEvent::SessionStopped { generation }) => {
                    Some(*generation)
                }
                _ => None,
            },
        }
    }
}

fn observe_hardware_check(
    state: &mut AppState,
    event: HardwareCheckObservation,
    effects: &mut Vec<AppEffect>,
) -> bool {
    use super::hardware_check::HardwareCheckPhase as P;
    let before = (
        state.hardware_check.phase,
        state.hardware_check.outcome,
        state.hardware_check.prepared,
        state.hardware_check.generation,
    );
    let configuration_request = event.configuration_request;
    let matching_fault = event.fault_generation.is_some()
        && event.fault_generation == state.hardware_check.generation
        && event.fault_generation == Some(state.generations.session);
    match state.hardware_check.phase {
        P::Connecting | P::Listening => {
            let generation = state.hardware_check.generation;
            if configuration_request
                || !state.hardware_check.matches(state)
                || generation != Some(state.generations.session)
                || !matches!(
                    state.stream,
                    StreamState::Starting { .. } | StreamState::Streaming { .. }
                )
            {
                state.hardware_check.outcome = None;
                let (stop, _) = stop_requested(state);
                effects.extend(stop);
                if !matches!(state.stream, StreamState::Stopping { .. }) {
                    let generation = next_generation(&mut state.generations.session);
                    state.stream = StreamState::Stopping { generation };
                    effects.push(AppEffect::StopSession { generation });
                }
                state.hardware_check.phase = P::StopPending;
                state.hardware_check.generation = Some(state.generations.session);
            } else if matches!(state.stream, StreamState::Streaming { .. })
                && state.active_receivers == state.desired_receivers
            {
                if state.hardware_check.phase == P::Connecting {
                    effects.push(AppEffect::PlayListeningTone {
                        generation: state.generations.session,
                    });
                }
                state.hardware_check.phase = P::Listening;
            }
        }
        P::StopPending => {
            if configuration_request || matching_fault || !state.hardware_check.matches(state) {
                state.hardware_check.outcome = None;
            }
            if event.stopped_generation.is_some()
                && event.stopped_generation == state.hardware_check.generation
                && matches!(state.stream, StreamState::Stopped)
            {
                state.hardware_check.phase = match state.hardware_check.outcome {
                    Some(true) => P::Heard,
                    Some(false) => P::NotHeard,
                    None => P::Invalidated,
                };
                state.hardware_check.prepared = false;
            }
        }
        P::Heard | P::NotHeard => {
            if configuration_request
                || matching_fault
                || !state.hardware_check.matches(state)
                || !matches!(state.stream, StreamState::Stopped)
            {
                state.hardware_check.phase = P::Invalidated;
            }
        }
        P::Preparation | P::Invalidated => {}
    }
    before
        != (
            state.hardware_check.phase,
            state.hardware_check.outcome,
            state.hardware_check.prepared,
            state.hardware_check.generation,
        )
}

fn toggle_staged(state: &mut AppState, receiver_id: DeviceId) -> (Vec<AppEffect>, bool) {
    if !state.staged_receivers.remove(&receiver_id) {
        state.staged_receivers.insert(receiver_id);
    }
    prune_irrelevant_unavailable(state);

    (Vec::new(), true)
}

fn apply_staged(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if state.desired_known {
        let mut receiver_ids: Vec<_> = state.staged_receivers.iter().cloned().collect();
        receiver_ids.sort();
        return group_requested(state, super::GroupCommand::ApplySelection { receiver_ids });
    }
    state.desired_revision += 1;
    state.desired_receivers = state.staged_receivers.clone();
    state.staged_base_revision = state.desired_revision;
    prune_irrelevant_unavailable(state);

    if !state.stream.is_running() {
        return (Vec::new(), true);
    }

    let effect = if state.desired_receivers.is_empty() {
        let generation = next_generation(&mut state.generations.session);
        Some(AppEffect::StopSession { generation })
    } else {
        let receiver_ids = available_selected_ids(state);
        if receiver_ids.is_empty() {
            None
        } else {
            let generation = next_generation(&mut state.generations.session);
            Some(AppEffect::StartSession {
                generation,
                receiver_ids,
                volume: state.master_volume,
            })
        }
    };

    (effect.into_iter().collect(), true)
}

fn start_requested(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if state.shutting_down
        || !matches!(
            state.stream,
            StreamState::Stopped | StreamState::Failed { .. }
        )
    {
        return (Vec::new(), false);
    }

    let mut receiver_ids = available_selected_ids(state);
    if receiver_ids.is_empty() {
        return (Vec::new(), false);
    }
    if state.desired_known {
        receiver_ids = state.desired_receivers.iter().cloned().collect();
        receiver_ids.sort();
    }

    start_with_receiver_ids(state, receiver_ids)
}

fn start_with_receiver_ids(
    state: &mut AppState,
    receiver_ids: Vec<DeviceId>,
) -> (Vec<AppEffect>, bool) {
    let generation = next_generation(&mut state.generations.session);
    state.stream = StreamState::Starting { generation };
    state.active_receivers.clear();

    (
        vec![AppEffect::StartSession {
            generation,
            receiver_ids,
            volume: state.master_volume.clamp(0.0, 1.0),
        }],
        true,
    )
}

fn stop_requested(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if !state.stream.is_running() {
        return (Vec::new(), false);
    }

    let generation = next_generation(&mut state.generations.session);
    state.stream = StreamState::Stopping { generation };
    state.active_receivers.clear();

    (vec![AppEffect::StopSession { generation }], true)
}

fn session_started(
    state: &mut AppState,
    generation: GenerationId,
    active_receiver_ids: Vec<DeviceId>,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.session {
        return (Vec::new(), false);
    }

    let active_receivers = active_receiver_ids.into_iter().collect();
    let stream = StreamState::Streaming { generation };
    let retracts_notice = clear_session_failure_notice(state);
    let snapshot_changed =
        state.stream != stream || state.active_receivers != active_receivers || retracts_notice;
    state.stream = stream;
    state.active_receivers = active_receivers;

    (Vec::new(), snapshot_changed)
}

/// The session is alive but incomplete.
///
/// Shares the shape of [`session_started`] and differs only in the state it
/// writes, because that difference is the whole point: the active set says
/// which receivers really carry audio, and the phase says the delivery is not
/// the one that was asked for. No notice is raised -- the phase itself is the
/// statement, and a notice would put a dismissible banner over a condition
/// the backend is still actively working on.
fn session_degraded(
    state: &mut AppState,
    generation: GenerationId,
    active_receiver_ids: Vec<DeviceId>,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.session {
        return (Vec::new(), false);
    }

    let active_receivers = active_receiver_ids.into_iter().collect();
    let stream = StreamState::Degraded { generation };
    let retracts_notice = clear_session_failure_notice(state);
    let snapshot_changed =
        state.stream != stream || state.active_receivers != active_receivers || retracts_notice;
    state.stream = stream;
    state.active_receivers = active_receivers;

    (Vec::new(), snapshot_changed)
}

/// The controller is rebuilding the group under a running session.
///
/// The active set is cleared, and that is a statement rather than
/// housekeeping: a full-group restart tears every negotiated connection down
/// before it builds the next one, so leaving the previous members marked as
/// carrying audio would be the window asserting a fact that stopped being
/// true. They come back on the next `SessionStarted` or `SessionDegraded`.
fn session_restarting(state: &mut AppState, generation: GenerationId) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.session {
        return (Vec::new(), false);
    }

    let stream = StreamState::Restarting { generation };
    let snapshot_changed = state.stream != stream || !state.active_receivers.is_empty();
    state.stream = stream;
    state.active_receivers.clear();

    (Vec::new(), snapshot_changed)
}

/// A matching live session disproves only the earlier session failure.
fn clear_session_failure_notice(state: &mut AppState) -> bool {
    if state
        .notice
        .as_ref()
        .is_some_and(|notice| notice.code == NoticeCode::SessionFailed)
    {
        state.notice = None;
        true
    } else {
        false
    }
}

fn session_stopped(state: &mut AppState, generation: GenerationId) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.session {
        return (Vec::new(), false);
    }

    let snapshot_changed =
        state.stream != StreamState::Stopped || !state.active_receivers.is_empty();
    state.stream = StreamState::Stopped;
    state.active_receivers.clear();

    (Vec::new(), snapshot_changed)
}

fn session_failed(
    state: &mut AppState,
    generation: GenerationId,
    summary: String,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.session {
        return (Vec::new(), false);
    }

    let notice = UserNotice {
        severity: Severity::Error,
        code: NoticeCode::SessionFailed,
        summary,
        action: Some(CorrectiveAction::Retry),
    };
    let snapshot_changed = state.stream != StreamState::Stopped
        || !state.active_receivers.is_empty()
        || state.notice.as_ref() != Some(&notice);
    state.stream = StreamState::Stopped;
    state.active_receivers.clear();
    state.notice = Some(notice);

    (Vec::new(), snapshot_changed)
}

/// Records the newest diagnostics reading, and only when it says something
/// new.
///
/// The registry republishes on every diagnostic event it ingests, and almost
/// none of those move the three values this reading carries. Comparing before
/// storing is what keeps a busy backend from raising the snapshot revision --
/// and repainting the window -- several times a second for a page the user
/// may not even be looking at.
fn diagnostics_updated(
    state: &mut AppState,
    reading: DiagnosticsReading,
) -> (Vec<AppEffect>, bool) {
    if state.diagnostics == Some(reading) {
        return (Vec::new(), false);
    }
    state.diagnostics = Some(reading);
    (Vec::new(), true)
}

fn channel_closed(state: &mut AppState, summary: String) -> (Vec<AppEffect>, bool) {
    state.controller_closed = true;
    let volume_changed = state.volume_pending;
    if state.volume_pending {
        if let Some(confirmed) = state.confirmed_master_volume {
            state.master_volume = confirmed;
        }
        state.volume_pending = false;
    }
    let operation_changed = if let Some(operation) = state
        .group_operation
        .as_mut()
        .filter(|operation| operation.status == super::GroupOperationStatus::Pending)
    {
        operation.status = super::GroupOperationStatus::Failed(super::GroupFailure::Closed);
        true
    } else {
        false
    };
    let notice = UserNotice {
        severity: Severity::Error,
        code: NoticeCode::ControllerUnavailable,
        summary,
        action: None,
    };
    let snapshot_changed =
        volume_changed || operation_changed || state.notice.as_ref() != Some(&notice);
    state.notice = Some(notice);

    (Vec::new(), snapshot_changed)
}

fn discard_staged(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    use super::{GroupCommand, GroupOperationStatus};
    if state
        .group_operation
        .as_ref()
        .is_some_and(|operation| operation.status == GroupOperationStatus::Pending)
    {
        return (Vec::new(), false);
    }
    let acknowledged = state
        .group_operation
        .as_ref()
        .is_some_and(|operation| matches!(operation.command, GroupCommand::ApplySelection { .. }));
    if acknowledged {
        state.group_operation = None;
    }
    let membership_changed = state.staged_receivers != state.desired_receivers
        || state.staged_base_revision != state.desired_revision;

    state.staged_receivers = state.desired_receivers.clone();
    state.staged_base_revision = state.desired_revision;
    let inventory_changed = prune_irrelevant_unavailable(state);

    (
        Vec::new(),
        membership_changed || inventory_changed || acknowledged,
    )
}

/// Folds a completed browse into the inventory, and retires the accusation a
/// previous failed browse left standing.
///
/// The retraction is not a nicety. `DiscoveryPhase::Retrying` is the daemon's
/// ordinary self-healing backoff, so the notice is raised on transient
/// hiccups as much as on a permanent outage; without a way back it would
/// appear on the first stumble and stay for the rest of the session while the
/// search ran perfectly. A warning that never clears is a warning the user
/// learns to ignore, which costs exactly the case it exists for.
///
/// Only its own code is cleared -- see [`is_discovery_failed_notice`]. A
/// successful browse has disproved the search, and nothing else.
fn merge_discovery(
    state: &mut AppState,
    generation: GenerationId,
    receivers: Vec<ReceiverState>,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.discovery {
        return (Vec::new(), false);
    }

    let mut merged: Vec<ReceiverState> =
        Vec::with_capacity(receivers.len() + state.receivers.len());
    for receiver in receivers {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.id == receiver.id)
        {
            if receiver_precedes(&receiver, existing) {
                *existing = receiver;
            }
        } else {
            merged.push(receiver);
        }
    }

    for previous in &state.receivers {
        let is_missing = !merged.iter().any(|receiver| receiver.id == previous.id);
        let is_relevant = state.desired_receivers.contains(&previous.id)
            || state.staged_receivers.contains(&previous.id);
        if is_missing && is_relevant {
            let mut unavailable = previous.clone();
            unavailable.availability = Availability::Unavailable;
            merged.push(unavailable);
        }
    }
    merged.sort_by_key(|receiver| receiver.id.0);

    let retracts_notice = state
        .notice
        .as_ref()
        .is_some_and(is_discovery_failed_notice);

    let snapshot_changed =
        state.discovery != DiscoveryState::Ready || state.receivers != merged || retracts_notice;
    state.discovery = DiscoveryState::Ready;
    state.receivers = merged;
    if retracts_notice {
        state.notice = None;
    }

    (Vec::new(), snapshot_changed)
}

/// The notice a search that could not be carried out publishes.
///
/// The code alone is the test, unlike [`is_scoped_persistence_notice`]: this
/// one is raised from a single place with a single corrective action, so
/// there is no second shape of it to tell apart. A `SessionFailed` or
/// `PreferencesFailed` notice is about something a browse cannot speak to and
/// is therefore left exactly where it is.
fn is_discovery_failed_notice(notice: &UserNotice) -> bool {
    notice.code == NoticeCode::DiscoveryFailed
}

/// Records that the search for receivers could not be carried out.
///
/// The inventory is deliberately left standing. A failed search says nothing
/// about whether the speakers are still there, and replacing the rows with an
/// empty list would tell the user their receivers had disappeared. What
/// changes is the *freshness* of what is shown: `DiscoveryState::Failed` is
/// what makes the window mark the receiver count as no longer measured, and
/// the notice is what says so in words with the one action that can help.
///
/// `summary` is stored but never rendered: [`crate::ui::presentation::
/// NoticeModel::from_notice`] builds its sentence from [`NoticeCode`] alone,
/// so no backend prose can reach the screen through this path.
///
/// The notice is retired again by [`merge_discovery`] as soon as a browse
/// completes. Raising it without that return path would turn every transient
/// backoff into a permanent accusation.
fn discovery_failed(
    state: &mut AppState,
    generation: GenerationId,
    summary: String,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.discovery {
        return (Vec::new(), false);
    }

    let discovery = DiscoveryState::Failed {
        summary: summary.clone(),
    };
    let notice = UserNotice {
        severity: Severity::Warning,
        code: NoticeCode::DiscoveryFailed,
        summary,
        action: Some(CorrectiveAction::Refresh),
    };
    let snapshot_changed = state.discovery != discovery || state.notice.as_ref() != Some(&notice);
    state.discovery = discovery;
    state.notice = Some(notice);

    (Vec::new(), snapshot_changed)
}

fn update_desired(
    state: &mut AppState,
    desired_revision: u64,
    receiver_ids: Vec<DeviceId>,
) -> (Vec<AppEffect>, bool) {
    if desired_revision < state.desired_revision
        || (state.desired_known && desired_revision == state.desired_revision)
    {
        return (Vec::new(), false);
    }

    let staging_was_clean = state.staged_receivers == state.desired_receivers;
    state.desired_known = true;
    state.desired_receivers = receiver_ids.into_iter().collect();
    state.desired_revision = desired_revision;
    if staging_was_clean || state.staged_receivers == state.desired_receivers {
        state.staged_receivers = state.desired_receivers.clone();
        state.staged_base_revision = desired_revision;
    }
    prune_irrelevant_unavailable(state);

    (Vec::new(), true)
}

fn group_requested(state: &mut AppState, command: super::GroupCommand) -> (Vec<AppEffect>, bool) {
    if state.shutting_down
        || state
            .group_operation
            .as_ref()
            .is_some_and(|operation| operation.status == super::GroupOperationStatus::Pending)
    {
        return (vec![], false);
    }
    state.next_group_request += 1;
    let request = state.next_group_request;
    let status = if state.controller_closed {
        super::GroupOperationStatus::Failed(super::GroupFailure::Closed)
    } else {
        super::GroupOperationStatus::Pending
    };
    state.group_operation = Some(super::GroupOperation {
        request,
        command: command.clone(),
        status,
    });
    if state.controller_closed {
        return (vec![], true);
    }
    let generation = if matches!(command, super::GroupCommand::Apply { start: true, .. })
        && matches!(
            state.stream,
            StreamState::Stopped | StreamState::Failed { .. } | StreamState::Stopping { .. }
        ) {
        let generation = next_generation(&mut state.generations.session);
        state.stream = StreamState::Starting { generation };
        state.active_receivers.clear();
        Some(generation)
    } else {
        None
    };
    (
        vec![AppEffect::Group {
            request,
            generation,
            command,
        }],
        true,
    )
}

fn available_selected_ids(state: &AppState) -> Vec<DeviceId> {
    let mut receiver_ids: Vec<_> = state
        .receivers
        .iter()
        .filter(|receiver| {
            receiver.availability == Availability::Available
                && state.desired_receivers.contains(&receiver.id)
        })
        .map(|receiver| receiver.id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    receiver_ids.sort_by_key(|receiver_id| receiver_id.0);
    receiver_ids
}

fn next_generation(counter: &mut GenerationId) -> GenerationId {
    counter.0 += 1;
    *counter
}

fn navigate(state: &mut AppState, page: Page) -> (Vec<AppEffect>, bool) {
    if state.page == page {
        return (Vec::new(), false);
    }

    state.page = page;

    (vec![persist_preferences(state)], true)
}

fn theme_changed(state: &mut AppState, theme: ThemePreference) -> (Vec<AppEffect>, bool) {
    if state.theme == theme {
        return (Vec::new(), false);
    }

    state.theme = theme;

    (vec![persist_preferences(state)], true)
}

fn locale_changed(state: &mut AppState, preference: LocalePreference) -> (Vec<AppEffect>, bool) {
    if state.locale_preference == preference {
        return (Vec::new(), false);
    }

    state.locale_preference = preference;
    state.resolved_locale = preference.resolve(state.windows_display_locale);

    (vec![persist_preferences(state)], true)
}

fn advanced_information_changed(state: &mut AppState, enabled: bool) -> (Vec<AppEffect>, bool) {
    if state.advanced_information == enabled {
        return (Vec::new(), false);
    }

    state.advanced_information = enabled;

    (vec![persist_preferences(state)], true)
}

/// Records what Windows now renders in.
///
/// The cache is updated unconditionally, even while an explicit language is
/// chosen, so a later return to `System` resolves against the current OS
/// language rather than the one that happened to be set when the app started.
/// The published language moves only under `System`, and nothing here is ever
/// written to disk: the OS language is an observation, not a user preference.
fn windows_display_language_changed(
    state: &mut AppState,
    locale: ResolvedLocale,
) -> (Vec<AppEffect>, bool) {
    state.windows_display_locale = locale;

    let resolved = state.locale_preference.resolve(locale);
    let snapshot_changed = state.resolved_locale != resolved;
    state.resolved_locale = resolved;

    (Vec::new(), snapshot_changed)
}

fn window_geometry_changed(
    state: &mut AppState,
    geometry: WindowGeometry,
) -> (Vec<AppEffect>, bool) {
    let Some(valid) = geometry.validate() else {
        return reject_invalid_input(
            state,
            "Window geometry was ignored because it contains invalid values.",
        );
    };

    if state.window.geometry == valid {
        return (Vec::new(), false);
    }

    state.window.geometry = valid;

    (vec![persist_preferences(state)], true)
}

fn master_volume_changed(state: &mut AppState, volume: f32) -> (Vec<AppEffect>, bool) {
    if !volume.is_finite() {
        return reject_invalid_input(state, "Master volume must be between 0% and 100%.");
    }

    let clamped = volume.clamp(0.0, 1.0);
    let generation = next_generation(&mut state.generations.volume);
    if !state.volume_pending {
        state.confirmed_master_volume = Some(state.master_volume);
    }
    state.volume_pending = true;
    let effect = AppEffect::ApplyVolume {
        generation,
        volume: clamped,
    };
    let snapshot_changed = state.master_volume != clamped;
    state.master_volume = clamped;

    (vec![effect], snapshot_changed)
}

/// The user asked for mute or unmute.
///
/// The control keeps showing the applied value until the backend reports a
/// change. An unaccepted send or a failed durable write may leave that value
/// unchanged, in which case the projection has no new reading to publish.
fn mute_change_requested(state: &mut AppState, muted: bool) -> (Vec<AppEffect>, bool) {
    if state.muted == Some(muted) {
        return (Vec::new(), false);
    }

    (vec![AppEffect::ApplyMute(muted)], false)
}

/// The user picked a latency profile.
///
/// Refused unless the newest reading says the profile is selectable. The
/// window already draws a gated profile as unselectable, so this guard is the
/// second lock on the same door: a stale frame, a keyboard activation racing
/// a new reading, or a future caller cannot turn a profile the backend
/// rejects into a command it has to reject again.
fn latency_choice_requested(state: &mut AppState, choice: LatencyChoice) -> (Vec<AppEffect>, bool) {
    let Some(reading) = state.latency.as_ref() else {
        // Nothing has reported, so there is no offer to accept.
        return (Vec::new(), false);
    };

    let selectable = reading
        .options
        .iter()
        .any(|option| option.choice == choice && option.unavailable.is_none());
    if !selectable {
        return (Vec::new(), false);
    }

    if reading.selected == choice {
        return (Vec::new(), false);
    }

    (vec![AppEffect::ApplyLatency(choice)], false)
}

/// The user picked a capture source.
///
/// A key that is no longer on offer is refused rather than sent: the bridge
/// would have to decide what a stale key means, and the only safe answer --
/// never a different device -- is easier to guarantee here, where the offered
/// list is in hand.
fn audio_endpoint_requested(
    state: &mut AppState,
    request: AudioEndpointRequest,
) -> (Vec<AppEffect>, bool) {
    let Some(reading) = state.audio_source.as_ref() else {
        return (Vec::new(), false);
    };

    let selection = match request {
        AudioEndpointRequest::SystemDefault => AudioEndpointSelection::SystemDefault,
        AudioEndpointRequest::Endpoint(key) => {
            if !reading.endpoints.iter().any(|choice| choice.key == key) {
                return (Vec::new(), false);
            }
            AudioEndpointSelection::Chosen(key)
        }
    };

    if reading.selection == selection {
        return (Vec::new(), false);
    }

    (vec![AppEffect::ApplyAudioEndpoint(request)], false)
}

/// The backend's own mute state. The newest reading always wins.
fn mute_changed(state: &mut AppState, muted: bool) -> (Vec<AppEffect>, bool) {
    let changed = state.muted != Some(muted);
    state.muted = Some(muted);

    (Vec::new(), changed)
}

/// The backend's own latency reading. The newest reading always wins.
fn latency_changed(state: &mut AppState, reading: LatencyReading) -> (Vec<AppEffect>, bool) {
    let changed = state.latency.as_ref() != Some(&reading);
    state.latency = Some(reading);

    (Vec::new(), changed)
}

/// The backend's own capture-source reading. The newest reading always wins.
fn audio_source_changed(
    state: &mut AppState,
    reading: AudioSourceReading,
) -> (Vec<AppEffect>, bool) {
    let changed = state.audio_source.as_ref() != Some(&reading);
    state.audio_source = Some(reading);

    (Vec::new(), changed)
}

fn hotkey_changed(state: &mut AppState, binding: HotkeyBinding) -> (Vec<AppEffect>, bool) {
    if state.hotkey == binding {
        return (Vec::new(), false);
    }

    let generation = next_generation(&mut state.generations.hotkey);

    (
        vec![AppEffect::ReconfigureHotkey {
            generation,
            value: binding,
        }],
        false,
    )
}

fn hotkey_reconfigured(
    state: &mut AppState,
    generation: GenerationId,
    applied: HotkeyBinding,
    failure: Option<String>,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.hotkey {
        return (Vec::new(), false);
    }

    let hotkey_changed = state.hotkey != applied;
    state.hotkey = applied;
    let mut snapshot_changed = hotkey_changed;

    if let Some(summary) = failure {
        let notice = UserNotice {
            severity: Severity::Warning,
            code: NoticeCode::HotkeyFailed,
            summary,
            action: Some(CorrectiveAction::OpenSettings),
        };
        snapshot_changed |= state.notice.as_ref() != Some(&notice);
        state.notice = Some(notice);
    }

    if !snapshot_changed {
        return (Vec::new(), false);
    }

    (vec![persist_preferences(state)], true)
}

/// The scoped notice a failed settings write publishes.
///
/// Recognising it again is how a later success knows what it is allowed to
/// clear: an unreadable settings file at startup also carries
/// `PreferencesFailed`, but offers Open Settings, and a successful write says
/// nothing about it.
fn preferences_persist_failed_notice() -> UserNotice {
    UserNotice {
        severity: Severity::Warning,
        code: NoticeCode::PreferencesFailed,
        summary: PREFERENCES_PERSIST_FAILED_SUMMARY.to_string(),
        action: Some(CorrectiveAction::RetryPreferencesPersistence),
    }
}

fn is_scoped_persistence_notice(notice: &UserNotice) -> bool {
    notice.code == NoticeCode::PreferencesFailed
        && notice.action == Some(CorrectiveAction::RetryPreferencesPersistence)
}

/// Whether a notice already on screen outranks a failed settings write.
///
/// Every other notice this reducer publishes answers something the user just
/// did, so replacing whatever stood before it is the right behaviour. This one
/// does not: it is raised by the debounce timer on the preferences thread, and
/// `persist_preferences` runs for a window drag and a page change as much as
/// for a settings edit. On a profile that cannot be written it therefore
/// arrives over and over, unprompted, for as long as the app is open -- while
/// the errors it would land on top of, such as the controller channel closing,
/// are published once and never cleared again.
///
/// So the error keeps the slot. A save that did not happen is recoverable, is
/// logged with its concrete cause, and costs the user nothing this session:
/// the values they set are the values the app is running on. An error is the
/// thing they have to see. Severity is the whole test -- no preference notice
/// is raised at `Error`, so this needs no exception for its own code.
fn outranks_persistence_failure(notice: &UserNotice) -> bool {
    notice.severity == Severity::Error
}

/// Reports a settings write that did not happen.
///
/// The in-memory values stay exactly as the user set them: they are what the
/// app is running on, and the failure is about the file, not about them. The
/// previous file on disk is likewise still whatever it was -- the write was
/// atomic, so a failure replaced nothing.
///
/// It reports only into a slot it is entitled to: see
/// [`outranks_persistence_failure`] for why a standing error keeps it.
fn preferences_persist_failed(
    state: &mut AppState,
    generation: GenerationId,
) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.preferences {
        return (Vec::new(), false);
    }

    if state
        .notice
        .as_ref()
        .is_some_and(outranks_persistence_failure)
    {
        return (Vec::new(), false);
    }

    let notice = preferences_persist_failed_notice();
    let snapshot_changed = state.notice.as_ref() != Some(&notice);
    state.notice = Some(notice);

    (Vec::new(), snapshot_changed)
}

/// Retires the scoped failure notice once the write it accused actually
/// landed, and leaves every other notice alone.
fn preferences_persisted(state: &mut AppState, generation: GenerationId) -> (Vec<AppEffect>, bool) {
    if generation != state.generations.preferences {
        return (Vec::new(), false);
    }

    if !state
        .notice
        .as_ref()
        .is_some_and(is_scoped_persistence_notice)
    {
        return (Vec::new(), false);
    }

    state.notice = None;

    (Vec::new(), true)
}

/// Offers the settings file again, and does nothing else.
///
/// The value offered is the whole current shell state at a fresh generation,
/// not the value that failed: preferences may have moved on since, and the
/// generation is what tells the two outcomes apart. Nothing about the session,
/// discovery, volume, or the hotkey is touched -- that separation is the whole
/// reason this is not `CorrectiveAction::Retry`.
fn retry_preferences_persistence(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    (vec![persist_preferences(state)], false)
}

fn main_window_close_requested(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    let snapshot_changed = state.window.visible;
    state.window.visible = false;

    (vec![AppEffect::HideMainWindow], snapshot_changed)
}

fn show_main_window(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    let snapshot_changed = !state.window.visible;
    state.window.visible = true;

    (vec![AppEffect::ShowMainWindow], snapshot_changed)
}

fn global_hotkey_pressed(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if state.shutting_down {
        return (Vec::new(), false);
    }

    if matches!(
        state.stream,
        StreamState::Starting { .. } | StreamState::Streaming { .. }
    ) {
        return stop_requested(state);
    }

    if available_selected_ids(state).is_empty() {
        if state.desired_receivers.is_empty() {
            if state.desired_known {
                if matches!(
                    state.stream,
                    StreamState::Stopped | StreamState::Failed { .. }
                ) {
                    if let Some(candidate) = first_available_receiver(state) {
                        return start_with_receiver_ids(state, vec![candidate]);
                    }
                }
            } else {
                select_first_available_receiver(state);
            }
        }
        if available_selected_ids(state).is_empty() {
            let notice = UserNotice {
                severity: Severity::Warning,
                code: NoticeCode::NoReceiverAvailable,
                summary: "No AirPlay receiver is available right now.".to_string(),
                action: Some(CorrectiveAction::Refresh),
            };
            let snapshot_changed = state.notice.as_ref() != Some(&notice);
            state.notice = Some(notice);

            return (Vec::new(), snapshot_changed);
        }
    }

    start_requested(state)
}

fn first_available_receiver(state: &AppState) -> Option<DeviceId> {
    state
        .receivers
        .iter()
        .filter(|receiver| receiver.availability == Availability::Available)
        .min_by_key(|receiver| (receiver.name.to_lowercase(), receiver.id.0))
        .map(|receiver| receiver.id.clone())
}

fn select_first_available_receiver(state: &mut AppState) -> bool {
    let Some(candidate) = first_available_receiver(state) else {
        return false;
    };

    state.staged_receivers.insert(candidate.clone());
    state.desired_receivers.insert(candidate);
    state.desired_revision += 1;
    state.staged_base_revision = state.desired_revision;

    true
}

fn refresh_requested(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if state.shutting_down {
        return (Vec::new(), false);
    }

    let generation = next_generation(&mut state.generations.discovery);
    state.discovery = DiscoveryState::Discovering { generation };

    (vec![AppEffect::Discover { generation }], true)
}

fn quit_requested(state: &mut AppState) -> (Vec<AppEffect>, bool) {
    if state.shutting_down {
        return (Vec::new(), false);
    }
    state.shutting_down = true;

    let mut effects = vec![persist_preferences(state)];
    if state.stream.is_running() {
        let generation = next_generation(&mut state.generations.session);
        state.stream = StreamState::Stopping { generation };
        state.active_receivers.clear();
        effects.push(AppEffect::StopSession { generation });
    }
    effects.push(AppEffect::HideMainWindow);
    effects.push(AppEffect::BeginShutdown);

    (effects, true)
}

fn reject_invalid_input(state: &mut AppState, summary: &str) -> (Vec<AppEffect>, bool) {
    let notice = UserNotice {
        severity: Severity::Warning,
        code: NoticeCode::InvalidInput,
        summary: summary.to_string(),
        action: None,
    };
    let snapshot_changed = state.notice.as_ref() != Some(&notice);
    state.notice = Some(notice);

    (Vec::new(), snapshot_changed)
}

fn persist_preferences(state: &mut AppState) -> AppEffect {
    let generation = next_generation(&mut state.generations.preferences);

    AppEffect::PersistPreferences {
        generation,
        value: preferences_from_state(state),
    }
}

/// The single builder for a complete, current-schema preference value.
///
/// Every persisting reducer arm goes through here so no change can drop an
/// unrelated field: the value written is always the whole current shell state,
/// never a patch.
fn preferences_from_state(state: &AppState) -> Preferences {
    Preferences {
        schema_version: PREFERENCES_SCHEMA_VERSION,
        hotkey: state.hotkey,
        theme: state.theme,
        locale: state.locale_preference,
        advanced_information: state.advanced_information,
        window: state.window.geometry,
        last_page: state.page,
        close_to_tray: state.window.close_to_tray,
        launch_at_startup: state.window.launch_at_startup,
    }
}

fn receiver_precedes(candidate: &ReceiverState, current: &ReceiverState) -> bool {
    receiver_preference_key(candidate) < receiver_preference_key(current)
}

fn receiver_preference_key(receiver: &ReceiverState) -> (u8, &str, &str) {
    let availability = match receiver.availability {
        Availability::Available => 0,
        Availability::Unavailable => 1,
    };
    (
        availability,
        receiver.name.as_str(),
        receiver.model.as_str(),
    )
}

fn prune_irrelevant_unavailable(state: &mut AppState) -> bool {
    let original_len = state.receivers.len();
    state.receivers.retain(|receiver| {
        receiver.availability == Availability::Available
            || state.desired_receivers.contains(&receiver.id)
            || state.staged_receivers.contains(&receiver.id)
    });
    state.receivers.len() != original_len
}

fn finish(state: &mut AppState, effects: Vec<AppEffect>, snapshot_changed: bool) -> Transition {
    if snapshot_changed {
        state.revision += 1;
    }

    Transition {
        effects,
        snapshot_changed,
    }
}

#[cfg(test)]
mod tests;
