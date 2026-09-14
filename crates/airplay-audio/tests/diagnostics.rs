//! Integration tests for truthful sender-buffer and capture-queue diagnostics.
//!
//! Every number asserted here must come from real pipeline state:
//! monotonic counters, actual queue length, exact sample math.

use airplay_audio::{
    AudioBuffer, AudioDiagnosticsSnapshot, AudioFrame, CaptureQueueSnapshot, LiveAudioDecoder,
    LivePcmFrame, SenderBufferSnapshot,
};
use airplay_core::{AudioCodec, AudioFormat, SampleRate};

fn test_format() -> AudioFormat {
    AudioFormat {
        codec: AudioCodec::Alac,
        sample_rate: SampleRate::Hz44100,
        bit_depth: 16,
        channels: 2,
        frames_per_packet: 352,
    }
}

fn stereo_frame(timestamp: u64) -> AudioFrame {
    // 352 samples per channel, stereo interleaved.
    AudioFrame::new(vec![0i16; 352 * 2], timestamp)
}

fn pcm_frame() -> LivePcmFrame {
    LivePcmFrame {
        samples: vec![0i16; 352 * 2],
        channels: 2,
        sample_rate: 44_100,
    }
}

#[test]
fn sender_buffer_reflects_written_and_read_totals() {
    let mut buffer = AudioBuffer::new(test_format(), 1000);
    let source = buffer.diagnostics_source(44_100);

    let capacity = buffer.capacity() as u32;
    let snapshot = source.snapshot();
    assert_eq!(snapshot.sender_buffer.capacity_frames, capacity);
    assert_eq!(snapshot.sender_buffer.queued_frames, 0);
    assert_eq!(snapshot.sender_buffer.samples_written_total, 0);

    // Write N = 5 frames.
    for i in 0..5 {
        buffer.push(stereo_frame(i)).unwrap();
    }
    let snapshot = source.snapshot();
    assert_eq!(snapshot.sender_buffer.samples_written_total, 5 * 352);
    assert_eq!(snapshot.sender_buffer.queued_samples_per_channel, 5 * 352);
    assert_eq!(snapshot.sender_buffer.queued_frames, 5);
    assert!(snapshot.sender_buffer.buffered_ns > 0);

    // Read M = 2 frames.
    assert!(buffer.pop().is_some());
    assert!(buffer.pop().is_some());
    let snapshot = source.snapshot();
    assert_eq!(snapshot.sender_buffer.samples_written_total, 5 * 352);
    assert_eq!(snapshot.sender_buffer.samples_read_total, 2 * 352);
    assert_eq!(snapshot.sender_buffer.queued_frames, 3);
    assert_eq!(snapshot.sender_buffer.queued_samples_per_channel, 3 * 352);

    // Exact integer math: 3 * 352 samples at 44100 Hz.
    let expected_ns = (3u64 * 352 * 1_000_000_000) / 44_100;
    assert_eq!(snapshot.sender_buffer.buffered_ns, expected_ns);

    // fill_ratio is a real ratio in 0.0..=1.0.
    assert!(snapshot.sender_buffer.fill_ratio > 0.0);
    assert!(snapshot.sender_buffer.fill_ratio <= 1.0);
}

#[test]
fn sender_buffer_counts_underrun_events_truthfully() {
    // The underrun site inside AudioBuffer is `pop()` finding the ring empty
    // (it returns None). That path is reachable through the public API, so we
    // count exactly there: two empty pops => exactly 2 events.
    let mut buffer = AudioBuffer::new(test_format(), 1000);
    let source = buffer.diagnostics_source(44_100);

    assert_eq!(
        source.snapshot().sender_buffer.underrun_events_total,
        0,
        "no underruns before any read"
    );

    assert!(buffer.pop().is_none(), "read from empty buffer");
    assert!(buffer.pop().is_none(), "second read from empty buffer");
    assert_eq!(source.snapshot().sender_buffer.underrun_events_total, 2);

    // Normal operations must not inflate the counter.
    buffer.push(stereo_frame(0)).unwrap();
    assert!(buffer.pop().is_some());
    buffer.clear();
    assert_eq!(
        source.snapshot().sender_buffer.underrun_events_total,
        2,
        "successful reads and clear are not underruns"
    );
}

#[test]
fn capture_queue_counts_submissions_and_full_drops() {
    let (sender, _decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);
    let source = sender.diagnostics_source(44_100);

    // Two attempts fill the queue to capacity; both succeed.
    assert!(sender.try_send(pcm_frame()));
    assert!(sender.try_send(pcm_frame()));
    // Queue is now full; the next attempt must be rejected as Full.
    assert!(!sender.try_send(pcm_frame()));

    let snapshot = source.snapshot().capture_queue;
    assert_eq!(snapshot.submitted_frames_total, 3, "every attempt counts");
    assert_eq!(snapshot.full_queue_drops_total, 1, "exactly one rejection");
    assert_eq!(snapshot.disconnected_drops_total, 0);
    assert_eq!(snapshot.queued_frames, 2, "queue holds capacity frames");
    assert_eq!(snapshot.capacity_frames, 2);
    assert_eq!(
        snapshot.submitted_samples_per_channel_total,
        3 * 352,
        "samples of every attempted frame count"
    );
}

#[test]
fn capture_queue_counts_disconnected_drops() {
    let (sender, decoder) = LiveAudioDecoder::create_pair(44_100, 2, 2);
    drop(decoder); // disconnect the receiver side

    // Must not panic on a disconnected channel.
    let sent = sender.try_send(pcm_frame());
    assert!(!sent);

    let snapshot = source_of(&sender).capture_queue;
    assert!(snapshot.disconnected_drops_total >= 1);
    assert_eq!(snapshot.submitted_frames_total, 1);
    assert_eq!(snapshot.full_queue_drops_total, 0);
}

// Helper kept tiny so the assertion above reads cleanly.
fn source_of(sender: &airplay_audio::LiveFrameSender) -> AudioDiagnosticsSnapshot {
    sender.diagnostics_source(44_100).snapshot()
}

mod scheduler_jitter {
    use airplay_audio::{SchedulerJitterRecorder, SchedulerJitterSnapshot};

    #[test]
    fn scheduler_snapshot_percentile_math_is_exact() {
        // 18x +100ns, one +400ns, one -50ns => absolute samples
        // {100 x18, 400, 50}, sum 2250.
        let rec = SchedulerJitterRecorder::new();
        for _ in 0..18 {
            rec.record_jitter(100);
        }
        rec.record_jitter(400);
        rec.record_jitter(-50);

        let snap = rec.snapshot();
        assert_eq!(snap.sample_count, 20);
        assert_eq!(snap.last_signed_jitter_ns, Some(-50));
        assert_eq!(
            snap.mean_absolute_jitter_ns,
            Some(112),
            "(18*100+400+50)/20 = 112.5, integer mean truncates to 112"
        );
        assert_eq!(snap.maximum_absolute_jitter_ns, Some(400));
        assert_eq!(
            snap.p95_absolute_jitter_ns,
            Some(100),
            "rank ceil(0.95*20)=19 of sorted [50, 100 x18, 400] is 100"
        );
        assert_eq!(snap.deadline_resets_total, 0);

        // Empty recorder: every optional statistic stays None, no invention.
        let empty = SchedulerJitterRecorder::new();
        assert_eq!(empty.snapshot(), SchedulerJitterSnapshot::default());
        let s0 = empty.snapshot();
        assert_eq!(s0.last_signed_jitter_ns, None);
        assert_eq!(s0.mean_absolute_jitter_ns, None);
        assert_eq!(s0.p95_absolute_jitter_ns, None);
        assert_eq!(s0.maximum_absolute_jitter_ns, None);

        // doc(hidden) test constructor builds an equivalent recorder.
        let built = SchedulerJitterRecorder::from_samples(&[10, -30]);
        let sb = built.snapshot();
        assert_eq!(sb.sample_count, 2);
        assert_eq!(sb.last_signed_jitter_ns, Some(-30));
        assert_eq!(sb.mean_absolute_jitter_ns, Some(20));
        assert_eq!(
            sb.p95_absolute_jitter_ns,
            Some(30),
            "rank ceil(1.9)=2 of [10,30]"
        );
        assert_eq!(sb.maximum_absolute_jitter_ns, Some(30));
    }

    #[test]
    fn scheduler_ring_bounds_at_1024_and_counts_resets() {
        let rec = SchedulerJitterRecorder::new();
        // First 476 samples are huge, the remaining 1024 are small: the ring
        // must have forgotten every huge value once 1500 samples were pushed.
        let jitter_at = |i: u64| -> i64 {
            if i < 476 {
                1_000_000 + i as i64
            } else {
                ((i % 997) as i64) + 1
            }
        };
        for i in 0..1500u64 {
            rec.record_jitter(jitter_at(i));
        }
        for _ in 0..3 {
            rec.record_deadline_reset();
        }

        let snap = rec.snapshot();
        assert_eq!(snap.sample_count, 1503, "every recorded event counts");
        assert_eq!(snap.deadline_resets_total, 3);
        assert_eq!(
            snap.maximum_absolute_jitter_ns,
            Some(997),
            "ring-derived max covers only the LAST 1024 jitters (idx 476..1500)"
        );
        assert_ne!(
            snap.maximum_absolute_jitter_ns,
            Some(1_000_000_475),
            "all-time max must NOT leak through the bounded ring"
        );
        assert_eq!(snap.last_signed_jitter_ns, Some(jitter_at(1499)));
    }
}

mod transport_targets {
    use airplay_audio::{RtpSender, TargetTransportCounters};
    use std::net::UdpSocket;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn target_counters_count_attempts_accepts_failures_bytes() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let dest = listener.local_addr().unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let counters = Arc::new(TargetTransportCounters::default());
        let mut sender = RtpSender::new(dest, 0xABCD);
        sender.set_control_dest(dest);
        sender.bind(0).unwrap();
        sender.set_transport_counters(Arc::clone(&counters));

        const PAYLOAD: usize = 8;
        for seq in 0..3u16 {
            sender
                .send_audio(96, u32::from(seq) * 352, &[0u8; PAYLOAD], false)
                .unwrap();
        }
        sender.send_sync(352, 1_000).unwrap();
        sender.send_sync(704, 2_000).unwrap();

        let snap = counters.snapshot();
        assert_eq!(snap.data_datagrams_attempted_total, 3);
        assert_eq!(snap.data_datagrams_accepted_local_total, 3);
        assert_eq!(snap.data_send_failures_total, 0);
        assert_eq!(
            snap.data_bytes_sent_total,
            3 * (12 + PAYLOAD) as u64,
            "unencrypted wire length = header + payload"
        );
        assert_eq!(snap.sync_datagrams_attempted_total, 2);
        assert_eq!(snap.sync_datagrams_accepted_local_total, 2);
        assert_eq!(snap.sync_send_failures_total, 0);
        assert_eq!(snap.sync_bytes_sent_total, 2 * 20);

        // The datagrams really reached the wire (accepted_local is truthful):
        let mut buf = [0u8; 64];
        for _ in 0..3 {
            let (len, _) = listener.recv_from(&mut buf).unwrap();
            assert_eq!(len, 12 + PAYLOAD);
        }
        for _ in 0..2 {
            let (len, _) = listener.recv_from(&mut buf).unwrap();
            assert_eq!(len, 20);
        }

        // Buffered mode skips sync entirely (control port 0) BEFORE any send
        // attempt: nothing may be counted.
        let skip_counters = Arc::new(TargetTransportCounters::default());
        let mut buffered_mode = RtpSender::new("127.0.0.1:0".parse().unwrap(), 7);
        buffered_mode.set_transport_counters(Arc::clone(&skip_counters));
        buffered_mode.send_sync(0, 0).unwrap();
        let snap = skip_counters.snapshot();
        assert_eq!(
            snap.sync_datagrams_attempted_total, 0,
            "port-0 skip is not a datagram attempt"
        );
    }
}

mod streamer_telemetry {
    use airplay_audio::{AudioStreamer, SchedulerJitterSnapshot};
    use airplay_core::StreamConfig;

    #[test]
    fn frames_prepared_total_matches_streamer_counter() {
        let streamer = AudioStreamer::new(StreamConfig::default());
        let source = streamer.diagnostics_source_with_transport(44_100);
        let snap = source.snapshot();

        assert_eq!(
            snap.rtp_frames_prepared_total,
            streamer.packets_sent(),
            "frames_prepared mirrors the same atomic as packets_sent()"
        );
        assert_eq!(snap.rtp_frames_prepared_total, 0);
        assert!(snap.targets.is_empty(), "no targets registered yet");
        assert_eq!(snap.scheduler, SchedulerJitterSnapshot::default());

        // The buffer half stays live (streamer owns its AudioBuffer):
        // 2000ms at 44100 Hz = 88200 samples => floor(88200/352) = 250 frames.
        assert_eq!(snap.sender_buffer.capacity_frames, 250);
        assert_eq!(
            snap.capture_queue,
            airplay_audio::CaptureQueueSnapshot {
                queued_frames: 0,
                capacity_frames: 0,
                submitted_frames_total: 0,
                submitted_samples_per_channel_total: 0,
                full_queue_drops_total: 0,
                disconnected_drops_total: 0,
            },
            "streamer has no capture queue: half stays zeroed"
        );
    }
}

mod retransmit_outcomes {
    use airplay_audio::{
        AudioStreamer, RetransmitOutcome, RetransmitRequest, RetransmitSnapshot, RtpSender,
    };
    use airplay_core::StreamConfig;
    use std::net::UdpSocket;
    use std::time::Duration;

    #[tokio::test]
    async fn retransmit_outcome_classifies_history_hits_and_misses() {
        // Localhost receiver like the T4 transport tests: retransmit responses
        // go to the control dest and must really reach this socket.
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let dest = listener.local_addr().unwrap();

        let mut sender = RtpSender::new(dest, 0xABCD);
        sender.set_control_dest(dest);
        sender.bind(0).unwrap();

        // Fill history with sequences 0..=2 (unencrypted wire packets).
        for seq in 0..3u16 {
            sender
                .prepare_audio(96, u32::from(seq) * 352, &[0u8; 8], false)
                .unwrap();
        }

        let mut streamer = AudioStreamer::new(StreamConfig::default());
        streamer.set_rtp_sender(sender).await;
        let source = streamer.diagnostics_source_with_transport(44_100);

        // Sequences 1 and 2 are in history; 3 was never sent => exactly one
        // miss. Requested slots are never deduplicated.
        let request = RetransmitRequest {
            first_sequence: 1,
            count: 3,
        };
        let outcome = streamer
            .handle_retransmit_for_target(0, &request)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            RetransmitOutcome {
                requested_slots: 3,
                accepted_local: 2,
                history_misses: 1,
                send_failures: 0,
            }
        );

        // The two served responses really reached the local receiver:
        // PT=86 header (4 bytes) + original RTP packet (12 + 8 bytes).
        let mut buf = [0u8; 128];
        for _ in 0..2 {
            let (len, _) = listener.recv_from(&mut buf).unwrap();
            assert_eq!(len, 4 + 20);
        }

        // Cumulative snapshot counters match the returned outcome.
        assert_eq!(
            source.snapshot().retransmit,
            RetransmitSnapshot {
                retransmit_request_datagrams_total: 1,
                retransmit_packet_slots_requested_total: 3,
                retransmit_datagrams_accepted_local_total: 2,
                retransmit_history_misses_total: 1,
                retransmit_send_failures_total: 0,
            }
        );
    }

    #[tokio::test]
    async fn retransmit_send_failures_count_when_receiver_socket_closes() {
        // Deterministic send-failure fixture: the target has no usable
        // transport (no bound socket, no reachable peer), so serving a
        // history-present slot can never complete a local send. A synchronous
        // UDP Err from a dropped-but-previously-bound loopback peer is not
        // reliably reproducible across platforms (Windows swallows it), so the
        // transport-unavailable state stands in for "peer closed".
        let mut sender = RtpSender::new("127.0.0.1:9".parse().unwrap(), 7);
        for seq in 0..2u16 {
            sender
                .prepare_audio(96, u32::from(seq) * 352, &[0u8; 8], false)
                .unwrap();
        }

        let mut streamer = AudioStreamer::new(StreamConfig::default());
        streamer.set_rtp_sender(sender).await;
        let source = streamer.diagnostics_source_with_transport(44_100);

        let request = RetransmitRequest {
            first_sequence: 0,
            count: 2,
        };
        let outcome = streamer
            .handle_retransmit_for_target(0, &request)
            .await
            .unwrap();
        assert_eq!(outcome.requested_slots, 2);
        assert_eq!(
            outcome.accepted_local, 0,
            "nothing was accepted without a working transport"
        );
        assert_eq!(outcome.history_misses, 0, "both slots are in history");
        assert!(
            outcome.send_failures >= 1,
            "every served slot must report its failed send"
        );

        let snap = source.snapshot().retransmit;
        assert_eq!(snap.retransmit_request_datagrams_total, 1);
        assert_eq!(snap.retransmit_packet_slots_requested_total, 2);
        assert_eq!(snap.retransmit_datagrams_accepted_local_total, 0);
        assert_eq!(snap.retransmit_history_misses_total, 0);
        assert_eq!(
            snap.retransmit_send_failures_total,
            u64::from(outcome.send_failures),
            "snapshot reflects the same failures as the outcome"
        );
    }

    #[test]
    fn snapshot_exposes_cumulative_retransmit_counters() {
        // Before any activity every counter stays at its truthful zero value.
        let streamer = AudioStreamer::new(StreamConfig::default());
        let source = streamer.diagnostics_source_with_transport(44_100);
        assert_eq!(source.snapshot().retransmit, RetransmitSnapshot::default());
    }
}

#[test]
fn snapshots_default_halves_are_zero_not_fabricated() {
    // Buffer-backed source: capture half must be all-zero ("not applicable").
    let mut buffer = AudioBuffer::new(test_format(), 500);
    buffer.push(stereo_frame(0)).unwrap();
    let snapshot = buffer.diagnostics_source(44_100).snapshot();
    assert!(snapshot.sender_buffer.queued_frames > 0);
    assert_eq!(
        snapshot.capture_queue,
        CaptureQueueSnapshot {
            queued_frames: 0,
            capacity_frames: 0,
            submitted_frames_total: 0,
            submitted_samples_per_channel_total: 0,
            full_queue_drops_total: 0,
            disconnected_drops_total: 0,
        }
    );

    // Sender-backed source: sender-buffer half must be all-zero.
    let (sender, _decoder) = LiveAudioDecoder::create_pair(44_100, 2, 4);
    sender.try_send(pcm_frame());
    let snapshot = source_of(&sender);
    assert!(snapshot.capture_queue.queued_frames > 0);
    assert_eq!(
        snapshot.sender_buffer,
        SenderBufferSnapshot {
            queued_frames: 0,
            capacity_frames: 0,
            queued_samples_per_channel: 0,
            buffered_ns: 0,
            fill_ratio: 0.0,
            samples_written_total: 0,
            samples_read_total: 0,
            underrun_events_total: 0,
        }
    );
}
