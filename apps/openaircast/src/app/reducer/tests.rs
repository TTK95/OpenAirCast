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

fn receiver_record(last: u8, name: &str, model: &str, availability: Availability) -> ReceiverState {
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
            AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(1))),
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
            AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(99))),
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

    const SOURCE: &str = include_str!("../reducer.rs");

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
                && pattern.starts_with(|first: char| first.is_ascii_alphanumeric() || first == '_')
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
    let available_alpha_mini = receiver_record(1, "Alpha", "HomePod mini", Availability::Available);
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
                summary: "Window geometry was ignored because it contains invalid values.".into(),
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
        CorrectiveAction, DiscoveryState, Generations, HotkeyBinding, LocalePreference, NoticeCode,
        Page, Preferences, PreferencesEvent, ResolvedLocale, Severity, ThemePreference, UserNotice,
        WindowGeometry, WindowState, PREFERENCES_SCHEMA_VERSION,
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
