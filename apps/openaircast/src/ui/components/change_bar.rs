//! The staged-change bar.
//!
//! It renders a localized summary of the staged difference, Apply, and
//! Discard -- in that accessibility and tab order -- into whatever `Ui` it is
//! handed, and it dispatches exactly two typed events.
//!
//! What it deliberately does *not* do is place itself. It owns no panel, no
//! fixed position, no scroll region, and no page selection; pinning it to the
//! bottom of the page viewport and reserving the space it needs belongs to the
//! App Shell. A component that placed itself would put a second owner in
//! charge of the page geometry, and the bar would end up drawn over the
//! content it is supposed to sit below.

use egui::{Sense, WidgetInfo, WidgetType};

use super::{command_action, CONTROL_GAP};
use crate::app::AppEvent;
use crate::ui::presentation::{
    ChangeBarModel, CommandActionModel, Emphasis, FilledActionOwner, CARD_PADDING,
};
use crate::ui::theme::ThemeTokens;

/// Renders the bar's content and dispatches Apply or Discard.
///
/// `apply_label` and `discard_label` arrive already localized, so this
/// renderer performs no catalog lookup of its own.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &ChangeBarModel,
    apply_label: &str,
    discard_label: &str,
    emit: &mut dyn FnMut(AppEvent),
) {
    egui::Frame::NONE
        .fill(tokens.surface)
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                show_summary(ui, tokens, &model.summary);
                ui.add_space(CONTROL_GAP);

                let apply = CommandActionModel::new(
                    apply_label.to_owned(),
                    model.apply_emphasis(),
                    model.apply_enabled,
                );
                if command_action::show(ui, tokens, &apply).clicked() {
                    emit(AppEvent::ApplyStagedReceivers);
                }

                ui.add_space(CONTROL_GAP);
                let discard = CommandActionModel::new(
                    discard_label.to_owned(),
                    Emphasis::Quiet,
                    model.discard_enabled,
                );
                if command_action::show(ui, tokens, &discard).clicked() {
                    emit(AppEvent::DiscardStagedReceivers);
                }
            });
        });
}

/// The summary is one accessible text node so the change is announced once.
fn show_summary(ui: &mut egui::Ui, tokens: &ThemeTokens, summary: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(summary.to_owned(), font, tokens.ink);
    let size = galley.size();
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let announced = summary.to_owned();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, announced.clone()));
    if ui.is_rect_visible(rect) {
        ui.painter().galley(rect.left_top(), galley, tokens.ink);
    }
}

/// Builds the bar's model for the given context.
pub fn model(
    catalog: crate::ui::i18n::Catalog,
    changes: crate::ui::i18n::ChangeSummaryArgs,
    owner: FilledActionOwner,
) -> ChangeBarModel {
    ChangeBarModel::new(catalog, changes, owner)
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::super::test_support::{catalog, light, LOCALES};
    use super::*;
    use crate::ui::i18n::{ChangeSummaryArgs, TextKey};
    use crate::ui::presentation::{filled_action_owner, LifecycleAction};

    struct Fixture {
        model: ChangeBarModel,
        apply_label: String,
        discard_label: String,
        events: Vec<AppEvent>,
    }

    type BarHarness = egui_kittest::Harness<'static, Fixture>;

    fn harness(locale: crate::app::ResolvedLocale, owner: FilledActionOwner) -> BarHarness {
        let catalog = catalog(locale);
        let changes = ChangeSummaryArgs {
            added: 2,
            removed: 1,
        };
        let fixture = Fixture {
            model: model(catalog, changes, owner),
            apply_label: catalog.text(TextKey::ApplyChanges).to_owned(),
            discard_label: catalog.text(TextKey::Discard).to_owned(),
            events: Vec::new(),
        };
        let mut harness: BarHarness = egui_kittest::Harness::new_ui_state(
            |ui, fixture: &mut Fixture| {
                let tokens = light();
                let mut emitted = Vec::new();
                {
                    let mut emit = |event: AppEvent| emitted.push(event);
                    show(
                        ui,
                        &tokens,
                        &fixture.model,
                        &fixture.apply_label,
                        &fixture.discard_label,
                        &mut emit,
                    );
                }
                fixture.events.extend(emitted);
            },
            fixture,
        );
        harness.set_size(egui::vec2(900.0, 200.0));
        harness.step();
        harness
    }

    #[test]
    fn apply_and_discard_dispatch_exactly_their_own_typed_event() {
        for &locale in LOCALES {
            let owner = filled_action_owner(Some(LifecycleAction::Start), true, true);

            let mut applying = harness(locale, owner);
            let apply_label = applying.state().apply_label.clone();
            applying.get_by_label(&apply_label).click();
            applying.step();
            assert_eq!(
                applying.state().events,
                vec![AppEvent::ApplyStagedReceivers],
                "{locale:?}"
            );

            let mut discarding = harness(locale, owner);
            let discard_label = discarding.state().discard_label.clone();
            discarding.get_by_label(&discard_label).click();
            discarding.step();
            assert_eq!(
                discarding.state().events,
                vec![AppEvent::DiscardStagedReceivers],
                "{locale:?}"
            );
        }
    }

    #[test]
    fn the_summary_precedes_apply_which_precedes_discard_in_the_accessibility_tree() {
        for &locale in LOCALES {
            let harness = harness(locale, FilledActionOwner::Apply);
            let announced: Vec<String> = harness
                .root()
                .children_recursive()
                .filter_map(|node| {
                    let node = node.accesskit_node();
                    node.label()
                        .map(|text| text.to_string())
                        .or_else(|| node.value().map(|text| text.to_string()))
                })
                .collect();

            let summary = harness.state().model.summary.clone();
            let index = |needle: &str| {
                announced
                    .iter()
                    .position(|text| text == needle)
                    .unwrap_or_else(|| panic!("{needle:?} missing from {announced:?}"))
            };
            let summary_at = index(&summary);
            let apply_at = index(&harness.state().apply_label);
            let discard_at = index(&harness.state().discard_label);
            assert!(
                summary_at < apply_at && apply_at < discard_at,
                "{locale:?} order was {announced:?}"
            );
        }
    }

    #[test]
    fn a_running_session_keeps_apply_quiet_but_still_actionable() {
        let owner = filled_action_owner(Some(LifecycleAction::Stop), true, false);
        let mut harness = harness(crate::app::ResolvedLocale::English, owner);
        assert!(!harness.state().model.apply_filled);
        assert!(harness.state().model.apply_enabled);

        let apply_label = harness.state().apply_label.clone();
        let node = harness.get_by_label(&apply_label);
        assert!(!node.accesskit_node().is_disabled());
        node.click();
        harness.step();
        assert_eq!(harness.state().events, vec![AppEvent::ApplyStagedReceivers]);
    }

    /// The bar must not place itself. Reading its own source is the cheapest
    /// way to prove that, and it cannot be fooled by a rendering that merely
    /// happens to look right at one window size.
    #[test]
    fn the_bar_owns_no_panel_no_fixed_area_no_scrolling_and_no_page_selection() {
        const SOURCE: &str = include_str!("change_bar.rs");
        let body = SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("the renderer precedes its tests");

        // Needles are assembled so that this test's own source cannot match.
        for needle in [
            concat!("Pan", "el"),
            concat!("Scroll", "Area"),
            concat!("Area", "::new"),
            concat!("Page", "::"),
            concat!("Window", "::new"),
            concat!("set_", "clip_rect"),
        ] {
            assert!(
                !body.contains(needle),
                "the change bar must not use {needle:?}; placement belongs to the shell"
            );
        }
    }

    #[test]
    fn an_empty_difference_leaves_nothing_to_commit() {
        let catalog = catalog(crate::app::ResolvedLocale::German);
        let empty = model(
            catalog,
            ChangeSummaryArgs {
                added: 0,
                removed: 0,
            },
            FilledActionOwner::None,
        );
        assert!(!empty.apply_enabled);
        assert!(!empty.apply_filled);
    }
}
