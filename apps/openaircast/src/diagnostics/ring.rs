//! Bounded cursor ring storage for structured diagnostic events.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::snapshot::{
    DiagnosticEvent, DiagnosticEventDraft, EventBatch, EventCursor, EventGap, EVENT_RING_CAPACITY,
    MAX_EVENT_READ_LIMIT,
};

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
    pub(super) fn export_batch(&self) -> (Vec<DiagnosticEvent>, Option<EventGap>) {
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
