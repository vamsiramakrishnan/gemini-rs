#!/usr/bin/env python3
"""TurnBench driver for the gemini-rs mic-chain predictor.

Streams every conversation's two speaker channels through the Rust
`turnbench-predictor` binary (Denoiser + VoiceActivityDetector, the SDK's shipped
chain) and assembles a TurnBench predictions JSON; optionally scores it
against the dataset's gold labels in the same run.

    python driver.py --dataset <hf repo|local dir> --out preds.json [--score]

Env knobs are forwarded to the binary: CHAIN=raw|denoise, VAD=default|noisy_street,
EOT_HOLD_MS. Env/CLI control for paths:
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

# Resolve paths: script dir as reference, env overrides defaults
script_dir = Path(__file__).parent.absolute()
turnbench_repo = Path(
    os.environ.get(
        "TURNBENCH_REPO",
        script_dir.parent.parent / "sesameailabs/turnbench",
    )
).absolute()
sys.path.insert(0, str(turnbench_repo))

# Import turnbench components
from turnbench.data import (
    Conversation,
    conversation,
    conversation_ids,
    resolve_dataset,
)
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


def predict_conversation(conv: Conversation) -> tuple[ConversationPrediction, dict]:
    with tempfile.TemporaryDirectory() as tmp:
        paths = []
        for speaker in (1, 2):
            samples, rate = conv.audio(speaker)
            path = Path(tmp) / f"sp{speaker}.wav"
            path.write_bytes(to_pcm16_16k(samples, rate))
            paths.append(str(path))
        raw = subprocess.run(
            [str(PREDICT_BIN), *paths], capture_output=True, text=True, check=True
        ).stdout
    events = json.loads(raw)
    raw_events = events
    # Clamp into the scored duration (strictly increasing is preserved).
    limit = conv.duration_s - 1e-3
    clamp = lambda ts: [min(t, limit) for t in ts if t <= conv.duration_s]
    prediction = ConversationPrediction(
        conversation_id=conv.conversation_id,
        speaker_1=SpeakerEvents(
            eot=clamp(events["speaker_1"]["eot"]),
            interruption=clamp(events["speaker_1"]["interruption"]),
        ),
        speaker_2=SpeakerEvents(
            eot=clamp(events["speaker_2"]["eot"]),
            interruption=clamp(events["speaker_2"]["interruption"]),
        ),
    )
    return prediction, raw_events


def stream_conversations(source: str):
    """Yield one Conversation at a time without materialising the split.

    resolve_dataset() concatenates every shard's audio into one in-memory
    Arrow table (~14 GB for dev) and OOMs a 15 GB container; this reads the
    already-snapshotted parquet shards row by row instead, holding one
    conversation's audio at a time. Annotations are not needed for
    prediction, so only id + audio columns are read."""
    import pyarrow.parquet as pq
    from huggingface_hub import snapshot_download

    from turnbench.data import DEV_DATASET, PINNED_REVISIONS

    if Path(source).is_dir():
        files = sorted(str(p) for p in Path(source).glob("*.parquet"))
    else:
        snapshot = snapshot_download(
            source,
            repo_type="dataset",
            revision=PINNED_REVISIONS.get(source),
            allow_patterns="*.parquet",
        )
        files = sorted(str(p) for p in Path(snapshot).rglob("*.parquet"))
    durations = load_durations_for_source(source)
    columns = ["conversation_id", "speaker_1_audio", "speaker_2_audio"]
    for file in files:
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


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--out", default="predictions-gemini-rs.json")
    parser.add_argument("--score", action="store_true")
    parser.add_argument("--raw-out", help="also write raw predictor output (segments) per conversation")
    args = parser.parse_args()

    predictions = []
    raw_all = {}
    for conv in stream_conversations(args.dataset):
        pred, raw_events = predict_conversation(conv)
        predictions.append(pred)
        raw_all[conv.conversation_id] = {"duration_s": conv.duration_s, **raw_events}
        print(f"  {conv.conversation_id}: done ({conv.duration_s:.0f}s)", file=sys.stderr)
    if args.raw_out:
        Path(args.raw_out).write_text(json.dumps(raw_all))
        print(f"wrote {args.raw_out}")

    submission = Submission(schema_version=SCHEMA_VERSION, predictions=predictions)
    Path(args.out).write_text(submission.model_dump_json(indent=1))
    print(f"wrote {args.out} ({len(predictions)} conversations)")

    if args.score:
        dataset = resolve_dataset(args.dataset, skip_audio=True)
        scores = score_submission(submission, dataset)
        for task in ("task_eot", "task_int"):
            cell = getattr(scores, task)
            print(f"{task}: {cell}")


if __name__ == "__main__":
    main()
