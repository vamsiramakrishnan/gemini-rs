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

mod llm;
mod fn_agent;
mod sequential;
mod parallel;
mod loop_agent;
mod fallback;
mod route;
mod race;
mod timeout;
mod map_over;
mod tap;
mod dispatch;

pub use llm::LlmTextAgent;
pub use fn_agent::FnTextAgent;
pub use sequential::SequentialTextAgent;
pub use parallel::ParallelTextAgent;
pub use loop_agent::LoopTextAgent;
pub use fallback::FallbackTextAgent;
pub use route::{RouteRule, RouteTextAgent};
pub use race::RaceTextAgent;
pub use timeout::TimeoutTextAgent;
pub use map_over::MapOverTextAgent;
pub use tap::TapTextAgent;
pub use dispatch::{TaskRegistry, DispatchTextAgent, JoinTextAgent};

// ── TextAgent trait ────────────────────────────────────────────────────────

/// A text-based agent that runs via `BaseLlm::generate()` (request/response).
///
/// Unlike `Agent` (which requires a Live WebSocket session), `TextAgent` can be
/// dispatched from anywhere — event hooks, background tasks, CLI tools.
#[async_trait]
pub trait TextAgent: Send + Sync {
    /// Human-readable name for logging and debugging.
    fn name(&self) -> &str;

    /// Execute this agent. Reads/writes `state`. Returns the final text output.
    async fn run(&self, state: &State) -> Result<String, AgentError>;
}

// Verify object safety at compile time.
const _: () = {
    fn _assert_object_safe(_: &dyn TextAgent) {}
};

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use crate::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
    use rs_genai::prelude::{Content, FunctionCall, Part, Role};

    /// A mock LLM that returns a fixed response.
    struct FixedLlm {
        response: String,
    }

    #[async_trait]
    impl BaseLlm for FixedLlm {
        fn model_id(&self) -> &str {
            "fixed-mock"
        }

        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: self.response.clone(),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    /// A mock LLM that echoes the input back with a prefix.
    struct EchoLlm {
        prefix: String,
    }

    #[async_trait]
    impl BaseLlm for EchoLlm {
        fn model_id(&self) -> &str {
            "echo-mock"
        }

        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let input_text: String = req
                .contents
                .iter()
                .flat_map(|c| &c.parts)
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");

            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: format!("{}{}", self.prefix, input_text),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    /// A mock LLM that issues a tool call on first request, then returns text.
    struct ToolCallingLlm {
        tool_name: String,
        tool_args: serde_json::Value,
        final_response: String,
    }

    #[async_trait]
    impl BaseLlm for ToolCallingLlm {
        fn model_id(&self) -> &str {
            "tool-mock"
        }

        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            // Check if we already have a function response in the conversation.
            let has_tool_response = req.contents.iter().any(|c| {
                c.parts
                    .iter()
                    .any(|p| matches!(p, Part::FunctionResponse { .. }))
            });

            if has_tool_response {
                // Already dispatched — return final text.
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::Text {
                            text: self.final_response.clone(),
                        }],
                    },
                    finish_reason: Some("STOP".into()),
                    usage: None,
                })
            } else {
                // First call — issue tool call.
                Ok(LlmResponse {
                    content: Content {
                        role: Some(Role::Model),
                        parts: vec![Part::FunctionCall {
                            function_call: FunctionCall {
                                name: self.tool_name.clone(),
                                args: self.tool_args.clone(),
                                id: Some("call-1".into()),
                            },
                        }],
                    },
                    finish_reason: None,
                    usage: None,
                })
            }
        }
    }

    /// A mock LLM that always fails.
    struct FailLlm;

    #[async_trait]
    impl BaseLlm for FailLlm {
        fn model_id(&self) -> &str {
            "fail-mock"
        }

        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::RequestFailed("intentional failure".into()))
        }
    }

    // ── TextAgent trait ──

    #[test]
    fn text_agent_is_object_safe() {
        fn _assert(_: &dyn TextAgent) {}
    }

    // ── LlmTextAgent ──

    #[tokio::test]
    async fn llm_text_agent_returns_text() {
        let llm = Arc::new(FixedLlm {
            response: "Hello world".into(),
        });
        let agent = LlmTextAgent::new("greeter", llm).instruction("Say hello");
        let state = State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "Hello world");
        assert_eq!(state.get::<String>("output"), Some("Hello world".into()));
    }

    #[tokio::test]
    async fn llm_text_agent_reads_input_from_state() {
        let llm = Arc::new(EchoLlm {
            prefix: "Echo: ".into(),
        });
        let agent = LlmTextAgent::new("echoer", llm);
        let state = State::new();
        state.set("input", "test message");
        let result = agent.run(&state).await.unwrap();
        assert!(result.contains("test message"));
    }

    #[tokio::test]
    async fn llm_text_agent_dispatches_tools() {
        let llm = Arc::new(ToolCallingLlm {
            tool_name: "get_weather".into(),
            tool_args: serde_json::json!({"city": "London"}),
            final_response: "The weather is sunny".into(),
        });

        let mut dispatcher = crate::tool::ToolDispatcher::new();
        dispatcher.register_function(Arc::new(crate::tool::SimpleTool::new(
            "get_weather",
            "Get weather",
            None,
            |_args| async { Ok(serde_json::json!({"temp": 22})) },
        )));

        let agent = LlmTextAgent::new("weather", llm).tools(Arc::new(dispatcher));
        let state = State::new();
        let result = agent.run(&state).await.unwrap();
        assert_eq!(result, "The weather is sunny");
    }

    #[tokio::test]
    async fn llm_text_agent_propagates_llm_error() {
        let llm = Arc::new(FailLlm);
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
            state.set("output", &upper);
            Ok(upper)
        });

        let state = State::new();
        state.set("input", "hello");
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
        let llm1: Arc<dyn BaseLlm> = Arc::new(FixedLlm {
            response: "step1 done".into(),
        });
        let llm2: Arc<dyn BaseLlm> = Arc::new(EchoLlm {
            prefix: "step2: ".into(),
        });

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
            Arc::new(LlmTextAgent::new("ok", Arc::new(FixedLlm {
                response: "fine".into(),
            }))),
            Arc::new(LlmTextAgent::new("fail", Arc::new(FailLlm))),
            Arc::new(LlmTextAgent::new("never", Arc::new(FixedLlm {
                response: "unreachable".into(),
            }))),
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
                state.set("key_a", "val_a");
                Ok("result_a".into())
            })),
            Arc::new(FnTextAgent::new("b", |state: &State| {
                state.set("key_b", "val_b");
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
            state.set("n", n + 1);
            Ok(format!("n={}", n + 1))
        }));

        let loop_agent = LoopTextAgent::new("loop", body, 100).until(|state: &State| {
            state.get::<i32>("n").unwrap_or(0) >= 3
        });

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
        let agent_a: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("a", |_| Ok("route_a".into())));
        let agent_b: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("b", |_| Ok("route_b".into())));
        let default: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("default", |_| Ok("default".into())));

        let router = RouteTextAgent::new(
            "router",
            vec![
                RouteRule::new(|s: &State| s.get::<String>("mode") == Some("a".into()), agent_a),
                RouteRule::new(|s: &State| s.get::<String>("mode") == Some("b".into()), agent_b),
            ],
            default,
        );

        let state = State::new();
        state.set("mode", "b");
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
        let fast: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("fast", |_| Ok("winner".into())));
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
        let fast: Arc<dyn TextAgent> =
            Arc::new(FnTextAgent::new("fast", |_| Ok("done".into())));
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
        state.set(
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
        state.set("input", "hello");
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
            vec![
                ("task_a".into(), agent_a),
                ("task_b".into(), agent_b),
            ],
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
            ("x".into(), Arc::new(FnTextAgent::new("x", |_| Ok("rx".into())))),
            ("y".into(), Arc::new(FnTextAgent::new("y", |_| Ok("ry".into())))),
            ("z".into(), Arc::new(FnTextAgent::new("z", |_| Ok("rz".into())))),
        ];

        let dispatch = DispatchTextAgent::new("dispatch", children, registry.clone(), budget);
        let state = State::new();
        dispatch.run(&state).await.unwrap();

        // Only join x and z
        let join = JoinTextAgent::new("joiner", registry.clone())
            .targets(vec!["x".into(), "z".into()]);
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

        let join = JoinTextAgent::new("joiner", registry)
            .timeout(Duration::from_millis(50));
        let err = join.run(&state).await.unwrap_err();
        assert!(matches!(err, AgentError::Timeout));
    }
}
