//! Middleware trait and chain — wraps agent execution at lifecycle points.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use gemini_live_wire::prelude::FunctionCall;

use crate::context::AgentEvent;
use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};

/// Middleware hooks — all optional, implement only what you need.
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    fn name(&self) -> &str;

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        Ok(())
    }
    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        Ok(())
    }

    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> {
        Ok(())
    }
    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        Ok(())
    }
    async fn on_tool_error(
        &self,
        _call: &FunctionCall,
        _err: &ToolError,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    async fn on_event(&self, _event: &AgentEvent) -> Result<(), AgentError> {
        Ok(())
    }

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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, middleware: Arc<dyn Middleware>) {
        self.layers.push(middleware);
    }

    pub async fn run_before_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_agent(ctx).await?;
        }
        Ok(())
    }

    pub async fn run_after_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in self.layers.iter().rev() {
            m.after_agent(ctx).await?;
        }
        Ok(())
    }

    pub async fn run_before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_tool(call).await?;
        }
        Ok(())
    }

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

    pub async fn run_on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_event(event).await?;
        }
        Ok(())
    }

    pub async fn run_on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_error(err).await?;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }
}

// ── Built-in Middleware ──────────────────────────────────────────────────────

/// Logs agent and tool lifecycle events.
///
/// When the `tracing-support` feature is enabled, uses `tracing` macros for
/// structured logging. Without the feature, all hooks are silent no-ops.
pub struct LogMiddleware;

impl LogMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for LogMiddleware {
    fn name(&self) -> &str {
        "log"
    }

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        #[cfg(feature = "tracing-support")]
        tracing::info!("Agent starting");
        Ok(())
    }

    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        #[cfg(feature = "tracing-support")]
        tracing::info!("Agent completed");
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        let _ = call; // used only when tracing-support is enabled
        #[cfg(feature = "tracing-support")]
        {
            tracing::info!(tool = %call.name, "Tool call starting");
            tracing::debug!(tool = %call.name, args = %call.args, "Tool call args");
        }
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let _ = call;
        #[cfg(feature = "tracing-support")]
        tracing::info!(tool = %call.name, "Tool call completed");
        Ok(())
    }

    async fn on_tool_error(
        &self,
        call: &FunctionCall,
        err: &ToolError,
    ) -> Result<(), AgentError> {
        let _ = (call, err);
        #[cfg(feature = "tracing-support")]
        tracing::warn!(tool = %call.name, error = %err, "Tool call failed");
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        let _ = err;
        #[cfg(feature = "tracing-support")]
        tracing::error!(error = %err, "Agent error");
        Ok(())
    }
}

// ── Latency Middleware ───────────────────────────────────────────────────────

/// A recorded tool-call latency measurement.
#[derive(Debug, Clone)]
pub struct ToolLatency {
    /// Tool name.
    pub name: String,
    /// Elapsed wall-clock time.
    pub elapsed: Duration,
    /// Whether the tool call succeeded.
    pub success: bool,
}

/// Middleware that records latency metrics for tool calls.
///
/// Stores `ToolLatency` entries that can be retrieved via [`LatencyMiddleware::tool_latencies`].
/// Thread-safe and suitable for use across async tasks.
pub struct LatencyMiddleware {
    /// In-flight tool start times, keyed by tool name.
    /// Multiple concurrent calls to the same tool name will overwrite,
    /// but this is acceptable for metrics collection.
    in_flight: parking_lot::Mutex<std::collections::HashMap<String, Instant>>,
    /// Completed tool latency records.
    records: parking_lot::Mutex<Vec<ToolLatency>>,
}

impl LatencyMiddleware {
    pub fn new() -> Self {
        Self {
            in_flight: parking_lot::Mutex::new(std::collections::HashMap::new()),
            records: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Returns a snapshot of all recorded tool latencies.
    pub fn tool_latencies(&self) -> Vec<ToolLatency> {
        self.records.lock().clone()
    }

    /// Clears all recorded latencies and in-flight state.
    pub fn clear(&self) {
        self.in_flight.lock().clear();
        self.records.lock().clear();
    }
}

impl Default for LatencyMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for LatencyMiddleware {
    fn name(&self) -> &str {
        "latency"
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        self.in_flight
            .lock()
            .insert(call.name.clone(), Instant::now());
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let elapsed = self
            .in_flight
            .lock()
            .remove(&call.name)
            .map(|start| start.elapsed())
            .unwrap_or_default();
        self.records.lock().push(ToolLatency {
            name: call.name.clone(),
            elapsed,
            success: true,
        });
        Ok(())
    }

    async fn on_tool_error(
        &self,
        call: &FunctionCall,
        _err: &ToolError,
    ) -> Result<(), AgentError> {
        let elapsed = self
            .in_flight
            .lock()
            .remove(&call.name)
            .map(|start| start.elapsed())
            .unwrap_or_default();
        self.records.lock().push(ToolLatency {
            name: call.name.clone(),
            elapsed,
            success: false,
        });
        Ok(())
    }
}

// ── Retry Middleware ─────────────────────────────────────────────────────────

/// Advisory middleware that tracks errors and provides retry guidance.
///
/// The `Middleware` trait hooks are lifecycle callbacks, not control-flow points.
/// `RetryMiddleware` counts errors via [`Middleware::on_error`] and exposes a
/// [`RetryMiddleware::should_retry`] method the caller can query to decide
/// whether to re-invoke the agent.
///
/// # Example
///
/// ```ignore
/// let retry = Arc::new(RetryMiddleware::new(3));
/// // ... run agent ...
/// if retry.should_retry() {
///     retry.record_attempt();
///     // re-run agent
/// }
/// ```
pub struct RetryMiddleware {
    max_retries: u32,
    error_count: std::sync::atomic::AtomicU32,
    attempt: std::sync::atomic::AtomicU32,
}

impl RetryMiddleware {
    /// Create a new retry middleware with the given maximum retry count.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            error_count: std::sync::atomic::AtomicU32::new(0),
            attempt: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Returns `true` if the number of attempts is below `max_retries`
    /// and at least one error has been recorded since the last reset.
    pub fn should_retry(&self) -> bool {
        let attempts = self.attempt.load(std::sync::atomic::Ordering::SeqCst);
        let errors = self.error_count.load(std::sync::atomic::Ordering::SeqCst);
        errors > 0 && attempts < self.max_retries
    }

    /// Record that a retry attempt is being made.
    /// Call this before re-invoking the agent.
    pub fn record_attempt(&self) {
        self.attempt
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Reset the error flag so we wait for a new error before retrying again.
        self.error_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns the current attempt count (0-based).
    pub fn attempts(&self) -> u32 {
        self.attempt.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the configured maximum number of retries.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Reset all counters, allowing the middleware to be reused.
    pub fn reset(&self) {
        self.error_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.attempt
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl Middleware for RetryMiddleware {
    fn name(&self) -> &str {
        "retry"
    }

    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> {
        self.error_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            !retry.should_retry(),
            "at max retries, should not retry"
        );
    }

    #[test]
    fn retry_middleware_reset() {
        let retry = RetryMiddleware::new(2);
        retry
            .error_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        retry
            .attempt
            .store(1, std::sync::atomic::Ordering::SeqCst);
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
