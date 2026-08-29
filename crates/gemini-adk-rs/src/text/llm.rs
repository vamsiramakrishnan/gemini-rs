use std::sync::Arc;

use async_trait::async_trait;
use gemini_genai_rs::prelude::{Content, FunctionCall, FunctionResponse, Part, Role};

use super::TextAgent;
use crate::context::AgentEvent;
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
    /// Dynamic instruction source, resolved against state on every run;
    /// wins over the static `instruction` when both are set.
    instruction_provider: Option<Arc<dyn crate::instruction::InstructionProvider>>,
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
            instruction_provider: None,
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

    /// Set a dynamic instruction source — an
    /// [`InstructionProvider`](crate::instruction::InstructionProvider)
    /// (any `Fn(&State) -> String` closure, or a `TemplateInstruction`
    /// under the `templates` feature) resolved against session state at
    /// the start of every run. Wins over [`instruction`](Self::instruction).
    pub fn instruction_provider(
        mut self,
        provider: impl crate::instruction::InstructionProvider + 'static,
    ) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
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
    fn build_request(&self, contents: Vec<Content>, instruction: &Option<String>) -> LlmRequest {
        let mut req = LlmRequest::from_contents(contents);
        req.system_instruction = instruction.clone();
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

        // Resolve the instruction for this run: provider (against live
        // state) wins over the static string.
        let instruction = match &self.instruction_provider {
            Some(provider) => Some(provider.provide(state)),
            None => self.instruction.clone(),
        };

        // Lifecycle event — makes `on_event` (e.g. M::tap) observe agent start.
        let _ = self
            .middleware
            .run_on_event(&AgentEvent::AgentStarted {
                name: self.name.clone(),
            })
            .await;

        // Enforce the tightest middleware timeout (M::timeout) over the whole run.
        let result = match self.middleware.timeout() {
            Some(limit) => {
                match tokio::time::timeout(limit, self.run_inner(&mut contents, &instruction)).await
                {
                    Ok(r) => r,
                    Err(_) => {
                        let _ = self.middleware.run_on_event(&AgentEvent::Timeout).await;
                        Err(AgentError::Other(format!(
                            "agent '{}' timed out after {:?}",
                            self.name, limit
                        )))
                    }
                }
            }
            None => self.run_inner(&mut contents, &instruction).await,
        };

        if let Err(ref e) = result {
            let _ = self.middleware.run_on_error(e).await;
        } else if let Ok(ref text) = result {
            let _ = state.set("output", text);
            let _ = self
                .middleware
                .run_on_event(&AgentEvent::AgentCompleted {
                    name: self.name.clone(),
                })
                .await;
        }

        result
    }
}

impl LlmTextAgent {
    /// Inner execution loop — separated so `on_error` fires exactly once.
    async fn run_inner(
        &self,
        contents: &mut Vec<Content>,
        instruction: &Option<String>,
    ) -> Result<String, AgentError> {
        for _round in 0..MAX_TOOL_ROUNDS {
            let mut request = self.build_request(contents.clone(), instruction);

            // transform_request hook — may rewrite the request (e.g. context
            // policies trimming conversation history) before it is sent.
            self.middleware.run_transform_request(&mut request).await?;

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

            // Dispatch tools (middleware hooks inside). Media a tool
            // attached under `_media` is lifted out of the JSON and
            // delivered as inline_data parts in the same turn, so the
            // model *sees* images rather than base64 noise.
            let tool_responses = self.dispatch_tools(&calls).await;
            let mut media_parts: Vec<Part> = Vec::new();
            let mut response_parts: Vec<Part> = tool_responses
                .into_iter()
                .map(|mut fr| {
                    for attachment in crate::tool::media::extract(&mut fr.response) {
                        media_parts.push(Part::inline_data(
                            attachment.mime_type,
                            attachment.data_base64,
                        ));
                    }
                    Part::FunctionResponse {
                        function_response: fr,
                    }
                })
                .collect();
            response_parts.append(&mut media_parts);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AgentEvent;
    use crate::llm::{LlmError, LlmResponse};
    use crate::middleware::Middleware;
    use gemini_genai_rs::prelude::{Content, Part, Role};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn text_response(t: &str) -> LlmResponse {
        LlmResponse {
            content: Content {
                role: Some(Role::Model),
                parts: vec![Part::Text { text: t.into() }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        }
    }

    /// LLM that returns a function call on the first request, text on the
    /// second, capturing every request it sees.
    struct CapturingLlm {
        requests: std::sync::Mutex<Vec<LlmRequest>>,
    }
    #[async_trait]
    impl BaseLlm for CapturingLlm {
        fn model_id(&self) -> &str {
            "capturing"
        }
        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            if requests.len() == 1 {
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::FunctionCall {
                            function_call: gemini_genai_rs::prelude::FunctionCall {
                                name: "snap".into(),
                                args: serde_json::json!({}),
                                id: None,
                            },
                        }],
                    },
                    finish_reason: None,
                    usage: None,
                })
            } else {
                Ok(text_response("described"))
            }
        }
    }

    #[tokio::test]
    async fn tool_media_reaches_the_model_as_inline_data() {
        use crate::tool::{media, SimpleTool, ToolDispatcher};
        let llm = Arc::new(CapturingLlm {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(SimpleTool::new(
            "snap",
            "Take a snapshot",
            None,
            |_args| async move {
                let mut result = serde_json::json!({"took": true});
                media::attach(&mut result, "image/png", b"fakepng");
                Ok(result)
            },
        ));
        let agent = LlmTextAgent::new("vision", llm.clone()).tools(Arc::new(dispatcher));
        let state = State::new();
        let _ = state.set("input", "what do you see?");
        assert_eq!(agent.run(&state).await.unwrap(), "described");

        let requests = llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        // The second request's tool-response turn carries the image part and
        // the function response JSON no longer contains the base64 blob.
        let turn = requests[1].contents.last().unwrap();
        let has_inline = turn
            .parts
            .iter()
            .any(|p| matches!(p, Part::InlineData { .. }));
        assert!(has_inline, "expected an inline_data part, got {turn:?}");
        let fr_clean = turn.parts.iter().all(|p| match p {
            Part::FunctionResponse { function_response } => {
                function_response.response.get(media::MEDIA_KEY).is_none()
            }
            _ => true,
        });
        assert!(
            fr_clean,
            "media key should be stripped from the response JSON"
        );
    }

    #[tokio::test]
    async fn instruction_provider_resolves_against_state_each_run() {
        let llm = Arc::new(CapturingLlm {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let agent = LlmTextAgent::new("persona", llm.clone()).instruction_provider(|s: &State| {
            format!(
                "You are {}.",
                s.get::<String>("persona").unwrap_or_default()
            )
        });
        let state = State::new();
        let _ = state.set("input", "hi");
        let _ = state.set("persona", "a pirate");
        // CapturingLlm returns a function call first; with no dispatcher the
        // loop sends an empty tool-response turn and the second reply ends
        // the run — both requests must carry the resolved instruction.
        let _ = agent.run(&state).await.unwrap();
        let requests = llm.requests.lock().unwrap();
        assert!(requests
            .iter()
            .all(|r| r.system_instruction.as_deref() == Some("You are a pirate.")));
    }

    struct SlowLlm;
    #[async_trait]
    impl BaseLlm for SlowLlm {
        fn model_id(&self) -> &str {
            "slow"
        }
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(text_response("done"))
        }
    }

    struct FastLlm;
    #[async_trait]
    impl BaseLlm for FastLlm {
        fn model_id(&self) -> &str {
            "fast"
        }
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(text_response("hi"))
        }
    }

    struct ShortTimeout;
    #[async_trait]
    impl Middleware for ShortTimeout {
        fn name(&self) -> &str {
            "short-timeout"
        }
        fn timeout(&self) -> Option<Duration> {
            Some(Duration::from_millis(20))
        }
    }

    struct EventFlag(Arc<AtomicBool>);
    #[async_trait]
    impl Middleware for EventFlag {
        fn name(&self) -> &str {
            "event-flag"
        }
        async fn on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
            if matches!(event, AgentEvent::AgentStarted { .. }) {
                self.0.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn timeout_aborts_slow_run() {
        let agent =
            LlmTextAgent::new("slowpoke", Arc::new(SlowLlm)).add_middleware(Arc::new(ShortTimeout));
        let state = State::new();
        let _ = state.set("input", "hi");
        let err = agent.run(&state).await.expect_err("expected timeout");
        assert!(format!("{err:?}").contains("timed out"), "got: {err:?}");
    }

    #[tokio::test]
    async fn on_event_fires_for_agent_lifecycle() {
        let flag = Arc::new(AtomicBool::new(false));
        let agent = LlmTextAgent::new("a", Arc::new(FastLlm))
            .add_middleware(Arc::new(EventFlag(flag.clone())));
        let state = State::new();
        let _ = state.set("input", "hi");
        let _ = agent.run(&state).await;
        assert!(
            flag.load(Ordering::SeqCst),
            "on_event(AgentStarted) should fire"
        );
    }
}
