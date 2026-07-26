//! The out-of-band retrieval-plan extractor seam.
//!
//! Plan extraction is a small structured-output model call that runs *after*
//! the final transcript, while the model is already speaking. It never sits on
//! the response path, so its failure mode is "slightly worse retrieval", not
//! "slower conversation".

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;

use super::deterministic::DeterministicPlanner;
use super::plan::RetrievalPlan;
use crate::core::{MemoryError, TurnId};

/// The system instruction for the plan extractor.
///
/// The prohibitions matter as much as the task: a model asked to plan retrieval
/// will otherwise answer the user, invent memories, or treat its own previous
/// output as something the user said.
pub const RETRIEVAL_PLAN_INSTRUCTION: &str = "\
You produce a retrieval plan for a personal memory system. Given the user's \
most recent utterance and a little surrounding conversation, decide which of \
the user's existing stored memories might be relevant to answering them.

Return only the structured plan. Specifically:
- Do NOT answer the user's question.
- Do NOT propose anything to remember; that is a separate task.
- Do NOT treat the assistant's own statements as facts about the user.
- Do NOT infer sensitive attributes (health, religion, politics, sexuality) \
  that the user did not state.
- Set requires_memory to false for generic factual, visual or world-knowledge \
  questions that do not depend on this user's history.
- Prefer few, specific search terms over many broad ones.";

/// What the extractor is given.
#[derive(Debug, Clone)]
pub struct RetrievalExtractionContext {
    /// The finalized user utterance.
    pub transcript: String,
    /// Up to four preceding user turns, oldest first.
    pub recent_user_turns: Vec<String>,
    /// Up to two preceding assistant turns, for reference resolution only.
    pub recent_assistant_turns: Vec<String>,
    /// Entities already known in this session.
    pub known_entities: Vec<String>,
    /// The rule-based plan, as a starting point.
    pub deterministic: RetrievalPlan,
    /// The turn being planned for.
    pub turn_id: TurnId,
    /// The generation the request was issued at.
    pub generation: u64,
    /// Evaluation time.
    pub now: DateTime<Utc>,
}

impl RetrievalExtractionContext {
    /// Render the context as a prompt body.
    pub fn to_prompt(&self) -> String {
        let mut out = String::new();
        if !self.recent_user_turns.is_empty() {
            out.push_str("Earlier user turns:\n");
            for turn in &self.recent_user_turns {
                out.push_str("- ");
                out.push_str(turn);
                out.push('\n');
            }
        }
        if !self.recent_assistant_turns.is_empty() {
            out.push_str("\nAssistant turns (for reference resolution only):\n");
            for turn in &self.recent_assistant_turns {
                out.push_str("- ");
                out.push_str(turn);
                out.push('\n');
            }
        }
        if !self.known_entities.is_empty() {
            out.push_str("\nKnown entities: ");
            out.push_str(&self.known_entities.join(", "));
            out.push('\n');
        }
        out.push_str("\nCurrent user utterance:\n");
        out.push_str(&self.transcript);
        out.push('\n');
        out
    }
}

/// Produces a retrieval plan from conversation context.
#[async_trait]
pub trait RetrievalPlanExtractor: Send + Sync {
    /// Extract a plan.
    async fn extract(
        &self,
        context: RetrievalExtractionContext,
    ) -> Result<RetrievalPlan, MemoryError>;
}

/// The rule-based planner, exposed as an extractor.
///
/// This is the default: it needs no model, it is deterministic, and it is what
/// every other implementation degrades to.
pub struct DeterministicPlanExtractor {
    planner: Arc<DeterministicPlanner>,
}

impl DeterministicPlanExtractor {
    /// Wrap a planner.
    pub fn new(planner: Arc<DeterministicPlanner>) -> Self {
        Self { planner }
    }
}

#[async_trait]
impl RetrievalPlanExtractor for DeterministicPlanExtractor {
    async fn extract(
        &self,
        context: RetrievalExtractionContext,
    ) -> Result<RetrievalPlan, MemoryError> {
        Ok(self.planner.plan(
            &context.transcript,
            context.turn_id,
            context.generation,
            context.now,
        ))
    }
}

/// Runs an extractor under a deadline and falls back to the deterministic plan.
///
/// The deterministic plan is already in the context, so a failed or slow model
/// call costs nothing beyond the deadline itself.
pub struct BoundedPlanExtractor {
    inner: Arc<dyn RetrievalPlanExtractor>,
    timeout: Duration,
}

impl BoundedPlanExtractor {
    /// Bound `inner` to `timeout`.
    pub fn new(inner: Arc<dyn RetrievalPlanExtractor>, timeout: Duration) -> Self {
        Self { inner, timeout }
    }
}

#[async_trait]
impl RetrievalPlanExtractor for BoundedPlanExtractor {
    async fn extract(
        &self,
        context: RetrievalExtractionContext,
    ) -> Result<RetrievalPlan, MemoryError> {
        let fallback = context.deterministic.clone();
        match tokio::time::timeout(self.timeout, self.inner.extract(context)).await {
            Ok(Ok(plan)) => Ok(plan.normalized()),
            Ok(Err(_)) | Err(_) => Ok(fallback),
        }
    }
}

/// The JSON Schema a structured-output model call should be constrained to.
pub fn retrieval_plan_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(RetrievalPlan);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

/// Build an extraction context from a transcript and a rule-based plan.
pub fn context_for(
    planner: &DeterministicPlanner,
    transcript: &str,
    turn_id: TurnId,
    generation: u64,
    now: DateTime<Utc>,
) -> RetrievalExtractionContext {
    RetrievalExtractionContext {
        transcript: transcript.to_string(),
        recent_user_turns: Vec::new(),
        recent_assistant_turns: Vec::new(),
        known_entities: Vec::new(),
        deterministic: planner.plan(transcript, turn_id, generation, now),
        turn_id,
        generation,
        now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::deterministic::KnownEntities;
    use crate::retrieval::plan::RetrievalIntent;

    fn planner() -> Arc<DeterministicPlanner> {
        let mut known = KnownEntities::new();
        known.insert("Rhea", "rhea");
        Arc::new(DeterministicPlanner::with_entities(known))
    }

    fn context(text: &str) -> RetrievalExtractionContext {
        context_for(&planner(), text, TurnId(2), 2, Utc::now())
    }

    struct AlwaysFails;

    #[async_trait]
    impl RetrievalPlanExtractor for AlwaysFails {
        async fn extract(
            &self,
            _context: RetrievalExtractionContext,
        ) -> Result<RetrievalPlan, MemoryError> {
            Err(MemoryError::Extraction("model unavailable".into()))
        }
    }

    struct Hangs;

    #[async_trait]
    impl RetrievalPlanExtractor for Hangs {
        async fn extract(
            &self,
            _context: RetrievalExtractionContext,
        ) -> Result<RetrievalPlan, MemoryError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            unreachable!("the bound should fire first")
        }
    }

    #[tokio::test]
    async fn the_deterministic_extractor_produces_a_usable_plan() {
        let plan = DeterministicPlanExtractor::new(planner())
            .extract(context("what does Rhea like to eat"))
            .await
            .unwrap();
        assert!(plan.requires_memory);
        assert!(!plan.lexical_queries.is_empty());
    }

    #[tokio::test]
    async fn a_failing_model_extractor_degrades_to_the_rule_based_plan() {
        let bounded = BoundedPlanExtractor::new(Arc::new(AlwaysFails), Duration::from_millis(50));
        let plan = bounded
            .extract(context("what does Rhea like to eat"))
            .await
            .unwrap();
        assert!(plan.requires_memory);
        assert_eq!(plan.intent, RetrievalIntent::RelationshipReference);
    }

    #[tokio::test]
    async fn a_hanging_model_extractor_is_abandoned_at_the_deadline() {
        let bounded = BoundedPlanExtractor::new(Arc::new(Hangs), Duration::from_millis(20));
        let started = std::time::Instant::now();
        let plan = bounded
            .extract(context("what does Rhea like to eat"))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(plan.requires_memory);
    }

    #[test]
    fn the_prompt_separates_user_turns_from_assistant_turns() {
        let mut ctx = context("what should we cook");
        ctx.recent_user_turns = vec!["I am pescatarian".into()];
        ctx.recent_assistant_turns = vec!["You mentioned salmon".into()];
        let prompt = ctx.to_prompt();
        assert!(prompt.contains("Earlier user turns:"));
        assert!(prompt.contains("reference resolution only"));
        assert!(prompt.ends_with("what should we cook\n"));
    }

    #[test]
    fn the_instruction_forbids_answering_and_storing() {
        assert!(RETRIEVAL_PLAN_INSTRUCTION.contains("Do NOT answer"));
        assert!(RETRIEVAL_PLAN_INSTRUCTION.contains("separate task"));
    }

    #[test]
    fn the_plan_schema_is_a_json_object_schema() {
        let schema = retrieval_plan_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["requires_memory"].is_object());
    }
}
