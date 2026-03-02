use std::collections::HashMap;
use std::fmt;

use crate::schema::{AdkSchema, AgentDef, FieldDef, ToolDef};

/// Result of diffing two AdkSchema documents.
#[derive(Debug, Default)]
pub struct SchemaDiff {
    pub agents: EntityDiffs,
    pub tools: EntityDiffs,
}

#[derive(Debug, Default)]
pub struct EntityDiffs {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<EntityChange>,
}

#[derive(Debug)]
pub struct EntityChange {
    pub name: String,
    pub field_changes: Vec<FieldChange>,
}

#[derive(Debug)]
pub enum FieldChange {
    Added(String, String),           // (name, type)
    Removed(String, String),         // (name, type)
    TypeChanged(String, String, String), // (name, old_type, new_type)
    OptionalityChanged(String, bool, bool), // (name, old_optional, new_optional)
}

impl fmt::Display for SchemaDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== ADK Schema Diff ===")?;
        writeln!(f)?;

        if self.is_empty() {
            writeln!(f, "No changes detected.")?;
            return Ok(());
        }

        // Agents
        if !self.agents.is_empty() {
            writeln!(f, "--- Agents ---")?;
            for name in &self.agents.added {
                writeln!(f, "  + ADDED: {}", name)?;
            }
            for name in &self.agents.removed {
                writeln!(f, "  - REMOVED: {}", name)?;
            }
            for change in &self.agents.changed {
                writeln!(f, "  ~ CHANGED: {}", change.name)?;
                for fc in &change.field_changes {
                    match fc {
                        FieldChange::Added(name, ty) => {
                            writeln!(f, "      + field '{}': {}", name, ty)?;
                        }
                        FieldChange::Removed(name, ty) => {
                            writeln!(f, "      - field '{}': {}", name, ty)?;
                        }
                        FieldChange::TypeChanged(name, old, new) => {
                            writeln!(f, "      ~ field '{}': {} -> {}", name, old, new)?;
                        }
                        FieldChange::OptionalityChanged(name, old, new) => {
                            writeln!(
                                f,
                                "      ~ field '{}': optional {} -> {}",
                                name, old, new
                            )?;
                        }
                    }
                }
            }
            writeln!(f)?;
        }

        // Tools
        if !self.tools.is_empty() {
            writeln!(f, "--- Tools ---")?;
            for name in &self.tools.added {
                writeln!(f, "  + ADDED: {}", name)?;
            }
            for name in &self.tools.removed {
                writeln!(f, "  - REMOVED: {}", name)?;
            }
            for change in &self.tools.changed {
                writeln!(f, "  ~ CHANGED: {}", change.name)?;
                for fc in &change.field_changes {
                    match fc {
                        FieldChange::Added(name, ty) => {
                            writeln!(f, "      + field '{}': {}", name, ty)?;
                        }
                        FieldChange::Removed(name, ty) => {
                            writeln!(f, "      - field '{}': {}", name, ty)?;
                        }
                        FieldChange::TypeChanged(name, old, new) => {
                            writeln!(f, "      ~ field '{}': {} -> {}", name, old, new)?;
                        }
                        FieldChange::OptionalityChanged(name, old, new) => {
                            writeln!(
                                f,
                                "      ~ field '{}': optional {} -> {}",
                                name, old, new
                            )?;
                        }
                    }
                }
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

impl SchemaDiff {
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && self.tools.is_empty()
    }
}

impl EntityDiffs {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compute the diff between two AdkSchema documents.
pub fn diff_schemas(old: &AdkSchema, new: &AdkSchema) -> SchemaDiff {
    SchemaDiff {
        agents: diff_agents(&old.agents, &new.agents),
        tools: diff_tools(&old.tools, &new.tools),
    }
}

fn diff_agents(old: &[AgentDef], new: &[AgentDef]) -> EntityDiffs {
    let old_map: HashMap<&str, &AgentDef> = old.iter().map(|a| (a.name.as_str(), a)).collect();
    let new_map: HashMap<&str, &AgentDef> = new.iter().map(|a| (a.name.as_str(), a)).collect();

    let mut diffs = EntityDiffs::default();

    // Find added
    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            diffs.added.push(name.to_string());
        }
    }

    // Find removed
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.removed.push(name.to_string());
        }
    }

    // Find changed
    for (name, old_agent) in &old_map {
        if let Some(new_agent) = new_map.get(name) {
            let changes = diff_fields(&old_agent.fields, &new_agent.fields);
            if !changes.is_empty() {
                diffs.changed.push(EntityChange {
                    name: name.to_string(),
                    field_changes: changes,
                });
            }
        }
    }

    diffs.added.sort();
    diffs.removed.sort();
    diffs.changed.sort_by(|a, b| a.name.cmp(&b.name));

    diffs
}

fn diff_tools(old: &[ToolDef], new: &[ToolDef]) -> EntityDiffs {
    let old_map: HashMap<&str, &ToolDef> = old.iter().map(|t| (t.name.as_str(), t)).collect();
    let new_map: HashMap<&str, &ToolDef> = new.iter().map(|t| (t.name.as_str(), t)).collect();

    let mut diffs = EntityDiffs::default();

    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            diffs.added.push(name.to_string());
        }
    }

    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.removed.push(name.to_string());
        }
    }

    for (name, old_tool) in &old_map {
        if let Some(new_tool) = new_map.get(name) {
            let changes = diff_fields(&old_tool.fields, &new_tool.fields);
            if !changes.is_empty() {
                diffs.changed.push(EntityChange {
                    name: name.to_string(),
                    field_changes: changes,
                });
            }
        }
    }

    diffs.added.sort();
    diffs.removed.sort();
    diffs.changed.sort_by(|a, b| a.name.cmp(&b.name));

    diffs
}

fn diff_fields(old: &[FieldDef], new: &[FieldDef]) -> Vec<FieldChange> {
    let old_map: HashMap<&str, &FieldDef> = old.iter().map(|f| (f.name.as_str(), f)).collect();
    let new_map: HashMap<&str, &FieldDef> = new.iter().map(|f| (f.name.as_str(), f)).collect();

    let mut changes = Vec::new();

    // Added fields
    for (name, field) in &new_map {
        if !old_map.contains_key(name) {
            changes.push(FieldChange::Added(
                name.to_string(),
                field.ts_type.clone(),
            ));
        }
    }

    // Removed fields
    for (name, field) in &old_map {
        if !new_map.contains_key(name) {
            changes.push(FieldChange::Removed(
                name.to_string(),
                field.ts_type.clone(),
            ));
        }
    }

    // Changed fields
    for (name, old_field) in &old_map {
        if let Some(new_field) = new_map.get(name) {
            if old_field.ts_type != new_field.ts_type {
                changes.push(FieldChange::TypeChanged(
                    name.to_string(),
                    old_field.ts_type.clone(),
                    new_field.ts_type.clone(),
                ));
            }
            if old_field.optional != new_field.optional {
                changes.push(FieldChange::OptionalityChanged(
                    name.to_string(),
                    old_field.optional,
                    new_field.optional,
                ));
            }
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn make_schema(agents: Vec<AgentDef>, tools: Vec<ToolDef>) -> AdkSchema {
        AdkSchema {
            source: SourceInfo {
                framework: "adk-js".to_string(),
                source_dir: "/tmp/test".to_string(),
                extracted_at: "2026-01-01T00:00:00Z".to_string(),
            },
            agents,
            tools,
        }
    }

    fn make_field(name: &str, ts_type: &str, optional: bool) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            ts_type: ts_type.to_string(),
            rust_type: "String".to_string(),
            optional,
            default_value: None,
            description: None,
        }
    }

    #[test]
    fn test_diff_no_changes() {
        let schema = make_schema(vec![], vec![]);
        let diff = diff_schemas(&schema, &schema);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_diff_added_agent() {
        let old = make_schema(vec![], vec![]);
        let new = make_schema(
            vec![AgentDef {
                name: "NewAgent".to_string(),
                kind: AgentKind::Llm,
                description: None,
                fields: vec![],
                callbacks: vec![],
                extends: None,
            }],
            vec![],
        );
        let diff = diff_schemas(&old, &new);
        assert_eq!(diff.agents.added, vec!["NewAgent"]);
        assert!(diff.agents.removed.is_empty());
    }

    #[test]
    fn test_diff_removed_tool() {
        let old = make_schema(
            vec![],
            vec![ToolDef {
                name: "OldTool".to_string(),
                description: None,
                fields: vec![],
                extends: None,
            }],
        );
        let new = make_schema(vec![], vec![]);
        let diff = diff_schemas(&old, &new);
        assert_eq!(diff.tools.removed, vec!["OldTool"]);
    }

    #[test]
    fn test_diff_changed_field_type() {
        let old_agent = AgentDef {
            name: "TestAgent".to_string(),
            kind: AgentKind::Base,
            description: None,
            fields: vec![make_field("model", "string", true)],
            callbacks: vec![],
            extends: None,
        };
        let new_agent = AgentDef {
            name: "TestAgent".to_string(),
            kind: AgentKind::Base,
            description: None,
            fields: vec![make_field("model", "string | BaseLlm", true)],
            callbacks: vec![],
            extends: None,
        };
        let old = make_schema(vec![old_agent], vec![]);
        let new = make_schema(vec![new_agent], vec![]);
        let diff = diff_schemas(&old, &new);

        assert_eq!(diff.agents.changed.len(), 1);
        assert_eq!(diff.agents.changed[0].name, "TestAgent");
        assert!(matches!(
            &diff.agents.changed[0].field_changes[0],
            FieldChange::TypeChanged(name, old, new) if name == "model" && old == "string" && new == "string | BaseLlm"
        ));
    }

    #[test]
    fn test_diff_display() {
        let diff = SchemaDiff {
            agents: EntityDiffs {
                added: vec!["NewAgent".to_string()],
                removed: vec![],
                changed: vec![],
            },
            tools: EntityDiffs::default(),
        };
        let output = format!("{}", diff);
        assert!(output.contains("ADDED: NewAgent"));
    }
}
