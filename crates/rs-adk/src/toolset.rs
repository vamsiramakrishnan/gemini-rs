//! Toolset trait — collections of tools that can be enumerated and filtered.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tool::ToolFunction;

/// A collection of tools that can be enumerated.
#[async_trait]
pub trait Toolset: Send + Sync {
    /// Get all tools in this toolset.
    fn get_tools(&self) -> Vec<Arc<dyn ToolFunction>>;

    /// Clean up resources when the toolset is no longer needed.
    async fn close(&self) {}
}

/// A simple toolset backed by a fixed list of tools.
pub struct StaticToolset {
    tools: Vec<Arc<dyn ToolFunction>>,
}

impl StaticToolset {
    /// Create a new static toolset from a list of tools.
    pub fn new(tools: Vec<Arc<dyn ToolFunction>>) -> Self {
        Self { tools }
    }

    /// Create a new toolset containing only tools whose names are in `names`.
    pub fn filter_by_name(&self, names: &[&str]) -> Self {
        let filtered = self
            .tools
            .iter()
            .filter(|t| names.contains(&t.name()))
            .cloned()
            .collect();
        Self { tools: filtered }
    }
}

#[async_trait]
impl Toolset for StaticToolset {
    fn get_tools(&self) -> Vec<Arc<dyn ToolFunction>> {
        self.tools.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;

    struct DummyTool {
        name: &'static str,
    }

    #[async_trait]
    impl ToolFunction for DummyTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> Option<serde_json::Value> {
            None
        }
        async fn call(&self, _args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[test]
    fn static_toolset_get_tools() {
        let toolset = StaticToolset::new(vec![
            Arc::new(DummyTool { name: "a" }),
            Arc::new(DummyTool { name: "b" }),
        ]);
        let tools = toolset.get_tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name(), "a");
        assert_eq!(tools[1].name(), "b");
    }

    #[test]
    fn filter_by_name() {
        let toolset = StaticToolset::new(vec![
            Arc::new(DummyTool { name: "alpha" }),
            Arc::new(DummyTool { name: "beta" }),
            Arc::new(DummyTool { name: "gamma" }),
        ]);

        let filtered = toolset.filter_by_name(&["alpha", "gamma"]);
        let tools = filtered.get_tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name(), "alpha");
        assert_eq!(tools[1].name(), "gamma");
    }

    #[test]
    fn empty_toolset() {
        let toolset = StaticToolset::new(vec![]);
        assert!(toolset.get_tools().is_empty());
    }

    #[test]
    fn filter_by_nonexistent_name() {
        let toolset = StaticToolset::new(vec![Arc::new(DummyTool { name: "a" })]);
        let filtered = toolset.filter_by_name(&["nonexistent"]);
        assert!(filtered.get_tools().is_empty());
    }

    #[test]
    fn toolset_is_object_safe() {
        fn _assert(_: &dyn Toolset) {}
    }
}
