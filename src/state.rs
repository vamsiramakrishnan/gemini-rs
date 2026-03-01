//! State engineering for Gemini Live sessions.
//!
//! Inspired by ADK's `S` (State) namespace and `StateSchema`. Provides
//! scoped, typed conversation state with composable transforms and guards.
//!
//! # Architecture
//!
//! Unlike request-response agents where state changes between discrete calls,
//! Gemini Live state flows continuously with events. This module provides:
//!
//! 1. **[`ConversationState`]** — scoped key-value state bag (session, app, user, temp)
//! 2. **[`StateTransform`]** — composable state mutations triggered by events
//! 3. **[`StatePolicy`]** — declarative rules for event-driven state management
//! 4. **[`StateGuard`]** — invariant checks at specified lifecycle points
//!
//! # Scoping
//!
//! State keys are scoped via prefixes, following ADK conventions:
//!
//! | Prefix | Scope | Persistence |
//! |--------|-------|-------------|
//! | (none) | Current session | Session lifetime |
//! | `app:` | All sessions | Application-wide |
//! | `user:` | Per-user | Across sessions |
//! | `temp:` | Current execution | Discarded on disconnect |
//!
//! # Example
//!
//! ```rust
//! use gemini_live_rs::state::*;
//!
//! let mut state = ConversationState::new();
//! state.set("intent", serde_json::json!("billing"));
//! state.set_scoped(StateScope::User, "tier", serde_json::json!("gold"));
//!
//! assert_eq!(state.get::<String>("intent"), Some("billing".to_string()));
//! assert_eq!(
//!     state.get_scoped::<String>(StateScope::User, "tier"),
//!     Some("gold".to_string())
//! );
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::session::SessionEvent;

// Type alias to reduce complexity in StateGuard
type GuardPredicate = Arc<dyn Fn(&ConversationState) -> Result<(), String> + Send + Sync>;

// ---------------------------------------------------------------------------
// State scopes
// ---------------------------------------------------------------------------

/// Scope tiers for state keys, following ADK conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateScope {
    /// Session-local state (default, no prefix).
    Session,
    /// Application-wide state (`app:` prefix).
    App,
    /// Per-user state persisted across sessions (`user:` prefix).
    User,
    /// Temporary scratch state discarded on disconnect (`temp:` prefix).
    Temp,
}

impl StateScope {
    /// Key prefix for this scope.
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Session => "",
            Self::App => "app:",
            Self::User => "user:",
            Self::Temp => "temp:",
        }
    }

    /// Determine the scope of a fully-qualified key.
    pub fn from_key(key: &str) -> (Self, &str) {
        if let Some(rest) = key.strip_prefix("app:") {
            (Self::App, rest)
        } else if let Some(rest) = key.strip_prefix("user:") {
            (Self::User, rest)
        } else if let Some(rest) = key.strip_prefix("temp:") {
            (Self::Temp, rest)
        } else {
            (Self::Session, key)
        }
    }

    /// Build a fully-qualified key with this scope's prefix.
    pub fn qualify(&self, key: &str) -> String {
        format!("{}{}", self.prefix(), key)
    }
}

impl fmt::Display for StateScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session => write!(f, "session"),
            Self::App => write!(f, "app"),
            Self::User => write!(f, "user"),
            Self::Temp => write!(f, "temp"),
        }
    }
}

// ---------------------------------------------------------------------------
// Conversation state
// ---------------------------------------------------------------------------

/// Scoped key-value state bag for conversation-level data.
///
/// Provides typed access with scope isolation. Session state lives for one
/// connection, temp state is discarded on disconnect, and app/user state
/// can be persisted across sessions.
#[derive(Debug, Clone, Default)]
pub struct ConversationState {
    /// All state stored as scope-prefixed keys.
    data: HashMap<String, serde_json::Value>,
}

impl ConversationState {
    /// Create an empty state bag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a session-scoped key.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.data.insert(key.into(), value);
    }

    /// Set a key in a specific scope.
    pub fn set_scoped(
        &mut self,
        scope: StateScope,
        key: impl AsRef<str>,
        value: serde_json::Value,
    ) {
        self.data.insert(scope.qualify(key.as_ref()), value);
    }

    /// Get a session-scoped value, deserializing to the requested type.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.data
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Get a value from a specific scope.
    pub fn get_scoped<T: serde::de::DeserializeOwned>(
        &self,
        scope: StateScope,
        key: &str,
    ) -> Option<T> {
        let qualified = scope.qualify(key);
        self.data
            .get(&qualified)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Get a raw JSON value.
    pub fn get_raw(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }

    /// Get a raw JSON value from a specific scope.
    pub fn get_raw_scoped(&self, scope: StateScope, key: &str) -> Option<&serde_json::Value> {
        let qualified = scope.qualify(key);
        self.data.get(&qualified)
    }

    /// Remove a key, returning its value if present.
    pub fn remove(&mut self, key: &str) -> Option<serde_json::Value> {
        self.data.remove(key)
    }

    /// Remove a scoped key.
    pub fn remove_scoped(
        &mut self,
        scope: StateScope,
        key: &str,
    ) -> Option<serde_json::Value> {
        let qualified = scope.qualify(key);
        self.data.remove(&qualified)
    }

    /// Check if a key exists (any scope).
    pub fn contains(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Get all keys for a given scope.
    pub fn keys_for_scope(&self, scope: StateScope) -> Vec<String> {
        let prefix = scope.prefix();
        self.data
            .keys()
            .filter(|k| {
                if prefix.is_empty() {
                    // Session scope: keys without any scope prefix
                    !k.starts_with("app:")
                        && !k.starts_with("user:")
                        && !k.starts_with("temp:")
                } else {
                    k.starts_with(prefix)
                }
            })
            .cloned()
            .collect()
    }

    /// Clear all temporary state.
    pub fn clear_temp(&mut self) {
        self.data.retain(|k, _| !k.starts_with("temp:"));
    }

    /// Clear all session-scoped state (no prefix).
    pub fn clear_session(&mut self) {
        self.data.retain(|k, _| {
            k.starts_with("app:") || k.starts_with("user:") || k.starts_with("temp:")
        });
    }

    /// Export all state as a flat HashMap (for serialization/persistence).
    pub fn export(&self) -> &HashMap<String, serde_json::Value> {
        &self.data
    }

    /// Import state from a flat HashMap (for restoration).
    pub fn import(&mut self, data: HashMap<String, serde_json::Value>) {
        self.data.extend(data);
    }

    /// Merge another state into this one (other values win on conflict).
    pub fn merge(&mut self, other: &ConversationState) {
        for (k, v) in &other.data {
            self.data.insert(k.clone(), v.clone());
        }
    }

    /// Total number of keys across all scopes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the state bag is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Access the full data map (for context injection and template resolution).
    pub fn as_map(&self) -> &HashMap<String, serde_json::Value> {
        &self.data
    }
}

// ---------------------------------------------------------------------------
// State transforms
// ---------------------------------------------------------------------------

/// A composable state mutation.
///
/// Transforms can be chained to build complex state update pipelines
/// triggered by session events.
#[derive(Clone)]
pub enum StateTransform {
    /// Set a key to a fixed value.
    Set {
        key: String,
        value: serde_json::Value,
    },
    /// Remove one or more keys.
    Drop(Vec<String>),
    /// Rename a key (old_key → new_key).
    Rename {
        from: String,
        to: String,
    },
    /// Increment a numeric key by a delta.
    Increment {
        key: String,
        delta: i64,
    },
    /// Apply a function to transform a single value.
    Map {
        key: String,
        func: Arc<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>,
    },
    /// Compute a new value from the full state.
    Compute {
        key: String,
        func: Arc<dyn Fn(&ConversationState) -> serde_json::Value + Send + Sync>,
    },
    /// Chain multiple transforms (applied in order).
    Chain(Vec<StateTransform>),
}

impl fmt::Debug for StateTransform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set { key, value } => write!(f, "Set({key}, {value})"),
            Self::Drop(keys) => write!(f, "Drop({keys:?})"),
            Self::Rename { from, to } => write!(f, "Rename({from} → {to})"),
            Self::Increment { key, delta } => write!(f, "Increment({key}, {delta})"),
            Self::Map { key, .. } => write!(f, "Map({key}, <fn>)"),
            Self::Compute { key, .. } => write!(f, "Compute({key}, <fn>)"),
            Self::Chain(transforms) => write!(f, "Chain({transforms:?})"),
        }
    }
}

impl StateTransform {
    /// Set a key to a value.
    pub fn set(key: impl Into<String>, value: serde_json::Value) -> Self {
        Self::Set {
            key: key.into(),
            value,
        }
    }

    /// Drop one or more keys.
    pub fn drop_keys(keys: Vec<String>) -> Self {
        Self::Drop(keys)
    }

    /// Rename a key.
    pub fn rename(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::Rename {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Increment a numeric key.
    pub fn increment(key: impl Into<String>, delta: i64) -> Self {
        Self::Increment {
            key: key.into(),
            delta,
        }
    }

    /// Apply a function to transform a value.
    pub fn map<F>(key: impl Into<String>, func: F) -> Self
    where
        F: Fn(serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    {
        Self::Map {
            key: key.into(),
            func: Arc::new(func),
        }
    }

    /// Compute a new value from the full state.
    pub fn compute<F>(key: impl Into<String>, func: F) -> Self
    where
        F: Fn(&ConversationState) -> serde_json::Value + Send + Sync + 'static,
    {
        Self::Compute {
            key: key.into(),
            func: Arc::new(func),
        }
    }

    /// Chain multiple transforms.
    pub fn chain(transforms: Vec<StateTransform>) -> Self {
        Self::Chain(transforms)
    }

    /// Apply this transform to a state bag.
    pub fn apply(&self, state: &mut ConversationState) {
        match self {
            Self::Set { key, value } => {
                state.set(key.clone(), value.clone());
            }
            Self::Drop(keys) => {
                for key in keys {
                    state.remove(key);
                }
            }
            Self::Rename { from, to } => {
                if let Some(value) = state.remove(from) {
                    state.set(to.clone(), value);
                }
            }
            Self::Increment { key, delta } => {
                let current = state
                    .get::<i64>(key)
                    .unwrap_or(0);
                state.set(key.clone(), serde_json::json!(current + delta));
            }
            Self::Map { key, func } => {
                if let Some(value) = state.remove(key) {
                    state.set(key.clone(), func(value));
                }
            }
            Self::Compute { key, func } => {
                let value = func(state);
                state.set(key.clone(), value);
            }
            Self::Chain(transforms) => {
                for t in transforms {
                    t.apply(state);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State guard
// ---------------------------------------------------------------------------

/// A guard that validates state invariants.
///
/// Guards are checked at specified lifecycle points and can block operations
/// (like tool execution) if preconditions aren't met.
#[derive(Clone)]
pub struct StateGuard {
    /// Human-readable name for error messages.
    pub name: String,
    /// The guard predicate.
    predicate: GuardPredicate,
}

impl StateGuard {
    /// Create a new guard with a predicate function.
    pub fn new<F>(name: impl Into<String>, predicate: F) -> Self
    where
        F: Fn(&ConversationState) -> Result<(), String> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            predicate: Arc::new(predicate),
        }
    }

    /// Check this guard against the state. Returns Ok(()) or an error message.
    pub fn check(&self, state: &ConversationState) -> Result<(), String> {
        (self.predicate)(state)
    }
}

impl fmt::Debug for StateGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StateGuard({})", self.name)
    }
}

// ---------------------------------------------------------------------------
// Event trigger
// ---------------------------------------------------------------------------

/// When to trigger a state transform or guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTrigger {
    /// After the session connects.
    OnConnect,
    /// After a model turn completes.
    OnTurnComplete,
    /// When the model is interrupted.
    OnInterrupted,
    /// When a tool call is received.
    OnToolCall,
    /// When a tool response is sent.
    OnToolResponse,
    /// When input transcription is received.
    OnInputTranscription,
    /// When the session disconnects.
    OnDisconnect,
    /// On any error event.
    OnError,
}

impl EventTrigger {
    /// Check if this trigger matches a session event.
    pub fn matches(&self, event: &SessionEvent) -> bool {
        matches!(
            (self, event),
            (EventTrigger::OnConnect, SessionEvent::Connected)
                | (EventTrigger::OnTurnComplete, SessionEvent::TurnComplete)
                | (EventTrigger::OnInterrupted, SessionEvent::Interrupted)
                | (EventTrigger::OnToolCall, SessionEvent::ToolCall(_))
                | (
                    EventTrigger::OnInputTranscription,
                    SessionEvent::InputTranscription(_)
                )
                | (EventTrigger::OnDisconnect, SessionEvent::Disconnected(_))
                | (EventTrigger::OnError, SessionEvent::Error(_))
        )
    }
}

/// Where to check a guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardPoint {
    /// Check before tool execution.
    BeforeTool(Option<String>),
    /// Check before sending text.
    BeforeSendText,
    /// Check on each turn completion.
    OnTurnComplete,
}

// ---------------------------------------------------------------------------
// State policy
// ---------------------------------------------------------------------------

/// Declarative policy for event-driven state management.
///
/// A state policy binds transforms and guards to event triggers,
/// creating an automatic state management pipeline.
///
/// # Example
///
/// ```rust
/// use gemini_live_rs::state::*;
///
/// let policy = StatePolicy::builder()
///     .on_connect(StateTransform::set("status", serde_json::json!("active")))
///     .on_turn_complete(StateTransform::increment("turn_count", 1))
///     .on_interrupted(StateTransform::increment("interruption_count", 1))
///     .guard_before_tool(
///         None,
///         StateGuard::new("identity_check", |state| {
///             if state.get::<bool>("identity_verified").unwrap_or(false) {
///                 Ok(())
///             } else {
///                 Err("Customer identity not verified".to_string())
///             }
///         }),
///     )
///     .build();
/// ```
#[derive(Clone, Default)]
pub struct StatePolicy {
    /// State transforms triggered by events.
    pub event_transforms: Vec<(EventTrigger, StateTransform)>,
    /// Guards checked at specified points.
    pub guards: Vec<(GuardPoint, StateGuard)>,
    /// Initial state values set on connect.
    pub initial_state: Vec<(String, serde_json::Value)>,
}

impl fmt::Debug for StatePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatePolicy")
            .field("event_transforms", &self.event_transforms.len())
            .field("guards", &self.guards.len())
            .field("initial_state", &self.initial_state.len())
            .finish()
    }
}

impl StatePolicy {
    /// Create a builder.
    pub fn builder() -> StatePolicyBuilder {
        StatePolicyBuilder::default()
    }

    /// Get transforms that should fire for a given event.
    pub fn transforms_for_event(&self, event: &SessionEvent) -> Vec<&StateTransform> {
        self.event_transforms
            .iter()
            .filter(|(trigger, _)| trigger.matches(event))
            .map(|(_, transform)| transform)
            .collect()
    }

    /// Get guards for a given guard point.
    pub fn guards_for_point(&self, point: &GuardPoint) -> Vec<&StateGuard> {
        self.guards
            .iter()
            .filter(|(p, _)| p == point)
            .map(|(_, guard)| guard)
            .collect()
    }

    /// Check all guards for a given point. Returns Ok(()) or first error.
    pub fn check_guards(
        &self,
        point: &GuardPoint,
        state: &ConversationState,
    ) -> Result<(), String> {
        for guard in self.guards_for_point(point) {
            guard.check(state)?;
        }
        Ok(())
    }

    /// Apply all transforms that match a given event.
    pub fn apply_event_transforms(
        &self,
        event: &SessionEvent,
        state: &mut ConversationState,
    ) {
        for transform in self.transforms_for_event(event) {
            transform.apply(state);
        }
    }

    /// Initialize state with initial values.
    pub fn initialize_state(&self, state: &mut ConversationState) {
        for (key, value) in &self.initial_state {
            state.set(key.clone(), value.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// State policy builder
// ---------------------------------------------------------------------------

/// Builder for [`StatePolicy`].
#[derive(Default)]
pub struct StatePolicyBuilder {
    event_transforms: Vec<(EventTrigger, StateTransform)>,
    guards: Vec<(GuardPoint, StateGuard)>,
    initial_state: Vec<(String, serde_json::Value)>,
}

impl StatePolicyBuilder {
    /// Add a transform triggered on connect.
    pub fn on_connect(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnConnect, transform));
        self
    }

    /// Add a transform triggered on turn completion.
    pub fn on_turn_complete(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnTurnComplete, transform));
        self
    }

    /// Add a transform triggered on interruption.
    pub fn on_interrupted(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnInterrupted, transform));
        self
    }

    /// Add a transform triggered on tool call receipt.
    pub fn on_tool_call(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnToolCall, transform));
        self
    }

    /// Add a transform triggered on input transcription.
    pub fn on_input_transcription(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnInputTranscription, transform));
        self
    }

    /// Add a transform triggered on disconnect.
    pub fn on_disconnect(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnDisconnect, transform));
        self
    }

    /// Add a transform triggered on error.
    pub fn on_error(mut self, transform: StateTransform) -> Self {
        self.event_transforms
            .push((EventTrigger::OnError, transform));
        self
    }

    /// Add a transform for a custom event trigger.
    pub fn on_event(mut self, trigger: EventTrigger, transform: StateTransform) -> Self {
        self.event_transforms.push((trigger, transform));
        self
    }

    /// Add a guard checked before tool execution.
    /// If `tool_name` is None, the guard applies to all tools.
    pub fn guard_before_tool(
        mut self,
        tool_name: Option<String>,
        guard: StateGuard,
    ) -> Self {
        self.guards.push((GuardPoint::BeforeTool(tool_name), guard));
        self
    }

    /// Add a guard checked before sending text.
    pub fn guard_before_send_text(mut self, guard: StateGuard) -> Self {
        self.guards.push((GuardPoint::BeforeSendText, guard));
        self
    }

    /// Add a guard checked on turn completion.
    pub fn guard_on_turn_complete(mut self, guard: StateGuard) -> Self {
        self.guards.push((GuardPoint::OnTurnComplete, guard));
        self
    }

    /// Set an initial state value (applied on connect).
    pub fn initial(
        mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Self {
        self.initial_state.push((key.into(), value));
        self
    }

    /// Build the state policy.
    pub fn build(self) -> StatePolicy {
        StatePolicy {
            event_transforms: self.event_transforms,
            guards: self.guards,
            initial_state: self.initial_state,
        }
    }
}

// ---------------------------------------------------------------------------
// State manager (runtime)
// ---------------------------------------------------------------------------

/// Runtime state manager that applies policies to conversation events.
///
/// Created from a [`StatePolicy`] and [`ConversationState`], used internally
/// by the agent's event router to automatically update state on events.
pub struct StateManager {
    policy: StatePolicy,
    state: ConversationState,
}

impl StateManager {
    /// Create a new state manager with a policy and initial state.
    pub fn new(policy: StatePolicy) -> Self {
        let mut state = ConversationState::new();
        policy.initialize_state(&mut state);
        Self { policy, state }
    }

    /// Process a session event: apply matching transforms.
    pub fn process_event(&mut self, event: &SessionEvent) {
        self.policy.apply_event_transforms(event, &mut self.state);
    }

    /// Check guards for a given point.
    pub fn check_guards(&self, point: &GuardPoint) -> Result<(), String> {
        self.policy.check_guards(point, &self.state)
    }

    /// Get a typed value from session state.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(key)
    }

    /// Set a session state value.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.state.set(key, value);
    }

    /// Access the full conversation state.
    pub fn state(&self) -> &ConversationState {
        &self.state
    }

    /// Mutable access to the conversation state.
    pub fn state_mut(&mut self) -> &mut ConversationState {
        &mut self.state
    }

    /// Access the policy.
    pub fn policy(&self) -> &StatePolicy {
        &self.policy
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_scope_prefixes() {
        assert_eq!(StateScope::Session.prefix(), "");
        assert_eq!(StateScope::App.prefix(), "app:");
        assert_eq!(StateScope::User.prefix(), "user:");
        assert_eq!(StateScope::Temp.prefix(), "temp:");
    }

    #[test]
    fn state_scope_from_key() {
        assert_eq!(StateScope::from_key("name"), (StateScope::Session, "name"));
        assert_eq!(StateScope::from_key("app:config"), (StateScope::App, "config"));
        assert_eq!(StateScope::from_key("user:prefs"), (StateScope::User, "prefs"));
        assert_eq!(StateScope::from_key("temp:cache"), (StateScope::Temp, "cache"));
    }

    #[test]
    fn conversation_state_basic() {
        let mut state = ConversationState::new();
        state.set("name", serde_json::json!("Alice"));
        state.set("age", serde_json::json!(30));

        assert_eq!(state.get::<String>("name"), Some("Alice".to_string()));
        assert_eq!(state.get::<i64>("age"), Some(30));
        assert!(state.contains("name"));
        assert!(!state.contains("missing"));
        assert_eq!(state.len(), 2);
    }

    #[test]
    fn conversation_state_scoped() {
        let mut state = ConversationState::new();
        state.set_scoped(StateScope::User, "tier", serde_json::json!("gold"));
        state.set_scoped(StateScope::App, "version", serde_json::json!("1.0"));
        state.set_scoped(StateScope::Temp, "cache", serde_json::json!(42));

        assert_eq!(
            state.get_scoped::<String>(StateScope::User, "tier"),
            Some("gold".to_string())
        );
        assert_eq!(
            state.get_scoped::<String>(StateScope::App, "version"),
            Some("1.0".to_string())
        );
        assert_eq!(
            state.get_scoped::<i64>(StateScope::Temp, "cache"),
            Some(42)
        );
    }

    #[test]
    fn clear_temp() {
        let mut state = ConversationState::new();
        state.set("session_key", serde_json::json!(1));
        state.set_scoped(StateScope::Temp, "temp_key", serde_json::json!(2));
        state.set_scoped(StateScope::User, "user_key", serde_json::json!(3));

        state.clear_temp();
        assert!(state.contains("session_key"));
        assert!(!state.contains("temp:temp_key"));
        assert!(state.contains("user:user_key"));
    }

    #[test]
    fn clear_session() {
        let mut state = ConversationState::new();
        state.set("session_key", serde_json::json!(1));
        state.set_scoped(StateScope::App, "app_key", serde_json::json!(2));
        state.set_scoped(StateScope::Temp, "temp_key", serde_json::json!(3));

        state.clear_session();
        assert!(!state.contains("session_key"));
        assert!(state.contains("app:app_key"));
        assert!(state.contains("temp:temp_key"));
    }

    #[test]
    fn keys_for_scope() {
        let mut state = ConversationState::new();
        state.set("a", serde_json::json!(1));
        state.set("b", serde_json::json!(2));
        state.set_scoped(StateScope::App, "c", serde_json::json!(3));
        state.set_scoped(StateScope::User, "d", serde_json::json!(4));

        let session_keys = state.keys_for_scope(StateScope::Session);
        assert_eq!(session_keys.len(), 2);

        let app_keys = state.keys_for_scope(StateScope::App);
        assert_eq!(app_keys.len(), 1);
    }

    #[test]
    fn transform_set() {
        let mut state = ConversationState::new();
        StateTransform::set("key", serde_json::json!("value")).apply(&mut state);
        assert_eq!(state.get::<String>("key"), Some("value".to_string()));
    }

    #[test]
    fn transform_drop() {
        let mut state = ConversationState::new();
        state.set("a", serde_json::json!(1));
        state.set("b", serde_json::json!(2));
        StateTransform::drop_keys(vec!["a".to_string()]).apply(&mut state);
        assert!(!state.contains("a"));
        assert!(state.contains("b"));
    }

    #[test]
    fn transform_rename() {
        let mut state = ConversationState::new();
        state.set("old", serde_json::json!("data"));
        StateTransform::rename("old", "new").apply(&mut state);
        assert!(!state.contains("old"));
        assert_eq!(state.get::<String>("new"), Some("data".to_string()));
    }

    #[test]
    fn transform_increment() {
        let mut state = ConversationState::new();
        state.set("counter", serde_json::json!(5));
        StateTransform::increment("counter", 3).apply(&mut state);
        assert_eq!(state.get::<i64>("counter"), Some(8));
    }

    #[test]
    fn transform_increment_from_zero() {
        let mut state = ConversationState::new();
        StateTransform::increment("counter", 1).apply(&mut state);
        assert_eq!(state.get::<i64>("counter"), Some(1));
    }

    #[test]
    fn transform_chain() {
        let mut state = ConversationState::new();
        let chain = StateTransform::chain(vec![
            StateTransform::set("a", serde_json::json!(1)),
            StateTransform::set("b", serde_json::json!(2)),
            StateTransform::increment("a", 10),
        ]);
        chain.apply(&mut state);
        assert_eq!(state.get::<i64>("a"), Some(11));
        assert_eq!(state.get::<i64>("b"), Some(2));
    }

    #[test]
    fn transform_compute() {
        let mut state = ConversationState::new();
        state.set("x", serde_json::json!(10));
        state.set("y", serde_json::json!(20));

        let compute = StateTransform::compute("sum", |s| {
            let x = s.get::<i64>("x").unwrap_or(0);
            let y = s.get::<i64>("y").unwrap_or(0);
            serde_json::json!(x + y)
        });
        compute.apply(&mut state);
        assert_eq!(state.get::<i64>("sum"), Some(30));
    }

    #[test]
    fn state_guard_passes() {
        let guard = StateGuard::new("check", |state| {
            if state.get::<bool>("ready").unwrap_or(false) {
                Ok(())
            } else {
                Err("Not ready".to_string())
            }
        });

        let mut state = ConversationState::new();
        state.set("ready", serde_json::json!(true));
        assert!(guard.check(&state).is_ok());
    }

    #[test]
    fn state_guard_fails() {
        let guard = StateGuard::new("check", |state| {
            if state.get::<bool>("ready").unwrap_or(false) {
                Ok(())
            } else {
                Err("Not ready".to_string())
            }
        });

        let state = ConversationState::new();
        assert_eq!(guard.check(&state), Err("Not ready".to_string()));
    }

    #[test]
    fn event_trigger_matching() {
        assert!(EventTrigger::OnConnect.matches(&SessionEvent::Connected));
        assert!(EventTrigger::OnTurnComplete.matches(&SessionEvent::TurnComplete));
        assert!(EventTrigger::OnInterrupted.matches(&SessionEvent::Interrupted));
        assert!(!EventTrigger::OnConnect.matches(&SessionEvent::TurnComplete));
    }

    #[test]
    fn state_policy_applies_transforms() {
        let policy = StatePolicy::builder()
            .on_turn_complete(StateTransform::increment("turn_count", 1))
            .on_interrupted(StateTransform::increment("interruptions", 1))
            .initial("turn_count", serde_json::json!(0))
            .initial("interruptions", serde_json::json!(0))
            .build();

        let mut state = ConversationState::new();
        policy.initialize_state(&mut state);

        policy.apply_event_transforms(&SessionEvent::TurnComplete, &mut state);
        policy.apply_event_transforms(&SessionEvent::TurnComplete, &mut state);
        policy.apply_event_transforms(&SessionEvent::Interrupted, &mut state);

        assert_eq!(state.get::<i64>("turn_count"), Some(2));
        assert_eq!(state.get::<i64>("interruptions"), Some(1));
    }

    #[test]
    fn state_policy_guards() {
        let policy = StatePolicy::builder()
            .guard_before_tool(
                None,
                StateGuard::new("auth", |s| {
                    if s.get::<bool>("authenticated").unwrap_or(false) {
                        Ok(())
                    } else {
                        Err("Not authenticated".to_string())
                    }
                }),
            )
            .build();

        let state = ConversationState::new();
        let result = policy.check_guards(&GuardPoint::BeforeTool(None), &state);
        assert!(result.is_err());
    }

    #[test]
    fn state_manager_processes_events() {
        let policy = StatePolicy::builder()
            .initial("status", serde_json::json!("idle"))
            .on_connect(StateTransform::set("status", serde_json::json!("active")))
            .on_turn_complete(StateTransform::increment("turns", 1))
            .build();

        let mut mgr = StateManager::new(policy);
        assert_eq!(mgr.get::<String>("status"), Some("idle".to_string()));

        mgr.process_event(&SessionEvent::Connected);
        assert_eq!(mgr.get::<String>("status"), Some("active".to_string()));

        mgr.process_event(&SessionEvent::TurnComplete);
        mgr.process_event(&SessionEvent::TurnComplete);
        assert_eq!(mgr.get::<i64>("turns"), Some(2));
    }

    #[test]
    fn state_merge() {
        let mut state1 = ConversationState::new();
        state1.set("a", serde_json::json!(1));

        let mut state2 = ConversationState::new();
        state2.set("b", serde_json::json!(2));
        state2.set("a", serde_json::json!(99)); // override

        state1.merge(&state2);
        assert_eq!(state1.get::<i64>("a"), Some(99));
        assert_eq!(state1.get::<i64>("b"), Some(2));
    }

    #[test]
    fn state_export_import() {
        let mut state = ConversationState::new();
        state.set("key", serde_json::json!("value"));

        let exported = state.export().clone();
        let mut state2 = ConversationState::new();
        state2.import(exported);

        assert_eq!(state2.get::<String>("key"), Some("value".to_string()));
    }
}
