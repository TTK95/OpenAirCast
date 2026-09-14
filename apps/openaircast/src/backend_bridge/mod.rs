//! The single seam between the egui shell and the resilience backend library.
//!
//! The shell's binary module tree and the backend library are two disjoint
//! module trees over one directory: nothing under `src/app`, `src/ui`, or
//! `src/tray` can name a backend type, and nothing under `src/backend` can
//! name a shell type. This module is the one deliberate exception, and a test
//! in it enforces that it stays the only one. That is what makes "no address,
//! socket, secret, raw endpoint ID, device, connection, session, or backend
//! diagnostic object reaches a snapshot or a renderer" checkable by reading
//! one directory rather than auditing the whole shell.
//!
//! Two pure halves and the thread that joins them:
//!
//! * downward, [`BackendPort`] adapts the shell effect executor's
//!   [`crate::app::ControllerPort`] onto [`homepod_cast::backend::BackendCommand`];
//! * upward, [`Projection::project`] derives [`crate::app::ControllerEvent`]s from one
//!   [`homepod_cast::DeviceSnapshot`], and [`project_event`] translates the low-volume
//!   [`homepod_cast::backend::event::BackendEvent`] feed;
//! * between them, [`spawn_backend_bridge`] owns one thread and one
//!   `current_thread` runtime, and holds no lock across an `await`.
//!
//! [`backend_device_seam`] ties all three to a started backend. Which
//! implementation the shell opens is one value,
//! [`crate::app::ACTIVE_DEVICE_STAGE`], with
//! [`crate::device_service::legacy_device_seam`] as the other arm and the way
//! back.
//!
//! The upward half is level-driven, not transactional. Every pass re-derives
//! the whole answer from the newest snapshot, and an event the shell's bounded
//! queue refuses is retained and offered again rather than dropped, so the
//! window cannot end up permanently out of step with a backend that has
//! settled and has no reason to publish again.
//!
//! What the shell still does not learn from the backend is listed field by
//! field in [`Projection::project`] and [`session_echo`], each with the reason
//! it stays behind. Nothing is omitted silently.

mod commands;
mod projection;
mod runtime;

#[cfg(test)]
mod tests;

// These names formed the original single-file module facade. Most are used by
// the private contract tests rather than the release binary, but keeping them
// here makes the split invisible to every existing crate-local consumer.
#[allow(unused_imports)]
pub(crate) use commands::{BackendPort, GenerationEcho, GenerationsSeen, SessionEdge};
#[allow(unused_imports)]
pub(crate) use projection::{
    diagnostics_reading, project_event, EndpointDirectory, EndpointTable, Projection,
};
#[allow(unused_imports)]
pub(crate) use runtime::{
    backend_config_for, backend_device_seam, legacy_volume_source, spawn_backend_bridge,
    BridgeHandle, DEVICE_SHUTDOWN_BUDGET,
};

#[cfg(test)]
use projection::{audio_echo, project_live_diagnostics, DISCOVERY_FAILED_SUMMARY};
#[cfg(test)]
use runtime::{device_seam_from, failed_session_state, finished, flush, repair_delay};
