//! The empty state.
//!
//! An empty state says what is missing, why, and -- only when a real typed
//! command exists -- offers exactly one way forward. The model carries its
//! action as an `Option`, so "at most one action" is a type invariant rather
//! than a rule this renderer has to remember. A page whose backend contract is
//! not projected yet gets an honest explanation here instead of a disabled
//! button that would promise a feature that cannot run.

use egui::{Sense, WidgetInfo, WidgetType};

use super::command_action;
use crate::app::AppEvent;
use crate::ui::presentation::{
    CommandActionModel, Emphasis, EmptyStateModel, CARD_PADDING, SECTION_GAP,
};
use crate::ui::theme::ThemeTokens;

/// Gap between the title and the explanation.
const TITLE_GAP: f32 = 8.0;

/// The single phrase a screen reader announces for the empty state.
pub fn accessible_text(model: &EmptyStateModel) -> String {
    format!("{} {}", model.title, model.body)
}

/// Renders the empty state and dispatches its one real action, if it has one.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &EmptyStateModel,
    emit: &mut dyn FnMut(AppEvent),
) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                show_text(ui, tokens, model);
                if let Some(action) = &model.action {
                    ui.add_space(SECTION_GAP);
                    let command =
                        CommandActionModel::new(action.label.clone(), Emphasis::Quiet, true);
                    if command_action::show(ui, tokens, &command).clicked() {
                        emit(action.event.clone());
                    }
                }
            });
        });
}

fn show_text(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &EmptyStateModel) {
    let width = ui.available_width().max(160.0);
    let title = ui.painter().layout(
        model.title.clone(),
        egui::TextStyle::Heading.resolve(ui.style()),
        tokens.ink,
        width,
    );
    let body = ui.painter().layout(
        model.body.clone(),
        egui::TextStyle::Body.resolve(ui.style()),
        tokens.ink_muted,
        width,
    );

    let height = title.size().y + TITLE_GAP + body.size().y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let accessible = accessible_text(model);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, accessible.clone()));

    if ui.is_rect_visible(rect) {
        let title_height = title.size().y;
        let painter = ui.painter();
        painter.galley(rect.left_top(), title, tokens.ink);
        painter.galley(
            rect.left_top() + egui::vec2(0.0, title_height + TITLE_GAP),
            body,
            tokens.ink_muted,
        );
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{accessible_strings, catalog, light, LOCALES};
    use super::*;

    struct Fixture {
        model: EmptyStateModel,
        events: Vec<AppEvent>,
    }

    type EmptyHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(model: EmptyStateModel) -> EmptyHarness {
        let mut harness: EmptyHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                let tokens = light();
                let mut emit = |event: AppEvent| fixture.events.push(event);
                show(ui, &tokens, &fixture.model, &mut emit);
            },
            Fixture {
                model,
                events: Vec::new(),
            },
        );
        harness.set_size(egui::vec2(700.0, 400.0));
        harness.step();
        harness
    }

    fn button_count(harness: &EmptyHarness) -> usize {
        harness
            .root()
            .children_recursive()
            .filter(|node| node.accesskit_node().role() == accesskit::Role::Button)
            .count()
    }

    #[test]
    fn a_page_without_a_typed_command_shows_explanation_and_no_control() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            for model in [
                EmptyStateModel::speakers(catalog),
                EmptyStateModel::groups(catalog),
                EmptyStateModel::audio(catalog),
                EmptyStateModel::diagnostics(catalog),
            ] {
                let expected = accessible_text(&model);
                let harness = harness(model);
                assert_eq!(
                    button_count(&harness),
                    0,
                    "an unfinished page must not grow a fake control in {locale:?}"
                );
                let announced = accessible_strings(harness.root());
                assert!(
                    announced.iter().any(|text| text == &expected),
                    "{locale:?} announced {announced:?}, wanted {expected:?}"
                );
            }
        }
    }

    #[test]
    fn the_discovery_empty_state_offers_exactly_one_real_action() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            let model = EmptyStateModel::no_receivers(catalog);
            let label = model
                .action
                .as_ref()
                .expect("discovery has a real Refresh command")
                .label
                .clone();

            let mut harness = harness(model);
            assert_eq!(button_count(&harness), 1, "{locale:?}");
            harness.get_by_label(&label).click();
            harness.step();
            assert_eq!(
                harness.state().events,
                vec![AppEvent::RefreshRequested],
                "{locale:?}"
            );
        }
    }

    #[test]
    fn the_title_and_the_explanation_are_localized_and_differ_between_languages() {
        let german = EmptyStateModel::speakers(catalog(crate::app::ResolvedLocale::German));
        let english = EmptyStateModel::speakers(catalog(crate::app::ResolvedLocale::English));
        assert_ne!(german.title, english.title);
        assert_ne!(german.body, english.body);
    }
}
