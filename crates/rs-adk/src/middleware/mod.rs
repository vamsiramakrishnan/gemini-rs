//! Middleware trait and chain — wraps agent execution at lifecycle points.

pub mod latency;
pub mod log;
pub mod retry;

pub use latency::*;
pub use log::*;
pub use retry::*;

use std::sync::Arc;

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use crate::context::AgentEvent;
use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};

/// Middleware hooks — all optional, implement only what you need.
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    /// Unique name for this middleware (used in logging/debugging).
    fn name(&self) -> &str;

    /// Called before an agent begins execution.
    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        Ok(())
    }
    /// Called after an agent completes execution.
    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called before a tool is invoked.
    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> {
        Ok(())
    }
    /// Called after a tool completes successfully.
    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        Ok(())
    }
    /// Called when a tool execution fails.
    async fn on_tool_error(
        &self,
        _call: &FunctionCall,
        _err: &ToolError,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called when an agent event is emitted.
    async fn on_event(&self, _event: &AgentEvent) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called when an agent error occurs.
    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> {
        Ok(())
    }
}

/// Ordered chain of middleware.
#[derive(Clone, Default)]
pub struct MiddlewareChain {
    layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareChain {
    /// Create a new empty middleware chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a middleware to the end of the chain.
    pub fn add(&mut self, middleware: Arc<dyn Middleware>) {
        self.layers.push(middleware);
    }

    /// Prepend a middleware to the front of the chain.
    pub fn prepend(&mut self, middleware: Arc<dyn Middleware>) {
        self.layers.insert(0, middleware);
    }

    /// Run all `before_agent` hooks in order.
    pub async fn run_before_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_agent(ctx).await?;
        }
        Ok(())
    }

    /// Run all `after_agent` hooks in reverse order.
    pub async fn run_after_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in self.layers.iter().rev() {
            m.after_agent(ctx).await?;
        }
        Ok(())
    }

    /// Run all `before_tool` hooks in order.
    pub async fn run_before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_tool(call).await?;
        }
        Ok(())
    }

    /// Run all `after_tool` hooks in reverse order.
    pub async fn run_after_tool(
        &self,
        call: &FunctionCall,
        result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        for m in self.layers.iter().rev() {
            m.after_tool(call, result).await?;
        }
        Ok(())
    }

    /// Run all `on_tool_error` hooks in order.
    pub async fn run_on_tool_error(
        &self,
        call: &FunctionCall,
        err: &ToolError,
    ) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_tool_error(call, err).await?;
        }
        Ok(())
    }

    /// Run all `on_event` hooks in order.
    pub async fn run_on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_event(event).await?;
        }
        Ok(())
    }

    /// Run all `on_error` hooks in order.
    pub async fn run_on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_error(err).await?;
        }
        Ok(())
    }

    /// Whether the chain has no middleware layers.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Number of middleware layers in the chain.
    pub fn len(&self) -> usize {
        self.layers.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Helper: create a FunctionCall for testing.
    fn test_call(name: &str) -> FunctionCall {
        FunctionCall {
            name: name.to_string(),
            args: serde_json::json!({"key": "value"}),
            id: None,
        }
    }

    // ── Existing tests ──

    struct CountingMiddleware {
        call_count: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait]
    impl Middleware for CountingMiddleware {
        fn name(&self) -> &str {
            "counter"
        }

        async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn middleware_chain_ordering() {
        let chain = MiddlewareChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn middleware_is_object_safe() {
        fn _assert(_: &dyn Middleware) {}
    }

    #[test]
    fn add_middleware_to_chain() {
        let mut chain = MiddlewareChain::new();
        let counter = Arc::new(CountingMiddleware {
            call_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        });
        chain.add(counter);
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
    }

    #[test]
    fn chain_is_clone() {
        let mut chain = MiddlewareChain::new();
        chain.add(Arc::new(LogMiddleware::new()));
        let chain2 = chain.clone();
        assert_eq!(chain2.len(), 1);
    }

    #[test]
    fn log_middleware_defaults() {
        let log = LogMiddleware::new();
        assert_eq!(log.name(), "log");
    }

    #[test]
    fn latency_middleware_defaults() {
        let lat = LatencyMiddleware::new();
        assert_eq!(lat.name(), "latency");
    }

    // ── LogMiddleware tests ──

    #[tokio::test]
    async fn logging_middleware_doesnt_panic() {
        let log = LogMiddleware::new();
        let call = test_call("my_tool");
        let result = serde_json::json!({"ok": true});
        let tool_err = ToolError::ExecutionFailed("boom".to_string());
        let agent_err = AgentError::Other("oops".to_string());

        // All hooks should complete without panic.
        assert!(log.before_tool(&call).await.is_ok());
        assert!(log.after_tool(&call, &result).await.is_ok());
        assert!(log.on_tool_error(&call, &tool_err).await.is_ok());
        assert!(log.on_error(&agent_err).await.is_ok());
    }

    // ── LatencyMiddleware tests ──

    #[tokio::test]
    async fn latency_middleware_records_timing() {
        let lat = LatencyMiddleware::new();
        let call = test_call("slow_tool");
        let result = serde_json::json!("done");

        // Simulate a tool call.
        lat.before_tool(&call).await.unwrap();
        // Small delay to ensure non-zero elapsed time.
        tokio::time::sleep(Duration::from_millis(5)).await;
        lat.after_tool(&call, &result).await.unwrap();

        let records = lat.tool_latencies();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "slow_tool");
        assert!(records[0].success);
        assert!(records[0].elapsed >= Duration::from_millis(1));
    }

    #[tokio::test]
    async fn latency_middleware_records_failure() {
        let lat = LatencyMiddleware::new();
        let call = test_call("failing_tool");
        let err = ToolError::ExecutionFailed("kaboom".to_string());

        lat.before_tool(&call).await.unwrap();
        lat.on_tool_error(&call, &err).await.unwrap();

        let records = lat.tool_latencies();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "failing_tool");
        assert!(!records[0].success);
    }

    #[tokio::test]
    async fn latency_middleware_clear() {
        let lat = LatencyMiddleware::new();
        let call = test_call("tool_a");
        let result = serde_json::json!(null);

        lat.before_tool(&call).await.unwrap();
        lat.after_tool(&call, &result).await.unwrap();
        assert_eq!(lat.tool_latencies().len(), 1);

        lat.clear();
        assert!(lat.tool_latencies().is_empty());
    }

    // ── RetryMiddleware tests ──

    #[tokio::test]
    async fn retry_middleware_tracks_retries() {
        let retry = RetryMiddleware::new(3);
        assert_eq!(retry.max_retries(), 3);
        assert_eq!(retry.attempts(), 0);
        assert!(!retry.should_retry(), "no error yet, should not retry");

        // Simulate an error.
        let err = AgentError::Other("transient".to_string());
        retry.on_error(&err).await.unwrap();
        assert!(retry.should_retry(), "error recorded, should retry");

        // Record first attempt.
        retry.record_attempt();
        assert_eq!(retry.attempts(), 1);
        assert!(!retry.should_retry(), "error was cleared by record_attempt");

        // Another error + attempt cycle.
        retry.on_error(&err).await.unwrap();
        assert!(retry.should_retry());
        retry.record_attempt();
        assert_eq!(retry.attempts(), 2);

        // Third error + attempt.
        retry.on_error(&err).await.unwrap();
        assert!(retry.should_retry());
        retry.record_attempt();
        assert_eq!(retry.attempts(), 3);

        // Now at max — should not retry even with new error.
        retry.on_error(&err).await.unwrap();
        assert!(!retry.should_retry(), "at max retries, should not retry");
    }

    #[test]
    fn retry_middleware_reset() {
        let retry = RetryMiddleware::new(2);
        retry
            .error_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        retry.attempt.store(1, std::sync::atomic::Ordering::SeqCst);
        retry.reset();
        assert_eq!(retry.attempts(), 0);
        assert!(!retry.should_retry());
    }

    // ── Chain integration test ──

    #[test]
    fn chain_with_all_builtin_middleware() {
        let mut chain = MiddlewareChain::new();
        chain.add(Arc::new(LogMiddleware::new()));
        chain.add(Arc::new(LatencyMiddleware::new()));
        chain.add(Arc::new(RetryMiddleware::new(3)));
        assert_eq!(chain.len(), 3);
    }
}
