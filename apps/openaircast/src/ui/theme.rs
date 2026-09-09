//! Windows Native semantic themes, typography, and motion policy.

use std::sync::Arc;

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, RichText, Style, TextStyle};

use crate::app::ThemePreference;
use crate::platform::{SystemAppearance, SystemColors};

pub const INTER_VARIABLE: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
pub const IBM_PLEX_MONO: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/openaircast.png");

pub const PAGE_TITLE_SIZE: f32 = 28.0;
pub const HERO_TITLE_SIZE: f32 = 20.0;
pub const SECTION_TITLE_SIZE: f32 = 16.0;
pub const BODY_SIZE: f32 = 14.0;
pub const SECONDARY_SIZE: f32 = 12.0;
pub const EYEBROW_SIZE: f32 = 11.0;
pub const MONO_SIZE: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypographyRole {
    PageTitle,
    HeroTitle,
    SectionTitle,
    Body,
    Button,
    Secondary,
    Eyebrow,
    Measurement,
}

impl TypographyRole {
    pub const fn font_size(self) -> f32 {
        match self {
            Self::PageTitle => 28.0,
            Self::HeroTitle => 20.0,
            Self::SectionTitle => 16.0,
            Self::Body | Self::Button => 14.0,
            Self::Secondary | Self::Measurement => 12.0,
            Self::Eyebrow => 11.0,
        }
    }

    pub const fn line_height(self) -> f32 {
        match self {
            Self::PageTitle => 34.0,
            Self::HeroTitle => 26.0,
            Self::SectionTitle => 22.0,
            Self::Body => 20.0,
            Self::Button => 18.0,
            Self::Secondary => 17.0,
            Self::Eyebrow => 15.0,
            Self::Measurement => 16.0,
        }
    }

    pub fn text_style(self) -> TextStyle {
        match self {
            Self::PageTitle => TextStyle::Heading,
            Self::HeroTitle => TextStyle::Name("openaircast-hero".into()),
            Self::SectionTitle => TextStyle::Name("openaircast-section".into()),
            Self::Body => TextStyle::Body,
            Self::Button => TextStyle::Button,
            Self::Secondary => TextStyle::Small,
            Self::Eyebrow => TextStyle::Name("openaircast-eyebrow".into()),
            Self::Measurement => TextStyle::Monospace,
        }
    }

    pub fn font_id(self) -> egui::FontId {
        match self {
            Self::Measurement => egui::FontId::monospace(self.font_size()),
            _ => egui::FontId::proportional(self.font_size()),
        }
    }

    pub fn rich_text(self, text: impl Into<String>) -> RichText {
        RichText::new(text)
            .font(self.font_id())
            .line_height(Some(self.line_height()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeTokens {
    pub canvas: Color32,
    pub surface: Color32,
    pub surface_subtle: Color32,
    pub ink: Color32,
    pub ink_muted: Color32,
    pub border: Color32,
    pub route: Color32,
    pub on_route: Color32,
    pub route_soft: Color32,
    pub live: Color32,
    pub live_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub fault: Color32,
    pub fault_soft: Color32,
    pub focus: Color32,
    pub disabled: Color32,
    pub link: Color32,
    pub focus_width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedTheme {
    Light,
    Dark,
    HighContrast,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedThemeStyle {
    pub theme: ResolvedTheme,
    pub tokens: ThemeTokens,
    pub transition_seconds: f32,
}

pub fn resolve_theme(
    preference: ThemePreference,
    system_theme: Option<egui::Theme>,
    appearance: SystemAppearance,
) -> ResolvedThemeStyle {
    let theme = if appearance.high_contrast {
        ResolvedTheme::HighContrast
    } else {
        match preference {
            ThemePreference::Light => ResolvedTheme::Light,
            ThemePreference::Dark => ResolvedTheme::Dark,
            ThemePreference::System => match system_theme.unwrap_or(egui::Theme::Light) {
                egui::Theme::Light => ResolvedTheme::Light,
                egui::Theme::Dark => ResolvedTheme::Dark,
            },
        }
    };
    let tokens = match theme {
        ResolvedTheme::Light => light_tokens(),
        ResolvedTheme::Dark => dark_tokens(),
        ResolvedTheme::HighContrast => high_contrast_tokens(appearance.colors),
    };

    ResolvedThemeStyle {
        theme,
        tokens,
        transition_seconds: if appearance.client_animation_enabled {
            0.12
        } else {
            0.0
        },
    }
}

/// Installs an already-resolved style. Preference and OS appearance resolution
/// deliberately stay outside this function so a frame never re-queries them.
pub fn apply_theme(ctx: &Context, resolved: &ResolvedThemeStyle) {
    let target = match resolved.theme {
        ResolvedTheme::Dark => egui::Theme::Dark,
        ResolvedTheme::Light | ResolvedTheme::HighContrast => egui::Theme::Light,
    };
    let mut style = (*ctx.style_of(target)).clone();
    apply_text_styles(&mut style);
    apply_spacing(&mut style);
    apply_colors(&mut style, resolved.tokens);
    style.animation_time = resolved.transition_seconds;
    ctx.set_style_of(target, style);
    ctx.data_mut(|data| {
        data.insert_temp(theme_tokens_id(), resolved.tokens);
    });
    ctx.set_theme(match resolved.theme {
        ResolvedTheme::Dark => egui::ThemePreference::Dark,
        ResolvedTheme::Light | ResolvedTheme::HighContrast => egui::ThemePreference::Light,
    });
}

pub fn install_fonts(ctx: &Context, segoe_ui_variable: Option<Arc<[u8]>>) {
    ctx.set_fonts(build_fonts(segoe_ui_variable));
}

pub fn build_fonts(segoe_ui_variable: Option<Arc<[u8]>>) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let has_segoe = segoe_ui_variable.is_some();
    fonts.font_data.insert(
        "inter_variable".into(),
        FontData::from_static(INTER_VARIABLE).into(),
    );
    fonts.font_data.insert(
        "ibm_plex_mono".into(),
        FontData::from_static(IBM_PLEX_MONO).into(),
    );
    if let Some(segoe) = segoe_ui_variable {
        fonts.font_data.insert(
            "segoe_ui_variable".into(),
            FontData::from_owned(segoe.as_ref().to_vec()).into(),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe_ui_variable".into());
    }
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(if has_segoe { 1 } else { 0 }, "inter_variable".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "ibm_plex_mono".into());
    fonts
}

pub fn load_segoe_ui_variable() -> Option<Arc<[u8]>> {
    crate::platform::fonts::load_segoe_ui_variable()
}

/// Exact semantic colors installed alongside the egui visuals. Keeping this
/// separately avoids losing roles that egui's visual style cannot represent.
pub fn active_tokens(ctx: &Context) -> ThemeTokens {
    ctx.data(|data| data.get_temp(theme_tokens_id()))
        .expect("a resolved theme must be applied before rendering OpenAirCast UI")
}

fn theme_tokens_id() -> egui::Id {
    egui::Id::new("openaircast-semantic-theme-tokens")
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |left: u8, right: u8| {
        (left as f32 + (right as f32 - left as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn color(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn light_tokens() -> ThemeTokens {
    ThemeTokens {
        canvas: color(0xF4F6FB),
        surface: color(0xFFFFFF),
        surface_subtle: color(0xE9EDF5),
        ink: color(0x172033),
        ink_muted: color(0x5C6678),
        border: color(0xDCE2ED),
        route: color(0x3168E8),
        on_route: color(0xFFFFFF),
        route_soft: color(0xE8EFFF),
        live: color(0x157766),
        live_soft: color(0xDFF4EF),
        warning: color(0xA9570D),
        warning_soft: color(0xFFF3E4),
        fault: color(0xB94B5B),
        fault_soft: color(0xFFF0F2),
        focus: color(0x3168E8),
        disabled: color(0x657084),
        link: color(0x3168E8),
        focus_width: 2.0,
    }
}

fn dark_tokens() -> ThemeTokens {
    ThemeTokens {
        canvas: color(0x10151F),
        surface: color(0x18202D),
        surface_subtle: color(0x222C3A),
        ink: color(0xF4F7FB),
        ink_muted: color(0x9BA7BA),
        border: color(0x2F3A49),
        route: color(0x7EA6FF),
        on_route: color(0x10151F),
        route_soft: color(0x243657),
        live: color(0x60D6C2),
        live_soft: color(0x183D3A),
        warning: color(0xFFB45C),
        warning_soft: color(0x3E2E1D),
        fault: color(0xFF8B9A),
        fault_soft: color(0x43252D),
        focus: color(0x7EA6FF),
        disabled: color(0x9BA7BA),
        link: color(0x7EA6FF),
        focus_width: 2.0,
    }
}

fn high_contrast_tokens(colors: SystemColors) -> ThemeTokens {
    let background = Color32::from_rgb(
        colors.background[0],
        colors.background[1],
        colors.background[2],
    );
    let foreground = Color32::from_rgb(
        colors.foreground[0],
        colors.foreground[1],
        colors.foreground[2],
    );
    let highlight = Color32::from_rgb(
        colors.highlight[0],
        colors.highlight[1],
        colors.highlight[2],
    );
    let highlight_text = Color32::from_rgb(
        colors.highlight_text[0],
        colors.highlight_text[1],
        colors.highlight_text[2],
    );
    let disabled = Color32::from_rgb(
        colors.disabled_text[0],
        colors.disabled_text[1],
        colors.disabled_text[2],
    );
    let link = Color32::from_rgb(colors.link[0], colors.link[1], colors.link[2]);
    ThemeTokens {
        canvas: background,
        surface: background,
        surface_subtle: background,
        ink: foreground,
        ink_muted: foreground,
        border: foreground,
        route: highlight,
        on_route: highlight_text,
        route_soft: background,
        live: foreground,
        live_soft: background,
        warning: foreground,
        warning_soft: background,
        fault: foreground,
        fault_soft: background,
        focus: highlight,
        disabled,
        link,
        focus_width: 3.0,
    }
}

fn apply_text_styles(style: &mut Style) {
    use egui::FontId;
    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(PAGE_TITLE_SIZE)),
        (
            TextStyle::Name("openaircast-hero".into()),
            FontId::proportional(HERO_TITLE_SIZE),
        ),
        (
            TextStyle::Name("openaircast-section".into()),
            FontId::proportional(SECTION_TITLE_SIZE),
        ),
        (TextStyle::Body, FontId::proportional(BODY_SIZE)),
        (TextStyle::Button, FontId::proportional(BODY_SIZE)),
        (TextStyle::Small, FontId::proportional(SECONDARY_SIZE)),
        (
            TextStyle::Name("openaircast-eyebrow".into()),
            FontId::proportional(EYEBROW_SIZE),
        ),
        (TextStyle::Monospace, FontId::monospace(MONO_SIZE)),
    ]
    .into();
}

fn apply_spacing(style: &mut Style) {
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 10.0);
    style.spacing.interact_size = egui::vec2(40.0, 40.0);
    style.spacing.menu_margin = egui::Margin::same(8);
    for state in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        state.corner_radius = egui::CornerRadius::same(8);
    }
    style.visuals.window_corner_radius = egui::CornerRadius::same(12);
}

fn apply_colors(style: &mut Style, tokens: ThemeTokens) {
    let visuals = &mut style.visuals;
    visuals.dark_mode = relative_luminance(tokens.canvas) < 0.5;
    visuals.override_text_color = Some(tokens.ink);
    visuals.weak_text_color = Some(tokens.ink_muted);
    visuals.panel_fill = tokens.canvas;
    visuals.window_fill = tokens.surface;
    visuals.faint_bg_color = tokens.surface_subtle;
    visuals.extreme_bg_color = tokens.surface_subtle;
    visuals.code_bg_color = tokens.live_soft;
    visuals.warn_fg_color = tokens.warning;
    visuals.error_fg_color = tokens.fault;
    visuals.hyperlink_color = tokens.link;
    visuals.window_stroke = egui::Stroke::new(1.0, tokens.border);
    visuals.selection.bg_fill = tokens.route;
    visuals.selection.stroke = egui::Stroke::new(tokens.focus_width, tokens.focus);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, tokens.ink);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, tokens.border);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, tokens.ink);
    visuals.widgets.inactive.bg_fill = tokens.surface;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, tokens.border);
    visuals.widgets.hovered.bg_fill = tokens.route_soft;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(tokens.focus_width, tokens.focus);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, tokens.ink);
    // egui routes *keyboard focus* through `widgets.active`
    // (`Widgets::style`: `is_pointer_button_down_on() || has_focus() ||
    // clicked()`), so this is the state a stock RadioButton, Checkbox or
    // DragValue paints while it holds focus. It therefore has to carry the
    // focus ring: a thicker stroke in the focus colour, which differs from
    // `inactive` in width as well as in hue, so focus survives without
    // colour. The fill is the route tint, not `fault_soft` -- focus is not an
    // error, and under High Contrast `fault_soft` collapses onto the system
    // background, leaving focus with no indicator at all.
    visuals.widgets.active.bg_fill = tokens.route_soft;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(tokens.focus_width, tokens.focus);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, tokens.ink);
    visuals.widgets.open.bg_fill = tokens.live;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, tokens.on_route);
}

pub(crate) fn relative_luminance(color: Color32) -> f32 {
    fn channel(value: u8) -> f32 {
        let c = value as f32 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

pub(crate) fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (lighter, darker) = if relative_luminance(a) >= relative_luminance(b) {
        (relative_luminance(a), relative_luminance(b))
    } else {
        (relative_luminance(b), relative_luminance(a))
    };
    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use egui::{text::LayoutJob, Align, FontSelection};

    use super::*;

    const APPEARANCE: SystemAppearance = SystemAppearance {
        client_animation_enabled: true,
        high_contrast: false,
        colors: SystemColors {
            background: [1, 2, 3],
            foreground: [4, 5, 6],
            highlight: [7, 8, 9],
            highlight_text: [10, 11, 12],
            disabled_text: [13, 14, 15],
            link: [16, 17, 18],
        },
    };

    /// Windows High Contrast Black -- the palette Windows itself ships, not a
    /// synthetic fixture, because this test makes a claim about legibility.
    const HIGH_CONTRAST_BLACK: SystemAppearance = SystemAppearance {
        client_animation_enabled: false,
        high_contrast: true,
        colors: SystemColors {
            background: [0, 0, 0],
            foreground: [255, 255, 255],
            highlight: [26, 235, 255],
            highlight_text: [0, 0, 0],
            disabled_text: [61, 61, 61],
            link: [128, 128, 255],
        },
    };

    /// Keyboard focus has to be perceivable on the stock widgets too.
    ///
    /// egui resolves *keyboard focus* to `visuals.widgets.active`
    /// (`Widgets::style` in egui 0.36.1 picks `active` on `has_focus()`), and
    /// `egui::RadioButton`, `Checkbox` and `DragValue` all paint through
    /// `Style::interact`. Settings uses all three, so this mapping is the only
    /// thing standing between those controls and an invisible focus state.
    ///
    /// The ratios are computed here rather than asserted in prose.
    #[test]
    fn keyboard_focus_is_visible_in_every_theme_by_shape_and_by_measured_contrast() {
        for (label, resolved) in [
            (
                "light",
                resolve_theme(ThemePreference::Light, None, APPEARANCE),
            ),
            (
                "dark",
                resolve_theme(ThemePreference::Dark, None, APPEARANCE),
            ),
            (
                "high contrast",
                resolve_theme(ThemePreference::System, None, HIGH_CONTRAST_BLACK),
            ),
        ] {
            let mut style = Style::default();
            apply_colors(&mut style, resolved.tokens);
            let focused = style.visuals.widgets.active;
            let resting = style.visuals.widgets.inactive;

            // Shape first: the ring grows, so the state is not carried by
            // colour alone.
            assert!(
                focused.bg_stroke.width >= resting.bg_stroke.width + 1.0,
                "{label}: focus ring is {} wide against a {} resting border",
                focused.bg_stroke.width,
                resting.bg_stroke.width
            );

            // Then colour, against the surface the control sits on. WCAG 1.4.11
            // asks 3:1 for a non-text indicator.
            let against_surface = contrast_ratio(focused.bg_stroke.color, resting.bg_fill);
            assert!(
                against_surface >= 3.0,
                "{label}: focus ring against its surface is {against_surface:.3}:1"
            );

            // The resting border is deliberately *not* held to 3:1 against the
            // ring. Under High Contrast the border is the system foreground and
            // the ring the system highlight -- Windows picks both, and the two
            // legitimately sit close (1.461:1 in High Contrast Black). What
            // separates the states there is the width above, which is why that
            // assertion comes first and holds in all three themes.
        }
    }

    #[test]
    fn embedded_assets_are_present_and_icon_has_required_sizes() {
        assert!(INTER_VARIABLE.len() > 800_000);
        assert!(IBM_PLEX_MONO.len() > 100_000);
        let dir = ico::IconDir::read(Cursor::new(include_bytes!("../../assets/openaircast.ico")))
            .expect("valid ICO");
        let mut sizes = dir
            .entries()
            .iter()
            .map(|entry| (entry.width(), entry.height()))
            .collect::<Vec<_>>();
        sizes.sort_unstable();
        assert_eq!(sizes, [(16, 16), (20, 20), (24, 24), (32, 32)]);
    }

    #[test]
    fn light_theme_has_the_complete_approved_semantic_palette() {
        let style = resolve_theme(ThemePreference::Light, Some(egui::Theme::Dark), APPEARANCE);
        assert_eq!(style.theme, ResolvedTheme::Light);
        assert_eq!(
            style.tokens,
            ThemeTokens {
                canvas: color(0xF4F6FB),
                surface: color(0xFFFFFF),
                surface_subtle: color(0xE9EDF5),
                ink: color(0x172033),
                ink_muted: color(0x5C6678),
                border: color(0xDCE2ED),
                route: color(0x3168E8),
                on_route: color(0xFFFFFF),
                route_soft: color(0xE8EFFF),
                live: color(0x157766),
                live_soft: color(0xDFF4EF),
                warning: color(0xA9570D),
                warning_soft: color(0xFFF3E4),
                fault: color(0xB94B5B),
                fault_soft: color(0xFFF0F2),
                focus: color(0x3168E8),
                disabled: color(0x657084),
                link: color(0x3168E8),
                focus_width: 2.0
            }
        );
        assert_eq!(style.transition_seconds, 0.12);
    }

    #[test]
    fn dark_theme_has_the_complete_approved_semantic_palette() {
        let style = resolve_theme(ThemePreference::Dark, Some(egui::Theme::Light), APPEARANCE);
        assert_eq!(style.theme, ResolvedTheme::Dark);
        assert_eq!(
            style.tokens,
            ThemeTokens {
                canvas: color(0x10151F),
                surface: color(0x18202D),
                surface_subtle: color(0x222C3A),
                ink: color(0xF4F7FB),
                ink_muted: color(0x9BA7BA),
                border: color(0x2F3A49),
                route: color(0x7EA6FF),
                on_route: color(0x10151F),
                route_soft: color(0x243657),
                live: color(0x60D6C2),
                live_soft: color(0x183D3A),
                warning: color(0xFFB45C),
                warning_soft: color(0x3E2E1D),
                fault: color(0xFF8B9A),
                fault_soft: color(0x43252D),
                focus: color(0x7EA6FF),
                disabled: color(0x9BA7BA),
                link: color(0x7EA6FF),
                focus_width: 2.0
            }
        );
    }

    #[test]
    fn normal_themes_keep_required_text_and_non_text_contrast_pairs() {
        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            let tokens = resolve_theme(preference, None, APPEARANCE).tokens;
            for (name, foreground, background) in [
                ("ink/canvas", tokens.ink, tokens.canvas),
                ("ink/surface", tokens.ink, tokens.surface),
                ("muted/canvas", tokens.ink_muted, tokens.canvas),
                ("disabled/canvas", tokens.disabled, tokens.canvas),
                ("link/canvas", tokens.link, tokens.canvas),
                ("on-route/route", tokens.on_route, tokens.route),
            ] {
                assert!(
                    contrast_ratio(foreground, background) >= 4.5,
                    "{preference:?} {name}"
                );
            }
            for (name, foreground, background) in [
                ("route/canvas", tokens.route, tokens.canvas),
                ("live/canvas", tokens.live, tokens.canvas),
                ("warning/canvas", tokens.warning, tokens.canvas),
                ("fault/canvas", tokens.fault, tokens.canvas),
                ("focus/canvas", tokens.focus, tokens.canvas),
            ] {
                assert!(
                    contrast_ratio(foreground, background) >= 3.0,
                    "{preference:?} {name}"
                );
            }
        }
    }

    /// No component picks this pair, and no component can avoid it either:
    /// `apply_colors` wires `ink_muted` into egui's `weak_text_color` and
    /// `surface_subtle` into both of egui's backdrop fills, so egui paints
    /// `TextEdit` hint text and striped rows with exactly that combination.
    /// High Contrast is excluded on purpose -- there both sides come from
    /// Windows, and approximating them would break the harder rule.
    #[test]
    fn the_global_weak_text_meets_body_contrast_on_the_egui_backdrops() {
        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            let tokens = resolve_theme(preference, None, APPEARANCE).tokens;
            let mut style = Style::default();
            apply_colors(&mut style, tokens);
            let weak = style
                .visuals
                .weak_text_color
                .expect("the applied style names a weak text colour");

            for (name, backdrop) in [
                ("extreme_bg_color", style.visuals.extreme_bg_color),
                ("faint_bg_color", style.visuals.faint_bg_color),
            ] {
                let ratio = contrast_ratio(weak, backdrop);
                assert!(
                    ratio >= 4.5,
                    "{preference:?} weak text on {name} was {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn high_contrast_overrides_preferences_and_uses_only_system_colors() {
        let appearance = SystemAppearance {
            client_animation_enabled: true,
            high_contrast: true,
            colors: SystemColors {
                background: [11, 12, 13],
                foreground: [21, 22, 23],
                highlight: [31, 32, 33],
                highlight_text: [41, 42, 43],
                disabled_text: [51, 52, 53],
                link: [61, 62, 63],
            },
        };
        let style = resolve_theme(ThemePreference::Dark, Some(egui::Theme::Light), appearance);
        assert_eq!(style.theme, ResolvedTheme::HighContrast);
        assert_eq!(
            style.tokens,
            ThemeTokens {
                canvas: Color32::from_rgb(11, 12, 13),
                surface: Color32::from_rgb(11, 12, 13),
                surface_subtle: Color32::from_rgb(11, 12, 13),
                ink: Color32::from_rgb(21, 22, 23),
                ink_muted: Color32::from_rgb(21, 22, 23),
                border: Color32::from_rgb(21, 22, 23),
                route: Color32::from_rgb(31, 32, 33),
                on_route: Color32::from_rgb(41, 42, 43),
                route_soft: Color32::from_rgb(11, 12, 13),
                live: Color32::from_rgb(21, 22, 23),
                live_soft: Color32::from_rgb(11, 12, 13),
                warning: Color32::from_rgb(21, 22, 23),
                warning_soft: Color32::from_rgb(11, 12, 13),
                fault: Color32::from_rgb(21, 22, 23),
                fault_soft: Color32::from_rgb(11, 12, 13),
                focus: Color32::from_rgb(31, 32, 33),
                disabled: Color32::from_rgb(51, 52, 53),
                link: Color32::from_rgb(61, 62, 63),
                focus_width: 3.0
            }
        );
    }

    #[test]
    fn theme_precedence_honors_explicit_preference_then_system_then_light_fallback() {
        assert_eq!(
            resolve_theme(ThemePreference::Light, Some(egui::Theme::Dark), APPEARANCE).theme,
            ResolvedTheme::Light
        );
        assert_eq!(
            resolve_theme(ThemePreference::Dark, Some(egui::Theme::Light), APPEARANCE).theme,
            ResolvedTheme::Dark
        );
        assert_eq!(
            resolve_theme(ThemePreference::System, Some(egui::Theme::Dark), APPEARANCE).theme,
            ResolvedTheme::Dark
        );
        assert_eq!(
            resolve_theme(ThemePreference::System, None, APPEARANCE).theme,
            ResolvedTheme::Light
        );
    }

    #[test]
    fn disabled_client_animations_remove_theme_transitions() {
        assert_eq!(
            resolve_theme(
                ThemePreference::Light,
                None,
                SystemAppearance {
                    client_animation_enabled: false,
                    ..APPEARANCE
                }
            )
            .transition_seconds,
            0.0
        );
    }

    #[test]
    fn embedded_fonts_follow_the_system_inter_mono_fallback_order() {
        let fonts = build_fonts(None);
        assert_eq!(
            fonts.families[&FontFamily::Proportional][0],
            "inter_variable"
        );
        assert!(fonts.families[&FontFamily::Proportional].len() > 1);
        assert_eq!(fonts.families[&FontFamily::Monospace][0], "ibm_plex_mono");
        assert!(fonts.families[&FontFamily::Monospace].len() > 1);
    }

    #[test]
    fn supplied_segoe_font_precedes_embedded_inter_without_a_filesystem_read() {
        let fonts = build_fonts(Some(Arc::from([0u8].as_slice())));
        let proportional = &fonts.families[&FontFamily::Proportional];

        assert_eq!(proportional[0], "segoe_ui_variable");
        assert_eq!(proportional[1], "inter_variable");
    }

    #[test]
    fn applied_high_contrast_theme_keeps_every_resolved_semantic_token() {
        let ctx = Context::default();
        let resolved = resolve_theme(
            ThemePreference::System,
            None,
            SystemAppearance {
                client_animation_enabled: false,
                high_contrast: true,
                colors: SystemColors {
                    background: [1, 2, 3],
                    foreground: [4, 5, 6],
                    highlight: [7, 8, 9],
                    highlight_text: [10, 11, 12],
                    disabled_text: [13, 14, 15],
                    link: [16, 17, 18],
                },
            },
        );

        apply_theme(&ctx, &resolved);

        assert_eq!(active_tokens(&ctx), resolved.tokens);
    }

    #[test]
    fn every_typography_role_exposes_its_approved_size_and_line_height() {
        let approved = [
            (TypographyRole::PageTitle, 28.0, 34.0),
            (TypographyRole::HeroTitle, 20.0, 26.0),
            (TypographyRole::SectionTitle, 16.0, 22.0),
            (TypographyRole::Body, 14.0, 20.0),
            (TypographyRole::Button, 14.0, 18.0),
            (TypographyRole::Secondary, 12.0, 17.0),
            (TypographyRole::Eyebrow, 11.0, 15.0),
            (TypographyRole::Measurement, 12.0, 16.0),
        ];

        let mut style = Style::default();
        apply_text_styles(&mut style);
        for (role, size, line_height) in approved {
            assert_eq!(role.font_size(), size, "{role:?} size");
            assert_eq!(role.line_height(), line_height, "{role:?} line height");
            assert_eq!(role.text_style().resolve(&style), role.font_id());
            let mut job = LayoutJob::default();
            role.rich_text("Metric").append_to(
                &mut job,
                &style,
                FontSelection::Default,
                Align::Center,
            );
            assert_eq!(job.sections[0].format.line_height, Some(line_height));
        }
    }
}
