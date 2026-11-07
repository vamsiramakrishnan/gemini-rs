//! Live sessions: audio frames in and out as `bytes`, events as dicts, no
//! device dependency.
//!
//! Python never runs on the fast lane. The session's `LiveEvent` broadcast
//! (the processor's own, capacity `event_capacity`, drop-oldest for a lagging
//! subscriber) feeds a bridge task that maps each event to plain data and
//! forwards it over a bounded channel; `events()` drains that channel from
//! Python. A consumer that falls more than `event_capacity + buffer` events
//! behind loses the oldest events for itself alone and is told so by a
//! `lagged` event carrying the count.

use std::sync::Arc;

use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_rs::live::LiveHandle;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::convert::{to_json, to_py};
use crate::errors::{to_py_err, SessionClosedError};
use crate::runtime::{block_on, future_into_py, runtime};
use crate::tools::runtime_tools;

/// A fast-lane or control-lane event, as plain data for Python.
pub enum PyLiveEvent {
    Audio(bytes::Bytes),
    Json(Value),
}

fn map_event(event: LiveEvent) -> Option<PyLiveEvent> {
    Some(match event {
        LiveEvent::Audio(data) => PyLiveEvent::Audio(data),
        LiveEvent::TextDelta(text) => PyLiveEvent::Json(json!({ "kind": "text", "text": text })),
        LiveEvent::TextComplete(text) => {
            PyLiveEvent::Json(json!({ "kind": "text_complete", "text": text }))
        }
        LiveEvent::InputTranscript { text, is_final } => PyLiveEvent::Json(
            json!({ "kind": "input_transcript", "text": text, "final": is_final }),
        ),
        LiveEvent::OutputTranscript { text, is_final } => PyLiveEvent::Json(
            json!({ "kind": "output_transcript", "text": text, "final": is_final }),
        ),
        LiveEvent::Thought(text) => PyLiveEvent::Json(json!({ "kind": "thought", "text": text })),
        LiveEvent::VadStart => PyLiveEvent::Json(json!({ "kind": "vad_start" })),
        LiveEvent::VadEnd => PyLiveEvent::Json(json!({ "kind": "vad_end" })),
        LiveEvent::ToolExecution { name, args, result } => PyLiveEvent::Json(
            json!({ "kind": "tool", "name": name, "args": args, "result": result }),
        ),
        LiveEvent::ToolCancelled { ids } => {
            PyLiveEvent::Json(json!({ "kind": "tool_cancelled", "ids": ids }))
        }
        LiveEvent::TurnComplete => PyLiveEvent::Json(json!({ "kind": "turn_complete" })),
        LiveEvent::Interrupted => PyLiveEvent::Json(json!({ "kind": "interrupted" })),
        LiveEvent::Connected => PyLiveEvent::Json(json!({ "kind": "connected" })),
        LiveEvent::Disconnected { reason } => {
            PyLiveEvent::Json(json!({ "kind": "disconnected", "reason": reason }))
        }
        LiveEvent::Error(message) => {
            PyLiveEvent::Json(json!({ "kind": "error", "message": message }))
        }
        LiveEvent::GoAway { time_left } => PyLiveEvent::Json(
            json!({ "kind": "go_away", "time_left_ms": time_left.as_millis() as u64 }),
        ),
        LiveEvent::Extraction { name, value } => {
            PyLiveEvent::Json(json!({ "kind": "extraction", "name": name, "value": value }))
        }
        LiveEvent::PhaseTransition { from, to, reason } => {
            PyLiveEvent::Json(json!({ "kind": "phase", "from": from, "to": to, "reason": reason }))
        }
        // Other control-lane events are not part of the first release's
        // surface; they are dropped rather than half-mapped.
        _ => return None,
    })
}

/// A Live session.
#[pyclass(name = "LiveSession", module = "gemini_adk", frozen)]
pub struct PyLiveSession {
    handle: Arc<LiveHandle>,
    /// The scripted server's control, for `ScriptedLive` sessions; releasing
    /// it starts the script.
    scripted: Option<gemini_genai_rs::transport::ReplayControl>,
    /// A fast-lane probe installed by `scripted(..., _probe=True)`.
    probe: Option<Arc<crate::testing::AudioProbe>>,
}

fn build_live(
    instruction: Option<String>,
    tools: Option<Vec<Bound<'_, PyAny>>>,
    greeting: Option<String>,
    event_capacity: Option<usize>,
    text_only: bool,
) -> PyResult<Live> {
    let mut live = Live::builder();
    if let Some(instruction) = instruction {
        live = live.instruction(instruction);
    }
    if let Some(greeting) = greeting {
        live = live.greeting(greeting);
    }
    if let Some(capacity) = event_capacity {
        live = live.event_capacity(capacity);
    }
    if text_only {
        live = live.text_only();
    }
    for tool in runtime_tools(&tools.unwrap_or_default())? {
        live = live.tool(tool);
    }
    Ok(live)
}

/// Connect a Live session to Gemini using the environment's credentials.
#[pyfunction]
#[pyo3(signature = (*, instruction=None, tools=None, greeting=None, event_capacity=None, text_only=false))]
pub fn connect(
    py: Python<'_>,
    instruction: Option<String>,
    tools: Option<Vec<Bound<'_, PyAny>>>,
    greeting: Option<String>,
    event_capacity: Option<usize>,
    text_only: bool,
) -> PyResult<PyLiveSession> {
    let live = build_live(instruction, tools, greeting, event_capacity, text_only)?;
    let handle = block_on(py, async move { live.connect_from_env().await })?.map_err(to_py_err)?;
    Ok(PyLiveSession {
        handle: Arc::new(handle),
        scripted: None,
        probe: None,
    })
}

/// A Live session against a scripted server, for tests: `frames` are the
/// server's messages (dicts) in order, played after `setupComplete`. No
/// network, no credentials.
#[pyfunction]
#[pyo3(signature = (frames, *, instruction=None, tools=None, greeting=None, event_capacity=None, text_only=false, released=true, _probe=false))]
#[allow(
    clippy::too_many_arguments,
    reason = "every parameter is a keyword with a default"
)]
pub fn scripted(
    py: Python<'_>,
    frames: Vec<Bound<'_, PyAny>>,
    instruction: Option<String>,
    tools: Option<Vec<Bound<'_, PyAny>>>,
    greeting: Option<String>,
    event_capacity: Option<usize>,
    text_only: bool,
    released: bool,
    _probe: bool,
) -> PyResult<PyLiveSession> {
    let mut live = build_live(instruction, tools, greeting, event_capacity, text_only)?;
    // The one fast-lane callback the binding ever installs, and only for
    // the test suite: it records which thread the lane runs on.
    let probe = _probe.then(|| Arc::new(crate::testing::AudioProbe::default()));
    if let Some(probe) = &probe {
        let probe = probe.clone();
        live = live.on_audio(move |_| probe.record());
    }
    let mut server = ScriptedServer::new();
    for frame in frames {
        server = server.frame(to_json(&frame)?);
    }
    let (transport, control) = server.into_transport();
    let handle = block_on(
        py,
        async move { live.connect_with_transport(transport).await },
    )?
    .map_err(to_py_err)?;
    if released {
        control.release();
    }
    Ok(PyLiveSession {
        handle: Arc::new(handle),
        scripted: Some(control),
        probe,
    })
}

#[pymethods]
impl PyLiveSession {
    /// Send one frame of PCM16 16 kHz mono audio.
    fn send_audio(&self, py: Python<'_>, data: &Bound<'_, PyBytes>) -> PyResult<()> {
        let bytes = bytes::Bytes::copy_from_slice(data.as_bytes());
        let handle = self.handle.clone();
        block_on(py, async move { handle.send_audio(bytes).await })?.map_err(session_err)
    }

    /// `await session.send_audio_async(data)`.
    fn send_audio_async<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let bytes = bytes::Bytes::copy_from_slice(data.as_bytes());
        let handle = self.handle.clone();
        future_into_py(py, async move {
            handle.send_audio(bytes).await.map_err(session_err)
        })
    }

    /// Send a text turn.
    fn send_text(&self, py: Python<'_>, text: String) -> PyResult<()> {
        let handle = self.handle.clone();
        block_on(py, async move { handle.send_text(text).await })?.map_err(session_err)
    }

    /// `await session.send_text_async(text)`.
    fn send_text_async<'py>(&self, py: Python<'py>, text: String) -> PyResult<Bound<'py, PyAny>> {
        let handle = self.handle.clone();
        future_into_py(py, async move {
            handle.send_text(text).await.map_err(session_err)
        })
    }

    /// Start the scripted server's frames (only when `scripted(...,
    /// released=False)` held them back, so a consumer can subscribe first).
    fn release(&self) {
        if let Some(control) = &self.scripted {
            control.release();
        }
    }

    /// The session's events: an iterator and async iterator of dicts with a
    /// `kind`, audio frames as `{"kind": "audio", "data": bytes}`. `buffer`
    /// is the bridge channel's depth; see the module docs for the
    /// backpressure policy.
    #[pyo3(signature = (*, buffer=64))]
    fn events(&self, buffer: usize) -> PyResult<PyLiveEvents> {
        let mut source = self.handle.events();
        let (tx, rx) = tokio::sync::mpsc::channel::<PyLiveEvent>(buffer.max(1));
        runtime()?.spawn(async move {
            loop {
                match source.recv().await {
                    Ok(event) => {
                        let last = matches!(event, LiveEvent::Disconnected { .. });
                        if let Some(mapped) = map_event(event) {
                            if tx.send(mapped).await.is_err() {
                                break;
                            }
                        }
                        // The session is over: end the stream after its
                        // final event so `for`/`async for` finish.
                        if last {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        let lagged = PyLiveEvent::Json(json!({ "kind": "lagged", "dropped": n }));
                        if tx.send(lagged).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        Ok(PyLiveEvents {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
        })
    }

    /// The fast-lane probe of a `scripted(..., _probe=True)` session.
    fn _audio_probe(&self) -> Option<crate::testing::PyAudioProbe> {
        self.probe
            .as_ref()
            .map(|p| crate::testing::PyAudioProbe { inner: p.clone() })
    }

    /// The session state key, as JSON, or `None`.
    fn state<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let value: Option<Value> = self.handle.state().get(key);
        value.as_ref().map(|v| to_py(py, v)).transpose()
    }

    /// The latest session-resumption handle, if the server sent one.
    fn resume_handle(&self) -> Option<String> {
        self.handle.resume_handle()
    }

    /// Close the session.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let handle = self.handle.clone();
        close_outcome(block_on(py, async move { handle.disconnect().await })?)
    }

    /// `await session.close_async()`.
    fn close_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = self.handle.clone();
        future_into_py(py, async move { close_outcome(handle.disconnect().await) })
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, pyo3::types::PyTuple>) -> PyResult<()> {
        self.close(py)
    }
}

fn is_closed(err: &gemini_genai_rs::session::SessionError) -> bool {
    use gemini_genai_rs::session::SessionError as E;
    matches!(err, E::NotConnected | E::ChannelClosed)
}

fn session_err(err: gemini_genai_rs::session::SessionError) -> PyErr {
    let message = err.to_string();
    if is_closed(&err) {
        SessionClosedError::new_err(message)
    } else {
        to_py_err(AgentError::Session(err))
    }
}

/// `close()` on a session that is already closed succeeds.
fn close_outcome(result: Result<(), gemini_genai_rs::session::SessionError>) -> PyResult<()> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if is_closed(&e) => Ok(()),
        Err(e) => Err(session_err(e)),
    }
}

/// The event stream of a session.
#[pyclass(name = "LiveEvents", module = "gemini_adk", frozen)]
pub struct PyLiveEvents {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<PyLiveEvent>>>,
}

fn live_event_to_py<'py>(py: Python<'py>, event: PyLiveEvent) -> PyResult<Bound<'py, PyAny>> {
    match event {
        PyLiveEvent::Audio(data) => {
            let dict = PyDict::new(py);
            dict.set_item("kind", "audio")?;
            dict.set_item("data", PyBytes::new(py, &data))?;
            Ok(dict.into_any())
        }
        PyLiveEvent::Json(value) => to_py(py, &value),
    }
}

#[pymethods]
impl PyLiveEvents {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let rx = self.rx.clone();
        let next = block_on(py, async move { rx.lock().await.recv().await })?;
        next.map(|event| live_event_to_py(py, event)).transpose()
    }

    /// The next event, or `None` when `timeout` seconds pass or the stream
    /// has ended (after the `disconnected` event).
    fn next_timeout<'py>(
        &self,
        py: Python<'py>,
        timeout: f64,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let rx = self.rx.clone();
        let wait = std::time::Duration::from_secs_f64(timeout.max(0.0));
        let next = block_on(py, async move {
            tokio::time::timeout(wait, async { rx.lock().await.recv().await })
                .await
                .unwrap_or_default()
        })?;
        next.map(|event| live_event_to_py(py, event)).transpose()
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        future_into_py(py, async move {
            match rx.lock().await.recv().await {
                Some(event) => Python::attach(|py| live_event_to_py(py, event).map(Bound::unbind)),
                None => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }
}
