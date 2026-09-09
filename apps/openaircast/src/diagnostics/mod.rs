//! Structured application diagnostics, immutable snapshots, and support export.
//!
//! This facade preserves the diagnostics API while the implementation remains
//! split by ownership: domain snapshots, bounded ring storage, the registry
//! worker, and allow-listed export formatting.

mod export;
mod registry;
mod ring;
mod snapshot;

#[cfg(test)]
mod tests;

pub use export::{
    build_support_export, build_support_summary, ReceiverAlias, RedactionDescriptor, SessionAlias,
    SupportApplication, SupportAudio, SupportAudioConfiguration, SupportCalibration,
    SupportCalibrationContext, SupportCaptureQueue, SupportConfiguration, SupportError,
    SupportEvent, SupportEventGap, SupportEventPayload, SupportExportArtifact,
    SupportExportContext, SupportExportInput, SupportExportV1, SupportFeedback, SupportLifecycle,
    SupportReceiver, SupportRetransmit, SupportScheduler, SupportSenderBuffer, SupportSession,
    SupportSessionPhase, SupportSnapshot, SupportTiming, SupportTransport,
    SUPPORT_EXPORT_MEDIA_TYPE, SUPPORT_EXPORT_SCHEMA, SUPPORT_EXPORT_SCHEMA_VERSION,
};
pub use registry::{DiagnosticsHandle, DiagnosticsRegistry};
pub use ring::DiagnosticsRing;
pub use snapshot::{
    BackendDiagnosticPayload, CalibrationApplyState, ClientDiagnosticEvent, DefiniteCounterKind,
    DiagnosticComponent, DiagnosticError, DiagnosticErrorCode, DiagnosticEvent,
    DiagnosticEventDraft, DiagnosticSeverity, DiagnosticsClock, DiagnosticsSessionTracker,
    DiagnosticsSnapshot, EventBatch, EventCursor, EventGap, ExportEventState, Health,
    ReceiverDiagnosticsSnapshot, ReceiverLifecycleState, ReceiverSessionKey,
    ReceiverTimingSnapshot, ReceiverTimingSource, ReceiverTransportSnapshot, Recoverability,
    SessionDiagnosticsSnapshot, SessionDiagnosticsState, SessionId, SessionRegistration,
    SessionStopReason, StructuredDiagnosticPayload, SystemDiagnosticsClock,
    DIAGNOSTICS_SNAPSHOT_SCHEMA_VERSION, EVENT_RING_CAPACITY, MAX_EVENT_READ_LIMIT,
};
