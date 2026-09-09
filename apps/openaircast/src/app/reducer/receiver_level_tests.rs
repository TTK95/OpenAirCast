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
