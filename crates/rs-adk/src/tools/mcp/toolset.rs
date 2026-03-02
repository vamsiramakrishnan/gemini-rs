//! MCP toolset — discovers tools from an MCP server.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tool::ToolFunction;
use crate::toolset::Toolset;

use super::session_manager::McpSessionManager;

/// A toolset that discovers tools from an MCP server.
pub struct McpToolset {
    session_manager: Arc<McpSessionManager>,
    filter: Option<Vec<String>>,
}

impl McpToolset {
    pub fn new(session_manager: Arc<McpSessionManager>) -> Self {
        Self {
            session_manager,
            filter: None,
        }
    }

    /// Only expose tools whose names match the filter.
    pub fn with_filter(mut self, names: Vec<String>) -> Self {
        self.filter = Some(names);
        self
    }

    /// Returns the current filter, if any.
    pub fn filter(&self) -> Option<&[String]> {
        self.filter.as_deref()
    }

    /// Returns a reference to the underlying session manager.
    pub fn session_manager(&self) -> &Arc<McpSessionManager> {
        &self.session_manager
    }
}

#[async_trait]
impl Toolset for McpToolset {
    fn get_tools(&self) -> Vec<Arc<dyn ToolFunction>> {
        // Note: This is synchronous in the trait, but tool discovery is async.
        // In practice, tools should be pre-loaded. For now, return empty.
        // A more complete implementation would cache tools after async initialization.
        vec![]
    }

    async fn close(&self) {
        // Close MCP session if connected
    }
}
