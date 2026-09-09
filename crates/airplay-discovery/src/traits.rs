//! Trait definitions for service discovery (enables mocking in tests).

use airplay_core::error::DiscoveryError;
use airplay_core::{Device, DeviceId, Result};
use async_trait::async_trait;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

/// Fallible stream of browse events.
///
/// Each stream is tied to one independent daemon registration: dropping the
/// stream stops that registration's daemon without affecting other streams.
pub type BrowseStream =
    Pin<Box<dyn Stream<Item = std::result::Result<BrowseEvent, DiscoveryError>> + Send>>;

/// Event emitted during device browsing.
#[derive(Debug, Clone)]
pub enum BrowseEvent {
    /// New device discovered.
    Added(Device),
    /// Existing device updated (e.g., IP changed).
    Updated(Device),
    /// Device went offline.
    Removed(DeviceId),
}

impl BrowseEvent {
    /// Get the device from an Added or Updated event.
    pub fn device(&self) -> Option<&Device> {
        match self {
            BrowseEvent::Added(d) | BrowseEvent::Updated(d) => Some(d),
            BrowseEvent::Removed(_) => None,
        }
    }

    /// Get the device ID from any event.
    pub fn device_id(&self) -> &DeviceId {
        match self {
            BrowseEvent::Added(d) | BrowseEvent::Updated(d) => &d.id,
            BrowseEvent::Removed(id) => id,
        }
    }

    /// Check if this is an Added event.
    pub fn is_added(&self) -> bool {
        matches!(self, BrowseEvent::Added(_))
    }

    /// Check if this is an Updated event.
    pub fn is_updated(&self) -> bool {
        matches!(self, BrowseEvent::Updated(_))
    }

    /// Check if this is a Removed event.
    pub fn is_removed(&self) -> bool {
        matches!(self, BrowseEvent::Removed(_))
    }
}

/// Trait for service discovery implementations.
///
/// This trait enables testing with mock implementations.
#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait Discovery: Send + Sync {
    /// Start continuous browsing for AirPlay devices.
    ///
    /// Returns a fallible stream of browse events. Each call runs on its own
    /// daemon instance with an independent lifetime; dropping the returned
    /// stream stops that instance without affecting concurrent browses.
    async fn browse(&self) -> Result<BrowseStream>;

    /// Perform a one-shot scan with timeout.
    ///
    /// Collects all devices found within the timeout period.
    async fn scan(&self, timeout: Duration) -> Result<Vec<Device>>;

    /// Stop all browsing activity.
    async fn stop(&self);

    /// Get a specific device by ID if currently known.
    async fn get_device(&self, id: &DeviceId) -> Option<Device>;

    /// Get all currently known devices.
    async fn get_all_devices(&self) -> Vec<Device>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::{Features, Version};
    use std::net::{IpAddr, Ipv4Addr};

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

    mod browse_event {
        use super::*;

        #[test]
        fn added_event_contains_device() {
            let device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Test Device");
            let event = BrowseEvent::Added(device.clone());

            assert!(event.is_added());
            assert!(!event.is_updated());
            assert!(!event.is_removed());
            assert_eq!(event.device().unwrap().name, "Test Device");
            assert_eq!(event.device_id().0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        }

        #[test]
        fn updated_event_contains_device() {
            let device = make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Updated Device");
            let event = BrowseEvent::Updated(device.clone());

            assert!(!event.is_added());
            assert!(event.is_updated());
            assert!(!event.is_removed());
            assert_eq!(event.device().unwrap().name, "Updated Device");
            assert_eq!(event.device_id().0, [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        }

        #[test]
        fn removed_event_contains_device_id() {
            let device_id = DeviceId([0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE]);
            let event = BrowseEvent::Removed(device_id.clone());

            assert!(!event.is_added());
            assert!(!event.is_updated());
            assert!(event.is_removed());
            assert!(event.device().is_none());
            assert_eq!(event.device_id().0, [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE]);
        }
    }

    mod mock_discovery {
        use super::*;
        use futures::StreamExt;

        #[tokio::test]
        async fn mock_scan_returns_configured_devices() {
            let mut mock = MockDiscovery::new();

            let devices = vec![
                make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Device 1"),
                make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Device 2"),
            ];

            mock.expect_scan().returning(move |_| {
                let d = devices.clone();
                Box::pin(async move { Ok(d) })
            });

            let result = mock.scan(Duration::from_secs(5)).await.unwrap();
            assert_eq!(result.len(), 2);
            assert_eq!(result[0].name, "Device 1");
            assert_eq!(result[1].name, "Device 2");
        }

        #[tokio::test]
        async fn mock_get_device_returns_device() {
            let mut mock = MockDiscovery::new();

            let device = make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Test Device");

            mock.expect_get_device().returning(move |_| {
                let d = device.clone();
                Box::pin(async move { Some(d) })
            });

            let device_id = DeviceId([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
            let result = mock.get_device(&device_id).await;
            assert!(result.is_some());
            assert_eq!(result.unwrap().name, "Test Device");
        }

        #[tokio::test]
        async fn mock_get_all_devices_returns_all() {
            let mut mock = MockDiscovery::new();

            let devices = vec![
                make_test_device([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF], "Device 1"),
                make_test_device([0x11, 0x22, 0x33, 0x44, 0x55, 0x66], "Device 2"),
            ];

            mock.expect_get_all_devices().returning(move || {
                let d = devices.clone();
                Box::pin(async move { d })
            });

            let result = mock.get_all_devices().await;
            assert_eq!(result.len(), 2);
        }

        #[tokio::test]
        async fn mock_stop_is_callable() {
            let mut mock = MockDiscovery::new();

            mock.expect_stop().returning(|| Box::pin(async {}));

            mock.stop().await;
            // Test passes if stop() doesn't panic
        }

        #[tokio::test]
        async fn mock_browse_can_surface_daemon_failure() {
            let mut mock = MockDiscovery::new();
            mock.expect_browse().return_once(|| {
                let error = DiscoveryError::Daemon("closed".into());
                Box::pin(
                    async move { Ok(Box::pin(tokio_stream::iter([Err(error)])) as BrowseStream) },
                )
            });
            let mut stream = mock.browse().await.unwrap();
            assert!(stream.next().await.unwrap().is_err());
        }
    }

    mod discovery_streams {
        use super::*;
        use crate::browser::{BrowseDaemon, DaemonFactory, ServiceBrowser};
        use crate::{AIRPLAY_SERVICE_TYPE, RAOP_SERVICE_TYPE};
        use airplay_core::error::DiscoveryError;
        use futures::StreamExt;
        use mdns_sd::{ServiceEvent, ServiceInfo};
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        /// A fake mDNS daemon whose event sources are fed by the test.
        struct FakeDaemon {
            senders: Mutex<HashMap<&'static str, tokio::sync::mpsc::UnboundedSender<ServiceEvent>>>,
            stops: Mutex<HashMap<&'static str, usize>>,
            shutdown_count: AtomicUsize,
        }

        impl Default for FakeDaemon {
            fn default() -> Self {
                Self {
                    senders: Mutex::new(HashMap::new()),
                    stops: Mutex::new(HashMap::new()),
                    shutdown_count: AtomicUsize::new(0),
                }
            }
        }

        impl FakeDaemon {
            fn stop_count(&self, service_type: &str) -> usize {
                self.stops
                    .lock()
                    .unwrap()
                    .get(service_type)
                    .copied()
                    .unwrap_or(0)
            }

            fn shutdown_count(&self) -> usize {
                self.shutdown_count.load(Ordering::SeqCst)
            }

            fn is_alive(&self) -> bool {
                self.shutdown_count.load(Ordering::SeqCst) == 0
            }

            fn emit_airplay(&self, mac: &str, name: &str, ip: [u8; 4]) {
                let event = resolved_airplay_event(mac, name, ip);
                let sender = self
                    .senders
                    .lock()
                    .unwrap()
                    .get(AIRPLAY_SERVICE_TYPE)
                    .cloned();
                match sender {
                    Some(sender) => {
                        let _ = sender.send(event);
                    }
                    None => panic!("no AirPlay browse registered on this fake daemon"),
                }
            }
        }

        impl BrowseDaemon for FakeDaemon {
            fn start_browse(
                &self,
                service_type: &'static str,
            ) -> std::result::Result<crate::browser::ServiceEventStream, DiscoveryError>
            {
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                self.senders.lock().unwrap().insert(service_type, sender);
                Ok(Box::pin(
                    tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
                ))
            }

            fn stop_browse(&self, service_type: &'static str) {
                *self.stops.lock().unwrap().entry(service_type).or_insert(0) += 1;
            }

            fn shutdown(&self) {
                self.shutdown_count.fetch_add(1, Ordering::SeqCst);
            }
        }

        /// Factory handing out independent fake daemons, one per browse/scan call.
        #[derive(Clone)]
        struct FakeDaemonFactory {
            daemons: Arc<Mutex<Vec<Arc<FakeDaemon>>>>,
        }

        impl FakeDaemonFactory {
            fn new() -> Self {
                Self {
                    daemons: Arc::new(Mutex::new(Vec::new())),
                }
            }

            fn created_count(&self) -> usize {
                self.daemons.lock().unwrap().len()
            }

            fn daemon(&self, index: usize) -> Option<Arc<FakeDaemon>> {
                self.daemons.lock().unwrap().get(index).cloned()
            }

            fn active_registration_sets(&self) -> usize {
                self.daemons
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|d| d.is_alive())
                    .count()
            }
        }

        impl DaemonFactory for FakeDaemonFactory {
            fn create_daemon(&self) -> std::result::Result<Arc<dyn BrowseDaemon>, DiscoveryError> {
                let daemon = Arc::new(FakeDaemon::default());
                self.daemons.lock().unwrap().push(daemon.clone());
                Ok(daemon)
            }
        }

        fn resolved_airplay_event(mac: &str, name: &str, ip: [u8; 4]) -> ServiceEvent {
            let ip = std::net::Ipv4Addr::from(ip);
            ServiceEvent::ServiceResolved(
                ServiceInfo::new(
                    AIRPLAY_SERVICE_TYPE,
                    name,
                    "fake-host.local.",
                    ip.to_string(),
                    7000,
                    &[("deviceid", mac)][..],
                )
                .unwrap(),
            )
        }

        #[tokio::test]
        async fn browse_returns_fallible_stream_type() {
            let factory = FakeDaemonFactory::new();
            let browser = ServiceBrowser::with_factory(Arc::new(factory.clone()));

            let mut stream: BrowseStream = browser.browse().await.expect("browse must succeed");
            factory.daemon(0).expect("factory invoked").emit_airplay(
                "AA:BB:CC:DD:EE:03",
                "Living Room",
                [192, 168, 1, 3],
            );

            let item = stream.next().await.unwrap().expect("one fallible item");
            assert!(matches!(item, BrowseEvent::Added(ref device) if device.name == "Living Room"));
        }

        #[tokio::test]
        async fn each_browse_gets_independent_daemon_and_drop_stops_it() {
            let factory = FakeDaemonFactory::new();
            let browser = ServiceBrowser::with_factory(Arc::new(factory.clone()));

            let first = browser.browse().await.unwrap();
            let mut second = browser.browse().await.unwrap();
            assert_eq!(
                factory.created_count(),
                2,
                "each browse gets its own daemon"
            );

            drop(first);

            let first_daemon = factory.daemon(0).unwrap();
            assert_eq!(
                first_daemon.stop_count(AIRPLAY_SERVICE_TYPE),
                1,
                "drop stops first browse"
            );
            assert_eq!(first_daemon.stop_count(RAOP_SERVICE_TYPE), 1);
            assert_eq!(first_daemon.shutdown_count(), 1);
            assert_eq!(
                factory.daemon(1).unwrap().stop_count(AIRPLAY_SERVICE_TYPE),
                0,
                "second browse unaffected"
            );

            factory.daemon(1).unwrap().emit_airplay(
                "AA:BB:CC:DD:EE:04",
                "Kitchen",
                [192, 168, 1, 4],
            );
            let item = second
                .next()
                .await
                .unwrap()
                .expect("second stream still alive");
            assert!(matches!(item, BrowseEvent::Added(ref device) if device.name == "Kitchen"));

            drop(second);
            assert_eq!(factory.active_registration_sets(), 0);
        }

        #[tokio::test]
        async fn scan_uses_its_own_stream_under_timeout() {
            let factory = FakeDaemonFactory::new();
            let browser = ServiceBrowser::with_factory(Arc::new(factory.clone()));

            // Slow producer: emits an event long after the scan budget expires.
            let slow_factory = factory.clone();
            tokio::spawn(async move {
                let daemon = loop {
                    if let Some(daemon) = slow_factory.daemon(0) {
                        break daemon;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                tokio::time::sleep(Duration::from_millis(200)).await;
                daemon.emit_airplay("AA:BB:CC:DD:EE:05", "Slow Device", [192, 168, 1, 5]);
            });

            let started = Instant::now();
            let devices = browser.scan(Duration::from_millis(50)).await.unwrap();

            assert!(
                started.elapsed() < Duration::from_secs(5),
                "scan hung past its timeout"
            );
            assert!(devices.is_empty(), "slow event must not be included");
            assert_eq!(
                factory.created_count(),
                1,
                "scan owns exactly its own daemon"
            );
            assert_eq!(
                factory.daemon(0).unwrap().stop_count(AIRPLAY_SERVICE_TYPE),
                1
            );
            assert_eq!(factory.daemon(0).unwrap().shutdown_count(), 1);
        }

        #[tokio::test]
        async fn scan_drop_does_not_stop_existing_browse() {
            let factory = FakeDaemonFactory::new();
            let browser = ServiceBrowser::with_factory(Arc::new(factory.clone()));

            let browse = browser.browse().await.unwrap();
            browser.scan(Duration::from_millis(1)).await.unwrap();

            assert_eq!(factory.created_count(), 2, "scan created a second daemon");
            assert_eq!(
                factory.daemon(0).unwrap().stop_count(AIRPLAY_SERVICE_TYPE),
                0,
                "scan must not stop an existing browse"
            );
            assert_eq!(
                factory.daemon(1).unwrap().stop_count(AIRPLAY_SERVICE_TYPE),
                1,
                "scan releases only its own registration"
            );

            drop(browse);
            assert_eq!(
                factory.daemon(0).unwrap().stop_count(AIRPLAY_SERVICE_TYPE),
                1
            );
            assert_eq!(factory.active_registration_sets(), 0);
        }
    }
}
