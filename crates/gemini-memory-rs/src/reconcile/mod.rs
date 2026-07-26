//! Turning a conversation's evidence into durable memory.
//!
//! Reconciliation runs after the conversation, where it can afford to be
//! careful. A sealed ledger is consolidated into a handful of proposals, each
//! proposal is resolved against the records that could plausibly be about the
//! same thing, and the whole session commits as one transaction.
//!
//! The division of labour is deliberate: a model may generate proposals, but
//! only [`resolver`] decides what a proposal means, and only [`commit`] writes
//! anything.

pub mod commit;
pub mod consolidate;
pub mod promotion;
pub mod proposal;
pub mod resolver;

pub use commit::{commit_promotions, MemoryCommitter, ReconciliationReport};
pub use consolidate::{consolidate, ConsolidationOutput};
pub use promotion::{
    evaluate, sweep, PromotionOutcome, PromotionShortfall, STAGING_RETENTION_DAYS,
};
pub use proposal::{MemorySelector, ProposedMemory, ResolutionKind, ResolvedMutation};
pub use resolver::{proposal_from, Resolver};
