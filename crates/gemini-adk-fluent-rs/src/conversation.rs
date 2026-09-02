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
//! let mut monitor = convo.monitor(Enforcement::Enforce);
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
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gemini_adk_rs::extract::Extract;
use gemini_adk_rs::flow::{
    CompiledFlow, Enforcement, Flow, FlowErrors, FlowExplanation, FlowMonitor, Guard, Pred,
};
use gemini_adk_rs::frame::{Frame, FrameSpec};
use gemini_adk_rs::state::State;

/// A boxed async fetcher: bind args (a JSON object) → resolved value.
type SlotFetch =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// A registry of named async resolvers, bound to a [`ConversationSpec`]'s
/// declared resolver slots at load time via
/// [`Conversation::from_spec_with_resolvers`].
///
/// This is what makes a JSON spec a complete deployable unit: the spec carries
/// the *declarations* (slot, resolver name, args, ttl) and the registry supplies
/// the *implementations*.
///
/// ```ignore
/// let registry = ResolverRegistry::new()
///     .with("availability", |args| async move {
///         Ok(serde_json::json!({ "open": true }))
///     });
/// let convo = Conversation::from_spec_with_resolvers(spec, &registry)?;
/// ```
#[derive(Clone, Default)]
pub struct ResolverRegistry {
    fetchers: BTreeMap<String, SlotFetch>,
}

impl ResolverRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an async resolver under `name` (builder style).
    pub fn with<F, Fut>(mut self, name: impl Into<String>, fetch: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.add(name, fetch);
        self
    }

    /// Register an async resolver under `name`.
    pub fn add<F, Fut>(&mut self, name: impl Into<String>, fetch: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let fetch = Arc::new(fetch);
        self.fetchers.insert(
            name.into(),
            Arc::new(move |v| {
                let fetch = fetch.clone();
                Box::pin(async move { fetch(v).await })
            }),
        );
    }

    /// Whether a resolver named `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.fetchers.contains_key(name)
    }

    /// A registry that stubs every resolver declared anywhere in `spec` (stages
    /// and overlays) with a no-op returning JSON `null`.
    ///
    /// For **structural validation and deterministic simulation**, the resolver
    /// *implementations* are irrelevant — a resolver is an external fetch, so in
    /// a model-free test its output is supplied via a scenario `set` step (like
    /// a tool result via `tool_ok`), and the stub is never actually invoked when
    /// the slot is pre-set. Real implementations bind at deploy time via
    /// [`Conversation::from_spec_with_resolvers`].
    pub fn stubbing(spec: &ConversationSpec) -> Self {
        let mut reg = Self::new();
        let stages = spec
            .stages
            .iter()
            .chain(spec.overlays.iter().flat_map(|o| o.stages.iter()));
        for stage in stages {
            for r in &stage.resolve {
                let name = r.resolver_name().to_string();
                if !reg.contains(&name) {
                    reg.add(name, |_args| async { Ok(serde_json::Value::Null) });
                }
            }
        }
        reg
    }

    fn get(&self, name: &str) -> Option<SlotFetch> {
        self.fetchers.get(name).cloned()
    }
}

/// A resolver binding attached to a stage by [`Conversation::resolve_slot`]. The
/// closure lives only in the builder (it is not serializable); the serializable
/// [`ConversationSpec`] is unaffected.
#[derive(Clone)]
struct StageResolver {
    stage: String,
    name: String,
    args: Vec<String>,
    ttl: Option<Duration>,
    fetch: SlotFetch,
}

/// A serializable declaration that a slot is filled by a *named* async resolver.
///
/// The resolver's implementation (the async fetch) is bound at load time from a
/// [`ResolverRegistry`] — so a `ConversationSpec` carrying these is a complete,
/// JSON-deployable unit once paired with a registry. This is the data half of
/// [`Conversation::resolve_slot`]; the closure half lives only in the builder.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResolveSpec {
    /// The slot (state key) the resolver fills. Added to the stage's `collect`.
    pub slot: String,
    /// The registered resolver name to bind (defaults to `slot` if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,
    /// State keys passed to the resolver as a JSON object argument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Optional memoization TTL in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

impl ResolveSpec {
    /// The resolver name to look up in the registry (the explicit `resolver`, or
    /// the slot name as a default).
    fn resolver_name(&self) -> &str {
        self.resolver.as_deref().unwrap_or(&self.slot)
    }
}

/// A transition: advance to `to` when `when` holds.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransitionSpec {
    /// Target stage id.
    pub to: String,
    /// The (serializable) guard that fires this transition.
    pub when: Guard,
}

/// A confirm-before-act tool, gated by `when`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommitSpec {
    /// The committing tool (e.g. `book`, `charge_card`).
    pub tool: String,
    /// The guard that must hold before the tool is admitted.
    pub when: Guard,
}

/// One authored conversation stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// Repair policy for this stage (reprompt/escalate on stalling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<RepairPolicy>,
    /// Named-resolver slot declarations. Bound to implementations at load via a
    /// [`ResolverRegistry`]; the data lives in the spec so it round-trips JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolve: Vec<ResolveSpec>,
}

/// The state key set when a stage's repair policy escalates.
fn escalate_flag(stage: &str) -> String {
    format!("repair:{stage}:escalate")
}

/// The state key set when a stage's repair policy raises a reprompt.
fn reprompt_flag(stage: &str) -> String {
    format!("repair:{stage}:reprompt")
}

/// How the main flow continues after a digression (overlay) completes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Resume {
    /// Resume the main flow exactly where it was suspended (history state).
    #[default]
    Previous,
    /// Re-enter the main flow from its start.
    Restart,
    /// End the conversation (e.g. a cancel/handoff digression).
    Terminate,
}

fn default_reprompt_after() -> u32 {
    2
}
fn default_escalate_after() -> u32 {
    4
}

/// A stage's repair policy for the weird paths (silence, no-match, the user
/// stalling). The runtime sets `repair:{stage}:reprompt` once the stage has been
/// active `reprompt_after` turns without completing, and `repair:{stage}:escalate`
/// after `escalate_after`. When `escalate_to` is set, escalation also *completes*
/// the stage and routes to that stage — a deterministic "give up and hand off".
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepairPolicy {
    /// Turns the stage may be active before a reprompt signal is raised.
    #[serde(default = "default_reprompt_after")]
    pub reprompt_after: u32,
    /// Turns the stage may be active before an escalation signal is raised.
    #[serde(default = "default_escalate_after")]
    pub escalate_after: u32,
    /// Stage to route to on escalation (also completes the current stage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_to: Option<String>,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            reprompt_after: default_reprompt_after(),
            escalate_after: default_escalate_after(),
            escalate_to: None,
        }
    }
}

impl RepairPolicy {
    /// A policy with the given reprompt/escalate turn thresholds.
    pub fn new(reprompt_after: u32, escalate_after: u32) -> Self {
        Self {
            reprompt_after,
            escalate_after,
            escalate_to: None,
        }
    }

    /// Route to `stage` on escalation (also completes the current stage).
    pub fn escalate_to(mut self, stage: impl Into<String>) -> Self {
        self.escalate_to = Some(stage.into());
        self
    }
}

/// A digression (overlay): a named sub-flow that suspends the main flow when its
/// `trigger` holds, runs to completion, then resumes per `resume`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OverlaySpec {
    /// Overlay name.
    pub name: String,
    /// The guard that activates this overlay (e.g. an `intent:*` flag).
    pub trigger: Guard,
    /// The overlay's own stages.
    #[serde(default)]
    pub stages: Vec<StageSpec>,
    /// Stages required for the overlay to be considered complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<String>,
    /// What the main flow does once the overlay completes.
    #[serde(default)]
    pub resume: Resume,
}

/// The serializable authoring spec — the single source of truth from which the
/// typed builder, YAML, and (later) codegen all derive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationSpec {
    /// Conversation name.
    pub name: String,
    /// The authored stages.
    #[serde(default)]
    pub stages: Vec<StageSpec>,
    /// Stages that must be done for the conversation to be complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<String>,
    /// Digressions/overlays that can suspend and resume the main flow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<OverlaySpec>,
    /// Cross-cutting policy aspects (safety/redaction/commit governance).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<crate::policy::Policy>,
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

/// The JSON Schema for a [`ConversationSpec`], as a pretty-printed string.
///
/// This is the machine-readable authoring contract: a web form, an IDE, or an
/// LLM/skill drafting a spec targets this schema. Generated from the same
/// `#[derive(JsonSchema)]` types the runtime compiles, so it cannot drift.
pub fn conversation_spec_schema() -> String {
    let schema = schemars::schema_for!(ConversationSpec);
    serde_json::to_string_pretty(&schema).expect("schema serialization is infallible")
}

impl serde::Serialize for ConversationError {
    /// A machine-readable diagnostic so authoring tools (web/CLI/skills) can
    /// render structured errors. Shape:
    /// `{ "kind": "compile", "errors": [ { "kind": "unreachable_step", ... } ] }`.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("message", &self.to_string())?;
        match self {
            ConversationError::Empty => {
                m.serialize_entry("kind", "empty")?;
            }
            ConversationError::Spec(msg) => {
                m.serialize_entry("kind", "spec")?;
                m.serialize_entry("detail", msg)?;
            }
            ConversationError::Flow(errs) => {
                m.serialize_entry("kind", "flow")?;
                m.serialize_entry("errors", errs)?;
            }
            ConversationError::Compile(e) => {
                m.serialize_entry("kind", "compile")?;
                m.serialize_entry("errors", &e.0)?;
            }
        }
        m.end()
    }
}

/// A compiled digression: its trigger, lowered flow, extractors, and resume policy.
#[derive(Clone)]
pub struct CompiledOverlay {
    /// Overlay name.
    pub name: String,
    /// Guard that activates the overlay.
    pub trigger: Guard,
    /// The overlay's lowered governance flow.
    pub flow: CompiledFlow,
    /// Extractors that fill the overlay's frame slots.
    pub extractors: Vec<Extract>,
    /// What the main flow does once this overlay completes.
    pub resume: Resume,
}

/// A compiled conversation: the validated main [`CompiledFlow`], the extractors
/// that fill its frames' slots, any digressions, and the source spec.
#[derive(Clone)]
pub struct CompiledConversation {
    flow: CompiledFlow,
    extractors: Vec<Extract>,
    overlays: Vec<CompiledOverlay>,
    repair: BTreeMap<String, RepairPolicy>,
    policies: Vec<crate::policy::Policy>,
    spec: ConversationSpec,
}

// Manual: the lowered `Extract`s hold recognizer/resolver closures (not `Debug`),
// so they are summarized by count.
impl std::fmt::Debug for CompiledConversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledConversation")
            .field("flow", &self.flow)
            .field("extractors", &self.extractors.len())
            .field("overlays", &self.overlays.len())
            .field("policies", &self.policies)
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
    /// The compiled digressions/overlays.
    pub fn overlays(&self) -> &[CompiledOverlay] {
        &self.overlays
    }
    /// The cross-cutting policy aspects attached to this conversation.
    pub fn policies(&self) -> &[crate::policy::Policy] {
        &self.policies
    }
    /// The set of state keys marked for redaction by `Policy::redact`.
    pub fn redacted_fields(&self) -> BTreeSet<String> {
        self.policies
            .iter()
            .flat_map(|p| p.redacted_keys().iter().cloned())
            .collect()
    }
    /// Every extractor (main + overlays) — what [`Live::converse`](crate::live::Live)
    /// registers so slots fill whether the main flow or a digression is active.
    pub fn all_extractors(&self) -> Vec<Extract> {
        let mut all = self.extractors.clone();
        for ov in &self.overlays {
            all.extend(ov.extractors.iter().cloned());
        }
        all
    }
    /// Build the runtime [`FlowStack`] — the main flow plus its digressions, with
    /// push-on-trigger / resume-on-completion.
    pub fn stack(&self, mode: Enforcement) -> FlowStack {
        FlowStack::new(self, mode)
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
///
/// (Not `Debug`: [`resolve_slot`](Conversation::resolve_slot) bindings hold async
/// closures. The serializable [`ConversationSpec`] is `Debug` via [`spec`](Conversation::spec).)
#[derive(Clone, Default)]
pub struct Conversation {
    spec: ConversationSpec,
    resolvers: Vec<StageResolver>,
    /// When `Some(i)`, stage setters target `spec.overlays[i]` instead of the main
    /// flow (between `.overlay(..)` and `.done_overlay()`).
    current_overlay: Option<usize>,
}

impl Conversation {
    /// Start a new conversation.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            spec: ConversationSpec {
                name: name.into(),
                ..Default::default()
            },
            resolvers: Vec::new(),
            current_overlay: None,
        }
    }

    /// Begin authoring a new stage; subsequent setters apply to it. Routes to the
    /// active overlay when between `.overlay(..)` and `.done_overlay()`.
    pub fn stage(mut self, id: impl Into<String>) -> Self {
        let stage = StageSpec {
            id: id.into(),
            ..Default::default()
        };
        match self.current_overlay {
            Some(i) => self.spec.overlays[i].stages.push(stage),
            None => self.spec.stages.push(stage),
        }
        self
    }

    /// Begin authoring a digression/overlay; subsequent `.stage(..)` calls (until
    /// `.done_overlay()`) populate it. Set its activation guard with `.trigger(..)`
    /// (an overlay with no trigger never fires — fail-closed).
    pub fn overlay(mut self, name: impl Into<String>) -> Self {
        self.spec.overlays.push(OverlaySpec {
            name: name.into(),
            // Fail-closed default: never triggers until `.trigger(..)` is set.
            trigger: Guard::is_true("__overlay_never_triggers__"),
            stages: Vec::new(),
            require: Vec::new(),
            resume: Resume::Previous,
        });
        self.current_overlay = Some(self.spec.overlays.len() - 1);
        self
    }

    /// Set the activation guard of the overlay currently being authored.
    pub fn trigger(mut self, guard: Guard) -> Self {
        if let Some(i) = self.current_overlay {
            self.spec.overlays[i].trigger = guard;
        }
        self
    }

    /// Set the resume policy of the overlay currently being authored.
    pub fn resume(mut self, resume: Resume) -> Self {
        if let Some(i) = self.current_overlay {
            self.spec.overlays[i].resume = resume;
        }
        self
    }

    /// Finish the current overlay; subsequent `.stage(..)` calls target the main
    /// flow again.
    pub fn done_overlay(mut self) -> Self {
        self.current_overlay = None;
        self
    }

    /// Append a pre-built [`StageSpec`] (e.g. from a [`Motif`](crate::motifs::Motif)).
    /// Routes to the active overlay when authoring one, else the main flow.
    /// Subsequent stage setters (`next`, `say`, …) apply to it.
    pub fn add_stage(mut self, stage: StageSpec) -> Self {
        match self.current_overlay {
            Some(i) => self.spec.overlays[i].stages.push(stage),
            None => self.spec.stages.push(stage),
        }
        self
    }

    /// Append a pre-built [`OverlaySpec`] (e.g. a `Motif::faq_digression`). Leaves
    /// overlay-authoring mode (the overlay is already complete).
    pub fn add_overlay(mut self, overlay: OverlaySpec) -> Self {
        self.spec.overlays.push(overlay);
        self.current_overlay = None;
        self
    }

    /// Attach a cross-cutting [`Policy`](crate::policy::Policy) aspect.
    pub fn policy(mut self, policy: impl Into<crate::policy::Policy>) -> Self {
        self.spec.policies.push(policy.into());
        self
    }

    fn current(&mut self) -> &mut StageSpec {
        let stages = match self.current_overlay {
            Some(i) => &mut self.spec.overlays[i].stages,
            None => &mut self.spec.stages,
        };
        stages
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

    /// Collect the slots of a typed [`gemini_adk_rs::frame::Frame`] in this
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

    /// Fill a slot in the current stage from an **async resolver** — a tool call,
    /// HTTP fetch, MCP request, or agent. `args` names the `State` keys bound into
    /// the JSON object passed to `fetch`; the returned value fills `name`. With a
    /// `ttl`, results are memoized by `(field, canonical args)`.
    ///
    /// The slot is added to the stage's `collect`, so its `captured` completion
    /// waits for the resolution. The closure lives only in the builder; the
    /// serializable spec is unaffected.
    pub fn resolve_slot<I, S, F, Fut>(
        mut self,
        name: impl Into<String>,
        args: I,
        ttl: Option<Duration>,
        fetch: F,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let name = name.into();
        let stage = self.current().id.clone();
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        if !self.current().collect.contains(&name) {
            self.current().collect.push(name.clone());
        }
        // Record the serializable declaration too, so `into_spec()` carries the
        // resolver and the spec can later be re-bound from a `ResolverRegistry`.
        self.current().resolve.push(ResolveSpec {
            slot: name.clone(),
            resolver: None,
            args: args.clone(),
            ttl_secs: ttl.map(|d| d.as_secs()),
        });
        let fetch = Arc::new(fetch);
        self.resolvers.push(StageResolver {
            stage,
            name,
            args,
            ttl,
            fetch: Arc::new(move |v| {
                let fetch = fetch.clone();
                Box::pin(async move { fetch(v).await })
            }),
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

    /// Attach a [`RepairPolicy`] to the current stage.
    pub fn repair(mut self, policy: RepairPolicy) -> Self {
        self.current().repair = Some(policy);
        self
    }

    /// Require these stages for completion (lowers to a Flow `require`). Targets
    /// the active overlay when authoring one, else the main flow.
    pub fn require<I, S>(mut self, steps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let req: Vec<String> = steps.into_iter().map(Into::into).collect();
        match self.current_overlay {
            Some(i) => self.spec.overlays[i].require = req,
            None => self.spec.require = req,
        }
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

    /// Compile from a [`ConversationSpec`] (e.g. parsed from JSON/YAML).
    ///
    /// If the spec declares any named-resolver slots (`stage.resolve`), this
    /// errors — those slots would be collected but never filled. Use
    /// [`Conversation::from_spec_with_resolvers`] to bind them.
    pub fn from_spec(spec: ConversationSpec) -> Result<CompiledConversation, ConversationError> {
        Self::from_spec_with_resolvers(spec, &ResolverRegistry::new())
    }

    /// Compile a [`ConversationSpec`] with its declared resolvers **stubbed**
    /// (each returns JSON `null`) — for structural validation, model-free
    /// simulation, and CI, where resolver outputs are supplied by scenario
    /// `set` steps rather than live fetches.
    ///
    /// Use this (not [`from_spec`](Self::from_spec)) when compiling a spec from
    /// untrusted JSON for testing/authoring; use
    /// [`from_spec_with_resolvers`](Self::from_spec_with_resolvers) at deploy
    /// time to bind real implementations.
    pub fn from_spec_stubbing_resolvers(
        spec: ConversationSpec,
    ) -> Result<CompiledConversation, ConversationError> {
        let registry = ResolverRegistry::stubbing(&spec);
        Self::from_spec_with_resolvers(spec, &registry)
    }

    /// Compile a [`ConversationSpec`] and bind its declared named-resolver slots
    /// from `registry`. This makes a JSON spec + a resolver registry a complete
    /// deployable unit.
    ///
    /// Errors if the spec references a resolver name absent from `registry`.
    pub fn from_spec_with_resolvers(
        spec: ConversationSpec,
        registry: &ResolverRegistry,
    ) -> Result<CompiledConversation, ConversationError> {
        let mut resolvers = Vec::new();
        let stages = spec
            .stages
            .iter()
            .chain(spec.overlays.iter().flat_map(|o| o.stages.iter()));
        for stage in stages {
            for r in &stage.resolve {
                let fetch = registry.get(r.resolver_name()).ok_or_else(|| {
                    ConversationError::Spec(format!(
                        "stage '{}' slot '{}' needs resolver '{}', which is not in the registry",
                        stage.id,
                        r.slot,
                        r.resolver_name()
                    ))
                })?;
                resolvers.push(StageResolver {
                    stage: stage.id.clone(),
                    name: r.slot.clone(),
                    args: r.args.clone(),
                    ttl: r.ttl_secs.map(Duration::from_secs),
                    fetch,
                });
            }
        }
        compile_spec(spec, resolvers)
    }

    /// Lower and validate into a [`CompiledConversation`].
    pub fn compile(self) -> Result<CompiledConversation, ConversationError> {
        compile_spec(self.spec, self.resolvers)
    }
}

impl crate::live::Live {
    /// Drive a [`Live`](crate::live::Live) session from a compiled conversation:
    /// **govern** with its lowered flow and **register** the extractors that fill
    /// its frames' slots each turn. The one-liner entrypoint for "run this
    /// conversation".
    ///
    /// ```ignore
    /// let convo = Conversation::new("booking")./* … */.compile()?;
    /// let handle = Live::builder()
    ///     .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
    ///     .converse(&convo)
    ///     .connect_from_env()
    ///     .await?;
    /// ```
    pub fn converse(self, convo: &CompiledConversation) -> Self {
        let mut live = self.govern_compiled(convo.flow().clone());
        for extract in convo.all_extractors() {
            live = live.extract_record(extract);
        }
        live
    }

    /// Like [`converse`](Self::converse) but attaches the flow in **observe** mode
    /// (nothing blocked; deviations recorded) while still registering extractors.
    pub fn converse_observe(self, convo: &CompiledConversation) -> Self {
        let mut live = self.observe_compiled(convo.flow().clone());
        for extract in convo.all_extractors() {
            live = live.extract_record(extract);
        }
        live
    }
}

/// A digression currently suspending the main flow.
struct ActiveOverlay {
    name: String,
    monitor: FlowMonitor,
    resume: Resume,
}

/// The runtime above the DAG: the main flow plus its digressions, with
/// push-on-trigger and resume-on-completion (MVP: nesting depth 1).
///
/// While a digression is active, governance — tool admission, postures/grounds,
/// `explain()` — delegates to the **active** layer, and the main flow's marking is
/// untouched, so [`Resume::Previous`] resumes exactly where it left off. Driven by
/// `State`/guards (model-free, deterministic).
pub struct FlowStack {
    main_flow: CompiledFlow,
    main: FlowMonitor,
    mode: Enforcement,
    overlays: Vec<CompiledOverlay>,
    active: Option<ActiveOverlay>,
    terminated: bool,
    /// Per-main-stage repair policies.
    repair: BTreeMap<String, RepairPolicy>,
    /// Consecutive turns each main stage has been active without completing.
    active_turns: BTreeMap<String, u32>,
}

impl FlowStack {
    fn new(convo: &CompiledConversation, mode: Enforcement) -> Self {
        Self {
            main_flow: convo.flow.clone(),
            main: FlowMonitor::compiled(convo.flow.clone(), mode),
            mode,
            overlays: convo.overlays.clone(),
            active: None,
            terminated: false,
            repair: convo.repair.clone(),
            active_turns: BTreeMap::new(),
        }
    }

    /// Bump per-stage active-turn counters for the main flow and raise repair
    /// signals (`repair:{stage}:reprompt` / `:escalate`) when thresholds are hit.
    /// Clears signals for stages that are no longer active.
    fn apply_repair(&mut self, state: &State) {
        if self.repair.is_empty() {
            return;
        }
        let active: BTreeSet<String> = self.main.explain(state).active.into_iter().collect();
        // Reset stages that left active since last turn.
        let left: Vec<String> = self
            .active_turns
            .keys()
            .filter(|k| !active.contains(*k))
            .cloned()
            .collect();
        for stage in left {
            self.active_turns.remove(&stage);
            let _ = state.set(reprompt_flag(&stage), false);
            let _ = state.set(escalate_flag(&stage), false);
        }
        for stage in &active {
            let count = self.active_turns.entry(stage.clone()).or_insert(0);
            *count += 1;
            if let Some(rp) = self.repair.get(stage) {
                if *count >= rp.reprompt_after {
                    let _ = state.set(reprompt_flag(stage), true);
                }
                if *count >= rp.escalate_after {
                    let _ = state.set(escalate_flag(stage), true);
                }
            }
        }
    }

    /// The monitor currently driving — the active overlay if any, else the main flow.
    pub fn current(&self) -> &FlowMonitor {
        self.active.as_ref().map_or(&self.main, |a| &a.monitor)
    }

    /// The name of the active digression, if one is suspending the main flow.
    pub fn active_overlay(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.name.as_str())
    }

    /// Whether the conversation is finished (main complete, or a `Terminate`
    /// digression ran).
    pub fn is_complete(&self) -> bool {
        self.terminated || (self.active.is_none() && self.main.is_complete())
    }

    /// Index of the first overlay whose trigger holds against the main context.
    fn triggered(&self, state: &State) -> Option<usize> {
        self.overlays
            .iter()
            .position(|ov| self.main.eval(&ov.trigger, state))
    }

    /// Advance one turn. Enters a triggered digression (suspending the main flow),
    /// advances an active digression and resumes when it completes, or advances the
    /// main flow.
    pub fn on_turn(&mut self, state: &State) {
        if self.terminated {
            return;
        }
        match &mut self.active {
            Some(active) => {
                active.monitor.on_turn(state);
                if active.monitor.is_complete() {
                    let resume = active.resume;
                    self.active = None;
                    match resume {
                        // Main marking was untouched while suspended — nothing to do.
                        Resume::Previous => {}
                        Resume::Restart => {
                            self.main = FlowMonitor::compiled(self.main_flow.clone(), self.mode);
                        }
                        Resume::Terminate => self.terminated = true,
                    }
                }
            }
            None => {
                if let Some(idx) = self.triggered(state) {
                    let ov = &self.overlays[idx];
                    let mut monitor = FlowMonitor::compiled(ov.flow.clone(), self.mode);
                    // Drive the digression's first turn so single-stage overlays can latch.
                    monitor.on_turn(state);
                    if monitor.is_complete() {
                        match ov.resume {
                            Resume::Previous => {}
                            Resume::Restart => {
                                self.main =
                                    FlowMonitor::compiled(self.main_flow.clone(), self.mode);
                            }
                            Resume::Terminate => self.terminated = true,
                        }
                    } else {
                        self.active = Some(ActiveOverlay {
                            name: ov.name.clone(),
                            monitor,
                            resume: ov.resume,
                        });
                    }
                } else {
                    // Repair bookkeeping is based on the pre-turn active set so
                    // an escalation signal can take effect this turn.
                    self.apply_repair(state);
                    self.main.on_turn(state);
                }
            }
        }
    }

    /// Record a successful tool call against the active layer.
    pub fn on_tool_ok(&mut self, tool: &str, state: &State) {
        match &mut self.active {
            Some(active) => active.monitor.on_tool_ok(tool, state),
            None => self.main.on_tool_ok(tool, state),
        }
    }

    /// Whether `tool` is admitted right now (delegates to the active layer).
    pub fn admits_tool(&self, tool: &str, state: &State) -> Result<(), String> {
        self.current().admits_tool(tool, state)
    }

    /// Explain the active layer's control-plane state.
    pub fn explain(&self, state: &State) -> FlowExplanation {
        self.current().explain(state)
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

/// Lower a set of stages (the main flow or an overlay) into a [`CompiledFlow`],
/// with conversation-level referential checks.
fn lower_flow(stages: &[StageSpec], require: &[String]) -> Result<CompiledFlow, ConversationError> {
    if stages.is_empty() {
        return Err(ConversationError::Empty);
    }
    let ids: BTreeSet<&str> = stages.iter().map(|s| s.id.as_str()).collect();
    if ids.len() != stages.len() {
        return Err(ConversationError::Spec("duplicate stage ids".into()));
    }
    for s in stages {
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
        if let Some(target) = s.repair.as_ref().and_then(|r| r.escalate_to.as_ref())
            && !ids.contains(target.as_str())
        {
            return Err(ConversationError::Spec(format!(
                "stage '{}' escalates to unknown stage '{}'",
                s.id, target
            )));
        }
    }
    for r in require {
        if !ids.contains(r.as_str()) {
            return Err(ConversationError::Spec(format!(
                "require references unknown stage '{r}'"
            )));
        }
    }

    // Incoming edges: target -> [(source, when)].
    let mut incoming: BTreeMap<&str, Vec<(&str, Guard)>> = BTreeMap::new();
    for s in stages {
        for t in &s.next {
            incoming
                .entry(t.to.as_str())
                .or_default()
                .push((s.id.as_str(), t.when.clone()));
        }
        // Repair escalation is an extra edge gated on the escalate signal.
        if let Some(target) = s.repair.as_ref().and_then(|r| r.escalate_to.as_ref()) {
            incoming
                .entry(target.as_str())
                .or_default()
                .push((s.id.as_str(), Guard::is_true(escalate_flag(&s.id))));
        }
    }

    let mut fb = Flow::new();
    for s in stages {
        fb = fb.step(&s.id);

        let mut deps: BTreeSet<&str> = s.after.iter().map(String::as_str).collect();
        if let Some(inc) = incoming.get(s.id.as_str()) {
            for (src, _) in inc {
                deps.insert(src);
            }
        }
        for d in deps {
            fb = fb.after(d);
        }

        if let Some(inc) = incoming.get(s.id.as_str())
            && let Some(gate) = any_of(inc.iter().map(|(_, w)| w.clone()).collect())
        {
            fb = fb.gate(gate);
        }

        if let Some(say) = &s.say {
            fb = fb.posture(say.clone());
        }
        if let Some(ground) = &s.ground {
            fb = fb.ground(ground.clone());
        }

        let mut allow: Vec<String> = s.allow.clone();
        if let Some(c) = &s.commit
            && !allow.contains(&c.tool)
        {
            allow.push(c.tool.clone());
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

    if !require.is_empty() {
        fb = fb.require(require.to_vec());
    }

    let flow = fb.build().map_err(|e| ConversationError::Flow(e.issues))?;
    flow.compile().map_err(ConversationError::Compile)
}

/// The extractors lowered from a stage list's `collect_frame` frames.
fn frame_extractors(stages: &[StageSpec]) -> Vec<Extract> {
    stages
        .iter()
        .filter_map(|s| s.frame.as_ref().and_then(FrameSpec::to_extract))
        .collect()
}

fn compile_spec(
    mut spec: ConversationSpec,
    resolvers: Vec<StageResolver>,
) -> Result<CompiledConversation, ConversationError> {
    // Apply cross-cutting policies. SafetyHandoff lowers to a `safety` digression
    // (terminate on intent); Redact/Commit are carried for the runtime.
    for policy in spec.policies.clone() {
        if let crate::policy::Policy::SafetyHandoff { intents } = policy
            && let Some(trigger) = any_of(
                intents
                    .iter()
                    .map(|i| Guard::is_true(format!("intent:{i}")))
                    .collect(),
            )
        {
            spec.overlays.push(OverlaySpec {
                name: "safety".into(),
                trigger,
                stages: vec![StageSpec {
                    id: "safety_handoff".into(),
                    say: Some("Safety concern detected — hand off to a human now.".into()),
                    terminal: true,
                    ..Default::default()
                }],
                require: Vec::new(),
                resume: Resume::Terminate,
            });
        }
    }

    // A declared resolver slot behaves like a collected slot: the stage's
    // implicit `captured` completion must wait for the resolution. The builder
    // path (`resolve_slot`) adds it eagerly; specs parsed from JSON get the
    // same normalization here, before lowering.
    for stage in spec
        .stages
        .iter_mut()
        .chain(spec.overlays.iter_mut().flat_map(|o| o.stages.iter_mut()))
    {
        for slot in stage
            .resolve
            .iter()
            .map(|r| r.slot.clone())
            .collect::<Vec<_>>()
        {
            if !stage.collect.contains(&slot) {
                stage.collect.push(slot);
            }
        }
    }

    // Main flow.
    let flow = lower_flow(&spec.stages, &spec.require)?;

    // Resolver bindings must reference a known stage — main or overlay. For a
    // stage id that appears in both, the main flow wins (ids are expected to be
    // globally unique across a spec).
    let ids: BTreeSet<&str> = spec.stages.iter().map(|s| s.id.as_str()).collect();
    let mut overlay_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, ov) in spec.overlays.iter().enumerate() {
        for s in &ov.stages {
            overlay_of.entry(s.id.as_str()).or_insert(i);
        }
    }
    for r in &resolvers {
        if !ids.contains(r.stage.as_str()) && !overlay_of.contains_key(r.stage.as_str()) {
            return Err(ConversationError::Spec(format!(
                "resolver for slot '{}' references unknown stage '{}'",
                r.name, r.stage
            )));
        }
    }

    // Group resolver bindings per stage, routed to the main flow or the owning
    // overlay so overlay-declared resolvers actually fill their slots.
    let build_resolver_extractor = |stage: &str, binds: &[&StageResolver]| {
        let mut builder = Extract::record(format!("{}__{}_resolve", spec.name, stage));
        for r in binds {
            let fetch = r.fetch.clone();
            builder = builder.field_resolve(r.name.clone(), r.args.clone(), r.ttl, move |args| {
                let fetch = fetch.clone();
                async move { fetch(args).await }
            });
        }
        builder.build()
    };
    let mut by_stage: BTreeMap<&str, Vec<&StageResolver>> = BTreeMap::new();
    for r in &resolvers {
        by_stage.entry(r.stage.as_str()).or_default().push(r);
    }
    let mut overlay_extractors: BTreeMap<usize, Vec<Extract>> = BTreeMap::new();

    // Main extractors: frame recognizers + resolver-slot bindings.
    let mut extractors = frame_extractors(&spec.stages);
    for (stage, binds) in &by_stage {
        let extractor = build_resolver_extractor(stage, binds);
        if ids.contains(stage) {
            extractors.push(extractor);
        } else if let Some(&i) = overlay_of.get(stage) {
            overlay_extractors.entry(i).or_default().push(extractor);
        }
    }

    // Overlays: each lowers to its own validated flow + extractors. An overlay
    // with no explicit `require` is complete when its terminal stages are done —
    // so completion is meaningful (without it, `is_complete()` is trivially true).
    let mut overlays = Vec::with_capacity(spec.overlays.len());
    for ov in &spec.overlays {
        let require = if ov.require.is_empty() {
            ov.stages
                .iter()
                .filter(|s| s.terminal)
                .map(|s| s.id.clone())
                .collect()
        } else {
            ov.require.clone()
        };
        let ov_flow = lower_flow(&ov.stages, &require)?;
        let mut ov_extractors = frame_extractors(&ov.stages);
        if let Some(bound) = overlay_extractors.remove(&overlays.len()) {
            ov_extractors.extend(bound);
        }
        overlays.push(CompiledOverlay {
            name: ov.name.clone(),
            trigger: ov.trigger.clone(),
            flow: ov_flow,
            extractors: ov_extractors,
            resume: ov.resume,
        });
    }

    // Per-stage repair policies for the runtime to apply.
    let repair = spec
        .stages
        .iter()
        .filter_map(|s| s.repair.clone().map(|p| (s.id.clone(), p)))
        .collect();

    let policies = spec.policies.clone();

    Ok(CompiledConversation {
        flow,
        extractors,
        overlays,
        repair,
        policies,
        spec,
    })
}

/// The completion guard for a non-terminal stage, by priority:
/// explicit `done` → `captured(collect)` → disjunction of `next` conditions.
/// When repair escalation is configured, the stage may also complete by escalating
/// (so a stalled stage can hand off even though its normal completion never fired).
fn stage_completion(s: &StageSpec) -> Option<Guard> {
    let base = if let Some(g) = &s.done {
        Some(g.clone())
    } else if !s.collect.is_empty() {
        Some(Guard::captured(s.collect.clone()))
    } else {
        any_of(s.next.iter().map(|t| t.when.clone()).collect())
    };
    if s.repair
        .as_ref()
        .and_then(|r| r.escalate_to.as_ref())
        .is_some()
    {
        let esc = Guard::is_true(escalate_flag(&s.id));
        return Some(match base {
            Some(b) => Guard::any(vec![b, esc]),
            None => esc,
        });
    }
    base
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
        assert!(convo.flow().tool_surface().tools.contains("book"));
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

    #[tokio::test]
    async fn named_resolver_spec_round_trips_and_binds_from_registry() {
        // A spec declaring a named-resolver slot (data only, no closure).
        let json = r#"
        {
          "name": "booking",
          "stages": [
            { "id": "check",
              "resolve": [{ "slot": "availability", "resolver": "avail", "args": ["party_size"] }],
              "next": [{ "to": "done", "when": { "captured": ["availability"] } }] },
            { "id": "done", "terminal": true }
          ],
          "require": ["done"]
        }
        "#;
        let spec: ConversationSpec = serde_json::from_str(json).expect("parse");
        // Round-trips losslessly.
        let reser = serde_json::to_string(&spec).unwrap();
        let back: ConversationSpec = serde_json::from_str(&reser).unwrap();
        assert_eq!(back.stages[0].resolve[0].resolver_name(), "avail");

        // Without a registry, an unbound resolver is a loud error (not a silently
        // unfillable slot).
        let err = Conversation::from_spec(back.clone()).expect_err("unbound resolver errors");
        assert!(matches!(err, ConversationError::Spec(m) if m.contains("avail")));

        // With a registry, it compiles and the resolver extractor is wired.
        let registry = ResolverRegistry::new().with("avail", |_args| async move {
            Ok(serde_json::json!({ "open": true }))
        });
        let convo =
            Conversation::from_spec_with_resolvers(back, &registry).expect("binds and compiles");
        // The resolver lowered to an extractor that fills the `availability` slot.
        assert!(
            convo.extractors().iter().any(|e| e
                .field_state_keys()
                .iter()
                .any(|(_, k)| k == "availability")),
            "a resolver extractor filling 'availability' was compiled in"
        );
    }

    #[tokio::test]
    async fn resolver_spec_is_validatable_and_simulatable_without_a_registry() {
        // The exact shape the CLI/Python/CI data-plane sees: a resolver slot
        // with no implementation available.
        let json = r#"{ "name": "r", "stages": [
            { "id": "check",
              "resolve": [{ "slot": "avail", "resolver": "lookup", "args": ["x"] }],
              "next": [{ "to": "done", "when": { "captured": ["avail"] } }] },
            { "id": "done", "terminal": true } ], "require": ["done"] }"#;
        let spec: ConversationSpec = serde_json::from_str(json).unwrap();

        // Strict path (no registry) errors — as it should at deploy time.
        assert!(Conversation::from_spec(spec.clone()).is_err());

        // Stubbing path compiles for structural validation + simulation.
        let convo = Conversation::from_spec_stubbing_resolvers(spec)
            .expect("stubbed resolvers compile for testing");

        // A scenario supplies the resolver's output via `set` (the stub fetch is
        // never invoked when the slot is pre-set), and the flow completes.
        use crate::simulation::Sim;
        let mut sim = Sim::new(&convo, gemini_adk_rs::flow::Enforcement::Enforce);
        sim.set("avail", serde_json::json!({ "open": true }));
        sim.turn();
        assert!(sim.is_complete(), "resolver slot supplied by set completes");
    }

    #[tokio::test]
    async fn overlay_resolver_binds_validates_and_fills_the_overlay() {
        // A resolver declared on an OVERLAY stage must be validated against the
        // registry and lowered into that overlay's extractors — not silently
        // ignored (which left the slot permanently unfilled at runtime).
        let json = r#"{ "name": "ov", "stages": [
            { "id": "main", "terminal": true } ],
          "require": ["main"],
          "overlays": [ { "name": "lookup", "trigger": { "is_true": "intent:lookup" },
            "stages": [
              { "id": "fetch", "resolve": [{ "slot": "balance", "resolver": "bal" }] },
              { "id": "ov_done", "terminal": true, "after": ["fetch"] } ] } ] }"#;
        let spec: ConversationSpec = serde_json::from_str(json).unwrap();

        // Unbound overlay resolver is a loud error, same as a main-stage one.
        let err = Conversation::from_spec(spec.clone()).expect_err("unbound overlay resolver");
        assert!(matches!(err, ConversationError::Spec(m) if m.contains("bal")));

        // Bound, it compiles and the extractor lands on the overlay.
        let registry =
            ResolverRegistry::new().with("bal", |_args| async move { Ok(serde_json::json!(42)) });
        let convo = Conversation::from_spec_with_resolvers(spec, &registry).expect("compiles");
        assert!(
            convo.overlays()[0]
                .extractors
                .iter()
                .any(|e| e.field_state_keys().iter().any(|(_, k)| k == "balance")),
            "the overlay carries the resolver extractor for 'balance'"
        );
    }

    #[tokio::test]
    async fn resolver_slot_counts_toward_stage_completion_from_json() {
        // A resolver-only stage from JSON must gain the slot in `collect` (the
        // builder path adds it eagerly), so its implicit captured completion
        // exists and lowering succeeds instead of failing with "no completion".
        let json = r#"{ "name": "r2", "stages": [
            { "id": "check", "resolve": [{ "slot": "avail", "resolver": "lookup" }] },
            { "id": "done", "terminal": true, "after": ["check"] } ],
          "require": ["done"] }"#;
        let spec: ConversationSpec = serde_json::from_str(json).unwrap();
        let convo =
            Conversation::from_spec_stubbing_resolvers(spec).expect("resolver-only stage compiles");
        assert!(
            convo.spec().stages[0]
                .collect
                .contains(&"avail".to_string()),
            "declared resolver slot normalized into collect"
        );
    }

    #[test]
    fn conversation_spec_schema_is_valid_json_with_expected_shape() {
        let schema_str = conversation_spec_schema();
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("schema is valid JSON");
        // The root describes ConversationSpec; its `stages` property must exist
        // (proves the transitive JsonSchema derives wired through StageSpec).
        assert_eq!(schema["title"], "ConversationSpec");
        assert!(
            schema["properties"]["stages"].is_object(),
            "schema exposes the stages property: {schema}"
        );
        // The closed atom set (Pred, via Guard) must appear in $defs.
        assert!(
            schema["definitions"]["Pred"].is_object() || schema["$defs"]["Pred"].is_object(),
            "Pred atom schema is present"
        );
    }

    #[test]
    fn conversation_error_serializes_machine_readable() {
        // An unguarded commit is rejected at compile with a Compile error.
        let err = Conversation::new("x")
            .stage("s")
            .commit("pay", Guard::always())
            .done(Guard::called_ok("pay"))
            .next("done", Guard::called_ok("pay"))
            .stage("done")
            .terminal()
            .compile()
            .expect_err("unguarded commit must fail");
        let json = serde_json::to_value(&err).expect("error serializes");
        assert_eq!(json["kind"], "compile");
        assert!(json["message"].is_string());
        // The structured per-error list carries the tagged FlowError diagnostics.
        let errors = json["errors"].as_array().expect("errors array");
        assert!(
            errors.iter().any(|e| e["kind"] == "unguarded_commit_tool"),
            "structured FlowError kinds present: {json}"
        );
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

    #[tokio::test]
    async fn validate_rejects_out_of_range_recognized_values() {
        use gemini_adk_rs::frame::{Frame, FrameSpec, SlotRecognizer, SlotSpec, SlotValidator};
        use gemini_adk_rs::live::TranscriptTurn;

        struct Party;
        impl Frame for Party {
            fn frame() -> FrameSpec {
                FrameSpec {
                    name: "party".into(),
                    slots: vec![SlotSpec {
                        recognizer: Some(SlotRecognizer::Integer),
                        validate: Some(SlotValidator::Range {
                            min: Some(1.0),
                            max: Some(12.0),
                        }),
                        ..SlotSpec::new("party_size")
                    }],
                }
            }
        }

        let convo = Conversation::new("p")
            .stage("collect")
            .collect_frame::<Party>()
            .next("done", Guard::captured(["party_size"]))
            .stage("done")
            .terminal()
            .compile()
            .expect("compiles");
        let extractor = convo.extractors()[0].clone().into_extractor();

        let run = |text: &str| {
            let extractor = extractor.clone();
            let text = text.to_string();
            async move {
                let state = State::new();
                let window = vec![TranscriptTurn {
                    turn_number: 0,
                    user: text,
                    model: String::new(),
                    tool_calls: Vec::new(),
                    timestamp: std::time::Instant::now(),
                }];
                let out = extractor.extract_with_state(&window, &state).await.unwrap();
                out.get("party_size").cloned()
            }
        };

        // In range -> filled; out of range -> rejected (no value promoted).
        assert_eq!(run("a table for 4").await, Some(serde_json::json!(4)));
        assert_eq!(run("a table for 40").await, None);
    }

    #[tokio::test]
    async fn resolve_slot_fills_from_async_fetch() {
        use gemini_adk_rs::live::TranscriptTurn;

        let convo = Conversation::new("c")
            .stage("check")
            .resolve_slot("availability", ["party_size"], None, |args| async move {
                // Echo an availability decision derived from the bound arg.
                let n = args
                    .get("party_size")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                Ok(serde_json::json!(n <= 8))
            })
            .next("done", Guard::is_set("availability"))
            .stage("done")
            .terminal()
            .compile()
            .expect("compiles");

        // The resolver lowered to an extractor.
        assert_eq!(convo.extractors().len(), 1);
        let extractor = convo.extractors()[0].clone().into_extractor();

        let state = State::new();
        let _ = state.set("party_size", 4i64);
        let window = vec![TranscriptTurn {
            turn_number: 0,
            user: "any".into(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }];
        let out = extractor.extract_with_state(&window, &state).await.unwrap();
        assert_eq!(out.get("availability"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn converse_registers_flow_and_extractors() {
        // Smoke test: the one-liner entrypoint wires onto a Live builder.
        use gemini_adk_rs::frame::{Frame, FrameSpec, SlotRecognizer, SlotSpec};

        struct Order;
        impl Frame for Order {
            fn frame() -> FrameSpec {
                FrameSpec {
                    name: "order".into(),
                    slots: vec![SlotSpec {
                        recognizer: Some(SlotRecognizer::OneOf(vec!["pizza".into()])),
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

        // Builds without panic; converse is the one-liner that govern()s + registers.
        let _live = crate::live::Live::builder().converse(&convo);
    }

    #[test]
    fn overlay_suspends_main_then_resumes_previous() {
        // Main: a -> b. FAQ overlay triggered by intent:faq, single terminal stage,
        // resume Previous. While the overlay runs, the main marking is untouched.
        let convo = Conversation::new("support")
            .stage("a")
            .next("b", Guard::is_true("a_done"))
            .stage("b")
            .terminal()
            .overlay("faq")
            .trigger(Guard::is_true("intent:faq"))
            // A gated answer stage so the overlay does not complete in one turn.
            .stage("answer")
            .done(Guard::is_true("faq_answered"))
            .next("faq_end", Guard::is_true("faq_answered"))
            .stage("faq_end")
            .terminal()
            .resume(Resume::Previous)
            .done_overlay()
            .compile()
            .expect("compiles");

        assert_eq!(convo.overlays().len(), 1);
        let mut stack = convo.stack(Enforcement::Enforce);
        let state = State::new();

        // Main is active on `a`.
        assert!(stack.explain(&state).active.contains(&"a".to_string()));
        assert!(stack.active_overlay().is_none());

        // Intent fires -> digression suspends the main flow and stays active.
        let _ = state.set("intent:faq", true);
        stack.on_turn(&state);
        assert_eq!(stack.active_overlay(), Some("faq"));

        // Answer the FAQ and clear the intent so the overlay completes and resumes.
        let _ = state.set("faq_answered", true);
        let _ = state.set("intent:faq", false);
        stack.on_turn(&state);
        assert!(stack.active_overlay().is_none());

        // Main resumed exactly where it was: still on `a`, not advanced.
        assert!(stack.explain(&state).active.contains(&"a".to_string()));

        // Main continues normally afterward.
        let _ = state.set("a_done", true);
        stack.on_turn(&state);
        assert!(stack.current().marking().done.contains("a"));
    }

    #[test]
    fn overlay_spec_round_trips_through_json() {
        let spec = Conversation::new("s")
            .stage("main")
            .terminal()
            .overlay("cancel")
            .trigger(Guard::is_true("intent:cancel"))
            .stage("confirm")
            .terminal()
            .resume(Resume::Terminate)
            .done_overlay()
            .into_spec();
        let json = serde_json::to_string(&spec).unwrap();
        let back: ConversationSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.overlays.len(), 1);
        assert_eq!(back.overlays[0].resume, Resume::Terminate);
        // Recompiles from the round-tripped spec.
        assert!(Conversation::from_spec(back).is_ok());
    }

    #[tokio::test]
    async fn safety_policy_terminates_on_intent() {
        use crate::policy::Policy;
        use crate::simulation::Sim;

        let convo = Conversation::new("support")
            .policy(Policy::safety_handoff(["self_harm", "abuse"]))
            .policy(Policy::redact(["card_number"]))
            .stage("triage")
            .next("resolve", Guard::is_true("triaged"))
            .stage("resolve")
            .terminal()
            .require(["resolve"])
            .compile()
            .expect("compiles");

        // Redaction set is recorded for the runtime.
        assert!(convo.redacted_fields().contains("card_number"));
        // SafetyHandoff lowered to a `safety` digression.
        assert!(convo.overlays().iter().any(|o| o.name == "safety"));

        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        assert!(sim.active().contains(&"triage".to_string()));
        assert!(!sim.is_complete());

        // A safety intent fires -> the conversation hands off (terminates).
        sim.set("intent:abuse", true);
        sim.turn();
        assert!(sim.is_complete());
    }

    #[tokio::test]
    async fn repair_reprompts_then_escalates_to_handoff() {
        use crate::simulation::Sim;

        // `collect` needs `info`; if the user stalls, reprompt after 2 turns and
        // escalate (route to `handoff`) after 3.
        let convo = Conversation::new("support")
            .stage("collect")
            .done(Guard::is_true("info"))
            .next("done", Guard::is_true("info"))
            .repair(RepairPolicy::new(2, 3).escalate_to("handoff"))
            .stage("done")
            .terminal()
            // Non-terminal so it stays *active* (terminal stages latch instantly).
            .stage("handoff")
            .done(Guard::is_true("handoff_complete"))
            .compile()
            .expect("compiles");

        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        assert!(sim.active().contains(&"collect".to_string()));

        // User stalls. Turn 1 active: no signal yet.
        sim.turn();
        assert_eq!(sim.slot::<bool>("repair:collect:reprompt"), None);
        // Turn 2 active: reprompt raised.
        sim.turn();
        assert_eq!(sim.slot::<bool>("repair:collect:reprompt"), Some(true));
        assert!(sim.active().contains(&"collect".to_string()));
        // Turn 3 active: escalate raised -> stage completes via escalation -> handoff.
        sim.turn();
        assert_eq!(sim.slot::<bool>("repair:collect:escalate"), Some(true));
        assert!(sim.active().contains(&"handoff".to_string()));
        assert!(!sim.active().contains(&"collect".to_string()));
    }

    #[tokio::test]
    async fn repair_signal_clears_when_stage_satisfied() {
        use crate::simulation::Sim;

        let convo = Conversation::new("s")
            .stage("collect")
            .done(Guard::is_true("info"))
            .next("done", Guard::is_true("info"))
            .repair(RepairPolicy::new(1, 9))
            .stage("done")
            .terminal()
            .require(["done"])
            .compile()
            .expect("compiles");

        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        sim.turn(); // active 1 turn -> reprompt (threshold 1)
        assert_eq!(sim.slot::<bool>("repair:collect:reprompt"), Some(true));

        // User provides info -> collect completes this turn; the signal clears the
        // following turn (once the stage is observed no longer active).
        sim.set("info", true);
        sim.turn();
        sim.turn();
        assert_eq!(sim.slot::<bool>("repair:collect:reprompt"), Some(false));
        assert!(sim.is_complete());
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
