# dspbench — does the mic chain actually help?

Unit tests prove each DSP stage does what its math says. This bench answers
the different question: **does the chain improve the decisions the product
makes?** A chain is not judged on how the audio sounds — it is judged on
whether the VAD fires when the user speaks, stays quiet when they don't,
and whether the model still hears intelligible speech.

## Method

Deterministic, labeled scenes (`scenes.toml`, everything seeded — reruns are
bit-identical) compose what a microphone actually receives:

- near-end speech (a harmonic speech proxy with syllabic rhythm; ground-truth
  activity intervals are free because we composed the scene),
- noise beds at controlled SNRs (white, pink, speech-shaped babble proxy,
  traffic proxy),
- **echo**: a far-end "bot voice" convolved with a sparse synthetic room
  impulse response at a set gain and delay — bot playback re-entering the
  mic, the open-speaker failure mode.

Each scene runs through **chain variants** (`raw`, `hpf`, `hpf+denoise`,
`hpf+aec`, `full` = hpf→aec→denoise→agc→limiter), 20 ms frames through the
same `InputAudioProcessor` path production uses, with the AEC's far-end
reference fed in lockstep like a real playback tap.

### Decision metrics (the verdict)

| Metric | Meaning |
|---|---|
| `false_activations_per_min` | VAD onsets outside every truth interval. On the `echo_only_speakerphone` scene this is the **self-barge-in rate** — the AEC's system-level score. |
| `missed_onsets / total` | Truth intervals that never fired. |
| `onset_latency_ms_p50` | Detection commit minus truth start, after compensating the chain's *declared* group delay — the latency contract paying for itself. |

### Diagnostics (the explanation)

Segmental SNR vs the clean reference (speech segments only, per-segment
clamped ±[-10,35] dB), log-spectral distance (near-end distortion — "did the
AEC mangle the user during double-talk"), AGC level mean/std over 100 ms
blocks, the AEC's own ERLE, and the chain's exit-clip count.

## Run it

```bash
cargo run --release --manifest-path evals/dspbench/Cargo.toml
# custom manifest / output:
cargo run --release --manifest-path evals/dspbench/Cargo.toml -- my-scenes.toml --out report.json
```

The table prints to stderr; the full report lands in `dspbench-report.json`.
`golden/report.json` is the committed reference for the shipped chain —
regenerate it when a chain change is *intended* to move the numbers, and
let the diff be the review.

## What this bench does not cover

Synthetic proxies, not recorded speech: the babble/traffic beds are shaped
noise, and there is no ΔWER intelligibility probe here (that needs real
speech with transcripts and an ASR — run the TurnBench harness in
`evals/turnbench/` for real-conversation, task-level scoring, and the live
session bench for end-to-end confirmation). Treat dspbench as the fast,
deterministic middle layer: sharp enough to rank chain variants and catch
regressions, honest enough to say when a question needs the heavier layers.
