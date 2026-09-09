//! Session/configuration binding for a finite, explicitly requested test tone.

use super::model::{
    AudioEndpoint, AudioEndpointPreference, AudioSourceState, DeviceSnapshot, LatencyPreset,
    ReceiverId, RunIntent, SessionPhase, Volume,
};
use std::collections::{BTreeMap, BTreeSet};

/// Exact audio configuration authorized by the user's listening-check start.
/// Kept inside the backend; its debug representation excludes device identities.
#[derive(Clone, PartialEq)]
pub struct ListeningTestGuard {
    generation: u64,
    members: BTreeSet<ReceiverId>,
    volume: Volume,
    levels: BTreeMap<ReceiverId, Volume>,
    endpoint: AudioEndpointPreference,
    captured: Option<AudioEndpoint>,
}

impl std::fmt::Debug for ListeningTestGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListeningTestGuard")
            .field("generation", &self.generation)
            .field("receivers", &self.members.len())
            .finish_non_exhaustive()
    }
}

impl ListeningTestGuard {
    /// Captures only a fully connected, unmuted and low-volume configuration.
    pub fn capture(s: &DeviceSnapshot) -> Option<Self> {
        let SessionPhase::Streaming { generation } = s.session.phase else {
            return None;
        };
        if s.run_intent != RunIntent::Running
            || s.muted
            || s.desired_members.is_empty()
            || s.session.active != s.desired_members
            || s.master_volume.get() <= 0.0
            || s.master_volume.get() > 0.15
            || s.latency_preset != LatencyPreset::Normal
            || !matches!(
                s.audio_source.state,
                AudioSourceState::Capturing | AudioSourceState::SilentSystem
            )
            || s.desired_members
                .iter()
                .any(|id| s.receiver_levels.get(id).is_some_and(|v| v.get() <= 0.0))
        {
            return None;
        }
        Some(Self {
            generation,
            members: s.desired_members.clone(),
            volume: s.master_volume,
            levels: s.receiver_levels.clone(),
            endpoint: s.audio_source.preference.clone(),
            captured: s.audio_source.captured_endpoint.clone(),
        })
    }

    /// Backend generation, never a shell generation or a clock timestamp.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Rejects changed configuration even if the new volume is also low.
    pub fn matches(&self, snapshot: &DeviceSnapshot) -> bool {
        Self::capture(snapshot).as_ref() == Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::*;

    fn active() -> DeviceSnapshot {
        let mut s = DeviceSnapshot::default();
        let id = ReceiverId::from(airplay_core::DeviceId([1; 6]));
        s.run_intent = RunIntent::Running;
        s.master_volume = Volume::new(0.1).unwrap();
        s.desired_members.insert(id);
        s.session.active.insert(id);
        s.session.phase = SessionPhase::Streaming { generation: 7 };
        s.audio_source.state = AudioSourceState::SilentSystem;
        s
    }

    #[test]
    fn listening_test_guard_accepts_silent_source_but_rejects_unsafe_or_changed_sessions() {
        let s = active();
        let guard = ListeningTestGuard::capture(&s)
            .expect("silent Windows does not prevent generated audio");
        assert!(guard.matches(&s));
        for change in [
            |s: &mut DeviceSnapshot| s.run_intent = RunIntent::Stopped,
            |s: &mut DeviceSnapshot| s.session.phase = SessionPhase::Streaming { generation: 8 },
            |s: &mut DeviceSnapshot| s.session.phase = SessionPhase::Stopping { generation: 7 },
            |s: &mut DeviceSnapshot| s.session.active.clear(),
            |s: &mut DeviceSnapshot| s.desired_members.clear(),
            |s: &mut DeviceSnapshot| s.muted = true,
            |s: &mut DeviceSnapshot| s.master_volume = Volume::new(0.2).unwrap(),
            |s: &mut DeviceSnapshot| s.master_volume = Volume::new(0.12).unwrap(),
            |s: &mut DeviceSnapshot| {
                s.receiver_levels
                    .insert(*s.desired_members.first().unwrap(), Volume::MUTED)
                    .map(|_| ())
                    .unwrap_or(())
            },
            |s: &mut DeviceSnapshot| {
                s.audio_source.captured_endpoint = Some(AudioEndpoint {
                    id: "changed".into(),
                    name: "same".into(),
                })
            },
            |s: &mut DeviceSnapshot| s.audio_source.state = AudioSourceState::Unavailable,
        ] {
            let mut changed = s.clone();
            change(&mut changed);
            assert!(
                !guard.matches(&changed),
                "any changed binding must cancel the tone"
            );
        }
        for volume in [0.0, 0.16, 1.0] {
            let mut unsafe_state = active();
            unsafe_state.master_volume = Volume::new(volume).unwrap();
            assert!(ListeningTestGuard::capture(&unsafe_state).is_none());
        }
    }
}
