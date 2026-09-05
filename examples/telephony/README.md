# telephony — a Gemini Live agent that answers real phone calls

Point a Twilio phone number's voice webhook at this server and the number
answers with a governed Live session. Twilio fetches TwiML from `POST /twiml`,
the TwiML opens a Media Stream back to `/media`, and from there the caller's
μ-law 8 kHz audio flows in, the model's voice flows out, and barge-in maps to
Twilio's `clear` so the agent stops the instant the caller speaks.

This is `gemini_adk_fluent_rs::telephony` (G.711 codecs, the Media Streams
protocol, the call bridge) behind a small axum server.

## Run

```bash
export GEMINI_API_KEY=...            # or Vertex env — see connect_from_env
cargo run -p example-telephony       # listens on 0.0.0.0:8080
ngrok http 8080                      # in another terminal, during development
```

Then in Twilio: **Phone Numbers → your number → Voice → A call comes in** →
webhook `https://<public-host>/twiml`, HTTP POST. Call the number.

| Variable            | Default                                                | Purpose                            |
|---------------------|--------------------------------------------------------|------------------------------------|
| `GEMINI_API_KEY`    | —                                                      | Google AI key (or the Vertex trio) |
| `GEMINI_LIVE_MODEL` | `models/gemini-2.5-flash-native-audio-preview-12-2025` | The Live model to dial             |
| `BIND_ADDR`         | `0.0.0.0:8080`                                         | Where the HTTP/WebSocket server listens |
| `AGENT_INSTRUCTION` | a short receptionist prompt                            | System instruction for the session |

Keypad presses arrive as Twilio `dtmf` events and land in the `telephony:*`
state keys — the same keys the SIP and AudioHook examples write, so one flow
serves all three transports.

See the book chapter *Contact-center connectors* for the shared bridge, the
latency filler, and warm handoff.
