//! Preload memory tool — preloads memory into the LLM context.
//!
//! Mirrors ADK-Python's `preload_memory_tool`. Automatically injects
//! relevant memories into the LLM request context before generation.

use crate::llm::LlmRequest;

/// Tool that preloads relevant memories into the LLM request context.
///
/// Unlike [`crate::tools::LoadMemoryTool`] which is called by the model, this tool
/// automatically injects memories into the system instruction or context
/// before the model generates a response.
#[derive(Debug, Clone, Default)]
pub struct PreloadMemoryTool;

impl PreloadMemoryTool {
    /// Create a new preload memory tool.
    pub fn new() -> Self {
        Self
    }

    /// Inject memory context into the LLM request.
    ///
    /// Appends memory entries to the system instruction.
    pub fn process_llm_request(&self, request: &mut LlmRequest, memories: &[String]) {
        if memories.is_empty() {
            return;
        }

        let memory_context = format!(
            "\n\nRelevant memories from previous interactions:\n{}",
            memories
                .iter()
                .map(|m| format!("- {m}"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        if let Some(ref mut instruction) = request.system_instruction {
            instruction.push_str(&memory_context);
        } else {
            request.system_instruction = Some(memory_context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_memories_into_instruction() {
        let tool = PreloadMemoryTool::new();
        let mut request = LlmRequest::default();
        request.system_instruction = Some("You are helpful.".into());

        tool.process_llm_request(
            &mut request,
            &["User likes Rust".into(), "User is a developer".into()],
        );

        let instruction = request.system_instruction.unwrap();
        assert!(instruction.contains("User likes Rust"));
        assert!(instruction.contains("User is a developer"));
    }

    #[test]
    fn empty_memories_noop() {
        let tool = PreloadMemoryTool::new();
        let mut request = LlmRequest::default();
        request.system_instruction = Some("Original".into());

        tool.process_llm_request(&mut request, &[]);
        assert_eq!(request.system_instruction.unwrap(), "Original");
    }

    #[test]
    fn creates_instruction_if_none() {
        let tool = PreloadMemoryTool::new();
        let mut request = LlmRequest::default();

        tool.process_llm_request(&mut request, &["Memory 1".into()]);
        assert!(request.system_instruction.is_some());
        assert!(request.system_instruction.unwrap().contains("Memory 1"));
    }
}
