//! The Route Ribbon.
//!
//! It paints where the audio goes: a rounded square for the Windows capture
//! source, one segment whose treatment follows the authoritative session
//! phase, and one circular node per receiver on the applied route.
//!
//! Two rules shape everything here:
//!
//! * **The ribbon is a picture of state, never a control.** It allocates no
//!   clickable widget and dispatches no event. Selection lives on the
//!   Receiver Cards, where the checkbox semantics are.
//! * **The picture is never the only carrier.** The phase word, every node
//!   name, and every node state word reach the accessibility tree as text,
//!   and the complete route sentence -- including the receivers a collapse
//!   hides -- is announced on the headline. High Contrast therefore loses
//!   nothing but colour.
//!
//! The single motion in the component is one bounded sweep when the route
//! becomes live. It runs for the theme's own transition time and stops; there
//! is no repeating timer and no animation that outlives the transition.

use egui::{Sense, StrokeKind};

use super::{show_labeled_text, show_text};
use crate::ui::presentation::{
    RouteNodeState, RoutePhase, RouteRibbonModel, CARD_PADDING, CARD_RADIUS, ROUTE_SWEEP_SECONDS,
};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Height of the painted route strip inside the compact route card.
///
/// The card's text carries source, selection, and active status. This strip
/// therefore only needs to make the route legible by shape, which keeps the
/// normal card within the approved 88--112 logical point range.
const RIBBON_HEIGHT: f32 = 28.0;
/// Edge length of the source node.
const SOURCE_SIZE: f32 = 24.0;
/// Radius of one receiver node.
const NODE_RADIUS: f32 = 10.0;
/// Thickness of the route segment.
const SEGMENT_WIDTH: f32 = 2.0;

/// How far the success sweep has run, from 0 to 1.
///
/// The duration is the theme's own transition time, capped at the specified
/// 120 ms. When Windows reports that animations are off the theme publishes
/// zero seconds, and the ribbon then reaches its final state on the first
/// frame instead of animating quickly.
pub fn sweep_progress(ui: &egui::Ui, model: &RouteRibbonModel) -> f32 {
    let live = model.phase == RoutePhase::Live;
    let seconds = sweep_seconds(ui.style().animation_time);
    if seconds <= 0.0 {
        return if live { 1.0 } else { 0.0 };
    }
    ui.ctx()
        .animate_bool_with_time(ui.id().with("route_sweep"), live, seconds)
}

/// How long the sweep may run, given the theme's transition time.
///
/// Zero when Windows reports that animations are off, and capped at the
/// specified 120 ms otherwise, so a longer theme transition can never turn
/// the acknowledgement into a lingering animation.
pub fn sweep_seconds(animation_time: f32) -> f32 {
    animation_time.clamp(0.0, ROUTE_SWEEP_SECONDS)
}

/// Gap kept clear between two label boxes, and between the source label and
/// the first node's label.
const LABEL_GAP: f32 = 8.0;
/// The widest slot one node and its label may claim.
///
/// Without a cap the nodes were spread from the source to the ribbon's right
/// edge whatever their number, so two receivers read as two unrelated things
/// at opposite ends of an empty strip instead of as a short route.
const MAX_NODE_SLOT: f32 = 132.0;
/// The most of the ribbon the source label may claim before it is
/// ellipsized. Enough for the catalog's own wording, never enough to crowd
/// the route out of its own picture.
const SOURCE_LABEL_SHARE: f32 = 0.28;

/// Where one node and the label under it sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeSlot {
    /// Centre of the node circle.
    pub center_x: f32,
    /// The widest the label under it may be before it is ellipsized.
    pub label_width: f32,
}

impl NodeSlot {
    /// The horizontal span the label occupies, as `(left, right)`.
    pub fn label_box(self) -> (f32, f32) {
        (
            self.center_x - self.label_width / 2.0,
            self.center_x + self.label_width / 2.0,
        )
    }
}

/// The horizontal layout of one ribbon, resolved without a window.
#[derive(Clone, Debug, PartialEq)]
pub struct RibbonSlots {
    /// Centre of the source square.
    pub source_center_x: f32,
    /// Left edge of the source label. The label is anchored, not centred:
    /// centring "Windows-Audio" on a point fourteen points from the ribbon's
    /// edge is what clipped it to "ows-Aud" and pushed what was left of it
    /// under the first node's name.
    pub source_label_left: f32,
    /// The widest the source label may be before it is ellipsized.
    pub source_label_width: f32,
    /// One slot per painted node, including the collapsed overflow node.
    pub nodes: Vec<NodeSlot>,
    /// Where the route trunk stops.
    pub trunk_end_x: f32,
}

/// Lays out the ribbon between `left` and `right`.
///
/// `source_label_width` is what the caller measured for the source word, so
/// the track can start after it instead of underneath it.
///
/// Two properties hold for every input, and the tests state them rather than
/// restating the arithmetic: every label box lies inside `[left, right]`, and
/// no two label boxes overlap.
pub fn ribbon_slots(left: f32, right: f32, source_label_width: f32, painted: usize) -> RibbonSlots {
    let width = (right - left).max(0.0);
    let source_center_x = left + SOURCE_SIZE / 2.0;
    let source_label_width = source_label_width.clamp(0.0, (width * SOURCE_LABEL_SHARE).max(0.0));

    // The node track starts clear of both the source square and its label.
    let track_start = (left + source_label_width + LABEL_GAP)
        .max(source_center_x + SOURCE_SIZE / 2.0 + LABEL_GAP)
        .min(right);
    let track = (right - track_start).max(0.0);

    let mut nodes = Vec::with_capacity(painted);
    if painted > 0 {
        // `min`, never `clamp`: a slot forced up to a floor would put the
        // last node past the ribbon's right edge in a narrow window, which is
        // the failure this whole function replaces.
        let slot = (track / painted as f32).min(MAX_NODE_SLOT);
        for index in 0..painted {
            nodes.push(NodeSlot {
                center_x: track_start + slot * (index as f32 + 0.5),
                label_width: (slot - LABEL_GAP).max(0.0),
            });
        }
    }

    let trunk_end_x = nodes
        .last()
        .map(|slot| slot.center_x)
        .unwrap_or(source_center_x);

    RibbonSlots {
        source_center_x,
        source_label_left: left,
        source_label_width,
        nodes,
        trunk_end_x,
    }
}

/// Renders the ribbon. It never dispatches.
pub fn show(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &RouteRibbonModel) -> egui::Response {
    let sweep = sweep_progress(ui, model);
    egui::Frame::NONE
        .fill(tokens.surface)
        .corner_radius(egui::CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |card| {
            // The headline carries source and phase in text; the count line
            // separately states applied selection and actual audio. The
            // announced name remains the complete route sentence, so the
            // compact diagram cannot remove a receiver from accessibility.
            show_labeled_text(
                card,
                TypographyRole::SectionTitle,
                tokens.ink,
                &format!(
                    "{} {} \u{00b7} {}{}",
                    model.phase.symbol(),
                    model.phase_label,
                    model.source_label,
                    destination_caption(model),
                ),
                &model.summary,
            );
            show_text(
                card,
                TypographyRole::Secondary,
                tokens.ink_muted,
                &model.counts_label,
            );
            paint_route(card, tokens, model, sweep);
        })
        .response
}

/// Keeps a real destination name in the compact visual line. The circle is a
/// useful status shape, but it must not be the sole visible identity of where
/// captured Windows audio will go.
fn destination_caption(model: &RouteRibbonModel) -> String {
    let names = model
        .nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if names.is_empty() {
        String::new()
    } else if let Some(overflow) = &model.overflow {
        format!(" > {names}, {}", overflow.marker)
    } else {
        format!(" > {names}")
    }
}

fn paint_route(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &RouteRibbonModel, sweep: f32) {
    let width = ui.available_width().max(SOURCE_SIZE * 4.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, RIBBON_HEIGHT), Sense::hover());
    let baseline = rect.center().y;

    let painted = model.nodes.len() + usize::from(model.overflow.is_some());
    // Text belongs above the diagram in the compact layout. Keeping the
    // geometry independent of label widths avoids a narrow window making a
    // truthful route unreadable or taller.
    let slots = ribbon_slots(rect.left(), rect.right(), 0.0, painted);
    let source_center = egui::pos2(slots.source_center_x, baseline);
    let segment = segment_color(tokens, model.phase);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        // The trunk. While the sweep runs, the live stroke grows out of the
        // source instead of appearing at full length.
        let trunk_end = slots.trunk_end_x;
        let reach = source_center.x + (trunk_end - source_center.x) * sweep_reach(model, sweep);
        painter.line_segment(
            [source_center, egui::pos2(trunk_end, baseline)],
            egui::Stroke::new(SEGMENT_WIDTH, tokens.border),
        );
        painter.line_segment(
            [source_center, egui::pos2(reach, baseline)],
            egui::Stroke::new(SEGMENT_WIDTH, segment),
        );

        // The source: a rounded square, never a circle, so the capture
        // endpoint stays distinguishable from a receiver by shape alone.
        let source_rect =
            egui::Rect::from_center_size(source_center, egui::vec2(SOURCE_SIZE, SOURCE_SIZE));
        painter.rect_filled(
            source_rect,
            egui::CornerRadius::same(6),
            tokens.surface_subtle,
        );
        painter.rect_stroke(
            source_rect,
            egui::CornerRadius::same(6),
            egui::Stroke::new(SEGMENT_WIDTH, tokens.route),
            StrokeKind::Inside,
        );
    }

    let overflow_index = model.nodes.len();
    for (index, node) in model.nodes.iter().enumerate() {
        let slot = slots.nodes[index];
        let center = egui::pos2(slot.center_x, baseline);
        paint_node(ui, tokens, rect, center, node.state);
        let response = ui.interact(
            egui::Rect::from_center_size(center, egui::vec2(NODE_RADIUS * 2.0, RIBBON_HEIGHT)),
            ui.id().with(("route_node", index)),
            Sense::hover(),
        );
        let announced = node.accessible_name.clone();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, announced.clone())
        });
    }

    if let Some(overflow) = &model.overflow {
        let slot = slots.nodes[overflow_index];
        let center = egui::pos2(slot.center_x, baseline);
        paint_node(ui, tokens, rect, center, RouteNodeState::Selected);
        let response = ui.interact(
            egui::Rect::from_center_size(center, egui::vec2(NODE_RADIUS * 2.0, RIBBON_HEIGHT)),
            ui.id().with("route_overflow"),
            Sense::hover(),
        );
        let announced = overflow.accessible_name.clone();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, announced.clone())
        });
    }
}

/// The trunk only grows while the route is live; every other phase draws its
/// full length at once, because there is no success to acknowledge.
fn sweep_reach(model: &RouteRibbonModel, sweep: f32) -> f32 {
    if model.phase == RoutePhase::Live {
        sweep
    } else {
        1.0
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    clip: egui::Rect,
    center: egui::Pos2,
    state: RouteNodeState,
) {
    if !ui.is_rect_visible(clip) {
        return;
    }
    let painter = ui.painter_at(clip);
    let (stroke, fill) = node_colors(tokens, state);
    if let Some(fill) = fill {
        painter.circle_filled(center, NODE_RADIUS, fill);
    }
    painter.circle_stroke(
        center,
        NODE_RADIUS,
        egui::Stroke::new(SEGMENT_WIDTH, stroke),
    );
}

/// The segment's colour role. High Contrast resolves every one of these to a
/// Windows system colour, which is why the symbol and the words carry the
/// phase there.
fn segment_color(tokens: &ThemeTokens, phase: RoutePhase) -> egui::Color32 {
    match phase {
        RoutePhase::Neutral => tokens.border,
        RoutePhase::Pending => tokens.route,
        RoutePhase::Live => tokens.live,
        RoutePhase::Failed => tokens.fault,
    }
}

fn node_colors(
    tokens: &ThemeTokens,
    state: RouteNodeState,
) -> (egui::Color32, Option<egui::Color32>) {
    match state {
        RouteNodeState::Selected => (tokens.route, None),
        RouteNodeState::Active => (tokens.live, Some(tokens.live_soft)),
        RouteNodeState::Unavailable => (tokens.disabled, None),
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{accessible_strings, catalog, tokens, LOCALES};
    use super::*;
    use crate::app::{
        AppState, Availability, GenerationId, ReceiverState, ResolvedLocale, StreamState,
        ThemePreference, UiSnapshot,
    };
    use crate::ui::presentation::{OverviewModel, RouteRibbonModel, ROUTE_SWEEP_SECONDS};
    use crate::ui::theme::ThemeTokens;
    use airplay_core::DeviceId;
    use std::collections::HashSet;

    fn rid(last: u8) -> DeviceId {
        DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last])
    }

    fn receiver(last: u8, name: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "HomePod mini".into(),
            availability,
        }
    }

    fn state_with(count: u8, stream: StreamState) -> AppState {
        let receivers: Vec<ReceiverState> = (1..=count)
            .map(|index| {
                let name = format!("Room {index}");
                receiver(index, &name, Availability::Available)
            })
            .collect();
        let ids: HashSet<DeviceId> = (1..=count).map(rid).collect();
        AppState {
            receivers,
            desired_receivers: ids.clone(),
            staged_receivers: ids,
            stream,
            ..AppState::default()
        }
    }

    fn model(state: &AppState, locale: ResolvedLocale) -> RouteRibbonModel {
        OverviewModel::from_snapshot(&UiSnapshot::from_state(state), catalog(locale)).route
    }

    struct Fixture {
        model: RouteRibbonModel,
        tokens: ThemeTokens,
        animation_seconds: f32,
        sweep: f32,
        sweeps: Vec<f32>,
        route_rect: egui::Rect,
    }

    type RibbonHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(fixture: Fixture) -> RibbonHarness {
        let mut harness: RibbonHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                ui.style_mut().animation_time = fixture.animation_seconds;
                fixture.sweep = sweep_progress(ui, &fixture.model);
                fixture.sweeps.push(fixture.sweep);
                fixture.route_rect = show(ui, &fixture.tokens, &fixture.model).rect;
            },
            fixture,
        );
        harness.set_size(egui::vec2(720.0, 240.0));
        harness.step();
        harness
    }

    fn still_ribbon(model: RouteRibbonModel, tokens: ThemeTokens) -> RibbonHarness {
        harness(Fixture {
            model,
            tokens,
            animation_seconds: 0.0,
            sweep: 0.0,
            sweeps: Vec::new(),
            route_rect: egui::Rect::NOTHING,
        })
    }

    #[test]
    fn normal_route_card_uses_the_approved_compact_height_and_names_a_destination() {
        let model = model(
            &state_with(2, StreamState::Stopped),
            ResolvedLocale::English,
        );
        let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, false));
        let height = harness.state().route_rect.height();

        assert!(
            (88.0..=112.0).contains(&height),
            "normal route card measured {height} points, outside the approved compact range"
        );
        let visible = accessible_strings(harness.root()).join(" ");
        assert!(
            visible.contains(&model.nodes[0].name),
            "the compact visual line omitted the first destination: {visible}"
        );
    }

    mod slots {
        use super::*;

        /// The ribbon widths worth checking: the two acceptance window
        /// sizes' content measures, a narrow one, and an absurd one.
        const WIDTHS: [f32; 5] = [112.0, 320.0, 640.0, 872.0, 2000.0];
        /// A source word wide enough to have caused the original clipping.
        const SOURCE_WORD: f32 = 86.0;

        /// Nothing the layout places may leave the ribbon it was given, for
        /// any width and any number of painted nodes.
        #[test]
        fn every_label_box_stays_inside_the_ribbon() {
            for width in WIDTHS {
                for painted in 0..=4 {
                    let left = 24.0;
                    let right = left + width;
                    let slots = ribbon_slots(left, right, SOURCE_WORD, painted);

                    assert!(
                        slots.source_label_left >= left,
                        "{width}/{painted}: the source label starts at {} outside {left}",
                        slots.source_label_left
                    );
                    assert!(
                        slots.source_label_left + slots.source_label_width <= right + 0.01,
                        "{width}/{painted}: the source label ends past {right}"
                    );
                    assert_eq!(slots.nodes.len(), painted);
                    for (index, slot) in slots.nodes.iter().enumerate() {
                        let (box_left, box_right) = slot.label_box();
                        assert!(
                            box_left >= left - 0.01 && box_right <= right + 0.01,
                            "{width}/{painted}: node {index} spans {box_left}..{box_right} \
                             outside {left}..{right}"
                        );
                    }
                }
            }
        }

        /// Two labels may never share a pixel -- neither with each other nor
        /// with the source word. The window showed "ows-Aud" running into the
        /// first receiver's name.
        #[test]
        fn no_two_labels_overlap_and_none_of_them_reaches_the_source_word() {
            for width in WIDTHS {
                for painted in 1..=4 {
                    let slots = ribbon_slots(0.0, width, SOURCE_WORD, painted);
                    let source_right = slots.source_label_left + slots.source_label_width;
                    let (first_left, _) = slots.nodes[0].label_box();
                    assert!(
                        first_left >= source_right - 0.01,
                        "{width}/{painted}: the first node's label starts at {first_left}, \
                         under a source word that runs to {source_right}"
                    );
                    for pair in slots.nodes.windows(2) {
                        let (_, left_end) = pair[0].label_box();
                        let (right_start, _) = pair[1].label_box();
                        assert!(
                            right_start >= left_end - 0.01,
                            "{width}/{painted}: labels overlap between {left_end} and \
                             {right_start}"
                        );
                    }
                }
            }
        }

        /// The route's own size must not depend on the ribbon's.
        ///
        /// The spacing used to be `(right_edge - first_node) / (n - 1)`, so
        /// widening the window widened the route with it: the same two
        /// receivers sat 260 points apart in one window and 800 in another,
        /// with the last node pinned against the far edge either way.
        #[test]
        fn a_wider_ribbon_does_not_make_the_same_route_wider() {
            for painted in 1..=4 {
                let reference = ribbon_slots(0.0, 640.0, SOURCE_WORD, painted);
                let extent = |slots: &RibbonSlots| {
                    slots.nodes.last().unwrap().center_x - slots.source_center_x
                };
                for wider in [872.0, 1400.0, 2000.0] {
                    let grown = ribbon_slots(0.0, wider, SOURCE_WORD, painted);
                    assert!(
                        (extent(&grown) - extent(&reference)).abs() < 0.01,
                        "{painted} node(s): the route spans {} on a 640-point ribbon and \
                         {} on a {wider}-point one",
                        extent(&reference),
                        extent(&grown)
                    );
                }
            }
        }

        /// A short route stays in the part of the ribbon it needs, instead of
        /// leaving several hundred points of nothing in the middle.
        #[test]
        fn two_receivers_do_not_reach_the_far_edge_of_a_wide_ribbon() {
            let width = 872.0;
            let slots = ribbon_slots(0.0, width, SOURCE_WORD, 2);
            let last = slots.nodes[1].center_x;
            assert!(
                last < width / 2.0,
                "two nodes reached x={last} in a {width}-point ribbon"
            );
            let gap = slots.nodes[1].center_x - slots.nodes[0].center_x;
            assert!(
                gap >= NODE_RADIUS * 2.0,
                "adjacent nodes are {gap} apart, closer than their own diameter"
            );
        }

        /// A ribbon too narrow for the labels shrinks them rather than
        /// letting the route walk off the edge.
        #[test]
        fn a_narrow_ribbon_shrinks_the_slots_instead_of_overflowing() {
            let wide = ribbon_slots(0.0, 872.0, SOURCE_WORD, 4);
            let narrow = ribbon_slots(0.0, 320.0, SOURCE_WORD, 4);
            assert!(
                narrow.nodes[0].label_width < wide.nodes[0].label_width,
                "the narrow ribbon kept the wide ribbon's label width"
            );
            assert!(
                narrow.nodes.last().unwrap().label_box().1 <= 320.0 + 0.01,
                "the narrow ribbon let its last label leave the strip"
            );
        }

        /// The trunk ends at the route's last node, never in empty space.
        #[test]
        fn the_trunk_stops_at_the_last_node_and_at_the_source_when_there_is_none() {
            let empty = ribbon_slots(0.0, 640.0, SOURCE_WORD, 0);
            assert_eq!(empty.trunk_end_x, empty.source_center_x);
            let three = ribbon_slots(0.0, 640.0, SOURCE_WORD, 3);
            assert_eq!(three.trunk_end_x, three.nodes[2].center_x);
        }

        /// A source word longer than the ribbon can carry is capped, not
        /// allowed to push the whole route off the right edge.
        #[test]
        fn an_over_long_source_word_is_capped_rather_than_pushing_the_route_away() {
            let slots = ribbon_slots(0.0, 400.0, 10_000.0, 3);
            assert!(
                slots.source_label_width < 400.0 / 2.0,
                "the source word claimed {} of a 400-point ribbon",
                slots.source_label_width
            );
            assert!(slots.nodes.iter().all(|slot| slot.label_width > 0.0));
        }
    }

    /// The width of the harness window. Node positions are asserted against
    /// this rather than against the layout constants, so the assertions
    /// survive a change to those constants.
    const RIBBON_WINDOW: f32 = 720.0;

    /// A short route must read as a short route.
    ///
    /// The first node used to be pinned a fixed distance from the source and
    /// the last one to the ribbon's right edge, with everything in between
    /// stretched to fit. With two receivers that put one node beside the
    /// source, one against the far edge, and several hundred points of
    /// nothing between them.
    #[test]
    fn a_short_route_does_not_fling_its_last_node_at_the_far_edge() {
        let state = state_with(2, StreamState::Stopped);
        let model = model(&state, ResolvedLocale::English);
        let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, false));

        let first = harness
            .get_by_label(model.nodes[0].accessible_name.as_str())
            .rect();
        let last = harness
            .get_by_label(model.nodes[1].accessible_name.as_str())
            .rect();

        assert!(
            last.left() > first.right(),
            "the two nodes overlap: {first:?} {last:?}"
        );
        assert!(
            last.center().x < RIBBON_WINDOW / 2.0,
            "two nodes were spread over the whole ribbon: the second sits at \
             x={} in a {RIBBON_WINDOW}-point window",
            last.center().x
        );
    }

    #[test]
    fn the_complete_summary_reaches_the_accessibility_tree_in_both_locales() {
        for &locale in LOCALES {
            let state = state_with(
                2,
                StreamState::Streaming {
                    generation: GenerationId(1),
                },
            );
            let model = model(&state, locale);
            let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, false));
            let announced = accessible_strings(harness.root());
            assert!(
                announced.iter().any(|text| text == &model.summary),
                "{locale:?} never announced the summary: {announced:?}"
            );
        }
    }

    #[test]
    fn every_painted_node_names_itself_and_its_state() {
        let state = state_with(
            3,
            StreamState::Streaming {
                generation: GenerationId(2),
            },
        );
        let model = model(&state, ResolvedLocale::English);
        let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, false));
        let announced = accessible_strings(harness.root());
        for node in &model.nodes {
            assert!(
                announced.iter().any(|text| text.contains(&node.name)),
                "{} was painted without a name: {announced:?}",
                node.name
            );
        }
    }

    #[test]
    fn more_than_four_receivers_paint_three_nodes_and_one_counted_node() {
        let state = state_with(6, StreamState::Stopped);
        let model = model(&state, ResolvedLocale::English);
        let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, false));
        let announced = accessible_strings(harness.root());

        assert_eq!(model.nodes.len(), 3);
        let overflow = model.overflow.as_ref().expect("a counted node");
        assert!(
            announced
                .iter()
                .any(|text| text.contains(&overflow.accessible_name)),
            "the counted node did not announce what it stands for: {announced:?}"
        );
        // The three hidden receivers are not painted as nodes, but the
        // summary still names them.
        for hidden in ["Room 4", "Room 5", "Room 6"] {
            assert!(
                announced
                    .iter()
                    .any(|text| text.contains(hidden) && text == &model.summary),
                "{hidden} left the accessibility tree entirely"
            );
        }
    }

    /// The symbols are the colour-free half of the ribbon; High Contrast has
    /// no distinct route colours at all, so they have to survive it.
    #[test]
    fn the_phase_and_node_symbols_survive_high_contrast() {
        let state = state_with(
            2,
            StreamState::Streaming {
                generation: GenerationId(3),
            },
        );
        let model = model(&state, ResolvedLocale::English);
        let harness = still_ribbon(model.clone(), tokens(ThemePreference::Light, true));
        let announced = accessible_strings(harness.root()).join(" ");
        assert!(
            announced.contains(&model.phase_label),
            "the phase lost its word in High Contrast: {announced}"
        );
        for node in &model.nodes {
            assert!(
                announced.contains(&node.state_label),
                "{} lost its state word in High Contrast",
                node.name
            );
        }
    }

    #[test]
    fn without_motion_the_live_route_reaches_its_final_state_on_the_first_frame() {
        let state = state_with(
            2,
            StreamState::Streaming {
                generation: GenerationId(4),
            },
        );
        let harness = still_ribbon(
            model(&state, ResolvedLocale::English),
            tokens(ThemePreference::Light, false),
        );
        assert_eq!(harness.state().sweep, 1.0);
    }

    #[test]
    fn a_route_that_is_not_live_never_sweeps() {
        let state = state_with(2, StreamState::Stopped);
        let mut harness = harness(Fixture {
            model: model(&state, ResolvedLocale::English),
            tokens: tokens(ThemePreference::Light, false),
            animation_seconds: ROUTE_SWEEP_SECONDS,
            sweep: 1.0,
            sweeps: Vec::new(),
            route_rect: egui::Rect::NOTHING,
        });
        for _ in 0..20 {
            harness.step();
        }
        assert_eq!(harness.state().sweep, 0.0);
    }

    /// One bounded sweep: once the route is live it settles at full reach,
    /// never falls back, and never starts again. An indefinite animation or a
    /// repeating timer would show up as a value that moves after it settled.
    #[test]
    fn the_live_sweep_settles_once_and_never_restarts() {
        let stopped = state_with(2, StreamState::Stopped);
        let live = state_with(
            2,
            StreamState::Streaming {
                generation: GenerationId(5),
            },
        );
        let mut harness = harness(Fixture {
            model: model(&stopped, ResolvedLocale::English),
            tokens: tokens(ThemePreference::Light, false),
            animation_seconds: ROUTE_SWEEP_SECONDS,
            sweep: 1.0,
            sweeps: Vec::new(),
            route_rect: egui::Rect::NOTHING,
        });
        assert_eq!(harness.state().sweep, 0.0, "a stopped route does not sweep");

        harness.state_mut().model = model(&live, ResolvedLocale::English);
        harness.state_mut().sweeps.clear();
        for _ in 0..30 {
            harness.step();
        }
        let sweeps = harness.state().sweeps.clone();
        assert_eq!(
            sweeps.last(),
            Some(&1.0),
            "the sweep never reached full reach: {sweeps:?}"
        );
        for pair in sweeps.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "the sweep ran backwards, so it repeats: {sweeps:?}"
            );
        }
    }

    /// The sweep is bounded by the specified 120 ms and disappears entirely
    /// when Windows reports that animations are off. Both ends are checked
    /// against the specification's number, not against the ribbon's own
    /// constant: a theme that raised its transition must not lengthen it.
    #[test]
    fn the_sweep_is_zero_without_motion_and_capped_at_one_hundred_twenty_milliseconds() {
        let with_motion = crate::ui::theme::resolve_theme(
            ThemePreference::Light,
            None,
            super::super::test_support::appearance(false),
        );
        assert_eq!(sweep_seconds(0.0), 0.0);
        assert_eq!(sweep_seconds(with_motion.transition_seconds), 0.0);
        assert_eq!(sweep_seconds(0.12), 0.12);
        assert_eq!(
            sweep_seconds(2.5),
            0.12,
            "a long transition is still capped"
        );
    }

    #[test]
    fn the_ribbon_offers_no_control_at_all() {
        let state = state_with(2, StreamState::Stopped);
        let harness = still_ribbon(
            model(&state, ResolvedLocale::English),
            tokens(ThemePreference::Light, false),
        );
        for node in harness.root().children_recursive() {
            let role = node.accesskit_node().role();
            assert!(
                !matches!(
                    role,
                    accesskit::Role::Button | accesskit::Role::CheckBox | accesskit::Role::Slider
                ),
                "the ribbon is a picture of state, not a control: {role:?}"
            );
        }
    }
}
