//! Main AirPlay client API.

use crate::connection::{Health, WorkerLedger};
use crate::diagnostics::{ClientDiagnosticsSource, ClientTimingRole};
use crate::events::{GroupRuntimeEvent, GroupRuntimeEventFeed};
use crate::group::{
    choose_primary, find_duplicate_member, GroupConnectReport, GroupLinkFactory, GroupMemberLink,
    GroupPrimaryTiming, MemberFailure, MemberResult, SetupPhase, MAX_PARALLEL_SETUPS,
    MEMBER_PROBE_TIMEOUT,
};
use crate::{ClientEvent, Connection, DeviceGroup, EventHandler, PlaybackState};
use airplay_audio::{
    AlacEncoder, AudioDecoder, AudioStreamer, EqConfig, EqParams, LiveAudioDecoder,
    LiveFrameSender, RetransmitRequest,
};
use airplay_core::error::{DiscoveryError, Error, RtspError};
use airplay_core::{
    error::Result, Device, DeviceId, SenderBufferCapacity, StreamConfig, TimingProtocol,
};
use airplay_discovery::{Discovery, ServiceBrowser};
use futures::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::net::UdpSocket;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Project a calibration profile keyed by stable [`DeviceId`] onto the
/// positional order the audio streamer fans packets out in.
///
/// `targets` is the sender order (primary first, then group members). Every
/// target needs its own entry: zero is a valid request, an absent one is not,
/// because a silently defaulted zero would move that speaker relative to the
/// rest without anyone asking for it. Entries for receivers that are not
/// connected are rejected as well — they are a sign that the profile and the
/// active selection disagree.
///
/// Sender indices exist only here; nothing above this adapter sees them.
fn map_presentation_delays_for(
    targets: &[DeviceId],
    effective_delay_ns: &BTreeMap<DeviceId, u64>,
) -> Result<Vec<u64>> {
    let mut delays = Vec::with_capacity(targets.len());
    let mut missing = 0usize;

    for receiver in targets {
        match effective_delay_ns.get(receiver) {
            Some(delay) => delays.push(*delay),
            None => {
                missing += 1;
                delays.push(0);
            }
        }
    }

    if missing > 0 {
        return Err(Error::Rtsp(RtspError::SetupFailed(format!(
            "calibration is missing an entry for {missing} of {} connected receiver(s)",
            targets.len()
        ))));
    }
    if effective_delay_ns.len() != targets.len() {
        return Err(Error::Rtsp(RtspError::SetupFailed(format!(
            "calibration carries {} entries for {} connected receiver(s)",
            effective_delay_ns.len(),
            targets.len()
        ))));
    }

    Ok(delays)
}

/// High-level AirPlay 2 sender client.
pub struct AirPlayClient {
    browser: ServiceBrowser,
    connection: Option<Connection>,
    /// Secondary connections for multi-speaker group streaming.
    group_connections: Vec<Connection>,
    group: Option<DeviceGroup>,
    event_handler: Option<Box<dyn EventHandler>>,
    stream_config: StreamConfig,
    /// Render delay in ms added to NTP timestamps for extra retransmit headroom.
    /// Default: 200ms for reliable playback over WiFi.
    render_delay_ms: u32,
    /// AudioStreamer for group streaming (shared across all group devices).
    group_streamer: Option<AudioStreamer>,
    /// Playback state for group streaming (tracked separately from primary connection).
    group_playback_state: Option<PlaybackState>,
    /// Stream statistics (shared across all streaming threads).
    stream_stats: Arc<crate::stats::StreamStats>,
    /// Member establishment seam (production wraps `Connection`).
    group_links: Arc<dyn GroupLinkFactory>,
    /// Per-member worker ledgers, retained across teardown (instrumentation).
    member_ledgers: HashMap<DeviceId, WorkerLedger>,
    /// Primary-critical attempt order of the last best-effort connect.
    primary_attempt_log: Vec<DeviceId>,
    /// Timing protocol selections per receiver of the last best-effort connect.
    timing_protocol_log: HashMap<DeviceId, Vec<TimingProtocol>>,
    /// Bounded fan-out failure event feed with truthful loss counting.
    runtime_events: GroupRuntimeEventFeed,
    /// Consumer half of the runtime-event feed (session adapter/tests take it).
    runtime_event_rx: Option<tokio::sync::mpsc::Receiver<GroupRuntimeEvent>>,
}

impl AirPlayClient {
    /// Create new client.
    pub fn new() -> Result<Self> {
        let (runtime_events, runtime_event_rx) =
            GroupRuntimeEventFeed::channel(crate::events::GROUP_RUNTIME_EVENT_CAPACITY);
        Ok(Self {
            browser: ServiceBrowser::new()?,
            connection: None,
            group_connections: Vec::new(),
            group: None,
            event_handler: None,
            stream_config: StreamConfig::default(),
            render_delay_ms: 200, // 200ms default for reliable playback over WiFi
            group_streamer: None,
            group_playback_state: None,
            stream_stats: crate::stats::StreamStats::new(),
            group_links: Arc::new(crate::group::RealLinkFactory),
            member_ledgers: HashMap::new(),
            primary_attempt_log: Vec::new(),
            timing_protocol_log: HashMap::new(),
            runtime_events,
            runtime_event_rx: Some(runtime_event_rx),
        })
    }

    /// Create client with specific configuration.
    pub fn with_config(
        config: StreamConfig,
        event_handler: Option<Box<dyn EventHandler>>,
    ) -> Result<Self> {
        let (runtime_events, runtime_event_rx) =
            GroupRuntimeEventFeed::channel(crate::events::GROUP_RUNTIME_EVENT_CAPACITY);
        Ok(Self {
            browser: ServiceBrowser::new()?,
            connection: None,
            group_connections: Vec::new(),
            group: None,
            event_handler,
            stream_config: config,
            render_delay_ms: 200, // 200ms default for reliable playback over WiFi
            group_streamer: None,
            group_playback_state: None,
            stream_stats: crate::stats::StreamStats::new(),
            group_links: Arc::new(crate::group::RealLinkFactory),
            member_ledgers: HashMap::new(),
            primary_attempt_log: Vec::new(),
            timing_protocol_log: HashMap::new(),
            runtime_events,
            runtime_event_rx: Some(runtime_event_rx),
        })
    }

    /// Set event handler.
    pub fn set_event_handler(&mut self, handler: impl EventHandler + 'static) {
        self.event_handler = Some(Box::new(handler));
    }

    /// Replace the member-establishment seam (offline test scripting).
    #[doc(hidden)]
    pub fn set_group_link_factory_for_test(&mut self, factory: Arc<dyn GroupLinkFactory>) {
        self.group_links = factory;
    }

    /// Primary-critical attempt order of the last best-effort connect.
    ///
    /// Records every receiver that was promoted to primary-critical setup,
    /// in order — one entry per attempt, at most two (initial + fallback).
    #[doc(hidden)]
    pub fn primary_attempts(&self) -> Vec<DeviceId> {
        self.primary_attempt_log.clone()
    }

    /// Timing protocol selection ledger keyed by stable receiver identity.
    ///
    /// `Ptp` is recorded when a member joins/roots the shared PTP domain;
    /// a lone survivor's reconnect through the single-receiver NTP path
    /// appends `Ntp` for that receiver.
    #[doc(hidden)]
    pub fn timing_protocol_ledger(&self) -> HashMap<DeviceId, Vec<TimingProtocol>> {
        self.timing_protocol_log.clone()
    }

    /// Worker ledger of one member's connection, retained across teardown.
    #[doc(hidden)]
    pub fn worker_ledger(&self, receiver: &DeviceId) -> Option<WorkerLedger> {
        self.member_ledgers.get(receiver).cloned()
    }

    /// Stored volume of one live member's connection (`None` if unknown).
    #[doc(hidden)]
    pub fn member_volume(&self, receiver: &DeviceId) -> Option<f32> {
        self.connection
            .as_ref()
            .filter(|conn| conn.device().id == *receiver)
            .map(|conn| conn.volume())
            .or_else(|| {
                self.group_connections
                    .iter()
                    .find(|conn| conn.device().id == *receiver)
                    .map(|conn| conn.volume())
            })
    }

    /// Render lead stored on one live member's connection (`None` if
    /// unknown). Only the single-receiver streaming path reads it; group
    /// members take their lead from the shared streamer instead.
    #[doc(hidden)]
    pub fn member_render_delay_ms(&self, receiver: &DeviceId) -> Option<u32> {
        self.connection
            .as_ref()
            .filter(|conn| conn.device().id == *receiver)
            .map(|conn| conn.render_delay_ms())
            .or_else(|| {
                self.group_connections
                    .iter()
                    .find(|conn| conn.device().id == *receiver)
                    .map(|conn| conn.render_delay_ms())
            })
    }

    /// Feed handle for the bounded runtime-event channel (loss counter incl.).
    #[doc(hidden)]
    pub fn runtime_event_feed(&self) -> GroupRuntimeEventFeed {
        self.runtime_events.clone()
    }

    /// Take the consumer half of the bounded runtime-event channel.
    #[doc(hidden)]
    pub fn take_runtime_event_receiver(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<GroupRuntimeEvent>> {
        self.runtime_event_rx.take()
    }

    /// Set render delay in milliseconds.
    ///
    /// Shifts NTP timestamps in sync packets into the future, telling the
    /// receiver to buffer audio longer before rendering. This gives more
    /// headroom for retransmit recovery of lost packets over lossy WiFi.
    ///
    /// Default is 200ms. Typical values: 100-500ms.
    /// Must be called before `connect()`.
    pub fn set_render_delay_ms(&mut self, delay_ms: u32) {
        self.render_delay_ms = delay_ms;
    }

    /// Sets the local sender buffer capacity for new streaming sessions.
    ///
    /// Call this before connecting. It does not retime streams that are
    /// already running and it does not alter RTSP latency parameters.
    pub fn set_sender_buffer_capacity(&mut self, capacity: SenderBufferCapacity) {
        self.stream_config.sender_buffer = capacity;
    }

    /// Emit an event if handler is set.
    async fn emit_event(&self, event: ClientEvent) {
        if let Some(ref handler) = self.event_handler {
            handler.on_event(event).await;
        }
    }

    /// Discover AirPlay devices on the network.
    pub async fn discover(&self, timeout: Duration) -> Result<Vec<Device>> {
        self.browser.scan(timeout).await
    }

    /// Get a specific device by ID.
    pub async fn get_device(&self, id: &DeviceId) -> Option<Device> {
        self.browser.get_device(id).await
    }

    /// Connect to a device.
    pub async fn connect(&mut self, device: &Device) -> Result<()> {
        // Disconnect existing connection if any
        if self.connection.is_some() {
            self.disconnect().await?;
        }

        // Use the user-provided stream config (don't override based on device features)
        let stream_config = self.stream_config.clone();

        // Establish connection
        let mut connection = Connection::connect(device.clone(), stream_config).await?;

        // Set render delay for retransmit headroom
        connection.set_render_delay_ms(self.render_delay_ms);

        // Complete RTSP SETUP handshake (CRITICAL - required before streaming)
        connection.setup().await?;

        self.connection = Some(connection);

        self.emit_event(ClientEvent::Connected(device.clone()))
            .await;

        Ok(())
    }

    /// Connect to a device with PIN (for password-protected devices).
    pub async fn connect_with_pin(&mut self, device: &Device, pin: &str) -> Result<()> {
        // Disconnect existing connection if any
        if self.connection.is_some() {
            self.disconnect().await?;
        }

        // Use the user-provided stream config (don't override based on device features)
        let stream_config = self.stream_config.clone();

        // Establish connection
        let mut connection =
            Connection::connect_with_pin(device.clone(), stream_config, pin).await?;

        // Set render delay for retransmit headroom
        connection.set_render_delay_ms(self.render_delay_ms);

        // Complete RTSP SETUP handshake (CRITICAL - required before streaming)
        connection.setup().await?;

        self.connection = Some(connection);

        self.emit_event(ClientEvent::Connected(device.clone()))
            .await;

        Ok(())
    }

    /// Disconnect from current device and any group connections.
    pub async fn disconnect(&mut self) -> Result<()> {
        // Stop group streamer if running
        if let Some(ref mut streamer) = self.group_streamer {
            let _ = streamer.stop().await;
        }
        self.group_streamer = None;

        // Disconnect group connections CONCURRENTLY. Each connection enforces
        // its own two-second TEARDOWN budget, so a sequential fan-out would
        // cost (N+1) x 2s and blow the four-second group teardown budget the
        // session supervisor enforces. Concurrent teardown keeps the whole
        // group inside one per-receiver budget.
        let group_teardowns: Vec<_> = self
            .group_connections
            .iter_mut()
            .map(|conn| conn.disconnect())
            .collect();
        for outcome in futures::future::join_all(group_teardowns).await {
            let _ = outcome;
        }
        self.group_connections.clear();
        self.group_playback_state = None;

        if let Some(ref mut connection) = self.connection {
            connection.disconnect().await?;
        }
        self.connection = None;

        self.emit_event(ClientEvent::Disconnected(None)).await;

        Ok(())
    }

    /// Check if connected.
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Get connected device.
    pub fn connected_device(&self) -> Option<&Device> {
        self.connection.as_ref().map(|c| c.device())
    }

    /// Play audio from file.
    pub async fn play_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        // Create decoder for the file
        let decoder = AudioDecoder::open(path)?;

        // Start streaming
        connection.start_streaming(decoder).await?;

        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(())
    }

    /// Play audio from raw PCM samples (one-shot).
    pub async fn play_pcm(
        &mut self,
        _samples: &[i16],
        _sample_rate: u32,
        _channels: u8,
    ) -> Result<()> {
        let _connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        // TODO: One-shot PCM streaming path not implemented yet
        // For streaming, use start_live_streaming() instead
        Err(Error::Streaming(airplay_core::error::StreamingError::InvalidFormat(
            "One-shot PCM streaming not implemented. Use start_live_streaming() for live sources.".into(),
        )))
    }

    /// Start live audio streaming from an external source (e.g., Bluetooth).
    ///
    /// Returns a `LiveFrameSender` that can be used to push PCM frames to the
    /// AirPlay stream. The stream will continue until stopped or the sender is dropped.
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate of the source audio in Hz (e.g., 44100)
    /// * `channels` - Number of audio channels (typically 2 for stereo)
    ///
    /// # Example
    /// ```ignore
    /// let sender = client.start_live_streaming(44100, 2).await?;
    ///
    /// // Push frames in a loop
    /// loop {
    ///     let frame = LivePcmFrame {
    ///         samples: captured_audio,
    ///         channels: 2,
    ///         sample_rate: 44100,
    ///     };
    ///     sender.try_send(frame);
    /// }
    /// ```
    pub async fn start_live_streaming(
        &mut self,
        sample_rate: u32,
        channels: u8,
    ) -> Result<LiveFrameSender> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        // Create live decoder and sender pair
        // Capacity of 16 frames provides ~350ms buffer at 352 frames/packet, 44.1kHz
        let (sender, decoder) = LiveAudioDecoder::create_pair(sample_rate, channels, 16);

        // Start live streaming
        connection.start_streaming_live(decoder).await?;

        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(sender)
    }

    /// Start live audio streaming with an existing decoder.
    ///
    /// This allows the caller to create the sender/decoder pair first, pre-fill
    /// the channel with audio data, and then start streaming. This avoids startup
    /// artifacts from empty buffers.
    ///
    /// # Example
    /// ```ignore
    /// // Create sender/decoder pair with larger buffer
    /// let (sender, decoder) = LiveAudioDecoder::create_pair(44100, 2, 64);
    ///
    /// // Start capture thread that sends frames to sender
    /// std::thread::spawn(move || {
    ///     loop { sender.try_send(frame); }
    /// });
    ///
    /// // Wait for channel to fill
    /// std::thread::sleep(Duration::from_millis(500));
    ///
    /// // Now start streaming with pre-filled decoder
    /// client.start_live_streaming_with_decoder(decoder).await?;
    /// ```
    pub async fn start_live_streaming_with_decoder(
        &mut self,
        decoder: LiveAudioDecoder,
    ) -> Result<()> {
        self.start_live_streaming_with_decoder_gated(decoder, None)
            .await
    }

    /// Completes single-receiver preparation while holding first audio until release.
    pub async fn start_live_streaming_with_decoder_gated(
        &mut self,
        decoder: LiveAudioDecoder,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        // Start live streaming with the provided decoder
        connection
            .start_streaming_live_gated(decoder, release)
            .await?;

        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(())
    }

    /// Pause playback.
    pub async fn pause(&mut self) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.pause().await?;

        // Flush group connections too
        for conn in &mut self.group_connections {
            let _ = conn.send_flush(0, 0).await;
        }

        if self.group_playback_state.is_some() {
            self.group_playback_state = Some(PlaybackState::Paused);
        }
        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Paused))
            .await;

        Ok(())
    }

    /// Resume playback.
    pub async fn resume(&mut self) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.resume().await?;

        // Resume group connections too
        for conn in &mut self.group_connections {
            let _ = conn.send_record().await;
        }

        if self.group_playback_state.is_some() {
            self.group_playback_state = Some(PlaybackState::Playing);
        }
        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(())
    }

    /// Stop playback.
    pub async fn stop(&mut self) -> Result<()> {
        // Stop group streamer first
        if let Some(ref mut streamer) = self.group_streamer {
            let _ = streamer.stop().await;
        }
        self.group_streamer = None;

        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.stop().await?;

        // Flush group connections too
        for conn in &mut self.group_connections {
            let _ = conn.send_flush(0, 0).await;
        }

        self.group_playback_state = None;
        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Stopped))
            .await;

        Ok(())
    }

    /// Seek to position in seconds.
    pub async fn seek(&mut self, position_secs: f64) -> Result<()> {
        let _connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        // Seeking requires buffer manipulation and timestamp coordination
        // For now, this is a stub - full implementation would need:
        // 1. Flush current buffer
        // 2. Seek decoder to position
        // 3. Refill buffer
        // 4. Resume playback

        self.emit_event(ClientEvent::PositionUpdated(position_secs))
            .await;

        Ok(())
    }

    /// Set volume (0.0 to 1.0).
    pub async fn set_volume(&mut self, volume: f32) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.set_volume(volume).await?;

        // Set volume on group connections too
        for conn in &mut self.group_connections {
            let _ = conn.set_volume(volume).await;
        }

        self.emit_event(ClientEvent::VolumeChanged(volume)).await;

        Ok(())
    }

    /// Send feedback/keepalive to the receiver.
    ///
    /// **IMPORTANT:** AirPlay 2 receivers expect periodic feedback requests (~every 2 seconds)
    /// during active playback. Call this from your main loop to maintain the session and prevent
    /// timeouts.
    ///
    /// # Example
    /// ```ignore
    /// // In your playback loop:
    /// loop {
    ///     tokio::time::sleep(Duration::from_secs(2)).await;
    ///     if let Err(e) = client.send_feedback().await {
    ///         eprintln!("Feedback failed: {}", e);
    ///     }
    /// }
    /// ```
    pub async fn send_feedback(&mut self) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.send_feedback().await?;

        // Also send feedback to group connections to prevent session timeouts
        for conn in &mut self.group_connections {
            let _ = conn.send_feedback().await;
        }

        Ok(())
    }

    /// Set up the equalizer with shared parameters.
    ///
    /// The EQ will be applied to audio during streaming. Parameters can be
    /// updated atomically from another thread (e.g., the UI).
    ///
    /// Must be called after `connect()` and before `play_file()` or `start_live_streaming()`.
    ///
    /// # Example
    /// ```ignore
    /// let config = EqConfig::five_band();
    /// let params = Arc::new(EqParams::new(config.num_bands()));
    ///
    /// // Set bass boost
    /// params.set_gain_db(0, 6.0);
    ///
    /// client.set_eq_params(config, params.clone()).await?;
    /// client.play_file("song.mp3").await?;
    ///
    /// // Adjust EQ during playback
    /// params.set_gain_db(4, -3.0);  // Reduce treble
    /// ```
    pub fn set_eq_params(&mut self, config: EqConfig, params: Arc<EqParams>) -> Result<()> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;

        connection.set_eq_params(config, params);
        Ok(())
    }

    /// Get a clone of the EQ params Arc if set.
    pub fn eq_params(&self) -> Option<Arc<EqParams>> {
        self.connection.as_ref().and_then(|c| c.eq_params())
    }

    /// Get current playback state.
    pub fn playback_state(&self) -> PlaybackState {
        // Group playback state takes priority when set
        if let Some(state) = self.group_playback_state {
            return state;
        }
        self.connection
            .as_ref()
            .map(|c| c.playback_state())
            .unwrap_or(PlaybackState::Stopped)
    }

    /// Get current playback position in seconds.
    pub fn playback_position(&self) -> f64 {
        self.connection
            .as_ref()
            .map(|c| c.playback_position())
            .unwrap_or(0.0)
    }

    /// Wait for playback to complete.
    pub async fn wait_for_completion(&self) -> Result<()> {
        // Poll playback state until stopped
        loop {
            let state = self.playback_state();
            match state {
                PlaybackState::Stopped | PlaybackState::Error => break,
                _ => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        Ok(())
    }

    // Multi-speaker group streaming methods

    /// Connect to multiple devices for synchronized group playback.
    ///
    /// Legacy all-or-nothing adapter: delegates to
    /// [`AirPlayClient::connect_group_best_effort`] and succeeds only when
    /// every requested member reached the live session.
    pub async fn connect_group(&mut self, devices: &[Device]) -> Result<()> {
        if devices.len() < 2 {
            return Err(Error::Discovery(DiscoveryError::NoDevicesFound));
        }
        let report = self.connect_group_best_effort(devices, None).await?;
        if report.connected.len() == devices.len() {
            Ok(())
        } else {
            Err(Error::Rtsp(RtspError::SetupFailed(format!(
                "{} group member(s) failed",
                report.failures.len()
            ))))
        }
    }

    /// Connect a group best effort with deterministic primary selection.
    ///
    /// Every member is connected and paired independently (bounded at
    /// [`MAX_PARALLEL_SETUPS`] concurrent establishments); one member's
    /// failure never aborts the others. The primary is the preferred device
    /// when it is healthy and PTP-capable, otherwise the lexicographically
    /// smallest healthy PTP-capable stable ID — never derived from input
    /// order. Duplicate member IDs are rejected upfront.
    ///
    /// When a chosen primary fails a primary-critical phase it is torn down
    /// member-locally and the deterministic rule runs ONCE more over the
    /// remaining healthy candidates; a second primary-critical failure ends
    /// the attempt without a third try. A lone survivor leaves the partial
    /// PTP topology and reconnects through the single-receiver NTP path.
    ///
    /// The returned report lists the live session (primary first) and every
    /// phase-classified failure. When no primary can be established all
    /// members are torn down and `report.primary` is `None`.
    pub async fn connect_group_best_effort(
        &mut self,
        devices: &[Device],
        preferred_primary: Option<&DeviceId>,
    ) -> Result<GroupConnectReport> {
        if devices.is_empty() {
            return Err(Error::Discovery(DiscoveryError::NoDevicesFound));
        }
        if let Some(duplicate) = find_duplicate_member(devices) {
            return Err(Error::Discovery(DiscoveryError::Resolution(format!(
                "duplicate group member {}",
                duplicate.to_mac_string()
            ))));
        }

        // Announced restart semantics: any existing session ends first.
        self.disconnect().await?;

        // Fresh instrumentation for this attempt.
        self.member_ledgers.clear();
        self.primary_attempt_log.clear();
        self.timing_protocol_log.clear();

        // Group streaming shares one PTP clock (primary runs BMCA yield).
        let mut config = self.stream_config.clone();
        config.timing_protocol = TimingProtocol::Ptp;
        config.ptp_mode = airplay_core::PtpMode::Master;

        // Generate ALAC magic cookie if needed
        if config.audio_format.codec == airplay_core::AudioCodec::Alac && config.asc.is_none() {
            let temp_encoder = AlacEncoder::new(config.audio_format.clone()).map_err(|e| {
                Error::Streaming(airplay_core::error::StreamingError::Encoding(format!(
                    "Failed to create encoder for magic cookie: {}",
                    e
                )))
            })?;
            config.asc = Some(temp_encoder.magic_cookie());
        }
        let render_delay_ms = self.render_delay_ms;

        // Phase Connect/Pair: establish every candidate independently with
        // bounded parallelism; one member's failure never aborts the others.
        // Results are re-sorted into deterministic input order afterwards.
        let factory = Arc::clone(&self.group_links);
        let establishment_config = config.clone();
        let mut established_results = futures::stream::iter(devices.iter().cloned().enumerate())
            .map(move |(index, device)| {
                let factory = Arc::clone(&factory);
                let config = establishment_config.clone();
                async move {
                    let result = factory.connect_member(&device, &config).await;
                    (index, device, result)
                }
            })
            .buffer_unordered(MAX_PARALLEL_SETUPS)
            .collect::<Vec<_>>()
            .await;
        established_results.sort_by_key(|(index, _, _)| *index);

        let mut failures: Vec<MemberFailure> = Vec::new();
        let mut members: Vec<Option<EstablishedMember>> = Vec::with_capacity(devices.len());
        for (_, device, result) in established_results {
            match result {
                Ok(link) => {
                    self.member_ledgers
                        .insert(device.id.clone(), link.worker_ledger());
                    members.push(Some(EstablishedMember {
                        device,
                        healthy: true,
                        link: Some(link),
                    }));
                }
                Err(failure) => {
                    failures.push(failure);
                    members.push(None);
                }
            }
        }

        // Deliberately NO liveness probe here. `POST <session-uri>/feedback`
        // is the step-6 keepalive of the baseline request flow
        // (AIRPLAY_2_SPEC.md 4.2); before SETUP the receiver has never been
        // told the session UUID that the probe URI names, so its answer says
        // nothing about the receiver's health -- and the spec requires senders
        // to tolerate 4xx outright (4.3). Establishment has already proven
        // liveness three times over (GET /info, the pairing handshake, and an
        // encrypted OPTIONS), and the primary-critical run below is the real
        // eligibility test: a member that cannot serve the role fails SETUP
        // there, is torn down member-locally, and the deterministic rule
        // promotes the next candidate.
        //
        // Primary-critical establishment with EXACTLY ONE deterministic
        // fallback: a failed candidate is torn down member-locally and the
        // lowest remaining healthy PTP-capable stable ID takes over once.
        let mut preferred_opt = preferred_primary.cloned();
        let mut peer_addresses: Vec<String> = Vec::new();
        let mut primary: Option<(usize, GroupPrimaryTiming)> = None;
        for _primary_attempt in 0..=1usize {
            let candidates: Vec<Device> = members
                .iter()
                .filter_map(|slot| slot.as_ref())
                .filter(|member| member.healthy && member.link.is_some())
                .map(|member| member.device.clone())
                .collect();
            let Some(primary_device) = choose_primary(&candidates, preferred_opt.as_ref()).cloned()
            else {
                break;
            };
            let index = members
                .iter()
                .position(|slot| {
                    slot.as_ref()
                        .is_some_and(|m| m.device.id == primary_device.id)
                })
                .expect("chosen primary is an established member");
            self.primary_attempt_log.push(primary_device.id.clone());

            // SETPEERS describes the timing domain, so it must name the
            // members that hold a live link -- never the requested membership,
            // which still contains everyone who failed to establish.
            let established: Vec<Device> = members
                .iter()
                .filter_map(|slot| slot.as_ref())
                .filter(|member| member.link.is_some())
                .map(|member| member.device.clone())
                .collect();
            peer_addresses = build_peer_addresses(
                &established,
                members[index]
                    .as_ref()
                    .and_then(|m| m.link.as_ref())
                    .and_then(|link| link.local_peer_address()),
            );

            let link = members[index]
                .as_mut()
                .and_then(|m| m.link.as_mut())
                .expect("chosen primary owns its link");
            match run_primary_critical(link, &peer_addresses).await {
                Ok(timing) => {
                    self.timing_protocol_log
                        .entry(primary_device.id.clone())
                        .or_default()
                        .push(TimingProtocol::Ptp);
                    primary = Some((index, timing));
                    break;
                }
                Err((phase, source)) => {
                    failures.push(member_failure(primary_device.id.clone(), phase, source));
                    // Member-local teardown: only the failed candidate leaves;
                    // everyone else stays established for the fallback round.
                    let member = members[index].as_mut().expect("primary slot");
                    if let Some(mut link) = member.link.take() {
                        link.teardown().await;
                    }
                    member.healthy = false;
                    // The fallback ignores the original preference and applies
                    // the deterministic lowest-stable-ID rule instead.
                    preferred_opt = None;
                }
            }
        }

        let Some((primary_index, timing)) = primary else {
            // No primary could be established (or none was eligible): tear
            // down every established member and report without a session.
            for slot in members.iter_mut() {
                if let Some(member) = slot.as_mut() {
                    if let Some(mut link) = member.link.take() {
                        link.teardown().await;
                    }
                }
            }
            return Ok(GroupConnectReport {
                primary: None,
                connected: Vec::new(),
                failures,
            });
        };

        // Member-local phases: secondaries join the primary's clock domain;
        // one failure tears down only that receiver.
        let mut survivors: Vec<usize> = Vec::new();
        for (index, slot) in members.iter_mut().enumerate() {
            if index == primary_index {
                continue;
            }
            let Some(member) = slot.as_mut() else {
                continue;
            };
            let Some(link) = member.link.as_mut() else {
                continue;
            };
            if let Err(source) = link.join_group_timing(&timing, render_delay_ms).await {
                failures.push(teardown_and_fail(link, SetupPhase::RtspSetup, source).await);
                continue;
            }
            if let Err(source) = link.send_setpeers(&peer_addresses).await {
                failures.push(teardown_and_fail(link, SetupPhase::SetPeers, source).await);
                continue;
            }
            self.timing_protocol_log
                .entry(member.device.id.clone())
                .or_default()
                .push(TimingProtocol::Ptp);
            survivors.push(index);
        }

        // SINGLE SURVIVOR: with nobody left to share the PTP domain, tear
        // down the partial group attempt and reconnect through the single-
        // receiver NTP path.
        if survivors.is_empty() {
            let survivor = members[primary_index].take().expect("primary slot");
            return self
                .finish_single_survivor_session(survivor, config, failures)
                .await;
        }

        // Store the live session: primary as main connection, rest as group.
        let primary_device = members[primary_index]
            .as_ref()
            .expect("primary slot")
            .device
            .clone();
        let survivor_devices: Vec<Device> = survivors
            .iter()
            .map(|&index| {
                members[index]
                    .as_ref()
                    .expect("survivor slot")
                    .device
                    .clone()
            })
            .collect();
        let mut connections: Vec<Option<Connection>> = members
            .into_iter()
            .map(|slot| {
                slot.and_then(|mut member| member.link.take().map(|link| link.into_connection()))
            })
            .collect();
        let primary_connection = connections[primary_index]
            .take()
            .expect("primary connection");
        let secondary_connections: Vec<Connection> = survivors
            .iter()
            .map(|&index| connections[index].take().expect("survivor connection"))
            .collect();
        self.connection = Some(primary_connection);
        self.group_connections = secondary_connections;

        // Diagnostics: the primary runs BMCA-yield (PtpPrimary); sender/target
        // indices follow construction order (primary 0, members 1..n).
        if let Some(ref mut conn) = self.connection {
            conn.diag_entry().set_timing_role(
                ClientTimingRole::PtpPrimary,
                airplay_timing::diagnostics::TimingReferenceKind::PtpMeasured,
            );
        }
        for (index, conn) in self
            .connection
            .iter_mut()
            .chain(self.group_connections.iter_mut())
            .enumerate()
        {
            conn.diag_entry().set_sender_target_index(index);
            conn.publish_diag_state();
        }

        // Group metadata reflects only the surviving membership.
        let mut group = DeviceGroup::new(primary_device.clone());
        for device in &survivor_devices {
            group.add_member(device.clone())?;
        }
        self.group = Some(group);

        self.emit_event(ClientEvent::Connected(primary_device.clone()))
            .await;
        self.emit_event(ClientEvent::GroupChanged).await;

        let mut connected = vec![primary_device.id.clone()];
        connected.extend(survivor_devices.iter().map(|device| device.id.clone()));
        Ok(GroupConnectReport {
            primary: Some(primary_device.id),
            connected,
            failures,
        })
    }

    /// Reconnect a lone surviving member through the single-receiver NTP path.
    ///
    /// Consumes the partial PTP-era state: the survivor's group link is torn
    /// down and re-established via the injected factory with an NTP stream
    /// config, mirroring `connect()`; the timing ledger records `Ntp` for it.
    async fn finish_single_survivor_session(
        &mut self,
        survivor: EstablishedMember,
        mut config: StreamConfig,
        mut failures: Vec<MemberFailure>,
    ) -> Result<GroupConnectReport> {
        let primary_device = survivor.device;
        if let Some(mut link) = survivor.link {
            link.teardown().await;
        }

        config.timing_protocol = TimingProtocol::Ntp;
        match self
            .group_links
            .connect_member(&primary_device, &config)
            .await
        {
            Ok(mut link) => {
                // Same order as `connect()`: the render lead is applied to
                // the fresh connection before SETUP, so the single-receiver
                // streaming path -- the only one that reads it off the
                // connection -- streams with the configured headroom.
                link.set_render_delay_ms(self.render_delay_ms);
                match link.setup_stream().await {
                    Ok(()) => {
                        let receiver = link.receiver();
                        self.timing_protocol_log
                            .entry(receiver.clone())
                            .or_default()
                            .push(TimingProtocol::Ntp);
                        self.member_ledgers.insert(receiver, link.worker_ledger());
                        let conn = link.into_connection();
                        conn.diag_entry().set_sender_target_index(0);
                        conn.publish_diag_state();
                        self.connection = Some(conn);
                        self.group_connections.clear();
                        self.group = Some(DeviceGroup::new(primary_device.clone()));

                        self.emit_event(ClientEvent::Connected(primary_device.clone()))
                            .await;
                        self.emit_event(ClientEvent::GroupChanged).await;

                        let primary_id = primary_device.id.clone();
                        return Ok(GroupConnectReport {
                            primary: Some(primary_id.clone()),
                            connected: vec![primary_id],
                            failures,
                        });
                    }
                    Err(source) => {
                        failures.push(
                            teardown_and_fail(&mut link, SetupPhase::RtspSetup, source).await,
                        );
                    }
                }
            }
            Err(failure) => failures.push(failure),
        }

        Ok(GroupConnectReport {
            primary: None,
            connected: Vec::new(),
            failures,
        })
    }

    /// Probe every live group member's health independently.
    ///
    /// Returns results keyed by stable receiver identity: `Ok(Health)` on an
    /// answered probe (healthy or timed out), `Err(MemberFailure)` on hard
    /// transport/protocol errors so callers can classify them separately.
    pub async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
        // Independent means concurrent: every member owns its RTSP socket, so
        // one silent receiver must cost the cycle its own timeout, not
        // `members * timeout`. Probing sequentially would make the health
        // cadence -- and with it the ten-second degraded budget -- depend on
        // how many members are currently unresponsive.
        let mut probes = Vec::new();
        if let Some(ref mut conn) = self.connection {
            probes.push(probe_one(conn));
        }
        for conn in self.group_connections.iter_mut() {
            probes.push(probe_one(conn));
        }
        futures::future::join_all(probes).await
    }

    /// Set volume on one specific group member (receiver-addressable).
    ///
    /// Never routes through the primary connection; unknown receivers fail
    /// with a phase-classified [`MemberFailure`] instead of affecting others.
    pub async fn set_member_volume(
        &mut self,
        receiver: &DeviceId,
        volume: f32,
    ) -> MemberResult<()> {
        let target = self
            .connection
            .as_mut()
            .filter(|conn| conn.device().id == *receiver)
            .or_else(|| {
                self.group_connections
                    .iter_mut()
                    .find(|conn| conn.device().id == *receiver)
            });
        let result = match target {
            Some(conn) => conn
                .set_volume(volume)
                .await
                .map_err(|source| crate::group::classify_runtime_failure(receiver.clone(), source)),
            None => Err(crate::group::classify_runtime_failure(
                receiver.clone(),
                Error::Rtsp(RtspError::NoSession),
            )),
        };
        MemberResult {
            receiver: receiver.clone(),
            result,
        }
    }

    /// Play an audio file to all group devices simultaneously.
    ///
    /// Requires `connect_group()` to have been called first. Uses the same
    /// AudioStreamer as single-device playback, getting RT priority, burst
    /// sending, precise timing, buffer management, EQ, and proper retransmit
    /// handling for free.
    pub async fn play_file_to_group(&mut self, path: impl AsRef<Path>) -> Result<()> {
        // Stop existing group streamer
        if let Some(ref mut streamer) = self.group_streamer {
            let _ = streamer.stop().await;
        }
        self.group_streamer = None;

        // FLUSH all connections to reset sequence numbers.
        // Do NOT send RECORD here — it was already sent during setup()/setup_for_group().
        // Sending a duplicate RECORD causes 500 Internal Server Error on some devices.
        if let Some(ref mut conn) = self.connection {
            let _ = conn.send_flush(0, 0).await;
        }
        for conn in &mut self.group_connections {
            let _ = conn.send_flush(0, 0).await;
        }

        // Build RTP senders for all connections
        let mut senders = Vec::new();
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;
        senders.push(connection.build_rtp_sender()?);
        for conn in &self.group_connections {
            senders.push(conn.build_rtp_sender()?);
        }

        let ptp_clock_rx = connection.ptp_clock_rx().ok_or_else(|| {
            RtspError::SetupFailed("No coherent PTP clock state for group streaming".into())
        })?;

        // Create streamer with multi-target senders
        let mut streamer = AudioStreamer::new(self.stream_config.clone());
        streamer.set_rtp_senders(senders).await;

        // Configure timing from one complete clock generation.
        streamer.set_ptp_clock_updates(ptp_clock_rx).await;
        if self.render_delay_ms > 0 {
            streamer.set_render_delay_ms(self.render_delay_ms).await;
        }

        // Set up EQ if configured on primary connection
        if let Some(ref conn) = self.connection {
            if let (Some(config), Some(params)) = (conn.eq_config(), conn.eq_params()) {
                streamer.set_eq_params(config, params).await;
            }
        }

        // Open audio file and start streaming
        let decoder = AudioDecoder::open(path)?;
        streamer.start(decoder).await?;

        // Create per-device stream stats (1 primary + N group connections)
        let device_count = 1 + self.group_connections.len();
        self.stream_stats = crate::stats::StreamStats::with_device_count(device_count);

        // Spawn control channel listener for retransmit handling (all devices)
        self.spawn_group_control_listener(&streamer);

        self.group_streamer = Some(streamer);
        self.group_playback_state = Some(PlaybackState::Playing);
        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(())
    }

    /// Start live audio streaming to all group devices simultaneously.
    ///
    /// Requires `connect_group()` to have been called first. Uses the same
    /// AudioStreamer as single-device playback, getting all optimizations for free.
    pub async fn start_live_streaming_to_group(&mut self, decoder: LiveAudioDecoder) -> Result<()> {
        let delays = vec![0u64; self.group_target_count()];
        self.start_live_streaming_to_group_inner(decoder, delays, None)
            .await
    }

    /// Start live group streaming with a per-receiver presentation delay.
    ///
    /// `effective_delay_ns` carries the already normalized, non-negative
    /// delays keyed by stable [`DeviceId`] — the earliest speaker holds zero
    /// and every other one is delayed relative to it. It must contain exactly
    /// one entry per connected target; a zero entry is a valid request, a
    /// missing or unknown one is rejected rather than silently defaulted.
    ///
    /// The delay reaches only each target's sync-packet presentation clock.
    /// The shared RTP stream — timestamps, payload, marker, PTP identity — is
    /// byte-for-byte the same as without calibration, so group
    /// synchronization of the stream itself cannot drift apart.
    ///
    /// This is session setup, not a live control: reconnecting or restarting
    /// rebuilds the vector from the persisted profile.
    pub async fn start_live_streaming_to_group_with_presentation_delays(
        &mut self,
        decoder: LiveAudioDecoder,
        effective_delay_ns: &BTreeMap<DeviceId, u64>,
    ) -> Result<()> {
        let delays = self.map_presentation_delays(effective_delay_ns)?;
        self.start_live_streaming_to_group_inner(decoder, delays, None)
            .await
    }

    /// Completes group preparation while holding first audio until release.
    pub async fn prepare_live_group_with_presentation_delays(
        &mut self,
        decoder: LiveAudioDecoder,
        effective_delay_ns: &BTreeMap<DeviceId, u64>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let delays = self.map_presentation_delays(effective_delay_ns)?;
        self.start_live_streaming_to_group_inner(decoder, delays, Some(release))
            .await
    }

    /// Number of RTP targets a group stream fans out to (primary + members).
    fn group_target_count(&self) -> usize {
        usize::from(self.connection.is_some()) + self.group_connections.len()
    }

    /// Map stable receiver identities onto the sender-index order the audio
    /// streamer uses (primary first, then group members in connect order).
    ///
    /// Sender indices never leave this adapter; everything above it is keyed
    /// by [`DeviceId`].
    fn map_presentation_delays(
        &self,
        effective_delay_ns: &BTreeMap<DeviceId, u64>,
    ) -> Result<Vec<u64>> {
        let targets: Vec<DeviceId> = self
            .connection
            .iter()
            .chain(self.group_connections.iter())
            .map(|conn| conn.device().id.clone())
            .collect();
        map_presentation_delays_for(&targets, effective_delay_ns)
    }

    async fn start_live_streaming_to_group_inner(
        &mut self,
        decoder: LiveAudioDecoder,
        presentation_delays_ns: Vec<u64>,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Result<()> {
        // Stop existing group streamer
        if let Some(ref mut streamer) = self.group_streamer {
            let _ = streamer.stop().await;
        }
        self.group_streamer = None;

        // FLUSH all connections to reset sequence numbers.
        // Do NOT send RECORD here — it was already sent during setup()/setup_for_group().
        // Sending a duplicate RECORD causes 500 Internal Server Error on some devices.
        if let Some(ref mut conn) = self.connection {
            let _ = conn.send_flush(0, 0).await;
        }
        for conn in &mut self.group_connections {
            let _ = conn.send_flush(0, 0).await;
        }

        // Build RTP senders for all connections
        let mut senders = Vec::new();
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| Error::Rtsp(RtspError::NoSession))?;
        senders.push(connection.build_rtp_sender()?);
        for conn in &self.group_connections {
            senders.push(conn.build_rtp_sender()?);
        }
        // After FLUSH every target owes the receiver a first sync carrying the
        // extension bit, and that state lives per target.
        for sender in &mut senders {
            sender.reset_sync_state();
        }
        if senders.len() != presentation_delays_ns.len() {
            return Err(Error::Rtsp(RtspError::SetupFailed(format!(
                "{} presentation delay(s) for {} RTP target(s)",
                presentation_delays_ns.len(),
                senders.len()
            ))));
        }

        let ptp_clock_rx = connection.ptp_clock_rx().ok_or_else(|| {
            RtspError::SetupFailed("No coherent PTP clock state for group streaming".into())
        })?;

        // Create streamer with multi-target senders
        let mut streamer = AudioStreamer::new(self.stream_config.clone());
        streamer.set_rtp_senders(senders).await;
        // Set before streaming begins: calibration is restart-only.
        streamer
            .set_target_presentation_delays_ns(presentation_delays_ns)
            .await?;

        // Configure timing from one complete clock generation.
        streamer.set_ptp_clock_updates(ptp_clock_rx).await;
        if self.render_delay_ms > 0 {
            streamer.set_render_delay_ms(self.render_delay_ms).await;
        }

        // Set up EQ if configured on primary connection
        if let Some(ref conn) = self.connection {
            if let (Some(config), Some(params)) = (conn.eq_config(), conn.eq_params()) {
                streamer.set_eq_params(config, params).await;
            }
        }

        // Start live streaming
        streamer.start_live_gated(decoder, release).await?;

        // Create per-device stream stats (1 primary + N group connections)
        let device_count = 1 + self.group_connections.len();
        self.stream_stats = crate::stats::StreamStats::with_device_count(device_count);

        // Spawn control channel listener for retransmit handling (all devices)
        self.spawn_group_control_listener(&streamer);

        self.group_streamer = Some(streamer);
        self.group_playback_state = Some(PlaybackState::Playing);
        self.emit_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
            .await;

        Ok(())
    }

    /// Spawn a single control channel listener thread that polls ALL device
    /// control sockets in round-robin for retransmit requests (PT=85).
    fn spawn_group_control_listener(&self, streamer: &AudioStreamer) {
        let mut control_sockets: Vec<(usize, UdpSocket)> = Vec::new();
        // Per-target diagnostics entries (same order as control_sockets).
        let mut diag_entries: Vec<Option<Arc<crate::diagnostics::EntryShared>>> = Vec::new();
        // Receiver identity per control socket index (fan-out failure events).
        let mut control_receivers: Vec<(usize, DeviceId)> = Vec::new();

        // Primary connection
        if let Some(ref conn) = self.connection {
            diag_entries.push(Some(conn.diag_entry()));
            control_receivers.push((0, conn.device().id.clone()));
            if let Some(sock) = conn.clone_control_socket_for_recv() {
                control_sockets.push((0, sock));
            }
        }

        // Group connections
        for (i, conn) in self.group_connections.iter().enumerate() {
            diag_entries.push(Some(conn.diag_entry()));
            control_receivers.push((i + 1, conn.device().id.clone()));
            if let Some(sock) = conn.clone_control_socket_for_recv() {
                control_sockets.push((i + 1, sock));
            }
        }

        if control_sockets.is_empty() {
            return;
        }

        let streamer_clone = streamer.clone();
        let rt_handle = tokio::runtime::Handle::current();
        let stats = Arc::clone(&self.stream_stats);
        let listener_entries = diag_entries;
        let event_feed = self.runtime_events.clone();
        let event_receivers = control_receivers;

        std::thread::Builder::new()
            .name("group-ctrl".into())
            .spawn(move || {
                for (_, sock) in &control_sockets {
                    sock.set_read_timeout(Some(Duration::from_millis(1))).ok();
                }
                let mut buf = [0u8; 2048];
                tracing::debug!("Group control listener started ({} sockets)", control_sockets.len());

                loop {
                    for &(device_index, ref sock) in &control_sockets {
                        match sock.recv_from(&mut buf) {
                            Ok((len, _)) => {
                                if len < 4 { continue; }
                                let payload_type = buf[1] & 0x7F;
                                if payload_type == 85 {
                                    let request = if len == 8 {
                                        let first_seq = u16::from_be_bytes([buf[4], buf[5]]);
                                        let count = u16::from_be_bytes([buf[6], buf[7]]);
                                        Some(RetransmitRequest { first_sequence: first_seq, count })
                                    } else if len >= 12 {
                                        RetransmitRequest::parse(&buf[..len]).ok()
                                    } else {
                                        None
                                    };
                                    if let Some(req) = request {
                                        // Update aggregate stats
                                        stats.rtx_requested.fetch_add(req.count as u64, Ordering::Relaxed);
                                        // Update per-device stats
                                        if let Some(dev) = stats.device(device_index) {
                                            dev.rtx_requested.fetch_add(req.count as u64, Ordering::Relaxed);
                                        }
                                        match rt_handle.block_on(
                                            streamer_clone.handle_retransmit_for_target(device_index, &req)
                                        ) {
                                            Ok(outcome) => {
                                                if outcome.accepted_local > 0 {
                                                    stats.rtx_fulfilled.fetch_add(outcome.accepted_local as u64, Ordering::Relaxed);
                                                    if let Some(dev) = stats.device(device_index) {
                                                        dev.rtx_fulfilled.fetch_add(outcome.accepted_local as u64, Ordering::Relaxed);
                                                    }
                                                    tracing::debug!(
                                                        "Group RTX[{}]: served {}/{} requested slots",
                                                        device_index, outcome.accepted_local, req.count
                                                    );
                                                }
                                                if outcome.send_failures > 0 {
                                                    tracing::warn!(
                                                        "Group RTX[{}]: {} of {} requested slot sends failed",
                                                        device_index, outcome.send_failures, req.count
                                                    );
                                                    if let Some(Some(entry)) =
                                                        listener_entries.get(device_index)
                                                    {
                                                        entry.record_runtime_warning();
                                                    }
                                                    // Fan-out send failures carry the target's
                                                    // stable identity into the bounded event
                                                    // feed; a full channel drops + counts
                                                    // instead of blocking RTP.
                                                    if let Some((_, receiver)) = event_receivers
                                                        .iter()
                                                        .find(|(idx, _)| *idx == device_index)
                                                    {
                                                        event_feed.try_publish(
                                                            GroupRuntimeEvent::TargetSendFailed {
                                                                receiver: receiver.clone(),
                                                                source: format!(
                                                                    "{} retransmit send(s) failed",
                                                                    outcome.send_failures
                                                                ),
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("Group RTX[{}]: retransmit failed: {}", device_index, e);
                                                if let Some(Some(entry)) =
                                                    listener_entries.get(device_index)
                                                {
                                                    entry.record_runtime_warning();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => {
                                // Socket closed — mark inactive but keep polling others
                            }
                        }
                    }
                }
            })
            .ok(); // Thread spawn failure is non-fatal
    }

    /// Check if group streaming is active.
    pub fn is_group_connected(&self) -> bool {
        !self.group_connections.is_empty()
    }

    /// Get the number of devices in the active group (including primary).
    pub fn group_device_count(&self) -> usize {
        if self.group_connections.is_empty() {
            0
        } else {
            1 + self.group_connections.len()
        }
    }

    /// Get the shared stream stats.
    pub fn stream_stats(&self) -> Arc<crate::stats::StreamStats> {
        Arc::clone(&self.stream_stats)
    }

    /// Build the read-only aggregated client diagnostics source.
    ///
    /// Connection rows are keyed by stable `DeviceId` and sorted at snapshot
    /// time, so receiver mapping never depends on discovery/registration
    /// order. The audio half comes from the group streamer when one is active,
    /// otherwise from the single connection's own streamer; both are real
    /// streamer-backed sources, and `None` (no fabricated numbers) when no
    /// streamer exists yet.
    pub fn diagnostics_source(&self) -> ClientDiagnosticsSource {
        let mut entries = Vec::new();
        if let Some(ref conn) = self.connection {
            entries.push(conn.diag_entry());
        }
        for conn in &self.group_connections {
            entries.push(conn.diag_entry());
        }

        let rate = self.stream_config.audio_format.sample_rate.as_hz();
        let audio = self
            .group_streamer
            .as_ref()
            .map(|s| s.diagnostics_source_with_transport(rate))
            .or_else(|| {
                self.connection
                    .as_ref()
                    .and_then(|c| c.streamer_audio_source())
            });

        ClientDiagnosticsSource::from_parts(entries, audio)
    }

    /// Get a snapshot of current stream statistics.
    ///
    /// For group streaming: uses client-level per-device stats + streamer packets_sent.
    /// For single-device: uses connection-level stats + streamer packets_sent.
    pub fn stats_snapshot(&self) -> crate::stats::StatsSnapshot {
        if self.group_streamer.is_some() {
            let mut snap = self.stream_stats.snapshot();
            if let Some(ref streamer) = self.group_streamer {
                snap.packets_sent = streamer.packets_sent();
                snap.underruns = streamer.underruns();
            }
            snap
        } else if let Some(ref conn) = self.connection {
            let mut snap = conn.stream_stats().snapshot();
            snap.packets_sent = conn.streamer_packets_sent();
            snap.underruns = conn.streamer_underruns();
            snap
        } else {
            crate::stats::StatsSnapshot::default()
        }
    }

    // Multi-room methods (legacy metadata-only group management)

    /// Create a multi-room group.
    pub async fn create_group(&mut self, devices: &[&Device]) -> Result<()> {
        if devices.is_empty() {
            return Err(Error::Discovery(DiscoveryError::NoDevicesFound));
        }

        // First device is the leader
        let leader = devices[0];

        // Connect to leader if not already connected
        if self.connection.is_none() || self.connected_device().map(|d| &d.id) != Some(&leader.id) {
            self.connect(leader).await?;
        }

        // Create group with leader
        let mut group = DeviceGroup::new(leader.clone());

        // Add remaining devices as members
        for device in devices.iter().skip(1) {
            group.add_member((*device).clone())?;
        }

        self.group = Some(group);

        self.emit_event(ClientEvent::GroupChanged).await;

        Ok(())
    }

    /// Add device to current group.
    pub async fn add_to_group(&mut self, device: &Device) -> Result<()> {
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::SetupFailed("No group exists".to_string())))?;

        group.add_member(device.clone())?;

        // Send SETPEERS to all connections
        if let Some(ref mut connection) = self.connection {
            let addresses = group.peer_addresses();
            connection.send_setpeers(&addresses).await?;
        }

        self.emit_event(ClientEvent::GroupChanged).await;

        Ok(())
    }

    /// Remove device from current group.
    pub async fn remove_from_group(&mut self, device: &Device) -> Result<()> {
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| Error::Rtsp(RtspError::SetupFailed("No group exists".to_string())))?;

        group.remove_member(&device.id)?;

        // Send updated SETPEERS
        if let Some(ref mut connection) = self.connection {
            let addresses = group.peer_addresses();
            connection.send_setpeers(&addresses).await?;
        }

        self.emit_event(ClientEvent::GroupChanged).await;

        Ok(())
    }

    /// Disband the current group.
    pub async fn disband_group(&mut self) -> Result<()> {
        self.group = None;

        self.emit_event(ClientEvent::GroupChanged).await;

        Ok(())
    }

    /// Get current group.
    pub fn group(&self) -> Option<&DeviceGroup> {
        self.group.as_ref()
    }
}

/// One member's health probe, tagged with its stable receiver identity.
///
/// Borrowing exactly one connection keeps the futures disjoint, which is what
/// lets [`AirPlayClient::probe_members`] run them all at once.
async fn probe_one(conn: &mut Connection) -> MemberResult<Health> {
    let receiver = conn.device().id.clone();
    let result = conn
        .probe(MEMBER_PROBE_TIMEOUT)
        .await
        .map_err(|source| crate::group::classify_runtime_failure(receiver.clone(), source));
    MemberResult { receiver, result }
}

/// Build a phase-classified member failure using the phase's retry policy.
fn member_failure(receiver: DeviceId, phase: SetupPhase, source: Error) -> MemberFailure {
    let retryable = phase.is_retryable();
    MemberFailure {
        receiver,
        phase,
        retryable,
        source,
    }
}

/// Tear down one failed member link and classify its failure.
async fn teardown_and_fail(
    link: &mut Box<dyn GroupMemberLink>,
    phase: SetupPhase,
    source: Error,
) -> MemberFailure {
    let failure = member_failure(link.receiver(), phase, source);
    link.teardown().await;
    failure
}

/// One successfully established group member awaiting role assignment.
struct EstablishedMember {
    /// Candidate device identity/metadata.
    device: Device,
    /// Still eligible for the primary role. Cleared when this member was
    /// tried as primary and failed, so the one fallback round cannot pick it
    /// again.
    healthy: bool,
    /// Established transport; taken on teardown or session storage.
    link: Option<Box<dyn GroupMemberLink>>,
}

/// Run the primary-critical sequence on one member link: RTSP SETUP, PTP
/// timing identity export, and SETPEERS distribution.
///
/// Failure returns the phase it happened in; the caller records the failure,
/// tears the member down member-locally, and (once) falls back to the next
/// deterministic candidate.
async fn run_primary_critical(
    link: &mut Box<dyn GroupMemberLink>,
    peer_addresses: &[String],
) -> std::result::Result<GroupPrimaryTiming, (SetupPhase, Error)> {
    link.setup_stream()
        .await
        .map_err(|source| (SetupPhase::RtspSetup, source))?;
    let timing = link.primary_timing_identity().ok_or_else(|| {
        (
            SetupPhase::PrimaryTiming,
            Error::Rtsp(RtspError::SetupFailed(
                "group primary has no PTP timing identity".into(),
            )),
        )
    })?;
    link.send_setpeers(peer_addresses)
        .await
        .map_err(|source| (SetupPhase::SetPeers, source))?;
    Ok(timing)
}

/// Peer address list: one IPv4 per member that holds a live link, plus our own
/// address as seen from the primary connection.
///
/// `members` are the established members, not the requested ones: a receiver
/// that never established has no place in the group's timing domain.
fn build_peer_addresses(members: &[Device], local: Option<String>) -> Vec<String> {
    let mut peers: Vec<String> = members
        .iter()
        .filter_map(|device| {
            device
                .addresses
                .iter()
                .find(|addr| addr.is_ipv4())
                .map(|addr| addr.to_string())
        })
        .collect();
    if let Some(local) = local {
        if !peers.contains(&local) {
            peers.push(local);
        }
    }
    peers
}

impl Default for AirPlayClient {
    fn default() -> Self {
        Self::new().expect("Failed to create client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod presentation_delay_mapping {
        use super::*;

        fn receiver(seed: u8) -> DeviceId {
            DeviceId([0x02, 0x00, 0x00, 0x00, 0x00, seed])
        }

        #[test]
        fn maps_stable_ids_onto_sender_order() {
            let targets = [receiver(1), receiver(2), receiver(3)];
            let profile = BTreeMap::from([
                (receiver(3), 7_000_000u64),
                (receiver(1), 0),
                (receiver(2), 3_500_000),
            ]);

            let delays = map_presentation_delays_for(&targets, &profile).unwrap();

            // Sender order, not the profile's sorted key order.
            assert_eq!(delays, vec![0, 3_500_000, 7_000_000]);
        }

        #[test]
        fn a_zero_delay_is_a_request_not_a_missing_entry() {
            let targets = [receiver(1), receiver(2)];
            let profile = BTreeMap::from([(receiver(1), 0u64), (receiver(2), 0)]);

            assert_eq!(
                map_presentation_delays_for(&targets, &profile).unwrap(),
                vec![0, 0]
            );
        }

        #[test]
        fn a_missing_receiver_is_rejected_instead_of_defaulted() {
            let targets = [receiver(1), receiver(2)];
            let profile = BTreeMap::from([(receiver(1), 1_000u64)]);

            let err = map_presentation_delays_for(&targets, &profile)
                .expect_err("an unmapped target must not silently default to zero");
            assert!(err.to_string().contains("missing"), "{err}");
        }

        #[test]
        fn an_unknown_receiver_is_rejected() {
            let targets = [receiver(1), receiver(2)];
            let profile = BTreeMap::from([
                (receiver(1), 0u64),
                (receiver(2), 1_000),
                (receiver(9), 2_000),
            ]);

            let err = map_presentation_delays_for(&targets, &profile)
                .expect_err("an entry for a receiver that is not connected must be rejected");
            assert!(err.to_string().contains("3 entries"), "{err}");
        }

        #[test]
        fn an_empty_target_list_needs_an_empty_profile() {
            assert_eq!(
                map_presentation_delays_for(&[], &BTreeMap::new()).unwrap(),
                Vec::<u64>::new()
            );
            assert!(
                map_presentation_delays_for(&[], &BTreeMap::from([(receiver(1), 0u64)])).is_err()
            );
        }
    }

    mod client_creation {
        use super::*;

        #[test]
        fn new_creates_disconnected_client() {
            // Skip if mDNS not available
            if let Ok(client) = AirPlayClient::new() {
                assert!(!client.is_connected());
                assert!(client.connected_device().is_none());
                assert_eq!(client.playback_state(), PlaybackState::Stopped);
            }
        }

        #[test]
        fn default_creates_client() {
            // Skip if mDNS not available
            // Note: Default panics if creation fails, so we use new() for testing
            if let Ok(client) = AirPlayClient::new() {
                assert!(!client.is_connected());
            }
        }
    }

    mod discovery {
        use super::*;

        #[tokio::test]
        async fn discover_returns_devices() {
            // Skip if mDNS not available
            if let Ok(client) = AirPlayClient::new() {
                // Very short timeout - we don't expect to find devices in tests
                let devices = client.discover(Duration::from_millis(100)).await;
                assert!(devices.is_ok());
            }
        }

        #[tokio::test]
        async fn discover_respects_timeout() {
            if let Ok(client) = AirPlayClient::new() {
                let start = std::time::Instant::now();
                let _ = client.discover(Duration::from_millis(200)).await;
                let elapsed = start.elapsed();
                // Should complete within a reasonable time of the timeout
                assert!(elapsed >= Duration::from_millis(200));
                assert!(elapsed < Duration::from_secs(2));
            }
        }

        #[tokio::test]
        async fn get_device_returns_none_for_unknown() {
            if let Ok(client) = AirPlayClient::new() {
                let unknown_id = DeviceId([0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00]);
                let result = client.get_device(&unknown_id).await;
                assert!(result.is_none());
            }
        }
    }

    mod connection {
        use super::*;

        #[tokio::test]
        async fn connect_establishes_connection() {
            // This test requires a real device - skip in unit tests
        }

        #[tokio::test]
        async fn connect_with_pin_for_protected_device() {
            // This test requires a real device - skip in unit tests
        }

        #[tokio::test]
        async fn disconnect_clears_connection() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Even without being connected, disconnect should succeed
                let result = client.disconnect().await;
                assert!(result.is_ok());
                assert!(!client.is_connected());
            }
        }

        #[tokio::test]
        async fn is_connected_reflects_state() {
            if let Ok(client) = AirPlayClient::new() {
                assert!(!client.is_connected());
            }
        }

        #[tokio::test]
        async fn connected_device_returns_device() {
            if let Ok(client) = AirPlayClient::new() {
                // Not connected, should return None
                assert!(client.connected_device().is_none());
            }
        }
    }

    mod playback {
        use super::*;

        #[tokio::test]
        async fn play_file_starts_playback() {
            // This test requires a real device - skip in unit tests
        }

        #[tokio::test]
        async fn play_file_error_when_disconnected() {
            if let Ok(mut client) = AirPlayClient::new() {
                let result = client.play_file("/nonexistent.mp3").await;
                // Should fail because we're not connected
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn pause_pauses_playback() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Should fail when not connected
                let result = client.pause().await;
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn resume_resumes_playback() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Should fail when not connected
                let result = client.resume().await;
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn stop_stops_playback() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Should fail when not connected
                let result = client.stop().await;
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn seek_changes_position() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Should fail when not connected
                let result = client.seek(10.0).await;
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn set_volume_in_range() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Should fail when not connected
                let result = client.set_volume(0.5).await;
                assert!(result.is_err());
            }
        }

        #[tokio::test]
        async fn playback_state_reflects_current_state() {
            if let Ok(client) = AirPlayClient::new() {
                assert_eq!(client.playback_state(), PlaybackState::Stopped);
            }
        }

        #[tokio::test]
        async fn playback_position_tracks_progress() {
            if let Ok(client) = AirPlayClient::new() {
                assert_eq!(client.playback_position(), 0.0);
            }
        }

        #[tokio::test]
        async fn wait_for_completion_blocks() {
            // This test would block forever without actual playback
            // Skip in unit tests
        }
    }

    mod multi_room {
        use super::*;

        #[tokio::test]
        async fn create_group_with_multiple_devices() {
            // This test requires real devices - skip in unit tests
        }

        #[tokio::test]
        async fn add_to_group_adds_device() {
            // This test requires real devices - skip in unit tests
        }

        #[tokio::test]
        async fn remove_from_group_removes_device() {
            // This test requires real devices - skip in unit tests
        }

        #[tokio::test]
        async fn disband_group_clears_group() {
            if let Ok(mut client) = AirPlayClient::new() {
                // Even without a group, disband should succeed
                let result = client.disband_group().await;
                assert!(result.is_ok());
                assert!(client.group().is_none());
            }
        }
    }

    mod events {
        use super::*;

        #[tokio::test]
        async fn event_handler_called_on_connect() {
            // This test requires real devices - skip in unit tests
        }

        #[tokio::test]
        async fn event_handler_called_on_disconnect() {
            // We can test this without a device
            use crate::events::CallbackHandler;
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;

            let event_received = Arc::new(AtomicBool::new(false));
            let event_received_clone = Arc::clone(&event_received);

            if let Ok(mut client) = AirPlayClient::new() {
                client.set_event_handler(CallbackHandler::new(move |event| {
                    if matches!(event, ClientEvent::Disconnected(_)) {
                        event_received_clone.store(true, Ordering::SeqCst);
                    }
                }));

                client.disconnect().await.unwrap();
                assert!(event_received.load(Ordering::SeqCst));
            }
        }

        #[tokio::test]
        async fn event_handler_called_on_playback_change() {
            // This test requires real devices - skip in unit tests
        }
    }
}
