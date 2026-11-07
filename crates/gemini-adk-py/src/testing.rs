//! Hooks for the Python test suite: they construct runtime conditions the
//! public surface cannot reach on purpose (every error variant, the fast
//! lane's thread). Underscore-prefixed on the module; not part of the API.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use gemini_adk_rs::error::{AgentError, ToolError};
use gemini_adk_rs::llm::LlmError;
use gemini_adk_rs::state::StateError;
use gemini_genai_rs::session::SessionError;
use pyo3::prelude::*;

use crate::errors::to_py_err;

/// Raise the Python exception `to_py_err` maps `variant` to.
#[pyfunction]
pub fn _raise(variant: &str) -> PyResult<()> {
    let err = match variant {
        "Session" => AgentError::Session(SessionError::NotConnected),
        "Tool.ExecutionFailed" => AgentError::Tool(ToolError::ExecutionFailed("boom".into())),
        "Tool.NotFound" => AgentError::Tool(ToolError::NotFound("nope".into())),
        "Tool.InvalidArgs" => AgentError::Tool(ToolError::InvalidArgs("bad".into())),
        "Tool.Cancelled" => AgentError::Tool(ToolError::Cancelled),
        "Tool.Declined" => AgentError::Tool(ToolError::Declined("no".into())),
        "Tool.Timeout" => AgentError::Tool(ToolError::Timeout(std::time::Duration::from_secs(1))),
        "Tool.Other" => AgentError::Tool(ToolError::Other("other".into())),
        "Llm" => AgentError::Llm(LlmError::Other("llm".into())),
        "State" => match serde_json::from_str::<i32>("x") {
            Err(source) => AgentError::State(StateError::WrongType {
                key: "k".into(),
                source,
            }),
            Ok(_) => AgentError::Other("unreachable: \"x\" is not an i32".into()),
        },
        "InvalidOutput" => AgentError::InvalidOutput {
            expected: "T",
            reason: "why".into(),
            text: "raw".into(),
        },
        "UnknownAgent" => AgentError::UnknownAgent("a".into()),
        "TransferRequested" => AgentError::TransferRequested("a".into()),
        "TransferFailed" => AgentError::TransferFailed("a".into()),
        "SessionClosed" => AgentError::SessionClosed,
        "Timeout" => AgentError::Timeout,
        "Config" => AgentError::Config("cfg".into()),
        "Other" => AgentError::Other("other".into()),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown AgentError variant {other:?}"
            )))
        }
    };
    Err(to_py_err(err))
}

/// Every variant name `_raise` accepts, so the test cannot drift from it.
#[pyfunction]
pub fn _variants() -> Vec<&'static str> {
    vec![
        "Session",
        "Tool.ExecutionFailed",
        "Tool.NotFound",
        "Tool.InvalidArgs",
        "Tool.Cancelled",
        "Tool.Declined",
        "Tool.Timeout",
        "Tool.Other",
        "Llm",
        "State",
        "InvalidOutput",
        "UnknownAgent",
        "TransferRequested",
        "TransferFailed",
        "SessionClosed",
        "Timeout",
        "Config",
        "Other",
    ]
}

/// The current thread's identity, in the same form `AudioProbe` records.
#[pyfunction]
pub fn _thread_token() -> String {
    format!("{:?}", std::thread::current().id())
}

/// Whether this thread is inside the Tokio runtime (a worker or a
/// blocking-pool thread).
#[pyfunction]
pub fn _inside_runtime() -> bool {
    tokio::runtime::Handle::try_current().is_ok()
}

/// What a fast-lane `on_audio` callback saw: the thread it ran on and when.
#[derive(Default)]
pub struct AudioProbe {
    pub threads: Mutex<Vec<String>>,
    pub arrivals: Mutex<Vec<Instant>>,
}

impl AudioProbe {
    pub fn record(&self) {
        // Test-only: a lock on the fast lane is acceptable here because the
        // probe exists to measure that lane in a scripted session.
        if let Ok(mut t) = self.threads.lock() {
            t.push(format!("{:?}", std::thread::current().id()));
        }
        if let Ok(mut a) = self.arrivals.lock() {
            a.push(Instant::now());
        }
    }
}

/// The probe's findings, for `LiveSession._audio_probe()`.
#[pyclass(name = "_AudioProbe", module = "gemini_adk._gemini_adk", frozen)]
pub struct PyAudioProbe {
    pub inner: Arc<AudioProbe>,
}

#[pymethods]
impl PyAudioProbe {
    /// Distinct thread tokens the `on_audio` callback ran on.
    fn threads(&self) -> Vec<String> {
        let mut t = self
            .inner
            .threads
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        t.sort();
        t.dedup();
        t
    }

    /// How many frames the callback saw.
    fn frames(&self) -> usize {
        self.inner.arrivals.lock().map(|a| a.len()).unwrap_or(0)
    }

    /// Gaps between consecutive frames, in milliseconds.
    fn gaps_ms(&self) -> Vec<f64> {
        let arrivals = self
            .inner
            .arrivals
            .lock()
            .map(|a| a.clone())
            .unwrap_or_default();
        arrivals
            .windows(2)
            .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
            .collect()
    }
}
