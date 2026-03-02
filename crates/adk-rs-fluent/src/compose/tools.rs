//! T — Tool composition.
//!
//! Compose tools in any order with `|`.

use std::sync::Arc;

use rs_genai::prelude::Tool;
use rs_adk::tool::ToolFunction;

/// A tool composite — one or more tool entries.
#[derive(Clone)]
pub struct ToolComposite {
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
    pub fn from_function(f: Arc<dyn ToolFunction>) -> Self {
        Self {
            entries: vec![ToolCompositeEntry::Function(f)],
        }
    }

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
}
