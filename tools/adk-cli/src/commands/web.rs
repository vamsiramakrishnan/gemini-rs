use crate::manifest::{self, AgentManifest};
use adk_rs_fluent::prelude::*;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxumState,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Static assets (embedded from adk-web at compile time) ────────────────────

#[derive(Embed)]
#[folder = "../../apps/adk-web/static"]
struct Assets;

// ── Message types (matching adk-web's WebSocket protocol) ────────────────────

/// Messages sent from server → browser.
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
#[allow(dead_code)]
enum ServerMsg {
    Connected,
    AppMeta {
        info: AppInfo,
    },
    #[serde(rename_all = "camelCase")]
    TextDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    TextComplete {
        text: String,
    },
    TurnComplete,
    Interrupted,
    #[serde(rename_all = "camelCase")]
    InputTranscription {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    OutputTranscription {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Thought {
        text: String,
    },
    VoiceActivityStart,
    VoiceActivityEnd,
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    StateUpdate {
        key: String,
        value: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallEvent {
        name: String,
        args: serde_json::Value,
        result: serde_json::Value,
    },
}

/// Messages sent from browser → server.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClientMsg {
    Start {
        #[allow(dead_code)]
        system_instruction: Option<String>,
        #[allow(dead_code)]
        model: Option<String>,
        #[allow(dead_code)]
        voice: Option<String>,
    },
    Text {
        text: String,
    },
    Audio {
        data: String,
    },
    Stop,
}

/// Outgoing wire messages (JSON text OR binary audio).
enum WsOut {
    Json(ServerMsg),
    Binary(Vec<u8>),
}

// ── App metadata (for /api/apps discovery) ───────────────────────────────────

#[derive(Serialize, Clone)]
struct AppInfo {
    name: String,
    description: String,
    category: String,
    features: Vec<String>,
    tips: Vec<String>,
    #[serde(rename = "trySaying")]
    try_saying: Vec<String>,
}

// ── App state ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WebState {
    agents: Arc<HashMap<String, AgentManifest>>,
}

// ── Config ───────────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct WebConfig {
    pub agent_dir: String,
    pub host: String,
    pub port: u16,
    pub allow_origins: Option<String>,
    pub log_level: String,
    pub reload: bool,
    pub a2a: bool,
    pub trace_to_cloud: bool,
    pub session_service_uri: Option<String>,
    pub artifact_storage_uri: Option<String>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(config: WebConfig) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .init();

    // Discover agents
    let dir = PathBuf::from(&config.agent_dir);
    let manifests = manifest::discover_agents(&dir);

    if manifests.is_empty() {
        eprintln!("  No agents found in {}\n", config.agent_dir);
        eprintln!("  Make sure the directory contains an agent.toml file.");
        eprintln!("  Create one with: adk create my-agent\n");
        return Ok(());
    }

    let agents: HashMap<String, AgentManifest> = manifests
        .into_iter()
        .map(|(_path, m)| (m.name.clone(), m))
        .collect();

    let agent_names: Vec<String> = agents.keys().cloned().collect();

    let state = WebState {
        agents: Arc::new(agents),
    };

    let app = build_router(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Banner
    println!();
    println!("  ┌─────────────────────────────────────────────┐");
    println!("  │  adk web — Agent Development Kit             │");
    println!("  │                                              │");
    for name in &agent_names {
        println!("  │  Agent: {:<36} │", name);
    }
    println!(
        "  │  URL:   {:<36} │",
        format!("http://{}", addr)
    );
    println!("  │                                              │");
    println!("  │  Press Ctrl+C to stop.                       │");
    println!("  └─────────────────────────────────────────────┘");
    println!();

    axum::serve(listener, app).await?;
    Ok(())
}

// ── Router ───────────────────────────────────────────────────────────────────

fn build_router(state: WebState) -> Router {
    Router::new()
        .route("/", get(landing_page))
        .route("/app/{name}", get(app_page))
        .route("/api/apps", get(list_apps))
        .route("/ws/{name}", get(ws_upgrade))
        .route("/favicon.ico", get(favicon))
        .route("/static/{*path}", get(serve_static))
        .with_state(Arc::new(state))
}

// ── Route handlers ───────────────────────────────────────────────────────────

async fn landing_page() -> Html<String> {
    let html = Assets::get("index.html")
        .map(|f| String::from_utf8_lossy(&f.data).to_string())
        .unwrap_or_else(|| "<h1>adk web</h1><p>Static assets not found.</p>".into());
    Html(html)
}

async fn app_page() -> Html<String> {
    let html = Assets::get("app.html")
        .map(|f| String::from_utf8_lossy(&f.data).to_string())
        .unwrap_or_else(|| "<h1>App</h1><p>Static assets not found.</p>".into());
    Html(html)
}

async fn list_apps(AxumState(state): AxumState<Arc<WebState>>) -> Json<Vec<AppInfo>> {
    let apps: Vec<AppInfo> = state
        .agents
        .iter()
        .map(|(name, m)| {
            let features = m.tools.iter().map(|t| match t.as_str() {
                "google_search" => "Google Search".into(),
                "code_execution" => "Code Execution".into(),
                other => other.to_string(),
            }).collect();

            AppInfo {
                name: name.clone(),
                description: m.description.clone(),
                category: "walk".into(),
                features,
                tips: vec![
                    format!("Instruction: {}", truncate(&m.instruction, 80)),
                    format!("Model: {}", m.model),
                ],
                try_saying: vec![
                    "Hello! What can you do?".into(),
                    "Tell me something interesting.".into(),
                ],
            }
        })
        .collect();

    Json(apps)
}

async fn favicon() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml")],
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><text y=".9em" font-size="90">🤖</text></svg>"#,
    )
}

async fn serve_static(Path(path): Path<String>) -> impl IntoResponse {
    let path = path.trim_start_matches('/');
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (
                        header::CACHE_CONTROL,
                        "no-cache, no-store, must-revalidate".into(),
                    ),
                ],
                content.data.to_vec(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

// ── WebSocket upgrade ────────────────────────────────────────────────────────

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Path(name): Path<String>,
    AxumState(state): AxumState<Arc<WebState>>,
) -> impl IntoResponse {
    let manifest = state.agents.get(&name).cloned();
    ws.on_upgrade(move |socket| async move {
        if let Some(manifest) = manifest {
            if let Err(e) = run_session(socket, manifest).await {
                tracing::error!("Session error for '{}': {}", name, e);
            }
        } else {
            tracing::warn!("Agent '{}' not found", name);
        }
    })
}

// ── WebSocket session (Live voice + text) ────────────────────────────────────

async fn run_session(
    socket: WebSocket,
    manifest: AgentManifest,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsOut>();

    // ── Task: forward outgoing messages to WebSocket ─────────────────
    let send_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let result = match msg {
                WsOut::Binary(data) => ws_tx.send(Message::Binary(data.into())).await,
                WsOut::Json(server_msg) => match serde_json::to_string(&server_msg) {
                    Ok(json) => ws_tx.send(Message::Text(json.into())).await,
                    Err(_) => continue,
                },
            };
            if result.is_err() {
                break;
            }
        }
    });

    // ── Wait for Start message ───────────────────────────────────────
    let _start = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(msg @ ClientMsg::Start { .. }) = serde_json::from_str(&text) {
                    break msg;
                }
            }
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            _ => continue,
        }
    };

    // ── Send Connected + AppMeta ─────────────────────────────────────
    let _ = out_tx.send(WsOut::Json(ServerMsg::Connected));
    let _ = out_tx.send(WsOut::Json(ServerMsg::AppMeta {
        info: AppInfo {
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            category: "walk".into(),
            features: manifest.tools.clone(),
            tips: vec![],
            try_saying: vec!["Hello!".into()],
        },
    }));

    // ── Resolve API key ──────────────────────────────────────────────
    let api_key = std::env::var("GOOGLE_GENAI_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .map_err(|_| "No API key found. Set GOOGLE_GENAI_API_KEY environment variable.")?;

    // ── Build Live session from manifest ─────────────────────────────
    let tx_audio = out_tx.clone();
    let tx_text = out_tx.clone();
    let tx_turn = out_tx.clone();
    let tx_interrupt = out_tx.clone();
    let tx_in_transcript = out_tx.clone();
    let tx_out_transcript = out_tx.clone();
    let tx_thought = out_tx.clone();

    // Resolve voice from manifest or default to Kore
    let voice = match manifest.voice.as_deref() {
        Some("Puck") | Some("puck") => Voice::Puck,
        Some("Charon") | Some("charon") => Voice::Charon,
        Some("Fenrir") | Some("fenrir") => Voice::Fenrir,
        Some("Aoede") | Some("aoede") => Voice::Aoede,
        _ => Voice::Kore,
    };

    let mut builder = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction(&manifest.instruction)
        .voice(voice)
        .transcription(true, true)
        .on_audio(move |data| {
            tx_audio.send(WsOut::Binary(data.to_vec())).ok();
        })
        .on_text(move |t| {
            tx_text
                .send(WsOut::Json(ServerMsg::TextDelta {
                    text: t.to_string(),
                }))
                .ok();
        })
        .on_turn_complete({
            move || {
                let tx = tx_turn.clone();
                async move {
                    tx.send(WsOut::Json(ServerMsg::TurnComplete)).ok();
                }
            }
        })
        .on_interrupted({
            move || {
                let tx = tx_interrupt.clone();
                async move {
                    tx.send(WsOut::Json(ServerMsg::Interrupted)).ok();
                }
            }
        })
        .on_input_transcript(move |t, _is_final| {
            tx_in_transcript
                .send(WsOut::Json(ServerMsg::InputTranscription {
                    text: t.to_string(),
                }))
                .ok();
        })
        .on_output_transcript(move |t, _is_final| {
            tx_out_transcript
                .send(WsOut::Json(ServerMsg::OutputTranscription {
                    text: t.to_string(),
                }))
                .ok();
        })
        .on_thought(move |t| {
            tx_thought
                .send(WsOut::Json(ServerMsg::Thought {
                    text: t.to_string(),
                }))
                .ok();
        });

    // Wire optional Live features from manifest
    if let Some(ref greeting) = manifest.greeting {
        builder = builder.greeting(greeting);
    }
    if let Some(budget) = manifest.thinking {
        builder = builder.thinking(budget);
    }

    // Wire built-in tools
    for tool in &manifest.tools {
        builder = match tool.as_str() {
            "google_search" => builder.google_search(),
            "code_execution" => builder.code_execution(),
            "url_context" => builder.url_context(),
            _ => builder,
        };
    }

    // ── Connect ──────────────────────────────────────────────────────
    let handle = match builder.connect_google_ai(&api_key).await {
        Ok(h) => h,
        Err(e) => {
            let _ = out_tx.send(WsOut::Json(ServerMsg::Error {
                message: format!("Connection failed: {}", e),
            }));
            send_task.abort();
            return Ok(());
        }
    };

    // ── Forward browser messages → Live handle ───────────────────────
    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let parsed: Result<ClientMsg, _> = serde_json::from_str(&text);
                match parsed {
                    Ok(ClientMsg::Text { text }) => {
                        if let Err(e) = handle.send_text(&text).await {
                            tracing::warn!("send_text error: {}", e);
                        }
                    }
                    Ok(ClientMsg::Audio { data }) => {
                        if let Ok(decoded) = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            &data,
                        ) {
                            if let Err(e) = handle.send_audio(decoded.into()).await {
                                tracing::warn!("send_audio error: {}", e);
                            }
                        }
                    }
                    Ok(ClientMsg::Stop) => break,
                    _ => {}
                }
            }
            Ok(Message::Binary(data)) => {
                // Raw audio binary frame from browser
                if let Err(e) = handle.send_audio(data.to_vec().into()).await {
                    tracing::warn!("send_audio error: {}", e);
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // ── Cleanup ──────────────────────────────────────────────────────
    let _ = handle.disconnect().await;
    send_task.abort();

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
