# Python vs Rust on the Live wire

How much CPU, memory and audio timing does a Live voice session cost in
Python (google-genai) and in Rust (gemini-rs)? Every run here talks to a local
stand-in for the Live API, so the numbers measure the client, not the model.

Two benchmarks:

- [Trattoria calls](#trattoria-calls-2026-10-11): the same booking agent in
  both languages, taking whole phone calls. This is the one to quote.
- [Wire bench](#wire-bench-2026-10-09): sessions with no agent logic, one
  turn each.

## Trattoria calls (2026-10-11)

### What a call is

`trattoria_mock` (Rust, its own process) stands in for Gemini Live and for
`generateContent`. Each connection plays one booking call, about 41 seconds
long:

1. The client sets up and sends the greeting. The model greets the caller,
   saying the stage's verbatim line ("This call may be recorded").
2. The caller asks for a table for four on Wednesday at seven, under Rossi,
   with input transcription as the words land.
3. The model says it will check and calls `check_availability`, then offers
   two times.
4. The caller picks one, and the model reads the booking back.
5. The caller says "Yes, please book it" and the model calls `book_table`.
   The client's commit gate admits it only once extraction has heard that
   yes.
6. The model gives the reference, and the caller hangs up.

The caller's microphone streams for the whole call: 20 ms frames of 16 kHz
audio, as from a phone line. The stand-in hears the caller only through those
frames, so a client that falls behind on its mic stream stretches its own
calls. Model audio comes back in 40 ms frames of 24 kHz audio, in real time.
The model answers 500 ms after the caller stops. The extraction model answers
400 ms after each request.

### The two agents

- `src/bin/trattoria.rs` is the spec-built agent: `trattoria.json` from
  `crates/gemini-adk-fluent-rs/tests/fixtures/live`, applied with
  `SessionSpec::apply` and connected over a real WebSocket. Its extraction
  goes through `GeminiLlm`, with `base_url` pointing at the stand-in.
- `python/trattoria.py` is the same agent written on google-genai. It has:
  - the stages and the stage text;
  - the tool gate and the commit gate, with the second-chance extraction;
  - both extractors, with the same prompts, schema, thinking budget and
    promotion policies;
  - the verbatim check, repair, and the handoff overlay;
  - `contextUpdate` steering, sent as raw JSON because google-genai has no
    API for it;
  - floor holding: silence on the mic while the model speaks the verbatim
    line.

With the stand-in's `--log`, one call from each sends the same messages in
the same order, and the same extraction requests.

### What is measured

- **Cost per call.** Client CPU, divided by the total length of the calls
  that finished: CPU milliseconds per second of call.
- **Caller audio lateness.** Every mic frame carries the time its 20 ms clock
  captured it. The stand-in measures how late the frame reached it. This is
  the delay a model would hear.
- **Model audio lateness.** Every model frame carries the time the stand-in
  sent it. The client measures how late the frame reached its audio callback.
  This is the delay a caller would hear, before any playout.
- **Steering latency.** From the end of the turn that collects the booking
  details to the `contextUpdate` that declares `book_table`. That covers both
  extractions and the stage change.
- **Outcomes.** Calls booked, and how long each call took (41.4 s when
  nothing is late).
- **Memory.** Peak RSS.

Each run uses a fresh stand-in on cores 2–3 and one client on core 0
(`taskset`). The Rust client runs one Tokio worker; Python runs one asyncio
loop, or uvloop. Each run lasts 90 s, with the lines' first calls spread over
40 s and every line taking calls back to back. Python 3.13 with google-genai
2.29.0, websockets 16.1.1 and uvloop 0.23.0, on a 4-vCPU container.

### Results

Raw runs: `results/2026-10-11-trattoria.jsonl`, one record per run, holding
everything the client and the stand-in measured. The tables come from
`python/summarize_trattoria.py`.

Cost and memory. "CPU, % of one core" is averaged over the whole run, ramp-up
and tail included. "KB per line" is peak RSS above the process's start for
Rust, and above its post-import size for Python, divided by the line count:

| Client | Lines | Booked | CPU, % of one core | CPU ms per call-second | Peak RSS MB | KB per line |
|---|---|---|---|---|---|---|
| Rust | 25 | 55/55 | 4.5 | 2.56 | 28 | 898 |
| Rust | 50 | 109/109 | 7.1 | 2.07 | 39 | 662 |
| Rust | 100 | 218/218 | 11.4 | 1.65 | 59 | 539 |
| Rust | 200 | 436/436 | 19.5 | 1.42 | 100 | 478 |
| Rust | 400 | 872/872 | 34.6 | 1.26 | 182 | 449 |
| Rust | 600 | 1307/1307 | 49.1 | 1.19 | 263 | 438 |
| Rust | 800 | 1743/1743 | 57.9 | 1.05 | 346 | 436 |
| Rust | 1000 | 2174/2174 | 71.7 | 1.04 | 448 | 452 |
| Rust | 1500 | 3187/3187 | 80.2 | 0.79 | 715 | 484 |
| Python | 25 | 55/55 | 21.6 | 12.4 | 64 | 549 |
| Python | 50 | 109/109 | 39 | 11.29 | 67 | 345 |
| Python | 100 | 218/218 | 64.7 | 9.41 | 75 | 254 |
| Python | 125 | 271/271 | 74.2 | 8.6 | 77 | 221 |
| Python | 150 | 292/292 | 77 | 7.53 | 85 | 237 |
| Python + uvloop | 25 | 55/55 | 19.6 | 11.24 | 66 | 641 |
| Python + uvloop | 50 | 109/109 | 36.6 | 10.6 | 69 | 392 |
| Python + uvloop | 100 | 218/218 | 63.8 | 9.26 | 77 | 269 |
| Python + uvloop | 125 | 258/258 | 71.2 | 8.49 | 82 | 261 |

Timing. "Mic late" is how late the caller's audio reached the stand-in.
"Model audio late" is how late the model's audio reached the client's
callback. "Steering" includes the 400 ms the extraction model takes:

| Client | Lines | Mic late p50 / p99 / p99.9 ms | Model audio late p50 / p99 / p99.9 ms | Steering p50 / p99 ms | Call p50 / p99 s |
|---|---|---|---|---|---|
| Rust | 25 | 1.2 / 2.3 / 22.7 | 0.1 / 0.6 / 4.5 | 402.3 / 403.2 | 41.4 / 41.5 |
| Rust | 50 | 1.2 / 2.3 / 32.2 | 0.1 / 0.6 / 2.5 | 402.3 / 403.5 | 41.4 / 41.5 |
| Rust | 100 | 1.2 / 2.3 / 7.5 | 0.1 / 0.9 / 2.9 | 402.4 / 404.5 | 41.4 / 41.5 |
| Rust | 200 | 1.3 / 2.8 / 63.4 | 0.2 / 1.8 / 21.5 | 402.6 / 406.8 | 41.4 / 41.5 |
| Rust | 400 | 1.3 / 3.2 / 8.9 | 0.3 / 2.3 / 6.3 | 403.2 / 408.3 | 41.5 / 41.5 |
| Rust | 600 | 1.4 / 5.2 / 28.6 | 0.4 / 4 / 20.5 | 403.2 / 417.2 | 41.4 / 41.5 |
| Rust | 800 | 1.6 / 34 / 143.7 | 0.6 / 21.6 / 148 | 404.2 / 472.7 | 41.5 / 41.6 |
| Rust | 1000 | 2.9 / 131.5 / 211.9 | 1.8 / 126.6 / 230.8 | 409.3 / 644.1 | 41.5 / 41.8 |
| Rust | 1500 | 107.4 / 590.4 / 755.9 | 97.3 / 724.6 / 894.9 | 671.9 / 1638.2 | 41.8 / 43.3 |
| Python | 25 | 0.9 / 3.4 / 7.2 | 0.4 / 2.6 / 5.3 | 408.1 / 448.1 | 41.5 / 41.5 |
| Python | 50 | 1 / 4.8 / 22.5 | 0.5 / 4 / 17.4 | 410.8 / 431.5 | 41.5 / 41.5 |
| Python | 100 | 1.8 / 30 / 47.5 | 1.3 / 28.6 / 46.1 | 425 / 588.1 | 41.5 / 41.7 |
| Python | 125 | 4.8 / 102.8 / 179.7 | 4 / 109 / 193.9 | 477.3 / 1356 | 41.7 / 43.5 |
| Python | 150 | 42.2 / 743.9 / 973.4 | 50.7 / 811.2 / 1051 | 1736.8 / 12762.3 | 46.3 / 58.9 |
| Python + uvloop | 25 | 0.5 / 2.6 / 89.9 | 0.4 / 2.3 / 30 | 408.5 / 453.3 | 41.5 / 41.7 |
| Python + uvloop | 50 | 0.7 / 4.5 / 22.2 | 0.5 / 3.5 / 11 | 411.2 / 431.2 | 41.5 / 41.5 |
| Python + uvloop | 100 | 1.5 / 27.8 / 47.5 | 1.1 / 24.3 / 40.9 | 432.6 / 653.2 | 41.5 / 41.8 |
| Python + uvloop | 125 | 5.5 / 407.9 / 705 | 3.9 / 419.8 / 813 | 565.3 / 5314.6 | 42.2 / 47.4 |

### What it shows

- **A call costs Rust about a fifth to a sixth of the CPU it costs Python.**
  At loads both hold, Rust takes 1.65–2.56 ms of CPU per second of call, and
  Python 9.4–12.4 ms (4.8× at 25 lines, 5.7× at 100). uvloop takes Python 1–10%
  lower.
- **One core holds about 8× as many calls in Rust.** With the p99 of both
  audio directions under 50 ms and no call stretched:
  - Rust holds 800 calls on a core (p99 34 and 22 ms).
  - Python holds 100 (p99 30 and 29 ms).

  Past that point:
  - Python at 125 lines reaches 0.1–0.4 s at p99, varying run to run; at
    150 it reaches 0.75–0.8 s, steering waits 1.7 s at the median, and calls
    stretch by 5 s.
  - Rust at 1,000 lines reaches about 130 ms at p99 and still books every
    call. At 1,500 it reaches 0.6–0.7 s.
- **uvloop does not move Python's breaking point.** It is a little cheaper at
  light load. At 125 lines one run did worse than plain asyncio: near
  saturation, a single run's p99 is noisy.
- **Rust holds more memory per call; Python starts much higher.**
  - Rust holds about 440 KB per call on top of about 6 MB at start.
  - Python holds about 220–270 KB per call on top of about 51 MB after
    importing google-genai.
  - Rust's 800 calls on one core fit in 346 MB. Python needs eight
    single-core processes for the same 800 calls, at about 75 MB each,
    roughly 600 MB in all.
  - The Rust runtime's per-session state is the larger of the two, and is
    worth a look.
- **Steering stays at the model's pace in Rust.** The stage change lands
  2–17 ms after the extraction model answers, up to 600 lines. Python adds
  8–25 ms up to 100 lines, then falls behind.
- **Every Rust call booked at every load, 1,500 lines included.** Python
  booked every call up to 150 lines.

### What it does not show

- **The stand-in is not a model.** Its reply and extraction delays are
  fixed, and real ones vary.
- **The audio callbacks do nothing with the audio.** A phone deployment
  would resample it and forward it to the call leg. That costs both
  languages extra, and costs Python more per byte.
- **Each client is one process on one core.** Python scales out by running
  more processes, each paying its import floor. Rust scales across cores
  within one process with more Tokio workers; that is not measured here.
- **Cost per call can overstate a failing run.** "CPU ms per call-second"
  divides by the length of the calls that finished, so it overstates the
  cost of a run whose calls failed. The CPU share is averaged over a run
  that ramps up and drains.

### Running it

```bash
cargo build --release --bin trattoria_mock --bin trattoria
python3 -m venv .venv && .venv/bin/pip install -r python/requirements.txt

# One sweep: a fresh stand-in per run on cores 2-3, the client on core 0.
CARGO_TARGET_DIR=target .venv/bin/python python/run_trattoria.py \
    --impl rust --impl python --impl python-uvloop \
    --lines 50 --lines 100 --secs 90 --out results/my-run.jsonl
.venv/bin/python python/summarize_trattoria.py results/my-run.jsonl

# One call, printing every message the client sends (mic audio aside).
target/release/trattoria_mock --port 8766 --log &
target/release/trattoria --lines 1 --secs 1 --ramp-secs 0 --port 8766
.venv/bin/python python/trattoria.py --lines 1 --secs 1 --ramp-secs 0 --port 8766
```

`trattoria_mock` takes `--reply-ms`, `--extract-ms`, `--end-of-speech-ms` and
`--caller-gap-ms`. The Rust client reads `TOKIO_WORKERS`, and the Python
client takes `--uvloop`.

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
