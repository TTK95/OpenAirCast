//! Stable PCM bridge and capture supervisor (Task 9).
//!
//! Owns one live decoder pair per session generation, a replaceable capture
//! worker per capture generation, and a [`PcmBridge`] thread that forwards
//! real frames through replace-oldest delivery while pacing 50 ms silence
//! whenever no real frame arrives â€” keeping the AirPlay timeline alive
//! across Windows silence, endpoint swaps, and worker recovery.

// The decoder-source seam is wired to the controller; a few
// inspection surfaces still await their call sites, so non-test builds would
// otherwise warn on them.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use airplay_audio::{
    AudioDiagnosticsSource, LiveAudioDecoder, LiveFrameSender, LivePcmFrame, LiveSendOutcome,
};
use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use crate::backend::discovery::Timer;
use crate::backend::event::{
    DiagnosticCategory, DiagnosticEvent, DiagnosticKind, DiagnosticPayload, GenerationCell,
    Generations, Severity, SUPERVISOR_CAPACITY,
};
use crate::backend::model::{
    AudioEndpoint, AudioEndpointPreference, AudioSourceSnapshot, AudioSourceState, UserFacingError,
};
use crate::backend::session::SessionDecoderSource;

/// Normalized live PCM sample rate for the whole pipeline.
pub const CAPTURE_SAMPLE_RATE: u32 = 44_100;
/// Normalized live PCM channel count.
pub const CAPTURE_CHANNELS: u8 = 2;
/// Capacity of the stable PCM bridge feeding the live decoder.
pub const CAPTURE_PCM_CAPACITY: usize = super::PCM_CAPACITY;

/// Interval after which the bridge paces one frame of silence.
const SILENCE_PERIOD: Duration = Duration::from_millis(50);
/// Frame cadence used by the finite receiver listening tone.
const LISTENING_TONE_FRAME_PERIOD: Duration = Duration::from_millis(20);

#[doc(hidden)]
#[derive(Default)]
/// Lock-bounded, best-effort peak recorder shared with the real capture source.
pub struct CaptureInputMeter {
    window: Mutex<CaptureInputWindow>,
}

#[derive(Default)]
struct CaptureInputWindow {
    generation: Option<u64>,
    observed: bool,
    peak_permille: u16,
    observed_at: Option<Instant>,
}

impl CaptureInputMeter {
    fn begin_generation(&self, generation: u64) {
        let mut window = lock_meter(&self.window);
        *window = CaptureInputWindow {
            generation: Some(generation),
            observed: false,
            peak_permille: 0,
            observed_at: None,
        };
    }

    fn generation(&self) -> Option<u64> {
        lock_meter(&self.window).generation
    }

    fn invalidate_generation(&self, generation: u64) {
        let mut window = lock_meter(&self.window);
        if window.generation == Some(generation) {
            *window = CaptureInputWindow::default();
        }
    }

    fn discard_window(&self, generation: u64) {
        let mut window = lock_meter(&self.window);
        if window.generation == Some(generation) {
            window.observed = false;
            window.peak_permille = 0;
            window.observed_at = None;
        }
    }

    fn record_peak(&self, generation: u64, peak_permille: u16) {
        self.record_peak_at(generation, peak_permille, Instant::now());
    }

    fn record_peak_at(&self, generation: u64, peak_permille: u16, observed_at: Instant) {
        let Ok(mut window) = self.window.try_lock() else {
            return;
        };
        if window.generation != Some(generation) {
            return;
        }
        if window.observed_at.is_some_and(|oldest| {
            observed_at.saturating_duration_since(oldest) > Duration::from_secs(1)
        }) {
            window.observed = false;
            window.peak_permille = 0;
            window.observed_at = None;
        }
        window.observed = true;
        window.peak_permille = window.peak_permille.max(peak_permille.min(1_000));
        window.observed_at.get_or_insert(observed_at);
    }

    fn take_window(&self, generation: u64) -> Option<u16> {
        self.take_window_at(generation, Instant::now())
    }

    fn take_window_at(&self, generation: u64, now: Instant) -> Option<u16> {
        let mut window = lock_meter(&self.window);
        if window.generation != Some(generation) || !window.observed {
            return None;
        }
        let peak = window.peak_permille;
        let fresh = window.observed_at.is_some_and(|observed_at| {
            now.saturating_duration_since(observed_at) <= Duration::from_secs(1)
        });
        window.observed = false;
        window.peak_permille = 0;
        window.observed_at = None;
        fresh.then_some(peak)
    }
}

fn lock_meter(meter: &Mutex<CaptureInputWindow>) -> MutexGuard<'_, CaptureInputWindow> {
    meter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Worker restart ladder: 250 ms, 500 ms, 1 s, 2 s, then every 5 s.
pub const CAPTURE_RETRY_LADDER: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// Failure of endpoint enumeration or of a capture worker.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// Endpoint enumeration failed before a worker could be opened.
    #[error("endpoint enumeration failed: {0}")]
    Endpoints(String),
    /// The opened capture worker failed while streaming.
    #[error("capture worker failed: {0}")]
    Worker(String),
}

/// Source of replaceable capture workers; the injection seam for WASAPI.
#[async_trait]
pub trait CaptureSource: Send + Sync {
    /// Lists all currently active render endpoints.
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError>;
    /// Opens a capture worker for the given endpoint preference.
    async fn open(
        &self,
        preference: &AudioEndpointPreference,
    ) -> Result<Box<dyn CaptureWorker>, CaptureError>;

    /// Optional meter populated by this source's workers.
    fn input_meter(&self) -> Option<Arc<CaptureInputMeter>> {
        None
    }
}

/// One dedicated enumeration thread and at most one coalesced follow-up.
/// A stuck COM call cannot block the controller or spawn replacement workers.
pub(crate) struct EndpointScanner {
    requests: std::sync::mpsc::SyncSender<()>,
    pub(crate) results: mpsc::Receiver<Result<Vec<AudioEndpoint>, CaptureError>>,
}

impl EndpointScanner {
    pub(crate) fn start(source: Arc<dyn CaptureSource>) -> std::io::Result<Self> {
        let (requests, receive) = std::sync::mpsc::sync_channel(1);
        let (publish, results) = mpsc::channel(1);
        thread::Builder::new()
            .name("endpoint-enumeration".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    // Closing `publish` reports terminal scanner failure through
                    // the controller's endpoint-result channel, including to UI.
                    return;
                };
                while receive.recv().is_ok() {
                    if publish.is_closed() {
                        break;
                    }
                    let result = runtime.block_on(source.endpoints());
                    if publish.blocking_send(result).is_err() {
                        break;
                    }
                }
            })?;
        let scanner = Self { requests, results };
        scanner.request();
        Ok(scanner)
    }

    pub(crate) fn request(&self) {
        let _ = self.requests.try_send(());
    }
}

/// One replaceable capture worker producing normalized PCM frames.
///
/// `run` blocks its calling thread until cancellation or failure; readiness
/// is reported exactly once through the oneshot handshake.
pub trait CaptureWorker: Send {
    /// Runs the capture loop on the calling thread.
    fn run(
        self: Box<Self>,
        frames: crossbeam_channel::Sender<LivePcmFrame>,
        ready: oneshot::Sender<Result<AudioEndpoint, UserFacingError>>,
        cancel: CancellationToken,
    ) -> Result<(), CaptureError>;
}

// --- WASAPI seam (Task 10) ---

/// Plain-data reference to one resolved Windows render endpoint.
///
/// The seam deliberately reduces wasapi's COM `Device` to stable ID plus
/// display name so resolved values can cross threads freely; all COM objects
/// stay confined inside [`RealWasapiApi`]/[`RealLoopbackStream`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WasapiDeviceRef {
    /// Stable Windows endpoint ID.
    pub id: String,
    /// Current friendly name of the endpoint.
    pub name: String,
}

impl From<&WasapiDeviceRef> for AudioEndpoint {
    fn from(device: &WasapiDeviceRef) -> Self {
        Self {
            id: device.id.clone(),
            name: device.name.clone(),
        }
    }
}

/// One opened loopback stream on a render endpoint.
///
/// Mirrors the legacy cast.rs call sequence: lazy client initialization in
/// [`Self::start_stream`] (render device + Capture direction, shared mode
/// with autoconvert), event-driven pacing, and interleaved f32 reads.
///
/// Intentionally NOT `Send`: wasapi's COM wrappers are thread-affine, so the
/// real stream is opened, read, and stopped entirely on one worker thread
/// (inside [`CaptureWorker::run`]).
pub trait WasapiLoopbackStream {
    /// Initializes the loopback client if needed and starts the stream.
    fn start_stream(&mut self) -> Result<(), CaptureError>;
    /// Reads newly captured interleaved f32 samples; empty while silent.
    fn read_samples(&mut self) -> Result<Vec<f32>, CaptureError>;
    /// Blocks up to `timeout_ms` for capture data; true when data may exist.
    fn wait_for_event(&mut self, timeout_ms: u32) -> bool;
    /// Stops the stream; safe to call more than once.
    fn stop_stream(&mut self);
}

/// Injectable surface over the wasapi crate.
///
/// Exactly the operations the loopback capture path needs; every unsafe COM
/// access stays confined to [`RealWasapiApi`].
pub trait WasapiApi: Send + Sync {
    /// Lists all active render endpoints sorted for display.
    fn enumerate_render_endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError>;
    /// Resolves exactly this endpoint ID; never falls back to any default.
    fn get_device_by_id(&self, id: &str) -> Result<WasapiDeviceRef, CaptureError>;
    /// Resolves the system default output (render) endpoint.
    fn get_default_output_device(&self) -> Result<WasapiDeviceRef, CaptureError>;
    /// Opens a loopback capture stream on the given endpoint.
    fn open_loopback(
        &self,
        device: &WasapiDeviceRef,
    ) -> Result<Box<dyn WasapiLoopbackStream>, CaptureError>;
}

/// Sorts endpoints case-insensitively by name with the stable ID as tiebreak
/// so UI ordering never depends on Windows enumeration order.
pub(crate) fn sort_endpoints(endpoints: &mut [AudioEndpoint]) {
    endpoints.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
}

fn endpoints_error(context: &str, error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Endpoints(format!("{context}: {error}"))
}

fn worker_error(context: &str, error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Worker(format!("{context}: {error}"))
}

/// Production [`WasapiApi`] wrapping the real wasapi crate calls.
///
/// Every method initializes COM (MTA) on the calling thread exactly like the
/// legacy capture path and converts all failures into pre-redacted
/// [`CaptureError`] summaries.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealWasapiApi;

impl WasapiApi for RealWasapiApi {
    fn enumerate_render_endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        // Keep the COM guard alive for this call.
        let _com = initialize_mta();
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| endpoints_error("WASAPI enumerator unavailable", error))?;
        let collection = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(|error| endpoints_error("render endpoint enumeration failed", error))?;
        let mut endpoints = Vec::new();
        for item in &collection {
            let device =
                item.map_err(|error| endpoints_error("render endpoint enumeration failed", error))?;
            let id = device
                .get_id()
                .map_err(|error| endpoints_error("endpoint ID unavailable", error))?;
            let name = device
                .get_friendlyname()
                .map_err(|error| endpoints_error("endpoint name unavailable", error))?;
            endpoints.push(AudioEndpoint { id, name });
        }
        sort_endpoints(&mut endpoints);
        Ok(endpoints)
    }

    fn get_device_by_id(&self, id: &str) -> Result<WasapiDeviceRef, CaptureError> {
        let _com = initialize_mta();
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| endpoints_error("WASAPI enumerator unavailable", error))?;
        // Plan-mandated rule: explicit IDs resolve through get_device ONLY;
        // a missing ID errors naming it instead of falling back to default.
        let device = enumerator.get_device(id).map_err(|error| {
            endpoints_error(&format!("render endpoint '{id}' not found"), error)
        })?;
        Ok(WasapiDeviceRef {
            id: device
                .get_id()
                .map_err(|error| endpoints_error("endpoint ID unavailable", error))?,
            name: device
                .get_friendlyname()
                .map_err(|error| endpoints_error("endpoint name unavailable", error))?,
        })
    }

    fn get_default_output_device(&self) -> Result<WasapiDeviceRef, CaptureError> {
        let _com = initialize_mta();
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| endpoints_error("WASAPI enumerator unavailable", error))?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .map_err(|error| endpoints_error("default render endpoint unavailable", error))?;
        Ok(WasapiDeviceRef {
            id: device
                .get_id()
                .map_err(|error| endpoints_error("endpoint ID unavailable", error))?,
            name: device
                .get_friendlyname()
                .map_err(|error| endpoints_error("endpoint name unavailable", error))?,
        })
    }

    fn open_loopback(
        &self,
        device: &WasapiDeviceRef,
    ) -> Result<Box<dyn WasapiLoopbackStream>, CaptureError> {
        // Lazy handle: the actual COM objects are created inside
        // start_stream() on whichever thread runs the read loop, keeping all
        // WASAPI access confined to one thread like the legacy path.
        Ok(Box::new(RealLoopbackStream {
            device_id: device.id.clone(),
            client: None,
            capture_client: None,
            event: None,
            queue: VecDeque::new(),
        }))
    }
}

/// Production loopback stream; owns its COM objects only between
/// [`RealLoopbackStream::start_stream`] and [`Self::stop_stream`], which run
/// on the worker thread.
struct RealLoopbackStream {
    device_id: String,
    client: Option<wasapi::AudioClient>,
    capture_client: Option<wasapi::AudioCaptureClient>,
    event: Option<wasapi::Handle>,
    queue: VecDeque<u8>,
}

impl WasapiLoopbackStream for RealLoopbackStream {
    fn start_stream(&mut self) -> Result<(), CaptureError> {
        let _com = initialize_mta();
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| worker_error("WASAPI enumerator unavailable", error))?;
        let device = enumerator
            .get_device(&self.device_id)
            .map_err(|error| worker_error("loopback device unavailable", error))?;
        let mut audio_client = device
            .get_iaudioclient()
            .map_err(|error| worker_error("audio client unavailable", error))?;

        // Render device + Capture direction + Shared mode => loopback.
        // autoconvert gives 44.1 kHz / stereo / f32 regardless of mix format.
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            CAPTURE_SAMPLE_RATE as usize,
            CAPTURE_CHANNELS as usize,
            None,
        );
        let (_default_period, min_period) = audio_client
            .get_device_period()
            .map_err(|error| worker_error("device period unavailable", error))?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: min_period,
        };
        audio_client
            .initialize_client(&format, &Direction::Capture, &mode)
            .map_err(|error| worker_error("loopback initialization failed", error))?;

        let h_event = audio_client
            .set_get_eventhandle()
            .map_err(|error| worker_error("event handle unavailable", error))?;
        let capture_client = audio_client
            .get_audiocaptureclient()
            .map_err(|error| worker_error("capture client unavailable", error))?;
        audio_client
            .start_stream()
            .map_err(|error| worker_error("stream start failed", error))?;
        tracing::info!(
            "loopback capture started ({CAPTURE_SAMPLE_RATE} Hz, {CAPTURE_CHANNELS}ch, f32)"
        );

        self.client = Some(audio_client);
        self.capture_client = Some(capture_client);
        self.event = Some(h_event);
        Ok(())
    }

    fn read_samples(&mut self) -> Result<Vec<f32>, CaptureError> {
        let Some(capture_client) = self.capture_client.as_ref() else {
            return Err(CaptureError::Worker(
                "loopback stream read before start".into(),
            ));
        };
        capture_client
            .read_from_device_to_deque(&mut self.queue)
            .map_err(|error| worker_error("loopback read failed", error))?;

        let bytes_per_frame = CAPTURE_CHANNELS as usize * 4; // f32 per sample
        let frame_count = self.queue.len() / bytes_per_frame;
        let mut samples = Vec::with_capacity(frame_count * CAPTURE_CHANNELS as usize);
        for _ in 0..frame_count * CAPTURE_CHANNELS as usize {
            let bytes = [
                self.queue.pop_front().unwrap_or_default(),
                self.queue.pop_front().unwrap_or_default(),
                self.queue.pop_front().unwrap_or_default(),
                self.queue.pop_front().unwrap_or_default(),
            ];
            samples.push(f32::from_le_bytes(bytes));
        }
        Ok(samples)
    }

    fn wait_for_event(&mut self, timeout_ms: u32) -> bool {
        match self.event.as_ref() {
            // Event timeouts are normal pacing (silent system), not errors.
            Some(event) => event.wait_for_event(timeout_ms).is_ok(),
            None => false,
        }
    }

    fn stop_stream(&mut self) {
        if let Some(client) = self.client.take() {
            let _ = client.stop_stream();
        }
        self.capture_client = None;
        self.event = None;
    }
}

/// Production [`CaptureSource`] over real Windows WASAPI endpoints.
pub struct WasapiCaptureSource {
    api: Arc<dyn WasapiApi>,
    resolve_slot: Arc<tokio::sync::Semaphore>,
    input_meter: Arc<CaptureInputMeter>,
}

impl WasapiCaptureSource {
    /// Creates the source over the real WASAPI surface.
    pub fn new() -> Self {
        Self {
            api: Arc::new(RealWasapiApi),
            resolve_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            input_meter: Arc::new(CaptureInputMeter::default()),
        }
    }

    /// Creates the source over an injected [`WasapiApi`] seam (tests).
    pub fn with_api(api: Arc<dyn WasapiApi>) -> Self {
        Self {
            api,
            resolve_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            input_meter: Arc::new(CaptureInputMeter::default()),
        }
    }

    /// Resolves an endpoint preference against the WASAPI surface.
    ///
    /// Plan-mandated resolution rule: `SystemDefault` follows the default
    /// OUTPUT/render device (loopback target); `Explicit { id }` resolves
    /// through the stable ID only and never falls back to the default when
    /// the ID is missing — the error names the missing ID instead.
    pub fn resolve(
        &self,
        preference: &AudioEndpointPreference,
    ) -> Result<AudioEndpoint, CaptureError> {
        let device = match preference {
            AudioEndpointPreference::SystemDefault => self.api.get_default_output_device()?,
            AudioEndpointPreference::Explicit { id, .. } => self.api.get_device_by_id(id)?,
        };
        Ok(AudioEndpoint::from(&device))
    }
}

impl Default for WasapiCaptureSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CaptureSource for WasapiCaptureSource {
    async fn endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        self.api.enumerate_render_endpoints()
    }

    async fn open(
        &self,
        preference: &AudioEndpointPreference,
    ) -> Result<Box<dyn CaptureWorker>, CaptureError> {
        // The native thread retains its permit even if this future is cancelled.
        let permit = Arc::clone(&self.resolve_slot)
            .acquire_owned()
            .await
            .map_err(|error| endpoints_error("resolution unavailable", error))?;
        let api = Arc::clone(&self.api);
        let preference = preference.clone();
        let (send, receive) = oneshot::channel();
        thread::Builder::new()
            .name("endpoint-resolution".into())
            .spawn(move || {
                let _permit = permit;
                let _com = initialize_mta();
                let result = match preference {
                    AudioEndpointPreference::SystemDefault => api.get_default_output_device(),
                    AudioEndpointPreference::Explicit { id, .. } => api.get_device_by_id(&id),
                }
                .map(|device| AudioEndpoint::from(&device));
                let _ = send.send(result);
            })
            .map_err(|error| endpoints_error("could not start resolution", error))?;
        let endpoint = receive
            .await
            .map_err(|error| endpoints_error("resolution ended", error))??;
        Ok(Box::new(WasapiCaptureWorker {
            api: Arc::clone(&self.api),
            device: WasapiDeviceRef {
                id: endpoint.id.clone(),
                name: endpoint.name.clone(),
            },
            endpoint,
            input_meter: Arc::clone(&self.input_meter),
            capture_generation: self.input_meter.generation().unwrap_or(0),
        }))
    }

    fn input_meter(&self) -> Option<Arc<CaptureInputMeter>> {
        Some(Arc::clone(&self.input_meter))
    }
}

/// Replaceable capture worker streaming one loopback endpoint into
/// normalized PCM frames.
pub struct WasapiCaptureWorker {
    api: Arc<dyn WasapiApi>,
    device: WasapiDeviceRef,
    endpoint: AudioEndpoint,
    input_meter: Arc<CaptureInputMeter>,
    capture_generation: u64,
}

impl CaptureWorker for WasapiCaptureWorker {
    fn run(
        self: Box<Self>,
        frames: crossbeam_channel::Sender<LivePcmFrame>,
        ready: oneshot::Sender<Result<AudioEndpoint, UserFacingError>>,
        cancel: CancellationToken,
    ) -> Result<(), CaptureError> {
        // Keep the COM guard alive for the lifetime of this thread.
        let _com = initialize_mta();
        let mut stream = self.api.open_loopback(&self.device)?;
        stream.start_stream()?;
        tracing::info!("loopback capture worker ready ({})", self.endpoint.name);
        // Readiness fires only AFTER start_stream succeeded (plan Task 10
        // Step 5); if nobody is left listening, stop cleanly.
        if ready.send(Ok(self.endpoint.clone())).is_err() {
            stream.stop_stream();
            return Ok(());
        }
        let result = wasapi_read_loop(
            stream.as_mut(),
            &frames,
            &cancel,
            &self.input_meter,
            self.capture_generation,
        );
        stream.stop_stream();
        result
    }
}

/// Read loop mirroring the legacy cast.rs pacing: block up to 50 ms on the
/// capture event, then drain whatever arrived. Cancellation is honored
/// between reads; a failed read stops the stream and reports the error.
fn wasapi_read_loop(
    stream: &mut dyn WasapiLoopbackStream,
    frames: &crossbeam_channel::Sender<LivePcmFrame>,
    cancel: &CancellationToken,
    input_meter: &CaptureInputMeter,
    capture_generation: u64,
) -> Result<(), CaptureError> {
    while !cancel.is_cancelled() {
        if !stream.wait_for_event(50) {
            continue;
        }
        if cancel.is_cancelled() {
            break;
        }
        let samples = stream.read_samples()?;
        if samples.is_empty() {
            continue;
        }
        // Drop-on-full mirrors the legacy path; the PcmBridge paces silence
        // and counts evictions truthfully on its own queue.
        let _ = frames.try_send(pcm_frame_from_f32_with_meter(
            samples,
            input_meter,
            capture_generation,
        ));
    }
    Ok(())
}

fn pcm_frame_from_f32_with_meter(
    samples: Vec<f32>,
    input_meter: &CaptureInputMeter,
    capture_generation: u64,
) -> LivePcmFrame {
    let observed = !samples.is_empty();
    let mut peak = 0.0_f32;
    let samples = samples
        .into_iter()
        .map(|sample| {
            let normalized = if sample.is_nan() {
                0.0
            } else {
                sample.clamp(-1.0, 1.0)
            };
            peak = peak.max(normalized.abs());
            (normalized * 32_767.0) as i16
        })
        .collect();
    if observed {
        input_meter.record_peak(
            capture_generation,
            (peak * 1_000.0).round().clamp(0.0, 1_000.0) as u16,
        );
    }
    LivePcmFrame {
        samples,
        channels: CAPTURE_CHANNELS,
        sample_rate: CAPTURE_SAMPLE_RATE,
    }
}

/// Supervisor control edges sent through a Tokio watch channel so the newest
/// request always wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureControl {
    /// Cancel the active worker and reopen capture with the latest endpoint
    /// preference; the decoder pair and its generation stay untouched.
    RequestEndpointChange,
    /// Cancel the active worker, hold the bridge on silence, and park until a
    /// [`Self::Resume`] arrives. The update channel stays open.
    Suspend,
    /// Leave the suspended state with a fresh capture generation.
    Resume,
    /// Cancel everything, publish `Stopped`, and close the update channel.
    Stop,
}

/// Generation-tagged supervisor output published on a bounded
/// [`SUPERVISOR_CAPACITY`] queue via non-blocking `try_send`.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // plan-fixed shape: `endpoint: AudioEndpoint` by value
pub enum CaptureUpdate {
    /// A new capture generation started opening a worker.
    Started {
        /// Generation of the stable decoder pair this update belongs to.
        decoder_generation: u64,
        /// Capture generation of the attempt.
        capture_generation: u64,
    },
    /// A worker finished initialization and delivers live audio.
    Ready {
        /// Generation of the stable decoder pair this update belongs to.
        decoder_generation: u64,
        /// Capture generation that became ready.
        capture_generation: u64,
        /// Endpoint resolved by the worker.
        endpoint: AudioEndpoint,
    },
    /// Replace-oldest delivery evicted queued frames.
    FrameDrop {
        /// Generation of the stable decoder pair this update belongs to.
        decoder_generation: u64,
        /// Capture generation active when frames were evicted.
        capture_generation: u64,
        /// Cumulative frames evicted under this decoder pair.
        dropped: u64,
    },
    /// A worker failed; a retry is scheduled on the ladder.
    Recovering {
        /// Generation of the stable decoder pair this update belongs to.
        decoder_generation: u64,
        /// Capture generation that failed.
        capture_generation: u64,
        /// Pre-redacted failure summary.
        error: UserFacingError,
        /// Monotonic deadline of the scheduled retry.
        retry_at: Instant,
    },
    /// Supervision stopped for this decoder-pair generation.
    Stopped {
        /// Decoder-pair generation that was stopped.
        decoder_generation: u64,
    },
    /// Capture is parked for a system suspend; the bridge paces silence.
    Suspended {
        /// Generation of the stable decoder pair this update belongs to.
        decoder_generation: u64,
        /// Capture generation that was cancelled.
        capture_generation: u64,
    },
}

/// Construction parameters of [`CaptureSupervisor`].
pub struct CaptureConfig {
    /// Sleep seam for retry-ladder delays.
    pub(crate) timer: Arc<dyn Timer>,
    /// Optional broadcast feed emitting `CaptureTransition`, `PcmDrop`,
    /// `SilenceBridge`, `Retry`, and `QueueWatermark` payloads.
    pub diagnostics: Option<broadcast::Sender<DiagnosticEvent>>,
    /// Shared view of the discovery counter this supervisor does not own, so
    /// its records line up with the controller's. `None` leaves it at zero.
    pub generations: Option<Arc<GenerationCell>>,
}

/// Factory for the live decoder pair; invoked exactly once per session
/// generation so the AirPlay timeline never notices capture recovery.
pub(crate) trait DecoderFactory: Send + Sync {
    /// Creates one `(sender, decoder)` pair with normalized parameters.
    fn create_pair(&self) -> (LiveFrameSender, LiveAudioDecoder);
}

/// Production factory wrapping [`LiveAudioDecoder::create_pair`].
///
/// Not yet constructed inside this crate: the device controller wires it in
/// a later plan task.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LiveDecoderFactory;

impl DecoderFactory for LiveDecoderFactory {
    fn create_pair(&self) -> (LiveFrameSender, LiveAudioDecoder) {
        LiveAudioDecoder::create_pair(CAPTURE_SAMPLE_RATE, CAPTURE_CHANNELS, CAPTURE_PCM_CAPACITY)
    }
}

/// One frame of true silence in the normalized layout (50 ms stereo).
pub(crate) fn silence_frame() -> LivePcmFrame {
    LivePcmFrame {
        samples: vec![0; CAPTURE_SAMPLE_RATE as usize * CAPTURE_CHANNELS as usize / 20],
        channels: CAPTURE_CHANNELS,
        sample_rate: CAPTURE_SAMPLE_RATE,
    }
}

/// Stable PCM bridge for one decoder-pair generation.
///
/// Forwards real worker frames through replace-oldest delivery, paces a
/// silence frame whenever no real frame arrives within [`SILENCE_PERIOD`],
/// and counts every eviction truthfully against the sender's own counters.
struct PcmBridge {
    sender: LiveFrameSender,
    pending_drops: u64,
    /// Whether the bridge is inside an uninterrupted run of evictions.
    ///
    /// Only the leading and trailing edge of a run are published, so a
    /// permanently backed-up decoder costs two controller wakeups instead of
    /// one per audio frame.
    in_eviction_run: bool,
    last_frame_was_silence: bool,
    updates_tx: mpsc::Sender<CaptureUpdate>,
    diagnostics: Option<broadcast::Sender<DiagnosticEvent>>,
    /// Shared view of the counters the bridge does not own itself.
    generations: Option<Arc<GenerationCell>>,
    decoder_generation: u64,
    capture_generation: Arc<AtomicU64>,
    dropped_total: Arc<AtomicU64>,
    listening_tone_active: Arc<AtomicBool>,
}

struct ListeningToneRequest {
    cancellation: CancellationToken,
}

struct ActiveListeningTone {
    samples: Vec<i16>,
    offset: usize,
    next_frame_at: Instant,
    cancellation: CancellationToken,
}

impl PcmBridge {
    /// Runs the forwarding loop until the session token is cancelled, the
    /// queue disconnects, or the supervisor task ends.
    fn run(
        mut self,
        worker_rx: crossbeam_channel::Receiver<LivePcmFrame>,
        injection_rx: crossbeam_channel::Receiver<LivePcmFrame>,
        listening_tone_rx: crossbeam_channel::Receiver<ListeningToneRequest>,
        root: CancellationToken,
    ) {
        let mut listening_tone: Option<ActiveListeningTone> = None;
        while !root.is_cancelled() {
            if listening_tone.is_none() {
                if let Ok(request) = listening_tone_rx.try_recv() {
                    if request.cancellation.is_cancelled() {
                        self.listening_tone_active.store(false, Ordering::Release);
                    } else {
                        drain_capture_frames(&worker_rx);
                        listening_tone = Some(ActiveListeningTone {
                            samples: crate::backend::listening_tone::generate_listening_tone(),
                            offset: 0,
                            next_frame_at: Instant::now(),
                            cancellation: request.cancellation,
                        });
                    }
                }
            }

            if let Some(tone) = listening_tone.as_mut() {
                // Never mix loopback capture into the diagnostic signal or
                // preserve a live backlog to burst out at its trailing edge.
                drain_capture_frames(&worker_rx);
                if tone.cancellation.is_cancelled() {
                    listening_tone = None;
                    self.listening_tone_active.store(false, Ordering::Release);
                    continue;
                }

                let now = Instant::now();
                if now < tone.next_frame_at {
                    thread::sleep((tone.next_frame_at - now).min(Duration::from_millis(2)));
                    continue;
                }

                let samples_per_frame =
                    CAPTURE_SAMPLE_RATE as usize * usize::from(CAPTURE_CHANNELS) / 50;
                let end = (tone.offset + samples_per_frame).min(tone.samples.len());
                let frame = LivePcmFrame {
                    samples: tone.samples[tone.offset..end].to_vec(),
                    channels: CAPTURE_CHANNELS,
                    sample_rate: CAPTURE_SAMPLE_RATE,
                };
                tone.offset = end;
                // Schedule from the actual send time so a stalled bridge
                // stretches safely instead of flooding catch-up frames.
                tone.next_frame_at = Instant::now() + LISTENING_TONE_FRAME_PERIOD;
                let finished = tone.offset == tone.samples.len();
                self.last_frame_was_silence = false;
                if !self.forward(frame) {
                    break;
                }
                if finished {
                    listening_tone = None;
                    self.listening_tone_active.store(false, Ordering::Release);
                }
                continue;
            }

            // An injected alignment pattern preempts live capture for its own
            // duration: the point of the test is to hear the clicks, not the
            // music underneath them. It enters the SAME bridge as capture, so
            // it reaches every receiver through the one shared ALAC/RTP
            // fan-out and cannot be receiver-specific by construction.
            if let Ok(frame) = injection_rx.try_recv() {
                self.last_frame_was_silence = false;
                if !self.forward(frame) {
                    break;
                }
                continue;
            }
            match worker_rx.recv_timeout(SILENCE_PERIOD) {
                Ok(frame) => {
                    self.last_frame_was_silence = false;
                    if !self.forward(frame) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if !self.last_frame_was_silence {
                        self.last_frame_was_silence = true;
                        emit(
                            &self.diagnostics,
                            correlate(
                                &self.generations,
                                self.capture_generation.load(Ordering::SeqCst),
                            ),
                            Severity::Info,
                            DiagnosticPayload::SilenceBridge {
                                duration: SILENCE_PERIOD,
                            },
                        );
                    }
                    if !self.forward(silence_frame()) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        self.listening_tone_active.store(false, Ordering::Release);
        self.flush_drops();
    }

    /// Forwards one frame; returns false when the decoder side is gone.
    ///
    /// Evictions are accounted immediately so `dropped_total` stays truthful
    /// at every instant, but they are *published* only at the edges of an
    /// eviction run. Publishing per frame would wake the device controller
    /// once per audio frame for as long as the decoder stays backed up.
    fn forward(&mut self, frame: LivePcmFrame) -> bool {
        match self.sender.try_send_latest(frame) {
            LiveSendOutcome::Enqueued => {
                if self.in_eviction_run {
                    self.in_eviction_run = false;
                    self.flush_drops();
                }
            }
            LiveSendOutcome::ReplacedOldest => {
                self.dropped_total.fetch_add(1, Ordering::SeqCst);
                self.pending_drops += 1;
                if !self.in_eviction_run {
                    self.in_eviction_run = true;
                    self.flush_drops();
                }
            }
            LiveSendOutcome::Disconnected => return false,
        }
        true
    }

    /// Publishes accumulated evictions as one coalesced `FrameDrop` edge.
    ///
    /// On a full lifecycle queue the watermark diagnostic is emitted and the
    /// count stays pending so the cumulative total is never lost.
    fn flush_drops(&mut self) {
        if self.pending_drops == 0 {
            return;
        }
        let newly_dropped = self.pending_drops;
        let total = self.dropped_total.load(Ordering::SeqCst);
        match self.updates_tx.try_send(CaptureUpdate::FrameDrop {
            decoder_generation: self.decoder_generation,
            capture_generation: self.capture_generation.load(Ordering::SeqCst),
            dropped: total,
        }) {
            Ok(()) => {
                self.pending_drops = 0;
                emit(
                    &self.diagnostics,
                    correlate(
                        &self.generations,
                        self.capture_generation.load(Ordering::SeqCst),
                    ),
                    Severity::Warning,
                    DiagnosticPayload::PcmDrop {
                        dropped: newly_dropped,
                    },
                );
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                emit(
                    &self.diagnostics,
                    correlate(
                        &self.generations,
                        self.capture_generation.load(Ordering::SeqCst),
                    ),
                    Severity::Warning,
                    DiagnosticPayload::QueueWatermark {
                        depth: SUPERVISOR_CAPACITY as u32,
                        capacity: SUPERVISOR_CAPACITY as u32,
                    },
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }
}

/// Discards at most the entire bounded capture queue in one bridge turn.
fn drain_capture_frames(worker_rx: &crossbeam_channel::Receiver<LivePcmFrame>) {
    for _ in 0..CAPTURE_PCM_CAPACITY {
        if worker_rx.try_recv().is_err() {
            break;
        }
    }
}

/// Internal supervisor state shared with the handle side.
pub(crate) struct CaptureState {
    decoder_generation: u64,
    capture_generation: u64,
    state: AudioSourceState,
    captured_endpoint: Option<AudioEndpoint>,
    preference: AudioEndpointPreference,
    child: Option<CancellationToken>,
    /// Canonical consumer handle of the stable PCM pair. It owns the pair's
    /// liveness latch and is never drained in production: session generations
    /// attach their own short-lived handles to the same queue through
    /// [`CaptureState::attach_decoder`].
    decoder: LiveAudioDecoder,
}

impl CaptureState {
    /// Current decoder-pair generation.
    ///
    /// This is the lifetime counter of the stable decoder pair, not the
    /// controller's session generation; the two are unrelated numbers.
    pub(crate) fn decoder_generation(&self) -> u64 {
        self.decoder_generation
    }

    /// Current capture generation.
    #[allow(dead_code)] // consumed by the controller in a later plan task
    pub(crate) fn capture_generation(&self) -> u64 {
        self.capture_generation
    }

    /// Mutable access to the canonical handle of the stable live decoder.
    ///
    /// Draining it steals frames from the attached session handle; production
    /// code must use [`Self::attach_decoder`] instead.
    pub(crate) fn decoder(&mut self) -> &mut LiveAudioDecoder {
        &mut self.decoder
    }

    /// Attaches one session-scoped consumer to the stable decoder pair.
    ///
    /// The pair itself is created exactly once per session generation, so a
    /// capture worker restart, an endpoint swap, or a whole AirPlay session
    /// rebuild never replaces the PCM queue the sender writes into. Dropping
    /// the returned handle leaves the pair — and the capture side — intact.
    pub(crate) fn attach_decoder(&self) -> LiveAudioDecoder {
        self.decoder.attach()
    }

    /// Clone of the live attempt's cancellation token, if any.
    pub(crate) fn child_token(&self) -> Option<CancellationToken> {
        self.child.clone()
    }

    /// Replaces the endpoint preference used by future opens.
    pub(crate) fn set_preference(&mut self, preference: AudioEndpointPreference) {
        self.preference = preference;
    }

    /// Builds the render-model view of the audio source.
    #[allow(dead_code)] // consumed by the controller in a later plan task
    pub(crate) fn snapshot(&self, active_endpoints: Vec<AudioEndpoint>) -> AudioSourceSnapshot {
        AudioSourceSnapshot {
            refresh_failed: false,
            active_endpoints: Some(active_endpoints),
            preference: self.preference.clone(),
            captured_endpoint: self.captured_endpoint.clone(),
            state: self.state,
            windows_input_peak_permille: None,
            pcm_frames_dropped_total: 0,
        }
    }
}

impl AudioSourceSnapshot {
    /// Snapshot of a capturing system where Windows itself is silent.
    pub(crate) fn silent_system() -> Self {
        Self {
            state: AudioSourceState::SilentSystem,
            ..Self::default()
        }
    }

    /// Snapshot while capture recovery is in progress; the redacted reason
    /// travels through diagnostics, not the presentation snapshot.
    pub(crate) fn recovering(_reason: UserFacingError) -> Self {
        Self {
            state: AudioSourceState::Recovering,
            ..Self::default()
        }
    }
}

/// Outcome of a non-blocking update send.
enum Flow {
    Sent,
    Overloaded,
    Closed,
}

/// Result of the interruptible ladder delay.
enum LadderOutcome {
    Elapsed,
    RestartNow,
    /// Park for a system suspend instead of climbing further.
    Suspend,
    Stop,
}

/// Long-running capture supervisor handle plus its control/state surfaces.
pub struct CaptureSupervisor {
    control_tx: watch::Sender<Option<CaptureControl>>,
    /// Producer half of the alignment-pattern injection queue feeding the
    /// stable PCM bridge. Bounded and drop-on-full: a saturated bridge is a
    /// reason to lose clicks, never to block the caller.
    injection_tx: crossbeam_channel::Sender<LivePcmFrame>,
    /// Single bounded request path into the existing PCM bridge thread.
    listening_tone_tx: crossbeam_channel::Sender<ListeningToneRequest>,
    listening_tone_active: Arc<AtomicBool>,
    /// `None` once [`CaptureSupervisor::take_updates`] handed the receive half
    /// to an owner outside this handle. The channel is created once in
    /// [`CaptureSupervisor::start`] and survives every capture generation --
    /// a worker restart or an endpoint swap replaces the child, never the
    /// queue -- so handing the receiver out once loses nothing.
    updates: Option<mpsc::Receiver<CaptureUpdate>>,
    shared: Arc<Mutex<CaptureState>>,
    pcm: AudioDiagnosticsSource,
    input_meter: Arc<CaptureInputMeter>,
    dropped_total: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

impl CaptureSupervisor {
    /// Spawns the supervisor over the given source, decoder factory, and
    /// configuration; decoder-pair generation one starts immediately.
    pub(crate) fn start(
        source: Arc<dyn CaptureSource>,
        factory: Arc<dyn DecoderFactory>,
        config: CaptureConfig,
    ) -> Self {
        let input_meter = source
            .input_meter()
            .unwrap_or_else(|| Arc::new(CaptureInputMeter::default()));
        let (control_tx, control_rx) = watch::channel(None);
        let (updates_tx, updates) = mpsc::channel(SUPERVISOR_CAPACITY);
        // The decoder pair is created here so its diagnostics source, the drop
        // counter, AND the attachable canonical handle are all reachable from
        // the handle before the supervisor task has been polled even once.
        let (sender, decoder) = factory.create_pair();
        let shared = Arc::new(Mutex::new(CaptureState {
            decoder_generation: 1,
            capture_generation: 0,
            state: AudioSourceState::Unavailable,
            captured_endpoint: None,
            preference: AudioEndpointPreference::default(),
            child: None,
            decoder,
        }));
        let pcm = sender.diagnostics_source(CAPTURE_SAMPLE_RATE);
        let dropped_total = Arc::new(AtomicU64::new(0));
        let (injection_tx, injection_rx) =
            crossbeam_channel::bounded::<LivePcmFrame>(CAPTURE_PCM_CAPACITY);
        let (listening_tone_tx, listening_tone_rx) = crossbeam_channel::bounded(1);
        let listening_tone_active = Arc::new(AtomicBool::new(false));
        let join = tokio::spawn(run_supervisor(
            source,
            config,
            control_rx,
            updates_tx,
            Arc::clone(&shared),
            sender,
            Arc::clone(&dropped_total),
            injection_rx,
            listening_tone_rx,
            Arc::clone(&listening_tone_active),
            Arc::clone(&input_meter),
        ));
        Self {
            control_tx,
            injection_tx,
            listening_tone_tx,
            listening_tone_active,
            updates: Some(updates),
            shared,
            pcm,
            input_meter,
            dropped_total,
            join: Some(join),
        }
    }

    /// Publishes a control edge; the newest value wins (watch semantics).
    pub fn request_control(&self, control: CaptureControl) {
        if matches!(control, CaptureControl::Suspend | CaptureControl::Stop) {
            self.input_meter
                .invalidate_generation(lock(&self.shared).capture_generation);
        }
        let _ = self.control_tx.send(Some(control));
    }

    /// Injects one interleaved PCM pattern into the stable bridge.
    ///
    /// Chunked into bridge-sized frames and enqueued without blocking; a full
    /// queue drops the remainder rather than stalling the actor. Neither the
    /// samples nor their count are recorded anywhere -- they are audio.
    pub fn inject_pattern(&self, samples: Vec<i16>) {
        let channels = usize::from(CAPTURE_CHANNELS);
        let chunk = CAPTURE_SAMPLE_RATE as usize * channels / 20; // 50 ms
        for frame in samples.chunks(chunk) {
            let sent = self.injection_tx.try_send(LivePcmFrame {
                samples: frame.to_vec(),
                channels: CAPTURE_CHANNELS,
                sample_rate: CAPTURE_SAMPLE_RATE,
            });
            if sent.is_err() {
                break;
            }
        }
    }

    /// Requests one finite, paced listening tone on the stable PCM bridge.
    ///
    /// Returns `false` when the token is already cancelled, another tone is
    /// active or pending, or the bridge has shut down. This call never waits
    /// for audio delivery.
    pub fn start_listening_tone(&self, cancellation: CancellationToken) -> bool {
        if cancellation.is_cancelled()
            || self
                .listening_tone_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }

        if self
            .listening_tone_tx
            .try_send(ListeningToneRequest { cancellation })
            .is_err()
        {
            self.listening_tone_active.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// Receive half of the bounded supervisor update queue.
    ///
    /// # Panics
    ///
    /// Panics once [`Self::take_updates`] has moved the receiver out. Owning
    /// the stream and borrowing it through the handle are mutually exclusive
    /// by construction; a caller doing both has two consumers of one queue.
    pub fn updates_mut(&mut self) -> &mut mpsc::Receiver<CaptureUpdate> {
        self.updates
            .as_mut()
            .expect("the update receiver was taken out of this supervisor")
    }

    /// Moves the receive half out of the handle, once.
    ///
    /// Callers that own the stream outright never need the supervisor lock to
    /// read it, so nothing has to wait on an update while holding a lock that
    /// control requests need. Returns `None` on every call after the first.
    pub fn take_updates(&mut self) -> Option<mpsc::Receiver<CaptureUpdate>> {
        self.updates.take()
    }

    /// Locks the internal capture state for inspection or snapshots.
    pub(crate) fn lock_state(&self) -> MutexGuard<'_, CaptureState> {
        lock(&self.shared)
    }

    /// Shares the internal capture state so the controller's decoder source
    /// can attach session handles without going through the controller's
    /// async supervisor mutex.
    pub(crate) fn shared_state(&self) -> Arc<Mutex<CaptureState>> {
        Arc::clone(&self.shared)
    }

    /// Decoder source handing every AirPlay session generation an attached
    /// consumer of this supervisor's stable PCM pair.
    pub fn decoder_source(&self) -> Arc<CaptureDecoderSource> {
        Arc::new(CaptureDecoderSource {
            shared: Arc::clone(&self.shared),
        })
    }

    /// Cumulative frames the bridge evicted from the full decoder queue.
    pub fn frame_drops_total(&self) -> u64 {
        self.dropped_total.load(Ordering::SeqCst)
    }

    /// The LiveFrameSender's own full-queue drop counter, read live.
    pub fn capture_queue_drops(&self) -> u64 {
        self.pcm.snapshot().capture_queue.full_queue_drops_total
    }

    /// Consumes the newest bounded Windows-input meter window and pairs it
    /// with the lifetime PCM eviction total.
    pub(crate) fn take_input_telemetry(&self) -> (Option<u16>, u64) {
        let state = lock(&self.shared);
        let peak = if state.state == AudioSourceState::Capturing {
            self.input_meter.take_window(state.capture_generation)
        } else {
            self.input_meter.discard_window(state.capture_generation);
            None
        };
        (peak, self.frame_drops_total())
    }

    /// Awaits clean task completion after a stop control edge.
    pub async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        self.join
            .take()
            .expect("join handle is only consumed once")
            .await
    }
}

/// Hands out session-scoped consumers of the capture supervisor's stable
/// decoder pair.
///
/// The pair is created once per session generation of the capture supervisor
/// and outlives every AirPlay session generation, so a capture worker restart,
/// an endpoint swap, or a full group rebuild never replaces the PCM queue the
/// bridge writes into — the AirPlay timeline cannot notice a capture recovery.
///
/// Locking is a plain synchronous `std::sync::Mutex` held for the duration of
/// one `attach` and nothing else, so the session supervisor may call this from
/// inside its spawned start task without an async lock.
pub struct CaptureDecoderSource {
    shared: Arc<Mutex<CaptureState>>,
}

impl SessionDecoderSource for CaptureDecoderSource {
    fn take_decoder(&self) -> LiveAudioDecoder {
        lock(&self.shared).attach_decoder()
    }
}

impl Drop for CaptureSupervisor {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

fn lock(shared: &Mutex<CaptureState>) -> MutexGuard<'_, CaptureState> {
    shared.lock().expect("capture state mutex poisoned")
}

#[allow(clippy::too_many_arguments)]
async fn run_supervisor(
    source: Arc<dyn CaptureSource>,
    config: CaptureConfig,
    mut control_rx: watch::Receiver<Option<CaptureControl>>,
    updates_tx: mpsc::Sender<CaptureUpdate>,
    shared: Arc<Mutex<CaptureState>>,
    sender: LiveFrameSender,
    dropped_total: Arc<AtomicU64>,
    injection: crossbeam_channel::Receiver<LivePcmFrame>,
    listening_tone: crossbeam_channel::Receiver<ListeningToneRequest>,
    listening_tone_active: Arc<AtomicBool>,
    input_meter: Arc<CaptureInputMeter>,
) {
    control_rx.borrow_and_update();
    // The stable decoder pair is created once and never replaced while this
    // supervisor lives, so its generation is a constant. It is NOT the
    // controller's session generation -- diagnostics take that one from the
    // shared cell, see `correlate`.
    let decoder_generation: u64 = 1;
    let root = CancellationToken::new();

    // The stable bridge outlives every worker swap for this session.
    let (worker_tx, worker_rx) = crossbeam_channel::bounded::<LivePcmFrame>(CAPTURE_PCM_CAPACITY);
    let injection_rx = injection;
    let capture_generation_cell = Arc::new(AtomicU64::new(0));
    let bridge = PcmBridge {
        sender,
        pending_drops: 0,
        in_eviction_run: false,
        last_frame_was_silence: false,
        updates_tx: updates_tx.clone(),
        diagnostics: config.diagnostics.clone(),
        generations: config.generations.clone(),
        decoder_generation,
        capture_generation: Arc::clone(&capture_generation_cell),
        dropped_total,
        listening_tone_active,
    };
    let bridge_handle = {
        let bridge_root = root.clone();
        thread::spawn(move || bridge.run(worker_rx, injection_rx, listening_tone, bridge_root))
    };
    let mut worker_handles: Vec<thread::JoinHandle<()>> = Vec::new();

    let mut capture_generation: u64 = 0;
    let mut ladder_index: usize = 0;
    let mut suspend_requested = false;

    'outer: loop {
        if suspend_requested {
            suspend_requested = false;
            // `capture_generation` still holds the generation the suspend just
            // cancelled, which is what the edge below names. The resume then
            // mints the next number: a generation is spent once and never
            // handed out twice, so anything correlating diagnostics by
            // generation cannot merge the discarded pre-suspend worker with
            // the fresh post-resume one.
            {
                let mut state = lock(&shared);
                state.state = AudioSourceState::Unavailable;
                state.captured_endpoint = None;
                state.child = None;
            }
            emit(
                &config.diagnostics,
                correlate(&config.generations, capture_generation),
                Severity::Info,
                DiagnosticPayload::CaptureTransition {
                    state: AudioSourceState::Unavailable,
                },
            );
            // The bridge outlives the worker and keeps pacing silence, so the
            // AirPlay timeline hears silence rather than a truncated buffer
            // while the session performs its bounded teardown.
            let _ = updates_tx.try_send(CaptureUpdate::Suspended {
                decoder_generation,
                capture_generation,
            });
            let resumed = loop {
                match next_control(&mut control_rx).await {
                    Some(CaptureControl::Resume) => break true,
                    // A suspended machine has no endpoint to swap to; the
                    // preference is stored already and the resume applies it.
                    Some(CaptureControl::RequestEndpointChange) | Some(CaptureControl::Suspend) => {
                        continue
                    }
                    Some(CaptureControl::Stop) | None => break false,
                }
            };
            if !resumed {
                break 'outer;
            }
            ladder_index = 0;
        }
        capture_generation += 1;
        input_meter.begin_generation(capture_generation);
        capture_generation_cell.store(capture_generation, Ordering::SeqCst);
        // Child of the session root so bounded teardown cascades.
        let child = root.child_token();
        {
            let mut state = lock(&shared);
            state.capture_generation = capture_generation;
            state.child = Some(child.clone());
        }
        match push_update(
            &config,
            &updates_tx,
            &child,
            capture_generation,
            CaptureUpdate::Started {
                decoder_generation,
                capture_generation,
            },
        ) {
            Flow::Sent => {}
            Flow::Overloaded => {
                tokio::task::yield_now().await;
                continue 'outer;
            }
            Flow::Closed => break 'outer,
        }

        // Open a worker, staying interruptible by control edges.
        enum OpenStep {
            Opened(Result<Box<dyn CaptureWorker>, CaptureError>),
            Restart,
            Suspend,
            Stop,
        }
        let preference = lock(&shared).preference.clone();
        let step = tokio::select! {
            opened = source.open(&preference) => OpenStep::Opened(opened),
            control = next_control(&mut control_rx) => match action_of(control) {
                ControlAction::Restart => {
                    child.cancel();
                    OpenStep::Restart
                }
                ControlAction::Suspend => {
                    child.cancel();
                    OpenStep::Suspend
                }
                ControlAction::Stop => OpenStep::Stop,
            },
        };
        let worker = match step {
            OpenStep::Stop => break 'outer,
            OpenStep::Suspend => {
                suspend_requested = true;
                continue 'outer;
            }
            OpenStep::Restart => continue 'outer,
            OpenStep::Opened(opened) => opened,
        };

        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                match climb_ladder(
                    &config,
                    &mut control_rx,
                    &updates_tx,
                    &shared,
                    decoder_generation,
                    capture_generation,
                    &child,
                    &mut ladder_index,
                    Some(error),
                )
                .await
                {
                    LadderOutcome::Elapsed | LadderOutcome::RestartNow => continue 'outer,
                    LadderOutcome::Suspend => {
                        suspend_requested = true;
                        continue 'outer;
                    }
                    LadderOutcome::Stop => break 'outer,
                }
            }
        };

        // Readiness handshake plus terminal outcome collection on one worker
        // thread; both stay interruptible by control edges.
        let (ready_tx, mut ready_rx) = oneshot::channel();
        let (outcome_tx, mut outcome_rx) = oneshot::channel();
        let tx = worker_tx.clone();
        let worker_child = child.clone();
        worker_handles.push(thread::spawn(move || {
            let result = worker.run(tx, ready_tx, worker_child);
            let _ = outcome_tx.send(result);
        }));

        enum WorkerStep {
            Ready(AudioEndpoint),
            Failed(Option<CaptureError>),
            Restart,
            Suspend,
            Stop,
        }
        let step = tokio::select! {
            ready = &mut ready_rx => match ready {
                Ok(Ok(endpoint)) => WorkerStep::Ready(endpoint),
                _ => WorkerStep::Failed(None),
            },
            control = next_control(&mut control_rx) => match action_of(control) {
                ControlAction::Restart => {
                    child.cancel();
                    WorkerStep::Restart
                }
                ControlAction::Suspend => {
                    child.cancel();
                    WorkerStep::Suspend
                }
                ControlAction::Stop => WorkerStep::Stop,
            },
        };
        let endpoint = match step {
            WorkerStep::Stop => break 'outer,
            WorkerStep::Suspend => {
                suspend_requested = true;
                continue 'outer;
            }
            WorkerStep::Restart => continue 'outer,
            WorkerStep::Failed(cause) => {
                match climb_ladder(
                    &config,
                    &mut control_rx,
                    &updates_tx,
                    &shared,
                    decoder_generation,
                    capture_generation,
                    &child,
                    &mut ladder_index,
                    cause,
                )
                .await
                {
                    LadderOutcome::Elapsed | LadderOutcome::RestartNow => continue 'outer,
                    LadderOutcome::Suspend => {
                        suspend_requested = true;
                        continue 'outer;
                    }
                    LadderOutcome::Stop => break 'outer,
                }
            }
            WorkerStep::Ready(endpoint) => endpoint,
        };

        {
            let mut state = lock(&shared);
            state.state = AudioSourceState::Capturing;
            state.captured_endpoint = Some(endpoint.clone());
        }
        ladder_index = 0;
        emit(
            &config.diagnostics,
            correlate(&config.generations, capture_generation),
            Severity::Info,
            DiagnosticPayload::CaptureTransition {
                state: AudioSourceState::Capturing,
            },
        );
        match push_update(
            &config,
            &updates_tx,
            &child,
            capture_generation,
            CaptureUpdate::Ready {
                decoder_generation,
                capture_generation,
                endpoint,
            },
        ) {
            Flow::Sent => {}
            Flow::Overloaded => {
                child.cancel();
                tokio::task::yield_now().await;
                continue 'outer;
            }
            Flow::Closed => break 'outer,
        }

        // Terminal phase: wait for worker failure, clean end, or control.
        enum TerminalStep {
            Failed(Option<CaptureError>),
            Restart,
            Suspend,
            Stop,
        }
        let terminal = tokio::select! {
            outcome = &mut outcome_rx => match outcome {
                Ok(Ok(())) => {
                    if pending_endpoint_change(&control_rx) {
                        child.cancel();
                        TerminalStep::Restart
                    } else {
                        TerminalStep::Failed(None)
                    }
                }
                Ok(Err(error)) => TerminalStep::Failed(Some(error)),
                Err(_) => TerminalStep::Failed(None),
            },
            control = next_control(&mut control_rx) => match action_of(control) {
                ControlAction::Restart => {
                    child.cancel();
                    TerminalStep::Restart
                }
                ControlAction::Suspend => {
                    child.cancel();
                    TerminalStep::Suspend
                }
                ControlAction::Stop => TerminalStep::Stop,
            },
        };
        match terminal {
            TerminalStep::Stop => break 'outer,
            TerminalStep::Suspend => {
                suspend_requested = true;
                continue 'outer;
            }
            TerminalStep::Restart => continue 'outer,
            TerminalStep::Failed(cause) => {
                match climb_ladder(
                    &config,
                    &mut control_rx,
                    &updates_tx,
                    &shared,
                    decoder_generation,
                    capture_generation,
                    &child,
                    &mut ladder_index,
                    cause,
                )
                .await
                {
                    LadderOutcome::Elapsed | LadderOutcome::RestartNow => continue 'outer,
                    LadderOutcome::Suspend => {
                        suspend_requested = true;
                        continue 'outer;
                    }
                    LadderOutcome::Stop => break 'outer,
                }
            }
        }
    }

    // Bounded teardown: cancel everything, join the bridge and workers, then
    // publish the final edge.
    root.cancel();
    lock(&shared).child = None;
    let workers_to_join = std::mem::take(&mut worker_handles);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = bridge_handle.join();
        for handle in workers_to_join {
            let _ = handle.join();
        }
    })
    .await;
    emit(
        &config.diagnostics,
        correlate(&config.generations, capture_generation),
        Severity::Info,
        DiagnosticPayload::CaptureTransition {
            state: AudioSourceState::Unavailable,
        },
    );
    let _ = push_update(
        &config,
        &updates_tx,
        &root,
        capture_generation,
        CaptureUpdate::Stopped { decoder_generation },
    );
}

/// Schedules the retry-ladder wait after a failure, staying interruptible.
#[allow(clippy::too_many_arguments)]
async fn climb_ladder(
    config: &CaptureConfig,
    control_rx: &mut watch::Receiver<Option<CaptureControl>>,
    updates_tx: &mpsc::Sender<CaptureUpdate>,
    shared: &Arc<Mutex<CaptureState>>,
    decoder_generation: u64,
    capture_generation: u64,
    child: &CancellationToken,
    ladder_index: &mut usize,
    cause: Option<CaptureError>,
) -> LadderOutcome {
    let delay = CAPTURE_RETRY_LADDER[(*ladder_index).min(CAPTURE_RETRY_LADDER.len() - 1)];
    let retry_at = Instant::now() + delay;
    let wall_deadline = SystemTime::now() + delay;
    {
        let mut state = lock(shared);
        state.state = AudioSourceState::Recovering;
        state.captured_endpoint = None;
    }
    let error = cause.map_or_else(
        || UserFacingError::new("audio capture worker ended unexpectedly"),
        |cause| UserFacingError::new(format!("audio capture failed: {cause}")),
    );
    match push_update(
        config,
        updates_tx,
        child,
        capture_generation,
        CaptureUpdate::Recovering {
            decoder_generation,
            capture_generation,
            error: error.clone(),
            retry_at,
        },
    ) {
        Flow::Sent => {}
        Flow::Overloaded => {
            child.cancel();
            return LadderOutcome::RestartNow;
        }
        Flow::Closed => return LadderOutcome::Stop,
    }
    emit(
        &config.diagnostics,
        correlate(&config.generations, capture_generation),
        Severity::Warning,
        DiagnosticPayload::CaptureTransition {
            state: AudioSourceState::Recovering,
        },
    );
    emit(
        &config.diagnostics,
        correlate(&config.generations, capture_generation),
        Severity::Warning,
        DiagnosticPayload::Retry {
            receiver: None,
            attempt: *ladder_index as u32,
            next_at: wall_deadline,
        },
    );
    *ladder_index = (*ladder_index + 1).min(CAPTURE_RETRY_LADDER.len());

    tokio::select! {
        _ = config.timer.sleep(delay) => LadderOutcome::Elapsed,
        control = next_control(control_rx) => match action_of(control) {
            ControlAction::Restart => {
                child.cancel();
                LadderOutcome::RestartNow
            }
            ControlAction::Suspend => {
                child.cancel();
                LadderOutcome::Suspend
            }
            ControlAction::Stop => LadderOutcome::Stop,
        },
    }
}

/// What a control edge means for the generation currently being built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlAction {
    /// Cancel this generation and open a fresh one.
    Restart,
    /// Cancel this generation and park until a resume arrives.
    Suspend,
    /// Cancel everything and end supervision.
    Stop,
}

/// Classifies one control edge exhaustively, so a new variant is a compile
/// error instead of a silent stop.
fn action_of(control: Option<CaptureControl>) -> ControlAction {
    match control {
        Some(CaptureControl::RequestEndpointChange) => ControlAction::Restart,
        Some(CaptureControl::Suspend) => ControlAction::Suspend,
        // A resume with no suspend outstanding is a no-op that must not lose
        // the running worker; reopening the same endpoint is the closest
        // truthful answer the loop can give.
        Some(CaptureControl::Resume) => ControlAction::Restart,
        Some(CaptureControl::Stop) | None => ControlAction::Stop,
    }
}

/// Resolves the latest control edge; `None` means the channel closed.
///
/// A closed channel is reported honestly rather than answered with the last
/// value seen. `changed()` completes instantly once every sender is gone, so
/// repeating a stale value would leave callers with a future that never yields
/// -- and the suspend park, whose stale value is `Suspend`, would spin on it
/// forever.
async fn next_control(
    control_rx: &mut watch::Receiver<Option<CaptureControl>>,
) -> Option<CaptureControl> {
    if control_rx.changed().await.is_err() {
        return None;
    }
    *control_rx.borrow_and_update()
}

/// Non-consuming check for a pending endpoint-change request.
fn pending_endpoint_change(control_rx: &watch::Receiver<Option<CaptureControl>>) -> bool {
    matches!(
        *control_rx.borrow(),
        Some(CaptureControl::RequestEndpointChange)
    )
}

fn push_update(
    config: &CaptureConfig,
    updates_tx: &mpsc::Sender<CaptureUpdate>,
    child: &CancellationToken,
    capture_generation: u64,
    update: CaptureUpdate,
) -> Flow {
    match updates_tx.try_send(update) {
        Ok(()) => Flow::Sent,
        Err(mpsc::error::TrySendError::Full(_)) => {
            emit(
                &config.diagnostics,
                correlate(&config.generations, capture_generation),
                Severity::Warning,
                DiagnosticPayload::QueueWatermark {
                    depth: SUPERVISOR_CAPACITY as u32,
                    capacity: SUPERVISOR_CAPACITY as u32,
                },
            );
            child.cancel();
            Flow::Overloaded
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Flow::Closed,
    }
}

/// Correlation key of a capture record.
///
/// This supervisor owns exactly one of the three counters -- the capture
/// generation -- and overrides only that one. The session and discovery
/// generations belong to the controller and to the discovery supervisor; they
/// are read from the shared cell so a capture drop and a receiver failure in
/// the same session carry the same key. Writing a locally invented session
/// number here would make every capture record claim the session that was
/// live at process start, forever.
fn correlate(generations: &Option<Arc<GenerationCell>>, capture_generation: u64) -> Generations {
    let mut correlation = generations
        .as_ref()
        .map(|cell| cell.get())
        .unwrap_or_default();
    correlation.capture = capture_generation;
    correlation
}

fn emit(
    diagnostics: &Option<broadcast::Sender<DiagnosticEvent>>,
    correlation: Generations,
    severity: Severity,
    payload: DiagnosticPayload,
) {
    let Some(sender) = diagnostics.as_ref() else {
        return;
    };
    let category = diagnostic_category(payload.kind());
    let _ = sender.send(DiagnosticEvent {
        monotonic_ns: monotonic_ns_now(),
        wall_time: SystemTime::now(),
        session_generation: correlation.session,
        discovery_generation: correlation.discovery,
        capture_generation: correlation.capture,
        receiver: None,
        severity,
        category,
        payload,
    });
}

fn diagnostic_category(kind: DiagnosticKind) -> DiagnosticCategory {
    match kind {
        DiagnosticKind::CaptureTransition
        | DiagnosticKind::PcmDrop
        | DiagnosticKind::SilenceBridge => DiagnosticCategory::Capture,
        DiagnosticKind::Retry => DiagnosticCategory::Retry,
        DiagnosticKind::QueueWatermark => DiagnosticCategory::Queue,
        _ => DiagnosticCategory::General,
    }
}

fn monotonic_ns_now() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

// The shared fixtures file is intentionally included once per test module
// (model and discovery already include it), so this duplicate registration
// is deliberate.
#[allow(clippy::duplicate_mod)]
#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use airplay_audio::{LiveAudioDecoder, LiveFrameSender};
    use tokio::sync::broadcast;

    use super::support::{
        audio_endpoint, user_error, FakeCaptureSource, FakeWasapiApi, FakeWorkerPlan,
    };
    use super::*;

    /// Timer seam recorder: captures every requested delay and completes
    /// instantly; the recorded durations are the observable timing contract.
    #[derive(Default)]
    struct RecordingTimer {
        recorded: Mutex<Vec<Duration>>,
    }

    impl RecordingTimer {
        fn recorded(&self) -> Vec<Duration> {
            self.recorded.lock().expect("timer mutex poisoned").clone()
        }
    }

    impl Timer for RecordingTimer {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            self.recorded
                .lock()
                .expect("timer mutex poisoned")
                .push(duration);
            Box::pin(std::future::ready(()))
        }
    }

    /// Decoder factory seam counter: proves the pair is created exactly once
    /// per session generation.
    struct CountingFactory {
        pairs: AtomicUsize,
    }

    impl CountingFactory {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                pairs: AtomicUsize::new(0),
            })
        }

        fn count(&self) -> usize {
            self.pairs.load(Ordering::SeqCst)
        }
    }

    impl DecoderFactory for CountingFactory {
        fn create_pair(&self) -> (LiveFrameSender, LiveAudioDecoder) {
            self.pairs.fetch_add(1, Ordering::SeqCst);
            LiveAudioDecoder::create_pair(
                CAPTURE_SAMPLE_RATE,
                CAPTURE_CHANNELS,
                CAPTURE_PCM_CAPACITY,
            )
        }
    }

    struct CaptureHarness {
        supervisor: CaptureSupervisor,
        source: Arc<FakeCaptureSource>,
        factory: Arc<CountingFactory>,
        timer: Arc<RecordingTimer>,
        diagnostics_rx: broadcast::Receiver<DiagnosticEvent>,
    }

    impl CaptureHarness {
        fn start(source: FakeCaptureSource) -> Self {
            let source = Arc::new(source);
            let timer = Arc::new(RecordingTimer::default());
            let factory = CountingFactory::new();
            let (diagnostics_tx, diagnostics_rx) = broadcast::channel(2_048);
            let config = CaptureConfig {
                generations: None,
                timer: timer.clone() as Arc<dyn Timer>,
                diagnostics: Some(diagnostics_tx),
            };
            let supervisor = CaptureSupervisor::start(source.clone(), factory.clone(), config);
            Self {
                supervisor,
                source,
                factory,
                timer,
                diagnostics_rx,
            }
        }

        async fn recv_update(&mut self) -> CaptureUpdate {
            self.supervisor
                .updates_mut()
                .recv()
                .await
                .expect("capture update channel stays open until shutdown")
        }

        /// Receives the next lifecycle edge, tolerating interleaved
        /// `FrameDrop` edges from the bridge.
        async fn recv_lifecycle(&mut self) -> CaptureUpdate {
            loop {
                match self.recv_update().await {
                    CaptureUpdate::FrameDrop { .. } => continue,
                    lifecycle => return lifecycle,
                }
            }
        }

        /// Requests a clean shutdown and awaits task completion.
        async fn shutdown_and_join(self) {
            self.supervisor.request_control(CaptureControl::Stop);
            self.supervisor.join().await.expect("task ends cleanly");
        }

        /// Requests shutdown and drains updates until the channel closes,
        /// keeping the supervisor usable for final counter reads.
        async fn stop_and_drain(&mut self) {
            self.supervisor.request_control(CaptureControl::Stop);
            while self.supervisor.updates_mut().recv().await.is_some() {}
        }

        fn drain_diagnostics(&mut self) -> Vec<DiagnosticPayload> {
            let mut payloads = Vec::new();
            loop {
                match self.diagnostics_rx.try_recv() {
                    Ok(event) => payloads.push(event.payload),
                    Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                        panic!("diagnostic receiver lagged by {dropped}");
                    }
                    Err(broadcast::error::TryRecvError::Empty)
                    | Err(broadcast::error::TryRecvError::Closed) => break,
                }
            }
            payloads
        }
    }

    mod decoder_ownership {
        use super::*;

        #[tokio::test]
        async fn decoder_pair_created_once_per_session_generation() {
            let source = FakeCaptureSource::new();
            // Worker one fails after two frames (forces recovery + reopen);
            // worker two streams until the endpoint change swaps it.
            source.push_open_ok(FakeWorkerPlan::FramesThenError(2), audio_endpoint(0x0A));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0B));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0C));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    decoder_generation: 1,
                    capture_generation: 1
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready { endpoint, .. } => {
                    assert_eq!(endpoint.id, "endpoint-0A");
                }
                other => panic!("expected Ready for worker one, got {other:?}"),
            }
            match harness.recv_lifecycle().await {
                CaptureUpdate::Recovering { error, .. } => {
                    assert!(error.as_str().contains("fake capture worker failed"));
                }
                other => panic!("expected Recovering after worker failure, got {other:?}"),
            }
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    decoder_generation: 1,
                    capture_generation: 2
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready { endpoint, .. } => {
                    assert_eq!(endpoint.id, "endpoint-0B");
                }
                other => panic!("expected Ready for worker two, got {other:?}"),
            }

            harness
                .supervisor
                .request_control(CaptureControl::RequestEndpointChange);
            match harness.recv_lifecycle().await {
                CaptureUpdate::Started {
                    capture_generation, ..
                } => {
                    assert_eq!(capture_generation, 3);
                }
                other => panic!("expected Started for swapped worker, got {other:?}"),
            }
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready { endpoint, .. } => {
                    assert_eq!(endpoint.id, "endpoint-0C");
                }
                other => panic!("expected Ready for swapped worker, got {other:?}"),
            }

            assert_eq!(
                harness.factory.count(),
                1,
                "one decoder pair survives recovery and endpoint swap"
            );
            {
                let mut state = harness.supervisor.lock_state();
                assert_eq!(state.decoder_generation(), 1);
                assert_eq!(
                    state.decoder().sample_rate(),
                    CAPTURE_SAMPLE_RATE,
                    "decoder stays alive and readable"
                );
            }
            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn stable_pair_is_attachable_before_the_supervisor_task_runs() {
            // The session supervisor may ask for a decoder in the very same
            // scheduler tick the capture supervisor was started in, so the
            // stable pair must be reachable without awaiting anything.
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0A));
            let mut harness = CaptureHarness::start(source);

            // No await between `start` and `attach_decoder`: the supervisor
            // task has provably not been polled yet at this point.
            let attached = harness
                .supervisor
                .shared_state()
                .lock()
                .unwrap()
                .attach_decoder();
            assert_eq!(attached.sample_rate(), CAPTURE_SAMPLE_RATE);
            assert_eq!(attached.channels(), CAPTURE_CHANNELS);

            // Let the supervisor reach its control-watch loop before stopping
            // it (a Stop published before the first poll is swallowed by the
            // supervisor's initial `borrow_and_update`).
            harness.recv_lifecycle().await;
            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn every_session_generation_attaches_to_the_same_pair() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0A));
            let mut harness = CaptureHarness::start(source);

            harness.recv_lifecycle().await;
            let shared = harness.supervisor.shared_state();
            let first = shared.lock().unwrap().attach_decoder();
            drop(first);
            let second = shared.lock().unwrap().attach_decoder();
            drop(second);

            assert_eq!(
                harness.factory.count(),
                1,
                "attaching must never create a second decoder pair"
            );
            // A third attach after two handle lifetimes still sees a live pair.
            let mut third = shared.lock().unwrap().attach_decoder();
            third.set_recv_timeout(Duration::from_millis(200));
            assert!(!third.is_eof(), "the stable pair outlives its consumers");

            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn the_decoder_source_feeds_every_session_generation_from_one_pair() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0A));
            let mut harness = CaptureHarness::start(source);

            // Reachable without awaiting: the session supervisor calls this
            // synchronously from inside its spawned start task.
            let decoders: Arc<dyn SessionDecoderSource> = harness.supervisor.decoder_source();
            let first = decoders.take_decoder();
            assert_eq!(first.sample_rate(), CAPTURE_SAMPLE_RATE);
            assert_eq!(first.channels(), CAPTURE_CHANNELS);
            drop(first);

            harness.recv_lifecycle().await;
            let second = decoders.take_decoder();
            assert!(
                !second.is_eof(),
                "a later generation still attaches to a live pair"
            );
            drop(second);
            assert_eq!(
                harness.factory.count(),
                1,
                "the stable pair is never replaced per generation"
            );

            harness.shutdown_and_join().await;
        }

        #[test]
        fn audio_snapshot_distinguishes_silence_from_failed_capture() {
            assert!(matches!(
                AudioSourceSnapshot::silent_system().state,
                AudioSourceState::SilentSystem
            ));
            assert!(matches!(
                AudioSourceSnapshot::recovering(user_error("endpoint lost")).state,
                AudioSourceState::Recovering
            ));
        }
    }

    mod retry_ladder {
        use super::*;

        #[tokio::test]
        async fn open_failure_retries_ladder_then_recovers() {
            let source = FakeCaptureSource::new();
            source.push_open_err("endpoint enumeration exploded");
            source.push_open_err("endpoint enumeration exploded again");
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x21));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    decoder_generation: 1,
                    capture_generation: 1
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Recovering {
                    capture_generation,
                    retry_at,
                    ..
                } => {
                    assert_eq!(capture_generation, 1);
                    assert!(retry_at > Instant::now());
                }
                other => panic!("expected Recovering, got {other:?}"),
            }
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 2,
                    ..
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Recovering { .. } => {}
                other => panic!("expected second Recovering, got {other:?}"),
            }
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 3,
                    ..
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready { endpoint, .. } => {
                    assert_eq!(endpoint.id, "endpoint-21");
                }
                other => panic!("expected Ready after ladder, got {other:?}"),
            }

            assert_eq!(
                harness.timer.recorded(),
                vec![Duration::from_millis(250), Duration::from_millis(500)],
                "open failures climb the injected 250 ms/500 ms rungs"
            );

            harness.shutdown_and_join().await;
        }
    }

    mod silence_bridge {
        use super::*;

        #[tokio::test]
        async fn silence_bridge_flows_while_capture_recovering() {
            let source = FakeCaptureSource::new();
            // The worker never becomes ready; the bridge still paces silence.
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x31));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));

            // Poll-decode until the first paced silence frame shows up.
            let deadline = Instant::now() + Duration::from_secs(5);
            let frame = loop {
                {
                    let mut state = harness.supervisor.lock_state();
                    if let Some(decoded) = state.decoder().decode_frame().unwrap() {
                        break decoded;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "silence bridge produced no frame within five seconds"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            };

            assert!(
                frame.samples.iter().all(|sample| *sample == 0),
                "bridge frames are true silence"
            );
            assert_eq!(frame.sample_rate, CAPTURE_SAMPLE_RATE);
            assert_eq!(frame.channels, CAPTURE_CHANNELS);
            assert!(!harness.supervisor.lock_state().decoder().is_eof());

            harness.shutdown_and_join().await;
        }
    }

    mod alignment_injection {
        use super::*;

        use crate::calibration::{calibration_click_pattern, CALIBRATION_CLICK_AMPLITUDE};

        #[tokio::test]
        async fn an_injected_click_pattern_reaches_the_shared_pcm_bridge() {
            // The alignment test must travel the SAME bridge as live capture:
            // one shared ALAC/RTP fan-out means every receiver hears the
            // identical signal, so what differs between speakers can only be
            // the per-target presentation clock.
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x41));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));

            harness.supervisor.inject_pattern(calibration_click_pattern(
                CAPTURE_SAMPLE_RATE,
                CAPTURE_CHANNELS,
            ));

            // Collect until a non-silent sample arrives; the bridge is also
            // pacing silence, so interleaved zero frames are expected.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut peak = 0_i16;
            let mut layout = None;
            while Instant::now() < deadline && peak == 0 {
                {
                    let mut state = harness.supervisor.lock_state();
                    while let Some(frame) = state.decoder().decode_frame().unwrap() {
                        layout = Some((frame.sample_rate, frame.channels));
                        peak = peak.max(frame.samples.iter().copied().max().unwrap_or(0));
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            assert_eq!(
                peak, CALIBRATION_CLICK_AMPLITUDE,
                "the click reached the bridge at its fixed amplitude"
            );
            assert_eq!(
                layout,
                Some((CAPTURE_SAMPLE_RATE, CAPTURE_CHANNELS)),
                "injection keeps the normalized live layout"
            );

            harness.shutdown_and_join().await;
        }
    }

    mod listening_tone {
        use super::*;

        #[tokio::test]
        async fn tone_reaches_an_attached_session_decoder_without_canonical_drainage() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x50));
            let mut harness = CaptureHarness::start(source);
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));

            let decoder_source = harness.supervisor.decoder_source();
            let mut session_decoder = decoder_source.take_decoder();
            let cancellation = CancellationToken::new();
            assert!(harness
                .supervisor
                .start_listening_tone(cancellation.clone()));

            let expected_samples = CAPTURE_SAMPLE_RATE as usize * CAPTURE_CHANNELS as usize * 2;
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut tone_samples = 0usize;
            while tone_samples < expected_samples {
                while let Some(frame) = session_decoder.decode_frame().unwrap() {
                    if frame.samples.iter().any(|sample| *sample != 0) {
                        assert_eq!(frame.sample_rate, CAPTURE_SAMPLE_RATE);
                        assert_eq!(frame.channels, CAPTURE_CHANNELS);
                        assert!(frame
                            .samples
                            .iter()
                            .all(|sample| sample.unsigned_abs() <= 8_192));
                        tone_samples += frame.samples.len();
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "the attached session decoder lost part of the finite tone"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            assert_eq!(tone_samples, expected_samples);
            cancellation.cancel();
            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn tone_is_paced_through_the_stable_bridge_and_rejects_duplicates() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x51));
            let mut harness = CaptureHarness::start(source);
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));

            let cancellation = CancellationToken::new();
            assert!(harness
                .supervisor
                .start_listening_tone(cancellation.clone()));
            assert!(
                !harness
                    .supervisor
                    .start_listening_tone(CancellationToken::new()),
                "an active or pending tone request must be unique"
            );

            let started = Instant::now();
            let mut frames = 0usize;
            let mut samples = 0usize;
            while samples < CAPTURE_SAMPLE_RATE as usize * CAPTURE_CHANNELS as usize * 2 {
                {
                    let mut state = harness.supervisor.lock_state();
                    while let Some(frame) = state.decoder().decode_frame().unwrap() {
                        if frame.samples.iter().any(|sample| *sample != 0) {
                            assert_eq!(frame.sample_rate, CAPTURE_SAMPLE_RATE);
                            assert_eq!(frame.channels, CAPTURE_CHANNELS);
                            assert_eq!(
                                frame.samples.len(),
                                1_764,
                                "tone frames are paced at 20 ms"
                            );
                            assert!(frame
                                .samples
                                .iter()
                                .all(|sample| sample.unsigned_abs() <= 8_192));
                            frames += 1;
                            samples += frame.samples.len();
                        }
                    }
                }
                assert!(started.elapsed() < Duration::from_secs(4));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            assert_eq!(frames, 100);
            assert!(
                started.elapsed() >= Duration::from_millis(1_700),
                "the finite two-second tone must not be flooded into the decoder queue"
            );
            harness.shutdown_and_join().await;
        }

        #[tokio::test]
        async fn cancellation_stops_tone_without_leaving_queued_audio() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x52));
            let mut harness = CaptureHarness::start(source);
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));

            let already_cancelled = CancellationToken::new();
            already_cancelled.cancel();
            assert!(!harness.supervisor.start_listening_tone(already_cancelled));

            let cancellation = CancellationToken::new();
            assert!(harness
                .supervisor
                .start_listening_tone(cancellation.clone()));

            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let heard_tone = {
                    let mut state = harness.supervisor.lock_state();
                    state
                        .decoder()
                        .decode_frame()
                        .unwrap()
                        .is_some_and(|frame| frame.samples.iter().any(|sample| *sample != 0))
                };
                if heard_tone {
                    break;
                }
                assert!(Instant::now() < deadline, "tone never reached the bridge");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            cancellation.cancel();
            tokio::time::sleep(Duration::from_millis(80)).await;
            let mut residual_nonzero = false;
            {
                let mut state = harness.supervisor.lock_state();
                while let Some(frame) = state.decoder().decode_frame().unwrap() {
                    residual_nonzero |= frame.samples.iter().any(|sample| *sample != 0);
                }
            }
            assert!(
                !residual_nonzero,
                "cancelled tone frames must not remain queued for a later session"
            );

            let replacement = CancellationToken::new();
            assert!(
                harness.supervisor.start_listening_tone(replacement.clone()),
                "cancellation releases the single request slot"
            );
            replacement.cancel();
            harness.shutdown_and_join().await;
        }
    }

    mod input_telemetry {
        use super::*;

        #[test]
        fn real_pcm_conversion_records_a_finite_clamped_windows_peak() {
            let meter = CaptureInputMeter::default();
            meter.begin_generation(7);

            let frame = pcm_frame_from_f32_with_meter(vec![-1.5, -1.0, 0.25, f32::NAN], &meter, 7);

            assert_eq!(frame.samples, vec![-32_767, -32_767, 8_191, 0]);
            assert_eq!(meter.take_window(7), Some(1_000));
            assert_eq!(meter.take_window(7), None, "a window is consumed once");
        }

        #[test]
        fn observed_silence_differs_from_no_recent_windows_observation() {
            let meter = CaptureInputMeter::default();
            meter.begin_generation(3);

            assert_eq!(meter.take_window(3), None);
            let _ = pcm_frame_from_f32_with_meter(vec![0.0, -0.0], &meter, 3);
            assert_eq!(meter.take_window(3), Some(0));
            assert_eq!(meter.take_window(3), None);
        }

        #[test]
        fn stale_or_cancelled_capture_generations_cannot_repopulate_the_meter() {
            let meter = CaptureInputMeter::default();
            meter.begin_generation(11);
            let _ = pcm_frame_from_f32_with_meter(vec![0.75], &meter, 11);

            meter.begin_generation(12);
            let _ = pcm_frame_from_f32_with_meter(vec![1.0], &meter, 11);
            assert_eq!(
                meter.take_window(12),
                None,
                "the new generation must not inherit either old observations or late writes"
            );

            let _ = pcm_frame_from_f32_with_meter(vec![0.5], &meter, 12);
            assert_eq!(meter.take_window(12), Some(500));
            meter.invalidate_generation(12);
            let _ = pcm_frame_from_f32_with_meter(vec![1.0], &meter, 12);
            assert_eq!(meter.take_window(12), None);
        }

        #[test]
        fn stale_windows_input_window_is_not_presented_as_current() {
            let meter = CaptureInputMeter::default();
            let observed_at = Instant::now();
            meter.begin_generation(21);
            meter.record_peak_at(21, 800, observed_at);

            assert_eq!(
                meter.take_window_at(21, observed_at + Duration::from_secs(1)),
                Some(800)
            );
            meter.record_peak_at(21, 900, observed_at);
            assert_eq!(
                meter.take_window_at(21, observed_at + Duration::from_millis(1_001)),
                None
            );
        }

        #[test]
        fn empty_batches_do_not_fabricate_observed_silence() {
            let meter = CaptureInputMeter::default();
            meter.begin_generation(22);

            let frame = pcm_frame_from_f32_with_meter(Vec::new(), &meter, 22);

            assert!(frame.samples.is_empty());
            assert_eq!(meter.take_window(22), None);
        }

        #[test]
        fn telemetry_preserves_the_existing_infinity_clamp_semantics() {
            let meter = CaptureInputMeter::default();
            meter.begin_generation(23);

            let frame =
                pcm_frame_from_f32_with_meter(vec![f32::NEG_INFINITY, f32::INFINITY], &meter, 23);

            assert_eq!(frame.samples, vec![-32_767, 32_767]);
            assert_eq!(meter.take_window(23), Some(1_000));
        }

        #[tokio::test]
        async fn sampling_during_pending_open_does_not_invalidate_the_new_generation() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x61));
            let mut harness = CaptureHarness::start(source);
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 1,
                    ..
                }
            ));

            assert_eq!(harness.supervisor.take_input_telemetry().0, None);
            harness.supervisor.input_meter.record_peak(1, 625);
            harness.supervisor.lock_state().state = AudioSourceState::Capturing;

            assert_eq!(
                harness.supervisor.take_input_telemetry().0,
                Some(625),
                "the pending-open sample must discard stale data without deleting generation 1"
            );
            harness.shutdown_and_join().await;
        }
    }

    mod drop_accounting {
        use super::*;

        #[test]
        fn pcm_drop_payload_reports_the_new_delta_not_the_lifetime_total() {
            let (sender, _decoder) =
                LiveAudioDecoder::create_pair(CAPTURE_SAMPLE_RATE, CAPTURE_CHANNELS, 1);
            let (updates_tx, _updates_rx) = mpsc::channel(SUPERVISOR_CAPACITY);
            let (diagnostics_tx, mut diagnostics_rx) = broadcast::channel(8);
            let mut bridge = PcmBridge {
                sender,
                pending_drops: 2,
                in_eviction_run: true,
                last_frame_was_silence: false,
                updates_tx,
                diagnostics: Some(diagnostics_tx),
                generations: None,
                decoder_generation: 1,
                capture_generation: Arc::new(AtomicU64::new(4)),
                dropped_total: Arc::new(AtomicU64::new(9)),
                listening_tone_active: Arc::new(AtomicBool::new(false)),
            };

            bridge.flush_drops();

            let event = diagnostics_rx
                .try_recv()
                .expect("one drop delta is emitted");
            assert_eq!(event.payload, DiagnosticPayload::PcmDrop { dropped: 2 });
            assert_eq!(bridge.dropped_total.load(Ordering::SeqCst), 9);
        }

        #[tokio::test]
        async fn pcm_drops_counted_truthfully_on_decoder_full() {
            let source = FakeCaptureSource::new();
            // Far more frames than the 32-frame decoder queue can hold while
            // nobody decodes; the bridge must evict oldest-first truthfully.
            // A follow-up hanging worker serves the ladder retry cleanly.
            source.push_open_ok(FakeWorkerPlan::FramesThenError(40), audio_endpoint(0x41));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x42));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready { .. } => {}
                other => panic!("expected Ready, got {other:?}"),
            }
            // Wait for the worker's terminal failure; by then all 40 frames
            // have been handed to the bridge queue.
            match harness.recv_lifecycle().await {
                CaptureUpdate::Recovering { .. } => {}
                other => panic!("expected Recovering after flood, got {other:?}"),
            }
            // Let the bridge drain its internal queue into the decoder.
            tokio::time::sleep(Duration::from_millis(300)).await;

            harness.stop_and_drain().await;

            let bridge_reported = harness.supervisor.frame_drops_total();
            let sender_side_total = harness.supervisor.capture_queue_drops();
            assert!(
                bridge_reported >= 8,
                "40 frames into a 32-frame queue must drop some: {bridge_reported}"
            );
            assert_eq!(
                bridge_reported, sender_side_total,
                "bridge-reported drops equal the LiveFrameSender's own counters"
            );

            let payloads = harness.drain_diagnostics();
            let diagnostic_drops = payloads
                .iter()
                .filter_map(|payload| match payload {
                    DiagnosticPayload::PcmDrop { dropped } => Some(*dropped),
                    _ => None,
                })
                .sum::<u64>();
            assert_eq!(
                diagnostic_drops, bridge_reported,
                "PcmDrop diagnostics partition the lifetime total into non-overlapping deltas"
            );
        }

        /// A sustained eviction run must not wake the controller once per
        /// frame. The bridge documents `FrameDrop` as a *coalesced edge*;
        /// this pins that contract against the drain pattern the device
        /// controller actually uses (continuous, non-blocking).
        #[tokio::test]
        async fn a_sustained_eviction_run_publishes_bounded_frame_drop_edges() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::FramesThenError(400), audio_endpoint(0x41));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x42));
            let mut harness = CaptureHarness::start(source);

            let mut frame_drop_edges = 0usize;
            let mut saw_ready = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(250), harness.recv_update()).await
                {
                    Ok(CaptureUpdate::FrameDrop { .. }) => frame_drop_edges += 1,
                    Ok(CaptureUpdate::Ready { .. }) => saw_ready = true,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            assert!(saw_ready, "the bridge must have reached Ready");
            let evictions = harness.supervisor.frame_drops_total();
            assert!(
                evictions >= 100,
                "the flood must actually evict frames, got {evictions}"
            );
            assert!(
                frame_drop_edges <= 8,
                "a sustained run of {evictions} evictions must coalesce into a                  handful of edges, but produced {frame_drop_edges} controller wakeups"
            );

            harness.stop_and_drain().await;
        }
    }

    mod endpoint_change {
        use super::*;

        #[tokio::test]
        async fn endpoint_change_swaps_worker_with_new_ready_signal() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x51));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x52));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 1,
                    ..
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready {
                    capture_generation,
                    endpoint,
                    ..
                } => {
                    assert_eq!(capture_generation, 1);
                    assert_eq!(endpoint.id, "endpoint-51");
                }
                other => panic!("expected initial Ready, got {other:?}"),
            }

            harness
                .supervisor
                .request_control(CaptureControl::RequestEndpointChange);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 2,
                    ..
                }
            ));
            match harness.recv_lifecycle().await {
                CaptureUpdate::Ready {
                    capture_generation,
                    endpoint,
                    ..
                } => {
                    assert_eq!(capture_generation, 2);
                    assert_eq!(endpoint.id, "endpoint-52");
                }
                other => panic!("expected swapped Ready, got {other:?}"),
            }

            {
                let state = harness.supervisor.lock_state();
                assert_eq!(state.decoder_generation(), 1);
            }
            assert_eq!(harness.factory.count(), 1, "decoder not replaced");
            assert_eq!(harness.source.opens(), 2);

            harness.shutdown_and_join().await;
        }
    }

    mod stop {
        use super::*;

        #[tokio::test]
        async fn stop_cancels_within_budget() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x61));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started { .. }
            ));
            let child = harness
                .supervisor
                .lock_state()
                .child_token()
                .expect("attempt child token stored");

            let started_at = Instant::now();
            harness.supervisor.request_control(CaptureControl::Stop);
            assert!(
                matches!(
                    harness.recv_update().await,
                    CaptureUpdate::Stopped {
                        decoder_generation: 1
                    }
                ),
                "stop publishes the final Stopped edge"
            );
            assert!(
                harness.supervisor.updates_mut().recv().await.is_none(),
                "update channel closes after Stopped"
            );
            let elapsed = started_at.elapsed();

            assert!(child.is_cancelled(), "stop cancels the attempt token");
            assert!(
                elapsed < Duration::from_secs(2),
                "bounded teardown budget of two seconds exceeded: {elapsed:?}"
            );
            harness.shutdown_and_join().await;
        }
    }

    mod silence_fixture {
        use super::*;

        #[test]
        fn silence_frame_has_the_normalized_layout() {
            let frame = silence_frame();
            assert!(frame.samples.iter().all(|sample| *sample == 0));
            assert_eq!(
                frame.samples.len(),
                CAPTURE_SAMPLE_RATE as usize * CAPTURE_CHANNELS as usize / 20,
                "exactly 50 ms of stereo audio at the normalized rate"
            );
            assert_eq!(frame.sample_rate, CAPTURE_SAMPLE_RATE);
            assert_eq!(frame.channels, CAPTURE_CHANNELS);
        }
    }

    mod wasapi_endpoint_resolution {
        use super::*;

        struct GatedResolution {
            caller: thread::ThreadId,
            entered: tokio::sync::mpsc::UnboundedSender<()>,
            release: Arc<std::sync::atomic::AtomicBool>,
        }

        impl WasapiApi for GatedResolution {
            fn enumerate_render_endpoints(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
                Ok(Vec::new())
            }
            fn get_device_by_id(&self, _id: &str) -> Result<WasapiDeviceRef, CaptureError> {
                unreachable!()
            }
            fn get_default_output_device(&self) -> Result<WasapiDeviceRef, CaptureError> {
                if thread::current().id() == self.caller {
                    return Err(CaptureError::Endpoints(
                        "resolution blocked the async runtime".into(),
                    ));
                }
                let _ = self.entered.send(());
                while !self.release.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(WasapiDeviceRef {
                    id: "usb".into(),
                    name: "USB DAC".into(),
                })
            }
            fn open_loopback(
                &self,
                _device: &WasapiDeviceRef,
            ) -> Result<Box<dyn WasapiLoopbackStream>, CaptureError> {
                unreachable!()
            }
        }

        #[tokio::test]
        async fn cancelled_endpoint_resolution_stays_off_runtime_and_single_flight() {
            let (entered, mut calls) = tokio::sync::mpsc::unbounded_channel();
            let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
            struct Release(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for Release {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _guard = Release(release.clone());
            let source = Arc::new(WasapiCaptureSource::with_api(Arc::new(GatedResolution {
                caller: thread::current().id(),
                entered,
                release: release.clone(),
            })));
            let open = |source: Arc<WasapiCaptureSource>| {
                tokio::spawn(
                    async move { source.open(&AudioEndpointPreference::SystemDefault).await },
                )
            };
            let first = open(source.clone());
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), calls.recv())
                    .await
                    .unwrap(),
                Some(()),
                "resolution must run on its own thread"
            );
            first.abort();
            let _ = first.await;
            let waiting: Vec<_> = (0..8).map(|_| open(source.clone())).collect();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), calls.recv())
                    .await
                    .is_err(),
                "cancellation must not release a hung native call's slot"
            );
            release.store(true, Ordering::SeqCst);
            for pending in waiting {
                assert!(tokio::time::timeout(Duration::from_secs(2), pending)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_ok());
            }
        }

        /// RED anchor (plan Task 10 Step 1): explicit endpoint preferences
        /// resolve through the stable ID only and never fall back to the
        /// default render device.
        #[tokio::test]
        async fn explicit_endpoint_uses_id_and_never_falls_back_to_default() {
            // Missing ID: the error names the missing ID and the default
            // device is never queried.
            let api = FakeWasapiApi::with_default("default-out");
            let source = WasapiCaptureSource::with_api(Arc::new(api.clone()));
            let preference = AudioEndpointPreference::Explicit {
                id: "missing-id".into(),
                last_known_name: "Ghost DAC".into(),
            };
            let error = source.resolve(&preference).unwrap_err();
            assert!(
                error.to_string().contains("missing-id"),
                "error must name the missing id: {error}"
            );
            assert_eq!(api.requested_ids(), vec!["missing-id"]);
            assert_eq!(api.default_requests(), 0);

            // With the ID present, exactly that device resolves and opens;
            // the default device stays untouched.
            let explicit = || AudioEndpointPreference::Explicit {
                id: "chosen".into(),
                last_known_name: "USB DAC".into(),
            };
            let api = FakeWasapiApi::with_default("default-out").with_endpoint("chosen", "USB DAC");
            let source = WasapiCaptureSource::with_api(Arc::new(api.clone()));
            let resolved = source.resolve(&explicit()).unwrap();
            assert_eq!(resolved.id, "chosen");
            assert_eq!(resolved.name, "USB DAC");

            // Running the worker must open exactly the explicit device.
            let worker = source.open(&explicit()).await.unwrap();
            let (frames_tx, _frames_rx) = crossbeam_channel::bounded(4);
            let (ready_tx, ready_rx) = oneshot::channel();
            let cancel = CancellationToken::new();
            let worker_cancel = cancel.clone();
            let runner =
                tokio::task::spawn_blocking(move || worker.run(frames_tx, ready_tx, worker_cancel));
            let ready = tokio::time::timeout(Duration::from_secs(2), ready_rx)
                .await
                .expect("worker reports readiness")
                .expect("readiness oneshot stays open")
                .expect("explicit endpoint becomes ready");
            assert_eq!(ready.id, "chosen");
            assert_eq!(ready.name, "USB DAC");
            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(2), runner)
                .await
                .expect("cancel stops the worker promptly")
                .expect("worker thread joins")
                .expect("cancelled worker ends cleanly");
            assert!(
                api.requested_ids().iter().all(|id| id == "chosen"),
                "only the explicit ID may be queried: {:?}",
                api.requested_ids()
            );
            assert_eq!(api.default_requests(), 0, "no fallback to the default");
            assert_eq!(api.opened_ids(), vec!["chosen"]);
        }

        #[test]
        fn system_default_uses_default_render_device() {
            let api = FakeWasapiApi::with_default("speakers")
                .with_endpoint("speakers", "Speakers")
                .with_endpoint("other", "Other Out");
            let source = WasapiCaptureSource::with_api(Arc::new(api.clone()));

            let resolved = source
                .resolve(&AudioEndpointPreference::SystemDefault)
                .unwrap();
            assert_eq!(resolved.id, "speakers");
            assert_eq!(resolved.name, "Speakers");
            assert_eq!(api.default_requests(), 1);
            assert!(api.requested_ids().is_empty());
            assert_eq!(api.opened_ids(), Vec::<String>::new());
        }

        #[test]
        fn enumerated_endpoints_sort_case_insensitively_with_id_tiebreak() {
            let mut endpoints = vec![
                AudioEndpoint {
                    id: "b".into(),
                    name: "beta".into(),
                },
                AudioEndpoint {
                    id: "A".into(),
                    name: "Alpha".into(),
                },
                AudioEndpoint {
                    id: "a".into(),
                    name: "alpha".into(),
                },
            ];
            sort_endpoints(&mut endpoints);
            let ids: Vec<&str> = endpoints
                .iter()
                .map(|endpoint| endpoint.id.as_str())
                .collect();
            assert_eq!(ids, vec!["A", "a", "b"]);
        }
    }

    mod suspend_and_resume {
        use super::*;

        /// A closed control channel must read as "stop", not as "the last
        /// thing anyone said".
        ///
        /// `watch::Receiver::changed()` completes instantly once every sender
        /// is gone, so a resolver that swallows that error yields no await
        /// point at all. In the suspend park the last value is by definition
        /// `Suspend`, which the park maps to `continue` -- a yield-free loop
        /// with no exit, on the backend's current-thread runtime.
        #[tokio::test]
        async fn a_closed_control_channel_reads_as_stop() {
            let (control_tx, mut control_rx) = watch::channel(Some(CaptureControl::Suspend));
            drop(control_tx);

            assert_eq!(
                next_control(&mut control_rx).await,
                None,
                "a closed control channel reported a stale control edge"
            );
            assert_eq!(
                next_control(&mut control_rx).await,
                None,
                "the closed verdict must stay closed"
            );
            assert!(
                matches!(action_of(None), ControlAction::Stop),
                "a closed channel must classify as stop"
            );
        }

        /// The generation numbers on the suspend/resume edges are a diagnostic
        /// contract: `Suspended` names the generation that was actually
        /// cancelled, and the resume mints a number never handed out before.
        /// Reusing one would make a consumer correlating by generation file
        /// the discarded pre-suspend worker and the fresh post-resume worker
        /// under the same section.
        #[tokio::test]
        async fn suspend_names_the_cancelled_generation_and_resume_mints_a_fresh_one() {
            let source = FakeCaptureSource::new();
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0A));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0B));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    decoder_generation: 1,
                    capture_generation: 1
                }
            ));
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Ready {
                    capture_generation: 1,
                    ..
                }
            ));

            harness.supervisor.request_control(CaptureControl::Suspend);
            match harness.recv_lifecycle().await {
                CaptureUpdate::Suspended {
                    decoder_generation,
                    capture_generation,
                } => {
                    assert_eq!(decoder_generation, 1, "the decoder pair was replaced");
                    assert_eq!(
                        capture_generation, 1,
                        "`Suspended` named a generation other than the cancelled one"
                    );
                }
                other => panic!("expected a suspend edge, got {other:?}"),
            }

            harness.supervisor.request_control(CaptureControl::Resume);
            match harness.recv_lifecycle().await {
                CaptureUpdate::Started {
                    decoder_generation,
                    capture_generation,
                } => {
                    assert_eq!(decoder_generation, 1, "the decoder pair was replaced");
                    assert_eq!(
                        capture_generation, 2,
                        "the resume reused an already-spent capture generation"
                    );
                }
                other => panic!("expected a fresh capture generation, got {other:?}"),
            }
            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Ready {
                    capture_generation: 2,
                    ..
                }
            ));

            harness.shutdown_and_join().await;
        }

        /// A suspend that lands before the worker is even open costs exactly
        /// one generation too, and it is that generation the edge names.
        #[tokio::test]
        async fn a_suspend_during_the_open_still_names_its_own_generation() {
            let source = FakeCaptureSource::new();
            // Never reports readiness: the supervisor is still in the open
            // phase when the suspend arrives.
            source.push_open_ok(FakeWorkerPlan::ReadyNever, audio_endpoint(0x0A));
            source.push_open_ok(FakeWorkerPlan::HangUntilCancel, audio_endpoint(0x0B));
            let mut harness = CaptureHarness::start(source);

            assert!(matches!(
                harness.recv_lifecycle().await,
                CaptureUpdate::Started {
                    capture_generation: 1,
                    ..
                }
            ));

            harness.supervisor.request_control(CaptureControl::Suspend);
            match harness.recv_lifecycle().await {
                CaptureUpdate::Suspended {
                    capture_generation, ..
                } => assert_eq!(
                    capture_generation, 1,
                    "`Suspended` named a generation other than the cancelled one"
                ),
                other => panic!("expected a suspend edge, got {other:?}"),
            }

            harness.supervisor.request_control(CaptureControl::Resume);
            match harness.recv_lifecycle().await {
                CaptureUpdate::Started {
                    capture_generation, ..
                } => assert_eq!(
                    capture_generation, 2,
                    "the resume reused an already-spent capture generation"
                ),
                other => panic!("expected a fresh capture generation, got {other:?}"),
            }

            harness.shutdown_and_join().await;
        }
    }
}
