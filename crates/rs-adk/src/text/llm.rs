use std::sync::Arc;

use async_trait::async_trait;
use rs_genai::prelude::{Content, FunctionCall, FunctionResponse, Part, Role};

use super::TextAgent;
use crate::error::AgentError;
use crate::llm::{BaseLlm, LlmRequest};
use crate::state::State;
use crate::tool::ToolDispatcher;

/// Maximum number of tool-dispatch round-trips before giving up.
const MAX_TOOL_ROUNDS: usize = 10;

/// Core text agent — calls `BaseLlm::generate()`, dispatches tools, loops
/// until the model produces a final text response.
pub struct LlmTextAgent {
    name: String,
    llm: Arc<dyn BaseLlm>,
    instruction: Option<String>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
}

impl LlmTextAgent {
    /// Create a new LLM text agent.
    pub fn new(name: impl Into<String>, llm: Arc<dyn BaseLlm>) -> Self {
        Self {
            name: name.into(),
            llm,
            instruction: None,
            dispatcher: None,
            temperature: None,
            max_output_tokens: None,
        }
    }

    /// Set the system instruction.
    pub fn instruction(mut self, inst: impl Into<String>) -> Self {
        self.instruction = Some(inst.into());
        self
    }

    /// Set the tool dispatcher.
    pub fn tools(mut self, dispatcher: Arc<ToolDispatcher>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Set temperature.
    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Set max output tokens.
    pub fn max_output_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = Some(n);
        self
    }

    /// Build an LlmRequest, taking ownership of contents to avoid cloning.
    fn build_request(&self, contents: Vec<Content>) -> LlmRequest {
        let mut req = LlmRequest::from_contents(contents);
        req.system_instruction = self.instruction.clone();
        req.temperature = self.temperature;
        req.max_output_tokens = self.max_output_tokens;

        if let Some(dispatcher) = &self.dispatcher {
            req.tools = dispatcher.to_tool_declarations();
        }

        req
    }

    /// Dispatch function calls and return function responses.
    async fn dispatch_tools(&self, calls: &[FunctionCall]) -> Vec<FunctionResponse> {
        let dispatcher = match &self.dispatcher {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut responses = Vec::with_capacity(calls.len());
        for call in calls {
            let result = dispatcher
                .call_function(&call.name, call.args.clone())
                .await;
            responses.push(ToolDispatcher::build_response(call, result));
        }
        responses
    }
}

#[async_trait]
impl TextAgent for LlmTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        // Build initial contents from state "input" key, or empty user message.
        let input = state.get::<String>("input").unwrap_or_default();

        let mut contents = vec![Content::user(&input)];

        for _round in 0..MAX_TOOL_ROUNDS {
            let request = self.build_request(contents.clone());
            let response = self
                .llm
                .generate(request)
                .await
                .map_err(|e| AgentError::Other(format!("LLM error: {e}")))?;

            let calls: Vec<FunctionCall> = response.function_calls().into_iter().cloned().collect();

            if calls.is_empty() {
                // No tool calls — we have a final text response.
                let text = response.text();
                state.set("output", &text);
                return Ok(text);
            }

            // Move model response into conversation (no clone needed).
            contents.push(response.content);

            // Dispatch tools and append responses.
            let tool_responses = self.dispatch_tools(&calls).await;
            let response_parts: Vec<Part> = tool_responses
                .into_iter()
                .map(|fr| Part::FunctionResponse {
                    function_response: fr,
                })
                .collect();

            contents.push(Content {
                role: Some(Role::User),
                parts: response_parts,
            });
        }

        Err(AgentError::Other(format!(
            "Agent '{}' exceeded max tool rounds ({})",
            self.name, MAX_TOOL_ROUNDS
        )))
    }
}
