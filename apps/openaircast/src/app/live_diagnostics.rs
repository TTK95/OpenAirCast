//! Narrow, typed readings for the live Diagnostics page.

use super::GenerationId;
use airplay_core::DeviceId;

/// Numeric telemetry; identities only join the already-sanitized receiver inventory.
/// Never contains backend prose, addresses, endpoint IDs or wall-clock timestamps.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveDiagnosticsReading {
    pub generation: GenerationId,
    /// Peak of actual Windows samples in the recent observation window, not test audio.
    pub input_peak_per_mille: Option<u16>,
    /// PCM bridge drops since this app's capture supervisor was created.
    pub capture_drops_total: u64,
    /// Only the current, positively matched session's buffer is exposed.
    pub buffer: Option<BufferDiagnosticsReading>,
    pub receivers: Vec<ReceiverDiagnosticsReading>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BufferDiagnosticsReading {
    pub queued_frames: u32,
    pub capacity_frames: u32,
    pub buffered_ms: u64,
    pub underruns: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportDiagnosticsReading {
    /// Accepted by the local OS, not acknowledged as played by the receiver.
    pub packets_accepted: u64,
    pub bytes_accepted: u64,
    pub send_failures: u64,
    /// Requested sequence slots, not unique lost packets.
    pub retransmit_requests: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiverDiagnosticsReading {
    pub id: DeviceId,
    pub state: DiagnosticReceiverState,
    /// Missing client measurements remain unknown rather than invented zeroes.
    pub transport: Option<TransportDiagnosticsReading>,
    pub issue: Option<DiagnosticIssue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticReceiverState {
    Ready,
    Connecting,
    Streaming,
    Recovering,
    Offline,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticIssue {
    Pairing,
    Setup,
    Capture,
    Timing,
    Transport,
    Feedback,
    Teardown,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::*;

    #[test]
    fn live_diagnostics_is_read_only_and_old_generations_cannot_replace_it() {
        let mut state = AppState::default();
        state.generations.session = GenerationId(4);
        let current = LiveDiagnosticsReading {
            generation: GenerationId(4),
            input_peak_per_mille: Some(500),
            capture_drops_total: 3,
            ..Default::default()
        };
        let update = reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated {
                reading: current.clone(),
            }),
        );
        assert!(update.effects.is_empty());
        assert_eq!(
            UiSnapshot::from_state(&state).live_diagnostics,
            Some(current.clone())
        );
        let revision = state.revision;
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated {
                reading: current.clone(),
            }),
        );
        assert_eq!(
            state.revision, revision,
            "unchanged counters must not trigger repaint"
        );
        reduce(
            &mut state,
            AppEvent::Controller(ControllerEvent::LiveDiagnosticsUpdated {
                reading: LiveDiagnosticsReading {
                    generation: GenerationId(3),
                    input_peak_per_mille: Some(0),
                    ..Default::default()
                },
            }),
        );
        assert_eq!(state.live_diagnostics, Some(current));
        state.generations.session = GenerationId(5);
        assert!(UiSnapshot::from_state(&state).live_diagnostics.is_none());
    }

    #[test]
    fn live_diagnostics_disappears_when_the_controller_is_closed() {
        let mut state = AppState::default();
        state.live_diagnostics = Some(LiveDiagnosticsReading::default());
        state.controller_closed = true;
        assert!(UiSnapshot::from_state(&state).live_diagnostics.is_none());
    }
}
