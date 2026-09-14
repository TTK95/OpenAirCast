use std::collections::BTreeMap;

use airplay_core::DeviceId;
use homepod_cast::backend::event::{BackendEvent, NoticeCode as BackendNoticeCode};
use homepod_cast::backend::model::{
    AudioEndpoint, AudioEndpointPreference, AudioSourceSnapshot, AudioSourceState,
    DiscoverySnapshot, LatencyPreset, LatencyPresetOption, ReceiverSnapshot, SessionSnapshot,
};
use homepod_cast::{
    DeviceSnapshot, DiagnosticsSnapshot, DiscoveryPhase, Health, ReceiverDiagnosticsSnapshot,
    ReceiverId, ReceiverLifecycle, RunIntent, SessionPhase,
};

use crate::app::{
    AudioEndpointChoice, AudioEndpointKey, AudioEndpointSelection, AudioSourceReading,
    Availability, CaptureState, ControllerEvent, DiagnosticsHealth, DiagnosticsReading,
    GenerationId, LatencyChoice, LatencyOption, LatencyReading, LatencyUnavailable, ReceiverState,
};

use super::commands::{GenerationsSeen, SessionEdge};

/// Joins only current-session numeric observations to the known receiver inventory.
/// No backend text, endpoint identity, absolute clock or protocol object crosses.

pub(super) fn project_live_diagnostics(
    backend: &DeviceSnapshot,
    registry: Option<&DiagnosticsSnapshot>,
    generation: GenerationId,
    registration: Option<(u64, homepod_cast::SessionId)>,
) -> crate::app::LiveDiagnosticsReading {
    use crate::app::{
        BufferDiagnosticsReading, DiagnosticReceiverState as S, LiveDiagnosticsReading,
        ReceiverDiagnosticsReading, TransportDiagnosticsReading,
    };
    let current = registry.filter(|registry| {
        backend.run_intent == RunIntent::Running && registry.session.as_ref().is_some_and(|session| {
            registry.active_session_id == Some(session.session_id)
                && session.finished_elapsed_ns.is_none()
                && matches!((&registration, &backend.session.phase),
                    (Some((a, id)), SessionPhase::Streaming { generation: b } | SessionPhase::Degraded { generation: b })
                    if a == b && *id == session.session_id)
        })
    });
    let buffer = current
        .and_then(|r| r.session.as_ref())
        .and_then(|s| s.audio.as_ref())
        .filter(|audio| audio.sender_buffer.capacity_frames > 0)
        .map(|audio| BufferDiagnosticsReading {
            queued_frames: audio.sender_buffer.queued_frames,
            capacity_frames: audio.sender_buffer.capacity_frames,
            buffered_ms: audio.sender_buffer.buffered_ns / 1_000_000,
            underruns: audio.sender_buffer.underrun_events_total,
        });
    let receivers = backend
        .receivers
        .iter()
        .map(|receiver| {
            let measured = current.and_then(|r| {
                r.receivers.iter().find(|row| {
                    Some(row.key.session_id) == r.active_session_id
                        && row.key.receiver_id == receiver.id
                        && r.session.as_ref().is_some_and(|s| {
                            s.members.get(&receiver.id) == Some(&DeviceId::from(receiver.id))
                        })
                })
            });
            // A registry lifecycle row can exist before any client observation.
            let observed = measured.filter(|row| {
                row.timing.source != homepod_cast::ReceiverTimingSource::Unavailable
                    || row.transport.data_datagrams_attempted_total > 0
                    || row.feedback.attempts_total > 0
            });
            ReceiverDiagnosticsReading {
                id: DeviceId::from(receiver.id),
                state: match receiver.lifecycle {
                    ReceiverLifecycle::Discovered | ReceiverLifecycle::Ready { .. } => S::Ready,
                    ReceiverLifecycle::Connecting { .. } | ReceiverLifecycle::SettingUp { .. } => {
                        S::Connecting
                    }
                    ReceiverLifecycle::Streaming { .. } => S::Streaming,
                    ReceiverLifecycle::RetryWaiting { .. } => S::Recovering,
                    ReceiverLifecycle::Unavailable => S::Offline,
                    ReceiverLifecycle::Failed { .. } => S::Failed,
                },
                transport: observed.map(|row| TransportDiagnosticsReading {
                    packets_accepted: row.transport.data_datagrams_accepted_local_total,
                    bytes_accepted: row.transport.data_bytes_accepted_local_total,
                    send_failures: row.transport.data_datagram_send_failures_total,
                    retransmit_requests: row.transport.retransmit_slots_requested_total,
                }),
                issue: measured
                    .and_then(|row| row.last_error.as_ref())
                    .map(|error| diagnostic_issue(error.code)),
            }
        })
        .collect();
    LiveDiagnosticsReading {
        generation,
        input_peak_per_mille: match backend.audio_source.state {
            AudioSourceState::Capturing | AudioSourceState::SilentSystem => {
                backend.audio_source.windows_input_peak_permille
            }
            _ => None,
        },
        capture_drops_total: backend.audio_source.pcm_frames_dropped_total,
        buffer,
        receivers,
    }
}

fn diagnostic_issue(code: homepod_cast::DiagnosticErrorCode) -> crate::app::DiagnosticIssue {
    use crate::app::DiagnosticIssue as I;
    use homepod_cast::DiagnosticErrorCode as C;
    match code {
        C::PairingFailure => I::Pairing,
        C::SetupRejected | C::SetupTimeout => I::Setup,
        C::CaptureFailure | C::CaptureQueueFull | C::BufferUnderrun | C::EncodeFailure => {
            I::Capture
        }
        C::PtpBindFallback | C::PtpSampleStale => I::Timing,
        C::SenderQueueDisconnected | C::UdpSendFailure | C::RetransmitHistoryMiss => I::Transport,
        C::FeedbackFailure | C::FeedbackTimeout => I::Feedback,
        C::TeardownTimeout => I::Teardown,
        C::DiscoveryFailure
        | C::EventChannelFailure
        | C::CalibrationApplyFailure
        | C::ExportFailure
        | C::Notice => I::Other,
    }
}

/// What the bridge last told the reducer, so a pass emits only differences.
///
/// Level-driven rather than transactional: each pass re-derives the whole
/// answer from the newest snapshot and compares it with this. An event lost
/// to a full shell queue is therefore repaired by the next revision instead
/// of leaving the window permanently out of step with the backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Projection {
    master_volume: Option<f32>,
    desired: Option<(u64, Vec<DeviceId>)>,
    groups: Option<Vec<crate::app::SavedGroupReading>>,
    /// Newest discovery answer published, with the generation it was
    /// published under. `None` until the daemon has produced one.
    discovery: Option<(GenerationId, DiscoveryEcho)>,
    /// Newest session answer published, with its shell generation.
    pub(super) session: Option<(GenerationId, SessionEcho)>,
    /// Newest mute answer published. `None` until the first pass.
    mute: Option<bool>,
    /// Last durable per-receiver balances published to the shell.
    receiver_levels: Option<BTreeMap<DeviceId, f32>>,
    /// Newest latency answer published.
    latency: Option<LatencyReading>,
    /// Newest capture-source answer published.
    audio: Option<AudioSourceReading>,
    /// Which opaque key stands for which Windows endpoint ID.
    ///
    /// Part of the projection rather than a side table because `project` is
    /// pure: a pass that meets a new endpoint returns a table with one more
    /// entry, and the caller decides what to do with it. The bridge thread
    /// publishes it into the shared [`EndpointDirectory`] the command half
    /// reads.
    pub(super) endpoints: EndpointTable,
}

/// The allocation of opaque keys to raw Windows endpoint IDs.
///
/// Two rules, and both are safety rather than tidiness:
///
/// * **A key is a counter.** It is not a truncation of the ID and not a hash
///   of it, so no argument about irreversibility is needed: there is nothing
///   in the key derived from the ID at all.
/// * **An entry is never removed and a key is never reused.** Endpoints come
///   and go -- a headset is unplugged, a monitor sleeps -- and a key that
///   came to mean a *different* device would turn a stale click into the
///   silent selection of the wrong microphone. Retiring an entry instead
///   would reintroduce the same race between the frame that painted a key and
///   the command that carries it. The table therefore grows by one per
///   distinct endpoint ID seen in the process lifetime, which on any real
///   machine is a handful.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EndpointTable {
    /// Next key to hand out. Monotonic for the life of the process.
    next: u64,
    /// Every endpoint ID this run has ever offered, in allocation order,
    /// with the newest display name seen for it.
    known: Vec<EndpointEntry>,
}

#[derive(Clone, PartialEq)]
struct EndpointEntry {
    key: AudioEndpointKey,
    /// The raw Windows endpoint ID. This field is the reason this type is
    /// private to the bridge.
    id: String,
    name: String,
}

/// Written by hand so the raw endpoint ID is not printable.
///
/// [`Projection`] is `Debug` and holds the table; one `tracing::debug!` line
/// in this file naming it would otherwise put every `{0.0.0.00000000}.{guid}`
/// on the machine into a log, with no compile error and no test to catch it,
/// because the shell-side canaries only ever read what the *window* says.
/// Redacting at the only place the ID lives closes that door for every future
/// caller instead of asking each one to remember.
impl std::fmt::Debug for EndpointEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EndpointEntry")
            .field("key", &self.key)
            .field("id", &"<redacted>")
            .field("name", &self.name)
            .finish()
    }
}

impl EndpointTable {
    /// The key for `endpoint`, allocating one the first time it is seen and
    /// refreshing the display name every time after.
    fn key_for(&mut self, endpoint: &AudioEndpoint) -> AudioEndpointKey {
        if let Some(entry) = self.known.iter_mut().find(|entry| entry.id == endpoint.id) {
            entry.name.clone_from(&endpoint.name);
            return entry.key;
        }

        let key = AudioEndpointKey(self.next);
        self.next += 1;
        self.known.push(EndpointEntry {
            key,
            id: endpoint.id.clone(),
            name: endpoint.name.clone(),
        });
        key
    }

    /// The backend preference one key stands for, or `None` for a key this
    /// process never allocated.
    ///
    /// `None` is the only safe answer to an unrecognised key, and the command
    /// half turns it into no command at all rather than into a guess.
    fn preference(&self, key: AudioEndpointKey) -> Option<AudioEndpointPreference> {
        self.known
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| AudioEndpointPreference::Explicit {
                id: entry.id.clone(),
                last_known_name: entry.name.clone(),
            })
    }
}

/// The key table as the command half sees it.
///
/// One `Mutex` around a table the bridge thread writes after each projection
/// pass and the shell thread reads when it sends a selection. No lock is ever
/// held across an `await`: the publish is a single synchronous statement in
/// the bridge loop, and the read happens on the shell thread, which has no
/// runtime at all.
#[derive(Debug, Default)]
pub(crate) struct EndpointDirectory {
    table: std::sync::Mutex<EndpointTable>,
}

impl EndpointDirectory {
    pub(super) fn publish(&self, table: &EndpointTable) {
        let mut guard = self
            .table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard != *table {
            guard.clone_from(table);
        }
    }

    pub(super) fn preference(&self, key: AudioEndpointKey) -> Option<AudioEndpointPreference> {
        self.table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .preference(key)
    }
}

/// The shell-visible content of one discovery answer.
#[derive(Clone, Debug, PartialEq)]
enum DiscoveryEcho {
    Inventory(Vec<ReceiverState>),
    Failed(String),
}

/// The shell-visible content of one session answer.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum SessionEcho {
    Stopped,
    Active(Vec<DeviceId>),
    /// Running, delivering to the receivers named here, but not to every one
    /// that was asked for -- or without a capture source.
    Degraded(Vec<DeviceId>),
    /// Running, with the whole group being rebuilt by the controller. Carries
    /// no membership because during the rebuild there is none.
    Restarting,
    Failed(String),
}

impl Projection {
    /// Derives the shell events one backend snapshot implies.
    ///
    /// Pure and total: no clock, no I/O, no channel. Every field of
    /// [`DeviceSnapshot`] is destructured by name without a `..` rest
    /// pattern, so a new backend field is a compile error here until someone
    /// decides in this file what the shell may learn about it.
    pub(crate) fn project(
        &self,
        snapshot: &DeviceSnapshot,
        seen: GenerationsSeen,
    ) -> (Self, Vec<ControllerEvent>) {
        let DeviceSnapshot {
            // The backend's own snapshot counter; the shell has its own.
            revision: _,
            desired_revision,
            discovery,
            receivers,
            saved_groups,
            desired_members,
            // Correlated separately by GenerationEcho after this state projection.
            command_outcomes: _,
            // The shell derives running/stopped from the session phase, which
            // is the settled fact rather than the wish.
            run_intent: _,
            session,
            // Forwarded, but never verbatim: this is where the raw Windows
            // endpoint IDs live. `audio_echo` hands out opaque keys for them
            // and keeps every ID inside this file.
            audio_source,
            master_volume,
            receiver_levels,
            muted,
            latency_preset,
            latency_presets,
            // No shell vocabulary yet: auto-connect.
            auto_connect: _,
            // No shell vocabulary yet: device-state persistence health. Not
            // to be confused with the settings file, which the shell already
            // reports through `PreferencesEvent`.
            persistence: _,
        } = snapshot;

        let mut next = self.clone();
        let mut events = Vec::new();
        if next.master_volume != Some(master_volume.get()) {
            next.master_volume = Some(master_volume.get());
            events.push(ControllerEvent::VolumeApplied {
                generation: GenerationId(0),
                volume: master_volume.get(),
            });
        }

        let desired = (
            *desired_revision,
            desired_members
                .iter()
                .map(|id| DeviceId::from(*id))
                .collect::<Vec<_>>(),
        );
        if next.desired.as_ref() != Some(&desired) {
            events.push(ControllerEvent::DesiredReceiversChanged {
                desired_revision: desired.0,
                receiver_ids: desired.1.clone(),
            });
            next.desired = Some(desired);
        }
        let groups = saved_groups
            .iter()
            .map(|group| crate::app::SavedGroupReading {
                id: crate::app::GroupId(group.id.as_uuid()),
                name: group.name.clone(),
                members: group
                    .members
                    .iter()
                    .map(|member| crate::app::GroupMember {
                        receiver: DeviceId::from(member.receiver),
                        name: member.last_known_name.clone(),
                        level: member.level.get(),
                    })
                    .collect(),
                available_members: group
                    .members
                    .iter()
                    .filter(|member| {
                        receivers.iter().any(|row| {
                            row.id == member.receiver
                                && availability_of(&row.lifecycle) == Availability::Available
                        })
                    })
                    .map(|member| DeviceId::from(member.receiver))
                    .collect(),
            })
            .collect::<Vec<_>>();
        if next.groups.as_ref() != Some(&groups) {
            events.push(ControllerEvent::SavedGroupsChanged {
                groups: groups.clone(),
            });
            next.groups = Some(groups);
        }

        if let Some(echo) = discovery_echo(discovery, receivers) {
            let stamped = (seen.discovery, echo);
            if next.discovery.as_ref() != Some(&stamped) {
                events.push(match &stamped.1 {
                    DiscoveryEcho::Inventory(receivers) => ControllerEvent::DiscoveryCompleted {
                        generation: stamped.0,
                        receivers: receivers.clone(),
                    },
                    DiscoveryEcho::Failed(summary) => ControllerEvent::DiscoveryFailed {
                        generation: stamped.0,
                        summary: summary.clone(),
                    },
                });
                next.discovery = Some(stamped);
            }
        }

        if let Some(echo) = session_echo(session) {
            // Deliberately *not* the discovery rule. A raised discovery
            // generation is always answered from the current inventory; a
            // raised session generation is answered from an unchanged
            // snapshot only when that snapshot already says what the shell
            // asked for. See `SessionEdge::answered_by` for why copying
            // the discovery rule here would make every Start and Stop press
            // visibly snap back.
            // A predecessor can change phase after the shell requested a new
            // generation. Changed content is not proof that it answers that
            // newer request. Group starts also wait for their exact receipt,
            // so an old Active/Failed snapshot cannot settle them prematurely.
            let publish = if let SessionEdge::ConfirmedGroupStart { backend_floor } =
                seen.session_edge
            {
                let backend_generation = match session.phase {
                    homepod_cast::backend::model::SessionPhase::Starting { generation }
                    | homepod_cast::backend::model::SessionPhase::Streaming { generation }
                    | homepod_cast::backend::model::SessionPhase::Degraded { generation }
                    | homepod_cast::backend::model::SessionPhase::Restarting {
                        generation, ..
                    }
                    | homepod_cast::backend::model::SessionPhase::Stopping { generation }
                    | homepod_cast::backend::model::SessionPhase::Failed { generation, .. } => {
                        Some(generation)
                    }
                    homepod_cast::backend::model::SessionPhase::Stopped => None,
                };
                let settled = next
                    .session
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == seen.session);
                // Recovering is a real outcome too (empty castable set or
                // preflight failure), and keeps Cancel available. Once settled,
                // spontaneous stop/health updates retain their usual semantics.
                backend_generation.map_or(settled, |generation| generation >= backend_floor)
                    && next.session.as_ref() != Some(&(seen.session, echo.clone()))
            } else if seen.session_edge == SessionEdge::PendingGroupStart {
                false
            } else {
                match next.session.as_ref() {
                    None if seen.session_edge != SessionEdge::Untouched => {
                        seen.session_edge.answered_by(&echo)
                    }
                    None => true,
                    Some((generation, _)) if *generation != seen.session => {
                        seen.session_edge.answered_by(&echo)
                    }
                    Some((_, published)) => published != &echo,
                }
            };
            let stamped = (seen.session, echo);
            if publish {
                events.push(match &stamped.1 {
                    SessionEcho::Stopped => ControllerEvent::SessionStopped {
                        generation: stamped.0,
                    },
                    SessionEcho::Active(active) => ControllerEvent::SessionStarted {
                        generation: stamped.0,
                        active_receiver_ids: active.clone(),
                    },
                    SessionEcho::Degraded(active) => ControllerEvent::SessionDegraded {
                        generation: stamped.0,
                        active_receiver_ids: active.clone(),
                    },
                    SessionEcho::Restarting => ControllerEvent::SessionRestarting {
                        generation: stamped.0,
                    },
                    SessionEcho::Failed(summary) => ControllerEvent::SessionFailed {
                        generation: stamped.0,
                        summary: summary.clone(),
                    },
                });
                next.session = Some(stamped);
            }
        }

        if next.mute != Some(*muted) {
            next.mute = Some(*muted);
            events.push(ControllerEvent::MuteChanged { muted: *muted });
        }

        let levels = receiver_levels
            .iter()
            .map(|(receiver, level)| (DeviceId::from(*receiver), level.get()))
            .collect::<BTreeMap<_, _>>();
        if next.receiver_levels.as_ref() != Some(&levels) {
            next.receiver_levels = Some(levels.clone());
            events.push(ControllerEvent::ReceiverLevelsChanged { levels });
        }

        let latency = latency_echo(*latency_preset, latency_presets);
        if next.latency.as_ref() != Some(&latency) {
            next.latency = Some(latency.clone());
            events.push(ControllerEvent::LatencyChanged { reading: latency });
        }

        let audio = audio_echo(audio_source, &mut next.endpoints);
        if next.audio.as_ref() != Some(&audio) {
            next.audio = Some(audio.clone());
            events.push(ControllerEvent::AudioSourceChanged { reading: audio });
        }

        (next, events)
    }
}

/// The shell's word for one backend latency profile.
///
/// Written out without a rest pattern so a fourth backend profile stops this
/// file compiling until someone decides here whether the window may offer it.
fn latency_choice(preset: LatencyPreset) -> LatencyChoice {
    match preset {
        LatencyPreset::Low => LatencyChoice::Low,
        LatencyPreset::Normal => LatencyChoice::Normal,
        LatencyPreset::Stable => LatencyChoice::Stable,
    }
}

/// What one latency offer means to the shell.
///
/// The backend states its refusal as a `UserFacingError`, which is a newtype
/// over free-form text: it has no variants to match, and its sentence is
/// composed at the point of failure. So the text is not narrowed here -- it
/// is not read at all. The bridge learns only *whether* a profile is refused
/// and substitutes the shell's own code, exactly as `DiscoveryFailed` is
/// turned into `DISCOVERY_FAILED_SUMMARY` on the presentation side. The
/// `Option` is matched without a wildcard, so if `UserFacingError` ever
/// becomes an enum whose cases the window should tell apart, this is the arm
/// that has to grow.
fn latency_echo(selected: LatencyPreset, offered: &[LatencyPresetOption]) -> LatencyReading {
    LatencyReading {
        selected: latency_choice(selected),
        options: offered
            .iter()
            .map(|option| LatencyOption {
                choice: latency_choice(option.preset),
                unavailable: latency_unavailable(option),
            })
            .collect(),
    }
}

/// Whether one offered profile is refused, as a shell code.
///
/// The offer is destructured by name without a rest pattern, so a second
/// reason the backend might one day attach to a profile is a compile error
/// here rather than a fact the window silently drops. The refusal itself is
/// bound and never read: it is free-form text composed at the point of
/// failure, and the catalog that owns every visible string in this shell
/// holds `&'static str` only.
fn latency_unavailable(option: &LatencyPresetOption) -> Option<LatencyUnavailable> {
    let LatencyPresetOption {
        preset: _,
        disabled_reason,
    } = option;

    disabled_reason
        .as_ref()
        .map(|_refused| LatencyUnavailable::NotValidated)
}

/// The shell's word for what capture is doing.
fn capture_state(state: AudioSourceState) -> CaptureState {
    match state {
        AudioSourceState::Capturing => CaptureState::Capturing,
        AudioSourceState::SilentSystem => CaptureState::SilentSystem,
        AudioSourceState::Recovering => CaptureState::Recovering,
        AudioSourceState::Unavailable => CaptureState::Unavailable,
        AudioSourceState::Failed => CaptureState::Failed,
    }
}

/// What one capture-source snapshot means to the shell.
///
/// The one function in the shell that ever sees a Windows endpoint ID, and it
/// keeps every one of them: the offered endpoints cross as
/// `(opaque key, display name)` pairs, and the stored preference crosses as
/// the key of the endpoint it names *while that endpoint is on offer* and as
/// a bare saved name otherwise. That second case is paragraph 7.4's
/// "unfulfillable preference", and it is read off the snapshot rather than
/// guessed: the ID is either in the offered list or it is not.
///
/// Display names are carried. A name is what Windows itself shows the user in
/// its own volume mixer, it is the same class of datum as the receiver names
/// that already cross this boundary, and without it the list would be a
/// column of numbers nobody could choose from.
pub(super) fn audio_echo(
    source: &AudioSourceSnapshot,
    table: &mut EndpointTable,
) -> AudioSourceReading {
    let AudioSourceSnapshot {
        windows_input_peak_permille: _,
        pcm_frames_dropped_total: _,
        refresh_failed,
        active_endpoints,
        preference,
        captured_endpoint,
        state,
    } = source;

    let endpoints: Vec<AudioEndpointChoice> = active_endpoints
        .iter()
        .flatten()
        .map(|endpoint| AudioEndpointChoice {
            key: table.key_for(endpoint),
            name: endpoint.name.clone(),
        })
        .collect();

    let selection = match preference {
        AudioEndpointPreference::SystemDefault => AudioEndpointSelection::SystemDefault,
        AudioEndpointPreference::Explicit {
            id,
            last_known_name,
        } => match active_endpoints
            .iter()
            .flatten()
            .position(|endpoint| &endpoint.id == id)
        {
            Some(index) => AudioEndpointSelection::Chosen(endpoints[index].key),
            None if active_endpoints.is_none()
                || captured_endpoint
                    .as_ref()
                    .is_some_and(|endpoint| &endpoint.id == id) =>
            {
                AudioEndpointSelection::ChosenUnverified {
                    name: captured_endpoint
                        .as_ref()
                        .filter(|endpoint| &endpoint.id == id)
                        .map_or_else(|| last_known_name.clone(), |endpoint| endpoint.name.clone()),
                }
            }
            None => AudioEndpointSelection::ChosenButMissing {
                name: last_known_name.clone(),
            },
        },
    };

    AudioSourceReading {
        refresh_failed: *refresh_failed,
        endpoints_known: active_endpoints.is_some(),
        endpoints,
        selection,
        captured_key: captured_endpoint
            .as_ref()
            .map(|endpoint| table.key_for(endpoint)),
        captured_name: captured_endpoint
            .as_ref()
            .map(|endpoint| endpoint.name.clone()),
        state: capture_state(*state),
    }
}

/// What one discovery snapshot means to the shell, or `None` while the daemon
/// has nothing to say.
fn discovery_echo(
    discovery: &DiscoverySnapshot,
    receivers: &[ReceiverSnapshot],
) -> Option<DiscoveryEcho> {
    let DiscoverySnapshot {
        // The backend's browse generation, not the shell's refresh counter.
        generation: _,
        phase,
    } = discovery;

    match phase {
        // Suspend and shutdown stop the daemon. An empty inventory published
        // here would tell the user their receivers had disappeared, so the
        // last answer stands until the daemon runs again.
        DiscoveryPhase::Stopped => None,
        DiscoveryPhase::Running => Some(DiscoveryEcho::Inventory(inventory(receivers))),
        // A supervised restart with backoff is the closest thing the daemon
        // has to the legacy one-shot browse failing. `retry_at` is a
        // wall-clock instant and deliberately stays behind.
        DiscoveryPhase::Retrying {
            attempt: _,
            retry_at: _,
        } => Some(DiscoveryEcho::Failed(DISCOVERY_FAILED_SUMMARY.to_owned())),
    }
}

/// What the shell is told when discovery cannot be maintained.
///
/// Owned here rather than taken from the backend, and that ownership is the
/// point. `Notice.message` is the one free-form string the backend can put in
/// front of the bridge, and it is not always a constant: the network-monitor
/// arm interpolates a `NetworkError` into it. Today that error renders no
/// address, but "today it happens to be safe" is not a boundary. Every
/// discovery failure the shell sees is therefore this sentence, from both the
/// snapshot's `Retrying` phase and [`project_event`]'s notice arm, which
/// keeps the redaction promise checkable by reading this file.
pub(super) const DISCOVERY_FAILED_SUMMARY: &str = "Devices could not be searched for.";

/// Projects the receiver inventory into presentation-safe records, ordered by
/// stable identity so the comparison against the previous pass cannot depend
/// on the backend's display-name sort.
fn inventory(receivers: &[ReceiverSnapshot]) -> Vec<ReceiverState> {
    let mut records: Vec<ReceiverState> = receivers
        .iter()
        .map(|receiver| {
            let ReceiverSnapshot {
                id,
                name,
                model,
                lifecycle,
            } = receiver;
            ReceiverState {
                id: DeviceId::from(*id),
                name: display_name(*id, name),
                model: model.clone(),
                availability: availability_of(lifecycle),
            }
        })
        .collect();
    records.sort_by_key(|record| record.id.0);
    records
}

/// Passes a receiver's display name through, unless it *is* the identity.
///
/// The backend no longer synthesizes a name from [`ReceiverId`], so in a
/// consistent tree this returns `name` unchanged. It stays because the bridge
/// is where "no raw endpoint ID reaches the window" is enforced and checked,
/// and a name field is the one string on this path that crosses verbatim: the
/// leak canary cannot distinguish a MAC-shaped name from a legitimate one,
/// because it depends on names crossing. A rule the boundary owns outlives
/// the backend arm that happened to need it.
fn display_name(id: ReceiverId, name: &str) -> String {
    if name == id.to_string() {
        tracing::warn!("a receiver display name matched its identity and was suppressed");
        return String::new();
    }
    name.to_owned()
}

/// Collapses the eight-state receiver lifecycle onto the shell's two-state
/// availability.
///
/// Only "not currently discoverable" becomes `Unavailable`. Every other state
/// is a session concern the backend owns, and reporting one of them as
/// unavailable would drop the receiver out of the next start's member set --
/// and therefore out of the backend's own retry and rejoin policy.
fn availability_of(lifecycle: &ReceiverLifecycle) -> Availability {
    match lifecycle {
        ReceiverLifecycle::Unavailable => Availability::Unavailable,
        ReceiverLifecycle::Discovered
        | ReceiverLifecycle::Connecting { attempt: _ }
        | ReceiverLifecycle::SettingUp { role: _, phase: _ }
        | ReceiverLifecycle::Ready { role: _ }
        | ReceiverLifecycle::Streaming { role: _ }
        // `retry_at` is a wall-clock deadline and must not reach the shell.
        | ReceiverLifecycle::RetryWaiting {
            attempt: _,
            retry_at: _,
        }
        | ReceiverLifecycle::Failed {
            retryable: _,
            error: _,
        } => Availability::Available,
    }
}

/// What one session snapshot means to the shell, or `None` while a phase is
/// in flight and the window should keep showing what it already shows.
fn session_echo(session: &SessionSnapshot) -> Option<SessionEcho> {
    let SessionSnapshot {
        phase,
        // The committed wish, not the settled fact; see `desired_revision`.
        desired: _,
        active,
        // No shell vocabulary yet: per-receiver failure and retry state. The
        // whole-session phase now distinguishes a reduced delivery from a
        // complete one, but which receiver is waiting on which attempt is a
        // further step, and the inventory's availability is what the window
        // renders per row today.
        failed: _,
        retry_waiting: _,
        // No shell vocabulary yet: which receiver holds the PTP clock.
        primary: _,
        // No shell vocabulary yet: live versus silence-bridged audio.
        audio_flow: _,
        // No shell vocabulary yet: "stopped because Windows suspended" as
        // distinct from "stopped because the user stopped it".
        resume_pending: _,
    } = session;

    match phase {
        SessionPhase::Stopped => Some(SessionEcho::Stopped),
        // In flight, and the shell already stands where its own press put it.
        // Reporting either as a stop would flip the window to "stopped" and
        // back for every start and every teardown.
        SessionPhase::Starting { generation: _ } | SessionPhase::Stopping { generation: _ } => None,
        // A restart, by contrast, is *not* something the shell asked for: the
        // controller rebuilds the group after a membership change, a primary
        // replacement, a network rebind, or a resume. Reporting it as a stop
        // would still be wrong -- the session never ended -- but reporting
        // nothing at all left an audible interruption with no explanation on
        // screen, which is what this variant fixes.
        //
        // `reason` stays behind. It is presentation-safe, but the window has
        // no vocabulary for thirteen distinct causes yet, and inventing one
        // sentence for all of them would say less than the phase already does.
        SessionPhase::Restarting {
            generation: _,
            reason: _,
        } => Some(SessionEcho::Restarting),
        SessionPhase::Streaming { generation: _ } => Some(SessionEcho::Active(
            active.iter().map(|id| DeviceId::from(*id)).collect(),
        )),
        // Reported as its own phase rather than as an ordinary stream. The
        // active set says which receivers really carry audio, but only the
        // phase can say that the set is smaller than the one that was asked
        // for -- the shell cannot derive that, because its own desired set is
        // not the committed membership of this generation.
        SessionPhase::Degraded { generation: _ } => Some(SessionEcho::Degraded(
            active.iter().map(|id| DeviceId::from(*id)).collect(),
        )),
        SessionPhase::Failed {
            generation: _,
            error,
        } => Some(SessionEcho::Failed(error.as_str().to_owned())),
    }
}

// ---------------------------------------------------------------------------
// The diagnostics registry, projected
// ---------------------------------------------------------------------------

/// Derives the shell's [`DiagnosticsReading`] from one registry snapshot.
///
/// The most dangerous projection in the shell, and the reason it is written
/// like the device one: pure, total, and destructured by name without a `..`
/// rest pattern at every level it looks at, so a new field in
/// [`DiagnosticsSnapshot`] or in a receiver row stops this file compiling
/// until someone decides here whether the window may learn about it.
///
/// What crosses, and what does not:
///
/// * `schema_version` -- the registry's own contract number. It says nothing
///   about this installation that the build version does not already say.
/// * `snapshot_sequence` -- a cache-invalidation counter. It moves on every
///   publication, so putting it in the reading would make every reading
///   different from the last one and turn the shell's "changed?" comparison
///   into "always", which is exactly the check that keeps the window from
///   repainting for nothing.
/// * `captured_at_utc` -- **an absolute wall-clock instant, refused.** It is
///   the same class as `DiscoveryPhase::Retrying`'s `retry_at` and
///   `ReceiverLifecycle::RetryWaiting`'s, both of which this file already
///   holds back. A window value that changes with the calendar cannot be
///   asserted without asserting the clock, and a page that showed it would be
///   showing the reader their own system time back.
/// * `process_elapsed_ns` -- **the age of this process, refused,** for the
///   same reason: it moves on every publication and measures the app rather
///   than the audio.
/// * `active_session_id` -- a UUID used only to gate whether retained receiver
///   rows are current. The support export only ever emits it behind a
///   per-export alias; a page has no use for either form, because the window
///   already names the session by what it is doing.
/// * `health` -- crosses, as the shell's own four-word vocabulary.
/// * `session` -- held back whole. Its members are `ReceiverId`/`DeviceId`
///   pairs, its `phase` carries free-form `reason` text, and its `audio` half
///   is a foreign crate's snapshot; none of that has a word on this page yet,
///   and a partial session row would be the least honest thing here.
/// * `receivers` -- **counted, never forwarded.** See [`measured_receivers`].
/// * `diagnostics_events_dropped_total` -- crosses. A plain counter of what
///   the diagnostics feed itself lost, and the one number on this page that
///   is a real measurement from the first moment the registry runs.
pub(crate) fn diagnostics_reading(snapshot: &DiagnosticsSnapshot) -> DiagnosticsReading {
    let DiagnosticsSnapshot {
        schema_version: _,
        snapshot_sequence: _,
        captured_at_utc: _,
        process_elapsed_ns: _,
        active_session_id,
        health,
        session: _,
        receivers,
        diagnostics_events_dropped_total,
    } = snapshot;

    DiagnosticsReading {
        health: health_of(*health),
        events_dropped_total: *diagnostics_events_dropped_total,
        measured_receivers: if active_session_id.is_some() {
            measured_receivers(receivers)
        } else {
            0
        },
    }
}

/// Translates the registry's health badge into the shell's own word.
///
/// A wildcard-free match, so a fifth registry state cannot arrive silently as
/// one of these four.
fn health_of(health: Health) -> DiagnosticsHealth {
    match health {
        Health::Unknown => DiagnosticsHealth::Unknown,
        Health::Running => DiagnosticsHealth::Running,
        Health::Attention => DiagnosticsHealth::Attention,
        Health::Error => DiagnosticsHealth::Error,
    }
}

/// How many receivers the registry currently holds measurements for.
///
/// A count, and the destructuring above it is the point rather than a
/// formality. Every field of a receiver row is named and refused here, which
/// is what makes "the window learns the number of rows and nothing else"
/// something the compiler keeps true: a seventh field added to
/// [`ReceiverDiagnosticsSnapshot`] does not compile until it appears in this
/// pattern, and whoever adds it has to decide in this file whether the window
/// may see it.
///
/// Every one of the six is refused today, and none of them for a small
/// reason:
///
/// * `key` -- a session UUID and a MAC-derived `ReceiverId`. Raw identity.
/// * `lifecycle` -- carries a `UserFacingError` whose text is free-form.
/// * `timing`, `transport`, `feedback` -- presentation-safe numbers, and the
///   natural next step, but a per-receiver row needs a receiver *name* to sit
///   next to, and the name lives in the device snapshot rather than here.
///   Half a row -- numbers with no receiver against them -- would be worse
///   than none.
/// * `last_error` -- classification is safe, but `public_message` and
///   `technical_detail` are unbounded strings and the support export withholds
///   both. What that export refuses to put in a file a user sends to support,
///   this page does not put on screen permanently.
fn measured_receivers(receivers: &[ReceiverDiagnosticsSnapshot]) -> usize {
    receivers
        .iter()
        .filter(|receiver| {
            let ReceiverDiagnosticsSnapshot {
                key: _,
                lifecycle: _,
                timing: _,
                transport: _,
                feedback: _,
                last_error: _,
            } = receiver;
            true
        })
        .count()
}

/// Translates one backend event into the shell's vocabulary, or drops it.
///
/// The snapshot is authoritative for state, so almost everything here is
/// deliberately dropped. The one exception is the discovery notice: the
/// snapshot's `Retrying` phase says discovery is failing, but a notice can
/// also arrive while the phase is `Running` -- the network-change monitor
/// refusing to register is reported that way -- and that condition would
/// otherwise never reach the shell at all.
///
/// The path is complete: `reducer` answers
/// [`ControllerEvent::DiscoveryFailed`] with a `DiscoveryFailed` notice, so
/// what leaves the bridge here is what the window shows. It was not always
/// so -- the reducer discarded this event in a catch-all until that arm was
/// added -- and the note survives because it names where the other half of
/// this path lives.
///
/// No string on this path is forwarded. `message` is free-form backend prose
/// and at least one emitter interpolates an error into it, so what the shell
/// learns is the code, rendered as the bridge's own
/// [`DISCOVERY_FAILED_SUMMARY`]. `severity` is dropped with it: the shell's
/// `DiscoveryFailed` has one severity, and a warning that is worth telling
/// the user about is worth telling them plainly.
pub(crate) fn project_event(
    event: &BackendEvent,
    seen: GenerationsSeen,
) -> Option<ControllerEvent> {
    match event {
        // Correlated group outcomes are delivered by GenerationEcho from the
        // retained snapshot history, so broadcast lag cannot lose a result.
        // A completed coalesced setting such as SetMasterVolume is not news.
        BackendEvent::CommandCompleted { id: _ } => None,
        BackendEvent::CommandFailed { id: _, error: _ } => None,
        BackendEvent::Notice {
            severity: _,
            code,
            scope: _,
            message: _,
        } => match code {
            BackendNoticeCode::DiscoveryFailed => Some(ControllerEvent::DiscoveryFailed {
                generation: seen.discovery,
                summary: DISCOVERY_FAILED_SUMMARY.to_owned(),
            }),
            // Session failure is taken from `SessionPhase::Failed`, which
            // carries its own redacted reason and cannot disagree with the
            // phase the window is showing.
            BackendNoticeCode::SessionFailed => None,
            // No shell vocabulary yet: capture loss, device-state write
            // failure, and internal queue overload.
            BackendNoticeCode::CaptureUnavailable
            | BackendNoticeCode::PersistenceWrite
            | BackendNoticeCode::QueueOverloaded => None,
        },
    }
}
