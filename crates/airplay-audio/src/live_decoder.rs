//! Live audio decoder for streaming PCM from external sources.
//!
//! This module provides a decoder-like interface for live audio sources
//! (e.g., Bluetooth capture, microphone) that can be used with the existing
//! AudioStreamer pipeline.

use airplay_core::{error::Result, AudioFormat};
use crossbeam_channel::{bounded, Receiver, SendError, Sender, TrySendError};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::decoder::DecodedFrame;
use crate::diagnostics::{
    pcm_frame_samples_per_channel, AudioDiagnosticsSource, CaptureQueueCounters,
};

/// Frame of PCM audio sent to the live decoder.
#[derive(Debug, Clone)]
pub struct LivePcmFrame {
    /// Interleaved PCM samples (i16).
    pub samples: Vec<i16>,
    /// Number of channels.
    pub channels: u8,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

/// Outcome of [`LiveFrameSender::try_send_latest`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveSendOutcome {
    /// The frame was appended to a queue with free capacity.
    Enqueued,
    /// The queue was full: the oldest queued frame was evicted (counted once
    /// in `full_queue_drops_total`) and this frame took its place.
    ReplacedOldest,
    /// The paired decoder is gone; the frame was dropped (counted once in
    /// `disconnected_drops_total`).
    Disconnected,
}

/// Sender for pushing live PCM frames to a LiveAudioDecoder.
///
/// Carries shared capture-queue counters so
/// [`LiveFrameSender::diagnostics_source`] can report every submit attempt,
/// full-queue drop, and disconnected drop truthfully.
///
/// # Replace-oldest delivery (`try_send_latest`)
///
/// The sender retains a clone of the decoder-side channel receiver used only
/// to evict the oldest queued frame when the bounded channel is full
/// (crossbeam receivers cannot pop from the sender side).
///
/// DEVIATION from plan Task 8 Step 3: retaining that receiver clone keeps the
/// crossbeam channel permanently "connected", so raw channel state can no
/// longer report decoder drop. Decoder liveness is therefore tracked by the
/// explicit `decoder_alive` latch (cleared by [`LiveAudioDecoder::drop`]),
/// which restores the historical disconnect behavior required by both the T3
/// diagnostics test `capture_queue_counts_disconnected_drops` and Task 8's own
/// disconnect expectations. Outcomes still reflect real pipeline state: a send
/// into the retained receiver with the decoder gone would silently buffer
/// frames nobody consumes, so it is reported as `Disconnected` instead.
///
/// The struct intentionally stays non-`Clone` (plan constraint): sole
/// ownership makes eviction plus replacement atomic with respect to producers
/// while the decoder keeps consuming.
pub struct LiveFrameSender {
    tx: Sender<LivePcmFrame>,
    /// Shared clone of the decoder-side receiver; pops exactly one oldest
    /// frame per successful eviction in [`Self::try_send_latest`].
    eviction_rx: Receiver<LivePcmFrame>,
    /// Cleared exactly once, when the paired decoder is dropped.
    decoder_alive: Arc<AtomicBool>,
    stats: Arc<CaptureQueueCounters>,
}

impl LiveFrameSender {
    /// True once the paired decoder was dropped.
    ///
    /// With the retained eviction receiver the crossbeam channel itself never
    /// disconnects, so this latch is the source of truth for the "receiver
    /// side gone" outcome.
    fn decoder_disconnected(&self) -> bool {
        !self
            .decoder_alive
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Count one dropped frame caused by a missing receiver side.
    fn count_disconnected_drop(&self) {
        tracing::debug!("Live audio channel disconnected");
        self.stats
            .disconnected_drops_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one frame dropped because the bounded queue was full.
    fn count_full_queue_drop(&self) {
        tracing::debug!("Live audio channel full, dropping frame");
        self.stats
            .full_queue_drops_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Send a frame of PCM audio.
    ///
    /// Returns true if the frame was sent, false if the channel is full or
    /// disconnected. Every call counts as one submission attempt in the
    /// diagnostics counters, regardless of outcome.
    pub fn try_send(&self, frame: LivePcmFrame) -> bool {
        self.stats
            .submitted_frames_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.stats.submitted_samples_per_channel_total.fetch_add(
            pcm_frame_samples_per_channel(&frame),
            std::sync::atomic::Ordering::Relaxed,
        );
        if self.decoder_disconnected() {
            self.count_disconnected_drop();
            return false;
        }
        match self.tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.count_full_queue_drop();
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.count_disconnected_drop();
                false
            }
        }
    }

    /// Deliver the newest frame without ever blocking ("latest-wins").
    ///
    /// On a full queue the oldest queued frame is evicted through the retained
    /// receiver and the new frame takes its place; the decoder keeps consuming
    /// the remaining frames in order. Never blocks; never panics on channel
    /// state.
    ///
    /// Diagnostics: one call counts as exactly ONE submission attempt (frame
    /// and samples) regardless of internal evictions/retries — it truthfully
    /// represents one delivered frame replacing another. Each eviction counts
    /// once in `full_queue_drops_total`; a lost decoder counts once in
    /// `disconnected_drops_total`.
    ///
    /// Takes `&mut self` deliberately: sole-producer ownership makes the
    /// evict-and-replace step atomic with respect to other producers (the only
    /// other `tx` clone lives in the diagnostics source and never sends).
    pub fn try_send_latest(&mut self, mut frame: LivePcmFrame) -> LiveSendOutcome {
        self.stats
            .submitted_frames_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.stats.submitted_samples_per_channel_total.fetch_add(
            pcm_frame_samples_per_channel(&frame),
            std::sync::atomic::Ordering::Relaxed,
        );
        if self.decoder_disconnected() {
            self.count_disconnected_drop();
            return LiveSendOutcome::Disconnected;
        }
        match self.tx.try_send(frame) {
            Ok(()) => LiveSendOutcome::Enqueued,
            Err(TrySendError::Disconnected(_)) => {
                self.count_disconnected_drop();
                LiveSendOutcome::Disconnected
            }
            Err(TrySendError::Full(returned)) => {
                frame = returned;
                loop {
                    // One oldest frame leaves the queue per successful recv,
                    // freeing one slot; an Err means the consumer drained
                    // concurrently and there is nothing left to evict.
                    if let Ok(_evicted) = self.eviction_rx.try_recv() {
                        self.count_full_queue_drop();
                    }
                    match self.tx.try_send(frame) {
                        Ok(()) => return LiveSendOutcome::ReplacedOldest,
                        // Only reachable when the consumer freed nothing and
                        // eviction found an empty queue; loop until a slot is
                        // observable. Terminates because this method is the
                        // only producer and each pass frees or observes space.
                        Err(TrySendError::Full(again)) => frame = again,
                        Err(TrySendError::Disconnected(_)) => {
                            self.count_disconnected_drop();
                            return LiveSendOutcome::Disconnected;
                        }
                    }
                }
            }
        }
    }

    /// Send a frame, blocking if the channel is full.
    ///
    /// Returns true if the frame was sent, false if the receiver was dropped.
    pub fn send(&self, frame: LivePcmFrame) -> bool {
        self.stats
            .submitted_frames_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.stats.submitted_samples_per_channel_total.fetch_add(
            pcm_frame_samples_per_channel(&frame),
            std::sync::atomic::Ordering::Relaxed,
        );
        if self.decoder_disconnected() {
            self.count_disconnected_drop();
            return false;
        }
        match self.tx.send(frame) {
            Ok(()) => true,
            Err(SendError(_)) => {
                self.count_disconnected_drop();
                false
            }
        }
    }

    /// Get the channel capacity.
    pub fn capacity(&self) -> Option<usize> {
        self.tx.capacity()
    }

    /// Check if the channel is full.
    pub fn is_full(&self) -> bool {
        self.tx.is_full()
    }

    /// Create a diagnostics source observing this sender's capture queue.
    ///
    /// The returned source reports live values in its `capture_queue` half;
    /// the `sender_buffer` half stays all-zero because the capture queue is
    /// not the sender-side ring buffer ("not applicable", not fabricated
    /// numbers).
    ///
    /// `queued_frames` comes from `tx.len()` at snapshot time — the true
    /// channel occupancy read without blocking. `sample_rate_hz` is accepted
    /// for API symmetry with [`crate::buffer::AudioBuffer::diagnostics_source`]
    /// and currently unused because [`CaptureQueueSnapshot`](crate::diagnostics::CaptureQueueSnapshot)
    /// has no time dimension.
    pub fn diagnostics_source(&self, sample_rate_hz: u32) -> AudioDiagnosticsSource {
        let _ = sample_rate_hz;
        let tx = self.tx.clone();
        AudioDiagnosticsSource::capture_queue(Arc::clone(&self.stats), move || tx.len())
    }
}

/// Live audio decoder that receives PCM from a channel.
///
/// This provides a decoder-like interface compatible with AudioStreamer,
/// allowing live audio sources to be streamed over AirPlay.
pub struct LiveAudioDecoder {
    rx: Receiver<LivePcmFrame>,
    sample_rate: u32,
    channels: u8,
    position_samples: u64,
    eof: bool,
    /// Residual samples from previous decode_resampled call.
    residual_samples: Vec<i16>,
    /// Timeout for receiving frames.
    recv_timeout: Duration,
    /// High-quality sinc resampler (lazily initialized when needed).
    resampler: Option<airplay_resampler::Resampler>,
    /// Set when this decoder owns half of a [`Self::create_pair`] pair; its
    /// `LiveFrameSender` observes this latch to report disconnect truthfully
    /// (the retained eviction receiver would otherwise keep the crossbeam
    /// channel connected forever).
    pair_alive: Option<Arc<AtomicBool>>,
}

impl LiveAudioDecoder {
    /// Create a new live decoder with the given channel receiver.
    ///
    /// NOTE: The default receive timeout is zero so checking the live source can
    /// never stall the packet scheduler. This matters on Windows, where even a
    /// nominal 2 ms channel timeout can be rounded to roughly 15.6 ms. Callers
    /// outside the real-time scheduler can opt into waiting with
    /// [`Self::set_recv_timeout`].
    pub fn new(rx: Receiver<LivePcmFrame>, sample_rate: u32, channels: u8) -> Self {
        Self {
            rx,
            sample_rate,
            channels,
            position_samples: 0,
            eof: false,
            residual_samples: Vec::new(),
            recv_timeout: Duration::ZERO,
            resampler: None,
            pair_alive: None,
        }
    }

    /// Create a live decoder and sender pair.
    ///
    /// The sender can be used to push PCM frames to the decoder.
    /// Channel capacity controls buffering (typically 8-16 frames).
    ///
    /// The sender retains a receiver clone for replace-oldest eviction and
    /// observes decoder liveness through a shared latch cleared when this
    /// decoder is dropped, so disconnect outcomes stay truthful.
    pub fn create_pair(sample_rate: u32, channels: u8, capacity: usize) -> (LiveFrameSender, Self) {
        let (tx, rx) = bounded::<LivePcmFrame>(capacity);
        let pair_alive = Arc::new(AtomicBool::new(true));
        let sender = LiveFrameSender {
            tx,
            eviction_rx: rx.clone(),
            decoder_alive: Arc::clone(&pair_alive),
            stats: Arc::new(CaptureQueueCounters::with_capacity(capacity)),
        };
        let mut decoder = Self::new(rx, sample_rate, channels);
        decoder.pair_alive = Some(pair_alive);
        (sender, decoder)
    }

    /// Attaches an additional consumer handle to the same stable PCM queue.
    ///
    /// Use this when a long-lived producer (a capture bridge) must outlive the
    /// short-lived consumers that drain it: the pair — and therefore the
    /// producer's view of the timeline — stays intact while each consumer
    /// generation gets its own decoder-local state (position, residual
    /// samples, resampler).
    ///
    /// The returned handle deliberately does NOT own the pair-liveness latch,
    /// so dropping it never makes the paired [`LiveFrameSender`] report
    /// [`LiveSendOutcome::Disconnected`]; only dropping the handle returned by
    /// [`Self::create_pair`] ends the pair.
    ///
    /// Frames are delivered to exactly one handle. Keep at most one draining
    /// consumer alive at a time, or the two will split the stream between
    /// them.
    pub fn attach(&self) -> Self {
        let mut attached = Self::new(self.rx.clone(), self.sample_rate, self.channels);
        attached.recv_timeout = self.recv_timeout;
        attached
    }

    /// Set the receive timeout.
    pub fn set_recv_timeout(&mut self, timeout: Duration) {
        self.recv_timeout = timeout;
    }

    /// Mark the stream as ended (no more frames will be sent).
    pub fn mark_eof(&mut self) {
        self.eof = true;
    }

    /// Get source sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get number of channels.
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Get duration in samples (always None for live streams).
    pub fn duration_samples(&self) -> Option<u64> {
        None
    }

    /// Get current position in samples.
    pub fn position_samples(&self) -> u64 {
        self.position_samples
    }

    /// Seek to position (not supported for live streams).
    pub fn seek(&mut self, _position_samples: u64) -> Result<()> {
        Err(airplay_core::error::StreamingError::Encoding(
            "Cannot seek in live audio stream".into(),
        )
        .into())
    }

    /// Decode next frame of audio.
    pub fn decode_frame(&mut self) -> Result<Option<DecodedFrame>> {
        if self.eof {
            return Ok(None);
        }

        match self.rx.recv_timeout(self.recv_timeout) {
            Ok(frame) => {
                let num_frames = frame.samples.len() / frame.channels as usize;
                let decoded = DecodedFrame {
                    samples: frame.samples,
                    channels: frame.channels,
                    sample_rate: frame.sample_rate,
                    timestamp: self.position_samples,
                };
                self.position_samples += num_frames as u64;
                Ok(Some(decoded))
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // Timeout - no data available but stream may continue
                tracing::trace!("Live decoder: receive timeout (no data)");
                Ok(None)
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                // Channel disconnected - mark EOF
                tracing::debug!("Live decoder: channel disconnected, marking EOF");
                self.eof = true;
                Ok(None)
            }
        }
    }

    /// Decode and resample to target format.
    ///
    /// This method matches the AudioDecoder interface for compatibility
    /// with AudioStreamer.
    pub fn decode_resampled(
        &mut self,
        target_format: &AudioFormat,
        frames_per_packet: usize,
    ) -> Result<Option<DecodedFrame>> {
        let target_rate = target_format.sample_rate.as_hz();
        let source_rate = self.sample_rate;

        // Initialize resampler lazily if needed
        if source_rate != target_rate && self.resampler.is_none() {
            self.resampler = Some(airplay_resampler::Resampler::new(
                source_rate,
                target_rate,
                self.channels,
            )?);
        }

        // Start with any leftover samples from previous call
        let mut collected_samples = std::mem::take(&mut self.residual_samples);
        let target_samples = frames_per_packet * target_format.channels as usize;

        // Collect frames until we have enough samples
        while collected_samples.len() < target_samples {
            match self.decode_frame()? {
                Some(frame) => {
                    if source_rate == target_rate {
                        collected_samples.extend(frame.samples);
                    } else {
                        // High-quality sinc resampling
                        let resampled = self
                            .resampler
                            .as_mut()
                            .expect("resampler should be initialized")
                            .process(&frame.samples)?;
                        collected_samples.extend(resampled);
                    }
                }
                None => {
                    if collected_samples.is_empty() {
                        return Ok(None);
                    }
                    // Not enough samples for a full packet — save partial data
                    // back to residual instead of padding with silence (which
                    // causes audible pops). Next call will pick these up.
                    self.residual_samples = collected_samples;
                    return Ok(None);
                }
            }
        }

        if collected_samples.is_empty() {
            return Ok(None);
        }

        // Save excess samples for next call
        if collected_samples.len() > target_samples {
            self.residual_samples = collected_samples[target_samples..].to_vec();
            collected_samples.truncate(target_samples);
        }

        // Scale timestamp from source sample rate to target sample rate
        let scaled_timestamp = if source_rate != target_rate {
            (self.position_samples as f64 * target_rate as f64 / source_rate as f64) as u64
        } else {
            self.position_samples
        };

        Ok(Some(DecodedFrame {
            samples: collected_samples,
            channels: target_format.channels,
            sample_rate: target_rate,
            timestamp: scaled_timestamp,
        }))
    }

    /// Check if at end of stream.
    pub fn is_eof(&self) -> bool {
        self.eof && self.rx.is_empty()
    }
}

impl Drop for LiveAudioDecoder {
    fn drop(&mut self) {
        // Notify the paired sender that the real consumer is gone; the
        // retained eviction receiver would otherwise keep the crossbeam
        // channel "connected" forever.
        if let Some(alive) = self.pair_alive.as_ref() {
            alive.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::SampleRate;

    fn test_format() -> AudioFormat {
        AudioFormat {
            codec: airplay_core::AudioCodec::Alac,
            sample_rate: SampleRate::Hz44100,
            bit_depth: 16,
            channels: 2,
            frames_per_packet: 352,
        }
    }

    /// Generate a stereo sine wave for testing.
    fn generate_sine_wave(frequency: f64, sample_rate: u32, num_samples: usize) -> Vec<i16> {
        let mut samples = Vec::with_capacity(num_samples * 2);
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let value = (2.0 * std::f64::consts::PI * frequency * t).sin();
            let sample = (value * 16000.0) as i16; // ~50% amplitude
            samples.push(sample); // Left
            samples.push(sample); // Right
        }
        samples
    }

    /// Frame whose first sample equals `value` so decoded frame order is
    /// directly observable.
    fn frame_with_value(value: i16) -> LivePcmFrame {
        LivePcmFrame {
            samples: vec![value; 8],
            channels: 2,
            sample_rate: 44_100,
        }
    }

    #[test]
    fn latest_send_replaces_oldest_frame_when_full() {
        let (mut sender, mut decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);

        // Enqueued path comes first.
        assert_eq!(
            sender.try_send_latest(frame_with_value(1)),
            LiveSendOutcome::Enqueued
        );
        assert_eq!(
            sender.try_send_latest(frame_with_value(2)),
            LiveSendOutcome::Enqueued
        );

        // Queue is full: the newest frame replaces the oldest one.
        assert_eq!(
            sender.try_send_latest(frame_with_value(3)),
            LiveSendOutcome::ReplacedOldest
        );

        // Frame 1 was evicted; delivery continues with 2, then 3.
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 2);
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 3);

        // Dropping the decoder disconnects the delivery path.
        drop(decoder);
        assert_eq!(
            sender.try_send_latest(frame_with_value(9)),
            LiveSendOutcome::Disconnected
        );
    }

    #[test]
    fn try_send_keeps_previous_semantics_and_counts() {
        let (mut sender, mut decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);
        let source = sender.diagnostics_source(44_100);

        // Legacy contract: every attempt counts, including a rejected one.
        assert!(sender.try_send(frame_with_value(1)));
        assert!(sender.try_send(frame_with_value(2)));
        assert!(!sender.try_send(frame_with_value(3)), "queue is full");
        {
            let snap = source.snapshot().capture_queue;
            assert_eq!(snap.submitted_frames_total, 3);
            assert_eq!(snap.submitted_samples_per_channel_total, 3 * 4);
            assert_eq!(snap.full_queue_drops_total, 1);
            assert_eq!(snap.disconnected_drops_total, 0);
            assert_eq!(snap.queued_frames, 2);
        }

        // Freeing a slot makes the same send succeed again.
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 1);
        assert!(sender.try_send(frame_with_value(3)));
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 2);

        // try_send_latest counts exactly ONE submission attempt per call even
        // when it evicts and retries internally; the evicted oldest frame is
        // counted once as a full-queue drop.
        assert_eq!(
            sender.try_send_latest(frame_with_value(4)),
            LiveSendOutcome::Enqueued
        );
        assert_eq!(
            sender.try_send_latest(frame_with_value(5)),
            LiveSendOutcome::ReplacedOldest
        );
        {
            let snap = source.snapshot().capture_queue;
            assert_eq!(snap.submitted_frames_total, 6);
            assert_eq!(snap.full_queue_drops_total, 2);
            assert_eq!(snap.disconnected_drops_total, 0);
        }
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 4);
        assert_eq!(decoder.decode_frame().unwrap().unwrap().samples[0], 5);

        // Disconnect behavior of try_send is preserved after decoder drop.
        drop(decoder);
        assert!(!sender.try_send(frame_with_value(8)));
        assert_eq!(
            sender.try_send_latest(frame_with_value(9)),
            LiveSendOutcome::Disconnected
        );
        let snap = source.snapshot().capture_queue;
        assert_eq!(snap.submitted_frames_total, 8);
        assert_eq!(snap.full_queue_drops_total, 2);
        assert_eq!(snap.disconnected_drops_total, 2);
    }

    #[test]
    fn create_pair_works() {
        let (sender, decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        assert_eq!(sender.capacity(), Some(8));
        assert_eq!(decoder.sample_rate(), 44100);
        assert_eq!(decoder.channels(), 2);
    }

    #[test]
    fn empty_default_decoder_does_not_block_the_packet_scheduler() {
        let (_sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        let start = std::time::Instant::now();

        assert!(decoder.decode_frame().unwrap().is_none());
        assert!(
            start.elapsed() < Duration::from_millis(10),
            "empty live decode blocked for {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn send_and_receive_frame() {
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);

        let frame = LivePcmFrame {
            samples: vec![100i16; 704],
            channels: 2,
            sample_rate: 44100,
        };

        assert!(sender.try_send(frame));

        let decoded = decoder.decode_frame().unwrap();
        assert!(decoded.is_some());
        let decoded = decoded.unwrap();
        assert_eq!(decoded.samples.len(), 704);
        assert_eq!(decoded.samples[0], 100);
    }

    #[test]
    fn continuous_streaming_works() {
        // Simulate continuous Bluetooth audio capture
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 16);
        decoder.set_recv_timeout(Duration::from_millis(50));

        let format = test_format();
        let frames_per_packet = format.frames_per_packet as usize;

        // Send continuous audio (like PipeWire capture)
        // PipeWire sends 1024 frames per period at 44100Hz
        let pipewire_period = 1024;
        let mut total_input_samples = 0;
        let mut total_output_frames = 0;

        // Send 10 periods worth of audio
        for period in 0..10 {
            let sine_samples = generate_sine_wave(440.0, 44100, pipewire_period);
            let frame = LivePcmFrame {
                samples: sine_samples.clone(),
                channels: 2,
                sample_rate: 44100,
            };
            assert!(sender.try_send(frame), "Failed to send period {}", period);
            total_input_samples += pipewire_period;
        }

        // Now decode all available frames
        loop {
            match decoder.decode_resampled(&format, frames_per_packet) {
                Ok(Some(frame)) => {
                    // Verify frame is correct size
                    assert_eq!(
                        frame.samples.len(),
                        frames_per_packet * 2,
                        "Frame has wrong number of samples"
                    );

                    // Verify audio isn't silent (RMS > 0)
                    let rms: f64 = frame
                        .samples
                        .iter()
                        .map(|&s| (s as f64).powi(2))
                        .sum::<f64>()
                        / frame.samples.len() as f64;
                    let rms = rms.sqrt();
                    assert!(rms > 1000.0, "Audio appears silent, RMS = {}", rms);

                    total_output_frames += 1;
                }
                Ok(None) => break,
                Err(e) => panic!("Decode error: {}", e),
            }
        }

        // We sent 10 * 1024 = 10240 frames
        // Each output packet is 352 frames
        // So we should get floor(10240 / 352) = 29 packets
        let expected_packets = total_input_samples / frames_per_packet;
        assert!(
            total_output_frames >= expected_packets - 1
                && total_output_frames <= expected_packets + 1,
            "Expected ~{} output frames, got {}",
            expected_packets,
            total_output_frames
        );

        println!(
            "Continuous streaming test: {} input samples -> {} output packets",
            total_input_samples, total_output_frames
        );
    }

    #[test]
    fn decode_produces_non_silent_audio() {
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        decoder.set_recv_timeout(Duration::from_millis(10));

        // Send a 440Hz sine wave
        let samples = generate_sine_wave(440.0, 44100, 1024);
        let frame = LivePcmFrame {
            samples: samples.clone(),
            channels: 2,
            sample_rate: 44100,
        };
        sender.try_send(frame);

        let format = test_format();
        let decoded = decoder.decode_resampled(&format, 352).unwrap().unwrap();

        // Calculate RMS
        let rms: f64 = decoded
            .samples
            .iter()
            .map(|&s| (s as f64).powi(2))
            .sum::<f64>()
            / decoded.samples.len() as f64;
        let rms = rms.sqrt();

        println!("Decoded RMS: {}", rms);
        assert!(rms > 5000.0, "Audio is too quiet, RMS = {}", rms);
    }

    #[test]
    fn residual_samples_preserved_across_calls() {
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        decoder.set_recv_timeout(Duration::from_millis(10));

        // Send a frame larger than what we'll request
        // 1024 stereo samples = 2048 total i16 values
        let frame = LivePcmFrame {
            samples: vec![1234i16; 2048],
            channels: 2,
            sample_rate: 44100,
        };
        sender.try_send(frame);

        let format = test_format();

        // First decode: should get 352 frames
        let decoded1 = decoder.decode_resampled(&format, 352).unwrap().unwrap();
        assert_eq!(decoded1.samples.len(), 704);

        // Second decode: should get next 352 frames from residual
        let decoded2 = decoder.decode_resampled(&format, 352).unwrap().unwrap();
        assert_eq!(decoded2.samples.len(), 704);

        // Third decode: should get remaining ~320 frames (padded with silence)
        // or return None if we don't have enough
    }

    #[test]
    fn decode_resampled_collects_frames() {
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        decoder.set_recv_timeout(Duration::from_millis(10));

        // Send multiple small frames
        for _ in 0..3 {
            let frame = LivePcmFrame {
                samples: vec![1000i16; 300],
                channels: 2,
                sample_rate: 44100,
            };
            sender.try_send(frame);
        }

        let format = test_format();
        let decoded = decoder.decode_resampled(&format, 352).unwrap();
        assert!(decoded.is_some());
        let decoded = decoded.unwrap();
        // Should have exactly 352 frames * 2 channels = 704 samples
        assert_eq!(decoded.samples.len(), 704);
    }

    #[test]
    fn eof_on_disconnect() {
        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        decoder.set_recv_timeout(Duration::from_millis(10));

        drop(sender);

        // Should get None and mark EOF
        let decoded = decoder.decode_frame().unwrap();
        assert!(decoded.is_none());
        assert!(decoder.is_eof());
    }

    mod attach {
        use super::*;

        fn frame(value: i16) -> LivePcmFrame {
            LivePcmFrame {
                samples: vec![value; 8],
                channels: 2,
                sample_rate: 44100,
            }
        }

        #[test]
        fn attached_handle_consumes_the_same_stable_queue() {
            let (sender, canonical) = LiveAudioDecoder::create_pair(44100, 2, 8);
            let mut attached = canonical.attach();
            attached.set_recv_timeout(Duration::from_millis(50));

            assert!(sender.send(frame(7)));

            let decoded = attached.decode_frame().unwrap().expect("frame arrives");
            assert_eq!(decoded.samples[0], 7);
            assert_eq!(attached.sample_rate(), 44100);
            assert_eq!(attached.channels(), 2);
        }

        #[test]
        fn dropping_an_attached_handle_keeps_the_pair_alive() {
            let (sender, canonical) = LiveAudioDecoder::create_pair(44100, 2, 8);

            drop(canonical.attach());

            assert!(
                sender.send(frame(1)),
                "the stable pair must survive a session-scoped handle"
            );
        }

        #[test]
        fn attaching_twice_serves_consecutive_session_generations() {
            let (sender, canonical) = LiveAudioDecoder::create_pair(44100, 2, 8);

            let first = canonical.attach();
            drop(first);
            let mut second = canonical.attach();
            second.set_recv_timeout(Duration::from_millis(50));

            assert!(sender.send(frame(3)));
            let decoded = second.decode_frame().unwrap().expect("frame arrives");
            assert_eq!(decoded.samples[0], 3);
        }

        #[test]
        fn dropping_the_canonical_handle_still_ends_the_pair() {
            let (sender, canonical) = LiveAudioDecoder::create_pair(44100, 2, 8);
            let _attached = canonical.attach();

            drop(canonical);

            assert!(
                !sender.send(frame(1)),
                "the pair latch belongs to the canonical handle alone"
            );
        }
    }

    #[test]
    fn duration_always_none() {
        let (_sender, decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        assert!(decoder.duration_samples().is_none());
    }

    #[test]
    fn seek_returns_error() {
        let (_sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        assert!(decoder.seek(1000).is_err());
    }

    /// Test that decoded frames can be encoded with ALAC encoder.
    #[test]
    fn full_encoder_pipeline() {
        use crate::encoder::create_encoder;

        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 16);
        decoder.set_recv_timeout(Duration::from_millis(50));

        let format = test_format();
        let frames_per_packet = format.frames_per_packet as usize;

        // Create ALAC encoder
        let mut encoder = create_encoder(format.clone()).expect("Failed to create encoder");

        // Send 20 periods of audio (enough for multiple packets)
        for _ in 0..20 {
            let samples = generate_sine_wave(440.0, 44100, 1024);
            let frame = LivePcmFrame {
                samples,
                channels: 2,
                sample_rate: 44100,
            };
            sender.try_send(frame);
        }

        // Decode and encode packets
        let mut encoded_count = 0;
        let mut total_encoded_bytes = 0;

        loop {
            match decoder.decode_resampled(&format, frames_per_packet) {
                Ok(Some(frame)) => {
                    // Verify we got the right number of samples
                    assert_eq!(frame.samples.len(), frames_per_packet * 2);

                    // Encode with ALAC
                    match encoder.encode(&frame.samples) {
                        Ok(packet) => {
                            assert!(packet.data.len() > 0, "Encoded packet is empty");
                            assert_eq!(packet.samples, frames_per_packet as u32);
                            total_encoded_bytes += packet.data.len();
                            encoded_count += 1;

                            // ALAC compression ratio is typically 40-60%
                            // Raw size = 352 * 2 channels * 2 bytes = 1408 bytes
                            // Expected encoded: 500-900 bytes
                            assert!(
                                packet.data.len() < 1408,
                                "Encoded packet too large: {} bytes (raw=1408)",
                                packet.data.len()
                            );
                        }
                        Err(e) => panic!("Encode failed: {}", e),
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("Decode error: {}", e),
            }
        }

        println!(
            "Full pipeline test: {} packets encoded, {} total bytes",
            encoded_count, total_encoded_bytes
        );

        // We sent 20 * 1024 = 20480 samples
        // Each packet is 352 samples
        // Expected: ~58 packets (but some may be lost to residual handling)
        assert!(
            encoded_count >= 45,
            "Expected at least 45 packets, got {}",
            encoded_count
        );
    }

    /// Test simulating real Bluetooth capture behavior.
    #[test]
    fn simulated_bluetooth_capture() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;

        let (sender, mut decoder) = LiveAudioDecoder::create_pair(44100, 2, 8);
        decoder.set_recv_timeout(Duration::from_millis(100));

        let format = test_format();
        let frames_per_packet = format.frames_per_packet as usize;

        // Simulate PipeWire capture thread
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let producer = thread::spawn(move || {
            let mut period = 0;
            while running_clone.load(Ordering::Relaxed) && period < 50 {
                // PipeWire sends 1024 frames per period
                let samples = generate_sine_wave(440.0, 44100, 1024);
                let frame = LivePcmFrame {
                    samples,
                    channels: 2,
                    sample_rate: 44100,
                };

                if !sender.try_send(frame) {
                    // Channel full, wait a bit
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }

                period += 1;

                // Simulate ~23ms period (1024 samples at 44100Hz)
                thread::sleep(Duration::from_millis(10));
            }
            println!("Producer sent {} periods", period);
        });

        // Consumer (simulating streamer decode loop)
        let mut decoded_count = 0;
        let start = std::time::Instant::now();

        while start.elapsed() < Duration::from_secs(2) {
            match decoder.decode_resampled(&format, frames_per_packet) {
                Ok(Some(frame)) => {
                    assert_eq!(frame.samples.len(), frames_per_packet * 2);
                    decoded_count += 1;
                }
                Ok(None) => {
                    // No data available, wait a bit
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("Decode error: {}", e),
            }

            if decoded_count >= 100 {
                break;
            }
        }

        running.store(false, Ordering::Relaxed);
        producer.join().unwrap();

        println!("Simulated capture test: decoded {} packets", decoded_count);
        assert!(
            decoded_count >= 40,
            "Expected at least 40 packets, got {}",
            decoded_count
        );
    }
}
