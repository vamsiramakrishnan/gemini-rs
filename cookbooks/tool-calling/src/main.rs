//! Tool Calling cookbook — agent with typed function calling.
//!
//! Demonstrates TypedTool with auto-generated JSON Schema and
//! automatic tool dispatch in a Gemini Live session.
//!
//! Usage:
//!   cargo run -p cookbook-tool-calling
//!   # then open http://127.0.0.1:3003

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{sink::SinkExt, stream::StreamExt};
use rs_adk::tool::{ToolDispatcher, TypedTool};
use rs_genai::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Tool argument types — JsonSchema auto-generates the schema
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name to get weather for
    city: String,
}

#[derive(Deserialize, JsonSchema)]
struct CalculatorArgs {
    /// Mathematical expression to evaluate (e.g. "2 + 3 * 4")
    expression: String,
}

// ---------------------------------------------------------------------------
// App types
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    auth: AuthConfig,
}

#[derive(Clone, Debug)]
enum AuthConfig {
    GoogleAI { api_key: String },
    VertexAI { project: String, location: String },
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "start")]
    Start {
        #[allow(dead_code)]
        model: Option<String>,
        #[allow(dead_code)]
        voice: Option<String>,
        system_instruction: Option<String>,
    },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "audio")]
    Audio { data: String },
    #[serde(rename = "stop")]
    Stop,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type")]
enum ServerMessage {
    #[serde(rename = "connected")]
    Connected,
    #[serde(rename = "textDelta")]
    TextDelta { text: String },
    #[serde(rename = "textComplete")]
    TextComplete { text: String },
    #[serde(rename = "audio")]
    Audio { data: String },
    #[serde(rename = "turnComplete")]
    TurnComplete,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "error")]
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Tool dispatcher setup
// ---------------------------------------------------------------------------

fn create_tool_dispatcher() -> ToolDispatcher {
    let mut dispatcher = ToolDispatcher::new();

    // get_weather — returns mock weather data for any city
    dispatcher.register_function(Arc::new(TypedTool::new(
        "get_weather",
        "Get current weather for a city including temperature, condition, and humidity",
        |args: WeatherArgs| async move {
            // In production, this would call a real weather API
            info!("Tool called: get_weather(city={})", args.city);
            Ok(serde_json::json!({
                "city": args.city,
                "temperature_celsius": 22,
                "condition": "Partly cloudy",
                "humidity_percent": 65,
                "wind_speed_kmh": 12
            }))
        },
    )));

    // calculate — evaluates simple math expressions
    dispatcher.register_function(Arc::new(TypedTool::new(
        "calculate",
        "Evaluate a mathematical expression and return the result",
        |args: CalculatorArgs| async move {
            info!("Tool called: calculate(expr={})", args.expression);
            // Simple expression evaluator for demo purposes
            let result = eval_simple_expression(&args.expression);
            match result {
                Some(val) => Ok(serde_json::json!({
                    "expression": args.expression,
                    "result": val
                })),
                None => Ok(serde_json::json!({
                    "expression": args.expression,
                    "error": "Could not evaluate expression. Supported: basic arithmetic (+, -, *, /)"
                })),
            }
        },
    )));

    dispatcher
}

/// Simple arithmetic expression evaluator for demo purposes.
/// Supports: integer/float literals with +, -, *, / operators (left-to-right, no precedence).
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

    // Evaluate left-to-right with operator precedence (* and / before + and -)
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
// Server
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let _ = dotenvy::dotenv();

    let use_vertex = std::env::var("GOOGLE_GENAI_USE_VERTEXAI")
        .map(|v| v.to_uppercase() == "TRUE" || v == "1")
        .unwrap_or(false);

    let auth = if use_vertex {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT")
            .expect("GOOGLE_CLOUD_PROJECT required for Vertex AI");
        let location = std::env::var("GOOGLE_CLOUD_LOCATION")
            .unwrap_or_else(|_| "us-central1".to_string());
        info!("Using Vertex AI (project: {}, location: {})", project, location);
        AuthConfig::VertexAI { project, location }
    } else {
        let api_key = std::env::var("GEMINI_API_KEY")
            .expect("Set GEMINI_API_KEY or enable Vertex AI");
        info!("Using Google AI Studio");
        AuthConfig::GoogleAI { api_key }
    };

    let state = AppState { auth };

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../cookbooks/ui/static");

    let app = Router::new()
        .fallback_service(
            ServeDir::new(static_dir)
                .fallback(ServeFile::new(format!("{}/index.html", static_dir))),
        )
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = "127.0.0.1:3003";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Tool Calling cookbook running at http://{}", addr);

    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut session_handle: Option<SessionHandle> = None;
    let mut session_event_task: Option<tokio::task::JoinHandle<()>> = None;

    let (ws_tx, mut ws_rx) = mpsc::channel::<ServerMessage>(100);

    let send_task = tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        if let Message::Text(text) = msg {
            if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                match client_msg {
                    ClientMessage::Start {
                        system_instruction, ..
                    } => {
                        info!("Starting tool-calling session");

                        let base_config = match &state.auth {
                            AuthConfig::GoogleAI { api_key } => {
                                SessionConfig::new(api_key)
                            }
                            AuthConfig::VertexAI { project, location } => {
                                let token = String::from_utf8(
                                    std::process::Command::new("gcloud")
                                        .args(["auth", "print-access-token"])
                                        .output()
                                        .expect("gcloud CLI required for Vertex AI")
                                        .stdout,
                                )
                                .unwrap()
                                .trim()
                                .to_string();
                                SessionConfig::from_vertex(project, location, token)
                            }
                        };

                        // Create tool dispatcher and get declarations
                        let dispatcher = create_tool_dispatcher();
                        let tool_declarations = dispatcher.to_tool_declarations();

                        info!(
                            "Registered {} tools: {:?}",
                            dispatcher.len(),
                            tool_declarations
                        );

                        // Text model with function calling tools
                        let sys_instruction = system_instruction.unwrap_or_else(|| {
                            "You are a helpful assistant. Use the get_weather tool when asked \
                             about weather in any city. Use the calculate tool for math \
                             expressions. Always use tools when relevant rather than guessing."
                                .to_string()
                        });

                        let mut config = base_config
                            .model(GeminiModel::Gemini2_0FlashLive)
                            .text_only()
                            .system_instruction(sys_instruction);

                        // Add tool declarations to the config
                        for tool in tool_declarations {
                            config = config.add_tool(tool);
                        }

                        match connect(config, TransportConfig::default()).await {
                            Ok(session) => {
                                session_handle = Some(session.clone());
                                let mut events = session.subscribe();
                                let tx = ws_tx.clone();

                                if let Some(t) = session_event_task.take() {
                                    t.abort();
                                }

                                session_event_task = Some(tokio::spawn(async move {
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(15),
                                        session.wait_for_phase(SessionPhase::Active),
                                    )
                                    .await
                                    {
                                        Ok(_) => {
                                            info!("Session active");
                                            let _ = tx.send(ServerMessage::Connected).await;
                                        }
                                        Err(_) => {
                                            error!("Timed out waiting for active session");
                                            let _ = tx
                                                .send(ServerMessage::Error {
                                                    message: "Connection timeout".into(),
                                                })
                                                .await;
                                            return;
                                        }
                                    }

                                    // Event loop with automatic tool dispatch
                                    while let Some(event) = recv_event(&mut events).await {
                                        match event {
                                            SessionEvent::ToolCall(calls) => {
                                                info!(
                                                    "Received {} tool call(s)",
                                                    calls.len()
                                                );

                                                // Notify UI that tools are being called
                                                for call in &calls {
                                                    let _ = tx
                                                        .send(ServerMessage::TextDelta {
                                                            text: format!(
                                                                "[Calling tool: {}({})]\n",
                                                                call.name, call.args
                                                            ),
                                                        })
                                                        .await;
                                                }

                                                // Execute each tool call and collect responses
                                                let mut responses = Vec::new();
                                                for call in &calls {
                                                    let result = dispatcher
                                                        .call_function(
                                                            &call.name,
                                                            call.args.clone(),
                                                        )
                                                        .await;

                                                    let response =
                                                        ToolDispatcher::build_response(
                                                            call, result,
                                                        );

                                                    info!(
                                                        "Tool '{}' result: {}",
                                                        call.name, response.response
                                                    );

                                                    responses.push(response);
                                                }

                                                // Send all tool responses back to Gemini
                                                if let Err(e) = session
                                                    .send_tool_response(responses)
                                                    .await
                                                {
                                                    error!(
                                                        "Failed to send tool response: {}",
                                                        e
                                                    );
                                                }
                                            }
                                            SessionEvent::TextDelta(t) => {
                                                let _ = tx
                                                    .send(ServerMessage::TextDelta { text: t })
                                                    .await;
                                            }
                                            SessionEvent::TextComplete(t) => {
                                                let _ = tx
                                                    .send(ServerMessage::TextComplete {
                                                        text: t,
                                                    })
                                                    .await;
                                            }
                                            SessionEvent::AudioData(data) => {
                                                let base64_data = BASE64.encode(&data);
                                                let _ = tx
                                                    .send(ServerMessage::Audio {
                                                        data: base64_data,
                                                    })
                                                    .await;
                                            }
                                            SessionEvent::TurnComplete => {
                                                let _ =
                                                    tx.send(ServerMessage::TurnComplete).await;
                                            }
                                            SessionEvent::Interrupted => {
                                                let _ =
                                                    tx.send(ServerMessage::Interrupted).await;
                                            }
                                            SessionEvent::ToolCallCancelled(ids) => {
                                                info!("Tool calls cancelled: {:?}", ids);
                                                dispatcher.cancel_by_ids(&ids).await;
                                            }
                                            SessionEvent::Error(e) => {
                                                error!("Session error: {}", e);
                                                let _ = tx
                                                    .send(ServerMessage::Error { message: e })
                                                    .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                }));
                            }
                            Err(e) => {
                                error!("Failed to connect: {}", e);
                                let _ = ws_tx
                                    .send(ServerMessage::Error {
                                        message: format!("Failed to connect: {}", e),
                                    })
                                    .await;
                            }
                        }
                    }
                    ClientMessage::Text { text } => {
                        if let Some(session) = &session_handle {
                            if let Err(e) = session.send_text(text).await {
                                error!("Failed to send text: {}", e);
                            }
                        }
                    }
                    ClientMessage::Audio { data } => {
                        if let Some(session) = &session_handle {
                            if let Ok(bytes) = BASE64.decode(data) {
                                if let Err(e) = session.send_audio(bytes).await {
                                    error!("Failed to send audio: {}", e);
                                }
                            }
                        }
                    }
                    ClientMessage::Stop => {
                        info!("Stopping session");
                        if let Some(session) = &session_handle {
                            let _ = session.disconnect().await;
                        }
                        if let Some(t) = session_event_task.take() {
                            t.abort();
                        }
                        session_handle = None;
                    }
                }
            }
        }
    }

    if let Some(session) = session_handle {
        let _ = session.disconnect().await;
    }
    if let Some(t) = session_event_task {
        t.abort();
    }
    send_task.abort();
}
