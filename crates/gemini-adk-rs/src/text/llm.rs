use std::sync::Arc;

use async_trait::async_trait;
use gemini_genai_rs::prelude::{Content, FunctionCall, FunctionResponse, Part, Role};

use futures_util::stream::BoxStream;
use futures_util::{FutureExt, StreamExt};

use super::{RunEvent, RunRequest, RunResult, TextAgent, ToolCallRecord};
use crate::context::AgentEvent;
use crate::error::AgentError;
use crate::llm::{BaseLlm, LlmRequest, LlmResponse};
use crate::middleware::MiddlewareChain;
use crate::state::State;
use crate::tool::ToolDispatcher;

/// Where a streamed run sends its events.
type EventSender = tokio::sync::mpsc::UnboundedSender<Result<RunEvent, AgentError>>;

/// Maximum number of tool-dispatch round-trips before giving up.
const MAX_TOOL_ROUNDS: usize = 10;

/// A dynamic model source: state in, the model to use for this run out.
type LlmProviderFn = Arc<dyn Fn(&State) -> Arc<dyn BaseLlm> + Send + Sync>;

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
    /// Dynamic model source, resolved against state on every run;
    /// wins over the constructor's model when set. Risk-based escalation,
    /// cost routing, per-tenant model selection without rebuilding the agent.
    llm_provider: Option<LlmProviderFn>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    /// Every per-request setting (model, sampling, built-in tools, response
    /// schema); each call starts from a copy with the conversation filled in.
    template: LlmRequest,
    /// A state key that receives the final text, besides `"output"`.
    output_key: Option<String>,
    middleware: MiddlewareChain,
}

impl LlmTextAgent {
    /// Create a new LLM text agent.
    ///
    /// `llm` is any [`BaseLlm`]: a `GeminiLlm`, a shared `Arc<dyn BaseLlm>`,
    /// or a [`MockLlm`](crate::llm::MockLlm) in tests.
    pub fn new(name: impl Into<String>, llm: impl BaseLlm + 'static) -> Self {
        Self {
            name: name.into(),
            llm: Arc::new(llm),
            instruction: None,
            instruction_provider: None,
            llm_provider: None,
            dispatcher: None,
            template: LlmRequest::default(),
            output_key: None,
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

    /// Set a dynamic model source, resolved against session state at the
    /// start of every run — risk-based escalation to a stronger model, cost
    /// routing to a cheaper one, per-tenant model selection — without
    /// rebuilding the agent. Wins over the constructor's model when set.
    pub fn llm_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn(&State) -> Arc<dyn BaseLlm> + Send + Sync + 'static,
    {
        self.llm_provider = Some(Arc::new(provider));
        self
    }

    /// Set the tool dispatcher.
    pub fn tools(mut self, dispatcher: Arc<ToolDispatcher>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Call `model` instead of the provider's default model, e.g.
    /// `"gemini-2.5-pro"`.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.template.model = Some(model.into());
        self
    }

    /// Set temperature.
    pub fn temperature(mut self, t: f32) -> Self {
        self.template.temperature = Some(t);
        self
    }

    /// Set max output tokens.
    pub fn max_output_tokens(mut self, n: u32) -> Self {
        self.template.max_output_tokens = Some(n);
        self
    }

    /// Set the nucleus sampling threshold.
    pub fn top_p(mut self, p: f32) -> Self {
        self.template.top_p = Some(p);
        self
    }

    /// Sample from the `k` most likely tokens.
    pub fn top_k(mut self, k: u32) -> Self {
        self.template.top_k = Some(k);
        self
    }

    /// End generation when the model produces any of `sequences`.
    pub fn stop_sequences(
        mut self,
        sequences: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.template.stop_sequences = sequences.into_iter().map(Into::into).collect();
        self
    }

    /// Give the model a thinking budget, in tokens.
    pub fn thinking_budget(mut self, tokens: u32) -> Self {
        self.template.thinking_budget = Some(tokens);
        self
    }

    /// Add a built-in tool (Google Search, code execution, URL context),
    /// sent alongside the dispatcher's function declarations.
    pub fn built_in_tool(mut self, tool: gemini_genai_rs::prelude::Tool) -> Self {
        self.template.tools.push(tool);
        self
    }

    /// Constrain the reply to JSON matching `schema`.
    pub fn response_schema(mut self, schema: serde_json::Value) -> Self {
        self.template.response_mime_type = Some("application/json".into());
        self.template.response_json_schema = Some(schema);
        self
    }

    /// Also write the final text to state under `key` (it is always written
    /// to `"output"`).
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = Some(key.into());
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
        let mut req = LlmRequest {
            contents,
            system_instruction: instruction.clone(),
            ..self.template.clone()
        };
        if let Some(dispatcher) = &self.dispatcher {
            req.tools.extend(dispatcher.to_tool_declarations());
        }
        req
    }

    /// Call the model with streaming, passing each text chunk to `on_text`
    /// when it is set, and return the whole reply.
    async fn generate_streamed(
        &self,
        llm: &Arc<dyn BaseLlm>,
        request: LlmRequest,
        on_text: Option<&(dyn Fn(RunEvent) + Send + Sync)>,
    ) -> Result<LlmResponse, AgentError> {
        let mut chunks = llm
            .generate_stream(request)
            .await
            .map_err(AgentError::Llm)?;
        let mut whole: Option<LlmResponse> = None;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(AgentError::Llm)?;
            if let Some(on_text) = on_text {
                let text = chunk.text();
                if !text.is_empty() {
                    on_text(RunEvent::TextDelta(text));
                }
            }
            match &mut whole {
                Some(so_far) => so_far.append(chunk),
                None => whole = Some(chunk),
            }
        }
        Ok(whole.unwrap_or(LlmResponse {
            content: Content {
                role: Some(Role::Model),
                parts: Vec::new(),
            },
            finish_reason: None,
            usage: None,
        }))
    }

    /// Dispatch function calls and return function responses, firing middleware hooks.
    async fn dispatch_tools(
        &self,
        calls: &[FunctionCall],
        records: &mut Vec<ToolCallRecord>,
    ) -> Vec<FunctionResponse> {
        let mut responses = Vec::with_capacity(calls.len());
        for call in calls {
            // A model can call a tool the agent never declared; tell it so.
            let Some(dispatcher) = &self.dispatcher else {
                let missing = Err(crate::error::ToolError::NotFound(call.name.clone()));
                records.push(ToolCallRecord::new(
                    &call.name,
                    call.args.clone(),
                    missing.clone(),
                ));
                responses.push(ToolDispatcher::build_response(call, missing));
                continue;
            };
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
                let refused = Err(crate::error::ToolError::ExecutionFailed(e.to_string()));
                records.push(ToolCallRecord::new(
                    &call.name,
                    call.args.clone(),
                    refused.clone(),
                ));
                responses.push(ToolDispatcher::build_response(call, refused));
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

            records.push(ToolCallRecord::new(
                &call.name,
                call.args.clone(),
                result.clone(),
            ));
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
        let input = state.get::<String>("input").unwrap_or_default();
        Ok(self.run_with(RunRequest::new(input), state).await?.text)
    }

    async fn run_with(&self, request: RunRequest, state: &State) -> Result<RunResult, AgentError> {
        self.run_with_events(request, state, None).await
    }

    fn run_stream<'a>(
        &'a self,
        request: RunRequest,
        state: State,
    ) -> BoxStream<'a, Result<RunEvent, AgentError>> {
        // The run drives itself inside the returned stream, sending events
        // through a channel as they happen; `Finished` (or the error) is last.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let driver = async move {
            let finished = self.run_with_events(request, &state, Some(&tx)).await;
            let _ = tx.send(finished.map(RunEvent::Finished));
        };
        let events = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        });
        futures_util::stream::select(
            driver
                .into_stream()
                .filter_map(|()| async { None::<Result<RunEvent, AgentError>> }),
            events,
        )
        .boxed()
    }
}

impl LlmTextAgent {
    /// One run, reporting events to `events` as they happen when it is set.
    async fn run_with_events(
        &self,
        request: RunRequest,
        state: &State,
        events: Option<&EventSender>,
    ) -> Result<RunResult, AgentError> {
        // Resolve the instruction for this run: provider (against live
        // state) wins over the static string.
        let instruction = match &self.instruction_provider {
            Some(provider) => Some(provider.provide(state)),
            None => self.instruction.clone(),
        };

        // Resolve the LLM for this run: provider (against live state) wins
        // over the constructor's LLM.
        let llm = self
            .llm_provider
            .as_ref()
            .map(|p| p(state))
            .unwrap_or_else(|| self.llm.clone());

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
                match tokio::time::timeout(
                    limit,
                    self.run_inner(request, &instruction, &llm, events),
                )
                .await
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
            None => self.run_inner(request, &instruction, &llm, events).await,
        };

        match &result {
            Err(e) => {
                let _ = self.middleware.run_on_error(e).await;
            }
            Ok(done) => {
                let _ = state.set("output", &done.text);
                if let Some(key) = &self.output_key {
                    let _ = state.set(key, &done.text);
                }
                let _ = self
                    .middleware
                    .run_on_event(&AgentEvent::AgentCompleted {
                        name: self.name.clone(),
                    })
                    .await;
            }
        }

        result
    }

    /// Inner execution loop — separated so `on_error` fires exactly once.
    async fn run_inner(
        &self,
        request: RunRequest,
        instruction: &Option<String>,
        llm: &Arc<dyn BaseLlm>,
        events: Option<&EventSender>,
    ) -> Result<RunResult, AgentError> {
        let emit = |event: RunEvent| {
            if let Some(tx) = events {
                let _ = tx.send(Ok(event));
            }
        };
        // Without middleware nothing can rewrite a reply, so text is emitted
        // as it streams; otherwise only after `after_model` has seen it.
        let stream_live = events.is_some() && self.middleware.is_empty();
        let history_len = request.history.len();
        let mut contents = request.history;
        contents.push(request.input);
        let mut result = RunResult::default();

        for _round in 0..MAX_TOOL_ROUNDS {
            let mut llm_request = self.build_request(contents.clone(), instruction);
            if let Some(schema) = &request.response_schema {
                llm_request.response_mime_type = Some("application/json".into());
                llm_request.response_json_schema = Some(schema.clone());
            }

            // transform_request hook — may rewrite the request (e.g. context
            // policies trimming conversation history) before it is sent.
            self.middleware
                .run_transform_request(&mut llm_request)
                .await?;

            // before_model hook — may short-circuit with a cached response.
            let mut streamed = false;
            let response = match self.middleware.run_before_model(&llm_request).await? {
                Some(cached) => cached,
                None => {
                    let llm_response = if events.is_some() {
                        streamed = stream_live;
                        self.generate_streamed(
                            llm,
                            llm_request.clone(),
                            stream_live.then_some(&emit),
                        )
                        .await?
                    } else {
                        llm.generate(llm_request.clone())
                            .await
                            .map_err(AgentError::Llm)?
                    };
                    result.model_calls += 1;
                    if let Some(usage) = llm_response.usage {
                        result.usage += usage;
                    }

                    // after_model hook — may replace the response.
                    match self
                        .middleware
                        .run_after_model(&llm_request, &llm_response)
                        .await?
                    {
                        Some(replaced) => replaced,
                        None => llm_response,
                    }
                }
            };

            if events.is_some() && !streamed {
                let text = response.text();
                if !text.is_empty() {
                    emit(RunEvent::TextDelta(text));
                }
            }

            let calls: Vec<FunctionCall> = response.function_calls().into_iter().cloned().collect();

            if calls.is_empty() {
                // No tool calls — we have a final text response.
                result.text = response.text();
                if !response.content.parts.is_empty() {
                    contents.push(response.content);
                }
                result.messages = contents.split_off(history_len);
                return Ok(result);
            }

            // Move model response into conversation (no clone needed).
            contents.push(response.content);

            // Dispatch tools (middleware hooks inside). Media a tool
            // attached under `_media` is lifted out of the JSON and
            // delivered as inline_data parts in the same turn, so the
            // model *sees* images rather than base64 noise.
            for call in &calls {
                emit(RunEvent::ToolCall {
                    name: call.name.clone(),
                    args: call.args.clone(),
                });
            }
            let already = result.tool_calls.len();
            let tool_responses = self.dispatch_tools(&calls, &mut result.tool_calls).await;
            for record in &result.tool_calls[already..] {
                emit(RunEvent::ToolResult(record.clone()));
            }
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
        use crate::tool::{SimpleTool, ToolDispatcher, media};
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
        assert!(
            requests
                .iter()
                .all(|r| r.system_instruction.as_deref() == Some("You are a pirate."))
        );
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

    #[tokio::test]
    async fn llm_provider_switches_model_per_run() {
        // Two mock LLMs with distinguishable responses.
        struct MockLlmA;
        #[async_trait]
        impl BaseLlm for MockLlmA {
            fn model_id(&self) -> &str {
                "mock-a"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Ok(text_response("from-a"))
            }
        }

        struct MockLlmB;
        #[async_trait]
        impl BaseLlm for MockLlmB {
            fn model_id(&self) -> &str {
                "mock-b"
            }
            async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                Ok(text_response("from-b"))
            }
        }

        let llm_a = Arc::new(MockLlmA);
        let llm_b = Arc::new(MockLlmB);

        // Agent with provider that switches based on escalate flag.
        let agent = LlmTextAgent::new("switcher", llm_a.clone()).llm_provider(move |state| {
            if state.get::<bool>("escalate").unwrap_or(false) {
                llm_b.clone()
            } else {
                llm_a.clone()
            }
        });

        // Run 1: without escalate flag -> should use model A
        let state = State::new();
        let _ = state.set("input", "hi");
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "from-a", "without escalate, should use model A");

        // Run 2: with escalate flag -> should use model B
        let state2 = State::new();
        let _ = state2.set("input", "hi");
        let _ = state2.set("escalate", true);
        let result2 = agent.run(&state2).await.unwrap();
        assert_eq!(result2, "from-b", "with escalate, should use model B");
    }
}
