"""Python access to the gemini-rs governed-conversation data plane.

Author, compile, validate, and **deterministically** simulate voice-agent
conversation specs — the same governed control-plane code the Rust runtime
executes, with no live model in the loop. Everything crosses the boundary as
JSON, so this module is a thin, fast surface over the Rust core.

Example::

    import json, gemini_adk as adk

    spec = {
        "name": "booking",
        "stages": [
            {"id": "collect", "collect": ["party_size"],
             "next": [{"to": "done", "when": {"captured": ["party_size"]}}]},
            {"id": "done", "terminal": True},
        ],
        "require": ["done"],
    }

    convo = adk.Conversation(spec)            # compiles (raises on invalid spec)
    print(convo.mermaid())                    # render the governed flow
    result = convo.run_scenario({             # deterministic, model-free test
        "name": "happy",
        "steps": [
            {"expect_active": ["collect"]},
            {"set": {"key": "party_size", "value": 4}},
            "turn",
            "expect_complete",
        ],
    })
    assert result["ok"], result
"""

from __future__ import annotations

import json as _json
from typing import Any

from . import _gemini_adk as _ext

__all__ = ["spec_schema", "validate_spec", "Conversation", "Sim"]


def spec_schema() -> dict[str, Any]:
    """The JSON Schema for a ConversationSpec (the authoring contract)."""
    return _json.loads(_ext.spec_schema())


def validate_spec(spec: dict[str, Any]) -> dict[str, Any]:
    """Validate a spec dict. Returns ``{"valid": True}`` or
    ``{"valid": False, "error": {...}}`` — the error is structured data."""
    return _json.loads(_ext.validate_spec(_json.dumps(spec)))


class Conversation:
    """A compiled, governed conversation. Constructing one compiles the spec and
    raises ``ValueError`` (carrying the structured diagnostic) if it is invalid.
    """

    def __init__(self, spec: dict[str, Any]):
        self._handle = _ext.compile_spec(_json.dumps(spec))

    def mermaid(self) -> str:
        """Render the governed flow as a Mermaid diagram."""
        return _ext.spec_to_mermaid(self._handle)

    def run_scenario(self, scenario: dict[str, Any], mode: str = "enforce") -> dict[str, Any]:
        """Run a model-free Scenario. Returns ``{"ok": True}`` or
        ``{"ok": False, "error": "..."}``."""
        return _json.loads(_ext.run_scenario(self._handle, _json.dumps(scenario), mode))

    def sim(self, mode: str = "enforce") -> "Sim":
        """Open an interactive deterministic simulator over this conversation."""
        return Sim(_ext.sim_new(self._handle, mode))

    def __del__(self):  # best-effort handle cleanup
        try:
            _ext.drop_conversation(self._handle)
        except Exception:
            pass


class Sim:
    """An interactive, deterministic simulator. Drive it with :meth:`step`;
    ``expect_*`` steps raise ``ValueError`` on a failed assertion."""

    def __init__(self, handle: int):
        self._handle = handle

    def step(self, step: Any) -> dict[str, Any]:
        """Apply one SimStep (e.g. ``"turn"``, ``{"set": {...}}``,
        ``{"expect_active": [...]}``). Returns the post-step snapshot."""
        return _json.loads(_ext.sim_step(self._handle, _json.dumps(step)))

    def snapshot(self) -> dict[str, Any]:
        """The current ``{active, denied, complete, explain}`` snapshot."""
        return _json.loads(_ext.sim_snapshot(self._handle))

    def __del__(self):
        try:
            _ext.drop_sim(self._handle)
        except Exception:
            pass
