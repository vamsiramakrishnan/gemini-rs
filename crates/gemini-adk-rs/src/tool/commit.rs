//! Commit governance for a tool that acts on the world: idempotency and
//! compensation.
//!
//! A voice agent that charges a card or books a table can call the same
//! tool twice for one intent. The model retries after a timeout, the user
//! repeats themselves, a resumed session replays. [`CommitGuard`] wraps such
//! a tool:
//!
//! - **Idempotency.** An idempotency key is rendered from a template
//!   (`"{user_id}:{amount}"`). A call whose key already succeeded returns the
//!   first call's result without running the tool again. The result is
//!   remembered in state under [`idempotency_key`]`(tool, key)`.
//! - **Compensation.** When the tool fails, a compensating tool (a refund, a
//!   release) runs with the same arguments to undo any partial effect, and
//!   `compensated:{tool}` is set in state. The failure is still reported to
//!   the model.
//!
//! `Policy::commit(..)` in the fluent crate lowers to this.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::ToolFunction;
use crate::error::ToolError;
use crate::state::State;

/// The state key that remembers a committed call's result.
pub fn idempotency_key(tool: &str, key: &str) -> String {
    format!("idempotency:{tool}:{key}")
}

/// The state key raised after a failed commit was compensated.
pub fn compensated_key(tool: &str) -> String {
    format!("compensated:{tool}")
}

/// A tool wrapped with idempotency and compensation. See the
/// [module docs](self).
pub struct CommitGuard {
    inner: Arc<dyn ToolFunction>,
    state: State,
    key_template: Option<String>,
    compensate: Option<Arc<dyn ToolFunction>>,
}

impl CommitGuard {
    /// Guard `inner`, remembering results in `state`.
    pub fn new(inner: Arc<dyn ToolFunction>, state: State) -> Self {
        Self {
            inner,
            state,
            key_template: None,
            compensate: None,
        }
    }

    /// Deduplicate calls by this key template. `{name}` takes the call's
    /// argument `name`, else the state value at `name`. A call where any
    /// placeholder has no value is not deduplicated (and says so in the
    /// log), so an incomplete key never merges two different commits.
    pub fn idempotency_key(mut self, template: impl Into<String>) -> Self {
        self.key_template = Some(template.into());
        self
    }

    /// Run `tool` with the same arguments when the guarded tool fails.
    pub fn compensate_with(mut self, tool: Arc<dyn ToolFunction>) -> Self {
        self.compensate = Some(tool);
        self
    }

    fn render_key(&self, args: &Value) -> Option<String> {
        let template = self.key_template.as_deref()?;
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after.find('}')?;
            let name = after[..close].trim();
            let value = args
                .get(name)
                .cloned()
                .or_else(|| self.state.get_raw(name))
                .filter(|v| !v.is_null());
            match value {
                Some(Value::String(s)) => out.push_str(&s),
                Some(v) => out.push_str(&v.to_string()),
                None => {
                    tracing::warn!(
                        tool = self.inner.name(),
                        placeholder = name,
                        "idempotency key incomplete; this call is not deduplicated"
                    );
                    return None;
                }
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        Some(out)
    }
}

#[async_trait]
impl ToolFunction for CommitGuard {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Option<Value> {
        self.inner.parameters()
    }

    fn requires_confirmation(&self) -> bool {
        self.inner.requires_confirmation()
    }

    fn confirmation_message(&self) -> Option<&str> {
        self.inner.confirmation_message()
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let tool = self.inner.name();
        let key = self
            .render_key(&args)
            .map(|key| idempotency_key(tool, &key));
        if let Some(key) = &key
            && let Some(previous) = self.state.get_raw(key)
        {
            tracing::info!(tool, "commit already made; returning its result");
            return Ok(previous);
        }
        match self.inner.call(args.clone()).await {
            Ok(result) => {
                if let Some(key) = key {
                    let _ = self.state.set(key, &result);
                }
                Ok(result)
            }
            Err(error) => {
                if let Some(compensate) = &self.compensate {
                    match compensate.call(args).await {
                        Ok(_) => {
                            let _ = self.state.set(compensated_key(tool), true);
                        }
                        Err(e) => tracing::error!(
                            tool,
                            compensating = compensate.name(),
                            "compensation failed: {e}"
                        ),
                    }
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::SimpleTool;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting(name: &'static str, calls: Arc<AtomicUsize>, fail: bool) -> Arc<dyn ToolFunction> {
        Arc::new(SimpleTool::new(name, name, None, move |args| {
            let calls = calls.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if fail {
                    Err(ToolError::ExecutionFailed("card declined".into()))
                } else {
                    Ok(json!({ "charge": n, "amount": args["amount"] }))
                }
            }
        }))
    }

    #[tokio::test]
    async fn the_same_commit_runs_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = State::new();
        let _ = state.set("user_id", "u1");
        let guard = CommitGuard::new(counting("charge", calls.clone(), false), state.clone())
            .idempotency_key("{user_id}:{amount}");

        let first = guard.call(json!({ "amount": 40 })).await.unwrap();
        let again = guard.call(json!({ "amount": 40 })).await.unwrap();
        assert_eq!(first, again, "the retry gets the first charge back");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the card was charged once");

        guard.call(json!({ "amount": 55 })).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a different amount is a new commit"
        );
        assert!(state.get_raw(&idempotency_key("charge", "u1:40")).is_some());
    }

    #[tokio::test]
    async fn an_incomplete_key_never_merges_commits() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = CommitGuard::new(counting("charge", calls.clone(), false), State::new())
            .idempotency_key("{user_id}:{amount}");
        guard.call(json!({ "amount": 40 })).await.unwrap();
        guard.call(json!({ "amount": 40 })).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_commit_is_compensated_and_still_fails() {
        let charges = Arc::new(AtomicUsize::new(0));
        let refunds = Arc::new(AtomicUsize::new(0));
        let state = State::new();
        let guard = CommitGuard::new(counting("charge", charges, true), state.clone())
            .compensate_with(counting("refund", refunds.clone(), false));
        assert!(guard.call(json!({ "amount": 40 })).await.is_err());
        assert_eq!(refunds.load(Ordering::SeqCst), 1);
        assert_eq!(state.get::<bool>(&compensated_key("charge")), Some(true));
    }
}
