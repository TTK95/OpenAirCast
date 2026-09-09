use std::collections::HashSet;

#[cfg(test)]
mod receiver_level_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn populated() -> AppState {
        AppState {
            receivers: vec![ReceiverState {
                id: DeviceId([1; 6]),
                name: "Office".into(),
                model: "HomePod".into(),
                availability: Availability::Available,
            }],
            receiver_levels: Some(BTreeMap::from([(DeviceId([1; 6]), 0.6)])),
            master_volume: 0.4,
            stream: StreamState::Streaming {
                generation: GenerationId(9),
            },
            ..AppState::default()
        }
    }

    #[test]
    fn receiver_level_request_preserves_session_and_waits_for_durable_echo() {
        let mut state = populated();
        let before_stream = state.stream.clone();
        let transition = reduce(
            &mut state,
            AppEvent::ReceiverLevelRequested {
                receiver: DeviceId([1; 6]),
                level: 0.5,
            },
        );
        assert_eq!(
            transition.effects,
            vec![AppEffect::ApplyReceiverLevel {
                receiver: DeviceId([1; 6]),
                level: 0.5,
            }]
        );
        assert_eq!(
            state.receiver_levels.as_ref().unwrap()[&DeviceId([1; 6])],
            0.6
        );
        assert_eq!(state.stream, before_stream);
        assert_eq!(state.master_volume, 0.4);
        let levels = BTreeMap::from([(DeviceId([1; 6]), 0.5), (DeviceId([2; 6]), 0.75)]);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::ReceiverLevelsChanged {
                levels: levels.clone(),
            }),
        );
        assert_eq!(state.receiver_levels, Some(levels.clone()));
        reduce(&mut state, AppEvent::MasterVolumeChanged(0.8));
        assert_eq!(state.receiver_levels, Some(levels));
        assert_eq!(state.stream, before_stream);
    }

    #[test]
    fn receiver_level_rejects_unknown_nonfinite_and_unmeasured_requests() {
        let mut state = populated();
        for (receiver, level) in [
            (DeviceId([9; 6]), 0.5),
            (DeviceId([1; 6]), f32::NAN),
            (DeviceId([1; 6]), f32::INFINITY),
        ] {
            assert!(reduce(
                &mut state,
                AppEvent::ReceiverLevelRequested { receiver, level }
            )
            .effects
            .is_empty());
        }
        state.receiver_levels = None;
        assert!(reduce(
            &mut state,
            AppEvent::ReceiverLevelRequested {
                receiver: DeviceId([1; 6]),
                level: 0.5
            }
        )
        .effects
        .is_empty());
    }

    #[test]
    fn receiver_level_clamps_and_allows_resending_unchanged_values() {
        for (input, expected) in [(-1.0, 0.0), (2.0, 1.0), (0.6, 0.6)] {
            let mut state = populated();
            let effects = reduce(
                &mut state,
                AppEvent::ReceiverLevelRequested {
                    receiver: DeviceId([1; 6]),
                    level: input,
                },
            )
            .effects;
            assert_eq!(
                effects,
                vec![AppEffect::ApplyReceiverLevel {
                    receiver: DeviceId([1; 6]),
                    level: expected
                }]
            );
        }
    }

    #[test]
    fn receiver_level_does_not_send_while_shutting_down() {
        let mut state = populated();
        state.shutting_down = true;
        let before = state.clone();
        let result = reduce(
            &mut state,
            AppEvent::ReceiverLevelRequested {
                receiver: DeviceId([1; 6]),
                level: 0.2,
            },
        );
        assert!(result.effects.is_empty());
        assert!(!result.snapshot_changed);
        assert_eq!(state, before);
    }
}

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
mod tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;

    use super::super::{
        AppEffect, AppEvent, AppState, AudioEndpointChoice, AudioEndpointKey, AudioEndpointRequest,
        AudioEndpointSelection, AudioSourceReading, Availability, CaptureState, ControllerEvent,
        CorrectiveAction, GenerationId, LatencyChoice, LatencyOption, LatencyReading,
        LatencyUnavailable, NoticeCode, ReceiverState, Severity, StreamState, UserNotice,
    };
    use super::reduce;

    fn id(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn receiver(last: u8, name: &str) -> ReceiverState {
        receiver_record(last, name, "HomePod", Availability::Available)
    }

    fn receiver_record(
        last: u8,
        name: &str,
        model: &str,
        availability: Availability,
    ) -> ReceiverState {
        ReceiverState {
            id: id(last),
            name: name.into(),
            model: model.into(),
            availability,
        }
    }

    fn ids(values: &[u8]) -> HashSet<DeviceId> {
        values.iter().copied().map(id).collect()
    }

    /// The three audio capabilities, in both directions.
    ///
    /// The rule they share: the window may act on a capability only after the
    /// backend has reported it, and the backend's own reading always wins
    /// afterwards. Requests leave the displayed values alone until a reading
    /// confirms the change, including when rejected work produces no update.
    mod audio {
        use super::*;

        fn latency(selected: LatencyChoice) -> LatencyReading {
            LatencyReading {
                selected,
                options: vec![
                    LatencyOption {
                        choice: LatencyChoice::Low,
                        unavailable: Some(LatencyUnavailable::NotValidated),
                    },
                    LatencyOption {
                        choice: LatencyChoice::Normal,
                        unavailable: None,
                    },
                    LatencyOption {
                        choice: LatencyChoice::Stable,
                        unavailable: Some(LatencyUnavailable::NotValidated),
                    },
                ],
            }
        }

        fn capture() -> AudioSourceReading {
            AudioSourceReading {
                refresh_failed: false,
                endpoints_known: true,
                endpoints: vec![
                    AudioEndpointChoice {
                        key: AudioEndpointKey(0),
                        name: "Arctis 7 Chat".into(),
                    },
                    AudioEndpointChoice {
                        key: AudioEndpointKey(1),
                        name: "Realtek HDMI".into(),
                    },
                ],
                selection: AudioEndpointSelection::SystemDefault,
                captured_key: None,
                captured_name: Some("Realtek HDMI".into()),
                state: CaptureState::Capturing,
            }
        }

        #[test]
        fn a_mute_request_waits_for_confirmation_and_is_sent_on() {
            let mut state = AppState {
                muted: Some(false),
                ..AppState::default()
            };

            let transition = reduce(&mut state, AppEvent::MuteChangeRequested(true));

            assert_eq!(transition.effects, vec![AppEffect::ApplyMute(true)]);
            assert!(!transition.snapshot_changed);
            assert_eq!(state.muted, Some(false));
        }

        #[test]
        fn a_mute_request_that_asks_for_the_applied_value_sends_nothing() {
            let mut state = AppState {
                muted: Some(true),
                ..AppState::default()
            };

            let transition = reduce(&mut state, AppEvent::MuteChangeRequested(true));

            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
        }

        /// The backend's reading is the authority, including when it differs
        /// from the requested value.
        #[test]
        fn the_backends_mute_reading_overrides_what_the_window_asked_for() {
            let mut state = AppState::default();

            reduce(&mut state, AppEvent::MuteChangeRequested(true));
            assert_eq!(state.muted, None);

            let transition = reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::MuteChanged { muted: false }),
            );

            assert!(transition.effects.is_empty());
            assert!(transition.snapshot_changed);
            assert_eq!(state.muted, Some(false));
        }

        #[test]
        fn a_selectable_latency_profile_waits_for_confirmation_and_is_sent_on() {
            let mut state = AppState {
                latency: Some(latency(LatencyChoice::Stable)),
                ..AppState::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::LatencyChoiceRequested(LatencyChoice::Normal),
            );

            assert_eq!(
                transition.effects,
                vec![AppEffect::ApplyLatency(LatencyChoice::Normal)]
            );
            assert_eq!(
                state.latency.as_ref().map(|reading| reading.selected),
                Some(LatencyChoice::Stable)
            );
            assert!(!transition.snapshot_changed);
        }

        /// The second lock on the door the window already keeps shut.
        ///
        /// The page draws a gated profile as a sentence rather than a control,
        /// so no press can normally reach here. A stale frame or a future
        /// caller must not be able to turn a profile the backend rejects into
        /// a command it has to reject again.
        #[test]
        fn a_gated_latency_profile_changes_nothing_and_sends_nothing() {
            let mut state = AppState {
                latency: Some(latency(LatencyChoice::Normal)),
                ..AppState::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::LatencyChoiceRequested(LatencyChoice::Low),
            );

            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(
                state.latency.as_ref().map(|reading| reading.selected),
                Some(LatencyChoice::Normal)
            );
        }

        #[test]
        fn a_latency_choice_before_anything_reported_sends_nothing() {
            let mut state = AppState::default();

            let transition = reduce(
                &mut state,
                AppEvent::LatencyChoiceRequested(LatencyChoice::Normal),
            );

            assert!(transition.effects.is_empty());
            assert_eq!(state.latency, None);
        }

        #[test]
        fn choosing_an_offered_capture_endpoint_waits_for_confirmation_and_is_sent_on() {
            let mut state = AppState {
                audio_source: Some(capture()),
                ..AppState::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(
                    1,
                ))),
            );

            assert_eq!(
                transition.effects,
                vec![AppEffect::ApplyAudioEndpoint(
                    AudioEndpointRequest::Endpoint(AudioEndpointKey(1))
                )]
            );
            assert_eq!(
                state
                    .audio_source
                    .as_ref()
                    .map(|reading| &reading.selection),
                Some(&AudioEndpointSelection::SystemDefault)
            );
            assert!(!transition.snapshot_changed);
        }

        /// A key that is no longer on offer selects nothing.
        ///
        /// Refused here rather than in the bridge, because the offered list is
        /// in hand here. The bridge's directory keeps every key it ever handed
        /// out, so this is what stops a stale press from re-selecting a device
        /// the backend no longer lists.
        #[test]
        fn a_capture_key_that_is_no_longer_offered_changes_nothing() {
            let mut state = AppState {
                audio_source: Some(capture()),
                ..AppState::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(
                    99,
                ))),
            );

            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(
                state
                    .audio_source
                    .as_ref()
                    .map(|reading| &reading.selection),
                Some(&AudioEndpointSelection::SystemDefault)
            );
        }

        #[test]
        fn the_backends_capture_reading_replaces_whatever_the_window_holds() {
            let mut state = AppState {
                audio_source: Some(capture()),
                ..AppState::default()
            };
            let mut newer = capture();
            newer.state = CaptureState::Recovering;
            newer.captured_name = None;

            let transition = reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::AudioSourceChanged {
                    reading: newer.clone(),
                }),
            );

            assert!(transition.effects.is_empty());
            assert!(transition.snapshot_changed);
            assert_eq!(state.audio_source, Some(newer));
        }
    }

    /// The registry's reading lands in state, and an unchanged one is not
    /// news.
    ///
    /// The second half is the load-bearing one. The bridge polls the registry
    /// and offers whatever it finds; the registry republishes on every
    /// diagnostic event it ingests, most of which change nothing this reading
    /// carries. A reducer that raised the revision anyway would repaint the
    /// whole window for every backend log line.
    #[test]
    fn a_diagnostics_reading_lands_in_state_and_an_unchanged_one_is_not_news() {
        use super::super::{DiagnosticsHealth, DiagnosticsReading};

        let mut state = AppState::default();
        assert_eq!(
            state.diagnostics, None,
            "before the registry reports, the shell knows nothing -- not zero"
        );
        let reading = DiagnosticsReading {
            health: DiagnosticsHealth::Attention,
            events_dropped_total: 4,
            measured_receivers: 2,
        };

        let first = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiagnosticsUpdated { reading }),
        );

        assert_eq!(state.diagnostics, Some(reading));
        assert!(first.snapshot_changed);
        assert!(first.effects.is_empty(), "a reading commands nothing");
        let revision = state.revision;

        let again = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiagnosticsUpdated { reading }),
        );

        assert!(!again.snapshot_changed, "the same reading is not news");
        assert_eq!(state.revision, revision);
    }

    /// The assurance every capability added after this one rests on.
    ///
    /// A wildcard arm in `reduce` turns an unhandled event into a silent
    /// no-op: no compile error, no failing test, and a window that simply
    /// stops moving. `ControllerEvent::DiscoveryFailed` lived in exactly that
    /// arm -- produced by both the legacy service and the backend bridge,
    /// discarded on arrival, and invisible to the whole suite.
    ///
    /// Deliberate non-handling is still allowed and there are two of them,
    /// but each has to name its variants and say why. That is the difference
    /// between a decision and an accident, and it is what makes the next
    /// event someone adds a compile error rather than a frozen interface.
    ///
    /// The check is on the *shape of the pattern*, not on the character `_`.
    /// `unhandled => (Vec::new(), false)` is an irrefutable pattern just like
    /// `_`, matches every future variant just as silently, and reads to a
    /// reviewer as a harmless naming choice. Every arm therefore has to open
    /// with `AppEvent::`; a bare binding of any name is the violation.
    #[test]
    fn the_event_dispatch_has_no_wildcard_arm() {
        /// The column rustfmt puts an arm's pattern in. Continuation lines of
        /// a multi-line pattern sit deeper, and the lines that close one
        /// (`}) => ...`, `) => ...`) open with punctuation, so an arm start
        /// is exactly a line at this indent beginning with a name character.
        const ARM_INDENT: usize = 8;

        const SOURCE: &str = include_str!("reducer.rs");

        let dispatch = SOURCE
            .split_once("let (effects, snapshot_changed) = match event {")
            .expect("`reduce` still dispatches on the event")
            .1
            .split_once("\n    };")
            .expect("the dispatch still closes at the statement's own indentation")
            .0;

        let arm_starts = dispatch
            .lines()
            .filter(|line| {
                let pattern = line.trim_start();
                !pattern.is_empty()
                    && line.len() - pattern.len() == ARM_INDENT
                    && pattern
                        .starts_with(|first: char| first.is_ascii_alphanumeric() || first == '_')
            })
            .map(str::trim_start)
            .collect::<Vec<_>>();

        assert!(
            !arm_starts.is_empty(),
            "no arm was recognised at all, so this test is measuring nothing; \
             the dispatch's formatting must have moved"
        );
        assert_eq!(
            arm_starts
                .iter()
                .find(|pattern| !pattern.starts_with("AppEvent::")),
            None,
            "an arm whose pattern is a bare binding matches every event the \
             dispatch does not name, present and future, without a compile \
             error; name the variants and give them a reason instead"
        );
    }

    #[test]
    fn session_start_enters_starting_with_sorted_available_targets_and_no_active_membership() {
        let mut state = AppState {
            revision: 7,
            desired_receivers: ids(&[3, 1, 2]),
            staged_receivers: ids(&[3, 1, 2]),
            active_receivers: ids(&[9]),
            receivers: vec![
                receiver(3, "Office"),
                receiver_record(1, "Kitchen", "HomePod", Availability::Unavailable),
                receiver(2, "Living room"),
            ],
            master_volume: 1.4,
            generations: super::super::Generations {
                session: GenerationId(12),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::StartRequested);

        assert_eq!(state.generations.session, GenerationId(13));
        assert_eq!(
            state.stream,
            StreamState::Starting {
                generation: GenerationId(13)
            }
        );
        assert!(state.active_receivers.is_empty());
        assert_eq!(
            transition.effects,
            vec![AppEffect::StartSession {
                generation: GenerationId(13),
                receiver_ids: vec![id(2), id(3)],
                volume: 1.0,
            }]
        );
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 8);
    }

    #[test]
    fn session_matching_started_streams_only_confirmed_active_members() {
        let mut state = AppState {
            revision: 4,
            desired_receivers: ids(&[1, 2]),
            staged_receivers: ids(&[1, 2]),
            active_receivers: ids(&[9]),
            stream: StreamState::Starting {
                generation: GenerationId(13),
            },
            generations: super::super::Generations {
                session: GenerationId(13),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation: GenerationId(13),
                active_receiver_ids: vec![id(2), id(3), id(2)],
            }),
        );

        assert_eq!(
            state.stream,
            StreamState::Streaming {
                generation: GenerationId(13)
            }
        );
        assert_eq!(state.active_receivers, ids(&[2, 3]));
        assert_eq!(state.desired_receivers, ids(&[1, 2]));
        assert_eq!(state.staged_receivers, ids(&[1, 2]));
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 5);
    }

    #[test]
    fn session_recovery_retracts_only_its_failure_notice() {
        for degraded in [false, true] {
            let mut state = AppState {
                desired_receivers: ids(&[1]),
                receivers: vec![receiver(1, "Kitchen")],
                ..Default::default()
            };
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::SessionFailed {
                    generation: GenerationId(0),
                    summary: "connection failed".into(),
                }),
            );
            reduce(&mut state, AppEvent::StartRequested);
            let failed_notice = state.notice.clone();
            let recovered = |generation| {
                if degraded {
                    ControllerEvent::SessionDegraded {
                        generation,
                        active_receiver_ids: vec![id(1)],
                    }
                } else {
                    ControllerEvent::SessionStarted {
                        generation,
                        active_receiver_ids: vec![id(1)],
                    }
                }
            };
            let stale = reduce(&mut state, AppEvent::Controller(recovered(GenerationId(0))));
            assert!(!stale.snapshot_changed);
            assert_eq!(state.notice, failed_notice);

            let accepted = reduce(&mut state, AppEvent::Controller(recovered(GenerationId(1))));
            assert!(accepted.snapshot_changed);
            assert!(accepted.effects.is_empty());
            assert_eq!(state.notice, None, "successful retry retires its error");
            assert_eq!(state.active_receivers, ids(&[1]));

            let unrelated = UserNotice {
                severity: Severity::Warning,
                code: NoticeCode::PreferencesFailed,
                summary: "settings were not saved".into(),
                action: Some(CorrectiveAction::RetryPreferencesPersistence),
            };
            state.notice = Some(unrelated.clone());
            let repeat = reduce(&mut state, AppEvent::Controller(recovered(GenerationId(1))));
            assert!(!repeat.snapshot_changed);
            assert!(repeat.effects.is_empty());
            assert_eq!(state.notice, Some(unrelated));
        }
    }

    #[test]
    fn session_stale_controller_results_are_complete_noops_without_revisions() {
        let state = AppState {
            revision: 9,
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1]),
            active_receivers: ids(&[1]),
            stream: StreamState::Stopping {
                generation: GenerationId(20),
            },
            generations: super::super::Generations {
                session: GenerationId(20),
                ..Default::default()
            },
            ..Default::default()
        };

        for event in [
            ControllerEvent::SessionStarted {
                generation: GenerationId(19),
                active_receiver_ids: vec![id(2)],
            },
            ControllerEvent::SessionFailed {
                generation: GenerationId(19),
                summary: "late failure".into(),
            },
            ControllerEvent::SessionStopped {
                generation: GenerationId(19),
            },
        ] {
            let mut current = state.clone();

            let transition = reduce(&mut current, AppEvent::Controller(event));

            assert_eq!(current, state);
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(current.revision, 9);
        }
    }

    #[test]
    fn session_matching_failure_stops_clears_active_and_preserves_membership_with_retry_notice() {
        let mut state = AppState {
            revision: 3,
            desired_revision: 7,
            desired_receivers: ids(&[1, 2]),
            staged_receivers: ids(&[1, 3]),
            staged_base_revision: 6,
            active_receivers: ids(&[1]),
            stream: StreamState::Starting {
                generation: GenerationId(13),
            },
            generations: super::super::Generations {
                session: GenerationId(13),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionFailed {
                generation: GenerationId(13),
                summary: "receiver rejected the connection".into(),
            }),
        );

        assert_eq!(state.stream, StreamState::Stopped);
        assert!(state.active_receivers.is_empty());
        assert_eq!(state.desired_receivers, ids(&[1, 2]));
        assert_eq!(state.staged_receivers, ids(&[1, 3]));
        assert_eq!(state.staged_base_revision, 6);
        assert_eq!(
            state.notice,
            Some(super::super::UserNotice {
                severity: super::super::Severity::Error,
                code: super::super::NoticeCode::SessionFailed,
                summary: "receiver rejected the connection".into(),
                action: Some(super::super::CorrectiveAction::Retry),
            })
        );
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 4);
    }

    #[test]
    fn session_start_is_a_complete_noop_when_shutting_down_or_without_available_target() {
        for state in [
            AppState {
                revision: 5,
                shutting_down: true,
                desired_receivers: ids(&[1]),
                receivers: vec![receiver(1, "Kitchen")],
                generations: super::super::Generations {
                    session: GenerationId(13),
                    ..Default::default()
                },
                ..Default::default()
            },
            AppState {
                revision: 5,
                desired_receivers: ids(&[1]),
                receivers: vec![receiver_record(
                    1,
                    "Kitchen",
                    "HomePod",
                    Availability::Unavailable,
                )],
                generations: super::super::Generations {
                    session: GenerationId(13),
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            let mut current = state.clone();

            let transition = reduce(&mut current, AppEvent::StartRequested);

            assert_eq!(current, state);
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
        }
    }

    #[test]
    fn session_stop_enters_stopping_clears_active_and_preserves_membership() {
        let mut state = AppState {
            revision: 3,
            desired_revision: 7,
            desired_receivers: ids(&[1, 2]),
            staged_receivers: ids(&[1, 3]),
            staged_base_revision: 6,
            active_receivers: ids(&[1, 2]),
            stream: StreamState::Streaming {
                generation: GenerationId(13),
            },
            generations: super::super::Generations {
                session: GenerationId(13),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::StopRequested);

        assert_eq!(state.generations.session, GenerationId(14));
        assert_eq!(
            state.stream,
            StreamState::Stopping {
                generation: GenerationId(14)
            }
        );
        assert!(state.active_receivers.is_empty());
        assert_eq!(state.desired_receivers, ids(&[1, 2]));
        assert_eq!(state.staged_receivers, ids(&[1, 3]));
        assert_eq!(state.staged_base_revision, 6);
        assert_eq!(
            transition.effects,
            vec![AppEffect::StopSession {
                generation: GenerationId(14)
            }]
        );
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 4);
    }

    /// The two phases the resilience backend added are running sessions, and
    /// the Stop press has to reach the backend from both of them.
    ///
    /// `StreamState::is_running` is what decides this, and until this test
    /// existed nothing said what it must answer for the new phases. Answering
    /// `false` compiles, passes, and turns Stop into a no-op under a button
    /// the window still draws as live -- the user presses it, the audio keeps
    /// playing, and nothing anywhere reports a fault.
    #[test]
    fn a_stop_press_ends_the_session_from_every_running_phase() {
        for (name, stream) in [
            (
                "degraded",
                StreamState::Degraded {
                    generation: GenerationId(13),
                },
            ),
            (
                "restarting",
                StreamState::Restarting {
                    generation: GenerationId(13),
                },
            ),
        ] {
            let mut state = AppState {
                desired_receivers: ids(&[1, 2]),
                staged_receivers: ids(&[1, 2]),
                active_receivers: ids(&[1]),
                stream,
                generations: super::super::Generations {
                    session: GenerationId(13),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::StopRequested);

            assert_eq!(
                transition.effects,
                vec![AppEffect::StopSession {
                    generation: GenerationId(14)
                }],
                "{name}: the press did not reach the backend"
            );
            assert_eq!(
                state.stream,
                StreamState::Stopping {
                    generation: GenerationId(14)
                },
                "{name}"
            );
            assert!(state.active_receivers.is_empty(), "{name}");
            assert!(transition.snapshot_changed, "{name}");
        }
    }

    /// Quitting during a self-healing restart still tears the session down.
    ///
    /// The same `is_running` answer decides this one. Getting it wrong here
    /// exits the process while the backend session keeps running -- the
    /// unbounded teardown, with no window left to notice it.
    #[test]
    fn quitting_during_a_restart_still_stops_the_session() {
        let mut state = AppState {
            active_receivers: ids(&[1]),
            stream: StreamState::Restarting {
                generation: GenerationId(4),
            },
            generations: super::super::Generations {
                session: GenerationId(4),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::QuitRequested);

        assert!(
            transition.effects.contains(&AppEffect::StopSession {
                generation: GenerationId(5),
            }),
            "quitting left the backend session running: {:?}",
            transition.effects
        );
        assert_eq!(
            state.stream,
            StreamState::Stopping {
                generation: GenerationId(5)
            }
        );
        assert!(state.active_receivers.is_empty());
    }

    #[test]
    fn session_matching_stopped_enters_stopped_without_membership_changes() {
        let mut state = AppState {
            revision: 3,
            desired_receivers: ids(&[1, 2]),
            staged_receivers: ids(&[1, 3]),
            staged_base_revision: 6,
            stream: StreamState::Stopping {
                generation: GenerationId(14),
            },
            generations: super::super::Generations {
                session: GenerationId(14),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: GenerationId(14),
            }),
        );

        assert_eq!(state.stream, StreamState::Stopped);
        assert!(state.active_receivers.is_empty());
        assert_eq!(state.desired_receivers, ids(&[1, 2]));
        assert_eq!(state.staged_receivers, ids(&[1, 3]));
        assert_eq!(state.staged_base_revision, 6);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 4);
    }

    #[test]
    fn session_stop_is_idempotent_when_already_stopped_or_stopping() {
        for state in [
            AppState {
                revision: 5,
                stream: StreamState::Stopped,
                generations: super::super::Generations {
                    session: GenerationId(13),
                    ..Default::default()
                },
                ..Default::default()
            },
            AppState {
                revision: 5,
                stream: StreamState::Stopping {
                    generation: GenerationId(13),
                },
                generations: super::super::Generations {
                    session: GenerationId(13),
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            let mut current = state.clone();

            let transition = reduce(&mut current, AppEvent::StopRequested);

            assert_eq!(current, state);
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
        }
    }

    #[test]
    fn staged_toggle_changes_only_the_stage_and_emits_nothing_while_streaming() {
        let kitchen = id(1);
        let mut state = AppState {
            desired_receivers: ids(&[2]),
            staged_receivers: ids(&[2]),
            active_receivers: ids(&[2]),
            stream: StreamState::Streaming {
                generation: GenerationId(4),
            },
            generations: super::super::Generations {
                session: GenerationId(4),
                ..Default::default()
            },
            ..Default::default()
        };
        let before_desired = state.desired_receivers.clone();
        let before_active = state.active_receivers.clone();
        let before_stream = state.stream.clone();

        let transition = reduce(&mut state, AppEvent::ToggleStagedReceiver(kitchen.clone()));

        assert!(state.staged_receivers.contains(&kitchen));
        assert_eq!(state.desired_receivers, before_desired);
        assert_eq!(state.active_receivers, before_active);
        assert_eq!(state.stream, before_stream);
        assert_eq!(state.generations.session, GenerationId(4));
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_apply_commits_complete_set_once_and_emits_one_sorted_replacement() {
        let mut state = AppState {
            desired_revision: 8,
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[3, 2]),
            staged_base_revision: 8,
            active_receivers: ids(&[1]),
            receivers: vec![receiver(3, "Office"), receiver(2, "Kitchen")],
            stream: StreamState::Streaming {
                generation: GenerationId(20),
            },
            generations: super::super::Generations {
                session: GenerationId(20),
                ..Default::default()
            },
            master_volume: 0.4,
            ..Default::default()
        };
        let before_active = state.active_receivers.clone();
        let before_stream = state.stream.clone();

        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

        assert_eq!(state.desired_receivers, ids(&[2, 3]));
        assert_eq!(state.desired_revision, 9);
        assert_eq!(state.staged_base_revision, 9);
        assert_eq!(state.generations.session, GenerationId(21));
        assert_eq!(state.active_receivers, before_active);
        assert_eq!(state.stream, before_stream);
        assert_eq!(
            transition.effects,
            vec![AppEffect::StartSession {
                generation: GenerationId(21),
                receiver_ids: vec![id(2), id(3)],
                volume: 0.4,
            }]
        );
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_apply_while_stopped_commits_without_starting_playback() {
        let mut state = AppState {
            desired_revision: 3,
            staged_receivers: ids(&[1]),
            receivers: vec![receiver(1, "Kitchen")],
            generations: super::super::Generations {
                session: GenerationId(7),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

        assert_eq!(state.desired_receivers, ids(&[1]));
        assert_eq!(state.desired_revision, 4);
        assert_eq!(state.staged_base_revision, 4);
        assert_eq!(state.generations.session, GenerationId(7));
        assert_eq!(state.stream, StreamState::Stopped);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_apply_empty_while_starting_emits_one_new_stop_generation() {
        let mut state = AppState {
            desired_revision: 5,
            desired_receivers: ids(&[1]),
            staged_receivers: HashSet::new(),
            stream: StreamState::Starting {
                generation: GenerationId(11),
            },
            generations: super::super::Generations {
                session: GenerationId(11),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

        assert!(state.desired_receivers.is_empty());
        assert_eq!(state.desired_revision, 6);
        assert_eq!(state.staged_base_revision, 6);
        assert_eq!(state.generations.session, GenerationId(12));
        assert_eq!(
            transition.effects,
            vec![AppEffect::StopSession {
                generation: GenerationId(12),
            }]
        );
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_discard_loads_latest_desired_set_without_controller_effect() {
        let mut state = AppState {
            revision: 10,
            desired_revision: 6,
            desired_receivers: ids(&[2, 3]),
            staged_receivers: ids(&[1]),
            staged_base_revision: 4,
            stream: StreamState::Streaming {
                generation: GenerationId(9),
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::DiscardStagedReceivers);

        assert_eq!(state.staged_receivers, ids(&[2, 3]));
        assert_eq!(state.staged_base_revision, 6);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 11);
    }

    #[test]
    fn staged_clean_discard_pruning_inventory_publishes_exactly_one_revision() {
        let mut state = AppState {
            revision: 10,
            desired_revision: 6,
            receivers: vec![
                receiver(1, "Kitchen"),
                receiver_record(2, "Old Office", "HomePod mini", Availability::Unavailable),
            ],
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1]),
            staged_base_revision: 6,
            active_receivers: ids(&[1]),
            stream: StreamState::Streaming {
                generation: GenerationId(9),
            },
            generations: super::super::Generations {
                session: GenerationId(9),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut expected = state.clone();
        expected.revision = 11;
        expected.receivers = vec![receiver(1, "Kitchen")];

        let transition = reduce(&mut state, AppEvent::DiscardStagedReceivers);

        assert_eq!(state, expected);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 11);
    }

    #[test]
    fn staged_genuinely_clean_discard_is_a_complete_noop() {
        let mut state = AppState {
            revision: 10,
            desired_revision: 6,
            receivers: vec![receiver(1, "Kitchen")],
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1]),
            staged_base_revision: 6,
            active_receivers: ids(&[1]),
            stream: StreamState::Streaming {
                generation: GenerationId(9),
            },
            generations: super::super::Generations {
                session: GenerationId(9),
                ..Default::default()
            },
            ..Default::default()
        };
        let before = state.clone();

        let transition = reduce(&mut state, AppEvent::DiscardStagedReceivers);

        assert_eq!(state, before);
        assert!(transition.effects.is_empty());
        assert!(!transition.snapshot_changed);
        assert_eq!(state.revision, 10);
    }

    #[test]
    fn staged_running_apply_with_only_unavailable_targets_emits_nothing() {
        for (case, stream) in [
            (
                "starting",
                StreamState::Starting {
                    generation: GenerationId(10),
                },
            ),
            (
                "streaming",
                StreamState::Streaming {
                    generation: GenerationId(10),
                },
            ),
        ] {
            let mut state = AppState {
                desired_revision: 5,
                receivers: vec![receiver_record(
                    1,
                    "Missing Kitchen",
                    "HomePod",
                    Availability::Unavailable,
                )],
                desired_receivers: ids(&[1]),
                staged_receivers: ids(&[1]),
                staged_base_revision: 5,
                stream: stream.clone(),
                generations: super::super::Generations {
                    session: GenerationId(10),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

            assert!(transition.effects.is_empty(), "{case}");
            assert_eq!(state.generations.session, GenerationId(10), "{case}");
            assert_eq!(state.stream, stream, "{case}");
            assert_eq!(state.desired_receivers, ids(&[1]), "{case}");
            assert_eq!(state.desired_revision, 6, "{case}");
            assert_eq!(state.staged_base_revision, 6, "{case}");
            assert!(transition.snapshot_changed, "{case}");
            assert_eq!(state.revision, 1, "{case}");
        }
    }

    #[test]
    fn staged_same_set_apply_reconciles_both_running_states() {
        for (case, stream) in [
            (
                "starting",
                StreamState::Starting {
                    generation: GenerationId(10),
                },
            ),
            (
                "streaming",
                StreamState::Streaming {
                    generation: GenerationId(10),
                },
            ),
        ] {
            let mut state = AppState {
                desired_revision: 5,
                receivers: vec![receiver(1, "Kitchen")],
                desired_receivers: ids(&[1]),
                staged_receivers: ids(&[1]),
                staged_base_revision: 5,
                stream: stream.clone(),
                generations: super::super::Generations {
                    session: GenerationId(10),
                    ..Default::default()
                },
                master_volume: 0.6,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

            assert_eq!(state.desired_receivers, ids(&[1]), "{case}");
            assert_eq!(state.desired_revision, 6, "{case}");
            assert_eq!(state.staged_base_revision, 6, "{case}");
            assert_eq!(state.generations.session, GenerationId(11), "{case}");
            assert_eq!(state.stream, stream, "{case}");
            assert_eq!(
                transition.effects,
                vec![AppEffect::StartSession {
                    generation: GenerationId(11),
                    receiver_ids: vec![id(1)],
                    volume: 0.6,
                }],
                "{case}"
            );
            assert!(transition.snapshot_changed, "{case}");
            assert_eq!(state.revision, 1, "{case}");
        }
    }

    #[test]
    fn staged_empty_apply_stops_both_running_states() {
        for (case, stream) in [
            (
                "starting",
                StreamState::Starting {
                    generation: GenerationId(10),
                },
            ),
            (
                "streaming",
                StreamState::Streaming {
                    generation: GenerationId(10),
                },
            ),
        ] {
            let mut state = AppState {
                desired_revision: 5,
                staged_base_revision: 5,
                stream: stream.clone(),
                generations: super::super::Generations {
                    session: GenerationId(10),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

            assert!(state.desired_receivers.is_empty(), "{case}");
            assert_eq!(state.desired_revision, 6, "{case}");
            assert_eq!(state.staged_base_revision, 6, "{case}");
            assert_eq!(state.generations.session, GenerationId(11), "{case}");
            assert_eq!(state.stream, stream, "{case}");
            assert_eq!(
                transition.effects,
                vec![AppEffect::StopSession {
                    generation: GenerationId(11),
                }],
                "{case}"
            );
            assert!(transition.snapshot_changed, "{case}");
            assert_eq!(state.revision, 1, "{case}");
        }
    }

    #[test]
    fn staged_clean_selection_refreshes_from_newer_desired_result() {
        let mut state = AppState {
            desired_revision: 4,
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1]),
            staged_base_revision: 4,
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DesiredReceiversChanged {
                desired_revision: 7,
                receiver_ids: vec![id(3), id(2)],
            }),
        );

        assert_eq!(state.desired_receivers, ids(&[2, 3]));
        assert_eq!(state.staged_receivers, ids(&[2, 3]));
        assert_eq!(state.desired_revision, 7);
        assert_eq!(state.staged_base_revision, 7);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_dirty_selection_survives_newer_desired_result_and_becomes_stale() {
        let mut state = AppState {
            desired_revision: 4,
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1, 2]),
            staged_base_revision: 4,
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DesiredReceiversChanged {
                desired_revision: 7,
                receiver_ids: vec![id(3)],
            }),
        );

        assert_eq!(state.desired_receivers, ids(&[3]));
        assert_eq!(state.staged_receivers, ids(&[1, 2]));
        assert_eq!(state.desired_revision, 7);
        assert_eq!(state.staged_base_revision, 4);
        assert!(state.staged_base_revision < state.desired_revision);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_apply_stale_selection_deliberately_replaces_complete_desired_set() {
        let mut state = AppState {
            desired_revision: 7,
            desired_receivers: ids(&[3]),
            staged_receivers: ids(&[1, 2]),
            staged_base_revision: 4,
            receivers: vec![receiver(1, "Kitchen"), receiver(2, "Office")],
            stream: StreamState::Streaming {
                generation: GenerationId(15),
            },
            generations: super::super::Generations {
                session: GenerationId(15),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

        assert_eq!(state.desired_receivers, ids(&[1, 2]));
        assert_eq!(state.desired_revision, 8);
        assert_eq!(state.staged_base_revision, 8);
        assert_eq!(transition.effects.len(), 1);
        assert!(matches!(
            &transition.effects[0],
            AppEffect::StartSession {
                generation: GenerationId(16),
                receiver_ids,
                ..
            } if receiver_ids.as_slice() == [id(1), id(2)]
        ));
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn staged_older_desired_result_is_ignored_without_revision() {
        let mut state = AppState {
            revision: 5,
            desired_revision: 8,
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1]),
            staged_base_revision: 8,
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DesiredReceiversChanged {
                desired_revision: 7,
                receiver_ids: vec![id(2)],
            }),
        );

        assert_eq!(state.desired_receivers, ids(&[1]));
        assert_eq!(state.staged_receivers, ids(&[1]));
        assert_eq!(state.desired_revision, 8);
        assert!(transition.effects.is_empty());
        assert!(!transition.snapshot_changed);
        assert_eq!(state.revision, 5);
    }

    #[test]
    fn discovery_merge_updates_by_id_retains_relevant_missing_and_removes_irrelevant() {
        let mut state = AppState {
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(5),
            },
            receivers: vec![
                receiver(1, "Old Kitchen"),
                receiver(2, "Office"),
                receiver(3, "Guest Room"),
            ],
            desired_receivers: ids(&[1]),
            staged_receivers: ids(&[1, 2]),
            generations: super::super::Generations {
                discovery: GenerationId(5),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(5),
                receivers: vec![receiver(1, "Renamed Kitchen"), receiver(4, "Living Room")],
            }),
        );

        assert_eq!(state.discovery, super::super::DiscoveryState::Ready);
        assert_eq!(state.receivers.len(), 3);
        assert_eq!(
            state
                .receivers
                .iter()
                .find(|receiver| receiver.id == id(1))
                .map(|receiver| (&receiver.name, receiver.availability)),
            Some((&"Renamed Kitchen".to_string(), Availability::Available))
        );
        assert_eq!(
            state
                .receivers
                .iter()
                .find(|receiver| receiver.id == id(2))
                .map(|receiver| receiver.availability),
            Some(Availability::Unavailable)
        );
        assert!(state.receivers.iter().any(|receiver| receiver.id == id(4)));
        assert!(!state.receivers.iter().any(|receiver| receiver.id == id(3)));
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn discovery_missing_desired_stays_unavailable_and_never_enters_start_targets() {
        let mut state = AppState {
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(2),
            },
            desired_revision: 3,
            receivers: vec![receiver(1, "Kitchen"), receiver(2, "Office")],
            desired_receivers: ids(&[1, 2]),
            staged_receivers: ids(&[1, 2]),
            staged_base_revision: 3,
            stream: StreamState::Streaming {
                generation: GenerationId(9),
            },
            generations: super::super::Generations {
                discovery: GenerationId(2),
                session: GenerationId(9),
                ..Default::default()
            },
            ..Default::default()
        };

        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(2),
                receivers: vec![receiver(1, "Kitchen")],
            }),
        );
        let transition = reduce(&mut state, AppEvent::ApplyStagedReceivers);

        assert_eq!(
            state
                .receivers
                .iter()
                .find(|receiver| receiver.id == id(2))
                .map(|receiver| receiver.availability),
            Some(Availability::Unavailable)
        );
        assert_eq!(
            transition.effects,
            vec![AppEffect::StartSession {
                generation: GenerationId(10),
                receiver_ids: vec![id(1)],
                volume: 0.25,
            }]
        );
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn discovery_nonmatching_generation_is_ignored_without_revision() {
        let mut state = AppState {
            revision: 12,
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(6),
            },
            receivers: vec![receiver(1, "Kitchen")],
            generations: super::super::Generations {
                discovery: GenerationId(6),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(5),
                receivers: vec![receiver(2, "Office")],
            }),
        );

        assert_eq!(state.receivers, vec![receiver(1, "Kitchen")]);
        assert_eq!(
            state.discovery,
            super::super::DiscoveryState::Discovering {
                generation: GenerationId(6),
            }
        );
        assert!(transition.effects.is_empty());
        assert!(!transition.snapshot_changed);
        assert_eq!(state.revision, 12);
    }

    /// A browse that succeeds withdraws the accusation a browse that failed
    /// left on screen.
    ///
    /// `DiscoveryPhase::Retrying` is the daemon's normal backoff, so without
    /// this the first transient stumble parks a warning in the window for the
    /// rest of the session while the search runs fine -- the false alarm that
    /// teaches the user to ignore the real one.
    #[test]
    fn a_search_that_succeeds_withdraws_the_warning_a_failed_search_left() {
        let mut state = AppState {
            generations: super::super::Generations {
                discovery: GenerationId(6),
                ..Default::default()
            },
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(6),
            },
            ..Default::default()
        };

        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryFailed {
                generation: GenerationId(6),
                summary: "the daemon fell over".into(),
            }),
        );
        assert_eq!(
            state.notice.as_ref().map(|notice| notice.code),
            Some(NoticeCode::DiscoveryFailed),
            "the failure has to be reported before its withdrawal means anything"
        );

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(6),
                receivers: vec![receiver(1, "Kitchen")],
            }),
        );

        assert_eq!(
            state.notice, None,
            "the daemon is browsing again, yet the window still accuses it"
        );
        assert_eq!(state.discovery, super::super::DiscoveryState::Ready);
        assert!(
            transition.snapshot_changed,
            "withdrawing a notice changes what the window shows"
        );
    }

    /// The withdrawal is scoped to the code it owns.
    ///
    /// A session that failed and a settings file that could not be written
    /// are unrelated to whether the search works; clearing them because a
    /// browse came back would drop the only report the user ever gets of
    /// them.
    #[test]
    fn a_search_that_succeeds_leaves_every_other_warning_standing() {
        for standing in [
            UserNotice {
                severity: Severity::Error,
                code: NoticeCode::SessionFailed,
                summary: "the session fell over".into(),
                action: Some(CorrectiveAction::Retry),
            },
            UserNotice {
                severity: Severity::Warning,
                code: NoticeCode::PreferencesFailed,
                summary: super::PREFERENCES_PERSIST_FAILED_SUMMARY.to_string(),
                action: Some(CorrectiveAction::RetryPreferencesPersistence),
            },
        ] {
            let mut state = AppState {
                notice: Some(standing.clone()),
                generations: super::super::Generations {
                    discovery: GenerationId(2),
                    ..Default::default()
                },
                discovery: super::super::DiscoveryState::Discovering {
                    generation: GenerationId(2),
                },
                ..Default::default()
            };

            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                    generation: GenerationId(2),
                    receivers: vec![receiver(1, "Kitchen")],
                }),
            );

            assert_eq!(
                state.notice,
                Some(standing.clone()),
                "a completed browse says nothing about {:?}",
                standing.code
            );
        }
    }

    #[test]
    fn discovery_reordered_same_id_inventory_is_a_semantic_noop() {
        let mut state = AppState {
            revision: 12,
            discovery: super::super::DiscoveryState::Ready,
            receivers: vec![receiver(1, "Kitchen"), receiver(2, "Office")],
            generations: super::super::Generations {
                discovery: GenerationId(6),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(6),
                receivers: vec![receiver(2, "Office"), receiver(1, "Kitchen")],
            }),
        );

        assert_eq!(
            state.receivers,
            vec![receiver(1, "Kitchen"), receiver(2, "Office")]
        );
        assert!(transition.effects.is_empty());
        assert!(!transition.snapshot_changed);
        assert_eq!(state.revision, 12);
    }

    #[test]
    fn discovery_duplicate_ids_collapse_to_one_record() {
        let duplicate = receiver(1, "Kitchen");
        let mut state = AppState {
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(6),
            },
            generations: super::super::Generations {
                discovery: GenerationId(6),
                ..Default::default()
            },
            ..Default::default()
        };

        let transition = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(6),
                receivers: vec![duplicate.clone(), duplicate.clone()],
            }),
        );

        assert_eq!(state.receivers, vec![duplicate]);
        assert!(transition.effects.is_empty());
        assert!(transition.snapshot_changed);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn discovery_conflicting_duplicates_choose_order_independent_canonical_winner() {
        let unavailable_alpha = receiver_record(1, "Alpha", "HomePod", Availability::Unavailable);
        let available_zeta = receiver_record(1, "Zeta", "HomePod", Availability::Available);
        let available_alpha_mini =
            receiver_record(1, "Alpha", "HomePod mini", Availability::Available);
        let available_alpha = receiver_record(1, "Alpha", "HomePod", Availability::Available);
        let records = vec![
            unavailable_alpha,
            available_zeta,
            available_alpha_mini,
            available_alpha.clone(),
        ];

        let mut forward = AppState {
            discovery: super::super::DiscoveryState::Discovering {
                generation: GenerationId(6),
            },
            generations: super::super::Generations {
                discovery: GenerationId(6),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut reverse = forward.clone();

        let forward_transition = reduce(
            &mut forward,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(6),
                receivers: records.clone(),
            }),
        );
        let reverse_transition = reduce(
            &mut reverse,
            AppEvent::Controller(ControllerEvent::DiscoveryCompleted {
                generation: GenerationId(6),
                receivers: records.into_iter().rev().collect(),
            }),
        );

        assert_eq!(forward.receivers, vec![available_alpha]);
        assert_eq!(reverse, forward);
        assert_eq!(reverse_transition, forward_transition);
        assert!(forward_transition.effects.is_empty());
        assert!(forward_transition.snapshot_changed);
        assert_eq!(forward.revision, 1);
    }

    mod shell {
        use super::*;
        use crate::app::{
            CorrectiveAction, DiscoveryState, Generations, HotkeyBinding, NoticeCode, Page,
            Preferences, PreferencesEvent, Severity, ThemePreference, UserNotice, WindowGeometry,
            MOD_ALT, MOD_CONTROL,
        };

        fn binding(enabled: bool, virtual_key: u32) -> HotkeyBinding {
            HotkeyBinding {
                enabled,
                modifiers: MOD_CONTROL | MOD_ALT,
                virtual_key,
            }
        }

        #[test]
        fn hotkey_press_during_running_sessions_delegates_to_stop() {
            for (case, stream) in [
                (
                    "starting",
                    StreamState::Starting {
                        generation: GenerationId(4),
                    },
                ),
                (
                    "streaming",
                    StreamState::Streaming {
                        generation: GenerationId(4),
                    },
                ),
            ] {
                let mut state = AppState {
                    desired_receivers: ids(&[1]),
                    staged_receivers: ids(&[1]),
                    active_receivers: ids(&[1]),
                    stream: stream.clone(),
                    generations: Generations {
                        session: GenerationId(4),
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let transition = reduce(&mut state, AppEvent::GlobalHotkeyPressed);

                assert_eq!(state.generations.session, GenerationId(5), "{case}");
                assert_eq!(
                    state.stream,
                    StreamState::Stopping {
                        generation: GenerationId(5)
                    },
                    "{case}"
                );
                assert_eq!(
                    transition.effects,
                    vec![AppEffect::StopSession {
                        generation: GenerationId(5)
                    }],
                    "{case}"
                );
                assert!(transition.snapshot_changed, "{case}");
                assert_eq!(state.desired_revision, 0, "{case}");
            }
        }

        #[test]
        fn hotkey_press_when_stopped_with_desired_targets_starts_without_membership_changes() {
            let mut state = AppState {
                desired_revision: 3,
                receivers: vec![receiver(1, "Kitchen"), receiver(2, "Office")],
                desired_receivers: ids(&[1, 2]),
                staged_receivers: ids(&[1, 2]),
                staged_base_revision: 3,
                generations: Generations {
                    session: GenerationId(9),
                    ..Default::default()
                },
                master_volume: 0.5,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::GlobalHotkeyPressed);

            assert_eq!(state.desired_receivers, ids(&[1, 2]));
            assert_eq!(state.staged_receivers, ids(&[1, 2]));
            assert_eq!(state.desired_revision, 3);
            assert_eq!(state.generations.session, GenerationId(10));
            assert_eq!(
                transition.effects,
                vec![AppEffect::StartSession {
                    generation: GenerationId(10),
                    receiver_ids: vec![id(1), id(2)],
                    volume: 0.5,
                }]
            );
            assert!(transition.snapshot_changed);
        }

        #[test]
        fn hotkey_press_without_desired_selects_first_available_in_display_order_then_starts() {
            let mut state = AppState {
                receivers: vec![
                    receiver(3, "zeta"),
                    receiver_record(5, "Alpha", "HomePod", Availability::Available),
                    receiver_record(1, "Muted Kitchen", "HomePod", Availability::Unavailable),
                ],
                generations: Generations {
                    session: GenerationId(2),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::GlobalHotkeyPressed);

            assert_eq!(state.desired_receivers, ids(&[5]));
            assert_eq!(state.staged_receivers, ids(&[5]));
            assert_eq!(state.desired_revision, 1);
            assert_eq!(state.staged_base_revision, 1);
            assert_eq!(state.generations.session, GenerationId(3));
            assert_eq!(
                transition.effects,
                vec![AppEffect::StartSession {
                    generation: GenerationId(3),
                    receiver_ids: vec![id(5)],
                    volume: 0.25,
                }]
            );
            assert!(transition.snapshot_changed);
        }

        #[test]
        fn hotkey_press_without_any_available_receiver_publishes_notice_without_effects() {
            for (case, mut state) in [
                ("empty inventory", AppState::default()),
                (
                    "desired unavailable",
                    AppState {
                        desired_receivers: ids(&[1]),
                        staged_receivers: ids(&[1]),
                        receivers: vec![receiver_record(
                            1,
                            "Kitchen",
                            "HomePod",
                            Availability::Unavailable,
                        )],
                        ..Default::default()
                    },
                ),
            ] {
                let transition = reduce(&mut state, AppEvent::GlobalHotkeyPressed);

                assert_eq!(state.stream, StreamState::Stopped, "{case}");
                assert_eq!(
                    state.notice,
                    Some(UserNotice {
                        severity: Severity::Warning,
                        code: NoticeCode::NoReceiverAvailable,
                        summary: "No AirPlay receiver is available right now.".into(),
                        action: Some(CorrectiveAction::Refresh),
                    }),
                    "{case}"
                );
                assert_eq!(state.desired_revision, 0, "{case}");
                assert!(transition.effects.is_empty(), "{case}");
                assert!(transition.snapshot_changed, "{case}");
            }
        }

        #[test]
        fn volume_finite_input_is_clamped_and_applied_immediately() {
            let mut state = AppState {
                master_volume: 0.25,
                generations: Generations {
                    volume: GenerationId(6),
                    ..Default::default()
                },
                ..Default::default()
            };

            let first = reduce(&mut state, AppEvent::MasterVolumeChanged(1.4));
            assert_eq!(state.master_volume, 1.0);
            assert_eq!(
                first.effects,
                vec![AppEffect::ApplyVolume {
                    generation: GenerationId(7),
                    volume: 1.0,
                }]
            );
            assert!(first.snapshot_changed);

            let second = reduce(&mut state, AppEvent::MasterVolumeChanged(-2.0));
            assert_eq!(state.master_volume, 0.0);
            assert_eq!(
                second.effects,
                vec![AppEffect::ApplyVolume {
                    generation: GenerationId(8),
                    volume: 0.0,
                }]
            );
            assert!(second.snapshot_changed);
            assert_eq!(state.revision, 2);
        }

        #[test]
        fn volume_echo_settles_latest_edit_and_ignores_stale_acknowledgements() {
            let mut state = AppState::default();
            reduce(&mut state, AppEvent::MasterVolumeChanged(0.4));
            reduce(&mut state, AppEvent::MasterVolumeChanged(0.6));
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::VolumeApplied {
                    generation: GenerationId(1),
                    volume: 0.4,
                }),
            );
            assert_eq!(
                state.master_volume, 0.6,
                "old acknowledgement cannot move the drag"
            );
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::VolumeApplied {
                    generation: GenerationId(2),
                    volume: 0.8,
                }),
            );
            assert_eq!(
                state.master_volume, 0.8,
                "failed latest write restores confirmed level"
            );
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::VolumeApplied {
                    generation: GenerationId(1),
                    volume: 0.4,
                }),
            );
            assert_eq!(state.master_volume, 0.8);
        }

        #[test]
        fn controller_closure_restores_confirmed_master_instead_of_unconfirmed_drag() {
            let mut state = AppState::default();
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::VolumeApplied {
                    generation: GenerationId(0),
                    volume: 0.8,
                }),
            );
            reduce(&mut state, AppEvent::MasterVolumeChanged(0.4));
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::ChannelClosed {
                    summary: "closed".into(),
                }),
            );
            assert_eq!(state.master_volume, 0.8);
            assert!(!state.volume_pending);
        }

        #[test]
        fn volume_non_finite_input_keeps_value_and_publishes_one_notice() {
            let mut state = AppState::default();

            let first = reduce(&mut state, AppEvent::MasterVolumeChanged(f32::NAN));
            assert_eq!(state.master_volume, 0.25);
            assert!(first.effects.is_empty());
            assert_eq!(
                state.notice,
                Some(UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::InvalidInput,
                    summary: "Master volume must be between 0% and 100%.".into(),
                    action: None,
                })
            );
            assert!(first.snapshot_changed);
            assert_eq!(state.revision, 1);

            let second = reduce(&mut state, AppEvent::MasterVolumeChanged(f32::INFINITY));
            assert_eq!(state.master_volume, 0.25);
            assert!(second.effects.is_empty());
            assert!(!second.snapshot_changed);
            assert_eq!(state.revision, 1);
        }

        #[test]
        fn volume_release_of_unchanged_value_applies_without_false_revision() {
            let mut state = AppState {
                revision: 4,
                master_volume: 0.25,
                generations: Generations {
                    volume: GenerationId(3),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::MasterVolumeChanged(0.25));

            assert_eq!(state.master_volume, 0.25);
            assert_eq!(
                transition.effects,
                vec![AppEffect::ApplyVolume {
                    generation: GenerationId(4),
                    volume: 0.25,
                }]
            );
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 4);
        }

        #[test]
        fn page_navigation_schedules_preference_persistence_once_per_change() {
            let mut state = AppState {
                revision: 2,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::Navigate(Page::Speakers));

            assert_eq!(state.page, Page::Speakers);
            assert_eq!(state.generations.preferences, GenerationId(1));
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(1),
                    value: Preferences {
                        last_page: Page::Speakers,
                        ..Preferences::default()
                    },
                }]
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 3);

            let repeat = reduce(&mut state, AppEvent::Navigate(Page::Speakers));
            assert!(repeat.effects.is_empty());
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.revision, 3);
        }

        #[test]
        fn theme_change_persists_preferences_built_from_new_state() {
            let mut state = AppState {
                page: Page::Settings,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::ThemeChanged(ThemePreference::Dark));

            assert_eq!(state.theme, ThemePreference::Dark);
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(1),
                    value: Preferences {
                        theme: ThemePreference::Dark,
                        last_page: Page::Settings,
                        ..Preferences::default()
                    },
                }]
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 1);
        }

        #[test]
        fn valid_geometry_change_clamps_persists_and_invalid_is_rejected_with_notice() {
            let mut state = AppState::default();

            let requested = WindowGeometry {
                x: Some(12.0),
                y: Some(-4.0),
                width: 800.0,
                height: 700.0,
                maximized: true,
            };
            let applied = WindowGeometry {
                x: Some(12.0),
                y: Some(-4.0),
                width: 900.0,
                height: 700.0,
                maximized: true,
            };
            let change = reduce(&mut state, AppEvent::WindowGeometryChanged(requested));

            assert_eq!(state.window.geometry, applied);
            assert_eq!(
                change.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(1),
                    value: Preferences {
                        window: applied,
                        ..Preferences::default()
                    },
                }]
            );
            assert!(change.snapshot_changed);

            let rejected = reduce(
                &mut state,
                AppEvent::WindowGeometryChanged(WindowGeometry {
                    x: None,
                    y: None,
                    width: f32::NAN,
                    height: 700.0,
                    maximized: true,
                }),
            );

            assert_eq!(state.window.geometry, applied);
            assert!(rejected.effects.is_empty());
            assert_eq!(
                state.notice,
                Some(UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::InvalidInput,
                    summary: "Window geometry was ignored because it contains invalid values."
                        .into(),
                    action: None,
                })
            );
            assert!(rejected.snapshot_changed);
            assert_eq!(state.revision, 2);
        }

        #[test]
        fn hotkey_change_requests_reconfiguration_and_retains_applied_binding() {
            let mut state = AppState {
                revision: 5,
                ..Default::default()
            };
            let requested = binding(false, 0x50);

            let transition = reduce(&mut state, AppEvent::HotkeyChanged(requested));

            assert_eq!(state.hotkey, HotkeyBinding::default());
            assert_eq!(state.generations.hotkey, GenerationId(1));
            assert_eq!(
                transition.effects,
                vec![AppEffect::ReconfigureHotkey {
                    generation: GenerationId(1),
                    value: requested,
                }]
            );
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 5);
        }

        #[test]
        fn matching_hotkey_confirmation_applies_binding_and_persists_it() {
            let mut state = AppState::default();
            let requested = binding(false, 0x50);
            reduce(&mut state, AppEvent::HotkeyChanged(requested));

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::HotkeyReconfigured {
                    generation: GenerationId(1),
                    applied: requested,
                    failure: None,
                }),
            );

            assert_eq!(state.hotkey, requested);
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(1),
                    value: Preferences {
                        hotkey: requested,
                        ..Preferences::default()
                    },
                }]
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 1);
        }

        #[test]
        fn failed_hotkey_reconfiguration_preserves_applied_binding_and_reports_once() {
            let mut state = AppState::default();
            let requested = binding(true, 0x50);
            reduce(&mut state, AppEvent::HotkeyChanged(requested));

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::HotkeyReconfigured {
                    generation: GenerationId(1),
                    applied: HotkeyBinding::default(),
                    failure: Some("the chord is registered by another program".into()),
                }),
            );

            assert_eq!(state.hotkey, HotkeyBinding::default());
            assert_eq!(
                state.notice,
                Some(UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::HotkeyFailed,
                    summary: "the chord is registered by another program".into(),
                    action: Some(CorrectiveAction::OpenSettings),
                })
            );
            assert_eq!(transition.effects.len(), 1);
            assert!(matches!(
                &transition.effects[0],
                AppEffect::PersistPreferences { value, .. } if value.hotkey == HotkeyBinding::default()
            ));
            assert!(transition.snapshot_changed);
        }

        #[test]
        fn stale_hotkey_confirmation_is_a_complete_noop() {
            let mut state = AppState {
                generations: Generations {
                    hotkey: GenerationId(3),
                    ..Default::default()
                },
                ..Default::default()
            };
            let before = state.clone();

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::HotkeyReconfigured {
                    generation: GenerationId(2),
                    applied: binding(false, 0x50),
                    failure: None,
                }),
            );

            assert_eq!(state, before);
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
        }

        #[test]
        fn main_window_close_request_hides_only() {
            let mut state = AppState {
                revision: 6,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::MainWindowCloseRequested);

            assert!(!state.window.visible);
            assert_eq!(transition.effects, vec![AppEffect::HideMainWindow]);
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 7);

            let repeat = reduce(&mut state, AppEvent::MainWindowCloseRequested);
            assert_eq!(repeat.effects, vec![AppEffect::HideMainWindow]);
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.revision, 7);
        }

        #[test]
        fn show_main_window_event_only_requests_show() {
            let mut state = AppState {
                revision: 6,
                ..Default::default()
            };
            state.window.visible = false;

            let shown = reduce(&mut state, AppEvent::ShowMainWindow);
            assert!(state.window.visible);
            assert_eq!(shown.effects, vec![AppEffect::ShowMainWindow]);
            assert!(shown.snapshot_changed);
            assert_eq!(state.revision, 7);

            let repeat = reduce(&mut state, AppEvent::ShowMainWindow);
            assert_eq!(repeat.effects, vec![AppEffect::ShowMainWindow]);
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.revision, 7);
        }

        #[test]
        fn first_quit_persists_stops_hides_and_begins_shutdown_exactly_once() {
            let mut state = AppState {
                revision: 8,
                desired_receivers: ids(&[1, 2]),
                staged_receivers: ids(&[1, 2]),
                active_receivers: ids(&[1]),
                stream: StreamState::Streaming {
                    generation: GenerationId(4),
                },
                generations: Generations {
                    session: GenerationId(4),
                    ..Default::default()
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::QuitRequested);

            assert!(state.shutting_down);
            assert_eq!(
                state.stream,
                StreamState::Stopping {
                    generation: GenerationId(5)
                }
            );
            assert!(state.active_receivers.is_empty());
            assert_eq!(
                transition.effects,
                vec![
                    AppEffect::PersistPreferences {
                        generation: GenerationId(1),
                        value: Preferences::default(),
                    },
                    AppEffect::StopSession {
                        generation: GenerationId(5),
                    },
                    AppEffect::HideMainWindow,
                    AppEffect::BeginShutdown,
                ]
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 9);

            let second_quit = reduce(&mut state, AppEvent::QuitRequested);
            assert!(second_quit.effects.is_empty());
            assert!(!second_quit.snapshot_changed);

            let start_after_quit = reduce(&mut state, AppEvent::StartRequested);
            assert!(start_after_quit.effects.is_empty());
            assert!(!start_after_quit.snapshot_changed);
        }

        #[test]
        fn quit_while_stopped_omits_the_stop_effect() {
            let mut state = AppState::default();

            let transition = reduce(&mut state, AppEvent::QuitRequested);

            assert!(state.shutting_down);
            assert_eq!(state.stream, StreamState::Stopped);
            assert_eq!(
                transition.effects,
                vec![
                    AppEffect::PersistPreferences {
                        generation: GenerationId(1),
                        value: Preferences::default(),
                    },
                    AppEffect::HideMainWindow,
                    AppEffect::BeginShutdown,
                ]
            );
            assert!(transition.snapshot_changed);
        }

        #[test]
        fn refresh_requests_discovery_with_a_new_generation() {
            let mut state = AppState {
                revision: 1,
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::RefreshRequested);

            assert_eq!(
                state.discovery,
                DiscoveryState::Discovering {
                    generation: GenerationId(1)
                }
            );
            assert_eq!(
                transition.effects,
                vec![AppEffect::Discover {
                    generation: GenerationId(1),
                }]
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 2);
        }

        #[test]
        fn controller_channel_closure_publishes_needs_attention_without_membership_changes() {
            let mut state = AppState {
                desired_receivers: ids(&[1]),
                staged_receivers: ids(&[1]),
                ..Default::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::ChannelClosed {
                    summary: "device service stopped unexpectedly".into(),
                }),
            );

            assert_eq!(
                state.notice,
                Some(UserNotice {
                    severity: Severity::Error,
                    code: NoticeCode::ControllerUnavailable,
                    summary: "device service stopped unexpectedly".into(),
                    action: None,
                })
            );
            assert_eq!(state.desired_receivers, ids(&[1]));
            assert!(!state.shutting_down);
            assert!(transition.effects.is_empty());
            assert!(transition.snapshot_changed);

            let repeat = reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::ChannelClosed {
                    summary: "device service stopped unexpectedly".into(),
                }),
            );
            assert!(!repeat.snapshot_changed);
        }
    }

    mod language {
        use super::*;
        use crate::app::{
            Generations, HotkeyBinding, LocalePreference, Page, Preferences, ResolvedLocale,
            ThemePreference, WindowGeometry, WindowState, PREFERENCES_SCHEMA_VERSION,
        };

        /// A state whose every persisted field differs from its default, so a
        /// value built from it cannot accidentally look right.
        fn distinctive_state() -> AppState {
            AppState {
                page: Page::Settings,
                theme: ThemePreference::Dark,
                hotkey: HotkeyBinding {
                    enabled: false,
                    modifiers: 5,
                    virtual_key: 0x50,
                },
                window: WindowState {
                    visible: true,
                    geometry: WindowGeometry {
                        x: Some(20.0),
                        y: Some(-8.0),
                        width: 1024.0,
                        height: 768.0,
                        maximized: true,
                    },
                    close_to_tray: false,
                    launch_at_startup: true,
                },
                windows_display_locale: ResolvedLocale::English,
                ..Default::default()
            }
        }

        /// The complete value the distinctive state must produce, spelled out
        /// field by field so a dropped field fails instead of defaulting.
        fn expected_value(locale: LocalePreference, advanced_information: bool) -> Preferences {
            Preferences {
                schema_version: PREFERENCES_SCHEMA_VERSION,
                hotkey: HotkeyBinding {
                    enabled: false,
                    modifiers: 5,
                    virtual_key: 0x50,
                },
                theme: ThemePreference::Dark,
                locale,
                advanced_information,
                window: WindowGeometry {
                    x: Some(20.0),
                    y: Some(-8.0),
                    width: 1024.0,
                    height: 768.0,
                    maximized: true,
                },
                last_page: Page::Settings,
                close_to_tray: false,
                launch_at_startup: true,
            }
        }

        #[test]
        fn locale_choice_updates_memory_and_schedules_the_complete_current_value() {
            let mut state = AppState {
                advanced_information: true,
                ..distinctive_state()
            };

            let transition = reduce(
                &mut state,
                AppEvent::LocaleChanged(LocalePreference::German),
            );

            assert_eq!(state.locale_preference, LocalePreference::German);
            assert_eq!(state.resolved_locale, ResolvedLocale::German);
            assert_eq!(
                state.windows_display_locale,
                ResolvedLocale::English,
                "an explicit choice does not disturb the cached OS language"
            );
            assert_eq!(state.generations.preferences, GenerationId(1));
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(1),
                    value: expected_value(LocalePreference::German, true),
                }]
            );
            assert!(transition.snapshot_changed);

            let repeat = reduce(
                &mut state,
                AppEvent::LocaleChanged(LocalePreference::German),
            );
            assert!(repeat.effects.is_empty());
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.generations.preferences, GenerationId(1));
        }

        #[test]
        fn advanced_information_toggle_updates_memory_and_schedules_the_complete_current_value() {
            let mut state = AppState {
                locale_preference: LocalePreference::English,
                resolved_locale: ResolvedLocale::English,
                generations: Generations {
                    preferences: GenerationId(7),
                    ..Default::default()
                },
                ..distinctive_state()
            };

            let transition = reduce(&mut state, AppEvent::AdvancedInformationChanged(true));

            assert!(state.advanced_information);
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(8),
                    value: expected_value(LocalePreference::English, true),
                }]
            );
            assert!(transition.snapshot_changed);

            let repeat = reduce(&mut state, AppEvent::AdvancedInformationChanged(true));
            assert!(repeat.effects.is_empty());
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.generations.preferences, GenerationId(8));
        }

        #[test]
        fn choosing_system_again_resolves_through_the_cached_windows_language() {
            let mut state = AppState {
                locale_preference: LocalePreference::English,
                resolved_locale: ResolvedLocale::English,
                windows_display_locale: ResolvedLocale::German,
                ..Default::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::LocaleChanged(LocalePreference::System),
            );

            assert_eq!(state.locale_preference, LocalePreference::System);
            assert_eq!(state.resolved_locale, ResolvedLocale::German);
            assert!(transition.snapshot_changed);
        }

        #[test]
        fn an_os_language_change_under_system_republishes_and_never_persists() {
            let mut state = AppState {
                locale_preference: LocalePreference::System,
                windows_display_locale: ResolvedLocale::English,
                resolved_locale: ResolvedLocale::English,
                ..Default::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German),
            );

            assert_eq!(state.windows_display_locale, ResolvedLocale::German);
            assert_eq!(state.resolved_locale, ResolvedLocale::German);
            assert!(
                transition.effects.is_empty(),
                "an OS language change is not a user preference"
            );
            assert_eq!(state.generations.preferences, GenerationId(0));
            assert!(transition.snapshot_changed);

            let repeat = reduce(
                &mut state,
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German),
            );
            assert!(repeat.effects.is_empty());
            assert!(!repeat.snapshot_changed);
        }

        #[test]
        fn an_os_language_change_under_an_explicit_choice_updates_only_the_cache() {
            let mut state = AppState {
                revision: 5,
                locale_preference: LocalePreference::English,
                windows_display_locale: ResolvedLocale::English,
                resolved_locale: ResolvedLocale::English,
                ..Default::default()
            };

            let transition = reduce(
                &mut state,
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German),
            );

            assert_eq!(
                state.windows_display_locale,
                ResolvedLocale::German,
                "the cache always follows Windows"
            );
            assert_eq!(
                state.resolved_locale,
                ResolvedLocale::English,
                "an explicit choice keeps rendering in its own language"
            );
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 5);
            assert_eq!(state.generations.preferences, GenerationId(0));

            let back_to_system = reduce(
                &mut state,
                AppEvent::LocaleChanged(LocalePreference::System),
            );
            assert_eq!(
                state.resolved_locale,
                ResolvedLocale::German,
                "returning to System uses the language Windows renders in now"
            );
            assert!(back_to_system.snapshot_changed);
        }
    }

    /// Outcomes of the settings write, and the retry that is scoped to it.
    ///
    /// Everything here turns on one number: `generations.preferences`. It is
    /// the only authority for which write the shell is currently waiting on,
    /// so an outcome that names a different one says nothing and must change
    /// nothing.
    mod persistence_outcomes {
        use super::*;
        use crate::app::{
            CorrectiveAction, DiscoveryState, Generations, HotkeyBinding, LocalePreference,
            NoticeCode, Page, Preferences, PreferencesEvent, ResolvedLocale, Severity,
            ThemePreference, UserNotice, WindowGeometry, WindowState, PREFERENCES_SCHEMA_VERSION,
        };

        /// A state whose every persisted field differs from its default, so a
        /// Retry value that dropped a field cannot accidentally look right.
        fn distinctive_state() -> AppState {
            AppState {
                page: Page::Settings,
                theme: ThemePreference::Dark,
                locale_preference: LocalePreference::German,
                resolved_locale: ResolvedLocale::German,
                advanced_information: true,
                hotkey: HotkeyBinding {
                    enabled: false,
                    modifiers: 5,
                    virtual_key: 0x50,
                },
                window: WindowState {
                    visible: true,
                    geometry: WindowGeometry {
                        x: Some(20.0),
                        y: Some(-8.0),
                        width: 1024.0,
                        height: 768.0,
                        maximized: true,
                    },
                    close_to_tray: false,
                    launch_at_startup: true,
                },
                ..Default::default()
            }
        }

        /// The complete value the distinctive state must re-offer on Retry,
        /// spelled out field by field so a dropped field fails the test.
        fn expected_value() -> Preferences {
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
                last_page: Page::Settings,
                close_to_tray: false,
                launch_at_startup: true,
            }
        }

        /// The one notice a successful write is allowed to clear.
        ///
        /// Its text is the reducer's own constant, never the worker's: the
        /// outcome event carries a generation and nothing else, so no I/O
        /// error string can reach state, snapshot, or renderer.
        fn scoped_persistence_notice() -> UserNotice {
            UserNotice {
                severity: Severity::Warning,
                code: NoticeCode::PreferencesFailed,
                summary: super::super::PREFERENCES_PERSIST_FAILED_SUMMARY.to_string(),
                action: Some(CorrectiveAction::RetryPreferencesPersistence),
            }
        }

        fn with_preferences_generation(generation: u64, state: AppState) -> AppState {
            AppState {
                generations: Generations {
                    preferences: GenerationId(generation),
                    ..state.generations
                },
                ..state
            }
        }

        #[test]
        fn a_matching_failure_publishes_the_scoped_notice_and_keeps_every_memory_value() {
            let mut state = AppState {
                revision: 4,
                ..with_preferences_generation(3, distinctive_state())
            };
            let before = state.clone();

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(3),
                }),
            );

            assert_eq!(state.notice, Some(scoped_persistence_notice()));
            assert!(
                transition.effects.is_empty(),
                "a failed write reports; it does not requeue itself, got {:?}",
                transition.effects
            );
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 5);
            assert_eq!(
                state.generations.preferences,
                GenerationId(3),
                "reporting an outcome must not consume a generation"
            );

            // The optimistic in-memory values are what the user is looking at.
            // A failed write leaves the app exactly as they set it up.
            assert_eq!(state.theme, before.theme);
            assert_eq!(state.locale_preference, before.locale_preference);
            assert_eq!(state.resolved_locale, before.resolved_locale);
            assert_eq!(state.advanced_information, before.advanced_information);
            assert_eq!(state.page, before.page);
            assert_eq!(state.window, before.window);
            assert_eq!(state.hotkey, before.hotkey);

            let repeat = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(3),
                }),
            );
            assert!(!repeat.snapshot_changed);
            assert_eq!(state.revision, 5);
        }

        #[test]
        fn a_stale_failure_never_accuses_the_write_that_superseded_it() {
            let mut state = AppState {
                revision: 9,
                ..with_preferences_generation(5, AppState::default())
            };

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(4),
                }),
            );

            assert_eq!(state.notice, None);
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 9);
        }

        #[test]
        fn a_stale_success_neither_clears_the_notice_nor_changes_the_snapshot() {
            let mut state = AppState {
                revision: 9,
                notice: Some(scoped_persistence_notice()),
                ..with_preferences_generation(5, AppState::default())
            };

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::Persisted {
                    generation: GenerationId(4),
                }),
            );

            assert_eq!(
                state.notice,
                Some(scoped_persistence_notice()),
                "an older write succeeding says nothing about the newest one"
            );
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 9);
        }

        #[test]
        fn a_matching_success_clears_only_the_scoped_persistence_notice() {
            let mut state = AppState {
                revision: 2,
                notice: Some(scoped_persistence_notice()),
                ..with_preferences_generation(6, AppState::default())
            };

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::Persisted {
                    generation: GenerationId(6),
                }),
            );

            assert_eq!(state.notice, None);
            assert!(transition.effects.is_empty());
            assert!(transition.snapshot_changed);
            assert_eq!(state.revision, 3);

            // Every other notice outlives the write, including the *other*
            // PreferencesFailed one: a settings file that could not be read at
            // startup is still unreadable after a later write succeeded.
            for unrelated in [
                UserNotice {
                    severity: Severity::Error,
                    code: NoticeCode::SessionFailed,
                    summary: "receiver rejected the connection".into(),
                    action: Some(CorrectiveAction::Retry),
                },
                UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::NoReceiverAvailable,
                    summary: "No AirPlay receiver is available right now.".into(),
                    action: Some(CorrectiveAction::Refresh),
                },
                UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::PreferencesFailed,
                    summary: "Saved settings could not be read and were reset to their defaults."
                        .into(),
                    action: Some(CorrectiveAction::OpenSettings),
                },
            ] {
                let mut state = AppState {
                    revision: 2,
                    notice: Some(unrelated.clone()),
                    ..with_preferences_generation(6, AppState::default())
                };

                let transition = reduce(
                    &mut state,
                    AppEvent::Preferences(PreferencesEvent::Persisted {
                        generation: GenerationId(6),
                    }),
                );

                assert_eq!(
                    state.notice,
                    Some(unrelated.clone()),
                    "a successful write must not silence an unrelated notice"
                );
                assert!(!transition.snapshot_changed);
                assert_eq!(state.revision, 2);
            }
        }

        #[test]
        fn retry_reschedules_the_complete_current_value_at_the_next_generation() {
            let mut state = AppState {
                revision: 4,
                notice: Some(scoped_persistence_notice()),
                ..with_preferences_generation(3, distinctive_state())
            };

            let transition = reduce(&mut state, AppEvent::RetryPreferencesPersistence);

            assert_eq!(state.generations.preferences, GenerationId(4));
            assert_eq!(
                transition.effects,
                vec![AppEffect::PersistPreferences {
                    generation: GenerationId(4),
                    value: expected_value(),
                }]
            );
            assert_eq!(
                state.notice,
                Some(scoped_persistence_notice()),
                "the notice stands until the retry actually succeeds"
            );
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, 4);

            // The retry's own generation is now the only one that counts.
            let superseded = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::Persisted {
                    generation: GenerationId(3),
                }),
            );
            assert!(!superseded.snapshot_changed);
            assert_eq!(state.notice, Some(scoped_persistence_notice()));

            let matching = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::Persisted {
                    generation: GenerationId(4),
                }),
            );
            assert!(matching.snapshot_changed);
            assert_eq!(state.notice, None);
        }

        #[test]
        fn retry_never_starts_refreshes_or_touches_any_controller_work() {
            let mut state = AppState {
                receivers: vec![receiver(1, "Kitchen")],
                desired_receivers: ids(&[1]),
                staged_receivers: ids(&[1]),
                stream: StreamState::Stopped,
                discovery: DiscoveryState::Ready,
                notice: Some(scoped_persistence_notice()),
                generations: Generations {
                    discovery: GenerationId(2),
                    session: GenerationId(3),
                    volume: GenerationId(4),
                    preferences: GenerationId(5),
                    hotkey: GenerationId(9),
                },
                ..Default::default()
            };

            let transition = reduce(&mut state, AppEvent::RetryPreferencesPersistence);

            assert_eq!(
                transition.effects.len(),
                1,
                "the scoped retry re-offers the settings file and nothing else, got {:?}",
                transition.effects
            );
            assert!(
                matches!(
                    transition.effects[0],
                    AppEffect::PersistPreferences {
                        generation: GenerationId(6),
                        ..
                    }
                ),
                "got {:?}",
                transition.effects
            );
            assert_eq!(state.stream, StreamState::Stopped);
            assert_eq!(state.discovery, DiscoveryState::Ready);
            assert_eq!(state.generations.discovery, GenerationId(2));
            assert_eq!(state.generations.session, GenerationId(3));
            assert_eq!(state.generations.volume, GenerationId(4));
            assert_eq!(state.generations.hotkey, GenerationId(9));
            assert_eq!(state.active_receivers, ids(&[]));
        }

        /// A background save that fails must not talk over an error.
        ///
        /// This notice is unlike every other one the reducer publishes: no
        /// user asked for it, and it arrives again on every debounced write --
        /// a window drag or a page change is enough. On a profile that cannot
        /// be written it therefore repeats indefinitely, while the errors it
        /// would overwrite fire exactly once and are never cleared again.
        #[test]
        fn a_failure_never_displaces_a_standing_error() {
            let mut state = with_preferences_generation(3, AppState::default());
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::ChannelClosed {
                    summary: "the audio controller stopped".to_string(),
                }),
            );
            let standing = state.notice.clone();
            let revision = state.revision;

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(3),
                }),
            );

            assert_eq!(
                state.notice, standing,
                "a settings write that failed must not erase the error the user has to see"
            );
            assert!(transition.effects.is_empty());
            assert!(!transition.snapshot_changed);
            assert_eq!(state.revision, revision);
            assert_eq!(state.generations.preferences, GenerationId(3));

            // The error outranks it for as long as it stands, however many
            // writes fail behind it.
            let repeat = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(3),
                }),
            );
            assert_eq!(state.notice, standing);
            assert!(!repeat.snapshot_changed);
        }

        /// The guard is about severity, not about being second.
        ///
        /// Warnings keep the reducer's ordinary last-writer-wins behaviour, so
        /// a failure that has something newer to say still says it.
        #[test]
        fn a_failure_still_replaces_a_standing_warning() {
            let mut state = AppState {
                notice: Some(UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::PreferencesFailed,
                    summary: "Saved settings could not be read and were reset to their defaults."
                        .into(),
                    action: Some(CorrectiveAction::OpenSettings),
                }),
                ..with_preferences_generation(3, AppState::default())
            };

            let transition = reduce(
                &mut state,
                AppEvent::Preferences(PreferencesEvent::PersistFailed {
                    generation: GenerationId(3),
                }),
            );

            assert_eq!(state.notice, Some(scoped_persistence_notice()));
            assert!(transition.snapshot_changed);
        }
    }
}
