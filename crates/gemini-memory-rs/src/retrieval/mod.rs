//! Preparing memory context before the model asks for it.
//!
//! The pipeline runs entirely off the response path: a plan is derived from the
//! transcript, queries are fused, and a budgeted snapshot is frozen into state.
//! When the `recall_context` tool call arrives, serving it is a state read.

pub mod assembler;
pub mod deterministic;
pub mod extractor;
pub mod fusion;
pub mod plan;
pub mod retriever;
pub mod snapshot;

pub use assembler::ContextAssembler;
pub use deterministic::{DeterministicPlanner, KnownEntities, RetrievalSignal};
pub use extractor::{
    context_for, retrieval_plan_schema, BoundedPlanExtractor, DeterministicPlanExtractor,
    RetrievalExtractionContext, RetrievalPlanExtractor, RETRIEVAL_PLAN_INSTRUCTION,
};
pub use fusion::{reciprocal_rank_fusion, FusedCandidate, RRF_K};
pub use plan::{limits, RetrievalEntity, RetrievalIntent, RetrievalPlan, TemporalConstraint};
pub(crate) use retriever::non_topical_terms;
pub use retriever::{
    IndexHandle, LocalMemoryRetriever, MemoryRetriever, RetrievalBudget, RetrievalRequest,
    SemanticFallback,
};
pub use snapshot::{
    estimate_tokens, PreparedMemorySnapshot, RetrievedMemory, SNAPSHOT_TTL_SECONDS,
};
