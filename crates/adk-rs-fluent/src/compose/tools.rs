//! T — Tool composition.
//!
//! Compose tools in any order with `|`.

use std::future::Future;
use std::sync::Arc;

use rs_adk::tool::{SimpleTool, ToolFunction};
use rs_genai::prelude::Tool;

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
        Fut: Future<Output = Result<serde_json::Value, rs_adk::ToolError>> + Send + 'static,
    {
        let tool = SimpleTool::new(name, description, None, f);
        ToolComposite::from_function(Arc::new(tool))
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
