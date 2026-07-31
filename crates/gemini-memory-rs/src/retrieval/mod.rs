//! Preparing memory context before the model asks for it.
//!
//! The pipeline runs entirely off the response path: a plan is derived from the
//! transcript, queries are fused, and a budgeted snapshot is frozen into state.
//! When the `recall_context` tool call arrives, serving it is a state read.

pub mod assembler;
pub mod deterministic;
pub mod embedding;
pub mod extractor;
pub mod fusion;
pub mod plan;
pub mod retriever;
pub mod semantic;
pub mod snapshot;
pub mod vocabulary;

pub use assembler::ContextAssembler;
pub use deterministic::{DeterministicPlanner, KnownEntities, RetrievalSignal};
pub use embedding::{embedding_text, frontmatter_prose, predicate_line};
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
pub use semantic::{Embedder, PrecomputedSemanticIndex, StaticEmbedder, RERANK_DEPTH};
pub use snapshot::{
    estimate_tokens, fuse_snapshots, PreparedMemorySnapshot, RetrievedMemory, SNAPSHOT_TTL_SECONDS,
};
pub use vocabulary::{memory_map, memory_map_with_limit, DEFAULT_LIMIT as MEMORY_MAP_LIMIT};
