//! Declaring only the current phase's and step's tools to the model.
//!
//! Under [`SteeringMode::ContextUpdate`](super::SteeringMode::ContextUpdate) the
//! session keeps every tool declaration here and, at each phase transition or
//! flow-step change, replaces what the model has declared with the tools the
//! current phase and step offer, through a `contextUpdate` message. A tool the
//! step allows but whose `never(tool).until(..)` guard has not held yet, such
//! as a commit waiting for the caller's yes, is offered: the model can call
//! it in the turn the guard comes to hold, and a call before that is refused
//! with the reason. Elsewhere a phase's or step's tool list is only enforced:
//! the model sees every tool and has the calls it may not make refused.

use std::collections::BTreeSet;
use std::sync::Arc;

use gemini_genai_rs::prelude::{ContextUpdate, Tool};
use gemini_genai_rs::session::SessionWriter;
use parking_lot::Mutex;

use super::phase::PhaseMachine;
use crate::flow::{Enforcement, FlowStack, SharedFlowStack};
use crate::state::State;

/// Session state key holding the function names declared to the model now.
pub(crate) const DECLARED_TOOLS_KEY: &str = "declared_tools";

/// Every tool declaration of the session, and the names currently declared.
#[derive(Debug)]
pub(crate) struct ToolScope {
    /// The session's tool declarations, as resolved at connect.
    catalog: Vec<Tool>,
    /// The connect-time system instruction, which a phase instruction is
    /// appended to rather than replacing.
    base_instruction: Option<String>,
    /// The function names the model has declared now, sorted.
    declared: Mutex<Vec<String>>,
    /// The system instruction the model has now, when [`sync`](Self::sync)
    /// manages it (task sessions). Starts as the connect-time instruction.
    instruction: Mutex<Option<String>>,
}

impl ToolScope {
    /// Scope over `catalog`, with `declared` the function names the setup
    /// message declares.
    pub(crate) fn new(
        catalog: Vec<Tool>,
        base_instruction: Option<String>,
        declared: &BTreeSet<String>,
    ) -> Self {
        Self {
            catalog,
            instruction: Mutex::new(base_instruction.clone()),
            base_instruction,
            declared: Mutex::new(declared.iter().cloned().collect()),
        }
    }

    /// The function names offered right now: in the phase's tool list (when
    /// it has one) and offered by an enforcing flow.
    pub(crate) fn admitted(
        &self,
        phase_tools: Option<&[String]>,
        flow: Option<&FlowStack>,
        state: &State,
    ) -> BTreeSet<String> {
        offered_names(&self.catalog, phase_tools, flow, state)
    }

    /// The declarations to send for `names`: their function declarations, and
    /// every tool that is not a function list (search, code execution), which
    /// no phase or step can name.
    pub(crate) fn tools_for(&self, names: &BTreeSet<String>) -> Vec<Tool> {
        tools_for(&self.catalog, names)
    }

    /// Record `names` as declared. Returns the declarations to send when they
    /// differ from what is declared now, `None` when nothing changed.
    pub(crate) fn redeclare(&self, names: &BTreeSet<String>) -> Option<Vec<Tool>> {
        let next: Vec<String> = names.iter().cloned().collect();
        let mut declared = self.declared.lock();
        if *declared == next {
            return None;
        }
        *declared = next;
        Some(self.tools_for(names))
    }

    /// The connect-time system instruction.
    pub(crate) fn base_instruction(&self) -> Option<&str> {
        self.base_instruction.as_deref()
    }

    /// The function names declared now.
    pub(crate) fn declared(&self) -> Vec<String> {
        self.declared.lock().clone()
    }

    /// Record `names` and `instruction` as what the model has, and return the
    /// update that brings it there: only the fields that changed, `None` when
    /// neither did. Each update invalidates the server's prefix cache, so
    /// nothing is sent for an unchanged list or instruction.
    pub(crate) fn sync(
        &self,
        names: &BTreeSet<String>,
        instruction: Option<String>,
    ) -> Option<ContextUpdate> {
        let tools = self.redeclare(names);
        let instruction = instruction.filter(|next| {
            let mut current = self.instruction.lock();
            if current.as_deref() == Some(next.as_str()) {
                return false;
            }
            *current = Some(next.clone());
            true
        });
        if tools.is_none() && instruction.is_none() {
            return None;
        }
        let mut update = ContextUpdate::new();
        if let Some(tools) = tools {
            update = update.tools(tools);
        }
        if let Some(instruction) = instruction {
            update = update.system_instruction(instruction);
        }
        Some(update)
    }

    /// The system instruction for a phase: the connect-time instruction with
    /// the phase's own instruction after it. A `contextUpdate` replaces the
    /// instruction outright, so the base would otherwise be lost.
    pub(crate) fn instruction_for(&self, phase_instruction: &str) -> String {
        compose_instruction(self.base_instruction.as_deref(), phase_instruction)
    }
}

/// Recompute what the current phase and flow step admit and, when it differs
/// from what is declared, record it and return the declarations to send.
/// `None` without a scope (any steering mode but `ContextUpdate`).
pub(crate) async fn rescope(
    scope: &Option<Arc<ToolScope>>,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    flow: &Option<SharedFlowStack>,
    state: &State,
) -> Option<Vec<Tool>> {
    let scope = scope.as_ref()?;
    let phase_tools = match phase_machine {
        Some(pm) => pm.lock().await.active_tools().map(<[String]>::to_vec),
        None => None,
    };
    let admitted = {
        let flow = flow.as_ref().map(|f| f.lock());
        scope.admitted(phase_tools.as_deref(), flow.as_deref(), state)
    };
    let tools = scope.redeclare(&admitted)?;
    let _ = state.session().set(DECLARED_TOOLS_KEY, scope.declared());
    Some(tools)
}

/// Re-declare `tools` on their own, outside a turn boundary.
pub(crate) async fn send_tools(writer: &Arc<dyn SessionWriter>, tools: Vec<Tool>) {
    if let Err(e) = writer
        .update_context(ContextUpdate::new().tools(tools))
        .await
    {
        tracing::warn!(error = %e, "re-declaring the tools failed");
    }
}

/// `base` followed by `phase`, either of which may be absent or empty.
pub(crate) fn compose_instruction(base: Option<&str>, phase: &str) -> String {
    match base.filter(|b| !b.trim().is_empty()) {
        Some(base) if !phase.trim().is_empty() => format!("{base}\n\n{phase}"),
        Some(base) => base.to_string(),
        None => phase.to_string(),
    }
}

/// See [`ToolScope::admitted`].
pub(crate) fn offered_names(
    catalog: &[Tool],
    phase_tools: Option<&[String]>,
    flow: Option<&FlowStack>,
    state: &State,
) -> BTreeSet<String> {
    // A flow that only observes records deviations; it does not refuse calls,
    // so it does not narrow what the model is offered either.
    let flow = flow.filter(|f| f.mode() == Enforcement::Enforce);
    catalog
        .iter()
        .filter_map(|tool| tool.function_declarations.as_ref())
        .flatten()
        .map(|decl| decl.name.as_str())
        .filter(|name| phase_tools.is_none_or(|allowed| allowed.iter().any(|t| t == name)))
        .filter(|name| flow.is_none_or(|f| f.offers_tool(name, state)))
        .map(str::to_string)
        .collect()
}

/// See [`ToolScope::tools_for`].
pub(crate) fn tools_for(catalog: &[Tool], names: &BTreeSet<String>) -> Vec<Tool> {
    catalog
        .iter()
        .filter_map(|tool| match &tool.function_declarations {
            None => Some(tool.clone()),
            Some(decls) => {
                let kept: Vec<_> = decls
                    .iter()
                    .filter(|d| names.contains(&d.name))
                    .cloned()
                    .collect();
                // A tool entry may carry more than function declarations;
                // keep the entry when anything else is set on it.
                let mut tool = tool.clone();
                if kept.is_empty() {
                    tool.function_declarations = None;
                    let other = tool.url_context.is_some()
                        || tool.google_search.is_some()
                        || tool.code_execution.is_some()
                        || tool.google_search_retrieval.is_some();
                    other.then_some(tool)
                } else {
                    tool.function_declarations = Some(kept);
                    Some(tool)
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Flow, FlowMonitor, Guard};
    use gemini_genai_rs::prelude::FunctionDeclaration;

    fn decl(name: &str) -> FunctionDeclaration {
        FunctionDeclaration {
            name: name.into(),
            description: format!("{name} tool"),
            parameters: None,
            behavior: None,
        }
    }

    fn catalog() -> Vec<Tool> {
        vec![
            Tool::functions(vec![
                decl("verify_identity"),
                decl("get_balance"),
                decl("transfer"),
            ]),
            Tool::google_search(),
        ]
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(std::string::ToString::to_string).collect()
    }

    fn declared_functions(tools: &[Tool]) -> Vec<String> {
        tools
            .iter()
            .filter_map(|t| t.function_declarations.as_ref())
            .flatten()
            .map(|d| d.name.clone())
            .collect()
    }

    #[test]
    fn without_a_phase_or_flow_every_function_is_admitted() {
        let admitted = offered_names(&catalog(), None, None, &State::new());
        assert_eq!(
            admitted,
            names(&["get_balance", "transfer", "verify_identity"])
        );
    }

    #[test]
    fn a_phase_tool_list_narrows_the_functions() {
        let phase = vec!["verify_identity".to_string(), "not_a_tool".to_string()];
        let admitted = offered_names(&catalog(), Some(&phase), None, &State::new());
        assert_eq!(admitted, names(&["verify_identity"]));
    }

    fn banking_flow(mode: Enforcement) -> FlowStack {
        let flow = Flow::new()
            .step("verify")
            .allow(["verify_identity"])
            .done(Guard::called_ok("verify_identity"))
            .step("serve")
            .after("verify")
            .allow(["get_balance"])
            .done(Guard::called_ok("get_balance"))
            .step("end")
            .after("serve")
            .terminal()
            .build()
            .expect("valid flow");
        FlowMonitor::new(flow, mode).into_stack()
    }

    #[test]
    fn an_enforcing_flow_admits_the_active_steps_tools() {
        let state = State::new();
        let mut flow = banking_flow(Enforcement::Enforce);
        let admitted = offered_names(&catalog(), None, Some(&flow), &state);
        assert_eq!(admitted, names(&["verify_identity"]));

        flow.observe_tool("verify_identity", true, &state);
        flow.on_turn(&state);
        let admitted = offered_names(&catalog(), None, Some(&flow), &state);
        assert_eq!(admitted, names(&["get_balance"]));
    }

    #[test]
    fn a_tool_waiting_on_its_guard_is_offered_but_refused() {
        // A commit waiting for the caller's yes must be declared, or the
        // model cannot call it in the turn the yes arrives.
        let flow = Flow::new()
            .step("confirm")
            .allow(["charge"])
            .done(Guard::called_ok("charge"))
            .step("end")
            .after("confirm")
            .terminal()
            .commit("charge", Guard::is_true("confirmed"))
            .build()
            .expect("valid flow");
        let stack = FlowMonitor::new(flow, Enforcement::Enforce).into_stack();
        let state = State::new();
        let catalog = vec![Tool::functions(vec![decl("charge")])];
        assert_eq!(
            offered_names(&catalog, None, Some(&stack), &state),
            names(&["charge"])
        );
        assert!(stack.admits_tool("charge", &state).is_err());
        assert_eq!(
            stack.blocking_keys("charge", &state),
            ["confirmed".to_string()].into_iter().collect()
        );
        let _ = state.set("confirmed", true);
        assert!(stack.admits_tool("charge", &state).is_ok());
        assert!(stack.blocking_keys("charge", &state).is_empty());
    }

    #[test]
    fn an_observing_flow_does_not_narrow_the_declarations() {
        let flow = banking_flow(Enforcement::Observe);
        let admitted = offered_names(&catalog(), None, Some(&flow), &State::new());
        assert_eq!(
            admitted,
            names(&["get_balance", "transfer", "verify_identity"])
        );
    }

    #[test]
    fn tools_for_keeps_non_function_tools_and_drops_empty_lists() {
        let tools = tools_for(&catalog(), &names(&["get_balance"]));
        assert_eq!(declared_functions(&tools), vec!["get_balance"]);
        assert!(tools.iter().any(|t| t.google_search.is_some()));

        let tools = tools_for(&catalog(), &BTreeSet::new());
        assert!(declared_functions(&tools).is_empty());
        assert_eq!(tools.len(), 1, "only the search tool is left");
    }

    #[test]
    fn redeclare_reports_only_a_change() {
        let scope = ToolScope::new(catalog(), None, &names(&["verify_identity"]));
        assert!(scope.redeclare(&names(&["verify_identity"])).is_none());
        let tools = scope.redeclare(&names(&["get_balance"])).expect("changed");
        assert_eq!(declared_functions(&tools), vec!["get_balance"]);
        assert_eq!(scope.declared(), vec!["get_balance"]);
    }

    #[test]
    fn a_phase_instruction_follows_the_base() {
        assert_eq!(
            compose_instruction(Some("You are a bank agent."), "Verify the caller."),
            "You are a bank agent.\n\nVerify the caller."
        );
        assert_eq!(compose_instruction(Some("Base."), " "), "Base.");
        assert_eq!(compose_instruction(None, "Phase."), "Phase.");
        assert_eq!(compose_instruction(Some(""), "Phase."), "Phase.");
    }
}
