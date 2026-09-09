//! Client events and handlers.
//!
//! # Drop accounting note (truthfulness)
//!
//! [`EventHandler`] deliveries remain unbounded direct `async` calls, so the
//! diagnostics snapshot's `client_events_dropped_total` stays at 0 until a
//! real bounded queue with an actual drop path exists for them; no drop
//! counter is invented for events that cannot drop. The group fan-out
//! failure feed below IS a real bounded queue with a real drop site: its
//! [`GroupRuntimeEventFeed::lost_events`] counter counts exactly the events
//! that had to be dropped because the channel was full or closed.

use crate::PlaybackState;
use airplay_core::{Device, DeviceId};
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Events emitted by the client.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Device discovered.
    DeviceDiscovered(Device),
    /// Device lost.
    DeviceLost(DeviceId),
    /// Connected to device.
    Connected(Device),
    /// Disconnected from device.
    Disconnected(Option<String>), // Optional reason
    /// Playback state changed.
    PlaybackStateChanged(PlaybackState),
    /// Playback position updated.
    PositionUpdated(f64),
    /// Volume changed.
    VolumeChanged(f32),
    /// Buffer level changed.
    BufferLevelChanged(f32),
    /// Error occurred.
    Error(String),
    /// Group membership changed.
    GroupChanged,
}

/// One significant client diagnostic event in the bounded ring feed.
///
/// `elapsed_ns` is stamped from the process-lifetime monotone base shared by
/// all diagnostics timestamps; `seq` orders same-nanosecond events within one
/// connection's sink and stays crate-internal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientDiagnosticEvent {
    pub elapsed_ns: u64,
    pub(crate) seq: u64,
    pub device_id: Option<DeviceId>,
    pub kind: crate::diagnostics::ClientDiagnosticKind,
}

/// Handler for client events.
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Called when an event occurs.
    async fn on_event(&self, event: ClientEvent);
}

/// No-op event handler.
pub struct NoOpHandler;

#[async_trait]
impl EventHandler for NoOpHandler {
    async fn on_event(&self, _event: ClientEvent) {}
}

/// Callback-based event handler.
pub struct CallbackHandler<F>
where
    F: Fn(ClientEvent) + Send + Sync,
{
    callback: F,
}

impl<F> CallbackHandler<F>
where
    F: Fn(ClientEvent) + Send + Sync,
{
    pub fn new(callback: F) -> Self {
        Self { callback }
    }
}

#[async_trait]
impl<F> EventHandler for CallbackHandler<F>
where
    F: Fn(ClientEvent) + Send + Sync,
{
    async fn on_event(&self, event: ClientEvent) {
        (self.callback)(event);
    }
}

// ---------------------------------------------------------------------------
// Bounded group runtime-event feed (fan-out send failures)
// ---------------------------------------------------------------------------

/// Capacity of the bounded runtime-event channel owned by the client.
pub const GROUP_RUNTIME_EVENT_CAPACITY: usize = 256;

/// Runtime fan-out failure observed for exactly one group member.
///
/// These are transport-level observations (a send targeted at one receiver
/// failed); they never substitute for RTSP health probes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupRuntimeEvent {
    /// One or more fan-out sends targeted at this receiver failed.
    TargetSendFailed {
        /// Stable receiver identity the failed send was addressed to.
        receiver: DeviceId,
        /// Pre-redacted human-readable summary of what failed.
        source: String,
    },
}

/// Cloneable producer half of the bounded runtime-event feed.
///
/// Publishing never blocks and therefore never stalls RTP pacing: when the
/// channel is full (or closed) the event is dropped and counted in the
/// atomic loss counter instead.
#[derive(Clone)]
pub struct GroupRuntimeEventFeed {
    tx: tokio::sync::mpsc::Sender<GroupRuntimeEvent>,
    lost_total: Arc<AtomicU64>,
}

impl GroupRuntimeEventFeed {
    /// Create a bounded feed with the given channel capacity.
    pub fn channel(capacity: usize) -> (Self, tokio::sync::mpsc::Receiver<GroupRuntimeEvent>) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity.max(1));
        (
            Self {
                tx,
                lost_total: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// Non-blocking publish; `false` means the event was dropped and counted.
    pub fn try_publish(&self, event: GroupRuntimeEvent) -> bool {
        use tokio::sync::mpsc::error::TrySendError;
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => {
                self.lost_total.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Events truthfully dropped because the channel could not accept them.
    pub fn lost_events(&self) -> u64 {
        self.lost_total.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod client_event {
        use super::*;
        use airplay_core::device::DeviceId;
        use airplay_core::device::Version;
        use airplay_core::features::Features;
        use std::net::{IpAddr, Ipv4Addr};

        fn make_test_device() -> Device {
            Device {
                id: DeviceId([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]),
                name: "Test Device".to_string(),
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

        #[test]
        fn all_events_constructible() {
            let device = make_test_device();
            let device_id = device.id.clone();

            let _ = ClientEvent::DeviceDiscovered(device.clone());
            let _ = ClientEvent::DeviceLost(device_id);
            let _ = ClientEvent::Connected(device.clone());
            let _ = ClientEvent::Disconnected(Some("test".to_string()));
            let _ = ClientEvent::Disconnected(None);
            let _ = ClientEvent::PlaybackStateChanged(PlaybackState::Playing);
            let _ = ClientEvent::PositionUpdated(10.5);
            let _ = ClientEvent::VolumeChanged(0.8);
            let _ = ClientEvent::BufferLevelChanged(0.5);
            let _ = ClientEvent::Error("test error".to_string());
            let _ = ClientEvent::GroupChanged;
        }

        #[test]
        fn events_are_clone() {
            let event = ClientEvent::PositionUpdated(10.5);
            let cloned = event.clone();
            match cloned {
                ClientEvent::PositionUpdated(pos) => assert_eq!(pos, 10.5),
                _ => panic!("Clone failed"),
            }
        }

        #[test]
        fn events_are_debug() {
            let event = ClientEvent::PlaybackStateChanged(PlaybackState::Playing);
            let debug_str = format!("{:?}", event);
            assert!(debug_str.contains("PlaybackStateChanged"));
        }
    }

    mod no_op_handler {
        use super::*;

        #[tokio::test]
        async fn handles_all_events() {
            let handler = NoOpHandler;

            // Should not panic or error on any event type
            handler
                .on_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
                .await;
            handler.on_event(ClientEvent::PositionUpdated(10.0)).await;
            handler.on_event(ClientEvent::VolumeChanged(0.5)).await;
            handler
                .on_event(ClientEvent::Error("test".to_string()))
                .await;
            handler.on_event(ClientEvent::GroupChanged).await;
        }
    }

    mod callback_handler {
        use super::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        #[tokio::test]
        async fn invokes_callback() {
            let called = Arc::new(AtomicBool::new(false));
            let called_clone = Arc::clone(&called);

            let handler = CallbackHandler::new(move |_event| {
                called_clone.store(true, Ordering::SeqCst);
            });

            handler
                .on_event(ClientEvent::PlaybackStateChanged(PlaybackState::Playing))
                .await;
            assert!(called.load(Ordering::SeqCst));
        }

        #[tokio::test]
        async fn captures_event() {
            let count = Arc::new(AtomicUsize::new(0));
            let count_clone = Arc::clone(&count);

            let handler = CallbackHandler::new(move |event| match event {
                ClientEvent::PositionUpdated(_) => {
                    count_clone.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            });

            handler.on_event(ClientEvent::PositionUpdated(1.0)).await;
            handler.on_event(ClientEvent::PositionUpdated(2.0)).await;
            handler.on_event(ClientEvent::VolumeChanged(0.5)).await;

            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    mod group_runtime_feed {
        use super::*;

        fn target_failed(n: u8) -> GroupRuntimeEvent {
            GroupRuntimeEvent::TargetSendFailed {
                receiver: DeviceId([n, 0, 0, 0, 0, n]),
                source: format!("send {n} failed"),
            }
        }

        #[tokio::test]
        async fn delivers_under_capacity_without_loss() {
            let (feed, mut rx) = GroupRuntimeEventFeed::channel(GROUP_RUNTIME_EVENT_CAPACITY);
            for n in 0..GROUP_RUNTIME_EVENT_CAPACITY {
                assert!(feed.try_publish(target_failed(n as u8)));
            }
            assert_eq!(feed.lost_events(), 0);
            let first = rx.recv().await.unwrap();
            assert_eq!(
                first,
                GroupRuntimeEvent::TargetSendFailed {
                    receiver: DeviceId([0, 0, 0, 0, 0, 0]),
                    source: "send 0 failed".into(),
                }
            );
        }

        #[tokio::test(start_paused = true)]
        async fn overflow_drops_truthfully_and_never_blocks() {
            let (feed, mut rx) = GroupRuntimeEventFeed::channel(2);
            assert!(feed.try_publish(target_failed(1)));
            assert!(feed.try_publish(target_failed(2)));
            // Channel full: events drop and are counted, never awaited.
            assert!(!feed.try_publish(target_failed(3)));
            assert_eq!(feed.lost_events(), 1);
            assert!(!feed.try_publish(target_failed(4)));
            assert_eq!(feed.lost_events(), 2);
            // Draining restores capacity; loss accounting stays immutable.
            assert!(rx.recv().await.is_some());
            assert!(feed.try_publish(target_failed(5)));
            assert_eq!(feed.lost_events(), 2);
        }
    }
}
