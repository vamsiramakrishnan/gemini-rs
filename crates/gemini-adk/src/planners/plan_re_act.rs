//! Plan-ReAct planner — structures LLM output through planning-then-action.
//!
//! Mirrors ADK-Python's `PlanReActPlanner`. Forces the model to generate
//! explicit plans using tagged sections before executing actions.

use async_trait::async_trait;

use super::{Planner, PlannerError};
use crate::llm::LlmRequest;

/// Tags used to structure the model's planning output.
const TAG_PLANNING: &str = "/*PLANNING*/";
const TAG_REPLANNING: &str = "/*REPLANNING*/";
const TAG_REASONING: &str = "/*REASONING*/";
const TAG_ACTION: &str = "/*ACTION*/";
const TAG_FINAL_ANSWER: &str = "/*FINAL_ANSWER*/";

/// Plan-ReAct planner that constrains the model to plan before acting.
///
/// The model is instructed to use specific tags to separate planning,
/// reasoning, action, and final answer sections. The planner then
/// filters the response to preserve only relevant sections.
#[derive(Debug, Clone)]
pub struct PlanReActPlanner {
    /// Whether to include tool use instructions.
    include_tool_instructions: bool,
}

impl PlanReActPlanner {
    /// Create a new Plan-ReAct planner.
    pub fn new() -> Self {
        Self {
            include_tool_instructions: true,
        }
    }

    /// Set whether to include tool use instructions in the planning prompt.
    pub fn with_tool_instructions(mut self, include: bool) -> Self {
        self.include_tool_instructions = include;
        self
    }
}

impl Default for PlanReActPlanner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Planner for PlanReActPlanner {
    fn build_planning_instruction(
        &self,
        _request: &LlmRequest,
    ) -> Result<Option<String>, PlannerError> {
        let mut instruction = format!(
            r#"For every turn, you must follow the format below and use these exact tags to organize your output:

1. {TAG_PLANNING} — Create a natural language plan for how to approach the query. Plans should be:
   - Coherent and cover all aspects of the query
   - Decomposed into numbered steps
   - Aware of available tools and their capabilities

2. {TAG_REASONING} — For each step in your plan, explain your reasoning before taking action.

3. {TAG_ACTION} — Execute one step at a time using available tools when needed.

4. {TAG_REPLANNING} — If new information changes your approach, create an updated plan.

5. {TAG_FINAL_ANSWER} — After completing all steps, provide the final answer."#
        );

        if self.include_tool_instructions {
            instruction.push_str(
                "\n\nWhen using tools:\n\
                 - Only use tools that are available to you\n\
                 - Write self-contained tool calls\n\
                 - Prefer using information from previous tool results over making redundant calls",
            );
        }

        Ok(Some(instruction))
    }

    fn process_planning_response(
        &self,
        response_text: &str,
    ) -> Result<Option<String>, PlannerError> {
        // Extract and keep only the meaningful sections
        let mut filtered = String::new();
        let mut in_planning = false;
        let mut in_reasoning = false;

        for line in response_text.lines() {
            let trimmed = line.trim();

            if trimmed.contains(TAG_PLANNING) || trimmed.contains(TAG_REPLANNING) {
                in_planning = true;
                in_reasoning = false;
                continue;
            }
            if trimmed.contains(TAG_REASONING) {
                in_reasoning = true;
                in_planning = false;
                continue;
            }
            if trimmed.contains(TAG_ACTION) || trimmed.contains(TAG_FINAL_ANSWER) {
                in_planning = false;
                in_reasoning = false;
                // Keep action and final answer lines
                if !filtered.is_empty() {
                    filtered.push('\n');
                }
                filtered.push_str(line);
                continue;
            }

            if in_planning || in_reasoning {
                // Planning and reasoning are treated as thoughts — kept but annotated
                if !filtered.is_empty() {
                    filtered.push('\n');
                }
                filtered.push_str(line);
            } else {
                // Regular content — keep as is
                if !filtered.is_empty() {
                    filtered.push('\n');
                }
                filtered.push_str(line);
            }
        }

        if filtered.trim().is_empty() || filtered == response_text {
            Ok(None)
        } else {
            Ok(Some(filtered))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_instruction_with_tags() {
        let planner = PlanReActPlanner::new();
        let request = LlmRequest::default();
        let instruction = planner.build_planning_instruction(&request).unwrap();
        let text = instruction.unwrap();
        assert!(text.contains(TAG_PLANNING));
        assert!(text.contains(TAG_REASONING));
        assert!(text.contains(TAG_ACTION));
        assert!(text.contains(TAG_FINAL_ANSWER));
    }

    #[test]
    fn instruction_includes_tool_guidance() {
        let planner = PlanReActPlanner::new().with_tool_instructions(true);
        let request = LlmRequest::default();
        let text = planner
            .build_planning_instruction(&request)
            .unwrap()
            .unwrap();
        assert!(text.contains("Only use tools"));
    }

    #[test]
    fn instruction_without_tool_guidance() {
        let planner = PlanReActPlanner::new().with_tool_instructions(false);
        let request = LlmRequest::default();
        let text = planner
            .build_planning_instruction(&request)
            .unwrap()
            .unwrap();
        assert!(!text.contains("Only use tools"));
    }

    #[test]
    fn process_passthrough_plain_text() {
        let planner = PlanReActPlanner::new();
        let result = planner
            .process_planning_response("Just a plain response")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn process_filters_tagged_response() {
        let planner = PlanReActPlanner::new();
        let response = format!(
            "{TAG_PLANNING}\nStep 1: Search\nStep 2: Summarize\n{TAG_ACTION}\nSearching...\n{TAG_FINAL_ANSWER}\nThe answer is 42."
        );
        let result = planner.process_planning_response(&response).unwrap();
        // Should produce a filtered version (not None since it's different from input)
        assert!(result.is_some());
    }
}
