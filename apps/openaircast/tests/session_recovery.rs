//! Session recovery integration tests (Task 14).
//!
//! Scripted fakes live HERE, deliberately not in `tests/support/mod.rs`:
//! that shared file is another work stream's territory and these session
//! fakes are only meaningful to this suite (documented placement deviation).

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airplay_audio::LiveAudioDecoder;
use airplay_client::{
    ClientDiagnosticsSource, GroupRuntimeEvent, Health, MemberFailure, MemberResult,
};
use airplay_core::error::RtspError;
use airplay_core::{Device, Error as CoreError};
use homepod_cast::backend::model::{LatencyConfig, ReceiverId, RestartReason, UserFacingError};
use homepod_cast::backend::session::{
    SessionDecoderSource, SessionRequest, SessionSupervisor, SessionSupervisorHandle,
    SessionTransport, SessionTransportFactory, SessionUpdate,
};
use homepod_cast::SessionDiagnosticsState;
use tokio::sync::watch;

#[path = "support/backend_rig.rs"]
mod backend_rig;

use backend_rig::{device_id, reconcile, rid, set_of, LeakCounter, RecoveryRig, TestClock};
use homepod_cast::backend::command::{BackendCommand, SystemEvent};
use homepod_cast::backend::model::ReceiverLifecycle;

/// Phase blueprint materialized into fresh `MemberFailure`s per call
/// (`MemberFailure` is not `Clone`, mirroring the upstream contract).
#[derive(Clone, Copy, Debug)]
struct FailureBlueprint {
    seed: u8,
    phase: airplay_client::SetupPhase,
}

impl FailureBlueprint {
    fn materialize(self) -> MemberFailure {
        MemberFailure {
            receiver: device_id(self.seed),
            phase: self.phase,
            retryable: matches!(
                self.phase,
                airplay_client::SetupPhase::Connect
                    | airplay_client::SetupPhase::PrimaryTiming
                    | airplay_client::SetupPhase::SetPeers
                    | airplay_client::SetupPhase::StartAudio
            ),
            source: CoreError::Rtsp(RtspError::SetupFailed("scripted failure".into())),
        }
    }
}

/// One scripted outcome of `SessionTransportFactory::start`.
#[derive(Clone)]
struct ScriptedSession {
    primary_seed: Option<u8>,
    member_seeds: Vec<u8>,
    failures: Vec<FailureBlueprint>,
    events: VecDeque<GroupRuntimeEvent>,
}

/// Scripted transport: records stops/volumes, replays queued runtime events,
/// and counts its own drop through the shared [`LeakCounter`].
struct ScriptedTransport {
    primary_seed: Option<u8>,
    member_seeds: Vec<u8>,
    failures: Vec<FailureBlueprint>,
    events: Mutex<VecDeque<GroupRuntimeEvent>>,
    leaks: LeakCounter,
}

impl Drop for ScriptedTransport {
    fn drop(&mut self) {
        self.leaks.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl SessionTransport for ScriptedTransport {
    fn diagnostics_source(&self) -> Option<ClientDiagnosticsSource> {
        let source = ClientDiagnosticsSource::test_new_empty();
        for seed in &self.member_seeds {
            source.test_register_connection(device_id(*seed), None);
        }
        Some(source)
    }

    fn primary(&self) -> Option<ReceiverId> {
        self.primary_seed.map(rid)
    }

    fn active_members(&self) -> BTreeSet<ReceiverId> {
        self.member_seeds.iter().map(|seed| rid(*seed)).collect()
    }

    fn setup_failures(&mut self) -> Vec<MemberFailure> {
        self.failures
            .iter()
            .map(|blueprint| blueprint.materialize())
            .collect()
    }

    fn try_runtime_event(
        &mut self,
    ) -> Result<Option<GroupRuntimeEvent>, homepod_cast::backend::session::RuntimeEventLag> {
        Ok(self
            .events
            .lock()
            .expect("events mutex poisoned")
            .pop_front())
    }

    async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
        self.member_seeds
            .iter()
            .map(|seed| MemberResult {
                receiver: device_id(*seed),
                result: Ok(Health::Healthy),
            })
            .collect()
    }

    async fn set_member_volume(&mut self, receiver: &ReceiverId, _volume: f32) -> MemberResult<()> {
        MemberResult {
            receiver: (*receiver).into(),
            result: Ok(()),
        }
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Scripted factory: records every start's desired set, parks until released
/// when armed, then pops the next scripted session.
struct ScriptedFactory {
    inner: Arc<ScriptedFactoryInner>,
}

struct ScriptedFactoryInner {
    script: Mutex<VecDeque<ScriptedSession>>,
    started_sets: Mutex<Vec<BTreeSet<ReceiverId>>>,
    release_rx: watch::Receiver<bool>,
    leaks: LeakCounter,
}

impl ScriptedFactory {
    fn started_sets(&self) -> Vec<BTreeSet<ReceiverId>> {
        self.inner
            .started_sets
            .lock()
            .expect("started-sets mutex poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl SessionTransportFactory for ScriptedFactory {
    async fn start(
        &self,
        desired: Vec<Device>,
        _preferred_primary: Option<ReceiverId>,
        _decoder: LiveAudioDecoder,
        _latency: LatencyConfig,
        _calibration: homepod_cast::calibration::CalibrationProfile,
    ) -> Result<Box<dyn SessionTransport>, homepod_cast::backend::session::SessionStartFailure>
    {
        let started: BTreeSet<ReceiverId> = desired
            .iter()
            .map(|device| ReceiverId::from(device.id.clone()))
            .collect();
        self.inner
            .started_sets
            .lock()
            .expect("started-sets mutex poisoned")
            .push(started);

        let mut release = self.inner.release_rx.clone();
        while !*release.borrow_and_update() {
            release.changed().await.map_err(|_| {
                homepod_cast::backend::session::SessionStartFailure {
                    error: UserFacingError::new("factory release channel closed"),
                    failures: Vec::new(),
                }
            })?;
        }

        let next = self
            .inner
            .script
            .lock()
            .expect("script mutex poisoned")
            .pop_front()
            .expect("scripted session available");
        self.inner.leaks.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ScriptedTransport {
            primary_seed: next.primary_seed,
            member_seeds: next.member_seeds,
            failures: next.failures,
            events: Mutex::new(next.events),
            leaks: self.inner.leaks.clone(),
        }))
    }
}

/// Decoder source handing out throwaway pairs; the capture seam's stand-in.
struct PairDecoderSource;

impl SessionDecoderSource for PairDecoderSource {
    fn take_decoder(&self) -> LiveAudioDecoder {
        LiveAudioDecoder::create_pair(44_100, 2, 32).1
    }
}

/// Test harness wiring factory + decoder source + supervisor together.
struct Rig {
    supervisor: SessionSupervisor,
    handle: SessionSupervisorHandle,
    scripted: Arc<ScriptedFactory>,
    release_tx: watch::Sender<bool>,
    leaks: LeakCounter,
    diagnostics: watch::Receiver<SessionDiagnosticsState>,
}

impl Rig {
    /// Starts a supervisor whose scripted sessions are handed out in order;
    /// when `parked`, every start blocks until [`Self::release_start`].
    fn start(sessions: Vec<ScriptedSession>, parked: bool) -> Self {
        let leaks = LeakCounter::default();
        let (release_tx, release_rx) = watch::channel(!parked);
        let scripted = Arc::new(ScriptedFactory {
            inner: Arc::new(ScriptedFactoryInner {
                script: Mutex::new(sessions.into()),
                started_sets: Mutex::new(Vec::new()),
                release_rx,
                leaks: leaks.clone(),
            }),
        });
        let factory: Arc<dyn SessionTransportFactory> = scripted.clone();
        let (diagnostics_tx, diagnostics) = watch::channel(SessionDiagnosticsState::default());
        let supervisor = SessionSupervisor::start_with_diagnostics(
            factory,
            Arc::new(PairDecoderSource),
            diagnostics_tx,
        );
        let handle = supervisor.handle();
        Self {
            supervisor,
            handle,
            scripted,
            release_tx,
            leaks,
            diagnostics,
        }
    }

    /// Releases every parked start; the newest generation's outcome wins.
    async fn release_start(&self) {
        let _ = self.release_tx.send(true);
    }

    fn factory_started_sets(&self) -> Vec<BTreeSet<ReceiverId>> {
        self.scripted.started_sets()
    }

    async fn next_update(&mut self) -> SessionUpdate {
        tokio::time::timeout(Duration::from_secs(2), self.supervisor.updates_mut().recv())
            .await
            .expect("update arrives within the test budget")
            .expect("supervisor update channel stays open")
    }

    async fn next_diagnostics(&mut self) -> SessionDiagnosticsState {
        self.diagnostics
            .changed()
            .await
            .expect("diagnostics state publisher stays open");
        self.diagnostics.borrow_and_update().clone()
    }

    /// Cancels supervision, awaits the bounded teardown, and returns the
    /// final truthful leak count.
    async fn shutdown(self) -> usize {
        let leaks = self.leaks.clone();
        self.supervisor.shutdown().await;
        leaks.leaked()
    }
}

#[tokio::test]
async fn diagnostics_state_tracks_single_group_stop_and_rejects_stale_generation() {
    let mut rig = Rig::start(vec![session(1, &[1]), session(1, &[1, 2])], false);

    rig.handle
        .try_send(reconcile(3, &[1]))
        .expect("single generation accepted");
    let _ = rig.next_update().await;
    let _ = rig.next_update().await;
    match rig.next_diagnostics().await {
        SessionDiagnosticsState::Active {
            generation,
            primary,
            members,
            source,
            ..
        } => {
            assert_eq!(generation, 3);
            assert_eq!(primary, None, "single sessions use the NTP path");
            assert_eq!(members.len(), 1);
            assert_eq!(source.snapshot(0).connections.len(), 1);
        }
        state => panic!("single session did not publish an active source: {state:?}"),
    }

    rig.handle
        .try_send(reconcile(4, &[1, 2]))
        .expect("group generation accepted");
    let _ = rig.next_update().await;
    let _ = rig.next_update().await;
    match rig.next_diagnostics().await {
        SessionDiagnosticsState::Active {
            generation,
            primary,
            members,
            source,
            ..
        } => {
            assert_eq!(generation, 4);
            assert_eq!(primary, Some(rid(1)));
            assert_eq!(members.len(), 2);
            assert_eq!(source.snapshot(0).connections.len(), 2);
        }
        state => panic!("group session did not publish an active source: {state:?}"),
    }

    rig.handle
        .try_send(reconcile(3, &[1]))
        .expect("stale request fits the queue");
    tokio::task::yield_now().await;
    assert!(matches!(
        &*rig.diagnostics.borrow(),
        SessionDiagnosticsState::Active { generation: 4, .. }
    ));

    rig.handle
        .try_send(SessionRequest::Suspend)
        .expect("stop request accepted");
    let _ = rig.next_update().await;
    assert!(matches!(
        rig.next_diagnostics().await,
        SessionDiagnosticsState::Inactive { generation: 4, .. }
    ));

    assert_eq!(rig.shutdown().await, 0);
}

/// A scripted session whose primary is its first member by default.
fn session(primary_seed: u8, member_seeds: &[u8]) -> ScriptedSession {
    ScriptedSession {
        primary_seed: Some(primary_seed),
        member_seeds: member_seeds.to_vec(),
        failures: Vec::new(),
        events: VecDeque::new(),
    }
}

#[tokio::test]
async fn rapid_generations_start_only_the_latest_membership() {
    let mut rig = Rig::start(vec![session(3, &[3])], true);
    let handle = rig.handle.clone();

    // Three rapid reconciles; producers are non-blocking try_sends.
    handle.try_send(reconcile(4, &[1])).expect("queue accepts");
    handle
        .try_send(reconcile(5, &[1, 2]))
        .expect("queue accepts");
    handle.try_send(reconcile(6, &[3])).expect("queue accepts");

    rig.release_start().await;

    let starting = rig.next_update().await;
    assert!(
        matches!(starting, SessionUpdate::Starting { generation: 6 }),
        "expected Starting {{ generation: 6 }}, got {starting:?}"
    );
    let active = rig.next_update().await;
    match active {
        SessionUpdate::Active {
            generation,
            primary,
            members,
            partial_failures,
        } => {
            assert_eq!(generation, 6);
            assert_eq!(primary, rid(3));
            assert_eq!(members, set_of(&[3]));
            assert!(partial_failures.is_empty());
        }
        other => panic!("expected Active for generation 6, got {other:?}"),
    }

    // Only generation six reached the factory; four and five lost.
    assert_eq!(rig.factory_started_sets(), vec![set_of(&[3])]);

    let leaked = rig.shutdown().await;
    assert_eq!(
        leaked, 0,
        "every scripted transport must be dropped after shutdown"
    );
}

#[tokio::test]
async fn suspend_resume_flow_emits_expected_updates() {
    let mut rig = Rig::start(vec![session(1, &[1, 2]), session(1, &[1, 2])], false);
    let handle = rig.handle.clone();

    handle
        .try_send(reconcile(1, &[1, 2]))
        .expect("queue accepts");
    let starting = rig.next_update().await;
    assert!(matches!(
        starting,
        SessionUpdate::Starting { generation: 1 }
    ));
    let active = rig.next_update().await;
    match &active {
        SessionUpdate::Active {
            generation,
            primary,
            members,
            ..
        } => {
            assert_eq!(*generation, 1);
            assert_eq!(*primary, rid(1));
            assert_eq!(*members, set_of(&[1, 2]));
        }
        other => panic!("expected Active for generation 1, got {other:?}"),
    }

    // Inline volume change: must not produce an edge nor bump the generation.
    handle
        .try_send(SessionRequest::SetVolume {
            receiver: rid(2),
            volume: 0.5,
        })
        .expect("queue accepts");

    handle
        .try_send(SessionRequest::Suspend)
        .expect("queue accepts");
    let stopped = rig.next_update().await;
    assert!(
        matches!(stopped, SessionUpdate::Stopped { generation: 1 }),
        "expected Stopped {{ generation: 1 }} right after Active+volume, got {stopped:?}"
    );

    handle
        .try_send(SessionRequest::Resume)
        .expect("queue accepts");
    let recovering = rig.next_update().await;
    match &recovering {
        SessionUpdate::Recovering {
            generation,
            reason,
            attempt,
        } => {
            assert_eq!(*generation, 1);
            assert_eq!(*reason, RestartReason::SystemResume);
            assert_eq!(*attempt, 0);
        }
        other => panic!("expected Recovering(SystemResume), got {other:?}"),
    }
    let restarting = rig.next_update().await;
    assert!(matches!(
        restarting,
        SessionUpdate::Starting { generation: 2 }
    ));
    let reactivated = rig.next_update().await;
    match &reactivated {
        SessionUpdate::Active { generation, .. } => assert_eq!(*generation, 2),
        other => panic!("expected Active for generation 2, got {other:?}"),
    }

    // Both generations ran against the identical retained desired set.
    let expected = set_of(&[1, 2]);
    assert_eq!(rig.factory_started_sets(), vec![expected.clone(), expected]);

    let leaked = rig.shutdown().await;
    assert_eq!(leaked, 0);
}

// ---------------------------------------------------------------------------
// Task 16 Steps 1-4: receiver health, failure isolation, and retry policy.
//
// These tests observe the WHOLE backend (controller + session supervisor +
// discovery and capture seams), because the two properties under test live on
// opposite sides of that boundary: `ReceiverLifecycle` rows are published by
// the controller snapshot, while probe failures originate inside the session
// supervisor's transport. The `Rig` above deliberately stays at supervisor
// level; `RecoveryRig` below is its full-stack twin.
//
// FAKE PLACEMENT NOTE: same rule as `tests/backend_lifecycle.rs` -- the shared
// `tests/support/mod.rs` implements crate-private seams and therefore cannot
// compile inside an integration-test binary, so the doubles below are local.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secondary_failure_does_not_restart_healthy_members() {
    let rig = RecoveryRig::active(&[1, 2, 3], 1).await;
    rig.fail_probe(2);

    // Backoff entry is the synchronization point: only after the failed
    // secondary reached it is the absence of a restart meaningful.
    rig.wait_for(|snapshot| {
        snapshot.receivers.iter().any(|row| {
            row.id == rid(2) && matches!(row.lifecycle, ReceiverLifecycle::RetryWaiting { .. })
        })
    })
    .await;

    assert_eq!(rig.active_members(), set_of(&[1, 3]));
    assert_eq!(rig.full_restart_count(), 0);
    assert!(matches!(
        rig.receiver_state(2),
        ReceiverLifecycle::RetryWaiting { .. }
    ));
    assert_eq!(rig.primary(), Some(rid(1)), "the primary keeps streaming");

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

#[tokio::test]
async fn primary_failure_restarts_the_survivors_once() {
    let rig = RecoveryRig::active(&[1, 2, 3], 1).await;
    rig.fail_probe(1);

    rig.wait_for(|snapshot| snapshot.session.primary == Some(rid(2)))
        .await;

    assert_eq!(rig.full_restart_count(), 1);
    assert_eq!(rig.primary(), Some(rid(2)));
    assert_eq!(rig.active_members(), set_of(&[2, 3]));

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// An mDNS record expiring under a streaming receiver is a discovery fact.
/// The session keeps streaming to it, so the row must keep saying so --
/// `session.active` and `receivers[..].lifecycle` are two views of one truth.
#[tokio::test]
async fn an_expired_service_never_contradicts_a_streaming_member() {
    let rig = RecoveryRig::active(&[1, 2], 1).await;

    rig.remove_service(2);
    // Discovery events arrive in order on one channel, so the appearance of a
    // later addition proves the removal above was already processed.
    rig.add_service(3);
    rig.wait_for(|snapshot| snapshot.receivers.iter().any(|row| row.id == rid(3)))
        .await;

    assert!(
        rig.active_members().contains(&rid(2)),
        "the session still streams to the receiver"
    );
    assert!(
        matches!(rig.receiver_state(2), ReceiverLifecycle::Streaming { .. }),
        "its row must agree; got {:?}",
        rig.receiver_state(2)
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The retry ladder is only policy once something consumes its deadlines: a
/// receiver that recovers must rejoin without the user pressing anything.
#[tokio::test]
async fn a_healed_secondary_rejoins_without_user_input() {
    let rig = RecoveryRig::active(&[1, 2], 1).await;
    rig.fail_probe(2);

    rig.wait_for(|snapshot| {
        snapshot.receivers.iter().any(|row| {
            row.id == rid(2) && matches!(row.lifecycle, ReceiverLifecycle::RetryWaiting { .. })
        })
    })
    .await;
    rig.heal_probe(2);

    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Dropping a receiver from the desired set closes its generation boundary
/// too; leaving the row on `Streaming` would contradict `session.active`.
#[tokio::test]
async fn removing_a_member_while_streaming_clears_its_row() {
    let rig = RecoveryRig::active(&[1, 2], 1).await;

    rig.send(BackendCommand::SetDesiredMembers {
        members: set_of(&[1]),
    })
    .await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1]))
        .await;

    assert!(
        !matches!(rig.receiver_state(2), ReceiverLifecycle::Streaming { .. }),
        "the removed receiver still claims to stream: {:?}",
        rig.receiver_state(2)
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Suspend/resume mints a supervisor-side generation. If the controller does
/// not adopt it, its next reconcile re-sends an already-accepted number and
/// the supervisor's strictly-newer gate drops it without a word -- every
/// reconcile after a resume would then be silently ignored.
#[tokio::test]
async fn a_reconcile_after_resume_still_reaches_the_supervisor() {
    let rig = RecoveryRig::active(&[1, 2, 3], 1).await;

    rig.send(BackendCommand::NotifySystem(SystemEvent::Suspending))
        .await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;
    rig.send(BackendCommand::NotifySystem(SystemEvent::Resumed))
        .await;
    rig.wait_for(|snapshot| {
        snapshot.session.active == set_of(&[1, 2, 3]) && snapshot.session.primary == Some(rid(1))
    })
    .await;

    // Membership is the cleanest probe of the gate: no failure, no backoff,
    // nothing else that could restart the group on its own.
    rig.send(BackendCommand::SetDesiredMembers {
        members: set_of(&[1, 2]),
    })
    .await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A probe cycle that outruns the probe interval must not make the probe arm
/// permanently ready; under `biased` that would starve every request the
/// controller sends and freeze the UI.
#[tokio::test]
async fn a_slow_probe_cycle_does_not_starve_session_requests() {
    let rig = RecoveryRig::active(&[1, 2], 1).await;
    // Three times the injected twenty-millisecond cadence.
    rig.script.set_probe_delay(Duration::from_millis(60));
    // The delay only bites from the next cycle on, so the request below must
    // be sent once the slow cadence is genuinely running -- otherwise it could
    // slip through the last fast interval and prove nothing.
    rig.wait_for_probes(2).await;

    rig.send(BackendCommand::SetDesiredMembers {
        members: set_of(&[1]),
    })
    .await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1]))
        .await;

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

// ---------------------------------------------------------------------------
// Task 16 Steps 5-7: rejoin through one rate-limited full restart, primary
// loss, and failure storms. Every timing decision runs on the injected fake
// clock, so the two-second and thirty-second windows are stated by the test.
// ---------------------------------------------------------------------------

/// Step 5. A receiver whose mDNS record expired must not be dragged back into
/// the group the moment its backoff deadline passes -- and it must not be
/// stranded either. Only two continuous seconds of castable discovery release
/// it, through exactly one announced rejoin restart.
#[tokio::test]
async fn a_rediscovered_receiver_rejoins_after_two_stable_seconds() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2], 1, clock).await;

    rig.fail_secondaries(&[2]).await;
    rig.remove_service(2);
    rig.wait_for_state(2, |state| matches!(state, ReceiverLifecycle::Unavailable))
        .await;

    // A minute past every rung of the ladder: an undiscoverable receiver may
    // not be reconnected however long its backoff has been over.
    rig.advance(Duration::from_secs(60)).await;
    assert_eq!(rig.rejoin_restarts(), 0);
    assert_eq!(rig.active_members(), set_of(&[1]));

    rig.heal_probe(2);
    rig.add_service(2);
    rig.wait_for_state(2, |state| !matches!(state, ReceiverLifecycle::Unavailable))
        .await;

    // One second of castable discovery is not two.
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(rig.rejoin_restarts(), 0);
    assert_eq!(rig.active_members(), set_of(&[1]));

    rig.advance(Duration::from_secs(1)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;
    assert_eq!(rig.rejoin_restarts(), 1);

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Step 6. Losing the primary invalidates group timing, so the survivors are
/// rebuilt at once -- without waiting for a desired member discovery cannot
/// currently reach, and without asking the rejoin spacing for permission. A
/// lone survivor is reconnected as `Single`, which is the controller-side
/// half of the switch back to NTP timing.
#[tokio::test]
async fn primary_loss_rebuilds_survivors_without_waiting_or_spacing() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4], 1, clock).await;

    // Consume the rejoin allowance first: receiver 4 fails, comes back, and
    // rejoins through the one restart the limiter permits.
    rig.fail_secondaries(&[4]).await;
    rig.rediscover_for(4, Duration::from_secs(2)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2, 3, 4]))
        .await;
    assert_eq!(rig.rejoin_restarts(), 1);

    // Receiver 3 drops out and then loses its mDNS record entirely; nothing
    // may wait for a member discovery cannot reach.
    rig.fail_secondaries(&[3]).await;
    rig.remove_service(3);
    rig.wait_for_state(3, |state| matches!(state, ReceiverLifecycle::Unavailable))
        .await;

    // No clock advance anywhere below: the rebuild has to be immediate even
    // though the thirty-second rejoin window is still closed.
    rig.fail_secondaries(&[2]).await;
    rig.fail_primary(1).await;

    assert_eq!(rig.primary_loss_restarts(), 1);
    assert_eq!(rig.rejoin_restarts(), 1, "no rejoin restart may sneak in");
    assert_eq!(rig.primary(), Some(rid(4)), "the lone survivor takes over");
    assert_eq!(rig.active_members(), set_of(&[4]));
    assert_eq!(
        rig.receiver_state(4),
        ReceiverLifecycle::Streaming {
            role: homepod_cast::backend::model::ReceiverRole::Single
        },
        "a lone survivor streams through the single-receiver timing path"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Step 7. The storm: three secondaries drop while the primary keeps
/// streaming, one of them comes back and rejoins through exactly one restart,
/// and then flaps for twenty-nine seconds without earning a second one --
/// while losing the primary is immediate regardless of that window.
///
/// The plan spells this test with `#[tokio::test(start_paused = true)]`. That
/// attribute cannot govern this system: the backend runs on a private thread
/// with its own runtime, which a paused test clock does not reach, while the
/// rig's own polling deadlines would auto-advance into a busy spin. The
/// injected clock below is the same idea applied where the decisions actually
/// happen, and it makes every window exact instead of load-dependent.
#[tokio::test]
async fn rejoin_storm_is_rate_limited_but_primary_loss_is_immediate() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4], 1, clock).await;

    rig.fail_secondaries(&[2, 3, 4]).await;
    assert_eq!(rig.active_members(), set_of(&[1]), "the primary streams on");
    assert_eq!(rig.primary(), Some(rid(1)));
    assert_eq!(rig.rejoin_restarts(), 0);
    assert_eq!(
        rig.full_restart_count(),
        0,
        "no secondary may restart the group"
    );

    rig.rediscover_for(2, Duration::from_secs(2)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;
    assert_eq!(rig.rejoin_restarts(), 1);

    rig.flap_for(2, Duration::from_secs(29)).await;
    assert_eq!(rig.rejoin_restarts(), 1, "the flap must not earn a restart");
    assert_eq!(rig.active_members(), set_of(&[1, 2]));

    rig.fail_primary(1).await;
    assert_eq!(rig.primary_loss_restarts(), 1);
    assert_eq!(rig.rejoin_restarts(), 1);
    assert_eq!(rig.primary(), Some(rid(2)));

    assert_eq!(rig.shutdown().await, 0, "the storm may leak no transport");
}

/// A system suspend leaves `RunIntent` on `Running` -- the user did not stop
/// anything -- while `SessionUpdate::Stopped` returns every streaming machine
/// to `Discovered`. Both halves of the automatic-rejoin predicate are then
/// satisfied for the whole desired set, so without an explicit suspend gate
/// the controller rebuilds the very session the supervisor is tearing down,
/// and keeps doing so every thirty seconds while the machine sleeps.
#[tokio::test]
async fn a_suspended_machine_is_never_woken_by_an_automatic_rejoin() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3], 1, clock).await;

    rig.send(BackendCommand::NotifySystem(SystemEvent::Suspending))
        .await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;

    // A full minute of sleep. Every desired member is discoverable, none is in
    // backoff, and no rejoin restart has ever spent the spacing budget -- the
    // suspend is the only thing that can hold the arm back.
    rig.advance(Duration::from_secs(60)).await;
    assert_eq!(
        rig.rejoin_restarts(),
        0,
        "the sleeping machine was sent back to work"
    );
    assert_eq!(rig.full_restart_count(), 0, "no transport may be started");
    assert!(rig.active_members().is_empty());

    // Resume is what ends it -- but only after the mandated two-second
    // interface settle, which on this rig is fake time the test has to spend.
    // `notify` returns only once the settle deadline is actually registered;
    // advancing before that would place it in the past, where it never fires.
    rig.notify(SystemEvent::Resumed).await;
    rig.advance(Duration::from_secs(2)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2, 3]))
        .await;
    assert_eq!(
        rig.rejoin_restarts(),
        0,
        "the resume rebuild was raced by a rejoin"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Losing the primary rebuilds the group around the complete desired set, so
/// that rebuild *is* the restart the thirty-second spacing counts. If it does
/// not spend the budget, a receiver whose window and backoff rung have both
/// elapsed is still reported as due -- `begin_generation` reaches it only when
/// `Starting` comes back -- and immediately spends a second generation on a
/// group that was just rebuilt.
#[tokio::test]
async fn primary_loss_spends_the_rejoin_spacing_budget() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4], 1, clock).await;

    // Receiver 4 drops out but stays discoverable, so only the spacing gate
    // can hold its rejoin back once the first rung has passed.
    rig.fail_secondaries(&[4]).await;
    rig.heal_probe(4);

    // No clock advance before this: the spacing gate is wide open when the
    // primary falls over.
    rig.fail_primary(1).await;
    assert_eq!(rig.primary_loss_restarts(), 1);

    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        rig.rejoin_restarts(),
        0,
        "the rebuild that just ran was restarted again two seconds later"
    );
    assert!(!rig.active_members().contains(&rid(4)));

    // Not stranded either: once the spacing window is over, the rejoin runs.
    rig.advance(Duration::from_secs(30)).await;
    rig.wait_for(|snapshot| snapshot.session.active.contains(&rid(4)))
        .await;
    assert_eq!(rig.rejoin_restarts(), 1);

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The spacing gate is deliberately about restarts, not about how a receiver
/// came to need one. A receiver that never lost its mDNS record and is merely
/// serving its backoff ladder still restarts the whole group when it returns,
/// so it waits out the same thirty seconds as a rediscovered one. Pinning that
/// here because the coupling is a policy decision, not an accident: dropping
/// it would let a receiver failing on every rung restart the group at 1, 2, 4,
/// 8 and 16 seconds, which is exactly the storm the gate exists to prevent.
#[tokio::test]
async fn a_ladder_retry_waits_out_the_same_thirty_second_spacing() {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4], 1, clock).await;

    // Receiver 2 spends the budget through an ordinary ladder retry; its
    // service was never removed, so this is not the discovery-rejoin route.
    rig.fail_secondaries(&[2]).await;
    rig.heal_probe(2);
    rig.advance(Duration::from_secs(2)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2, 3, 4]))
        .await;
    assert_eq!(rig.rejoin_restarts(), 1);

    // Receiver 3 fails two seconds later and would be back on its first rung
    // one second after that -- but the group was just restarted.
    rig.fail_secondaries(&[3]).await;
    rig.heal_probe(3);
    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        rig.rejoin_restarts(),
        1,
        "a second restart inside the spacing window"
    );
    assert_eq!(rig.active_members(), set_of(&[1, 2, 4]));

    // Thirty seconds after the first restart it is allowed, and it happens.
    rig.advance(Duration::from_secs(28)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2, 3, 4]))
        .await;
    assert_eq!(rig.rejoin_restarts(), 2);

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

// ---------------------------------------------------------------------------
// Task 16 Steps 6-7: determinism across report orders, and bounded work
// under a sustained storm.
// ---------------------------------------------------------------------------

/// Everything one survivor rebuild is allowed to depend on.
///
/// Compared whole rather than field by field: a determinism claim that only
/// checks the successor would be satisfied by a rebuild that reached the same
/// primary through a different set, a different preference, or a different
/// number of restarts.
#[derive(Debug, PartialEq, Eq)]
struct RebuildOutcome {
    primary: Option<ReceiverId>,
    active: BTreeSet<ReceiverId>,
    rejoin_restarts: usize,
    primary_loss_restarts: usize,
    full_restarts: usize,
    /// Every session start the controller asked for, oldest first, as
    /// `(desired in the order it was handed down, preferred primary)`.
    requests: Vec<(Vec<ReceiverId>, Option<ReceiverId>)>,
}

/// Fails `order`'s receivers one at a time -- each one fully reported before
/// the next is touched -- waiting `gap` of fake time between reports, then
/// loses the primary and returns everything the rebuild produced.
async fn rebuild_after(order: &[u8], gap: Duration) -> RebuildOutcome {
    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4, 5, 6], 1, clock).await;

    for seed in order {
        rig.fail_secondaries(&[*seed]).await;
        if !gap.is_zero() {
            rig.advance(gap).await;
        }
    }
    rig.fail_primary(1).await;

    let outcome = RebuildOutcome {
        primary: rig.primary(),
        active: rig.active_members(),
        rejoin_restarts: rig.rejoin_restarts(),
        primary_loss_restarts: rig.primary_loss_restarts(),
        full_restarts: rig.full_restart_count(),
        requests: rig.start_requests(),
    };
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
    outcome
}

/// Every ordering of `items`, built by insertion so the enumeration itself
/// does not favour the sorted order.
fn orderings(items: &[u8]) -> Vec<Vec<u8>> {
    let Some((head, tail)) = items.split_first() else {
        return vec![Vec::new()];
    };
    let mut out = Vec::new();
    for rest in orderings(tail) {
        for slot in 0..=rest.len() {
            let mut candidate = rest.clone();
            candidate.insert(slot, *head);
            out.push(candidate);
        }
    }
    out
}

/// Step 6 through the whole controller. "Deterministic" is a claim about
/// order independence, so one scripted sequence cannot establish it: the
/// same failure set is replayed in all six orders, and half of them also put
/// fake time between the reports. The rebuild -- the set handed down, the
/// preference named with it, the restarts spent, and the successor that comes
/// back -- has to be identical every time.
///
/// The gap stays far below the first backoff rung (one second, minus up to
/// twenty percent of jitter). A larger one would release a rung mid-scenario
/// and change the input rather than its ordering.
///
/// Receivers 2, 3 and 5 fail so that the survivors, 4 and 6, are neither the
/// first nor the last receivers in any of the orders: a rebuild that leaked
/// report order into its choice could not land on 4 by accident.
#[tokio::test]
async fn the_survivor_rebuild_never_depends_on_the_order_failures_arrive() {
    const GAP: Duration = Duration::from_millis(100);

    let mut outcomes: Vec<(Vec<u8>, Duration, RebuildOutcome)> = Vec::new();
    for (index, order) in orderings(&[2, 3, 5]).into_iter().enumerate() {
        let gap = if index % 2 == 0 { Duration::ZERO } else { GAP };
        let outcome = rebuild_after(&order, gap).await;
        outcomes.push((order, gap, outcome));
    }

    let (first_order, _, first) = outcomes.first().expect("three receivers permute six ways");
    assert_eq!(
        first.primary,
        Some(rid(4)),
        "the lowest survivor did not take over"
    );
    assert_eq!(first.active, set_of(&[4, 6]));
    assert_eq!(first.primary_loss_restarts, 1);
    assert_eq!(first.rejoin_restarts, 0);
    assert_eq!(
        first.requests,
        vec![(vec![rid(4), rid(6)], None)],
        "the rebuild asked for something other than the survivors, once, \
         without a preference"
    );

    for (order, gap, outcome) in outcomes.iter().skip(1) {
        assert_eq!(
            outcome, first,
            "failures reported as {order:?} with {gap:?} between them rebuilt \
             differently from {first_order:?}"
        );
    }
}

/// Step 7's other half: a storm must cost bounded work, not work that grows
/// with the number of events in it.
///
/// Four receivers drop, recover at the protocol level, and then spend a
/// minute and a half announcing and withdrawing their mDNS records once a
/// second. That is hundreds of discovery events, every one of which satisfies
/// the retry ladder and the spacing gate -- only the two-second stability
/// requirement stands in the way. Not one of them may buy a restart, and none
/// of the bounded resources may grow.
///
/// When the storm ends, all four become eligible at the same instant, and the
/// rejoin they earn is ONE coordinated restart for the whole batch, not one
/// restart each.
#[tokio::test]
async fn a_flapping_storm_costs_one_restart_and_leaks_nothing() {
    const STORM: Duration = Duration::from_secs(90);
    const FLAPPERS: [u8; 4] = [2, 3, 4, 5];

    let clock = TestClock::new();
    let rig = RecoveryRig::active_on(&[1, 2, 3, 4, 5], 1, clock).await;
    // Both ledgers count entries that are legitimately open while the backend
    // runs, so they only read zero after shutdown -- which consumes the rig.
    let discovery = Arc::clone(&rig.discovery);
    let capture = rig.capture.clone();

    rig.fail_secondaries(&FLAPPERS).await;
    assert_eq!(rig.active_members(), set_of(&[1]), "the primary streams on");
    assert_eq!(rig.full_restart_count(), 0);
    // The capture worker runs on its own thread and is not part of the edge
    // bring-up waits on, so its baseline needs its own edge: without this the
    // count below records whether the thread had got going yet, not whether
    // the storm replaced it.
    rig.settle_capture().await;
    let browses_before = discovery.streams.opened();
    let workers_before = capture.workers_started();

    // Healthy again at the protocol level: from here only the discovery
    // gates decide, which is what makes the flap the thing under test.
    for seed in FLAPPERS {
        rig.heal_probe(seed);
    }
    rig.flap_all_for(&FLAPPERS, STORM).await;

    assert_eq!(
        rig.rejoin_restarts(),
        0,
        "a flapping record bought a restart"
    );
    assert_eq!(rig.full_restart_count(), 0, "the storm started a session");
    assert_eq!(rig.active_members(), set_of(&[1]));
    assert_eq!(rig.primary(), Some(rid(1)));
    // Named for what it can actually observe. `queue_overruns` counts
    // `QueueWatermark` diagnostics, and only the capture and discovery queues
    // publish one -- the storm drives the discovery queue directly, with
    // hundreds of browse events through a bounded channel. The session
    // request queue emits no watermark, so nothing here speaks for it.
    assert_eq!(
        rig.queue_overruns(),
        0,
        "the discovery or capture queue reported a depth above its capacity"
    );
    // The work the storm caused must not scale with the events in it: an
    // expiring service record is news for the inventory, never a reason to
    // re-register the browse or to replace the capture worker.
    assert_eq!(
        discovery.streams.opened(),
        browses_before,
        "the storm re-registered the mDNS browse"
    );
    assert_eq!(
        capture.workers_started(),
        workers_before,
        "the storm replaced the capture worker"
    );

    // The storm ends. All four are eligible together, so they share one
    // restart -- the batching the plan calls "one coordinated full restart
    // against the complete newest desired set".
    rig.advance(Duration::from_secs(2)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2, 3, 4, 5]))
        .await;
    assert_eq!(
        rig.rejoin_restarts(),
        1,
        "four receivers rejoining took more than one restart"
    );
    assert_eq!(rig.full_restart_count(), 1);

    // Losing the primary on top of all that is still immediate and still one
    // restart, and the successor is still the lowest survivor.
    rig.fail_primary(1).await;
    assert_eq!(rig.primary_loss_restarts(), 1);
    assert_eq!(rig.rejoin_restarts(), 1, "a rejoin restart snuck in");
    assert_eq!(rig.primary(), Some(rid(2)));

    assert_eq!(
        rig.queue_overruns(),
        0,
        "the rejoin and the primary loss pushed a watched queue over capacity"
    );

    assert_eq!(rig.shutdown().await, 0, "the storm leaked a transport");
    // Zero is only meaningful once the ledgers have seen traffic: the storm
    // opened a browse registration and ran a capture worker, and both were
    // handed back.
    assert!(
        discovery.streams.opened() > 0,
        "the storm never registered a browse at all"
    );
    assert_eq!(
        discovery.streams.leaked(),
        0,
        "the storm left an mDNS browse registration open"
    );
    assert!(
        capture.workers_started() > 0,
        "the storm never started a capture worker at all"
    );
    assert_eq!(
        capture.leaked_workers(),
        0,
        "the storm left a capture worker thread running"
    );
}

/// Every rig in this file drives a backend whose interface-change seam is a
/// double, never the platform one.
///
/// `BackendOverrides` reads an absent `network`/`local_bindings` pair as "use
/// the platform seam", so a rig that scripts no interface change does not run
/// without a monitor -- it registers the host's `NotifyIpInterfaceChange` and
/// lets the machine's real network activity into the test. What arrives is one
/// `SystemEvent::NetworkChanged`, and the controller answers it by restarting
/// discovery and rebuilding the group around the whole desired set. Both
/// halves are fatal here and both are silent: the restart makes receivers a
/// test is deliberately holding out of the session streaming members again,
/// and a row a generation owns stops following its service record -- so
/// `flap_all_for` and every other helper that waits on `Unavailable` waits for
/// something that can no longer happen, and the test dies sixty seconds later
/// on its liveness deadline, printing a snapshot that says nothing about the
/// condition it was waiting for.
///
/// The assertion is on the double's own subscription count, because that is
/// the only thing that distinguishes the two seams from inside the test: an
/// unsubscribed double would mean the backend went to the platform instead.
#[tokio::test]
async fn no_rig_ever_subscribes_to_the_host_interface_monitor() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    assert!(
        rig.network.is_none(),
        "this rig must be one that scripts no interface change"
    );
    // Not a poll: the monitor starts before the controller consumes its first
    // command, and the rig only returns once a session is streaming, so the
    // subscription has provably happened by now. Waiting for it would turn a
    // regression into a sixty-second timeout instead of an immediate failure.
    assert_eq!(
        rig.network_seam.subscriptions(),
        1,
        "the backend did not subscribe to the rig's double, so it subscribed \
         to the host's interface monitor instead"
    );
    assert_eq!(
        rig.network_seam.cancellations(),
        0,
        "the monitor released the double and may have fallen back to the host"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}
