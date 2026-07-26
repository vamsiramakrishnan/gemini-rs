#![warn(missing_docs)]
//! # gemini-memory-rs
//!
//! A contextual memory engine for Gemini Live voice sessions.
//!
//! The engine's organising principle is that **context is prepared
//! asynchronously and consumed synchronously**. Nothing expensive — model
//! calls, search, repository writes — ever happens on the path between the
//! model asking for memory and the memory arriving. By the time a
//! `recall_context` tool call lands, the answer is already sitting in state.
//!
//! ```text
//! user speech ─► input transcription ─► retrieval-state extraction
//!                                            │
//!                                            ▼
//!                                    local BM25 search
//!                                            │
//!                                            ▼
//!                              immutable prepared snapshot ─► Gemini
//!
//! final transcript ─► observation extraction ─► session ledger
//!                                                    │
//!                        ┌───────────────────────────┤
//!                        ▼                           ▼
//!                 session overlay          post-session reconciliation
//!                 (usable now)                       │
//!                                                    ▼
//!                                          canonical OKF markdown
//! ```
//!
//! ## Layout
//!
//! Each module is the design's correspondingly-named component:
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`core`] | Domain vocabulary, deterministic policy, event log |
//! | [`okf`] | Canonical Markdown memory records and the repository |
//! | [`bm25`] | Fielded lexical index, ranking, and search explanation |
//! | [`transcript`] | Partial/final transcript accumulation and debouncing |
//! | [`retrieval`] | Retrieval plans, fusion, budgeted context assembly |
//! | [`ingestion`] | Observation extraction, candidate ledger, session overlay |
//! | [`reconcile`] | Consolidation, conflict resolution, promotion, commit |
//! | [`runtime`] | Live-session wiring: state keys, control loop, tools |
//! | [`evals`] | Fixture-driven quality harness |
//!
//! ## Getting started
//!
//! ```no_run
//! use gemini_memory_rs::prelude::*;
//!
//! # async fn demo() -> Result<(), MemoryError> {
//! let engine = MemoryEngine::in_memory(UserId::new("usr_72ab"));
//!
//! // A finalized user turn: evidence in, context out.
//! let session = engine.begin_session(SessionId::new("ses_01"));
//! session.observe_final_transcript(TurnId(1), "I am pescatarian").await?;
//!
//! let snapshot = session.prepare(TurnId(2), "what should we eat tonight").await?;
//! for fact in snapshot.facts.iter() {
//!     println!("{}", fact.statement);
//! }
//! # Ok(())
//! # }
//! ```

pub mod bm25;
pub mod core;
pub mod okf;
pub mod retrieval;
pub mod transcript;

/// The types a typical application touches.
#[cfg(any())]
pub mod prelude {
    pub use crate::bm25::{MemoryIndex, SearchExplanation, SearchHit};
    pub use crate::core::{
        CanonicalMemory, CanonicalPredicate, EntityRef, Explicitness, MemoryError, MemoryEvent,
        MemoryKind, MemoryObservation, MemoryRuntimeConfig, MemoryStatus, MemoryValue,
        MutationIntent, ProposedPersistence, SensitivityClass, SessionId, SpeakerAttribution,
        TemporalScope, TurnId, UserId,
    };
    pub use crate::engine::{MemoryEngine, MemorySession};
    pub use crate::ingestion::{SessionCandidate, SessionCandidateStatus, SessionMemoryOverlay};
    pub use crate::okf::{MemoryRepository, OkfDocument};
    pub use crate::reconcile::{ProposedMutation, ResolvedMutation};
    pub use crate::retrieval::{
        MemoryRetriever, PreparedMemorySnapshot, RetrievalPlan, RetrievedMemory,
    };
    pub use crate::runtime::{keys, MemoryRuntimeEvent};
}
