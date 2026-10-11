"""The trattoria booking agent on google-genai, taking calls on N phone lines
against trattoria_mock: the Python counterpart of src/bin/trattoria.rs.

The agent does what the Rust runtime does with the same spec
(crates/gemini-adk-fluent-rs/tests/fixtures/live/trattoria.json), written
the way a Python service would write it on google-genai:

- setup with the base instruction, every tool, Kore, input and output
  transcription, interrupting activity handling and context compression;
- the opening stage text as a model turn, then the greeting;
- at every turn complete: the verbatim check, the transcript turn, both
  extractors in parallel on the last three turns (google-genai
  generate_content with a JSON schema and a thinking budget), the promotion
  policies, the handoff overlay trigger, repair counters, the stage machine;
  then a contextUpdate when the offered tools changed, and the new stage
  text as model turns (lines already sent at the previous boundary dropped);
- tool calls through the same gate: once, never-until with a second chance
  that re-runs the caller-signal extractor on the turn in progress, the
  active stage's allow list; canned responses, saved to state; a
  contextUpdate before the response when the call moved the stage;
- while the verbatim stage is active and the model speaks, the mic sends
  silence (floor holding).

google-genai has no contextUpdate, so it goes out as raw JSON on the
session's WebSocket. The caller's mic is a 20 ms clock of 16 kHz audio for
the whole call, frames stamped with their capture time; model audio only has
its lateness noted. The caller hangs up a second after the turn that follows
the booking.

Prints one JSON line.
"""

import argparse
import asyncio
import json
import os
import resource
import struct
import time

RSS_START = None


def rss_kb():
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    return 0


RSS_START = rss_kb()

from google import genai  # noqa: E402
from google.genai import types  # noqa: E402

RSS_IMPORTS = rss_kb()

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC_PATH = os.path.join(
    HERE, "..", "..", "..", "crates", "gemini-adk-fluent-rs", "tests", "fixtures", "live", "trattoria.json"
)
LIVE_MODEL = "models/gemini-3.8-live"
MIC_FRAME_S = 0.02
MIC_FRAME_BYTES = 640  # 20 ms of 16 kHz mono PCM16
MIC_TAIL = bytes(i % 13 for i in range(8, MIC_FRAME_BYTES))
SILENCE = bytes(MIC_FRAME_BYTES)
WINDOW = 3
MIN_WORDS = 3
VERBATIM_MIN_SIMILARITY = 0.9

NULL_GUIDANCE = (
    "\n\nUse null for any field the transcript does not state. "
    'Never guess, and never use placeholders such as "unknown", empty strings or zero.'
)
KNOWN_GUIDANCE = (
    "\n\nSome fields are already known (listed before the transcript). "
    "Compare each known field only with the user's latest turn, quoted after the transcript: "
    "return it only if that turn gives a different value. Agreeing, confirming, repeating or "
    "rewording a known value is not a change: return null."
)
VERBATIM_LEAD = (
    "\n\nSay the following exactly as written, word for word. "
    "Do not paraphrase, add to or leave out any part of it:\n\n"
)


def cpu_s():
    r = resource.getrusage(resource.RUSAGE_SELF)
    return r.ru_utime + r.ru_stime


# ---------------------------------------------------------------------------
# Verbatim matching: word edit distance, the text allowed anywhere in the turn
# ---------------------------------------------------------------------------


def _words(s):
    out = []
    for w in s.split():
        w = "".join(c.lower() for c in w if c.isalnum())
        if w:
            out.append(w)
    return out


def similarity(expected, heard):
    a, b = _words(expected), _words(heard)
    longest = max(len(a), len(b))
    if longest == 0:
        return 1.0
    prev = list(range(len(b) + 1))
    for i, wa in enumerate(a):
        row = [i + 1] * (len(b) + 1)
        for j, wb in enumerate(b):
            row[j + 1] = min(prev[j] + (wa != wb), prev[j + 1] + 1, row[j] + 1)
        prev = row
    return 1.0 - prev[len(b)] / longest


def contained_similarity(expected, heard):
    want, got = _words(expected), _words(heard)
    n = len(want)
    if n == 0 or len(got) <= n:
        return similarity(expected, heard)
    slack = max(n // 10, 1)
    want_s = " ".join(want)
    best = 0.0
    for length in range(max(n - slack, 1), min(n + slack, len(got)) + 1):
        for start in range(0, len(got) - length + 1):
            best = max(best, similarity(want_s, " ".join(got[start : start + length])))
            if best >= 1.0:
                return best
    return best


# ---------------------------------------------------------------------------
# Extraction
# ---------------------------------------------------------------------------


def nullable(schema):
    schema = json.loads(json.dumps(schema))
    for prop in schema.get("properties", {}).values():
        t = prop.get("type")
        if isinstance(t, str) and t != "null":
            prop["type"] = [t, "null"]
        elif isinstance(t, list) and "null" not in t:
            t.append("null")
    return schema


def equivalent(a, b):
    if isinstance(a, str) and isinstance(b, str):
        return a.strip().lower() == b.strip().lower()
    return a == b


class Extractor:
    def __init__(self, spec, model, budget):
        self.name = spec["name"]
        self.instruction = spec["instruction"]
        self.schema = nullable(spec["schema"])
        self.model = model
        self.budget = budget
        self.rules = []
        for r in spec.get("promote", []):
            self.rules.append(
                {
                    "field": r["field"],
                    "key": r.get("to", r["field"]),
                    "policy": r.get("policy", "keep_known"),
                }
            )

    def writes(self, key):
        return any(r["key"] == key for r in self.rules)

    def should_extract(self, window):
        if window and not window[-1]["user"].strip() and any(t["user"].strip() for t in window):
            return False
        for t in reversed(window):
            if t["user"]:
                return len(t["user"].split()) >= MIN_WORDS
        return False

    async def run(self, client, window, state):
        known = {}
        for r in self.rules:
            v = state.get(r["key"])
            if v is not None:
                known[r["field"]] = v
        transcript = ""
        for t in window:
            if t["user"]:
                transcript += "User: " + t["user"].strip() + "\n"
            if t["model"]:
                transcript += "Assistant: " + t["model"].strip() + "\n"
            transcript += "\n"
        preamble = latest = guidance = ""
        if known:
            last_user = next((t["user"].strip() for t in reversed(window) if t["user"].strip()), "")
            preamble = "Already known: " + json.dumps(known, separators=(",", ":")) + "\n\n"
            latest = "The user's latest turn: " + json.dumps(last_user) + "\n\n"
            guidance = KNOWN_GUIDANCE
        response = await client.aio.models.generate_content(
            model=self.model,
            contents=f"{preamble}Transcript:\n{transcript}\n{latest}Extract the requested information.",
            config=types.GenerateContentConfig(
                system_instruction=self.instruction + NULL_GUIDANCE + guidance,
                response_mime_type="application/json",
                response_json_schema=self.schema,
                thinking_config=types.ThinkingConfig(thinking_budget=self.budget),
            ),
        )
        return json.loads(response.text)

    def promote(self, value, state):
        """Apply the promotion policies; return the keys written."""
        state[self.name] = value
        written = []
        if not isinstance(value, dict):
            return written
        for r in self.rules:
            if r["field"] not in value:
                continue
            v = value[r["field"]]
            key = r["key"]
            if v is None:
                continue
            if key in state and equivalent(state[key], v):
                continue
            if r["policy"] == "true_only" and v is not True:
                continue
            if r["policy"] == "keep_known" and key in state:
                continue
            state[key] = v
            state["state_meta:" + key] = {"source": "extraction", "extractor": self.name, "field": r["field"]}
            written.append(key)
        return written


# ---------------------------------------------------------------------------
# The conversation: stages, overlay, gate
# ---------------------------------------------------------------------------


def holds(cond, state, called):
    if "captured" in cond:
        return all(state.get(k) is not None for k in cond["captured"])
    if "called_ok" in cond:
        return cond["called_ok"] in called
    if "is_true" in cond:
        return state.get(cond["is_true"]) is True
    return False


def render(template, state):
    out, i = "", 0
    while i < len(template):
        if template[i] == "{":
            j = template.index("}", i)
            v = state.get(template[i + 1 : j])
            out += "" if v is None else (v if isinstance(v, str) else json.dumps(v))
            i = j + 1
        else:
            out += template[i]
            i += 1
    return out


class Conversation:
    def __init__(self, conv, tool_names):
        self.stages = {s["id"]: s for s in conv["stages"]}
        self.require = conv.get("require", [])
        self.overlays = conv.get("overlays", [])
        self.tool_names = sorted(tool_names)
        self.entered = [conv["stages"][0]["id"]]
        self.completed = set()
        self.called = set()  # tools that ran successfully
        self.ran = set()
        self.repair = {}
        self.overlay = None  # {"spec", "done": set(), "called": set()}
        self.overlay_finished = False
        self.terminated = False
        self.slots_at_collect = None
        # Commit constraints: once(tool) and never(tool).until(guard).
        self.once = set()
        self.guards = {}
        for s in conv["stages"]:
            c = s.get("commit")
            if c:
                self.once.add(c["tool"])
                self.guards[c["tool"]] = c["when"]

    def allow_of(self, stage):
        allow = list(stage.get("allow", []))
        if stage.get("commit") and stage["commit"]["tool"] not in allow:
            allow.append(stage["commit"]["tool"])
        return allow if ("allow" in stage or stage.get("commit")) else None

    def active(self):
        return [s for s in self.entered if s not in self.completed]

    def active_overlay_steps(self):
        if not self.overlay:
            return []
        return [s for s in self.overlay["spec"]["stages"] if s["id"] not in self.overlay["done"]]

    def offered(self):
        if self.terminated:
            return []
        steps = self.active_overlay_steps() if self.overlay else [self.stages[s] for s in self.active()]
        allows = [self.allow_of(s) for s in steps]
        allows = [a for a in allows if a is not None]
        names = sorted({n for a in allows for n in a}) if allows else list(self.tool_names)
        return [n for n in names if not (n in self.once and n in self.ran)]

    def gate(self, name, state):
        """None when the call may run, else (reason, kind)."""
        if self.terminated:
            return (f"'{name}' is not available: the conversation has ended.", "ended")
        if name in self.once and name in self.ran:
            return (
                f"'{name}' has already run and may run only once in this conversation. Do not call it again.",
                "once",
            )
        guard = self.guards.get(name)
        if guard and not holds(guard, state, self.called):
            key = guard.get("is_true", "")
            return (f"'{name}' is not permitted yet — first, '{key}' must be true.", "guard")
        steps = self.active_overlay_steps() if self.overlay else [self.stages[s] for s in self.active()]
        allows = [self.allow_of(s) for s in steps]
        allows = [a for a in allows if a is not None]
        if allows and not any(name in a for a in allows):
            current = steps[0]["id"] if steps else ""
            return (f"'{name}' is not part of the current step ('{current}').", "allow")
        return None

    def denial(self, head, state):
        offered = self.offered()
        text = head + " It did not run, so do not tell the caller it is done."
        if offered:
            text += " Available now: " + ", ".join(offered) + "."
        else:
            text += " No tool is available right now — continue the conversation instead."
        postures = self.postures(state)
        if postures:
            text += " " + postures[0]
        return {"error": text}

    def observe(self, name):
        self.ran.add(name)
        self.called.add(name)
        if self.overlay:
            self.overlay["called"].add(name)

    def relatch(self, state):
        if self.overlay:
            for step in self.active_overlay_steps():
                if holds(step["done"], state, self.overlay["called"]):
                    self.overlay["done"].add(step["id"])
            return
        changed = True
        while changed:
            changed = False
            for sid in self.active():
                stage = self.stages[sid]
                if stage.get("terminal"):
                    self.completed.add(sid)
                    changed = True
                    continue
                if stage.get("verbatim") and state.get(f"verbatim:{sid}") is not True:
                    continue
                target = None
                if state.get(f"repair:{sid}:escalate") is True and "repair" in stage:
                    target = stage["repair"]["escalate_to"]
                for edge in stage.get("next", []):
                    if target is None and holds(edge["when"], state, self.called):
                        target = edge["to"]
                if target is not None:
                    self.completed.add(sid)
                    if sid == "collect":
                        self.slots_at_collect = {k: state.get(k) for k in stage.get("collect", [])}
                    if target not in self.entered:
                        self.entered.append(target)
                    changed = True

    def govern(self, state, caller_spoke, previous_had_tools, first_turn):
        # A corrected slot reopens what followed collecting it.
        if self.slots_at_collect is not None:
            now = {k: state.get(k) for k in self.slots_at_collect}
            if now != self.slots_at_collect:
                for sid in ("offer", "confirm", "done", "handoff"):
                    self.completed.discard(sid)
                    if sid in self.entered:
                        self.entered.remove(sid)
                state.pop("book_table_confirmed", None)
                self.slots_at_collect = now
                self.entered.append("offer")
        # The overlay: enter on its trigger; when finished, terminate.
        if self.overlay and not self.active_overlay_steps():
            self.terminated = True
            state["flow:terminated"] = True
            self.overlay = None
            self.overlay_finished = True
        elif not self.overlay and not self.overlay_finished:
            for o in self.overlays:
                if holds(o["trigger"], state, self.called):
                    self.overlay = {"spec": o, "done": set(), "called": set()}
                    break
        # Repair: turns a stage stays active without completing.
        if not first_turn and (caller_spoke or not previous_had_tools) and not self.overlay:
            for sid in self.active():
                stage = self.stages[sid]
                if "repair" not in stage:
                    continue
                n = self.repair.get(sid, 0) + 1
                self.repair[sid] = n
                if n >= stage["repair"]["reprompt_after"]:
                    state[f"repair:{sid}:reprompt"] = True
                if n >= stage["repair"]["escalate_after"]:
                    state[f"repair:{sid}:escalate"] = True
        self.relatch(state)

    def verbatim(self):
        """(stage, text) of the active verbatim requirement."""
        if self.overlay:
            return None
        for sid in self.active():
            v = self.stages[sid].get("verbatim")
            if v:
                return sid, v
        return None

    def holds_floor(self):
        return any(
            self.stages[sid].get("verbatim") and not self.stages[sid].get("timing") for sid in self.active()
        ) and not self.overlay

    def postures(self, state):
        if self.terminated:
            return []
        steps = self.active_overlay_steps() if self.overlay else [self.stages[s] for s in self.active()]
        out = []
        for s in steps:
            text = s.get("say", "")
            if s.get("verbatim"):
                text += VERBATIM_LEAD + s["verbatim"]
            if text:
                out.append(text)
        return out

    def lines(self, state):
        """Stage text for the model: postures, grounds, what is still owed."""
        if self.terminated:
            return []
        out = self.postures(state)
        if not self.overlay:
            for sid in self.active():
                g = self.stages[sid].get("ground")
                if g:
                    out.append(render(g, state))
            missing = [r for r in self.require if r not in self.completed]
        else:
            missing = [s["id"] for s in self.active_overlay_steps()]
        if missing and (self.active() or self.overlay):
            out.append("Before finishing, these still need to happen: " + ", ".join(missing) + ".")
        return out


# ---------------------------------------------------------------------------
# One call
# ---------------------------------------------------------------------------


class Totals:
    def __init__(self):
        self.calls = self.booked = self.failed = 0
        self.errors = {}
        self.audio_late_ms = []


class Call:
    def __init__(self, ctx):
        self.ctx = ctx
        spec = ctx.spec
        self.state = {}
        self.tools = {t["name"]: t for t in spec["tools"]}
        self.conv = Conversation(spec["conversation"], self.tools)
        self.extractors = ctx.extractors
        self.turns = []
        self.user = ""
        self.model = ""
        self.turn_tools = 0
        self.previous_had_tools = True
        self.first_turn = True
        self.model_speaking = False
        self.last_lines = []
        self.declared = self.conv.offered()
        self.queue = asyncio.Queue()
        self.hangup = asyncio.Event()
        self.session = None

    def declaration(self, name):
        t = self.tools[name]
        d = {"name": name, "description": t.get("description", "")}
        if "parameters" in t:
            d["parameters"] = t["parameters"]
        return d

    def connect_config(self):
        spec = self.ctx.spec
        return types.LiveConnectConfig(
            response_modalities=["AUDIO"],
            system_instruction=spec["instruction"],
            speech_config=types.SpeechConfig(
                voice_config=types.VoiceConfig(prebuilt_voice_config=types.PrebuiltVoiceConfig(voice_name=spec["voice"]))
            ),
            tools=[
                types.Tool(
                    function_declarations=[
                        types.FunctionDeclaration(
                            name=n,
                            description=self.tools[n].get("description", ""),
                            parameters_json_schema=self.tools[n].get("parameters"),
                        )
                        for n in self.declared
                    ]
                )
            ],
            input_audio_transcription=types.AudioTranscriptionConfig(),
            output_audio_transcription=types.AudioTranscriptionConfig(),
            realtime_input_config=types.RealtimeInputConfig(
                activity_handling=types.ActivityHandling.START_OF_ACTIVITY_INTERRUPTS,
                turn_coverage=types.TurnCoverage.TURN_INCLUDES_ONLY_ACTIVITY,
            ),
            context_window_compression=types.ContextWindowCompressionConfig(
                trigger_tokens=100000, sliding_window=types.SlidingWindow(target_tokens=50000)
            ),
        )

    async def say_to_model(self, lines):
        await self.session.send_client_content(
            turns=[types.Content(role="model", parts=[types.Part(text=line)]) for line in lines],
            turn_complete=False,
        )

    async def open(self):
        opening = self.conv.postures(self.state)
        for sid in self.conv.active():
            g = self.conv.stages[sid].get("ground")
            if g:
                opening.append(render(g, self.state))
        if opening:
            await self.say_to_model(opening)
        self.last_lines = opening
        await self.session.send_client_content(
            turns=types.Content(role="user", parts=[types.Part(text=self.ctx.spec["greeting"])]),
            turn_complete=True,
        )

    async def rescope(self):
        offered = self.conv.offered()
        if offered != self.declared:
            self.declared = offered
            update = {"contextUpdate": {"tools": {"tools": [{"functionDeclarations": [self.declaration(n) for n in offered]}]}}}
            await self.session._ws.send(json.dumps(update))

    # The mic: a frame every 20 ms; a late loop sends every frame it owes.
    async def mic(self):
        loop = asyncio.get_running_loop()
        t0 = loop.time()
        start_us = int(time.time() * 1e6)
        n = 0
        while True:
            delay = t0 + n * MIC_FRAME_S - loop.time()
            if delay > 0:
                await asyncio.sleep(delay)
            if self.model_speaking and self.conv.holds_floor():
                frame = SILENCE
            else:
                frame = struct.pack("<Q", start_us + n * 20000) + MIC_TAIL
            n += 1
            await self.session.send_realtime_input(audio=types.Blob(data=frame, mime_type="audio/pcm;rate=16000"))

    # The fast lane: everything the server sends.
    async def receive(self):
        late = self.ctx.totals.audio_late_ms
        while True:
            async for msg in self.session.receive():
                sc = msg.server_content
                if sc is not None:
                    if sc.input_transcription and sc.input_transcription.text:
                        self.user += sc.input_transcription.text
                    if sc.output_transcription and sc.output_transcription.text:
                        self.model += sc.output_transcription.text
                    if sc.model_turn and sc.model_turn.parts:
                        for part in sc.model_turn.parts:
                            blob = part.inline_data
                            if blob is not None and blob.data:
                                self.model_speaking = True
                                sent = struct.unpack_from("<Q", blob.data)[0]
                                late.append(time.time() * 1000 - sent / 1000)
                    if sc.interrupted:
                        self.model_speaking = False
                    if sc.turn_complete:
                        self.model_speaking = False
                        self.queue.put_nowait(("turn", None))
                if msg.tool_call is not None and msg.tool_call.function_calls:
                    self.turn_tools += len(msg.tool_call.function_calls)
                    self.queue.put_nowait(("tools", msg.tool_call.function_calls))

    # The control lane: turn boundaries and tool calls, in order.
    async def control(self):
        while True:
            kind, payload = await self.queue.get()
            if kind == "turn":
                await self.turn_boundary()
                if "booking" in self.state:
                    await asyncio.sleep(1.0)
                    self.hangup.set()
                    return
            else:
                await self.tool_calls(payload)

    async def extract(self, extractors, window):
        runnable = [e for e in extractors if e.should_extract(window)]
        if not runnable:
            return []
        results = await asyncio.gather(
            *(e.run(self.ctx.client, window, self.state) for e in runnable), return_exceptions=True
        )
        written = []
        for e, r in zip(runnable, results):
            if isinstance(r, Exception):
                continue
            written += e.promote(r, self.state)
        return written

    async def turn_boundary(self):
        # The verbatim check on what the model said this turn.
        v = self.conv.verbatim()
        if v and self.model.strip():
            sid, text = v
            passed = contained_similarity(text, self.model) >= VERBATIM_MIN_SIMILARITY
            flag = f"verbatim:{sid}"
            if passed or self.state.get(flag) is not True:
                self.state[flag] = passed
        caller_spoke = bool(self.user.strip())
        self.turns.append({"user": self.user, "model": self.model})
        self.user = self.model = ""
        had_tools, self.turn_tools = self.turn_tools > 0, 0
        await self.extract(self.extractors, self.turns[-WINDOW:])
        self.conv.govern(self.state, caller_spoke, self.previous_had_tools, self.first_turn)
        self.first_turn = False
        self.previous_had_tools = had_tools
        await self.rescope()
        lines = self.conv.lines(self.state)
        new = [line for line in lines if line not in self.last_lines]
        self.last_lines = lines
        if new:
            await self.say_to_model(new)

    async def tool_calls(self, calls):
        responses = []
        for call in calls:
            name = call.name
            denied = self.conv.gate(name, self.state)
            if denied and denied[1] == "guard" and name in self.conv.offered():
                # A second chance: decide the guard on the turn in progress.
                guard_key = self.conv.guards[name].get("is_true")
                window = self.turns[-(WINDOW - 1) :] + [{"user": self.user, "model": self.model}]
                await self.extract([e for e in self.extractors if e.writes(guard_key)], window)
                denied = self.conv.gate(name, self.state)
            if denied:
                result = self.conv.denial(denied[0], self.state)
            else:
                tool = self.tools[name]
                result = tool.get("response", {"ok": True})
                if not isinstance(result, dict):
                    result = {"output": result}
                if tool.get("save_response_as"):
                    self.state[tool["save_response_as"]] = result
                self.conv.observe(name)
                self.conv.relatch(self.state)
                await self.rescope()
            responses.append(types.FunctionResponse(id=call.id, name=name, response=result))
        await self.session.send_tool_response(function_responses=responses)


# ---------------------------------------------------------------------------
# Lines and the run
# ---------------------------------------------------------------------------


class Context:
    def __init__(self, port):
        self.spec = json.load(open(SPEC_PATH))
        models = self.spec.get("models", {})
        model = models.get("extraction", "gemini-3.5-flash-lite")
        budget = models.get("extraction_thinking_budget")
        self.extractors = [Extractor(e, model, budget) for e in self.spec.get("extract", [])]
        # One client for every call: REST to the stand-in, Live over plain ws.
        self.client = genai.Client(api_key="bench", http_options=types.HttpOptions(base_url=f"http://127.0.0.1:{port}"))
        self.client._api_client._websocket_base_url = lambda: f"ws://127.0.0.1:{port}"
        self.client._api_client._websocket_ssl_ctx = {}
        self.totals = Totals()


async def one_call(ctx):
    c = Call(ctx)
    async with ctx.client.aio.live.connect(model=LIVE_MODEL, config=c.connect_config()) as session:
        c.session = session
        await c.open()
        tasks = [asyncio.create_task(c.mic()), asyncio.create_task(c.control())]
        receiver = asyncio.create_task(c.receive())
        hung_up = asyncio.create_task(c.hangup.wait())
        done, _ = await asyncio.wait([receiver, hung_up], timeout=120, return_when=asyncio.FIRST_COMPLETED)
        for t in tasks + [receiver, hung_up]:
            t.cancel()
        if hung_up in done:
            return True
        if receiver in done and receiver.exception() is not None:
            raise receiver.exception()
        if not done:
            raise TimeoutError("call timed out")
        return False


async def line(ctx, start_after, until):
    await asyncio.sleep(start_after)
    t = ctx.totals
    while time.monotonic() < until:
        t.calls += 1
        try:
            if await one_call(ctx):
                t.booked += 1
            else:
                t.failed += 1
        except Exception as e:  # noqa: BLE001
            t.failed += 1
            key = type(e).__name__ + ": " + str(e)[:80]
            t.errors[key] = t.errors.get(key, 0) + 1
            await asyncio.sleep(1.0)


def quantiles(v):
    if not v:
        return None
    v.sort()

    def q(p):
        return round(v[min(len(v) - 1, int(p * len(v)))], 1)

    return {"n": len(v), "p50": q(0.5), "p99": q(0.99), "p999": q(0.999), "max": q(1.0)}


async def main(args):
    ctx = Context(args.port)
    peak = rss_kb()

    async def sampler():
        nonlocal peak
        while True:
            peak = max(peak, rss_kb())
            await asyncio.sleep(0.1)

    s = asyncio.create_task(sampler())
    t0, c0 = time.monotonic(), cpu_s()
    until = t0 + args.secs
    await asyncio.gather(
        *(line(ctx, args.ramp_secs * i / max(args.lines, 1), until) for i in range(args.lines))
    )
    wall, cpu = time.monotonic() - t0, cpu_s() - c0
    s.cancel()
    t = ctx.totals
    print(
        json.dumps(
            {
                "impl": "python (google-genai)" + (" + uvloop" if args.uvloop else ""),
                "lines": args.lines,
                "secs": args.secs,
                "calls": t.calls,
                "booked": t.booked,
                "failed": t.failed,
                "errors": t.errors,
                "wall_s": round(wall, 2),
                "cpu_s": round(cpu, 2),
                "cpu_pct_of_one_core": round(100 * cpu / wall, 1),
                "rss_start_kb": RSS_START,
                "rss_after_imports_kb": RSS_IMPORTS,
                "rss_peak_kb": peak,
                "per_line_kb": round((peak - RSS_IMPORTS) / args.lines, 1),
                "audio_frames": len(t.audio_late_ms),
                "audio_late_ms": quantiles(t.audio_late_ms),
            }
        )
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--lines", type=int, default=10)
    p.add_argument("--secs", type=float, default=60)
    p.add_argument("--port", type=int, default=8766)
    p.add_argument("--ramp-secs", type=float, default=30)
    p.add_argument("--uvloop", action="store_true", help="run on uvloop instead of asyncio's loop")
    args = p.parse_args()
    if args.uvloop:
        import uvloop

        uvloop.run(main(args))
    else:
        asyncio.run(main(args))
