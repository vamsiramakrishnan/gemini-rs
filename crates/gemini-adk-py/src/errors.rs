//! One exception hierarchy, rooted at `gemini_adk.GeminiAdkError`.
//!
//! Every `AgentError` variant maps to one class (`to_py_err`); the mapping is
//! the only place the two vocabularies meet, and `every_variant_maps` in the
//! Python tests constructs each variant through the test hooks to check it.

use gemini_adk_rs::error::{AgentError, ConfigError as AdkConfigError, ToolError as AdkToolError};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTimeoutError};
use pyo3::prelude::*;

create_exception!(
    gemini_adk,
    GeminiAdkError,
    PyException,
    "Base of every gemini_adk error."
);
create_exception!(
    gemini_adk,
    ConfigError,
    GeminiAdkError,
    "An agent, session or spec was configured wrongly."
);
create_exception!(
    gemini_adk,
    SessionError,
    GeminiAdkError,
    "The Live session failed (transport, setup, codec)."
);
create_exception!(
    gemini_adk,
    SessionClosedError,
    SessionError,
    "The Live session is closed."
);
create_exception!(gemini_adk, ToolError, GeminiAdkError, "A tool call failed.");
create_exception!(
    gemini_adk,
    ToolNotFoundError,
    ToolError,
    "No tool of that name is registered."
);
create_exception!(
    gemini_adk,
    ToolArgumentError,
    ToolError,
    "The tool's arguments were invalid."
);
create_exception!(
    gemini_adk,
    ToolExecutionError,
    ToolError,
    "The tool ran and failed, or was cancelled."
);
create_exception!(
    gemini_adk,
    LlmError,
    GeminiAdkError,
    "The model call failed (API, auth, transport)."
);
create_exception!(
    gemini_adk,
    StateError,
    GeminiAdkError,
    "A session-state read or write failed."
);
create_exception!(
    gemini_adk,
    InvalidOutputError,
    GeminiAdkError,
    "The model's reply did not match the requested schema, even after one correction."
);
create_exception!(
    gemini_adk,
    TransferError,
    GeminiAdkError,
    "An agent transfer named an unknown agent or failed."
);
create_exception!(
    gemini_adk,
    SpecError,
    GeminiAdkError,
    "A session spec or conversation did not validate; `.diagnostics` holds the details."
);

/// `GeminiAdkError` and `builtins.TimeoutError` at once, so both `except`
/// clauses catch a timed-out call.
pub fn timeout_error(py: Python<'_>) -> PyResult<Py<pyo3::types::PyType>> {
    static CLASS: pyo3::sync::PyOnceLock<Py<pyo3::types::PyType>> = pyo3::sync::PyOnceLock::new();
    CLASS
        .get_or_try_init(py, || {
            let bases = (
                py.get_type::<GeminiAdkError>(),
                py.get_type::<PyTimeoutError>(),
            );
            let class = pyo3::types::PyType::new::<pyo3::types::PyType>(py).call1((
                "TimeoutError",
                bases,
                pyo3::types::PyDict::new(py),
            ))?;
            class.setattr("__module__", "gemini_adk")?;
            Ok::<_, PyErr>(class.cast_into::<pyo3::types::PyType>()?.unbind())
        })
        .map(|c| c.clone_ref(py))
}

/// The Python exception for an `AgentError`.
pub fn to_py_err(err: AgentError) -> PyErr {
    let message = err.to_string();
    match err {
        AgentError::Config(_) => ConfigError::new_err(message),
        AgentError::SessionClosed => SessionClosedError::new_err(message),
        AgentError::Session(_) => SessionError::new_err(message),
        AgentError::Tool(tool) => tool_error(tool),
        AgentError::Llm(_) => LlmError::new_err(message),
        AgentError::State(_) => StateError::new_err(message),
        AgentError::InvalidOutput { .. } => InvalidOutputError::new_err(message),
        AgentError::UnknownAgent(_)
        | AgentError::TransferRequested(_)
        | AgentError::TransferFailed(_) => TransferError::new_err(message),
        AgentError::Timeout => Python::attach(|py| match timeout_error(py) {
            Ok(class) => PyErr::from_type(class.into_bound(py), message),
            Err(e) => e,
        }),
        _ => GeminiAdkError::new_err(message),
    }
}

/// The Python exception for a `ToolError`.
pub fn tool_error(err: AdkToolError) -> PyErr {
    let message = err.to_string();
    match err {
        AdkToolError::NotFound(_) => ToolNotFoundError::new_err(message),
        AdkToolError::InvalidArgs(_) => ToolArgumentError::new_err(message),
        _ => ToolExecutionError::new_err(message),
    }
}

/// The Python exception for a builder's `ConfigError`.
pub fn config_error(err: AdkConfigError) -> PyErr {
    ConfigError::new_err(err.to_string())
}

/// A `SpecError` carrying structured diagnostics in `.diagnostics`.
pub fn spec_error(
    py: Python<'_>,
    message: impl Into<String>,
    diagnostics: serde_json::Value,
) -> PyErr {
    let err = SpecError::new_err(message.into());
    if let Ok(value) = pythonize::pythonize(py, &diagnostics) {
        let _ = err.value(py).setattr("diagnostics", value);
    }
    err
}

/// Register every exception class on the module.
pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("GeminiAdkError", py.get_type::<GeminiAdkError>())?;
    m.add("ConfigError", py.get_type::<ConfigError>())?;
    m.add("SessionError", py.get_type::<SessionError>())?;
    m.add("SessionClosedError", py.get_type::<SessionClosedError>())?;
    m.add("ToolError", py.get_type::<ToolError>())?;
    m.add("ToolNotFoundError", py.get_type::<ToolNotFoundError>())?;
    m.add("ToolArgumentError", py.get_type::<ToolArgumentError>())?;
    m.add("ToolExecutionError", py.get_type::<ToolExecutionError>())?;
    m.add("LlmError", py.get_type::<LlmError>())?;
    m.add("StateError", py.get_type::<StateError>())?;
    m.add("InvalidOutputError", py.get_type::<InvalidOutputError>())?;
    m.add("TransferError", py.get_type::<TransferError>())?;
    m.add("SpecError", py.get_type::<SpecError>())?;
    m.add("TimeoutError", timeout_error(py)?)?;
    Ok(())
}
