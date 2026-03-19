//! Tool confirmation — user confirmation for sensitive tool calls.

use serde::{Deserialize, Serialize};

/// Represents a user's confirmation decision for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfirmation {
    /// Optional hint text explaining what needs confirmation.
    pub hint: Option<String>,
    /// Whether the user confirmed the action.
    pub confirmed: bool,
    /// Optional payload with additional context.
    pub payload: Option<serde_json::Value>,
}

impl ToolConfirmation {
    /// Create a confirmed result.
    pub fn confirmed() -> Self {
        Self {
            hint: None,
            confirmed: true,
            payload: None,
        }
    }

    /// Create a denied result with a hint explaining why.
    pub fn denied(hint: impl Into<String>) -> Self {
        Self {
            hint: Some(hint.into()),
            confirmed: false,
            payload: None,
        }
    }

    /// Attach a payload to this confirmation.
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_constructor() {
        let c = ToolConfirmation::confirmed();
        assert!(c.confirmed);
        assert!(c.hint.is_none());
        assert!(c.payload.is_none());
    }

    #[test]
    fn denied_constructor() {
        let c = ToolConfirmation::denied("Too dangerous");
        assert!(!c.confirmed);
        assert_eq!(c.hint.as_deref(), Some("Too dangerous"));
    }

    #[test]
    fn with_payload() {
        let c =
            ToolConfirmation::confirmed().with_payload(serde_json::json!({"reason": "approved"}));
        assert!(c.confirmed);
        assert_eq!(c.payload.unwrap()["reason"], "approved");
    }

    #[test]
    fn serde_roundtrip() {
        let c =
            ToolConfirmation::denied("risky").with_payload(serde_json::json!({"level": "high"}));
        let json = serde_json::to_string(&c).unwrap();
        let parsed: ToolConfirmation = serde_json::from_str(&json).unwrap();
        assert!(!parsed.confirmed);
        assert_eq!(parsed.hint.as_deref(), Some("risky"));
        assert_eq!(parsed.payload.unwrap()["level"], "high");
    }
}
