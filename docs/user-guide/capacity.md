# Capacity and Cost per Session

How much memory one idle Live session costs, how many a process can hold, and
how much latency the runtime adds on top of the model. The
`example-session-bench` harness answers those questions without a model or a
network: it attaches the real L1 control plane to a scripted L0 transport
that answers every turn after a fixed delay, so the numbers isolate the
runtime.

```bash
just bench-sessions 100          # 100 sessions, 10 turns each, JSON in target/
cargo run --release -p example-session-bench -- \
    --sessions 200 --turns 20 --think-ms 200 --response-delay-ms 300 \
    --hold-secs 60 --json capacity.json
```

Always benchmark a `--release` build: the debug profile inflates both memory
and per-turn time.

## What one session is

Every `Live::connect` (or `attach_session`) starts one L0 connection task and
the L1 lanes: a router, a fast lane, a control lane and a telemetry lane, plus
a temporal timer when temporal actions are registered. Voice I/O adds an
uplink and a downlink task. The harness measures exactly that stack. It does
not include your tools, extractors or phase machine; build those into the
`LiveSessionBuilder` in `open_session` to measure a configuration that
matches production.

## What the report says

| Field | Meaning |
|---|---|
| `rss.baseline_kb` | Resident set before the first session is built. |
| `rss.after_connect_kb` | With every session connected and idle. |
| `rss.per_session_kb` | `(after_connect - baseline) / sessions`. The marginal cost of an idle session, allocator slack included. |
| `rss.peak_kb` | Highest sample seen while turns were in flight. |
| `rss.after_disconnect_kb` | After every session disconnected and its lanes joined. Rising across runs of the same size is a leak. |
| `connect_ms` | `connect` plus `attach_session`, per session, while all sessions connect at once. |
| `first_text_ms` | `send_text` to the first `TextDelta` the application sees. Subtract `response_delay_ms` for the runtime's share. |
| `turn_ms` | `send_text` to `TurnComplete`. |
| `telemetry_response_count` | The runtime's own `SessionTelemetry` count of text turns. It must equal `turns_completed`; the CLI exits non-zero when it does not. |

RSS comes from `/proc/self/status`; on other platforms `rss.available` is
`false` and the memory fields are zero.

## Reading the latency numbers

`TurnComplete` travels on the control lane and the turn's text on the fast
lane. The two are ordered within a lane, not across lanes, so an application
that starts its next turn on `TurnComplete` alone can see the previous turn's
text arrive first. The harness waits for `TextComplete` and `TurnComplete`
before it starts the next turn; do the same in a load generator of your own,
or the first-text figures will be attributed to the wrong turn.

**Known limitation.** The runtime's first-text latency is recorded by the
telemetry lane, a third consumer of the L0 event broadcast, while
`send_text` stamps the send time from the caller's task. If the next
`send_text` is issued before the telemetry lane has consumed the previous
turn's first delta, that delta is attributed to the new send and the new
turn's real first delta is dropped, so `telemetry_response_count` falls
short. The window is the lane's scheduling lag, microseconds in practice,
and only `--think-ms 0` reaches it. Keep a non-zero think time when the
cross-check matters.

`--think-ms` is the pause between one session's turns. With `--think-ms 0`
every session is always mid-turn, which is a worst case, not a call center.
Pick a value near the real gap between user utterances.

## Reference numbers

Release profile, one Linux container (shared vCPUs, no tuning), scripted
delay 20 ms, 20 ms think time. Treat these as the shape of the cost, not a
spec; run the harness on the deployment hardware for numbers you can plan
with.

| Sessions | Turns | Idle RSS per session | Connect p99 | First text p50 / p99 | Turn p99 |
|---|---|---|---|---|---|
| 100 | 10 | 787 kB | 1.2 ms | 21.7 / 22.6 ms | 22.6 ms |
| 500 | 5 | 772 kB | 1.7 ms | 22.8 / 26.2 ms | 26.0 ms |

Subtracting the 20 ms scripted delay, the runtime adds about 2 ms at p50 and
under 3 ms at p99 with 100 concurrent sessions, and about 6 ms at p99 with
500, all turns in flight at once. Idle memory is under 1 MB per session, so a
process holding 1,000 idle sessions needs on the order of 800 MB before any
audio buffers, tools or extractors are added.

RSS after disconnect stays above the baseline (21 MB after the 100-session
run, 75 MB after 500). That is allocator retention of freed pages, not a
per-session leak: a second run of the same size in a fresh process reaches
the same `after_connect_kb`, and `after_disconnect_kb` does not grow with
repeated runs. Use `--hold-secs` with a long idle period to look for the
slow kind.

## Microphone jitter

A voice agent streams microphone audio in 20 ms chunks, and the model's
voice activity detection reads their timing. A chunk held up behind a busy
lane arrives late, and a run of late chunks reads as a pause. The jitter mode
measures how the runtime forwards that audio:

```bash
cargo run --release -p example-session-bench -- --jitter-secs 10 --sessions 100 --json jitter.json
```

Each session gets a paced microphone task that stamps a sequence number into
its PCM and calls `send_audio` every 20 ms. The scripted transport reads the
number back from each `realtimeInput` frame and records when the frame
arrived. The report has three distributions:

- **mic → wire**: `send_audio` to the frame reaching the transport, through
  L1 and the L0 encoder. This is the latency the runtime adds.
- **wire jitter**: how far the spacing between one session's frames on the
  wire strays from 20 ms.
- **source jitter**: the same measure for the microphone task's own timer.
  This is the harness's noise floor, and wire jitter cannot be measured
  below it.

It also counts chunks lost and chunks reordered. The run exits non-zero if
either is non-zero.

Release profile, the same container as above (4 shared vCPUs, Intel Xeon
2.1 GHz), Rust 1.93.1, 10 s per run:

| Sessions | Chunks | Mic → wire p50 / p99 / max | Wire jitter p99 / max | Source jitter p99 / max |
|---|---|---|---|---|
| 1 | 501 | 0.032 / 0.063 / 0.082 ms | 0.829 / 0.843 ms | 0.829 / 0.852 ms |
| 10 | 5,010 | 0.005 / 0.045 / 0.301 ms | 0.809 / 0.906 ms | 0.813 / 0.896 ms |
| 100 | 50,100 | 0.006 / 0.034 / 0.890 ms | 1.162 / 2.722 ms | 1.159 / 2.739 ms |
| 500 | 250,500 | 0.006 / 0.029 / 1.607 ms | 2.305 / 7.164 ms | 2.302 / 7.147 ms |
| 1,000 | 501,000 | 0.006 / 0.036 / 2.388 ms | 2.886 / 13.349 ms | 2.879 / 13.342 ms |

No chunk was lost or reordered in any run. Mic → wire stays below 0.04 ms at
p99 at every size. Wire jitter matches source jitter to within 0.01 ms, which
means the runtime adds no jitter this harness can detect. The jitter that
does grow with load, 2.9 ms at p99 with 1,000 sessions, comes from the
scheduler waking the microphone tasks late. It is a property of the host
running 1,000 timers on four vCPUs, not of the audio path.

The benchmark covers what the SDK controls. It stops at the transport, so
network jitter between the process and the Live endpoint, and the model's
own buffering, are not included.

## Using it as a gate

Keep one JSON report per release under version control and diff the
per-session memory and the p99 turn latency. The `smoke` test in the harness
crate runs under `cargo test --workspace` with four sessions, so the harness
itself cannot rot; it asserts completion and the telemetry cross-check, not
absolute numbers, because those depend on the machine.

For the multi-app server (`gemini-adk-web-rs`, `gemini-adk-api-rs`) note that
each browser WebSocket builds one of these sessions, and the server currently
sets no cap on how many. Use `rss.per_session_kb` from a release-profile run
on the deployment hardware to choose one.
