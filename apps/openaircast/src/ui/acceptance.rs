//! The private acceptance matrix for the Windows Native Control Center.
//!
//! Everything here is deliberately *above* the components. A component test
//! can state that one renderer draws the right thing from the right model;
//! none of them can state that the six destinations together form one
//! keyboard cycle, that every event the surface is supposed to be able to send
//! actually has a control behind it, or that no page anywhere leaks a path or
//! an address into a screen reader. Those are the claims this module makes,
//! and each one is written so that a *new* page, a *new* event, or a *new*
//! preference field cannot slip past it silently.
//!
//! Four defects that reached a running window in this subproject are the
//! reason the assertions are shaped the way they are:
//!
//! * A tray menu that was fully implemented and fully model-tested, and whose
//!   `sync` had no caller, so a right click showed nothing.
//! * Two events that existed for six tasks with no renderer sending them.
//! * A catalog key no input could reach, because the field clamped its input.
//! * A state that was painted into a galley that produces no AccessKit node.
//!
//! All four are the same class: something built, tested in isolation, and
//! never driven. The matrix below drives.
//!
//! This module lives inside the binary on purpose. `apps/openaircast/tests`
//! cannot see `crate::ui` or `crate::app`, so an integration test could not
//! make any of these statements.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use airplay_core::DeviceId;
    use egui_kittest::kittest::{NodeT, Queryable};

    use crate::app::{
        AppEvent, AppState, AudioEndpointChoice, AudioEndpointKey, AudioEndpointRequest,
        AudioEndpointSelection, AudioSourceReading, Availability, BufferDiagnosticsReading,
        CaptureState, ControllerEvent, CorrectiveAction, DiagnosticIssue, DiagnosticReceiverState,
        DiagnosticsHealth, DiagnosticsReading, DiscoveryState, GenerationId, GroupId, GroupMember,
        HotkeyBinding, LatencyChoice, LatencyOption, LatencyReading, LatencyUnavailable,
        LiveDiagnosticsReading, LocalePreference, NoticeCode, Page, Preferences, PreferencesEvent,
        ReceiverDiagnosticsReading, ReceiverState, ResolvedLocale, SavedGroupReading, Severity,
        StreamState, ThemePreference, TransportDiagnosticsReading, UiSnapshot, UserNotice,
        WindowGeometry, WindowState,
    };
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::components::app_shell::{change_bar_model, show_shell};
    use crate::ui::i18n::{Catalog, TextKey};
    use crate::ui::layout::UiResources;
    use crate::ui::pages::DESTINATIONS;
    use crate::ui::theme;

    // ---------------------------------------------------------------- looks

    const LOCALES: [ResolvedLocale; 2] = [ResolvedLocale::German, ResolvedLocale::English];
    const WIDE: [f32; 2] = [1120.0, 720.0];
    const NARROW: [f32; 2] = [900.0, 600.0];

    /// One appearance the shell has to survive.
    ///
    /// High Contrast is not a fourth theme preference: it is a Windows
    /// reading that replaces the palette underneath whichever preference is
    /// in force, which is why it is carried beside the preference rather than
    /// inside it.
    #[derive(Clone, Copy, Debug)]
    struct Look {
        name: &'static str,
        theme: ThemePreference,
        high_contrast: bool,
        size: [f32; 2],
    }

    const LOOKS: &[Look] = &[
        Look {
            name: "light 1120x720",
            theme: ThemePreference::Light,
            high_contrast: false,
            size: WIDE,
        },
        Look {
            name: "dark 1120x720",
            theme: ThemePreference::Dark,
            high_contrast: false,
            size: WIDE,
        },
        Look {
            name: "light 900x600",
            theme: ThemePreference::Light,
            high_contrast: false,
            size: NARROW,
        },
        Look {
            name: "dark 900x600",
            theme: ThemePreference::Dark,
            high_contrast: false,
            size: NARROW,
        },
        Look {
            name: "high contrast 1120x720",
            theme: ThemePreference::System,
            high_contrast: true,
            size: WIDE,
        },
        Look {
            name: "high contrast 900x600",
            theme: ThemePreference::System,
            high_contrast: true,
            size: NARROW,
        },
    ];

    /// The look every assertion that is not itself about appearance uses.
    const PLAIN: Look = LOOKS[0];

    fn appearance(high_contrast: bool) -> SystemAppearance {
        SystemAppearance {
            client_animation_enabled: false,
            high_contrast,
            colors: SystemColors {
                background: [0, 0, 0],
                foreground: [255, 255, 255],
                highlight: [26, 235, 255],
                highlight_text: [0, 0, 0],
                disabled_text: [63, 242, 63],
                link: [255, 255, 0],
            },
        }
    }

    // ------------------------------------------------------------ scenarios

    /// The poisoned backend text every failure scenario carries.
    ///
    /// One string with every shape the shell must never repeat: an operating
    /// system error number, a hexadecimal status, a listening address, a user
    /// profile path, and a hardware address. A renderer that passes any
    /// backend summary through instead of resolving its notice code will put
    /// one of these in front of the user.
    const POISONED_SUMMARY: &str = concat!(
        "connect failed: os error 10061 (0x80070005) to 192.168.178.44:7000 ",
        r"for a0:b1:c2:d3:e4:01 while writing C:\Users\Thorsten\AppData\Roaming\",
        "OpenAirCast\\settings.json"
    );

    /// The fragments no announced or painted string may contain.
    ///
    /// Checked as substrings rather than as the whole summary, because the
    /// interesting failure is a renderer that quotes *part* of the backend
    /// text -- a truncated summary, a first sentence, an "including
    /// {address}" phrasing.
    const FORBIDDEN_FRAGMENTS: &[&str] = &[
        "os error",
        "0x",
        "192.168.",
        ":7000",
        r"C:\",
        "AppData",
        "settings.json",
        "a0:b1",
        "a0b1c2",
        // The receiver's own hardware model identifier. Receiver *names* are
        // the user's own words and belong on screen; `AudioAccessory5,1` is a
        // service record field and does not.
        "AudioAccessory",
    ];

    fn rid(last: u8) -> DeviceId {
        DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last])
    }

    fn receiver(last: u8, name: &str, model: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: model.into(),
            availability,
        }
    }

    /// Two discovered receivers, one of them applied.
    fn discovered() -> Vec<ReceiverState> {
        vec![
            receiver(1, "Kitchen", "AudioAccessory5,1", Availability::Available),
            receiver(2, "Studio", "AudioAccessory1,1", Availability::Available),
        ]
    }

    fn base() -> AppState {
        AppState {
            receivers: discovered(),
            saved_groups: Some(vec![SavedGroupReading {
                id: GroupId(uuid::Uuid::nil()),
                name: "Evening".into(),
                members: vec![
                    GroupMember {
                        receiver: rid(1),
                        name: "Kitchen".into(),
                        level: 0.7,
                    },
                    GroupMember {
                        receiver: rid(2),
                        name: "Studio".into(),
                        level: 0.8,
                    },
                    GroupMember {
                        receiver: rid(3),
                        name: "Office".into(),
                        level: 0.6,
                    },
                ],
                available_members: std::collections::BTreeSet::from([rid(1), rid(2)]),
            }]),
            receiver_levels: Some(Default::default()),
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            discovery: DiscoveryState::Ready,
            ..AppState::default()
        }
    }

    /// A named application state the whole surface has to render.
    struct Scenario {
        name: &'static str,
        state: AppState,
    }

    /// Every lifecycle state the Command Home has today, plus the two failure
    /// shapes and the two empty shapes.
    ///
    /// This is the axis a component test cannot cover: each renderer sees one
    /// model, while the claims below are about what the *whole* window says
    /// while the session moves through its states.
    ///
    /// A state absent from here runs through none of the seven whole-window
    /// guarantees -- not the redaction promise, not "no reachable control is
    /// inert", not the two-language rule. That is not a smaller test, it is
    /// no test, which is why `every_session_phase_has_a_scenario` stands
    /// beside this list.
    fn scenarios() -> Vec<Scenario> {
        let generation = GenerationId(7);

        let mut staged = base();
        staged.staged_receivers.insert(rid(2));

        let mut connecting = base();
        connecting.stream = StreamState::Starting { generation };

        let mut streaming = base();
        streaming.stream = StreamState::Streaming { generation };
        streaming.active_receivers = HashSet::from_iter([rid(1)]);

        // Audio reaches some of the group but not all of it: two receivers
        // were asked for, one is carrying sound. A running session, so every
        // control a running session offers has to be here too.
        let mut reduced = base();
        reduced.stream = StreamState::Degraded { generation };
        reduced.desired_receivers = HashSet::from_iter([rid(1), rid(2)]);
        reduced.staged_receivers = HashSet::from_iter([rid(1), rid(2)]);
        reduced.active_receivers = HashSet::from_iter([rid(1)]);

        // The backend rebuilding the whole group inside a live session. The
        // active set is empty on purpose -- nothing is carrying audio while
        // the group is torn down -- which makes this the one running state
        // with no active member, and therefore the one most likely to render
        // an empty Route Ribbon or a dead command.
        let mut reconnecting = base();
        reconnecting.stream = StreamState::Restarting { generation };
        reconnecting.desired_receivers = HashSet::from_iter([rid(1), rid(2)]);
        reconnecting.staged_receivers = HashSet::from_iter([rid(1), rid(2)]);

        let mut stopping = base();
        stopping.stream = StreamState::Stopping { generation };
        stopping.active_receivers = HashSet::from_iter([rid(1)]);

        let mut advanced = base();
        advanced.advanced_information = true;
        advanced.stream = StreamState::Streaming { generation };
        advanced.active_receivers = HashSet::from_iter([rid(1)]);
        // The one scenario that carries a diagnostics reading, so every
        // whole-window guarantee below evaluates a Diagnostics page with real
        // tiles on it rather than only its empty state. Kept to one scenario
        // deliberately: the other eleven are what keep the empty state under
        // the same guarantees.
        advanced.diagnostics = Some(DiagnosticsReading {
            health: DiagnosticsHealth::Attention,
            events_dropped_total: 12,
            measured_receivers: 2,
        });

        let mut failed = base();
        failed.stream = StreamState::Failed {
            generation,
            summary: POISONED_SUMMARY.into(),
        };
        failed.discovery = DiscoveryState::Failed {
            summary: POISONED_SUMMARY.into(),
        };
        failed.notice = Some(UserNotice {
            severity: Severity::Error,
            code: NoticeCode::SessionFailed,
            summary: POISONED_SUMMARY.into(),
            action: Some(CorrectiveAction::Retry),
        });

        let mut save_failed = base();
        save_failed.notice = Some(UserNotice {
            severity: Severity::Warning,
            code: NoticeCode::PreferencesFailed,
            summary: POISONED_SUMMARY.into(),
            action: Some(CorrectiveAction::RetryPreferencesPersistence),
        });

        let mut nothing_found = AppState {
            discovery: DiscoveryState::Failed {
                summary: POISONED_SUMMARY.into(),
            },
            notice: Some(UserNotice {
                severity: Severity::Warning,
                code: NoticeCode::DiscoveryFailed,
                summary: POISONED_SUMMARY.into(),
                action: Some(CorrectiveAction::Refresh),
            }),
            ..AppState::default()
        };
        nothing_found.master_volume = 0.25;

        // The one scenario carrying an audio reading, so every whole-window
        // guarantee is evaluated against a real capture list, a real switch,
        // and a latency group with a gated profile in it. The other twelve are
        // what keep the unmeasured Audio destination -- its honest empty
        // state -- under the same guarantees.
        //
        // The endpoint names are hardware product names on purpose: the
        // two-language guard reads every announced string, and a name that
        // happened to be an English catalog word would trip it while saying
        // nothing about the page.
        let mut audio = base();
        audio.muted = Some(false);
        audio.latency = Some(LatencyReading {
            selected: LatencyChoice::Normal,
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
        });
        audio.audio_source = Some(AudioSourceReading {
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
            selection: AudioEndpointSelection::Chosen(AudioEndpointKey(1)),
            captured_key: None,
            captured_name: Some("Realtek HDMI".into()),
            state: CaptureState::Capturing,
        });

        // The unfulfillable preference of paragraph 7.4, with nothing on
        // offer at all: the page still has to draw the Windows-default row and
        // has to say that the chosen device is absent.
        let mut audio_missing = base();
        audio_missing.muted = Some(true);
        audio_missing.audio_source = Some(AudioSourceReading {
            refresh_failed: false,
            endpoints_known: true,
            endpoints: Vec::new(),
            selection: AudioEndpointSelection::ChosenButMissing {
                name: "Arctis 7 Chat".into(),
            },
            captured_key: None,
            captured_name: None,
            state: CaptureState::Unavailable,
        });

        // What every real machine shows today: capture is running, and no
        // backend stage publishes an endpoint list, so the group has nothing
        // to offer beyond the source already in force. The whole-window
        // guarantees have to hold for the read-only shape too -- it is the
        // shape the owner will actually see.
        let mut audio_fixed = base();
        audio_fixed.muted = Some(false);
        audio_fixed.audio_source = Some(AudioSourceReading {
            refresh_failed: false,
            endpoints_known: true,
            endpoints: Vec::new(),
            selection: AudioEndpointSelection::SystemDefault,
            captured_key: None,
            captured_name: Some("Realtek HDMI".into()),
            state: CaptureState::Capturing,
        });

        let mut unavailable = base();
        unavailable.receivers = vec![receiver(
            3,
            "Bedroom",
            "AudioAccessory6,1",
            Availability::Unavailable,
        )];
        unavailable.desired_receivers = HashSet::from_iter([rid(3)]);
        unavailable.staged_receivers = HashSet::from_iter([rid(3)]);

        vec![
            Scenario {
                name: "ready",
                state: base(),
            },
            Scenario {
                name: "staged change",
                state: staged,
            },
            Scenario {
                name: "connecting",
                state: connecting,
            },
            Scenario {
                name: "streaming",
                state: streaming,
            },
            Scenario {
                name: "reduced",
                state: reduced,
            },
            Scenario {
                name: "reconnecting",
                state: reconnecting,
            },
            Scenario {
                name: "stopping",
                state: stopping,
            },
            Scenario {
                name: "streaming with advanced information",
                state: advanced,
            },
            Scenario {
                name: "session failed",
                state: failed,
            },
            Scenario {
                name: "settings could not be saved",
                state: save_failed,
            },
            Scenario {
                name: "nothing discovered",
                state: nothing_found,
            },
            Scenario {
                name: "only an unavailable receiver",
                state: unavailable,
            },
            Scenario {
                name: "audio reported",
                state: audio,
            },
            Scenario {
                name: "the chosen capture device is absent",
                state: audio_missing,
            },
            Scenario {
                name: "no capture device is on offer",
                state: audio_fixed,
            },
        ]
    }

    mod coverage {
        use super::*;
        use crate::ui::presentation::SessionVisualPhase;

        /// The scenario list has to cover the lifecycle, not a remembered
        /// subset of it.
        ///
        /// Every whole-window guarantee in this file is quantified over
        /// [`scenarios`], so a phase missing from that list is a phase for
        /// which none of them hold -- including the redaction promise and
        /// "no reachable control is inert". `Degraded` and `Restarting` were
        /// declared, wired, rendered, and shipped through this file without
        /// one of the seven ever seeing them.
        ///
        /// [`SessionVisualPhase::ALL`] is the anchor because it is what the
        /// window actually presents; the next phase somebody adds fails here
        /// instead of quietly rendering unverified.
        #[test]
        fn every_session_phase_has_a_scenario() {
            let covered = scenarios()
                .iter()
                .map(|scenario| {
                    SessionVisualPhase::from_stream(&UiSnapshot::from_state(&scenario.state).stream)
                })
                .collect::<Vec<_>>();

            let missing = SessionVisualPhase::ALL
                .iter()
                .filter(|phase| !covered.contains(phase))
                .collect::<Vec<_>>();

            assert!(
                missing.is_empty(),
                "the whole-window guarantees are never evaluated for these phases: \
                 {missing:?}"
            );
        }
    }

    // -------------------------------------------------------------- surface

    type Surface = egui_kittest::Harness<'static, Vec<AppEvent>>;

    // Mutable backend readings let the real shell see skipped Pending frames
    // and retained terminal results without starting an actor or audio device.
    type GroupsSurface = egui_kittest::Harness<'static, (AppState, Vec<AppEvent>)>;
    fn groups_surface(locale: ResolvedLocale, look: Look) -> GroupsSurface {
        let mut state = base();
        state.page = Page::Groups;
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, data: &mut (AppState, Vec<AppEvent>)| {
                let resolved =
                    theme::resolve_theme(look.theme, None, appearance(look.high_contrast));
                theme::apply_theme(ui.ctx(), &resolved);
                let resources = UiResources {
                    tokens: &resolved.tokens,
                    catalog: Catalog::new(locale),
                };
                let snapshot = UiSnapshot::from_state(&data.0);
                let bar = change_bar_model(&snapshot, resources.catalog);
                show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut |event| {
                    data.1.push(event)
                });
            },
            (state, Vec::new()),
        );
        harness.set_size(egui::vec2(look.size[0], look.size[1]));
        harness.run();
        harness
    }
    fn group_button(h: &mut GroupsSurface, locale: ResolvedLocale, key: TextKey) {
        h.get_by_role_and_label(accesskit::Role::Button, Catalog::new(locale).text(key))
            .click_accesskit();
        h.run();
    }
    fn group_name(h: &mut GroupsSurface, locale: ResolvedLocale, name: &str) {
        let label = Catalog::new(locale).text(TextKey::GroupName);
        h.get_by_role_and_label(accesskit::Role::TextInput, label)
            .focus();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run();
        h.get_by_role_and_label(accesskit::Role::TextInput, label)
            .type_text(name);
        h.run();
    }
    fn group_result(h: &mut GroupsSurface, status: crate::app::GroupOperationStatus) {
        let event = h
            .state_mut()
            .1
            .pop()
            .expect("a real control emitted a command");
        assert!(matches!(event, AppEvent::GroupRequested(_)));
        let result = crate::app::reduce(&mut h.state_mut().0, event);
        assert_eq!(result.effects.len(), 1);
        let request = h.state().0.group_operation.as_ref().unwrap().request;
        crate::app::reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished { request, status }),
        );
        h.run();
    }

    #[test]
    fn groups_create_save_and_cancel_are_real_local_controls() {
        for locale in LOCALES {
            let mut h = groups_surface(locale, PLAIN);
            let original = h.state().0.clone();
            group_button(&mut h, locale, TextKey::CreateGroup);
            assert!(h
                .get_by_role_and_label(
                    accesskit::Role::Button,
                    Catalog::new(locale).text(TextKey::SaveGroup)
                )
                .accesskit_node()
                .is_disabled());
            group_name(&mut h, locale, "  Morning  ");
            h.get_by_role_and_label(accesskit::Role::CheckBox, "Kitchen")
                .click_accesskit();
            h.run();
            assert!(h.state().1.is_empty());
            group_button(&mut h, locale, TextKey::Cancel);
            assert!(h.state().1.is_empty());
            assert_eq!(h.state().0, original);
            group_button(&mut h, locale, TextKey::CreateGroup);
            group_name(&mut h, locale, "  Morning  ");
            h.get_by_role_and_label(accesskit::Role::CheckBox, "Kitchen")
                .click_accesskit();
            h.run();
            group_button(&mut h, locale, TextKey::SaveGroup);
            assert_eq!(
                h.state().1,
                vec![AppEvent::GroupRequested(crate::app::GroupCommand::Save {
                    id: None,
                    name: "Morning".into(),
                    members: vec![GroupMember {
                        receiver: rid(1),
                        name: "Kitchen".into(),
                        level: 1.0
                    }]
                })]
            );
            assert_eq!(
                h.state().0,
                original,
                "only a backend reading may change the saved list or session"
            );
        }
    }

    #[test]
    fn groups_fast_save_success_cannot_close_a_later_editor_or_match_old_success() {
        use crate::app::GroupOperationStatus;
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        group_button(&mut h, locale, TextKey::EditGroup);
        group_button(&mut h, locale, TextKey::SaveGroup);
        group_result(&mut h, GroupOperationStatus::Succeeded); // No Pending render.
        h.get_by_role_and_label(
            accesskit::Role::Button,
            Catalog::new(locale).text(TextKey::EditGroup),
        );
        group_button(&mut h, locale, TextKey::EditGroup);
        h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        group_button(&mut h, locale, TextKey::SaveGroup); // Identical command to retained success.
        h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        group_result(
            &mut h,
            GroupOperationStatus::Failed(crate::app::GroupFailure::Persistence),
        );
        h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        h.get_by_label(Catalog::new(locale).text(TextKey::GroupPersistenceFailed));
    }

    fn group_save_replaced_while_away(observed: Option<crate::app::GroupOperationStatus>) {
        use crate::app::{reduce, GroupOperationStatus};
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        group_button(&mut h, locale, TextKey::CreateGroup);
        group_name(&mut h, locale, "Morning");
        h.get_by_role_and_label(accesskit::Role::CheckBox, "Kitchen")
            .click_accesskit();
        h.run();
        group_button(&mut h, locale, TextKey::SaveGroup);
        let save = h.state_mut().1.pop().unwrap();
        let mut confirmed = h.state().0.saved_groups.clone().unwrap();
        confirmed.push(SavedGroupReading {
            id: GroupId(uuid::Uuid::from_u128(7)),
            name: "Morning".into(),
            members: vec![GroupMember {
                receiver: rid(1),
                name: "Kitchen".into(),
                level: 1.0,
            }],
            available_members: std::collections::BTreeSet::from([rid(1)]),
        });
        if observed.is_some() {
            reduce(&mut h.state_mut().0, save.clone());
            h.run();
        }
        if observed == Some(GroupOperationStatus::Succeeded) {
            let request = h.state().0.group_operation.as_ref().unwrap().request;
            reduce(
                &mut h.state_mut().0,
                AppEvent::Controller(ControllerEvent::SavedGroupsChanged {
                    groups: confirmed.clone(),
                }),
            );
            reduce(
                &mut h.state_mut().0,
                AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                    request,
                    status: GroupOperationStatus::Succeeded,
                }),
            );
            h.run();
        }
        h.get_by_label(
            &Catalog::new(locale).named_value(crate::ui::i18n::NamedValueArgs {
                name: Catalog::new(locale).text(TextKey::NavigationDestinationAccessibility),
                value: "Speakers",
            }),
        )
        .click();
        h.run();
        let navigation = h.state_mut().1.pop().unwrap();
        assert_eq!(navigation, AppEvent::Navigate(Page::Speakers));
        if observed.is_none() {
            // Both queued events reach the reducer before another Groups frame.
            reduce(&mut h.state_mut().0, save);
        }
        let request = h.state().0.group_operation.as_ref().unwrap().request;
        reduce(&mut h.state_mut().0, navigation);
        h.run();
        reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::SavedGroupsChanged { groups: confirmed }),
        );
        reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request,
                status: GroupOperationStatus::Succeeded,
            }),
        );
        h.run(); // The Groups renderer is absent while its save completes.
        h.state_mut().0.desired_known = true;
        h.state_mut().0.staged_receivers.insert(rid(2));
        h.run();
        h.get_by_label(Catalog::new(locale).text(TextKey::ApplyChanges))
            .click_accesskit();
        h.run();
        let apply = h.state_mut().1.pop().unwrap();
        assert_eq!(apply, AppEvent::ApplyStagedReceivers);
        reduce(&mut h.state_mut().0, apply);
        let replacement = h.state().0.group_operation.as_ref().unwrap().request;
        assert_ne!(request, replacement);
        reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request: replacement,
                status: GroupOperationStatus::Succeeded,
            }),
        );
        h.run();
        h.get_by_label(
            &Catalog::new(locale).named_value(crate::ui::i18n::NamedValueArgs {
                name: Catalog::new(locale).text(TextKey::NavigationDestinationAccessibility),
                value: "Groups",
            }),
        )
        .click();
        h.run();
        let navigation = h.state_mut().1.pop().unwrap();
        assert_eq!(navigation, AppEvent::Navigate(Page::Groups));
        reduce(&mut h.state_mut().0, navigation);
        h.run();
        let expected = if observed == Some(GroupOperationStatus::Succeeded) {
            TextKey::GroupSaveSucceeded
        } else {
            TextKey::GroupUnconfirmed
        };
        h.get_by_label(Catalog::new(locale).text(expected));
        assert!(h
            .query_by_label(Catalog::new(locale).text(TextKey::GroupBusy))
            .is_none());
        if observed != Some(GroupOperationStatus::Succeeded) {
            h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        }
        h.run();
        assert!(
            h.state().1.is_empty(),
            "no automatic create retry after replacement"
        );
        assert_eq!(h.state().0.saved_groups.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn groups_navigation_replacement_before_association_is_unconfirmed() {
        group_save_replaced_while_away(None);
    }

    #[test]
    fn groups_navigation_replacement_after_association_is_unconfirmed() {
        group_save_replaced_while_away(Some(crate::app::GroupOperationStatus::Pending));
    }

    #[test]
    fn groups_navigation_replacement_retains_an_already_observed_success() {
        group_save_replaced_while_away(Some(crate::app::GroupOperationStatus::Succeeded));
    }

    #[test]
    fn groups_list_explains_availability_in_each_locale() {
        for (locale, expected) in [
            (ResolvedLocale::German, "2 von 3 verfügbar"),
            (ResolvedLocale::English, "2 of 3 available"),
        ] {
            let h = groups_surface(locale, PLAIN);
            h.get_by_label(expected);
        }
    }

    #[test]
    fn groups_edit_slider_is_local_and_failures_retain_offline_member() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        h.state_mut().0.receivers[0].availability = Availability::Unavailable;
        h.run();
        let original = h.state().0.clone();
        group_button(&mut h, locale, TextKey::EditGroup);
        h.get_by_role_and_label(accesskit::Role::CheckBox, "Kitchen · Unavailable");
        h.get_by_role_and_label(accesskit::Role::Slider, "Office")
            .focus();
        h.key_press(egui::Key::ArrowRight);
        h.run();
        assert!(
            h.state().1.is_empty(),
            "editor slider must not send live level effects"
        );
        group_button(&mut h, locale, TextKey::SaveGroup);
        let AppEvent::GroupRequested(crate::app::GroupCommand::Save { id, name, members }) =
            &h.state().1[0]
        else {
            panic!("save only")
        };
        assert_eq!(*id, Some(GroupId(uuid::Uuid::nil())));
        assert_eq!(name, "Evening");
        assert_eq!(members.len(), 3);
        assert_eq!(members[2].receiver, rid(3));
        assert_eq!(members[2].name, "Office");
        assert!(members[2].level > 0.6);
        let submitted = h.state().1[0].clone();
        group_result(
            &mut h,
            crate::app::GroupOperationStatus::Failed(crate::app::GroupFailure::Persistence),
        );
        group_button(&mut h, locale, TextKey::SaveGroup);
        assert_eq!(h.state().1, vec![submitted]);
        assert_eq!(h.state().0.saved_groups, original.saved_groups);
        assert_eq!(h.state().0.desired_receivers, original.desired_receivers);
        assert_eq!(h.state().0.receiver_levels, original.receiver_levels);
    }

    #[test]
    fn groups_delete_requires_exact_named_confirmation_and_cancel_sends_nothing() {
        for locale in LOCALES {
            let mut h = groups_surface(locale, PLAIN);
            group_button(&mut h, locale, TextKey::DeleteGroup);
            h.get_by_label(&Catalog::new(locale).group_delete_confirmation("Evening"));
            assert!(h.state().1.is_empty());
            group_button(&mut h, locale, TextKey::Cancel);
            assert!(h.state().1.is_empty());
            group_button(&mut h, locale, TextKey::DeleteGroup);
            group_button(&mut h, locale, TextKey::DeleteGroup);
            assert_eq!(
                h.state().1,
                vec![AppEvent::GroupRequested(crate::app::GroupCommand::Delete(
                    GroupId(uuid::Uuid::nil())
                ))]
            );
            assert_eq!(h.state().0.saved_groups.as_ref().unwrap().len(), 1);
        }
    }

    #[test]
    fn groups_apply_controls_require_dirty_selection_decision_and_preserve_start_flag() {
        for locale in LOCALES {
            for (key, start) in [
                (TextKey::ApplyGroup, false),
                (TextKey::ApplyGroupAndStart, true),
            ] {
                let mut h = groups_surface(locale, PLAIN);
                group_button(&mut h, locale, key);
                assert_eq!(
                    h.state().1,
                    vec![AppEvent::GroupRequested(crate::app::GroupCommand::Apply {
                        id: GroupId(uuid::Uuid::nil()),
                        start
                    })]
                );
                let mut h = groups_surface(locale, PLAIN);
                h.state_mut().0.staged_receivers.insert(rid(2));
                h.run();
                group_button(&mut h, locale, key);
                assert!(h.state().1.is_empty());
                group_button(&mut h, locale, TextKey::KeepSelectionDraft);
                assert!(h.state().1.is_empty());
                assert!(h.state().0.staged_receivers.contains(&rid(2)));
                group_button(&mut h, locale, key);
                group_button(&mut h, locale, TextKey::DiscardSelectionAndApply);
                assert_eq!(
                    h.state().1,
                    vec![
                        AppEvent::DiscardStagedReceivers,
                        AppEvent::GroupRequested(crate::app::GroupCommand::Apply {
                            id: GroupId(uuid::Uuid::nil()),
                            start
                        })
                    ]
                );
            }
        }
    }

    #[test]
    fn groups_editor_keyboard_targets_and_pending_are_accessible_in_both_locales_and_contrast() {
        for locale in LOCALES {
            for look in [LOOKS[2], LOOKS[5]] {
                let mut h = groups_surface(locale, look);
                group_button(&mut h, locale, TextKey::EditGroup);
                let field = h.get_by_role_and_label(
                    accesskit::Role::TextInput,
                    Catalog::new(locale).text(TextKey::GroupName),
                );
                assert!(field.rect().height() >= 40.0);
                let slider = h.get_by_role_and_label(accesskit::Role::Slider, "Office");
                assert!(
                    slider.rect().height() >= 40.0,
                    "slider hit target: {:?}",
                    slider.rect()
                );
                assert!(slider.rect().right() <= NARROW[0]);
                h.get_by_role_and_label(
                    accesskit::Role::Button,
                    Catalog::new(locale).text(TextKey::SaveGroup),
                )
                .focus();
                h.key_press(egui::Key::Enter);
                h.run();
                assert_eq!(h.state().1.len(), 1);
                let save = h.get_by_role_and_label(
                    accesskit::Role::Button,
                    Catalog::new(locale).text(TextKey::SaveGroup),
                );
                assert!(
                    save.accesskit_node().is_disabled(),
                    "admission awaiting reducer must block double clicks"
                );
                group_button(&mut h, locale, TextKey::SaveGroup);
                assert_eq!(h.state().1.len(), 1);
                let event = h.state_mut().1.pop().unwrap();
                crate::app::reduce(&mut h.state_mut().0, event);
                h.run();
                assert!(h
                    .get_by_role_and_label(
                        accesskit::Role::TextInput,
                        Catalog::new(locale).text(TextKey::GroupName)
                    )
                    .accesskit_node()
                    .is_disabled());
                h.get_by_label(Catalog::new(locale).text(TextKey::GroupOperationPending));
            }
        }
    }

    #[test]
    fn groups_unrelated_selection_result_never_closes_editor() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        group_button(&mut h, locale, TextKey::EditGroup);
        h.state_mut().0.group_operation = Some(crate::app::GroupOperation {
            request: 7,
            command: crate::app::GroupCommand::ApplySelection {
                receiver_ids: vec![rid(1)],
            },
            status: crate::app::GroupOperationStatus::Succeeded,
        });
        h.run();
        h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        group_button(&mut h, locale, TextKey::Cancel);
        assert!(h.state().1.is_empty());
    }

    #[test]
    fn groups_shell_admission_failure_retains_draft_and_never_claims_success() {
        for (failure, key) in [
            (crate::app_handle::AppUnavailable::Busy, TextKey::GroupBusy),
            (
                crate::app_handle::AppUnavailable::Closed,
                TextKey::GroupClosed,
            ),
        ] {
            let locale = ResolvedLocale::English;
            let mut h = groups_surface(locale, PLAIN);
            group_button(&mut h, locale, TextKey::EditGroup);
            group_name(&mut h, locale, "Updated");
            group_button(&mut h, locale, TextKey::SaveGroup);
            // The same callback that the native shell uses after try_send.
            crate::ui::pages::groups::admission_failed(&h.ctx, failure);
            h.state_mut().1.clear();
            h.run();
            h.get_by_label(Catalog::new(locale).text(key));
            h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
            assert!(h.state().0.group_operation.is_none());
            assert_eq!(
                h.state().0.saved_groups.as_ref().unwrap()[0].name,
                "Evening"
            );
            group_button(&mut h, locale, TextKey::Cancel);
            assert!(h.state().1.is_empty());
            h.get_by_role_and_label(
                accesskit::Role::Button,
                Catalog::new(locale).text(TextKey::EditGroup),
            );
        }
    }

    #[test]
    fn groups_edit_delete_leave_active_playback_and_header_control_unchanged() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        h.state_mut().0.stream = StreamState::Streaming {
            generation: GenerationId(3),
        };
        h.state_mut().0.active_receivers.insert(rid(1));
        h.run();
        let stream = h.state().0.stream.clone();
        group_button(&mut h, locale, TextKey::EditGroup);
        assert!(UiSnapshot::from_state(&h.state().0).can_stop);
        h.get_by_role_and_label(
            accesskit::Role::Button,
            Catalog::new(locale).text(TextKey::StopStreaming),
        );
        group_button(&mut h, locale, TextKey::SaveGroup);
        let save = h.state_mut().1.pop().unwrap();
        crate::app::reduce(&mut h.state_mut().0, save);
        h.run();
        group_button(&mut h, locale, TextKey::StopStreaming);
        assert_eq!(h.state().1, vec![AppEvent::StopRequested]);
        h.get_by_role_and_label(accesskit::Role::TextInput, "Group name");
        let request = h.state().0.group_operation.as_ref().unwrap().request;
        crate::app::reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                request,
                status: crate::app::GroupOperationStatus::Failed(
                    crate::app::GroupFailure::Persistence,
                ),
            }),
        );
        h.state_mut().1.clear();
        h.run();
        group_button(&mut h, locale, TextKey::Cancel);
        group_button(&mut h, locale, TextKey::DeleteGroup);
        group_button(&mut h, locale, TextKey::DeleteGroup);
        group_result(&mut h, crate::app::GroupOperationStatus::Succeeded);
        assert_eq!(h.state().0.stream, stream);
        assert!(UiSnapshot::from_state(&h.state().0).can_stop);
        assert_eq!(h.state().0.active_receivers, HashSet::from([rid(1)]));
    }

    #[test]
    fn release_groups_apply_start_exposes_actual_cancel_before_active() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        group_button(&mut h, locale, TextKey::ApplyGroupAndStart);
        let apply = h.state_mut().1.pop().unwrap();
        crate::app::reduce(&mut h.state_mut().0, apply);
        h.run();
        assert!(matches!(h.state().0.stream, StreamState::Starting { .. }));
        group_button(&mut h, locale, TextKey::Cancel);
        assert_eq!(h.state().1, vec![AppEvent::StopRequested]);
        let stop = h.state_mut().1.pop().unwrap();
        crate::app::reduce(&mut h.state_mut().0, stop);
        h.run();
        assert!(matches!(h.state().0.stream, StreamState::Stopping { .. }));
    }

    #[test]
    fn release_groups_start_during_stopping_owns_a_new_cancellable_generation() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        h.state_mut().0.generations.session = GenerationId(7);
        h.state_mut().0.stream = StreamState::Stopping {
            generation: GenerationId(7),
        };
        h.run();
        group_button(&mut h, locale, TextKey::ApplyGroupAndStart);
        let apply = h.state_mut().1.pop().unwrap();
        let result = crate::app::reduce(&mut h.state_mut().0, apply);
        assert_eq!(
            h.state().0.stream,
            StreamState::Starting {
                generation: GenerationId(8)
            }
        );
        assert!(matches!(
            &result.effects[0],
            crate::app::AppEffect::Group {
                generation: Some(GenerationId(8)),
                ..
            }
        ));
        crate::app::reduce(
            &mut h.state_mut().0,
            AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: GenerationId(7),
            }),
        );
        assert_eq!(
            h.state().0.stream,
            StreamState::Starting {
                generation: GenerationId(8)
            }
        );
        h.run();
        group_button(&mut h, locale, TextKey::Cancel);
        let cancel = h.state_mut().1.pop().unwrap();
        assert_eq!(cancel, AppEvent::StopRequested);
        crate::app::reduce(&mut h.state_mut().0, cancel);
        assert_eq!(
            h.state().0.stream,
            StreamState::Stopping {
                generation: GenerationId(9)
            }
        );
    }

    #[test]
    fn groups_name_validation_blocks_duplicates_and_limits_names_to_64_characters() {
        let locale = ResolvedLocale::English;
        let mut h = groups_surface(locale, PLAIN);
        group_button(&mut h, locale, TextKey::CreateGroup);
        group_name(&mut h, locale, "evening");
        h.get_by_role_and_label(accesskit::Role::CheckBox, "Kitchen")
            .click_accesskit();
        h.run();
        group_button(&mut h, locale, TextKey::SaveGroup);
        assert!(h.state().1.is_empty());
        h.get_by_label(Catalog::new(locale).text(TextKey::GroupNameDuplicate));
        group_name(&mut h, locale, &"a".repeat(65));
        group_button(&mut h, locale, TextKey::SaveGroup);
        let AppEvent::GroupRequested(crate::app::GroupCommand::Save { name, .. }) = &h.state().1[0]
        else {
            panic!("save")
        };
        assert_eq!(name, &"a".repeat(64));
    }

    #[test]
    fn groups_every_failure_keeps_the_editor_and_uses_localized_feedback() {
        for locale in LOCALES {
            for (failure, key) in [
                (crate::app::GroupFailure::Busy, TextKey::GroupBusy),
                (crate::app::GroupFailure::Closed, TextKey::GroupClosed),
                (
                    crate::app::GroupFailure::Validation,
                    TextKey::GroupValidationFailed,
                ),
                (
                    crate::app::GroupFailure::Persistence,
                    TextKey::GroupPersistenceFailed,
                ),
                (
                    crate::app::GroupFailure::ConfirmationLost,
                    TextKey::GroupUnconfirmed,
                ),
            ] {
                let mut h = groups_surface(locale, LOOKS[5]);
                group_button(&mut h, locale, TextKey::EditGroup);
                group_button(&mut h, locale, TextKey::SaveGroup);
                group_result(&mut h, crate::app::GroupOperationStatus::Failed(failure));
                h.get_by_role_and_label(
                    accesskit::Role::TextInput,
                    Catalog::new(locale).text(TextKey::GroupName),
                );
                let feedback = h.get_by_label(Catalog::new(locale).text(key));
                assert!(
                    feedback.rect().bottom() <= NARROW[1],
                    "error must be visible without scrolling: {:?}",
                    feedback.rect()
                );
                assert_eq!(
                    h.state().0.saved_groups.as_ref().unwrap()[0].members[2].name,
                    "Office"
                );
            }
        }
    }

    /// Opt-in GPU render of the real shell with synthetic receiver data only.
    /// Never starts capture, discovery, or a hardware session.
    #[test]
    #[ignore = "manual visual QA; requires a GPU adapter"]
    fn render_receiver_level_visual_review() {
        let mut state = base();
        state.receivers.push(receiver(
            3,
            "Living room",
            "AudioAccessory1,1",
            Availability::Available,
        ));
        state.receiver_levels = Some(std::collections::BTreeMap::from([
            (rid(1), 0.3),
            (rid(2), 0.65),
            (rid(3), 1.0),
        ]));
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        for (index, look) in LOOKS.iter().enumerate() {
            for (name, page) in [("overview", Page::Home), ("speakers", Page::Speakers)] {
                let mut harness = surface(&state, page, ResolvedLocale::German, *look);
                harness.run();
                let image = harness.render().expect("render actual egui shell");
                image
                    .save(directory.join(format!("receiver-volume-{name}-{index}.png")))
                    .unwrap();
                assert!(
                    harness.state().is_empty(),
                    "rendering must send no commands: {:?}",
                    harness.state()
                );
            }
        }
    }

    /// Opt-in synthetic visual review for the saved-groups page. No native
    /// window, devices, discovery, capture, or playback are involved.
    #[test]
    #[ignore = "manual visual QA; requires a GPU adapter"]
    fn render_saved_groups_visual_review() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let mut empty = base();
        empty.saved_groups = Some(Vec::new());
        for (name, state, look) in [
            ("groups-empty", empty, LOOKS[2]),
            ("groups-list", base(), LOOKS[0]),
            ("groups-list-small", base(), LOOKS[2]),
        ] {
            let mut harness = surface(&state, Page::Groups, ResolvedLocale::German, look);
            harness.run();
            harness
                .render()
                .expect("render actual saved-groups shell")
                .save(directory.join(format!("{name}.png")))
                .unwrap();
            assert!(
                harness.state().is_empty(),
                "rendering must send no commands"
            );
        }
        let mut editor = groups_surface(ResolvedLocale::German, LOOKS[2]);
        group_button(&mut editor, ResolvedLocale::German, TextKey::EditGroup);
        editor
            .render()
            .expect("render actual saved-groups editor")
            .save(directory.join("groups-editor.png"))
            .unwrap();
        let mut failed = groups_surface(ResolvedLocale::German, LOOKS[5]);
        group_button(&mut failed, ResolvedLocale::German, TextKey::EditGroup);
        group_button(&mut failed, ResolvedLocale::German, TextKey::SaveGroup);
        group_result(
            &mut failed,
            crate::app::GroupOperationStatus::Failed(crate::app::GroupFailure::ConfirmationLost),
        );
        failed.get_by_label(Catalog::new(ResolvedLocale::German).text(TextKey::GroupUnconfirmed));
        failed
            .render()
            .expect("real editor with associated error")
            .save(directory.join("groups-error-contrast.png"))
            .unwrap();
        let mut active = groups_surface(ResolvedLocale::German, LOOKS[1]);
        active.state_mut().0.stream = StreamState::Streaming {
            generation: GenerationId(3),
        };
        active.run();
        group_button(&mut active, ResolvedLocale::German, TextKey::EditGroup);
        group_button(&mut active, ResolvedLocale::German, TextKey::SaveGroup);
        let save = active.state_mut().1.pop().unwrap();
        crate::app::reduce(&mut active.state_mut().0, save);
        active.run();
        active.get_by_role_and_label(
            accesskit::Role::Button,
            Catalog::new(ResolvedLocale::German).text(TextKey::StopStreaming),
        );
        active
            .render()
            .expect("active session keeps its Stop header during save")
            .save(directory.join("groups-active-pending.png"))
            .unwrap();
    }

    #[test]
    fn compact_volume_controls_stay_inside_the_everyday_viewports() {
        let mut state = base();
        state.receivers.push(receiver(
            3,
            "Living room",
            "AudioAccessory1,1",
            Availability::Available,
        ));
        state.receiver_levels = Some(std::collections::BTreeMap::from([
            (rid(1), 0.3),
            (rid(2), 0.65),
            (rid(3), 1.0),
        ]));

        let overview = surface(&state, Page::Home, ResolvedLocale::German, LOOKS[0]);
        let master = overview
            .get_by_label("Gesamtlautstärke für alle ausgewählten Lautsprecher")
            .rect();
        assert!(
            master.bottom() <= WIDE[1],
            "master slider ends at {}, outside the 1120x720 viewport",
            master.bottom()
        );

        let speakers = surface(&state, Page::Speakers, ResolvedLocale::German, LOOKS[2]);
        let third = speakers.get_by_label("Lautstärke: Studio").rect();
        assert!(
            third.bottom() <= NARROW[1],
            "third speaker slider ends at {}, outside the 900x600 viewport",
            third.bottom()
        );
    }

    /// The whole client area, drawn exactly as [`crate::ui::ControlCenterApp`]
    /// draws it: one snapshot, one resolved style, one catalog, one derived
    /// Change Bar, one `show_shell`.
    ///
    /// Nothing here reimplements the shell's composition. If the shell ever
    /// starts deriving its chrome differently, this stops matching it and the
    /// mismatch is the point.
    fn surface(state: &AppState, page: Page, locale: ResolvedLocale, look: Look) -> Surface {
        let mut state = state.clone();
        state.page = page;
        let Look {
            theme,
            high_contrast,
            size,
            ..
        } = look;
        let mut harness: Surface = egui_kittest::Harness::new_ui_state(
            move |ui, log: &mut Vec<AppEvent>| {
                let resolved = theme::resolve_theme(theme, None, appearance(high_contrast));
                theme::apply_theme(ui.ctx(), &resolved);
                let resources = UiResources {
                    tokens: &resolved.tokens,
                    catalog: Catalog::new(locale),
                };
                let snapshot = UiSnapshot::from_state(&state);
                let bar = change_bar_model(&snapshot, resources.catalog);
                let mut emit = |event: AppEvent| log.push(event);
                show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut emit);
            },
            Vec::new(),
        );
        harness.set_size(egui::vec2(size[0], size[1]));
        harness.step();
        harness.step();
        harness
    }

    /// One interactive element as the accessibility tree exposes it.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct Control {
        name: String,
        role: accesskit::Role,
    }

    /// Every element the keyboard can land on.
    ///
    /// Focusability is the criterion rather than clickability, and the two are
    /// genuinely different here: egui advertises no `Click` action on a
    /// `Slider` (its keyboard contract is increment/decrement) and it
    /// advertises `Click` on a scroll bar that the keyboard never visits. What
    /// a user reaches with Tab is the focusable set, and that is the set every
    /// claim about names, order, and effect below is made over.
    fn controls(harness: &Surface) -> Vec<Control> {
        harness
            .root()
            .children_recursive()
            .filter(|node| {
                node.accesskit_node()
                    .data()
                    .supports_action(accesskit::Action::Focus)
            })
            .map(|node| {
                let raw = node.accesskit_node();
                Control {
                    name: raw.label().unwrap_or_default(),
                    role: raw.role(),
                }
            })
            .collect()
    }

    /// Every string the accessibility tree currently carries.
    ///
    /// egui reports a `Label`'s text as the node's *value* and every other
    /// widget's text as its *label*, so a claim about "what a screen reader
    /// would say" has to read both.
    fn announced(harness: &Surface) -> Vec<String> {
        harness
            .root()
            .children_recursive()
            .flat_map(|node| {
                let raw = node.accesskit_node();
                [raw.label(), raw.value()]
            })
            .flatten()
            .collect()
    }

    /// Whether `needle` occurs in `haystack` as a whole word.
    ///
    /// Substring matching alone is useless across languages: "On" occurs
    /// inside "Version", and "Aus" inside "Auswahl". The boundary check is
    /// what makes a cross-language claim possible at all.
    fn contains_word(haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        let mut from = 0;
        while let Some(offset) = haystack[from..].find(needle) {
            let start = from + offset;
            let end = start + needle.len();
            let before_ok = haystack[..start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric());
            let after_ok = haystack[end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_alphanumeric());
            if before_ok && after_ok {
                return true;
            }
            from = end;
        }
        false
    }

    // ------------------------------------------- reading the declarations

    /// The members declared at the top level of one `enum` or `struct` block.
    ///
    /// A test that classifies every variant of an enum, or every field of a
    /// struct, has to be bound to the declaration itself or it is decoration.
    /// The compiler binds the *naming* -- a wildcard-free match will not build
    /// against a new variant -- but nothing in the language binds a list or a
    /// count written beside it, and a `field: _` in a destructuring pattern
    /// binds nothing at all. Reading the declaration out of the source is the
    /// cheapest thing that does bind, and it needs no dependency and no change
    /// to the code under test.
    ///
    /// Deliberately simple: it understands one-member-per-line declarations,
    /// doc comments, and attributes, which is what both declarations it is
    /// pointed at look like. It cannot silently under-report, because every
    /// caller compares the whole set of names against a hand-written set and
    /// prints both sides on failure.
    fn members_declared_in(source: &str, header: &str) -> Vec<String> {
        let start = source.find(header).unwrap_or_else(|| {
            panic!("`{header}` is no longer declared where this matrix looks for it")
        });
        let body = &source[start + header.len()..];

        let mut members = Vec::new();
        let mut depth = 0i32;
        for line in body.lines() {
            let code = line.split("//").next().unwrap_or_default();
            let trimmed = code.trim();
            let opens = code.matches(['{', '(', '[']).count() as i32;
            let closes = code.matches(['}', ')', ']']).count() as i32;

            if depth == 0 {
                if trimmed.starts_with('}') {
                    break;
                }
                if !trimmed.is_empty() && !trimmed.starts_with("#[") {
                    let name = trimmed
                        .trim_start_matches("pub(crate) ")
                        .trim_start_matches("pub ")
                        .split(|character: char| !character.is_alphanumeric() && character != '_')
                        .find(|token| !token.is_empty())
                        .unwrap_or_default();
                    members.push(name.to_string());
                }
            }

            depth += opens - closes;
        }
        members
    }

    // -------------------------------------------------- event classification

    /// What one [`AppEvent`] variant is, as far as the window is concerned.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Classification {
        /// Some control, key, or window-manager interaction produces it. That
        /// is a promise, and the matrix discharges it by driving the whole
        /// surface and demanding the event actually come out.
        SentByTheWindow,
        /// It reaches the app from somewhere other than the window, with the
        /// reason written down rather than left to be inferred from silence.
        /// The claim is wrong the moment the surface grows a control for it,
        /// so the matrix also fails if an excluded variant *is* observed.
        NeverFromTheWindow(&'static str),
    }

    /// The variant's own name and its classification, decided in one arm.
    ///
    /// The match has no wildcard arm, and naming and classifying share an arm,
    /// both on purpose: a new [`AppEvent`] variant does not compile until
    /// someone has said in the same breath what it is called and whether the
    /// window can send it.
    ///
    /// An earlier shape of this module split the two -- a name here, a
    /// membership in a hand-written list over there, and a hand-written
    /// variant count to tie them together -- and that shape was vacuous. A
    /// twenty-second variant satisfied the compiler by being named, the count
    /// of the two lists still matched the count literal, and the variant
    /// passed unclassified. That is precisely how `LocaleChanged` and
    /// `AdvancedInformationChanged` lived for six tasks with no renderer
    /// sending them, so a matrix that cannot catch it is worth nothing.
    fn classify(event: &AppEvent) -> (&'static str, Classification) {
        use Classification::{NeverFromTheWindow, SentByTheWindow};
        match event {
            AppEvent::GroupRequested(_) => ("GroupRequested", SentByTheWindow),
            AppEvent::Navigate(_) => ("Navigate", SentByTheWindow),
            AppEvent::MainWindowCloseRequested => ("MainWindowCloseRequested", SentByTheWindow),
            AppEvent::ShowMainWindow => (
                "ShowMainWindow",
                NeverFromTheWindow(
                    "the tray is the only surface a hidden window has; \
                     crate::tray::menu_dispatch sends it",
                ),
            ),
            AppEvent::ToggleStagedReceiver(_) => ("ToggleStagedReceiver", SentByTheWindow),
            AppEvent::ApplyStagedReceivers => ("ApplyStagedReceivers", SentByTheWindow),
            AppEvent::DiscardStagedReceivers => ("DiscardStagedReceivers", SentByTheWindow),
            AppEvent::StartRequested => ("StartRequested", SentByTheWindow),
            AppEvent::StopRequested => ("StopRequested", SentByTheWindow),
            AppEvent::GlobalHotkeyPressed => (
                "GlobalHotkeyPressed",
                NeverFromTheWindow(
                    "the chord is registered with Windows and arrives on the hotkey \
                     message thread; crate::platform::hotkey sends it",
                ),
            ),
            AppEvent::RefreshRequested => ("RefreshRequested", SentByTheWindow),
            AppEvent::MasterVolumeChanged(_) => ("MasterVolumeChanged", SentByTheWindow),
            AppEvent::ReceiverLevelRequested { .. } => ("ReceiverLevelRequested", SentByTheWindow),
            AppEvent::MuteChangeRequested(_) => ("MuteChangeRequested", SentByTheWindow),
            AppEvent::LatencyChoiceRequested(_) => ("LatencyChoiceRequested", SentByTheWindow),
            AppEvent::AudioEndpointRequested(_) => ("AudioEndpointRequested", SentByTheWindow),
            AppEvent::ThemeChanged(_) => ("ThemeChanged", SentByTheWindow),
            AppEvent::LocaleChanged(_) => ("LocaleChanged", SentByTheWindow),
            AppEvent::AdvancedInformationChanged(_) => {
                ("AdvancedInformationChanged", SentByTheWindow)
            }
            AppEvent::WindowsDisplayLanguageChanged(_) => (
                "WindowsDisplayLanguageChanged",
                NeverFromTheWindow(
                    "a WM_SETTINGCHANGE reading, not a user choice; \
                     crate::platform::windows_settings sends it",
                ),
            ),
            AppEvent::RetryPreferencesPersistence => {
                ("RetryPreferencesPersistence", SentByTheWindow)
            }
            AppEvent::HotkeyChanged(_) => ("HotkeyChanged", SentByTheWindow),
            AppEvent::WindowGeometryChanged(_) => ("WindowGeometryChanged", SentByTheWindow),
            AppEvent::Controller(_) => (
                "Controller",
                NeverFromTheWindow("discovery and session outcomes from the device service"),
            ),
            AppEvent::Preferences(_) => (
                "Preferences",
                NeverFromTheWindow("load and write outcomes from the debounced settings worker"),
            ),
            AppEvent::QuitRequested => ("QuitRequested", SentByTheWindow),
            AppEvent::HardwareCheck(_) => ("HardwareCheck", SentByTheWindow),
        }
    }

    /// The variant's own name, and nothing about its payload.
    fn variant_name(event: &AppEvent) -> &'static str {
        classify(event).0
    }

    /// One value per [`AppEvent`] variant.
    ///
    /// Hand-written, and bound to the enum by
    /// `every_event_is_classified_exactly_once`, which reads the variant names
    /// straight out of `app/event.rs` and demands exactly this set back. The
    /// payloads are arbitrary: only which variant it is matters here.
    fn every_variant() -> Vec<AppEvent> {
        vec![
            AppEvent::GroupRequested(crate::app::GroupCommand::Delete(crate::app::GroupId(
                uuid::Uuid::nil(),
            ))),
            AppEvent::Navigate(Page::Home),
            AppEvent::MainWindowCloseRequested,
            AppEvent::ShowMainWindow,
            AppEvent::ToggleStagedReceiver(rid(1)),
            AppEvent::ApplyStagedReceivers,
            AppEvent::DiscardStagedReceivers,
            AppEvent::StartRequested,
            AppEvent::StopRequested,
            AppEvent::GlobalHotkeyPressed,
            AppEvent::RefreshRequested,
            AppEvent::MasterVolumeChanged(0.5),
            AppEvent::ReceiverLevelRequested {
                receiver: rid(1),
                level: 0.5,
            },
            AppEvent::MuteChangeRequested(true),
            AppEvent::LatencyChoiceRequested(LatencyChoice::Normal),
            AppEvent::AudioEndpointRequested(AudioEndpointRequest::SystemDefault),
            AppEvent::ThemeChanged(ThemePreference::Light),
            AppEvent::LocaleChanged(LocalePreference::System),
            AppEvent::AdvancedInformationChanged(true),
            AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::English),
            AppEvent::RetryPreferencesPersistence,
            AppEvent::HotkeyChanged(HotkeyBinding::default()),
            AppEvent::WindowGeometryChanged(WindowGeometry::default()),
            AppEvent::Controller(ControllerEvent::SessionStopped {
                generation: GenerationId::default(),
            }),
            AppEvent::Preferences(PreferencesEvent::LoadFailed {
                summary: String::new(),
            }),
            AppEvent::QuitRequested,
            AppEvent::HardwareCheck(crate::app::hardware_check::HardwareCheckAction::Open),
        ]
    }

    /// Every event a user can cause from the window itself.
    ///
    /// Derived from [`classify`] rather than maintained beside it, so the list
    /// and the classification cannot disagree.
    fn surface_sends() -> Vec<&'static str> {
        every_variant()
            .iter()
            .filter_map(|event| match classify(event) {
                (name, Classification::SentByTheWindow) => Some(name),
                (_, Classification::NeverFromTheWindow(_)) => None,
            })
            .collect()
    }

    /// Every event that reaches the app from somewhere other than the window,
    /// paired with the reason it has no control.
    fn never_from_the_window() -> Vec<(&'static str, &'static str)> {
        every_variant()
            .iter()
            .filter_map(|event| match classify(event) {
                (name, Classification::NeverFromTheWindow(reason)) => Some((name, reason)),
                (_, Classification::SentByTheWindow) => None,
            })
            .collect()
    }

    // ------------------------------------------------------- driving helpers

    /// Activates one control the way an assistive technology does.
    ///
    /// Through AccessKit rather than through a synthetic pointer, and that is
    /// the honest choice for this matrix rather than a convenience: a control
    /// that has scrolled below the fold still has a rectangle, and a pointer
    /// aimed at it lands on the clip. Driving the same `Action::Click` a
    /// screen reader sends reaches every control of a destination without the
    /// matrix having to reproduce a scroll gesture per control -- and it
    /// exercises the path this window is meant to support anyway. Real mouse
    /// scrolling and clicking is covered separately, by the Shell's own
    /// `the_bar_keeps_its_place_while_the_page_content_scrolls`.
    ///
    /// The slider is the one exception. Its value comes from *where* the
    /// pointer landed, so an activation without a position cannot move it,
    /// and it is driven with a real click.
    ///
    /// Queried by role *and* name: a group heading and the control it
    /// introduces legitimately carry the same words -- "Advanced information"
    /// is a section title and a check box -- and a name-only lookup cannot
    /// tell a `Label` from the `CheckBox` beneath it.
    fn activate(harness: &mut Surface, control: &Control) -> Vec<AppEvent> {
        activate_with_output(harness, control).0
    }

    /// Four synthetic screenshots; no service, native capture or playback.
    #[test]
    #[ignore = "manual visual QA; requires a GPU adapter"]
    fn render_hardware_check_visual_review() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        for (name, locale, look, listening) in [
            (
                "hardware-check-prepare-de-narrow-light",
                ResolvedLocale::German,
                LOOKS[2],
                false,
            ),
            (
                "hardware-check-prepare-en-wide-dark",
                ResolvedLocale::English,
                LOOKS[1],
                false,
            ),
            (
                "hardware-check-listening-de-wide-light",
                ResolvedLocale::German,
                LOOKS[0],
                true,
            ),
            (
                "hardware-check-listening-en-narrow-dark",
                ResolvedLocale::English,
                LOOKS[3],
                true,
            ),
        ] {
            let mut state = crate::app::hardware_check::tests::prepared();
            if listening {
                crate::app::reduce(
                    &mut state,
                    AppEvent::HardwareCheck(crate::app::hardware_check::HardwareCheckAction::Start),
                );
                let generation = state.generations.session;
                let active_receiver_ids = state.desired_receivers.iter().cloned().collect();
                crate::app::reduce(
                    &mut state,
                    AppEvent::Controller(ControllerEvent::SessionStarted {
                        generation,
                        active_receiver_ids,
                    }),
                );
            }
            let mut harness = surface(&state, Page::Diagnostics, locale, look);
            harness.run();
            harness
                .render()
                .expect("render synthetic hardware check")
                .save(directory.join(format!("{name}.png")))
                .unwrap();
            assert!(
                harness.state().is_empty(),
                "rendering dispatches no commands"
            );
        }
    }

    /// Idle, healthy, and warning-rich diagnostics rendered without devices or playback.
    #[test]
    #[ignore = "manual visual QA; requires a GPU adapter"]
    fn render_live_diagnostics_visual_review() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let generation = GenerationId(21);
        let reading = |warning: bool| LiveDiagnosticsReading {
            generation,
            input_peak_per_mille: Some(420),
            capture_drops_total: u64::from(warning),
            buffer: Some(BufferDiagnosticsReading {
                queued_frames: 4,
                capacity_frames: 12,
                buffered_ms: 80,
                underruns: if warning { 2 } else { 0 },
            }),
            receivers: vec![ReceiverDiagnosticsReading {
                id: rid(1),
                state: if warning {
                    DiagnosticReceiverState::Recovering
                } else {
                    DiagnosticReceiverState::Streaming
                },
                transport: Some(TransportDiagnosticsReading {
                    packets_accepted: 8_420,
                    bytes_accepted: 2_694_400,
                    send_failures: u64::from(warning),
                    retransmit_requests: if warning { 7 } else { 0 },
                }),
                issue: warning.then_some(DiagnosticIssue::Transport),
            }],
        };

        let mut idle = surface(&base(), Page::Diagnostics, ResolvedLocale::German, LOOKS[2]);
        idle.run();
        idle.render()
            .expect("render idle live diagnostics")
            .save(directory.join("live-diagnostics-idle.png"))
            .unwrap();

        for (name, warning, look) in [
            ("live-diagnostics-healthy", false, LOOKS[0]),
            ("live-diagnostics-warning", true, LOOKS[5]),
        ] {
            let mut state = base();
            state.stream = StreamState::Streaming { generation };
            state.generations.session = generation;
            state.active_receivers = HashSet::from_iter([rid(1)]);
            state.live_diagnostics = Some(reading(warning));
            state.audio_source = Some(AudioSourceReading {
                endpoints_known: true,
                refresh_failed: false,
                endpoints: vec![AudioEndpointChoice {
                    key: AudioEndpointKey(1),
                    name: "Synthetic speakers".into(),
                }],
                selection: AudioEndpointSelection::SystemDefault,
                captured_key: Some(AudioEndpointKey(1)),
                captured_name: Some("Synthetic speakers".into()),
                state: if warning {
                    CaptureState::Recovering
                } else {
                    CaptureState::Capturing
                },
            });
            let mut harness = surface(&state, Page::Diagnostics, ResolvedLocale::English, look);
            harness.run();
            harness.get_by_label("Show detailed counters").click();
            harness.run();
            harness
                .render()
                .expect("render populated live diagnostics")
                .save(directory.join(format!("{name}.png")))
                .unwrap();

            let counter_before = harness
                .get_by_label_contains("Packets accepted by local OS")
                .rect();
            let target_y = look.size[1] * 0.65;
            let scroll_points = (counter_before.center().y - target_y).max(0.0);
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(egui::pos2(
                    look.size[0] / 2.0,
                    look.size[1] / 2.0,
                )));
            harness.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -scroll_points),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            harness.step();
            harness
                .render()
                .expect("render scrolled live diagnostic counters")
                .save(directory.join(format!("{name}-counters.png")))
                .unwrap();
            let counter = harness
                .get_by_label_contains("Packets accepted by local OS")
                .rect();
            assert!(
                counter.top() < look.size[1] && counter.bottom() > 0.0,
                "scroll did not expose the receiver counters: {counter:?}"
            );
            assert!(harness.state().is_empty(), "visual review emitted an event");
        }
    }

    fn activate_with_output(
        harness: &mut Surface,
        control: &Control,
    ) -> (Vec<AppEvent>, Vec<egui::OutputCommand>) {
        harness.state_mut().clear();
        let node = harness.get_by_role_and_label(control.role, &control.name);
        if control.role == accesskit::Role::Slider {
            node.click();
        } else {
            node.click_accesskit();
        }
        // Two frames, not one: the first consumes the activation, and a
        // control whose whole effect is a change to this window's own
        // transient state -- the About viewer opening -- only draws that
        // change on the next.
        harness.step();
        // Platform commands are one-shot output: preserve the activation
        // frame before the following repaint clears it.
        let mut commands = harness.output().platform_output.commands.clone();
        harness.step();
        commands.extend(harness.output().platform_output.commands.clone());
        (harness.state().clone(), commands)
    }

    /// Every event the whole surface produces when each of its controls is
    /// activated once, across every scenario and every destination.
    ///
    /// A fresh window per control, because activation is not always
    /// commutative: opening the About viewer changes what the next control
    /// would be, and a matrix that let one click bleed into the next would be
    /// asserting over a surface no user ever sees.
    /// Driving every control of every destination in every state is the
    /// expensive part of this module, and two assertions need the same answer,
    /// so it is computed once. `OnceLock` and not a `static mut`: the test
    /// harness runs the two on different threads.
    fn events_the_window_produces() -> &'static HashSet<&'static str> {
        static DRIVEN: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
        DRIVEN.get_or_init(drive_every_control)
    }

    fn drive_every_control() -> HashSet<&'static str> {
        let mut produced = HashSet::new();
        // The same destination, role, and announced name is the same rendered
        // control, and driving it a second time in another lifecycle state
        // adds nothing: the six rail destinations alone would otherwise be
        // driven sixty times each. Anything a state really changes shows up as
        // a different name -- a receiver row carries its own state words -- or
        // as a control that only that state draws.
        let mut already = HashSet::new();
        for scenario in scenarios() {
            for &page in DESTINATIONS.iter() {
                let listed: Vec<Control> = {
                    let harness = surface(&scenario.state, page, ResolvedLocale::English, PLAIN);
                    controls(&harness)
                        .into_iter()
                        .filter(|control| control.role != accesskit::Role::TextInput)
                        .filter(|control| {
                            already.insert((
                                crate::ui::pages::destination_index(page),
                                control.clone(),
                            ))
                        })
                        .collect()
                };
                for control in listed {
                    let mut harness =
                        surface(&scenario.state, page, ResolvedLocale::English, PLAIN);
                    for event in activate(&mut harness, &control) {
                        produced.insert(variant_name(&event));
                    }
                }
            }
        }
        produced.extend(events_from_editing_the_shortcut());
        produced
    }

    /// The shortcut Apply command is enabled only by a draft that differs from
    /// the applied chord, so reaching `HotkeyChanged` needs a typed edit and
    /// not a click. The field is the one control on the surface whose effect
    /// is deferred to a second control.
    fn events_from_editing_the_shortcut() -> HashSet<&'static str> {
        let catalog = Catalog::new(ResolvedLocale::English);
        let mut harness = surface(&base(), Page::Settings, ResolvedLocale::English, PLAIN);
        let field = catalog.text(TextKey::ShortcutKeyAccessibility);
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, field)
            .focus();
        harness.step();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.step();
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, field)
            .type_text("77");
        harness.step();
        harness.step();

        harness.state_mut().clear();
        harness
            .get_by_role_and_label(
                accesskit::Role::Button,
                catalog.text(TextKey::ApplyShortcut),
            )
            .click_accesskit();
        harness.step();
        harness.step();
        harness
            .state()
            .iter()
            .map(variant_name)
            .collect::<HashSet<_>>()
    }

    // ------------------------------------------------------ the whole window

    /// A real [`crate::ui::ControlCenterApp`] on a real actor, so the claims
    /// the window itself owns -- the close request and the live geometry --
    /// are made against the same `eframe::App` hooks the event loop calls.
    struct Window {
        harness: egui_kittest::Harness<'static, crate::ui::ControlCenterApp>,
        handle: crate::app_handle::AppHandle,
        _runtime: crate::app_handle::AppRuntime,
    }

    /// Answers `BeginShutdown` the way the real coordinator does, so the
    /// close path can be driven end to end without a device service.
    #[derive(Clone, Default)]
    struct QuietExecutor;

    impl crate::app_handle::EffectExecutor for QuietExecutor {
        fn execute(
            &mut self,
            effect: crate::app::AppEffect,
            feedback: &crate::app_handle::AppFeedback,
        ) {
            match effect {
                crate::app::AppEffect::ShowMainWindow => {
                    feedback
                        .ui_effects
                        .send(crate::app::UiEffect::ShowMainWindow);
                }
                crate::app::AppEffect::HideMainWindow => {
                    feedback
                        .ui_effects
                        .send(crate::app::UiEffect::HideMainWindow);
                }
                crate::app::AppEffect::BeginShutdown => {
                    feedback
                        .ui_effects
                        .send(crate::app::UiEffect::CloseApplication);
                }
                _ => {}
            }
        }
    }

    fn window(initial: AppState) -> Window {
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel(256);
        let (handle, runtime) = crate::app_handle::start_app_with_channel(
            initial,
            Box::new(QuietExecutor),
            event_tx,
            event_rx,
        );
        let for_app = handle.clone();
        let harness = egui_kittest::Harness::builder().build_eframe(move |cc| {
            crate::ui::ControlCenterApp::new(
                cc,
                for_app.clone(),
                crate::platform::WindowsSettingsMonitor::detached(
                    crate::platform::WindowsSettingsSnapshot {
                        locale: ResolvedLocale::English,
                        appearance: appearance(false),
                    },
                ),
                None,
            )
        });
        Window {
            harness,
            handle,
            _runtime: runtime,
        }
    }

    /// How many frames a window test pumps while the actor thread catches up.
    ///
    /// A frame budget and not a deadline: the actor runs on its own thread, so
    /// a wall-clock bound would tighten exactly when that thread is slowest to
    /// be scheduled, which turns a correct window into a red test on a busy
    /// machine. Nothing here asserts how long anything takes.
    const MAX_FRAMES: usize = 5_000;

    fn step_until(window: &mut Window, what: &str, mut done: impl FnMut(&UiSnapshot) -> bool) {
        for _ in 0..MAX_FRAMES {
            if done(&window.handle.snapshot()) {
                return;
            }
            window.harness.step();
            std::thread::yield_now();
        }
        panic!("{what} never happened in {MAX_FRAMES} frames");
    }

    fn request_close(window: &mut Window) {
        window
            .harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
    }

    fn report_size(window: &mut Window, size: egui::Vec2, maximized: bool) {
        let viewport = window
            .harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default();
        viewport.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(64.0, 48.0), size));
        viewport.maximized = Some(maximized);
    }

    // ============================================================== the tests

    mod events {
        use super::*;

        /// Every variant is either something the window sends or something it
        /// deliberately does not, and no variant is both or neither.
        ///
        /// No component can make this claim: a renderer knows the events it
        /// sends and nothing about the ones it does not.
        ///
        /// The binding to the enum is the point, and it is made twice. The
        /// wildcard-free match in [`classify`] means a new variant cannot be
        /// named without being classified in the same arm; the scan of
        /// `app/event.rs` below means it cannot be left out of
        /// [`every_variant`] either. Neither half alone is enough -- a hand
        /// count beside the lists, which is what this test used to compare
        /// against, is satisfied by a variant nobody classified.
        #[test]
        fn every_event_is_classified_exactly_once() {
            let mut declared =
                members_declared_in(include_str!("../app/event.rs"), "pub enum AppEvent {");
            declared.sort();
            assert!(
                declared.len() > 1,
                "the scan of app/event.rs found {declared:?}, which cannot be right; \
                 the declaration moved and this matrix is no longer bound to it"
            );

            let mut driven: Vec<String> = every_variant()
                .iter()
                .map(|event| variant_name(event).to_string())
                .collect();
            driven.sort();

            assert_eq!(
                driven, declared,
                "AppEvent's variants and the variants this matrix classifies have \
                 drifted apart; a new variant needs a control or a written reason \
                 for having none"
            );

            let sends: HashSet<&str> = surface_sends().into_iter().collect();
            let never: HashSet<&str> = never_from_the_window()
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            assert!(
                !sends.is_empty() && !never.is_empty(),
                "one of the two categories came out empty, so classify no longer \
                 discriminates"
            );
            assert!(
                sends.is_disjoint(&never),
                "an event is both sent and excluded: {:?}",
                sends.intersection(&never).collect::<Vec<_>>()
            );

            for (name, reason) in never_from_the_window() {
                assert!(
                    !reason.trim().is_empty(),
                    "{name} is excluded without a reason"
                );
            }
        }

        /// Every event the window promises to send is actually produced by
        /// driving the window.
        ///
        /// This is the assertion the tray defect would have failed: the menu
        /// model was complete and tested, and nothing drove it. Here the whole
        /// surface is driven -- every control of every destination in every
        /// lifecycle state -- and an event with no control behind it shows up
        /// as a name the drive never produced.
        #[test]
        fn every_event_the_window_promises_has_a_control_behind_it() {
            let mut produced = events_the_window_produces().clone();

            // The two window-manager paths and the live geometry are not
            // widgets, so they are driven through the real `eframe::App`.
            produced.extend(events_the_window_manager_produces());

            let missing: Vec<&'static str> = surface_sends()
                .into_iter()
                .filter(|name| !produced.contains(name))
                .collect();
            assert!(
                missing.is_empty(),
                "the window promises to send these events and no control, key, \
                 or window-manager interaction produces them: {missing:?}"
            );
        }

        /// The window must not send what it declared it never sends.
        #[test]
        fn the_window_sends_nothing_it_declared_it_never_sends() {
            let produced = events_the_window_produces();
            for (name, reason) in never_from_the_window() {
                assert!(
                    !produced.contains(&name),
                    "{name} is excluded because {reason}, but a control on the \
                     window sends it"
                );
            }
        }

        /// Closing and resizing are the window's own two events.
        fn events_the_window_manager_produces() -> HashSet<&'static str> {
            let mut produced = HashSet::new();

            // Close with close-to-tray on means hide, and that is
            // `MainWindowCloseRequested`.
            let mut hidden = window(AppState {
                window: WindowState {
                    close_to_tray: true,
                    ..WindowState::default()
                },
                ..AppState::default()
            });
            hidden.harness.step();
            request_close(&mut hidden);
            hidden.harness.step();
            step_until(&mut hidden, "the window hiding to the tray", |snapshot| {
                !snapshot.window.visible
            });
            produced.insert("MainWindowCloseRequested");

            // Close with it off means quit, and that is `QuitRequested`.
            let mut quitting = window(AppState {
                window: WindowState {
                    close_to_tray: false,
                    ..WindowState::default()
                },
                ..AppState::default()
            });
            quitting.harness.step();
            request_close(&mut quitting);
            quitting.harness.step();
            step_until(&mut quitting, "the bounded shutdown starting", |snapshot| {
                snapshot.shutting_down
            });
            produced.insert("QuitRequested");

            // A window the user resized has to tell the app its new size, or
            // the size it is reopened at can never be the size it was left at.
            let mut resized = window(AppState::default());
            resized.harness.step();
            report_size(&mut resized, egui::vec2(1004.0, 664.0), false);
            resized.harness.step();
            let reported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                step_until(&mut resized, "the resize reaching the app", |snapshot| {
                    snapshot.window.geometry.width == 1004.0
                        && snapshot.window.geometry.height == 664.0
                });
            }));
            if reported.is_ok() {
                produced.insert("WindowGeometryChanged");
            }

            produced
        }
    }

    mod preferences {
        use super::*;

        /// Every persisted field is either changeable from the window or
        /// carries a written reason for not being.
        ///
        /// The scan of `app/state.rs` is the mechanism, and the destructuring
        /// below is only its cheap first line of defence. A new [`Preferences`]
        /// field does not compile until the pattern mentions it -- but a
        /// developer answering that error with `new_field: _` has classified
        /// nothing, which is why the field names are also read out of the
        /// declaration and compared. Without that, a value can be written to
        /// disk, read back, and never once be reachable, which is how a whole
        /// persisted field ended up with a debounce rule tuned for it and no
        /// producer at all.
        #[test]
        fn every_persisted_field_is_reachable_from_the_window_or_says_why_not() {
            let value = Preferences::default();
            let Preferences {
                schema_version: _,
                hotkey: _,
                theme: _,
                locale: _,
                advanced_information: _,
                window: _,
                last_page: _,
                close_to_tray: _,
                launch_at_startup: _,
            } = value;

            /// Each persisted field paired with the event that changes it, or
            /// with the reason it has no control yet.
            const FIELDS: &[(&str, Result<&str, &str>)] = &[
                (
                    "schema_version",
                    Err("the file format's own version; migration owns it, not the user"),
                ),
                ("hotkey", Ok("HotkeyChanged")),
                ("theme", Ok("ThemeChanged")),
                ("locale", Ok("LocaleChanged")),
                ("advanced_information", Ok("AdvancedInformationChanged")),
                ("window", Ok("WindowGeometryChanged")),
                ("last_page", Ok("Navigate")),
                (
                    "close_to_tray",
                    Err("Settings has no Behavior group yet; section 7.6 defers it \
                         rather than shipping a switch with no effect"),
                ),
                (
                    "launch_at_startup",
                    Err("the same deferred Behavior group; it also needs a \
                         registry effect that does not exist"),
                ),
            ];

            let mut declared =
                members_declared_in(include_str!("../app/state.rs"), "pub struct Preferences {");
            declared.sort();
            assert!(
                declared.len() > 1,
                "the scan of app/state.rs found {declared:?}, which cannot be right; \
                 the declaration moved and this matrix is no longer bound to it"
            );

            let mut classified: Vec<String> = FIELDS
                .iter()
                .map(|(field, _)| (*field).to_string())
                .collect();
            classified.sort();
            assert_eq!(
                classified, declared,
                "Preferences' fields and the fields classified here have drifted \
                 apart; a persisted field needs a control or a written reason for \
                 having none"
            );

            let sends: HashSet<&str> = surface_sends().into_iter().collect();
            for (field, classification) in FIELDS {
                match classification {
                    Ok(event) => assert!(
                        sends.contains(event),
                        "{field} is persisted and its {event} is not something the \
                         window can send, so the value can be written and restored \
                         but never chosen"
                    ),
                    Err(reason) => assert!(
                        !reason.trim().is_empty(),
                        "{field} has no control and no reason"
                    ),
                }
            }
        }

        /// A value that is persisted has to be *used*, not merely stored.
        ///
        /// The other half of the same defect: the geometry was loaded,
        /// validated, and placed in `AppState`, and the window was then opened
        /// from a hard-coded size that ignored it. Reading it back out of the
        /// builder the launch path really constructs is the only way to state
        /// that the restored value reaches the window, because nothing between
        /// the two is observable from a snapshot.
        #[test]
        fn the_window_opens_at_the_size_it_was_last_left_at() {
            let left_at = WindowGeometry {
                x: Some(10.0),
                y: Some(20.0),
                width: 1004.0,
                height: 664.0,
                maximized: false,
            };
            let viewport = crate::app::main_viewport(left_at);
            assert_eq!(
                viewport.inner_size,
                Some(egui::vec2(1004.0, 664.0)),
                "the window ignores the size it was left at"
            );
            assert_eq!(
                viewport.min_inner_size,
                Some(egui::vec2(900.0, 600.0)),
                "the smallest supported window is still the floor"
            );
            assert_eq!(viewport.maximized, Some(false));

            let maximized = crate::app::main_viewport(WindowGeometry {
                maximized: true,
                ..left_at
            });
            assert_eq!(
                maximized.maximized,
                Some(true),
                "a window left maximized opens merely large"
            );

            // Everything above tests the builder. The defect was not in a
            // builder -- there was none -- but in `run()`, which built its
            // viewport from a hard-coded 1120 x 720 and ignored the restored
            // value entirely. `run()` opens a real window, so no test can call
            // it; what can be stated is that the one launch path still reaches
            // the restored geometry through this function rather than through
            // a literal. Without this, reverting `run()` leaves every
            // assertion above green, because the test itself keeps
            // `main_viewport` alive and even the dead-code warning stays
            // silent.
            let launch = include_str!("../app/mod.rs");
            assert_eq!(
                launch.matches("eframe::NativeOptions {").count(),
                1,
                "the launch options are no longer built in one place, so this \
                 guard may be reading the wrong one"
            );
            let options = &launch[launch
                .find("eframe::NativeOptions {")
                .expect("the launch path no longer builds NativeOptions")..];
            let viewport = options
                .lines()
                .find(|line| line.trim_start().starts_with("viewport:"))
                .expect("the launch options no longer set a viewport");
            assert!(
                viewport.contains("main_viewport("),
                "the launch path builds its viewport without main_viewport, so the \
                 restored size cannot reach the window: {}",
                viewport.trim()
            );
            assert!(
                viewport.contains(".window.geometry"),
                "the launch path builds its viewport from something other than the \
                 restored geometry: {}",
                viewport.trim()
            );
            assert!(
                !viewport.contains(|character: char| character.is_ascii_digit()),
                "the launch path still carries a literal size: {}",
                viewport.trim()
            );
        }
    }

    mod accessibility {
        use super::*;

        /// Every element the keyboard can reach announces something, in every
        /// language and every appearance.
        ///
        /// A component test proves one renderer sets one name. Only the whole
        /// window can state that no *combination* of theme, contrast, window
        /// size, and language leaves a control mute -- and the compact rail is
        /// exactly where that goes wrong, because it drops the painted label.
        #[test]
        fn every_reachable_control_announces_itself_everywhere() {
            // Exercise the same complete matrix in a persistent window, as
            // the application does. Recreating egui and its font atlas for
            // every snapshot dominated this check's runtime.
            let scenarios = scenarios();
            for look in LOOKS {
                for locale in LOCALES {
                    let resolved =
                        theme::resolve_theme(look.theme, None, appearance(look.high_contrast));
                    let mut harness = egui_kittest::Harness::new_ui_state(
                        move |ui, state: &mut AppState| {
                            theme::apply_theme(ui.ctx(), &resolved);
                            let resources = UiResources {
                                tokens: &resolved.tokens,
                                catalog: Catalog::new(locale),
                            };
                            let snapshot = UiSnapshot::from_state(state);
                            let bar = change_bar_model(&snapshot, resources.catalog);
                            show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut |_| {});
                        },
                        AppState::default(),
                    );
                    harness.set_size(egui::vec2(look.size[0], look.size[1]));
                    for scenario in &scenarios {
                        for &page in DESTINATIONS.iter() {
                            *harness.state_mut() = scenario.state.clone();
                            harness.state_mut().page = page;
                            harness.step();
                            harness.step();
                            for control in harness.root().children_recursive().filter(|node| {
                                node.accesskit_node()
                                    .data()
                                    .supports_action(accesskit::Action::Focus)
                            }) {
                                let node = control.accesskit_node();
                                assert!(
                                    node.label().is_some_and(|name| !name.trim().is_empty()),
                                    "{look} / {locale:?} / {} / {page:?}: a {:?} the \
                                     keyboard reaches announces nothing",
                                    scenario.name,
                                    node.role(),
                                    look = look.name,
                                );
                            }
                        }
                    }
                }
            }
        }

        /// No two reachable controls on one destination announce the same
        /// thing.
        ///
        /// This is the claim behind the Settings rule that a radio announces
        /// "Appearance: System" while it paints "System": a screen-reader user
        /// who hears one word twice cannot tell the two groups apart. The rule
        /// is a page-wide property, so no radio button can be the one to check
        /// it.
        #[test]
        fn no_destination_announces_one_name_twice() {
            for locale in LOCALES {
                for scenario in scenarios() {
                    for &page in DESTINATIONS.iter() {
                        let harness = surface(&scenario.state, page, locale, PLAIN);
                        let mut seen: HashMap<String, usize> = HashMap::new();
                        for control in controls(&harness) {
                            *seen.entry(control.name).or_default() += 1;
                        }
                        let repeated: Vec<(&String, &usize)> =
                            seen.iter().filter(|(_, count)| **count > 1).collect();
                        assert!(
                            repeated.is_empty(),
                            "{locale:?} / {} / {page:?}: two controls answer to the \
                             same name, so nothing spoken tells them apart: {repeated:?}",
                            scenario.name
                        );
                    }
                }
            }
        }

        /// Tab reaches every control of a destination and then comes back.
        ///
        /// The rail is drawn by the Shell, the body by a page, and the Change
        /// Bar by the Shell again. Whether those three compose into one
        /// complete cycle is a property of none of them, and a control that
        /// falls out of the cycle -- or one that swallows the focus -- is
        /// invisible to every component test.
        #[test]
        fn tab_visits_every_control_of_every_destination_and_returns() {
            for scenario in scenarios() {
                for &page in DESTINATIONS.iter() {
                    let mut harness =
                        surface(&scenario.state, page, ResolvedLocale::English, PLAIN);
                    let expected: HashSet<String> = controls(&harness)
                        .into_iter()
                        .map(|control| control.name)
                        .collect();
                    assert!(
                        !expected.is_empty(),
                        "{page:?} has no reachable control at all"
                    );

                    let mut visited: HashSet<String> = HashSet::new();
                    // Twice around: egui parks the focus once between the last
                    // control and the first, so one pass per control is one
                    // press short of a full cycle.
                    for _ in 0..(expected.len() * 2 + 2) {
                        harness.key_press(egui::Key::Tab);
                        harness.step();
                        let focused: Vec<String> = harness
                            .root()
                            .children_recursive()
                            .filter(|node| node.is_focused())
                            .filter_map(|node| node.accesskit_node().label())
                            .collect();
                        visited.extend(focused);
                    }

                    let unreachable: Vec<&String> =
                        expected.difference(&visited).collect::<Vec<_>>();
                    assert!(
                        unreachable.is_empty(),
                        "{} / {page:?}: Tab never reaches {unreachable:?}",
                        scenario.name
                    );
                }
            }
        }
    }

    mod copy {
        use super::*;

        /// Keys whose two languages differ enough to be evidence.
        fn distinguishing_keys(
            rendered: ResolvedLocale,
        ) -> Vec<(TextKey, &'static str, &'static str)> {
            let native = Catalog::new(rendered);
            let foreign = Catalog::new(match rendered {
                ResolvedLocale::German => ResolvedLocale::English,
                ResolvedLocale::English => ResolvedLocale::German,
            });
            TextKey::ALL
                .iter()
                .map(|&key| (key, native.text(key), foreign.text(key)))
                .filter(|(_, native, foreign)| {
                    native != foreign && !native.contains(*foreign) && foreign.len() >= 2
                })
                .collect()
        }

        /// No destination mixes the two languages.
        ///
        /// A single renderer cannot state this: it knows the catalog it was
        /// handed and nothing about the sentence its neighbour drew. What goes
        /// wrong in practice is one hard-coded English word among translated
        /// copy, and it is only visible when the whole destination is read at
        /// once.
        #[test]
        fn no_destination_mixes_the_two_languages() {
            for locale in LOCALES {
                let foreign = distinguishing_keys(locale);
                for scenario in scenarios() {
                    for &page in DESTINATIONS.iter() {
                        let harness = surface(&scenario.state, page, locale, PLAIN);
                        for spoken in announced(&harness) {
                            for (key, _, other) in &foreign {
                                assert!(
                                    !contains_word(&spoken, other),
                                    "{locale:?} / {} / {page:?}: {spoken:?} carries the \
                                     other language's copy for {key:?} ({other:?})",
                                    scenario.name
                                );
                            }
                        }
                    }
                }
            }
        }

        /// Every destination announces a title of its own, in both languages.
        ///
        /// Two claims, and the first one is what makes the second worth
        /// anything. Comparing the drawn header against `title_key` alone
        /// would be a tautology -- the header is drawn *from* `title_key` --
        /// so the mapping is first checked to be injective: six destinations,
        /// six different words. A rail whose entries share a title is a rail
        /// nobody can navigate by name, and no page can notice that about
        /// itself.
        #[test]
        fn every_destination_announces_a_title_of_its_own() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let titles: HashSet<&str> = DESTINATIONS
                    .iter()
                    .map(|&page| catalog.text(crate::ui::pages::title_key(page)))
                    .collect();
                assert_eq!(
                    titles.len(),
                    DESTINATIONS.len(),
                    "{locale:?}: two destinations answer to the same title"
                );

                for &page in DESTINATIONS.iter() {
                    let harness = surface(&base(), page, locale, PLAIN);
                    let title = catalog.text(crate::ui::pages::title_key(page));
                    // The header, not the rail: the rail carries all six
                    // titles at once, so "the tree contains this word" would
                    // hold on every destination and prove nothing.
                    let header = harness.root().children_recursive().any(|node| {
                        let raw = node.accesskit_node();
                        raw.role() == accesskit::Role::Label
                            && raw.value().as_deref() == Some(title)
                    });
                    assert!(
                        header,
                        "{locale:?} / {page:?}: the page header never says {title:?}"
                    );
                }
            }
        }
    }

    mod controls {
        use super::*;

        /// Every control the keyboard reaches does something.
        ///
        /// "Something" is deliberately broad: a typed event, a browser command,
        /// or a visible change to what the window announces. All are observable; a control
        /// that produces neither is decoration in the tab order. egui's own
        /// disabled commands never appear here at all, because a disabled
        /// command senses nothing and therefore is not focusable -- which is
        /// the honest outcome, not a gap.
        ///
        /// No component can state this. `command_action::show` returns a
        /// response and never dispatches; whether the caller does anything
        /// with it is a fact about the page, and whether the page's event
        /// reaches the app is a fact about the shell.
        #[test]
        fn no_reachable_control_is_inert() {
            // Same reasoning as the event drive: one rendered control per
            // destination and announced name.
            let mut already = HashSet::new();
            for scenario in scenarios() {
                for &page in DESTINATIONS.iter() {
                    let listed = {
                        let harness =
                            surface(&scenario.state, page, ResolvedLocale::English, PLAIN);
                        controls(&harness)
                            .into_iter()
                            .filter(|control| {
                                already.insert((
                                    crate::ui::pages::destination_index(page),
                                    control.clone(),
                                ))
                            })
                            .collect::<Vec<_>>()
                    };
                    for control in listed {
                        let mut harness =
                            surface(&scenario.state, page, ResolvedLocale::English, PLAIN);

                        // A text field's whole effect is taking the caret. It
                        // is the one control whose activation is not supposed
                        // to change anything else.
                        // A text field is not activated, it is typed into, and
                        // what it has to prove is that the typing lands. This
                        // is exactly the defect that made
                        // `TextKey::ShortcutInvalidKey` unreachable: the field
                        // was a numeric control that clamped every keystroke,
                        // so no sequence of keys could ever produce the
                        // sentence the catalog promised.
                        if control.role == accesskit::Role::TextInput {
                            let node = harness.get_by_role_and_label(control.role, &control.name);
                            let before = node.value();
                            node.focus();
                            harness.step();
                            harness
                                .get_by_role_and_label(control.role, &control.name)
                                .type_text("9");
                            harness.step();
                            harness.step();
                            let node = harness.get_by_role_and_label(control.role, &control.name);
                            assert!(
                                node.is_focused(),
                                "{} / {page:?}: {:?} cannot take the caret",
                                scenario.name,
                                control.name
                            );
                            assert_ne!(
                                node.value(),
                                before,
                                "{} / {page:?}: {:?} takes the caret and then swallows \
                                 what is typed into it",
                                scenario.name,
                                control.name
                            );
                            continue;
                        }

                        let before = announced(&harness);
                        let (events, commands) = activate_with_output(&mut harness, &control);
                        let after = announced(&harness);
                        let opens_browser = commands.iter().any(|command| {
                            matches!(command, egui::OutputCommand::OpenUrl(url)
                                if url.url == "https://paypal.me/ttk95")
                        });
                        let copies_hardware_report = control.name == Catalog::new(ResolvedLocale::English).text(TextKey::HardwareReport)
                            && commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text.starts_with(Catalog::new(ResolvedLocale::English).text(TextKey::HardwareSnapshot))));
                        assert!(
                            !events.is_empty() || opens_browser || copies_hardware_report || after != before,
                            "{} / {page:?}: activating {:?} sends no event, opens no browser and changes \
                             nothing the window says",
                            scenario.name,
                            control.name
                        );
                    }
                }
            }
        }
    }

    mod redaction {
        use super::*;

        /// Nothing a backend produced reaches anything the window says.
        ///
        /// Measured against [`backend_shape`], the same yardstick the
        /// diagnostics sweep uses, and not against a shorter list of its
        /// own. The shorter list was the whole defect: it held the planted
        /// fragments and the IPv4 shape, so a Windows endpoint ID was
        /// caught only by the accident that `{0.0.0.00000000}` parses as
        /// four numbers -- and its distinguishing half, a bare 8-4-4-4-12
        /// GUID, walked straight through. That half is exactly what an
        /// endpoint ID leaked into a display name would look like.
        ///
        /// Every failure scenario above carries the same poisoned summary
        /// through three different doors at once -- the stream state, the
        /// discovery state, and the notice -- and this reads every string the
        /// accessibility tree exposes on every destination in both languages.
        ///
        /// A component test cannot make this claim, because the summary does
        /// not enter through the component: it enters through the snapshot,
        /// and any of the six destinations could be the one that decides to
        /// quote it.
        #[test]
        fn no_destination_repeats_backend_text() {
            for locale in LOCALES {
                for scenario in scenarios() {
                    for &page in DESTINATIONS.iter() {
                        let harness = surface(&scenario.state, page, locale, PLAIN);
                        for spoken in announced(&harness) {
                            assert!(
                                backend_shape(&spoken).is_none(),
                                "{locale:?} / {} / {page:?}: {spoken:?} carries {}",
                                scenario.name,
                                backend_shape(&spoken).unwrap_or("nothing")
                            );
                        }
                    }
                }
            }
        }

        /// Four dot-separated numbers in a row. Version strings have three
        /// parts and measurements have one decimal, so neither matches.
        fn looks_like_an_address(text: &str) -> bool {
            text.split(|character: char| !character.is_ascii_digit() && character != '.')
                .any(|candidate| {
                    let parts: Vec<&str> = candidate.split('.').collect();
                    parts.len() == 4
                        && parts
                            .iter()
                            .all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
                })
        }

        // ------------------------------------------------- the registry page

        /// Which class of backend material a string carries, if any.
        ///
        /// [`FORBIDDEN_FRAGMENTS`] catches the exact poisoned text the
        /// scenarios plant. This catches the *shapes*, which is what a page
        /// built out of the diagnostics registry needs: the registry's own
        /// snapshot holds session UUIDs, MAC-derived receiver identities, an
        /// error record with two unbounded strings in it, and counters whose
        /// formatting nobody controls. None of that is any of the planted
        /// sentences, so a projection that leaked one of them would sail past
        /// a fragment list and fail here instead.
        fn backend_shape(text: &str) -> Option<&'static str> {
            for fragment in FORBIDDEN_FRAGMENTS {
                if text.contains(fragment) {
                    return Some("a planted backend fragment");
                }
            }
            if text.contains("://") {
                return Some("a URL scheme");
            }
            if looks_like_an_address(text) {
                return Some("an IPv4 address");
            }
            // A drive letter, a UNC prefix, or a home directory.
            if text.contains(":\\")
                || text.contains("\\\\")
                || text.contains("/Users/")
                || text.contains("/home/")
            {
                return Some("a filesystem path");
            }
            if looks_like_a_uuid(text) {
                return Some("a UUID");
            }
            for token in text.split_whitespace() {
                if let Some(shape) = colon_shape(token) {
                    return Some(shape);
                }
            }
            None
        }

        /// The 8-4-4-4-12 hexadecimal shape of a `SessionId`, anywhere in the
        /// text.
        ///
        /// The one class of registry material [`FORBIDDEN_FRAGMENTS`] can
        /// never cover by content: a session identifier is different on every
        /// run, so nothing can be planted for it and only its form gives it
        /// away.
        fn looks_like_a_uuid(text: &str) -> bool {
            text.split(|character: char| !character.is_ascii_hexdigit() && character != '-')
                .any(|candidate| {
                    let groups: Vec<&str> = candidate.split('-').collect();
                    groups.len() == 5
                        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, width)| {
                            group.len() == width
                                && group.chars().all(|character| character.is_ascii_hexdigit())
                        })
                })
        }

        /// What a single whitespace-free token's colons make it look like.
        ///
        /// One walk over three shapes that all use `:` as their separator and
        /// all mean the same thing here -- a raw identifier the window has no
        /// business showing:
        ///
        /// * `a0:b1:c2:d3:e4:01` -- a hardware address, and therefore a
        ///   `ReceiverId`, which is exactly what the registry keys its rows by;
        /// * `fe80::1` and friends -- an IPv6 address;
        /// * `7000:1` -- a socket, or the tail of one after a host was split
        ///   off.
        ///
        /// Deliberately requires the colon to have no space around it, because
        /// `label: value` is how every metric tile and every `named_value`
        /// sentence on this page announces itself, and flagging those would
        /// make the guard fire on correct copy and get deleted.
        fn colon_shape(token: &str) -> Option<&'static str> {
            let trimmed = token.trim_matches(|c: char| c == ',' || c == '.' || c == ';');
            if !trimmed.contains(':') {
                return None;
            }
            let parts: Vec<&str> = trimmed.split(':').collect();
            let hex = |part: &&str| part.len() <= 4 && part.chars().all(|c| c.is_ascii_hexdigit());
            if parts.len() == 6 && parts.iter().all(|part| part.len() == 2 && hex(part)) {
                return Some("a hardware address");
            }
            if parts.len() >= 3 && parts.iter().all(|part| part.is_empty() || hex(part)) {
                return Some("an IPv6 address");
            }
            if parts.len() == 2
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && parts
                    .iter()
                    .all(|part| part.chars().all(|c| c.is_ascii_digit()))
            {
                return Some("a socket");
            }
            None
        }

        /// A window state carrying one diagnostics reading.
        fn reading_state(reading: DiagnosticsReading) -> AppState {
            let mut state = base();
            state.stream = StreamState::Streaming {
                generation: GenerationId(7),
            };
            state.active_receivers = HashSet::from_iter([rid(1)]);
            state.diagnostics = Some(reading);
            state
        }

        /// Everything the Diagnostics page shows *and says* is checked, in
        /// both languages, for every health state and for numbers chosen to
        /// be as unhelpful as the type allows.
        ///
        /// This is the page that had no data at all until now, and it is the
        /// one whose data comes from the diagnostics registry -- the object
        /// that holds more identity, more free-form text, and more clock than
        /// anything else the backend owns. So the sweep is over the whole
        /// value space the shell's own type can hold, not over one example.
        ///
        /// **What this does not prove.** The reading is built here, by hand.
        /// A field added to [`DiagnosticsReading`] that carried registry
        /// material out of the projection would be filled with a placeholder
        /// in this file, and the page would then faithfully draw the nothing
        /// this test put into it -- and pass. This test proves the page's
        /// *copy* is clean, never that the *projection* is. The projection is
        /// guarded on its own side of the boundary, by a second copy of this
        /// shape check next to `backend_bridge::diagnostics_reading`; neither
        /// guard substitutes for the other, and the duplication is why.
        #[test]
        fn nothing_the_diagnostics_page_says_carries_backend_material() {
            let counters = [
                (0_u64, 0_usize),
                (1, 1),
                (u64::MAX, usize::MAX),
                (0xdead_beef, 0xbeef),
            ];
            let mut checked = 0_usize;

            for locale in LOCALES {
                for &health in DiagnosticsHealth::ALL {
                    for (events_dropped_total, measured_receivers) in counters {
                        let state = reading_state(DiagnosticsReading {
                            health,
                            events_dropped_total,
                            measured_receivers,
                        });
                        let harness = surface(&state, Page::Diagnostics, locale, PLAIN);
                        let spoken = announced(&harness);
                        assert!(
                            spoken.len() > 3,
                            "{locale:?} / {health:?}: the page announced {spoken:?}, \
                             which cannot be right -- this guard would pass vacuously"
                        );
                        for text in spoken {
                            assert!(
                                backend_shape(&text).is_none(),
                                "{locale:?} / {health:?}: {text:?} carries {} ",
                                backend_shape(&text).unwrap_or("nothing")
                            );
                            checked += 1;
                        }
                    }
                }
            }

            assert!(checked > 100, "only {checked} strings were checked");
        }

        /// The shape guard has to be able to fail, or it proves nothing.
        ///
        /// Both halves matter. A guard that flags nothing is decoration; a
        /// guard that flags the page's own correct copy gets deleted the
        /// first time it fires, which is the same thing one release later.
        #[test]
        fn the_shape_guard_flags_planted_shapes_and_leaves_correct_copy_alone() {
            for planted in [
                "a0:b1:c2:d3:e4:01",
                "fe80::1ff:fe23:4567:890a",
                "7000:1",
                r"C:\Users\Thorsten\AppData",
                "rtsp://192.168.178.44",
                "connect failed: os error 10061",
                "192.168.178.44",
                "30a6c3dd-ef9e-456e-84c6-2fc015ee9051",
                "session 30a6c3dd-ef9e-456e-84c6-2fc015ee9051 failed",
            ] {
                assert!(
                    backend_shape(planted).is_some(),
                    "the guard let {planted:?} through"
                );
            }

            for legitimate in [
                "Gesamtzustand: Unbekannt (Unbekannt)",
                "Overall health: Needs attention",
                "Verlorene Diagnoseereignisse: 18446744073709551615",
                "Receivers with measurements: 0",
                "1 selected, 1 streaming",
                "Status: Streaming",
                "Version: 0.1.0",
            ] {
                assert_eq!(
                    backend_shape(legitimate),
                    None,
                    "the guard fires on correct copy: {legitimate:?}"
                );
            }
        }

        /// The guard has to be able to fail, or it proves nothing.
        #[test]
        fn the_redaction_guard_flags_planted_backend_text() {
            assert!(looks_like_an_address("192.168.178.44"));
            assert!(!looks_like_an_address("Version: 0.1.0"));
            assert!(!looks_like_an_address("1,2 s"));
            assert!(FORBIDDEN_FRAGMENTS
                .iter()
                .any(|fragment| POISONED_SUMMARY.contains(fragment)));
        }

        /// The word-boundary matcher the language claim rests on.
        #[test]
        fn the_word_matcher_ignores_fragments_inside_longer_words() {
            assert!(!contains_word("Version: 0.1.0", "On"));
            assert!(!contains_word("Auswahl aufheben", "Aus"));
            assert!(contains_word("Ein: Aus", "Aus"));
            assert!(contains_word("Off", "Off"));
        }
    }

    /// The scan two claims rest on has to actually see a member that was just
    /// added, or both of them are decoration.
    ///
    /// Written against a synthetic declaration rather than the real one on
    /// purpose: adding a variant to `AppEvent` or a field to `Preferences`
    /// just to watch a test fail drags seven unrelated initialisers and a
    /// serde round trip along with it, and none of that is what is under
    /// test here. What is under test is that a member appearing in the
    /// declaration appears in the scan, including one behind a doc comment
    /// and one behind an attribute, and that a nested block does not leak its
    /// own members out.
    #[test]
    fn the_declaration_scan_sees_a_newly_added_member() {
        let before = "\
#[derive(Clone)]
pub enum Probe {
    First,
    /// Documented, and carrying a payload with braces in it.
    Second(Vec<Something>),
    Nested {
        leaked: bool,
    },
}

pub struct NotThisOne {
    pub decoy: bool,
}
";
        assert_eq!(
            members_declared_in(before, "pub enum Probe {"),
            vec!["First", "Second", "Nested"],
            "the scan does not read a declaration it is pointed at"
        );

        let after = before.replace(
            "    First,\n",
            "    First,\n    #[serde(skip)]\n    Added,\n",
        );
        assert_eq!(
            members_declared_in(&after, "pub enum Probe {"),
            vec!["First", "Added", "Second", "Nested"],
            "a member added to the declaration does not reach the scan, so every \
             classification claim resting on it is vacuous"
        );

        assert_eq!(
            members_declared_in(before, "pub struct NotThisOne {"),
            vec!["decoy"],
            "the scan reads struct fields without their visibility"
        );
    }

    /// A helper the drives above rely on: the events sink has to actually be
    /// the sink, or every claim about "no event was produced" is vacuous.
    #[test]
    fn the_event_sink_records_what_the_window_dispatches() {
        let catalog = Catalog::new(ResolvedLocale::English);
        let mut harness = surface(&base(), Page::Home, ResolvedLocale::English, PLAIN);
        let refresh = Control {
            name: catalog.text(TextKey::RefreshReceivers).to_owned(),
            role: accesskit::Role::Button,
        };
        let events = activate(&mut harness, &refresh);
        assert_eq!(events, vec![AppEvent::RefreshRequested]);
    }
}
