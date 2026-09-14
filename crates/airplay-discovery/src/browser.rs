//! mDNS service browser implementation.

use crate::parser::TxtRecordParser;
use crate::traits::{BrowseEvent, BrowseStream, Discovery};
use crate::{AIRPLAY_SERVICE_TYPE, RAOP_SERVICE_TYPE};
use airplay_core::error::DiscoveryError;
use airplay_core::{Device, DeviceId, Result};
use async_trait::async_trait;
use futures::StreamExt;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::Stream;
use tracing::{debug, trace, warn};

/// Stream of raw mDNS service events for one browse registration.
pub(crate) type ServiceEventStream = Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>;

/// One independent mDNS daemon instance with its own browse registrations.
pub(crate) trait BrowseDaemon: Send + Sync + 'static {
    /// Start browsing a single service type on this daemon instance.
    fn start_browse(
        &self,
        service_type: &'static str,
    ) -> std::result::Result<ServiceEventStream, DiscoveryError>;

    /// Stop browsing the given service type.
    fn stop_browse(&self, service_type: &'static str);

    /// Shut the daemon down entirely.
    fn shutdown(&self);
}

/// Creates independent daemon instances so every browse/scan operation owns
/// its own daemon lifetime. Seam for tests to inject fake daemons.
pub(crate) trait DaemonFactory: Send + Sync + 'static {
    /// Create a fresh daemon instance.
    fn create_daemon(&self) -> std::result::Result<Arc<dyn BrowseDaemon>, DiscoveryError>;
}

/// Default factory creating real mDNS daemons.
pub(crate) struct MdnsDaemonFactory;

impl DaemonFactory for MdnsDaemonFactory {
    fn create_daemon(&self) -> std::result::Result<Arc<dyn BrowseDaemon>, DiscoveryError> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| DiscoveryError::Daemon(format!("Failed to create mDNS daemon: {}", e)))?;
        Ok(Arc::new(MdnsDaemon { daemon }))
    }
}

/// Adapter around a real `mdns_sd` daemon handle.
struct MdnsDaemon {
    daemon: ServiceDaemon,
}

impl BrowseDaemon for MdnsDaemon {
    fn start_browse(
        &self,
        service_type: &'static str,
    ) -> std::result::Result<ServiceEventStream, DiscoveryError> {
        let receiver = self.daemon.browse(service_type).map_err(|e| {
            DiscoveryError::Daemon(format!("Failed to browse {}: {}", service_type, e))
        })?;
        Ok(Box::pin(futures::stream::unfold(
            receiver,
            |receiver| async move {
                match receiver.recv_async().await {
                    Ok(event) => Some((event, receiver)),
                    Err(_) => None,
                }
            },
        )))
    }

    fn stop_browse(&self, service_type: &'static str) {
        let _ = self.daemon.stop_browse(service_type);
    }

    fn shutdown(&self) {
        let _ = self.daemon.shutdown();
    }
}

/// Guard tying one browse stream to its daemon registration.
///
/// Dropping it stops both service registrations and shuts the daemon down,
/// which ends that browse without affecting any other browse.
struct BrowseRegistration {
    daemon: Arc<dyn BrowseDaemon>,
}

impl Drop for BrowseRegistration {
    fn drop(&mut self) {
        self.daemon.stop_browse(AIRPLAY_SERVICE_TYPE);
        self.daemon.stop_browse(RAOP_SERVICE_TYPE);
        self.daemon.shutdown();
    }
}

/// Which mDNS service type a keyed record belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ServiceKind {
    AirPlay,
    Raop,
}

impl ServiceKind {
    fn is_raop(self) -> bool {
        matches!(self, Self::Raop)
    }
}

/// Uniquely identifies one advertised service instance of a receiver.
///
/// The same fullname can legitimately appear under both service types, so
/// the kind is part of the identity; the recorded mapping is what allows
/// removals to find their receiver without parsing service names.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ServiceKey {
    kind: ServiceKind,
    fullname: String,
}

impl ServiceKey {
    fn new(kind: ServiceKind, fullname: impl Into<String>) -> Self {
        Self {
            kind,
            fullname: fullname.into(),
        }
    }
}

/// Latest resolved record per service type for one physical receiver.
#[derive(Default)]
struct ReceiverServices {
    airplay: Option<Device>,
    raop: Option<Device>,
}

impl ReceiverServices {
    fn slot_mut(&mut self, kind: ServiceKind) -> &mut Option<Device> {
        match kind {
            ServiceKind::AirPlay => &mut self.airplay,
            ServiceKind::Raop => &mut self.raop,
        }
    }

    /// The user-visible device: AirPlay fields preferred, gaps filled from
    /// the RAOP record via `TxtRecordParser::merge_device_info`.
    fn merged(&self) -> Option<Device> {
        match (&self.airplay, &self.raop) {
            (Some(airplay), Some(raop)) => Some(TxtRecordParser::merge_device_info(airplay, raop)),
            (Some(airplay), None) => Some(airplay.clone()),
            (None, Some(raop)) => Some(raop.clone()),
            (None, None) => None,
        }
    }
}

/// Dual-service bookkeeping mapping every advertised service instance to its
/// physical receiver and retaining each receiver's per-service records.
///
/// A receiver row exists as long as at least one of its services is known;
/// it is dropped only when both services are gone. Rows whose AirPlay record
/// was removed persist as non-castable RAOP-only cache entries.
#[derive(Default)]
struct ServiceIndex {
    service_to_receiver: HashMap<ServiceKey, DeviceId>,
    resolvers: HashMap<DeviceId, ReceiverServices>,
}

impl ServiceIndex {
    /// The receiver a service instance currently maps to.
    fn resolve(&self, key: &ServiceKey) -> Option<&DeviceId> {
        self.service_to_receiver.get(key)
    }

    /// Record a resolved service instance and return the resulting event:
    /// `Added` when the receiver first becomes known, `Updated` afterwards.
    fn upsert(&mut self, key: ServiceKey, device: Device) -> BrowseEvent {
        let receiver = device.id.clone();

        // If this service instance previously belonged to another receiver
        // (fullname reuse after replacement), detach it from the stale row.
        if let Some(previous) = self
            .service_to_receiver
            .insert(key.clone(), receiver.clone())
        {
            if previous != receiver {
                self.clear_service_slot(&previous, &key);
            }
        }

        let known = self.contains(&receiver);
        let services = self.resolvers.entry(receiver).or_default();
        *services.slot_mut(key.kind) = Some(device);

        match services.merged() {
            Some(merged) if known => BrowseEvent::Updated(merged),
            Some(merged) => BrowseEvent::Added(merged),
            None => unreachable!("service slot was just filled"),
        }
    }

    /// Remove one service instance's mapping and slot.
    ///
    /// Returns the receiver id the key mapped to, if any; the receiver's row
    /// is removed only when both of its services are gone.
    fn remove(&mut self, key: &ServiceKey) -> Option<DeviceId> {
        let receiver = self.service_to_receiver.remove(key)?;
        self.clear_service_slot(&receiver, key);
        Some(receiver)
    }

    /// Apply a `ServiceRemoved` for `key` and derive the implied event.
    ///
    /// Clearing RAOP emits `Updated` while AirPlay remains; clearing AirPlay
    /// always emits `Removed` even when a RAOP-only cache record persists;
    /// dropping a final RAOP-only service is silent.
    fn removal_event(&mut self, key: &ServiceKey) -> Option<BrowseEvent> {
        let receiver = self.resolve(key)?.clone();
        let _ = self.remove(key);
        trace!(
            receiver = %receiver.to_mac_string(),
            castable = self.is_castable(&receiver),
            receivers_known = self.receiver_count(),
            "service record removed"
        );

        match key.kind {
            ServiceKind::AirPlay => Some(BrowseEvent::Removed(receiver)),
            ServiceKind::Raop => self
                .resolvers
                .get(&receiver)
                .and_then(ReceiverServices::merged)
                .map(BrowseEvent::Updated),
        }
    }

    /// Whether the receiver still has an AirPlay record (i.e., is castable).
    fn is_castable(&self, id: &DeviceId) -> bool {
        self.resolvers
            .get(id)
            .is_some_and(|services| services.airplay.is_some())
    }

    /// Whether any record (even RAOP-only) exists for the receiver.
    fn contains(&self, id: &DeviceId) -> bool {
        self.resolvers.contains_key(id)
    }

    /// Number of known receivers.
    fn receiver_count(&self) -> usize {
        self.resolvers.len()
    }

    /// The merged user-visible device for a receiver.
    fn device(&self, id: &DeviceId) -> Option<Device> {
        self.resolvers.get(id).and_then(ReceiverServices::merged)
    }

    /// All merged devices currently known.
    fn all_devices(&self) -> Vec<Device> {
        self.resolvers
            .values()
            .filter_map(ReceiverServices::merged)
            .collect()
    }

    fn clear_service_slot(&mut self, receiver: &DeviceId, key: &ServiceKey) {
        if let Some(services) = self.resolvers.get_mut(receiver) {
            *services.slot_mut(key.kind) = None;
            if services.airplay.is_none() && services.raop.is_none() {
                self.resolvers.remove(receiver);
            }
        }
    }
}

/// mDNS service browser for AirPlay device discovery.
pub struct ServiceBrowser {
    index: Arc<RwLock<ServiceIndex>>,
    daemon_factory: Arc<dyn DaemonFactory>,
    /// Incremented on every explicit stop; streams terminate once they see a
    /// higher value than their own creation epoch.
    stop_epoch: Arc<AtomicU64>,
    /// Wakeup signal broadcast to all active browse streams.
    stop_signal: broadcast::Sender<()>,
}

impl ServiceBrowser {
    /// Create a new service browser.
    pub fn new() -> Result<Self> {
        Ok(Self::with_factory(Arc::new(MdnsDaemonFactory)))
    }

    /// Create a service browser using a custom daemon factory (test seam).
    pub(crate) fn with_factory(daemon_factory: Arc<dyn DaemonFactory>) -> Self {
        let (stop_signal, _) = broadcast::channel(16);
        Self {
            index: Arc::new(RwLock::new(ServiceIndex::default())),
            daemon_factory,
            stop_epoch: Arc::new(AtomicU64::new(0)),
            stop_signal,
        }
    }

    /// Parse a resolved mDNS service into a Device.
    fn parse_service_event(
        service_info: &mdns_sd::ServiceInfo,
        kind: ServiceKind,
    ) -> Option<Device> {
        let name = service_info.get_fullname();
        let port = service_info.get_port();

        // Collect addresses - mdns-sd returns IpAddr directly
        let addresses: Vec<IpAddr> = service_info.get_addresses().iter().copied().collect();

        if addresses.is_empty() {
            debug!("Service {} has no addresses, skipping", name);
            return None;
        }

        // Build TXT record map
        let txt: HashMap<String, String> = service_info
            .get_properties()
            .iter()
            .map(|prop| (prop.key().to_string(), prop.val_str().to_string()))
            .collect();

        // Extract service name (without domain suffix)
        let service_name = service_info
            .get_fullname()
            .split('.')
            .next()
            .unwrap_or(name);

        let result = if kind.is_raop() {
            TxtRecordParser::parse_raop_txt(service_name, &txt, addresses, port)
        } else {
            TxtRecordParser::parse_airplay_txt(service_name, &txt, addresses, port)
        };

        match result {
            Ok(device) => {
                debug!(
                    "Parsed device: {} ({})",
                    device.name,
                    device.id.to_mac_string()
                );
                Some(device)
            }
            Err(e) => {
                warn!("Failed to parse service {}: {}", name, e);
                None
            }
        }
    }

    /// Handle a service event and optionally return a browse event.
    async fn handle_service_event(
        event: ServiceEvent,
        kind: ServiceKind,
        index: &Arc<RwLock<ServiceIndex>>,
    ) -> Option<BrowseEvent> {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                trace!("Service resolved: {}", info.get_fullname());
                if let Some(device) = Self::parse_service_event(&info, kind) {
                    let key = ServiceKey::new(kind, info.get_fullname());
                    let mut index = index.write().await;
                    Some(index.upsert(key, device))
                } else {
                    None
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                trace!("Service removed: {}", fullname);
                // Removals use the recorded (kind, fullname) -> receiver
                // mapping instead of parsing the service name.
                let key = ServiceKey::new(kind, fullname);
                let mut index = index.write().await;
                index.removal_event(&key)
            }
            ServiceEvent::SearchStarted(_) => {
                trace!("Search started");
                None
            }
            ServiceEvent::SearchStopped(_) => {
                trace!("Search stopped");
                None
            }
            _ => None,
        }
    }
}

impl Default for ServiceBrowser {
    fn default() -> Self {
        Self::new().expect("Failed to create ServiceBrowser")
    }
}

#[async_trait]
impl Discovery for ServiceBrowser {
    async fn browse(&self) -> Result<BrowseStream> {
        // Every browse runs on its own daemon instance with an independent
        // lifetime; the registration guard below stops it when the stream drops.
        let daemon = self.daemon_factory.create_daemon()?;
        let airplay_events = daemon.start_browse(AIRPLAY_SERVICE_TYPE)?;
        let raop_events = daemon.start_browse(RAOP_SERVICE_TYPE)?;

        let index = Arc::clone(&self.index);
        let mut stop_rx = self.stop_signal.subscribe();
        let stop_epoch = Arc::clone(&self.stop_epoch);
        let epoch = self.stop_epoch.load(Ordering::SeqCst);

        let registration = BrowseRegistration { daemon };

        let stream = async_stream::stream! {
            let _registration = registration;
            let mut airplay_events = airplay_events;
            let mut raop_events = raop_events;
            loop {
                tokio::select! {
                    stopped = stop_rx.recv() => {
                        if stopped.is_err() || stop_epoch.load(Ordering::SeqCst) > epoch {
                            break;
                        }
                    }
                    item = airplay_events.next() => match item {
                        Some(event) => {
                            if let Some(browse_event) =
                                Self::handle_service_event(event, ServiceKind::AirPlay, &index)
                                    .await
                            {
                                yield Ok(browse_event);
                            }
                        }
                        None => {
                            yield Err(DiscoveryError::Daemon(
                                "AirPlay browse channel closed".to_string(),
                            ));
                            break;
                        }
                    },
                    item = raop_events.next() => match item {
                        Some(event) => {
                            if let Some(browse_event) =
                                Self::handle_service_event(event, ServiceKind::Raop, &index).await
                            {
                                yield Ok(browse_event);
                            }
                        }
                        None => {
                            yield Err(DiscoveryError::Daemon(
                                "RAOP browse channel closed".to_string(),
                            ));
                            break;
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn scan(&self, timeout: Duration) -> Result<Vec<Device>> {
        // Scan consumes its own browse stream under a timeout, so it cannot
        // stop or interfere with any concurrent browse registration.
        let mut stream = self.browse().await?;
        let start = std::time::Instant::now();

        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, stream.next()).await {
                Ok(Some(Ok(_event))) => continue,
                Ok(Some(Err(error))) => {
                    debug!("browse stream ended during scan: {}", error);
                    break;
                }
                Ok(None) => break,
                Err(_elapsed) => break,
            }
        }

        drop(stream);

        // Return all discovered devices
        Ok(self.get_all_devices().await)
    }

    async fn stop(&self) {
        // Terminate every active browse stream of this browser; each stream's
        // registration guard then stops its own daemon.
        self.stop_epoch.fetch_add(1, Ordering::SeqCst);
        let _ = self.stop_signal.send(());
    }

    async fn get_device(&self, id: &DeviceId) -> Option<Device> {
        self.index.read().await.device(id)
    }

    async fn get_all_devices(&self) -> Vec<Device> {
        self.index.read().await.all_devices()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::{Features, Version};
    use std::net::Ipv4Addr;

    fn make_test_device(mac: [u8; 6], name: &str) -> Device {
        Device {
            id: DeviceId(mac),
            name: name.to_string(),
            model: "TestModel".to_string(),
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

    mod service_browser {
        use super::*;

        #[test]
        fn new_creates_empty_device_list() {
            // Note: This test requires mDNS to be available on the system
            // Skip if we can't create a daemon
            if let Ok(browser) = ServiceBrowser::new() {
                assert_eq!(browser.index.try_read().unwrap().receiver_count(), 0);
            }
        }
    }

    mod device_cache {
        use super::*;

        #[tokio::test]
        async fn get_device_returns_none_when_not_found() {
            if let Ok(browser) = ServiceBrowser::new() {
                let device_id = DeviceId([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
                let result = browser.get_device(&device_id).await;
                assert!(result.is_none());
            }
        }

        #[tokio::test]
        async fn get_device_returns_device_when_found() {
            if let Ok(browser) = ServiceBrowser::new() {
                let device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Test Device");
                let device_id = device.id.clone();

                // Insert device directly for testing
                browser.index.write().await.upsert(
                    ServiceKey::new(ServiceKind::AirPlay, "Test Device._airplay._tcp.local."),
                    device,
                );

                let result = browser.get_device(&device_id).await;
                assert!(result.is_some());
                assert_eq!(result.unwrap().name, "Test Device");
            }
        }

        #[tokio::test]
        async fn get_all_devices_returns_all_cached() {
            if let Ok(browser) = ServiceBrowser::new() {
                let device1 = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Device 1");
                let device2 = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Device 2");

                {
                    let mut index = browser.index.write().await;
                    index.upsert(
                        ServiceKey::new(ServiceKind::AirPlay, "Device 1._airplay._tcp.local."),
                        device1,
                    );
                    index.upsert(
                        ServiceKey::new(ServiceKind::AirPlay, "Device 2._airplay._tcp.local."),
                        device2,
                    );
                }

                let all = browser.get_all_devices().await;
                assert_eq!(all.len(), 2);
            }
        }
    }

    mod service_index {
        use super::*;

        const AP_FULLNAME: &str = "Living._airplay._tcp.local.";
        const RAOP_FULLNAME: &str = "AABBCCDDEE01@Living._raop._tcp.local.";

        fn device_id(n: u8) -> DeviceId {
            DeviceId([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n])
        }

        fn airplay_device(n: u8) -> Device {
            let mut device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n], "Living AirPlay");
            device.features = Features::from_raw(0x445F_8A00);
            device
        }

        fn raop_device(n: u8) -> Device {
            let mut device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n], "Living RAOP");
            device.features = Features::from_raw(0x01);
            device.raop_port = Some(5353);
            device
        }

        fn airplay_device_at(n: u8, ip: [u8; 4]) -> Device {
            let mut device = airplay_device(n);
            device.addresses = vec![IpAddr::V4(Ipv4Addr::from(ip))];
            device
        }

        fn key(kind: ServiceKind, fullname: &str) -> ServiceKey {
            ServiceKey {
                kind,
                fullname: fullname.to_string(),
            }
        }

        #[test]
        fn airplay_then_raop_merges_into_one_receiver_preferring_airplay_fields() {
            let mut index = ServiceIndex::default();

            let added = index.upsert(key(ServiceKind::AirPlay, AP_FULLNAME), airplay_device(1));
            assert!(matches!(added, BrowseEvent::Added(ref device) if device.id == device_id(1)));

            let updated = index.upsert(key(ServiceKind::Raop, RAOP_FULLNAME), raop_device(1));
            assert!(matches!(
                updated,
                BrowseEvent::Updated(ref device)
                    if device.id == device_id(1)
                        && device.name == "Living AirPlay"
                        && device.features.raw() == 0x445F_8A00
                        && device.raop_port == Some(5353)
            ));

            assert_eq!(
                index.resolve(&key(ServiceKind::AirPlay, AP_FULLNAME)),
                Some(&device_id(1))
            );
            assert_eq!(index.receiver_count(), 1);
            assert!(index.contains(&device_id(1)));
            assert!(index.is_castable(&device_id(1)));
        }

        #[test]
        fn raop_removal_keeps_receiver_as_updated_airplay_removal_emits_removed() {
            let mut index = ServiceIndex::default();
            index.upsert(key(ServiceKind::AirPlay, AP_FULLNAME), airplay_device(1));
            index.upsert(key(ServiceKind::Raop, RAOP_FULLNAME), raop_device(1));

            let updated = index
                .removal_event(&key(ServiceKind::Raop, RAOP_FULLNAME))
                .expect("RAOP removal with AirPlay present must emit");
            assert!(
                matches!(updated, BrowseEvent::Updated(ref device) if device.id == device_id(1))
            );
            assert!(index.contains(&device_id(1)));
            assert!(index.is_castable(&device_id(1)));

            // RAOP re-announces, then AirPlay disappears.
            index.upsert(key(ServiceKind::Raop, RAOP_FULLNAME), raop_device(1));
            let removed = index
                .removal_event(&key(ServiceKind::AirPlay, AP_FULLNAME))
                .expect("AirPlay removal must emit Removed");
            assert!(matches!(removed, BrowseEvent::Removed(ref id) if *id == device_id(1)));
            assert!(
                index.contains(&device_id(1)),
                "RAOP-only cache record persists"
            );
            assert!(!index.is_castable(&device_id(1)));
        }

        #[test]
        fn duplicate_fullnames_on_different_services_map_to_same_receiver() {
            const SHARED: &str = "Dup@Shared._tcp.local.";
            let mut index = ServiceIndex::default();
            index.upsert(key(ServiceKind::AirPlay, SHARED), airplay_device(2));
            index.upsert(key(ServiceKind::Raop, SHARED), raop_device(2));

            assert_eq!(
                index.resolve(&key(ServiceKind::AirPlay, SHARED)),
                Some(&device_id(2))
            );
            assert_eq!(
                index.resolve(&key(ServiceKind::Raop, SHARED)),
                Some(&device_id(2))
            );
            assert_eq!(index.receiver_count(), 1);
            assert!(index.is_castable(&device_id(2)));
        }

        #[test]
        fn removing_last_service_drops_receiver_and_count() {
            let mut index = ServiceIndex::default();
            index.upsert(key(ServiceKind::AirPlay, AP_FULLNAME), airplay_device(3));
            let removed = index
                .removal_event(&key(ServiceKind::AirPlay, AP_FULLNAME))
                .expect("last service removal emits");
            assert!(matches!(removed, BrowseEvent::Removed(ref id) if *id == device_id(3)));
            assert!(!index.contains(&device_id(3)));
            assert_eq!(index.receiver_count(), 0);

            let mut index = ServiceIndex::default();
            assert!(index
                .removal_event(&key(ServiceKind::Raop, RAOP_FULLNAME))
                .is_none());
            index.upsert(key(ServiceKind::Raop, RAOP_FULLNAME), raop_device(3));
            assert!(
                index
                    .removal_event(&key(ServiceKind::Raop, RAOP_FULLNAME))
                    .is_none(),
                "final RAOP removal is silent"
            );
            assert!(!index.contains(&device_id(3)));
            assert_eq!(index.receiver_count(), 0);
        }

        #[test]
        fn resolving_new_address_updates_same_receiver_id() {
            let mut index = ServiceIndex::default();
            index.upsert(
                key(ServiceKind::AirPlay, AP_FULLNAME),
                airplay_device_at(1, [192, 168, 1, 2]),
            );

            let event = index.upsert(
                key(ServiceKind::AirPlay, AP_FULLNAME),
                airplay_device_at(1, [192, 168, 1, 44]),
            );
            assert!(matches!(
                event,
                BrowseEvent::Updated(ref device)
                    if device.addresses[0].to_string() == "192.168.1.44"
            ));
            assert_eq!(index.receiver_count(), 1);
        }
    }

    // Integration tests that require real mDNS on the network
    // These are marked as ignored by default
    mod integration {
        use super::*;

        #[tokio::test]
        #[ignore = "requires real AirPlay devices on network"]
        async fn scan_finds_real_devices() {
            let browser = ServiceBrowser::new().expect("Failed to create browser");
            let devices = browser.scan(Duration::from_secs(5)).await.unwrap();

            println!("Found {} devices:", devices.len());
            for device in &devices {
                println!(
                    "  - {} ({}) at {:?}:{}",
                    device.name,
                    device.id.to_mac_string(),
                    device.addresses,
                    device.port
                );
                println!("    Model: {}", device.model);
                println!("    Features: 0x{:X}", device.features.raw());
                println!("    AirPlay 2: {}", device.supports_airplay2());
            }
        }

        #[tokio::test]
        #[ignore = "requires real AirPlay devices on network"]
        async fn browse_emits_events_for_real_devices() {
            use futures::StreamExt;

            let browser = ServiceBrowser::new().expect("Failed to create browser");
            let mut stream = browser.browse().await.unwrap();

            println!("Browsing for devices (10 seconds)...");

            let timeout = tokio::time::sleep(Duration::from_secs(10));
            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    event = stream.next() => {
                        match event {
                            Some(Ok(BrowseEvent::Added(device))) => {
                                println!("+ Added: {} ({})", device.name, device.id.to_mac_string());
                            }
                            Some(Ok(BrowseEvent::Updated(device))) => {
                                println!("~ Updated: {} ({})", device.name, device.id.to_mac_string());
                            }
                            Some(Ok(BrowseEvent::Removed(id))) => {
                                println!("- Removed: {}", id.to_mac_string());
                            }
                            Some(Err(error)) => {
                                println!("discovery stopped: {}", error);
                                break;
                            }
                            None => break,
                        }
                    }
                    _ = &mut timeout => {
                        println!("Timeout reached");
                        break;
                    }
                }
            }

            browser.stop().await;
        }
    }
}
