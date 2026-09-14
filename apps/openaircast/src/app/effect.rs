use airplay_core::DeviceId;

use super::{AudioEndpointRequest, GenerationId, HotkeyBinding, LatencyChoice, Preferences};

/// Private controller seam: today adapted onto the legacy AirPlay control
/// thread, replaced by `DeviceBackendHandle` in subproject 2 without touching
/// UI, tray, hotkey, reducer, or `AppHandle` call sites.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DeviceCommand {
    Group {
        request: u64,
        generation: Option<GenerationId>,
        command: super::GroupCommand,
    },
    ApplyReceiverLevel {
        receiver: DeviceId,
        level: f32,
    },
    Discover {
        generation: GenerationId,
    },
    StartSession {
        generation: GenerationId,
        receiver_ids: Vec<DeviceId>,
        volume: f32,
    },
    StopSession {
        generation: GenerationId,
    },
    PlayListeningTone {
        generation: GenerationId,
    },
    ApplyVolume {
        generation: GenerationId,
        volume: f32,
    },
    /// Mute or unmute the group.
    ///
    /// No generation, and that is the same decision `ApplyVolume`'s carries
    /// but never uses: these are coalesced backend settings, not lifecycle
    /// edges. The backend answers them by republishing its state, and the
    /// level-driven projection is what makes a refused one visible -- so
    /// there is no press for a generation to match against.
    ApplyMute(bool),
    /// Select a latency profile.
    ApplyLatency(LatencyChoice),
    /// Select the capture source.
    ApplyAudioEndpoint(AudioEndpointRequest),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ControllerSendError {
    #[error("device service queue is busy")]
    Busy,
    #[error("device service is closed")]
    Closed,
}

pub(crate) trait ControllerPort: Send + Sync {
    /// Non-blocking bounded send of one backend command.
    fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    /// Emit one finite tone in the explicitly authorized hearing-test session.
    PlayListeningTone {
        generation: GenerationId,
    },
    /// Send the reducer-allocated request and complete target to the backend.
    Group {
        /// Correlation identity, allocated automatically by GroupRequested.
        request: u64,
        /// Session generation for an explicit group start from idle/failure/stopping.
        generation: Option<GenerationId>,
        /// Requested template or applied-selection operation.
        command: super::GroupCommand,
    },
    /// Apply one receiver's balance without a session lifecycle edge.
    ApplyReceiverLevel {
        receiver: DeviceId,
        level: f32,
    },
    Discover {
        generation: GenerationId,
    },
    StartSession {
        generation: GenerationId,
        receiver_ids: Vec<DeviceId>,
        volume: f32,
    },
    StopSession {
        generation: GenerationId,
    },
    ApplyVolume {
        generation: GenerationId,
        volume: f32,
    },
    /// Mute or unmute the group. See [`DeviceCommand::ApplyMute`].
    ApplyMute(bool),
    /// Select a latency profile.
    ApplyLatency(LatencyChoice),
    /// Select the capture source.
    ApplyAudioEndpoint(AudioEndpointRequest),
    PersistPreferences {
        generation: GenerationId,
        value: Preferences,
    },
    ReconfigureHotkey {
        generation: GenerationId,
        value: HotkeyBinding,
    },
    ShowMainWindow,
    HideMainWindow,
    BeginShutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiEffect {
    ShowMainWindow,
    HideMainWindow,
    #[allow(dead_code)] // consumed by subproject 3 diagnostics wiring.
    FocusMainWindow,
    #[allow(dead_code)]
    Announce(String),
    DropTray,
    CloseApplication,
}

#[cfg(test)]
mod tests {
    use crate::app::UiEffect;

    #[test]
    fn frozen_ui_effect_surface_exposes_every_variant() {
        let effects = [
            UiEffect::ShowMainWindow,
            UiEffect::HideMainWindow,
            UiEffect::FocusMainWindow,
            UiEffect::Announce("Connected".into()),
            UiEffect::DropTray,
            UiEffect::CloseApplication,
        ];

        assert_eq!(
            effects,
            [
                UiEffect::ShowMainWindow,
                UiEffect::HideMainWindow,
                UiEffect::FocusMainWindow,
                UiEffect::Announce("Connected".into()),
                UiEffect::DropTray,
                UiEffect::CloseApplication,
            ]
        );
    }
}
