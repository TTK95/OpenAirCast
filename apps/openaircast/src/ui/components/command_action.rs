//! The command button.
//!
//! One renderer serves both the filled primary action and every quiet
//! secondary action, so the two can never drift apart in size, focus
//! treatment, or accessible reporting. Which of the two a given button is was
//! decided upstream by [`crate::ui::presentation::filled_action_owner`].

use egui::{Sense, Stroke, StrokeKind, WidgetInfo, WidgetType};

use super::{BUTTON_HORIZONTAL_PADDING, BUTTON_VERTICAL_PADDING};
use crate::ui::presentation::{
    action_colors, focus_ring, CommandActionModel, CONTROL_MIN_HEIGHT, CONTROL_RADIUS,
};
use crate::ui::theme::ThemeTokens;

/// Renders one command button and returns its response.
///
/// The caller decides what activation means; this renderer never dispatches.
pub fn show(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &CommandActionModel) -> egui::Response {
    let colors = action_colors(tokens, model.emphasis, model.enabled);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(model.label.clone(), font, colors.foreground);

    let size = egui::vec2(
        galley.size().x + 2.0 * BUTTON_HORIZONTAL_PADDING,
        CONTROL_MIN_HEIGHT.max(galley.size().y + 2.0 * BUTTON_VERTICAL_PADDING),
    );
    // A disabled command still occupies its full size and stays in the tree;
    // it simply senses nothing, so the page geometry does not jump.
    let sense = if model.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_at_least(size, sense);

    let name = model.accessible_name.clone();
    let enabled = model.enabled;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, name.clone()));

    if ui.is_rect_visible(rect) {
        let radius = egui::CornerRadius::same(CONTROL_RADIUS as u8);
        let painter = ui.painter();
        if let Some(fill) = colors.fill {
            painter.rect_filled(rect, radius, fill);
        }
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, colors.border),
            StrokeKind::Inside,
        );
        if response.has_focus() {
            let ring = focus_ring(tokens);
            painter.rect_stroke(
                rect.expand(ring.offset),
                egui::CornerRadius::same((CONTROL_RADIUS + ring.offset) as u8),
                Stroke::new(ring.width, ring.color),
                StrokeKind::Outside,
            );
        }
        painter.galley(
            rect.center() - galley.size() / 2.0,
            galley,
            colors.foreground,
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{light, tokens};
    use super::*;
    use crate::app::ThemePreference;
    use crate::ui::presentation::Emphasis;

    struct Fixture {
        model: CommandActionModel,
        tokens: ThemeTokens,
        take_focus: bool,
        activations: usize,
        height: f32,
    }

    type CommandHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(model: CommandActionModel, tokens: ThemeTokens, take_focus: bool) -> CommandHarness {
        let mut harness: CommandHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                let response = show(ui, &fixture.tokens, &fixture.model);
                fixture.height = response.rect.height();
                if fixture.take_focus && !response.has_focus() {
                    response.request_focus();
                }
                if response.clicked() {
                    fixture.activations += 1;
                }
            },
            Fixture {
                model,
                tokens,
                take_focus,
                activations: 0,
                height: 0.0,
            },
        );
        harness.set_size(egui::vec2(400.0, 120.0));
        harness.step();
        harness
    }

    #[test]
    fn every_command_is_at_least_forty_points_high_in_both_emphases_and_themes() {
        for theme in [ThemePreference::Light, ThemePreference::Dark] {
            for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                for enabled in [true, false] {
                    let harness = harness(
                        CommandActionModel::new("Start streaming", emphasis, enabled),
                        tokens(theme, false),
                        false,
                    );
                    let height = harness.state().height;
                    assert!(
                        height >= CONTROL_MIN_HEIGHT,
                        "{theme:?}/{emphasis:?}/enabled={enabled} was {height} points high"
                    );
                }
            }
        }
    }

    #[test]
    fn an_enabled_command_reports_the_button_role_and_its_accessible_name() {
        let harness = harness(
            CommandActionModel::new("Apply changes", Emphasis::Filled, true)
                .with_accessible_name("Apply changes to the speaker selection"),
            light(),
            false,
        );
        let node = harness.get_by_label("Apply changes to the speaker selection");
        assert_eq!(node.accesskit_node().role(), accesskit::Role::Button);
        assert!(!node.accesskit_node().is_disabled());
    }

    #[test]
    fn a_disabled_command_stays_visible_reports_disabled_and_cannot_be_activated() {
        let mut harness = harness(
            CommandActionModel::new("Stopping", Emphasis::Quiet, false),
            light(),
            false,
        );
        {
            let node = harness.get_by_label("Stopping");
            assert!(
                node.accesskit_node().is_disabled(),
                "a disabled command must report its state, not merely look grey"
            );
            node.click();
        }
        harness.step();
        assert_eq!(
            harness.state().activations,
            0,
            "a disabled command must not activate"
        );
        assert!(harness.state().height >= CONTROL_MIN_HEIGHT);
    }

    #[test]
    fn a_focused_command_activates_with_space_and_with_enter() {
        for key in [egui::Key::Space, egui::Key::Enter] {
            let mut harness = harness(
                CommandActionModel::new("Start streaming", Emphasis::Filled, true),
                light(),
                true,
            );
            harness.step();
            assert_eq!(harness.state().activations, 0, "focus alone must not act");
            harness.key_press(key);
            harness.step();
            assert_eq!(
                harness.state().activations,
                1,
                "{key:?} did not activate the focused command"
            );
        }
    }

    #[test]
    fn an_accessible_click_activates_exactly_once() {
        let mut harness = harness(
            CommandActionModel::new("Stop streaming", Emphasis::Filled, true),
            light(),
            false,
        );
        harness.get_by_label("Stop streaming").click();
        harness.step();
        assert_eq!(harness.state().activations, 1);
        harness.step();
        assert_eq!(harness.state().activations, 1, "one activation, one event");
    }

    #[test]
    fn the_disabled_treatment_comes_from_the_semantic_token_in_every_theme() {
        for high_contrast in [false, true] {
            for theme in [ThemePreference::Light, ThemePreference::Dark] {
                let palette = tokens(theme, high_contrast);
                for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                    assert_eq!(
                        action_colors(&palette, emphasis, false).foreground,
                        palette.disabled,
                        "{theme:?} high_contrast={high_contrast} {emphasis:?}"
                    );
                }
            }
        }
    }
}
