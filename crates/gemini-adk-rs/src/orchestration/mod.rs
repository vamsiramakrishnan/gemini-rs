//! Agent orchestration — invoke an agent in a [`Mode`].
//!
//! An agent is a value ([`TextAgent`] — a local agent, a composed pipeline, or a
//! remote A2A agent). Orchestration is the single question of *how* you invoke
//! it; the result always lands in governed `State` under `{name}:result` (or
//! `{name}:error`), so coordination is reactive and uniform regardless of the
//! invoker (the model, a `Flow`, an `Extract`, or a watcher).
//!
//! | Mode | Sync? | Lowers to |
//! |------|-------|-----------|
//! | [`Mode::Call`] | sync — caller awaits | [`call`] (agent-as-tool, awaited inline) |
//! | [`Mode::Dispatch`] | async, fire-and-forget | [`BackgroundAgentDispatcher::dispatch`](crate::live::BackgroundAgentDispatcher) |
//! | [`Mode::Background`] | async, model-aware | an agent-tool marked [`ToolExecutionMode::Background`](crate::live::ToolExecutionMode) |
//!
//! All three write `{name}:result`, so a `Flow` step can complete on a resolved
//! result via [`Guard::resolved`](crate::flow::Guard::resolved), and any
//! consumer reads the value the same way.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::error::AgentError;
use crate::llm::{BaseLlm, LlmRequest};
use crate::state::State;
use crate::text::TextAgent;

/// How an agent is invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Synchronous — the caller awaits the result. Use only for *fast*
    /// dependencies (a voice session should not block on slow work).
    Call,
    /// Asynchronous, fire-and-forget — the conversation does not wait.
    Dispatch,
    /// Asynchronous, model-aware — runs detached; the result is delivered back
    /// to the model via `FunctionResponseScheduling`.
    Background,
}

/// State key an agent's successful result is written to.
pub fn result_key(name: &str) -> String {
    format!("{name}:result")
}

/// State key an agent's error is written to.
pub fn error_key(name: &str) -> String {
    format!("{name}:error")
}

/// The provenance source of a value at `key` (e.g. `"agent"`, `"fetch"`,
/// `"llm"`, or `"extraction"`), if one was recorded under `state_meta:{key}`.
pub fn provenance(state: &State, key: &str) -> Option<String> {
    state
        .get::<serde_json::Value>(&format!("state_meta:{key}"))
        .and_then(|m| m.get("source").and_then(|s| s.as_str().map(String::from)))
}

/// Invoke `agent` **synchronously**: run it to completion, write its result to
/// `{name}:result` (or its error to `{name}:error`), and return the result.
///
/// This is the [`Mode::Call`] lowering. It uses the same `{name}:result`
/// convention as [`BackgroundAgentDispatcher::dispatch`](crate::live::BackgroundAgentDispatcher),
/// so sync and async invocations are observed identically.
pub async fn call(
    name: &str,
    agent: Arc<dyn TextAgent>,
    state: &State,
) -> Result<String, AgentError> {
    let result = agent.run(state).await;
    match &result {
        Ok(r) => {
            let key = result_key(name);
            let _ = state.set(
                format!("state_meta:{key}"),
                serde_json::json!({ "source": "agent", "resolver": name }),
            );
            let _ = state.set(key, r);
        }
        Err(e) => {
            let _ = state.set(error_key(name), e.to_string());
        }
    }
    result
}

/// The async source of a value, bound from `State`.
type FetchFn =
    Arc<dyn Fn(State) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

enum Source {
    /// Run a [`TextAgent`] (which reads its inputs from `State`).
    Agent(Arc<dyn TextAgent>),
    /// Run an async closure that reads `State` and returns a value — the seam
    /// for a tool call, an HTTP fetch, or an MCP request.
    Fetch(FetchFn),
    /// One-shot OOB LLM completion over a `State`-interpolated prompt.
    Llm {
        /// The out-of-band LLM.
        llm: Arc<dyn BaseLlm>,
        /// Prompt template; `{key}` interpolates the `State` value at `key`.
        prompt: String,
    },
}

/// Interpolate `{key}` placeholders in `template` with `State` string values.
fn interpolate(template: &str, state: &State) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let key = after[..close].trim();
        match state.get::<serde_json::Value>(key) {
            Some(serde_json::Value::String(s)) => out.push_str(&s),
            Some(v) => out.push_str(&v.to_string()),
            None => {}
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A named async value source whose inputs come from `State` and whose result
/// lands back in `State` under `{name}:result` (or `{name}:error`).
///
/// `Resolver` is the async sibling of the deterministic
/// [`Recognizer`](crate::extract::Recognizer): both are *inputs from State →
/// value*. A `Resolver`
/// generalizes [`call`] from "a sub-agent" to **any** async source — a sub-agent
/// ([`Resolver::agent`]) or a system fetch / tool call / MCP request
/// ([`Resolver::fetch`]) — under one result convention, so a `Flow` step can
/// complete on it via [`Guard::resolved`](crate::flow::Guard::resolved)
/// regardless of where the value came from.
pub struct Resolver {
    name: String,
    source: Source,
}

impl Resolver {
    /// Resolve by running a sub-agent. Its `String` output becomes the result.
    pub fn agent(name: impl Into<String>, agent: Arc<dyn TextAgent>) -> Self {
        Self {
            name: name.into(),
            source: Source::Agent(agent),
        }
    }

    /// Resolve by running an async closure over a clone of `State` — the seam
    /// for an HTTP fetch, a tool call, or an MCP request. The closure returns
    /// `Ok(value)` on success or `Err(message)` to record an error.
    pub fn fetch<F, Fut>(name: impl Into<String>, f: F) -> Self
    where
        F: Fn(State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let f = Arc::new(f);
        Self {
            name: name.into(),
            source: Source::Fetch(Arc::new(move |state| {
                let f = f.clone();
                Box::pin(async move { f(state).await })
            })),
        }
    }

    /// Resolve by running a one-shot OOB LLM over a `State`-interpolated prompt
    /// (`{key}` placeholders). The completion text becomes the result.
    pub fn llm(name: impl Into<String>, llm: Arc<dyn BaseLlm>, prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: Source::Llm {
                llm,
                prompt: prompt.into(),
            },
        }
    }

    /// The resolver's name (the `{name}:result` prefix it writes).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provenance kind of this resolver's source (`agent`/`fetch`/`llm`).
    fn source_kind(&self) -> &'static str {
        match &self.source {
            Source::Agent(_) => "agent",
            Source::Fetch(_) => "fetch",
            Source::Llm { .. } => "llm",
        }
    }

    /// Resolve **synchronously** ([`Mode::Call`]): await the source, write its
    /// value to `{name}:result` (or its error to `{name}:error`), record its
    /// provenance under `state_meta:{name}:result`, and return it.
    pub async fn resolve(&self, state: &State) -> Result<Value, String> {
        let outcome = match &self.source {
            Source::Agent(a) => a
                .run(state)
                .await
                .map(Value::from)
                .map_err(|e| e.to_string()),
            Source::Fetch(f) => f(state.clone()).await,
            Source::Llm { llm, prompt } => {
                let rendered = interpolate(prompt, state);
                llm.generate(LlmRequest::from_text(rendered))
                    .await
                    .map(|r| Value::from(r.text()))
                    .map_err(|e| e.to_string())
            }
        };
        match &outcome {
            Ok(v) => {
                let key = result_key(&self.name);
                let _ = state.set(
                    format!("state_meta:{key}"),
                    serde_json::json!({ "source": self.source_kind(), "resolver": self.name }),
                );
                let _ = state.set(key, v.clone());
            }
            Err(e) => {
                let _ = state.set(error_key(&self.name), e);
            }
        }
        outcome
    }

    /// Resolve **detached** ([`Mode::Dispatch`]): spawn the resolution on the
    /// runtime and return immediately. The conversation does not wait; consumers
    /// observe completion reactively via `{name}:result`.
    pub fn dispatch(self, state: State) {
        tokio::spawn(async move {
            let _ = self.resolve(&state).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    struct Echo(&'static str);
    #[async_trait]
    impl TextAgent for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Ok(self.0.to_string())
        }
    }

    struct Boom;
    #[async_trait]
    impl TextAgent for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Err(AgentError::Other("kaboom".into()))
        }
    }

    #[tokio::test]
    async fn call_writes_result_to_state() {
        let state = State::new();
        let out = call("verify", Arc::new(Echo("ok-123")), &state)
            .await
            .unwrap();
        assert_eq!(out, "ok-123");
        assert_eq!(
            state.get::<String>("verify:result").as_deref(),
            Some("ok-123")
        );
    }

    #[tokio::test]
    async fn call_writes_error_to_state() {
        let state = State::new();
        let r = call("verify", Arc::new(Boom), &state).await;
        assert!(r.is_err());
        assert!(state.contains("verify:error"));
        assert!(!state.contains("verify:result"));
    }

    #[tokio::test]
    async fn resolver_fetch_binds_state_and_writes_result() {
        let state = State::new();
        let _ = state.set("slot", "afternoon");
        let r = Resolver::fetch("availability", |s: State| async move {
            // Inputs come from State; the value is arbitrary JSON.
            let slot = s.get::<String>("slot").unwrap_or_default();
            Ok(json!({ "open": slot == "afternoon" }))
        });
        let out = r.resolve(&state).await.unwrap();
        assert_eq!(out, json!({ "open": true }));
        assert_eq!(
            state.get::<Value>("availability:result"),
            Some(json!({ "open": true }))
        );
        // Provenance is recorded for the resolved value.
        assert_eq!(
            provenance(&state, "availability:result").as_deref(),
            Some("fetch")
        );
    }

    #[tokio::test]
    async fn resolver_agent_uses_result_convention() {
        let state = State::new();
        // An agent resolver shares the `{name}:result` convention with `call`.
        Resolver::agent("verify", Arc::new(Echo("ok-9")))
            .resolve(&state)
            .await
            .unwrap();
        assert_eq!(
            state.get::<String>("verify:result").as_deref(),
            Some("ok-9")
        );
    }

    #[tokio::test]
    async fn resolver_fetch_records_error() {
        let state = State::new();
        let r = Resolver::fetch("lookup", |_s: State| async move {
            Err::<Value, String>("upstream 503".into())
        });
        assert!(r.resolve(&state).await.is_err());
        assert_eq!(
            state.get::<String>("lookup:error").as_deref(),
            Some("upstream 503")
        );
        assert!(!state.contains("lookup:result"));
    }

    #[tokio::test]
    async fn resolver_llm_interpolates_prompt_and_stores_text() {
        use crate::llm::{LlmError, LlmResponse};
        use gemini_genai_rs::prelude::Content;

        struct EchoLlm;
        #[async_trait]
        impl BaseLlm for EchoLlm {
            fn model_id(&self) -> &str {
                "echo"
            }
            async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
                // Echo the (interpolated) prompt back as the completion.
                let prompt = request.contents[0].parts.iter().find_map(|p| match p {
                    gemini_genai_rs::prelude::Part::Text { text } => Some(text.clone()),
                    _ => None,
                });
                Ok(LlmResponse {
                    content: Content::model(prompt.unwrap_or_default()),
                    finish_reason: None,
                    usage: None,
                })
            }
        }

        let state = State::new();
        let _ = state.set("topic", "billing");
        let out = Resolver::llm("summary", Arc::new(EchoLlm), "Summarize the {topic} issue")
            .resolve(&state)
            .await
            .unwrap();
        assert_eq!(out, json!("Summarize the billing issue"));
        assert_eq!(
            state.get::<String>("summary:result").as_deref(),
            Some("Summarize the billing issue")
        );
    }

    #[tokio::test]
    async fn resolver_dispatch_runs_detached() {
        let state = State::new();
        Resolver::fetch("ping", |_s: State| async move { Ok(json!("pong")) })
            .dispatch(state.clone());
        // The spawned task writes the result; await it becoming visible.
        for _ in 0..100 {
            if state.contains("ping:result") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(state.get::<String>("ping:result").as_deref(), Some("pong"));
    }
}
