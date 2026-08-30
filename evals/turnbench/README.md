# TurnBench: Turn-Taking Evaluation

TurnBench is a benchmark for evaluating turn-taking and interruption prediction in conversational AI. This harness evaluates the SDK's shipped audio processing chain — RNNoise denoising plus VAD configured for noisy street environments — against the [TurnBench dataset](https://github.com/SesameAILabs/turnbench).

## The Chain Evaluated

The chain is strictly causal:

1. **Denoiser** — RNNoise speech enhancement (pure Rust, ~0.008× realtime)
2. **VAD** — Energy-based voice activity detector configured at `VadConfig::noisy_street()` 
3. **Commit time** — Each decision (speech start/end, turn end, interruption) commits at the end of the 30 ms VAD frame that produced it. The reported timestamp includes any configured EOT hold and denoiser latency, and audio after the timestamp is never consulted.

## Setup

### 1. Clone and Sync the TurnBench Benchmark

The dataset is gated on Hugging Face. You'll need a Hugging Face token in the benchmark's `.env`:

```bash
git clone https://github.com/SesameAILabs/turnbench
cd turnbench
uv sync
echo "HF_TOKEN=<your-token>" >> .env
```

### 2. Build the Predictor

From the repo root:

```bash
cargo build --release --manifest-path evals/turnbench/predictor/Cargo.toml
```

The binary lands at `evals/turnbench/predictor/target/release/turnbench-predictor`.

### 3. Run the Driver

The driver streams the dev set's conversations through the predictor and assembles a predictions JSON:

```bash
# Default: looks for turnbench at ../sesameailabs/turnbench relative to this directory
python evals/turnbench/driver.py --dataset mundo-ai/turn-benchmark-dev --out preds.json

# Or override the turnbench repo:
TURNBENCH_REPO=/path/to/turnbench python evals/turnbench/driver.py --dataset mundo-ai/turn-benchmark-dev --out preds.json

# Score against gold in the same run:
python evals/turnbench/driver.py --dataset mundo-ai/turn-benchmark-dev --out preds.json --score

# Save raw segments for operating-point sweeps:
python evals/turnbench/driver.py --dataset mundo-ai/turn-benchmark-dev --raw-out segments.json --out preds.json
```

Env knobs forwarded to the predictor: `CHAIN` (comma list from `{hpf, denoise}`; `raw` = none; default denoise), `VAD=energy|earshot|fusion` (default energy at `noisy_street`; `default` selects `VadConfig::default()`), `EOT_HOLD_MS` (default 400), and for earshot `EARSHOT_THRESHOLD` / `EARSHOT_START_MS` / `EARSHOT_END_MS`.

### Pluggable VAD backends and the ablation matrix

The predictor's decision stage is a `VadBackend` trait — a causal
frame-in/edge-out interface. Three backends ship:

- `energy` — the L0 `VoiceActivityDetector` (30 ms frames), the SDK's client VAD;
- `earshot` — [pykeio/earshot](https://github.com/pykeio/earshot)'s neural
  VAD (16 ms frames) behind causal run-length hysteresis
  (48 ms attack / 240 ms release by default);
- `fusion` — earshot AND energy for onset (both must agree), either-silent
  for offset, at the energy VAD's 30 ms cadence.

Adding a backend = implementing the trait and one match arm. The chain is
ablatable the same way (`CHAIN` stages compose in order).

To run a whole ablation matrix in ONE pass over the dataset (each
conversation decoded once, every config scored on it; shards downloaded
one at a time and deleted, so the split never sits on disk):

```bash
python evals/turnbench/driver.py --dataset mundo-ai/turn-benchmark-dev \
    --configs evals/turnbench/ablation.json --out-dir results/ --score
```

`ablation.json` is a list of `{"name": ..., "env": {...}}`; the committed
one is the 12-config matrix `{energy, earshot, fusion} × {raw, hpf,
denoise, hpf+denoise}` at the conversational operating point. Per config
you get `preds-<name>.json`, `raw-<name>.json` (segments for sweeps), and
a consolidated `scores.json`.

Nothing is ever computed twice:

- `--resume` skips configs whose `preds-<name>.json` already exists — add
  a 13th config to the matrix and only that one costs a dataset pass;
- `--score-only` recomputes `scores.json` from saved predictions with no
  audio pass at all;
- operating-point changes (EOT hold, INT sustain) need no re-run either:
  sweep them offline from the saved `raw-<name>.json` segments with
  `sweep.py`.

### 4. Operating-Point Sweep

The sweep derives EOT and INT events from the same speech segments at different hold/sustain thresholds:

```bash
# Requires segments.json from driver with --raw-out
python evals/turnbench/sweep.py segments.json

# Or specify the dataset:
python evals/turnbench/sweep.py segments.json --dataset mundo-ai/turn-benchmark-dev
```

## Measured Results

### Dev Set Baseline (default operating point: EOT hold 400 ms, INT sustain 600 ms)

38 conversations, ~6.6 hours of speech.

| Task | Recall | FP | p50 (ms) |
|------|--------|-----|----------|
| EOT  | 0.895  | 0.206 | 715 |
| INT  | 0.856  | 0.699 | 83  |

Both exceed the 0.1 FP budget, which motivates the parameter sweep for operating points that trade latency for false-positive control.

### EOT Sweep (INT sustain fixed at 600 ms)

Hold (ms) | Recall | FP | p50 (ms)
----------|--------|-----|----------
200       | 0.944  | 0.333 | 495
400       | 0.900  | 0.214 | 695
600       | 0.855  | 0.135 | 899
**800**   | 0.798  | 0.087 | 1101 ← qualifying (FP ≤ 0.10)
1000      | 0.725  | 0.053 | 1301
1200      | 0.645  | 0.032 | 1495
1600      | 0.508  | 0.011 | 1884

### INT Sweep (EOT hold fixed at 400 ms)

Sustain (ms) | Recall | FP | p50 (ms)
-------------|--------|-----|----------
0            | 0.856  | 0.699 | 83
200          | 0.911  | 0.683 | 270
400          | 0.931  | 0.544 | 470
600          | 0.939  | 0.319 | 672
800          | 0.939  | 0.191 | 873
1000         | 0.931  | 0.126 | 1076
**1400**     | 0.899  | 0.062 | 1476 ← qualifying (FP ≤ 0.10)

## Context: TurnBench Leaderboard

For reference, the TurnBench leaderboard leader (VAP, test set):

| Task | Recall | FP | p50 (ms) |
|------|--------|-----|----------|
| EOT  | 0.845  | 0.055 | 368 |
| INT  | 0.945  | 0.107 | 994 |

Our numbers are **dev-set** (38 conversations); the leaderboard reports **test-set** (held-out). Learned models (VAP, etc.) beat our energy-based chain on EOT latency by predicting turn ends from prosody instead of waiting out silence. Our chain wins on INT false positives at its qualifying point (0.062 vs 0.107).

## SDK Integration

The two qualifying operating points are shipped as presets in `TurnCommitConfig`:

- **Responsive** (400 ms EOT hold, 600 ms INT sustain) — lower latency, higher false positives
- **Conversational** (800 ms EOT hold, 1400 ms INT sustain) — lower false positives, higher latency

```rust
Live::builder()
    .commit_config(TurnCommitConfig::conversational())  // 0.087 EOT FP, 0.062 INT FP
```

Or set thresholds directly:

```rust
Live::builder()
    .commit_config(TurnCommitConfig {
        eot_hold: Duration::from_millis(800),
        min_interruption: Duration::from_millis(1400),
    })
```
