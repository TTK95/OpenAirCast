//! Multi-room device group management.
//!
//! Also hosts the typed best-effort group-connect contract ([`SetupPhase`],
//! [`MemberFailure`], [`MemberResult`], [`GroupConnectReport`]), the
//! deterministic primary-selection rule, and the private-by-default member
//! link seam used by tests to script per-member outcomes without network.

use crate::connection::{Connection, Health, WorkerLedger};
use airplay_core::{error::Result, Device, DeviceId};
use airplay_timing::PtpClockState;
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

/// Pin used for HomeKit transient pairing during group establishment.
pub(crate) const GROUP_PAIRING_PIN: &str = "3939";

/// Probe budget for the runtime keepalive health check of a live member.
///
/// Only valid on a member that has been through SETUP: the probe is the
/// step-6 `/feedback` keepalive and names the session UUID in its URI.
pub(crate) const MEMBER_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Upper bound on concurrently establishing group members during best-effort
/// connect. Bounds socket/pairing fan-out without serializing startup.
pub(crate) const MAX_PARALLEL_SETUPS: usize = 4;

/// Member of a device group.
#[derive(Debug, Clone)]
pub struct GroupMember {
    pub device: Device,
    pub volume: f32,
    pub is_leader: bool,
}

/// Multi-room device group.
pub struct DeviceGroup {
    id: Uuid,
    members: HashMap<DeviceId, GroupMember>,
    leader_id: DeviceId,
}

impl DeviceGroup {
    /// Create new group with leader device.
    pub fn new(leader: Device) -> Self {
        let leader_id = leader.id.clone();
        let mut members = HashMap::new();
        members.insert(
            leader.id.clone(),
            GroupMember {
                device: leader,
                volume: 1.0,
                is_leader: true,
            },
        );

        Self {
            id: Uuid::new_v4(),
            members,
            leader_id,
        }
    }

    /// Get group UUID.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Add device to group.
    pub fn add_member(&mut self, device: Device) -> Result<()> {
        let device_id = device.id.clone();

        // Don't allow adding duplicate members
        if self.members.contains_key(&device_id) {
            return Ok(()); // Already a member
        }

        self.members.insert(
            device_id,
            GroupMember {
                device,
                volume: 1.0,
                is_leader: false,
            },
        );

        Ok(())
    }

    /// Remove device from group.
    pub fn remove_member(&mut self, id: &DeviceId) -> Result<()> {
        // Cannot remove the leader
        if id == &self.leader_id {
            return Err(airplay_core::error::RtspError::SetupFailed(
                "Cannot remove group leader".to_string(),
            )
            .into());
        }

        self.members.remove(id);
        Ok(())
    }

    /// Get group members.
    pub fn members(&self) -> impl Iterator<Item = &GroupMember> {
        self.members.values()
    }

    /// Get member count.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Get leader device.
    pub fn leader(&self) -> &GroupMember {
        &self.members[&self.leader_id]
    }

    /// Set member volume.
    pub fn set_member_volume(&mut self, id: &DeviceId, volume: f32) -> Result<()> {
        if let Some(member) = self.members.get_mut(id) {
            // Clamp volume to valid range
            member.volume = volume.clamp(0.0, 1.0);
            Ok(())
        } else {
            Err(airplay_core::error::DiscoveryError::DeviceNotFound(id.to_mac_string()).into())
        }
    }

    /// Get addresses for SETPEERS command.
    pub fn peer_addresses(&self) -> Vec<String> {
        self.members
            .values()
            .flat_map(|member| {
                member
                    .device
                    .addresses
                    .iter()
                    .filter(|addr| {
                        // Exclude IPv6 link-local addresses (fe80::)
                        match addr {
                            std::net::IpAddr::V6(v6) => {
                                // Link-local addresses start with fe80::
                                let segments = v6.segments();
                                segments[0] != 0xfe80
                            }
                            std::net::IpAddr::V4(_) => true,
                        }
                    })
                    .map(|addr| addr.to_string())
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Best-effort group connect: typed phases, failures, and reports
// ---------------------------------------------------------------------------

/// Setup pipeline stage in which a member failure occurred.
///
/// The order is the chronological setup order used by
/// `AirPlayClient::connect_group_best_effort`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupPhase {
    /// TCP/RTSP transport establishment.
    Connect,
    /// HomeKit pairing (pair-setup / pair-verify).
    Pair,
    /// PTP timing identity of the group primary (BMCA yield, clock ID).
    PrimaryTiming,
    /// RTSP SETUP handshake (phases 1 and 2).
    RtspSetup,
    /// SETPEERS distribution.
    SetPeers,
    /// RTP sender construction.
    BuildSender,
    /// Runtime audio start or per-member control (RECORD, volume).
    StartAudio,
}

impl SetupPhase {
    /// Every phase in setup order; iteration-safe classification tables.
    pub const ALL: [SetupPhase; 7] = [
        SetupPhase::Connect,
        SetupPhase::Pair,
        SetupPhase::PrimaryTiming,
        SetupPhase::RtspSetup,
        SetupPhase::SetPeers,
        SetupPhase::BuildSender,
        SetupPhase::StartAudio,
    ];
}

impl SetupPhase {
    /// Whether a failure in this phase is worth retrying later.
    ///
    /// Transient network/timing conditions are retryable; pairing material
    /// and protocol/stateful handshakes are not.
    pub(crate) fn is_retryable(self) -> bool {
        match self {
            SetupPhase::Connect
            | SetupPhase::PrimaryTiming
            | SetupPhase::SetPeers
            | SetupPhase::StartAudio => true,
            SetupPhase::Pair | SetupPhase::RtspSetup | SetupPhase::BuildSender => false,
        }
    }
}

/// One member's failure with the phase it happened in.
#[derive(Debug)]
pub struct MemberFailure {
    /// Stable receiver identity.
    pub receiver: DeviceId,
    /// Pipeline stage that produced the failure.
    pub phase: SetupPhase,
    /// Whether a later reconnect attempt may succeed.
    pub retryable: bool,
    /// Original protocol error.
    pub source: airplay_core::Error,
}

/// A per-member operation outcome keyed by stable receiver identity.
#[derive(Debug)]
pub struct MemberResult<T> {
    /// Stable receiver identity.
    pub receiver: DeviceId,
    /// What happened for this receiver alone.
    pub result: std::result::Result<T, MemberFailure>,
}

/// Outcome of a best-effort group connect.
#[derive(Debug, Default)]
pub struct GroupConnectReport {
    /// Selected primary, `None` when no session could be established.
    pub primary: Option<DeviceId>,
    /// Live members: primary first, then secondaries in input order.
    pub connected: Vec<DeviceId>,
    /// Every per-member failure, in input order.
    pub failures: Vec<MemberFailure>,
}

/// Reject duplicate member IDs deterministically (first duplicate wins).
#[doc(hidden)]
pub fn find_duplicate_member(devices: &[Device]) -> Option<DeviceId> {
    for (index, device) in devices.iter().enumerate() {
        if devices[..index].iter().any(|other| other.id == device.id) {
            return Some(device.id.clone());
        }
    }
    None
}

/// Deterministic primary choice among already-connected members.
///
/// Rule: honor `preferred` only when it is present, healthy (part of the
/// candidate list), and PTP-capable; otherwise take the lexicographically
/// smallest PTP-capable stable ID. Never derives from input order.
#[doc(hidden)]
pub fn choose_primary<'a>(
    healthy_members: &'a [Device],
    preferred: Option<&DeviceId>,
) -> Option<&'a Device> {
    let ptp_capable: Vec<&Device> = healthy_members
        .iter()
        .filter(|device| device.supports_ptp())
        .collect();
    preferred
        .and_then(|id| ptp_capable.iter().copied().find(|device| &device.id == id))
        .or_else(|| ptp_capable.into_iter().min_by_key(|device| device.id.0))
}

// ---------------------------------------------------------------------------
// Member link seam (production wraps Connection; tests script outcomes)
// ---------------------------------------------------------------------------

/// Timing identity exported by the group primary after BMCA yield.
#[doc(hidden)]
#[derive(Debug)]
pub struct GroupPrimaryTiming {
    /// Live complete clock generations shared with every secondary.
    pub updates: tokio::sync::watch::Receiver<Option<PtpClockState>>,
}

/// Transport handle for one group member.
///
/// The production implementation wraps a [`Connection`]; tests replace it so
/// per-member establishment, health, and teardown run without network.
#[doc(hidden)]
#[async_trait]
pub trait GroupMemberLink: Send {
    /// Stable receiver identity.
    fn receiver(&self) -> DeviceId;

    /// Local address string advertised via SETPEERS, when known.
    fn local_peer_address(&self) -> Option<String> {
        None
    }

    /// RTSP `/feedback` keepalive probe of a member that is already set up.
    ///
    /// Never a precondition for setup: before SETUP the receiver does not know
    /// the session this probe names, so its answer carries no health signal.
    async fn probe(&mut self, timeout: Duration) -> Result<Health>;

    /// Apply the client's render lead before this member's stream is set up.
    ///
    /// Deliberately without a default body: a link that silently ignores the
    /// lead streams with no buffer headroom at all, which is exactly the
    /// failure this seam exists to prevent.
    fn set_render_delay_ms(&mut self, delay_ms: u32);

    /// RTSP SETUP handshake for this member.
    async fn setup_stream(&mut self) -> Result<()>;

    /// PTP timing identity after primary establishment (`None` = no timing).
    fn primary_timing_identity(&self) -> Option<GroupPrimaryTiming>;

    /// Join the primary's timing domain as a secondary member.
    async fn join_group_timing(
        &mut self,
        timing: &GroupPrimaryTiming,
        render_delay_ms: u32,
    ) -> Result<()>;

    /// Distribute the peer address list.
    async fn send_setpeers(&mut self, peer_addresses: &[String]) -> Result<()>;

    /// Receiver-local volume control.
    async fn set_volume(&mut self, volume: f32) -> Result<()>;

    /// Worker ledger of the underlying connection (test instrumentation).
    fn worker_ledger(&self) -> WorkerLedger;

    /// Bounded best-effort teardown; must never block indefinitely.
    async fn teardown(&mut self);

    /// Recover the underlying connection for session storage.
    fn into_connection(self: Box<Self>) -> Connection;
}

/// Per-member establishment seam: connect + pair one device.
#[doc(hidden)]
#[async_trait]
pub trait GroupLinkFactory: Send + Sync {
    /// Establish one member, classifying any failure into its phase.
    async fn connect_member(
        &self,
        device: &Device,
        stream_config: &crate::StreamConfig,
    ) -> std::result::Result<Box<dyn GroupMemberLink>, MemberFailure>;
}

/// Production member link wrapping a real [`Connection`].
pub(crate) struct RealConnectionLink {
    conn: Connection,
}

impl RealConnectionLink {
    pub(crate) fn new(conn: Connection) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl GroupMemberLink for RealConnectionLink {
    fn receiver(&self) -> DeviceId {
        self.conn.device().id.clone()
    }

    fn local_peer_address(&self) -> Option<String> {
        self.conn.local_addr().map(|addr| addr.ip().to_string())
    }

    async fn probe(&mut self, timeout: Duration) -> Result<Health> {
        self.conn.probe(timeout).await
    }

    fn set_render_delay_ms(&mut self, delay_ms: u32) {
        self.conn.set_render_delay_ms(delay_ms);
    }

    async fn setup_stream(&mut self) -> Result<()> {
        self.conn.setup().await
    }

    fn primary_timing_identity(&self) -> Option<GroupPrimaryTiming> {
        Some(GroupPrimaryTiming {
            updates: self.conn.ptp_clock_rx()?,
        })
    }

    async fn join_group_timing(
        &mut self,
        timing: &GroupPrimaryTiming,
        render_delay_ms: u32,
    ) -> Result<()> {
        self.conn.set_render_delay_ms(render_delay_ms);
        self.conn
            .setup_for_group_with_clock(timing.updates.clone())
            .await
    }

    async fn send_setpeers(&mut self, peer_addresses: &[String]) -> Result<()> {
        self.conn.send_setpeers(peer_addresses).await
    }

    async fn set_volume(&mut self, volume: f32) -> Result<()> {
        self.conn.set_volume(volume).await
    }

    fn worker_ledger(&self) -> WorkerLedger {
        self.conn.worker_ledger()
    }

    async fn teardown(&mut self) {
        let _ = self.conn.disconnect().await;
    }

    fn into_connection(self: Box<Self>) -> Connection {
        self.conn
    }
}

/// Production establishment: `Connection::connect_auto` with phase mapping.
pub(crate) struct RealLinkFactory;

#[async_trait]
impl GroupLinkFactory for RealLinkFactory {
    async fn connect_member(
        &self,
        device: &Device,
        stream_config: &crate::StreamConfig,
    ) -> std::result::Result<Box<dyn GroupMemberLink>, MemberFailure> {
        match Connection::connect_auto(device.clone(), stream_config.clone(), GROUP_PAIRING_PIN)
            .await
        {
            Ok(conn) => Ok(Box::new(RealConnectionLink::new(conn))),
            Err(source) => Err(classify_connect_failure(device.id.clone(), source)),
        }
    }
}

/// Map an establishment error onto its pipeline phase.
///
/// Pairing-material errors classify as [`SetupPhase::Pair`] (not retryable);
/// everything else is treated as transport-level [`SetupPhase::Connect`].
pub(crate) fn classify_connect_failure(
    receiver: DeviceId,
    source: airplay_core::Error,
) -> MemberFailure {
    let phase = match &source {
        airplay_core::Error::Pairing(_)
        | airplay_core::Error::Crypto(_)
        | airplay_core::Error::MfiRequired => SetupPhase::Pair,
        _ => SetupPhase::Connect,
    };
    let retryable = phase.is_retryable();
    MemberFailure {
        receiver,
        phase,
        retryable,
        source,
    }
}

/// Map a runtime control/probe error onto the runtime phase.
pub(crate) fn classify_runtime_failure(
    receiver: DeviceId,
    source: airplay_core::Error,
) -> MemberFailure {
    MemberFailure {
        receiver,
        phase: SetupPhase::StartAudio,
        retryable: SetupPhase::StartAudio.is_retryable(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::device::Version;
    use airplay_core::features::Features;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn make_test_device(mac: [u8; 6], name: &str) -> Device {
        Device {
            id: DeviceId(mac),
            name: name.to_string(),
            model: "AppleTV5,3".to_string(),
            manufacturer: None,
            serial_number: None,
            addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))],
            port: 7000,
            features: Features::default(),
            required_sender_features: None,
            public_key: None,
            source_version: Version::default(),
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

    mod group_creation {
        use super::*;

        #[test]
        fn new_creates_group_with_leader() {
            let device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let group = DeviceGroup::new(device.clone());

            assert_eq!(group.member_count(), 1);
            assert_eq!(group.leader().device.name, "Leader");
            assert!(group.leader().is_leader);
        }

        #[test]
        fn id_is_unique() {
            let device1 = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader1");
            let device2 = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Leader2");

            let group1 = DeviceGroup::new(device1);
            let group2 = DeviceGroup::new(device2);

            assert_ne!(group1.id(), group2.id());
        }
    }

    mod membership {
        use super::*;

        #[test]
        fn add_member_adds_device() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let member = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Member");

            let mut group = DeviceGroup::new(leader);
            assert_eq!(group.member_count(), 1);

            group.add_member(member).unwrap();
            assert_eq!(group.member_count(), 2);
        }

        #[test]
        fn add_member_not_leader() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let member = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Member");

            let mut group = DeviceGroup::new(leader);
            group.add_member(member.clone()).unwrap();

            // Find the member and verify it's not a leader
            for m in group.members() {
                if m.device.name == "Member" {
                    assert!(!m.is_leader);
                }
            }
        }

        #[test]
        fn remove_member_removes_device() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let member = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Member");
            let member_id = member.id.clone();

            let mut group = DeviceGroup::new(leader);
            group.add_member(member).unwrap();
            assert_eq!(group.member_count(), 2);

            group.remove_member(&member_id).unwrap();
            assert_eq!(group.member_count(), 1);
        }

        #[test]
        fn cannot_remove_leader() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let leader_id = leader.id.clone();

            let mut group = DeviceGroup::new(leader);
            let result = group.remove_member(&leader_id);
            assert!(result.is_err());
        }

        #[test]
        fn member_count_tracks_size() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let member1 = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Member1");
            let member2 = make_test_device([0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC], "Member2");

            let mut group = DeviceGroup::new(leader);
            assert_eq!(group.member_count(), 1);

            group.add_member(member1).unwrap();
            assert_eq!(group.member_count(), 2);

            group.add_member(member2).unwrap();
            assert_eq!(group.member_count(), 3);
        }
    }

    mod volume {
        use super::*;

        #[test]
        fn set_member_volume_updates() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let leader_id = leader.id.clone();

            let mut group = DeviceGroup::new(leader);
            group.set_member_volume(&leader_id, 0.5).unwrap();

            assert_eq!(group.leader().volume, 0.5);
        }

        #[test]
        fn volume_clamped_to_range() {
            let leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            let leader_id = leader.id.clone();

            let mut group = DeviceGroup::new(leader);

            // Test clamping above 1.0
            group.set_member_volume(&leader_id, 1.5).unwrap();
            assert_eq!(group.leader().volume, 1.0);

            // Test clamping below 0.0
            group.set_member_volume(&leader_id, -0.5).unwrap();
            assert_eq!(group.leader().volume, 0.0);
        }
    }

    mod peer_addresses {
        use super::*;

        #[test]
        fn returns_all_member_addresses() {
            let mut leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            leader.addresses = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))];

            let mut member = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Member");
            member.addresses = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101))];

            let mut group = DeviceGroup::new(leader);
            group.add_member(member).unwrap();

            let addresses = group.peer_addresses();
            assert_eq!(addresses.len(), 2);
            assert!(addresses.contains(&"192.168.1.100".to_string()));
            assert!(addresses.contains(&"192.168.1.101".to_string()));
        }

        #[test]
        fn excludes_ipv6_link_local() {
            let mut leader = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Leader");
            leader.addresses = vec![
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
                // Link-local IPv6 (fe80::)
                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
                // Global IPv6 (not link-local)
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            ];

            let group = DeviceGroup::new(leader);
            let addresses = group.peer_addresses();

            assert_eq!(addresses.len(), 2);
            assert!(addresses.contains(&"192.168.1.100".to_string()));
            assert!(addresses.contains(&"2001:db8::1".to_string()));
            // Link-local should be excluded
            assert!(!addresses.iter().any(|a| a.starts_with("fe80")));
        }
    }

    mod primary_timing {
        use super::*;
        use airplay_timing::ClockOffset;

        #[test]
        fn primary_and_secondary_receivers_observe_the_same_complete_transition() {
            let a = PtpClockState {
                master_clock_id: [0xAA; 8],
                offset: ClockOffset {
                    offset_ns: 100,
                    error_ns: 1,
                    rtt_ns: 0,
                },
            };
            let b = PtpClockState {
                master_clock_id: [0xBB; 8],
                offset: ClockOffset {
                    offset_ns: -200,
                    error_ns: 2,
                    rtt_ns: 0,
                },
            };
            let (tx, primary_rx) = tokio::sync::watch::channel(Some(a));
            let timing = GroupPrimaryTiming {
                updates: primary_rx,
            };
            let secondary_rx = timing.updates.clone();

            tx.send_replace(None);
            assert!((*timing.updates.borrow()).is_none());
            assert!((*secondary_rx.borrow()).is_none());

            tx.send_replace(Some(b));
            let primary = (*timing.updates.borrow()).expect("primary sees B");
            let secondary = (*secondary_rx.borrow()).expect("secondary sees B");
            assert_eq!(primary.master_clock_id, [0xBB; 8]);
            assert_eq!(primary.offset.offset_ns, -200);
            assert_eq!(secondary.master_clock_id, primary.master_clock_id);
            assert_eq!(secondary.offset.offset_ns, primary.offset.offset_ns);
        }
    }
}
