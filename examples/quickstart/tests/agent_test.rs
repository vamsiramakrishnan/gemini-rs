use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::{LlmResponse, MockLlm};

/// Look up the status of an order.
///
/// # Arguments
///
/// * `id` - The order id, e.g. "A-17".
#[tool]
async fn order_status(id: String) -> String {
    format!("order {id} has shipped")
}

#[tokio::test]
async fn the_agent_checks_the_order_before_answering() -> Result<(), Box<dyn std::error::Error>> {
    let llm = MockLlm::script([
        LlmResponse::tool_call("order_status", serde_json::json!({ "id": "A-17" })),
        LlmResponse::from_text("Your order has shipped."),
    ]);
    let agent = AgentBuilder::new("support")
        .tool(order_status())
        .build(llm.clone())?;

    assert_eq!(
        agent.ask("Where is order A-17?").await?,
        "Your order has shipped."
    );

    // The tool ran, and its result went back to the model.
    let followup = serde_json::to_string(&llm.last_request().unwrap().contents)?;
    assert!(followup.contains("order A-17 has shipped"));
    Ok(())
}
