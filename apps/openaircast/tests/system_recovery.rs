//! System recovery integration tests (Task 17, Steps 1-4 plus the network and
//! suspend/resume half of Step 8).
//!
//! Membership is expressed with the seed-byte convention this workspace's
//! backend suites already use (`set_of(&[1, 2])`) rather than the plan's
//! illustrative `receivers(&["a", "b"])`; the identities are the same fixed
//! `ReceiverId`s either way.
//!
//! Every timing decision under test runs on the injected [`TestClock`] and its
//! matching timer seam. `#[tokio::test(start_paused = true)]` -- which the plan
//! sketches -- cannot govern this backend: the controller owns a private
//! thread with its own runtime, so the test runtime's paused clock never
//! reaches it, and auto-advance turns the rig's polling helpers into spins.

use std::time::Duration;

#[path = "support/backend_rig.rs"]
mod backend_rig;

use backend_rig::{rid, set_of, SystemRig, TestClock};
use homepod_cast::backend::command::{BackendCommand, SystemEvent};
use homepod_cast::backend::controller::{AUTO_CONNECT_WINDOW, SUSPEND_WATCHDOG};
use homepod_cast::backend::model::{
    AudioEndpointPreference, LatencyPreset, PersistenceSnapshot, RestartReason, RunIntent,
    SavedGroup, SavedGroupId, SavedGroupMember, SessionPhase, Volume,
};
use homepod_cast::backend::PersistedStateV1;

/// Interface-settle period the monitor and the resume path both observe.
const SETTLE: Duration = Duration::from_secs(2);

/// Continuous discoverability a receiver owes before the backend treats it as
/// stably castable.
const DISCOVERY_STABLE: Duration = Duration::from_secs(2);

/// Durable state of a previous run that selected `desired` and asked for
/// auto-connect.
fn auto_connect_state(desired: &[u8]) -> PersistedStateV1 {
    PersistedStateV1 {
        last_desired_members: set_of(desired),
        auto_connect: true,
        ..PersistedStateV1::default()
    }
}

/// A resume must rebuild against the newest desired set, not against whatever
/// the session supervisor happened to retain when the machine went to sleep.
/// Anything else silently reconnects receivers the user deselected while the
/// lid was shut.
#[tokio::test]
async fn resume_restarts_discovery_and_reconciles_only_latest_intent() {
    // Receiver 3 is discovered from the start but is not part of the group, so
    // selecting it mid-sleep is a membership change and not a discovery race.
    let rig = SystemRig::active_with_spare(&[1, 2], &[3], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;

    rig.set_desired(&[3]).await;
    rig.notify(SystemEvent::Resumed).await;
    rig.discover_stably(3, SETTLE).await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2]), set_of(&[3])],
        "the resume rebuilt an obsolete desired set"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The desired set changed while the machine slept must not start a session
/// before the resume settle has run: a suspended backend that reconciles is a
/// backend fighting the teardown it just requested.
#[tokio::test]
async fn a_membership_change_while_suspended_starts_nothing() {
    let rig = SystemRig::active_with_spare(&[1, 2], &[3], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;
    rig.set_desired(&[3]).await;
    rig.round_trip().await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "a suspended backend started a session"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Suspend publishes a stopped session that is explicitly resume-pending, and
/// keeps both the desired set and the run intent: the user never stopped
/// anything, so a resume has to know what to bring back.
#[tokio::test]
async fn suspend_publishes_stopped_with_resume_pending_and_keeps_intent() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    // Resume-pending is published the moment the suspend is accepted; the
    // supervisor reports the emptied session a moment later.
    rig.wait_for(|snapshot| snapshot.session.resume_pending && snapshot.session.active.is_empty())
        .await;

    let snapshot = rig.snapshot();
    assert!(
        snapshot.session.active.is_empty(),
        "the session kept members"
    );
    assert_eq!(
        snapshot.run_intent,
        RunIntent::Running,
        "the run intent was dropped"
    );
    assert_eq!(
        snapshot.desired_members,
        set_of(&[1, 2]),
        "the desired set was dropped"
    );

    rig.notify(SystemEvent::Resumed).await;
    rig.discover_stably(1, SETTLE).await;
    assert!(
        !rig.snapshot().session.resume_pending,
        "resume-pending outlived the resume"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A resume recreates the capture generation, so no pre-suspend WASAPI worker
/// can keep feeding a timeline that no longer exists.
#[tokio::test]
async fn resume_recreates_the_capture_generation() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let before = rig.capture_readies();

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.resume_pending && snapshot.session.active.is_empty())
        .await;
    rig.notify(SystemEvent::Resumed).await;
    rig.discover_stably(1, SETTLE).await;
    rig.wait_until(|| rig.capture_readies() > before).await;

    assert!(
        rig.capture_readies() > before,
        "the pre-suspend capture generation survived the resume"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Repeated sleep cycles must not accumulate transports; each suspend drops
/// its generation and each resume builds exactly one new one.
#[tokio::test]
async fn suspend_resume_cycles_leak_no_transports() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    for _ in 0..5 {
        rig.notify(SystemEvent::Suspending).await;
        rig.wait_for(|snapshot| {
            snapshot.session.resume_pending && snapshot.session.active.is_empty()
        })
        .await;
        rig.notify(SystemEvent::Resumed).await;
        rig.discover_stably(1, SETTLE).await;
    }

    assert_eq!(
        rig.started_member_sets().len(),
        6,
        "each cycle owes exactly one rebuild"
    );
    assert_eq!(rig.shutdown().await, 0, "sleep cycles leaked transports");
}

/// One interface change fires the Windows callback many times. Twenty
/// callbacks are one event, not twenty discovery restarts -- and a genuinely
/// later change still has to get through.
#[tokio::test]
async fn callback_burst_coalesces_but_later_binding_change_recovers() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    rig.network_callbacks(20).await;
    rig.settle_network().await;
    assert_eq!(
        rig.network_restart_count(),
        1,
        "the burst was not coalesced"
    );

    rig.change_active_binding().await;
    assert_eq!(
        rig.network_restart_count(),
        2,
        "a later binding change was swallowed by the coalescer"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Restarting the AirPlay group is only justified when a local binding
/// actually moved. A cable unplugged somewhere else on the machine must cost
/// the listeners nothing.
#[tokio::test]
async fn only_a_changed_local_binding_restarts_the_session() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    rig.network_callbacks(3).await;
    rig.settle_network().await;
    assert_eq!(
        rig.restarts_for(RestartReason::LocalInterfaceChanged),
        0,
        "an unchanged binding restarted the session"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "an unchanged binding rebuilt the group"
    );

    rig.change_active_binding().await;
    rig.wait_for_starts(2).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;
    assert_eq!(
        rig.restarts_for(RestartReason::LocalInterfaceChanged),
        1,
        "the changed binding did not rebuild the group"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Network recovery is not a secondary rejoin, so the thirty-second rejoin
/// spacing must not delay it. The binding change below lands well inside that
/// window and still has to rebuild immediately.
#[tokio::test]
async fn network_recovery_ignores_the_rejoin_spacing_window() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    // Spend the group's rejoin budget: the primary loss below is exactly the
    // restart the spacing gate counts.
    rig.fail_primary(1).await;
    rig.wait_for(|snapshot| snapshot.session.primary == Some(rid(2)))
        .await;
    let before = rig.started_member_sets().len();

    // Far inside the thirty-second window.
    rig.advance(Duration::from_secs(1)).await;
    rig.change_active_binding().await;
    // The coalesced edge is observable before the rebuild reaches the factory;
    // this wait is the assertion that it arrives at all.
    rig.wait_for_starts(before + 1).await;

    assert_eq!(
        rig.started_member_sets().len(),
        before + 1,
        "the rejoin spacing swallowed a network recovery"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A network edge that arrives while the machine is suspended must not rebuild
/// anything: the resume path owns that rebuild, and doing it twice races the
/// teardown still in flight.
#[tokio::test]
async fn a_network_edge_while_suspended_rebuilds_nothing() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.resume_pending && snapshot.session.active.is_empty())
        .await;

    rig.change_active_binding().await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "a suspended backend rebuilt on a network edge"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The notification handle is an OS resource; shutting the backend down has to
/// hand it back. A fake subscription counts the cancellation the real
/// `CancelMibChangeNotify2` would perform.
#[tokio::test]
async fn shutdown_cancels_the_network_subscription() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let network = rig.network();
    assert_eq!(network.subscriptions(), 1, "the monitor never subscribed");
    assert_eq!(network.cancellations(), 0, "the subscription died early");

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
    network.wait_for_cancel().await;
}

/// Swapping the render endpoint is a capture-side event. It re-opens capture
/// and never touches the AirPlay session -- the listeners hear bridged silence
/// for the swap, not a reconnect.
#[tokio::test]
async fn endpoint_recovery_does_not_restart_the_airplay_session() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let generation_before = rig.session_generation();

    rig.send(BackendCommand::SetAudioEndpoint(
        AudioEndpointPreference::Explicit {
            id: "endpoint-other".to_owned(),
            last_known_name: "Other Speakers".to_owned(),
        },
    ))
    .await;
    rig.wait_for(|snapshot| {
        snapshot
            .audio_source
            .captured_endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.id == "endpoint-other")
    })
    .await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "an endpoint swap restarted the AirPlay session"
    );
    assert_eq!(
        rig.session_generation(),
        generation_before,
        "an endpoint swap consumed a session generation"
    );
    assert_eq!(rig.active_members(), set_of(&[1, 2]));
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The clock seam itself: without an explicit advance nothing in this suite
/// may fire, or every assertion above would be a race against wall time.
#[tokio::test]
async fn the_resume_settle_waits_for_the_injected_clock() {
    let clock = TestClock::new();
    let rig = SystemRig::active_on(&[1, 2], 1, clock).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.resume_pending && snapshot.session.active.is_empty())
        .await;
    rig.notify(SystemEvent::Resumed).await;

    // One second short of the mandated settle.
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "the resume rebuilt before the interface settle elapsed"
    );

    rig.advance(Duration::from_secs(1)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2]), set_of(&[1, 2])],
        "the settle deadline produced no rebuild"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The suspend gate must not depend on a broadcast Windows is free to swallow.
/// It closes every automatic recovery path AND every user command that would
/// reconcile, so a lost `PBT_APMRESUMEAUTOMATIC` would otherwise leave a
/// backend that wants to run, shows itself as running, and never starts
/// anything again.
#[tokio::test]
async fn a_swallowed_resume_broadcast_is_overtaken_by_the_watchdog() {
    let rig = SystemRig::active(&[1, 2], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.resume_pending && snapshot.session.active.is_empty())
        .await;

    // No `Resumed` ever arrives. Short of the watchdog deadline the machine
    // stays asleep -- the gate is still doing its job.
    rig.advance(SUSPEND_WATCHDOG - Duration::from_secs(1)).await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "the watchdog fired before its deadline"
    );
    assert!(
        rig.snapshot().session.resume_pending,
        "the suspend ended early"
    );

    rig.advance(Duration::from_secs(1)).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;
    assert!(
        !rig.snapshot().session.resume_pending,
        "a swallowed resume broadcast wedged the backend"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2]), set_of(&[1, 2])],
        "the watchdog produced no rebuild"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A user command that arrives while the suspend gate is shut is deliberately
/// held, not lost: once the gate opens -- here through the watchdog, with no
/// resume broadcast at all -- the rebuild targets the NEWEST desired set.
#[tokio::test]
async fn a_command_held_by_the_suspend_gate_survives_the_watchdog() {
    let rig = SystemRig::active_with_spare(&[1, 2], &[3], 1).await;

    rig.notify(SystemEvent::Suspending).await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;
    rig.set_desired(&[3]).await;

    rig.advance(SUSPEND_WATCHDOG).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[3]))
        .await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2]), set_of(&[3])],
        "the held membership change was dropped instead of applied"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

// ---------------------------------------------------------------------------
// Startup auto-connect (Task 17 Step 5)
// ---------------------------------------------------------------------------
//
// The arm under test is a deadline, and a deadline is hard to observe: the
// controller drops and rebuilds its timer futures on every loop iteration, so
// a live arm is missing from the pending map for the microseconds in between.
// Both "and then nothing started" and "and the window is closed now" pass
// against a fully armed backend when they sample that gap -- both mistakes
// were made here and both were caught only by mutating the disarm away.
// `deadline_registrations` is the fix: it counts every arming, never
// decreases, and therefore says "gone" only when it really is.
//
// At an offset of two seconds this counter belongs to the startup arm alone.
// The resume settle shares the offset but needs a suspend first, and the
// discovery/capture ladders only arm after a failure; neither happens here.

/// Auto-connect is a startup convenience, not a membership policy. It waits
/// for the persisted group to become stably castable, flips the run intent
/// exactly once, and leaves the desired set completely alone -- a receiver the
/// user never selected must not be dragged into the group just because it
/// happened to answer discovery first.
#[tokio::test]
async fn startup_auto_connect_runs_once_and_never_selects_an_unrelated_receiver() {
    // Receiver 3 is castable from the same instant as receiver 1 and is not
    // part of the persisted group.
    let rig = SystemRig::launched_with(auto_connect_state(&[1]), &[1, 3], TestClock::new()).await;

    rig.wait_until(|| rig.deadline_registrations(DISCOVERY_STABLE) > 0)
        .await;
    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "auto-connect started before discovery was stable"
    );
    assert!(
        rig.started_member_sets().is_empty(),
        "auto-connect started a session before discovery was stable"
    );

    rig.advance(DISCOVERY_STABLE).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1]))
        .await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Running,
        "auto-connect never switched the run intent"
    );
    assert_eq!(
        rig.snapshot().desired_members,
        set_of(&[1]),
        "auto-connect widened the desired membership"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1])],
        "an unrelated receiver was pulled into the group"
    );

    // The arm is spent: it is never armed again, so no amount of further
    // stability can produce a second automatic start.
    //
    // The offset watched here is zero, not the stability window: receiver 1
    // settled at the instant the advance above reached, so a still-armed
    // backend would compute a delay of zero and re-register it on every loop
    // iteration. Watching `DISCOVERY_STABLE` from *this* point in fake time
    // would watch a deadline nothing could ever arm, and the assertion would
    // hold no matter what the backend did.
    let armings = rig.deadline_registrations(Duration::ZERO);
    rig.round_trip().await;
    rig.round_trip().await;
    assert_eq!(
        rig.deadline_registrations(Duration::ZERO),
        armings,
        "the startup arm re-armed itself after firing"
    );
    rig.advance(DISCOVERY_STABLE * 4).await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1])],
        "auto-connect fired more than once per process"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// An explicit stop outranks a persisted preference for the rest of the
/// process. Anything else means the user presses Stop and the machine starts
/// playing again two seconds later.
#[tokio::test]
async fn an_explicit_stop_disarms_startup_auto_connect_for_the_whole_process() {
    let rig = SystemRig::launched_with(auto_connect_state(&[1]), &[1], TestClock::new()).await;

    // Control: the launch really is holding an automatic start.
    rig.wait_until(|| rig.deadline_registrations(DISCOVERY_STABLE) > 0)
        .await;

    rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.round_trip().await;

    // A still-armed backend re-arms on every loop iteration; two round trips
    // are two iterations, so an unchanged count means the arm is gone.
    let armings = rig.deadline_registrations(DISCOVERY_STABLE);
    rig.round_trip().await;
    rig.round_trip().await;
    assert_eq!(
        rig.deadline_registrations(DISCOVERY_STABLE),
        armings,
        "the explicit stop left the automatic start armed"
    );
    rig.advance(DISCOVERY_STABLE * 10).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "auto-connect overruled an explicit stop"
    );
    assert!(
        rig.started_member_sets().is_empty(),
        "a deliberately stopped backend started a session"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Switching the policy on is a decision about later launches. It persists,
/// and it does not reach into the session the user is sitting in front of.
#[tokio::test]
async fn enabling_auto_connect_at_runtime_persists_without_starting_anything() {
    let state = PersistedStateV1 {
        last_desired_members: set_of(&[1]),
        ..PersistedStateV1::default()
    };
    let rig = SystemRig::launched_with(state, &[1], TestClock::new()).await;

    rig.send(BackendCommand::SetAutoConnect(true)).await;
    rig.wait_for(|snapshot| snapshot.auto_connect).await;

    // The command armed no automatic start: a backend that had armed one
    // would re-arm it on every loop iteration from here on.
    let armings = rig.deadline_registrations(DISCOVERY_STABLE);
    rig.round_trip().await;
    rig.round_trip().await;
    assert_eq!(
        rig.deadline_registrations(DISCOVERY_STABLE),
        armings,
        "SetAutoConnect armed the running process"
    );
    rig.advance(DISCOVERY_STABLE * 10).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "SetAutoConnect changed the current run intent"
    );
    assert!(
        rig.started_member_sets().is_empty(),
        "SetAutoConnect started a session in the running process"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Switching the policy off before the stability window closes has to cancel
/// the pending start too; otherwise the setting the user just cleared still
/// gets one last turn.
#[tokio::test]
async fn switching_auto_connect_off_cancels_the_pending_startup_arm() {
    let rig = SystemRig::launched_with(auto_connect_state(&[1]), &[1], TestClock::new()).await;

    rig.wait_until(|| rig.deadline_registrations(DISCOVERY_STABLE) > 0)
        .await;

    rig.send(BackendCommand::SetAutoConnect(false)).await;
    rig.wait_for(|snapshot| !snapshot.auto_connect).await;

    let armings = rig.deadline_registrations(DISCOVERY_STABLE);
    rig.round_trip().await;
    rig.round_trip().await;
    assert_eq!(
        rig.deadline_registrations(DISCOVERY_STABLE),
        armings,
        "clearing the preference left the pending start armed"
    );
    rig.advance(DISCOVERY_STABLE * 10).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "a cleared auto-connect preference still started the group"
    );
    assert!(
        rig.started_member_sets().is_empty(),
        "a cleared auto-connect preference still started a session"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Activating a saved group with `start: false` is the user answering exactly
/// the question auto-connect exists to answer, and the answer is no. A launch
/// arm that is still open -- because the persisted group was never castable --
/// must not overrule it two seconds later.
#[tokio::test]
async fn activating_a_saved_group_without_starting_disarms_startup_auto_connect() {
    let group = SavedGroup {
        id: SavedGroupId::generate(),
        name: "Kitchen".to_owned(),
        members: vec![SavedGroupMember {
            receiver: rid(1),
            last_known_name: "kitchen speaker".to_owned(),
            level: Volume::UNITY,
        }],
    };
    let id = group.id;
    // Receiver 9 is persisted as desired but never announced, so the launch
    // arm cannot spend itself and is still open when the command below lands.
    let state = PersistedStateV1 {
        saved_groups: vec![group],
        ..auto_connect_state(&[9])
    };
    let rig = SystemRig::launched_with(state, &[1], TestClock::new()).await;
    rig.round_trip().await;

    rig.send(BackendCommand::ActivateSavedGroup { id, start: false })
        .await;
    rig.wait_for(|snapshot| snapshot.desired_members == set_of(&[1]))
        .await;

    rig.advance(DISCOVERY_STABLE * 2).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "a declined start was overruled by the launch arm"
    );
    // A stopped app builds nothing, so the membership edit itself starts
    // no session. The arm remains countable here all the same: firing it
    // would set the run intent to `Running` first, and the reconciliation
    // that follows would show up as a start.
    assert_eq!(
        rig.started_member_sets().len(),
        0,
        "the launch arm reconciled on top of the declined activation"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The launch window is absolute. Past it, ticking a receiver is an edit in a
/// running process and nothing else -- a persisted preference from the last
/// run may not turn it into a play command.
#[tokio::test]
async fn the_startup_auto_connect_window_closes_with_the_launch() {
    // Nothing the launch wants is announced, so the arm survives the launch
    // itself and only the window can close it.
    let rig = SystemRig::launched_with(auto_connect_state(&[9]), &[1], TestClock::new()).await;
    rig.round_trip().await;
    rig.advance(AUTO_CONNECT_WINDOW + DISCOVERY_STABLE).await;

    rig.set_desired(&[1]).await;
    rig.wait_for(|snapshot| snapshot.desired_members == set_of(&[1]))
        .await;
    rig.advance(DISCOVERY_STABLE * 2).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "selecting a receiver long after launch started playback by itself"
    );
    // Same reasoning as above: the edit alone starts nothing while stopped,
    // and a firing arm would announce itself by starting one.
    assert_eq!(
        rig.started_member_sets().len(),
        0,
        "a stale launch arm reconciled on top of the membership edit"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

// ---------------------------------------------------------------------------
// Mute and effective volume (Task 17 Step 6)
// ---------------------------------------------------------------------------

/// Validated linear volume for a test literal.
fn volume(value: f32) -> Volume {
    Volume::new(value).expect("test literals stay in range")
}

/// Mute has to reach every receiver that is actually playing. A group where
/// only the primary falls silent is worse than no mute at all -- and none of
/// this may cost a session generation, because a restart is an audible gap.
#[tokio::test]
async fn mute_sends_zero_effective_volume_to_every_active_receiver() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let generation = rig.session_generation();
    let baseline = rig.volume_sends().len();

    rig.send(BackendCommand::SetMuted(true)).await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 2)
        .await;

    assert_eq!(
        rig.volume_sends()[baseline..baseline + 2],
        vec![(rid(1), 0.0), (rid(2), 0.0)],
        "mute did not silence every active receiver"
    );
    assert_eq!(
        rig.session_generation(),
        generation,
        "mute consumed a session generation"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "mute restarted the group"
    );
    assert_eq!(
        rig.snapshot().master_volume,
        Volume::DEFAULT_MASTER,
        "mute overwrote the stored master volume"
    );

    // Unmuting restores the preserved value rather than some default.
    rig.send(BackendCommand::SetMuted(false)).await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 4)
        .await;
    assert_eq!(
        rig.volume_sends()[baseline + 2..baseline + 4],
        [(rid(1), 0.25), (rid(2), 0.25)],
        "unmuting did not restore the preserved volumes"
    );
    assert_eq!(
        rig.session_generation(),
        generation,
        "unmuting consumed a session generation"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Loading restores receiver levels into the published snapshot, and a
/// refused update leaves that snapshot truthful along with durable state.
#[tokio::test]
async fn receiver_level_snapshot_restores_and_rejects_failed_save() {
    let receiver = rid(1);
    let state = PersistedStateV1 {
        receiver_levels: [(receiver, volume(0.4))].into_iter().collect(),
        ..PersistedStateV1::default()
    };
    let rig = SystemRig::launched_with_failing_store(state, &[1], TestClock::new()).await;
    rig.round_trip().await;

    assert_eq!(
        rig.snapshot().receiver_levels.get(&receiver),
        Some(&volume(0.4)),
        "launch must publish persisted receiver levels"
    );

    rig.send(BackendCommand::SetReceiverLevel {
        receiver,
        level: volume(0.8),
    })
    .await;
    rig.wait_for(|snapshot| snapshot.persistence == PersistenceSnapshot::Error)
        .await;

    assert_eq!(
        rig.snapshot().receiver_levels.get(&receiver),
        Some(&volume(0.4)),
        "a failed save must not publish the rejected receiver level"
    );
    assert_eq!(rig.shutdown().await, 0);
}

/// A receiver that refuses a volume request is that receiver's problem alone.
/// The send is per receiver, so the others still get the value the user just
/// chose -- and every later change as well, the refusing one included: a
/// rejected control request is not evidence that the stream is gone, and the
/// health probe is what establishes that.
#[tokio::test]
async fn a_failing_volume_control_does_not_block_the_other_receivers() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let baseline = rig.volume_sends().len();
    rig.fail_volume(2);

    rig.send(BackendCommand::SetMasterVolume(volume(0.5))).await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 2)
        .await;
    rig.round_trip().await;
    rig.round_trip().await;

    assert_eq!(
        rig.volume_sends()[baseline..baseline + 2],
        vec![(rid(1), 0.5), (rid(2), 0.5)],
        "the failing receiver was skipped instead of attempted"
    );
    assert_eq!(
        rig.snapshot().session.active,
        set_of(&[1, 2]),
        "a refused volume request removed a streaming receiver"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "one refused volume request restarted the whole group"
    );

    // Every member still follows later changes.
    rig.send(BackendCommand::SetMasterVolume(volume(0.75)))
        .await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 4)
        .await;
    assert_eq!(
        rig.volume_sends()[baseline + 2..baseline + 4],
        [(rid(1), 0.75), (rid(2), 0.75)],
        "a refused request blocked every later send"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The primary refusing a volume request is the mirror of the case above, and
/// it must end the same way. One rejected SET_PARAMETER response says nothing
/// about the group's PTP time base, so escalating it to a primary loss would
/// buy a full teardown -- and an audible gap -- for a single drag of a slider.
#[tokio::test]
async fn a_failing_volume_control_on_the_primary_does_not_restart_the_group() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let generation = rig.session_generation();
    let baseline = rig.volume_sends().len();
    rig.fail_volume(1);

    rig.send(BackendCommand::SetMasterVolume(volume(0.5))).await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 2)
        .await;
    // Two loop iterations after the refusal: a restart this triggered would
    // have been requested by now.
    rig.round_trip().await;
    rig.round_trip().await;

    assert_eq!(
        rig.session_generation(),
        generation,
        "a refused volume request on the primary opened a new session generation"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "a refused volume request on the primary restarted the whole group"
    );
    assert_eq!(
        rig.snapshot().session.active,
        set_of(&[1, 2]),
        "a refused volume request removed a streaming receiver"
    );
    assert_eq!(
        rig.primary(),
        Some(rid(1)),
        "a refused volume request unseated the PTP primary"
    );

    // The group still follows later changes, including on the refusing one.
    rig.send(BackendCommand::SetMasterVolume(volume(0.75)))
        .await;
    rig.wait_until(|| rig.volume_sends().len() >= baseline + 4)
        .await;
    assert_eq!(
        rig.volume_sends()[baseline + 2..baseline + 4],
        [(rid(1), 0.75), (rid(2), 0.75)],
        "a refused request blocked every later send"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The per-receiver level multiplies the master value, and neither of them
/// costs a restart.
#[tokio::test]
async fn a_receiver_level_scales_the_master_volume_without_a_restart() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let generation = rig.session_generation();
    let baseline = rig.volume_sends().len();

    rig.send(BackendCommand::SetReceiverLevel {
        receiver: rid(2),
        level: volume(0.5),
    })
    .await;
    rig.wait_for(|snapshot| snapshot.receiver_levels.get(&rid(2)) == Some(&volume(0.5)))
        .await;

    assert_eq!(
        rig.snapshot().receiver_levels.get(&rid(2)),
        Some(&volume(0.5)),
        "the published snapshot must expose the committed receiver level"
    );

    assert_eq!(
        rig.volume_sends()[baseline..baseline + 2],
        vec![(rid(1), 0.25), (rid(2), 0.125)],
        "the receiver level did not scale the master volume"
    );
    assert_eq!(
        rig.session_generation(),
        generation,
        "a receiver level consumed a session generation"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "a receiver level restarted the group"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Persisted per-receiver levels must be applied when a session becomes
/// active, including after a later full restart.
#[tokio::test]
async fn stored_receiver_levels_apply_on_start_and_restart() {
    let state = PersistedStateV1 {
        master_volume: volume(0.5),
        last_desired_members: set_of(&[1, 2]),
        receiver_levels: [(rid(1), volume(0.4)), (rid(2), volume(0.8))]
            .into_iter()
            .collect(),
        ..PersistedStateV1::default()
    };
    let rig = SystemRig::launched_with(state, &[1, 2], TestClock::new()).await;

    rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
        .await;
    rig.wait_for(|snapshot| matches!(snapshot.session.phase, SessionPhase::Streaming { .. }))
        .await;
    assert!(
        rig.volume_sends().len() >= 2,
        "active session received no startup volumes"
    );
    assert_eq!(
        rig.volume_sends(),
        vec![(rid(1), 0.2), (rid(2), 0.4)],
        "stored levels were not applied when the session became active"
    );

    rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.wait_for(|snapshot| snapshot.session.phase == SessionPhase::Stopped)
        .await;
    rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
        .await;
    rig.wait_for(|snapshot| matches!(snapshot.session.phase, SessionPhase::Streaming { .. }))
        .await;
    assert!(
        rig.volume_sends().len() >= 4,
        "restarted session received no volumes"
    );
    assert_eq!(
        rig.volume_sends()[2..],
        [(rid(1), 0.2), (rid(2), 0.4)],
        "stored levels were not reapplied after restart"
    );
    assert_eq!(rig.shutdown().await, 0);
}

// ---------------------------------------------------------------------------
// Latency preset availability (Task 17 Step 7)
// ---------------------------------------------------------------------------

/// A preset behind its hardware gate is visible, refused, and inert. Visible
/// because a missing option is indistinguishable from a broken build; refused
/// with the very reason the row carries; inert because a rejected command that
/// still moves state is worse than one that never ran.
#[tokio::test]
async fn a_disabled_latency_preset_is_rejected_and_changes_nothing() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let before = rig.snapshot();

    let id = rig
        .send(BackendCommand::SetLatencyPreset(LatencyPreset::Low))
        .await;
    let error = rig.command_failure(id).await;

    let offered = before
        .latency_presets
        .iter()
        .find(|row| row.preset == LatencyPreset::Low)
        .expect("a gated preset is still offered");
    assert_eq!(
        offered.disabled_reason.as_ref(),
        Some(&error),
        "the rejection and the greyed-out row disagree about why"
    );
    assert!(
        before
            .latency_presets
            .iter()
            .any(|row| row.preset == LatencyPreset::Normal && row.disabled_reason.is_none()),
        "the validated default was offered as disabled"
    );

    // The rejection itself is now durable-in-memory command feedback and
    // publishes a snapshot revision. Only domain state must remain unchanged.
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(completed, failure)| {
                *completed == id
                    && *failure == Some(homepod_cast::backend::model::CommandFailure::Rejected)
            })
    })
    .await;
    let after = rig.snapshot();
    assert_eq!(
        after.latency_preset, before.latency_preset,
        "a rejected preset was applied anyway"
    );
    assert_eq!(after.latency_presets, before.latency_presets);
    assert_eq!(after.desired_members, before.desired_members);
    assert_eq!(after.desired_revision, before.desired_revision);
    assert_eq!(after.run_intent, before.run_intent);
    assert_eq!(after.session, before.session);
    assert_eq!(after.receiver_levels, before.receiver_levels);
    assert_eq!(after.master_volume, before.master_volume);
    assert_eq!(after.muted, before.muted);
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "a rejected preset restarted the group"
    );
    assert_eq!(
        rig.restarts_for(RestartReason::LatencyPresetChanged),
        0,
        "a rejected preset announced a restart"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Re-selecting the preset that is already active is a no-op, not a restart:
/// the restart matrix pays for a change, never for a confirmation.
#[tokio::test]
async fn reselecting_the_active_latency_preset_completes_without_a_restart() {
    let rig = SystemRig::active(&[1, 2], 1).await;
    let generation = rig.session_generation();

    let id = rig
        .send(BackendCommand::SetLatencyPreset(LatencyPreset::Normal))
        .await;
    rig.command_completion(id).await;

    assert_eq!(
        rig.session_generation(),
        generation,
        "confirming the active preset consumed a session generation"
    );
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "confirming the active preset restarted the group"
    );
    assert_eq!(
        rig.restarts_for(RestartReason::LatencyPresetChanged),
        0,
        "confirming the active preset announced a restart"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A stopped app records the selection and builds nothing. Anything else makes
/// the Stop button a lie: the user stops, ticks one more speaker, and audio is
/// back on every receiver.
#[tokio::test]
async fn ticking_a_receiver_while_stopped_changes_only_the_selection() {
    let rig =
        SystemRig::launched_with(PersistedStateV1::default(), &[1, 2], TestClock::new()).await;
    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "a launch without auto-connect is the stopped baseline this test needs"
    );

    rig.set_desired(&[1, 2]).await;
    rig.wait_for(|snapshot| snapshot.desired_members == set_of(&[1, 2]))
        .await;

    // Two further loop iterations: a backend that reconciled the selection
    // would have handed the desired set to the factory by now.
    rig.round_trip().await;
    rig.round_trip().await;

    assert!(
        rig.started_member_sets().is_empty(),
        "a stopped backend built a session out of a selection change: {:?}",
        rig.started_member_sets()
    );
    assert_eq!(
        rig.snapshot().session.phase,
        SessionPhase::Stopped,
        "a stopped backend published a running session"
    );
    // Synchronous proof rather than an absence observed after a while: the
    // restart diagnostic is emitted inside `request_reconcile`, in the very
    // batch that carried the command, so a reconciliation that ran would be
    // countable here no matter how the runtime scheduled the rest.
    assert_eq!(
        rig.restarts_for(RestartReason::MembershipChange),
        0,
        "the stopped backend spent a session generation on a selection change"
    );

    // The selection was not discarded, only held: the start that follows
    // builds against what was ticked while the app was stopped.
    rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
        .await;
    rig.wait_for_starts(1).await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1, 2])],
        "the start built against a stale selection"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Stop means stop: the session is torn down, and nothing the user selects
/// afterwards revives it until they ask for it again.
#[tokio::test]
async fn stop_tears_the_session_down_and_a_later_selection_does_not_revive_it() {
    let rig = SystemRig::active_with_spare(&[1], &[2], 1).await;
    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1])],
        "the rig brings the group up in exactly one generation"
    );

    rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.wait_for(|snapshot| {
        snapshot.session.active.is_empty() && snapshot.session.phase == SessionPhase::Stopped
    })
    .await;

    rig.set_desired(&[1, 2]).await;
    rig.round_trip().await;
    rig.round_trip().await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1])],
        "a stopped backend started a session; the stop button is decoration"
    );
    assert_eq!(
        rig.snapshot().desired_members,
        set_of(&[1, 2]),
        "the selection made while stopped was dropped instead of recorded"
    );
    // Exactly one: the generation the stop itself spent tearing the session
    // down. The selection that followed found nothing to hold down and
    // therefore reconciled nothing -- again observed synchronously.
    assert_eq!(
        rig.restarts_for(RestartReason::MembershipChange),
        1,
        "a stopped backend reconciled on top of its own teardown"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A running app whose only receiver drops out is *restarting*, not stopped.
///
/// The reconciliation that follows a primary failure hands the supervisor an
/// empty desired set -- the failed receiver is serving its backoff and is
/// filtered out -- but the emptiness is a symptom, not the user's intent. A
/// supervisor that reads it as "stopped" publishes `SessionPhase::Stopped`
/// under `RunIntent::Running`, a pair `derive_phase` never produces, and the
/// shell loses the only signal that says the app is still trying.
#[tokio::test]
async fn a_lone_receiver_failing_publishes_recovery_not_a_stop() {
    let rig = SystemRig::active(&[1], 1).await;
    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Running,
        "the rig starts this session, so the intent under test is Running"
    );

    rig.fail_probe(1);
    // Both outcomes -- the defect's stop and the correct recovery -- settle
    // here, so the wait bounds only the failure case and the assertion below
    // decides which one actually happened.
    rig.wait_for(|snapshot| {
        matches!(
            snapshot.session.phase,
            SessionPhase::Restarting { .. } | SessionPhase::Stopped
        )
    })
    .await;

    let snapshot = rig.snapshot();
    assert!(
        matches!(
            snapshot.session.phase,
            SessionPhase::Restarting {
                reason: RestartReason::DeadRtspRecovered,
                ..
            }
        ),
        "a running app with its only receiver in backoff published {:?}",
        snapshot.session.phase
    );
    assert_eq!(
        snapshot.run_intent,
        RunIntent::Running,
        "the failure changed the run intent"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Starting a saved group that is already the current selection is still a
/// start. The durable transaction has nothing to write in that case and
/// returns early -- so the run intent may not be carried inside it.
#[tokio::test]
async fn activating_the_already_selected_group_still_starts_it() {
    let group = SavedGroup {
        id: SavedGroupId::generate(),
        name: "Kitchen".to_owned(),
        members: vec![SavedGroupMember {
            receiver: rid(1),
            last_known_name: "kitchen speaker".to_owned(),
            level: Volume::UNITY,
        }],
    };
    let id = group.id;
    // Membership and level are already on disk exactly as the group states
    // them, so activating it produces a candidate equal to the persisted
    // document and the durable commit short-circuits.
    let state = PersistedStateV1 {
        saved_groups: vec![group],
        last_desired_members: set_of(&[1]),
        receiver_levels: [(rid(1), Volume::UNITY)].into_iter().collect(),
        ..PersistedStateV1::default()
    };
    let rig = SystemRig::launched_with(state, &[1], TestClock::new()).await;
    rig.round_trip().await;
    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Stopped,
        "a launch without auto-connect is the stopped baseline this test needs"
    );

    rig.send(BackendCommand::ActivateSavedGroup { id, start: true })
        .await;
    rig.wait_for(|snapshot| snapshot.run_intent == RunIntent::Running)
        .await;
    rig.wait_for_starts(1).await;

    assert_eq!(
        rig.started_member_sets(),
        vec![set_of(&[1])],
        "the explicit start built against the wrong set"
    );
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

#[tokio::test]
async fn release_group_receipt_while_suspended_requires_the_resume_attempt() {
    use homepod_cast::backend::command::ReceiptState;
    let rig = SystemRig::active(&[1], 1).await;
    let previous = match rig.snapshot().session.phase {
        SessionPhase::Streaming { generation } => generation,
        ref phase => panic!("expected initial active phase, got {phase:?}"),
    };
    let id = SavedGroupId::generate();
    rig.send(BackendCommand::SaveGroup {
        id: Some(id),
        name: "Kitchen".into(),
        members: vec![SavedGroupMember {
            receiver: rid(1),
            last_known_name: "Kitchen".into(),
            level: Volume::UNITY,
        }],
    })
    .await;
    rig.wait_for(|snapshot| snapshot.saved_groups.iter().any(|group| group.id == id))
        .await;
    rig.send(BackendCommand::NotifySystem(SystemEvent::Suspending))
        .await;
    rig.wait_for(|snapshot| {
        snapshot.session.resume_pending && snapshot.session.phase == SessionPhase::Stopped
    })
    .await;
    let (_, receipt) = rig
        .handle()
        .try_send_confirmed(BackendCommand::ActivateSavedGroup { id, start: true })
        .unwrap();
    let receipt = std::cell::RefCell::new(receipt);
    rig.wait_for(|snapshot| {
        receipt.borrow_mut().poll(snapshot.revision) == ReceiptState::Completed(None)
    })
    .await;
    let floor = receipt
        .borrow()
        .session_generation_floor(rig.snapshot().revision)
        .unwrap();
    assert!(floor > previous);
    assert_eq!(rig.snapshot().session.phase, SessionPhase::Stopped);
    rig.notify(SystemEvent::Resumed).await;
    rig.discover_stably(1, SETTLE).await;
    rig.wait_for(|snapshot| matches!(snapshot.session.phase, SessionPhase::Streaming { generation } if generation >= floor)).await;
    assert_eq!(rig.shutdown().await, 0);
}

#[tokio::test]
async fn release_failed_group_level_write_with_matching_members_never_starts() {
    let group = SavedGroup {
        id: SavedGroupId::generate(),
        name: "Kitchen".into(),
        members: vec![SavedGroupMember {
            receiver: rid(1),
            last_known_name: "Kitchen".into(),
            level: Volume::new(0.42).unwrap(),
        }],
    };
    let id = group.id;
    let state = PersistedStateV1 {
        saved_groups: vec![group],
        last_desired_members: set_of(&[1]),
        receiver_levels: [(rid(1), Volume::UNITY)].into_iter().collect(),
        ..PersistedStateV1::default()
    };
    let rig = SystemRig::launched_with_failing_store(state, &[1], TestClock::new()).await;
    rig.round_trip().await;
    rig.send(BackendCommand::ActivateSavedGroup { id, start: true })
        .await;
    rig.wait_for(|snapshot| snapshot.persistence == PersistenceSnapshot::Error)
        .await;
    rig.round_trip().await;
    assert_eq!(rig.snapshot().run_intent, RunIntent::Stopped);
    assert_eq!(rig.snapshot().receiver_levels[&rid(1)], Volume::UNITY);
    assert!(rig.started_member_sets().is_empty());
    assert_eq!(rig.shutdown().await, 0);
}

/// A refused save must leave nothing behind. The backend tells the user the
/// change was applied neither on disk nor in memory, so the one in-memory
/// side effect this command carries -- disarming the launch arm -- may not
/// survive the failure either. A user whose disk is full would otherwise be
/// told nothing happened while the automatic start they were counting on had
/// quietly been cancelled for the rest of the process.
#[tokio::test]
async fn a_failed_activation_does_not_disarm_startup_auto_connect() {
    let group = SavedGroup {
        id: SavedGroupId::generate(),
        name: "Kitchen".to_owned(),
        members: vec![SavedGroupMember {
            receiver: rid(1),
            last_known_name: "kitchen speaker".to_owned(),
            // Differs from the stored document, so the activation really does
            // attempt a write instead of completing as an accepted no-op.
            level: Volume::new(0.42).expect("a valid level"),
        }],
    };
    let id = group.id;
    // Receiver 1 is both the persisted desired member and the group member,
    // so the only thing the command can change is the stored level -- and the
    // launch arm, if it is allowed to.
    let state = PersistedStateV1 {
        saved_groups: vec![group],
        ..auto_connect_state(&[1])
    };
    let rig = SystemRig::launched_with_failing_store(state, &[1], TestClock::new()).await;
    rig.round_trip().await;

    rig.send(BackendCommand::ActivateSavedGroup { id, start: false })
        .await;
    rig.wait_for(|snapshot| snapshot.persistence == PersistenceSnapshot::Error)
        .await;
    assert_eq!(
        rig.snapshot().desired_members,
        set_of(&[1]),
        "the refused save announced its candidate anyway"
    );

    rig.advance(DISCOVERY_STABLE * 2).await;

    assert_eq!(
        rig.snapshot().run_intent,
        RunIntent::Running,
        "a save that failed still cancelled the launch arm, so the backend          reported a change it had in fact applied in memory"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}
