//! Structured application diagnostics domain and the bounded cursor ring.
//!
//! This module defines the presentation-safe diagnostic value types shared by
//! every later application task ([`DiagnosticError`], [`DiagnosticEvent`],
//! [`ReceiverSessionKey`], [`SessionId`]) plus the bounded 4,096-entry event
//! ring with its clamped, gap-explicit read contract
//! ([`DiagnosticsRing::events_since`]).
//!
//! Payload reuse decision: the spec's structured payload mirrors neither the
//! backend feed nor the client feed one-to-one; it wraps them. The backend
//! variant therefore re-exports [`backend::event::DiagnosticPayload`] under
//! the spec name [`BackendDiagnosticPayload`] instead of duplicating its 18
//! variants, and the client variant wraps
//! `airplay_client::ClientDiagnosticEvent` directly.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::SystemTime;

use uuid::Uuid;

use crate::backend::model::ReceiverId;

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

/// Result of one clamped [`DiagnosticsRing::events_since`] read.
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

/// Ring state guarded by a short critical section.
struct RingInner {
    entries: VecDeque<DiagnosticEvent>,
    next_cursor: u64,
    overwritten_total: u64,
}

/// Bounded, non-blocking sink and reader for significant diagnostic events.
///
/// Producers only append into bounded memory behind a mutex; they never wait
/// for a consumer, serialize JSON, perform I/O, or acquire UI locks. When the
/// ring is full the oldest entry is overwritten and overwrite accounting
/// advances so later reads can report truthful [`EventGap`] values.
pub struct DiagnosticsRing {
    inner: Mutex<RingInner>,
}

impl DiagnosticsRing {
    /// Creates an empty ring whose first event receives cursor one.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RingInner {
                entries: VecDeque::with_capacity(EVENT_RING_CAPACITY),
                next_cursor: 1,
                overwritten_total: 0,
            }),
        }
    }

    /// Records one draft, assigning the next monotonic cursor.
    ///
    /// Bounded and non-blocking: when the ring holds
    /// [`EVENT_RING_CAPACITY`] entries the oldest is evicted first. Returns
    /// the cursor assigned to this event.
    pub fn push(&self, draft: DiagnosticEventDraft) -> EventCursor {
        let mut inner = self
            .inner
            .lock()
            .expect("diagnostics ring lock poisoned by a panicking writer");
        let cursor = EventCursor(inner.next_cursor);
        inner.next_cursor = inner.next_cursor.wrapping_add(1);
        let event = DiagnosticEvent {
            cursor,
            occurred_at_utc: draft.occurred_at_utc,
            process_elapsed_ns: draft.process_elapsed_ns,
            receiver_session: draft.receiver_session,
            severity: draft.severity,
            component: draft.component,
            code: draft.code,
            public_message: draft.public_message,
            payload: draft.payload,
        };
        inner.entries.push_back(event);
        while inner.entries.len() > EVENT_RING_CAPACITY {
            if inner.entries.pop_front().is_some() {
                inner.overwritten_total += 1;
            }
        }
        cursor
    }

    /// Reads up to `limit` events strictly after `cursor`.
    ///
    /// `limit` is clamped to `1..=[MAX_EVENT_READ_LIMIT]`. If `cursor` is
    /// behind the oldest retained event the batch starts there and carries an
    /// explicit [`EventGap`] whose `overwritten_events` equals
    /// `oldest_available - requested`. When nothing is available,
    /// `next_cursor` stays pinned at the newest assigned cursor so callers
    /// never skip future events.
    pub fn events_since(&self, cursor: EventCursor, limit: usize) -> EventBatch {
        let limit = limit.clamp(1, MAX_EVENT_READ_LIMIT);
        let inner = self
            .inner
            .lock()
            .expect("diagnostics ring lock poisoned by a panicking writer");

        let oldest_available_cursor = match inner.entries.front() {
            Some(front) => front.cursor,
            None => EventCursor(inner.next_cursor),
        };
        let newest_assigned = EventCursor(inner.next_cursor.wrapping_sub(1));
        let gap =
            (cursor < oldest_available_cursor && inner.overwritten_total > 0).then(|| EventGap {
                requested_cursor: cursor,
                resumed_at_cursor: oldest_available_cursor,
                overwritten_events: oldest_available_cursor.0.saturating_sub(cursor.0),
            });

        let mut events = Vec::new();
        let mut next_cursor = newest_assigned.min(cursor);
        for event in &inner.entries {
            if event.cursor <= cursor {
                continue;
            }
            if events.len() == limit {
                break;
            }
            next_cursor = event.cursor;
            events.push(event.clone());
        }

        EventBatch {
            events,
            next_cursor,
            oldest_available_cursor,
            gap,
        }
    }

    /// Copies every retained event plus the truthful overwrite gap under one
    /// short critical section.
    ///
    /// Used only by the support export, which needs the whole retained window
    /// at once instead of the UI's clamped 512-entry pages. The reported gap
    /// is anchored at cursor zero -- the start of the process -- and carries
    /// the exact number of events the ring overwrote, not the
    /// `oldest - requested` estimate [`Self::events_since`] computes for a
    /// reader that is only partially behind.
    fn export_batch(&self) -> (Vec<DiagnosticEvent>, Option<EventGap>) {
        let inner = self
            .inner
            .lock()
            .expect("diagnostics ring lock poisoned by a panicking writer");
        let events: Vec<DiagnosticEvent> = inner.entries.iter().cloned().collect();
        let oldest_available_cursor = match inner.entries.front() {
            Some(front) => front.cursor,
            None => EventCursor(inner.next_cursor),
        };
        let gap = (inner.overwritten_total > 0).then(|| EventGap {
            requested_cursor: EventCursor(0),
            resumed_at_cursor: oldest_available_cursor,
            overwritten_events: inner.overwritten_total,
        });
        (events, gap)
    }
}

impl Default for DiagnosticsRing {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Task 8: single registry, immutable snapshots, conservative health
//
// Design: exactly one dedicated consumer thread owns the event ring, session
// registrations, definite-counter baselines, and snapshot sequence. It reads
// the backend diagnostic feed through a blocking `DiagnosticEventReceiver`
// (driven by a tiny current-thread Tokio runtime so lag surfaces as typed
// QueueLag payloads) multiplexed with a bounded lifecycle command channel.
// Every consumed change rebuilds one owned `DiagnosticsSnapshot` and swaps it
// into an `ArcSwap`, so readers (`DiagnosticsHandle::snapshot`) are never
// blocked by writers. `events_since` delegates to the same shared ring.
// ===========================================================================

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{sync_channel, Receiver as CommandReceiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use airplay_client::{
    ClientDiagnosticError as ClientRowError, ClientDiagnosticErrorKind, ClientDiagnosticsSource,
    ClientTimingRole, DeviceId, FeedbackSnapshot,
};
use arc_swap::ArcSwap;
use tokio::sync::{broadcast, Notify};

use crate::backend::event::{
    DiagnosticCategory as BackendCategory, DiagnosticEvent as BackendFeedEvent,
    DiagnosticEventReceiver, Severity as BackendSeverity,
};
use crate::backend::model::{ReceiverLifecycle, SessionPhase};

use airplay_client::ClientDiagnosticKind;

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

struct HandleInner {
    current: ArcSwap<DiagnosticsSnapshot>,
    ring: Arc<DiagnosticsRing>,
}

/// Cloneable, synchronous, read-only diagnostics surface for the UI.
#[derive(Clone)]
pub struct DiagnosticsHandle {
    inner: Arc<HandleInner>,
}

impl DiagnosticsHandle {
    /// Loads the latest published immutable snapshot without blocking.
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        Arc::unwrap_or_clone(self.inner.current.load_full())
    }

    /// Reads up to the clamped limit of events after `cursor` from the same
    /// ring the registry ingests into.
    pub fn events_since(&self, cursor: EventCursor, limit: usize) -> EventBatch {
        self.inner.ring.events_since(cursor, limit)
    }
}

enum Command {
    Register(SessionRegistration),
    Finish(SessionId, SessionStopReason),
}

/// Sole application-level diagnostics aggregator.
///
/// Spawns exactly one background consumer that owns all mutable registry
/// state; public methods only enqueue lifecycle commands. Dropping the
/// registry shuts the consumer down after draining pending commands.
pub struct DiagnosticsRegistry {
    commands: SyncSender<Command>,
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Read-only view of the same published snapshot and ring the consumer
    /// owns, so [`Self::support_export_input`] never has to ask the worker.
    shared: Arc<HandleInner>,
}

impl DiagnosticsRegistry {
    /// Starts the registry consumer thread.
    ///
    /// Returns the registry handle for lifecycle commands plus the shared
    /// read handle. The initial empty snapshot is published synchronously so
    /// the returned handle is immediately usable.
    pub fn start(
        backend_events: DiagnosticEventReceiver,
        clock: Arc<dyn DiagnosticsClock>,
    ) -> (Self, DiagnosticsHandle) {
        let shared = Arc::new(HandleInner {
            current: ArcSwap::from_pointee(DiagnosticsSnapshot {
                schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
                snapshot_sequence: 0,
                captured_at_utc: clock.now_utc(),
                process_elapsed_ns: clock.elapsed_ns(),
                active_session_id: None,
                health: Health::Unknown,
                session: None,
                receivers: Vec::new(),
                diagnostics_events_dropped_total: 0,
            }),
            ring: Arc::new(DiagnosticsRing::new()),
        });
        let (commands, command_rx) = sync_channel::<Command>(256);
        let wake = Arc::new(Notify::new());
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut state = WorkerState::new(Arc::clone(&clock), Arc::clone(&shared));
        state.publish();

        let worker_wake = Arc::clone(&wake);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = std::thread::Builder::new()
            .name("diagnostics-registry".to_owned())
            .spawn(move || {
                run_registry_worker(
                    state,
                    backend_events,
                    command_rx,
                    worker_wake,
                    worker_shutdown,
                )
            })
            .expect("spawning the diagnostics registry thread must succeed");

        (
            Self {
                commands,
                wake,
                shutdown,
                worker: Mutex::new(Some(worker)),
                shared: Arc::clone(&shared),
            },
            DiagnosticsHandle { inner: shared },
        )
    }

    /// Copies everything one support export needs in one short read.
    ///
    /// The latest published snapshot comes from the same `ArcSwap` the UI
    /// reads, and the retained events plus their overwrite gap come from one
    /// critical section on the shared ring, so no writer is ever blocked and
    /// no partial page can be stitched together across ticks.
    pub fn support_export_input(&self) -> SupportExportInput {
        let latest_snapshot = Arc::unwrap_or_clone(self.shared.current.load_full());
        let (retained_events, event_gap) = self.shared.ring.export_batch();
        let producer_drops_total = latest_snapshot.diagnostics_events_dropped_total;
        SupportExportInput {
            latest_snapshot,
            retained_events,
            event_gap,
            producer_drops_total,
        }
    }

    /// Registers the active session, replacing any previous registration
    /// (the resilience layer always finishes before registering anew).
    ///
    /// Best-effort: if the registry is shutting down the call is a no-op.
    pub fn register_session(&self, registration: SessionRegistration) {
        self.send(Command::Register(registration));
    }

    /// Marks the session finished and retains its final view for Speakers.
    pub fn finish_session(&self, session_id: SessionId, reason: SessionStopReason) {
        self.send(Command::Finish(session_id, reason));
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.wake.notify_one();
        }
    }

    /// Test seam: builds a bounded backend feed pair without touching the
    /// backend module's own constructors.
    #[doc(hidden)]
    pub fn test_diagnostic_feed(
        capacity: usize,
    ) -> (broadcast::Sender<BackendFeedEvent>, DiagnosticEventReceiver) {
        let (tx, rx) = broadcast::channel(capacity.max(1));
        (tx, DiagnosticEventReceiver::new(rx))
    }
}

impl Drop for DiagnosticsRegistry {
    fn drop(&mut self) {
        self.shutdown.store(true, AtomicOrdering::Relaxed);
        self.wake.notify_one();
        if let Some(worker) = self.worker.lock().expect("registry worker lock").take() {
            let _ = worker.join();
        }
    }
}

fn run_registry_worker(
    mut state: WorkerState,
    mut feed: DiagnosticEventReceiver,
    commands: CommandReceiver<Command>,
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async {
        let mut feed_closed = false;
        loop {
            // Drain every pending lifecycle command first, then publish once.
            let mut applied = false;
            while let Ok(command) = commands.try_recv() {
                state.apply_command(command);
                applied = true;
            }
            if applied {
                state.publish();
            }
            if shutdown.load(AtomicOrdering::Relaxed) {
                break;
            }

            tokio::select! {
                _ = wake.notified() => {}
                received = async {
                    if feed_closed {
                        std::future::pending::<Result<BackendFeedEvent, crate::backend::event::DiagnosticRecvError>>().await
                    } else {
                        feed.recv().await
                    }
                } => match received {
                    Ok(event) => {
                        state.ingest_backend_event(event);
                        state.publish();
                    }
                    Err(crate::backend::event::DiagnosticRecvError::Closed) => {
                        // Keep serving snapshots and commands without a feed.
                        feed_closed = true;
                    }
                },
            }
        }
        // Final drain so shutdown never loses a just-sent command.
        while let Ok(command) = commands.try_recv() {
            state.apply_command(command);
        }
        state.publish();
    });
}

/// Definite-counter last-seen values observed for the active session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DefiniteBaseline {
    capture_queue_full_drops: u64,
    buffer_underruns: u64,
    udp_send_failures: u64,
    retransmit_history_misses: u64,
}

struct ActiveSession {
    registration: SessionRegistration,
}

/// Retained view of the most recently completed session.
struct RetainedSession {
    session: SessionDiagnosticsSnapshot,
    receivers: Vec<ReceiverDiagnosticsSnapshot>,
}

/// All mutable registry state; owned exclusively by the consumer thread.
struct WorkerState {
    clock: Arc<dyn DiagnosticsClock>,
    shared: Arc<HandleInner>,
    sequence: u64,
    dropped_total: u64,
    active: Option<ActiveSession>,
    retained: Option<RetainedSession>,
    lifecycle: BTreeMap<ReceiverId, ReceiverLifecycle>,
    phase: Option<SessionPhase>,
    attention_definite: bool,
    feedback_attention: bool,
    pcm_drop_accumulator: u64,
    definite_baseline: Option<DefiniteBaseline>,
}

impl WorkerState {
    fn new(clock: Arc<dyn DiagnosticsClock>, shared: Arc<HandleInner>) -> Self {
        Self {
            clock,
            shared,
            sequence: 0,
            dropped_total: 0,
            active: None,
            retained: None,
            lifecycle: BTreeMap::new(),
            phase: None,
            attention_definite: false,
            feedback_attention: false,
            pcm_drop_accumulator: 0,
            definite_baseline: None,
        }
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            Command::Register(registration) => {
                self.active = Some(ActiveSession { registration });
                self.retained = None;
                self.lifecycle.clear();
                self.phase = None;
                self.attention_definite = false;
                self.feedback_attention = false;
                self.pcm_drop_accumulator = 0;
                self.definite_baseline = None;
            }
            Command::Finish(session_id, reason) => {
                let matches_active = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.registration.session_id == session_id);
                if !matches_active {
                    return;
                }
                let active = self.active.take().expect("checked above");
                let finished_elapsed_ns = self.clock.elapsed_ns();
                let client = active.registration.source.snapshot(self.clock.elapsed_ns());
                let receivers = self.build_receiver_rows(&active.registration, &client);
                let session = SessionDiagnosticsSnapshot {
                    session_id: active.registration.session_id,
                    started_elapsed_ns: active.registration.started_elapsed_ns,
                    finished_elapsed_ns: Some(finished_elapsed_ns),
                    stop_reason: Some(reason),
                    phase: self.phase.clone(),
                    primary: active.registration.primary,
                    members: active.registration.members.clone(),
                    audio: client.audio.clone(),
                };
                self.retained = Some(RetainedSession { session, receivers });
            }
        }
    }

    fn ingest_backend_event(&mut self, event: BackendFeedEvent) {
        let BackendFeedEvent {
            monotonic_ns,
            wall_time,
            ref receiver,
            ref severity,
            ref category,
            ref payload,
            ..
        } = event;

        if let BackendDiagnosticPayload::QueueLag { dropped } = payload {
            self.dropped_total += dropped;
        }
        if let BackendDiagnosticPayload::PcmDrop { dropped } = payload {
            let previous = self.pcm_drop_accumulator;
            self.pcm_drop_accumulator = previous.saturating_add(*dropped);
            if self.active.is_some() && self.pcm_drop_accumulator > previous {
                self.record_definite_transition(
                    DefiniteCounterKind::CaptureDrops,
                    DiagnosticComponent::Capture,
                    DiagnosticErrorCode::CaptureQueueFull,
                    previous,
                    self.pcm_drop_accumulator,
                );
                self.attention_definite = true;
            }
        }
        if let BackendDiagnosticPayload::ReceiverTransition {
            receiver: changed,
            to,
        } = payload
        {
            if self.is_active_member(changed) {
                self.lifecycle.insert(*changed, to.clone());
            }
        }
        if let BackendDiagnosticPayload::SessionTransition { phase, .. } = payload {
            self.phase = Some(phase.clone());
        }

        let receiver_session = receiver.as_ref().and_then(|id| self.session_key_for(id));
        self.shared.ring.push(DiagnosticEventDraft {
            occurred_at_utc: wall_time,
            process_elapsed_ns: monotonic_ns,
            receiver_session,
            severity: map_backend_severity(*severity),
            component: backend_component_for(*category, receiver.is_some()),
            code: backend_code_for(payload),
            public_message: backend_public_message(payload),
            // Calibration has its own structured variant; wrapping it in
            // `Backend(..)` would hide a first-class lifecycle edge inside a
            // generic envelope.
            payload: match payload {
                BackendDiagnosticPayload::CalibrationState(state) => {
                    StructuredDiagnosticPayload::CalibrationState(*state)
                }
                other => StructuredDiagnosticPayload::Backend(other.clone()),
            },
        });
    }

    /// Drains the registered client source's bounded event feed into the
    /// shared ring, exactly once per event.
    fn drain_client_events(&mut self, source: &ClientDiagnosticsSource) {
        for _ in 0..16 {
            let drained = source.drain_events(512);
            if drained.is_empty() {
                return;
            }
            for event in drained {
                let receiver_session = event
                    .device_id
                    .as_ref()
                    .and_then(|device| self.device_key(device));
                let (severity, component, code, attention, message) = match event.kind {
                    ClientDiagnosticKind::Feedback => (
                        DiagnosticSeverity::Info,
                        DiagnosticComponent::Feedback,
                        DiagnosticErrorCode::Notice,
                        false,
                        "feedback transaction recorded",
                    ),
                    ClientDiagnosticKind::SetupFailure => (
                        DiagnosticSeverity::Warning,
                        DiagnosticComponent::Rtsp,
                        DiagnosticErrorCode::SetupRejected,
                        false,
                        "receiver setup failed",
                    ),
                    ClientDiagnosticKind::RuntimeWarning => (
                        DiagnosticSeverity::Warning,
                        DiagnosticComponent::Retransmit,
                        DiagnosticErrorCode::Notice,
                        true,
                        "recoverable transport problem",
                    ),
                };
                if attention {
                    self.attention_definite = true;
                }
                self.shared.ring.push(DiagnosticEventDraft {
                    occurred_at_utc: self.clock.now_utc(),
                    process_elapsed_ns: event.elapsed_ns,
                    receiver_session,
                    severity,
                    component,
                    code,
                    public_message: message.to_owned(),
                    payload: StructuredDiagnosticPayload::Client(event),
                });
            }
        }
    }

    /// Observes the client's shared audio counters against the last-seen
    /// baseline, recording one transition event per increased counter.
    fn observe_definite_counters(&mut self, client: &airplay_client::ClientDiagnosticsSnapshot) {
        let Some(audio) = client.audio.as_ref() else {
            return;
        };
        let current = DefiniteBaseline {
            capture_queue_full_drops: audio.capture_queue.full_queue_drops_total,
            buffer_underruns: audio.sender_buffer.underrun_events_total,
            udp_send_failures: audio
                .targets
                .iter()
                .map(|target| target.data_send_failures_total)
                .fold(0_u64, u64::saturating_add),
            retransmit_history_misses: audio.retransmit.retransmit_history_misses_total,
        };
        let Some(baseline) = self.definite_baseline else {
            // First observation seeds the baseline silently so pre-session
            // totals are never fabricated into transitions.
            self.definite_baseline = Some(current);
            return;
        };

        let increases = [
            (
                baseline.capture_queue_full_drops,
                current.capture_queue_full_drops,
                DefiniteCounterKind::CaptureDrops,
                DiagnosticComponent::Capture,
                DiagnosticErrorCode::CaptureQueueFull,
            ),
            (
                baseline.buffer_underruns,
                current.buffer_underruns,
                DefiniteCounterKind::BufferUnderruns,
                DiagnosticComponent::Buffer,
                DiagnosticErrorCode::BufferUnderrun,
            ),
            (
                baseline.udp_send_failures,
                current.udp_send_failures,
                DefiniteCounterKind::UdpSendFailures,
                DiagnosticComponent::UdpTransport,
                DiagnosticErrorCode::UdpSendFailure,
            ),
            (
                baseline.retransmit_history_misses,
                current.retransmit_history_misses,
                DefiniteCounterKind::RetransmitHistoryMisses,
                DiagnosticComponent::Retransmit,
                DiagnosticErrorCode::RetransmitHistoryMiss,
            ),
        ];
        for (previous, now, kind, component, code) in increases {
            if now > previous && self.active.is_some() {
                self.record_definite_transition(kind, component, code, previous, now);
                self.attention_definite = true;
            }
        }
        self.definite_baseline = Some(current);
    }

    fn record_definite_transition(
        &mut self,
        counter: DefiniteCounterKind,
        component: DiagnosticComponent,
        code: DiagnosticErrorCode,
        previous_total: u64,
        current_total: u64,
    ) {
        self.shared.ring.push(DiagnosticEventDraft {
            occurred_at_utc: self.clock.now_utc(),
            process_elapsed_ns: self.clock.elapsed_ns(),
            receiver_session: None,
            severity: DiagnosticSeverity::Warning,
            component,
            code,
            public_message: format!("{counter:?} total advanced"),
            payload: StructuredDiagnosticPayload::DefiniteCounterTransition {
                counter,
                previous_total,
                current_total,
            },
        });
    }

    fn build_receiver_rows(
        &self,
        registration: &SessionRegistration,
        client: &airplay_client::ClientDiagnosticsSnapshot,
    ) -> Vec<ReceiverDiagnosticsSnapshot> {
        let shared_frames_prepared = client
            .audio
            .as_ref()
            .map(|audio| audio.rtp_frames_prepared_total)
            .unwrap_or(0);

        registration
            .members
            .iter()
            .map(|(receiver_id, device_id)| {
                let key = ReceiverSessionKey {
                    session_id: registration.session_id,
                    receiver_id: *receiver_id,
                };
                let conn = client
                    .connections
                    .iter()
                    .find(|conn| &conn.device_id == device_id);

                let timing = match conn.map(|conn| conn.timing_role) {
                    Some(ClientTimingRole::PtpPrimary) => {
                        let values = conn.expect("role implies a connection").timing.clone();
                        ReceiverTimingSnapshot {
                            source: ReceiverTimingSource::PtpMeasuredAgainstThisMaster,
                            independently_measured: true,
                            offset_local_minus_master_ns: values
                                .latest
                                .as_ref()
                                .map(|m| m.offset_local_minus_master_ns()),
                            mean_path_delay_ns: values
                                .latest
                                .as_ref()
                                .map(|m| m.mean_path_delay_ns()),
                            drift_ppm: values.drift.as_ref().map(|d| d.drift_ppm),
                            sample_age_ns: values.sample_age_ns,
                            stale: values.stale,
                        }
                    }
                    Some(ClientTimingRole::PtpSecondary) => ReceiverTimingSnapshot {
                        source: ReceiverTimingSource::PtpSharedFromPrimary {
                            source_receiver_id: registration.primary,
                        },
                        independently_measured: false,
                        offset_local_minus_master_ns: None,
                        mean_path_delay_ns: None,
                        drift_ppm: None,
                        sample_age_ns: None,
                        stale: false,
                    },
                    role => ReceiverTimingSnapshot {
                        source: if conn.is_some() && matches!(role, Some(ClientTimingRole::Single))
                        {
                            ReceiverTimingSource::SenderReference
                        } else {
                            ReceiverTimingSource::Unavailable
                        },
                        independently_measured: false,
                        offset_local_minus_master_ns: None,
                        mean_path_delay_ns: None,
                        drift_ppm: None,
                        sample_age_ns: None,
                        stale: false,
                    },
                };

                let transport_row = conn.map(|conn| conn.transport).unwrap_or_default();
                let retransmit = conn.map(|conn| conn.retransmit).unwrap_or_default();
                let requested_slots = retransmit.retransmit_packet_slots_requested_total;
                let transport = ReceiverTransportSnapshot {
                    data_datagrams_attempted_total: transport_row.data_datagrams_attempted_total,
                    data_datagrams_accepted_local_total: transport_row
                        .data_datagrams_accepted_local_total,
                    data_datagram_send_failures_total: transport_row.data_send_failures_total,
                    data_bytes_accepted_local_total: transport_row.data_bytes_sent_total,
                    sync_datagrams_attempted_total: transport_row.sync_datagrams_attempted_total,
                    sync_datagrams_accepted_local_total: transport_row
                        .sync_datagrams_accepted_local_total,
                    sync_datagram_send_failures_total: transport_row.sync_send_failures_total,
                    sync_bytes_accepted_local_total: transport_row.sync_bytes_sent_total,
                    retransmit_slots_requested_total: requested_slots,
                    retransmit_slots_accepted_local_total: retransmit
                        .retransmit_datagrams_accepted_local_total,
                    recovery_demand_numerator_slots: requested_slots,
                    recovery_demand_denominator_shared_frames: shared_frames_prepared,
                    recovery_demand_ratio: (shared_frames_prepared > 0)
                        .then(|| requested_slots as f64 / shared_frames_prepared as f64),
                };

                let last_error = conn.and_then(|conn| {
                    conn.last_error
                        .as_ref()
                        .map(|error| client_error_to_diagnostic(error, key.clone()))
                });

                ReceiverDiagnosticsSnapshot {
                    key,
                    lifecycle: self
                        .lifecycle
                        .get(receiver_id)
                        .cloned()
                        .unwrap_or(ReceiverLifecycle::Discovered),
                    timing,
                    transport,
                    feedback: conn.map(|conn| conn.feedback.clone()).unwrap_or_default(),
                    last_error,
                }
            })
            .collect()
    }

    fn compute_health(&self) -> Health {
        if self.active.is_none() {
            // Spec health naming: no current session maps to Unknown, even
            // though a stopped session stays retained for Speakers.
            return Health::Unknown;
        }
        let authoritative_failure = self
            .lifecycle
            .values()
            .any(|state| matches!(state, ReceiverLifecycle::Failed { .. }))
            || matches!(self.phase, Some(SessionPhase::Failed { .. }));
        if authoritative_failure {
            return Health::Error;
        }
        let degraded = matches!(self.phase, Some(SessionPhase::Degraded { .. }))
            || self.attention_definite
            || self.feedback_attention;
        if degraded {
            return Health::Attention;
        }
        Health::Running
    }

    fn publish(&mut self) {
        let captured_at_utc = self.clock.now_utc();
        let process_elapsed_ns = self.clock.elapsed_ns();

        let client_source = self
            .active
            .as_ref()
            .map(|active| active.registration.source.clone());
        let mut receivers = Vec::new();
        let mut session = None;

        if let Some(source) = &client_source {
            self.drain_client_events(source);
            let client = source.snapshot(process_elapsed_ns);
            self.observe_definite_counters(&client);
            let feedback_failed_now = client.connections.iter().any(|conn| {
                matches!(
                    conn.feedback.last_result,
                    Some(
                        airplay_client::FeedbackResultKind::ProtocolFailure
                            | airplay_client::FeedbackResultKind::TransportFailure
                            | airplay_client::FeedbackResultKind::Timeout
                    )
                )
            });
            if feedback_failed_now {
                self.feedback_attention = true;
            }
            if let Some(active) = self.active.as_ref() {
                receivers = self.build_receiver_rows(&active.registration, &client);
                session = Some(SessionDiagnosticsSnapshot {
                    session_id: active.registration.session_id,
                    started_elapsed_ns: active.registration.started_elapsed_ns,
                    finished_elapsed_ns: None,
                    stop_reason: None,
                    phase: self.phase.clone(),
                    primary: active.registration.primary,
                    members: active.registration.members.clone(),
                    audio: client.audio.clone(),
                });
            }
        } else if let Some(retained) = &self.retained {
            receivers = retained.receivers.clone();
            session = Some(retained.session.clone());
        }

        let health = self.compute_health();
        self.sequence += 1;
        self.shared.current.store(Arc::new(DiagnosticsSnapshot {
            schema_version: DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION,
            snapshot_sequence: self.sequence,
            captured_at_utc,
            process_elapsed_ns,
            active_session_id: self
                .active
                .as_ref()
                .map(|active| active.registration.session_id),
            health,
            session,
            receivers,
            diagnostics_events_dropped_total: self.dropped_total,
        }));
    }

    fn is_active_member(&self, receiver_id: &ReceiverId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.registration.members.contains_key(receiver_id))
    }

    fn session_key_for(&self, receiver_id: &ReceiverId) -> Option<ReceiverSessionKey> {
        self.active.as_ref().and_then(|active| {
            if active.registration.members.contains_key(receiver_id) {
                Some(ReceiverSessionKey {
                    session_id: active.registration.session_id,
                    receiver_id: *receiver_id,
                })
            } else {
                None
            }
        })
    }

    fn device_key(&self, device_id: &DeviceId) -> Option<ReceiverSessionKey> {
        self.active.as_ref().and_then(|active| {
            active
                .registration
                .members
                .iter()
                .find(|(_, member_device)| *member_device == device_id)
                .map(|(receiver_id, _)| ReceiverSessionKey {
                    session_id: active.registration.session_id,
                    receiver_id: *receiver_id,
                })
        })
    }
}

fn map_backend_severity(severity: BackendSeverity) -> DiagnosticSeverity {
    match severity {
        BackendSeverity::Info => DiagnosticSeverity::Info,
        BackendSeverity::Warning => DiagnosticSeverity::Warning,
        BackendSeverity::Error => DiagnosticSeverity::Error,
    }
}

/// Coarse component routing for wrapped backend events. The wrapped payload
/// stays authoritative; this only labels the Events page filter column.
fn backend_component_for(category: BackendCategory, receiver_scoped: bool) -> DiagnosticComponent {
    match category {
        BackendCategory::Discovery => DiagnosticComponent::Discovery,
        BackendCategory::Capture => DiagnosticComponent::Capture,
        BackendCategory::Probe => DiagnosticComponent::Feedback,
        BackendCategory::Retry => {
            if receiver_scoped {
                DiagnosticComponent::Rtsp
            } else {
                DiagnosticComponent::Discovery
            }
        }
        BackendCategory::Receiver | BackendCategory::Session => DiagnosticComponent::Rtsp,
        BackendCategory::Command
        | BackendCategory::General
        | BackendCategory::Persistence
        | BackendCategory::Queue
        | BackendCategory::Worker => DiagnosticComponent::Backend,
    }
}

/// Coarse failure-class routing for wrapped backend events. Only payloads
/// whose class is unambiguous get a specific code; everything else carries
/// [`DiagnosticErrorCode::Notice`] so no failure is claimed that the payload
/// does not state.
fn backend_code_for(payload: &BackendDiagnosticPayload) -> DiagnosticErrorCode {
    match payload {
        BackendDiagnosticPayload::PcmDrop { .. } => DiagnosticErrorCode::CaptureQueueFull,
        BackendDiagnosticPayload::QueueLag { .. } => DiagnosticErrorCode::EventChannelFailure,
        BackendDiagnosticPayload::Probe { healthy: false, .. } => {
            DiagnosticErrorCode::FeedbackFailure
        }
        _ => DiagnosticErrorCode::Notice,
    }
}

/// User-safe message derived from the typed backend payload.
///
/// Exhaustive on purpose: a catch-all would let a new payload variant reach
/// the export as "backend diagnostic", which says nothing and is therefore
/// worse than no record. Every arm restates only what its own payload holds,
/// so none of them can name an address, a path, or a credential.
fn backend_public_message(payload: &BackendDiagnosticPayload) -> String {
    match payload {
        BackendDiagnosticPayload::Message(text) => text.clone(),
        BackendDiagnosticPayload::DiscoveryGeneration { generation, phase } => {
            format!("discovery generation {generation} is {phase:?}")
        }
        BackendDiagnosticPayload::DiscoveryReceiver { present, .. } => {
            if *present {
                "a receiver appeared in discovery".to_owned()
            } else {
                "a receiver disappeared from discovery".to_owned()
            }
        }
        BackendDiagnosticPayload::CommandResult { id, error } => match error {
            Some(error) => format!("command {id} failed: {error}"),
            None => format!("command {id} completed"),
        },
        BackendDiagnosticPayload::CommandCoalesced { dropped } => {
            format!("{dropped} superseded commands were merged away")
        }
        BackendDiagnosticPayload::ReceiverTransition { to, .. } => {
            format!("receiver entered {to:?}")
        }
        BackendDiagnosticPayload::SetupPhaseDuration {
            phase, duration, ..
        } => format!("setup phase {phase:?} took {} ms", duration.as_millis()),
        BackendDiagnosticPayload::SessionTransition {
            phase,
            active_members,
        } => format!("session entered {phase:?} with {active_members} active receivers"),
        BackendDiagnosticPayload::SessionRestart { generation, reason } => {
            format!("session generation {generation} restarts: {reason:?}")
        }
        BackendDiagnosticPayload::Retry {
            receiver, attempt, ..
        } => match receiver {
            Some(_) => format!("reconnect attempt {attempt} scheduled for a receiver"),
            None => format!("retry attempt {attempt} scheduled"),
        },
        BackendDiagnosticPayload::CaptureTransition { state } => {
            format!("audio capture is {state:?}")
        }
        BackendDiagnosticPayload::PcmDrop { dropped } => {
            format!("capture bridge dropped {dropped} frames")
        }
        BackendDiagnosticPayload::QueueLag { dropped } => {
            format!("diagnostic feed lagged; {dropped} events lost")
        }
        BackendDiagnosticPayload::SilenceBridge { duration } => {
            format!("bridge paced silence for {} ms", duration.as_millis())
        }
        BackendDiagnosticPayload::QueueWatermark { depth, capacity } => {
            format!("queue reached {depth}/{capacity} entries")
        }
        BackendDiagnosticPayload::Probe {
            healthy,
            latency_ms,
        } => {
            if *healthy {
                format!("probe answered in {latency_ms} ms")
            } else {
                "probe failed".to_owned()
            }
        }
        BackendDiagnosticPayload::Persistence { outcome } => {
            format!("saving device state is {outcome:?}")
        }
        BackendDiagnosticPayload::WorkerShutdown {
            duration,
            timed_out,
        } => {
            if *timed_out {
                format!(
                    "shutdown ran out of its budget after {} ms",
                    duration.as_millis()
                )
            } else {
                format!("shutdown completed in {} ms", duration.as_millis())
            }
        }
        BackendDiagnosticPayload::SystemTransition { transition } => {
            format!("platform edge applied: {transition:?}")
        }
        BackendDiagnosticPayload::CalibrationState(state) => {
            format!("speaker calibration is {state:?}")
        }
    }
}

/// Maps a classified client error onto the structured application error.
///
/// The classification today comes from feedback transactions and connection
/// failures, so the coarse codes reflect those producers; the original
/// message text is preserved verbatim.
fn client_error_to_diagnostic(error: &ClientRowError, key: ReceiverSessionKey) -> DiagnosticError {
    let (code, component) = match error.kind {
        ClientDiagnosticErrorKind::Timeout => (
            DiagnosticErrorCode::FeedbackTimeout,
            DiagnosticComponent::Feedback,
        ),
        ClientDiagnosticErrorKind::ProtocolFailure => (
            DiagnosticErrorCode::FeedbackFailure,
            DiagnosticComponent::Feedback,
        ),
        ClientDiagnosticErrorKind::TransportFailure => (
            DiagnosticErrorCode::UdpSendFailure,
            DiagnosticComponent::UdpTransport,
        ),
        ClientDiagnosticErrorKind::Other => {
            (DiagnosticErrorCode::Notice, DiagnosticComponent::Backend)
        }
    };
    DiagnosticError {
        code,
        component,
        severity: DiagnosticSeverity::Error,
        recoverability: Recoverability::AutomaticRetry,
        operation: "client_connection",
        receiver_session: Some(key),
        public_message: error.public_message.clone(),
        technical_detail: None,
    }
}

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

use std::collections::BTreeSet;

use serde::Serialize;

use crate::calibration::EffectiveCalibration;

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
// `diagnostics.rs` -- including the converters that perform the redaction --
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

#[cfg(test)]
mod session_lifecycle_tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use airplay_client::ClientDiagnosticsSource;
    use airplay_core::DeviceId;

    use super::*;

    #[derive(Default)]
    struct FixedClock;

    impl DiagnosticsClock for FixedClock {
        fn now_utc(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }

        fn elapsed_ns(&self) -> u64 {
            99
        }
    }

    fn active(generation: u64, members: &[u8]) -> SessionDiagnosticsState {
        let members: BTreeMap<_, _> = members
            .iter()
            .map(|seed| {
                let device = DeviceId([0, 0, 0, 0, 0, *seed]);
                (ReceiverId::from(device.clone()), device)
            })
            .collect();
        let source = ClientDiagnosticsSource::test_new_empty();
        for device in members.values() {
            source.test_register_connection(device.clone(), None);
        }
        SessionDiagnosticsState::Active {
            generation,
            started_elapsed_ns: generation * 10,
            primary: (members.len() > 1).then(|| *members.keys().next().expect("member")),
            members,
            source,
        }
    }

    #[test]
    fn session_binding_debug_excludes_hardware_identifiers() {
        let rendered = format!("{:?}", active(1, &[1, 2]));
        assert!(!rendered.contains("ReceiverId"), "{rendered}");
        assert!(!rendered.contains("[0, 0,"), "{rendered}");
        assert!(rendered.contains("member_count: 2"));
    }

    fn wait_for(
        handle: &DiagnosticsHandle,
        predicate: impl Fn(&DiagnosticsSnapshot) -> bool,
    ) -> DiagnosticsSnapshot {
        for _ in 0..2_000 {
            let snapshot = handle.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("diagnostics registry did not publish the expected lifecycle state")
    }

    #[test]
    fn single_session_is_registered_and_finished_in_the_registry() {
        let (_feed_tx, feed) = DiagnosticsRegistry::test_diagnostic_feed(4);
        let (registry, handle) = DiagnosticsRegistry::start(feed, Arc::new(FixedClock));
        let mut tracker = DiagnosticsSessionTracker::default();

        tracker.reconcile(&registry, &active(1, &[1]));
        let running = wait_for(&handle, |snapshot| snapshot.active_session_id.is_some());
        assert_eq!(running.receivers.len(), 1);
        let session_id = running.active_session_id.expect("single session active");

        tracker.reconcile(
            &registry,
            &SessionDiagnosticsState::Inactive {
                generation: 1,
                reason: SessionStopReason::Stopped,
            },
        );
        let stopped = wait_for(&handle, |snapshot| snapshot.active_session_id.is_none());
        assert_eq!(
            stopped.session.expect("retained session").session_id,
            session_id
        );
    }

    #[test]
    fn newer_group_replaces_single_and_stale_finish_cannot_remove_it() {
        let (_feed_tx, feed) = DiagnosticsRegistry::test_diagnostic_feed(4);
        let (registry, handle) = DiagnosticsRegistry::start(feed, Arc::new(FixedClock));
        let mut tracker = DiagnosticsSessionTracker::default();

        tracker.reconcile(&registry, &active(3, &[1]));
        let first = wait_for(&handle, |snapshot| snapshot.active_session_id.is_some())
            .active_session_id
            .expect("first session active");

        tracker.reconcile(&registry, &active(4, &[1, 2]));
        let group = wait_for(&handle, |snapshot| {
            snapshot.active_session_id.is_some_and(|id| id != first)
                && snapshot.receivers.len() == 2
        });
        let group_id = group.active_session_id.expect("group active");

        tracker.reconcile(
            &registry,
            &SessionDiagnosticsState::Inactive {
                generation: 3,
                reason: SessionStopReason::Stopped,
            },
        );
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(handle.snapshot().active_session_id, Some(group_id));

        tracker.reconcile(
            &registry,
            &SessionDiagnosticsState::Inactive {
                generation: 4,
                reason: SessionStopReason::Stopped,
            },
        );
        let _ = wait_for(&handle, |snapshot| snapshot.active_session_id.is_none());
        tracker.reconcile(&registry, &active(4, &[1, 2]));
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            handle.snapshot().active_session_id.is_none(),
            "an inactive generation is a tombstone and cannot reactivate"
        );
    }
}
