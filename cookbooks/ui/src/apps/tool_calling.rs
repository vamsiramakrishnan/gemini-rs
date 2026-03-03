use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

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

    fn tips(&self) -> Vec<String> {
        vec![
            "Three demo tools available: get_weather, get_time, calculate".into(),
            "Watch the devtools State tab to see tool call arguments and results".into(),
            "Tools return mock data — try asking follow-up questions about the results".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "What's the weather in San Francisco?".into(),
            "What time is it in Tokyo?".into(),
            "Calculate 15 * 7 + 23".into(),
        ]
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

        // Build Live session with callbacks.
        let tx_text = tx.clone();
        let tx_text_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupted = tx.clone();
        let tx_error = tx.clone();
        let tx_disconnected = tx.clone();
        let tx_tool = tx.clone();

        let handle = Live::builder()
            .on_text(move |t| {
                let _ = tx_text.send(ServerMessage::TextDelta {
                    text: t.to_string(),
                });
            })
            .on_text_complete(move |t| {
                let _ = tx_text_complete.send(ServerMessage::TextComplete {
                    text: t.to_string(),
                });
            })
            .on_tool_call(move |calls, _state| {
                let tx = tx_tool.clone();
                async move {
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
                            let _ = tx.send(ServerMessage::ToolCallEvent {
                                name: call.name.clone(),
                                args: serde_json::to_string(&call.args).unwrap_or_default(),
                                result: serde_json::to_string(&result).unwrap_or_default(),
                            });

                            FunctionResponse {
                                name: call.name.clone(),
                                response: result,
                                id: call.id.clone(),
                            }
                        })
                        .collect();

                    Some(responses)
                }
            })
            .on_turn_complete(move || {
                let tx = tx_turn.clone();
                async move {
                    let _ = tx.send(ServerMessage::TurnComplete);
                }
            })
            .on_interrupted(move || {
                let tx = tx_interrupted.clone();
                async move {
                    let _ = tx.send(ServerMessage::Interrupted);
                }
            })
            .on_error(move |msg| {
                let tx = tx_error.clone();
                async move {
                    let _ = tx.send(ServerMessage::Error { message: msg });
                }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("ToolCalling session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("ToolCalling session connected");

        // Browser -> Gemini loop.
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                    }
                }
                ClientMessage::Stop => {
                    info!("ToolCalling session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {} // Ignore audio messages in text mode
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
