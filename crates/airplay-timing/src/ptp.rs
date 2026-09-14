//! PTP (IEEE 1588) timing for AirPlay 2.
//!
//! Uses UDP ports 319 (event) and 320 (general).
//! Provides sub-millisecond synchronization for multi-room audio.

use crate::diagnostics::{local_to_master_ns, ptp_measurement, TimingDiagnosticsRecorder};
use crate::{Clock, ClockOffset, TimingProtocol};
use airplay_core::error::{Error, Result};
use async_trait::async_trait;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// PTP event port (IEEE 1588).
pub const PTP_EVENT_PORT: u16 = 319;

/// PTP general port (IEEE 1588).
pub const PTP_GENERAL_PORT: u16 = 320;

/// Path delay a one-way offset estimate cannot see.
///
/// Declared, not assumed away: one link traversal on a home network, which is
/// the whole error of the estimate published when a gPTP grandmaster leaves a
/// Delay_Req unanswered.
const UNMEASURED_PATH_DELAY_NS: u64 = 500_000;

/// The offset a sender can state without a round trip.
///
/// `t1` is the master's own send time, carried in its Follow_Up; `t2` is when
/// the matching Sync arrived here. Their difference is the offset plus one
/// link traversal, and the traversal is what a real delay measurement would
/// subtract.
///
/// It exists because AirPlay 2 speaks gPTP, whose delay measurement is the
/// peer mechanism, while this module asks with the end-to-end `Delay_Req` that
/// a gPTP grandmaster ignores -- measured: 156 requests, no answers. Waiting
/// for `t4` therefore meant waiting forever and leaving the offset at zero,
/// which is not a small error: a HomePod's PTP clock counts from its own boot,
/// so a sender stamping its own Unix time under the master's clock identity
/// hands out presentation times decades in the future, and every receiver in
/// the group drops the audio. Both speakers stayed silent, the primary
/// included, because the secondary mirrors the primary's offset.
fn one_way_offset(t1: PtpTimestamp, t2: PtpTimestamp) -> ClockOffset {
    let offset_ns = t2.to_nanos() as i128 - t1.to_nanos() as i128;
    ClockOffset {
        offset_ns: offset_ns as i64,
        error_ns: UNMEASURED_PATH_DELAY_NS,
        rtt_ns: 0,
    }
}

/// One complete, measured view of the active PTP clock domain.
///
/// The clock identity and offset are deliberately published together: using
/// either field from a different generation creates an invalid PT=87 packet.
#[derive(Debug, Clone, Copy)]
pub struct PtpClockState {
    /// Grandmaster identity advertised by the selected relay.
    pub master_clock_id: [u8; 8],
    /// Local-minus-master offset measured from a matched Sync/Follow_Up pair.
    pub offset: ClockOffset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackerPublication {
    Unchanged,
    Invalidate,
}

#[derive(Debug, Clone, Copy)]
struct AnnouncedClock {
    grandmaster_id: [u8; 8],
    relay_port_identity: [u8; 10],
    generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingSync {
    sequence_id: u16,
    relay_port_identity: [u8; 10],
    generation: u64,
    received_at: PtpTimestamp,
}

#[derive(Debug, Clone, Copy)]
struct CompletedSync {
    generation: u64,
    master_sent_at: PtpTimestamp,
    local_received_at: PtpTimestamp,
}

#[derive(Debug, Clone, Copy)]
struct PendingDelay {
    sequence_id: u16,
    requesting_port_identity: [u8; 10],
    generation: u64,
    local_sent_at: PtpTimestamp,
    sync: CompletedSync,
}

/// Pure generation-aware matcher used by the BMCA socket loop.
struct PtpMeasurementTracker {
    local_port_identity: [u8; 10],
    announced: Option<AnnouncedClock>,
    pending_sync: Option<PendingSync>,
    completed_sync: Option<CompletedSync>,
    pending_delay: Option<PendingDelay>,
    next_generation: u64,
}

impl PtpMeasurementTracker {
    fn new(local_port_identity: [u8; 10]) -> Self {
        Self {
            local_port_identity,
            announced: None,
            pending_sync: None,
            completed_sync: None,
            pending_delay: None,
            next_generation: 1,
        }
    }

    fn observe_announce(&mut self, header: &PtpHeader, packet: &[u8]) -> TrackerPublication {
        if header.message_type != PtpMessageType::Announce || packet.len() < 61 {
            return TrackerPublication::Unchanged;
        }

        let grandmaster_id: [u8; 8] = packet[53..61].try_into().expect("length checked");
        if grandmaster_id == [0; 8] || header.source_port_identity[..8] == [0; 8] {
            return TrackerPublication::Unchanged;
        }

        if self.announced.is_some_and(|current| {
            current.grandmaster_id == grandmaster_id
                && current.relay_port_identity == header.source_port_identity
        }) {
            return TrackerPublication::Unchanged;
        }

        let invalidates_ready_generation = self.announced.is_some();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.announced = Some(AnnouncedClock {
            grandmaster_id,
            relay_port_identity: header.source_port_identity,
            generation,
        });
        self.pending_sync = None;
        self.completed_sync = None;
        self.pending_delay = None;

        if invalidates_ready_generation {
            TrackerPublication::Invalidate
        } else {
            TrackerPublication::Unchanged
        }
    }

    fn observe_sync(&mut self, header: &PtpHeader, packet: &[u8], received_at: PtpTimestamp) {
        if header.message_type != PtpMessageType::Sync || packet.len() < 34 {
            return;
        }
        let Some(announced) = self.announced else {
            return;
        };
        if header.source_port_identity != announced.relay_port_identity {
            return;
        }
        self.pending_sync = Some(PendingSync {
            sequence_id: header.sequence_id,
            relay_port_identity: header.source_port_identity,
            generation: announced.generation,
            received_at,
        });
    }

    fn observe_follow_up(&mut self, header: &PtpHeader, packet: &[u8]) -> Option<PtpClockState> {
        if header.message_type != PtpMessageType::FollowUp || packet.len() < 44 {
            return None;
        }
        let announced = self.announced?;
        let pending = self.pending_sync?;
        if header.sequence_id != pending.sequence_id
            || header.source_port_identity != pending.relay_port_identity
            || header.source_port_identity != announced.relay_port_identity
            || pending.generation != announced.generation
        {
            return None;
        }
        let master_sent_at = PtpTimestamp::parse(&packet[34..44]).ok()?;
        self.pending_sync = None;
        let completed = CompletedSync {
            generation: announced.generation,
            master_sent_at,
            local_received_at: pending.received_at,
        };
        self.completed_sync = Some(completed);
        Some(PtpClockState {
            master_clock_id: announced.grandmaster_id,
            offset: one_way_offset(master_sent_at, pending.received_at),
        })
    }

    fn record_delay_request(
        &mut self,
        sequence_id: u16,
        requesting_port_identity: [u8; 10],
        local_sent_at: PtpTimestamp,
    ) {
        let Some(announced) = self.announced else {
            return;
        };
        let Some(sync) = self.completed_sync else {
            return;
        };
        if requesting_port_identity != self.local_port_identity
            || sync.generation != announced.generation
        {
            return;
        }
        self.pending_delay = Some(PendingDelay {
            sequence_id,
            requesting_port_identity,
            generation: announced.generation,
            local_sent_at,
            sync,
        });
    }

    fn observe_delay_response(
        &mut self,
        header: &PtpHeader,
        packet: &[u8],
    ) -> Option<PtpClockState> {
        if header.message_type != PtpMessageType::DelayResp || packet.len() < 54 {
            return None;
        }
        let announced = self.announced?;
        let pending = self.pending_delay?;
        if header.sequence_id != pending.sequence_id
            || header.source_port_identity != announced.relay_port_identity
            || packet[44..54] != pending.requesting_port_identity
            || pending.generation != announced.generation
            || pending.sync.generation != announced.generation
        {
            return None;
        }

        let master_received_at = PtpTimestamp::parse(&packet[34..44]).ok()?;
        let t1_ns = pending.sync.master_sent_at.to_nanos() as i128;
        let t2_ns = pending.sync.local_received_at.to_nanos() as i128;
        let t3_ns = pending.local_sent_at.to_nanos() as i128;
        let t4_ns = master_received_at.to_nanos() as i128;
        let offset_ns = ((t2_ns - t1_ns) + (t3_ns - t4_ns)) / 2;
        let delay_ns = ((t2_ns - t1_ns) - (t3_ns - t4_ns)) / 2;
        self.pending_delay = None;
        Some(PtpClockState {
            master_clock_id: announced.grandmaster_id,
            offset: ClockOffset {
                offset_ns: offset_ns as i64,
                error_ns: (delay_ns.abs() / 2) as u64,
                rtt_ns: delay_ns.unsigned_abs() as u64,
            },
        })
    }
}

fn publish_clock_state(
    tx: &tokio::sync::watch::Sender<Option<PtpClockState>>,
    state: Option<PtpClockState>,
) {
    match state {
        Some(state) => tracing::debug!(
            "BMCA slave: publishing coherent clock master={:02x?}, offset={}ns",
            state.master_clock_id,
            state.offset.offset_ns
        ),
        None => tracing::debug!("BMCA slave: publishing clock transition (not ready)"),
    }
    tx.send_replace(state);
}

/// PTP message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PtpMessageType {
    Sync = 0x00,
    DelayReq = 0x01,
    FollowUp = 0x08,
    DelayResp = 0x09,
    Announce = 0x0B,
    Signaling = 0x0C, // gPTP (802.1AS) signaling
}

impl PtpMessageType {
    /// Parse from byte.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b & 0x0F {
            0x00 => Some(Self::Sync),
            0x01 => Some(Self::DelayReq),
            0x08 => Some(Self::FollowUp),
            0x09 => Some(Self::DelayResp),
            0x0B => Some(Self::Announce),
            0x0C => Some(Self::Signaling),
            _ => None,
        }
    }
}

// ============================================================================
// TLV (Type-Length-Value) Support for gPTP (802.1AS)
// ============================================================================

/// TLV Type values
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TlvType {
    /// Organization extension TLV (used for 802.1AS and Apple extensions)
    OrganizationExtension = 0x0003,
}

/// Organization IDs for TLVs
pub mod org_id {
    /// 802.1AS (gPTP) organization ID
    pub const GPTP: [u8; 3] = [0x00, 0x80, 0xC2];
    /// Apple Inc. organization ID
    pub const APPLE: [u8; 3] = [0x00, 0x0D, 0x93];
}

/// 802.1AS organization subtypes
pub mod gptp_subtype {
    /// Follow-Up Information TLV (in Follow_Up messages)
    pub const FOLLOW_UP_INFO: [u8; 3] = [0x00, 0x00, 0x01];
    /// Message Interval Request TLV (in Signaling messages)
    pub const MESSAGE_INTERVAL_REQUEST: [u8; 3] = [0x00, 0x00, 0x02];
}

/// Apple proprietary subtypes (observed in pcap)
pub mod apple_subtype {
    pub const TYPE_01: [u8; 3] = [0x00, 0x00, 0x01];
    pub const TYPE_04: [u8; 3] = [0x00, 0x00, 0x04];
    pub const TYPE_05: [u8; 3] = [0x00, 0x00, 0x05];
}

/// Base TLV header (4 bytes: type + length)
#[derive(Debug, Clone)]
pub struct TlvHeader {
    pub tlv_type: u16,
    pub length: u16,
}

impl TlvHeader {
    pub fn serialize(&self) -> [u8; 4] {
        let mut buf = [0u8; 4];
        buf[0..2].copy_from_slice(&self.tlv_type.to_be_bytes());
        buf[2..4].copy_from_slice(&self.length.to_be_bytes());
        buf
    }
}

/// Follow-Up Information TLV (802.1AS-2011 Section 10.6.4.4.3)
/// Sent in Follow_Up messages to provide rate ratio and timing metadata
#[derive(Debug, Clone)]
pub struct FollowUpInformationTlv {
    /// Organization ID (0x0080C2 for 802.1AS)
    pub organization_id: [u8; 3],
    /// Organization subtype (0x000001 for Follow-Up Info)
    pub organization_subtype: [u8; 3],
    /// Cumulative rate offset scaled by 2^41
    pub cumulative_scaled_rate_offset: i32,
    /// GM time base indicator
    pub gm_time_base_indicator: u16,
    /// Last GM phase change (12 bytes)
    pub last_gm_phase_change: [u8; 12],
    /// Scaled last GM frequency change
    pub scaled_last_gm_freq_change: i32,
}

impl FollowUpInformationTlv {
    pub fn new() -> Self {
        Self {
            organization_id: org_id::GPTP,
            organization_subtype: gptp_subtype::FOLLOW_UP_INFO,
            cumulative_scaled_rate_offset: 0,
            gm_time_base_indicator: 0,
            last_gm_phase_change: [0; 12],
            scaled_last_gm_freq_change: 0,
        }
    }

    /// Serialize to bytes (4-byte header + 28-byte payload)
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);

        // TLV header
        let header = TlvHeader {
            tlv_type: TlvType::OrganizationExtension as u16,
            length: 28, // Payload length
        };
        buf.extend_from_slice(&header.serialize());

        // Payload
        buf.extend_from_slice(&self.organization_id);
        buf.extend_from_slice(&self.organization_subtype);
        buf.extend_from_slice(&self.cumulative_scaled_rate_offset.to_be_bytes());
        buf.extend_from_slice(&self.gm_time_base_indicator.to_be_bytes());
        buf.extend_from_slice(&self.last_gm_phase_change);
        buf.extend_from_slice(&self.scaled_last_gm_freq_change.to_be_bytes());

        buf
    }
}

/// Message Interval Request TLV (802.1AS-2011 Section 10.6.4.3.1)
/// Sent in Signaling messages to negotiate timing intervals
#[derive(Debug, Clone)]
pub struct MessageIntervalRequestTlv {
    /// Organization ID (0x0080C2 for 802.1AS)
    pub organization_id: [u8; 3],
    /// Organization subtype (0x000002 for Message Interval Request)
    pub organization_subtype: [u8; 3],
    /// Link delay interval (log base 2)
    pub link_delay_interval: i8,
    /// Time sync interval (log base 2)
    pub time_sync_interval: i8,
    /// Announce interval (log base 2)
    pub announce_interval: i8,
    /// Flags (bit 1: computeNeighborRateRatio)
    pub flags: u8,
}

impl MessageIntervalRequestTlv {
    pub fn new(sync_interval: i8, announce_interval: i8) -> Self {
        Self {
            organization_id: org_id::GPTP,
            organization_subtype: gptp_subtype::MESSAGE_INTERVAL_REQUEST,
            link_delay_interval: sync_interval, // Use same as sync for now
            time_sync_interval: sync_interval,
            announce_interval,
            flags: 0x02, // computeNeighborRateRatio bit set
        }
    }

    /// Serialize to bytes (4-byte header + 10-byte payload)
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(14);

        // TLV header
        let header = TlvHeader {
            tlv_type: TlvType::OrganizationExtension as u16,
            length: 10, // Payload length
        };
        buf.extend_from_slice(&header.serialize());

        // Payload
        buf.extend_from_slice(&self.organization_id);
        buf.extend_from_slice(&self.organization_subtype);
        buf.push(self.link_delay_interval as u8);
        buf.push(self.time_sync_interval as u8);
        buf.push(self.announce_interval as u8);
        buf.push(self.flags);

        buf
    }
}

/// Apple proprietary TLV (observed in pcap, exact format TBD)
/// These appear in Follow_Up and Signaling messages
#[derive(Debug, Clone)]
pub struct AppleTlv {
    /// Organization ID (0x000D93 for Apple)
    pub organization_id: [u8; 3],
    /// Organization subtype (0x01, 0x04, or 0x05)
    pub organization_subtype: [u8; 3],
    /// Variable-length payload (Apple-specific metadata)
    pub payload: Vec<u8>,
}

impl AppleTlv {
    pub fn new(subtype: [u8; 3], payload: Vec<u8>) -> Self {
        Self {
            organization_id: org_id::APPLE,
            organization_subtype: subtype,
            payload,
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // TLV header
        let header = TlvHeader {
            tlv_type: TlvType::OrganizationExtension as u16,
            length: (6 + self.payload.len()) as u16,
        };
        buf.extend_from_slice(&header.serialize());

        // Payload
        buf.extend_from_slice(&self.organization_id);
        buf.extend_from_slice(&self.organization_subtype);
        buf.extend_from_slice(&self.payload);

        buf
    }

    /// Parse AppleTlv from TLV data (assumes TLV type/length already parsed)
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 6 {
            return Err(
                std::io::Error::new(std::io::ErrorKind::InvalidData, "AppleTlv too short").into(),
            );
        }

        let mut organization_id = [0u8; 3];
        organization_id.copy_from_slice(&data[0..3]);

        let mut organization_subtype = [0u8; 3];
        organization_subtype.copy_from_slice(&data[3..6]);

        let payload = data[6..].to_vec();

        Ok(Self {
            organization_id,
            organization_subtype,
            payload,
        })
    }
}

/// PTP timestamp (10 bytes: 6 bytes seconds + 4 bytes nanoseconds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PtpTimestamp {
    /// Seconds (48-bit, upper 16 bits of u64 unused).
    pub seconds: u64,
    /// Nanoseconds (32-bit).
    pub nanoseconds: u32,
}

impl PtpTimestamp {
    /// Create timestamp for current time.
    pub fn now() -> Self {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self::from_nanos(ns)
    }

    /// Create from nanoseconds since epoch.
    pub fn from_nanos(nanos: u128) -> Self {
        Self {
            seconds: (nanos / 1_000_000_000) as u64,
            nanoseconds: (nanos % 1_000_000_000) as u32,
        }
    }

    /// Convert to nanoseconds since epoch.
    pub fn to_nanos(&self) -> u128 {
        (self.seconds as u128) * 1_000_000_000 + (self.nanoseconds as u128)
    }

    /// Serialize to 10 bytes (48-bit seconds BE + 32-bit nanos BE).
    pub fn serialize(&self) -> [u8; 10] {
        let mut buf = [0u8; 10];
        // 48-bit seconds (6 bytes, big-endian) - take lower 48 bits
        let sec_bytes = self.seconds.to_be_bytes();
        buf[0..6].copy_from_slice(&sec_bytes[2..8]);
        // 32-bit nanoseconds
        buf[6..10].copy_from_slice(&self.nanoseconds.to_be_bytes());
        buf
    }

    /// Parse from 10 bytes.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 10 {
            return Err(Error::Parse(
                airplay_core::error::ParseError::InvalidFormat("PTP timestamp too short".into()),
            ));
        }

        let mut sec_bytes = [0u8; 8];
        sec_bytes[2..8].copy_from_slice(&data[0..6]);
        let seconds = u64::from_be_bytes(sec_bytes);

        let nanoseconds = u32::from_be_bytes(data[6..10].try_into().unwrap());

        Ok(Self {
            seconds,
            nanoseconds,
        })
    }
}

/// PTP header (34 bytes).
#[derive(Debug, Clone)]
pub struct PtpHeader {
    pub message_type: PtpMessageType,
    pub version: u8,
    pub message_length: u16,
    pub domain_number: u8,
    pub flags: u16,
    pub correction_field: i64,
    pub source_port_identity: [u8; 10],
    pub sequence_id: u16,
    pub control_field: u8,
    pub log_message_interval: i8,
}

impl PtpHeader {
    /// Create new header for message type.
    pub fn new(message_type: PtpMessageType, sequence_id: u16) -> Self {
        Self {
            message_type,
            version: 2,
            message_length: 44, // Header (34) + timestamp (10)
            domain_number: 0,
            flags: 0,
            correction_field: 0,
            source_port_identity: [0; 10],
            sequence_id,
            control_field: match message_type {
                PtpMessageType::Sync => 0,
                PtpMessageType::DelayReq => 1,
                PtpMessageType::FollowUp => 2,
                PtpMessageType::DelayResp => 3,
                PtpMessageType::Announce => 5,
                PtpMessageType::Signaling => 5, // gPTP uses 5 for Signaling
            },
            log_message_interval: 0,
        }
    }

    /// Serialize to 34 bytes.
    pub fn serialize(&self) -> [u8; 34] {
        let mut buf = [0u8; 34];

        buf[0] = self.message_type as u8;
        buf[1] = self.version;
        buf[2..4].copy_from_slice(&self.message_length.to_be_bytes());
        buf[4] = self.domain_number;
        buf[5] = 0; // Reserved
        buf[6..8].copy_from_slice(&self.flags.to_be_bytes());
        buf[8..16].copy_from_slice(&self.correction_field.to_be_bytes());
        buf[16..20].fill(0); // Reserved
        buf[20..30].copy_from_slice(&self.source_port_identity);
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());
        buf[32] = self.control_field;
        buf[33] = self.log_message_interval as u8;

        buf
    }

    /// Parse from bytes.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 34 {
            return Err(Error::Parse(
                airplay_core::error::ParseError::InvalidFormat("PTP header too short".into()),
            ));
        }

        let message_type = PtpMessageType::from_byte(data[0]).ok_or_else(|| {
            Error::Parse(airplay_core::error::ParseError::InvalidFormat(format!(
                "Unknown PTP message type: {}",
                data[0] & 0x0F
            )))
        })?;

        Ok(Self {
            message_type,
            version: data[1] & 0x0F,
            message_length: u16::from_be_bytes([data[2], data[3]]),
            domain_number: data[4],
            flags: u16::from_be_bytes([data[6], data[7]]),
            correction_field: i64::from_be_bytes(data[8..16].try_into().unwrap()),
            source_port_identity: data[20..30].try_into().unwrap(),
            sequence_id: u16::from_be_bytes([data[30], data[31]]),
            control_field: data[32],
            log_message_interval: data[33] as i8,
        })
    }
}

/// PTP client state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtpState {
    /// Not synchronized.
    Unsynchronized,
    /// Synchronizing.
    Synchronizing,
    /// Synchronized with master.
    Synchronized,
}

/// PTP client for AirPlay 2.
pub struct PtpClient {
    master_addr: SocketAddr,
    state: PtpState,
    event_socket: Option<UdpSocket>,
    general_socket: Option<UdpSocket>,
    clock: Clock,
    sequence_id: u16,

    // Timing state for offset calculation
    t1: Option<PtpTimestamp>, // Sync send time (from Follow_Up)
    t2: Option<PtpTimestamp>, // Sync receive time (local)
    t3: Option<PtpTimestamp>, // Delay_Req send time (local)
    t4: Option<PtpTimestamp>, // Delay_Req receive time (from Delay_Resp)

    offset: ClockOffset,
    diagnostics: Option<TimingDiagnosticsRecorder>,
    diagnostics_started: Option<Instant>,
}

impl PtpClient {
    /// Create new PTP client.
    pub fn new(master_addr: SocketAddr, sample_rate: u32) -> Self {
        Self {
            master_addr,
            state: PtpState::Unsynchronized,
            event_socket: None,
            general_socket: None,
            clock: Clock::new(sample_rate),
            sequence_id: 0,
            t1: None,
            t2: None,
            t3: None,
            t4: None,
            offset: ClockOffset::default(),
            diagnostics: None,
            diagnostics_started: None,
        }
    }

    /// Install a diagnostics recorder fed once per completed timing exchange.
    ///
    /// The monotone origin for `observed_elapsed_ns` is captured when the
    /// recorder is installed.
    pub fn set_diagnostics_recorder(&mut self, recorder: TimingDiagnosticsRecorder) {
        self.diagnostics_started = Some(Instant::now());
        self.diagnostics = Some(recorder);
    }

    /// Bind to PTP ports.
    pub fn bind(&mut self) -> Result<()> {
        // Event port 319 (privileged) - try it, fallback to any port
        let event = UdpSocket::bind(("0.0.0.0", PTP_EVENT_PORT))
            .or_else(|_| UdpSocket::bind("0.0.0.0:0"))?;
        event.set_read_timeout(Some(Duration::from_secs(1)))?;

        // General port 320 (privileged) - try it, fallback to any port
        let general = UdpSocket::bind(("0.0.0.0", PTP_GENERAL_PORT))
            .or_else(|_| UdpSocket::bind("0.0.0.0:0"))?;
        general.set_read_timeout(Some(Duration::from_secs(1)))?;

        self.event_socket = Some(event);
        self.general_socket = Some(general);

        Ok(())
    }

    /// Process received Sync message.
    pub fn process_sync(&mut self, _header: &PtpHeader) {
        // Record receive time
        self.t2 = Some(PtpTimestamp::now());
        self.state = PtpState::Synchronizing;
    }

    /// Process received Follow_Up message.
    pub fn process_follow_up(&mut self, _header: &PtpHeader, data: &[u8]) -> Result<()> {
        // Extract precise origin timestamp from Follow_Up
        if data.len() >= 44 {
            self.t1 = Some(PtpTimestamp::parse(&data[34..44])?);
        }
        Ok(())
    }

    /// Send Delay_Req message.
    pub fn send_delay_req(&mut self) -> Result<()> {
        let socket = self
            .event_socket
            .as_ref()
            .ok_or_else(|| Error::Rtsp(airplay_core::error::RtspError::NoSession))?;

        self.sequence_id = self.sequence_id.wrapping_add(1);
        let header = PtpHeader::new(PtpMessageType::DelayReq, self.sequence_id);

        // Record send time
        self.t3 = Some(PtpTimestamp::now());

        let mut packet = [0u8; 44];
        packet[..34].copy_from_slice(&header.serialize());
        packet[34..44].copy_from_slice(&self.t3.unwrap().serialize());

        let dest = SocketAddr::new(self.master_addr.ip(), PTP_EVENT_PORT);
        socket.send_to(&packet, dest)?;

        Ok(())
    }

    /// Process Delay_Resp message.
    pub fn process_delay_resp(&mut self, _header: &PtpHeader, data: &[u8]) -> Result<()> {
        if data.len() >= 44 {
            self.t4 = Some(PtpTimestamp::parse(&data[34..44])?);
            self.calculate_offset();
        }
        Ok(())
    }

    /// Calculate offset from timestamps.
    fn calculate_offset(&mut self) {
        if let (Some(t1), Some(t2), Some(t3), Some(t4)) = (self.t1, self.t2, self.t3, self.t4) {
            // offset = ((t2 - t1) + (t3 - t4)) / 2
            // delay = ((t2 - t1) - (t3 - t4)) / 2

            let t1_ns = t1.to_nanos() as i128;
            let t2_ns = t2.to_nanos() as i128;
            let t3_ns = t3.to_nanos() as i128;
            let t4_ns = t4.to_nanos() as i128;

            let offset = ((t2_ns - t1_ns) + (t3_ns - t4_ns)) / 2;
            let delay = ((t2_ns - t1_ns) - (t3_ns - t4_ns)) / 2;

            self.offset = ClockOffset {
                offset_ns: offset as i64,
                error_ns: (delay.abs() / 2) as u64,
                rtt_ns: delay.abs() as u64,
            };

            self.state = PtpState::Synchronized;

            // Diagnostics hook: mirror this exchange's wire timestamps through
            // the same NTP/PTP math as `ptp_measurement`. The recorder owns
            // sequence stamping; a failed conversion only skips the record.
            if let (Some(recorder), Some(started)) =
                (self.diagnostics.as_ref(), self.diagnostics_started)
            {
                let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let conv = |t: PtpTimestamp| u64::try_from(t.to_nanos()).ok();
                if let (Some(t1_ns), Some(t2_ns), Some(t3_ns), Some(t4_ns)) =
                    (conv(t1), conv(t2), conv(t3), conv(t4))
                {
                    if let Some(measurement) =
                        ptp_measurement(0, elapsed_ns, t1_ns, t2_ns, t3_ns, t4_ns)
                    {
                        recorder.record(measurement);
                    }
                }
            }

            // Clear for next cycle
            self.t1 = None;
            self.t2 = None;
            self.t3 = None;
            self.t4 = None;
        }
    }

    /// Get current state.
    pub fn state(&self) -> PtpState {
        self.state
    }

    /// Get clock offset in nanoseconds.
    pub fn offset_ns(&self) -> i64 {
        self.offset.offset_ns
    }

    /// Get path delay in nanoseconds.
    pub fn delay_ns(&self) -> u64 {
        self.offset.rtt_ns
    }

    /// Convert local timestamp to master time.
    ///
    /// [`Self::calculate_offset`] stores the offset with the explicit
    /// convention `offset = local - master`, so this conversion **subtracts**
    /// it (see [`crate::diagnostics::local_to_master_ns`]). A conversion that
    /// would leave the representable range saturates at `0` instead of
    /// wrapping.
    pub fn local_to_master(&self, local_ns: u64) -> u64 {
        local_to_master_ns(local_ns, self.offset.offset_ns).unwrap_or(0)
    }

    /// Convert master timestamp to local time.
    ///
    /// The exact inverse of [`Self::local_to_master`]: `local = master +
    /// offset`, saturating instead of wrapping.
    pub fn master_to_local(&self, master_ns: u64) -> u64 {
        let local = master_ns as i128 + self.offset.offset_ns as i128;
        u64::try_from(local.clamp(0, u64::MAX as i128)).unwrap_or(0)
    }
}

#[async_trait]
impl TimingProtocol for PtpClient {
    async fn start(&mut self) -> Result<()> {
        self.bind()?;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.event_socket = None;
        self.general_socket = None;
        self.state = PtpState::Unsynchronized;
        Ok(())
    }

    async fn sync(&mut self) -> Result<ClockOffset> {
        // In real PTP, we'd wait for Sync/Follow_Up from master
        // For active sync, send Delay_Req and wait for response
        self.send_delay_req()?;

        // Read response
        let socket = self
            .event_socket
            .as_ref()
            .ok_or_else(|| Error::Rtsp(airplay_core::error::RtspError::NoSession))?;

        let mut buf = [0u8; 128];
        let (len, _) = socket.recv_from(&mut buf)?;

        let header = PtpHeader::parse(&buf[..len])?;
        if header.message_type == PtpMessageType::DelayResp {
            self.process_delay_resp(&header, &buf[..len])?;
        }

        Ok(self.offset)
    }

    fn offset(&self) -> ClockOffset {
        self.offset
    }

    fn is_synchronized(&self) -> bool {
        self.state == PtpState::Synchronized
    }

    fn local_to_remote(&self, local_ns: u64) -> u64 {
        self.local_to_master(local_ns)
    }

    fn remote_to_local(&self, remote_ns: u64) -> u64 {
        self.master_to_local(remote_ns)
    }
}

/// PTP master/grandmaster for AirPlay 2.
///
/// The sender acts as PTP master, and receivers sync to us.
/// For gPTP bidirectional mode, we also sync TO the receiver's clock.
pub struct PtpMaster {
    event_socket: Option<std::sync::Arc<tokio::net::UdpSocket>>,
    general_socket: Option<std::sync::Arc<tokio::net::UdpSocket>>,
    sequence_id: u16,
    clock_identity: [u8; 8],
    port: u16,
    running: bool,
    sync_task: Option<tokio::task::JoinHandle<()>>,
    offset_tx: Option<tokio::sync::watch::Sender<ClockOffset>>,
}

impl PtpMaster {
    /// Create new PTP master.
    pub fn new() -> Self {
        // Generate a clock identity from system time (in real PTP this would be based on MAC address)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let clock_identity = (now as u64).to_be_bytes();

        Self {
            event_socket: None,
            general_socket: None,
            sequence_id: 0,
            clock_identity,
            port: PTP_EVENT_PORT,
            running: false,
            sync_task: None,
            offset_tx: None,
        }
    }

    /// Set the offset sender for bidirectional gPTP
    pub fn set_offset_sender(&mut self, tx: tokio::sync::watch::Sender<ClockOffset>) {
        self.offset_tx = Some(tx);
    }

    /// Start the PTP master, binding to ports.
    pub async fn start(&mut self) -> Result<()> {
        use tokio::net::UdpSocket;

        // Bind to PTP event port 319 (requires root)
        let event = UdpSocket::bind(("0.0.0.0", PTP_EVENT_PORT))
            .await
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "Failed to bind PTP event port {}: {} (requires root)",
                        PTP_EVENT_PORT, e
                    ),
                )
            })?;

        // Bind to PTP general port 320 (requires root)
        let general = UdpSocket::bind(("0.0.0.0", PTP_GENERAL_PORT))
            .await
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "Failed to bind PTP general port {}: {} (requires root)",
                        PTP_GENERAL_PORT, e
                    ),
                )
            })?;

        let event = std::sync::Arc::new(event);
        let general = std::sync::Arc::new(general);

        self.event_socket = Some(event.clone());
        self.general_socket = Some(general.clone());
        self.running = true;

        tracing::info!(
            "PTP master started on ports {}/{}",
            PTP_EVENT_PORT,
            PTP_GENERAL_PORT
        );

        // Start background task to handle incoming messages and calculate offset
        let event_clone = event.clone();
        let general_clone = general.clone();
        let clock_identity = self.clock_identity;
        let offset_tx = self.offset_tx.clone();

        // Spawn task to listen on both ports
        self.sync_task = Some(tokio::spawn(async move {
            let mut event_buf = [0u8; 1024];
            let mut general_buf = [0u8; 1024];

            // Bidirectional gPTP: track timestamps for syncing TO HomePod's clock
            let mut t1: Option<PtpTimestamp> = None; // HomePod's send time (from Follow_Up)
            let mut t2: Option<PtpTimestamp> = None; // Our receive time (when we got Sync)
            let mut t3: Option<PtpTimestamp> = None; // Our send time (when we send Delay_Req)
            let mut delay_req_seq: u16 = 0;
            let mut master_addr: Option<std::net::SocketAddr> = None;

            tracing::info!(
                "PTP master: listening for messages on ports 319 and 320 (bidirectional gPTP)"
            );
            loop {
                tokio::select! {
                    // Listen on event port (319) for Delay_Req
                    result = event_clone.recv_from(&mut event_buf) => {
                        match result {
                            Ok((len, src)) => {
                                if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                                    tracing::info!("gPTP: Received message on port 319: type={:?}, seq={}, from={}, len={}",
                                        header.message_type, header.sequence_id, src, len);

                                    match header.message_type {
                                        PtpMessageType::Sync => {
                                            // HomePod sent us a Sync - capture receive time (t2)
                                            t2 = Some(PtpTimestamp::now());
                                            master_addr = Some(src);
                                            tracing::trace!("gPTP: Received Sync from HomePod (seq={}), t2 captured", header.sequence_id);
                                        }
                                        PtpMessageType::DelayResp => {
                                            tracing::info!("gPTP: Received Delay_Resp from HomePod (len={})", len);
                                            // HomePod's response to our Delay_Req - extract t4 and calculate offset
                                            if len >= 44 {
                                                if let Ok(t4) = PtpTimestamp::parse(&event_buf[34..44]) {
                                                    tracing::info!("gPTP: Parsed t4 timestamp: {}.{:09}s", t4.seconds, t4.nanoseconds);
                                                    tracing::info!("gPTP: Current timestamps - t1={:?}, t2={:?}, t3={:?}", t1, t2, t3);
                                                    if let (Some(t1v), Some(t2v), Some(t3v)) = (t1, t2, t3) {
                                                        let t1_ns = t1v.to_nanos() as i128;
                                                        let t2_ns = t2v.to_nanos() as i128;
                                                        let t3_ns = t3v.to_nanos() as i128;
                                                        let t4_ns = t4.to_nanos() as i128;

                                                        let offset_val = ((t2_ns - t1_ns) + (t3_ns - t4_ns)) / 2;
                                                        let delay = ((t2_ns - t1_ns) - (t3_ns - t4_ns)) / 2;

                                                        let clock_offset = ClockOffset {
                                                            offset_ns: offset_val as i64,
                                                            error_ns: (delay.abs() / 2) as u64,
                                                            rtt_ns: delay.abs() as u64,
                                                        };

                                                        tracing::info!("gPTP: Synchronized to HomePod: offset={}ns, delay={}ns",
                                                            clock_offset.offset_ns, clock_offset.rtt_ns);

                                                        if let Some(ref tx) = offset_tx {
                                                            let _ = tx.send(clock_offset);
                                                        }

                                                        // Clear for next cycle
                                                        t1 = None;
                                                        t2 = None;
                                                        t3 = None;
                                                    }
                                                }
                                            }
                                        }
                                        PtpMessageType::DelayReq => {
                                            // Receiver sent us Delay_Req - send Delay_Resp
                                            tracing::info!("PTP: Received Delay_Req from {} (seq={})", src, header.sequence_id);
                                            let recv_time = PtpTimestamp::now();
                                            if let Err(e) = Self::send_delay_resp_static(
                                                &event_clone,
                                                src,
                                                header.sequence_id,
                                                &clock_identity,
                                                recv_time,
                                                &header.source_port_identity,
                                            ).await {
                                                tracing::warn!("Failed to send Delay_Resp: {}", e);
                                            } else {
                                                tracing::trace!("PTP: Sent Delay_Resp to {} (seq={}, t4={}.{:09}s)",
                                                    src, header.sequence_id, recv_time.seconds, recv_time.nanoseconds);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("PTP event socket error: {}", e);
                                break;
                            }
                        }
                    }
                    // Listen on general port (320) for Signaling, Announce, etc.
                    result = general_clone.recv_from(&mut general_buf) => {
                        match result {
                            Ok((len, src)) => {
                                if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                                    tracing::info!("gPTP: Received message on port 320: type={:?}, seq={}, from={}, len={}",
                                        header.message_type, header.sequence_id, src, len);

                                    // Determine TLV start offset based on message type
                                    // Signaling: 34-byte header + 10-byte targetPortIdentity = 44
                                    // Follow_Up: 34-byte header + 10-byte preciseOriginTimestamp = 44
                                    // Announce: 34-byte header + 10-byte originTimestamp + 16+ announce fields = varies
                                    let tlv_offset = match header.message_type {
                                        PtpMessageType::Signaling => 44,  // After targetPortIdentity
                                        PtpMessageType::FollowUp => 44,   // After preciseOriginTimestamp
                                        PtpMessageType::Announce => 64,   // After announce data fields
                                        _ => 34,
                                    };

                                    // Parse TLVs if message is long enough
                                    if len > tlv_offset {
                                        let tlvs = parse_tlvs(&general_buf[tlv_offset..len]);

                                        for (tlv_type, tlv_len, tlv_data) in tlvs {
                                            tracing::debug!("  TLV: type=0x{:04x}, len={}", tlv_type, tlv_len);

                                            // Organization Extension TLV (0x0003)
                                            if tlv_type == 0x0003 && tlv_data.len() >= 6 {
                                                let org_id = &tlv_data[0..3];
                                                let org_subtype = &tlv_data[3..6];
                                                let payload = &tlv_data[6..];

                                                let org_hex: String = org_id.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("");
                                                let sub_hex: String = org_subtype.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("");

                                                tracing::info!("    Org TLV: org_id=0x{}, subtype=0x{}, payload_len={}",
                                                    org_hex, sub_hex, payload.len());

                                                // Log Apple TLV payload
                                                if org_id == [0x00, 0x0d, 0x93] {
                                                    let payload_hex: String = payload.iter()
                                                        .take(16)
                                                        .map(|b| format!("{:02x}", b))
                                                        .collect::<Vec<_>>()
                                                        .join(" ");
                                                    tracing::info!("    Apple TLV payload (first 16 bytes): {}", payload_hex);
                                                }
                                            }
                                        }
                                    }

                                    // Handle specific message types
                                    match header.message_type {
                                        PtpMessageType::FollowUp => {
                                            // Extract t1 from HomePod's Follow_Up and send Delay_Req
                                            if len >= 44 {
                                                if let Ok(timestamp) = PtpTimestamp::parse(&general_buf[34..44]) {
                                                    t1 = Some(timestamp);
                                                    tracing::trace!("gPTP: Received Follow_Up from HomePod (seq={}, t1={}.{:09}s)",
                                                        header.sequence_id, timestamp.seconds, timestamp.nanoseconds);

                                                    // Send Delay_Req to complete the timing exchange
                                                    if let Some(master) = master_addr {
                                                        delay_req_seq = delay_req_seq.wrapping_add(1);
                                                        let delay_header = PtpHeader::new(PtpMessageType::DelayReq, delay_req_seq);

                                                        // Capture t3 before sending
                                                        t3 = Some(PtpTimestamp::now());

                                                        let mut delay_packet = [0u8; 44];
                                                        delay_packet[..34].copy_from_slice(&delay_header.serialize());
                                                        delay_packet[34..44].copy_from_slice(&t3.unwrap().serialize());

                                                        let master_event = std::net::SocketAddr::new(master.ip(), PTP_EVENT_PORT);
                                                        if let Err(e) = event_clone.send_to(&delay_packet, master_event).await {
                                                            tracing::warn!("Failed to send Delay_Req: {}", e);
                                                        } else {
                                                            tracing::info!("gPTP: Sent Delay_Req to HomePod (seq={})", delay_req_seq);
                                                        }
                                                    } else {
                                                        tracing::warn!("gPTP: Cannot send Delay_Req - master_addr is None");
                                                    }
                                                }
                                            }
                                        }
                                        PtpMessageType::Signaling => {
                                            tracing::info!("gPTP: Received Signaling from HomePod - negotiation in progress");
                                            // TODO: Respond to Signaling with our own capabilities
                                        }
                                        PtpMessageType::Announce => {
                                            // Parse clock quality from Announce message (bytes 47-49 in message)
                                            if len >= 50 {
                                                let clock_class = general_buf[47];
                                                tracing::info!("gPTP: Received Announce - HomePod clock class={} (ours=193)", clock_class);
                                                // Clock class 248 = slave-only, 193 = better master
                                                // We should win BMCA with clock class 193
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("PTP general socket error: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
        }));

        Ok(())
    }

    /// Send Sync + Follow_Up to a specific address.
    pub async fn send_sync(&mut self, dest: SocketAddr) -> Result<()> {
        let event_socket = self
            .event_socket
            .as_ref()
            .ok_or_else(|| Error::Rtsp(airplay_core::error::RtspError::NoSession))?;
        let general_socket = self
            .general_socket
            .as_ref()
            .ok_or_else(|| Error::Rtsp(airplay_core::error::RtspError::NoSession))?;

        self.sequence_id = self.sequence_id.wrapping_add(1);
        let seq = self.sequence_id;

        // Build source port identity (clock identity + port number)
        let mut source_port_identity = [0u8; 10];
        source_port_identity[..8].copy_from_slice(&self.clock_identity);
        source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes()); // port 1

        // Send Sync message (event port) - two-step: timestamp in Follow_Up
        let mut sync_header = PtpHeader::new(PtpMessageType::Sync, seq);
        sync_header.flags = 0x0200; // Two-step flag
        sync_header.source_port_identity = source_port_identity;

        let mut sync_packet = [0u8; 44];
        sync_packet[..34].copy_from_slice(&sync_header.serialize());
        // Sync timestamp field is zeroed (actual time sent in Follow_Up)

        event_socket.send_to(&sync_packet, dest).await?;
        let sync_time = PtpTimestamp::now(); // Capture immediately after send for accurate t1
        tracing::debug!(
            "gPTP: Sent Sync to {}:{} (seq={})",
            dest.ip(),
            dest.port(),
            seq
        );

        // Send Follow_Up message (general port) with precise timestamp + gPTP TLV
        let mut followup_header = PtpHeader::new(PtpMessageType::FollowUp, seq);
        followup_header.source_port_identity = source_port_identity;

        // Create Follow_Up Information TLV for gPTP
        let followup_tlv = FollowUpInformationTlv::new();
        let tlv_bytes = followup_tlv.serialize();

        // Update message length to include TLV
        followup_header.message_length = 44 + tlv_bytes.len() as u16;

        let mut followup_packet = Vec::new();
        followup_packet.extend_from_slice(&followup_header.serialize());
        followup_packet.extend_from_slice(&sync_time.serialize());
        followup_packet.extend_from_slice(&tlv_bytes);

        let general_dest = SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
        general_socket
            .send_to(&followup_packet, general_dest)
            .await?;
        tracing::debug!(
            "gPTP: Sent Follow_Up to {}:{} (seq={}, t1={}.{:09}s, with TLV)",
            general_dest.ip(),
            general_dest.port(),
            seq,
            sync_time.seconds,
            sync_time.nanoseconds
        );

        Ok(())
    }

    /// Static helper to send Delay_Resp (used from background task).
    async fn send_delay_resp_static(
        socket: &tokio::net::UdpSocket,
        dest: SocketAddr,
        sequence_id: u16,
        clock_identity: &[u8; 8],
        recv_time: PtpTimestamp,
        requesting_port_identity: &[u8; 10],
    ) -> Result<()> {
        let mut source_port_identity = [0u8; 10];
        source_port_identity[..8].copy_from_slice(clock_identity);
        source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

        let mut header = PtpHeader::new(PtpMessageType::DelayResp, sequence_id);
        header.source_port_identity = source_port_identity;
        header.message_length = 54; // 34 header + 10 timestamp + 10 requesting port identity

        let mut packet = [0u8; 54];
        packet[..34].copy_from_slice(&header.serialize());
        packet[34..44].copy_from_slice(&recv_time.serialize());
        packet[44..54].copy_from_slice(requesting_port_identity);

        socket.send_to(&packet, dest).await?;
        Ok(())
    }

    /// Stop the PTP master.
    pub async fn stop(&mut self) {
        self.running = false;
        if let Some(task) = self.sync_task.take() {
            task.abort();
        }
        self.event_socket = None;
        self.general_socket = None;
        tracing::info!("PTP master stopped");
    }

    /// Get the port we're listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Check if running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Get reference to event socket for external sync sending.
    pub fn event_socket(&self) -> Option<&std::sync::Arc<tokio::net::UdpSocket>> {
        self.event_socket.as_ref()
    }

    /// Get reference to general socket for external sync sending.
    pub fn general_socket(&self) -> Option<&std::sync::Arc<tokio::net::UdpSocket>> {
        self.general_socket.as_ref()
    }

    /// Get the clock identity.
    pub fn clock_identity(&self) -> [u8; 8] {
        self.clock_identity
    }
}

impl Default for PtpMaster {
    fn default() -> Self {
        Self::new()
    }
}

/// Send a PTP Announce message to a destination.
///
/// IEEE 1588 Announce (64 bytes): 34-byte header + 10-byte origin timestamp + 20-byte body.
/// The receiver uses this to run the Best Master Clock Algorithm (BMCA)
/// and accept us as the grandmaster before processing Sync messages.
pub async fn send_ptp_announce(
    general_socket: &tokio::net::UdpSocket,
    dest: std::net::SocketAddr,
    clock_identity: &[u8; 8],
    announce_seq: &mut u16,
    clock_class: u8, // 193 = master, 248+ = slave
    priority1: u8,   // BMCA priority: lower wins. Mac=250, HomePod=248
) -> Result<()> {
    *announce_seq = announce_seq.wrapping_add(1);
    let seq = *announce_seq;

    let mut source_port_identity = [0u8; 10];
    source_port_identity[..8].copy_from_slice(clock_identity);
    source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

    let mut header = PtpHeader::new(PtpMessageType::Announce, seq);
    header.source_port_identity = source_port_identity;
    header.message_length = 64; // 34 header + 10 timestamp + 20 announce body
    header.log_message_interval = 1; // Announce interval: 2^1 = 2 seconds

    let mut packet = [0u8; 64];
    packet[..34].copy_from_slice(&header.serialize());

    // Bytes 34-43: Origin timestamp (10 bytes) — zeroed for Announce
    // (already zero)

    // Bytes 44-63: Announce body (20 bytes)
    // Bytes 44-45: currentUtcOffset (i16 BE) — 37 seconds (TAI - UTC as of 2017+)
    packet[44..46].copy_from_slice(&37i16.to_be_bytes());
    // Byte 46: reserved
    // Byte 47: grandmasterPriority1 — lower wins BMCA
    packet[47] = priority1;
    // Bytes 48-51: grandmasterClockQuality (4 bytes)
    //   Byte 48: clockClass — 193 (master), 248+ (slave)
    //   Lower number = better clock, wins BMCA election
    packet[48] = clock_class;
    //   Byte 49: clockAccuracy — 0xFE (unknown)
    packet[49] = 0xFE;
    //   Bytes 50-51: offsetScaledLogVariance — 0xFFFF (unknown)
    packet[50..52].copy_from_slice(&0xFFFFu16.to_be_bytes());
    // Byte 52: grandmasterPriority2 — 128 (default)
    packet[52] = 128;
    // Bytes 53-60: grandmasterIdentity (8 bytes) — same as our clock identity
    packet[53..61].copy_from_slice(clock_identity);
    // Bytes 61-62: stepsRemoved (u16 BE) — 0 (we are the grandmaster)
    // (already zero)
    // Byte 63: timeSource — 0xA0 (internal oscillator)
    packet[63] = 0xA0;

    let general_dest = std::net::SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
    general_socket.send_to(&packet, general_dest).await?;
    tracing::info!(
        "PTP: Sent Announce to {}:{} (seq={}, clockClass={})",
        general_dest.ip(),
        general_dest.port(),
        seq,
        clock_class
    );

    Ok(())
}

/// Parse TLVs from a PTP message body (starting after the 34-byte header)
fn parse_tlvs(data: &[u8]) -> Vec<(u16, u16, Vec<u8>)> {
    let mut tlvs = Vec::new();
    let mut offset = 0;

    while offset + 4 <= data.len() {
        let tlv_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
        offset += 4;

        if offset + length as usize > data.len() {
            tracing::warn!(
                "TLV length exceeds buffer: type={}, length={}",
                tlv_type,
                length
            );
            break;
        }

        let tlv_data = data[offset..offset + length as usize].to_vec();
        tlvs.push((tlv_type, length, tlv_data));
        offset += length as usize;
    }

    tlvs
}

/// Send a gPTP Signaling message with Message Interval Request TLV.
///
/// This is sent during initial handshake to negotiate timing intervals with the receiver.
pub async fn send_ptp_signaling(
    general_socket: &tokio::net::UdpSocket,
    dest: std::net::SocketAddr,
    clock_identity: &[u8; 8],
    sequence_id: &mut u16,
    sync_interval_log: i8,     // -3 for 125ms (2^-3 = 1/8 second)
    announce_interval_log: i8, // -2 for 250ms (2^-2 = 1/4 second)
) -> Result<()> {
    *sequence_id = sequence_id.wrapping_add(1);
    let seq = *sequence_id;

    let mut source_port_identity = [0u8; 10];
    source_port_identity[..8].copy_from_slice(clock_identity);
    source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

    let mut header = PtpHeader::new(PtpMessageType::Signaling, seq);
    header.source_port_identity = source_port_identity;

    // Create Message Interval Request TLV
    let tlv = MessageIntervalRequestTlv::new(sync_interval_log, announce_interval_log);
    let tlv_bytes = tlv.serialize();

    // Create Apple TLV subtype 0x01 (based on HomePod's format)
    // HomePod sends: 00 04 03 01 00 00 00 00 00 00 00 00 00 20 13 9f
    // Pattern suggests: [flags?] [intervals?] [reserved] [some ID?]
    let apple_tlv_01_payload = vec![
        0x00,
        0x04,
        sync_interval_log as u8,
        announce_interval_log as u8,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00, // Simplified - zeros instead of HomePod's ID
    ];
    let apple_tlv_01 = AppleTlv::new([0x00, 0x00, 0x01], apple_tlv_01_payload);
    let apple_tlv_01_bytes = apple_tlv_01.serialize();

    // Create Apple TLV subtype 0x05 (based on HomePod's format)
    // HomePod sends: 00 10 03 01 ... (26 bytes total)
    let apple_tlv_05_payload = vec![
        0x00,
        0x10,
        sync_interval_log as u8,
        announce_interval_log as u8,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ];
    let apple_tlv_05 = AppleTlv::new([0x00, 0x00, 0x05], apple_tlv_05_payload);
    let apple_tlv_05_bytes = apple_tlv_05.serialize();

    // Total message length: 34 (header) + 10 (target port identity) + all TLVs
    header.message_length = 34
        + 10
        + tlv_bytes.len() as u16
        + apple_tlv_01_bytes.len() as u16
        + apple_tlv_05_bytes.len() as u16;

    let mut packet = Vec::new();
    packet.extend_from_slice(&header.serialize());

    // Target port identity (all 0xFF for wildcard = all ports)
    packet.extend_from_slice(&[0xFF; 10]);

    // Append TLVs (802.1AS first, then Apple TLVs)
    packet.extend_from_slice(&tlv_bytes);
    packet.extend_from_slice(&apple_tlv_01_bytes);
    packet.extend_from_slice(&apple_tlv_05_bytes);

    let general_dest = std::net::SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
    general_socket.send_to(&packet, general_dest).await?;
    tracing::info!(
        "gPTP: Sent Signaling to {}:{} (seq={}, sync={}ms, announce={}ms)",
        general_dest.ip(),
        general_dest.port(),
        seq,
        1000 / (1 << (-sync_interval_log)),
        1000 / (1 << (-announce_interval_log))
    );

    Ok(())
}

/// Send a single PTP Sync + Follow_Up message pair to a destination.
///
/// This is a standalone helper for use in external sync tasks.
pub async fn send_ptp_sync(
    event_socket: &tokio::net::UdpSocket,
    general_socket: &tokio::net::UdpSocket,
    dest: std::net::SocketAddr,
    clock_identity: &[u8; 8],
    sequence_id: &mut u16,
) -> Result<()> {
    *sequence_id = sequence_id.wrapping_add(1);
    let seq = *sequence_id;

    // Build source port identity (clock identity + port number)
    let mut source_port_identity = [0u8; 10];
    source_port_identity[..8].copy_from_slice(clock_identity);
    source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes()); // port 1

    // Send Sync message (event port) - two-step: timestamp in Follow_Up
    let mut sync_header = PtpHeader::new(PtpMessageType::Sync, seq);
    sync_header.flags = 0x0200; // Two-step flag
    sync_header.source_port_identity = source_port_identity;

    let mut sync_packet = [0u8; 44];
    sync_packet[..34].copy_from_slice(&sync_header.serialize());
    // Sync timestamp field is zeroed (actual time sent in Follow_Up)

    event_socket.send_to(&sync_packet, dest).await?;
    let sync_time = PtpTimestamp::now(); // Capture immediately after send for accurate t1
    tracing::debug!(
        "gPTP: Sent Sync to {}:{} (seq={})",
        dest.ip(),
        dest.port(),
        seq
    );

    // Send Follow_Up message (general port) with precise timestamp + gPTP TLV
    let mut followup_header = PtpHeader::new(PtpMessageType::FollowUp, seq);
    followup_header.source_port_identity = source_port_identity;

    // Create Follow_Up Information TLV for gPTP
    let followup_tlv = FollowUpInformationTlv::new();
    let tlv_bytes = followup_tlv.serialize();

    // Update message length to include TLV
    followup_header.message_length = 44 + tlv_bytes.len() as u16;

    let mut followup_packet = Vec::new();
    followup_packet.extend_from_slice(&followup_header.serialize());
    followup_packet.extend_from_slice(&sync_time.serialize());
    followup_packet.extend_from_slice(&tlv_bytes);

    let general_dest = std::net::SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
    general_socket
        .send_to(&followup_packet, general_dest)
        .await?;
    tracing::debug!(
        "gPTP: Sent Follow_Up to {}:{} (seq={}, t1={}.{:09}s, with TLV)",
        general_dest.ip(),
        general_dest.port(),
        seq,
        sync_time.seconds,
        sync_time.nanoseconds
    );

    Ok(())
}

/// Send Mac-style gPTP Signaling with Apple TLVs using `0x2710` values.
///
/// The Mac uses different Apple TLV payloads than HomePod. Key difference:
/// HomePod sends `0x0301` version values, Mac sends `0x2710` (10000) values.
pub async fn send_mac_style_signaling(
    general_socket: &tokio::net::UdpSocket,
    dest: std::net::SocketAddr,
    clock_identity: &[u8; 8],
    sequence_id: &mut u16,
) -> Result<()> {
    *sequence_id = sequence_id.wrapping_add(1);
    let seq = *sequence_id;

    let mut source_port_identity = [0u8; 10];
    source_port_identity[..8].copy_from_slice(clock_identity);
    source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

    let mut header = PtpHeader::new(PtpMessageType::Signaling, seq);
    header.source_port_identity = source_port_identity;

    // 802.1AS Message Interval Request TLV
    let tlv = MessageIntervalRequestTlv::new(-3, -2);
    let tlv_bytes = tlv.serialize();

    // Apple TLV 0x01 with Mac format (0x2710 values, not 0x0301)
    let apple_tlv_01_payload = vec![
        0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x27, 0x10, 0x00, 0x00, 0x27, 0x10, 0x00, 0x00, 0x00,
        0x00,
    ];
    let apple_tlv_01 = AppleTlv::new([0x00, 0x00, 0x01], apple_tlv_01_payload);
    let apple_tlv_01_bytes = apple_tlv_01.serialize();

    // Apple TLV 0x05 with Mac format (0x2710 values)
    let apple_tlv_05_payload = vec![
        0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x27, 0x10, 0x00, 0x00, 0x27, 0x10, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let apple_tlv_05 = AppleTlv::new([0x00, 0x00, 0x05], apple_tlv_05_payload);
    let apple_tlv_05_bytes = apple_tlv_05.serialize();

    header.message_length = 34
        + 10
        + tlv_bytes.len() as u16
        + apple_tlv_01_bytes.len() as u16
        + apple_tlv_05_bytes.len() as u16;

    let mut packet = Vec::new();
    packet.extend_from_slice(&header.serialize());
    packet.extend_from_slice(&[0xFF; 10]); // Target port identity: wildcard
    packet.extend_from_slice(&tlv_bytes);
    packet.extend_from_slice(&apple_tlv_01_bytes);
    packet.extend_from_slice(&apple_tlv_05_bytes);

    let general_dest = std::net::SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
    general_socket.send_to(&packet, general_dest).await?;
    tracing::info!(
        "gPTP: Sent Mac-style Signaling to {} (seq={}, Apple TLVs with 0x2710)",
        general_dest,
        seq
    );

    Ok(())
}

/// Send stop Signaling (802.1AS Message Interval Request with all intervals = 0x7E).
///
/// 0x7E (126) means "stop sending". This is sent when yielding master role
/// to the remote device during BMCA.
pub async fn send_stop_signaling(
    general_socket: &tokio::net::UdpSocket,
    dest: std::net::SocketAddr,
    clock_identity: &[u8; 8],
    sequence_id: &mut u16,
) -> Result<()> {
    *sequence_id = sequence_id.wrapping_add(1);
    let seq = *sequence_id;

    let mut source_port_identity = [0u8; 10];
    source_port_identity[..8].copy_from_slice(clock_identity);
    source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

    let mut header = PtpHeader::new(PtpMessageType::Signaling, seq);
    header.source_port_identity = source_port_identity;

    // 802.1AS Message Interval Request with all intervals = 0x7E (stop)
    let stop_tlv = MessageIntervalRequestTlv {
        organization_id: org_id::GPTP,
        organization_subtype: gptp_subtype::MESSAGE_INTERVAL_REQUEST,
        link_delay_interval: 0x7E,
        time_sync_interval: 0x7E,
        announce_interval: 0x7E,
        flags: 0x00,
    };
    let tlv_bytes = stop_tlv.serialize();

    header.message_length = 34 + 10 + tlv_bytes.len() as u16;

    let mut packet = Vec::new();
    packet.extend_from_slice(&header.serialize());
    packet.extend_from_slice(&[0xFF; 10]); // Target port identity: wildcard
    packet.extend_from_slice(&tlv_bytes);

    let general_dest = std::net::SocketAddr::new(dest.ip(), PTP_GENERAL_PORT);
    general_socket.send_to(&packet, general_dest).await?;
    tracing::info!(
        "gPTP: Sent STOP Signaling to {} (seq={}, all intervals=0x7E)",
        general_dest,
        seq
    );

    Ok(())
}

/// Run BMCA yield flow and adapt coherent clock states to the legacy channels.
///
/// New callers should use [`run_bmca_yield_flow_state`]. The offset-only
/// receiver cannot represent a transition, so this compatibility adapter
/// forwards only complete measurements and sends the first measured identity
/// through the historical one-shot channel.
pub async fn run_bmca_yield_flow(
    master_ip: std::net::IpAddr,
    priority1: u8,
    offset_tx: tokio::sync::watch::Sender<ClockOffset>,
    clock_id_tx: tokio::sync::oneshot::Sender<[u8; 8]>,
) -> Result<()> {
    let (state_tx, mut state_rx) = tokio::sync::watch::channel(None);
    let state_flow = run_bmca_yield_flow_state(master_ip, priority1, state_tx);
    tokio::pin!(state_flow);
    let mut clock_id_tx = Some(clock_id_tx);

    loop {
        tokio::select! {
            result = &mut state_flow => return result,
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                if let Some(state) = *state_rx.borrow_and_update() {
                    offset_tx.send_replace(state.offset);
                    if let Some(tx) = clock_id_tx.take() {
                        let _ = tx.send(state.master_clock_id);
                    }
                }
            }
        }
    }
}

/// Run BMCA yield flow: act like a Mac sender.
///
/// 1. Send 3 Syncs + 2 Announces (Priority1=250) + Mac-style Signaling
/// 2. Listen for HomePod's Announce, extract Priority1 + clock identity
/// 3. If HomePod wins (lower Priority1): send stop Signaling, become slave
/// 4. Slave loop: Sync → Follow_Up → Delay_Req → Delay_Resp → offset calculation
///
/// Complete clock generations are published through `state_tx`. `None` means
/// no measurement is ready or an announced clock transition is in progress.
pub async fn run_bmca_yield_flow_state(
    master_ip: std::net::IpAddr,
    priority1: u8,
    state_tx: tokio::sync::watch::Sender<Option<PtpClockState>>,
) -> Result<()> {
    use tokio::net::UdpSocket;

    // Bind to PTP ports (privileged, fall back to ephemeral)
    let event_socket = match UdpSocket::bind(("0.0.0.0", PTP_EVENT_PORT)).await {
        Ok(s) => {
            tracing::info!("BMCA: bound to event port {}", PTP_EVENT_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "BMCA: using ephemeral event port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    let general_socket = match UdpSocket::bind(("0.0.0.0", PTP_GENERAL_PORT)).await {
        Ok(s) => {
            tracing::info!("BMCA: bound to general port {}", PTP_GENERAL_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "BMCA: using ephemeral general port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    let event_dest = std::net::SocketAddr::new(master_ip, PTP_EVENT_PORT);
    let general_dest = std::net::SocketAddr::new(master_ip, PTP_GENERAL_PORT);

    // Generate clock identity
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let clock_identity = (now as u64).to_be_bytes();
    let mut local_port_identity = [0u8; 10];
    local_port_identity[..8].copy_from_slice(&clock_identity);
    local_port_identity[8..].copy_from_slice(&1u16.to_be_bytes());
    let mut tracker = PtpMeasurementTracker::new(local_port_identity);

    let mut sync_seq: u16 = 0;
    let mut announce_seq: u16 = 0;
    let mut signaling_seq: u16 = 0;

    tracing::info!(
        "BMCA: Starting Mac-style negotiation with {} (our priority1={})",
        master_ip,
        priority1
    );

    // Phase 1: Send 3 Syncs + 2 Announces + Mac-style Signaling (like Mac does)
    for i in 0..3 {
        send_ptp_sync(
            &event_socket,
            &general_socket,
            event_dest,
            &clock_identity,
            &mut sync_seq,
        )
        .await?;
        if i < 2 {
            send_ptp_announce(
                &general_socket,
                general_dest,
                &clock_identity,
                &mut announce_seq,
                248,
                priority1,
            )
            .await?;
        }
        tokio::time::sleep(std::time::Duration::from_millis(125)).await;
    }
    send_mac_style_signaling(
        &general_socket,
        general_dest,
        &clock_identity,
        &mut signaling_seq,
    )
    .await?;

    tracing::info!("BMCA: Initial messages sent, waiting for remote Announce...");

    // Phase 2: Listen for remote Announce to get their Priority1 + clock identity
    // Filter by source IP — only process messages from our target master.
    // Other PTP peers (secondary HomePods) may share ports 319/320.
    let mut remote_priority1: u8 = 255;
    let mut general_buf = [0u8; 256];
    let mut event_buf = [0u8; 256];
    let bmca_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(bmca_deadline) => {
                tracing::warn!("BMCA: Timeout waiting for remote Announce, proceeding as slave anyway");
                break;
            }
            result = general_socket.recv_from(&mut general_buf) => {
                if let Ok((len, src)) = result {
                    if src.ip() != master_ip {
                        tracing::debug!("BMCA: Ignoring PTP general from {} (not master {})", src.ip(), master_ip);
                        continue;
                    }
                    if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                        if header.message_type == PtpMessageType::Announce && len >= 61 {
                            remote_priority1 = general_buf[47];
                            let _ = tracker.observe_announce(&header, &general_buf[..len]);
                            if tracker.announced.is_some() {
                                tracing::info!("BMCA: Received valid Announce from {}: priority1={}",
                                    src.ip(), remote_priority1);
                                break;
                            }
                        }
                    }
                }
            }
            // Also drain event port during BMCA phase
            result = event_socket.recv_from(&mut event_buf) => {
                if let Ok((len, src)) = result {
                    if src.ip() != master_ip {
                        tracing::debug!("BMCA: Ignoring PTP event from {} (not master {})", src.ip(), master_ip);
                        continue;
                    }
                    if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                        tracing::debug!("BMCA: Received {:?} on event port during negotiation from {}", header.message_type, src.ip());
                    }
                }
            }
        }
    }

    // Phase 3: BMCA decision — lower priority1 wins master
    let we_are_master = priority1 < remote_priority1;
    tracing::info!(
        "BMCA: Decision — our_p1={}, remote_p1={}, we_are_master={}",
        priority1,
        remote_priority1,
        we_are_master
    );

    if !we_are_master {
        // Yield: send stop Signaling, transition to slave
        send_stop_signaling(
            &general_socket,
            general_dest,
            &clock_identity,
            &mut signaling_seq,
        )
        .await?;
        tracing::info!("BMCA: Yielded master to remote, transitioning to slave mode");
    }

    // Phase 4: Slave loop (receive Sync/Follow_Up, send Delay_Req, calculate offset)
    // Filter all incoming PTP by source IP — only process messages from our master.
    // Messages from other peers (secondary HomePods sharing ports 319/320) are drained.
    let mut delay_req_seq: u16 = 0;

    tracing::info!(
        "BMCA: Entering slave loop, syncing to {} (filtering other peers)",
        master_ip
    );

    loop {
        tokio::select! {
            result = event_socket.recv_from(&mut event_buf) => {
                if let Ok((len, src)) = result {
                    if src.ip() != master_ip {
                        tracing::trace!("BMCA slave: Ignoring event from {} (not master {})", src.ip(), master_ip);
                        continue;
                    }
                    if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                        match header.message_type {
                            PtpMessageType::Sync => {
                                tracker.observe_sync(
                                    &header,
                                    &event_buf[..len],
                                    PtpTimestamp::now(),
                                );
                                tracing::trace!("BMCA slave: Received Sync from {} (seq={})", src.ip(), header.sequence_id);
                            }
                            PtpMessageType::DelayResp => {
                                if let Some(state) = tracker.observe_delay_response(
                                    &header,
                                    &event_buf[..len],
                                ) {
                                    tracing::debug!(
                                        "BMCA slave: refined current clock generation offset={}ns, delay={}ns",
                                        state.offset.offset_ns,
                                        state.offset.rtt_ns
                                    );
                                    publish_clock_state(&state_tx, Some(state));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            result = general_socket.recv_from(&mut general_buf) => {
                if let Ok((len, src)) = result {
                    if src.ip() != master_ip {
                        tracing::trace!("BMCA slave: Ignoring general from {} (not master {})", src.ip(), master_ip);
                        continue;
                    }
                    if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                        match header.message_type {
                            PtpMessageType::Announce => {
                                if tracker.observe_announce(&header, &general_buf[..len])
                                    == TrackerPublication::Invalidate
                                {
                                    tracing::info!(
                                        "BMCA slave: announced clock generation changed; invalidating measurement"
                                    );
                                    publish_clock_state(&state_tx, None);
                                }
                            }
                            // Delay_Resp is a general message (IEEE 1588-2008
                            // clause 13.8). The tracker also accepts it on the
                            // event socket for compatibility, but validates the
                            // request sequence, requester, relay and generation.
                            PtpMessageType::DelayResp => {
                                if let Some(state) = tracker.observe_delay_response(
                                    &header,
                                    &general_buf[..len],
                                ) {
                                    tracing::debug!(
                                        "BMCA slave: refined current clock generation offset={}ns, delay={}ns",
                                        state.offset.offset_ns,
                                        state.offset.rtt_ns
                                    );
                                    publish_clock_state(&state_tx, Some(state));
                                }
                            }
                            PtpMessageType::FollowUp => {
                                if let Some(state) = tracker.observe_follow_up(
                                    &header,
                                    &general_buf[..len],
                                ) {
                                    tracing::debug!(
                                        "BMCA slave: measured clock generation offset={}ns (path delay unmeasured)",
                                        state.offset.offset_ns
                                    );
                                    publish_clock_state(&state_tx, Some(state));

                                    // Retain the legacy end-to-end delay probe.
                                    // A response may refine this same generation;
                                    // unanswered requests leave the one-way estimate.
                                    delay_req_seq = delay_req_seq.wrapping_add(1);
                                    let sent_at = PtpTimestamp::now();
                                    tracker.record_delay_request(
                                        delay_req_seq,
                                        local_port_identity,
                                        sent_at,
                                    );
                                    let mut delay_header = PtpHeader::new(
                                        PtpMessageType::DelayReq,
                                        delay_req_seq,
                                    );
                                    delay_header.source_port_identity = local_port_identity;

                                    let mut delay_packet = [0u8; 44];
                                    delay_packet[..34]
                                        .copy_from_slice(&delay_header.serialize());
                                    delay_packet[34..44].copy_from_slice(&sent_at.serialize());

                                    if let Err(e) =
                                        event_socket.send_to(&delay_packet, event_dest).await
                                    {
                                        tracing::warn!(
                                            "BMCA slave: Failed to send Delay_Req: {}",
                                            e
                                        );
                                    } else {
                                        tracing::trace!(
                                            "BMCA slave: Sent Delay_Req (seq={})",
                                            delay_req_seq
                                        );
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// Run as PTP master to multiple HomePods for group streaming.
///
/// Unlike `run_bmca_yield_flow` (where we yield and become slave), this flow:
/// 1. Sends BMCA Announces with priority1=246 (wins against HomePod's 248)
/// 2. Stays as master: sends periodic Sync+Follow_Up to all peers
/// 3. Responds to Delay_Req from any peer with Delay_Resp
/// 4. Reports our own clock identity (offset is 0 since we ARE the clock)
///
/// All HomePods sync to our clock, putting them in the same clock domain.
pub async fn run_ptp_group_master_flow(
    peer_ips: Vec<std::net::IpAddr>,
    priority1: u8,
    clock_id_tx: tokio::sync::oneshot::Sender<[u8; 8]>,
) -> Result<()> {
    use tokio::net::UdpSocket;

    // Bind to PTP ports (privileged)
    let event_socket = match UdpSocket::bind(("0.0.0.0", PTP_EVENT_PORT)).await {
        Ok(s) => {
            tracing::info!("PTP master: bound to event port {}", PTP_EVENT_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "PTP master: using ephemeral event port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    let general_socket = match UdpSocket::bind(("0.0.0.0", PTP_GENERAL_PORT)).await {
        Ok(s) => {
            tracing::info!("PTP master: bound to general port {}", PTP_GENERAL_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "PTP master: using ephemeral general port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    // Build destination addresses for all peers
    let event_dests: Vec<std::net::SocketAddr> = peer_ips
        .iter()
        .map(|ip| std::net::SocketAddr::new(*ip, PTP_EVENT_PORT))
        .collect();
    let general_dests: Vec<std::net::SocketAddr> = peer_ips
        .iter()
        .map(|ip| std::net::SocketAddr::new(*ip, PTP_GENERAL_PORT))
        .collect();

    // Generate clock identity
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let clock_identity = (now as u64).to_be_bytes();

    let mut sync_seq: u16 = 0;
    let mut announce_seq: u16 = 0;
    let mut signaling_seq: u16 = 0;

    tracing::info!(
        "PTP master: Starting group master flow with {} peers (pri1={})",
        peer_ips.len(),
        priority1
    );

    // Phase 1: Send initial BMCA messages to all peers
    // 3 Syncs + 2 Announces + Mac-style Signaling to each peer
    for peer_idx in 0..peer_ips.len() {
        let event_dest = event_dests[peer_idx];
        let general_dest = general_dests[peer_idx];

        for i in 0..3 {
            send_ptp_sync(
                &event_socket,
                &general_socket,
                event_dest,
                &clock_identity,
                &mut sync_seq,
            )
            .await?;
            if i < 2 {
                send_ptp_announce(
                    &general_socket,
                    general_dest,
                    &clock_identity,
                    &mut announce_seq,
                    248,
                    priority1,
                )
                .await?;
            }
            tokio::time::sleep(std::time::Duration::from_millis(125)).await;
        }
        send_mac_style_signaling(
            &general_socket,
            general_dest,
            &clock_identity,
            &mut signaling_seq,
        )
        .await?;
        tracing::info!(
            "PTP master: Initial messages sent to peer {}",
            peer_ips[peer_idx]
        );
    }

    // Phase 2: Wait for Announces from peers, then verify we win BMCA
    let mut event_buf = [0u8; 256];
    let mut general_buf = [0u8; 256];
    let bmca_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut peers_heard = 0usize;

    loop {
        if peers_heard >= peer_ips.len() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(bmca_deadline) => {
                tracing::warn!("PTP master: Timeout waiting for all peer Announces ({}/{})",
                    peers_heard, peer_ips.len());
                break;
            }
            result = general_socket.recv_from(&mut general_buf) => {
                if let Ok((len, src)) = result {
                    if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                        if header.message_type == PtpMessageType::Announce && len >= 61 {
                            let remote_priority1 = general_buf[47];
                            tracing::info!("PTP master: Peer {} Announce pri1={} (ours={})",
                                src.ip(), remote_priority1, priority1);
                            if priority1 < remote_priority1 {
                                tracing::info!("PTP master: We win BMCA against {}", src.ip());
                            } else {
                                tracing::warn!("PTP master: Peer {} has equal/better priority ({}), continuing as master anyway",
                                    src.ip(), remote_priority1);
                            }
                            peers_heard += 1;
                        }
                    }
                }
            }
            result = event_socket.recv_from(&mut event_buf) => {
                if let Ok((len, _src)) = result {
                    if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                        tracing::debug!("PTP master: Received {:?} on event port during BMCA", header.message_type);
                    }
                }
            }
        }
    }

    // Report our own clock identity
    let _ = clock_id_tx.send(clock_identity);

    tracing::info!(
        "PTP master: Entering master loop — sending Syncs to {} peers",
        peer_ips.len()
    );

    // Phase 3: Master loop — periodic Sync+Announce to all peers, handle Delay_Req
    let sync_interval = std::time::Duration::from_millis(125); // 8 Hz sync rate
    let mut announce_interval_counter: u32 = 0;
    let announce_every = 16; // Send Announce every 16 sync cycles (~2s)

    loop {
        // Send Sync + Follow_Up to all peers
        for &event_dest in &event_dests {
            if let Err(e) = send_ptp_sync(
                &event_socket,
                &general_socket,
                event_dest,
                &clock_identity,
                &mut sync_seq,
            )
            .await
            {
                tracing::warn!("PTP master: Failed to send Sync to {}: {}", event_dest, e);
            }
        }

        // Periodically send Announce to all peers
        announce_interval_counter += 1;
        if announce_interval_counter >= announce_every {
            announce_interval_counter = 0;
            for &general_dest in &general_dests {
                if let Err(e) = send_ptp_announce(
                    &general_socket,
                    general_dest,
                    &clock_identity,
                    &mut announce_seq,
                    248,
                    priority1,
                )
                .await
                {
                    tracing::warn!(
                        "PTP master: Failed to send Announce to {}: {}",
                        general_dest,
                        e
                    );
                }
            }
        }

        // Wait for sync interval, handling any incoming Delay_Req
        let deadline = tokio::time::Instant::now() + sync_interval;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    break;
                }
                result = event_socket.recv_from(&mut event_buf) => {
                    if let Ok((len, src)) = result {
                        if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                            if header.message_type == PtpMessageType::DelayReq {
                                let recv_time = PtpTimestamp::now();
                                tracing::trace!("PTP master: Delay_Req from {} (seq={})", src, header.sequence_id);

                                // Send Delay_Resp back
                                let mut source_port_identity = [0u8; 10];
                                source_port_identity[..8].copy_from_slice(&clock_identity);
                                source_port_identity[8..10].copy_from_slice(&1u16.to_be_bytes());

                                let mut resp_header = PtpHeader::new(PtpMessageType::DelayResp, header.sequence_id);
                                resp_header.source_port_identity = source_port_identity;
                                resp_header.message_length = 54;

                                let mut resp_packet = [0u8; 54];
                                resp_packet[..34].copy_from_slice(&resp_header.serialize());
                                resp_packet[34..44].copy_from_slice(&recv_time.serialize());
                                resp_packet[44..54].copy_from_slice(&header.source_port_identity);

                                if let Err(e) = event_socket.send_to(&resp_packet, src).await {
                                    tracing::warn!("PTP master: Failed to send Delay_Resp to {}: {}", src, e);
                                }
                            }
                        }
                    }
                }
                result = general_socket.recv_from(&mut general_buf) => {
                    if let Ok((len, src)) = result {
                        if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                            tracing::trace!("PTP master: Received {:?} on general port from {}", header.message_type, src);
                        }
                    }
                }
            }
        }
    }
}

/// Background PTP sync task.
pub async fn ptp_sync_loop(
    client: &mut PtpClient,
    interval: Duration,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                if let Err(e) = client.sync().await {
                    tracing::warn!("PTP sync failed: {}", e);
                }
            }
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Run an async PTP slave loop that listens for Sync/Follow_Up from the master
/// (HomePod/AirPlay receiver) and calculates clock offset.
///
/// The receiver is always the PTP master in AirPlay 2. Our sender acts as slave:
/// 1. Listen for Sync on event port (records t2 = local receive time)
/// 2. Listen for Follow_Up on general port (contains t1 = master send time)
/// 3. Send Delay_Req after Follow_Up
/// 4. Process Delay_Resp (contains t4 = master receive time)
/// 5. Calculate offset = ((t2-t1) + (t3-t4)) / 2
pub async fn run_ptp_slave(
    master_ip: std::net::IpAddr,
    offset_tx: tokio::sync::watch::Sender<ClockOffset>,
) -> Result<()> {
    use tokio::net::UdpSocket;

    // Bind to PTP ports (privileged, fall back to ephemeral)
    let event_socket = match UdpSocket::bind(("0.0.0.0", PTP_EVENT_PORT)).await {
        Ok(s) => {
            tracing::info!("PTP slave bound to event port {}", PTP_EVENT_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "PTP slave using ephemeral event port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    let general_socket = match UdpSocket::bind(("0.0.0.0", PTP_GENERAL_PORT)).await {
        Ok(s) => {
            tracing::info!("PTP slave bound to general port {}", PTP_GENERAL_PORT);
            s
        }
        Err(_) => {
            let s = UdpSocket::bind("0.0.0.0:0").await?;
            tracing::warn!(
                "PTP slave using ephemeral general port {}",
                s.local_addr()?.port()
            );
            s
        }
    };

    let master_event = std::net::SocketAddr::new(master_ip, PTP_EVENT_PORT);
    let master_general = std::net::SocketAddr::new(master_ip, PTP_GENERAL_PORT);

    // Generate clock identity for slave
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let clock_identity = (now as u64).to_be_bytes();

    let mut t1: Option<PtpTimestamp> = None;
    let mut t2: Option<PtpTimestamp> = None;
    let mut t3: Option<PtpTimestamp> = None;
    let mut sequence_id: u16 = 0;
    let mut announce_seq: u16 = 0;
    let mut signaling_seq: u16 = 0;

    let mut event_buf = [0u8; 256];
    let mut general_buf = [0u8; 256];

    tracing::info!(
        "PTP slave started, listening for Sync from master {}",
        master_ip
    );

    // gPTP negotiation: Send initial Announce + Signaling to HomePod
    // This triggers HomePod to start sending its Sync/Follow_Up messages
    tracing::info!("gPTP slave: Sending initial Announce+Signaling handshake");

    if let Err(e) = send_ptp_announce(
        &general_socket,
        master_general,
        &clock_identity,
        &mut announce_seq,
        255, // Worst clock class - we want HomePod to be master
        255, // Worst priority1 - we want HomePod to be master
    )
    .await
    {
        tracing::warn!("Failed to send initial Announce: {}", e);
    }

    if let Err(e) = send_ptp_signaling(
        &general_socket,
        master_general,
        &clock_identity,
        &mut signaling_seq,
        -3, // Request 125ms sync interval
        -2, // Request 250ms announce interval
    )
    .await
    {
        tracing::warn!("Failed to send initial Signaling: {}", e);
    }

    tracing::info!("gPTP slave: Handshake complete, waiting for master's Sync messages");

    // Periodic Announce interval (gPTP: 250ms = 2^-2)
    let mut announce_interval = tokio::time::interval(std::time::Duration::from_millis(250));
    announce_interval.tick().await; // Skip first immediate tick

    loop {
        tokio::select! {
            _ = announce_interval.tick() => {
                // Send periodic Announce to maintain gPTP session
                if let Err(e) = send_ptp_announce(
                    &general_socket,
                    master_general,
                    &clock_identity,
                    &mut announce_seq,
                    255,  // Worst clock class - we want HomePod to be master
                    255,  // Worst priority1 - we want HomePod to be master
                ).await {
                    tracing::warn!("Failed to send periodic Announce: {}", e);
                } else {
                    tracing::trace!("gPTP slave: Sent periodic Announce (seq={})", announce_seq);
                }
            }
            result = event_socket.recv_from(&mut event_buf) => {
                match result {
                    Ok((len, _src)) => {
                        if let Ok(header) = PtpHeader::parse(&event_buf[..len]) {
                            match header.message_type {
                                PtpMessageType::Sync => {
                                    t2 = Some(PtpTimestamp::now());
                                    tracing::trace!(
                                        "PTP: Received Sync (seq={})",
                                        header.sequence_id
                                    );
                                }
                                PtpMessageType::DelayResp => {
                                    if len >= 44 {
                                        if let Ok(t4) = PtpTimestamp::parse(&event_buf[34..44]) {
                                            if let (Some(t1v), Some(t2v), Some(t3v)) =
                                                (t1, t2, t3)
                                            {
                                                let t1_ns = t1v.to_nanos() as i128;
                                                let t2_ns = t2v.to_nanos() as i128;
                                                let t3_ns = t3v.to_nanos() as i128;
                                                let t4_ns = t4.to_nanos() as i128;

                                                let offset_val =
                                                    ((t2_ns - t1_ns) + (t3_ns - t4_ns)) / 2;
                                                let delay =
                                                    ((t2_ns - t1_ns) - (t3_ns - t4_ns)) / 2;

                                                let clock_offset = ClockOffset {
                                                    offset_ns: offset_val as i64,
                                                    error_ns: (delay.abs() / 2) as u64,
                                                    rtt_ns: delay.abs() as u64,
                                                };

                                                tracing::debug!(
                                                    "PTP synchronized: offset={}ns, delay={}ns",
                                                    clock_offset.offset_ns,
                                                    clock_offset.rtt_ns
                                                );
                                                let _ = offset_tx.send(clock_offset);
                                            }
                                            // Clear for next cycle
                                            t1 = None;
                                            t2 = None;
                                            t3 = None;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("PTP event socket error: {}", e);
                    }
                }
            }
            result = general_socket.recv_from(&mut general_buf) => {
                match result {
                    Ok((len, _src)) => {
                        if let Ok(header) = PtpHeader::parse(&general_buf[..len]) {
                            if header.message_type == PtpMessageType::FollowUp && len >= 44 {
                                if let Ok(ts) = PtpTimestamp::parse(&general_buf[34..44]) {
                                    t1 = Some(ts);
                                    tracing::trace!(
                                        "PTP: Received Follow_Up (seq={}, t1={:?})",
                                        header.sequence_id,
                                        ts
                                    );

                                    // Send Delay_Req after receiving Follow_Up
                                    sequence_id = sequence_id.wrapping_add(1);
                                    let delay_header = PtpHeader::new(
                                        PtpMessageType::DelayReq,
                                        sequence_id,
                                    );

                                    // Capture timestamp just before send (t3)
                                    // Note: Unlike Sync, Delay_Req includes timestamp in the packet
                                    t3 = Some(PtpTimestamp::now());

                                    let mut delay_packet = [0u8; 44];
                                    delay_packet[..34]
                                        .copy_from_slice(&delay_header.serialize());
                                    delay_packet[34..44]
                                        .copy_from_slice(&t3.unwrap().serialize());

                                    if let Err(e) = event_socket
                                        .send_to(&delay_packet, master_event)
                                        .await
                                    {
                                        tracing::warn!(
                                            "Failed to send Delay_Req: {}",
                                            e
                                        );
                                    } else {
                                        tracing::trace!(
                                            "PTP: Sent Delay_Req (seq={})",
                                            sequence_id
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("PTP general socket error: {}", e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod one_way_offset_tests {
    use super::*;

    fn stamp(seconds: u64, nanoseconds: u32) -> PtpTimestamp {
        PtpTimestamp {
            seconds,
            nanoseconds,
        }
    }

    /// The whole point: two clocks on different epochs must produce an offset,
    /// not a zero.
    ///
    /// A HomePod's PTP clock counts from its own boot -- measured at 1 886 724
    /// seconds against a sender running Unix time. Publishing zero for that
    /// pair is what silenced every group session: the sender kept stamping its
    /// own timebase under the master's clock identity, and the receivers threw
    /// away audio dated decades ahead.
    #[test]
    fn two_clocks_on_different_epochs_do_not_produce_a_zero_offset() {
        let master = stamp(1_886_724, 0);
        let local = stamp(1_787_778_347, 0);

        let offset = one_way_offset(master, local);

        assert_eq!(offset.offset_ns, 1_785_891_623_000_000_000);
        assert_ne!(
            offset.offset_ns, 0,
            "a zero here means the sender is calling its own clock the master's"
        );
    }

    /// The omitted path delay is stated, not hidden.
    ///
    /// This estimate is wrong by one link traversal because no round trip
    /// answers it. A consumer that treats `error_ns` as zero would be trusting
    /// a number nobody measured.
    #[test]
    fn the_unmeasured_path_delay_is_declared_rather_than_assumed_away() {
        let offset = one_way_offset(stamp(10, 0), stamp(10, 500));

        assert_eq!(offset.error_ns, UNMEASURED_PATH_DELAY_NS);
        assert_eq!(
            offset.rtt_ns, 0,
            "no round trip happened, so none may be reported"
        );
    }

    /// Sub-second arithmetic survives the seconds/nanoseconds split.
    #[test]
    fn nanoseconds_carry_across_the_second_boundary() {
        let earlier = stamp(4, 999_999_000);
        let later = stamp(5, 1_000);

        assert_eq!(one_way_offset(earlier, later).offset_ns, 2_000);
    }

    /// A master ahead of us yields a negative offset rather than an underflow.
    #[test]
    fn a_master_ahead_of_the_sender_yields_a_negative_offset() {
        let offset = one_way_offset(stamp(100, 0), stamp(99, 0));

        assert_eq!(offset.offset_ns, -1_000_000_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod clock_generation_tracker {
        use super::*;

        const RELAY_A: [u8; 10] = [0x11; 10];
        const RELAY_B: [u8; 10] = [0x22; 10];
        const LOCAL: [u8; 10] = [0x33; 10];
        const GM_A: [u8; 8] = [0xAA; 8];
        const GM_B: [u8; 8] = [0xBB; 8];

        fn packet(
            message_type: PtpMessageType,
            sequence_id: u16,
            source: [u8; 10],
            timestamp: Option<PtpTimestamp>,
        ) -> Vec<u8> {
            let mut header = PtpHeader::new(message_type, sequence_id);
            header.source_port_identity = source;
            let mut bytes = header.serialize().to_vec();
            if let Some(timestamp) = timestamp {
                bytes.extend_from_slice(&timestamp.serialize());
            }
            bytes
        }

        fn announce(source: [u8; 10], grandmaster: [u8; 8]) -> Vec<u8> {
            let mut bytes = packet(PtpMessageType::Announce, 1, source, None);
            bytes.resize(64, 0);
            bytes[53..61].copy_from_slice(&grandmaster);
            bytes
        }

        fn observe_announce(
            tracker: &mut PtpMeasurementTracker,
            bytes: &[u8],
        ) -> TrackerPublication {
            let header = PtpHeader::parse(bytes).expect("valid test header");
            tracker.observe_announce(&header, bytes)
        }

        fn observe_sync(
            tracker: &mut PtpMeasurementTracker,
            bytes: &[u8],
            received_at: PtpTimestamp,
        ) {
            let header = PtpHeader::parse(bytes).expect("valid test header");
            tracker.observe_sync(&header, bytes, received_at);
        }

        fn observe_follow_up(
            tracker: &mut PtpMeasurementTracker,
            bytes: &[u8],
        ) -> Option<PtpClockState> {
            let header = PtpHeader::parse(bytes).expect("valid test header");
            tracker.observe_follow_up(&header, bytes)
        }

        #[test]
        fn grandmaster_change_discards_the_old_generation_and_publishes_only_a_matched_new_pair() {
            let mut tracker = PtpMeasurementTracker::new(LOCAL);
            assert_eq!(
                observe_announce(&mut tracker, &announce(RELAY_A, GM_A)),
                TrackerPublication::Unchanged
            );
            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 7, RELAY_A, None),
                PtpTimestamp::from_nanos(2_000),
            );

            assert_eq!(
                observe_announce(&mut tracker, &announce(RELAY_A, GM_B)),
                TrackerPublication::Invalidate
            );
            assert!(observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    7,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .is_none());

            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 8, RELAY_A, None),
                PtpTimestamp::from_nanos(5_000),
            );
            let state = observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    8,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(4_000)),
                ),
            )
            .expect("matched B generation publishes");

            assert_eq!(state.master_clock_id, GM_B);
            assert_eq!(state.offset.offset_ns, 1_000);
        }

        #[test]
        fn mismatched_sequence_or_relay_source_never_completes_a_sample() {
            let mut tracker = PtpMeasurementTracker::new(LOCAL);
            observe_announce(&mut tracker, &announce(RELAY_A, GM_A));
            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 9, RELAY_A, None),
                PtpTimestamp::from_nanos(2_000),
            );

            assert!(observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    10,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .is_none());
            assert!(observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    9,
                    RELAY_B,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .is_none());
        }

        #[test]
        fn repeated_announce_does_not_invalidate_a_completed_sample() {
            let mut tracker = PtpMeasurementTracker::new(LOCAL);
            observe_announce(&mut tracker, &announce(RELAY_A, GM_A));
            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 11, RELAY_A, None),
                PtpTimestamp::from_nanos(2_000),
            );
            assert!(observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    11,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .is_some());

            assert_eq!(
                observe_announce(&mut tracker, &announce(RELAY_A, GM_A)),
                TrackerPublication::Unchanged
            );
        }

        #[test]
        fn zero_or_truncated_announce_never_creates_a_ready_clock() {
            let mut tracker = PtpMeasurementTracker::new(LOCAL);
            assert_eq!(
                observe_announce(&mut tracker, &announce(RELAY_A, [0; 8])),
                TrackerPublication::Unchanged
            );
            let truncated = packet(PtpMessageType::Announce, 1, RELAY_A, None);
            assert_eq!(
                observe_announce(&mut tracker, &truncated),
                TrackerPublication::Unchanged
            );
            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 12, RELAY_A, None),
                PtpTimestamp::from_nanos(2_000),
            );
            assert!(observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    12,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .is_none());
        }

        #[test]
        fn delay_response_must_match_sequence_requester_relay_and_generation() {
            let mut tracker = PtpMeasurementTracker::new(LOCAL);
            observe_announce(&mut tracker, &announce(RELAY_A, GM_A));
            observe_sync(
                &mut tracker,
                &packet(PtpMessageType::Sync, 13, RELAY_A, None),
                PtpTimestamp::from_nanos(2_000),
            );
            let initial = observe_follow_up(
                &mut tracker,
                &packet(
                    PtpMessageType::FollowUp,
                    13,
                    RELAY_A,
                    Some(PtpTimestamp::from_nanos(1_000)),
                ),
            )
            .expect("initial sample");
            tracker.record_delay_request(21, LOCAL, PtpTimestamp::from_nanos(3_000));

            let mut wrong = packet(
                PtpMessageType::DelayResp,
                22,
                RELAY_A,
                Some(PtpTimestamp::from_nanos(4_000)),
            );
            wrong.extend_from_slice(&LOCAL);
            let header = PtpHeader::parse(&wrong).unwrap();
            assert!(tracker.observe_delay_response(&header, &wrong).is_none());

            let mut matched = packet(
                PtpMessageType::DelayResp,
                21,
                RELAY_A,
                Some(PtpTimestamp::from_nanos(4_000)),
            );
            matched.extend_from_slice(&LOCAL);
            let header = PtpHeader::parse(&matched).unwrap();
            let refined = tracker
                .observe_delay_response(&header, &matched)
                .expect("matched response refines current generation");
            assert_eq!(refined.master_clock_id, initial.master_clock_id);
            assert_eq!(refined.offset.offset_ns, 0);
        }

        #[test]
        fn send_replace_keeps_the_complete_state_for_late_subscribers() {
            let (tx, early) = tokio::sync::watch::channel(None);
            let state = PtpClockState {
                master_clock_id: GM_A,
                offset: ClockOffset {
                    offset_ns: 123,
                    error_ns: 4,
                    rtt_ns: 8,
                },
            };

            publish_clock_state(&tx, Some(state));
            drop(early);
            let late = tx.subscribe();
            let observed = (*late.borrow()).expect("late subscriber sees latest state");
            assert_eq!(observed.master_clock_id, GM_A);
            assert_eq!(observed.offset.offset_ns, 123);
        }
    }

    mod ptp_timestamp {
        use super::*;

        #[test]
        fn from_nanos_splits_correctly() {
            let ts = PtpTimestamp::from_nanos(1_234_567_890_123_456_789);
            assert_eq!(ts.seconds, 1_234_567_890);
            assert_eq!(ts.nanoseconds, 123_456_789);
        }

        #[test]
        fn to_nanos_combines_correctly() {
            let ts = PtpTimestamp {
                seconds: 1_234_567_890,
                nanoseconds: 123_456_789,
            };
            assert_eq!(ts.to_nanos(), 1_234_567_890_123_456_789);
        }

        #[test]
        fn serialize_is_10_bytes() {
            let ts = PtpTimestamp::now();
            let bytes = ts.serialize();
            assert_eq!(bytes.len(), 10);
        }

        #[test]
        fn parse_serialize_roundtrip() {
            let original = PtpTimestamp {
                seconds: 0x123456789ABC, // 48-bit value
                nanoseconds: 987_654_321,
            };
            let bytes = original.serialize();
            let parsed = PtpTimestamp::parse(&bytes).unwrap();

            // Only lower 48 bits of seconds are preserved
            assert_eq!(parsed.seconds, original.seconds & 0xFFFFFFFFFFFF);
            assert_eq!(parsed.nanoseconds, original.nanoseconds);
        }
    }

    mod ptp_header {
        use super::*;

        #[test]
        fn new_sets_fields() {
            let header = PtpHeader::new(PtpMessageType::Sync, 42);
            assert_eq!(header.message_type, PtpMessageType::Sync);
            assert_eq!(header.sequence_id, 42);
            assert_eq!(header.version, 2);
        }

        #[test]
        fn parse_sync_message() {
            let header = PtpHeader::new(PtpMessageType::Sync, 123);
            let bytes = header.serialize();
            let parsed = PtpHeader::parse(&bytes).unwrap();

            assert_eq!(parsed.message_type, PtpMessageType::Sync);
            assert_eq!(parsed.sequence_id, 123);
        }

        #[test]
        fn parse_follow_up_message() {
            let header = PtpHeader::new(PtpMessageType::FollowUp, 456);
            let bytes = header.serialize();
            let parsed = PtpHeader::parse(&bytes).unwrap();

            assert_eq!(parsed.message_type, PtpMessageType::FollowUp);
            assert_eq!(parsed.sequence_id, 456);
        }

        #[test]
        fn parse_delay_resp_message() {
            let header = PtpHeader::new(PtpMessageType::DelayResp, 789);
            let bytes = header.serialize();
            let parsed = PtpHeader::parse(&bytes).unwrap();

            assert_eq!(parsed.message_type, PtpMessageType::DelayResp);
            assert_eq!(parsed.sequence_id, 789);
        }

        #[test]
        fn serialize_delay_req_message() {
            let header = PtpHeader::new(PtpMessageType::DelayReq, 100);
            let bytes = header.serialize();

            assert_eq!(bytes[0], PtpMessageType::DelayReq as u8);
            assert_eq!(bytes.len(), 34);
        }
    }

    mod ptp_client {
        use super::*;

        #[test]
        fn new_starts_unsynchronized() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let client = PtpClient::new(addr, 44100);
            assert_eq!(client.state(), PtpState::Unsynchronized);
        }

        #[tokio::test]
        async fn start_opens_sockets() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // This may fail on systems without permission for ports 319/320
            // but should fall back to ephemeral ports
            let result = client.start().await;
            assert!(result.is_ok());
            assert!(client.event_socket.is_some());
            assert!(client.general_socket.is_some());
        }

        #[test]
        fn process_sync_stores_t2() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            let header = PtpHeader::new(PtpMessageType::Sync, 1);
            client.process_sync(&header);

            assert!(client.t2.is_some());
            assert_eq!(client.state(), PtpState::Synchronizing);
        }

        #[test]
        fn process_follow_up_stores_t1() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Create a follow-up packet with timestamp
            let header = PtpHeader::new(PtpMessageType::FollowUp, 1);
            let ts = PtpTimestamp::from_nanos(1_000_000_000);

            let mut data = [0u8; 44];
            data[..34].copy_from_slice(&header.serialize());
            data[34..44].copy_from_slice(&ts.serialize());

            client.process_follow_up(&header, &data).unwrap();
            assert!(client.t1.is_some());
        }

        #[test]
        fn process_delay_resp_stores_t4() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            let header = PtpHeader::new(PtpMessageType::DelayResp, 1);
            let ts = PtpTimestamp::from_nanos(2_000_000_000);

            let mut data = [0u8; 44];
            data[..34].copy_from_slice(&header.serialize());
            data[34..44].copy_from_slice(&ts.serialize());

            client.process_delay_resp(&header, &data).unwrap();
            assert!(client.t4.is_some());
        }

        #[test]
        fn calculate_offset_with_all_timestamps() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Set up timestamps
            client.t1 = Some(PtpTimestamp::from_nanos(100_000_000)); // Master send: 100ms
            client.t2 = Some(PtpTimestamp::from_nanos(110_000_000)); // Local recv: 110ms
            client.t3 = Some(PtpTimestamp::from_nanos(120_000_000)); // Local send: 120ms
            client.t4 = Some(PtpTimestamp::from_nanos(130_000_000)); // Master recv: 130ms

            client.calculate_offset();

            // offset = ((110-100) + (120-130)) / 2 = (10 + (-10)) / 2 = 0
            assert_eq!(client.offset.offset_ns, 0);
            assert_eq!(client.state(), PtpState::Synchronized);
        }
    }

    mod offset_calculation {
        use super::*;

        #[test]
        fn offset_zero_when_synchronized() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Symmetric delay, no offset
            client.t1 = Some(PtpTimestamp::from_nanos(0));
            client.t2 = Some(PtpTimestamp::from_nanos(10_000_000));
            client.t3 = Some(PtpTimestamp::from_nanos(10_000_000));
            client.t4 = Some(PtpTimestamp::from_nanos(20_000_000));

            client.calculate_offset();

            // offset = ((10-0) + (10-20)) / 2 = (10 + (-10)) / 2 = 0
            assert_eq!(client.offset.offset_ns, 0);
        }

        #[test]
        fn offset_positive_when_master_ahead() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Master is 5ms ahead
            client.t1 = Some(PtpTimestamp::from_nanos(15_000_000)); // Master at 15ms
            client.t2 = Some(PtpTimestamp::from_nanos(10_000_000)); // Local at 10ms
            client.t3 = Some(PtpTimestamp::from_nanos(20_000_000)); // Local at 20ms
            client.t4 = Some(PtpTimestamp::from_nanos(25_000_000)); // Master at 25ms

            client.calculate_offset();

            // offset = ((10-15) + (20-25)) / 2 = (-5 + (-5)) / 2 = -5ms
            // Negative means local is behind (master is ahead)
            assert!(client.offset.offset_ns < 0);
        }

        #[test]
        fn offset_negative_when_master_behind() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Master is 5ms behind
            client.t1 = Some(PtpTimestamp::from_nanos(5_000_000)); // Master at 5ms
            client.t2 = Some(PtpTimestamp::from_nanos(10_000_000)); // Local at 10ms
            client.t3 = Some(PtpTimestamp::from_nanos(20_000_000)); // Local at 20ms
            client.t4 = Some(PtpTimestamp::from_nanos(15_000_000)); // Master at 15ms

            client.calculate_offset();

            // offset = ((10-5) + (20-15)) / 2 = (5 + 5) / 2 = 5ms
            // Positive means local is ahead (master is behind)
            assert!(client.offset.offset_ns > 0);
        }

        #[test]
        fn delay_calculation() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // 10ms one-way delay
            client.t1 = Some(PtpTimestamp::from_nanos(0));
            client.t2 = Some(PtpTimestamp::from_nanos(10_000_000));
            client.t3 = Some(PtpTimestamp::from_nanos(20_000_000));
            client.t4 = Some(PtpTimestamp::from_nanos(30_000_000));

            client.calculate_offset();

            // delay = ((10-0) - (20-30)) / 2 = (10 - (-10)) / 2 = 10ms
            assert_eq!(client.offset.rtt_ns, 10_000_000);
        }
    }

    mod state_transitions {
        use super::*;

        #[test]
        fn unsynchronized_to_synchronizing() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            assert_eq!(client.state(), PtpState::Unsynchronized);

            let header = PtpHeader::new(PtpMessageType::Sync, 1);
            client.process_sync(&header);

            assert_eq!(client.state(), PtpState::Synchronizing);
        }

        #[test]
        fn synchronizing_to_synchronized() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // Receive Sync
            let header = PtpHeader::new(PtpMessageType::Sync, 1);
            client.process_sync(&header);
            assert_eq!(client.state(), PtpState::Synchronizing);

            // Receive Follow_Up with t1
            client.t1 = Some(PtpTimestamp::from_nanos(0));

            // Send Delay_Req (simulated)
            client.t3 = Some(PtpTimestamp::from_nanos(10_000_000));

            // Receive Delay_Resp with t4
            client.t4 = Some(PtpTimestamp::from_nanos(20_000_000));

            client.calculate_offset();
            assert_eq!(client.state(), PtpState::Synchronized);
        }

        #[test]
        fn synchronized_maintained_on_updates() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);

            // First sync
            client.t1 = Some(PtpTimestamp::from_nanos(0));
            client.t2 = Some(PtpTimestamp::from_nanos(10_000_000));
            client.t3 = Some(PtpTimestamp::from_nanos(20_000_000));
            client.t4 = Some(PtpTimestamp::from_nanos(30_000_000));
            client.calculate_offset();

            assert_eq!(client.state(), PtpState::Synchronized);
            let first_offset = client.offset.offset_ns;

            // Second sync
            client.t1 = Some(PtpTimestamp::from_nanos(100_000_000));
            client.t2 = Some(PtpTimestamp::from_nanos(110_000_000));
            client.t3 = Some(PtpTimestamp::from_nanos(120_000_000));
            client.t4 = Some(PtpTimestamp::from_nanos(130_000_000));
            client.calculate_offset();

            assert_eq!(client.state(), PtpState::Synchronized);
            // Offset should be the same (symmetric delay, no drift)
            assert_eq!(client.offset.offset_ns, first_offset);
        }
    }

    mod timestamp_conversion {
        use super::*;

        #[test]
        fn local_to_master_subtracts_a_positive_offset() {
            // `calculate_offset` stores offset = local - master, so a positive
            // offset means the local clock reads *ahead* of the master and the
            // conversion must subtract.
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);
            client.offset.offset_ns = 5_000_000; // local is 5ms ahead of master

            let master = client.local_to_master(10_000_000);
            assert_eq!(master, 5_000_000);
        }

        #[test]
        fn local_to_master_adds_a_negative_offset() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);
            client.offset.offset_ns = -5_000_000; // local is 5ms behind master

            let master = client.local_to_master(10_000_000);
            assert_eq!(master, 15_000_000);
        }

        #[test]
        fn local_to_master_saturates_instead_of_wrapping() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);
            client.offset.offset_ns = 5_000_000;

            assert_eq!(client.local_to_master(1_000), 0);
        }

        #[test]
        fn master_to_local_applies_offset() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);
            client.offset.offset_ns = 5_000_000; // local is 5ms ahead of master

            let local = client.master_to_local(5_000_000);
            assert_eq!(local, 10_000_000);
        }

        #[test]
        fn roundtrip_conversion() {
            let addr: SocketAddr = "127.0.0.1:319".parse().unwrap();
            let mut client = PtpClient::new(addr, 44100);
            client.offset.offset_ns = 5_000_000;

            let original = 12345678u64;
            let master = client.local_to_master(original);
            let recovered = client.master_to_local(master);

            assert_eq!(recovered, original);
        }
    }
}
