//! Built-in server-side Google Search tool.
//!
//! Does not execute locally -- modifies the [`LlmRequest`] to include the
//! appropriate search configuration depending on the model version:
//!
//! * **Gemini 2.x+**: adds [`Tool::google_search()`]
//! * **Gemini 1.x**: adds a `Tool` with `google_search_retrieval`
//! * **Non-Gemini models**: no-op (the request is left unchanged)

use crate::llm::LlmRequest;
use crate::utils::model_name::{is_gemini1_model, is_gemini2_or_above};
use rs_genai::prelude::{GoogleSearchRetrieval, Tool};

/// Built-in server-side Google Search tool.
///
/// This tool does not perform any local execution. Instead, it modifies
/// the outgoing [`LlmRequest`] to include the Google Search tool
/// configuration appropriate for the target model.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleSearchTool;

impl GoogleSearchTool {
    /// Create a new `GoogleSearchTool`.
    pub fn new() -> Self {
        Self
    }

    /// Add Google Search configuration to the given request.
    ///
    /// Inspects the `model` string to determine which variant to use:
    ///
    /// * Gemini 2.x or above: adds `Tool::google_search()` (the `googleSearch`
    ///   field).
    /// * Gemini 1.x: adds a `Tool` with `google_search_retrieval`.
    /// * Non-Gemini models: the request is left unchanged (no-op).
    pub fn process_llm_request(&self, request: &mut LlmRequest, model: &str) {
        if is_gemini2_or_above(model) {
            request.tools.push(Tool::google_search());
        } else if is_gemini1_model(model) {
            request.tools.push(Tool {
                function_declarations: None,
                url_context: None,
                google_search: None,
                code_execution: None,
                google_search_retrieval: Some(GoogleSearchRetrieval {}),
            });
        }
        // Non-Gemini models: no-op
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini2_adds_google_search() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-2.5-flash");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search.is_some());
        assert!(request.tools[0].google_search_retrieval.is_none());
    }

    #[test]
    fn gemini2_0_adds_google_search() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-2.0-flash");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search.is_some());
    }

    #[test]
    fn gemini1_adds_google_search_retrieval() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-1.5-pro");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search_retrieval.is_some());
        assert!(request.tools[0].google_search.is_none());
    }

    #[test]
    fn gemini1_0_adds_google_search_retrieval() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-1.0-pro");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search_retrieval.is_some());
    }

    #[test]
    fn non_gemini_model_is_noop() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "claude-3-opus");

        assert!(request.tools.is_empty());
    }

    #[test]
    fn unknown_model_is_noop() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gpt-4");

        assert!(request.tools.is_empty());
    }

    #[test]
    fn empty_model_string_is_noop() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "");

        assert!(request.tools.is_empty());
    }

    #[test]
    fn full_resource_path_gemini2() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(
            &mut request,
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.5-flash",
        );

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search.is_some());
    }

    #[test]
    fn full_resource_path_gemini1() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(
            &mut request,
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-1.5-pro-002",
        );

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search_retrieval.is_some());
    }

    #[test]
    fn preserves_existing_tools() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        // Add a pre-existing tool
        request.tools.push(Tool::code_execution());

        tool.process_llm_request(&mut request, "gemini-2.5-flash");

        assert_eq!(request.tools.len(), 2);
        assert!(request.tools[0].code_execution.is_some());
        assert!(request.tools[1].google_search.is_some());
    }

    #[test]
    fn gemini3_future_version() {
        let tool = GoogleSearchTool::new();
        let mut request = LlmRequest::default();
        tool.process_llm_request(&mut request, "gemini-3.0-ultra");

        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].google_search.is_some());
    }
}
