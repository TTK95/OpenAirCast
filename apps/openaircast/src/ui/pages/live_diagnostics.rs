//! Read-only, presentation-safe live pipeline diagnostics.

use crate::app::{
    CaptureState, DiagnosticIssue, DiagnosticReceiverState, LiveDiagnosticsReading, StreamSnapshot,
    UiSnapshot,
};
use crate::ui::{
    components::{command_action, metric_tile},
    i18n::{Catalog, NamedValueArgs, SignalPercentageArgs, TextKey as K},
    layout::UiResources,
    presentation::{CommandActionModel, Emphasis, MetricTileModel, DENSE_PADDING, SECTION_GAP},
    theme::TypographyRole,
};

/// Renders the actionable verdict and optional local pipeline counters.
pub fn show(ui: &mut egui::Ui, snapshot: &UiSnapshot, r: &UiResources<'_>) {
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        r.tokens.ink,
        r.catalog.text(K::LiveDiagnostics),
    );
    super::show_text(
        ui,
        TypographyRole::Body,
        r.tokens.ink_muted,
        r.catalog.text(verdict(snapshot)),
    );

    ui.add_space(DENSE_PADDING);
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        r.tokens.ink,
        r.catalog.text(K::WindowsAudio),
    );
    let source = super::diagnostics::source_tiles(snapshot, r.catalog);
    let peak = peak_tile(snapshot.live_diagnostics.as_ref(), r.catalog);
    let source_tiles = [source[0].clone(), source[1].clone(), peak];
    ui.columns(source_tiles.len(), |cells| {
        for (cell, tile) in cells.iter_mut().zip(&source_tiles) {
            metric_tile::show(cell, r.tokens, tile);
        }
    });

    let Some(reading) = snapshot.live_diagnostics.as_ref() else {
        return;
    };
    ui.add_space(DENSE_PADDING);
    let details_id = ui.id().with("live-diagnostics-details");
    let mut open = ui.data(|d| d.get_temp::<bool>(details_id).unwrap_or(false));
    let label = r
        .catalog
        .text(if open {
            K::DiagnosticsHideDetails
        } else {
            K::DiagnosticsShowDetails
        })
        .to_owned();
    if command_action::show(
        ui,
        r.tokens,
        &CommandActionModel {
            accessible_name: label.clone(),
            label,
            emphasis: Emphasis::Quiet,
            enabled: true,
        },
    )
    .clicked()
    {
        open = !open;
        ui.data_mut(|d| d.insert_temp(details_id, open));
    }
    if !open {
        return;
    }

    ui.add_space(DENSE_PADDING);
    show_buffer(ui, reading, r);
    ui.add_space(SECTION_GAP);
    show_receivers(ui, snapshot, reading, r);
    super::show_text(
        ui,
        TypographyRole::Secondary,
        r.tokens.ink_muted,
        r.catalog.text(K::DiagnosticsCounterScope),
    );
    super::show_text(
        ui,
        TypographyRole::Secondary,
        r.tokens.ink_muted,
        r.catalog.text(K::DiagnosticsRetransmitDisclaimer),
    );
    super::show_text(
        ui,
        TypographyRole::Secondary,
        r.tokens.ink_muted,
        r.catalog.text(K::DiagnosticsLocalOnly),
    );
}

fn verdict(snapshot: &UiSnapshot) -> K {
    if matches!(snapshot.stream, StreamSnapshot::Stopped) {
        return K::DiagnosticsIdle;
    }
    if snapshot.audio_source.as_ref().is_some_and(|source| {
        matches!(
            source.state,
            CaptureState::Unavailable | CaptureState::Failed
        )
    }) {
        return K::DiagnosticsSourceUnavailable;
    }
    if snapshot
        .audio_source
        .as_ref()
        .is_some_and(|source| source.state == CaptureState::Recovering)
    {
        return K::DiagnosticsSourceRecovering;
    }
    let Some(reading) = snapshot.live_diagnostics.as_ref() else {
        return K::DiagnosticsSignalUnknown;
    };
    let joined = snapshot.receivers.iter().filter_map(|receiver| {
        let in_session = snapshot.desired_receivers.contains(&receiver.id)
            || snapshot.active_receivers.contains(&receiver.id);
        if !in_session {
            return None;
        }
        reading
            .receivers
            .iter()
            .find(|diagnostic| diagnostic.id == receiver.id)
    });
    let joined: Vec<_> = joined.collect();
    if joined.iter().any(|receiver| {
        matches!(
            receiver.state,
            DiagnosticReceiverState::Offline | DiagnosticReceiverState::Failed
        )
    }) {
        return K::DiagnosticsCurrentFailure;
    }
    if joined
        .iter()
        .any(|receiver| receiver.state == DiagnosticReceiverState::Recovering)
    {
        return K::DiagnosticsCurrentRecovering;
    }
    if joined
        .iter()
        .any(|receiver| receiver.state == DiagnosticReceiverState::Connecting)
    {
        return K::DiagnosticsCurrentConnecting;
    }
    if reading.input_peak_per_mille == Some(0) {
        return K::DiagnosticsNoSignal;
    }
    let historical_transport = joined.iter().any(|receiver| {
        receiver
            .transport
            .is_some_and(|transport| transport.send_failures > 0)
    });
    if reading.capture_drops_total > 0 {
        return K::DiagnosticsHistoricalCapture;
    }
    if reading.buffer.is_some_and(|buffer| buffer.underruns > 0) {
        return K::DiagnosticsHistoricalBuffer;
    }
    if historical_transport {
        return K::DiagnosticsHistoricalErrors;
    }
    if reading.input_peak_per_mille.is_none() {
        return K::DiagnosticsSignalUnknown;
    }
    if joined.iter().any(|receiver| {
        receiver
            .transport
            .is_some_and(|transport| transport.packets_accepted > 0)
    }) {
        K::DiagnosticsLocalHealthy
    } else {
        K::DiagnosticsAwaitingSend
    }
}

fn peak_tile(reading: Option<&LiveDiagnosticsReading>, catalog: Catalog) -> MetricTileModel {
    match reading.and_then(|reading| reading.input_peak_per_mille) {
        Some(peak) => MetricTileModel::measured(
            catalog,
            catalog.text(K::DiagnosticsInputSignal),
            catalog.signal_percentage(SignalPercentageArgs { per_mille: peak }),
            None,
        ),
        None => MetricTileModel::unknown(catalog, catalog.text(K::DiagnosticsInputSignal)),
    }
}

fn show_buffer(ui: &mut egui::Ui, reading: &LiveDiagnosticsReading, r: &UiResources<'_>) {
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        r.tokens.ink,
        r.catalog.text(K::DiagnosticsBuffer),
    );
    let mut tiles = Vec::new();
    if let Some(buffer) = reading.buffer {
        tiles.push(MetricTileModel::measured(
            r.catalog,
            r.catalog.text(K::DiagnosticsBufferFill),
            format!("{} / {}", buffer.queued_frames, buffer.capacity_frames),
            Some(r.catalog.text(K::Frames).to_owned()),
        ));
        tiles.push(MetricTileModel::measured(
            r.catalog,
            r.catalog.text(K::DiagnosticsBufferedAudio),
            buffer.buffered_ms.to_string(),
            Some("ms".to_owned()),
        ));
        tiles.push(MetricTileModel::measured(
            r.catalog,
            r.catalog.text(K::DiagnosticsBufferUnderruns),
            buffer.underruns.to_string(),
            None,
        ));
    } else {
        for key in [
            K::DiagnosticsBufferFill,
            K::DiagnosticsBufferedAudio,
            K::DiagnosticsBufferUnderruns,
        ] {
            tiles.push(MetricTileModel::unknown(r.catalog, r.catalog.text(key)));
        }
    }
    tiles.push(MetricTileModel::measured(
        r.catalog,
        r.catalog.text(K::DiagnosticsCaptureDrops),
        reading.capture_drops_total.to_string(),
        None,
    ));
    show_grid(ui, r, &tiles);
}

fn show_receivers(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    reading: &LiveDiagnosticsReading,
    r: &UiResources<'_>,
) {
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        r.tokens.ink,
        r.catalog.text(K::DiagnosticsReceivers),
    );
    for receiver in snapshot.receivers.iter() {
        let Some(diagnostic) = reading
            .receivers
            .iter()
            .find(|diagnostic| diagnostic.id == receiver.id)
        else {
            continue;
        };
        super::group_card(ui, r, |card| {
            super::show_text(
                card,
                TypographyRole::SectionTitle,
                r.tokens.ink,
                &receiver.name,
            );
            let state = r.catalog.named_value(NamedValueArgs {
                name: r.catalog.text(K::Status),
                value: r.catalog.text(receiver_state_key(diagnostic.state)),
            });
            super::show_text(card, TypographyRole::Body, r.tokens.ink_muted, &state);
            if let Some(issue) = diagnostic.issue {
                super::show_text(
                    card,
                    TypographyRole::Body,
                    r.tokens.ink,
                    r.catalog.text(issue_key(issue)),
                );
            }
            let tiles = match diagnostic.transport {
                Some(transport) => vec![
                    measured(
                        r.catalog,
                        K::DiagnosticsPacketsAccepted,
                        transport.packets_accepted,
                    ),
                    measured(
                        r.catalog,
                        K::DiagnosticsBytesAccepted,
                        transport.bytes_accepted,
                    ),
                    measured(
                        r.catalog,
                        K::DiagnosticsSendFailures,
                        transport.send_failures,
                    ),
                    measured(
                        r.catalog,
                        K::DiagnosticsRetransmitRequests,
                        transport.retransmit_requests,
                    ),
                ],
                None => [
                    K::DiagnosticsPacketsAccepted,
                    K::DiagnosticsBytesAccepted,
                    K::DiagnosticsSendFailures,
                    K::DiagnosticsRetransmitRequests,
                ]
                .into_iter()
                .map(|key| MetricTileModel::unknown(r.catalog, r.catalog.text(key)))
                .collect(),
            };
            show_grid(card, r, &tiles);
        });
        ui.add_space(DENSE_PADDING);
    }
}

fn measured(catalog: Catalog, key: K, value: u64) -> MetricTileModel {
    MetricTileModel::measured(catalog, catalog.text(key), value.to_string(), None)
}

fn receiver_state_key(state: DiagnosticReceiverState) -> K {
    match state {
        DiagnosticReceiverState::Ready => K::Ready,
        DiagnosticReceiverState::Connecting => K::Connecting,
        DiagnosticReceiverState::Streaming => K::Streaming,
        DiagnosticReceiverState::Recovering => K::Reconnecting,
        DiagnosticReceiverState::Offline => K::ReceiverUnavailable,
        DiagnosticReceiverState::Failed => K::DiagnosticsHealthError,
    }
}

fn issue_key(issue: DiagnosticIssue) -> K {
    match issue {
        DiagnosticIssue::Pairing => K::DiagnosticsIssuePairing,
        DiagnosticIssue::Setup => K::DiagnosticsIssueSetup,
        DiagnosticIssue::Capture => K::DiagnosticsIssueCapture,
        DiagnosticIssue::Timing => K::DiagnosticsIssueTiming,
        DiagnosticIssue::Transport => K::DiagnosticsIssueTransport,
        DiagnosticIssue::Feedback => K::DiagnosticsIssueFeedback,
        DiagnosticIssue::Teardown => K::DiagnosticsIssueTeardown,
        DiagnosticIssue::Other => K::DiagnosticsIssueOther,
    }
}

fn show_grid(ui: &mut egui::Ui, r: &UiResources<'_>, tiles: &[MetricTileModel]) {
    for row in tiles.chunks(2) {
        ui.columns(row.len(), |cells| {
            for (cell, tile) in cells.iter_mut().zip(row) {
                metric_tile::show(cell, r.tokens, tile);
            }
        });
        ui.add_space(DENSE_PADDING);
    }
}
