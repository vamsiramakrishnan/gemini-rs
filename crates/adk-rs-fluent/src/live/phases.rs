//! Phase machine, instruction templating, computed state, watchers, and
//! temporal pattern configuration methods for `Live`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use rs_adk::live::{
    ComputedVar, Phase, RateDetector, SustainedDetector, TemporalPattern, TurnCountDetector,
    Watcher,
};
use rs_adk::State;
use rs_genai::prelude::*;
use rs_genai::session::SessionWriter;

use crate::live_builders::{PhaseBuilder, PhaseDefaults, WatchBuilder};

use super::Live;

impl Live {
    /// State-reactive system instruction template.
    ///
    /// Called after extractors run on each turn. If it returns `Some(instruction)`,
    /// the system instruction is updated mid-session (deduped — same instruction
    /// is not sent twice). Returns `None` to leave the instruction unchanged.
    ///
    /// # Example
    /// ```ignore
    /// .instruction_template(|state| {
    ///     let phase: String = state.get("phase").unwrap_or_default();
    ///     match phase.as_str() {
    ///         "ordering" => Some("Focus on taking the order accurately.".into()),
    ///         "confirming" => Some("Summarize and confirm the order.".into()),
    ///         _ => None,
    ///     }
    /// })
    /// ```
    pub fn instruction_template(
        mut self,
        f: impl Fn(&rs_adk::State) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.instruction_template = Some(Arc::new(f));
        self
    }

    /// State-reactive instruction amendment (additive, not replacement).
    ///
    /// Unlike `instruction_template` (which replaces the entire instruction),
    /// this appends to the current phase instruction. The developer never needs
    /// to know or repeat the base instruction.
    ///
    /// # Example
    /// ```ignore
    /// .instruction_amendment(|state| {
    ///     let risk: String = state.get("derived:risk").unwrap_or_default();
    ///     if risk == "high" {
    ///         Some("[IMPORTANT: Use empathetic language. Do not threaten.]".into())
    ///     } else {
    ///         None
    ///     }
    /// })
    /// ```
    pub fn instruction_amendment(
        mut self,
        f: impl Fn(&rs_adk::State) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.instruction_amendment = Some(Arc::new(f));
        self
    }

    // -- Computed State --

    /// Register a computed (derived) state variable.
    ///
    /// The compute function receives the full `State` and returns `Some(value)`
    /// to write to `derived:{key}`, or `None` to skip.
    pub fn computed(
        mut self,
        key: impl Into<String>,
        deps: &[&str],
        f: impl Fn(&State) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        self.computed.register(ComputedVar {
            key: key.into(),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            compute: Arc::new(f),
        });
        self
    }

    // -- Phase Machine --

    /// Set default modifiers and `prompt_on_enter` inherited by all phases.
    ///
    /// Phase-specific modifiers are applied *after* defaults, so they extend (not replace).
    ///
    /// ```ignore
    /// Live::builder()
    ///     .phase_defaults(|p| {
    ///         p.with_state(&["emotional_state", "risk_level"])
    ///          .when(risk_is_elevated, "Show extra empathy.")
    ///          .prompt_on_enter(true)
    ///     })
    ///     .phase("greet").instruction("...").done()
    ///     .phase("close").instruction("...").done()
    ///     // Both phases inherit the modifiers and prompt_on_enter.
    /// ```
    pub fn phase_defaults(mut self, f: impl FnOnce(PhaseDefaults) -> PhaseDefaults) -> Self {
        let defaults = f(PhaseDefaults::new());
        self.phase_default_modifiers = defaults.modifiers;
        self.phase_default_prompt_on_enter = defaults.prompt_on_enter;
        self
    }

    /// Start building a conversation phase.
    ///
    /// Returns a [`PhaseBuilder`] that flows back to this `Live` via `.done()`.
    pub fn phase(self, name: impl Into<String>) -> PhaseBuilder {
        PhaseBuilder::new(self, name)
    }

    /// Set the initial phase name (must match a registered phase).
    pub fn initial_phase(mut self, name: impl Into<String>) -> Self {
        self.initial_phase = Some(name.into());
        self
    }

    /// Internal method called by [`PhaseBuilder::done`].
    pub(crate) fn add_phase(&mut self, phase: Phase) {
        self.phases.push(phase);
    }

    // -- Watchers --

    /// Start building a state watcher.
    ///
    /// Returns a [`WatchBuilder`] that flows back to this `Live` via `.then()`.
    pub fn watch(self, key: impl Into<String>) -> WatchBuilder {
        WatchBuilder::new(self, key)
    }

    /// Internal method called by [`WatchBuilder::then`].
    pub(crate) fn add_watcher(&mut self, watcher: Watcher) {
        self.watchers.add(watcher);
    }

    // -- Temporal Patterns --

    /// Register a sustained condition pattern.
    ///
    /// Fires when the condition remains true for at least `duration`.
    pub fn when_sustained<F, Fut>(
        mut self,
        name: impl Into<String>,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        duration: Duration,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = SustainedDetector::new(Arc::new(condition), duration);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }

    /// Register a rate detection pattern.
    ///
    /// Fires when at least `count` matching events occur within `window`.
    pub fn when_rate<F, Fut>(
        mut self,
        name: impl Into<String>,
        filter: impl Fn(&SessionEvent) -> bool + Send + Sync + 'static,
        count: u32,
        window: Duration,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = RateDetector::new(Arc::new(filter), count, window);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }

    /// Register a turn count pattern.
    ///
    /// Fires when the condition is true for `turn_count` consecutive turns.
    pub fn when_turns<F, Fut>(
        mut self,
        name: impl Into<String>,
        condition: impl Fn(&State) -> bool + Send + Sync + 'static,
        turn_count: u32,
        action: F,
    ) -> Self
    where
        F: Fn(State, Arc<dyn SessionWriter>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let detector = TurnCountDetector::new(Arc::new(condition), turn_count);
        self.temporal.add(TemporalPattern::new(
            name,
            Box::new(detector),
            Arc::new(move |s, w| Box::pin(action(s, w))),
            None,
        ));
        self
    }
}
