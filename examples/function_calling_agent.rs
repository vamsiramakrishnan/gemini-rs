//! Agent with tool use — registers functions and auto-dispatches tool calls.

use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    TelemetryConfig::default().init()?;

    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    // Build function registry
    let mut registry = FunctionRegistry::new();

    registry.register(
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
    );

    registry.register(
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
    );

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck)
        .system_instruction(
            "You are a stock trading assistant. Use the available tools to look up \
             prices and place orders. Always confirm with the user before placing orders.",
        )
        .add_tool(registry.to_tool_declaration())
        .tool_config(ToolConfig::auto());

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("Stock trading agent ready!\n");

    // Auto-dispatch tool calls through the registry
    let mut events = session.subscribe();
    let cmd_tx = session.command_tx.clone();

    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::ToolCall(calls) => {
                    println!("[Tool calls: {}]", calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "));
                    let responses = registry.execute_all(&calls).await;
                    let _ = cmd_tx
                        .send(SessionCommand::SendToolResponse(responses))
                        .await;
                }
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                SessionEvent::Disconnected(_) => break,
                _ => {}
            }
        }
    });

    // Send initial prompt
    session
        .send_text("What's the current price of GOOGL?")
        .await?;

    // Keep alive until Ctrl+C
    tokio::signal::ctrl_c().await?;
    session.disconnect().await?;
    Ok(())
}
