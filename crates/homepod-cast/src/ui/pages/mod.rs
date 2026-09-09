//! The six destinations and the exhaustive dispatcher that reaches them.
//!
//! Every renderer consumes only the supplied [`UiSnapshot`] and the shared
//! [`UiResources`]; none of them touch disk, time, services, or an
//! [`crate::app_handle::AppHandle`]. There is no handle to pass, so there is
//! no path by which a device, a socket, a session, or a backend diagnostic
//! object could reach a page.
//!
//! [`show`] matches every [`Page`] variant by name and has no wildcard arm. A
//! seventh destination is therefore a compiler error here and in
//! [`destination_index`] and [`title_key`], rather than a page that silently
//! renders nothing.

pub mod audio;
pub mod diagnostics;
pub mod groups;
pub mod hardware_check;
pub mod live_diagnostics;
pub mod overview;
pub mod settings;
pub mod speakers;

use crate::app::{AppEvent, Page, StreamSnapshot, UiSnapshot};
use crate::ui::i18n::TextKey;
use crate::ui::layout::UiResources;
use crate::ui::presentation::{CARD_PADDING, CARD_RADIUS};
use crate::ui::theme::TypographyRole;

/// The six destinations, in rail order.
pub const DESTINATIONS: [Page; 6] = [
    Page::Home,
    Page::Speakers,
    Page::Groups,
    Page::Audio,
    Page::Diagnostics,
    Page::Settings,
];

/// Where a destination sits in the rail.
///
/// Exhaustive by construction: a new [`Page`] variant fails to compile here
/// until it has been given a position, so it cannot appear as an unreachable
/// destination.
pub const fn destination_index(page: Page) -> usize {
    match page {
        Page::Home => 0,
        Page::Speakers => 1,
        Page::Groups => 2,
        Page::Audio => 3,
        Page::Diagnostics => 4,
        Page::Settings => 5,
    }
}

/// The localized title of a destination.
///
/// `Page::Home` keeps its internal variant name and its persisted value; only
/// the visible label is "Overview" / "Übersicht".
pub const fn title_key(page: Page) -> TextKey {
    match page {
        Page::Home => TextKey::Overview,
        Page::Speakers => TextKey::Speakers,
        Page::Groups => TextKey::Groups,
        Page::Audio => TextKey::Audio,
        Page::Diagnostics => TextKey::Diagnostics,
        Page::Settings => TextKey::Settings,
    }
}

/// The one word that names the application's real state right now.
///
/// A notice outranks the stream phase: something needs the user, and saying
/// "Ready" over it would be the shell's own lie.
pub fn status_key(snapshot: &UiSnapshot) -> TextKey {
    if snapshot.notice.is_some() {
        return TextKey::NeedsAttention;
    }
    match snapshot.stream {
        StreamSnapshot::Stopped => TextKey::Ready,
        StreamSnapshot::Starting { .. } => TextKey::Connecting,
        StreamSnapshot::Streaming { .. } => TextKey::Streaming,
        StreamSnapshot::Degraded { .. } => TextKey::Degraded,
        StreamSnapshot::Restarting { .. } => TextKey::Reconnecting,
        StreamSnapshot::Stopping { .. } => TextKey::Stopping,
        StreamSnapshot::Failed { .. } => TextKey::NeedsAttention,
    }
}

/// Whether the destination's own body already prints the phase word.
///
/// Only the Command Home does: paragraph 7.1 puts the phase in the Status
/// hero, directly under the page title, which outranks anything the
/// navigation rail could say. The rail's status footer prints the word on
/// exactly the destinations that do not, so the window carries the phase once
/// and never zero times -- the two failures this table exists to keep apart.
///
/// It is a claim, not a measurement, so it is checked against the real paint
/// output for every destination by
/// `the_expanded_rail_prints_the_phase_exactly_where_the_page_does_not`. The
/// match is exhaustive on purpose: a new destination has to answer for itself
/// before the crate compiles.
pub const fn prints_status_word(page: Page) -> bool {
    match page {
        Page::Home => true,
        Page::Speakers | Page::Groups | Page::Audio | Page::Diagnostics | Page::Settings => false,
    }
}

/// Renders the destination for `page`.
///
/// The match is exhaustive and has no wildcard arm on purpose: a new
/// destination has to be given a renderer before the crate compiles.
pub fn show(
    page: Page,
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    match page {
        Page::Home => overview::show(ui, snapshot, resources, emit),
        Page::Speakers => speakers::show(ui, snapshot, resources, emit),
        Page::Groups => groups::show(ui, snapshot, resources, emit),
        Page::Audio => audio::show(ui, snapshot, resources, emit),
        Page::Diagnostics => diagnostics::show(ui, snapshot, resources, emit),
        Page::Settings => settings::show(ui, snapshot, resources, emit),
    }
}

/// One settings group's card.
///
/// Section 5.3's surface, radius, and padding, taken from the shared
/// constants. Lifted out of `settings.rs` when the Audio destination grew
/// groups of its own: two cards drawn from two copies of the same four lines
/// would be one ragged column away from disagreeing.
///
/// The width is claimed explicitly. `egui::Frame` sizes itself from its
/// content's `min_rect`, so a card holding one short row would otherwise end
/// wherever that row ended.
pub(crate) fn group_card<R>(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let inner_width = (ui.available_width() - 2.0 * CARD_PADDING).max(1.0);
    egui::Frame::NONE
        .fill(resources.tokens.surface)
        .corner_radius(egui::CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(egui::Margin::same(CARD_PADDING as i8))
        .show(ui, |card| {
            card.set_min_width(inner_width);
            body(card)
        })
        .inner
}

/// The title and explanation a settings group opens with.
pub(crate) fn group_heading(
    ui: &mut egui::Ui,
    resources: &UiResources<'_>,
    title: &str,
    body: &str,
) {
    show_text(
        ui,
        TypographyRole::SectionTitle,
        resources.tokens.ink,
        title,
    );
    show_text(ui, TypographyRole::Body, resources.tokens.ink_muted, body);
}

/// One localized text node.
///
/// Shared with the components so a page and a component cannot disagree about
/// how a sentence reaches the accessibility tree.
pub(crate) fn show_text(
    ui: &mut egui::Ui,
    role: TypographyRole,
    color: egui::Color32,
    text: &str,
) -> egui::Response {
    crate::ui::components::show_text(ui, role, color, text)
}

#[cfg(test)]
mod destination_tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::app::{
        AppState, Availability, GenerationId, HotkeyBinding, ReceiverState, ResolvedLocale,
        StreamState, ThemePreference,
    };
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::i18n::{Catalog, NamedValueArgs, TextKey};
    use crate::ui::layout::UiResources;
    use crate::ui::theme;

    const TEST_APPEARANCE: SystemAppearance = SystemAppearance {
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

    /// The real Windows "Contrast Black" theme. Invented colours here would
    /// make every contrast claim about High Contrast self-fulfilling.
    const HIGH_CONTRAST_BLACK: SystemAppearance = SystemAppearance {
        client_animation_enabled: false,
        high_contrast: true,
        colors: SystemColors {
            background: [0x00, 0x00, 0x00],
            foreground: [0xff, 0xff, 0xff],
            highlight: [0x1a, 0xeb, 0xff],
            highlight_text: [0x00, 0x00, 0x00],
            disabled_text: [0x3f, 0xf2, 0x3f],
            link: [0xff, 0xff, 0x00],
        },
    };

    const LOCALES: [ResolvedLocale; 2] = [ResolvedLocale::German, ResolvedLocale::English];
    const TARGET_SIZES: [[f32; 2]; 2] = [[1120.0, 720.0], [900.0, 600.0]];

    /// A page canvas tall enough to expose a whole destination at once.
    ///
    /// The App Shell owns the only scroll region of the client area, and a
    /// page harness has no shell around it. A control that sits below a
    /// 720-point viewport here is therefore neither painted nor clickable --
    /// which in the running window it is, after the shell scrolls to it. So
    /// any test that clicks a control far down a page, or measures what that
    /// control paints, uses this canvas; the geometry and order tests keep
    /// the real target sizes.
    const TALL_CANVAS: [f32; 2] = [1120.0, 2000.0];

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn receiver(last: u8, name: &str, availability: Availability) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "HomePod".into(),
            availability,
        }
    }

    /// Receiver names are product names on purpose: a language guard must not
    /// trip over user data that legitimately reads the same in both locales.
    fn populated_state() -> AppState {
        AppState {
            receivers: vec![
                receiver(1, "HomePod", Availability::Available),
                receiver(2, "HomePod mini", Availability::Unavailable),
            ],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            ..AppState::default()
        }
    }

    #[test]
    fn hardware_check_entry_keyboard_and_report_are_explicit_and_private() {
        use crate::app::hardware_check::HardwareCheckAction as A;
        for locale in LOCALES {
            let catalog = Catalog::new(locale);
            let mut state = populated_state();
            state.receivers[0].name = "PRIVATE-NAME-10.0.0.1".into();
            let mut audio = page_harness(Page::Audio, locale, TALL_CANVAS, state.clone());
            assert!(audio.state().is_empty());
            audio
                .get_by_label(catalog.text(TextKey::HardwareCheck))
                .focus();
            audio.step();
            audio.key_press(egui::Key::Enter);
            audio.step();
            assert_eq!(audio.state().as_slice(), [AppEvent::HardwareCheck(A::Open)]);
            let mut diagnostic = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
            assert!(diagnostic.output().platform_output.commands.is_empty());
            diagnostic
                .get_by_label(catalog.text(TextKey::HardwareReport))
                .click();
            diagnostic.step();
            let copied: Vec<_> = diagnostic
                .output()
                .platform_output
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(text) => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(copied.len(), 1);
            assert!(copied[0].contains(catalog.text(TextKey::HardwareSnapshot)));
            assert!(!copied[0].contains("PRIVATE"));
            assert!(!copied[0].contains("10.0.0.1"));
            assert!(diagnostic.state().is_empty());
        }
    }

    #[test]
    fn hardware_check_start_click_and_keyboard_emit_one_explicit_request() {
        use crate::app::hardware_check::HardwareCheckAction as A;
        for locale in LOCALES {
            for keyboard in [false, true] {
                let state = crate::app::hardware_check::tests::prepared();
                let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                assert!(harness.state().is_empty());
                let label = Catalog::new(locale).text(TextKey::HardwareStart);
                if keyboard {
                    harness.get_by_label(label).focus();
                    harness.step();
                    harness.key_press(egui::Key::Enter);
                } else {
                    harness.get_by_label(label).click();
                }
                harness.step();
                assert_eq!(
                    harness.state().as_slice(),
                    [AppEvent::HardwareCheck(A::Start)]
                );
            }
        }
    }

    #[test]
    fn hardware_check_navigation_and_help_are_the_first_row() {
        for locale in LOCALES {
            let catalog = Catalog::new(locale);
            let state = crate::app::hardware_check::tests::prepared();
            let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
            harness.step();

            let back = harness
                .get_by_label(catalog.text(TextKey::HardwareBack))
                .rect();
            let help = harness
                .get_by_label(catalog.text(TextKey::HardwareHelp))
                .rect();
            let title = harness
                .get_by_label(catalog.text(TextKey::HardwareTitle))
                .rect();
            assert!(
                (back.center().y - help.center().y).abs() < 1.0,
                "{locale:?}: Back and Help are not one top row"
            );
            assert!(
                back.bottom().max(help.bottom()) <= title.top(),
                "{locale:?}: content appears above the top controls"
            );

            harness
                .get_by_label(catalog.text(TextKey::HardwareHelp))
                .click();
            harness.run();
            let row_bottom = harness
                .get_by_label(catalog.text(TextKey::HardwareBack))
                .rect()
                .bottom()
                .max(
                    harness
                        .get_by_label(catalog.text(TextKey::HardwareHideHelp))
                        .rect()
                        .bottom(),
                );
            let detail = harness
                .get_by_label(catalog.text(TextKey::HardwareRequirements))
                .rect();
            let title = harness
                .get_by_label(catalog.text(TextKey::HardwareTitle))
                .rect();
            assert!(
                row_bottom <= detail.top(),
                "{locale:?}: Help is not below its toggle"
            );
            assert!(
                detail.bottom() <= title.top(),
                "{locale:?}: Help is not directly before page content"
            );
            assert!(harness.state().is_empty());
        }
    }

    #[test]
    fn active_hardware_check_keeps_cancel_in_the_top_row_and_dispatches_once() {
        use crate::app::hardware_check::{HardwareCheckAction as A, HardwareCheckPhase};

        for locale in LOCALES {
            let catalog = Catalog::new(locale);
            let mut state = crate::app::hardware_check::tests::prepared();
            state.hardware_check.phase = HardwareCheckPhase::Listening;
            let mut harness = page_harness(Page::Diagnostics, locale, [900.0, 600.0], state);
            harness.step();

            let back = harness
                .get_by_label(catalog.text(TextKey::HardwareBack))
                .rect();
            let help = harness
                .get_by_label(catalog.text(TextKey::HardwareHelp))
                .rect();
            let cancel = harness.get_by_label(catalog.text(TextKey::Cancel)).rect();
            assert!(
                (cancel.center().y - back.center().y).abs() < 1.0
                    && (cancel.center().y - help.center().y).abs() < 1.0,
                "{locale:?}: Cancel is not in the first row"
            );
            assert!(
                cancel.bottom() <= 600.0,
                "{locale:?}: Cancel is below the fold"
            );

            harness.get_by_label(catalog.text(TextKey::Cancel)).click();
            harness.step();
            assert_eq!(
                harness.state().as_slice(),
                [AppEvent::HardwareCheck(A::Cancel)]
            );
        }
    }

    #[test]
    fn prepared_hardware_check_keeps_start_visible_at_the_wide_target_height() {
        for locale in LOCALES {
            let catalog = Catalog::new(locale);
            let state = crate::app::hardware_check::tests::prepared();
            let mut harness = page_harness(Page::Diagnostics, locale, [1120.0, 720.0], state);
            harness.step();
            let start = harness
                .get_by_label(catalog.text(TextKey::HardwareStart))
                .rect();
            assert!(
                start.bottom() <= 720.0,
                "{locale:?}: Start is below the wide viewport: {start:?}"
            );
            assert!(harness.state().is_empty());
        }
    }

    #[test]
    fn diagnostics_shows_app_state_and_capture_source_while_idle() {
        use crate::app::{
            AudioEndpointKey, AudioEndpointSelection, AudioSourceReading, CaptureState,
        };

        for locale in LOCALES {
            let catalog = Catalog::new(locale);
            let mut state = populated_state();
            state.audio_source = Some(AudioSourceReading {
                endpoints_known: true,
                refresh_failed: false,
                endpoints: Vec::new(),
                selection: AudioEndpointSelection::SystemDefault,
                captured_key: Some(AudioEndpointKey(7)),
                captured_name: Some("Speakers".into()),
                state: CaptureState::Capturing,
            });
            let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
            harness.step();

            let status = catalog.named_value(NamedValueArgs {
                name: catalog.text(TextKey::Status),
                value: catalog.text(TextKey::Ready),
            });
            let source = catalog.named_value(NamedValueArgs {
                name: catalog.text(TextKey::DiagnosticsAudioSource),
                value: "Speakers",
            });
            let capture = catalog.named_value(NamedValueArgs {
                name: catalog.text(TextKey::AudioCaptureStatus),
                value: catalog.text(TextKey::CaptureCapturing),
            });
            let wizard_top = harness
                .get_by_label(catalog.text(TextKey::HardwareTitle))
                .rect()
                .top();
            for fact in [&status, &source, &capture] {
                let fact_rect = harness.get_by_label_contains(fact).rect();
                assert!(
                    fact_rect.bottom() <= wizard_top,
                    "{locale:?}: diagnostic fact appears below the listening wizard: {fact}"
                );
            }
            assert!(harness.state().is_empty());
        }
    }

    #[test]
    fn hardware_check_back_and_optional_help_do_not_start_or_stop_audio() {
        for (locale, back, help, hide) in [
            (
                ResolvedLocale::German,
                "Zurück zu Audio",
                "Hilfe anzeigen",
                "Hilfe ausblenden",
            ),
            (
                ResolvedLocale::English,
                "Back to Audio",
                "Show help",
                "Hide help",
            ),
        ] {
            let state = crate::app::hardware_check::tests::prepared();
            let mut h = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
            let detail = Catalog::new(locale).text(TextKey::HardwareRequirements);
            assert!(h.query_by_label(detail).is_none());
            h.get_by_label(help).click();
            h.run();
            h.get_by_label(detail);
            assert!(h.state().is_empty());
            h.get_by_label(hide).click();
            h.run();
            assert!(h.query_by_label(detail).is_none());
            h.get_by_label(back).focus();
            h.step();
            h.key_press(egui::Key::Enter);
            h.step();
            assert_eq!(h.state().as_slice(), [AppEvent::Navigate(Page::Audio)]);
        }
    }

    #[test]
    fn hardware_check_running_session_explains_block_and_only_explicit_stop_is_sent() {
        let mut state = crate::app::hardware_check::tests::prepared();
        state.stream = StreamState::Streaming {
            generation: GenerationId(8),
        };
        let mut h = page_harness(
            Page::Diagnostics,
            ResolvedLocale::German,
            TALL_CANVAS,
            state,
        );
        h.get_by_label(
            "Streaming läuft bereits. Stoppe es zuerst; danach kannst du den Hörtest starten.",
        );
        h.get_by_label(
            "Der leise Testton endet nach etwa 2 Sekunden. Danach läuft Windows-Audio weiter; die Sitzung bleibt bis Gehört, Nicht gehört oder Abbrechen geöffnet.",
        );
        assert!(h.state().is_empty());
        h.get_by_label(Catalog::new(ResolvedLocale::German).text(TextKey::HardwareStart))
            .click();
        h.step();
        assert!(h.state().is_empty());
        h.get_by_label(Catalog::new(ResolvedLocale::German).text(TextKey::StopStreaming))
            .click();
        h.step();
        assert_eq!(h.state().as_slice(), [AppEvent::StopRequested]);
    }

    type PageHarness = egui_kittest::Harness<'static, Vec<AppEvent>>;

    fn page_harness(
        page: Page,
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
    ) -> PageHarness {
        themed_page_harness(page, locale, size, state, ThemePreference::Light)
    }

    /// The same harness with the theme left open.
    ///
    /// Task 8 replaced the deleted `pages.rs` smoke test, which drew every
    /// destination in Light *and* Dark, with one that varies the locale
    /// instead -- so roughly 1300 lines of new drawing code had never been
    /// executed against the Dark tokens at all.
    fn themed_page_harness(
        page: Page,
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
        theme: ThemePreference,
    ) -> PageHarness {
        styled_page_harness(page, locale, size, state, theme, TEST_APPEARANCE)
    }

    /// The same harness with the Windows appearance left open as well, so a
    /// page can be drawn under a real High Contrast theme.
    fn styled_page_harness(
        page: Page,
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
        theme: ThemePreference,
        appearance: SystemAppearance,
    ) -> PageHarness {
        let mut harness: PageHarness = egui_kittest::Harness::new_ui_state(
            move |ui, log: &mut Vec<AppEvent>| {
                let resolved = theme::resolve_theme(theme, None, appearance);
                theme::apply_theme(ui.ctx(), &resolved);
                let resources = UiResources {
                    tokens: &resolved.tokens,
                    catalog: Catalog::new(locale),
                };
                let snapshot = UiSnapshot::from_state(&state);
                let mut emit = |event: AppEvent| log.push(event);
                show(page, ui, &snapshot, &resources, &mut emit);
            },
            Vec::new(),
        );
        harness.set_size(egui::vec2(size[0], size[1]));
        harness.step();
        harness
    }

    mod dispatcher {
        use super::*;

        #[test]
        fn destinations_list_the_six_pages_once_in_rail_order() {
            assert_eq!(DESTINATIONS.len(), 6);
            let mut seen: Vec<usize> = DESTINATIONS.iter().map(|&p| destination_index(p)).collect();
            for (index, page) in DESTINATIONS.iter().enumerate() {
                assert_eq!(destination_index(*page), index, "{page:?}");
            }
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), DESTINATIONS.len(), "a destination repeats");
        }

        #[test]
        fn overview_keeps_the_home_variant_but_carries_a_localized_title() {
            assert_eq!(DESTINATIONS[0], Page::Home);
            assert_eq!(title_key(Page::Home), TextKey::Overview);
            assert_eq!(
                Catalog::new(ResolvedLocale::German).text(title_key(Page::Home)),
                "\u{dc}bersicht"
            );
            assert_eq!(
                Catalog::new(ResolvedLocale::English).text(title_key(Page::Home)),
                "Overview"
            );
        }

        #[test]
        fn every_destination_has_a_distinct_title_in_both_locales() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let titles: HashSet<&str> = DESTINATIONS
                    .iter()
                    .map(|&page| catalog.text(title_key(page)))
                    .collect();
                assert_eq!(titles.len(), DESTINATIONS.len(), "{locale:?}");
            }
        }
    }

    mod honesty {
        // Deliberately no `use super::*`: these guards read the page sources
        // as text and must not depend on a single item of the module they
        // police.

        /// Every page renderer, paired with its source.
        const PAGE_SOURCES: &[(&str, &str)] = &[
            ("overview.rs", include_str!("overview.rs")),
            ("speakers.rs", include_str!("speakers.rs")),
            ("groups.rs", include_str!("groups.rs")),
            ("audio.rs", include_str!("audio.rs")),
            ("diagnostics.rs", include_str!("diagnostics.rs")),
            ("settings.rs", include_str!("settings.rs")),
        ];

        fn renderer_body(source: &str) -> &str {
            source
                .split("#[cfg(test)]")
                .next()
                .expect("the renderer precedes its tests")
        }

        /// The Change Bar is Shell-owned. A page that placed one would put a
        /// second Apply in the tab order and a second owner in charge of the
        /// page geometry.
        #[test]
        fn no_page_renderer_places_a_change_bar() {
            for (name, source) in PAGE_SOURCES {
                assert!(
                    !renderer_body(source).contains(concat!("change_", "bar")),
                    "{name} places a change bar"
                );
            }
        }

        /// The single filled action per context is arbitrated once, by
        /// `presentation::filled_action_owner`. A page naming the filled
        /// emphasis itself could put a second filled button beside the
        /// Shell's.
        #[test]
        fn no_page_renderer_names_the_filled_emphasis_itself() {
            for (name, source) in PAGE_SOURCES {
                assert!(
                    !renderer_body(source).contains(concat!("Emphasis::", "Filled")),
                    "{name} decides its own filled action"
                );
            }
        }

        /// Both guards must be able to fail, or they prove nothing.
        #[test]
        fn the_honesty_guards_flag_a_planted_violation() {
            let planted = format!("{}::show(ui);", concat!("change_", "bar"));
            assert!(planted.contains(concat!("change_", "bar")));
            let planted = format!("let e = {};", concat!("Emphasis::", "Filled"));
            assert!(planted.contains(concat!("Emphasis::", "Filled")));
        }

        /// Every page that can draw an empty state, paired with its source
        /// and with the catalog keys that empty state is built from.
        ///
        /// Overview is absent because its empty state is
        /// `EmptyStateModel::no_receivers`, which the Command Home reaches
        /// only after reading `snapshot.receivers`.
        const PAGE_EMPTY_COPY: &[(&str, &str, &[crate::ui::i18n::TextKey])] = &[
            (
                "speakers.rs",
                include_str!("speakers.rs"),
                &[
                    crate::ui::i18n::TextKey::SpeakersEmptyTitle,
                    crate::ui::i18n::TextKey::SpeakersEmptyBody,
                ],
            ),
            (
                "groups.rs",
                include_str!("groups.rs"),
                &[
                    crate::ui::i18n::TextKey::GroupsEmptyTitle,
                    crate::ui::i18n::TextKey::GroupsEmptyBody,
                ],
            ),
            (
                "audio.rs",
                include_str!("audio.rs"),
                &[
                    crate::ui::i18n::TextKey::AudioEmptyTitle,
                    crate::ui::i18n::TextKey::AudioEmptyBody,
                ],
            ),
            (
                "diagnostics.rs",
                include_str!("diagnostics.rs"),
                &[
                    crate::ui::i18n::TextKey::DiagnosticsEmptyTitle,
                    crate::ui::i18n::TextKey::DiagnosticsEmptyBody,
                ],
            ),
        ];

        /// Stems that turn a sentence into a claim about the here and now:
        /// something was asked, answered, or counted.
        ///
        /// Stems rather than whole words, so that a declension cannot slip
        /// one past the guard.
        const MEASUREMENT_CLAIMS: &[&str] = &[
            // German
            "meldet",
            "gefunden",
            "erkannt",
            "verf\u{fc}gbar",
            "derzeit",
            "zurzeit",
            "aktuell",
            "gemessen",
            // English
            "reports",
            "found",
            "detected",
            "available",
            "currently",
            "right now",
        ];

        /// Whether a page can know anything about the running system.
        ///
        /// The underscore binding is the compiler's own proof: a renderer
        /// that declares `_snapshot` cannot read one field of it without the
        /// name changing here first.
        fn reads_its_snapshot(source: &str) -> bool {
            !renderer_body(source).contains(concat!("_snapshot", ": &UiSnapshot"))
        }

        fn measurement_claims(text: &str) -> Vec<&'static str> {
            let lowered = text.to_lowercase();
            MEASUREMENT_CLAIMS
                .iter()
                .copied()
                .filter(|stem| lowered.contains(stem))
                .collect()
        }

        /// A page that never reads its snapshot has measured nothing, so
        /// nothing it shows may be phrased as a measurement.
        ///
        /// This is the class, not the sentence: the Audio page told every
        /// user that "WASAPI capture currently reports no selectable
        /// devices" while the capture worker was running and named in the
        /// same session's log -- and the page had never asked. A missing
        /// projection is a fact about the build and has to be said as one.
        #[test]
        fn a_page_that_never_reads_its_snapshot_claims_no_measurement() {
            use crate::app::ResolvedLocale;
            use crate::ui::i18n::Catalog;

            for (name, source, keys) in PAGE_EMPTY_COPY {
                if reads_its_snapshot(source) {
                    continue;
                }
                for locale in [ResolvedLocale::German, ResolvedLocale::English] {
                    let catalog = Catalog::new(locale);
                    for &key in *keys {
                        let text = catalog.text(key);
                        let claims = measurement_claims(text);
                        assert!(
                            claims.is_empty(),
                            "{name} never reads its snapshot, but {key:?} claims a \
                             measurement in {locale:?} via {claims:?}: {text}"
                        );
                    }
                }
            }
        }

        /// Exercise both detector branches without requiring a product page
        /// to remain a placeholder after its implementation is complete.
        #[test]
        fn the_measurement_guard_sees_both_kinds_of_page_and_can_fail() {
            let reading: Vec<&str> = PAGE_EMPTY_COPY
                .iter()
                .filter(|(_, source, _)| reads_its_snapshot(source))
                .map(|(name, _, _)| *name)
                .collect();
            assert!(
                !reading.is_empty(),
                "no page reads its snapshot, so the exemption is untested"
            );
            assert!(!reads_its_snapshot(
                "pub fn show(_snapshot: &UiSnapshot) {}"
            ));

            assert_eq!(
                measurement_claims("Die Aufnahme meldet derzeit keine Ger\u{e4}te."),
                vec!["meldet", "derzeit"]
            );
            assert_eq!(
                measurement_claims("Capture currently reports no devices."),
                vec!["reports", "currently"]
            );
            assert!(measurement_claims("Diese Fassung bietet das noch nicht an.").is_empty());
        }
    }

    mod rendering {
        use super::*;

        #[test]
        fn every_destination_renders_without_navigating_at_both_target_sizes() {
            for &page in DESTINATIONS.iter() {
                for locale in LOCALES {
                    for size in TARGET_SIZES {
                        let mut harness = page_harness(page, locale, size, populated_state());
                        harness.step();
                        assert!(
                            !harness
                                .state()
                                .iter()
                                .any(|event| matches!(event, AppEvent::Navigate(_))),
                            "{page:?} navigated by itself"
                        );
                    }
                }
            }
        }

        /// The smoke test the deleted `pages.rs` carried: every destination,
        /// every theme, every target size, drawn once for real.
        ///
        /// It asserts nothing beyond "the frame completed and the title is
        /// there" on purpose -- its whole job is to execute the drawing code
        /// in both palettes, so a token that resolves only in one of them
        /// fails here instead of on a user's screen.
        #[test]
        fn every_destination_renders_in_both_themes_at_both_target_sizes() {
            for &page in DESTINATIONS.iter() {
                for theme in [ThemePreference::Light, ThemePreference::Dark] {
                    for size in TARGET_SIZES {
                        let mut harness = themed_page_harness(
                            page,
                            ResolvedLocale::German,
                            size,
                            populated_state(),
                            theme,
                        );
                        harness.step();
                        // The page body carries no title -- the shell header
                        // owns it -- so the frame completing and staying
                        // silent is the whole assertion.
                        assert!(
                            harness.state().is_empty(),
                            "{page:?} dispatched by itself at {size:?} in {theme:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn groups_waits_honestly_for_its_first_backend_reading() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness =
                    page_harness(Page::Groups, locale, TARGET_SIZES[0], populated_state());
                harness.step();
                let _ = harness.get_by_label_contains(catalog.text(TextKey::GroupsLoading));
                assert!(harness.state().is_empty());
            }
        }

        /// Nothing has reported, so the page offers nothing and says so.
        #[test]
        fn audio_shows_an_honest_localized_empty_state_without_controls() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness =
                    page_harness(Page::Audio, locale, TARGET_SIZES[0], populated_state());
                harness.step();
                let _ = harness.get_by_label_contains(catalog.text(TextKey::AudioEmptyTitle));
                assert!(harness.state().is_empty());
            }
        }

        /// The three audio capabilities as the window draws and dispatches
        /// them.
        mod audio_controls {
            use super::*;
            use crate::app::{
                AudioEndpointChoice, AudioEndpointKey, AudioEndpointRequest,
                AudioEndpointSelection, AudioSourceReading, CaptureState, LatencyChoice,
                LatencyOption, LatencyReading, LatencyUnavailable,
            };
            use crate::ui::i18n::NamedValueArgs;

            fn latency_reading() -> LatencyReading {
                LatencyReading {
                    selected: LatencyChoice::Normal,
                    options: vec![
                        LatencyOption {
                            choice: LatencyChoice::Low,
                            unavailable: Some(LatencyUnavailable::NotValidated),
                        },
                        LatencyOption {
                            choice: LatencyChoice::Normal,
                            unavailable: None,
                        },
                        LatencyOption {
                            choice: LatencyChoice::Stable,
                            unavailable: Some(LatencyUnavailable::NotValidated),
                        },
                    ],
                }
            }

            fn capture_reading() -> AudioSourceReading {
                AudioSourceReading {
                    refresh_failed: false,
                    endpoints_known: true,
                    endpoints: vec![
                        AudioEndpointChoice {
                            key: AudioEndpointKey(0),
                            name: "Arctis 7 Chat".into(),
                        },
                        AudioEndpointChoice {
                            key: AudioEndpointKey(1),
                            name: "Realtek HDMI".into(),
                        },
                    ],
                    selection: AudioEndpointSelection::SystemDefault,
                    captured_key: None,
                    captured_name: Some("Realtek HDMI".into()),
                    state: CaptureState::Capturing,
                }
            }

            fn named(catalog: Catalog, group: TextKey, value: &str) -> String {
                catalog.named_value(NamedValueArgs {
                    name: catalog.text(group),
                    value,
                })
            }

            /// A capability the backend has not reported draws nothing at all.
            ///
            /// Not a switch resting on `false`, not a greyed-out group: the
            /// window has taken no reading, and the only honest thing it can
            /// do with a capability it knows nothing about is leave it out.
            #[test]
            fn only_the_reported_capabilities_are_drawn() {
                let catalog = Catalog::new(ResolvedLocale::English);
                let mut state = populated_state();
                state.muted = Some(false);

                let mut harness =
                    page_harness(Page::Audio, ResolvedLocale::English, TALL_CANVAS, state);
                harness.step();
                let announced =
                    crate::ui::components::test_support::accessible_strings(harness.root());

                assert!(
                    announced
                        .iter()
                        .any(|text| text == catalog.text(TextKey::AudioMute)),
                    "the reported capability is missing: {announced:?}"
                );
                for absent in [TextKey::AudioLatency, TextKey::AudioCaptureSource] {
                    assert!(
                        !announced
                            .iter()
                            .any(|text| text.contains(catalog.text(absent))),
                        "{absent:?} was drawn without a reading: {announced:?}"
                    );
                }
            }

            /// The switch dispatches the opposite of the applied state, and
            /// the command reaches the effect executor as a request.
            #[test]
            fn the_mute_switch_dispatches_the_opposite_of_the_applied_state() {
                for (applied, wanted) in [(false, true), (true, false)] {
                    let catalog = Catalog::new(ResolvedLocale::English);
                    let mut state = populated_state();
                    state.muted = Some(applied);

                    let mut harness =
                        page_harness(Page::Audio, ResolvedLocale::English, TALL_CANVAS, state);
                    harness.step();
                    // By role as well as by name: the group heading and the
                    // switch legitimately carry the same word, exactly as
                    // Advanced information does in Settings.
                    harness
                        .get_by_role_and_label(
                            accesskit::Role::CheckBox,
                            catalog.text(TextKey::AudioMute),
                        )
                        .click();
                    harness.step();

                    assert_eq!(
                        harness.state().as_slice(),
                        [AppEvent::MuteChangeRequested(wanted)],
                        "applied {applied} should offer {wanted}"
                    );
                }
            }

            /// A gated profile is stated and not offered.
            ///
            /// Both halves matter. It stays on the page with its reason, so a
            /// missing feature cannot be mistaken for a broken one; and it is
            /// not a control, so nothing in the tab order announces itself as
            /// a choice and then answers to nothing.
            #[test]
            fn a_gated_latency_profile_is_stated_but_never_offered() {
                for locale in LOCALES {
                    let catalog = Catalog::new(locale);
                    let mut state = populated_state();
                    state.latency = Some(latency_reading());

                    let mut harness = page_harness(Page::Audio, locale, TALL_CANVAS, state);
                    harness.step();
                    let announced =
                        crate::ui::components::test_support::accessible_strings(harness.root());

                    let reason = catalog.text(TextKey::LatencyNotValidated);
                    for gated in [TextKey::LatencyLow, TextKey::LatencyStable] {
                        let line = catalog.named_value(NamedValueArgs {
                            name: catalog.text(gated),
                            value: reason,
                        });
                        assert!(
                            announced.iter().any(|text| text == &line),
                            "{locale:?}: the gated profile never states its reason: \
                             {announced:?}"
                        );
                    }

                    let offered: Vec<accesskit::Role> = harness
                        .root()
                        .children_recursive()
                        .filter(|node| {
                            node.accesskit_node().label().is_some_and(|label| {
                                label.contains(catalog.text(TextKey::LatencyLow))
                            })
                        })
                        .map(|node| node.accesskit_node().role())
                        .collect();
                    assert!(
                        !offered.contains(&accesskit::Role::RadioButton),
                        "{locale:?}: a gated profile is drawn as a choice: {offered:?}"
                    );
                }
            }

            #[test]
            fn the_selectable_latency_profile_dispatches_its_own_choice() {
                let catalog = Catalog::new(ResolvedLocale::English);
                let mut state = populated_state();
                state.latency = Some(LatencyReading {
                    selected: LatencyChoice::Stable,
                    ..latency_reading()
                });

                let mut harness =
                    page_harness(Page::Audio, ResolvedLocale::English, TALL_CANVAS, state);
                harness.step();
                harness
                    .get_by_label_contains(&named(
                        catalog,
                        TextKey::AudioLatency,
                        catalog.text(TextKey::LatencyNormal),
                    ))
                    .click();
                harness.step();

                assert_eq!(
                    harness.state().as_slice(),
                    [AppEvent::LatencyChoiceRequested(LatencyChoice::Normal)]
                );
            }

            /// Each capture row dispatches the key of the endpoint it names,
            /// and the Windows default row dispatches the default.
            #[test]
            fn every_capture_row_dispatches_its_own_endpoint() {
                let catalog = Catalog::new(ResolvedLocale::English);
                for (label, expected) in [
                    (
                        catalog.text(TextKey::AudioCaptureSystemDefault).to_owned(),
                        AudioEndpointRequest::SystemDefault,
                    ),
                    (
                        "Arctis 7 Chat".to_owned(),
                        AudioEndpointRequest::Endpoint(AudioEndpointKey(0)),
                    ),
                    (
                        "Realtek HDMI".to_owned(),
                        AudioEndpointRequest::Endpoint(AudioEndpointKey(1)),
                    ),
                ] {
                    let mut state = populated_state();
                    state.audio_source = Some(capture_reading());

                    let mut harness =
                        page_harness(Page::Audio, ResolvedLocale::English, TALL_CANVAS, state);
                    harness.step();
                    harness
                        .get_by_label_contains(&named(catalog, TextKey::AudioCaptureSource, &label))
                        .click();
                    harness.step();

                    assert_eq!(
                        harness.state().as_slice(),
                        [AppEvent::AudioEndpointRequested(expected)],
                        "the row for {label:?} dispatched the wrong endpoint"
                    );
                }
            }

            /// A chooser with nothing to choose is not drawn as a chooser.
            ///
            /// A successful empty scan reduces the group to the one
            /// Windows-default row -- already selected.
            /// Drawn as a radio it is a control that announces a choice,
            /// takes the caret, accepts the click, dispatches an event, and
            /// cannot change a thing: the exact dead control this project
            /// treats as its worst defect, and one that
            /// `no_reachable_control_is_inert` cannot see, because that guard
            /// is satisfied by *an* event being sent.
            #[test]
            fn a_capture_source_with_no_alternative_is_stated_and_never_offered() {
                for locale in LOCALES {
                    let catalog = Catalog::new(locale);
                    let mut state = populated_state();
                    state.audio_source = Some(AudioSourceReading {
                        refresh_failed: false,
                        endpoints_known: true,
                        endpoints: Vec::new(),
                        selection: AudioEndpointSelection::SystemDefault,
                        captured_key: None,
                        captured_name: Some("Realtek HDMI".into()),
                        state: CaptureState::Capturing,
                    });

                    let mut harness = page_harness(Page::Audio, locale, TALL_CANVAS, state);
                    harness.step();

                    let offered: Vec<String> = harness
                        .root()
                        .children_recursive()
                        .filter(|node| node.accesskit_node().role() == accesskit::Role::RadioButton)
                        .filter_map(|node| {
                            node.accesskit_node().label().map(|label| label.to_string())
                        })
                        .collect();
                    assert!(
                        offered.is_empty(),
                        "{locale:?}: the only source there is was drawn as a choice: \
                         {offered:?}"
                    );

                    // Absent is not the same as broken, so the source stays
                    // on the page -- named, and labeled read-only exactly as
                    // paragraph 7.4 asks.
                    let announced =
                        crate::ui::components::test_support::accessible_strings(harness.root());
                    for wanted in [
                        named(
                            catalog,
                            TextKey::AudioCaptureSource,
                            catalog.text(TextKey::AudioCaptureSystemDefault),
                        ),
                        catalog.text(TextKey::AudioCaptureSourceFixed).to_owned(),
                    ] {
                        assert!(
                            announced.iter().any(|text| text == &wanted),
                            "{locale:?}: the fixed source never says {wanted:?}: \
                             {announced:?}"
                        );
                    }
                }
            }

            /// An empty offer does not lock the user into a dead preference.
            ///
            /// The rule that silences the group is "no row names anything
            /// other than what is already in force" -- deliberately not "the
            /// backend published no endpoints". With a stored preference the
            /// machine can no longer satisfy, the Windows-default row names
            /// something else, and it is the only way back: silence it too
            /// and a state file written on another machine would strand this
            /// one on a device that does not exist.
            #[test]
            fn the_way_back_from_an_absent_preference_stays_a_choice() {
                let catalog = Catalog::new(ResolvedLocale::English);
                let mut state = populated_state();
                state.audio_source = Some(AudioSourceReading {
                    refresh_failed: false,
                    endpoints_known: true,
                    endpoints: Vec::new(),
                    selection: AudioEndpointSelection::ChosenButMissing {
                        name: "Arctis 7 Chat".into(),
                    },
                    captured_key: None,
                    captured_name: None,
                    state: CaptureState::Unavailable,
                });

                let mut harness =
                    page_harness(Page::Audio, ResolvedLocale::English, TALL_CANVAS, state);
                harness.step();
                let label = named(
                    catalog,
                    TextKey::AudioCaptureSource,
                    catalog.text(TextKey::AudioCaptureSystemDefault),
                );
                let node = harness.get_by_role_and_label(accesskit::Role::RadioButton, &label);
                node.click();
                harness.step();

                assert_eq!(
                    harness.state().as_slice(),
                    [AppEvent::AudioEndpointRequested(
                        AudioEndpointRequest::SystemDefault
                    )],
                    "the only escape from an absent preference sent nothing"
                );
            }

            /// Paragraph 7.4's unfulfillable preference, and the tile that
            /// refuses to invent a capture device.
            #[test]
            fn a_chosen_capture_device_that_is_absent_is_named_as_absent() {
                for locale in LOCALES {
                    let catalog = Catalog::new(locale);
                    let mut state = populated_state();
                    state.audio_source = Some(AudioSourceReading {
                        refresh_failed: false,
                        endpoints_known: true,
                        endpoints: Vec::new(),
                        selection: AudioEndpointSelection::ChosenButMissing {
                            name: "Arctis 7 Chat".into(),
                        },
                        captured_key: None,
                        captured_name: None,
                        state: CaptureState::Unavailable,
                    });

                    let mut harness = page_harness(Page::Audio, locale, TALL_CANVAS, state);
                    harness.step();
                    let announced =
                        crate::ui::components::test_support::accessible_strings(harness.root());

                    let missing = catalog.named_value(NamedValueArgs {
                        name: catalog.text(TextKey::AudioCaptureSelectionMissing),
                        value: "Arctis 7 Chat",
                    });
                    assert!(
                        announced.iter().any(|text| text == &missing),
                        "{locale:?}: the absent preference is not named: {announced:?}"
                    );

                    let unknown = catalog.text(TextKey::Unknown);
                    assert!(
                        announced.iter().any(|text| {
                            text.contains(catalog.text(TextKey::AudioCaptureActiveDevice))
                                && text.contains(unknown)
                        }),
                        "{locale:?}: nothing is captured, so the tile has to say the word \
                         rather than a name: {announced:?}"
                    );
                }
            }
        }

        #[test]
        fn speakers_lists_discovered_receivers_and_stages_the_clicked_receiver() {
            let mut harness = page_harness(
                Page::Speakers,
                ResolvedLocale::English,
                TARGET_SIZES[0],
                populated_state(),
            );
            harness.step();

            let catalog = Catalog::new(ResolvedLocale::English);
            let _ = harness.get_by_label_contains(catalog.text(TextKey::ReceiverSelected));
            let _ = harness.get_by_label_contains(catalog.text(TextKey::ReceiverUnavailable));

            harness.get_by_label_contains("HomePod mini").click();
            harness.step();
            assert_eq!(
                *harness.state(),
                vec![crate::app::AppEvent::ToggleStagedReceiver(rid(2))],
                "the shared selection draft is the only click effect"
            );
        }

        /// The staged set is the selection; the applied set is not.
        ///
        /// With `desired = {HomePod}` and `staged = {HomePod mini}` the Change
        /// Bar reads "1 speaker added, 1 speaker removed". A page that called
        /// both of them selected would name a state no reducer can produce,
        /// and would contradict the bar on the same frame.
        #[test]
        fn speakers_calls_only_the_staged_receiver_selected() {
            let mut state = populated_state();
            state.desired_receivers = HashSet::from_iter([rid(1)]);
            state.staged_receivers = HashSet::from_iter([rid(2)]);

            let catalog = Catalog::new(ResolvedLocale::English);
            let selected = catalog.text(TextKey::ReceiverSelected);
            let mut harness = page_harness(
                Page::Speakers,
                ResolvedLocale::English,
                TARGET_SIZES[0],
                state,
            );
            harness.step();

            // Selection cards are accessible checkboxes; their announced
            // name and checked state describe the shared staged set.
            let claims: Vec<String> = harness
                .query_all_by_label_contains(selected)
                .filter(|node| node.accesskit_node().toggled() == Some(accesskit::Toggled::True))
                .filter_map(|node| node.accesskit_node().label())
                .collect();
            assert_eq!(
                claims.len(),
                1,
                "exactly one receiver is staged, so exactly one row may claim it: {claims:?}"
            );
            assert!(
                claims[0].contains("HomePod mini"),
                "the staged receiver is the selected one: {claims:?}"
            );
        }

        /// Speakers and Overview describe the same receiver in the same
        /// words.
        ///
        /// The page used to build its own sentence -- `"<name>: <state>"` --
        /// which read the two facts as an assignment and dropped the device
        /// class, the availability word, and the applied membership that the
        /// Overview card announces for the very same receiver. Two
        /// descriptions of one thing in one window is one too many, and the
        /// worse one sat on the page built for it.
        #[test]
        fn every_speaker_row_announces_the_receiver_card_sentence() {
            for locale in LOCALES {
                let state = populated_state();
                let expected: Vec<String> = crate::ui::presentation::receiver_card_models(
                    &UiSnapshot::from_state(&state),
                    Catalog::new(locale),
                )
                .into_iter()
                .map(|card| card.accessible_name)
                .collect();
                assert_eq!(expected.len(), 2, "the fixture lost a receiver");

                let mut harness = page_harness(Page::Speakers, locale, TALL_CANVAS, state);
                harness.step();
                let announced =
                    crate::ui::components::test_support::accessible_strings(harness.root());
                for sentence in &expected {
                    assert!(
                        announced.iter().any(|text| text == sentence),
                        "{locale:?}: no row announces {sentence:?}; the page says {announced:?}"
                    );
                }
            }
        }

        /// Each inventory row is a card, not a line of set text.
        ///
        /// The geometry is section 5.3's: corner radius 12, padding 16, and a
        /// receiver row at least 56 points tall. Read off the paint output,
        /// because a card has no accessibility node of its own to ask.
        #[test]
        fn every_speaker_row_is_a_card_and_not_a_line_of_bare_text() {
            use crate::ui::presentation::{CARD_RADIUS, RECEIVER_CARD_MIN_HEIGHT};

            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let state = populated_state();
                let names: Vec<String> = state.receivers.iter().map(|r| r.name.clone()).collect();
                let mut harness = page_harness(Page::Speakers, locale, TALL_CANVAS, state);
                harness.step();

                let radius = egui::CornerRadius::same(CARD_RADIUS as u8);
                let cards: Vec<egui::Rect> =
                    crate::ui::components::test_support::painted_rects(&harness)
                        .into_iter()
                        .filter(|(rect, corner, _)| {
                            *corner == radius && rect.height() >= RECEIVER_CARD_MIN_HEIGHT
                        })
                        .map(|(rect, _, _)| rect)
                        .collect();
                assert_eq!(
                    cards.len(),
                    names.len() * 2,
                    "{locale:?}: expected one fill and one border per receiver, painted {cards:?}"
                );

                // The colon joined a name to a state word, which reads as an
                // assignment. Name and state are two facts about one device.
                let painted = crate::ui::components::test_support::painted_strings(&harness);
                for name in &names {
                    for key in [
                        TextKey::ReceiverAvailable,
                        TextKey::ReceiverUnavailable,
                        TextKey::ReceiverSelected,
                        TextKey::ReceiverActive,
                        TextKey::ReceiverSelectedAndActive,
                    ] {
                        let assignment = catalog.named_value(NamedValueArgs {
                            name,
                            value: catalog.text(key),
                        });
                        assert!(
                            !painted.iter().any(|text| text == &assignment),
                            "{locale:?}: the page still paints {assignment:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn speakers_shows_the_empty_state_when_nothing_was_discovered() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness =
                    page_harness(Page::Speakers, locale, TARGET_SIZES[1], AppState::default());
                harness.step();
                let _ = harness.get_by_label_contains(catalog.text(TextKey::SpeakersEmptyTitle));
            }
        }

        #[test]
        fn diagnostics_guides_idle_users_and_is_factual_once_streaming() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TARGET_SIZES[0],
                populated_state(),
            );
            harness.step();
            let _ = harness.get_by_label(catalog.text(TextKey::DiagnosticsIdle));

            let mut running = populated_state();
            running.stream = StreamState::Streaming {
                generation: GenerationId(2),
            };
            running.generations.session = GenerationId(2);
            running.active_receivers = HashSet::from_iter([rid(1)]);
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TARGET_SIZES[0],
                running,
            );
            harness.step();
            let _ = harness.get_by_label(&catalog.named_value(NamedValueArgs {
                name: catalog.text(TextKey::Status),
                value: catalog.text(TextKey::Streaming),
            }));
            let _ = harness.get_by_label_contains("1 selected, 1 streaming");
        }

        #[test]
        fn settings_theme_radios_are_localized_and_dispatch_theme_changed() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness =
                    page_harness(Page::Settings, locale, TARGET_SIZES[0], populated_state());
                harness.step();
                let dark = catalog.named_value(NamedValueArgs {
                    name: catalog.text(TextKey::Appearance),
                    value: catalog.text(TextKey::ThemeDark),
                });
                harness.get_by_label(&dark).click();
                harness.step();
                assert_eq!(
                    *harness.state(),
                    vec![AppEvent::ThemeChanged(ThemePreference::Dark)],
                    "{locale:?}"
                );
            }
        }

        #[test]
        fn settings_hotkey_apply_dispatches_exactly_one_changed_event() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let mut harness = page_harness(
                Page::Settings,
                ResolvedLocale::English,
                TARGET_SIZES[0],
                populated_state(),
            );
            harness.step();

            harness
                .get_by_label(catalog.text(TextKey::ShortcutAlt))
                .click();
            harness.step();
            harness
                .get_by_label(catalog.text(TextKey::ApplyShortcut))
                .click();
            harness.step();

            assert_eq!(
                *harness.state(),
                vec![AppEvent::HotkeyChanged(HotkeyBinding {
                    enabled: true,
                    modifiers: crate::app::MOD_CONTROL,
                    virtual_key: 0x48,
                })]
            );
        }

        #[test]
        fn settings_hotkey_validation_is_localized_and_blocks_dispatch() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness =
                    page_harness(Page::Settings, locale, TARGET_SIZES[0], populated_state());
                harness.step();

                harness
                    .get_by_label(catalog.text(TextKey::ShortcutControl))
                    .click();
                harness.step();
                harness
                    .get_by_label(catalog.text(TextKey::ShortcutAlt))
                    .click();
                harness.step();

                let _ = harness.get_by_label_contains(catalog.text(TextKey::ShortcutNeedsModifier));
                assert!(harness.state().is_empty(), "{locale:?}");
            }
        }

        #[test]
        fn settings_about_names_the_real_version_and_licence_documents() {
            let catalog = Catalog::new(ResolvedLocale::English);
            let mut harness = page_harness(
                Page::Settings,
                ResolvedLocale::English,
                TALL_CANVAS,
                populated_state(),
            );
            harness.step();
            let _ = harness.get_by_label_contains(env!("CARGO_PKG_VERSION"));
            let _ = harness.get_by_label_contains(catalog.text(TextKey::ThirdPartyNotices));
            let _ = harness.get_by_label(catalog.text(TextKey::OpenLicense));
            harness
                .get_by_label(catalog.text(TextKey::CopyLicenseText))
                .click();
            harness.step();
            assert!(
                harness.state().is_empty(),
                "copying a document is not a domain event"
            );
        }

        /// Words that only ever occur in English copy. A German rendering that
        /// contains one of them is a page that hard-coded a string.
        const ENGLISH_MARKERS: &[&str] = &[
            "the",
            "and",
            "with",
            "for",
            "settings",
            "speakers",
            "receiver",
            "receivers",
            "available",
            "unavailable",
            "selected",
            "apply",
            "discard",
            "shortcut",
            "keyboard",
            "appearance",
            "light",
            "dark",
            "overview",
            "groups",
            "diagnostics",
            "license",
            "network",
            "session",
            "measurements",
        ];

        fn words(text: &str) -> Vec<String> {
            text.split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(|word| word.to_lowercase())
                .collect()
        }

        /// The guard has to be able to fail.
        #[test]
        fn the_language_guard_flags_a_planted_english_sentence() {
            assert!(words("Die Verbindung wird for the network aufgebaut")
                .iter()
                .any(|word| ENGLISH_MARKERS.contains(&word.as_str())));
        }

        /// Every destination, Overview included: the Command Home replaced
        /// the legacy Home renderer and its English literals.
        #[test]
        fn german_destinations_announce_no_english_words() {
            for &page in &[
                Page::Home,
                Page::Speakers,
                Page::Groups,
                Page::Audio,
                Page::Diagnostics,
                Page::Settings,
            ] {
                let mut harness = page_harness(
                    page,
                    ResolvedLocale::German,
                    TARGET_SIZES[0],
                    populated_state(),
                );
                harness.step();
                for text in crate::ui::components::test_support::accessible_strings(harness.root())
                {
                    for word in words(&text) {
                        assert!(
                            !ENGLISH_MARKERS.contains(&word.as_str()),
                            "{page:?} announces English copy: {text}"
                        );
                    }
                }
            }
        }
    }

    /// Task 9: the Settings groups that had no control at all.
    ///
    /// `AppEvent::LocaleChanged` and `AppEvent::AdvancedInformationChanged`
    /// existed since Task 3 with zero triggers anywhere in the renderer, and
    /// the German/English catalog from Task 2 had no consumer that could
    /// switch languages. These tests are what closes both gaps.
    /// The Diagnostics destination, once it has something real to show.
    ///
    /// Three tiles, and the whole point of them is the difference between a
    /// number the registry measured and a number nobody has measured yet. A
    /// page that drew `0` for both would be lying in the one place a user
    /// goes to find out whether something is wrong.
    mod live_diagnostics {
        use super::*;
        use crate::app::{
            BufferDiagnosticsReading, DiagnosticIssue, DiagnosticReceiverState,
            LiveDiagnosticsReading, ReceiverDiagnosticsReading, TransportDiagnosticsReading,
        };

        fn running(reading: LiveDiagnosticsReading) -> AppState {
            let mut state = populated_state();
            state.stream = StreamState::Streaming {
                generation: GenerationId(12),
            };
            state.generations.session = GenerationId(12);
            state.active_receivers = HashSet::from_iter([rid(1)]);
            state.live_diagnostics = Some(reading);
            state
        }

        #[test]
        fn idle_diagnostics_explains_how_to_get_transport_measurements() {
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                populated_state(),
            );
            harness.step();
            harness.get_by_label("Start streaming to see transport diagnostics.");
            assert!(harness.state().is_empty());
        }

        #[test]
        fn input_peak_distinguishes_unmeasured_from_measured_silence() {
            let unmeasured = running(LiveDiagnosticsReading {
                generation: GenerationId(12),
                input_peak_per_mille: None,
                ..Default::default()
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                unmeasured,
            );
            harness.step();
            harness.get_by_label_contains("Input signal: Unknown");

            let silent = running(LiveDiagnosticsReading {
                generation: GenerationId(12),
                input_peak_per_mille: Some(0),
                ..Default::default()
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                silent,
            );
            harness.step();
            harness.get_by_label_contains("Input signal: 0%");
            harness.get_by_label(
                "No Windows input signal is currently measured. Start audio on the selected source.",
            );
            assert!(harness.state().is_empty());
        }

        #[test]
        fn stopped_session_stays_idle_even_when_a_reading_is_present() {
            let mut state = populated_state();
            state.live_diagnostics = Some(LiveDiagnosticsReading {
                generation: GenerationId::default(),
                input_peak_per_mille: Some(800),
                ..Default::default()
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                state,
            );
            harness.step();
            harness.get_by_label("Start streaming to see transport diagnostics.");
        }

        #[test]
        fn known_source_failure_stays_actionable_without_live_transport_reading() {
            use crate::app::{AudioEndpointSelection, AudioSourceReading, CaptureState};

            for (capture_state, verdict) in [
                (
                    CaptureState::Unavailable,
                    "The Windows audio source is unavailable. Check the selected playback device in Audio.",
                ),
                (
                    CaptureState::Failed,
                    "The Windows audio source is unavailable. Check the selected playback device in Audio.",
                ),
                (
                    CaptureState::Recovering,
                    "The Windows audio source is recovering. Wait briefly and check again.",
                ),
            ] {
                let mut state = populated_state();
                state.stream = StreamState::Streaming {
                    generation: GenerationId(12),
                };
                state.generations.session = GenerationId(12);
                state.active_receivers = HashSet::from_iter([rid(1)]);
                state.audio_source = Some(AudioSourceReading {
                    endpoints_known: true,
                    refresh_failed: false,
                    endpoints: Vec::new(),
                    selection: AudioEndpointSelection::SystemDefault,
                    captured_key: None,
                    captured_name: None,
                    state: capture_state,
                });
                let mut harness = page_harness(
                    Page::Diagnostics,
                    ResolvedLocale::English,
                    TALL_CANVAS,
                    state,
                );
                harness.step();
                harness.get_by_label(verdict);
            }
        }

        #[test]
        fn verdict_ignores_unselected_receiver_failures() {
            let state = running(LiveDiagnosticsReading {
                generation: GenerationId(12),
                input_peak_per_mille: Some(500),
                buffer: Some(BufferDiagnosticsReading {
                    queued_frames: 3,
                    capacity_frames: 10,
                    buffered_ms: 60,
                    underruns: 0,
                }),
                receivers: vec![
                    ReceiverDiagnosticsReading {
                        id: rid(1),
                        state: DiagnosticReceiverState::Streaming,
                        transport: Some(TransportDiagnosticsReading {
                            packets_accepted: 10,
                            bytes_accepted: 3_200,
                            send_failures: 0,
                            retransmit_requests: 0,
                        }),
                        issue: None,
                    },
                    ReceiverDiagnosticsReading {
                        id: rid(2),
                        state: DiagnosticReceiverState::Offline,
                        transport: None,
                        issue: Some(DiagnosticIssue::Transport),
                    },
                ],
                ..Default::default()
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                state,
            );
            harness.step();
            harness.get_by_label("Windows input is measured and local send packets are recorded. Acoustic playback is not verified.");
            assert!(harness
                .query_by_label_contains("currently offline or failed")
                .is_none());
        }

        #[test]
        fn historical_capture_buffer_and_transport_problems_are_named_separately() {
            let cases = [
                (
                    2,
                    0,
                    0,
                    "Capture drops were recorded since app start. If audio drops out, check the Windows source.",
                ),
                (
                    0,
                    3,
                    0,
                    "Buffer underruns were recorded in this session. If audio drops out, try the Normal latency profile.",
                ),
                (
                    0,
                    0,
                    4,
                    "Local transport errors were recorded in this session. If audio drops out, check the speaker connection.",
                ),
            ];
            for (capture_drops, underruns, send_failures, verdict) in cases {
                let state = running(LiveDiagnosticsReading {
                    generation: GenerationId(12),
                    input_peak_per_mille: Some(500),
                    capture_drops_total: capture_drops,
                    buffer: Some(BufferDiagnosticsReading {
                        queued_frames: 3,
                        capacity_frames: 10,
                        buffered_ms: 60,
                        underruns,
                    }),
                    receivers: vec![ReceiverDiagnosticsReading {
                        id: rid(1),
                        state: DiagnosticReceiverState::Streaming,
                        transport: Some(TransportDiagnosticsReading {
                            packets_accepted: 10,
                            bytes_accepted: 3_200,
                            send_failures,
                            retransmit_requests: 0,
                        }),
                        issue: None,
                    }],
                });
                let mut harness = page_harness(
                    Page::Diagnostics,
                    ResolvedLocale::English,
                    TALL_CANVAS,
                    state,
                );
                harness.step();
                harness.get_by_label(verdict);
            }
        }

        #[test]
        fn signal_present_waits_for_send_measurements_before_the_healthy_verdict() {
            let state = running(LiveDiagnosticsReading {
                generation: GenerationId(12),
                input_peak_per_mille: Some(500),
                receivers: vec![ReceiverDiagnosticsReading {
                    id: rid(1),
                    state: DiagnosticReceiverState::Streaming,
                    transport: None,
                    issue: None,
                }],
                ..Default::default()
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                state,
            );
            harness.step();
            harness.get_by_label(
                "Windows input signal is present. Waiting for local send measurements.",
            );
            assert!(harness
                .query_by_label_contains("local send packets are recorded")
                .is_none());
        }

        #[test]
        fn detailed_live_counters_are_local_named_and_explanatory() {
            let state = running(LiveDiagnosticsReading {
                generation: GenerationId(12),
                input_peak_per_mille: Some(375),
                capture_drops_total: 0,
                buffer: Some(BufferDiagnosticsReading {
                    queued_frames: 3,
                    capacity_frames: 10,
                    buffered_ms: 60,
                    underruns: 0,
                }),
                receivers: vec![
                    ReceiverDiagnosticsReading {
                        id: rid(1),
                        state: DiagnosticReceiverState::Streaming,
                        transport: Some(TransportDiagnosticsReading {
                            packets_accepted: 120,
                            bytes_accepted: 48_000,
                            send_failures: 2,
                            retransmit_requests: 4,
                        }),
                        issue: None,
                    },
                    ReceiverDiagnosticsReading {
                        id: rid(99),
                        state: DiagnosticReceiverState::Failed,
                        transport: None,
                        issue: Some(DiagnosticIssue::Pairing),
                    },
                ],
            });
            let mut harness = page_harness(
                Page::Diagnostics,
                ResolvedLocale::English,
                TALL_CANVAS,
                state,
            );
            harness.step();

            harness.get_by_label("Local transport errors were recorded in this session. If audio drops out, check the speaker connection.");
            harness.get_by_label_contains("Input signal: 37.5%");
            assert!(harness
                .query_by_label_contains("Packets accepted by local OS")
                .is_none());
            harness.get_by_label("Show detailed counters").click();
            harness.run();

            for fact in [
                "Buffer fill: 3 / 10 frames",
                "Buffered audio: 60 ms",
                "Buffer underruns since session start: 0",
                "Capture drops since app start: 0",
                "Packets accepted by local OS: 120",
                "Bytes accepted by local OS: 48000",
                "Local send errors: 2",
                "Retransmit slots requested: 4",
                "Retransmit slots are requests, not measured packet loss.",
            ] {
                harness.get_by_label_contains(fact);
            }
            harness.get_by_label_contains("HomePod");
            assert!(
                harness.query_by_label_contains("Pairing problem").is_none(),
                "a diagnostics ID absent from the safe receiver inventory must not create a row"
            );
            assert!(harness.state().is_empty());
        }
    }

    mod diagnostics_registry {
        use super::*;
        use crate::app::{DiagnosticsHealth, DiagnosticsReading};
        use crate::ui::presentation::{DiagnosticsModel, MetricFreshness};

        fn running(reading: Option<DiagnosticsReading>) -> AppState {
            let mut state = populated_state();
            state.stream = StreamState::Streaming {
                generation: GenerationId(2),
            };
            state.generations.session = GenerationId(2);
            state.active_receivers = HashSet::from_iter([rid(1)]);
            state.diagnostics = reading;
            state
        }

        fn model(state: &AppState, locale: ResolvedLocale) -> DiagnosticsModel {
            DiagnosticsModel::from_snapshot(&UiSnapshot::from_state(state), Catalog::new(locale))
        }

        /// Nothing heard from the registry is not the same as nothing wrong.
        #[test]
        fn an_absent_reading_marks_every_tile_unknown_and_fabricates_no_zero() {
            for locale in LOCALES {
                let state = running(None);
                let built = model(&state, locale);

                assert_eq!(built.registry.len(), 3, "{locale:?}");
                for tile in &built.registry {
                    assert_eq!(
                        tile.freshness,
                        MetricFreshness::Unknown,
                        "{locale:?} {}",
                        tile.label
                    );
                    assert!(
                        !tile.value.contains(|c: char| c.is_ascii_digit()),
                        "{locale:?} fabricated a measurement: {}",
                        tile.value
                    );
                }

                let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                harness.step();
                for tile in &built.registry {
                    let _ = harness.get_by_label_contains(&tile.accessible_phrase());
                }
            }
        }

        /// A registry that reports zero has measured zero, and the page says
        /// so without the freshness marker an unmeasured tile carries.
        #[test]
        fn a_reading_of_zero_is_drawn_as_a_measurement_and_not_as_unknown() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let state = running(Some(DiagnosticsReading {
                    health: DiagnosticsHealth::Running,
                    events_dropped_total: 0,
                    measured_receivers: 0,
                }));
                let built = model(&state, locale);

                assert_eq!(built.registry.len(), 3, "{locale:?}");
                for tile in &built.registry {
                    assert_eq!(
                        tile.freshness,
                        MetricFreshness::Live,
                        "{locale:?} {}",
                        tile.label
                    );
                    assert_eq!(tile.symbol, None, "{locale:?} {}", tile.label);
                }
                let counters = &built.registry[1..];
                assert!(
                    counters.iter().all(|tile| tile.value == "0"),
                    "{locale:?} lost a measured zero: {counters:?}"
                );
                assert!(
                    built
                        .registry
                        .iter()
                        .all(|tile| tile.value != catalog.text(TextKey::Unknown)),
                    "{locale:?} drew a measurement as unknown"
                );

                let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                harness.step();
                for tile in &built.registry {
                    let _ = harness.get_by_label_contains(&tile.accessible_phrase());
                }
            }
        }

        /// A count of zero is a measurement only where there is something to
        /// count in.
        ///
        /// [`DiagnosticsHealth::Unknown`] is the registry stating it has no
        /// active session, and its receiver rows are built from nowhere else,
        /// so the zero that arrives beside it is an absence rather than a
        /// count. That matters today rather than in principle: nothing in the
        /// production path registers a session with the registry yet, so this
        /// is the state the window is in *while receivers are streaming*, and
        /// "Receivers with measurements: 0" next to a running stream is a
        /// false sentence, not a terse one.
        #[test]
        fn a_receiver_count_without_a_session_is_drawn_as_unmeasured() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let state = running(Some(DiagnosticsReading {
                    health: DiagnosticsHealth::Unknown,
                    events_dropped_total: 4,
                    measured_receivers: 0,
                }));
                let built = model(&state, locale);

                let receivers = &built.registry[2];
                assert_eq!(receivers.freshness, MetricFreshness::Unknown, "{locale:?}");
                assert_eq!(
                    receivers.value,
                    catalog.text(TextKey::Unknown),
                    "{locale:?}"
                );
                assert!(
                    !receivers.value.contains(|c: char| c.is_ascii_digit()),
                    "{locale:?} fabricated a count: {}",
                    receivers.value
                );

                // The counter beside it is a real measurement with or without
                // a session, and must not be swept up by the same rule.
                let lost = &built.registry[1];
                assert_eq!(lost.freshness, MetricFreshness::Live, "{locale:?}");
                assert_eq!(lost.value, "4", "{locale:?}");

                let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                harness.step();
                for tile in &built.registry {
                    let _ = harness.get_by_label_contains(&tile.accessible_phrase());
                }
            }
        }

        /// The empty state is reachable, and a registry reading cannot take
        /// it away.
        ///
        /// The backend bridge is opened unconditionally at start-up and
        /// publishes a first reading in the first pass of its loop, before it
        /// ever parks, so `diagnostics` is `Some` within a fraction of a
        /// second of launch and never returns to `None`. An empty state that
        /// additionally required no reading would therefore be unreachable
        /// for the whole life of the process: a stopped window would answer
        /// with three tiles of nothing instead of the one sentence this page
        /// has for exactly that case.
        #[test]
        fn a_stopped_window_keeps_its_empty_state_after_the_registry_has_reported() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut state = populated_state();
                state.diagnostics = Some(DiagnosticsReading {
                    health: DiagnosticsHealth::Unknown,
                    events_dropped_total: 0,
                    measured_receivers: 0,
                });
                assert!(matches!(state.stream, StreamState::Stopped), "{locale:?}");
                assert!(state.notice.is_none(), "{locale:?}");

                assert!(
                    model(&state, locale).empty.is_some(),
                    "{locale:?} lost the empty state to a registry reading"
                );

                let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                harness.step();
                let _ = harness.get_by_label(catalog.text(TextKey::DiagnosticsIdle));
            }
        }

        /// Every health state the shell can hold reaches the page as its own
        /// word, and the roster is bound to the enum rather than remembered
        /// beside it.
        #[test]
        fn every_health_state_has_its_own_word_on_the_page() {
            macro_rules! health_roster {
                ($($pattern:pat => $name:literal,)+) => {
                    const EVERY_NAME: &[&str] = &[$($name),+];

                    fn name_of(health: DiagnosticsHealth) -> &'static str {
                        match health {
                            $($pattern => $name,)+
                        }
                    }
                };
            }

            health_roster! {
                DiagnosticsHealth::Unknown => "unknown",
                DiagnosticsHealth::Running => "running",
                DiagnosticsHealth::Attention => "attention",
                DiagnosticsHealth::Error => "error",
            }

            let covered = DiagnosticsHealth::ALL
                .iter()
                .map(|health| name_of(*health))
                .collect::<std::collections::HashSet<_>>();
            let missing = EVERY_NAME
                .iter()
                .filter(|name| !covered.contains(*name))
                .collect::<Vec<_>>();
            assert!(
                missing.is_empty(),
                "these health states never reach the page: {missing:?}"
            );

            for locale in LOCALES {
                let mut words = Vec::new();
                for &health in DiagnosticsHealth::ALL {
                    let state = running(Some(DiagnosticsReading {
                        health,
                        events_dropped_total: 0,
                        measured_receivers: 0,
                    }));
                    let built = model(&state, locale);
                    let tile = built.registry[0].clone();
                    assert_eq!(
                        tile.freshness,
                        MetricFreshness::Live,
                        "{locale:?} {health:?}"
                    );

                    let mut harness = page_harness(Page::Diagnostics, locale, TALL_CANVAS, state);
                    harness.step();
                    let _ = harness.get_by_label_contains(&tile.accessible_phrase());
                    words.push(tile.value);
                }
                words.sort();
                let distinct = {
                    let mut unique = words.clone();
                    unique.dedup();
                    unique.len()
                };
                assert_eq!(
                    distinct,
                    DiagnosticsHealth::ALL.len(),
                    "{locale:?} gave two health states the same word: {words:?}"
                );
            }
        }
    }

    mod settings_groups {
        use super::*;

        use crate::app::LocalePreference;

        /// The announced name of one choice inside a Settings radio group.
        ///
        /// Appearance and Language both offer a choice painted "System", so
        /// the accessible name has to carry the group. A bare "System" is
        /// ambiguous to a screen reader and ambiguous to this test.
        fn choice_name(locale: ResolvedLocale, group: TextKey, choice: TextKey) -> String {
            let catalog = Catalog::new(locale);
            catalog.named_value(NamedValueArgs {
                name: catalog.text(group),
                value: catalog.text(choice),
            })
        }

        const LANGUAGE_CHOICES: [(TextKey, LocalePreference); 3] = [
            (TextKey::LanguageSystem, LocalePreference::System),
            (TextKey::LanguageGerman, LocalePreference::German),
            (TextKey::LanguageEnglish, LocalePreference::English),
        ];

        fn settings_harness(locale: ResolvedLocale, state: AppState) -> PageHarness {
            page_harness(Page::Settings, locale, TARGET_SIZES[0], state)
        }

        /// Which language choices report themselves selected.
        fn selected_languages(harness: &PageHarness, locale: ResolvedLocale) -> Vec<TextKey> {
            let mut selected = Vec::new();
            for (choice, _) in LANGUAGE_CHOICES {
                let name = choice_name(locale, TextKey::Language, choice);
                let node = harness.get_by_label(&name);
                if node.accesskit_node().toggled() == Some(accesskit::Toggled::True) {
                    selected.push(choice);
                }
            }
            selected
        }

        #[test]
        fn every_language_choice_is_localized_and_dispatches_exactly_one_event() {
            for locale in LOCALES {
                for (choice, preference) in LANGUAGE_CHOICES {
                    let mut state = populated_state();
                    // The click has to be a real change, so the starting
                    // preference is never the one being clicked.
                    state.locale_preference = match preference {
                        LocalePreference::System => LocalePreference::English,
                        _ => LocalePreference::System,
                    };
                    state.windows_display_locale = locale;
                    state.resolved_locale = locale;

                    let mut harness = settings_harness(locale, state);
                    let name = choice_name(locale, TextKey::Language, choice);
                    harness.get_by_label(&name).click();
                    harness.step();

                    assert_eq!(
                        *harness.state(),
                        vec![AppEvent::LocaleChanged(preference)],
                        "{locale:?} / {choice:?}"
                    );
                }
            }
        }

        /// The selection reads `locale_preference`, not `resolved_locale`.
        /// Following System into German must not silently move the dot onto
        /// "Deutsch": the user never chose German, and moving it would make
        /// the return to System invisible.
        #[test]
        fn system_stays_selected_while_it_resolves_to_german() {
            let mut state = populated_state();
            state.locale_preference = LocalePreference::System;
            state.windows_display_locale = ResolvedLocale::German;
            state.resolved_locale = ResolvedLocale::German;

            let harness = settings_harness(ResolvedLocale::German, state);
            assert_eq!(
                selected_languages(&harness, ResolvedLocale::German),
                vec![TextKey::LanguageSystem]
            );
        }

        /// An explicit choice ignores later Windows language changes.
        #[test]
        fn an_explicit_english_choice_survives_windows_switching_to_german() {
            let mut state = populated_state();
            state.locale_preference = LocalePreference::English;
            state.windows_display_locale = ResolvedLocale::German;
            state.resolved_locale = ResolvedLocale::English;

            let harness = settings_harness(ResolvedLocale::English, state);
            assert_eq!(
                selected_languages(&harness, ResolvedLocale::English),
                vec![TextKey::LanguageEnglish]
            );
        }

        /// Appearance keeps dispatching its own event after the group gained
        /// a qualified accessible name.
        #[test]
        fn every_appearance_choice_is_localized_and_dispatches_exactly_one_event() {
            for locale in LOCALES {
                for (choice, preference) in [
                    (TextKey::ThemeSystem, ThemePreference::System),
                    (TextKey::ThemeLight, ThemePreference::Light),
                    (TextKey::ThemeDark, ThemePreference::Dark),
                ] {
                    let mut state = populated_state();
                    state.theme = match preference {
                        ThemePreference::System => ThemePreference::Dark,
                        _ => ThemePreference::System,
                    };

                    let mut harness = settings_harness(locale, state);
                    let name = choice_name(locale, TextKey::Appearance, choice);
                    harness.get_by_label(&name).click();
                    harness.step();

                    assert_eq!(
                        *harness.state(),
                        vec![AppEvent::ThemeChanged(preference)],
                        "{locale:?} / {choice:?}"
                    );
                }
            }
        }

        /// The switch reports its state to assistive technology and paints the
        /// state word, so neither a screen reader nor a user who cannot
        /// separate two tints has to infer it from colour.
        #[test]
        fn the_advanced_switch_announces_its_state_and_paints_the_state_word() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                for enabled in [false, true] {
                    let mut state = populated_state();
                    state.advanced_information = enabled;
                    let harness = settings_harness(locale, state);

                    let switch = harness.get_by_role_and_label(
                        accesskit::Role::CheckBox,
                        catalog.text(TextKey::AdvancedInformation),
                    );
                    let expected = if enabled {
                        accesskit::Toggled::True
                    } else {
                        accesskit::Toggled::False
                    };
                    assert_eq!(
                        switch.accesskit_node().toggled(),
                        Some(expected),
                        "{locale:?} / {enabled}"
                    );

                    let word = catalog.text(if enabled { TextKey::On } else { TextKey::Off });
                    let painted =
                        crate::ui::components::test_support::accessible_strings(harness.root());
                    assert!(
                        painted.iter().any(|text| text == word),
                        "{locale:?} / {enabled}: the state word {word:?} is not on the page"
                    );
                }
            }
        }

        /// Everything one destination announces with the flag in one position.
        fn announced_with_advanced(
            page: Page,
            locale: ResolvedLocale,
            enabled: bool,
        ) -> Vec<String> {
            let mut state = populated_state();
            state.advanced_information = enabled;
            let harness = page_harness(page, locale, TARGET_SIZES[0], state);
            crate::ui::components::test_support::accessible_strings(harness.root())
        }

        /// The sentence beside the switch names the destinations the flag
        /// really changes -- no more, no less.
        ///
        /// A description that promises more than the build delivers is the
        /// same defect as a control wired to nothing, moved from the code
        /// into the copy: the user flips the switch, walks to the page the
        /// sentence named, sees no difference, and concludes the switch is
        /// broken. So the promise is measured against the behaviour rather
        /// than proof-read, and it is measured per destination, so widening
        /// the sentence without widening the behaviour fails just as loudly
        /// as the reverse.
        ///
        /// Settings is exempt: it owns the switch, and a control that reports
        /// its own state is not the flag reaching a page.
        #[test]
        fn the_advanced_sentence_names_exactly_the_destinations_the_flag_changes() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let sentence = catalog.text(TextKey::AdvancedInformationDescription);
                let mut changed = Vec::new();
                for page in DESTINATIONS {
                    if page == Page::Settings {
                        continue;
                    }
                    let name = catalog.text(title_key(page));
                    let named = sentence.contains(name);
                    let differs = announced_with_advanced(page, locale, true)
                        != announced_with_advanced(page, locale, false);
                    assert_eq!(
                        named, differs,
                        "{locale:?}: {sentence:?} names {name:?}: {named}, but turning the flag \
                         on changes that destination: {differs}"
                    );
                    if differs {
                        changed.push(page);
                    }
                }
                // Without this the whole claim could be satisfied by a dead
                // flag and a sentence that names nothing.
                assert!(
                    !changed.is_empty(),
                    "{locale:?}: the flag changes no destination at all"
                );
            }
        }

        #[test]
        fn the_advanced_switch_dispatches_exactly_one_event_in_both_directions() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                for enabled in [false, true] {
                    let mut state = populated_state();
                    state.advanced_information = enabled;
                    let mut harness = settings_harness(locale, state);

                    harness
                        .get_by_role_and_label(
                            accesskit::Role::CheckBox,
                            catalog.text(TextKey::AdvancedInformation),
                        )
                        .click();
                    harness.step();

                    assert_eq!(
                        *harness.state(),
                        vec![AppEvent::AdvancedInformationChanged(!enabled)],
                        "{locale:?} / {enabled}"
                    );
                }
            }
        }
    }

    /// The two preferences end to end: the renderer dispatches, the reducer
    /// reduces, and the *next* frame of the whole shell shows the difference.
    ///
    /// A page-level test can only prove that Settings emitted an event. It
    /// cannot prove that the language of the rail, the page header, the
    /// status footer and the destination body actually changed, or that the
    /// Advanced switch reveals content that is genuinely absent without it.
    /// Both claims need a shell that reduces its own events, which is what
    /// this module builds.
    mod preferences_end_to_end {
        use super::*;

        use crate::app::{reduce, LocalePreference};
        use crate::ui::components::app_shell::{change_bar_model, show_shell};

        type LiveHarness = egui_kittest::Harness<'static, AppState>;

        /// A shell whose catalog is derived from the state it renders, and
        /// which feeds every event it dispatches back through the real
        /// reducer before the next frame.
        fn live_shell(state: AppState) -> LiveHarness {
            sized_live_shell(state, TARGET_SIZES[0])
        }

        /// The same live shell on a taller viewport.
        ///
        /// The Shell owns the only scroll region of the client area, so a
        /// control below the fold is reachable in the running window and
        /// unclickable in a harness that never scrolls. A test whose subject
        /// is what a control *does* -- not where it sits -- gets a viewport
        /// tall enough to hold the whole destination, exactly as the page
        /// harness does with `TALL_CANVAS`.
        fn sized_live_shell(state: AppState, size: [f32; 2]) -> LiveHarness {
            let mut harness: LiveHarness = egui_kittest::Harness::new_ui_state(
                move |ui, state: &mut AppState| {
                    let resolved =
                        theme::resolve_theme(ThemePreference::Light, None, TEST_APPEARANCE);
                    theme::apply_theme(ui.ctx(), &resolved);
                    let dispatched = {
                        let snapshot = UiSnapshot::from_state(state);
                        let resources = UiResources {
                            tokens: &resolved.tokens,
                            // The one line under test: the catalog follows the
                            // resolved locale of the current snapshot.
                            catalog: Catalog::new(snapshot.resolved_locale),
                        };
                        let bar = change_bar_model(&snapshot, resources.catalog);
                        let mut dispatched: Vec<AppEvent> = Vec::new();
                        show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut |event| {
                            dispatched.push(event);
                        });
                        dispatched
                    };
                    for event in dispatched {
                        reduce(state, event);
                    }
                },
                state,
            );
            harness.set_size(egui::vec2(size[0], size[1]));
            harness.step();
            harness
        }

        fn settings_state(locale: LocalePreference, windows: ResolvedLocale) -> AppState {
            AppState {
                page: Page::Settings,
                locale_preference: locale,
                windows_display_locale: windows,
                resolved_locale: locale.resolve(windows),
                ..populated_state()
            }
        }

        fn choice_name(locale: ResolvedLocale, group: TextKey, choice: TextKey) -> String {
            let catalog = Catalog::new(locale);
            catalog.named_value(NamedValueArgs {
                name: catalog.text(group),
                value: catalog.text(choice),
            })
        }

        /// Every string the shell currently announces.
        fn announced(harness: &LiveHarness) -> Vec<String> {
            crate::ui::components::test_support::accessible_strings(harness.root())
        }

        fn announces(harness: &LiveHarness, text: &str) -> bool {
            announced(harness).iter().any(|shown| shown.contains(text))
        }

        /// Choosing a language has to change the language of the *shell*, not
        /// only of the page that carries the control.
        ///
        /// The four checked strings come from four different owners: the
        /// navigation rail, the page header, the status footer, and the
        /// Settings body. A renderer that resolved copy from anything other
        /// than the snapshot's locale fails at least one of them.
        #[test]
        fn choosing_german_switches_the_rail_the_header_the_footer_and_the_body() {
            let mut harness = live_shell(settings_state(
                LocalePreference::English,
                ResolvedLocale::English,
            ));
            let english = Catalog::new(ResolvedLocale::English);
            let german = Catalog::new(ResolvedLocale::German);

            assert!(
                announces(&harness, english.text(TextKey::Speakers)),
                "the rail starts in English"
            );

            let name = choice_name(
                ResolvedLocale::English,
                TextKey::Language,
                TextKey::LanguageGerman,
            );
            harness.get_by_label(&name).click();
            harness.step();
            harness.step();

            for key in [
                // the rail
                TextKey::Speakers,
                // the page header
                TextKey::Settings,
                // the status footer
                TextKey::Ready,
                // the destination body
                TextKey::Appearance,
                TextKey::Language,
                TextKey::Keyboard,
                TextKey::AdvancedInformation,
            ] {
                assert!(
                    announces(&harness, german.text(key)),
                    "{key:?} is not German after the switch: {:?}",
                    announced(&harness)
                );
            }

            for word in ["Speakers", "Appearance", "Keyboard", "Ready"] {
                assert!(
                    !announces(&harness, word),
                    "{word:?} survived the switch to German"
                );
            }
        }

        /// System follows Windows; an explicit choice does not.
        #[test]
        fn system_follows_a_windows_language_change_and_an_explicit_choice_ignores_it() {
            let mut harness = live_shell(settings_state(
                LocalePreference::System,
                ResolvedLocale::English,
            ));
            let german = Catalog::new(ResolvedLocale::German);
            let english = Catalog::new(ResolvedLocale::English);

            // Windows switches to German while the preference is System.
            reduce(
                harness.state_mut(),
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German),
            );
            harness.step();
            assert!(
                announces(&harness, german.text(TextKey::Speakers)),
                "System did not follow Windows into German"
            );
            let system = choice_name(
                ResolvedLocale::German,
                TextKey::Language,
                TextKey::LanguageSystem,
            );
            assert_eq!(
                harness.get_by_label(&system).accesskit_node().toggled(),
                Some(accesskit::Toggled::True),
                "System stopped being the selected choice"
            );

            // The user now picks English explicitly.
            let explicit = choice_name(
                ResolvedLocale::German,
                TextKey::Language,
                TextKey::LanguageEnglish,
            );
            harness.get_by_label(&explicit).click();
            harness.step();
            harness.step();
            assert!(announces(&harness, english.text(TextKey::Speakers)));

            // Windows changes again. The explicit choice wins.
            reduce(
                harness.state_mut(),
                AppEvent::WindowsDisplayLanguageChanged(ResolvedLocale::German),
            );
            harness.step();
            assert!(
                announces(&harness, english.text(TextKey::Speakers)),
                "an explicit English choice followed Windows into German"
            );
            let explicit = choice_name(
                ResolvedLocale::English,
                TextKey::Language,
                TextKey::LanguageEnglish,
            );
            assert_eq!(
                harness.get_by_label(&explicit).accesskit_node().toggled(),
                Some(accesskit::Toggled::True)
            );
        }

        /// The Advanced switch is not decoration: content that is absent with
        /// it off appears with it on, on a *different* destination.
        #[test]
        fn the_advanced_switch_reveals_content_that_is_absent_while_it_is_off() {
            let locale = ResolvedLocale::English;
            let catalog = Catalog::new(locale);
            let mut harness = sized_live_shell(
                settings_state(LocalePreference::English, ResolvedLocale::English),
                TALL_CANVAS,
            );
            assert!(!harness.state().advanced_information);

            harness
                .get_by_role_and_label(
                    accesskit::Role::CheckBox,
                    catalog.text(TextKey::AdvancedInformation),
                )
                .click();
            harness.step();
            harness.step();
            assert!(
                harness.state().advanced_information,
                "the switch did not reach the reducer"
            );

            // Overview, before and after, using the same live shell.
            reduce(harness.state_mut(), AppEvent::Navigate(Page::Home));
            harness.step();
            assert!(
                announces(&harness, catalog.text(TextKey::SessionMembershipIncluded)),
                "Advanced information adds nothing to a receiver row: {:?}",
                announced(&harness)
            );

            reduce(
                harness.state_mut(),
                AppEvent::AdvancedInformationChanged(false),
            );
            harness.step();
            assert!(
                !announces(&harness, catalog.text(TextKey::SessionMembershipIncluded)),
                "the extra detail survived the switch being turned off"
            );
        }
    }

    /// The global shortcut: capture, validate, apply.
    ///
    /// The key code used to be a `DragValue` clamped to 1..=254, so
    /// `TextKey::ShortcutInvalidKey` -- a sentence the catalog promises the
    /// user -- could not be reached by any input at all. A field the user can
    /// actually type a wrong value into is what makes the rejection real.
    mod shortcut {
        use super::*;

        use crate::app::HotkeyBinding;

        const KEY_FIELD: accesskit::Role = accesskit::Role::TextInput;

        fn settings_page(locale: ResolvedLocale) -> PageHarness {
            page_harness(Page::Settings, locale, TALL_CANVAS, populated_state())
        }

        /// Replaces whatever the key field holds with `text`.
        fn retype_key_code(harness: &mut PageHarness, locale: ResolvedLocale, text: &str) {
            let name = Catalog::new(locale).text(TextKey::ShortcutKeyAccessibility);
            harness.get_by_role_and_label(KEY_FIELD, name).focus();
            harness.step();
            harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            harness.step();
            harness
                .get_by_role_and_label(KEY_FIELD, name)
                .type_text(text);
            harness.step();
        }

        fn announced(harness: &PageHarness) -> Vec<String> {
            crate::ui::components::test_support::accessible_strings(harness.root())
        }

        fn announces(harness: &PageHarness, text: &str) -> bool {
            announced(harness).iter().any(|shown| shown.contains(text))
        }

        /// Any run of digits standing on its own in `line`.
        fn names_the_bare_code(line: &str, code: &str) -> bool {
            line.split(|c: char| !c.is_ascii_digit())
                .any(|token| token == code)
        }

        /// The field holds the key, not the number Windows files it under.
        ///
        /// Naming the *applied* chord after its key was only half of it: the
        /// control the user actually edits still read "72" under a label that
        /// said "virtual key code", which is section 3 point 9 -- an
        /// implementation-near label -- sitting on the one control the whole
        /// group exists for.
        #[test]
        fn the_key_field_holds_the_key_rather_than_its_windows_code() {
            for locale in LOCALES {
                let harness = settings_page(locale);
                let name = Catalog::new(locale).text(TextKey::ShortcutKeyAccessibility);
                let node = harness.get_by_role_and_label(KEY_FIELD, name);
                assert_eq!(
                    node.accesskit_node().value().as_deref(),
                    Some("H"),
                    "{locale:?}: the field reports {:?}",
                    node.accesskit_node().value()
                );
            }
        }

        /// Nothing in the group -- label, field, tooltip, or applied chord --
        /// reports the key as its Windows number.
        #[test]
        fn the_shortcut_group_never_shows_the_bare_windows_code() {
            let code = HotkeyBinding::default().virtual_key.to_string();
            for locale in LOCALES {
                let harness = settings_page(locale);
                let seen = announced(&harness)
                    .into_iter()
                    .chain(crate::ui::components::test_support::painted_strings(
                        &harness,
                    ))
                    .find(|line| names_the_bare_code(line, &code));
                assert_eq!(
                    seen, None,
                    "{locale:?}: the page still reports the key as {code}"
                );
            }
        }

        /// A key typed by name reaches the applied chord as its code.
        #[test]
        fn a_key_typed_by_name_applies_as_exactly_one_hotkey_changed() {
            for (typed, expected) in [("F5", 0x74_u32), ("h", 0x48), ("Q", 0x51), ("7", 0x37)] {
                let locale = ResolvedLocale::English;
                let catalog = Catalog::new(locale);
                let mut harness = settings_page(locale);
                retype_key_code(&mut harness, locale, typed);
                harness
                    .get_by_label(catalog.text(TextKey::ApplyShortcut))
                    .click();
                harness.step();
                if expected == HotkeyBinding::default().virtual_key {
                    // "h" is the applied chord already: a draft equal to what
                    // is applied is not a change and must dispatch nothing.
                    assert!(harness.state().is_empty(), "{typed}");
                    continue;
                }
                assert_eq!(
                    *harness.state(),
                    vec![AppEvent::HotkeyChanged(HotkeyBinding {
                        enabled: true,
                        modifiers: crate::app::MOD_CONTROL | crate::app::MOD_ALT,
                        virtual_key: expected,
                    })],
                    "{typed}"
                );
            }
        }

        /// Rejection is localized, the command stays unusable, and nothing
        /// leaves the page.
        #[test]
        fn an_unparsable_key_code_is_rejected_in_the_users_language_without_dispatching() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness = settings_page(locale);
                retype_key_code(&mut harness, locale, "zz");

                assert!(
                    announces(&harness, catalog.text(TextKey::ShortcutInvalidKey)),
                    "{locale:?}: no localized rejection: {:?}",
                    announced(&harness)
                );
                let apply = harness.get_by_label(catalog.text(TextKey::ApplyShortcut));
                assert!(
                    apply.accesskit_node().is_disabled(),
                    "{locale:?}: Apply stayed usable on an invalid chord"
                );
                apply.click();
                harness.step();
                assert!(harness.state().is_empty(), "{locale:?}");
            }
        }

        #[test]
        fn an_out_of_range_key_code_is_rejected_without_dispatching() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness = settings_page(locale);
                retype_key_code(&mut harness, locale, "999");

                assert!(
                    announces(&harness, catalog.text(TextKey::ShortcutInvalidKey)),
                    "{locale:?}: 999 was accepted: {:?}",
                    announced(&harness)
                );
                harness
                    .get_by_label(catalog.text(TextKey::ApplyShortcut))
                    .click();
                harness.step();
                assert!(harness.state().is_empty(), "{locale:?}");
            }
        }

        /// A typed key code really reaches the applied chord.
        #[test]
        fn a_typed_key_code_applies_as_exactly_one_hotkey_changed() {
            let locale = ResolvedLocale::English;
            let catalog = Catalog::new(locale);
            let mut harness = settings_page(locale);
            retype_key_code(&mut harness, locale, "65");

            harness
                .get_by_label(catalog.text(TextKey::ApplyShortcut))
                .click();
            harness.step();

            assert_eq!(
                *harness.state(),
                vec![AppEvent::HotkeyChanged(HotkeyBinding {
                    enabled: true,
                    modifiers: crate::app::MOD_CONTROL | crate::app::MOD_ALT,
                    virtual_key: 65,
                })]
            );
        }

        /// Escape discards the transient edit and nothing else.
        #[test]
        fn escape_returns_the_draft_to_the_applied_chord_without_dispatching() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let mut harness = settings_page(locale);
                retype_key_code(&mut harness, locale, "65");
                assert!(
                    announces(&harness, catalog.text(TextKey::ShortcutChangePending)),
                    "{locale:?}: the edit was never pending"
                );

                harness.key_press(egui::Key::Escape);
                harness.step();
                harness.step();

                assert!(
                    !announces(&harness, catalog.text(TextKey::ShortcutChangePending)),
                    "{locale:?}: Escape left the draft dirty: {:?}",
                    announced(&harness)
                );
                assert!(harness.state().is_empty(), "{locale:?}");
            }
        }
    }

    /// About: the embedded documents, shown and copied for real.
    mod about {
        use super::*;

        #[test]
        fn support_button_opens_only_the_confirmed_paypal_link_after_a_click() {
            for (locale, label) in [
                (ResolvedLocale::German, "Mit PayPal unterstützen"),
                (ResolvedLocale::English, "Support with PayPal"),
            ] {
                let mut harness = settings_page(locale);
                harness.step();
                assert!(harness.output().platform_output.commands.is_empty());
                harness.get_by_label(label).click();
                harness.step();
                let urls: Vec<_> = harness
                    .output()
                    .platform_output
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        egui::OutputCommand::OpenUrl(url) => Some(url.url.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(urls, ["https://paypal.me/ttk95"], "{locale:?}");
                assert!(
                    harness.state().is_empty(),
                    "support must not control playback"
                );
                harness.step();
                assert!(harness.output().platform_output.commands.is_empty());
            }
        }

        /// The repository files, read from disk at test time.
        ///
        /// Deliberately *not* the `include_str!` the renderer uses: comparing
        /// the page against the same constant the page reads would assert
        /// nothing. Reading the file independently is what proves the embedded
        /// copy is the repository's licence and notices.
        fn repository_document(name: &str) -> String {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join(name);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        }

        fn first_line(document: &str) -> String {
            document
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .expect("a document has a first line")
                .to_owned()
        }

        fn settings_page(locale: ResolvedLocale) -> PageHarness {
            page_harness(Page::Settings, locale, TALL_CANVAS, populated_state())
        }

        fn announced(harness: &PageHarness) -> Vec<String> {
            crate::ui::components::test_support::accessible_strings(harness.root())
        }

        fn announces(harness: &PageHarness, text: &str) -> bool {
            announced(harness).iter().any(|shown| shown.contains(text))
        }

        fn copied_text(harness: &PageHarness) -> Vec<String> {
            harness
                .output()
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        }

        const DOCUMENTS: [(&str, TextKey, TextKey); 2] = [
            ("LICENSE", TextKey::OpenLicense, TextKey::CopyLicenseText),
            (
                "THIRD_PARTY_NOTICES.md",
                TextKey::OpenThirdPartyNotices,
                TextKey::CopyNoticesText,
            ),
        ];

        #[test]
        fn open_shows_the_embedded_document_and_escape_dismisses_it() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                for (name, open, _) in DOCUMENTS {
                    let head = first_line(&repository_document(name));
                    let mut harness = settings_page(locale);
                    assert!(
                        !announces(&harness, &head),
                        "{locale:?}/{name}: the viewer is open before it was asked for"
                    );

                    harness.get_by_label(catalog.text(open)).click();
                    harness.step();
                    assert!(
                        announces(&harness, &head),
                        "{locale:?}/{name}: Open showed nothing"
                    );
                    assert!(
                        announces(&harness, catalog.text(TextKey::CloseAboutViewer)),
                        "{locale:?}/{name}: the viewer cannot be closed"
                    );

                    harness.key_press(egui::Key::Escape);
                    harness.step();
                    harness.step();
                    assert!(
                        !announces(&harness, &head),
                        "{locale:?}/{name}: Escape did not dismiss the viewer"
                    );
                    assert!(
                        harness.state().is_empty(),
                        "{locale:?}/{name}: reading a document is not a domain event"
                    );
                }
            }
        }

        #[test]
        fn the_close_command_dismisses_the_viewer_too() {
            let locale = ResolvedLocale::German;
            let catalog = Catalog::new(locale);
            let head = first_line(&repository_document("LICENSE"));
            let mut harness = settings_page(locale);

            harness
                .get_by_label(catalog.text(TextKey::OpenLicense))
                .click();
            harness.step();
            assert!(announces(&harness, &head));

            harness
                .get_by_label(catalog.text(TextKey::CloseAboutViewer))
                .click();
            harness.step();
            harness.step();
            assert!(!announces(&harness, &head));
            assert!(harness.state().is_empty());
        }

        /// The clipboard receives the document, not a path to it.
        #[test]
        fn copy_puts_the_whole_repository_document_on_the_clipboard() {
            for (name, _, copy) in DOCUMENTS {
                let document = repository_document(name);
                let catalog = Catalog::new(ResolvedLocale::English);
                let mut harness = settings_page(ResolvedLocale::English);

                harness.get_by_label(catalog.text(copy)).click();
                harness.step();

                let copied = copied_text(&harness);
                assert_eq!(copied.len(), 1, "{name}: {copied:?}");
                assert_eq!(
                    copied[0].replace("\r\n", "\n"),
                    document.replace("\r\n", "\n"),
                    "{name}: the clipboard did not get the repository document"
                );
                assert!(
                    announces(&harness, catalog.text(TextKey::CopiedToClipboard)),
                    "{name}: the copy was silent"
                );
            }
        }

        /// Nothing on the closed page looks like a filesystem path.
        #[test]
        fn the_about_group_never_announces_a_build_or_user_path() {
            for locale in LOCALES {
                let harness = settings_page(locale);
                for text in announced(&harness) {
                    for needle in [":\\", "\\\\", "/target/", "CARGO_MANIFEST_DIR", ".rs"] {
                        assert!(
                            !text.contains(needle),
                            "{locale:?}: {text:?} carries {needle:?}"
                        );
                    }
                }
            }
        }

        /// The one node carrying a whole embedded document.
        fn document_node<'h>(harness: &'h PageHarness, head: &str) -> egui_kittest::Node<'h> {
            let mut found: Vec<egui_kittest::Node<'h>> = harness
                .root()
                .children_recursive()
                .filter(|node| {
                    node.accesskit_node()
                        .value()
                        .is_some_and(|value| value.contains(head))
                })
                .collect();
            assert_eq!(
                found.len(),
                1,
                "{head:?}: {} nodes carry this text",
                found.len()
            );
            found.remove(0)
        }

        /// How far down the viewport the document currently begins. It falls
        /// as the reader scrolls, because the text moves up past the top of
        /// the scroll area.
        fn document_top(harness: &PageHarness, head: &str) -> f32 {
            document_node(harness, head).rect().top()
        }

        /// Each document opens at its own beginning.
        ///
        /// egui keeps a scroll offset in context memory under the id the
        /// area's salt produces, so a salt shared by both documents hands the
        /// place reached in the long licence to the short notices. The reader
        /// then lands past their beginning with nothing on screen admitting
        /// that text was skipped.
        #[test]
        fn switching_documents_shows_the_new_one_from_its_beginning() {
            let locale = ResolvedLocale::English;
            let catalog = Catalog::new(locale);
            let licence = first_line(&repository_document("LICENSE"));
            let notices = first_line(&repository_document("THIRD_PARTY_NOTICES.md"));

            // Where the notices begin when nothing was read before them.
            let mut untouched = settings_page(locale);
            untouched
                .get_by_label(catalog.text(TextKey::OpenThirdPartyNotices))
                .click();
            untouched.step();
            let unread = document_top(&untouched, &notices);

            // The same document, reached after the licence was scrolled away
            // from its own beginning.
            let mut harness = settings_page(locale);
            harness
                .get_by_label(catalog.text(TextKey::OpenLicense))
                .click();
            harness.step();
            let resting = document_top(&harness, &licence);
            for _ in 0..4 {
                document_node(&harness, &licence).scroll_down();
                harness.step();
            }
            let scrolled = document_top(&harness, &licence);
            assert!(
                scrolled < resting - 1.0,
                "the licence never moved, so this test would prove nothing: \
                 {resting} -> {scrolled}"
            );

            harness
                .get_by_label(catalog.text(TextKey::OpenThirdPartyNotices))
                .click();
            harness.step();
            let after = document_top(&harness, &notices);
            assert!(
                (after - unread).abs() < 1.0,
                "the notices opened at {after} instead of {unread}: the place reached in the \
                 licence carried over into a different document"
            );
        }

        /// Escape belongs to the viewer while the viewer is open; the hotkey
        /// draft only gets it back once the viewer is gone. Without that
        /// arbitration one key press would silently do two things.
        #[test]
        fn escape_closes_the_viewer_before_it_touches_the_hotkey_draft() {
            let locale = ResolvedLocale::English;
            let catalog = Catalog::new(locale);
            let head = first_line(&repository_document("LICENSE"));
            let mut harness = settings_page(locale);

            harness
                .get_by_label(catalog.text(TextKey::ShortcutAlt))
                .click();
            harness.step();
            harness
                .get_by_label(catalog.text(TextKey::OpenLicense))
                .click();
            harness.step();
            assert!(announces(&harness, &head));

            harness.key_press(egui::Key::Escape);
            harness.step();
            harness.step();
            assert!(!announces(&harness, &head), "the viewer stayed open");
            assert!(
                announces(&harness, catalog.text(TextKey::ShortcutChangePending)),
                "the same Escape also discarded the chord edit"
            );

            harness.key_press(egui::Key::Escape);
            harness.step();
            harness.step();
            assert!(
                !announces(&harness, catalog.text(TextKey::ShortcutChangePending)),
                "the second Escape did not reach the chord edit"
            );
            assert!(harness.state().is_empty());
        }
    }

    /// Shape, order, target size, and keyboard focus of the Settings controls.
    ///
    /// Task 7 shipped an invisible keyboard focus because the theme mapping
    /// was checked and the individual controls were not. Everything here is
    /// therefore measured on the *painted frame* of each control, one control
    /// at a time.
    mod settings_controls {
        use super::*;

        use crate::ui::theme::contrast_ratio;

        /// One control of the page, by the query that reaches it.
        struct Control {
            what: &'static str,
            role: accesskit::Role,
            label: String,
            /// Whether the control only exists in a usable state once the
            /// chord draft differs from the applied binding. Apply is such a
            /// control: a disabled command senses nothing, so it is correctly
            /// not a tab stop, and asking it to take focus proves nothing.
            needs_a_pending_chord: bool,
        }

        fn control(what: &'static str, role: accesskit::Role, label: &str) -> Control {
            Control {
                what,
                role,
                label: label.to_owned(),
                needs_a_pending_chord: false,
            }
        }

        fn qualified(catalog: Catalog, group: TextKey, choice: TextKey) -> String {
            catalog.named_value(NamedValueArgs {
                name: catalog.text(group),
                value: catalog.text(choice),
            })
        }

        /// Every focusable control the page offers, in the order the
        /// specification puts the groups in.
        fn controls(locale: ResolvedLocale) -> Vec<Control> {
            use accesskit::Role::{Button, CheckBox, RadioButton, TextInput};
            let catalog = Catalog::new(locale);
            let mut controls = Vec::new();
            for choice in [
                TextKey::ThemeSystem,
                TextKey::ThemeLight,
                TextKey::ThemeDark,
            ] {
                controls.push(control(
                    "appearance choice",
                    RadioButton,
                    &qualified(catalog, TextKey::Appearance, choice),
                ));
            }
            for choice in [
                TextKey::LanguageSystem,
                TextKey::LanguageGerman,
                TextKey::LanguageEnglish,
            ] {
                controls.push(control(
                    "language choice",
                    RadioButton,
                    &qualified(catalog, TextKey::Language, choice),
                ));
            }
            for key in [
                TextKey::ShortcutEnabled,
                TextKey::ShortcutControl,
                TextKey::ShortcutAlt,
                TextKey::ShortcutShift,
                TextKey::ShortcutWindows,
            ] {
                controls.push(control("shortcut modifier", CheckBox, catalog.text(key)));
            }
            controls.push(control(
                "key code field",
                TextInput,
                catalog.text(TextKey::ShortcutKeyAccessibility),
            ));
            controls.push(Control {
                needs_a_pending_chord: true,
                ..control(
                    "apply shortcut",
                    Button,
                    catalog.text(TextKey::ApplyShortcut),
                )
            });
            controls.push(control(
                "advanced switch",
                CheckBox,
                catalog.text(TextKey::AdvancedInformation),
            ));
            // Document by document, each with its own pair of commands.
            for key in [
                TextKey::OpenLicense,
                TextKey::CopyLicenseText,
                TextKey::OpenThirdPartyNotices,
                TextKey::CopyNoticesText,
            ] {
                controls.push(control("about command", Button, catalog.text(key)));
            }
            controls
        }

        /// Every stroke the last frame painted over `area`.
        ///
        /// Only shapes that belong to the control are collected: a shape has
        /// to overlap it and stay inside its immediate surroundings, so a
        /// neighbour's border cannot be mistaken for this control's ring.
        fn strokes_over(harness: &PageHarness, area: egui::Rect) -> Vec<egui::Stroke> {
            fn collect(shape: &egui::Shape, area: egui::Rect, out: &mut Vec<egui::Stroke>) {
                let neighbourhood = area.expand(6.0);
                match shape {
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, area, out);
                        }
                    }
                    egui::Shape::Rect(rect) => {
                        if rect.stroke.width > 0.0
                            && rect.rect.intersects(area)
                            && neighbourhood.contains_rect(rect.rect)
                        {
                            out.push(rect.stroke);
                        }
                    }
                    egui::Shape::Circle(circle) => {
                        let bounds = circle.visual_bounding_rect();
                        if circle.stroke.width > 0.0
                            && bounds.intersects(area)
                            && neighbourhood.contains_rect(bounds)
                        {
                            out.push(circle.stroke);
                        }
                    }
                    _ => {}
                }
            }

            let mut found = Vec::new();
            for clipped in &harness.output().shapes {
                collect(&clipped.shape, area, &mut found);
            }
            found
        }

        /// The colour of the largest filled rectangle the frame painted behind
        /// `area` -- the surface the focus ring actually has to stand out
        /// against, taken from the frame rather than from a token.
        fn surface_behind(harness: &PageHarness, area: egui::Rect) -> egui::Color32 {
            fn collect(shape: &egui::Shape, area: egui::Rect, out: &mut Vec<(f32, egui::Color32)>) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, area, out);
                        }
                    }
                    egui::Shape::Rect(rect)
                        if rect.fill.a() == 255 && rect.rect.contains_rect(area) =>
                    {
                        out.push((rect.rect.area(), rect.fill));
                    }
                    _ => {}
                }
            }

            let mut found = Vec::new();
            for clipped in &harness.output().shapes {
                collect(&clipped.shape, area, &mut found);
            }
            found
                .into_iter()
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, fill)| fill)
                .expect("the page paints a background behind its controls")
        }

        fn widest(strokes: &[egui::Stroke]) -> f32 {
            strokes
                .iter()
                .map(|stroke| stroke.width)
                .fold(0.0, f32::max)
        }

        /// Keyboard focus has to be visible on *every* control of the page,
        /// in both themes, and it has to survive without colour.
        ///
        /// Nothing here is compared against the token the renderer reads. The
        /// resting frame and the focused frame are both measured, and the
        /// claim is about the difference between them.
        #[test]
        fn every_control_paints_a_wider_differently_coloured_ring_while_it_holds_focus() {
            for (theme, appearance) in [
                (ThemePreference::Light, TEST_APPEARANCE),
                (ThemePreference::Dark, TEST_APPEARANCE),
                // High Contrast is not a fourth preference: Windows imposes
                // it on whatever the user chose, so it is checked as an
                // appearance over the System preference.
                (ThemePreference::System, HIGH_CONTRAST_BLACK),
            ] {
                for locale in LOCALES {
                    for control in controls(locale) {
                        let mut harness = styled_page_harness(
                            Page::Settings,
                            locale,
                            TALL_CANVAS,
                            populated_state(),
                            theme,
                            appearance,
                        );
                        let Control {
                            what,
                            role,
                            label,
                            needs_a_pending_chord,
                        } = &control;
                        if *needs_a_pending_chord {
                            // Arm the command by making the draft differ from
                            // the applied chord. The click leaves focus on the
                            // checkbox that did it, not on the control under
                            // test, so the resting measurement stays clean.
                            harness
                                .get_by_label(Catalog::new(locale).text(TextKey::ShortcutShift))
                                .click();
                            harness.step();
                        }
                        let rect = harness.get_by_role_and_label(*role, label).rect();
                        let resting = strokes_over(&harness, rect);

                        harness.get_by_role_and_label(*role, label).focus();
                        harness.step();
                        harness.step();
                        let node = harness.get_by_role_and_label(*role, label);
                        assert!(
                            node.is_focused(),
                            "{theme:?}/{locale:?}: {what} {label:?} cannot take keyboard focus"
                        );
                        let rect = node.rect();
                        let focused = strokes_over(&harness, rect);

                        let resting_width = widest(&resting);
                        let ring = focused
                            .iter()
                            .copied()
                            .filter(|stroke| stroke.width >= resting_width + 1.0)
                            .max_by(|a, b| a.width.total_cmp(&b.width));
                        let ring = ring.unwrap_or_else(|| {
                            panic!(
                                "{theme:?}/{locale:?}: {what} {label:?} paints no ring wider than \
                                 its {resting_width} point resting border: {focused:?}"
                            )
                        });

                        let resting_colours: Vec<egui::Color32> =
                            resting.iter().map(|stroke| stroke.color).collect();
                        assert!(
                            !resting_colours.contains(&ring.color),
                            "{theme:?}/{locale:?}: {what} {label:?} focus ring reuses the resting \
                             colour {:?}",
                            ring.color
                        );

                        // WCAG 1.4.11 asks 3:1 for a non-text indicator. The
                        // number is the specification's, both colours come
                        // from the painted frame.
                        let behind = surface_behind(&harness, rect);
                        let ratio = contrast_ratio(ring.color, behind);
                        assert!(
                            ratio >= 3.0,
                            "{theme:?}/{locale:?}: {what} {label:?} ring {:?} against {behind:?} \
                             is {ratio:.3}:1",
                            ring.color
                        );
                    }
                }
            }
        }

        /// The guard has to be able to fail: a frame with no ring at all must
        /// not satisfy it.
        #[test]
        fn the_focus_measurement_rejects_a_control_that_paints_no_ring() {
            let strokes = vec![egui::Stroke::new(1.0, egui::Color32::from_rgb(1, 2, 3))];
            let resting_width = widest(&strokes);
            assert!(strokes
                .iter()
                .copied()
                .filter(|stroke| stroke.width >= resting_width + 1.0)
                .max_by(|a, b| a.width.total_cmp(&b.width))
                .is_none());
        }

        /// Every control is a 40-point target, at both window sizes and in
        /// both languages.
        #[test]
        fn every_control_meets_the_40_point_target() {
            for locale in LOCALES {
                for size in TARGET_SIZES {
                    let harness = page_harness(Page::Settings, locale, size, populated_state());
                    for Control {
                        what, role, label, ..
                    } in controls(locale)
                    {
                        let rect = harness.get_by_role_and_label(role, &label).rect();
                        // The literal is the specification's number, not the
                        // module's constant.
                        assert!(
                            rect.height() >= 40.0,
                            "{locale:?}/{size:?}: {what} {label:?} is {} points tall",
                            rect.height()
                        );
                    }
                }
            }
        }

        /// The groups appear in the order the specification lists for the
        /// capabilities that really exist: Appearance, Language, Keyboard,
        /// Advanced information, About. Behavior and Connection are absent
        /// because no typed shell capability backs them.
        #[test]
        fn the_groups_appear_in_the_supported_specification_order() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness =
                    page_harness(Page::Settings, locale, TARGET_SIZES[0], populated_state());
                let mut previous = f32::MIN;
                for key in [
                    TextKey::Appearance,
                    TextKey::Language,
                    TextKey::Keyboard,
                    TextKey::AdvancedInformation,
                    TextKey::About,
                ] {
                    let title = catalog.text(key);
                    let top = harness.get_by_value(title).rect().top();
                    assert!(
                        top > previous,
                        "{locale:?}: {title:?} is out of order at {top}"
                    );
                    previous = top;
                }
            }
        }

        /// The keyboard order is the visual order, and it is the same in both
        /// languages.
        #[test]
        fn the_keyboard_order_is_the_visual_order_in_both_languages() {
            let mut shapes_per_locale = Vec::new();
            for locale in LOCALES {
                let harness =
                    page_harness(Page::Settings, locale, TARGET_SIZES[0], populated_state());
                let mut previous = f32::MIN;
                let mut roles = Vec::new();
                for Control {
                    what, role, label, ..
                } in controls(locale)
                {
                    let rect = harness.get_by_role_and_label(role, &label).rect();
                    assert!(
                        rect.top() >= previous - 1.0,
                        "{locale:?}: {what} {label:?} sits above the control before it"
                    );
                    previous = rect.top();
                    roles.push(role);
                }
                shapes_per_locale.push(roles);
            }
            assert_eq!(
                shapes_per_locale[0], shapes_per_locale[1],
                "the two languages offer a different sequence of controls"
            );
        }
    }

    /// Section 5.3 geometry on the Settings destination, and the one control
    /// that was drawn as a bare mark.
    mod settings_surface {
        use super::*;
        use crate::ui::components::test_support::{painted_rects, painted_strings};
        use crate::ui::presentation::{CARD_PADDING, CARD_RADIUS, CONTROL_MIN_HEIGHT};

        const GROUPS: [TextKey; 5] = [
            TextKey::Appearance,
            TextKey::Language,
            TextKey::Keyboard,
            TextKey::AdvancedInformation,
            TextKey::About,
        ];

        /// Every group sits on a card of the specified geometry.
        ///
        /// The five groups used to be set text running down the canvas with
        /// no surface, no rule, and no rhythm -- five headings and their
        /// controls indistinguishable from one continuous column. The card is
        /// what separates them, and its radius and padding are section 5.3's,
        /// measured from the paint rather than from the constants the
        /// renderer reads.
        #[test]
        fn every_settings_group_sits_on_a_card_of_the_specified_geometry() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = page_harness(Page::Settings, locale, TALL_CANVAS, populated_state());
                let radius = egui::CornerRadius::same(CARD_RADIUS as u8);
                let painted = painted_rects(&harness);

                let mut cards = Vec::new();
                for key in GROUPS {
                    let title = harness.get_by_value(catalog.text(key)).rect();
                    let card = painted
                        .iter()
                        .filter(|(rect, corner, _)| *corner == radius && rect.contains_rect(title))
                        .min_by(|a, b| a.0.area().total_cmp(&b.0.area()));
                    let (card, _, _) = card.unwrap_or_else(|| {
                        panic!("{locale:?}: {:?} sits on no card at all", catalog.text(key))
                    });
                    assert!(
                        (title.left() - card.left() - CARD_PADDING).abs() <= 1.0,
                        "{locale:?}: {:?} is {} points inside its card, not {CARD_PADDING}",
                        catalog.text(key),
                        title.left() - card.left()
                    );
                    cards.push((catalog.text(key), *card));
                }

                // A column, not a staircase. The width each card claims is
                // the reason the renderer sets one at all -- an `egui::Frame`
                // measures itself against its content, so the group holding
                // one short radio row would otherwise end where that row
                // ends. Only the left edge was ever checked, and the left
                // edge is the one the layout gives away for free.
                let (widest, reference) = cards
                    .iter()
                    .max_by(|a, b| a.1.right().total_cmp(&b.1.right()))
                    .copied()
                    .expect("the page has groups");
                for (name, card) in &cards {
                    assert!(
                        (card.right() - reference.right()).abs() <= 1.0,
                        "{locale:?}: {name:?} ends at {}, {widest:?} at {}",
                        card.right(),
                        reference.right()
                    );
                    assert!(
                        (card.left() - reference.left()).abs() <= 1.0,
                        "{locale:?}: {name:?} starts at {}, {widest:?} at {}",
                        card.left(),
                        reference.left()
                    );
                }
            }
        }

        /// The Advanced-information control is a switch, drawn where the rest
        /// of its group starts.
        ///
        /// It used to be an unlabelled egui checkbox: a 14-point icon that the
        /// theme's 8-point corner radius rounded into a circle, indented past
        /// every other line of the page by the ambient button padding, with
        /// the state word floating beside it. It read as a control somebody
        /// had half finished.
        #[test]
        fn the_advanced_switch_is_a_track_at_the_left_edge_of_its_group() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = page_harness(Page::Settings, locale, TALL_CANVAS, populated_state());
                let control = harness
                    .get_by_role_and_label(
                        accesskit::Role::CheckBox,
                        catalog.text(TextKey::AdvancedInformation),
                    )
                    .rect();

                let marks: Vec<egui::Rect> = painted_rects(&harness)
                    .into_iter()
                    .filter(|(rect, _, _)| {
                        control.contains_rect(*rect) && rect.area() < control.area()
                    })
                    .map(|(rect, _, _)| rect)
                    .collect();
                assert!(
                    !marks.is_empty(),
                    "{locale:?}: the switch paints nothing inside its own target"
                );

                let left = marks
                    .iter()
                    .map(|rect| rect.left())
                    .fold(f32::MAX, f32::min);
                assert!(
                    (left - control.left()).abs() <= 1.0,
                    "{locale:?}: the switch is painted {} points inside its own target, so it no \
                     longer lines up with the group around it",
                    left - control.left()
                );

                let track = marks
                    .iter()
                    .max_by(|a, b| a.area().total_cmp(&b.area()))
                    .expect("a mark was found above");
                assert!(
                    track.width() > track.height(),
                    "{locale:?}: the switch paints {track:?}, which is not a track"
                );
                assert!(
                    control.height() >= CONTROL_MIN_HEIGHT,
                    "{locale:?}: the switch target is {} points tall",
                    control.height()
                );
            }
        }

        /// The Keyboard group opens with one heading, not two.
        ///
        /// The group title with the shortcut's own name set directly beneath
        /// it read as two section titles for one section. The shortcut names
        /// itself where it explains itself instead.
        #[test]
        fn the_keyboard_group_does_not_stack_two_headings() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = page_harness(Page::Settings, locale, TALL_CANVAS, populated_state());
                let painted = painted_strings(&harness);

                let heading = catalog.text(TextKey::GlobalShortcut);
                assert!(
                    !painted.iter().any(|text| text == heading),
                    "{locale:?}: {heading:?} is still set as a heading of its own"
                );
                let explained = catalog.named_value(NamedValueArgs {
                    name: heading,
                    value: catalog.text(TextKey::GlobalShortcutDescription),
                });
                assert!(
                    painted.iter().any(|text| text == &explained),
                    "{locale:?}: the shortcut no longer names itself: {painted:?}"
                );
            }
        }

        /// The applied chord names its key, not the number Windows uses for
        /// it.
        #[test]
        fn the_active_shortcut_names_the_key_rather_than_its_windows_code() {
            for locale in LOCALES {
                let catalog = Catalog::new(locale);
                let harness = page_harness(Page::Settings, locale, TALL_CANVAS, populated_state());
                let expected = catalog.named_value(NamedValueArgs {
                    name: catalog.text(TextKey::ActiveShortcut),
                    value: "Ctrl+Alt+H",
                });
                assert!(
                    painted_strings(&harness)
                        .iter()
                        .any(|text| text == &expected),
                    "{locale:?}: the page does not say {expected:?}"
                );
            }
        }
    }
}
