# Python binding vs native Rust, on the real Gemini API

Produced by `python bench/python/live_api.py` (method and reproduction in
its docstring). Both sides run the same SDK code: `gemini_adk` from
Python, and `crates/gemini-adk-py/examples/native_bench.rs` from Rust,
each timed at its own API boundary. Samples are paired: the two sides run
back to back in random order, and the comparison is the median of the
per-pair difference (Python minus Rust) with a bootstrap 95% interval.
Raw samples are in `live_api.json`.

Run 2026-11-07 20:22 UTC; revision `3998959`; Linux x86_64, 4 vCPU cloud
container; Python 3.13.18; gemini_adk 5.0.0 (release build); text model
`gemini-3.5-flash-lite`, temperature 0; Live model the SDK default
(`gemini-2.5-flash-native-audio-latest`).

## Against the API

| Workload | Pairs | Python p50 (ms) | Rust p50 (ms) | Python − Rust, median (ms) | 95% interval (ms) | Python faster in |
|---|---:|---:|---:|---:|---:|---:|
| `ask`, one-word reply | 60 | 528.2 | 527.2 | +12.7 | −46.6 … +73.1 | 28 / 60 |
| `ask`, ~120-word reply | 30 | 1082.6 | 1050.5 | +29.1 | −40.0 … +72.0 | 14 / 30 |
| Tool round trip (2 model calls + tool) | 30 | 1087.0 | 1036.1 | +15.6 | −41.6 … +86.4 | 13 / 30 |
| Live: connect | 20 | 112.9 | 109.8 | −1.0 | −8.8 … +7.6 | 11 / 20 |
| Live: text sent → first audio | 20 | 1695.6 | 1626.8 | +25.1 | −69.9 … +237.3 | 9 / 20 |
| Live: text sent → turn complete | 20 | 4904.3 | 4844.0 | +1.0 | −152.1 … +215.1 | 10 / 20 |

Every interval contains zero: at this many samples, no difference between
the two is measurable. The intervals are the resolution of the
measurement, set by the API's own variance: about ±60 ms for a text call
and about ±200 ms for Live audio. Both sides made 30 tool calls in 30
round trips, and Live replies averaged 55 to 57 audio chunks on both.
The ~120-word replies came out 22 characters shorter on the Python side
at the median (interval −30 … −1.5); both sides send the same request,
so this is the model's own variation at temperature 0, and if anything it
favours the Python timings slightly.

An earlier run the same day on the `gemini-flash-lite-latest` alias gave
the same answer (every interval containing zero, Python faster in about
half of the pairs).

## Without the network

| Workload | Calls | Python p50 (µs) | Rust p50 (µs) | Python p99 (µs) | Rust p99 (µs) |
|---|---:|---:|---:|---:|---:|
| `ask` against `MockLlm` (echo) | 6000 | 4.29 | 4.32 | 11.4 | 20.2 |

This is the binding's own cost per call, and it is below what this
measures: the median difference was −0.03 µs here and +0.3 µs in the
earlier run, against a 500 ms request. For Live audio the per-frame
bridge cost is in `RESULTS.md` (about 26 µs per 40 ms frame, end to end
through the binding).

## Reading it

Python running the Rust runtime and native Rust are the same speed against
Gemini: the request time is the model and the network, and the binding
adds at most a few microseconds to it. Choose by language and deployment, not by
latency. The one place the binding's cost could matter is a tight
in-process loop with no network (the offline row), where it is still a
few microseconds per call.

The benchmark also found a bug both sides shared: a tool round trip on a
thinking model failed with HTTP 400 because function calls went back
without their `thoughtSignature`; fixed in `e65646c` (decision D22) before
these numbers were taken.
