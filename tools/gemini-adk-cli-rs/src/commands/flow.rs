//! `adk flow` — devtools for the conversation compiler: inspect, graph, simulate.
//!
//! Operates on a serializable [`ConversationSpec`] (JSON) — the same artifact the
//! `conversation-from-script` skill emits — so the authoring loop is: draft →
//! `inspect`/`graph` → `simulate`, all without a live API.

use std::fs;

use gemini_adk_fluent_rs::conversation::{CompiledConversation, Conversation, ConversationSpec};
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::simulation::Scenario;

/// Load and compile a conversation spec from a JSON file.
fn load(spec_path: &str) -> Result<CompiledConversation, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(spec_path)?;
    let spec: ConversationSpec = serde_json::from_str(&raw)?;
    Conversation::from_spec(spec).map_err(|e| e.to_string().into())
}

/// A human-readable summary of a compiled conversation.
pub fn inspect_summary(convo: &CompiledConversation) -> String {
    let flow = convo.flow().flow();
    let mut out = String::new();
    out.push_str(&format!("conversation: {}\n", convo.spec().name));
    out.push_str(&format!("stages ({}):\n", flow.steps.len()));
    for s in &flow.steps {
        let kind = if s.terminal { "terminal" } else { "stage" };
        out.push_str(&format!("  - {} [{kind}]\n", s.id));
    }
    let tools = &convo.flow().tool_policy().tools;
    out.push_str(&format!(
        "tools ({}): {}\n",
        tools.len(),
        tools.iter().cloned().collect::<Vec<_>>().join(", ")
    ));
    if !convo.overlays().is_empty() {
        out.push_str(&format!("digressions ({}):\n", convo.overlays().len()));
        for ov in convo.overlays() {
            out.push_str(&format!("  - {} (resume: {:?})\n", ov.name, ov.resume));
        }
    }
    let redacted = convo.redacted_fields();
    if !redacted.is_empty() {
        out.push_str(&format!(
            "redacted: {}\n",
            redacted.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !convo.policies().is_empty() {
        out.push_str(&format!("policies ({}):\n", convo.policies().len()));
        for p in convo.policies() {
            out.push_str(&format!("  - {p:?}\n"));
        }
    }
    out
}

/// `adk flow inspect <spec.json>` — print a compiled-conversation summary.
pub fn inspect(spec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    print!("{}", inspect_summary(&convo));
    Ok(())
}

/// `adk flow graph <spec.json>` — print the governed flow as a Mermaid diagram.
pub fn graph(spec_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    println!("{}", convo.to_mermaid());
    Ok(())
}

/// `adk flow simulate <spec.json> <scenario.json>` — run a model-free scenario.
pub async fn simulate(
    spec_path: &str,
    scenario_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let convo = load(spec_path)?;
    let scenario: Scenario = serde_json::from_str(&fs::read_to_string(scenario_path)?)?;
    match scenario.run(&convo, FlowMode::Enforce).await {
        Ok(()) => {
            println!("PASS: scenario '{}'", scenario.name);
            Ok(())
        }
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            Err(msg.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
    {
      "name": "booking",
      "stages": [
        { "id": "collect", "collect": ["party_size"],
          "next": [{ "to": "confirm", "when": { "captured": ["party_size"] } }] },
        { "id": "confirm", "allow": ["book"],
          "commit": { "tool": "book", "when": { "is_true": "user_confirmed" } },
          "next": [{ "to": "done", "when": { "called_ok": "book" } }] },
        { "id": "done", "terminal": true }
      ],
      "require": ["done"],
      "policies": [{ "kind": "redact", "keys": ["card_number"] }]
    }
    "#;

    fn compiled() -> CompiledConversation {
        let spec: ConversationSpec = serde_json::from_str(SPEC).unwrap();
        Conversation::from_spec(spec).unwrap()
    }

    #[test]
    fn inspect_summary_lists_stages_tools_and_policies() {
        let summary = inspect_summary(&compiled());
        assert!(summary.contains("conversation: booking"));
        assert!(summary.contains("collect"));
        assert!(summary.contains("book"));
        assert!(summary.contains("redacted: card_number"));
    }

    #[test]
    fn graph_renders_mermaid() {
        let mermaid = compiled().to_mermaid();
        assert!(mermaid.contains("flowchart") || mermaid.contains("graph"));
    }
}
