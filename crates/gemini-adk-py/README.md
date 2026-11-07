# gemini-adk (Python)

Gemini text agents, Python tools, Live sessions, session specs and offline
simulation, on the gemini-rs Rust runtime. One compiled module built with
[maturin](https://www.maturin.rs/) and PyO3; `import gemini_adk` is the whole
install.

```bash
pip install gemini-adk              # Python >= 3.10, one abi3 wheel per platform
pip install "gemini-adk[pydantic]"  # typed answers as Pydantic models

Audio devices are not part of the binding: Python sends and receives PCM
frames as `bytes` and plays or captures them with the audio library of its
choice.
```

Nothing is published yet: the PyPI name is unconfirmed (see "Status").

## Use

```python
import gemini_adk as adk

@adk.tool
def get_weather(city: str) -> dict:
    """Get the current weather for a city.

    Args:
        city: The city name, e.g. "Paris".
    """
    return {"city": city, "condition": "rain", "celsius": 14}

agent = adk.Agent("weather", instruction="Answer weather questions using your tool.",
                  tools=[get_weather])          # credentials from the environment
print(agent.ask("Do I need an umbrella in Paris?"))

chat = agent.chat()                              # remembers its turns
for event in chat.send_stream("Hi, I'm Ada."):   # also `async for`
    if event["kind"] == "text":
        print(event["text"], end="")

city = agent.ask("Describe the largest city in Japan.",
                 schema={"type": "object", "properties": {"name": {"type": "string"}}})
```

Tests use a scripted model and never touch the network:

```python
llm = adk.MockLlm([{"tool": "get_weather", "args": {"city": "Paris"}}, "Take an umbrella."])
agent = adk.Agent("weather", tools=[get_weather], llm=llm)
assert agent.ask("Umbrella?") == "Take an umbrella."
assert llm.call_count() == 2
```

### Live sessions

```python
session = adk.connect(instruction="You are a concierge.", tools=[get_weather])
with session:
    session.send_audio(pcm16_16khz_mono_frame)   # bytes in
    for event in session.events():               # dicts out; `async for` too
        if event["kind"] == "audio":
            play(event["data"])                   # bytes, PCM16 24 kHz
        elif event["kind"] == "turn_complete":
            break
```

`adk.scripted(frames, ...)` is the same session against a scripted server,
for tests: `frames` are server messages in wire form, no credentials.

### Session specs and offline simulation

```python
spec = adk.SessionSpec(document)      # the document Flow Studio and `adk` read
spec.validate()                       # {"valid", "errors", "warnings", ...}
spec.run_tests()                      # the document's embedded tests, offline
spec.to_rust()                        # the equivalent Rust program

convo = adk.Conversation(conversation_spec)
convo.run_scenario({"name": "happy", "steps": [...]})
sim = convo.sim(); sim.step("turn")   # {"active", "denied", "complete", "explain"}
```

## Threads, the GIL and backpressure

- Every synchronous call releases the GIL while the Rust runtime works. The
  `*_async` methods and `async for` integrate with asyncio through
  pyo3-async-runtimes; one Tokio runtime per process, created on first use.
- Python code never runs on a Live session's fast lane. Events reach Python
  through a bridge task that reads the session's broadcast and forwards
  plain data over a bounded channel (`events(buffer=64)`). A consumer that
  falls more than `event_capacity + buffer` events behind loses the oldest
  events for itself alone and receives `{"kind": "lagged", "dropped": n}`;
  nothing upstream slows down.
- Tools run on the control lane. A `def` tool runs on Tokio's blocking pool
  holding the GIL only for its own call. An `async def` tool runs on the
  asyncio loop that was running when the tool was created, so create async
  tools inside the coroutine that runs the agent. A barge-in cancels an
  in-flight async tool's task.
- A blocking entry point called from inside a tool, or from a coroutine on
  a running asyncio loop (which would stall every async tool on that loop),
  raises `RuntimeError` instead of deadlocking; use the `*_async` form
  there. A session that goes out of scope disconnects on the runtime, and
  every open session is closed at interpreter exit.
- `os.fork()` after the runtime started is refused in the child with a
  clear error; use `multiprocessing` with the `spawn` start method.
- One exception hierarchy under `GeminiAdkError`; `TimeoutError` is also a
  `builtins.TimeoutError`. `SpecError.diagnostics` carries the structured
  details.

The design, with the invariant behind each of these and the test that
holds it: `docs/cleanup/python-binding-design.md`.

## Develop

```bash
cd crates/gemini-adk-py
python -m venv .venv && . .venv/bin/activate
pip install "maturin>=1.7,<2.0" -e ".[dev]"
maturin develop --locked
pytest
python -m mypy.stubtest gemini_adk._gemini_adk --ignore-disjoint-bases
```

The crate is excluded from the Cargo workspace (it links libpython).

## Benchmarks

`bench/python/` holds the harness and `RESULTS.md` the committed numbers:
in-process, model-free workloads compared with `google-genai` and the
Python ADK where a comparable operation exists. Request latency to the model
is not benchmarked: it is network-bound and the binding does not change it.

## Status

Experimental. The PyPI distribution name and import name (`gemini-adk` /
`gemini_adk`) are not final and nothing is published until they are.
