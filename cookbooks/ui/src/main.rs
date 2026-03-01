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
use gemini_live_wire::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};

#[derive(Clone, Debug)]
enum AuthConfig {
    GoogleAI { api_key: String },
    VertexAI { project: String, location: String },
}

#[derive(Clone)]
struct AppState {
    auth: AuthConfig,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "start")]
    Start {
        model: Option<String>,
        voice: Option<String>,
        system_instruction: Option<String>,
    },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "audio")]
    Audio { data: String }, // base64 encoded PCM16
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
    Audio { data: String }, // base64 encoded PCM16
    #[serde(rename = "turnComplete")]
    TurnComplete,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "error")]
    Error { message: String },
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let _ = dotenvy::dotenv();

    let use_vertex = std::env::var("GOOGLE_GENAI_USE_VERTEXAI")
        .map(|v| v.to_uppercase() == "TRUE" || v == "1")
        .unwrap_or(false);

    let auth = if use_vertex {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT").expect("GOOGLE_CLOUD_PROJECT must be set when using Vertex AI");
        let location = std::env::var("GOOGLE_CLOUD_LOCATION").expect("GOOGLE_CLOUD_LOCATION must be set when using Vertex AI");
        info!("Using Vertex AI backend (Project: {}, Location: {})", project, location);
        AuthConfig::VertexAI { project, location }
    } else {
        let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set when not using Vertex AI");
        info!("Using Google AI Studio backend");
        AuthConfig::GoogleAI { api_key }
    };

    let state = AppState { auth };

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let app = Router::new()
        .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(format!("{}/index.html", static_dir))))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3000);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!("UI Server running at http://{}", addr);

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

    // Task to send messages from ws_tx to the actual websocket
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
                        model,
                        voice,
                        system_instruction,
                    } => {
                        info!("Received Start command from UI");
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
                            },
                            AuthConfig::VertexAI { project, location } => {
                                info!("Fetching token via gcloud...");
                                // Obtain token via gcloud
                                match std::process::Command::new("gcloud")
                                    .args(&["auth", "print-access-token"])
                                    .output() {
                                    Ok(output) if output.status.success() => {
                                        info!("Successfully fetched gcloud token");
                                        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                        SessionConfig::from_vertex(project, location, token)
                                    },
                                    err => {
                                        error!("Failed to fetch gcloud token: {:?}", err);
                                        let _ = ws_tx.send(ServerMessage::Error { message: "Failed to obtain Vertex AI token via gcloud".into() }).await;
                                        continue;
                                    }
                                }
                            }
                        };

                        // Use the requested model or default to standard Live
                        let model_enum = match model.as_deref() {
                            Some("gemini-2.0-flash-live-001") => GeminiModel::Gemini2_0FlashLive,
                            Some("gemini-2.5-flash-live-native-audio") => GeminiModel::Custom("gemini-2.5-flash-live-native-audio".to_string()),
                            Some(other) => GeminiModel::Custom(other.to_string()),
                            None => GeminiModel::Gemini2_0FlashLive,
                        };

                        let mut config = base_config
                            .model(model_enum)
                            .voice(voice_enum)
                            .enable_input_transcription()
                            .enable_output_transcription();

                        if let Some(sys) = system_instruction {
                            config = config.system_instruction(sys);
                        }

                        info!("Connecting to Gemini Live API...");
                        match connect(config, TransportConfig::default()).await {
                            Ok(session) => {
                                info!("Connection established, waiting for Active phase...");
                                session_handle = Some(session.clone());
                                let mut events = session.subscribe();
                                let tx = ws_tx.clone();

                                if let Some(t) = session_event_task.take() {
                                    t.abort();
                                }

                                session_event_task = Some(tokio::spawn(async move {
                                    // Wait for active with timeout
                                    match tokio::time::timeout(std::time::Duration::from_secs(15), session.wait_for_phase(SessionPhase::Active)).await {
                                        Ok(_) => {
                                            info!("Session is now Active!");
                                            let _ = tx.send(ServerMessage::Connected).await;
                                        }
                                        Err(_) => {
                                            error!("Timed out waiting for SessionPhase::Active. The model or location might be incorrect.");
                                            let _ = tx.send(ServerMessage::Error { message: "Connection timeout during setup. Check your Vertex project/location/model settings.".into() }).await;
                                            return;
                                        }
                                    }

                                    while let Ok(event) = events.recv().await {
                                        match event {
                                            SessionEvent::TextDelta(t) => {
                                                let _ = tx.send(ServerMessage::TextDelta { text: t }).await;
                                            }
                                            SessionEvent::TextComplete(t) => {
                                                let _ = tx.send(ServerMessage::TextComplete { text: t }).await;
                                            }
                                            SessionEvent::AudioData(data) => {
                                                let base64_data = BASE64.encode(&data);
                                                let _ = tx.send(ServerMessage::Audio { data: base64_data }).await;
                                            }
                                            SessionEvent::TurnComplete => {
                                                let _ = tx.send(ServerMessage::TurnComplete).await;
                                            }
                                            SessionEvent::Interrupted => {
                                                let _ = tx.send(ServerMessage::Interrupted).await;
                                            }
                                            SessionEvent::Error(e) => {
                                                error!("Session error: {}", e);
                                                let _ = tx.send(ServerMessage::Error { message: e }).await;
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
                        info!("Received Stop command from UI");
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
