//! Tool-calling agent using the GeminiAgent builder — zero manual tool dispatch.
//!
//! Compare with `function_calling_agent.rs` (105 lines) — the ToolCall event
//! handler, registry.execute_all(), and SendToolResponse are all automatic.

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    let agent = GeminiAgent::builder()
        .api_key(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck)
        .system_instruction(
            "You are a stock trading assistant. Use tools to look up prices \
             and place orders. Always confirm before placing orders.",
        )
        // Tools — auto-dispatched, no manual event loop needed
        .tool(
            "get_stock_price",
            "Get the current stock price for a ticker symbol",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "ticker": { "type": "string", "description": "Stock ticker (e.g., GOOGL)" }
                },
                "required": ["ticker"]
            })),
            |args| async move {
                let ticker = args["ticker"].as_str().ok_or("missing ticker")?;
                Ok(serde_json::json!({
                    "ticker": ticker,
                    "price": 178.52,
                    "currency": "USD",
                    "change": "+1.2%"
                }))
            },
        )
        .tool(
            "place_order",
            "Place a buy or sell order for a stock",
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "action": { "type": "string", "enum": ["buy", "sell"] },
                    "quantity": { "type": "integer" }
                },
                "required": ["ticker", "action", "quantity"]
            })),
            |args| async move {
                let order_id = uuid::Uuid::new_v4();
                Ok(serde_json::json!({
                    "order_id": order_id.to_string(),
                    "status": "confirmed",
                    "ticker": args["ticker"],
                    "action": args["action"],
                    "quantity": args["quantity"]
                }))
            },
        )
        // Callbacks
        .on_text(|t| async move { print!("{t}") })
        .on_turn_complete(|_| async move { println!("\n---") })
        .on_error(|e| async move { eprintln!("[Error]: {e}") })
        .build()
        .await?;

    println!("Stock trading agent ready!\n");

    agent.send_text("What's the current price of GOOGL?").await?;

    tokio::signal::ctrl_c().await?;
    agent.shutdown().await?;
    Ok(())
}
