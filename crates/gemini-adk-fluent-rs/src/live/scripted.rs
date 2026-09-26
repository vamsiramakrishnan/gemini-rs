//! A scripted Gemini Live server for offline session tests; see
//! [`ScriptedServer`].

use std::time::Duration;

use gemini_adk_rs::State;
use gemini_adk_rs::error::AgentError;
use gemini_adk_rs::live::replay::collect_events_until_idle;
use gemini_adk_rs::live::{LiveEvent, LiveHandle};
use gemini_genai_rs::transport::{ReplayControl, ReplayTransport};
use serde_json::{Value, json};

use super::Live;

/// A scripted Gemini Live server, for testing a [`Live`] session offline.
///
/// [`ScriptedServer`] is a script of what the server says: model text, what
/// it heard, tool calls, interruptions. [`play`](ScriptedServer::play)
/// connects your fully configured [`Live`] builder to it over an in-memory
/// transport and runs the script. Your tools, phases, extractors, watchers,
/// governance and callbacks all run for real. What comes back is a
/// [`ScriptedRun`]: the events the session emitted, what it sent to the
/// "server" (setup, tool responses, context), and its state.
///
/// No network or credential is used, and the model is not involved. The
/// script is its stand-in.
///
/// ```
/// # tokio_test::block_on(async {
/// use gemini_adk_fluent_rs::prelude::*;
/// use gemini_adk_fluent_rs::testing::ScriptedServer;
/// use serde_json::json;
///
/// let run = ScriptedServer::new()
///     .hears("What's the weather in Paris?")
///     .calls("get_weather", json!({ "city": "Paris" }))
///     .says("It is sunny in Paris.")
///     .play(
///         Live::builder()
///             .instruction("You are a weather assistant.")
///             .tools(T::simple("get_weather", "Weather for a city", |args| async move {
///                 Ok(json!({ "city": args["city"], "sky": "sunny" }))
///             })),
///     )
///     .await
///     .unwrap();
///
/// let responses = run.tool_responses();
/// assert_eq!(responses[0]["name"], "get_weather");
/// assert_eq!(responses[0]["response"]["sky"], "sunny");
/// assert!(run.transcript_text().contains("It is sunny"));
/// run.disconnect().await;
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct ScriptedServer {
    frames: Vec<Value>,
    next_call: usize,
    idle: Duration,
}

impl Default for ScriptedServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedServer {
    /// A script that starts with the setup handshake.
    pub fn new() -> Self {
        Self {
            frames: vec![json!({ "setupComplete": {} })],
            next_call: 0,
            idle: Duration::from_millis(200),
        }
    }

    /// The model says `text` and ends its turn.
    pub fn says(self, text: impl Into<String>) -> Self {
        self.text(text).turn_complete()
    }

    /// The model says `text` without ending its turn.
    pub fn text(self, text: impl Into<String>) -> Self {
        self.frame(json!({
            "serverContent": { "modelTurn": { "parts": [{ "text": text.into() }] } }
        }))
    }

    /// The model ends its turn.
    pub fn turn_complete(self) -> Self {
        self.frame(json!({ "serverContent": { "turnComplete": true } }))
    }

    /// The server transcribes the user saying `text`.
    pub fn hears(self, text: impl Into<String>) -> Self {
        self.frame(json!({
            "serverContent": { "inputTranscription": { "text": text.into() } }
        }))
    }

    /// The server transcribes the model's speech as `text`.
    pub fn speaks(self, text: impl Into<String>) -> Self {
        self.frame(json!({
            "serverContent": { "outputTranscription": { "text": text.into() } }
        }))
    }

    /// The model calls tool `name` with `args`. Call ids are `call-1`,
    /// `call-2`, … in script order.
    pub fn calls(mut self, name: impl Into<String>, args: Value) -> Self {
        self.next_call += 1;
        let id = format!("call-{}", self.next_call);
        self.frame(json!({
            "toolCall": { "functionCalls": [{ "name": name.into(), "args": args, "id": id }] }
        }))
    }

    /// The user barges in: the server reports the model's turn interrupted.
    pub fn interrupts(self) -> Self {
        self.frame(json!({ "serverContent": { "interrupted": true } }))
    }

    /// Any raw server message, for what the helpers above do not cover.
    pub fn frame(mut self, message: Value) -> Self {
        self.frames.push(message);
        self
    }

    /// How long the session must stay quiet after the last frame before the
    /// run counts as settled (default 200 ms).
    pub fn settle_after(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// The ids of the scripted tool calls the session must answer: every
    /// call not listed in a later cancellation.
    fn awaited_call_ids(&self) -> Vec<String> {
        let ids = |frame: &Value, pointer: &str| -> Vec<String> {
            frame
                .pointer(pointer)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.get("id").or(Some(v)).and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        };
        let cancelled: Vec<String> = self
            .frames
            .iter()
            .flat_map(|f| ids(f, "/toolCallCancellation/ids"))
            .collect();
        self.frames
            .iter()
            .flat_map(|f| ids(f, "/toolCall/functionCalls"))
            .filter(|id| !cancelled.contains(id))
            .collect()
    }

    /// The script as an in-memory transport plus its control handle, for a
    /// test that drives the connection itself. Nothing past the handshake
    /// flows until [`ReplayControl::release`].
    pub fn into_transport(self) -> (ReplayTransport, ReplayControl) {
        ReplayTransport::from_frames(
            self.frames
                .iter()
                .map(|frame| frame.to_string().into_bytes())
                .collect(),
        )
    }

    /// Connect `live` to this script, play every frame, and return once the
    /// session has settled: every scripted tool call that was not cancelled
    /// has been answered, and no event arrived for the
    /// [`settle_after`](Self::settle_after) window.
    pub async fn play(self, live: Live) -> Result<ScriptedRun, AgentError> {
        let idle = self.idle;
        let mut awaiting = self.awaited_call_ids();
        let (transport, control) = self.into_transport();
        let handle = live.connect_with_transport(transport).await?;
        let mut rx = handle.events();
        control.release();
        control.drained().await;
        // A slow tool emits nothing while it runs, so a quiet window alone
        // can end the run before the tool answers.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut events = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            events.extend(collect_events_until_idle(&mut rx, idle, left).await);
            awaiting.retain(|id| !answered(&control, id));
            if awaiting.is_empty() || left.is_zero() {
                break;
            }
        }
        Ok(ScriptedRun {
            handle,
            events,
            control,
        })
    }
}

/// Whether the session has sent a function response for call `id`.
fn answered(control: &ReplayControl, id: &str) -> bool {
    control.outbound_frames().iter().any(|frame| {
        serde_json::from_slice::<Value>(frame)
            .ok()
            .and_then(|m| m.pointer("/toolResponse/functionResponses").cloned())
            .and_then(|r| r.as_array().cloned())
            .is_some_and(|rs| rs.iter().any(|r| r["id"] == id))
    })
}

/// The outcome of [`ScriptedServer::play`].
pub struct ScriptedRun {
    handle: LiveHandle,
    events: Vec<LiveEvent>,
    control: ReplayControl,
}

impl ScriptedRun {
    /// The live session, still connected.
    pub fn handle(&self) -> &LiveHandle {
        &self.handle
    }

    /// The session's state.
    pub fn state(&self) -> &State {
        self.handle.state()
    }

    /// Every event the session emitted while the script played.
    pub fn events(&self) -> &[LiveEvent] {
        &self.events
    }

    /// The model text the session delivered, concatenated.
    pub fn transcript_text(&self) -> String {
        self.events
            .iter()
            .filter_map(|event| match event {
                LiveEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Every message the session sent to the server, parsed, in order: the
    /// setup first, then tool responses, context and anything else.
    pub fn sent(&self) -> Vec<Value> {
        self.control
            .outbound_frames()
            .iter()
            .filter_map(|frame| serde_json::from_slice(frame).ok())
            .collect()
    }

    /// The setup message the session opened with.
    pub fn setup(&self) -> Option<Value> {
        self.sent()
            .into_iter()
            .find_map(|m| m.get("setup").cloned())
    }

    /// Every function response the session sent, in order.
    pub fn tool_responses(&self) -> Vec<Value> {
        self.sent()
            .iter()
            .filter_map(|m| m.pointer("/toolResponse/functionResponses"))
            .filter_map(Value::as_array)
            .flatten()
            .cloned()
            .collect()
    }

    /// Disconnect the session.
    pub async fn disconnect(self) {
        let _ = self.handle.disconnect().await;
    }
}
