"""§5: one exception hierarchy, every `AgentError` variant mapped."""

import pytest

import gemini_adk as adk
from gemini_adk import _gemini_adk as ext

EXPECTED = {
    "Session": adk.SessionError,
    "Tool.ExecutionFailed": adk.ToolExecutionError,
    "Tool.NotFound": adk.ToolNotFoundError,
    "Tool.InvalidArgs": adk.ToolArgumentError,
    "Tool.Cancelled": adk.ToolExecutionError,
    "Tool.Declined": adk.ToolExecutionError,
    "Tool.Timeout": adk.ToolExecutionError,
    "Tool.Other": adk.ToolExecutionError,
    "Llm": adk.LlmError,
    "State": adk.StateError,
    "InvalidOutput": adk.InvalidOutputError,
    "UnknownAgent": adk.TransferError,
    "TransferRequested": adk.TransferError,
    "TransferFailed": adk.TransferError,
    "SessionClosed": adk.SessionClosedError,
    "Timeout": adk.TimeoutError,
    "Config": adk.ConfigError,
    "Other": adk.GeminiAdkError,
}


def test_every_variant_maps():
    variants = ext._variants()
    assert set(variants) == set(EXPECTED), "the test table and the Rust hook list differ"
    for variant in variants:
        with pytest.raises(EXPECTED[variant]) as info:
            ext._raise(variant)
        assert type(info.value) is EXPECTED[variant], variant
        assert isinstance(info.value, adk.GeminiAdkError)
        assert str(info.value)


def test_timeout_is_both_hierarchies():
    with pytest.raises(TimeoutError):
        ext._raise("Timeout")
    with pytest.raises(adk.GeminiAdkError):
        ext._raise("Timeout")


def test_tool_errors_share_a_base():
    for variant in ("Tool.NotFound", "Tool.InvalidArgs", "Tool.ExecutionFailed"):
        with pytest.raises(adk.ToolError):
            ext._raise(variant)


def test_spec_error_carries_diagnostics():
    with pytest.raises(adk.SpecError) as info:
        adk.Conversation({"name": "empty", "stages": []})
    assert info.value.diagnostics is not None


def test_unknown_variant_is_a_value_error():
    with pytest.raises(ValueError):
        ext._raise("NoSuchVariant")
