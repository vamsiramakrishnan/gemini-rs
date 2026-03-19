//! Response types for generateContent.

use serde::{Deserialize, Serialize};

use crate::protocol::types::{
    CitationMetadata, Content, FinishReason, GroundingMetadata, SafetyRating, UsageMetadata,
};

/// Top-level response from generateContent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    /// Response candidates (usually 1).
    #[serde(default)]
    pub candidates: Vec<Candidate>,

    /// Feedback about the prompt (may indicate blocking).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_feedback: Option<PromptFeedback>,

    /// Token usage statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,

    /// Model version that generated the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
}

impl GenerateContentResponse {
    /// Extract the text from the first candidate's first text part.
    ///
    /// Returns `None` if there are no candidates or no text parts.
    pub fn text(&self) -> Option<&str> {
        self.candidates
            .first()
            .and_then(|c| c.content.as_ref())
            .and_then(|content| {
                content.parts.iter().find_map(|part| {
                    if let crate::protocol::types::Part::Text { text } = part {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
            })
    }

    /// Check if the prompt was blocked.
    pub fn is_prompt_blocked(&self) -> bool {
        self.prompt_feedback
            .as_ref()
            .and_then(|f| f.block_reason.as_ref())
            .is_some()
    }

    /// Get the finish reason of the first candidate.
    pub fn finish_reason(&self) -> Option<FinishReason> {
        self.candidates.first().and_then(|c| c.finish_reason)
    }

    /// Get all function calls from the first candidate.
    pub fn function_calls(&self) -> Vec<&crate::protocol::types::FunctionCall> {
        self.candidates
            .first()
            .and_then(|c| c.content.as_ref())
            .map(|content| {
                content
                    .parts
                    .iter()
                    .filter_map(|part| {
                        if let crate::protocol::types::Part::FunctionCall { function_call } = part {
                            Some(function_call)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A single response candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The generated content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,

    /// Why the model stopped generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,

    /// Safety ratings for this candidate.
    #[serde(default)]
    pub safety_ratings: Vec<SafetyRating>,

    /// Citation information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_metadata: Option<CitationMetadata>,

    /// Token count for this candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,

    /// Grounding metadata (when search grounding is used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grounding_metadata: Option<GroundingMetadata>,

    /// Candidate index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// Feedback about the prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFeedback {
    /// If set, the prompt was blocked for this reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<BlockReason>,

    /// Safety ratings for the prompt.
    #[serde(default)]
    pub safety_ratings: Vec<SafetyRating>,
}

/// Reason a prompt was blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlockReason {
    /// Block reason not specified.
    BlockReasonUnspecified,
    /// Blocked due to safety filters.
    Safety,
    /// Blocked for other reasons.
    Other,
    /// Blocked due to blocklist match.
    Blocklist,
    /// Blocked due to prohibited content.
    ProhibitedContent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_response() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hi"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.text().unwrap(), "Hi");
        assert_eq!(resp.finish_reason(), Some(FinishReason::Stop));
        assert!(!resp.is_prompt_blocked());
    }

    #[test]
    fn parse_blocked_prompt() {
        let json = serde_json::json!({
            "candidates": [],
            "promptFeedback": {
                "blockReason": "SAFETY",
                "safetyRatings": []
            }
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        assert!(resp.is_prompt_blocked());
        assert!(resp.text().is_none());
    }

    #[test]
    fn parse_with_function_calls() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "London"}
                        }
                    }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        let fns = resp.function_calls();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "get_weather");
    }

    #[test]
    fn parse_with_usage_metadata() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Ok"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        let usage = resp.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, Some(10));
        assert_eq!(usage.total_token_count, Some(15));
    }

    #[test]
    fn parse_with_safety_ratings() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello"}]
                },
                "finishReason": "STOP",
                "safetyRatings": [{
                    "category": "HARM_CATEGORY_HARASSMENT",
                    "probability": "NEGLIGIBLE"
                }, {
                    "category": "HARM_CATEGORY_HATE_SPEECH",
                    "probability": "LOW"
                }]
            }]
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.candidates[0].safety_ratings.len(), 2);
    }

    #[test]
    fn parse_unknown_finish_reason() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": "x"}]},
                "finishReason": "SOME_FUTURE_REASON"
            }]
        });
        let resp: GenerateContentResponse = serde_json::from_value(json).unwrap();
        assert_eq!(
            resp.finish_reason(),
            Some(FinishReason::FinishReasonUnspecified)
        );
    }
}
