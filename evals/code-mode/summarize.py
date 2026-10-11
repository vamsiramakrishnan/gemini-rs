"""Summarize code-mode experiment results, regrading spoken answers leniently.

Usage: python3 -I summarize.py results.json [more.json ...]
"""
import json
import re
import statistics
import sys

rows = []
for path in sys.argv[1:]:
    rows.extend(r for r in json.load(open(path)) if "error" not in r)


def spoken_times(t):
    h, m = map(int, t.split(":"))
    h12 = h - 12 if h > 12 else h
    words = ["", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"]
    out = {t, f"{h12}:{m:02d}", f"{h}:{m:02d}"}
    if m == 0:
        out |= {f"{h12} pm", f"{h12}pm", f"{h12} p.m", f"{words[h12]} pm", f"{words[h12]} o'clock", f"{words[h12]} p.m"}
    if m == 30:
        out |= {f"{words[h12]} thirty", f"half past {words[h12]}"}
    teens = {17: "seventeen", 18: "eighteen", 19: "nineteen", 20: "twenty", 21: "twenty-one"}
    if h in teens:
        out |= {f"{teens[h]}-{'thirty' if m == 30 else 'hundred'}", f"{teens[h]} {'thirty' if m == 30 else 'hundred'}"}
    return out


def said(r):
    return " ".join(t["said"] for t in r["turns"]).lower()


def regrade(r):
    """The harness checks the calls; recheck only the wording of the answer."""
    why = r["why"]
    calls_ok = "true" in why.split(";")[0]
    text = said(r)
    task = r["task"]
    if task == "t1-date":
        return calls_ok and any(s in text for s in spoken_times("20:00"))
    if task == "t2-date":
        return calls_ok and any(s in text for s in spoken_times("17:30"))
    if task in ("t7-aggregate-large", "t9-aggregate-120"):
        nums = {"5": "five", "22": "twenty-two"}
        fixed = re.sub(r"(\d+) dollars and (\d+) cents", r"\1.\2", text.replace(",", ""))
        for k, v in nums.items():
            fixed = fixed.replace(v, k)
        total = "1530.45" if task.startswith("t7") else "2588.70"
        late = "5" if task.startswith("t7") else "22"
        extra = task.startswith("t7") or "79.34" in fixed
        return "looked up: true" in why and total in fixed and re.search(rf"\b{late}\b", fixed) is not None and extra
    if task == "t10-fanout-14":
        day = "12th" in text or "twelfth" in text or "october 12" in text
        return calls_ok and day and any(s in text for s in spoken_times("20:30"))
    if task == "t3-fanout":
        day = "tuesday" in text or "20th" in text or " 20 " in text or "twentieth" in text
        return calls_ok and day and any(s in text for s in spoken_times("18:30"))
    return r["ok"]


arms = sorted({r["arm"] for r in rows})
tasks = sorted({r["task"] for r in rows})
print(f"{'task':<20}" + "".join(f"{a:>44}" for a in arms))
print(f"{'':<20}" + "".join(f"{'right   ms p50  trips  tokens  errs ref unb':>44}" for _ in arms))
totals = {a: {"ok": 0, "n": 0, "ms": [], "tokens": [], "trips": [], "errs": 0} for a in arms}
for task in tasks:
    line = f"{task:<20}"
    for a in arms:
        rs = [r for r in rows if r["task"] == task and r["arm"] == a]
        if not rs:
            line += f"{'-':>44}"
            continue
        ok = sum(regrade(r) for r in rs)
        ms = statistics.median(r["ms"] for r in rs)
        trips = statistics.mean(r["round_trips"] for r in rs)
        toks = statistics.median(r["max_prompt_tokens"] for r in rs)
        errs = sum(r["code_errors"] for r in rs)
        ref = sum(r["refusals"] for r in rs)
        unb = sum(r.get("unbacked_claims", 0) for r in rs)
        t = totals[a]
        t["ok"] += ok; t["n"] += len(rs); t["ms"] += [r["ms"] for r in rs]
        t["tokens"] += [r["max_prompt_tokens"] for r in rs]; t["trips"] += [r["round_trips"] for r in rs]; t["errs"] += errs
        line += f"{f'{ok}/{len(rs)}':>11}{ms:>9.0f}{trips:>7.1f}{toks:>8.0f}{errs:>6}{ref:>4}{unb:>4}"
    print(line)
print()
print("first output (first turn), median ms:")
for task in tasks:
    print(f"  {task:<20}" + "".join(
        f"{a}={statistics.median([r['turns'][0]['first_output_ms'] or 0 for r in rows if r['task']==task and r['arm']==a] or [0]):>6.0f}  " for a in arms))
print()
for a in arms:
    t = totals[a]
    print(f"{a:>6}: right {t['ok']}/{t['n']}, latency p50 {statistics.median(t['ms']):.0f} ms, "
          f"round trips mean {statistics.mean(t['trips']):.2f}, prompt tokens p50 {statistics.median(t['tokens']):.0f}, code errors {t['errs']}")
