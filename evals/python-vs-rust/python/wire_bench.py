"""N concurrent google-genai Live sessions against the local mock: each
connects, runs one turn, then holds idle. Reports RSS like session-bench."""
import argparse, asyncio, json, os, resource, sys, time

def cpu_s():
    r = resource.getrusage(resource.RUSAGE_SELF)
    return r.ru_utime + r.ru_stime

frames = 0
gaps = []

def rss_kb():
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])

p = argparse.ArgumentParser()
p.add_argument("--sessions", type=int, default=100)
p.add_argument("--port", type=int, default=8765)
p.add_argument("--audio-secs", type=float, default=0)
p.add_argument("--work-ms", type=float, default=0, help="CPU-bound app work per call per second")
p.add_argument("--numpy", action="store_true", help="also import numpy (Option A's DSP dependency)")
args = p.parse_args()

rss_start = rss_kb()
from google import genai
from google.genai import types
if args.numpy:
    import numpy  # noqa: F401
rss_imports = rss_kb()

client = genai.Client(api_key="bench")
client._api_client._websocket_base_url = lambda: f"ws://127.0.0.1:{args.port}"
client._api_client._websocket_ssl_ctx = {}  # plain ws:// to the local mock

peak = rss_imports
connected = 0
turn_ms = []
release = None
all_ready = None

async def app_work():
    # Naive inline app work (tool handler, transcript processing): holds the GIL.
    while True:
        await asyncio.sleep(1.0)
        t = time.perf_counter()
        while time.perf_counter() - t < args.work_ms / 1000:
            pass

async def session_task(i):
    global connected
    worker = asyncio.create_task(app_work()) if args.work_ms else None
    async with client.aio.live.connect(model="gemini-2.0-flash-live-001",
                                       config=types.LiveConnectConfig(response_modalities=["AUDIO"])) as s:
        t0 = time.perf_counter()
        await s.send_client_content(turns=types.Content(role="user", parts=[types.Part(text=(f"stream:{args.audio_secs}" if args.audio_secs else "hi"))]),
                                    turn_complete=True)
        global frames
        last = None
        async for msg in s.receive():
            if msg.data:
                frames += 1
                now = time.perf_counter()
                if last is not None:
                    gaps.append((now - last) * 1000)
                last = now
            if msg.server_content and msg.server_content.turn_complete:
                break
        turn_ms.append((time.perf_counter() - t0) * 1000)
        if worker:
            worker.cancel()
        connected += 1
        if connected == args.sessions:
            all_ready.set()
        await release.wait()

def gap_stats():
    if not gaps:
        return {}
    g = sorted(gaps)
    q = lambda p: round(g[min(len(g) - 1, int(p * len(g)))], 1)
    return {"gap_p50_ms": q(0.5), "gap_p99_ms": q(0.99), "gap_p999_ms": q(0.999),
            "gap_max_ms": round(g[-1], 1),
            "late_over_60ms_pct": round(100 * sum(1 for x in g if x > 60) / len(g), 2)}

async def sampler():
    global peak
    while True:
        peak = max(peak, rss_kb())
        await asyncio.sleep(0.05)

async def main():
    global release, all_ready
    release, all_ready = asyncio.Event(), asyncio.Event()
    s = asyncio.create_task(sampler())
    t0 = time.perf_counter()
    c0 = cpu_s()
    tasks = [asyncio.create_task(session_task(i)) for i in range(args.sessions)]
    await asyncio.wait_for(all_ready.wait(), timeout=300)
    ready_s = time.perf_counter() - t0
    cpu = cpu_s() - c0
    await asyncio.sleep(1.0)
    idle = rss_kb()
    release.set()
    await asyncio.gather(*tasks, return_exceptions=True)
    s.cancel()
    turn_ms.sort()
    pct = lambda q: turn_ms[min(len(turn_ms) - 1, int(q * len(turn_ms)))]
    print(json.dumps({
        "impl": "python google-genai" + (" + numpy" if args.numpy else ""),
        "sessions": args.sessions,
        "rss_start_kb": rss_start, "rss_after_imports_kb": rss_imports,
        "rss_idle_kb": idle, "rss_peak_kb": peak,
        "per_session_kb": round((idle - rss_imports) / args.sessions, 1),
        "all_connected_s": round(ready_s, 2),
        "audio_secs": args.audio_secs, "work_ms": args.work_ms, "audio_frames": frames,
        "cpu_s": round(cpu, 2),
        **gap_stats(), "cpu_pct_of_one_core": round(100 * cpu / ready_s, 1),
        "turn_ms_p50": round(pct(0.5), 1), "turn_ms_p99": round(pct(0.99), 1),
    }))

asyncio.run(main())
