//! Core casting logic: discovery, an AirPlay streaming session, and WASAPI
//! loopback capture. Kept independent of any UI so it can be driven from the
//! CLI (`--list`) or the system-tray control thread.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use airplay_audio::{AlacEncoder, LiveAudioDecoder, LiveFrameSender, LivePcmFrame};
use airplay_client::{AirPlayClient, Connection};
use airplay_core::device::Device;
use airplay_core::stream::{PtpMode, StreamType, TimingProtocol};
use airplay_core::{AudioCodec, AudioFormat, StreamConfig};
use airplay_discovery::{Discovery, ServiceBrowser};
use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

/// Capture/stream sample rate and layout. WASAPI autoconverts the system mix to
/// this, so the live decoder receives it as-is (no resampling on our side).
const RATE: u32 = 44_100;
const CHANNELS: u8 = 2;

fn silence_sample_count(elapsed: Duration) -> usize {
    let frames = elapsed.as_nanos() * RATE as u128 / 1_000_000_000;
    let bounded_frames = frames.min((RATE / 4) as u128);
    bounded_frames as usize * CHANNELS as usize
}

/// HomePod volume (0.0–1.0) used on first connect, kept low so it doesn't blast.
/// The persisted copy of this value lives in `device_service::LegacyDevicePreferences`
/// (`%APPDATA%\volume.txt` -- this module wrote `%APPDATA%\HomePodCast\volume.txt`
/// before that refactor, which is why `backend_bridge::legacy_volume_source`
/// has to look for both); the saved hotkey migrated to schema-v1 shell
/// preferences in `preferences`.
pub const DEFAULT_VOLUME: f32 = 0.25;

/// Discover AirPlay 2 audio devices on the LAN, HomePods first.
pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<Device>> {
    let browser = ServiceBrowser::new()?;
    let mut devices = browser.scan(timeout).await?;
    devices.retain(|d| d.supports_airplay2() && d.addresses.iter().any(|a| a.is_ipv4()));
    devices.sort_by_key(|d| u8::from(!d.model.starts_with("AudioAccessory")));
    Ok(devices)
}

fn validate_receiver_count(count: usize) -> anyhow::Result<()> {
    anyhow::ensure!(count > 0, "select at least one AirPlay receiver");
    Ok(())
}

fn timing_protocol_for(receiver_count: usize) -> TimingProtocol {
    if receiver_count > 1 {
        TimingProtocol::Ptp
    } else {
        TimingProtocol::Ntp
    }
}

fn stream_config(receiver_count: usize) -> anyhow::Result<StreamConfig> {
    let audio_format = AudioFormat::default();
    let asc = if audio_format.codec == AudioCodec::Alac {
        Some(AlacEncoder::new(audio_format.clone())?.magic_cookie())
    } else {
        None
    };

    Ok(StreamConfig {
        stream_type: StreamType::Realtime,
        audio_format,
        timing_protocol: timing_protocol_for(receiver_count),
        ptp_mode: PtpMode::Master,
        latency_min: 22050,
        latency_max: 88200,
        supports_dynamic_stream_id: true,
        sender_buffer: Default::default(),
        asc,
    })
}

enum Transport {
    Single(Connection),
    Group(AirPlayClient),
}

impl Transport {
    async fn start_live(&mut self, decoder: LiveAudioDecoder) -> anyhow::Result<()> {
        match self {
            Self::Single(connection) => connection.start_streaming_live(decoder).await?,
            Self::Group(client) => client.start_live_streaming_to_group(decoder).await?,
        }
        Ok(())
    }

    async fn feedback(&mut self) {
        match self {
            Self::Single(connection) => {
                let _ = connection.send_feedback().await;
            }
            Self::Group(client) => {
                let _ = client.send_feedback().await;
            }
        }
    }

    async fn set_volume(&mut self, volume: f32) {
        match self {
            Self::Single(connection) => {
                let _ = connection.set_volume(volume).await;
            }
            Self::Group(client) => {
                let _ = client.set_volume(volume).await;
            }
        }
    }

    async fn disconnect(&mut self) {
        match self {
            Self::Single(connection) => {
                let _ = connection.disconnect().await;
            }
            Self::Group(client) => {
                let _ = client.disconnect().await;
            }
        }
    }
}

/// An active stream to one or more receivers: a single shared WASAPI capture
/// thread feeding either one connection or the AirPlay group transport.
pub struct Session {
    transport: Transport,
    cap_stop: Arc<AtomicBool>,
    cap_handle: Option<JoinHandle<()>>,
}

impl Session {
    /// Connect and start one synchronized stream. A single receiver keeps the
    /// established NTP path; groups use PTP and one shared live decoder.
    pub async fn start(mut devices: Vec<Device>, volume: f32) -> anyhow::Result<Self> {
        validate_receiver_count(devices.len())?;

        for device in &mut devices {
            let ipv4 = device
                .addresses
                .iter()
                .find(|address| address.is_ipv4())
                .copied()
                .ok_or_else(|| anyhow::anyhow!("{} has no IPv4 address", device.name))?;
            device.addresses = vec![ipv4];
        }

        let config = stream_config(devices.len())?;
        let mut transport = if devices.len() == 1 {
            let mut connection =
                Connection::connect_auto(devices.remove(0), config, "3939").await?;
            connection.setup().await?;
            Transport::Single(connection)
        } else {
            let mut client = AirPlayClient::with_config(config, None)?;
            client.connect_group(&devices).await?;
            Transport::Group(client)
        };

        // Set volume BEFORE audio starts so the first packets aren't at the
        // library's default of 1.0 (full blast).
        transport.set_volume(volume).await;

        // Start capture BEFORE start_streaming_live so the buffer pre-fills (with
        // real audio or silence) — otherwise the streamer's buffer-fill wait times
        // out at 0% and starts with underruns.
        let (sender, decoder) = LiveAudioDecoder::create_pair(RATE, CHANNELS, 32);
        let cap_stop = Arc::new(AtomicBool::new(false));
        let stop2 = cap_stop.clone();
        let cap_handle = std::thread::spawn(move || {
            if let Err(e) = run_capture(sender, stop2) {
                tracing::error!("capture error: {e:#}");
            }
        });

        if let Err(error) = transport.start_live(decoder).await {
            cap_stop.store(true, Ordering::Relaxed);
            let _ = cap_handle.join();
            transport.disconnect().await;
            return Err(error);
        }

        Ok(Self {
            transport,
            cap_stop,
            cap_handle: Some(cap_handle),
        })
    }

    /// Send the periodic AirPlay 2 keepalive. The receiver expects this every
    /// ~2 s; without it the HomePod tears the session down and audio stops.
    pub async fn feedback(&mut self) {
        self.transport.feedback().await;
    }

    /// Change the playback volume (0.0–1.0) on the connected device.
    pub async fn set_volume(&mut self, volume: f32) {
        self.transport.set_volume(volume).await;
    }

    /// Stop capture and tear down the AirPlay connection.
    pub async fn stop(mut self) {
        // Stop capture first: dropping the sender disconnects the live decoder,
        // so the streamer winds down.
        self.cap_stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.cap_handle.take() {
            let _ = h.join();
        }
        // Tear down the connection, but bound it with a timeout: this alpha stack
        // can block on teardown, and hanging here would freeze the next Start.
        let _ = tokio::time::timeout(Duration::from_secs(4), self.transport.disconnect()).await;
    }
}

/// Capture the default render endpoint via WASAPI loopback and push PCM frames
/// to the AirPlay live decoder until `stop` is set.
fn run_capture(sender: LiveFrameSender, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    // Keep the COM guard alive for the lifetime of this thread.
    let _com = initialize_mta();

    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Render)?;
    let mut audio_client = device.get_iaudioclient()?;

    // Render device + Capture direction + Shared mode => loopback. autoconvert
    // gives us 44.1 kHz / stereo / f32 regardless of the device's mix format.
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        RATE as usize,
        CHANNELS as usize,
        None,
    );
    let (_default_period, min_period) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_period,
    };
    audio_client.initialize_client(&format, &Direction::Capture, &mode)?;

    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    let mut queue: VecDeque<u8> = VecDeque::new();
    audio_client.start_stream()?;
    tracing::info!("loopback capture started ({RATE} Hz, {CHANNELS}ch, f32)");

    let bytes_per_frame = CHANNELS as usize * 4; // f32 per sample

    // WASAPI loopback delivers no frames while the PC is silent. Track real
    // elapsed time so Windows timer rounding cannot under-produce keepalive
    // silence and slowly drain the AirPlay buffer.
    let mut last_capture_at = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        capture_client.read_from_device_to_deque(&mut queue)?;
        let now = Instant::now();
        let elapsed = now.duration_since(last_capture_at);

        let n_frames = queue.len() / bytes_per_frame;
        if n_frames > 0 {
            let n_samples = n_frames * CHANNELS as usize;
            let mut samples: Vec<i16> = Vec::with_capacity(n_samples);
            for _ in 0..n_samples {
                let b = [
                    queue.pop_front().unwrap(),
                    queue.pop_front().unwrap(),
                    queue.pop_front().unwrap(),
                    queue.pop_front().unwrap(),
                ];
                let f = f32::from_le_bytes(b);
                samples.push((f.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
            sender.try_send(LivePcmFrame {
                samples,
                channels: CHANNELS,
                sample_rate: RATE,
            });
        } else {
            let sample_count = silence_sample_count(elapsed);
            if sample_count > 0 {
                sender.try_send(LivePcmFrame {
                    samples: vec![0i16; sample_count],
                    channels: CHANNELS,
                    sample_rate: RATE,
                });
            }
        }
        last_capture_at = now;

        // Responsive (~event latency) while audio plays; ~50 ms idle polling to
        // pace the silence keepalive.
        let _ = h_event.wait_for_event(50);
    }

    let _ = audio_client.stop_stream();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_requires_at_least_one_receiver() {
        let error = validate_receiver_count(0).unwrap_err();
        assert_eq!(error.to_string(), "select at least one AirPlay receiver");
    }

    #[test]
    fn groups_use_ptp_while_single_receivers_keep_ntp() {
        assert_eq!(timing_protocol_for(1), TimingProtocol::Ntp);
        assert_eq!(timing_protocol_for(2), TimingProtocol::Ptp);
        assert_eq!(timing_protocol_for(8), TimingProtocol::Ptp);
    }

    #[test]
    fn idle_silence_matches_actual_capture_elapsed_time() {
        assert_eq!(silence_sample_count(Duration::from_millis(50)), 4_410);
        assert_eq!(silence_sample_count(Duration::from_millis(625) / 10), 5_512);
    }
}
