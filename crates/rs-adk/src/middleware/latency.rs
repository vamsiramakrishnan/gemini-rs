//! Latency tracking middleware for tool calls.

use std::time::{Duration, Instant};

use async_trait::async_trait;

use rs_genai::prelude::FunctionCall;

use super::Middleware;
use crate::error::{AgentError, ToolError};

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
    /// Create a new latency middleware with empty records.
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
