//! Testing utilities — mock backends, agent harnesses, contract validation.

use std::collections::{HashMap, HashSet};

use crate::builder::AgentBuilder;

/// Contract violation detected during static analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractViolation {
    /// A consumer reads a key that no producer writes.
    UnproducedKey {
        /// Name of the agent that reads the unproduced key.
        consumer: String,
        /// The state key that is read but never written.
        key: String,
    },
    /// Multiple agents write to the same key (race condition risk).
    DuplicateWrite {
        /// Names of agents that write to the same key.
        agents: Vec<String>,
        /// The contested state key.
        key: String,
    },
    /// A producer writes to a key that no consumer reads (dead output).
    OrphanedOutput {
        /// Name of the agent that writes the orphaned key.
        producer: String,
        /// The state key that is written but never read.
        key: String,
    },
}

/// Check state contracts across a set of agents.
///
/// Validates that:
/// - Every key a consumer reads is produced by some agent
/// - No two agents write the same key (race condition detection)
/// - Every key a producer writes is consumed by some agent (dead code detection)
pub fn check_contracts(agents: &[AgentBuilder]) -> Vec<ContractViolation> {
    let mut violations = Vec::new();

    // Collect all writes and reads
    let mut all_writes: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_reads: HashSet<String> = HashSet::new();
    let mut all_written_keys: HashSet<String> = HashSet::new();

    for agent in agents {
        for key in agent.get_writes() {
            all_writes
                .entry(key.clone())
                .or_default()
                .push(agent.name().to_string());
            all_written_keys.insert(key.clone());
        }
        for key in agent.get_reads() {
            all_reads.insert(key.clone());
        }
    }

    // Check for unproduced keys (consumer reads what nobody writes)
    for agent in agents {
        for key in agent.get_reads() {
            if !all_written_keys.contains(key) {
                violations.push(ContractViolation::UnproducedKey {
                    consumer: agent.name().to_string(),
                    key: key.clone(),
                });
            }
        }
    }

    // Check for duplicate writes
    for (key, writers) in &all_writes {
        if writers.len() > 1 {
            violations.push(ContractViolation::DuplicateWrite {
                agents: writers.clone(),
                key: key.clone(),
            });
        }
    }

    // Check for orphaned outputs (producer writes, nobody reads)
    for agent in agents {
        for key in agent.get_writes() {
            if !all_reads.contains(key) {
                violations.push(ContractViolation::OrphanedOutput {
                    producer: agent.name().to_string(),
                    key: key.clone(),
                });
            }
        }
    }

    violations
}

/// Infer data flow between agents based on reads/writes declarations.
///
/// Returns a list of `(producer, consumer, key)` tuples representing data dependencies.
pub fn infer_data_flow(agents: &[AgentBuilder]) -> Vec<DataFlowEdge> {
    let mut edges = Vec::new();

    for producer in agents {
        for consumer in agents {
            if producer.name() == consumer.name() {
                continue;
            }
            for write_key in producer.get_writes() {
                if consumer.get_reads().contains(write_key) {
                    edges.push(DataFlowEdge {
                        producer: producer.name().to_string(),
                        consumer: consumer.name().to_string(),
                        key: write_key.clone(),
                    });
                }
            }
        }
    }

    edges
}

/// A data flow edge between two agents.
#[derive(Debug, Clone, PartialEq)]
pub struct DataFlowEdge {
    /// The agent that writes the key.
    pub producer: String,
    /// The agent that reads the key.
    pub consumer: String,
    /// The state key.
    pub key: String,
}

/// A test harness for running agents with controlled inputs.
pub struct AgentHarness {
    state: gemini_adk_rs::State,
}

impl AgentHarness {
    /// Create a new harness with empty state.
    pub fn new() -> Self {
        Self {
            state: gemini_adk_rs::State::new(),
        }
    }

    /// Set a state value before running.
    pub fn set<V: serde::Serialize>(self, key: &str, value: V) -> Self {
        let _ = self.state.set(key, value);
        self
    }

    /// Get the underlying state.
    pub fn state(&self) -> &gemini_adk_rs::State {
        &self.state
    }

    /// Run a text agent against this harness state.
    pub async fn run(
        &self,
        agent: &dyn gemini_adk_rs::text::TextAgent,
    ) -> Result<String, gemini_adk_rs::error::AgentError> {
        agent.run(&self.state).await
    }
}

impl Default for AgentHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// Diagnostic utility — returns a summary of an agent builder's configuration.
pub fn diagnose(agent: &AgentBuilder) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Agent: {}", agent.name()));

    if let Some(model) = agent.get_model() {
        lines.push(format!("  Model: {:?}", model));
    }
    if let Some(inst) = agent.get_instruction() {
        let truncated = if inst.len() > 80 {
            format!("{}...", &inst[..80])
        } else {
            inst.to_string()
        };
        lines.push(format!("  Instruction: {}", truncated));
    }
    if let Some(t) = agent.get_temperature() {
        lines.push(format!("  Temperature: {}", t));
    }
    if agent.tool_count() > 0 {
        lines.push(format!("  Tools: {}", agent.tool_count()));
    }
    if !agent.get_writes().is_empty() {
        lines.push(format!("  Writes: {:?}", agent.get_writes()));
    }
    if !agent.get_reads().is_empty() {
        lines.push(format!("  Reads: {:?}", agent.get_reads()));
    }
    if !agent.get_sub_agents().is_empty() {
        lines.push(format!("  Sub-agents: {}", agent.get_sub_agents().len()));
    }

    lines.join("\n")
}

// ─── Live session contracts ─────────────────────────────────────────────────

/// A misconfiguration found in a [`Live`](crate::live::Live) builder, before
/// connecting.
///
/// The counterpart to [`ContractViolation`] for voice sessions. `check_contracts`
/// only ever saw `AgentBuilder`, so the configuration where cross-referencing
/// actually matters — phases, a governing flow, memory slots, watchers, all
/// naming each other by string — had no static check at all.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveViolation {
    /// A governing flow names a tool this session does not register.
    ///
    /// Not inert: a step whose `allow` list contains only names that match
    /// nothing denies *every* tool for as long as that step is active.
    FlowToolNotRegistered {
        /// The name the flow uses.
        tool: String,
        /// What the session actually registers, for spotting a typo.
        registered: Vec<String>,
    },
    /// Phases were configured but no initial phase was named, so **every phase
    /// is silently discarded at connect** and the session runs unphased.
    PhasesWithoutInitialPhase {
        /// The phases that will be dropped.
        phases: Vec<String>,
    },
    /// `initial_phase` names a phase that was never defined. The machine starts
    /// in a state with no instruction, no tools and no transitions.
    UnknownInitialPhase {
        /// The name given to `initial_phase`.
        name: String,
        /// The phases that do exist.
        defined: Vec<String>,
    },
    /// A phase no transition targets and which is not the initial phase — it
    /// can never be entered.
    UnreachablePhase {
        /// The phase that cannot be reached.
        name: String,
    },
    /// A phase transition targets a phase that does not exist.
    UnknownTransitionTarget {
        /// The phase declaring the transition.
        from: String,
        /// The target that does not exist.
        target: String,
    },
    /// Some tools resolve over the network at connect, so name-based checks
    /// here are working from a partial registry.
    ///
    /// Advisory, not an error: it reports that the check could not be complete,
    /// which is worth saying out loud rather than implying full coverage.
    ToolsUnresolvedAtCheckTime {
        /// How many tools will only exist after connect.
        count: usize,
    },
}

impl std::fmt::Display for LiveViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FlowToolNotRegistered { tool, registered } => write!(
                f,
                "the governing flow names tool `{tool}`, which this session does not register. \
                 Registered: [{}]. A step whose `allow` list matches nothing denies every tool \
                 while it is active.",
                registered.join(", ")
            ),
            Self::PhasesWithoutInitialPhase { phases } => write!(
                f,
                "{} phase(s) defined ({}) but no `initial_phase(..)` — every one of them is \
                 discarded at connect and the session runs unphased.",
                phases.len(),
                phases.join(", ")
            ),
            Self::UnknownInitialPhase { name, defined } => write!(
                f,
                "`initial_phase(\"{name}\")` names a phase that does not exist. Defined: [{}].",
                defined.join(", ")
            ),
            Self::UnreachablePhase { name } => write!(
                f,
                "phase `{name}` is not the initial phase and no transition targets it — it can \
                 never be entered."
            ),
            Self::UnknownTransitionTarget { from, target } => write!(
                f,
                "phase `{from}` transitions to `{target}`, which does not exist."
            ),
            Self::ToolsUnresolvedAtCheckTime { count } => write!(
                f,
                "{count} tool(s) resolve at connect (MCP/A2A/OpenAPI), so tool-name checks here \
                 are partial. Re-check after connect, or expect connect to catch the rest."
            ),
        }
    }
}

/// Statically check a configured [`Live`](crate::live::Live) session.
///
/// Cross-references the parts that name each other by string — the flow's tool
/// names against the registered tools, phase transitions against defined
/// phases, `initial_phase` against both — and reports what cannot line up. Call
/// it in a test, before the session ever connects:
///
/// ```
/// # use gemini_adk_fluent_rs::live::Live;
/// # use gemini_adk_fluent_rs::testing::check_live;
/// let session = Live::builder().instruction("Be helpful.");
/// assert!(check_live(&session).is_empty());
/// ```
///
/// Connect enforces the tool-name half itself and will refuse a mismatched
/// flow; this exists so the failure arrives in a unit test instead of at the
/// first connection attempt. The phase checks have no runtime counterpart —
/// forgetting `initial_phase` discards every phase in silence.
pub fn check_live(live: &crate::live::Live) -> Vec<LiveViolation> {
    let mut violations = Vec::new();

    let registered = live.declared_tool_names();
    if live.pending_tool_count() > 0 {
        violations.push(LiveViolation::ToolsUnresolvedAtCheckTime {
            count: live.pending_tool_count(),
        });
    }

    // Flow tool names. `compile_with_tools` already owns this reasoning
    // (including ambient tools and every constraint kind), so borrow it rather
    // than re-walking the vocabulary and drifting from it.
    if let Some(flow) = live.flow() {
        let mut flow = flow.clone();
        crate::live::merge_ambient_for_check(&mut flow, live.ambient_tool_names());
        let names: Vec<&str> = registered.iter().map(String::as_str).collect();
        if let Err(errors) = flow.compile_with_tools(&names) {
            for error in &errors.0 {
                if let gemini_adk_rs::flow::FlowError::UnknownTool(tool) = error {
                    violations.push(LiveViolation::FlowToolNotRegistered {
                        tool: tool.clone(),
                        registered: registered.clone(),
                    });
                }
            }
        }
    }

    // Phases.
    let defined: Vec<String> = live.phases().iter().map(|p| p.name.clone()).collect();
    match live.initial_phase_name() {
        None if !defined.is_empty() => {
            violations.push(LiveViolation::PhasesWithoutInitialPhase {
                phases: defined.clone(),
            });
        }
        Some(initial) if !defined.iter().any(|p| p == initial) => {
            violations.push(LiveViolation::UnknownInitialPhase {
                name: initial.to_string(),
                defined: defined.clone(),
            });
        }
        _ => {}
    }

    let mut targeted: HashSet<String> = HashSet::new();
    for phase in live.phases() {
        for transition in &phase.transitions {
            targeted.insert(transition.target.clone());
            if !defined.contains(&transition.target) {
                violations.push(LiveViolation::UnknownTransitionTarget {
                    from: phase.name.clone(),
                    target: transition.target.clone(),
                });
            }
        }
    }
    if let Some(initial) = live.initial_phase_name() {
        for phase in live.phases() {
            if phase.name != initial && !targeted.contains(&phase.name) {
                violations.push(LiveViolation::UnreachablePhase {
                    name: phase.name.clone(),
                });
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_violations_for_matching_contracts() {
        let writer = AgentBuilder::new("writer").writes("output");
        let reader = AgentBuilder::new("reader").reads("output");
        let violations = check_contracts(&[writer, reader]);
        assert!(violations.is_empty());
    }

    #[test]
    fn detects_unproduced_key() {
        let reader = AgentBuilder::new("reader").reads("missing");
        let violations = check_contracts(&[reader]);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            ContractViolation::UnproducedKey {
                consumer,
                key,
            } if consumer == "reader" && key == "missing"
        ));
    }

    #[test]
    fn detects_duplicate_write() {
        let a = AgentBuilder::new("a").writes("shared");
        let b = AgentBuilder::new("b").writes("shared").reads("shared");
        let violations = check_contracts(&[a, b]);
        assert!(violations.iter().any(
            |v| matches!(v, ContractViolation::DuplicateWrite { key, .. } if key == "shared")
        ));
    }

    #[test]
    fn detects_orphaned_output() {
        let writer = AgentBuilder::new("writer").writes("unused");
        let violations = check_contracts(&[writer]);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            ContractViolation::OrphanedOutput {
                producer,
                key,
            } if producer == "writer" && key == "unused"
        ));
    }

    #[test]
    fn multiple_violations() {
        let a = AgentBuilder::new("a").writes("orphan");
        let b = AgentBuilder::new("b").reads("missing");
        let violations = check_contracts(&[a, b]);
        assert_eq!(violations.len(), 2);
    }

    #[test]
    fn empty_agents_no_violations() {
        let violations = check_contracts(&[]);
        assert!(violations.is_empty());
    }

    #[test]
    fn infer_data_flow_finds_edges() {
        let writer = AgentBuilder::new("writer").writes("output");
        let reader = AgentBuilder::new("reader").reads("output");
        let edges = infer_data_flow(&[writer, reader]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].producer, "writer");
        assert_eq!(edges[0].consumer, "reader");
        assert_eq!(edges[0].key, "output");
    }

    #[test]
    fn infer_data_flow_no_self_edges() {
        let agent = AgentBuilder::new("self").writes("key").reads("key");
        let edges = infer_data_flow(&[agent]);
        assert!(edges.is_empty());
    }

    #[test]
    fn diagnose_basic() {
        let agent = AgentBuilder::new("test")
            .instruction("Be helpful")
            .temperature(0.5)
            .writes("output");
        let diag = diagnose(&agent);
        assert!(diag.contains("test"));
        assert!(diag.contains("Be helpful"));
        assert!(diag.contains("0.5"));
    }

    #[test]
    fn harness_sets_state() {
        let harness = AgentHarness::new().set("key", "value");
        let val: Option<String> = harness.state().get("key");
        assert_eq!(val, Some("value".into()));
    }

    #[test]
    fn complex_pipeline_contracts() {
        let researcher = AgentBuilder::new("researcher")
            .writes("findings")
            .writes("sources");
        let writer = AgentBuilder::new("writer")
            .reads("findings")
            .writes("draft");
        let reviewer = AgentBuilder::new("reviewer")
            .reads("draft")
            .writes("quality");

        let violations = check_contracts(&[researcher, writer, reviewer]);
        // "sources" is orphaned (nobody reads it), "quality" is orphaned (nobody reads it)
        let orphans: Vec<_> = violations
            .iter()
            .filter(|v| matches!(v, ContractViolation::OrphanedOutput { .. }))
            .collect();
        assert_eq!(orphans.len(), 2);
    }

    // ─── check_live ─────────────────────────────────────────────────────────

    use crate::live::Live;
    use gemini_adk_rs::flow::{Flow, Guard};

    fn book_tool() -> crate::compose::tools::ToolComposite {
        crate::compose::T::simple("book_table", "Book a table", |_| async {
            Ok(serde_json::json!({"ok": true}))
        })
    }

    #[test]
    fn a_clean_session_reports_nothing() {
        let live = Live::builder()
            .instruction("Be helpful.")
            .with_tools(book_tool())
            .govern(
                Flow::new()
                    .step("book")
                    .allow(["book_table"])
                    .done(Guard::called_ok("book_table"))
                    .build()
                    .expect("valid"),
            )
            .phase("greet")
            .instruction("Say hello")
            .done()
            .initial_phase("greet");
        assert_eq!(check_live(&live), vec![]);
    }

    #[test]
    fn a_flow_tool_typo_is_caught_before_connecting() {
        let live = Live::builder().with_tools(book_tool()).govern(
            Flow::new()
                .step("book")
                .allow(["book_tabel"])
                .done(Guard::called_ok("book_tabel"))
                .build()
                .expect("valid shape"),
        );
        let found = check_live(&live);
        assert!(
            found.iter().any(|v| matches!(
                v,
                LiveViolation::FlowToolNotRegistered { tool, .. } if tool == "book_tabel"
            )),
            "{found:?}"
        );
        assert!(
            found[0].to_string().contains("book_table"),
            "the message must show what is registered so the typo is visible: {}",
            found[0]
        );
    }

    #[test]
    fn ambient_tools_are_counted_as_the_session_will_see_them() {
        // `check_live` must check the flow connect will actually run, which
        // includes builder-registered ambient tools — otherwise it reports a
        // failure that will not happen.
        let live = Live::builder()
            .with_tools(book_tool())
            .ambient_tools(["book_table"])
            .govern(
                Flow::new()
                    .step("book")
                    .done(Guard::called_ok("book_table"))
                    .build()
                    .expect("valid"),
            );
        assert_eq!(check_live(&live), vec![]);
    }

    #[test]
    fn phases_without_an_initial_phase_are_reported_as_discarded() {
        // The whole set is dropped at connect, in silence. CLAUDE.md lists
        // forgetting `initial_phase` as a common mistake; nothing enforced it.
        let live = Live::builder()
            .phase("greet")
            .instruction("Say hello")
            .done()
            .phase("main")
            .instruction("Help")
            .done();
        let found = check_live(&live);
        assert!(
            found.iter().any(|v| matches!(
                v,
                LiveViolation::PhasesWithoutInitialPhase { phases } if phases.len() == 2
            )),
            "{found:?}"
        );
        assert!(found[0].to_string().contains("discarded"), "{}", found[0]);
    }

    #[test]
    fn an_initial_phase_naming_nothing_is_reported() {
        let live = Live::builder()
            .phase("greet")
            .instruction("Say hello")
            .done()
            .initial_phase("greting");
        assert!(check_live(&live).iter().any(
            |v| matches!(v, LiveViolation::UnknownInitialPhase { name, .. } if name == "greting")
        ));
    }

    #[test]
    fn an_unreachable_phase_is_reported() {
        let live = Live::builder()
            .phase("greet")
            .instruction("Say hello")
            .done()
            .phase("orphan")
            .instruction("Never entered")
            .done()
            .initial_phase("greet");
        assert!(check_live(&live)
            .iter()
            .any(|v| matches!(v, LiveViolation::UnreachablePhase { name } if name == "orphan")));
    }

    #[test]
    fn a_transition_to_a_missing_phase_is_reported() {
        let live = Live::builder()
            .phase("greet")
            .instruction("Say hello")
            .transition("mian", |_| true)
            .done()
            .phase("main")
            .instruction("Help")
            .done()
            .initial_phase("greet");
        let found = check_live(&live);
        assert!(
            found.iter().any(|v| matches!(
                v,
                LiveViolation::UnknownTransitionTarget { target, .. } if target == "mian"
            )),
            "{found:?}"
        );
    }

    #[test]
    fn an_unphased_session_is_not_reported() {
        // No phases at all is the ordinary shape, not a mistake.
        let live = Live::builder().instruction("Be helpful.");
        assert_eq!(check_live(&live), vec![]);
    }
}
