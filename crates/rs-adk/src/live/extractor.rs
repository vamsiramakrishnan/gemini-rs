//! Turn-windowed extraction — OOB LLM structured data extraction between turns.
//!
//! A `TurnExtractor` runs after each turn completes, taking a window of recent
//! transcript turns and producing a structured JSON value via an out-of-band
//! LLM call.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::llm::{BaseLlm, LlmError, LlmRequest};

use super::transcript::TranscriptTurn;

/// Controls WHEN an extractor runs.
///
/// The default is `EveryTurn`, which preserves backward compatibility.
/// Use `AfterToolCall` when tool calls are the primary state source,
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
    /// Run on GenerationComplete — before interruption truncation.
    ///
    /// Use this to extract from the model's full intended output, even if
    /// the user barged in and the audio delivery was interrupted.
    OnGenerationComplete,
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

    /// Extract structured data from the transcript window.
    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError>;
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
        }
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

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        let transcript = Self::format_transcript(window);

        let mut request = LlmRequest::from_text(format!(
            "Transcript:\n{transcript}\nExtract the requested information."
        ));
        request.system_instruction = Some(self.prompt.clone());

        // Use native JSON mode when a schema is available — the API constrains
        // the model to produce valid JSON matching the schema, eliminating
        // markdown fences and malformed output.
        if let Some(ref schema) = self.schema {
            request.response_mime_type = Some("application/json".to_string());
            request.response_json_schema = Some(schema.clone());
        } else {
            request.response_mime_type = Some("application/json".to_string());
        }

        let response = self.llm.generate(request).await?;
        let text = response.text();

        // Fallback: strip markdown code fences if the model still wraps output
        let cleaned = strip_code_fences(&text);

        serde_json::from_str(cleaned).map_err(|e| {
            LlmError::Other(format!(
                "Failed to parse extraction result as JSON: {e}. Raw: {text}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmResponse;
    use rs_genai::prelude::{Content, Part, Role};
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
}
