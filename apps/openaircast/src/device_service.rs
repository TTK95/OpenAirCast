//! Private legacy device service: owns the Tokio runtime, the discovered
//! `DeviceId -> Device` map, the current AirPlay session, keepalive timing,
//! and the legacy volume file. Exposed only through [`ControllerPort`].
//!
//! Subproject 2 replaces this adapter with `DeviceBackendHandle`/`DeviceSnapshot`
//! without changing the public `AppHandle` contract or any view/tray code.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use airplay_core::{Device, DeviceId};

use crate::app::{
    AppEffect, Availability, ControllerEvent, ControllerPort, ControllerSendError, DeviceCommand,
    ReceiverState,
};
use crate::app_handle::AppEventSender;
use crate::cast;

/// Capacity of the backend command queue.
const COMMAND_CAPACITY: usize = 64;
/// How often the service loop wakes to service keepalive timing.
const TICK: Duration = Duration::from_millis(250);
/// Legacy AirPlay feedback interval; HomePods tear sessions down without it.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// Minimum spacing between legacy `volume.txt` writes.
const VOLUME_WRITE_SPACING: Duration = Duration::from_millis(750);

/// Cheap cloneable port handle onto the dedicated controller thread.
#[derive(Clone)]
pub struct DeviceServiceHandle {
    tx: SyncSender<DeviceCommand>,
    done: Arc<AtomicBool>,
}

impl ControllerPort for DeviceServiceHandle {
    fn try_send(&self, command: DeviceCommand) -> Result<(), ControllerSendError> {
        if matches!(
            command,
            DeviceCommand::Group { .. } | DeviceCommand::PlayListeningTone { .. }
        ) {
            return Err(ControllerSendError::Closed);
        }
        self.tx.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => ControllerSendError::Busy,
            TrySendError::Disconnected(_) => ControllerSendError::Closed,
        })
    }
}

impl crate::app::DeviceShutdown for DeviceServiceHandle {
    fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        DeviceServiceHandle::shutdown_and_wait(self, timeout)
    }
}

/// The legacy device stage, as the shell's device seam.
///
/// The other arm of [`crate::app::DeviceStage`]. It is kept compiling and
/// kept reachable on purpose: a way back that has to be re-derived under
/// pressure is not a way back. Nothing calls it while
/// [`crate::app::ACTIVE_DEVICE_STAGE`] names the backend.
pub(crate) fn legacy_device_seam(
    events: AppEventSender,
    appdata_dir: PathBuf,
) -> anyhow::Result<crate::app::DeviceSeam> {
    let handle = spawn_device_service(events, appdata_dir)?;
    Ok(crate::app::DeviceSeam {
        controller: Arc::new(handle.clone()),
        shutdown: Arc::new(handle),
        // Unchanged: this is the budget the coordinator has always given the
        // legacy service, now carried by the seam instead of by a default.
        budget: crate::app::ShutdownBudgets::default().device,
    })
}

impl DeviceServiceHandle {
    /// Requests shutdown, waits up to `timeout` for the service to finish
    /// session cleanup plus the newest volume flush, and reports completion.
    pub fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.try_send(DeviceCommand::Shutdown) {
                Ok(()) | Err(ControllerSendError::Closed) => break,
                Err(ControllerSendError::Busy) => {
                    if Instant::now() >= deadline {
                        return self.done.load(Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        while Instant::now() < deadline {
            if self.done.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.done.load(Ordering::SeqCst)
    }
}

/// Starts the dedicated controller thread. Startup returns immediately;
/// discovery happens only after a `Discover` command.
pub(crate) fn spawn_device_service(
    events: AppEventSender,
    appdata_dir: PathBuf,
) -> anyhow::Result<DeviceServiceHandle> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<DeviceCommand>(COMMAND_CAPACITY);
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);

    std::thread::Builder::new()
        .name("openaircast-device-service".into())
        .spawn(move || {
            run_service(rx, events, appdata_dir);
            thread_done.store(true, Ordering::SeqCst);
        })
        .map_err(|error| anyhow::anyhow!("could not spawn device service: {error}"))?;

    Ok(DeviceServiceHandle { tx, done })
}

/// Maps controller effects onto backend commands; shell-only effects map to
/// nothing because their own ports execute them instead.
pub(crate) fn effect_to_command(effect: &AppEffect) -> Option<DeviceCommand> {
    match effect {
        AppEffect::PlayListeningTone { generation } => Some(DeviceCommand::PlayListeningTone {
            generation: *generation,
        }),
        AppEffect::Group {
            request,
            generation,
            command,
        } => Some(DeviceCommand::Group {
            request: *request,
            generation: *generation,
            command: command.clone(),
        }),
        AppEffect::Discover { generation } => Some(DeviceCommand::Discover {
            generation: *generation,
        }),
        AppEffect::StartSession {
            generation,
            receiver_ids,
            volume,
        } => Some(DeviceCommand::StartSession {
            generation: *generation,
            receiver_ids: receiver_ids.clone(),
            volume: *volume,
        }),
        AppEffect::StopSession { generation } => Some(DeviceCommand::StopSession {
            generation: *generation,
        }),
        AppEffect::ApplyVolume { generation, volume } => Some(DeviceCommand::ApplyVolume {
            generation: *generation,
            volume: *volume,
        }),
        AppEffect::ApplyMute(muted) => Some(DeviceCommand::ApplyMute(*muted)),
        AppEffect::ApplyReceiverLevel { receiver, level } => {
            Some(DeviceCommand::ApplyReceiverLevel {
                receiver: receiver.clone(),
                level: *level,
            })
        }
        AppEffect::ApplyLatency(choice) => Some(DeviceCommand::ApplyLatency(*choice)),
        AppEffect::ApplyAudioEndpoint(request) => Some(DeviceCommand::ApplyAudioEndpoint(*request)),
        _ => None,
    }
}

/// Resolves requested IDs against the private device map by stable identity:
/// unknown entries are ignored, survivors keep the caller's order even after
/// discovery reordered its results.
pub(crate) fn resolve_devices(
    devices: &HashMap<DeviceId, Device>,
    receiver_ids: &[DeviceId],
) -> Vec<Device> {
    receiver_ids
        .iter()
        .filter_map(|id| devices.get(id).cloned())
        .collect()
}

/// Projects devices into presentation-safe receiver records, deterministically
/// ordered by ID. Addresses never cross this boundary.
pub(crate) fn receiver_records(devices: &[Device]) -> Vec<ReceiverState> {
    let mut records: Vec<ReceiverState> = devices
        .iter()
        .map(|device| ReceiverState {
            id: device.id.clone(),
            name: device.name.clone(),
            model: device.model.clone(),
            availability: Availability::Available,
        })
        .collect();
    records.sort_by_key(|record| record.id.0);
    records
}

/// The volume file *this* service writes: `%APPDATA%\volume.txt`.
///
/// Named rather than spelled into the store below because a second reader
/// exists now. The backend migrates the legacy volume once and then owns it,
/// so its migration source and this path must not be able to drift apart --
/// they did, and the drift cost every user their saved volume on the switch.
///
/// This is *not* the only volume file in the wild: every build up to
/// `33521c2` wrote `%APPDATA%\HomePodCast\volume.txt` from `cast.rs`, and the
/// refactor that created this module moved the file up one directory without
/// migrating it. See [`crate::backend_bridge::legacy_volume_source`], which
/// has to consider both.
pub(crate) fn legacy_volume_file(appdata_dir: &Path) -> PathBuf {
    appdata_dir.join("volume.txt")
}

/// Owns the legacy volume file with the exact legacy format so an upgrade
/// keeps the previous default-volume behaviour.
struct LegacyDevicePreferences {
    file: PathBuf,
}

impl LegacyDevicePreferences {
    fn new(appdata_dir: &Path) -> Self {
        Self {
            file: legacy_volume_file(appdata_dir),
        }
    }

    fn load(&self) -> f32 {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|raw| raw.trim().parse::<f32>().ok())
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(cast::DEFAULT_VOLUME)
    }

    fn save(&self, value: f32) {
        if let Some(parent) = self.file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.file, format!("{:.3}", value.clamp(0.0, 1.0)));
    }
}

fn report(events: &AppEventSender, event: ControllerEvent) {
    if let Err(error) = events.try_send(crate::app::AppEvent::Controller(event)) {
        tracing::warn!("could not report controller result: {error}");
    }
}

fn run_service(commands: Receiver<DeviceCommand>, events: AppEventSender, appdata_dir: PathBuf) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        report(
            &events,
            ControllerEvent::ChannelClosed {
                summary: "the audio controller could not start".into(),
            },
        );
        return;
    };

    let mut devices: HashMap<DeviceId, Device> = HashMap::new();
    let mut session: Option<cast::Session> = None;
    let prefs = LegacyDevicePreferences::new(&appdata_dir);
    let mut volume = prefs.load();
    let mut last_feedback = Instant::now();
    let mut last_volume_write = Instant::now() - VOLUME_WRITE_SPACING;
    let mut volume_dirty = false;
    let mut deferred: VecDeque<DeviceCommand> = VecDeque::new();

    loop {
        // Keepalive first so a busy command stream never starves the session.
        if let Some(active) = session.as_mut() {
            if last_feedback.elapsed() >= KEEPALIVE_INTERVAL {
                runtime.block_on(active.feedback());
                last_feedback = Instant::now();
            }
        }

        if volume_dirty && last_volume_write.elapsed() >= VOLUME_WRITE_SPACING {
            prefs.save(volume);
            volume_dirty = false;
            last_volume_write = Instant::now();
        }

        let command = if let Some(command) = deferred.pop_front() {
            command
        } else {
            match commands.recv_timeout(TICK) {
                Ok(command) => command,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };

        match command {
            DeviceCommand::Group { .. } | DeviceCommand::PlayListeningTone { .. } => {
                unreachable!("legacy port refuses template and listening-test commands")
            }
            DeviceCommand::Discover { generation } => {
                match runtime.block_on(cast::discover(Duration::from_secs(3))) {
                    Ok(found) => {
                        devices = found.into_iter().map(|d| (d.id.clone(), d)).collect();
                        let snapshot: Vec<Device> = devices.values().cloned().collect();
                        report(
                            &events,
                            ControllerEvent::DiscoveryCompleted {
                                generation,
                                receivers: receiver_records(&snapshot),
                            },
                        );
                    }
                    Err(error) => report(
                        &events,
                        ControllerEvent::DiscoveryFailed {
                            generation,
                            summary: format!("{error:#}"),
                        },
                    ),
                }
            }
            DeviceCommand::StartSession {
                generation,
                receiver_ids,
                volume: start_volume,
            } => {
                volume = start_volume.clamp(0.0, 1.0);
                let selected = resolve_devices(&devices, &receiver_ids);
                if selected.is_empty() {
                    report(
                        &events,
                        ControllerEvent::SessionFailed {
                            generation,
                            summary: "none of the selected receivers was found on the network"
                                .into(),
                        },
                    );
                } else {
                    stop_session(&runtime, &mut session);
                    match runtime.block_on(cast::Session::start(selected, volume)) {
                        Ok(new_session) => {
                            let active_receiver_ids = resolve_devices(&devices, &receiver_ids)
                                .iter()
                                .map(|device| device.id.clone())
                                .collect();
                            session = Some(new_session);
                            last_feedback = Instant::now();
                            report(
                                &events,
                                ControllerEvent::SessionStarted {
                                    generation,
                                    active_receiver_ids,
                                },
                            );
                        }
                        Err(error) => report(
                            &events,
                            ControllerEvent::SessionFailed {
                                generation,
                                summary: format!("{error:#}"),
                            },
                        ),
                    }
                }
            }
            DeviceCommand::StopSession { generation } => {
                stop_session(&runtime, &mut session);
                report(&events, ControllerEvent::SessionStopped { generation });
            }
            DeviceCommand::ApplyVolume {
                generation,
                volume: new_volume,
            } => {
                let mut latest = (generation, new_volume.clamp(0.0, 1.0));
                // Collapse a queued slider burst into one backend call while
                // keeping every other command pending in arrival order.
                loop {
                    match commands.try_recv() {
                        Ok(DeviceCommand::ApplyVolume { generation, volume }) => {
                            latest = (generation, volume.clamp(0.0, 1.0))
                        }
                        Ok(other) => deferred.push_back(other),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                volume = latest.1;
                volume_dirty = true;
                if let Some(active) = session.as_mut() {
                    runtime.block_on(active.set_volume(volume));
                }
                report(
                    &events,
                    ControllerEvent::VolumeApplied {
                        generation: latest.0,
                        volume,
                    },
                );
            }
            // The legacy stage has no mute, no latency profile, and no
            // endpoint selection, and -- decisively -- publishes no reading of
            // any of the three. The Audio destination therefore draws no
            // control for them on this stage at all, so these arms are
            // unreachable from the window rather than merely unimplemented.
            // They are written out because the command vocabulary is shared
            // with the backend stage, and a wildcard here would swallow a
            // fourth capability that this service really should refuse
            // loudly.
            DeviceCommand::ApplyMute(_)
            | DeviceCommand::ApplyReceiverLevel { .. }
            | DeviceCommand::ApplyLatency(_)
            | DeviceCommand::ApplyAudioEndpoint(_) => {
                tracing::warn!("the legacy device stage offers no audio settings; ignored");
            }
            DeviceCommand::Shutdown => break,
        }
    }

    stop_session(&runtime, &mut session);
    if volume_dirty {
        prefs.save(volume);
    }
    // The session above was stopped and awaited, so the work this service owns
    // is finished. What can remain is a `spawn_blocking` task the AirPlay
    // streamer abandons when its sender thread outlives the join budget, and a
    // blocking task cannot be aborted -- a full runtime drop would block this
    // thread forever and the shell's shutdown coordinator would report the
    // device service as never done. Releasing the runtime into the background
    // returns immediately, without a deadline and without abandoning anything
    // the caller is waiting for, so this function returns and its thread ends
    // naturally.
    runtime.shutdown_background();
}

fn stop_session(runtime: &tokio::runtime::Runtime, session: &mut Option<cast::Session>) {
    if let Some(active) = session.take() {
        runtime.block_on(active.stop());
        tracing::info!("stopped streaming session");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    use airplay_core::{Device, DeviceId, Features, Version};

    use super::{
        effect_to_command, receiver_records, resolve_devices, DeviceServiceHandle,
        LegacyDevicePreferences,
    };
    use crate::app::{
        AppEffect, Availability, ControllerPort, ControllerSendError, DeviceCommand, GenerationId,
        ReceiverState,
    };
    use crate::app_handle::AppEventSender;

    fn rid(last: u8) -> DeviceId {
        DeviceId([0, 0, 0, 0, 0, last])
    }

    fn device(last: u8, name: &str, model: &str) -> Device {
        Device {
            id: rid(last),
            name: name.into(),
            model: model.into(),
            manufacturer: None,
            serial_number: None,
            addresses: Vec::new(),
            port: 0,
            features: Features::default(),
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

    #[test]
    fn every_controller_effect_maps_to_exactly_one_device_command() {
        assert_eq!(
            effect_to_command(&AppEffect::Discover {
                generation: GenerationId(4),
            }),
            Some(DeviceCommand::Discover {
                generation: GenerationId(4),
            })
        );

        let start = AppEffect::StartSession {
            generation: GenerationId(5),
            receiver_ids: vec![rid(2), rid(1)],
            volume: 0.4,
        };
        assert_eq!(
            effect_to_command(&start),
            Some(DeviceCommand::StartSession {
                generation: GenerationId(5),
                receiver_ids: vec![rid(2), rid(1)],
                volume: 0.4,
            })
        );

        assert_eq!(
            effect_to_command(&AppEffect::StopSession {
                generation: GenerationId(6),
            }),
            Some(DeviceCommand::StopSession {
                generation: GenerationId(6),
            })
        );
        assert_eq!(
            effect_to_command(&AppEffect::ApplyVolume {
                generation: GenerationId(7),
                volume: 0.9,
            }),
            Some(DeviceCommand::ApplyVolume {
                generation: GenerationId(7),
                volume: 0.9,
            })
        );
    }

    #[test]
    fn shell_only_effects_never_reach_the_device_backend() {
        for effect in [
            AppEffect::PersistPreferences {
                generation: GenerationId(1),
                value: Default::default(),
            },
            AppEffect::ReconfigureHotkey {
                generation: GenerationId(2),
                value: Default::default(),
            },
            AppEffect::ShowMainWindow,
            AppEffect::HideMainWindow,
            AppEffect::BeginShutdown,
        ] {
            assert!(effect_to_command(&effect).is_none());
        }
    }

    #[test]
    fn resolver_returns_selected_devices_in_caller_order_ignoring_unknown_ids() {
        let map: HashMap<DeviceId, Device> = [
            device(3, "Zeta", "HomePod"),
            device(1, "Alpha", "HomePod mini"),
            device(2, "Muted", "HomePod"),
        ]
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect();

        let resolved = resolve_devices(&map, &[rid(3), rid(99), rid(1)]);

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].name, "Zeta");
        assert_eq!(
            resolved[1].name, "Alpha",
            "caller order wins over map order"
        );
        assert_eq!(resolve_devices(&map, &[rid(99)]).len(), 0);
    }

    #[test]
    fn receiver_records_expose_only_presentation_safe_fields_sorted_by_id() {
        let devices = [device(3, "Zeta", "HomePod"), device(1, "Alpha", "HomePod")];

        let records = receiver_records(&devices);

        assert_eq!(
            records,
            vec![
                ReceiverState {
                    id: rid(1),
                    name: "Alpha".into(),
                    model: "HomePod".into(),
                    availability: Availability::Available,
                },
                ReceiverState {
                    id: rid(3),
                    name: "Zeta".into(),
                    model: "HomePod".into(),
                    availability: Availability::Available,
                },
            ]
        );
    }

    #[test]
    fn legacy_volume_store_round_trips_with_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = LegacyDevicePreferences::new(dir.path());

        assert_eq!(store.load(), 0.25, "missing file falls back to default");

        store.save(0.75);
        assert_eq!(store.load(), 0.75);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("volume.txt")).unwrap(),
            "0.750",
            "exact legacy serialization is preserved"
        );
    }

    /// The two ends of the volume migration, asserted against each other.
    ///
    /// They drifted once: this store wrote `%APPDATA%\volume.txt` while the
    /// backend's migration source named `%APPDATA%\HomePodCast\volume.txt`,
    /// and nothing failed -- the migration simply found no file and every
    /// user's saved volume was replaced by the default on the switch. Writing
    /// through this store and reading through the backend's chosen source is
    /// the only assertion that cannot be satisfied by two paths that merely
    /// look alike.
    #[test]
    fn the_backend_migration_reads_the_file_this_service_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        LegacyDevicePreferences::new(dir.path()).save(0.42);

        let source = crate::backend_bridge::legacy_volume_source(dir.path());

        assert_eq!(
            std::fs::read_to_string(&source).ok(),
            Some("0.420".to_owned()),
            "the backend migrates from {source:?}, which is not where this \
             service wrote the volume"
        );
    }

    #[test]
    fn handle_reports_busy_and_closed_as_typed_errors_without_blocking() {
        let (tx, rx) = mpsc::sync_channel::<DeviceCommand>(64);
        let handle = DeviceServiceHandle {
            tx,
            done: Arc::new(AtomicBool::new(false)),
        };

        for generation in 0..64u64 {
            assert!(handle
                .try_send(DeviceCommand::Discover {
                    generation: GenerationId(generation),
                })
                .is_ok());
        }
        assert_eq!(
            handle.try_send(DeviceCommand::Discover {
                generation: GenerationId(999),
            }),
            Err(ControllerSendError::Busy)
        );

        drop(rx);
        assert_eq!(
            handle.try_send(DeviceCommand::Shutdown),
            Err(ControllerSendError::Closed)
        );
    }

    #[test]
    fn spawned_service_shuts_down_promptly_without_touching_the_network() {
        let (events_tx, events_rx) = mpsc::sync_channel(16);
        let sender = AppEventSender::new(events_tx);
        let dir = tempfile::TempDir::new().unwrap();
        let handle = super::spawn_device_service(sender, dir.path().to_path_buf()).unwrap();

        assert!(handle.shutdown_and_wait(Duration::from_secs(2)));

        drop(handle);
        let no_event = events_rx.recv_timeout(Duration::from_millis(50)).is_err();
        assert!(no_event);
    }
}
