//! Shared scaffolding for the integration tests.
//!
//! Some of these tests reach the real Gemini API. Without a key they skip
//! rather than fail, so `cargo test --workspace` stays meaningful on a machine
//! — or a CI runner — that has no credentials. The parts that need no model at
//! all (scratch directories, corpus rendering, the haystack fixture) are not
//! gated, so a retrieval test can use them on a default build.

#![allow(dead_code)]

pub mod corpus;
#[cfg(feature = "gemini-llm")]
pub mod live;
pub mod paraphrase;
pub mod rank;
pub mod views;
pub mod voice;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gemini_memory_rs::core::{
    CanonicalMemory, MemoryEvent, MemoryRuntimeConfig, MemoryStatus, SessionId, UserId,
};
use gemini_memory_rs::engine::MemoryEngine;
#[cfg(feature = "gemini-llm")]
use gemini_memory_rs::llm::{extraction_llm, GeminiObservationExtractor, GeminiPlanExtractor};
use gemini_memory_rs::okf::{FsStore, OkfRepository};

/// The extraction model these tests drive.
///
/// Extraction wants the smallest model that still holds the schema: it runs on
/// every finalized turn, off the voice path but inside the window before the
/// user says something else. Override with `GEMINI_EXTRACTION_MODEL` to A/B a
/// different one against the same suites.
#[cfg(feature = "gemini-llm")]
pub fn extraction_model() -> String {
    std::env::var("GEMINI_EXTRACTION_MODEL")
        .unwrap_or_else(|_| gemini_memory_rs::llm::DEFAULT_TRANSCRIPT_MODEL.to_string())
}

/// The model driving *retrieval plans*, which defaults to [`extraction_model`].
///
/// Split from observation extraction because the two jobs are not equally hard.
/// Observation extraction reads an utterance that is already in front of it;
/// planning has to canonicalise the *question* into the English search terms the
/// stored fact was canonicalised into, with nothing but the question to go on.
/// A model can be entirely adequate at the first and unreliable at the second,
/// so `GEMINI_PLAN_MODEL` allows pinning them independently.
#[cfg(feature = "gemini-llm")]
pub fn plan_model() -> String {
    std::env::var("GEMINI_PLAN_MODEL")
        .unwrap_or_else(|_| gemini_memory_rs::llm::DEFAULT_EXTRACTION_MODEL.to_string())
}

/// Whether a Gemini API key is configured.
pub fn have_api_key() -> bool {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

/// Report a skip, for tests that need credentials.
pub fn skip(test: &str) {
    eprintln!("SKIP {test}: no Gemini API key configured");
}

/// A scratch directory that removes itself.
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create a fresh directory under the system temp dir.
    pub fn new(label: &str) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!("gemini-memory-{label}-{nanos:x}"));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self { path }
    }

    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// An engine backed by real files, with the bundled deterministic extractors.
///
/// The corpus is written and read back as OKF Markdown, which is the part worth
/// exercising; nothing here reaches the network.
pub fn file_backed_engine(user: &str, root: &Path) -> MemoryEngine {
    MemoryEngine::new(
        UserId::new(user),
        Arc::new(OkfRepository::new(Arc::new(FsStore::new(root)))),
        Arc::new(gemini_memory_rs::core::InMemoryEventLog::new()),
        MemoryRuntimeConfig::default(),
    )
}

/// An engine backed by real files and real model-driven extraction.
#[cfg(feature = "gemini-llm")]
pub fn model_backed_engine(user: &str, root: &Path) -> MemoryEngine {
    let observations = extraction_llm(&extraction_model());
    let plans = extraction_llm(&plan_model());
    file_backed_engine(user, root)
        .with_plan_extractor(Arc::new(GeminiPlanExtractor::new(plans)))
        .with_observation_extractor(Arc::new(GeminiObservationExtractor::new(observations)))
}

/// Every canonical Markdown file under a root, concatenated with headers.
pub fn corpus_text(root: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push_str(&format!("\n===== {} =====\n", path.display()));
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    out
}

/// Render a corpus for a failure message: one line per record.
pub fn describe(records: &[CanonicalMemory]) -> String {
    records
        .iter()
        .map(|m| {
            format!(
                "  [{:?}/{:?}] {} = {} (conf {:.2}, ev {}×{}s)",
                m.status,
                m.kind,
                m.predicate,
                m.statement,
                m.confidence,
                m.evidence.count,
                m.evidence.distinct_sessions
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Active records only.
pub fn active(records: &[CanonicalMemory]) -> Vec<&CanonicalMemory> {
    records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect()
}

/// Whether any record's statement mentions `needle`, case-insensitively.
pub fn mentions(records: &[&CanonicalMemory], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    records
        .iter()
        .any(|m| m.statement.to_lowercase().contains(&needle))
}

/// Extraction failures and policy rejections recorded for a session.
///
/// A failing extractor and a quiet conversation look identical in the corpus,
/// so every assertion about "nothing was stored" should report these too.
pub async fn extraction_failures(engine: &MemoryEngine, session: &str) -> Vec<String> {
    engine
        .events()
        .replay_session(&SessionId::new(session))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| match e.payload {
            MemoryEvent::ExtractionFailed { stage, reason } => Some(format!("{stage}: {reason}")),
            // A policy rejection is the other way evidence vanishes quietly.
            MemoryEvent::ObservationRejected { reason, .. } => {
                Some(format!("observation rejected: {reason:?}"))
            }
            _ => None,
        })
        .collect()
}

/// A failure message that says what the corpus holds *and* what went wrong.
pub async fn diagnose(engine: &MemoryEngine, session: &str, records: &[CanonicalMemory]) -> String {
    let failures = extraction_failures(engine, session).await;
    let mut out = describe(records);
    if !failures.is_empty() {
        out.push_str("\nextraction failures:\n");
        for failure in failures {
            out.push_str("  ");
            out.push_str(&failure);
            out.push('\n');
        }
    }
    out
}
