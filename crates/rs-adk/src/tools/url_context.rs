//! URL context tool — built-in server-side URL context grounding.
//!
//! Mirrors ADK-Python's `url_context_tool`. Modifies the LLM request
//! to include the URL context tool for Gemini models.

use crate::llm::LlmRequest;
use crate::utils::model_name::is_gemini2_or_above;
use rs_genai::prelude::Tool;

/// Built-in server-side URL context tool.
///
/// This tool does not perform any local execution. Instead, it modifies
/// the outgoing [`LlmRequest`] to include the URL context tool
/// configuration for Gemini 2.x+ models.
#[derive(Debug, Clone, Copy, Default)]
pub struct UrlContextTool;

impl UrlContextTool {
    /// Create a new `UrlContextTool`.
    pub fn new() -> Self {
        Self
    }

    /// Add URL context configuration to the given request.
    ///
    /// Only supported for Gemini 2.x+ models. Non-Gemini models are a no-op.
    pub fn process_llm_request(&self, request: &mut LlmRequest, model: &str) {
        if is_gemini2_or_above(model) {
            request.tools.push(Tool::url_context());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini2_adds_url_context() {
        let tool = UrlContextTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-2.5-flash");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].url_context.is_some());
    }

    #[test]
    fn gemini1_is_noop() {
        let tool = UrlContextTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-1.5-pro");

        assert!(request.tools.is_empty());
    }

    #[test]
    fn non_gemini_is_noop() {
        let tool = UrlContextTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gpt-4");

        assert!(request.tools.is_empty());
    }
}
