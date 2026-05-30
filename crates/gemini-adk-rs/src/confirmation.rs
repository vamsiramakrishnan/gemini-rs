//! Tool confirmation — user confirmation for sensitive tool calls.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
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

/// A request for confirmation of a sensitive tool call, handed to a
/// [`ConfirmationProvider`] before the tool executes.
#[derive(Debug, Clone)]
pub struct ConfirmationRequest {
    /// The tool about to run.
    pub tool_name: String,
    /// The arguments the model supplied.
    pub args: serde_json::Value,
    /// Optional hint describing what needs confirming (from the tool's policy).
    pub message: Option<String>,
}

/// Decides whether a confirmation-gated tool call may proceed.
///
/// Wire one into a [`ToolDispatcher`](crate::tool::ToolDispatcher) via
/// [`with_confirmation_provider`](crate::tool::ToolDispatcher::with_confirmation_provider).
/// When a tool reports [`ToolFunction::requires_confirmation`](crate::tool::ToolFunction::requires_confirmation)
/// (e.g. one built with `T::confirm(..)`), the dispatcher consults the provider
/// before executing and returns an error if it is denied. Enforcement is
/// opt-in: with no provider configured, confirmation-gated tools run normally.
#[async_trait]
pub trait ConfirmationProvider: Send + Sync {
    /// Resolve a confirmation decision for the given request.
    async fn confirm(&self, request: ConfirmationRequest) -> ToolConfirmation;
}

/// Blanket impl so a plain async closure can act as a [`ConfirmationProvider`]:
///
/// ```rust,ignore
/// dispatcher.set_confirmation_provider(std::sync::Arc::new(
///     |req: ConfirmationRequest| async move {
///         if req.tool_name == "delete_account" {
///             ToolConfirmation::denied("blocked by policy")
///         } else {
///             ToolConfirmation::confirmed()
///         }
///     },
/// ));
/// ```
#[async_trait]
impl<F, Fut> ConfirmationProvider for F
where
    F: Fn(ConfirmationRequest) -> Fut + Send + Sync,
    Fut: Future<Output = ToolConfirmation> + Send,
{
    async fn confirm(&self, request: ConfirmationRequest) -> ToolConfirmation {
        self(request).await
    }
}

/// A [`ConfirmationProvider`] that approves or denies every request uniformly —
/// handy for tests and "deny-all" / "allow-all" defaults.
pub struct StaticConfirmation {
    confirmed: bool,
    hint: Option<String>,
}

impl StaticConfirmation {
    /// Approve every confirmation request.
    pub fn allow_all() -> Arc<dyn ConfirmationProvider> {
        Arc::new(Self {
            confirmed: true,
            hint: None,
        })
    }

    /// Deny every confirmation request with an optional hint.
    pub fn deny_all(hint: impl Into<String>) -> Arc<dyn ConfirmationProvider> {
        Arc::new(Self {
            confirmed: false,
            hint: Some(hint.into()),
        })
    }
}

#[async_trait]
impl ConfirmationProvider for StaticConfirmation {
    async fn confirm(&self, _request: ConfirmationRequest) -> ToolConfirmation {
        ToolConfirmation {
            hint: self.hint.clone(),
            confirmed: self.confirmed,
            payload: None,
        }
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
