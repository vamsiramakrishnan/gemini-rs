use crate::manifest::{self, AgentManifest};
use async_trait::async_trait;
use axum::{
    Router,
    extract::{Path, State as AxumState, ws::WebSocketUpgrade},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Json},
    routing::get,
};
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_server_rs::ws::{
    AgentSource, AppCategory, AppError, AppInfo, AppRegistry, ClientMessage, ServerMessage,
    WsSender, handle_ws,
};
use rust_embed::Embed;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Static assets (embedded at compile time) ───────────────────────────────────────────
//
// The embed folder MUST live inside this crate: `cargo publish` ships only the
// crate directory, so a `../../apps/...` path compiles in the workspace but is
// absent from the package tarball, and the derive silently produces no impl
// (v1.0.0's publish verify failed exactly this way). `assets/web/` is a
// committed copy of `apps/gemini-adk-web-rs/static/`, kept identical by the
// `vendored_web_assets_match_source` drift test.

#[derive(Embed)]
#[folder = "assets/web"]
struct Assets;

// ── App state ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WebState {
    registry: Arc<AppRegistry>,
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

// ── Manifest-backed agent source ─────────────────────────────────────────────

/// An [`AgentSource`] that runs a Live voice/text session configured from an
/// `agent.toml` / `agent.json` manifest. This is the CLI's plug into the shared
/// `gemini-adk-server-rs` WebSocket bridge.
struct ManifestApp {
    manifest: AgentManifest,
}

#[async_trait]
impl AgentSource for ManifestApp {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn category(&self) -> AppCategory {
        AppCategory::Basic
    }

    fn features(&self) -> Vec<String> {
        self.manifest
            .tools
            .iter()
            .map(|t| match t.as_str() {
                "google_search" => "Google Search".into(),
                "code_execution" => "Code Execution".into(),
                other => other.to_string(),
            })
            .collect()
    }

    fn tips(&self) -> Vec<String> {
        vec![
            format!("Instruction: {}", truncate(&self.manifest.instruction, 80)),
            format!("Model: {}", self.manifest.model),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hello! What can you do?".into(),
            "Tell me something interesting.".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let manifest = &self.manifest;

        // ── Wait for Start ───────────────────────────────────────────────
        loop {
            match rx.recv().await {
                Some(ClientMessage::Start { .. }) => break,
                Some(_) => continue,
                None => return Ok(()),
            }
        }

        // ── Send Connected + AppMeta ─────────────────────────────────────
        let _ = tx.send(ServerMessage::Connected);
        let _ = tx.send(ServerMessage::AppMeta {
            info: AppInfo {
                name: self.name().to_string(),
                description: self.description().to_string(),
                category: self.category(),
                features: self.features(),
                tips: vec![],
                try_saying: vec!["Hello!".into()],
            },
        });

        // ── Resolve API key ──────────────────────────────────────────────
        let api_key = std::env::var("GOOGLE_GENAI_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| {
                AppError::Connection(
                    "No API key found. Set GOOGLE_GENAI_API_KEY environment variable.".into(),
                )
            })?;

        // ── Resolve voice ────────────────────────────────────────────────
        let voice = match manifest.voice.as_deref() {
            Some("Puck") | Some("puck") => Voice::Puck,
            Some("Charon") | Some("charon") => Voice::Charon,
            Some("Fenrir") | Some("fenrir") => Voice::Fenrir,
            Some("Aoede") | Some("aoede") => Voice::Aoede,
            _ => Voice::Kore,
        };

        // ── Build Live session from manifest ─────────────────────────────
        let tx_audio = tx.clone();
        let tx_text = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupt = tx.clone();
        let tx_in = tx.clone();
        let tx_out = tx.clone();
        let tx_thought = tx.clone();

        let mut builder = Live::builder()
            .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
            .instruction(&manifest.instruction)
            .voice(voice)
            .transcription(true, true)
            .on_audio(move |data| {
                tx_audio
                    .send(ServerMessage::Audio {
                        data: data.to_vec(),
                    })
                    .ok();
            })
            .on_text(move |t| {
                tx_text
                    .send(ServerMessage::TextDelta {
                        text: t.to_string(),
                    })
                    .ok();
            })
            .on_turn_complete({
                move || {
                    let tx = tx_turn.clone();
                    async move {
                        tx.send(ServerMessage::TurnComplete).ok();
                    }
                }
            })
            .on_interrupted({
                move || {
                    let tx = tx_interrupt.clone();
                    async move {
                        tx.send(ServerMessage::Interrupted).ok();
                    }
                }
            })
            .on_input_transcript(move |t, _is_final| {
                tx_in
                    .send(ServerMessage::InputTranscription {
                        text: t.to_string(),
                    })
                    .ok();
            })
            .on_output_transcript(move |t, _is_final| {
                tx_out
                    .send(ServerMessage::OutputTranscription {
                        text: t.to_string(),
                    })
                    .ok();
            })
            .on_thought(move |t| {
                tx_thought
                    .send(ServerMessage::Thought {
                        text: t.to_string(),
                    })
                    .ok();
            });

        if let Some(ref greeting) = manifest.greeting {
            builder = builder.greeting(greeting);
        }
        if let Some(budget) = manifest.thinking {
            builder = builder.thinking(budget);
        }
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
                let _ = tx.send(ServerMessage::Error {
                    message: format!("Connection failed: {}", e),
                });
                return Err(AppError::Connection(e.to_string()));
            }
        };

        // ── Forward browser messages → Live handle ───────────────────────
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        tracing::warn!("send_text error: {}", e);
                    }
                }
                ClientMessage::Audio { data } => {
                    if let Ok(decoded) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
                        && let Err(e) = handle.send_audio(decoded).await
                    {
                        tracing::warn!("send_audio error: {}", e);
                    }
                }
                ClientMessage::Stop => break,
                _ => {}
            }
        }

        let _ = handle.disconnect().await;
        Ok(())
    }
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

    let mut registry = AppRegistry::new();
    for (_path, m) in manifests {
        registry.register(ManifestApp { manifest: m });
    }
    let agent_names: Vec<String> = registry.list().into_iter().map(|a| a.name).collect();

    let state = WebState {
        registry: Arc::new(registry),
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
    println!("  │  URL:   {:<36} │", format!("http://{}", addr));
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
    Json(state.registry.list())
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
    let app = state.registry.get(&name);
    ws.on_upgrade(move |socket| async move {
        match app {
            Some(app) => handle_ws(socket, app, None).await,
            None => tracing::warn!("Agent '{}' not found", name),
        }
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
