//! Production session transport over `airplay_client::AirPlayClient`.
//!
//! Closes the transport half of the stub gap Task 15 left open; it is NOT
//! the receiver health/retry/rejoin state machine of plan Task 16, which is
//! still untouched.
//!
//! This module is the only place where the device backend touches the AirPlay
//! protocol stack. It translates one best-effort group connect into the
//! [`SessionTransport`]/[`SessionStartFailure`] contract the session
//! supervisor consumes:
//!
//! * partial success IS success — whichever members went live keep the
//!   session, and every member that did not is reported with its setup phase
//!   instead of being swallowed;
//! * no primary means no session — the report is turned into a start failure
//!   and every member established along the way is torn down;
//! * nothing user-facing carries an address, a socket, or a device object.
//!   Protocol detail travels through `tracing` only.
//!
//! The whole protocol surface is reached through the [`GroupClient`] seam so
//! the translation above is verifiable without receivers or a network; only
//! the thin [`AirPlayGroupClient`] passthrough needs real hardware.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use airplay_audio::LiveAudioDecoder;
use airplay_client::{
    AirPlayClient, ClientDiagnosticsSource, GroupConnectReport, GroupRuntimeEvent,
    GroupRuntimeEventFeed, Health, MemberFailure, MemberResult,
};
use airplay_core::{Device, DeviceId, SenderBufferCapacity};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::backend::model::{LatencyConfig, ReceiverId, UserFacingError};
use crate::backend::session::{
    EffectiveVolumeState, RuntimeEventLag, SessionStartFailure, SessionTransport,
    SessionTransportFactory, TEARDOWN_GROUP,
};
use crate::calibration::{effective_delays_by_device, normalize_calibration, CalibrationProfile};

/// Pre-redacted message used whenever no member could be brought up.
const NO_MEMBER_REACHABLE: &str = "no selected receiver could be reached";

/// Pre-redacted message used when the group came up but audio could not start.
const AUDIO_START_FAILED: &str = "the receivers accepted the session but audio could not start";

/// Pre-redacted message used when a connected receiver cannot accept its
/// required level before audio starts.
const INITIAL_VOLUME_FAILED: &str =
    "the receivers accepted the session but safe volume setup failed";

/// A continuously changing slider must not let setup loop forever. Three
/// complete passes give a coalesced latest value a bounded chance to settle;
/// otherwise the attempt fails safely before RTP begins.
const INITIAL_VOLUME_MAX_PASSES: usize = 3;

// ---------------------------------------------------------------------------
// Protocol seam
// ---------------------------------------------------------------------------

/// Narrow view of the group-capable AirPlay client used by this adapter.
///
/// Production is [`AirPlayGroupClient`], a pure passthrough onto
/// [`AirPlayClient`]; tests script it to exercise every translation branch
/// offline.
#[doc(hidden)]
#[async_trait]
pub(crate) trait GroupClient: Send {
    /// Sets the local sender buffer capacity; must be applied before connecting.
    fn set_sender_buffer_capacity(&mut self, capacity: SenderBufferCapacity);

    /// Sets the render lead; must be applied before connecting.
    fn set_render_delay_ms(&mut self, delay_ms: u32);

    /// Takes the consumer half of the bounded runtime-event feed.
    fn take_runtime_events(&mut self) -> Option<mpsc::Receiver<GroupRuntimeEvent>>;

    /// Events the bounded feed truthfully dropped so far (monotonic).
    fn runtime_events_lost(&self) -> u64;

    /// Read-only diagnostics backed by this client's live connections/streamer.
    fn diagnostics_source(&self) -> ClientDiagnosticsSource;

    /// Best-effort group connect with per-member failure isolation.
    async fn connect_group(
        &mut self,
        devices: &[Device],
        preferred_primary: Option<&DeviceId>,
    ) -> airplay_core::Result<GroupConnectReport>;

    /// Prepares two or more members sharing one PTP domain; audio awaits release.
    ///
    /// Requires the primary's PTP master clock identity, which only the group
    /// timing handshake establishes.
    async fn start_group_live_stream(
        &mut self,
        decoder: LiveAudioDecoder,
        presentation_delays_ns: &BTreeMap<DeviceId, u64>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> airplay_core::Result<()>;

    /// Prepares exactly one live member; audio awaits release.
    ///
    /// That session runs on NTP timing and therefore has no PTP clock
    /// identity; the group entry point would reject it.
    async fn start_single_live_stream(
        &mut self,
        decoder: LiveAudioDecoder,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> airplay_core::Result<()>;

    /// Probes every live member independently.
    async fn probe_members(&mut self) -> Vec<MemberResult<Health>>;

    /// Sets exactly one member's volume.
    async fn set_member_volume(&mut self, receiver: &DeviceId, volume: f32) -> MemberResult<()>;

    /// Bounded best-effort teardown of every connection.
    async fn disconnect(&mut self) -> airplay_core::Result<()>;
}

/// Creates one fresh protocol client per session generation.
#[doc(hidden)]
pub(crate) trait GroupClientFactory: Send + Sync {
    /// Builds a client, or a redacted reason why the stack is unavailable.
    fn create(&self) -> Result<Box<dyn GroupClient>, UserFacingError>;
}

/// Production passthrough onto [`AirPlayClient`].
///
/// Every method here forwards verbatim; all decision-making lives in
/// [`AirPlaySessionTransportFactory`] and [`AirPlayGroupTransport`].
struct AirPlayGroupClient {
    client: AirPlayClient,
    /// Producer-side handle retained solely for its loss counter.
    feed: GroupRuntimeEventFeed,
}

#[async_trait]
impl GroupClient for AirPlayGroupClient {
    fn set_sender_buffer_capacity(&mut self, capacity: SenderBufferCapacity) {
        self.client.set_sender_buffer_capacity(capacity);
    }

    fn set_render_delay_ms(&mut self, delay_ms: u32) {
        self.client.set_render_delay_ms(delay_ms);
    }

    fn take_runtime_events(&mut self) -> Option<mpsc::Receiver<GroupRuntimeEvent>> {
        self.client.take_runtime_event_receiver()
    }

    fn runtime_events_lost(&self) -> u64 {
        self.feed.lost_events()
    }

    fn diagnostics_source(&self) -> ClientDiagnosticsSource {
        self.client.diagnostics_source()
    }

    async fn connect_group(
        &mut self,
        devices: &[Device],
        preferred_primary: Option<&DeviceId>,
    ) -> airplay_core::Result<GroupConnectReport> {
        self.client
            .connect_group_best_effort(devices, preferred_primary)
            .await
    }

    async fn start_group_live_stream(
        &mut self,
        decoder: LiveAudioDecoder,
        presentation_delays_ns: &BTreeMap<DeviceId, u64>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> airplay_core::Result<()> {
        self.client
            .prepare_live_group_with_presentation_delays(decoder, presentation_delays_ns, release)
            .await
    }

    async fn start_single_live_stream(
        &mut self,
        decoder: LiveAudioDecoder,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> airplay_core::Result<()> {
        self.client
            .start_live_streaming_with_decoder_gated(decoder, Some(release))
            .await
    }

    async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
        self.client.probe_members().await
    }

    async fn set_member_volume(&mut self, receiver: &DeviceId, volume: f32) -> MemberResult<()> {
        self.client.set_member_volume(receiver, volume).await
    }

    async fn disconnect(&mut self) -> airplay_core::Result<()> {
        self.client.disconnect().await
    }
}

/// Normalizes `calibration` for exactly the receivers that connected and
/// re-keys the result onto the client's own identity space.
///
/// The reference receiver is an authoring anchor, not part of the arithmetic:
/// if it dropped out of the group, the surviving speakers' pairwise offsets
/// are still exactly what the user tuned, so it is dropped here instead of
/// failing the whole session. Rejecting the anchor belongs at apply time,
/// where the user is choosing it; a receiver going missing at setup is not
/// something the user did.
fn presentation_delays_for(
    connected: &[DeviceId],
    calibration: &CalibrationProfile,
) -> Result<BTreeMap<DeviceId, u64>, crate::calibration::CalibrationError> {
    let active: BTreeSet<ReceiverId> = connected.iter().cloned().map(ReceiverId::from).collect();
    let anchored = CalibrationProfile {
        reference_receiver: calibration
            .reference_receiver
            .filter(|reference| active.contains(reference)),
        requested_relative_delay_ns: calibration.requested_relative_delay_ns.clone(),
    };
    Ok(effective_delays_by_device(&normalize_calibration(
        &active, &anchored,
    )?))
}

/// Production factory: one `AirPlayClient` per session generation.
struct RealGroupClientFactory;

impl GroupClientFactory for RealGroupClientFactory {
    fn create(&self) -> Result<Box<dyn GroupClient>, UserFacingError> {
        match AirPlayClient::new() {
            Ok(client) => {
                let feed = client.runtime_event_feed();
                Ok(Box::new(AirPlayGroupClient { client, feed }))
            }
            Err(error) => {
                tracing::warn!("AirPlay client could not be created: {error}");
                Err(UserFacingError::new(
                    "the AirPlay stack could not be started on this machine",
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Establishes one AirPlay session generation over the real protocol stack.
pub struct AirPlaySessionTransportFactory {
    clients: Arc<dyn GroupClientFactory>,
}

impl Default for AirPlaySessionTransportFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl AirPlaySessionTransportFactory {
    /// Production wiring over `airplay_client::AirPlayClient`.
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RealGroupClientFactory),
        }
    }

    /// Scripted wiring used by this crate's offline tests.
    #[cfg(test)]
    pub(crate) fn with_clients(clients: Arc<dyn GroupClientFactory>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl SessionTransportFactory for AirPlaySessionTransportFactory {
    async fn start(
        &self,
        desired: Vec<Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        latency: LatencyConfig,
        calibration: CalibrationProfile,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
        self.start_with_volumes(
            desired,
            preferred_primary,
            decoder,
            latency,
            calibration,
            EffectiveVolumeState::default(),
        )
        .await
    }

    async fn start_with_volumes(
        &self,
        desired: Vec<Device>,
        preferred_primary: Option<ReceiverId>,
        decoder: LiveAudioDecoder,
        latency: LatencyConfig,
        calibration: CalibrationProfile,
        volumes: EffectiveVolumeState,
    ) -> Result<Box<dyn SessionTransport>, SessionStartFailure> {
        let Some(sender_buffer) = SenderBufferCapacity::from_millis(latency.sender_buffer_ms)
        else {
            return Err(SessionStartFailure {
                error: UserFacingError::new("the selected sender buffer capacity is invalid"),
                failures: Vec::new(),
            });
        };

        let mut client = self.clients.create().map_err(|error| SessionStartFailure {
            error,
            failures: Vec::new(),
        })?;

        // Both local settings are read while connecting. The buffer remains a
        // local resource setting and is deliberately not guessed onto the
        // RTSP SETUP latency window; see the module tests.
        client.set_sender_buffer_capacity(sender_buffer);
        client.set_render_delay_ms(latency.render_delay_ms);
        let events = client.take_runtime_events();
        let lost_baseline = client.runtime_events_lost();

        let preferred: Option<DeviceId> = preferred_primary.map(DeviceId::from);
        let report = match client.connect_group(&desired, preferred.as_ref()).await {
            Ok(report) => report,
            Err(error) => {
                // The protocol error may name addresses; it goes to the log,
                // never to the caller.
                tracing::warn!("best-effort group connect failed: {error}");
                let _ = client.disconnect().await;
                return Err(SessionStartFailure {
                    error: UserFacingError::new(NO_MEMBER_REACHABLE),
                    failures: Vec::new(),
                });
            }
        };

        // No primary means no timing domain and therefore no session. The
        // client already tore its members down; disconnect again so a partial
        // topology can never outlive the failed attempt.
        let Some(primary) = report.primary.clone() else {
            let _ = client.disconnect().await;
            return Err(SessionStartFailure {
                error: UserFacingError::new(NO_MEMBER_REACHABLE),
                failures: report.failures,
            });
        };

        // The same current-value source serves slider changes that arrive
        // while pairing or connecting. This early check fails unsafe sessions
        // promptly. Audio preparation stays gated; activate() revalidates the
        // latest levels after all preparation and at supervisor adoption.
        let mut volumes_stable = false;
        for _ in 0..INITIAL_VOLUME_MAX_PASSES {
            let snapshot = volumes.snapshot();
            for receiver in &report.connected {
                let volume = snapshot.get(ReceiverId::from(receiver.clone()));
                let set = tokio::time::timeout(
                    TEARDOWN_GROUP,
                    client.set_member_volume(receiver, volume),
                )
                .await;
                let accepted = matches!(set, Ok(MemberResult { result: Ok(()), .. }));
                if !accepted {
                    tracing::warn!(
                        "initial receiver volume setup failed or timed out; refusing audio start"
                    );
                    let _ = tokio::time::timeout(TEARDOWN_GROUP, client.disconnect()).await;
                    return Err(SessionStartFailure {
                        error: UserFacingError::new(INITIAL_VOLUME_FAILED),
                        failures: report.failures,
                    });
                }
            }
            if volumes.is_current(&snapshot) {
                volumes_stable = true;
                break;
            }
        }
        if !volumes_stable {
            tracing::warn!("initial receiver volume setup did not stabilize; refusing audio start");
            let _ = tokio::time::timeout(TEARDOWN_GROUP, client.disconnect()).await;
            return Err(SessionStartFailure {
                error: UserFacingError::new(INITIAL_VOLUME_FAILED),
                failures: report.failures,
            });
        }

        // Timing domain decides the streaming entry point. A group shares the
        // primary's PTP clock; a session that ended up with a single live
        // member runs on NTP and has no PTP clock identity at all, so the
        // group entry point would reject it and no receiver would ever get
        // audio. `connected` is the authoritative membership: the
        // single-receiver fallback reports exactly one, every surviving group
        // reports at least two.
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let audio_started = if report.connected.len() <= 1 {
            // One receiver has nothing to be aligned against, and the NTP
            // path carries no per-target presentation clock at all.
            client.start_single_live_stream(decoder, release_rx).await
        } else {
            // Normalize against the receivers that ACTUALLY connected, not
            // against the desired set: a member that failed setup must not
            // shift the survivors, and the client adapter rejects a delay map
            // that does not match its connected targets one-for-one rather
            // than defaulting a missing entry to zero.
            let delays = match presentation_delays_for(&report.connected, &calibration) {
                Ok(delays) => delays,
                Err(error) => {
                    // Redacted by construction: the domain error names no
                    // receiver, address, or value.
                    tracing::warn!("calibration could not be normalized: {error}");
                    let _ = client.disconnect().await;
                    return Err(SessionStartFailure {
                        error: UserFacingError::new(AUDIO_START_FAILED),
                        failures: report.failures,
                    });
                }
            };
            client
                .start_group_live_stream(decoder, &delays, release_rx)
                .await
        };
        if let Err(error) = audio_started {
            tracing::warn!("live streaming failed to start: {error}");
            let _ = client.disconnect().await;
            return Err(SessionStartFailure {
                error: UserFacingError::new(AUDIO_START_FAILED),
                failures: report.failures,
            });
        }

        Ok(Box::new(AirPlayGroupTransport {
            release: Some(release_tx),
            volumes,
            client,
            primary: ReceiverId::from(primary),
            members: report.connected.into_iter().map(ReceiverId::from).collect(),
            failures: Some(report.failures),
            events,
            lost_seen: lost_baseline,
        }))
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// One live AirPlay group session.
struct AirPlayGroupTransport {
    release: Option<tokio::sync::oneshot::Sender<()>>,
    volumes: EffectiveVolumeState,
    client: Box<dyn GroupClient>,
    primary: ReceiverId,
    members: BTreeSet<ReceiverId>,
    /// Setup failures of the establishing connect; handed out exactly once
    /// because `MemberFailure` carries a non-cloneable protocol error.
    failures: Option<Vec<MemberFailure>>,
    events: Option<mpsc::Receiver<GroupRuntimeEvent>>,
    /// Loss count already reported through [`RuntimeEventLag`].
    lost_seen: u64,
}

#[async_trait]
impl SessionTransport for AirPlayGroupTransport {
    async fn activate(&mut self) -> Result<(), UserFacingError> {
        if self.release.is_none() {
            return Ok(());
        }
        for _ in 0..INITIAL_VOLUME_MAX_PASSES {
            let snapshot = self.volumes.snapshot();
            for receiver in &self.members {
                let result = tokio::time::timeout(
                    TEARDOWN_GROUP,
                    self.client
                        .set_member_volume(&DeviceId::from(*receiver), snapshot.get(*receiver)),
                )
                .await;
                if !matches!(result, Ok(MemberResult { result: Ok(()), .. })) {
                    return Err(UserFacingError::new(INITIAL_VOLUME_FAILED));
                }
            }
            // No await after successful release: the supervisor adopts this
            // transport in the same poll, before it consumes queued controls.
            if self
                .volumes
                .release_if_current(&snapshot, &mut self.release)
            {
                return Ok(());
            }
        }
        Err(UserFacingError::new(INITIAL_VOLUME_FAILED))
    }
    fn diagnostics_source(&self) -> Option<ClientDiagnosticsSource> {
        Some(self.client.diagnostics_source())
    }
    fn primary(&self) -> Option<ReceiverId> {
        Some(self.primary)
    }

    fn active_members(&self) -> BTreeSet<ReceiverId> {
        self.members.clone()
    }

    fn setup_failures(&mut self) -> Vec<MemberFailure> {
        self.failures.take().unwrap_or_default()
    }

    fn try_runtime_event(&mut self) -> Result<Option<GroupRuntimeEvent>, RuntimeEventLag> {
        // Delivery first, gap report second. The feed only ever drops events
        // that arrive at a FULL queue, so everything still queued is older
        // than everything it lost: handing the queued events out first is the
        // chronologically truthful order.
        //
        // It is also the only order that cannot starve. Reporting the gap
        // first returns without consuming anything, so a receiver that keeps
        // failing — and therefore keeps growing the loss count between two
        // drains — would make every single drain end in a lag report while
        // the queue stays full forever. The supervisor would then see nothing
        // but lag warnings for the entire duration of the failure.
        if let Some(events) = self.events.as_mut() {
            if let Ok(event) = events.try_recv() {
                return Ok(Some(event));
            }
        }
        // Nothing left to hand out: now report the accumulated gap, each
        // dropped event exactly once.
        let lost = self.client.runtime_events_lost();
        if lost > self.lost_seen {
            let dropped = lost - self.lost_seen;
            self.lost_seen = lost;
            return Err(RuntimeEventLag { dropped });
        }
        Ok(None)
    }

    async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
        self.client.probe_members().await
    }

    async fn set_member_volume(&mut self, receiver: &ReceiverId, volume: f32) -> MemberResult<()> {
        let target = DeviceId::from(*receiver);
        self.client.set_member_volume(&target, volume).await
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.client
            .disconnect()
            .await
            .map_err(|error| anyhow::anyhow!("group teardown failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use airplay_client::SetupPhase;
    use airplay_core::device::Version;
    use airplay_core::error::{PairingError, RtspError};
    use airplay_core::features::Features;
    use tokio::sync::oneshot;

    use crate::backend::capture::{CAPTURE_CHANNELS, CAPTURE_PCM_CAPACITY, CAPTURE_SAMPLE_RATE};
    use crate::backend::model::Volume;
    use crate::backend::persistence::effective_volume;

    // -- fixtures ----------------------------------------------------------

    fn device(seed: u8) -> Device {
        Device {
            id: DeviceId([0, 0, 0, 0, 0, seed]),
            name: format!("Receiver {seed}"),
            model: "TestModel".to_owned(),
            manufacturer: None,
            serial_number: None,
            addresses: vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                192, 168, 1, seed,
            ))],
            port: 7000,
            features: Features(1 << 40),
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

    fn receiver(seed: u8) -> ReceiverId {
        ReceiverId::from(DeviceId([0, 0, 0, 0, 0, seed]))
    }

    fn failure(seed: u8, phase: SetupPhase) -> MemberFailure {
        MemberFailure {
            receiver: DeviceId([0, 0, 0, 0, 0, seed]),
            phase,
            retryable: matches!(phase, SetupPhase::Connect),
            source: airplay_core::Error::Rtsp(RtspError::NoSession),
        }
    }

    fn decoder() -> LiveAudioDecoder {
        let (_sender, decoder) = LiveAudioDecoder::create_pair(
            CAPTURE_SAMPLE_RATE,
            CAPTURE_CHANNELS,
            CAPTURE_PCM_CAPACITY,
        );
        decoder
    }

    fn latency() -> LatencyConfig {
        LatencyConfig {
            sender_buffer_ms: 2_000,
            render_delay_ms: 275,
        }
    }

    // -- scripted client ---------------------------------------------------

    #[derive(Debug, Default)]
    struct Journal {
        dispatches: usize,
        reject_activation: bool,
        activation_started: Option<oneshot::Sender<()>>,
        activation_resume: Option<oneshot::Receiver<()>>,
        render_delay: Option<u32>,
        sender_buffer: Option<u32>,
        connected: Option<(Vec<DeviceId>, Option<DeviceId>)>,
        live_started: bool,
        events: Vec<JournalEvent>,
        /// Which streaming entry point the adapter chose, if any.
        live_path: Option<LivePath>,
        /// Per-target presentation delays handed to the group entry point.
        presentation_delays: Option<BTreeMap<DeviceId, u64>>,
        disconnects: u32,
        volumes: Vec<(DeviceId, f32)>,
        probes: u32,
    }

    /// Streaming entry point recorded by the scripted client.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LivePath {
        /// Single receiver, NTP timing, no PTP clock identity available.
        Single,
        /// Two or more receivers sharing the primary's PTP domain.
        Group,
    }

    /// Observable setup actions emitted by the scripted protocol client.
    #[derive(Clone, Debug, PartialEq)]
    enum JournalEvent {
        /// A connected receiver received its initial volume.
        Volume(DeviceId, f32),
        /// The client preparation entry point (actual dispatch awaits its gate).
        Live(LivePath),
    }

    #[derive(Default)]
    struct Script {
        connect: Option<airplay_core::Result<GroupConnectReport>>,
        connect_started: Option<oneshot::Sender<()>>,
        connect_continue: Option<oneshot::Receiver<()>>,
        prepare_started: Option<oneshot::Sender<()>>,
        prepare_continue: Option<oneshot::Receiver<()>>,
        volume_started: Option<oneshot::Sender<()>>,
        volume_continue: Option<oneshot::Receiver<()>>,
        live: Option<airplay_core::Result<()>>,
        initial_volume_fails: bool,
        probe: Vec<MemberResult<Health>>,
        create_error: Option<UserFacingError>,
    }

    struct ScriptedClients {
        script: Mutex<Script>,
        journal: Arc<Mutex<Journal>>,
        feed: GroupRuntimeEventFeed,
        events: Mutex<Option<mpsc::Receiver<GroupRuntimeEvent>>>,
        lost: Arc<AtomicU64>,
    }

    impl ScriptedClients {
        fn new(script: Script) -> Arc<Self> {
            let (feed, events) = GroupRuntimeEventFeed::channel(4);
            Arc::new(Self {
                script: Mutex::new(script),
                journal: Arc::new(Mutex::new(Journal::default())),
                feed,
                events: Mutex::new(Some(events)),
                lost: Arc::new(AtomicU64::new(0)),
            })
        }

        fn journal(&self) -> Arc<Mutex<Journal>> {
            Arc::clone(&self.journal)
        }
    }

    impl GroupClientFactory for ScriptedClients {
        fn create(&self) -> Result<Box<dyn GroupClient>, UserFacingError> {
            let mut script = self.script.lock().unwrap();
            if let Some(error) = script.create_error.take() {
                return Err(error);
            }
            Ok(Box::new(ScriptedClient {
                connect: script.connect.take(),
                connect_started: script.connect_started.take(),
                connect_continue: script.connect_continue.take(),
                prepare_started: script.prepare_started.take(),
                prepare_continue: script.prepare_continue.take(),
                volume_started: script.volume_started.take(),
                volume_continue: script.volume_continue.take(),
                live: script.live.take(),
                initial_volume_fails: script.initial_volume_fails,
                probe: std::mem::take(&mut script.probe),
                journal: Arc::clone(&self.journal),
                events: self.events.lock().unwrap().take(),
                lost: Arc::clone(&self.lost),
            }))
        }
    }

    struct ScriptedClient {
        connect: Option<airplay_core::Result<GroupConnectReport>>,
        connect_started: Option<oneshot::Sender<()>>,
        connect_continue: Option<oneshot::Receiver<()>>,
        prepare_started: Option<oneshot::Sender<()>>,
        prepare_continue: Option<oneshot::Receiver<()>>,
        volume_started: Option<oneshot::Sender<()>>,
        volume_continue: Option<oneshot::Receiver<()>>,
        live: Option<airplay_core::Result<()>>,
        initial_volume_fails: bool,
        probe: Vec<MemberResult<Health>>,
        journal: Arc<Mutex<Journal>>,
        events: Option<mpsc::Receiver<GroupRuntimeEvent>>,
        lost: Arc<AtomicU64>,
    }

    #[async_trait]
    impl GroupClient for ScriptedClient {
        fn set_sender_buffer_capacity(&mut self, capacity: SenderBufferCapacity) {
            self.journal.lock().unwrap().sender_buffer = Some(capacity.millis());
        }

        fn set_render_delay_ms(&mut self, delay_ms: u32) {
            self.journal.lock().unwrap().render_delay = Some(delay_ms);
        }

        fn take_runtime_events(&mut self) -> Option<mpsc::Receiver<GroupRuntimeEvent>> {
            self.events.take()
        }

        fn runtime_events_lost(&self) -> u64 {
            self.lost.load(Ordering::SeqCst)
        }

        fn diagnostics_source(&self) -> ClientDiagnosticsSource {
            ClientDiagnosticsSource::test_new_empty()
        }

        async fn connect_group(
            &mut self,
            devices: &[Device],
            preferred_primary: Option<&DeviceId>,
        ) -> airplay_core::Result<GroupConnectReport> {
            self.journal.lock().unwrap().connected = Some((
                devices.iter().map(|device| device.id.clone()).collect(),
                preferred_primary.cloned(),
            ));
            if let Some(started) = self.connect_started.take() {
                let _ = started.send(());
            }
            if let Some(continue_connect) = self.connect_continue.take() {
                let _ = continue_connect.await;
            }
            self.connect
                .take()
                .expect("connect scripted exactly once per client")
        }

        async fn start_group_live_stream(
            &mut self,
            _decoder: LiveAudioDecoder,
            presentation_delays_ns: &BTreeMap<DeviceId, u64>,
            release: oneshot::Receiver<()>,
        ) -> airplay_core::Result<()> {
            if let Some(started) = self.prepare_started.take() {
                let _ = started.send(());
            }
            if let Some(resume) = self.prepare_continue.take() {
                let _ = resume.await;
            }
            {
                let mut journal = self.journal.lock().unwrap();
                journal.live_started = true;
                journal.live_path = Some(LivePath::Group);
                journal.events.push(JournalEvent::Live(LivePath::Group));
                journal.presentation_delays = Some(presentation_delays_ns.clone());
            }
            let journal = self.journal.clone();
            tokio::spawn(async move {
                if release.await.is_ok() {
                    journal.lock().unwrap().dispatches += 1;
                }
            });
            self.live.take().unwrap_or(Ok(()))
        }

        async fn start_single_live_stream(
            &mut self,
            _decoder: LiveAudioDecoder,
            release: oneshot::Receiver<()>,
        ) -> airplay_core::Result<()> {
            if let Some(started) = self.prepare_started.take() {
                let _ = started.send(());
            }
            if let Some(resume) = self.prepare_continue.take() {
                let _ = resume.await;
            }
            {
                let mut journal = self.journal.lock().unwrap();
                journal.live_started = true;
                journal.live_path = Some(LivePath::Single);
                journal.events.push(JournalEvent::Live(LivePath::Single));
            }
            let journal = self.journal.clone();
            tokio::spawn(async move {
                if release.await.is_ok() {
                    journal.lock().unwrap().dispatches += 1;
                }
            });
            self.live.take().unwrap_or(Ok(()))
        }

        async fn probe_members(&mut self) -> Vec<MemberResult<Health>> {
            self.journal.lock().unwrap().probes += 1;
            std::mem::take(&mut self.probe)
        }

        async fn set_member_volume(
            &mut self,
            receiver: &DeviceId,
            volume: f32,
        ) -> MemberResult<()> {
            {
                let mut journal = self.journal.lock().unwrap();
                journal.volumes.push((receiver.clone(), volume));
                journal
                    .events
                    .push(JournalEvent::Volume(receiver.clone(), volume));
            }
            if let Some(started) = self.volume_started.take() {
                let _ = started.send(());
            }
            if let Some(continue_volume) = self.volume_continue.take() {
                let _ = continue_volume.await;
            }
            let (started, resume) = {
                let mut journal = self.journal.lock().unwrap();
                if journal.live_started {
                    (
                        journal.activation_started.take(),
                        journal.activation_resume.take(),
                    )
                } else {
                    (None, None)
                }
            };
            if let Some(started) = started {
                let _ = started.send(());
            }
            if let Some(resume) = resume {
                let _ = resume.await;
            }
            let result =
                if self.initial_volume_fails || self.journal.lock().unwrap().reject_activation {
                    Err(MemberFailure {
                        receiver: receiver.clone(),
                        phase: SetupPhase::RtspSetup,
                        retryable: false,
                        source: airplay_core::Error::Rtsp(RtspError::NoSession),
                    })
                } else {
                    Ok(())
                };
            MemberResult {
                receiver: receiver.clone(),
                result,
            }
        }

        async fn disconnect(&mut self) -> airplay_core::Result<()> {
            self.journal.lock().unwrap().disconnects += 1;
            Ok(())
        }
    }

    /// Drops one event on the floor, exactly as a full bounded feed would.
    fn drop_one(client: &ScriptedClients) {
        client.lost.fetch_add(1, Ordering::SeqCst);
    }

    // -- tests -------------------------------------------------------------

    mod start {
        use super::*;

        #[tokio::test]
        async fn stop_or_reconcile_during_activation_cancels_without_releasing_audio() {
            use crate::backend::session::{
                SessionDecoderSource, SessionRequest, SessionSupervisor, SessionUpdate,
            };
            struct Decoders;
            impl SessionDecoderSource for Decoders {
                fn take_decoder(&self) -> LiveAudioDecoder {
                    decoder()
                }
            }
            for teardown_only in [true, false] {
                let clients = ScriptedClients::new(Script {
                    connect: Some(Ok(GroupConnectReport {
                        primary: Some(device(1).id),
                        connected: vec![device(1).id],
                        failures: vec![],
                    })),
                    ..Script::default()
                });
                let journal = clients.journal();
                let (started, started_rx) = oneshot::channel();
                let (resume, resume_rx) = oneshot::channel();
                {
                    let mut journal = journal.lock().unwrap();
                    journal.activation_started = Some(started);
                    journal.activation_resume = Some(resume_rx);
                }
                let mut supervisor = SessionSupervisor::start(
                    Arc::new(AirPlaySessionTransportFactory::with_clients(clients)),
                    Arc::new(Decoders),
                );
                let handle = supervisor.handle();
                handle
                    .try_send(SessionRequest::Reconcile {
                        generation: 1,
                        desired: vec![device(1)],
                        preferred_primary: None,
                        latency: latency(),
                        calibration: CalibrationProfile::default(),
                        teardown_only: false,
                    })
                    .unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
                    .await
                    .unwrap()
                    .unwrap();
                handle
                    .try_send(SessionRequest::Reconcile {
                        generation: 2,
                        desired: vec![],
                        preferred_primary: None,
                        latency: latency(),
                        calibration: CalibrationProfile::default(),
                        teardown_only,
                    })
                    .unwrap();
                // Make activation and the already-admitted cancellation ready
                // together: the new generation must win before release.
                let _ = resume.send(());
                let mut unsafe_active = false;
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while let Some(update) = supervisor.updates_mut().recv().await {
                        if matches!(update, SessionUpdate::Active { generation: 1, .. }) {
                            unsafe_active = true;
                        }
                        if matches!(
                            update,
                            SessionUpdate::Stopped { generation: 2 }
                                | SessionUpdate::Recovering { generation: 2, .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                tokio::task::yield_now().await;
                supervisor.shutdown().await;
                assert!(!unsafe_active, "Stop/Reconcile was admitted before release");
                assert_eq!(journal.lock().unwrap().dispatches, 0);
                assert!(journal.lock().unwrap().disconnects > 0);
            }
        }

        #[tokio::test]
        async fn preparation_changes_are_safe_before_first_audio_for_single_and_group() {
            for count in [1, 2] {
                let (started_tx, started_rx) = oneshot::channel();
                let (resume_tx, resume_rx) = oneshot::channel();
                let clients = ScriptedClients::new(Script {
                    connect: Some(Ok(GroupConnectReport {
                        primary: Some(device(1).id),
                        connected: (1..=count).map(|n| device(n).id).collect(),
                        failures: vec![],
                    })),
                    prepare_started: Some(started_tx),
                    prepare_continue: Some(resume_rx),
                    ..Script::default()
                });
                let journal = clients.journal();
                let factory = AirPlaySessionTransportFactory::with_clients(clients);
                let volumes = EffectiveVolumeState::default();
                volumes.set(receiver(1), 0.8);
                let start = factory.start_with_volumes(
                    (1..=count).map(device).collect(),
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                    volumes.clone(),
                );
                tokio::pin!(start);
                tokio::select! { _ = &mut start => panic!("preparation was not held"), _ = started_rx => {} }
                volumes.set(receiver(1), 0.0);
                if count == 2 {
                    volumes.set(receiver(2), 0.3);
                }
                resume_tx.send(()).unwrap();
                let mut transport = start.await.expect("prepared");
                assert_eq!(journal.lock().unwrap().dispatches, 0);
                // A further edit during the preparation/adoption handoff must
                // also be covered, not consumed by an inactive supervisor.
                if count == 2 {
                    volumes.set(receiver(2), 0.45);
                }
                transport.activate().await.expect("safe release");
                tokio::task::yield_now().await;
                let journal = journal.lock().unwrap();
                assert_eq!(journal.dispatches, 1);
                assert_eq!(
                    journal
                        .volumes
                        .iter()
                        .rev()
                        .find(|(id, _)| id == &device(1).id)
                        .unwrap()
                        .1,
                    0.0,
                    "mute during real preparation must precede first audio"
                );
                if count == 2 {
                    assert_eq!(
                        journal
                            .volumes
                            .iter()
                            .rev()
                            .find(|(id, _)| id == &device(2).id)
                            .unwrap()
                            .1,
                        0.45
                    );
                }
            }
        }

        #[tokio::test]
        async fn activation_revalidates_awaited_volume_and_failure_releases_no_audio() {
            for reject in [false, true] {
                for count in [1, 2] {
                    let clients = ScriptedClients::new(Script {
                        connect: Some(Ok(GroupConnectReport {
                            primary: Some(device(1).id),
                            connected: (1..=count).map(|n| device(n).id).collect(),
                            failures: vec![],
                        })),
                        ..Script::default()
                    });
                    let journal = clients.journal();
                    let factory = AirPlaySessionTransportFactory::with_clients(clients);
                    let volumes = EffectiveVolumeState::default();
                    volumes.set(receiver(1), 0.8);
                    let mut transport = factory
                        .start_with_volumes(
                            (1..=count).map(device).collect(),
                            None,
                            decoder(),
                            latency(),
                            CalibrationProfile::default(),
                            volumes.clone(),
                        )
                        .await
                        .unwrap();
                    let (started, started_rx) = oneshot::channel();
                    let (resume, resume_rx) = oneshot::channel();
                    {
                        let mut journal = journal.lock().unwrap();
                        journal.activation_started = Some(started);
                        journal.activation_resume = Some(resume_rx);
                        journal.reject_activation = reject;
                    }
                    let activate = transport.activate();
                    tokio::pin!(activate);
                    tokio::select! { _ = &mut activate => panic!("volume was not held"), _ = started_rx => {} }
                    volumes.set(receiver(1), 0.0);
                    assert_eq!(journal.lock().unwrap().dispatches, 0);
                    resume.send(()).unwrap();
                    assert_eq!(activate.await.is_err(), reject);
                    tokio::task::yield_now().await;
                    let journal = journal.lock().unwrap();
                    assert_eq!(journal.dispatches, usize::from(!reject));
                    if !reject {
                        assert_eq!(
                            journal
                                .volumes
                                .iter()
                                .rev()
                                .find(|(id, _)| id == &device(1).id)
                                .unwrap()
                                .1,
                            0.0
                        );
                    }
                }
            }
        }

        #[tokio::test]
        async fn connected_members_receive_initial_volumes_before_group_audio_starts() {
            // Regression caught: removing the initial-volume barrier lets the
            // RTP sender start before receivers have their effective level.
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1]), DeviceId([0, 0, 0, 0, 0, 2])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("a connected group starts after setup");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 1.0),
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 2]), 1.0),
                    JournalEvent::Live(LivePath::Group),
                ],
                "each connected receiver must be safe before group audio begins"
            );
        }

        #[tokio::test]
        async fn effective_master_level_and_mute_values_precede_single_audio() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);
            let volumes = EffectiveVolumeState::default();
            volumes.set(
                receiver(1),
                effective_volume(
                    Volume::new(0.5).expect("valid master"),
                    Volume::new(0.4).expect("valid receiver level"),
                    false,
                )
                .get(),
            );

            factory
                .start_with_volumes(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                    volumes,
                )
                .await
                .expect("the effective level is applied before single audio");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.2),
                    JournalEvent::Live(LivePath::Single),
                ]
            );
        }

        #[tokio::test]
        async fn muted_effective_value_precedes_single_audio() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);
            let volumes = EffectiveVolumeState::default();
            volumes.set(
                receiver(1),
                effective_volume(Volume::UNITY, Volume::UNITY, true).get(),
            );

            factory
                .start_with_volumes(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                    volumes,
                )
                .await
                .expect("muted receivers still get a safe initial level");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.0),
                    JournalEvent::Live(LivePath::Single),
                ]
            );
        }

        #[tokio::test]
        async fn partial_connect_applies_levels_only_to_survivors_before_audio() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: vec![failure(2, SetupPhase::Connect)],
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("the surviving receiver can stream");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 1.0),
                    JournalEvent::Live(LivePath::Single),
                ]
            );
        }

        #[tokio::test]
        async fn failed_initial_volume_disconnects_without_starting_audio() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                initial_volume_fails: true,
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let result = factory
                .start(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await;
            let Err(error) = result else {
                panic!("unsafe initial volume refuses audio");
            };

            assert_eq!(error.error.as_str(), INITIAL_VOLUME_FAILED);
            let journal = journal.lock().unwrap();
            assert!(
                !journal.live_started,
                "failed initial setup must not leak audio"
            );
            assert_eq!(journal.disconnects, 1, "the failed setup is cleaned up");
        }

        #[tokio::test]
        async fn level_changed_while_connecting_is_used_by_the_setup_barrier() {
            let (connected_tx, connected_rx) = oneshot::channel();
            let (continue_tx, continue_rx) = oneshot::channel();
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                connect_started: Some(connected_tx),
                connect_continue: Some(continue_rx),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);
            let volumes = EffectiveVolumeState::default();
            volumes.set(receiver(1), 0.2);

            let start = factory.start_with_volumes(
                vec![device(1)],
                None,
                decoder(),
                latency(),
                CalibrationProfile::default(),
                volumes.clone(),
            );
            tokio::pin!(start);
            tokio::select! {
                _ = &mut start => panic!("connect gate released too early"),
                _ = connected_rx => {}
            }
            volumes.set(receiver(1), 0.6);
            continue_tx
                .send(())
                .expect("connect waits for the current level");
            start.await.expect("the delayed connection starts safely");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.6),
                    JournalEvent::Live(LivePath::Single),
                ],
                "the old level is never restored after the receiver connects"
            );
        }

        #[tokio::test]
        async fn recovery_build_reuses_the_latest_effective_level() {
            let volumes = EffectiveVolumeState::default();
            volumes.set(receiver(1), 0.2);

            let first_clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            AirPlaySessionTransportFactory::with_clients(first_clients)
                .start_with_volumes(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                    volumes.clone(),
                )
                .await
                .expect("first generation starts");

            // The supervisor retains this source across NetworkChanged and
            // Resume rebuilds. Model the later durable update before the
            // replacement transport reaches its post-connect barrier.
            volumes.set(receiver(1), 0.7);
            let recovery_clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let recovery_journal = recovery_clients.journal();
            AirPlaySessionTransportFactory::with_clients(recovery_clients)
                .start_with_volumes(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                    volumes,
                )
                .await
                .expect("recovery generation starts");

            assert_eq!(
                recovery_journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.7),
                    JournalEvent::Live(LivePath::Single),
                ],
                "the replacement session must not restore the earlier level"
            );
        }

        #[tokio::test]
        async fn changed_mute_during_initial_volume_is_reapplied_before_audio() {
            let (volume_started_tx, volume_started_rx) = oneshot::channel();
            let (continue_volume_tx, continue_volume_rx) = oneshot::channel();
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                volume_started: Some(volume_started_tx),
                volume_continue: Some(continue_volume_rx),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);
            let volumes = EffectiveVolumeState::default();
            volumes.set(receiver(1), 0.4);

            let start = factory.start_with_volumes(
                vec![device(1)],
                None,
                decoder(),
                latency(),
                CalibrationProfile::default(),
                volumes.clone(),
            );
            tokio::pin!(start);
            tokio::select! {
                _ = &mut start => panic!("initial volume gate released too early"),
                _ = volume_started_rx => {}
            }
            // This is the persisted muted effective value delivered while no
            // transport is active. Audio must wait for this reapplication.
            volumes.set(receiver(1), 0.0);
            continue_volume_tx
                .send(())
                .expect("first volume call is gated");
            start.await.expect("the revalidated setup succeeds");

            assert_eq!(
                journal.lock().unwrap().events,
                vec![
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.4),
                    JournalEvent::Volume(DeviceId([0, 0, 0, 0, 0, 1]), 0.0),
                    JournalEvent::Live(LivePath::Single),
                ],
                "RTP cannot start after an obsolete initial volume succeeds"
            );
        }

        #[tokio::test]
        async fn live_report_becomes_a_transport_carrying_the_group_state() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 2])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 2]), DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let mut transport = factory
                .start(
                    vec![device(1), device(2)],
                    Some(receiver(2)),
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("a report with a primary is a live session");

            assert_eq!(transport.primary(), Some(receiver(2)));
            assert_eq!(
                transport.active_members(),
                BTreeSet::from([receiver(1), receiver(2)])
            );
            assert!(transport.setup_failures().is_empty());

            let journal = journal.lock().unwrap();
            assert_eq!(journal.render_delay, Some(275));
            assert!(journal.live_started, "audio must be started after connect");
            let (devices, preferred) = journal.connected.clone().expect("connect was called");
            assert_eq!(
                devices,
                vec![DeviceId([0, 0, 0, 0, 0, 1]), DeviceId([0, 0, 0, 0, 0, 2])]
            );
            assert_eq!(preferred, Some(DeviceId([0, 0, 0, 0, 0, 2])));
            assert_eq!(journal.disconnects, 0);
        }

        #[tokio::test]
        async fn calibration_is_normalized_against_the_receivers_that_connected() {
            // Three receivers were desired and calibrated; only two came up.
            // The absent one must neither appear in the delay map nor shift
            // the survivors, and the reference receiver -- an authoring
            // anchor, not part of the arithmetic -- being gone must not cost
            // the group its audio.
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 2])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 2]), DeviceId([0, 0, 0, 0, 0, 3])],
                    failures: vec![failure(1, SetupPhase::Connect)],
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let calibration = CalibrationProfile {
                // Receiver 1 failed setup; it is still the stored anchor.
                reference_receiver: Some(receiver(1)),
                requested_relative_delay_ns: BTreeMap::from([
                    (receiver(1), -50_000_000),
                    (receiver(2), 4_000_000),
                    (receiver(3), 6_000_000),
                ]),
            };

            let transport = factory
                .start(
                    vec![device(1), device(2), device(3)],
                    None,
                    decoder(),
                    latency(),
                    calibration,
                )
                .await
                .expect("a missing anchor must not cost the group its audio");
            assert_eq!(
                transport.active_members(),
                BTreeSet::from([receiver(2), receiver(3)])
            );

            let journal = journal.lock().unwrap();
            assert_eq!(journal.live_path, Some(LivePath::Group));
            // Exactly one entry per connected target, renormalized so the
            // earliest survivor holds zero. Receiver 1's -50 ms is ignored;
            // had it counted, both survivors would carry +54/+56 ms and the
            // group as a whole would sit 54 ms late for no reason.
            assert_eq!(
                journal.presentation_delays,
                Some(BTreeMap::from([
                    (DeviceId([0, 0, 0, 0, 0, 2]), 0_u64),
                    (DeviceId([0, 0, 0, 0, 0, 3]), 2_000_000_u64),
                ])),
            );
            assert_eq!(journal.disconnects, 0);
        }

        #[tokio::test]
        async fn a_lone_survivor_streams_over_the_single_receiver_path() {
            // One live member means the client fell back to the single-
            // receiver NTP session, which has no PTP clock identity; the
            // group streaming entry point would fail on exactly that.
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: vec![failure(2, SetupPhase::Connect)],
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let transport = factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("a lone survivor is still a session");

            assert_eq!(transport.primary(), Some(receiver(1)));
            assert_eq!(
                journal.lock().unwrap().live_path,
                Some(LivePath::Single),
                "a one-member session must not use the PTP group path"
            );
        }

        #[tokio::test]
        async fn two_or_more_members_stream_over_the_ptp_group_path() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1]), DeviceId([0, 0, 0, 0, 0, 2])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("a two-member group is a session");

            assert_eq!(
                journal.lock().unwrap().live_path,
                Some(LivePath::Group),
                "a shared PTP domain streams through the group path"
            );
        }

        #[tokio::test]
        async fn partial_success_keeps_survivors_and_reports_the_failed_member() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1]), DeviceId([0, 0, 0, 0, 0, 2])],
                    failures: vec![failure(3, SetupPhase::Pair)],
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let mut transport = factory
                .start(
                    vec![device(1), device(2), device(3)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("partial success is success");

            assert_eq!(
                transport.active_members(),
                BTreeSet::from([receiver(1), receiver(2)]),
                "survivors keep the session"
            );
            let failures = transport.setup_failures();
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].receiver, DeviceId([0, 0, 0, 0, 0, 3]));
            assert_eq!(failures[0].phase, SetupPhase::Pair);
            assert!(journal.lock().unwrap().live_started);
        }

        #[tokio::test]
        async fn a_report_without_a_primary_fails_the_start_and_tears_down() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: None,
                    connected: Vec::new(),
                    failures: vec![
                        failure(1, SetupPhase::PrimaryTiming),
                        failure(2, SetupPhase::Connect),
                    ],
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let Err(failure) = factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
            else {
                panic!("no primary means no session");
            };

            assert_eq!(failure.error.as_str(), NO_MEMBER_REACHABLE);
            assert_eq!(
                failure
                    .failures
                    .iter()
                    .map(|item| item.phase)
                    .collect::<Vec<_>>(),
                vec![SetupPhase::PrimaryTiming, SetupPhase::Connect],
                "phase classification must survive the translation"
            );
            let journal = journal.lock().unwrap();
            assert!(
                journal.disconnects >= 1,
                "every member of a failed attempt is torn down"
            );
            assert!(
                !journal.live_started,
                "audio must never start without a primary"
            );
        }

        #[tokio::test]
        async fn a_hard_connect_error_never_leaks_addresses_or_ports() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Err(airplay_core::Error::Rtsp(RtspError::SetupFailed(
                    "SETUP to 192.168.1.42:7000 failed".to_owned(),
                )))),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let Err(failure) = factory
                .start(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
            else {
                panic!("a hard protocol error is a start failure");
            };

            let message = failure.error.as_str();
            assert_eq!(message, NO_MEMBER_REACHABLE);
            assert!(!message.contains("192.168"), "no address may leak");
            assert!(!message.contains("7000"), "no port may leak");
            assert!(journal.lock().unwrap().disconnects >= 1);
        }

        #[tokio::test]
        async fn a_failed_audio_start_tears_the_group_down_again() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: vec![failure(2, SetupPhase::Connect)],
                })),
                live: Some(Err(airplay_core::Error::Rtsp(RtspError::NoSession))),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let Err(failure) = factory
                .start(
                    vec![device(1), device(2)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
            else {
                panic!("a group without audio is not a session");
            };

            assert_eq!(failure.error.as_str(), AUDIO_START_FAILED);
            assert_eq!(failure.failures.len(), 1);
            assert_eq!(journal.lock().unwrap().disconnects, 1);
        }

        #[tokio::test]
        async fn an_unavailable_protocol_stack_fails_before_any_connect() {
            let clients = ScriptedClients::new(Script {
                create_error: Some(UserFacingError::new("stack unavailable")),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let Err(failure) = factory
                .start(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
            else {
                panic!("no client, no session");
            };

            assert_eq!(failure.error.as_str(), "stack unavailable");
            assert!(failure.failures.is_empty());
            assert!(journal.lock().unwrap().connected.is_none());
        }

        #[tokio::test]
        async fn sender_buffer_ms_is_not_guessed_onto_the_render_lead() {
            for sender_buffer_ms in [500, 4_000] {
                let clients = ScriptedClients::new(Script {
                    connect: Some(Ok(GroupConnectReport {
                        primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                        connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                        failures: Vec::new(),
                    })),
                    ..Script::default()
                });
                let journal = clients.journal();
                let factory = AirPlaySessionTransportFactory::with_clients(clients);

                factory
                    .start(
                        vec![device(1)],
                        None,
                        decoder(),
                        LatencyConfig {
                            sender_buffer_ms,
                            render_delay_ms: 200,
                        },
                        CalibrationProfile::default(),
                    )
                    .await
                    .expect("start succeeds");

                let journal = journal.lock().unwrap();
                assert_eq!(journal.sender_buffer, Some(sender_buffer_ms));
                assert_eq!(
                    journal.render_delay,
                    Some(200),
                    "render lead remains independent from the local sender buffer"
                );
            }
        }

        #[tokio::test]
        async fn invalid_sender_buffer_capacity_does_not_connect() {
            let clients = ScriptedClients::new(Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            });
            let journal = clients.journal();
            let factory = AirPlaySessionTransportFactory::with_clients(clients);

            let result = factory
                .start(
                    vec![device(1)],
                    None,
                    decoder(),
                    LatencyConfig {
                        sender_buffer_ms: 99,
                        render_delay_ms: 200,
                    },
                    CalibrationProfile::default(),
                )
                .await;
            let Err(failure) = result else {
                panic!("invalid local buffer capacity is rejected before connect");
            };

            assert_eq!(failure.error.as_str(), "the selected sender buffer capacity is invalid");
            assert!(journal.lock().unwrap().connected.is_none());
        }
    }

    mod runtime {
        use super::*;

        async fn live_transport(clients: Arc<ScriptedClients>) -> Box<dyn SessionTransport> {
            let clients: Arc<dyn GroupClientFactory> = clients;
            let factory = AirPlaySessionTransportFactory::with_clients(clients);
            factory
                .start(
                    vec![device(1)],
                    None,
                    decoder(),
                    latency(),
                    CalibrationProfile::default(),
                )
                .await
                .expect("scripted start succeeds")
        }

        fn healthy_script() -> Script {
            Script {
                connect: Some(Ok(GroupConnectReport {
                    primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                    connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                    failures: Vec::new(),
                })),
                ..Script::default()
            }
        }

        #[tokio::test]
        async fn runtime_events_are_handed_out_in_order() {
            let clients = ScriptedClients::new(healthy_script());
            let feed = clients.feed.clone();
            let mut transport = live_transport(Arc::clone(&clients)).await;

            assert!(feed.try_publish(GroupRuntimeEvent::TargetSendFailed {
                receiver: DeviceId([0, 0, 0, 0, 0, 1]),
                source: "fan-out send failed".to_owned(),
            }));

            match transport.try_runtime_event() {
                Ok(Some(GroupRuntimeEvent::TargetSendFailed { receiver, .. })) => {
                    assert_eq!(receiver, DeviceId([0, 0, 0, 0, 0, 1]));
                }
                other => panic!("expected the published event, got {other:?}"),
            }
            assert!(matches!(transport.try_runtime_event(), Ok(None)));
        }

        #[tokio::test]
        async fn dropped_events_are_reported_once_as_a_delta() {
            let clients = ScriptedClients::new(healthy_script());
            let mut transport = live_transport(Arc::clone(&clients)).await;

            drop_one(&clients);
            drop_one(&clients);

            assert_eq!(
                transport.try_runtime_event(),
                Err(RuntimeEventLag { dropped: 2 }),
                "the whole gap is reported truthfully at once"
            );
            assert!(
                matches!(transport.try_runtime_event(), Ok(None)),
                "the same gap must never be reported twice"
            );

            drop_one(&clients);
            assert_eq!(
                transport.try_runtime_event(),
                Err(RuntimeEventLag { dropped: 1 }),
                "a later gap is a new delta"
            );
        }

        #[tokio::test]
        async fn a_still_lagging_feed_keeps_handing_out_the_events_it_kept() {
            // A receiver that fails continuously keeps the bounded feed full,
            // so every drain sees a fresh loss delta. Reporting that delta
            // must never starve the events the feed did keep.
            let clients = ScriptedClients::new(healthy_script());
            let feed = clients.feed.clone();
            let mut transport = live_transport(Arc::clone(&clients)).await;

            for _ in 0..2 {
                assert!(feed.try_publish(GroupRuntimeEvent::TargetSendFailed {
                    receiver: DeviceId([0, 0, 0, 0, 0, 1]),
                    source: "fan-out send failed".to_owned(),
                }));
            }
            // The producer keeps dropping while those two wait in the queue.
            drop_one(&clients);
            drop_one(&clients);

            let mut delivered = 0_u32;
            for _ in 0..8 {
                match transport.try_runtime_event() {
                    Ok(Some(_)) => delivered += 1,
                    Ok(None) => break,
                    Err(_) => {
                        // A gap report is fine, but it must not be the only
                        // thing this feed ever produces.
                        drop_one(&clients);
                    }
                }
            }

            assert_eq!(
                delivered, 2,
                "kept events must reach the supervisor even while the feed keeps losing"
            );
        }

        #[tokio::test]
        async fn volume_and_probes_route_by_stable_receiver_identity() {
            let mut script = healthy_script();
            script.probe = vec![MemberResult {
                receiver: DeviceId([0, 0, 0, 0, 0, 1]),
                result: Ok(Health::Healthy),
            }];
            let clients = ScriptedClients::new(script);
            let journal = clients.journal();
            let mut transport = live_transport(Arc::clone(&clients)).await;

            let outcome = transport.set_member_volume(&receiver(1), 0.4).await;
            assert_eq!(outcome.receiver, DeviceId([0, 0, 0, 0, 0, 1]));
            assert!(outcome.result.is_ok());

            let probes = transport.probe_members().await;
            assert_eq!(probes.len(), 1);
            assert!(matches!(probes[0].result, Ok(Health::Healthy)));

            let journal = journal.lock().unwrap();
            assert_eq!(
                journal.volumes,
                vec![
                    (DeviceId([0, 0, 0, 0, 0, 1]), 1.0),
                    (DeviceId([0, 0, 0, 0, 0, 1]), 0.4),
                ],
                "initial setup and live control address the same stable receiver"
            );
            assert_eq!(journal.probes, 1);
        }

        #[tokio::test]
        async fn stop_tears_the_group_down_exactly_once() {
            let clients = ScriptedClients::new(healthy_script());
            let journal = clients.journal();
            let mut transport = live_transport(Arc::clone(&clients)).await;

            transport.stop().await.expect("teardown succeeds");

            assert_eq!(journal.lock().unwrap().disconnects, 1);
        }

        #[tokio::test]
        async fn setup_failures_are_handed_out_once_and_never_duplicated() {
            let mut script = healthy_script();
            script.connect = Some(Ok(GroupConnectReport {
                primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                failures: vec![failure(2, SetupPhase::Pair)],
            }));
            let clients = ScriptedClients::new(script);
            let mut transport = live_transport(Arc::clone(&clients)).await;

            assert_eq!(transport.setup_failures().len(), 1);
            assert!(
                transport.setup_failures().is_empty(),
                "the supervisor consumes them exactly once"
            );
        }

        #[tokio::test]
        async fn a_pairing_failure_keeps_its_non_retryable_classification() {
            let mut script = healthy_script();
            script.connect = Some(Ok(GroupConnectReport {
                primary: Some(DeviceId([0, 0, 0, 0, 0, 1])),
                connected: vec![DeviceId([0, 0, 0, 0, 0, 1])],
                failures: vec![MemberFailure {
                    receiver: DeviceId([0, 0, 0, 0, 0, 2]),
                    phase: SetupPhase::Pair,
                    retryable: false,
                    source: airplay_core::Error::Pairing(PairingError::SrpVerificationFailed),
                }],
            }));
            let clients = ScriptedClients::new(script);
            let mut transport = live_transport(Arc::clone(&clients)).await;

            let failures = transport.setup_failures();
            assert!(!failures[0].retryable);
            assert_eq!(failures[0].phase, SetupPhase::Pair);
        }
    }
}
