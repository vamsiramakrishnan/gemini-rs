//! Tool dispatch — regular, streaming, and input-streaming tools.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use gemini_live_wire::prelude::{FunctionCall, FunctionDeclaration, FunctionResponse, Tool};

use crate::agent_session::InputEvent;
use crate::error::ToolError;

/// A regular tool — called once, returns a result.
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError>;
}

/// A streaming tool — runs in background, yields multiple results.
#[async_trait]
pub trait StreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn run(
        &self,
        args: serde_json::Value,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}

/// An input-streaming tool — receives duplicated live input while running.
#[async_trait]
pub trait InputStreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
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
    Regular,
    Streaming,
    InputStream,
}

/// Unified tool storage.
pub enum ToolKind {
    Function(Arc<dyn ToolFunction>),
    Streaming(Arc<dyn StreamingTool>),
    InputStream(Arc<dyn InputStreamingTool>),
}

/// Handle to a running streaming tool.
pub struct ActiveStreamingTool {
    pub task: JoinHandle<()>,
    pub cancel: CancellationToken,
}

/// Routes function calls to the right tool implementation.
pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
}

impl ToolDispatcher {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            active: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Register a regular function tool.
    pub fn register_function(&mut self, tool: Arc<dyn ToolFunction>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::Function(tool));
    }

    /// Register a streaming tool.
    pub fn register_streaming(&mut self, tool: Arc<dyn StreamingTool>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::Streaming(tool));
    }

    /// Register an input-streaming tool.
    pub fn register_input_streaming(&mut self, tool: Arc<dyn InputStreamingTool>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::InputStream(tool));
    }

    /// Classify a tool by name.
    pub fn classify(&self, name: &str) -> Option<ToolClass> {
        self.tools.get(name).map(|t| match t {
            ToolKind::Function(_) => ToolClass::Regular,
            ToolKind::Streaming(_) => ToolClass::Streaming,
            ToolKind::InputStream(_) => ToolClass::InputStream,
        })
    }

    /// Call a regular function tool by name.
    pub async fn call_function(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        match self.tools.get(name) {
            Some(ToolKind::Function(f)) => f.call(args).await,
            Some(_) => Err(ToolError::Other(format!(
                "{name} is not a regular function tool"
            ))),
            None => Err(ToolError::NotFound(name.to_string())),
        }
    }

    /// Build a FunctionResponse from a FunctionCall result.
    pub fn build_response(
        call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> FunctionResponse {
        match result {
            Ok(value) => FunctionResponse {
                name: call.name.clone(),
                response: value,
                id: call.id.clone(),
            },
            Err(e) => FunctionResponse {
                name: call.name.clone(),
                response: serde_json::json!({"error": e.to_string()}),
                id: call.id.clone(),
            },
        }
    }

    /// Cancel a streaming tool by name.
    pub async fn cancel_streaming(&self, name: &str) {
        let mut active = self.active.lock().await;
        if let Some(tool) = active.remove(name) {
            tool.cancel.cancel();
            tool.task.abort();
        }
    }

    /// Cancel streaming tools by IDs.
    pub async fn cancel_by_ids(&self, ids: &[String]) {
        let mut active = self.active.lock().await;
        for id in ids {
            if let Some(tool) = active.remove(id.as_str()) {
                tool.cancel.cancel();
                tool.task.abort();
            }
        }
    }

    /// Generate Tool declarations for the setup message.
    pub fn to_tool_declarations(&self) -> Vec<Tool> {
        let declarations: Vec<FunctionDeclaration> = self
            .tools
            .values()
            .map(|t| {
                let (name, desc, params) = match t {
                    ToolKind::Function(f) => (f.name(), f.description(), f.parameters()),
                    ToolKind::Streaming(s) => (s.name(), s.description(), s.parameters()),
                    ToolKind::InputStream(i) => (i.name(), i.description(), i.parameters()),
                };
                FunctionDeclaration {
                    name: name.to_string(),
                    description: desc.to_string(),
                    parameters: params,
                }
            })
            .collect();

        if declarations.is_empty() {
            vec![]
        } else {
            vec![Tool::functions(declarations)]
        }
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl gemini_live_wire::prelude::ToolProvider for ToolDispatcher {
    fn declarations(&self) -> Vec<gemini_live_wire::prelude::Tool> {
        self.to_tool_declarations()
    }
}

/// Simple function tool that wraps an async closure.
pub struct SimpleTool {
    name: String,
    description: String,
    parameters: Option<serde_json::Value>,
    handler: Box<
        dyn Fn(serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send>>
            + Send
            + Sync,
    >,
}

impl SimpleTool {
    /// Create a new simple function tool.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
        handler: F,
    ) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            handler: Box::new(move |args| Box::pin(handler(args))),
        }
    }
}

#[async_trait]
impl ToolFunction for SimpleTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Option<serde_json::Value> {
        self.parameters.clone()
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        (self.handler)(args).await
    }
}

/// Type-safe function tool with auto-generated JSON Schema.
///
/// Unlike [`SimpleTool`] which takes raw `serde_json::Value` arguments and
/// requires a manually written schema, `TypedTool` auto-generates the JSON
/// Schema from a struct that derives [`schemars::JsonSchema`] and deserializes
/// the arguments into that struct before calling the handler.
///
/// # Example
///
/// ```ignore
/// use schemars::JsonSchema;
/// use serde::Deserialize;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct WeatherArgs {
///     /// The city to get weather for
///     city: String,
/// }
///
/// let tool = TypedTool::new::<WeatherArgs>(
///     "get_weather",
///     "Get current weather for a city",
///     |args: WeatherArgs| async move {
///         Ok(serde_json::json!({ "temp": 22, "city": args.city }))
///     },
/// );
/// ```
pub struct TypedTool<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> {
    name: String,
    description: String,
    schema: serde_json::Value,
    handler: Box<
        dyn Fn(T) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send>>
            + Send
            + Sync,
    >,
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> TypedTool<T> {
    /// Create a new typed function tool with auto-generated schema.
    ///
    /// The JSON Schema is derived from `T`'s [`JsonSchema`] implementation,
    /// including any doc-comment descriptions on fields.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Self
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        let root_schema = schemars::schema_for!(T);
        let schema = serde_json::to_value(root_schema)
            .expect("schemars schema should serialize to JSON");

        Self {
            name: name.into(),
            description: description.into(),
            schema,
            handler: Box::new(move |args| Box::pin(handler(args))),
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T: DeserializeOwned + JsonSchema + Send + Sync + 'static> ToolFunction for TypedTool<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(self.schema.clone())
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let typed_args: T = serde_json::from_value(args).map_err(|e| {
            ToolError::InvalidArgs(format!("Failed to deserialize arguments: {e}"))
        })?;
        (self.handler)(typed_args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let result = dispatcher.call_function("mock_tool", json!({})).await.unwrap();
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
        use gemini_live_wire::prelude::ToolProvider;
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
            Some(json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}})),
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

    #[derive(serde::Deserialize, JsonSchema)]
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
        assert!(
            required_names.contains(&"city"),
            "city should be required"
        );
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
        let result = tool
            .call(json!({"city": 12345}))
            .await;
        assert!(result.is_err(), "should fail with wrong type");
    }

    #[tokio::test]
    async fn typed_tool_registers_in_dispatcher() {
        let tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |args: WeatherArgs| async move {
                Ok(json!({"city": args.city}))
            },
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
}
