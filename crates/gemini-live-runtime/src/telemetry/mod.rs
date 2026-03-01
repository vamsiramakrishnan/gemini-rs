//! Agent-level observability — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! Mirrors the wire crate's telemetry layer but for agent lifecycle operations.
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: OTel spans and structured logging
//! - `metrics`: Prometheus metric definitions

use async_trait::async_trait;

use crate::middleware::Middleware;

pub mod logging;
pub mod metrics;
pub mod spans;

/// Auto-registered middleware that calls telemetry functions.
/// Zero-overhead when `tracing-support` and `metrics` features are disabled.
///
/// This is automatically prepended to every LlmAgent's middleware chain
/// at build time. Task 6 will fill in the actual hook implementations.
pub struct TelemetryMiddleware {
    agent_name: String,
}

impl TelemetryMiddleware {
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
        }
    }

    /// Returns the agent name this middleware is tracking.
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }
}

#[async_trait]
impl Middleware for TelemetryMiddleware {
    fn name(&self) -> &str {
        "telemetry"
    }
}
