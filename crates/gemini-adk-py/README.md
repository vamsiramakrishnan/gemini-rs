# gemini-adk (Python)

Python access to the **gemini-rs governed-conversation data plane**: author,
compile, validate, and **deterministically** simulate voice-agent conversation
specs — running the *same* governed control-plane code the Rust runtime
executes, with no live model in the loop.

This is the JSON-first surface: everything crosses the boundary as JSON, so it
is a thin, fast wrapper over the Rust core (no async, no callbacks, no deep
object binding). It is ~5% of the full SDK surface and delivers the
deterministic-governance value — the part nobody else in the field has.

## Install

```bash
pip install gemini-adk
```

(Built from the Rust crate with [maturin](https://www.maturin.rs/); requires
Python ≥ 3.10.)

## Use

```python
import gemini_adk as adk

spec = {
    "name": "booking",
    "stages": [
        {"id": "collect", "collect": ["party_size"],
         "next": [{"to": "done", "when": {"captured": ["party_size"]}}]},
        {"id": "done", "terminal": True},
    ],
    "require": ["done"],
}

# The JSON Schema is the authoring contract (target it from an LLM/skill/form):
schema = adk.spec_schema()

# Validate returns structured diagnostics (never throws for a compile error):
report = adk.validate_spec(spec)         # {"valid": True} or {"valid": False, "error": {...}}

# Compile (raises ValueError with the structured diagnostic on an invalid spec):
convo = adk.Conversation(spec)
print(convo.mermaid())                   # render the governed flow

# Deterministic, model-free test — the artifact you test is the one that runs:
result = convo.run_scenario({
    "name": "happy",
    "steps": [
        {"expect_active": ["collect"]},
        {"set": {"key": "party_size", "value": 4}},
        "turn",
        "expect_complete",
    ],
})
assert result["ok"], result

# Or drive it interactively and inspect why a tool is blocked:
sim = convo.sim()
snap = sim.step({"set": {"key": "party_size", "value": 2}})
snap = sim.step("turn")                  # {active, denied, complete, explain}
```

## Surface

| Function / method | What it does |
|---|---|
| `adk.spec_schema()` | JSON Schema for a `ConversationSpec` |
| `adk.validate_spec(spec)` | `{"valid": ...}` with structured errors |
| `adk.Conversation(spec)` | compile (raises on invalid spec) |
| `.mermaid()` | render the governed flow |
| `.run_scenario(scenario, mode="enforce")` | deterministic model-free test |
| `.sim(mode="enforce")` | open an interactive simulator |
| `Sim.step(step)` / `.snapshot()` | drive / inspect a simulator |

The vocabulary of `spec`, `scenario`, and `step` is exactly the serializable
Rust `ConversationSpec` / `Scenario` / `SimStep` — see `adk.spec_schema()` and
the gemini-rs docs.

## Building from source

```bash
cd crates/gemini-adk-py
python -m venv .venv && . .venv/bin/activate
maturin develop          # build + install into the venv
python tests/smoke.py    # end-to-end smoke test
```
