//! Client-level diagnostics: stable receiver mapping, feedback outcomes, and
//! a bounded significant-event feed.
//!
//! # Truthfulness contract
//!
//! * Every number in a snapshot comes from real counters maintained at the
//!   exact mutation sites (relaxed atomics or tiny mutexes). Values that are
//!   unavailable are `None`/`false`/zeroed defaults — never fabricated.
//! * Producers never block consumers: feedback recording is a handful of
//!   relaxed atomic stores; event recording appends to a bounded ring under a
//!   tiny uncontended mutex and drops the oldest entry instead of waiting.
//! * Connection rows are keyed by [`airplay_core::DeviceId`] and sorted by
//!   MAC bytes so receiver mapping is independent of discovery/registration
//!   order. Sender indices stay inside the client and are reported as
//!   `sender_target_index` for joining per-target transport counters.
//!
//! # Known asymmetries (documented, not hidden)
//!
//! * **Retransmit half:** the streamer's retransmit counters are cumulative
//!   across all targets, so they are reported once in
//!   [`ClientDiagnosticsSnapshot::audio`] (`retransmit` field) and left
//!   zeroed in per-receiver rows. Per-target UDP transport counters ARE
//!   joined into rows via `targets[target_index]`.
//! * **Timing measurements:** the PTP/NTP exchange loops live as free
//!   functions inside `airplay-timing` and own their measurement state
//!   internally; attaching a `TimingDiagnosticsRecorder` would require
//!   cross-crate API changes outside this task's scope. Timing snapshots
//!   therefore carry the truthfully derivable reference kind with all
//!   measurement values unavailable (`None`/`false`) rather than invented
//!   numbers. The NTP single-receiver case legitimately reports exactly this
//!   shape (sender-reference clock: no remote offset exists to measure).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use airplay_audio::diagnostics::{
    AudioDiagnosticsSnapshot, AudioDiagnosticsSource, RetransmitSnapshot, TargetTransportSnapshot,
};
use airplay_core::error::Error as CoreError;
use airplay_core::DeviceId;
use airplay_timing::diagnostics::{TimingDiagnosticsSnapshot, TimingReferenceKind};

use crate::PlaybackState;

/// Re-export under the plan's name so consumers do not depend on
/// `airplay-rtsp` directly.
pub use airplay_rtsp::SessionState as RtspSessionState;

/// Re-export so producers and the application registry share one event type.
pub use crate::events::ClientDiagnosticEvent;

/// Process-lifetime monotone base for `elapsed_ns` stamps.
fn process_elapsed_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read_lock<T>(rwlock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rwlock
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ===========================================================================
// Feedback outcomes
// ===========================================================================

/// Terminal classification of one RTSP feedback/keepalive transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackResultKind {
    /// Response received with a non-error status.
    Success,
    /// The receiver (or protocol layer) rejected the request.
    ProtocolFailure,
    /// Local socket/I/O failure prevented completing the transaction.
    TransportFailure,
    /// No terminal response within the keepalive timeout window.
    Timeout,
}

/// Point-in-time totals of feedback transactions for one connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedbackSnapshot {
    pub attempts_total: u64,
    pub successes_total: u64,
    pub protocol_failures_total: u64,
    pub transport_failures_total: u64,
    pub timeouts_total: u64,
    /// Classification of the most recent terminal result.
    pub last_result: Option<FeedbackResultKind>,
    /// Wall duration of the most recent attempted transaction.
    pub last_transaction_duration_ns: Option<u64>,
    /// Reader-clock elapsed stamp of the most recent success.
    pub last_success_elapsed_ns: Option<u64>,
}

const LAST_RESULT_SUCCESS: u8 = 1;

fn encode_result(kind: FeedbackResultKind) -> u8 {
    match kind {
        FeedbackResultKind::Success => LAST_RESULT_SUCCESS,
        FeedbackResultKind::ProtocolFailure => 2,
        FeedbackResultKind::TransportFailure => 3,
        FeedbackResultKind::Timeout => 4,
    }
}

fn decode_result(code: u8) -> Option<FeedbackResultKind> {
    match code {
        1 => Some(FeedbackResultKind::Success),
        2 => Some(FeedbackResultKind::ProtocolFailure),
        3 => Some(FeedbackResultKind::TransportFailure),
        4 => Some(FeedbackResultKind::Timeout),
        _ => None,
    }
}

/// Writer half of the feedback statistics; cheap to clone, lock-free.
///
/// Every [`Self::record`] is a fixed number of relaxed atomic stores and may
/// be called from any thread (including the hot keepalive path).
#[derive(Default)]
pub struct FeedbackRecorder {
    attempts_total: AtomicU64,
    successes_total: AtomicU64,
    protocol_failures_total: AtomicU64,
    transport_failures_total: AtomicU64,
    timeouts_total: AtomicU64,
    last_result: AtomicU8,
    last_transaction_duration_ns: AtomicU64,
    has_last_transaction_duration: AtomicBool,
    last_success_elapsed_ns: AtomicU64,
    has_last_success_elapsed: AtomicBool,
}

impl Clone for FeedbackRecorder {
    fn clone(&self) -> Self {
        Self {
            attempts_total: AtomicU64::new(self.attempts_total.load(Ordering::Relaxed)),
            successes_total: AtomicU64::new(self.successes_total.load(Ordering::Relaxed)),
            protocol_failures_total: AtomicU64::new(
                self.protocol_failures_total.load(Ordering::Relaxed),
            ),
            transport_failures_total: AtomicU64::new(
                self.transport_failures_total.load(Ordering::Relaxed),
            ),
            timeouts_total: AtomicU64::new(self.timeouts_total.load(Ordering::Relaxed)),
            last_result: AtomicU8::new(self.last_result.load(Ordering::Relaxed)),
            last_transaction_duration_ns: AtomicU64::new(
                self.last_transaction_duration_ns.load(Ordering::Relaxed),
            ),
            has_last_transaction_duration: AtomicBool::new(
                self.has_last_transaction_duration.load(Ordering::Relaxed),
            ),
            last_success_elapsed_ns: AtomicU64::new(
                self.last_success_elapsed_ns.load(Ordering::Relaxed),
            ),
            has_last_success_elapsed: AtomicBool::new(
                self.has_last_success_elapsed.load(Ordering::Relaxed),
            ),
        }
    }
}

impl std::fmt::Debug for FeedbackRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackRecorder")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl FeedbackRecorder {
    fn new() -> Self {
        Self::default()
    }

    /// Record one terminal feedback result. Always records the transaction
    /// duration; success additionally stamps `now_elapsed_ns`.
    pub fn record(&self, kind: FeedbackResultKind, duration_ns: u64, now_elapsed_ns: u64) {
        self.attempts_total.fetch_add(1, Ordering::Relaxed);
        match kind {
            FeedbackResultKind::Success => {
                self.successes_total.fetch_add(1, Ordering::Relaxed);
                self.last_success_elapsed_ns
                    .store(now_elapsed_ns, Ordering::Relaxed);
                self.has_last_success_elapsed.store(true, Ordering::Relaxed);
            }
            FeedbackResultKind::ProtocolFailure => {
                self.protocol_failures_total.fetch_add(1, Ordering::Relaxed);
            }
            FeedbackResultKind::TransportFailure => {
                self.transport_failures_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            FeedbackResultKind::Timeout => {
                self.timeouts_total.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.last_result
            .store(encode_result(kind), Ordering::Relaxed);
        self.last_transaction_duration_ns
            .store(duration_ns, Ordering::Relaxed);
        self.has_last_transaction_duration
            .store(true, Ordering::Relaxed);
    }

    /// Point-in-time snapshot of the recorded totals (relaxed loads).
    pub fn snapshot(&self) -> FeedbackSnapshot {
        FeedbackSnapshot {
            attempts_total: self.attempts_total.load(Ordering::Relaxed),
            successes_total: self.successes_total.load(Ordering::Relaxed),
            protocol_failures_total: self.protocol_failures_total.load(Ordering::Relaxed),
            transport_failures_total: self.transport_failures_total.load(Ordering::Relaxed),
            timeouts_total: self.timeouts_total.load(Ordering::Relaxed),
            last_result: decode_result(self.last_result.load(Ordering::Relaxed)),
            last_transaction_duration_ns: if self
                .has_last_transaction_duration
                .load(Ordering::Relaxed)
            {
                Some(self.last_transaction_duration_ns.load(Ordering::Relaxed))
            } else {
                None
            },
            last_success_elapsed_ns: if self.has_last_success_elapsed.load(Ordering::Relaxed) {
                Some(self.last_success_elapsed_ns.load(Ordering::Relaxed))
            } else {
                None
            },
        }
    }
}

/// Classify a core error from the feedback path into a truthful result kind.
pub(crate) fn classify_feedback_error(err: &CoreError) -> FeedbackResultKind {
    match err {
        CoreError::Connection(_) => FeedbackResultKind::TransportFailure,
        CoreError::Timeout => FeedbackResultKind::Timeout,
        // Everything else (RTSP status/plist/pairing/crypto/parse/streaming)
        // is a protocol-level rejection rather than a socket failure.
        _ => FeedbackResultKind::ProtocolFailure,
    }
}

// ===========================================================================
// Errors, roles, and diagnostic events
// ===========================================================================

/// Coarse truthful classification of a client-side error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientDiagnosticErrorKind {
    ProtocolFailure,
    TransportFailure,
    Timeout,
    Other,
}

/// A client-side error reduced to its classification and a display-safe
/// message. Contains no packet/body/header/key material beyond what the
/// existing `Display` implementations already emit (fixed labels and error
/// kinds only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientDiagnosticError {
    pub kind: ClientDiagnosticErrorKind,
    pub public_message: String,
}

impl ClientDiagnosticError {
    /// Reduce a core error to its classification and display message.
    pub fn from_core_error(err: &CoreError) -> Self {
        let kind = match err {
            CoreError::Connection(_) => ClientDiagnosticErrorKind::TransportFailure,
            CoreError::Timeout => ClientDiagnosticErrorKind::Timeout,
            CoreError::Discovery(_) => ClientDiagnosticErrorKind::Other,
            _ => ClientDiagnosticErrorKind::ProtocolFailure,
        };
        Self {
            kind,
            public_message: err.to_string(),
        }
    }

    fn from_feedback_kind(kind: FeedbackResultKind, context: &str) -> Self {
        let kind = match kind {
            FeedbackResultKind::Success => {
                return Self {
                    kind: ClientDiagnosticErrorKind::Other,
                    public_message: format!("{context}: succeeded"),
                }
            }
            FeedbackResultKind::ProtocolFailure => ClientDiagnosticErrorKind::ProtocolFailure,
            FeedbackResultKind::TransportFailure => ClientDiagnosticErrorKind::TransportFailure,
            FeedbackResultKind::Timeout => ClientDiagnosticErrorKind::Timeout,
        };
        Self {
            kind,
            public_message: format!("{context} failed"),
        }
    }
}

/// This connection's role in the client's timing topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClientTimingRole {
    /// Not sharing a PTP clock with other connections in this process.
    #[default]
    Single,
    /// Root of the shared group clock (runs the BMCA/master flow others derive from).
    PtpPrimary,
    /// Group member mirroring the primary connection's clock.
    PtpSecondary,
}

/// Kind of a significant client diagnostic event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientDiagnosticKind {
    /// Classified feedback/keepalive outcome.
    Feedback,
    /// RTSP/session setup failed.
    SetupFailure,
    /// Recoverable runtime problem (e.g. retransmit serving failed).
    RuntimeWarning,
}

// ===========================================================================
// Bounded event ring (per connection)
// ===========================================================================

const EVENT_RING_CAPACITY: usize = 256;

/// Bounded FIFO of significant events with exactly-once draining.
///
/// Recording never blocks: when the ring is full the oldest event is evicted
/// (counted internally) and the new event takes its slot.
#[derive(Debug, Default)]
pub(crate) struct EventSink {
    ring: Mutex<VecDeque<(ClientDiagnosticEvent, u64)>>,
    next_seq: AtomicU64,
    overflow_dropped_total: AtomicU64,
}

impl EventSink {
    fn new() -> Self {
        Self::default()
    }

    /// Record one event stamped with process elapsed nanoseconds.
    fn record(&self, device_id: Option<DeviceId>, kind: ClientDiagnosticKind) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let elapsed_ns = process_elapsed_ns();
        let event = ClientDiagnosticEvent {
            elapsed_ns,
            seq,
            device_id,
            kind,
        };
        let mut ring = lock(&self.ring);
        while ring.len() >= EVENT_RING_CAPACITY {
            ring.pop_front();
            self.overflow_dropped_total.fetch_add(1, Ordering::Relaxed);
        }
        ring.push_back((event, seq));
    }

    /// Remove every buffered event (preserving FIFO order).
    fn take_all(&self) -> Vec<(ClientDiagnosticEvent, u64)> {
        let mut ring = lock(&self.ring);
        ring.drain(..).collect()
    }

    /// Spill undrained events back in their original order.
    fn restore(&self, items: Vec<(ClientDiagnosticEvent, u64)>) {
        if items.is_empty() {
            return;
        }
        let mut ring = lock(&self.ring);
        for item in items {
            ring.push_back(item);
        }
    }
}

// ===========================================================================
// Per-connection shared state
// ===========================================================================

/// Mirrored live state published by [`crate::connection::Connection`] at its
/// transition points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionLiveState {
    pub rtsp_state: RtspSessionState,
    pub playback_state: PlaybackState,
    pub timing_role: ClientTimingRole,
    pub timing_reference: TimingReferenceKind,
}

impl Default for ConnectionLiveState {
    fn default() -> Self {
        Self {
            rtsp_state: RtspSessionState::Disconnected,
            playback_state: PlaybackState::Stopped,
            timing_role: ClientTimingRole::Single,
            timing_reference: TimingReferenceKind::SenderReference,
        }
    }
}

/// Shared per-connection diagnostics state written by the connection at its
/// exact transition/mutation sites and read by snapshots.
pub(crate) struct EntryShared {
    device_id: DeviceId,
    sender_target_index: AtomicUsize,
    has_target_index: AtomicBool,
    live_state: Mutex<ConnectionLiveState>,
    feedback: FeedbackRecorder,
    last_error: Mutex<Option<ClientDiagnosticError>>,
    /// Streamer-backed audio source when THIS connection owns its streamer
    /// (single-device streaming). Group members have none; their transport
    /// halves are joined from the client-level audio snapshot instead.
    streamer_audio: Mutex<Option<AudioDiagnosticsSource>>,
    sink: Arc<EventSink>,
}

impl EntryShared {
    pub(crate) fn new(device_id: DeviceId) -> Arc<Self> {
        Arc::new(Self {
            device_id,
            sender_target_index: AtomicUsize::new(0),
            has_target_index: AtomicBool::new(false),
            live_state: Mutex::new(ConnectionLiveState::default()),
            feedback: FeedbackRecorder::new(),
            last_error: Mutex::new(None),
            streamer_audio: Mutex::new(None),
            sink: Arc::new(EventSink::new()),
        })
    }

    pub(crate) fn event_sink(&self) -> Arc<EventSink> {
        Arc::clone(&self.sink)
    }

    /// Publish current RTSP/playback mirrors (called at transition sites).
    pub(crate) fn publish_live_state(
        &self,
        rtsp_state: RtspSessionState,
        playback_state: PlaybackState,
    ) {
        let mut live = lock(&self.live_state);
        live.rtsp_state = rtsp_state;
        live.playback_state = playback_state;
    }

    /// Record which timing topology role this connection took on (set at the
    /// exact setup branch that decided it).
    pub(crate) fn set_timing_role(&self, role: ClientTimingRole, reference: TimingReferenceKind) {
        let mut live = lock(&self.live_state);
        live.timing_role = role;
        live.timing_reference = reference;
    }

    /// Store the sender/target index assigned during group setup (or 0 for
    /// single-device streaming).
    pub(crate) fn set_sender_target_index(&self, index: usize) {
        self.sender_target_index.store(index, Ordering::Relaxed);
        self.has_target_index.store(true, Ordering::Relaxed);
    }

    /// Attach this connection's own streamer-backed audio source (set once
    /// streaming starts).
    pub(crate) fn set_streamer_audio_source(&self, source: AudioDiagnosticsSource) {
        *lock(&self.streamer_audio) = Some(source);
    }

    /// Record a classified feedback outcome: totals, bounded event, and the
    /// last error for non-success results. Non-blocking.
    pub(crate) fn record_feedback_result(&self, kind: FeedbackResultKind, duration: Duration) {
        self.feedback.record(
            kind,
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
            process_elapsed_ns(),
        );
        self.sink
            .record(Some(self.device_id.clone()), ClientDiagnosticKind::Feedback);
        if kind != FeedbackResultKind::Success {
            *lock(&self.last_error) =
                Some(ClientDiagnosticError::from_feedback_kind(kind, "Feedback"));
        }
    }

    /// Record a failed RTSP setup (member failure site). Non-blocking.
    pub(crate) fn record_setup_failure(&self, err: &CoreError) {
        *lock(&self.last_error) = Some(ClientDiagnosticError::from_core_error(err));
        self.sink.record(
            Some(self.device_id.clone()),
            ClientDiagnosticKind::SetupFailure,
        );
    }

    /// Record a recoverable runtime warning (e.g. retransmit serving failed).
    pub(crate) fn record_runtime_warning(&self) {
        self.sink.record(
            Some(self.device_id.clone()),
            ClientDiagnosticKind::RuntimeWarning,
        );
    }

    fn sender_target_index(&self) -> Option<usize> {
        if self.has_target_index.load(Ordering::Relaxed) {
            Some(self.sender_target_index.load(Ordering::Relaxed))
        } else {
            None
        }
    }

    fn transport_snapshot(
        &self,
        joined_targets: Option<&[TargetTransportSnapshot]>,
    ) -> TargetTransportSnapshot {
        let index = self.sender_target_index().unwrap_or(0);
        let own = lock(&self.streamer_audio);
        if let Some(source) = own.as_ref() {
            let audio: AudioDiagnosticsSnapshot = source.snapshot();
            return audio.targets.get(index).copied().unwrap_or_default();
        }
        if let Some(targets) = joined_targets {
            return targets.get(index).copied().unwrap_or_default();
        }
        // Neither this connection nor the client owns stream telemetry for
        // this receiver (e.g. connected but not yet streaming): report the
        // zeroed default instead of inventing activity.
        TargetTransportSnapshot::default()
    }

    /// Build the per-connection snapshot row.
    ///
    /// `now_elapsed_ns` is accepted for interface symmetry with the client
    /// snapshot; it is currently unused because timing measurements are
    /// genuinely unavailable (see the module docs) and no sample age can be
    /// derived without them.
    fn snapshot(
        &self,
        now_elapsed_ns: u64,
        joined_targets: Option<&[TargetTransportSnapshot]>,
    ) -> ConnectionDiagnosticsSnapshot {
        let _ = now_elapsed_ns;
        let live = *lock(&self.live_state);
        let last_error = lock(&self.last_error).clone();
        ConnectionDiagnosticsSnapshot {
            device_id: self.device_id.clone(),
            sender_target_index: self.sender_target_index(),
            rtsp_state: live.rtsp_state,
            playback_state: live.playback_state,
            timing_role: live.timing_role,
            timing: TimingDiagnosticsSnapshot {
                reference: live.timing_reference,
                // Measurement values are genuinely unavailable here: the PTP
                // exchange loops own their measurement state inside
                // airplay-timing free functions that this crate cannot reach.
                // Unavailable is reported as None/false, never as zeros.
                latest: None,
                sample_age_ns: None,
                stale: false,
                drift: None,
            },
            transport: self.transport_snapshot(joined_targets),
            retransmit: RetransmitSnapshot::default(),
            feedback: self.feedback.snapshot(),
            last_error,
        }
    }
}

// ===========================================================================
// Snapshots
// ===========================================================================

/// Diagnostics for one receiver connection, keyed by stable [`DeviceId`].
#[derive(Clone, Debug)]
pub struct ConnectionDiagnosticsSnapshot {
    pub device_id: DeviceId,
    /// Sender/target index used for per-target transport joins, when known.
    pub sender_target_index: Option<usize>,
    pub rtsp_state: RtspSessionState,
    pub playback_state: PlaybackState,
    pub timing_role: ClientTimingRole,
    pub timing: TimingDiagnosticsSnapshot,
    pub transport: TargetTransportSnapshot,
    /// Always zeroed here: streamer retransmit counters are cumulative across
    /// all targets and are reported once per client in the audio snapshot.
    pub retransmit: RetransmitSnapshot,
    pub feedback: FeedbackSnapshot,
    pub last_error: Option<ClientDiagnosticError>,
}

/// Aggregated point-in-time client diagnostics.
#[derive(Clone, Debug)]
pub struct ClientDiagnosticsSnapshot {
    pub captured_elapsed_ns: u64,
    pub audio: Option<AudioDiagnosticsSnapshot>,
    /// Receiver rows sorted by `DeviceId` bytes — stable across insertion order.
    pub connections: Vec<ConnectionDiagnosticsSnapshot>,
    /// Increments only where client events are actually dropped. No such
    /// drop site exists today (`EventHandler` deliveries are unbounded direct
    /// calls), so this truthfully stays 0 until one appears.
    pub client_events_dropped_total: u64,
}

struct Inner {
    entries: RwLock<Vec<Arc<EntryShared>>>,
    audio: RwLock<Option<AudioDiagnosticsSource>>,
    client_events_dropped_total: AtomicU64,
}

/// Cloneable read source over client diagnostics plus a bounded event feed.
///
/// This is the only client input consumed by the application diagnostics
/// registry (plan Task 6/8): [`Self::snapshot`] and [`Self::drain_events`].
#[derive(Clone)]
pub struct ClientDiagnosticsSource {
    inner: Arc<Inner>,
}

impl ClientDiagnosticsSource {
    /// Assemble a source from per-connection entries plus an optional
    /// streamer-backed audio half (used by the client-level aggregate).
    pub(crate) fn from_parts(
        entries: Vec<Arc<EntryShared>>,
        audio: Option<AudioDiagnosticsSource>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: RwLock::new(entries),
                audio: RwLock::new(audio),
                client_events_dropped_total: AtomicU64::new(0),
            }),
        }
    }

    fn entries(&self) -> Vec<Arc<EntryShared>> {
        read_lock(&self.inner.entries).clone()
    }

    /// Take a point-in-time snapshot. Never blocks producers; individual
    /// counters are relaxed loads that may mix slightly different instants.
    pub fn snapshot(&self, now_elapsed_ns: u64) -> ClientDiagnosticsSnapshot {
        let entries = self.entries();
        let joined_targets: Option<Vec<TargetTransportSnapshot>> = read_lock(&self.inner.audio)
            .as_ref()
            .map(|audio| audio.snapshot().targets);
        let mut connections: Vec<ConnectionDiagnosticsSnapshot> = entries
            .iter()
            .map(|entry| entry.snapshot(now_elapsed_ns, joined_targets.as_deref()))
            .collect();
        connections.sort_unstable_by_key(|c| c.device_id.0);

        ClientDiagnosticsSnapshot {
            captured_elapsed_ns: now_elapsed_ns,
            audio: read_lock(&self.inner.audio).as_ref().map(|a| a.snapshot()),
            connections,
            client_events_dropped_total: self
                .inner
                .client_events_dropped_total
                .load(Ordering::Relaxed),
        }
    }

    /// Drain up to `limit` significant events, strictly FIFO across all
    /// registered connections, exactly once.
    ///
    /// Crate-internal contract consumed by the application diagnostics
    /// registry; kept public (doc-visible) because the registry lives in
    /// another workspace crate.
    pub fn drain_events(&self, limit: usize) -> Vec<ClientDiagnosticEvent> {
        let entries = self.entries();
        if entries.is_empty() || limit == 0 {
            return Vec::new();
        }

        // Take everything out (each sink briefly, one at a time).
        let mut pooled: Vec<(u64, u64, usize, ClientDiagnosticEvent)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            for (event, seq) in entry.event_sink().take_all() {
                pooled.push((event.elapsed_ns, seq, index, event));
            }
        }
        pooled.sort_unstable_by_key(|(elapsed, seq, _, _)| (*elapsed, *seq));

        // split_off leaves the first `limit` entries in `pooled` and returns
        // the remainder that must be restored afterwards.
        let remainder: Vec<_> = pooled.split_off(pooled.len().min(limit));
        let drained: Vec<ClientDiagnosticEvent> =
            pooled.into_iter().map(|(_, _, _, event)| event).collect();

        // Spill the remainder back, grouped per sink, order preserved.
        let mut rest_by_sink: Vec<Vec<(ClientDiagnosticEvent, u64)>> =
            vec![Vec::new(); entries.len()];
        for (_, seq, index, event) in remainder {
            rest_by_sink[index].push((event, seq));
        }
        for (index, items) in rest_by_sink.into_iter().enumerate() {
            if !items.is_empty() {
                entries[index].event_sink().restore(items);
            }
        }
        drained
    }

    // ------------------------------------------------------------------
    // Documented test seams (hardware-independent contract tests).
    // ------------------------------------------------------------------

    /// Test seam: an empty source with no connections and no audio half.
    #[doc(hidden)]
    pub fn test_new_empty() -> Self {
        Self::from_parts(Vec::new(), None)
    }

    /// Test seam: register a synthetic connection entry with a fresh event
    /// sink. Real sources register entries through the connection/client
    /// constructors instead.
    #[doc(hidden)]
    pub fn test_register_connection(&self, device_id: DeviceId, target_index: Option<usize>) {
        let entry = EntryShared::new(device_id);
        if let Some(index) = target_index {
            entry.set_sender_target_index(index);
        }
        self.inner
            .entries
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .push(entry);
    }

    /// Test seam: a standalone feedback recorder for classification tests.
    #[doc(hidden)]
    pub fn test_feedback_recorder() -> FeedbackRecorder {
        FeedbackRecorder::new()
    }

    /// Test seam: push one event through the first registered entry's real
    /// recording path.
    #[doc(hidden)]
    pub fn test_record_event(&self, device_id: Option<DeviceId>, kind: ClientDiagnosticKind) {
        if let Some(entry) = self.entries().first() {
            entry.event_sink().record(device_id, kind);
        }
    }

    /// Test seam: drain through the production drain path.
    #[doc(hidden)]
    pub fn test_drain_events(&self, limit: usize) -> Vec<ClientDiagnosticEvent> {
        self.drain_events(limit)
    }
}
