//! Request/response processors — middleware for LLM request pipelines.
//!
//! Unlike ADK-JS where processors are baked into the LLM request pipeline,
//! our processors compose as middleware — they work with any `BaseLlm`,
//! not just Gemini.

use async_trait::async_trait;

use crate::llm::{LlmRequest, LlmResponse};

/// Errors from processor operations.
#[derive(Debug, thiserror::Error)]
pub enum ProcessorError {
    /// An error during request processing.
    #[error("Processor error: {0}")]
    Processing(String),
}

/// Trait for processing LLM requests before they are sent.
#[async_trait]
pub trait RequestProcessor: Send + Sync {
    /// Processor name for logging/debugging.
    fn name(&self) -> &str;

    /// Process the request, potentially modifying it.
    async fn process_request(
        &self,
        request: LlmRequest,
    ) -> Result<LlmRequest, ProcessorError>;
}

/// Trait for processing LLM responses after they are received.
#[async_trait]
pub trait ResponseProcessor: Send + Sync {
    /// Processor name for logging/debugging.
    fn name(&self) -> &str;

    /// Process the response, potentially modifying it.
    async fn process_response(
        &self,
        response: LlmResponse,
    ) -> Result<LlmResponse, ProcessorError>;
}

/// Processor that prepends a system instruction to every request.
pub struct InstructionInserter {
    instruction: String,
}

impl InstructionInserter {
    /// Create a new instruction inserter.
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            instruction: instruction.into(),
        }
    }
}

#[async_trait]
impl RequestProcessor for InstructionInserter {
    fn name(&self) -> &str {
        "instruction_inserter"
    }

    async fn process_request(
        &self,
        mut request: LlmRequest,
    ) -> Result<LlmRequest, ProcessorError> {
        match &mut request.system_instruction {
            Some(existing) => {
                existing.push('\n');
                existing.push_str(&self.instruction);
            }
            None => {
                request.system_instruction = Some(self.instruction.clone());
            }
        }
        Ok(request)
    }
}

/// Processor that filters content parts, keeping only those that match a predicate.
pub struct ContentFilter {
    /// Keep only text parts.
    text_only: bool,
}

impl ContentFilter {
    /// Create a filter that keeps only text parts.
    pub fn text_only() -> Self {
        Self { text_only: true }
    }
}

#[async_trait]
impl RequestProcessor for ContentFilter {
    fn name(&self) -> &str {
        "content_filter"
    }

    async fn process_request(
        &self,
        mut request: LlmRequest,
    ) -> Result<LlmRequest, ProcessorError> {
        if self.text_only {
            for content in &mut request.contents {
                content.parts.retain(|p| {
                    matches!(p, rs_genai::prelude::Part::Text { .. })
                });
            }
        }
        Ok(request)
    }
}

/// An ordered chain of request processors.
#[derive(Default)]
pub struct RequestProcessorChain {
    processors: Vec<Box<dyn RequestProcessor>>,
}

impl RequestProcessorChain {
    /// Create an empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a processor to the end of the chain.
    pub fn add(&mut self, processor: impl RequestProcessor + 'static) {
        self.processors.push(Box::new(processor));
    }

    /// Process a request through all processors in order.
    pub async fn process(&self, mut request: LlmRequest) -> Result<LlmRequest, ProcessorError> {
        for processor in &self.processors {
            request = processor.process_request(request).await?;
        }
        Ok(request)
    }

    /// Number of processors in the chain.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Returns true if chain is empty.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

/// An ordered chain of response processors.
#[derive(Default)]
pub struct ResponseProcessorChain {
    processors: Vec<Box<dyn ResponseProcessor>>,
}

impl ResponseProcessorChain {
    /// Create an empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a processor to the end of the chain.
    pub fn add(&mut self, processor: impl ResponseProcessor + 'static) {
        self.processors.push(Box::new(processor));
    }

    /// Process a response through all processors in order.
    pub async fn process(
        &self,
        mut response: LlmResponse,
    ) -> Result<LlmResponse, ProcessorError> {
        for processor in &self.processors {
            response = processor.process_response(response).await?;
        }
        Ok(response)
    }

    /// Number of processors in the chain.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Returns true if chain is empty.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmRequest;

    #[test]
    fn request_processor_is_object_safe() {
        fn _assert(_: &dyn RequestProcessor) {}
    }

    #[test]
    fn response_processor_is_object_safe() {
        fn _assert(_: &dyn ResponseProcessor) {}
    }

    #[tokio::test]
    async fn instruction_inserter_sets_instruction() {
        let inserter = InstructionInserter::new("Be helpful");
        let req = LlmRequest::from_text("Hello");
        let processed = inserter.process_request(req).await.unwrap();
        assert_eq!(processed.system_instruction, Some("Be helpful".into()));
    }

    #[tokio::test]
    async fn instruction_inserter_appends_to_existing() {
        let inserter = InstructionInserter::new("And concise");
        let mut req = LlmRequest::from_text("Hello");
        req.system_instruction = Some("Be helpful".into());
        let processed = inserter.process_request(req).await.unwrap();
        assert_eq!(
            processed.system_instruction,
            Some("Be helpful\nAnd concise".into())
        );
    }

    #[tokio::test]
    async fn content_filter_text_only() {
        use rs_genai::prelude::{Content, Part, Role};

        let filter = ContentFilter::text_only();
        let req = LlmRequest {
            contents: vec![Content {
                role: Some(Role::User),
                parts: vec![
                    Part::Text {
                        text: "hello".into(),
                    },
                    Part::InlineData {
                        inline_data: rs_genai::prelude::Blob {
                            mime_type: "image/png".into(),
                            data: "base64data".into(),
                        },
                    },
                ],
            }],
            ..Default::default()
        };
        let processed = filter.process_request(req).await.unwrap();
        assert_eq!(processed.contents[0].parts.len(), 1);
        assert!(matches!(
            &processed.contents[0].parts[0],
            Part::Text { .. }
        ));
    }

    #[tokio::test]
    async fn request_processor_chain() {
        let mut chain = RequestProcessorChain::new();
        chain.add(InstructionInserter::new("Rule 1"));
        chain.add(InstructionInserter::new("Rule 2"));

        let req = LlmRequest::from_text("Hello");
        let processed = chain.process(req).await.unwrap();
        assert_eq!(
            processed.system_instruction,
            Some("Rule 1\nRule 2".into())
        );
    }

    #[test]
    fn chain_len() {
        let mut chain = RequestProcessorChain::new();
        assert!(chain.is_empty());
        chain.add(InstructionInserter::new("x"));
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
    }
}
