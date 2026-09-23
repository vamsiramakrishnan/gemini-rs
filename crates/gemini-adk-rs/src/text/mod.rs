//! Text-based agent execution — request/response LLM pipelines.
//!
//! While `Agent::run_live()` operates over a Gemini Live WebSocket session,
//! `TextAgent::run()` makes standard `BaseLlm::generate()` calls. This enables
//! dispatching text-based agent pipelines from Live session event hooks.
//!
//! # Agent types
//!
//! | Type | Purpose |
//! |------|---------|
//! | `LlmTextAgent` | Core agent — generate → tool dispatch → loop |
//! | `FnTextAgent` | Zero-cost state transform (no LLM call) |
//! | `SequentialTextAgent` | Run children in order, state flows forward |
//! | `ParallelTextAgent` | Run children concurrently via `tokio::spawn` |
//! | `LoopTextAgent` | Repeat until max iterations or predicate |
//! | `FallbackTextAgent` | Try each child, first success wins |
//! | `RouteTextAgent` | State-driven deterministic branching |
//! | `RaceTextAgent` | Run concurrently, first to finish wins |
//! | `TimeoutTextAgent` | Wrap an agent with a time limit |
//! | `MapOverTextAgent` | Iterate an agent over a list in state |
//! | `TapTextAgent` | Read-only observation (no mutation) |
//! | `DispatchTextAgent` | Fire-and-forget background tasks |
//! | `JoinTextAgent` | Wait for dispatched tasks |

use async_trait::async_trait;

use crate::error::AgentError;
use crate::state::State;

mod run;
pub use run::{Chat, RunEvent, RunRequest, RunResult, ToolCallRecord};

mod dispatch;
mod fallback;
mod fn_agent;
mod llm;
mod loop_agent;
mod map_over;
mod parallel;
mod race;
mod route;
mod sequential;
mod tap;
mod timeout;

pub use dispatch::{DispatchTextAgent, JoinTextAgent, TaskRegistry};
pub use fallback::FallbackTextAgent;
pub use fn_agent::FnTextAgent;
pub use llm::LlmTextAgent;
pub use loop_agent::LoopTextAgent;
pub use map_over::MapOverTextAgent;
pub use parallel::ParallelTextAgent;
pub use race::RaceTextAgent;
pub use route::{RouteRule, RouteTextAgent};
pub use sequential::SequentialTextAgent;
pub use tap::TapTextAgent;
pub use timeout::TimeoutTextAgent;

// ── TextAgent trait ────────────────────────────────────────────────────────

/// A text-based agent that runs via `BaseLlm::generate()` (request/response).
///
/// Unlike `Agent` (which requires a Live WebSocket session), `TextAgent` can be
/// dispatched from anywhere — event hooks, background tasks, CLI tools.
///
/// Ask a question, get a typed answer, or hold a conversation:
///
/// ```
/// use gemini_adk_rs::llm::{LlmResponse, MockLlm};
/// use gemini_adk_rs::text::{LlmTextAgent, TextAgent};
///
/// #[derive(serde::Deserialize, schemars::JsonSchema)]
/// struct City {
///     name: String,
///     country: String,
/// }
///
/// # tokio_test::block_on(async {
/// let llm = MockLlm::script([
///     LlmResponse::from_text("Paris."),
///     LlmResponse::from_text(r#"{"name":"Paris","country":"France"}"#),
/// ]);
/// let agent = LlmTextAgent::new("geo", llm);
///
/// assert_eq!(agent.ask("Capital of France?").await.unwrap(), "Paris.");
///
/// let city: City = agent.ask_as("Describe the capital of France.").await.unwrap();
/// assert_eq!(city.country, "France");
/// # });
/// ```
///
/// [`run_with`](Self::run_with) is the primitive the others are built on;
/// [`run`](Self::run) is the state-in, state-out form combinators use, reading
/// the prompt from the `"input"` state key.
#[async_trait]
pub trait TextAgent: Send + Sync {
    /// Human-readable name for logging and debugging.
    fn name(&self) -> &str;

    /// Execute this agent. Reads/writes `state`. Returns the final text output.
    ///
    /// The prompt is read from the `"input"` state key; prefer
    /// [`run_with`](Self::run_with) or [`ask`](Self::ask), which take it
    /// directly.
    async fn run(&self, state: &State) -> Result<String, AgentError>;

    /// Run one request and report everything it produced: the reply, the
    /// turns to append to a conversation, token usage and tool calls.
    ///
    /// The default writes the request's text to the `"input"` state key and
    /// calls [`run`](Self::run), so every agent supports it; history and a
    /// response schema are honoured by agents that call a model
    /// ([`LlmTextAgent`]).
    async fn run_with(&self, request: RunRequest, state: &State) -> Result<RunResult, AgentError> {
        state.set("input", request.input_text())?;
        let text = self.run(state).await?;
        Ok(RunResult {
            messages: vec![request.input, run::model_turn(text.clone())],
            ..RunResult::from_text(text)
        })
    }

    /// Run one request as a stream of [`RunEvent`]s: text as the model writes
    /// it, each tool call and result, then [`RunEvent::Finished`].
    ///
    /// The default emits the reply of [`run_with`](Self::run_with) as a single
    /// delta. [`LlmTextAgent`] streams from the model; when it has middleware
    /// (which may rewrite a reply, e.g. to redact it), each model turn is
    /// emitted only after the middleware has seen it.
    fn run_stream<'a>(
        &'a self,
        request: RunRequest,
        state: State,
    ) -> futures_util::stream::BoxStream<'a, Result<RunEvent, AgentError>> {
        use futures_util::StreamExt;

        futures_util::stream::once(async move { self.run_with(request, &state).await })
            .flat_map(|outcome| {
                let events = match outcome {
                    Ok(result) if result.text.is_empty() => vec![Ok(RunEvent::Finished(result))],
                    Ok(result) => vec![
                        Ok(RunEvent::TextDelta(result.text.clone())),
                        Ok(RunEvent::Finished(result)),
                    ],
                    Err(e) => vec![Err(e)],
                };
                futures_util::stream::iter(events)
            })
            .boxed()
    }

    /// Stream the reply to one question, with no history and fresh state.
    ///
    /// ```
    /// use futures_util::StreamExt;
    /// use gemini_adk_rs::llm::MockLlm;
    /// use gemini_adk_rs::text::{LlmTextAgent, RunEvent, TextAgent};
    ///
    /// # tokio_test::block_on(async {
    /// let agent = LlmTextAgent::new("storyteller", MockLlm::text("Once upon a time"));
    /// let mut events = agent.stream("Tell me a story.");
    /// let mut story = String::new();
    /// while let Some(event) = events.next().await {
    ///     if let RunEvent::TextDelta(text) = event.unwrap() {
    ///         story.push_str(&text);
    ///     }
    /// }
    /// assert_eq!(story, "Once upon a time");
    /// # });
    /// ```
    fn stream(
        &self,
        prompt: impl Into<String>,
    ) -> futures_util::stream::BoxStream<'_, Result<RunEvent, AgentError>>
    where
        Self: Sized,
    {
        self.run_stream(RunRequest::new(prompt), State::new())
    }

    /// Ask one question, with no history and fresh state, and get the reply.
    async fn ask(&self, prompt: impl Into<String> + Send) -> Result<String, AgentError>
    where
        Self: Sized,
    {
        Ok(self
            .run_with(RunRequest::new(prompt), &State::new())
            .await?
            .text)
    }

    /// Ask one question and get the reply as a `T`.
    ///
    /// `T`'s JSON Schema is sent as the response schema. If the reply still
    /// does not parse, the model is shown the error and asked once more;
    /// a second failure is [`AgentError::InvalidOutput`].
    async fn ask_as<T>(&self, prompt: impl Into<String> + Send) -> Result<T, AgentError>
    where
        Self: Sized,
        T: serde::de::DeserializeOwned + schemars::JsonSchema + Send,
    {
        let schema = crate::tool::wire_schema::<T>();
        let state = State::new();
        let first = self
            .run_with(
                RunRequest::new(prompt).response_schema(schema.clone()),
                &state,
            )
            .await?;
        let reason = match first.parse::<T>() {
            Err(AgentError::InvalidOutput { reason, .. }) => reason,
            parsed => return parsed,
        };
        let repair = RunRequest::new(format!(
            "That reply could not be read as the requested JSON ({reason}). \
             Reply again with only JSON that matches the schema."
        ))
        .history(first.messages)
        .response_schema(schema);
        self.run_with(repair, &state).await?.parse::<T>()
    }

    /// Start a conversation that remembers its turns. See [`Chat`].
    fn chat(&self) -> Chat<&Self>
    where
        Self: Sized,
    {
        Chat::new(self)
    }
}

// Verify object safety at compile time.
const _: () = {
    fn _assert_object_safe(_: &dyn TextAgent) {}
};

/// Forward every required method, so a wrapper behaves exactly like the agent
/// it holds (including an overridden `run_with`).
macro_rules! forward_text_agent {
    ($($wrapper:ty),*) => {$(
        #[async_trait]
        impl<A: TextAgent + ?Sized> TextAgent for $wrapper {
            fn name(&self) -> &str {
                (**self).name()
            }

            async fn run(&self, state: &State) -> Result<String, AgentError> {
                (**self).run(state).await
            }

            async fn run_with(
                &self,
                request: RunRequest,
                state: &State,
            ) -> Result<RunResult, AgentError> {
                (**self).run_with(request, state).await
            }

            fn run_stream<'a>(
                &'a self,
                request: RunRequest,
                state: State,
            ) -> futures_util::stream::BoxStream<'a, Result<RunEvent, AgentError>> {
                (**self).run_stream(request, state)
            }
        }
    )*};
}

// A shared agent is an agent, so a built `Arc<dyn TextAgent>` can be passed
// straight back into any combinator or `agent_tool` without a cast; a borrow
// is one too, which is what `chat()` holds.
forward_text_agent!(std::sync::Arc<A>, Box<A>, &A);

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{BaseLlm, LlmError, LlmResponse, MockLlm};
    use gemini_genai_rs::prelude::Part;
    use std::sync::Arc;
    use std::time::Duration;

    /// A model that echoes every text part it was sent, after `prefix`.
    fn echo(prefix: &'static str) -> MockLlm {
        MockLlm::from_fn(move |req| {
            let input: Vec<&str> = req
                .contents
                .iter()
                .flat_map(|c| &c.parts)
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            Ok(LlmResponse::from_text(format!(
                "{prefix}{}",
                input.join(" ")
            )))
        })
    }

    /// A model whose every call fails.
    fn failing() -> MockLlm {
        MockLlm::from_fn(|_| Err(LlmError::RequestFailed("intentional failure".into())))
    }

    // ── TextAgent trait ──

    #[test]
    fn text_agent_is_object_safe() {
        fn _assert(_: &dyn TextAgent) {}
    }

    /// A built agent is handed around as `Arc<dyn TextAgent>`; it must satisfy
    /// a generic `impl TextAgent` bound without a cast or a second API.
    #[tokio::test]
    async fn shared_and_boxed_agents_are_agents() {
        async fn run_it(agent: impl TextAgent) -> String {
            agent.run(&State::new()).await.unwrap()
        }
        let built: Arc<dyn TextAgent> = Arc::new(LlmTextAgent::new("a", MockLlm::text("from arc")));
        assert_eq!(built.name(), "a");
        assert_eq!(run_it(built).await, "from arc");
        let boxed: Box<dyn TextAgent> = Box::new(FnTextAgent::new("b", |_| Ok("from box".into())));
        assert_eq!(run_it(boxed).await, "from box");
    }

    // ── run_with, ask, ask_as, chat ──

    #[tokio::test]
    async fn run_with_reports_messages_usage_and_tool_calls() {
        let llm = MockLlm::script([
            LlmResponse::tool_call("get_weather", serde_json::json!({"city": "Oslo"}))
                .with_usage(10, 2),
            LlmResponse::from_text("Cold.").with_usage(20, 1),
        ]);
        let mut dispatcher = crate::tool::ToolDispatcher::new();
        dispatcher.register_function(Arc::new(crate::tool::SimpleTool::new(
            "get_weather",
            "Get weather",
            None,
            |_| async { Ok(serde_json::json!({"temp": -3})) },
        )));
        let agent = LlmTextAgent::new("weather", llm).tools(Arc::new(dispatcher));

        let result = agent
            .run_with(RunRequest::new("Weather in Oslo?"), &State::new())
            .await
            .unwrap();
        assert_eq!(result.text, "Cold.");
        assert_eq!(result.model_calls, 2);
        assert_eq!(result.usage, crate::llm::TokenUsage::new(30, 3));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "get_weather");
        assert_eq!(result.tool_calls[0].outcome.as_ref().unwrap()["temp"], -3);
        // user, model(call), user(tool response), model(text)
        assert_eq!(result.messages.len(), 4);
    }

    #[tokio::test]
    async fn ask_as_repairs_once_then_gives_up() {
        #[derive(serde::Deserialize, schemars::JsonSchema, Debug)]
        #[allow(dead_code)]
        struct Answer {
            value: u32,
        }

        let fixed = MockLlm::script([
            LlmResponse::from_text("forty-two"),
            LlmResponse::from_text(r#"{"value": 42}"#),
        ]);
        let agent = LlmTextAgent::new("a", fixed.clone());
        let answer: Answer = agent.ask_as("What is 6 x 7?").await.unwrap();
        assert_eq!(answer.value, 42);
        let repair = fixed.last_request().unwrap();
        assert_eq!(
            repair.contents.len(),
            3,
            "the bad reply is shown to the model"
        );
        assert!(repair.response_json_schema.is_some());

        let stubborn = MockLlm::text("no");
        let agent = LlmTextAgent::new("b", stubborn.clone());
        let err = agent.ask_as::<Answer>("?").await.unwrap_err();
        assert!(matches!(err, AgentError::InvalidOutput { .. }), "{err}");
        assert_eq!(stubborn.call_count(), 2, "one repair, not a loop");
    }

    #[tokio::test]
    async fn any_agent_can_ask_and_chat() {
        let upper = FnTextAgent::new("upper", |state| {
            Ok(state
                .get::<String>("input")
                .unwrap_or_default()
                .to_uppercase())
        });
        assert_eq!(upper.ask("hi").await.unwrap(), "HI");
        let shared: Arc<dyn TextAgent> = Arc::new(upper);
        let mut chat = shared.chat();
        assert_eq!(chat.send("one").await.unwrap(), "ONE");
        assert_eq!(chat.send("two").await.unwrap(), "TWO");
        assert_eq!(chat.history().len(), 4);
    }

    #[tokio::test]
    async fn a_failed_turn_is_not_added_to_the_history() {
        let llm = MockLlm::script([LlmResponse::from_text("first")]).then_fail(LlmError::Api {
            status: 503,
            message: "overloaded".into(),
        });
        let agent = LlmTextAgent::new("a", llm);
        let mut chat = Chat::new(&agent);
        chat.send("1").await.unwrap();
        let err = chat.send("2").await.unwrap_err();
        assert!(err.as_llm().is_some_and(LlmError::is_retryable), "{err}");
        assert_eq!(chat.history().len(), 2);
    }

    /// A model that calls a tool on an agent with no tools is told so, instead
    /// of receiving an empty turn and calling again until the round limit.
    #[tokio::test]
    async fn a_tool_call_without_tools_is_answered_not_found() {
        let llm = MockLlm::script([
            LlmResponse::tool_call("imaginary", serde_json::json!({})),
            LlmResponse::from_text("Sorry, I cannot do that."),
        ]);
        let agent = LlmTextAgent::new("toolless", llm.clone());
        let result = agent
            .run_with(RunRequest::new("Use your tool."), &State::new())
            .await
            .unwrap();
        assert_eq!(result.text, "Sorry, I cannot do that.");
        assert!(matches!(
            result.tool_calls[0].outcome,
            Err(crate::error::ToolError::NotFound(_))
        ));
    }

    // ── Streaming ──

    async fn collect(
        mut events: futures_util::stream::BoxStream<'_, Result<RunEvent, AgentError>>,
    ) -> Vec<RunEvent> {
        use futures_util::StreamExt;
        let mut out = Vec::new();
        while let Some(event) = events.next().await {
            out.push(event.unwrap());
        }
        out
    }

    fn deltas(events: &[RunEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                RunEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Text arrives as the model writes it, tool calls and results in between,
    /// and `Finished` carries the same result `run_with` would return.
    #[tokio::test]
    async fn stream_reports_text_tools_and_the_result_in_order() {
        let llm = MockLlm::script([
            LlmResponse::tool_call("get_weather", serde_json::json!({"city": "Oslo"})),
            LlmResponse::from_text("It is cold today").with_usage(9, 4),
        ]);
        let mut dispatcher = crate::tool::ToolDispatcher::new();
        dispatcher.register_function(Arc::new(crate::tool::SimpleTool::new(
            "get_weather",
            "Get weather",
            None,
            |_| async { Ok(serde_json::json!({"temp": -3})) },
        )));
        let agent = LlmTextAgent::new("weather", llm).tools(Arc::new(dispatcher));

        let events = collect(agent.stream("Weather in Oslo?")).await;
        assert!(matches!(&events[0], RunEvent::ToolCall { name, .. } if name == "get_weather"));
        assert!(matches!(&events[1], RunEvent::ToolResult(r) if r.outcome.is_ok()));
        let text_deltas = events
            .iter()
            .filter(|e| matches!(e, RunEvent::TextDelta(_)))
            .count();
        assert_eq!(text_deltas, 4, "one per word from the mock: {events:?}");
        assert_eq!(deltas(&events), "It is cold today");
        match events.last() {
            Some(RunEvent::Finished(result)) => {
                assert_eq!(result.text, "It is cold today");
                assert_eq!(result.usage, crate::llm::TokenUsage::new(9, 4));
                assert_eq!(result.tool_calls.len(), 1);
            }
            other => panic!("the last event must be Finished, got {other:?}"),
        }
    }

    /// A middleware that rewrites replies must see a turn before any of it is
    /// streamed, or a redaction could be bypassed through the deltas.
    #[tokio::test]
    async fn middleware_sees_a_reply_before_it_is_streamed() {
        struct Redact;
        #[async_trait]
        impl crate::middleware::Middleware for Redact {
            fn name(&self) -> &str {
                "redact"
            }
            async fn after_model(
                &self,
                _request: &crate::llm::LlmRequest,
                _response: &LlmResponse,
            ) -> Result<Option<LlmResponse>, AgentError> {
                Ok(Some(LlmResponse::from_text("[redacted]")))
            }
        }
        let agent = LlmTextAgent::new("guarded", MockLlm::text("the secret is 42"))
            .add_middleware(Arc::new(Redact));
        let events = collect(agent.stream("?")).await;
        assert_eq!(deltas(&events), "[redacted]", "{events:?}");
    }

    #[tokio::test]
    async fn any_agent_streams_and_chat_streams_into_its_history() {
        let echo = FnTextAgent::new("echo", |state| {
            Ok(state.get::<String>("input").unwrap_or_default())
        });
        let events = collect(echo.stream("hi")).await;
        assert_eq!(deltas(&events), "hi");

        let agent = LlmTextAgent::new("a", MockLlm::text("hello there"));
        let mut chat = agent.chat();
        let events = collect(chat.send_stream("hi")).await;
        assert_eq!(deltas(&events), "hello there");
        assert_eq!(chat.history().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_stream_ends_with_the_error() {
        use futures_util::StreamExt;
        let agent = LlmTextAgent::new("a", failing());
        let mut events = agent.stream("?");
        let last = events.next().await.expect("one event");
        assert!(matches!(last, Err(AgentError::Llm(_))), "{last:?}");
        assert!(events.next().await.is_none());
    }

    // ── LlmTextAgent ──

    #[tokio::test]
    async fn llm_text_agent_returns_text() {
        let llm = Arc::new(MockLlm::text("Hello world"));
        let agent = LlmTextAgent::new("greeter", llm).instruction("Say hello");
        let state = State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "Hello world");
        assert_eq!(state.get::<String>("output"), Some("Hello world".into()));
    }

    #[tokio::test]
    async fn llm_text_agent_reads_input_from_state() {
        let llm = Arc::new(echo("Echo: "));
        let agent = LlmTextAgent::new("echoer", llm);
        let state = State::new();
        let _ = state.set("input", "test message");
        let result = agent.run(&state).await.unwrap();
        assert!(result.contains("test message"));
    }

    #[tokio::test]
    async fn llm_text_agent_dispatches_tools() {
        let llm = MockLlm::script([
            LlmResponse::tool_call("get_weather", serde_json::json!({"city": "London"})),
            LlmResponse::from_text("The weather is sunny"),
        ]);

        let mut dispatcher = crate::tool::ToolDispatcher::new();
        dispatcher.register_function(Arc::new(crate::tool::SimpleTool::new(
            "get_weather",
            "Get weather",
            None,
            |_args| async { Ok(serde_json::json!({"temp": 22})) },
        )));

        let agent = LlmTextAgent::new("weather", llm.clone()).tools(Arc::new(dispatcher));
        let state = State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "The weather is sunny");

        // The second model call carries the tool's result back.
        let followup = llm.last_request().expect("two model calls");
        let returned = followup
            .contents
            .iter()
            .flat_map(|c| &c.parts)
            .find_map(|p| match p {
                Part::FunctionResponse { function_response } => Some(function_response),
                _ => None,
            })
            .expect("the tool result is sent back to the model");
        assert_eq!(returned.name, "get_weather");
        assert_eq!(returned.response["temp"], 22);
    }

    #[tokio::test]
    async fn llm_text_agent_propagates_llm_error() {
        let llm = Arc::new(failing());
        let agent = LlmTextAgent::new("failer", llm);
        let state = State::new();
        let result = agent.run(&state).await;
        assert!(result.is_err());
    }

    // ── FnTextAgent ──

    #[tokio::test]
    async fn fn_agent_transforms_state() {
        let agent = FnTextAgent::new("upper", |state: &State| {
            let input = state.get::<String>("input").unwrap_or_default();
            let upper = input.to_uppercase();
            let _ = state.set("output", &upper);
            Ok(upper)
        });

        let state = State::new();
        let _ = state.set("input", "hello");
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "HELLO");
        assert_eq!(state.get::<String>("output"), Some("HELLO".into()));
    }

    #[tokio::test]
    async fn fn_agent_can_fail() {
        let agent = FnTextAgent::new("failer", |_state: &State| {
            Err(AgentError::Other("nope".into()))
        });
        let state = State::new();
        assert!(agent.run(&state).await.is_err());
    }

    // ── SequentialTextAgent ──

    #[tokio::test]
    async fn sequential_chains_agents() {
        let llm1: Arc<dyn BaseLlm> = Arc::new(MockLlm::text("step1 done"));
        let llm2: Arc<dyn BaseLlm> = Arc::new(echo("step2: "));

        let children: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(LlmTextAgent::new("step1", llm1)),
            Arc::new(LlmTextAgent::new("step2", llm2)),
        ];

        let pipeline = SequentialTextAgent::new("pipeline", children);
        let state = State::new();
        let result = pipeline.run(&state).await.unwrap();
        // step2 should receive step1's output as input
        assert!(result.contains("step2:"));
        assert!(result.contains("step1 done"));
    }

    #[tokio::test]
    async fn sequential_stops_on_error() {
        let children: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(LlmTextAgent::new("ok", Arc::new(MockLlm::text("fine")))),
            Arc::new(LlmTextAgent::new("fail", Arc::new(failing()))),
            Arc::new(LlmTextAgent::new(
                "never",
                Arc::new(MockLlm::text("unreachable")),
            )),
        ];

        let pipeline = SequentialTextAgent::new("pipeline", children);
        let state = State::new();
        assert!(pipeline.run(&state).await.is_err());
    }

    #[tokio::test]
    async fn sequential_empty_returns_empty() {
        let pipeline = SequentialTextAgent::new("empty", vec![]);
        let state = State::new();
        let result = pipeline.run(&state).await.unwrap();
        assert_eq!(result, "");
    }

    // ── ParallelTextAgent ──

    #[tokio::test]
    async fn parallel_runs_concurrently() {
        let branches: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(FnTextAgent::new("a", |state: &State| {
                let _ = state.set("key_a", "val_a");
                Ok("result_a".into())
            })),
            Arc::new(FnTextAgent::new("b", |state: &State| {
                let _ = state.set("key_b", "val_b");
                Ok("result_b".into())
            })),
        ];

        let par = ParallelTextAgent::new("parallel", branches);
        let state = State::new();
        let result = par.run(&state).await.unwrap();
        assert!(result.contains("result_a"));
        assert!(result.contains("result_b"));
        assert_eq!(state.get::<String>("key_a"), Some("val_a".into()));
        assert_eq!(state.get::<String>("key_b"), Some("val_b".into()));
    }

    #[tokio::test]
    async fn parallel_fails_if_any_fails() {
        let branches: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(FnTextAgent::new("ok", |_| Ok("fine".into()))),
            Arc::new(FnTextAgent::new("fail", |_| {
                Err(AgentError::Other("boom".into()))
            })),
        ];

        let par = ParallelTextAgent::new("parallel", branches);
        let state = State::new();
        assert!(par.run(&state).await.is_err());
    }

    // ── LoopTextAgent ──

    #[tokio::test]
    async fn loop_runs_max_iterations() {
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter_clone = counter.clone();

        let body = Arc::new(FnTextAgent::new("counter", move |_state: &State| {
            counter_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("tick".into())
        }));

        let loop_agent = LoopTextAgent::new("loop", body, 5);
        let state = State::new();
        loop_agent.run(&state).await.unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn loop_breaks_on_predicate() {
        let body = Arc::new(FnTextAgent::new("incrementer", |state: &State| {
            let n = state.get::<i32>("n").unwrap_or(0);
            let _ = state.set("n", n + 1);
            Ok(format!("n={}", n + 1))
        }));

        let loop_agent = LoopTextAgent::new("loop", body, 100)
            .until(|state: &State| state.get::<i32>("n").unwrap_or(0) >= 3);

        let state = State::new();
        loop_agent.run(&state).await.unwrap();
        assert_eq!(state.get::<i32>("n"), Some(3));
    }

    // ── FallbackTextAgent ──

    #[tokio::test]
    async fn fallback_returns_first_success() {
        let candidates: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(FnTextAgent::new("fail1", |_| {
                Err(AgentError::Other("fail1".into()))
            })),
            Arc::new(FnTextAgent::new("ok", |_| Ok("success".into()))),
            Arc::new(FnTextAgent::new("never", |_| Ok("unreachable".into()))),
        ];

        let fallback = FallbackTextAgent::new("fallback", candidates);
        let state = State::new();
        let result = fallback.run(&state).await.unwrap();
        assert_eq!(result, "success");
    }

    #[tokio::test]
    async fn fallback_returns_last_error() {
        let candidates: Vec<Arc<dyn TextAgent>> = vec![
            Arc::new(FnTextAgent::new("fail1", |_| {
                Err(AgentError::Other("fail1".into()))
            })),
            Arc::new(FnTextAgent::new("fail2", |_| {
                Err(AgentError::Other("fail2".into()))
            })),
        ];

        let fallback = FallbackTextAgent::new("fallback", candidates);
        let state = State::new();
        let err = fallback.run(&state).await.unwrap_err();
        assert!(err.to_string().contains("fail2"));
    }

    #[tokio::test]
    async fn fallback_empty_returns_error() {
        let fallback = FallbackTextAgent::new("fallback", vec![]);
        let state = State::new();
        assert!(fallback.run(&state).await.is_err());
    }

    // ── RouteTextAgent ──

    #[tokio::test]
    async fn route_dispatches_matching_rule() {
        let agent_a: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("a", |_| Ok("route_a".into())));
        let agent_b: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("b", |_| Ok("route_b".into())));
        let default: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("default", |_| Ok("default".into())));

        let router = RouteTextAgent::new(
            "router",
            vec![
                RouteRule::new(
                    |s: &State| s.get::<String>("mode") == Some("a".into()),
                    agent_a,
                ),
                RouteRule::new(
                    |s: &State| s.get::<String>("mode") == Some("b".into()),
                    agent_b,
                ),
            ],
            default,
        );

        let state = State::new();
        let _ = state.set("mode", "b");
        let result = router.run(&state).await.unwrap();
        assert_eq!(result, "route_b");
    }

    #[tokio::test]
    async fn route_uses_default_when_no_match() {
        let default: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("default", |_| Ok("fallback".into())));

        let router = RouteTextAgent::new(
            "router",
            vec![RouteRule::new(|_: &State| false, default.clone())],
            default,
        );

        let state = State::new();
        let result = router.run(&state).await.unwrap();
        assert_eq!(result, "fallback");
    }

    // ── Async test helper ──

    /// A test agent that sleeps asynchronously (cooperative with tokio timeout).
    struct AsyncSleepAgent {
        delay: Duration,
    }

    #[async_trait]
    impl TextAgent for AsyncSleepAgent {
        fn name(&self) -> &str {
            "async-sleeper"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            tokio::time::sleep(self.delay).await;
            Ok("too late".into())
        }
    }

    // ── RaceTextAgent ──

    #[tokio::test]
    async fn race_returns_first_to_complete() {
        // Fast agent completes immediately, slow agent sleeps async.
        let fast: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("fast", |_| Ok("winner".into())));
        let slow: Arc<dyn TextAgent> = Arc::new(AsyncSleepAgent {
            delay: Duration::from_millis(500),
        });

        let race = RaceTextAgent::new("race", vec![fast, slow]);
        let state = State::new();
        let result = race.run(&state).await.unwrap();
        assert_eq!(result, "winner");
    }

    #[tokio::test]
    async fn race_empty_returns_error() {
        let race = RaceTextAgent::new("race", vec![]);
        let state = State::new();
        assert!(race.run(&state).await.is_err());
    }

    // ── TimeoutTextAgent ──

    #[tokio::test]
    async fn timeout_returns_result_within_limit() {
        let fast: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("fast", |_| Ok("done".into())));
        let timeout = TimeoutTextAgent::new("timeout", fast, Duration::from_secs(5));
        let state = State::new();
        let result = timeout.run(&state).await.unwrap();
        assert_eq!(result, "done");
    }

    #[tokio::test]
    async fn timeout_returns_error_when_exceeded() {
        let slow: Arc<dyn TextAgent> = Arc::new(AsyncSleepAgent {
            delay: Duration::from_secs(2),
        });
        let timeout = TimeoutTextAgent::new("timeout", slow, Duration::from_millis(50));
        let state = State::new();
        let err = timeout.run(&state).await.unwrap_err();
        assert!(matches!(err, AgentError::Timeout));
    }

    // ── MapOverTextAgent ──

    #[tokio::test]
    async fn map_over_iterates_items() {
        let agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("upper", |state: &State| {
            let item: String = state
                .get::<serde_json::Value>("_item")
                .map(|v| v.as_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(item.to_uppercase())
        }));

        let map = MapOverTextAgent::new("mapper", agent, "items");
        let state = State::new();
        let _ = state.set(
            "items",
            vec![
                serde_json::Value::String("hello".into()),
                serde_json::Value::String("world".into()),
            ],
        );

        let result = map.run(&state).await.unwrap();
        assert!(result.contains("HELLO"));
        assert!(result.contains("WORLD"));

        let results: Vec<String> = state.get("_results").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], "HELLO");
        assert_eq!(results[1], "WORLD");
    }

    #[tokio::test]
    async fn map_over_empty_list() {
        let agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("noop", |_| Ok("x".into())));
        let map = MapOverTextAgent::new("mapper", agent, "items");
        let state = State::new();
        // no "items" key → empty Vec
        let result = map.run(&state).await.unwrap();
        assert_eq!(result, "");
    }

    // ── TapTextAgent ──

    #[tokio::test]
    async fn tap_observes_state() {
        let observed = Arc::new(std::sync::Mutex::new(String::new()));
        let observed_clone = observed.clone();

        let tap = TapTextAgent::new("observer", move |state: &State| {
            let val = state.get::<String>("input").unwrap_or_default();
            *observed_clone.lock().unwrap() = val;
        });

        let state = State::new();
        let _ = state.set("input", "hello");
        let result = tap.run(&state).await.unwrap();
        assert_eq!(result, ""); // Tap returns empty string
        assert_eq!(*observed.lock().unwrap(), "hello");
    }

    // ── DispatchTextAgent + JoinTextAgent ──

    #[tokio::test]
    async fn dispatch_and_join_round_trip() {
        let registry = TaskRegistry::new();
        let budget = Arc::new(tokio::sync::Semaphore::new(10));

        let agent_a: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("task_a", |_| Ok("result_a".into())));
        let agent_b: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("task_b", |_| Ok("result_b".into())));

        let dispatch = DispatchTextAgent::new(
            "dispatch",
            vec![("task_a".into(), agent_a), ("task_b".into(), agent_b)],
            registry.clone(),
            budget,
        );

        let state = State::new();
        let dispatch_result = dispatch.run(&state).await.unwrap();
        assert_eq!(dispatch_result, ""); // Fire-and-forget returns empty

        let join = JoinTextAgent::new("joiner", registry);
        let join_result = join.run(&state).await.unwrap();
        assert!(join_result.contains("result_a"));
        assert!(join_result.contains("result_b"));
    }

    #[tokio::test]
    async fn join_with_target_names() {
        let registry = TaskRegistry::new();
        let budget = Arc::new(tokio::sync::Semaphore::new(10));

        let children: Vec<(String, Arc<dyn TextAgent>)> = vec![
            (
                "x".into(),
                Arc::new(FnTextAgent::new("x", |_| Ok("rx".into()))),
            ),
            (
                "y".into(),
                Arc::new(FnTextAgent::new("y", |_| Ok("ry".into()))),
            ),
            (
                "z".into(),
                Arc::new(FnTextAgent::new("z", |_| Ok("rz".into()))),
            ),
        ];

        let dispatch = DispatchTextAgent::new("dispatch", children, registry.clone(), budget);
        let state = State::new();
        dispatch.run(&state).await.unwrap();

        // Only join x and z
        let join =
            JoinTextAgent::new("joiner", registry.clone()).targets(vec!["x".into(), "z".into()]);
        let result = join.run(&state).await.unwrap();
        assert!(result.contains("rx"));
        assert!(result.contains("rz"));

        // y should still be in registry
        let remaining = registry.inner.lock().await;
        assert!(remaining.contains_key("y"));
    }

    #[tokio::test]
    async fn join_with_timeout() {
        let registry = TaskRegistry::new();
        let budget = Arc::new(tokio::sync::Semaphore::new(10));

        let slow: Arc<dyn TextAgent> = Arc::new(AsyncSleepAgent {
            delay: Duration::from_secs(2),
        });

        let dispatch = DispatchTextAgent::new(
            "dispatch",
            vec![("slow".into(), slow)],
            registry.clone(),
            budget,
        );
        let state = State::new();
        dispatch.run(&state).await.unwrap();

        let join = JoinTextAgent::new("joiner", registry).timeout(Duration::from_millis(50));
        let err = join.run(&state).await.unwrap_err();
        assert!(matches!(err, AgentError::Timeout));
    }
}
