//! # airplay-audio
//!
//! Audio encoding and RTP streaming for AirPlay 2.
//!
//! This crate provides:
//! - Audio decoding from various formats (via symphonia)
//! - Live audio streaming from external sources (e.g., Bluetooth)
//! - ALAC encoding for realtime streaming
//! - AAC encoding for buffered streaming
//! - RTP packet formatting and transmission
//! - Audio buffer management
//! - Retransmission handling

mod buffer;
pub mod cipher;
mod decoder;
pub mod diagnostics;
mod encoder;
pub mod eq;
mod live_decoder;
mod rtp;
mod streamer;
mod traits;

pub use buffer::{AudioBuffer, AudioFrame};
pub use decoder::{AudioDecoder, DecodedFrame};
pub use diagnostics::{
    AudioDiagnosticsSnapshot, AudioDiagnosticsSource, CaptureQueueSnapshot, RetransmitOutcome,
    RetransmitSnapshot, SchedulerJitterRecorder, SchedulerJitterSnapshot, SenderBufferSnapshot,
    TargetTransportCounters, TargetTransportSnapshot,
};
#[cfg(feature = "aac")]
pub use encoder::AacEncoder;
pub use encoder::{create_encoder, AlacEncoder, AudioEncoder, EncodedPacket};
pub use eq::{EqConfig, EqParams, Equalizer};
pub use live_decoder::{LiveAudioDecoder, LiveFrameSender, LivePcmFrame, LiveSendOutcome};
pub use rtp::{
    build_retransmit_response, RetransmitRequest, RtpHeader, RtpPacket, RtpReceiver, RtpSender,
};
pub use streamer::{
    checked_presentation_time_ns, prepare_frame_for_targets, AudioStreamer, FramePlan,
    PrepareFrameError, PreparedFrame, PresentationTimeError,
};
pub use traits::{AudioSource, EncoderTrait};
