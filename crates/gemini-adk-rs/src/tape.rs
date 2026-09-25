//! Record model and resolver outputs once; replay them without the network.
//!
//! [`replay_session`](crate::live::replay::replay_session) re-drives a
//! recorded Live session through the real control plane, but anything that
//! calls a model or a remote service out of band still runs for real:
//!
//! - an LLM extractor;
//! - an async resolver behind a slot;
//! - a background agent.
//!
//! A replay would then cost money, need credentials, and could answer
//! differently. A [`Tape`] closes that gap. While recording, every call's
//! input and output are appended to it. While replaying, calls are answered
//! from it, in the order they were recorded, and nothing leaves the process.
//!
//! - [`TapedLlm`] wraps any [`BaseLlm`]. [`TapedLlm::recording`] passes calls
//!   through and records them; [`TapedLlm::replaying`] needs no inner model
//!   and no credentials.
//! - [`taped_resolver`] wraps a resolver's `fetch` the same way, for
//!   `Extract::field_resolve` and `Conversation::resolve_slot`.
//!
//! Calls are matched on their canonical JSON input: the request, or the
//! resolver's name and arguments. The same input recorded twice replays its
//! two outputs in order. A replay that asks for something the tape does not
//! hold fails loudly rather than calling out.
//!
//! ```
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use gemini_adk_rs::llm::{BaseLlm, LlmRequest, LlmResponse, MockLlm};
//! use gemini_adk_rs::tape::{MemoryTape, TapedLlm};
//!
//! let tape = Arc::new(MemoryTape::new());
//!
//! // Record against a real (here: scripted) model.
//! let live = TapedLlm::recording(MockLlm::script([LlmResponse::from_text("Paris")]), tape.clone());
//! live.generate(LlmRequest::from_text("Capital of France?")).await?;
//!
//! // Replay with no model at all.
//! let offline = TapedLlm::replaying("gemini-2.5-flash", tape);
//! let reply = offline.generate(LlmRequest::from_text("Capital of France?")).await?;
//! assert_eq!(reply.text(), "Paris");
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse, ModelCapabilities};

/// The kind of call a [`TapeEntry`] records.
pub const LLM_CALL: &str = "llm";

/// One recorded call: what went in and what came out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TapeEntry {
    /// What kind of call this was: [`LLM_CALL`], or `resolver:{name}`.
    pub kind: String,
    /// The call's input as canonical JSON (object keys sorted).
    pub input: String,
    /// The call's output, or the error message it failed with.
    pub output: Result<Value, String>,
}

/// Where recorded calls are kept.
pub trait Tape: Send + Sync {
    /// Append a call.
    fn record(&self, entry: TapeEntry);

    /// Take the next unreplayed output recorded for `kind` and `input`, in
    /// recording order. `None` when the tape holds no (more) such call.
    fn next(&self, kind: &str, input: &str) -> Option<Result<Value, String>>;
}

/// Recorded calls, queued per `(kind, input)` for replay.
#[derive(Debug, Default)]
struct Queues {
    entries: Vec<TapeEntry>,
    pending: HashMap<(String, String), VecDeque<Result<Value, String>>>,
}

impl Queues {
    fn push(&mut self, entry: TapeEntry) {
        self.pending
            .entry((entry.kind.clone(), entry.input.clone()))
            .or_default()
            .push_back(entry.output.clone());
        self.entries.push(entry);
    }

    fn next(&mut self, kind: &str, input: &str) -> Option<Result<Value, String>> {
        self.pending
            .get_mut(&(kind.to_string(), input.to_string()))?
            .pop_front()
    }
}

/// A tape held in memory.
#[derive(Debug, Default)]
pub struct MemoryTape {
    queues: Mutex<Queues>,
}

impl MemoryTape {
    /// An empty tape.
    pub fn new() -> Self {
        Self::default()
    }

    /// A tape holding `entries`, ready to replay.
    pub fn from_entries(entries: impl IntoIterator<Item = TapeEntry>) -> Self {
        let tape = Self::new();
        for entry in entries {
            tape.record(entry);
        }
        tape
    }

    /// Every call recorded so far, in order.
    pub fn entries(&self) -> Vec<TapeEntry> {
        self.queues.lock().entries.clone()
    }
}

impl Tape for MemoryTape {
    fn record(&self, entry: TapeEntry) {
        self.queues.lock().push(entry);
    }

    fn next(&self, kind: &str, input: &str) -> Option<Result<Value, String>> {
        self.queues.lock().next(kind, input)
    }
}

/// A tape kept in a JSONL file, one [`TapeEntry`] per line.
///
/// [`FileTape::create`] starts an empty file to record into.
/// [`FileTape::open`] loads an existing one for replay. Any calls recorded
/// after that are appended to the same file.
#[derive(Debug)]
pub struct FileTape {
    path: PathBuf,
    queues: Mutex<Queues>,
    writer: Mutex<BufWriter<File>>,
}

impl FileTape {
    /// Create (or truncate) `path` and record into it.
    pub fn create(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::create(&path)?;
        Ok(Self {
            path,
            queues: Mutex::new(Queues::default()),
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Load the calls recorded in `path`, ready to replay.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut queues = Queues::default();
        for (n, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: TapeEntry = serde_json::from_str(&line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}:{}: {e}", path.display(), n + 1),
                )
            })?;
            queues.push(entry);
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            path,
            queues: Mutex::new(queues),
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    /// The file this tape reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Tape for FileTape {
    fn record(&self, entry: TapeEntry) {
        match serde_json::to_string(&entry) {
            Ok(line) => {
                let mut writer = self.writer.lock();
                if let Err(e) = writeln!(writer, "{line}").and_then(|()| writer.flush()) {
                    tracing::warn!(path = %self.path.display(), "tape write failed: {e}");
                }
            }
            Err(e) => tracing::warn!("tape entry not serializable: {e}"),
        }
        self.queues.lock().push(entry);
    }

    fn next(&self, kind: &str, input: &str) -> Option<Result<Value, String>> {
        self.queues.lock().next(kind, input)
    }
}

/// Canonical JSON for `value`: object keys sorted, no whitespace.
///
/// Values that fail to serialize fall back to their `Debug` form, so a key is
/// always produced.
pub fn canonical_json(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .map(|v| v.to_string())
        .unwrap_or_else(|e| format!("<unserializable: {e}>"))
}

enum Mode<L> {
    Recording(L),
    Replaying { model_id: String },
}

/// A [`BaseLlm`] that records its calls to a [`Tape`] or answers them from
/// one. See the [module docs](self).
pub struct TapedLlm<L = Arc<dyn BaseLlm>> {
    mode: Mode<L>,
    tape: Arc<dyn Tape>,
}

impl<L: BaseLlm> TapedLlm<L> {
    /// Pass every call through to `inner` and record it on `tape`.
    pub fn recording(inner: L, tape: Arc<dyn Tape>) -> Self {
        Self {
            mode: Mode::Recording(inner),
            tape,
        }
    }
}

impl TapedLlm {
    /// Answer every call from `tape`. No model, network or credential is
    /// used; a call the tape does not hold fails with [`LlmError::Config`].
    pub fn replaying(model_id: impl Into<String>, tape: Arc<dyn Tape>) -> Self {
        Self {
            mode: Mode::Replaying {
                model_id: model_id.into(),
            },
            tape,
        }
    }
}

#[async_trait]
impl<L: BaseLlm> BaseLlm for TapedLlm<L> {
    fn model_id(&self) -> &str {
        match &self.mode {
            Mode::Recording(inner) => inner.model_id(),
            Mode::Replaying { model_id } => model_id,
        }
    }

    fn capabilities(&self) -> ModelCapabilities {
        match &self.mode {
            Mode::Recording(inner) => inner.capabilities(),
            Mode::Replaying { model_id } => ModelCapabilities::infer_from_id(model_id),
        }
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let input = canonical_json(&request);
        match &self.mode {
            Mode::Recording(inner) => {
                let result = inner.generate(request).await;
                let output = match &result {
                    Ok(response) => {
                        serde_json::to_value(response).map_err(|e| format!("unserializable: {e}"))
                    }
                    Err(e) => Err(e.to_string()),
                };
                self.tape.record(TapeEntry {
                    kind: LLM_CALL.into(),
                    input,
                    output,
                });
                result
            }
            Mode::Replaying { .. } => match self.tape.next(LLM_CALL, &input) {
                Some(Ok(value)) => serde_json::from_value(value)
                    .map_err(|e| LlmError::Other(format!("taped response unreadable: {e}"))),
                Some(Err(message)) => Err(LlmError::Other(message)),
                None => Err(LlmError::Config(format!(
                    "the replay tape holds no (more) recorded call for this request: {}",
                    truncate(&input, 200)
                ))),
            },
        }
    }

    async fn warm_up(&self) -> Result<(), LlmError> {
        match &self.mode {
            Mode::Recording(inner) => inner.warm_up().await,
            Mode::Replaying { .. } => Ok(()),
        }
    }
}

/// Whether a [`taped_resolver`] records calls or answers them from the tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapeMode {
    /// Call through and record.
    Record,
    /// Answer from the tape; never call through.
    Replay,
}

/// Wrap a resolver's `fetch` so its calls are recorded on, or answered from,
/// `tape`.
///
/// `name` scopes the recording (use the field or slot name), so two
/// resolvers given the same arguments do not answer for each other. The
/// result has the shape `Extract::field_resolve` and
/// `Conversation::resolve_slot` take.
pub fn taped_resolver<F, Fut>(
    tape: Arc<dyn Tape>,
    mode: TapeMode,
    name: impl Into<String>,
    fetch: F,
) -> impl Fn(Value) -> std::pin::Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
+ Send
+ Sync
+ 'static
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    let kind = format!("resolver:{}", name.into());
    let fetch = Arc::new(fetch);
    move |args: Value| {
        let tape = tape.clone();
        let kind = kind.clone();
        let fetch = fetch.clone();
        Box::pin(async move {
            let input = canonical_json(&args);
            match mode {
                TapeMode::Record => {
                    let output = fetch(args).await;
                    tape.record(TapeEntry {
                        kind,
                        input,
                        output: output.clone(),
                    });
                    output
                }
                TapeMode::Replay => tape.next(&kind, &input).unwrap_or_else(|| {
                    Err(format!(
                        "the replay tape holds no (more) recorded {kind} call for {}",
                        truncate(&input, 200)
                    ))
                }),
            }
        })
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlm;
    use serde_json::json;

    #[tokio::test]
    async fn a_replay_answers_in_recorded_order_without_the_model() {
        let tape = Arc::new(MemoryTape::new());
        let live = TapedLlm::recording(
            MockLlm::script([LlmResponse::from_text("one"), LlmResponse::from_text("two")]),
            tape.clone(),
        );
        let ask = || LlmRequest::from_text("again?");
        assert_eq!(live.generate(ask()).await.unwrap().text(), "one");
        assert_eq!(live.generate(ask()).await.unwrap().text(), "two");

        let offline = TapedLlm::replaying("m", Arc::new(MemoryTape::from_entries(tape.entries())));
        assert_eq!(offline.generate(ask()).await.unwrap().text(), "one");
        assert_eq!(offline.generate(ask()).await.unwrap().text(), "two");
        let exhausted = offline.generate(ask()).await.unwrap_err();
        assert!(matches!(exhausted, LlmError::Config(_)), "{exhausted}");
    }

    #[tokio::test]
    async fn a_replay_refuses_a_request_it_never_saw() {
        let offline = TapedLlm::replaying("m", Arc::new(MemoryTape::new()));
        let err = offline
            .generate(LlmRequest::from_text("unseen"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no (more) recorded call"), "{err}");
    }

    #[tokio::test]
    async fn a_recorded_failure_replays_as_a_failure() {
        let tape = Arc::new(MemoryTape::new());
        let failing = TapedLlm::recording(
            MockLlm::from_fn(|_| Err(LlmError::RateLimited)),
            tape.clone(),
        );
        assert!(failing.generate(LlmRequest::from_text("x")).await.is_err());

        let offline = TapedLlm::replaying("m", tape);
        let err = offline
            .generate(LlmRequest::from_text("x"))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Rate limited");
    }

    #[tokio::test]
    async fn a_resolver_replays_per_name_and_arguments() {
        let tape: Arc<dyn Tape> = Arc::new(MemoryTape::new());
        let record = taped_resolver(
            tape.clone(),
            TapeMode::Record,
            "balance",
            |args| async move { Ok(json!({ "for": args["account"], "cents": 1200 })) },
        );
        let recorded = record(json!({ "account": "A1" })).await.unwrap();

        let replay = taped_resolver(tape.clone(), TapeMode::Replay, "balance", |_| async {
            Err::<Value, _>("must not be called".to_string())
        });
        assert_eq!(replay(json!({ "account": "A1" })).await.unwrap(), recorded);
        assert!(replay(json!({ "account": "B2" })).await.is_err());

        let other = taped_resolver(tape, TapeMode::Replay, "limit", |_| async {
            Err::<Value, _>("must not be called".to_string())
        });
        assert!(other(json!({ "account": "A1" })).await.is_err());
    }

    #[test]
    fn canonical_json_sorts_keys() {
        assert_eq!(
            canonical_json(&json!({ "b": 1, "a": { "d": 2, "c": 3 } })),
            r#"{"a":{"c":3,"d":2},"b":1}"#
        );
    }

    #[test]
    fn a_file_tape_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "gemini-adk-tape-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let tape = FileTape::create(&path).unwrap();
            tape.record(TapeEntry {
                kind: "resolver:x".into(),
                input: "{}".into(),
                output: Ok(json!(7)),
            });
        }
        let tape = FileTape::open(&path).unwrap();
        assert_eq!(tape.next("resolver:x", "{}"), Some(Ok(json!(7))));
        assert_eq!(tape.next("resolver:x", "{}"), None);
        std::fs::remove_file(path).unwrap();
    }
}
