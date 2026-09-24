use gemini_adk_fluent_rs::prelude::*;

/// The model's answer, as a Rust type: its JSON Schema is the response schema.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct City {
    name: String,
    country: String,
    /// Population, in millions.
    population_millions: f64,
}

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("geographer").build(GeminiLlm::from_env()?)?;

    let city: City = agent.ask_as("Describe the largest city in Japan.").await?;
    println!(
        "{}, {}: {:.1} million people",
        city.name, city.country, city.population_millions
    );
    Ok(())
}
