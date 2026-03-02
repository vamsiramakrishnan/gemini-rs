//! Plugin system — lifecycle hooks with control-flow capabilities.
//!
//! Plugins are a superset of middleware: they can observe AND control agent
//! execution. A plugin can deny a tool call, short-circuit with a custom
//! response, or simply continue. The `PluginManager` runs plugins in order
//! and respects the first non-Continue result.

mod logging;
mod security;

pub use logging::LoggingPlugin;
pub use security::{AllowAllPolicy, DenyListPolicy, PolicyEngine, PolicyOutcome, SecurityPlugin};

use std::sync::Arc;

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use crate::context::InvocationContext;
use crate::events::Event;

/// The result of a plugin hook — controls whether execution continues.
#[derive(Debug, Clone)]
pub enum PluginResult {
    /// Continue with normal execution.
    Continue,
    /// Short-circuit execution with a custom value (e.g., cached response).
    ShortCircuit(serde_json::Value),
    /// Deny the action with a reason string.
    Deny(String),
}

impl PluginResult {
    /// Returns true if this result is `Continue`.
    pub fn is_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }

    /// Returns true if this result is `Deny`.
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny(_))
    }

    /// Returns true if this result is `ShortCircuit`.
    pub fn is_short_circuit(&self) -> bool {
        matches!(self, Self::ShortCircuit(_))
    }
}

/// Plugin trait — lifecycle hooks with control-flow capabilities.
///
/// Unlike `Middleware` (which is observe-only), plugins can deny or
/// short-circuit execution. All hooks default to `PluginResult::Continue`.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Plugin name for logging/debugging.
    fn name(&self) -> &str;

    /// Called before an agent starts execution.
    async fn before_agent(&self, _ctx: &InvocationContext) -> PluginResult {
        PluginResult::Continue
    }

    /// Called after an agent completes execution.
    async fn after_agent(&self, _ctx: &InvocationContext) -> PluginResult {
        PluginResult::Continue
    }

    /// Called before a tool is executed. Return `Deny` to prevent execution.
    async fn before_tool(
        &self,
        _call: &FunctionCall,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        PluginResult::Continue
    }

    /// Called after a tool completes. Can transform or deny the result.
    async fn after_tool(
        &self,
        _call: &FunctionCall,
        _result: &serde_json::Value,
        _ctx: &InvocationContext,
    ) -> PluginResult {
        PluginResult::Continue
    }

    /// Called when an event is emitted.
    async fn on_event(&self, _event: &Event, _ctx: &InvocationContext) -> PluginResult {
        PluginResult::Continue
    }
}

/// Manages an ordered list of plugins, running them in sequence.
///
/// On each hook, plugins run in order. The first non-Continue result
/// short-circuits the remaining plugins.
#[derive(Clone, Default)]
pub struct PluginManager {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl PluginManager {
    /// Create an empty plugin manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a plugin to the manager.
    pub fn add(&mut self, plugin: Arc<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns true if no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Run before_agent hooks. Returns first non-Continue result, or Continue.
    pub async fn run_before_agent(&self, ctx: &InvocationContext) -> PluginResult {
        for plugin in &self.plugins {
            let result = plugin.before_agent(ctx).await;
            if !result.is_continue() {
                return result;
            }
        }
        PluginResult::Continue
    }

    /// Run after_agent hooks. Returns first non-Continue result, or Continue.
    pub async fn run_after_agent(&self, ctx: &InvocationContext) -> PluginResult {
        for plugin in self.plugins.iter().rev() {
            let result = plugin.after_agent(ctx).await;
            if !result.is_continue() {
                return result;
            }
        }
        PluginResult::Continue
    }

    /// Run before_tool hooks. Returns first non-Continue result, or Continue.
    pub async fn run_before_tool(
        &self,
        call: &FunctionCall,
        ctx: &InvocationContext,
    ) -> PluginResult {
        for plugin in &self.plugins {
            let result = plugin.before_tool(call, ctx).await;
            if !result.is_continue() {
                return result;
            }
        }
        PluginResult::Continue
    }

    /// Run after_tool hooks. Returns first non-Continue result, or Continue.
    pub async fn run_after_tool(
        &self,
        call: &FunctionCall,
        value: &serde_json::Value,
        ctx: &InvocationContext,
    ) -> PluginResult {
        for plugin in self.plugins.iter().rev() {
            let result = plugin.after_tool(call, value, ctx).await;
            if !result.is_continue() {
                return result;
            }
        }
        PluginResult::Continue
    }

    /// Run on_event hooks. Returns first non-Continue result, or Continue.
    pub async fn run_on_event(
        &self,
        event: &Event,
        ctx: &InvocationContext,
    ) -> PluginResult {
        for plugin in &self.plugins {
            let result = plugin.on_event(event, ctx).await;
            if !result.is_continue() {
                return result;
            }
        }
        PluginResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_result_helpers() {
        assert!(PluginResult::Continue.is_continue());
        assert!(!PluginResult::Continue.is_deny());
        assert!(!PluginResult::Continue.is_short_circuit());

        assert!(PluginResult::Deny("nope".into()).is_deny());
        assert!(!PluginResult::Deny("nope".into()).is_continue());

        let val = serde_json::json!({"cached": true});
        assert!(PluginResult::ShortCircuit(val).is_short_circuit());
    }

    #[test]
    fn plugin_manager_empty() {
        let pm = PluginManager::new();
        assert!(pm.is_empty());
        assert_eq!(pm.len(), 0);
    }

    #[test]
    fn plugin_manager_add() {
        let mut pm = PluginManager::new();
        pm.add(Arc::new(LoggingPlugin::new()));
        assert_eq!(pm.len(), 1);
        assert!(!pm.is_empty());
    }

    #[test]
    fn plugin_is_object_safe() {
        fn _assert(_: &dyn Plugin) {}
    }

    struct DenyPlugin;

    #[async_trait]
    impl Plugin for DenyPlugin {
        fn name(&self) -> &str {
            "deny"
        }

        async fn before_tool(
            &self,
            _call: &FunctionCall,
            _ctx: &InvocationContext,
        ) -> PluginResult {
            PluginResult::Deny("blocked by policy".into())
        }
    }

    struct CountPlugin {
        count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Plugin for CountPlugin {
        fn name(&self) -> &str {
            "count"
        }

        async fn before_tool(
            &self,
            _call: &FunctionCall,
            _ctx: &InvocationContext,
        ) -> PluginResult {
            self.count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            PluginResult::Continue
        }
    }

    // Test that a deny plugin prevents later plugins from running
    #[tokio::test]
    async fn plugin_manager_deny_short_circuits() {
        use tokio::sync::broadcast;

        let count_plugin = Arc::new(CountPlugin {
            count: std::sync::atomic::AtomicU32::new(0),
        });

        let mut pm = PluginManager::new();
        pm.add(Arc::new(DenyPlugin));
        pm.add(count_plugin.clone());

        // Create a minimal InvocationContext for testing
        let (evt_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(crate::test_helpers::MockWriter);
        let session =
            crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        let call = FunctionCall {
            name: "dangerous_tool".into(),
            args: serde_json::json!({}),
            id: None,
        };

        let result = pm.run_before_tool(&call, &ctx).await;
        assert!(result.is_deny());

        // CountPlugin should NOT have been called
        assert_eq!(
            count_plugin.count.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
