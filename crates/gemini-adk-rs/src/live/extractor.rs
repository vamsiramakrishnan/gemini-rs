//! Turn-windowed extraction — OOB LLM structured data extraction between turns.
//!
//! A `TurnExtractor` runs after each turn completes, taking a window of recent
//! transcript turns and producing a structured JSON value via an out-of-band
//! LLM call.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
use crate::state::State;

use super::phase::Phase;
use super::transcript::TranscriptTurn;

/// Controls WHEN an extractor runs.
///
/// The default is `EveryTurn`. Use `AfterToolCall` when tool calls are the primary state source,
/// `Interval(n)` to reduce extraction frequency, or `OnPhaseChange`
/// to extract only when entering a new conversation phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractionTrigger {
    /// Run on every TurnComplete event (current default).
    EveryTurn,
    /// Run every N TurnComplete events.
    Interval(u32),
    /// Run after tool calls complete.
    AfterToolCall,
    /// Run when a phase transition occurs.
    OnPhaseChange,
    /// Run on GenerationComplete, as soon as the model has generated its
    /// response and before the turn completes. Current Live models send no
    /// GenerationComplete for an interrupted turn.
    OnGenerationComplete,
}

/// How an extracted field should be merged into authoritative session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePolicy {
    /// Keep an existing state value; write only when the target key is absent.
    KeepKnown,
    /// Always overwrite the target state key with the extracted field value.
    Overwrite,
}

/// Predicate used to decide whether an extracted field may be promoted.
pub type PromotionPredicate = Arc<dyn Fn(&State, &Value) -> bool + Send + Sync>;

/// Rule for promoting one raw extraction field into authoritative state.
#[derive(Clone)]
pub struct FieldPromotion {
    /// Field name inside the extractor's JSON object.
    pub field: String,
    /// State key to write when the field is accepted.
    pub state_key: String,
    /// Merge behavior for the target state key.
    pub merge: MergePolicy,
    /// Optional acceptance predicate.
    pub accept: Option<PromotionPredicate>,
}

impl FieldPromotion {
    /// Promote `field` into the same state key using [`MergePolicy::KeepKnown`].
    pub fn keep_known(field: impl Into<String>) -> Self {
        let field = field.into();
        Self {
            state_key: field.clone(),
            field,
            merge: MergePolicy::KeepKnown,
            accept: None,
        }
    }

    /// Promote `field` into the same state key using [`MergePolicy::Overwrite`].
    pub fn overwrite(field: impl Into<String>) -> Self {
        let field = field.into();
        Self {
            state_key: field.clone(),
            field,
            merge: MergePolicy::Overwrite,
            accept: None,
        }
    }

    /// Promote a boolean field only when its extracted value is `true`.
    pub fn true_only(field: impl Into<String>) -> Self {
        Self::overwrite(field).accept_when(|_, value| value.as_bool() == Some(true))
    }

    /// Promote a string field only when its extracted value is non-empty.
    pub fn non_empty(field: impl Into<String>) -> Self {
        Self::overwrite(field)
            .accept_when(|_, value| value.as_str().is_some_and(|s| !s.trim().is_empty()))
    }

    /// Promote into a custom target state key.
    pub fn to(mut self, state_key: impl Into<String>) -> Self {
        self.state_key = state_key.into();
        self
    }

    /// Only accept this promotion when `predicate` returns true.
    ///
    /// This is the escape hatch for application-specific logic:
    /// `FieldPromotion::overwrite("intent").accept_when(|state, value| ...)`.
    pub fn accept_when(
        mut self,
        predicate: impl Fn(&State, &Value) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.accept = Some(Arc::new(predicate));
        self
    }

    /// Add an additional acceptance predicate, preserving any existing predicate.
    pub fn and_accept_when(
        mut self,
        predicate: impl Fn(&State, &Value) -> bool + Send + Sync + 'static,
    ) -> Self {
        let previous = self.accept.take();
        self.accept = Some(Arc::new(move |state, value| {
            previous.as_ref().is_none_or(|accept| accept(state, value)) && predicate(state, value)
        }));
        self
    }

    /// Only promote after the named concept has been presented by a phase.
    pub fn after_presented(self, concept: impl Into<String>) -> Self {
        let concept = concept.into();
        self.and_accept_when(move |state, _| Phase::is_presented(state, &concept))
    }
}

/// Strip markdown code fences from LLM output.
///
/// Handles `` ```json\n...\n``` ``, `` ```\n...\n``` ``, and bare JSON.
fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag (e.g., "json") on the first line
        let rest = rest.trim_start_matches(|c: char| c != '\n');
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        // Strip trailing ```
        let rest = rest.trim_end();
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    }
}

/// Trait for between-turn extraction from transcript windows.
///
/// Implementations receive a window of recent transcript turns and produce
/// a structured JSON value. The processor stores the result in `State`
/// under the extractor's name.
#[async_trait]
pub trait TurnExtractor: Send + Sync {
    /// Name of this extractor (used as the State key).
    fn name(&self) -> &str;

    /// How many recent turns this extractor needs.
    fn window_size(&self) -> usize;

    /// Whether this extractor should run for the current turn.
    ///
    /// Override to skip extraction on trivial turns (e.g., short utterances,
    /// turns without user speech). Default returns `true` (always extract).
    ///
    /// This is checked before launching the async extraction, so returning
    /// `false` avoids an LLM round-trip entirely.
    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        let _ = window;
        true
    }

    /// The trigger mode for this extractor.
    ///
    /// Controls when the extractor runs. Default is `EveryTurn`.
    fn trigger(&self) -> ExtractionTrigger {
        ExtractionTrigger::EveryTurn
    }

    /// Field promotion rules for this extractor.
    ///
    /// When empty, the runtime auto-flattens every top-level non-null field of
    /// the extraction into state under the field's own name. When non-empty,
    /// only these rules can promote raw extraction fields into authoritative
    /// state.
    fn promotion_rules(&self) -> &[FieldPromotion] {
        &[]
    }

    /// Whether this extractor may write the state key `key`.
    ///
    /// The tool lane asks this to pick the extractors to run early when a
    /// guard that reads `key` refuses a tool. The default: a promotion rule
    /// targets `key`, or there are no rules, in which case every field is
    /// promoted under its own name and any key may be written.
    fn may_write(&self, key: &str) -> bool {
        let rules = self.promotion_rules();
        rules.is_empty() || rules.iter().any(|r| r.state_key == key)
    }

    /// Extract structured data from the transcript window.
    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError>;

    /// Extract with access to session `State` — for extractors whose sources
    /// bind arguments from `State` (e.g. async fetch/agent resolvers).
    ///
    /// The default delegates to [`extract`](Self::extract), so transcript-only
    /// extractors need not implement it. The pipeline always calls this method.
    async fn extract_with_state(
        &self,
        window: &[TranscriptTurn],
        state: &State,
    ) -> Result<Value, LlmError> {
        let _ = state;
        self.extract(window).await
    }

    /// An optional agent to run when this extractor's results land in state —
    /// the `on_complete(dispatch(agent))` effect. Fired by the pipeline after
    /// promotion, only when the extractor produced a non-empty object.
    fn on_complete(&self) -> Option<OnComplete> {
        None
    }
}

/// A state mutation or promotion decision produced by the shared extraction reducer.
#[derive(Clone, Debug)]
pub enum PromotionEvent {
    /// A field was promoted and can be observed under this extraction event name.
    Promoted {
        /// Event name.
        name: String,
        /// Promoted value.
        value: Value,
    },
    /// An explicit promotion rule accepted or rejected its field.
    Decision {
        /// Source field.
        field: String,
        /// Destination key.
        state_key: String,
        /// Whether the value was accepted.
        accepted: bool,
        /// Stable explanatory reason.
        reason: String,
        /// Observed value.
        value: Value,
    },
}
/// Apply the same null, merge and acceptance rules for Live and owned task extraction.
pub fn promote_fields(
    extractor: &dyn TurnExtractor,
    name: &str,
    value: &Value,
    state: &State,
) -> Vec<PromotionEvent> {
    let mut events = Vec::new();
    let Some(object) = value.as_object() else {
        return events;
    };
    let rules = extractor.promotion_rules();
    if rules.is_empty() {
        for (field, value) in object {
            if value.is_null() {
                continue;
            }
            let _ = state.set(field, value.clone());
            events.push(PromotionEvent::Promoted {
                name: format!("{name}.{field}"),
                value: value.clone(),
            });
        }
        return events;
    }
    for rule in rules {
        let Some(value) = object.get(&rule.field) else {
            continue;
        };
        let reason = if value.is_null() {
            Some("extracted value was null")
        } else if state
            .get_raw(&rule.state_key)
            .is_some_and(|known| crate::state::equivalent_values(&known, value))
        {
            // Re-stating a known value in other words is not news; writing it
            // would only churn the journal and could read as a correction.
            Some("an equivalent value is already known")
        } else if rule
            .accept
            .as_ref()
            .is_some_and(|accept| !accept(state, value))
        {
            Some("promotion predicate rejected the value")
        } else if matches!(rule.merge, MergePolicy::KeepKnown) && state.contains(&rule.state_key) {
            Some("existing state value was kept")
        } else {
            None
        };
        if reason.is_none() {
            let _ = state.set(&rule.state_key, value.clone());
            let _ = state.set(
                format!("state_meta:{}", rule.state_key),
                serde_json::json!({"source":"extraction", "extractor":name, "field":rule.field}),
            );
            events.push(PromotionEvent::Promoted {
                name: format!("{name}.{}", rule.field),
                value: value.clone(),
            });
        }
        events.push(PromotionEvent::Decision {
            field: rule.field.clone(),
            state_key: rule.state_key.clone(),
            accepted: reason.is_none(),
            reason: reason.unwrap_or("promotion rule accepted the value").into(),
            value: value.clone(),
        });
    }
    events
}

/// A downstream agent fired when an extractor's results land in state.
#[derive(Clone)]
pub struct OnComplete {
    /// The agent to run; it reads its inputs from `State`.
    pub agent: Arc<dyn crate::text::TextAgent>,
    /// How to run it (`Call` awaits inline; `Dispatch`/`Background` detached).
    pub mode: crate::orchestration::AgentMode,
}

/// Appended to every extraction prompt: a field the transcript does not
/// state is null, never a guess.
const NULL_GUIDANCE: &str = "\n\nUse null for any field the transcript does not state. \
     Never guess, and never use placeholders such as \"unknown\", empty strings or zero.";

/// Appended when some fields are already in state: re-reading the window
/// every turn, the model would otherwise re-state known values in new words,
/// and each re-statement would read as a correction.
const KNOWN_GUIDANCE: &str = "\n\nSome fields are already known (listed before the transcript). \
     Return a known field only if the user's latest turn changes it; otherwise return null \
     for it, even if the transcript mentions it again in other words.";

/// The schema with every top-level property allowed to be null, so the
/// model can say "not stated" instead of inventing a value of the right type.
fn nullable_fields(schema: &Value) -> Value {
    let mut schema = schema.clone();
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return schema;
    };
    for property in properties.values_mut() {
        let Some(object) = property.as_object_mut() else {
            continue;
        };
        match object.get_mut("type") {
            Some(Value::String(t)) if t != "null" => {
                let t = t.clone();
                object.insert("type".into(), serde_json::json!([t, "null"]));
            }
            Some(Value::Array(types)) => {
                if !types.iter().any(|t| t == "null") {
                    types.push(Value::String("null".into()));
                }
            }
            Some(_) => {}
            None => {
                let inner = Value::Object(std::mem::take(object));
                object.insert(
                    "anyOf".into(),
                    serde_json::json!([inner, { "type": "null" }]),
                );
            }
        }
    }
    schema
}

/// LLM-backed turn extractor that sends transcript windows to an OOB LLM
/// with a structured extraction prompt.
pub struct LlmExtractor {
    name: String,
    llm: Arc<dyn BaseLlm>,
    prompt: String,
    window_size: usize,
    schema: Option<Value>,
    /// Pre-rendered schema string (computed once at construction)
    schema_str: Option<String>,
    /// Minimum word count in the last user utterance to trigger extraction.
    min_words: usize,
    /// When this extractor should fire.
    trigger: ExtractionTrigger,
    /// Field promotion rules. Empty means every top-level non-null field is
    /// auto-flattened into state under its own name.
    promotion_rules: Vec<FieldPromotion>,
    /// Thinking budget sent with each request; `None` leaves the model's
    /// default.
    thinking_budget: Option<u32>,
    /// Set once the model rejects `thinking_budget`, so it is not sent again.
    budget_rejected: std::sync::atomic::AtomicBool,
}

impl LlmExtractor {
    /// Create a new LLM-backed extractor.
    ///
    /// - `name`: key for storing results in State
    /// - `llm`: the out-of-band LLM to use for extraction
    /// - `prompt`: system instruction describing what to extract
    /// - `window_size`: how many recent turns to include
    pub fn new(
        name: impl Into<String>,
        llm: Arc<dyn BaseLlm>,
        prompt: impl Into<String>,
        window_size: usize,
    ) -> Self {
        Self {
            name: name.into(),
            llm,
            prompt: prompt.into(),
            window_size,
            schema: None,
            schema_str: None,
            min_words: 0,
            trigger: ExtractionTrigger::EveryTurn,
            promotion_rules: Vec::new(),
            thinking_budget: Some(0),
            budget_rejected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Set the thinking budget sent with each extraction request.
    ///
    /// The default is `Some(0)`: extraction reads a few turns, and the turn
    /// pipeline waits for it, so thinking costs seconds of latency on every
    /// turn. Measured on `gemini-flash-latest`, a budget of 0 cut a short
    /// extraction from 7.7 s to 2.3 s with the same results. `None` leaves
    /// the model's default. When a model rejects the budget (some only work
    /// in thinking mode), the extractor retries without it and stops sending
    /// it.
    pub fn with_thinking_budget(mut self, budget: Option<u32>) -> Self {
        self.thinking_budget = budget;
        self
    }

    /// Set the minimum word count in the last user utterance to trigger extraction.
    ///
    /// Turns where the user said fewer than `n` words will skip the LLM call.
    /// Useful for filtering out "uh huh", "ok", "yes" style responses.
    pub fn with_min_words(mut self, n: usize) -> Self {
        self.min_words = n;
        self
    }

    /// Set a JSON Schema for structured output.
    ///
    /// When set, the schema is included in the prompt to guide the LLM
    /// toward producing valid JSON matching the schema.
    pub fn with_schema(mut self, schema: Value) -> Self {
        self.schema_str = serde_json::to_string_pretty(&schema).ok();
        self.schema = Some(schema);
        self
    }

    /// Set the trigger mode for this extractor.
    pub fn with_trigger(mut self, trigger: ExtractionTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Set explicit field promotion rules.
    ///
    /// Once promotion rules are present, top-level fields are no longer
    /// automatically flattened into state; only accepted rules promote.
    pub fn with_promotions(mut self, rules: Vec<FieldPromotion>) -> Self {
        self.promotion_rules = rules;
        self
    }

    /// The values already in state for this extractor's fields, keyed by
    /// field name: the promotion targets when there are rules, otherwise the
    /// schema's properties.
    fn known_fields(&self, state: &State) -> serde_json::Map<String, Value> {
        let pairs: Vec<(String, String)> = if self.promotion_rules.is_empty() {
            self.schema
                .as_ref()
                .and_then(|s| s.get("properties"))
                .and_then(Value::as_object)
                .map(|p| p.keys().map(|k| (k.clone(), k.clone())).collect())
                .unwrap_or_default()
        } else {
            self.promotion_rules
                .iter()
                .map(|r| (r.field.clone(), r.state_key.clone()))
                .collect()
        };
        pairs
            .into_iter()
            .filter_map(|(field, key)| {
                state
                    .get_raw(&key)
                    .filter(|v| !v.is_null())
                    .map(|v| (field, v))
            })
            .collect()
    }

    async fn extract_knowing(
        &self,
        window: &[TranscriptTurn],
        known: &serde_json::Map<String, Value>,
    ) -> Result<Value, LlmError> {
        let transcript = Self::format_transcript(window);
        let (preamble, guidance) = if known.is_empty() {
            (String::new(), "")
        } else {
            (
                format!("Already known: {}\n\n", Value::Object(known.clone())),
                KNOWN_GUIDANCE,
            )
        };
        let mut request = LlmRequest::from_text(format!(
            "{preamble}Transcript:\n{transcript}\nExtract the requested information."
        ));
        request.system_instruction = Some(format!("{}{NULL_GUIDANCE}{guidance}", self.prompt));

        // Use native JSON mode when a schema is available — the API constrains
        // the model to produce valid JSON matching the schema, eliminating
        // markdown fences and malformed output. Every field may be null:
        // constrained to a bare type, the model fills fields the transcript
        // never mentions with placeholders ("unknown", "", 0).
        if let Some(ref schema) = self.schema {
            request.response_mime_type = Some("application/json".to_string());
            request.response_json_schema = Some(nullable_fields(schema));
        } else {
            request.response_mime_type = Some("application/json".to_string());
        }

        let response = self.generate(request).await?;
        let text = response.text();

        // Fallback: strip markdown code fences if the model still wraps output
        let cleaned = strip_code_fences(&text);

        serde_json::from_str(cleaned).map_err(|e| {
            LlmError::Other(format!(
                "Failed to parse extraction result as JSON: {e}. Raw: {text}"
            ))
        })
    }

    /// Send `request` with the thinking budget, falling back without it if
    /// the model rejects it, and retrying once on a transient error: a lost
    /// extraction loses what the caller said in that turn.
    async fn generate(&self, mut request: LlmRequest) -> Result<LlmResponse, LlmError> {
        use std::sync::atomic::Ordering;
        let budget = self
            .thinking_budget
            .filter(|_| !self.budget_rejected.load(Ordering::Relaxed));
        request.thinking_budget = budget;
        match self.llm.generate(request.clone()).await {
            Err(LlmError::Api { status: 400, .. }) if budget.is_some() => {
                tracing::warn!(
                    extractor = %self.name,
                    "the extraction model rejected thinking budget {budget:?}; \
                     sending requests without it"
                );
                self.budget_rejected.store(true, Ordering::Relaxed);
                request.thinking_budget = None;
                self.llm.generate(request).await
            }
            Err(e) if e.is_retryable() => {
                tracing::warn!(extractor = %self.name, "extraction failed ({e}); retrying once");
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                self.llm.generate(request).await
            }
            other => other,
        }
    }

    /// Format transcript turns for the LLM prompt.
    fn format_transcript(window: &[TranscriptTurn]) -> String {
        let mut out = String::new();
        for turn in window {
            if !turn.user.is_empty() {
                out.push_str("User: ");
                out.push_str(turn.user.trim());
                out.push('\n');
            }
            if !turn.model.is_empty() {
                out.push_str("Assistant: ");
                out.push_str(turn.model.trim());
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }
}

#[async_trait]
impl TurnExtractor for LlmExtractor {
    fn name(&self) -> &str {
        &self.name
    }

    fn window_size(&self) -> usize {
        self.window_size
    }

    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        // A turn the caller said nothing in, such as the model speaking a
        // tool's result, holds no new caller information, and the turn
        // pipeline waits for every extraction: in live runs each one held up
        // the next tool call by seconds. Only when the window shows the
        // caller's words are transcribed at all, so a session without input
        // transcription still extracts.
        if self.trigger == ExtractionTrigger::EveryTurn
            && window.last().is_some_and(|t| t.user.trim().is_empty())
            && window.iter().any(|t| !t.user.trim().is_empty())
        {
            return false;
        }
        if self.min_words == 0 {
            return true;
        }
        // Check the last user utterance
        window
            .iter()
            .rev()
            .find(|t| !t.user.is_empty())
            .is_some_and(|t| t.user.split_whitespace().count() >= self.min_words)
    }

    fn trigger(&self) -> ExtractionTrigger {
        self.trigger.clone()
    }

    fn promotion_rules(&self) -> &[FieldPromotion] {
        &self.promotion_rules
    }

    fn may_write(&self, key: &str) -> bool {
        // Without rules every top-level field is promoted under its own
        // name; the schema says which fields there are.
        if self.promotion_rules.is_empty()
            && let Some(properties) = self
                .schema
                .as_ref()
                .and_then(|s| s.get("properties"))
                .and_then(Value::as_object)
        {
            return properties.contains_key(key);
        }
        self.promotion_rules.is_empty() || self.promotion_rules.iter().any(|r| r.state_key == key)
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        self.extract_knowing(window, &serde_json::Map::new()).await
    }

    /// Extract, telling the model which fields are already known so it
    /// returns only what the latest turn adds or changes.
    async fn extract_with_state(
        &self,
        window: &[TranscriptTurn],
        state: &State,
    ) -> Result<Value, LlmError> {
        self.extract_knowing(window, &self.known_fields(state))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmResponse;
    use gemini_genai_rs::prelude::{Content, Part, Role};
    use std::time::Instant;

    struct MockLlm {
        response: String,
    }

    #[async_trait]
    impl BaseLlm for MockLlm {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn generate(&self, _request: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: self.response.clone(),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    fn make_turns(pairs: &[(&str, &str)]) -> Vec<TranscriptTurn> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (user, model))| TranscriptTurn {
                turn_number: i as u32,
                user: user.to_string(),
                model: model.to_string(),
                tool_calls: Vec::new(),
                timestamp: Instant::now(),
            })
            .collect()
    }

    #[tokio::test]
    async fn llm_extractor_produces_json() {
        let llm = Arc::new(MockLlm {
            response: r#"{"phase": "ordering", "items": ["pizza"]}"#.to_string(),
        });

        let extractor = LlmExtractor::new("OrderState", llm, "Extract order state", 3);

        let turns = make_turns(&[
            ("I'd like a pizza", "Great! What size?"),
            ("Large please", "Coming right up!"),
        ]);

        let result = extractor.extract(&turns).await.unwrap();
        assert_eq!(result["phase"], "ordering");
        assert_eq!(result["items"][0], "pizza");
    }

    #[tokio::test]
    async fn llm_extractor_with_schema() {
        let llm = Arc::new(MockLlm {
            response: r#"{"sentiment": "positive", "score": 0.9}"#.to_string(),
        });

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "sentiment": {"type": "string", "enum": ["positive", "neutral", "negative"]},
                "score": {"type": "number"}
            }
        });

        let extractor =
            LlmExtractor::new("Sentiment", llm, "Rate sentiment", 1).with_schema(schema);

        let turns = make_turns(&[("This is great!", "Glad you think so!")]);
        let result = extractor.extract(&turns).await.unwrap();
        assert_eq!(result["sentiment"], "positive");
    }

    #[tokio::test]
    async fn llm_extractor_invalid_json_returns_error() {
        let llm = Arc::new(MockLlm {
            response: "not json at all".to_string(),
        });

        let extractor = LlmExtractor::new("Bad", llm, "Extract", 1);
        let turns = make_turns(&[("hi", "hello")]);
        let result = extractor.extract(&turns).await;
        assert!(result.is_err());
    }

    #[test]
    fn format_transcript_readable() {
        let turns = make_turns(&[("Hello", "Hi there!"), ("How are you?", "I'm doing well")]);
        let formatted = LlmExtractor::format_transcript(&turns);
        assert!(formatted.contains("User: Hello"));
        assert!(formatted.contains("Assistant: Hi there!"));
        assert!(formatted.contains("User: How are you?"));
    }

    #[tokio::test]
    async fn llm_extractor_handles_markdown_fenced_json() {
        let llm = Arc::new(MockLlm {
            response: "```json\n{\"status\": \"ok\"}\n```".to_string(),
        });

        let extractor = LlmExtractor::new("Fenced", llm, "Extract", 1);
        let turns = make_turns(&[("test", "reply")]);
        let result = extractor.extract(&turns).await.unwrap();
        assert_eq!(result["status"], "ok");
    }

    #[test]
    fn strip_code_fences_variants() {
        assert_eq!(super::strip_code_fences("```json\n{}\n```"), "{}");
        assert_eq!(super::strip_code_fences("```\n{}\n```"), "{}");
        assert_eq!(
            super::strip_code_fences("  ```json\n{\"a\":1}\n```  "),
            "{\"a\":1}"
        );
        assert_eq!(
            super::strip_code_fences("{\"bare\":true}"),
            "{\"bare\":true}"
        );
    }

    #[test]
    fn every_field_of_the_extraction_schema_may_be_null() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "party_size": { "type": "integer" },
                "slot": { "type": ["string"] },
                "kind": { "enum": ["a", "b"] },
                "note": { "type": ["string", "null"] }
            }
        });
        let nullable = nullable_fields(&schema);
        let props = &nullable["properties"];
        assert_eq!(
            props["party_size"]["type"],
            serde_json::json!(["integer", "null"])
        );
        assert_eq!(props["slot"]["type"], serde_json::json!(["string", "null"]));
        assert_eq!(
            props["kind"]["anyOf"],
            serde_json::json!([{ "enum": ["a", "b"] }, { "type": "null" }])
        );
        assert_eq!(props["note"]["type"], serde_json::json!(["string", "null"]));
        // A schema without properties is left alone.
        assert_eq!(
            nullable_fields(&serde_json::json!({ "type": "object" })),
            serde_json::json!({ "type": "object" })
        );
    }

    #[tokio::test]
    async fn the_extraction_request_allows_and_asks_for_null() {
        struct Capture(parking_lot::Mutex<Option<LlmRequest>>);
        #[async_trait]
        impl BaseLlm for Capture {
            fn model_id(&self) -> &str {
                "capture"
            }
            async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
                *self.0.lock() = Some(request);
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text { text: "{}".into() }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }
        let llm = Arc::new(Capture(parking_lot::Mutex::new(None)));
        let extractor = LlmExtractor::new("booking", llm.clone(), "Extract the booking.", 3)
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": { "party_size": { "type": "integer" } }
            }));
        extractor
            .extract(&make_turns(&[("hi", "hello")]))
            .await
            .unwrap();
        let request = llm.0.lock().take().unwrap();
        assert_eq!(
            request.response_json_schema.unwrap()["properties"]["party_size"]["type"],
            serde_json::json!(["integer", "null"])
        );
        let system = request.system_instruction.unwrap();
        assert!(system.starts_with("Extract the booking."));
        assert!(system.contains("Use null for any field the transcript does not state"));
    }

    #[tokio::test]
    async fn known_fields_are_sent_with_the_transcript() {
        struct Capture(parking_lot::Mutex<Vec<LlmRequest>>);
        #[async_trait]
        impl BaseLlm for Capture {
            fn model_id(&self) -> &str {
                "capture"
            }
            async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
                self.0.lock().push(request);
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text { text: "{}".into() }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            }
        }
        let llm = Arc::new(Capture(parking_lot::Mutex::new(Vec::new())));
        let extractor = LlmExtractor::new("booking", llm.clone(), "Extract the booking.", 3)
            .with_promotions(vec![
                FieldPromotion::overwrite("party_size"),
                FieldPromotion::overwrite("slot").to("requested_slot"),
            ]);
        let state = State::new();
        let window = make_turns(&[("seven is perfect", "great")]);

        // Nothing known yet: no preamble and no known-field guidance.
        extractor.extract_with_state(&window, &state).await.unwrap();
        // Known values are listed by field name, read from their state keys.
        let _ = state.set("party_size", 4);
        let _ = state.set("requested_slot", "tomorrow at 7 pm");
        extractor.extract_with_state(&window, &state).await.unwrap();

        let requests = llm.0.lock();
        let text = |r: &LlmRequest| {
            r.contents[0]
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>()
        };
        assert!(text(&requests[0]).starts_with("Transcript:"));
        assert!(
            !requests[0]
                .system_instruction
                .as_ref()
                .unwrap()
                .contains("already known")
        );
        let second = text(&requests[1]);
        assert!(second.starts_with("Already known: "), "{second}");
        assert!(
            second.contains(r#""party_size":4"#) && second.contains(r#""slot":"tomorrow at 7 pm""#)
        );
        assert!(
            requests[1]
                .system_instruction
                .as_ref()
                .unwrap()
                .contains("Some fields are already known")
        );
    }

    #[test]
    fn a_restated_value_is_not_promoted_again() {
        let llm = Arc::new(MockLlm {
            response: "{}".into(),
        });
        let extractor = LlmExtractor::new("booking", llm, "x", 3)
            .with_promotions(vec![FieldPromotion::overwrite("slot")]);
        let state = State::new();
        let _ = state.set("slot", "tomorrow at 7 pm");
        let before = state.get_raw("slot");
        let events = promote_fields(
            &extractor,
            "booking",
            &serde_json::json!({ "slot": "Tomorrow at 7 PM." }),
            &state,
        );
        assert_eq!(state.get_raw("slot"), before, "the known wording is kept");
        assert!(events.iter().any(|e| matches!(
            e,
            PromotionEvent::Decision { accepted: false, reason, .. }
                if reason == "an equivalent value is already known"
        )));

        // A different value is still promoted.
        promote_fields(
            &extractor,
            "booking",
            &serde_json::json!({ "slot": "8 pm" }),
            &state,
        );
        assert_eq!(state.get::<String>("slot").as_deref(), Some("8 pm"));
    }

    #[test]
    fn extractor_name_and_window_size() {
        let llm = Arc::new(MockLlm {
            response: "{}".to_string(),
        });
        let ext = LlmExtractor::new("TestExtractor", llm, "test", 5);
        assert_eq!(ext.name(), "TestExtractor");
        assert_eq!(ext.window_size(), 5);
    }

    #[test]
    fn extractor_default_trigger_is_every_turn() {
        let llm = Arc::new(MockLlm {
            response: "{}".to_string(),
        });
        let ext = LlmExtractor::new("Test", llm, "test", 5);
        assert_eq!(ext.trigger(), ExtractionTrigger::EveryTurn);
    }

    #[test]
    fn extractor_with_trigger() {
        let llm = Arc::new(MockLlm {
            response: "{}".to_string(),
        });
        let ext = LlmExtractor::new("Test", llm, "test", 5)
            .with_trigger(ExtractionTrigger::AfterToolCall);
        assert_eq!(ext.trigger(), ExtractionTrigger::AfterToolCall);
    }

    /// Answers with each scripted result in turn and records every request.
    struct Scripted {
        replies: parking_lot::Mutex<Vec<Result<&'static str, LlmError>>>,
        requests: parking_lot::Mutex<Vec<LlmRequest>>,
    }

    impl Scripted {
        fn new(replies: Vec<Result<&'static str, LlmError>>) -> Arc<Self> {
            Arc::new(Self {
                replies: parking_lot::Mutex::new(replies),
                requests: parking_lot::Mutex::new(Vec::new()),
            })
        }
        fn budgets(&self) -> Vec<Option<u32>> {
            self.requests
                .lock()
                .iter()
                .map(|r| r.thinking_budget)
                .collect()
        }
    }

    #[async_trait]
    impl BaseLlm for Scripted {
        fn model_id(&self) -> &str {
            "scripted"
        }
        async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.requests.lock().push(request);
            let text = self.replies.lock().remove(0)?;
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text { text: text.into() }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    fn bad_request() -> LlmError {
        LlmError::Api {
            status: 400,
            message: "Budget 0 is invalid. This model only works in thinking mode.".into(),
        }
    }

    #[tokio::test]
    async fn extraction_does_not_think_by_default() {
        let llm = Scripted::new(vec![Ok("{}")]);
        let extractor = LlmExtractor::new("x", llm.clone(), "Extract.", 2);
        extractor
            .extract(&make_turns(&[("hi", "hello")]))
            .await
            .unwrap();
        assert_eq!(llm.budgets(), [Some(0)]);

        let llm = Scripted::new(vec![Ok("{}")]);
        let extractor =
            LlmExtractor::new("x", llm.clone(), "Extract.", 2).with_thinking_budget(None);
        extractor
            .extract(&make_turns(&[("hi", "hello")]))
            .await
            .unwrap();
        assert_eq!(llm.budgets(), [None], "None leaves the model's default");
    }

    #[tokio::test]
    async fn a_rejected_thinking_budget_is_dropped_for_good() {
        let llm = Scripted::new(vec![Err(bad_request()), Ok(r#"{"a": 1}"#), Ok("{}")]);
        let extractor = LlmExtractor::new("x", llm.clone(), "Extract.", 2);
        let window = make_turns(&[("hi", "hello")]);
        assert_eq!(extractor.extract(&window).await.unwrap()["a"], 1);
        extractor.extract(&window).await.unwrap();
        assert_eq!(llm.budgets(), [Some(0), None, None]);
    }

    #[tokio::test]
    async fn a_bad_request_without_a_budget_is_not_retried() {
        let llm = Scripted::new(vec![Err(bad_request())]);
        let extractor =
            LlmExtractor::new("x", llm.clone(), "Extract.", 2).with_thinking_budget(None);
        assert!(
            extractor
                .extract(&make_turns(&[("hi", "hello")]))
                .await
                .is_err()
        );
        assert_eq!(llm.requests.lock().len(), 1);
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried_once() {
        let unavailable = || LlmError::Api {
            status: 503,
            message: "Service Unavailable".into(),
        };
        let llm = Scripted::new(vec![Err(unavailable()), Ok(r#"{"a": 1}"#)]);
        let extractor = LlmExtractor::new("x", llm.clone(), "Extract.", 2);
        let window = make_turns(&[("hi", "hello")]);
        assert_eq!(extractor.extract(&window).await.unwrap()["a"], 1);

        let llm = Scripted::new(vec![Err(unavailable()), Err(unavailable())]);
        let extractor = LlmExtractor::new("x", llm.clone(), "Extract.", 2);
        assert!(extractor.extract(&window).await.is_err());
        assert_eq!(llm.requests.lock().len(), 2, "one retry, not more");
    }

    #[test]
    fn may_write_follows_the_rules_or_the_schema() {
        let llm = Scripted::new(vec![]);
        let with_rules = LlmExtractor::new("x", llm.clone(), "Extract.", 2)
            .with_promotions(vec![FieldPromotion::true_only("ok").to("confirmed")]);
        assert!(with_rules.may_write("confirmed"));
        assert!(!with_rules.may_write("ok"));

        let flattened = LlmExtractor::new("x", llm.clone(), "Extract.", 2).with_schema(
            serde_json::json!({ "type": "object", "properties": { "confirmed": { "type": "boolean" } } }),
        );
        assert!(flattened.may_write("confirmed"));
        assert!(!flattened.may_write("slot"));

        let unknown = LlmExtractor::new("x", llm, "Extract.", 2);
        assert!(
            unknown.may_write("anything"),
            "no schema, no rules: it may write any key"
        );
    }

    #[test]
    fn a_turn_without_the_caller_is_not_extracted() {
        let llm = Scripted::new(vec![]);
        let extractor = LlmExtractor::new("x", llm, "Extract.", 3);
        // The model speaks a tool's result: nothing new from the caller.
        assert!(!extractor.should_extract(&make_turns(&[
            ("Four at seven.", "Let me check."),
            ("", "Seven is free."),
        ])));
        assert!(extractor.should_extract(&make_turns(&[
            ("", "Seven is free."),
            ("Book it.", "Done."),
        ])));
        // No caller words anywhere: transcription may be off, so extract.
        assert!(extractor.should_extract(&make_turns(&[("", "Hello.")])));
        // Extractors that read the model's own words still run.
        let on_generation = LlmExtractor::new("y", Scripted::new(vec![]), "Extract.", 3)
            .with_trigger(ExtractionTrigger::OnGenerationComplete);
        assert!(on_generation.should_extract(&make_turns(&[
            ("Four at seven.", "Let me check."),
            ("", "Seven is free."),
        ])));
    }
}
