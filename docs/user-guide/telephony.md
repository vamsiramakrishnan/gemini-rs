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

## Raw SIP — no carrier in the path (feature `sip`)

`telephony::sip` terminates the call **in-process**: SIP signalling via
[`rsipstack`](https://docs.rs/rsipstack) (the stack under the `rustpbx`
PBX), and G.711-over-RTP media built from this crate's own pure layers
(`telephony::rtp`, `telephony::sdp`, `telephony::g711`). Any SIP endpoint —
a softphone, an Asterisk/FreeSWITCH extension, a provider's SIP trunk —
dials the agent directly:

```rust,ignore
use gemini_adk_fluent_rs::telephony::sip::SipAgent;

let mut agent = SipAgent::bind("0.0.0.0:5060".parse()?).await?;
while let Some(incoming) = agent.next_call().await {
    println!("call from {}", incoming.from);       // screen before answering
    let session = Live::builder()
        .instruction("Answer the phone politely.")
        .greeting("Greet the caller.")
        .connect_from_env().await?;
    let call = incoming.answer(&session).await?;   // SDP answer + RTP starts
    tokio::spawn(async move { call.ended().await; });
}
```

What `answer` does: picks the offer's preferred G.711 law (μ-law or A-law),
binds an RTP socket, sends the SDP answer in the 200 OK, and runs a paced
20 ms media loop on the same `voice::pump`. Media is **symmetric RTP** — it
sends toward the offer's address but re-latches onto the source of the
first arriving packet, so NATted softphones work. Barge-in on raw RTP has
no carrier buffer to clear: the agent *is* the buffer, and `Playback::Flush`
drops it. The caller hanging up (BYE) resolves `call.ended()`;
`call.hangup()` sends BYE the other way.

`examples/sip-agent` is the runnable end:

```bash
export GEMINI_API_KEY=...
cargo run -p example-sip-agent    # 0.0.0.0:5060/udp
# From Linphone/Zoiper on the same network: call sip:gemini@<host>
```

### Registering with a PBX or trunk

A directly dialed agent needs a fixed address. To be reached through a PBX
extension or a provider's SIP trunk instead, register it:

```rust,ignore
use gemini_adk_fluent_rs::telephony::sip::{SipAccount, SipAgent};

let mut agent = SipAgent::bind("0.0.0.0:5060".parse()?).await?;
let registration = agent
    .register(SipAccount::new("sip:pbx.example.com", "agent", password))
    .await?; // fails here on a wrong password or an unreachable registrar
// ... agent.next_call() as above ...
registration.unregister().await?;
```

The first REGISTER, including the digest challenge, completes before
`register` returns. After that the binding is refreshed at three quarters of
the lifetime the registrar granted. A failed refresh is retried every 30 s
and shows as `RegistrationState::Retrying` on `registration.watch()`. The
Contact is corrected by the address the registrar reports seeing, so an
agent behind NAT stays reachable. `SipAccount::contact` overrides it.
Dropping the registration, or calling `unregister`, removes the binding.
`SipAccount`'s `Debug` output never shows the password.

### Encrypted media (SRTP)

An offer on the secure profile (`RTP/SAVP`) with SDES keys (`a=crypto`) is
answered with SRTP (RFC 3711). The answer accepts the first offered suite
the agent supports, `AES_CM_128_HMAC_SHA1_80` or `AES_CM_128_HMAC_SHA1_32`,
and carries a fresh key for the agent's direction. Inbound packets that fail
authentication, or replay an earlier packet, are dropped before decoding. A
secure offer with no supported suite is refused with 488, and `answer`
returns `SipError::NoCommonCrypto`. `incoming.offer.secure` tells you which
kind of call it is before you answer, so a deployment that requires
encryption can reject plain calls.

SDES carries the keys in the SDP body, so media is only as private as the
signalling. Use SIP over TLS on the trunk when the keys must not be readable
on the network. SRTCP, MKIs and DTLS-SRTP are not supported.

`telephony::srtp` is the packet layer on its own (`SrtpSession::protect` /
`unprotect`, `CryptoAttribute` for the SDP line) for other RTP transports.
It is checked against the RFC 3711 key-derivation and keystream test vectors
and libsrtp's reference packet.

The signalling layer is covered by in-repo tests that drive hand-written SIP
peers against a bound agent:

- OPTIONS → 200, then INVITE → ringing → surfaced call → reject → final
  failure;
- registration through a digest challenge, then removal;
- a refused registration;
- an SRTP call where the caller's encrypted audio reaches the model, audio
  under a wrong key does not, and the model's audio comes back as SRTP
  under the answered key.

## Keypad entry, masking and recording

Every bridge moves the caller's audio through a `KeypadGuard` before the
model hears it.

- **Keypad tones never reach the model.** Frames carrying DTMF tones are
  silenced, from the moment a tone is recognized until 60 ms after it
  ends. At most one detection block (26 ms) of a tone's onset can get
  through, which is too short to decode.
- **Where digits come from.** They come from the platform when it reports
  them: Twilio `dtmf` events, or RFC 4733 on SIP. On a SIP leg without
  telephone-events, the in-band detector (`telephony::dtmf::DtmfDetector`,
  Goertzel) records them from the audio.
- **Masking card numbers and PINs.** Set `telephony:keypad_mask` to `true`,
  for example from the tool that starts a payment. While it is set:
  - the caller's audio is replaced with silence, both for the model and
    for the recording;
  - keypresses go to `telephony:keypad_entry` instead of `telephony:dtmf`.
    That key is marked sensitive, so tools read it but journals, exports
    and telemetry see it redacted;
  - `telephony:keypad_len` counts the digits;
  - `#` sets `telephony:keypad_done`.

  Set the mask back to `false` to resume the conversation.

```rust,ignore
use gemini_adk_fluent_rs::telephony::bridge::{KEY_KEYPAD_ENTRY, KEY_KEYPAD_MASK};

// In the tool that starts card entry:
state.set(KEY_KEYPAD_MASK, true)?;
// ...later, once `telephony:keypad_done` is true:
let card: String = state.get(KEY_KEYPAD_ENTRY).unwrap_or_default();
state.set(KEY_KEYPAD_MASK, false)?;
```

**Recording a call.** `TwilioCall::attach_with(&session, CallOptions { recorder: Some(..), .. })`
records the call as a stereo WAV, with the caller on the left channel and
the agent on the right (`telephony::recorder::CallRecorder`).

- Both channels sit on the call's clock. Agent audio is placed where
  playback reached it, not when the model generated it.
- On barge-in, the agent audio that was queued but never played is cut, so
  the file holds what the caller actually heard.
- The file is written as the call goes, so memory stays flat for calls of
  any length.

```rust,ignore
use std::sync::Arc;
use gemini_adk_fluent_rs::telephony::{recorder::CallRecorder, CallOptions, TwilioCall};

let recorder = Arc::new(CallRecorder::create("call.wav", 8_000)?);
let call = TwilioCall::attach_with(&session, CallOptions { recorder: Some(recorder.clone()), ..Default::default() });
// ...when the call ends:
recorder.finish()?;
```

**What the caller heard.** Every bridge reports its playback to the
session, so on barge-in the model's side of the turn is cut to what the
caller heard: in the transcript, the final output transcript callback and
the verbatim check (see
[what the listener heard](./live-callbacks.md#what-the-listener-heard)).
The Twilio bridge also sets `telephony:unplayed_ms`: how much of the reply
had been sent but not yet played. Agent audio goes out in 20 ms frames, and
the last partial frame is padded once the model pauses.

**Audio quality.** Audio moving between 8 kHz on the line and 16 or 24 kHz
in the session is resampled by `voice::StreamResampler`.

- It is a windowed-sinc polyphase filter that keeps its state across
  chunks.
- On the way down to 8 kHz it removes content above 4 kHz instead of
  folding it back into the voice band. A 7 kHz component is attenuated by
  more than 40 dB, where the previous linear interpolator passed it at full
  level.
- It adds no clicks at chunk boundaries.

## Bringing your own transport

Twilio and raw SIP are two connectors on one seam. A contact-center
platform that exposes a virtual-agent slot — a gRPC bidirectional audio
stream (as in Cisco Webex CC's BYoVA or Genesys AudioHook style
integrations), a proprietary media WebSocket, anything that can move
20 ms audio frames both ways — becomes a third connector by composing the
same parts:

1. **Audio**: decode inbound frames to mono PCM16 and feed
   [`voice::pump`](../api/gemini_adk_fluent_rs/voice/fn.pump.html) (or
   `pump_processed`, for a denoiser chain) at the transport's native
   rate; encode `Playback::Chunk` back out; treat `Playback::Flush` as
   the platform's "stop playback" action — that is barge-in.
2. **Semantics**: land keypresses and call identity through
   [`telephony::bridge`](../api/gemini_adk_fluent_rs/telephony/bridge/index.html)
   (`record_dtmf`, the shared `telephony:` state keys), so flows and
   guards behave identically across transports.
3. **Hardening**: attach the latency filler and the handoff recorder from
   the same module — see [Hardening a Voice Deployment](./hardening.md).

This is not a hypothetical: [`examples/audiohook`](https://github.com/vamsiramakrishnan/gemini-rs/tree/main/examples/audiohook)
is a third connector built exactly this way, speaking the open
[AudioHook protocol](https://developer.genesys.cloud/devapps/audiohook/) a
Genesys-style contact-center platform dials out to — JSON text frames for
the session lifecycle, binary μ-law frames for audio, both directions. The
wire dialect (envelope sequencing, media negotiation, position tracking,
the platform's connection probe) lives in one pure state machine with its
own offline test suite; the glue between it and a governed session is one
`select!` loop. Barge-in becomes an AudioHook `barge_in` event, DTMF lands
in the same `telephony:*` keys, and the latency filler attaches with one
line — no SDK changes anywhere.

`TwilioCall::attach` (~100 lines) and the AudioHook example's
`ServerSession` are the references: each owns no socket, speaks its
platform's dialect at the edges, and delegates everything else to the
shared components. A new connector should look like them.

## Choosing a path

| | Twilio Media Streams | Raw SIP (`sip` feature) | AudioHook (example) |
|---|---|---|---|
| Who terminates PSTN | Twilio (also Vonage/Telnyx equivalents) | your SIP trunk / PBX | the contact-center platform |
| Transport | WebSocket you host | UDP SIP + RTP or SRTP in-process | WebSocket you host |
| Barge-in | `clear` message to carrier | drop own RTP buffer | `barge_in` event to platform |
| DTMF | ✓ (`telephony:dtmf*` state) | ✓ (RFC 4733, same state keys) | ✓ (`dtmf` message, same state keys) |
| Extra dependency | none (any WS server) | `rsipstack` | none (any WS server) |
