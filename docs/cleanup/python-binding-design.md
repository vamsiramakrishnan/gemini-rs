# Python binding design (`crates/gemini-adk-py`)

Status: design, written before any binding code beyond the existing JSON
data-plane surface. Phase 5 implements it. Every invariant below has a test
named beside it; the tests are written with the code that must hold them.

## 0. What exists and what changes

`crates/gemini-adk-py` today is a `cdylib` named `_gemini_adk`, pyo3 0.29
with `abi3-py310`, excluded from the Cargo workspace, built by maturin into
the pure-Python package `gemini_adk` (`python/gemini_adk/__init__.py`). Its
surface is JSON strings in and out: `spec_schema`, `validate_spec`,
`Conversation` (compile, mermaid, `run_scenario`, `sim`), `Sim` (step,
snapshot). It holds compiled conversations in a global map behind integer
handles and runs the two async paths on a current-thread Tokio runtime with
`block_on`, holding the GIL.

That surface stays, re-implemented on the runtime described here (so it stops
blocking with the GIL held). The first release adds:

| Surface | Rust it binds |
|---|---|
| `Agent.ask`, `Agent.chat` → `Chat.send`, `Chat.send_stream` (async iterator of `TextDelta`/`ToolCall`/`ToolResult`/`Finished`), typed answers from a JSON Schema (a Pydantic model's `model_json_schema()`) | `AgentBuilder`, `TextAgent::ask`/`chat`/`run_stream`, `RunRequest.response_schema`, the one-retry correction `ask_as` performs |
| `@tool` on a sync or async Python callable, with the JSON Schema derived from type hints and the docstring | `ToolFunction` implemented by a `PyTool` |
| `LiveSession`: `send_audio(bytes)`, `send_text`, `events()` async iterator carrying audio frames as `bytes`, `close()`; no device dependency | `Live::builder`, `LiveHandle::send_audio`/`send_text`/`events`/`disconnect` |
| `Flow`, `SessionSpec`: `load`, `validate`, `simulate` (offline), `to_rust` | `spec::SessionSpec::from_value`/`validate`/`run_tests`, `codegen::to_rust`, `simulation::Sim` |
| `gemini_adk.testing.MockLlm`, `LlmResponse`, `ScriptedLive` | `llm::MockLlm`, `LlmResponse`, `live::scripted` + `ReplayTransport` |

Out of scope for the first release: Live callbacks registered from Python
(Python observes events; it does not run inside the lanes), voice devices
(`talk()`), the memory crate, telephony transports.

## 1. Lanes and the GIL

The runtime has three lanes (CONTRIBUTING, "Three-lane processor
architecture"). The binding's rule per lane:

| Lane | Thread | May hold the GIL? | Python code runs here? |
|---|---|---|---|
| Fast | the `run_fast_lane` task's worker thread | never | never |
| Control | control-lane task; Python tools are moved to the blocking pool or the asyncio loop | only inside a bounded `Python::with_gil` | yes: tool callables |
| Telemetry | telemetry task | never | never |
| Router (`route_event`) | router task | never | never |

**Invariant L1 (fast-lane isolation).** No Python frame executes on the
thread that runs the fast lane, and no fast-lane operation waits on
anything Python does.

How it holds: the binding never populates a fast-lane slot of
`EventCallbacks` (`on_audio`, `on_text`, `on_input_transcript`, …) with a
Python callable; the only Python-visible path for fast-lane data is the
`LiveEvent` broadcast the processor already emits
(`crates/gemini-adk-rs/src/live/processor.rs::run_fast_lane`, a
`broadcast::Sender::send` that never blocks). The bridge in §3 consumes that
broadcast on its own Tokio task.

Ways it can break, and the test for each:

| Break | Test |
|---|---|
| A future change adds a Python fast-lane callback | `fast_lane_never_sees_python`: a Rust `on_audio` callback records its thread id; a Python tool and a Python event consumer record theirs; the sets are disjoint over a scripted session with 500 audio frames and one tool call |
| A slow Python consumer stalls the fast lane | `slow_consumer_does_not_stall_fast_lane`: the consumer sleeps 50 ms per event; the Rust `on_audio` callback's inter-arrival gaps over 500 frames stay within the replay's own pacing (p99 under 5 ms added); the consumer receives a `Lagged` event whose count plus received frames equals 500 |
| A Python event consumer holds the GIL while the bridge needs it | the bridge needs the GIL only to build the Python object handed to the consumer, inside `future_into_py`; test `bridge_builds_events_off_lane`: event objects are constructed on the asyncio thread (thread id recorded) |

**Invariant L2 (control-lane GIL bound).** A Python tool holds the GIL only
for the duration of its own call, on a thread that is not a Tokio worker
unless the tool is async.

- Sync callables: `PyTool::call_with_context` runs the callable in
  `tokio::task::spawn_blocking`, acquiring the GIL inside it. A tool that
  takes a second blocks one blocking-pool thread, not a runtime worker.
- Async callables: the callable is invoked under the GIL to obtain the
  coroutine, the coroutine is scheduled on the asyncio loop that owns the
  session (`pyo3_async_runtimes::tokio::into_future_with_locals`), and the
  control lane awaits the resulting Rust future without the GIL. The user's
  asyncio loop must be running for an async tool to progress; a loop
  blocked by the user's own sync code stalls async tools, as it would any
  coroutine. Documented, not worked around.
- The result crosses back as `serde_json::Value` built under the GIL
  (`pythonize`), then the GIL is released before the control lane continues.

Test `sync_tool_runs_on_blocking_pool`: the tool records whether
`tokio::runtime::Handle::try_current()` sees a worker (it must not) and that
`Python::with_gil` was entered exactly once per call.

**Invariant L3 (every call into the runtime releases the GIL).** Each
`#[pyfunction]`/`#[pymethods]` entry that reaches the runtime does so either
through `pyo3_async_runtimes::tokio::future_into_py` (the Rust future runs on
Tokio, the GIL is not held) or, for the few synchronous entry points
(`SessionSpec.validate`, `Flow.simulate`, `Conversation.run_scenario`,
`Sim.step`), through `py.allow_threads(|| runtime().block_on(...))`.

Test `sync_entry_points_release_gil`: a Python thread spins a counter while
the main thread runs `SessionSpec.simulate` on a 2 000-step scenario; the
counter advances (it cannot if the GIL is held). Test
`async_entry_points_release_gil`: the same with `await agent.ask(...)` on
`MockLlm` configured with a 200 ms delayed reply.

## 2. One Tokio runtime per process

`runtime()` returns a `&'static tokio::runtime::Runtime`, created on first
use by `std::sync::OnceLock`, multi-threaded, `worker_threads = min(4,
available_parallelism)`, thread name `gemini-adk-py-worker`. It is also the
runtime `pyo3-async-runtimes` uses (`pyo3_async_runtimes::tokio::init` with
the same builder, called once from the module initialiser so the two never
diverge).

Ways it can break:

| Break | Handling | Test |
|---|---|---|
| `os.fork()` after the runtime started: worker threads do not survive in the child | documented: use `multiprocessing` with the `spawn` start method; the child's first runtime call panics with a clear message if `pthread_atfork` detects a fork (a `OnceLock<pid>` compared on each `runtime()` call) | `fork_after_init_is_refused` (Linux only) |
| Interpreter finalisation while Tokio tasks hold `Py<PyAny>` | tasks hold only `Py<PyAny>`, whose drop without the GIL is deferred by pyo3; the module registers an `atexit` hook that closes every open `LiveSession` and waits up to 2 s for the bridge tasks | `atexit_closes_sessions` |
| Two runtimes (the old current-thread `RT` and the new one) | the old static is deleted in the same commit that introduces `runtime()` | `exactly_one_runtime` counts `gemini-adk-py-worker` threads after exercising every surface |
| A Python callback re-enters the runtime from inside a Tokio worker (`block_on` inside `block_on`) | synchronous entry points assert `Handle::try_current().is_err()` and raise `RuntimeError("called from inside the runtime; use the async form")` | `block_on_inside_worker_is_an_error` |

## 3. The asyncio bridge and the fast-lane channel

Events reach Python through this chain:

```
processor fast lane ──broadcast(cap E, drop-oldest per lagging receiver)──▶ bridge task
bridge task ──mpsc(cap B)──▶ future_into_py per __anext__ ──▶ asyncio consumer
```

- `E` is `LiveSessionBuilder::event_capacity` (default 512). A receiver that
  falls more than `E` behind skips the oldest events and receives
  `RecvError::Lagged(n)`; the sender never blocks (tokio broadcast
  semantics). This is the only place frames are dropped, and it drops for
  the slow subscriber alone.
- The bridge task (one per `LiveSession.events()` call) maps each
  `LiveEvent` to a plain-Rust `PyLiveEvent` (an enum of `Vec<u8>`/`String`/
  `serde_json::Value`, no Python objects) and does `tx.send(ev).await` on a
  bounded `mpsc` of capacity `B = 64`. When Python is slow the mpsc fills,
  the bridge awaits, the broadcast backlog grows to `E`, then the broadcast
  starts skipping the oldest frames. `Lagged(n)` becomes a `LiveEvent` of
  kind `"lagged"` with `dropped = n` delivered in order to the consumer.
- `__anext__` returns `future_into_py(rx.recv())`; the Python object for the
  event is built on the asyncio thread when that future resolves.

**Backpressure policy, stated for the documentation:** the consumer may fall
up to `E + B` events behind (about 23 s of model audio at the default
capacities and 40 ms frames); beyond that the oldest events are discarded
for that consumer, each discard run is reported once as a `lagged` event
carrying the count, and nothing upstream of the broadcast slows down or
drops. `LiveSession.events(capacity=...)` sets `B`; `Live.builder()
.event_capacity(...)` sets `E`. There is no "latest only" mode: audio is
sequential, so skipping the oldest frames and saying so is the one honest
policy. Text deltas and transcripts are small and fit the same buffer.

Input audio (`send_audio(bytes)`) goes the other way: `future_into_py(
handle.send_audio(Bytes::from(buf)))`. The bytes are copied once from the
Python buffer under the GIL (`PyBytes::as_bytes().to_vec()`); after that the
GIL is released. The wire writer's own backpressure
(`gemini-genai-rs/src/transport/backpressure.rs`) applies.

Tests: `slow_consumer_does_not_stall_fast_lane` (§1), `lagged_event_counts_
every_drop` (2 000 frames, consumer sleeps, `received + sum(lagged) == 2000`,
events arrive in order), `two_consumers_lag_independently`, `events_after_
close_end_the_iterator` (the iterator raises `StopAsyncIteration` once the
session closes and the buffer drains).

## 4. Cancellation from Python into Rust

| Python action | Rust effect | Test |
|---|---|---|
| `task.cancel()` on a task awaiting `agent.ask()` / `chat.send()` / `session.send_audio()` | `pyo3-async-runtimes` drops the Rust future when the Python future is cancelled; the `MockLlm`/transport call in flight is dropped | `cancelled_ask_drops_rust_future` (a drop guard in a `from_fn` `MockLlm` sets a flag) |
| `await session.close()` | `LiveHandle::disconnect()`; the bridge tasks finish when the broadcast closes; pending `send_audio` futures resolve with `SessionClosedError` | `close_ends_events_and_fails_sends` |
| `async with session:` exit, or garbage collection of a `LiveSession` | `close()` scheduled on the runtime; drop never blocks the GIL thread | `dropped_session_disconnects` |
| Barge-in cancels an in-flight Python tool | sync tool: cannot be interrupted; its result is discarded by the runtime as today. Async tool: the wrapper holds the asyncio `Task` and calls `task.cancel()` under the GIL when `ToolContext.cancellation` fires; the control lane does not wait for the Python side | `async_tool_is_cancelled_on_barge_in`, `sync_tool_result_after_barge_in_is_discarded` |
| `KeyboardInterrupt` during a sync entry point | `allow_threads` returns when the Rust call returns; the sync entry points are bounded (simulation, validation), so this is at most the remaining simulation; documented | n/a |

## 5. Errors across the boundary

One exception hierarchy in `gemini_adk.errors`, rooted at `GeminiAdkError
(Exception)`. Mapping from `AgentError` (`crates/gemini-adk-rs/src/error.rs`):

| `AgentError` variant | Python exception |
|---|---|
| `Config(String)`, and `ConfigError` from `build()` | `ConfigError` |
| `Session(SessionError)` | `SessionError` (`.kind` carries the variant name) |
| `SessionClosed` | `SessionClosedError(SessionError)` |
| `Tool(ToolError::NotFound)` | `ToolNotFoundError(ToolError)` |
| `Tool(ToolError::InvalidArgs)` | `ToolArgumentError(ToolError)` |
| `Tool(ToolError::ExecutionFailed)` and the other `ToolError` variants | `ToolExecutionError(ToolError)` |
| `Llm(LlmError)` | `LlmError` (`.kind`) |
| `State(StateError)` | `StateError` |
| `InvalidOutput { .. }` | `InvalidOutputError` (`.raw` holds the model's text) |
| `UnknownAgent`, `TransferRequested`, `TransferFailed` | `TransferError` |
| `Timeout` | `TimeoutError(GeminiAdkError, builtins.TimeoutError)` |
| `Other(String)` | `GeminiAdkError` |
| spec validation (`SpecValidation` errors), `Conversation` compile diagnostics | `SpecError` with `.diagnostics: list[dict]` |

The mapping lives in one function, `to_py_err(AgentError) -> PyErr`, and a
test `every_variant_maps` constructs each variant and asserts the Python
class. A Python tool that raises produces `ToolError::ExecutionFailed` whose
message is `"{ExceptionType}: {message}"` with the traceback appended, so
the model's correction turn and the Rust logs both see it; the original
exception object is also attached as `__cause__` when the error is
re-raised to Python from the same process (`tool_exception_is_chained`).

## 6. Typed answers

`Agent.ask(prompt, *, schema=None)`: `schema` is a JSON Schema `dict`; when
given, the request carries it as the response schema and the reply is
parsed as JSON, retried once with the parse error as the correction prompt
(mirroring `ask_as`), and returned as a `dict`. A Pydantic helper in the
pure-Python layer, `ask_as(prompt, model: type[BaseModel])`, passes
`model.model_json_schema()` and returns `model.model_validate(reply)`.
Pydantic is an optional extra (`gemini-adk[pydantic]`), never imported by
the core. Test `typed_answer_retries_once` uses `MockLlm.script([bad JSON,
good JSON])` and asserts two requests.

## 7. Packaging

- `pyproject.toml`: maturin, `abi3-py310`, extras `pydantic` and `voice`
  (the latter builds with the `voice-io` feature and pulls the system audio
  stack; it is not part of the first release's surface and is only a build
  option).
- Wheels: `maturin build --release` for `linux x86_64` and `aarch64`
  (manylinux_2_28, via `maturin-action` with QEMU for aarch64), `macos
  arm64`, `windows x86_64`. One abi3 wheel per platform.
- `python/gemini_adk/_gemini_adk.pyi` is hand-written from the Rust
  signatures; `py.typed` marks the package; CI runs
  `python -m mypy.stubtest gemini_adk._gemini_adk` against the built module.
- PyPI name and import name are unconfirmed (today `gemini-adk` /
  `gemini_adk`); nothing is published until Vamsi confirms them.

## 8. Benchmarks

`bench/python/` compares, in-process and model-free, `gemini_adk` against
`google-genai` and the Python ADK (`google-adk`) on: Live frame decoding
(JSON server messages, text and binary paths), PCM conversion and
resampling, VAD, guard evaluation, `SessionSpec` validation, offline
simulation. p50 and p99 per workload, machine and revision recorded, output
committed as `bench/python/RESULTS.md`. Request latency to the model is not
benchmarked and the report says so: it is network-bound and the binding
does not change it. README numbers come only from that file.

## 9. Order of work in Phase 5

1. Runtime + bridge + error module, with the §1–§5 tests, no new surface.
2. Text agents, tools, `MockLlm`, the quickstart port in CI.
3. `SessionSpec`/`Flow`/`Conversation` on the new runtime (old handle maps
   removed).
4. `LiveSession` + `ScriptedLive`, the slow-consumer tests.
5. Stubs, stubtest, wheel matrix.
6. Benchmark harness and `RESULTS.md`.
