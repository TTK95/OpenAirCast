//! Diagnostics / Diagnose.
//!
//! Help and safety controls precede actionable live diagnostics, the current
//! Windows source, optional local counters, and the session summary. The
//! listening wizard remains a separate functional check below them.
//!
//! Registry values arrive as [`crate::app::DiagnosticsReading`] and
//! [`crate::app::LiveDiagnosticsReading`], deliberately projected by
//! `backend_bridge`. Absolute wall clocks, addresses, and free-form backend
//! failure text stay on the other side of that boundary.
//!
//! Nothing on this page invents a number. A value the registry has not
//! reported is drawn as an unknown metric -- the localized word and a marker,
//! never a zero -- because a fabricated zero on the one page a user opens to
//! find out whether something is wrong is worse than no page.

use crate::app::{AppEvent, AudioEndpointSelection, CaptureState, UiSnapshot};
use crate::ui::components::metric_tile;
use crate::ui::i18n::{NamedValueArgs, ReceiverCountArgs, RouteSummaryArgs, TextKey};
use crate::ui::layout::UiResources;
use crate::ui::presentation::{DiagnosticsModel, MetricTileModel, DENSE_PADDING, SECTION_GAP};
use crate::ui::theme::{ThemeTokens, TypographyRole};

/// Renders the Diagnostics destination.
pub fn show(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    resources: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let catalog = resources.catalog;
    let tokens = resources.tokens;
    let model = DiagnosticsModel::from_snapshot(snapshot, catalog);

    super::hardware_check::show_controls(ui, snapshot, resources, emit);
    ui.add_space(SECTION_GAP);
    super::live_diagnostics::show(ui, snapshot, resources);
    ui.add_space(SECTION_GAP);

    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        tokens.ink,
        &catalog.named_value(NamedValueArgs {
            name: catalog.text(TextKey::Status),
            value: catalog.text(super::status_key(snapshot)),
        }),
    );
    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink_muted,
        &catalog.route_summary(RouteSummaryArgs {
            selected: snapshot.desired_receivers.len(),
            active: snapshot.active_receivers.len(),
        }),
    );
    super::show_text(
        ui,
        TypographyRole::Body,
        tokens.ink_muted,
        &catalog.receiver_count(ReceiverCountArgs {
            count: snapshot.receivers.len(),
        }),
    );

    if model.empty.is_none() {
        super::show_text(ui, TypographyRole::SectionTitle, tokens.ink, &model.title);
        ui.add_space(DENSE_PADDING);
        show_tiles(ui, tokens, &model.registry);
    }
    ui.add_space(SECTION_GAP);
    super::hardware_check::show_wizard(ui, snapshot, resources, emit);
}

pub(super) fn source_tiles(
    snapshot: &UiSnapshot,
    catalog: crate::ui::i18n::Catalog,
) -> [MetricTileModel; 2] {
    let Some(reading) = snapshot.audio_source.as_ref() else {
        return [
            MetricTileModel::unknown(catalog, catalog.text(TextKey::DiagnosticsAudioSource)),
            MetricTileModel::unknown(catalog, catalog.text(TextKey::AudioCaptureStatus)),
        ];
    };
    let source_name = reading
        .captured_name
        .as_deref()
        .or_else(|| match &reading.selection {
            AudioEndpointSelection::SystemDefault => {
                Some(catalog.text(TextKey::AudioCaptureSystemDefault))
            }
            AudioEndpointSelection::Chosen(key) => reading
                .endpoints
                .iter()
                .find(|endpoint| endpoint.key == *key)
                .map(|endpoint| endpoint.name.as_str()),
            AudioEndpointSelection::ChosenButMissing { name }
            | AudioEndpointSelection::ChosenUnverified { name } => Some(name.as_str()),
        });
    let source = source_name.map_or_else(
        || MetricTileModel::unknown(catalog, catalog.text(TextKey::DiagnosticsAudioSource)),
        |name| {
            MetricTileModel::measured(
                catalog,
                catalog.text(TextKey::DiagnosticsAudioSource),
                name,
                None,
            )
        },
    );
    let state = MetricTileModel::measured(
        catalog,
        catalog.text(TextKey::AudioCaptureStatus),
        catalog.text(match reading.state {
            CaptureState::Capturing => TextKey::CaptureCapturing,
            CaptureState::SilentSystem => TextKey::CaptureSilentSystem,
            CaptureState::Recovering => TextKey::CaptureRecovering,
            CaptureState::Unavailable => TextKey::CaptureUnavailable,
            CaptureState::Failed => TextKey::CaptureFailed,
        }),
        None,
    );
    [source, state]
}

/// The registry strip.
///
/// `ui.columns` panics on zero columns, and the model always produces three,
/// so the guard is about the panic and not about an expected case.
fn show_tiles(ui: &mut egui::Ui, tokens: &ThemeTokens, tiles: &[MetricTileModel]) {
    if tiles.is_empty() {
        return;
    }
    ui.columns(tiles.len(), |cells| {
        for (cell, tile) in cells.iter_mut().zip(tiles) {
            metric_tile::show(cell, tokens, tile);
        }
    });
}
