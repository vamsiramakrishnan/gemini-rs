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
    state: rs_adk::State,
}

impl AgentHarness {
    /// Create a new harness with empty state.
    pub fn new() -> Self {
        Self {
            state: rs_adk::State::new(),
        }
    }

    /// Set a state value before running.
    pub fn set<V: serde::Serialize>(self, key: &str, value: V) -> Self {
        self.state.set(key, value);
        self
    }

    /// Get the underlying state.
    pub fn state(&self) -> &rs_adk::State {
        &self.state
    }

    /// Run a text agent against this harness state.
    pub async fn run(
        &self,
        agent: &dyn rs_adk::text::TextAgent,
    ) -> Result<String, rs_adk::error::AgentError> {
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
}
