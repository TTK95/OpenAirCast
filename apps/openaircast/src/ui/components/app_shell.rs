//! The responsive App Shell.
//!
//! One frame is drawn here and nowhere else: the navigation rail with the six
//! fixed destinations and the true status footer, the page header with the
//! localized title and the one contextual primary command, the page-owned
//! scroll region, and the staged-change bar pinned to the bottom of the page
//! viewport.
//!
//! Three ownership decisions in this module are load-bearing:
//!
//! * **The Shell owns the Change Bar.** A page that placed its own would put a
//!   second Apply into the tab order and a second owner in charge of the page
//!   geometry, and the bar would end up drawn over the content it is supposed
//!   to sit below. The bar is created last within the frame, so it is last in
//!   the accessibility and keyboard order as well, and the content region is
//!   shortened by exactly the bar's height rather than overlaid by it.
//! * **The Shell owns the page header.** The single filled primary action per
//!   context is arbitrated once, by
//!   [`crate::ui::presentation::filled_action_owner`], between this header's
//!   lifecycle command and the bar's Apply.
//! * **The rail never scrolls.** Only the page content owns a scroll region,
//!   so navigation and the primary command stay reachable at 900 x 600.
//!
//! Nothing here reaches an [`crate::app_handle::AppHandle`]: the Shell reads a
//! [`UiSnapshot`] and writes typed [`AppEvent`]s through the caller's sink.

use egui::{Align2, Color32, Pos2, Rect, Sense, Shape, StrokeKind, WidgetInfo, WidgetType};

use super::{change_bar, command_action};
use crate::app::{AppEvent, Page, StreamSnapshot, UiSnapshot};
use crate::ui::i18n::{Catalog, ChangeSummaryArgs, NamedValueArgs, TextKey};
use crate::ui::layout::{readable_inset, readable_width, LayoutMetrics, RailMode, UiResources};
use crate::ui::pages;
use crate::ui::presentation::{
    filled_action_owner, focus_ring, ChangeBarModel, CommandActionModel, LifecycleAction,
    CONTROL_RADIUS, SECTION_GAP,
};
use crate::ui::theme::TypographyRole;

/// Height of one navigation destination; also its minimum hit target.
pub const DESTINATION_HEIGHT: f32 = 44.0;
/// Height the Change Bar reserves at the bottom of the page viewport.
pub const CHANGE_BAR_HEIGHT: f32 = 72.0;
/// Width of the leading indicator that marks the current destination.
const SELECTION_INDICATOR_WIDTH: f32 = 3.0;
/// Height of that indicator.
const SELECTION_INDICATOR_HEIGHT: f32 = 20.0;
/// Padding inside a rail item.
const RAIL_ITEM_PADDING: f32 = 8.0;
/// Gap that separates Settings from the daily destinations.
const RAIL_GROUP_GAP: f32 = 16.0;
/// Distance from the rail's left edge to the icon centre when expanded.
const EXPANDED_ICON_X: f32 = 32.0;
/// Distance from the rail's left edge to the label when expanded.
const EXPANDED_LABEL_X: f32 = 52.0;
/// Radius of the status glyph the compact rail paints instead of words.
const STATUS_GLYPH_RADIUS: f32 = 7.0;

/// The lifecycle command the authoritative phase offers, if any.
///
/// One implementation, in [`crate::ui::presentation`], so the header and the
/// Overview model cannot disagree about which command a phase has.
pub fn lifecycle_action(snapshot: &UiSnapshot) -> Option<LifecycleAction> {
    crate::ui::presentation::lifecycle_action(snapshot)
}

/// Whether applying a staged membership right now is a safe, non-disruptive
/// act: the authoritative run intent is stopped and no transition is in
/// flight.
pub fn apply_is_safe(snapshot: &UiSnapshot) -> bool {
    matches!(
        snapshot.stream,
        StreamSnapshot::Stopped | StreamSnapshot::Failed { .. }
    )
}

/// How the staged selection differs from the applied one.
pub fn staged_difference(snapshot: &UiSnapshot) -> ChangeSummaryArgs {
    let added = snapshot
        .staged_receivers
        .iter()
        .filter(|id| !snapshot.desired_receivers.contains(id))
        .count();
    let removed = snapshot
        .desired_receivers
        .iter()
        .filter(|id| !snapshot.staged_receivers.contains(id))
        .count();
    ChangeSummaryArgs { added, removed }
}

/// The Change Bar for this snapshot, or `None` when nothing is staged.
///
/// The caller derives this once per frame and hands it to [`show_shell`], so
/// the arbitration of the single filled action happens in one place.
///
/// The competitor Apply is weighed against is [`primary_action`] -- the
/// command this destination actually draws -- and never
/// [`lifecycle_action`], which describes the phase whether or not the page
/// offers it. Weighing against the phase would let Apply step aside for a
/// Stop button on a destination that does not render it, leaving that
/// destination with no filled action at all and Apply indistinguishable
/// from the Discard beside it.
pub fn change_bar_model(snapshot: &UiSnapshot, catalog: Catalog) -> Option<ChangeBarModel> {
    use crate::app::{GroupCommand, GroupFailure, GroupOperationStatus};
    let selection = snapshot
        .group_operation
        .as_ref()
        .filter(|operation| matches!(operation.command, GroupCommand::ApplySelection { .. }));
    let pending = snapshot
        .group_operation
        .as_ref()
        .is_some_and(|operation| operation.status == GroupOperationStatus::Pending);
    let feedback =
        selection.filter(|operation| operation.status != GroupOperationStatus::Succeeded);
    if !snapshot.staged_membership_dirty && feedback.is_none() {
        return None;
    }
    let owner = filled_action_owner(primary_action(snapshot), true, apply_is_safe(snapshot));
    let mut model = ChangeBarModel::new(catalog, staged_difference(snapshot), owner);
    let key = if pending {
        model.apply_enabled = false;
        model.discard_enabled = false;
        Some(if selection.is_some() {
            TextKey::SelectionApplying
        } else {
            TextKey::OperationPending
        })
    } else if let Some(operation) = feedback {
        model.discard_enabled = true;
        match operation.status {
            GroupOperationStatus::Failed(failure) => Some(match failure {
                GroupFailure::Busy => TextKey::SelectionBusy,
                GroupFailure::Validation => TextKey::SelectionInvalid,
                GroupFailure::Persistence => TextKey::SelectionSaveFailed,
                GroupFailure::ConfirmationLost => TextKey::SelectionUnconfirmed,
                GroupFailure::Closed => {
                    model.apply_enabled = false;
                    TextKey::SelectionClosed
                }
            }),
            _ => None,
        }
    } else {
        None
    };
    if let Some(key) = key {
        model.summary = catalog.text(key).to_owned();
    }
    Some(model)
}

/// Renders the whole client area.
pub fn show_shell(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    change_bar: Option<&ChangeBarModel>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let metrics = LayoutMetrics::for_window_width(ui.available_width());
    show_rail(ui, snapshot, resources, &metrics, emit);
    show_page_area(ui, snapshot, resources, &metrics, change_bar, emit);
}

fn show_rail(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    metrics: &LayoutMetrics,
    emit: &mut dyn FnMut(AppEvent),
) {
    let tokens = resources.tokens;
    egui::Panel::left(egui::Id::new("openaircast_shell_rail"))
        .resizable(false)
        .show_separator_line(false)
        .exact_size(metrics.rail_width)
        .frame(
            egui::Frame::NONE
                .fill(tokens.surface_subtle)
                .inner_margin(egui::Margin::symmetric(0, RAIL_ITEM_PADDING as i8)),
        )
        .show(ui, |rail| {
            // A compact identity anchors the navigation without competing
            // with the page's primary action.
            let (brand, _) =
                rail.allocate_exact_size(egui::vec2(rail.available_width(), 52.0), Sense::hover());
            let mark = egui::pos2(
                brand.left()
                    + if metrics.rail_mode == RailMode::Expanded {
                        EXPANDED_ICON_X
                    } else {
                        brand.width() / 2.0
                    },
                brand.center().y,
            );
            paint_icon(rail.painter(), Page::Audio, mark, tokens.route);
            if metrics.rail_mode == RailMode::Expanded {
                rail.painter().text(
                    egui::pos2(brand.left() + EXPANDED_LABEL_X, brand.center().y),
                    Align2::LEFT_CENTER,
                    "OpenAirCast",
                    TypographyRole::Body.font_id(),
                    tokens.ink,
                );
            }
            rail.add_space(8.0);
            for &page in pages::DESTINATIONS.iter() {
                // Settings sits with the destination set but visually apart.
                if page == Page::Settings {
                    rail.add_space(RAIL_GROUP_GAP);
                }
                show_destination(rail, snapshot, resources, metrics, page, emit);
            }
            show_status_footer(rail, snapshot, resources, metrics);
        });
}

fn show_destination(
    rail: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    metrics: &LayoutMetrics,
    page: Page,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    let name = catalog.text(pages::title_key(page));
    let accessible = catalog.named_value(NamedValueArgs {
        name: catalog.text(TextKey::NavigationDestinationAccessibility),
        value: name,
    });

    rail.push_id(
        egui::Id::new(("shell_destination", pages::destination_index(page))),
        |ui| {
            let width = ui.available_width();
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, DESTINATION_HEIGHT), Sense::click());
            let selected = snapshot.page == page;

            if ui.is_rect_visible(rect) {
                let painter = ui.painter_at(rect);
                let body = rect.shrink2(egui::vec2(4.0, 2.0));
                let radius = egui::CornerRadius::same(CONTROL_RADIUS as u8);
                if selected {
                    // A quiet surface plus a leading indicator, never a
                    // saturated block behind icon and text.
                    painter.rect_filled(body, radius, tokens.surface);
                    let indicator = egui::Rect::from_min_size(
                        egui::pos2(
                            body.left(),
                            body.center().y - SELECTION_INDICATOR_HEIGHT / 2.0,
                        ),
                        egui::vec2(SELECTION_INDICATOR_WIDTH, SELECTION_INDICATOR_HEIGHT),
                    );
                    painter.rect_filled(indicator, egui::CornerRadius::same(1), tokens.route);
                } else if response.hovered() {
                    painter.rect_filled(body, radius, tokens.route_soft);
                }

                let icon_center = match metrics.rail_mode {
                    RailMode::Expanded => {
                        egui::pos2(rect.left() + EXPANDED_ICON_X, rect.center().y)
                    }
                    RailMode::Compact => rect.center(),
                };
                paint_icon(
                    &painter,
                    page,
                    icon_center,
                    if selected {
                        tokens.route
                    } else {
                        tokens.ink_muted
                    },
                );

                if metrics.rail_mode == RailMode::Expanded {
                    painter.text(
                        egui::pos2(rect.left() + EXPANDED_LABEL_X, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        TypographyRole::Button.font_id(),
                        tokens.ink,
                    );
                }

                if response.has_focus() {
                    let ring = focus_ring(tokens);
                    painter.rect_stroke(
                        body.expand(ring.offset),
                        egui::CornerRadius::same((CONTROL_RADIUS + ring.offset) as u8),
                        egui::Stroke::new(ring.width, ring.color),
                        StrokeKind::Outside,
                    );
                }
            }

            // Compact mode drops the painted label, never the name.
            let response = match metrics.rail_mode {
                RailMode::Compact => response.on_hover_text(name),
                RailMode::Expanded => response,
            };
            let announced = accessible.clone();
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::SelectableLabel,
                    true,
                    selected,
                    announced.clone(),
                )
            });

            let activated = response.clicked()
                || (response.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
                    }));
            if activated {
                emit(AppEvent::Navigate(page));
            }
        },
    );
}

/// The true application state, pinned below the destinations.
///
/// A colour-free shape carries it in every rail mode and on every
/// destination; the word is added beside the shape in the expanded rail, and
/// only where the page does not print it itself.
///
/// Both of the obvious rules are wrong, and each was tried. Always printing
/// the word put the identical word in the window twice whenever the Command
/// Home was open -- once in the hero under the page title, once in the
/// bottom-left corner -- with nothing to rank the two. Never printing it
/// removed the duplicate by taking the state off the other five
/// destinations, so a 176-point rail spent fourteen points of itself on a
/// circle and the window said the state nowhere. What the two failures have
/// in common is that they ignore the page: [`pages::prints_status_word`]
/// names the one destination whose own body states the phase, and the footer
/// speaks exactly where that is not the case.
///
/// The compact rail keeps the shape alone in every case, which is what
/// paragraph 6.2 gives a 64-point rail: centred icons, accessible labels,
/// hover tooltips. Nothing loses the state either way -- the tooltip and the
/// accessible name carry the full sentence in all of them.
fn show_status_footer(
    rail: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    metrics: &LayoutMetrics,
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    let key = pages::status_key(snapshot);
    let phase = catalog.text(key);
    let accessible = catalog.named_value(NamedValueArgs {
        name: catalog.text(TextKey::StatusFooterAccessibility),
        value: phase,
    });

    let width = rail.available_width();
    let height = DESTINATION_HEIGHT;

    rail.add_space((rail.available_height() - height).max(0.0));
    let (rect, response) = rail.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let response = response.on_hover_text(phase);
    let announced = accessible.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, announced.clone()));

    if rail.is_rect_visible(rect) {
        let painter = rail.painter_at(rect);
        match metrics.rail_mode {
            RailMode::Compact => paint_status_glyph(&painter, key, rect.center(), tokens.ink),
            RailMode::Expanded => {
                // The shape sits on the destinations' own icon column, so the
                // footer reads as the last row of the rail rather than as a
                // stray mark centred under it.
                paint_status_glyph(
                    &painter,
                    key,
                    egui::pos2(rect.left() + EXPANDED_ICON_X, rect.center().y),
                    tokens.ink,
                );
                if !pages::prints_status_word(snapshot.page) {
                    painter.text(
                        egui::pos2(rect.left() + EXPANDED_LABEL_X, rect.center().y),
                        Align2::LEFT_CENTER,
                        phase,
                        TypographyRole::Button.font_id(),
                        tokens.ink_muted,
                    );
                }
            }
        }
    }
}

/// A distinct outline per phase, so the status footer stays readable without
/// colour vision and without a font that happens to carry a symbol.
fn paint_status_glyph(
    painter: &egui::Painter,
    key: TextKey,
    center: egui::Pos2,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(2.0, color);
    match key {
        TextKey::Streaming => {
            painter.circle_filled(center, STATUS_GLYPH_RADIUS, color);
        }
        TextKey::Connecting => {
            painter.circle_stroke(center, STATUS_GLYPH_RADIUS, stroke);
            painter.circle_filled(center, STATUS_GLYPH_RADIUS / 2.5, color);
        }
        TextKey::Stopping => {
            painter.circle_stroke(center, STATUS_GLYPH_RADIUS, stroke);
            painter.line_segment(
                [
                    center - egui::vec2(STATUS_GLYPH_RADIUS / 2.0, 0.0),
                    center + egui::vec2(STATUS_GLYPH_RADIUS / 2.0, 0.0),
                ],
                stroke,
            );
        }
        TextKey::NeedsAttention => {
            let arm = STATUS_GLYPH_RADIUS * 0.7;
            painter.line_segment(
                [center - egui::vec2(arm, arm), center + egui::vec2(arm, arm)],
                stroke,
            );
            painter.line_segment(
                [
                    center - egui::vec2(arm, -arm),
                    center + egui::vec2(arm, -arm),
                ],
                stroke,
            );
        }
        // Ready, and any phase key a later task adds before it has a glyph of
        // its own: an empty ring is the honest "nothing is happening" mark.
        _ => {
            painter.circle_stroke(center, STATUS_GLYPH_RADIUS, stroke);
        }
    }
}

fn show_page_area(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    metrics: &LayoutMetrics,
    change_bar: Option<&ChangeBarModel>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let tokens = resources.tokens;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(tokens.canvas))
        .show(ui, |page| {
            let full = page.available_rect_before_wrap();
            let reserved = if change_bar.is_some() {
                CHANGE_BAR_HEIGHT
            } else {
                0.0
            };
            let split = (full.bottom() - reserved).max(full.top());
            let content_rect = egui::Rect::from_min_max(full.min, egui::pos2(full.right(), split));

            let mut content = page.new_child(
                egui::UiBuilder::new()
                    .max_rect(content_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            show_page_content(&mut content, snapshot, resources, metrics, emit);

            let Some(model) = change_bar else {
                return;
            };
            let bar_rect = egui::Rect::from_min_max(egui::pos2(full.left(), split), full.max);
            let mut bar = page.new_child(
                egui::UiBuilder::new()
                    .max_rect(bar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            bar.painter()
                .rect_filled(bar_rect, egui::CornerRadius::ZERO, tokens.surface);
            bar.painter().hline(
                bar_rect.x_range(),
                bar_rect.top(),
                egui::Stroke::new(1.0, tokens.border),
            );
            change_bar::show(
                &mut bar,
                tokens,
                model,
                resources.catalog.text(TextKey::ApplyChanges),
                resources.catalog.text(TextKey::Discard),
                emit,
            );
        });
}

/// Paints a simple deterministic glyph per destination; every icon has its
/// textual equivalent in the accessible node and expanded label.
fn paint_icon(painter: &egui::Painter, page: Page, center: Pos2, color: Color32) {
    let stroke = egui::Stroke::new(1.75, color);
    match page {
        Page::Home => {
            painter.add(Shape::line(
                vec![
                    center + vec(-11.0, -1.0),
                    center + vec(0.0, -10.0),
                    center + vec(11.0, -1.0),
                ],
                stroke,
            ));
            painter.add(Shape::line(
                vec![
                    center + vec(-8.0, -3.0),
                    center + vec(-8.0, 9.0),
                    center + vec(-3.0, 9.0),
                    center + vec(-3.0, 3.0),
                    center + vec(3.0, 3.0),
                    center + vec(3.0, 9.0),
                    center + vec(8.0, 9.0),
                    center + vec(8.0, -3.0),
                ],
                stroke,
            ));
        }
        Page::Speakers => {
            // Speaker box + cone
            painter.rect_stroke(
                Rect::from_center_size(center, vec(16.0, 22.0)),
                egui::CornerRadius::same(3),
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.circle_filled(center + vec(0.0, -5.0), 1.5, color);
            painter.circle_stroke(center + vec(0.0, 4.0), 4.0, stroke);
        }
        Page::Groups => {
            for offset in [-6.0, 6.0] {
                let c = center + vec(offset, if offset < 0.0 { -3.0 } else { 3.0 });
                painter.rect_stroke(
                    Rect::from_center_size(c, vec(8.0, 16.0)),
                    egui::CornerRadius::same(2),
                    stroke,
                    StrokeKind::Middle,
                );
                painter.circle_filled(c + vec(0.0, 3.0), 1.5, color);
            }
        }
        Page::Audio => {
            for (x, height) in [
                (-10.0, 3.0),
                (-5.0, 7.0),
                (0.0, 11.0),
                (5.0, 6.0),
                (10.0, 3.0),
            ] {
                painter.line_segment([center + vec(x, -height), center + vec(x, height)], stroke);
            }
        }
        Page::Diagnostics => {
            let points = [
                center + vec(-11.0, 0.0),
                center + vec(-6.0, 0.0),
                center + vec(-2.0, -9.0),
                center + vec(3.0, 9.0),
                center + vec(6.0, 0.0),
                center + vec(11.0, 0.0),
            ];
            painter.add(Shape::line(points.to_vec(), stroke));
        }
        Page::Settings => {
            painter.circle_stroke(center, 6.0, stroke);
            for index in 0..6 {
                let angle = index as f32 * std::f32::consts::TAU / 6.0;
                let dir = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([center + dir * 8.0, center + dir * 12.0], stroke);
            }
        }
    }
}

/// Short alias used only by the icon geometry above.
fn vec(x: f32, y: f32) -> egui::Vec2 {
    egui::vec2(x, y)
}

fn show_page_content(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    metrics: &LayoutMetrics,
    emit: &mut dyn FnMut(AppEvent),
) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::same(metrics.page_inset as i8))
        .show(ui, |inner| {
            show_page_header(inner, snapshot, resources, emit);
            inner.add_space(SECTION_GAP);
            // The page owns the only scroll region in the client area.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(inner, |scroll| {
                    // Capped *and* centred: the cap is the readable measure,
                    // the inset is what stops the whole page from hugging the
                    // rail with the leftover piled up on the right.
                    let region = scroll.available_rect_before_wrap();
                    let available = scroll.available_width();
                    let body = egui::Rect::from_min_size(
                        region.min + egui::vec2(readable_inset(available), 0.0),
                        egui::vec2(readable_width(available), region.height()),
                    );
                    scroll.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(body)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |page| pages::show(snapshot.page, page, snapshot, resources, emit),
                    );
                });
        });
}

fn show_page_header(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    ui.horizontal(|bar| {
        pages::show_text(
            bar,
            TypographyRole::PageTitle,
            tokens.ink,
            catalog.text(pages::title_key(snapshot.page)),
        );
        let Some(action) = primary_action(snapshot) else {
            return;
        };
        bar.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |actions| {
                let owner = filled_action_owner(
                    Some(action),
                    snapshot.staged_membership_dirty,
                    apply_is_safe(snapshot),
                );
                let model = CommandActionModel::lifecycle(catalog, action, owner);
                if command_action::show(actions, tokens, &model).clicked() {
                    if let Some(event) = lifecycle_event(action) {
                        emit(event);
                    }
                }
            },
        );
    });
}

/// Which destination carries a lifecycle command in its header.
///
/// Overview offers the full lifecycle. Groups retains the current session's
/// safety controls during editing, but its only start action applies a group.
fn primary_action(snapshot: &UiSnapshot) -> Option<LifecycleAction> {
    match snapshot.page {
        Page::Home => lifecycle_action(snapshot),
        Page::Groups => lifecycle_action(snapshot).filter(|action| {
            matches!(
                action,
                LifecycleAction::Stop
                    | LifecycleAction::Cancel
                    | LifecycleAction::StopTrying
                    | LifecycleAction::DisabledStopping
            )
        }),
        _ => None,
    }
}

/// The typed event a lifecycle command dispatches.
///
/// The disabled variants map to nothing: they are drawn so the header
/// geometry does not jump, and they can never be activated. The renderer
/// already refuses their activation; this arm is what keeps a future caller
/// from routing one to an event the state cannot honour.
fn lifecycle_event(action: LifecycleAction) -> Option<AppEvent> {
    match action {
        LifecycleAction::Start | LifecycleAction::Retry => Some(AppEvent::StartRequested),
        LifecycleAction::Cancel | LifecycleAction::Stop | LifecycleAction::StopTrying => {
            Some(AppEvent::StopRequested)
        }
        LifecycleAction::DisabledStart
        | LifecycleAction::DisabledRetry
        | LifecycleAction::DisabledStopping => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use airplay_core::DeviceId;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::app::{
        AppState, Availability, GenerationId, NoticeCode, ReceiverState, ResolvedLocale, Severity,
        StreamState, ThemePreference, UserNotice,
    };
    use crate::platform::{SystemAppearance, SystemColors};
    use crate::ui::i18n::TextKey;
    use crate::ui::layout::{COMPACT_RAIL_WIDTH, EXPANDED_RAIL_WIDTH};
    use crate::ui::pages::{title_key, DESTINATIONS};
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

    const LOCALES: [ResolvedLocale; 2] = [ResolvedLocale::German, ResolvedLocale::English];
    const WIDE: [f32; 2] = [1120.0, 720.0];
    const NARROW: [f32; 2] = [900.0, 600.0];

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    /// The `model` is a real service-record identifier rather than a product
    /// name, because that is what discovery actually reports and what the
    /// renderers have to resolve to a localized device class. A made-up model
    /// resolves to the "unknown class" fallback -- the same word as the
    /// Speakers destination's own title -- and a fixture that produced it
    /// would make every lookup by that title ambiguous.
    fn receiver(last: u8, name: &str) -> ReceiverState {
        ReceiverState {
            id: rid(last),
            name: name.into(),
            model: "AudioAccessory1,1".into(),
            availability: Availability::Available,
        }
    }

    fn base_state(page: Page) -> AppState {
        AppState {
            page,
            receivers: vec![receiver(1, "HomePod"), receiver(2, "HomePod mini")],
            desired_receivers: HashSet::from_iter([rid(1)]),
            staged_receivers: HashSet::from_iter([rid(1)]),
            ..AppState::default()
        }
    }

    /// One receiver staged in addition to the applied selection.
    fn dirty_state(page: Page) -> AppState {
        let mut state = base_state(page);
        state.staged_receivers.insert(rid(2));
        state
    }

    type ShellHarness = egui_kittest::Harness<'static, Vec<AppEvent>>;

    fn shell(locale: ResolvedLocale, size: [f32; 2], state: AppState) -> ShellHarness {
        themed_shell(locale, size, state, ThemePreference::Light)
    }

    /// The same shell with the theme left open, so the rail, the header, and
    /// the Change Bar get drawn against the Dark tokens too.
    fn themed_shell(
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
        theme: ThemePreference,
    ) -> ShellHarness {
        appearance_shell(locale, size, state, theme, TEST_APPEARANCE)
    }

    fn appearance_shell(
        locale: ResolvedLocale,
        size: [f32; 2],
        state: AppState,
        theme: ThemePreference,
        appearance: SystemAppearance,
    ) -> ShellHarness {
        let mut harness: ShellHarness = egui_kittest::Harness::new_ui_state(
            move |ui, log: &mut Vec<AppEvent>| {
                let resolved = theme::resolve_theme(theme, None, appearance);
                theme::apply_theme(ui.ctx(), &resolved);
                let resources = UiResources {
                    tokens: &resolved.tokens,
                    catalog: Catalog::new(locale),
                };
                let snapshot = UiSnapshot::from_state(&state);
                let bar = change_bar_model(&snapshot, resources.catalog);
                let mut emit = |event: AppEvent| log.push(event);
                show_shell(ui, &snapshot, &resources, bar.as_ref(), &mut emit);
            },
            Vec::new(),
        );
        harness.set_size(egui::vec2(size[0], size[1]));
        harness.step();
        harness
    }

    fn catalog(locale: ResolvedLocale) -> Catalog {
        Catalog::new(locale)
    }

    fn destination_name(locale: ResolvedLocale, page: Page) -> String {
        let catalog = catalog(locale);
        catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::NavigationDestinationAccessibility),
            value: catalog.text(title_key(page)),
        })
    }

    /// Every accessible label in tree order, which is the order egui also
    /// gives the keyboard.
    fn labels_in_order(harness: &ShellHarness) -> Vec<String> {
        harness
            .root()
            .children_recursive()
            .filter_map(|node| node.accesskit_node().label().map(|label| label.to_string()))
            .collect()
    }

    mod state_mapping {
        use super::*;

        #[test]
        fn the_status_names_the_real_phase_and_a_notice_outranks_it() {
            let mut state = AppState::default();
            assert_eq!(status_of(&state), TextKey::Ready);

            state.stream = StreamState::Starting {
                generation: GenerationId(1),
            };
            assert_eq!(status_of(&state), TextKey::Connecting);

            state.stream = StreamState::Streaming {
                generation: GenerationId(1),
            };
            assert_eq!(status_of(&state), TextKey::Streaming);

            state.stream = StreamState::Stopping {
                generation: GenerationId(1),
            };
            assert_eq!(status_of(&state), TextKey::Stopping);

            state.stream = StreamState::Stopped;
            state.notice = Some(UserNotice {
                severity: Severity::Error,
                code: NoticeCode::SessionFailed,
                summary: "boom".into(),
                action: None,
            });
            assert_eq!(status_of(&state), TextKey::NeedsAttention);
        }

        fn status_of(state: &AppState) -> TextKey {
            crate::ui::pages::status_key(&UiSnapshot::from_state(state))
        }

        #[test]
        fn the_lifecycle_command_follows_the_authoritative_phase() {
            let mut state = AppState {
                receivers: vec![receiver(1, "HomePod")],
                desired_receivers: HashSet::from_iter([rid(1)]),
                staged_receivers: HashSet::from_iter([rid(1)]),
                ..AppState::default()
            };
            let snapshot = UiSnapshot::from_state(&state);
            assert_eq!(lifecycle_action(&snapshot), Some(LifecycleAction::Start));
            assert!(apply_is_safe(&snapshot));

            state.stream = StreamState::Starting {
                generation: GenerationId(1),
            };
            let snapshot = UiSnapshot::from_state(&state);
            assert_eq!(lifecycle_action(&snapshot), Some(LifecycleAction::Cancel));
            assert!(!apply_is_safe(&snapshot));

            state.stream = StreamState::Streaming {
                generation: GenerationId(1),
            };
            let snapshot = UiSnapshot::from_state(&state);
            assert_eq!(lifecycle_action(&snapshot), Some(LifecycleAction::Stop));

            state.stream = StreamState::Stopping {
                generation: GenerationId(1),
            };
            let snapshot = UiSnapshot::from_state(&state);
            assert_eq!(
                lifecycle_action(&snapshot),
                Some(LifecycleAction::DisabledStopping)
            );
            assert!(!lifecycle_action(&snapshot).unwrap().is_enabled());
        }

        #[test]
        fn the_staged_difference_counts_additions_and_removals() {
            let mut state = base_state(Page::Home);
            state.staged_receivers = HashSet::from_iter([rid(2)]);
            let snapshot = UiSnapshot::from_state(&state);
            let difference = staged_difference(&snapshot);
            assert_eq!(difference.added, 1);
            assert_eq!(difference.removed, 1);
        }

        #[test]
        fn a_clean_membership_has_no_change_bar_at_all() {
            let snapshot = UiSnapshot::from_state(&base_state(Page::Home));
            assert!(change_bar_model(&snapshot, catalog(ResolvedLocale::English)).is_none());

            let snapshot = UiSnapshot::from_state(&dirty_state(Page::Home));
            let model = change_bar_model(&snapshot, catalog(ResolvedLocale::English))
                .expect("a staged difference has to produce a bar");
            assert!(model.apply_enabled);
            assert_eq!(model.summary, "1 speaker added");
        }

        /// While audio is live, Stop keeps the filled slot and Apply goes
        /// quiet; once the session is stopped, Apply owns it.
        #[test]
        fn the_filled_slot_moves_between_stop_and_apply_but_is_never_shared() {
            let snapshot = UiSnapshot::from_state(&dirty_state(Page::Home));
            let stopped = change_bar_model(&snapshot, catalog(ResolvedLocale::English)).unwrap();
            assert!(stopped.apply_filled);

            let mut running = dirty_state(Page::Home);
            running.stream = StreamState::Streaming {
                generation: GenerationId(1),
            };
            let snapshot = UiSnapshot::from_state(&running);
            let live = change_bar_model(&snapshot, catalog(ResolvedLocale::English)).unwrap();
            assert!(!live.apply_filled, "Stop owns the filled slot while live");
        }

        /// Apply may only step aside for a command this destination draws.
        ///
        /// The competing command is [`primary_action`], not
        /// [`lifecycle_action`]: the phase offers a command on every
        /// destination, but not every page renders one. Weighing Apply against
        /// the phase leaves those pages with a quiet Apply and no filled
        /// command anywhere.
        ///
        /// Reaching that combination needs `can_stop` inside `Failed`, which
        /// `UiSnapshot::from_state` does not produce today -- `can_stop` is
        /// true only in `Starting` and `Streaming`, and `apply_is_safe` is
        /// false in both. So the field is set by hand here: this is a contract
        /// test of `change_bar_model`, which accepts any `UiSnapshot`, not a
        /// claim that the reducer emits this state. The day it does, the
        /// arbitration is already right.
        #[test]
        fn apply_only_steps_aside_on_the_destination_that_draws_the_competing_command() {
            for &page in DESTINATIONS.iter() {
                let mut state = dirty_state(page);
                state.stream = StreamState::Failed {
                    generation: GenerationId(1),
                    summary: "boom".into(),
                };
                let mut snapshot = UiSnapshot::from_state(&state);
                snapshot.can_stop = true;

                assert_eq!(
                    lifecycle_action(&snapshot),
                    Some(LifecycleAction::StopTrying),
                    "{page:?}: the phase offers a safety command"
                );
                assert!(apply_is_safe(&snapshot), "{page:?}");

                let bar = change_bar_model(&snapshot, catalog(ResolvedLocale::English))
                    .expect("a staged difference has to produce a bar");
                let draws_the_command = primary_action(&snapshot).is_some();
                assert_eq!(
                    bar.apply_filled, !draws_the_command,
                    "{page:?}: Apply is filled exactly where no command competes with it"
                );
            }
        }
    }

    mod navigation {
        use super::*;

        #[test]
        fn the_rail_lists_the_six_localized_destinations_in_order_with_44_point_targets() {
            for locale in LOCALES {
                let mut harness = shell(locale, WIDE, base_state(Page::Home));
                harness.step();
                let mut previous_bottom = 0.0_f32;
                for &page in DESTINATIONS.iter() {
                    let name = destination_name(locale, page);
                    let rect = harness.get_by_label(&name).rect();
                    // The literal is the specification's number, not the
                    // module's constant: asserting against the constant would
                    // only prove that it equals itself.
                    assert!(
                        rect.height() >= 44.0,
                        "{page:?} hit target is {} points",
                        rect.height()
                    );
                    assert!(rect.top() >= previous_bottom, "{page:?} out of order");
                    previous_bottom = rect.bottom();
                }
            }
        }

        #[test]
        fn the_rail_is_176_points_wide_when_expanded_and_64_when_compact() {
            let mut harness = shell(ResolvedLocale::English, WIDE, base_state(Page::Home));
            harness.step();
            let rect = harness
                .get_by_label(&destination_name(ResolvedLocale::English, Page::Home))
                .rect();
            assert!(rect.width() <= EXPANDED_RAIL_WIDTH + 1.0);
            assert!(rect.width() > COMPACT_RAIL_WIDTH);

            let mut harness = shell(ResolvedLocale::English, NARROW, base_state(Page::Home));
            harness.step();
            let rect = harness
                .get_by_label(&destination_name(ResolvedLocale::English, Page::Home))
                .rect();
            assert!(
                rect.width() <= COMPACT_RAIL_WIDTH + 1.0,
                "compact rail is {} points wide",
                rect.width()
            );
        }

        #[test]
        fn the_compact_rail_keeps_every_localized_accessible_name() {
            for locale in LOCALES {
                let mut harness = shell(locale, NARROW, base_state(Page::Home));
                harness.step();
                for &page in DESTINATIONS.iter() {
                    let _ = harness.get_by_label(&destination_name(locale, page));
                }
            }
        }

        #[test]
        fn clicking_a_destination_dispatches_exactly_one_navigate() {
            let mut harness = shell(ResolvedLocale::German, WIDE, base_state(Page::Home));
            harness.step();
            harness
                .get_by_label(&destination_name(ResolvedLocale::German, Page::Speakers))
                .click();
            harness.step();
            assert_eq!(*harness.state(), vec![AppEvent::Navigate(Page::Speakers)]);
        }

        #[test]
        fn keyboard_activation_dispatches_exactly_one_navigate() {
            let mut harness = shell(ResolvedLocale::English, WIDE, base_state(Page::Home));
            harness.step();
            harness
                .get_by_label(&destination_name(ResolvedLocale::English, Page::Audio))
                .focus();
            harness.key_press(egui::Key::Enter);
            harness.step();
            harness.step();
            assert_eq!(*harness.state(), vec![AppEvent::Navigate(Page::Audio)]);
        }

        /// Selection must survive without colour. The leading indicator is a
        /// shape rather than a fill, and the state itself is in the
        /// accessibility tree, where a screen reader can reach it.
        #[test]
        fn the_current_destination_is_marked_selected_for_assistive_technology() {
            let locale = ResolvedLocale::English;
            let mut harness = shell(locale, WIDE, base_state(Page::Groups));
            harness.step();
            for &page in DESTINATIONS.iter() {
                let name = destination_name(locale, page);
                let node = harness.get_by_label(&name);
                // egui projects a selectable widget's state onto AccessKit's
                // `Toggled`, which is what Narrator and NVDA read out.
                let expected = if page == Page::Groups {
                    accesskit::Toggled::True
                } else {
                    accesskit::Toggled::False
                };
                assert_eq!(node.accesskit_node().toggled(), Some(expected), "{page:?}");
            }
        }

        /// How often the last frame painted a line that says the phase and
        /// nothing else.
        ///
        /// The hero prefixes the word with the colour-free phase marker, so
        /// an exact match would miss the very copy this counts. Leading and
        /// trailing marks are stripped and the rest has to *be* the word: a
        /// sentence that merely mentions it is not a second statement of the
        /// state.
        fn painted_phase_lines(harness: &ShellHarness, word: &str) -> usize {
            crate::ui::components::test_support::painted_strings(harness)
                .into_iter()
                .filter(|line| {
                    line.trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
                        .trim()
                        == word
                })
                .count()
        }

        /// The window paints the phase word once, and never zero times where
        /// it has the room to say it.
        ///
        /// Both halves matter and each was broken on its own. Printing it in
        /// the expanded rail *and* in the Overview hero put the same word in
        /// the window twice with nothing to rank the two. Printing it nowhere
        /// but the hero fixed the duplicate by taking the word off five of
        /// six destinations, leaving a 176-point rail spending fourteen
        /// points of it on a circle while the state itself went unsaid.
        /// Paragraph 6.2 pins the true application state below the
        /// destinations; the expanded rail says it in words exactly where the
        /// page does not, which is what [`pages::prints_status_word`] decides
        /// and what this test checks that decision against.
        #[test]
        fn the_expanded_rail_prints_the_phase_exactly_where_the_page_does_not() {
            for page in pages::DESTINATIONS.iter().copied() {
                for locale in LOCALES {
                    let catalog = catalog(locale);
                    let mut harness = shell(locale, WIDE, base_state(page));
                    harness.step();
                    let word = catalog.text(TextKey::Ready);
                    let painted = painted_phase_lines(&harness, word);
                    assert_eq!(
                        painted, 1,
                        "{locale:?} {page:?}: the phase word is painted {painted} times"
                    );
                }
            }
        }

        /// The compact rail carries the state without the room to write it.
        ///
        /// Paragraph 6.2 gives the 64-point rail centred icons with
        /// accessible labels and hover tooltips, so the footer keeps its
        /// shape there and says nothing twice: the Command Home still prints
        /// the word in its hero, and no destination prints it in the rail.
        #[test]
        fn the_compact_rail_carries_the_state_as_a_shape_and_never_as_a_second_word() {
            for page in pages::DESTINATIONS.iter().copied() {
                for locale in LOCALES {
                    let catalog = catalog(locale);
                    let mut harness = shell(locale, NARROW, base_state(page));
                    harness.step();
                    let word = catalog.text(TextKey::Ready);
                    let painted = painted_phase_lines(&harness, word);
                    let expected = usize::from(pages::prints_status_word(page));
                    assert_eq!(
                        painted, expected,
                        "{locale:?} {page:?}: the narrow rail painted the word {painted} times"
                    );
                }
            }
        }

        /// Whatever the rail paints, it always *reports* the state: the
        /// footer occupies a target of its own and announces the full
        /// sentence, in both rail modes and on every destination.
        #[test]
        fn the_status_footer_reports_the_state_on_every_destination_in_both_rail_modes() {
            for size in [WIDE, NARROW] {
                for page in pages::DESTINATIONS.iter().copied() {
                    for locale in LOCALES {
                        let catalog = catalog(locale);
                        let mut harness = shell(locale, size, base_state(page));
                        harness.step();
                        let announced = catalog.named_value(NamedValueArgs {
                            name: catalog.text(TextKey::StatusFooterAccessibility),
                            value: catalog.text(TextKey::Ready),
                        });
                        let footer = harness.get_by_label(&announced);
                        assert!(
                            footer.rect().area() > 0.0,
                            "{locale:?} {page:?} {size:?}: the footer occupies nothing"
                        );
                    }
                }
            }
        }

        #[test]
        fn the_status_footer_announces_the_real_state_in_the_users_language() {
            let mut state = base_state(Page::Home);
            state.stream = StreamState::Streaming {
                generation: GenerationId(1),
            };
            for locale in LOCALES {
                let catalog = catalog(locale);
                let mut harness = shell(locale, WIDE, state.clone());
                harness.step();
                let expected = catalog.named_value(NamedValueArgs {
                    name: catalog.text(TextKey::StatusFooterAccessibility),
                    value: catalog.text(TextKey::Streaming),
                });
                let _ = harness.get_by_label(&expected);
            }
        }
    }

    mod page_chrome {
        use super::*;

        #[test]
        fn the_page_header_shows_the_localized_title_of_the_current_destination() {
            for locale in LOCALES {
                for &page in DESTINATIONS.iter() {
                    let mut harness = shell(locale, WIDE, base_state(page));
                    harness.step();
                    let _ = harness.get_by_label(catalog(locale).text(title_key(page)));
                }
            }
        }

        #[test]
        fn overview_carries_the_one_primary_command_in_its_page_header() {
            let mut harness = shell(ResolvedLocale::English, NARROW, base_state(Page::Home));
            harness.step();
            let start = catalog(ResolvedLocale::English).text(TextKey::StartStreaming);
            let node = harness.get_by_label(start);
            assert!(
                node.rect().bottom() < NARROW[1] / 2.0,
                "the primary command has to stay visible in the header"
            );
            node.click();
            harness.step();
            assert_eq!(*harness.state(), vec![AppEvent::StartRequested]);
        }

        /// A destination without its own lifecycle command shows none, rather
        /// than a disabled button that promises one.
        #[test]
        fn a_destination_without_a_command_shows_no_command() {
            let mut harness = shell(ResolvedLocale::English, WIDE, base_state(Page::Groups));
            harness.step();
            let start = catalog(ResolvedLocale::English).text(TextKey::StartStreaming);
            assert!(
                !labels_in_order(&harness).iter().any(|label| label == start),
                "Groups must not offer the Overview command"
            );
        }

        /// Scrolling is page-owned: it moves the page content and nothing
        /// else. The navigation rail and the page header sit outside the
        /// scroll region, so the primary command stays reachable at 900 x 600.
        #[test]
        fn scrolling_the_page_moves_neither_the_rail_nor_the_page_header() {
            let locale = ResolvedLocale::English;
            let catalog = catalog(locale);
            let title = catalog.text(title_key(Page::Settings));
            let moving = catalog.text(TextKey::CopyLicenseText);
            let rail_item = destination_name(locale, Page::Settings);

            let mut harness = shell(locale, NARROW, base_state(Page::Settings));
            harness.step();
            let title_before = harness.get_by_label(title).rect();
            let rail_before = harness.get_by_label(&rail_item).rect();
            let content_before = harness.get_by_label(moving).rect();

            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(egui::pos2(
                    NARROW[0] / 2.0,
                    NARROW[1] / 2.0,
                )));
            harness.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -200.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            harness.step();

            assert!(
                (harness.get_by_label(moving).rect().top() - content_before.top()).abs() > 1.0,
                "the page content did not scroll, so this proves nothing"
            );
            assert_eq!(
                title_before,
                harness.get_by_label(title).rect(),
                "the page header scrolled away"
            );
            assert_eq!(
                rail_before,
                harness.get_by_label(&rail_item).rect(),
                "the navigation rail scrolled with the page"
            );
        }

        /// The shell draws the rail, the header, the Change Bar, and the
        /// whole page beneath it. Running that once per theme is the cheapest
        /// guard against a Dark-only drawing fault, and it was lost with
        /// `pages.rs`.
        #[test]
        fn the_whole_shell_draws_in_both_themes_at_both_target_sizes() {
            for theme in [ThemePreference::Light, ThemePreference::Dark] {
                for size in [WIDE, NARROW] {
                    for &page in DESTINATIONS.iter() {
                        let mut harness =
                            themed_shell(ResolvedLocale::German, size, dirty_state(page), theme);
                        harness.step();
                        let title = catalog(ResolvedLocale::German).text(title_key(page));
                        assert!(
                            harness.query_all_by_label(title).count() >= 1,
                            "{page:?} lost its title at {size:?} in {theme:?}"
                        );
                        assert!(
                            harness.state().is_empty(),
                            "{page:?} dispatched by itself at {size:?} in {theme:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn every_destination_stays_inside_the_window_at_both_target_sizes() {
            for size in [WIDE, NARROW] {
                for &page in DESTINATIONS.iter() {
                    let mut harness = shell(ResolvedLocale::German, size, dirty_state(page));
                    harness.step();
                    let mut named: Vec<(String, egui::Rect)> = Vec::new();
                    for destination in DESTINATIONS.iter() {
                        let name = destination_name(ResolvedLocale::German, *destination);
                        named.push((name.clone(), harness.get_by_label(&name).rect()));
                    }
                    let title = catalog(ResolvedLocale::German).text(title_key(page));
                    named.push((title.to_owned(), harness.get_by_label(title).rect()));
                    let apply = catalog(ResolvedLocale::German).text(TextKey::ApplyChanges);
                    named.push((apply.to_owned(), harness.get_by_label(apply).rect()));

                    for (name, rect) in named {
                        assert!(
                            rect.min.x >= -1.0
                                && rect.min.y >= -1.0
                                && rect.max.x <= size[0] + 1.0
                                && rect.max.y <= size[1] + 1.0,
                            "{page:?} at {size:?}: {name} paints at {rect:?}"
                        );
                    }
                }
            }
        }
    }

    mod shell_owned_change_bar {
        use super::*;

        #[test]
        fn selection_pending_and_failure_are_visible_on_overview_and_speakers() {
            use crate::app::{GroupCommand, GroupFailure, GroupOperation, GroupOperationStatus};
            for page in [Page::Home, Page::Speakers] {
                for locale in LOCALES {
                    for (status, text, enabled) in [
                        (
                            GroupOperationStatus::Pending,
                            if locale == ResolvedLocale::German {
                                "Auswahl wird angewendet …"
                            } else {
                                "Applying selection…"
                            },
                            false,
                        ),
                        (
                            GroupOperationStatus::Failed(GroupFailure::Persistence),
                            if locale == ResolvedLocale::German {
                                "Speichern fehlgeschlagen. Erneut versuchen."
                            } else {
                                "Save failed. Try again."
                            },
                            true,
                        ),
                    ] {
                        let mut state = dirty_state(page);
                        state.group_operation = Some(GroupOperation {
                            request: 1,
                            command: GroupCommand::ApplySelection {
                                receiver_ids: vec![rid(1), rid(2)],
                            },
                            status,
                        });
                        let harness = shell(locale, WIDE, state);
                        harness.get_by_label(text);
                        let apply =
                            harness.get_by_label(catalog(locale).text(TextKey::ApplyChanges));
                        assert_eq!(!apply.accesskit_node().is_disabled(), enabled);
                    }
                }
            }
        }

        #[test]
        fn selection_feedback_is_accessible_and_bounded_in_high_contrast_for_every_failure() {
            use crate::app::{GroupCommand, GroupFailure, GroupOperation, GroupOperationStatus};
            for page in [Page::Home, Page::Speakers] {
                for locale in LOCALES {
                    for (failure, key) in [
                        (GroupFailure::Busy, TextKey::SelectionBusy),
                        (GroupFailure::Validation, TextKey::SelectionInvalid),
                        (GroupFailure::Persistence, TextKey::SelectionSaveFailed),
                        (GroupFailure::Closed, TextKey::SelectionClosed),
                        (
                            GroupFailure::ConfirmationLost,
                            TextKey::SelectionUnconfirmed,
                        ),
                    ] {
                        let mut state = dirty_state(page);
                        state.group_operation = Some(GroupOperation {
                            request: 3,
                            command: GroupCommand::ApplySelection {
                                receiver_ids: vec![rid(1), rid(2)],
                            },
                            status: GroupOperationStatus::Failed(failure),
                        });
                        let harness = appearance_shell(
                            locale,
                            NARROW,
                            state,
                            ThemePreference::Dark,
                            SystemAppearance {
                                high_contrast: true,
                                ..TEST_APPEARANCE
                            },
                        );
                        let catalog = catalog(locale);
                        harness.get_by_label(catalog.text(key));
                        let apply = harness.get_by_label(catalog.text(TextKey::ApplyChanges));
                        assert_eq!(
                            apply.accesskit_node().is_disabled(),
                            failure == GroupFailure::Closed
                        );
                        let discard = harness.get_by_label(catalog.text(TextKey::Discard));
                        assert!(!discard.accesskit_node().is_disabled());
                        assert!(
                            discard.rect().right() <= NARROW[0],
                            "{page:?}/{locale:?}/{failure:?}: {:?}",
                            discard.rect()
                        );
                    }
                }
            }
        }

        #[test]
        fn selection_keyboard_retry_and_discard_use_the_shared_reducer_contract() {
            use crate::app::{reduce, ControllerEvent, GroupFailure, GroupOperationStatus};
            for page in [Page::Home, Page::Speakers] {
                for locale in LOCALES {
                    let mut state = dirty_state(page);
                    state.desired_known = true;
                    assert_eq!(
                        reduce(&mut state, AppEvent::ApplyStagedReceivers)
                            .effects
                            .len(),
                        1
                    );
                    let pending = state.clone();
                    assert!(reduce(&mut state, AppEvent::ApplyStagedReceivers)
                        .effects
                        .is_empty());
                    reduce(&mut state, AppEvent::DiscardStagedReceivers);
                    assert_eq!(state, pending, "pending operations cannot be abandoned");
                    let request = state.group_operation.as_ref().unwrap().request;
                    reduce(
                        &mut state,
                        AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                            request,
                            status: GroupOperationStatus::Failed(GroupFailure::Persistence),
                        }),
                    );
                    let mut retry = shell(locale, WIDE, state.clone());
                    retry
                        .get_by_label(catalog(locale).text(TextKey::ApplyChanges))
                        .focus();
                    retry.key_press(egui::Key::Enter);
                    retry.step();
                    retry.step();
                    assert_eq!(*retry.state(), vec![AppEvent::ApplyStagedReceivers]);
                    assert_eq!(
                        reduce(&mut state, retry.state()[0].clone()).effects.len(),
                        1
                    );
                    assert_eq!(state.group_operation.as_ref().unwrap().request, request + 1);
                    reduce(
                        &mut state,
                        AppEvent::Controller(ControllerEvent::GroupOperationFinished {
                            request: request + 1,
                            status: GroupOperationStatus::Failed(GroupFailure::ConfirmationLost),
                        }),
                    );
                    let mut discard = shell(locale, WIDE, state.clone());
                    discard
                        .get_by_label(catalog(locale).text(TextKey::Discard))
                        .focus();
                    discard.key_press(egui::Key::Space);
                    discard.step();
                    discard.step();
                    assert_eq!(*discard.state(), vec![AppEvent::DiscardStagedReceivers]);
                    reduce(&mut state, discard.state()[0].clone());
                    assert!(state.group_operation.is_none());
                    assert_eq!(state.staged_receivers, state.desired_receivers);
                    assert!(
                        change_bar_model(&UiSnapshot::from_state(&state), catalog(locale))
                            .is_none()
                    );
                }
            }
        }

        #[test]
        fn selection_feedback_survives_a_clean_echo_until_terminal_acknowledgement() {
            use crate::app::{
                reduce, GroupCommand, GroupFailure, GroupOperation, GroupOperationStatus,
            };
            for status in [
                GroupOperationStatus::Pending,
                GroupOperationStatus::Failed(GroupFailure::ConfirmationLost),
            ] {
                let mut state = base_state(Page::Speakers);
                state.group_operation = Some(GroupOperation {
                    request: 8,
                    command: GroupCommand::ApplySelection {
                        receiver_ids: vec![rid(1)],
                    },
                    status: status.clone(),
                });
                let bar = change_bar_model(
                    &UiSnapshot::from_state(&state),
                    catalog(ResolvedLocale::English),
                )
                .unwrap();
                assert!(!bar.apply_enabled);
                assert_eq!(bar.discard_enabled, status != GroupOperationStatus::Pending);
                reduce(&mut state, AppEvent::DiscardStagedReceivers);
                assert_eq!(
                    state.group_operation.is_some(),
                    status == GroupOperationStatus::Pending
                );
            }
        }

        #[test]
        fn a_dirty_membership_puts_one_bar_at_the_bottom_of_every_destination() {
            let catalog = catalog(ResolvedLocale::English);
            let apply = catalog.text(TextKey::ApplyChanges);
            let discard = catalog.text(TextKey::Discard);
            for size in [WIDE, NARROW] {
                for &page in DESTINATIONS.iter() {
                    let mut harness = shell(ResolvedLocale::English, size, dirty_state(page));
                    harness.step();

                    let matches = harness.query_all_by_label(apply).count();
                    assert_eq!(
                        matches, 1,
                        "{page:?} at {size:?} has {matches} Apply buttons"
                    );
                    let bar = harness.get_by_label(apply).rect();
                    assert!(
                        bar.bottom() <= size[1] + 1.0 && bar.bottom() >= size[1] - 32.0,
                        "{page:?} at {size:?}: Apply is not pinned to the bottom,                          it sits at {bar:?}"
                    );
                    let discard_rect = harness.get_by_label(discard).rect();
                    assert!(
                        discard_rect.left() >= bar.left(),
                        "Discard follows Apply in reading order"
                    );
                }
            }
        }

        #[test]
        fn a_clean_membership_shows_no_bar_on_any_destination() {
            let apply = catalog(ResolvedLocale::English).text(TextKey::ApplyChanges);
            for &page in DESTINATIONS.iter() {
                let mut harness = shell(ResolvedLocale::English, WIDE, base_state(page));
                harness.step();
                assert_eq!(
                    harness.query_all_by_label(apply).count(),
                    0,
                    "{page:?} shows a bar with nothing staged"
                );
            }
        }

        #[test]
        fn apply_and_discard_are_the_last_two_names_in_the_accessibility_order() {
            for &page in DESTINATIONS.iter() {
                let mut harness = shell(ResolvedLocale::English, NARROW, dirty_state(page));
                harness.step();
                let catalog = catalog(ResolvedLocale::English);
                let labels = labels_in_order(&harness);
                let apply = labels
                    .iter()
                    .position(|label| label == catalog.text(TextKey::ApplyChanges))
                    .expect("Apply is announced");
                let discard = labels
                    .iter()
                    .position(|label| label == catalog.text(TextKey::Discard))
                    .expect("Discard is announced");
                assert!(apply < discard, "{page:?}: Apply precedes Discard");
                assert_eq!(
                    discard,
                    labels.len() - 1,
                    "{page:?}: Discard must be last, order is {labels:?}"
                );
            }
        }

        /// The accessibility order above is the order egui also gives the
        /// keyboard, but that is an inference. This walks the real Tab key
        /// across a destination whose only focusable content is the Shell's
        /// own chrome, and checks where it ends up.
        #[test]
        fn tab_reaches_the_rail_first_and_apply_then_discard_last() {
            let locale = ResolvedLocale::English;
            let catalog = catalog(locale);
            let mut harness = shell(locale, WIDE, dirty_state(Page::Groups));
            harness.step();

            let mut expected: Vec<String> = DESTINATIONS
                .iter()
                .map(|&page| destination_name(locale, page))
                .collect();
            expected.push(catalog.text(TextKey::ApplyChanges).to_owned());
            expected.push(catalog.text(TextKey::Discard).to_owned());

            for (step, wanted) in expected.iter().enumerate() {
                harness.key_press(egui::Key::Tab);
                harness.step();
                let focused = expected
                    .iter()
                    .find(|candidate| harness.get_by_label(candidate.as_str()).is_focused());
                assert_eq!(focused, Some(wanted), "tab stop {step} should be {wanted}");
            }
        }

        #[test]
        fn the_bar_keeps_its_place_while_the_page_content_scrolls() {
            let catalog = catalog(ResolvedLocale::English);
            let apply = catalog.text(TextKey::ApplyChanges);
            // A control far down the longest page, so the scroll has to move
            // something for this test to mean anything.
            let moving = catalog.text(TextKey::CopyLicenseText);
            let mut harness = shell(ResolvedLocale::English, NARROW, dirty_state(Page::Settings));
            harness.step();
            let bar_before = harness.get_by_label(apply).rect();
            let content_before = harness.get_by_label(moving).rect();

            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(egui::pos2(
                    NARROW[0] / 2.0,
                    NARROW[1] / 2.0,
                )));
            harness.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -200.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            harness.step();

            let content_after = harness.get_by_label(moving).rect();
            assert!(
                (content_after.top() - content_before.top()).abs() > 1.0,
                "the page content did not scroll, so this proves nothing: \
                 {content_before:?} -> {content_after:?}"
            );
            assert_eq!(
                bar_before,
                harness.get_by_label(apply).rect(),
                "the bar scrolled with the page"
            );
        }

        #[test]
        fn the_bar_reserves_its_own_height_instead_of_covering_the_content() {
            let catalog = catalog(ResolvedLocale::English);
            let apply = catalog.text(TextKey::ApplyChanges);
            let mut harness = shell(ResolvedLocale::English, NARROW, dirty_state(Page::Settings));
            harness.step();
            let bar_top = harness.get_by_label(apply).rect().top();

            let title = catalog.text(title_key(Page::Settings));
            assert!(
                harness.get_by_label(title).rect().bottom() <= bar_top,
                "the page header must sit above the reserved strip"
            );
        }
    }
}
