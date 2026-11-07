"""Text agents over `MockLlm`: ask, chat, streams, typed answers, tools."""

import asyncio
import json
import subprocess
import sys
import threading

import pytest

import gemini_adk as adk
from gemini_adk import _gemini_adk as ext


def test_ask_with_scripted_reply():
    llm = adk.MockLlm(["Hello, Ada."])
    agent = adk.Agent("assistant", instruction="Be brief.", llm=llm)
    assert agent.ask("Hi") == "Hello, Ada."
    assert llm.call_count() == 1
    request = llm.last_request()
    assert request is not None
    assert "Hi" in str(request)


def test_echo_mock_repeats_the_prompt():
    agent = adk.Agent("echo", llm=adk.MockLlm())
    assert "ping" in agent.ask("ping")


def test_chat_keeps_history():
    llm = adk.MockLlm(["Nice to meet you.", "Your name is Ada."])
    chat = adk.Agent("assistant", llm=llm).chat()
    assert chat.send("Hi, I'm Ada.") == "Nice to meet you."
    assert chat.send("What's my name?") == "Your name is Ada."
    history = chat.history()
    assert len(history) == 4
    # The second request carried the first turn.
    assert "Ada" in str(llm.requests()[1])


def test_send_full_reports_usage_and_tool_calls():
    @adk.tool
    def lookup(key: str) -> str:
        """Look a key up."""
        return f"value of {key}"

    llm = adk.MockLlm([{"tool": "lookup", "args": {"key": "k"}}, "done"])
    chat = adk.Agent("a", tools=[lookup], llm=llm).chat()
    result = chat.send_full("look up k")
    assert result["text"] == "done"
    assert result["tool_calls"][0]["name"] == "lookup"
    assert result["tool_calls"][0]["result"] == "value of k"
    assert result["model_calls"] == 2
    assert set(result["usage"]) == {"prompt_tokens", "completion_tokens", "total_tokens"}


def test_stream_yields_text_then_finished():
    llm = adk.MockLlm(["streamed reply"])
    chat = adk.Agent("a", llm=llm).chat()
    events = list(chat.send_stream("go"))
    kinds = [e["kind"] for e in events]
    assert kinds[-1] == "finished"
    assert "".join(e["text"] for e in events if e["kind"] == "text") == "streamed reply"
    assert len(chat.history()) == 2


async def test_stream_async_iteration():
    llm = adk.MockLlm(["async reply"])
    chat = adk.Agent("a", llm=llm).chat()
    kinds = [e["kind"] async for e in chat.send_stream("go")]
    assert kinds[-1] == "finished"


async def test_ask_async():
    agent = adk.Agent("a", llm=adk.MockLlm(["later"]))
    assert await agent.ask_async("now") == "later"


def test_typed_answer_returns_a_dict():
    llm = adk.MockLlm(['{"name": "Tokyo", "population_millions": 37.4}'])
    agent = adk.Agent("geo", llm=llm)
    answer = agent.ask("Largest city in Japan?", schema={"type": "object"})
    assert answer == {"name": "Tokyo", "population_millions": 37.4}
    assert llm.call_count() == 1


def test_typed_answer_retries_once():
    llm = adk.MockLlm(["not json", '{"ok": true}'])
    agent = adk.Agent("geo", llm=llm)
    assert agent.ask("?", schema={"type": "object"}) == {"ok": True}
    assert llm.call_count() == 2
    second = llm.requests()[1]
    assert "could not be read as the requested JSON" in str(second)


def test_typed_answer_gives_up_after_one_retry():
    llm = adk.MockLlm(["nope", "still nope", '{"never": "reached"}'])
    agent = adk.Agent("geo", llm=llm)
    with pytest.raises(adk.InvalidOutputError):
        agent.ask("?", schema={"type": "object"})
    assert llm.call_count() == 2


def test_ask_as_with_pydantic():
    pydantic = pytest.importorskip("pydantic")

    class City(pydantic.BaseModel):
        name: str
        population_millions: float

    llm = adk.MockLlm(['{"name": "Tokyo", "population_millions": 37.4}'])
    city = adk.ask_as(adk.Agent("geo", llm=llm), "Largest city in Japan?", City)
    assert city.name == "Tokyo"
    assert llm.last_request()["response_json_schema"]["title"] == "City"


def test_ask_as_without_pydantic_model_is_a_type_error():
    with pytest.raises(TypeError):
        adk.ask_as(adk.Agent("a", llm=adk.MockLlm()), "x", dict)


def test_sync_entry_points_release_gil():
    """A Python thread keeps running while the main thread is inside the
    runtime (`py.detach`); it could not if the GIL were held."""
    llm = adk.MockLlm([f"reply {i}" for i in range(200)])
    agent = adk.Agent("a", llm=llm)
    ticks = 0
    stop = threading.Event()

    def spin():
        nonlocal ticks
        while not stop.is_set():
            ticks += 1

    t = threading.Thread(target=spin)
    t.start()
    try:
        chat = agent.chat()
        for i in range(200):
            chat.send(f"turn {i}")
    finally:
        stop.set()
        t.join()
    assert ticks > 0


def test_block_on_inside_worker_is_an_error():
    """A sync tool runs on the blocking pool, inside the runtime; a blocking
    entry point from there must refuse instead of deadlocking."""
    inner = adk.Agent("inner", llm=adk.MockLlm(["unused"]))
    seen = {}

    @adk.tool
    def nested(x: int) -> str:
        """Call the runtime from inside a tool."""
        seen["inside"] = ext._inside_runtime()
        try:
            return inner.ask("hi")
        except RuntimeError as e:
            seen["error"] = str(e)
            raise

    llm = adk.MockLlm([{"tool": "nested", "args": {"x": 1}}, "after"])
    chat = adk.Agent("outer", tools=[nested], llm=llm).chat()
    result = chat.send_full("go")
    assert seen["inside"] is True
    assert "use the async form" in seen["error"]
    assert result["tool_calls"][0]["error"]
    assert "RuntimeError" in result["tool_calls"][0]["error"]
    assert result["text"] == "after"


def test_tool_exception_reaches_the_model_with_traceback():
    @adk.tool
    def explode() -> str:
        """Fail."""
        raise KeyError("missing")

    llm = adk.MockLlm([{"tool": "explode", "args": {}}, "recovered"])
    result = adk.Agent("a", tools=[explode], llm=llm).chat().send_full("go")
    error = result["tool_calls"][0]["error"]
    assert "KeyError" in error and "missing" in error and "Traceback" in error
    assert result["text"] == "recovered"


def test_sync_tool_runs_on_blocking_pool_not_the_calling_thread():
    main = threading.get_ident()
    where = {}

    @adk.tool
    def probe() -> str:
        """Record the thread."""
        where["thread"] = threading.get_ident()
        where["inside_runtime"] = ext._inside_runtime()
        return "ok"

    llm = adk.MockLlm([{"tool": "probe", "args": {}}, "done"])
    adk.Agent("a", tools=[probe], llm=llm).ask("go")
    assert where["thread"] != main
    assert where["inside_runtime"] is True


async def test_async_tool_runs_on_the_asyncio_loop():
    loop = asyncio.get_running_loop()
    seen = {}

    @adk.tool
    async def fetch(url: str) -> dict:
        """Fetch something, asynchronously."""
        seen["loop"] = asyncio.get_running_loop()
        await asyncio.sleep(0.01)
        return {"url": url, "status": 200}

    assert fetch.is_async
    llm = adk.MockLlm([{"tool": "fetch", "args": {"url": "x"}}, "fetched"])
    result = await adk.Agent("a", tools=[fetch], llm=llm).ask_async("go")
    assert result == "fetched"
    assert seen["loop"] is loop
    assert '"status":200' in json.dumps(llm.requests()[1], separators=(",", ":"))


def test_async_tool_outside_a_loop_fails_clearly():
    @adk.tool
    async def fetch() -> str:
        """Needs a loop."""
        return "never"

    llm = adk.MockLlm([{"tool": "fetch", "args": {}}, "after"])
    result = adk.Agent("a", tools=[fetch], llm=llm).chat().send_full("go")
    assert "outside a running asyncio loop" in result["tool_calls"][0]["error"]


def test_tool_decorator_schema():
    @adk.tool(name="weather", description="Current weather.")
    def get_weather(city: str, units: str = "c", days: list[int] | None = None) -> dict:
        """Ignored: the decorator was given a description.

        Args:
            city: The city name.
        """
        return {}

    assert get_weather.name == "weather"
    assert get_weather.description == "Current weather."
    schema = get_weather.parameters
    assert schema["required"] == ["city"]
    assert schema["properties"]["city"] == {"type": "string", "description": "The city name."}
    assert schema["properties"]["days"] == {"anyOf": [{"type": "array", "items": {"type": "integer"}}, {"type": "null"}]}


def test_tool_needs_a_callable():
    with pytest.raises(TypeError):
        adk.Tool(42, name="x", description="y")  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        adk.Agent("a", tools=["not a tool"], llm=adk.MockLlm())  # type: ignore[list-item]


def test_mock_reply_shapes_are_checked():
    with pytest.raises(TypeError):
        adk.MockLlm([42])  # type: ignore[list-item]
    with pytest.raises(ValueError):
        adk.MockLlm([{"args": {}}])


async def test_cancelled_ask_does_not_hang():
    """Cancelling the Python task drops the Rust future."""
    agent = adk.Agent("a", llm=adk.MockLlm(["x"] * 3))
    task = asyncio.ensure_future(agent.ask_async("go"))
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert await agent.ask_async("again") == "x"


def test_no_credentials_is_a_config_or_llm_error(monkeypatch):
    for var in ("GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GENAI_USE_VERTEXAI", "GOOGLE_CLOUD_PROJECT"):
        monkeypatch.delenv(var, raising=False)
    with pytest.raises((adk.LlmError, adk.ConfigError)):
        adk.Agent("real")


def test_fork_after_init_is_refused():
    os = pytest.importorskip("os")
    if not hasattr(os, "fork"):
        pytest.skip("no fork()")
    adk.Agent("a", llm=adk.MockLlm(["warm"])).ask("start the runtime")
    pid = os.fork()
    if pid == 0:  # child
        code = 1
        try:
            adk.Agent("a", llm=adk.MockLlm(["x"])).ask("in the child")
        except RuntimeError as e:
            code = 0 if "fork" in str(e) else 2
        except BaseException:
            code = 3
        os._exit(code)
    _, status = os.waitpid(pid, 0)
    assert os.WEXITSTATUS(status) == 0


def test_typed_answer_checks_the_top_level_type():
    """A scalar reply does not satisfy an object schema: it is sent back
    once, then reported, instead of being returned as-is."""
    llm = adk.MockLlm(["42", '{"n": 42}'])
    assert adk.Agent("a", llm=llm).ask("?", schema={"type": "object"}) == {"n": 42}
    assert llm.call_count() == 2
    llm = adk.MockLlm(['{"n": 1}', '{"n": 2}'])
    with pytest.raises(adk.InvalidOutputError) as info:
        adk.Agent("a", llm=llm).ask("?", schema={"type": "object", "required": ["name"]})
    assert "name" in info.value.reason
    assert info.value.raw == '{"n": 2}'


def test_error_attributes_from_the_design():
    with pytest.raises(adk.LlmError) as info:
        ext._raise("Llm")
    assert info.value.kind == "Other"
    with pytest.raises(adk.SessionError) as info:
        ext._raise("Session")
    assert info.value.kind == "NotConnected"
    with pytest.raises(adk.InvalidOutputError) as info:
        ext._raise("InvalidOutput")
    assert (info.value.reason, info.value.raw) == ("why", "raw")


async def test_blocking_entry_point_from_a_running_loop_is_an_error():
    """`asyncio.run`'s rule: a blocking call on the loop thread would stall
    every coroutine (including an async tool), so it raises instead."""
    agent = adk.Agent("a", llm=adk.MockLlm(["x"]))
    with pytest.raises(RuntimeError, match="running asyncio loop"):
        agent.ask("hi")
    assert await agent.ask_async("hi") == "x"


def test_callable_object_with_async_call_is_async():
    class Fetcher:
        async def __call__(self, url: str) -> str:
            return url

    t = adk.Tool(Fetcher(), name="fetch", description="fetch")
    assert t.is_async


def test_sync_tool_returning_an_awaitable_is_reported():
    async def inner() -> str:
        return "late"

    @adk.tool
    def wrong() -> str:
        """Returns a coroutine by mistake."""
        return inner()  # type: ignore[return-value]

    llm = adk.MockLlm([{"tool": "wrong", "args": {}}, "after"])
    result = adk.Agent("a", tools=[wrong], llm=llm).chat().send_full("go")
    assert "returned an awaitable" in result["tool_calls"][0]["error"]


def test_schema_for_covers_the_hint_shapes():
    import functools

    from typing import Annotated, Literal, Optional

    pydantic = pytest.importorskip("pydantic")

    class Address(pydantic.BaseModel):
        city: str

    class Person(pydantic.BaseModel):
        name: str
        address: Address

    def f(
        mode: Literal["fast", "slow"],
        level: Literal[1, 2],
        tags: tuple[str, ...],
        pair: tuple[int, str],
        note: Annotated[int, pydantic.Field(description="a note")],
        maybe: Optional[str],
        who: Person,
    ) -> None: ...

    props = adk.schema_for(f)["properties"]
    assert props["mode"] == {"type": "string", "enum": ["fast", "slow"]}
    assert props["level"] == {"type": "integer", "enum": [1, 2]}
    assert props["tags"] == {"type": "array", "items": {"type": "string"}}
    assert props["pair"] == {"type": "array"}
    assert props["note"] == {"type": "integer", "description": "a note"}
    assert props["maybe"] == {"anyOf": [{"type": "string"}, {"type": "null"}]}
    assert "$ref" not in json.dumps(props["who"]) and "$defs" not in props["who"]
    assert props["who"]["properties"]["address"]["properties"]["city"] == {"type": "string", "title": "City"}

    class K:
        def m(self, x: int) -> int:
            return x

    with pytest.raises(TypeError, match="bound method"):
        adk.tool(K.m)
    assert adk.tool(K().m).parameters["required"] == ["x"]
    with pytest.raises(TypeError, match="name="):
        adk.tool(functools.partial(lambda a, b: a + b, 1))
    with pytest.raises(TypeError, match="name="):
        adk.tool(lambda a: a)
    assert adk.tool(name="add_one")(lambda a: a + 1).name == "add_one"


def test_exactly_one_runtime():
    """Every surface shares one Tokio runtime: exercising agents, sims and
    Live sessions a second time adds no worker threads (a second runtime
    would add min(4, cores) of them)."""
    import os
    import pathlib

    tasks = pathlib.Path("/proc/self/task")
    if not tasks.is_dir():
        pytest.skip("needs /proc")

    def exercise():
        adk.Agent("a", llm=adk.MockLlm(["x"])).ask("warm")
        adk.Conversation({"name": "c", "stages": [{"id": "s", "terminal": True}], "require": ["s"]}).sim().snapshot()
        session = adk.scripted([{"serverContent": {"turnComplete": True}}])
        session.events().next_timeout(5.0)
        session.close()

    def workers():
        names = [p.joinpath("comm").read_text().strip() for p in tasks.iterdir()]
        return sum(1 for n in names if n.startswith("gemini-adk-py"))

    exercise()
    before = workers()
    assert before >= 1
    for _ in range(3):
        exercise()
    assert workers() - before < min(4, os.cpu_count() or 4)


def test_atexit_closes_sessions():
    """A program that exits with an open session and a pending event
    awaitable ends quietly: no Rust panic from a worker after
    finalisation."""
    program = """
import asyncio, gemini_adk as adk
async def main():
    s = await adk.scripted_async([{"serverContent": {"turnComplete": True}}], released=False)
    ev = s.events()
    asyncio.ensure_future(ev.__anext__())
    await asyncio.sleep(0.05)
asyncio.run(main())
print("done")
"""
    proc = subprocess.run([sys.executable, "-I", "-c", program], capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == "done"
    assert "panicked" not in proc.stderr, proc.stderr

