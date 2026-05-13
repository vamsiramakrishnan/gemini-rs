use gemini_adk_rs::live::{
    ControlContract, ExtractorContract, PhaseContract, PreparationContract, PromotionContract,
    RuntimeContract, ToolContract, TransitionContract,
};
use gemini_genai_rs::prelude::Tool;

use super::Live;

impl Live {
    /// Describe the configured runtime contract before the session connects.
    ///
    /// The contract is intended for DevTools, replay validation, and generated
    /// docs. It is metadata only; predicates and callbacks are represented by
    /// stable names or boolean capabilities rather than executable closures.
    pub fn describe_contract(&self) -> RuntimeContract {
        let mut tools = describe_tools(&self.config.tools);
        if let Some(dispatcher) = &self.dispatcher {
            tools.extend(describe_tools(&dispatcher.to_tool_declarations()));
        }
        tools.extend(self.deferred_agent_tools.iter().map(|tool| ToolContract {
            name: tool.name.clone(),
            description: tool.description.clone(),
            behavior: Some("AgentTool".into()),
        }));

        RuntimeContract {
            version: 1,
            model: self.config.model.to_string(),
            tools,
            phases: self.phases.iter().map(describe_phase).collect(),
            initial_phase: self.initial_phase.clone(),
            extractors: self
                .extractors
                .iter()
                .map(|extractor| ExtractorContract {
                    name: extractor.name().to_string(),
                    window_size: extractor.window_size(),
                    trigger: format!("{:?}", extractor.trigger()),
                    promotions: extractor
                        .promotion_rules()
                        .iter()
                        .map(|rule| PromotionContract {
                            field: rule.field.clone(),
                            state_key: rule.state_key.clone(),
                            merge: format!("{:?}", rule.merge),
                            has_predicate: rule.accept.is_some(),
                        })
                        .collect(),
                })
                .collect(),
            computed: self.computed.describe(),
            watchers: self.watchers.describe(),
            controls: ControlContract {
                soft_turn_timeout_ms: self
                    .soft_turn_timeout
                    .map(|timeout| timeout.as_millis() as u64),
                steering_mode: format!("{:?}", self.steering_mode),
                context_delivery: format!("{:?}", self.context_delivery),
                tool_advisory: self.tool_advisory,
                telemetry_interval_ms: self
                    .telemetry_interval
                    .map(|interval| interval.as_millis() as u64),
                repair_enabled: self.repair_config.is_some(),
                persistence_enabled: self.persistence.is_some(),
            },
        }
    }
}

fn describe_phase(phase: &gemini_adk_rs::live::Phase) -> PhaseContract {
    PhaseContract {
        name: phase.name.clone(),
        terminal: phase.terminal,
        tools_enabled: phase.tools_enabled.clone(),
        needs: phase.needs.clone(),
        requires: phase.requires.clone(),
        preparations: phase
            .preparations
            .iter()
            .map(|prep| PreparationContract {
                name: prep.name.clone(),
                produces: prep.produces.clone(),
            })
            .collect(),
        presents: phase.presents.clone(),
        clear_on_enter: phase.clear_on_enter.clone(),
        transitions: phase
            .transitions
            .iter()
            .map(|transition| TransitionContract {
                target: transition.target.clone(),
                description: transition.description.clone(),
                has_guard: true,
            })
            .collect(),
        has_guard: phase.guard.is_some(),
        prompt_on_enter: phase.prompt_on_enter,
    }
}

fn describe_tools(tools: &[Tool]) -> Vec<ToolContract> {
    let mut described = Vec::new();
    for tool in tools {
        if let Some(functions) = &tool.function_declarations {
            described.extend(functions.iter().map(|function| {
                ToolContract {
                    name: function.name.clone(),
                    description: function.description.clone(),
                    behavior: function
                        .behavior
                        .as_ref()
                        .map(|behavior| format!("{behavior:?}")),
                }
            }));
        }
        if tool.google_search.is_some() {
            described.push(ToolContract {
                name: "google_search".into(),
                description: "Google Search grounding".into(),
                behavior: None,
            });
        }
        if tool.code_execution.is_some() {
            described.push(ToolContract {
                name: "code_execution".into(),
                description: "Code execution".into(),
                behavior: None,
            });
        }
        if tool.url_context.is_some() {
            described.push(ToolContract {
                name: "url_context".into(),
                description: "URL context retrieval".into(),
                behavior: None,
            });
        }
        if tool.google_search_retrieval.is_some() {
            described.push(ToolContract {
                name: "google_search_retrieval".into(),
                description: "Google Search retrieval".into(),
                behavior: None,
            });
        }
    }
    described
}
