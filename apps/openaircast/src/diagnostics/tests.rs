use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use airplay_client::ClientDiagnosticsSource;
use airplay_core::DeviceId;

use crate::backend::model::ReceiverId;

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
        snapshot.active_session_id.is_some_and(|id| id != first) && snapshot.receivers.len() == 2
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
