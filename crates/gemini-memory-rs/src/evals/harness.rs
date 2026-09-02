//! Running the evaluation corpus against the real engine.
//!
//! The harness exercises the same code path a live session would: a plan is
//! derived from the utterance, queries are fused, and the assembler produces a
//! budgeted snapshot. Nothing is stubbed, so a regression in ranking or budget
//! shows up here rather than in production.

use chrono::Utc;
use std::sync::Arc;

use super::fixtures::{
    IngestionCase, RetrievalCase, corpus, eval_user, ingestion_cases, retrieval_cases,
};
use super::metrics;
use crate::bm25::{IndexedMemory, MemoryIndex};
use crate::core::{
    AdmissionVerdict, IngestionConfig, MemoryError, MemoryStatus, RetrievalConfig, SessionId,
    TurnId, admit_observation,
};
use crate::ingestion::{
    MemoryObservationExtractor, ObservationExtractionContext, RuleBasedObservationExtractor,
};
use crate::retrieval::{
    DeterministicPlanner, IndexHandle, KnownEntities, LocalMemoryRetriever, MemoryRetriever,
    RetrievalRequest,
};

/// Per-case retrieval outcome.
#[derive(Debug, Clone)]
pub struct RetrievalCaseResult {
    /// Case name.
    pub name: &'static str,
    /// Records returned, in rank order.
    pub returned: Vec<String>,
    /// Precision over the case's relevant set.
    pub precision: f32,
    /// Recall over the case's relevant set.
    pub recall: f32,
    /// Reciprocal rank of the first relevant record.
    pub reciprocal_rank: f32,
    /// Whether the context the case produced matched the expectation: facts
    /// for a case that needs memory, nothing for one that does not.
    pub skip_correct: bool,
    /// Whether any forbidden record was returned.
    pub leaked_forbidden: bool,
    /// Tokens the assembled context cost.
    pub tokens: usize,
}

/// Aggregate retrieval report.
#[derive(Debug, Clone)]
pub struct RetrievalReport {
    /// Per-case detail.
    pub cases: Vec<RetrievalCaseResult>,
    /// Mean precision across cases that expected memory.
    pub precision: f32,
    /// Mean recall across cases that expected memory.
    pub recall: f32,
    /// Mean reciprocal rank.
    pub mrr: f32,
    /// Fraction of cases whose skip decision was right.
    pub skip_accuracy: f32,
    /// Mean context size in tokens.
    pub mean_tokens: f32,
    /// 95th-percentile context size in tokens.
    pub p95_tokens: f32,
}

impl RetrievalReport {
    /// Cases where a forbidden record surfaced.
    pub fn leaks(&self) -> Vec<&'static str> {
        self.cases
            .iter()
            .filter(|c| c.leaked_forbidden)
            .map(|c| c.name)
            .collect()
    }

    /// A one-line-per-case rendering, for eyeballing a run.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for case in &self.cases {
            let _ = writeln!(
                out,
                "{:<44} p={:.2} r={:.2} rr={:.2} tokens={:<4} {}",
                case.name,
                case.precision,
                case.recall,
                case.reciprocal_rank,
                case.tokens,
                if case.skip_correct { "" } else { "SKIP-WRONG" }
            );
        }
        let _ = writeln!(
            out,
            "\nprecision={:.3} recall={:.3} mrr={:.3} skip={:.3} tokens(mean)={:.1} tokens(p95)={:.1}",
            self.precision,
            self.recall,
            self.mrr,
            self.skip_accuracy,
            self.mean_tokens,
            self.p95_tokens
        );
        out
    }
}

/// Build a retriever over the evaluation corpus.
fn eval_retriever() -> (LocalMemoryRetriever, Arc<DeterministicPlanner>) {
    let records = corpus();
    let index = MemoryIndex::build(
        records
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .map(IndexedMemory::from_canonical),
    );
    let planner = Arc::new(DeterministicPlanner::with_entities(
        KnownEntities::from_index(&index),
    ));
    let canonical = Arc::new(IndexHandle::with_index(index));
    let overlay = Arc::new(IndexHandle::new());
    (
        LocalMemoryRetriever::new(canonical, overlay, RetrievalConfig::default()),
        planner,
    )
}

/// Run the retrieval evaluation.
pub async fn run_retrieval_eval() -> Result<RetrievalReport, MemoryError> {
    let (retriever, planner) = eval_retriever();
    let mut results = Vec::new();

    for case in retrieval_cases() {
        results.push(run_retrieval_case(&retriever, &planner, &case).await?);
    }

    let recall_cases: Vec<&RetrievalCaseResult> = results
        .iter()
        .zip(retrieval_cases())
        .filter(|(_, case)| case.expects_memory)
        .map(|(result, _)| result)
        .collect();

    let precision = metrics::mean(&recall_cases.iter().map(|c| c.precision).collect::<Vec<_>>());
    let recall = metrics::mean(&recall_cases.iter().map(|c| c.recall).collect::<Vec<_>>());
    let mrr = metrics::mean(
        &recall_cases
            .iter()
            .map(|c| c.reciprocal_rank)
            .collect::<Vec<_>>(),
    );
    let skip_accuracy = metrics::mean(
        &results
            .iter()
            .map(|c| if c.skip_correct { 1.0 } else { 0.0 })
            .collect::<Vec<_>>(),
    );
    let mut tokens: Vec<f32> = results.iter().map(|c| c.tokens as f32).collect();
    let mean_tokens = metrics::mean(&tokens);
    let p95_tokens = metrics::percentile(&mut tokens, 95.0);

    Ok(RetrievalReport {
        cases: results,
        precision,
        recall,
        mrr,
        skip_accuracy,
        mean_tokens,
        p95_tokens,
    })
}

async fn run_retrieval_case(
    retriever: &LocalMemoryRetriever,
    planner: &DeterministicPlanner,
    case: &RetrievalCase,
) -> Result<RetrievalCaseResult, MemoryError> {
    let now = Utc::now();
    let plan = planner.plan(case.query, TurnId(1), 1, now);
    let snapshot = retriever.prepare(RetrievalRequest { plan, now }).await?;

    let returned: Vec<String> = snapshot
        .facts
        .iter()
        .map(|f| f.memory_id.to_string())
        .collect();
    let relevant: Vec<String> = case.relevant.iter().map(|r| (*r).to_string()).collect();
    let forbidden: Vec<String> = case.forbidden.iter().map(|r| (*r).to_string()).collect();

    Ok(RetrievalCaseResult {
        name: case.name,
        precision: metrics::precision(&returned, &relevant),
        recall: metrics::recall(&returned, &relevant),
        reciprocal_rank: metrics::reciprocal_rank(&returned, &relevant),
        // The observable contract, not the internal flag. A question that
        // needs no memory must surface no facts; whether the planner declined
        // to search or searched and scored nothing is invisible from outside,
        // and only one of those two is a decision the planner can get right.
        skip_correct: returned.is_empty() != case.expects_memory,
        leaked_forbidden: returned.iter().any(|r| forbidden.contains(r)),
        tokens: usize::from(snapshot.token_count),
        returned,
    })
}

/// Per-case ingestion outcome.
#[derive(Debug, Clone)]
pub struct IngestionCaseResult {
    /// Case name.
    pub name: &'static str,
    /// Whether anything was admitted.
    pub stored: bool,
    /// Whether that matched the expectation.
    pub correct: bool,
    /// Why it differed, when it did.
    pub detail: Option<String>,
}

/// Aggregate ingestion report.
#[derive(Debug, Clone)]
pub struct IngestionReport {
    /// Per-case detail.
    pub cases: Vec<IngestionCaseResult>,
    /// Fraction of cases that behaved as specified.
    pub accuracy: f32,
    /// Utterances stored that should not have been.
    pub false_stores: usize,
    /// Utterances not stored that should have been.
    pub missed_stores: usize,
}

impl IngestionReport {
    /// Cases that did not behave as specified.
    pub fn failures(&self) -> Vec<&IngestionCaseResult> {
        self.cases.iter().filter(|c| !c.correct).collect()
    }
}

/// Run the ingestion evaluation.
pub async fn run_ingestion_eval() -> Result<IngestionReport, MemoryError> {
    let extractor = RuleBasedObservationExtractor::new();
    let config = IngestionConfig::default();
    let mut results = Vec::new();
    let mut false_stores = 0;
    let mut missed_stores = 0;

    for case in ingestion_cases() {
        let observations = extractor
            .extract(
                ObservationExtractionContext::user_turn(
                    case.utterance,
                    SessionId::new("ses_eval"),
                    TurnId(1),
                    Utc::now(),
                )
                .attributed_to(case.speaker),
            )
            .await?;

        // Admission is part of ingestion, so a candidate the policy refuses is
        // not "stored" however confidently the extractor produced it.
        let admitted: Vec<_> = observations
            .into_iter()
            .filter(|o| matches!(admit_observation(o, &config), AdmissionVerdict::Accept(_)))
            .collect();

        let stored = !admitted.is_empty();
        let mut detail = None;
        let mut correct = stored == case.stores;

        if stored != case.stores {
            if stored {
                false_stores += 1;
            } else {
                missed_stores += 1;
            }
            detail = Some(format!("expected stored={}, got {stored}", case.stores));
        } else if let Some(observation) = admitted.first() {
            correct = check_expectations(&case, observation, &mut detail);
        }

        results.push(IngestionCaseResult {
            name: case.name,
            stored,
            correct,
            detail,
        });
    }

    let accuracy = metrics::mean(
        &results
            .iter()
            .map(|c| if c.correct { 1.0 } else { 0.0 })
            .collect::<Vec<_>>(),
    );

    Ok(IngestionReport {
        cases: results,
        accuracy,
        false_stores,
        missed_stores,
    })
}

fn check_expectations(
    case: &IngestionCase,
    observation: &crate::core::MemoryObservation,
    detail: &mut Option<String>,
) -> bool {
    if let Some(expected) = case.kind
        && observation.kind != expected
    {
        *detail = Some(format!(
            "expected kind {expected:?}, got {:?}",
            observation.kind
        ));
        return false;
    }
    if let Some(expected) = case.explicitness
        && observation.explicitness != expected
    {
        *detail = Some(format!(
            "expected explicitness {expected:?}, got {:?}",
            observation.explicitness
        ));
        return false;
    }
    true
}

/// The evaluation user, re-exported for callers driving the harness directly.
pub fn user() -> crate::core::UserId {
    eval_user()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance thresholds (§42). Lowering one is a product decision.
    const MIN_PRECISION: f32 = 0.85;
    const MIN_RECALL: f32 = 0.80;
    const MAX_MEAN_TOKENS: f32 = 250.0;
    const MAX_TOKENS: usize = 500;

    #[tokio::test]
    async fn retrieval_meets_its_acceptance_thresholds() {
        let report = run_retrieval_eval().await.unwrap();
        assert!(
            report.precision >= MIN_PRECISION,
            "precision {:.3} below {MIN_PRECISION}\n{}",
            report.precision,
            report.render()
        );
        assert!(
            report.recall >= MIN_RECALL,
            "recall {:.3} below {MIN_RECALL}\n{}",
            report.recall,
            report.render()
        );
    }

    #[tokio::test]
    async fn memory_is_skipped_for_questions_that_do_not_need_it() {
        let report = run_retrieval_eval().await.unwrap();
        assert_eq!(
            report.skip_accuracy,
            1.0,
            "skip decisions were wrong somewhere\n{}",
            report.render()
        );
    }

    #[tokio::test]
    async fn superseded_and_forbidden_records_never_surface() {
        let report = run_retrieval_eval().await.unwrap();
        assert!(
            report.leaks().is_empty(),
            "forbidden records leaked in: {:?}\n{}",
            report.leaks(),
            report.render()
        );
    }

    #[tokio::test]
    async fn context_stays_inside_its_budget() {
        let report = run_retrieval_eval().await.unwrap();
        assert!(
            report.mean_tokens <= MAX_MEAN_TOKENS,
            "mean context {:.1} tokens exceeds {MAX_MEAN_TOKENS}\n{}",
            report.mean_tokens,
            report.render()
        );
        for case in &report.cases {
            assert!(
                case.tokens <= MAX_TOKENS,
                "case `{}` returned {} tokens",
                case.name,
                case.tokens
            );
        }
    }

    #[tokio::test]
    async fn ingestion_behaves_as_specified() {
        let report = run_ingestion_eval().await.unwrap();
        assert_eq!(
            report.accuracy,
            1.0,
            "ingestion failures: {:?}",
            report.failures()
        );
    }

    #[tokio::test]
    async fn nothing_is_stored_that_should_not_be() {
        let report = run_ingestion_eval().await.unwrap();
        assert_eq!(report.false_stores, 0, "{:?}", report.failures());
    }

    #[tokio::test]
    async fn the_report_renders_every_case() {
        let report = run_retrieval_eval().await.unwrap();
        let rendered = report.render();
        for case in &report.cases {
            assert!(rendered.contains(case.name));
        }
        assert!(rendered.contains("precision="));
    }
}
