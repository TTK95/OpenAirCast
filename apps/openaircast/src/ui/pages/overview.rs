//! Overview / Übersicht -- the Command Home.
//!
//! The daily control surface: what the session is doing, where the audio
//! goes, which speakers are selected, how loud they are, and the one thing
//! that needs the user right now. Everything on it is reachable without
//! navigating away.
//!
//! The order is fixed by the specification and asserted by the tests below:
//! status hero with the Route Ribbon, the receiver section, the Audio Dock,
//! one inline notice, and -- in Advanced mode, with real data -- the metric
//! strip.
//!
//! Two things this page deliberately does *not* own:
//!
//! * **The page header and its primary command.** The App Shell draws them,
//!   so the single filled action per context stays arbitrated in one place.
//!   [`OverviewModel::primary`] states which command the phase has; it is not
//!   a second button.
//! * **The staged-change bar.** The Shell pins it to the bottom of the page
//!   viewport. A bar placed here would be a second Apply in the tab order and
//!   a second owner of the page geometry.

use crate::app::{AppEvent, CorrectiveAction, Page, UiSnapshot};
use crate::ui::components::{
    audio_dock, command_action, empty_state, metric_tile, notice, receiver_card, receiver_level,
    route_ribbon,
};
use crate::ui::i18n::{Catalog, ReceiverCountArgs, TextKey};
use crate::ui::layout::{receiver_columns_for_content, UiResources};
use crate::ui::presentation::{
    CommandActionModel, Emphasis, EmptyStateModel, MetricTileModel, OverviewModel,
    ReceiverCardModel, DENSE_PADDING, SECTION_GAP,
};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Renders the Overview destination body.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let model = OverviewModel::from_snapshot(snapshot, resources.catalog);
    let tokens = resources.tokens;

    show_hero(ui, tokens, &model);
    ui.add_space(SECTION_GAP);
    show_receivers(
        ui,
        tokens,
        resources.catalog,
        snapshot,
        &model.receivers,
        emit,
    );
    ui.add_space(SECTION_GAP);
    audio_dock::show(ui, tokens, &model.audio, emit);

    if let Some(model) = &model.notice {
        ui.add_space(SECTION_GAP);
        notice::show(ui, tokens, model, &mut |corrective| {
            emit(corrective_event(corrective));
        });
    }

    if !model.advanced_metrics.is_empty() {
        ui.add_space(SECTION_GAP);
        show_metrics(ui, tokens, resources.catalog, &model.advanced_metrics);
    }
}

/// The status hero: what the session is doing, why that matters, and where
/// the audio goes.
///
/// The phase is an eyebrow, not a heading. It used to be drawn in the hero
/// role, which made a one-word state the largest thing on the page after the
/// page title -- outranking every section under it. An eyebrow is what it
/// actually is: a small label that classifies the sentence below it. The
/// marker travels with it so the phase survives without colour, and the
/// announced name stays the bare word.
///
/// This is the copy that outranks the other candidate. The rail's status
/// footer used to paint the same word a second time, in the same size class,
/// with nothing to rank the two; it now prints it only on the destinations
/// whose own body says nothing about the phase, which by
/// [`crate::ui::pages::prints_status_word`] is every destination except this
/// one.
fn show_hero(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &OverviewModel) {
    crate::ui::components::show_labeled_text(
        ui,
        TypographyRole::Eyebrow,
        tokens.ink_muted,
        &hero_eyebrow(model),
        &model.title,
    );
    super::show_text(ui, TypographyRole::Body, tokens.ink, &model.explanation);
    ui.add_space(DENSE_PADDING);
    route_ribbon::show(ui, tokens, &model.route);
}

/// The Command Home's own statement of the session phase.
///
/// Paragraph 7.1 puts the phase here, so this copy wins and the rail's status
/// footer stays silent on this destination alone. The tests state the rule
/// from the window's side: the phase word is painted exactly once, whichever
/// destination is open.
fn hero_eyebrow(model: &OverviewModel) -> String {
    format!("{} {}", model.phase.symbol(), model.title)
}

/// The receiver section: the count, then the selection grid.
///
/// With nothing discovered there is nothing to select, and the page says so
/// with the one empty state that carries a real command -- Refresh is a typed
/// event, not a promise.
fn show_receivers(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    catalog: Catalog,
    snapshot: &UiSnapshot,
    receivers: &[ReceiverCardModel],
    emit: &mut dyn FnMut(AppEvent),
) {
    if receivers.is_empty() {
        empty_state::show(ui, tokens, &EmptyStateModel::no_receivers(catalog), emit);
        return;
    }

    ui.horizontal(|header| {
        super::show_text(
            header,
            TypographyRole::SectionTitle,
            tokens.ink,
            &catalog.receiver_count(ReceiverCountArgs {
                count: receivers.len(),
            }),
        );
        header.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |actions| {
                // Quiet on purpose: asking discovery to look again is a
                // maintenance command, and the one emphasised action of this
                // context belongs to the Shell.
                let refresh = CommandActionModel::new(
                    catalog.text(TextKey::RefreshReceivers).to_owned(),
                    Emphasis::Quiet,
                    true,
                );
                if command_action::show(actions, tokens, &refresh).clicked() {
                    emit(AppEvent::RefreshRequested);
                }
            },
        );
    });
    ui.add_space(DENSE_PADDING);

    // The grid asks the layout module how many columns its own measure
    // affords, so the page and the window-level metrics cannot disagree
    // about where the second column appears.
    let columns = receiver_columns_for_content(ui.available_width());
    for row in receivers.chunks(columns) {
        ui.columns(columns, |cells| {
            for (cell, card) in cells.iter_mut().zip(row) {
                receiver_card::show(cell, tokens, card, emit);
                if let Some(level) =
                    crate::ui::presentation::receiver_level_model(snapshot, &card.id, catalog)
                {
                    receiver_level::show(cell, tokens, &level, snapshot.shutting_down, emit);
                }
            }
        });
    }
}

/// The Advanced metric strip. It is only reached when the model produced
/// measurements, so an empty strip is never drawn as a headline over nothing.
fn show_metrics(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    catalog: Catalog,
    metrics: &[MetricTileModel],
) {
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        tokens.ink,
        catalog.text(TextKey::AdvancedInformation),
    );
    ui.add_space(DENSE_PADDING);
    ui.columns(metrics.len(), |cells| {
        for (cell, tile) in cells.iter_mut().zip(metrics) {
            metric_tile::show(cell, tokens, tile);
        }
    });
}

/// The typed event a corrective action stands for.
///
/// A failed preference write is scoped on purpose: a bare Retry here would
/// read as "retry the stream" and, worse, would dispatch it.
fn corrective_event(action: CorrectiveAction) -> AppEvent {
    match action {
        CorrectiveAction::Refresh => AppEvent::RefreshRequested,
        CorrectiveAction::Retry => AppEvent::StartRequested,
        CorrectiveAction::RetryPreferencesPersistence => AppEvent::RetryPreferencesPersistence,
        CorrectiveAction::OpenSettings => AppEvent::Navigate(Page::Settings),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::app::{
        AppState, Availability, CorrectiveAction, DiscoveryState, GenerationId, NoticeCode, Page,
        ReceiverState, ResolvedLocale, Severity, StreamState, ThemePreference, UserNotice,
    };
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::components::app_shell::{change_bar_model, show_shell};
    use crate::ui::i18n::{Catalog, ReceiverCountArgs, TextKey};
    use crate::ui::presentation::{OverviewModel, VOLUME_STEP};
    use crate::ui::theme;

    const TEST_APPEARANCE: SystemAppearance = SystemAppearance {
        client_animation_enabled: false,
        high_contrast: false,
        colors: SystemColors {
            background: [255, 255, 255],
            foreground: [0, 0, 0],
            highlight: [0, 120, 215],
            highlight_text: [255, 255, 255],
            disabled_text: [128, 128, 128],
            link: [0, 102, 204],
        },
    };

    const LOCALES: [ResolvedLocale; 2] = [ResolvedLocale::German, ResolvedLocale::English];
    const WIDE: [f32; 2] = [1120.0, 720.0];
    const NARROW: [f32; 2] = [900.0, 600.0];

    fn rid(last: u8) -> DeviceId {
        DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last])
    }

    fn receiver(last: u8, name: &str) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "AudioAccessory1,1".into(),
            availability: Availability::Available,
        }
    }

    /// Receiver names are product names on purpose: the language guard must
    /// not trip over user data that reads the same in both locales.
    fn base_state() -> AppState {
        AppState {
            page: Page::Home,
            receivers: vec![receiver(1, "HomePod"), receiver(2, "HomePod mini")],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            discovery: DiscoveryState::Ready,
            master_volume: 0.6,
            ..AppState::default()
        }
    }

    type ShellHarness = egui_kittest::Harness<'static, Vec<AppEvent>>;

    fn shell(locale: ResolvedLocale, size: [f32; 2], state: AppState) -> ShellHarness {
        let mut harness: ShellHarness = egui_kittest::Harness::new_ui_state(
            move |ui, log: &mut Vec<AppEvent>| {
                let resolved = theme::resolve_theme(ThemePreference::Light, None, TEST_APPEARANCE);
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
        harness
    }

    /// What the page body reported about itself while it drew.
    ///
    /// The dock's card has no accessibility node, so the only honest way to
    /// compare its edges with the receiver grid's is to have the renderer
    /// report the rect it painted.
    struct BodyGeometry {
        dock: egui::Rect,
        events: Vec<AppEvent>,
    }

    /// The page body, in a column of exactly the measure the Shell would give
    /// it at this window size.
    ///
    /// Not the full Shell: the Shell is what *hands out* the measure, and the
    /// question here is whether the two sections inside the column agree
    /// about the column. The width therefore comes from the same two layout
    /// functions the Shell calls, so this cannot silently test a measure the
    /// window never produces.
    fn page_body(
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
    ) -> egui_kittest::Harness<'static, BodyGeometry> {
        use crate::ui::layout::{readable_width, LayoutMetrics};

        let mut harness: egui_kittest::Harness<'static, BodyGeometry> =
            egui_kittest::Harness::new_ui_state(
                move |ui, body: &mut BodyGeometry| {
                    let resolved =
                        theme::resolve_theme(ThemePreference::Light, None, TEST_APPEARANCE);
                    theme::apply_theme(ui.ctx(), &resolved);
                    let catalog = Catalog::new(locale);
                    let snapshot = UiSnapshot::from_state(&state);
                    let overview = OverviewModel::from_snapshot(&snapshot, catalog);
                    let metrics = LayoutMetrics::for_window_width(size[0]);
                    ui.set_max_width(readable_width(metrics.content_width(size[0])));

                    let mut events = Vec::new();
                    {
                        let mut emit = |event: AppEvent| events.push(event);
                        show_receivers(
                            ui,
                            &resolved.tokens,
                            catalog,
                            &snapshot,
                            &overview.receivers,
                            &mut emit,
                        );
                        ui.add_space(SECTION_GAP);
                        body.dock =
                            audio_dock::show(ui, &resolved.tokens, &overview.audio, &mut emit).card;
                    }
                    body.events.extend(events);
                },
                BodyGeometry {
                    dock: egui::Rect::ZERO,
                    events: Vec::new(),
                },
            );
        harness.set_size(egui::vec2(size[0], size[1]));
        harness.step();
        harness
    }

    fn model(state: &AppState, locale: ResolvedLocale) -> OverviewModel {
        OverviewModel::from_snapshot(&UiSnapshot::from_state(state), Catalog::new(locale))
    }

    /// Every name and value the accessibility tree exposes, in tree order.
    ///
    /// Both halves are needed: egui reports a `Label` node's text as its
    /// *value* and every other widget's text as its *label*, so a collector
    /// that read only one of them would silently miss every sentence on the
    /// page.
    fn labels_in_order(harness: &ShellHarness) -> Vec<String> {
        crate::ui::components::test_support::accessible_strings(harness.root())
    }

    /// The one node with this role and this name.
    fn node_named<'a>(
        harness: &'a ShellHarness,
        role: accesskit::Role,
        name: &str,
    ) -> egui_kittest::Node<'a> {
        harness
            .root()
            .children_recursive()
            .find(|node| {
                let node = node.accesskit_node();
                node.role() == role && node.label().is_some_and(|label| label == name)
            })
            .unwrap_or_else(|| panic!("no {role:?} named {name:?}"))
    }

    /// The rect of the one text node carrying exactly this string.
    ///
    /// egui reports a `Label`'s text as its accesskit *value* and every other
    /// widget's as its *label*, so both have to be consulted.
    fn text_node(harness: &ShellHarness, text: &str) -> egui::Rect {
        harness
            .root()
            .children_recursive()
            .find(|node| {
                let accesskit = node.accesskit_node();
                accesskit.label().is_some_and(|label| label == text)
                    || accesskit.value().is_some_and(|value| value == text)
            })
            .unwrap_or_else(|| panic!("no text node says {text:?}"))
            .rect()
    }

    /// Prefers an exact match: several sentences on the page legitimately
    /// contain a shorter one, and a substring hit would then measure the
    /// wrong node's position.
    fn index_of(labels: &[String], needle: &str) -> usize {
        labels
            .iter()
            .position(|label| label == needle)
            .or_else(|| labels.iter().position(|label| label.contains(needle)))
            .unwrap_or_else(|| panic!("{needle:?} never reached the page: {labels:?}"))
    }

    mod order_and_layout {
        use super::*;

        /// The specification fixes the order: status hero with the Route
        /// Ribbon, receivers, Audio Dock, notice, advanced metrics.
        #[test]
        fn the_overview_sections_appear_in_the_specified_order_in_both_locales() {
            for locale in LOCALES {
                let mut state = base_state();
                state.advanced_information = true;
                state.notice = Some(UserNotice {
                    severity: Severity::Warning,
                    code: NoticeCode::DiscoveryFailed,
                    summary: "Network unavailable".into(),
                    action: Some(CorrectiveAction::Refresh),
                });

                let overview = model(&state, locale);
                let catalog = Catalog::new(locale);
                let harness = shell(locale, WIDE, state.clone());
                let labels = labels_in_order(&harness);

                let positions = [
                    ("hero title", index_of(&labels, &overview.title)),
                    ("route summary", index_of(&labels, &overview.route.summary)),
                    (
                        "receiver count",
                        index_of(
                            &labels,
                            &catalog.receiver_count(ReceiverCountArgs {
                                count: overview.receivers.len(),
                            }),
                        ),
                    ),
                    (
                        "first receiver card",
                        index_of(&labels, &overview.receivers[0].accessible_name),
                    ),
                    ("audio dock", index_of(&labels, &overview.audio.label)),
                    (
                        "notice",
                        index_of(&labels, &overview.notice.as_ref().unwrap().message),
                    ),
                    (
                        "advanced metrics",
                        index_of(&labels, &overview.advanced_metrics[0].accessible_phrase()),
                    ),
                ];
                for pair in positions.windows(2) {
                    assert!(
                        pair[0].1 < pair[1].1,
                        "{locale:?}: {} came after {}",
                        pair[0].0,
                        pair[1].0
                    );
                }
            }
        }

        /// The state word belongs in exactly one place in the window.
        ///
        /// "Bereit" stood twice: as the hero's eyebrow above the Route
        /// Ribbon, and again in the rail's status footer, in the same size
        /// class, with nothing to rank the two. The hero's copy is the one
        /// that survives -- paragraph 7.1 lists the phase as part of the
        /// status hero, and the hero is where a reader is already looking --
        /// so the rail stopped painting its second copy and reports the state
        /// as a shape, a tooltip, and an accessible name instead.
        ///
        /// Read off the paint output rather than off the accessibility tree:
        /// the rail still *announces* the word, and a test that could not
        /// tell announcing from painting would pass on the wrong evidence.
        #[test]
        fn the_state_word_is_painted_once_in_the_whole_window() {
            for locale in LOCALES {
                let state = base_state();
                let overview = model(&state, locale);
                let catalog = Catalog::new(locale);
                let harness = shell(locale, WIDE, state);
                let word = catalog.text(TextKey::Ready);
                let painted = crate::ui::components::test_support::painted_strings(&harness);
                let carrying: Vec<&String> =
                    painted.iter().filter(|line| line.contains(word)).collect();
                assert_eq!(
                    carrying.len(),
                    1,
                    "{locale:?}: {word:?} is painted in {carrying:?}"
                );
                assert_eq!(
                    carrying[0],
                    &hero_eyebrow(&overview),
                    "{locale:?}: the surviving copy has to be the hero's"
                );
            }
        }

        /// "Ready" is a status, not a section.
        ///
        /// The page drew the session phase in the hero role, which made a
        /// one-word state the largest thing on the page after the page title
        /// itself -- larger than every actual section heading under it.
        /// Measured as rendered height against the section heading beside
        /// it, not against the type scale the renderer reads.
        #[test]
        fn the_session_phase_is_never_larger_than_a_section_heading_beside_it() {
            for locale in LOCALES {
                let state = base_state();
                let overview = model(&state, locale);
                let catalog = Catalog::new(locale);
                let harness = shell(locale, WIDE, state);

                let phase = text_node(&harness, &overview.title);
                let section = text_node(
                    &harness,
                    &catalog.receiver_count(ReceiverCountArgs {
                        count: overview.receivers.len(),
                    }),
                );
                assert!(
                    phase.height() <= section.height(),
                    "{locale:?}: the phase word is {} tall beside a {} section heading",
                    phase.height(),
                    section.height()
                );
            }
        }

        /// A window wider than the readable measure must not leave all of the
        /// slack on one side.
        ///
        /// The content was capped at the readable measure and left-aligned,
        /// so at 1275 points the cards stopped around x=1130 and the last
        /// 145 points of the window carried nothing at all. Capped content is
        /// legitimate; capped content shoved against the rail is not.
        #[test]
        fn a_window_wider_than_the_readable_measure_centres_its_content() {
            const WIDE_WINDOW: [f32; 2] = [1275.0, 900.0];
            let state = base_state();
            let overview = model(&state, ResolvedLocale::English);
            let harness = shell(ResolvedLocale::English, WIDE_WINDOW, state);

            let rects: Vec<egui::Rect> = overview
                .receivers
                .iter()
                .map(|card| harness.get_by_label(card.accessible_name.as_str()).rect())
                .collect();
            let content_left = rects
                .iter()
                .map(|rect| rect.left())
                .fold(f32::INFINITY, f32::min);
            let content_right = rects
                .iter()
                .map(|rect| rect.right())
                .fold(f32::NEG_INFINITY, f32::max);

            // The rail occupies the left edge; the slack either side of the
            // content is what has to match.
            let rail = crate::ui::layout::LayoutMetrics::for_window_width(WIDE_WINDOW[0]);
            let left_slack = content_left - rail.rail_width;
            let right_slack = WIDE_WINDOW[0] - content_right;
            assert!(
                (left_slack - right_slack).abs() <= 4.0,
                "the content sits {left_slack} from the rail and {right_slack} from the \
                 right edge of a {}-point window",
                WIDE_WINDOW[0]
            );
        }

        #[test]
        fn the_receiver_grid_is_two_columns_wide_and_one_column_narrow() {
            let state = base_state();
            let overview = model(&state, ResolvedLocale::English);
            let first = overview.receivers[0].accessible_name.as_str();
            let second = overview.receivers[1].accessible_name.as_str();

            let wide = shell(ResolvedLocale::English, WIDE, state.clone());
            let left = wide.get_by_label(first).rect();
            let right = wide.get_by_label(second).rect();
            assert!(
                (left.top() - right.top()).abs() < 1.0 && right.left() > left.left(),
                "1120 x 720 must place two cards side by side: {left:?} {right:?}"
            );

            let narrow = shell(ResolvedLocale::English, NARROW, state);
            let upper = narrow.get_by_label(first).rect();
            let lower = narrow.get_by_label(second).rect();
            assert!(
                lower.top() >= upper.bottom() - 1.0,
                "900 x 600 must stack the cards: {upper:?} {lower:?}"
            );
        }

        /// The receiver grid and the Audio Dock are handed the same measure,
        /// so they have to end at the same place.
        ///
        /// They did not. The dock's card stopped 26 points short of the cards
        /// above it, because the card is sized from what its content used and
        /// the percentage cell -- reserved at `PERCENT_WIDTH` so the slider
        /// would not resize as the number grows -- only ever *used* the width
        /// of the string in it. The reservation was real for the slider's
        /// arithmetic and imaginary for the card's.
        ///
        /// Stated as an edge against an edge rather than as a width: a fixed
        /// number would have to be rewritten for every window size and would
        /// hold nothing about the two containers agreeing.
        ///
        /// The volumes are walked because the cause was text-width dependent:
        /// the card ended `PERCENT_WIDTH` minus the width of the percentage
        /// short, so a test run at one volume would have measured one
        /// particular string. "0 %" and "100 %" are the two ends of it.
        #[test]
        fn the_audio_dock_and_the_receiver_grid_share_the_pages_edges() {
            for (size, volume) in [WIDE, NARROW]
                .into_iter()
                .flat_map(|size| [0.0f32, 0.6, 1.0].map(|volume| (size, volume)))
            {
                let mut state = base_state();
                state.master_volume = volume;
                let overview = model(&state, ResolvedLocale::English);
                let harness = page_body(ResolvedLocale::English, size, state);
                let cards: Vec<egui::Rect> = overview
                    .receivers
                    .iter()
                    .map(|card| harness.get_by_label(card.accessible_name.as_str()).rect())
                    .collect();
                let grid_left = cards
                    .iter()
                    .map(|rect| rect.left())
                    .fold(f32::INFINITY, f32::min);
                let grid_right = cards
                    .iter()
                    .map(|rect| rect.right())
                    .fold(f32::NEG_INFINITY, f32::max);
                let dock = harness.state().dock;

                assert!(
                    (dock.left() - grid_left).abs() <= 0.5,
                    "{size:?} at {volume}: the dock starts at {} and the grid at {grid_left}",
                    dock.left()
                );
                assert!(
                    (dock.right() - grid_right).abs() <= 0.5,
                    "{size:?} at {volume}: the dock ends at {} and the grid at {grid_right}",
                    dock.right()
                );
            }
        }

        /// A page that placed its own bar would put a second Apply into the
        /// tab order; the Shell owns exactly one.
        #[test]
        fn the_overview_never_places_a_change_bar_of_its_own() {
            let mut state = base_state();
            state.staged_receivers.insert(rid(2));
            let harness = shell(ResolvedLocale::English, WIDE, state);
            let apply = Catalog::new(ResolvedLocale::English).text(TextKey::ApplyChanges);
            let count = labels_in_order(&harness)
                .iter()
                .filter(|label| label.as_str() == apply)
                .count();
            assert_eq!(count, 1, "exactly one Apply reaches the page");
        }

        #[test]
        fn nothing_the_overview_paints_leaves_the_window_at_either_target_size() {
            for size in [WIDE, NARROW] {
                let mut state = base_state();
                state.advanced_information = true;
                let overview = model(&state, ResolvedLocale::German);
                let harness = shell(ResolvedLocale::German, size, state);
                for card in &overview.receivers {
                    let rect = harness.get_by_label(card.accessible_name.as_str()).rect();
                    assert!(
                        rect.right() <= size[0] + 1.0 && rect.left() >= -1.0,
                        "{size:?}: {rect:?} left the window"
                    );
                }
            }
        }
    }

    mod dispatch {
        use super::*;

        fn events(mut harness: ShellHarness, label: &str) -> Vec<AppEvent> {
            harness.get_by_label(label).click();
            harness.step();
            harness.state().clone()
        }

        #[test]
        fn the_lifecycle_command_dispatches_exactly_the_typed_event_for_every_state() {
            let catalog = Catalog::new(ResolvedLocale::English);
            for (stream, label, expected) in [
                (
                    StreamState::Stopped,
                    catalog.text(TextKey::StartStreaming),
                    AppEvent::StartRequested,
                ),
                (
                    StreamState::Starting {
                        generation: GenerationId(1),
                    },
                    catalog.text(TextKey::Cancel),
                    AppEvent::StopRequested,
                ),
                (
                    StreamState::Streaming {
                        generation: GenerationId(2),
                    },
                    catalog.text(TextKey::StopStreaming),
                    AppEvent::StopRequested,
                ),
                (
                    StreamState::Failed {
                        generation: GenerationId(3),
                        summary: "Could not connect".into(),
                    },
                    catalog.text(TextKey::Retry),
                    AppEvent::StartRequested,
                ),
            ] {
                let mut state = base_state();
                state.stream = stream.clone();
                let harness = shell(ResolvedLocale::English, WIDE, state);
                assert_eq!(
                    events(harness, label),
                    vec![expected],
                    "{stream:?} behind {label}"
                );
            }
        }

        /// Stopping draws its command so the header does not jump, and that
        /// command can never be activated.
        #[test]
        fn the_stopping_command_is_visible_and_inert() {
            let mut state = base_state();
            state.stream = StreamState::Stopping {
                generation: GenerationId(4),
            };
            let catalog = Catalog::new(ResolvedLocale::English);
            let mut harness = shell(ResolvedLocale::English, WIDE, state);
            {
                // The hero title says "Stopping" too, so the command has to
                // be picked by role rather than by text.
                let node = node_named(
                    &harness,
                    accesskit::Role::Button,
                    catalog.text(TextKey::Stopping),
                );
                assert!(node.accesskit_node().is_disabled());
                node.click();
            }
            harness.step();
            assert!(harness.state().is_empty());
        }

        /// Nothing to start is not nothing to show.
        ///
        /// With no available applied receiver the header used to draw no
        /// command at all: beside the page title stood empty space, and the
        /// header's geometry changed the moment a selection was applied. The
        /// command is drawn, inert, reported disabled, and its accessible
        /// name carries the precondition -- the same treatment Stopping
        /// already gets.
        #[test]
        fn the_start_command_is_visible_and_inert_when_nothing_can_be_started() {
            for locale in LOCALES {
                let mut state = base_state();
                state.receivers[0].availability = Availability::Unavailable;
                let catalog = Catalog::new(locale);
                let mut harness = shell(locale, WIDE, state);
                let title = catalog.text(TextKey::Overview);
                {
                    let node = node_named(
                        &harness,
                        accesskit::Role::Button,
                        &catalog.named_value(crate::ui::i18n::NamedValueArgs {
                            name: catalog.text(TextKey::StartStreaming),
                            value: catalog.text(TextKey::ReadyExplanation),
                        }),
                    );
                    assert!(
                        node.accesskit_node().is_disabled(),
                        "{locale:?}: a command that cannot run must report so"
                    );
                    // Header geometry, not merely presence: the command sits
                    // on the page title's own row, which is what stops the
                    // header from changing shape with the state.
                    let header = text_node(&harness, title);
                    assert!(
                        node.rect().center().y > header.top()
                            && node.rect().center().y < header.bottom(),
                        "{locale:?}: the command left the header row"
                    );
                    node.click();
                }
                harness.step();
                assert!(
                    harness.state().is_empty(),
                    "{locale:?}: an inert command must not dispatch"
                );
            }
        }

        /// Nothing to retry is not something to offer.
        ///
        /// A failed session drew Retry filled and enabled even when the
        /// selection held nothing reachable. Clicking it sent
        /// `StartRequested`, which the reducer discards for exactly the reason
        /// that made the command invalid -- so the most prominent control on
        /// the page did nothing at all, twice over: once in the window, once
        /// in the state machine.
        #[test]
        fn the_retry_command_is_visible_and_inert_when_nothing_can_be_started() {
            for locale in LOCALES {
                let mut state = base_state();
                state.receivers[0].availability = Availability::Unavailable;
                state.stream = StreamState::Failed {
                    generation: GenerationId(9),
                    summary: "Could not connect".into(),
                };
                let catalog = Catalog::new(locale);
                let mut harness = shell(locale, WIDE, state);
                {
                    let node = node_named(
                        &harness,
                        accesskit::Role::Button,
                        &catalog.named_value(crate::ui::i18n::NamedValueArgs {
                            name: catalog.text(TextKey::Retry),
                            value: catalog.text(TextKey::ReadyExplanation),
                        }),
                    );
                    assert!(
                        node.accesskit_node().is_disabled(),
                        "{locale:?}: a command that cannot run must report so"
                    );
                    node.click();
                }
                harness.step();
                assert!(
                    harness.state().is_empty(),
                    "{locale:?}: an inert command must not dispatch"
                );
            }
        }

        #[test]
        fn a_receiver_card_dispatches_one_toggle_for_its_own_receiver() {
            let state = base_state();
            let overview = model(&state, ResolvedLocale::English);
            let harness = shell(ResolvedLocale::English, WIDE, state);
            assert_eq!(
                events(harness, overview.receivers[1].accessible_name.as_str()),
                vec![AppEvent::ToggleStagedReceiver(rid(2))]
            );
        }

        #[test]
        fn the_master_volume_steps_with_the_keyboard_and_dispatches_the_typed_event() {
            let state = base_state();
            let overview = model(&state, ResolvedLocale::English);
            let mut harness = shell(ResolvedLocale::English, WIDE, state);
            harness
                .get_by_label(overview.audio.accessible_name.as_str())
                .focus();
            harness.step();
            assert!(harness.state().is_empty(), "focus alone must not act");
            harness.key_press(egui::Key::ArrowRight);
            harness.step();
            match harness.state().as_slice() {
                [AppEvent::MasterVolumeChanged(value)] => assert!(
                    (value - (0.6 + VOLUME_STEP)).abs() < 1e-4,
                    "stepped to {value}"
                ),
                other => panic!("{other:?}"),
            }
        }

        #[test]
        fn the_shell_owns_apply_and_discard_and_each_dispatches_once() {
            let catalog = Catalog::new(ResolvedLocale::English);
            for (label, expected) in [
                (
                    catalog.text(TextKey::ApplyChanges),
                    AppEvent::ApplyStagedReceivers,
                ),
                (
                    catalog.text(TextKey::Discard),
                    AppEvent::DiscardStagedReceivers,
                ),
            ] {
                let mut state = base_state();
                state.staged_receivers.insert(rid(2));
                let harness = shell(ResolvedLocale::English, WIDE, state);
                assert_eq!(events(harness, label), vec![expected], "{label}");
            }
        }

        #[test]
        fn a_notice_offers_the_matching_corrective_action() {
            for (code, action, label_key, expected) in [
                (
                    NoticeCode::DiscoveryFailed,
                    CorrectiveAction::Refresh,
                    TextKey::Refresh,
                    AppEvent::RefreshRequested,
                ),
                (
                    NoticeCode::SessionFailed,
                    CorrectiveAction::Retry,
                    TextKey::Retry,
                    AppEvent::StartRequested,
                ),
                (
                    NoticeCode::PreferencesFailed,
                    CorrectiveAction::RetryPreferencesPersistence,
                    TextKey::PreferenceRetry,
                    AppEvent::RetryPreferencesPersistence,
                ),
                (
                    NoticeCode::HotkeyFailed,
                    CorrectiveAction::OpenSettings,
                    TextKey::Settings,
                    AppEvent::Navigate(Page::Settings),
                ),
            ] {
                let mut state = base_state();
                // A stopped session keeps the header command out of the way
                // of the notice's own action.
                state.notice = Some(UserNotice {
                    severity: Severity::Error,
                    code,
                    summary: "raw backend text".into(),
                    action: Some(action),
                });
                // Tall on purpose: the notice is the fifth section, and a
                // node below the fold cannot be clicked. Where the notice
                // sits at the two target sizes is the order test's job.
                let harness = shell(ResolvedLocale::English, [WIDE[0], 1400.0], state);
                let label = Catalog::new(ResolvedLocale::English).text(label_key);
                assert!(
                    events(harness, label).contains(&expected),
                    "{code:?} did not offer {expected:?}"
                );
            }
        }

        /// The legacy Home page carried a Refresh beside Start. Without one
        /// here, a user whose speaker list is stale but not empty would have
        /// no way at all to ask discovery to look again.
        #[test]
        fn the_receiver_section_offers_a_real_refresh_while_speakers_are_listed() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = shell(locale, WIDE, base_state());
                assert_eq!(
                    events(harness, catalog.text(TextKey::RefreshReceivers)),
                    vec![AppEvent::RefreshRequested],
                    "{locale:?}"
                );
            }
        }

        /// Exactly one: the empty state's Refresh and the section's Refresh
        /// are the same command and must never both be on the page.
        #[test]
        fn only_one_refresh_command_is_ever_on_the_page() {
            for state in [base_state(), {
                let mut empty = base_state();
                empty.receivers.clear();
                empty.desired_receivers.clear();
                empty.staged_receivers.clear();
                empty
            }] {
                let catalog = Catalog::new(ResolvedLocale::English);
                let harness = shell(ResolvedLocale::English, WIDE, state);
                let count = labels_in_order(&harness)
                    .iter()
                    .filter(|label| label.as_str() == catalog.text(TextKey::RefreshReceivers))
                    .count();
                assert_eq!(count, 1);
            }
        }

        #[test]
        fn an_empty_discovery_shows_the_honest_empty_state_with_a_real_refresh() {
            let mut state = base_state();
            state.receivers.clear();
            state.desired_receivers.clear();
            state.staged_receivers.clear();

            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = shell(locale, WIDE, state.clone());
                let labels = labels_in_order(&harness);
                assert!(
                    labels
                        .iter()
                        .any(|label| label.contains(catalog.text(TextKey::NoReceiversTitle))),
                    "{locale:?}: {labels:?}"
                );
                assert_eq!(
                    events(harness, catalog.text(TextKey::RefreshReceivers)),
                    vec![AppEvent::RefreshRequested],
                    "{locale:?}"
                );
            }
        }
    }

    mod honesty {
        use super::*;

        /// The raw backend summary is pre-redacted, not presentation-safe. It
        /// may reach a log; it may never reach the screen, the accessibility
        /// tree, or a support export.
        #[test]
        fn no_backend_summary_and_no_device_address_reach_the_page() {
            let mut state = base_state();
            state.advanced_information = true;
            state.stream = StreamState::Failed {
                generation: GenerationId(9),
                summary: "Authentication failed at 192.168.1.44:7000".into(),
            };
            state.notice = Some(UserNotice {
                severity: Severity::Error,
                code: NoticeCode::SessionFailed,
                summary: "Authentication failed at 192.168.1.44:7000".into(),
                action: Some(CorrectiveAction::Retry),
            });

            for locale in LOCALES {
                let harness = shell(locale, WIDE, state.clone());
                for label in labels_in_order(&harness) {
                    assert!(!label.contains("192.168"), "{locale:?}: {label}");
                    assert!(
                        !label.contains("Authentication failed"),
                        "{locale:?}: {label}"
                    );
                    assert!(
                        !label.to_lowercase().contains("a0b1c2"),
                        "{locale:?} leaked the device address: {label}"
                    );
                }
            }
        }

        #[test]
        fn standard_mode_shows_no_metric_strip_at_all() {
            let mut state = base_state();
            state.advanced_information = true;
            let advanced = model(&state, ResolvedLocale::English);
            let phrase = advanced.advanced_metrics[0].accessible_phrase();

            state.advanced_information = false;
            let harness = shell(ResolvedLocale::English, WIDE, state);
            assert!(
                !labels_in_order(&harness)
                    .iter()
                    .any(|label| label.contains(&phrase)),
                "Standard mode showed an advanced measurement"
            );
        }
    }
}
