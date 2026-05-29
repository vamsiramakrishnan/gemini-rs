use std::sync::Arc;

use async_trait::async_trait;
use gemini_genai_rs::prelude::{Content, FunctionCall, FunctionResponse, Part, Role};

use super::TextAgent;
use crate::error::AgentError;
use crate::llm::{BaseLlm, LlmRequest};
use crate::middleware::MiddlewareChain;
use crate::state::State;
use crate::tool::ToolDispatcher;

/// Maximum number of tool-dispatch round-trips before giving up.
const MAX_TOOL_ROUNDS: usize = 10;

/// Core text agent — calls `BaseLlm::generate()`, dispatches tools, loops
/// until the model produces a final text response.
///
/// Middleware hooks fire at each lifecycle point:
///
/// - `before_model` / `after_model` — wraps each `BaseLlm::generate()` call;
///   `before_model` may return a cached response to skip the LLM entirely.
/// - `before_tool` / `after_tool` / `on_tool_error` — wraps each tool dispatch.
/// - `on_error` — called when `run()` is about to return an error.
///
/// Note: `before_agent`/`after_agent` are Live-session hooks that require an
/// `InvocationContext` (a Live WebSocket concept) and are therefore not invoked
/// by `LlmTextAgent`.  Use `before_model` or wrap in a custom `TextAgent` if you
/// need entry/exit hooks for the text path.
pub struct LlmTextAgent {
    name: String,
    llm: Arc<dyn BaseLlm>,
    instruction: Option<String>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    middleware: MiddlewareChain,
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
            middleware: MiddlewareChain::new(),
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

    /// Append a middleware layer to the chain.
    ///
    /// Layers are run in insertion order for `before_*` / `on_error` hooks
    /// and in reverse insertion order for `after_*` hooks (outermost last).
    pub fn add_middleware(mut self, mw: Arc<dyn crate::middleware::Middleware>) -> Self {
        self.middleware.add(mw);
        self
    }

    /// Replace the entire middleware chain (advanced — prefer `add_middleware`).
    pub fn with_middleware_chain(mut self, chain: MiddlewareChain) -> Self {
        self.middleware = chain;
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

    /// Dispatch function calls and return function responses, firing middleware hooks.
    async fn dispatch_tools(&self, calls: &[FunctionCall]) -> Vec<FunctionResponse> {
        let dispatcher = match &self.dispatcher {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut responses = Vec::with_capacity(calls.len());
        for call in calls {
            // before_tool hook
            if let Err(e) = self.middleware.run_before_tool(call).await {
                // Hook error — record it and return an error response.
                let _ = self
                    .middleware
                    .run_on_tool_error(
                        call,
                        &crate::error::ToolError::ExecutionFailed(e.to_string()),
                    )
                    .await;
                responses.push(ToolDispatcher::build_response(
                    call,
                    Err(crate::error::ToolError::ExecutionFailed(e.to_string())),
                ));
                continue;
            }

            let result = dispatcher
                .call_function(&call.name, call.args.clone())
                .await;

            match &result {
                Ok(value) => {
                    let _ = self.middleware.run_after_tool(call, value).await;
                }
                Err(e) => {
                    let _ = self.middleware.run_on_tool_error(call, e).await;
                }
            }

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

        let result = self.run_inner(&mut contents).await;

        if let Err(ref e) = result {
            let _ = self.middleware.run_on_error(e).await;
        } else if let Ok(ref text) = result {
            state.set("output", text);
        }

        result
    }
}

impl LlmTextAgent {
    /// Inner execution loop — separated so `on_error` fires exactly once.
    async fn run_inner(&self, contents: &mut Vec<Content>) -> Result<String, AgentError> {
        for _round in 0..MAX_TOOL_ROUNDS {
            let request = self.build_request(contents.clone());

            // before_model hook — may short-circuit with a cached response.
            let response = match self.middleware.run_before_model(&request).await? {
                Some(cached) => cached,
                None => {
                    let llm_response = self
                        .llm
                        .generate(request.clone())
                        .await
                        .map_err(|e| AgentError::Other(format!("LLM error: {e}")))?;

                    // after_model hook — may replace the response.
                    match self
                        .middleware
                        .run_after_model(&request, &llm_response)
                        .await?
                    {
                        Some(replaced) => replaced,
                        None => llm_response,
                    }
                }
            };

            let calls: Vec<FunctionCall> = response.function_calls().into_iter().cloned().collect();

            if calls.is_empty() {
                // No tool calls — we have a final text response.
                return Ok(response.text());
            }

            // Move model response into conversation (no clone needed).
            contents.push(response.content);

            // Dispatch tools (middleware hooks inside).
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
