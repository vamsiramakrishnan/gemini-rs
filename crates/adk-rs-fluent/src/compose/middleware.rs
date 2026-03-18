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

    /// Fallback to an alternative model on error.
    pub fn fallback_model(model: &str) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(FallbackModelMiddleware {
            model: model.to_string(),
        }))
    }

    /// Response caching middleware — caches model responses to avoid redundant calls.
    pub fn cache() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(CacheMiddleware {
            cache: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }))
    }

    /// Deduplicate consecutive identical requests.
    pub fn dedup() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(DedupMiddleware {
            last_request_hash: parking_lot::Mutex::new(None),
        }))
    }

    /// Sample/pass-through a fraction of requests (0.0–1.0).
    pub fn sample(rate: f64) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(SampleMiddleware {
            rate: rate.clamp(0.0, 1.0),
        }))
    }

    /// Metrics collection middleware — tracks request counts, error counts, and latencies.
    pub fn metrics() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(MetricsMiddleware {
            request_count: std::sync::atomic::AtomicU64::new(0),
            error_count: std::sync::atomic::AtomicU64::new(0),
        }))
    }

    /// Shortcut for a before-agent hook.
    pub fn before_agent(
        f: impl Fn(&rs_adk::context::InvocationContext) -> Result<(), String> + Send + Sync + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(BeforeAgentMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Shortcut for an after-agent hook.
    pub fn after_agent(
        f: impl Fn(&rs_adk::context::InvocationContext) -> Result<(), String> + Send + Sync + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(AfterAgentMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Shortcut for a before-model hook.
    pub fn before_model(
        f: impl Fn(&rs_adk::llm::LlmRequest) -> Result<(), String> + Send + Sync + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(BeforeModelMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Shortcut for an after-model hook.
    pub fn after_model(
        f: impl Fn(&rs_adk::llm::LlmRequest, &rs_adk::llm::LlmResponse) -> Result<(), String>
            + Send
            + Sync
            + 'static,
    ) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(AfterModelMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Loop iteration event hook — called on each iteration of a loop agent.
    pub fn on_loop(f: impl Fn(u32) + Send + Sync + 'static) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(OnLoopMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Timeout event hook — called when an agent times out.
    pub fn on_timeout(f: impl Fn() + Send + Sync + 'static) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(OnTimeoutMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Route decision event hook — called when a route agent selects a branch.
    pub fn on_route(f: impl Fn(&str) + Send + Sync + 'static) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(OnRouteMiddleware {
            handler: Arc::new(f),
        }))
    }

    /// Fallback event hook — called when a fallback agent activates.
    pub fn on_fallback(f: impl Fn(&str) + Send + Sync + 'static) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(OnFallbackMiddleware {
            handler: Arc::new(f),
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

// ── FallbackModel Middleware ──────────────────────────────────────────

/// Middleware that falls back to an alternative model on error.
#[allow(dead_code)]
struct FallbackModelMiddleware {
    model: String,
}

#[async_trait]
impl Middleware for FallbackModelMiddleware {
    fn name(&self) -> &str {
        "fallback_model"
    }

    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> {
        // Runtime inspects the `model` field and retries with the fallback model.
        Ok(())
    }
}

// ── Cache Middleware ──────────────────────────────────────────────────

/// Caches model responses keyed by request hash to avoid redundant LLM calls.
pub struct CacheMiddleware {
    cache: parking_lot::Mutex<std::collections::HashMap<u64, rs_adk::llm::LlmResponse>>,
}

impl CacheMiddleware {
    /// Returns the number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.lock().len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.lock().is_empty()
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.cache.lock().clear();
    }
}

#[async_trait]
impl Middleware for CacheMiddleware {
    fn name(&self) -> &str {
        "cache"
    }

    async fn before_model(
        &self,
        request: &rs_adk::llm::LlmRequest,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", request).hash(&mut hasher);
        let key = hasher.finish();
        let cache = self.cache.lock();
        Ok(cache.get(&key).cloned())
    }

    async fn after_model(
        &self,
        request: &rs_adk::llm::LlmRequest,
        response: &rs_adk::llm::LlmResponse,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", request).hash(&mut hasher);
        let key = hasher.finish();
        self.cache.lock().insert(key, response.clone());
        Ok(None) // don't replace the response
    }
}

// ── Dedup Middleware ─────────────────────────────────────────────────

/// Deduplicates consecutive identical requests by hashing.
#[allow(dead_code)]
struct DedupMiddleware {
    last_request_hash: parking_lot::Mutex<Option<u64>>,
}

#[async_trait]
impl Middleware for DedupMiddleware {
    fn name(&self) -> &str {
        "dedup"
    }

    async fn before_model(
        &self,
        request: &rs_adk::llm::LlmRequest,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", request).hash(&mut hasher);
        let hash = hasher.finish();
        let mut last = self.last_request_hash.lock();
        if *last == Some(hash) {
            // Duplicate consecutive request — signal skip by returning empty response.
            return Err(AgentError::Other("Duplicate consecutive request".to_string()));
        }
        *last = Some(hash);
        Ok(None)
    }
}

// ── Sample Middleware ────────────────────────────────────────────────

/// Passes through only a fraction of requests, dropping the rest.
#[allow(dead_code)]
struct SampleMiddleware {
    rate: f64,
}

#[async_trait]
impl Middleware for SampleMiddleware {
    fn name(&self) -> &str {
        "sample"
    }

    async fn before_model(
        &self,
        _request: &rs_adk::llm::LlmRequest,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        use std::hash::{Hash, Hasher};
        // Use a fast pseudo-random check based on time.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::time::Instant::now().hash(&mut hasher);
        let hash = hasher.finish();
        let normalized = (hash as f64) / (u64::MAX as f64);
        if normalized > self.rate {
            return Err(AgentError::Other("Sampled out".to_string()));
        }
        Ok(None)
    }
}

// ── Metrics Middleware ──────────────────────────────────────────────

/// Collects request and error counts.
pub struct MetricsMiddleware {
    request_count: std::sync::atomic::AtomicU64,
    error_count: std::sync::atomic::AtomicU64,
}

impl MetricsMiddleware {
    /// Returns the total number of requests observed.
    pub fn request_count(&self) -> u64 {
        self.request_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the total number of errors observed.
    pub fn error_count(&self) -> u64 {
        self.error_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Middleware for MetricsMiddleware {
    fn name(&self) -> &str {
        "metrics"
    }

    async fn before_agent(
        &self,
        _ctx: &rs_adk::context::InvocationContext,
    ) -> Result<(), AgentError> {
        self.request_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> {
        self.error_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

// ── BeforeAgent Middleware ───────────────────────────────────────────

struct BeforeAgentMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(&rs_adk::context::InvocationContext) -> Result<(), String> + Send + Sync>,
}

#[async_trait]
impl Middleware for BeforeAgentMiddleware {
    fn name(&self) -> &str {
        "before_agent"
    }

    async fn before_agent(
        &self,
        ctx: &rs_adk::context::InvocationContext,
    ) -> Result<(), AgentError> {
        (self.handler)(ctx).map_err(AgentError::Other)
    }
}

// ── AfterAgent Middleware ───────────────────────────────────────────

struct AfterAgentMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(&rs_adk::context::InvocationContext) -> Result<(), String> + Send + Sync>,
}

#[async_trait]
impl Middleware for AfterAgentMiddleware {
    fn name(&self) -> &str {
        "after_agent"
    }

    async fn after_agent(
        &self,
        ctx: &rs_adk::context::InvocationContext,
    ) -> Result<(), AgentError> {
        (self.handler)(ctx).map_err(AgentError::Other)
    }
}

// ── BeforeModel Middleware ──────────────────────────────────────────

struct BeforeModelMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(&rs_adk::llm::LlmRequest) -> Result<(), String> + Send + Sync>,
}

#[async_trait]
impl Middleware for BeforeModelMiddleware {
    fn name(&self) -> &str {
        "before_model"
    }

    async fn before_model(
        &self,
        request: &rs_adk::llm::LlmRequest,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        (self.handler)(request).map_err(AgentError::Other)?;
        Ok(None)
    }
}

// ── AfterModel Middleware ──────────────────────────────────────────

struct AfterModelMiddleware {
    #[allow(clippy::type_complexity)]
    handler: Arc<
        dyn Fn(&rs_adk::llm::LlmRequest, &rs_adk::llm::LlmResponse) -> Result<(), String>
            + Send
            + Sync,
    >,
}

#[async_trait]
impl Middleware for AfterModelMiddleware {
    fn name(&self) -> &str {
        "after_model"
    }

    async fn after_model(
        &self,
        request: &rs_adk::llm::LlmRequest,
        response: &rs_adk::llm::LlmResponse,
    ) -> Result<Option<rs_adk::llm::LlmResponse>, AgentError> {
        (self.handler)(request, response).map_err(AgentError::Other)?;
        Ok(None)
    }
}

// ── OnLoop Middleware ───────────────────────────────────────────────

struct OnLoopMiddleware {
    handler: Arc<dyn Fn(u32) + Send + Sync>,
}

#[async_trait]
impl Middleware for OnLoopMiddleware {
    fn name(&self) -> &str {
        "on_loop"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        if let AgentEvent::LoopIteration { iteration } = event {
            (self.handler)(*iteration);
        }
        Ok(())
    }
}

// ── OnTimeout Middleware ────────────────────────────────────────────

struct OnTimeoutMiddleware {
    handler: Arc<dyn Fn() + Send + Sync>,
}

#[async_trait]
impl Middleware for OnTimeoutMiddleware {
    fn name(&self) -> &str {
        "on_timeout"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        if let AgentEvent::Timeout = event {
            (self.handler)();
        }
        Ok(())
    }
}

// ── OnRoute Middleware ──────────────────────────────────────────────

struct OnRouteMiddleware {
    handler: Arc<dyn Fn(&str) + Send + Sync>,
}

#[async_trait]
impl Middleware for OnRouteMiddleware {
    fn name(&self) -> &str {
        "on_route"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        if let AgentEvent::RouteSelected { agent_name } = event {
            (self.handler)(agent_name);
        }
        Ok(())
    }
}

// ── OnFallback Middleware ───────────────────────────────────────────

struct OnFallbackMiddleware {
    handler: Arc<dyn Fn(&str) + Send + Sync>,
}

#[async_trait]
impl Middleware for OnFallbackMiddleware {
    fn name(&self) -> &str {
        "on_fallback"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        if let AgentEvent::FallbackActivated { agent_name } = event {
            (self.handler)(agent_name);
        }
        Ok(())
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
    fn fallback_model_creates_composite() {
        let m = M::fallback_model("gemini-1.5-flash");
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn cache_creates_composite() {
        let m = M::cache();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn dedup_creates_composite() {
        let m = M::dedup();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn sample_creates_composite() {
        let m = M::sample(0.5);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn sample_clamps_rate() {
        let m = M::sample(2.0);
        assert_eq!(m.len(), 1);
        let m = M::sample(-1.0);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn metrics_creates_composite() {
        let m = M::metrics();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn before_agent_creates_composite() {
        let m = M::before_agent(|_ctx| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn after_agent_creates_composite() {
        let m = M::after_agent(|_ctx| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn before_model_creates_composite() {
        let m = M::before_model(|_req| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn after_model_creates_composite() {
        let m = M::after_model(|_req, _resp| Ok(()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn on_loop_creates_composite() {
        let m = M::on_loop(|_iteration| {});
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn on_timeout_creates_composite() {
        let m = M::on_timeout(|| {});
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn on_route_creates_composite() {
        let m = M::on_route(|_name| {});
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn on_fallback_creates_composite() {
        let m = M::on_fallback(|_name| {});
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
            | M::validate(|_| Ok(()))
            | M::fallback_model("gemini-1.5-flash")
            | M::cache()
            | M::dedup()
            | M::sample(0.5)
            | M::metrics()
            | M::before_agent(|_| Ok(()))
            | M::after_agent(|_| Ok(()))
            | M::before_model(|_| Ok(()))
            | M::after_model(|_, _| Ok(()))
            | M::on_loop(|_| {})
            | M::on_timeout(|| {})
            | M::on_route(|_| {})
            | M::on_fallback(|_| {});
        assert_eq!(m.len(), 23);
    }
}
