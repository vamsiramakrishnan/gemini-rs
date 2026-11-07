# Python binding vs native Rust, on the real Gemini API

Produced by `python bench/python/live_api.py` (method and reproduction in
its docstring). Both sides run the same SDK code: `gemini_adk` from
Python, and `crates/gemini-adk-py/examples/native_bench.rs` from Rust,
each timed at its own API boundary. Samples are paired: the two sides run
back to back in random order, and the comparison is the median of the
per-pair difference (Python minus Rust) with a bootstrap 95% interval.
Raw samples are in `live_api.json`.

Run 2026-11-07 11:06 UTC; revision `caac7a8` plus nothing; Linux x86_64,
4 vCPU cloud container; Python 3.13.18; gemini_adk 5.0.0 (release build);
text model `gemini-flash-lite-latest`, temperature 0; Live model the SDK
default (`gemini-2.5-flash-native-audio-latest`).

## Against the API

| Workload | Pairs | Python p50 (ms) | Rust p50 (ms) | Python − Rust, median (ms) | 95% interval (ms) | Python faster in |
|---|---:|---:|---:|---:|---:|---:|
| `ask`, one-word reply | 60 | 676.7 | 669.8 | +8.9 | −28.7 … +31.8 | 29 / 60 |
| `ask`, ~120-word reply | 30 | 1140.2 | 1187.0 | −31.0 | −91.1 … +29.5 | 19 / 30 |
| Tool round trip (2 model calls + tool) | 30 | 1290.0 | 1363.0 | −20.5 | −159.2 … +54.3 | 16 / 30 |
| Live: connect | 20 | 105.2 | 112.7 | −4.9 | −17.0 … +2.9 | 12 / 20 |
| Live: text sent → first audio | 20 | 2225.0 | 2127.4 | +290.6 | −555.6 … +753.2 | 8 / 20 |
| Live: text sent → turn complete | 20 | 5240.5 | 5157.9 | +108.9 | −533.2 … +671.5 | 9 / 20 |

Every interval contains zero: at this many samples, no difference between
the two is measurable. The intervals are the resolution of the
measurement, set by the API's own variance: about ±30 ms for a short
`ask`, about ±100 ms for a tool round trip, and about ±600 ms for Live
audio, whose first-audio time alone ranges over several hundred
milliseconds between identical requests. Both sides made 30 tool calls in
30 round trips, and Live replies averaged 54 audio chunks on both.

## Without the network

| Workload | Calls | Python p50 (µs) | Rust p50 (µs) | Python p99 (µs) | Rust p99 (µs) |
|---|---:|---:|---:|---:|---:|
| `ask` against `MockLlm` (echo) | 6000 | 4.17 | 3.84 | 14.0 | 20.0 |

This is the binding's own cost per call: about 0.3 µs at the median,
against a 670 ms request — roughly one part in two million. For Live
audio the per-frame bridge cost is in `RESULTS.md` (about 26 µs per 40 ms
frame, end to end through the binding).

## Reading it

Python running the Rust runtime and native Rust are the same speed against
Gemini: the request time is the model and the network, and the binding
adds microseconds to it. Choose by language and deployment, not by
latency. The one place the binding's cost could matter is a tight
in-process loop with no network (the offline row), where it is still a
few microseconds per call.

The benchmark also found a bug both sides shared: a tool round trip on a
thinking model failed with HTTP 400 because function calls went back
without their `thoughtSignature`; fixed in `e65646c` (decision D22) before
these numbers were taken.
