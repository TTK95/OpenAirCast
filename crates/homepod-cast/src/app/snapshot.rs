#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;

    use super::*;
    use crate::app::{
        AppState, AudioEndpointChoice, AudioEndpointKey, AudioEndpointSelection,
        AudioSourceReading, Availability, CaptureState, CorrectiveAction, DiagnosticsHealth,
        DiagnosticsReading, DiscoveryState, GenerationId, HotkeyBinding, LatencyChoice,
        LatencyOption, LatencyReading, LatencyUnavailable, LocalePreference, NoticeCode, Page,
        ReceiverState, ResolvedLocale, Severity, StreamState, ThemePreference, UserNotice,
        WindowGeometry, WindowState,
    };

    fn id(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn state_with(stream: StreamState) -> AppState {
        AppState {
            revision: 12,
            saved_groups: None,
            group_operation: None,
            next_group_request: 0,
            desired_known: false,
            controller_closed: false,
            desired_revision: 10,
            page: Page::Home,
            window: WindowState {
                visible: true,
                geometry: WindowGeometry::default(),
                close_to_tray: true,
                launch_at_startup: false,
            },
            discovery: DiscoveryState::Ready,
            receivers: vec![
                ReceiverState {
                    id: id(3),
                    name: "zeta".into(),
                    model: "HomePod".into(),
                    availability: Availability::Unavailable,
                },
                ReceiverState {
                    id: id(2),
                    name: "Alpha".into(),
                    model: "HomePod mini".into(),
                    availability: Availability::Available,
                },
                ReceiverState {
                    id: id(1),
                    name: "alpha".into(),
                    model: "Apple TV".into(),
                    availability: Availability::Available,
                },
            ],
            desired_receivers: HashSet::from([id(3), id(1)]),
            staged_receivers: HashSet::from([id(3)]),
            staged_base_revision: 9,
            active_receivers: HashSet::from([id(3), id(1)]),
            stream,
            master_volume: 0.6,
            volume_pending: false,
            confirmed_master_volume: Some(0.6),
            receiver_levels: None,
            theme: ThemePreference::System,
            locale_preference: LocalePreference::System,
            windows_display_locale: ResolvedLocale::English,
            resolved_locale: ResolvedLocale::English,
            advanced_information: false,
            hotkey: HotkeyBinding::default(),
            notice: None,
            muted: None,
            latency: None,
            audio_source: None,
            diagnostics: None,
            live_diagnostics: None,
            hardware_check: Default::default(),
            shutting_down: false,
            generations: Default::default(),
        }
    }

    #[test]
    fn projects_safe_deterministic_receiver_and_membership_ordering() {
        let snapshot = UiSnapshot::from_state(&state_with(StreamState::Stopped));

        assert_eq!(
            snapshot
                .receivers
                .iter()
                .map(|receiver| receiver.id.0[5])
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            snapshot
                .desired_receivers
                .iter()
                .map(|receiver_id| receiver_id.0[5])
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            snapshot
                .staged_receivers
                .iter()
                .map(|receiver_id| receiver_id.0[5])
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(
            snapshot
                .active_receivers
                .iter()
                .map(|receiver_id| receiver_id.0[5])
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(snapshot.staged_membership_dirty);
        assert!(snapshot.staged_membership_stale);
        assert!(snapshot.can_start);
        assert!(!snapshot.can_stop);
    }

    /// One example of every lifecycle state, with the two commands it offers.
    ///
    /// Kept as a named list because three things depend on it: the assertion
    /// below, the coverage guard beside it, and the fact that a phase missing
    /// from here is a phase whose `can_stop` nobody ever checks.
    fn every_stream_state() -> Vec<(StreamState, bool, bool)> {
        vec![
            (StreamState::Stopped, true, false),
            (
                StreamState::Starting {
                    generation: GenerationId(1),
                },
                false,
                true,
            ),
            (
                StreamState::Streaming {
                    generation: GenerationId(2),
                },
                false,
                true,
            ),
            // Running sessions, both of them, and the Stop button is the
            // whole point of saying so: `can_stop` is `is_running`, and an
            // `is_running` that answered `false` here would leave the window
            // drawing a live Stop over a reducer that ignores the press.
            (
                StreamState::Degraded {
                    generation: GenerationId(5),
                },
                false,
                true,
            ),
            (
                StreamState::Restarting {
                    generation: GenerationId(6),
                },
                false,
                true,
            ),
            (
                StreamState::Stopping {
                    generation: GenerationId(3),
                },
                false,
                false,
            ),
            (
                StreamState::Failed {
                    generation: GenerationId(4),
                    summary: "Could not connect".into(),
                },
                true,
                false,
            ),
        ]
    }

    #[test]
    fn derives_actions_from_actual_stream_state() {
        for (stream, can_start, can_stop) in every_stream_state() {
            let snapshot = UiSnapshot::from_state(&state_with(stream.clone()));

            assert_eq!(snapshot.can_start, can_start, "{stream:?}");
            assert_eq!(snapshot.can_stop, can_stop, "{stream:?}");
        }
    }

    /// The table above has to cover the enum, not a remembered subset of it.
    ///
    /// One list feeds both halves of the check: the macro builds an
    /// exhaustive match *and* the roster of names from the same lines. A new
    /// `StreamState` variant therefore stops this file compiling -- the match
    /// is no longer exhaustive -- and the line the author adds to fix that is
    /// the same line that adds the variant to the roster. There is no way to
    /// satisfy the compiler and still leave the roster short, which is
    /// exactly what a hand-kept second list would allow.
    ///
    /// `Degraded` and `Restarting` reached the enum, the snapshot, and the
    /// window without any command table noticing. This is the check that was
    /// missing.
    #[test]
    fn the_command_table_covers_every_stream_state() {
        macro_rules! stream_state_roster {
            ($($pattern:pat => $name:literal,)+) => {
                const EVERY_NAME: &[&str] = &[$($name),+];

                fn name_of(stream: &StreamState) -> &'static str {
                    match stream {
                        $($pattern => $name,)+
                    }
                }
            };
        }

        stream_state_roster! {
            StreamState::Stopped => "stopped",
            StreamState::Starting { .. } => "starting",
            StreamState::Streaming { .. } => "streaming",
            StreamState::Degraded { .. } => "degraded",
            StreamState::Restarting { .. } => "restarting",
            StreamState::Stopping { .. } => "stopping",
            StreamState::Failed { .. } => "failed",
        }

        let covered = every_stream_state()
            .iter()
            .map(|(stream, _, _)| name_of(stream))
            .collect::<std::collections::HashSet<_>>();

        let missing = EVERY_NAME
            .iter()
            .filter(|expected| !covered.contains(*expected))
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "these lifecycle states offer commands that nothing checks: {missing:?}"
        );
    }

    #[test]
    fn does_not_offer_start_when_desired_receivers_are_unavailable() {
        let mut state = state_with(StreamState::Stopped);
        state.receivers[2].availability = Availability::Unavailable;

        let snapshot = UiSnapshot::from_state(&state);

        assert!(!snapshot.can_start);
    }

    #[test]
    fn keeps_clean_staged_membership_non_stale_even_after_a_newer_desired_revision() {
        let mut state = state_with(StreamState::Stopped);
        state.staged_receivers = state.desired_receivers.clone();

        let snapshot = UiSnapshot::from_state(&state);

        assert!(!snapshot.staged_membership_dirty);
        assert!(!snapshot.staged_membership_stale);
    }

    #[test]
    fn publishes_the_chosen_and_resolved_language_but_never_the_cached_windows_one() {
        let mut state = state_with(StreamState::Stopped);
        state.locale_preference = LocalePreference::English;
        state.windows_display_locale = ResolvedLocale::German;
        state.resolved_locale = ResolvedLocale::English;
        state.advanced_information = true;

        let snapshot = UiSnapshot::from_state(&state);

        assert_eq!(snapshot.locale_preference, LocalePreference::English);
        assert_eq!(
            snapshot.resolved_locale,
            ResolvedLocale::English,
            "the published language is the resolved one, not the cached Windows reading"
        );
        assert!(snapshot.advanced_information);
    }

    #[test]
    fn projects_every_presentation_field_with_an_exhaustive_snapshot_shape() {
        let mut state = state_with(StreamState::Failed {
            generation: GenerationId(42),
            summary: "Authentication failed".into(),
        });
        state.revision = 37;
        state.desired_revision = 21;
        state.page = Page::Diagnostics;
        state.window = WindowState {
            visible: false,
            geometry: WindowGeometry {
                x: Some(40.0),
                y: Some(80.0),
                width: 1280.0,
                height: 900.0,
                maximized: true,
            },
            close_to_tray: true,
            launch_at_startup: false,
        };
        state.discovery = DiscoveryState::Failed {
            summary: "Network unavailable".into(),
        };
        state.master_volume = 0.73;
        state.theme = ThemePreference::Dark;
        state.locale_preference = LocalePreference::System;
        state.windows_display_locale = ResolvedLocale::German;
        state.resolved_locale = ResolvedLocale::German;
        state.advanced_information = true;
        state.hotkey = HotkeyBinding {
            enabled: false,
            modifiers: 0x0001,
            virtual_key: 0x4B,
        };
        state.notice = Some(UserNotice {
            severity: Severity::Error,
            code: NoticeCode::SessionFailed,
            summary: "Retry after reconnecting".into(),
            action: Some(CorrectiveAction::Retry),
        });
        state.diagnostics = Some(DiagnosticsReading {
            health: DiagnosticsHealth::Attention,
            events_dropped_total: 6,
            measured_receivers: 2,
        });
        state.muted = Some(true);
        state.latency = Some(LatencyReading {
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
            ],
        });
        state.audio_source = Some(AudioSourceReading {
            refresh_failed: false,
            endpoints_known: true,
            endpoints: vec![AudioEndpointChoice {
                key: AudioEndpointKey(3),
                name: "Speakers".into(),
            }],
            selection: AudioEndpointSelection::Chosen(AudioEndpointKey(3)),
            captured_key: None,
            captured_name: Some("Speakers".into()),
            state: CaptureState::Capturing,
        });
        state.shutting_down = true;
        state.receiver_levels = Some(std::collections::BTreeMap::from([(id(1), 0.4)]));

        let snapshot = UiSnapshot::from_state(&state);
        let UiSnapshot {
            revision,
            saved_groups,
            group_operation,
            desired_revision,
            page,
            window,
            discovery,
            receivers,
            desired_receivers,
            staged_receivers,
            staged_membership_dirty,
            staged_membership_stale,
            active_receivers,
            stream,
            master_volume,
            receiver_levels,
            theme,
            locale_preference,
            resolved_locale,
            advanced_information,
            hotkey,
            notice,
            muted,
            latency,
            audio_source,
            diagnostics,
            live_diagnostics: _,
            hardware_check,
            can_start,
            can_stop,
            shutting_down,
        } = snapshot;
        assert_eq!(saved_groups, state.saved_groups);
        assert_eq!(
            hardware_check,
            super::super::hardware_check::HardwareCheckSnapshot::from_state(&state)
        );
        assert_eq!(group_operation, state.group_operation);
        let WindowSnapshot {
            visible,
            geometry,
            close_to_tray,
        } = window;
        let ReceiverSnapshot {
            id: receiver_id,
            name: receiver_name,
            model: receiver_model,
            availability: receiver_availability,
        } = &receivers[0];

        assert_eq!(revision, 37);
        assert_eq!(receiver_levels, state.receiver_levels);
        assert_eq!(desired_revision, 21);
        assert_eq!(page, Page::Diagnostics);
        assert!(!visible);
        assert!(close_to_tray);
        assert_eq!(
            geometry,
            WindowGeometry {
                x: Some(40.0),
                y: Some(80.0),
                width: 1280.0,
                height: 900.0,
                maximized: true,
            }
        );
        assert_eq!(
            discovery,
            DiscoverySnapshot::Failed {
                summary: "Network unavailable".into(),
            }
        );
        assert_eq!(receiver_id, &id(1));
        assert_eq!(receiver_name, "alpha");
        assert_eq!(receiver_model, "Apple TV");
        assert_eq!(*receiver_availability, Availability::Available);
        assert_eq!(
            desired_receivers
                .iter()
                .map(|id| id.0[5])
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            staged_receivers
                .iter()
                .map(|id| id.0[5])
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(staged_membership_dirty);
        assert!(staged_membership_stale);
        assert_eq!(
            active_receivers
                .iter()
                .map(|id| id.0[5])
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            stream,
            StreamSnapshot::Failed {
                generation: GenerationId(42),
                summary: "Authentication failed".into(),
            }
        );
        assert_eq!(master_volume, 0.73);
        assert_eq!(theme, ThemePreference::Dark);
        assert_eq!(locale_preference, LocalePreference::System);
        assert_eq!(resolved_locale, ResolvedLocale::German);
        assert!(advanced_information);
        assert_eq!(
            hotkey,
            HotkeyBinding {
                enabled: false,
                modifiers: 0x0001,
                virtual_key: 0x4B,
            }
        );
        assert_eq!(
            notice,
            Some(UserNotice {
                severity: Severity::Error,
                code: NoticeCode::SessionFailed,
                summary: "Retry after reconnecting".into(),
                action: Some(CorrectiveAction::Retry),
            })
        );
        assert_eq!(
            diagnostics,
            Some(DiagnosticsReading {
                health: DiagnosticsHealth::Attention,
                events_dropped_total: 6,
                measured_receivers: 2,
            })
        );
        assert_eq!(muted, Some(true));
        assert_eq!(
            latency,
            Some(LatencyReading {
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
                ],
            })
        );
        assert_eq!(
            audio_source,
            Some(AudioSourceReading {
                refresh_failed: false,
                endpoints_known: true,
                endpoints: vec![AudioEndpointChoice {
                    key: AudioEndpointKey(3),
                    name: "Speakers".into(),
                }],
                selection: AudioEndpointSelection::Chosen(AudioEndpointKey(3)),
                captured_key: None,
                captured_name: Some("Speakers".into()),
                state: CaptureState::Capturing,
            })
        );
        assert!(can_start);
        assert!(!can_stop);
        assert!(shutting_down);
    }
}
use std::sync::Arc;

use airplay_core::DeviceId;

use super::{
    AppState, AudioSourceReading, Availability, DiagnosticsReading, DiscoveryState, HotkeyBinding,
    LatencyReading, LocalePreference, Page, ResolvedLocale, StreamState, ThemePreference,
    UserNotice, WindowGeometry,
};

#[derive(Clone, Debug, PartialEq)]
pub struct WindowSnapshot {
    pub visible: bool,
    pub geometry: WindowGeometry,
    /// Whether closing the window hides it instead of ending the app.
    ///
    /// The shell has to know this at the moment the window manager asks it to
    /// close: without it the X button would end the event loop directly,
    /// which skips the bounded shutdown -- no worker stopped, no settings
    /// written, no exit valve.
    pub close_to_tray: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoverySnapshot {
    Idle,
    Discovering,
    Ready,
    Failed { summary: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverSnapshot {
    pub id: DeviceId,
    pub name: String,
    pub model: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamSnapshot {
    Stopped,
    Starting {
        generation: super::GenerationId,
    },
    Streaming {
        generation: super::GenerationId,
    },
    /// Running, but not delivering to every member that was asked for.
    Degraded {
        generation: super::GenerationId,
    },
    /// Running, with the whole group being rebuilt by the controller.
    Restarting {
        generation: super::GenerationId,
    },
    Stopping {
        generation: super::GenerationId,
    },
    Failed {
        generation: super::GenerationId,
        summary: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSnapshot {
    pub revision: u64,
    /// Confirmed templates, or None before the backend reports its first list.
    pub saved_groups: Option<Vec<super::SavedGroupReading>>,
    /// Latest correlated action and status for contextual pending/error UI.
    pub group_operation: Option<super::GroupOperation>,
    pub desired_revision: u64,
    pub page: Page,
    pub window: WindowSnapshot,
    pub discovery: DiscoverySnapshot,
    pub receivers: Arc<[ReceiverSnapshot]>,
    pub desired_receivers: Arc<[DeviceId]>,
    pub staged_receivers: Arc<[DeviceId]>,
    pub staged_membership_dirty: bool,
    pub staged_membership_stale: bool,
    pub active_receivers: Arc<[DeviceId]>,
    pub stream: StreamSnapshot,
    pub master_volume: f32,
    /// Durable per-receiver balance; unknown until the backend reports.
    pub receiver_levels: Option<std::collections::BTreeMap<DeviceId, f32>>,
    pub theme: ThemePreference,
    /// The language the user chose, for the control that shows the choice.
    pub locale_preference: LocalePreference,
    /// The language to render in. The Windows reading `System` resolves
    /// through stays behind in `AppState`: renderers need the answer, not the
    /// input.
    pub resolved_locale: ResolvedLocale,
    /// Whether advanced diagnostic detail is revealed.
    pub advanced_information: bool,
    pub hotkey: HotkeyBinding,
    pub notice: Option<UserNotice>,
    /// Whether the backend reports the group muted, or `None` while nothing
    /// has reported.
    ///
    /// Copied straight through from [`AppState`]: it is already one bit, and
    /// the `Option` survives the copy so the Audio destination can tell
    /// "not muted" from "not measured" and refuse to draw a switch over the
    /// second one.
    pub muted: Option<bool>,
    /// The newest latency reading, or `None` while nothing has reported.
    pub latency: Option<LatencyReading>,
    /// The newest capture-source reading, or `None` while nothing has
    /// reported.
    ///
    /// Carries opaque [`super::AudioEndpointKey`]s, never a Windows endpoint
    /// ID: the translation both ways lives in `backend_bridge`.
    pub audio_source: Option<AudioSourceReading>,
    /// The newest reading of the diagnostics registry, or `None` while the
    /// registry has never reported.
    ///
    /// Copied straight through from [`AppState`]: it is already the shell's
    /// own presentation-safe type, chosen field by field in
    /// `backend_bridge::diagnostics_reading`, so there is nothing left here
    /// to narrow. The `Option` survives the copy because the Diagnostics page
    /// must be able to tell "measured zero" from "not measured".
    pub diagnostics: Option<DiagnosticsReading>,
    /// Current shell-generation telemetry only; closed controllers expose no stale readings.
    pub live_diagnostics: Option<super::LiveDiagnosticsReading>,
    pub hardware_check: super::hardware_check::HardwareCheckSnapshot,
    pub can_start: bool,
    pub can_stop: bool,
    pub shutting_down: bool,
}

impl UiSnapshot {
    pub fn from_state(state: &AppState) -> Self {
        let mut receivers = state
            .receivers
            .iter()
            .map(|receiver| ReceiverSnapshot {
                id: receiver.id.clone(),
                name: receiver.name.clone(),
                model: receiver.model.clone(),
                availability: receiver.availability,
            })
            .collect::<Vec<_>>();
        receivers.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.0.cmp(&right.id.0))
        });

        let desired_receivers = sorted_ids(&state.desired_receivers);
        let staged_receivers = sorted_ids(&state.staged_receivers);
        let active_receivers = sorted_ids(&state.active_receivers);
        let staged_membership_dirty = state.staged_receivers != state.desired_receivers;
        let has_available_desired_receiver = state.receivers.iter().any(|receiver| {
            receiver.availability == Availability::Available
                && state.desired_receivers.contains(&receiver.id)
        });
        let can_start = matches!(
            state.stream,
            StreamState::Stopped | StreamState::Failed { .. }
        ) && has_available_desired_receiver;
        let can_stop = state.stream.is_running();

        Self {
            revision: state.revision,
            saved_groups: state.saved_groups.clone(),
            group_operation: state.group_operation.clone(),
            desired_revision: state.desired_revision,
            page: state.page,
            window: WindowSnapshot {
                visible: state.window.visible,
                geometry: state.window.geometry,
                close_to_tray: state.window.close_to_tray,
            },
            discovery: match &state.discovery {
                DiscoveryState::Idle => DiscoverySnapshot::Idle,
                DiscoveryState::Discovering { .. } => DiscoverySnapshot::Discovering,
                DiscoveryState::Ready => DiscoverySnapshot::Ready,
                DiscoveryState::Failed { summary } => DiscoverySnapshot::Failed {
                    summary: summary.clone(),
                },
            },
            receivers: receivers.into(),
            desired_receivers: desired_receivers.into(),
            staged_receivers: staged_receivers.into(),
            staged_membership_dirty,
            staged_membership_stale: staged_membership_dirty
                && state.staged_base_revision < state.desired_revision,
            active_receivers: active_receivers.into(),
            stream: stream_snapshot(&state.stream),
            master_volume: state.master_volume,
            receiver_levels: state.receiver_levels.clone(),
            theme: state.theme,
            locale_preference: state.locale_preference,
            resolved_locale: state.resolved_locale,
            advanced_information: state.advanced_information,
            hotkey: state.hotkey,
            notice: state.notice.clone(),
            muted: state.muted,
            latency: state.latency.clone(),
            audio_source: state.audio_source.clone(),
            diagnostics: state.diagnostics,
            live_diagnostics: state
                .live_diagnostics
                .as_ref()
                .filter(|reading| {
                    !state.controller_closed && reading.generation == state.generations.session
                })
                .cloned(),
            hardware_check: super::hardware_check::HardwareCheckSnapshot::from_state(state),
            can_start,
            can_stop,
            shutting_down: state.shutting_down,
        }
    }
}

fn sorted_ids(ids: &std::collections::HashSet<DeviceId>) -> Vec<DeviceId> {
    let mut sorted = ids.iter().cloned().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    sorted
}

fn stream_snapshot(stream: &StreamState) -> StreamSnapshot {
    match stream {
        StreamState::Stopped => StreamSnapshot::Stopped,
        StreamState::Starting { generation } => StreamSnapshot::Starting {
            generation: *generation,
        },
        StreamState::Streaming { generation } => StreamSnapshot::Streaming {
            generation: *generation,
        },
        StreamState::Degraded { generation } => StreamSnapshot::Degraded {
            generation: *generation,
        },
        StreamState::Restarting { generation } => StreamSnapshot::Restarting {
            generation: *generation,
        },
        StreamState::Stopping { generation } => StreamSnapshot::Stopping {
            generation: *generation,
        },
        StreamState::Failed {
            generation,
            summary,
        } => StreamSnapshot::Failed {
            generation: *generation,
            summary: summary.clone(),
        },
    }
}
