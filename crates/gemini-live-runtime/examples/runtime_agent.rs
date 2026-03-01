//! Runtime agent example — using gemini-live-runtime.
//!
//! Demonstrates the Agent trait, TypedTool (type-safe with auto-generated
//! JSON Schema), and ToolDispatcher with configurable timeouts.
//!
//! This example shows the structure — it won't run without a live
//! Gemini API connection, but illustrates the programming model.

use std::sync::Arc;

use gemini_live_runtime::agent::Agent;
use gemini_live_runtime::context::InvocationContext;
use gemini_live_runtime::error::AgentError;
use gemini_live_runtime::router::AgentRegistry;
use gemini_live_runtime::tool::{TypedTool, ToolDispatcher};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

/// Type-safe arguments for the weather tool — schema is auto-generated
/// from this struct via `schemars::JsonSchema`.
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name to get weather for
    city: String,
}

/// A weather agent that can look up weather data.
struct WeatherAgent {
    dispatcher: ToolDispatcher,
}

impl WeatherAgent {
    fn new() -> Self {
        // TypedTool auto-generates JSON Schema from WeatherArgs — no manual
        // schema needed. The doc comment on `city` becomes the field description.
        let weather_tool = TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |args: WeatherArgs| async move {
                Ok(serde_json::json!({
                    "city": args.city,
                    "temperature": "22°C",
                    "condition": "Sunny"
                }))
            },
        );

        // ToolDispatcher::with_timeout sets the default timeout for all tool
        // calls. Individual calls can still override via call_function_with_timeout().
        let mut dispatcher = ToolDispatcher::new()
            .with_timeout(std::time::Duration::from_secs(10));
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
        println!("WeatherAgent running with {} tools (default timeout: {:?})",
            self.dispatcher.len(),
            self.dispatcher.default_timeout(),
        );
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
