//! Telephony example — a Gemini Live agent that answers real phone calls.
//!
//! Point a Twilio phone number's voice webhook at `POST /twiml` on this
//! server (through a public tunnel such as `ngrok http 8080` during
//! development). Twilio fetches TwiML that opens a Media Stream back to
//! `/media`, and each call gets its own Live session: the caller's μ-law
//! 8 kHz audio flows in, the model's voice flows out, and barge-in maps to
//! Twilio's `clear` so the agent stops the instant the caller speaks.
//!
//! Run:
//! ```bash
//! export GEMINI_API_KEY=...           # or Vertex env, see connect_from_env
//! export GEMINI_LIVE_MODEL=models/gemini-2.5-flash-native-audio-preview-12-2025
//! cargo run -p example-telephony
//! ```
//!
//! Then in Twilio: Phone Numbers → your number → Voice → webhook
//! `https://<public-host>/twiml` (HTTP POST).

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::http::header::HOST;
use axum::response::IntoResponse;
use axum::routing::{any, get};
use tracing::{error, info, warn};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::telephony::TwilioCall;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = Router::new()
        .route(
            "/",
            get(|| async { "gemini-rs telephony example — POST /twiml, WS /media" }),
        )
        .route("/twiml", any(twiml))
        .route("/media", get(media));

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    info!("listening on {addr} — point your Twilio voice webhook at POST /twiml");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind BIND_ADDR");
    axum::serve(listener, app).await.expect("serve");
}

/// TwiML webhook: tell Twilio to open a Media Stream to this server.
async fn twiml(headers: HeaderMap) -> impl IntoResponse {
    // Twilio reached us at this host; the stream must come back to the same
    // place. Behind a tunnel/proxy the public name is what the Host (or
    // X-Forwarded-Host) header carries.
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost:8080");
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="wss://{host}/media" />
  </Connect>
</Response>"#
    );
    ([("content-type", "text/xml")], body)
}

/// Media Streams WebSocket: one Live session per call.
async fn media(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(|socket| async {
        if let Err(err) = run_call(socket).await {
            error!("call ended with error: {err}");
        }
    })
}

async fn run_call(mut socket: WebSocket) -> Result<(), Box<dyn std::error::Error>> {
    info!("call connected — starting Live session");
    let instruction = std::env::var("AGENT_INSTRUCTION").unwrap_or_else(|_| {
        "You are a friendly phone receptionist. Keep answers short and \
         conversational — this is a voice call."
            .into()
    });

    let session = Live::builder()
        .model(live_model())
        .voice(Voice::Puck)
        .instruction(instruction)
        .greeting("Greet the caller and ask how you can help.")
        .connect_from_env()
        .await?;

    let mut call = TwilioCall::attach(&session);

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if call.from_twilio.send(text).await.is_err() {
                        break; // bridge ended (stream stopped)
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // Twilio sends no binary frames; ignore pings
                Some(Err(err)) => {
                    warn!("websocket error: {err}");
                    break;
                }
            },
            outgoing = call.to_twilio.recv() => match outgoing {
                Some(frame) => socket.send(Message::Text(frame)).await?,
                None => break, // session closed
            },
        }
    }

    call.abort();
    session.disconnect().await.ok();
    info!("call finished");
    Ok(())
}

/// Live model from `GEMINI_LIVE_MODEL`, defaulting to the Google AI
/// native-audio preview (see CLAUDE.md: Live model names differ by platform).
fn live_model() -> GeminiModel {
    GeminiModel::Custom(
        std::env::var("GEMINI_LIVE_MODEL")
            .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".into()),
    )
}
