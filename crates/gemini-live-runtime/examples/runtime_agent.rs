//! Runtime agent example — using gemini-live-runtime.
//!
//! Demonstrates the Agent trait, ToolDispatcher, and SimpleTool
//! for building agents with function calling capabilities.
//!
//! This example shows the structure — it won't run without a live
//! Gemini API connection, but illustrates the programming model.

use std::sync::Arc;

use gemini_live_runtime::agent::Agent;
use gemini_live_runtime::context::InvocationContext;
use gemini_live_runtime::error::AgentError;
use gemini_live_runtime::router::AgentRegistry;
use gemini_live_runtime::tool::{SimpleTool, ToolDispatcher};

use async_trait::async_trait;

/// A weather agent that can look up weather data.
struct WeatherAgent {
    dispatcher: ToolDispatcher,
}

impl WeatherAgent {
    fn new() -> Self {
        let mut dispatcher = ToolDispatcher::new();

        let weather_tool = SimpleTool::new(
            "get_weather",
            "Get current weather for a city",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "City name" }
                },
                "required": ["city"]
            })),
            |args| async move {
                let city = args["city"].as_str().unwrap_or("unknown");
                Ok(serde_json::json!({
                    "city": city,
                    "temperature": "22°C",
                    "condition": "Sunny"
                }))
            },
        );

        dispatcher.register_function(Arc::new(weather_tool));

        Self { dispatcher }
    }
}

#[async_trait]
impl Agent for WeatherAgent {
    fn name(&self) -> &str {
        "weather"
    }

    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        // In a real implementation, this would:
        // 1. Listen for SessionEvents via ctx.agent_session
        // 2. When a FunctionCall arrives, dispatch to self.dispatcher
        // 3. Send tool responses back via ctx.agent_session
        // 4. Emit AgentEvents for observability
        println!("WeatherAgent running with {} tools", self.dispatcher.len());
        Ok(())
    }

    fn tools(&self) -> Vec<gemini_live_wire::prelude::Tool> {
        self.dispatcher.to_tool_declarations()
    }
}

/// A greeting agent — simple, no tools.
struct GreetingAgent;

#[async_trait]
impl Agent for GreetingAgent {
    fn name(&self) -> &str {
        "greeter"
    }

    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        println!("GreetingAgent: Hello!");
        Ok(())
    }
}

fn main() {
    // Register agents for routing
    let mut registry = AgentRegistry::new();
    registry.register(Arc::new(WeatherAgent::new()));
    registry.register(Arc::new(GreetingAgent));

    println!("Registered agents: {:?}", registry.names());

    // Resolve by name (used for agent transfer)
    if let Some(agent) = registry.resolve("weather") {
        println!("Resolved '{}' with {} tools", agent.name(), agent.tools().len());
    }
}
