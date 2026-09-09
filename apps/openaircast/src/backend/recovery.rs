//! Deterministic recovery policy: exponential backoff with stable per-receiver
//! jitter, healthy-streaming reset, manual retry, and rejoin-restart rate
//! limiting.
//!
//! All time flows through the injected [`Clock`] so every decision is
//! reproducible in tests without sleeping.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use crate::backend::model::{
    ReceiverId, ReceiverLifecycle, ReceiverRole, SetupPhase, UserFacingError,
};

/// Monotonic time source.
///
/// Production uses [`SystemClock`]; tests inject a manually advanced fake.
pub trait Clock: Send + Sync {
    /// Returns the current monotonic instant.
    fn now(&self) -> Instant;
}

/// Real monotonic clock backed by [`Instant::now`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Hard ceiling for any computed delay.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// FNV-style 64-bit prime used by the deterministic jitter hash.
const JITTER_HASH_PRIME: u64 = 0x100_0000_01b3;

/// Exponential reconnect backoff with deterministic ±jitter percent.
///
/// The base schedule is 1, 2, 4, 8, 16, and 30 seconds. Jitter scales each
/// delay by a stable offset in `±jitter_percent` derived only from the
/// receiver identity (or discovery generation), the attempt number, and this
/// configuration — identical inputs always produce identical delays.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Base delays indexed by `min(attempt, len - 1)`; attempt zero first.
    pub base: [Duration; 6],
    /// Maximum symmetric jitter percentage applied to every delay.
    pub jitter_percent: u8,
    /// Continuous streaming required before a receiver's attempts reset.
    pub healthy_reset_after: Duration,
    /// Continuous discoverability required before a rejoin restart may run.
    pub discovery_stable_for: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: [1, 2, 4, 8, 16, 30].map(Duration::from_secs),
            jitter_percent: 20,
            healthy_reset_after: Duration::from_secs(60),
            discovery_stable_for: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// Deterministic delay for the given receiver and zero-based attempt.
    ///
    /// The jitter seed folds the receiver's storage-key bytes with the
    /// attempt number through an FNV-1a style multiply-add, maps the hash
    /// onto `[-jitter_percent, +jitter_percent]`, and caps the result at
    /// thirty seconds.
    pub fn delay(&self, receiver: &ReceiverId, attempt: u32) -> Duration {
        let base = self.base[usize::min(attempt as usize, self.base.len() - 1)];
        let hash = receiver
            .storage_key()
            .bytes()
            .fold(u64::from(attempt), |value, byte| {
                value
                    .wrapping_mul(JITTER_HASH_PRIME)
                    .wrapping_add(u64::from(byte))
            });
        scale_with_hash(base, hash, self.jitter_percent)
    }

    /// Deterministic discovery-restart delay seeded by generation and
    /// attempt instead of receiver identity.
    pub fn discovery_delay(&self, generation: u64, attempt: u32) -> Duration {
        let base = self.base[usize::min(attempt as usize, self.base.len() - 1)];
        let hash = generation
            .to_le_bytes()
            .into_iter()
            .fold(u64::from(attempt), |value, byte| {
                value
                    .wrapping_mul(JITTER_HASH_PRIME)
                    .wrapping_add(u64::from(byte))
            });
        scale_with_hash(base, hash, self.jitter_percent)
    }
}

/// Scales `base` by the signed percentage encoded in `hash`.
fn scale_with_hash(base: Duration, hash: u64, jitter_percent: u8) -> Duration {
    let span = i64::from(jitter_percent);
    let modulus = (span * 2 + 1) as u64;
    let signed = (hash % modulus) as i64 - span;
    let scaled = base.mul_f64(1.0 + signed as f64 / 100.0);
    scaled.min(MAX_RETRY_DELAY)
}

/// Per-receiver recovery bookkeeping.
#[derive(Clone, Copy, Debug, Default)]
struct ReceiverRetryEntry {
    attempt: u32,
    next_retry: Option<Instant>,
    streaming_since: Option<Instant>,
}

/// Attempt counters, retry deadlines, and healthy-streaming windows for all
/// receivers under one policy.
#[derive(Default)]
pub struct RetryState {
    receivers: BTreeMap<ReceiverId, ReceiverRetryEntry>,
}

impl RetryState {
    /// Creates empty bookkeeping.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one failed attempt and schedules the next retry.
    ///
    /// Returns the deadline after which reconciliation may try again; a
    /// failure also ends any continuous-streaming window.
    pub fn note_failure(
        &mut self,
        policy: &RetryPolicy,
        now: Instant,
        receiver: ReceiverId,
    ) -> Instant {
        let entry = self.receivers.entry(receiver).or_default();
        entry.attempt = entry.attempt.saturating_add(1);
        entry.streaming_since = None;
        let index = entry.attempt - 1;
        let deadline = now + policy.delay(&receiver, index);
        entry.next_retry = Some(deadline);
        deadline
    }

    /// Reports that the receiver is streaming at `now`.
    ///
    /// Attempts reset (and `true` is returned) only once streaming has been
    /// continuously healthy for [`RetryPolicy::healthy_reset_after`]; the
    /// window itself stays armed for later resets.
    pub fn note_streaming(
        &mut self,
        policy: &RetryPolicy,
        now: Instant,
        receiver: ReceiverId,
    ) -> bool {
        let entry = self.receivers.entry(receiver).or_default();
        match entry.streaming_since {
            None => {
                entry.streaming_since = Some(now);
                false
            }
            Some(since) if now.duration_since(since) >= policy.healthy_reset_after => {
                entry.attempt = 0;
                entry.next_retry = None;
                true
            }
            Some(_) => false,
        }
    }

    /// Clears the pending retry deadline for `receiver` without touching its
    /// attempt history or desired membership.
    pub fn clear_deadline(&mut self, receiver: ReceiverId) {
        if let Some(entry) = self.receivers.get_mut(&receiver) {
            entry.next_retry = None;
        }
    }

    /// Manual-retry spelling of [`Self::clear_deadline`].
    ///
    /// Kept as its own name because the `RetryReceiver` command means exactly
    /// this and nothing else: drop the deadline, keep the attempt history.
    pub fn manual_retry(&mut self, receiver: ReceiverId) {
        self.clear_deadline(receiver);
    }

    /// Receivers whose scheduled retry deadline has passed at `now`.
    ///
    /// The deadline ladder is only a policy until something consumes it; this
    /// is that consumer's query, so no caller has to reach into the map.
    pub fn due_at(&self, now: Instant) -> Vec<ReceiverId> {
        self.receivers
            .iter()
            .filter(|(_, entry)| entry.next_retry.is_some_and(|at| at <= now))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Earliest pending retry deadline across all receivers.
    pub fn earliest_retry(&self) -> Option<Instant> {
        self.receivers
            .values()
            .filter_map(|entry| entry.next_retry)
            .min()
    }

    /// Current consecutive failure count for `receiver`.
    pub fn attempt(&self, receiver: ReceiverId) -> u32 {
        self.receivers
            .get(&receiver)
            .map_or(0, |entry| entry.attempt)
    }

    /// Pending retry deadline for `receiver`, if one is scheduled.
    pub fn next_retry(&self, receiver: ReceiverId) -> Option<Instant> {
        self.receivers
            .get(&receiver)
            .and_then(|entry| entry.next_retry)
    }
}

/// Rate limiter for rejoin-triggered full-session restarts.
///
/// A receiver becomes eligible only after being continuously discoverable
/// for two seconds, and such restarts are spaced at least thirty seconds
/// apart so a flapping receiver cannot repeatedly interrupt healthy members.
pub struct RejoinRestartLimiter {
    clock: std::sync::Arc<dyn Clock>,
    online_since: BTreeMap<ReceiverId, Instant>,
    last_rejoin_restart: Option<Instant>,
}

/// Continuous discoverability required before a rejoin restart is allowed.
const REJOIN_ONLINE_STABLE_FOR: Duration = Duration::from_secs(2);
/// Minimum spacing between rejoin-triggered group restarts.
const REJOIN_RESTART_SPACING: Duration = Duration::from_secs(30);

impl RejoinRestartLimiter {
    /// Creates a limiter driven by the injected clock.
    pub fn new(clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            clock,
            online_since: BTreeMap::new(),
            last_rejoin_restart: None,
        }
    }

    /// Records (or restarts) the discoverability window of `receiver`.
    pub fn note_discovered(&mut self, receiver: ReceiverId) {
        let now = self.clock.now();
        self.online_since.entry(receiver).or_insert(now);
    }

    /// Drops the discoverability window; the next sighting starts over.
    pub fn note_lost(&mut self, receiver: ReceiverId) {
        self.online_since.remove(&receiver);
    }

    /// Whether `receiver` has been continuously discoverable long enough to
    /// justify a rejoin restart.
    pub fn may_rejoin(&self, receiver: ReceiverId) -> bool {
        self.rejoin_eligible_at(receiver)
            .is_some_and(|at| at <= self.clock.now())
    }

    /// Instant from which `receiver`'s discoverability window is long enough.
    ///
    /// `None` means discovery currently offers no castable service, so no
    /// amount of waiting would make this receiver eligible; the caller must
    /// arm nothing until [`Self::note_discovered`] runs again.
    pub fn rejoin_eligible_at(&self, receiver: ReceiverId) -> Option<Instant> {
        self.online_since
            .get(&receiver)
            .map(|since| *since + REJOIN_ONLINE_STABLE_FOR)
    }

    /// Instant from which the next rejoin restart is allowed.
    ///
    /// `None` means no rejoin restart has happened yet, so the first one is
    /// unconstrained.
    pub fn group_allowed_at(&self) -> Option<Instant> {
        self.last_rejoin_restart
            .map(|at| at + REJOIN_RESTART_SPACING)
    }

    /// Records that a rejoin-triggered restart happened just now.
    pub fn note_restart(&mut self) {
        self.last_rejoin_restart = Some(self.clock.now());
    }

    /// Whether enough time has passed since the previous rejoin restart.
    pub fn may_restart_group(&self) -> bool {
        let now = self.clock.now();
        self.last_rejoin_restart
            .is_none_or(|at| now.duration_since(at) >= REJOIN_RESTART_SPACING)
    }

    /// Combined gate used by session reconciliation before restarting.
    pub fn may_rejoin_restart(&self, receiver: ReceiverId) -> bool {
        self.may_rejoin(receiver) && self.may_restart_group()
    }
}

// ---------------------------------------------------------------------------
// Receiver state machine (Task 16 Step 3)
// ---------------------------------------------------------------------------

/// A rejected receiver-lifecycle transition.
///
/// Both ends are rendered as static variant labels, never as payloads, so the
/// invariant diagnostic derived from this value stays presentation-safe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTransition {
    /// Label of the state the receiver was in.
    pub from: &'static str,
    /// Label of the state that was rejected.
    pub to: &'static str,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid receiver transition {} -> {}",
            self.from, self.to
        )
    }
}

/// Static variant label of a lifecycle state.
fn label(state: &ReceiverLifecycle) -> &'static str {
    match state {
        ReceiverLifecycle::Discovered => "Discovered",
        ReceiverLifecycle::Connecting { .. } => "Connecting",
        ReceiverLifecycle::SettingUp { .. } => "SettingUp",
        ReceiverLifecycle::Ready { .. } => "Ready",
        ReceiverLifecycle::Streaming { .. } => "Streaming",
        ReceiverLifecycle::RetryWaiting { .. } => "RetryWaiting",
        ReceiverLifecycle::Unavailable => "Unavailable",
        ReceiverLifecycle::Failed { .. } => "Failed",
    }
}

/// Whether `from -> to` is one of the permitted edges.
///
/// This is the approved edge set verbatim; nothing else may pass. It governs
/// the transitions this crate decides on its own (discovery availability,
/// setup progress, backoff). Session-authoritative operations
/// ([`ReceiverMachine::begin_generation`], [`ReceiverMachine::enter_streaming`],
/// [`ReceiverMachine::fail`]) and the generation boundary
/// [`ReceiverMachine::leave_session`] are NOT edges of this relation: they
/// report what the transport actually did, so they re-anchor the row instead
/// of vetoing it.
fn permitted(from: &ReceiverLifecycle, to: &ReceiverLifecycle) -> bool {
    use ReceiverLifecycle as L;
    match (from, to) {
        // Discovery losing the castable service, but only for receivers no
        // generation currently owns -- `set_service_available` defers the rest.
        (_, L::Unavailable) => true,
        // Unavailable -> Discovered once the service is castable again.
        (L::Unavailable, L::Discovered) => true,
        (L::Discovered, L::Connecting { .. }) => true,
        (L::Connecting { .. }, L::SettingUp { .. }) => true,
        // Setup phases progress within SettingUp.
        (L::SettingUp { .. }, L::SettingUp { .. }) => true,
        (L::SettingUp { .. }, L::Ready { .. }) => true,
        (L::Ready { .. }, L::Streaming { .. }) => true,
        (
            L::Connecting { .. } | L::SettingUp { .. } | L::Ready { .. } | L::Streaming { .. },
            L::Failed { .. },
        ) => true,
        (
            L::Failed {
                retryable: true, ..
            },
            L::RetryWaiting { .. },
        ) => true,
        (L::RetryWaiting { .. }, L::Connecting { .. }) => true,
        // Manual retry is the documented escape from a terminal failure; a
        // non-retryable one would otherwise absorb the receiver for good.
        (L::Failed { .. }, L::Connecting { .. }) => true,
        _ => false,
    }
}

/// Whether a session generation currently owns this receiver.
fn owned_by_generation(state: &ReceiverLifecycle) -> bool {
    matches!(
        state,
        ReceiverLifecycle::Connecting { .. }
            | ReceiverLifecycle::SettingUp { .. }
            | ReceiverLifecycle::Ready { .. }
            | ReceiverLifecycle::Streaming { .. }
    )
}

/// Per-receiver lifecycle, attempt bookkeeping, and last failure reason.
///
/// The machine owns the receiver's published [`ReceiverLifecycle`]; the
/// controller never assembles one by hand. Rejected transitions panic in debug
/// builds (so a test observes them) and surface as [`InvalidTransition`] in
/// release builds, where the caller turns them into an invariant diagnostic.
#[derive(Clone, Debug)]
pub struct ReceiverMachine {
    state: ReceiverLifecycle,
    attempt: u32,
    next_retry: Option<SystemTime>,
    last_error: Option<UserFacingError>,
    generation: u64,
    service_available: bool,
    role: Option<ReceiverRole>,
}

impl ReceiverMachine {
    /// Creates a machine for a receiver whose service availability is known.
    pub fn new(service_available: bool) -> Self {
        Self {
            state: if service_available {
                ReceiverLifecycle::Discovered
            } else {
                ReceiverLifecycle::Unavailable
            },
            attempt: 0,
            next_retry: None,
            last_error: None,
            generation: 0,
            service_available,
            role: None,
        }
    }

    /// Currently published lifecycle state.
    pub fn state(&self) -> &ReceiverLifecycle {
        &self.state
    }

    /// Consecutive failed attempts recorded for this receiver.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Wall-clock deadline of the pending retry, if one is scheduled.
    pub fn next_retry(&self) -> Option<SystemTime> {
        self.next_retry
    }

    /// Pre-redacted summary of the most recent failure.
    pub fn last_error(&self) -> Option<&UserFacingError> {
        self.last_error.as_ref()
    }

    /// Session generation this receiver last participated in.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether discovery currently offers a castable AirPlay service.
    pub fn service_available(&self) -> bool {
        self.service_available
    }

    /// Role negotiated in the current generation, if any.
    pub fn role(&self) -> Option<ReceiverRole> {
        self.role
    }

    /// Whether the receiver may be dropped from the snapshot.
    ///
    /// Inventory cleanup, not a lifecycle variant: a receiver that is no
    /// longer desired stays visible until it is unavailable.
    pub fn removable(&self) -> bool {
        matches!(self.state, ReceiverLifecycle::Unavailable)
    }

    /// Whether this receiver is desired-but-outside the running generation
    /// and could therefore be pulled back in by an automatic rejoin.
    ///
    /// Covers both halves of the gap that a discovery-driven rejoin has to
    /// close: a receiver still serving its backoff, and one whose service
    /// vanished and came back, which left backoff for `Discovered` without
    /// anything ever reconnecting it. A terminal failure stays out -- only a
    /// manual retry releases that.
    pub fn awaiting_rejoin(&self) -> bool {
        matches!(
            self.state,
            ReceiverLifecycle::Discovered
                | ReceiverLifecycle::RetryWaiting { .. }
                | ReceiverLifecycle::Failed {
                    retryable: true,
                    ..
                }
        )
    }

    /// Whether reconciliation must currently leave this receiver out.
    ///
    /// Receivers serving their backoff (or terminally failed) are excluded
    /// from the desired set handed to the transport, so the ladder actually
    /// governs when they are tried again. A terminal failure is not an
    /// absorbing state: [`Self::retry_now`] releases it on demand.
    pub fn in_backoff(&self) -> bool {
        matches!(
            self.state,
            ReceiverLifecycle::RetryWaiting { .. } | ReceiverLifecycle::Failed { .. }
        )
    }

    /// Applies one permitted transition.
    fn transition(&mut self, to: ReceiverLifecycle) -> Result<(), InvalidTransition> {
        if self.state == to {
            return Ok(()); // idempotent republication, not a transition
        }
        if !permitted(&self.state, &to) {
            let rejected = InvalidTransition {
                from: label(&self.state),
                to: label(&to),
            };
            debug_assert!(false, "{rejected}");
            return Err(rejected);
        }
        self.state = to;
        Ok(())
    }

    /// Records discovery availability, moving to/from `Unavailable`.
    ///
    /// Losing the service is a discovery fact, never a session fact: while a
    /// generation owns this receiver the loss is only remembered, because the
    /// transport is still streaming to it and the row must not contradict
    /// `session.active`. [`Self::leave_session`] applies the deferred loss at
    /// the generation boundary.
    pub fn set_service_available(&mut self, available: bool) -> Result<(), InvalidTransition> {
        self.service_available = available;
        if available {
            if matches!(self.state, ReceiverLifecycle::Unavailable) {
                return self.transition(ReceiverLifecycle::Discovered);
            }
            Ok(())
        } else if owned_by_generation(&self.state) {
            Ok(()) // deferred until the generation releases the receiver
        } else {
            self.role = None;
            self.transition(ReceiverLifecycle::Unavailable)
        }
    }

    /// Enters `Connecting` for a newly started session generation.
    ///
    /// Session-authoritative and therefore NOT an edge of [`permitted`]: the
    /// supervisor is connecting this receiver right now, so `Connecting` is
    /// the truthful row no matter what the previous one said. Discovery
    /// availability or a stale failure can legitimately have moved the row
    /// while the reconcile was in flight; re-anchoring here is what keeps that
    /// race from being mistaken for a programming error.
    pub fn begin_generation(
        &mut self,
        generation: u64,
        attempt: u32,
    ) -> Result<(), InvalidTransition> {
        self.generation = generation;
        self.attempt = attempt;
        self.role = None;
        self.state = ReceiverLifecycle::Connecting { attempt };
        Ok(())
    }

    /// Reports transport-observed setup progress.
    pub fn enter_setup(
        &mut self,
        role: ReceiverRole,
        phase: SetupPhase,
    ) -> Result<(), InvalidTransition> {
        self.role = Some(role);
        self.transition(ReceiverLifecycle::SettingUp { role, phase })
    }

    /// Walks the receiver up to `Streaming` along permitted edges only.
    ///
    /// The group transport reports membership, not each intermediate step, so
    /// the skipped states are traversed here instead of being jumped over.
    /// Like [`Self::begin_generation`] this is session-authoritative: a row
    /// that drifted out of the chain while the start was in flight is
    /// re-anchored, because the transport really did add this member.
    pub fn enter_streaming(&mut self, role: ReceiverRole) -> Result<(), InvalidTransition> {
        if matches!(self.state, ReceiverLifecycle::Streaming { role: held } if held == role) {
            return Ok(());
        }
        if !owned_by_generation(&self.state) {
            self.state = ReceiverLifecycle::Connecting {
                attempt: self.attempt,
            };
        }
        if matches!(self.state, ReceiverLifecycle::Connecting { .. }) {
            self.transition(ReceiverLifecycle::SettingUp {
                role,
                phase: SetupPhase::StartAudio,
            })?;
        }
        if matches!(self.state, ReceiverLifecycle::SettingUp { .. }) {
            self.transition(ReceiverLifecycle::Ready { role })?;
        }
        self.role = Some(role);
        self.transition(ReceiverLifecycle::Streaming { role })
    }

    /// Records a terminal-for-now failure with its pre-redacted reason.
    ///
    /// Session-authoritative like the two operations above: the transport
    /// reports the failure it observed, so the reason is recorded whatever the
    /// row happened to say (an updated reason for an already-failed receiver,
    /// or one whose service vanished in the same batch).
    pub fn fail(
        &mut self,
        retryable: bool,
        error: UserFacingError,
    ) -> Result<(), InvalidTransition> {
        self.last_error = Some(error.clone());
        self.role = None;
        self.state = ReceiverLifecycle::Failed { retryable, error };
        Ok(())
    }

    /// Moves a retryable failure into its scheduled backoff window.
    pub fn wait_retry(
        &mut self,
        attempt: u32,
        retry_at: SystemTime,
    ) -> Result<(), InvalidTransition> {
        self.attempt = attempt;
        self.next_retry = Some(retry_at);
        self.transition(ReceiverLifecycle::RetryWaiting { attempt, retry_at })
    }

    /// Clears the pending deadline and re-enters `Connecting` immediately.
    ///
    /// Backs the manual `RetryReceiver` command and the deadline-driven
    /// automatic retry: only this receiver's deadline is dropped, its attempt
    /// history stays intact. A terminal `Failed { retryable: false }` is
    /// released too -- otherwise nothing could ever free it again.
    pub fn retry_now(&mut self) -> Result<(), InvalidTransition> {
        self.next_retry = None;
        if !matches!(
            self.state,
            ReceiverLifecycle::RetryWaiting { .. } | ReceiverLifecycle::Failed { .. }
        ) {
            // Nothing was scheduled; retrying an idle receiver is a no-op
            // rather than an invariant violation.
            return Ok(());
        }
        let attempt = self.attempt;
        self.transition(ReceiverLifecycle::Connecting { attempt })
    }

    /// Returns the receiver to `Discovered` when its session generation ends.
    ///
    /// Like [`Self::begin_generation`] this is a generation boundary rather
    /// than a failure edge; waiting and failed receivers are untouched. A
    /// service loss that [`Self::set_service_available`] deferred while the
    /// generation was running is applied here.
    pub fn leave_session(&mut self) {
        if owned_by_generation(&self.state) {
            self.role = None;
            self.state = if self.service_available {
                ReceiverLifecycle::Discovered
            } else {
                ReceiverLifecycle::Unavailable
            };
        }
    }

    /// Drops every session-related state because the receiver left the
    /// desired set.
    ///
    /// Stronger than [`Self::leave_session`]: a receiver nobody wants any more
    /// must not keep a backoff deadline or a terminal failure that would keep
    /// it out of (or visible in) the next generation.
    pub fn release(&mut self) {
        if matches!(self.state, ReceiverLifecycle::Unavailable) {
            return;
        }
        self.role = None;
        self.next_retry = None;
        self.state = if self.service_available {
            ReceiverLifecycle::Discovered
        } else {
            ReceiverLifecycle::Unavailable
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod receiver_machine {
        use super::*;

        fn wall(offset_secs: u64) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(offset_secs)
        }

        #[test]
        fn permitted_chain_walks_discovered_to_streaming() {
            let mut machine = ReceiverMachine::new(true);
            assert_eq!(machine.state(), &ReceiverLifecycle::Discovered);
            machine.begin_generation(7, 0).expect("start is permitted");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Connecting { attempt: 0 }
            );
            machine
                .enter_setup(ReceiverRole::Primary, SetupPhase::Pair)
                .expect("setup is permitted");
            machine
                .enter_setup(ReceiverRole::Primary, SetupPhase::RtspSetup)
                .expect("phase progression stays inside SettingUp");
            machine
                .enter_streaming(ReceiverRole::Primary)
                .expect("ready and streaming follow");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Streaming {
                    role: ReceiverRole::Primary
                }
            );
            assert_eq!(machine.generation(), 7);
            assert_eq!(machine.role(), Some(ReceiverRole::Primary));
        }

        #[test]
        fn failure_enters_backoff_and_manual_retry_reconnects() {
            let mut machine = ReceiverMachine::new(true);
            machine.begin_generation(1, 0).expect("start");
            machine
                .enter_streaming(ReceiverRole::Secondary)
                .expect("streaming");
            machine
                .fail(true, UserFacingError::new("probe timed out"))
                .expect("streaming may fail");
            assert!(matches!(
                machine.state(),
                ReceiverLifecycle::Failed {
                    retryable: true,
                    ..
                }
            ));
            assert_eq!(
                machine.last_error().map(UserFacingError::as_str),
                Some("probe timed out")
            );
            machine.wait_retry(1, wall(30)).expect("backoff follows");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::RetryWaiting {
                    attempt: 1,
                    retry_at: wall(30)
                }
            );
            assert_eq!(machine.next_retry(), Some(wall(30)));
            assert!(machine.in_backoff(), "backoff keeps it out of reconciles");

            machine.retry_now().expect("manual retry reconnects");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Connecting { attempt: 1 }
            );
            assert_eq!(machine.next_retry(), None, "only the deadline is cleared");
            assert_eq!(machine.attempt(), 1, "attempt history survives");
        }

        #[test]
        fn losing_the_service_reaches_unavailable_outside_a_generation() {
            for build in [
                (|| ReceiverMachine::new(true)) as fn() -> ReceiverMachine,
                || {
                    let mut machine = ReceiverMachine::new(true);
                    machine.begin_generation(1, 0).expect("start");
                    machine
                        .fail(true, UserFacingError::new("gone"))
                        .expect("failure");
                    machine.wait_retry(1, wall(5)).expect("backoff");
                    machine
                },
            ] {
                let mut machine = build();
                machine
                    .set_service_available(false)
                    .expect("an unowned receiver may become unavailable");
                assert_eq!(machine.state(), &ReceiverLifecycle::Unavailable);
                assert!(machine.removable(), "unavailable rows may be pruned");
                machine
                    .set_service_available(true)
                    .expect("castable again returns to Discovered");
                assert_eq!(machine.state(), &ReceiverLifecycle::Discovered);
                assert!(!machine.removable());
            }
        }

        /// A streaming receiver whose mDNS record expires must not be pulled
        /// out from under the session: `session.active` still contains it, so
        /// an `Unavailable` row would make the two views contradict.
        #[test]
        fn losing_the_service_while_streaming_defers_until_the_generation_ends() {
            let mut machine = ReceiverMachine::new(true);
            machine.begin_generation(1, 0).expect("start");
            machine
                .enter_streaming(ReceiverRole::Single)
                .expect("streaming");
            machine
                .set_service_available(false)
                .expect("the loss is recorded, not applied");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Streaming {
                    role: ReceiverRole::Single
                },
                "the generation still owns this receiver"
            );
            assert!(
                !machine.service_available(),
                "the fact itself is remembered"
            );

            machine.leave_session();
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Unavailable,
                "the deferred loss lands at the generation boundary"
            );
        }

        /// Discovery and session events race on one queue; whatever the row
        /// drifted to, the supervisor really is (re)connecting this receiver.
        #[test]
        fn a_generation_boundary_reanchors_a_drifted_receiver() {
            let mut unavailable = ReceiverMachine::new(false);
            assert_eq!(unavailable.state(), &ReceiverLifecycle::Unavailable);
            unavailable
                .begin_generation(1, 0)
                .expect("the session is authoritative");
            assert_eq!(
                unavailable.state(),
                &ReceiverLifecycle::Connecting { attempt: 0 }
            );

            let mut failed = ReceiverMachine::new(true);
            failed.begin_generation(1, 0).expect("start");
            failed
                .fail(false, UserFacingError::new("pairing rejected"))
                .expect("failure");
            failed
                .begin_generation(2, 0)
                .expect("the next generation re-anchors it");
            assert_eq!(
                failed.state(),
                &ReceiverLifecycle::Connecting { attempt: 0 }
            );

            let mut member = ReceiverMachine::new(false);
            member
                .enter_streaming(ReceiverRole::Single)
                .expect("the transport reported membership");
            assert_eq!(
                member.state(),
                &ReceiverLifecycle::Streaming {
                    role: ReceiverRole::Single
                }
            );
        }

        /// A non-retryable failure keeps the receiver out of automatic
        /// reconciles, but the retry button must still free it.
        #[test]
        fn manual_retry_frees_a_terminally_failed_receiver() {
            let mut machine = ReceiverMachine::new(true);
            machine.begin_generation(1, 0).expect("start");
            machine
                .fail(false, UserFacingError::new("pairing rejected"))
                .expect("failure");
            assert!(
                machine.in_backoff(),
                "a terminal failure stays out of automatic reconciles"
            );

            machine.retry_now().expect("the retry command frees it");
            assert_eq!(
                machine.state(),
                &ReceiverLifecycle::Connecting { attempt: 0 }
            );
            assert!(!machine.in_backoff(), "and it is reconcilable again");
        }

        /// Leaving the desired set drops the backoff too: nobody is going to
        /// connect this receiver any more, so a pending deadline is a lie.
        #[test]
        fn releasing_an_undesired_receiver_clears_its_backoff() {
            let mut machine = ReceiverMachine::new(true);
            machine.begin_generation(1, 0).expect("start");
            machine
                .fail(true, UserFacingError::new("gone"))
                .expect("failure");
            machine.wait_retry(1, wall(5)).expect("backoff");

            machine.release();
            assert_eq!(machine.state(), &ReceiverLifecycle::Discovered);
            assert_eq!(machine.next_retry(), None);
            assert!(!machine.in_backoff());
        }

        #[test]
        fn manual_retry_on_an_idle_receiver_is_a_no_op() {
            let mut machine = ReceiverMachine::new(true);
            machine.retry_now().expect("nothing was scheduled");
            assert_eq!(machine.state(), &ReceiverLifecycle::Discovered);
        }

        /// Rejected edges must break the build's tests, not degrade silently.
        #[cfg(debug_assertions)]
        #[test]
        #[should_panic(expected = "invalid receiver transition")]
        fn a_rejected_edge_fails_debug_builds() {
            let mut machine = ReceiverMachine::new(true);
            // Discovered -> Ready skips Connecting and SettingUp entirely.
            let _ = machine.transition(ReceiverLifecycle::Ready {
                role: ReceiverRole::Single,
            });
        }

        /// The same rejection must be reportable rather than panicking once
        /// debug assertions are compiled out.
        #[cfg(not(debug_assertions))]
        #[test]
        fn a_rejected_edge_is_reported_in_release_builds() {
            let mut machine = ReceiverMachine::new(true);
            let rejected = machine
                .transition(ReceiverLifecycle::Ready {
                    role: ReceiverRole::Single,
                })
                .expect_err("the edge is not permitted");
            assert_eq!(rejected.from, "Discovered");
            assert_eq!(rejected.to, "Ready");
            assert_eq!(machine.state(), &ReceiverLifecycle::Discovered);
        }

        /// Ending a generation returns live receivers to `Discovered` and
        /// leaves waiting or unavailable ones untouched.
        #[test]
        fn leaving_a_session_only_releases_live_receivers() {
            let mut streaming = ReceiverMachine::new(true);
            streaming.begin_generation(1, 0).expect("start");
            streaming
                .enter_streaming(ReceiverRole::Single)
                .expect("streaming");
            streaming.leave_session();
            assert_eq!(streaming.state(), &ReceiverLifecycle::Discovered);

            let mut waiting = ReceiverMachine::new(true);
            waiting.begin_generation(1, 0).expect("start");
            waiting
                .fail(true, UserFacingError::new("gone"))
                .expect("failure");
            waiting.wait_retry(1, wall(5)).expect("backoff");
            waiting.leave_session();
            assert!(
                matches!(waiting.state(), ReceiverLifecycle::RetryWaiting { .. }),
                "a receiver serving its backoff keeps waiting"
            );
        }
    }

    mod policy {
        use super::*;

        #[test]
        fn defaults_match_the_specified_baseline() {
            let policy = RetryPolicy::default();
            assert_eq!(policy.base, [1, 2, 4, 8, 16, 30].map(Duration::from_secs));
            assert_eq!(policy.jitter_percent, 20);
            assert_eq!(policy.healthy_reset_after, Duration::from_secs(60));
            assert_eq!(policy.discovery_stable_for, Duration::from_secs(2));
        }
    }
}
