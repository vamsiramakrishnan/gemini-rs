//! How a record is turned into the text an embedder sees.
//!
//! Shared because two suites must embed *identical* text or their numbers are
//! not comparable: `semantic_fusion_probe` measures which view retrieves best,
//! and `quantization_probe` measures what compressing the winning view costs.
//! A copy-paste divergence between them would silently invalidate the second.

#![allow(dead_code)]

use gemini_memory_rs::core::CanonicalMemory;

/// What the frontmatter knows, written out as prose.
///
/// The fields are already there and already indexed by BM25 with their own
/// weights; the embedding was throwing all of them away. This costs nothing to
/// produce — no model, no author, no judgement.
pub fn structural(memory: &CanonicalMemory) -> String {
    let mut lines = vec![
        format!("About: {}", memory.subject.display),
        predicate(memory),
    ];
    if !memory.retrieval.entities.is_empty() {
        lines.push(format!(
            "Mentions: {}",
            memory.retrieval.entities.join(", ")
        ));
    }
    if let Some(location) = &memory.retrieval.location {
        lines.push(format!("Place: {location}"));
    }
    if let Some(qualifier) = &memory.qualifier {
        lines.push(format!("When: {qualifier}"));
    }
    lines.push(format!("Holds: {:?}", memory.temporal_scope));
    lines.join("\n")
}

/// The line that names the attribute this record is about.
///
/// A statement says the *value* — "The user's usual coffee order is a cortado"
/// — and only implies the attribute. The predicate names it outright, which is
/// what a question asks by.
pub fn predicate(memory: &CanonicalMemory) -> String {
    format!(
        "Kind: {:?} {}",
        memory.kind,
        memory.predicate.as_str().replace('_', " ")
    )
}

/// The aliases and tags the record already carries.
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

/// The winning view: the statement plus the frontmatter written out as prose.
///
/// Named because it is now a production recommendation rather than one column
/// of an experiment.
pub fn structural_view(memory: &CanonicalMemory) -> String {
    format!("{}\n{}", memory.statement, structural(memory))
}
