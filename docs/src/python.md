# Python Bindings

`gemini_adk` is the Rust runtime as one Python extension module, built with
PyO3 and maturin. Text agents, Python tools, Live sessions, session specs
and offline simulation; `import gemini_adk` is the whole install. The
package lives in `crates/gemini-adk-py`; its README is the reference for
the surface and the threading contract. This page is the short version.

> Experimental: the Python API may change between minor releases. Install
> it with `pip install gemini-adk` (Python 3.10+), or build it locally with
> `maturin develop` (see the crate README).

## A text agent with a tool

```python
import gemini_adk as adk

@adk.tool
def get_weather(city: str) -> dict:
    """Get the current weather for a city.

    Args:
        city: The city name, e.g. "Paris".
    """
    return {"city": city, "condition": "rain", "celsius": 14}

agent = adk.Agent("weather", instruction="Use your tool.", tools=[get_weather])
print(agent.ask("Do I need an umbrella in Paris?"))
```

The decorator derives the tool's JSON Schema from the type hints and the
`Args:` section of the docstring. `async def` tools run on the asyncio
loop that was running when the tool was created.

Typed answers take a JSON Schema (`agent.ask(prompt, schema=...)`) or, with
the `pydantic` extra, a model class (`adk.ask_as(agent, prompt, City)`). A
reply that does not parse is sent back once for correction, as
`ask_as` does in Rust.

Tests use `adk.MockLlm([...])`, a scripted model that records every
request, so nothing in a test suite reaches the network.

## Live sessions

```python
session = adk.connect(instruction="You are a concierge.", tools=[get_weather])
with session:
    session.send_audio(frame)                 # PCM16 16 kHz mono bytes
    for event in session.events():            # or `async for`
        if event["kind"] == "audio":
            play(event["data"])               # PCM16 24 kHz bytes
        elif event["kind"] == "turn_complete":
            break
```

Python never runs on the fast lane. Events cross through a bridge task and
a bounded channel; a consumer that falls behind loses the oldest events for
itself alone and receives `{"kind": "lagged", "dropped": n}`. The
invariants and the tests behind them are in the
[binding design](https://github.com/vamsiramakrishnan/gemini-rs/blob/main/docs/cleanup/python-binding-design.md).

`adk.scripted(frames, ...)` runs the same session against a scripted server
for tests.

## Session specs offline

```python
spec = adk.SessionSpec(document)   # the document Flow Studio and `adk` read
spec.validate()
spec.run_tests()                   # the embedded tests, no model
spec.to_rust()                     # the equivalent Rust program

convo = adk.Conversation(conversation_spec)
convo.run_scenario(scenario)
sim = convo.sim()
sim.step("turn")                   # {"active", "denied", "complete", "explain"}
```

See [Flows as JSON](./user-guide/flow-json.md) for the document format and
[Testing Conversations Offline](./user-guide/testing.md) for scenarios.
