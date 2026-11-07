//! Python callables as runtime tools.
//!
//! The runtime calls a tool on the control lane. A synchronous callable runs
//! on Tokio's blocking pool, holding the GIL only inside the call; an async
//! callable is handed to the asyncio loop that was running when the tool was
//! registered (`asyncio.run_coroutine_threadsafe`), and the control lane
//! waits for the result without the GIL. A barge-in cancels the asyncio task.

use std::sync::Arc;

use async_trait::async_trait;
use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::{ToolContext, ToolFunction};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use serde_json::Value;

use crate::convert::{to_json, to_py};

/// A tool backed by a Python callable.
#[pyclass(name = "Tool", module = "gemini_adk", frozen)]
pub struct PyTool {
    inner: Arc<ToolInner>,
}

pub struct ToolInner {
    name: String,
    description: String,
    parameters: Option<Value>,
    callable: Py<PyAny>,
    is_async: bool,
    /// The asyncio loop an async callable belongs to, captured when the tool
    /// was made inside a running loop.
    event_loop: Option<Py<PyAny>>,
}

#[pymethods]
impl PyTool {
    /// A tool from a callable. `parameters` is the JSON Schema of its
    /// keyword arguments (the `@tool` decorator derives it from type hints).
    #[new]
    #[pyo3(signature = (callable, *, name, description, parameters=None))]
    fn new(
        py: Python<'_>,
        callable: Bound<'_, PyAny>,
        name: String,
        description: String,
        parameters: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        if !callable.is_callable() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "a tool needs a callable",
            ));
        }
        let inspect = py.import("inspect")?;
        // A plain `async def`, or a callable object whose `__call__` is one.
        let is_async = inspect
            .call_method1("iscoroutinefunction", (&callable,))?
            .is_truthy()?
            || callable
                .getattr("__call__")
                .ok()
                .map(|c| {
                    inspect
                        .call_method1("iscoroutinefunction", (c,))
                        .and_then(|r| r.is_truthy())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        let event_loop = if is_async { running_loop(py) } else { None };
        let parameters = parameters
            .filter(|p| !p.is_none())
            .map(|p| to_json(&p))
            .transpose()?;
        Ok(Self {
            inner: Arc::new(ToolInner {
                name,
                description,
                parameters,
                callable: callable.unbind(),
                is_async,
                event_loop,
            }),
        })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[getter]
    fn parameters<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .parameters
            .as_ref()
            .map(|p| to_py(py, p))
            .transpose()
    }

    #[getter]
    fn is_async(&self) -> bool {
        self.inner.is_async
    }

    /// Call the underlying function directly (tests, local use).
    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.inner.callable.bind(py).call(args, kwargs)
    }

    /// The wrapped callable.
    #[getter]
    fn func<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.inner.callable.bind(py).clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "Tool(name={:?}, async={})",
            self.inner.name, self.inner.is_async
        )
    }
}

impl PyTool {
    /// The runtime-facing tool, shared with the registry.
    pub fn runtime_tool(&self) -> RuntimeTool {
        RuntimeTool(self.inner.clone())
    }
}

/// The current asyncio loop, when this thread is running one.
fn running_loop(py: Python<'_>) -> Option<Py<PyAny>> {
    py.import("asyncio")
        .and_then(|m| m.call_method0("get_running_loop"))
        .ok()
        .map(Bound::unbind)
}

/// The `ToolFunction` the runtime sees.
pub struct RuntimeTool(Arc<ToolInner>);

/// Call the Python callable with the arguments as keywords; the return
/// value becomes the tool result (`None` becomes `null`).
fn call_sync(inner: &ToolInner, args: &Value) -> Result<Value, ToolError> {
    Python::attach(|py| -> Result<Value, ToolError> {
        let kwargs = kwargs_from(py, args)?;
        let result = inner
            .callable
            .bind(py)
            .call(PyTuple::empty(py), Some(&kwargs))
            .map_err(|e| python_failure(py, &e))?;
        if is_awaitable(py, &result) {
            return Err(ToolError::ExecutionFailed(format!(
                "tool {:?} returned an awaitable from a synchronous call; declare it `async def` \
                 (or give it an `async def __call__`) so the runtime awaits it",
                inner.name
            )));
        }
        to_json(&result).map_err(|e| ToolError::ExecutionFailed(format!("result is not JSON: {e}")))
    })
}

fn is_awaitable(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    py.import("inspect")
        .and_then(|m| m.call_method1("isawaitable", (value,)))
        .and_then(|r| r.is_truthy())
        .unwrap_or(false)
}

fn kwargs_from<'py>(py: Python<'py>, args: &Value) -> Result<Bound<'py, PyDict>, ToolError> {
    let dict = PyDict::new(py);
    if let Value::Object(map) = args {
        for (k, v) in map {
            let value = to_py(py, v).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            dict.set_item(k, value)
                .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        }
    } else if !args.is_null() {
        return Err(ToolError::InvalidArgs(format!(
            "tool arguments must be an object, got {args}"
        )));
    }
    Ok(dict)
}

/// `"{ExceptionType}: {message}"` plus the traceback, so the model's
/// correction turn and the Rust logs both see what went wrong.
fn python_failure(py: Python<'_>, err: &PyErr) -> ToolError {
    let kind = err
        .get_type(py)
        .name()
        .map(|n| n.to_string())
        .unwrap_or_default();
    let traceback = err
        .traceback(py)
        .and_then(|tb| tb.format().ok())
        .unwrap_or_default();
    ToolError::ExecutionFailed(
        format!("{kind}: {}\n{traceback}", err.value(py))
            .trim_end()
            .to_string(),
    )
}

#[async_trait]
impl ToolFunction for RuntimeTool {
    fn name(&self) -> &str {
        &self.0.name
    }

    fn description(&self) -> &str {
        &self.0.description
    }

    fn parameters(&self) -> Option<Value> {
        self.0.parameters.clone()
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        self.call_with_context(args, ToolContext::detached()).await
    }

    async fn call_with_context(&self, args: Value, ctx: ToolContext) -> Result<Value, ToolError> {
        let inner = self.0.clone();
        if !inner.is_async {
            // A sync callable blocks one blocking-pool thread, never a
            // runtime worker.
            return tokio::task::spawn_blocking(move || call_sync(&inner, &args))
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("tool thread failed: {e}")))?;
        }
        let Some(event_loop) = inner.event_loop.as_ref() else {
            return Err(ToolError::ExecutionFailed(format!(
                "async tool {:?} was created outside a running asyncio loop; create it \
                 inside the coroutine that runs the agent",
                inner.name
            )));
        };
        // Schedule the coroutine on its loop from this (Tokio) thread.
        let future = Python::attach(|py| -> Result<Py<PyAny>, ToolError> {
            let kwargs = kwargs_from(py, &args)?;
            let coro = inner
                .callable
                .bind(py)
                .call(PyTuple::empty(py), Some(&kwargs))
                .map_err(|e| python_failure(py, &e))?;
            let asyncio = py.import("asyncio").map_err(|e| python_failure(py, &e))?;
            asyncio
                .call_method1("run_coroutine_threadsafe", (coro, event_loop.bind(py)))
                .map(Bound::unbind)
                .map_err(|e| {
                    let failure = python_failure(py, &e);
                    match failure {
                        ToolError::ExecutionFailed(text) if text.contains("Event loop is closed") => {
                            ToolError::ExecutionFailed(format!(
                                "{text}\nasync tool {:?} belongs to an asyncio loop that has closed; \
                                 create the tool inside the coroutine that runs the agent",
                                inner.name
                            ))
                        }
                        other => other,
                    }
                })
        })?;
        // Wait for the concurrent.futures.Future on the blocking pool (its
        // `result()` releases the GIL while it waits), racing a barge-in.
        // The runtime drops a cancelled inline tool's future at its current
        // await point (see `tool_handler.rs`), so the guard, not the select
        // arm, is what cancels the asyncio task in that case.
        let mut guard = CancelOnDrop {
            future: future.clone_ref_any(),
            armed: true,
        };
        let waiter = {
            let future = future.clone_ref_any();
            tokio::task::spawn_blocking(move || {
                Python::attach(|py| -> Result<Value, ToolError> {
                    let result = future
                        .bind(py)
                        .call_method0("result")
                        .map_err(|e| python_failure(py, &e))?;
                    to_json(&result)
                        .map_err(|e| ToolError::ExecutionFailed(format!("result is not JSON: {e}")))
                })
            })
        };
        let outcome = tokio::select! {
            outcome = waiter => outcome
                .map_err(|e| ToolError::ExecutionFailed(format!("tool thread failed: {e}")))?,
            () = ctx.cancel.cancelled() => {
                Err(ToolError::Cancelled)
            }
        };
        // A finished future needs no cancel (and no GIL round trip on the
        // control lane); a cancelled or failed one must not run on.
        if outcome.is_ok() {
            guard.armed = false;
        }
        drop(guard);
        outcome
    }
}

/// Cancels the asyncio task behind a `concurrent.futures.Future` when the
/// control-lane future that awaited it is dropped or finishes with it still
/// pending. `cancel()` on a finished future is a no-op.
struct CancelOnDrop {
    future: Py<PyAny>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            Python::attach(|py| {
                let _ = self.future.bind(py).call_method0("cancel");
            });
        }
    }
}

/// `Py<PyAny>::clone_ref` without a `Python` token in scope.
trait CloneRefAny {
    fn clone_ref_any(&self) -> Py<PyAny>;
}

impl CloneRefAny for Py<PyAny> {
    fn clone_ref_any(&self) -> Py<PyAny> {
        Python::attach(|py| self.clone_ref(py))
    }
}

/// Every `Tool` in `tools`, as runtime tools.
pub fn runtime_tools(tools: &[Bound<'_, PyAny>]) -> PyResult<Vec<RuntimeTool>> {
    tools
        .iter()
        .map(|t| {
            t.cast::<PyTool>()
                .map(|t| t.get().runtime_tool())
                .map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err(
                        "tools must be gemini_adk.Tool objects (use the @tool decorator)",
                    )
                })
        })
        .collect()
}
