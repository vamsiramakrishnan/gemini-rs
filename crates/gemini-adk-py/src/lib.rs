//! Python bindings for gemini-rs.
//!
//! One extension module, `gemini_adk._gemini_adk`, built by maturin. The
//! pure Python package `gemini_adk` re-exports it and adds the `@tool`
//! decorator and the Pydantic helper. Design:
//! `docs/cleanup/python-binding-design.md`.
//!
//! Lanes: Python never runs on the fast lane (`live.rs`); tool callables run
//! on the control lane, on the blocking pool or their own asyncio loop
//! (`tools.rs`); every entry point releases the GIL while Rust runs
//! (`runtime.rs`).

mod agent;
mod convert;
mod errors;
mod live;
mod runtime;
mod spec;
mod testing;
mod tools;

use pyo3::prelude::*;

/// The `_gemini_adk` extension module.
#[pymodule]
fn _gemini_adk(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    errors::register(py, m)?;
    m.add_class::<tools::PyTool>()?;
    m.add_class::<agent::PyMockLlm>()?;
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<agent::PyChat>()?;
    m.add_class::<agent::PyEventStream>()?;
    m.add_class::<live::PyLiveSession>()?;
    m.add_class::<live::PyLiveEvents>()?;
    m.add_function(wrap_pyfunction!(live::connect, m)?)?;
    m.add_function(wrap_pyfunction!(live::scripted, m)?)?;
    m.add_class::<spec::PySessionSpec>()?;
    m.add_class::<spec::PyConversation>()?;
    m.add_class::<spec::PySim>()?;
    m.add_function(wrap_pyfunction!(spec::conversation_schema, m)?)?;
    m.add_function(wrap_pyfunction!(spec::validate_conversation, m)?)?;
    m.add_class::<testing::PyAudioProbe>()?;
    m.add_function(wrap_pyfunction!(testing::_raise, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_variants, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_thread_token, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_inside_runtime, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
