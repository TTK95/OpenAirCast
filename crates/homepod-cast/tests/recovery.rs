//! Integration tests for the deterministic recovery policy
//! (`backend::recovery`): backoff with stable jitter, healthy-streaming
//! reset, manual retry, and rejoin-restart rate limiting.
//!
//! Every test drives an injected fake `Clock`; no test sleeps.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use homepod_cast::backend::model::ReceiverId;
use homepod_cast::backend::recovery::{
    Clock, RejoinRestartLimiter, RetryPolicy, RetryState, SystemClock,
};

/// Deterministic receiver identity for the given seed byte.
fn receiver_id(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}")).expect("valid storage key")
}

/// Manually advanced monotonic clock shared through an `Arc`.
#[derive(Clone)]
struct FakeClock {
    current: Arc<Mutex<Instant>>,
}

impl FakeClock {
    fn at_zero() -> Self {
        Self {
            current: Arc::new(Mutex::new(Instant::now())),
        }
    }

    fn advance(&self, by: Duration) {
        *self.current.lock().unwrap() += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.current.lock().unwrap()
    }
}

#[test]
fn retry_schedule_caps_at_thirty_seconds_and_is_deterministic() {
    let policy = RetryPolicy::default();
    let id = receiver_id(7);
    let first = (0..8)
        .map(|attempt| policy.delay(&id, attempt))
        .collect::<Vec<_>>();
    let second = (0..8)
        .map(|attempt| policy.delay(&id, attempt))
        .collect::<Vec<_>>();
    assert_eq!(first, second, "same inputs must always produce same output");
    assert!(first[0] >= Duration::from_millis(800) && first[0] <= Duration::from_millis(1_200));
    assert!(first[7] >= Duration::from_secs(24) && first[7] <= Duration::from_secs(36));
    assert!(
        first.iter().all(|delay| *delay <= Duration::from_secs(30)),
        "no delay may exceed the 30 s cap"
    );
}

#[test]
fn jitter_within_twenty_percent_and_stable_for_same_inputs() {
    let policy = RetryPolicy::default();
    for seed in 0_u8..12 {
        let receiver = receiver_id(seed);
        for attempt in 0_u32..6 {
            let base = policy.base[usize::min(attempt as usize, policy.base.len() - 1)];
            let low = base.mul_f64(0.8);
            let high = base.mul_f64(1.2);

            let delay = policy.delay(&receiver, attempt);
            assert!(
                delay >= low && delay <= high,
                "seed {seed} attempt {attempt}"
            );
            assert_eq!(
                delay,
                policy.delay(&receiver, attempt),
                "repeat call must be identical"
            );

            let discovery = policy.discovery_delay(seed as u64, attempt);
            assert!(
                discovery >= low && discovery <= high,
                "discovery seed {seed}"
            );
            assert_eq!(
                discovery,
                policy.discovery_delay(seed as u64, attempt),
                "repeat discovery call must be identical"
            );
        }
    }
}

#[test]
fn streaming_reset_requires_sixty_seconds_healthy() {
    let clock = FakeClock::at_zero();
    let policy = RetryPolicy::default();
    let receiver = receiver_id(3);
    let mut state = RetryState::new();

    state.note_failure(&policy, clock.now(), receiver);
    clock.advance(Duration::from_secs(1));
    state.note_failure(&policy, clock.now(), receiver);
    assert_eq!(state.attempt(receiver), 2);
    assert!(state.next_retry(receiver).is_some());

    // Streaming begins but has not yet been continuous long enough.
    state.note_streaming(&policy, clock.now(), receiver);
    clock.advance(Duration::from_millis(59_999));
    assert!(
        !state.note_streaming(&policy, clock.now(), receiver),
        "reset must not happen before sixty seconds"
    );
    assert_eq!(state.attempt(receiver), 2);
    assert!(state.next_retry(receiver).is_some());

    clock.advance(Duration::from_millis(1));
    assert!(state.note_streaming(&policy, clock.now(), receiver));
    assert_eq!(state.attempt(receiver), 0);
    assert_eq!(state.next_retry(receiver), None);
}

/// The controller evaluates the healthy window at failure time (the span
/// between a receiver's `Active` edge and its failure is exactly the interval
/// during which every probe answered), so the ladder must restart at one after
/// a long healthy run and continue climbing after a short one.
#[test]
fn a_long_healthy_run_puts_the_next_failure_back_on_the_first_rung() {
    let policy = RetryPolicy::default();
    let receiver = receiver_id(6);

    let short = FakeClock::at_zero();
    let mut climbing = RetryState::new();
    climbing.note_failure(&policy, short.now(), receiver);
    climbing.note_failure(&policy, short.now(), receiver);
    assert_eq!(climbing.attempt(receiver), 2);
    climbing.note_streaming(&policy, short.now(), receiver);
    short.advance(Duration::from_secs(59));
    // Controller order at failure time: evaluate health, then record failure.
    climbing.note_streaming(&policy, short.now(), receiver);
    climbing.note_failure(&policy, short.now(), receiver);
    assert_eq!(
        climbing.attempt(receiver),
        3,
        "a short healthy run keeps climbing the ladder"
    );

    let long = FakeClock::at_zero();
    let mut reset = RetryState::new();
    reset.note_failure(&policy, long.now(), receiver);
    reset.note_failure(&policy, long.now(), receiver);
    reset.note_streaming(&policy, long.now(), receiver);
    long.advance(Duration::from_secs(60));
    reset.note_streaming(&policy, long.now(), receiver);
    let deadline = reset.note_failure(&policy, long.now(), receiver);
    assert_eq!(
        reset.attempt(receiver),
        1,
        "sixty continuously healthy seconds reset the attempt counter"
    );
    let first_rung = policy.base[0];
    assert!(deadline - long.now() >= first_rung.mul_f64(0.8));
    assert!(deadline - long.now() <= first_rung.mul_f64(1.2));
}

#[test]
fn manual_retry_clears_deadline_only() {
    let clock = FakeClock::at_zero();
    let policy = RetryPolicy::default();
    let receiver = receiver_id(5);
    let mut state = RetryState::new();

    state.note_failure(&policy, clock.now(), receiver);
    assert!(state.next_retry(receiver).is_some());
    assert_eq!(state.attempt(receiver), 1);

    state.manual_retry(receiver);
    assert_eq!(
        state.next_retry(receiver),
        None,
        "manual retry clears the pending deadline"
    );
    assert_eq!(state.attempt(receiver), 1, "history is preserved");

    // The next failure continues from history instead of restarting at one.
    clock.advance(Duration::from_secs(10));
    let deadline = state.note_failure(&policy, clock.now(), receiver);
    assert_eq!(state.attempt(receiver), 2);
    let expected_base = policy.base[1];
    assert!(deadline > clock.now());
    assert!(deadline - clock.now() >= expected_base.mul_f64(0.8));
    assert!(deadline - clock.now() <= expected_base.mul_f64(1.2));
}

#[test]
fn rejoin_limiter_enforces_two_second_online_and_thirty_second_spacing() {
    let clock = FakeClock::at_zero();
    let mut limiter = RejoinRestartLimiter::new(Arc::new(clock.clone()));
    limiter.note_discovered(receiver_id(2));

    clock.advance(Duration::from_millis(1_999));
    assert!(!limiter.may_rejoin(receiver_id(2)));
    clock.advance(Duration::from_millis(1));
    assert!(limiter.may_rejoin(receiver_id(2)));

    limiter.note_restart();
    assert!(
        !limiter.may_restart_group(),
        "a flapping receiver cannot repeatedly restart the group"
    );
    clock.advance(Duration::from_secs(30));
    assert!(limiter.may_restart_group());
}

#[test]
fn rejoin_limiter_requires_continuous_discoverability() {
    let clock = FakeClock::at_zero();
    let mut limiter = RejoinRestartLimiter::new(Arc::new(clock.clone()));
    let receiver = receiver_id(4);

    limiter.note_discovered(receiver);
    clock.advance(Duration::from_millis(1_500));
    limiter.note_lost(receiver);
    limiter.note_discovered(receiver);
    clock.advance(Duration::from_millis(1_500));
    assert!(!limiter.may_rejoin(receiver), "stability window restarted");
    clock.advance(Duration::from_millis(500));
    assert!(limiter.may_rejoin(receiver));

    // Unknown receivers are never eligible.
    assert!(!limiter.may_rejoin(receiver_id(9)));
}

#[test]
fn system_clock_reports_current_instant() {
    let before = Instant::now();
    let observed = SystemClock.now();
    let after = Instant::now();
    assert!(observed >= before && observed <= after);
}
