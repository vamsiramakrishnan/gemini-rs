//! Text agents: `Agent`, `Chat`, streamed replies and `MockLlm`.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::llm::{LlmResponse as AdkLlmResponse, MockLlm as AdkMockLlm};
use gemini_adk_rs::text::{RunEvent, RunRequest, RunResult, TextAgent};
use gemini_genai_rs::prelude::Content;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::{json, Value};

use crate::convert::{opt_to_json, to_json, to_py};
use crate::errors::{config_error, to_py_err};
use crate::runtime::{block_on, future_into_py};
use crate::tools::runtime_tools;

/// A scripted model for tests: replies in order, records every request.
#[pyclass(name = "MockLlm", module = "gemini_adk", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyMockLlm {
    pub inner: AdkMockLlm,
}

#[pymethods]
impl PyMockLlm {
    /// `MockLlm(replies)`: each reply is a string (a text reply) or a dict
    /// `{"tool": name, "args": {...}}` (a tool call). With no replies the
    /// model echoes the last user message.
    #[new]
    #[pyo3(signature = (replies=None))]
    fn new(replies: Option<Vec<Bound<'_, PyAny>>>) -> PyResult<Self> {
        let Some(replies) = replies else {
            return Ok(Self {
                inner: AdkMockLlm::echo(),
            });
        };
        let mut script = Vec::with_capacity(replies.len());
        for reply in replies {
            if let Ok(text) = reply.extract::<String>() {
                script.push(AdkLlmResponse::from_text(text));
                continue;
            }
            let dict = reply.cast::<PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "a MockLlm reply is a str or {'tool': name, 'args': {...}}",
                )
            })?;
            let name: String = dict
                .get_item("tool")?
                .ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("a tool reply needs 'tool'")
                })?
                .extract()?;
            let args = match dict.get_item("args")? {
                Some(a) => to_json(&a)?,
                None => json!({}),
            };
            script.push(AdkLlmResponse::tool_call(name, args));
        }
        Ok(Self {
            inner: AdkMockLlm::script(script),
        })
    }

    /// How many requests the model has received.
    fn call_count(&self) -> usize {
        self.inner.call_count()
    }

    /// Every request so far, as JSON (`contents`, `tools`, ...).
    fn requests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for request in self.inner.requests() {
            let value = serde_json::to_value(&request)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
            list.append(to_py(py, &value)?)?;
        }
        Ok(list)
    }

    /// The last request, as JSON, or `None`.
    fn last_request<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.last_request() {
            Some(request) => {
                let value = serde_json::to_value(&request)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
                Ok(Some(to_py(py, &value)?))
            }
            None => Ok(None),
        }
    }
}

fn result_to_py<'py>(py: Python<'py>, result: &RunResult) -> PyResult<Bound<'py, PyAny>> {
    let tool_calls: Vec<Value> = result
        .tool_calls
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "args": c.args,
                "result": c.outcome.as_ref().ok(),
                "error": c.outcome.as_ref().err().map(ToString::to_string),
            })
        })
        .collect();
    to_py(
        py,
        &json!({
            "text": result.text,
            "usage": {
                "prompt_tokens": result.usage.prompt_tokens,
                "completion_tokens": result.usage.completion_tokens,
                "total_tokens": result.usage.total_tokens,
            },
            "tool_calls": tool_calls,
            "model_calls": result.model_calls,
        }),
    )
}

/// A `RunEvent` as a dict with a `kind`.
fn event_to_json(event: RunEvent) -> Value {
    match event {
        RunEvent::TextDelta(text) => json!({ "kind": "text", "text": text }),
        RunEvent::ToolCall { name, args } => {
            json!({ "kind": "tool_call", "name": name, "args": args })
        }
        RunEvent::ToolResult(record) => json!({
            "kind": "tool_result",
            "name": record.name,
            "args": record.args,
            "result": record.outcome.as_ref().ok(),
            "error": record.outcome.as_ref().err().map(ToString::to_string),
        }),
        RunEvent::Finished(result) => json!({
            "kind": "finished",
            "text": result.text,
            "usage": {
                "prompt_tokens": result.usage.prompt_tokens,
                "completion_tokens": result.usage.completion_tokens,
                "total_tokens": result.usage.total_tokens,
            },
        }),
        _ => json!({ "kind": "other" }),
    }
}

/// A text agent: a model, an instruction and tools.
#[pyclass(name = "Agent", module = "gemini_adk", frozen)]
pub struct PyAgent {
    inner: Arc<dyn TextAgent>,
    name: String,
}

#[pymethods]
impl PyAgent {
    /// `Agent(name, instruction=None, tools=(), llm=None, model=None)`.
    /// With no `llm`, the model comes from the environment
    /// (`GEMINI_API_KEY`, or the Vertex AI variables); pass a `MockLlm` for
    /// tests.
    #[new]
    #[pyo3(signature = (name, *, instruction=None, tools=None, llm=None, model=None, temperature=None))]
    fn new(
        name: String,
        instruction: Option<String>,
        tools: Option<Vec<Bound<'_, PyAny>>>,
        llm: Option<PyMockLlm>,
        model: Option<String>,
        temperature: Option<f32>,
    ) -> PyResult<Self> {
        let mut builder = AgentBuilder::new(&name);
        if let Some(instruction) = instruction {
            builder = builder.instruction(instruction);
        }
        if let Some(model) = model {
            builder = builder.model(ModelId::new(model));
        }
        if let Some(t) = temperature {
            builder = builder.temperature(t);
        }
        for tool in runtime_tools(&tools.unwrap_or_default())? {
            builder = builder.tool(tool);
        }
        let inner = match llm {
            Some(mock) => builder.build(mock.inner).map_err(config_error)?,
            None => {
                let llm = GeminiLlm::from_env().map_err(|e| to_py_err(AgentError::Llm(e)))?;
                builder.build(llm).map_err(config_error)?
            }
        };
        Ok(Self { inner, name })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// Ask one question and get the reply. With `schema` (a JSON Schema
    /// dict), the reply is parsed as JSON matching it and returned as a
    /// dict; a reply that does not parse is sent back once for correction.
    #[pyo3(signature = (prompt, *, schema=None))]
    fn ask<'py>(
        &self,
        py: Python<'py>,
        prompt: String,
        schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let schema = opt_to_json(schema.as_ref())?;
        let agent = self.inner.clone();
        let outcome = block_on(py, async move { ask(agent, prompt, schema).await })?;
        outcome_to_py(py, outcome?)
    }

    /// `await agent.ask_async(prompt, schema=None)`.
    #[pyo3(signature = (prompt, *, schema=None))]
    fn ask_async<'py>(
        &self,
        py: Python<'py>,
        prompt: String,
        schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let schema = opt_to_json(schema.as_ref())?;
        let agent = self.inner.clone();
        future_into_py(py, async move {
            let outcome = ask(agent, prompt, schema).await?;
            Python::attach(|py| outcome_to_py(py, outcome).map(Bound::unbind))
        })
    }

    /// A conversation that remembers its turns.
    fn chat(&self) -> PyChat {
        PyChat {
            agent: self.inner.clone(),
            history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn __repr__(&self) -> String {
        format!("Agent(name={:?})", self.name)
    }
}

/// Either a plain reply or a parsed JSON reply.
enum Outcome {
    Text(String),
    Json(Value),
}

fn outcome_to_py<'py>(py: Python<'py>, outcome: Outcome) -> PyResult<Bound<'py, PyAny>> {
    match outcome {
        Outcome::Text(text) => Ok(text.into_pyobject(py)?.into_any()),
        Outcome::Json(value) => to_py(py, &value),
    }
}

/// One question, with the same one-correction retry `ask_as` makes.
async fn ask(
    agent: Arc<dyn TextAgent>,
    prompt: String,
    schema: Option<Value>,
) -> PyResult<Outcome> {
    let Some(schema) = schema else {
        return agent
            .ask(prompt)
            .await
            .map(Outcome::Text)
            .map_err(to_py_err);
    };
    let state = State::new();
    let first = agent
        .run_with(
            RunRequest::new(prompt).response_schema(schema.clone()),
            &state,
        )
        .await
        .map_err(to_py_err)?;
    let reason = match serde_json::from_str::<Value>(first.text.trim()) {
        Ok(value) => return Ok(Outcome::Json(value)),
        Err(e) => e.to_string(),
    };
    let repair = RunRequest::new(format!(
        "That reply could not be read as the requested JSON ({reason}). \
         Reply again with only JSON that matches the schema."
    ))
    .history(first.messages)
    .response_schema(schema);
    let second = agent.run_with(repair, &state).await.map_err(to_py_err)?;
    serde_json::from_str::<Value>(second.text.trim())
        .map(Outcome::Json)
        .map_err(|e| {
            to_py_err(AgentError::InvalidOutput {
                expected: "JSON matching the schema",
                reason: e.to_string(),
                text: second.text,
            })
        })
}

/// A conversation: every `send` carries the turns before it.
#[pyclass(name = "Chat", module = "gemini_adk", frozen)]
pub struct PyChat {
    agent: Arc<dyn TextAgent>,
    history: Arc<Mutex<Vec<Content>>>,
}

async fn send_turn(
    agent: Arc<dyn TextAgent>,
    history: Arc<Mutex<Vec<Content>>>,
    message: String,
) -> PyResult<RunResult> {
    let snapshot = history.lock().map(|h| h.clone()).unwrap_or_default();
    let request = RunRequest::new(message).history(snapshot);
    let result = agent
        .run_with(request, &State::new())
        .await
        .map_err(to_py_err)?;
    if let Ok(mut h) = history.lock() {
        h.extend(result.messages.iter().cloned());
    }
    Ok(result)
}

#[pymethods]
impl PyChat {
    /// Send a message and get the reply; the turn joins the history when
    /// it succeeds.
    fn send(&self, py: Python<'_>, message: String) -> PyResult<String> {
        let (agent, history) = (self.agent.clone(), self.history.clone());
        block_on(py, send_turn(agent, history, message)).and_then(|r| r.map(|r| r.text))
    }

    /// `await chat.send_async(message)`.
    fn send_async<'py>(&self, py: Python<'py>, message: String) -> PyResult<Bound<'py, PyAny>> {
        let (agent, history) = (self.agent.clone(), self.history.clone());
        future_into_py(py, async move {
            send_turn(agent, history, message).await.map(|r| r.text)
        })
    }

    /// Send a message and get the full result (`text`, `usage`,
    /// `tool_calls`, `model_calls`) as a dict.
    fn send_full<'py>(&self, py: Python<'py>, message: String) -> PyResult<Bound<'py, PyAny>> {
        let (agent, history) = (self.agent.clone(), self.history.clone());
        let result = block_on(py, send_turn(agent, history, message))??;
        result_to_py(py, &result)
    }

    /// Stream the reply: an iterator (and async iterator) of event dicts
    /// (`kind`: `text`, `tool_call`, `tool_result`, `finished`).
    fn send_stream(&self, py: Python<'_>, message: String) -> PyResult<PyEventStream> {
        let (agent, history) = (self.agent.clone(), self.history.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<PyResult<Value>>(64);
        let rt = crate::runtime::runtime()?;
        rt.spawn(async move {
            let snapshot = history.lock().map(|h| h.clone()).unwrap_or_default();
            let request = RunRequest::new(message).history(snapshot);
            let state = State::new();
            let mut stream = agent.run_stream(request, state);
            while let Some(event) = stream.next().await {
                let item = match event {
                    Ok(RunEvent::Finished(result)) => {
                        if let Ok(mut h) = history.lock() {
                            h.extend(result.messages.iter().cloned());
                        }
                        Ok(event_to_json(RunEvent::Finished(result)))
                    }
                    Ok(event) => Ok(event_to_json(event)),
                    Err(e) => Err(to_py_err(e)),
                };
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        let _ = py;
        Ok(PyEventStream {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
        })
    }

    /// Every turn so far, oldest first, as `{"role": ..., "text": ...}`.
    fn history<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        let history = self.history.lock().map(|h| h.clone()).unwrap_or_default();
        for content in history {
            let value = serde_json::to_value(&content)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
            list.append(to_py(py, &value)?)?;
        }
        Ok(list)
    }
}

/// A stream of event dicts, usable as `for` and `async for`.
#[pyclass(name = "EventStream", module = "gemini_adk", frozen)]
pub struct PyEventStream {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<PyResult<Value>>>>,
}

#[pymethods]
impl PyEventStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let rx = self.rx.clone();
        let next = block_on(py, async move { rx.lock().await.recv().await })?;
        match next {
            Some(Ok(value)) => Ok(Some(to_py(py, &value)?)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        future_into_py(py, async move {
            match rx.lock().await.recv().await {
                Some(Ok(value)) => Python::attach(|py| to_py(py, &value).map(Bound::unbind)),
                Some(Err(e)) => Err(e),
                None => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }
}
