//! The conversation compiler (Phase 1 MVP).
//!
//! Authors describe a voice experience in terms of **stages** that *say* things,
//! *collect* slots, *commit* tools behind confirmation, and advance via *next*
//! transitions. A [`Conversation`] builder produces a serializable
//! [`ConversationSpec`] (the single source of truth), and [`Conversation::compile`]
//! lowers it to the governed [`CompiledFlow`] IR — so the high level is sugar over
//! the low level, never a parallel runtime (see
//! `docs/plans/2026-06-06-conversation-compiler-rfc.md`).
//!
//! ```ignore
//! let convo = Conversation::new("booking")
//!     .stage("collect")
//!         .say("Help the user book a table.")
//!         .collect(["party_size", "slot"])
//!         .next("check", Guard::captured(["party_size", "slot"]))
//!     .stage("check")
//!         .ground("Party of {party_size} at {slot}.")
//!         .next("confirm", Guard::is_true("availability_ok"))
//!     .stage("confirm")
//!         .commit("book", Guard::is_true("user_confirmed"))
//!         .next("done", Guard::called_ok("book"))
//!     .stage("done").terminal()
//!     .require(["done"])
//!     .compile()?;
//! let mut monitor = convo.monitor(FlowMode::Enforce);
//! ```
//!
//! ### Lowering semantics (MVP)
//!
//! - A stage lowers to a Flow [`Step`](gemini_adk_rs::flow). `say` → posture,
//!   `ground` → grounding template, `allow` → tool whitelist.
//! - `commit(tool, when)` gates a confirm-before-act tool (`tool` is auto-allowed
//!   in the stage).
//! - `next(to, when)` adds a forward edge: `to` depends on this stage (`after`)
//!   and its activation gate is `when`. Multiple incoming edges are an **AND-join**
//!   (all predecessors must complete) — richer topologies are Phase 3.
//! - A non-terminal stage's completion is, in priority order: an explicit
//!   `done`, else `captured(collect)` when it collects slots, else the disjunction
//!   of its `next` conditions.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use gemini_adk_rs::extract::Extract;
use gemini_adk_rs::flow::{CompiledFlow, Enforcement, Flow, FlowErrors, FlowMonitor, Guard, Pred};
use gemini_adk_rs::frame::{Frame, FrameSpec};

/// A transition: advance to `to` when `when` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionSpec {
    /// Target stage id.
    pub to: String,
    /// The (serializable) guard that fires this transition.
    pub when: Guard,
}

/// A confirm-before-act tool, gated by `when`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSpec {
    /// The committing tool (e.g. `book`, `charge_card`).
    pub tool: String,
    /// The guard that must hold before the tool is admitted.
    pub when: Guard,
}

/// One authored conversation stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageSpec {
    /// Unique stage id.
    pub id: String,
    /// Instruction projected as steering while the stage is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    /// Grounding template projected while active (`{key}` interpolation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<String>,
    /// Slots to collect; drives the default completion (`captured`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collect: Vec<String>,
    /// The frame whose slots this stage collects, if set via `collect_frame`. Its
    /// recognizer-bearing slots lower to an extractor that fills the slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameSpec>,
    /// Tools available while this stage is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Explicit completion guard (overrides the `collect`/`next` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<Guard>,
    /// A confirm-before-act tool committed in this stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitSpec>,
    /// Forward transitions out of this stage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<TransitionSpec>,
    /// Explicit dependency stage ids (in addition to `next`-derived edges).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    /// Whether this is a terminal stage.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

/// The serializable authoring spec — the single source of truth from which the
/// typed builder, YAML, and (later) codegen all derive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationSpec {
    /// Conversation name.
    pub name: String,
    /// The authored stages.
    #[serde(default)]
    pub stages: Vec<StageSpec>,
    /// Stages that must be done for the conversation to be complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<String>,
}

/// Error compiling a [`ConversationSpec`] into a [`CompiledConversation`].
#[derive(Debug)]
pub enum ConversationError {
    /// The spec has no stages.
    Empty,
    /// An authoring-level error (e.g. a transition to an unknown stage).
    Spec(String),
    /// The lowered flow failed referential/acyclicity validation.
    Flow(Vec<String>),
    /// The lowered flow failed to compile (unreachable steps, unguarded commit…).
    Compile(FlowErrors),
}

impl std::fmt::Display for ConversationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversationError::Empty => write!(f, "conversation has no stages"),
            ConversationError::Spec(m) => write!(f, "conversation spec error: {m}"),
            ConversationError::Flow(errs) => {
                write!(f, "lowered flow is invalid: {}", errs.join("; "))
            }
            ConversationError::Compile(e) => write!(f, "lowered flow failed to compile: {e}"),
        }
    }
}

impl std::error::Error for ConversationError {}

/// A compiled conversation: the validated [`CompiledFlow`], the extractors that
/// fill its frames' slots, and the source spec.
#[derive(Clone)]
pub struct CompiledConversation {
    flow: CompiledFlow,
    extractors: Vec<Extract>,
    spec: ConversationSpec,
}

// Manual: the lowered `Extract`s hold recognizer/resolver closures (not `Debug`),
// so they are summarized by count.
impl std::fmt::Debug for CompiledConversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledConversation")
            .field("flow", &self.flow)
            .field("extractors", &self.extractors.len())
            .field("spec", &self.spec)
            .finish()
    }
}

impl CompiledConversation {
    /// The governed flow IR this conversation lowered to.
    pub fn flow(&self) -> &CompiledFlow {
        &self.flow
    }
    /// The extractors lowered from `collect_frame` stages — register these on the
    /// live session so each turn fills the frames' slots from the transcript.
    pub fn extractors(&self) -> &[Extract] {
        &self.extractors
    }
    /// The authoring spec it was compiled from.
    pub fn spec(&self) -> &ConversationSpec {
        &self.spec
    }
    /// Render the lowered flow as a Mermaid diagram.
    pub fn to_mermaid(&self) -> String {
        self.flow.to_mermaid()
    }
    /// Build a [`FlowMonitor`] over the lowered flow.
    pub fn monitor(&self, mode: Enforcement) -> FlowMonitor {
        FlowMonitor::compiled(self.flow.clone(), mode)
    }
}

/// Fluent builder that produces a [`ConversationSpec`]; sugar over the spec.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    spec: ConversationSpec,
}

impl Conversation {
    /// Start a new conversation.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            spec: ConversationSpec {
                name: name.into(),
                ..Default::default()
            },
        }
    }

    /// Begin authoring a new stage; subsequent setters apply to it.
    pub fn stage(mut self, id: impl Into<String>) -> Self {
        self.spec.stages.push(StageSpec {
            id: id.into(),
            ..Default::default()
        });
        self
    }

    fn current(&mut self) -> &mut StageSpec {
        self.spec
            .stages
            .last_mut()
            .expect("call .stage(..) before configuring a stage")
    }

    /// Set the stage's steering instruction.
    pub fn say(mut self, text: impl Into<String>) -> Self {
        self.current().say = Some(text.into());
        self
    }

    /// Set the stage's grounding template.
    pub fn ground(mut self, template: impl Into<String>) -> Self {
        self.current().ground = Some(template.into());
        self
    }

    /// Collect the given slots in this stage (drives default completion).
    pub fn collect<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.current().collect = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Collect the slots of a typed [`Frame`](gemini_adk_rs::frame::Frame) in this
    /// stage. The frame's slot state-keys drive the `captured` completion; its
    /// metadata (prompts/confirm/pii) is available via `F::frame()` for
    /// confirmation and repair.
    pub fn collect_frame<F: Frame>(mut self) -> Self {
        let spec = F::frame();
        let stage = self.current();
        stage.collect = spec.slot_keys();
        stage.frame = Some(spec);
        self
    }

    /// Allow the given tools while this stage is active.
    pub fn allow<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.current().allow = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Set an explicit completion guard for this stage.
    pub fn done(mut self, guard: Guard) -> Self {
        self.current().done = Some(guard);
        self
    }

    /// Commit a confirm-before-act tool in this stage, gated by `when`.
    pub fn commit(mut self, tool: impl Into<String>, when: Guard) -> Self {
        self.current().commit = Some(CommitSpec {
            tool: tool.into(),
            when,
        });
        self
    }

    /// Add a forward transition to `to` when `when` holds.
    pub fn next(mut self, to: impl Into<String>, when: Guard) -> Self {
        self.current().next.push(TransitionSpec {
            to: to.into(),
            when,
        });
        self
    }

    /// Add an explicit dependency on another stage.
    pub fn after(mut self, dep: impl Into<String>) -> Self {
        self.current().after.push(dep.into());
        self
    }

    /// Mark the current stage terminal.
    pub fn terminal(mut self) -> Self {
        self.current().terminal = true;
        self
    }

    /// Require these stages for completion (lowers to a Flow `require`).
    pub fn require<I, S>(mut self, steps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.spec.require = steps.into_iter().map(Into::into).collect();
        self
    }

    /// The spec built so far.
    pub fn spec(&self) -> &ConversationSpec {
        &self.spec
    }

    /// Consume into the underlying spec.
    pub fn into_spec(self) -> ConversationSpec {
        self.spec
    }

    /// Compile from a [`ConversationSpec`] (e.g. parsed from YAML).
    pub fn from_spec(spec: ConversationSpec) -> Result<CompiledConversation, ConversationError> {
        compile_spec(spec)
    }

    /// Lower and validate into a [`CompiledConversation`].
    pub fn compile(self) -> Result<CompiledConversation, ConversationError> {
        compile_spec(self.spec)
    }
}

fn is_always(g: &Guard) -> bool {
    matches!(g, Guard::Spec(Pred::Always))
}

/// Combine guards into a single disjunction, collapsing trivial cases.
fn any_of(guards: Vec<Guard>) -> Option<Guard> {
    if guards.is_empty() {
        return None;
    }
    if guards.iter().any(is_always) {
        return Some(Guard::always());
    }
    if guards.len() == 1 {
        return guards.into_iter().next();
    }
    Some(Guard::any(guards))
}

fn compile_spec(spec: ConversationSpec) -> Result<CompiledConversation, ConversationError> {
    if spec.stages.is_empty() {
        return Err(ConversationError::Empty);
    }

    let ids: BTreeSet<&str> = spec.stages.iter().map(|s| s.id.as_str()).collect();
    if ids.len() != spec.stages.len() {
        return Err(ConversationError::Spec("duplicate stage ids".into()));
    }

    // Referential checks with conversation-level messages (before Flow lowering).
    for s in &spec.stages {
        for t in &s.next {
            if !ids.contains(t.to.as_str()) {
                return Err(ConversationError::Spec(format!(
                    "stage '{}' transitions to unknown stage '{}'",
                    s.id, t.to
                )));
            }
        }
        for d in &s.after {
            if !ids.contains(d.as_str()) {
                return Err(ConversationError::Spec(format!(
                    "stage '{}' depends on unknown stage '{}'",
                    s.id, d
                )));
            }
        }
    }
    for r in &spec.require {
        if !ids.contains(r.as_str()) {
            return Err(ConversationError::Spec(format!(
                "require references unknown stage '{r}'"
            )));
        }
    }

    // Incoming edges: target -> [(source, when)].
    let mut incoming: BTreeMap<&str, Vec<(&str, Guard)>> = BTreeMap::new();
    for s in &spec.stages {
        for t in &s.next {
            incoming
                .entry(t.to.as_str())
                .or_default()
                .push((s.id.as_str(), t.when.clone()));
        }
    }

    let mut fb = Flow::new();
    for s in &spec.stages {
        fb = fb.step(&s.id);

        // Dependencies: explicit `after` plus the sources of incoming transitions.
        let mut deps: BTreeSet<&str> = s.after.iter().map(String::as_str).collect();
        if let Some(inc) = incoming.get(s.id.as_str()) {
            for (src, _) in inc {
                deps.insert(src);
            }
        }
        for d in deps {
            fb = fb.after(d);
        }

        // Activation gate: disjunction of the conditions on incoming edges.
        if let Some(inc) = incoming.get(s.id.as_str()) {
            if let Some(gate) = any_of(inc.iter().map(|(_, w)| w.clone()).collect()) {
                fb = fb.gate(gate);
            }
        }

        if let Some(say) = &s.say {
            fb = fb.posture(say.clone());
        }
        if let Some(ground) = &s.ground {
            fb = fb.ground(ground.clone());
        }

        // Tool whitelist: authored allow plus any commit tool.
        let mut allow: Vec<String> = s.allow.clone();
        if let Some(c) = &s.commit {
            if !allow.contains(&c.tool) {
                allow.push(c.tool.clone());
            }
        }
        if !allow.is_empty() {
            fb = fb.allow(allow);
        }
        if let Some(c) = &s.commit {
            fb = fb.commit(&c.tool, c.when.clone());
        }

        if s.terminal {
            fb = fb.terminal();
        } else {
            let done = stage_completion(s).ok_or_else(|| {
                ConversationError::Spec(format!(
                    "non-terminal stage '{}' has no completion (add collect, next, or done)",
                    s.id
                ))
            })?;
            fb = fb.done(done);
        }
    }

    if !spec.require.is_empty() {
        fb = fb.require(spec.require.clone());
    }

    let flow = fb.build().map_err(ConversationError::Flow)?;
    let compiled = flow.compile().map_err(ConversationError::Compile)?;

    // Lower each frame-collecting stage into an extractor that fills its slots.
    let extractors = spec
        .stages
        .iter()
        .filter_map(|s| s.frame.as_ref().and_then(FrameSpec::to_extract))
        .collect();

    Ok(CompiledConversation {
        flow: compiled,
        extractors,
        spec,
    })
}

/// The completion guard for a non-terminal stage, by priority:
/// explicit `done` → `captured(collect)` → disjunction of `next` conditions.
fn stage_completion(s: &StageSpec) -> Option<Guard> {
    if let Some(g) = &s.done {
        return Some(g.clone());
    }
    if !s.collect.is_empty() {
        return Some(Guard::captured(s.collect.clone()));
    }
    any_of(s.next.iter().map(|t| t.when.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_adk_rs::flow::Enforcement;
    use gemini_adk_rs::state::State;

    fn booking() -> CompiledConversation {
        Conversation::new("booking")
            .stage("collect")
            .say("Help the user book a table.")
            .collect(["party_size", "slot"])
            .next("check", Guard::captured(["party_size", "slot"]))
            .stage("check")
            .ground("Party of {party_size} at {slot}.")
            .next("confirm", Guard::is_true("availability_ok"))
            .stage("confirm")
            .commit("book", Guard::is_true("user_confirmed"))
            .next("done", Guard::called_ok("book"))
            .stage("done")
            .terminal()
            .require(["done"])
            .compile()
            .expect("booking compiles")
    }

    #[test]
    fn compiles_to_a_governed_flow() {
        let convo = booking();
        // The commit tool is in the tool universe and gated.
        assert!(convo.flow().tool_policy().tools.contains("book"));
        assert_eq!(convo.flow().flow().steps.len(), 4);
    }

    #[test]
    fn lowered_flow_enforces_stage_order_and_commit() {
        let convo = booking();
        let mut mon = convo.monitor(Enforcement::Enforce);
        let state = State::new();

        // First stage active; book is blocked (not allowed here + not confirmed).
        let ex = mon.explain(&state);
        assert!(ex.active.contains(&"collect".to_string()));
        assert!(ex.blocked_tools.contains_key("book"));

        // Collect the slots → collect completes, check activates.
        let _ = state.set("party_size", 4u8);
        let _ = state.set("slot", "tomorrow 7pm");
        mon.on_turn(&state);
        assert!(mon.explain(&state).active.contains(&"check".to_string()));

        // Availability → confirm activates; book still needs confirmation.
        let _ = state.set("availability_ok", true);
        mon.on_turn(&state);
        assert!(mon.admits_tool("book", &state).is_err());

        // Confirm → book admitted; calling it completes the conversation.
        let _ = state.set("user_confirmed", true);
        assert!(mon.admits_tool("book", &state).is_ok());
        mon.on_tool_ok("book", &state);
        mon.on_turn(&state);
        assert!(mon.is_complete());
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = booking().spec().clone();
        let json = serde_json::to_string(&spec).expect("serialize spec");
        let back: ConversationSpec = serde_json::from_str(&json).expect("deserialize spec");
        let recompiled = Conversation::from_spec(back).expect("recompile from spec");
        assert_eq!(recompiled.flow().flow().steps.len(), 4);
    }

    #[test]
    fn collect_frame_uses_frame_slot_keys() {
        use gemini_adk_rs::frame::{Frame, FrameSpec, SlotSpec};

        struct Booking;
        impl Frame for Booking {
            fn frame() -> FrameSpec {
                FrameSpec {
                    name: "booking".into(),
                    slots: vec![SlotSpec::new("party_size"), SlotSpec::new("slot")],
                }
            }
        }

        let convo = Conversation::new("b")
            .stage("collect")
            .collect_frame::<Booking>()
            .next("done", Guard::captured(["party_size", "slot"]))
            .stage("done")
            .terminal()
            .compile()
            .expect("compiles");

        // The collect stage completes on the frame's slots being captured.
        let mut mon = convo.monitor(Enforcement::Enforce);
        let state = State::new();
        assert!(mon.explain(&state).active.contains(&"collect".to_string()));
        let _ = state.set("party_size", 2u8);
        let _ = state.set("slot", "noon");
        mon.on_turn(&state);
        // Frame slots captured -> collect completes and the (terminal) done latches.
        assert!(mon.marking().done.contains("collect"));
        assert!(mon.marking().done.contains("done"));
    }

    #[tokio::test]
    async fn collect_frame_extractor_fills_and_scores_slots() {
        use gemini_adk_rs::frame::{Frame, FrameSpec, SlotRecognizer, SlotSpec};
        use gemini_adk_rs::live::TranscriptTurn;

        struct Order;
        impl Frame for Order {
            fn frame() -> FrameSpec {
                FrameSpec {
                    name: "order".into(),
                    slots: vec![SlotSpec {
                        recognizer: Some(SlotRecognizer::OneOf(vec![
                            "pizza".into(),
                            "salad".into(),
                        ])),
                        ..SlotSpec::new("item")
                    }],
                }
            }
        }

        let convo = Conversation::new("o")
            .stage("collect")
            .collect_frame::<Order>()
            .next("done", Guard::captured(["item"]))
            .stage("done")
            .terminal()
            .compile()
            .expect("compiles");

        // The frame lowered to exactly one extractor.
        assert_eq!(convo.extractors().len(), 1);
        let extractor = convo.extractors()[0].clone().into_extractor();

        // Run it over a transcript turn against State — it fills the slot and
        // records confidence under `state_meta:` for evidence.
        let state = State::new();
        let window = vec![TranscriptTurn {
            turn_number: 0,
            user: "I'd like a large PIZZA".into(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }];
        let out = extractor.extract_with_state(&window, &state).await.unwrap();
        assert_eq!(out.get("item").and_then(|v| v.as_str()), Some("pizza"));

        let ev = state.evidence("item");
        assert_eq!(ev.source.as_deref(), Some("extraction"));
        assert!(ev.confidence.unwrap() > 0.0);
    }

    #[test]
    fn rejects_transition_to_unknown_stage() {
        let err = Conversation::new("x")
            .stage("a")
            .next("ghost", Guard::always())
            .stage("b")
            .terminal()
            .compile()
            .expect_err("unknown target must fail");
        assert!(matches!(err, ConversationError::Spec(_)));
    }

    #[test]
    fn rejects_unguarded_commit_via_flow_compile() {
        // commit guarded by Always is an unguarded commit — Flow::compile rejects it.
        let err = Conversation::new("x")
            .stage("s")
            .commit("pay", Guard::always())
            .done(Guard::called_ok("pay"))
            .next("done", Guard::called_ok("pay"))
            .stage("done")
            .terminal()
            .compile()
            .expect_err("unguarded commit must fail");
        assert!(matches!(err, ConversationError::Compile(_)));
    }
}
