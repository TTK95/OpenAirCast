//! Speakers / Lautsprecher.
//!
//! A selection-capable inventory of what discovery actually found. Every
//! visible string is either catalog copy or a receiver's own name and device
//! class; nothing here reads an address, a socket, or a service record.
//!
//! Overview and Speakers deliberately reuse the same card and the global
//! staged selection. The Shell owns the one Apply/Discard bar, so neither page
//! can gain a competing draft or an extra confirmation action. Per-speaker
//! level remains a separate live control below the selection card: operating a
//! slider must never toggle its receiver. When discovery has found nothing the
//! page shows the localized empty state instead -- shown only then, because an
//! empty state displayed beside a populated list would be a false statement.

use crate::app::{AppEvent, UiSnapshot};
use crate::ui::components::{empty_state, receiver_card, receiver_level};
use crate::ui::i18n::ReceiverCountArgs;
use crate::ui::layout::UiResources;
use crate::ui::presentation::{
    receiver_card_models, receiver_level_model, EmptyStateModel, SECTION_GAP,
};
use crate::ui::theme::TypographyRole;

/// Renders the Speakers destination.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;

    if snapshot.receivers.is_empty() {
        let model = EmptyStateModel::speakers(catalog);
        empty_state::show(ui, tokens, &model, emit);
        return;
    }

    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        tokens.ink,
        &catalog.receiver_count(ReceiverCountArgs {
            count: snapshot.receivers.len(),
        }),
    );
    ui.add_space(SECTION_GAP);

    for model in receiver_card_models(snapshot, catalog) {
        // The card's own egui identity is salted with the MAC-derived device
        // id, so a discovery reorder moves a row without moving its focus.
        receiver_card::show(ui, tokens, &model, emit);
        if let Some(level) = receiver_level_model(snapshot, &model.id, catalog) {
            receiver_level::show(ui, tokens, &level, snapshot.shutting_down, emit);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use airplay_core::DeviceId;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::app::{
        reduce, AppState, Availability, DiscoveryState, Page, ReceiverState, ResolvedLocale,
        ThemePreference, UiSnapshot,
    };
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::components::app_shell::{change_bar_model, show_shell};
    use crate::ui::i18n::Catalog;
    use crate::ui::layout::UiResources;
    use crate::ui::presentation::{receiver_card_models, receiver_level_model, VOLUME_STEP};
    use crate::ui::theme;

    const APPEARANCE: SystemAppearance = SystemAppearance {
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

    fn receiver(last: u8, name: &str) -> ReceiverState {
        ReceiverState {
            id: DeviceId([0xa0, 0xb1, 0xc2, 0xd3, 0xe4, last]),
            name: name.into(),
            model: "AudioAccessory1,1".into(),
            availability: Availability::Available,
        }
    }

    fn state() -> AppState {
        let first = receiver(1, "Office");
        let second = receiver(2, "Kitchen");
        AppState {
            page: Page::Speakers,
            receiver_levels: Some(BTreeMap::from([
                (first.id.clone(), 0.5),
                (second.id.clone(), 0.6),
            ])),
            desired_receivers: HashSet::from([first.id.clone()]),
            staged_receivers: HashSet::from([first.id.clone()]),
            receivers: vec![first, second],
            discovery: DiscoveryState::Ready,
            ..AppState::default()
        }
    }

    type Shell = egui_kittest::Harness<'static, Vec<AppEvent>>;

    fn shell(state: AppState) -> Shell {
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, events: &mut Vec<AppEvent>| {
                let resolved = theme::resolve_theme(ThemePreference::Light, None, APPEARANCE);
                theme::apply_theme(ui.ctx(), &resolved);
                let resources = UiResources {
                    tokens: &resolved.tokens,
                    catalog: Catalog::new(ResolvedLocale::English),
                };
                let snapshot = UiSnapshot::from_state(&state);
                let bar = change_bar_model(&snapshot, resources.catalog);
                show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut |event| {
                    events.push(event)
                });
            },
            Vec::new(),
        );
        harness.set_size(egui::vec2(1120.0, 720.0));
        harness.run();
        harness
    }

    #[test]
    fn speakers_selection_survives_navigation_and_uses_the_shell_apply_feedback() {
        let mut staged = state();
        let second = staged.receivers[1].id.clone();
        let second_card = receiver_card_models(
            &UiSnapshot::from_state(&staged),
            Catalog::new(ResolvedLocale::English),
        )
        .into_iter()
        .find(|card| card.id == second)
        .expect("the selected receiver survives snapshot ordering")
        .accessible_name;

        let mut speakers = shell(staged.clone());
        speakers
            .get_by_role_and_label(accesskit::Role::CheckBox, &second_card)
            .click();
        speakers.step();
        assert_eq!(
            speakers.state().as_slice(),
            [AppEvent::ToggleStagedReceiver(second.clone())]
        );

        reduce(&mut staged, AppEvent::ToggleStagedReceiver(second.clone()));
        reduce(&mut staged, AppEvent::Navigate(Page::Home));
        let overview = shell(staged.clone());
        let overview_card = receiver_card_models(
            &UiSnapshot::from_state(&staged),
            Catalog::new(ResolvedLocale::English),
        )
        .into_iter()
        .find(|card| card.id == second)
        .expect("the staged receiver survives navigation")
        .accessible_name;
        assert!(
            overview
                .get_by_label(&overview_card)
                .accesskit_node()
                .toggled()
                == Some(accesskit::Toggled::True),
            "the Overview must render the Speakers draft, not an independent selection"
        );
        assert!(
            overview.root().children_recursive().any(|node| {
                let node = node.accesskit_node();
                node.label().as_deref() == Some("1 speaker added")
                    || node.value().as_deref() == Some("1 speaker added")
            }),
            "the one shell-owned change bar must describe the same draft"
        );
    }

    #[test]
    fn speaker_slider_changes_only_its_level_not_the_staged_selection() {
        let state = state();
        let snapshot = UiSnapshot::from_state(&state);
        let level = receiver_level_model(
            &snapshot,
            &state.receivers[1].id,
            Catalog::new(ResolvedLocale::English),
        )
        .expect("known backend levels expose a slider");
        let mut speakers = shell(state);
        speakers.get_by_label(&level.accessible_name).focus();
        speakers.step();
        speakers.state_mut().clear();
        speakers.key_press(egui::Key::ArrowRight);
        speakers.step();
        assert_eq!(
            speakers.state().as_slice(),
            [AppEvent::ReceiverLevelRequested {
                receiver: level.receiver,
                level: 0.6 + VOLUME_STEP,
            }],
            "the slider has its own identity and must not activate the card checkbox"
        );
    }
}
