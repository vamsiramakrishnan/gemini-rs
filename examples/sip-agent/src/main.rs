//! SIP agent example — a Gemini Live agent any SIP endpoint can dial.
//!
//! No carrier service in the path: this process terminates SIP signalling
//! (via rsipstack) and G.711-over-RTP media itself. Point a softphone
//! (Linphone, Zoiper), an Asterisk/FreeSWITCH extension, or a SIP trunk at
//! it and call:
//!
//! ```bash
//! export GEMINI_API_KEY=...           # or Vertex env, see connect_from_env
//! cargo run -p example-sip-agent      # listens on 0.0.0.0:5060/udp
//! # In a softphone: call sip:gemini@<host>  (no registration needed)
//! ```
//!
//! Each call gets its own Live session; barge-in stops playback immediately
//! (the agent is its own RTP buffer, and it drops it on interruption).

use tracing::{error, info};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::telephony::sip::SipAgent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let addr = std::env::var("SIP_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:5060".into());
    let mut agent = SipAgent::bind(addr.parse()?).await?;
    info!(
        "SIP agent on {addr} (port {}) — dial sip:gemini@<this-host> from any softphone",
        agent.sip_port()
    );

    while let Some(incoming) = agent.next_call().await {
        info!("incoming call from {}", incoming.from);
        let session = match build_session().await {
            Ok(session) => session,
            Err(err) => {
                error!("could not start Live session: {err}");
                incoming.reject();
                continue;
            }
        };
        match incoming.answer(&session).await {
            Ok(call) => {
                tokio::spawn(async move {
                    call.ended().await;
                    session.disconnect().await.ok();
                    info!("call finished");
                });
            }
            Err(err) => {
                error!("failed to answer call: {err}");
                session.disconnect().await.ok();
            }
        }
    }
    Ok(())
}

async fn build_session() -> Result<LiveHandle, Box<dyn std::error::Error>> {
    let instruction = std::env::var("AGENT_INSTRUCTION").unwrap_or_else(|_| {
        "You are a friendly phone receptionist. Keep answers short and \
         conversational — this is a voice call."
            .into()
    });
    let model = GeminiModel::Custom(
        std::env::var("GEMINI_LIVE_MODEL")
            .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".into()),
    );
    Ok(Live::builder()
        .model(model)
        .voice(Voice::Puck)
        .instruction(instruction)
        .greeting("Greet the caller and ask how you can help.")
        .connect_from_env()
        .await?)
}
