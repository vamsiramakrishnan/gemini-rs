//! Example tool — adds few-shot examples to LLM requests.
//!
//! Mirrors ADK-Python's `ExampleTool`. Enriches LLM requests by
//! injecting example conversations into the context.

use crate::llm::LlmRequest;

/// A single few-shot example for an agent.
#[derive(Debug, Clone)]
pub struct Example {
    /// The user input.
    pub input: String,
    /// The expected model output.
    pub output: String,
}

impl Example {
    /// Create a new example.
    pub fn new(input: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
        }
    }
}

/// Trait for providing examples dynamically.
pub trait ExampleProvider: Send + Sync {
    /// Get the examples to inject.
    fn examples(&self) -> Vec<Example>;
}

/// Tool that adds few-shot examples to the LLM request.
///
/// This is not a callable tool — it modifies the LLM request to include
/// example conversations that guide the model's behavior.
#[derive(Debug, Clone)]
pub struct ExampleTool {
    examples: Vec<Example>,
}

impl ExampleTool {
    /// Create a new example tool with static examples.
    pub fn new(examples: Vec<Example>) -> Self {
        Self { examples }
    }

    /// Create from an example provider.
    pub fn from_provider(provider: &dyn ExampleProvider) -> Self {
        Self {
            examples: provider.examples(),
        }
    }

    /// Add example instructions to the LLM request.
    ///
    /// Appends the examples to the system instruction as formatted
    /// input/output pairs.
    pub fn process_llm_request(&self, request: &mut LlmRequest) {
        if self.examples.is_empty() {
            return;
        }

        let mut example_text = String::from("\n\nHere are some examples of expected behavior:\n");
        for (i, example) in self.examples.iter().enumerate() {
            example_text.push_str(&format!(
                "\nExample {}:\nUser: {}\nAssistant: {}\n",
                i + 1,
                example.input,
                example.output
            ));
        }

        if let Some(ref mut instruction) = request.system_instruction {
            instruction.push_str(&example_text);
        } else {
            request.system_instruction = Some(example_text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_examples() {
        let tool = ExampleTool::new(vec![
            Example::new("What is 2+2?", "4"),
            Example::new("What color is the sky?", "Blue"),
        ]);

        let mut request = LlmRequest::default();
        request.system_instruction = Some("You are helpful.".into());

        tool.process_llm_request(&mut request);
        let instruction = request.system_instruction.unwrap();
        assert!(instruction.contains("Example 1:"));
        assert!(instruction.contains("What is 2+2?"));
        assert!(instruction.contains("Example 2:"));
        assert!(instruction.contains("Blue"));
    }

    #[test]
    fn empty_examples_noop() {
        let tool = ExampleTool::new(vec![]);
        let mut request = LlmRequest::default();
        request.system_instruction = Some("Original".into());

        tool.process_llm_request(&mut request);
        assert_eq!(request.system_instruction.unwrap(), "Original");
    }

    #[test]
    fn creates_instruction_if_none() {
        let tool = ExampleTool::new(vec![Example::new("Hi", "Hello!")]);
        let mut request = LlmRequest::default();

        tool.process_llm_request(&mut request);
        assert!(request.system_instruction.is_some());
    }

    struct StaticProvider;
    impl ExampleProvider for StaticProvider {
        fn examples(&self) -> Vec<Example> {
            vec![Example::new("test", "response")]
        }
    }

    #[test]
    fn from_provider() {
        let tool = ExampleTool::from_provider(&StaticProvider);
        assert_eq!(tool.examples.len(), 1);
    }
}
