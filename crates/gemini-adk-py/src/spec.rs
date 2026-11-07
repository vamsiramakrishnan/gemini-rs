//! Session specs and conversations: load, validate, simulate and generate,
//! all offline. Everything crosses the boundary as JSON-compatible dicts.

use gemini_adk_fluent_rs::conversation::{
    conversation_spec_schema, CompiledConversation, Conversation, ConversationSpec,
};
use gemini_adk_fluent_rs::flow::Enforcement;
use gemini_adk_fluent_rs::simulation::{Scenario, Sim, SimStep};
use gemini_adk_fluent_rs::spec::SessionSpec;
use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::convert::{to_json, to_py};
use crate::errors::spec_error;
use crate::runtime::block_on;

fn parse_mode(mode: &str) -> PyResult<Enforcement> {
    match mode {
        "enforce" | "Enforce" => Ok(Enforcement::Enforce),
        "observe" | "Observe" => Ok(Enforcement::Observe),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown enforcement mode '{other}' (expected 'enforce' or 'observe')"
        ))),
    }
}

/// A session document: the same `SessionSpec` the runtime, the CLI and
/// Flow Studio read.
#[pyclass(name = "SessionSpec", module = "gemini_adk", frozen)]
pub struct PySessionSpec {
    inner: SessionSpec,
}

#[pymethods]
impl PySessionSpec {
    /// Load a document from a dict. Raises `SpecError` with `.diagnostics`
    /// when it does not parse.
    #[new]
    fn new(py: Python<'_>, document: Bound<'_, PyAny>) -> PyResult<Self> {
        let value = to_json(&document)?;
        let inner = SessionSpec::from_value(value)
            .map_err(|e| spec_error(py, format!("session spec does not parse: {e}"), json!([e])))?;
        Ok(Self { inner })
    }

    /// The JSON Schema every session spec follows.
    #[staticmethod]
    fn schema(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        to_py(py, &SessionSpec::json_schema())
    }

    /// Validate: `{"valid", "errors", "warnings", "tools", "steps", "mermaid"}`.
    fn validate<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = self.inner.validate();
        to_py(
            py,
            &json!({
                "valid": v.valid,
                "errors": v.errors,
                "warnings": v.warnings,
                "tools": v.tools,
                "steps": v.steps,
                "mermaid": v.mermaid,
            }),
        )
    }

    /// Run the document's embedded tests offline: one report per test.
    fn run_tests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let reports = self.inner.run_tests();
        let value = serde_json::to_value(&reports)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        to_py(py, &value)
    }

    /// The flow the document declares (or compiles to), as a dict.
    fn flow<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let flow = self.inner.effective_flow().map_err(|errors| {
            spec_error(py, "the document's flow does not compile", json!(errors))
        })?;
        let value = serde_json::to_value(&flow)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        to_py(py, &value)
    }

    /// The flow as a Mermaid diagram.
    fn mermaid(&self, py: Python<'_>) -> PyResult<String> {
        self.inner
            .effective_flow()
            .map(|flow| flow.to_mermaid())
            .map_err(|errors| spec_error(py, "the document's flow does not compile", json!(errors)))
    }

    /// The Rust program this document is equivalent to (`main.rs`).
    fn to_rust(&self) -> String {
        self.inner.to_rust()
    }

    /// The `Cargo.toml` for `to_rust()`.
    fn to_cargo_toml(&self) -> String {
        self.inner.to_cargo_toml()
    }

    /// The document as a dict, as the runtime serializes it.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let value = serde_json::to_value(&self.inner)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        to_py(py, &value)
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }
}

/// The JSON Schema for a `ConversationSpec`.
#[pyfunction]
pub fn conversation_schema(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let schema: Value = serde_json::from_str(&conversation_spec_schema())
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    to_py(py, &schema)
}

/// Validate a conversation spec: `{"valid": True}` or `{"valid": False,
/// "error": {...}}` (a compile error is data, not an exception).
#[pyfunction]
pub fn validate_conversation<'py>(
    py: Python<'py>,
    spec: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let spec: ConversationSpec = serde_json::from_value(to_json(&spec)?).map_err(|e| {
        spec_error(
            py,
            format!("invalid ConversationSpec: {e}"),
            json!([e.to_string()]),
        )
    })?;
    let report = match Conversation::from_spec_stubbing_resolvers(spec) {
        Ok(_) => json!({ "valid": true }),
        Err(e) => json!({ "valid": false, "error": e }),
    };
    to_py(py, &report)
}

/// A compiled, governed conversation.
#[pyclass(name = "Conversation", module = "gemini_adk", frozen)]
pub struct PyConversation {
    inner: CompiledConversation,
}

#[pymethods]
impl PyConversation {
    /// Compile a conversation spec (dict). Raises `SpecError` with
    /// `.diagnostics` when it does not compile.
    #[new]
    fn new(py: Python<'_>, spec: Bound<'_, PyAny>) -> PyResult<Self> {
        let spec: ConversationSpec = serde_json::from_value(to_json(&spec)?).map_err(|e| {
            spec_error(
                py,
                format!("invalid ConversationSpec: {e}"),
                json!([e.to_string()]),
            )
        })?;
        let inner = Conversation::from_spec_stubbing_resolvers(spec).map_err(|e| {
            let detail = serde_json::to_value(&e).unwrap_or_else(|_| json!(e.to_string()));
            spec_error(py, format!("conversation does not compile: {e}"), detail)
        })?;
        Ok(Self { inner })
    }

    /// The governed flow as a Mermaid diagram.
    fn mermaid(&self) -> String {
        self.inner.to_mermaid()
    }

    /// Run a model-free scenario (dict): `{"ok": True}` or `{"ok": False,
    /// "error": "..."}`.
    #[pyo3(signature = (scenario, *, mode="enforce"))]
    fn run_scenario<'py>(
        &self,
        py: Python<'py>,
        scenario: Bound<'py, PyAny>,
        mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let enforcement = parse_mode(mode)?;
        let scenario: Scenario = serde_json::from_value(to_json(&scenario)?).map_err(|e| {
            spec_error(py, format!("invalid Scenario: {e}"), json!([e.to_string()]))
        })?;
        let convo = &self.inner;
        let report = match block_on(py, scenario.run(convo, enforcement))? {
            Ok(()) => json!({ "ok": true }),
            Err(message) => json!({ "ok": false, "error": message }),
        };
        to_py(py, &report)
    }

    /// An interactive simulator over this conversation.
    #[pyo3(signature = (*, mode="enforce"))]
    fn sim(&self, mode: &str) -> PyResult<PySim> {
        let enforcement = parse_mode(mode)?;
        Ok(PySim {
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(Sim::new(&self.inner, enforcement))),
        })
    }
}

/// A step-by-step simulator.
#[pyclass(name = "Sim", module = "gemini_adk", frozen)]
pub struct PySim {
    /// An async mutex, locked inside the runtime future: a std lock held
    /// across `block_on` (which releases the GIL) would deadlock against a
    /// second thread that holds the GIL and waits for the lock.
    inner: std::sync::Arc<tokio::sync::Mutex<Sim>>,
}

fn snapshot(sim: &Sim) -> Value {
    json!({
        "active": sim.active(),
        "denied": sim.denied(),
        "complete": sim.is_complete(),
        "explain": sim.explain(),
    })
}

#[pymethods]
impl PySim {
    /// Apply one `SimStep` (a dict such as `{"set": {"key": ..., "value":
    /// ...}}` or the string `"turn"`) and return the snapshot. An
    /// `expect_*` step that fails raises `SpecError`.
    fn step<'py>(&self, py: Python<'py>, step: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let step: SimStep = serde_json::from_value(to_json(&step)?)
            .map_err(|e| spec_error(py, format!("invalid SimStep: {e}"), json!([e.to_string()])))?;
        let sim = self.inner.clone();
        let outcome = block_on(py, async move {
            let mut sim = sim.lock().await;
            sim.apply(&step).await.map(|()| snapshot(&sim))
        })?;
        let snap = outcome.map_err(|message| spec_error(py, message.clone(), json!([message])))?;
        to_py(py, &snap)
    }

    /// `{"active", "denied", "complete", "explain"}`.
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let sim = self.inner.clone();
        let snap = block_on(py, async move { snapshot(&*sim.lock().await) })?;
        to_py(py, &snap)
    }
}
