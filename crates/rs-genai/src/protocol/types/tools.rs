//! Tool declarations: FunctionDeclaration, Tool, ToolProvider, ToolConfig.

use serde::{Deserialize, Serialize};

use super::enums::{FunctionCallingBehavior, FunctionCallingMode};

// ---------------------------------------------------------------------------
// Tool declarations
// ---------------------------------------------------------------------------

/// Schema for a single function that the model can call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    /// Function name.
    pub name: String,
    /// Human-readable description for the model.
    pub description: String,
    /// JSON Schema describing function parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Per-function calling behavior override.
    ///
    /// When set to `NonBlocking`, the model continues generating while
    /// this function executes asynchronously. The response can then include
    /// a [`super::enums::FunctionResponseScheduling`] to control how results are delivered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<FunctionCallingBehavior>,
}

/// A tool declaration sent in the setup message.
/// Each Tool object can contain one of: function declarations, urlContext,
/// googleSearch, codeExecution, or googleSearchRetrieval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Function declarations for this tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_declarations: Option<Vec<FunctionDeclaration>>,
    /// URL context tool (web content fetching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_context: Option<UrlContext>,
    /// Google Search tool (grounded search).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search: Option<GoogleSearch>,
    /// Code execution tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<ToolCodeExecution>,
    /// Google Search retrieval tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search_retrieval: Option<GoogleSearchRetrieval>,
}

impl Tool {
    /// Create a tool with function declarations.
    pub fn functions(declarations: Vec<FunctionDeclaration>) -> Self {
        Self {
            function_declarations: Some(declarations),
            url_context: None,
            google_search: None,
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a URL context tool (enables the model to fetch and use web content).
    pub fn url_context() -> Self {
        Self {
            function_declarations: None,
            url_context: Some(UrlContext {}),
            google_search: None,
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a Google Search tool (enables grounded search).
    pub fn google_search() -> Self {
        Self {
            function_declarations: None,
            url_context: None,
            google_search: Some(GoogleSearch {}),
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a code execution tool.
    pub fn code_execution() -> Self {
        Self {
            function_declarations: None,
            url_context: None,
            google_search: None,
            code_execution: Some(ToolCodeExecution {}),
            google_search_retrieval: None,
        }
    }
}

/// Declares tools for a Gemini session setup message.
/// Implement this trait to provide tools from any source (runtime ToolDispatcher, etc.).
pub trait ToolProvider: Send + Sync + 'static {
    /// Return tool declarations for the setup message.
    fn declarations(&self) -> Vec<Tool>;
}

/// `Vec<Tool>` is a trivial `ToolProvider`.
impl ToolProvider for Vec<Tool> {
    fn declarations(&self) -> Vec<Tool> {
        self.clone()
    }
}

/// URL context tool configuration (empty — presence enables the feature).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlContext {}

/// Google Search tool configuration (empty — presence enables the feature).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleSearch {}

/// Code execution tool configuration (empty — presence enables the feature).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCodeExecution {}

/// Google Search retrieval tool configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleSearchRetrieval {}

/// Backward-compatible alias for `Tool`.
pub type ToolDeclaration = Tool;

/// Controls how and when the model uses tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    /// Function calling configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
}

impl ToolConfig {
    /// Auto mode — model decides when to call functions.
    pub fn auto() -> Self {
        Self {
            function_calling_config: Some(FunctionCallingConfig {
                mode: FunctionCallingMode::Auto,
                behavior: None,
            }),
        }
    }
}

/// Configuration for function calling behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    /// When to call functions (Auto, Any, None).
    pub mode: FunctionCallingMode,
    /// Whether tool calls block model output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<FunctionCallingBehavior>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_url_context_serialization() {
        let tool = Tool::url_context();
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"urlContext\""));
        assert!(!json.contains("\"functionDeclarations\""));
        assert!(!json.contains("\"googleSearch\""));
    }

    #[test]
    fn tool_google_search_serialization() {
        let tool = Tool::google_search();
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"googleSearch\""));
        assert!(!json.contains("\"urlContext\""));
    }

    #[test]
    fn tool_code_execution_serialization() {
        let tool = Tool::code_execution();
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"codeExecution\""));
    }

    #[test]
    fn tool_function_declarations_serialization() {
        let tool = Tool::functions(vec![FunctionDeclaration {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: None,
            behavior: None,
        }]);
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"functionDeclarations\""));
        assert!(json.contains("\"get_weather\""));
    }

    #[test]
    fn tool_url_context_is_empty_object() {
        let tool = Tool::url_context();
        let json = serde_json::to_string(&tool).unwrap();
        assert_eq!(json, r#"{"urlContext":{}}"#);
    }

    #[test]
    fn tool_backward_compat_alias() {
        // ToolDeclaration is a type alias for Tool
        let _td: ToolDeclaration = Tool::functions(vec![]);
    }

    // ── ToolProvider trait tests ──

    #[test]
    fn vec_tool_implements_tool_provider() {
        fn assert_impl<T: ToolProvider>() {}
        assert_impl::<Vec<Tool>>();
    }

    #[test]
    fn tool_provider_is_object_safe() {
        fn _assert(_: &dyn ToolProvider) {}
    }

    #[test]
    fn empty_vec_tool_provider() {
        let tools: Vec<Tool> = vec![];
        assert!(tools.declarations().is_empty());
    }

    #[test]
    fn vec_tool_provider_round_trip() {
        let tools = vec![Tool::google_search()];
        let decls = tools.declarations();
        assert_eq!(decls.len(), 1);
    }
}
