//! Tasks 12/13: typed group transport, deterministic primary selection,
//! best-effort per-member failure isolation, one primary fallback, bounded
//! parallel establishment, and the single-survivor NTP reconnect.
//!
//! Everything here runs without receivers or a real network: member
//! establishment goes through a scripted [`GroupLinkFactory`], and the only
//! sockets are localhost RTSP stubs backing the stored connections.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airplay_client::{
    choose_primary, AirPlayClient, Connection, Device, DeviceId, Error as CoreError,
    GroupLinkFactory, GroupMemberLink, GroupPrimaryTiming, Health, MemberFailure, MemberResult,
    SetupPhase, StreamConfig, WorkerLedger,
};
use airplay_core::codec::{AudioCodec, AudioFormat, SampleRate};
use airplay_core::device::Version;
use airplay_core::error::{PairingError, Result as CoreResult, RtspError};
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

/// Accept connections that are held open but never answered.
fn spawn_silent_endpoint(listener: TcpListener) {
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _held = sock;
                std::future::pending::<()>().await;
            });
        }
    });
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
    spawn_silent_endpoint(silent_listener);
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
    silent: SocketAddr,
}

/// Stable IDs whose `[u8; 6]` byte order equals the tag letter order, so
/// "lexicographically smallest ID" == "smallest tag".
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
    /// Established member whose probe answers Healthy.
    Healthy,
    /// Established member whose probe times out (unhealthy).
    TimedOut,
    /// Established member whose probe fails hard with an RTSP error.
    ///
    /// This is what a receiver does with a keepalive that names a session it
    /// has never been told about: it answers something other than 200, which
    /// `Connection::probe` surfaces as `Err`, not as a health value. The
    /// member itself is perfectly alive -- every later phase succeeds.
    ProbeRejected,
    /// Establishment fails during HomeKit pairing.
    PairRejected,
    /// Transport-level rejection before any link exists.
    ConnectRejected,
    /// Establishes normally; one pipeline method then fails.
    FailAt(SetupPhase),
}

/// RAII in-flight tracker for bounded-concurrency observation.
struct InFlightGuard {
    counter: Arc<AtomicUsize>,
}

impl InFlightGuard {
    fn new(counter: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) -> Self {
        let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(value, Ordering::SeqCst);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ScriptedFactory {
    scripts: HashMap<DeviceId, Scripted>,
    endpoints: StubEndpoints,
    config: StreamConfig,
    /// Every device handed to `connect_member`, in call order.
    attempts: Arc<Mutex<Vec<DeviceId>>>,
    /// Receivers whose link was torn down, in call order.
    teardowns: Arc<Mutex<Vec<DeviceId>>>,
    /// Peer list handed to each receiver via SETPEERS.
    setpeers: Arc<Mutex<HashMap<DeviceId, Vec<String>>>>,
    /// Per-establishment pacing to expose overlapping setups.
    pace: Duration,
    /// Currently in-flight `connect_member` calls.
    in_flight: Arc<AtomicUsize>,
    /// Peak concurrent in-flight value ever observed.
    peak_in_flight: Arc<AtomicUsize>,
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
            setpeers: Arc::new(Mutex::new(HashMap::new())),
            pace: Duration::ZERO,
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn script(mut self, tag: char, scripted: Scripted) -> Self {
        self.scripts.insert(device_id(tag), scripted);
        self
    }

    fn fail_at(self, tag: char, phase: SetupPhase) -> Self {
        self.script(tag, Scripted::FailAt(phase))
    }

    fn with_pace(mut self, pace: Duration) -> Self {
        self.pace = pace;
        self
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
            setpeers: Arc::clone(&self.setpeers),
            pace: self.pace,
            in_flight: Arc::clone(&self.in_flight),
            peak_in_flight: Arc::clone(&self.peak_in_flight),
        }));
        self
    }

    fn attempts(&self) -> Vec<DeviceId> {
        self.attempts.lock().unwrap().clone()
    }

    fn teardowns(&self) -> Vec<DeviceId> {
        self.teardowns.lock().unwrap().clone()
    }

    /// Peer list this receiver was handed via SETPEERS, sorted for comparison.
    fn setpeers_for(&self, receiver: &DeviceId) -> Vec<String> {
        let mut peers = self
            .setpeers
            .lock()
            .unwrap()
            .get(receiver)
            .cloned()
            .unwrap_or_default();
        peers.sort();
        peers
    }

    fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
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
        let _guard = InFlightGuard::new(
            Arc::clone(&self.in_flight),
            Arc::clone(&self.peak_in_flight),
        );
        if !self.pace.is_zero() {
            tokio::time::sleep(self.pace).await;
        }
        match scripted {
            Scripted::PairRejected => Err(MemberFailure {
                receiver: device.id.clone(),
                phase: SetupPhase::Pair,
                retryable: false,
                source: CoreError::Pairing(PairingError::Rejected),
            }),
            Scripted::ConnectRejected => Err(MemberFailure {
                receiver: device.id.clone(),
                phase: SetupPhase::Connect,
                retryable: true,
                source: CoreError::Rtsp(RtspError::ConnectionRefused),
            }),
            Scripted::Healthy
            | Scripted::TimedOut
            | Scripted::ProbeRejected
            | Scripted::FailAt(_) => {
                let addr = match scripted {
                    Scripted::TimedOut => self.endpoints.silent,
                    _ => self.endpoints.ok,
                };
                let mut stub_device = device.clone();
                stub_device.addresses = vec![addr.ip()];
                stub_device.port = addr.port();
                let ledger = WorkerLedger::new();
                let mut conn = Connection::connect_unpaired_test(
                    stub_device,
                    self.config.clone(),
                    ledger.clone(),
                )
                .await
                .expect("offline stub connection");
                // One session worker per established member: while the
                // connection lives it counts as unfinished work; teardown
                // cancels and joins it deterministically.
                conn.register_worker(|token| async move {
                    token.cancelled().await;
                });
                let probe = match scripted {
                    Scripted::TimedOut => ScriptedProbe::Answers(Health::TimedOut),
                    Scripted::ProbeRejected => ScriptedProbe::Rejects,
                    _ => ScriptedProbe::Answers(Health::Healthy),
                };
                Ok(Box::new(FakeLink {
                    receiver: device.id.clone(),
                    conn: Some(conn),
                    probe,
                    local_peer: "127.0.0.1".to_string(),
                    fail_at: match scripted {
                        Scripted::FailAt(phase) => Some(phase),
                        _ => None,
                    },
                    ledger,
                    teardowns: Arc::clone(&self.teardowns),
                    setpeers: Arc::clone(&self.setpeers),
                }))
            }
        }
    }
}

/// What a scripted member's health probe does when it is called.
#[derive(Clone, Copy)]
enum ScriptedProbe {
    /// Answers with a health value, exactly as `Connection::probe` does on a
    /// clean RTSP 200 or on silence.
    Answers(Health),
    /// Fails with a protocol error, as `Connection::probe` does on any status
    /// other than 200.
    Rejects,
}

/// Scripted member transport: virtual control-plane results backed by a real
/// plaintext connection so the client can store something concrete.
struct FakeLink {
    receiver: DeviceId,
    conn: Option<Connection>,
    probe: ScriptedProbe,
    local_peer: String,
    /// Pipeline method that fails once established (`None` = never fails).
    fail_at: Option<SetupPhase>,
    ledger: WorkerLedger,
    teardowns: Arc<Mutex<Vec<DeviceId>>>,
    /// Peer list this member was handed via SETPEERS, per receiver.
    setpeers: Arc<Mutex<HashMap<DeviceId, Vec<String>>>>,
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
        match self.probe {
            ScriptedProbe::Answers(health) => Ok(health),
            ScriptedProbe::Rejects => Err(CoreError::Rtsp(RtspError::SetupFailed(
                "probe returned RTSP 454".into(),
            ))),
        }
    }

    /// Forwarded to the stub connection, so a test asserting on the stored
    /// connection observes what the production link would have applied.
    fn set_render_delay_ms(&mut self, delay_ms: u32) {
        if let Some(ref mut conn) = self.conn {
            conn.set_render_delay_ms(delay_ms);
        }
    }

    async fn setup_stream(&mut self) -> CoreResult<()> {
        if self.fail_at == Some(SetupPhase::RtspSetup) {
            return Err(self.scripted_error());
        }
        Ok(())
    }

    fn primary_timing_identity(&self) -> Option<GroupPrimaryTiming> {
        if self.fail_at == Some(SetupPhase::PrimaryTiming) {
            return None;
        }
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

    async fn send_setpeers(&mut self, peer_addresses: &[String]) -> CoreResult<()> {
        if self.fail_at == Some(SetupPhase::SetPeers) {
            return Err(self.scripted_error());
        }
        self.setpeers
            .lock()
            .unwrap()
            .insert(self.receiver.clone(), peer_addresses.to_vec());
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
async fn preferred_healthy_member_is_primary_regardless_of_input_order() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints).install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['b', 'a', 'c']), Some(&device_id('c')))
        .await
        .unwrap();

    assert_eq!(report.primary, Some(device_id('c')));
    // Primary leads the connected list; remaining members follow in input order.
    assert_eq!(
        report.connected,
        vec![device_id('c'), device_id('b'), device_id('a')]
    );
    assert!(report.failures.is_empty());
    drop(factory);
}

#[tokio::test]
async fn smallest_ptp_capable_id_wins_without_preference_or_when_preferred_fails_setup() {
    let endpoints = stub_endpoints().await;

    // Without preference: lexicographically smallest PTP-capable ID.
    let mut client = test_client();
    ScriptedFactory::new(&endpoints).install(&mut client);
    let report = client
        .connect_group_best_effort(&devices(&['c', 'a', 'b']), None)
        .await
        .unwrap();
    assert_eq!(report.primary, Some(device_id('a')));

    // Preferred member present but unable to serve the role: it loses the
    // preference at the phase that actually needs the receiver, and the same
    // deterministic fallback applies.
    let mut client = test_client();
    ScriptedFactory::new(&endpoints)
        .fail_at('a', SetupPhase::RtspSetup)
        .install(&mut client);
    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.primary, Some(device_id('b')));
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('a'));
    assert_eq!(report.connected, vec![device_id('b'), device_id('c')]);
}

#[tokio::test]
async fn member_failures_carry_phase_classification() {
    let endpoints = stub_endpoints().await;

    // Pair-stage rejection keeps its phase classification.
    let mut client = test_client();
    ScriptedFactory::new(&endpoints)
        .script('b', Scripted::PairRejected)
        .install(&mut client);
    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('b'));
    assert_eq!(report.failures[0].phase, SetupPhase::Pair);
    assert!(!report.failures[0].retryable);
    assert_eq!(report.primary, Some(device_id('a')));

    // Receiver-addressable control on an unknown member classifies StartAudio.
    let unknown = device_id('z');
    let MemberResult { receiver, result } = client.set_member_volume(&unknown, 0.5).await;
    assert_eq!(receiver, unknown);
    let failure = result.expect_err("unknown member must fail");
    assert_eq!(failure.phase, SetupPhase::StartAudio);
    assert!(failure.retryable);
    assert!(matches!(failure.source, CoreError::Rtsp(_)));
}

#[tokio::test]
async fn duplicates_are_rejected_upfront() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints).install(&mut client);

    let result = client
        .connect_group_best_effort(&devices(&['a', 'b', 'a']), Some(&device_id('a')))
        .await;
    assert!(result.is_err(), "duplicate member IDs must be rejected");

    let result = client.connect_group_best_effort(&[], None).await;
    assert!(result.is_err(), "empty membership must be rejected");

    // Rejection happens before any member establishment attempt.
    assert!(factory.attempts().is_empty());
    assert!(factory.teardowns().is_empty());
}

#[tokio::test]
async fn best_effort_returns_connected_plus_failures_without_aborting_remaining_members() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .script('b', Scripted::PairRejected)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // The mid-list failure neither aborts 'c' nor displaces the primary.
    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a'), device_id('c')]);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('b'));
    assert_eq!(report.failures[0].phase, SetupPhase::Pair);

    // All three devices were attempted in input order. A Pair-stage failure
    // owns no established link, so there is nothing to tear down.
    assert_eq!(
        factory.attempts(),
        vec![device_id('a'), device_id('b'), device_id('c')]
    );
    assert!(factory.teardowns().is_empty());
    drop(factory);
}

#[tokio::test]
async fn probe_members_maps_health_per_receiver() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    ScriptedFactory::new(&endpoints)
        .script('b', Scripted::TimedOut)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.primary, Some(device_id('a')));

    let results = client.probe_members().await;
    let by_receiver: HashMap<DeviceId, Health> = results
        .into_iter()
        .map(|MemberResult { receiver, result }| {
            (receiver, result.expect("probe outcome is a health value"))
        })
        .collect();
    assert_eq!(by_receiver.get(&device_id('a')), Some(&Health::Healthy));
    assert_eq!(by_receiver.get(&device_id('b')), Some(&Health::TimedOut));
    assert_eq!(by_receiver.get(&device_id('c')), Some(&Health::Healthy));
}

// ---------------------------------------------------------------------------
// Primary eligibility is decided by SETUP, not by a pre-SETUP keepalive
// ---------------------------------------------------------------------------

/// The same device reachable at a distinct routable address, so a peer list
/// built from the wrong set is visible in the addresses themselves.
fn device_at(tag: char, last_octet: u8) -> Device {
    let mut device = device(tag);
    device.addresses = vec![IpAddr::from([10, 0, 0, last_octet])];
    device
}

/// A receiver that refuses a keepalive naming a session it has never been
/// told about is not an unreachable receiver -- it is a receiver that has not
/// been through SETUP yet, which is every member at that point in the flow.
///
/// `POST <session-uri>/feedback` is step 6 of the baseline request flow
/// (AIRPLAY_2_SPEC.md 4.2); nothing before SETUP may hinge on it, and the
/// spec explicitly requires senders to tolerate 4xx (4.3). If every member's
/// pre-SETUP probe is allowed to veto primary eligibility, no member is
/// eligible and a group of perfectly healthy speakers never forms at all --
/// which is exactly the "only one speaker ever works" symptom, because the
/// single-receiver path never probes.
#[tokio::test]
async fn members_rejecting_a_pre_setup_keepalive_still_form_a_group() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .script('a', Scripted::ProbeRejected)
        .script('b', Scripted::ProbeRejected)
        .script('c', Scripted::ProbeRejected)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), None)
        .await
        .unwrap();

    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(
        report.connected,
        vec![device_id('a'), device_id('b'), device_id('c')]
    );
    assert!(report.failures.is_empty());
    // Nobody was excluded, so nobody was torn down.
    assert!(factory.teardowns().is_empty());
    drop(factory);
}

/// Silence to a pre-SETUP keepalive is just as uninformative as a rejection:
/// the member that would be chosen by the deterministic rule stays chosen.
#[tokio::test]
async fn silence_to_a_pre_setup_keepalive_does_not_move_the_primary() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    ScriptedFactory::new(&endpoints)
        .script('a', Scripted::TimedOut)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected.len(), 3);
    assert!(report.failures.is_empty());
}

/// The other direction, which the fix must not break: a member that really is
/// unreachable fails the handshake that actually needs the receiver, and the
/// group forms around the next deterministic candidate instead of collapsing.
#[tokio::test]
async fn a_member_that_fails_setup_never_blocks_the_group_as_primary() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('a', SetupPhase::RtspSetup)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // 'a' was tried first and lost the role to the lowest remaining ID.
    assert_eq!(
        client.primary_attempts(),
        vec![device_id('a'), device_id('b')]
    );
    assert_eq!(report.primary, Some(device_id('b')));
    assert_eq!(report.connected, vec![device_id('b'), device_id('c')]);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('a'));
    assert_eq!(report.failures[0].phase, SetupPhase::RtspSetup);
    // The dead member is gone, not silently carried as a live secondary.
    assert_eq!(factory.teardowns(), vec![device_id('a')]);
    assert!(!report.connected.contains(&device_id('a')));
    drop(factory);
}

/// SETPEERS describes the timing domain the receivers are supposed to align
/// against. Naming a member that never established puts an address in that
/// domain which no receiver can reach, so the peer list must be built from
/// the members that hold a live link, not from the requested membership.
#[tokio::test]
async fn setpeers_names_only_members_that_established() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .script('b', Scripted::PairRejected)
        .install(&mut client);

    let requested = vec![device_at('a', 1), device_at('b', 2), device_at('c', 3)];
    let report = client
        .connect_group_best_effort(&requested, Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a'), device_id('c')]);

    // 'b' never established: its address must appear in nobody's peer list.
    let expected = vec![
        "10.0.0.1".to_string(),
        "10.0.0.3".to_string(),
        "127.0.0.1".to_string(),
    ];
    assert_eq!(factory.setpeers_for(&device_id('a')), expected);
    assert_eq!(factory.setpeers_for(&device_id('c')), expected);
    drop(factory);
}

// ---------------------------------------------------------------------------
// Task 13: best-effort setup, primary fallback, survivor NTP, bounded parallelism
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secondary_failure_keeps_other_members_playing() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('b', SetupPhase::RtspSetup)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // 'b' failed alone; 'a' and 'c' stay live.
    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a'), device_id('c')]);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('b'));
    assert_eq!(report.failures[0].phase, SetupPhase::RtspSetup);

    // Member-local teardown: exactly the failed member was torn down...
    assert_eq!(factory.teardowns(), vec![device_id('b')]);
    // ...its workers are fully joined while survivors keep theirs running.
    let failed_ledger = client
        .worker_ledger(&device_id('b'))
        .expect("failed member keeps its ledger for inspection");
    assert_eq!(
        failed_ledger.running(),
        0,
        "no unfinished work after member teardown"
    );
    let survivor_ledger = client
        .worker_ledger(&device_id('a'))
        .expect("survivor has a ledger");
    assert!(
        survivor_ledger.running() > 0,
        "live members keep their session workers"
    );
    drop(factory);
}

#[tokio::test]
async fn failed_primary_gets_one_deterministic_fallback_attempt() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('a', SetupPhase::SetPeers)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // Exactly one deterministic fallback: lowest remaining stable ID.
    assert_eq!(report.primary, Some(device_id('b')));
    assert_eq!(report.connected, vec![device_id('b'), device_id('c')]);
    assert_eq!(
        client.primary_attempts(),
        vec![device_id('a'), device_id('b')]
    );
    // The failed primary is phase-classified and torn down member-locally.
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('a'));
    assert_eq!(report.failures[0].phase, SetupPhase::SetPeers);
    assert_eq!(factory.teardowns(), vec![device_id('a')]);
    // The fallback reused already-established links: no extra factory connect.
    assert_eq!(
        factory.attempts(),
        vec![device_id('a'), device_id('b'), device_id('c')]
    );
    drop(factory);
}

#[tokio::test]
async fn second_primary_failure_yields_report_without_primary() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('a', SetupPhase::SetPeers)
        .fail_at('b', SetupPhase::SetPeers)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
        .await
        .unwrap();

    // Both phase-classified failures, no third attempt even though 'c'
    // remains healthy and established.
    assert_eq!(report.primary, None);
    assert!(report.connected.is_empty());
    assert_eq!(
        client.primary_attempts(),
        vec![device_id('a'), device_id('b')]
    );
    assert_eq!(report.failures.len(), 2);
    assert_eq!(report.failures[0].receiver, device_id('a'));
    assert_eq!(report.failures[0].phase, SetupPhase::SetPeers);
    assert_eq!(report.failures[1].receiver, device_id('b'));
    assert_eq!(report.failures[1].phase, SetupPhase::SetPeers);
    // Whole attempt abandoned: every established link torn down, none leaking.
    assert_eq!(
        factory.teardowns(),
        vec![device_id('a'), device_id('b'), device_id('c')]
    );
    let leftover = client
        .worker_ledger(&device_id('c'))
        .expect("'c' was established");
    assert_eq!(leftover.running(), 0);
    drop(factory);
}

#[tokio::test]
async fn single_survivor_reconnects_timing_via_ntp_ledger() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('b', SetupPhase::RtspSetup)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b']), Some(&device_id('a')))
        .await
        .unwrap();

    // Only 'a' survives; it stays live as a single-receiver session.
    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a')]);

    // Timing selection ledger: PTP during the group attempt, then the
    // single-survivor reconnect through the NTP path.
    let ledger = client.timing_protocol_ledger();
    assert_eq!(
        ledger.get(&device_id('a')),
        Some(&vec![TimingProtocol::Ptp, TimingProtocol::Ntp])
    );

    // The partial PTP attempt was torn down ('b' failed first, then 'a''s
    // partial session) and 'a' was re-established through the factory once.
    assert_eq!(factory.teardowns(), vec![device_id('b'), device_id('a')]);
    let mut expected_attempts = vec![device_id('a'), device_id('b')];
    expected_attempts.push(device_id('a'));
    assert_eq!(factory.attempts(), expected_attempts);

    // The reconnected member owns a fresh live session worker; 'b' is joined.
    let reconnected = client
        .worker_ledger(&device_id('a'))
        .expect("reconnected member ledger refreshed");
    assert!(reconnected.running() > 0);
    let failed = client
        .worker_ledger(&device_id('b'))
        .expect("failed member ledger");
    assert_eq!(failed.running(), 0);
    drop(factory);
}

/// The render lead is the only buffer between encoding a frame and the
/// instant the receiver is told to play it. A session that streams with zero
/// lead has no headroom for a retransmit and drops out audibly on the first
/// lost packet, so the configured value has to reach the connection that
/// actually streams -- including the single-receiver survivor path, which is
/// the one the device backend drives whenever exactly one receiver is live.
#[tokio::test]
async fn single_survivor_session_streams_with_the_configured_render_lead() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    client.set_render_delay_ms(275);
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('b', SetupPhase::RtspSetup)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b']), Some(&device_id('a')))
        .await
        .unwrap();
    assert_eq!(report.connected, vec![device_id('a')]);

    assert_eq!(
        client.member_render_delay_ms(&device_id('a')),
        Some(275),
        "the lone survivor streams without any buffer headroom"
    );
    drop(factory);
}

#[tokio::test]
async fn parallel_setups_bounded_at_four() {
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .with_pace(Duration::from_millis(30))
        .install(&mut client);

    let tags: Vec<char> = vec!['a', 'b', 'c', 'd', 'e', 'f'];
    let report = client
        .connect_group_best_effort(&devices(&tags), None)
        .await
        .unwrap();

    // All six members establish despite the concurrency bound...
    assert_eq!(factory.attempts().len(), 6);
    assert_eq!(report.connected.len(), 6);
    // ...setups actually overlap but never exceed four in flight.
    let peak = factory.peak_in_flight();
    assert!(peak >= 2, "establishment should overlap, peak was {peak}");
    assert!(
        peak <= 4,
        "in-flight setups must be capped at MAX_PARALLEL_SETUPS=4, peak was {peak}"
    );
    drop(factory);
}

#[tokio::test]
async fn failed_member_teardown_completes_for_every_phase() {
    // Secondary-role phases that can fail mid-establishment.
    let secondary_phases = [
        SetupPhase::Connect,
        SetupPhase::Pair,
        SetupPhase::RtspSetup,
        SetupPhase::SetPeers,
    ];
    for phase in secondary_phases {
        let endpoints = stub_endpoints().await;
        let mut client = test_client();
        let scripted = match phase {
            SetupPhase::Connect => Scripted::ConnectRejected,
            SetupPhase::Pair => Scripted::PairRejected,
            other => Scripted::FailAt(other),
        };
        let factory = ScriptedFactory::new(&endpoints)
            .script('b', scripted)
            .install(&mut client);

        let report = client
            .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('a')))
            .await
            .unwrap();

        // Best effort: the failure stays isolated to 'b'.
        assert_eq!(report.primary, Some(device_id('a')), "phase {phase:?}");
        assert_eq!(
            report.connected,
            vec![device_id('a'), device_id('c')],
            "phase {phase:?}"
        );
        assert_eq!(
            report.failures[0].receiver,
            device_id('b'),
            "phase {phase:?}"
        );
        assert_eq!(report.failures[0].phase, phase, "phase {phase:?}");

        match client.worker_ledger(&device_id('b')) {
            // Never-established members own no ledger and no teardown work.
            None => assert!(
                !factory.teardowns().contains(&device_id('b')),
                "phase {phase:?}: nothing established, nothing to tear down"
            ),
            Some(ledger) => {
                assert_eq!(
                    ledger.running(),
                    0,
                    "phase {phase:?}: teardown joined all member work"
                );
                assert!(
                    factory.teardowns().contains(&device_id('b')),
                    "phase {phase:?}: teardown recorded"
                );
            }
        }
        // Survivors remain live with their workers running.
        let survivor = client
            .worker_ledger(&device_id('a'))
            .expect("survivor ledger");
        assert!(survivor.running() > 0, "phase {phase:?}");
        drop(factory);
    }

    // Primary-critical timing failure on the PREFERRED member: one fallback
    // round promotes the next candidate and tears only 'b' down.
    let endpoints = stub_endpoints().await;
    let mut client = test_client();
    let factory = ScriptedFactory::new(&endpoints)
        .fail_at('b', SetupPhase::PrimaryTiming)
        .install(&mut client);

    let report = client
        .connect_group_best_effort(&devices(&['a', 'b', 'c']), Some(&device_id('b')))
        .await
        .unwrap();

    assert_eq!(report.primary, Some(device_id('a')));
    assert_eq!(report.connected, vec![device_id('a'), device_id('c')]);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].receiver, device_id('b'));
    assert_eq!(report.failures[0].phase, SetupPhase::PrimaryTiming);
    assert_eq!(
        client.primary_attempts(),
        vec![device_id('b'), device_id('a')]
    );
    assert_eq!(factory.teardowns(), vec![device_id('b')]);
    let failed = client.worker_ledger(&device_id('b')).expect("ledger");
    assert_eq!(failed.running(), 0);
    drop(factory);
}

// ---------------------------------------------------------------------------
// Task 16 Step 6: the successor is a function of the surviving set alone
// ---------------------------------------------------------------------------

/// Every ordering of `items`, generated by insertion so the enumeration
/// itself carries no bias towards the sorted order.
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let Some((head, tail)) = items.split_first() else {
        return vec![Vec::new()];
    };
    let mut out = Vec::new();
    for rest in permutations(tail) {
        for slot in 0..=rest.len() {
            let mut candidate = rest.clone();
            candidate.insert(slot, head.clone());
            out.push(candidate);
        }
    }
    out
}

/// The same device with the PTP feature bit cleared: discoverable, healthy,
/// and streamable as a secondary, but unable to serve group timing.
fn without_ptp_bit(tag: char) -> Device {
    let mut device = device(tag);
    device.features = Features::from_raw(0);
    device
}

/// The same device with a firmware too old for PTP, which `supports_ptp()`
/// rejects through the version half of the predicate rather than the bit.
fn too_old_for_ptp(tag: char) -> Device {
    let mut device = device(tag);
    device.source_version = Version::new(365, 99, 99);
    device
}

/// Step 6 says the successor is "the lexicographically lowest PTP-capable
/// stable ID". Both halves of that phrase are load-bearing and neither may
/// depend on the order the survivors are reported in: a group whose timing
/// master cannot speak PTP has no timing at all, and a successor that varies
/// with report order turns one failure into an unpredictable regroup.
///
/// Exhaustive over all 24 orderings of a survivor set whose lowest two IDs
/// are both PTP-incapable -- one by feature bit, one by firmware version.
#[test]
fn the_successor_is_the_lowest_ptp_capable_id_in_every_input_order() {
    let survivors = vec![
        without_ptp_bit('a'),
        too_old_for_ptp('b'),
        device('c'),
        device('d'),
    ];

    for order in permutations(&survivors) {
        let seen: Vec<DeviceId> = order.iter().map(|device| device.id.clone()).collect();

        assert_eq!(
            choose_primary(&order, None).map(|device| device.id.clone()),
            Some(device_id('c')),
            "order {seen:?} produced a different successor without a preference"
        );

        // A preferred member that cannot serve PTP loses its preference
        // entirely -- it does not merely slip one place.
        assert_eq!(
            choose_primary(&order, Some(&device_id('a'))).map(|device| device.id.clone()),
            Some(device_id('c')),
            "order {seen:?} honoured a preference for a non-PTP member"
        );
        assert_eq!(
            choose_primary(&order, Some(&device_id('b'))).map(|device| device.id.clone()),
            Some(device_id('c')),
            "order {seen:?} honoured a preference for a member too old for PTP"
        );

        // A preferred member that can serve PTP is honoured from any position,
        // including one that is not the lowest.
        assert_eq!(
            choose_primary(&order, Some(&device_id('d'))).map(|device| device.id.clone()),
            Some(device_id('d')),
            "order {seen:?} overrode a healthy PTP-capable preference"
        );

        // A preference naming a member that did not survive falls back to the
        // same deterministic choice rather than to the input order.
        assert_eq!(
            choose_primary(&order, Some(&device_id('z'))).map(|device| device.id.clone()),
            Some(device_id('c')),
            "order {seen:?} fell back by input order after an absent preference"
        );
    }

    // No survivor can serve PTP: there is no primary, rather than a silent
    // downgrade to a member that cannot hold the group clock.
    let no_timing = vec![without_ptp_bit('a'), too_old_for_ptp('b')];
    for order in permutations(&no_timing) {
        assert!(
            choose_primary(&order, None).is_none(),
            "a group with no PTP-capable survivor elected a primary anyway"
        );
        assert!(
            choose_primary(&order, Some(&device_id('a'))).is_none(),
            "a preference resurrected a member that cannot serve PTP"
        );
    }
}
