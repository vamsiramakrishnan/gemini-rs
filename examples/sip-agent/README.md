# sip-agent — a Gemini Live agent any SIP endpoint can dial

No carrier in the path. This process terminates SIP signalling itself (via
[rsipstack](https://crates.io/crates/rsipstack)) and carries G.711-over-RTP
media in-process, so a softphone, an Asterisk/FreeSWITCH extension, or a SIP
trunk can call it directly. Each call gets its own governed Live session;
barge-in stops playback immediately, because the agent *is* its own RTP
buffer and drops it on interruption.

This is `gemini_adk_fluent_rs::telephony::sip` (feature `sip`) plus about a
hundred lines of glue — the same `telephony::bridge` the Twilio and AudioHook
examples use, so DTMF and latency-filler behaviour are shared.

## Run

```bash
export GEMINI_API_KEY=...            # or Vertex env — see connect_from_env
cargo run -p example-sip-agent       # listens on 0.0.0.0:5060/udp
```

Then, in a softphone (Linphone, Zoiper, …), call `sip:gemini@<host>` — no
registration needed. From Asterisk, a plain `Dial(SIP/gemini@<host>)` works.

| Variable            | Default                                                | Purpose                            |
|---------------------|--------------------------------------------------------|------------------------------------|
| `GEMINI_API_KEY`    | —                                                      | Google AI key (or the Vertex trio) |
| `GEMINI_LIVE_MODEL` | `models/gemini-2.5-flash-native-audio-preview-12-2025` | The Live model to dial             |
| `SIP_BIND_ADDR`     | `0.0.0.0:5060`                                         | UDP address for SIP + RTP          |
| `AGENT_INSTRUCTION` | a short receptionist prompt                            | System instruction for the session |

RTP uses ephemeral UDP ports on the same interface; if the agent sits behind
NAT, put it on a public address or a machine on the caller's network — SIP
media traversal is out of scope here.

DTMF (RFC 4733) is negotiated in the SDP and decoded from RTP into the
`telephony:*` state keys, so a flow guard can wait for a keypad entry exactly
as it would on Twilio.

See the book chapter *Contact-center connectors* for how the three transports
share one bridge.
