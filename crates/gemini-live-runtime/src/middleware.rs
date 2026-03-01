//! Middleware trait and chain — wraps agent execution at lifecycle points.

use std::sync::Arc;

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

    pub async fn run_on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_event(event).await?;
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

// ── Built-in Middleware ──

/// Logs agent and tool lifecycle events.
pub struct LogMiddleware {
    pub name: String,
}

impl LogMiddleware {
    pub fn new() -> Self {
        Self {
            name: "log".to_string(),
        }
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
        &self.name
    }

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        // In production, use tracing::info! here
        Ok(())
    }

    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> {
        // tracing::info!(tool = %call.name, "tool call started");
        Ok(())
    }

    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
    ) -> Result<(), AgentError> {
        // tracing::info!(tool = %call.name, "tool call completed");
        Ok(())
    }
}

/// Tracks latency of tool calls.
pub struct LatencyMiddleware {
    pub name: String,
}

impl LatencyMiddleware {
    pub fn new() -> Self {
        Self {
            name: "latency".to_string(),
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(log.name, "log");
    }

    #[test]
    fn latency_middleware_defaults() {
        let lat = LatencyMiddleware::new();
        assert_eq!(lat.name, "latency");
    }
}
