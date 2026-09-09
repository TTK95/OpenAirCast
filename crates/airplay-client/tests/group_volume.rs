//! Task 13: receiver-addressable member volume routing.
//!
//! `set_member_volume` must address exactly ONE member's link (never via the
//! primary), succeed for every live member independently, and fail with a
//! phase-classified [`MemberResult`] error for unknown or failed receivers.
//!
//! Everything runs offline through a scripted [`GroupLinkFactory`] against
//! localhost RTSP stubs, mirroring the fixtures in `group_connect.rs`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airplay_client::{
    AirPlayClient, Connection, Device, DeviceId, Error as CoreError, GroupLinkFactory,
    GroupMemberLink, GroupPrimaryTiming, Health, MemberFailure, SetupPhase, StreamConfig,
    WorkerLedger,
};
use airplay_core::codec::{AudioCodec, AudioFormat, SampleRate};
use airplay_core::device::Version;
use airplay_core::error::{Result as CoreResult, RtspError};
use airplay_core::features::Features;
use airplay_core::stream::{StreamType, TimingProtocol};
use airplay_timing::{ClockOffset, PtpClockState};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Localhost RTSP stubs
// ---------------------------------------------------------------------------

async fn bind_stub_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

/// Answer every complete RTSP request with `200 OK`.
fn spawn_ok_responder(listener: TcpListener) {
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(answer_every_request_with_ok(sock));
        }
    });
}

async fn answer_every_request_with_ok(mut sock: tokio::net::TcpStream) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let reply = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n";
            if sock.write_all(reply).await.is_err() {
                return;
            }
            buf.clear();
        }
    }
}

/// Bind both stub endpoint kinds and start their responders.
async fn stub_endpoints() -> StubEndpoints {
    let ok_listener = bind_stub_listener().await;
    let silent_listener = bind_stub_listener().await;
    let endpoints = StubEndpoints {
        ok: ok_listener.local_addr().unwrap(),
        silent: silent_listener.local_addr().unwrap(),
    };
    spawn_ok_responder(ok_listener);
    endpoints
}

fn stub_config() -> StreamConfig {
    StreamConfig {
        stream_type: StreamType::Buffered,
        audio_format: AudioFormat {
            codec: AudioCodec::Alac,
            sample_rate: SampleRate::Hz44100,
            channels: 2,
            bit_depth: 16,
            frames_per_packet: 352,
        },
        timing_protocol: TimingProtocol::Ntp,
        ptp_mode: airplay_core::PtpMode::Master,
        latency_min: 22050,
        latency_max: 132300,
        supports_dynamic_stream_id: true,
        sender_buffer: Default::default(),
        asc: None,
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct StubEndpoints {
    ok: SocketAddr,
    #[allow(dead_code)]
    silent: SocketAddr,
}

/// Stable IDs whose `[u8; 6]` byte order equals the tag letter order.
fn device_id(tag: char) -> DeviceId {
    let n = tag as u8 - b'a' + 1;
    DeviceId([n, 0, 0, 0, 0, n])
}

fn device(tag: char) -> Device {
    Device {
        id: device_id(tag),
        name: format!("receiver-{tag}"),
        model: "StubModel1,1".to_string(),
        manufacturer: None,
        serial_number: None,
        addresses: vec![IpAddr::from([127, 0, 0, 1])],
        port: 7000,
        features: Features::from_raw(Features::SUPPORTS_PTP),
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

fn devices(tags: &[char]) -> Vec<Device> {
    tags.iter().copied().map(device).collect()
}

fn test_client() -> AirPlayClient {
    AirPlayClient::new().expect("mDNS daemon available for offline client construction")
}

// ---------------------------------------------------------------------------
// Scripted member-link factory (the deterministic test seam)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Scripted {
    /// Established healthy member.
    Healthy,
    /// Establishes normally; one pipeline method then fails.
    FailAt(SetupPhase),
}

struct ScriptedFactory {
    scripts: HashMap<DeviceId, Scripted>,
    endpoints: StubEndpoints,
    config: StreamConfig,
    /// Every device handed to `connect_member`, in call order.
    attempts: Arc<Mutex<Vec<DeviceId>>>,
    /// Receivers whose link was torn down, in call order.
    teardowns: Arc<Mutex<Vec<DeviceId>>>,
}

impl ScriptedFactory {
    fn new(endpoints: &StubEndpoints) -> Self {
        Self {
            scripts: HashMap::new(),
            endpoints: StubEndpoints {
                ok: endpoints.ok,
                silent: endpoints.silent,
            },
            config: stub_config(),
            attempts: Arc::new(Mutex::new(Vec::new())),
            teardowns: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn fail_at(self, tag: char, phase: SetupPhase) -> Self {
        let mut scripted = self;
        scripted
            .scripts
            .insert(device_id(tag), Scripted::FailAt(phase));
        scripted
    }

    fn install(self, client: &mut AirPlayClient) -> Self {
        client.set_group_link_factory_for_test(Arc::new(ScriptedFactory {
            scripts: self.scripts.clone(),
            endpoints: StubEndpoints {
                ok: self.endpoints.ok,
                silent: self.endpoints.silent,
            },
            config: self.config.clone(),
            attempts: Arc::clone(&self.attempts),
            teardowns: Arc::clone(&self.teardowns),
        }));
        self
    }
}

#[async_trait]
impl GroupLinkFactory for ScriptedFactory {
    async fn connect_member(
        &self,
        device: &Device,
        _stream_config: &StreamConfig,
    ) -> std::result::Result<Box<dyn GroupMemberLink>, MemberFailure> {
        self.attempts.lock().unwrap().push(device.id.clone());
        let scripted = self
            .scripts
            .get(&device.id)
            .copied()
            .unwrap_or(Scripted::Healthy);
        match scripted {
            Scripted::FailAt(_) | Scripted::Healthy => {
                let mut stub_device = device.clone();
                stub_device.addresses = vec![self.endpoints.ok.ip()];
                stub_device.port = self.endpoints.ok.port();
                let ledger = WorkerLedger::new();
                let conn = Connection::connect_unpaired_test(
                    stub_device,
                    self.config.clone(),
                    ledger.clone(),
                )
                .await
                .expect("offline stub connection");
                Ok(Box::new(FakeLink {
                    receiver: device.id.clone(),
                    conn: Some(conn),
                    local_peer: "127.0.0.1".to_string(),
                    fail_at: match scripted {
                        Scripted::FailAt(phase) => Some(phase),
                        Scripted::Healthy => None,
                    },
                    ledger,
                    teardowns: Arc::clone(&self.teardowns),
                }))
            }
        }
    }
}

/// Scripted member transport backed by a real plaintext stub connection.
struct FakeLink {
    receiver: DeviceId,
    conn: Option<Connection>,
    local_peer: String,
    fail_at: Option<SetupPhase>,
    ledger: WorkerLedger,
    teardowns: Arc<Mutex<Vec<DeviceId>>>,
}

impl FakeLink {
    fn scripted_error(&self) -> CoreError {
        CoreError::Rtsp(RtspError::SetupFailed(format!(
            "scripted failure at {:?}",
            self.fail_at
        )))
    }
}

#[async_trait]
impl GroupMemberLink for FakeLink {
    fn receiver(&self) -> DeviceId {
        self.receiver.clone()
    }

    fn local_peer_address(&self) -> Option<String> {
        Some(self.local_peer.clone())
    }

    fn worker_ledger(&self) -> WorkerLedger {
        self.ledger.clone()
    }

    async fn probe(&mut self, _timeout: Duration) -> CoreResult<Health> {
        Ok(Health::Healthy)
    }

    /// Forwarded to the stub connection, so a test asserting on the stored
    /// connection observes what the production link would have applied.
    fn set_render_delay_ms(&mut self, delay_ms: u32) {
        if let Some(ref mut conn) = self.conn {
            conn.set_render_delay_ms(delay_ms);
        }
    }

    async fn setup_stream(&mut self) -> CoreResult<()> {
        Ok(())
    }

    fn primary_timing_identity(&self) -> Option<GroupPrimaryTiming> {
        let (_tx, rx) = tokio::sync::watch::channel(Some(PtpClockState {
            master_clock_id: [0x11; 8],
            offset: ClockOffset::default(),
        }));
        Some(GroupPrimaryTiming { updates: rx })
    }

    async fn join_group_timing(
        &mut self,
        _timing: &GroupPrimaryTiming,
        _render_delay_ms: u32,
    ) -> CoreResult<()> {
        if self.fail_at == Some(SetupPhase::RtspSetup) {
            return Err(self.scripted_error());
        }
        Ok(())
    }

    async fn send_setpeers(&mut self, _peer_addresses: &[String]) -> CoreResult<()> {
        if self.fail_at == Some(SetupPhase::SetPeers) {
            return Err(self.scripted_error());
        }
        Ok(())
    }

    async fn set_volume(&mut self, _volume: f32) -> CoreResult<()> {
        if self.fail_at == Some(SetupPhase::StartAudio) {
            return Err(self.scripted_error());
        }
        Ok(())
    }

    async fn teardown(&mut self) {
        self.teardowns.lock().unwrap().push(self.receiver.clone());
        if let Some(ref mut conn) = self.conn {
            let _ = conn.disconnect().await;
        }
    }

    fn into_connection(mut self: Box<Self>) -> Connection {
        self.conn
            .take()
            .expect("fake link keeps its stub connection")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_member_volume_routes_to_target_link_only() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    ScriptedFactory::new(&endpoints).install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b']), Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a'), device_id('b')]);

    // Address the SECONDARY directly; the primary stays untouched.
    let result = client.set_member_volume(&device_id('b'), 0.3).await;
    assert_eq!(result.receiver, device_id('b'));
    assert!(result.result.is_ok(), "secondary volume must apply");
    assert_eq!(client.member_volume(&device_id('b')), Some(0.3));
    assert_eq!(client.member_volume(&device_id('a')), Some(1.0));

    // And the primary itself via its stable ID — still member-local routing,
    // leaving the secondary exactly where it was.
    let result = client.set_member_volume(&device_id('a'), 0.75).await;
    assert_eq!(result.receiver, device_id('a'));
    assert!(result.result.is_ok(), "primary volume must apply");
    assert_eq!(client.member_volume(&device_id('a')), Some(0.75));
    assert_eq!(client.member_volume(&device_id('b')), Some(0.3));

    // Unknown receivers fail without touching any live member.
    let unknown = device_id('z');
    let result = client.set_member_volume(&unknown, 0.5).await;
    assert_eq!(result.receiver, unknown);
    let failure = result.result.expect_err("unknown receiver must fail");
    assert_eq!(failure.phase, SetupPhase::StartAudio);
    assert!(failure.retryable);
    assert_eq!(client.member_volume(&unknown), None);
    assert_eq!(client.member_volume(&device_id('b')), Some(0.3));
    assert_eq!(client.member_volume(&device_id('a')), Some(0.75));
}

#[tokio::test]
async fn volume_on_failed_member_returns_member_result_err() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    ScriptedFactory::new(&endpoints)
        .fail_at('b', SetupPhase::SetPeers)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // 'b' failed member-locally and owns no live link anymore.
    assert_eq!(report.connected, vec![device_id('a'), device_id('c')]);
    assert_eq!(
        client.member_volume(&device_id('b')),
        None,
        "failed member has no stored connection"
    );

    // Volume addressed at the failed receiver is receiver-scoped: an
    // Err(MemberFailure) for THAT receiver only, others unaffected.
    let result = client.set_member_volume(&device_id('b'), 0.5).await;
    assert_eq!(result.receiver, device_id('b'));
    let failure = result.result.expect_err("failed member volume errors");
    assert_eq!(failure.receiver, device_id('b'));
    assert_eq!(failure.phase, SetupPhase::StartAudio);
    assert!(failure.retryable);
    assert!(matches!(failure.source, CoreError::Rtsp(_)));

    // The rest of the group still accepts receiver-addressable control.
    let result = client.set_member_volume(&device_id('c'), 0.6).await;
    assert!(result.result.is_ok());
    assert_eq!(client.member_volume(&device_id('c')), Some(0.6));
}
