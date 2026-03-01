//! Local function call dispatch — register handlers and execute tool calls.

use crate::protocol::{FunctionCall, FunctionDeclaration, FunctionResponse, ToolDeclaration};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type FnHandler =
    Box<dyn Fn(serde_json::Value) -> BoxFuture<Result<serde_json::Value, String>> + Send + Sync>;

/// Registry of callable functions for Gemini tool use.
///
/// Functions are registered with their JSON Schema declaration (sent to Gemini
/// in the setup message) and an async handler. When Gemini requests a tool call,
/// the registry dispatches to the appropriate handler.
pub struct FunctionRegistry {
    handlers: HashMap<String, FnHandler>,
    declarations: Vec<FunctionDeclaration>,
}

impl FunctionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            declarations: Vec::new(),
        }
    }

    /// Register a function with its schema and async handler.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gemini_live_rs::agent::FunctionRegistry;
    /// let mut registry = FunctionRegistry::new();
    /// registry.register(
    ///     "get_weather",
    ///     "Get current weather for a city",
    ///     Some(serde_json::json!({
    ///         "type": "object",
    ///         "properties": {
    ///             "city": { "type": "string" }
    ///         },
    ///         "required": ["city"]
    ///     })),
    ///     |args| async move {
    ///         let city = args["city"].as_str().ok_or("missing city")?;
    ///         Ok(serde_json::json!({ "temperature": 22, "city": city }))
    ///     },
    /// );
    /// ```
    pub fn register<F, Fut>(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
        handler: F,
    ) where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
    {
        let name = name.into();
        let description = description.into();

        self.declarations.push(FunctionDeclaration {
            name: name.clone(),
            description,
            parameters,
        });

        self.handlers
            .insert(name, Box::new(move |args| Box::pin(handler(args))));
    }

    /// Check if a function is registered.
    pub fn has(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// Number of registered functions.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Execute a single function call.
    pub async fn execute(&self, call: &FunctionCall) -> FunctionResponse {
        let result = if let Some(handler) = self.handlers.get(&call.name) {
            match handler(call.args.clone()).await {
                Ok(response) => response,
                Err(e) => serde_json::json!({ "error": e }),
            }
        } else {
            serde_json::json!({ "error": format!("Unknown function: {}", call.name) })
        };

        FunctionResponse {
            name: call.name.clone(),
            response: result,
            id: call.id.clone(),
        }
    }

    /// Execute multiple function calls in parallel.
    pub async fn execute_all(&self, calls: &[FunctionCall]) -> Vec<FunctionResponse> {
        let futures: Vec<_> = calls.iter().map(|c| self.execute(c)).collect();
        futures_util::future::join_all(futures).await
    }

    /// Get the tool declaration for the setup message.
    pub fn to_tool_declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            function_declarations: self.declarations.clone(),
        }
    }

    /// Get the function declarations.
    pub fn declarations(&self) -> &[FunctionDeclaration] {
        &self.declarations
    }
}

impl Default for FunctionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_execute() {
        let mut registry = FunctionRegistry::new();

        registry.register(
            "greet",
            "Say hello",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                }
            })),
            |args| async move {
                let name = args["name"].as_str().unwrap_or("world");
                Ok(serde_json::json!({ "greeting": format!("Hello, {name}!") }))
            },
        );

        assert!(registry.has("greet"));
        assert_eq!(registry.len(), 1);

        let call = FunctionCall {
            name: "greet".to_string(),
            args: serde_json::json!({"name": "Alice"}),
            id: Some("call-1".to_string()),
        };

        let response = registry.execute(&call).await;
        assert_eq!(response.name, "greet");
        assert_eq!(response.id, Some("call-1".to_string()));
        assert_eq!(
            response.response["greeting"],
            "Hello, Alice!"
        );
    }

    #[tokio::test]
    async fn unknown_function() {
        let registry = FunctionRegistry::new();

        let call = FunctionCall {
            name: "nonexistent".to_string(),
            args: serde_json::json!({}),
            id: None,
        };

        let response = registry.execute(&call).await;
        assert!(response.response["error"]
            .as_str()
            .unwrap()
            .contains("Unknown function"));
    }

    #[tokio::test]
    async fn execute_all_parallel() {
        let mut registry = FunctionRegistry::new();

        registry.register("add", "Add two numbers", None, |args| async move {
            let a = args["a"].as_i64().unwrap_or(0);
            let b = args["b"].as_i64().unwrap_or(0);
            Ok(serde_json::json!({ "result": a + b }))
        });

        let calls = vec![
            FunctionCall {
                name: "add".to_string(),
                args: serde_json::json!({"a": 1, "b": 2}),
                id: Some("c1".to_string()),
            },
            FunctionCall {
                name: "add".to_string(),
                args: serde_json::json!({"a": 10, "b": 20}),
                id: Some("c2".to_string()),
            },
        ];

        let responses = registry.execute_all(&calls).await;
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].response["result"], 3);
        assert_eq!(responses[1].response["result"], 30);
    }

    #[test]
    fn to_tool_declaration() {
        let mut registry = FunctionRegistry::new();
        registry.register(
            "test_fn",
            "A test function",
            Some(serde_json::json!({"type": "object"})),
            |_| async { Ok(serde_json::json!({})) },
        );

        let decl = registry.to_tool_declaration();
        assert_eq!(decl.function_declarations.len(), 1);
        assert_eq!(decl.function_declarations[0].name, "test_fn");
        assert_eq!(decl.function_declarations[0].description, "A test function");
    }

    #[tokio::test]
    async fn handler_error_wrapped() {
        let mut registry = FunctionRegistry::new();
        registry.register("fail", "Always fails", None, |_| async {
            Err("Something went wrong".to_string())
        });

        let call = FunctionCall {
            name: "fail".to_string(),
            args: serde_json::json!({}),
            id: None,
        };

        let response = registry.execute(&call).await;
        assert_eq!(
            response.response["error"],
            "Something went wrong"
        );
    }
}
