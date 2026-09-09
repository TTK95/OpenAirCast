//! The Audio Dock.
//!
//! One labelled slider and one monospace percentage. Two decisions here are
//! deliberate and easy to get wrong:
//!
//! * **The percentage is the authoritative reading.** The slider is seeded
//!   from the snapshot on every frame and the percentage is rendered from the
//!   model, never from the value the pointer is currently dragging. A dock
//!   that showed the dragged value would report a volume the reducer has not
//!   accepted -- and would keep reporting it if the command failed.
//! * **The label says "master volume", not "system volume".** OpenAirCast
//!   scales what it sends; it does not touch the Windows mixer, and the copy
//!   must not imply that it does.

use egui::{StrokeKind, WidgetInfo};

use super::{show_text, CONTROL_GAP};
use crate::app::AppEvent;
use crate::ui::presentation::{
    focus_ring, AudioDockModel, CARD_PADDING, CARD_RADIUS, CONTROL_MIN_HEIGHT, CONTROL_RADIUS,
    VOLUME_STEP,
};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Width reserved for the percentage, so the slider does not resize as the
/// number grows from one digit to three.
pub(crate) const PERCENT_WIDTH: f32 = 64.0;

/// Height of the painted volume track.
const TRACK_HEIGHT: f32 = 6.0;
/// Thickness of the ring around the slider handle.
const HANDLE_EDGE_WIDTH: f32 = 1.5;
/// egui derives the slider handle's radius from the widget's height
/// (`Slider::handle_radius`). Mirrored here so the painted track spans
/// exactly the range the handle's centre can travel, and the reading a user
/// takes off the track is the reading the widget acts on.
const HANDLE_RADIUS_DIVISOR: f32 = 2.5;

/// The handle shape the dock pins on its slider.
///
/// The radius is only half of egui's geometry: `Slider::position_range`
/// keeps the handle's centre clear of either end by `radius * aspect_ratio`
/// for a rectangular handle and by `radius` alone for a circular one. A
/// track drawn from the radius alone is therefore too short at both ends,
/// and the handle stands outside the bar it is supposed to run in. Pinned
/// here rather than read from the ambient style, so the widget and the
/// painted track cannot be changed apart from one another.
const HANDLE_SHAPE: egui::style::HandleShape = egui::style::HandleShape::Circle;

/// How far the handle's centre stays clear of either end of the slider.
///
/// Mirrors `Slider::position_range` for [`HANDLE_SHAPE`].
fn handle_travel_inset(height: f32) -> f32 {
    let radius = height / HANDLE_RADIUS_DIVISOR;
    match HANDLE_SHAPE {
        egui::style::HandleShape::Circle => radius,
        egui::style::HandleShape::Rect { aspect_ratio } => radius * aspect_ratio,
    }
}

/// Where the volume track and its handle sit inside the slider's rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VolumeTrack {
    pub rail: egui::Rect,
    /// The x the handle's centre stands at for this value.
    pub handle_center_x: f32,
}

/// The track for one slider rect and one authoritative value.
///
/// Pure, so the position a user reads off the track can be checked against
/// the percentage printed beside it without a window. The two used to have no
/// stated relationship at all, and the track was not drawn.
pub(crate) fn volume_track(rect: egui::Rect, value: f32) -> VolumeTrack {
    let inset = handle_travel_inset(rect.height());
    let left = rect.left() + inset;
    let right = (rect.right() - inset).max(left);
    let center_y = rect.center().y;
    let rail = egui::Rect::from_min_max(
        egui::pos2(left, center_y - TRACK_HEIGHT / 2.0),
        egui::pos2(right, center_y + TRACK_HEIGHT / 2.0),
    );
    VolumeTrack {
        rail,
        handle_center_x: left + (right - left) * value.clamp(0.0, 1.0),
    }
}

/// The colours the track and its handle are painted from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackColors {
    /// The bar itself.
    pub rail: egui::Color32,
    /// The handle's body.
    pub handle: egui::Color32,
    /// The ring around the handle.
    pub handle_edge: egui::Color32,
}

/// The one place the track resolves a colour.
///
/// egui fills both the rail and the resting handle from the same field,
/// `widgets.inactive.bg_fill`, which the theme sets to `surface` -- the very
/// colour the dock's card is filled with. That is why there was no bar at all
/// and the handle was a bare one-point outline floating on it. The dock
/// therefore paints its own rail and leaves egui only the handle, which is
/// the one thing the two can no longer collide over.
///
/// The rail is the muted ink rather than `border` or `surface_subtle`:
/// both of those are tuned to sit almost invisibly on a card, which is right
/// for a card's edge and wrong for a control's track. The contrast test
/// beside this function is what holds that choice to a number.
pub(crate) fn track_colors(tokens: &ThemeTokens) -> TrackColors {
    TrackColors {
        rail: tokens.ink_muted,
        handle: tokens.surface,
        handle_edge: tokens.ink,
    }
}

/// What the dock painted, so the page it sits on can be held to one geometry.
///
/// The card rect is reported rather than inferred: a card is a painted
/// surface with no accessibility node of its own, so a test that wanted to
/// compare its edge against the receiver grid's could otherwise only
/// re-derive it from the dock's own constants -- which is not a measurement
/// but a restatement of the arithmetic under test.
pub struct DockGeometry {
    /// The painted card, including its padding.
    pub card: egui::Rect,
    /// The slider, for focus, keyboard, and dispatch.
    pub slider: egui::Response,
}

/// Renders the dock and dispatches one event per accepted change.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &AudioDockModel,
    emit: &mut dyn FnMut(AppEvent),
) -> DockGeometry {
    let card = egui::Frame::NONE
        .fill(tokens.surface)
        .corner_radius(egui::CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |card| {
            card.horizontal(|row| {
                // Keep the master control in the same compact 40-point row
                // as every receiver level. Its label remains visible, while
                // the slider and authoritative percentage stay independently
                // focusable and readable in the 1120 x 720 everyday window.
                show_text(row, TypographyRole::SectionTitle, tokens.ink, &model.label);
                row.add_space(CONTROL_GAP);
                // The row's own gap is the only gap. `show_slider` sizes the
                // slider as "everything except the percentage column and one
                // `CONTROL_GAP`", and the ambient style adds a second gap of
                // its own between two allocated items -- so with the column
                // actually claimed, the row came out one `item_spacing` wider
                // than the measure it was given and the card overhung the
                // receiver cards above it. Pinned here rather than read from
                // the style, for the same reason the handle shape is.
                row.spacing_mut().item_spacing.x = 0.0;
                let response = show_slider(row, tokens, model, emit);
                row.add_space(CONTROL_GAP);
                row.allocate_ui(egui::vec2(PERCENT_WIDTH, CONTROL_MIN_HEIGHT), |cell| {
                    // The reservation has to be real for the card as well as
                    // for the slider's arithmetic. `allocate_ui` hands the
                    // child a *maximum* and then allocates only what it used,
                    // so the row ended `PERCENT_WIDTH` minus the width of the
                    // printed number short -- and the card, which egui sizes
                    // from its content's `min_rect`, ended there too: some
                    // thirty points inside the right edge of the receiver
                    // cards directly above it. Claiming the column makes the
                    // dock end where the page told it to.
                    cell.set_min_width(PERCENT_WIDTH);
                    show_text(
                        cell,
                        TypographyRole::Measurement,
                        tokens.ink,
                        &model.percent_text,
                    );
                });
                response
            })
            .inner
        });
    DockGeometry {
        card: card.response.rect,
        slider: card.inner,
    }
}

pub(crate) fn show_slider(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &AudioDockModel,
    emit: &mut dyn FnMut(AppEvent),
) -> egui::Response {
    // Seeded from the snapshot on every frame: the widget owns no state that
    // could survive a rejected command.
    let mut value = model.value;
    let width = (ui.available_width() - PERCENT_WIDTH - CONTROL_GAP).max(CONTROL_MIN_HEIGHT);

    // `Slider` sizes itself from `spacing.slider_width` (100 points by
    // default) and ignores the size `add_sized` asks for, so the dock used to
    // draw a 100-point control in a 440-point row -- and a card only wide
    // enough to hold it. This is the supported way to tell it otherwise.
    ui.spacing_mut().slider_width = width;

    // The same problem in the other axis, and the one that actually moved the
    // handle out of the bar. `Slider` takes its height from
    // `interact_size.y` (18 by default) unless the body text is taller, and
    // then returns the *union* of that inner rect with the row `add_sized`
    // justified around it. So the response the dock measures the track from
    // was 40 points tall while the widget egui laid out was 18 -- and the
    // track was scaled from a height the handle never had. Asking for the
    // height the dock already wanted makes the two rects one rect, and gives
    // the control the `CONTROL_MIN_HEIGHT` pointer target it was always
    // supposed to have.
    ui.spacing_mut().interact_size.y = CONTROL_MIN_HEIGHT;

    let colors = track_colors(tokens);
    // egui's own rail is suppressed rather than recoloured: it shares
    // `widgets.inactive.bg_fill` with the handle, so one field cannot give
    // both of them a colour that contrasts with the other.
    ui.spacing_mut().slider_rail_height = 0.0;
    {
        let widgets = &mut ui.visuals_mut().widgets;
        for state in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            state.bg_fill = colors.handle;
            state.fg_stroke = egui::Stroke::new(HANDLE_EDGE_WIDTH, colors.handle_edge);
        }
    }

    // Reserved before the widget so the rail is painted underneath its
    // handle; the rect is only known once the widget has been laid out.
    let rail_slot = ui.painter().add(egui::Shape::Noop);
    let value_slot = ui.painter().add(egui::Shape::Noop);
    let response = ui.add_sized(
        [width, CONTROL_MIN_HEIGHT],
        egui::Slider::new(&mut value, 0.0..=1.0)
            // Models already clamp valid snapshots. Always-clamping also
            // quantizes during paint and can emit a command without input
            // (e.g. 0.65 becomes 0.65000004 with the f32 step).
            .clamping(egui::SliderClamping::Edits)
            .show_value(false)
            .handle_shape(HANDLE_SHAPE)
            .step_by(f64::from(VOLUME_STEP)),
    );
    if ui.is_rect_visible(response.rect) {
        let track = volume_track(response.rect, model.value);
        ui.painter().set(
            rail_slot,
            egui::Shape::rect_filled(
                track.rail,
                egui::CornerRadius::same((TRACK_HEIGHT / 2.0) as u8),
                colors.rail,
            ),
        );
        if track.handle_center_x > track.rail.left() {
            ui.painter().set(
                value_slot,
                egui::Shape::rect_filled(
                    egui::Rect::from_min_max(
                        track.rail.min,
                        egui::pos2(track.handle_center_x, track.rail.bottom()),
                    ),
                    egui::CornerRadius::same((TRACK_HEIGHT / 2.0) as u8),
                    tokens.route,
                ),
            );
        }
    }

    let name = model.accessible_name.clone();
    let announced = f64::from(model.percent);
    response.widget_info(|| WidgetInfo::slider(true, announced, name.clone()));

    if response.changed() {
        emit(AppEvent::MasterVolumeChanged(value.clamp(0.0, 1.0)));
    }

    // egui routes a focused widget to its "active" visuals, which are not a
    // focus indicator. Without this ring the keyboard user cannot see where
    // the arrow keys would land.
    if response.has_focus() && ui.is_rect_visible(response.rect) {
        let ring = focus_ring(tokens);
        ui.painter().rect_stroke(
            response.rect.expand(ring.offset),
            egui::CornerRadius::same((CONTROL_RADIUS + ring.offset) as u8),
            egui::Stroke::new(ring.width, ring.color),
            StrokeKind::Outside,
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{accessible_strings, catalog, tokens, LOCALES};
    use super::*;
    use crate::app::{AppEvent, AppState, ResolvedLocale, ThemePreference, UiSnapshot};
    use crate::ui::presentation::{AudioDockModel, OverviewModel, VOLUME_STEP};
    use crate::ui::theme::ThemeTokens;

    fn model(volume: f32, locale: ResolvedLocale) -> AudioDockModel {
        let state = AppState {
            master_volume: volume,
            ..AppState::default()
        };
        OverviewModel::from_snapshot(&UiSnapshot::from_state(&state), catalog(locale)).audio
    }

    struct Fixture {
        model: AudioDockModel,
        tokens: ThemeTokens,
        take_focus: bool,
        events: Vec<AppEvent>,
    }

    type DockHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(model: AudioDockModel, take_focus: bool) -> DockHarness {
        let mut harness: DockHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                let mut events = Vec::new();
                {
                    let mut emit = |event: AppEvent| events.push(event);
                    let dock = show(ui, &fixture.tokens, &fixture.model, &mut emit);
                    if fixture.take_focus && !dock.slider.has_focus() {
                        dock.slider.request_focus();
                    }
                }
                fixture.events.extend(events);
            },
            Fixture {
                model,
                tokens: tokens(ThemePreference::Light, false),
                take_focus,
                events: Vec::new(),
            },
        );
        harness.set_size(egui::vec2(520.0, 160.0));
        harness.step();
        harness
    }

    fn volume_events(harness: &DockHarness) -> Vec<f32> {
        harness
            .state()
            .events
            .iter()
            .filter_map(|event| match event {
                AppEvent::MasterVolumeChanged(value) => Some(*value),
                _ => None,
            })
            .collect()
    }

    /// Every theme the shell can resolve.
    fn every_theme() -> Vec<(&'static str, ThemeTokens)> {
        vec![
            ("light", tokens(ThemePreference::Light, false)),
            ("dark", tokens(ThemePreference::Dark, false)),
            ("high contrast", tokens(ThemePreference::System, true)),
        ]
    }

    /// The track has to be a thing the eye can find.
    ///
    /// Ratios are computed, not asserted by eye: 3:1 is the threshold a
    /// non-text user-interface component has to meet, and the bar, the handle
    /// on it, and the handle's own edge are three such components.
    #[test]
    fn the_volume_track_and_its_handle_are_visible_in_every_theme() {
        use crate::ui::theme::contrast_ratio;

        for (name, tokens) in every_theme() {
            let colors = track_colors(&tokens);
            assert!(
                contrast_ratio(colors.rail, tokens.surface) >= 3.0,
                "{name}: the track is {:.2}:1 against the card it sits on",
                contrast_ratio(colors.rail, tokens.surface)
            );
            assert!(
                contrast_ratio(colors.handle, colors.rail) >= 3.0,
                "{name}: the handle is {:.2}:1 against the track",
                contrast_ratio(colors.handle, colors.rail)
            );
            assert!(
                contrast_ratio(colors.handle_edge, colors.handle) >= 3.0,
                "{name}: the handle's edge is {:.2}:1 against the handle",
                contrast_ratio(colors.handle_edge, colors.handle)
            );
        }
    }

    /// The painted track only lines up with the handle if the slider really
    /// fills the width the dock allocates for it.
    ///
    /// `Ui::add_sized` asks for a size but `Slider` also has a desired width
    /// of its own, and a slider that fell back to that default would leave
    /// the dock painting a track under a control that is somewhere else.
    /// Stated against the harness window rather than against the dock's own
    /// constants. The compact row reserves its visible master-volume label
    /// and percentage before handing the remaining half-plus of the window
    /// to the slider.
    #[test]
    fn the_slider_fills_the_width_the_dock_gives_it() {
        const WINDOW: f32 = 520.0;
        let model = model(0.6, ResolvedLocale::English);
        let harness = harness(model.clone(), false);
        let slider = harness.get_by_label(model.accessible_name.as_str()).rect();
        assert!(
            slider.width() > WINDOW * 0.5,
            "the slider is only {} wide in a {WINDOW}-point window",
            slider.width()
        );
    }

    /// The printed percentage and the drawn handle are two readings of one
    /// number, and they have to be the same reading.
    ///
    /// The window showed a label, a handle, and a persisted setting that
    /// disagreed. The label and the handle cannot: both are derived here from
    /// the snapshot, and this states that derivation rather than assuming it.
    #[test]
    fn the_handle_stands_where_the_printed_percentage_says_it_does() {
        let rect = egui::Rect::from_min_size(egui::pos2(20.0, 0.0), egui::vec2(400.0, 40.0));
        for hundredths in [0u8, 1, 25, 50, 60, 99, 100] {
            let volume = f32::from(hundredths) / 100.0;
            let model = model(volume, ResolvedLocale::English);
            let track = volume_track(rect, model.value);
            let fraction = (track.handle_center_x - track.rail.left()) / track.rail.width() * 100.0;
            assert!(
                (fraction - f32::from(model.percent)).abs() < 0.5,
                "the label says {}% and the handle stands at {fraction:.1}%",
                model.percent
            );
            assert_eq!(model.percent, hundredths, "the model lost the volume");
        }
    }

    /// The track never reaches past the widget, and the handle's centre
    /// never leaves the track -- at either end of the range.
    ///
    /// The centre, not the body: half a handle overhangs each end of the
    /// bar by construction. What the real widget does with that is stated
    /// against egui's own paint output further down; this one only holds
    /// `volume_track` to its arithmetic.
    #[test]
    fn the_track_stays_inside_the_widget_and_the_handle_inside_the_track() {
        let rect = egui::Rect::from_min_size(egui::pos2(20.0, 0.0), egui::vec2(400.0, 40.0));
        for value in [-1.0, 0.0, 0.5, 1.0, 2.0] {
            let track = volume_track(rect, value);
            assert!(
                track.rail.left() >= rect.left() && track.rail.right() <= rect.right(),
                "{value}: the track {:?} left the widget {rect:?}",
                track.rail
            );
            assert!(
                track.handle_center_x >= track.rail.left() - 0.01
                    && track.handle_center_x <= track.rail.right() + 0.01,
                "{value}: the handle stands at {} outside {:?}",
                track.handle_center_x,
                track.rail
            );
        }
        assert_eq!(
            volume_track(rect, 0.0).handle_center_x,
            volume_track(rect, 0.0).rail.left()
        );
        assert_eq!(
            volume_track(rect, 1.0).handle_center_x,
            volume_track(rect, 1.0).rail.right()
        );
    }

    #[test]
    fn the_dock_labels_the_slider_and_shows_the_percentage_in_both_locales() {
        for &locale in LOCALES {
            let model = model(0.6, locale);
            let harness = harness(model.clone(), false);
            let announced = accessible_strings(harness.root()).join(" | ");
            assert!(announced.contains(&model.label), "{locale:?}: {announced}");
            assert!(
                announced.contains(&model.percent_text),
                "{locale:?}: {announced}"
            );
            let node = harness.get_by_label(model.accessible_name.as_str());
            assert_eq!(node.accesskit_node().role(), accesskit::Role::Slider);
        }
    }

    /// The label deliberately names the application's own master volume, not
    /// the Windows system volume it does not control.
    #[test]
    fn the_label_is_the_catalogs_master_volume_wording() {
        for &locale in LOCALES {
            let model = model(0.6, locale);
            assert_eq!(
                model.label,
                catalog(locale).text(crate::ui::i18n::TextKey::MasterVolume)
            );
        }
    }

    #[test]
    fn arrow_keys_step_by_exactly_one_increment() {
        for (key, expected) in [
            (egui::Key::ArrowRight, 0.6 + VOLUME_STEP),
            (egui::Key::ArrowLeft, 0.6 - VOLUME_STEP),
        ] {
            let mut harness = harness(model(0.6, ResolvedLocale::English), true);
            harness.step();
            assert!(
                volume_events(&harness).is_empty(),
                "focus alone must not act"
            );
            harness.key_press(key);
            harness.step();
            let values = volume_events(&harness);
            assert_eq!(values.len(), 1, "{key:?} produced {values:?}");
            assert!(
                (values[0] - expected).abs() < 1e-4,
                "{key:?} moved to {} instead of {expected}",
                values[0]
            );
        }
    }

    /// The percentage is the authoritative reading. A dock that showed the
    /// dragged value would report a volume the reducer never accepted.
    #[test]
    fn the_percentage_stays_the_snapshot_value_until_the_reducer_answers() {
        let model = model(0.6, ResolvedLocale::English);
        let mut harness = harness(model.clone(), true);
        harness.key_press(egui::Key::ArrowRight);
        harness.step();
        harness.step();

        assert_eq!(volume_events(&harness).len(), 1, "one key, one event");
        let announced = accessible_strings(harness.root()).join(" | ");
        assert!(
            announced.contains(&model.percent_text),
            "the dock showed an optimistic value: {announced}"
        );
    }

    #[test]
    fn the_dock_dispatches_nothing_while_it_merely_renders() {
        for value in [0.0, 0.01, 0.25, 0.63, 0.65, 1.0] {
            let mut harness = harness(model(value, ResolvedLocale::German), false);
            harness.run();
            assert!(
                harness.state().events.is_empty(),
                "{value}: {:?}",
                harness.state().events
            );
        }
    }

    /// Every rect the frame painted, in paint order.
    fn painted_rects(harness: &DockHarness) -> Vec<egui::epaint::RectShape> {
        fn collect(shape: &egui::Shape, found: &mut Vec<egui::epaint::RectShape>) {
            match shape {
                egui::Shape::Rect(rect) => found.push(rect.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, found);
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &harness.output().shapes {
            collect(&clipped.shape, &mut found);
        }
        found
    }

    /// The bar the dock painted, read back off the frame.
    fn painted_rail(harness: &DockHarness, colors: TrackColors) -> egui::Rect {
        let mut found = painted_rects(harness)
            .into_iter()
            .filter(|rect| rect.fill == colors.rail && rect.stroke.width == 0.0)
            .map(|rect| rect.rect);
        let rail = found.next().expect("the dock painted no track at all");
        assert!(found.next().is_none(), "more than one bar was painted");
        rail
    }

    /// The handle egui painted, read back off the same frame.
    ///
    /// The handle is the only shape drawn with the handle's own edge stroke,
    /// which makes this an independent reading: it is egui's layout and
    /// egui's `HandleShape`, not anything this file computed.
    fn painted_handle(harness: &DockHarness, colors: TrackColors) -> egui::Rect {
        fn collect(shape: &egui::Shape, edge: egui::Color32, found: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Circle(circle)
                    if circle.stroke.width > 0.0 && circle.stroke.color == edge =>
                {
                    found.push(egui::Rect::from_center_size(
                        circle.center,
                        egui::Vec2::splat(circle.radius * 2.0),
                    ));
                }
                egui::Shape::Rect(rect) if rect.stroke.width > 0.0 && rect.stroke.color == edge => {
                    found.push(rect.rect);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, edge, found);
                    }
                }
                _ => {}
            }
        }
        let mut handles = Vec::new();
        for clipped in &harness.output().shapes {
            collect(&clipped.shape, colors.handle_edge, &mut handles);
        }
        let mut found = handles.into_iter();
        let handle = found.next().expect("egui painted no handle");
        assert!(found.next().is_none(), "more than one handle was painted");
        handle
    }

    /// The bar the user reads has to be the range the handle really travels.
    ///
    /// This is the claim `volume_track` exists to make, and it was false in
    /// two independent ways at once. `Slider::position_range` keeps the
    /// handle clear of either end by `radius * aspect_ratio`, not by the
    /// radius; and the response the dock measured was the union of the
    /// slider with the row around it, so the height feeding the radius was
    /// 40 where the widget's own was 18. Together they put the handle a
    /// full handle's width outside the painted bar at both ends.
    ///
    /// Stated against the two rectangles that actually reached the frame --
    /// egui's handle and the dock's bar -- so no constant in this file can
    /// satisfy it by agreeing with itself.
    #[test]
    fn the_handle_egui_paints_travels_the_bar_the_dock_paints() {
        let colors = track_colors(&tokens(ThemePreference::Light, false));
        for hundredths in [0u8, 25, 50, 75, 100] {
            let model = model(f32::from(hundredths) / 100.0, ResolvedLocale::English);
            let harness = harness(model.clone(), false);
            let rail = painted_rail(&harness, colors);
            let handle = painted_handle(&harness, colors);
            let reserved = harness.get_by_label(model.accessible_name.as_str()).rect();

            let expected = rail.left() + rail.width() * f32::from(hundredths) / 100.0;
            assert!(
                (handle.center().x - expected).abs() < 0.5,
                "{hundredths}%: the bar puts that reading at {expected} \
                 and egui painted the handle at {}",
                handle.center().x
            );
            // Half a handle overhangs the bar's end by construction -- the
            // centre is what travels it -- but nothing may overhang the row
            // the dock reserved, and at the extremes the handle has to reach
            // it exactly. That is the same shrink stated from the outside.
            assert!(
                handle.left() >= reserved.left() - 0.5 && handle.right() <= reserved.right() + 0.5,
                "{hundredths}%: the handle {handle:?} hangs out of the row {reserved:?}"
            );
        }
    }

    /// The slider has to be as tall as the dock's own control height, or the
    /// pointer target is the 18 points egui defaults to rather than the 40
    /// the layout reserves for it -- and the track is measured from a height
    /// the handle never had.
    ///
    /// Checked against the space the row reserves, not against the constant
    /// the dock reads.
    #[test]
    fn the_slider_is_as_tall_as_the_row_reserves_for_it() {
        let colors = track_colors(&tokens(ThemePreference::Light, false));
        let model = model(0.5, ResolvedLocale::English);
        let harness = harness(model.clone(), false);
        let reserved = harness.get_by_label(model.accessible_name.as_str()).rect();
        let handle = painted_handle(&harness, colors);
        assert!(
            handle.height() > reserved.height() * 0.6,
            "the handle is {} tall in a {} row, so egui laid the slider out \
             at its own default height",
            handle.height(),
            reserved.height()
        );
    }
}
