//! A stand-in for Gemini Live (WebSocket) and `generateContent` (REST) that
//! plays one scripted Trattoria Rustica booking call per connection.
//!
//! The call is driven by the client's microphone: the caller "speaks" only
//! while the client keeps streaming audio, so a client that falls behind on
//! its mic stream stretches its own calls. Each connection:
//!
//! 1. answers `setup`, waits for the greeting turn and speaks the greeting,
//!    which carries the stage's verbatim line;
//! 2. hears the caller ask for a table (input transcription, paced by the
//!    mic audio received), says it will check, calls `check_availability`,
//!    then offers the times;
//! 3. hears the caller pick a time and reads the booking back;
//! 4. hears "Yes, please book it" and calls `book_table`, which the client's
//!    commit gate admits only once the caller's yes has been extracted;
//! 5. gives the reference and waits for the caller to hang up.
//!
//! Model audio is 24 kHz PCM16 in 40 ms frames sent in real time, with the
//! output transcription spread over them. The REST side answers the client's
//! extraction requests after `--extract-ms`, from the transcript they carry.
//!
//! Audio carries its own clock, both ways: the first 8 bytes of every frame
//! are a little-endian Unix time in microseconds. In model audio it is when
//! the stand-in sent the frame, so a client can tell how late each frame
//! reached its audio callback. In mic audio it is when the client's 20 ms
//! clock captured the frame, so the stand-in can tell how late the caller's
//! audio reached the model. Client and stand-in share the host's clock.
//!
//! `GET /stats` reports what the stand-in measured (mic lateness, tool round
//! trips, steering latency, call outcomes); `POST /reset` clears it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::serve::ListenerExt as _;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// One 40 ms frame of 24 kHz mono PCM16.
const OUT_FRAME_MS: u64 = 40;
const OUT_FRAME_BYTES: usize = 24_000 * 2 * OUT_FRAME_MS as usize / 1000;
/// Mic audio is 16 kHz mono PCM16: 32 bytes per millisecond.
const MIC_BYTES_PER_MS: f64 = 32.0;
/// Speaking rate for the scripted lines.
const MS_PER_WORD: u64 = 340;

const GREETING: &str = "Hello, you've reached Trattoria Rustica's booking line. This call may be recorded. How can I help you today?";
const ASK: &[&str] = &[
    "Hi, I'd like a table for four people",
    " this Wednesday at seven in the evening,",
    " under the name Rossi please.",
];
const CHECKING: &str = "Of course, let me check that for you.";
const OFFER: &str = "I have seven or seven thirty on Wednesday. Which would you like?";
const PICK: &[&str] = &["Seven o'clock is perfect,", " thank you."];
const READ_BACK: &str =
    "So that's a party of four on Wednesday at seven, under Rossi. Shall I book it?";
const YES: &[&str] = &["Yes, please book it."];
const GOODBYE: &str =
    "You're all set. Your reference is T R 2 0 4 4. See you on Wednesday, goodbye!";
const SORRY: &str = "I'm sorry, I couldn't complete that booking. Goodbye.";

/// Microseconds since the Unix epoch.
fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// The clock a frame carries in its first 8 bytes, read from the first 12
/// characters of its base64 (9 bytes). google-genai encodes bytes with the
/// URL-safe alphabet, Rust with the standard one; both are read.
fn stamp_of(b64: &str) -> Option<u64> {
    let head: String = b64
        .get(..12)?
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    let head = base64::engine::general_purpose::STANDARD
        .decode(head)
        .ok()?;
    Some(u64::from_le_bytes(head.get(..8)?.try_into().ok()?))
}

#[derive(Clone, Copy)]
struct Timing {
    /// Print every message a client sends, apart from mic audio.
    log: bool,
    /// The model's pause between the caller's last word and its reply.
    reply_ms: u64,
    /// How long the caller stays silent before the turn is taken as over.
    end_of_speech_ms: u64,
    /// The caller's pause after the model finishes before speaking.
    caller_gap_ms: u64,
    /// The extraction model's latency.
    extract_ms: u64,
}

#[derive(Default)]
struct Stats {
    calls_started: u64,
    calls_booked: u64,
    calls_hung_up: u64,
    failures: BTreeMap<String, u64>,
    mic_ms: f64,
    mic_late_ms: Vec<f32>,
    /// Round trip of each tool call, by tool.
    tool_rtt_ms: BTreeMap<String, Vec<f32>>,
    steer_ms: Vec<f32>,
    call_secs: Vec<f32>,
    extract_requests: u64,
    context_updates: u64,
    posture_turns: u64,
}

struct Shared {
    timing: Timing,
    /// A frame's audio after its 9-byte clock header, in base64.
    audio_tail_b64: String,
    stats: parking_lot::Mutex<Stats>,
}

type App = Arc<Shared>;

#[tokio::main]
async fn main() {
    let mut port = 8766u16;
    let mut timing = Timing {
        log: false,
        reply_ms: 500,
        end_of_speech_ms: 500,
        caller_gap_ms: 600,
        extract_ms: 400,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--log" {
            timing.log = true;
            continue;
        }
        let mut num = || args.next().expect("value").parse::<u64>().expect("number");
        match a.as_str() {
            "--port" => port = num() as u16,
            "--reply-ms" => timing.reply_ms = num(),
            "--end-of-speech-ms" => timing.end_of_speech_ms = num(),
            "--caller-gap-ms" => timing.caller_gap_ms = num(),
            "--extract-ms" => timing.extract_ms = num(),
            other => panic!("unknown argument {other}"),
        }
    }
    let shared = Arc::new(Shared {
        timing,
        audio_tail_b64: base64::engine::general_purpose::STANDARD.encode(
            (9..OUT_FRAME_BYTES)
                .map(|i| ((i * 7) % 251) as u8)
                .collect::<Vec<u8>>(),
        ),
        stats: parking_lot::Mutex::new(Stats::default()),
    });
    let app = Router::new()
        .route("/stats", get(stats))
        .route("/reset", post(reset))
        .route("/ws/{*rest}", any(upgrade))
        .route("/v1beta/models/{*rest}", post(generate))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    println!("ready on {port}");
    // Small frames go out at once, as a real Live server sends them; with
    // Nagle on, each would wait for the client's next packet.
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    axum::serve(listener, app).await.expect("serve");
}

async fn upgrade(ws: WebSocketUpgrade, State(shared): State<App>) -> Response {
    ws.max_message_size(64 << 20)
        .on_upgrade(move |socket| call(socket, shared))
}

// ---------------------------------------------------------------------------
// The Live side
// ---------------------------------------------------------------------------

/// What the client sent, as the script needs it. Mic audio never reaches the
/// script: the reader counts it and publishes the running total.
enum Inbound {
    Setup,
    /// A `clientContent`: whether it completes the turn.
    Content {
        turn_complete: bool,
    },
    ToolResponse(Value),
    ContextUpdate(Vec<String>),
}

/// Look a key up in its camelCase (Rust, the API) or snake_case (google-genai)
/// spelling.
fn field<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| v.get(snake))
}

async fn call(socket: WebSocket, shared: App) {
    let start = Instant::now();
    shared.stats.lock().calls_started += 1;
    let (mut sink, mut stream) = socket.split();
    let (events_tx, events_rx) = mpsc::unbounded_channel::<Inbound>();
    let (mic_tx, mic_rx) = watch::channel(0f64);

    let reader_shared = shared.clone();
    let reader = tokio::spawn(async move {
        let mut heard_ms = 0f64;
        let mut late = Vec::new();
        while let Some(Ok(msg)) = stream.next().await {
            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => match Utf8Bytes::try_from(b) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
                Message::Close(_) => break,
                _ => continue,
            };
            let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if let Some(input) = field(&v, "realtimeInput", "realtime_input") {
                let data = input
                    .get("audio")
                    .and_then(|a| a.get("data"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // A client holding the floor sends silence in place of the
                // mic while the model speaks; those frames carry no clock.
                if let Some(captured) = stamp_of(data).filter(|c| *c > 0) {
                    late.push(now_us().saturating_sub(captured) as f32 / 1000.0);
                }
                heard_ms += (data.len() * 3 / 4) as f64 / MIC_BYTES_PER_MS;
                mic_tx.send_replace(heard_ms);
                continue;
            }
            if reader_shared.timing.log {
                println!("<- {v}");
            }
            let event = if v.get("setup").is_some() {
                Inbound::Setup
            } else if let Some(c) = field(&v, "clientContent", "client_content") {
                let turn_complete = field(c, "turnComplete", "turn_complete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !turn_complete {
                    reader_shared.stats.lock().posture_turns += 1;
                }
                Inbound::Content { turn_complete }
            } else if let Some(r) = field(&v, "toolResponse", "tool_response") {
                Inbound::ToolResponse(r.clone())
            } else if let Some(u) = field(&v, "contextUpdate", "context_update") {
                let names = u
                    .get("tools")
                    .and_then(|t| t.get("tools"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| field(t, "functionDeclarations", "function_declarations"))
                    .filter_map(Value::as_array)
                    .flatten()
                    .filter_map(|d| d.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                reader_shared.stats.lock().context_updates += 1;
                Inbound::ContextUpdate(names)
            } else {
                continue;
            };
            if events_tx.send(event).is_err() {
                break;
            }
        }
        let mut s = reader_shared.stats.lock();
        s.mic_ms += heard_ms;
        s.mic_late_ms.extend(late);
    });

    let mut script = Script {
        shared: shared.clone(),
        sink: &mut sink,
        inbox: Inbox {
            shared: shared.clone(),
            events: events_rx,
            steer_from: None,
        },
        mic: mic_rx,
        calls: 0,
    };
    let outcome = script.run().await;
    match outcome {
        Ok(()) => {
            let mut s = shared.stats.lock();
            s.calls_hung_up += 1;
            s.call_secs.push(start.elapsed().as_secs_f32());
        }
        Err(reason) => {
            *shared.stats.lock().failures.entry(reason).or_default() += 1;
            let _ = sink.send(Message::Close(None)).await;
        }
    }
    reader.abort();
    let _ = reader.await;
}

struct Script<'a, S> {
    shared: App,
    sink: &'a mut S,
    inbox: Inbox,
    mic: watch::Receiver<f64>,
    calls: u32,
}

/// The client's messages, apart from mic audio.
struct Inbox {
    shared: App,
    events: mpsc::UnboundedReceiver<Inbound>,
    /// When the turn that should move the call to `confirm` completed.
    steer_from: Option<Instant>,
}

impl Inbox {
    /// The next event the script cares about. Steering messages are counted
    /// and timed here; `None` once the client has gone.
    async fn next(&mut self) -> Option<Inbound> {
        loop {
            let event = self.events.recv().await?;
            if let Inbound::ContextUpdate(names) = &event {
                if names.iter().any(|n| n == "book_table")
                    && let Some(from) = self.steer_from.take()
                {
                    self.shared
                        .stats
                        .lock()
                        .steer_ms
                        .push(from.elapsed().as_secs_f32() * 1000.0);
                }
                continue;
            }
            return Some(event);
        }
    }
}

type Step = Result<(), String>;

impl<S> Script<'_, S>
where
    S: futures_util::Sink<Message> + Unpin,
{
    async fn run(&mut self) -> Step {
        let t = self.shared.timing;
        self.expect_setup().await?;
        self.send(json!({ "setupComplete": {} })).await?;
        self.await_turn().await?;
        self.pause(t.reply_ms).await?;
        self.speak(GREETING).await?;
        self.turn_complete().await?;

        // The caller asks; the model checks and offers.
        self.caller_says(ASK).await?;
        self.speak(CHECKING).await?;
        let availability = self
            .tool_call(
                "check_availability",
                json!({ "party_size": 4, "datetime": "2026-10-14T19:00" }),
            )
            .await?;
        if availability.get("options").is_none() {
            return Err(format!("check_availability answered {availability}"));
        }
        self.speak(OFFER).await?;
        self.turn_complete().await?;
        // Extraction now captures the slots: collect completes, offer is
        // already satisfied, and confirm declares book_table.
        self.inbox.steer_from = Some(Instant::now());

        self.caller_says(PICK).await?;
        self.speak(READ_BACK).await?;
        self.turn_complete().await?;

        self.caller_says(YES).await?;
        let booking = self
            .tool_call(
                "book_table",
                json!({ "party_size": 4, "slot": "2026-10-14T19:00", "name": "Rossi" }),
            )
            .await?;
        if booking.get("reference").and_then(Value::as_str) != Some("TR-2044") {
            self.speak(SORRY).await?;
            self.turn_complete().await?;
            return Err(format!("book_table answered {booking}"));
        }
        self.shared.stats.lock().calls_booked += 1;
        self.speak(GOODBYE).await?;
        self.turn_complete().await?;

        // The caller hangs up.
        match tokio::time::timeout(Duration::from_secs(15), self.inbox.next()).await {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err("spoke after the goodbye".into()),
            Err(_) => Err("never hung up".into()),
        }
    }

    async fn send(&mut self, v: Value) -> Step {
        self.sink
            .send(Message::Text(v.to_string().into()))
            .await
            .map_err(|_| "socket closed".to_string())
    }

    async fn expect_setup(&mut self) -> Step {
        match tokio::time::timeout(Duration::from_secs(30), self.inbox.next()).await {
            Ok(Some(Inbound::Setup)) => Ok(()),
            Ok(_) => Err("no setup".into()),
            Err(_) => Err("setup timed out".into()),
        }
    }

    /// Wait for a `clientContent` that completes the turn (the greeting).
    /// Model-role context turns (stage postures) arrive without it.
    async fn await_turn(&mut self) -> Step {
        loop {
            match tokio::time::timeout(Duration::from_secs(30), self.inbox.next()).await {
                Ok(Some(Inbound::Content {
                    turn_complete: true,
                })) => return Ok(()),
                Ok(Some(_)) => continue,
                Ok(None) => return Err("closed before the greeting".into()),
                Err(_) => return Err("no greeting".into()),
            }
        }
    }

    /// Sleep, draining whatever the client sends meanwhile.
    async fn pause(&mut self, ms: u64) -> Step {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return Ok(()),
                e = self.inbox.next() => {
                    if e.is_none() {
                        return Err("closed mid-call".into());
                    }
                }
            }
        }
    }

    /// Wait until the client has streamed `ms` more milliseconds of mic
    /// audio than `from`, draining other messages meanwhile.
    async fn hear(&mut self, from: f64, ms: f64) -> Step {
        let target = from + ms;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if *self.mic.borrow_and_update() >= target {
                return Ok(());
            }
            tokio::select! {
                changed = self.mic.changed() => {
                    if changed.is_err() {
                        return Err("closed while the caller spoke".into());
                    }
                }
                e = self.inbox.next() => {
                    if e.is_none() {
                        return Err("closed while the caller spoke".into());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return Err("mic stopped".into()),
            }
        }
    }

    /// The caller speaks `chunks` (each takes its words' worth of mic audio),
    /// with input transcription as each lands; then silence until the turn
    /// is taken as over, and the model's pause before it answers.
    async fn caller_says(&mut self, chunks: &[&str]) -> Step {
        let t = self.shared.timing;
        let mut at = *self.mic.borrow();
        self.hear(at, t.caller_gap_ms as f64).await?;
        at += t.caller_gap_ms as f64;
        for chunk in chunks {
            let ms = (chunk.split_whitespace().count() as u64 * MS_PER_WORD) as f64;
            self.hear(at, ms).await?;
            at += ms;
            self.send(json!({ "serverContent": { "inputTranscription": { "text": chunk } } }))
                .await?;
        }
        self.hear(at, t.end_of_speech_ms as f64).await?;
        self.pause(t.reply_ms).await
    }

    /// Speak `text` in real time: 40 ms audio frames with the output
    /// transcription a word at a time between them.
    async fn speak(&mut self, text: &str) -> Step {
        let words: Vec<&str> = text.split(' ').collect();
        let frames = (words.len() as u64 * MS_PER_WORD).div_ceil(OUT_FRAME_MS);
        let start = tokio::time::Instant::now();
        let mut said = 0usize;
        for i in 0..frames {
            let due = words.len() * (i as usize + 1) / frames as usize;
            while said < due {
                let word = if said == 0 {
                    words[0].to_string()
                } else {
                    format!(" {}", words[said])
                };
                said += 1;
                self.send(json!({ "serverContent": { "outputTranscription": { "text": word } } }))
                    .await?;
            }
            let mut head = [0u8; 9];
            head[..8].copy_from_slice(&now_us().to_le_bytes());
            let data = format!(
                "{}{}",
                base64::engine::general_purpose::STANDARD.encode(head),
                self.shared.audio_tail_b64
            );
            self.send(json!({ "serverContent": { "modelTurn": { "parts": [
                { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": data } }
            ] } } }))
            .await?;
            let next = start + Duration::from_millis((i + 1) * OUT_FRAME_MS);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(next) => break,
                    e = self.inbox.next() => {
                        if e.is_none() {
                            return Err("closed while the model spoke".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn turn_complete(&mut self) -> Step {
        self.send(json!({ "serverContent": { "generationComplete": true } }))
            .await?;
        self.send(json!({
            "serverContent": { "turnComplete": true },
            "usageMetadata": { "promptTokenCount": 900, "responseTokenCount": 60, "totalTokenCount": 960 }
        }))
        .await
    }

    /// Call `name` and return its response, timing the round trip.
    async fn tool_call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        self.calls += 1;
        let id = format!("fc-{}", self.calls);
        self.send(json!({ "toolCall": { "functionCalls": [ { "id": id, "name": name, "args": args } ] } }))
            .await?;
        let sent = Instant::now();
        loop {
            match tokio::time::timeout(Duration::from_secs(30), self.inbox.next()).await {
                Ok(Some(Inbound::ToolResponse(r))) => {
                    self.shared
                        .stats
                        .lock()
                        .tool_rtt_ms
                        .entry(name.to_string())
                        .or_default()
                        .push(sent.elapsed().as_secs_f32() * 1000.0);
                    let responses = field(&r, "functionResponses", "function_responses")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let ours = responses
                        .iter()
                        .find(|f| f.get("name").and_then(Value::as_str) == Some(name))
                        .ok_or_else(|| format!("no response for {name}"))?;
                    if ours.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                        return Err(format!("{name} answered with the wrong id"));
                    }
                    return Ok(ours.get("response").cloned().unwrap_or(Value::Null));
                }
                Ok(Some(_)) => continue,
                Ok(None) => return Err(format!("closed during {name}")),
                Err(_) => return Err(format!("{name} never answered")),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The REST side: the extraction model
// ---------------------------------------------------------------------------

/// Answer an extraction request from the transcript it carries, after the
/// configured latency. The two extractors are told apart by their schema.
async fn generate(
    State(shared): State<App>,
    Path(_rest): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    shared.stats.lock().extract_requests += 1;
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if shared.timing.log {
        println!("<- POST {request}");
    }
    let prompt: String = request
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("parts").and_then(Value::as_array))
        .flatten()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let schema = request
        .get("generationConfig")
        .or_else(|| request.get("generation_config"))
        .map(Value::to_string)
        .unwrap_or_default();
    let answer = if schema.contains("book_table_confirmed") {
        // The caller's yes, after the read-back.
        let yes = prompt.contains("Yes, please book it");
        json!({
            "book_table_confirmed": if yes { json!(true) } else { Value::Null },
            "intent_human_agent": Value::Null,
        })
    } else if prompt.contains("table for four") {
        json!({ "party_size": 4, "slot": "2026-10-14T19:00", "guest_name": "Rossi" })
    } else {
        json!({ "party_size": Value::Null, "slot": Value::Null, "guest_name": Value::Null })
    };
    tokio::time::sleep(Duration::from_millis(shared.timing.extract_ms)).await;
    axum::Json(json!({
        "candidates": [{
            "content": { "role": "model", "parts": [ { "text": answer.to_string() } ] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": { "promptTokenCount": 420, "candidatesTokenCount": 24, "totalTokenCount": 444 },
        "modelVersion": "gemini-3.5-flash-lite"
    }))
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

fn quantiles(v: &mut [f32]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| {
        let x = v[((p * v.len() as f64) as usize).min(v.len() - 1)];
        (x as f64 * 10.0).round() / 10.0
    };
    json!({ "n": v.len(), "p50": q(0.5), "p99": q(0.99), "p999": q(0.999), "max": q(1.0) })
}

async fn stats(State(shared): State<App>) -> axum::Json<Value> {
    let mut s = shared.stats.lock();
    let failures = s.failures.clone();
    let tool_rtt: serde_json::Map<String, Value> = s
        .tool_rtt_ms
        .iter_mut()
        .map(|(k, v)| (k.clone(), quantiles(v)))
        .collect();
    axum::Json(json!({
        "calls_started": s.calls_started,
        "calls_booked": s.calls_booked,
        "calls_hung_up": s.calls_hung_up,
        "failures": failures,
        "mic_secs": (s.mic_ms / 10.0).round() / 100.0,
        "mic_late_ms": quantiles(&mut s.mic_late_ms),
        "tool_rtt_ms": tool_rtt,
        "steer_ms": quantiles(&mut s.steer_ms),
        "call_secs_total": (s.call_secs.iter().map(|x| *x as f64).sum::<f64>() * 10.0).round() / 10.0,
        "call_secs": quantiles(&mut s.call_secs),
        "extract_requests": s.extract_requests,
        "context_updates": s.context_updates,
        "posture_turns": s.posture_turns,
    }))
}

async fn reset(State(shared): State<App>) -> &'static str {
    *shared.stats.lock() = Stats::default();
    "ok"
}
