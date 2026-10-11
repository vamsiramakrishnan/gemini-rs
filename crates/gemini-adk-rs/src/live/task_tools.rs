//! Fixed provider declarations and model command boundary for task sessions.

use std::collections::BTreeSet;

use gemini_genai_rs::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};
use serde_json::json;

use crate::tasks::{TaskCommand, TaskError, TaskRuntime, TaskToolEffect};

pub(super) const CONTROL_TOOL: &str = "task_control";

/// Prefix of a skill's typed entry point, `start_{skill}`.
const START_PREFIX: &str = "start_";

/// The argument of a `start_{skill}` call that names a parent task, unless
/// the skill's own input has a field of that name.
const PARENT_ARG: &str = "parent_task";

/// The typed entry point of `skill`.
pub(super) fn start_tool(skill: &str) -> String {
    format!("{START_PREFIX}{skill}")
}

/// The tools every task session keeps declared: `task_control` and each
/// installed skill's typed entry point. The foreground skill's own tools come
/// and go with it.
pub(super) fn entry_names(runtime: &TaskRuntime) -> BTreeSet<String> {
    std::iter::once(CONTROL_TOOL.to_string())
        .chain(runtime.catalog().iter().map(|s| start_tool(&s.key.name)))
        .collect()
}

/// A `start_{skill}` call as a start command: its arguments are the skill's
/// input, less `parent_task`. `None` when `name` is no installed skill's
/// entry point.
pub(super) fn start_command(
    name: &str,
    mut args: serde_json::Value,
    runtime: &TaskRuntime,
) -> Option<TaskCommand> {
    let skill = name.strip_prefix(START_PREFIX)?;
    let summary = runtime
        .catalog()
        .into_iter()
        .find(|s| s.key.name == skill)?;
    let own_parent = summary.input_schema["properties"].get(PARENT_ARG).is_some();
    let parent = if own_parent {
        None
    } else {
        args.as_object_mut()
            .and_then(|a| a.remove(PARENT_ARG))
            .and_then(|p| p.as_str().map(|p| crate::tasks::TaskId(p.to_string())))
    };
    if args.is_null() {
        args = json!({});
    }
    Some(TaskCommand::Start {
        skill: skill.to_string(),
        input: args,
        parent,
    })
}

/// `schema` as function parameters: a skill's input contract is JSON Schema,
/// and the Live API refuses a declaration with keywords outside its subset
/// (`additionalProperties` closes the session with 1007). Keeps the keywords
/// that describe the shape and drops empty descriptions; the contract itself
/// still validates the input when the task starts.
fn declarable(schema: &serde_json::Value) -> serde_json::Value {
    const KEEP: [&str; 8] = [
        "type",
        "description",
        "properties",
        "required",
        "items",
        "enum",
        "format",
        "nullable",
    ];
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = serde_json::Map::new();
    for (key, value) in object {
        if !KEEP.contains(&key.as_str()) {
            continue;
        }
        let value = match key.as_str() {
            "description" if value.as_str().is_some_and(|d| d.trim().is_empty()) => continue,
            "properties" => serde_json::Value::Object(
                value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, field)| (name.clone(), declarable(field)))
                    .collect(),
            ),
            "items" => declarable(value),
            _ => value.clone(),
        };
        out.insert(key.clone(), value);
    }
    serde_json::Value::Object(out)
}

/// The declaration of `skill`'s entry point: its input contract as typed
/// parameters. The model fills typed parameters; given `task_control`'s
/// free-form `input` object instead, Gemini 3.8 Live left it out and retried
/// the same failing start over a hundred times in one turn.
fn start_declaration(skill: &crate::tasks::SkillSummary) -> FunctionDeclaration {
    let mut parameters = declarable(&skill.input_schema);
    if !parameters.is_object() {
        parameters = json!({ "type": "object" });
    }
    parameters["type"] = json!("object");
    let properties = parameters["properties"]
        .as_object_mut()
        .map(std::mem::take)
        .unwrap_or_default();
    let mut properties = properties;
    properties
        .entry(PARENT_ARG.to_string())
        .or_insert_with(|| {
            json!({
                "type": "string",
                "description": "The id of the foreground task, to start this as a temporary child that returns to it on completion."
            })
        });
    parameters["properties"] = serde_json::Value::Object(properties);
    FunctionDeclaration {
        name: start_tool(&skill.key.name),
        description: format!(
            "Start the {} skill: {} Starting it suspends the foreground task.",
            skill.key.name,
            skill.description.trim_end_matches('.').to_string() + "."
        ),
        parameters: Some(parameters),
        behavior: None,
    }
}

pub(super) fn declarations(runtime: &TaskRuntime) -> Vec<Tool> {
    let mut declarations = vec![FunctionDeclaration {
        name: CONTROL_TOOL.into(),
        description: "Manage independent tasks: start a skill, suspend or resume a task, revise its inputs, cancel it, or complete it. Starting another task suspends the foreground task. Set parent for a temporary child task that returns to its parent on completion. Never claim an external effect succeeded before its tool receipt.".into(),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "action": {"type":"string", "enum":["start","suspend","resume","revise","cancel","complete"]},
                "skill": {"type":"string"},
                "input": {"type":"object"},
                "parent": {"type":"string"},
                "task": {"type":"string"},
                "expected_revision": {"type":"integer"},
                "output": {"type":"object"}
            },
            "required": ["action"]
        })),
        behavior: None,
    }];
    declarations.extend(runtime.catalog().iter().map(start_declaration));
    for skill in runtime.catalog() {
        declarations.extend(skill.tools.into_iter().map(|tool| {
            let effect = match tool.effect {
                TaskToolEffect::Read => "Read operation; wait for its result before answering.",
                TaskToolEffect::Commit { .. } => "Calling this tool creates an approval proposal in the application. Call it when the requested action and arguments are clear, then ask the user to approve that proposal in the application. No external action executes until trusted approval. Spoken consent does not approve the proposal. Wait for a successful receipt before claiming success.",
            };
            FunctionDeclaration {
                name: format!("{}__{}", skill.key.name, tool.name),
                description: format!("{}: {}. {effect}", skill.key.name, tool.description),
                parameters: tool.parameters,
                behavior: Some(FunctionCallingBehavior::NonBlocking),
            }
        }));
    }
    vec![Tool::functions(declarations)]
}

pub(super) fn catalog_instruction(runtime: &TaskRuntime) -> String {
    let catalog: Vec<_> = runtime
        .catalog()
        .into_iter()
        .map(|skill| {
            json!({
                "name": skill.key.name,
                "version": skill.key.version,
                "description": skill.description,
                "inputs": skill.input_schema
            })
        })
        .collect();
    format!(
        "You manage several independent tasks in one conversation. Start the relevant skill with its start_<skill> tool, filling in its inputs, before calling its qualified tools; manage started tasks with task_control. Only the foreground task speaks. Ask a clarifying question when the skill or required inputs are unclear. For compound requests, create separate tasks and resume them deliberately. Calling a commit tool creates a proposal; ask the user to approve it in the application after creating it. Tool approvals come from the application; never approve your own action or treat spoken consent as approval. A speech interruption does not cancel a business task. Installed skills: {}",
        json!(catalog)
    )
}

pub(super) fn model_command(args: serde_json::Value) -> Result<TaskCommand, TaskError> {
    let command: TaskCommand = serde_json::from_value(args)
        .map_err(|error| TaskError(format!("invalid task command: {error}")))?;
    match command {
        TaskCommand::Decide { .. } | TaskCommand::Reconcile { .. } | TaskCommand::Invoke { .. } => {
            Err(TaskError(
                "this command requires the trusted application interface".into(),
            ))
        }
        _ => Ok(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_declaration_keeps_only_what_the_live_api_accepts() {
        let declared = declarable(&json!({
            "type": "object",
            "properties": {
                "account_id": {"type": "string", "description": ""},
                "amount": {"type": "number", "description": "In dollars", "default": 0},
            },
            "required": ["account_id"],
            "additionalProperties": false,
        }));
        assert_eq!(
            declared,
            json!({
                "type": "object",
                "properties": {
                    "account_id": {"type": "string"},
                    "amount": {"type": "number", "description": "In dollars"},
                },
                "required": ["account_id"],
            })
        );
    }

    #[test]
    fn model_cannot_approve_or_reconcile_an_operation() {
        for command in [
            json!({"action":"decide", "operation":"op-1", "approve":true}),
            json!({"action":"reconcile", "operation":"op-1", "outcome":{"status":"succeeded","result":{}}}),
            json!({"action":"invoke", "task":"task-1", "tool":"charge", "args":{}}),
        ] {
            assert!(model_command(command).is_err());
        }
        assert!(model_command(json!({"action":"start","skill":"faq","input":{}})).is_ok());
    }
}
