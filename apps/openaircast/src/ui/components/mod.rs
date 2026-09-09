//! Reusable accessible controls for the Windows Native Control Center.
//!
//! Every renderer in this module takes three things and nothing else: a
//! `&ThemeTokens`, an already-localized model from
//! [`crate::ui::presentation`], and -- where it can act -- a typed event sink.
//!
//! That signature is the enforcement mechanism for three rules at once:
//!
//! * **No palette reaches a component.** A component cannot resolve a colour
//!   any way other than through the tokens it was handed, so High Contrast
//!   keeps its Windows system colours through every control.
//! * **No component invents copy.** Strings arrive resolved; there is no
//!   catalog lookup and no English literal inside a renderer.
//! * **No component decides emphasis.** The single filled primary action is
//!   decided once by [`crate::ui::presentation::filled_action_owner`].

pub mod app_shell;
pub mod audio_dock;
pub mod change_bar;
pub mod command_action;
pub mod empty_state;
pub mod metric_tile;
pub mod notice;
pub mod receiver_card;
pub mod receiver_level;
pub mod route_ribbon;
pub mod switch;

/// Horizontal padding between a command button's edge and its label.
pub(crate) const BUTTON_HORIZONTAL_PADDING: f32 = 16.0;
/// Vertical padding between a command button's edge and its label.
pub(crate) const BUTTON_VERTICAL_PADDING: f32 = 8.0;
/// Gap between two controls in the same row.
pub(crate) const CONTROL_GAP: f32 = 8.0;

/// One localized text node.
///
/// The painted string and the announced string are the same value, so a
/// component cannot show copy that a screen reader does not receive, and the
/// colour comes from the caller's `ThemeTokens` rather than from the egui
/// style.
pub(crate) fn show_text(
    ui: &mut egui::Ui,
    role: crate::ui::theme::TypographyRole,
    color: egui::Color32,
    text: &str,
) -> egui::Response {
    show_labeled_text(ui, role, color, text, text)
}

/// A text node whose announced name carries more than the painted string.
///
/// The ribbon needs exactly this: a short painted phase word and the complete
/// route sentence in the accessibility tree, without painting the sentence
/// twice or hiding it in a zero-sized node.
pub(crate) fn show_labeled_text(
    ui: &mut egui::Ui,
    role: crate::ui::theme::TypographyRole,
    color: egui::Color32,
    painted: &str,
    announced: &str,
) -> egui::Response {
    let width = ui.available_width().max(1.0);
    let galley = ui
        .painter()
        .layout(painted.to_owned(), role.font_id(), color, width);
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    let name = announced.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, name.clone()));
    if ui.is_rect_visible(rect) {
        ui.painter().galley(rect.left_top(), galley, color);
    }
    response
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::app::{ResolvedLocale, ThemePreference};
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::i18n::Catalog;
    use crate::ui::theme::{resolve_theme, ThemeTokens};

    pub(crate) const LOCALES: &[ResolvedLocale] =
        &[ResolvedLocale::German, ResolvedLocale::English];

    pub(crate) fn appearance(high_contrast: bool) -> SystemAppearance {
        SystemAppearance {
            client_animation_enabled: false,
            high_contrast,
            colors: SystemColors {
                background: [0x00, 0x00, 0x00],
                foreground: [0xff, 0xff, 0xff],
                highlight: [0x1a, 0xeb, 0xff],
                highlight_text: [0x00, 0x00, 0x00],
                disabled_text: [0x3f, 0xf2, 0x3f],
                link: [0xff, 0xff, 0x00],
            },
        }
    }

    /// Tokens for one theme, resolved the same way the shell resolves them.
    pub(crate) fn tokens(preference: ThemePreference, high_contrast: bool) -> ThemeTokens {
        resolve_theme(preference, None, appearance(high_contrast)).tokens
    }

    pub(crate) fn light() -> ThemeTokens {
        tokens(ThemePreference::Light, false)
    }

    pub(crate) fn catalog(locale: ResolvedLocale) -> Catalog {
        Catalog::new(locale)
    }

    /// Every name and value the accessibility tree currently exposes.
    ///
    /// egui reports a `Label` node's text as its *value* and every other
    /// widget's text as its *label*, so both have to be collected before a
    /// test can claim what a screen reader would announce.
    pub(crate) fn accessible_strings(root: egui_kittest::Node<'_>) -> Vec<String> {
        use egui_kittest::kittest::NodeT;

        let mut strings = Vec::new();
        for node in root.children_recursive() {
            let node = node.accesskit_node();
            if let Some(label) = node.label() {
                strings.push(label.to_string());
            }
            if let Some(value) = node.value() {
                strings.push(value.to_string());
            }
        }
        strings
    }

    /// Every string the last frame actually *painted*.
    ///
    /// The accessibility tree cannot answer this: a component may announce
    /// more than it paints -- [`super::show_labeled_text`] exists precisely
    /// for that -- so a claim about what a user *sees* has to be read off the
    /// paint output rather than off the names. Nested shapes are walked,
    /// because a frame's background and its content arrive as one
    /// [`egui::Shape::Vec`].
    pub(crate) fn painted_strings<S>(harness: &egui_kittest::Harness<'_, S>) -> Vec<String> {
        fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => into.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, into);
                    }
                }
                _ => {}
            }
        }

        let mut strings = Vec::new();
        for clipped in &harness.output().shapes {
            collect(&clipped.shape, &mut strings);
        }
        strings
    }

    /// Every filled rectangle the last frame painted, with its corner radius.
    ///
    /// A card is a painted surface with no accessibility node of its own, so
    /// "this row is a card and not a line of bare text" can only be read off
    /// the paint output. Nested shapes are walked for the same reason
    /// [`painted_strings`] walks them: a frame arrives as one
    /// [`egui::Shape::Vec`].
    pub(crate) fn painted_rects<S>(
        harness: &egui_kittest::Harness<'_, S>,
    ) -> Vec<(egui::Rect, egui::CornerRadius, egui::Color32)> {
        fn collect(
            shape: &egui::Shape,
            into: &mut Vec<(egui::Rect, egui::CornerRadius, egui::Color32)>,
        ) {
            match shape {
                egui::Shape::Rect(rect) => {
                    into.push((rect.rect, rect.corner_radius, rect.fill));
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, into);
                    }
                }
                _ => {}
            }
        }

        let mut rects = Vec::new();
        for clipped in &harness.output().shapes {
            collect(&clipped.shape, &mut rects);
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    /// Every renderer in this module, paired with its source.
    const SOURCES: &[(&str, &str)] = &[
        ("mod.rs", include_str!("mod.rs")),
        ("app_shell.rs", include_str!("app_shell.rs")),
        ("change_bar.rs", include_str!("change_bar.rs")),
        ("command_action.rs", include_str!("command_action.rs")),
        ("empty_state.rs", include_str!("empty_state.rs")),
        ("metric_tile.rs", include_str!("metric_tile.rs")),
        ("notice.rs", include_str!("notice.rs")),
        ("route_ribbon.rs", include_str!("route_ribbon.rs")),
        ("receiver_card.rs", include_str!("receiver_card.rs")),
        ("receiver_level.rs", include_str!("receiver_level.rs")),
        ("audio_dock.rs", include_str!("audio_dock.rs")),
        ("switch.rs", include_str!("switch.rs")),
    ];

    /// A component reaching past its `ThemeTokens` into a palette, a theme
    /// resolver, or a literal colour is an abort criterion for this design:
    /// it is exactly how High Contrast stops using Windows system colours.
    /// Reading the sources proves it for code that no test happens to render.
    /// Ways of getting at a colour that are not `ThemeTokens`.
    ///
    /// The needles are assembled from fragments so that this file's own
    /// source cannot match them.
    fn forbidden_needles() -> Vec<String> {
        [
            concat!("light_", "tokens"),
            concat!("dark_", "tokens"),
            concat!("high_contrast_", "tokens"),
            concat!("LIGHT_", "PALETTE"),
            concat!("DARK_", "PALETTE"),
            concat!("resolve_", "theme"),
            concat!("active_", "tokens"),
            concat!("Color32::from_", "rgb"),
            concat!("Color32::", "WHITE"),
            concat!("Color32::", "BLACK"),
            concat!("System", "Colors"),
            concat!("GetSys", "Color"),
        ]
        .iter()
        .map(|needle| (*needle).to_owned())
        .collect()
    }

    /// The renderer half of a source file: everything before its tests.
    fn renderer_body(source: &str) -> &str {
        source
            .split("#[cfg(test)]")
            .next()
            .expect("the renderer precedes its tests")
    }

    fn palette_accesses(body: &str) -> Vec<String> {
        forbidden_needles()
            .into_iter()
            .filter(|needle| body.contains(needle.as_str()))
            .collect()
    }

    #[test]
    fn no_component_resolves_a_colour_any_way_other_than_through_its_tokens() {
        for (name, source) in SOURCES {
            let found = palette_accesses(renderer_body(source));
            assert!(
                found.is_empty(),
                "{name} resolves a colour through {found:?} instead of its tokens"
            );
        }
    }

    /// The guard must be able to fail, or it proves nothing.
    /// Every component that can hold keyboard focus, paired with its source.
    ///
    /// egui routes a focused widget to its `widgets.active` visuals, which
    /// are a pressed state and not a focus indicator. A custom-painted
    /// control that does not paint the ring itself therefore has an
    /// *invisible* keyboard focus -- the exact defect Task 7 shipped.
    const FOCUSABLE_SOURCES: &[(&str, &str)] = &[
        ("app_shell.rs", include_str!("app_shell.rs")),
        ("audio_dock.rs", include_str!("audio_dock.rs")),
        ("command_action.rs", include_str!("command_action.rs")),
        ("receiver_card.rs", include_str!("receiver_card.rs")),
        ("receiver_level.rs", include_str!("receiver_level.rs")),
    ];

    fn paints_a_focus_ring(body: &str) -> bool {
        body.contains(concat!("has_", "focus()")) && body.contains(concat!("focus_", "ring("))
    }

    #[test]
    fn every_focusable_component_paints_its_own_focus_ring() {
        for (name, source) in FOCUSABLE_SOURCES {
            // Receiver levels delegate the whole slider, including focus
            // painting, to the same renderer as the master control.
            if *name == "receiver_level.rs" {
                assert!(renderer_body(source).contains("super::audio_dock::show_slider("));
                assert!(paints_a_focus_ring(renderer_body(include_str!(
                    "audio_dock.rs"
                ))));
                continue;
            }
            assert!(
                paints_a_focus_ring(renderer_body(source)),
                "{name} leaves keyboard focus invisible"
            );
        }
    }

    /// The guard must be able to fail, or it proves nothing.
    #[test]
    fn the_focus_ring_guard_flags_a_control_without_one() {
        assert!(!paints_a_focus_ring(
            "let (rect, response) = ui.allocate_exact_size(size, Sense::click());"
        ));
        assert!(paints_a_focus_ring(&format!(
            "if response.{} {{ let ring = {}tokens); }}",
            concat!("has_", "focus()"),
            concat!("focus_", "ring(")
        )));
    }

    #[test]
    fn the_palette_guard_flags_a_planted_access() {
        let planted = format!(
            "pub fn show(ui: &mut Ui) {{ let tokens = crate::ui::theme::{}(); }}",
            concat!("light_", "tokens")
        );
        assert_eq!(palette_accesses(&planted).len(), 1, "{planted}");
        assert!(palette_accesses("pub fn show(ui: &mut Ui) {}").is_empty());
    }
}
