//! Non-blocking tool execution infrastructure.
//!
//! Provides [`BackgroundToolTracker`] for managing in-flight background tool
//! executions, [`ResultFormatter`] for customizing tool response formatting,
//! and [`ToolExecutionMode`] for declaring whether a tool runs synchronously
//! or in the background.

use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use gemini_live::prelude::FunctionCall;

use crate::error::ToolError;

// ---------------------------------------------------------------------------
// ResultFormatter trait
// ---------------------------------------------------------------------------

/// Formats tool responses for background execution lifecycle.
///
/// Implementations control the shape of JSON values sent back to the model
/// at each stage of a background tool's lifecycle: when the tool starts
/// running, when it completes (or fails), and when it is cancelled.
pub trait ResultFormatter: Send + Sync + 'static {
    /// Format the immediate acknowledgment sent when a background tool starts.
    fn format_running(&self, call: &FunctionCall) -> Value;

    /// Format the final result after tool completes or fails.
    fn format_result(&self, call: &FunctionCall, result: Result<Value, ToolError>) -> Value;

    /// Format a cancellation response.
    fn format_cancelled(&self, call_id: &str) -> Value;
}

// ---------------------------------------------------------------------------
// DefaultResultFormatter
// ---------------------------------------------------------------------------

/// Default formatter that wraps results in a status object.
///
/// Produces JSON like:
/// ```json
/// { "status": "running", "tool": "search" }
/// { "status": "completed", "tool": "search", "result": { ... } }
/// { "status": "error", "tool": "search", "error": "..." }
/// { "status": "cancelled", "call_id": "abc123" }
/// ```
pub struct DefaultResultFormatter;

impl ResultFormatter for DefaultResultFormatter {
    fn format_running(&self, call: &FunctionCall) -> Value {
        serde_json::json!({
            "status": "running",
            "tool": call.name,
        })
    }

    fn format_result(&self, call: &FunctionCall, result: Result<Value, ToolError>) -> Value {
        match result {
            Ok(value) => serde_json::json!({
                "status": "completed",
                "tool": call.name,
                "result": value,
            }),
            Err(e) => serde_json::json!({
                "status": "error",
                "tool": call.name,
                "error": e.to_string(),
            }),
        }
    }

    fn format_cancelled(&self, call_id: &str) -> Value {
        serde_json::json!({
            "status": "cancelled",
            "call_id": call_id,
        })
    }
}

// ---------------------------------------------------------------------------
// ToolExecutionMode
// ---------------------------------------------------------------------------

/// Execution mode for a tool.
///
/// - [`Standard`](ToolExecutionMode::Standard): the tool runs inline and the
///   model waits for the result before continuing.
/// - [`Background`](ToolExecutionMode::Background): the tool is spawned as a
///   background task. An immediate "running" acknowledgment is sent to the
///   model, and the final result is delivered asynchronously when the task
///   completes.
///
/// # With the L2 Fluent API
///
/// ```rust,ignore
/// Live::builder()
///     .tools(dispatcher)
///     .tool_background("search_kb")           // uses DefaultResultFormatter
///     .tool_background_with_formatter(         // custom formatter
///         "analyze",
///         Arc::new(MyFormatter),
///     )
///     .connect_vertex(project, location, token)
///     .await?;
/// ```
///
/// # With the L1 Builder
///
/// ```rust,ignore
/// LiveSessionBuilder::new(config)
///     .dispatcher(dispatcher)
///     .tool_execution_mode("search_kb", ToolExecutionMode::Background {
///         formatter: None,
///     })
///     .connect()
///     .await?;
/// ```
#[derive(Clone, Default)]
pub enum ToolExecutionMode {
    /// The tool runs inline (blocking the model turn until complete).
    #[default]
    Standard,

    /// The tool runs in the background.
    ///
    /// An optional [`ResultFormatter`] controls how acknowledgment, result,
    /// and cancellation messages are shaped. When `None`, the
    /// [`DefaultResultFormatter`] is used.
    ///
    /// The `scheduling` field controls how the model handles async results:
    /// - `Interrupt`: halts current output, immediately reports the result
    /// - `WhenIdle`: waits until current output finishes before handling
    /// - `Silent`: integrates the result without notifying the user
    Background {
        /// Custom formatter for background tool results, or `None` for the default.
        formatter: Option<Arc<dyn ResultFormatter>>,
        /// How the model should handle the async result. Defaults to `WhenIdle`.
        scheduling: Option<gemini_live::prelude::FunctionResponseScheduling>,
    },
}

impl std::fmt::Debug for ToolExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "Standard"),
            Self::Background {
                formatter,
                scheduling,
            } => {
                write!(
                    f,
                    "Background(formatter={}, scheduling={:?})",
                    formatter.is_some(),
                    scheduling
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BackgroundToolTracker
// ---------------------------------------------------------------------------

/// Tracks in-flight background tool executions for cancellation.
///
/// Uses [`DashMap`] internally so that spawned tasks can remove themselves
/// upon completion while the control lane concurrently spawns or cancels
/// other tasks.
pub struct BackgroundToolTracker {
    tasks: DashMap<String, (JoinHandle<()>, CancellationToken)>,
}

impl BackgroundToolTracker {
    /// Create a new, empty tracker.
    pub fn new() -> Self {
        Self {
            tasks: DashMap::new(),
        }
    }

    /// Register a spawned background task.
    ///
    /// The `call_id` is the unique identifier for the function call (usually
    /// from [`FunctionCall::id`]). The caller provides both a
    /// [`JoinHandle`] (for aborting) and a [`CancellationToken`] (for
    /// cooperative cancellation).
    pub fn spawn(&self, call_id: String, task: JoinHandle<()>, cancel: CancellationToken) {
        self.tasks.insert(call_id, (task, cancel));
    }

    /// Cancel specific tool calls by their IDs.
    ///
    /// For each matching ID the cancellation token is triggered **and** the
    /// task handle is aborted, providing belt-and-suspenders cleanup.
    /// Non-existent IDs are silently ignored.
    pub fn cancel(&self, call_ids: &[String]) {
        for id in call_ids {
            if let Some((_, (handle, token))) = self.tasks.remove(id) {
                token.cancel();
                handle.abort();
            }
        }
    }

    /// Cancel all in-flight background tasks.
    ///
    /// Useful during session shutdown to ensure no orphaned tasks remain.
    pub fn cancel_all(&self) {
        let keys: Vec<String> = self.tasks.iter().map(|r| r.key().clone()).collect();
        for key in keys {
            if let Some((_, (handle, token))) = self.tasks.remove(&key) {
                token.cancel();
                handle.abort();
            }
        }
    }

    /// Get IDs of active background tool calls.
    pub fn active_ids(&self) -> Vec<String> {
        self.tasks.iter().map(|r| r.key().clone()).collect()
    }

    /// Remove a completed task (called when background task finishes).
    ///
    /// This is typically invoked by the spawned task itself to clean up the
    /// tracker entry once execution is done.
    pub fn remove(&self, call_id: &str) {
        self.tasks.remove(call_id);
    }

    /// Number of active background tasks.
    pub fn active_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Default for BackgroundToolTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // BackgroundToolTracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn tracker_new_is_empty() {
        let tracker = BackgroundToolTracker::new();
        assert_eq!(tracker.active_count(), 0);
        assert!(tracker.active_ids().is_empty());
    }

    #[tokio::test]
    async fn spawn_shows_active_id() {
        let tracker = BackgroundToolTracker::new();
        let token = CancellationToken::new();
        let t = token.clone();
        let handle = tokio::spawn(async move {
            t.cancelled().await;
        });
        tracker.spawn("call1".into(), handle, token.clone());

        let ids = tracker.active_ids();
        assert_eq!(ids, vec!["call1".to_string()]);

        // Clean up
        token.cancel();
    }

    #[tokio::test]
    async fn spawn_increments_active_count() {
        let tracker = BackgroundToolTracker::new();

        let token1 = CancellationToken::new();
        let t1 = token1.clone();
        let h1 = tokio::spawn(async move {
            t1.cancelled().await;
        });
        tracker.spawn("call1".into(), h1, token1.clone());

        let token2 = CancellationToken::new();
        let t2 = token2.clone();
        let h2 = tokio::spawn(async move {
            t2.cancelled().await;
        });
        tracker.spawn("call2".into(), h2, token2.clone());

        assert_eq!(tracker.active_count(), 2);

        // Clean up
        token1.cancel();
        token2.cancel();
    }

    #[tokio::test]
    async fn cancel_removes_task_and_cancels_token() {
        let tracker = BackgroundToolTracker::new();
        let token = CancellationToken::new();
        let t = token.clone();
        let handle = tokio::spawn(async move {
            t.cancelled().await;
        });
        tracker.spawn("call1".into(), handle, token.clone());

        assert_eq!(tracker.active_count(), 1);

        tracker.cancel(&["call1".into()]);

        assert_eq!(tracker.active_count(), 0);
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_all_clears_all_tasks() {
        let tracker = BackgroundToolTracker::new();

        let token1 = CancellationToken::new();
        let t1 = token1.clone();
        let h1 = tokio::spawn(async move {
            t1.cancelled().await;
        });
        tracker.spawn("call1".into(), h1, token1.clone());

        let token2 = CancellationToken::new();
        let t2 = token2.clone();
        let h2 = tokio::spawn(async move {
            t2.cancelled().await;
        });
        tracker.spawn("call2".into(), h2, token2.clone());

        let token3 = CancellationToken::new();
        let t3 = token3.clone();
        let h3 = tokio::spawn(async move {
            t3.cancelled().await;
        });
        tracker.spawn("call3".into(), h3, token3.clone());

        assert_eq!(tracker.active_count(), 3);

        tracker.cancel_all();

        assert_eq!(tracker.active_count(), 0);
        assert!(token1.is_cancelled());
        assert!(token2.is_cancelled());
        assert!(token3.is_cancelled());
    }

    #[tokio::test]
    async fn remove_cleans_up_completed_task() {
        let tracker = BackgroundToolTracker::new();
        let token = CancellationToken::new();
        let t = token.clone();
        let handle = tokio::spawn(async move {
            t.cancelled().await;
        });
        tracker.spawn("call1".into(), handle, token.clone());

        assert_eq!(tracker.active_count(), 1);

        tracker.remove("call1");

        assert_eq!(tracker.active_count(), 0);
        assert!(tracker.active_ids().is_empty());

        // Clean up — token not cancelled by remove (that's intentional)
        token.cancel();
    }

    #[test]
    fn cancel_nonexistent_id_is_noop() {
        let tracker = BackgroundToolTracker::new();
        // Should not panic
        tracker.cancel(&["nonexistent".into()]);
        assert_eq!(tracker.active_count(), 0);
    }

    // -----------------------------------------------------------------------
    // DefaultResultFormatter tests
    // -----------------------------------------------------------------------

    fn make_call(name: &str) -> FunctionCall {
        FunctionCall {
            name: name.to_string(),
            args: serde_json::json!({"query": "test"}),
            id: Some("fc_123".to_string()),
        }
    }

    #[test]
    fn format_running_output() {
        let fmt = DefaultResultFormatter;
        let call = make_call("search");
        let result = fmt.format_running(&call);

        assert_eq!(result["status"], "running");
        assert_eq!(result["tool"], "search");
    }

    #[test]
    fn format_result_ok() {
        let fmt = DefaultResultFormatter;
        let call = make_call("search");
        let value = serde_json::json!({"items": [1, 2, 3]});
        let result = fmt.format_result(&call, Ok(value.clone()));

        assert_eq!(result["status"], "completed");
        assert_eq!(result["tool"], "search");
        assert_eq!(result["result"], value);
    }

    #[test]
    fn format_result_err() {
        let fmt = DefaultResultFormatter;
        let call = make_call("search");
        let err = ToolError::ExecutionFailed("connection timeout".into());
        let result = fmt.format_result(&call, Err(err));

        assert_eq!(result["status"], "error");
        assert_eq!(result["tool"], "search");
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("connection timeout"));
    }

    #[test]
    fn format_cancelled_output() {
        let fmt = DefaultResultFormatter;
        let result = fmt.format_cancelled("fc_456");

        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["call_id"], "fc_456");
    }

    // -----------------------------------------------------------------------
    // ToolExecutionMode tests
    // -----------------------------------------------------------------------

    #[test]
    fn tool_execution_mode_default_is_standard() {
        let mode = ToolExecutionMode::default();
        assert!(matches!(mode, ToolExecutionMode::Standard));
    }

    #[test]
    fn tool_execution_mode_debug_standard() {
        let mode = ToolExecutionMode::Standard;
        assert_eq!(format!("{:?}", mode), "Standard");
    }

    #[test]
    fn tool_execution_mode_debug_background_none() {
        let mode = ToolExecutionMode::Background {
            formatter: None,
            scheduling: None,
        };
        assert_eq!(
            format!("{:?}", mode),
            "Background(formatter=false, scheduling=None)"
        );
    }

    #[test]
    fn tool_execution_mode_debug_background_some() {
        let mode = ToolExecutionMode::Background {
            formatter: Some(Arc::new(DefaultResultFormatter)),
            scheduling: None,
        };
        assert_eq!(
            format!("{:?}", mode),
            "Background(formatter=true, scheduling=None)"
        );
    }
}
