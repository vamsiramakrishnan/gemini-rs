# Telephony — answering phone calls

The public phone network delivers audio as G.711 (μ-law or A-law) at 8 kHz;
the Live API expects PCM16 at 16 kHz in and produces 24 kHz out. The
`gemini_adk_fluent_rs::telephony` module bridges the two on top of
[`voice::pump`](./layers.md) — the same device-independent duplex core that
drives a local microphone — so a phone call gets the same guarantees as any
other voice surface: resampling both directions, and barge-in that *drops*
buffered speech rather than playing over the caller.

```text
caller ──μ-law 8k──▶ Twilio ──WS JSON──▶ TwilioCall ──PCM16──▶ pump ──16k──▶ Live session
caller ◀─μ-law 8k── Twilio ◀──WS JSON── TwilioCall ◀─Playback── pump ◀─24k── model audio
                                        Playback::Flush  ⇒  {"event":"clear"}   (barge-in)
```

## Twilio Media Streams

[Media Streams](https://www.twilio.com/docs/voice/media-streams) forks a live
call over a WebSocket: μ-law 8 kHz frames arrive base64-encoded in JSON text
messages; JSON messages you send play back to the caller. `TwilioCall`
adapts that protocol onto a connected session — it owns no socket, so it
works with any WebSocket server:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::telephony::TwilioCall;

let session = Live::builder()
    .model(GeminiModel::Custom("models/gemini-2.5-flash-native-audio-preview-12-2025".into()))
    .instruction("You are the front desk. This is a voice call — keep it short.")
    .greeting("Greet the caller and ask how you can help.")
    .connect_from_env()
    .await?;

let mut call = TwilioCall::attach(&session);
loop {
    tokio::select! {
        Some(Ok(msg)) = ws.recv() => {
            if let Message::Text(text) = msg {
                if call.from_twilio.send(text).await.is_err() { break; }
            }
        }
        Some(frame) = call.to_twilio.recv() => ws.send(Message::Text(frame)).await?,
        else => break,
    }
}
```

Forward Twilio's text frames into `from_twilio`; forward everything from
`to_twilio` back over the socket. The bridge handles the rest:

- **Inbound audio**: `media` frames are μ-law-decoded to PCM16 @ 8 kHz and
  pumped into the session (resampled to 16 kHz).
- **Outbound audio**: session audio comes back at 8 kHz, is μ-law-encoded,
  and sent as `media` frames.
- **Barge-in**: an interruption becomes Twilio's `clear` message, which
  drops every frame Twilio has buffered server-side. Without this, the
  model keeps "speaking" for seconds after the caller interrupts — the
  classic broken-telephony-bot failure.
- **DTMF**: keypad digits land in session state — `telephony:dtmf` (last
  digit) and `telephony:dtmf_history` (all digits, in order) — where flow
  guards, watchers, and phase transitions read them like any other fact:

  ```rust,ignore
  .phase("gather_account")
      .transition("verify", S::eq("telephony:dtmf_history", json!("1234")))
  ```

- **Call metadata**: `telephony:call_sid` and `telephony:stream_sid` are set
  from the `start` frame; `<Parameter>` values from your TwiML arrive in
  [`StartMeta::custom_parameters`].

## The runnable example

`examples/telephony` is a complete phone agent — an axum server with two
routes:

- `POST /twiml` — the Twilio voice webhook. Returns
  `<Response><Connect><Stream url="wss://<host>/media"/></Connect></Response>`,
  deriving the host from the request so it works behind a tunnel.
- `GET /media` — the Media Streams WebSocket. One Live session per call.

```bash
export GEMINI_API_KEY=...
cargo run -p example-telephony       # listens on 0.0.0.0:8080
ngrok http 8080                      # during development
# Twilio Console → your number → Voice → webhook: https://<ngrok-host>/twiml (POST)
```

Call the number: the agent greets you, and you can interrupt it mid-sentence.

## G.711 codecs

`telephony::g711` implements both ITU-T G.711 companding laws as pure
functions — `encode_ulaw` / `decode_ulaw` (North America, Japan, Twilio) and
`encode_alaw` / `decode_alaw` (most SIP trunks elsewhere) — tested for
monotonicity and bounded quantisation error. Combined with
[`voice::resample`], they are the building blocks for any other telephone
transport.

## Scope: what "SIP support" means here

Twilio (and services like it — Vonage, Telnyx have equivalent stream APIs)
terminates SIP/PSTN for you and hands audio over a WebSocket; this module
speaks that hand-off. Terminating **raw SIP/RTP in-process** (no
carrier-stream service in the path) is a different engineering problem —
SIP signalling, RTP/SRTP, jitter at the network layer — tracked on the
roadmap as the `rustpbx`/`rsipstack` integration ("single-binary
telephony"). The seam is ready for it: any transport that can produce PCM16
frames and consume `Playback` instructions attaches to the same pump.
