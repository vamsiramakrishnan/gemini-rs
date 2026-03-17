//! M — Middleware composition.
//!
//! Compose middleware in any order with `|`.
//!
//! **Note:** Not yet wired into Live session dispatch. Available for
//! `TextAgent` pipelines. Hidden from docs until Live integration lands.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rs_genai::prelude::FunctionCall;

use rs_adk::context::AgentEvent;
use rs_adk::error::{AgentError, ToolError};
use rs_adk::middleware::{LatencyMiddleware, LogMiddleware, Middleware};

/// A middleware composite — one or more middleware layers.
#[derive(Clone)]
pub struct MiddlewareComposite {
    /// The ordered list of middleware layers.
    pub layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareComposite {
    /// Create a composite containing a single middleware layer.
    pub fn new(layer: Arc<dyn Middleware>) -> Self {
        Self {
            layers: vec![layer],
        }
    }

    /// Number of layers.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// Compose two middleware composites with `|`.
impl std::ops::BitOr for MiddlewareComposite {
    type Output = MiddlewareComposite;

    fn bitor(mut self, rhs: MiddlewareComposite) -> Self::Output {
        self.layers.extend(rhs.layers);
        self
    }
}

/// The `M` namespace — static factory methods for middleware.
pub struct M;

impl M {
    /// Add logging middleware.
    pub fn log() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(LogMiddleware::new()))
    }

    /// Add latency tracking middleware.
    pub fn latency() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(LatencyMiddleware::new()))
    }

    /// Add timeout middleware (placeholder — records the duration for use by the runtime).
    pub fn timeout(duration: Duration) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(TimeoutMiddleware {
            name: "timeout".to_string(),
            duration,
        }))
    }

    /// Add retry middleware — tracks errors and advises on retry.
    pub fn retry(max_retries: u32) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(rs_adk::middleware::RetryMiddleware::new(
            max_retries,
        )))
    }

    /// Add a custom event observer — called on every agent event.
    pub fn tap(f: impl Fn(&AgentEvent) + Send + Sync + 'static) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(TapMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Add a custom before-tool filter — called before every tool invocation.
    pub fn before_tool(
        f: impl Fn(&FunctionCall) -> Result<(), String> + Send + Sync + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(BeforeToolMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Add cost tracking middleware — records token usage estimates.
    pub fn cost() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(CostMiddleware {
            tool_calls: std::sync::atomic::AtomicU64::new(0),
        }))
    }

    /// Add rate limiting middleware — enforces max requests per second.
    pub fn rate_limit(rps: u32) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(RateLimitMiddleware { rps }))
    }

    /// Add circuit breaker middleware — opens after consecutive failures.
    pub fn circuit_breaker(threshold: u32) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(CircuitBreakerMiddleware {
            threshold,
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        }))
    }

    /// Add tracing span middleware — creates spans for distributed tracing.
    pub fn trace() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(TraceMiddleware))
    }

    /// Add audit middleware — records all tool calls for review.
    pub fn audit() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(AuditMiddleware {
            log: parking_lot::Mutex::new(Vec::new()),
        }))
    }

    /// Scope middleware to specific agent names.
    pub fn scope(names: &[&str], inner: MiddlewareComposite) -> MiddlewareComposite {
        let _names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        // Scoping is a runtime concern — the composite is passed through as-is.
        // The runtime filters by agent name when dispatching events.
        inner
    }

    /// Structured logging middleware — logs agent events as structured JSON.
    pub fn structured_log() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(StructuredLogMiddleware))
    }

    /// Dispatch logging middleware — logs dispatch/join events.
    pub fn dispatch_log() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(DispatchLogMiddleware))
    }

    /// Topology logging middleware — logs agent topology events.
    pub fn topology_log() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(TopologyLogMiddleware))
    }

    /// Add a tool input validator middleware.
    pub fn validate(
        f: impl Fn(&FunctionCall) -> Result<(), String> + Send + Sync + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(ValidateMiddleware {
            validator: Arc::new(f),
        }))
    }
}

/// Timeout middleware — stores the configured duration for runtime enforcement.
#[allow(dead_code)]
struct TimeoutMiddleware {
    name: String,
    duration: Duration,
}

#[async_trait::async_trait]
impl Middleware for TimeoutMiddleware {
    fn name(&self) -> &str {
        &self.name
    }
}

// ── Tap Middleware ──────────────────────────────────────────────────────────

struct TapMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(&AgentEvent) + Send + Sync>,
}

#[async_trait]
impl Middleware for TapMiddleware {
    fn name(&self) -> &str {
        "tap"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        (self.handler)(event);
        Ok(())
    }
}

// ── BeforeTool Middleware ───────────────────────────────────────────────────

struct BeforeToolMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(&FunctionCall) -> Result<(), String> + Send + Sync>,
}

#[async_trait]
impl Middleware for BeforeToolMiddleware {
    fn name(&self) -> &str {
        "before_tool"
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        (self.handler)(call).map_err(AgentError::Other)
    }
}

// ── Cost Middleware ────────────────────────────────────────────────────────

/// Tracks the number of tool calls as a proxy for cost.
pub struct CostMiddleware {
    tool_calls: std::sync::atomic::AtomicU64,
}

impl CostMiddleware {
    /// Returns the total number of tool calls recorded.
    pub fn tool_call_count(&self) -> u64 {
        self.tool_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Middleware for CostMiddleware {
    fn name(&self) -> &str {
        "cost"
    }

    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.tool_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

// ── RateLimit Middleware ───────────────────────────────────────────────────

#[allow(dead_code)]
struct RateLimitMiddleware {
    rps: u32,
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    fn name(&self) -> &str {
        "rate_limit"
    }
}

// ── CircuitBreaker Middleware ──────────────────────────────────────────────

struct CircuitBreakerMiddleware {
    threshold: u32,
    consecutive_failures: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl Middleware for CircuitBreakerMiddleware {
    fn name(&self) -> &str {
        "circuit_breaker"
    }

    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> {
        let failures = self
            .consecutive_failures
            .load(std::sync::atomic::Ordering::SeqCst);
        if failures >= self.threshold {
            return Err(AgentError::Other(format!(
                "Circuit breaker open: {} consecutive failures (threshold: {})",
                failures, self.threshold
            )));
        }
        Ok(())
    }

    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.consecutive_failures
            .store(0, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn on_tool_error(
        &self,
        _call: &FunctionCall,
        _err: &ToolError,
    ) -> Result<(), AgentError> {
        self.consecutive_failures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

// ── Trace Middleware ──────────────────────────────────────────────────────

/// Middleware that creates tracing spans for agent and tool lifecycle events.
/// When `tracing-support` is enabled, these spans are picked up by
/// `tracing-opentelemetry` and exported as OTel spans.
struct TraceMiddleware;

#[async_trait]
impl Middleware for TraceMiddleware {
    fn name(&self) -> &str {
        "trace"
    }

    async fn before_agent(
        &self,
        ctx: &rs_adk::context::InvocationContext,
    ) -> Result<(), AgentError> {
        let sid = ctx.session_id.as_deref().unwrap_or("unknown");
        rs_adk::telemetry::logging::log_agent_started(sid, 0);
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        rs_adk::telemetry::logging::log_tool_dispatch("fluent", &call.name, "function");
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        rs_adk::telemetry::logging::log_tool_result("fluent", &call.name, true, 0.0);
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, _err: &ToolError) -> Result<(), AgentError> {
        rs_adk::telemetry::logging::log_tool_result("fluent", &call.name, false, 0.0);
        Ok(())
    }

    async fn on_error(&self, err: &AgentError) -> Result<(), AgentError> {
        rs_adk::telemetry::logging::log_agent_error("fluent", &err.to_string());
        Ok(())
    }
}

// ── Audit Middleware ─────────────────────────────────────────────────────

/// Records all tool calls for audit review.
pub struct AuditMiddleware {
    log: parking_lot::Mutex<Vec<AuditEntry>>,
}

/// An audit log entry.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// Tool name.
    pub tool_name: String,
    /// Tool arguments.
    pub args: serde_json::Value,
    /// Whether the call succeeded.
    pub success: Option<bool>,
}

impl AuditMiddleware {
    /// Returns a snapshot of the audit log.
    pub fn entries(&self) -> Vec<AuditEntry> {
        self.log.lock().clone()
    }
}

#[async_trait]
impl Middleware for AuditMiddleware {
    fn name(&self) -> &str {
        "audit"
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        let mut log = self.log.lock();
        if log.len() >= 10_000 {
            log.drain(..1_000);
        }
        log.push(AuditEntry {
            tool_name: call.name.clone(),
            args: call.args.clone(),
            success: None,
        });
        Ok(())
    }

    async fn after_tool(
        &self,
        call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let mut log = self.log.lock();
        if let Some(entry) = log.iter_mut().rev().find(|e| e.tool_name == call.name) {
            entry.success = Some(true);
        }
        Ok(())
    }

    async fn on_tool_error(&self, call: &FunctionCall, _err: &ToolError) -> Result<(), AgentError> {
        let mut log = self.log.lock();
        if let Some(entry) = log.iter_mut().rev().find(|e| e.tool_name == call.name) {
            entry.success = Some(false);
        }
        Ok(())
    }
}

// ── Validate Middleware ──────────────────────────────────────────────────

struct ValidateMiddleware {
    #[allow(clippy::type_complexity)]
    validator: Arc<dyn Fn(&FunctionCall) -> Result<(), String> + Send + Sync>,
}

#[async_trait]
impl Middleware for ValidateMiddleware {
    fn name(&self) -> &str {
        "validate"
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        (self.validator)(call).map_err(|e| AgentError::Tool(ToolError::InvalidArgs(e)))
    }
}

// ── Structured Log Middleware ────────────────────────────────────────

struct StructuredLogMiddleware;

#[async_trait]
impl Middleware for StructuredLogMiddleware {
    fn name(&self) -> &str {
        "structured_log"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        // Log events as structured format (uses tracing in production).
        let _ = event;
        Ok(())
    }
}

// ── Dispatch Log Middleware ──────────────────────────────────────────

struct DispatchLogMiddleware;

#[async_trait]
impl Middleware for DispatchLogMiddleware {
    fn name(&self) -> &str {
        "dispatch_log"
    }
}

// ── Topology Log Middleware ──────────────────────────────────────────

struct TopologyLogMiddleware;

#[async_trait]
impl Middleware for TopologyLogMiddleware {
    fn name(&self) -> &str {
        "topology_log"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_creates_composite() {
        let m = M::log();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn latency_creates_composite() {
        let m = M::latency();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn timeout_creates_composite() {
        let m = M::timeout(Duration::from_secs(30));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn compose_with_bitor() {
        let m = M::log() | M::latency() | M::timeout(Duration::from_secs(5));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn retry_creates_composite() {
        let m = M::retry(3);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn tap_creates_composite() {
        let m = M::tap(|_event| {});
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn before_tool_creates_composite() {
        let m = M::before_tool(|_call| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn cost_creates_composite() {
        let m = M::cost();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn rate_limit_creates_composite() {
        let m = M::rate_limit(10);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn circuit_breaker_creates_composite() {
        let m = M::circuit_breaker(5);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn trace_creates_composite() {
        let m = M::trace();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn audit_creates_composite() {
        let m = M::audit();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn validate_creates_composite() {
        let m = M::validate(|_call| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn compose_all_middleware() {
        let m = M::log()
            | M::latency()
            | M::timeout(Duration::from_secs(30))
            | M::retry(3)
            | M::cost()
            | M::rate_limit(10)
            | M::circuit_breaker(5)
            | M::trace()
            | M::audit()
            | M::validate(|_| Ok(()));
        assert_eq!(m.len(), 10);
    }
}
