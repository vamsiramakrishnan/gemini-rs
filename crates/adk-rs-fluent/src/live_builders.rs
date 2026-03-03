//! Sub-builders for the fluent [`Live`] API.
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
    BoxFuture, InstructionModifier, Phase, PhaseInstruction, Transition, TranscriptWindow,
    WatchPredicate, Watcher,
};
use rs_adk::State;
use rs_genai::prelude::Content;
use rs_genai::session::SessionWriter;

use crate::live::Live;

// ── PhaseDefaults ────────────────────────────────────────────────────────────

/// Default modifiers and settings inherited by all phases.
///
/// Created by [`Live::phase_defaults`] and applied in [`PhaseBuilder::done`].
pub struct PhaseDefaults {
    pub(crate) modifiers: Vec<InstructionModifier>,
    pub(crate) prompt_on_enter: bool,
}

impl PhaseDefaults {
    pub(crate) fn new() -> Self {
        Self {
            modifiers: Vec::new(),
            prompt_on_enter: false,
        }
    }

    /// Append state keys to every phase's instruction at runtime.
    pub fn with_state(mut self, keys: &[&str]) -> Self {
        self.modifiers.push(InstructionModifier::StateAppend(
            keys.iter().map(|s| s.to_string()).collect(),
        ));
        self
    }

    /// Conditionally append text to every phase when predicate is true.
    pub fn when(
        mut self,
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> Self {
        self.modifiers.push(InstructionModifier::Conditional {
            predicate: Arc::new(predicate),
            text: text.into(),
        });
        self
    }

    /// Append custom formatted context to every phase's instruction.
    pub fn with_context(
        mut self,
        f: impl Fn(&State) -> String + Send + Sync + 'static,
    ) -> Self {
        self.modifiers.push(InstructionModifier::CustomAppend(Arc::new(f)));
        self
    }

    /// Enable `prompt_on_enter` for all phases (model responds immediately on entry).
    pub fn prompt_on_enter(mut self, enabled: bool) -> Self {
        self.prompt_on_enter = enabled;
        self
    }
}

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
    modifiers: Vec<InstructionModifier>,
    prompt_on_enter_flag: bool,
    on_enter_context_fn: Option<Arc<
        dyn Fn(&State, &TranscriptWindow) -> Option<Vec<Content>> + Send + Sync
    >>,
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
            modifiers: Vec::new(),
            prompt_on_enter_flag: false,
            on_enter_context_fn: None,
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

    /// Append state keys to the instruction at runtime.
    /// Renders as `[Context: key1=val1, key2=val2, ...]`.
    pub fn with_state(mut self, keys: &[&str]) -> Self {
        self.modifiers.push(InstructionModifier::StateAppend(
            keys.iter().map(|s| s.to_string()).collect(),
        ));
        self
    }

    /// Conditionally append text when a predicate is true.
    pub fn when(
        mut self,
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> Self {
        self.modifiers.push(InstructionModifier::Conditional {
            predicate: Arc::new(predicate),
            text: text.into(),
        });
        self
    }

    /// Append the result of a custom formatter to the instruction.
    pub fn with_context(
        mut self,
        f: impl Fn(&State) -> String + Send + Sync + 'static,
    ) -> Self {
        self.modifiers.push(InstructionModifier::CustomAppend(Arc::new(f)));
        self
    }

    /// Send `turnComplete: true` after instruction + context on phase entry,
    /// causing the model to generate a response immediately.
    pub fn prompt_on_enter(mut self, enabled: bool) -> Self {
        self.prompt_on_enter_flag = enabled;
        self
    }

    /// Set a context injection callback for phase entry.
    /// Returns `Content` to send as `client_content` before prompting.
    pub fn on_enter_context<F>(mut self, f: F) -> Self
    where
        F: Fn(&State, &TranscriptWindow) -> Option<Vec<Content>> + Send + Sync + 'static,
    {
        self.on_enter_context_fn = Some(Arc::new(f));
        self
    }

    /// Inject a model-role bridge message on phase entry and prompt immediately.
    ///
    /// Combines `on_enter_context` + `prompt_on_enter(true)` into a single call,
    /// eliminating the need to import `Content` in cookbook code.
    ///
    /// ```ignore
    /// .phase("verify_identity")
    ///     .instruction(VERIFY_IDENTITY_INSTRUCTION)
    ///     .enter_prompt("The caller confirmed the disclosure. I'll now verify their identity.")
    ///     .done()
    /// ```
    pub fn enter_prompt(mut self, message: impl Into<String>) -> Self {
        let msg = message.into();
        self.on_enter_context_fn = Some(Arc::new(move |_, _| {
            Some(vec![Content::model(msg.clone())])
        }));
        self.prompt_on_enter_flag = true;
        self
    }

    /// Like [`enter_prompt`](Self::enter_prompt) but with a state-aware closure.
    ///
    /// ```ignore
    /// .enter_prompt_fn(|state, _tw| {
    ///     if state.get::<bool>("cease_desist_requested").unwrap_or(false) {
    ///         "Cease-and-desist requested. Closing call respectfully.".into()
    ///     } else {
    ///         "Wrapping up the call.".into()
    ///     }
    /// })
    /// ```
    pub fn enter_prompt_fn<F>(mut self, f: F) -> Self
    where
        F: Fn(&State, &TranscriptWindow) -> String + Send + Sync + 'static,
    {
        self.on_enter_context_fn = Some(Arc::new(move |state, tw| {
            Some(vec![Content::model(f(state, tw))])
        }));
        self.prompt_on_enter_flag = true;
        self
    }

    /// Apply a slice of pre-built instruction modifiers to this phase.
    ///
    /// Use with `P::with_state()`, `P::when()`, `P::context_fn()` factories.
    ///
    /// ```ignore
    /// .phase("disclosure")
    ///     .modifiers(&[P::with_state(KEYS), P::when(pred, "warning")])
    ///     .done()
    /// ```
    pub fn modifiers(mut self, mods: &[InstructionModifier]) -> Self {
        self.modifiers.extend(mods.iter().cloned());
        self
    }

    /// Finish building this phase and return the `Live` builder.
    ///
    /// Merges phase defaults (from [`Live::phase_defaults`]) with phase-specific
    /// settings. Defaults are prepended so phase-specific modifiers take priority.
    pub fn done(mut self) -> Live {
        // Merge defaults: prepend default modifiers, inherit prompt_on_enter if not set.
        let mut merged_modifiers = self.live.phase_default_modifiers.clone();
        merged_modifiers.append(&mut self.modifiers);

        let prompt = self.prompt_on_enter_flag || self.live.phase_default_prompt_on_enter;

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
            modifiers: merged_modifiers,
            prompt_on_enter: prompt,
            on_enter_context: self.on_enter_context_fn,
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
