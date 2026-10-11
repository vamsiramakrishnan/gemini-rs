"""Run the trattoria agent, Rust and Python, at several line counts, and write
one JSON record per run.

Each run starts a fresh trattoria_mock pinned to its own cores, runs one
client pinned to one core, then merges what the client measured (CPU, RSS,
model-audio lateness, outcomes) with what the stand-in measured (mic
lateness, tool round trips, steering latency, call length) and the stand-in's
own CPU, so a saturated stand-in would show.

    python run_trattoria.py --impl rust --impl python --lines 25 --lines 50 \\
        --secs 90 --out ../results/2026-10-11-trattoria.jsonl

Paths: the Rust binaries in $CARGO_TARGET_DIR/release (default ../target),
the Python agent beside this file, run with this interpreter.
"""

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def proc_cpu_s(pid):
    with open(f"/proc/{pid}/stat") as f:
        rest = f.read().rsplit(")", 1)[1].split()
    return (int(rest[11]) + int(rest[12])) / os.sysconf("SC_CLK_TCK")


def get(url):
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


def run(impl, lines, args, port):
    target = os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target"))
    bins = os.path.join(target, "release")
    mock = subprocess.Popen(
        ["taskset", "-c", args.mock_cpus, os.path.join(bins, "trattoria_mock"), "--port", str(port),
         "--extract-ms", str(args.extract_ms), "--reply-ms", str(args.reply_ms)],
        stdout=subprocess.PIPE, text=True,
    )
    mock.stdout.readline()  # "ready on …"
    common = ["--lines", str(lines), "--secs", str(args.secs), "--ramp-secs", str(args.ramp_secs), "--port", str(port)]
    if impl == "rust":
        cmd = [os.path.join(bins, "trattoria")] + common
        env = dict(os.environ, TOKIO_WORKERS=str(args.tokio_workers))
    else:
        cmd = [sys.executable, os.path.join(HERE, "trattoria.py")] + common
        if impl == "python-uvloop":
            cmd.append("--uvloop")
        env = dict(os.environ)
    m0 = proc_cpu_s(mock.pid)
    t0 = time.monotonic()
    out = subprocess.run(["taskset", "-c", args.client_cpus] + cmd, env=env, capture_output=True, text=True)
    wall = time.monotonic() - t0
    mock_cpu = proc_cpu_s(mock.pid) - m0
    # The stand-in records a call once it sees the connection close, which
    # can trail the client's exit: wait until every call it started is done.
    deadline = time.monotonic() + 30
    while True:
        stats = get(f"http://127.0.0.1:{port}/stats")
        settled = stats["calls_hung_up"] + sum(stats["failures"].values())
        if settled >= stats["calls_started"] or time.monotonic() > deadline:
            break
        time.sleep(0.2)
    mock.terminate()
    mock.wait()
    try:
        client = json.loads(out.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError):
        client = {"error": out.stderr[-2000:]}
    call_secs = stats.get("call_secs_total") or 0
    record = {
        "impl": impl,
        "lines": lines,
        "client_cpus": args.client_cpus,
        "mock_cpus": args.mock_cpus,
        "client": client,
        "mock": stats,
        "mock_cpu_pct_of_one_core": round(100 * mock_cpu / wall, 1),
        # Client CPU per second of call, in ms: the cost of one live call.
        "cpu_ms_per_call_second": round(1000 * client.get("cpu_s", 0) / call_secs, 2) if call_secs else None,
    }
    return record


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--impl", action="append", choices=["rust", "python", "python-uvloop"], required=True)
    p.add_argument("--lines", action="append", type=int, required=True)
    p.add_argument("--secs", type=int, default=90)
    p.add_argument("--ramp-secs", type=int, default=40)
    p.add_argument("--client-cpus", default="0")
    p.add_argument("--mock-cpus", default="2,3")
    p.add_argument("--tokio-workers", type=int, default=1)
    p.add_argument("--extract-ms", type=int, default=400)
    p.add_argument("--reply-ms", type=int, default=500)
    p.add_argument("--port", type=int, default=8790)
    p.add_argument("--out", required=True)
    args = p.parse_args()
    port = args.port
    for lines in args.lines:
        for impl in args.impl:
            record = run(impl, lines, args, port)
            port += 1
            with open(args.out, "a") as f:
                f.write(json.dumps(record) + "\n")
            c, m = record["client"], record["mock"]
            print(
                f"{impl:6} lines={lines:4} booked={c.get('booked')}/{c.get('calls')} "
                f"cpu={c.get('cpu_pct_of_one_core')}% ms/call-s={record['cpu_ms_per_call_second']} "
                f"rss_peak={c.get('rss_peak_kb')}KB mic_late_p99={(m.get('mic_late_ms') or {}).get('p99')} "
                f"audio_late_p99={(c.get('audio_late_ms') or {}).get('p99')} "
                f"steer_p50={(m.get('steer_ms') or {}).get('p50')} call_p50={(m.get('call_secs') or {}).get('p50')} "
                f"mock_cpu={record['mock_cpu_pct_of_one_core']}%",
                flush=True,
            )


if __name__ == "__main__":
    main()
