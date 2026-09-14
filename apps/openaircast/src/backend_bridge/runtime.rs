use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use homepod_cast::backend::controller::SHUTDOWN_GROUP_BUDGET;
use homepod_cast::backend::event::{BackendEvent, DiagnosticEventReceiver};
use homepod_cast::backend::{
    start_device_backend, BackendCommand, BackendConfig, BackendSendError, DeviceBackendHandle,
    DeviceBackendUpdates,
};
use homepod_cast::{
    DeviceSnapshot, DiagnosticsClock, DiagnosticsHandle, DiagnosticsRegistry,
    DiagnosticsSessionTracker, SessionDiagnosticsState, SessionStopReason, SystemDiagnosticsClock,
};
use tokio::sync::{broadcast, watch};

use crate::app::{AppEvent, ControllerEvent, DeviceSeam, DeviceShutdown, DiagnosticsReading};
use crate::app_handle::{AppEventSender, AppUnavailable};

use super::commands::{BackendPort, GenerationEcho};
use super::projection::{
    diagnostics_reading, project_event, project_live_diagnostics, EndpointDirectory, Projection,
    SessionEcho,
};

// ---------------------------------------------------------------------------
// The bridge thread
// ---------------------------------------------------------------------------

/// How long a refused shell event waits before the bridge offers it again.
///
/// Needed because the two halves are driven by different things. A refusal is
/// a fact about the *shell's* queue, and nothing on either backend feed has to
/// happen afterwards: a backend that has settled publishes no further
/// revision, so without a tick of its own the refused event would wait for an
/// edge that never comes and the window would stay out of step for the rest of
/// the run. Short enough to be invisible, long enough that a shell which is
/// genuinely behind is not hammered while it catches up.
const REPAIR_INTERVAL: Duration = Duration::from_millis(100);

/// How often the bridge re-reads the diagnostics registry.
///
/// The one permanent timer in this file, and it needs the justification
/// [`repair_delay`] refuses to give itself. The registry publishes into an
/// `ArcSwap` and offers its readers no wakeup at all, so a poll is the only
/// way its snapshot reaches the shell -- unlike a repair tick, which has
/// something to do only while something is retained. Twice a second is a
/// cadence for a page a human is reading, not a measurement rate, and a wake
/// that finds nothing new costs one atomic load and a comparison of three
/// scalars.
const DIAGNOSTICS_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Spacing of [`BridgeHandle::wait_until_done`]'s poll; matches the legacy
/// device service's own shutdown poll so the exit path has one cadence.
const DONE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Handle on the bridge thread, held by the shell for exactly one question.
///
/// [`Self::is_done`] is the shell's only evidence that the backend has
/// finished tearing down. Both feeds close when the backend control thread's
/// future is dropped -- it owns the only `watch::Sender` and the last
/// `broadcast::Sender` -- so "both feeds closed" is precisely "the controller
/// returned", which is what the shutdown coordinator's device stage waits for.
///
/// Deliberately not a join handle. The bridge thread ends on its own when both
/// feeds close, and a `Drop` that joined it would turn a backend that failed
/// to close its feeds into a process that never exits -- the very failure the
/// exit valve exists to bound.
pub(crate) struct BridgeHandle {
    done: Arc<AtomicBool>,
}

impl BridgeHandle {
    /// Whether both backend feeds have closed, i.e. the backend is gone.
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Waits up to `timeout` for [`Self::is_done`], reporting what it found.
    ///
    /// Returns as soon as the flag is set; the timeout only bounds the wait so
    /// a backend that never finishes cannot hold the exit open.
    pub(crate) fn wait_until_done(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.is_done() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return self.is_done();
            }
            std::thread::sleep(DONE_POLL_INTERVAL);
        }
    }
}

/// Starts the thread that joins the two halves of this module.
///
/// Built like [`crate::diagnostics::run_registry_worker`]: one named std
/// thread owning a `current_thread` runtime and a single `tokio::select!`
/// loop. Nothing here holds a lock across an `await` -- the shell generations
/// are three atomics plus a `Notify`, and the newest snapshot is cloned out of
/// the watch guard in one statement that ends before the next `await`.
pub(crate) fn spawn_backend_bridge(
    updates: DeviceBackendUpdates,
    echo: Arc<GenerationEcho>,
    endpoints: Arc<EndpointDirectory>,
    events: AppEventSender,
) -> BridgeHandle {
    let DeviceBackendUpdates {
        state,
        events: backend_events,
        // The support export's feed. It goes to the registry the bridge
        // thread owns -- never to the window, which was the condition written
        // here while this receiver was still being dropped. That condition
        // still holds: what the shell learns from the registry is three
        // scalars chosen one at a time in `diagnostics_reading`, and no value
        // carried on this feed reaches a renderer.
        diagnostics,
        session_diagnostics,
    } = updates;

    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let spawned = std::thread::Builder::new()
        .name("openaircast-backend-bridge".to_owned())
        .spawn(move || {
            run_bridge(
                state,
                backend_events,
                diagnostics,
                session_diagnostics,
                echo,
                endpoints,
                events,
                thread_done,
            )
        });

    if let Err(error) = spawned {
        // Same rule the shutdown coordinator applies to its own valve: a
        // thread the OS refuses must not turn Quit into a window that never
        // closes. Without a bridge nothing translates either way, so the
        // window will report a lost controller on the first command; reporting
        // the backend as already finished keeps the exit bounded instead of
        // spending the whole device budget waiting for a thread that does not
        // exist.
        tracing::error!("could not start the backend bridge ({error}); the window will report a lost controller");
        done.store(true, Ordering::SeqCst);
    }

    BridgeHandle { done }
}

/// Hands as much of the outbox to the shell as its bounded queue accepts.
///
/// `Busy` leaves the remainder in place. The shell queue is 256 deep, so a
/// refusal means the actor is momentarily behind rather than gone -- and
/// dropping the event would leave the window out of step with a backend that
/// has no reason to publish again.
///
/// `Closed` means there is no window left to be out of step with. The outbox
/// is discarded and the caller stops deriving; the loop keeps running only so
/// that `done` still reports the backend truthfully.
pub(super) fn flush(
    out: &AppEventSender,
    outbox: &mut VecDeque<ControllerEvent>,
    shell_gone: &mut bool,
) {
    while let Some(event) = outbox.front() {
        match out.try_send(AppEvent::Controller(event.clone())) {
            Ok(()) => {
                outbox.pop_front();
            }
            Err(AppUnavailable::Busy) => break,
            Err(AppUnavailable::Closed) => {
                *shell_gone = true;
                outbox.clear();
                break;
            }
        }
    }
}

/// How long the bridge may park before it offers a retained event again.
///
/// `None` while nothing is retained: a timer that ran on an empty outbox would
/// wake the thread ten times a second for the life of the app to discover it
/// has nothing to do.
///
/// Separated from the loop for the same reason as [`finished`]: forcing the
/// shell's queue to refuse a delivery from a *test* means winning a race
/// against the test's own reader, so a bridge that had lost the retry
/// altogether would still pass a thread-level test most of the time.
pub(super) fn repair_delay(retained: usize) -> Option<Duration> {
    (retained > 0).then_some(REPAIR_INTERVAL)
}

/// Whether the bridge has nothing left to do, as a decision separate from the
/// schedule that produces it.
///
/// It is here rather than spelled into the loop because the loop cannot prove
/// it. `tokio::select!` picks at random among simultaneously ready branches,
/// so a bridge that stopped one feed too early would still pass a
/// thread-level test most of the time -- and "most of the time" is exactly
/// what a shutdown contract must not be. The rule itself is total and has
/// four cases, so it is checked as four cases.
///
/// One surviving feed is not a finished backend: the shell's exit path reads
/// this through `done` to learn that the backend control thread has returned,
/// and it returns only when it has dropped *both* senders.
pub(super) fn finished(state_closed: bool, events_closed: bool) -> bool {
    state_closed && events_closed
}

/// Converts the last retained session binding into a fail-closed tombstone.
pub(super) fn failed_session_state(state: &SessionDiagnosticsState) -> SessionDiagnosticsState {
    let generation = match state {
        SessionDiagnosticsState::Inactive { generation, .. }
        | SessionDiagnosticsState::Active { generation, .. } => *generation,
    };
    SessionDiagnosticsState::Inactive {
        generation,
        reason: SessionStopReason::Failed,
    }
}

/// Starts the diagnostics registry, or reports that it could not be started.
///
/// The registry `expect`s its own thread spawn, and a panic here would leave
/// `done` unset for the whole device budget on every exit -- the exact
/// failure [`spawn_backend_bridge`] already bounds for its own thread, with
/// the same reasoning. So the panic is caught and the bridge runs on without
/// a registry; the Diagnostics page then draws every registry tile marked
/// unmeasured wherever it draws them at all, which is what "no measurements
/// exist" is supposed to look like. (While nothing is running it draws its
/// empty state instead, as it does with a healthy registry -- that is the
/// page's answer to an idle window, not a claim about the registry.)
fn start_registry(
    feed: DiagnosticEventReceiver,
) -> Option<(DiagnosticsRegistry, DiagnosticsHandle)> {
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let clock: Arc<dyn DiagnosticsClock> = Arc::new(SystemDiagnosticsClock::new());
        DiagnosticsRegistry::start(feed, clock)
    }));
    match started {
        Ok(started) => Some(started),
        Err(_) => {
            tracing::error!(
                "the diagnostics registry could not be started; the Diagnostics page \
                 will report that nothing has been measured"
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_bridge(
    mut state: watch::Receiver<Arc<DeviceSnapshot>>,
    mut backend_events: broadcast::Receiver<BackendEvent>,
    diagnostics: DiagnosticEventReceiver,
    mut session_diagnostics: watch::Receiver<SessionDiagnosticsState>,
    echo: Arc<GenerationEcho>,
    endpoints: Arc<EndpointDirectory>,
    out: AppEventSender,
    done: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Same reasoning as the failed spawn above.
            tracing::error!("backend bridge runtime unavailable: {error}");
            done.store(true, Ordering::SeqCst);
            return;
        }
    };

    // Owned by this thread, and therefore dropped when the block below
    // returns -- which is what shuts the registry down: its `Drop` signals
    // the worker and joins it. Started after the runtime, so a runtime that
    // could not be built leaves no registry thread behind.
    let registry = start_registry(diagnostics);

    runtime.block_on(async move {
        let diagnostics = registry.as_ref().map(|(_, handle)| handle.clone());
        let mut diagnostics_sessions = DiagnosticsSessionTracker::default();
        // The newest reading offered to the shell. The registry republishes
        // on every diagnostic event it ingests and almost none of them move
        // these three values, so an unchanged reading is not published.
        let mut published: Option<DiagnosticsReading> = None;
        let mut published_live: Option<crate::app::LiveDiagnosticsReading> = None;
        let mut next_live_poll = std::time::Instant::now();
        let mut registry_stamp: Option<(u64, std::time::Instant)> = None;
        let mut projection = Projection::default();
        // At most one pass's worth of events: a new pass is derived only once
        // the previous one has been delivered, so this cannot grow with a
        // shell that stays behind.
        let mut outbox: VecDeque<ControllerEvent> = VecDeque::new();
        let mut state_closed = false;
        let mut events_closed = false;
        let mut session_diagnostics_closed = false;
        let mut closure_reported = false;
        let mut shell_gone = false;

        loop {
            flush(&out, &mut outbox, &mut shell_gone);

            if !session_diagnostics_closed {
                if let Some((registry, _)) = registry.as_ref() {
                    let state = session_diagnostics.borrow_and_update().clone();
                    diagnostics_sessions.reconcile(registry, &state);
                }
            }

            // Level-driven: every pass re-derives the whole answer from the
            // newest snapshot. Held back while the outbox is not empty, which
            // is what keeps a refusal from turning into a duplicate.
            if outbox.is_empty() && !shell_gone {
                // One statement: the watch guard is a read lock and is gone
                // before the next `await`.
                let snapshot = Arc::clone(&state.borrow_and_update());
                let (next, produced) = projection.project(&snapshot, echo.sample());
                echo.observe_listening_target(&snapshot, &produced);
                projection = next;
                // Published before the events that mention the keys, not
                // after: the window may act on an endpoint the moment it sees
                // it, and a key the command half cannot resolve yet would be
                // dropped. One synchronous statement, no `await` inside it.
                endpoints.publish(&projection.endpoints);
                outbox.extend(produced);
                outbox.extend(echo.group_events(&snapshot, state_closed));
                outbox.extend(echo.volume_events(&snapshot, state_closed));
                outbox.extend(echo.listening_events(&snapshot, state_closed));
                if state_closed && !closure_reported {
                    outbox.push_back(ControllerEvent::ChannelClosed {
                        summary: "the audio controller stopped".into(),
                    });
                    closure_reported = true;
                }
                flush(&out, &mut outbox, &mut shell_gone);
            }

            // Level-driven for the same reason as the pass above: the answer
            // is whatever the registry has published, re-derived each time,
            // and held back while the outbox is not empty so a refusal cannot
            // turn into a duplicate here either.
            if outbox.is_empty() && !shell_gone {
                if let Some(handle) = &diagnostics {
                    let registry_snapshot = handle.snapshot();
                    let reading = diagnostics_reading(&registry_snapshot);
                    if published != Some(reading) {
                        published = Some(reading);
                        outbox.push_back(ControllerEvent::DiagnosticsUpdated { reading });
                        flush(&out, &mut outbox, &mut shell_gone);
                    }
                    let now = std::time::Instant::now();
                    let seen = echo.sample();
                    if now >= next_live_poll
                        || published_live.as_ref().map(|r| r.generation) != Some(seen.session)
                    {
                        next_live_poll = now + Duration::from_millis(250);
                        if registry_stamp.as_ref().map(|(sequence, _)| *sequence)
                            != Some(registry_snapshot.snapshot_sequence)
                        {
                            registry_stamp = Some((registry_snapshot.snapshot_sequence, now));
                        }
                        let fresh = registry_stamp.is_some_and(|(_, stamp)| {
                            now.duration_since(stamp) < Duration::from_secs(2)
                        });
                        let backend = Arc::clone(&state.borrow());
                        let ready = !state_closed
                            && projection
                                .session
                                .as_ref()
                                .is_some_and(|(generation, status)| {
                                    *generation == seen.session
                                        && matches!(
                                            status,
                                            SessionEcho::Active(_) | SessionEcho::Degraded(_)
                                        )
                                });
                        let registration = ready
                            .then(|| diagnostics_sessions.active_registration())
                            .flatten();
                        let live = project_live_diagnostics(
                            &backend,
                            fresh.then_some(&registry_snapshot),
                            seen.session,
                            registration,
                        );
                        if published_live.as_ref() != Some(&live) {
                            published_live = Some(live.clone());
                            outbox.push_back(ControllerEvent::LiveDiagnosticsUpdated {
                                reading: live,
                            });
                            flush(&out, &mut outbox, &mut shell_gone);
                        }
                    }
                }
            }

            if finished(state_closed, events_closed) {
                // Backend teardown is done even if its last shell notification
                // must still wait for space. Keep repairing that delivery.
                done.store(true, Ordering::SeqCst);
                if outbox.is_empty() || shell_gone {
                    break;
                }
            }

            // Copies rather than the flags themselves: the branch handlers
            // below write the flags, and a branch future borrowing them would
            // not compile.
            let state_live = !state_closed;
            let events_live = !events_closed;
            let session_diagnostics_live = !session_diagnostics_closed;
            let repair = repair_delay(outbox.len());
            // Only while there is a registry to read. Without one this branch
            // is `pending` and the thread parks exactly as it did before.
            let registry_poll = diagnostics.as_ref().map(|_| DIAGNOSTICS_POLL_INTERVAL);

            tokio::select! {
                changed = async {
                    if state_live {
                        state.changed().await
                    } else {
                        std::future::pending::<Result<(), watch::error::RecvError>>().await
                    }
                } => {
                    if changed.is_err() {
                        state_closed = true;
                    }
                }
                received = async {
                    if events_live {
                        backend_events.recv().await
                    } else {
                        std::future::pending::<
                            Result<BackendEvent, broadcast::error::RecvError>,
                        >().await
                    }
                } => match received {
                    Ok(event) => {
                        if let Some(translated) = project_event(&event, echo.sample()) {
                            outbox.push_back(translated);
                        }
                    }
                    // Bounded and lossy by construction, and the snapshot is
                    // authoritative for state: a gap costs the entries, never
                    // the bridge. Treating it as a closed feed would silently
                    // stop translating notices for the rest of the run.
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "the backend event feed lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        events_closed = true;
                    }
                },
                changed = async {
                    if session_diagnostics_live {
                        session_diagnostics.changed().await
                    } else {
                        std::future::pending::<Result<(), watch::error::RecvError>>().await
                    }
                } => {
                    if changed.is_err() {
                        session_diagnostics_closed = true;
                        if let Some((registry, _)) = registry.as_ref() {
                            let failed = failed_session_state(&session_diagnostics.borrow());
                            diagnostics_sessions.reconcile(registry, &failed);
                        }
                    }
                }
                // A shell command that moves no backend state -- a refresh, a
                // start, a stop -- reaches the bridge only this way.
                () = echo.wait() => {}
                // Offers a retained event again; see `repair_delay`.
                () = async {
                    match repair {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {}
                // Re-reads the registry; see `DIAGNOSTICS_POLL_INTERVAL`.
                () = async {
                    match registry_poll {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {}
            }
        }

        done.store(true, Ordering::SeqCst);
    });
}

// ---------------------------------------------------------------------------
// The switch
// ---------------------------------------------------------------------------

/// What the shell's shutdown coordinator must allow its device stage once the
/// backend is behind it.
///
/// Derived, never stated. The backend gives its own teardown
/// [`SHUTDOWN_GROUP_BUDGET`] -- itself derived from the session supervisor's
/// group teardown -- and only *then* returns from its control thread and drops
/// the feeds this bridge watches. A device stage shorter than that would
/// report a timeout on every exit with one unreachable receiver and walk on
/// while the backend was still disconnecting; the coordinator's `exit_valve`
/// is derived from the budgets in force, so raising this raises the valve with
/// it rather than leaving the valve to fire inside the teardown it guards.
///
/// The margin on top is not a second guess at the teardown. The backend's
/// budget is measured from the moment it *begins* tearing down, and two things
/// happen outside it: getting the shutdown command into a queue that may
/// briefly be full, and one bridge wakeup afterwards to observe both feeds
/// closed and publish `done`. Neither is bounded by the backend's number, so
/// neither may be taken out of it.
const SWITCH_SHUTDOWN_SLACK: Duration = Duration::from_secs(1);

pub(crate) const DEVICE_SHUTDOWN_BUDGET: Duration =
    SHUTDOWN_GROUP_BUDGET.saturating_add(SWITCH_SHUTDOWN_SLACK);

/// The device stage of the bounded exit, with the backend behind it.
struct BackendShutdown {
    handle: DeviceBackendHandle,
    bridge: BridgeHandle,
}

impl DeviceShutdown for BackendShutdown {
    fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;

        // `Busy` is not a refusal: the actor drains its whole queue on every
        // wake, so the command lands as soon as there is room. Retried rather
        // than reported, because a shutdown that was merely queued behind a
        // slider burst is not a shutdown that failed.
        loop {
            match self.handle.try_send(BackendCommand::Shutdown) {
                // Closed: the actor is already gone, so there is nothing left
                // to ask. Whether it *finished* is still the bridge's answer.
                Ok(_) | Err(BackendSendError::Closed) => break,
                Err(BackendSendError::Busy) => {
                    if std::time::Instant::now() >= deadline {
                        return self.bridge.is_done();
                    }
                    std::thread::sleep(DONE_POLL_INTERVAL);
                }
            }
        }

        // Waits for evidence, not for a duration: both feeds close when the
        // backend control thread returns, which is the only thing that proves
        // its supervisors were torn down rather than abandoned.
        self.bridge
            .wait_until_done(deadline.saturating_duration_since(std::time::Instant::now()))
    }
}

/// Wires one already-started backend onto the shell's device seam.
///
/// Split out from [`backend_device_seam`] so the wiring can be asserted
/// against hand-built channels: everything below is pure plumbing, and the
/// only thing the caller adds is a backend that opens sockets, browses the
/// network, and takes the audio endpoint.
///
/// One [`GenerationEcho`] is shared by both halves, and that sharing is the
/// whole switch: the port records what the user asked for, the bridge answers
/// under exactly that generation, and the reducer's guards line up.
pub(super) fn device_seam_from(
    handle: DeviceBackendHandle,
    updates: DeviceBackendUpdates,
    events: AppEventSender,
) -> DeviceSeam {
    let echo = Arc::new(GenerationEcho::new());
    let endpoints = Arc::new(EndpointDirectory::default());
    let bridge = spawn_backend_bridge(updates, Arc::clone(&echo), Arc::clone(&endpoints), events);
    DeviceSeam {
        controller: Arc::new(BackendPort::new(handle.clone(), echo, endpoints)),
        shutdown: Arc::new(BackendShutdown { handle, bridge }),
        budget: DEVICE_SHUTDOWN_BUDGET,
    }
}

/// Starts the resilience backend and hands the shell its device seam.
///
/// Reached through [`crate::app::DeviceStage::Backend`];
/// [`crate::device_service::legacy_device_seam`] is the other arm and the way
/// back. Which one the app runs is [`crate::app::ACTIVE_DEVICE_STAGE`], and
/// two tests there pin it.
/// Picks the legacy volume file this installation actually has.
///
/// Two of them shipped, and naming either one alone loses the other:
///
/// * `%APPDATA%\HomePodCast\volume.txt` -- written by `cast.rs::volume_file`
///   in every build up to `33521c2`;
/// * `%APPDATA%\volume.txt` -- written by
///   [`crate::device_service::legacy_volume_file`] since that refactor, which
///   moved the file up one directory while its own doc comment kept claiming
///   the old location.
///
/// This is the last launch that can read either. The backend migrates once
/// and then owns volume in `state-v1.json`, so a source that resolves to a
/// file the user never had silently hands them the default volume and makes
/// that permanent on the first backend write.
///
/// The newer writer wins when both exist: it is the one the last launch
/// updated, so the older file is by construction stale. When neither exists
/// the newer path is returned unchanged -- the migration finds nothing either
/// way, and the backend falls back to its own default.
pub(crate) fn legacy_volume_source(appdata_dir: &std::path::Path) -> std::path::PathBuf {
    let current = crate::device_service::legacy_volume_file(appdata_dir);
    if current.exists() {
        return current;
    }

    let historical = appdata_dir.join("HomePodCast").join("volume.txt");
    if historical.exists() {
        return historical;
    }

    current
}

/// The filesystem layout the shell hands the backend.
///
/// Split out of [`backend_device_seam`] so the one decision in it that can be
/// wrong without anything failing -- which file the volume migration reads --
/// is reachable from a test. The seam itself starts sockets and takes the
/// audio endpoint, so nothing below it can be asserted.
///
/// The same layout as `BackendConfig::for_current_user`, but rooted at the
/// folder `run()` has already validated rather than read from the environment
/// a second time: that constructor falls back to the OS temporary directory
/// when `APPDATA` is missing, which would silently put device state somewhere
/// the next launch does not look.
pub(crate) fn backend_config_for(appdata_dir: &std::path::Path) -> BackendConfig {
    BackendConfig {
        state_directory: appdata_dir.join("OpenAirCast"),
        legacy_volume_path: legacy_volume_source(appdata_dir),
    }
}

pub(crate) fn backend_device_seam(
    events: AppEventSender,
    appdata_dir: std::path::PathBuf,
) -> anyhow::Result<DeviceSeam> {
    let (handle, updates) = start_device_backend(backend_config_for(&appdata_dir))?;
    Ok(device_seam_from(handle, updates, events))
}
