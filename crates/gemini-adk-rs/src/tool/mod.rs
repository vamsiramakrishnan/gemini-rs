//! Tool dispatch — regular, streaming, and input-streaming tools.

pub mod dispatcher;
pub mod simple;
pub mod typed;

pub use dispatcher::*;
pub use simple::*;
pub use typed::*;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent_session::InputEvent;
use crate::error::ToolError;

/// A regular tool — called once, returns a result.
///
/// # Examples
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use gemini_adk_rs::tool::ToolFunction;
/// use gemini_adk_rs::error::ToolError;
///
/// struct MyTool;
///
/// #[async_trait]
/// impl ToolFunction for MyTool {
///     fn name(&self) -> &str { "my_tool" }
///     fn description(&self) -> &str { "Does something useful" }
///     fn parameters(&self) -> Option<serde_json::Value> { None }
///     async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
///         Ok(serde_json::json!({"status": "ok"}))
///     }
/// }
/// ```
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    /// The unique name of this tool.
    fn name(&self) -> &str;
    /// Human-readable description of what this tool does.
    fn description(&self) -> &str;
    /// JSON Schema for the tool's input parameters, or `None` if parameterless.
    fn parameters(&self) -> Option<serde_json::Value>;
    /// Execute the tool with the given arguments and return the result.
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError>;
}

/// A streaming tool — runs in background, yields multiple results.
#[async_trait]
pub trait StreamingTool: Send + Sync + 'static {
    /// The unique name of this tool.
    fn name(&self) -> &str;
    /// Human-readable description of what this tool does.
    fn description(&self) -> &str;
    /// JSON Schema for the tool's input parameters, or `None` if parameterless.
    fn parameters(&self) -> Option<serde_json::Value>;
    /// Execute the tool, sending intermediate results via `yield_tx`.
    async fn run(
        &self,
        args: serde_json::Value,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}

/// An input-streaming tool — receives duplicated live input while running.
#[async_trait]
pub trait InputStreamingTool: Send + Sync + 'static {
    /// The unique name of this tool.
    fn name(&self) -> &str;
    /// Human-readable description of what this tool does.
    fn description(&self) -> &str;
    /// JSON Schema for the tool's input parameters, or `None` if parameterless.
    fn parameters(&self) -> Option<serde_json::Value>;
    /// Execute the tool, receiving live input via `input_rx` and sending results via `yield_tx`.
    async fn run(
        &self,
        args: serde_json::Value,
        input_rx: broadcast::Receiver<InputEvent>,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}

/// Classification of a registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// A one-shot tool that returns a single result.
    Regular,
    /// A tool that yields multiple results over time.
    Streaming,
    /// A tool that receives live input while producing results.
    InputStream,
}

/// Unified tool storage.
pub enum ToolKind {
    /// A regular one-shot function tool.
    Function(Arc<dyn ToolFunction>),
    /// A streaming tool that yields multiple results.
    Streaming(Arc<dyn StreamingTool>),
    /// An input-streaming tool that receives live input.
    InputStream(Arc<dyn InputStreamingTool>),
}

/// Handle to a running streaming tool.
pub struct ActiveStreamingTool {
    /// The spawned task handle.
    pub task: JoinHandle<()>,
    /// Token to cancel this streaming tool.
    pub cancel: CancellationToken,
}

/// Default timeout for tool execution (30 seconds).
pub(crate) const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_genai_rs::prelude::FunctionCall;
    use serde_json::json;

    struct MockTool;

    #[async_trait]
    impl ToolFunction for MockTool {
        fn name(&self) -> &str {
            "mock_tool"
        }
        fn description(&self) -> &str {
            "A mock tool"
        }
        fn parameters(&self) -> Option<serde_json::Value> {
            None
        }
        async fn call(&self, _args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            Ok(json!({"result": "ok"}))
        }
    }

    #[tokio::test]
    async fn register_and_call_function_tool() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        let result = dispatcher
            .call_function("mock_tool", json!({}))
            .await
            .unwrap();
        assert_eq!(result["result"], "ok");
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_error() {
        let dispatcher = ToolDispatcher::new();
        let result = dispatcher.call_function("nonexistent", json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn to_tool_declarations() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        let decls = dispatcher.to_tool_declarations();
        assert_eq!(decls.len(), 1);
    }

    #[test]
    fn classify_tool() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        assert_eq!(dispatcher.classify("mock_tool"), Some(ToolClass::Regular));
        assert_eq!(dispatcher.classify("nonexistent"), None);
    }

    #[test]
    fn empty_dispatcher() {
        let dispatcher = ToolDispatcher::new();
        assert!(dispatcher.is_empty());
        assert_eq!(dispatcher.len(), 0);
        assert!(dispatcher.to_tool_declarations().is_empty());
    }

    #[test]
    fn build_response_success() {
        let call = FunctionCall {
            name: "test".to_string(),
            args: json!({}),
            id: Some("call-1".to_string()),
        };
        let resp = ToolDispatcher::build_response(&call, Ok(json!({"ok": true})));
        assert_eq!(resp.name, "test");
        assert_eq!(resp.response["ok"], true);
    }

    #[test]
    fn build_response_error() {
        let call = FunctionCall {
            name: "test".to_string(),
            args: json!({}),
            id: Some("call-1".to_string()),
        };
        let resp = ToolDispatcher::build_response(
            &call,
            Err(ToolError::ExecutionFailed("boom".to_string())),
        );
        assert!(resp.response["error"].as_str().unwrap().contains("boom"));
    }

    #[test]
    fn tool_dispatcher_implements_tool_provider() {
        use gemini_genai_rs::prelude::ToolProvider;
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        let decls = dispatcher.declarations();
        assert_eq!(decls.len(), 1);
    }

    #[tokio::test]
    async fn simple_tool_closure() {
        let tool = SimpleTool::new(
            "add",
            "Add two numbers",
            Some(
                json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}),
            ),
            |args| async move {
                let a = args["a"].as_f64().unwrap_or(0.0);
                let b = args["b"].as_f64().unwrap_or(0.0);
                Ok(json!({"sum": a + b}))
            },
        );

        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(tool));
        let result = dispatcher
            .call_function("add", json!({"a": 3, "b": 4}))
            .await
            .unwrap();
        assert_eq!(result["sum"], 7.0);
    }

    // --- TypedTool tests ---

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct WeatherArgs {
        /// The city to get weather for
        city: String,
        /// Temperature units (celsius or fahrenheit)
        #[serde(default = "default_units")]
        units: String,
    }

    fn default_units() -> String {
        "celsius".to_string()
    }

    #[test]
    fn typed_tool_auto_generates_schema() {
        let tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |_args: WeatherArgs| async move { Ok(json!({})) },
        );

        let params = tool.parameters().expect("should have parameters");

        // The schema should be an object type with "city" and "units" properties
        let props = &params["properties"];
        assert!(
            props.get("city").is_some(),
            "schema should contain 'city' property"
        );
        assert!(
            props.get("units").is_some(),
            "schema should contain 'units' property"
        );

        // "city" should be required (no default), "units" has a default so may not be
        let required = params["required"]
            .as_array()
            .expect("should have required array");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_names.contains(&"city"), "city should be required");
    }

    #[tokio::test]
    async fn typed_tool_deserializes_args() {
        let tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |args: WeatherArgs| async move {
                Ok(json!({
                    "temp": 22,
                    "city": args.city,
                    "units": args.units,
                }))
            },
        );

        let result = tool
            .call(json!({"city": "London", "units": "fahrenheit"}))
            .await
            .unwrap();
        assert_eq!(result["city"], "London");
        assert_eq!(result["units"], "fahrenheit");
        assert_eq!(result["temp"], 22);
    }

    #[tokio::test]
    async fn typed_tool_invalid_args_returns_error() {
        let tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |_args: WeatherArgs| async move { Ok(json!({})) },
        );

        // Missing required field "city"
        let result = tool.call(json!({"units": "celsius"})).await;
        assert!(result.is_err(), "should fail with missing required field");
        let err = result.unwrap_err();
        match &err {
            ToolError::InvalidArgs(msg) => {
                assert!(
                    msg.contains("city"),
                    "error message should mention the missing field: {msg}"
                );
            }
            other => panic!("expected ToolError::InvalidArgs, got: {other:?}"),
        }

        // Wrong type for "city" (number instead of string)
        let result = tool.call(json!({"city": 12345})).await;
        assert!(result.is_err(), "should fail with wrong type");
    }

    #[tokio::test]
    async fn typed_tool_registers_in_dispatcher() {
        let tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |args: WeatherArgs| async move { Ok(json!({"city": args.city})) },
        );

        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(tool));

        assert_eq!(dispatcher.classify("get_weather"), Some(ToolClass::Regular));
        assert_eq!(dispatcher.len(), 1);

        let result = dispatcher
            .call_function("get_weather", json!({"city": "Paris"}))
            .await
            .unwrap();
        assert_eq!(result["city"], "Paris");

        // Verify it appears in tool declarations
        let decls = dispatcher.to_tool_declarations();
        assert_eq!(decls.len(), 1);
    }

    // --- Timeout and cancellation tests ---

    /// A tool that sleeps forever (until cancelled/timed out).
    struct SlowTool;

    #[async_trait]
    impl ToolFunction for SlowTool {
        fn name(&self) -> &str {
            "slow_tool"
        }
        fn description(&self) -> &str {
            "A tool that never completes"
        }
        fn parameters(&self) -> Option<serde_json::Value> {
            None
        }
        async fn call(&self, _args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            // Sleep effectively forever
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(json!({"result": "should never reach here"}))
        }
    }

    #[tokio::test]
    async fn tool_timeout_returns_error() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(SlowTool));

        let timeout = Duration::from_millis(50);
        let result = dispatcher
            .call_function_with_timeout("slow_tool", json!({}), timeout)
            .await;

        match result {
            Err(ToolError::Timeout(d)) => assert_eq!(d, timeout),
            other => panic!("expected ToolError::Timeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_completes_before_timeout() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));

        let result = dispatcher
            .call_function_with_timeout("mock_tool", json!({}), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result["result"], "ok");
    }

    #[tokio::test]
    async fn tool_cancelled_returns_error() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(SlowTool));

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Cancel after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = dispatcher
            .call_function_with_cancel("slow_tool", json!({}), cancel)
            .await;

        match result {
            Err(ToolError::Cancelled) => {} // expected
            other => panic!("expected ToolError::Cancelled, got: {other:?}"),
        }
    }

    #[test]
    fn default_timeout_is_30s() {
        let dispatcher = ToolDispatcher::new();
        assert_eq!(dispatcher.default_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn with_timeout_overrides_default() {
        let dispatcher = ToolDispatcher::new().with_timeout(Duration::from_secs(10));
        assert_eq!(dispatcher.default_timeout(), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn call_function_uses_default_timeout() {
        // Set a very short default timeout so the slow tool times out
        let mut dispatcher = ToolDispatcher::new().with_timeout(Duration::from_millis(50));
        dispatcher.register_function(Arc::new(SlowTool));

        let result = dispatcher.call_function("slow_tool", json!({})).await;

        match result {
            Err(ToolError::Timeout(d)) => assert_eq!(d, Duration::from_millis(50)),
            other => panic!("expected ToolError::Timeout, got: {other:?}"),
        }
    }
}
