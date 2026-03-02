//! Transcription cookbook — demonstrates ALL configurable Gemini Live API properties.
//!
//! This cookbook showcases every session configuration option including:
//! - Input/output transcription
//! - Voice activity detection (VAD) settings
//! - Activity handling (barge-in behavior)
//! - Turn coverage
//! - Context window compression
//! - Session resumption
//! - Affective dialog
//!
//! Usage:
//!   cargo run -p cookbook-transcription
//!   # then open http://127.0.0.1:3004

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
use rs_genai::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Auth configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum AuthConfig {
    GoogleAI { api_key: String },
    VertexAI { project: String, location: String },
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    auth: AuthConfig,
}

// ---------------------------------------------------------------------------
// Client → Server messages (from browser UI)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "start")]
    Start {
        #[allow(dead_code)]
        model: Option<String>,
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

// ---------------------------------------------------------------------------
// Server → Client messages (to browser UI)
// ---------------------------------------------------------------------------

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
    // Transcription events
    #[serde(rename = "inputTranscription")]
    InputTranscription { text: String },
    #[serde(rename = "outputTranscription")]
    OutputTranscription { text: String },
    // Voice activity events
    #[serde(rename = "voiceActivityStart")]
    VoiceActivityStart,
    #[serde(rename = "voiceActivityEnd")]
    VoiceActivityEnd,
}

// ---------------------------------------------------------------------------
// Main
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

    // Serve static files from the shared UI directory
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../cookbooks/ui/static");

    let app = Router::new()
        .fallback_service(
            ServeDir::new(static_dir)
                .fallback(ServeFile::new(format!("{}/index.html", static_dir))),
        )
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = "127.0.0.1:3004";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Transcription cookbook running at http://{}", addr);

    axum::serve(listener, app).await.unwrap();
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut session_handle: Option<SessionHandle> = None;
    let mut session_event_task: Option<tokio::task::JoinHandle<()>> = None;

    let (ws_tx, mut ws_rx) = mpsc::channel::<ServerMessage>(100);

    // Task to forward messages from our channel to the browser WebSocket
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
                        voice,
                        system_instruction,
                        ..
                    } => {
                        info!("Starting transcription session with all config options");

                        let voice_enum = match voice.as_deref() {
                            Some("Puck") => Voice::Puck,
                            Some("Charon") => Voice::Charon,
                            Some("Kore") => Voice::Kore,
                            Some("Fenrir") => Voice::Fenrir,
                            Some("Aoede") => Voice::Aoede,
                            _ => Voice::Aoede,
                        };

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

                        // Demonstrate ALL configurable Gemini Live API properties
                        let config = base_config
                            // Model
                            .model(GeminiModel::GeminiLive2_5FlashNativeAudio)
                            // Voice
                            .voice(voice_enum)
                            // Transcription — the focus of this cookbook
                            .enable_input_transcription()
                            .enable_output_transcription()
                            // System instruction
                            .system_instruction(
                                system_instruction
                                    .unwrap_or_else(|| {
                                        "You are a helpful voice assistant. Speak naturally and conversationally.".to_string()
                                    }),
                            )
                            // Realtime input config — activity handling & turn coverage
                            .activity_handling(ActivityHandling::StartOfActivityInterrupts)
                            .turn_coverage(TurnCoverage::TurnIncludesOnlyActivity)
                            // Server VAD — automatic activity detection with default sensitivity
                            .server_vad(AutomaticActivityDetection {
                                disabled: None,
                                start_of_speech_sensitivity: Some(Sensitivity::Automatic),
                                end_of_speech_sensitivity: Some(Sensitivity::Automatic),
                                prefix_padding_ms: None,
                                silence_duration_ms: None,
                            })
                            // Context window compression for long sessions
                            .context_window_compression(2048)
                            // Session resumption — enables reconnection with state
                            .session_resumption(None)
                            // Thinking — commented out: native audio model doesn't support thinking
                            // .thinking(1024).include_thoughts()
                            // Affective dialog — emotionally expressive responses
                            .affective_dialog(true);

                        info!("Config built with all properties, connecting...");

                        match connect(config, TransportConfig::default()).await {
                            Ok(session) => {
                                session_handle = Some(session.clone());
                                let mut events = session.subscribe();
                                let tx = ws_tx.clone();

                                if let Some(t) = session_event_task.take() {
                                    t.abort();
                                }

                                session_event_task = Some(tokio::spawn(async move {
                                    // Wait for session to become active
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(15),
                                        session.wait_for_phase(SessionPhase::Active),
                                    )
                                    .await
                                    {
                                        Ok(_) => {
                                            info!("Session active — transcription enabled");
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

                                    // Forward ALL event types to the browser UI
                                    while let Some(event) = recv_event(&mut events).await {
                                        match event {
                                            SessionEvent::AudioData(data) => {
                                                let base64_data = BASE64.encode(&data);
                                                let _ = tx
                                                    .send(ServerMessage::Audio {
                                                        data: base64_data,
                                                    })
                                                    .await;
                                            }
                                            SessionEvent::TextDelta(t) => {
                                                let _ = tx
                                                    .send(ServerMessage::TextDelta { text: t })
                                                    .await;
                                            }
                                            SessionEvent::TextComplete(t) => {
                                                let _ = tx
                                                    .send(ServerMessage::TextComplete { text: t })
                                                    .await;
                                            }
                                            // Transcription events — forwarded with dedicated types
                                            SessionEvent::InputTranscription(t) => {
                                                info!("Input transcription: {}", t);
                                                let _ = tx
                                                    .send(ServerMessage::InputTranscription { text: t })
                                                    .await;
                                            }
                                            SessionEvent::OutputTranscription(t) => {
                                                info!("Output transcription: {}", t);
                                                let _ = tx
                                                    .send(ServerMessage::OutputTranscription { text: t })
                                                    .await;
                                            }
                                            // Voice activity detection events
                                            SessionEvent::VoiceActivityStart => {
                                                info!("Voice activity started");
                                                let _ = tx
                                                    .send(ServerMessage::VoiceActivityStart)
                                                    .await;
                                            }
                                            SessionEvent::VoiceActivityEnd => {
                                                info!("Voice activity ended");
                                                let _ = tx
                                                    .send(ServerMessage::VoiceActivityEnd)
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
