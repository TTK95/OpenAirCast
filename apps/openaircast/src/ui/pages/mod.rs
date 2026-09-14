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
mod destination_tests;
