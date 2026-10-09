//! `context-update-spike`: probe how a Live target handles `contextUpdate`,
//! the message that replaces the declared tools and the system instruction
//! mid-session.
//!
//! ```text
//! context-update-spike [--model NAME] [--only PROBE[,PROBE..]] [--json PATH] [--log PATH]
//! ```
//!
//! Credentials come from the environment, as for every example
//! (`ApiEndpoint::from_env`):
//!
//! - Google AI: `GEMINI_API_KEY`.
//! - Vertex AI: `GOOGLE_GENAI_USE_VERTEXAI=true`, `GOOGLE_CLOUD_PROJECT`,
//!   `GOOGLE_CLOUD_LOCATION` and `GOOGLE_ACCESS_TOKEN`
//!   (`gcloud auth print-access-token`).
//!
//! Each probe opens its own session over a raw transport, so it decides
//! exactly which frame goes out when. Updates are encoded by
//! `SessionConfig::to_context_update_message`, the same code the session
//! codec uses, but are sent even where the codec would refuse them, so the
//! probe can report what the server does.
//!
//! The probes:
//!
//! | Probe | Question |
//! |---|---|
//! | `replace_tools` | Is an update accepted between turns, and does the model then see only the new tools? |
//! | `replace_instruction` | Does the model follow a replaced system instruction? |
//! | `before_tool_response` | Is an update accepted while a tool call is pending, and can the tool response then point at a tool the update declared? |
//! | `during_generation` | Does an update sent while the model is speaking interrupt it? |
//! | `resume` | After a resume whose setup declares the old tools, which tools are in effect? |
//! | `resume_lost_update` | When an update is lost with the connection and the resume's setup declares the updated tools, which tools are in effect, with and without the update re-sent after the resumed setup? |
//! | `token_cost` | How do prompt and cached token counts move when the tool list shrinks? |

use std::time::{Duration, Instant};

use gemini_genai_rs::protocol::{
    ApiEndpoint, ContextUpdate, FunctionCall, FunctionDeclaration, FunctionResponse, ModelId, Part,
    ServerMessage, SessionConfig, SessionResumptionConfig, ThinkingLevel, Tool, UsageMetadata,
};
use gemini_genai_rs::session::SessionCommand;
use gemini_genai_rs::transport::{Codec, JsonCodec, Transport, TungsteniteTransport};
use serde_json::{Value, json};

const TURN_TIMEOUT: Duration = Duration::from_secs(30);

const ALL_PROBES: [&str; 7] = [
    "replace_tools",
    "replace_instruction",
    "before_tool_response",
    "during_generation",
    "resume",
    "resume_lost_update",
    "token_cost",
];

// ---------------------------------------------------------------------------
// Wire: one raw Live connection
// ---------------------------------------------------------------------------

/// How the `tools` field of an update is laid out on the wire.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `{"tools": {"tools": [...]}}`: the wrapper the API definition describes,
    /// and what the session codec sends.
    Wrapped,
    /// `{"tools": [...]}`: a bare list, tried only if the wrapper is refused.
    Flat,
}

/// What came back during one stretch of reading.
#[derive(Debug, Default)]
struct Turn {
    text: String,
    calls: Vec<FunctionCall>,
    audio_frames: usize,
    interrupted: bool,
    turn_complete: bool,
    usage: Option<UsageMetadata>,
    closed: Option<String>,
    timed_out: bool,
}

impl Turn {
    fn has_content(&self) -> bool {
        self.audio_frames > 0 || !self.text.is_empty()
    }

    fn called(&self, name: &str) -> Option<&FunctionCall> {
        self.calls.iter().find(|c| c.name == name)
    }

    fn call_names(&self) -> Vec<&str> {
        self.calls.iter().map(|c| c.name.as_str()).collect()
    }

    fn summary(&self) -> Value {
        json!({
            "text": self.text.trim(),
            "calls": self.call_names(),
            "audio_frames": self.audio_frames,
            "interrupted": self.interrupted,
            "turn_complete": self.turn_complete,
            "closed": self.closed,
            "timed_out": self.timed_out,
            "usage": self.usage.as_ref().map(usage_json),
        })
    }
}

struct Wire {
    transport: TungsteniteTransport,
    config: SessionConfig,
    resume_handle: Option<String>,
    log: Vec<Value>,
    started: Instant,
}

impl Wire {
    async fn open(config: SessionConfig) -> Result<Self, String> {
        let mut transport = TungsteniteTransport::new();
        let mut headers = Vec::new();
        if let Some(token) = config.bearer_token() {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        transport
            .connect(&config.ws_url(), headers)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let mut wire = Self {
            transport,
            config,
            resume_handle: None,
            log: Vec::new(),
            started: Instant::now(),
        };
        let setup = JsonCodec
            .encode_setup(&wire.config)
            .map_err(|e| e.to_string())?;
        wire.send(setup).await?;

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, wire.transport.recv()).await {
                Err(_) => return Err("no setupComplete within 20 s".into()),
                Ok(Err(e)) => return Err(format!("setup: {e}")),
                Ok(Ok(None)) => {
                    return Err(format!(
                        "setup refused: {}",
                        wire.transport
                            .close_reason()
                            .unwrap_or_else(|| "closed without a reason".into())
                    ));
                }
                Ok(Ok(Some(bytes))) => {
                    wire.record("recv", &bytes);
                    if let Ok(ServerMessage::SetupComplete(sc)) = JsonCodec.decode_message(&bytes) {
                        if let Some(handle) =
                            sc.setup_complete.session_resumption.and_then(|r| r.handle)
                        {
                            wire.resume_handle = Some(handle);
                        }
                        return Ok(wire);
                    }
                }
            }
        }
    }

    fn record(&mut self, dir: &str, bytes: &[u8]) {
        let mut frame =
            serde_json::from_slice(bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(bytes)));
        elide_media(&mut frame);
        self.log.push(json!({
            "ms": self.started.elapsed().as_millis() as u64,
            "dir": dir,
            "frame": frame,
        }));
    }

    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.record("send", &bytes);
        self.transport
            .send(bytes)
            .await
            .map_err(|e| format!("send: {e}"))
    }

    async fn command(&mut self, cmd: SessionCommand) -> Result<(), String> {
        let bytes = JsonCodec
            .encode_command(&cmd, &self.config)
            .map_err(|e| e.to_string())?;
        self.send(bytes).await
    }

    async fn say(&mut self, text: &str) -> Result<(), String> {
        self.command(SessionCommand::SendText(text.into())).await
    }

    async fn update(&mut self, update: &ContextUpdate, shape: Shape) -> Result<(), String> {
        let mut message = serde_json::to_value(self.config.to_context_update_message(update))
            .map_err(|e| e.to_string())?;
        if let (Shape::Flat, Some(tools)) = (shape, message["contextUpdate"].get_mut("tools")) {
            *tools = tools["tools"].take();
        }
        self.send(serde_json::to_vec(&message).map_err(|e| e.to_string())?)
            .await
    }

    async fn respond(&mut self, call: &FunctionCall, response: Value) -> Result<(), String> {
        self.command(SessionCommand::SendToolResponse(vec![FunctionResponse {
            name: call.name.clone(),
            response,
            id: call.id.clone(),
            scheduling: None,
        }]))
        .await
    }

    /// Read into `turn` until the turn completes, the connection closes,
    /// `until` holds, or `within` elapses.
    async fn read(&mut self, turn: &mut Turn, within: Duration, until: impl Fn(&Turn) -> bool) {
        let deadline = Instant::now() + within;
        loop {
            if turn.turn_complete || turn.closed.is_some() || until(turn) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let bytes = match tokio::time::timeout(remaining, self.transport.recv()).await {
                Err(_) => {
                    turn.timed_out = true;
                    return;
                }
                Ok(Err(e)) => {
                    turn.closed = Some(e.to_string());
                    return;
                }
                Ok(Ok(None)) => {
                    turn.closed = Some(
                        self.transport
                            .close_reason()
                            .unwrap_or_else(|| "closed without a reason".into()),
                    );
                    return;
                }
                Ok(Ok(Some(bytes))) => bytes,
            };
            self.record("recv", &bytes);
            // A frame can carry usage alone, which the codec reports as Unknown.
            if let Ok(raw) = serde_json::from_slice::<Value>(&bytes)
                && let Some(usage) = raw.get("usageMetadata")
                && let Ok(usage) = serde_json::from_value(usage.clone())
            {
                turn.usage = Some(usage);
            }
            match JsonCodec.decode_message(&bytes) {
                Ok(ServerMessage::ServerContent(sc)) => {
                    let content = sc.server_content;
                    for part in content.model_turn.iter().flat_map(|t| &t.parts) {
                        match part {
                            Part::Text { text } => turn.text.push_str(text),
                            Part::InlineData { .. } => turn.audio_frames += 1,
                            _ => {}
                        }
                    }
                    if let Some(text) = content.output_transcription.and_then(|t| t.text) {
                        turn.text.push_str(&text);
                    }
                    turn.interrupted |= content.interrupted.unwrap_or(false);
                    // Extended-thinking models end a spoken holding line with
                    // `IN_PROGRESS` and answer in a later turn.
                    let in_progress = content.interaction_status.as_deref() == Some("IN_PROGRESS");
                    turn.turn_complete |= content.turn_complete.unwrap_or(false) && !in_progress;
                }
                Ok(ServerMessage::ToolCall(tc)) => turn.calls.extend(tc.tool_call.function_calls),
                Ok(ServerMessage::SessionResumptionUpdate(update)) => {
                    if let Some(handle) = update.session_resumption_update.new_handle {
                        self.resume_handle = Some(handle);
                    }
                }
                _ => {}
            }
        }
    }

    /// Read a whole turn, answering every tool call with `answer`.
    async fn turn(&mut self, answer: fn(&FunctionCall) -> Value) -> Turn {
        self.continue_turn(Turn::default(), 0, answer).await
    }

    /// Keep reading `turn`, whose first `answered` calls already have
    /// responses, until the model has reacted to every tool result.
    ///
    /// On Gemini 3.8 Live the turn that issues a call ends on its own
    /// (`turnComplete` with nothing in it); the reaction to the result comes
    /// in a later turn. An empty completion after a response is therefore not
    /// the end of the exchange.
    async fn continue_turn(
        &mut self,
        mut turn: Turn,
        mut answered: usize,
        answer: fn(&FunctionCall) -> Value,
    ) -> Turn {
        let deadline = Instant::now() + TURN_TIMEOUT;
        let mut mark = (turn.audio_frames, turn.text.len());
        if answered > 0 {
            turn.turn_complete = false;
        }
        loop {
            let pending = answered;
            let remaining = deadline.saturating_duration_since(Instant::now());
            self.read(&mut turn, remaining, |t| t.calls.len() > pending)
                .await;
            if turn.closed.is_some() || turn.timed_out {
                return turn;
            }
            if turn.calls.len() > answered {
                let fresh = turn.calls[answered..].to_vec();
                for call in &fresh {
                    if self.respond(call, answer(call)).await.is_err() {
                        return turn;
                    }
                }
                answered = turn.calls.len();
                mark = (turn.audio_frames, turn.text.len());
                turn.turn_complete = false;
                continue;
            }
            if answered > 0 && (turn.audio_frames, turn.text.len()) == mark {
                // The call's own turn ended; the reaction has not started.
                turn.turn_complete = false;
                continue;
            }
            return turn;
        }
    }

    async fn ask(
        &mut self,
        text: &str,
        answer: fn(&FunctionCall) -> Value,
    ) -> Result<Turn, String> {
        self.say(text).await?;
        Ok(self.turn(answer).await)
    }

    async fn close(mut self) -> Vec<Value> {
        let _ = self.transport.close().await;
        self.log
    }
}

/// Replace base64 media in a logged frame with its length.
fn elide_media(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                match v {
                    Value::String(s) if key == "data" && s.len() > 64 => {
                        *v = json!(format!("<{} base64 chars>", s.len()));
                    }
                    _ => elide_media(v),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(elide_media),
        _ => {}
    }
}

fn usage_json(u: &UsageMetadata) -> Value {
    json!({
        "prompt": u.prompt_token_count,
        "cached": u.cached_content_token_count,
        "tool_use_prompt": u.tool_use_prompt_token_count,
        "response": u.response_token_count,
        "total": u.total_token_count,
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn function(name: &str, description: &str, param: &str) -> FunctionDeclaration {
    FunctionDeclaration {
        name: name.into(),
        description: description.into(),
        parameters: Some(json!({
            "type": "object",
            "properties": { param: { "type": "string" } },
            "required": [param],
        })),
        behavior: None,
    }
}

fn get_weather() -> FunctionDeclaration {
    function("get_weather", "Current weather for a city.", "city")
}

fn get_time() -> FunctionDeclaration {
    function("get_time", "Current local time in a city.", "city")
}

fn lookup_account() -> FunctionDeclaration {
    function(
        "lookup_account",
        "Look up a customer account by its id.",
        "account_id",
    )
}

fn get_balance() -> FunctionDeclaration {
    function(
        "get_balance",
        "Current balance of a customer account.",
        "account_id",
    )
}

fn tools(decls: Vec<FunctionDeclaration>) -> Vec<Tool> {
    vec![Tool::functions(decls)]
}

/// Scripted results. Never empty: an empty result makes the model retry.
fn scripted(call: &FunctionCall) -> Value {
    match call.name.as_str() {
        "get_time" => json!({ "status": "ok", "time": "09:41" }),
        "get_weather" => json!({ "status": "ok", "forecast": "sunny, 24 C" }),
        "get_balance" => json!({ "status": "ok", "balance": "1,250.00 INR" }),
        _ => json!({ "status": "ok" }),
    }
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

struct Finding {
    probe: String,
    verdict: &'static str,
    finding: String,
    data: Value,
    log: Vec<Value>,
}

impl Finding {
    fn new(probe: &str, verdict: &'static str, finding: impl Into<String>, data: Value) -> Self {
        Self {
            probe: probe.into(),
            verdict,
            finding: finding.into(),
            data,
            log: Vec::new(),
        }
    }
}

const TERSE: &str = "You are a terse assistant. Use a tool whenever one fits the request. \
                     Never invent tool results.";

async fn replace_tools(base: &SessionConfig, shape: Shape) -> Result<Finding, String> {
    let name = match shape {
        Shape::Wrapped => "replace_tools",
        Shape::Flat => "replace_tools_flat",
    };
    let config = base
        .clone()
        .system_instruction(TERSE)
        .add_tool(Tool::functions(vec![get_weather()]));
    let mut w = Wire::open(config).await?;
    let baseline = w.ask("Say hello in three words.", scripted).await?;
    w.update(&ContextUpdate::new().tools(tools(vec![get_time()])), shape)
        .await?;
    let time = w
        .ask(
            "What time is it in Tokyo right now? Use your tool.",
            scripted,
        )
        .await?;
    let weather = if time.closed.is_none() {
        Some(
            w.ask(
                "What is the weather in Paris? If none of your tools can answer that, say so.",
                scripted,
            )
            .await?,
        )
    } else {
        None
    };
    let data = json!({
        "baseline": baseline.summary(),
        "after_update_time": time.summary(),
        "after_update_weather": weather.as_ref().map(Turn::summary),
    });
    let mut f = if let Some(reason) = &time.closed {
        Finding::new(
            name,
            "refused",
            format!("closed after contextUpdate: {reason}"),
            data,
        )
    } else if time.called("get_time").is_some()
        && weather
            .as_ref()
            .is_some_and(|t| t.called("get_weather").is_none())
    {
        Finding::new(
            name,
            "pass",
            "accepted; the model called the new tool and did not call the removed one",
            data,
        )
    } else if time.called("get_weather").is_some()
        || weather
            .as_ref()
            .is_some_and(|t| t.called("get_weather").is_some())
    {
        Finding::new(
            name,
            "fail",
            "accepted, but the removed tool is still called",
            data,
        )
    } else {
        Finding::new(
            name,
            "inconclusive",
            format!("accepted; calls after update: {:?}", time.call_names()),
            data,
        )
    };
    f.log = w.close().await;
    Ok(f)
}

async fn replace_instruction(base: &SessionConfig) -> Result<Finding, String> {
    let name = "replace_instruction";
    let config = base
        .clone()
        .system_instruction("Reply in one short sentence.");
    let mut w = Wire::open(config).await?;
    let before = w.ask("Name a fruit.", scripted).await?;
    w.update(
        &ContextUpdate::new().system_instruction(
            "Reply in one short sentence. End every reply with the exact word PINEAPPLE.",
        ),
        Shape::Wrapped,
    )
    .await?;
    let after = w.ask("Name a primary color.", scripted).await?;
    let marked = |t: &Turn| t.text.to_lowercase().contains("pineapple");
    let data = json!({ "before": before.summary(), "after": after.summary() });
    let mut f = if let Some(reason) = &after.closed {
        Finding::new(
            name,
            "refused",
            format!("closed after contextUpdate: {reason}"),
            data,
        )
    } else if marked(&after) && !marked(&before) {
        Finding::new(
            name,
            "pass",
            "the replaced instruction was followed on the next turn",
            data,
        )
    } else if after.text.is_empty() {
        Finding::new(name, "inconclusive", "no transcript after the update", data)
    } else {
        Finding::new(
            name,
            "fail",
            "accepted, but the reply ignored the new instruction",
            data,
        )
    };
    f.log = w.close().await;
    Ok(f)
}

async fn before_tool_response(base: &SessionConfig) -> Result<Finding, String> {
    let name = "before_tool_response";
    let config = base
        .clone()
        .system_instruction(TERSE)
        .add_tool(Tool::functions(vec![lookup_account()]));
    let mut w = Wire::open(config).await?;
    w.say("Look up account 42 with your tool, then tell me what you find.")
        .await?;
    let mut turn = Turn::default();
    w.read(&mut turn, TURN_TIMEOUT, |t| !t.calls.is_empty())
        .await;
    let Some(lookup) = turn.called("lookup_account").cloned() else {
        let data = json!({ "turn": turn.summary() });
        let mut f = Finding::new(
            name,
            "inconclusive",
            "the model did not call lookup_account",
            data,
        );
        f.log = w.close().await;
        return Ok(f);
    };
    // The update goes out first: updates are processed in order with the rest
    // of the input, so the new tool is declared when the model reads the result.
    w.update(
        &ContextUpdate::new().tools(tools(vec![lookup_account(), get_balance()])),
        Shape::Wrapped,
    )
    .await?;
    w.respond(
        &lookup,
        json!({
            "status": "ok",
            "account_id": "42",
            "holder": "Asha",
            "next_step": "Call get_balance for account 42 now, then tell the user the balance.",
        }),
    )
    .await?;
    let rest = w.continue_turn(turn, 1, scripted).await;
    let data = json!({ "turn": rest.summary() });
    let mut f = if let Some(reason) = &rest.closed {
        Finding::new(
            name,
            "refused",
            format!("closed after the update: {reason}"),
            data,
        )
    } else if rest.called("get_balance").is_some() {
        Finding::new(
            name,
            "pass",
            "accepted while a call was pending; the tool response's next step used the newly declared tool",
            data,
        )
    } else {
        Finding::new(
            name,
            "fail",
            format!(
                "accepted, but the next step was not taken; calls: {:?}",
                rest.call_names()
            ),
            data,
        )
    };
    f.log = w.close().await;
    Ok(f)
}

async fn during_generation(base: &SessionConfig) -> Result<Finding, String> {
    let name = "during_generation";
    let config = base
        .clone()
        .system_instruction("Follow the user's instructions exactly.");
    let mut w = Wire::open(config).await?;
    w.say("Count from one to twenty in words, separated by commas.")
        .await?;
    let mut turn = Turn::default();
    w.read(&mut turn, TURN_TIMEOUT, Turn::has_content).await;
    if turn.turn_complete || !turn.has_content() {
        let data = json!({ "turn": turn.summary() });
        let mut f = Finding::new(name, "inconclusive", "no speech to update during", data);
        f.log = w.close().await;
        return Ok(f);
    }
    let (frames_before, text_before) = (turn.audio_frames, turn.text.len());
    w.update(
        &ContextUpdate::new()
            .system_instruction("Follow the user's instructions exactly. Be brief."),
        Shape::Wrapped,
    )
    .await?;
    w.read(&mut turn, TURN_TIMEOUT, |_| false).await;
    let data = json!({
        "turn": turn.summary(),
        "audio_frames_before_update": frames_before,
        "audio_frames_after_update": turn.audio_frames - frames_before,
        "transcript_chars_after_update": turn.text.len() - text_before,
    });
    let mut f = if let Some(reason) = &turn.closed {
        Finding::new(
            name,
            "refused",
            format!("closed after the update: {reason}"),
            data,
        )
    } else if turn.interrupted {
        Finding::new(
            name,
            "fail",
            "the update interrupted the model mid-speech",
            data,
        )
    } else if turn.turn_complete {
        Finding::new(
            name,
            "pass",
            format!(
                "speech continued to the end of the turn ({} audio frames after the update)",
                turn.audio_frames - frames_before
            ),
            data,
        )
    } else {
        Finding::new(name, "inconclusive", "the turn did not complete", data)
    };
    f.log = w.close().await;
    Ok(f)
}

/// An update lost with the connection: the server never saw it, and the
/// resume's setup declares the updated tools (as the session loop's folded
/// config does). Run twice: once as is, once re-sending the update right
/// after the resumed setup, which is what the session loop does.
async fn resume_lost_update(base: &SessionConfig) -> Result<Finding, String> {
    let name = "resume_lost_update";
    let update = ContextUpdate::new().tools(tools(vec![get_time()]));
    let mut log = Vec::new();
    let mut after = Vec::new();
    for replay in [false, true] {
        let mut config = base
            .clone()
            .system_instruction(TERSE)
            .add_tool(Tool::functions(vec![get_weather()]));
        config.session_resumption = Some(SessionResumptionConfig {
            handle: None,
            transparent: None,
        });
        let mut w = Wire::open(config.clone()).await?;
        w.ask("Say ok.", scripted).await?;
        let mut idle = Turn::default();
        w.read(&mut idle, Duration::from_secs(3), |_| false).await;
        let handle = w.resume_handle.clone();
        // The update is never sent: its frame is lost with the connection.
        log.extend(w.close().await);
        let Some(handle) = handle else {
            let mut f = Finding::new(
                name,
                "inconclusive",
                "no resumption handle was issued",
                json!({}),
            );
            f.log = log;
            return Ok(f);
        };

        config.apply_context_update(&update);
        config.session_resumption = Some(SessionResumptionConfig {
            handle: Some(handle),
            transparent: None,
        });
        let mut w = match Wire::open(config).await {
            Ok(w) => w,
            Err(e) => {
                let mut f =
                    Finding::new(name, "refused", format!("resume refused: {e}"), json!({}));
                f.log = log;
                return Ok(f);
            }
        };
        if replay {
            w.update(&update, Shape::Wrapped).await?;
        }
        after.push(
            w.ask(
                "What time is it in Tokyo right now? Use your tool.",
                scripted,
            )
            .await?,
        );
        log.extend(w.close().await);
    }

    let (plain, replayed) = (&after[0], &after[1]);
    let data = json!({
        "without_replay": plain.summary(),
        "with_replay": replayed.summary(),
    });
    let mut f = match (
        plain.called("get_time").is_some(),
        replayed.called("get_time").is_some(),
    ) {
        (false, true) => Finding::new(
            name,
            "pass",
            "the resumed setup's tools are ignored, so a lost update stays lost; re-sending it \
             after the resumed setup restores it",
            data,
        ),
        (true, true) => Finding::new(
            name,
            "observed",
            "the resumed setup's tools took effect; re-sending the update is harmless",
            data,
        ),
        _ => Finding::new(
            name,
            "inconclusive",
            format!(
                "calls without replay: {:?}, with replay: {:?}",
                plain.call_names(),
                replayed.call_names()
            ),
            data,
        ),
    };
    f.log = log;
    Ok(f)
}

async fn resume(base: &SessionConfig) -> Result<Finding, String> {
    let name = "resume";
    let mut config = base
        .clone()
        .system_instruction(TERSE)
        .add_tool(Tool::functions(vec![get_weather()]));
    config.session_resumption = Some(SessionResumptionConfig {
        handle: None,
        transparent: None,
    });
    let mut w = Wire::open(config.clone()).await?;
    w.ask("Say ok.", scripted).await?;
    w.update(
        &ContextUpdate::new().tools(tools(vec![get_time()])),
        Shape::Wrapped,
    )
    .await?;
    let handle_before = w.resume_handle.clone();
    let settle = w.ask("Say ok again.", scripted).await?;
    // Give the server a moment to issue a handle that covers the update.
    let mut idle = Turn::default();
    w.read(&mut idle, Duration::from_secs(3), |_| false).await;
    let handle = w.resume_handle.clone();
    let mut log = w.close().await;
    let Some(handle) = handle else {
        let mut f = Finding::new(
            name,
            "inconclusive",
            "no resumption handle was issued",
            json!({}),
        );
        f.log = log;
        return Ok(f);
    };

    // Resume with the setup the session started with: the old tools.
    config.session_resumption = Some(SessionResumptionConfig {
        handle: Some(handle.clone()),
        transparent: None,
    });
    let mut w = match Wire::open(config).await {
        Ok(w) => w,
        Err(e) => {
            let mut f = Finding::new(name, "refused", format!("resume refused: {e}"), json!({}));
            f.log = log;
            return Ok(f);
        }
    };
    let after = w
        .ask(
            "What time is it in Tokyo right now? Use your tool.",
            scripted,
        )
        .await?;
    let data = json!({
        "settle": settle.summary(),
        "handle_changed_after_update": handle_before.as_deref() != Some(handle.as_str()),
        "after_resume": after.summary(),
    });
    let mut f = if after.called("get_time").is_some() {
        Finding::new(
            name,
            "observed",
            "the server kept the updated tools across the resume, over the setup's tools",
            data,
        )
    } else if after.called("get_weather").is_some() {
        Finding::new(
            name,
            "observed",
            "the resume setup's tools are in effect: a client must re-declare the current tools on resume",
            data,
        )
    } else {
        Finding::new(
            name,
            "inconclusive",
            format!("calls after resume: {:?}", after.call_names()),
            data,
        )
    };
    log.extend(w.close().await);
    f.log = log;
    Ok(f)
}

async fn token_cost(base: &SessionConfig) -> Result<Finding, String> {
    let name = "token_cost";
    let padding = "Use only when the caller explicitly asks for this exact operation and has \
                   already confirmed the account, the amount and the effective date; never \
                   call it speculatively, and read back the result one digit at a time.";
    let many: Vec<FunctionDeclaration> = (0..12)
        .map(|i| {
            function(
                &format!("operation_{i}"),
                &format!("Operation {i}. {padding}"),
                "account_id",
            )
        })
        .collect();
    let config = base
        .clone()
        .system_instruction("Reply with the single word ok.")
        .add_tool(Tool::functions(many));
    let mut w = Wire::open(config).await?;
    let a = w.ask("Say ok.", scripted).await?;
    let b = w.ask("Say ok.", scripted).await?;
    w.update(
        &ContextUpdate::new().tools(tools(vec![function("noop", "Does nothing.", "x")])),
        Shape::Wrapped,
    )
    .await?;
    let c = w.ask("Say ok.", scripted).await?;
    let d = w.ask("Say ok.", scripted).await?;
    let usage = |t: &Turn| t.usage.as_ref().map(usage_json);
    let prompt = |t: &Turn| t.usage.as_ref().and_then(|u| u.prompt_token_count);
    let data = json!({
        "twelve_tools": [usage(&a), usage(&b)],
        "after_shrink_to_one": [usage(&c), usage(&d)],
        "closed": c.closed.as_ref().or(d.closed.as_ref()),
    });
    let mut f = if let Some(reason) = c.closed.as_ref().or(d.closed.as_ref()) {
        Finding::new(
            name,
            "refused",
            format!("closed after the update: {reason}"),
            data,
        )
    } else {
        Finding::new(
            name,
            "observed",
            format!(
                "prompt tokens per turn: 12 tools {:?}, {:?}; after shrinking to 1 tool {:?}, {:?}",
                prompt(&a),
                prompt(&b),
                prompt(&c),
                prompt(&d)
            ),
            data,
        )
    };
    f.log = w.close().await;
    Ok(f)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn usage() -> ! {
    eprintln!(
        "usage: context-update-spike [--model NAME] [--only PROBE[,PROBE..]] [--json PATH] [--log PATH]\n\
         probes: {}",
        ALL_PROBES.join(", ")
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let mut model = ModelId::LIVE_3_8;
    let mut only: Option<Vec<String>> = None;
    let mut json_path: Option<String> = None;
    let mut log_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--model" => model = ModelId::qualified(value()),
            "--only" => only = Some(value().split(',').map(str::to_string).collect()),
            "--json" => json_path = Some(value()),
            "--log" => log_path = Some(value()),
            _ => usage(),
        }
    }
    let endpoint = match ApiEndpoint::from_env() {
        Ok(endpoint) => endpoint,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    };
    let mut base = SessionConfig::from_endpoint(endpoint)
        .model(model)
        .text_only();
    // Extended-thinking models refuse a setup without a thinking level.
    if base.model_profile().thinking_level_required {
        base = base.thinking_level(ThinkingLevel::Low);
    }
    let platform = if base.is_vertex() {
        "Vertex AI"
    } else {
        "Google AI"
    };
    println!(
        "contextUpdate spike: {platform}, model {}, codec would {} it\n",
        base.resolved_model(),
        if base.supports_context_update() {
            "send"
        } else {
            "refuse"
        }
    );

    let wanted = |probe: &str| only.as_ref().is_none_or(|o| o.iter().any(|p| p == probe));
    let mut findings = Vec::new();
    for probe in ALL_PROBES.iter().copied().filter(|p| wanted(p)) {
        let result = match probe {
            "replace_tools" => replace_tools(&base, Shape::Wrapped).await,
            "replace_instruction" => replace_instruction(&base).await,
            "before_tool_response" => before_tool_response(&base).await,
            "during_generation" => during_generation(&base).await,
            "resume" => resume(&base).await,
            "resume_lost_update" => resume_lost_update(&base).await,
            "token_cost" => token_cost(&base).await,
            _ => unreachable!(),
        };
        let finding = result.unwrap_or_else(|e| Finding::new(probe, "error", e, json!({})));
        println!(
            "[{:>12}] {:<22} {}",
            finding.verdict, finding.probe, finding.finding
        );
        let refused = probe == "replace_tools" && finding.verdict == "refused";
        findings.push(finding);
        // A refused wrapper may be a field-layout problem rather than an
        // unsupported message: try the bare list once to tell them apart.
        if refused {
            let flat = replace_tools(&base, Shape::Flat)
                .await
                .unwrap_or_else(|e| Finding::new("replace_tools_flat", "error", e, json!({})));
            println!("[{:>12}] {:<22} {}", flat.verdict, flat.probe, flat.finding);
            findings.push(flat);
        }
    }

    let report = json!({
        "platform": platform,
        "model": base.resolved_model().to_string(),
        "codec_sends_context_update": base.supports_context_update(),
        "findings": findings.iter().map(|f| json!({
            "probe": f.probe,
            "verdict": f.verdict,
            "finding": f.finding,
            "data": f.data,
        })).collect::<Vec<_>>(),
    });
    if let Some(path) = json_path {
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap())
            .unwrap_or_else(|e| eprintln!("could not write {path}: {e}"));
    }
    if let Some(path) = log_path {
        let lines: Vec<String> = findings
            .iter()
            .flat_map(|f| {
                f.log
                    .iter()
                    .map(move |entry| json!({ "probe": f.probe, "entry": entry }).to_string())
            })
            .collect();
        std::fs::write(&path, lines.join("\n") + "\n")
            .unwrap_or_else(|e| eprintln!("could not write {path}: {e}"));
    }
}
