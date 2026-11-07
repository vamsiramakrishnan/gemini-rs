"""Shared fixtures: wire frames in the shape the Live API sends (see
crates/gemini-genai-rs/tests/fixtures/wire/) and a few helpers."""

from __future__ import annotations

import base64
from typing import Any

import pytest


def audio_frame(payload: bytes) -> dict[str, Any]:
    """A `serverContent.modelTurn` audio part, as the server sends it."""
    return {
        "serverContent": {
            "modelTurn": {
                "role": "model",
                "parts": [
                    {
                        "inlineData": {
                            "mimeType": "audio/pcm;rate=24000",
                            "data": base64.b64encode(payload).decode("ascii"),
                        }
                    }
                ],
            }
        }
    }


def text_frame(text: str) -> dict[str, Any]:
    return {"serverContent": {"modelTurn": {"role": "model", "parts": [{"text": text}]}}}


def turn_complete() -> dict[str, Any]:
    return {"serverContent": {"turnComplete": True}}


def interrupted() -> dict[str, Any]:
    return {"serverContent": {"interrupted": True}}


def tool_call(call_id: str, name: str, args: dict[str, Any]) -> dict[str, Any]:
    return {"toolCall": {"functionCalls": [{"id": call_id, "name": name, "args": args}]}}


def tool_call_cancellation(*ids: str) -> dict[str, Any]:
    return {"toolCallCancellation": {"ids": list(ids)}}


def go_away(time_left: str = "30s") -> dict[str, Any]:
    return {"goAway": {"timeLeft": time_left}}


@pytest.fixture
def frames():
    """The frame constructors, as one namespace."""

    class Frames:
        audio = staticmethod(audio_frame)
        text = staticmethod(text_frame)
        turn_complete = staticmethod(turn_complete)
        interrupted = staticmethod(interrupted)
        tool_call = staticmethod(tool_call)
        tool_call_cancellation = staticmethod(tool_call_cancellation)
        go_away = staticmethod(go_away)

    return Frames
