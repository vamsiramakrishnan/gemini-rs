"""§1 and §3: Live sessions over a scripted server, the lane invariants and
the backpressure policy."""

import asyncio
import threading
import time

import pytest

import gemini_adk as adk
from gemini_adk import _gemini_adk as ext


def drain(events, *, until="turn_complete", limit=10.0):
    out = []
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        try:
            event = events.next_timeout(0.5)
        except adk.TimeoutError:
            continue
        if event is None:  # the stream ended
            break
        out.append(event)
        if event["kind"] == until:
            break
    return out


def test_scripted_session_delivers_audio_as_bytes(frames):
    payloads = [bytes([i % 256]) * 320 for i in range(5)]
    session = adk.scripted(
        [frames.audio(p) for p in payloads] + [frames.text("hello"), frames.turn_complete()],
        released=False,
    )
    with session:
        events = session.events()
        session.release()
        got = drain(events)
    audio = [e["data"] for e in got if e["kind"] == "audio"]
    assert audio == payloads
    assert all(isinstance(a, bytes) for a in audio)
    assert [e["text"] for e in got if e["kind"] == "text"] == ["hello"]
    assert got[-1]["kind"] == "turn_complete"


def test_fast_lane_never_sees_python(frames):
    """The thread the `on_audio` fast-lane callback runs on is never one a
    Python tool or the Python consumer runs on."""
    python_threads = set()

    @adk.tool
    def note(x: int) -> str:
        """Record the tool's thread."""
        python_threads.add(ext._thread_token())
        return "noted"

    script = [frames.audio(b"\0" * 640) for _ in range(500)]
    script.insert(250, frames.tool_call("c1", "note", {"x": 1}))
    script.append(frames.turn_complete())
    session = adk.scripted(script, tools=[note], released=False, _probe=True)
    with session:
        events = session.events()
        session.release()
        for event in drain(events, limit=20.0):
            python_threads.add(ext._thread_token())
            if event["kind"] == "tool":
                assert event["result"] == "noted"
    probe = session._audio_probe()
    assert probe is not None
    assert probe.frames() == 500
    assert probe.threads()
    assert not (set(probe.threads()) & python_threads)


def test_slow_consumer_does_not_stall_fast_lane(frames):
    """A consumer that sleeps per event loses the oldest events for itself
    alone, is told how many, and the fast lane's pacing is unaffected."""
    total = 500
    script = [frames.audio(bytes([i % 256]) * 2) for i in range(total)] + [frames.turn_complete()]
    session = adk.scripted(script, event_capacity=16, released=False, _probe=True)
    with session:
        events = session.events(buffer=8)
        session.release()
        time.sleep(0.3)  # let the whole script arrive before reading
        received = 0
        dropped = 0
        lagged_events = 0
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                event = events.next_timeout(0.5)
            except adk.TimeoutError:
                continue
            if event is None:
                break
            if event["kind"] == "audio":
                received += 1
                time.sleep(0.002)
            elif event["kind"] == "lagged":
                lagged_events += 1
                dropped += event["dropped"]
            elif event["kind"] == "turn_complete":
                break
    probe = session._audio_probe()
    assert probe.frames() == total, "the fast lane saw every frame"
    assert lagged_events >= 1 and dropped > 0
    assert received + dropped == total
    gaps = sorted(probe.gaps_ms())
    p99 = gaps[int(len(gaps) * 0.99) - 1]
    assert p99 < 50.0, f"fast-lane inter-arrival p99 {p99:.2f} ms"


def test_lagged_event_counts_every_drop_in_order(frames):
    total = 2000
    script = [frames.audio(i.to_bytes(2, "little")) for i in range(total)] + [frames.turn_complete()]
    session = adk.scripted(script, event_capacity=32, released=False)
    with session:
        events = session.events(buffer=4)
        session.release()
        time.sleep(0.5)
        seen = []
        dropped = 0
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                event = events.next_timeout(0.5)
            except adk.TimeoutError:
                continue
            if event is None:
                break
            if event["kind"] == "audio":
                seen.append(int.from_bytes(event["data"], "little"))
            elif event["kind"] == "lagged":
                dropped += event["dropped"]
            elif event["kind"] == "turn_complete":
                break
    assert len(seen) + dropped == total
    assert seen == sorted(seen), "events arrive in order"


def test_two_consumers_lag_independently(frames):
    total = 300
    script = [frames.audio(b"ab") for _ in range(total)] + [frames.turn_complete()]
    session = adk.scripted(script, event_capacity=16, released=False)
    with session:
        fast = session.events(buffer=4096)
        slow = session.events(buffer=2)
        session.release()
        fast_got = [e for e in drain(fast, limit=20.0) if e["kind"] == "audio"]
        time.sleep(0.3)
        slow_audio = 0
        slow_dropped = 0
        for e in drain(slow, limit=20.0):
            if e["kind"] == "audio":
                slow_audio += 1
            elif e["kind"] == "lagged":
                slow_dropped += e["dropped"]
    assert len(fast_got) == total
    assert slow_audio + slow_dropped == total


def test_events_after_close_end_the_iterator(frames):
    session = adk.scripted([frames.text("bye"), frames.turn_complete()])
    events = session.events()
    drain(events)
    session.close()
    rest = list(events)  # ends after the final `disconnected` event
    assert rest[-1]["kind"] == "disconnected"
    assert events.next_timeout(0.5) is None  # ended, not timed out
    # A stream opened after the close ends at once instead of waiting for
    # a `disconnected` event that was broadcast before it subscribed.
    assert list(session.events()) == []


async def test_async_iteration_ends_after_close(frames):
    session = await adk.scripted_async([frames.turn_complete()])
    events = session.events()
    await session.close_async()
    kinds = [e["kind"] async for e in events]
    assert kinds[-1] == "disconnected"


def test_send_after_close_is_session_closed(frames):
    session = adk.scripted([frames.turn_complete()])
    session.close()
    with pytest.raises(adk.SessionClosedError):
        session.send_text("hello?")


async def test_async_iteration_and_async_sends(frames):
    session = await adk.scripted_async([frames.text("hi"), frames.turn_complete()], released=False)
    events = session.events()
    session.release()
    kinds = []
    async for event in events:
        kinds.append(event["kind"])
        if event["kind"] == "turn_complete":
            break
    assert "text" in kinds
    await session.send_text_async("thanks")
    await session.send_audio_async(b"\0\0" * 160)
    await session.close_async()


def test_tool_events_are_mapped(frames):
    @adk.tool
    def get_time() -> str:
        """The time."""
        return "noon"

    script = [frames.tool_call("c1", "get_time", {}), frames.text("ok"), frames.turn_complete()]
    session = adk.scripted(script, tools=[get_time], released=False)
    with session:
        events = session.events()
        session.release()
        got = drain(events)
    by_kind = {e["kind"]: e for e in got}
    assert by_kind["tool"]["name"] == "get_time" and by_kind["tool"]["result"] == "noon"


def test_go_away_is_mapped(frames):
    session = adk.scripted([frames.go_away("30s"), frames.turn_complete()], released=False)
    with session:
        events = session.events()
        session.release()
        got = drain(events, until="go_away")
    assert got[-1] == {"kind": "go_away", "time_left_ms": 30_000}


async def test_async_tool_is_cancelled_on_barge_in(frames):
    """An `interrupted` frame (the user barged in) cancels the in-flight
    async Python tool's asyncio task; no response is sent for it."""
    started = asyncio.Event()
    outcome = {}

    @adk.tool
    async def slow(x: int) -> str:
        """Take a long time."""
        started.set()
        try:
            await asyncio.sleep(30)
        except asyncio.CancelledError:
            outcome["cancelled"] = True
            raise
        outcome["finished"] = True
        return "late"

    script = [frames.tool_call("c1", "slow", {"x": 1}), frames.interrupted(), frames.text("moving on"), frames.turn_complete()]
    session = await adk.scripted_async(script, tools=[slow], released=False)
    events = session.events()
    session.release()
    kinds = []
    async for event in events:
        kinds.append(event["kind"])
        if event["kind"] == "turn_complete":
            break
    await asyncio.wait_for(started.wait(), 5)
    for _ in range(50):
        if "cancelled" in outcome:
            break
        await asyncio.sleep(0.05)
    assert "interrupted" in kinds and "tool_cancelled" in kinds
    assert outcome == {"cancelled": True}
    await session.close_async()


async def test_server_tool_cancellation_is_surfaced(frames):
    """A `toolCallCancellation` frame reaches Python as a `tool_cancelled`
    event with the ids."""
    @adk.tool
    async def quick() -> str:
        """Finish at once."""
        return "done"

    script = [frames.tool_call("c1", "quick", {}), frames.tool_call_cancellation("c1"), frames.turn_complete()]
    session = await adk.scripted_async(script, tools=[quick], released=False)
    events = session.events()
    session.release()
    cancelled = []
    async for event in events:
        if event["kind"] == "tool_cancelled":
            cancelled.append(event["ids"])
        if event["kind"] == "turn_complete":
            break
    assert cancelled == [["c1"]]
    await session.close_async()


async def test_dropped_session_disconnects(frames):
    """A session that goes out of scope is disconnected on the runtime: its
    event stream ends with `disconnected`, and later sessions work."""
    session = await adk.scripted_async([frames.turn_complete()], released=False)
    events = session.events()
    session.release()
    del session
    kinds = []
    async for event in events:
        kinds.append(event["kind"])
    assert kinds[-1] == "disconnected"
    # Subscribe before the script plays: a broadcast event is only seen by
    # the subscribers that exist when it is sent.
    again = await adk.scripted_async([frames.turn_complete()], released=False)
    events = again.events()
    again.release()
    assert await events.__anext__() == {"kind": "turn_complete"}
    await again.close_async()


async def test_async_with_closes_the_session(frames):
    async with await adk.scripted_async([frames.turn_complete()], released=False) as session:
        events = session.events()
        session.release()
        assert (await events.__anext__())["kind"] == "turn_complete"
    with pytest.raises(adk.SessionClosedError):
        await session.send_text_async("after")


def test_next_timeout_distinguishes_timeout_from_end(frames):
    session = adk.scripted([frames.turn_complete()], released=False)
    events = session.events()
    with pytest.raises(adk.TimeoutError):
        events.next_timeout(0.2)
    with pytest.raises(TimeoutError):  # also the builtin
        events.next_timeout(0.1)
    session.release()
    assert drain(events)[-1]["kind"] == "turn_complete"
    session.close()
    assert drain(events, until="disconnected")[-1]["kind"] == "disconnected"
    assert events.next_timeout(0.1) is None


def test_state_and_resume_handle_default(frames):
    session = adk.scripted([frames.turn_complete()])
    with session:
        assert session.resume_handle() is None
        assert session.state("session:nothing") is None
