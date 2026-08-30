#!/usr/bin/env python3
"""Diverse real-noise sweep for the mic chain, decision-level.

Mixes real speech into real DEMAND environment recordings at controlled
SNRs and scores every chain variant with the same Rust scorer the bench
uses (`examples/real_eval.rs`: shipped energy VAD, noisy_street profile,
120 ms tolerance, declared-latency compensation). Python adds the signal
diagnostics (speech-active segmental SNR vs the clean reference, noise
floor in speech gaps).

Inputs (built by the caller, see --audio-dir):
  speech-<i>.wav          mono 16 kHz speech utterances (>= 1)
  noise-<env>.wav         mono 16 kHz environment recordings

Run:
  uv run --with soundfile --with numpy python3 real_noise_eval.py \
      --audio-dir <dir> --bin <path-to-real_eval> --out report.json

Everything is seeded; reruns are bit-identical.
"""

import argparse
import json
import subprocess
from pathlib import Path

import numpy as np
import soundfile as sf

SR = 16000
SNRS_DB = [10, 5, 0]
VARIANTS = ["raw", "hpf", "hpf_denoise", "full"]
GAP_S = 0.7
TRACK_S = 25


def rms(x):
    return float(np.sqrt(np.mean(x**2) + 1e-12))


def build_speech(audio_dir: Path):
    """Concatenate utterances with gaps; truth intervals come free."""
    clips = sorted(audio_dir.glob("speech-*.wav"))
    assert clips, f"no speech-*.wav in {audio_dir}"
    gap = np.zeros(int(GAP_S * SR), dtype=np.float32)
    parts, truth, pos = [], [], 0
    for p in clips:
        c, sr = sf.read(p)
        assert sr == SR, f"{p}: {sr} Hz"
        c = c.astype(np.float32)
        truth.append((pos, pos + len(c)))
        parts.extend([c, gap])
        pos += len(c) + len(gap)
    speech = np.concatenate(parts)[: TRACK_S * SR]
    truth = [(a, min(b, len(speech))) for a, b in truth if a < len(speech)]
    mask = np.zeros(len(speech), dtype=bool)
    for a, b in truth:
        mask[a:b] = True
    return speech, truth, mask


def seg_snr(ref, x, mask):
    n, vals = SR // 10, []
    for a in range(0, min(len(ref), len(x)) - n, n):
        if not mask[a : a + n].any():
            continue
        e = x[a : a + n] - ref[a : a + n]
        s = 10 * np.log10((np.sum(ref[a : a + n] ** 2) + 1e-12) / (np.sum(e**2) + 1e-12))
        vals.append(np.clip(s, -10, 35))
    return float(np.mean(vals)) if vals else float("nan")


def gap_floor_dbfs(x, mask):
    g = x[~mask[: len(x)]]
    return 20 * np.log10(rms(g)) if len(g) else float("nan")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--audio-dir", required=True, type=Path)
    ap.add_argument("--bin", required=True, help="path to the real_eval example binary")
    ap.add_argument("--out", default="real-noise-report.json")
    ap.add_argument("--dump-dir", type=Path, help="write mix/processed WAVs here")
    args = ap.parse_args()

    speech, truth, mask = build_speech(args.audio_dir)
    truth_arg = ",".join(f"{a}:{b}" for a, b in truth)
    s_rms = rms(speech[mask])
    rng = np.random.default_rng(29)
    work = args.dump_dir or args.audio_dir
    work.mkdir(parents=True, exist_ok=True)

    envs = sorted(p.stem.replace("noise-", "") for p in args.audio_dir.glob("noise-*.wav"))
    report = {"envs": {}, "speech_utterances": len(truth), "snrs_db": SNRS_DB}
    for env in envs:
        noise, sr = sf.read(args.audio_dir / f"noise-{env}.wav")
        assert sr == SR
        noise = noise.astype(np.float32)
        start = int(rng.integers(0, max(1, len(noise) - len(speech))))
        n = noise[start : start + len(speech)]
        if len(n) < len(speech):
            n = np.tile(n, len(speech) // max(1, len(n)) + 1)[: len(speech)]
        report["envs"][env] = {}
        for snr in SNRS_DB:
            mix = speech + n * (s_rms / rms(n) / (10 ** (snr / 20)))
            peak = np.max(np.abs(mix))
            scale = 0.99 / peak if peak > 0.99 else 1.0
            mix, ref = mix * scale, speech * scale
            mix_path = work / f"mix-{env}-{snr}db.wav"
            sf.write(mix_path, (mix * 32767).astype(np.int16), SR, subtype="PCM_16")

            row = {}
            for variant in VARIANTS:
                out_wav = work / f"proc-{env}-{snr}db-{variant}.wav"
                cmd = [args.bin, variant, str(mix_path), truth_arg, str(out_wav)]
                res = json.loads(subprocess.run(cmd, capture_output=True, check=True, text=True).stdout)
                proc, _ = sf.read(out_wav)
                proc = proc.astype(np.float32)[res["latency_samples"] :]
                row[variant] = {
                    **res["decision"],
                    "latency_ms": res["latency_samples"] * 1000 // SR,
                    "seg_snr_db": round(seg_snr(ref[: len(proc)], proc, mask), 2),
                    "gap_floor_dbfs": round(gap_floor_dbfs(proc, mask), 1),
                }
                if not args.dump_dir:
                    out_wav.unlink()
            row["mix"] = {
                "seg_snr_db": round(seg_snr(ref, mix, mask), 2),
                "gap_floor_dbfs": round(gap_floor_dbfs(mix, mask), 1),
            }
            report["envs"][env][f"{snr}db"] = row
            d, f_ = row["hpf_denoise"], row["full"]
            print(
                f"{env:>10} {snr:>2} dB | miss raw {row['raw']['missed_onsets']}"
                f" -> dn {d['missed_onsets']} full {f_['missed_onsets']}"
                f" | fa/min raw {row['raw']['false_activations_per_min']:.1f}"
                f" -> dn {d['false_activations_per_min']:.1f}"
                f" full {f_['false_activations_per_min']:.1f}"
                f" | segSNR {row['mix']['seg_snr_db']:.1f} -> {d['seg_snr_db']:.1f}"
                f" | floor {row['mix']['gap_floor_dbfs']:.0f} -> {d['gap_floor_dbfs']:.0f} dBFS",
                flush=True,
            )
            if not args.dump_dir:
                mix_path.unlink()

    Path(args.out).write_text(json.dumps(report, indent=1))
    print(f"report -> {args.out}")


if __name__ == "__main__":
    main()
