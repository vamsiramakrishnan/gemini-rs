//! Independent capability activations inside one speaking session.
//!
//! [`TaskRuntime`] is a synchronous owner of task state, governance and approval.
//! Live and offline drivers execute the [`OwnedInvocation`] it returns, then
//! return the completion to the same runtime. Workers never select a foreground
//! task or deliver speech. State snapshots and tool factories isolate accepted
//! task state from cancelled or superseded workers. Trusted custom tools must
//! not retain unrelated mutable state outside the supplied context/factory.
//!
//! Snapshots are observations, not durable recovery checkpoints. Commit adapters
//! must honor their idempotency argument and support external reconciliation;
//! cancelling a future cannot establish whether an external effect happened.
mod runtime;
mod services;
mod types;
pub use runtime::{InvocationCompletion, OwnedInvocation, TaskRuntime};
pub use services::{
    AcceptedTaskEffect, OwnedTaskServiceWork, TaskEffect, TaskEffects, TaskMemoryService,
    TaskObservation, TaskPattern, TaskServiceCompletion, TaskServices, TaskWatcher,
};
pub use types::*;
#[cfg(test)]
mod tests;

/// Two installed skills for session-level tests: `billing`, whose flow
/// offers `verify` until it has run and then `pay`, and `faq`, whose one
/// tool `answer` has no flow.
#[cfg(test)]
pub(crate) fn two_skill_runtime() -> TaskRuntime {
    use crate::flow::{Enforcement, Flow, FlowStack, Guard};
    use crate::tool::{SimpleTool, ToolDispatcher};
    use serde_json::json;
    use std::sync::Arc;

    fn skill(name: &str, tools: &[&str], flow: Option<FlowFactory>) -> CompiledSkill {
        let names: Vec<String> = tools.iter().map(ToString::to_string).collect();
        let registered = names.clone();
        CompiledSkill {
            services: None,
            name: name.into(),
            version: "1.0.0".into(),
            description: format!("{name} tasks"),
            instruction: format!("You handle {name}."),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            validate_input: Arc::new(|_| Ok(())),
            validate_output: Arc::new(|_| Ok(())),
            initial_state: Default::default(),
            flow_factory: flow,
            tool_factory: Arc::new(move |_| {
                let mut dispatcher = ToolDispatcher::new();
                for tool in &registered {
                    dispatcher.register(SimpleTool::new(
                        tool.clone(),
                        tool.clone(),
                        None,
                        |_| async { Ok(json!({"ok": true})) },
                    ));
                }
                Ok(dispatcher)
            }),
            tools: names
                .iter()
                .map(|tool| TaskTool {
                    name: tool.clone(),
                    description: tool.clone(),
                    parameters: None,
                    effect: TaskToolEffect::Read,
                })
                .collect(),
            receipt_applier: None,
            project_output: None,
        }
    }
    let billing_flow: FlowFactory = Arc::new(|| {
        Ok(Flow::new()
            .step("verify")
            .allow(["verify"])
            .done(Guard::called_ok("verify"))
            .step("pay")
            .after("verify")
            .allow(["pay"])
            .done(Guard::called_ok("pay"))
            .step("end")
            .after("pay")
            .terminal()
            .build()
            .unwrap()
            .compile()
            .map(|flow| FlowStack::new(flow, Enforcement::Enforce))
            .unwrap())
    });
    TaskRuntime::new(vec![
        skill("billing", &["verify", "pay"], Some(billing_flow)),
        skill("faq", &["answer"], None),
    ])
    .unwrap()
}
