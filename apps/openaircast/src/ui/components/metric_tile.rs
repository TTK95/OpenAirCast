//! The metric tile.
//!
//! A tile shows a plain label, a monospace value, its unit, and -- when the
//! value is stale or was never measured -- a marker plus the localized word
//! for that state. It exposes one combined phrase to the accessibility tree
//! and is never interactive in this subproject, because no metric currently
//! has a detail view to navigate to.

use egui::{Align2, Sense, WidgetInfo, WidgetType};

use crate::ui::presentation::{MetricTileModel, CARD_RADIUS, DENSE_PADDING};
use crate::ui::theme::ThemeTokens;

/// Gap between the label row and the value row.
const ROW_GAP: f32 = 4.0;
/// Separator between the value and the freshness word.
const FRESHNESS_SEPARATOR: &str = "  ";

/// Renders one non-interactive metric tile.
pub fn show(ui: &mut egui::Ui, tokens: &ThemeTokens, model: &MetricTileModel) {
    egui::Frame::NONE
        .fill(tokens.surface)
        .corner_radius(egui::CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(egui::Margin::same(DENSE_PADDING as i8))
        .show(ui, |ui| {
            let label_font = egui::TextStyle::Small.resolve(ui.style());
            let value_font = egui::TextStyle::Monospace.resolve(ui.style());
            let width = ui.available_width().max(120.0);

            let label = ui.painter().layout(
                model.label.clone(),
                label_font.clone(),
                tokens.ink_muted,
                width,
            );
            let value_line = value_line(model);
            let value = ui
                .painter()
                .layout(value_line, value_font, tokens.ink, width);

            let height = label.size().y + ROW_GAP + value.size().y;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
            let accessible = model.accessible_phrase();
            response
                .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, accessible.clone()));

            if ui.is_rect_visible(rect) {
                let label_height = label.size().y;
                let painter = ui.painter();
                painter.galley(rect.left_top(), label, tokens.ink_muted);
                painter.galley(
                    rect.left_top() + egui::vec2(0.0, label_height + ROW_GAP),
                    value,
                    tokens.ink,
                );
                if let Some(symbol) = model.symbol {
                    painter.text(
                        rect.right_top(),
                        Align2::RIGHT_TOP,
                        symbol,
                        label_font,
                        tokens.ink,
                    );
                }
            }
        });
}

/// The visible value line: value, unit, and the freshness word when there is
/// one. The word is text, not a colour, so the state survives High Contrast.
fn value_line(model: &MetricTileModel) -> String {
    let mut line = model.value.clone();
    if let Some(unit) = &model.unit {
        line.push(' ');
        line.push_str(unit);
    }
    if let Some(freshness) = model.freshness_label.as_ref().filter(|word| **word != line) {
        line.push_str(FRESHNESS_SEPARATOR);
        line.push_str(freshness);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{accessible_strings, catalog, light, LOCALES};
    use super::*;
    use crate::ui::i18n::TextKey;
    use crate::ui::presentation::MetricFreshness;

    type MetricHarness = egui_kittest::Harness<'static, MetricTileModel>;

    fn harness(model: MetricTileModel) -> MetricHarness {
        let mut harness: MetricHarness = egui_kittest::Harness::new_ui_state(
            |ui, model: &mut MetricTileModel| {
                let tokens = light();
                show(ui, &tokens, model);
            },
            model,
        );
        harness.set_size(egui::vec2(320.0, 200.0));
        harness.step();
        harness
    }

    #[test]
    fn the_tile_announces_label_value_unit_and_freshness_as_one_phrase() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            let model = MetricTileModel::stale(catalog, "Latenz", "12", Some("ms".into()));
            let expected = model.accessible_phrase();
            let harness = harness(model);
            let announced = accessible_strings(harness.root());
            assert!(
                announced.iter().any(|text| text == &expected),
                "{locale:?} announced {announced:?}, wanted {expected:?}"
            );
            assert!(
                expected.contains(catalog.text(TextKey::Stale)),
                "{locale:?}"
            );
        }
    }

    #[test]
    fn stale_and_unknown_carry_a_marker_and_a_word_but_a_live_value_carries_neither() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            let live = MetricTileModel::measured(catalog, "Latenz", "12", Some("ms".into()));
            assert_eq!(live.symbol, None);
            assert_eq!(live.freshness_label, None);
            assert_eq!(value_line(&live), "12 ms");

            for model in [
                MetricTileModel::stale(catalog, "Latenz", "12", Some("ms".into())),
                MetricTileModel::unknown(catalog, "Latenz"),
            ] {
                assert!(model.symbol.is_some(), "{locale:?} {:?}", model.freshness);
                let word = model
                    .freshness_label
                    .as_ref()
                    .expect("a marked state needs its word");
                assert!(
                    value_line(&model).contains(word.as_str()),
                    "{locale:?} {:?} lost its word",
                    model.freshness
                );
            }
        }
    }

    #[test]
    fn unknown_metric_announces_its_unknown_state_once() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            let model = MetricTileModel::unknown(catalog, "Input");
            let word = catalog.text(TextKey::Unknown);
            assert_eq!(value_line(&model).matches(word).count(), 1);
            assert_eq!(model.accessible_phrase().matches(word).count(), 1);
        }
    }

    #[test]
    fn an_unknown_metric_never_renders_a_fabricated_zero() {
        for &locale in LOCALES {
            let catalog = catalog(locale);
            let model = MetricTileModel::unknown(catalog, "Latenz");
            assert_eq!(model.freshness, MetricFreshness::Unknown);
            let line = value_line(&model);
            assert!(!line.contains('0'), "{locale:?} rendered {line:?}");
            assert!(line.contains(catalog.text(TextKey::Unknown)), "{locale:?}");
        }
    }

    #[test]
    fn the_tile_exposes_no_interactive_node() {
        use egui_kittest::kittest::NodeT;

        let model = MetricTileModel::measured(
            catalog(crate::app::ResolvedLocale::English),
            "Latency",
            "12",
            Some("ms".into()),
        );
        let expected = model.accessible_phrase();
        let harness = harness(model);
        // A tile that rendered nothing would also expose nothing interactive.
        let announced = accessible_strings(harness.root());
        assert!(
            announced.iter().any(|text| text == &expected),
            "the tile announced {announced:?}, wanted {expected:?}"
        );
        let interactive = harness
            .root()
            .children_recursive()
            .filter(|node| {
                matches!(
                    node.accesskit_node().role(),
                    accesskit::Role::Button | accesskit::Role::Link | accesskit::Role::CheckBox
                )
            })
            .count();
        assert_eq!(
            interactive, 0,
            "a metric tile is not interactive without a named detail view"
        );
    }
}
