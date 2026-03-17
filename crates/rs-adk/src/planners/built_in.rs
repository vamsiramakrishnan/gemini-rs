//! Built-in planner — delegates planning to the model's native capabilities.
//!
//! When the model supports native planning (e.g., Gemini with thinking),
//! this planner injects minimal instructions and lets the model plan natively.

use async_trait::async_trait;

use super::{Planner, PlannerError};
use crate::llm::LlmRequest;

/// Built-in planner that leverages the model's native planning capabilities.
///
/// This is a lightweight planner that adds a simple planning instruction
/// to encourage the model to think step-by-step before acting.
#[derive(Debug, Clone, Default)]
pub struct BuiltInPlanner {
    /// Optional custom planning instruction override.
    custom_instruction: Option<String>,
}

impl BuiltInPlanner {
    /// Create a new built-in planner with default instructions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a built-in planner with a custom planning instruction.
    pub fn with_instruction(instruction: impl Into<String>) -> Self {
        Self {
            custom_instruction: Some(instruction.into()),
        }
    }
}

const DEFAULT_PLANNING_INSTRUCTION: &str = "\
Before taking any action, think step by step about what you need to do. \
Create a brief plan, then execute it. If you need to adjust your plan based \
on new information, explain your reasoning before changing course.";

#[async_trait]
impl Planner for BuiltInPlanner {
    fn build_planning_instruction(
        &self,
        _request: &LlmRequest,
    ) -> Result<Option<String>, PlannerError> {
        Ok(Some(self.custom_instruction.clone().unwrap_or_else(|| {
            DEFAULT_PLANNING_INSTRUCTION.to_string()
        })))
    }

    fn process_planning_response(
        &self,
        _response_text: &str,
    ) -> Result<Option<String>, PlannerError> {
        // Built-in planner doesn't filter responses — model handles planning natively
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_instruction() {
        let planner = BuiltInPlanner::new();
        let request = LlmRequest::default();
        let instruction = planner.build_planning_instruction(&request).unwrap();
        assert!(instruction.is_some());
        assert!(instruction.unwrap().contains("step by step"));
    }

    #[test]
    fn custom_instruction() {
        let planner = BuiltInPlanner::with_instruction("Plan carefully");
        let request = LlmRequest::default();
        let instruction = planner.build_planning_instruction(&request).unwrap();
        assert_eq!(instruction.unwrap(), "Plan carefully");
    }

    #[test]
    fn response_passthrough() {
        let planner = BuiltInPlanner::new();
        let result = planner.process_planning_response("some response").unwrap();
        assert!(result.is_none());
    }
}
