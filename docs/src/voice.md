# Voice I/O: `pump()` and `talk()`

The Live API speaks PCM16 — 16 kHz in, 24 kHz out. Everything between a
microphone and that contract — resampling, channel down-mix, playback
buffering, and *barge-in* (the user speaks over the model; buffered speech
must vanish **now**) — is plumbing every voice application needs and none
should write. `gemini_adk_fluent_rs::voice` is that plumbing, engineered as
two primitives.

## `talk()` — the five-line voice app

*(feature `voice-io`; Linux needs `libasound2-dev`)*

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

Live::builder()
    .instruction("You are a helpful concierge.")
    .greeting("Greet the caller.")
    .connect_from_env().await?
    .talk().await?;      // microphone in, speakers out, barge-in handled
```

`talk()` runs the whole loop on the system's default devices via `cpal`:
capture at the device's native rate, playback with drain signaling wired back
into the session's voice reactor, Ctrl-C or session end to stop. It is a
thin facade over `pump()` — nothing it does is device-magic you can't
reproduce.

## `pump()` — the device-independent duplex core

```rust,ignore
use gemini_adk_fluent_rs::voice::{pump, Playback};
use tokio::sync::mpsc;

let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
let (spk_tx, mut spk_rx) = mpsc::channel::<Playback>(64);

// 8 kHz both ways — e.g. a telephony bridge.
let running = pump(&handle, mic_rx, 8_000, spk_tx, 8_000);

// Feed microphone frames at your rate; consume playback at yours.
while let Some(instr) = spk_rx.recv().await {
    match instr {
        Playback::Chunk(samples) => { /* play or encode */ }
        Playback::Flush => { /* drop everything buffered — barge-in */ }
    }
}
```

`pump()` owns no devices. Feed it mono PCM16 frames at **any** sample rate on
one channel; receive `Playback` instructions at **any** sample rate on
another. It resamples both directions and, crucially, turns an interruption
into an explicit `Playback::Flush` so stale audio is dropped, never played.

That device independence is what makes the rest of the SDK compose:

- **`Talk::talk()`** pairs it with `cpal` streams.
- **[Telephony](./user-guide/telephony.md)** pairs it with a Twilio Media
  Streams socket (flush becomes Twilio's `clear`) or an in-process RTP loop
  (the agent *is* the playout buffer, so flush drops its own send queue).
- **Tests** drive it with plain channels — the resampler, down-mix, and
  event→playback policy are pure functions with unit tests, so the audio
  path is testable without a device or a session.

## The fast-lane contract still applies

`pump()` lives on the consuming side of the session's event stream; your
speaker channel should keep up or be bounded. If you also register raw
callbacks (`on_audio`, `on_vad_*`), the
[fast-lane rules](./user-guide/live-callbacks.md) apply: sync, sub-millisecond,
`try_send` only. For lossy workloads (dashboards, meters), see the per-event
delivery policies (`.lossy_audio()`, `.lossy_transcript()`).
