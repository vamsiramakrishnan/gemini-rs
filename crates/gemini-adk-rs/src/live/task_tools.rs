//! Fixed provider declarations and model command boundary for task sessions.

use gemini_genai_rs::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};
use serde_json::json;

use crate::tasks::{TaskCommand, TaskError, TaskRuntime, TaskToolEffect};

pub(super) const CONTROL_TOOL: &str = "task_control";

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
        "You manage several independent tasks in one conversation. Use task_control to start the relevant skill before calling its qualified tools. Only the foreground task speaks. Ask a clarifying question when the skill or required inputs are unclear. For compound requests, create separate tasks and resume them deliberately. Calling a commit tool creates a proposal; ask the user to approve it in the application after creating it. Tool approvals come from the application; never approve your own action or treat spoken consent as approval. A speech interruption does not cancel a business task. Installed skills: {}",
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
