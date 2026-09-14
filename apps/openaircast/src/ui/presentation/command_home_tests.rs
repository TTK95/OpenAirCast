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
    const SOURCE: &str = include_str!("../presentation.rs");

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
