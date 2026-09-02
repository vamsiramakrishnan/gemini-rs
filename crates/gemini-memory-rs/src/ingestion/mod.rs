//! Capturing evidence from a live conversation.
//!
//! Ingestion is the write path's front half: finalized user speech becomes
//! observations, observations accumulate into session candidates, and the
//! usable candidates project into an overlay the retriever can search
//! immediately. Nothing here writes canonical memory — that is reconciliation's
//! job, and it happens after the conversation.

pub mod checkpoint;
pub mod ledger;
pub mod observation;
pub mod overlay;

pub use checkpoint::{CadenceTracker, ScheduledWork};
pub use ledger::{
    InMemorySessionLedger, LedgerOutcome, MicroReconciliationReport, ObservationEvidence,
    SealedSessionLedger, SessionCandidate, SessionCandidateStatus, SessionLedger,
    SessionLedgerSnapshot,
};
pub use observation::{
    BoundedObservationExtractor, MemoryObservationExtractor, OBSERVATION_EXTRACTION_INSTRUCTION,
    ObservationExtractionContext, RuleBasedObservationExtractor, observation_schema,
};
pub use overlay::{SessionMemoryOverlay, provisional_memory, provisional_memory_id};
