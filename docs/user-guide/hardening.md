# Hardening a Voice Deployment

A demo needs audio in and audio out. A production voice line — a bank, a
clinic, a support desk — needs four more things before anyone should dial
it: sensitive data kept out of transcripts, silence kept out of the
caller's ear, a clean handoff when a human takes over, and an audio front
end that survives real phone-line noise. Each is a small, composable
capability; none is tied to any one transport.

## Transcript redaction

A caller will read a card number out loud, and speech recognition will
faithfully transcribe it. Everything downstream of the transcript —
callbacks, the transcript buffer, extraction, persistence snapshots, your
logs — then holds that number unless it is removed first.

```rust,ignore
use gemini_adk_rs::live::redaction::TranscriptRedactor;

Live::builder()
    .redaction(
        TranscriptRedactor::new()
            .card_numbers()      // Luhn-checked, replaced with "[card ending 1234]"
            .long_digits(6)      // OTPs, account numbers → "[redacted number]"
            .pattern(regex, "[redacted id]"),  // deployment-specific formats
    )
```

Redaction runs at the event router, **before either lane sees the text**:
fast-lane callbacks, the transcript buffer, extractors, persistence, and
the [handoff packet](#warm-handoff) below all receive the redacted form.
There is deliberately no unredacted side channel.

The card rule matches 13–19-digit runs (spaces and dashes allowed) and
verifies the Luhn checksum before replacing — a tracking number is not a
card. What it keeps, `[card ending 1234]`, is enough for the conversation
to stay coherent without retaining the number.

Two documented limits: streaming *deltas* are not redacted (a number can
straddle chunk boundaries — treat transcripts and `TextComplete`, both
redacted, as the record), and pattern-based scrubbing complements, rather
than replaces, infrastructure-level DLP on stored audio.

## Latency masking

Real turns that call tools take seconds; a phone caller hears seconds of
silence as a dead line. The
[`bridge::spawn_latency_filler`](../api/gemini_adk_fluent_rs/telephony/bridge/index.html)
component watches the session's own event stream: when the caller stops
speaking and no model audio arrives within a configured delay, it plays a
pre-synthesized clip ("one moment, let me check that") into the same
playback channel the pump feeds.

```rust,ignore
use gemini_adk_fluent_rs::telephony::bridge::FillerConfig;

let incoming = agent.next_call().await.unwrap();
let call = incoming
    .filler(FillerConfig::new(filler_pcm_8k))   // mono PCM16 at the call rate
    .answer(&session)
    .await?;
```

Model audio disarms the pending filler; a barge-in flushes a queued one
like any other audio; `min_interval` caps it at one reassurance per slow
operation rather than a loop of them. Masking is a tolerance for the
latency tail, not a fix for it — the silences it papers over remain
visible in the telemetry lane's turn metrics, so they can still be
measured down.

## Warm handoff

The single UX bar for an escalation: the human who picks up never asks
the caller to repeat themselves. What the receiving desk needs is a
compact, serializable packet — not the session.

```rust,ignore
use gemini_adk_fluent_rs::handoff::HandoffRecorder;

let recorder = HandoffRecorder::attach(&handle, 40);   // keep last 40 turns

// … escalation triggers (a flow step, a repair escalation, a keyword) …
let mut packet = recorder.packet(&handle, &[
    "telephony:caller", "verified", "intent",
]);
packet.summarize(&*flash_llm).await.ok();              // optional 2–3 sentences

deliver(serde_json::to_string(&packet)?);              // connector-specific
```

The packet carries the recorded transcript (already redacted, when
redaction is installed), the selected state keys, the governed flow's
standing — steps done, steps active, requirements still unmet, which is
precisely the human's to-do list — and, when the escalation path has the
latency budget, an LLM-written summary of what the caller wants and what
was already tried. Assembly is transport-agnostic; delivering it (a
screen-pop payload, SIP headers, a CRM note) is the connector's job.

## The microphone chain

Phone-line and contact-center audio is noisier than a laptop microphone.
[`voice::pump_processed`](../api/gemini_adk_fluent_rs/voice/fn.pump_processed.html)
accepts a chain of `MicProcessor` stages applied to each frame before
resampling — the insertion point for denoisers and client-side
voice-activity gates:

```rust,ignore
use gemini_adk_fluent_rs::voice::{pump_processed, MicProcessor, NoiseGate};

let running = pump_processed(
    &handle,
    mic_rx, 8_000,
    vec![Box::new(NoiseGate::new(700.0, 3))],  // or a denoiser impl
    spk_tx, 8_000,
);
```

`NoiseGate` is the reference implementation — an energy gate with
hangover, a floor rather than a denoiser. Third-party front ends (a
DeepFilterNet-style neural denoiser, a Silero-style VAD, a vendor SDK)
plug in as one `impl MicProcessor` each, with no pump changes. Evaluate
candidates the way everything else in this SDK is evaluated: the same
recorded call set, scored on both transcription accuracy *and* added
latency — a stage that cleans the audio but spends 200 ms per frame
defeats the point.

### A speech enhancer in the box

The `denoise` feature ships a first-party stage: `voice::Denoiser`, an
RNNoise-based suppressor (pure Rust via
[`nnnoiseless`](https://crates.io/crates/nnnoiseless), no system
dependencies). It exists because the energy VAD has two measurable noise
pathologies. On TTS speech mixed over synthesized noise — three utterances
with labeled noise-only gaps, scored as *false activations / utterances
detected (of 3) / % of noise-only time the VAD claimed speech* — the raw
detector, given continuous **white** noise at any SNR from 20 dB down,
fires one false activation at call start and then latches open for ~99 %
of the call; given **pink** noise at ≤10 dB it instead adapts its floor
upward and *misses* two or three of the three utterances. With
`Denoiser` ahead of it, every one of those cells reads `0/3/0 %` — no
false activations, no stuck-open time, all speech detected, down to
0 dB — at ~0.008× realtime on one CPU core, buffering 10 ms.

```rust,ignore
use gemini_adk_fluent_rs::voice::{pump_processed, Denoiser, NoiseGate};

let running = pump_processed(
    &handle,
    mic_rx, 8_000,
    vec![
        Box::new(Denoiser::new(8_000)),          // noise first…
        Box::new(NoiseGate::new(1_600.0, 3)),    // …then level, on clean audio
    ],
    spk_tx, 8_000,
);
```

The order matters, because the same measurements draw a sharp boundary: a
speech *enhancer* preserves speech, so babble noise and a second talker in
the room pass through it untouched — in a two-talker scene (a far talker
degraded by distance level drop, spectral tilt, and room reflections),
every enhancer tested left the far talker's activations exactly where the
raw VAD had them, at every level down to −18 dB. What rejected the far
talker was the *gate*, with its threshold calibrated between the two
talkers' levels: at that setting it produced zero far-talker activations
and zero stuck-open time while keeping every near-talker utterance. Level
is the mono-microphone cue for "the person closer to the phone"; run the
gate after the denoiser so it reads levels off clean audio, and derive
its threshold from the caller's own first utterance rather than a
constant. The residual hard case — two people at equal level on one
speakerphone — is not solvable by level or enhancement; that is
target-speaker extraction or server-side semantics.

Heavier option: DeepFilterNet — itself a Rust project, tract CPU
inference at ~0.12× realtime — matches these results on the same
benchmark and preserves more speech quality at very low SNR. Its
inference crate is published only as a git dependency, which a crates.io
release cannot carry, so it slots in as an application-side
`impl MicProcessor` rather than an SDK feature.

### A learned VAD, already paid for

Suppression and detection are the same estimation problem — "which
time-frequency cells are speech" is both the gain mask and a voice
activity decision — and RNNoise computes both from one recurrent
network. `Denoiser::vad_probability()` exposes that second output: the
per-10 ms speech probability from the network's VAD head, free with the
denoising you are already running. It responds to the statistical
fingerprint of speech (pitch movement, formants, syllabic modulation),
not to level, which makes it a different instrument from both the energy
VAD and Google's WebRTC VAD (a GMM over spectral features). Measured on
the same benchmark, with the same 60 ms-onset / 300 ms-hangover decision
layer on every detector (*false activations / utterances detected of 3 /
% of noise-only time claimed as speech*):

| condition | energy VAD | WebRTC VAD (most conservative) | RNNoise head (0.5 threshold) |
|---|---|---|---|
| street traffic 10 dB | 1 / 3 / **95 %** | 4 / 3 / 53 % | **0 / 3 / 0 %** |
| street traffic 0 dB | 1 / 3 / **95 %** | 3 / 3 / 82 % | 1 / 3 / 4 % |
| pink 0 dB | 0 / **0** / 0 % (all missed) | 10 / 3 / 34 % | **0 / 3 / 0 %** |
| white 0 dB | 1 / 3 / 99 % | 1 / 3 / 100 % | 1 / 3 / 29 % |
| babble (any SNR) | open ~99 % | open ~100 % | open ~100 % |

Horns, engines, and ambience fool a level detector and a GMM alike; the
learned head shrugs them off — and the babble row is the honest boundary
shared by all three: background *speech* reads as speech on any
speaker-blind detector (see the two-talker discussion above). Two
caveats from the same runs: loud broadband white noise from a cold start
can hold the head high until its noise estimate converges, and the
probability is per-block noisy — always wrap it in hysteresis (on above
≈ 0.6, off below ≈ 0.3, ~300 ms hangover) rather than acting on one
block.

### Tuning the decision path

The client VAD has three knobs that matter, and they trade against each
other:

| knob | raising it buys | raising it costs |
|---|---|---|
| `start_threshold_db` | fewer false activations from noise residue | quiet speech missed |
| `min_speech_frames` | clicks and horn onsets rejected | +30 ms onset latency per frame |
| `hangover_frames` | no mid-word speech-end flapping | +30 ms per frame before `SpeechEnd` |

Tuned as a closed loop over labeled scenes (synthesized noise beds under
TTS utterances with known spans, swept clean → 0 dB, each setting scored
as `40·missed + 12·false + 1.5·stuck-open% + 0.05·onset-ms`), one
configuration dominates — and it only exists *behind the denoiser*:
**`VadConfig::noisy_street()`** (start 21 dB, stop 16 dB, 1-frame
confirm, 300 ms hangover). Cleaning the signal first is what lets the
threshold go **up** 6 dB (horn residue rejected) while the confirmation
delay goes **down** to one frame (~150–310 ms measured onset). The same
sweep run on the raw noisy stream finds no good setting at any threshold
— the adaptive floor latches regardless — which is why the preset's
documentation insists on the denoiser.

```rust,ignore
let vad = VoiceActivityDetector::new(VadConfig::noisy_street());
// …fed with frames that already passed through voice::Denoiser.
```

Validated end-to-end against a live Gemini session (26 s of continuous
0 dB street traffic streamed while the model spoke): the server's own
VAD fired zero false interruptions and barged in on every utterance at
~0.6–1.4 s regardless of the client chain — so leave interruption
authority to the server — while the client VAD needed the
denoiser + preset to stay useful (raw, it latched open within 600 ms
and never recovered; denoised + preset, zero false activations). The
client's decisions are what drive local playback ducking, latency
fillers, and soft-turn logic; the ~10 ms the denoiser adds is noise
against the server's barge-in path.

To re-tune for a specific deployment, replace the synthesized bed with a
30-second recording of the real site's noise (captured through the real
device path, so its AGC is in the loop), re-run the sweep, and prefer
the highest threshold that still detects every utterance — false-accept
robustness ages better than onset speed, because noise levels vary
day-to-day and the onset cost of one extra confirm frame is only 30 ms.

## One state vocabulary across transports

All of this composes because connectors share one session-state
vocabulary ([`telephony::bridge`](../api/gemini_adk_fluent_rs/telephony/bridge/index.html)):
`telephony:dtmf`, `telephony:dtmf_history`, `telephony:caller`,
`telephony:call_sid`, `telephony:stream_sid`. A flow guard like
`Guard::eq("telephony:dtmf", "1")` behaves identically whether the digits
arrived as Twilio protocol events, RFC 4733 telephone events on a SIP
leg, or AudioHook `dtmf` messages from a contact-center platform
(`examples/audiohook`, the third connector) — and will behave identically
on the next transport. See [Telephony](./telephony.md) for the
connector-side picture.
