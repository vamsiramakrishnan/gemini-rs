# audiohook — a Genesys-style contact-center bot server

The third phone transport, built from the same parts as the other two. Twilio
Media Streams and raw SIP ship inside the SDK; this example speaks the open
[AudioHook protocol](https://developer.genesys.cloud/devapps/audiohook/) that a
Genesys-style platform dials *out* to — and it is composed entirely from the
public surface (`voice::pump`, `telephony::g711`, `telephony::bridge`), with no
SDK changes. That is the point of it: if your platform is not one of the
built-ins, this is the template.

`src/protocol.rs` is the AudioHook state machine — pure, offline, tested
without a socket. `src/main.rs` is only the glue between a WebSocket and a
governed Live session.

## Run

```bash
export GEMINI_API_KEY=...            # or Vertex env — see connect_from_env
cargo run -p example-audiohook       # listens on 0.0.0.0:8080
```

Then point the platform's AudioHook integration at `wss://<public-host>/audiohook`
(`ngrok http 8080` during development). The platform's connection probe — a
handshake carrying the all-zeros conversation id — is answered without starting
a session, so the integration validates before any call is placed.

| Variable            | Default                            | Purpose                                            |
|---------------------|------------------------------------|----------------------------------------------------|
| `GEMINI_API_KEY`    | —                                  | Google AI key (or the Vertex trio)                 |
| `GEMINI_LIVE_MODEL` | `models/gemini-2.5-flash-native-audio-preview-12-2025` | The Live model to dial              |
| `BIND_ADDR`         | `0.0.0.0:8080`                     | Where the WebSocket server listens                 |
| `AGENT_INSTRUCTION` | a short assistant prompt           | System instruction for the session                 |
| `FILLER_CLIP`       | none                               | Path to a PCM clip played while the model is silent|

DTMF digits land in the same `telephony:*` state keys as the Twilio and SIP
paths, so one flow with one set of guards serves all three transports.

## Tests

```bash
cargo test -p example-audiohook      # protocol state machine + an in-process WS client
```

See the book chapter *Contact-center connectors* for how the three transports
share `telephony::bridge`.
