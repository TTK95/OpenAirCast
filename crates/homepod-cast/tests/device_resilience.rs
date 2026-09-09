//! Task 19: end-to-end device-resilience diagnostics.
//!
//! Steps 1-5: one correlated timeline for one failed receiver, the
//! correlation fields that make such a timeline readable at all, the
//! independent-failure-domain matrix, and the guarantee that the backend ends
//! its workers rather than waiting out a teardown budget. The fifty-cycle
//! leak soak is Step 6 and lives in `backend_lifecycle.rs`, beside the other
//! lifecycle guarantees.
//!
//! The plan sketches a `ResilienceRig`. This suite drives the existing
//! [`RecoveryRig`] instead: a third harness over the same controller, the
//! same supervisors, and the same leak accounting would only be a place for
//! the three to drift apart. The plan's verbs (`fail_secondary`,
//! `advance_to_retry`, `recover`, `diagnostics_for`) were added to that rig.

use std::time::Duration;

use homepod_cast::backend::controller::SHUTDOWN_GROUP_BUDGET;
use homepod_cast::backend::model::{
    AudioEndpointPreference, AudioFlow, AudioSourceState, ReceiverLifecycle, RunIntent,
    SessionPhase, Volume,
};
use homepod_cast::backend::{
    BackendCommand, DiagnosticCategory, DiagnosticEvent, DiagnosticPayload,
};
use homepod_cast::calibration::{CalibrationCommand, CalibrationProfile};
use homepod_cast::CalibrationApplyState;

#[path = "support/backend_rig.rs"]
mod backend_rig;

use backend_rig::{set_of, RecoveryRig, TestClock};

/// The rig's second render endpoint, so switching to it is observable.
const OTHER_ENDPOINT: &str = "endpoint-other";

/// Discovery stability window a rediscovered receiver has to survive.
const DISCOVERY_STABLE: Duration = Duration::from_secs(2);

/// PCM frames the deliberate overflow pushes. Comfortably above the queue's
/// thirty-two-frame capacity, so more than half of them must be evicted.
const PCM_BURST: u64 = 512;

/// Restarts the capture worker without touching the session, and returns the
/// capture-category records that restart produced.
///
/// An endpoint switch is the only edge that replaces the capture worker while
/// the session keeps running, which is exactly the separation these tests
/// need: whatever the records say about the session cannot have come from the
/// same event that produced them.
async fn capture_records_from_an_endpoint_switch(rig: &RecoveryRig) -> Vec<DiagnosticEvent> {
    rig.forget_diagnostics();
    rig.send(BackendCommand::SetAudioEndpoint(
        AudioEndpointPreference::Explicit {
            id: OTHER_ENDPOINT.to_owned(),
            last_known_name: "Other Speakers".to_owned(),
        },
    ))
    .await;
    rig.wait_for(|snapshot| {
        snapshot
            .audio_source
            .captured_endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.id == OTHER_ENDPOINT)
    })
    .await;
    let records: Vec<DiagnosticEvent> = rig
        .records()
        .into_iter()
        .filter(|event| event.category == DiagnosticCategory::Capture)
        .collect();
    assert!(
        !records.is_empty(),
        "the endpoint switch produced no capture diagnostics at all"
    );
    records
}

/// The four moments the plan requires a failed receiver's timeline to make
/// legible, expressed over the diagnostic kinds that already exist.
///
/// Deliberately not four new `DiagnosticKind` variants: the backend feed
/// documents its kinds as one-to-one with payload variants, and a second name
/// for `ReceiverTransition { to: Failed }` would leave consumers two ways to
/// ask the same question with no rule for which one to trust.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Milestone {
    /// The receiver dropped out of its generation.
    ReceiverSetupFailed,
    /// A reconnect was scheduled, with attempt number and deadline.
    RetryScheduled,
    /// The scheduled reconnect began.
    RetryStarted,
    /// The receiver is carrying the stream again.
    ReceiverRejoined,
}

/// Maps one receiver-scoped event onto its milestone.
///
/// `None` marks the *state half* of a moment whose other half already carries
/// the numbers (`RetryWaiting` beside `Retry`) and the intermediate setup
/// states. Anything unmapped panics, so a new event kind cannot quietly
/// enter this timeline.
fn milestone(event: &DiagnosticEvent) -> Option<Milestone> {
    match &event.payload {
        DiagnosticPayload::ReceiverTransition { to, .. } => match to {
            ReceiverLifecycle::Failed { .. } => Some(Milestone::ReceiverSetupFailed),
            ReceiverLifecycle::Connecting { .. } => Some(Milestone::RetryStarted),
            ReceiverLifecycle::Streaming { .. } => Some(Milestone::ReceiverRejoined),
            ReceiverLifecycle::RetryWaiting { .. }
            | ReceiverLifecycle::SettingUp { .. }
            | ReceiverLifecycle::Ready { .. } => None,
            other => panic!("unexpected lifecycle in the failure timeline: {other:?}"),
        },
        DiagnosticPayload::Retry { .. } => Some(Milestone::RetryScheduled),
        other => panic!("unexpected receiver-scoped diagnostic: {other:?}"),
    }
}

fn milestones(events: &[DiagnosticEvent]) -> Vec<Milestone> {
    events.iter().filter_map(milestone).collect()
}

/// Step 1. One secondary fails, waits out its backoff, and rejoins. The
/// diagnostic feed has to tell that story in order *and* say which session
/// each record belongs to -- an unordered pile of events with the same
/// generation on all of them is not a timeline.
#[tokio::test]
async fn one_failed_receiver_has_a_correlated_diagnostic_timeline() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    // Bring-up wrote receiver 2's first lifecycle already; the window this
    // test asserts on starts here.
    rig.forget_diagnostics();

    rig.fail_secondary(2).await;
    rig.recover(2).await;
    rig.advance_to_retry(2).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1, 2]))
        .await;

    let events = rig.diagnostics_for(2);
    assert_eq!(
        milestones(&events),
        vec![
            Milestone::ReceiverSetupFailed,
            Milestone::RetryScheduled,
            Milestone::RetryStarted,
            Milestone::ReceiverRejoined,
        ],
        "the timeline for receiver 2 was: {events:#?}"
    );

    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].session_generation <= pair[1].session_generation),
        "session generations went backwards along the timeline: {:?}",
        events
            .iter()
            .map(|event| event.session_generation)
            .collect::<Vec<_>>()
    );
    assert!(
        events
            .last()
            .expect("the timeline is not empty")
            .session_generation
            > events
                .first()
                .expect("the timeline is not empty")
                .session_generation,
        "the rejoin was reported under the same session generation as the failure: {:?}",
        events
            .iter()
            .map(|event| event.session_generation)
            .collect::<Vec<_>>()
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Every receiver-scoped record has to say which discovery generation was
/// live when it was written, or a report cannot tell a receiver that failed
/// while discovery was healthy from one that failed during a browse restart.
#[tokio::test]
async fn receiver_diagnostics_carry_the_live_discovery_generation() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    rig.forget_diagnostics();

    rig.fail_secondary(2).await;

    let live = rig.snapshot().discovery.generation;
    assert!(live > 0, "the fixture never started a browse generation");
    let events = rig.diagnostics_for(2);
    assert!(!events.is_empty(), "the failure produced no diagnostics");
    for event in &events {
        assert_eq!(
            event.discovery_generation, live,
            "a receiver diagnostic claimed discovery generation \
             {} while {live} was live: {event:#?}",
            event.discovery_generation
        );
    }

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Capture diagnostics are the only place a report can learn which capture
/// generation a drop or a silence gap belongs to. Reporting zero there makes
/// every generation look like the same one.
#[tokio::test]
async fn capture_diagnostics_carry_a_live_capture_generation() {
    let rig = RecoveryRig::active_on(&[1], 1, TestClock::new()).await;
    rig.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
        .await;

    let generations = rig.capture_generations();
    assert!(
        !generations.is_empty(),
        "capture never reported a state transition"
    );
    assert!(
        generations.iter().all(|generation| *generation > 0),
        "capture diagnostics reported generation zero: {generations:?}"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A session that starts, streams, and stops has to leave a record of each of
/// those edges, with the member count it committed with. Without them every
/// "what was the session doing?" question has to be answered by inference
/// from receiver rows.
#[tokio::test]
async fn session_phase_changes_are_reported_as_diagnostics() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;

    rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.wait_for(|snapshot| snapshot.session.active.is_empty())
        .await;

    let phases = rig.session_phase_reports();
    assert!(
        phases
            .iter()
            .any(|(phase, _)| matches!(phase, SessionPhase::Starting { .. })),
        "no session start was reported: {phases:?}"
    );
    assert!(
        phases.iter().any(
            |(phase, members)| matches!(phase, SessionPhase::Streaming { .. }) && *members == 2
        ),
        "no streaming phase with its member count was reported: {phases:?}"
    );
    assert!(
        phases
            .iter()
            .any(|(phase, members)| matches!(phase, SessionPhase::Stopped) && *members == 0),
        "no session stop was reported: {phases:?}"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The shutdown record has to say whether the teardown fit in its budget. A
/// duration alone leaves the reader to guess what the budget was.
///
/// The assertion runs one way only. "Timed out" is a fact the controller
/// observed and can only be true if a bounded drain really did exhaust its
/// budget, so `timed_out => duration >= budget` holds regardless of load.
/// The converse would not: four stages can add up past one budget on a busy
/// machine without any single one of them timing out.
///
/// The record is what made the teardown defect it was written for visible:
/// an idle backend used to report `timed_out == true` on every exit because
/// the session supervisor was never told to stop and its drain expired.
/// `shutdown_joins_every_worker_instead_of_waiting_out_its_budget` now holds
/// the other direction.
#[tokio::test]
async fn shutdown_reports_whether_it_stayed_within_budget() {
    let rig = RecoveryRig::active_on(&[1], 1, TestClock::new()).await;
    let feed = rig.diagnostic_feed();
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );

    let shutdown = backend_rig::drain_shutdown_report(feed);
    let (duration, timed_out) = shutdown.expect("shutdown emitted no completion record");
    assert!(
        !timed_out || duration >= SHUTDOWN_GROUP_BUDGET,
        "the record claims a timeout after only {duration:?}, which is less \
         than the {SHUTDOWN_GROUP_BUDGET:?} budget"
    );
}

/// Guards the redaction rule from the far side: no diagnostic may carry text
/// that looks like a key, a PIN, an address, or a Windows user path.
#[tokio::test]
async fn no_diagnostic_carries_a_secret_or_an_address() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    rig.fail_secondary(2).await;
    rig.recover(2).await;
    rig.advance_to_retry(2).await;

    let rendered = rig.rendered_diagnostics();
    assert!(!rendered.is_empty(), "no diagnostics were produced at all");
    for text in &rendered {
        let lowered = text.to_lowercase();
        for needle in [
            "pin=",
            "secret",
            "private",
            "192.168.",
            "10.0.",
            "c:\\users",
            "/users/",
            "bearer ",
            "password",
        ] {
            assert!(
                !lowered.contains(needle),
                "a diagnostic carried {needle:?}: {text}"
            );
        }
    }

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// A capture drop has to name the session it happened in. The capture
/// supervisor does not own the session counter -- it reads it from the shared
/// cell -- so a record written during generation three must say three, not
/// whatever number happened to be live when the process started.
#[tokio::test]
async fn capture_records_name_the_session_that_was_live() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    rig.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
        .await;

    // A membership change spends a session generation, so the number the
    // records have to carry is no longer the one the process started on.
    rig.set_desired(&[1]).await;
    rig.wait_for(|snapshot| snapshot.session.active == set_of(&[1]))
        .await;
    let live = rig
        .session_generation()
        .expect("a session is running after the membership change");
    assert!(
        live > 1,
        "the fixture never left the first session generation, so this test          could not tell a live counter from a hard-coded one"
    );

    let records = capture_records_from_an_endpoint_switch(&rig).await;
    assert_eq!(
        rig.session_generation(),
        Some(live),
        "the endpoint switch spent a session generation of its own, so the          records below would be compared against the wrong number"
    );
    for record in &records {
        assert_eq!(
            record.session_generation, live,
            "a capture record claimed session generation {} while {live} was              live: {record:#?}",
            record.session_generation
        );
    }

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Zero means "no session was live here". After a stop the counter has to
/// return to it, or a consumer cannot tell a record written during the last
/// session from one written after it ended -- and the capture field, which
/// already follows that rule, would mean the opposite of the session field in
/// the very same record.
#[tokio::test]
async fn records_written_after_a_stop_name_no_session() {
    let rig = RecoveryRig::active_on(&[1], 1, TestClock::new()).await;
    rig.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
        .await;

    rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.wait_for(|snapshot| snapshot.session.phase == SessionPhase::Stopped)
        .await;

    // Capture keeps running while the app is stopped, so this edge produces
    // records at a moment when no session exists at all.
    let records = capture_records_from_an_endpoint_switch(&rig).await;
    assert_eq!(
        rig.session_generation(),
        None,
        "a session came back during the window this test measures"
    );
    for record in &records {
        assert_eq!(
            record.session_generation, 0,
            "a record written while nothing was running claimed session              generation {}: {record:#?}",
            record.session_generation
        );
    }

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// The browse counter belongs to the discovery supervisor, and a record
/// written by a different supervisor has to name the browse that was actually
/// running -- not zero, which the contract reserves for "no browse had
/// started yet".
#[tokio::test]
async fn capture_records_name_the_live_browse_generation() {
    let rig = RecoveryRig::active_on(&[1], 1, TestClock::new()).await;
    rig.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
        .await;
    let live = rig.snapshot().discovery.generation;
    assert!(live > 0, "the fixture never started a browse generation");

    let records = capture_records_from_an_endpoint_switch(&rig).await;
    for record in &records {
        assert_eq!(
            record.discovery_generation, live,
            "a capture record claimed discovery generation {} while {live}              was live: {record:#?}",
            record.discovery_generation
        );
    }

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Step 5. Exit is a join, not a timeout.
///
/// The controller has to *end* every worker it started -- capture, session,
/// discovery, network -- and then join them. Signalling only some of them and
/// letting the rest be collected by a bounded drain that expires is not a
/// teardown: it spends the whole budget on every single exit and hides a real
/// stuck worker behind a timeout that always fires anyway.
///
/// `timed_out` is the load-independent half of this: it is a fact the
/// controller recorded about its own drains, not a wall-clock measurement, so
/// this assertion cannot be flipped by a busy machine.
#[tokio::test]
async fn shutdown_joins_every_worker_instead_of_waiting_out_its_budget() {
    let rig = RecoveryRig::active_on(&[1, 2], 1, TestClock::new()).await;
    let feed = rig.diagnostic_feed();
    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );

    let (duration, timed_out) =
        backend_rig::drain_shutdown_report(feed).expect("shutdown emitted no completion record");
    assert!(
        !timed_out,
        "the teardown expired a bounded drain instead of joining its workers \
         (reported duration {duration:?}, budget {SHUTDOWN_GROUP_BUDGET:?})"
    );
}

/// Step 4. Six failures, six domains, one run.
///
/// The point is not that each of them is survivable -- the earlier suites
/// prove that one at a time. It is that they are *independent*: the RAOP
/// service of a member flapping, the capture endpoint dying, the PCM queue
/// overflowing, a secondary dropping out, and a volume change must each leave
/// every other domain exactly where it was, and only losing the PTP primary
/// may spend a session generation. A backend that rebuilt everything on every
/// fault would pass six separate tests and fail this one.
///
/// Fake time throughout: every window here is policy, so the test states the
/// elapsed time and load never does.
#[tokio::test]
async fn independent_failure_domains_do_not_bleed_into_one_another() {
    let rig = RecoveryRig::active_on(&[1, 2, 3], 1, TestClock::new()).await;
    rig.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
        .await;
    let browse = rig.snapshot().discovery.generation;
    let session = rig
        .session_generation()
        .expect("the fixture brought a session up");
    assert!(browse > 0, "the fixture never started a browse generation");
    rig.forget_diagnostics();

    // 1. Only RAOP flaps: one member's service record disappears and returns
    //    while the member itself keeps answering.
    rig.flap_for(2, Duration::from_secs(5)).await;
    rig.rediscover_for(2, DISCOVERY_STABLE).await;
    assert_eq!(
        rig.snapshot().discovery.generation,
        browse,
        "a flapping service restarted the browse daemon"
    );
    assert_eq!(rig.active_members(), set_of(&[1, 2, 3]));
    assert_eq!(
        rig.session_generation(),
        Some(session),
        "a flapping service spent a session generation"
    );
    assert_eq!(rig.full_restart_count(), 0);

    // 2. Capture dies while the AirPlay session stays up. Silence has to
    //    bridge the gap: the decoder pair outlives the worker, so the group
    //    keeps its generation and its membership across the whole recovery.
    let captures_before = rig.capture_generations().len();
    rig.fail_capture().await;
    assert_eq!(
        rig.session_generation(),
        Some(session),
        "losing capture tore the AirPlay session down"
    );
    assert_eq!(rig.active_members(), set_of(&[1, 2, 3]));
    assert_eq!(
        rig.snapshot().session.audio_flow,
        AudioFlow::SilenceBridged,
        "nothing bridged the capture gap"
    );
    assert_eq!(
        rig.snapshot().discovery.generation,
        browse,
        "losing capture restarted discovery"
    );
    rig.recover_capture().await;
    assert_eq!(rig.snapshot().session.audio_flow, AudioFlow::Live);
    assert_eq!(
        rig.session_generation(),
        Some(session),
        "capture coming back spent a session generation"
    );
    assert!(
        rig.capture_generations().len() > captures_before,
        "capture recovered without opening a new generation"
    );
    assert_eq!(rig.full_restart_count(), 0);

    // 3. The bounded PCM queue overflows. It has to shed and say so, and
    //    nothing outside the audio path may notice.
    let drops_before = rig.pcm_drops();
    rig.overflow_pcm(PCM_BURST).await;
    assert!(
        rig.pcm_drops() >= drops_before + PCM_BURST / 2,
        "the burst was not accounted as evicted: {drops_before} -> {}",
        rig.pcm_drops()
    );
    assert_eq!(
        rig.queue_overruns(),
        0,
        "a queue reported an occupancy above its own capacity"
    );
    assert!(
        rig.records()
            .iter()
            .any(|event| matches!(event.payload, DiagnosticPayload::SilenceBridge { .. })),
        "the bridge never reported pacing silence again after the burst"
    );
    assert_eq!(rig.session_generation(), Some(session));
    assert_eq!(rig.active_members(), set_of(&[1, 2, 3]));
    assert_eq!(rig.snapshot().discovery.generation, browse);
    assert_eq!(
        rig.snapshot().audio_source.state,
        AudioSourceState::Capturing
    );

    // 4. A secondary drops out. The healthy members keep streaming and the
    //    group is not rebuilt for it.
    rig.fail_secondary(3).await;
    assert_eq!(rig.active_members(), set_of(&[1, 2]));
    assert_eq!(rig.primary(), Some(backend_rig::rid(1)));
    assert_eq!(
        rig.session_generation(),
        Some(session),
        "a secondary loss spent a session generation"
    );
    assert_eq!(rig.full_restart_count(), 0);
    assert_eq!(
        rig.snapshot().audio_source.state,
        AudioSourceState::Capturing
    );
    assert_eq!(rig.snapshot().discovery.generation, browse);

    // 5. Master volume changes: a control-plane edit, not a lifecycle event.
    rig.send(BackendCommand::SetMasterVolume(
        Volume::new(0.4).expect("a valid master volume"),
    ))
    .await;
    rig.round_trip().await;
    rig.wait_until(|| {
        rig.volume_sends()
            .iter()
            .any(|(receiver, level)| *receiver == backend_rig::rid(1) && *level == 0.4)
    })
    .await;
    assert_eq!(
        rig.session_generation(),
        Some(session),
        "a volume change spent a session generation"
    );
    assert_eq!(rig.full_restart_count(), 0);

    // 6. The primary dies. This is the one loss that invalidates group
    //    timing, so this -- and only this -- rebuilds the session.
    rig.fail_primary(1).await;
    assert_eq!(rig.primary_loss_restarts(), 1);
    assert_eq!(
        rig.full_restart_count(),
        1,
        "exactly one of the six failures may restart the group"
    );
    assert_eq!(rig.active_members(), set_of(&[2]));
    assert!(
        rig.session_generation()
            .is_some_and(|generation| generation > session),
        "the rebuilt group kept the failed generation's number"
    );
    assert_eq!(
        rig.snapshot().discovery.generation,
        browse,
        "the primary loss restarted the browse daemon as well"
    );
    assert_eq!(
        rig.snapshot().audio_source.state,
        AudioSourceState::Capturing
    );
    assert_eq!(rig.queue_overruns(), 0);

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}

/// Calibration lifecycle edges the feed carried, oldest first.
fn calibration_states(records: &[DiagnosticEvent]) -> Vec<CalibrationApplyState> {
    records
        .iter()
        .filter_map(|event| match event.payload {
            DiagnosticPayload::CalibrationState(state) => Some(state),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_calibration_apply_is_only_reported_applied_once_its_restart_is_live() {
    // The apply itself can never say more than "pending": it persists the
    // profile and asks for a restart, and whether that restart carries the
    // new delays to a live session is a later, separate fact. A lifecycle
    // that stops at pending leaves the diagnostic record permanently
    // ambiguous about whether the user is hearing the alignment they set.
    let rig = RecoveryRig::active(&[1, 2], 1).await;
    rig.forget_diagnostics();
    let revision = rig.snapshot().desired_revision;

    rig.send(BackendCommand::Calibration(
        CalibrationCommand::ApplyCalibrationProfile {
            expected_desired_revision: revision,
            profile: CalibrationProfile {
                reference_receiver: Some(backend_rig::rid(1)),
                requested_relative_delay_ns: [(backend_rig::rid(2), 3_000_000_i64)]
                    .into_iter()
                    .collect(),
            },
        },
    ))
    .await;

    rig.wait_until(|| calibration_states(&rig.records()).len() >= 2)
        .await;

    assert_eq!(
        calibration_states(&rig.records()),
        vec![
            CalibrationApplyState::PendingRestart,
            CalibrationApplyState::Applied
        ],
        "pending at the apply, applied once a session built with it is live"
    );

    assert_eq!(
        rig.shutdown().await,
        0,
        "no transport may outlive the backend"
    );
}
