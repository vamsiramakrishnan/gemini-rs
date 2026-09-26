//! The phone entry point: Twilio's voice webhook and Media Streams socket.
//!
//! Point a Twilio number's voice webhook (HTTP POST) at
//! `https://<service>/twilio/voice/{bundle}`. The runtime checks
//! `X-Twilio-Signature`, then answers with TwiML that connects the call to
//! `wss://<service>/twilio/media/{bundle}/{token}`, where the token is
//! bound to this call and good once (see `auth::StreamTokens`). The media
//! socket is bridged to a Live session with
//! [`TwilioCall`](gemini_adk_fluent_rs::telephony::TwilioCall), the same
//! bridge `examples/telephony` uses.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{OriginalUri, Path, State as AxumState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::SinkExt;
use tracing::{info, warn};

use gemini_adk_fluent_rs::spec::SpecModality;
use gemini_adk_fluent_rs::telephony::TwilioCall;
use gemini_adk_fluent_rs::telephony::twilio::{Inbound, parse_inbound};

use super::auth::{self, StreamTokens, unix_now};
use super::{LoadedBundle, Runtime, SessionPermit};

/// How long Twilio has to send its `start` frame after the socket opens.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Frames accepted before `start` (Twilio sends one, `connected`).
const FRAMES_BEFORE_START: usize = 4;

fn twiml(body: &str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/xml")],
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Response>{body}</Response>\n"),
    )
        .into_response()
}

/// A busy signal: the call is refused without an application error.
fn busy() -> Response {
    twiml("<Reject reason=\"busy\"/>")
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The URL Twilio called: `ADK_PUBLIC_URL` plus the path when set, else
/// rebuilt from `X-Forwarded-Proto` and `Host`, as a proxy such as Cloud Run
/// passes them.
fn public_url(runtime: &Runtime, headers: &HeaderMap, path_and_query: &str) -> String {
    if let Some(base) = &runtime.config.public_url {
        return format!("{base}{path_and_query}");
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let proto = header("x-forwarded-proto")
        .and_then(|v| v.split(',').next())
        .map_or("https", str::trim);
    let host = header("x-forwarded-host")
        .or_else(|| header("host"))
        .unwrap_or("localhost");
    format!("{proto}://{host}{path_and_query}")
}

/// `POST /twilio/voice/{bundle}`: the call's voice webhook.
pub(super) async fn voice(
    AxumState(runtime): AxumState<Arc<Runtime>>,
    Path(name): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (Some(auth_token), Some(tokens)) = (
        runtime.config.twilio_auth_token.as_deref(),
        runtime.stream_tokens.as_ref(),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let params: Vec<(String, String)> = url::form_urlencoded::parse(&body).into_owned().collect();
    let path = uri.path_and_query().map_or(uri.path(), |pq| pq.as_str());
    let url = public_url(&runtime, &headers, path);
    let signature = headers
        .get("x-twilio-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !auth::twilio_signature_valid(auth_token, &url, &params, signature) {
        warn!("Twilio webhook for {url}: bad or missing X-Twilio-Signature");
        return (StatusCode::FORBIDDEN, "invalid Twilio signature").into_response();
    }

    let Some(bundle) = runtime.bundle(&name) else {
        warn!("Twilio call for unknown bundle '{name}'");
        return (
            StatusCode::NOT_FOUND,
            format!("no bundle '{name}' is served here"),
        )
            .into_response();
    };
    if !matches!(bundle.spec.modality, SpecModality::Audio) {
        warn!(
            "Twilio call for '{}', which is a text-only spec",
            bundle.reference
        );
        return busy();
    }
    let Some(call_sid) = params
        .iter()
        .find(|(k, _)| k == "CallSid")
        .map(|(_, v)| v.clone())
    else {
        return (StatusCode::BAD_REQUEST, "missing CallSid").into_response();
    };
    // Refuse early, while the caller can still get a busy signal. The media
    // socket is admitted again when it opens.
    if runtime.is_draining() || runtime.active_sessions() >= runtime.config.max_sessions {
        info!(call = %call_sid, "Twilio call refused: no capacity");
        return busy();
    }

    let token = tokens.mint(&bundle.name, &call_sid, unix_now());
    let base = public_url(&runtime, &headers, "");
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or(base);
    let stream_url = format!("{ws_base}/twilio/media/{}/{token}", bundle.name);
    twiml(&format!(
        "<Connect><Stream url=\"{}\"/></Connect>",
        xml_escape(&stream_url)
    ))
}

/// `GET /twilio/media/{bundle}/{token}`: the Media Streams socket.
pub(super) async fn media(
    AxumState(runtime): AxumState<Arc<Runtime>>,
    Path((name, token)): Path<(String, String)>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if runtime.stream_tokens.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(e) = StreamTokens::precheck(&token, unix_now()) {
        warn!("Twilio media socket refused: {e:?}");
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(bundle) = runtime.bundle(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let permit = match runtime.admit() {
        Ok(permit) => permit,
        Err(refusal) => return refusal.into_response(),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let limit = runtime.config.max_message_bytes;
    upgrade
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| run(runtime, bundle, token, socket, permit))
}

/// Read frames up to Twilio's `start`; returns them (to replay into the
/// bridge) and the call SID.
async fn read_until_start(socket: &mut WebSocket) -> Option<(Vec<String>, String)> {
    let mut frames = Vec::new();
    while frames.len() < FRAMES_BEFORE_START {
        let Some(Ok(Message::Text(text))) = socket.recv().await else {
            return None;
        };
        let text = text.to_string();
        match parse_inbound(&text) {
            Ok(Inbound::Connected) => frames.push(text),
            Ok(Inbound::Started(meta)) => {
                frames.push(text);
                return Some((frames, meta.call_sid));
            }
            _ => return None,
        }
    }
    None
}

async fn run(
    runtime: Arc<Runtime>,
    bundle: Arc<LoadedBundle>,
    token: String,
    mut socket: WebSocket,
    _permit: SessionPermit,
) {
    let Ok(Some((frames, call_sid))) =
        tokio::time::timeout(START_TIMEOUT, read_until_start(&mut socket)).await
    else {
        warn!("Twilio media socket closed before a valid start frame");
        let _ = socket.close().await;
        return;
    };
    let tokens: &StreamTokens = runtime
        .stream_tokens
        .as_ref()
        .expect("media route requires stream tokens");
    if let Err(e) = tokens.redeem(&token, &bundle.name, &call_sid, unix_now()) {
        warn!(call = %call_sid, "Twilio media socket refused: {e:?}");
        let _ = socket.close().await;
        return;
    }

    let handle = match runtime.connect(&bundle).await {
        Ok(handle) => handle,
        Err(e) => {
            warn!(call = %call_sid, bundle = %bundle.reference, "call session failed to start: {e}");
            let _ = socket.close().await;
            return;
        }
    };
    info!(call = %call_sid, bundle = %bundle.reference, version = %bundle.version.version, "call started");
    let mut call = TwilioCall::attach(&handle);
    for frame in frames {
        if call.from_twilio.send(frame).await.is_err() {
            break;
        }
    }

    let deadline = tokio::time::sleep(runtime.config.max_session);
    tokio::pin!(deadline);
    let mut closing = runtime.closing();
    loop {
        tokio::select! {
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Text(text))) => {
                    if call.from_twilio.send(text.to_string()).await.is_err() {
                        break; // the stream stopped
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            outbound = call.to_twilio.recv() => match outbound {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            () = &mut deadline => {
                info!(call = %call_sid, "call reached the session time limit");
                break;
            }
            () = super::closed(&mut closing) => break,
        }
    }
    call.abort();
    let _ = handle.disconnect().await;
    let _ = socket.close().await;
    info!(call = %call_sid, "call ended");
}
