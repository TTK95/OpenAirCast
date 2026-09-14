// ===========================================================================
// Task 11: versioned, support-safe export and copy summary
//
// Redaction design: the export never serializes a domain struct. Every field
// of every DTO below is either a number, a bool, a `&'static str` chosen by
// an exhaustive match, or an alias minted by `AliasTable`. There is no field
// a `String` from the running system can be assigned to, so no name, address,
// path, header, key, or protocol error text has anywhere to land -- and a
// future variant that carries one forces a compile error in the matching
// converter instead of slipping through a catch-all arm.
// ===========================================================================

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use airplay_client::{ClientDiagnosticKind, DeviceId, FeedbackSnapshot};
use serde::Serialize;

use crate::backend::model::{ReceiverId, ReceiverLifecycle, SessionPhase};
use crate::calibration::EffectiveCalibration;

use super::snapshot::{
    BackendDiagnosticPayload, CalibrationApplyState, DefiniteCounterKind, DiagnosticComponent,
    DiagnosticError, DiagnosticErrorCode, DiagnosticEvent, DiagnosticSeverity, DiagnosticsSnapshot,
    EventGap, ExportEventState, Health, ReceiverTimingSnapshot, ReceiverTimingSource,
    ReceiverTransportSnapshot, Recoverability, SessionDiagnosticsSnapshot, SessionId,
    SessionStopReason, StructuredDiagnosticPayload, EVENT_RING_CAPACITY, MAX_EVENT_READ_LIMIT,
};

/// Stable schema name of the support export document.
pub const SUPPORT_EXPORT_SCHEMA: &str = "openaircast.diagnostics.support-export";

/// Version of [`SUPPORT_EXPORT_SCHEMA`]; incremented on any incompatible
/// change to the exported field set.
pub const SUPPORT_EXPORT_SCHEMA_VERSION: u16 = 1;

/// Media type support tooling identifies the export by.
pub const SUPPORT_EXPORT_MEDIA_TYPE: &str = "application/vnd.openaircast.diagnostics+json";

/// Product name written into the export; a compile-time constant so no
/// installation-specific string can take its place.
const SUPPORT_EXPORT_PRODUCT: &str = "OpenAirCast";

/// Exact semantics of the `accepted_local` counter family, spelled out so a
/// reader cannot mistake it for delivery or loss.
const LOCAL_OS_UDP_ACCEPTANCE: &str = "accepted_local counts datagrams the local operating system \
     accepted; it is not receiver delivery and not packet loss";

/// Identity classes the export deliberately drops.
const OMITTED_CLASSES: &[&str] = &[
    "receiver_and_device_names",
    "receiver_and_device_identifiers",
    "network_addresses_and_ports",
    "file_system_paths_and_user_names",
    "free_form_error_and_protocol_text",
    "pairing_secrets_and_keys",
    "audio_payload_and_packet_bytes",
];

// ---------------------------------------------------------------------------
// Aliases
//
// The alias newtypes and the table that mints them live in a private
// submodule. Rust visibility is per module, not per type: without this
// boundary the inner `String` would be reachable from everywhere else in
// `diagnostics/export.rs` -- including the converters that perform the redaction --
// so wrapping a receiver name in an alias newtype would compile right next to
// the code that is supposed to strip it. The submodule is what makes "only
// `AliasTable` can mint an alias" true inside this file as well.
// ---------------------------------------------------------------------------

mod alias {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt;

    use serde::Serialize;

    use super::{
        collect_event_identities, collect_snapshot_identities, DeviceId, ReceiverId, SessionId,
        SupportExportInput,
    };

    /// Per-export stand-in for one stable receiver identity.
    ///
    /// The inner string is private to this submodule and only [`AliasTable`]
    /// can mint one, so a receiver name or identifier cannot be smuggled into
    /// an alias field -- not from another module, and not from the export
    /// converters either.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
    #[serde(transparent)]
    pub struct ReceiverAlias(String);

    impl ReceiverAlias {
        fn numbered(position: usize) -> Self {
            Self(format!("R-{position:03}"))
        }

        /// Fail-closed alias for a receiver the alias walk did not reach. It
        /// carries no identity; it only makes the omission visible.
        fn unresolved() -> Self {
            Self("R-unknown".to_owned())
        }

        /// The alias text, e.g. `R-001`.
        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    impl fmt::Display for ReceiverAlias {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    /// Per-export stand-in for one session identity.
    ///
    /// Minted under the same rule as [`ReceiverAlias`].
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
    #[serde(transparent)]
    pub struct SessionAlias(String);

    impl SessionAlias {
        fn numbered(position: usize) -> Self {
            Self(format!("S-{position:03}"))
        }

        fn unresolved() -> Self {
            Self("S-unknown".to_owned())
        }

        /// The alias text, e.g. `S-001`.
        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    impl fmt::Display for SessionAlias {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    /// Alias assignment for exactly one export run.
    ///
    /// Receivers are numbered from the sorted set of encountered `ReceiverId`
    /// values, so the same input always yields the same table while the raw
    /// identity never leaves this struct. Sessions are numbered in
    /// first-encounter order of a fixed traversal (snapshot before events,
    /// events by cursor), which keeps the live session at `S-001` and stays
    /// deterministic.
    pub(super) struct AliasTable {
        receivers: BTreeMap<ReceiverId, ReceiverAlias>,
        sessions: Vec<(SessionId, SessionAlias)>,
        device_to_receiver: BTreeMap<[u8; 6], ReceiverId>,
    }

    impl AliasTable {
        pub(super) fn build(input: &SupportExportInput) -> Self {
            let mut receivers = BTreeSet::new();
            let mut sessions = Vec::new();
            collect_snapshot_identities(&input.latest_snapshot, &mut receivers, &mut sessions);
            for event in &input.retained_events {
                collect_event_identities(event, &mut receivers, &mut sessions);
            }

            let device_to_receiver = input
                .latest_snapshot
                .session
                .as_ref()
                .map(|session| {
                    session
                        .members
                        .iter()
                        .map(|(receiver_id, device_id)| (device_id.0, *receiver_id))
                        .collect()
                })
                .unwrap_or_default();

            Self {
                receivers: receivers
                    .into_iter()
                    .enumerate()
                    .map(|(index, receiver_id)| (receiver_id, ReceiverAlias::numbered(index + 1)))
                    .collect(),
                sessions: sessions
                    .into_iter()
                    .enumerate()
                    .map(|(index, session_id)| (session_id, SessionAlias::numbered(index + 1)))
                    .collect(),
                device_to_receiver,
            }
        }

        pub(super) fn receiver(&self, receiver_id: &ReceiverId) -> ReceiverAlias {
            self.receivers
                .get(receiver_id)
                .cloned()
                .unwrap_or_else(ReceiverAlias::unresolved)
        }

        pub(super) fn session(&self, session_id: &SessionId) -> SessionAlias {
            self.sessions
                .iter()
                .find(|(known, _)| known == session_id)
                .map(|(_, alias)| alias.clone())
                .unwrap_or_else(SessionAlias::unresolved)
        }

        /// Maps a transport-level device identity onto its session member
        /// alias.
        ///
        /// Returns `None` when the device is not a member of the retained
        /// session: an unmapped device gets no alias rather than an invented
        /// one.
        pub(super) fn device(&self, device_id: &DeviceId) -> Option<ReceiverAlias> {
            self.device_to_receiver
                .get(&device_id.0)
                .map(|receiver_id| self.receiver(receiver_id))
        }

        /// Whether the table knows this receiver at all; used to drop
        /// calibration entries for receivers the observed state never
        /// mentioned.
        pub(super) fn knows(&self, receiver_id: &ReceiverId) -> bool {
            self.receivers.contains_key(receiver_id)
        }
    }
}

use alias::AliasTable;
pub use alias::{ReceiverAlias, SessionAlias};

fn remember_session(sessions: &mut Vec<SessionId>, session_id: SessionId) {
    if !sessions.contains(&session_id) {
        sessions.push(session_id);
    }
}

fn collect_snapshot_identities(
    snapshot: &DiagnosticsSnapshot,
    receivers: &mut BTreeSet<ReceiverId>,
    sessions: &mut Vec<SessionId>,
) {
    if let Some(session_id) = snapshot.active_session_id {
        remember_session(sessions, session_id);
    }
    if let Some(session) = &snapshot.session {
        remember_session(sessions, session.session_id);
        if let Some(primary) = session.primary {
            receivers.insert(primary);
        }
        receivers.extend(session.members.keys().copied());
    }
    for row in &snapshot.receivers {
        receivers.insert(row.key.receiver_id);
        remember_session(sessions, row.key.session_id);
        collect_timing_identities(&row.timing.source, receivers);
        if let Some(error) = &row.last_error {
            collect_error_identities(error, receivers, sessions);
        }
    }
}

/// Exhaustive on purpose: a timing source that starts naming a receiver must
/// be listed here or the build stops.
fn collect_timing_identities(source: &ReceiverTimingSource, receivers: &mut BTreeSet<ReceiverId>) {
    match source {
        ReceiverTimingSource::PtpSharedFromPrimary { source_receiver_id } => {
            if let Some(primary) = source_receiver_id {
                receivers.insert(*primary);
            }
        }
        ReceiverTimingSource::Unavailable
        | ReceiverTimingSource::SenderReference
        | ReceiverTimingSource::PtpMeasuredAgainstThisMaster => {}
    }
}

fn collect_error_identities(
    error: &DiagnosticError,
    receivers: &mut BTreeSet<ReceiverId>,
    sessions: &mut Vec<SessionId>,
) {
    if let Some(key) = &error.receiver_session {
        receivers.insert(key.receiver_id);
        remember_session(sessions, key.session_id);
    }
}

fn collect_event_identities(
    event: &DiagnosticEvent,
    receivers: &mut BTreeSet<ReceiverId>,
    sessions: &mut Vec<SessionId>,
) {
    if let Some(key) = &event.receiver_session {
        receivers.insert(key.receiver_id);
        remember_session(sessions, key.session_id);
    }
    collect_payload_identities(&event.payload, receivers, sessions);
}

/// Exhaustive on purpose: a future payload that carries a receiver or session
/// must be listed here. A missed variant would not leak an identity -- the
/// converter falls back to [`ReceiverAlias::unresolved`] -- but it would
/// silently drop the join support needs, so the decision is forced.
fn collect_payload_identities(
    payload: &StructuredDiagnosticPayload,
    receivers: &mut BTreeSet<ReceiverId>,
    sessions: &mut Vec<SessionId>,
) {
    match payload {
        StructuredDiagnosticPayload::Backend(backend) => {
            collect_backend_identities(backend, receivers);
        }
        StructuredDiagnosticPayload::Error(error) => {
            collect_error_identities(error, receivers, sessions);
        }
        // Client events are keyed by a transport `DeviceId`, which is resolved
        // through the session member map rather than introducing a receiver.
        StructuredDiagnosticPayload::Client(_)
        | StructuredDiagnosticPayload::DefiniteCounterTransition { .. }
        | StructuredDiagnosticPayload::CalibrationState(_)
        | StructuredDiagnosticPayload::ExportState(_) => {}
    }
}

/// Exhaustive on purpose, for the same reason as
/// [`collect_payload_identities`].
fn collect_backend_identities(
    payload: &BackendDiagnosticPayload,
    receivers: &mut BTreeSet<ReceiverId>,
) {
    match payload {
        BackendDiagnosticPayload::DiscoveryReceiver { receiver, .. }
        | BackendDiagnosticPayload::ReceiverTransition { receiver, .. }
        | BackendDiagnosticPayload::SetupPhaseDuration { receiver, .. } => {
            receivers.insert(*receiver);
        }
        BackendDiagnosticPayload::Retry { receiver, .. } => {
            if let Some(receiver) = receiver {
                receivers.insert(*receiver);
            }
        }
        BackendDiagnosticPayload::Message(_)
        | BackendDiagnosticPayload::DiscoveryGeneration { .. }
        | BackendDiagnosticPayload::CommandResult { .. }
        | BackendDiagnosticPayload::CommandCoalesced { .. }
        | BackendDiagnosticPayload::SessionTransition { .. }
        | BackendDiagnosticPayload::SessionRestart { .. }
        | BackendDiagnosticPayload::CaptureTransition { .. }
        | BackendDiagnosticPayload::PcmDrop { .. }
        | BackendDiagnosticPayload::SilenceBridge { .. }
        | BackendDiagnosticPayload::Probe { .. }
        | BackendDiagnosticPayload::Persistence { .. }
        | BackendDiagnosticPayload::QueueWatermark { .. }
        | BackendDiagnosticPayload::QueueLag { .. }
        | BackendDiagnosticPayload::WorkerShutdown { .. }
        | BackendDiagnosticPayload::SystemTransition { .. }
        // Calibration edges are group-wide: the profile, its receivers, and
        // its values never enter a record, so there is no receiver to alias.
        | BackendDiagnosticPayload::CalibrationState(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Export input and context
// ---------------------------------------------------------------------------

/// Everything one export run reads out of the registry.
///
/// The plan names this type `pub(crate)`; it is `pub` here because
/// [`build_support_export`] and [`build_support_summary`] are public and Rust
/// refuses a public function over a crate-private argument. All of its fields
/// are already-public diagnostics types, so nothing new is exposed.
#[derive(Clone, Debug)]
pub struct SupportExportInput {
    /// Latest published immutable snapshot.
    pub latest_snapshot: DiagnosticsSnapshot,
    /// Every event still retained by the ring, oldest first.
    pub retained_events: Vec<DiagnosticEvent>,
    /// Overwrite gap of the retained window, when the ring wrapped.
    pub event_gap: Option<EventGap>,
    /// Diagnostic events lost to producer/feed lag.
    pub producer_drops_total: u64,
}

/// Fixed audio configuration reported alongside the observed counters.
#[derive(Clone, Copy, Debug)]
pub struct SupportAudioConfiguration {
    /// Capture and stream sample rate.
    pub sample_rate_hz: u32,
    /// Channel count of the shared stream.
    pub channels: u16,
    /// Samples per channel in one RTP frame.
    pub frame_samples_per_channel: u32,
    /// Configured capacity of the bounded capture bridge queue.
    pub capture_queue_capacity_frames: u32,
}

/// Calibration state handed to the export by the application layer.
#[derive(Clone, Debug)]
pub struct SupportCalibrationContext {
    /// Reference receiver the signed relative delays were judged against.
    pub reference_receiver: Option<ReceiverId>,
    /// Normalized non-negative delays actually handed to session setup.
    pub effective: EffectiveCalibration,
    /// Lifecycle state of the last apply, if one happened.
    pub apply_state: Option<CalibrationApplyState>,
}

/// Build and configuration metadata the registry does not own.
#[derive(Clone, Debug)]
pub struct SupportExportContext {
    /// Wall-clock stamp written into the document.
    pub exported_at_utc: SystemTime,
    /// Application version string; a compile-time constant in production.
    pub application_version: &'static str,
    /// Cargo profile the binary was built with.
    pub build_profile: &'static str,
    /// Host operating system family.
    pub platform_os: &'static str,
    /// Host CPU architecture.
    pub platform_arch: &'static str,
    /// Windows build number, when the caller resolved one.
    pub windows_build: Option<u32>,
    /// Fixed audio configuration.
    pub audio: SupportAudioConfiguration,
    /// Active calibration, when one exists.
    pub calibration: Option<SupportCalibrationContext>,
}

impl SupportExportContext {
    /// Fills the build-identifying fields from compile-time constants.
    ///
    /// Only `&'static str` values from the toolchain reach these fields, so
    /// the machine the export was produced on stays unidentified.
    pub fn for_this_build(
        exported_at_utc: SystemTime,
        audio: SupportAudioConfiguration,
        calibration: Option<SupportCalibrationContext>,
    ) -> Self {
        Self {
            exported_at_utc,
            application_version: env!("CARGO_PKG_VERSION"),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            platform_os: std::env::consts::OS,
            platform_arch: std::env::consts::ARCH,
            windows_build: None,
            audio,
            calibration,
        }
    }
}

/// The finished export document.
#[derive(Clone, Debug)]
pub struct SupportExportArtifact {
    /// Always [`SUPPORT_EXPORT_MEDIA_TYPE`].
    pub media_type: &'static str,
    /// UTF-8 JSON bytes.
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Allow-listed DTOs
// ---------------------------------------------------------------------------

/// Root of the support export document.
#[derive(Debug, Serialize)]
pub struct SupportExportV1 {
    /// Always [`SUPPORT_EXPORT_SCHEMA`].
    pub schema: &'static str,
    /// Always [`SUPPORT_EXPORT_SCHEMA_VERSION`].
    pub schema_version: u16,
    /// RFC 3339 UTC stamp of the export run.
    pub exported_at_utc: String,
    /// What this document deliberately does not contain.
    pub redaction: RedactionDescriptor,
    /// Build metadata.
    pub application: SupportApplication,
    /// Fixed configuration and calibration.
    pub configuration: SupportConfiguration,
    /// Redacted view of the latest published snapshot.
    pub latest_snapshot: SupportSnapshot,
    /// Number of retained events in `events`.
    pub events_retained: usize,
    /// Diagnostic events lost to producer/feed lag.
    pub producer_drops_total: u64,
    /// Events lost to ring overwrite before this window, when any were.
    pub event_gap: Option<SupportEventGap>,
    /// Redacted retained events, oldest first.
    pub events: Vec<SupportEvent>,
}

/// Machine-readable statement of the export's redaction policy.
#[derive(Debug, Serialize)]
pub struct RedactionDescriptor {
    /// Always `allow_list`: fields are copied in, never filtered out.
    pub policy: &'static str,
    /// Always `false`: there is no unredacted export mode.
    pub unredacted_mode_available: bool,
    /// Always `per_export_alias`.
    pub receiver_identity: &'static str,
    /// Identity classes the document omits.
    pub omitted: &'static [&'static str],
}

/// Build metadata of the sender that produced the document.
#[derive(Debug, Serialize)]
pub struct SupportApplication {
    /// Always [`SUPPORT_EXPORT_PRODUCT`].
    pub product: &'static str,
    /// Application version.
    pub version: &'static str,
    /// Cargo profile.
    pub build_profile: &'static str,
    /// Host operating system family.
    pub platform_os: &'static str,
    /// Host CPU architecture.
    pub platform_arch: &'static str,
    /// Windows build number, when known.
    pub windows_build: Option<u32>,
    /// Monotonic process age at export time.
    pub process_elapsed_ns: u64,
}

/// Fixed configuration and the limits every number above must be read against.
#[derive(Debug, Serialize)]
pub struct SupportConfiguration {
    /// Capture and stream sample rate.
    pub sample_rate_hz: u32,
    /// Channel count of the shared stream.
    pub channels: u16,
    /// Samples per channel in one RTP frame.
    pub frame_samples_per_channel: u32,
    /// Configured capacity of the bounded capture bridge queue.
    pub capture_queue_capacity_frames: u32,
    /// Fixed capacity of the diagnostic event ring.
    pub event_ring_capacity: usize,
    /// Inclusive upper bound of one public event read.
    pub max_event_read_limit: usize,
    /// Always `true`: the sender configures a requested latency and never
    /// measures an end-to-end or acoustic one.
    pub requested_latency_not_measured: bool,
    /// Active calibration, when one exists.
    pub calibration: Option<SupportCalibration>,
}

/// Calibration as applied, expressed entirely in aliases.
#[derive(Debug, Serialize)]
pub struct SupportCalibration {
    /// Alias of the reference receiver, when one is set and observed.
    pub reference_receiver_alias: Option<ReceiverAlias>,
    /// Lifecycle state of the last apply.
    pub apply_state: Option<&'static str>,
    /// Smallest signed requested delay; the value normalization shifted away.
    pub minimum_requested_ns: i64,
    /// Effective added presentation delay per observed receiver.
    pub effective_delay_ns_by_receiver: BTreeMap<ReceiverAlias, u64>,
    /// Profile entries dropped because their receiver never appears in the
    /// observed state; disclosed instead of silently omitted.
    pub entries_omitted_for_unknown_receivers: usize,
}

/// Redacted view of one [`DiagnosticsSnapshot`].
#[derive(Debug, Serialize)]
pub struct SupportSnapshot {
    /// Schema version of the source snapshot.
    pub snapshot_schema_version: u16,
    /// Process-lifetime publication number.
    pub snapshot_sequence: u64,
    /// RFC 3339 UTC capture stamp.
    pub captured_at_utc: String,
    /// Monotonic process age at capture.
    pub process_elapsed_ns: u64,
    /// Alias of the active session, when one runs.
    pub active_session_alias: Option<SessionAlias>,
    /// Conservative health badge.
    pub health: &'static str,
    /// Active or most recently completed session.
    pub session: Option<SupportSession>,
    /// One row per observed receiver, ordered by alias.
    pub receivers: Vec<SupportReceiver>,
    /// Diagnostic events lost to producer/feed lag.
    pub diagnostics_events_dropped_total: u64,
}

/// Redacted view of one session.
#[derive(Debug, Serialize)]
pub struct SupportSession {
    /// Per-export session alias.
    pub session_alias: SessionAlias,
    /// Monotonic start stamp supplied at registration.
    pub started_elapsed_ns: u64,
    /// Monotonic finish stamp, once finished.
    pub finished_elapsed_ns: Option<u64>,
    /// Why the session stopped, once finished.
    pub stop_reason: Option<&'static str>,
    /// Last authoritative whole-session phase.
    pub phase: Option<SupportSessionPhase>,
    /// Alias of the PTP primary, when the group runs shared timing.
    pub primary_receiver_alias: Option<ReceiverAlias>,
    /// Aliases of every session member.
    pub member_receiver_aliases: Vec<ReceiverAlias>,
    /// Member count, so a truncated alias list can never hide members.
    pub member_count: usize,
    /// Shared audio counters.
    pub audio: Option<SupportAudio>,
}

/// Redacted view of one whole-session phase.
#[derive(Debug, Serialize)]
pub struct SupportSessionPhase {
    /// Phase name.
    pub phase: &'static str,
    /// Generation tag, when the phase carries one.
    pub generation: Option<u64>,
    /// Restart reason, when the phase carries one.
    pub restart_reason: Option<&'static str>,
    /// Whether the phase carried a free-form failure text that was withheld.
    pub free_form_text_omitted: bool,
}

/// Shared audio half of the session.
#[derive(Debug, Serialize)]
pub struct SupportAudio {
    /// Ring-buffer half.
    pub sender_buffer: SupportSenderBuffer,
    /// Capture-queue half.
    pub capture_queue: SupportCaptureQueue,
    /// Scheduler dispatch jitter distribution.
    pub scheduler: SupportScheduler,
    /// Shared RTP frames encoded and handed to the transport.
    pub rtp_frames_prepared_total: u64,
    /// Cumulative retransmit handling.
    pub retransmit: SupportRetransmit,
    /// How many send targets the streamer held; the per-target rows
    /// themselves are keyed by sender index and are therefore not exported.
    pub target_count: usize,
}

/// Shared sender ring buffer.
#[derive(Debug, Serialize)]
pub struct SupportSenderBuffer {
    /// Frames currently queued.
    pub queued_frames: u32,
    /// Total capacity in frames.
    pub capacity_frames: u32,
    /// Samples per channel currently queued.
    pub queued_samples_per_channel: u64,
    /// Queued audio duration.
    pub buffered_ns: u64,
    /// Occupancy ratio. A value that is not finite serializes as `null` --
    /// "unavailable" -- and never as zero.
    pub fill_ratio: f64,
    /// Samples per channel pushed.
    pub samples_written_total: u64,
    /// Samples per channel popped.
    pub samples_read_total: u64,
    /// Pops that hit an empty buffer.
    pub underrun_events_total: u64,
}

/// Bounded capture bridge queue.
#[derive(Debug, Serialize)]
pub struct SupportCaptureQueue {
    /// Frames currently queued.
    pub queued_frames: u32,
    /// Observed capacity in frames.
    pub capacity_frames: u32,
    /// Submit attempts, successes and drops alike.
    pub submitted_frames_total: u64,
    /// Samples per channel of every attempted frame.
    pub submitted_samples_per_channel_total: u64,
    /// Attempts rejected because the queue was full.
    pub full_queue_drops_total: u64,
    /// Attempts rejected because the consumer was gone.
    pub disconnected_drops_total: u64,
}

/// Scheduler dispatch jitter: how late the sender dispatched, never network
/// jitter and never receiver timing.
#[derive(Debug, Serialize)]
pub struct SupportScheduler {
    /// Number of dispatch samples behind the statistics.
    pub sample_count: u64,
    /// Newest signed dispatch deviation.
    pub last_signed_scheduler_dispatch_jitter_ns: Option<i64>,
    /// Mean absolute dispatch deviation.
    pub mean_absolute_scheduler_dispatch_jitter_ns: Option<u64>,
    /// 95th percentile absolute dispatch deviation.
    pub p95_absolute_scheduler_dispatch_jitter_ns: Option<u64>,
    /// Largest absolute dispatch deviation.
    pub maximum_absolute_scheduler_dispatch_jitter_ns: Option<u64>,
    /// Times the dispatch deadline had to be re-anchored.
    pub deadline_resets_total: u64,
}

/// Cumulative retransmit handling across every target.
#[derive(Debug, Serialize)]
pub struct SupportRetransmit {
    /// Retransmit request datagrams handled.
    pub retransmit_request_datagrams_total: u64,
    /// Sequence slots requested across all of them.
    pub retransmit_packet_slots_requested_total: u64,
    /// Replies the local operating system accepted.
    pub retransmit_datagrams_accepted_local_total: u64,
    /// Requested sequences no longer in history.
    pub retransmit_history_misses_total: u64,
    /// Replies whose local send failed.
    pub retransmit_send_failures_total: u64,
}

/// One redacted receiver row.
#[derive(Debug, Serialize)]
pub struct SupportReceiver {
    /// Per-export receiver alias.
    pub receiver_alias: ReceiverAlias,
    /// Authoritative lifecycle state.
    pub lifecycle: SupportLifecycle,
    /// Timing half.
    pub timing: SupportTiming,
    /// Transport half.
    pub transport: SupportTransport,
    /// Feedback totals.
    pub feedback: SupportFeedback,
    /// Last classified error, reduced to its stable classification.
    pub last_error: Option<SupportError>,
}

/// Redacted receiver lifecycle state.
#[derive(Debug, Serialize)]
pub struct SupportLifecycle {
    /// State name.
    pub state: &'static str,
    /// Attempt number, when the state carries one.
    pub attempt: Option<u32>,
    /// Negotiated or targeted role, when the state carries one.
    pub role: Option<&'static str>,
    /// Transport-reported setup phase, when the state carries one.
    pub setup_phase: Option<&'static str>,
    /// Whether retry policy may try again, when the state carries it.
    pub retryable: Option<bool>,
    /// Whether the state carried a free-form failure text that was withheld.
    pub free_form_text_omitted: bool,
}

/// Redacted receiver timing half.
#[derive(Debug, Serialize)]
pub struct SupportTiming {
    /// Presentation-level timing source.
    pub source: &'static str,
    /// Alias of the primary a secondary derives from.
    pub shared_from_receiver_alias: Option<ReceiverAlias>,
    /// Whether this row carries its own measurement.
    pub independently_measured: bool,
    /// Signed local-minus-master offset.
    pub offset_local_minus_master_ns: Option<i64>,
    /// Mean path-delay estimate.
    pub mean_path_delay_ns: Option<u64>,
    /// Estimated relative clock drift; never a clock-accuracy claim. A value
    /// that is not finite serializes as `null`, never as zero.
    pub estimated_relative_clock_drift_ppm: Option<f64>,
    /// Age of the newest accepted sample.
    pub sample_age_ns: Option<u64>,
    /// Whether timing observations went stale.
    pub stale: bool,
}

/// Redacted receiver transport half.
#[derive(Debug, Serialize)]
pub struct SupportTransport {
    /// Audio datagram send attempts.
    pub data_datagrams_attempted_total: u64,
    /// Audio datagrams the local OS accepted.
    pub data_datagrams_accepted_local_total: u64,
    /// Audio datagram send failures.
    pub data_datagram_send_failures_total: u64,
    /// Bytes the local OS reported sent for audio.
    pub data_bytes_accepted_local_total: u64,
    /// Sync datagram send attempts.
    pub sync_datagrams_attempted_total: u64,
    /// Sync datagrams the local OS accepted.
    pub sync_datagrams_accepted_local_total: u64,
    /// Sync datagram send failures.
    pub sync_datagram_send_failures_total: u64,
    /// Bytes the local OS reported sent for sync.
    pub sync_bytes_accepted_local_total: u64,
    /// Retransmit slots this receiver asked for.
    pub retransmit_slots_requested_total: u64,
    /// Requested slots answered and locally accepted.
    pub retransmit_slots_accepted_local_total: u64,
    /// Numerator of the recovery-demand ratio.
    pub recovery_demand_numerator_slots: u64,
    /// Denominator: shared frames prepared while a member.
    pub recovery_demand_denominator_shared_frames: u64,
    /// The ratio; `null` while the denominator is zero or the value is not
    /// finite. Never zero for an unavailable ratio.
    pub recovery_demand_ratio: Option<f64>,
    /// Exact meaning of the `accepted_local` counters above.
    pub local_os_udp_acceptance: &'static str,
}

/// Redacted feedback totals.
#[derive(Debug, Serialize)]
pub struct SupportFeedback {
    /// Transactions attempted.
    pub attempts_total: u64,
    /// Transactions that succeeded.
    pub successes_total: u64,
    /// Rejections by the receiver or protocol layer.
    pub protocol_failures_total: u64,
    /// Local socket failures.
    pub transport_failures_total: u64,
    /// Transactions without a terminal response in time.
    pub timeouts_total: u64,
    /// Classification of the newest terminal result.
    pub last_result: Option<&'static str>,
    /// Duration of the newest attempted control request.
    pub last_control_request_duration_ns: Option<u64>,
    /// Monotonic stamp of the newest success.
    pub last_success_elapsed_ns: Option<u64>,
}

/// Redacted error record: classification only, never message text.
#[derive(Debug, Serialize)]
pub struct SupportError {
    /// Stable failure class.
    pub code: &'static str,
    /// Producing subsystem.
    pub component: &'static str,
    /// Presentation severity.
    pub severity: &'static str,
    /// How the error can clear.
    pub recoverability: &'static str,
    /// Static operation name; a compile-time literal in the domain.
    pub operation: &'static str,
    /// Alias of the affected receiver, when the error is receiver-scoped.
    pub receiver_alias: Option<ReceiverAlias>,
    /// Always `true`: the domain's public message and technical detail are
    /// free-form strings and are therefore withheld, not exported.
    pub free_form_text_omitted: bool,
}

/// Explicit disclosure that the ring overwrote events before this window.
#[derive(Debug, Serialize)]
pub struct SupportEventGap {
    /// Cursor the window was anchored at.
    pub requested_cursor: u64,
    /// Oldest cursor actually retained.
    pub resumed_at_cursor: u64,
    /// Exact number of events the ring overwrote.
    pub overwritten_events: u64,
}

/// One redacted retained event.
#[derive(Debug, Serialize)]
pub struct SupportEvent {
    /// Monotonic ring cursor.
    pub cursor: u64,
    /// RFC 3339 UTC occurrence stamp.
    pub occurred_at_utc: String,
    /// Monotonic process age at occurrence.
    pub process_elapsed_ns: u64,
    /// Alias of the affected session, when the event is session-scoped.
    pub session_alias: Option<SessionAlias>,
    /// Alias of the affected receiver, when the event is receiver-scoped.
    pub receiver_alias: Option<ReceiverAlias>,
    /// Presentation severity.
    pub severity: &'static str,
    /// Producing subsystem.
    pub component: &'static str,
    /// Stable failure class.
    pub code: &'static str,
    /// Typed structured content.
    pub payload: SupportEventPayload,
}

/// Typed, allow-listed replacement for [`StructuredDiagnosticPayload`].
///
/// Every variant carries numbers, bools, aliases, and `&'static str` labels
/// only. Variants whose source held free-form text say so through
/// `free_form_text_omitted` instead of guessing at a replacement.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupportEventPayload {
    /// A backend message whose only content was free-form text.
    BackendMessage {
        /// Always `true`.
        free_form_text_omitted: bool,
    },
    /// Discovery supervisor generation edge.
    DiscoveryGeneration {
        /// Generation the edge belongs to.
        generation: u64,
        /// Supervisor phase reached.
        phase: &'static str,
        /// Retry attempt number, when the phase carries one.
        attempt: Option<u32>,
    },
    /// A receiver appeared in or left discovery.
    DiscoveryReceiver {
        /// Alias of the receiver.
        receiver_alias: ReceiverAlias,
        /// Whether it is now present.
        present: bool,
    },
    /// A backend command completed or failed.
    CommandResult {
        /// Whether it failed.
        failed: bool,
        /// Whether a failure text was withheld.
        free_form_text_omitted: bool,
    },
    /// Commands merged away during coalescing.
    CommandCoalesced {
        /// How many were superseded.
        superseded: u32,
    },
    /// Receiver lifecycle transition.
    ReceiverTransition {
        /// Alias of the receiver.
        receiver_alias: ReceiverAlias,
        /// State entered.
        lifecycle: SupportLifecycle,
    },
    /// Measured duration of one transport-reported setup phase.
    SetupPhaseDuration {
        /// Alias of the receiver.
        receiver_alias: ReceiverAlias,
        /// Phase that completed.
        phase: &'static str,
        /// Time spent in the phase.
        duration_ns: u64,
    },
    /// Whole-session phase transition.
    SessionTransition {
        /// Phase entered.
        phase: SupportSessionPhase,
        /// Receivers active when it committed.
        active_members: u32,
    },
    /// Announced full-session restart.
    SessionRestart {
        /// Generation being replaced.
        generation: u64,
        /// Matrix reason.
        reason: &'static str,
    },
    /// Retry scheduled for a receiver or a discovery generation.
    Retry {
        /// Alias of the receiver, for receiver-scoped retries.
        receiver_alias: Option<ReceiverAlias>,
        /// Zero-based attempt number that will run.
        attempt: u32,
    },
    /// Capture availability transition.
    CaptureTransition {
        /// State entered.
        state: &'static str,
    },
    /// Live capture frames dropped by the bounded bridge.
    CaptureDrop {
        /// Frames dropped since the last report.
        dropped_frames: u64,
    },
    /// Duration the bridge paced silence instead of live capture.
    SilenceBridge {
        /// Accumulated silence.
        duration_ns: u64,
    },
    /// Per-receiver feedback probe result.
    Probe {
        /// Whether the receiver answered within bounds.
        healthy: bool,
        /// Resolved control-request duration.
        control_request_duration_ms: u64,
    },
    /// Persistence cycle outcome.
    Persistence {
        /// Health of the latest cycle.
        outcome: &'static str,
    },
    /// Observed queue high-water mark.
    QueueWatermark {
        /// Deepest observed occupancy.
        depth: u32,
        /// Fixed capacity of that queue.
        capacity: u32,
    },
    /// Typed gap for events lost to feed lag.
    DiagnosticFeedLag {
        /// Events dropped.
        dropped_events: u64,
    },
    /// Bounded worker shutdown.
    WorkerShutdown {
        /// Time from cancellation to joined workers.
        duration_ns: u64,
        /// Whether a bounded drain hit its budget.
        timed_out: bool,
    },
    /// Platform sleep or network edge applied by the controller.
    SystemTransition {
        /// Edge that was applied.
        transition: &'static str,
        /// Whether an active local binding moved, for network edges.
        local_binding_changed: Option<bool>,
    },
    /// Significant transport-client event.
    ClientEvent {
        /// Alias of the receiver, when the device maps to a session member.
        receiver_alias: Option<ReceiverAlias>,
        /// Client event classification.
        client_kind: &'static str,
        /// Monotonic stamp from the client's clock domain.
        elapsed_ns: u64,
    },
    /// Fully structured error record.
    Error {
        /// The classified error.
        error: SupportError,
    },
    /// A definite counter crossed from one total to the next.
    DefiniteCounterTransition {
        /// Which counter moved.
        counter: &'static str,
        /// Total before.
        previous_total: u64,
        /// Total after.
        current_total: u64,
    },
    /// Calibration apply lifecycle edge.
    CalibrationState {
        /// State reached.
        state: &'static str,
    },
    /// Support export lifecycle edge.
    ExportState {
        /// State reached.
        state: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Builds the versioned, support-safe export document.
///
/// Fails only if the serializer rejects the allow-listed DTO. Nothing in the
/// DTO is known to trigger that today -- `serde_json` writes a non-finite
/// float as `null`, which is exactly the "unavailable" the diagnostics domain
/// means -- so the error path is a guard, not a routine outcome.
pub fn build_support_export(
    input: &SupportExportInput,
    context: &SupportExportContext,
) -> Result<SupportExportArtifact, DiagnosticError> {
    let aliases = AliasTable::build(input);
    let document = SupportExportV1 {
        schema: SUPPORT_EXPORT_SCHEMA,
        schema_version: SUPPORT_EXPORT_SCHEMA_VERSION,
        exported_at_utc: rfc3339_utc(context.exported_at_utc),
        redaction: RedactionDescriptor {
            policy: "allow_list",
            unredacted_mode_available: false,
            receiver_identity: "per_export_alias",
            omitted: OMITTED_CLASSES,
        },
        application: SupportApplication {
            product: SUPPORT_EXPORT_PRODUCT,
            version: context.application_version,
            build_profile: context.build_profile,
            platform_os: context.platform_os,
            platform_arch: context.platform_arch,
            windows_build: context.windows_build,
            process_elapsed_ns: input.latest_snapshot.process_elapsed_ns,
        },
        configuration: SupportConfiguration {
            sample_rate_hz: context.audio.sample_rate_hz,
            channels: context.audio.channels,
            frame_samples_per_channel: context.audio.frame_samples_per_channel,
            capture_queue_capacity_frames: context.audio.capture_queue_capacity_frames,
            event_ring_capacity: EVENT_RING_CAPACITY,
            max_event_read_limit: MAX_EVENT_READ_LIMIT,
            requested_latency_not_measured: true,
            calibration: context
                .calibration
                .as_ref()
                .map(|calibration| support_calibration(calibration, &aliases)),
        },
        latest_snapshot: support_snapshot(&input.latest_snapshot, &aliases),
        events_retained: input.retained_events.len(),
        producer_drops_total: input.producer_drops_total,
        event_gap: input.event_gap.map(|gap| SupportEventGap {
            requested_cursor: gap.requested_cursor.0,
            resumed_at_cursor: gap.resumed_at_cursor.0,
            overwritten_events: gap.overwritten_events,
        }),
        events: input
            .retained_events
            .iter()
            .map(|event| support_event(event, &aliases))
            .collect(),
    };

    match serde_json::to_vec_pretty(&document) {
        Ok(bytes) => Ok(SupportExportArtifact {
            media_type: SUPPORT_EXPORT_MEDIA_TYPE,
            bytes,
        }),
        Err(_) => Err(DiagnosticError {
            code: DiagnosticErrorCode::ExportFailure,
            component: DiagnosticComponent::Export,
            severity: DiagnosticSeverity::Error,
            recoverability: Recoverability::UserAction,
            operation: "support_export_encode",
            receiver_session: None,
            public_message: "the diagnostics export could not be encoded".to_owned(),
            technical_detail: None,
        }),
    }
}

/// Renders the copy summary from exactly the same redaction the export uses.
///
/// Takes no context on purpose: the summary is meant to be pasted into a
/// support conversation and therefore names only observed state, in aliases.
pub fn build_support_summary(input: &SupportExportInput) -> String {
    let aliases = AliasTable::build(input);
    let snapshot = support_snapshot(&input.latest_snapshot, &aliases);

    let mut out = String::new();
    out.push_str("OpenAirCast diagnostics summary\n");
    out.push_str(&format!(
        "schema {SUPPORT_EXPORT_SCHEMA} v{SUPPORT_EXPORT_SCHEMA_VERSION}\n"
    ));
    out.push_str(&format!("captured at {}\n", snapshot.captured_at_utc));
    out.push_str(&format!("health: {}\n", snapshot.health));

    match &snapshot.session {
        Some(session) => {
            let phase = session
                .phase
                .as_ref()
                .map_or("not reported", |phase| phase.phase);
            out.push_str(&format!(
                "session {}: phase {phase}, {} receivers\n",
                session.session_alias, session.member_count
            ));
        }
        None => out.push_str("session: none\n"),
    }

    out.push_str("receivers:\n");
    if snapshot.receivers.is_empty() {
        out.push_str("  none observed\n");
    }
    for row in &snapshot.receivers {
        out.push_str(&format!(
            "  {} {} timing {} offset_local_minus_master_ns {} udp send failures {}\n",
            row.receiver_alias,
            row.lifecycle.state,
            row.timing.source,
            row.timing
                .offset_local_minus_master_ns
                .map_or_else(|| "not measured".to_owned(), |value| value.to_string()),
            row.transport.data_datagram_send_failures_total,
        ));
        if let Some(error) = &row.last_error {
            out.push_str(&format!(
                "    last error {} in {} ({})\n",
                error.code, error.component, error.recoverability
            ));
        }
    }

    out.push_str(&format!(
        "events retained: {}\n",
        input.retained_events.len()
    ));
    if let Some(gap) = input.event_gap {
        out.push_str(&format!(
            "events overwritten before this read: {}\n",
            gap.overwritten_events
        ));
    }
    out.push_str(&format!(
        "diagnostic events dropped by producer lag: {}\n",
        input.producer_drops_total
    ));
    out.push_str(
        "redaction: allow list -- no names, addresses, paths, identifiers, or keys are included\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

fn support_calibration(
    calibration: &SupportCalibrationContext,
    aliases: &AliasTable,
) -> SupportCalibration {
    let mut effective_delay_ns_by_receiver = BTreeMap::new();
    let mut entries_omitted_for_unknown_receivers = 0;
    for (receiver_id, delay_ns) in &calibration.effective.effective_delay_ns {
        if aliases.knows(receiver_id) {
            effective_delay_ns_by_receiver.insert(aliases.receiver(receiver_id), *delay_ns);
        } else {
            entries_omitted_for_unknown_receivers += 1;
        }
    }
    SupportCalibration {
        reference_receiver_alias: calibration
            .reference_receiver
            .filter(|receiver_id| aliases.knows(receiver_id))
            .map(|receiver_id| aliases.receiver(&receiver_id)),
        apply_state: calibration.apply_state.map(calibration_apply_state_label),
        minimum_requested_ns: calibration.effective.minimum_requested_ns,
        effective_delay_ns_by_receiver,
        entries_omitted_for_unknown_receivers,
    }
}

fn support_snapshot(snapshot: &DiagnosticsSnapshot, aliases: &AliasTable) -> SupportSnapshot {
    let mut receivers: Vec<SupportReceiver> = snapshot
        .receivers
        .iter()
        .map(|row| SupportReceiver {
            receiver_alias: aliases.receiver(&row.key.receiver_id),
            lifecycle: support_lifecycle(&row.lifecycle),
            timing: support_timing(&row.timing, aliases),
            transport: support_transport(&row.transport),
            feedback: support_feedback(&row.feedback),
            last_error: row
                .last_error
                .as_ref()
                .map(|error| support_error(error, aliases)),
        })
        .collect();
    receivers.sort_by(|left, right| left.receiver_alias.cmp(&right.receiver_alias));

    SupportSnapshot {
        snapshot_schema_version: snapshot.schema_version,
        snapshot_sequence: snapshot.snapshot_sequence,
        captured_at_utc: rfc3339_utc(snapshot.captured_at_utc),
        process_elapsed_ns: snapshot.process_elapsed_ns,
        active_session_alias: snapshot
            .active_session_id
            .map(|session_id| aliases.session(&session_id)),
        health: health_label(snapshot.health),
        session: snapshot
            .session
            .as_ref()
            .map(|session| support_session(session, aliases)),
        receivers,
        diagnostics_events_dropped_total: snapshot.diagnostics_events_dropped_total,
    }
}

fn support_session(session: &SessionDiagnosticsSnapshot, aliases: &AliasTable) -> SupportSession {
    SupportSession {
        session_alias: aliases.session(&session.session_id),
        started_elapsed_ns: session.started_elapsed_ns,
        finished_elapsed_ns: session.finished_elapsed_ns,
        stop_reason: session.stop_reason.map(stop_reason_label),
        phase: session.phase.as_ref().map(support_session_phase),
        primary_receiver_alias: session
            .primary
            .map(|receiver_id| aliases.receiver(&receiver_id)),
        member_receiver_aliases: session
            .members
            .keys()
            .map(|receiver_id| aliases.receiver(receiver_id))
            .collect(),
        member_count: session.members.len(),
        audio: session.audio.as_ref().map(support_audio),
    }
}

fn support_audio(audio: &airplay_client::AudioDiagnosticsSnapshot) -> SupportAudio {
    SupportAudio {
        sender_buffer: SupportSenderBuffer {
            queued_frames: audio.sender_buffer.queued_frames,
            capacity_frames: audio.sender_buffer.capacity_frames,
            queued_samples_per_channel: audio.sender_buffer.queued_samples_per_channel,
            buffered_ns: audio.sender_buffer.buffered_ns,
            fill_ratio: f64::from(audio.sender_buffer.fill_ratio),
            samples_written_total: audio.sender_buffer.samples_written_total,
            samples_read_total: audio.sender_buffer.samples_read_total,
            underrun_events_total: audio.sender_buffer.underrun_events_total,
        },
        capture_queue: SupportCaptureQueue {
            queued_frames: audio.capture_queue.queued_frames,
            capacity_frames: audio.capture_queue.capacity_frames,
            submitted_frames_total: audio.capture_queue.submitted_frames_total,
            submitted_samples_per_channel_total: audio
                .capture_queue
                .submitted_samples_per_channel_total,
            full_queue_drops_total: audio.capture_queue.full_queue_drops_total,
            disconnected_drops_total: audio.capture_queue.disconnected_drops_total,
        },
        scheduler: SupportScheduler {
            sample_count: audio.scheduler.sample_count,
            last_signed_scheduler_dispatch_jitter_ns: audio.scheduler.last_signed_jitter_ns,
            mean_absolute_scheduler_dispatch_jitter_ns: audio.scheduler.mean_absolute_jitter_ns,
            p95_absolute_scheduler_dispatch_jitter_ns: audio.scheduler.p95_absolute_jitter_ns,
            maximum_absolute_scheduler_dispatch_jitter_ns: audio
                .scheduler
                .maximum_absolute_jitter_ns,
            deadline_resets_total: audio.scheduler.deadline_resets_total,
        },
        rtp_frames_prepared_total: audio.rtp_frames_prepared_total,
        retransmit: SupportRetransmit {
            retransmit_request_datagrams_total: audio.retransmit.retransmit_request_datagrams_total,
            retransmit_packet_slots_requested_total: audio
                .retransmit
                .retransmit_packet_slots_requested_total,
            retransmit_datagrams_accepted_local_total: audio
                .retransmit
                .retransmit_datagrams_accepted_local_total,
            retransmit_history_misses_total: audio.retransmit.retransmit_history_misses_total,
            retransmit_send_failures_total: audio.retransmit.retransmit_send_failures_total,
        },
        target_count: audio.targets.len(),
    }
}

fn support_timing(timing: &ReceiverTimingSnapshot, aliases: &AliasTable) -> SupportTiming {
    let (source, shared_from_receiver_alias) = timing_source_label(&timing.source, aliases);
    SupportTiming {
        source,
        shared_from_receiver_alias,
        independently_measured: timing.independently_measured,
        offset_local_minus_master_ns: timing.offset_local_minus_master_ns,
        mean_path_delay_ns: timing.mean_path_delay_ns,
        estimated_relative_clock_drift_ppm: timing.drift_ppm,
        sample_age_ns: timing.sample_age_ns,
        stale: timing.stale,
    }
}

fn support_transport(transport: &ReceiverTransportSnapshot) -> SupportTransport {
    SupportTransport {
        data_datagrams_attempted_total: transport.data_datagrams_attempted_total,
        data_datagrams_accepted_local_total: transport.data_datagrams_accepted_local_total,
        data_datagram_send_failures_total: transport.data_datagram_send_failures_total,
        data_bytes_accepted_local_total: transport.data_bytes_accepted_local_total,
        sync_datagrams_attempted_total: transport.sync_datagrams_attempted_total,
        sync_datagrams_accepted_local_total: transport.sync_datagrams_accepted_local_total,
        sync_datagram_send_failures_total: transport.sync_datagram_send_failures_total,
        sync_bytes_accepted_local_total: transport.sync_bytes_accepted_local_total,
        retransmit_slots_requested_total: transport.retransmit_slots_requested_total,
        retransmit_slots_accepted_local_total: transport.retransmit_slots_accepted_local_total,
        recovery_demand_numerator_slots: transport.recovery_demand_numerator_slots,
        recovery_demand_denominator_shared_frames: transport
            .recovery_demand_denominator_shared_frames,
        recovery_demand_ratio: transport.recovery_demand_ratio,
        local_os_udp_acceptance: LOCAL_OS_UDP_ACCEPTANCE,
    }
}

fn support_feedback(feedback: &FeedbackSnapshot) -> SupportFeedback {
    SupportFeedback {
        attempts_total: feedback.attempts_total,
        successes_total: feedback.successes_total,
        protocol_failures_total: feedback.protocol_failures_total,
        transport_failures_total: feedback.transport_failures_total,
        timeouts_total: feedback.timeouts_total,
        last_result: feedback.last_result.map(feedback_result_label),
        last_control_request_duration_ns: feedback.last_transaction_duration_ns,
        last_success_elapsed_ns: feedback.last_success_elapsed_ns,
    }
}

fn support_error(error: &DiagnosticError, aliases: &AliasTable) -> SupportError {
    SupportError {
        code: error_code_label(error.code),
        component: component_label(error.component),
        severity: severity_label(error.severity),
        recoverability: recoverability_label(error.recoverability),
        operation: error.operation,
        receiver_alias: error
            .receiver_session
            .as_ref()
            .map(|key| aliases.receiver(&key.receiver_id)),
        free_form_text_omitted: true,
    }
}

fn support_event(event: &DiagnosticEvent, aliases: &AliasTable) -> SupportEvent {
    SupportEvent {
        cursor: event.cursor.0,
        occurred_at_utc: rfc3339_utc(event.occurred_at_utc),
        process_elapsed_ns: event.process_elapsed_ns,
        session_alias: event
            .receiver_session
            .as_ref()
            .map(|key| aliases.session(&key.session_id)),
        receiver_alias: event
            .receiver_session
            .as_ref()
            .map(|key| aliases.receiver(&key.receiver_id)),
        severity: severity_label(event.severity),
        component: component_label(event.component),
        code: error_code_label(event.code),
        payload: support_payload(&event.payload, aliases),
    }
}

/// Exhaustive on purpose: a new structured payload must get an explicit
/// export shape here, or the build stops. A catch-all would let unreviewed
/// content reach a support document.
fn support_payload(
    payload: &StructuredDiagnosticPayload,
    aliases: &AliasTable,
) -> SupportEventPayload {
    match payload {
        StructuredDiagnosticPayload::Backend(backend) => support_backend_payload(backend, aliases),
        StructuredDiagnosticPayload::Client(event) => SupportEventPayload::ClientEvent {
            receiver_alias: event
                .device_id
                .as_ref()
                .and_then(|device_id| aliases.device(device_id)),
            client_kind: client_kind_label(event.kind),
            elapsed_ns: event.elapsed_ns,
        },
        StructuredDiagnosticPayload::Error(error) => SupportEventPayload::Error {
            error: support_error(error, aliases),
        },
        StructuredDiagnosticPayload::DefiniteCounterTransition {
            counter,
            previous_total,
            current_total,
        } => SupportEventPayload::DefiniteCounterTransition {
            counter: definite_counter_label(*counter),
            previous_total: *previous_total,
            current_total: *current_total,
        },
        StructuredDiagnosticPayload::CalibrationState(state) => {
            SupportEventPayload::CalibrationState {
                state: calibration_apply_state_label(*state),
            }
        }
        StructuredDiagnosticPayload::ExportState(state) => SupportEventPayload::ExportState {
            state: export_state_label(*state),
        },
    }
}

/// Exhaustive on purpose, for the same reason as [`support_payload`].
///
/// Free-form arms ([`BackendDiagnosticPayload::Message`],
/// [`BackendDiagnosticPayload::CommandResult`]) drop their text and say so;
/// the sender cannot prove an arbitrary string is free of addresses, paths,
/// or protocol detail, so it never travels.
fn support_backend_payload(
    payload: &BackendDiagnosticPayload,
    aliases: &AliasTable,
) -> SupportEventPayload {
    match payload {
        BackendDiagnosticPayload::Message(_) => SupportEventPayload::BackendMessage {
            free_form_text_omitted: true,
        },
        BackendDiagnosticPayload::DiscoveryGeneration { generation, phase } => {
            let (phase, attempt) = discovery_phase_label(phase);
            SupportEventPayload::DiscoveryGeneration {
                generation: *generation,
                phase,
                attempt,
            }
        }
        BackendDiagnosticPayload::DiscoveryReceiver { receiver, present } => {
            SupportEventPayload::DiscoveryReceiver {
                receiver_alias: aliases.receiver(receiver),
                present: *present,
            }
        }
        BackendDiagnosticPayload::CommandResult { id: _, error } => {
            SupportEventPayload::CommandResult {
                failed: error.is_some(),
                free_form_text_omitted: error.is_some(),
            }
        }
        BackendDiagnosticPayload::CommandCoalesced { dropped } => {
            SupportEventPayload::CommandCoalesced {
                superseded: *dropped,
            }
        }
        BackendDiagnosticPayload::ReceiverTransition { receiver, to } => {
            SupportEventPayload::ReceiverTransition {
                receiver_alias: aliases.receiver(receiver),
                lifecycle: support_lifecycle(to),
            }
        }
        BackendDiagnosticPayload::SetupPhaseDuration {
            receiver,
            phase,
            duration,
        } => SupportEventPayload::SetupPhaseDuration {
            receiver_alias: aliases.receiver(receiver),
            phase: setup_phase_label(*phase),
            duration_ns: duration_ns(*duration),
        },
        BackendDiagnosticPayload::SessionTransition {
            phase,
            active_members,
        } => SupportEventPayload::SessionTransition {
            phase: support_session_phase(phase),
            active_members: *active_members,
        },
        BackendDiagnosticPayload::SessionRestart { generation, reason } => {
            SupportEventPayload::SessionRestart {
                generation: *generation,
                reason: restart_reason_label(*reason),
            }
        }
        BackendDiagnosticPayload::Retry {
            receiver, attempt, ..
        } => SupportEventPayload::Retry {
            receiver_alias: receiver.map(|receiver| aliases.receiver(&receiver)),
            attempt: *attempt,
        },
        BackendDiagnosticPayload::CaptureTransition { state } => {
            SupportEventPayload::CaptureTransition {
                state: audio_source_state_label(*state),
            }
        }
        BackendDiagnosticPayload::PcmDrop { dropped } => SupportEventPayload::CaptureDrop {
            dropped_frames: *dropped,
        },
        BackendDiagnosticPayload::SilenceBridge { duration } => {
            SupportEventPayload::SilenceBridge {
                duration_ns: duration_ns(*duration),
            }
        }
        BackendDiagnosticPayload::Probe {
            healthy,
            latency_ms,
        } => SupportEventPayload::Probe {
            healthy: *healthy,
            control_request_duration_ms: *latency_ms,
        },
        BackendDiagnosticPayload::Persistence { outcome } => SupportEventPayload::Persistence {
            outcome: persistence_label(*outcome),
        },
        BackendDiagnosticPayload::QueueWatermark { depth, capacity } => {
            SupportEventPayload::QueueWatermark {
                depth: *depth,
                capacity: *capacity,
            }
        }
        BackendDiagnosticPayload::QueueLag { dropped } => SupportEventPayload::DiagnosticFeedLag {
            dropped_events: *dropped,
        },
        BackendDiagnosticPayload::WorkerShutdown {
            duration,
            timed_out,
        } => SupportEventPayload::WorkerShutdown {
            duration_ns: duration_ns(*duration),
            timed_out: *timed_out,
        },
        BackendDiagnosticPayload::SystemTransition { transition } => {
            let (transition, local_binding_changed) = system_transition_label(*transition);
            SupportEventPayload::SystemTransition {
                transition,
                local_binding_changed,
            }
        }
        BackendDiagnosticPayload::CalibrationState(state) => {
            SupportEventPayload::CalibrationState {
                state: calibration_apply_state_label(*state),
            }
        }
    }
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Labels
//
// Every function below turns one typed domain value into a compile-time
// literal. None of them can return caller data, and all of them are
// exhaustive so a new variant forces an explicit label instead of silently
// exporting nothing or, worse, a `Debug` rendering that could quote text.
// ---------------------------------------------------------------------------

fn health_label(health: Health) -> &'static str {
    match health {
        Health::Unknown => "unknown",
        Health::Running => "running",
        Health::Attention => "attention",
        Health::Error => "error",
    }
}

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Info => "info",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Error => "error",
    }
}

fn component_label(component: DiagnosticComponent) -> &'static str {
    match component {
        DiagnosticComponent::Discovery => "discovery",
        DiagnosticComponent::Pairing => "pairing",
        DiagnosticComponent::Rtsp => "rtsp",
        DiagnosticComponent::Timing => "timing",
        DiagnosticComponent::Capture => "capture",
        DiagnosticComponent::Buffer => "buffer",
        DiagnosticComponent::Encoder => "encoder",
        DiagnosticComponent::Scheduler => "scheduler",
        DiagnosticComponent::UdpTransport => "udp_transport",
        DiagnosticComponent::Retransmit => "retransmit",
        DiagnosticComponent::Feedback => "feedback",
        DiagnosticComponent::Calibration => "calibration",
        DiagnosticComponent::Export => "export",
        DiagnosticComponent::Backend => "backend",
    }
}

fn error_code_label(code: DiagnosticErrorCode) -> &'static str {
    match code {
        DiagnosticErrorCode::DiscoveryFailure => "discovery_failure",
        DiagnosticErrorCode::PairingFailure => "pairing_failure",
        DiagnosticErrorCode::SetupRejected => "setup_rejected",
        DiagnosticErrorCode::SetupTimeout => "setup_timeout",
        DiagnosticErrorCode::EventChannelFailure => "event_channel_failure",
        DiagnosticErrorCode::PtpBindFallback => "ptp_bind_fallback",
        DiagnosticErrorCode::PtpSampleStale => "ptp_sample_stale",
        DiagnosticErrorCode::CaptureFailure => "capture_failure",
        DiagnosticErrorCode::CaptureQueueFull => "capture_queue_full",
        DiagnosticErrorCode::BufferUnderrun => "buffer_underrun",
        DiagnosticErrorCode::EncodeFailure => "encode_failure",
        DiagnosticErrorCode::SenderQueueDisconnected => "sender_queue_disconnected",
        DiagnosticErrorCode::UdpSendFailure => "udp_send_failure",
        DiagnosticErrorCode::RetransmitHistoryMiss => "retransmit_history_miss",
        DiagnosticErrorCode::FeedbackFailure => "feedback_failure",
        DiagnosticErrorCode::FeedbackTimeout => "feedback_timeout",
        DiagnosticErrorCode::TeardownTimeout => "teardown_timeout",
        DiagnosticErrorCode::CalibrationApplyFailure => "calibration_apply_failure",
        DiagnosticErrorCode::ExportFailure => "export_failure",
        DiagnosticErrorCode::Notice => "notice",
    }
}

fn recoverability_label(recoverability: Recoverability) -> &'static str {
    match recoverability {
        Recoverability::Transient => "transient",
        Recoverability::AutomaticRetry => "automatic_retry",
        Recoverability::RequiresSessionRestart => "requires_session_restart",
        Recoverability::UserAction => "user_action",
        Recoverability::Fatal => "fatal",
    }
}

fn stop_reason_label(reason: SessionStopReason) -> &'static str {
    match reason {
        SessionStopReason::Stopped => "stopped",
        SessionStopReason::Restarted => "restarted",
        SessionStopReason::Failed => "failed",
    }
}

fn definite_counter_label(counter: DefiniteCounterKind) -> &'static str {
    match counter {
        DefiniteCounterKind::CaptureDrops => "capture_drops",
        DefiniteCounterKind::BufferUnderruns => "buffer_underruns",
        DefiniteCounterKind::UdpSendFailures => "udp_send_failures",
        DefiniteCounterKind::RetransmitHistoryMisses => "retransmit_history_misses",
    }
}

fn calibration_apply_state_label(state: CalibrationApplyState) -> &'static str {
    match state {
        CalibrationApplyState::PendingRestart => "pending_restart",
        CalibrationApplyState::Applied => "applied",
        CalibrationApplyState::Failed => "failed",
    }
}

fn export_state_label(state: ExportEventState) -> &'static str {
    match state {
        ExportEventState::Started => "started",
        ExportEventState::Completed => "completed",
        ExportEventState::Failed => "failed",
    }
}

fn client_kind_label(kind: ClientDiagnosticKind) -> &'static str {
    match kind {
        ClientDiagnosticKind::Feedback => "feedback",
        ClientDiagnosticKind::SetupFailure => "setup_failure",
        ClientDiagnosticKind::RuntimeWarning => "runtime_warning",
    }
}

fn feedback_result_label(result: airplay_client::FeedbackResultKind) -> &'static str {
    match result {
        airplay_client::FeedbackResultKind::Success => "success",
        airplay_client::FeedbackResultKind::ProtocolFailure => "protocol_failure",
        airplay_client::FeedbackResultKind::TransportFailure => "transport_failure",
        airplay_client::FeedbackResultKind::Timeout => "timeout",
    }
}

/// Returns the timing-source label plus, for a secondary, the alias of the
/// primary it derives from. Never returns a receiver identity.
fn timing_source_label(
    source: &ReceiverTimingSource,
    aliases: &AliasTable,
) -> (&'static str, Option<ReceiverAlias>) {
    match source {
        ReceiverTimingSource::Unavailable => ("not_measured", None),
        ReceiverTimingSource::SenderReference => ("sender_reference", None),
        ReceiverTimingSource::PtpMeasuredAgainstThisMaster => {
            ("ptp_measured_against_this_master", None)
        }
        ReceiverTimingSource::PtpSharedFromPrimary { source_receiver_id } => (
            "ptp_shared_from_primary",
            source_receiver_id.map(|receiver_id| aliases.receiver(&receiver_id)),
        ),
    }
}

fn role_label(role: crate::backend::model::ReceiverRole) -> &'static str {
    use crate::backend::model::ReceiverRole as R;
    match role {
        R::Single => "single",
        R::Primary => "primary",
        R::Secondary => "secondary",
    }
}

fn setup_phase_label(phase: crate::backend::model::SetupPhase) -> &'static str {
    use crate::backend::model::SetupPhase as P;
    match phase {
        P::Connect => "connect",
        P::Pair => "pair",
        P::PrimaryTiming => "primary_timing",
        P::RtspSetup => "rtsp_setup",
        P::SetPeers => "set_peers",
        P::BuildSender => "build_sender",
        P::StartAudio => "start_audio",
    }
}

/// Exhaustive on purpose: `Failed` carries a free-form `UserFacingError`,
/// which is dropped and disclosed instead of exported.
fn support_lifecycle(lifecycle: &ReceiverLifecycle) -> SupportLifecycle {
    let empty = SupportLifecycle {
        state: "discovered",
        attempt: None,
        role: None,
        setup_phase: None,
        retryable: None,
        free_form_text_omitted: false,
    };
    match lifecycle {
        ReceiverLifecycle::Discovered => empty,
        ReceiverLifecycle::Connecting { attempt } => SupportLifecycle {
            state: "connecting",
            attempt: Some(*attempt),
            ..empty
        },
        ReceiverLifecycle::SettingUp { role, phase } => SupportLifecycle {
            state: "setting_up",
            role: Some(role_label(*role)),
            setup_phase: Some(setup_phase_label(*phase)),
            ..empty
        },
        ReceiverLifecycle::Ready { role } => SupportLifecycle {
            state: "ready",
            role: Some(role_label(*role)),
            ..empty
        },
        ReceiverLifecycle::Streaming { role } => SupportLifecycle {
            state: "streaming",
            role: Some(role_label(*role)),
            ..empty
        },
        ReceiverLifecycle::RetryWaiting { attempt, .. } => SupportLifecycle {
            state: "retry_waiting",
            attempt: Some(*attempt),
            ..empty
        },
        ReceiverLifecycle::Unavailable => SupportLifecycle {
            state: "unavailable",
            ..empty
        },
        ReceiverLifecycle::Failed { retryable, .. } => SupportLifecycle {
            state: "failed",
            retryable: Some(*retryable),
            free_form_text_omitted: true,
            ..empty
        },
    }
}

/// Exhaustive on purpose: `Failed` carries a free-form `UserFacingError`,
/// which is dropped and disclosed instead of exported.
fn support_session_phase(phase: &SessionPhase) -> SupportSessionPhase {
    let empty = SupportSessionPhase {
        phase: "stopped",
        generation: None,
        restart_reason: None,
        free_form_text_omitted: false,
    };
    match phase {
        SessionPhase::Stopped => empty,
        SessionPhase::Starting { generation } => SupportSessionPhase {
            phase: "starting",
            generation: Some(*generation),
            ..empty
        },
        SessionPhase::Streaming { generation } => SupportSessionPhase {
            phase: "streaming",
            generation: Some(*generation),
            ..empty
        },
        SessionPhase::Degraded { generation } => SupportSessionPhase {
            phase: "degraded",
            generation: Some(*generation),
            ..empty
        },
        SessionPhase::Restarting { generation, reason } => SupportSessionPhase {
            phase: "restarting",
            generation: Some(*generation),
            restart_reason: Some(restart_reason_label(*reason)),
            ..empty
        },
        SessionPhase::Stopping { generation } => SupportSessionPhase {
            phase: "stopping",
            generation: Some(*generation),
            ..empty
        },
        SessionPhase::Failed { generation, .. } => SupportSessionPhase {
            phase: "failed",
            generation: Some(*generation),
            free_form_text_omitted: true,
            ..empty
        },
    }
}

fn restart_reason_label(reason: crate::backend::model::RestartReason) -> &'static str {
    use crate::backend::model::RestartReason as R;
    match reason {
        R::MembershipChange => "membership_change",
        R::SavedGroupActivated => "saved_group_activated",
        R::SecondaryRejoin => "secondary_rejoin",
        R::FailedTargetRemoved => "failed_target_removed",
        R::PrimaryReplaced => "primary_replaced",
        R::ReceiverAddressChanged => "receiver_address_changed",
        R::LocalInterfaceChanged => "local_interface_changed",
        R::TimingModeTransition => "timing_mode_transition",
        R::StreamFormatChanged => "stream_format_changed",
        R::LatencyPresetChanged => "latency_preset_changed",
        R::SystemResume => "system_resume",
        R::DeadRtspRecovered => "dead_rtsp_recovered",
        R::CalibrationChanged => "calibration_changed",
    }
}

fn audio_source_state_label(state: crate::backend::model::AudioSourceState) -> &'static str {
    use crate::backend::model::AudioSourceState as S;
    match state {
        S::Capturing => "capturing",
        S::SilentSystem => "silent_system",
        S::Recovering => "recovering",
        S::Unavailable => "unavailable",
        S::Failed => "failed",
    }
}

fn persistence_label(outcome: crate::backend::model::PersistenceSnapshot) -> &'static str {
    use crate::backend::model::PersistenceSnapshot as P;
    match outcome {
        P::Idle => "idle",
        P::Healthy => "healthy",
        P::Error => "error",
    }
}

/// Returns the discovery phase label plus its retry attempt, dropping the
/// wall-clock retry deadline that says nothing support can act on.
fn discovery_phase_label(
    phase: &crate::backend::model::DiscoveryPhase,
) -> (&'static str, Option<u32>) {
    use crate::backend::model::DiscoveryPhase as D;
    match phase {
        D::Stopped => ("stopped", None),
        D::Running => ("running", None),
        D::Retrying { attempt, .. } => ("retrying", Some(*attempt)),
    }
}

fn system_transition_label(
    transition: crate::backend::event::SystemTransition,
) -> (&'static str, Option<bool>) {
    use crate::backend::event::SystemTransition as T;
    match transition {
        T::Suspending => ("suspending", None),
        T::Resumed => ("resumed", None),
        T::NetworkChanged {
            local_binding_changed,
        } => ("network_changed", Some(local_binding_changed)),
    }
}

// ---------------------------------------------------------------------------
// RFC 3339
// ---------------------------------------------------------------------------

/// Renders a wall-clock instant as a zero-padded RFC 3339 UTC stamp.
///
/// Implemented here rather than pulled in as a dependency: the export needs
/// exactly one format, and a fixed nine-digit fraction keeps every document
/// byte-comparable.
fn rfc3339_utc(time: SystemTime) -> String {
    let (seconds, nanos) = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => (
            i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
            after.subsec_nanos(),
        ),
        Err(before) => {
            let before = before.duration();
            let secs = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            if before.subsec_nanos() == 0 {
                (-secs, 0)
            } else {
                (-secs - 1, 1_000_000_000 - before.subsec_nanos())
            }
        }
    };

    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{nanos:09}Z")
}

/// Days-since-epoch to civil date, after Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = u64::try_from(shifted - era * 146_097).unwrap_or(0);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = i64::try_from(year_of_era).unwrap_or(0) + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_position + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    })
    .unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}
