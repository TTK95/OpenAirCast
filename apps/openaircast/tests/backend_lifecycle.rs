//! Backend lifecycle guarantees for plan Task 10.
//!
//! Covers a cross-cutting seam that cannot be proven inside the library unit
//! tests alone: a real [`WasapiCaptureWorker`] emits readiness only after the
//! stream started, delivers converted frames, and stops promptly on cancel.

// The shared full-stack harness, so the fifty-cycle soak below drives the same
// controller, supervisors, and leak ledgers as every other backend suite
// instead of a fourth private rig.
#[path = "support/backend_rig.rs"]
mod backend_rig;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use homepod_cast::backend::capture::{
    CaptureError, CaptureSource, CaptureWorker, WasapiApi, WasapiCaptureSource, WasapiDeviceRef,
    WasapiLoopbackStream,
};
use homepod_cast::backend::model::{AudioEndpoint, AudioEndpointPreference};

/// NOTE: the canonical `FakeWasapiApi` lives in `tests/support/mod.rs`, but
/// that fixture file also carries crate-private seams (`pub(crate)
/// DiscoverySource`) and therefore cannot compile inside an integration-test
/// binary. This binary-local twin mirrors exactly the subset the lifecycle
/// test observes: start/stop counters, the scripted read batches, and the
/// open log.
#[derive(Clone, Default)]
struct LifecycleFakeWasapi {
    inner: Arc<LifecycleFakeInner>,
}

#[derive(Default)]
struct LifecycleFakeInner {
    endpoints: Mutex<HashMap<String, String>>,
    opened_ids: Mutex<Vec<String>>,
    started_streams: AtomicUsize,
    stopped_streams: AtomicUsize,
    read_script: Mutex<VecDeque<Vec<f32>>>,
}

impl LifecycleFakeWasapi {
    fn with_default(self, id: &str) -> Self {
        self.inner
            .endpoints
            .lock()
            .expect("endpoint map poisoned")
            .insert(id.to_owned(), id.to_owned());
        self
    }

    fn push_read_batch(&self, samples: Vec<f32>) {
        self.inner
            .read_script
            .lock()
            .expect("read script poisoned")
            .push_back(samples);
    }
}

impl WasapiApi for LifecycleFakeWasapi {
    fn enumerate_render_endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        Ok(self
            .inner
            .endpoints
            .lock()
            .expect("endpoint map poisoned")
            .iter()
            .map(|(id, name)| AudioEndpoint {
                id: id.clone(),
                name: name.clone(),
            })
            .collect())
    }

    fn get_device_by_id(&self, id: &str) -> Result<WasapiDeviceRef, CaptureError> {
        self.inner
            .endpoints
            .lock()
            .expect("endpoint map poisoned")
            .get(id)
            .map(|name| WasapiDeviceRef {
                id: id.to_owned(),
                name: name.clone(),
            })
            .ok_or_else(|| {
                CaptureError::Endpoints(format!("render endpoint '{id}' is unavailable"))
            })
    }

    fn get_default_output_device(&self) -> Result<WasapiDeviceRef, CaptureError> {
        let first = self
            .inner
            .endpoints
            .lock()
            .expect("endpoint map poisoned")
            .iter()
            .next()
            .map(|(id, name)| (id.clone(), name.clone()));
        first
            .map(|(id, name)| WasapiDeviceRef { id, name })
            .ok_or_else(|| CaptureError::Endpoints("no default render endpoint".into()))
    }

    fn open_loopback(
        &self,
        device: &WasapiDeviceRef,
    ) -> Result<Box<dyn WasapiLoopbackStream>, CaptureError> {
        self.inner
            .opened_ids
            .lock()
            .expect("opened log poisoned")
            .push(device.id.clone());
        Ok(Box::new(LifecycleFakeStream {
            inner: Arc::clone(&self.inner),
        }))
    }
}

struct LifecycleFakeStream {
    inner: Arc<LifecycleFakeInner>,
}

impl WasapiLoopbackStream for LifecycleFakeStream {
    fn start_stream(&mut self) -> Result<(), CaptureError> {
        self.inner.started_streams.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn read_samples(&mut self) -> Result<Vec<f32>, CaptureError> {
        Ok(self
            .inner
            .read_script
            .lock()
            .expect("read script poisoned")
            .pop_front()
            .unwrap_or_default())
    }

    fn wait_for_event(&mut self, _timeout_ms: u32) -> bool {
        if !self
            .inner
            .read_script
            .lock()
            .expect("read script poisoned")
            .is_empty()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
        false
    }

    fn stop_stream(&mut self) {
        self.inner.stopped_streams.fetch_add(1, Ordering::SeqCst);
    }
}

/// Readiness fires after `start_stream`, scripted batches flow through as
/// normalized PCM frames, and cancellation ends the worker promptly.
#[tokio::test]
async fn worker_emits_ready_then_frames_then_honors_cancel() {
    let api = LifecycleFakeWasapi::default().with_default("speakers");
    api.push_read_batch(vec![0.5, -0.5]);
    api.push_read_batch(vec![1.0, -1.0, 2.0]);
    let source = WasapiCaptureSource::with_api(Arc::new(api.clone()));

    let preference = AudioEndpointPreference::SystemDefault;
    let endpoint = source.resolve(&preference).unwrap();
    let worker = source.open(&preference).await.unwrap();

    let (frames_tx, frames_rx) = crossbeam_channel::bounded(8);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let runner =
        tokio::task::spawn_blocking(move || worker.run(frames_tx, ready_tx, worker_cancel));

    // Readiness arrives only after the fake stream started successfully.
    let ready = tokio::time::timeout(Duration::from_secs(2), ready_rx)
        .await
        .expect("worker reports readiness in time")
        .expect("readiness oneshot stays open")
        .expect("worker becomes ready");
    assert_eq!(ready.id, endpoint.id);
    assert_eq!(ready.name, "speakers");
    assert_eq!(api.inner.started_streams.load(Ordering::SeqCst), 1);

    // Scripted f32 batches arrive as one normalized i16 frame per batch
    // (clamp and scale exactly like the legacy capture loop).
    let mut batches = Vec::new();
    while batches.len() < 2 {
        let frame = frames_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker streams the scripted batches");
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 44_100);
        batches.push(frame.samples);
    }
    assert_eq!(batches[0], vec![16_383, -16_383]);
    assert_eq!(batches[1], vec![32_767, -32_767, 32_767]);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .expect("cancel stops the worker promptly")
        .expect("blocking task joins")
        .expect("cancelled worker ends cleanly");
    assert_eq!(
        api.inner.stopped_streams.load(Ordering::SeqCst),
        1,
        "stream stopped exactly once"
    );
}

// ---------------------------------------------------------------------------
// Task 15: device backend controller Ã¢â‚¬â€ public contract, coalescing, revisions,
// saved-group activation atomicity, and persistence-failure semantics.
//
// FAKE PLACEMENT NOTE: the canonical fixtures live in `tests/support/mod.rs`,
// but that file implements crate-private seams (`pub(crate) DiscoverySource`,
// `pub(crate) DecoderFactory`) and therefore cannot compile inside this
// integration-test binary (same reason as the `LifecycleFakeWasapi` twin
// above). The doubles below therefore mirror the minimal scripted behavior
// each test observes, wired through the controller's public `BackendOverrides`
// seam. `FakeSessionTransportFactory` is placed here too: it implements the
// doc-hidden-but-public `SessionTransportFactory` trait and records every
// start attempt so reconciliation counts are observable without hardware.
// ---------------------------------------------------------------------------

use airplay_audio::LiveAudioDecoder;
use airplay_core::{Device, DeviceId, Features, Version};
use airplay_discovery::BrowseEvent;
use tempfile::TempDir;
use tokio_stream::wrappers::UnboundedReceiverStream;

use homepod_cast::backend::command::BackendCommand;
use homepod_cast::backend::command::SystemEvent;
use homepod_cast::backend::controller::{
    start_device_backend_with, BackendConfig, BackendDiscoverySource, BackendOverrides,
    TestStartGate,
};
use homepod_cast::backend::event::{
    BackendEvent, DiagnosticPayload, DiagnosticTryRecvError, ErrorScope, NoticeCode, Severity,
};
use homepod_cast::backend::model::{
    DeviceSnapshot, LatencyConfig, PersistenceSnapshot, ReceiverId, RunIntent, SavedGroupId,
    SavedGroupMember, SessionPhase, UserFacingError, Volume,
};
use homepod_cast::backend::persistence::{LoadOutcome, PersistError, PersistedStateV1, StateStore};
use homepod_cast::backend::session::{
    SessionStartFailure, SessionTransport, SessionTransportFactory,
};
use homepod_cast::backend::{DeviceBackendHandle, DeviceBackendUpdates};

/// One fallible browse-stream item as produced by the discovery daemon.
type BrowseItem = std::result::Result<BrowseEvent, airplay_core::error::DiscoveryError>;

fn rid(seed: u8) -> ReceiverId {
    ReceiverId::from_storage_key(&format!("0000000000{seed:02X}"))
        .expect("fixed seed forms a valid storage key")
}

fn group(seed: u8) -> SavedGroupId {
    let uuid = uuid::Uuid::from_u128(u128::from(seed));
    serde_json::from_value(serde_json::Value::String(uuid.to_string()))
        .expect("uuid string deserializes into SavedGroupId")
}

fn volume(value: f32) -> Volume {
    Volume::new(value).expect("test volume is valid")
}

/// Castable discovered-device fixture mirroring `tests/support` (AirPlay 2 +
/// usable IPv4, stable identity equal to [`rid`] for the same seed).
fn castable_device(seed: u8, name: &str) -> Device {
    Device {
        id: DeviceId([0, 0, 0, 0, 0, seed]),
        name: name.to_owned(),
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

/// Scripted discovery source over the controller's public seam; mirrors the
/// support fixture's stream-per-browse-call contract.
#[derive(Default)]
struct LocalFakeDiscoverySource {
    script: Mutex<VecDeque<Vec<BrowseItem>>>,
    live: Mutex<Vec<Option<tokio::sync::mpsc::UnboundedSender<BrowseItem>>>>,
}

impl LocalFakeDiscoverySource {
    fn new(script: Vec<BrowseItem>) -> Arc<Self> {
        let source = Self::default();
        source.push_stream(script);
        Arc::new(source)
    }

    fn push_stream(&self, items: Vec<BrowseItem>) {
        self.script
            .lock()
            .expect("fake discovery script poisoned")
            .push_back(items);
    }
}

#[async_trait::async_trait]
impl BackendDiscoverySource for LocalFakeDiscoverySource {
    async fn browse(
        &self,
    ) -> std::result::Result<airplay_discovery::BrowseStream, UserFacingError> {
        let next = self
            .script
            .lock()
            .expect("fake discovery script poisoned")
            .pop_front();
        match next {
            Some(items) => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<BrowseItem>();
                for item in items {
                    let _ = tx.send(item);
                }
                self.live
                    .lock()
                    .expect("fake discovery live poisoned")
                    .push(Some(tx));
                Ok(Box::pin(UnboundedReceiverStream::new(rx)))
            }
            None => panic!("unexpected browse(): fake discovery script exhausted"),
        }
    }
}

/// Capture source double whose worker reports ready immediately and parks
/// until cancellation; no audio hardware is touched.
struct LocalHangCaptureSource;

struct LocalHangWorker(AudioEndpoint);

#[async_trait::async_trait]
impl CaptureSource for LocalHangCaptureSource {
    async fn endpoints(&self) -> std::result::Result<Vec<AudioEndpoint>, CaptureError> {
        Ok(vec![AudioEndpoint {
            id: "endpoint-test".to_owned(),
            name: "Test Speakers".to_owned(),
        }])
    }

    async fn open(
        &self,
        _preference: &AudioEndpointPreference,
    ) -> std::result::Result<Box<dyn CaptureWorker>, CaptureError> {
        Ok(Box::new(LocalHangWorker(AudioEndpoint {
            id: "endpoint-test".to_owned(),
            name: "Test Speakers".to_owned(),
        })))
    }
}

impl CaptureWorker for LocalHangWorker {
    fn run(
        self: Box<Self>,
        _frames: crossbeam_channel::Sender<airplay_audio::LivePcmFrame>,
        ready: tokio::sync::oneshot::Sender<std::result::Result<AudioEndpoint, UserFacingError>>,
        cancel: CancellationToken,
    ) -> std::result::Result<(), CaptureError> {
        let _ = ready.send(Ok(self.0));
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

/// Documented STUB session factory (Task 15 decision): production wiring of
/// `airplay_client::connect_group_best_effort` lands with Task 16. The stub
/// records every start attempt so tests can observe reconciliations, then
/// reports a best-effort start failure instead of touching the network.
#[derive(Clone, Default)]
struct FakeSessionTransportFactory {
    started_desired: Arc<Mutex<Vec<Vec<[u8; 6]>>>>,
    /// Calibration profile of the most recent start attempt, if any.
    calibration: Arc<Mutex<Option<homepod_cast::calibration::CalibrationProfile>>>,
}

impl FakeSessionTransportFactory {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn start_count(&self) -> usize {
        self.started_desired
            .lock()
            .expect("factory log poisoned")
            .len()
    }

    fn last_calibration(&self) -> Option<homepod_cast::calibration::CalibrationProfile> {
        self.calibration
            .lock()
            .expect("factory log poisoned")
            .clone()
    }

    #[allow(dead_code)] // read by the Task 15 coalescing assertions only
    fn started_sets(&self) -> Vec<Vec<[u8; 6]>> {
        self.started_desired
            .lock()
            .expect("factory log poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl SessionTransportFactory for FakeSessionTransportFactory {
    async fn start(
        &self,
        desired: Vec<Device>,
        _preferred_primary: Option<ReceiverId>,
        _decoder: LiveAudioDecoder,
        _latency: LatencyConfig,
        calibration: homepod_cast::calibration::CalibrationProfile,
    ) -> std::result::Result<Box<dyn SessionTransport>, SessionStartFailure> {
        self.started_desired
            .lock()
            .expect("factory log poisoned")
            .push(desired.iter().map(|device| device.id.0).collect());
        *self.calibration.lock().expect("factory log poisoned") = Some(calibration);
        Err(SessionStartFailure {
            error: UserFacingError::new("AirPlay session transport is not yet wired (Task 16)"),
            failures: Vec::new(),
        })
    }
}

/// File-backed store that records every accepted save candidate so tests can
/// assert atomicity and coalescing on the exact durable payloads, and that can
/// simulate write failure while keeping load working.
struct RecordingStore {
    inner: homepod_cast::backend::persistence::FileStore,
    saved: Mutex<Vec<PersistedStateV1>>,
    fail_saves: AtomicBool,
}

impl RecordingStore {
    fn new(state_directory: std::path::PathBuf, legacy_volume_path: std::path::PathBuf) -> Self {
        Self {
            inner: homepod_cast::backend::persistence::FileStore::new(
                state_directory,
                Some(legacy_volume_path),
            ),
            saved: Mutex::new(Vec::new()),
            fail_saves: AtomicBool::new(false),
        }
    }

    fn saved(&self) -> Vec<PersistedStateV1> {
        self.saved.lock().expect("save log poisoned").clone()
    }
}

#[async_trait::async_trait]
impl StateStore for RecordingStore {
    async fn load(&self) -> std::result::Result<LoadOutcome, PersistError> {
        self.inner.load().await
    }

    async fn save(&self, state: &PersistedStateV1) -> std::result::Result<(), PersistError> {
        if self.fail_saves.load(Ordering::Relaxed) {
            return Err(PersistError::Replace("simulated disk failure".to_owned()));
        }
        self.saved
            .lock()
            .expect("save log poisoned")
            .push(state.clone());
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

/// Deterministic harness: temp directories, fake seams, and a CLOSED start
/// gate so every test enqueues its whole command scenario before the actor
/// begins consuming Ã¢â‚¬â€ eliminating scheduler races around coalescing batches.
struct BackendRig {
    dir: TempDir,
    gate: Arc<TestStartGate>,
    factory: Arc<FakeSessionTransportFactory>,
    store: Option<Arc<RecordingStore>>,
    handle: Option<DeviceBackendHandle>,
    updates: Option<DeviceBackendUpdates>,
}

impl BackendRig {
    /// Starts a gated rig with an idle-open discovery stream.
    fn start() -> Self {
        Self::start_inner(Vec::new(), false, None)
    }

    /// Starts a gated rig whose first discovery generation replays `script`.
    fn start_with_discovery(script: Vec<BrowseItem>) -> Self {
        Self::start_inner(script, false, None)
    }

    /// Starts a gated rig whose durable writes fail while loads succeed.
    fn start_with_failing_store() -> Self {
        Self::start_inner(Vec::new(), true, None)
    }

    /// Starts a gated rig over a state directory a previous launch left
    /// behind, carrying the given schema-v2 calibration section.
    fn start_with_persisted_calibration(
        script: Vec<BrowseItem>,
        section: homepod_cast::backend::persistence::CalibrationStateV2,
    ) -> Self {
        let document = homepod_cast::backend::persistence::PersistedStateV2 {
            calibration: section,
            ..homepod_cast::backend::persistence::PersistedStateV2::default()
        };
        Self::start_inner(
            script,
            false,
            Some(
                String::from_utf8(serde_json::to_vec_pretty(&document).expect("state serializes"))
                    .expect("serialized state is UTF-8"),
            ),
        )
    }

    /// Starts a gated rig over a state directory containing exactly these
    /// bytes, however malformed.
    ///
    /// Separate from the typed seeding above because the interesting cases
    /// are precisely the ones the typed shape cannot express.
    fn start_with_state_document(script: Vec<BrowseItem>, document: &str) -> Self {
        Self::start_inner(script, false, Some(document.to_owned()))
    }

    fn start_inner(
        discovery_script: Vec<BrowseItem>,
        fail_saves: bool,
        seed_document: Option<String>,
    ) -> Self {
        Self::start_with_capture(
            discovery_script,
            fail_saves,
            seed_document,
            Arc::new(LocalHangCaptureSource),
        )
    }

    fn start_with_capture(
        discovery_script: Vec<BrowseItem>,
        fail_saves: bool,
        seed_document: Option<String>,
        capture: Arc<dyn CaptureSource>,
    ) -> Self {
        let dir = TempDir::new().expect("temp dir");
        if let Some(document) = seed_document {
            let state_directory = dir.path().join("OpenAirCast");
            std::fs::create_dir_all(&state_directory).expect("state directory");
            std::fs::write(state_directory.join("state-v1.json"), document)
                .expect("seed the state file");
        }
        let gate = TestStartGate::closed();
        let discovery = LocalFakeDiscoverySource::new(discovery_script);
        let factory = FakeSessionTransportFactory::new();
        let store = Arc::new(RecordingStore::new(
            dir.path().join("OpenAirCast"),
            dir.path().join("HomePodCast").join("volume.txt"),
        ));
        store.fail_saves.store(fail_saves, Ordering::Relaxed);
        let network_seam = backend_rig::FakeNetwork::new();

        let config = BackendConfig {
            state_directory: dir.path().join("OpenAirCast"),
            legacy_volume_path: dir.path().join("HomePodCast").join("volume.txt"),
        };
        let overrides = BackendOverrides {
            discovery: Some(discovery.clone()),
            capture: Some(capture),
            session_factory: Some(factory.clone()),
            decoders: None,
            store: Some(store.clone()),
            start_gate: Some(gate.clone()),
            probe_interval: None,
            clock: None,
            timer: None,
            // Never absent: `BackendOverrides` reads a missing pair as "use
            // the platform seam", so `None` here would subscribe this test
            // process to the host's `NotifyIpInterfaceChange` and let the
            // machine's own network activity restart discovery and rebuild the
            // group underneath the test. The double is inert -- nothing in
            // this file ever fires it.
            network: Some(Arc::clone(&network_seam)
                as Arc<dyn homepod_cast::backend::network::NetworkChangeSource>),
            local_bindings: Some(
                network_seam as Arc<dyn homepod_cast::backend::network::LocalBindingSource>,
            ),
        };
        let (handle, updates) = start_device_backend_with(config, overrides)
            .expect("backend starts with injected fakes");
        Self {
            dir,
            gate,
            factory,
            store: Some(store),
            handle: Some(handle),
            updates: Some(updates),
        }
    }

    fn handle(&self) -> &DeviceBackendHandle {
        self.handle.as_ref().expect("handle present until shutdown")
    }

    fn snapshot(&self) -> Arc<DeviceSnapshot> {
        self.updates
            .as_ref()
            .expect("updates present until shutdown")
            .state
            .borrow()
            .clone()
    }

    /// Sends a command through the awaiting path and returns its ID.
    async fn send(&self, command: BackendCommand) -> u64 {
        self.handle()
            .send(command)
            .await
            .expect("backend accepts commands until shutdown")
    }

    /// Polls the watch snapshot until `pred` holds or the timeout elapses.
    async fn wait_for(&self, pred: impl Fn(&DeviceSnapshot) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if pred(&self.snapshot()) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition not reached in time; last snapshot: {:?}",
                self.snapshot()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Waits until session setup was handed `expected`, or says what it was
    /// handed instead.
    ///
    /// The factory log is a side channel with no happens-before relation to
    /// `BackendEvent::CommandCompleted`: the controller completes a calibration
    /// command *before* it owes the restart, and the restart hands the profile
    /// to a task `spawn_start` owns. Sampling the log the instant the
    /// completion arrives therefore reads the previous attempt -- or none at
    /// all -- whenever the machine is busy, which is what made these
    /// assertions fail under a loaded parallel run while passing in isolation.
    /// The claim itself is unchanged: what session setup is handed has to be
    /// this profile, within the same budget every other wait here uses.
    async fn wait_for_calibration(&self, expected: &homepod_cast::calibration::CalibrationProfile) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let seen = self.factory.last_calibration();
            if seen.as_ref() == Some(expected) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "session setup was never handed {expected:?}; the last attempt carried {seen:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Drains currently buffered shell events.
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        if let Some(updates) = self.updates.as_mut() {
            loop {
                match updates.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
        events
    }

    /// Waits until `pred` matches some drained shell event.
    async fn wait_for_event(&mut self, pred: impl Fn(&BackendEvent) -> bool + Copy) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if self.drain_events().iter().any(pred) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected event not observed in time"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Drains currently buffered diagnostic payloads.
    fn drain_diagnostics(&mut self) -> Vec<DiagnosticPayload> {
        let mut payloads = Vec::new();
        if let Some(updates) = self.updates.as_mut() {
            // One pass drains the backlog; `try_recv` reports `Empty` as soon
            // as it is exhausted. Wrapping this in an outer `loop` would spin
            // on `Empty` forever instead of returning.
            while let Ok(event) = updates.diagnostics.try_recv() {
                payloads.push(event.payload);
            }
        }
        payloads
    }

    /// Reads the persisted calibration section straight off disk.
    ///
    /// `None` covers both "no file yet" and "still a schema-v1 document",
    /// which are exactly the two states in which nothing was written.
    fn calibration_from_disk(
        &self,
    ) -> Option<homepod_cast::backend::persistence::CalibrationStateV2> {
        let path = self.dir.path().join("OpenAirCast").join("state-v1.json");
        let bytes = std::fs::read(&path).ok()?;
        let document: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        serde_json::from_value(document.get("calibration")?.clone()).ok()
    }

    fn persisted_from_disk(&self) -> PersistedStateV1 {
        let path = self.dir.path().join("OpenAirCast").join("state-v1.json");
        let bytes = std::fs::read(&path).expect("state-v1.json exists on disk");
        serde_json::from_slice(&bytes).expect("persisted state parses")
    }

    /// Opens the gate, requests graceful shutdown, and joins the controller
    /// thread in a deterministic order (updates, then handle, then dirs).
    ///
    /// Completion events may already have been consumed by earlier event
    /// assertions, so synchronization relies on the lifecycle guard joining
    /// the control thread when the last handle drops.
    async fn shutdown(mut self) {
        self.gate.open();
        if let Some(handle) = self.handle.as_ref() {
            let _ = handle.send(BackendCommand::Shutdown).await;
        }
        // Brief grace period so in-flight durable writes land before the
        // blocking join below.
        tokio::time::sleep(Duration::from_millis(250)).await;
        drop(self.updates.take());
        drop(self.handle.take());
    }
}

#[tokio::test]
async fn endpoint_inventory_is_published_without_starting_playback() {
    let rig = BackendRig::start();
    rig.gate.open();
    rig.wait_for(|snapshot| {
        snapshot
            .audio_source
            .active_endpoints
            .as_ref()
            .is_some_and(|endpoints| {
                endpoints
                    .iter()
                    .any(|endpoint| endpoint.id == "endpoint-test")
            })
    })
    .await;
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn release_group_start_then_stop_in_one_batch_never_starts() {
    let rig = BackendRig::start_with_discovery(vec![Ok(BrowseEvent::Added(castable_device(
        8, "Office",
    )))]);
    rig.send(BackendCommand::SaveGroup {
        id: Some(group(9)),
        name: "Evening".into(),
        members: vec![SavedGroupMember {
            receiver: rid(8),
            last_known_name: "Office".into(),
            level: volume(0.6),
        }],
    })
    .await;
    rig.send(BackendCommand::ActivateSavedGroup {
        id: group(9),
        start: true,
    })
    .await;
    let stop = rig
        .send(BackendCommand::SetRunIntent(RunIntent::Stopped))
        .await;
    rig.gate.open();
    rig.wait_for(|snapshot| {
        snapshot.saved_groups.len() == 1
            && snapshot.desired_members == BTreeSet::from([rid(8)])
            && snapshot
                .command_outcomes
                .recent
                .iter()
                .any(|(id, _)| *id == stop)
    })
    .await;
    let barrier = rig.send(BackendCommand::DeleteGroup(group(1))).await;
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, _)| *id == barrier)
    })
    .await;
    assert_eq!(rig.snapshot().run_intent, RunIntent::Stopped);
    assert_eq!(rig.snapshot().session.phase, SessionPhase::Stopped);
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn release_group_start_after_stop_remains_the_latest_intent() {
    for earlier_start in [false, true] {
        let rig = BackendRig::start();
        for seed in [8, 9] {
            rig.send(BackendCommand::SaveGroup {
                id: Some(group(seed)),
                name: format!("Group {seed}"),
                members: vec![SavedGroupMember {
                    receiver: rid(seed),
                    last_known_name: format!("Receiver {seed}"),
                    level: volume(0.6),
                }],
            })
            .await;
        }
        if earlier_start {
            rig.send(BackendCommand::ActivateSavedGroup {
                id: group(8),
                start: true,
            })
            .await;
        }
        rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
            .await;
        let latest = rig
            .send(BackendCommand::ActivateSavedGroup {
                id: group(9),
                start: true,
            })
            .await;
        rig.gate.open();
        rig.wait_for(|snapshot| {
            snapshot
                .command_outcomes
                .recent
                .iter()
                .any(|(id, _)| *id == latest)
        })
        .await;
        assert_eq!(rig.snapshot().run_intent, RunIntent::Running);
        assert_eq!(rig.snapshot().desired_members, BTreeSet::from([rid(9)]));
        rig.shutdown().await;
    }
}

#[tokio::test]
async fn saved_group_outcomes_survive_broadcast_lag_and_validation_noops() {
    let rig = BackendRig::start();
    rig.gate.open();
    let failed = rig
        .send(BackendCommand::SaveGroup {
            id: None,
            name: " ".into(),
            members: vec![],
        })
        .await;
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == failed && error.is_some())
    })
    .await;
    let saved = rig
        .send(BackendCommand::SaveGroup {
            id: Some(group(9)),
            name: "Evening".into(),
            members: vec![SavedGroupMember {
                receiver: rid(8),
                last_known_name: "Offline".into(),
                level: volume(0.6),
            }],
        })
        .await;
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == saved && error.is_none())
    })
    .await;
    assert_eq!(rig.snapshot().saved_groups[0].id, group(9));
    assert_eq!(rig.factory.start_count(), 0);
    let applied = rig
        .send(BackendCommand::ActivateSavedGroup {
            id: group(9),
            start: false,
        })
        .await;
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == applied && error.is_none())
    })
    .await;
    assert_eq!(rig.snapshot().desired_members, BTreeSet::from([rid(8)]));
    assert_eq!(rig.snapshot().receiver_levels[&rid(8)], volume(0.6));
    assert_eq!(rig.snapshot().run_intent, RunIntent::Stopped);
    let reloaded = rig.persisted_from_disk();
    assert_eq!(
        reloaded.saved_groups[0].members[0].last_known_name,
        "Offline"
    );
    assert_eq!(reloaded.last_desired_members, BTreeSet::from([rid(8)]));
    let deleted = rig.send(BackendCommand::DeleteGroup(group(9))).await;
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == deleted && error.is_none())
    })
    .await;
    assert!(rig.snapshot().saved_groups.is_empty());
    assert!(rig.persisted_from_disk().saved_groups.is_empty());
    assert_eq!(rig.snapshot().desired_members, BTreeSet::from([rid(8)]));
    assert_eq!(rig.factory.start_count(), 0);
    // Deliberately never drain the broadcast: exceed its 256-slot capacity
    // with accepted no-ops and then inspect only the watch snapshot.
    let mut last = 0;
    for _ in 0..260 {
        last = rig.send(BackendCommand::DeleteGroup(group(9))).await;
    }
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == last && error.is_none())
    })
    .await;
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.command_outcomes.recent.len(), 128);
    assert!(snapshot.command_outcomes.evicted_through >= deleted);
    rig.shutdown().await;
}

#[tokio::test]
async fn saved_group_exact_receipts_survive_eviction_and_follow_atomic_state() {
    use homepod_cast::backend::command::ReceiptState;
    use homepod_cast::backend::model::CommandFailure;
    let rig = BackendRig::start();
    rig.gate.open();
    let (_, invalid) = rig
        .handle()
        .try_send_confirmed(BackendCommand::SaveGroup {
            id: None,
            name: " ".into(),
            members: vec![],
        })
        .unwrap();
    let invalid = std::cell::RefCell::new(invalid);
    rig.wait_for(|snapshot| {
        invalid.borrow_mut().poll(snapshot.revision)
            == ReceiptState::Completed(Some(CommandFailure::Rejected))
    })
    .await;
    let (_, saved) = rig
        .handle()
        .try_send_confirmed(BackendCommand::SaveGroup {
            id: Some(group(9)),
            name: "Evening".into(),
            members: vec![SavedGroupMember {
                receiver: rid(8),
                last_known_name: "Offline".into(),
                level: volume(0.6),
            }],
        })
        .unwrap();
    let saved = std::cell::RefCell::new(saved);
    rig.wait_for(|snapshot| {
        saved.borrow_mut().poll(snapshot.revision) == ReceiptState::Completed(None)
    })
    .await;
    assert_eq!(rig.snapshot().saved_groups[0].id, group(9));
    let (applied, mut receipt) = rig
        .handle()
        .try_send_confirmed(BackendCommand::ActivateSavedGroup {
            id: group(9),
            start: false,
        })
        .unwrap();
    rig.wait_for(|snapshot| {
        snapshot.desired_members == BTreeSet::from([rid(8)])
            && snapshot.receiver_levels.get(&rid(8)) == Some(&volume(0.6))
    })
    .await;
    let mut last = 0;
    for _ in 0..260 {
        last = rig.send(BackendCommand::DeleteGroup(group(1))).await;
    }
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, _)| *id == last)
    })
    .await;
    let snapshot = rig.snapshot();
    assert!(!snapshot
        .command_outcomes
        .recent
        .iter()
        .any(|(id, _)| *id == applied));
    assert_eq!(
        receipt.poll(snapshot.revision),
        ReceiptState::Completed(None)
    );
    assert_eq!(snapshot.desired_members, BTreeSet::from([rid(8)]));
    assert_eq!(snapshot.receiver_levels[&rid(8)], volume(0.6));
    assert_eq!(snapshot.run_intent, RunIntent::Stopped);
    assert_eq!(
        rig.persisted_from_disk().last_desired_members,
        snapshot.desired_members
    );
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn release_group_receipt_names_the_real_empty_attempt_and_reuses_it_for_noop() {
    use homepod_cast::backend::command::ReceiptState;
    let rig = BackendRig::start();
    rig.send(BackendCommand::SaveGroup {
        id: Some(group(9)),
        name: "Offline".into(),
        members: vec![SavedGroupMember {
            receiver: rid(8),
            last_known_name: "Offline".into(),
            level: volume(0.6),
        }],
    })
    .await;
    let (_, receipt) = rig
        .handle()
        .try_send_confirmed(BackendCommand::ActivateSavedGroup {
            id: group(9),
            start: true,
        })
        .unwrap();
    let receipt = std::cell::RefCell::new(receipt);
    rig.gate.open();
    rig.wait_for(|snapshot| {
        receipt.borrow_mut().poll(snapshot.revision) == ReceiptState::Completed(None)
    })
    .await;
    let floor = receipt
        .borrow()
        .session_generation_floor(rig.snapshot().revision)
        .unwrap();
    assert!(floor > 0);
    rig.wait_for(|snapshot| {
        matches!(snapshot.session.phase,
        SessionPhase::Restarting { generation, .. } if generation == floor)
    })
    .await;
    assert_eq!(rig.snapshot().run_intent, RunIntent::Running);
    assert_eq!(
        rig.factory.start_count(),
        0,
        "no available receivers must not fabricate a transport"
    );
    let (_, noop) = rig
        .handle()
        .try_send_confirmed(BackendCommand::ActivateSavedGroup {
            id: group(9),
            start: true,
        })
        .unwrap();
    let noop = std::cell::RefCell::new(noop);
    rig.wait_for(|snapshot| {
        noop.borrow_mut().poll(snapshot.revision) == ReceiptState::Completed(None)
    })
    .await;
    assert_eq!(
        noop.borrow()
            .session_generation_floor(rig.snapshot().revision),
        Some(floor)
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn release_group_receipt_names_the_real_preflight_failure_attempt() {
    use homepod_cast::backend::command::ReceiptState;
    let rig = BackendRig::start_with_discovery(vec![Ok(BrowseEvent::Added(castable_device(
        8, "Kitchen",
    )))]);
    rig.gate.open();
    rig.wait_for(|snapshot| {
        snapshot
            .receivers
            .iter()
            .any(|receiver| receiver.id == rid(8))
    })
    .await;
    rig.send(BackendCommand::SaveGroup {
        id: Some(group(9)),
        name: "Kitchen".into(),
        members: vec![SavedGroupMember {
            receiver: rid(8),
            last_known_name: "Kitchen".into(),
            level: volume(0.6),
        }],
    })
    .await;
    let (_, receipt) = rig
        .handle()
        .try_send_confirmed(BackendCommand::ActivateSavedGroup {
            id: group(9),
            start: true,
        })
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
    assert!(floor > 0);
    rig.wait_for(|snapshot| {
        matches!(snapshot.session.phase,
        SessionPhase::Restarting { generation, .. } if generation == floor)
    })
    .await;
    assert_eq!(
        rig.factory.start_count(),
        1,
        "the fake factory rejected this actual attempt"
    );
    assert!(rig.snapshot().session.active.is_empty());
    rig.shutdown().await;
}

#[tokio::test]
async fn saved_group_persistence_failure_is_retained_without_publishing_candidate() {
    let rig = BackendRig::start_with_failing_store();
    rig.gate.open();
    let (saved, receipt) = rig
        .handle()
        .try_send_confirmed(BackendCommand::SaveGroup {
            id: None,
            name: "Evening".into(),
            members: vec![SavedGroupMember {
                receiver: rid(8),
                last_known_name: "Offline".into(),
                level: volume(0.6),
            }],
        })
        .unwrap();
    rig.wait_for(|snapshot| {
        snapshot.command_outcomes.recent.iter().any(|(id, error)| {
            *id == saved
                && *error == Some(homepod_cast::backend::model::CommandFailure::Persistence)
        })
    })
    .await;
    let receipt = std::cell::RefCell::new(receipt);
    rig.wait_for(|snapshot| {
        receipt.borrow_mut().poll(snapshot.revision)
            == homepod_cast::backend::command::ReceiptState::Completed(Some(
                homepod_cast::backend::model::CommandFailure::Persistence,
            ))
    })
    .await;
    assert!(rig.snapshot().saved_groups.is_empty());
    assert!(rig.snapshot().desired_members.is_empty());
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn saved_group_explicit_selection_applies_in_order_without_coalescing_or_starting() {
    let rig = BackendRig::start();
    let first = rig
        .send(BackendCommand::ApplyDesiredMembers {
            members: BTreeSet::from([rid(1)]),
        })
        .await;
    let second = rig
        .send(BackendCommand::ApplyDesiredMembers {
            members: BTreeSet::from([rid(2), rid(3)]),
        })
        .await;
    rig.gate.open();
    rig.wait_for(|snapshot| {
        snapshot
            .command_outcomes
            .recent
            .iter()
            .any(|(id, error)| *id == second && error.is_none())
    })
    .await;
    assert!(rig
        .snapshot()
        .command_outcomes
        .recent
        .iter()
        .any(|(id, error)| *id == first && error.is_none()));
    assert_eq!(rig.snapshot().desired_revision, 2);
    assert_eq!(
        rig.snapshot().desired_members,
        BTreeSet::from([rid(2), rid(3)])
    );
    assert_eq!(
        rig.persisted_from_disk().last_desired_members,
        BTreeSet::from([rid(2), rid(3)])
    );
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

struct BlockingInventory {
    release: AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl CaptureSource for BlockingInventory {
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(Vec::new())
    }

    async fn open(
        &self,
        preference: &AudioEndpointPreference,
    ) -> Result<Box<dyn CaptureWorker>, CaptureError> {
        LocalHangCaptureSource.open(preference).await
    }
}

#[tokio::test]
async fn endpoint_inventory_hung_scan_keeps_commands_live_and_coalesces_refreshes() {
    let source = Arc::new(BlockingInventory {
        release: AtomicBool::new(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let rig = BackendRig::start_with_capture(Vec::new(), false, None, source.clone());
    rig.gate.open();
    // Always release the synthetic native call on panic, so the test cannot leak a worker.
    struct Release(Arc<BlockingInventory>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release.store(true, Ordering::SeqCst);
        }
    }
    let _release = Release(source.clone());
    rig.wait_for(|_| source.calls.load(Ordering::SeqCst) == 1)
        .await;
    assert!(rig.snapshot().audio_source.active_endpoints.is_none());
    for index in 0..10 {
        let id = format!("chosen-{index}");
        rig.send(BackendCommand::SetAudioEndpoint(
            AudioEndpointPreference::Explicit {
                id: id.clone(),
                last_known_name: id.clone(),
            },
        ))
        .await;
        rig.wait_for(|snapshot| matches!(&snapshot.audio_source.preference, AudioEndpointPreference::Explicit { id: current, .. } if current == &id)).await;
    }
    rig.send(BackendCommand::SetMuted(true)).await;
    rig.wait_for(|snapshot| snapshot.muted).await;
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "a hung scan must never spawn replacements"
    );
    source.release.store(true, Ordering::SeqCst);
    rig.wait_for(|snapshot| snapshot.audio_source.active_endpoints == Some(Vec::new()))
        .await;
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

struct TerminatedInventory;

#[async_trait::async_trait]
impl CaptureSource for TerminatedInventory {
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        panic!("synthetic scanner termination");
    }
    async fn open(
        &self,
        preference: &AudioEndpointPreference,
    ) -> Result<Box<dyn CaptureWorker>, CaptureError> {
        LocalHangCaptureSource.open(preference).await
    }
}

#[tokio::test]
async fn endpoint_inventory_terminated_worker_publishes_failure() {
    let rig =
        BackendRig::start_with_capture(Vec::new(), false, None, Arc::new(TerminatedInventory));
    rig.gate.open();
    rig.wait_for(|snapshot| snapshot.audio_source.refresh_failed)
        .await;
    assert!(rig.snapshot().audio_source.active_endpoints.is_none());
    assert_eq!(rig.factory.start_count(), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn endpoint_inventory_shutdown_completes_while_native_scan_is_still_blocked() {
    let source = Arc::new(BlockingInventory {
        release: AtomicBool::new(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let rig = BackendRig::start_with_capture(Vec::new(), false, None, source.clone());
    rig.gate.open();
    // A watchdog releases only on a broken shutdown so a failing test cannot hang.
    let rescue = source.clone();
    let (finished, observe) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if observe.recv_timeout(Duration::from_secs(3)).is_err() {
            rescue.release.store(true, Ordering::SeqCst);
        }
    });
    rig.wait_for(|_| source.calls.load(Ordering::SeqCst) == 1)
        .await;
    rig.shutdown().await;
    let stayed_blocked = !source.release.load(Ordering::SeqCst);
    source.release.store(true, Ordering::SeqCst);
    let _ = finished.send(());
    watchdog.join().unwrap();
    assert!(
        stayed_blocked,
        "shutdown waited for the watchdog to free the native scan"
    );
}

mod controller_task15 {
    use super::*;

    #[tokio::test]
    async fn master_receipt_follows_durable_winner_and_relaunch_restores_it() {
        use homepod_cast::backend::command::ReceiptState;
        for fail in [false, true] {
            let persisted = PersistedStateV1 {
                master_volume: volume(0.8),
                ..PersistedStateV1::default()
            };
            let rig = BackendRig::start_inner(
                vec![],
                fail,
                Some(serde_json::to_string(&persisted).unwrap()),
            );
            let (_, mut receipt) = rig
                .handle()
                .try_send_confirmed(BackendCommand::SetMasterVolume(volume(0.4)))
                .unwrap();
            // The normal Start batch may replace the same scalar without its
            // own receipt; the latest drag must still await that actual write.
            rig.handle()
                .try_send(BackendCommand::SetMasterVolume(volume(0.6)))
                .unwrap();
            assert_eq!(receipt.poll(u64::MAX), ReceiptState::Pending);
            rig.gate.open();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            let result = loop {
                let result = receipt.poll(rig.snapshot().revision);
                if result != ReceiptState::Pending {
                    break result;
                }
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_eq!(
                result,
                ReceiptState::Completed(
                    fail.then_some(homepod_cast::backend::model::CommandFailure::Persistence)
                )
            );
            assert_eq!(
                rig.snapshot().master_volume,
                volume(if fail { 0.8 } else { 0.6 })
            );
            let document = serde_json::to_string(&rig.persisted_from_disk()).unwrap();
            rig.shutdown().await;
            let reloaded = BackendRig::start_with_state_document(vec![], &document);
            reloaded.gate.open();
            reloaded
                .wait_for(|snapshot| snapshot.master_volume == volume(if fail { 0.8 } else { 0.6 }))
                .await;
            reloaded.shutdown().await;
        }
    }

    #[tokio::test]
    async fn startup_returns_handle_and_all_three_update_feeds() {
        let mut rig = BackendRig::start();

        // While the start gate is closed the actor publishes nothing: the
        // initial watch value stays the untouched revision-zero snapshot.
        let first = rig.snapshot();
        assert_eq!(first.revision, 0);
        assert_eq!(first.desired_revision, 0);
        assert!(matches!(
            rig.updates.as_mut().unwrap().events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            rig.updates.as_mut().unwrap().diagnostics.try_recv(),
            Err(DiagnosticTryRecvError::Empty)
        ));

        rig.gate.open();
        let id = rig.handle().send(BackendCommand::Shutdown).await.unwrap();
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandCompleted { id: completed } if *completed == id)
        })
        .await;

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn set_desired_members_bumps_revisions_and_persists_state_v1() {
        let mut rig = BackendRig::start();
        rig.gate.open();

        let mut members = BTreeSet::new();
        members.insert(rid(1));
        members.insert(rid(2));
        let id = rig
            .send(BackendCommand::SetDesiredMembers {
                members: members.clone(),
            })
            .await;

        rig.wait_for(|snapshot| {
            snapshot.desired_revision == 1 && snapshot.desired_members == members
        })
        .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandCompleted { id: completed } if *completed == id)
        })
        .await;

        // Desired members stay selected but unavailable before discovery.
        let snapshot = rig.snapshot();
        assert!(
            snapshot.revision >= 1,
            "semantic change bumped the revision"
        );
        assert_eq!(snapshot.receivers.len(), 2);
        assert!(snapshot.receivers.iter().all(
            |row| row.lifecycle == homepod_cast::backend::model::ReceiverLifecycle::Unavailable
        ));

        let persisted = rig.persisted_from_disk();
        assert_eq!(persisted.version, 1);
        assert_eq!(persisted.last_desired_members, members);

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn volume_coalesces_to_newest_under_rapid_commands() {
        let mut rig = BackendRig::start();

        // Enqueue five volumes while the actor is parked behind the gate so
        // they drain as ONE coalescing batch: newest wins.
        let mut ids = Vec::new();
        for step in 1..=5_u32 {
            let id = rig
                .handle()
                .try_send(BackendCommand::SetMasterVolume(volume(step as f32 / 10.0)))
                .expect("queue has room");
            ids.push(id);
        }
        rig.gate.open();

        rig.wait_for(|snapshot| snapshot.master_volume == volume(0.5))
            .await;
        // Only the winning (newest) envelope completes; the four superseded
        // ones are reported merged through the diagnostics feed below.
        let newest = *ids.last().expect("five ids");
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandCompleted { id: completed } if *completed == newest)
        })
        .await;

        // Exactly one effective change: exactly one save, carrying the newest
        // value; no intermediate value ever became durable.
        let saved = rig.store.as_ref().unwrap().saved();
        assert_eq!(saved.len(), 1, "exactly one durable write");
        assert_eq!(saved[0].master_volume, volume(0.5));
        assert_eq!(rig.persisted_from_disk().master_volume, volume(0.5));
        assert_eq!(rig.snapshot().master_volume, volume(0.5));

        // The merge is visible in the diagnostics feed.
        let coalesced = rig
            .drain_diagnostics()
            .iter()
            .any(|payload| matches!(payload, DiagnosticPayload::CommandCoalesced { dropped: 4 }));
        assert!(coalesced, "four superseded volumes were reported merged");

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn edges_execute_in_arrival_order_while_scalars_coalesce() {
        let mut rig = BackendRig::start();

        // Interleave scalars and edges: both volumes coalesce to 0.7, and the
        // save/delete edge pair must execute strictly in arrival order Ã¢â‚¬â€ if
        // the delete ran first, group ALPHA would survive.
        let alpha = group(0xA1);
        let save_id = rig
            .handle()
            .try_send(BackendCommand::SaveGroup {
                id: Some(alpha),
                name: "Alpha".to_owned(),
                members: vec![SavedGroupMember {
                    receiver: rid(1),
                    last_known_name: "Kitchen".to_owned(),
                    level: Volume::UNITY,
                }],
            })
            .expect("queue has room");
        let delete_id = rig
            .handle()
            .try_send(BackendCommand::DeleteGroup(alpha))
            .expect("queue has room");
        let _volume_one = rig
            .handle()
            .try_send(BackendCommand::SetMasterVolume(volume(0.9)))
            .expect("queue has room");
        let volume_two = rig
            .handle()
            .try_send(BackendCommand::SetMasterVolume(volume(0.7)))
            .expect("queue has room");
        rig.gate.open();

        rig.wait_for(|snapshot| {
            snapshot.master_volume == volume(0.7) && snapshot.saved_groups.is_empty()
        })
        .await;

        // Completions arrive for the surviving work items only (the 0.9
        // scalar was merged away); accumulate the drained stream because
        // completions interleave across polls.
        let expected = [save_id, delete_id, volume_two];
        let mut completions: Vec<u64> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !expected.iter().all(|id| completions.contains(id)) {
            completions.extend(
                rig.drain_events()
                    .into_iter()
                    .filter_map(|event| match event {
                        BackendEvent::CommandCompleted { id } => {
                            expected.contains(&id).then_some(id)
                        }
                        _ => None,
                    }),
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "completions for edges and winning scalar not observed in time"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Scalars coalesced: only the newest volume ever became durable.
        let saved = rig.store.as_ref().unwrap().saved();
        assert!(
            saved.iter().all(|state| state.master_volume == volume(0.7)),
            "no intermediate volume was persisted"
        );

        // Edges kept arrival order: save completed before delete.
        let position = |id: u64| {
            completions
                .iter()
                .position(|completed| *completed == id)
                .expect("completion was collected above")
        };
        let save_position = position(save_id);
        let delete_position = position(delete_id);
        assert!(
            save_position < delete_position,
            "edge commands executed in arrival order"
        );

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn saved_group_activation_is_atomic_and_persists_before_publish() {
        let mut rig = BackendRig::start_with_discovery(vec![
            Ok(BrowseEvent::Added(castable_device(1, "Kitchen"))),
            Ok(BrowseEvent::Added(castable_device(2, "Pantry"))),
        ]);
        rig.gate.open();

        // Wait until both receivers are known so activation can resolve real
        // devices for the follow-up reconciliation.
        rig.wait_for(|snapshot| {
            snapshot
                .receivers
                .iter()
                .filter(|row| row.id == rid(1) || row.id == rid(2))
                .count()
                == 2
        })
        .await;

        let evening = rig
            .send(BackendCommand::SaveGroup {
                id: None,
                name: "Evening".to_owned(),
                members: vec![
                    SavedGroupMember {
                        receiver: rid(1),
                        last_known_name: "Kitchen".to_owned(),
                        level: volume(0.4),
                    },
                    SavedGroupMember {
                        receiver: rid(2),
                        last_known_name: "Pantry".to_owned(),
                        level: volume(0.8),
                    },
                ],
            })
            .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandCompleted { id: completed } if *completed == evening)
        })
        .await;
        assert_eq!(
            rig.factory.start_count(),
            0,
            "saving a group never starts a session generation"
        );

        // The completion edge leaves the actor before the publication does
        // (`complete` then `publish`), so the saved group has to be waited
        // for rather than read straight off the back of the event.
        rig.wait_for(|snapshot| snapshot.saved_groups.len() == 1)
            .await;
        let group_id = rig.snapshot().saved_groups[0].id;
        rig.send(BackendCommand::ActivateSavedGroup {
            id: group_id,
            start: true,
        })
        .await;

        rig.wait_for(|snapshot| {
            snapshot.desired_members == BTreeSet::from([rid(1), rid(2)])
                && snapshot.run_intent == RunIntent::Running
        })
        .await;

        // Exactly one desired-revision bump for the whole transaction.
        assert_eq!(rig.snapshot().desired_revision, 1);

        // Atomicity: ONE save carried members and levels together, and the
        // on-disk file agrees.
        let saved = rig.store.as_ref().unwrap().saved();
        let transactions = saved
            .iter()
            .filter(|state| !state.last_desired_members.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(transactions.len(), 1, "one atomic activation transaction");
        assert_eq!(
            transactions[0].last_desired_members,
            BTreeSet::from([rid(1), rid(2)])
        );
        assert_eq!(
            transactions[0].receiver_levels.get(&rid(1)),
            Some(&volume(0.4))
        );
        assert_eq!(
            transactions[0].receiver_levels.get(&rid(2)),
            Some(&volume(0.8))
        );
        assert_eq!(
            rig.snapshot().receiver_levels,
            transactions[0].receiver_levels,
            "activation publishes the complete committed level map"
        );
        let on_disk = rig.persisted_from_disk();
        assert_eq!(
            on_disk.last_desired_members,
            transactions[0].last_desired_members
        );
        assert_eq!(on_disk.receiver_levels, transactions[0].receiver_levels);

        // Exactly one new session generation, against both committed values.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while rig.factory.start_count() < 1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "activation never reached the session transport factory"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let sets = rig.factory.started_sets();
        assert_eq!(sets.len(), 1, "at most one generation after activation");
        let mut started = sets[0].clone();
        started.sort_unstable();
        assert_eq!(started, vec![[0, 0, 0, 0, 0, 1], [0, 0, 0, 0, 0, 2]]);

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn save_failure_retains_old_state_and_emits_command_failed_with_persistence_notice() {
        let mut rig = BackendRig::start_with_failing_store();
        rig.gate.open();

        let id = rig.send(BackendCommand::SetMasterVolume(volume(0.9))).await;

        // The failure notice precedes the failure event, so accumulate the
        // drained stream until both signals appeared.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let (mut failed_seen, mut notice_seen, mut completed_seen) = (false, false, false);
        while !(failed_seen && notice_seen) {
            for event in rig.drain_events() {
                match event {
                    BackendEvent::CommandFailed { id: failed, .. } if failed == id => {
                        failed_seen = true;
                    }
                    BackendEvent::CommandCompleted { id: completed } if completed == id => {
                        completed_seen = true;
                    }
                    BackendEvent::Notice {
                        severity: Severity::Error,
                        code: NoticeCode::PersistenceWrite,
                        scope: ErrorScope::Persistence,
                        ..
                    } => notice_seen = true,
                    _ => {}
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "failure event and persistence notice not observed in time"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        rig.wait_for(|snapshot| snapshot.persistence == PersistenceSnapshot::Error)
            .await;

        // Old value retained; the failed command never reports completion.
        let snapshot = rig.snapshot();
        assert_eq!(snapshot.master_volume, Volume::DEFAULT_MASTER);
        assert!(!completed_seen, "failed commands never report completion");

        // The durable value is untouched by the failed attempt: with every
        // write failing, no state file can have been created or modified.
        let state_path = self_dir_state_file(&rig);
        assert!(
            !state_path.exists(),
            "failed writes must never create or modify state-v1.json"
        );
        assert!(
            rig.drain_diagnostics().iter().any(|payload| matches!(
                payload,
                DiagnosticPayload::Persistence {
                    outcome: PersistenceSnapshot::Error
                }
            )),
            "the diagnostics feed records the failed persistence cycle"
        );

        rig.shutdown().await;
    }
}

/// Path of the schema-v1 state file inside a rig's temporary directory.
fn self_dir_state_file(rig: &BackendRig) -> std::path::PathBuf {
    rig.dir.path().join("OpenAirCast").join("state-v1.json")
}

// ---------------------------------------------------------------------------
// Task 19 Step 6: fifty-cycle leak and stale-generation soak
// ---------------------------------------------------------------------------

/// Cycles the soak performs. Fifty is the plan's number; every window inside a
/// cycle is fake time, so the count costs scheduling, not wall clock.
const SOAK_CYCLES: usize = 50;

/// Interface-settle and discovery-stability period, identical by design.
const SOAK_SETTLE: Duration = Duration::from_secs(2);

/// The rig's second render endpoint, so an endpoint switch is observable.
const SOAK_OTHER_ENDPOINT: &str = "endpoint-other";
/// The rig's default render endpoint.
const SOAK_DEFAULT_ENDPOINT: &str = "endpoint-test";

/// Step 6. Fifty full lifecycles, then count what is left over.
///
/// One cycle stops and restarts the session, moves capture to the other
/// render endpoint, kills the browse daemon and lets it reconnect, and puts
/// the machine to sleep and wakes it up again. Each of those replaces a
/// worker, a generation, or both, so fifty of them is the shape of a defect
/// that only shows up after the hundredth restart: a thread nobody joins, an
/// mDNS registration nobody releases, a transport nobody drops, or a
/// superseded generation that comes back as the live one.
///
/// The ledgers are read AFTER the backend has been shut down and its control
/// thread joined, which is the only moment at which "left over" means
/// anything: before that, a worker that is merely still winding down is
/// indistinguishable from one that never ends.
///
/// Every wait inside the loop is on published state or on fake time. Nothing
/// here sleeps for a fixed duration hoping the backend keeps up, so the run
/// is as valid on a saturated machine as on an idle one.
#[tokio::test]
async fn fifty_lifecycles_leak_no_workers_and_revive_no_stale_generation() {
    let rig = backend_rig::SystemRig::active_on(&[1, 2], 1, backend_rig::TestClock::new()).await;
    // A browse restart clears the supervisor's inventory, so every generation
    // after the first has to announce the receivers again -- otherwise the
    // soak would quietly degrade into fifty cycles against an empty network.
    rig.discovery.announce_from_now_on(&[1, 2]);

    // Ledger handles have to outlive the rig: `shutdown` consumes it, and the
    // counts are only final once the control thread has been joined.
    let capture = rig.capture.clone();
    let discovery = std::sync::Arc::clone(&rig.discovery);
    let network = rig.network();
    let feed = rig.diagnostic_feed();

    let group = backend_rig::set_of(&[1, 2]);
    let mut revision = rig.snapshot().revision;
    let mut generation = rig
        .session_generation()
        .expect("the fixture brought a session up");

    for cycle in 0..SOAK_CYCLES {
        // 1. Stop and start again.
        rig.send(BackendCommand::SetRunIntent(RunIntent::Stopped))
            .await;
        rig.wait_for(|snapshot| snapshot.session.phase == SessionPhase::Stopped)
            .await;
        rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
            .await;
        rig.wait_for(|snapshot| snapshot.session.active == group)
            .await;

        // 2. Move capture to the other endpoint and back on alternate cycles.
        let (preference, expected) = if cycle % 2 == 0 {
            (
                AudioEndpointPreference::Explicit {
                    id: SOAK_OTHER_ENDPOINT.to_owned(),
                    last_known_name: "Other Speakers".to_owned(),
                },
                SOAK_OTHER_ENDPOINT,
            )
        } else {
            (
                AudioEndpointPreference::SystemDefault,
                SOAK_DEFAULT_ENDPOINT,
            )
        };
        rig.send(BackendCommand::SetAudioEndpoint(preference)).await;
        rig.wait_for(|snapshot| {
            snapshot
                .audio_source
                .captured_endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint.id == expected)
        })
        .await;

        // 3. The browse daemon dies and reconnects on its own backoff.
        rig.fail_discovery().await;

        // 4. The machine sleeps and wakes.
        rig.notify(SystemEvent::Suspending).await;
        rig.wait_for(|snapshot| {
            snapshot.session.resume_pending && snapshot.session.active.is_empty()
        })
        .await;
        rig.notify(SystemEvent::Resumed).await;
        rig.discover_stably(1, SOAK_SETTLE).await;
        rig.wait_for(|snapshot| snapshot.session.active == group)
            .await;

        // The snapshot revision is the shell's only ordering guarantee, and
        // the session generation is the backend's. Both are monotonic, and a
        // cycle that changed this much must have moved both.
        let published = rig.snapshot();
        assert!(
            published.revision > revision,
            "cycle {cycle} published no newer revision ({revision} -> {})",
            published.revision
        );
        revision = published.revision;

        let live = rig
            .session_generation()
            .expect("a session is running at the end of every cycle");
        assert!(
            live > generation,
            "cycle {cycle} reused session generation {generation}"
        );
        generation = live;

        assert_eq!(
            rig.stale_session_activations(),
            0,
            "cycle {cycle} published a superseded generation as live"
        );
        assert_eq!(
            rig.queue_overruns(),
            0,
            "cycle {cycle} reported a queue above its own capacity"
        );
    }

    assert_eq!(
        rig.highest_session_generation(),
        generation,
        "the feed's newest generation and the published one disagree"
    );

    let leaked_transports = rig.shutdown().await;
    assert_eq!(
        leaked_transports, 0,
        "{SOAK_CYCLES} cycles left AirPlay transports alive"
    );
    // A zero leak count is only meaningful if the ledgers saw traffic: every
    // cycle replaces at least one capture worker and opens at least one new
    // browse registration, so anything at or below the cycle count means the
    // soak was not exercising what it claims to.
    assert!(
        capture.workers_started() > SOAK_CYCLES,
        "the soak replaced fewer capture workers than it had cycles"
    );
    assert_eq!(
        capture.leaked_workers(),
        0,
        "{SOAK_CYCLES} cycles left capture worker threads running"
    );
    assert!(
        discovery.streams.opened() > SOAK_CYCLES,
        "the soak opened fewer browse registrations than it had cycles"
    );
    assert_eq!(
        discovery.streams.leaked(),
        0,
        "{SOAK_CYCLES} cycles left mDNS browse registrations open"
    );
    assert!(
        network.subscriptions() > 0,
        "the soak never subscribed to interface changes at all"
    );
    assert_eq!(
        network.subscriptions(),
        network.cancellations(),
        "the interface-change subscription was never handed back"
    );

    let (duration, timed_out) =
        backend_rig::drain_shutdown_report(feed).expect("shutdown emitted no completion record");
    assert!(
        !timed_out,
        "the teardown after {SOAK_CYCLES} cycles expired a bounded drain \
         instead of joining its workers (reported {duration:?})"
    );
}

// ---------------------------------------------------------------------------
// Task 9: manual calibration through the resilience backend.
//
// Calibration is a SETUP input, not a live control: a changed profile takes
// effect through the same controlled full-group restart every other
// membership/timing change uses. Applying it mid-stream would mean moving one
// receiver's presentation clock while the shared RTP timeline keeps running,
// which is exactly the state the full-restart matrix exists to avoid.
// ---------------------------------------------------------------------------

mod calibration_backend {
    use super::*;

    use homepod_cast::backend::model::RestartReason;
    use homepod_cast::backend::persistence::CalibrationStateV2;
    use homepod_cast::calibration::{CalibrationCommand, CalibrationProfile};

    const MS_NS: i64 = 1_000_000;

    fn profile(entries: &[(ReceiverId, i64)], reference: Option<ReceiverId>) -> CalibrationProfile {
        CalibrationProfile {
            reference_receiver: reference,
            requested_relative_delay_ns: entries.iter().copied().collect(),
        }
    }

    /// Number of announced full-group restarts carrying the calibration row.
    fn calibration_restarts(payloads: &[DiagnosticPayload]) -> usize {
        payloads
            .iter()
            .filter(|payload| {
                matches!(
                    payload,
                    DiagnosticPayload::SessionRestart {
                        reason: RestartReason::CalibrationChanged,
                        ..
                    }
                )
            })
            .count()
    }

    /// Brings a two-receiver group up to "running with both selected".
    async fn running_pair() -> BackendRig {
        let rig = BackendRig::start_with_discovery(vec![
            Ok(BrowseEvent::Added(castable_device(1, "Kitchen"))),
            Ok(BrowseEvent::Added(castable_device(2, "Pantry"))),
        ]);
        rig.gate.open();
        rig.wait_for(|snapshot| {
            snapshot
                .receivers
                .iter()
                .filter(|row| row.id == rid(1) || row.id == rid(2))
                .count()
                == 2
        })
        .await;
        rig.send(BackendCommand::SetDesiredMembers {
            members: BTreeSet::from([rid(1), rid(2)]),
        })
        .await;
        rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
            .await;
        rig.wait_for(|snapshot| {
            snapshot.desired_members == BTreeSet::from([rid(1), rid(2)])
                && snapshot.run_intent == RunIntent::Running
        })
        .await;
        rig
    }

    #[tokio::test]
    async fn applying_a_profile_persists_it_and_restarts_the_group_exactly_once() {
        let mut rig = running_pair().await;
        let revision = rig.snapshot().desired_revision;
        let _ = rig.drain_diagnostics();

        let authored = profile(&[(rid(1), -2 * MS_NS), (rid(2), 3 * MS_NS)], Some(rid(1)));
        let id = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ApplyCalibrationProfile {
                    expected_desired_revision: revision,
                    profile: authored.clone(),
                },
            ))
            .await;
        rig.wait_for_event(
            |event| matches!(event, BackendEvent::CommandCompleted { id: done } if *done == id),
        )
        .await;

        assert_eq!(
            rig.calibration_from_disk(),
            Some(CalibrationStateV2::from(&authored)),
            "the authored profile is the durable one"
        );

        // Exactly one controlled full-group restart, on the calibration row.
        let payloads = rig.drain_diagnostics();
        assert_eq!(
            calibration_restarts(&payloads),
            1,
            "apply announces exactly one calibration restart"
        );

        // Setup received the profile; the delays it will hand the client are
        // normalized against the receivers that actually connect.
        // Setup is handed the profile by a task of its own, after the
        // completion this test already waited for; the wait is what makes
        // "reaches session setup" an observation rather than a guess.
        rig.wait_for_calibration(&authored).await;

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn a_stale_desired_revision_neither_persists_nor_restarts() {
        let mut rig = running_pair().await;
        let revision = rig.snapshot().desired_revision;
        let _ = rig.drain_diagnostics();

        let id = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ApplyCalibrationProfile {
                    // The caller decided against an older selection; applying
                    // it now would attach delays to receivers it never saw.
                    expected_desired_revision: revision.saturating_sub(1),
                    profile: profile(&[(rid(1), 5 * MS_NS)], Some(rid(1))),
                },
            ))
            .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandFailed { id: failed, .. } if *failed == id)
        })
        .await;

        assert_eq!(rig.calibration_from_disk(), None, "nothing was written");
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 0);

        rig.shutdown().await;
    }

    /// The published row has to carry what the window renders.
    ///
    /// `model` is the service-record hardware identifier the shell resolves
    /// to a localized device class. Publishing an empty one is not a blank
    /// line on screen -- the resolver's documented fallback is "speaker" --
    /// so every HomePod and Apple TV would silently be called a generic
    /// speaker instead.
    #[tokio::test]
    async fn a_discovered_row_carries_the_name_and_hardware_identifier() {
        let rig = BackendRig::start_with_discovery(vec![Ok(BrowseEvent::Added(castable_device(
            1, "Kitchen",
        )))]);
        rig.gate.open();
        rig.wait_for(|snapshot| snapshot.receivers.iter().any(|row| row.id == rid(1)))
            .await;

        let snapshot = rig.snapshot();
        let row = snapshot
            .receivers
            .iter()
            .find(|row| row.id == rid(1))
            .expect("the sighted receiver is a row");
        assert_eq!(row.name, "Kitchen");
        assert_eq!(row.model, "TestModel");

        rig.shutdown().await;
    }

    /// The other half: a receiver restored from persisted state is a row
    /// before discovery has ever seen it, and the backend has no name for it.
    /// It must stay nameless rather than be called after its identity -- the
    /// stable identity renders as a MAC address.
    #[tokio::test]
    async fn a_row_without_a_sighting_is_nameless_rather_than_named_after_its_mac() {
        let rig = BackendRig::start();
        rig.gate.open();
        rig.send(BackendCommand::SetDesiredMembers {
            members: BTreeSet::from([rid(1)]),
        })
        .await;
        rig.wait_for(|snapshot| snapshot.receivers.iter().any(|row| row.id == rid(1)))
            .await;

        let snapshot = rig.snapshot();
        let row = snapshot
            .receivers
            .iter()
            .find(|row| row.id == rid(1))
            .expect("a desired member is a row before discovery");
        assert_eq!(row.name, "", "the identity was used as a display name");
        assert_eq!(row.model, "");
        assert!(
            !format!("{:?}", snapshot.receivers).contains(&rid(1).to_string()),
            "the identity reached the snapshot in its colon display form"
        );

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn a_single_receiver_selection_is_refused_without_a_restart() {
        let mut rig = BackendRig::start_with_discovery(vec![Ok(BrowseEvent::Added(
            castable_device(1, "Kitchen"),
        ))]);
        rig.gate.open();
        rig.wait_for(|snapshot| snapshot.receivers.iter().any(|row| row.id == rid(1)))
            .await;
        rig.send(BackendCommand::SetDesiredMembers {
            members: BTreeSet::from([rid(1)]),
        })
        .await;
        rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
            .await;
        rig.wait_for(|snapshot| snapshot.desired_members == BTreeSet::from([rid(1)]))
            .await;
        let revision = rig.snapshot().desired_revision;
        let _ = rig.drain_diagnostics();

        // One speaker has nothing to be aligned against.
        let id = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ApplyCalibrationProfile {
                    expected_desired_revision: revision,
                    profile: profile(&[(rid(1), 4 * MS_NS)], Some(rid(1))),
                },
            ))
            .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandFailed { id: failed, .. } if *failed == id)
        })
        .await;

        assert_eq!(rig.calibration_from_disk(), None);
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 0);

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn a_refused_write_leaves_the_stream_untouched() {
        // A LIVE group: the point is that a refused write must not restart
        // audio that is currently playing, which a stopped rig could not
        // observe at all.
        let mut rig = running_pair().await;
        let revision = rig.snapshot().desired_revision;
        rig.store
            .as_ref()
            .expect("the recording store is owned until shutdown")
            .fail_saves
            .store(true, Ordering::Relaxed);
        let baseline_calibration = rig.factory.last_calibration();
        let _ = rig.drain_diagnostics();

        let id = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ApplyCalibrationProfile {
                    expected_desired_revision: revision,
                    profile: profile(&[(rid(2), 9 * MS_NS)], None),
                },
            ))
            .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandFailed { id: failed, .. } if *failed == id)
        })
        .await;

        // Persist-before-apply: an unwritten profile must not restart audio,
        // or the next launch would silently play a different alignment.
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 0);
        assert_eq!(
            rig.factory.last_calibration(),
            baseline_calibration,
            "no new session generation carried the refused profile"
        );
        assert_eq!(rig.calibration_from_disk(), None);

        rig.store
            .as_ref()
            .expect("the recording store is owned until shutdown")
            .fail_saves
            .store(false, Ordering::Relaxed);
        rig.shutdown().await;
    }

    #[tokio::test]
    async fn the_click_test_is_refused_without_two_connected_receivers() {
        // Selecting two receivers is not the same as reaching them. The
        // alignment test only means anything when two speakers are actually
        // playing, and the fake transport factory never brings one up.
        let mut rig = running_pair().await;
        let _ = rig.drain_diagnostics();

        let id = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::RunCalibrationClickTest,
            ))
            .await;
        rig.wait_for_event(|event| {
            matches!(event, BackendEvent::CommandFailed { id: failed, .. } if *failed == id)
        })
        .await;

        // A rejected test is not a setup change and costs no restart.
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 0);
        assert_eq!(rig.calibration_from_disk(), None, "nothing was written");

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn a_profile_left_by_a_previous_launch_reaches_this_one_s_setup() {
        // The round trip the whole feature rests on. Without it a user would
        // recalibrate after every restart while the file on disk quietly kept
        // the values nobody applied.
        let section = CalibrationStateV2 {
            reference: Some(rid(1)),
            delays_ns: [(rid(1), 0_i64), (rid(2), 8 * MS_NS)].into_iter().collect(),
        };
        let rig = BackendRig::start_with_persisted_calibration(
            vec![
                Ok(BrowseEvent::Added(castable_device(1, "Kitchen"))),
                Ok(BrowseEvent::Added(castable_device(2, "Pantry"))),
            ],
            section.clone(),
        );
        rig.gate.open();
        rig.wait_for(|snapshot| {
            snapshot
                .receivers
                .iter()
                .filter(|row| row.id == rid(1) || row.id == rid(2))
                .count()
                == 2
        })
        .await;

        // No calibration command in this process at all: selecting and
        // starting is enough.
        rig.send(BackendCommand::SetDesiredMembers {
            members: BTreeSet::from([rid(1), rid(2)]),
        })
        .await;
        rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
            .await;
        rig.wait_for(|_| rig.factory.start_count() > 0).await;

        assert_eq!(
            rig.factory.last_calibration(),
            Some(CalibrationProfile::from(&section)),
            "the restored profile is what session setup was handed"
        );

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn resetting_clears_the_profile_through_the_same_restart_path() {
        let mut rig = running_pair().await;
        let revision = rig.snapshot().desired_revision;

        let authored = profile(&[(rid(2), 6 * MS_NS)], Some(rid(1)));
        let apply = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ApplyCalibrationProfile {
                    expected_desired_revision: revision,
                    profile: authored.clone(),
                },
            ))
            .await;
        rig.wait_for_event(
            |event| matches!(event, BackendEvent::CommandCompleted { id: done } if *done == apply),
        )
        .await;
        assert_eq!(
            rig.calibration_from_disk(),
            Some(CalibrationStateV2::from(&authored))
        );
        let _ = rig.drain_diagnostics();

        let reset = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ResetCalibration {
                    expected_desired_revision: revision,
                },
            ))
            .await;
        rig.wait_for_event(
            |event| matches!(event, BackendEvent::CommandCompleted { id: done } if *done == reset),
        )
        .await;

        assert_eq!(
            rig.calibration_from_disk(),
            Some(CalibrationStateV2::default()),
            "reset persists the empty profile rather than deleting the section"
        );
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 1);
        rig.wait_for_calibration(&CalibrationProfile::default())
            .await;

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn resetting_stays_available_after_the_selection_shrinks() {
        // The reset is the way OUT of a bad alignment, so gating it on the
        // selection size would withhold it at exactly the moment the user
        // shrank the selection to escape one -- and the section would stay on
        // disk and come back at every later launch. An empty profile has
        // nothing to be relative to and needs no reference receiver, so the
        // two guards that judge a profile do not apply to it.
        let section = CalibrationStateV2 {
            reference: Some(rid(1)),
            delays_ns: [(rid(1), 0_i64), (rid(2), 7 * MS_NS)].into_iter().collect(),
        };
        let mut rig = BackendRig::start_with_persisted_calibration(
            vec![
                Ok(BrowseEvent::Added(castable_device(1, "Kitchen"))),
                Ok(BrowseEvent::Added(castable_device(2, "Pantry"))),
            ],
            section.clone(),
        );
        rig.gate.open();
        rig.wait_for(|snapshot| {
            snapshot
                .receivers
                .iter()
                .filter(|row| row.id == rid(1) || row.id == rid(2))
                .count()
                == 2
        })
        .await;
        // Down to a single receiver: an APPLY would rightly be refused here.
        rig.send(BackendCommand::SetDesiredMembers {
            members: BTreeSet::from([rid(1)]),
        })
        .await;
        rig.send(BackendCommand::SetRunIntent(RunIntent::Running))
            .await;
        rig.wait_for(|snapshot| snapshot.desired_members == BTreeSet::from([rid(1)]))
            .await;
        let revision = rig.snapshot().desired_revision;
        assert_eq!(
            rig.calibration_from_disk(),
            Some(section),
            "the alignment to be cleared is on disk"
        );
        let _ = rig.drain_diagnostics();

        let reset = rig
            .send(BackendCommand::Calibration(
                CalibrationCommand::ResetCalibration {
                    expected_desired_revision: revision,
                },
            ))
            .await;
        rig.wait_for_event(
            |event| matches!(event, BackendEvent::CommandCompleted { id: done } if *done == reset),
        )
        .await;

        assert_eq!(
            rig.calibration_from_disk(),
            Some(CalibrationStateV2::default()),
            "the stored alignment is gone, not merely unused"
        );
        assert_eq!(calibration_restarts(&rig.drain_diagnostics()), 1);
        rig.wait_for_calibration(&CalibrationProfile::default())
            .await;

        rig.shutdown().await;
    }

    #[tokio::test]
    async fn a_damaged_state_document_is_set_aside_instead_of_blocking_the_launch() {
        // Task 9 is what puts schema-v2 documents on real disks, so it is
        // also what makes a damaged one reachable. A load that refuses would
        // refuse identically on every subsequent launch, leaving no way out
        // of the app -- the user would have to find and delete the file by
        // hand, losing volume, groups, and selection with it.
        let damaged = serde_json::to_string_pretty(
            &homepod_cast::backend::persistence::PersistedStateV2::default(),
        )
        .expect("state serializes")
        .replace(r#""delays_ns": {}"#, r#""delays_ns": "unreadable""#);
        assert!(
            damaged.contains("unreadable"),
            "the fixture has to actually be damaged: {damaged}"
        );

        let mut rig = BackendRig::start_with_state_document(
            vec![Ok(BrowseEvent::Added(castable_device(1, "Kitchen")))],
            &damaged,
        );
        rig.gate.open();
        // The backend came up and serves: discovery reached the snapshot.
        rig.wait_for(|snapshot| snapshot.receivers.iter().any(|row| row.id == rid(1)))
            .await;

        // Set aside rather than deleted, so the bytes survive for a bug
        // report, and the user is told rather than left guessing.
        let state_directory = rig.dir.path().join("OpenAirCast");
        assert!(
            !state_directory.join("state-v1.json").exists(),
            "the damaged document was moved out of the way"
        );
        let quarantined: Vec<std::path::PathBuf> = std::fs::read_dir(&state_directory)
            .expect("readable state directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.starts_with("corrupt-"))
            })
            .collect();
        assert_eq!(quarantined.len(), 1, "expected one quarantined document");
        assert_eq!(
            std::fs::read_to_string(&quarantined[0]).expect("readable quarantine"),
            damaged,
            "the evidence survives verbatim"
        );
        assert!(
            rig.drain_events().iter().any(|event| matches!(
                event,
                BackendEvent::Notice {
                    scope: ErrorScope::Persistence,
                    ..
                }
            )),
            "a damaged state file is reported, not swallowed"
        );

        rig.shutdown().await;
    }
}
