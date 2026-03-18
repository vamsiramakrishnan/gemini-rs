//! Custom TurnExtractor implementations for demo apps.
//!
//! Wraps regex-based extraction functions into the TurnExtractor trait
//! so they integrate with the L1 extraction pipeline.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;

use gemini_adk::live::{TranscriptTurn, TurnExtractor};
use gemini_adk::llm::LlmError;

/// A `TurnExtractor` that wraps a regex-based (or any synchronous) extraction
/// function.
///
/// On each call to `extract()`, the extractor:
/// 1. Formats the transcript window into a single text block.
/// 2. Calls `extract_fn` with that text and the previously-accumulated state.
/// 3. Merges newly-returned key-value pairs into the accumulated state.
/// 4. Returns the full accumulated state as a JSON `Value`.
///
/// This lets simple regex functions integrate with the L1 extraction pipeline
/// while preserving state across turns (previously-extracted keys are never lost).
pub struct RegexExtractor {
    name: String,
    window_size: usize,
    extract_fn: Arc<dyn Fn(&str, &HashMap<String, Value>) -> HashMap<String, Value> + Send + Sync>,
    /// Accumulated extracted state carried across turns.
    state: Mutex<HashMap<String, Value>>,
}

impl RegexExtractor {
    /// Create a new `RegexExtractor`.
    ///
    /// - `name`: key for storing results in State (also returned by `TurnExtractor::name()`).
    /// - `window_size`: how many recent transcript turns this extractor needs.
    /// - `extract_fn`: a function `(transcript_text, existing_state) -> new_key_value_pairs`.
    ///   It receives the previously-extracted state so it can skip already-known keys.
    ///   It should return only NEW key-value pairs to merge.
    pub fn new(
        name: impl Into<String>,
        window_size: usize,
        extract_fn: impl Fn(&str, &HashMap<String, Value>) -> HashMap<String, Value>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            window_size,
            extract_fn: Arc::new(extract_fn),
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Format transcript turns into a single text block.
    ///
    /// Output format:
    /// ```text
    /// [User] hello there [Agent] hi how can I help [User] my order is broken [Agent] I'm sorry
    /// ```
    fn format_window(window: &[TranscriptTurn]) -> String {
        let mut out = String::new();
        for turn in window {
            if !turn.user.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str("[User] ");
                out.push_str(turn.user.trim());
            }
            if !turn.model.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str("[Agent] ");
                out.push_str(turn.model.trim());
            }
        }
        out
    }
}

#[async_trait]
impl TurnExtractor for RegexExtractor {
    fn name(&self) -> &str {
        &self.name
    }

    fn window_size(&self) -> usize {
        self.window_size
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        let text = Self::format_window(window);
        let existing = self.state.lock().clone();
        let new_values = (self.extract_fn)(&text, &existing);

        // Merge new values into accumulated state
        {
            let mut state = self.state.lock();
            state.extend(new_values);
        }

        // Return full accumulated state as JSON
        let full = self.state.lock().clone();
        Ok(serde_json::to_value(full).unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use serde_json::json;
    use std::time::Instant;

    fn make_turns(pairs: &[(&str, &str)]) -> Vec<TranscriptTurn> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (user, model))| TranscriptTurn {
                turn_number: i as u32,
                user: user.to_string(),
                model: model.to_string(),
                timestamp: Instant::now(),
                tool_calls: Vec::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn regex_extractor_extracts_from_transcript() {
        // Simple extractor that looks for an order number pattern
        let extractor = RegexExtractor::new("order_info", 5, |text, _existing| {
            let mut result = HashMap::new();
            let re = Regex::new(r"order\s+#?(\d+)").unwrap();
            if let Some(caps) = re.captures(text) {
                result.insert(
                    "order_number".to_string(),
                    json!(caps.get(1).unwrap().as_str()),
                );
            }
            result
        });

        let turns = make_turns(&[
            ("my order #12345 is broken", "I'm sorry to hear that"),
            ("can you fix it?", "Let me look into order #12345"),
        ]);

        let result = extractor.extract(&turns).await.unwrap();
        assert_eq!(result["order_number"], json!("12345"));
    }

    #[tokio::test]
    async fn regex_extractor_accumulates_state() {
        // Extractor that picks up different info on each call
        let extractor = RegexExtractor::new("customer_info", 5, |text, _existing| {
            let mut result = HashMap::new();

            let name_re = Regex::new(r"my name is (\w+)").unwrap();
            if let Some(caps) = name_re.captures(text) {
                result.insert("name".to_string(), json!(caps.get(1).unwrap().as_str()));
            }

            let email_re = Regex::new(r"email is (\S+@\S+)").unwrap();
            if let Some(caps) = email_re.captures(text) {
                result.insert("email".to_string(), json!(caps.get(1).unwrap().as_str()));
            }

            result
        });

        // First call: provides name
        let turns1 = make_turns(&[("my name is Alice", "Nice to meet you Alice")]);
        let result1 = extractor.extract(&turns1).await.unwrap();
        assert_eq!(result1["name"], json!("Alice"));
        assert!(result1.get("email").is_none());

        // Second call: provides email — name should still be present
        let turns2 = make_turns(&[("email is alice@example.com", "Got it, thanks!")]);
        let result2 = extractor.extract(&turns2).await.unwrap();
        assert_eq!(result2["name"], json!("Alice"));
        assert_eq!(result2["email"], json!("alice@example.com"));
    }

    #[tokio::test]
    async fn regex_extractor_skips_existing_keys() {
        // Extractor that only extracts keys not already present
        let extractor = RegexExtractor::new("info", 5, |text, existing| {
            let mut result = HashMap::new();

            // Only extract "topic" if not already known
            if !existing.contains_key("topic") {
                let re = Regex::new(r"topic is (\w+)").unwrap();
                if let Some(caps) = re.captures(text) {
                    result.insert("topic".to_string(), json!(caps.get(1).unwrap().as_str()));
                }
            }

            // Always extract "mood" (updates each turn)
            let re = Regex::new(r"feeling (\w+)").unwrap();
            if let Some(caps) = re.captures(text) {
                result.insert("mood".to_string(), json!(caps.get(1).unwrap().as_str()));
            }

            result
        });

        // First call: both topic and mood
        let turns1 = make_turns(&[("topic is rust and I'm feeling happy", "Great!")]);
        let result1 = extractor.extract(&turns1).await.unwrap();
        assert_eq!(result1["topic"], json!("rust"));
        assert_eq!(result1["mood"], json!("happy"));

        // Second call: topic changes in text but extractor skips it; mood updates
        let turns2 = make_turns(&[("topic is python and I'm feeling excited", "Cool!")]);
        let result2 = extractor.extract(&turns2).await.unwrap();
        // Topic should still be "rust" because the extract_fn skipped it
        assert_eq!(result2["topic"], json!("rust"));
        // Mood should be updated to "excited"
        assert_eq!(result2["mood"], json!("excited"));
    }

    #[test]
    fn format_window_produces_readable_text() {
        let turns = make_turns(&[
            ("hello there", "hi how can I help"),
            ("my order is broken", "I'm sorry"),
        ]);

        let formatted = RegexExtractor::format_window(&turns);
        assert_eq!(
            formatted,
            "[User] hello there [Agent] hi how can I help [User] my order is broken [Agent] I'm sorry"
        );
    }
}
