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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rs_genai::prelude::{Content, FunctionCall, FunctionResponse, Part, Role};

use crate::error::AgentError;
use crate::llm::{BaseLlm, LlmRequest};
use crate::state::State;
use crate::tool::ToolDispatcher;

/// Maximum number of tool-dispatch round-trips before giving up.
const MAX_TOOL_ROUNDS: usize = 10;

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

// ── LlmTextAgent ──────────────────────────────────────────────────────────

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
    pub fn new(
        name: impl Into<String>,
        llm: Arc<dyn BaseLlm>,
    ) -> Self {
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
    async fn dispatch_tools(
        &self,
        calls: &[FunctionCall],
    ) -> Vec<FunctionResponse> {
        let dispatcher = match &self.dispatcher {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut responses = Vec::with_capacity(calls.len());
        for call in calls {
            let result = dispatcher.call_function(&call.name, call.args.clone()).await;
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
        let input = state
            .get::<String>("input")
            .unwrap_or_default();

        let mut contents = vec![Content::user(&input)];

        for _round in 0..MAX_TOOL_ROUNDS {
            let request = self.build_request(contents.clone());
            let response = self
                .llm
                .generate(request)
                .await
                .map_err(|e| AgentError::Other(format!("LLM error: {e}")))?;

            let calls: Vec<FunctionCall> = response
                .function_calls()
                .into_iter()
                .cloned()
                .collect();

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

// ── FnTextAgent ───────────────────────────────────────────────────────────

/// Zero-cost state transform agent — executes a closure, no LLM call.
pub struct FnTextAgent {
    name: String,
    #[allow(clippy::type_complexity)]
    func: Box<dyn Fn(&State) -> Result<String, AgentError> + Send + Sync>,
}

impl FnTextAgent {
    /// Create a new function agent.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&State) -> Result<String, AgentError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            func: Box::new(f),
        }
    }
}

#[async_trait]
impl TextAgent for FnTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        (self.func)(state)
    }
}

// ── SequentialTextAgent ───────────────────────────────────────────────────

/// Runs text agents sequentially. Each agent sees state mutations from
/// previous agents. The final agent's output is the pipeline's output.
pub struct SequentialTextAgent {
    name: String,
    children: Vec<Arc<dyn TextAgent>>,
}

impl SequentialTextAgent {
    /// Create a new sequential agent that runs children in order.
    pub fn new(name: impl Into<String>, children: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            children,
        }
    }
}

#[async_trait]
impl TextAgent for SequentialTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_output = String::new();
        for child in &self.children {
            last_output = child.run(state).await?;
            // Feed output as input for the next agent.
            state.set("input", &last_output);
        }
        Ok(last_output)
    }
}

// ── ParallelTextAgent ─────────────────────────────────────────────────────

/// Runs text agents concurrently. All branches share state. Results are
/// collected and joined with newlines.
pub struct ParallelTextAgent {
    name: String,
    branches: Vec<Arc<dyn TextAgent>>,
}

impl ParallelTextAgent {
    /// Create a new parallel agent that runs branches concurrently.
    pub fn new(name: impl Into<String>, branches: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            branches,
        }
    }
}

#[async_trait]
impl TextAgent for ParallelTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut handles = Vec::with_capacity(self.branches.len());

        for branch in &self.branches {
            let branch = branch.clone();
            let state = state.clone();
            handles.push(tokio::spawn(async move { branch.run(&state).await }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            let result = handle
                .await
                .map_err(|e| AgentError::Other(format!("Join error: {e}")))?;
            results.push(result?);
        }

        let combined = results.join("\n");
        state.set("output", &combined);
        Ok(combined)
    }
}

// ── LoopTextAgent ─────────────────────────────────────────────────────────

/// Runs a text agent repeatedly until max iterations or a state predicate.
pub struct LoopTextAgent {
    name: String,
    body: Arc<dyn TextAgent>,
    max: u32,
    until: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
}

impl LoopTextAgent {
    /// Create a new loop agent that repeats up to `max` iterations.
    pub fn new(name: impl Into<String>, body: Arc<dyn TextAgent>, max: u32) -> Self {
        Self {
            name: name.into(),
            body,
            max,
            until: None,
        }
    }

    /// Add a predicate — loop breaks when predicate returns true.
    pub fn until(
        mut self,
        pred: impl Fn(&State) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.until = Some(Arc::new(pred));
        self
    }
}

#[async_trait]
impl TextAgent for LoopTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_output = String::new();

        for _iter in 0..self.max {
            last_output = self.body.run(state).await?;

            if let Some(pred) = &self.until {
                if pred(state) {
                    break;
                }
            }
        }

        Ok(last_output)
    }
}

// ── FallbackTextAgent ─────────────────────────────────────────────────────

/// Tries each child agent in sequence. Returns the first successful result.
/// If all fail, returns the last error.
pub struct FallbackTextAgent {
    name: String,
    candidates: Vec<Arc<dyn TextAgent>>,
}

impl FallbackTextAgent {
    /// Create a new fallback agent that tries candidates in order.
    pub fn new(name: impl Into<String>, candidates: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            candidates,
        }
    }
}

#[async_trait]
impl TextAgent for FallbackTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut last_err = AgentError::Other("No candidates in fallback".into());

        for candidate in &self.candidates {
            match candidate.run(state).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }
}

// ── RouteTextAgent ────────────────────────────────────────────────────────

/// A routing rule: predicate over state → target agent.
pub struct RouteRule {
    predicate: Box<dyn Fn(&State) -> bool + Send + Sync>,
    agent: Arc<dyn TextAgent>,
}

impl RouteRule {
    /// Create a new route rule with a predicate and target agent.
    pub fn new(
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        agent: Arc<dyn TextAgent>,
    ) -> Self {
        Self {
            predicate: Box::new(predicate),
            agent,
        }
    }
}

/// State-driven deterministic branching — evaluates predicates in order,
/// dispatches to the first matching agent. Falls back to default if none match.
pub struct RouteTextAgent {
    name: String,
    rules: Vec<RouteRule>,
    default: Arc<dyn TextAgent>,
}

impl RouteTextAgent {
    /// Create a new route agent with rules and a default fallback.
    pub fn new(
        name: impl Into<String>,
        rules: Vec<RouteRule>,
        default: Arc<dyn TextAgent>,
    ) -> Self {
        Self {
            name: name.into(),
            rules,
            default,
        }
    }
}

#[async_trait]
impl TextAgent for RouteTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        for rule in &self.rules {
            if (rule.predicate)(state) {
                return rule.agent.run(state).await;
            }
        }
        self.default.run(state).await
    }
}

// ── RaceTextAgent ─────────────────────────────────────────────────────────

/// Runs agents concurrently, returns the first to complete. Cancels the rest.
pub struct RaceTextAgent {
    name: String,
    agents: Vec<Arc<dyn TextAgent>>,
}

impl RaceTextAgent {
    /// Create a new race agent that runs agents concurrently and returns the first result.
    pub fn new(name: impl Into<String>, agents: Vec<Arc<dyn TextAgent>>) -> Self {
        Self {
            name: name.into(),
            agents,
        }
    }
}

#[async_trait]
impl TextAgent for RaceTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        if self.agents.is_empty() {
            return Err(AgentError::Other("No agents in race".into()));
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, AgentError>>(1);
        let cancel = tokio_util::sync::CancellationToken::new();

        let mut handles = Vec::with_capacity(self.agents.len());
        for agent in &self.agents {
            let agent = agent.clone();
            let state = state.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();

            handles.push(tokio::spawn(async move {
                tokio::select! {
                    result = agent.run(&state) => {
                        let _ = tx.send(result).await;
                    }
                    _ = cancel.cancelled() => {}
                }
            }));
        }
        drop(tx); // Close our sender so rx completes when all are done.

        let result = rx.recv().await.unwrap_or(Err(AgentError::Other(
            "All race agents failed".into(),
        )));

        // Cancel remaining agents.
        cancel.cancel();
        for handle in handles {
            handle.abort();
        }

        result
    }
}

// ── TimeoutTextAgent ──────────────────────────────────────────────────────

/// Wraps an agent with a time limit. Returns `AgentError::Timeout` if exceeded.
pub struct TimeoutTextAgent {
    name: String,
    inner: Arc<dyn TextAgent>,
    timeout: Duration,
}

impl TimeoutTextAgent {
    /// Create a new timeout agent wrapping an inner agent with a time limit.
    pub fn new(
        name: impl Into<String>,
        inner: Arc<dyn TextAgent>,
        timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            inner,
            timeout,
        }
    }
}

#[async_trait]
impl TextAgent for TimeoutTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        match tokio::time::timeout(self.timeout, self.inner.run(state)).await {
            Ok(result) => result,
            Err(_) => Err(AgentError::Timeout),
        }
    }
}

// ── MapOverTextAgent ──────────────────────────────────────────────────────

/// Iterates a single agent over each item in a state list.
/// Reads `state[list_key]`, runs agent per item (setting `state[item_key]`),
/// collects results into `state[output_key]`.
pub struct MapOverTextAgent {
    name: String,
    agent: Arc<dyn TextAgent>,
    list_key: String,
    item_key: String,
    output_key: String,
}

impl MapOverTextAgent {
    /// Create a new map-over agent that iterates over a list in state.
    pub fn new(
        name: impl Into<String>,
        agent: Arc<dyn TextAgent>,
        list_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            agent,
            list_key: list_key.into(),
            item_key: "_item".into(),
            output_key: "_results".into(),
        }
    }

    /// Set the state key for the current item (default: "_item").
    pub fn item_key(mut self, key: impl Into<String>) -> Self {
        self.item_key = key.into();
        self
    }

    /// Set the state key for the output list (default: "_results").
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = key.into();
        self
    }
}

#[async_trait]
impl TextAgent for MapOverTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let items: Vec<serde_json::Value> = state
            .get(&self.list_key)
            .unwrap_or_default();

        let mut results = Vec::with_capacity(items.len());

        for item in &items {
            state.set(&self.item_key, item);
            state.set("input", item.to_string());
            let result = self.agent.run(state).await?;
            results.push(result);
        }

        state.set(&self.output_key, &results);
        Ok(results.join("\n"))
    }
}

// ── TapTextAgent ──────────────────────────────────────────────────────────

/// Read-only observation agent. Calls a function with the state but
/// cannot mutate it. Returns empty string. No LLM call.
pub struct TapTextAgent {
    name: String,
    func: Box<dyn Fn(&State) + Send + Sync>,
}

impl TapTextAgent {
    /// Create a new tap agent for read-only observation.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&State) + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            func: Box::new(f),
        }
    }
}

#[async_trait]
impl TextAgent for TapTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        (self.func)(state);
        Ok(String::new())
    }
}

// ── DispatchTextAgent ─────────────────────────────────────────────────────

/// Shared registry for dispatched background tasks.
#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<tokio::sync::Mutex<HashMap<String, tokio::task::JoinHandle<Result<String, String>>>>>,
}

impl TaskRegistry {
    /// Create a new empty task registry.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Fire-and-forget background task launcher with global task budget.
///
/// Launches each child agent as a background `tokio::spawn` task,
/// stores handles in a `TaskRegistry`, and returns immediately.
pub struct DispatchTextAgent {
    name: String,
    children: Vec<(String, Arc<dyn TextAgent>)>,
    registry: TaskRegistry,
    budget: Arc<tokio::sync::Semaphore>,
}

impl DispatchTextAgent {
    /// Create a new dispatch agent with named children and a concurrency budget.
    pub fn new(
        name: impl Into<String>,
        children: Vec<(String, Arc<dyn TextAgent>)>,
        registry: TaskRegistry,
        budget: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            name: name.into(),
            children,
            registry,
            budget,
        }
    }
}

#[async_trait]
impl TextAgent for DispatchTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut registry = self.registry.inner.lock().await;

        for (task_name, agent) in &self.children {
            let agent = agent.clone();
            let state = state.clone();
            let budget = self.budget.clone();
            let task_name_owned = task_name.clone();

            let handle = tokio::spawn(async move {
                let _permit = budget
                    .acquire()
                    .await
                    .map_err(|e| format!("Semaphore closed: {e}"))?;
                agent
                    .run(&state)
                    .await
                    .map_err(|e| format!("Task '{}' failed: {}", task_name_owned, e))
            });

            registry.insert(task_name.clone(), handle);
        }

        state.set(
            "_dispatch_status",
            self.children
                .iter()
                .map(|(name, _)| (name.clone(), "running".to_string()))
                .collect::<HashMap<String, String>>(),
        );

        Ok(String::new())
    }
}

// ── JoinTextAgent ─────────────────────────────────────────────────────────

/// Waits for dispatched background tasks and collects their results.
pub struct JoinTextAgent {
    name: String,
    registry: TaskRegistry,
    target_names: Option<Vec<String>>,
    timeout: Option<Duration>,
}

impl JoinTextAgent {
    /// Create a new join agent that waits for dispatched tasks.
    pub fn new(name: impl Into<String>, registry: TaskRegistry) -> Self {
        Self {
            name: name.into(),
            registry,
            target_names: None,
            timeout: None,
        }
    }

    /// Only wait for specific named tasks.
    pub fn targets(mut self, names: Vec<String>) -> Self {
        self.target_names = Some(names);
        self
    }

    /// Set a timeout for waiting.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[async_trait]
impl TextAgent for JoinTextAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, state: &State) -> Result<String, AgentError> {
        let mut registry = self.registry.inner.lock().await;

        // Select tasks to wait for.
        let tasks: HashMap<String, _> = if let Some(targets) = &self.target_names {
            targets
                .iter()
                .filter_map(|name| registry.remove(name).map(|h| (name.clone(), h)))
                .collect()
        } else {
            std::mem::take(&mut *registry)
        };
        drop(registry);

        let mut results = Vec::new();

        for (task_name, handle) in tasks {
            let result = if let Some(timeout) = self.timeout {
                match tokio::time::timeout(timeout, handle).await {
                    Ok(Ok(Ok(text))) => {
                        state.set(format!("_result_{}", task_name), &text);
                        Ok(text)
                    }
                    Ok(Ok(Err(e))) => Err(AgentError::Other(e)),
                    Ok(Err(e)) => Err(AgentError::Other(format!("Join error: {e}"))),
                    Err(_) => Err(AgentError::Timeout),
                }
            } else {
                match handle.await {
                    Ok(Ok(text)) => {
                        state.set(format!("_result_{}", task_name), &text);
                        Ok(text)
                    }
                    Ok(Err(e)) => Err(AgentError::Other(e)),
                    Err(e) => Err(AgentError::Other(format!("Join error: {e}"))),
                }
            };

            results.push(result?);
        }

        let combined = results.join("\n");
        state.set("output", &combined);
        Ok(combined)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmError, LlmResponse};

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

        let mut dispatcher = ToolDispatcher::new();
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
