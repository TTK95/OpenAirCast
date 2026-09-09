//! Shared full-stack backend test harness.
//!
//! Extracted from `tests/session_recovery.rs` so the session-recovery suite
//! and the system-recovery suite drive ONE rig instead of two divergent ones.
//! It implements only public seams, so unlike `tests/support/mod.rs` it can be
//! included from an integration-test binary.
//!
//! Each including binary uses a different subset, so unused items are expected.

#![allow(dead_code)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airplay_audio::LiveAudioDecoder;
use airplay_client::{GroupRuntimeEvent, Health, MemberFailure, MemberResult};
use airplay_core::{Device, DeviceId, Features, Version};
use airplay_discovery::BrowseEvent;
use homepod_cast::backend::capture::{CaptureError, CaptureSource, CaptureWorker};
use homepod_cast::backend::command::{BackendCommand, SystemEvent};
use homepod_cast::backend::controller::RESUME_SETTLE;
use homepod_cast::backend::controller::{
    start_device_backend_with, BackendConfig, BackendDiscoverySource, BackendOverrides,
    TestStartGate,
};
use homepod_cast::backend::model::{
    AudioEndpoint, AudioEndpointPreference, AudioSourceState, DeviceSnapshot, LatencyConfig,
    ReceiverId, ReceiverLifecycle, RestartReason, RunIntent, SavedGroupId, SavedGroupMember,
    SessionPhase, UserFacingError, Volume,
};
use homepod_cast::backend::network::{
    BindingFingerprint, LocalBindingSource, NetworkChangeSource, NetworkError, NetworkSubscription,
    NETWORK_SETTLE,
};
use homepod_cast::backend::persistence::{FileStore, LoadOutcome, PersistError, StateStore};
use homepod_cast::backend::session::{
    SessionRequest, SessionStartFailure, SessionTransport, SessionTransportFactory,
};
use homepod_cast::backend::{
    BackendEvent, DeviceBackendHandle, DeviceBackendUpdates, DiagnosticEvent,
    DiagnosticEventReceiver, DiagnosticPayload, PersistedStateV1, SystemTransition,
};
use tokio::sync::{broadcast, mpsc, watch};

/// Wall-clock bound on every "did the backend get there?" poll in this rig.
///
/// It never encodes an expected duration -- every window under test runs on
/// the injected clock -- and it is only ever reached on the failure path,
/// where its only job is to turn a hung backend into a readable panic instead
/// of a hung test. It is therefore set generously: on a machine whose cores
/// are oversubscribed, every step between a command and the snapshot it
/// produces stretches with the scheduler. A tight bound here would not detect
/// anything a loose one misses; it would only fail correct runs under load,
/// which is the one thing a test may never do.
const LIVENESS_DEADLINE: Duration = Duration::from_secs(60);

/// Deterministic receiver identity for the given seed byte.
pub fn rid(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
        .expect("fixed seed forms a valid storage key")
}

/// Stable device identity matching [`rid`] for the same seed.
pub fn device_id(seed: u8) -> DeviceId {
    DeviceId([0, 0, 0, 0, 0, seed])
}

/// A castable discovered-device fixture with a fixed identity.
pub fn device(seed: u8) -> Device {
    Device {
        id: device_id(seed),
        name: format!("Receiver {seed}"),
        model: "TestModel".to_owned(),
        manufacturer: None,
        serial_number: None,
        addresses: vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            192, 168, 1, seed,
        ))],
        port: 7000,
        features: Features(1 << 40),
        required_sender_features: None,
        public_key: None,
        source_version: Version::new(366, 0, 0),
        firmware_version: None,
        os_version: None,
        protocol_version: None,
        requires_password: false,
        status_flags: 0,
        access_control: None,
        pairing_identity: None,
        system_pairing_identity: None,
        bluetooth_address: None,
        homekit_home_id: None,
        group_id: None,
        is_group_leader: false,
        group_public_name: None,
        group_contains_discoverable_leader: false,
        home_group_id: None,
        household_id: None,
        parent_group_id: None,
        parent_group_contains_discoverable_leader: false,
        tight_sync_id: None,
        raop_port: None,
        raop_encryption_types: None,
        raop_codecs: None,
        raop_transport: None,
        raop_metadata_types: None,
        raop_digest_auth: false,
        vodka_version: None,
    }
}

pub fn desired(seeds: &[u8]) -> Vec<Device> {
    seeds.iter().map(|seed| device(*seed)).collect()
}

pub fn set_of(seeds: &[u8]) -> BTreeSet<ReceiverId> {
    seeds.iter().map(|seed| rid(*seed)).collect()
}

pub fn reconcile(generation: u64, seeds: &[u8]) -> SessionRequest {
    SessionRequest::Reconcile {
        generation,
        desired: desired(seeds),
        preferred_primary: None,
        latency: LatencyConfig {
            sender_buffer_ms: 2_000,
            render_delay_ms: 200,
        },
        calibration: homepod_cast::calibration::CalibrationProfile::default(),
        // No caller passes an empty set; if one ever does, it means the
        // teardown the controller sends for a stopped app.
        teardown_only: seeds.is_empty(),
    }
}

/// Truthful transport leak accounting across the supervisor lifecycle.
#[derive(Clone, Default)]
pub struct LeakCounter {
    pub created: Arc<AtomicUsize>,
    pub dropped: Arc<AtomicUsize>,
}

impl LeakCounter {
    pub fn leaked(&self) -> usize {
        self.created.load(Ordering::SeqCst) - self.dropped.load(Ordering::SeqCst)
    }
}

/// Health and membership script shared by the factory and every transport it
/// hands out. Tests mutate it; the running backend observes the change on the
/// next probe cycle or at the next session generation.
#[derive(Default)]
pub struct RecoveryScriptInner {
    /// Receivers currently scripted to time out on every probe.
    pub unhealthy: Mutex<BTreeSet<ReceiverId>>,
    /// Desired sets handed to the factory, oldest first.
    pub started: Mutex<Vec<BTreeSet<ReceiverId>>>,
    /// The same starts as the factory actually received them, oldest first:
    /// the desired members in the order they arrived, plus the preference
    /// that travelled with them.
    ///
    /// Separate from `started` because that one is a set and a set cannot
    /// answer an ordering question. A determinism test needs to see the
    /// request the controller *built*, not a normalized view of it.
    pub requests: Mutex<Vec<(Vec<ReceiverId>, Option<ReceiverId>)>>,
    /// Wall time one probe cycle costs; models a slow or silent group.
    pub probe_delay: Mutex<Duration>,
    /// Completed probe cycles; lets a test wait for a cadence to take effect.
    pub probes: AtomicUsize,
    /// Effective volumes handed to the transport, oldest first.
    ///
    /// Recorded before the scripted outcome is decided, so a rejected request
    /// still proves the backend attempted that receiver.
    pub volumes: Mutex<Vec<(ReceiverId, f32)>>,
    /// Receivers whose volume control rejects every request.
    pub volume_failures: Mutex<BTreeSet<ReceiverId>>,
    /// While set, the transport stops consuming PCM.
    ///
    /// A group that always keeps up can never be made to overflow on purpose:
    /// whether a burst evicts anything would depend on how often the probe
    /// cycle happened to run during it, which is a property of the machine's
    /// load and not of the code under test. Holding the consumer still makes
    /// the overflow a fact instead of a race.
    pub drain_paused: AtomicBool,
    pub leaks: LeakCounter,
}

#[derive(Clone, Default)]
pub struct RecoveryScript {
    pub inner: Arc<RecoveryScriptInner>,
}

impl RecoveryScript {
    pub fn fail_probe(&self, receiver: ReceiverId) {
        self.inner
            .unhealthy
            .lock()
            .expect("unhealthy set poisoned")
            .insert(receiver);
    }

    pub fn heal_probe(&self, receiver: ReceiverId) {
        self.inner
            .unhealthy
            .lock()
            .expect("unhealthy set poisoned")
            .remove(&receiver);
    }

    pub fn set_probe_delay(&self, delay: Duration) {
        *self.inner.probe_delay.lock().expect("probe delay poisoned") = delay;
    }

    pub fn probe_delay(&self) -> Duration {
        *self.inner.probe_delay.lock().expect("probe delay poisoned")
    }

    pub fn probe_count(&self) -> usize {
        self.inner.probes.load(Ordering::SeqCst)
    }

    pub fn unhealthy(&self) -> BTreeSet<ReceiverId> {
        self.inner
            .unhealthy
            .lock()
            .expect("unhealthy set poisoned")
            .clone()
    }

    pub fn started(&self) -> Vec<BTreeSet<ReceiverId>> {
        self.inner
            .started
            .lock()
            .expect("started log poisoned")
            .clone()
    }

    /// Session starts as the factory received them, oldest first.
    pub fn requests(&self) -> Vec<(Vec<ReceiverId>, Option<ReceiverId>)> {
        self.inner
            .requests
            .lock()
            .expect("request log poisoned")
            .clone()
    }

    /// Scripts every future volume request to `receiver` as rejected.
    pub fn fail_volume(&self, receiver: ReceiverId) {
        self.inner
            .volume_failures
            .lock()
            .expect("volume failures poisoned")
            .insert(receiver);
    }

    /// Stops the transport consuming PCM until [`Self::resume_drain`].
    pub fn pause_drain(&self) {
        self.inner.drain_paused.store(true, Ordering::SeqCst);
    }

    /// Lets the transport consume PCM again.
    pub fn resume_drain(&self) {
        self.inner.drain_paused.store(false, Ordering::SeqCst);
    }

    /// Effective volumes handed to the transport, oldest first.
    pub fn volumes(&self) -> Vec<(ReceiverId, f32)> {
        self.inner
            .volumes
            .lock()
            .expect("volume log poisoned")
            .clone()
    }
}

/// Transport whose membership was fixed when its generation started and whose
/// probe answers follow the shared script.
pub struct RecoveryTransport {
    pub primary: Option<ReceiverId>,
    pub members: BTreeSet<ReceiverId>,
    pub script: RecoveryScript,
    /// This generation's consumer of the stable PCM queue.
    ///
    /// A transport that never drains it is not a stand-in for an AirPlay
    /// group: the bounded queue would sit permanently full, every eviction
    /// would fall inside one endless run, and the bridge -- which reports
    /// only the edges of a run -- could never report an overflow again after
    /// the first. Draining on the probe cycle is what makes the PCM domain
    /// observable at all.
    pub decoder: LiveAudioDecoder,
}

impl Drop for RecoveryTransport {
    fn drop(&mut self) {
        self.script
            .inner
            .leaks
            .dropped
            .fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl SessionTransport for RecoveryTransport {
    fn primary(&self) -> Option<ReceiverId> {
        self.primary
    }

    fn active_members(&self) -> BTreeSet<ReceiverId> {
        self.members.clone()
    }

    fn setup_failures(&mut self) -> Vec<MemberFailure> {
        Vec::new()
    }

    fn try_runtime_event(
        &mut self,
    ) -> Result<Option<GroupRuntimeEvent>, homepod_cast::backend::session::RuntimeEventLag> {
        Ok(None)
    }

    async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
        // Consume whatever the bridge produced since the last cycle, the way
        // a live group's packet scheduler would -- unless a test is
        // deliberately holding the consumer still to force an overflow.
        if !self.script.inner.drain_paused.load(Ordering::SeqCst) {
            while matches!(self.decoder.decode_frame(), Ok(Some(_))) {}
        }
        let delay = self.script.probe_delay();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.script.inner.probes.fetch_add(1, Ordering::SeqCst);
        let unhealthy = self.script.unhealthy();
        self.members
            .iter()
            .map(|receiver| MemberResult {
                receiver: (*receiver).into(),
                result: Ok(if unhealthy.contains(receiver) {
                    Health::TimedOut
                } else {
                    Health::Healthy
                }),
            })
            .collect()
    }

    async fn set_member_volume(&mut self, receiver: &ReceiverId, volume: f32) -> MemberResult<()> {
        self.script
            .inner
            .volumes
            .lock()
            .expect("volume log poisoned")
            .push((*receiver, volume));
        let rejected = self
            .script
            .inner
            .volume_failures
            .lock()
            .expect("volume failures poisoned")
            .contains(receiver);
        MemberResult {
            receiver: (*receiver).into(),
            result: if rejected {
                Err(MemberFailure {
                    receiver: (*receiver).into(),
                    phase: airplay_client::SetupPhase::StartAudio,
                    retryable: true,
                    source: airplay_core::Error::Rtsp(airplay_core::error::RtspError::SetupFailed(
                        "scripted volume rejection".into(),
                    )),
                })
            } else {
                Ok(())
            },
        }
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Factory electing the preferred primary while it is still healthy, otherwise
/// the lexicographically lowest healthy member of the desired set.
pub struct RecoveryFactory {
    pub script: RecoveryScript,
}

#[async_trait::async_trait]
impl SessionTransportFactory for RecoveryFactory {
    async fn start(
        &self,
        desired: Vec<Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        _latency: LatencyConfig,
        _calibration: homepod_cast::calibration::CalibrationProfile,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
        let requested: BTreeSet<ReceiverId> = desired
            .iter()
            .map(|device| ReceiverId::from(device.id.clone()))
            .collect();
        self.script
            .inner
            .started
            .lock()
            .expect("started log poisoned")
            .push(requested.clone());
        // Recorded before the outcome is decided and in the same place as
        // `started`, so the two logs always have the same length -- a failed
        // start is still a request the controller made.
        self.script
            .inner
            .requests
            .lock()
            .expect("request log poisoned")
            .push((
                desired
                    .iter()
                    .map(|device| ReceiverId::from(device.id.clone()))
                    .collect(),
                preferred_primary,
            ));

        let unhealthy = self.script.unhealthy();
        let members: BTreeSet<ReceiverId> = requested
            .iter()
            .copied()
            .filter(|receiver| !unhealthy.contains(receiver))
            .collect();
        let elected = preferred_primary
            .filter(|candidate| members.contains(candidate))
            .or_else(|| members.iter().next().copied());
        let Some(primary) = elected else {
            return Err(SessionStartFailure {
                error: UserFacingError::new("no healthy receiver remained"),
                failures: Vec::new(),
            });
        };
        self.script
            .inner
            .leaks
            .created
            .fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecoveryTransport {
            primary: Some(primary),
            members,
            script: self.script.clone(),
            decoder,
        }))
    }
}

/// One fallible browse-stream item as produced by the discovery daemon.
pub type BrowseItem = std::result::Result<BrowseEvent, airplay_core::error::DiscoveryError>;

/// Browse-stream ledger: one registration opened per generation, one closed
/// when the supervisor drops it. A difference is a leaked mDNS daemon.
#[derive(Clone, Default)]
pub struct StreamLedger {
    pub opened: Arc<AtomicUsize>,
    pub closed: Arc<AtomicUsize>,
}

impl StreamLedger {
    /// Browse registrations opened since the backend started.
    pub fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    pub fn leaked(&self) -> usize {
        self.opened.load(Ordering::SeqCst) - self.closed.load(Ordering::SeqCst)
    }
}

/// Counts one browse registration for as long as its stream is alive.
struct StreamTicket(StreamLedger);

impl Drop for StreamTicket {
    fn drop(&mut self) {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
    }
}

/// Discovery double replaying one scripted stream and then staying open.
#[derive(Default)]
pub struct RecoveryDiscoverySource {
    pub script: Mutex<VecDeque<Vec<BrowseItem>>>,
    pub keepalive: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<BrowseItem>>>,
    /// Receiver seeds every generation past the scripted ones re-announces.
    /// Empty keeps the historical behaviour: later browses announce nothing.
    pub standing: Mutex<Vec<u8>>,
    pub streams: StreamLedger,
}

impl RecoveryDiscoverySource {
    /// Pushes one further browse event into every open generation, the way a
    /// live daemon reports an mDNS record expiring mid-session.
    pub fn push(&self, event: BrowseEvent) {
        self.keepalive
            .lock()
            .expect("discovery keepalive poisoned")
            .retain(|tx| tx.send(Ok(event.clone())).is_ok());
    }

    /// Faults every open generation, the way a daemon dying mid-browse does.
    pub fn push_failure(&self) {
        self.keepalive
            .lock()
            .expect("discovery keepalive poisoned")
            .retain(|tx| {
                tx.send(Err(airplay_core::error::DiscoveryError::Daemon(
                    "scripted daemon failure".to_owned(),
                )))
                .is_ok()
            });
    }

    /// Makes every generation after the scripted ones re-announce `seeds`.
    ///
    /// A browse restart clears the supervisor's inventory, so without this a
    /// rig that restarts discovery on purpose would silently drift into a
    /// world where nothing is discoverable any more.
    pub fn announce_from_now_on(&self, seeds: &[u8]) {
        *self.standing.lock().expect("discovery standing poisoned") = seeds.to_vec();
    }
}

#[async_trait::async_trait]
impl BackendDiscoverySource for RecoveryDiscoverySource {
    async fn browse(
        &self,
    ) -> std::result::Result<airplay_discovery::BrowseStream, UserFacingError> {
        let items = self
            .script
            .lock()
            .expect("discovery script poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                self.standing
                    .lock()
                    .expect("discovery standing poisoned")
                    .iter()
                    .map(|seed| Ok(BrowseEvent::Added(device(*seed))))
                    .collect()
            });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<BrowseItem>();
        for item in items {
            let _ = tx.send(item);
        }
        // Retaining the sender keeps the browse generation open instead of
        // ending it immediately after the scripted burst.
        self.keepalive
            .lock()
            .expect("discovery keepalive poisoned")
            .push(tx);
        self.streams.opened.fetch_add(1, Ordering::SeqCst);
        // The ticket lives inside the stream, so the supervisor dropping the
        // stream -- which is how a browse registration is released -- is what
        // closes the ledger entry. Nothing else can fake that.
        let ticket = StreamTicket(self.streams.clone());
        Ok(Box::pin(tokio_stream::StreamExt::map(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
            move |item| {
                let _held = &ticket;
                item
            },
        )))
    }
}

/// Script shared by the capture double and every worker it hands out.
///
/// Two independent failure domains live here on purpose. `failing` models the
/// endpoint going away -- the running worker ends with an error and every
/// reopen is refused, so the supervisor climbs its retry ladder instead of
/// recovering instantly. `flood` models the opposite problem: a worker that
/// produces faster than the AirPlay side consumes, which is the only way to
/// drive the bounded PCM queue into its replace-oldest behaviour on purpose.
#[derive(Default)]
pub struct CaptureScriptInner {
    failing: AtomicBool,
    flood: AtomicU64,
    /// Burst frames the worker has actually handed to the bridge.
    flooded: AtomicU64,
    /// Worker threads entered and left; equal means none leaked.
    workers_entered: AtomicUsize,
    workers_left: AtomicUsize,
}

#[derive(Clone, Default)]
pub struct CaptureScript {
    pub inner: Arc<CaptureScriptInner>,
}

impl CaptureScript {
    /// Ends the running worker with an error and refuses every reopen.
    pub fn fail(&self) {
        self.inner.failing.store(true, Ordering::SeqCst);
    }

    /// Lets the next reopen succeed again.
    pub fn heal(&self) {
        self.inner.failing.store(false, Ordering::SeqCst);
    }

    /// Makes the next worker push `frames` PCM frames as fast as the bridge
    /// accepts them, before it parks.
    pub fn flood(&self, frames: u64) {
        self.inner.flood.store(frames, Ordering::SeqCst);
    }

    /// Burst frames the worker has handed to the bridge so far.
    pub fn flooded(&self) -> u64 {
        self.inner.flooded.load(Ordering::SeqCst)
    }

    /// Capture worker threads started since the backend came up.
    pub fn workers_started(&self) -> usize {
        self.inner.workers_entered.load(Ordering::SeqCst)
    }

    /// Capture worker threads that started but have not returned.
    pub fn leaked_workers(&self) -> usize {
        self.inner.workers_entered.load(Ordering::SeqCst)
            - self.inner.workers_left.load(Ordering::SeqCst)
    }
}

/// Counts one worker thread for as long as it runs, however it ends.
struct WorkerTicket(CaptureScript);

impl WorkerTicket {
    fn new(script: CaptureScript) -> Self {
        script.inner.workers_entered.fetch_add(1, Ordering::SeqCst);
        Self(script)
    }
}

impl Drop for WorkerTicket {
    fn drop(&mut self) {
        self.0.inner.workers_left.fetch_add(1, Ordering::SeqCst);
    }
}

/// Capture double whose worker reports ready immediately and then parks.
#[derive(Clone, Default)]
pub struct RecoveryCaptureSource {
    pub script: CaptureScript,
}

pub struct RecoveryCaptureWorker {
    endpoint: AudioEndpoint,
    script: CaptureScript,
}

/// Two render endpoints, so an explicit preference is observable in the
/// published snapshot instead of always resolving to the same device.
fn test_endpoints() -> Vec<AudioEndpoint> {
    vec![
        AudioEndpoint {
            id: "endpoint-test".to_owned(),
            name: "Test Speakers".to_owned(),
        },
        AudioEndpoint {
            id: "endpoint-other".to_owned(),
            name: "Other Speakers".to_owned(),
        },
    ]
}

#[async_trait::async_trait]
impl CaptureSource for RecoveryCaptureSource {
    async fn endpoints(&self) -> std::result::Result<Vec<AudioEndpoint>, CaptureError> {
        Ok(test_endpoints())
    }

    async fn open(
        &self,
        preference: &AudioEndpointPreference,
    ) -> std::result::Result<Box<dyn CaptureWorker>, CaptureError> {
        if self.script.inner.failing.load(Ordering::SeqCst) {
            return Err(CaptureError::Worker("scripted capture failure".to_owned()));
        }
        let resolved = match preference {
            AudioEndpointPreference::SystemDefault => test_endpoints()
                .into_iter()
                .next()
                .expect("the fixture always offers a default"),
            AudioEndpointPreference::Explicit { id, .. } => test_endpoints()
                .into_iter()
                .find(|endpoint| &endpoint.id == id)
                .ok_or_else(|| CaptureError::Endpoints(format!("unknown endpoint {id}")))?,
        };
        Ok(Box::new(RecoveryCaptureWorker {
            endpoint: resolved,
            script: self.script.clone(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Network-change doubles (Task 17)
// ---------------------------------------------------------------------------

/// Interface-change source plus binding sampler in one scriptable fake.
///
/// It stands in for `NotifyIpInterfaceChange`/`CancelMibChangeNotify2` and for
/// the unicast address table: [`Self::fire`] plays a callback burst and
/// [`Self::bump_binding`] models an address actually moving.
#[derive(Default)]
pub struct FakeNetwork {
    senders: Mutex<Vec<mpsc::Sender<SystemEvent>>>,
    binding: AtomicU64,
    subscriptions: Arc<AtomicUsize>,
    cancellations: Arc<AtomicUsize>,
}

impl FakeNetwork {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Plays `count` raw callbacks of one physical interface change.
    pub fn fire(&self, count: usize) {
        let senders = self.senders.lock().expect("network senders poisoned");
        for _ in 0..count {
            for sender in senders.iter() {
                let _ = sender.try_send(SystemEvent::NetworkChanged {
                    local_binding_changed: false,
                });
            }
        }
    }

    /// Moves an active local binding, the way a DHCP lease or a VPN adapter
    /// would.
    pub fn bump_binding(&self) {
        self.binding.fetch_add(1, Ordering::SeqCst);
    }

    pub fn subscriptions(&self) -> usize {
        self.subscriptions.load(Ordering::SeqCst)
    }

    pub fn cancellations(&self) -> usize {
        self.cancellations.load(Ordering::SeqCst)
    }

    /// Waits until the monitor released the notification handle.
    pub async fn wait_for_cancel(&self) {
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while self.cancellations() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the network subscription was never cancelled"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

struct FakeSubscription {
    cancellations: Arc<AtomicUsize>,
    cancelled: bool,
}

impl NetworkSubscription for FakeSubscription {
    fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.cancelled = true;
        self.cancellations.fetch_add(1, Ordering::SeqCst);
    }
}

impl NetworkChangeSource for FakeNetwork {
    fn subscribe(
        &self,
        tx: mpsc::Sender<SystemEvent>,
    ) -> std::result::Result<Box<dyn NetworkSubscription>, NetworkError> {
        self.senders
            .lock()
            .expect("network senders poisoned")
            .push(tx);
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSubscription {
            cancellations: Arc::clone(&self.cancellations),
            cancelled: false,
        }))
    }
}

impl LocalBindingSource for FakeNetwork {
    fn fingerprint(&self) -> BindingFingerprint {
        BindingFingerprint::from_hash(self.binding.load(Ordering::SeqCst))
    }
}

impl CaptureWorker for RecoveryCaptureWorker {
    fn run(
        self: Box<Self>,
        frames: crossbeam_channel::Sender<airplay_audio::LivePcmFrame>,
        ready: tokio::sync::oneshot::Sender<std::result::Result<AudioEndpoint, UserFacingError>>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> std::result::Result<(), CaptureError> {
        let _ticket = WorkerTicket::new(self.script.clone());
        let _ = ready.send(Ok(self.endpoint.clone()));
        while !cancel.is_cancelled() {
            if self.script.inner.failing.load(Ordering::SeqCst) {
                return Err(CaptureError::Worker("scripted capture failure".to_owned()));
            }
            // A scripted burst reaches the bounded live queue exactly as an
            // overproducing endpoint's frames would. Polled here rather than
            // pushed once at startup so a test can overflow the queue without
            // first replacing the worker, which would confuse a PCM-domain
            // failure with a capture-domain one.
            let burst = self.script.inner.flood.swap(0, Ordering::SeqCst);
            for _ in 0..burst {
                if frames.send(pcm_frame()).is_err() {
                    return Ok(());
                }
                self.script.inner.flooded.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

/// One short PCM frame in the pipeline's normalized layout.
///
/// Deliberately tiny: the bounded queue counts frames, not samples, so a
/// flood large enough to prove the eviction path costs no real memory.
fn pcm_frame() -> airplay_audio::LivePcmFrame {
    airplay_audio::LivePcmFrame {
        samples: vec![0i16; 64],
        channels: 2,
        sample_rate: 44_100,
    }
}

/// Fake monotonic clock plus the delay seam driven by it.
///
/// The backend owns a private thread with its own Tokio runtime, so
/// `#[tokio::test(start_paused = true)]` would pause only the *test's* clock
/// and leave every backend timer on real time -- while making the rig's own
/// `wait_for` deadline auto-advance into a busy spin. Injecting clock and
/// timer instead makes the two-second discovery window and the thirty-second
/// rejoin spacing exact: the test states the elapsed time, load never does.
pub struct TestClock {
    pub base: std::time::Instant,
    /// Elapsed fake time; also the wake-up channel of [`TestTimer`].
    pub elapsed: watch::Sender<Duration>,
    /// Sleeps currently registered by the controller, keyed by request order.
    pub pending: Mutex<std::collections::BTreeMap<u64, Duration>>,
    /// Every deadline ever registered, in registration order.
    ///
    /// Unlike [`Self::pending`] this never shrinks. The controller rebuilds
    /// its timer futures on every loop iteration, so a live arm is *absent*
    /// from the pending map for the microseconds between two iterations --
    /// long enough for a single "is the window closed?" read to pass against
    /// a fully armed backend. Counting registrations instead is monotonic and
    /// cannot be sampled in that gap.
    pub registered: Mutex<Vec<Duration>>,
    pub next_sleep: AtomicUsize,
}

impl TestClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            base: std::time::Instant::now(),
            elapsed: watch::channel(Duration::ZERO).0,
            pending: Mutex::new(std::collections::BTreeMap::new()),
            registered: Mutex::new(Vec::new()),
            next_sleep: AtomicUsize::new(0),
        })
    }

    pub fn elapsed(&self) -> Duration {
        *self.elapsed.borrow()
    }

    /// Moves fake time forward and wakes every registered sleep.
    pub fn advance(&self, delta: Duration) {
        self.elapsed.send_modify(|value| *value += delta);
    }

    /// Sleeps currently registered for exactly `offset` from now.
    ///
    /// The rig waits on this before advancing: a settle window opens only when
    /// the component under test actually polls its timer, and advancing before
    /// that would place the deadline in the past, where it never fires.
    pub fn pending_at(&self, offset: Duration) -> usize {
        let target = self.elapsed() + offset;
        self.pending
            .lock()
            .expect("pending sleeps poisoned")
            .values()
            .filter(|deadline| **deadline == target)
            .count()
    }

    /// How often a deadline exactly `offset` from now has been armed since
    /// the backend started.
    ///
    /// Monotonic, so `before == after` across two full controller loop
    /// iterations is proof that the arm is gone rather than merely
    /// unobserved.
    pub fn registrations_at(&self, offset: Duration) -> usize {
        let target = self.elapsed() + offset;
        self.registered
            .lock()
            .expect("registered deadlines poisoned")
            .iter()
            .filter(|deadline| **deadline == target)
            .count()
    }

    /// Registered sleeps whose deadline has already passed.
    ///
    /// Zero means the controller has consumed every due deadline, which is
    /// the rig's quiescence signal after an [`Self::advance`].
    pub fn due_sleeps(&self) -> usize {
        let now = self.elapsed();
        self.pending
            .lock()
            .expect("pending sleeps poisoned")
            .values()
            .filter(|deadline| **deadline <= now)
            .count()
    }
}

impl homepod_cast::backend::Clock for TestClock {
    fn now(&self) -> std::time::Instant {
        self.base + self.elapsed()
    }
}

/// Delay seam resolving against [`TestClock`] instead of the wall clock.
pub struct TestTimer {
    pub clock: Arc<TestClock>,
}

/// Deregisters one pending sleep on completion *or* cancellation, so a
/// `select!` arm that lost the race leaves no phantom deadline behind.
pub struct SleepSlot {
    pub clock: Arc<TestClock>,
    pub id: u64,
}

impl Drop for SleepSlot {
    fn drop(&mut self) {
        self.clock
            .pending
            .lock()
            .expect("pending sleeps poisoned")
            .remove(&self.id);
    }
}

#[async_trait::async_trait]
impl homepod_cast::backend::BackendTimer for TestTimer {
    async fn sleep(&self, duration: Duration) {
        let deadline = self.clock.elapsed() + duration;
        let id = self.clock.next_sleep.fetch_add(1, Ordering::SeqCst) as u64;
        self.clock
            .pending
            .lock()
            .expect("pending sleeps poisoned")
            .insert(id, deadline);
        self.clock
            .registered
            .lock()
            .expect("registered deadlines poisoned")
            .push(deadline);
        let _slot = SleepSlot {
            clock: self.clock.clone(),
            id,
        };
        let mut elapsed = self.clock.elapsed.subscribe();
        while *elapsed.borrow_and_update() < deadline {
            if elapsed.changed().await.is_err() {
                // The clock outlives the backend; a closed channel can only
                // mean teardown, where parking forever is the honest answer.
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Drains a post-shutdown feed and returns the worker-shutdown record.
///
/// A free function rather than a rig method: the rig is consumed by
/// `shutdown()`, and this reads what the backend wrote on its way out.
pub fn drain_shutdown_report(mut feed: DiagnosticEventReceiver) -> Option<(Duration, bool)> {
    let mut found = None;
    while let Ok(event) = feed.try_recv() {
        if let DiagnosticPayload::WorkerShutdown {
            duration,
            timed_out,
        } = event.payload
        {
            found = Some((duration, timed_out));
        }
    }
    found
}

/// Generation of a phase in which a session is *live*, if any.
///
/// `Stopping` is excluded on purpose: a teardown edge naming the generation
/// it is tearing down is not a stale generation coming back, and counting it
/// would make every ordinary stop look like a defect.
fn live_phase_generation(phase: &SessionPhase) -> Option<u64> {
    match phase {
        SessionPhase::Starting { generation }
        | SessionPhase::Streaming { generation }
        | SessionPhase::Degraded { generation }
        | SessionPhase::Restarting { generation, .. } => Some(*generation),
        SessionPhase::Stopping { .. } | SessionPhase::Stopped | SessionPhase::Failed { .. } => None,
    }
}

/// Announced full-session restarts, drained from the diagnostics feed on
/// demand so a counter read never races a background consumer.
pub struct RestartLog {
    pub feed: DiagnosticEventReceiver,
    pub reasons: Vec<RestartReason>,
    /// Platform edges the controller applied, oldest first.
    pub transitions: Vec<SystemTransition>,
    /// Capture generations that reached live capture.
    pub capture_readies: usize,
    /// Highest cumulative PCM eviction count the bridge ever reported.
    pub pcm_drops: u64,
    /// Highest session generation any phase diagnostic ever named.
    pub highest_session_generation: u64,
    /// Times a *live* phase named a generation older than one already seen.
    pub stale_session_activations: usize,
    /// Queue watermark records whose depth exceeded the declared capacity.
    pub queue_overruns: usize,
    pub lagged: u64,
    /// Every event since the last [`Self::forget_events`], kept whole so a
    /// test can assert on correlation fields rather than on counters derived
    /// from them.
    pub events: Vec<DiagnosticEvent>,
}

impl RestartLog {
    pub fn drain(&mut self) {
        loop {
            match self.feed.try_recv() {
                Ok(event) => {
                    self.events.push(event.clone());
                    match event.payload {
                        DiagnosticPayload::SessionRestart { reason, .. } => {
                            self.reasons.push(reason);
                        }
                        DiagnosticPayload::SystemTransition { transition } => {
                            self.transitions.push(transition);
                        }
                        DiagnosticPayload::CaptureTransition {
                            state: AudioSourceState::Capturing,
                        } => self.capture_readies += 1,
                        DiagnosticPayload::PcmDrop { dropped } => {
                            self.pcm_drops = self.pcm_drops.max(dropped);
                        }
                        DiagnosticPayload::SessionTransition { ref phase, .. } => {
                            if let Some(generation) = live_phase_generation(phase) {
                                if generation < self.highest_session_generation {
                                    self.stale_session_activations += 1;
                                }
                                self.highest_session_generation =
                                    self.highest_session_generation.max(generation);
                            }
                        }
                        DiagnosticPayload::QueueWatermark { depth, capacity } => {
                            if depth > capacity {
                                self.queue_overruns += 1;
                            }
                        }
                        DiagnosticPayload::QueueLag { dropped } => self.lagged += dropped,
                        _ => {}
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// Drops the retained events without touching the counters.
    pub fn forget_events(&mut self) {
        self.events.clear();
    }
}

/// Full-stack harness: real controller, real session supervisor, scripted
/// discovery/capture/transport seams, and a compressed probe interval so the
/// two-second production cadence does not stretch the test budget.
/// Everything a rig needs beyond the two positional arguments the older
/// constructors take. Added rather than a fifth parameter so the existing
/// session-recovery call sites stay untouched.
#[derive(Default)]
pub struct RigSpec {
    /// Receivers discovery announces, by seed byte.
    pub seeds: Vec<u8>,
    /// Preferred primary seed.
    pub primary_seed: u8,
    /// Receivers discovery announces but the initial group does NOT contain.
    /// They exist so a test can select a receiver the backend already knows.
    pub spare_seeds: Vec<u8>,
    /// Fake time source; `None` leaves the backend on the wall clock.
    pub clock: Option<Arc<TestClock>>,
    /// Interface-change double for tests that script one. `None` does NOT
    /// leave the backend on the platform monitor -- [`RecoveryRig::build`]
    /// substitutes an inert double, because the platform seam would deliver
    /// the host machine's real interface changes into the test.
    pub network: Option<Arc<FakeNetwork>>,
    /// Bring the group up through one saved group instead of two separate
    /// commands, so exactly ONE session generation exists when the rig
    /// returns. Two commands can be drained as one coalesced batch or as two,
    /// which would make every "how many starts" assertion load-dependent.
    pub single_start: bool,
    /// Durable state written into the state directory before the backend
    /// starts, so the launch observes exactly what a previous run left there.
    pub persisted: Option<PersistedStateV1>,
    /// Return as soon as discovery has announced every receiver, without
    /// selecting or starting anything. The test then owns every edge, which
    /// is the only way to observe what a launch does on its own.
    pub idle: bool,
    /// Make every durable write fail while loads keep working, so a test can
    /// observe what a command leaves behind when persistence refuses it.
    pub fail_saves: bool,
}

/// File-backed store whose writes can be made to fail on demand.
///
/// Loads always go to the real [`FileStore`], so a rig started with seeded
/// state still restores it; only `save` is refused. That split is what makes
/// "the change was not applied" observable: the previous durable document is
/// still the live one afterwards.
struct FailingStore {
    inner: FileStore,
    fail_saves: AtomicBool,
}

#[async_trait::async_trait]
impl StateStore for FailingStore {
    async fn load(&self) -> std::result::Result<LoadOutcome, PersistError> {
        self.inner.load().await
    }

    async fn save(&self, state: &PersistedStateV1) -> std::result::Result<(), PersistError> {
        if self.fail_saves.load(Ordering::Relaxed) {
            return Err(PersistError::Replace("simulated disk failure".to_owned()));
        }
        self.inner.save(state).await
    }

    async fn load_calibration(
        &self,
    ) -> std::result::Result<
        Option<homepod_cast::backend::persistence::CalibrationStateV2>,
        PersistError,
    > {
        self.inner.load_calibration().await
    }

    async fn save_calibration(
        &self,
        calibration: Option<homepod_cast::backend::persistence::CalibrationStateV2>,
    ) -> std::result::Result<(), PersistError> {
        if self.fail_saves.load(Ordering::Relaxed) {
            return Err(PersistError::Replace("simulated disk failure".to_owned()));
        }
        self.inner.save_calibration(calibration).await
    }
}

pub struct RecoveryRig {
    pub _dir: tempfile::TempDir,
    pub script: RecoveryScript,
    pub capture: CaptureScript,
    pub discovery: Arc<RecoveryDiscoverySource>,
    pub handle: Option<DeviceBackendHandle>,
    pub state: watch::Receiver<Arc<DeviceSnapshot>>,
    pub events: tokio::sync::Mutex<broadcast::Receiver<BackendEvent>>,
    pub restarts: Mutex<RestartLog>,
    pub clock: Option<Arc<TestClock>>,
    pub baseline_starts: usize,
    pub network: Option<Arc<FakeNetwork>>,
    /// The interface-change double the backend was actually handed.
    ///
    /// Always present, whether or not the test asked for one, because the
    /// alternative is the platform seam -- see the comment in [`Self::build`].
    /// Separate from `network` so that helper still refuses to run against a
    /// rig that never scripted an interface change.
    pub network_seam: Arc<FakeNetwork>,
}

impl RecoveryRig {
    /// Starts the backend on the real clock, waits until `seeds` stream with
    /// `primary_seed` elected, and records the restart baseline.
    pub async fn active(seeds: &[u8], primary_seed: u8) -> Self {
        Self::start(seeds, primary_seed, None).await
    }

    /// Starts a rig from a full specification.
    pub async fn from_spec(spec: RigSpec) -> Self {
        Self::build(spec).await
    }

    /// Same, but with every backoff, discovery-stability, and rejoin-spacing
    /// decision bound to `clock`, which the test advances explicitly.
    pub async fn active_on(seeds: &[u8], primary_seed: u8, clock: Arc<TestClock>) -> Self {
        Self::start(seeds, primary_seed, Some(clock)).await
    }

    pub async fn start(seeds: &[u8], primary_seed: u8, clock: Option<Arc<TestClock>>) -> Self {
        Self::build(RigSpec {
            seeds: seeds.to_vec(),
            primary_seed,
            clock,
            ..RigSpec::default()
        })
        .await
    }

    async fn build(spec: RigSpec) -> Self {
        let RigSpec {
            seeds,
            primary_seed,
            spare_seeds,
            clock,
            network,
            single_start,
            persisted,
            idle,
            fail_saves,
        } = spec;
        let announced: Vec<u8> = seeds.iter().chain(spare_seeds.iter()).copied().collect();
        let seeds = seeds.as_slice();
        let dir = tempfile::TempDir::new().expect("temp dir");
        if let Some(state) = persisted.as_ref() {
            let state_directory = dir.path().join("OpenAirCast");
            std::fs::create_dir_all(&state_directory).expect("state directory is creatable");
            std::fs::write(
                state_directory.join("state-v1.json"),
                serde_json::to_vec(state).expect("the durable schema serializes"),
            )
            .expect("seed state is writable");
        }
        let script = RecoveryScript::default();
        let capture_script = CaptureScript::default();
        let discovery = Arc::new(RecoveryDiscoverySource {
            script: Mutex::new(VecDeque::from(vec![announced
                .iter()
                .map(|seed| Ok(BrowseEvent::Added(device(*seed))))
                .collect::<Vec<BrowseItem>>()])),
            ..RecoveryDiscoverySource::default()
        });
        let gate = TestStartGate::closed();
        // The double the backend actually gets. A rig that asked for none
        // still gets one -- inert, because nothing but the test can fire it --
        // and `Self::network` stays `None`, so a helper that needs a scripted
        // interface change still refuses to run against a rig that did not ask
        // for one.
        let network_seam = network.clone().unwrap_or_else(FakeNetwork::new);
        let config = BackendConfig {
            state_directory: dir.path().join("OpenAirCast"),
            legacy_volume_path: dir.path().join("HomePodCast").join("volume.txt"),
        };
        let overrides = BackendOverrides {
            discovery: Some(discovery.clone()),
            capture: Some(Arc::new(RecoveryCaptureSource {
                script: capture_script.clone(),
            })),
            session_factory: Some(Arc::new(RecoveryFactory {
                script: script.clone(),
            })),
            decoders: None,
            store: fail_saves.then(|| {
                Arc::new(FailingStore {
                    inner: FileStore::new(
                        dir.path().join("OpenAirCast"),
                        Some(dir.path().join("HomePodCast").join("volume.txt")),
                    ),
                    fail_saves: AtomicBool::new(true),
                }) as Arc<dyn StateStore>
            }),
            start_gate: Some(gate.clone()),
            probe_interval: Some(Duration::from_millis(20)),
            clock: clock
                .clone()
                .map(|clock| clock as Arc<dyn homepod_cast::backend::Clock>),
            timer: clock.clone().map(|clock| {
                Arc::new(TestTimer { clock }) as Arc<dyn homepod_cast::backend::BackendTimer>
            }),
            // Never absent. `BackendOverrides` reads a missing pair as "use
            // the platform seam", so leaving both `None` does not run the
            // backend without an interface monitor -- it subscribes this test
            // process to the host's `NotifyIpInterfaceChange`. One real
            // interface change on the machine then restarts discovery, which
            // leaves this rig's scripted browse without an open generation so
            // every later `add_service`/`remove_service` is discarded, and
            // rebuilds the group around the whole desired set, which makes
            // receivers the test is holding out of the session streaming
            // members again. Both effects are silent and permanent: a row that
            // a generation owns stops following its service record, so the
            // waits above can never be satisfied and the test dies on its
            // liveness deadline with a snapshot that says nothing about the
            // condition it was waiting for. An inert double is what actually
            // gives the seam the semantics its field documentation claims.
            network: Some(Arc::clone(&network_seam) as Arc<dyn NetworkChangeSource>),
            local_bindings: Some(Arc::clone(&network_seam) as Arc<dyn LocalBindingSource>),
        };
        let (handle, updates) =
            start_device_backend_with(config, overrides).expect("backend starts with fakes");
        gate.open();

        let DeviceBackendUpdates {
            state,
            events,
            diagnostics,
            session_diagnostics: _,
        } = updates;
        let mut rig = Self {
            _dir: dir,
            script,
            capture: capture_script,
            discovery,
            handle: Some(handle),
            state,
            events: tokio::sync::Mutex::new(events),
            restarts: Mutex::new(RestartLog {
                feed: diagnostics,
                reasons: Vec::new(),
                transitions: Vec::new(),
                capture_readies: 0,
                pcm_drops: 0,
                highest_session_generation: 0,
                stale_session_activations: 0,
                queue_overruns: 0,
                lagged: 0,
                events: Vec::new(),
            }),
            clock,
            baseline_starts: 0,
            network,
            network_seam,
        };
        if idle {
            // A row exists for every persisted desired member before any
            // browse event arrives, so mere presence proves nothing. Waiting
            // until the row has left `Unavailable` is what proves discovery
            // reported the service and started its stability window.
            rig.wait_for(|snapshot| {
                announced.iter().all(|seed| {
                    snapshot.receivers.iter().any(|row| {
                        row.id == rid(*seed)
                            && !matches!(row.lifecycle, ReceiverLifecycle::Unavailable)
                    })
                })
            })
            .await;
            return rig;
        }
        rig.wait_for(|snapshot| {
            announced
                .iter()
                .all(|seed| snapshot.receivers.iter().any(|row| row.id == rid(*seed)))
        })
        .await;
        if single_start {
            rig.activate_as_group(seeds).await;
        } else {
            rig.send(BackendCommand::SetDesiredMembers {
                members: set_of(seeds),
            })
            .await;
            rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
                .await;
        }
        rig.wait_for(|snapshot| {
            snapshot.session.active == set_of(seeds)
                && snapshot.session.primary == Some(rid(primary_seed))
        })
        .await;
        rig.baseline_starts = rig.script.started().len();
        rig
    }

    /// Brings `seeds` up through one saved group: membership and run intent
    /// commit in a single edge command, so the backend performs exactly one
    /// reconciliation and the start log is deterministic.
    async fn activate_as_group(&self, seeds: &[u8]) {
        let group = SavedGroupId::generate();
        self.send(BackendCommand::SaveGroup {
            id: Some(group),
            name: "rig".to_owned(),
            members: seeds
                .iter()
                .map(|seed| SavedGroupMember {
                    receiver: rid(*seed),
                    last_known_name: format!("Receiver {seed}"),
                    level: Volume::UNITY,
                })
                .collect(),
        })
        .await;
        self.wait_for(|snapshot| snapshot.saved_groups.iter().any(|row| row.id == group))
            .await;
        self.send(BackendCommand::ActivateSavedGroup {
            id: group,
            start: true,
        })
        .await;
    }

    fn test_clock(&self) -> &Arc<TestClock> {
        self.clock
            .as_ref()
            .expect("this helper requires a rig started on a TestClock")
    }

    /// Deadlines the controller currently holds for exactly `offset` from
    /// now.
    ///
    /// A backend arm is only observable through the deadline it registers:
    /// asserting that a window opened -- or that a command closed one -- is
    /// what makes an "and then nothing happened" test something other than a
    /// race against the scheduler.
    pub fn pending_deadlines(&self, offset: Duration) -> usize {
        self.test_clock().pending_at(offset)
    }

    /// How often a deadline exactly `offset` from now has been armed since
    /// the backend started.
    ///
    /// The counter never decreases, so an unchanged value across two full
    /// controller loop iterations proves the arm is gone -- which a single
    /// read of [`Self::pending_deadlines`] cannot, because the loop drops and
    /// rebuilds its timer futures on every iteration.
    pub fn deadline_registrations(&self, offset: Duration) -> usize {
        self.test_clock().registrations_at(offset)
    }

    /// The interface-change double this rig was started with.
    pub fn network(&self) -> Arc<FakeNetwork> {
        Arc::clone(
            self.network
                .as_ref()
                .expect("this helper requires a rig started with a FakeNetwork"),
        )
    }

    /// Desired sets handed to the session transport factory, oldest first.
    pub fn started_member_sets(&self) -> Vec<BTreeSet<ReceiverId>> {
        self.script.started()
    }

    /// Effective volumes the backend handed to the transport, oldest first.
    pub fn volume_sends(&self) -> Vec<(ReceiverId, f32)> {
        self.script.volumes()
    }

    /// Scripts `seed`'s volume control to reject every request.
    pub fn fail_volume(&self, seed: u8) {
        self.script.fail_volume(rid(seed));
    }

    /// Waits for command `id` to be rejected and returns the reason.
    ///
    /// A completion for the same id fails the test outright: a command that
    /// is supposed to be refused must not quietly succeed.
    pub async fn command_failure(&self, id: u64) -> UserFacingError {
        let mut events = self.events.lock().await;
        loop {
            match tokio::time::timeout(LIVENESS_DEADLINE, events.recv()).await {
                Ok(Ok(BackendEvent::CommandFailed { id: failed, error })) if failed == id => {
                    return error
                }
                Ok(Ok(BackendEvent::CommandCompleted { id: done })) if done == id => {
                    panic!("command {id} was accepted instead of rejected")
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => panic!("the backend event feed ended before command {id}"),
            }
        }
    }

    /// Waits for command `id` to complete, failing if it is rejected.
    pub async fn command_completion(&self, id: u64) {
        let mut events = self.events.lock().await;
        loop {
            match tokio::time::timeout(LIVENESS_DEADLINE, events.recv()).await {
                Ok(Ok(BackendEvent::CommandCompleted { id: done })) if done == id => return,
                Ok(Ok(BackendEvent::CommandFailed { id: failed, error })) if failed == id => {
                    panic!("command {id} was rejected: {error}")
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => panic!("the backend event feed ended before command {id}"),
            }
        }
    }

    /// Generation of the session currently published, if any.
    pub fn session_generation(&self) -> Option<u64> {
        match self.snapshot().session.phase {
            SessionPhase::Starting { generation }
            | SessionPhase::Streaming { generation }
            | SessionPhase::Degraded { generation }
            | SessionPhase::Restarting { generation, .. }
            | SessionPhase::Stopping { generation } => Some(generation),
            SessionPhase::Stopped | SessionPhase::Failed { .. } => None,
        }
    }

    /// Reports one platform edge and waits until it has been applied.
    pub async fn notify(&self, event: SystemEvent) {
        if matches!(event, SystemEvent::Resumed) {
            // A resume arms a settle deadline instead of acting; the clock may
            // not move before that deadline exists.
            let clock = self.test_clock();
            let before = clock.pending_at(RESUME_SETTLE);
            self.send(BackendCommand::NotifySystem(event)).await;
            self.wait_until(|| clock.pending_at(RESUME_SETTLE) > before)
                .await;
            return;
        }
        self.send(BackendCommand::NotifySystem(event)).await;
        self.round_trip().await;
    }

    /// Replaces the committed desired membership.
    pub async fn set_desired(&self, seeds: &[u8]) {
        self.send(BackendCommand::SetDesiredMembers {
            members: set_of(seeds),
        })
        .await;
        self.round_trip().await;
    }

    /// Advances `window` of fake time and waits until `seed` streams again.
    pub async fn discover_stably(&self, seed: u8, window: Duration) {
        self.advance(window).await;
        self.wait_for(|snapshot| snapshot.session.active.contains(&rid(seed)))
            .await;
    }

    /// Plays `count` interface-change callbacks and waits until the monitor
    /// has opened its settle window for them.
    pub async fn network_callbacks(&self, count: usize) {
        let clock = self.test_clock();
        let before = clock.pending_at(NETWORK_SETTLE);
        let registered_before = clock.registrations_at(NETWORK_SETTLE);
        self.network().fire(count);
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while clock.pending_at(NETWORK_SETTLE) <= before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "network_callbacks({count}): the monitor never armed a settle                  window. pending before={before} now={} registrations                  before={registered_before} now={} elapsed={:?}",
                clock.pending_at(NETWORK_SETTLE),
                clock.registrations_at(NETWORK_SETTLE),
                clock.elapsed(),
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Closes the monitor's settle window and waits until the controller has
    /// applied the resulting edge.
    pub async fn settle_network(&self) {
        let before = self.network_restart_count();
        let clock = self.test_clock();
        let pending_before = clock.pending_at(NETWORK_SETTLE);
        let elapsed_before = clock.elapsed();
        self.advance(NETWORK_SETTLE).await;
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while self.network_restart_count() <= before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "settle_network: the coalesced edge never reached the                  controller. restarts before={before} now={} pending at                  advance={pending_before} now={} registrations={}                  elapsed {:?}->{:?} discovery_gen={}",
                self.network_restart_count(),
                clock.pending_at(NETWORK_SETTLE),
                clock.registrations_at(NETWORK_SETTLE),
                elapsed_before,
                clock.elapsed(),
                self.snapshot().discovery.generation,
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Coalesced network edges the controller has applied.
    pub fn network_restart_count(&self) -> usize {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        assert_eq!(
            log.lagged, 0,
            "the diagnostics feed lagged; counts are unreliable"
        );
        log.transitions
            .iter()
            .filter(|edge| matches!(edge, SystemTransition::NetworkChanged { .. }))
            .count()
    }

    /// Capture generations that reached live capture since startup.
    pub fn capture_readies(&self) -> usize {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.capture_readies
    }

    /// Moves an active local binding and lets the resulting edge through.
    pub async fn change_active_binding(&self) {
        self.network().bump_binding();
        self.network_callbacks(1).await;

        // Effect-driven rather than proxy-driven. `pending_at` counts windows
        // "exactly one settle from now", which cannot distinguish this pulse's
        // window from one the monitor opened a scheduling hop earlier, so a
        // single advance sometimes lands beside the window it meant to close
        // (measured: 1 failure in 40 with every core saturated). Re-advancing
        // is safe here: a closed window cannot be closed twice, and the only
        // armed timer in a healthy streaming group is the settle itself.
        let before = self.network_restart_count();
        let clock = self.test_clock();
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        loop {
            self.advance(NETWORK_SETTLE).await;
            for _ in 0..40 {
                if self.network_restart_count() > before {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "change_active_binding: the binding edge never reached the                  controller. restarts before={before} now={} pending={}                  elapsed={:?} discovery_gen={}",
                self.network_restart_count(),
                clock.pending_at(NETWORK_SETTLE),
                clock.elapsed(),
                self.snapshot().discovery.generation,
            );
        }
    }

    /// Polls `pred` until it holds; the deadline bounds only the failure case.
    pub async fn wait_until(&self, pred: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while !pred() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition not reached in time; last snapshot: {:?}",
                self.snapshot()
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Waits until the factory has been asked to start `count` sessions.
    pub async fn wait_for_starts(&self, count: usize) {
        self.wait_until(|| self.started_member_sets().len() >= count)
            .await;
    }

    pub fn handle(&self) -> &DeviceBackendHandle {
        self.handle.as_ref().expect("handle present until shutdown")
    }

    pub async fn send(&self, command: BackendCommand) -> u64 {
        self.handle()
            .send(command)
            .await
            .expect("backend accepts commands until shutdown")
    }

    pub fn snapshot(&self) -> Arc<DeviceSnapshot> {
        self.state.borrow().clone()
    }

    /// Polls the published snapshot until `pred` holds; the deadline bounds
    /// only the failure case and never encodes an expected duration.
    pub async fn wait_for(&self, pred: impl Fn(&DeviceSnapshot) -> bool) {
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        loop {
            // One read per iteration, and the failure reports THAT read.
            // Reading a second time for the message let the printed snapshot
            // be newer than the one the predicate rejected -- so a snapshot
            // that satisfies the condition could be printed under "condition
            // not reached", which reads as a broken clock rather than as a
            // condition that is genuinely unreachable.
            let snapshot = self.snapshot();
            if pred(&snapshot) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition not reached in time; the snapshot it was last \
                 tested against: {snapshot:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Waits until `count` further probe cycles have completed.
    pub async fn wait_for_probes(&self, count: usize) {
        let target = self.script.probe_count() + count;
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while self.script.probe_count() < target {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the probe cycle stalled before reaching {target} cycles"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Scripts every future probe of `seed` as timed out.
    pub fn fail_probe(&self, seed: u8) {
        self.script.fail_probe(rid(seed));
    }

    /// Lets `seed` answer probes again from the next cycle on.
    pub fn heal_probe(&self, seed: u8) {
        self.script.heal_probe(rid(seed));
    }

    /// Reports `seed`'s mDNS record as expired, mid-session.
    pub fn remove_service(&self, seed: u8) {
        self.discovery.push(BrowseEvent::Removed(device_id(seed)));
    }

    /// Announces `seed` as a freshly discovered castable service.
    pub fn add_service(&self, seed: u8) {
        self.discovery.push(BrowseEvent::Added(device(seed)));
    }

    pub fn active_members(&self) -> BTreeSet<ReceiverId> {
        self.snapshot().session.active.clone()
    }

    pub fn primary(&self) -> Option<ReceiverId> {
        self.snapshot().session.primary
    }

    pub fn receiver_state(&self, seed: u8) -> ReceiverLifecycle {
        self.snapshot()
            .receivers
            .iter()
            .find(|row| row.id == rid(seed))
            .map(|row| row.lifecycle.clone())
            .expect("receiver row is published")
    }

    /// Announced full-session restarts observed since the rig became active.
    pub fn full_restart_count(&self) -> usize {
        self.script.started().len() - self.baseline_starts
    }

    /// Session starts the controller asked for since the rig became active,
    /// oldest first, as `(desired in the order it was handed down, preferred
    /// primary)`.
    ///
    /// Bring-up is excluded through the same baseline `full_restart_count`
    /// uses, so a test states what happened *after* the group came up.
    pub fn start_requests(&self) -> Vec<(Vec<ReceiverId>, Option<ReceiverId>)> {
        self.script.requests().split_off(self.baseline_starts)
    }

    /// Moves fake time forward and returns only once the controller has
    /// consumed every deadline that became due.
    ///
    /// Two synchronizations, both on real state rather than on elapsed wall
    /// time: no registered sleep is due any more, and a no-op command has
    /// completed a full loop iteration afterwards. Only then can a counter
    /// read observe the decision the advance triggered -- or prove that the
    /// controller deliberately made none.
    pub async fn advance(&self, delta: Duration) {
        let clock = self
            .clock
            .as_ref()
            .expect("advance requires a rig started on a TestClock");
        clock.advance(delta);
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        while clock.due_sleeps() > 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the controller never consumed the deadline that came due"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.round_trip().await;
    }

    /// Drives one command through the whole actor loop and back.
    ///
    /// Deleting an unknown group is the only command that changes nothing at
    /// all: no persistence, no publication, just the completion edge -- which
    /// is exactly the proof that the loop iterated.
    pub async fn round_trip(&self) {
        let id = self
            .send(BackendCommand::DeleteGroup(SavedGroupId::generate()))
            .await;
        let mut events = self.events.lock().await;
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the controller never completed the no-op round trip"
            );
            match tokio::time::timeout(LIVENESS_DEADLINE, events.recv()).await {
                Ok(Ok(BackendEvent::CommandCompleted { id: completed })) if completed == id => {
                    return
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => panic!("the backend event feed ended mid round trip"),
            }
        }
    }

    /// Polls until `seed`'s published row satisfies `pred`.
    pub async fn wait_for_state(&self, seed: u8, pred: impl Fn(&ReceiverLifecycle) -> bool) {
        self.wait_for(|snapshot| {
            snapshot
                .receivers
                .iter()
                .any(|row| row.id == rid(seed) && pred(&row.lifecycle))
        })
        .await;
    }

    /// Fails every listed secondary's probes and waits until each one has
    /// actually entered its backoff window.
    pub async fn fail_secondaries(&self, seeds: &[u8]) {
        for seed in seeds {
            self.fail_probe(*seed);
        }
        for seed in seeds {
            self.wait_for_state(*seed, |state| {
                matches!(state, ReceiverLifecycle::RetryWaiting { .. })
            })
            .await;
        }
    }

    /// Forgets every diagnostic recorded so far; the restart, transition, and
    /// capture counters are unaffected.
    ///
    /// Bring-up produces a full lifecycle timeline for every member; a test
    /// about what happens *after* that has to state where its window starts.
    pub fn forget_diagnostics(&self) {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.forget_events();
    }

    /// Receiver-scoped diagnostics for `seed` since the last
    /// [`Self::forget_diagnostics`], oldest first.
    ///
    /// Fails loudly on feed lag: a timeline with a hole in it would make an
    /// ordering assertion pass for the wrong reason.
    pub fn diagnostics_for(&self, seed: u8) -> Vec<DiagnosticEvent> {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        assert_eq!(
            log.lagged, 0,
            "the diagnostics feed lagged; the timeline has holes"
        );
        log.events
            .iter()
            .filter(|event| event.receiver == Some(rid(seed)))
            .cloned()
            .collect()
    }

    /// Highest cumulative PCM eviction count the bridge has reported.
    ///
    /// Cumulative and monotonic, so it survives
    /// [`Self::forget_diagnostics`]: a test can take it as a baseline before
    /// an overflow and compare afterwards.
    pub fn pcm_drops(&self) -> u64 {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.pcm_drops
    }

    /// Queue watermark records that claimed a depth above their own capacity.
    ///
    /// Anything other than zero means a bounded queue was not bounded.
    pub fn queue_overruns(&self) -> usize {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.queue_overruns
    }

    /// Waits until the capture worker is provably live.
    ///
    /// Bring-up deliberately does not wait for this: the session becoming
    /// active says nothing about the capture side, which runs on its own
    /// thread. The worker counts itself in before it signals readiness and
    /// the supervisor publishes `Capturing` only once it has that signal, so
    /// this published state is the edge that proves
    /// [`CaptureScript::workers_started`] has already counted the first
    /// worker.
    ///
    /// Any test that takes a worker count as a baseline has to establish
    /// that edge first. Without it the baseline records when the test
    /// happened to look rather than what the code did, and reads zero or one
    /// depending on machine load.
    pub async fn settle_capture(&self) {
        self.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Capturing)
            .await;
    }

    /// Ends the running capture worker with an error and refuses reopens,
    /// then waits until the supervisor has published its recovery state.
    pub async fn fail_capture(&self) {
        self.capture.fail();
        self.wait_for(|snapshot| snapshot.audio_source.state == AudioSourceState::Recovering)
            .await;
    }

    /// Lets capture reopen and advances the retry ladder until a worker is
    /// live again.
    ///
    /// Steps rather than jumps for the same reason `advance_to_retry` does:
    /// the ladder is policy, not a number this helper may restate.
    pub async fn recover_capture(&self) {
        // One step covers the ladder's longest rung.
        const STEP: Duration = Duration::from_secs(5);

        self.capture.heal();
        self.advance_until(
            || self.snapshot().audio_source.state == AudioSourceState::Capturing,
            STEP,
            "capture never came back after the endpoint was healed",
        )
        .await;
    }

    /// Pushes `frames` PCM frames through the live capture worker and waits
    /// until the bounded live queue has reported evicting.
    ///
    /// Deliberate rather than incidental: the bridge paces a silence frame
    /// every fifty milliseconds and would saturate the queue on its own after
    /// roughly a second and a half of wall time. Waiting for that would make
    /// the assertion depend on load; a burst saturates it at a moment the
    /// test chooses.
    pub async fn overflow_pcm(&self, frames: u64) {
        let before = self.pcm_drops();
        let sent_before = self.capture.flooded();

        // Three steps, and the order is the whole point.
        //
        // The consumer is held still first, so every burst frame beyond the
        // queue's capacity *must* evict -- with a live consumer the eviction
        // count would be whatever the probe cycle happened not to drain, and
        // that is load, not behaviour. The burst is then awaited in full,
        // because the bridge coalesces an eviction run into its two edges:
        // resuming while frames are still arriving would end the run early
        // and report a number that means nothing. Only then does the consumer
        // come back, which drains the queue and lets the next paced frame
        // enqueue -- and that enqueue is the trailing edge that finally
        // publishes the true cumulative count.
        self.script.pause_drain();
        self.capture.flood(frames);
        self.wait_until(|| self.capture.flooded() >= sent_before + frames)
            .await;
        self.script.resume_drain();

        let target = before + frames / 2;
        self.wait_until(|| self.pcm_drops() >= target).await;
    }

    /// Faults every open browse generation and waits until the supervisor has
    /// opened a newer one.
    ///
    /// The backoff is real policy with jitter, so the wait advances fake time
    /// in steps instead of naming a delay.
    pub async fn fail_discovery(&self) {
        // One step comfortably exceeds the ladder's thirty-second ceiling.
        const STEP: Duration = Duration::from_secs(40);

        let before = self.snapshot().discovery.generation;
        self.discovery.push_failure();
        self.advance_until(
            || self.snapshot().discovery.generation > before,
            STEP,
            "discovery never restarted after a daemon failure",
        )
        .await;
    }

    /// Advances fake time in `step`s until `done` holds, polling between
    /// steps and bounding only the failure case on the wall clock.
    ///
    /// Re-advancing is what makes this correct rather than merely patient.
    /// A deadline exists only once the component under test has actually
    /// called its timer, and nothing observable marks that instant: a single
    /// advance issued a scheduling hop too early lands *before* the sleep is
    /// registered and can never fire it. Moving fake time forward again is
    /// harmless -- a deadline already passed cannot pass twice -- and is the
    /// only way to close that gap without guessing a wall-clock delay.
    async fn advance_until(&self, done: impl Fn() -> bool, step: Duration, what: &str) {
        let deadline = tokio::time::Instant::now() + LIVENESS_DEADLINE;
        loop {
            if done() {
                return;
            }
            self.advance(step).await;
            for _ in 0..50 {
                if done() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what}; fake time elapsed {:?}, last snapshot: {:?}",
                self.test_clock().elapsed(),
                self.snapshot()
            );
        }
    }

    /// Live session phases that named a generation older than one the feed
    /// had already reported.
    ///
    /// Anything other than zero means a superseded generation was published
    /// as running -- the exact failure a monotonic counter exists to prevent.
    pub fn stale_session_activations(&self) -> usize {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        assert_eq!(
            log.lagged, 0,
            "the diagnostics feed lagged; generation ordering is unverifiable"
        );
        log.stale_session_activations
    }

    /// Highest session generation any phase diagnostic has named.
    pub fn highest_session_generation(&self) -> u64 {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.highest_session_generation
    }

    /// Browse registrations the supervisor opened but never released.
    pub fn leaked_browse_streams(&self) -> usize {
        self.discovery.streams.leaked()
    }

    /// Capture worker threads that started but never returned.
    pub fn leaked_capture_workers(&self) -> usize {
        self.capture.leaked_workers()
    }

    /// Capture generations reported by capture-state diagnostics, oldest
    /// first.
    pub fn capture_generations(&self) -> Vec<u64> {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.events
            .iter()
            .filter(|event| matches!(event.payload, DiagnosticPayload::CaptureTransition { .. }))
            .map(|event| event.capture_generation)
            .collect()
    }

    /// Session phases the feed reported, with the active member count each
    /// one committed with, oldest first.
    pub fn session_phase_reports(&self) -> Vec<(SessionPhase, u32)> {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.events
            .iter()
            .filter_map(|event| match &event.payload {
                DiagnosticPayload::SessionTransition {
                    phase,
                    active_members,
                } => Some((phase.clone(), *active_members)),
                _ => None,
            })
            .collect()
    }

    /// Every diagnostic the feed carried since the last
    /// [`Self::forget_diagnostics`], oldest first.
    ///
    /// Fails loudly on feed lag for the same reason [`Self::diagnostics_for`]
    /// does: a correlation assertion over a timeline with a hole in it would
    /// pass for the wrong reason.
    pub fn records(&self) -> Vec<DiagnosticEvent> {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        assert_eq!(
            log.lagged, 0,
            "the diagnostics feed lagged; the timeline has holes"
        );
        log.events.clone()
    }

    /// Debug renderings of every diagnostic the feed carried, for redaction
    /// assertions that must not depend on which payload variant produced the
    /// text.
    pub fn rendered_diagnostics(&self) -> Vec<String> {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        log.events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect()
    }

    /// An independent subscription to the diagnostic feed.
    ///
    /// Survives [`Self::shutdown`], which is the only way to observe records
    /// the backend writes while it is tearing itself down.
    pub fn diagnostic_feed(&self) -> DiagnosticEventReceiver {
        self.restarts
            .lock()
            .expect("restart log poisoned")
            .feed
            .resubscribe()
    }

    /// Fails one secondary's probes and waits until its backoff window opened.
    ///
    /// Runtime failure, not setup failure: the transport double reports a
    /// broken member through the probe cycle, and that is the route a member
    /// that was already streaming actually takes. Both routes converge on
    /// `note_receiver_failure`, so the diagnostic timeline is the same.
    pub async fn fail_secondary(&self, seed: u8) {
        self.fail_secondaries(&[seed]).await;
    }

    /// Lets `seed` answer probes again while it stays in its backoff window.
    pub async fn recover(&self, seed: u8) {
        self.heal_probe(seed);
        self.round_trip().await;
    }

    /// Advances fake time until `seed`'s backoff deadline has fired and the
    /// receiver has left `RetryWaiting`.
    ///
    /// Steps rather than jumps: the ladder carries deterministic jitter, so
    /// the exact deadline is policy, not a number a test may restate. Every
    /// step is fake time, so this never waits on load.
    pub async fn advance_to_retry(&self, seed: u8) {
        const STEP: Duration = Duration::from_millis(250);
        const BUDGET: Duration = Duration::from_secs(60);

        let mut spent = Duration::ZERO;
        while matches!(
            self.receiver_state(seed),
            ReceiverLifecycle::RetryWaiting { .. }
        ) {
            assert!(
                spent < BUDGET,
                "receiver {seed} never left its backoff window within {BUDGET:?} of fake time"
            );
            self.advance(STEP).await;
            spent += STEP;
        }
    }

    /// Fails the primary's probes and waits for a new primary to be elected.
    pub async fn fail_primary(&self, seed: u8) {
        self.fail_probe(seed);
        self.wait_for(|snapshot| {
            snapshot
                .session
                .primary
                .is_some_and(|primary| primary != rid(seed))
        })
        .await;
    }

    /// Lets `seed` answer again, re-announces its service, and then holds it
    /// castable for `stable` of fake time.
    pub async fn rediscover_for(&self, seed: u8, stable: Duration) {
        self.heal_probe(seed);
        self.add_service(seed);
        self.wait_for_state(seed, |state| {
            !matches!(state, ReceiverLifecycle::Unavailable)
        })
        .await;
        self.advance(stable).await;
    }

    /// Removes and re-announces every listed service repeatedly across
    /// `window` of fake time, one second per cycle.
    ///
    /// All of them move together, so no receiver is ever castable for the two
    /// continuous seconds an automatic rejoin requires -- which is what makes
    /// the flap a storm the gates must absorb rather than a recovery.
    ///
    /// Every edge is awaited on the published row rather than merely pushed.
    /// A browse event that is still in flight when fake time moves is an
    /// event the sender never observed, and a loss the sender never observed
    /// is not a flap at all: the discoverability window is only restarted by
    /// a loss the controller actually saw. Without the waits below, whether
    /// this helper produces a flap or an unbroken minute and a half of
    /// castable discovery depends on how fast the test drives the backend --
    /// which is load, not behaviour.
    ///
    /// Only usable for receivers whose row reacts to their service record. A
    /// streaming member deliberately keeps its row across an expired service,
    /// so it offers no edge to wait on; use [`Self::flap_for`] there and take
    /// its weaker synchronization with it.
    pub async fn flap_all_for(&self, seeds: &[u8], window: Duration) {
        let mut spent = Duration::ZERO;
        while spent < window {
            let step = Duration::from_secs(1).min(window - spent);
            for seed in seeds {
                self.remove_service(*seed);
            }
            for seed in seeds {
                self.wait_for_state(*seed, |state| {
                    matches!(state, ReceiverLifecycle::Unavailable)
                })
                .await;
            }
            for seed in seeds {
                self.add_service(*seed);
            }
            for seed in seeds {
                self.wait_for_state(*seed, |state| {
                    !matches!(state, ReceiverLifecycle::Unavailable)
                })
                .await;
            }
            self.advance(step).await;
            spent += step;
        }
    }

    /// Removes and re-announces `seed`'s service repeatedly across `window`
    /// of fake time, one second per cycle.
    pub async fn flap_for(&self, seed: u8, window: Duration) {
        let mut spent = Duration::ZERO;
        while spent < window {
            let step = Duration::from_secs(1).min(window - spent);
            self.remove_service(seed);
            self.round_trip().await;
            self.add_service(seed);
            self.advance(step).await;
            spent += step;
        }
    }

    /// Announced restarts carrying `reason` since the backend started.
    pub fn restarts_for(&self, reason: RestartReason) -> usize {
        let mut log = self.restarts.lock().expect("restart log poisoned");
        log.drain();
        assert_eq!(
            log.lagged, 0,
            "the diagnostics feed lagged; counts are unreliable"
        );
        log.reasons.iter().filter(|entry| **entry == reason).count()
    }

    /// Announced rejoin restarts (`RestartReason::SecondaryRejoin`).
    pub fn rejoin_restarts(&self) -> usize {
        self.restarts_for(RestartReason::SecondaryRejoin)
    }

    /// Announced primary-loss restarts (`RestartReason::PrimaryReplaced`).
    pub fn primary_loss_restarts(&self) -> usize {
        self.restarts_for(RestartReason::PrimaryReplaced)
    }

    /// Stops the backend and reports the transports that outlived it.
    ///
    /// Dropping the last handle joins the control thread, so the count is
    /// final rather than a snapshot of an unfinished teardown.
    pub async fn shutdown(mut self) -> usize {
        if let Some(handle) = self.handle.as_ref() {
            let _ = handle.send(BackendCommand::Shutdown).await;
        }
        drop(self.handle.take());
        self.script.inner.leaks.leaked()
    }
}

// ---------------------------------------------------------------------------
// System-recovery view of the same rig (Task 17)
// ---------------------------------------------------------------------------

/// [`RecoveryRig`] configured for platform edges: fake clock, fake
/// interface-change source, and a deterministic single-generation bring-up.
///
/// A thin wrapper rather than a second harness, so both suites exercise the
/// same controller, the same supervisors, and the same leak accounting.
pub struct SystemRig(RecoveryRig);

impl std::ops::Deref for SystemRig {
    type Target = RecoveryRig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SystemRig {
    /// Brings `seeds` up on a fresh fake clock.
    pub async fn active(seeds: &[u8], primary_seed: u8) -> Self {
        Self::active_on(seeds, primary_seed, TestClock::new()).await
    }

    /// Brings `seeds` up on the caller's clock, so a test can assert on the
    /// exact instant a settle deadline fires.
    pub async fn active_on(seeds: &[u8], primary_seed: u8, clock: Arc<TestClock>) -> Self {
        Self::build(seeds, &[], primary_seed, clock).await
    }

    /// Same, but discovery also announces `spare` receivers that the initial
    /// group deliberately excludes, so a test can select one later.
    pub async fn active_with_spare(seeds: &[u8], spare: &[u8], primary_seed: u8) -> Self {
        Self::build(seeds, spare, primary_seed, TestClock::new()).await
    }

    /// Launches with `state` already on disk and stops right there: nothing
    /// is selected and nothing is started, so whatever happens next is the
    /// backend's own startup policy rather than a command the rig sent.
    ///
    /// `announced` are the receivers discovery reports; they need not overlap
    /// the persisted desired set, which is how a test can prove that an
    /// unrelated receiver is never dragged in.
    pub async fn launched_with(
        state: PersistedStateV1,
        announced: &[u8],
        clock: Arc<TestClock>,
    ) -> Self {
        Self(
            RecoveryRig::from_spec(RigSpec {
                seeds: announced.to_vec(),
                clock: Some(clock),
                network: Some(FakeNetwork::new()),
                persisted: Some(state),
                idle: true,
                ..RigSpec::default()
            })
            .await,
        )
    }

    /// Same as [`Self::launched_with`], but every durable write fails.
    ///
    /// The launch itself performs no save, so the state on disk is still the
    /// one the test seeded; only what commands try to write is refused.
    pub async fn launched_with_failing_store(
        state: PersistedStateV1,
        announced: &[u8],
        clock: Arc<TestClock>,
    ) -> Self {
        Self(
            RecoveryRig::from_spec(RigSpec {
                seeds: announced.to_vec(),
                clock: Some(clock),
                network: Some(FakeNetwork::new()),
                persisted: Some(state),
                idle: true,
                fail_saves: true,
                ..RigSpec::default()
            })
            .await,
        )
    }

    async fn build(seeds: &[u8], spare: &[u8], primary_seed: u8, clock: Arc<TestClock>) -> Self {
        Self(
            RecoveryRig::from_spec(RigSpec {
                seeds: seeds.to_vec(),
                primary_seed,
                spare_seeds: spare.to_vec(),
                clock: Some(clock),
                network: Some(FakeNetwork::new()),
                single_start: true,
                ..RigSpec::default()
            })
            .await,
        )
    }

    /// Stops the backend and reports the transports that outlived it.
    pub async fn shutdown(self) -> usize {
        self.0.shutdown().await
    }
}
