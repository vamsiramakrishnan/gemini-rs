//! How a record is turned into the text an embedder sees.
//!
//! This is a thin re-export of the crate's own
//! [`gemini_memory_rs::retrieval::embedding`], and that is the point. The
//! winning view used to be defined here, in test-only code, which meant the
//! text every measurement was taken against was not the text any caller would
//! ship. Now the experiments import the production function, so the numbers in
//! `semantic_fusion_probe` cannot drift away from what `embedding_text`
//! actually produces — a change to one is a change to both.

#![allow(dead_code, unused_imports)]

pub use gemini_memory_rs::retrieval::{
    embedding_text as structural_view, frontmatter_prose as structural, predicate_line as predicate,
};

use gemini_memory_rs::core::CanonicalMemory;

/// The aliases and tags the record already carries.
///
/// Still local: this is one of the *losing* views from the comparison, kept
/// only so the experiment can still measure it. Nothing ships it.
pub fn curated(memory: &CanonicalMemory) -> String {
    let mut lines = Vec::new();
    if !memory.retrieval.aliases.is_empty() {
        lines.push(format!(
            "Also asked as: {}",
            memory.retrieval.aliases.join(", ")
        ));
    }
    if !memory.retrieval.tags.is_empty() {
        lines.push(format!("Topics: {}", memory.retrieval.tags.join(", ")));
    }
    lines.join("\n")
}
