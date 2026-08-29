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

## First golden: what the bench already caught

The very first run earned its keep (`golden/report.json`):

- **Three AEC instabilities**, none visible to white-noise unit tests:
  burst-onset step overshoot (the Px normalizer decayed through far-end
  silence), empty-bin weight random-walk on narrowband far ends, and a
  practical step-size bound far below theory (mu 0.25+ diverges, 0.1 sits
  at a bad equilibrium, **0.05 holds**: ERLE +8..11 dB on the harmonic
  worst case). All three fixed in `voice::dsp::aec` with a regression
  test on the discriminating input.
- **`hpf+aec` is the echo-scene chain**: self-barge-ins at the raw floor
  with 11 dB ERLE; zero false activations under double-talk with the
  near-end surviving at 20 ms onset latency.
- **A real composition flaw in `full`**: behind an AEC, the AGC's
  energy-based speech gate mistakes residual echo for quiet speech and
  amplifies it (+30 dB max gain), tripling self-barge-ins. Until the
  chain wires RNNoise's `vad_probability()` into
  `Agc::set_speech_probability`, do not put the AGC after an AEC in
  echo-prone deployments.
- The known noise story reproduced: `hpf+denoise` takes white/babble
  scenes from 8/9 missed onsets to 0/9 at zero false activations.

## Real recordings: the DEMAND × LibriSpeech sweep

The synthetic layer ranks variants; `scripts/real_noise_eval.py` +
`examples/real_eval.rs` confirm them on real audio: real LibriSpeech
utterances mixed into real DEMAND environment recordings (14 environments —
transport, street, nature, domestic, office, public) at 10/5/0 dB, scored
by the same Rust scorer (energy VAD, declared-latency compensation).

Headline from the first full sweep (168 utterance detections per variant):

- **raw misses 116 of 168** — the energy VAD is effectively deaf on real
  noisy mixtures at these SNRs, at zero false activations (it simply never
  fires);
- **hpf+denoise misses 4** — all in competing-speech scenes (meeting
  babble, cafeteria @ 0 dB) — with residual false activations of at most
  one event per 24 s track in 10 of 42 conditions (nature/traffic
  transients);
- noise floor in speech gaps drops 16–56 dB everywhere except
  meeting babble (2 dB — competing speech reads as speech to RNNoise);
- the denoiser's processing-distortion floor caps speech-active segmental
  SNR at ≈5.5 dB: on scenes that are already clean (bus/metro @ 10 dB,
  where raw sits at 8–10 dB segSNR), the chain *costs* fidelity. Run raw
  on clean close-talk inputs.

Scoring note: real speech re-triggers the VAD across prosodic pauses
within one utterance. `score_vad` counts those as `reactivations`, not
false activations — only an onset outside every truth window is false.
This distinction is invisible on single-burst synthetic scenes and
mandatory on real speech.

## What this bench does not cover

Synthetic proxies, not recorded speech: the babble/traffic beds are shaped
noise, and there is no ΔWER intelligibility probe here (that needs real
speech with transcripts and an ASR — run the TurnBench harness in
`evals/turnbench/` for real-conversation, task-level scoring, and the live
session bench for end-to-end confirmation). Treat dspbench as the fast,
deterministic middle layer: sharp enough to rank chain variants and catch
regressions, honest enough to say when a question needs the heavier layers.
