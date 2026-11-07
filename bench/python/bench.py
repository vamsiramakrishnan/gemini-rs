"""In-process, model-free benchmarks of the gemini_adk binding, with the
same workload run through google-genai and the Python ADK where they offer a
comparable operation.

    python bench/python/bench.py            # prints a Markdown table
    python bench/python/bench.py --json     # machine-readable

Request latency to the model is not measured: it is network-bound and the
binding does not change it. Numbers in RESULTS.md come from this script.
"""

from __future__ import annotations

import argparse
import base64
import json
import platform
import statistics
import subprocess
import sys
import time
from typing import Any, Callable

import gemini_adk as adk

# ---------------------------------------------------------------- workloads

AUDIO_FRAME = (bytes(range(256)) * 5)[:1280]  # 40 ms of PCM16 16 kHz


def server_audio_frame(payload: bytes) -> dict[str, Any]:
    return {
        "serverContent": {
            "modelTurn": {
                "role": "model",
                "parts": [{"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": base64.b64encode(payload).decode()}}],
            }
        }
    }


def server_text_frame(text: str) -> dict[str, Any]:
    return {"serverContent": {"modelTurn": {"role": "model", "parts": [{"text": text}]}}}


CONVERSATION = {
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
}

SCENARIO = {
    "name": "happy_path",
    "steps": [
        {"expect_active": ["collect"]},
        {"set": {"key": "party_size", "value": 4}},
        {"set": {"key": "slot", "value": "tomorrow 7pm"}},
        "turn",
        {"expect_active": ["confirm"]},
        {"set": {"key": "user_confirmed", "value": True}},
        "turn",
        {"tool_ok": "book"},
        "expect_complete",
    ],
}


def timed(fn: Callable[[], Any], *, repeat: int, warmup: int = 3) -> list[float]:
    for _ in range(warmup):
        fn()
    samples = []
    for _ in range(repeat):
        t0 = time.perf_counter()
        fn()
        samples.append((time.perf_counter() - t0) * 1e3)
    return samples


def percentiles(samples: list[float]) -> dict[str, float]:
    s = sorted(samples)
    return {
        "p50": statistics.median(s),
        "p99": s[min(len(s) - 1, int(len(s) * 0.99))],
        "n": len(s),
    }


# -------------------------------------------------------------- gemini_adk


def bench_live_frames(frames_per_run: int = 1000) -> list[float]:
    """Decode `frames_per_run` server audio frames through a scripted Live
    session, from wire JSON to the Python consumer's bytes. One sample is
    the whole run; the reported figure is per frame."""
    script = [server_audio_frame(AUDIO_FRAME) for _ in range(frames_per_run)] + [{"serverContent": {"turnComplete": True}}]

    def run() -> None:
        session = adk.scripted(script, released=False, event_capacity=4096)
        events = session.events(buffer=4096)
        session.release()
        seen = 0
        for event in events:
            if event["kind"] == "audio":
                seen += 1
            elif event["kind"] == "turn_complete":
                break
        session.close()
        assert seen == frames_per_run, seen

    return [s / frames_per_run for s in timed(run, repeat=20, warmup=2)]


def bench_spec_validate() -> list[float]:
    spec = {
        "name": "bench",
        "skills": [
            {"name": "booking", "stages": CONVERSATION["stages"], "require": ["done"]},
        ],
    }
    try:
        doc = adk.SessionSpec(spec)
    except adk.SpecError:
        doc = adk.SessionSpec({"name": "bench", "flow": {"steps": [{"id": "s1", "allow": ["book"]}]}})
    return timed(lambda: doc.validate(), repeat=2000)


def bench_conversation_compile() -> list[float]:
    return timed(lambda: adk.Conversation(CONVERSATION), repeat=2000)


def bench_scenario() -> list[float]:
    convo = adk.Conversation(CONVERSATION)
    return timed(lambda: convo.run_scenario(SCENARIO), repeat=2000)


def bench_sim_step() -> list[float]:
    convo = adk.Conversation(CONVERSATION)
    sim = convo.sim()
    return timed(lambda: sim.step({"set": {"key": "party_size", "value": 2}}), repeat=5000)


def bench_tool_round_trip() -> list[float]:
    """One model turn that calls a Python tool and answers: MockLlm in,
    tool dispatch on the control lane, result back, second model call."""

    @adk.tool
    def lookup(key: str) -> str:
        """Look a key up."""
        return key.upper()

    def run() -> None:
        llm = adk.MockLlm([{"tool": "lookup", "args": {"key": "k"}}, "done"])
        assert adk.Agent("a", tools=[lookup], llm=llm).ask("go") == "done"

    return timed(run, repeat=2000)


# ------------------------------------------------------------ comparisons


def bench_genai_frames(frames_per_run: int = 1000) -> list[float] | None:
    """google-genai: parse the same server frames into its
    `LiveServerMessage` model and read the audio bytes out, the work its Live
    client does per received message (minus the socket)."""
    try:
        from google.genai import types
    except ImportError:
        return None
    raw = [json.dumps(server_audio_frame(AUDIO_FRAME)) for _ in range(frames_per_run)]

    def run() -> None:
        seen = 0
        for text in raw:
            msg = types.LiveServerMessage.model_validate(json.loads(text))
            if msg.server_content and msg.server_content.model_turn:
                for part in msg.server_content.model_turn.parts or []:
                    if part.inline_data and part.inline_data.data:
                        seen += len(part.inline_data.data) > 0
        assert seen == frames_per_run

    return [s / frames_per_run for s in timed(run, repeat=20, warmup=2)]


def bench_json_frames(frames_per_run: int = 1000) -> list[float]:
    """The floor: `json.loads` plus base64 decode, no model objects."""
    raw = [json.dumps(server_audio_frame(AUDIO_FRAME)) for _ in range(frames_per_run)]

    def run() -> None:
        for text in raw:
            msg = json.loads(text)
            base64.b64decode(msg["serverContent"]["modelTurn"]["parts"][0]["inlineData"]["data"])

    return [s / frames_per_run for s in timed(run, repeat=20, warmup=2)]


def bench_adk_tool_round_trip() -> list[float] | None:
    """Python ADK: one turn that calls a function tool and answers, over
    an in-process stand-in for the model (`BaseLlm` subclass), the same
    shape as `bench_tool_round_trip`."""
    try:
        from google.adk.agents import LlmAgent
        from google.adk.models.base_llm import BaseLlm
        from google.adk.models.llm_response import LlmResponse
        from google.adk.runners import InMemoryRunner
        from google.genai import types
    except ImportError:
        return None
    import asyncio

    class Scripted(BaseLlm):
        model: str = "scripted"

        async def generate_content_async(self, llm_request, stream=False):  # type: ignore[override]
            has_response = any(
                part.function_response
                for content in llm_request.contents
                for part in (content.parts or [])
            )
            if has_response:
                yield LlmResponse(content=types.Content(role="model", parts=[types.Part(text="done")]))
            else:
                yield LlmResponse(
                    content=types.Content(
                        role="model",
                        parts=[types.Part(function_call=types.FunctionCall(name="lookup", args={"key": "k"}))],
                    )
                )

    def lookup(key: str) -> str:
        """Look a key up."""
        return key.upper()

    async def one_turn() -> None:
        agent = LlmAgent(name="a", model=Scripted(), tools=[lookup])
        runner = InMemoryRunner(agent=agent, app_name="bench")
        session = await runner.session_service.create_session(app_name="bench", user_id="u")
        final = None
        async for event in runner.run_async(
            user_id="u",
            session_id=session.id,
            new_message=types.Content(role="user", parts=[types.Part(text="go")]),
        ):
            if event.is_final_response() and event.content and event.content.parts:
                final = event.content.parts[0].text
        assert final == "done", final

    loop = asyncio.new_event_loop()
    try:
        return timed(lambda: loop.run_until_complete(one_turn()), repeat=300)
    finally:
        loop.close()


# ------------------------------------------------------------------ report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    rows: list[tuple[str, str, list[float] | None]] = [
        ("Live audio frame, wire JSON to Python bytes (per frame)", "gemini_adk scripted session", bench_live_frames()),
        ("Live audio frame, wire JSON to Python bytes (per frame)", "google-genai LiveServerMessage", bench_genai_frames()),
        ("Live audio frame, wire JSON to Python bytes (per frame)", "json.loads + b64decode (floor)", bench_json_frames()),
        ("Tool round trip: model call, Python tool, model call", "gemini_adk + MockLlm", bench_tool_round_trip()),
        ("Tool round trip: model call, Python tool, model call", "google-adk + scripted BaseLlm", bench_adk_tool_round_trip()),
        ("SessionSpec.validate()", "gemini_adk", bench_spec_validate()),
        ("Conversation compile", "gemini_adk", bench_conversation_compile()),
        ("Scenario, 9 steps, offline", "gemini_adk", bench_scenario()),
        ("Sim.step (one state write)", "gemini_adk", bench_sim_step()),
    ]
    revision = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    versions = {"python": platform.python_version(), "gemini_adk": adk.__version__}
    for name in ("google-genai", "google-adk"):
        try:
            from importlib.metadata import version

            versions[name] = version(name)
        except Exception:
            versions[name] = "not installed"
    meta = {
        "revision": revision,
        "machine": f"{platform.system()} {platform.machine()}, {platform.processor() or 'cpu'}",
        "versions": versions,
    }
    results = [
        {"workload": w, "implementation": i, **(percentiles(s) if s else {"p50": None, "p99": None, "n": 0})}
        for w, i, s in rows
    ]
    if args.json:
        print(json.dumps({"meta": meta, "results": results}, indent=2))
        return
    print(f"revision {revision}; {meta['machine']}; " + ", ".join(f"{k} {v}" for k, v in versions.items()))
    print()
    print("| Workload | Implementation | p50 (ms) | p99 (ms) | n |")
    print("|---|---|---:|---:|---:|")
    for r in results:
        if r["n"] == 0:
            print(f"| {r['workload']} | {r['implementation']} | not run | not run | 0 |")
        else:
            print(f"| {r['workload']} | {r['implementation']} | {r['p50']:.4f} | {r['p99']:.4f} | {r['n']} |")


if __name__ == "__main__":
    sys.exit(main())
