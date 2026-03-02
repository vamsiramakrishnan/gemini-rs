use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_genai::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, send_app_meta, wait_for_start};

/// Function calling with Gemini Live.
pub struct ToolCalling;

/// Build the demo tool declarations.
fn demo_tools() -> Tool {
    Tool::functions(vec![
        FunctionDeclaration {
            name: "get_weather".into(),
            description: "Get the current weather for a given city.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "city": {
                        "type": "string",
                        "description": "The city name, e.g. 'San Francisco'"
                    }
                },
                "required": ["city"]
            })),
        },
        FunctionDeclaration {
            name: "get_time".into(),
            description: "Get the current time in a given timezone.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "timezone": {
                        "type": "string",
                        "description": "IANA timezone string, e.g. 'America/New_York'"
                    }
                },
                "required": ["timezone"]
            })),
        },
        FunctionDeclaration {
            name: "calculate".into(),
            description: "Evaluate a simple arithmetic expression.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "A simple math expression, e.g. '2 + 3 * 4'"
                    }
                },
                "required": ["expression"]
            })),
        },
    ])
}

/// Execute a demo tool call and return the result as JSON.
fn execute_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
    match name {
        "get_weather" => {
            let city = args
                .get("city")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            // Return mock weather data.
            json!({
                "city": city,
                "temperature_f": 72,
                "temperature_c": 22,
                "condition": "Partly cloudy",
                "humidity": 65,
                "wind_mph": 8
            })
        }
        "get_time" => {
            let timezone = args
                .get("timezone")
                .and_then(|v| v.as_str())
                .unwrap_or("UTC");
            // Return current time as Unix timestamp (real impl would use the timezone).
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            json!({
                "timezone": timezone,
                "unix_timestamp": now.as_secs(),
                "note": "Demo: returns Unix timestamp. A real implementation would convert to the requested timezone."
            })
        }
        "calculate" => {
            let expression = args
                .get("expression")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Very simple evaluator for demo purposes — parse basic arithmetic.
            let result = evaluate_simple_expr(expression);
            json!({
                "expression": expression,
                "result": result
            })
        }
        _ => {
            json!({ "error": format!("Unknown function: {name}") })
        }
    }
}

/// Very basic arithmetic expression evaluator for demo purposes.
/// Supports +, -, *, / with integers and floats. No parentheses.
fn evaluate_simple_expr(expr: &str) -> f64 {
    // Tokenize into numbers and operators.
    let mut nums: Vec<f64> = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    let mut current = String::new();

    for ch in expr.chars() {
        if ch == '+' || ch == '-' || ch == '*' || ch == '/' {
            if !current.is_empty() {
                if let Ok(n) = current.trim().parse::<f64>() {
                    nums.push(n);
                }
                current.clear();
            }
            ops.push(ch);
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.trim().parse::<f64>() {
            nums.push(n);
        }
    }

    if nums.is_empty() {
        return 0.0;
    }

    // Evaluate * and / first (left to right).
    let mut i = 0;
    while i < ops.len() {
        if ops[i] == '*' || ops[i] == '/' {
            let result = if ops[i] == '*' {
                nums[i] * nums[i + 1]
            } else if nums[i + 1] != 0.0 {
                nums[i] / nums[i + 1]
            } else {
                f64::NAN
            };
            nums[i] = result;
            nums.remove(i + 1);
            ops.remove(i);
        } else {
            i += 1;
        }
    }

    // Evaluate + and -.
    let mut result = nums[0];
    for (i, op) in ops.iter().enumerate() {
        match op {
            '+' => result += nums[i + 1],
            '-' => result -= nums[i + 1],
            _ => {}
        }
    }

    result
}

#[async_trait]
impl CookbookApp for ToolCalling {
    fn name(&self) -> &str {
        "tool-calling"
    }

    fn description(&self) -> &str {
        "Function calling with Gemini Live"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Basic
    }

    fn features(&self) -> Vec<String> {
        vec!["text".into(), "tools".into()]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        // Build session config for text-only with tools.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .text_only()
            .add_tool(demo_tools())
            .system_instruction(
                start.system_instruction.as_deref().unwrap_or(
                    "You are a helpful assistant with access to tools. \
                     You can check the weather, get the current time, and calculate arithmetic expressions. \
                     Use the available tools when appropriate.",
                ),
            );

        // Connect to Gemini Live.
        let handle = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        handle.wait_for_phase(SessionPhase::Active).await;
        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("ToolCalling session connected");

        // Subscribe to server events.
        let mut events = handle.subscribe();

        loop {
            tokio::select! {
                // Client -> Gemini
                client_msg = rx.recv() => {
                    match client_msg {
                        Some(ClientMessage::Text { text }) => {
                            if let Err(e) = handle.send_text(&text).await {
                                warn!("Failed to send text: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Some(ClientMessage::Stop) | None => {
                            info!("ToolCalling session stopping");
                            let _ = handle.disconnect().await;
                            break;
                        }
                        _ => {} // Ignore audio messages in text mode
                    }
                }

                // Gemini -> Client
                event = recv_event(&mut events) => {
                    match event {
                        Some(SessionEvent::ToolCall(calls)) => {
                            info!("Tool calls received: {}", calls.len());

                            // Execute each tool call and collect responses.
                            let responses: Vec<FunctionResponse> = calls
                                .iter()
                                .map(|call| {
                                    let result = execute_tool(&call.name, &call.args);
                                    info!("Tool '{}' -> {}", call.name, result);

                                    // Notify the browser about the tool execution.
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: format!("tool:{}", call.name),
                                        value: json!({
                                            "name": call.name,
                                            "args": call.args,
                                            "result": result,
                                        }),
                                    });

                                    FunctionResponse {
                                        name: call.name.clone(),
                                        response: result,
                                        id: call.id.clone(),
                                    }
                                })
                                .collect();

                            // Send tool responses back to Gemini.
                            if let Err(e) = handle.send_tool_response(responses).await {
                                warn!("Failed to send tool response: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Some(SessionEvent::TextDelta(text)) => {
                            let _ = tx.send(ServerMessage::TextDelta { text });
                        }
                        Some(SessionEvent::TextComplete(text)) => {
                            let _ = tx.send(ServerMessage::TextComplete { text });
                        }
                        Some(SessionEvent::TurnComplete) => {
                            let _ = tx.send(ServerMessage::TurnComplete);
                        }
                        Some(SessionEvent::Interrupted) => {
                            let _ = tx.send(ServerMessage::Interrupted);
                        }
                        Some(SessionEvent::Error(msg)) => {
                            let _ = tx.send(ServerMessage::Error { message: msg });
                        }
                        Some(SessionEvent::Disconnected(_)) => {
                            info!("ToolCalling session disconnected by server");
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_addition() {
        assert!((evaluate_simple_expr("2 + 3") - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_multiplication_first() {
        assert!((evaluate_simple_expr("2 + 3 * 4") - 14.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_division() {
        assert!((evaluate_simple_expr("10 / 2") - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_mixed() {
        assert!((evaluate_simple_expr("10 - 2 * 3 + 1") - 5.0).abs() < f64::EPSILON);
    }
}
