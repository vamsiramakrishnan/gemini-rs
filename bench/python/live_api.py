#!/usr/bin/env python3
"""Python binding vs native Rust, against the real Gemini API.

Both sides run the same SDK code: the Python side through `gemini_adk`
(this repository's PyO3 binding), the native side through
`crates/gemini-adk-py/examples/native_bench.rs`, which makes the calls the
binding makes, on a Tokio runtime configured like the binding's. Each side
times its own call at its own API boundary (Python: around the `gemini_adk`
call; Rust: around the SDK call), so the native process's start-up and the
pipe between the two are never inside a measurement.

The API's own latency swings by hundreds of milliseconds from one request to
the next, far more than either client costs. So samples are paired: for each
sample the two sides run back to back, in random order, and the comparison
is the distribution of per-pair differences (Python minus Rust), with a
bootstrap confidence interval on its median. An offline workload (MockLlm,
no network) measures the binding's own cost directly.

Usage (from the repository root, with GEMINI_API_KEY set):

    cargo build --release --example native_bench \\
        --manifest-path crates/gemini-adk-py/Cargo.toml
    (cd crates/gemini-adk-py && maturin develop --release)
    python bench/python/live_api.py --out bench/python/live_api.json
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import random
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Callable

import gemini_adk as g

MODEL = "gemini-3.5-flash-lite"
INSTRUCTION = "You are a concise assistant."
TOOL_INSTRUCTION = "You are a concise assistant. Use tools when they help."
PROMPT_SHORT = "Reply with exactly the word: OK"
PROMPT_LONG = "In about 120 words, explain what a borrow checker does."
PROMPT_TOOL = "What is the weather in Paris right now? Use the tool, then answer in one sentence."
LIVE_INSTRUCTION = "You are a voice assistant. Answer in one short sentence."
LIVE_TEXT = "Say exactly: The quick brown fox jumps over the lazy dog."

TOOL_CALLS = 0


@g.tool
def get_weather(city: str) -> dict[str, Any]:
    """Current weather for a city."""
    global TOOL_CALLS
    TOOL_CALLS += 1
    return {"city": city, "temp_c": 21, "conditions": "sunny"}


# ── The two sides ───────────────────────────────────────────────────────────


class Native:
    """The Rust process: one JSON command in, one JSON result out."""

    def __init__(self, binary: Path) -> None:
        self.proc = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def call(self, **command: Any) -> dict[str, Any]:
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write(json.dumps(command) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("native bench exited")
        reply: dict[str, Any] = json.loads(line)
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        self.proc.wait(timeout=30)


class Python:
    def __init__(self, temperature: float) -> None:
        self.plain = g.Agent("bench", instruction=INSTRUCTION, model=MODEL, temperature=temperature)
        self.tooled = g.Agent(
            "bench_tools",
            instruction=TOOL_INSTRUCTION,
            model=MODEL,
            temperature=temperature,
            tools=[get_weather],
        )

    def ask(self, prompt: str) -> dict[str, Any]:
        t = time.perf_counter()
        text = self.plain.ask(prompt)
        return {"ms": (time.perf_counter() - t) * 1e3, "chars": len(text)}

    def tool(self, prompt: str) -> dict[str, Any]:
        before = TOOL_CALLS
        t = time.perf_counter()
        text = self.tooled.ask(prompt)
        ms = (time.perf_counter() - t) * 1e3
        return {"ms": ms, "tool_calls": TOOL_CALLS - before, "chars": len(text)}

    def live(self, instruction: str, text: str) -> dict[str, Any]:
        t = time.perf_counter()
        session = g.connect(instruction=instruction)
        connect_ms = (time.perf_counter() - t) * 1e3
        try:
            events = session.events()
            sent = time.perf_counter()
            session.send_text(text)
            first_audio_ms = None
            audio_bytes = chunks = 0
            turn_ms = None
            for event in events:
                kind = event["kind"]
                if kind == "audio":
                    if first_audio_ms is None:
                        first_audio_ms = (time.perf_counter() - sent) * 1e3
                    audio_bytes += len(event["data"])
                    chunks += 1
                elif kind == "turn_complete":
                    turn_ms = (time.perf_counter() - sent) * 1e3
                    break
                elif kind == "disconnected":
                    raise RuntimeError(f"disconnected: {event.get('reason')}")
                if time.perf_counter() - sent > 60:
                    raise RuntimeError("no turn_complete within 60 s")
            if turn_ms is None:
                raise RuntimeError("event stream ended before turn_complete")
        finally:
            session.close()
        return {
            "connect_ms": connect_ms,
            "first_audio_ms": first_audio_ms,
            "turn_ms": turn_ms,
            "audio_bytes": audio_bytes,
            "chunks": chunks,
        }


def python_mock(n: int) -> list[float]:
    agent = g.Agent("mock", instruction=INSTRUCTION, llm=g.MockLlm())
    out = []
    for i in range(n):
        t = time.perf_counter()
        agent.ask(f"ping {i}")
        out.append((time.perf_counter() - t) * 1e6)
    return out


# ── Statistics ──────────────────────────────────────────────────────────────


def pct(xs: list[float], q: float) -> float:
    s = sorted(xs)
    if not s:
        return float("nan")
    k = (len(s) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def bootstrap_median_ci(xs: list[float], rounds: int = 10_000, seed: int = 7) -> tuple[float, float]:
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(xs, k=len(xs))) for _ in range(rounds))
    return meds[int(0.025 * rounds)], meds[int(0.975 * rounds) - 1]


def summarize(pairs: list[tuple[dict[str, Any], dict[str, Any]]], metric: str) -> dict[str, Any]:
    both = [(p[metric], r[metric]) for p, r in pairs if p.get(metric) is not None and r.get(metric) is not None]
    py = [a for a, _ in both]
    rs = [b for _, b in both]
    diff = [a - b for a, b in both]
    lo, hi = bootstrap_median_ci(diff)
    return {
        "n": len(both),
        "python": {"p50": pct(py, 0.5), "p90": pct(py, 0.9), "mean": statistics.fmean(py)},
        "rust": {"p50": pct(rs, 0.5), "p90": pct(rs, 0.9), "mean": statistics.fmean(rs)},
        "diff_median": statistics.median(diff),
        "diff_ci95": [lo, hi],
        "diff_mean": statistics.fmean(diff),
        "python_faster_in": sum(d < 0 for d in diff),
    }


# ── Driver ──────────────────────────────────────────────────────────────────


def with_retry(label: str, run: Callable[[], dict[str, Any]], attempts: int = 4) -> dict[str, Any]:
    for attempt in range(attempts):
        try:
            return run()
        except Exception as e:  # noqa: BLE001 - logged, retried, then raised
            if attempt == attempts - 1:
                raise
            wait = 5 * 2**attempt
            print(f"  {label}: {e!s:.120} (retry in {wait}s)", file=sys.stderr)
            time.sleep(wait)
    raise AssertionError("unreachable")


def paired(
    name: str,
    n: int,
    warmup: int,
    py: Callable[[], dict[str, Any]],
    rs: Callable[[], dict[str, Any]],
    pause: float,
) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    for _ in range(warmup):
        with_retry(f"{name} warm-up py", py)
        with_retry(f"{name} warm-up rs", rs)
    pairs = []
    for i in range(n):
        order = ["py", "rs"]
        random.shuffle(order)
        got: dict[str, dict[str, Any]] = {}
        for side in order:
            got[side] = with_retry(f"{name} #{i} {side}", py if side == "py" else rs)
            time.sleep(pause)
        pairs.append((got["py"], got["rs"]))
        print(f"  {name} {i + 1}/{n}", file=sys.stderr, end="\r")
    print(file=sys.stderr)
    return pairs


def main() -> None:
    here = Path(__file__).resolve().parents[2]
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", type=Path, default=here / "crates/gemini-adk-py/target/release/examples/native_bench")
    ap.add_argument("--short", type=int, default=40, help="pairs for the short reply")
    ap.add_argument("--long", type=int, default=25, help="pairs for the ~120-word reply")
    ap.add_argument("--tool", type=int, default=25, help="pairs for the tool round trip")
    ap.add_argument("--live", type=int, default=15, help="pairs of Live sessions")
    ap.add_argument("--mock", type=int, default=2000, help="offline asks per side per round")
    ap.add_argument("--pause", type=float, default=0.3, help="seconds between requests")
    ap.add_argument("--temperature", type=float, default=0.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()
    if not os.environ.get("GEMINI_API_KEY"):
        sys.exit("GEMINI_API_KEY is not set")
    random.seed(args.seed)

    native = Native(args.binary)
    schema = get_weather.parameters
    native.call(op="init", model=MODEL, temperature=args.temperature, tool_schema=schema)
    py = Python(args.temperature)
    results: dict[str, Any] = {}
    raw: dict[str, Any] = {}

    try:
        # Offline first: the binding's own cost, no network in the way.
        py_us: list[float] = []
        rs_us: list[float] = []
        for _ in range(3):
            py_us += python_mock(args.mock)
            rs_us += native.call(op="mock", n=args.mock)["us"]
        results["mock_ask_us"] = {
            "n": len(py_us),
            "python": {"p50": pct(py_us, 0.5), "p99": pct(py_us, 0.99)},
            "rust": {"p50": pct(rs_us, 0.5), "p99": pct(rs_us, 0.99)},
            "diff_p50": pct(py_us, 0.5) - pct(rs_us, 0.5),
        }

        if args.short:
            p = paired("ask short", args.short, 3, lambda: py.ask(PROMPT_SHORT),
                       lambda: native.call(op="ask", prompt=PROMPT_SHORT), args.pause)
            raw["ask_short"] = p
            results["ask_short_ms"] = summarize(p, "ms")
        if args.long:
            p = paired("ask long", args.long, 2, lambda: py.ask(PROMPT_LONG),
                       lambda: native.call(op="ask", prompt=PROMPT_LONG), args.pause)
            raw["ask_long"] = p
            results["ask_long_ms"] = summarize(p, "ms")
            results["ask_long_chars"] = summarize(p, "chars")
        if args.tool:
            p = paired("tool", args.tool, 2, lambda: py.tool(PROMPT_TOOL),
                       lambda: native.call(op="tool", prompt=PROMPT_TOOL), args.pause)
            raw["tool"] = p
            results["tool_ms"] = summarize(p, "ms")
            results["tool_calls"] = {
                "python": sum(a["tool_calls"] for a, _ in p),
                "rust": sum(b["tool_calls"] for _, b in p),
                "pairs": len(p),
            }
        if args.live:
            p = paired("live", args.live, 1, lambda: py.live(LIVE_INSTRUCTION, LIVE_TEXT),
                       lambda: native.call(op="live", instruction=LIVE_INSTRUCTION, text=LIVE_TEXT),
                       max(args.pause, 1.0))
            raw["live"] = p
            for metric in ("connect_ms", "first_audio_ms", "turn_ms", "chunks"):
                results[f"live_{metric}"] = summarize(p, metric)
    finally:
        native.close()

    report = {
        "env": {
            "python": platform.python_version(),
            "platform": platform.platform(),
            "cpus": os.cpu_count(),
            "gemini_adk": g.__version__,
            "text_model": MODEL,
            "live_model": "SDK default (connect_from_env)",
            "temperature": args.temperature,
            "date": time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime()),
        },
        "results": results,
        "raw": raw,
    }
    if args.out:
        args.out.write_text(json.dumps(report, indent=2))
    json.dump({"env": report["env"], "results": results}, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
