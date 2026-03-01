//! Weather agent cookbook — demonstrates the runtime layer.
//!
//! Shows how to:
//! - Define an Agent with the Agent trait
//! - Register tools with TypedTool (type-safe, auto-generated schema)
//! - Configure ToolDispatcher with default timeouts
//! - Set up an AgentRegistry for multi-agent routing
//! - Use LatencyMiddleware and LogMiddleware for observability

use std::sync::Arc;

use async_trait::async_trait;
use gemini_live_runtime::agent::Agent;
use gemini_live_runtime::context::InvocationContext;
use gemini_live_runtime::error::AgentError;
use gemini_live_runtime::middleware::{LatencyMiddleware, LogMiddleware, MiddlewareChain, Middleware};
use gemini_live_runtime::router::AgentRegistry;
use gemini_live_runtime::tool::{TypedTool, ToolDispatcher};
use schemars::JsonSchema;
use serde::Deserialize;

/// Type-safe arguments for the weather tool.
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name to get weather for
    city: String,
}

/// Type-safe arguments for the forecast tool.
#[derive(Deserialize, JsonSchema)]
struct ForecastArgs {
    /// City name to get the forecast for
    city: String,
}

/// A weather agent that can look up weather and forecasts.
struct WeatherAgent {
    dispatcher: ToolDispatcher,
}

impl WeatherAgent {
    fn new() -> Self {
        // ToolDispatcher with a 10-second default timeout for all tool calls.
        let mut dispatcher = ToolDispatcher::new()
            .with_timeout(std::time::Duration::from_secs(10));

        // TypedTool auto-generates JSON Schema from the args struct.
        // No manual schema needed — field doc comments become descriptions.
        dispatcher.register_function(Arc::new(TypedTool::new(
            "get_weather",
            "Get current weather for a city",
            |args: WeatherArgs| async move {
                // In a real agent, this would call a weather API
                Ok(serde_json::json!({
                    "city": args.city,
                    "temperature_celsius": 22,
                    "condition": "Partly cloudy",
                    "humidity": 65
                }))
            },
        )));

        // Register a "get_forecast" tool
        dispatcher.register_function(Arc::new(TypedTool::new(
            "get_forecast",
            "Get 3-day weather forecast for a city",
            |args: ForecastArgs| async move {
                Ok(serde_json::json!({
                    "city": args.city,
                    "forecast": [
                        {"day": "Today", "high": 22, "low": 15, "condition": "Partly cloudy"},
                        {"day": "Tomorrow", "high": 25, "low": 17, "condition": "Sunny"},
                        {"day": "Day after", "high": 20, "low": 14, "condition": "Rain"}
                    ]
                }))
            },
        )));

        Self { dispatcher }
    }
}

#[async_trait]
impl Agent for WeatherAgent {
    fn name(&self) -> &str {
        "weather"
    }

    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        // In production, this would listen for SessionEvents and dispatch tool calls.
        // See the runtime_agent example for the full pattern.
        println!("WeatherAgent is ready with {} tools (default timeout: {:?})",
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
        println!("GreetingAgent: Hello! How can I help you today?");
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    println!("=== Weather Agent Cookbook ===\n");

    // 1. Create agents
    let weather = Arc::new(WeatherAgent::new());
    let greeter = Arc::new(GreetingAgent);

    // 2. Register in a routing registry
    let mut registry = AgentRegistry::new();
    registry.register(weather.clone());
    registry.register(greeter.clone());

    println!("Registered agents: {:?}", registry.names());
    println!("Weather agent tools: {}", weather.tools().len());

    // 3. Set up middleware for observability
    let latency = Arc::new(LatencyMiddleware::new());
    let log = Arc::new(LogMiddleware::new());
    let mut chain = MiddlewareChain::new();
    chain.add(log);
    chain.add(latency.clone());

    println!("\nMiddleware chain: {} layers", chain.len());

    // 4. Demonstrate tool dispatch with auto-generated schema
    let result = weather
        .dispatcher
        .call_function("get_weather", serde_json::json!({"city": "San Francisco"}))
        .await;

    match result {
        Ok(val) => println!(
            "\nWeather in {}: {}°C, {}",
            val["city"], val["temperature_celsius"], val["condition"]
        ),
        Err(e) => eprintln!("Tool error: {e}"),
    }

    let result = weather
        .dispatcher
        .call_function("get_forecast", serde_json::json!({"city": "Tokyo"}))
        .await;

    match result {
        Ok(val) => {
            println!("\nForecast for {}:", val["city"]);
            if let Some(days) = val["forecast"].as_array() {
                for day in days {
                    println!(
                        "  {}: {}°C / {}°C — {}",
                        day["day"], day["high"], day["low"], day["condition"]
                    );
                }
            }
        }
        Err(e) => eprintln!("Tool error: {e}"),
    }

    // 5. Demonstrate LatencyMiddleware by manually invoking hooks
    //    (In production, the runtime calls these automatically.)
    let test_call = gemini_live_wire::prelude::FunctionCall {
        name: "get_weather".to_string(),
        args: serde_json::json!({"city": "Berlin"}),
        id: None,
    };
    let _ = latency.before_tool(&test_call).await;
    let mock_result = serde_json::json!({"temp": 18});
    let _ = latency.after_tool(&test_call, &mock_result).await;

    println!("\nLatency records:");
    for record in latency.tool_latencies() {
        println!("  Tool '{}': {:?} (success: {})", record.name, record.elapsed, record.success);
    }

    // 6. Agent transfer routing
    println!("\nAgent routing:");
    if let Some(agent) = registry.resolve("weather") {
        println!("  Resolved 'weather' -> {} ({} tools)", agent.name(), agent.tools().len());
    }
    if let Some(agent) = registry.resolve("greeter") {
        println!("  Resolved 'greeter' -> {} ({} tools)", agent.name(), agent.tools().len());
    }
    assert!(registry.resolve("nonexistent").is_none());
    println!("  'nonexistent' -> None (correct)");
}
