//! Per-receiver level control, relative to the master volume.

use crate::app::AppEvent;
use crate::ui::presentation::{AudioDockModel, ReceiverLevelModel};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Draws a receiver level below its card and emits only receiver-level events.
pub fn show(
    ui: &mut egui::Ui,
    tokens: &ThemeTokens,
    model: &ReceiverLevelModel,
    shutting_down: bool,
    emit: &mut dyn FnMut(AppEvent),
) -> egui::Response {
    ui.scope_builder(
        egui::UiBuilder::new().id(egui::Id::new(("receiver_level", model.receiver.0))),
        |ui| {
            // The card already identifies the receiver. Keep its independent
            // level control on one 40-point line so three cards and their
            // controls remain usable in the everyday window. The relation to
            // the master is available from either the concise label or the
            // slider instead of consuming a second permanent line.
            ui.horizontal(|row| {
                let label = super::show_text(
                    row,
                    TypographyRole::Secondary,
                    tokens.ink_muted,
                    &model.label,
                );
                label.on_hover_text(&model.help);
                row.add_space(super::CONTROL_GAP);
                row.spacing_mut().item_spacing.x = 0.0;
                let dock = AudioDockModel {
                    label: model.label.clone(),
                    accessible_name: model.accessible_name.clone(),
                    value: model.value,
                    percent: model.percent,
                    percent_text: model.percent_text.clone(),
                };
                let response = row
                    .add_enabled_ui(!shutting_down, |row| {
                        super::audio_dock::show_slider(row, tokens, &dock, &mut |event| {
                            if let AppEvent::MasterVolumeChanged(level) = event {
                                emit(AppEvent::ReceiverLevelRequested {
                                    receiver: model.receiver.clone(),
                                    level,
                                });
                            }
                        })
                    })
                    .inner;
                response.clone().on_hover_text(&model.help);
                row.add_space(super::CONTROL_GAP);
                row.allocate_ui(
                    egui::vec2(
                        super::audio_dock::PERCENT_WIDTH,
                        crate::ui::presentation::CONTROL_MIN_HEIGHT,
                    ),
                    |cell| {
                        cell.set_min_width(super::audio_dock::PERCENT_WIDTH);
                        super::show_text(
                            cell,
                            TypographyRole::Measurement,
                            tokens.ink,
                            &model.percent_text,
                        );
                    },
                );
                response
            })
            .inner
        },
    )
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use std::collections::{BTreeMap, HashSet};

    use airplay_core::DeviceId;

    use super::super::test_support::tokens;
    use crate::app::{AppEvent, ThemePreference};
    use crate::app::{AppState, Availability, ReceiverState, UiSnapshot};
    use crate::ui::i18n::Catalog;
    use crate::ui::presentation::{receiver_level_model, ReceiverLevelModel};
    use crate::ui::theme::ThemeTokens;

    fn receiver() -> DeviceId {
        DeviceId([1, 2, 3, 4, 5, 6])
    }

    #[test]
    fn known_levels_default_missing_receiver_to_unity_and_clamp_persisted_values() {
        let mut state = AppState {
            receivers: vec![ReceiverState {
                id: receiver(),
                name: "Office".into(),
                model: "AudioAccessory5,1".into(),
                availability: Availability::Available,
            }],
            desired_receivers: HashSet::from([receiver()]),
            receiver_levels: Some(BTreeMap::new()),
            ..AppState::default()
        };
        let snapshot = UiSnapshot::from_state(&state);
        let model = receiver_level_model(
            &snapshot,
            &receiver(),
            Catalog::new(crate::app::ResolvedLocale::English),
        );
        assert_eq!(model.expect("known levels").value, 1.0);

        state.receiver_levels = Some(BTreeMap::from([(receiver(), 1.7)]));
        let model = receiver_level_model(
            &UiSnapshot::from_state(&state),
            &receiver(),
            Catalog::new(crate::app::ResolvedLocale::English),
        );
        assert_eq!(model.expect("known levels").value, 1.0);

        state.receiver_levels = Some(BTreeMap::from([(receiver(), -0.2)]));
        let model = receiver_level_model(
            &UiSnapshot::from_state(&state),
            &receiver(),
            Catalog::new(crate::app::ResolvedLocale::English),
        );
        assert_eq!(model.expect("known levels").value, 0.0);
    }

    #[test]
    fn unknown_levels_do_not_create_a_control_model() {
        let state = AppState {
            receivers: vec![ReceiverState {
                id: receiver(),
                name: "Office".into(),
                model: "AudioAccessory5,1".into(),
                availability: Availability::Available,
            }],
            ..AppState::default()
        };
        assert!(receiver_level_model(
            &UiSnapshot::from_state(&state),
            &receiver(),
            Catalog::new(crate::app::ResolvedLocale::English)
        )
        .is_none());
    }

    #[test]
    fn keyboard_level_change_targets_only_its_receiver() {
        struct Fixture {
            model: ReceiverLevelModel,
            tokens: ThemeTokens,
            events: Vec<AppEvent>,
        }
        let model = ReceiverLevelModel {
            receiver: receiver(),
            name: "Office".into(),
            label: "Speaker level".into(),
            help: "Relative to master volume".into(),
            accessible_name: "Speaker level: Office".into(),
            value: 0.5,
            percent: 50,
            percent_text: "50%".into(),
        };
        let mut harness: egui_kittest::Harness<'static, Fixture> =
            egui_kittest::Harness::new_ui_state(
                |ui, fixture: &mut Fixture| {
                    let mut events = Vec::new();
                    show(ui, &fixture.tokens, &fixture.model, false, &mut |event| {
                        events.push(event)
                    });
                    fixture.events.extend(events);
                },
                Fixture {
                    model,
                    tokens: tokens(ThemePreference::Light, false),
                    events: Vec::new(),
                },
            );
        harness.set_size(egui::vec2(520.0, 140.0));
        harness.step();
        harness.get_by_label("Speaker level: Office").focus();
        harness.key_press(egui::Key::ArrowRight);
        harness.step();
        assert_eq!(
            harness.state().events,
            vec![AppEvent::ReceiverLevelRequested {
                receiver: receiver(),
                level: 0.55
            }]
        );
    }

    #[test]
    fn receiver_level_focus_follows_identity_after_reorder_and_disables_on_shutdown() {
        struct Fixture {
            models: Vec<ReceiverLevelModel>,
            tokens: ThemeTokens,
            events: Vec<AppEvent>,
            ids: Vec<egui::Id>,
            shutting_down: bool,
        }
        let models = [("Office", 1), ("Kitchen", 2)]
            .map(|(name, last)| ReceiverLevelModel {
                receiver: DeviceId([last; 6]),
                name: name.into(),
                label: "Speaker level".into(),
                help: "Relative to master volume".into(),
                accessible_name: format!("Speaker level: {name}"),
                value: 0.5,
                percent: 50,
                percent_text: "50%".into(),
            })
            .to_vec();
        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, state: &mut Fixture| {
                state.ids.clear();
                for model in &state.models {
                    // Each row is a different parent, just like the Overview grid.
                    ui.horizontal(|ui| {
                        let response =
                            show(ui, &state.tokens, model, state.shutting_down, &mut |e| {
                                state.events.push(e)
                            });
                        state.ids.push(response.id);
                    });
                }
            },
            Fixture {
                models,
                tokens: tokens(ThemePreference::Light, false),
                events: vec![],
                ids: vec![],
                shutting_down: false,
            },
        );
        harness.set_size(egui::vec2(520.0, 300.0));
        harness.run();
        let office_id = harness.state().ids[0];
        harness.get_by_label("Speaker level: Office").focus();
        harness.step();
        harness.state_mut().models.reverse();
        harness.step();
        assert_eq!(harness.state().ids[1], office_id);
        harness.key_press(egui::Key::ArrowRight);
        harness.step();
        assert_eq!(
            harness.state().events,
            vec![AppEvent::ReceiverLevelRequested {
                receiver: DeviceId([1; 6]),
                level: 0.55,
            }]
        );
        harness.state_mut().events.clear();
        harness.state_mut().shutting_down = true;
        harness.step();
        harness.key_press(egui::Key::ArrowRight);
        harness.step();
        assert!(harness.state().events.is_empty());
    }
}
