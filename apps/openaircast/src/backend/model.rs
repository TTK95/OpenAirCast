//! Domain value types and the authoritative `DeviceSnapshot`.
//!
//! This module is intentionally free of transport, socket, key, and AirPlay
//! connection types: snapshots are presentation-safe by construction. Receiver
//! identity is always a validated [`ReceiverId`]; display names are mutable
//! metadata and never identity.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod support;

/// Error raised by domain value validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The input was not exactly twelve uppercase hexadecimal characters.
    #[error("receiver id must be twelve uppercase hexadecimal characters: {0}")]
    InvalidReceiverId(String),
    /// The value was NaN, infinite, or outside `0.0..=1.0`.
    #[error("volume must be a finite value within 0.0..=1.0")]
    InvalidVolume,
}

/// Stable receiver identity derived from the MAC address.
///
/// The persistent form is exactly twelve uppercase hexadecimal characters
/// (`5855CA1AE288`); the display form is colon separated. Addresses and names
/// are mutable discovery attributes and are never used as identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReceiverId([u8; 6]);

impl ReceiverId {
    /// Parses the persistent twelve-uppercase-hex-character form.
    ///
    /// Lowercase digits, wrong lengths, separators, and non-hexadecimal bytes
    /// are rejected so stored identities stay canonical.
    pub fn from_storage_key(value: &str) -> Result<Self, ModelError> {
        let valid = value.len() == 12
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
        if !valid {
            return Err(ModelError::InvalidReceiverId(value.to_owned()));
        }
        let mut bytes = [0_u8; 6];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| ModelError::InvalidReceiverId(value.to_owned()))?;
        }
        Ok(Self(bytes))
    }

    /// Returns the canonical persistent form: twelve uppercase hex characters.
    pub fn storage_key(self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }
}

impl fmt::Display for ReceiverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self
            .0
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        f.write_str(&rendered)
    }
}

impl From<airplay_core::DeviceId> for ReceiverId {
    fn from(value: airplay_core::DeviceId) -> Self {
        Self(value.0)
    }
}

impl From<ReceiverId> for airplay_core::DeviceId {
    fn from(value: ReceiverId) -> Self {
        Self(value.0)
    }
}

impl Serialize for ReceiverId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.storage_key())
    }
}

impl<'de> Deserialize<'de> for ReceiverId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_storage_key(&value).map_err(serde::de::Error::custom)
    }
}

/// A validated linear volume in `0.0..=1.0`.
///
/// NaN, infinity, and out-of-range values are rejected at construction and on
/// deserialization; transparent deserialization is deliberately avoided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume(f32);

impl Volume {
    /// Application default master volume.
    pub const DEFAULT_MASTER: Self = Self(0.25);
    /// Full scale; also the default per-receiver level.
    pub const UNITY: Self = Self(1.0);
    /// Silence.
    pub const MUTED: Self = Self(0.0);

    /// Validates `value` as a finite volume within `0.0..=1.0`.
    pub fn new(value: f32) -> Result<Self, ModelError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ModelError::InvalidVolume)
        }
    }

    /// Returns the raw linear value.
    pub fn get(self) -> f32 {
        self.0
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::DEFAULT_MASTER
    }
}

impl Serialize for Volume {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Volume {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Stable saved-group identifier wrapping a UUID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SavedGroupId(uuid::Uuid);

impl SavedGroupId {
    /// Lossless stable-ID projection for independent consumers.
    pub fn as_uuid(self) -> uuid::Uuid {
        self.0
    }

    /// Restores an identity previously projected with `as_uuid`.
    pub fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id)
    }
    /// Generates a fresh random group ID.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// A member entry of a [`SavedGroup`]: stable identity plus last-known label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedGroupMember {
    /// Stable receiver identity; never an index or address.
    pub receiver: ReceiverId,
    /// Last-seen display name, kept only for offline presentation.
    pub last_known_name: String,
    /// Intended per-receiver balance restored with the group.
    pub level: Volume,
}

/// A user-saved receiver group with trimmed, case-insensitively unique name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedGroup {
    /// Stable group identity.
    pub id: SavedGroupId,
    /// Trimmed display name, unique case-insensitively across saved groups.
    pub name: String,
    /// At least one unique receiver; offline members remain stored and visible.
    pub members: Vec<SavedGroupMember>,
}

/// Whether the user wants audio to run; desired membership is independent.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub enum RunIntent {
    /// Do not stream; cancel retries after a bounded teardown.
    #[default]
    Stopped,
    /// Keep the desired group running under normal recovery policy.
    Running,
}

/// Windows render endpoint selection for capture.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum AudioEndpointPreference {
    /// Follow the Windows default render endpoint.
    #[default]
    SystemDefault,
    /// Stay locked to one stable endpoint ID; if it disappears it stays
    /// selected but reported unavailable rather than silently switching.
    Explicit {
        /// Stable Windows endpoint ID.
        id: String,
        /// Display name captured at selection time.
        last_known_name: String,
    },
}

/// Product latency preset; the UI never exposes free-form timing values.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum LatencyPreset {
    /// Reduced buffering; disabled until the hardware gate passes.
    Low,
    /// The validated default configuration.
    #[default]
    Normal,
    /// Increased recovery headroom; disabled until the hardware gate passes.
    Stable,
}

/// Resolved sender-side timing configuration for one enabled preset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LatencyConfig {
    /// Sender buffer in milliseconds.
    pub sender_buffer_ms: u32,
    /// Render lead in milliseconds.
    pub render_delay_ms: u32,
}

impl LatencyPreset {
    /// Every preset the shell offers, in presentation order.
    ///
    /// Gated presets are listed too: hiding them would leave the user unable
    /// to tell a missing feature from a broken one.
    pub const ALL: [Self; 3] = [Self::Low, Self::Normal, Self::Stable];

    /// Stable English label used inside user-facing text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Normal => "Normal",
            Self::Stable => "Stable",
        }
    }

    /// Resolves the versioned configuration; disabled presets resolve to none
    /// rather than guessing unsafe timing values.
    pub fn enabled_config(self) -> Option<LatencyConfig> {
        match self {
            Self::Normal => Some(LatencyConfig {
                sender_buffer_ms: 2_000,
                render_delay_ms: 200,
            }),
            Self::Low | Self::Stable => None,
        }
    }

    /// `None` while the preset is selectable, otherwise the user-facing
    /// reason it is offered but refused.
    ///
    /// Availability is derived from [`Self::enabled_config`] alone, so the
    /// row the user sees and the rejection the backend sends can never
    /// disagree about which presets the hardware gate still holds back.
    pub fn disabled_reason(self) -> Option<UserFacingError> {
        self.enabled_config()
            .is_none()
            .then(|| UserFacingError::disabled_latency(self))
    }

    /// Every preset paired with its current availability.
    pub fn offered() -> Vec<LatencyPresetOption> {
        Self::ALL
            .into_iter()
            .map(|preset| LatencyPresetOption {
                preset,
                disabled_reason: preset.disabled_reason(),
            })
            .collect()
    }
}

/// One latency preset as offered to the shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatencyPresetOption {
    /// The preset this row describes.
    pub preset: LatencyPreset,
    /// `None` while the preset is selectable; otherwise the pre-redacted
    /// reason selecting it is rejected.
    pub disabled_reason: Option<UserFacingError>,
}

/// Role of a receiver within the current session generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReceiverRole {
    /// Sole receiver on the single-receiver NTP path.
    Single,
    /// PTP clock source shared by every secondary.
    Primary,
    /// Best-effort follower receiving SETPEERS membership.
    Secondary,
}

/// Setup progress reported by the group transport adapter, never inferred by
/// the shell. Variant order reflects real progression.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub enum SetupPhase {
    /// Opening the RTSP/TCP connection.
    Connect,
    /// HomeKit transient pairing exchange.
    Pair,
    /// Establishing timing with the PTP primary.
    PrimaryTiming,
    /// RTSP SETUP negotiation.
    RtspSetup,
    /// Distributing peer lists for PTP groups.
    SetPeers,
    /// Building the RTP sender targets.
    BuildSender,
    /// First audio packets flowing.
    StartAudio,
}

/// Pre-redacted, shell-safe error summary prepared by the backend.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserFacingError(String);

impl UserFacingError {
    /// Wraps an already redacted user-safe message.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Rejection reason for a latency preset still behind its hardware gate.
    ///
    /// The snapshot row and the command rejection are both built here, so the
    /// text the user reads next to a greyed-out option is exactly the text
    /// they get when something selects it anyway.
    pub fn disabled_latency(preset: LatencyPreset) -> Self {
        Self(format!(
            "the {} latency profile is disabled until hardware validation passes",
            preset.label()
        ))
    }

    /// Returns the user-safe message text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserFacingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle of one receiver as shown in the authoritative snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiverLifecycle {
    /// Present in discovery; no session work attempted yet.
    Discovered,
    /// Connection attempt in progress.
    Connecting {
        /// Zero-based attempt number driving backoff display.
        attempt: u32,
    },
    /// Setup handshake in progress.
    SettingUp {
        /// Role this setup attempt is negotiating towards.
        role: ReceiverRole,
        /// Current transport-reported phase.
        phase: SetupPhase,
    },
    /// Negotiated and ready, audio not yet flowing.
    Ready {
        /// Role negotiated during setup.
        role: ReceiverRole,
    },
    /// Actively receiving the live stream.
    Streaming {
        /// Role held in the current generation.
        role: ReceiverRole,
    },
    /// Waiting for the next scheduled reconnect.
    RetryWaiting {
        /// Attempt number that failed.
        attempt: u32,
        /// Wall-clock deadline of the pending retry.
        retry_at: SystemTime,
    },
    /// Not currently discoverable; stays desired if it was desired.
    Unavailable,
    /// Last attempt failed; recovery policy decides whether to retry.
    Failed {
        /// Whether ordinary retry policy may attempt again.
        retryable: bool,
        /// Pre-redacted reason for presentation.
        error: UserFacingError,
    },
}

/// Why an announced full-session restart was required.
///
/// Variants mirror the approved full-restart matrix; operations outside the
/// matrix (volume, mute, levels, metadata, saved-group edits, endpoint swaps)
/// must never produce a restart reason.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RestartReason {
    /// Applying any added or removed desired member.
    MembershipChange,
    /// Activating a saved group with a different member set.
    SavedGroupActivated,
    /// Reconnecting or rejoining a secondary receiver.
    SecondaryRejoin,
    /// Removing a failed target from the immutable streamer.
    FailedTargetRemoved,
    /// Primary loss or primary replacement.
    PrimaryReplaced,
    /// An active receiver address change.
    ReceiverAddressChanged,
    /// A local network-interface/address change.
    LocalInterfaceChanged,
    /// Transition between one-receiver/NTP and multi-receiver/PTP timing.
    TimingModeTransition,
    /// Stream format, receiver-negotiated timing, or SETUP latency change.
    StreamFormatChanged,
    /// Applying an enabled latency preset.
    LatencyPresetChanged,
    /// Windows resume after suspend.
    SystemResume,
    /// Recovery after dead RTSP sessions.
    DeadRtspRecovered,
    /// A manual calibration profile was applied or reset.
    ///
    /// Presentation delays are a session-setup input: they are handed to the
    /// targets while the group is built. Changing them therefore costs a
    /// controlled full-group restart rather than a live adjustment mid-stream.
    CalibrationChanged,
}

/// Phase of the whole session generation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SessionPhase {
    /// No session; run intent is stopped or startup has not begun.
    #[default]
    Stopped,
    /// Best-effort setup running for this generation.
    Starting {
        /// Generation performing the setup.
        generation: u64,
    },
    /// Every desired member is active.
    Streaming {
        /// Generation holding the stream.
        generation: u64,
    },
    /// At least one receiver is active but a desired receiver or the capture
    /// source is unavailable.
    Degraded {
        /// Generation operating degraded.
        generation: u64,
    },
    /// An announced full-session restart is in flight.
    Restarting {
        /// Generation being torn down/replaced.
        generation: u64,
        /// Matrix reason driving the restart.
        reason: RestartReason,
    },
    /// Bounded teardown in progress.
    Stopping {
        /// Generation being torn down.
        generation: u64,
    },
    /// Run intent stayed Running but no receiver could be kept active after
    /// best-effort recovery.
    Failed {
        /// Generation that exhausted recovery.
        generation: u64,
        /// Pre-redacted reason for presentation.
        error: UserFacingError,
    },
}

/// Whether the PCM bridge carries live capture or paced silence.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AudioFlow {
    /// Live captured audio feeds the AirPlay timeline.
    Live,
    /// The bridge paces silence because live capture cannot feed the timeline.
    #[default]
    SilenceBridged,
}

/// Active-membership view of the current session generation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionSnapshot {
    /// Current session phase with its generation tag.
    pub phase: SessionPhase,
    /// Committed desired set for this generation.
    pub desired: BTreeSet<ReceiverId>,
    /// Receivers with successfully negotiated connections.
    pub active: BTreeSet<ReceiverId>,
    /// Desired receivers whose latest attempt failed terminally.
    pub failed: BTreeSet<ReceiverId>,
    /// Desired receivers waiting for a scheduled retry.
    pub retry_waiting: BTreeSet<ReceiverId>,
    /// Current PTP primary, if any.
    pub primary: Option<ReceiverId>,
    /// Live versus silence-bridged PCM flow.
    pub audio_flow: AudioFlow,
    /// Whether this stopped session is waiting for a system resume.
    ///
    /// Set while the machine sleeps: the run intent is deliberately retained,
    /// so this flag is the only thing that distinguishes "the user stopped
    /// playback" from "Windows suspended the machine mid-stream".
    pub resume_pending: bool,
}

/// Discovery supervisor phase for the snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DiscoveryPhase {
    /// Discovery has not started or was stopped for shutdown/suspend.
    #[default]
    Stopped,
    /// A browse generation is running; an empty inventory is authoritative.
    Running,
    /// The daemon failed and restarts with deterministic backoff.
    Retrying {
        /// Attempt number that failed.
        attempt: u32,
        /// Wall-clock deadline of the pending restart.
        retry_at: SystemTime,
    },
}

/// Continuous dual-service discovery state for the snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoverySnapshot {
    /// Current discovery generation; stale events cannot alter newer ones.
    pub generation: u64,
    /// Supervisor phase.
    pub phase: DiscoveryPhase,
}

/// One discovered receiver rendered without transport internals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiverSnapshot {
    /// Stable identity.
    pub id: ReceiverId,
    /// Mutable display name from discovery, empty while none is known.
    ///
    /// Empty is a real state, not a gap to be papered over: a desired member
    /// restored from persisted state is a row before discovery has ever seen
    /// it. Filling it from [`ReceiverId`] would put a MAC address on screen,
    /// so the presentation layer -- which is the only layer that can
    /// localize -- decides what an unnamed receiver is called.
    pub name: String,
    /// Service-record hardware identifier (`AudioAccessory5,1`), empty while
    /// none is known.
    ///
    /// Never presented as-is: the shell resolves it to a localized device
    /// class. It is carried because resolving an always-empty record would
    /// silently call every HomePod and Apple TV a generic speaker.
    pub model: String,
    /// Lifecycle state.
    pub lifecycle: ReceiverLifecycle,
}

/// One saved group rendered for the shell.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedGroupSnapshot {
    /// Stable group identity.
    pub id: SavedGroupId,
    /// Group display name.
    pub name: String,
    /// Stored members in the order chosen when saving.
    pub members: Vec<SavedGroupMember>,
}

/// One render endpoint advertised by Windows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioEndpoint {
    /// Stable Windows endpoint ID.
    pub id: String,
    /// Current display name.
    pub name: String,
}

/// Capture-side availability states distinguished by the design.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AudioSourceState {
    /// WASAPI worker delivers live frames.
    Capturing,
    /// Windows is silent; the bridge keeps buffers alive.
    SilentSystem,
    /// Capture recovery is in progress after worker failure or endpoint loss.
    Recovering,
    /// No usable capture source exists right now.
    #[default]
    Unavailable,
    /// Recovery attempts failed; session reports degraded until resolved.
    Failed,
}

/// Render-endpoint and capture-recovery state for the snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AudioSourceSnapshot {
    /// Last successful render-endpoint scan. None is unmeasured; Some(empty)
    /// is a successful empty scan. Failed refreshes retain the last success.
    pub active_endpoints: Option<Vec<AudioEndpoint>>,
    /// The latest scan failed or its worker terminated; no raw Windows error is exposed.
    pub refresh_failed: bool,
    /// Persisted endpoint preference.
    pub preference: AudioEndpointPreference,
    /// Endpoint currently captured, if any.
    pub captured_endpoint: Option<AudioEndpoint>,
    /// Availability/capture-recovery state.
    pub state: AudioSourceState,
    /// Peak of genuine Windows loopback input observed in the latest sampling
    /// window, in thousandths of full scale. `None` means no fresh batch was
    /// observed; `Some(0)` is measured digital silence.
    pub windows_input_peak_permille: Option<u16>,
    /// Lifetime frames evicted from the stable capture-to-decoder queue.
    pub pcm_frames_dropped_total: u64,
}

/// Persistence subsystem health for the snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PersistenceSnapshot {
    /// No write has been attempted since startup.
    #[default]
    Idle,
    /// Latest load/save cycle succeeded.
    Healthy,
    /// The latest write failed. The change that could not be saved was not
    /// applied either -- persist-before-publish means a rejected write
    /// rejects the whole command -- so the previous state stands in memory
    /// as well as on disk. Nothing is retried automatically; the user
    /// repeats the change.
    Error,
}

/// Authoritative complete render model published through the watch channel.
///
/// Contains no `Device`, socket, cryptographic key, `Connection`, or join
/// handle. Receiver vectors are sorted case-insensitively by name and then by
/// stable ID so rendering never depends on discovery order.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceSnapshot {
    /// Recent command results survive loss on the broadcast channel.
    pub command_outcomes: CommandOutcomes,
    /// Monotonic snapshot revision.
    pub revision: u64,
    /// Revision counter incremented once per committed membership change.
    pub desired_revision: u64,
    /// Discovery supervisor state.
    pub discovery: DiscoverySnapshot,
    /// Known receivers sorted for deterministic rendering.
    pub receivers: Vec<ReceiverSnapshot>,
    /// Saved groups sorted for deterministic rendering.
    pub saved_groups: Vec<SavedGroupSnapshot>,
    /// Committed desired membership; offline members stay desired.
    pub desired_members: BTreeSet<ReceiverId>,
    /// Whether the user wants audio running.
    pub run_intent: RunIntent,
    /// Active-membership view of the current generation.
    pub session: SessionSnapshot,
    /// Capture/endpoint state.
    pub audio_source: AudioSourceSnapshot,
    /// Validated master volume.
    pub master_volume: Volume,
    /// Persisted per-receiver levels keyed by stable identity.
    pub receiver_levels: BTreeMap<ReceiverId, Volume>,
    /// Mute flag; preserves volumes while applying zero downstream.
    pub muted: bool,
    /// Selected latency preset.
    pub latency_preset: LatencyPreset,
    /// Every offered preset with its availability; gated ones carry the
    /// reason they are shown but refused.
    pub latency_presets: Vec<LatencyPresetOption>,
    /// Auto-connect after safe initial discovery; defaults false.
    pub auto_connect: bool,
    /// Persistence subsystem health.
    pub persistence: PersistenceSnapshot,
}

impl Default for DeviceSnapshot {
    /// Hand-written because [`Self::latency_presets`] is not empty by
    /// default: preset availability is a build-time fact of this binary, so
    /// every snapshot -- including the one published before anything has
    /// loaded -- has to carry the complete list.
    fn default() -> Self {
        Self {
            revision: 0,
            command_outcomes: CommandOutcomes::default(),
            desired_revision: 0,
            discovery: DiscoverySnapshot::default(),
            receivers: Vec::new(),
            saved_groups: Vec::new(),
            desired_members: BTreeSet::new(),
            run_intent: RunIntent::default(),
            session: SessionSnapshot::default(),
            audio_source: AudioSourceSnapshot::default(),
            master_volume: Volume::default(),
            receiver_levels: BTreeMap::new(),
            muted: false,
            latency_preset: LatencyPreset::default(),
            latency_presets: LatencyPreset::offered(),
            auto_connect: false,
            persistence: PersistenceSnapshot::default(),
        }
    }
}

/// Bounded completion history in completion order, not allocation order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandOutcomes {
    /// At most 128 retained outcomes, including rejected validation and writes.
    pub recent: std::collections::VecDeque<(u64, Option<CommandFailure>)>,
    /// Largest evicted allocation ID, for diagnostics only. Allocation order
    /// is not completion order: this says nothing about any other command.
    pub evicted_through: u64,
}

impl CommandOutcomes {
    /// Retains one terminal result while bounding snapshot memory.
    pub fn record(&mut self, id: u64, error: Option<CommandFailure>) {
        self.recent.push_back((id, error));
        while self.recent.len() > 128 {
            if let Some((id, _)) = self.recent.pop_front() {
                self.evicted_through = self.evicted_through.max(id);
            }
        }
    }
}

/// Retained failure category, independent of diagnostic prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandFailure {
    /// Invalid or unavailable requested value; the change was rejected.
    Rejected,
    /// Persist-before-publish rejected the change because the save failed.
    Persistence,
}

impl DeviceSnapshot {
    /// Sorts receivers and saved groups for publication: case-folded display
    /// name first, then stable ID, so equal names never depend on discovery
    /// order.
    pub fn sort_for_publication(&mut self) {
        self.receivers.sort_unstable_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        self.saved_groups.sort_unstable_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::support::{receiver_id, receiver_snapshot, saved_group_snapshot};
    use super::*;

    mod receiver_id {
        use super::*;

        #[test]
        fn receiver_id_has_stable_storage_and_display_forms() {
            let id = ReceiverId::from_storage_key("5855CA1AE288").unwrap();
            assert_eq!(id.storage_key(), "5855CA1AE288");
            assert_eq!(id.to_string(), "58:55:CA:1A:E2:88");
            assert!(ReceiverId::from_storage_key("58:55:CA:1A:E2:88").is_err());
            assert!(ReceiverId::from_storage_key("5855CA1AE28Z").is_err());
        }

        #[test]
        fn receiver_id_rejects_lowercase_wrong_length_and_empty_input() {
            assert!(ReceiverId::from_storage_key("5855ca1ae288").is_err());
            assert!(ReceiverId::from_storage_key("5855CA1AE28").is_err());
            assert!(ReceiverId::from_storage_key("5855CA1AE2880").is_err());
            assert!(ReceiverId::from_storage_key("").is_err());
        }

        #[test]
        fn receiver_id_json_round_trip_preserves_identity() {
            let id = ReceiverId::from_storage_key("5855CA1AE288").unwrap();
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, "\"5855CA1AE288\"");
            let decoded: ReceiverId = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, id);
            assert!(serde_json::from_str::<ReceiverId>("\"5855ca1ae288\"").is_err());
        }

        #[test]
        fn receiver_id_converts_losslessly_to_device_id() {
            let bytes = [0x58_u8, 0x55, 0xCA, 0x1A, 0xE2, 0x88];
            let id = ReceiverId::from(airplay_core::DeviceId(bytes));
            assert_eq!(id.storage_key(), "5855CA1AE288");
            let back: airplay_core::DeviceId = id.into();
            assert_eq!(back.0, bytes);
        }

        #[test]
        fn receiver_id_orders_by_raw_bytes() {
            let low = receiver_id(1);
            let high = receiver_id(2);
            assert!(low < high);
        }
    }

    mod volume {
        use super::*;

        #[test]
        fn volume_rejects_non_finite_and_out_of_range_values() {
            assert_eq!(Volume::new(0.25).unwrap().get(), 0.25);
            assert!(Volume::new(f32::NAN).is_err());
            assert!(Volume::new(-0.01).is_err());
            assert!(Volume::new(1.01).is_err());
        }

        #[test]
        fn volume_bounds_are_inclusive_and_defaults_match_spec() {
            assert_eq!(Volume::new(0.0), Ok(Volume::MUTED));
            assert_eq!(Volume::new(1.0), Ok(Volume::UNITY));
            assert_eq!(Volume::DEFAULT_MASTER.get(), 0.25);
            assert_eq!(Volume::default(), Volume::DEFAULT_MASTER);
        }

        #[test]
        fn volume_serde_round_trip_routes_through_validation() {
            let volume = Volume::new(0.375).unwrap();
            let json = serde_json::to_string(&volume).unwrap();
            assert_eq!(json, "0.375");
            let decoded: Volume = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, volume);
            assert!(serde_json::from_str::<Volume>("1.01").is_err());
            assert!(serde_json::from_str::<Volume>("-0.01").is_err());
            assert!(serde_json::from_str::<Volume>("\"loud\"").is_err());
        }
    }

    mod capture_telemetry {
        use super::*;

        #[test]
        fn an_unmeasured_audio_source_has_no_peak_and_no_historical_drops() {
            let source = AudioSourceSnapshot::default();

            assert_eq!(source.windows_input_peak_permille, None);
            assert_eq!(source.pcm_frames_dropped_total, 0);
        }
    }

    mod ordering {
        use super::*;

        #[test]
        fn snapshot_lists_are_sorted_for_rendering() {
            let mut snapshot = DeviceSnapshot {
                receivers: vec![
                    receiver_snapshot(2, "wohnzimmer"),
                    receiver_snapshot(1, "Büro"),
                ],
                saved_groups: vec![
                    saved_group_snapshot(2, "Unten"),
                    saved_group_snapshot(1, "Abends"),
                ],
                ..DeviceSnapshot::default()
            };
            snapshot.sort_for_publication();
            assert_eq!(
                snapshot
                    .receivers
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Büro", "wohnzimmer"]
            );
            assert_eq!(
                snapshot
                    .saved_groups
                    .iter()
                    .map(|g| g.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Abends", "Unten"]
            );
        }

        #[test]
        fn equal_names_tie_break_on_stable_ids_not_discovery_order() {
            let mut snapshot = DeviceSnapshot {
                receivers: vec![receiver_snapshot(5, "Saal"), receiver_snapshot(3, "saal")],
                ..DeviceSnapshot::default()
            };
            snapshot.sort_for_publication();
            assert_eq!(snapshot.receivers[0].id, receiver_id(3));
            assert_eq!(snapshot.receivers[1].id, receiver_id(5));
        }
    }

    mod latency {
        use super::*;

        #[test]
        fn normal_preset_resolves_the_validated_timing_configuration() {
            assert_eq!(
                LatencyPreset::Normal.enabled_config(),
                Some(LatencyConfig {
                    sender_buffer_ms: 2_000,
                    render_delay_ms: 200,
                })
            );
        }

        #[test]
        fn disabled_presets_expose_no_unvalidated_configuration() {
            assert_eq!(LatencyPreset::Low.enabled_config(), None);
            assert_eq!(LatencyPreset::Stable.enabled_config(), None);
        }

        #[test]
        fn every_preset_is_offered_and_the_gated_ones_carry_a_reason() {
            let offered = LatencyPreset::offered();
            assert_eq!(
                offered.iter().map(|row| row.preset).collect::<Vec<_>>(),
                vec![
                    LatencyPreset::Low,
                    LatencyPreset::Normal,
                    LatencyPreset::Stable,
                ],
                "a preset the user may not pick still has to be visible"
            );
            for row in &offered {
                assert_eq!(
                    row.disabled_reason.is_none(),
                    row.preset.enabled_config().is_some(),
                    "availability disagreed with the resolvable configuration"
                );
            }
        }

        #[test]
        fn a_disabled_row_names_the_preset_it_refuses() {
            let low = LatencyPreset::Low
                .disabled_reason()
                .expect("Low is behind its hardware gate");
            let stable = LatencyPreset::Stable
                .disabled_reason()
                .expect("Stable is behind its hardware gate");
            assert!(low.as_str().contains("Low"), "reason: {low}");
            assert!(stable.as_str().contains("Stable"), "reason: {stable}");
            assert_ne!(
                low, stable,
                "both gated presets shared one indistinguishable reason"
            );
            assert_eq!(low, UserFacingError::disabled_latency(LatencyPreset::Low));
        }

        #[test]
        fn a_default_snapshot_already_offers_every_preset() {
            assert_eq!(
                DeviceSnapshot::default().latency_presets,
                LatencyPreset::offered(),
                "availability is a build-time fact and must never start empty"
            );
        }
    }
}
