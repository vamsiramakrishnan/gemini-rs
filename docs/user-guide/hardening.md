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
