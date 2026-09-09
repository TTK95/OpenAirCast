//! Device backend: single-owner actor state, commands, events, supervisors.

pub mod command;
pub mod event;
pub mod listening_check;
pub(crate) mod listening_tone;
pub mod model;

pub use command::{
    BackendCommand, BackendSendError, CommandEnvelope, DeviceBackendHandle, SystemEvent,
    COMMAND_CAPACITY,
};
pub use event::{
    BackendEvent, DeviceBackendUpdates, DiagnosticCategory, DiagnosticEvent,
    DiagnosticEventReceiver, DiagnosticKind, DiagnosticPayload, DiagnosticRecvError,
    DiagnosticTryRecvError, ErrorScope, GenerationCell, Generations, NoticeCode, Severity,
    SystemTransition, BACKEND_EVENT_CAPACITY, DIAGNOSTIC_CAPACITY, PCM_CAPACITY,
    SUPERVISOR_CAPACITY,
};
pub mod persistence;
pub mod recovery;

pub use persistence::{
    effective_volume, FileStore, LoadOutcome, OsStateReplacer, PersistError, PersistedStateV1,
    StateReplacer, StateStore,
};
pub use recovery::{
    Clock, InvalidTransition, ReceiverMachine, RejoinRestartLimiter, RetryPolicy, RetryState,
    SystemClock,
};

// --- discovery supervisor (Task 7) ---
pub mod discovery;

pub use discovery::{DiscoveryConfig, DiscoveryControl, DiscoverySupervisor, DiscoveryUpdate};

// --- capture supervisor (Task 9) ---
pub mod capture;

pub use capture::{
    CaptureControl, CaptureError, CaptureSource, CaptureSupervisor, CaptureUpdate, CaptureWorker,
};

// --- session supervisor (Task 14) ---
pub mod session;

pub use session::{
    ReconfigureCause, RuntimeEventLag, SessionDecoderSource, SessionRequest, SessionSendError,
    SessionStartFailure, SessionSupervisor, SessionSupervisorHandle, SessionTransport,
    SessionTransportFactory, SessionUpdate, SESSION_REQUEST_CAPACITY, TEARDOWN_GROUP,
    TEARDOWN_PER_RECEIVER,
};

// --- production session transport (closes the Task 15 stub gap) ---
pub mod transport;

pub use transport::AirPlaySessionTransportFactory;

// --- network monitor (Task 17) ---
pub mod network;

pub use network::{
    BindingFingerprint, LocalBindingSource, NetworkChangeSource, NetworkError, NetworkMonitor,
    NetworkSubscription,
};

// --- backend controller (Task 15) ---
pub mod controller;

pub use controller::{
    start_device_backend, start_device_backend_with, BackendConfig, BackendDiscoverySource,
    BackendOverrides, BackendTimer, TestStartGate,
};
