//! Sub-builders for the fluent [`Live`](crate::live::Live) API.
//!
//! These builders use a "move self, return `Live`" pattern so that the
//! caller's chain stays fully typed and fluent:
//!
//! ```ignore
//! Live::builder()
//!     .phase("greeting")
//!         .instruction("Welcome the user")
//!         .transition("main", |s| s.get::<bool>("greeted").unwrap_or(false))
//!         .done()
//!     .phase("main")
//!         .instruction("Handle the conversation")
//!         .terminal()
//!         .done()
//!     .initial_phase("greeting")
//!     .connect_vertex(project, location, token)
//!     .await?;
//! ```

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use rs_adk::live::{
    BoxFuture, Phase, PhaseInstruction, Transition, WatchPredicate, Watcher,
};
use rs_adk::State;
use rs_genai::session::SessionWriter;

use crate::live::Live;

// ── PhaseBuilder ─────────────────────────────────────────────────────────────

/// Builder for a conversation phase.
///
/// Created by [`Live::phase`] and returned to the `Live` chain via [`done`](Self::done).
pub struct PhaseBuilder {
    live: Live,
    name: String,
    instruction: Option<PhaseInstruction>,
    tools_enabled: Option<Vec<String>>,
    guard: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
    on_enter: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    on_exit: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    transitions: Vec<Transition>,
    terminal: bool,
}

impl PhaseBuilder {
    pub(crate) fn new(live: Live, name: impl Into<String>) -> Self {
        Self {
            live,
            name: name.into(),
            instruction: None,
            tools_enabled: None,
            guard: None,
            on_enter: None,
            on_exit: None,
            transitions: Vec::new(),
            terminal: false,
        }
    }

    /// Set a static instruction for this phase.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(PhaseInstruction::Static(instruction.into()));
        self
    }

    /// Set a dynamic instruction that is resolved from state at transition time.
    pub fn dynamic_instruction<F>(mut self, f: F) -> Self
    where
        F: Fn(&State) -> String + Send + Sync + 'static,
    {
        self.instruction = Some(PhaseInstruction::Dynamic(Arc::new(f)));
        self
    }

    /// Set the tool filter for this phase. Only these tools will be enabled.
    pub fn tools(mut self, tools: Vec<String>) -> Self {
        self.tools_enabled = Some(tools);
        self
    }

    /// Set a guard that must return `true` for this phase to be entered.
    pub fn guard<F>(mut self, f: F) -> Self
    where
        F: Fn(&State) -> bool + Send + Sync + 'static,
    {
        self.guard = Some(Arc::new(f));
        self
    }

    /// Set an async callback to run when entering this phase.
    pub fn on_enter<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_enter = Some(Arc::new(move |s, w| Box::pin(f(s, w))));
        self
    }

    /// Set an async callback to run when exiting this phase.
    pub fn on_exit<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_exit = Some(Arc::new(move |s, w| Box::pin(f(s, w))));
        self
    }

    /// Add a guard-based transition to a target phase.
    pub fn transition(
        mut self,
        target: &str,
        guard: impl Fn(&State) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.transitions.push(Transition {
            target: target.to_string(),
            guard: Arc::new(guard),
        });
        self
    }

    /// Mark this phase as terminal (no outbound transitions will be evaluated).
    pub fn terminal(mut self) -> Self {
        self.terminal = true;
        self
    }

    /// Finish building this phase and return the `Live` builder.
    pub fn done(mut self) -> Live {
        let phase = Phase {
            name: self.name,
            instruction: self
                .instruction
                .unwrap_or(PhaseInstruction::Static(String::new())),
            tools_enabled: self.tools_enabled,
            guard: self.guard,
            on_enter: self.on_enter,
            on_exit: self.on_exit,
            transitions: self.transitions,
            terminal: self.terminal,
        };
        self.live.add_phase(phase);
        self.live
    }
}

// ── WatchBuilder ─────────────────────────────────────────────────────────────

/// Builder for a state watcher.
///
/// Created by [`Live::watch`] and returned to the `Live` chain via [`then`](Self::then).
pub struct WatchBuilder {
    live: Live,
    key: String,
    predicate: Option<WatchPredicate>,
    blocking: bool,
}

impl WatchBuilder {
    pub(crate) fn new(live: Live, key: impl Into<String>) -> Self {
        Self {
            live,
            key: key.into(),
            predicate: None,
            blocking: false,
        }
    }

    /// Fire on any change to the watched key (default).
    pub fn changed(mut self) -> Self {
        self.predicate = Some(WatchPredicate::Changed);
        self
    }

    /// Fire when the new value equals the given value.
    pub fn changed_to(mut self, value: Value) -> Self {
        self.predicate = Some(WatchPredicate::ChangedTo(value));
        self
    }

    /// Fire when the value crosses above the given threshold.
    pub fn crossed_above(mut self, threshold: f64) -> Self {
        self.predicate = Some(WatchPredicate::CrossedAbove(threshold));
        self
    }

    /// Fire when the value crosses below the given threshold.
    pub fn crossed_below(mut self, threshold: f64) -> Self {
        self.predicate = Some(WatchPredicate::CrossedBelow(threshold));
        self
    }

    /// Fire when the value changes from non-true to true.
    pub fn became_true(mut self) -> Self {
        self.predicate = Some(WatchPredicate::BecameTrue);
        self
    }

    /// Fire when the value changes from true to non-true.
    pub fn became_false(mut self) -> Self {
        self.predicate = Some(WatchPredicate::BecameFalse);
        self
    }

    /// Make this watcher blocking (awaited sequentially on the control lane).
    pub fn blocking(mut self) -> Self {
        self.blocking = true;
        self
    }

    /// Set the action and finish building the watcher, returning the `Live` builder.
    ///
    /// The action receives `(old_value, new_value, state)`.
    pub fn then<F, Fut>(mut self, f: F) -> Live
    where
        F: Fn(Value, Value, State) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let watcher = Watcher {
            key: self.key,
            predicate: self.predicate.unwrap_or(WatchPredicate::Changed),
            action: Arc::new(move |old, new, state| Box::pin(f(old, new, state))),
            blocking: self.blocking,
        };
        self.live.add_watcher(watcher);
        self.live
    }
}
