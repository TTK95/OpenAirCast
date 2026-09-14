//! Audio.
//!
//! Three groups, in the order paragraph 7.4 lists them: the capture source,
//! mute, and the latency profile. Master volume is not among them -- it lives
//! in the Overview Audio Dock, where it belongs to the daily control surface,
//! and a second slider here would put two owners on one number.
//!
//! Every group is drawn only once the backend has actually reported it. The
//! model carries `Option` per group for exactly that reason: "not muted" and
//! "nothing has said whether it is muted" are different facts, and a switch
//! resting on the second one would be a reading nobody took. While all three
//! are unmeasured the page falls back to its empty state, whose copy says
//! that Windows audio has sent nothing yet -- a statement about this session,
//! which the page is now entitled to make because it reads its snapshot.
//!
//! Two rules shape the controls, both inherited from Settings:
//!
//! * **The announced name carries its group.** Three groups on one page offer
//!   short words; a screen reader hearing "Normal" alone could not place it,
//!   so every choice announces `"<group>: <choice>"`.
//! * **An unavailable choice is stated, not offered.** A gated latency
//!   profile stays on the page -- hiding it would leave the user unable to
//!   tell a missing feature from a broken one -- but it is drawn as a
//!   sentence rather than as a control, so it is not in the tab order and
//!   nothing announces it as a choice that answers to nothing. Drawing it
//!   live and refusing the click would be the dead control this project
//!   treats as its worst defect. The reason is a localized sentence resolved
//!   from [`crate::app::LatencyUnavailable`] -- the backend's own free-form
//!   text for the same refusal is never read, let alone painted.
//!
//! The capture endpoints are addressed by [`crate::app::AudioEndpointKey`],
//! an opaque counter. The raw Windows endpoint ID never leaves
//! `crate::backend_bridge`, which allocates the keys on the way in and
//! translates them back on the way out.

use egui::{WidgetInfo, WidgetType};

use crate::app::{AppEvent, UiSnapshot};
use crate::ui::components::{empty_state, metric_tile, switch};
use crate::ui::layout::UiResources;
use crate::ui::presentation::{
    AudioPageModel, CaptureGroupModel, CaptureSourceModel, EmptyStateModel, LatencyGroupModel,
    MuteGroupModel, DENSE_PADDING, SECTION_GAP,
};
use crate::ui::theme::TypographyRole;

/// Renders the Audio destination.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    super::hardware_check::entry(ui, resources, emit);
    let model = AudioPageModel::from_snapshot(snapshot, resources.catalog);

    if model.is_empty() {
        let empty = EmptyStateModel::audio(resources.catalog);
        empty_state::show(ui, resources.tokens, &empty, emit);
        return;
    }

    let mut first = true;
    if let Some(capture) = &model.capture {
        first = false;
        super::group_card(ui, resources, |card| {
            capture_group(card, resources, capture, emit)
        });
    }
    if let Some(mute) = &model.mute {
        if !first {
            ui.add_space(SECTION_GAP);
        }
        first = false;
        super::group_card(ui, resources, |card| {
            mute_group(card, resources, mute, emit)
        });
    }
    if let Some(latency) = &model.latency {
        if !first {
            ui.add_space(SECTION_GAP);
        }
        super::group_card(ui, resources, |card| {
            latency_group(card, resources, latency, emit)
        });
    }
}

fn capture_group(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    model: &CaptureGroupModel,
    emit: &mut dyn FnMut(AppEvent),
) {
    super::group_heading(ui, resources, &model.title, &model.description);

    match &model.source {
        CaptureSourceModel::Offered(rows) => {
            for row in rows {
                let endpoint_key = match &row.request {
                    AppEvent::AudioEndpointRequested(
                        crate::app::AudioEndpointRequest::Endpoint(key),
                    ) => Some(key.0),
                    _ => None, // The system-default choice has no endpoint key.
                };
                let id = ui.id().with(("capture_endpoint", endpoint_key));
                // An explicit child identity also stabilizes the radio's
                // automatic ID when enumeration inserts or reorders rows.
                let response = ui
                    .scope_builder(egui::UiBuilder::new().id(id), |ui| {
                        choice_row(ui, &row.label, &row.accessible_name, row.selected)
                    })
                    .inner;
                if response.clicked() {
                    emit(row.request.clone());
                }
            }
        }
        // Text, not a radio group of one. The same reasoning as the gated
        // latency profile below, applied to the case where the choice is not
        // refused but simply does not exist: the source stays on the page, so
        // a capability nobody published cannot be mistaken for a broken one,
        // and it is not in the tab order, so nothing announces itself as a
        // choice that answers to nothing.
        CaptureSourceModel::Fixed { line, note } => {
            super::show_text(ui, TypographyRole::Body, resources.tokens.ink, line);
            if model.refresh_error.is_none() {
                super::show_text(
                    ui,
                    TypographyRole::Secondary,
                    resources.tokens.ink_muted,
                    note,
                );
            }
        }
    }

    if let Some(error) = &model.refresh_error {
        super::show_text(ui, TypographyRole::Secondary, resources.tokens.ink, error);
    }

    if let Some(missing) = &model.missing_selection {
        super::show_text(
            ui,
            TypographyRole::Secondary,
            resources.tokens.ink_muted,
            missing,
        );
    }

    ui.add_space(DENSE_PADDING);
    ui.columns(2, |cells| {
        metric_tile::show(&mut cells[0], resources.tokens, &model.captured);
        metric_tile::show(&mut cells[1], resources.tokens, &model.state);
    });
}

fn mute_group(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    model: &MuteGroupModel,
    emit: &mut dyn FnMut(AppEvent),
) {
    super::group_heading(ui, resources, &model.title, &model.description);
    switch::show(
        ui,
        resources.tokens,
        &model.switch,
        model.toggle.clone(),
        emit,
    );
}

fn latency_group(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    model: &LatencyGroupModel,
    emit: &mut dyn FnMut(AppEvent),
) {
    super::group_heading(ui, resources, &model.title, &model.description);

    for row in &model.rows {
        match &row.request {
            Some(request) => {
                if choice_row(ui, &row.label, &row.accessible_name, row.selected).clicked() {
                    emit(request.clone());
                }
            }
            // Text, not a disabled control. The profile stays visible -- a
            // hidden one could not be told from a broken one -- but it is not
            // in the tab order, so nothing announces itself as a choice that
            // answers to nothing.
            None => {
                if let Some(line) = &row.unavailable_line {
                    super::show_text(ui, TypographyRole::Body, resources.tokens.ink_muted, line);
                }
            }
        }
    }
}

/// One radio choice of an Audio group.
///
/// The painted label is the short word; the announced name carries the group,
/// so that two groups offering short words stay distinguishable.
///
/// Every row this function draws is selectable. A refused latency profile
/// never reaches it -- the page draws that as a sentence instead -- so there
/// is no disabled state to model here.
fn choice_row(
    ui: &mut egui::Ui,
    label: &str,
    accessible_name: &str,
    selected: bool,
) -> egui::Response {
    // `add_sized` centers the radio's contents. Keep a full-width target,
    // but align its indicator and label to the beginning of every row.
    let response = ui
        .allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 40.0),
            egui::Layout::left_to_right(egui::Align::Center)
                .with_main_align(egui::Align::Min)
                .with_main_justify(true),
            |row| {
                row.spacing_mut().interact_size.y = 40.0;
                row.add(egui::RadioButton::new(selected, label))
            },
        )
        .inner;
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::RadioButton, true, selected, accessible_name)
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        AppState, AudioEndpointChoice, AudioEndpointKey, AudioEndpointRequest,
        AudioEndpointSelection, AudioSourceReading, CaptureState, ResolvedLocale, ThemePreference,
    };
    use crate::ui::components::test_support::{catalog, tokens};
    use crate::ui::i18n::{NamedValueArgs, TextKey};
    use egui_kittest::kittest::Queryable;

    #[test]
    fn choice_labels_share_a_left_edge_in_wide_and_narrow_rows() {
        fn text_positions(shape: &egui::Shape, positions: &mut Vec<(String, f32)>) {
            match shape {
                egui::Shape::Text(text) => {
                    positions.push((text.galley.text().to_owned(), text.pos.x));
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_positions(shape, positions);
                    }
                }
                _ => {}
            }
        }

        for width in [360.0, 888.0] {
            let labels = ["USB DAC", "Headphones (Arctis 7 Game)", "Normal"];
            let mut harness = egui_kittest::Harness::new_ui(move |ui| {
                for label in labels {
                    choice_row(ui, label, label, false);
                }
            });
            harness.set_size(egui::vec2(width, 240.0));
            harness.step();
            let mut painted = Vec::new();
            for clipped in &harness.output().shapes {
                text_positions(&clipped.shape, &mut painted);
            }
            let mut previous_left: Option<f32> = None;
            for label in labels {
                let rect = harness.get_by_label(label).rect();
                assert!(rect.height() >= 40.0, "keep the accessible click height");
                assert!(rect.width() >= width - 32.0, "keep the full-width target");
                let left = painted.iter().find(|(text, _)| text == label).unwrap().1;
                assert!(
                    left - rect.left() < 40.0,
                    "{label} is centered at width {width}"
                );
                if let Some(previous) = previous_left {
                    assert!(
                        (left - previous).abs() < 1.0,
                        "labels must share their left edge"
                    );
                }
                previous_left = Some(left);
            }
        }
    }

    #[test]
    fn refresh_failure_is_localized_preserves_choices_and_clears_after_success() {
        for locale in [ResolvedLocale::German, ResolvedLocale::English] {
            for known in [false, true] {
                let mut state = AppState::default();
                state.audio_source = Some(AudioSourceReading {
                    refresh_failed: true,
                    endpoints_known: known,
                    endpoints: if known {
                        vec![AudioEndpointChoice {
                            key: AudioEndpointKey(42),
                            name: "USB DAC".into(),
                        }]
                    } else {
                        Vec::new()
                    },
                    selection: AudioEndpointSelection::SystemDefault,
                    captured_key: None,
                    captured_name: None,
                    state: CaptureState::Unavailable,
                });
                let catalog = catalog(locale);
                let palette = tokens(ThemePreference::Light, true);
                let mut harness = egui_kittest::Harness::new_ui_state(
                    move |ui, fixture: &mut (AppState, Vec<AppEvent>)| {
                        show(
                            ui,
                            &UiSnapshot::from_state(&fixture.0),
                            &UiResources {
                                tokens: &palette,
                                catalog,
                            },
                            &mut |event| fixture.1.push(event),
                        );
                    },
                    (state, Vec::new()),
                );
                let failure = catalog.text(if known {
                    TextKey::AudioCaptureRefreshFailed
                } else {
                    TextKey::AudioCaptureInventoryFailed
                });
                let strings =
                    crate::ui::components::test_support::accessible_strings(harness.root());
                assert!(
                    strings.iter().any(|text| text == failure),
                    "{locale:?}, known={known}: scan failure was hidden: {strings:?}"
                );
                if known {
                    let label = catalog.named_value(NamedValueArgs {
                        name: catalog.text(TextKey::AudioCaptureSource),
                        value: "USB DAC",
                    });
                    harness.get_by_label(label.as_str());
                }
                assert!(harness.state().1.is_empty());
                harness
                    .state_mut()
                    .0
                    .audio_source
                    .as_mut()
                    .unwrap()
                    .refresh_failed = false;
                harness.step();
                let strings =
                    crate::ui::components::test_support::accessible_strings(harness.root());
                assert!(
                    !strings.iter().any(|text| text == failure),
                    "success must remove the failure"
                );
            }
        }
    }

    #[test]
    fn playback_choices_are_keyboard_operable_and_at_least_forty_points_in_both_locales_and_contrasts(
    ) {
        for locale in [ResolvedLocale::German, ResolvedLocale::English] {
            for high_contrast in [false, true] {
                let mut state = AppState::default();
                state.audio_source = Some(AudioSourceReading {
                    refresh_failed: false,
                    endpoints_known: true,
                    endpoints: vec![AudioEndpointChoice {
                        key: AudioEndpointKey(42),
                        name: "USB DAC".into(),
                    }],
                    selection: AudioEndpointSelection::SystemDefault,
                    captured_key: None,
                    captured_name: Some("Speakers".into()),
                    state: CaptureState::Capturing,
                });
                let catalog = catalog(locale);
                let palette = tokens(ThemePreference::Light, high_contrast);
                let mut harness = egui_kittest::Harness::new_ui_state(
                    move |ui, fixture: &mut (AppState, Vec<AppEvent>)| {
                        show(
                            ui,
                            &UiSnapshot::from_state(&fixture.0),
                            &UiResources {
                                tokens: &palette,
                                catalog,
                            },
                            &mut |event| fixture.1.push(event),
                        );
                    },
                    (state, Vec::new()),
                );
                let label = catalog.named_value(NamedValueArgs {
                    name: catalog.text(TextKey::AudioCaptureSource),
                    value: "USB DAC",
                });
                assert!(
                    harness.state().1.is_empty(),
                    "rendering must not choose an endpoint"
                );
                assert!(harness.get_by_label(label.as_str()).rect().height() >= 40.0);
                harness.get_by_label(label.as_str()).focus();
                harness.step();
                harness.key_press(egui::Key::Space);
                harness.step();
                assert_eq!(
                    harness.state().1,
                    vec![AppEvent::AudioEndpointRequested(
                        AudioEndpointRequest::Endpoint(AudioEndpointKey(42))
                    )]
                );
                assert_eq!(
                    harness.state().0.audio_source.as_ref().unwrap().selection,
                    AudioEndpointSelection::SystemDefault,
                    "the choice stays backend-owned until echoed"
                );
            }
        }
    }

    #[test]
    fn focused_capture_endpoint_survives_reorder_and_removal_of_another_endpoint() {
        for remove_other in [false, true] {
            let mut state = AppState::default();
            state.audio_source = Some(AudioSourceReading {
                refresh_failed: false,
                endpoints_known: true,
                endpoints: vec![
                    AudioEndpointChoice {
                        key: AudioEndpointKey(11),
                        name: "Headphones".into(),
                    },
                    AudioEndpointChoice {
                        key: AudioEndpointKey(22),
                        name: "Speakers".into(),
                    },
                ],
                selection: AudioEndpointSelection::SystemDefault,
                captured_key: None,
                captured_name: None,
                state: CaptureState::Capturing,
            });
            let catalog = catalog(ResolvedLocale::English);
            let palette = tokens(ThemePreference::Light, false);
            let mut harness = egui_kittest::Harness::new_ui_state(
                move |ui, fixture: &mut (AppState, Vec<AppEvent>)| {
                    show(
                        ui,
                        &UiSnapshot::from_state(&fixture.0),
                        &UiResources {
                            tokens: &palette,
                            catalog,
                        },
                        &mut |event| fixture.1.push(event),
                    );
                },
                (state, Vec::new()),
            );
            let name = catalog.named_value(NamedValueArgs {
                name: catalog.text(TextKey::AudioCaptureSource),
                value: "Speakers",
            });
            harness.get_by_label(name.as_str()).focus();
            harness.step();
            let endpoints = &mut harness
                .state_mut()
                .0
                .audio_source
                .as_mut()
                .unwrap()
                .endpoints;
            if remove_other {
                endpoints.remove(0);
            } else {
                endpoints.reverse();
            }
            harness.step();
            harness.key_press(egui::Key::Space);
            harness.step();
            assert_eq!(harness.state().1,
                vec![AppEvent::AudioEndpointRequested(AudioEndpointRequest::Endpoint(AudioEndpointKey(22)))],
                "focus must remain with Speakers when other endpoints change (remove={remove_other})");
        }
    }
}
