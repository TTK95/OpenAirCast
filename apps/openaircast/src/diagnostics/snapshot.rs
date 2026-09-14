//! Structured application diagnostics domain and immutable snapshots.
//!
//! This module defines the presentation-safe diagnostic value types shared by
//! every later application task ([`DiagnosticError`], [`DiagnosticEvent`],
//! [`ReceiverSessionKey`], [`SessionId`]) plus the value contract consumed by
//! the bounded ring and registry worker.
//!
//! Payload reuse decision: the spec's structured payload mirrors neither the
//! backend feed nor the client feed one-to-one; it wraps them. The backend
//! variant therefore re-exports [`backend::event::DiagnosticPayload`] under
//! the spec name [`BackendDiagnosticPayload`] instead of duplicating its 18
//! variants, and the client variant wraps
//! `airplay_client::ClientDiagnosticEvent` directly.

use std::collections::BTreeMap;
use std::time::SystemTime;

use airplay_client::{ClientDiagnosticsSource, DeviceId, FeedbackSnapshot};
use uuid::Uuid;

use crate::backend::model::{ReceiverId, SessionPhase};

use super::registry::DiagnosticsRegistry;

pub use crate::backend::event::DiagnosticPayload as BackendDiagnosticPayload;
pub use airplay_client::ClientDiagnosticEvent;

/// Exact capacity of the diagnostic event ring.
pub const EVENT_RING_CAPACITY: usize = 4096;

/// Inclusive upper bound for public event reads.
pub const MAX_EVENT_READ_LIMIT: usize = 512;

/// Fresh per-session identity minted once per group start or restart.
///
/// Never reused by a later session so stale receiver-scoped observations can
/// never be attributed to a newer session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl SessionId {
    /// Mints a fresh process-unique session identity.
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Stable pairing of one session with one receiver.
///
/// Every in-session observation is keyed by this pair; sender indices,
/// discovery order, display names, IP addresses, and PTP roles are never used
/// as keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReceiverSessionKey {
    /// Session the observation belongs to.
    pub session_id: SessionId,
    /// Stable receiver identity (MAC-derived).
    pub receiver_id: ReceiverId,
}

/// Severity of a diagnostic event as shown in the control center.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticSeverity {
    /// Informational observation.
    Info,
    /// Degraded but recoverable condition.
    Warning,
    /// Failed operation or terminal condition.
    Error,
}

/// Sender subsystem that produced a diagnostic error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticComponent {
    /// mDNS discovery supervision.
    Discovery,
    /// HomeKit/FairPlay pairing.
    Pairing,
    /// RTSP setup and control channel.
    Rtsp,
    /// PTP/NTP timing exchange.
    Timing,
    /// WASAPI capture path.
    Capture,
    /// Shared sender PCM buffer.
    Buffer,
    /// ALAC encoding.
    Encoder,
    /// RTP scheduling.
    Scheduler,
    /// Per-receiver UDP data/sync transport.
    UdpTransport,
    /// Retransmit request handling.
    Retransmit,
    /// Feedback probes.
    Feedback,
    /// Manual presentation-time calibration.
    Calibration,
    /// Support export.
    Export,
    /// The backend actor/shell domain: commands, lifecycle queues,
    /// persistence, workers, and general notices that belong to no sender
    /// subsystem.
    Backend,
}

/// Whether and how an error can clear without user intervention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Recoverability {
    /// Expected to clear on its own.
    Transient,
    /// Cleared by an automatic retry that is already scheduled.
    AutomaticRetry,
    /// Clears only through a controlled full-group restart.
    RequiresSessionRestart,
    /// Requires explicit user action.
    UserAction,
    /// Terminal for the affected stream.
    Fatal,
}

/// Stable, exhaustive identity of a diagnostic failure class.
///
/// Display and export switch on this code; they never parse free-form tracing
/// text. Variant order is stable: new codes are appended only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticErrorCode {
    /// Discovery could not find or retain a receiver.
    DiscoveryFailure,
    /// Pairing with a receiver failed.
    PairingFailure,
    /// Receiver rejected session setup.
    SetupRejected,
    /// Session setup did not complete in time.
    SetupTimeout,
    /// The backend diagnostic event channel failed.
    EventChannelFailure,
    /// PTP fell back from the preferred bind role.
    PtpBindFallback,
    /// No fresh PTP sample arrived within the staleness bound.
    PtpSampleStale,
    /// Audio capture failed.
    CaptureFailure,
    /// The bounded capture queue overflowed.
    CaptureQueueFull,
    /// The shared sender buffer underran.
    BufferUnderrun,
    /// ALAC encoding failed.
    EncodeFailure,
    /// A receiver's send queue disconnected.
    SenderQueueDisconnected,
    /// A UDP datagram send failed locally.
    UdpSendFailure,
    /// A retransmit requested a sequence slot outside history.
    RetransmitHistoryMiss,
    /// A feedback probe failed.
    FeedbackFailure,
    /// A feedback probe timed out.
    FeedbackTimeout,
    /// Session teardown did not complete in time.
    TeardownTimeout,
    /// Applying a calibration profile failed.
    CalibrationApplyFailure,
    /// Writing a support export failed.
    ExportFailure,
    /// Neutral marker for an ingested event whose specific failure class is
    /// carried by its wrapped structured payload instead. Never claims a
    /// failure that the payload does not state.
    Notice,
}

/// Structured error captured where its context is still known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticError {
    /// Stable failure class.
    pub code: DiagnosticErrorCode,
    /// Sender subsystem that produced the error.
    pub component: DiagnosticComponent,
    /// Presentation severity.
    pub severity: DiagnosticSeverity,
    /// How the error can clear.
    pub recoverability: Recoverability,
    /// Static operation name suitable for stable matching.
    pub operation: &'static str,
    /// Affected receiver/session pair, if receiver-scoped.
    pub receiver_session: Option<ReceiverSessionKey>,
    /// Pre-redacted user-safe message.
    pub public_message: String,
    /// Optional sanitized technical detail, safe for support export.
    pub technical_detail: Option<String>,
}

/// Process-lifetime monotonic event position starting at one.
///
/// Cursors increase for the lifetime of the process and never move backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventCursor(pub u64);

/// Structured payload kinds carried by a [`DiagnosticEvent`].
///
/// Exhaustive per the spec. Backend and client payloads wrap the existing
/// typed feeds instead of duplicating their variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuredDiagnosticPayload {
    /// Backend actor diagnostic payload (re-export of
    /// [`backend::event::DiagnosticPayload`]).
    Backend(BackendDiagnosticPayload),
    /// Transport-client diagnostic event.
    Client(ClientDiagnosticEvent),
    /// Fully structured error record.
    Error(DiagnosticError),
    /// Definite counter crossed from one total to the next.
    DefiniteCounterTransition {
        /// Which definite counter transitioned.
        counter: DefiniteCounterKind,
        /// Total observed before the transition.
        previous_total: u64,
        /// Total observed after the transition.
        current_total: u64,
    },
    /// Manual calibration apply lifecycle edge.
    CalibrationState(CalibrationApplyState),
    /// Support export lifecycle edge.
    ExportState(ExportEventState),
}

/// Kinds of definite counters whose transitions may affect health.
///
/// Informational quantities (PTP offset, drift, path delay, jitter, recovery
/// demand) deliberately have no kind here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefiniteCounterKind {
    /// Capture frames dropped by the newest-data bridge.
    CaptureDrops,
    /// Shared sender buffer underruns.
    BufferUnderruns,
    /// Local UDP datagram send failures.
    UdpSendFailures,
    /// Retransmit requests outside packet history.
    RetransmitHistoryMisses,
}

/// Lifecycle of a calibration profile apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalibrationApplyState {
    /// Profile recorded; waiting for the controlled restart.
    PendingRestart,
    /// Profile active for the current session.
    Applied,
    /// Apply rejected or failed; audio continues unchanged.
    Failed,
}

/// Lifecycle of one support-export run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportEventState {
    /// Export started.
    Started,
    /// Export wrote its document completely.
    Completed,
    /// Export aborted before completing.
    Failed,
}

/// One retained diagnostic event in the bounded ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticEvent {
    /// Ring-assigned monotonic cursor.
    pub cursor: EventCursor,
    /// Wall-clock occurrence time.
    pub occurred_at_utc: SystemTime,
    /// Process-relative monotonic age in nanoseconds at occurrence.
    pub process_elapsed_ns: u64,
    /// Affected receiver/session pair, if receiver-scoped.
    pub receiver_session: Option<ReceiverSessionKey>,
    /// Presentation severity.
    pub severity: DiagnosticSeverity,
    /// Sender subsystem that produced the event.
    pub component: DiagnosticComponent,
    /// Stable failure class for errors.
    pub code: DiagnosticErrorCode,
    /// Pre-redacted user-safe message.
    pub public_message: String,
    /// Typed structured content.
    pub payload: StructuredDiagnosticPayload,
}

/// An event as producers submit it, before the ring assigns its cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticEventDraft {
    /// Wall-clock occurrence time.
    pub occurred_at_utc: SystemTime,
    /// Process-relative monotonic age in nanoseconds at occurrence.
    pub process_elapsed_ns: u64,
    /// Affected receiver/session pair, if receiver-scoped.
    pub receiver_session: Option<ReceiverSessionKey>,
    /// Presentation severity.
    pub severity: DiagnosticSeverity,
    /// Sender subsystem that produced the event.
    pub component: DiagnosticComponent,
    /// Stable failure class for errors.
    pub code: DiagnosticErrorCode,
    /// Pre-redacted user-safe message.
    pub public_message: String,
    /// Typed structured content.
    pub payload: StructuredDiagnosticPayload,
}

/// Explicit notice that events between two cursors were overwritten.
///
/// Gaps are never silent: any read positioned behind the oldest retained
/// event reports exactly what was lost relative to the requested cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventGap {
    /// Cursor the reader asked to resume from.
    pub requested_cursor: EventCursor,
    /// Oldest cursor actually returned.
    pub resumed_at_cursor: EventCursor,
    /// Overwritten count computed as `oldest_available - requested`.
    pub overwritten_events: u64,
}

/// Result of one clamped [`crate::diagnostics::DiagnosticsRing::events_since`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventBatch {
    /// Up to the clamped limit of events strictly after the requested cursor.
    pub events: Vec<DiagnosticEvent>,
    /// Position the reader should pass on the next call.
    pub next_cursor: EventCursor,
    /// Oldest cursor still retained by the ring.
    pub oldest_available_cursor: EventCursor,
    /// Present iff the requested cursor is behind retention.
    pub gap: Option<EventGap>,
}

/// Schema version of [`DiagnosticsSnapshot`].
pub const DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION: u16 = 1;

/// Wall-clock plus process-elapsed time source for the registry.
///
/// Injected so hardware-independent tests can drive snapshots with fake
/// clocks; production uses [`SystemDiagnosticsClock`].
pub trait DiagnosticsClock: Send + Sync {
    /// Current wall-clock time.
    fn now_utc(&self) -> SystemTime;
    /// Monotonic nanoseconds since process start.
    fn elapsed_ns(&self) -> u64;
}

/// Production clock backed by `SystemTime` and a process-lifetime `Instant`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDiagnosticsClock;

impl SystemDiagnosticsClock {
    /// Creates the production clock.
    pub fn new() -> Self {
        Self
    }
}

impl DiagnosticsClock for SystemDiagnosticsClock {
    fn now_utc(&self) -> SystemTime {
        SystemTime::now()
    }

    fn elapsed_ns(&self) -> u64 {
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let epoch = EPOCH.get_or_init(std::time::Instant::now);
        u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Conservative overall health badge derived only from authoritative states
/// and definite events; informational quantities never move it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Health {
    /// No current session or not enough measurement exists.
    Unknown,
    /// The stream is active and no definite problem is present.
    Running,
    /// A recoverable/noticeable problem exists (capture drops, underruns,
    /// UDP send failures, retransmit history misses, feedback failures,
    /// degraded session phase).
    Attention,
    /// An authoritative receiver/session state is failed or terminal.
    Error,
}

/// Why a session finished, as supplied by the session owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionStopReason {
    /// Stopped deliberately by user or shell intent.
    Stopped,
    /// Replaced by a controlled full-group restart.
    Restarted,
    /// Ended because recovery was exhausted.
    Failed,
}

/// Registration of one streaming session with the diagnostics registry.
///
/// Carries stable identity only: receivers are keyed by [`ReceiverId`] and
/// mapped to client connections through their [`DeviceId`]; sender indices,
/// discovery order, addresses, and names never cross this boundary.
#[derive(Clone)]
pub struct SessionRegistration {
    /// Fresh identity of the session being registered.
    pub session_id: SessionId,
    /// Monotonic start stamp from the caller's clock domain.
    pub started_elapsed_ns: u64,
    /// PTP primary receiver, if the group runs shared PTP timing.
    pub primary: Option<ReceiverId>,
    /// Stable member mapping used to join client rows to receiver keys.
    pub members: BTreeMap<ReceiverId, DeviceId>,
    /// The client read source whose snapshot/drain feed this session.
    pub source: ClientDiagnosticsSource,
}

/// Latest generation-scoped session state published by the backend.
///
/// This is carried over a `watch` channel: intermediate edges may coalesce,
/// so every value is a complete current answer. The registry bridge uses the
/// generation to reject stale values and infers replacement of an older
/// active session from a newer `Active` value.
#[derive(Clone)]
#[doc(hidden)]
pub enum SessionDiagnosticsState {
    /// No session is active at or after this generation.
    Inactive {
        /// Newest generation whose inactive state is authoritative.
        generation: u64,
        /// Why an earlier active session ended.
        reason: SessionStopReason,
    },
    /// One fully activated session is the current diagnostics source.
    Active {
        /// Generation that owns this transport and source.
        generation: u64,
        /// Monotonic start stamp from the process diagnostics clock.
        started_elapsed_ns: u64,
        /// PTP primary for a group; single-receiver NTP sessions use `None`.
        primary: Option<ReceiverId>,
        /// Stable receiver-to-device mapping for the active transport.
        members: BTreeMap<ReceiverId, DeviceId>,
        /// Read-only source backed by the live AirPlay client.
        source: ClientDiagnosticsSource,
    },
}

impl Default for SessionDiagnosticsState {
    fn default() -> Self {
        Self::Inactive {
            generation: 0,
            reason: SessionStopReason::Stopped,
        }
    }
}

impl std::fmt::Debug for SessionDiagnosticsState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inactive { generation, reason } => formatter
                .debug_struct("Inactive")
                .field("generation", generation)
                .field("reason", reason)
                .finish(),
            Self::Active {
                generation,
                started_elapsed_ns,
                primary,
                members,
                source: _,
            } => formatter
                .debug_struct("Active")
                .field("generation", generation)
                .field("started_elapsed_ns", started_elapsed_ns)
                .field("has_primary", &primary.is_some())
                .field("member_count", &members.len())
                .finish_non_exhaustive(),
        }
    }
}

/// Generation gate translating backend session state into commands for the
/// one application diagnostics registry.
#[derive(Default)]
#[doc(hidden)]
pub struct DiagnosticsSessionTracker {
    newest_generation: u64,
    newest_is_inactive: bool,
    active: Option<(u64, SessionId)>,
}

impl DiagnosticsSessionTracker {
    /// Generation and registry identity most recently registered by this tracker.
    /// Registration is asynchronous: readers must also match the published
    /// snapshot's active ID before exposing current-session measurements.
    pub fn active_registration(&self) -> Option<(u64, SessionId)> {
        self.active
    }

    /// Reconciles one complete backend state into the existing registry.
    ///
    /// Values older than the newest observed generation are ignored. A newer
    /// active value finishes the prior registration before installing its own,
    /// which remains correct even when an intermediate inactive watch value
    /// was coalesced.
    pub fn reconcile(&mut self, registry: &DiagnosticsRegistry, state: &SessionDiagnosticsState) {
        match state {
            SessionDiagnosticsState::Inactive { generation, reason } => {
                if *generation < self.newest_generation {
                    return;
                }
                self.newest_generation = *generation;
                self.newest_is_inactive = true;
                if let Some((active_generation, session_id)) = self.active {
                    if active_generation <= *generation {
                        registry.finish_session(session_id, *reason);
                        self.active = None;
                    }
                }
            }
            SessionDiagnosticsState::Active {
                generation,
                started_elapsed_ns,
                primary,
                members,
                source,
            } => {
                if *generation < self.newest_generation
                    || (*generation == self.newest_generation && self.newest_is_inactive)
                    || self.active.is_some_and(|(active, _)| active == *generation)
                {
                    return;
                }
                if let Some((_, session_id)) = self.active.take() {
                    registry.finish_session(session_id, SessionStopReason::Restarted);
                }
                self.newest_generation = *generation;
                self.newest_is_inactive = false;
                let session_id = SessionId::generate();
                registry.register_session(SessionRegistration {
                    session_id,
                    started_elapsed_ns: *started_elapsed_ns,
                    primary: *primary,
                    members: members.clone(),
                    source: source.clone(),
                });
                self.active = Some((*generation, session_id));
            }
        }
    }
}

/// Where a receiver's timing truth comes from, in presentation terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiverTimingSource {
    /// No client observation exists yet for this receiver.
    Unavailable,
    /// Single-receiver NTP path: the sender is the time reference and no
    /// remote offset exists to measure.
    SenderReference,
    /// This receiver runs the measured group PTP master flow.
    PtpMeasuredAgainstThisMaster,
    /// Secondary receiver deriving from the named primary's measurement.
    PtpSharedFromPrimary {
        /// Primary receiver id, when registration declared one.
        source_receiver_id: Option<ReceiverId>,
    },
}

/// Per-receiver timing half of the snapshot.
///
/// Measurement values mirror what the wired client source truly provides;
/// unavailable values stay `None` and are never replaced by zeros.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverTimingSnapshot {
    /// Presentation-level timing source label.
    pub source: ReceiverTimingSource,
    /// Whether this row carries its own independent measurement.
    pub independently_measured: bool,
    /// Signed local-minus-master offset, when independently measured.
    pub offset_local_minus_master_ns: Option<i64>,
    /// Mean path-delay estimate, when independently measured.
    pub mean_path_delay_ns: Option<u64>,
    /// Estimated relative clock drift, when the fit requirements are met.
    pub drift_ppm: Option<f64>,
    /// Age of the latest accepted sample.
    pub sample_age_ns: Option<u64>,
    /// Whether timing observations have gone stale.
    pub stale: bool,
}

/// Per-receiver transport counters plus the spelled-out recovery demand.
///
/// The recovery-demand numerator/denominator keep their exact definitions:
/// requested retransmit slots over shared RTP frames prepared while the
/// receiver was a session member. It is never packet loss.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReceiverTransportSnapshot {
    /// Audio datagram send attempts.
    pub data_datagrams_attempted_total: u64,
    /// Audio datagrams accepted by the local OS.
    pub data_datagrams_accepted_local_total: u64,
    /// Audio datagram `send_to` failures.
    pub data_datagram_send_failures_total: u64,
    /// Bytes reported sent by successful audio `send_to` calls.
    pub data_bytes_accepted_local_total: u64,
    /// Sync datagram send attempts.
    pub sync_datagrams_attempted_total: u64,
    /// Sync datagrams accepted by the local OS.
    pub sync_datagrams_accepted_local_total: u64,
    /// Sync datagram `send_to` failures.
    pub sync_datagram_send_failures_total: u64,
    /// Bytes reported sent by successful sync `send_to` calls.
    pub sync_bytes_accepted_local_total: u64,
    /// Retransmit slots requested by this receiver.
    pub retransmit_slots_requested_total: u64,
    /// Requested history packets accepted locally as PT=86 replies.
    pub retransmit_slots_accepted_local_total: u64,
    /// Numerator of the recovery-demand ratio (requested slots).
    pub recovery_demand_numerator_slots: u64,
    /// Denominator of the ratio (shared frames prepared while a member).
    pub recovery_demand_denominator_shared_frames: u64,
    /// The ratio itself; `None` while the denominator is zero.
    pub recovery_demand_ratio: Option<f64>,
}

/// Session-scoped half of the snapshot for the active or retained session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionDiagnosticsSnapshot {
    /// Identity of the session this row describes.
    pub session_id: SessionId,
    /// Caller-supplied monotonic start stamp.
    pub started_elapsed_ns: u64,
    /// Registry clock stamp of the finish command, once finished.
    pub finished_elapsed_ns: Option<u64>,
    /// Supplied stop reason, once finished.
    pub stop_reason: Option<SessionStopReason>,
    /// Last authoritative whole-session phase announced by the backend.
    pub phase: Option<SessionPhase>,
    /// PTP primary receiver, if any.
    pub primary: Option<ReceiverId>,
    /// Stable member identity mapping retained for Speakers joins.
    pub members: BTreeMap<ReceiverId, DeviceId>,
    /// Shared audio half mirrored by the registered client source.
    pub audio: Option<airplay_client::AudioDiagnosticsSnapshot>,
}

/// One stable per-receiver row of the snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverDiagnosticsSnapshot {
    /// Session/receiver pair this row is keyed by.
    pub key: ReceiverSessionKey,
    /// Authoritative subproject 2 lifecycle state.
    pub lifecycle: ReceiverLifecycleState,
    /// Timing half.
    pub timing: ReceiverTimingSnapshot,
    /// Transport half.
    pub transport: ReceiverTransportSnapshot,
    /// Feedback totals mirrored from the client connection.
    pub feedback: FeedbackSnapshot,
    /// Last classified client error for this receiver, if any.
    pub last_error: Option<DiagnosticError>,
}

/// Spec name for the authoritative receiver lifecycle enum.
pub use crate::backend::model::ReceiverLifecycle as ReceiverLifecycleState;

/// Immutable point-in-time application diagnostics view.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticsSnapshot {
    /// Exactly [`DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION`].
    pub schema_version: u16,
    /// Process-lifetime monotonically increasing publication number.
    pub snapshot_sequence: u64,
    /// Wall-clock stamp from the registry clock.
    pub captured_at_utc: SystemTime,
    /// Monotonic stamp from the registry clock.
    pub process_elapsed_ns: u64,
    /// Active session identity, `None` before registration or after finish.
    pub active_session_id: Option<SessionId>,
    /// Conservative badge value derived per the spec health rules. This
    /// field extends the spec struct so the Overview badge has one explicit
    /// truthful source instead of re-deriving rules per page.
    pub health: Health,
    /// Active or most recently completed session details.
    pub session: Option<SessionDiagnosticsSnapshot>,
    /// Rows sorted by stable `ReceiverId`.
    pub receivers: Vec<ReceiverDiagnosticsSnapshot>,
    /// Total diagnostic events lost to producer/feed lag, counted from the
    /// typed gap payloads actually received.
    pub diagnostics_events_dropped_total: u64,
}
