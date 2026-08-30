#!/usr/bin/env python3
"""TurnBench driver for the gemini-rs mic-chain predictor.

Streams every conversation's two speaker channels through the Rust
`turnbench-predictor` binary and assembles TurnBench predictions JSON;
optionally scores against the dataset's gold labels in the same run.

Single-config mode (env knobs forwarded to the binary — CHAIN, VAD,
EOT_HOLD_MS, EARSHOT_*):

    python driver.py --dataset <hf repo|local dir> --out preds.json [--score]

Ablation-matrix mode — run MANY predictor configs over ONE pass of the
dataset (each conversation is decoded once, then scored by every config;
shards are downloaded once and deleted, so the full split never sits on
disk):

    python driver.py --dataset <hf repo|local dir> --configs ablation.json \
        --out-dir results/ [--score]

where ablation.json is a list of {"name": str, "env": {VAR: value, ...}}.
Per config this writes results/preds-<name>.json and results/raw-<name>.json
(raw predictor output incl. committed speech segments, for operating-point
sweeps), plus results/scores.json when --score is given.

Env/CLI control for paths:
  TURNBENCH_REPO — TurnBench benchmark checkout (default ../sesameailabs/turnbench)
  TURNBENCH_PREDICTOR — predictor binary (default ./predictor/target/release/turnbench-predictor)
"""
from __future__ import annotations

import argparse
import io
import json
import os
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np
import soundfile as sf

script_dir = Path(__file__).parent.absolute()
turnbench_repo = Path(
    os.environ.get(
        "TURNBENCH_REPO",
        script_dir.parent.parent / "sesameailabs/turnbench",
    )
).absolute()
sys.path.insert(0, str(turnbench_repo))

from turnbench.data import (
    Conversation,
    resolve_dataset,
)
from turnbench.data import PINNED_REVISIONS
from turnbench.durations import load_durations_for_source
from turnbench.score import score_submission
from turnbench.submission import (
    SCHEMA_VERSION,
    ConversationPrediction,
    SpeakerEvents,
    Submission,
)

PREDICT_BIN = Path(
    os.environ.get(
        "TURNBENCH_PREDICTOR",
        script_dir / "predictor/target/release/turnbench-predictor",
    )
).absolute()
TARGET_SR = 16_000


def to_pcm16_16k(samples: np.ndarray, sample_rate: int) -> bytes:
    """Mono float samples at any rate -> PCM16 16 kHz WAV bytes."""
    if samples.ndim > 1:
        samples = samples.mean(axis=1)
    if sample_rate != TARGET_SR:
        n_out = int(round(len(samples) * TARGET_SR / sample_rate))
        x_old = np.linspace(0.0, 1.0, num=len(samples), endpoint=False)
        x_new = np.linspace(0.0, 1.0, num=n_out, endpoint=False)
        samples = np.interp(x_new, x_old, samples)
    pcm = np.clip(samples * 32767.0, -32768, 32767).astype("<i2")
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(TARGET_SR)
        w.writeframes(pcm.tobytes())
    return buf.getvalue()


def run_predictor(paths: list[str], env_overlay: dict | None) -> dict:
    env = dict(os.environ)
    if env_overlay:
        env.update({k: str(v) for k, v in env_overlay.items()})
    raw = subprocess.run(
        [str(PREDICT_BIN), *paths],
        capture_output=True,
        text=True,
        check=True,
        env=env,
    ).stdout
    return json.loads(raw)


def to_prediction(cid: str, duration_s: float, events: dict) -> ConversationPrediction:
    # Clamp into the scored duration (strictly increasing is preserved).
    limit = duration_s - 1e-3
    clamp = lambda ts: [min(t, limit) for t in ts if t <= duration_s]
    return ConversationPrediction(
        conversation_id=cid,
        speaker_1=SpeakerEvents(
            eot=clamp(events["speaker_1"]["eot"]),
            interruption=clamp(events["speaker_1"]["interruption"]),
        ),
        speaker_2=SpeakerEvents(
            eot=clamp(events["speaker_2"]["eot"]),
            interruption=clamp(events["speaker_2"]["interruption"]),
        ),
    )


def stream_conversations(source: str):
    """Yield one Conversation at a time without materialising the split.

    resolve_dataset() concatenates every shard's audio into one in-memory
    Arrow table (~14 GB for dev) and OOMs a 15 GB container, and the full
    parquet snapshot does not fit a small disk either — so shards are
    downloaded one at a time, read row by row, and deleted after use.
    Annotations are not needed for prediction, so only id + audio columns
    are read."""
    import pyarrow.parquet as pq
    from huggingface_hub import HfApi, hf_hub_download

    token = os.environ.get("HF_TOKEN")
    durations = load_durations_for_source(source)
    columns = ["conversation_id", "speaker_1_audio", "speaker_2_audio"]

    if Path(source).is_dir():
        shards = sorted(str(p) for p in Path(source).glob("*.parquet"))
        local = True
    else:
        revision = PINNED_REVISIONS.get(source)
        shards = sorted(
            name
            for name in HfApi(token=token).list_repo_files(
                source, repo_type="dataset", revision=revision
            )
            if name.endswith(".parquet")
        )
        local = False

    for shard in shards:
        if local:
            file = shard
        else:
            file = hf_hub_download(
                source,
                shard,
                repo_type="dataset",
                revision=PINNED_REVISIONS.get(source),
                token=token,
            )
        for batch in pq.ParquetFile(file).iter_batches(batch_size=1, columns=columns):
            row = batch.to_pylist()[0]
            cid = row["conversation_id"]
            audio_bytes = {s: row[f"speaker_{s}_audio"]["bytes"] for s in (1, 2)}
            duration = durations.get(cid)
            if duration is None:
                duration = sf.info(io.BytesIO(audio_bytes[1])).duration
            yield Conversation(
                conversation_id=cid,
                duration_s=duration,
                annotations={},
                audio_bytes=audio_bytes,
            )
        if not local:
            blob = Path(file).resolve()
            blob.unlink(missing_ok=True)
            Path(file).unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--out", default="predictions-gemini-rs.json")
    parser.add_argument("--score", action="store_true")
    parser.add_argument("--raw-out", help="also write raw predictor output (segments) per conversation")
    parser.add_argument("--configs", help="JSON file: [{name, env}] — ablation matrix in one dataset pass")
    parser.add_argument("--out-dir", default="results", help="output directory for --configs mode")
    parser.add_argument("--resume", action="store_true",
                        help="skip configs whose preds-<name>.json already exists in --out-dir; "
                        "only new configs cost a dataset pass, finished ones are never re-run")
    parser.add_argument("--score-only", action="store_true",
                        help="no audio pass at all: load existing preds-*.json from --out-dir and (re)score them")
    args = parser.parse_args()
    if args.score_only:
        args.score = True

    if args.configs:
        configs = json.loads(Path(args.configs).read_text())
    else:
        configs = [{"name": None, "env": {}}]

    done: dict[str, "Submission"] = {}
    if args.resume or args.score_only:
        for cfg in list(configs):
            name = cfg["name"]
            path = Path(args.out_dir) / f"preds-{name}.json" if name else Path(args.out)
            if path.exists():
                done[name] = Submission.model_validate_json(path.read_text())
                configs.remove(cfg)
                print(f"kept existing {path} ({len(done[name].predictions)} conversations)")
    if args.score_only and configs:
        missing = ", ".join(str(c["name"]) for c in configs)
        sys.exit(f"--score-only but no saved predictions for: {missing}")

    # Per-conversation checkpointing: every conversation's raw events are
    # appended to raw-<name>.jsonl the moment they are computed, so a crash
    # (disk-full, OOM, container reclaim) loses at most one conversation,
    # and a restart resumes exactly where it stopped instead of paying for
    # the whole dataset pass again.
    raw_all: dict[str, dict] = {c["name"]: {} for c in configs}
    ckpt_paths: dict[str, Path] = {}
    if args.configs:
        out_dir = Path(args.out_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        for cfg in configs:
            path = out_dir / f"raw-{cfg['name']}.jsonl"
            ckpt_paths[cfg["name"]] = path
            if path.exists():
                for line in path.read_text().splitlines():
                    if line.strip():
                        row = json.loads(line)
                        raw_all[cfg["name"]][row["conversation_id"]] = row
        loaded = {n: len(v) for n, v in raw_all.items() if v}
        if loaded:
            print(f"checkpoints loaded: {loaded}", file=sys.stderr)

    for conv in stream_conversations(args.dataset) if configs else ():
        todo = [c for c in configs if conv.conversation_id not in raw_all[c["name"]]]
        if not todo:
            continue
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for speaker in (1, 2):
                path = Path(tmp) / f"sp{speaker}.wav"
                samples, rate = conv.audio(speaker)
                path.write_bytes(to_pcm16_16k(samples, rate))
                paths.append(str(path))
            for cfg in todo:
                events = run_predictor(paths, cfg["env"])
                row = {
                    "conversation_id": conv.conversation_id,
                    "duration_s": conv.duration_s,
                    **events,
                }
                raw_all[cfg["name"]][conv.conversation_id] = row
                if cfg["name"] in ckpt_paths:
                    with ckpt_paths[cfg["name"]].open("a") as f:
                        f.write(json.dumps(row) + "\n")
        print(f"  {conv.conversation_id}: done ({conv.duration_s:.0f}s x {len(todo)} configs)", file=sys.stderr)

    # Write every config's outputs before any scoring, so a scorer failure
    # never discards a completed dataset pass.
    submissions = dict(done)
    for cfg in configs:
        name = cfg["name"]
        predictions = [
            to_prediction(cid, row["duration_s"], row)
            for cid, row in raw_all[name].items()
        ]
        submission = Submission(schema_version=SCHEMA_VERSION, predictions=predictions)
        submissions[name] = submission
        if name is None:
            out_path, raw_path = Path(args.out), args.raw_out and Path(args.raw_out)
        else:
            out_dir = Path(args.out_dir)
            out_dir.mkdir(parents=True, exist_ok=True)
            out_path = out_dir / f"preds-{name}.json"
            raw_path = out_dir / f"raw-{name}.json"
        out_path.write_text(submission.model_dump_json(indent=1))
        if raw_path:
            raw_path.write_text(json.dumps(raw_all[name]))
        print(f"wrote {out_path} ({len(predictions)} conversations)")

    if args.score:
        scores_out = {}
        dataset = resolve_dataset(args.dataset, skip_audio=True)
        for name in submissions:
            try:
                scores = score_submission(submissions[name], dataset)
            except Exception as e:  # keep scoring the rest
                print(f"{name or 'default'}: scoring failed: {e}", file=sys.stderr)
                continue
            row = {}
            for task in ("task_eot", "task_int"):
                cell = getattr(scores, task)
                print(f"{name or 'default'} {task}: {cell}")
                row[task] = json.loads(cell.model_dump_json()) if hasattr(cell, "model_dump_json") else str(cell)
            scores_out[name or "default"] = row
        if args.configs:
            (Path(args.out_dir) / "scores.json").write_text(json.dumps(scores_out, indent=1))
            print(f"wrote {Path(args.out_dir) / 'scores.json'}")


if __name__ == "__main__":
    main()
