"""End-to-end smoke test for the gemini_adk data plane.

Run after `maturin develop`:  python tests/smoke.py
Exercises the full author -> schema -> validate -> compile -> mermaid ->
simulate (scenario + interactive) -> explain loop, all model-free.
"""

import gemini_adk as adk

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


def test_schema_is_contract():
    schema = adk.spec_schema()
    assert schema["title"] == "ConversationSpec", schema
    assert "stages" in schema["properties"], schema


def test_validate_good_and_bad():
    assert adk.validate_spec(SPEC) == {"valid": True}
    # an always-true commit guard is rejected with a structured diagnostic
    bad = {
        "name": "x",
        "stages": [
            {"id": "a", "commit": {"tool": "pay", "when": {"always": None}},
             "next": [{"to": "b", "when": {"called_ok": "pay"}}]},
            {"id": "b", "terminal": True},
        ],
        "require": ["b"],
    }
    report = adk.validate_spec(bad)
    assert report["valid"] is False, report
    assert report["error"]["kind"] == "compile", report
    kinds = [e["kind"] for e in report["error"]["errors"]]
    assert "unguarded_commit_tool" in kinds, report


def test_compile_raises_on_bad_spec():
    try:
        adk.Conversation({"name": "empty", "stages": []})
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError compiling an empty spec")


def test_mermaid_and_scenario():
    convo = adk.Conversation(SPEC)
    mer = convo.mermaid()
    assert "flowchart" in mer or "graph" in mer, mer

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
    convo = adk.Conversation(SPEC)
    sim = convo.sim()
    snap = sim.step({"expect_active": ["collect"]})
    assert "collect" in snap["active"], snap
    # book is gated until confirmed — a denied assertion holds
    sim.step({"expect_denied": "book"})
    sim.step({"set": {"key": "party_size", "value": 2}})
    sim.step({"set": {"key": "slot", "value": "fri"}})
    snap = sim.step("turn")
    assert "confirm" in snap["active"], snap
    # explain() is structured why-data
    assert isinstance(snap["explain"], dict), snap

    # a failed expectation raises
    try:
        sim.step({"expect_allowed": "book"})  # not confirmed yet
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError: book should be denied")


def test_barge_ins_escalate_and_timing_round_trips():
    spec = {
        "name": "support",
        "stages": [
            {
                "id": "collect",
                "collect": ["issue"],
                "repair": {
                    "reprompt_after": 10,
                    "escalate_after": 10,
                    "escalate_after_interruptions": 2,
                    "escalate_to": "handoff",
                },
                "timing": {"reprompt_after_ms": 6000, "end_of_speech_ms": 700},
            },
            {"id": "handoff", "terminal": True},
        ],
    }
    assert adk.validate_spec(spec) == {"valid": True}
    sim = adk.Conversation(spec).sim()
    sim.step("turn")
    sim.step({"expect_active": ["collect"]})
    sim.step("interrupt")
    sim.step("turn")
    sim.step("interrupt")
    sim.step({"expect_slot": {"key": "repair:collect:escalate", "value": True}})
    sim.step("turn")
    sim.step("expect_complete")


def test_a_failed_tool_counts_against_the_stage():
    sim = adk.Conversation(SPEC).sim()
    sim.step({"tool_failed": "book"})
    sim.step({"expect_active": ["collect"]})


def test_verbatim_stage_validates():
    spec = {
        "name": "disclosure",
        "stages": [
            {"id": "terms", "verbatim": "Calls may be recorded."},
            {"id": "help", "after": ["terms"], "terminal": True},
        ],
    }
    assert adk.validate_spec(spec) == {"valid": True}


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"\nAll {len(tests)} smoke tests passed.")
