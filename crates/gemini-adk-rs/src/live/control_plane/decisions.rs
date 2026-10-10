//! Decision points: asking the session's decision questions against the
//! rolling conversation, at the turn boundary and before a tool is admitted.

use std::sync::Arc;

use crate::decision::{Conversation, Decisions, Round};
use crate::flow::{DecisionScope, SharedFlowStack};
use crate::live::transcript::TranscriptTurn;
use crate::state::State;

/// Where a decision round runs.
pub(super) enum DecisionPoint<'a> {
    /// The caller's turn ended: ask what the flow can act on, and the
    /// standing questions.
    TurnEnd,
    /// The model called these tools: ask only what decides whether they are
    /// admitted, so a call no decision governs does not wait.
    ToolGate(&'a [String]),
}

/// Ask the questions `point` calls for about `turns`, with the active stages
/// and their grounding lines as context. The caller turn in `turns` becomes
/// the one answers count for even when nothing is asked, so an earlier
/// answer never outlives its turn. `None` when there is nothing to ask.
pub(super) async fn decision_round(
    decisions: &Arc<Decisions>,
    flow: &Option<SharedFlowStack>,
    point: DecisionPoint<'_>,
    turns: &[TranscriptTurn],
    state: &State,
) -> Option<Round> {
    let conversation = Conversation::from_turns(turns);
    conversation.begin_turn(state);
    let (scope, context, active) = match flow {
        Some(stack) => {
            let stack = stack.lock();
            let scope = match point {
                DecisionPoint::TurnEnd => stack.decision_scope(state),
                DecisionPoint::ToolGate(tools) => stack.gate_scope(tools),
            };
            let active: Vec<String> = stack
                .active_steps(state)
                .iter()
                .map(|s| s.id.clone())
                .collect();
            (scope, stack.active_grounds(state), active)
        }
        None => (DecisionScope::default(), Vec::new(), Vec::new()),
    };
    let ids = match point {
        DecisionPoint::TurnEnd => decisions.select(&scope),
        DecisionPoint::ToolGate(_) => decisions.governing(&scope),
    };
    if ids.is_empty() {
        return None;
    }
    let conversation = conversation
        .with_context(context)
        .with_active_stages(active);
    Some(decisions.ask(&ids, &conversation, state).await)
}
