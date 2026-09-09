//! Shared deterministic fixtures for backend tests.
//!
//! Fixed seeds keep IDs stable across runs so ordering assertions never depend
//! on discovery order or hash randomization.
//!
//! This file is compiled once per including test module (`backend::model`,
//! `backend::discovery`, `backend::capture`), so each instance only uses part
//! of the fixtures. Dead-code and unused-import lints are therefore relaxed
//! for the whole file instead of annotated per item.

#![allow(dead_code)]
#![allow(unused_imports)]

use homepod_cast::backend::model::{
    ReceiverId, ReceiverLifecycle, ReceiverSnapshot, SavedGroupId, SavedGroupSnapshot,
};

/// Deterministic receiver identity for the given seed byte.
pub fn receiver_id(seed: u8) -> ReceiverId {
    let key = format!("0000000000{seed:02X}");
    ReceiverId::from_storage_key(&key).expect("fixed seed forms a valid storage key")
}

/// Deterministic saved-group identity for the given seed byte.
///
/// Routes through serde because the wrapper's field is private outside
/// `backend::model`, and this fixture compiles in several module trees.
pub fn group_id(seed: u8) -> SavedGroupId {
    let uuid = uuid::Uuid::from_u128(u128::from(seed));
    serde_json::from_value(serde_json::Value::String(uuid.to_string()))
        .expect("uuid string deserializes into SavedGroupId")
}

/// A discovered receiver snapshot with a fixed ID and display name.
pub fn receiver_snapshot(seed: u8, name: &str) -> ReceiverSnapshot {
    ReceiverSnapshot {
        id: receiver_id(seed),
        name: name.to_owned(),
        model: String::new(),
        lifecycle: ReceiverLifecycle::Discovered,
    }
}

/// A saved-group snapshot with a fixed ID and display name.
pub fn saved_group_snapshot(seed: u8, name: &str) -> SavedGroupSnapshot {
    SavedGroupSnapshot {
        id: group_id(seed),
        name: name.to_owned(),
        members: Vec::new(),
    }
}

// --- discovery supervisor (Task 7) ---

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use airplay_core::{Device, DeviceId, Features, Version};
use airplay_discovery::BrowseEvent;
use tokio_stream::wrappers::UnboundedReceiverStream;

use homepod_cast::backend::discovery::DiscoverySource;
use homepod_cast::backend::model::UserFacingError;
use homepod_cast::backend::recovery::Clock;

/// One fallible browse-stream item as produced by the discovery daemon.
pub type BrowseItem = std::result::Result<BrowseEvent, airplay_core::error::DiscoveryError>;

/// A pre-redacted user-safe error for tests.
pub fn user_error(message: &str) -> UserFacingError {
    UserFacingError::new(message)
}

/// A discovered device fixture that is castable (AirPlay 2 + IPv4) and whose
/// stable identity equals [`receiver_id`] for the same seed.
pub fn castable_device(seed: u8, name: &str) -> Device {
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

/// `BrowseEvent::Added` for a castable receiver fixture.
pub fn added(seed: u8) -> BrowseEvent {
    BrowseEvent::Added(castable_device(seed, "Kitchen"))
}

/// `BrowseEvent::Updated` for a castable receiver fixture.
pub fn updated(seed: u8) -> BrowseEvent {
    BrowseEvent::Updated(castable_device(seed, "Kitchen"))
}

/// `BrowseEvent::Removed` for the receiver identity of [`receiver_id`].
pub fn removed(seed: u8) -> BrowseEvent {
    BrowseEvent::Removed(DeviceId([0, 0, 0, 0, 0, seed]))
}

/// Frozen monotonic clock for deterministic retry-deadline assertions.
#[derive(Debug)]
pub struct FixedClock(Mutex<Instant>);

impl FixedClock {
    /// Anchors the frozen clock at construction time.
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    /// Returns the anchored instant.
    pub fn now_instant(&self) -> Instant {
        *self.0.lock().expect("fixed clock mutex poisoned")
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Instant {
        self.now_instant()
    }
}

/// Scripted discovery source: each queued outcome becomes one `browse()`
/// generation. Successful generations are backed by live channels so tests
/// can push further events into the running stream; every call is recorded.
#[derive(Clone, Default)]
pub struct FakeDiscoverySource {
    inner: Arc<FakeDiscoveryInner>,
}

#[derive(Default)]
struct FakeDiscoveryInner {
    script: Mutex<VecDeque<FakeScripted>>,
    live: Mutex<Vec<Option<tokio::sync::mpsc::UnboundedSender<BrowseItem>>>>,
    calls: AtomicUsize,
}

enum FakeScripted {
    Stream(Vec<BrowseItem>),
    Error(UserFacingError),
}

impl std::fmt::Debug for FakeScripted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(items) => f.debug_tuple("Stream").field(&items.len()).finish(),
            Self::Error(error) => f.debug_tuple("Error").field(error).finish(),
        }
    }
}

impl FakeDiscoverySource {
    /// Creates an empty script; every unexpected call panics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one successful generation preloaded with `items`; the stream
    /// stays open for later [`Self::emit`] pushes until it is dropped.
    pub fn push_stream(&self, items: Vec<BrowseItem>) {
        self.inner
            .script
            .lock()
            .expect("fake script mutex poisoned")
            .push_back(FakeScripted::Stream(items));
    }

    /// Queues one failed `browse()` call.
    pub fn push_browse_error(&self, message: &str) {
        self.inner
            .script
            .lock()
            .expect("fake script mutex poisoned")
            .push_back(FakeScripted::Error(user_error(message)));
    }

    /// Number of recorded `browse()` calls (= started generations).
    pub fn calls(&self) -> usize {
        self.inner.calls.load(Ordering::SeqCst)
    }

    /// Pushes an item into the live stream of the given recorded call.
    ///
    /// Returns false when that generation was dropped or never opened.
    pub fn emit(&self, call_index: usize, item: BrowseItem) -> bool {
        self.inner
            .live
            .lock()
            .expect("fake live mutex poisoned")
            .get(call_index)
            .and_then(|slot| slot.as_ref())
            .is_some_and(|sender| sender.send(item).is_ok())
    }
}

#[async_trait::async_trait]
impl DiscoverySource for FakeDiscoverySource {
    async fn browse(&self) -> Result<airplay_discovery::BrowseStream, UserFacingError> {
        let index = self.inner.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .inner
            .script
            .lock()
            .expect("fake script mutex poisoned")
            .pop_front();
        match next {
            Some(FakeScripted::Stream(items)) => {
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<BrowseItem>();
                for item in items {
                    let _ = sender.send(item);
                }
                self.inner
                    .live
                    .lock()
                    .expect("fake live mutex poisoned")
                    .push(Some(sender));
                Ok(Box::pin(UnboundedReceiverStream::new(receiver))
                    as airplay_discovery::BrowseStream)
            }
            Some(FakeScripted::Error(error)) => Err(error),
            None => panic!("unexpected browse() call #{index}: fake script exhausted"),
        }
    }
}

// --- capture supervisor (Task 9) ---

use std::thread;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use homepod_cast::backend::capture::{CaptureError, CaptureSource, CaptureWorker};
use homepod_cast::backend::model::{AudioEndpoint, AudioEndpointPreference};

/// One 50 ms stereo frame whose every sample is `value + 1` so real frames
/// are distinguishable from bridge silence (all zero).
pub fn fake_pcm_frame(value: u16) -> airplay_audio::LivePcmFrame {
    let sample = i16::try_from(value)
        .map(|value| value + 1)
        .unwrap_or(i16::MAX);
    airplay_audio::LivePcmFrame {
        samples: vec![
            sample;
            homepod_cast::backend::capture::CAPTURE_SAMPLE_RATE as usize
                * homepod_cast::backend::capture::CAPTURE_CHANNELS as usize
                / 20
        ],
        channels: homepod_cast::backend::capture::CAPTURE_CHANNELS,
        sample_rate: homepod_cast::backend::capture::CAPTURE_SAMPLE_RATE,
    }
}

/// Deterministic render endpoint fixture.
pub fn audio_endpoint(seed: u8) -> AudioEndpoint {
    AudioEndpoint {
        id: format!("endpoint-{seed:02X}"),
        name: format!("Endpoint {seed}"),
    }
}

/// Scripted behavior of one fake capture worker.
#[derive(Clone)]
pub enum FakeWorkerPlan {
    /// Reports ready, delivers `n` real frames, then fails.
    FramesThenError(usize),
    /// Reports ready, then parks until cancellation; ends cleanly.
    HangUntilCancel,
    /// Never reports readiness; parks until cancellation.
    ReadyNever,
}

struct FakeWorker {
    plan: FakeWorkerPlan,
    endpoint: AudioEndpoint,
}

impl CaptureWorker for FakeWorker {
    fn run(
        self: Box<Self>,
        frames: crossbeam_channel::Sender<airplay_audio::LivePcmFrame>,
        ready: tokio::sync::oneshot::Sender<Result<AudioEndpoint, UserFacingError>>,
        cancel: CancellationToken,
    ) -> Result<(), CaptureError> {
        let worker = *self;
        match worker.plan {
            FakeWorkerPlan::FramesThenError(count) => {
                let _ = ready.send(Ok(worker.endpoint));
                for index in 0..count {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    let mut frame = fake_pcm_frame(index as u16 + 1);
                    loop {
                        match frames.try_send(frame.clone()) {
                            Ok(()) => break,
                            Err(crossbeam_channel::TrySendError::Full(returned)) => {
                                frame = returned;
                                if cancel.is_cancelled() {
                                    return Ok(());
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                                return Err(CaptureError::Worker(
                                    "bridge queue closed while streaming".into(),
                                ));
                            }
                        }
                    }
                }
                Err(CaptureError::Worker("fake capture worker failed".into()))
            }
            FakeWorkerPlan::HangUntilCancel => {
                let _ = ready.send(Ok(worker.endpoint));
                while !cancel.is_cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            }
            FakeWorkerPlan::ReadyNever => {
                while !cancel.is_cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            }
        }
    }
}

enum FakeCaptureScripted {
    Endpoints(Vec<AudioEndpoint>),
    OpenOk(FakeWorkerPlan, AudioEndpoint),
    OpenErr(String),
}

/// Scripted capture source: `endpoints()` and `open()` pop queued outcomes;
/// every open is recorded. Exhausting the script panics the caller.
#[derive(Clone, Default)]
pub struct FakeCaptureSource {
    inner: Arc<FakeCaptureInner>,
}

#[derive(Default)]
struct FakeCaptureInner {
    script: Mutex<VecDeque<FakeCaptureScripted>>,
    opens: AtomicUsize,
}

impl std::fmt::Debug for FakeCaptureScripted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Endpoints(endpoints) => {
                f.debug_tuple("Endpoints").field(&endpoints.len()).finish()
            }
            Self::OpenOk(_, endpoint) => f.debug_tuple("OpenOk").field(endpoint).finish(),
            Self::OpenErr(message) => f.debug_tuple("OpenErr").field(message).finish(),
        }
    }
}

impl FakeCaptureSource {
    /// Creates an empty script; unexpected calls panic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the endpoint list returned by the next `endpoints()` call.
    pub fn push_endpoints(&self, endpoints: Vec<AudioEndpoint>) {
        self.inner
            .script
            .lock()
            .expect("fake capture script mutex poisoned")
            .push_back(FakeCaptureScripted::Endpoints(endpoints));
    }

    /// Queues one successful open producing a worker with the given plan.
    pub fn push_open_ok(&self, plan: FakeWorkerPlan, endpoint: AudioEndpoint) {
        self.inner
            .script
            .lock()
            .expect("fake capture script mutex poisoned")
            .push_back(FakeCaptureScripted::OpenOk(plan, endpoint));
    }

    /// Queues one failed open.
    pub fn push_open_err(&self, message: &str) {
        self.inner
            .script
            .lock()
            .expect("fake capture script mutex poisoned")
            .push_back(FakeCaptureScripted::OpenErr(message.to_owned()));
    }

    /// Number of recorded `open()` calls (= worker start attempts).
    pub fn opens(&self) -> usize {
        self.inner.opens.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl CaptureSource for FakeCaptureSource {
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        let next = self
            .inner
            .script
            .lock()
            .expect("fake capture script mutex poisoned")
            .pop_front();
        match next {
            Some(FakeCaptureScripted::Endpoints(endpoints)) => Ok(endpoints),
            _ => Ok(vec![audio_endpoint(1)]),
        }
    }

    async fn open(
        &self,
        _preference: &AudioEndpointPreference,
    ) -> Result<Box<dyn CaptureWorker>, CaptureError> {
        self.inner.opens.fetch_add(1, Ordering::SeqCst);
        let next = self
            .inner
            .script
            .lock()
            .expect("fake capture script mutex poisoned")
            .pop_front();
        match next {
            Some(FakeCaptureScripted::OpenOk(plan, endpoint)) => {
                Ok(Box::new(FakeWorker { plan, endpoint }))
            }
            Some(FakeCaptureScripted::OpenErr(message)) => Err(CaptureError::Endpoints(message)),
            Some(other) => panic!("unexpected fake capture script entry: {other:?}"),
            None => panic!("unexpected open(): fake capture script exhausted"),
        }
    }
}

// --- WASAPI fakes (Task 10) ---

use std::collections::HashMap;

use homepod_cast::backend::capture::{WasapiApi, WasapiDeviceRef, WasapiLoopbackStream};

/// Scripted in-memory WASAPI surface. Records every resolution and open call
/// so the plan-mandated endpoint-resolution rule (explicit IDs never fall
/// back to the default device) is observable without an audio device.
#[derive(Clone, Default)]
pub struct FakeWasapiApi {
    inner: Arc<FakeWasapiInner>,
}

#[derive(Default)]
struct FakeWasapiInner {
    default_id: Mutex<Option<String>>,
    endpoints: Mutex<HashMap<String, String>>,
    requested_ids: Mutex<Vec<String>>,
    opened_ids: Mutex<Vec<String>>,
    default_requests: AtomicUsize,
    started_streams: AtomicUsize,
    stopped_streams: AtomicUsize,
    read_script: Mutex<VecDeque<Vec<f32>>>,
    start_failure: Mutex<Option<String>>,
}

impl FakeWasapiApi {
    /// Creates an empty fake with no default device and no endpoints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a fake whose system-default render endpoint has the given ID
    /// (associated constructor, plan call shape).
    pub fn with_default(default_id: &str) -> Self {
        let fake = Self::default();
        *fake
            .inner
            .default_id
            .lock()
            .expect("fake default mutex poisoned") = Some(default_id.to_owned());
        fake
    }

    /// Registers one render endpoint with its friendly name (builder style).
    pub fn with_endpoint(self, id: &str, name: &str) -> Self {
        self.inner
            .endpoints
            .lock()
            .expect("fake endpoint map mutex poisoned")
            .insert(id.to_owned(), name.to_owned());
        self
    }

    /// Queues one batch of interleaved f32 samples returned by the next
    /// `read_samples` call.
    pub fn push_read_batch(&self, samples: Vec<f32>) {
        self.inner
            .read_script
            .lock()
            .expect("fake read-script mutex poisoned")
            .push_back(samples);
    }

    /// Makes the next `start_stream` call fail with the given message.
    pub fn fail_next_start(&self, message: &str) {
        *self
            .inner
            .start_failure
            .lock()
            .expect("fake start-failure mutex poisoned") = Some(message.to_owned());
    }

    /// Every endpoint ID passed to `get_device_by_id`, in call order.
    pub fn requested_ids(&self) -> Vec<String> {
        self.inner
            .requested_ids
            .lock()
            .expect("fake requested-id log mutex poisoned")
            .clone()
    }

    /// Every endpoint ID passed to `open_loopback`, in call order.
    pub fn opened_ids(&self) -> Vec<String> {
        self.inner
            .opened_ids
            .lock()
            .expect("fake opened-id log mutex poisoned")
            .clone()
    }

    /// Number of `get_default_output_device` calls.
    pub fn default_requests(&self) -> usize {
        self.inner.default_requests.load(Ordering::SeqCst)
    }

    /// Number of started loopback streams.
    pub fn started_streams(&self) -> usize {
        self.inner.started_streams.load(Ordering::SeqCst)
    }

    /// Number of stopped loopback streams.
    pub fn stopped_streams(&self) -> usize {
        self.inner.stopped_streams.load(Ordering::SeqCst)
    }
}

impl WasapiApi for FakeWasapiApi {
    fn enumerate_render_endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        let endpoints = self
            .inner
            .endpoints
            .lock()
            .expect("fake endpoint map mutex poisoned");
        let mut listed: Vec<AudioEndpoint> = endpoints
            .iter()
            .map(|(id, name)| AudioEndpoint {
                id: id.clone(),
                name: name.clone(),
            })
            .collect();
        if let Some(default) = self
            .inner
            .default_id
            .lock()
            .expect("fake default mutex poisoned")
            .as_ref()
        {
            if !endpoints.contains_key(default) {
                listed.push(AudioEndpoint {
                    id: default.clone(),
                    name: format!("{default} (default)"),
                });
            }
        }
        Ok(listed)
    }

    fn get_device_by_id(&self, id: &str) -> Result<WasapiDeviceRef, CaptureError> {
        self.inner
            .requested_ids
            .lock()
            .expect("fake requested-id log mutex poisoned")
            .push(id.to_owned());
        match self
            .inner
            .endpoints
            .lock()
            .expect("fake endpoint map mutex poisoned")
            .get(id)
        {
            Some(name) => Ok(WasapiDeviceRef {
                id: id.to_owned(),
                name: name.clone(),
            }),
            // The error names the missing ID; there is deliberately no
            // fallback path to the default device here.
            None => Err(CaptureError::Endpoints(format!(
                "render endpoint '{id}' is unavailable"
            ))),
        }
    }

    fn get_default_output_device(&self) -> Result<WasapiDeviceRef, CaptureError> {
        self.inner.default_requests.fetch_add(1, Ordering::SeqCst);
        match self
            .inner
            .default_id
            .lock()
            .expect("fake default mutex poisoned")
            .as_ref()
        {
            Some(default) => {
                let name = self
                    .inner
                    .endpoints
                    .lock()
                    .expect("fake endpoint map mutex poisoned")
                    .get(default)
                    .cloned()
                    .unwrap_or_else(|| format!("{default} (default)"));
                Ok(WasapiDeviceRef {
                    id: default.clone(),
                    name,
                })
            }
            None => Err(CaptureError::Endpoints(
                "no default render endpoint available".into(),
            )),
        }
    }

    fn open_loopback(
        &self,
        device: &WasapiDeviceRef,
    ) -> Result<Box<dyn WasapiLoopbackStream>, CaptureError> {
        self.inner
            .opened_ids
            .lock()
            .expect("fake opened-id log mutex poisoned")
            .push(device.id.clone());
        Ok(Box::new(FakeLoopbackStream {
            inner: Arc::clone(&self.inner),
        }))
    }
}

/// Loopback stream fake reading from the shared scripted sample batches.
struct FakeLoopbackStream {
    inner: Arc<FakeWasapiInner>,
}

impl WasapiLoopbackStream for FakeLoopbackStream {
    fn start_stream(&mut self) -> Result<(), CaptureError> {
        self.inner.started_streams.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self
            .inner
            .start_failure
            .lock()
            .expect("fake start-failure mutex poisoned")
            .take()
        {
            return Err(CaptureError::Worker(message));
        }
        Ok(())
    }

    fn read_samples(&mut self) -> Result<Vec<f32>, CaptureError> {
        Ok(self
            .inner
            .read_script
            .lock()
            .expect("fake read-script mutex poisoned")
            .pop_front()
            .unwrap_or_default())
    }

    fn wait_for_event(&mut self, _timeout_ms: u32) -> bool {
        if !self
            .inner
            .read_script
            .lock()
            .expect("fake read-script mutex poisoned")
            .is_empty()
        {
            return true;
        }
        // Park briefly instead of busy-spinning while "silent"; cancellation
        // is honored by the worker between iterations.
        thread::sleep(Duration::from_millis(5));
        false
    }

    fn stop_stream(&mut self) {
        self.inner.stopped_streams.fetch_add(1, Ordering::SeqCst);
    }
}
