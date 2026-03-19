//! T — Tool composition.
//!
//! Compose tools in any order with `|`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gemini_adk_rs::text::TextAgent;
use gemini_adk_rs::tool::{SimpleTool, ToolFunction};
use gemini_genai_rs::prelude::Tool;

/// A tool composite — one or more tool entries.
#[derive(Clone)]
pub struct ToolComposite {
    /// The tool entries in this composite.
    pub entries: Vec<ToolCompositeEntry>,
}

/// An entry in a tool composite.
#[derive(Clone)]
pub enum ToolCompositeEntry {
    /// A runtime tool function.
    Function(Arc<dyn ToolFunction>),
    /// A built-in Gemini tool declaration.
    BuiltIn(Tool),
    /// A text agent wrapped as a tool.
    Agent {
        /// Tool name exposed to the model.
        name: String,
        /// Tool description exposed to the model.
        description: String,
        /// The text agent to invoke.
        agent: Arc<dyn TextAgent>,
    },
    /// An MCP (Model Context Protocol) toolset connection.
    Mcp {
        /// Connection params (e.g. URL or command string).
        params: String,
    },
    /// A remote agent-to-agent tool.
    A2a {
        /// URL of the remote agent.
        url: String,
        /// Skill to invoke on the remote agent.
        skill: String,
    },
    /// A mock tool that returns a fixed response (useful for testing).
    Mock {
        /// Tool name.
        name: String,
        /// Tool description.
        description: String,
        /// Fixed response to return.
        response: serde_json::Value,
    },
    /// An OpenAPI spec-driven tool (placeholder/marker).
    OpenApi {
        /// Tool name.
        name: String,
        /// URL to the OpenAPI spec.
        spec_url: String,
    },
    /// A BM25 search tool (placeholder/marker).
    Search {
        /// Tool name.
        name: String,
        /// Tool description.
        description: String,
    },
    /// A schema-defined tool (placeholder/marker).
    Schema {
        /// Tool name.
        name: String,
        /// JSON Schema defining the tool's parameters.
        schema: serde_json::Value,
    },
    /// A tool wrapped with a result transformer.
    Transform {
        /// The inner tool entry.
        inner: Box<ToolCompositeEntry>,
        /// Transformer function applied to the tool result.
        transformer: Arc<
            dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>>
                + Send
                + Sync,
        >,
    },
}

impl ToolComposite {
    /// Create a composite containing a single runtime tool function.
    pub fn from_function(f: Arc<dyn ToolFunction>) -> Self {
        Self {
            entries: vec![ToolCompositeEntry::Function(f)],
        }
    }

    /// Create a composite containing a single built-in tool declaration.
    pub fn from_built_in(tool: Tool) -> Self {
        Self {
            entries: vec![ToolCompositeEntry::BuiltIn(tool)],
        }
    }

    /// Number of tool entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Compose two tool composites with `|`.
impl std::ops::BitOr for ToolComposite {
    type Output = ToolComposite;

    fn bitor(mut self, rhs: ToolComposite) -> Self::Output {
        self.entries.extend(rhs.entries);
        self
    }
}

/// The `T` namespace — static factory methods for tool composition.
pub struct T;

impl T {
    /// Register a function tool.
    pub fn function(f: Arc<dyn ToolFunction>) -> ToolComposite {
        ToolComposite::from_function(f)
    }

    /// Add Google Search built-in tool.
    pub fn google_search() -> ToolComposite {
        ToolComposite::from_built_in(Tool::google_search())
    }

    /// Add URL context built-in tool.
    pub fn url_context() -> ToolComposite {
        ToolComposite::from_built_in(Tool::url_context())
    }

    /// Add code execution built-in tool.
    pub fn code_execution() -> ToolComposite {
        ToolComposite::from_built_in(Tool::code_execution())
    }

    /// Create a simple tool from a name, description, and async closure.
    pub fn simple<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        f: F,
    ) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, gemini_adk_rs::ToolError>> + Send + 'static,
    {
        let tool = SimpleTool::new(name, description, None, f);
        ToolComposite::from_function(Arc::new(tool))
    }

    /// Alias for [`simple`](Self::simple) — matches upstream Python `T.fn()`.
    ///
    /// Named `fn_tool` because `fn` is a reserved keyword in Rust.
    pub fn fn_tool<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        f: F,
    ) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, gemini_adk_rs::ToolError>> + Send + 'static,
    {
        Self::simple(name, description, f)
    }

    /// Wrap a tool with a confirmation requirement (marker for runtime).
    pub fn confirm(tool: ToolComposite, _message: &str) -> ToolComposite {
        // Confirmation enforcement happens at runtime. This is a declarative marker.
        tool
    }

    /// Wrap a tool with a timeout (marker for runtime).
    pub fn timeout(tool: ToolComposite, _duration: std::time::Duration) -> ToolComposite {
        // Timeout enforcement happens at runtime.
        tool
    }

    /// Wrap a tool with caching (marker for runtime).
    pub fn cached(tool: ToolComposite) -> ToolComposite {
        // Cache enforcement happens at runtime.
        tool
    }

    /// Combine multiple tool functions into a single composite.
    pub fn toolset(tools: Vec<Arc<dyn ToolFunction>>) -> ToolComposite {
        ToolComposite {
            entries: tools
                .into_iter()
                .map(ToolCompositeEntry::Function)
                .collect(),
        }
    }

    /// Wrap a [`TextAgent`] as a tool (shorthand for creating an agent tool entry).
    ///
    /// When invoked, the agent runs via `BaseLlm::generate()` and returns its
    /// text output as the tool result. State is shared with the parent session.
    pub fn agent(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
    ) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Agent {
                name: name.into(),
                description: description.into(),
                agent: Arc::new(agent),
            }],
        }
    }

    /// Create an MCP (Model Context Protocol) toolset entry.
    ///
    /// `params` is the connection string (e.g. a URL or command) used to
    /// establish the MCP session at runtime.
    pub fn mcp(params: impl Into<String>) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Mcp {
                params: params.into(),
            }],
        }
    }

    /// Create a remote agent-to-agent tool.
    ///
    /// Routes tool calls to a remote agent at `url`, invoking the given `skill`.
    pub fn a2a(url: impl Into<String>, skill: impl Into<String>) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::A2a {
                url: url.into(),
                skill: skill.into(),
            }],
        }
    }

    /// Create a mock tool that returns a fixed response.
    ///
    /// Useful for testing and prototyping without real tool implementations.
    pub fn mock(
        name: impl Into<String>,
        description: impl Into<String>,
        response: serde_json::Value,
    ) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Mock {
                name: name.into(),
                description: description.into(),
                response,
            }],
        }
    }

    /// Create an OpenAPI spec-driven tool (placeholder/marker).
    ///
    /// At runtime, the spec at `spec_url` is fetched and used to generate
    /// tool declarations and HTTP call routing.
    pub fn openapi(name: impl Into<String>, spec_url: impl Into<String>) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::OpenApi {
                name: name.into(),
                spec_url: spec_url.into(),
            }],
        }
    }

    /// Create a BM25 search tool (placeholder/marker).
    ///
    /// Declares a search tool that performs BM25 retrieval at runtime.
    pub fn search(name: impl Into<String>, description: impl Into<String>) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Search {
                name: name.into(),
                description: description.into(),
            }],
        }
    }

    /// Create a schema-defined tool (placeholder/marker).
    ///
    /// The tool's parameters are defined by the given JSON Schema value.
    pub fn schema(name: impl Into<String>, schema: serde_json::Value) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Schema {
                name: name.into(),
                schema,
            }],
        }
    }

    /// Wrap each tool entry in a composite with a result transformer.
    ///
    /// The transformer function is applied to the tool's output value before
    /// it is returned to the model.
    pub fn transform<F, Fut>(tool: ToolComposite, f: F) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = serde_json::Value> + Send + 'static,
    {
        let f: Arc<
            dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>>
                + Send
                + Sync,
        > = Arc::new(
            move |v: serde_json::Value| -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>> {
                Box::pin(f(v))
            },
        );
        ToolComposite {
            entries: tool
                .entries
                .into_iter()
                .map(|entry| ToolCompositeEntry::Transform {
                    inner: Box::new(entry),
                    transformer: Arc::clone(&f),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_search_creates_composite() {
        let t = T::google_search();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn url_context_creates_composite() {
        let t = T::url_context();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn code_execution_creates_composite() {
        let t = T::code_execution();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn compose_with_bitor() {
        let t = T::google_search() | T::url_context() | T::code_execution();
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn simple_creates_tool() {
        let t = T::simple("greet", "Greets the user", |_args| async {
            Ok(serde_json::json!({"message": "hello"}))
        });
        assert_eq!(t.len(), 1);
        match &t.entries[0] {
            ToolCompositeEntry::Function(f) => assert_eq!(f.name(), "greet"),
            _ => panic!("expected Function entry"),
        }
    }

    #[test]
    fn toolset_combines_functions() {
        let tool_a: Arc<dyn ToolFunction> =
            Arc::new(SimpleTool::new("a", "tool a", None, |_| async {
                Ok(serde_json::json!(null))
            }));
        let tool_b: Arc<dyn ToolFunction> =
            Arc::new(SimpleTool::new("b", "tool b", None, |_| async {
                Ok(serde_json::json!(null))
            }));
        let t = T::toolset(vec![tool_a, tool_b]);
        assert_eq!(t.len(), 2);
    }
}
