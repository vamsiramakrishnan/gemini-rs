# Python vs Rust on the Live wire

How much CPU, memory and audio timing does a Live voice session cost in
Python (google-genai) and in Rust (gemini-rs)? Every run here talks to a local
stand-in for the Live API, so the numbers measure the client, not the model.

## Wire bench (2026-10-09)

The thinnest comparison: N concurrent sessions that each open a Live
connection, run one turn and hold. No agent logic.

| Client | What runs |
|---|---|
| `python google-genai` | `client.aio.live.connect`, `send_client_content`, `receive` |
| `python google-genai + numpy` | The same, with numpy imported (the usual DSP dependency) |
| `rust L0` | `gemini-genai-rs`: a `SessionHandle` only, the same scope as google-genai |
| `rust L1` | `gemini-genai-rs` plus the `gemini-adk-rs` runtime (`attach_session`): event bus, state, callbacks |

The stand-in (`python/mock_server.py`, its own process) answers each turn
after 20 ms with five 40 ms frames of 24 kHz audio, a transcript and
`turnComplete`. With `--audio-secs N` it streams one 40 ms frame every 40 ms
for N seconds instead, like real-time speech.

Three measurements, raw in `results/`:

- `2026-10-09-turns.jsonl`: one text turn per session, then idle. RSS, turn
  latency (p50/p99, including the stand-in's 20 ms pause) and the time until
  every session connected.
- `2026-10-09-stream.jsonl`: ten seconds of real-time audio into every
  session. CPU seconds and CPU as a share of one core.
- `2026-10-09-jitter.jsonl`: the gaps between audio frames as the client sees
  them (they should be 40 ms), with and without 5 ms of CPU-bound application
  work per session per second (`--work-ms 5`). In Python that work holds the
  GIL, as an inline tool handler or transcript processing would.

Setup: a 4-vCPU container. Python 3.13 with google-genai 2.29.0, websockets
16.1.1 and numpy 2.5.3 runs one asyncio loop on one core. Rust runs a
multi-threaded Tokio runtime and can use all four cores, so its "% of one
core" can pass 100. These runs predate the hot-path changes of 2026-10-09
(base64-simd, single-pass `Part` decoding, tokio-tungstenite 0.29 and a
configurable `LiveEvent` capacity); L1's memory per session in particular is
from before them. The results came from an earlier revision of
`src/bin/wire_bench.rs` that printed fewer fields; what it measured is the
same. Python's `per_session_kb` counts from after its imports (about 50 MB),
Rust's from process start (about 6 MB).

### Results

One text turn per session, then idle:

| Sessions | Client | Idle RSS KB | Per session KB | All connected s | Turn p50 ms | Turn p99 ms |
|---|---|---|---|---|---|---|
| 1 | Rust L0 | 7296 | 1404.0 | 0.02 | 20.9 | 20.9 |
| 1 | Rust L1 | 9224 | 3340.0 | 0.02 | 21.2 | 21.2 |
| 1 | Python | 57112 | 6256.0 | 0.07 | 37.3 | 37.3 |
| 1 | Python + numpy | 70356 | 6284.0 | 0.08 | 36.2 | 36.2 |
| 100 | Rust L0 | 18716 | 128.3 | 0.1 | 32.1 | 43.2 |
| 100 | Rust L1 | 88840 | 829.6 | 0.08 | 23.9 | 28.4 |
| 100 | Python | 64860 | 140.5 | 0.21 | 60.1 | 79.6 |
| 100 | Python + numpy | 78048 | 139.4 | 0.24 | 69.1 | 90.4 |
| 1000 | Rust L0 | 120872 | 115.0 | 0.64 | 207.5 | 289.8 |
| 1000 | Rust L1 | 812204 | 806.3 | 0.64 | 164.5 | 356.7 |
| 1000 | Python | 125576 | 74.7 | 1.61 | 434.3 | 536.8 |
| 1000 | Python + numpy | 139836 | 75.8 | 1.9 | 452.1 | 655.0 |

Ten seconds of real-time audio per session:

| Sessions | Client | CPU s | % of one core | Idle RSS KB | Per session KB |
|---|---|---|---|---|---|
| 100 | Rust L0 | 0.84 | 8.3 | 18616 | 128.3 |
| 100 | Rust L1 | 1.85 | 18.4 | 90596 | 847.4 |
| 100 | Python | 4.38 | 43.2 | 64780 | 138.7 |
| 300 | Rust L0 | 2.11 | 20.8 | 41480 | 119.0 |
| 300 | Rust L1 | 5.5 | 53.8 | 253104 | 824.0 |
| 300 | Python | 10.16 | 98.5 | 80408 | 98.5 |

Gaps between audio frames, in ms (they should be 40), without and with 5 ms
of work per session per second:

| Work | Sessions | Client | % of one core | p50 | p99 | p99.9 | max | Over 60 ms |
|---|---|---|---|---|---|---|---|---|
| 0 | 100 | Rust L0 | 8.5 | 40.0 | 44.3 | 48.1 | 48.5 | 0.0% |
| 0 | 100 | Rust L1 | 19.0 | 40.0 | 41.6 | 42.8 | 43.8 | 0.0% |
| 0 | 100 | Python | 43.7 | 40.0 | 42.8 | 45.5 | 47.1 | 0.0% |
| 0 | 200 | Rust L0 | 17.3 | 40.0 | 43.6 | 47.6 | 50.8 | 0.0% |
| 0 | 200 | Rust L1 | 39.4 | 40.0 | 41.7 | 43.9 | 46.5 | 0.0% |
| 0 | 200 | Python | 81.6 | 40.0 | 49.5 | 55.2 | 57.0 | 0.0% |
| 5 ms/s | 100 | Rust L0 | 51.8 | 40.0 | 47.2 | 84.3 | 105.9 | 0.39% |
| 5 ms/s | 100 | Rust L1 | 61.6 | 40.0 | 44.8 | 119.7 | 142.7 | 0.49% |
| 5 ms/s | 100 | Python | 88.6 | 39.6 | 144.5 | 664.1 | 686.0 | 6.74% |
| 5 ms/s | 200 | Rust L0 | 100.7 | 40.0 | 48.8 | 195.0 | 196.9 | 0.8% |
| 5 ms/s | 200 | Rust L1 | 119.5 | 40.0 | 49.7 | 216.8 | 232.4 | 0.51% |
| 5 ms/s | 200 | Python | 98.7 | 0.1 | 1348.3 | 1457.0 | 1491.0 | 3.44% |

In the last row Python fell behind and frames arrived in bursts, so the
median gap is 0.1 ms; the ten-second stream finished 0.85 s late (turn p50
10850.7 ms).

What this shows: Python holds less memory per idle session at 1,000 sessions
(74.7 KB against Rust L0's 115.0 KB) after a higher fixed cost, but its turns
take twice as long at that scale, it needs about five times the CPU for the
same audio, and a little application work in the loop breaks its audio
timing while Rust's p99 gap stays under 50 ms.

### Running it

```bash
python3 -m venv .venv && .venv/bin/pip install -r python/requirements.txt
.venv/bin/python python/mock_server.py 8765 &

# Python
.venv/bin/python python/wire_bench.py --sessions 100 --audio-secs 10
# Rust (--layer l0 or l1)
cargo run --release --bin wire_bench -- --layer l1 --sessions 100 --audio-secs 10
```

Both print one JSON line. `--work-ms 5` adds the application work; the Rust
bench also takes `--event-capacity N` and the environment variables
`RSBENCH_WORKERS` (Tokio worker threads), `RSBENCH_SHARDS` (one
current-thread runtime per shard) and `RSBENCH_IDLE_SECS` (measure idle CPU).
