use gemini_adk_fluent_rs::prelude::*;

/// Get the current weather for a city.
///
/// # Arguments
///
/// * `city` - The city name, e.g. "Paris".
#[tool]
async fn get_weather(city: String) -> serde_json::Value {
    // A real tool would call a weather service here.
    serde_json::json!({ "city": city, "condition": "rain", "celsius": 14 })
}

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("weather")
        .instruction("Answer weather questions using your tool.")
        .tool(get_weather())
        .build(GeminiLlm::from_env()?)?;

    println!("{}", agent.ask("Do I need an umbrella in Paris?").await?);
    Ok(())
}
