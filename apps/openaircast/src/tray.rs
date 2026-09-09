//! Snapshot-derived tray menu adapter.
//
// ===========================================================================
// Snapshot adapter (Task 13): the only tray code Task 14 keeps.
// ===========================================================================

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

// ===========================================================================

/// One receiver row in the tray menu, derived purely from a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayReceiver {
    pub id: airplay_core::DeviceId,
    pub name: String,
    pub checked: bool,
    pub enabled: bool,
    pub active: bool,
}

/// Start/Stop menu entry state derived from actual stream state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TraySessionAction {
    Start { enabled: bool },
    Stop { enabled: bool },
    StoppingProgress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayModel {
    pub tooltip: String,
    pub session_action: TraySessionAction,
    pub receivers: Vec<TrayReceiver>,
    pub has_pending_membership: bool,
}

/// Menu entries in fixed order; separators included for model tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayItem {
    Open,
    Separator,
    Session,
    Receiver(usize),
    Settings,
    Quit,
}

/// What one interaction dispatches into the app event queue.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrayDispatch {
    One(crate::app::AppEvent),
    Batch(Vec<crate::app::AppEvent>),
}

/// The one phrase the tray tooltip uses for the application state.
///
/// The tray is the only surface left that is not localized: it is drawn by
/// the OS shell, outside the catalog-driven window, and giving it a language
/// belongs with the rest of the tray work rather than with the Control
/// Center's Command Home.
fn actual_state_label(snapshot: &crate::app::UiSnapshot) -> String {
    use crate::app::StreamSnapshot;

    if snapshot.notice.is_some() {
        return "Needs attention".into();
    }
    match &snapshot.stream {
        StreamSnapshot::Stopped => "Stopped".into(),
        StreamSnapshot::Starting { .. } => "Connecting".into(),
        StreamSnapshot::Stopping { .. } => "Stopping\u{2026}".into(),
        StreamSnapshot::Failed { .. } => "Needs attention".into(),
        StreamSnapshot::Restarting { .. } => "Reconnecting\u{2026}".into(),
        StreamSnapshot::Streaming { .. } => {
            let count = snapshot.active_receivers.len();
            format!("Streaming to {count}")
        }
        // Named rather than folded into the streaming arm: the count alone
        // cannot say that a receiver which was asked for is not getting audio.
        StreamSnapshot::Degraded { .. } => {
            let count = snapshot.active_receivers.len();
            format!("Reduced, streaming to {count}")
        }
    }
}

fn tooltip_for(snapshot: &crate::app::UiSnapshot) -> String {
    let state = actual_state_label(snapshot);
    let active = snapshot.active_receivers.len();
    format!("OpenAirCast \u{2014} {state}, {active} active")
}

impl TrayModel {
    pub(crate) fn from_snapshot(snapshot: &crate::app::UiSnapshot) -> Self {
        use crate::app::StreamSnapshot;

        let session_action = match &snapshot.stream {
            StreamSnapshot::Starting { .. }
            | StreamSnapshot::Streaming { .. }
            // Both are running sessions, so both keep the command that ends
            // them. A restart the backend started on its own is still a
            // session the user is entitled to stop.
            | StreamSnapshot::Degraded { .. }
            | StreamSnapshot::Restarting { .. } => TraySessionAction::Stop { enabled: true },
            StreamSnapshot::Stopping { .. } => TraySessionAction::StoppingProgress,
            StreamSnapshot::Stopped | StreamSnapshot::Failed { .. } => TraySessionAction::Start {
                enabled: snapshot.can_start,
            },
        };

        let receivers = snapshot
            .receivers
            .iter()
            .map(|receiver| TrayReceiver {
                id: receiver.id.clone(),
                name: receiver.name.clone(),
                checked: snapshot.desired_receivers.contains(&receiver.id),
                enabled: receiver.availability == crate::app::Availability::Available,
                active: snapshot.active_receivers.contains(&receiver.id),
            })
            .collect();

        Self {
            tooltip: tooltip_for(snapshot),
            session_action,
            receivers,
            has_pending_membership: snapshot.staged_membership_dirty,
        }
    }
}

/// Fixed visual order of the tray menu.
pub(crate) fn menu_items(model: &TrayModel) -> Vec<TrayItem> {
    let mut items = vec![TrayItem::Open, TrayItem::Separator, TrayItem::Session];
    for index in 0..model.receivers.len() {
        items.push(TrayItem::Receiver(index));
    }
    items.push(TrayItem::Separator);
    items.push(TrayItem::Settings);
    items.push(TrayItem::Separator);
    items.push(TrayItem::Quit);
    items
}

/// Left button click on the tray icon restores/focuses the window.
#[allow(dead_code)] // semantics exercised by snapshot_model tests; handler inlines it.
pub(crate) fn left_click_dispatch() -> TrayDispatch {
    TrayDispatch::One(crate::app::AppEvent::ShowMainWindow)
}

/// Click semantics for one menu entry; disabled entries dispatch nothing.
pub(crate) fn click(model: &TrayModel, item: &TrayItem) -> Option<TrayDispatch> {
    use crate::app::{AppEvent, Page};

    match item {
        TrayItem::Open => Some(TrayDispatch::One(AppEvent::ShowMainWindow)),
        TrayItem::Separator => None,
        TrayItem::Session => match &model.session_action {
            TraySessionAction::Start { enabled } if *enabled => {
                Some(TrayDispatch::One(AppEvent::StartRequested))
            }
            TraySessionAction::Stop { enabled } if *enabled => {
                Some(TrayDispatch::One(AppEvent::StopRequested))
            }
            _ => None,
        },
        TrayItem::Receiver(index) => {
            let receiver = model.receivers.get(*index)?;
            if !receiver.enabled || model.has_pending_membership {
                // While window edits are pending the tray must not silently
                // commit membership; unavailable rows stay inert too.
                return None;
            }
            Some(TrayDispatch::Batch(vec![
                AppEvent::ToggleStagedReceiver(receiver.id.clone()),
                AppEvent::ApplyStagedReceivers,
            ]))
        }
        TrayItem::Settings => Some(TrayDispatch::Batch(vec![
            AppEvent::Navigate(Page::Settings),
            AppEvent::ShowMainWindow,
        ])),
        TrayItem::Quit => Some(TrayDispatch::One(AppEvent::QuitRequested)),
    }
}

/// One rendered row of the tray menu: everything a surface needs to build it,
/// and nothing that requires a window.
///
/// The menu used to be assembled straight into `tray_icon` types, which meant
/// its labels, its enabled flags, and its check marks could only be inspected
/// by looking at a real Windows shell. Resolving them here first is what lets
/// a test state what the user's menu actually says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrayEntry {
    Separator,
    Command {
        label: String,
        enabled: bool,
        /// `Some` on a checkable receiver row, `None` on a plain command.
        checked: Option<bool>,
        /// What activating the row dispatches; `None` makes it inert.
        dispatch: Option<TrayDispatch>,
    },
}

/// The English labels the shell-drawn menu uses.
///
/// The tray is the one surface outside the localized window: it is drawn by
/// the OS shell, which is why these are literals here rather than catalog
/// keys.
const LABEL_OPEN: &str = "Open";
const LABEL_START: &str = "Start streaming";
const LABEL_STOP: &str = "Stop streaming";
const LABEL_STOPPING: &str = "Stopping\u{2026}";
const LABEL_SETTINGS: &str = "Settings\u{2026}";
const LABEL_QUIT: &str = "Quit";
/// Appended to a receiver row that is actually carrying audio.
const ACTIVE_SUFFIX: &str = " \u{25CF}";

/// Renders the model into the rows a surface has to show, in menu order.
pub(crate) fn tray_entries(model: &TrayModel) -> Vec<TrayEntry> {
    menu_items(model)
        .into_iter()
        .map(|item| match &item {
            TrayItem::Separator => TrayEntry::Separator,
            TrayItem::Open => TrayEntry::Command {
                label: LABEL_OPEN.to_owned(),
                enabled: true,
                checked: None,
                dispatch: click(model, &item),
            },
            TrayItem::Session => {
                let (label, enabled) = match &model.session_action {
                    TraySessionAction::Start { enabled } => (LABEL_START, *enabled),
                    TraySessionAction::Stop { enabled } => (LABEL_STOP, *enabled),
                    TraySessionAction::StoppingProgress => (LABEL_STOPPING, false),
                };
                TrayEntry::Command {
                    label: label.to_owned(),
                    enabled,
                    checked: None,
                    dispatch: click(model, &item),
                }
            }
            TrayItem::Receiver(index) => {
                let receiver = &model.receivers[*index];
                let suffix = if receiver.active { ACTIVE_SUFFIX } else { "" };
                TrayEntry::Command {
                    label: format!("{}{}", receiver.name, suffix),
                    enabled: receiver.enabled && !model.has_pending_membership,
                    checked: Some(receiver.checked),
                    dispatch: click(model, &item),
                }
            }
            TrayItem::Settings => TrayEntry::Command {
                label: LABEL_SETTINGS.to_owned(),
                enabled: true,
                checked: None,
                dispatch: click(model, &item),
            },
            TrayItem::Quit => TrayEntry::Command {
                label: LABEL_QUIT.to_owned(),
                enabled: true,
                checked: None,
                dispatch: click(model, &item),
            },
        })
        .collect()
}

/// The one thing the controller needs a tray to be able to do.
///
/// Everything above this trait is a pure mapping from a snapshot; everything
/// below it is Win32. The seam exists so a test can assert that the menu is
/// driven at all -- the defect it was introduced for was a controller whose
/// `sync` had no caller, so the icon kept the empty menu it was built with
/// for the whole life of the process.
pub(crate) trait TraySurface {
    /// Replaces the tooltip and the whole menu.
    fn apply(&mut self, tooltip: &str, entries: &[TrayEntry]);
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // legacy run() above; Task 14 deletes it.
mod snapshot_model {
    use std::collections::HashSet;

    use airplay_core::DeviceId;

    use super::*;
    use crate::app::{
        AppState, Availability, NoticeCode, Severity, StreamState, UiSnapshot, UserNotice,
    };

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn receiver(last: u8, name: &str, availability: Availability) -> crate::app::ReceiverState {
        crate::app::ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "HomePod".into(),
            availability,
        }
    }

    fn snapshot_from(state: &mut AppState) -> UiSnapshot {
        UiSnapshot::from_state(state)
    }

    fn streaming_snapshot(active: bool) -> UiSnapshot {
        let mut state = AppState {
            receivers: vec![receiver(1, "Kitchen", Availability::Available)],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            ..AppState::default()
        };
        if active {
            state.stream = StreamState::Streaming {
                generation: crate::app::GenerationId(1),
            };
            state.generations.session = crate::app::GenerationId(1);
            state.active_receivers.insert(rid(1));
        }
        snapshot_from(&mut state)
    }

    /// Every branch of the tooltip phrase, one at a time.
    ///
    /// This is the coverage `ui::navigation`'s
    /// `actual_state_label_reflects_real_stream_and_notice_state` carried
    /// before Task 8 deleted that module and moved the function here. Only
    /// the notice branch survived the move -- through
    /// `needs_attention_model_reports_notice_and_keeps_quit_available` --
    /// which left the four stream branches, and the receiver count inside
    /// the streaming one, free to say anything at all.
    #[test]
    fn the_tray_phrase_names_the_real_stream_state_and_counts_its_receivers() {
        let mut state = AppState::default();
        assert_eq!(
            actual_state_label(&UiSnapshot::from_state(&state)),
            "Stopped"
        );

        state.stream = StreamState::Starting {
            generation: crate::app::GenerationId(1),
        };
        assert_eq!(
            actual_state_label(&UiSnapshot::from_state(&state)),
            "Connecting"
        );

        state.stream = StreamState::Streaming {
            generation: crate::app::GenerationId(1),
        };
        state.active_receivers = HashSet::from_iter([rid(6), rid(7)]);
        assert_eq!(
            actual_state_label(&UiSnapshot::from_state(&state)),
            "Streaming to 2",
            "the phrase reports the count, so it has to come from the set"
        );

        state.stream = StreamState::Stopping {
            generation: crate::app::GenerationId(1),
        };
        assert_eq!(
            actual_state_label(&UiSnapshot::from_state(&state)),
            "Stopping\u{2026}",
            "one horizontal ellipsis, not three periods and not mojibake"
        );

        state.stream = StreamState::Failed {
            generation: crate::app::GenerationId(1),
            summary: "device refused the session at 192.168.1.44:7000".into(),
        };
        let failed = actual_state_label(&UiSnapshot::from_state(&state));
        assert_eq!(failed, "Needs attention");
        assert!(
            !failed.contains("192.168"),
            "the backend summary must never reach the shell: {failed}"
        );
    }

    /// A notice outranks the stream state, whatever the stream is doing.
    #[test]
    fn a_notice_outranks_every_stream_state_in_the_tray_phrase() {
        for stream in [
            StreamState::Stopped,
            StreamState::Starting {
                generation: crate::app::GenerationId(2),
            },
            StreamState::Streaming {
                generation: crate::app::GenerationId(2),
            },
            StreamState::Stopping {
                generation: crate::app::GenerationId(2),
            },
        ] {
            let state = AppState {
                stream: stream.clone(),
                notice: Some(UserNotice {
                    severity: Severity::Error,
                    code: NoticeCode::ControllerUnavailable,
                    summary: "device service stopped unexpectedly".into(),
                    action: None,
                }),
                ..Default::default()
            };
            assert_eq!(
                actual_state_label(&UiSnapshot::from_state(&state)),
                "Needs attention",
                "{stream:?}"
            );
        }
    }

    #[test]
    fn stopped_model_offers_start_gated_by_can_start() {
        let with_target = streaming_snapshot(false);
        let model = TrayModel::from_snapshot(&with_target);
        assert_eq!(
            model.session_action,
            TraySessionAction::Start { enabled: true }
        );
        assert!(model.tooltip.contains("Stopped"));

        let without_target = UiSnapshot::from_state(&AppState::default());
        let model = TrayModel::from_snapshot(&without_target);
        assert_eq!(
            model.session_action,
            TraySessionAction::Start { enabled: false }
        );
    }

    #[test]
    fn starting_and_streaming_offer_enabled_stop_with_active_count_tooltip() {
        for (case, _snapshot) in [
            ("starting", streaming_snapshot(false)),
            ("streaming", streaming_snapshot(true)),
        ] {
            let mut state = AppState {
                receivers: vec![receiver(1, "Kitchen", Availability::Available)],
                desired_receivers: HashSet::from_iter([rid(1)]),
                staged_receivers: HashSet::from_iter([rid(1)]),
                stream: if case == "starting" {
                    StreamState::Starting {
                        generation: crate::app::GenerationId(1),
                    }
                } else {
                    StreamState::Streaming {
                        generation: crate::app::GenerationId(1),
                    }
                },
                generations: crate::app::Generations {
                    session: crate::app::GenerationId(1),
                    ..Default::default()
                },
                ..Default::default()
            };
            if case == "streaming" {
                state.active_receivers.insert(rid(1));
            }
            let snapshot = snapshot_from(&mut state);

            let model = TrayModel::from_snapshot(&snapshot);
            assert_eq!(
                model.session_action,
                TraySessionAction::Stop { enabled: true },
                "{case}"
            );
            if case == "streaming" {
                assert!(model.tooltip.contains("1 active"), "{}", model.tooltip);
            }
        }

        let stopping_streaming = streaming_snapshot(true);
        let mut state = AppState {
            stream: StreamState::Stopping {
                generation: crate::app::GenerationId(9),
            },
            generations: crate::app::Generations {
                session: crate::app::GenerationId(9),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = stopping_streaming;
        let model = TrayModel::from_snapshot(&snapshot_from(&mut state));
        assert_eq!(model.session_action, TraySessionAction::StoppingProgress);
    }

    #[test]
    fn needs_attention_model_reports_notice_and_keeps_quit_available() {
        let mut state = AppState {
            notice: Some(UserNotice {
                severity: Severity::Error,
                code: NoticeCode::ControllerUnavailable,
                summary: "device service stopped unexpectedly".into(),
                action: None,
            }),
            ..Default::default()
        };
        let snapshot = snapshot_from(&mut state);
        let model = TrayModel::from_snapshot(&snapshot);

        assert!(model.tooltip.contains("Needs attention"));
        let items = menu_items(&model);
        assert!(items.contains(&TrayItem::Quit));
        // Quit stays clickable:
        assert_eq!(
            click(&model, &TrayItem::Quit),
            Some(TrayDispatch::One(crate::app::AppEvent::QuitRequested))
        );
    }

    #[test]
    fn menu_order_is_open_session_receivers_settings_quit() {
        let snapshot = streaming_snapshot(false);
        let model = TrayModel::from_snapshot(&snapshot);
        assert_eq!(
            menu_items(&model),
            vec![
                TrayItem::Open,
                TrayItem::Separator,
                TrayItem::Session,
                TrayItem::Receiver(0),
                TrayItem::Separator,
                TrayItem::Settings,
                TrayItem::Separator,
                TrayItem::Quit,
            ]
        );
    }

    #[test]
    fn clean_receiver_click_toggles_then_applies_in_fifo_order() {
        let snapshot = streaming_snapshot(false);
        let model = TrayModel::from_snapshot(&snapshot);

        assert_eq!(
            click(&model, &TrayItem::Receiver(0)),
            Some(TrayDispatch::Batch(vec![
                crate::app::AppEvent::ToggleStagedReceiver(rid(1)),
                crate::app::AppEvent::ApplyStagedReceivers,
            ]))
        );
    }

    #[test]
    fn unavailable_or_dirty_membership_blocks_receiver_mutation() {
        let mut state = AppState {
            receivers: vec![receiver(2, "Office", Availability::Unavailable)],
            desired_receivers: HashSet::from_iter([rid(2)]),
            staged_receivers: HashSet::from_iter([rid(2)]),
            ..Default::default()
        };
        let model = TrayModel::from_snapshot(&snapshot_from(&mut state));
        assert!(!model.receivers[0].enabled);
        assert_eq!(click(&model, &TrayItem::Receiver(0)), None);

        let mut dirty = AppState {
            receivers: vec![receiver(1, "Kitchen", Availability::Available)],
            desired_revision: 3,
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1), rid(7)]),
            staged_base_revision: 3,
            ..Default::default()
        };
        let model = TrayModel::from_snapshot(&snapshot_from(&mut dirty));
        assert!(model.has_pending_membership);
        assert_eq!(click(&model, &TrayItem::Receiver(0)), None);
    }

    #[test]
    fn settings_dispatches_navigate_then_show_and_open_shows_only() {
        let snapshot = streaming_snapshot(false);
        let model = TrayModel::from_snapshot(&snapshot);

        assert_eq!(
            click(&model, &TrayItem::Settings),
            Some(TrayDispatch::Batch(vec![
                crate::app::AppEvent::Navigate(crate::app::Page::Settings),
                crate::app::AppEvent::ShowMainWindow,
            ]))
        );
        assert_eq!(
            click(&model, &TrayItem::Open),
            Some(TrayDispatch::One(crate::app::AppEvent::ShowMainWindow))
        );
        assert_eq!(
            left_click_dispatch(),
            TrayDispatch::One(crate::app::AppEvent::ShowMainWindow)
        );
    }

    /// A surface that records what it was handed, with no Windows behind it.
    #[derive(Clone, Default)]
    struct RecordingSurface(std::sync::Arc<std::sync::Mutex<Vec<Vec<TrayEntry>>>>);

    impl TraySurface for RecordingSurface {
        fn apply(&mut self, _tooltip: &str, entries: &[TrayEntry]) {
            self.0.lock().unwrap().push(entries.to_vec());
        }
    }

    /// The menu is rebuilt per published revision, not per frame.
    ///
    /// The window redraws on every OS input event; rebuilding a shell menu
    /// each time would replace the very item a click is being delivered to.
    #[test]
    fn the_controller_rebuilds_once_per_revision_and_not_once_per_frame() {
        let recorded = RecordingSurface::default();
        let seen = std::sync::Arc::clone(&recorded.0);
        let mut controller = TrayController::with_surface(Box::new(recorded));

        let mut state = AppState {
            receivers: vec![receiver(1, "Kitchen", Availability::Available)],
            ..AppState::default()
        };
        let first = snapshot_from(&mut state);
        assert!(
            controller.sync(&first),
            "the first snapshot has to build a menu"
        );
        assert!(
            !controller.sync(&first),
            "the same revision must not rebuild the menu"
        );
        assert!(!controller.sync(&first));
        assert_eq!(seen.lock().unwrap().len(), 1);

        state
            .receivers
            .push(receiver(2, "Study", Availability::Available));
        // The reducer bumps this on every accepted event; a hand-built state
        // has to move it too, or the gate under test is never crossed.
        state.revision += 1;
        let second = snapshot_from(&mut state);
        assert_ne!(
            second.revision, first.revision,
            "the fixture has to move the revision, or this proves nothing"
        );
        assert!(controller.sync(&second), "a new revision has to rebuild");

        let menus = seen.lock().unwrap().clone();
        assert_eq!(menus.len(), 2);
        let names = |menu: &Vec<TrayEntry>| {
            menu.iter()
                .filter_map(|row| match row {
                    TrayEntry::Command {
                        checked: Some(_),
                        label,
                        ..
                    } => Some(label.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&menus[0]), vec!["Kitchen".to_owned()]);
        assert_eq!(
            names(&menus[1]),
            vec!["Kitchen".to_owned(), "Study".to_owned()],
            "the second menu has to carry what the second revision says"
        );
    }

    /// The tooltip is shell-drawn copy and has to survive as text.
    ///
    /// It carried a UTF-8 em dash that had been decoded as Latin-1 and
    /// re-encoded, so the balloon read "OpenAirCast Ã¢â‚¬â€ Stopped".
    #[test]
    fn the_tray_tooltip_is_well_formed_text() {
        let tooltip = tooltip_for(&streaming_snapshot(true));
        assert!(tooltip.starts_with("OpenAirCast"), "{tooltip}");
        for suspect in ['\u{fffd}', '\u{c3}', '\u{e2}', '\u{201a}', '\u{2020}'] {
            assert!(
                !tooltip.contains(suspect),
                "the tooltip carries mis-decoded text ({suspect:?}): {tooltip}"
            );
        }
        assert!(
            tooltip.contains(&actual_state_label(&streaming_snapshot(true))),
            "the tooltip lost the state it exists to report: {tooltip}"
        );
    }

    /// A row the shell draws as usable has to do something, and a row that
    /// does nothing has to be drawn as unusable. Nothing in the menu may be
    /// a decoration the pointer can land on.
    #[test]
    fn every_menu_row_is_either_usable_and_wired_or_inert_and_greyed() {
        let mut pending = AppState {
            receivers: vec![
                receiver(1, "Kitchen", Availability::Available),
                receiver(2, "Study", Availability::Unavailable),
            ],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1), rid(2)]),
            ..AppState::default()
        };
        let snapshots = [
            streaming_snapshot(false),
            streaming_snapshot(true),
            snapshot_from(&mut pending),
        ];
        for snapshot in snapshots {
            let model = TrayModel::from_snapshot(&snapshot);
            let entries = tray_entries(&model);
            assert!(entries.len() > 3, "the menu collapsed to {entries:?}");
            for entry in entries {
                let TrayEntry::Command {
                    label,
                    enabled,
                    dispatch,
                    ..
                } = entry
                else {
                    continue;
                };
                assert!(!label.is_empty(), "an unnamed row reached the menu");
                assert_eq!(
                    enabled,
                    dispatch.is_some(),
                    "{label:?} is enabled={enabled} but dispatches {dispatch:?}"
                );
            }
        }
    }
}

/// Drives the tray from published snapshots and rebuilds it only when the
/// revision moved.
///
/// The surface behind it is a trait so the drive itself is observable: this
/// controller used to be constructed, handed to the shell, and then never
/// asked for anything again, which left the icon showing the empty menu it
/// was built with.
pub(crate) struct TrayController {
    surface: Box<dyn TraySurface>,
    last_revision: u64,
}

impl TrayController {
    /// The production controller: a real Windows tray icon and its menu.
    pub(crate) fn new(handle: crate::app_handle::AppHandle) -> anyhow::Result<Self> {
        Ok(Self::with_surface(Box::new(SystemTray::new(handle)?)))
    }

    /// A controller over any surface. Used by the shell tests to observe the
    /// menu the user would actually get.
    pub(crate) fn with_surface(surface: Box<dyn TraySurface>) -> Self {
        Self {
            surface,
            last_revision: u64::MAX, // force first sync even for revision 0
        }
    }

    /// Rebuilds menu/tooltip only when the snapshot revision changed.
    pub(crate) fn sync(&mut self, snapshot: &crate::app::UiSnapshot) -> bool {
        if snapshot.revision == self.last_revision {
            return false;
        }
        self.last_revision = snapshot.revision;

        let model = TrayModel::from_snapshot(snapshot);
        self.surface.apply(&model.tooltip, &tray_entries(&model));
        true
    }
}

/// The real Windows tray icon: the only part of this module that touches the
/// shell.
struct SystemTray {
    icon: TrayIcon,
    dispatches:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<MenuId, Option<TrayDispatch>>>>,
}

impl SystemTray {
    fn new(handle: crate::app_handle::AppHandle) -> anyhow::Result<Self> {
        let icon = load_tray_icon()?;

        let dispatches: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<MenuId, Option<TrayDispatch>>>,
        > = std::sync::Arc::default();

        // Handlers are process-global singletons; they never mutate the tray.
        {
            let dispatches = std::sync::Arc::clone(&dispatches);
            let handle = handle.clone();
            tray_icon::menu::MenuEvent::set_event_handler(Some(
                move |event: tray_icon::menu::MenuEvent| {
                    let slot = dispatches
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&event.id).cloned());
                    if let Some(Some(dispatch)) = slot {
                        let events = match dispatch {
                            TrayDispatch::One(event) => vec![event],
                            TrayDispatch::Batch(events) => events,
                        };
                        for event in events {
                            let _ = crate::app_handle::AppHandle::dispatch(&handle, event);
                        }
                    }
                },
            ));
        }
        {
            let handle = handle.clone();
            tray_icon::TrayIconEvent::set_event_handler(Some(
                move |event: tray_icon::TrayIconEvent| {
                    use tray_icon::{MouseButton, MouseButtonState};
                    if let tray_icon::TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = crate::app_handle::AppHandle::dispatch(
                            &handle,
                            crate::app::AppEvent::ShowMainWindow,
                        );
                    }
                },
            ));
        }

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(Menu::new()))
            .with_icon(icon)
            .build()?;

        Ok(Self {
            icon: tray,
            dispatches,
        })
    }
}

impl TraySurface for SystemTray {
    fn apply(&mut self, tooltip: &str, entries: &[TrayEntry]) {
        let _ = self.icon.set_tooltip(Some(tooltip.to_owned()));

        let menu = Menu::new();
        let mut new_dispatches = std::collections::HashMap::new();
        for entry in entries {
            match entry {
                TrayEntry::Separator => {
                    menu.append(&PredefinedMenuItem::separator()).ok();
                }
                TrayEntry::Command {
                    label,
                    enabled,
                    checked: Some(checked),
                    dispatch,
                } => {
                    let item = CheckMenuItem::new(label, *checked, *enabled, None);
                    menu.append(&item).ok();
                    new_dispatches.insert(item.id().clone(), dispatch.clone());
                }
                TrayEntry::Command {
                    label,
                    enabled,
                    checked: None,
                    dispatch,
                } => {
                    let item = MenuItem::new(label, *enabled, None);
                    menu.append(&item).ok();
                    new_dispatches.insert(item.id().clone(), dispatch.clone());
                }
            }
        }

        if let Ok(mut map) = self.dispatches.lock() {
            *map = new_dispatches;
        }
        self.icon.set_menu(Some(Box::new(menu)));
    }
}

fn load_tray_icon() -> anyhow::Result<Icon> {
    if let Ok(icon) = Icon::from_resource(1, Some((32, 32))) {
        return Ok(icon);
    }
    let img = image::load_from_memory(super::ui::theme::APP_ICON_PNG)?;
    let rgba = img.to_rgba8().into_raw();
    Ok(Icon::from_rgba(rgba, img.width(), img.height())?)
}
