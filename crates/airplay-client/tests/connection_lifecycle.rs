//! Connection lifecycle tests: health probe, idempotent bounded disconnect,
//! and the worker-leak regression across repeated connect/teardown cycles.
//!
//! Everything here is hardware/network-independent: all endpoints are
//! localhost TCP listeners stubbing a minimal RTSP responder.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use airplay_client::{Connection, Health, WorkerLedger};
use airplay_core::codec::{AudioCodec, AudioFormat, SampleRate};
use airplay_core::device::{DeviceId, Version};
use airplay_core::features::Features;
use airplay_core::stream::{StreamType, TimingProtocol};
use airplay_core::{Device, StreamConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Local RTSP stub endpoints
// ---------------------------------------------------------------------------

async fn bind_stub_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

/// Accept connections forever; answer every complete RTSP request with 200 OK.
async fn spawn_ok_responder(listener: TcpListener) {
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(answer_every_request_with_ok(sock));
        }
    });
}

async fn answer_every_request_with_ok(mut sock: TcpStream) {
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

/// Accept exactly one connection that never reads or writes anything.
async fn spawn_silent_endpoint(listener: TcpListener) {
    tokio::spawn(async move {
        if let Ok((sock, _)) = listener.accept().await {
            // Hold the socket open but never respond.
            let held = tokio::spawn(async move {
                let _sock = sock;
                std::future::pending::<()>().await;
            });
            let _ = held.await;
        }
    });
}

// ---------------------------------------------------------------------------
// Offline test device/config/connection construction
// ---------------------------------------------------------------------------

fn stub_device(addr: SocketAddr) -> Device {
    Device {
        id: DeviceId::from_mac_string("AA:BB:CC:DD:EE:FF").unwrap(),
        name: "Lifecycle Stub".to_string(),
        addresses: vec!["127.0.0.1".parse().unwrap()],
        port: addr.port(),
        model: "StubModel1,1".to_string(),
        manufacturer: None,
        serial_number: None,
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

/// Build an offline Connection against a local stub endpoint.
async fn stub_connection(addr: SocketAddr, ledger: WorkerLedger) -> Connection {
    Connection::connect_unpaired_test(stub_device(addr), stub_config(), ledger)
        .await
        .unwrap()
}

/// A cancellation-aware long-running worker registered into the connection.
fn register_idle_workers(conn: &mut Connection, count: usize) {
    for _ in 0..count {
        conn.register_worker(|token| async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                    _ = token.cancelled() => break,
                }
            }
        });
    }
}

/// A cancellation-aware long-running OS-thread worker.
fn register_idle_thread_worker(conn: &mut Connection) {
    conn.register_thread_worker(|token| loop {
        if token.is_cancelled() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    });
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn probe_returns_timed_out_on_silent_endpoint() {
    let listener = bind_stub_listener().await;
    let addr = listener.local_addr().unwrap();
    spawn_silent_endpoint(listener).await;

    let ledger = WorkerLedger::new();
    let mut conn = stub_connection(addr, ledger).await;

    let started = Instant::now();
    let health = conn.probe(Duration::from_millis(200)).await.unwrap();
    assert_eq!(health, Health::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn probe_returns_healthy_on_rtsp_ok() {
    let listener = bind_stub_listener().await;
    let addr = listener.local_addr().unwrap();
    spawn_ok_responder(listener).await;

    let ledger = WorkerLedger::new();
    let mut conn = stub_connection(addr, ledger).await;

    let health = conn.probe(Duration::from_secs(5)).await.unwrap();
    assert_eq!(health, Health::Healthy);
}

// ---------------------------------------------------------------------------
// Idempotent bounded disconnect with owned workers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disconnect_is_idempotent_and_cancels_workers_within_budget() {
    let listener = bind_stub_listener().await;
    let addr = listener.local_addr().unwrap();
    spawn_ok_responder(listener).await;

    let ledger = WorkerLedger::new();
    let mut conn = stub_connection(addr, ledger.clone()).await;
    register_idle_workers(&mut conn, 3);
    register_idle_thread_worker(&mut conn);
    assert_eq!(ledger.running(), 4);

    let started = Instant::now();
    conn.disconnect().await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "disconnect exceeded the 4 s budget: {:?}",
        elapsed
    );

    let second_started = Instant::now();
    conn.disconnect().await.unwrap();
    assert!(
        second_started.elapsed() < Duration::from_millis(200),
        "second disconnect must return immediately"
    );

    ledger.assert_all_joined();
    assert_eq!(ledger.running(), 0);

    // After teardown the transport is closed: probe fails instead of hanging.
    let probe_started = Instant::now();
    let result = conn.probe(Duration::from_secs(5)).await;
    assert!(result.is_err(), "post-disconnect probe must fail");
    assert!(probe_started.elapsed() < Duration::from_secs(1));
}

// ---------------------------------------------------------------------------
// Leak regression across repeated cycles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_ledger_detects_leak_across_twenty_five_cycles() {
    let listener = bind_stub_listener().await;
    let addr = listener.local_addr().unwrap();
    spawn_ok_responder(listener).await;

    let ledger = WorkerLedger::new();
    for cycle in 0..25 {
        let mut conn = stub_connection(addr, ledger.clone()).await;
        register_idle_workers(&mut conn, 2);
        register_idle_thread_worker(&mut conn);
        conn.disconnect().await.unwrap();

        assert_eq!(
            ledger.running(),
            0,
            "cycle {} leaked unfinished workers",
            cycle
        );
        ledger.assert_all_joined();
        drop(conn);
    }

    assert_eq!(ledger.spawned_total(), 25 * 3);
}
