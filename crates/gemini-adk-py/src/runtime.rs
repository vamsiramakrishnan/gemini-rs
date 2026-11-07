//! One Tokio runtime per process, created on first use.
//!
//! The runtime is the one `pyo3-async-runtimes` uses too, so an awaitable
//! handed to asyncio and a synchronous call that blocks run on the same
//! threads. Every synchronous entry point releases the GIL for the duration
//! of the Rust work ([`block_on`]); the async ones never hold it on a Tokio
//! thread ([`future_into_py`]).

use std::future::Future;
use std::sync::OnceLock;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// The process id the runtime was created in: a forked child inherits the
/// `OnceLock` but none of the worker threads, so its first call must fail
/// loudly instead of hanging.
static OWNER_PID: OnceLock<u32> = OnceLock::new();

fn init_once() -> PyResult<()> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2);
        builder
            .worker_threads(workers)
            .thread_name("gemini-adk-py-worker")
            .enable_all();
        pyo3_async_runtimes::tokio::init(builder);
        OWNER_PID.set(std::process::id()).ok();
    });
    if OWNER_PID.get().copied() != Some(std::process::id()) {
        return Err(PyRuntimeError::new_err(
            "gemini_adk was initialised in a parent process and this is a fork: \
             the Tokio runtime's threads do not survive fork(). Use \
             multiprocessing with the 'spawn' start method, or import \
             gemini_adk in the child only.",
        ));
    }
    Ok(())
}

/// The runtime, creating it on first use.
pub fn runtime() -> PyResult<&'static tokio::runtime::Runtime> {
    init_once()?;
    Ok(pyo3_async_runtimes::tokio::get_runtime())
}

/// Run `fut` to completion on the runtime with the GIL released.
///
/// Refuses to run from inside a runtime worker (a Python callback invoked by
/// a tool on the control lane): nesting `block_on` would deadlock the
/// worker. Such code uses the async form.
pub fn block_on<F, T>(py: Python<'_>, fut: F) -> PyResult<T>
where
    F: Future<Output = T> + Send,
    T: Send,
{
    let rt = runtime()?;
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(PyRuntimeError::new_err(
            "called a blocking gemini_adk entry point from inside the runtime \
             (a tool callback); use the async form (`await`) there",
        ));
    }
    Ok(py.detach(|| rt.block_on(fut)))
}

/// Hand `fut` to asyncio as an awaitable. The future runs on the runtime;
/// cancelling the Python future drops it.
pub fn future_into_py<'py, F, T>(py: Python<'py>, fut: F) -> PyResult<Bound<'py, PyAny>>
where
    F: Future<Output = PyResult<T>> + Send + 'static,
    T: for<'a> IntoPyObject<'a> + Send + 'static,
{
    runtime()?;
    pyo3_async_runtimes::tokio::future_into_py(py, fut)
}
