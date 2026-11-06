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
//! To be reached through a PBX extension or a provider's trunk instead, set
//! `SIP_REGISTRAR`, `SIP_USERNAME` and `SIP_PASSWORD` (and `SIP_REALM` when
//! the registrar needs it named): the agent then registers before taking
//! calls and removes the binding on exit. A secure offer (`RTP/SAVP` with
//! SDES keys) is answered with SRTP; no setting is needed for that.
//!
//! Each call gets its own Live session; barge-in stops playback immediately
//! (the agent is its own RTP buffer, and it drops it on interruption).

use tracing::{error, info};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::telephony::sip::{SipAccount, SipAgent, SipRegistration};

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
    let registration = match registration_account_from_env()? {
        Some(account) => {
            info!("registering {} at {}", account.username, account.registrar);
            // The first REGISTER (with its digest challenge) completes here, so
            // a wrong password or an unreachable registrar fails before any call.
            Some(agent.register(account).await?)
        }
        None => None,
    };

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
    if let Some(registration) = registration {
        unregister(registration).await;
    }
    Ok(())
}

/// Remove the registrar binding; a failure here only means the binding
/// expires on its own.
async fn unregister(registration: SipRegistration) {
    if let Err(err) = registration.unregister().await {
        error!("unregister failed: {err}");
    }
}

/// The account to register, from `SIP_REGISTRAR`, `SIP_USERNAME`,
/// `SIP_PASSWORD` and the optional `SIP_REALM`. `None` when no registrar is
/// set; an error when a registrar is set without credentials.
fn registration_account_from_env() -> Result<Option<SipAccount>, Box<dyn std::error::Error>> {
    account_from(
        std::env::var("SIP_REGISTRAR").ok(),
        std::env::var("SIP_USERNAME").ok(),
        std::env::var("SIP_PASSWORD").ok(),
        std::env::var("SIP_REALM").ok(),
    )
}

fn account_from(
    registrar: Option<String>,
    username: Option<String>,
    password: Option<String>,
    realm: Option<String>,
) -> Result<Option<SipAccount>, Box<dyn std::error::Error>> {
    let Some(registrar) = registrar.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let (Some(username), Some(password)) = (username, password) else {
        return Err("SIP_REGISTRAR is set: SIP_USERNAME and SIP_PASSWORD are needed too".into());
    };
    let mut account = SipAccount::new(registrar, username, password);
    if let Some(realm) = realm.filter(|value| !value.trim().is_empty()) {
        account = account.realm(realm);
    }
    Ok(Some(account))
}

async fn build_session() -> Result<LiveHandle, Box<dyn std::error::Error>> {
    let instruction = std::env::var("AGENT_INSTRUCTION").unwrap_or_else(|_| {
        "You are a friendly phone receptionist. Keep answers short and \
         conversational — this is a voice call."
            .into()
    });
    let model = ModelId::new(
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

#[cfg(test)]
mod tests {
    use super::account_from;

    #[test]
    fn no_registrar_means_no_registration() {
        let account = account_from(None, Some("u".into()), Some("p".into()), None).unwrap();
        assert!(account.is_none());
        let account = account_from(Some("  ".into()), None, None, None).unwrap();
        assert!(account.is_none());
    }

    #[test]
    fn registrar_without_credentials_is_an_error() {
        let err = account_from(Some("sip:pbx".into()), None, Some("p".into()), None).unwrap_err();
        assert!(err.to_string().contains("SIP_USERNAME"));
    }

    #[test]
    fn full_account_carries_realm() {
        let account = account_from(
            Some("sip:pbx.example.com".into()),
            Some("agent".into()),
            Some("secret".into()),
            Some("pbx".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(account.registrar, "sip:pbx.example.com");
        assert_eq!(account.username, "agent");
        assert_eq!(account.realm.as_deref(), Some("pbx"));
    }
}
