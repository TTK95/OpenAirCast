//! The persistent on/off switch.
//!
//! Windows draws a setting that is either on or off as a track with a knob,
//! and the Settings destination needs exactly one such control today. It is a
//! renderer of its own rather than `egui::Checkbox` for three reasons the
//! Advanced-information row demonstrated in the running window:
//!
//! * **A checkbox with no text is not a control.** Given an empty label,
//!   egui paints its 14-point icon and nothing else -- and this theme rounds
//!   every widget corner to 8 points, which turns that icon into a plain
//!   circle. Off, it has no tick either, so what reaches the screen is an
//!   empty ring with a loose word beside it.
//! * **It is indented by the ambient button padding**, so the one control of
//!   the group started twelve points to the right of every heading, sentence,
//!   and radio row on the page.
//! * **State must survive without colour.** The word travels with the track
//!   here, so a filled track is never the only carrier of "on".
//!
//! The track is the whole target: 40 points tall, the shared control height,
//! with the 24-point track centred in it.

use egui::{Sense, StrokeKind, WidgetInfo, WidgetType};

use super::{show_text, CONTROL_GAP};
use crate::app::AppEvent;
use crate::ui::presentation::{focus_ring, SwitchModel, CONTROL_MIN_HEIGHT};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Width of the painted track.
const TRACK_WIDTH: f32 = 44.0;
/// Height of the painted track.
const TRACK_HEIGHT: f32 = 24.0;
/// Inset of the knob inside the track.
const KNOB_INSET: f32 = 3.0;

/// Renders the switch and dispatches `event` once per activation.
///
/// The caller supplies the event rather than a boolean, so the switch cannot
/// invent the direction it is being moved in: the page reads the applied
/// state from the snapshot and says what the opposite of it would be.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &SwitchModel,
    event: AppEvent,
    emit: &mut dyn FnMut(AppEvent),
) -> egui::Response {
    ui.horizontal(|row| {
        // The gap between track and word is this module's, once. The ambient
        // item spacing would silently add a second one.
        row.spacing_mut().item_spacing.x = 0.0;
        let response = show_track(row, tokens, model, event, emit);
        row.add_space(CONTROL_GAP);
        show_text(row, TypographyRole::Body, tokens.ink, &model.state_label);
        response
    })
    .inner
}

fn show_track(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &SwitchModel,
    event: AppEvent,
    emit: &mut dyn FnMut(AppEvent),
) -> egui::Response {
    let id = ui.id().with("openaircast_switch");
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(TRACK_WIDTH, CONTROL_MIN_HEIGHT), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());

    let on = model.on;
    let name = model.accessible_name.clone();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, on, name.clone()));

    // egui routes focus to a widget but leaves keyboard activation to the
    // widget itself on a custom surface.
    let activated = response.clicked()
        || (response.has_focus()
            && ui.input(|input| {
                input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
            }));
    if activated {
        emit(event);
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let track = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - TRACK_HEIGHT / 2.0),
            egui::vec2(TRACK_WIDTH, TRACK_HEIGHT),
        );
        let radius = egui::CornerRadius::same((TRACK_HEIGHT / 2.0) as u8);
        painter.rect_filled(
            track,
            radius,
            if model.on {
                tokens.route
            } else {
                tokens.surface_subtle
            },
        );
        // The edge is the border token whatever the state. Under High
        // Contrast `route` and `focus` are the same Windows highlight, so a
        // route-coloured resting edge would make the focus ring
        // indistinguishable from the switch simply being on.
        painter.rect_stroke(
            track,
            radius,
            egui::Stroke::new(1.0, tokens.border),
            StrokeKind::Inside,
        );

        let knob_radius = TRACK_HEIGHT / 2.0 - KNOB_INSET;
        let knob_x = if model.on {
            track.right() - KNOB_INSET - knob_radius
        } else {
            track.left() + KNOB_INSET + knob_radius
        };
        painter.circle_filled(
            egui::pos2(knob_x, track.center().y),
            knob_radius,
            if model.on {
                tokens.on_route
            } else {
                tokens.ink_muted
            },
        );

        if response.has_focus() {
            let ring = focus_ring(tokens);
            // Both ends extend beyond the track's hit rectangle.
            ui.painter().rect_stroke(
                track.expand(ring.offset),
                egui::CornerRadius::same((TRACK_HEIGHT / 2.0 + ring.offset) as u8),
                egui::Stroke::new(ring.width, ring.color),
                StrokeKind::Outside,
            );
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ThemePreference;
    use crate::ui::components::test_support::tokens;

    #[test]
    fn a_focused_switch_paints_its_entire_focus_outline() {
        for high_contrast in [false, true] {
            let palette = tokens(ThemePreference::Light, high_contrast);
            let model = SwitchModel {
                on: true,
                state_label: "On".into(),
                accessible_name: "Advanced information".into(),
            };
            let mut harness = egui_kittest::Harness::new_ui(move |ui| {
                egui::Frame::NONE.inner_margin(20).show(ui, |ui| {
                    show(
                        ui,
                        &palette,
                        &model,
                        AppEvent::AdvancedInformationChanged(false),
                        &mut |_| {},
                    )
                    .request_focus();
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
            assert_eq!(outlines.len(), 1, "the focused switch paints one outline");
            let (clip, outline) = outlines[0];
            assert!(
                clip.contains_rect(outline.rect.expand(outline.stroke.width)),
                "the complete outside focus stroke must survive clipping: {clip:?} {outline:?}"
            );
        }
    }
}
