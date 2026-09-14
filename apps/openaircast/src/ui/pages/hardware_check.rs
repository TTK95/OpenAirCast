//! Localized manual listening check; report accepts only safe snapshot values.
use crate::app::hardware_check::{
    HardwareCheckAction as A, HardwareCheckBlocker as B, HardwareCheckPhase as P,
};
use crate::app::{AppEvent, Page, UiSnapshot};
use crate::ui::{
    components::command_action,
    i18n::{Catalog, TextKey as K},
    layout::UiResources,
    presentation::{CommandActionModel, Emphasis},
    theme::TypographyRole,
};

fn button(ui: &mut egui::Ui, r: &UiResources<'_>, key: K, enabled: bool) -> bool {
    let label = r.catalog.text(key).to_owned();
    command_action::show(
        ui,
        r.tokens,
        &CommandActionModel {
            accessible_name: label.clone(),
            label,
            emphasis: Emphasis::Quiet,
            enabled,
        },
    )
    .clicked()
}

/// Opens Diagnostics without a playback command.
pub fn entry(ui: &mut egui::Ui, r: &UiResources<'_>, emit: &mut dyn FnMut(AppEvent)) {
    if button(ui, r, K::HardwareCheck, true) {
        emit(AppEvent::HardwareCheck(A::Open));
    }
}

fn phase_key(phase: P) -> K {
    match phase {
        P::Preparation => K::HardwarePreparation,
        P::Connecting => K::Connecting,
        P::Listening => K::HardwareListening,
        P::StopPending => K::HardwarePending,
        P::Heard => K::HardwarePass,
        P::NotHeard => K::HardwareFail,
        P::Invalidated => K::HardwareInvalid,
    }
}

fn blocker_key(blocker: B) -> K {
    match blocker {
        B::Unavailable => K::HardwareUnavailable,
        B::StopPending => K::HardwarePending,
        B::SessionRunning => K::HardwareBusy,
        B::Selection => K::HardwareSelection,
        B::SelectionPending => K::HardwareSelectionPending,
        B::Offline => K::HardwareOffline,
        B::ReceiverLevels => K::HardwareLevels,
        B::Muted => K::HardwareMuted,
        B::Latency => K::HardwareLatency,
        B::VolumePending => K::HardwareVolumePending,
        B::Volume => K::HardwareVolume,
        B::Source => K::HardwareSource,
        B::Confirmation => K::HardwareConfirmation,
    }
}

fn text(ui: &mut egui::Ui, r: &UiResources<'_>, key: K) {
    super::show_text(
        ui,
        TypographyRole::Body,
        r.tokens.ink_muted,
        r.catalog.text(key),
    );
}

/// Copies only fixed catalog text and approved numeric registry values.
pub fn report(snapshot: &UiSnapshot, catalog: Catalog) -> String {
    let mut text = format!(
        "{}\n{}: {}\n{}\n{}",
        catalog.text(K::HardwareSnapshot),
        catalog.text(K::Selected),
        snapshot.hardware_check.count,
        catalog.text(phase_key(snapshot.hardware_check.phase)),
        catalog.text(K::HardwareLimitations)
    );
    if let Some(reading) = snapshot.diagnostics {
        let health = match reading.health {
            crate::app::DiagnosticsHealth::Unknown => K::Unknown,
            crate::app::DiagnosticsHealth::Running => K::DiagnosticsHealthRunning,
            crate::app::DiagnosticsHealth::Attention => K::NeedsAttention,
            crate::app::DiagnosticsHealth::Error => K::DiagnosticsHealthError,
        };
        text.push_str(&format!(
            "\n{}: {}\n{}: {}\n{}: {}",
            catalog.text(K::DiagnosticsOverallHealth),
            catalog.text(health),
            catalog.text(K::DiagnosticsEventsLost),
            reading.events_dropped_total,
            catalog.text(K::DiagnosticsMeasuredReceivers),
            reading.measured_receivers
        ));
    }
    text.push_str(&format!("\n{}", catalog.text(K::HardwareEventNote)));
    text
}

/// Renders navigation and optional help before every other Diagnostics element.
pub fn show_controls(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    r: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let help_id = ui.id().with("hardware-help-open");
    let mut help = ui.data(|d| d.get_temp::<bool>(help_id).unwrap_or(false));
    ui.horizontal_wrapped(|ui| {
        if button(ui, r, K::HardwareBack, true) {
            emit(AppEvent::Navigate(Page::Audio));
        }
        if button(
            ui,
            r,
            if help {
                K::HardwareHideHelp
            } else {
                K::HardwareHelp
            },
            true,
        ) {
            help = !help;
            ui.data_mut(|d| d.insert_temp(help_id, help));
        }
        if matches!(snapshot.hardware_check.phase, P::Connecting | P::Listening)
            && button(ui, r, K::Cancel, true)
        {
            emit(AppEvent::HardwareCheck(A::Cancel));
        }
    });
    if help {
        text(ui, r, K::HardwareRequirements);
        text(ui, r, K::HardwareLimitations);
        text(ui, r, K::HardwareEventNote);
    }
}

/// Renders explicit preparation, playback and acknowledgement steps.
pub fn show_wizard(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    r: &UiResources<'_>,
    emit: &mut dyn FnMut(AppEvent),
) {
    let check = snapshot.hardware_check;
    super::show_text(
        ui,
        TypographyRole::SectionTitle,
        r.tokens.ink,
        r.catalog.text(K::HardwareTitle),
    );
    text(ui, r, K::HardwareSourceNotice);
    let volume = r.catalog.percentage(crate::ui::i18n::PercentageArgs {
        percent: (snapshot.master_volume * 100.0).round().clamp(0.0, 100.0) as u8,
    });
    super::show_text(
        ui,
        TypographyRole::Body,
        r.tokens.ink_muted,
        &r.catalog.named_value(crate::ui::i18n::NamedValueArgs {
            name: r.catalog.text(K::MasterVolume),
            value: &volume,
        }),
    );
    super::show_text(
        ui,
        TypographyRole::Body,
        r.tokens.ink_muted,
        r.catalog.text(phase_key(check.phase)),
    );
    if !matches!(check.phase, P::Connecting | P::Listening | P::StopPending) {
        let mut prepared = check.prepared;
        if ui
            .checkbox(&mut prepared, r.catalog.text(K::HardwarePrepare))
            .changed()
        {
            emit(AppEvent::HardwareCheck(A::Prepare(prepared)));
        }
        ui.horizontal_wrapped(|ui| {
            if button(ui, r, K::HardwareLower, !snapshot.shutting_down) {
                emit(AppEvent::HardwareCheck(A::LowerVolume));
            }
            if button(ui, r, K::Speakers, true) {
                emit(AppEvent::Navigate(Page::Speakers));
            }
        });
        ui.add_space(8.0);
        text(
            ui,
            r,
            check.blocker.map(blocker_key).unwrap_or(K::HardwareReady),
        );
        if check.blocker == Some(B::SessionRunning)
            && button(ui, r, K::StopStreaming, snapshot.can_stop)
        {
            emit(AppEvent::StopRequested);
        }
        if button(ui, r, K::HardwareStart, check.ready && check.prepared) {
            emit(AppEvent::HardwareCheck(A::Start));
        }
    }
    if matches!(check.phase, P::Connecting | P::Listening) {
        text(ui, r, K::HardwareBackActive);
        if button(ui, r, K::HardwareHeard, check.can_confirm) {
            emit(AppEvent::HardwareCheck(A::Heard(true)));
        }
        if button(ui, r, K::HardwareNotHeard, check.can_confirm) {
            emit(AppEvent::HardwareCheck(A::Heard(false)));
        }
    }
    let copied = ui.id().with("hardware-report-copied");
    if button(ui, r, K::HardwareReport, true) {
        ui.ctx().copy_text(report(snapshot, r.catalog));
        ui.data_mut(|d| d.insert_temp(copied, true));
    }
    if ui.data(|d| d.get_temp::<bool>(copied).unwrap_or(false)) {
        super::show_text(
            ui,
            TypographyRole::Body,
            r.tokens.ink_muted,
            r.catalog.text(K::CopiedToClipboard),
        );
    }
    ui.add_space(12.0);
}
