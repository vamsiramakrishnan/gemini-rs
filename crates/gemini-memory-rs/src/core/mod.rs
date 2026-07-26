//! Domain vocabulary, policy and the event log.
//!
//! Nothing in `core` performs I/O, calls a model, or knows about Gemini Live.
//! It is the part of the memory engine that can be reasoned about — and tested
//! — entirely on its own.

pub mod domain;
pub mod error;
pub mod events;
pub mod ids;
pub mod policy;

pub use domain::{
    normalize_token, stable_hash, CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters,
    Explicitness, FactFingerprint, MemoryKind, MemoryObservation, MemorySource, MemoryStatus,
    MemoryValue, MutationIntent, PrivacyMetadata, ProposedPersistence, RetrievalMetadata,
    SensitivityClass, SpeakerAttribution, TemporalMetadata, TemporalScope, TranscriptEvidence,
};
pub use error::MemoryError;
pub use events::{
    CommitReceipt, InMemoryEventLog, MemoryEvent, MemoryEventEnvelope, MemoryEventLog,
    SessionEventWriter, EVENT_SCHEMA_VERSION,
};
pub use ids::{
    ConnectionId, EntityId, EventId, MemoryId, ObservationId, PlanId, SessionId, SnapshotId,
    TurnId, UserId,
};
pub use policy::{
    admit_observation, aggregate_confidence, contains_instruction_shaped_content,
    default_episodic_ttl, meets_promotion_criteria, resolve_expiry, speaker_is_admissible,
    AdmissionVerdict, CadenceConfig, DiscardReason, IngestionConfig, MemoryRuntimeConfig,
    PromotionConfig, PromotionEvidence, RetrievalConfig, SessionConfig, TranscriptConfig,
};
