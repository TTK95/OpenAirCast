//! The responsive layout mapping and the resources the shell resolves once
//! per frame.
//!
//! Every breakpoint lives here as a pure function of one number -- the window
//! width in logical points -- so the acceptance sizes can be checked without
//! a window, a renderer, or a frame. A renderer that decided a breakpoint
//! itself would only be observable by rendering it, and the two target sizes
//! would then be an eyeball claim rather than a test.

use crate::ui::i18n::Catalog;
use crate::ui::theme::ThemeTokens;

/// Window width at and above which the navigation rail shows labels.
pub const RAIL_BREAKPOINT: f32 = 1000.0;
/// Rail width with icon and label.
pub const EXPANDED_RAIL_WIDTH: f32 = 176.0;
/// Rail width with centered icons only.
pub const COMPACT_RAIL_WIDTH: f32 = 64.0;
/// Page inset at and above [`RAIL_BREAKPOINT`].
pub const WIDE_PAGE_INSET: f32 = 24.0;
/// Page inset below [`RAIL_BREAKPOINT`].
pub const NARROW_PAGE_INSET: f32 = 16.0;
/// Maximum readable measure for standard page content.
pub const MAX_READABLE_WIDTH: f32 = 920.0;
/// Gap between two receiver cards in the same row.
pub const RECEIVER_COLUMN_GAP: f32 = 16.0;
/// Narrowest receiver card that still fits its name, its device class, two
/// state words, and a 40-point control on one row without clipping.
///
/// The specification names the outcome (two columns at 1120 x 720, one at
/// 900 x 600) rather than this number; the number is what makes both outcomes
/// follow from the same rule instead of from a second hard-coded breakpoint.
pub const MIN_RECEIVER_CARD_WIDTH: f32 = 420.0;

/// How the navigation rail is presented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RailMode {
    /// Icon plus localized label.
    Expanded,
    /// Centered icon; the label survives as the accessible name and tooltip.
    Compact,
}

impl RailMode {
    /// The mode for a window of this width.
    pub fn for_window_width(window_width: f32) -> Self {
        if window_width >= RAIL_BREAKPOINT {
            Self::Expanded
        } else {
            Self::Compact
        }
    }

    /// The rail width this mode occupies.
    pub const fn width(self) -> f32 {
        match self {
            Self::Expanded => EXPANDED_RAIL_WIDTH,
            Self::Compact => COMPACT_RAIL_WIDTH,
        }
    }
}

/// Every layout decision the shell derives from the window width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutMetrics {
    pub rail_mode: RailMode,
    pub rail_width: f32,
    pub page_inset: f32,
    pub receiver_columns: usize,
}

impl LayoutMetrics {
    /// Derives every metric from the window width.
    pub fn for_window_width(window_width: f32) -> Self {
        let rail_mode = RailMode::for_window_width(window_width);
        let rail_width = rail_mode.width();
        let page_inset = if window_width >= RAIL_BREAKPOINT {
            WIDE_PAGE_INSET
        } else {
            NARROW_PAGE_INSET
        };
        let content = content_width(window_width, rail_width, page_inset);
        let receiver_columns = receiver_columns_for_content(content);
        Self {
            rail_mode,
            rail_width,
            page_inset,
            receiver_columns,
        }
    }

    /// The width the page content may occupy inside this window.
    pub fn content_width(&self, window_width: f32) -> f32 {
        content_width(window_width, self.rail_width, self.page_inset)
    }
}

/// How many receiver columns fit into a content area of this width.
///
/// The Overview grid asks this directly, because inside the page's scroll
/// region it knows its own measure but not the window width. Deriving both
/// answers from one function is what keeps the grid and [`LayoutMetrics`]
/// from disagreeing at the breakpoint.
pub fn receiver_columns_for_content(content_width: f32) -> usize {
    let two_column_card = (content_width - RECEIVER_COLUMN_GAP) / 2.0;
    if two_column_card >= MIN_RECEIVER_CARD_WIDTH {
        2
    } else {
        1
    }
}

fn content_width(window_width: f32, rail_width: f32, page_inset: f32) -> f32 {
    (window_width - rail_width - 2.0 * page_inset).max(0.0)
}

/// The readable measure for standard page content: capped, never stretched.
pub fn readable_width(content_width: f32) -> f32 {
    content_width.min(MAX_READABLE_WIDTH)
}

/// The left inset that centres the readable measure in the space available.
///
/// The cap alone left the content pinned against the rail with every point of
/// slack piled up on the right: at 1275 points the cards stopped short of the
/// window's last 145 points and nothing at all lived there. Capping the
/// measure is right; deciding that the leftover belongs entirely to one side
/// is not.
pub fn readable_inset(content_width: f32) -> f32 {
    ((content_width - readable_width(content_width)) / 2.0).max(0.0)
}

/// What every renderer below the shell is allowed to resolve text and colour
/// from, and nothing else.
///
/// Handing this down instead of a handle is what keeps a `Device`, a socket,
/// a session, or a backend diagnostic object structurally unable to reach a
/// renderer: there is no field through which one could arrive.
pub struct UiResources<'a> {
    pub tokens: &'a ThemeTokens,
    pub catalog: Catalog,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two window sizes the specification names as acceptance targets.
    const WIDE: f32 = 1120.0;
    const NARROW: f32 = 900.0;

    #[test]
    fn rail_mode_switches_at_one_thousand_logical_points() {
        assert_eq!(RailMode::for_window_width(WIDE), RailMode::Expanded);
        assert_eq!(
            RailMode::for_window_width(RAIL_BREAKPOINT),
            RailMode::Expanded
        );
        assert_eq!(
            RailMode::for_window_width(RAIL_BREAKPOINT - 0.5),
            RailMode::Compact
        );
        assert_eq!(RailMode::for_window_width(NARROW), RailMode::Compact);
    }

    #[test]
    fn rail_widths_are_176_and_64() {
        assert_eq!(RailMode::Expanded.width(), 176.0);
        assert_eq!(RailMode::Compact.width(), 64.0);
        assert_eq!(LayoutMetrics::for_window_width(WIDE).rail_width, 176.0);
        assert_eq!(LayoutMetrics::for_window_width(NARROW).rail_width, 64.0);
    }

    #[test]
    fn page_insets_are_24_and_16() {
        assert_eq!(LayoutMetrics::for_window_width(WIDE).page_inset, 24.0);
        assert_eq!(LayoutMetrics::for_window_width(NARROW).page_inset, 16.0);
    }

    #[test]
    fn the_receiver_grid_is_two_columns_wide_and_one_column_narrow() {
        assert_eq!(LayoutMetrics::for_window_width(WIDE).receiver_columns, 2);
        assert_eq!(LayoutMetrics::for_window_width(NARROW).receiver_columns, 1);
    }

    /// The second column is a consequence of the card minimum, not a second
    /// hard-coded breakpoint: one point below the width where two cards still
    /// meet the minimum, the grid has to fall back to one column.
    #[test]
    fn a_second_column_appears_exactly_when_two_cards_still_meet_the_minimum() {
        let widest_single = 2.0 * MIN_RECEIVER_CARD_WIDTH
            + RECEIVER_COLUMN_GAP
            + EXPANDED_RAIL_WIDTH
            + 2.0 * WIDE_PAGE_INSET;
        assert_eq!(
            LayoutMetrics::for_window_width(widest_single).receiver_columns,
            2
        );
        assert_eq!(
            LayoutMetrics::for_window_width(widest_single - 1.0).receiver_columns,
            1
        );
    }

    #[test]
    fn a_window_narrower_than_its_own_chrome_still_leaves_one_column() {
        for width in [0.0, 1.0, 64.0, 120.0] {
            let metrics = LayoutMetrics::for_window_width(width);
            assert_eq!(metrics.receiver_columns, 1, "at {width}");
            assert!(metrics.content_width(width) >= 0.0, "at {width}");
        }
    }

    /// The page grid and the window-level metrics must answer the same
    /// question the same way, or the second column would appear at two
    /// different widths.
    #[test]
    fn the_grid_and_the_window_metrics_agree_about_the_column_count() {
        for width in [WIDE, NARROW, 1000.0, 640.0] {
            let metrics = LayoutMetrics::for_window_width(width);
            assert_eq!(
                metrics.receiver_columns,
                receiver_columns_for_content(metrics.content_width(width)),
                "at {width}"
            );
        }
    }

    /// The measure and the two insets have to account for the whole content
    /// area, whatever its width -- nothing may be left over and nothing may
    /// be claimed twice.
    #[test]
    fn the_readable_measure_and_its_two_insets_fill_the_content_area_exactly() {
        for content in [0.0, 320.0, 900.0, MAX_READABLE_WIDTH, 1051.0, 2000.0] {
            let measure = readable_width(content);
            let inset = readable_inset(content);
            assert!(
                (2.0 * inset + measure - content).abs() < 0.01,
                "at {content}: {inset} + {measure} + {inset} does not add up"
            );
            assert!(inset >= 0.0, "at {content}: a negative inset");
        }
    }

    /// Below the cap there is nothing to centre, and the content keeps the
    /// whole measure.
    #[test]
    fn content_narrower_than_the_cap_is_not_indented_at_all() {
        for content in [0.0, 320.0, 900.0, MAX_READABLE_WIDTH] {
            assert_eq!(readable_inset(content), 0.0, "at {content}");
        }
        assert!(readable_inset(MAX_READABLE_WIDTH + 200.0) > 0.0);
    }

    #[test]
    fn readable_width_caps_at_920_without_ever_exceeding_the_content() {
        assert_eq!(readable_width(2000.0), MAX_READABLE_WIDTH);
        assert_eq!(readable_width(400.0), 400.0);
        let metrics = LayoutMetrics::for_window_width(WIDE);
        let content = metrics.content_width(WIDE);
        assert!(readable_width(content) <= content);
    }

    #[test]
    fn content_width_is_the_window_minus_rail_and_both_insets() {
        let metrics = LayoutMetrics::for_window_width(WIDE);
        assert_eq!(metrics.content_width(WIDE), WIDE - 176.0 - 48.0);
        let metrics = LayoutMetrics::for_window_width(NARROW);
        assert_eq!(metrics.content_width(NARROW), NARROW - 64.0 - 32.0);
    }
}
