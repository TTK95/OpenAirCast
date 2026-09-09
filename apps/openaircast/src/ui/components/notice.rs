//! The notice surface.
//!
//! A notice carries a severity marker, one localized sentence, the
//! consequence, and at most one corrective action. It never renders a backend
//! string, because the model it is given cannot contain one.
//!
//! The accessible text starts with the severity marker. The catalog has no
//! localized severity nouns yet, and a screen-reader user must still be able
//! to tell a failure from a hint, so the marker is the text equivalent until
//! those words exist.

use egui::{Align2, Sense, WidgetInfo, WidgetType};

use super::command_action;
use crate::app::CorrectiveAction;
use crate::ui::presentation::{
    notice_colors, CommandActionModel, Emphasis, NoticeModel, CARD_PADDING, CARD_RADIUS,
};
use crate::ui::theme::ThemeTokens;

/// Width reserved for the severity marker.
const MARKER_WIDTH: f32 = 24.0;
/// Gap between the marker column and the text column.
const MARKER_GAP: f32 = 8.0;
/// Gap between the sentence and the consequence.
const TEXT_GAP: f32 = 4.0;

/// The single phrase a screen reader announces for the notice.
pub fn accessible_text(model: &NoticeModel) -> String {
    let mut text = format!("{} {}", model.symbol, model.message);
    if let Some(consequence) = &model.consequence {
        text.push(' ');
        text.push_str(consequence);
    }
    text
}

/// Renders the notice and reports an activated corrective action.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &NoticeModel,
    emit: &mut dyn FnMut(CorrectiveAction),
) {
    let colors = notice_colors(tokens, model.severity);
    egui::Frame::NONE
        .fill(colors.background)
        .corner_radius(egui::CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let (marker_rect, _) =
                    ui.allocate_exact_size(egui::vec2(MARKER_WIDTH, MARKER_WIDTH), Sense::hover());
                ui.painter().text(
                    marker_rect.center(),
                    Align2::CENTER_CENTER,
                    model.symbol,
                    egui::TextStyle::Button.resolve(ui.style()),
                    colors.accent,
                );
                ui.add_space(MARKER_GAP);

                ui.vertical(|ui| {
                    show_text(ui, model, colors.foreground);
                    if let Some(action) = &model.action {
                        ui.add_space(TEXT_GAP);
                        let command =
                            CommandActionModel::new(action.label.clone(), Emphasis::Quiet, true);
                        if command_action::show(ui, tokens, &command).clicked() {
                            emit(action.corrective);
                        }
                    }
                });
            });
        });
}

/// Paints the sentence and consequence as one accessible text node.
fn show_text(ui: &mut egui::Ui, model: &NoticeModel, color: egui::Color32) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let width = ui.available_width().max(120.0);
    let message = ui
        .painter()
        .layout(model.message.clone(), font.clone(), color, width);
    let consequence = model
        .consequence
        .as_ref()
        .map(|text| ui.painter().layout(text.clone(), font, color, width));

    let height = message.size().y
        + consequence
            .as_ref()
            .map_or(0.0, |galley| TEXT_GAP + galley.size().y);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());

    let accessible = accessible_text(model);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, accessible.clone()));

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let message_height = message.size().y;
        painter.galley(rect.left_top(), message, color);
        if let Some(galley) = consequence {
            painter.galley(
                rect.left_top() + egui::vec2(0.0, message_height + TEXT_GAP),
                galley,
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{accessible_strings, catalog, light, LOCALES};
    use super::*;
    use crate::app::{NoticeCode, Severity, UserNotice};
    use crate::ui::i18n::TextKey;
    use crate::ui::presentation::severity_symbol;

    struct Fixture {
        model: NoticeModel,
        actions: Vec<CorrectiveAction>,
    }

    type NoticeHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(model: NoticeModel) -> NoticeHarness {
        let mut harness: NoticeHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                let tokens = light();
                let mut emit = |action: CorrectiveAction| fixture.actions.push(action);
                show(ui, &tokens, &fixture.model, &mut emit);
            },
            Fixture {
                model,
                actions: Vec::new(),
            },
        );
        harness.set_size(egui::vec2(600.0, 300.0));
        harness.step();
        harness
    }

    fn notice(
        severity: Severity,
        code: NoticeCode,
        action: Option<CorrectiveAction>,
    ) -> UserNotice {
        UserNotice {
            severity,
            code,
            summary: "os error 10054 while writing C:\\Users\\x\\preferences.json".into(),
            action,
        }
    }

    #[test]
    fn the_severity_marker_and_the_sentence_both_reach_the_accessibility_tree() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            for severity in [Severity::Info, Severity::Warning, Severity::Error] {
                let model = NoticeModel::from_notice(
                    &notice(severity, NoticeCode::SessionFailed, None),
                    catalog,
                );
                let harness = harness(model.clone());
                let announced = accessible_strings(harness.root());
                let expected = accessible_text(&model);
                assert!(
                    announced.iter().any(|text| text == &expected),
                    "{severity:?} in {locale:?} announced {announced:?}, wanted {expected:?}"
                );
                assert!(
                    expected.starts_with(severity_symbol(severity)),
                    "the marker must survive without colour"
                );
            }
        }
    }

    #[test]
    fn no_rendered_or_announced_string_contains_the_backend_summary() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            for code in [
                NoticeCode::SessionFailed,
                NoticeCode::PreferencesFailed,
                NoticeCode::DiscoveryFailed,
            ] {
                let model = NoticeModel::from_notice(
                    &notice(Severity::Error, code, Some(CorrectiveAction::Retry)),
                    catalog,
                );
                let harness = harness(model);
                let announced = accessible_strings(harness.root()).join(" ").to_lowercase();
                assert!(
                    !announced.is_empty(),
                    "{code:?} in {locale:?} announced nothing at all"
                );
                for sentinel in ["os error", "c:\\", "preferences.json", "10054"] {
                    assert!(
                        !announced.contains(sentinel),
                        "{code:?} in {locale:?} leaked {sentinel:?}: {announced}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_notice_offers_at_most_one_corrective_action_and_dispatches_it_once() {
        let catalog = catalog(crate::app::ResolvedLocale::English);
        let model = NoticeModel::from_notice(
            &notice(
                Severity::Warning,
                NoticeCode::NoReceiverAvailable,
                Some(CorrectiveAction::Refresh),
            ),
            catalog,
        );
        let label = model
            .action
            .as_ref()
            .expect("the fixture offers an action")
            .label
            .clone();
        assert_eq!(label, catalog.text(TextKey::Refresh));

        let mut harness = harness(model);
        let buttons = harness
            .root()
            .children_recursive()
            .filter(|node| node.accesskit_node().role() == accesskit::Role::Button)
            .count();
        assert_eq!(buttons, 1, "a notice must not grow a second command");

        harness.get_by_label(&label).click();
        harness.step();
        assert_eq!(harness.state().actions, vec![CorrectiveAction::Refresh]);
    }

    #[test]
    fn a_notice_without_a_corrective_action_renders_no_command_at_all() {
        let model = NoticeModel::from_notice(
            &notice(Severity::Info, NoticeCode::InvalidInput, None),
            catalog(crate::app::ResolvedLocale::German),
        );
        let expected = accessible_text(&model);
        let harness = harness(model);
        // A notice that rendered nothing would also render no button.
        let announced = accessible_strings(harness.root());
        assert!(
            announced.iter().any(|text| text == &expected),
            "the notice announced {announced:?}, wanted {expected:?}"
        );
        let buttons = harness
            .root()
            .children_recursive()
            .filter(|node| node.accesskit_node().role() == accesskit::Role::Button)
            .count();
        assert_eq!(
            buttons, 0,
            "no typed action means no button, not a grey one"
        );
    }
}
