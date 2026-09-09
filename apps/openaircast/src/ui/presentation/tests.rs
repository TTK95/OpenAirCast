use super::*;

use crate::app::{ResolvedLocale, ThemePreference};
use crate::platform::{SystemAppearance, SystemColors};
use crate::ui::theme::{contrast_ratio, resolve_theme};

const SEVERITIES: &[Severity] = &[Severity::Info, Severity::Warning, Severity::Error];
const CODES: &[NoticeCode] = &[
    NoticeCode::NoReceiverAvailable,
    NoticeCode::DiscoveryFailed,
    NoticeCode::SessionFailed,
    NoticeCode::ControllerUnavailable,
    NoticeCode::PreferencesFailed,
    NoticeCode::HotkeyFailed,
    NoticeCode::InvalidInput,
];
const CORRECTIVES: &[Option<CorrectiveAction>] = &[
    None,
    Some(CorrectiveAction::Refresh),
    Some(CorrectiveAction::Retry),
    Some(CorrectiveAction::OpenSettings),
];
const LOCALES: &[ResolvedLocale] = &[ResolvedLocale::German, ResolvedLocale::English];

fn normal_appearance() -> SystemAppearance {
    SystemAppearance {
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
    }
}

/// The real Windows "Contrast Black" and "Contrast White" themes. Using
/// invented colours here would make the contrast assertions self-fulfilling.
fn contrast_black() -> SystemAppearance {
    SystemAppearance {
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
    }
}

fn contrast_white() -> SystemAppearance {
    SystemAppearance {
        client_animation_enabled: false,
        high_contrast: true,
        colors: SystemColors {
            background: [0xff, 0xff, 0xff],
            foreground: [0x00, 0x00, 0x00],
            highlight: [0x37, 0x00, 0x6e],
            highlight_text: [0xff, 0xff, 0xff],
            disabled_text: [0x60, 0x00, 0x00],
            link: [0x00, 0x00, 0xff],
        },
    }
}

/// Every theme the product ships, named for assertion messages.
fn all_token_sets() -> Vec<(&'static str, ThemeTokens)> {
    vec![
        (
            "Light",
            resolve_theme(ThemePreference::Light, None, normal_appearance()).tokens,
        ),
        (
            "Dark",
            resolve_theme(ThemePreference::Dark, None, normal_appearance()).tokens,
        ),
        (
            "Contrast Black",
            resolve_theme(ThemePreference::System, None, contrast_black()).tokens,
        ),
        (
            "Contrast White",
            resolve_theme(ThemePreference::System, None, contrast_white()).tokens,
        ),
    ]
}

mod one_filled_action {
    use super::*;

    /// Enumerates every context the rule can be asked about.
    fn every_context() -> Vec<(Option<LifecycleAction>, bool, bool)> {
        let mut contexts = Vec::new();
        let mut lifecycles: Vec<Option<LifecycleAction>> = vec![None];
        lifecycles.extend(LifecycleAction::ALL.iter().copied().map(Some));
        for lifecycle in lifecycles {
            for staged_dirty in [false, true] {
                for apply_safe in [false, true] {
                    contexts.push((lifecycle, staged_dirty, apply_safe));
                }
            }
        }
        contexts
    }

    /// Counts filled buttons the way a rendered page would: the lifecycle
    /// command plus the change bar's Apply, when the bar is on screen.
    fn filled_button_count(
        lifecycle: Option<LifecycleAction>,
        staged_dirty: bool,
        apply_safe: bool,
    ) -> usize {
        let owner = filled_action_owner(lifecycle, staged_dirty, apply_safe);
        let lifecycle_filled = lifecycle_emphasis(owner, lifecycle) == Some(Emphasis::Filled);
        let mut count = 0;
        if lifecycle_filled {
            count += 1;
        }
        if staged_dirty && apply_emphasis(owner) == Emphasis::Filled {
            count += 1;
        }
        count
    }

    #[test]
    fn no_context_ever_shows_two_filled_actions() {
        let mut filled_somewhere = 0;
        for (lifecycle, staged_dirty, apply_safe) in every_context() {
            let count = filled_button_count(lifecycle, staged_dirty, apply_safe);
            assert!(
                count <= 1,
                "{lifecycle:?} dirty={staged_dirty} safe={apply_safe} showed {count} filled actions"
            );
            filled_somewhere += count;
        }
        // A rule that fills nothing anywhere would satisfy the bound above
        // without being a rule at all.
        assert!(
            filled_somewhere > 0,
            "not a single context produced a filled action"
        );
    }

    #[test]
    fn every_context_with_an_enabled_command_has_exactly_one_filled_action() {
        for (lifecycle, staged_dirty, apply_safe) in every_context() {
            let has_enabled_lifecycle = lifecycle.map(LifecycleAction::is_enabled).unwrap_or(false);
            // Apply only claims the slot in a safe state: while a session
            // runs or a transition is in flight it stays quiet, so a
            // context offering no lifecycle command fills nothing at all.
            let expected = usize::from(has_enabled_lifecycle || (staged_dirty && apply_safe));
            assert_eq!(
                filled_button_count(lifecycle, staged_dirty, apply_safe),
                expected,
                "{lifecycle:?} dirty={staged_dirty} safe={apply_safe}"
            );
        }
    }

    #[test]
    fn a_running_session_keeps_stop_filled_and_makes_apply_quiet() {
        let owner = filled_action_owner(Some(LifecycleAction::Stop), true, false);
        assert_eq!(owner, FilledActionOwner::Lifecycle);
        assert_eq!(apply_emphasis(owner), Emphasis::Quiet);
    }

    #[test]
    fn a_transition_in_flight_keeps_cancel_and_stop_trying_filled() {
        for action in [LifecycleAction::Cancel, LifecycleAction::StopTrying] {
            assert_eq!(
                filled_action_owner(Some(action), true, false),
                FilledActionOwner::Lifecycle,
                "{action:?}"
            );
        }
    }

    #[test]
    fn a_stopped_context_with_staged_changes_gives_apply_the_filled_slot() {
        for action in [LifecycleAction::Start, LifecycleAction::Retry] {
            let owner = filled_action_owner(Some(action), true, true);
            assert_eq!(owner, FilledActionOwner::Apply, "{action:?}");
            assert_eq!(
                lifecycle_emphasis(owner, Some(action)),
                Some(Emphasis::Quiet),
                "{action:?}"
            );
        }
    }

    #[test]
    fn a_stopped_context_without_staged_changes_gives_start_the_filled_slot() {
        let owner = filled_action_owner(Some(LifecycleAction::Start), false, true);
        assert_eq!(owner, FilledActionOwner::Lifecycle);
        assert_eq!(
            lifecycle_emphasis(owner, Some(LifecycleAction::Start)),
            Some(Emphasis::Filled)
        );
    }

    #[test]
    fn stopping_owns_no_filled_command_and_stays_disabled() {
        for (dirty, safe) in [(false, false), (true, false), (false, true), (true, true)] {
            let owner = filled_action_owner(Some(LifecycleAction::DisabledStopping), dirty, safe);
            if dirty && safe {
                assert_eq!(owner, FilledActionOwner::Apply);
            } else {
                assert_eq!(owner, FilledActionOwner::None, "dirty={dirty} safe={safe}");
            }
            assert_eq!(
                lifecycle_emphasis(owner, Some(LifecycleAction::DisabledStopping)),
                Some(Emphasis::Quiet)
            );
            assert!(!LifecycleAction::DisabledStopping.is_enabled());
        }
    }

    #[test]
    fn the_change_bar_model_mirrors_the_owner_and_stays_actionable() {
        let catalog = Catalog::new(ResolvedLocale::English);
        let changes = ChangeSummaryArgs {
            added: 1,
            removed: 0,
        };
        let running = ChangeBarModel::new(
            catalog,
            changes,
            filled_action_owner(Some(LifecycleAction::Stop), true, false),
        );
        assert!(!running.apply_filled);
        assert!(running.apply_enabled);
        assert_eq!(running.apply_emphasis(), Emphasis::Quiet);

        let stopped = ChangeBarModel::new(
            catalog,
            changes,
            filled_action_owner(Some(LifecycleAction::Start), true, true),
        );
        assert!(stopped.apply_filled);
        assert_eq!(stopped.apply_emphasis(), Emphasis::Filled);

        let unchanged = ChangeBarModel::new(
            catalog,
            ChangeSummaryArgs {
                added: 0,
                removed: 0,
            },
            FilledActionOwner::None,
        );
        assert!(!unchanged.apply_enabled);
    }
}

mod contrast {
    use super::*;

    /// WCAG AA for body text.
    const TEXT_MINIMUM: f32 = 4.5;
    /// WCAG 1.4.11 for non-text state indicators.
    const NON_TEXT_MINIMUM: f32 = 3.0;

    fn assert_text(name: &str, theme: &str, foreground: Color32, background: Color32) {
        let ratio = contrast_ratio(foreground, background);
        assert!(
            ratio >= TEXT_MINIMUM,
            "{theme}: {name} is {ratio:.2}:1, below {TEXT_MINIMUM}:1"
        );
    }

    fn assert_non_text(name: &str, theme: &str, indicator: Color32, adjacent: Color32) {
        let ratio = contrast_ratio(indicator, adjacent);
        assert!(
            ratio >= NON_TEXT_MINIMUM,
            "{theme}: {name} is {ratio:.2}:1, below {NON_TEXT_MINIMUM}:1"
        );
    }

    #[test]
    fn every_enabled_command_foreground_meets_body_text_contrast() {
        for (theme, tokens) in all_token_sets() {
            let filled = action_colors(&tokens, Emphasis::Filled, true);
            assert_text(
                "filled command label",
                theme,
                filled.foreground,
                filled.fill.expect("a filled command has a fill"),
            );

            let quiet = action_colors(&tokens, Emphasis::Quiet, true);
            assert!(quiet.fill.is_none(), "{theme}: a quiet command has no fill");
            for (surface_name, surface) in [("canvas", tokens.canvas), ("surface", tokens.surface)]
            {
                assert_text(
                    &format!("quiet command label on {surface_name}"),
                    theme,
                    quiet.foreground,
                    surface,
                );
            }
        }
    }

    #[test]
    fn every_command_border_meets_non_text_contrast() {
        for (theme, tokens) in all_token_sets() {
            for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                let colors = action_colors(&tokens, emphasis, true);
                for (surface_name, surface) in
                    [("canvas", tokens.canvas), ("surface", tokens.surface)]
                {
                    assert_non_text(
                        &format!("{emphasis:?} command border on {surface_name}"),
                        theme,
                        colors.border,
                        surface,
                    );
                }
            }
        }
    }

    #[test]
    fn the_disabled_treatment_stays_legible_and_uses_the_disabled_token() {
        for (theme, tokens) in all_token_sets() {
            for emphasis in [Emphasis::Filled, Emphasis::Quiet] {
                let colors = action_colors(&tokens, emphasis, false);
                assert_eq!(
                    colors.foreground, tokens.disabled,
                    "{theme}: {emphasis:?} disabled must use the semantic disabled token"
                );
                let background = colors.fill.unwrap_or(tokens.surface);
                assert_non_text(
                    &format!("{emphasis:?} disabled label"),
                    theme,
                    colors.foreground,
                    background,
                );
            }
        }
    }

    #[test]
    fn every_notice_severity_meets_text_and_indicator_contrast() {
        for (theme, tokens) in all_token_sets() {
            for &severity in SEVERITIES {
                let colors = notice_colors(&tokens, severity);
                assert_text(
                    &format!("{severity:?} notice text"),
                    theme,
                    colors.foreground,
                    colors.background,
                );
                assert_non_text(
                    &format!("{severity:?} notice accent"),
                    theme,
                    colors.accent,
                    colors.background,
                );
            }
        }
    }

    #[test]
    fn the_focus_ring_meets_indicator_contrast_against_every_neighbour() {
        for (theme, tokens) in all_token_sets() {
            let ring = focus_ring(&tokens);
            for (surface_name, surface) in [
                ("canvas", tokens.canvas),
                ("surface", tokens.surface),
                ("surface_subtle", tokens.surface_subtle),
            ] {
                assert_non_text(
                    &format!("focus ring on {surface_name}"),
                    theme,
                    ring.color,
                    surface,
                );
            }
        }
    }

    #[test]
    fn body_text_meets_contrast_on_every_page_surface() {
        for (theme, tokens) in all_token_sets() {
            for (surface_name, surface) in [
                ("canvas", tokens.canvas),
                ("surface", tokens.surface),
                ("surface_subtle", tokens.surface_subtle),
            ] {
                assert_text(
                    &format!("body text on {surface_name}"),
                    theme,
                    tokens.ink,
                    surface,
                );
            }
        }
    }

    /// Secondary copy has to hold up on every surface it can land on, and
    /// `surface_subtle` is one of them whether a component chooses it or
    /// not: `apply_colors` wires `ink_muted` into egui's `weak_text_color`
    /// and `surface_subtle` into both backdrop fills, so egui pairs them by
    /// itself for hint text and striped rows.
    #[test]
    fn secondary_text_meets_contrast_on_every_page_surface() {
        for (theme, tokens) in all_token_sets() {
            for (surface_name, surface) in [
                ("canvas", tokens.canvas),
                ("surface", tokens.surface),
                ("surface_subtle", tokens.surface_subtle),
            ] {
                assert_text(
                    &format!("secondary text on {surface_name}"),
                    theme,
                    tokens.ink_muted,
                    surface,
                );
            }
        }
    }

    /// A guard against a self-fulfilling suite: the helpers must be able
    /// to fail. Grey on grey is below both thresholds.
    #[test]
    fn the_contrast_helpers_reject_an_indistinguishable_pair() {
        let grey = Color32::from_rgb(0x80, 0x80, 0x80);
        let nearly = Color32::from_rgb(0x86, 0x86, 0x86);
        let ratio = contrast_ratio(grey, nearly);
        assert!(ratio < NON_TEXT_MINIMUM, "ratio was {ratio:.2}:1");
        assert!(ratio < TEXT_MINIMUM);
    }
}

mod without_color {
    use super::*;

    #[test]
    fn every_severity_has_its_own_text_marker() {
        let mut seen: Vec<&'static str> = Vec::new();
        for &severity in SEVERITIES {
            let symbol = severity_symbol(severity);
            assert!(!symbol.trim().is_empty(), "{severity:?} has no marker");
            assert!(
                !seen.contains(&symbol),
                "{severity:?} reuses the marker {symbol:?}"
            );
            seen.push(symbol);
        }
    }

    #[test]
    fn high_contrast_severities_share_a_colour_so_only_the_marker_separates_them() {
        let tokens = resolve_theme(ThemePreference::System, None, contrast_black()).tokens;
        let warning = notice_colors(&tokens, Severity::Warning);
        let error = notice_colors(&tokens, Severity::Error);
        assert_eq!(
            warning.accent, error.accent,
            "High Contrast must not approximate a warning hue"
        );
        assert_ne!(
            severity_symbol(Severity::Warning),
            severity_symbol(Severity::Error)
        );
    }

    #[test]
    fn unknown_and_stale_carry_both_a_marker_and_a_word() {
        for &locale in LOCALES {
            let catalog = Catalog::new(locale);
            for &freshness in MetricFreshness::ALL {
                let has_marker = freshness.symbol().is_some();
                let has_word = freshness.label_key().is_some();
                assert_eq!(has_marker, has_word, "{freshness:?} in {locale:?}");
                if let Some(key) = freshness.label_key() {
                    assert!(!catalog.text(key).trim().is_empty());
                }
            }
            assert_ne!(
                MetricFreshness::Stale.symbol(),
                MetricFreshness::Unknown.symbol()
            );
        }
    }

    #[test]
    fn the_focus_ring_is_two_points_with_a_two_point_offset_and_three_in_high_contrast() {
        for (theme, tokens) in all_token_sets() {
            let ring = focus_ring(&tokens);
            let expected = if theme.starts_with("Contrast") {
                3.0
            } else {
                2.0
            };
            assert_eq!(ring.width, expected, "{theme}");
            assert_eq!(ring.offset, 2.0, "{theme}");
        }
    }

    #[test]
    fn a_disabled_command_differs_from_an_enabled_one_beyond_colour() {
        for (theme, tokens) in all_token_sets() {
            let enabled = action_colors(&tokens, Emphasis::Filled, true);
            let disabled = action_colors(&tokens, Emphasis::Filled, false);
            assert_ne!(enabled.fill, disabled.fill, "{theme}");
        }
    }
}

mod notices {
    use super::*;

    fn notice(code: NoticeCode, action: Option<CorrectiveAction>) -> UserNotice {
        UserNotice {
            severity: Severity::Error,
            code,
            summary: "os error 10054 at C:\\Users\\x\\preferences.json".into(),
            action,
        }
    }

    #[test]
    fn no_notice_model_ever_carries_the_backend_summary() {
        for &locale in LOCALES {
            let catalog = Catalog::new(locale);
            for &code in CODES {
                for &action in CORRECTIVES {
                    let source = notice(code, action);
                    let model = NoticeModel::from_notice(&source, catalog);
                    let mut rendered = model.message.clone();
                    if let Some(consequence) = &model.consequence {
                        rendered.push_str(consequence);
                    }
                    if let Some(action) = &model.action {
                        rendered.push_str(&action.label);
                    }
                    // An empty model would trivially contain no sentinel.
                    assert!(
                        rendered.len() > 20,
                        "{code:?}/{action:?} in {locale:?} rendered almost nothing: {rendered:?}"
                    );
                    let lowered = rendered.to_lowercase();
                    for sentinel in ["os error", "c:\\", "preferences.json", "10054"] {
                        assert!(
                            !lowered.contains(sentinel),
                            "{code:?}/{action:?} in {locale:?} leaked {sentinel:?}: {rendered}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_notice_model_is_complete_in_both_locales() {
        for &locale in LOCALES {
            let catalog = Catalog::new(locale);
            for &code in CODES {
                for &action in CORRECTIVES {
                    let model = NoticeModel::from_notice(&notice(code, action), catalog);
                    assert!(
                        !model.message.trim().is_empty(),
                        "{code:?} has no message in {locale:?}"
                    );
                    assert!(
                        model
                            .consequence
                            .as_deref()
                            .is_some_and(|c| !c.trim().is_empty()),
                        "{code:?}/{action:?} has no consequence in {locale:?}"
                    );
                    assert_eq!(
                        model.action.is_some(),
                        action.is_some(),
                        "{code:?}/{action:?} in {locale:?}"
                    );
                    if let Some(rendered) = &model.action {
                        assert!(!rendered.label.trim().is_empty());
                        assert_eq!(Some(rendered.corrective), action);
                    }
                }
            }
        }
    }

    #[test]
    fn a_failed_preference_write_uses_the_scoped_keys() {
        let catalog = Catalog::new(ResolvedLocale::English);
        let model = NoticeModel::from_notice(
            &notice(NoticeCode::PreferencesFailed, Some(CorrectiveAction::Retry)),
            catalog,
        );
        assert_eq!(model.message, catalog.text(TextKey::PreferenceSaveFailed));
        assert_eq!(
            model.consequence.as_deref(),
            Some(catalog.text(TextKey::PreferenceSaveConsequence))
        );
        assert_eq!(
            model.action.as_ref().map(|a| a.label.as_str()),
            Some(catalog.text(TextKey::PreferenceRetry)),
            "a bare Retry would not say what is being retried"
        );
    }

    #[test]
    fn each_notice_code_maps_to_its_own_sentence() {
        let catalog = Catalog::new(ResolvedLocale::English);
        let mut seen: Vec<String> = Vec::new();
        for &code in CODES {
            let message = catalog.text(message_key(code)).to_owned();
            assert!(!seen.contains(&message), "{code:?} reuses {message:?}");
            seen.push(message);
        }
    }

    #[test]
    fn the_severity_marker_follows_the_authoritative_severity() {
        let catalog = Catalog::new(ResolvedLocale::German);
        for &severity in SEVERITIES {
            let source = UserNotice {
                severity,
                code: NoticeCode::SessionFailed,
                summary: String::new(),
                action: None,
            };
            let model = NoticeModel::from_notice(&source, catalog);
            assert_eq!(model.severity, severity);
            // Comparing two empty markers would pass without a marker
            // existing at all.
            assert!(
                !model.symbol.trim().is_empty(),
                "{severity:?} has no marker"
            );
            assert_eq!(model.symbol, severity_symbol(severity));
        }
    }
}

mod empty_states_and_metrics {
    use super::*;

    #[test]
    fn an_empty_state_offers_at_most_one_action_and_only_a_real_one() {
        for &locale in LOCALES {
            let catalog = Catalog::new(locale);
            let with_action = EmptyStateModel::no_receivers(catalog);
            assert_eq!(
                with_action.action.as_ref().map(|a| a.event.clone()),
                Some(AppEvent::RefreshRequested)
            );

            for model in [
                EmptyStateModel::speakers(catalog),
                EmptyStateModel::groups(catalog),
                EmptyStateModel::audio(catalog),
                EmptyStateModel::diagnostics(catalog),
            ] {
                assert!(
                    model.action.is_none(),
                    "a page without a typed command must not offer one in {locale:?}"
                );
                assert!(!model.title.trim().is_empty());
                assert!(!model.body.trim().is_empty());
            }
        }
    }

    #[test]
    fn an_unknown_metric_shows_the_word_not_a_fabricated_zero() {
        for &locale in LOCALES {
            let catalog = Catalog::new(locale);
            let model = MetricTileModel::unknown(catalog, "Latency");
            assert_eq!(model.value, catalog.text(TextKey::Unknown));
            assert_ne!(model.value, "0");
            assert!(model.unit.is_none());
            assert_eq!(model.symbol, Some("?"));
        }
    }

    #[test]
    fn the_accessible_phrase_joins_label_value_unit_and_freshness() {
        let catalog = Catalog::new(ResolvedLocale::English);
        let live = MetricTileModel::measured(catalog, "Latency", "12", Some("ms".into()));
        assert_eq!(live.accessible_phrase(), "Latency: 12 ms");

        let stale = MetricTileModel::stale(catalog, "Latency", "12", Some("ms".into()));
        assert_eq!(stale.accessible_phrase(), "Latency: 12 ms (Stale)");

        let unknown = MetricTileModel::unknown(catalog, "Latency");
        assert_eq!(unknown.accessible_phrase(), "Latency: Unknown");
    }

    #[test]
    fn the_spec_metrics_are_the_values_the_renderers_use() {
        assert_eq!(CONTROL_MIN_HEIGHT, 40.0);
        assert_eq!(CONTROL_RADIUS, 8.0);
        assert_eq!(CARD_RADIUS, 12.0);
        assert_eq!(CARD_PADDING, 16.0);
        assert_eq!(DENSE_PADDING, 12.0);
        assert_eq!(SECTION_GAP, 16.0);
        assert_eq!(FOCUS_RING_OFFSET, 2.0);
    }
}
