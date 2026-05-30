//! Per-tool execution policy — timeout, caching, and confirmation.
//!
//! A [`ToolPolicy`] describes optional runtime enforcement attached to an
//! individual tool. [`PolicyTool`] is a [`ToolFunction`] decorator that wraps
//! an inner tool and enforces its policy on every call:
//!
//! - **timeout**: the inner call is raced against [`tokio::time::timeout`];
//!   on elapse, [`ToolError::Timeout`] is returned and the inner future dropped.
//! - **cache**: successful results are memoized in a concurrent map keyed by
//!   `(tool name, canonical-JSON args)`. Repeat calls with identical args return
//!   the cached value without re-invoking the inner tool. Errors are not cached.
//! - **confirm**: a declarative flag recorded on the policy and surfaced via
//!   [`PolicyTool::requires_confirmation`]. The flag is never silently dropped;
//!   full interactive confirmation wiring is handled by the session runtime.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;

use crate::error::ToolError;

use super::ToolFunction;

/// Optional per-tool execution policy.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    /// If set, the tool call is bounded by this duration.
    pub timeout: Option<Duration>,
    /// If `true`, successful results are memoized by `(name, canonical args)`.
    pub cache: bool,
    /// If `true`, the tool requires user confirmation before execution.
    pub confirm: bool,
    /// Optional hint shown when confirmation is requested.
    pub confirm_message: Option<String>,
}

impl ToolPolicy {
    /// Create an empty policy (no enforcement).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this policy enforces anything at all.
    ///
    /// Used to decide whether wrapping a tool in a [`PolicyTool`] is worthwhile.
    pub fn is_noop(&self) -> bool {
        self.timeout.is_none() && !self.cache && !self.confirm
    }

    /// Set a timeout.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// Enable caching.
    pub fn with_cache(mut self) -> Self {
        self.cache = true;
        self
    }

    /// Require confirmation with an optional message.
    pub fn with_confirm(mut self, message: Option<String>) -> Self {
        self.confirm = true;
        self.confirm_message = message;
        self
    }

    /// Merge another policy into this one (the other takes precedence where set).
    pub fn merge(mut self, other: &ToolPolicy) -> Self {
        if other.timeout.is_some() {
            self.timeout = other.timeout;
        }
        self.cache |= other.cache;
        if other.confirm {
            self.confirm = true;
            if other.confirm_message.is_some() {
                self.confirm_message = other.confirm_message.clone();
            }
        }
        self
    }
}

/// A [`ToolFunction`] decorator that enforces a [`ToolPolicy`].
pub struct PolicyTool {
    inner: Arc<dyn ToolFunction>,
    policy: ToolPolicy,
    cache: Arc<DashMap<String, serde_json::Value>>,
}

impl PolicyTool {
    /// Wrap `inner` with the given `policy`.
    pub fn new(inner: Arc<dyn ToolFunction>, policy: ToolPolicy) -> Self {
        Self {
            inner,
            policy,
            cache: Arc::new(DashMap::new()),
        }
    }

    /// Wrap `inner` only if the policy enforces something; otherwise return `inner`.
    pub fn wrap(inner: Arc<dyn ToolFunction>, policy: ToolPolicy) -> Arc<dyn ToolFunction> {
        if policy.is_noop() {
            inner
        } else {
            Arc::new(Self::new(inner, policy))
        }
    }

    /// Whether this tool requires user confirmation before execution.
    pub fn requires_confirmation(&self) -> bool {
        self.policy.confirm
    }

    /// The policy attached to this tool.
    pub fn policy(&self) -> &ToolPolicy {
        &self.policy
    }

    /// Build a stable cache key from the tool name and canonical-JSON args.
    fn cache_key(&self, args: &serde_json::Value) -> String {
        format!("{}\u{1}{}", self.inner.name(), canonical_json(args))
    }
}

/// Render a JSON value canonically so equal values produce equal strings.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                out.push_str(&canonical_json(&map[*k]));
            }
            out.push('}');
            out
        }
        serde_json::Value::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_json(item));
            }
            out.push(']');
            out
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[async_trait]
impl ToolFunction for PolicyTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        self.inner.parameters()
    }

    fn requires_confirmation(&self) -> bool {
        // Propagate through nested wrappers so modifier order can't bypass a
        // gate: e.g. `T::cached(T::confirm(..))` wraps a confirm PolicyTool in
        // a cache PolicyTool whose own policy has `confirm == false`.
        self.policy.confirm || self.inner.requires_confirmation()
    }

    fn confirmation_message(&self) -> Option<&str> {
        self.policy
            .confirm_message
            .as_deref()
            .or_else(|| self.inner.confirmation_message())
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // Cache lookup (only for cacheable tools).
        let key = if self.policy.cache {
            let key = self.cache_key(&args);
            if let Some(hit) = self.cache.get(&key) {
                return Ok(hit.clone());
            }
            Some(key)
        } else {
            None
        };

        // Execute with optional timeout enforcement.
        let result = if let Some(timeout) = self.policy.timeout {
            match tokio::time::timeout(timeout, self.inner.call(args)).await {
                Ok(r) => r,
                Err(_elapsed) => Err(ToolError::Timeout(timeout)),
            }
        } else {
            self.inner.call(args).await
        };

        // Memoize successful results.
        if let (Some(key), Ok(value)) = (key, &result) {
            self.cache.insert(key, value.clone());
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::SimpleTool;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn timeout_policy_returns_timeout_error() {
        let slow: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "slow",
            "sleeps too long",
            None,
            |_| async move {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(json!({"ok": true}))
            },
        ));
        let tool = PolicyTool::new(
            slow,
            ToolPolicy::new().with_timeout(Duration::from_millis(50)),
        );

        match tool.call(json!({})).await {
            Err(ToolError::Timeout(d)) => assert_eq!(d, Duration::from_millis(50)),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn under_timeout_succeeds() {
        let fast: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "fast",
            "returns quickly",
            None,
            |_| async move { Ok(json!({"ok": true})) },
        ));
        let tool = PolicyTool::new(fast, ToolPolicy::new().with_timeout(Duration::from_secs(5)));
        let out = tool.call(json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
    }

    #[tokio::test]
    async fn cache_returns_same_value_and_runs_once() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let counting: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "count",
            "increments a counter",
            None,
            move |_| {
                let c = c.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(json!({"n": n}))
                }
            },
        ));
        let tool = PolicyTool::new(counting, ToolPolicy::new().with_cache());

        let first = tool.call(json!({"x": 1})).await.unwrap();
        let second = tool.call(json!({"x": 1})).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(first["n"], 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Different args -> cache miss, counter advances.
        let third = tool.call(json!({"x": 2})).await.unwrap();
        assert_eq!(third["n"], 2);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cache_key_is_order_independent() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let counting: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "count2",
            "increments a counter",
            None,
            move |_| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"ok": true}))
                }
            },
        ));
        let tool = PolicyTool::new(counting, ToolPolicy::new().with_cache());

        tool.call(json!({"a": 1, "b": 2})).await.unwrap();
        // Same logical args, different key order -> should hit cache.
        tool.call(json!({"b": 2, "a": 1})).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let failing: Arc<dyn ToolFunction> =
            Arc::new(SimpleTool::new("fail", "always fails", None, move |_| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(ToolError::ExecutionFailed("boom".into()))
                }
            }));
        let tool = PolicyTool::new(failing, ToolPolicy::new().with_cache());

        assert!(tool.call(json!({})).await.is_err());
        assert!(tool.call(json!({})).await.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn wrap_skips_noop_policy() {
        let inner: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "plain",
            "plain tool",
            None,
            |_| async move { Ok(json!({})) },
        ));
        let wrapped = PolicyTool::wrap(inner.clone(), ToolPolicy::new());
        assert_eq!(wrapped.name(), "plain");
        // confirm-only policy still wraps so the flag is preserved.
        let confirmed = PolicyTool::wrap(inner, ToolPolicy::new().with_confirm(None));
        assert_eq!(confirmed.name(), "plain");
    }

    #[test]
    fn confirm_flag_is_recorded() {
        let inner: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "danger",
            "dangerous",
            None,
            |_| async move { Ok(json!({})) },
        ));
        let tool = PolicyTool::new(
            inner,
            ToolPolicy::new().with_confirm(Some("are you sure?".into())),
        );
        assert!(tool.requires_confirmation());
        assert_eq!(
            tool.policy().confirm_message.as_deref(),
            Some("are you sure?")
        );
    }
}
