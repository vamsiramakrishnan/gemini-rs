//! Context engineering for Gemini Live sessions.
//!
//! Inspired by ADK's `C` (Context) namespace, adapted for real-time streaming.
//! Unlike request-response agents, Gemini Live has continuous context flow:
//!
//! - Audio/text streams fill the context window continuously
//! - The server maintains conversation history automatically
//! - Context compression via [`SlidingWindow`](crate::protocol::SlidingWindow) prevents overflow
//! - Session resumption preserves context across reconnections
//!
//! This module provides three layers of context control:
//!
//! 1. **[`ContextPolicy`]** — declarative rules governing compression, memory, and budgets
//! 2. **[`ContextInjection`]** — dynamic injection of state/data as context at runtime
//! 3. **[`MemoryStrategy`]** — client-side turn history management and summarization
//!
//! # Example
//!
//! ```rust,no_run
//! use gemini_live_rs::context::*;
//!
//! let policy = ContextPolicy::builder()
//!     .compression_threshold(8000)
//!     .memory(MemoryStrategy::window(20))
//!     .inject_on_connect("customer_tier", "premium")
//!     .inject_template_every(5, "Queue depth: {queue_depth}")
//!     .budget(ContextBudget::new(16000).system(0.3).tools(0.2).conversation(0.5))
//!     .build();
//! ```

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::session::Turn;

// ---------------------------------------------------------------------------
// Context provider trait
// ---------------------------------------------------------------------------

type BoxContextFuture = Pin<Box<dyn Future<Output = Option<String>> + Send>>;

/// A dynamic context provider that produces context strings at runtime.
///
/// Implementations can query databases, call APIs, or compute context
/// from conversation state. Return `None` to skip injection.
pub trait ContextProvider: Send + Sync + 'static {
    /// Produce context to inject. Called at the trigger point.
    fn provide(&self, state: &ContextSnapshot) -> BoxContextFuture;
}

/// Snapshot of conversation context available to providers and transforms.
#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    /// Recent turn history (client-side tracking).
    pub turns: Vec<TurnSummary>,
    /// Current turn count.
    pub turn_count: u32,
    /// Key-value state bag.
    pub state: HashMap<String, serde_json::Value>,
    /// Session ID.
    pub session_id: String,
    /// Whether the session has been resumed.
    pub is_resumed: bool,
}

/// Lightweight summary of a conversation turn for context decisions.
#[derive(Debug, Clone)]
pub struct TurnSummary {
    /// Turn identifier.
    pub id: String,
    /// Role: "user" or "model".
    pub role: String,
    /// Preview of text content (first 200 chars).
    pub text_preview: String,
    /// Whether this turn included audio.
    pub has_audio: bool,
    /// Tool call names in this turn.
    pub tool_calls: Vec<String>,
    /// Whether this turn was interrupted.
    pub interrupted: bool,
}

impl TurnSummary {
    /// Create from a session [`Turn`].
    pub fn from_turn(turn: &Turn, role: &str) -> Self {
        let preview = if turn.text.len() > 200 {
            format!("{}...", &turn.text[..197])
        } else {
            turn.text.clone()
        };
        Self {
            id: turn.id.clone(),
            role: role.to_string(),
            text_preview: preview,
            has_audio: turn.has_audio,
            tool_calls: turn.tool_calls.iter().map(|tc| tc.name.clone()).collect(),
            interrupted: turn.interrupted,
        }
    }
}

// ---------------------------------------------------------------------------
// Memory strategy
// ---------------------------------------------------------------------------

/// How the client tracks and manages conversation memory.
///
/// The Gemini Live server maintains its own conversation history, but the
/// client can maintain a parallel view for:
/// - Context injection decisions
/// - State-driven prompt augmentation
/// - Analytics and metrics
/// - Session resumption with compressed context
#[derive(Debug, Clone, Default)]
pub enum MemoryStrategy {
    /// Keep all turns in client memory (default). Server manages its own window.
    #[default]
    Full,
    /// Keep only the last N turns in client memory.
    Window {
        /// Maximum number of turns to retain.
        max_turns: u32,
    },
    /// Periodically summarize older turns into a condensed form.
    Summarize {
        /// Summarize every N turns.
        every_n_turns: u32,
        /// Maximum tokens for the summary.
        max_summary_tokens: u32,
    },
}

impl MemoryStrategy {
    /// Keep all turns.
    pub fn full() -> Self {
        Self::Full
    }

    /// Sliding window of the last N turns.
    pub fn window(max_turns: u32) -> Self {
        Self::Window { max_turns }
    }

    /// Periodic summarization.
    pub fn summarize(every_n_turns: u32, max_summary_tokens: u32) -> Self {
        Self::Summarize {
            every_n_turns,
            max_summary_tokens,
        }
    }

    /// Apply this strategy to a turn history, returning the retained turns.
    pub fn apply(&self, turns: &[TurnSummary]) -> Vec<TurnSummary> {
        match self {
            Self::Full => turns.to_vec(),
            Self::Window { max_turns } => {
                let start = turns.len().saturating_sub(*max_turns as usize);
                turns[start..].to_vec()
            }
            Self::Summarize { every_n_turns, .. } => {
                // Keep a summary marker for old turns + retain recent ones
                let keep_from = turns.len().saturating_sub(*every_n_turns as usize);
                turns[keep_from..].to_vec()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Context injection
// ---------------------------------------------------------------------------

/// When to inject context into the conversation.
#[derive(Debug, Clone)]
pub enum InjectionTrigger {
    /// Inject once when the session connects.
    OnConnect,
    /// Inject before every model turn.
    BeforeTurn,
    /// Inject every N turns.
    EveryNTurns(u32),
    /// Inject when a state key changes value.
    OnStateChange(String),
}

impl fmt::Display for InjectionTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnConnect => write!(f, "OnConnect"),
            Self::BeforeTurn => write!(f, "BeforeTurn"),
            Self::EveryNTurns(n) => write!(f, "EveryNTurns({n})"),
            Self::OnStateChange(key) => write!(f, "OnStateChange({key})"),
        }
    }
}

/// A rule for injecting context into the conversation.
///
/// Context injections send additional information as `client_content` messages
/// to the Gemini API, supplementing the system instruction with runtime data.
#[derive(Clone)]
pub struct ContextInjection {
    /// Human-readable label for debugging.
    pub label: String,
    /// When to inject.
    pub trigger: InjectionTrigger,
    /// The injection source.
    pub source: InjectionSource,
}

impl fmt::Debug for ContextInjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextInjection")
            .field("label", &self.label)
            .field("trigger", &self.trigger)
            .field("source", &self.source.kind_name())
            .finish()
    }
}

/// Source of injected context content.
#[derive(Clone)]
pub enum InjectionSource {
    /// Static key-value pair injected as "Context: {key} = {value}".
    Static {
        key: String,
        value: String,
    },
    /// Template string with `{key}` placeholders resolved from state.
    Template(String),
    /// Dynamic provider function.
    Dynamic(Arc<dyn ContextProvider>),
}

impl InjectionSource {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Static { .. } => "Static",
            Self::Template(_) => "Template",
            Self::Dynamic(_) => "Dynamic",
        }
    }

    /// Resolve this source to a context string.
    pub async fn resolve(&self, snapshot: &ContextSnapshot) -> Option<String> {
        match self {
            Self::Static { key, value } => Some(format!("[Context] {key}: {value}")),
            Self::Template(template) => {
                let mut result = template.clone();
                for (k, v) in &snapshot.state {
                    let placeholder = format!("{{{k}}}");
                    let replacement = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    result = result.replace(&placeholder, &replacement);
                }
                // Check for unresolved placeholders — skip if any remain
                if result.contains('{') && result.contains('}') {
                    None // Unresolved placeholders, skip
                } else {
                    Some(format!("[Context] {result}"))
                }
            }
            Self::Dynamic(provider) => provider.provide(snapshot).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Context budget
// ---------------------------------------------------------------------------

/// Token budget allocation for context window management.
///
/// Helps the system decide what to prioritize when the context window
/// is filling up. Priority weights are normalized to sum to 1.0.
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Total token budget for the context window.
    pub max_tokens: u32,
    /// Priority weight for system instructions (0.0 - 1.0).
    pub system_priority: f32,
    /// Priority weight for tool declarations (0.0 - 1.0).
    pub tool_priority: f32,
    /// Priority weight for conversation history (0.0 - 1.0).
    pub conversation_priority: f32,
}

impl ContextBudget {
    /// Create a new budget with the given total token limit.
    pub fn new(max_tokens: u32) -> Self {
        Self {
            max_tokens,
            system_priority: 0.3,
            tool_priority: 0.2,
            conversation_priority: 0.5,
        }
    }

    /// Set system instruction priority weight.
    pub fn system(mut self, weight: f32) -> Self {
        self.system_priority = weight;
        self
    }

    /// Set tool declaration priority weight.
    pub fn tools(mut self, weight: f32) -> Self {
        self.tool_priority = weight;
        self
    }

    /// Set conversation history priority weight.
    pub fn conversation(mut self, weight: f32) -> Self {
        self.conversation_priority = weight;
        self
    }

    /// Compute the token allocation for each category.
    pub fn allocations(&self) -> ContextAllocations {
        let total = self.system_priority + self.tool_priority + self.conversation_priority;
        if total == 0.0 {
            return ContextAllocations {
                system_tokens: self.max_tokens / 3,
                tool_tokens: self.max_tokens / 3,
                conversation_tokens: self.max_tokens / 3,
            };
        }
        ContextAllocations {
            system_tokens: ((self.system_priority / total) * self.max_tokens as f32) as u32,
            tool_tokens: ((self.tool_priority / total) * self.max_tokens as f32) as u32,
            conversation_tokens: ((self.conversation_priority / total) * self.max_tokens as f32)
                as u32,
        }
    }
}

/// Computed token allocations for each context category.
#[derive(Debug, Clone)]
pub struct ContextAllocations {
    /// Tokens allocated for system instructions.
    pub system_tokens: u32,
    /// Tokens allocated for tool declarations.
    pub tool_tokens: u32,
    /// Tokens allocated for conversation history.
    pub conversation_tokens: u32,
}

// ---------------------------------------------------------------------------
// Context policy
// ---------------------------------------------------------------------------

/// Declarative policy governing how a Gemini Live session manages context.
///
/// Combines server-side compression (via `contextWindowCompression` in the setup
/// message), client-side memory management, and dynamic context injection.
///
/// # Example
///
/// ```rust
/// use gemini_live_rs::context::*;
///
/// let policy = ContextPolicy::builder()
///     .compression_threshold(8000)
///     .memory(MemoryStrategy::window(20))
///     .inject_on_connect("user_tier", "premium")
///     .build();
///
/// assert_eq!(policy.compression_threshold, Some(8000));
/// ```
#[derive(Clone)]
pub struct ContextPolicy {
    /// Target token count before server-side compression triggers.
    /// Maps to `contextWindowCompression.slidingWindow.targetTokens`.
    pub compression_threshold: Option<u32>,
    /// Client-side conversation memory strategy.
    pub memory_strategy: MemoryStrategy,
    /// Context injection rules.
    pub injections: Vec<ContextInjection>,
    /// Token budget allocation.
    pub budget: Option<ContextBudget>,
    /// Whether to enable session resumption for context preservation.
    pub enable_resumption: bool,
}

impl fmt::Debug for ContextPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextPolicy")
            .field("compression_threshold", &self.compression_threshold)
            .field("memory_strategy", &self.memory_strategy)
            .field("injections_count", &self.injections.len())
            .field("budget", &self.budget)
            .field("enable_resumption", &self.enable_resumption)
            .finish()
    }
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            compression_threshold: None,
            memory_strategy: MemoryStrategy::Full,
            injections: Vec::new(),
            budget: None,
            enable_resumption: false,
        }
    }
}

impl ContextPolicy {
    /// Create a builder for constructing a context policy.
    pub fn builder() -> ContextPolicyBuilder {
        ContextPolicyBuilder::default()
    }

    /// Get injections that should fire for the given trigger context.
    pub fn injections_for_trigger(
        &self,
        trigger: &InjectionTrigger,
        turn_count: u32,
    ) -> Vec<&ContextInjection> {
        self.injections
            .iter()
            .filter(|inj| match (&inj.trigger, trigger) {
                (InjectionTrigger::OnConnect, InjectionTrigger::OnConnect) => true,
                (InjectionTrigger::BeforeTurn, InjectionTrigger::BeforeTurn) => true,
                (InjectionTrigger::EveryNTurns(n), InjectionTrigger::EveryNTurns(_)) => {
                    turn_count > 0 && turn_count.is_multiple_of(*n)
                }
                (InjectionTrigger::OnStateChange(k1), InjectionTrigger::OnStateChange(k2)) => {
                    k1 == k2
                }
                _ => false,
            })
            .collect()
    }

    /// Apply the memory strategy to a set of turns.
    pub fn apply_memory(&self, turns: &[TurnSummary]) -> Vec<TurnSummary> {
        self.memory_strategy.apply(turns)
    }
}

// ---------------------------------------------------------------------------
// Context policy builder
// ---------------------------------------------------------------------------

/// Builder for [`ContextPolicy`].
#[derive(Default)]
pub struct ContextPolicyBuilder {
    compression_threshold: Option<u32>,
    memory_strategy: Option<MemoryStrategy>,
    injections: Vec<ContextInjection>,
    budget: Option<ContextBudget>,
    enable_resumption: bool,
}

impl ContextPolicyBuilder {
    /// Set the server-side compression threshold (target tokens for sliding window).
    pub fn compression_threshold(mut self, tokens: u32) -> Self {
        self.compression_threshold = Some(tokens);
        self
    }

    /// Set the client-side memory strategy.
    pub fn memory(mut self, strategy: MemoryStrategy) -> Self {
        self.memory_strategy = Some(strategy);
        self
    }

    /// Inject a static key-value pair when the session connects.
    pub fn inject_on_connect(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let key = key.into();
        let label = format!("static:{}", &key);
        self.injections.push(ContextInjection {
            label,
            trigger: InjectionTrigger::OnConnect,
            source: InjectionSource::Static {
                key,
                value: value.into(),
            },
        });
        self
    }

    /// Inject a template string every N turns.
    pub fn inject_template_every(mut self, n: u32, template: impl Into<String>) -> Self {
        let template = template.into();
        let label = format!("template:every_{n}");
        self.injections.push(ContextInjection {
            label,
            trigger: InjectionTrigger::EveryNTurns(n),
            source: InjectionSource::Template(template),
        });
        self
    }

    /// Inject a template string before every turn.
    pub fn inject_template_before_turn(mut self, template: impl Into<String>) -> Self {
        let template = template.into();
        self.injections.push(ContextInjection {
            label: "template:before_turn".to_string(),
            trigger: InjectionTrigger::BeforeTurn,
            source: InjectionSource::Template(template),
        });
        self
    }

    /// Inject context from a dynamic provider at the given trigger.
    pub fn inject_dynamic(
        mut self,
        label: impl Into<String>,
        trigger: InjectionTrigger,
        provider: impl ContextProvider,
    ) -> Self {
        self.injections.push(ContextInjection {
            label: label.into(),
            trigger,
            source: InjectionSource::Dynamic(Arc::new(provider)),
        });
        self
    }

    /// Add a custom injection rule.
    pub fn inject(mut self, injection: ContextInjection) -> Self {
        self.injections.push(injection);
        self
    }

    /// Set the token budget allocation.
    pub fn budget(mut self, budget: ContextBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Enable session resumption for context preservation across reconnects.
    pub fn enable_resumption(mut self) -> Self {
        self.enable_resumption = true;
        self
    }

    /// Build the context policy.
    pub fn build(self) -> ContextPolicy {
        ContextPolicy {
            compression_threshold: self.compression_threshold,
            memory_strategy: self.memory_strategy.unwrap_or_default(),
            injections: self.injections,
            budget: self.budget,
            enable_resumption: self.enable_resumption,
        }
    }
}

// ---------------------------------------------------------------------------
// Context manager (runtime)
// ---------------------------------------------------------------------------

/// Runtime context manager that tracks conversation memory and fires injections.
///
/// Created from a [`ContextPolicy`] and used internally by the agent's event router.
pub struct ContextManager {
    policy: ContextPolicy,
    turns: Vec<TurnSummary>,
    turn_count: u32,
    state: HashMap<String, serde_json::Value>,
    session_id: String,
    is_resumed: bool,
}

impl ContextManager {
    /// Create a new context manager from a policy.
    pub fn new(policy: ContextPolicy, session_id: String) -> Self {
        Self {
            policy,
            turns: Vec::new(),
            turn_count: 0,
            state: HashMap::new(),
            session_id,
            is_resumed: false,
        }
    }

    /// Record a completed turn.
    pub fn record_turn(&mut self, turn: &Turn, role: &str) {
        self.turns.push(TurnSummary::from_turn(turn, role));
        self.turn_count += 1;
        // Apply memory strategy
        self.turns = self.policy.apply_memory(&self.turns);
    }

    /// Update a state key.
    pub fn set_state(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.state.insert(key.into(), value);
    }

    /// Mark this session as resumed.
    pub fn mark_resumed(&mut self) {
        self.is_resumed = true;
    }

    /// Get a snapshot of current context.
    pub fn snapshot(&self) -> ContextSnapshot {
        ContextSnapshot {
            turns: self.turns.clone(),
            turn_count: self.turn_count,
            state: self.state.clone(),
            session_id: self.session_id.clone(),
            is_resumed: self.is_resumed,
        }
    }

    /// Resolve all injections for a given trigger, returning context strings.
    pub async fn resolve_injections(&self, trigger: &InjectionTrigger) -> Vec<String> {
        let snapshot = self.snapshot();
        let injections = self.policy.injections_for_trigger(trigger, self.turn_count);
        let mut results = Vec::new();
        for inj in injections {
            if let Some(text) = inj.source.resolve(&snapshot).await {
                results.push(text);
            }
        }
        results
    }

    /// Current turn count.
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Access the underlying policy.
    pub fn policy(&self) -> &ContextPolicy {
        &self.policy
    }

    /// Access retained turn summaries.
    pub fn turns(&self) -> &[TurnSummary] {
        &self.turns
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_strategy_full_keeps_all() {
        let turns: Vec<TurnSummary> = (0..10)
            .map(|i| TurnSummary {
                id: format!("turn-{i}"),
                role: "model".to_string(),
                text_preview: format!("Turn {i}"),
                has_audio: false,
                tool_calls: vec![],
                interrupted: false,
            })
            .collect();

        let result = MemoryStrategy::full().apply(&turns);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn memory_strategy_window_limits() {
        let turns: Vec<TurnSummary> = (0..10)
            .map(|i| TurnSummary {
                id: format!("turn-{i}"),
                role: "model".to_string(),
                text_preview: format!("Turn {i}"),
                has_audio: false,
                tool_calls: vec![],
                interrupted: false,
            })
            .collect();

        let result = MemoryStrategy::window(3).apply(&turns);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "turn-7");
        assert_eq!(result[2].id, "turn-9");
    }

    #[test]
    fn context_budget_allocations() {
        let budget = ContextBudget::new(10000).system(0.3).tools(0.2).conversation(0.5);
        let alloc = budget.allocations();
        assert_eq!(alloc.system_tokens, 3000);
        assert_eq!(alloc.tool_tokens, 2000);
        assert_eq!(alloc.conversation_tokens, 5000);
    }

    #[test]
    fn context_policy_builder() {
        let policy = ContextPolicy::builder()
            .compression_threshold(8000)
            .memory(MemoryStrategy::window(20))
            .inject_on_connect("tier", "premium")
            .enable_resumption()
            .build();

        assert_eq!(policy.compression_threshold, Some(8000));
        assert!(policy.enable_resumption);
        assert_eq!(policy.injections.len(), 1);
        assert_eq!(policy.injections[0].label, "static:tier");
    }

    #[test]
    fn injection_trigger_matching() {
        let policy = ContextPolicy::builder()
            .inject_on_connect("key", "val")
            .inject_template_every(5, "hello {name}")
            .build();

        // OnConnect trigger matches
        let matched = policy.injections_for_trigger(&InjectionTrigger::OnConnect, 0);
        assert_eq!(matched.len(), 1);

        // EveryNTurns at turn 5 matches
        let matched = policy.injections_for_trigger(&InjectionTrigger::EveryNTurns(0), 5);
        assert_eq!(matched.len(), 1);

        // EveryNTurns at turn 3 does not match
        let matched = policy.injections_for_trigger(&InjectionTrigger::EveryNTurns(0), 3);
        assert_eq!(matched.len(), 0);
    }

    #[tokio::test]
    async fn static_injection_resolves() {
        let source = InjectionSource::Static {
            key: "tier".to_string(),
            value: "gold".to_string(),
        };
        let snapshot = ContextSnapshot {
            turns: vec![],
            turn_count: 0,
            state: HashMap::new(),
            session_id: "test".to_string(),
            is_resumed: false,
        };
        let result = source.resolve(&snapshot).await;
        assert_eq!(result, Some("[Context] tier: gold".to_string()));
    }

    #[tokio::test]
    async fn template_injection_resolves() {
        let source = InjectionSource::Template("User tier: {tier}, queue: {depth}".to_string());
        let mut state = HashMap::new();
        state.insert("tier".to_string(), serde_json::json!("premium"));
        state.insert("depth".to_string(), serde_json::json!(5));
        let snapshot = ContextSnapshot {
            turns: vec![],
            turn_count: 0,
            state,
            session_id: "test".to_string(),
            is_resumed: false,
        };
        let result = source.resolve(&snapshot).await;
        assert_eq!(
            result,
            Some("[Context] User tier: premium, queue: 5".to_string())
        );
    }

    #[tokio::test]
    async fn template_with_unresolved_placeholders_returns_none() {
        let source = InjectionSource::Template("User: {unknown_key}".to_string());
        let snapshot = ContextSnapshot {
            turns: vec![],
            turn_count: 0,
            state: HashMap::new(),
            session_id: "test".to_string(),
            is_resumed: false,
        };
        let result = source.resolve(&snapshot).await;
        assert!(result.is_none());
    }

    #[test]
    fn context_manager_records_and_limits_turns() {
        let policy = ContextPolicy::builder()
            .memory(MemoryStrategy::window(3))
            .build();
        let mut mgr = ContextManager::new(policy, "session-1".to_string());

        for i in 0..5 {
            let turn = Turn {
                id: format!("t-{i}"),
                text: format!("text {i}"),
                has_audio: false,
                tool_calls: vec![],
                started_at: std::time::Instant::now(),
                completed_at: Some(std::time::Instant::now()),
                interrupted: false,
            };
            mgr.record_turn(&turn, "model");
        }

        assert_eq!(mgr.turn_count(), 5);
        assert_eq!(mgr.turns().len(), 3);
        assert_eq!(mgr.turns()[0].id, "t-2");
    }

    #[test]
    fn context_manager_state() {
        let policy = ContextPolicy::default();
        let mut mgr = ContextManager::new(policy, "s1".to_string());
        mgr.set_state("key", serde_json::json!("value"));
        let snap = mgr.snapshot();
        assert_eq!(snap.state.get("key").unwrap(), &serde_json::json!("value"));
    }
}
