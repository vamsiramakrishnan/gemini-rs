//! Planner system — enables agents to generate plans before acting.
//!
//! Mirrors ADK-Python's `planners` module. Provides traits for building
//! planning instructions and processing planning responses, plus a
//! Plan-ReAct planner implementation.

mod built_in;
mod plan_re_act;

pub use built_in::BuiltInPlanner;
pub use plan_re_act::PlanReActPlanner;

use async_trait::async_trait;

use crate::llm::LlmRequest;

/// Errors from planner operations.
#[derive(Debug, thiserror::Error)]
pub enum PlannerError {
    /// Planning instruction generation failed.
    #[error("Planning instruction error: {0}")]
    Instruction(String),
    /// Planning response processing failed.
    #[error("Planning response error: {0}")]
    Response(String),
}

/// Trait for agent planners that guide reasoning and action.
///
/// A planner modifies the agent's LLM request to inject planning instructions
/// and post-processes the LLM response to extract/filter planning steps.
#[async_trait]
pub trait Planner: Send + Sync {
    /// Build planning instructions to inject into the LLM request.
    ///
    /// Returns `Some(instruction)` to append to the system instruction,
    /// or `None` to skip planning for this request.
    fn build_planning_instruction(
        &self,
        request: &LlmRequest,
    ) -> Result<Option<String>, PlannerError>;

    /// Process the LLM response from a planning-augmented request.
    ///
    /// Can filter, reorder, or annotate response parts.
    /// Returns `None` to use the response as-is.
    fn process_planning_response(
        &self,
        response_text: &str,
    ) -> Result<Option<String>, PlannerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn Planner) {}

    #[test]
    fn planner_error_display() {
        let err = PlannerError::Instruction("test".into());
        assert!(err.to_string().contains("test"));
    }
}
