//! UI-independent device backend contract for OpenAirCast.
//!
//! The library exposes the stable command/state/event surface consumed by the
//! egui shell. The binary target keeps its own private module tree; nothing in
//! here reaches into transport, socket, key, or AirPlay connection types.

// Let shared test-support modules reference the library by name whether they
// are compiled into unit tests or into integration-test binaries.
extern crate self as homepod_cast;

pub mod backend;

pub mod calibration;

pub mod diagnostics;

pub use diagnostics::{
    CalibrationApplyState, DefiniteCounterKind, DiagnosticComponent, DiagnosticError,
    DiagnosticErrorCode, DiagnosticEvent, DiagnosticEventDraft, DiagnosticSeverity,
    DiagnosticsClock, DiagnosticsHandle, DiagnosticsRegistry, DiagnosticsRing,
    DiagnosticsSessionTracker, DiagnosticsSnapshot, EventBatch, EventCursor, EventGap,
    ExportEventState, Health, ReceiverDiagnosticsSnapshot, ReceiverLifecycleState,
    ReceiverSessionKey, ReceiverTimingSnapshot, ReceiverTimingSource, ReceiverTransportSnapshot,
    Recoverability, SessionDiagnosticsSnapshot, SessionDiagnosticsState, SessionId,
    SessionRegistration, SessionStopReason, StructuredDiagnosticPayload, SystemDiagnosticsClock,
    DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION, EVENT_RING_CAPACITY, MAX_EVENT_READ_LIMIT,
};

pub use backend::model::{
    AudioEndpointPreference, AudioFlow, AudioSourceSnapshot, AudioSourceState, DeviceSnapshot,
    DiscoveryPhase, DiscoverySnapshot, LatencyConfig, LatencyPreset, ModelError,
    PersistenceSnapshot, ReceiverId, ReceiverLifecycle, ReceiverRole, RestartReason, RunIntent,
    SavedGroup, SavedGroupId, SavedGroupMember, SessionPhase, SessionSnapshot, SetupPhase,
    UserFacingError, Volume,
};
