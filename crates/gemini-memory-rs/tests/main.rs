//! One test binary for this crate's integration tests: each module is
//! one former `tests/*.rs` file. Linking one binary instead of one per
//! file is most of the build-time saving in `cargo test`.

mod common;

mod alias_terms_probe;
mod canonical_language_e2e;
mod code_switched_e2e;
mod contradiction_reconciliation;
mod filter_dsl_probe;
mod governed_integration;
mod live_memory_e2e;
mod live_session_e2e;
mod memory_at_scale;
mod memory_lifecycle_e2e;
mod memory_map_probe;
mod memory_paraphrase;
mod memory_retrieval;
mod model_latency_probe;
mod quantization_probe;
mod query_rewrite_probe;
mod semantic_fusion_probe;
mod serving_latency_probe;
mod speculation_gate;
mod storage_encoding_probe;
mod transcript_boundaries;
mod voice_personas_e2e;
