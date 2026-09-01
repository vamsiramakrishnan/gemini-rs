//! AudioHook example — the third connector, built from the same parts.
//!
//! Twilio Media Streams and raw SIP are the SDK's two built-in phone
//! transports. This example is the promised third: a bot server speaking the
//! open [AudioHook protocol](https://developer.genesys.cloud/devapps/audiohook/)
//! a Genesys-style contact-center platform dials out to — composed entirely
//! from the public surface (`voice::pump`, `telephony::g711`,
//! `telephony::bridge`), with no SDK changes. The protocol itself lives in
//! [`protocol`] as a pure, offline-testable state machine; this file is only
//! the glue between a WebSocket and a governed Live session.
//!
//! Run:
//! ```bash
//! export GEMINI_API_KEY=...           # or Vertex env, see connect_from_env
//! cargo run -p example-audiohook
//! ```
//!
//! Then point the platform's AudioHook integration at
//! `wss://<public-host>/audiohook`. The platform's connection probe (a
//! handshake with the all-zeros conversation id) is answered without
//! starting a session. DTMF digits land in the same `telephony:*` state
//! keys as the Twilio and SIP paths, so the same flow guards work on all
//! three transports; barge-in becomes an AudioHook `barge_in` event, the
//! platform's form of Twilio's `clear`.
//!
//! Optional: point `FILLER_CLIP` at a raw mono PCM16 little-endian file at
//! 8 kHz ("one moment, let me check that") to arm the latency filler.

mod protocol;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::json;
use tracing::{error, info, warn};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::telephony::{bridge, g711};
use gemini_adk_fluent_rs::voice::{Playback, pump};

use protocol::{AUDIOHOOK_HZ, Effect, OpenInfo, ServerSession};

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
            get(|| async { "gemini-rs AudioHook example — WS /audiohook" }),
        )
        .route("/audiohook", get(audiohook));

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    info!(
        "listening on {addr} — point the platform's AudioHook integration at wss://<host>/audiohook"
    );
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind BIND_ADDR");
    axum::serve(listener, app).await.expect("serve");
}

async fn audiohook(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(|socket| async {
        if let Err(err) = run_connection(socket).await {
            error!("connection ended with error: {err}");
        }
    })
}

async fn run_connection(mut socket: WebSocket) -> Result<(), Box<dyn std::error::Error>> {
    let mut driver = ServerSession::new();

    // Phase 1 — the handshake, no session yet. A connection probe never
    // gets past this phase, so probes cost nothing but the handshake.
    let info = loop {
        let Some(frame) = socket.recv().await else {
            return Ok(());
        };
        match frame? {
            Message::Text(text) => {
                let mut opened: Option<OpenInfo> = None;
                for effect in driver.handle_text(&text)? {
                    match effect {
                        Effect::Reply(reply) => socket.send(Message::Text(reply)).await?,
                        Effect::Opened(info) => opened = Some(info),
                        Effect::End => return Ok(()),
                        Effect::Dtmf(_) => {}
                    }
                }
                match opened {
                    Some(info) if info.probe => {
                        info!("connection probe answered — waiting for close");
                    }
                    Some(info) => break info,
                    None => {}
                }
            }
            Message::Close(_) => return Ok(()),
            // Binary audio cannot arrive before `opened`; anything else
            // here is a client racing the handshake — drop it.
            _ => {}
        }
    };

    info!(
        "call opened — conversation {} from {}",
        info.conversation_id, info.ani
    );

    // Phase 2 — a governed Live session for the call.
    let instruction = std::env::var("AGENT_INSTRUCTION").unwrap_or_else(|_| {
        "You are a friendly contact-center agent. Keep answers short and \
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

    // The same state vocabulary as the Twilio and SIP connectors — one
    // flow-guard language across all three transports.
    let state = session.state().clone();
    let _ = state.set(bridge::KEY_CALL_SID, info.conversation_id.clone());
    let _ = state.set(bridge::KEY_STREAM_SID, info.session_id.clone());
    if !info.ani.is_empty() {
        let _ = state.set(bridge::KEY_CALLER, info.ani.clone());
    }

    let (mic_tx, mic_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(64);
    let (speaker_tx, mut speaker_rx) = tokio::sync::mpsc::channel::<Playback>(64);
    let voice_pump = pump(
        &session,
        mic_rx,
        AUDIOHOOK_HZ,
        speaker_tx.clone(),
        AUDIOHOOK_HZ,
    );
    let filler = filler_config()
        .map(|config| bridge::spawn_latency_filler(&session, speaker_tx.clone(), config));
    drop(speaker_tx);

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let mut ended = false;
                    for effect in driver.handle_text(&text)? {
                        match effect {
                            Effect::Reply(reply) => socket.send(Message::Text(reply)).await?,
                            Effect::Dtmf(digit) => bridge::record_dtmf(&state, digit),
                            Effect::End => ended = true,
                            Effect::Opened(_) => {}
                        }
                    }
                    if ended {
                        break;
                    }
                }
                Some(Ok(Message::Binary(payload))) => {
                    if let Some(pcm) = driver.handle_binary(&payload)
                        && mic_tx.send(pcm).await.is_err() {
                            break; // session gone
                        }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    warn!("websocket error: {err}");
                    break;
                }
            },
            outgoing = speaker_rx.recv() => match outgoing {
                Some(Playback::Chunk(samples)) => {
                    socket.send(Message::Binary(g711::encode_ulaw(&samples))).await?;
                }
                Some(Playback::Flush) => {
                    socket.send(Message::Text(driver.barge_in_event())).await?;
                }
                None => {
                    // The session ended on our side (model done, flow
                    // terminal, error). Ask the platform to wrap up; it
                    // answers with `close`, which the driver ends on.
                    // A warm transfer would carry a HandoffPacket in these
                    // output variables instead.
                    if driver.is_open() {
                        socket
                            .send(Message::Text(driver.disconnect("completed", json!({}))))
                            .await?;
                        wait_for_close(&mut socket, &mut driver).await?;
                    }
                    break;
                }
            },
        }
    }

    if let Some(filler) = filler {
        filler.abort();
    }
    voice_pump.abort();
    session.disconnect().await.ok();
    info!("call finished");
    Ok(())
}

/// After a server-side `disconnect`, drain the socket until the platform's
/// `close` completes the shutdown handshake (or the socket drops).
async fn wait_for_close(
    socket: &mut WebSocket,
    driver: &mut ServerSession,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Some(frame) = socket.recv().await {
        match frame {
            Ok(Message::Text(text)) => {
                for effect in driver.handle_text(&text)? {
                    match effect {
                        Effect::Reply(reply) => socket.send(Message::Text(reply)).await?,
                        Effect::End => return Ok(()),
                        _ => {}
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Ok(())
}

/// Latency filler from `FILLER_CLIP`: a raw mono PCM16 little-endian file
/// at 8 kHz. Absent → no filler, exactly as before.
fn filler_config() -> Option<bridge::FillerConfig> {
    let path = std::env::var("FILLER_CLIP").ok()?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let clip: Vec<i16> = bytes
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            info!(
                "latency filler armed: {path} ({:.1}s)",
                clip.len() as f64 / 8000.0
            );
            Some(bridge::FillerConfig::new(clip))
        }
        Err(err) => {
            warn!("FILLER_CLIP unreadable ({err}) — continuing without a filler");
            None
        }
    }
}

/// Live model from `GEMINI_LIVE_MODEL`, defaulting to the Google AI
/// native-audio preview (see CLAUDE.md: Live model names differ by platform).
fn live_model() -> GeminiModel {
    GeminiModel::Custom(
        std::env::var("GEMINI_LIVE_MODEL")
            .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".into()),
    )
}
