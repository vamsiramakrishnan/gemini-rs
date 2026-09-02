//! Tool Calling example — a Live session with typed function calling.
//!
//! Tools are plain `async fn`s under the `#[tool]` attribute: the macro
//! derives the JSON Schema from the parameter list and registers the
//! function with the session's dispatcher via `.tool(..)`. The runtime
//! executes each call the model makes and sends the response back; the
//! `on_tool_call` hook only tells the browser what is being called.
//!
//! Usage:
//!   cargo run -p example-tool-calling
//!   # then open http://127.0.0.1:3003

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use futures::{sink::SinkExt, stream::StreamExt};
use gemini_adk_fluent_rs::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Tools — `#[tool]` turns each async fn into a registrable ToolFunction
// ---------------------------------------------------------------------------

/// Mock weather lookup; a real implementation would call a weather API.
#[tool("Get current weather for a city including temperature, condition, and humidity")]
async fn get_weather(city: String) -> Result<Value, ToolError> {
    info!("Tool called: get_weather(city={city})");
    Ok(json!({
        "city": city,
        "temperature_celsius": 22,
        "condition": "Partly cloudy",
        "humidity_percent": 65,
        "wind_speed_kmh": 12
    }))
}

/// Evaluates a basic arithmetic expression such as `"2 + 3 * 4"`.
#[tool("Evaluate a mathematical expression and return the result")]
async fn calculate(expression: String) -> Result<Value, ToolError> {
    info!("Tool called: calculate(expr={expression})");
    Ok(match eval_simple_expression(&expression) {
        Some(result) => json!({ "expression": expression, "result": result }),
        None => json!({
            "expression": expression,
            "error": "Could not evaluate expression. Supported: basic arithmetic (+, -, *, /)"
        }),
    })
}

/// Simple arithmetic expression evaluator for demo purposes.
/// Supports: integer/float literals with +, -, *, / operators (* and / bind tighter).
fn eval_simple_expression(expr: &str) -> Option<f64> {
    // Strip whitespace and try to parse as a simple sequence of operations
    let expr = expr.trim();

    // Try parsing as a single number first
    if let Ok(n) = expr.parse::<f64>() {
        return Some(n);
    }

    // Tokenize: split into numbers and operators
    let mut chars = expr.chars().peekable();
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();

    while let Some(&ch) = chars.peek() {
        if ch == '+' || ch == '*' || ch == '/' || (ch == '-' && !current.is_empty()) {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            tokens.push(ch.to_string());
            chars.next();
        } else if ch.is_ascii_digit() || ch == '.' || (ch == '-' && current.is_empty()) {
            current.push(ch);
            chars.next();
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            chars.next();
        } else {
            chars.next();
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    if tokens.is_empty() {
        return None;
    }

    // First pass: handle * and /
    let mut i = 0;
    let mut processed: Vec<String> = Vec::new();

    while i < tokens.len() {
        if i + 2 < tokens.len() && (tokens[i + 1] == "*" || tokens[i + 1] == "/") {
            let mut val = tokens[i].parse::<f64>().ok()?;
            while i + 2 < tokens.len() && (tokens[i + 1] == "*" || tokens[i + 1] == "/") {
                let right = tokens[i + 2].parse::<f64>().ok()?;
                val = if tokens[i + 1] == "*" {
                    val * right
                } else {
                    if right == 0.0 {
                        return None;
                    }
                    val / right
                };
                i += 2;
            }
            processed.push(val.to_string());
            i += 1;
        } else {
            processed.push(tokens[i].clone());
            i += 1;
        }
    }

    // Second pass: handle + and -
    let mut result = processed[0].parse::<f64>().ok()?;
    let mut j = 1;
    while j + 1 < processed.len() {
        let op = &processed[j];
        let right = processed[j + 1].parse::<f64>().ok()?;
        result = match op.as_str() {
            "+" => result + right,
            "-" => result - right,
            _ => return None,
        };
        j += 2;
    }

    Some(result)
}

// ---------------------------------------------------------------------------
// Browser protocol
// ---------------------------------------------------------------------------

/// Messages from the browser UI.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    Start {
        #[serde(alias = "systemInstruction")]
        system_instruction: Option<String>,
    },
    Text {
        text: String,
    },
    Audio {
        data: String,
    },
    Stop,
}

/// Messages to the browser UI.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ServerMessage {
    Connected,
    TextDelta { text: String },
    TextComplete { text: String },
    TurnComplete,
    Interrupted,
    Error { message: String },
}

type WsSender = mpsc::UnboundedSender<ServerMessage>;

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Credentials come from the environment (or `.env`): GEMINI_API_KEY for
    // Google AI, or GOOGLE_GENAI_USE_VERTEXAI=true + GOOGLE_CLOUD_PROJECT for
    // Vertex AI. `connect_from_env()` reads them when a session starts.
    let _ = dotenvy::dotenv();

    let static_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/gemini-adk-web-rs/static"
    );

    let app = Router::new()
        .fallback_service(
            ServeDir::new(static_dir).fallback(ServeFile::new(format!("{static_dir}/index.html"))),
        )
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive());

    let addr = "127.0.0.1:3003";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Tool Calling example running at http://{addr}");

    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

/// Open a text-only Live session with both tools registered.
async fn start_session(
    tx: &WsSender,
    system_instruction: Option<String>,
) -> Result<LiveHandle, AgentError> {
    let instruction = system_instruction.unwrap_or_else(|| {
        "You are a helpful assistant. Use the get_weather tool when asked \
         about weather in any city. Use the calculate tool for math \
         expressions. Always use tools when relevant rather than guessing."
            .to_string()
    });

    let tx_delta = tx.clone();
    let tx_complete = tx.clone();
    let tx_turn = tx.clone();
    let tx_interrupted = tx.clone();
    let tx_error = tx.clone();
    let tx_tool = tx.clone();

    Live::builder()
        .text_only()
        .instruction(instruction)
        // Each `#[tool]` fn yields a constructor for its ToolFunction; the
        // schema the model sees is derived from the parameter list.
        .tool(get_weather())
        .tool(calculate())
        .on_text(move |t| {
            let _ = tx_delta.send(ServerMessage::TextDelta { text: t.into() });
        })
        .on_text_complete(move |t| {
            let _ = tx_complete.send(ServerMessage::TextComplete { text: t.into() });
        })
        // Show the browser what the model asked for. Returning `None` lets
        // the dispatcher run the tools and reply to the model itself.
        .on_tool_call(move |calls, _state| {
            info!("Received {} tool call(s)", calls.len());
            for call in &calls {
                let _ = tx_tool.send(ServerMessage::TextDelta {
                    text: format!("[Calling tool: {}({})]\n", call.name, call.args),
                });
            }
            async { None }
        })
        .on_tool_cancelled(|ids| {
            info!("Tool calls cancelled: {ids:?}");
            async {}
        })
        .on_turn_complete(move || {
            let _ = tx_turn.send(ServerMessage::TurnComplete);
            async {}
        })
        .on_interrupted(move || {
            let _ = tx_interrupted.send(ServerMessage::Interrupted);
            async {}
        })
        .on_error(move |message| {
            error!("Session error: {message}");
            let _ = tx_error.send(ServerMessage::Error { message });
            async {}
        })
        // No `.model(..)`: connect picks the platform's current Live model
        // (override with GEMINI_LIVE_MODEL).
        .connect_from_env()
        .await
}

async fn handle_socket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Session callbacks push into `ws_tx`; this task writes to the browser.
    let send_task = tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg)
                && sender.send(Message::Text(json)).await.is_err()
            {
                break;
            }
        }
    });

    let mut session: Option<LiveHandle> = None;

    while let Some(Ok(Message::Text(text))) = receiver.next().await {
        let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else {
            continue;
        };
        match client_msg {
            ClientMessage::Start { system_instruction } => {
                info!("Starting tool-calling session");
                if let Some(old) = session.take() {
                    let _ = old.disconnect().await;
                }
                match start_session(&ws_tx, system_instruction).await {
                    Ok(handle) => {
                        info!("Session active");
                        let _ = ws_tx.send(ServerMessage::Connected);
                        session = Some(handle);
                    }
                    Err(e) => {
                        error!("Failed to connect: {e}");
                        let _ = ws_tx.send(ServerMessage::Error {
                            message: format!("Failed to connect: {e}"),
                        });
                    }
                }
            }
            ClientMessage::Text { text } => {
                if let Some(handle) = &session
                    && let Err(e) = handle.send_text(text).await
                {
                    error!("Failed to send text: {e}");
                }
            }
            ClientMessage::Audio { data } => {
                if let Some(handle) = &session
                    && let Ok(pcm) = BASE64.decode(data)
                    && let Err(e) = handle.send_audio(pcm).await
                {
                    error!("Failed to send audio: {e}");
                }
            }
            ClientMessage::Stop => {
                info!("Stopping session");
                if let Some(handle) = session.take() {
                    let _ = handle.disconnect().await;
                }
            }
        }
    }

    if let Some(handle) = session {
        let _ = handle.disconnect().await;
    }
    send_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_with_precedence() {
        assert_eq!(eval_simple_expression("2 + 3 * 4"), Some(14.0));
        assert_eq!(eval_simple_expression("10 / 4"), Some(2.5));
        assert_eq!(eval_simple_expression("7"), Some(7.0));
    }

    #[test]
    fn rejects_division_by_zero_and_garbage() {
        assert_eq!(eval_simple_expression("1 / 0"), None);
        assert_eq!(eval_simple_expression("abc"), None);
    }

    #[test]
    fn tools_declare_their_parameters() {
        let weather = get_weather();
        assert_eq!(weather.name(), "get_weather");
        let schema = weather.parameters().expect("schema");
        assert!(schema["properties"]["city"].is_object());

        let calc = calculate();
        assert_eq!(calc.name(), "calculate");
        let schema = calc.parameters().expect("schema");
        assert!(schema["properties"]["expression"].is_object());
    }
}
