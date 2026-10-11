"""Markdown tables from run_trattoria.py's JSONL: one row per run.

    python summarize_trattoria.py ../results/2026-10-11-trattoria.jsonl
"""

import json
import sys

NAMES = {"rust": "Rust", "python": "Python", "python-uvloop": "Python + uvloop"}
ORDER = {"rust": 0, "python": 1, "python-uvloop": 2}


def q(d, k):
    return "–" if not d or d.get(k) is None else f"{d[k]:g}"


def num(v, fmt="g"):
    return "–" if v is None else format(v, fmt)


def main(path):
    runs = [json.loads(line) for line in open(path)]
    runs.sort(key=lambda r: (ORDER[r["impl"]], r["lines"]))

    print("| Client | Lines | Booked | CPU, % of one core | CPU ms per call-second | Peak RSS MB | KB per line |")
    print("|---|---|---|---|---|---|---|")
    for r in runs:
        c = r["client"]
        if "error" in c:
            print(f"| {NAMES[r['impl']]} | {r['lines']} | the client failed | – | – | – | – |")
            continue
        base = c.get("rss_after_imports_kb", c["rss_start_kb"])
        print(
            f"| {NAMES[r['impl']]} | {r['lines']} | {c['booked']}/{c['calls']} | {num(c['cpu_pct_of_one_core'])} "
            f"| {num(r['cpu_ms_per_call_second'])} | {c['rss_peak_kb'] / 1024:.0f} "
            f"| {(c['rss_peak_kb'] - base) / r['lines']:.0f} |"
        )
    print()
    print("| Client | Lines | Mic late p50 / p99 / p99.9 ms | Model audio late p50 / p99 / p99.9 ms "
          "| Steering p50 / p99 ms | Call p50 / p99 s |")
    print("|---|---|---|---|---|---|")
    for r in runs:
        c, m = r["client"], r["mock"]
        mic, aud, st, call = m.get("mic_late_ms"), c.get("audio_late_ms"), m.get("steer_ms"), m.get("call_secs")
        print(
            f"| {NAMES[r['impl']]} | {r['lines']} "
            f"| {q(mic, 'p50')} / {q(mic, 'p99')} / {q(mic, 'p999')} "
            f"| {q(aud, 'p50')} / {q(aud, 'p99')} / {q(aud, 'p999')} "
            f"| {q(st, 'p50')} / {q(st, 'p99')} | {q(call, 'p50')} / {q(call, 'p99')} |"
        )


if __name__ == "__main__":
    main(sys.argv[1])
