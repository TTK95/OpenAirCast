//! Explicit, session-bound listening check. No hardware is measured here.

use super::{
    AppState, AudioEndpointSelection, Availability, CaptureState, GenerationId, LatencyChoice,
    StreamState,
};

/// Actions always originate from an explicit user gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HardwareCheckAction {
    Open,
    Prepare(bool),
    LowerVolume,
    Start,
    Heard(bool),
    Cancel,
}

/// A result is terminal only after the matching stop acknowledgement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HardwareCheckPhase {
    #[default]
    Preparation,
    Connecting,
    Listening,
    StopPending,
    Heard,
    NotHeard,
    Invalidated,
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    receivers: std::collections::HashSet<airplay_core::DeviceId>,
    endpoint: AudioEndpointSelection,
    captured_key: Option<super::AudioEndpointKey>,
    captured_name: Option<String>,
    volume: f32,
    levels: std::collections::BTreeMap<airplay_core::DeviceId, f32>,
}

/// Reducer-owned state; identities never enter the report projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HardwareCheck {
    pub phase: HardwareCheckPhase,
    pub prepared: bool,
    binding: Option<Binding>,
    pub(crate) generation: Option<GenerationId>,
    pub(crate) outcome: Option<bool>,
}

/// Identifier-free projection for the renderer and clipboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HardwareCheckBlocker {
    Unavailable,
    StopPending,
    SessionRunning,
    Selection,
    SelectionPending,
    Offline,
    ReceiverLevels,
    Muted,
    Latency,
    VolumePending,
    Volume,
    Source,
    Confirmation,
}

/// Identifier-free projection for the renderer and clipboard.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HardwareCheckSnapshot {
    pub phase: HardwareCheckPhase,
    pub prepared: bool,
    pub ready: bool,
    pub can_confirm: bool,
    pub count: usize,
    pub blocker: Option<HardwareCheckBlocker>,
}

fn binding(state: &AppState) -> Option<Binding> {
    Some(Binding {
        receivers: state.desired_receivers.clone(),
        endpoint: state.audio_source.as_ref()?.selection.clone(),
        captured_key: state.audio_source.as_ref()?.captured_key,
        captured_name: state.audio_source.as_ref()?.captured_name.clone(),
        volume: state.confirmed_master_volume?,
        levels: state.receiver_levels.clone()?,
    })
}

/// All prerequisites are known, settled and safe for this selection.
pub fn ready(state: &AppState) -> bool {
    prerequisite_blocker(state).is_none()
}

fn prerequisite_blocker(state: &AppState) -> Option<HardwareCheckBlocker> {
    use HardwareCheckBlocker as B;
    if state.shutting_down || state.controller_closed {
        return Some(B::Unavailable);
    }
    if !state.desired_known || state.desired_receivers.is_empty() {
        return Some(B::Selection);
    }
    if state.staged_receivers != state.desired_receivers
        || state
            .group_operation
            .as_ref()
            .is_some_and(|o| o.status == super::GroupOperationStatus::Pending)
    {
        return Some(B::SelectionPending);
    }
    if !state.desired_receivers.iter().all(|id| {
        state
            .receivers
            .iter()
            .any(|r| r.id == *id && r.availability == Availability::Available)
    }) {
        return Some(B::Offline);
    }
    if !state.receiver_levels.as_ref().is_some_and(|levels| {
        state.desired_receivers.iter().all(|id| {
            let level = levels.get(id).copied().unwrap_or(1.0);
            level.is_finite() && level > 0.0 && level <= 1.0
        })
    }) {
        return Some(B::ReceiverLevels);
    }
    if state.muted != Some(false) {
        return Some(B::Muted);
    }
    if !state
        .latency
        .as_ref()
        .is_some_and(|l| l.selected == LatencyChoice::Normal)
    {
        return Some(B::Latency);
    }
    if state.volume_pending {
        return Some(B::VolumePending);
    }
    if !state
        .confirmed_master_volume
        .is_some_and(|v| v.is_finite() && v > 0.0 && v <= 0.15 && v == state.master_volume)
    {
        return Some(B::Volume);
    }
    if !state.audio_source.as_ref().is_some_and(|a| {
        a.endpoints_known
            && !a.refresh_failed
            && !a.endpoints.is_empty()
            && matches!(
                a.selection,
                AudioEndpointSelection::SystemDefault | AudioEndpointSelection::Chosen(_)
            )
            && matches!(
                a.state,
                CaptureState::Capturing | CaptureState::SilentSystem
            )
    }) {
        return Some(B::Source);
    }
    None
}

fn start_blocker(state: &AppState) -> Option<HardwareCheckBlocker> {
    use HardwareCheckBlocker as B;
    if state.shutting_down
        || state.controller_closed
        || matches!(state.stream, StreamState::Failed { .. })
    {
        return Some(B::Unavailable);
    }
    if state.hardware_check.phase == HardwareCheckPhase::StopPending
        || matches!(state.stream, StreamState::Stopping { .. })
    {
        return Some(B::StopPending);
    }
    if !matches!(state.stream, StreamState::Stopped) {
        return Some(B::SessionRunning);
    }
    prerequisite_blocker(state)
        .or_else(|| (!state.hardware_check.prepared).then_some(B::Confirmation))
}

impl HardwareCheck {
    pub(crate) fn bind(&mut self, state: &AppState) {
        self.binding = binding(state);
        self.generation = Some(state.generations.session);
        self.phase = HardwareCheckPhase::Connecting;
        self.outcome = None;
    }

    pub(crate) fn matches(&self, state: &AppState) -> bool {
        ready(state)
            && self.binding.as_ref().is_some_and(|bound| {
                state.audio_source.as_ref().is_some_and(|source| {
                    bound.endpoint == source.selection
                        && bound.captured_key == source.captured_key
                        && bound.captured_name == source.captured_name
                }) && bound.receivers == state.desired_receivers
                    && Some(bound.volume) == state.confirmed_master_volume
                    && state.receiver_levels.as_ref() == Some(&bound.levels)
            })
    }
}

impl HardwareCheckSnapshot {
    pub fn from_state(state: &AppState) -> Self {
        let blocker = start_blocker(state);
        Self {
            phase: state.hardware_check.phase,
            prepared: state.hardware_check.prepared,
            ready: blocker.is_none() || blocker == Some(HardwareCheckBlocker::Confirmation),
            can_confirm: state.hardware_check.phase == HardwareCheckPhase::Listening
                && state.hardware_check.matches(state)
                && state.hardware_check.generation == Some(state.generations.session)
                && matches!(state.stream, StreamState::Streaming { .. })
                && state.active_receivers == state.desired_receivers,
            count: state.desired_receivers.len(),
            blocker,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::*;

    fn heard_pending() -> AppState {
        let mut state = prepared();
        state.audio_source.as_mut().unwrap().captured_name = Some("Device A".into());
        act(&mut state, HardwareCheckAction::Start);
        let generation = state.generations.session;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
            }),
        );
        act(&mut state, HardwareCheckAction::Heard(true));
        state
    }

    #[test]
    fn lifecycle_faults_invalidate_pending_and_completed_hearing_but_stale_faults_do_not() {
        for completed in [false, true] {
            for stale in [false, true] {
                for fault in 0..3 {
                    let mut state = heard_pending();
                    let stop_generation = state.generations.session;
                    if completed {
                        reduce(
                            &mut state,
                            AppEvent::Controller(ControllerEvent::SessionStopped {
                                generation: stop_generation,
                            }),
                        );
                    }
                    let generation = if stale {
                        GenerationId(999)
                    } else {
                        stop_generation
                    };
                    let event = match fault {
                        0 => ControllerEvent::SessionFailed {
                            generation,
                            summary: "private".into(),
                        },
                        1 => ControllerEvent::SessionRestarting { generation },
                        _ => ControllerEvent::SessionDegraded {
                            generation,
                            active_receiver_ids: vec![],
                        },
                    };
                    reduce(&mut state, AppEvent::Controller(event));
                    if !completed {
                        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
                        reduce(
                            &mut state,
                            AppEvent::Controller(ControllerEvent::SessionStopped {
                                generation: stop_generation,
                            }),
                        );
                    }
                    assert_eq!(
                        state.hardware_check.phase,
                        if stale {
                            HardwareCheckPhase::Heard
                        } else {
                            HardwareCheckPhase::Invalidated
                        },
                        "completed={completed} stale={stale} fault={fault}"
                    );
                }
            }
        }
    }

    #[test]
    fn actual_system_default_source_switch_invalidates_hearing() {
        for completed in [false, true] {
            let mut state = heard_pending();
            let generation = state.generations.session;
            if completed {
                reduce(
                    &mut state,
                    AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
                );
            }
            let mut reading = state.audio_source.clone().unwrap();
            reading.captured_name = Some("Device B".into());
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::AudioSourceChanged { reading }),
            );
            if !completed {
                reduce(
                    &mut state,
                    AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
                );
            }
            assert_eq!(state.hardware_check.phase, HardwareCheckPhase::Invalidated);
        }
    }

    #[test]
    fn duplicate_source_names_do_not_hide_an_actual_endpoint_switch() {
        let mut state = prepared();
        state.audio_source.as_mut().unwrap().captured_key = Some(AudioEndpointKey(1));
        state.audio_source.as_mut().unwrap().captured_name = Some("Identical name".into());
        act(&mut state, HardwareCheckAction::Start);
        let generation = state.generations.session;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
            }),
        );
        assert!(HardwareCheckSnapshot::from_state(&state).can_confirm);
        let mut reading = state.audio_source.clone().unwrap();
        reading.captured_key = Some(AudioEndpointKey(2));
        let changed = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::AudioSourceChanged { reading }),
        );
        assert!(changed
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::StopSession { .. })));
        assert!(!HardwareCheckSnapshot::from_state(&state).can_confirm);
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
    }

    pub(crate) fn prepared() -> AppState {
        let id = airplay_core::DeviceId([1; 6]);
        let mut state = AppState {
            desired_known: true,
            receivers: vec![ReceiverState {
                id: id.clone(),
                name: "PRIVATE-SPEAKER".into(),
                model: "private".into(),
                availability: Availability::Available,
            }],
            desired_receivers: [id.clone()].into(),
            staged_receivers: [id].into(),
            master_volume: 0.1,
            confirmed_master_volume: Some(0.1),
            muted: Some(false),
            receiver_levels: Some(Default::default()),
            latency: Some(LatencyReading {
                selected: LatencyChoice::Normal,
                options: vec![],
            }),
            audio_source: Some(AudioSourceReading {
                endpoints_known: true,
                refresh_failed: false,
                endpoints: vec![AudioEndpointChoice {
                    key: AudioEndpointKey(1),
                    name: "PRIVATE-ENDPOINT".into(),
                }],
                selection: AudioEndpointSelection::SystemDefault,
                captured_key: None,
                captured_name: None,
                state: CaptureState::SilentSystem,
            }),
            ..Default::default()
        };
        state.hardware_check.prepared = true;
        state
    }

    fn act(state: &mut AppState, action: HardwareCheckAction) -> Vec<AppEffect> {
        reduce(state, AppEvent::HardwareCheck(action)).effects
    }

    #[test]
    fn hardware_check_blockers_describe_the_same_guards_that_reject_start() {
        use HardwareCheckBlocker as B;
        let cases: &[(fn(&mut AppState), B)] = &[
            (|s| s.controller_closed = true, B::Unavailable),
            (|s| s.desired_receivers.clear(), B::Selection),
            (|s| s.staged_receivers.clear(), B::SelectionPending),
            (
                |s| s.receivers[0].availability = Availability::Unavailable,
                B::Offline,
            ),
            (|s| s.receiver_levels = None, B::ReceiverLevels),
            (|s| s.muted = Some(true), B::Muted),
            (|s| s.latency = None, B::Latency),
            (|s| s.volume_pending = true, B::VolumePending),
            (|s| s.confirmed_master_volume = None, B::Volume),
            (|s| s.audio_source = None, B::Source),
            (|s| s.hardware_check.prepared = false, B::Confirmation),
            (
                |s| {
                    s.stream = StreamState::Streaming {
                        generation: GenerationId(1),
                    }
                },
                B::SessionRunning,
            ),
            (
                |s| {
                    s.stream = StreamState::Stopping {
                        generation: GenerationId(1),
                    }
                },
                B::StopPending,
            ),
        ];
        for (change, expected) in cases {
            let mut state = prepared();
            change(&mut state);
            assert_eq!(
                HardwareCheckSnapshot::from_state(&state).blocker,
                Some(*expected)
            );
            assert!(act(&mut state, HardwareCheckAction::Start).is_empty());
        }
        assert_eq!(HardwareCheckSnapshot::from_state(&prepared()).blocker, None);
    }

    #[test]
    fn hearing_test_emits_one_tone_only_after_its_complete_session_connects() {
        let mut state = prepared();
        act(&mut state, HardwareCheckAction::Start);
        let generation = state.generations.session;
        let connected = AppEvent::Controller(ControllerEvent::SessionStarted {
            generation,
            active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
        });
        let first = reduce(&mut state, connected.clone());
        assert_eq!(
            first.effects.len(),
            1,
            "a connected hearing test must actually request audio"
        );
        assert_eq!(
            first.effects,
            vec![AppEffect::PlayListeningTone { generation }]
        );
        assert!(
            reduce(&mut state, connected).effects.is_empty(),
            "a repeated snapshot must not replay the tone"
        );

        let mut ordinary = prepared();
        reduce(&mut ordinary, AppEvent::StartRequested);
        let generation = ordinary.generations.session;
        assert!(
            reduce(
                &mut ordinary,
                AppEvent::Controller(ControllerEvent::SessionStarted {
                    generation,
                    active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
                })
            )
            .effects
            .is_empty(),
            "ordinary streaming must never inject a test tone"
        );
    }

    #[test]
    fn tone_failure_requests_real_teardown_and_waits_for_its_acknowledgement() {
        let mut state = prepared();
        act(&mut state, HardwareCheckAction::Start);
        let generation = state.generations.session;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
            }),
        );
        let failure = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionFailed {
                generation,
                summary: "The listening test tone could not be started.".into(),
            }),
        );
        let stop_generation = state.generations.session;
        assert_ne!(stop_generation, generation);
        assert_eq!(
            failure.effects,
            vec![AppEffect::StopSession {
                generation: stop_generation
            }]
        );
        assert_eq!(
            state.stream,
            StreamState::Stopping {
                generation: stop_generation
            }
        );
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
        assert!(!HardwareCheckSnapshot::from_state(&state).can_confirm);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
        );
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: stop_generation,
            }),
        );
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::Invalidated);
    }

    #[test]
    fn explicit_start_is_once_and_result_requires_matching_stop_ack() {
        let mut state = prepared();
        assert!(matches!(
            act(&mut state, HardwareCheckAction::Start).as_slice(),
            [AppEffect::StartSession { .. }]
        ));
        assert!(act(&mut state, HardwareCheckAction::Start).is_empty());
        assert!(act(&mut state, HardwareCheckAction::Open).is_empty());
        assert!(act(&mut state, HardwareCheckAction::Heard(true)).is_empty());
        let generation = state.generations.session;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation: GenerationId(99),
                active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
            }),
        );
        assert!(!HardwareCheckSnapshot::from_state(&state).can_confirm);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![],
            }),
        );
        assert!(!HardwareCheckSnapshot::from_state(&state).can_confirm);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStarted {
                generation,
                active_receiver_ids: vec![airplay_core::DeviceId([1; 6])],
            }),
        );
        assert!(matches!(
            act(&mut state, HardwareCheckAction::Heard(true)).as_slice(),
            [AppEffect::StopSession { .. }]
        ));
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
        );
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
        let generation = state.generations.session;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
        );
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::Heard);
    }

    #[test]
    fn unsafe_or_unknown_preparation_and_foreign_sessions_block_start() {
        for change in [
            |s: &mut AppState| s.confirmed_master_volume = None,
            |s: &mut AppState| {
                s.master_volume = 0.16;
                s.confirmed_master_volume = Some(0.16);
            },
            |s: &mut AppState| s.desired_receivers.clear(),
            |s: &mut AppState| s.receivers[0].availability = Availability::Unavailable,
            |s: &mut AppState| s.muted = Some(true),
            |s: &mut AppState| s.volume_pending = true,
            |s: &mut AppState| s.receiver_levels = None,
            |s: &mut AppState| {
                s.receiver_levels
                    .as_mut()
                    .unwrap()
                    .insert(airplay_core::DeviceId([1; 6]), 0.0);
            },
            |s: &mut AppState| {
                s.stream = StreamState::Streaming {
                    generation: GenerationId(4),
                }
            },
        ] {
            let mut state = prepared();
            change(&mut state);
            assert!(act(&mut state, HardwareCheckAction::Start).is_empty());
        }
    }

    #[test]
    fn cancel_and_configuration_changes_wait_for_stop_and_never_pass() {
        for event in [
            AppEvent::HardwareCheck(HardwareCheckAction::Cancel),
            AppEvent::MasterVolumeChanged(0.12),
            AppEvent::ReceiverLevelRequested {
                receiver: airplay_core::DeviceId([1; 6]),
                level: 0.5,
            },
        ] {
            let mut state = prepared();
            act(&mut state, HardwareCheckAction::Start);
            assert!(reduce(&mut state, event)
                .effects
                .iter()
                .any(|e| matches!(e, AppEffect::StopSession { .. })));
            assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
            assert!(act(&mut state, HardwareCheckAction::Start).is_empty());
            let generation = state.generations.session;
            reduce(
                &mut state,
                AppEvent::Controller(ControllerEvent::SessionStopped { generation }),
            );
            assert_eq!(state.hardware_check.phase, HardwareCheckPhase::Invalidated);
        }
    }

    #[test]
    fn opening_never_starts_and_unknown_preparation_blocks_start() {
        let mut state = AppState::default();
        assert!(reduce(
            &mut state,
            AppEvent::HardwareCheck(HardwareCheckAction::Open)
        )
        .effects
        .is_empty());
        assert!(reduce(
            &mut state,
            AppEvent::HardwareCheck(HardwareCheckAction::Start)
        )
        .effects
        .is_empty());
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::Preparation);
    }

    #[test]
    fn failed_attempt_requires_stop_ack_and_blocks_other_start_paths() {
        let mut state = prepared();
        act(&mut state, HardwareCheckAction::Start);
        let generation = state.generations.session;
        let result = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::SessionFailed {
                generation,
                summary: "PRIVATE-ERROR".into(),
            }),
        );
        assert!(result
            .effects
            .iter()
            .any(|e| matches!(e, AppEffect::StopSession { .. })));
        assert!(reduce(&mut state, AppEvent::StartRequested)
            .effects
            .is_empty());
        assert_eq!(state.hardware_check.phase, HardwareCheckPhase::StopPending);
    }
}
