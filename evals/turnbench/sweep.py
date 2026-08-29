#!/usr/bin/env python3
"""Operating-point sweep over the one-pass VAD segments.

Derives EOT and INT event streams from the committed speech segments for a
grid of (eot_hold_ms, int_min_ms) operating points and scores each against
the dev gold — the same audio pass, many operating points, mirroring the
benchmark's own threshold-sweep methodology.

Rules (all causal; times are commit times):
- EOT(hold h): a segment end at `e` emits an EOT at `e + h` unless the same
  speaker starts a new segment before `e + h`.
- INT(min m): a segment starting at `s` while the other speaker's segment is
  open emits an interruption at `s + m` only if the segment lasts at least
  `m` (shorter overlapped speech = backchannel, suppressed). m=0 keeps the
  original onset rule.

    python sweep.py segments-dev.json [--dataset <dataset>]

Env/CLI control for paths:
  TURNBENCH_REPO — TurnBench benchmark checkout (default ../sesameailabs/turnbench)
  --dataset — dataset to score against (default mundo-ai/turn-benchmark-dev)
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

import numpy as np

# Resolve paths: script dir as reference, env overrides defaults
script_dir = Path(__file__).parent.absolute()
turnbench_repo = Path(
    os.environ.get(
        "TURNBENCH_REPO",
        script_dir.parent.parent / "sesameailabs/turnbench",
    )
).absolute()
sys.path.insert(0, str(turnbench_repo))

from turnbench.data import resolve_dataset
from turnbench.score import score_submission
from turnbench.submission import (
    SCHEMA_VERSION,
    ConversationPrediction,
    SpeakerEvents,
    Submission,
)

EOT_HOLDS_MS = [200, 400, 600, 800, 1000, 1200, 1600]
INT_MINS_MS = [0, 200, 400, 600, 800, 1000, 1400]


def derive_events(
    segments: dict[str, list[list[float]]],
    duration_s: float,
    eot_hold_s: float,
    int_min_s: float,
) -> dict[str, SpeakerEvents]:
    out = {}
    for me, other in (("speaker_1", "speaker_2"), ("speaker_2", "speaker_1")):
        mine = segments[me]
        theirs = segments[other]
        eot, interruption = [], []
        for i, (start, end) in enumerate(mine):
            # EOT: hold survives only if no same-speaker restart within it.
            next_start = mine[i + 1][0] if i + 1 < len(mine) else float("inf")
            t = end + eot_hold_s
            if next_start > t and t < duration_s:
                eot.append(t)
            # INT: onset inside an open other-speaker segment, sustained >= m.
            other_open = any(o_start < start < o_end for o_start, o_end in theirs)
            if other_open and (end - start) >= int_min_s:
                t = start + int_min_s
                if t < duration_s:
                    interruption.append(t)
        out[me] = SpeakerEvents(eot=sorted(eot), interruption=sorted(interruption))
    return out


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("segments", help="segments JSON file")
    parser.add_argument("--dataset", default="mundo-ai/turn-benchmark-dev")
    args = parser.parse_args()

    seg_path = Path(args.segments)
    raw = json.loads(seg_path.read_text())
    dataset = resolve_dataset(args.dataset, skip_audio=True)

    def score_point(eot_hold_ms: int, int_min_ms: int):
        predictions = []
        for cid, conv in raw.items():
            segments = {sp: conv[sp]["segments"] for sp in ("speaker_1", "speaker_2")}
            events = derive_events(
                segments, conv["duration_s"], eot_hold_ms / 1000, int_min_ms / 1000
            )
            predictions.append(
                ConversationPrediction(
                    conversation_id=cid,
                    speaker_1=events["speaker_1"],
                    speaker_2=events["speaker_2"],
                )
            )
        sub = Submission(schema_version=SCHEMA_VERSION, predictions=predictions)
        return score_submission(sub, dataset)

    def cells(t):
        recall = t.tp / (t.tp + t.fn) if (t.tp + t.fn) else 0.0
        fp = t.fp / (t.fp + t.tn) if (t.fp + t.tn) else 0.0
        lat = np.array(t.latencies_ms)
        p50 = float(np.percentile(lat, 50)) if len(lat) else 0.0
        p90 = float(np.percentile(lat, 90)) if len(lat) else 0.0
        return recall, fp, p50, p90

    # The two tasks are independent knobs: sweep each axis separately.
    print("== EOT sweep (int_min fixed 600ms) ==")
    print(f"{'hold':>6} {'recall':>7} {'fp':>6} {'p50':>6} {'p90':>7}")
    for hold in EOT_HOLDS_MS:
        r, fp, p50, p90 = cells(score_point(hold, 600).task_eot)
        marker = " <=" if fp <= 0.10 else ""
        print(f"{hold:>6} {r:>7.3f} {fp:>6.3f} {p50:>6.0f} {p90:>7.0f}{marker}")

    print("== INT sweep (eot_hold fixed 400ms) ==")
    print(f"{'min':>6} {'recall':>7} {'fp':>6} {'p50':>6} {'p90':>7}")
    for m in INT_MINS_MS:
        r, fp, p50, p90 = cells(score_point(400, m).task_int)
        marker = " <=" if fp <= 0.10 else ""
        print(f"{m:>6} {r:>7.3f} {fp:>6.3f} {p50:>6.0f} {p90:>7.0f}{marker}")


if __name__ == "__main__":
    main()
