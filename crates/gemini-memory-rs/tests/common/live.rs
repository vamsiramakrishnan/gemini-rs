//! Shared harness for tests that drive a real Live WebSocket session.
//!
//! Text in, audio out: only the input is text, and the model answers in voice
//! exactly as it does in production. Assertions read the **output
//! transcription**, because reading a text-modality response would exercise a
//! path no deployment uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;

use gemini_adk_fluent_rs::live::Live;
use gemini_genai_rs::prelude::ModelId;

/// How long to wait for the model to finish a turn.
///
/// Voice turns are slower than text ones, a turn that calls a tool contains two
/// model generations, and the extractor runs an out-of-band model call at the
/// boundary.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(90);

/// Cap on the handshake, so a rejected setup fails loudly instead of retrying
/// behind a wait that never resolves.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Grace period after a turn boundary for a trailing final transcript.
const TRANSCRIPT_GRACE: Duration = Duration::from_millis(750);

/// The Live model these tests drive.
///
/// **The name differs by platform**, which is why this is resolved at runtime
/// rather than pinned to a [`ModelId`] variant:
///
/// | Platform | Native-audio Live model |
/// |---|---|
/// | Google AI (AI Studio) | `gemini-2.5-flash-native-audio-preview-12-2025` |
/// | Vertex AI | `gemini-2.5-flash-native-audio` |
///
/// These tests run against Google AI, so they default to the AI Studio name.
/// Override with `GEMINI_LIVE_MODEL` to point at Vertex, a different preview,
/// or a newer release.
///
/// Neither named variant in the enum works here: `Gemini2_0FlashLive`
/// (`gemini-2.0-flash-live-001`) and `GeminiLive2_5FlashNativeAudio`
/// (`gemini-live-2.5-flash-native-audio`) both draw "not found for API version
/// v1beta, or is not supported for bidiGenerateContent" from Google AI. List
/// what a key can actually reach with:
///
/// ```text
/// curl "https://generativelanguage.googleapis.com/v1beta/models?key=$GEMINI_API_KEY" \
///   | jq -r '.models[] | select(.supportedGenerationMethods[]? == "bidiGenerateContent") | .name'
/// ```
pub fn live_model() -> ModelId {
    ModelId::new(
        std::env::var("GEMINI_LIVE_MODEL")
            .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".to_string()),
    )
}

/// One tool response seen on the wire.
#[derive(Clone, Debug)]
pub struct ToolTrace {
    /// The tool that produced it.
    pub name: String,
    /// What it handed back to the model.
    pub payload: serde_json::Value,
}

/// One tool invocation, as the model wrote it.
///
/// Worth capturing separately from the response: when a recall comes back with
/// the wrong facts, the first question is always whether retrieval ranked badly
/// or whether the model asked for something else entirely, and the arguments
/// are the only place that is visible.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// The tool the model called.
    pub name: String,
    /// The arguments it wrote.
    pub args: serde_json::Value,
}

/// What a session observed while it ran.
#[derive(Default)]
pub struct Observed {
    /// Finalized output transcripts, in order.
    pub spoken: Mutex<Vec<String>>,
    /// Every tool response, in order, with its payload.
    pub tools: Mutex<Vec<ToolTrace>>,
    /// Every tool invocation, in order, with the arguments the model wrote.
    pub calls: Mutex<Vec<ToolCall>>,
    /// Bytes of PCM received — proof the session really is in voice mode.
    pub audio_bytes: AtomicUsize,
    /// Errors reported by the server or processor.
    pub errors: Mutex<Vec<String>>,
    /// Set once the session goes away, with the reason if there was one.
    ///
    /// Waiting out the full [`TURN_TIMEOUT`] on a socket that has already
    /// closed costs a minute and a half and then reports the wrong thing —
    /// "the model said nothing" rather than "the connection dropped".
    pub closed: Mutex<Option<String>>,
    /// Fired at every turn boundary.
    turn: Notify,
}

/// A position in the observation log, taken before a question is asked.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    spoken: usize,
    tools: usize,
    calls: usize,
}

impl Observed {
    /// Where the log stands right now.
    pub fn mark(&self) -> Mark {
        Mark {
            spoken: self.spoken.lock().len(),
            tools: self.tools.lock().len(),
            calls: self.calls.lock().len(),
        }
    }

    /// Everything spoken since `mark`, lowercased and joined.
    pub fn spoken_since(&self, mark: Mark) -> String {
        self.spoken.lock()[mark.spoken..].join(" ").to_lowercase()
    }

    /// Every tool response since `mark`.
    pub fn tools_since(&self, mark: Mark) -> Vec<ToolTrace> {
        self.tools.lock()[mark.tools..].to_vec()
    }

    /// Every tool invocation since `mark`.
    pub fn calls_since(&self, mark: Mark) -> Vec<ToolCall> {
        self.calls.lock()[mark.calls..].to_vec()
    }

    /// Every tool response the session has seen, from the start.
    pub fn all_tools(&self) -> Vec<ToolTrace> {
        self.tools.lock().clone()
    }

    /// Wait until the model has said something since `mark`.
    ///
    /// A turn that calls a tool can cross more than one turn boundary before an
    /// answer arrives, so waiting for a single `turn_complete` is not enough:
    /// this waits for boundaries until words actually appear, bounded by
    /// [`TURN_TIMEOUT`].
    pub async fn await_answer(&self, mark: Mark, what: &str) -> String {
        self.try_answer(mark, what).await.unwrap_or_else(|| {
            panic!(
                "the model said nothing within {TURN_TIMEOUT:?} while {what}\n  {}",
                self.report()
            )
        })
    }

    /// As [`Observed::await_answer`], but `None` if the turn stayed silent.
    ///
    /// A turn the server never answers is a stall, not a result — the API goes
    /// quiet often enough that a caller asking several questions in a row wants
    /// to re-ask rather than fail. A *disconnect* still panics: re-sending into
    /// a closed socket cannot work, and the reason should be reported as what
    /// it is rather than as a silent model.
    pub async fn try_answer(&self, mark: Mark, what: &str) -> Option<String> {
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let _ = tokio::time::timeout(remaining, self.turn.notified()).await;

            // Output transcription can trail the turn boundary by a frame.
            tokio::time::sleep(TRANSCRIPT_GRACE).await;
            let said = self.spoken_since(mark);
            if !said.trim().is_empty() {
                return Some(said);
            }
            if let Some(reason) = self.closed.lock().clone() {
                panic!(
                    "the session disconnected while {what} — this is the API dropping the \
                     connection, not a memory failure: {reason}\n  {}",
                    self.report()
                );
            }
        }
    }

    /// Everything the model said, lowercased and joined.
    pub fn transcript(&self) -> String {
        self.spoken.lock().join(" ").to_lowercase()
    }

    /// A dump of everything observed, for a failure message.
    pub fn report(&self) -> String {
        let tools = self
            .calls
            .lock()
            .iter()
            .map(|c| format!("{}({})", c.name, c.args))
            .chain(
                self.tools
                    .lock()
                    .iter()
                    .map(|t| format!("{} → {}", t.name, t.payload)),
            )
            .collect::<Vec<_>>()
            .join("\n    ");
        format!(
            "spoken: {:?}\n  audio bytes: {}\n  errors: {:?}\n  tools:\n    {tools}",
            self.spoken.lock(),
            self.audio_bytes.load(Ordering::Relaxed),
            self.errors.lock(),
        )
    }
}

/// Attach observation to a builder and connect.
///
/// The caller supplies everything the test is actually about — model, memory,
/// instruction — and this adds only the callbacks that watch.
pub async fn connect(
    builder: Live,
    observed: Arc<Observed>,
) -> Result<gemini_adk_rs::live::LiveHandle, gemini_adk_rs::error::AgentError> {
    let (spoken, tools, calls, audio, errors, gone, turn) = (
        observed.clone(),
        observed.clone(),
        observed.clone(),
        observed.clone(),
        observed.clone(),
        observed.clone(),
        observed.clone(),
    );

    let connecting = builder
        .model(live_model())
        // Records what the model *asked* for. `before_tool` observes the call
        // without standing in for the dispatcher, which is the component under
        // test.
        .middleware(gemini_adk_fluent_rs::compose::M::before_tool(move |call| {
            calls.calls.lock().push(ToolCall {
                name: call.name.clone(),
                args: call.args.clone(),
            });
            Ok(())
        }))
        // Input transcription feeds ingestion; output transcription is what the
        // assertions read, since the model answers in audio.
        .transcription(true, true)
        .on_output_transcript(move |text, is_final| {
            if is_final && !text.trim().is_empty() {
                spoken.spoken.lock().push(text.to_string());
            }
        })
        .on_audio(move |data| {
            audio.audio_bytes.fetch_add(data.len(), Ordering::Relaxed);
        })
        // `on_tool_call` returns a value, so it has no `_concurrent` variant and
        // intercepting it would displace the dispatcher under test. This
        // observes the same calls — and, crucially, what memory handed back —
        // without standing in for anything.
        .before_tool_response(move |responses, _state| {
            let tools = tools.clone();
            async move {
                tools
                    .tools
                    .lock()
                    .extend(responses.iter().map(|r| ToolTrace {
                        name: r.name.clone(),
                        payload: r.response.clone(),
                    }));
                responses
            }
        })
        .on_error(move |msg| {
            let errors = errors.clone();
            async move {
                errors.errors.lock().push(msg);
            }
        })
        .on_disconnected(move |reason| {
            let gone = gone.clone();
            async move {
                *gone.closed.lock() = Some(reason.unwrap_or_else(|| "closed normally".into()));
                // Wake anything waiting on a turn that will now never come.
                gone.turn.notify_one();
            }
        })
        .on_turn_complete(move || {
            let turn = turn.clone();
            async move {
                turn.turn.notify_one();
            }
        })
        .connect_from_env();

    tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .unwrap_or_else(|_| {
            panic!("connect did not settle within {CONNECT_TIMEOUT:?} — see `live_model`")
        })
}
