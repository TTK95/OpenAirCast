//! The Receiver Card.
//!
//! One card is one checkbox. The whole surface -- not a small box beside the
//! name -- is the target, because on the Command Home selecting a speaker is
//! the primary gesture and a 16-point box inside a 56-point card would be the
//! only part of it that worked.
//!
//! Three properties are load-bearing:
//!
//! * **The checked state is the staged selection.** `ToggleStagedReceiver`
//!   writes `staged_receivers` and nothing else, so a box bound to the
//!   applied set would spring back on the very next frame and contradict both
//!   the Change Bar and the Speakers inventory.
//! * **The egui identity is salted with the device id.** Discovery reorders
//!   the list whenever a name changes; an index-based identity would make a
//!   click land on the neighbouring receiver.
//! * **Selection, streaming, availability, focus, and hover are five separate
//!   visual states**, and each of the first three also carries a word and a
//!   symbol, so none of them depends on colour.

use egui::{Align2, Sense, StrokeKind, WidgetInfo, WidgetType};

use super::CONTROL_GAP;
use crate::app::Availability;
use crate::ui::presentation::{
    focus_ring, ReceiverCardModel, ReceiverVisualState, CARD_PADDING, CARD_RADIUS,
    RECEIVER_CARD_MIN_HEIGHT,
};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Edge length of the painted checkbox mark.
const MARK_SIZE: f32 = 20.0;
/// Gap between the mark column and the text column.
const MARK_GAP: f32 = 12.0;
/// Gap between the two text rows.
const ROW_GAP: f32 = 2.0;

/// Which carriers a card has for the facts it states.
///
/// The Overview card has three the inventory row does not: a check mark that
/// *is* the selection, a hover surface, and a click. The Speakers page has
/// none of them, so a state the mark would have carried has to be painted
/// there instead. One derivation, two carrier sets -- rather than a second
/// card component whose copy could drift away from this one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardCarriers {
    /// The Command Home card: the check mark states the selection.
    WithControl,
    /// The Speakers inventory row: nothing but text states anything.
    TextOnly,
}

/// Renders one card and dispatches at most one toggle per activation.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &ReceiverCardModel,
    emit: &mut dyn FnMut(crate::app::AppEvent),
) -> egui::Response {
    show_card(ui, tokens, model, emit)
}

/// Renders one card as a read-only inventory row.
///
/// The same surface, the same radius, the same padding, and the same rows as
/// the Command Home card -- minus the check-mark column, because there is no
/// control here to put in it, and minus every interaction, because the
/// Speakers page selects nothing. It announces one node carrying the card's
/// whole sentence, exactly as the interactive card does, so the two pages
/// cannot describe the same receiver differently.
pub fn show_readonly(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &ReceiverCardModel,
) -> egui::Response {
    let width = ui.available_width().max(1.0);
    let text_width = (width - 2.0 * CARD_PADDING).max(1.0);
    let galleys = layout_rows(ui, tokens, model, CardCarriers::TextOnly, text_width);
    let height = card_height(rows_height(&galleys));

    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let announced = model.accessible_name.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, announced.clone()));

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let radius = egui::CornerRadius::same(CARD_RADIUS as u8);
        painter.rect_filled(rect, radius, tokens.surface);
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, border_color(tokens, model)),
            StrokeKind::Inside,
        );
        paint_rows(
            &painter,
            galleys,
            egui::pos2(rect.left() + CARD_PADDING, rect.top() + CARD_PADDING),
        );
    }

    ui.add_space(CONTROL_GAP);
    response
}

/// One card's rows, laid out against the width they were given.
fn layout_rows(
    ui: &egui::Ui,
    tokens: &ThemeTokens,
    model: &ReceiverCardModel,
    carriers: CardCarriers,
    text_width: f32,
) -> Vec<(std::sync::Arc<egui::Galley>, egui::Color32)> {
    let painter = ui.painter().clone();
    card_rows(model, carriers)
        .iter()
        .map(|row| {
            let color = row_color(tokens, row.emphasis);
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = text_width;
            row.role.rich_text(&row.text).color(color).append_to(
                &mut job,
                ui.style(),
                egui::FontSelection::Default,
                egui::Align::Min,
            );
            (painter.layout_job(job), color)
        })
        .collect()
}

fn rows_height(galleys: &[(std::sync::Arc<egui::Galley>, egui::Color32)]) -> f32 {
    galleys
        .iter()
        .map(|(galley, _)| galley.size().y)
        .sum::<f32>()
        + ROW_GAP * galleys.len().saturating_sub(1) as f32
}

fn paint_rows(
    painter: &egui::Painter,
    galleys: Vec<(std::sync::Arc<egui::Galley>, egui::Color32)>,
    top_left: egui::Pos2,
) {
    let mut cursor = top_left.y;
    for (galley, color) in galleys {
        let height = galley.size().y;
        painter.galley(egui::pos2(top_left.x, cursor), galley, color);
        cursor += height + ROW_GAP;
    }
}

/// The egui identity of one card.
///
/// Derived from the MAC-derived device id rather than from an allocation
/// counter, so a discovery reorder moves the card without moving its focus,
/// its hover state, or a click that is already in flight.
fn card_id(ui: &egui::Ui, model: &ReceiverCardModel) -> egui::Id {
    ui.id().with(("overview_receiver_card", model.id.0))
}

fn show_card(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &ReceiverCardModel,
    emit: &mut dyn FnMut(crate::app::AppEvent),
) -> egui::Response {
    let width = ui.available_width().max(MARK_SIZE + MARK_GAP + 1.0);
    let text_width = (width - 2.0 * CARD_PADDING - MARK_SIZE - MARK_GAP).max(1.0);

    let galleys = layout_rows(ui, tokens, model, CardCarriers::WithControl, text_width);
    let height = card_height(rows_height(&galleys));
    let id = card_id(ui, model);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());

    let announced = model.accessible_name.clone();
    let selected = model.selected;
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Checkbox, true, selected, announced.clone())
    });

    let activated = response.clicked()
        || (response.has_focus()
            && ui.input(|input| {
                input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
            }));
    if activated {
        emit(crate::app::AppEvent::ToggleStagedReceiver(model.id.clone()));
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let radius = egui::CornerRadius::same(CARD_RADIUS as u8);
        // Selection, hover, and streaming are three different surfaces, and
        // each of them also has its word in the state row.
        let fill = if model.selected {
            tokens.route_soft
        } else if response.hovered() {
            tokens.surface_subtle
        } else {
            tokens.surface
        };
        painter.rect_filled(rect, radius, fill);
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, border_color(tokens, model)),
            StrokeKind::Inside,
        );

        let mark_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + CARD_PADDING, rect.top() + CARD_PADDING),
            egui::vec2(MARK_SIZE, MARK_SIZE),
        );
        painter.rect_stroke(
            mark_rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.5, tokens.ink_muted),
            StrokeKind::Inside,
        );
        if model.selected {
            painter.rect_filled(
                mark_rect.shrink(3.0),
                egui::CornerRadius::same(2),
                tokens.route,
            );
            painter.text(
                mark_rect.center(),
                Align2::CENTER_CENTER,
                ReceiverVisualState::Selected.symbol(),
                TypographyRole::Secondary.font_id(),
                tokens.on_route,
            );
        }

        paint_rows(
            &painter,
            galleys,
            egui::pos2(mark_rect.right() + MARK_GAP, rect.top() + CARD_PADDING),
        );

        // egui routes a focused widget to its "active" visuals, which are not
        // a focus indicator. The ring is painted here so keyboard focus is
        // visible on a custom surface at all.
        if response.has_focus() {
            let ring = focus_ring(tokens);
            // The outline extends beyond the card; retain the ancestor's
            // scroll clip without clipping it to the card surface itself.
            ui.painter().rect_stroke(
                rect.expand(ring.offset),
                egui::CornerRadius::same((CARD_RADIUS + ring.offset) as u8),
                egui::Stroke::new(ring.width, ring.color),
                StrokeKind::Outside,
            );
        }
    }

    ui.add_space(CONTROL_GAP);
    response
}

/// How prominently one card row is inked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RowEmphasis {
    /// The receiver's own name, and any state it is actively in.
    Strong,
    /// Everything the card says *about* it.
    Muted,
}

/// One painted text row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardRow {
    pub text: String,
    pub role: TypographyRole,
    pub emphasis: RowEmphasis,
}

/// The rows one card paints, top to bottom.
///
/// Deriving them here rather than inside the paint pass is what makes the
/// card's copy inspectable without a window -- and what stopped it from
/// printing the availability word twice, once in the detail row and once
/// again as the resting "state" below it.
pub(crate) fn card_rows(model: &ReceiverCardModel, carriers: CardCarriers) -> Vec<CardRow> {
    let mut rows = vec![
        CardRow {
            text: model.name.clone(),
            role: TypographyRole::Body,
            emphasis: RowEmphasis::Strong,
        },
        // Device class and reachability on one line: two facts about the
        // hardware, neither of which the selection can change.
        CardRow {
            text: format!(
                "{} {} {} {}",
                model.device_class, SEPARATOR, model.availability_symbol, model.availability_label
            ),
            role: TypographyRole::Secondary,
            emphasis: RowEmphasis::Muted,
        },
    ];
    // With a control on the card the check mark and the border already say
    // "Selected"; without one, nothing does.
    let shows_state = match carriers {
        CardCarriers::WithControl => model.state.shows_state_row(),
        CardCarriers::TextOnly => model.state.adds_to_availability(),
    };
    if shows_state {
        rows.push(CardRow {
            text: format!("{} {}", model.state_symbol, model.state_label),
            role: TypographyRole::Secondary,
            emphasis: RowEmphasis::Strong,
        });
    }
    if let Some(details) = &model.advanced_details {
        rows.push(CardRow {
            text: details.clone(),
            role: TypographyRole::Secondary,
            emphasis: RowEmphasis::Muted,
        });
    }
    rows
}

/// The one place a row emphasis becomes a colour.
fn row_color(tokens: &ThemeTokens, emphasis: RowEmphasis) -> egui::Color32 {
    match emphasis {
        RowEmphasis::Strong => tokens.ink,
        RowEmphasis::Muted => tokens.ink_muted,
    }
}

/// The dot that separates the device class from the availability word.
const SEPARATOR: &str = "\u{2022}";

/// The card's height: its text plus padding, never less than the specified
/// minimum target size.
fn card_height(text_height: f32) -> f32 {
    (text_height + 2.0 * CARD_PADDING).max(RECEIVER_CARD_MIN_HEIGHT)
}

/// The border reports availability first, and only then the selection.
///
/// A receiver discovery can no longer reach is not on a route however the
/// checkbox stands, so selecting it must not repaint its edge in the Route
/// colour. Availability therefore outranks both Selected and Streaming here;
/// the colour-free carriers of the same two facts -- the availability pair in
/// the detail row and the state word below it -- stay side by side, so
/// nothing is hidden by this precedence, only prioritized.
fn border_color(tokens: &ThemeTokens, model: &ReceiverCardModel) -> egui::Color32 {
    if model.availability == Availability::Unavailable {
        return tokens.disabled;
    }
    match model.state {
        ReceiverVisualState::Streaming | ReceiverVisualState::SelectedAndStreaming => tokens.live,
        ReceiverVisualState::Selected => tokens.route,
        ReceiverVisualState::Available | ReceiverVisualState::Unavailable => tokens.border,
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{accessible_strings, catalog, tokens, LOCALES};
    use super::*;
    use crate::app::{
        AppEvent, AppState, Availability, GenerationId, ReceiverState, ResolvedLocale, StreamState,
        ThemePreference, UiSnapshot,
    };
    use crate::ui::presentation::{OverviewModel, ReceiverCardModel};
    use crate::ui::theme::ThemeTokens;
    use airplay_core::DeviceId;
    use std::collections::HashSet;

    /// The specification's minimum card height, written out rather than read
    /// from the constant the renderer uses.
    const SPECIFIED_MIN_HEIGHT: f32 = 56.0;

    fn rid(last: u8) -> DeviceId {
        DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last])
    }

    fn receiver(last: u8, name: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "AudioAccessory5,1".into(),
            availability,
        }
    }

    fn base_state() -> AppState {
        AppState {
            receivers: vec![
                receiver(1, "Kitchen", Availability::Available),
                receiver(2, "Office", Availability::Unavailable),
            ],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            ..AppState::default()
        }
    }

    fn cards(state: &AppState, locale: ResolvedLocale) -> Vec<ReceiverCardModel> {
        OverviewModel::from_snapshot(&UiSnapshot::from_state(state), catalog(locale)).receivers
    }

    struct Fixture {
        cards: Vec<ReceiverCardModel>,
        tokens: ThemeTokens,
        take_focus: bool,
        events: Vec<AppEvent>,
        heights: Vec<f32>,
        ids: Vec<(String, egui::Id)>,
    }

    type CardHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(
        cards: Vec<ReceiverCardModel>,
        tokens: ThemeTokens,
        take_focus: bool,
    ) -> CardHarness {
        let mut harness: CardHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                fixture.heights.clear();
                fixture.ids.clear();
                let mut events = Vec::new();
                for card in &fixture.cards {
                    let response = {
                        let mut emit = |event: AppEvent| events.push(event);
                        show(ui, &fixture.tokens, card, &mut emit)
                    };
                    fixture.heights.push(response.rect.height());
                    fixture.ids.push((card.name.clone(), response.id));
                    if fixture.take_focus && !response.has_focus() {
                        response.request_focus();
                        break;
                    }
                }
                fixture.events.extend(events);
            },
            Fixture {
                cards,
                tokens,
                take_focus,
                events: Vec::new(),
                heights: Vec::new(),
                ids: Vec::new(),
            },
        );
        harness.set_size(egui::vec2(520.0, 400.0));
        harness.step();
        harness
    }

    #[test]
    fn the_whole_card_is_one_checkbox_target_carrying_the_receivers_sentence() {
        for &locale in LOCALES {
            let models = cards(&base_state(), locale);
            let harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
            for card in &models {
                let node = harness.get_by_label(card.accessible_name.as_str());
                assert_eq!(
                    node.accesskit_node().role(),
                    accesskit::Role::CheckBox,
                    "{locale:?} {}",
                    card.name
                );
            }
        }
    }

    #[test]
    fn the_checked_state_is_the_staged_selection() {
        let models = cards(&base_state(), ResolvedLocale::English);
        let harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
        assert!(models[0].selected && !models[1].selected);
        assert_eq!(
            harness
                .get_by_label(models[0].accessible_name.as_str())
                .accesskit_node()
                .toggled(),
            Some(accesskit::Toggled::True)
        );
        assert_eq!(
            harness
                .get_by_label(models[1].accessible_name.as_str())
                .accesskit_node()
                .toggled(),
            Some(accesskit::Toggled::False)
        );
    }

    /// The floor has to hold even for content that would not reach it on
    /// its own: a card whose rows shrink must still keep its target size.
    #[test]
    fn the_card_height_never_falls_below_the_specified_minimum() {
        assert_eq!(card_height(0.0), SPECIFIED_MIN_HEIGHT);
        assert_eq!(card_height(4.0), SPECIFIED_MIN_HEIGHT);
        assert!(card_height(200.0) > SPECIFIED_MIN_HEIGHT);
    }

    #[test]
    fn every_card_is_at_least_fifty_six_points_high_in_every_theme() {
        for high_contrast in [false, true] {
            for theme in [ThemePreference::Light, ThemePreference::Dark] {
                let harness = harness(
                    cards(&base_state(), ResolvedLocale::German),
                    tokens(theme, high_contrast),
                    false,
                );
                for height in &harness.state().heights {
                    assert!(
                        *height >= SPECIFIED_MIN_HEIGHT,
                        "{theme:?} high_contrast={high_contrast} card was {height} points high"
                    );
                }
            }
        }
    }

    #[test]
    fn clicking_a_card_dispatches_exactly_one_toggle_for_its_own_receiver() {
        let models = cards(&base_state(), ResolvedLocale::English);
        let mut harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
        harness
            .get_by_label(models[1].accessible_name.as_str())
            .click();
        harness.step();
        assert_eq!(
            harness.state().events,
            vec![AppEvent::ToggleStagedReceiver(rid(2))]
        );
    }

    /// A card's egui identity has to belong to the receiver, not to its
    /// position: discovery reorders the list whenever a name changes, and an
    /// index-salted identity would hand one card's focus, hover, and pending
    /// click to whoever moved into its slot.
    #[test]
    fn a_cards_egui_identity_survives_a_discovery_reorder() {
        let ordered = cards(&base_state(), ResolvedLocale::English);
        let first_pass = harness(
            ordered.clone(),
            tokens(ThemePreference::Light, false),
            false,
        );
        let before = first_pass.state().ids.clone();

        let mut reordered = ordered;
        reordered.reverse();
        let second_pass = harness(reordered, tokens(ThemePreference::Light, false), false);
        let after = second_pass.state().ids.clone();

        assert_ne!(
            before[0].1, before[1].1,
            "two receivers must not share one identity"
        );
        for (name, id) in &before {
            let moved = after
                .iter()
                .find(|(other, _)| other == name)
                .expect("the receiver survives the reorder");
            assert_eq!(*id, moved.1, "{name} changed identity when the list moved");
        }
    }

    /// The egui identity is salted with the device id, so a discovery reorder
    /// cannot make a click land on the neighbouring card.
    #[test]
    fn the_same_receiver_is_toggled_after_a_discovery_reorder() {
        let mut state = base_state();
        state.receivers.swap(0, 1);
        state.receivers[0].name = "Office".into();

        let models = cards(&state, ResolvedLocale::English);
        let office = models
            .iter()
            .find(|card| card.name == "Office")
            .expect("Office survives the reorder")
            .clone();
        let mut harness = harness(models, tokens(ThemePreference::Light, false), false);
        harness
            .get_by_label(office.accessible_name.as_str())
            .click();
        harness.step();
        assert_eq!(
            harness.state().events,
            vec![AppEvent::ToggleStagedReceiver(rid(2))]
        );
    }

    #[test]
    fn a_focused_card_toggles_with_space_and_with_enter() {
        for key in [egui::Key::Space, egui::Key::Enter] {
            let mut harness = harness(
                cards(&base_state(), ResolvedLocale::English),
                tokens(ThemePreference::Light, false),
                true,
            );
            harness.step();
            assert!(
                harness.state().events.is_empty(),
                "focus alone must not act"
            );
            harness.key_press(key);
            harness.step();
            assert_eq!(
                harness.state().events,
                vec![AppEvent::ToggleStagedReceiver(rid(1))],
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_focused_receiver_card_paints_its_entire_focus_outline() {
        for high_contrast in [false, true] {
            let palette = tokens(ThemePreference::Light, high_contrast);
            let model = cards(&base_state(), ResolvedLocale::English).remove(0);
            let mut harness = egui_kittest::Harness::new_ui(move |ui| {
                egui::Frame::NONE.inner_margin(20).show(ui, |ui| {
                    show(ui, &palette, &model, &mut |_| {}).request_focus();
                });
            });
            harness.step();
            let outlines: Vec<_> = harness
                .output()
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.stroke.color == palette.focus
                            && rect.stroke.width == palette.focus_width
                            && rect.stroke_kind == StrokeKind::Outside =>
                    {
                        Some((shape.clip_rect, rect))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(outlines.len(), 1, "the focused card paints one outline");
            let (clip, outline) = outlines[0];
            assert!(
                clip.contains_rect(outline.rect.expand(outline.stroke.width)),
                "the complete outside focus stroke must survive clipping: {clip:?} {outline:?}"
            );
        }
    }

    #[test]
    fn receiver_rows_use_readable_line_heights_and_fit_inside_the_card() {
        let palette = tokens(ThemePreference::Light, false);
        let mut model = cards(&base_state(), ResolvedLocale::English).remove(0);
        model.name = "A long receiver name that wraps onto multiple lines".into();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, measurements: &mut Vec<(f32, usize, f32)>| {
                measurements.clear();
                ui.set_width(184.0);
                let galleys = layout_rows(ui, &palette, &model, CardCarriers::WithControl, 120.0);
                let total = rows_height(&galleys);
                for (index, (galley, _)) in galleys.iter().enumerate() {
                    measurements.push((
                        galley.size().y,
                        galley.rows.len(),
                        if index == 0 { 20.0 } else { 17.0 },
                    ));
                }
                let response = show(ui, &palette, &model, &mut |_| {});
                assert!(
                    response.rect.height() >= total + 32.0,
                    "the laid-out rows must fit between the card's top and bottom padding"
                );
            },
            Vec::new(),
        );
        harness.step();
        assert!(
            harness.state()[0].1 > 1,
            "the fixture must exercise wrapping"
        );
        for &(height, lines, line_height) in harness.state() {
            assert!(
                (height - lines as f32 * line_height).abs() < 0.1,
                "{lines} lines need {line_height} points each, got {height}"
            );
        }
    }

    #[test]
    fn an_unavailable_receiver_keeps_its_toggle_and_says_so_in_both_locales() {
        for &locale in LOCALES {
            let models = cards(&base_state(), locale);
            let harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
            let announced = accessible_strings(harness.root()).join(" | ");
            assert!(
                announced.contains(&models[1].availability_label),
                "{locale:?} hid the availability word: {announced}"
            );
            assert!(
                !harness
                    .get_by_label(models[1].accessible_name.as_str())
                    .accesskit_node()
                    .is_disabled(),
                "an unavailable receiver stays selectable, or it can never be re-selected"
            );
        }
    }

    /// A checked speaker that goes offline is the case in which the state
    /// word alone is reassuring and wrong.
    ///
    /// The assertion is against the node that assistive technology actually
    /// reads -- the card's single `WidgetInfo` name in the rendered tree --
    /// not against the model's `availability_label` field, because that field
    /// existed all along and was painted into a galley that creates no
    /// accessibility node at all.
    #[test]
    fn a_selected_receiver_that_went_offline_announces_it_in_the_rendered_tree() {
        for &locale in LOCALES {
            let mut state = base_state();
            state.staged_receivers = HashSet::from_iter([rid(1), rid(2)]);
            let models = cards(&state, locale);
            let office = &models[1];
            assert!(office.selected, "{locale:?}: the fixture stages Office");
            assert_eq!(office.availability, Availability::Unavailable);

            let harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
            let node = harness.get_by_label(office.accessible_name.as_str());
            let announced = node.accesskit_node().label().unwrap_or_default();
            assert!(
                announced.contains(&office.availability_label),
                "{locale:?}: the offline fact never reached the tree: {announced}"
            );
            assert!(
                announced.contains(&office.state_label),
                "{locale:?}: the selection has to survive too: {announced}"
            );
            assert_eq!(
                node.accesskit_node().toggled(),
                Some(accesskit::Toggled::True),
                "{locale:?}: it is still checked"
            );
        }
    }

    /// Colour is not the carrier here, but it must not contradict the words:
    /// an unreachable speaker may not wear the Route edge just because the
    /// user checked it.
    #[test]
    fn the_border_of_a_selected_receiver_reports_availability_before_selection() {
        let tokens = tokens(ThemePreference::Light, false);
        let mut state = base_state();
        state.staged_receivers = HashSet::from_iter([rid(1), rid(2)]);
        let models = cards(&state, ResolvedLocale::English);

        assert_eq!(
            border_color(&tokens, &models[0]),
            tokens.route,
            "a reachable selected receiver is on the route"
        );
        assert_eq!(
            border_color(&tokens, &models[1]),
            tokens.disabled,
            "an unreachable receiver is not, however the checkbox stands"
        );
        assert_ne!(
            border_color(&tokens, &models[0]),
            border_color(&tokens, &models[1])
        );
    }

    /// One card for every state the card can be in, with a name and a class
    /// that share no words with the catalog copy under test.
    fn one_card_per_state(locale: ResolvedLocale) -> Vec<(ReceiverVisualState, ReceiverCardModel)> {
        let mut out = Vec::new();
        for &state in ReceiverVisualState::ALL {
            let (selected, streaming, availability) = match state {
                ReceiverVisualState::Available => (false, false, Availability::Available),
                ReceiverVisualState::Unavailable => (false, false, Availability::Unavailable),
                ReceiverVisualState::Selected => (true, false, Availability::Available),
                ReceiverVisualState::Streaming => (false, true, Availability::Available),
                ReceiverVisualState::SelectedAndStreaming => (true, true, Availability::Available),
            };
            let mut app = AppState {
                receivers: vec![receiver(1, "Kitchen", availability)],
                ..AppState::default()
            };
            if selected {
                app.staged_receivers = HashSet::from_iter([rid(1)]);
            }
            if streaming {
                app.active_receivers = HashSet::from_iter([rid(1)]);
                app.stream = StreamState::Streaming {
                    generation: GenerationId(1),
                };
            }
            let card = cards(&app, locale).remove(0);
            assert_eq!(card.state, state, "the fixture built the wrong state");
            out.push((state, card));
        }
        out
    }

    /// The reachability word belongs on the card exactly once.
    ///
    /// It used to be printed twice for a resting receiver: once in the detail
    /// row beside the device class, and once again as the "state" below it,
    /// because `Available` and `Unavailable` resolve to the very same catalog
    /// string that the availability label does.
    #[test]
    fn the_availability_word_is_printed_exactly_once_whatever_the_state() {
        for &locale in LOCALES {
            for (state, card) in one_card_per_state(locale) {
                let printed: Vec<String> = card_rows(&card, CardCarriers::WithControl)
                    .into_iter()
                    .map(|row| row.text)
                    .collect();
                let occurrences = printed
                    .iter()
                    .filter(|text| text.contains(&card.availability_label))
                    .count();
                assert_eq!(
                    occurrences, 1,
                    "{locale:?} {state:?}: {:?} appears {occurrences} times in {printed:?}",
                    card.availability_label
                );
            }
        }
    }

    /// One state, one signal.
    ///
    /// A selected card carried three: a route-coloured border, a filled check
    /// mark, and the sentence "Selected for streaming" underneath. The mark
    /// is the control, so it stays; the sentence goes -- but only from the
    /// paint. Losing it from the announced name is exactly the regression
    /// this assertion's second half exists to catch.
    #[test]
    fn a_selection_the_check_mark_already_states_is_not_repeated_as_a_sentence() {
        for &locale in LOCALES {
            for (state, card) in one_card_per_state(locale) {
                let printed: Vec<String> = card_rows(&card, CardCarriers::WithControl)
                    .into_iter()
                    .map(|row| row.text)
                    .collect();
                let painted = printed.iter().any(|text| text.contains(&card.state_label));

                match state {
                    ReceiverVisualState::Streaming | ReceiverVisualState::SelectedAndStreaming => {
                        assert!(
                            painted,
                            "{locale:?} {state:?}: live audio is not on any control, so it \
                         has to be in the copy: {printed:?}"
                        )
                    }
                    ReceiverVisualState::Selected => assert!(
                        !painted,
                        "{locale:?}: the check mark and the border already say this: {printed:?}"
                    ),
                    ReceiverVisualState::Available | ReceiverVisualState::Unavailable => {}
                }

                assert!(
                    card.accessible_name.contains(&card.state_label)
                        || !state.adds_to_availability(),
                    "{locale:?} {state:?}: the state left the announced sentence: {:?}",
                    card.accessible_name
                );
            }
        }
    }

    /// A card whose whole story is "reachable" must not tell it twice, and a
    /// selection the mark and the border already state must not be repeated
    /// as a sentence.
    ///
    /// The window showed `AudioAccessory5,1 - Lautsprecher verfuegbar` and,
    /// on the very next line, `Lautsprecher verfuegbar` again; a selected
    /// card carried a blue border, a filled check mark, and the sentence
    /// "Fuer das Streaming ausgewaehlt" for one and the same fact.
    /// `ReceiverVisualState::adds_to_availability` existed for exactly this
    /// and the renderer never consulted it.
    ///
    /// Measured as height rather than as text, because the rows are painted
    /// glyphs: a card that says nothing the availability word has not said
    /// has to be one row shorter than a card that reports live audio.
    #[test]
    fn a_resting_card_is_shorter_than_one_that_reports_something_new() {
        let mut state = base_state();
        state.receivers[1].availability = Availability::Available;
        state.staged_receivers = HashSet::from_iter([rid(2)]);
        state.desired_receivers = HashSet::from_iter([rid(2)]);
        state.active_receivers = HashSet::from_iter([rid(2)]);
        state.stream = StreamState::Streaming {
            generation: GenerationId(1),
        };

        for &locale in LOCALES {
            let models = cards(&state, locale);
            assert_eq!(models[0].state, ReceiverVisualState::Available);
            assert_eq!(models[1].state, ReceiverVisualState::SelectedAndStreaming);

            let harness = harness(models, tokens(ThemePreference::Light, false), false);
            let heights = harness.state().heights.clone();
            assert!(
                heights[0] < heights[1],
                "{locale:?}: the resting card is {} tall and the streaming card {}, \
                 so the resting card is still repeating a row it has nothing to put in",
                heights[0],
                heights[1]
            );
        }
    }

    #[test]
    fn a_card_paints_its_device_class_and_its_state_word() {
        let mut state = base_state();
        state.stream = StreamState::Streaming {
            generation: GenerationId(1),
        };
        state.active_receivers = HashSet::from_iter([rid(1)]);
        let models = cards(&state, ResolvedLocale::English);
        let harness = harness(models.clone(), tokens(ThemePreference::Light, false), false);
        let announced = accessible_strings(harness.root()).join(" | ");
        assert!(
            announced.contains(
                catalog(ResolvedLocale::English)
                    .text(crate::ui::i18n::TextKey::DeviceClassHomePodMini)
            ),
            "{announced}"
        );
        assert!(announced.contains(&models[0].state_label), "{announced}");
    }

    #[test]
    fn advanced_details_appear_below_the_standard_row_only_in_advanced_mode() {
        let mut state = base_state();
        state.advanced_information = true;
        let models = cards(&state, ResolvedLocale::German);
        let detail = models[0]
            .advanced_details
            .clone()
            .expect("advanced mode adds a detail");
        let advanced_harness = harness(models, tokens(ThemePreference::Light, false), false);
        assert!(accessible_strings(advanced_harness.root())
            .join(" | ")
            .contains(&detail));

        let standard = cards(&base_state(), ResolvedLocale::German);
        let standard_harness = harness(standard, tokens(ThemePreference::Light, false), false);
        assert!(!accessible_strings(standard_harness.root())
            .join(" | ")
            .contains(&detail));
    }

    /// The device address is an identity, not copy. It may reach the reducer
    /// as a typed event and never the screen.
    #[test]
    fn no_painted_or_announced_string_carries_the_device_address() {
        let harness = harness(
            cards(&base_state(), ResolvedLocale::English),
            tokens(ThemePreference::Light, false),
            false,
        );
        for text in accessible_strings(harness.root()) {
            assert!(!text.to_lowercase().contains("a0b1c2"), "{text}");
        }
    }
}
