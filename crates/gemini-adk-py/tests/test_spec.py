"""Session specs and conversations: load, validate, simulate, generate."""

import json
import pathlib

import pytest

import gemini_adk as adk

GALLERY = pathlib.Path(__file__).resolve().parents[3] / "apps" / "gemini-adk-web-rs" / "static" / "examples" / "flows"

SPEC = {
    "name": "booking",
    "stages": [
        {
            "id": "collect",
            "say": "Help the user book a table.",
            "collect": ["party_size", "slot"],
            "next": [{"to": "confirm", "when": {"captured": ["party_size", "slot"]}}],
        },
        {
            "id": "confirm",
            "allow": ["book"],
            "commit": {"tool": "book", "when": {"is_true": "user_confirmed"}},
            "next": [{"to": "done", "when": {"called_ok": "book"}}],
        },
        {"id": "done", "terminal": True},
    ],
    "require": ["done"],
    "policies": [{"kind": "redact", "keys": ["card_number"]}],
}


def gallery_documents():
    docs = []
    for sub in ("", "reference"):
        d = GALLERY / sub
        if not d.is_dir():
            continue
        for path in sorted(d.glob("*.json")):
            if path.name == "index.json":
                continue
            docs.append(path)
    return docs


def test_session_spec_schema_is_a_schema():
    schema = adk.SessionSpec.schema()
    assert "properties" in schema or "$defs" in schema or "oneOf" in schema


@pytest.mark.parametrize("path", gallery_documents(), ids=lambda p: p.stem)
def test_gallery_documents_load_validate_and_generate(path):
    document = json.loads(path.read_text())
    spec = adk.SessionSpec(document)
    assert spec.name
    report = spec.validate()
    assert report["valid"], report["errors"]
    assert "steps" in spec.flow()
    assert "flowchart" in spec.mermaid() or "graph" in spec.mermaid()
    rust = spec.to_rust()
    assert "fn main" in rust and "Live::builder()" in rust
    assert "[package]" in spec.to_cargo_toml()
    round_trip = adk.SessionSpec(spec.to_dict())
    assert round_trip.to_dict() == spec.to_dict()
    for test in spec.run_tests():
        assert test.get("passed", test.get("ok", True)), test


def test_bad_document_is_a_spec_error():
    with pytest.raises(adk.SpecError) as info:
        adk.SessionSpec({"name": 1})
    assert info.value.diagnostics


def test_conversation_schema_is_contract():
    schema = adk.conversation_schema()
    assert schema["title"] == "ConversationSpec"
    assert "stages" in schema["properties"]


def test_validate_conversation_good_and_bad():
    assert adk.validate_conversation(SPEC) == {"valid": True}
    bad = {
        "name": "x",
        "stages": [
            {"id": "a", "commit": {"tool": "pay", "when": {"always": None}}, "next": [{"to": "b", "when": {"called_ok": "pay"}}]},
            {"id": "b", "terminal": True},
        ],
        "require": ["b"],
    }
    report = adk.validate_conversation(bad)
    assert report["valid"] is False
    assert report["error"]["kind"] == "compile"
    assert "unguarded_commit_tool" in [e["kind"] for e in report["error"]["errors"]]


def test_mermaid_and_scenario():
    convo = adk.Conversation(SPEC)
    assert "flowchart" in convo.mermaid() or "graph" in convo.mermaid()
    result = convo.run_scenario({
        "name": "happy_path",
        "steps": [
            {"expect_active": ["collect"]},
            {"expect_denied": "book"},
            {"set": {"key": "party_size", "value": 4}},
            {"set": {"key": "slot", "value": "tomorrow 7pm"}},
            "turn",
            {"expect_active": ["confirm"]},
            {"set": {"key": "user_confirmed", "value": True}},
            "turn",
            {"expect_allowed": "book"},
            {"tool_ok": "book"},
            "expect_complete",
        ],
    })
    assert result["ok"], result


def test_interactive_sim_and_explain():
    sim = adk.Conversation(SPEC).sim()
    snap = sim.step({"expect_active": ["collect"]})
    assert "collect" in snap["active"]
    sim.step({"expect_denied": "book"})
    sim.step({"set": {"key": "party_size", "value": 2}})
    sim.step({"set": {"key": "slot", "value": "fri"}})
    snap = sim.step("turn")
    assert "confirm" in snap["active"]
    assert isinstance(snap["explain"], dict)
    with pytest.raises(adk.SpecError):
        sim.step({"expect_allowed": "book"})
    assert sim.snapshot()["complete"] is False


def test_observe_mode_and_bad_mode():
    convo = adk.Conversation(SPEC)
    assert convo.sim(mode="observe").snapshot()["complete"] is False
    with pytest.raises(ValueError):
        convo.sim(mode="loud")


def test_sim_step_releases_gil_over_a_long_scenario():
    import threading

    convo = adk.Conversation(SPEC)
    steps = [{"set": {"key": "party_size", "value": 2}}, "turn"] * 1000
    ticks = 0
    stop = threading.Event()

    def spin():
        nonlocal ticks
        while not stop.is_set():
            ticks += 1

    t = threading.Thread(target=spin)
    t.start()
    try:
        assert convo.run_scenario({"name": "long", "steps": steps})["ok"]
    finally:
        stop.set()
        t.join()
    assert ticks > 0


def test_sim_is_safe_to_share_between_threads():
    """`step` releases the GIL inside the runtime while `snapshot` on another
    thread holds it: no lock is held across that boundary, so neither
    deadlocks the interpreter."""
    import threading

    sim = adk.Conversation(SPEC).sim()
    stop = threading.Event()
    errors = []

    def stepper():
        try:
            for _ in range(300):
                sim.step({"set": {"key": "party_size", "value": 2}})
        except Exception as e:  # pragma: no cover - reported below
            errors.append(e)
        finally:
            stop.set()

    def reader():
        while not stop.is_set():
            sim.snapshot()

    threads = [threading.Thread(target=stepper), threading.Thread(target=reader)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=30)
    assert not any(t.is_alive() for t in threads), "a thread hung"
    assert not errors

