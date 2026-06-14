//! Python bindings for the gemini-rs **governed-conversation data plane**.
//!
//! This is the JSON-first surface from the FFI strategy
//! (`docs/plans/2026-06-13-json-ffi-parity-handoff.md`, Workstream B1): the
//! complete author → compile → validate → simulate → explain loop, exposed as a
//! handful of functions whose arguments and returns are JSON strings. Because
//! the `ConversationSpec`/`Scenario`/`SimStep` types are all serializable, no
//! deep object binding, async, or callback bridging is needed — this is ~5% of
//! the L2 surface for the deterministic-governance value.
//!
//! Everything here is **synchronous and model-free**: it exercises the real
//! `Conversation`/`Sim`/`Scenario` control-plane code, no live API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;

use gemini_adk_fluent_rs::conversation::{
    conversation_spec_schema, Conversation, ConversationSpec,
};
use gemini_adk_fluent_rs::flow::Enforcement;
use gemini_adk_fluent_rs::simulation::{Scenario, Sim, SimStep};

// ── Handle registries ───────────────────────────────────────────────────────
// Compiled conversations and live simulators are held Rust-side and referenced
// from Python by opaque integer handles, so no Rust object crosses the boundary.

type Compiled = gemini_adk_fluent_rs::conversation::CompiledConversation;

static COMPILED: Lazy<Mutex<HashMap<u64, Compiled>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static SIMS: Lazy<Mutex<HashMap<u64, Sim>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A shared current-thread runtime for the one async path (`SimStep::User`,
/// `Scenario::run`); the data plane is otherwise synchronous.
static RT: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime")
});

fn fresh_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn parse_mode(mode: &str) -> PyResult<Enforcement> {
    match mode {
        "enforce" | "Enforce" => Ok(Enforcement::Enforce),
        "observe" | "Observe" => Ok(Enforcement::Observe),
        other => Err(PyValueError::new_err(format!(
            "unknown enforcement mode '{other}' (expected 'enforce' or 'observe')"
        ))),
    }
}

fn parse_spec(spec_json: &str) -> PyResult<ConversationSpec> {
    serde_json::from_str(spec_json)
        .map_err(|e| PyValueError::new_err(format!("invalid ConversationSpec JSON: {e}")))
}

// ── The data-plane surface ──────────────────────────────────────────────────

/// Return the JSON Schema for a `ConversationSpec` — the authoring contract.
#[pyfunction]
fn spec_schema() -> String {
    conversation_spec_schema()
}

/// Validate a spec. Returns the diagnostic JSON: `{"valid": true}` or
/// `{"valid": false, "error": {...}}` (never raises for a *compile* error — the
/// error is data). Raises `ValueError` only if the JSON itself is malformed.
#[pyfunction]
fn validate_spec(spec_json: &str) -> PyResult<String> {
    let spec = parse_spec(spec_json)?;
    let report = match Conversation::from_spec_stubbing_resolvers(spec) {
        Ok(_) => serde_json::json!({ "valid": true }),
        Err(e) => serde_json::json!({ "valid": false, "error": e }),
    };
    Ok(report.to_string())
}

/// Compile a spec and return an opaque handle. Raises `ValueError` carrying the
/// structured compile diagnostic (as JSON) on failure.
#[pyfunction]
fn compile_spec(spec_json: &str) -> PyResult<u64> {
    let spec = parse_spec(spec_json)?;
    let convo = Conversation::from_spec_stubbing_resolvers(spec).map_err(|e| {
        let detail = serde_json::to_string(&e).unwrap_or_else(|_| e.to_string());
        PyValueError::new_err(detail)
    })?;
    let id = fresh_id();
    COMPILED.lock().unwrap().insert(id, convo);
    Ok(id)
}

/// Render a compiled conversation as a Mermaid diagram.
#[pyfunction]
fn spec_to_mermaid(handle: u64) -> PyResult<String> {
    let map = COMPILED.lock().unwrap();
    let convo = map
        .get(&handle)
        .ok_or_else(|| PyKeyError::new_err(format!("unknown conversation handle {handle}")))?;
    Ok(convo.to_mermaid())
}

/// Run a model-free `Scenario` (JSON) against a compiled conversation. Returns
/// `{"ok": true}` or `{"ok": false, "error": "[name] step i ..."}`.
#[pyfunction]
fn run_scenario(handle: u64, scenario_json: &str, mode: &str) -> PyResult<String> {
    let enforcement = parse_mode(mode)?;
    let scenario: Scenario = serde_json::from_str(scenario_json)
        .map_err(|e| PyValueError::new_err(format!("invalid Scenario JSON: {e}")))?;
    let map = COMPILED.lock().unwrap();
    let convo = map
        .get(&handle)
        .ok_or_else(|| PyKeyError::new_err(format!("unknown conversation handle {handle}")))?;
    let report = match RT.block_on(scenario.run(convo, enforcement)) {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(msg) => serde_json::json!({ "ok": false, "error": msg }),
    };
    Ok(report.to_string())
}

/// Open an interactive simulator over a compiled conversation. Returns a sim
/// handle; drive it with [`sim_step`] and inspect with [`sim_snapshot`].
#[pyfunction]
fn sim_new(handle: u64, mode: &str) -> PyResult<u64> {
    let enforcement = parse_mode(mode)?;
    let map = COMPILED.lock().unwrap();
    let convo = map
        .get(&handle)
        .ok_or_else(|| PyKeyError::new_err(format!("unknown conversation handle {handle}")))?;
    let sim = Sim::new(convo, enforcement);
    let id = fresh_id();
    SIMS.lock().unwrap().insert(id, sim);
    Ok(id)
}

/// Apply one `SimStep` (JSON) to a simulator. Mutating steps advance the sim;
/// `expect_*` steps assert and raise `ValueError` on a failed expectation.
/// Returns the post-step snapshot (same shape as [`sim_snapshot`]).
#[pyfunction]
fn sim_step(sim: u64, step_json: &str) -> PyResult<String> {
    let step: SimStep = serde_json::from_str(step_json)
        .map_err(|e| PyValueError::new_err(format!("invalid SimStep JSON: {e}")))?;
    let mut map = SIMS.lock().unwrap();
    let s = map
        .get_mut(&sim)
        .ok_or_else(|| PyKeyError::new_err(format!("unknown sim handle {sim}")))?;
    apply_step(s, &step)?;
    Ok(snapshot_json(s))
}

/// A JSON snapshot of a simulator: `{active, allowed?, denied, complete, explain}`.
#[pyfunction]
fn sim_snapshot(sim: u64) -> PyResult<String> {
    let map = SIMS.lock().unwrap();
    let s = map
        .get(&sim)
        .ok_or_else(|| PyKeyError::new_err(format!("unknown sim handle {sim}")))?;
    Ok(snapshot_json(s))
}

/// Free a conversation handle (and is a no-op if already gone).
#[pyfunction]
fn drop_conversation(handle: u64) {
    COMPILED.lock().unwrap().remove(&handle);
}

/// Free a simulator handle.
#[pyfunction]
fn drop_sim(sim: u64) {
    SIMS.lock().unwrap().remove(&sim);
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Apply a single step, mirroring `Scenario::run` so the interactive and
/// scenario paths share semantics.
fn apply_step(s: &mut Sim, step: &SimStep) -> PyResult<()> {
    let fail = |msg: String| Err(PyValueError::new_err(msg));
    match step {
        SimStep::User(text) => {
            RT.block_on(s.user(text));
        }
        SimStep::Set { key, value } => {
            s.set(key.clone(), value.clone());
        }
        SimStep::ToolOk(tool) => {
            s.tool_ok(tool);
        }
        SimStep::ScheduleTool { tool, after } => {
            s.schedule_tool(tool.clone(), *after);
        }
        SimStep::Turn => {
            s.turn();
        }
        SimStep::ExpectActive(expected) => {
            let active = s.active();
            for e in expected {
                if !active.contains(e) {
                    return fail(format!("expected active '{e}', got {active:?}"));
                }
            }
        }
        SimStep::ExpectDenied(tool) => {
            if s.allowed(tool) {
                return fail(format!("expected '{tool}' denied, but it was admitted"));
            }
        }
        SimStep::ExpectAllowed(tool) => {
            if !s.allowed(tool) {
                let why = s.denied().get(tool).cloned().unwrap_or_default();
                return fail(format!("expected '{tool}' allowed, but denied: {why}"));
            }
        }
        SimStep::ExpectSlot { key, value } => {
            let got = s.state().get_raw(key);
            if got.as_ref() != Some(value) {
                return fail(format!("expected slot '{key}' = {value}, got {got:?}"));
            }
        }
        SimStep::ExpectComplete => {
            if !s.is_complete() {
                return fail("expected conversation complete".into());
            }
        }
    }
    Ok(())
}

fn snapshot_json(s: &Sim) -> String {
    serde_json::json!({
        "active": s.active(),
        "denied": s.denied(),
        "complete": s.is_complete(),
        "explain": s.explain(),
    })
    .to_string()
}

/// The `_gemini_adk` extension module (re-exported by the `gemini_adk` package).
#[pymodule]
fn _gemini_adk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(spec_schema, m)?)?;
    m.add_function(wrap_pyfunction!(validate_spec, m)?)?;
    m.add_function(wrap_pyfunction!(compile_spec, m)?)?;
    m.add_function(wrap_pyfunction!(spec_to_mermaid, m)?)?;
    m.add_function(wrap_pyfunction!(run_scenario, m)?)?;
    m.add_function(wrap_pyfunction!(sim_new, m)?)?;
    m.add_function(wrap_pyfunction!(sim_step, m)?)?;
    m.add_function(wrap_pyfunction!(sim_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(drop_conversation, m)?)?;
    m.add_function(wrap_pyfunction!(drop_sim, m)?)?;
    Ok(())
}
