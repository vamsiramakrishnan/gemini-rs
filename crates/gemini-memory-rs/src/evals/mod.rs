//! The fixture-driven quality harness.
//!
//! Memory quality is not something a type system can hold up. These are the
//! measurements that decide whether a change to ranking, extraction or policy
//! made B better or merely different: a hand-written corpus, a set of cases
//! stating what should happen, and the thresholds from the design's acceptance
//! criteria enforced as tests.

pub mod fixtures;
pub mod harness;
pub mod metrics;

pub use fixtures::{
    corpus, eval_user, ingestion_cases, retrieval_cases, IngestionCase, RetrievalCase,
};
pub use harness::{run_ingestion_eval, run_retrieval_eval, IngestionReport, RetrievalReport};
