# gemini-genai-rs fuzz targets

libFuzzer targets for the Live frame decoder, the code that turns every
WebSocket frame from Google AI (text frames) and Vertex AI (binary frames)
into a `ServerMessage`.

| Target | Input | Checks |
|---|---|---|
| `decode_server_message` | raw bytes | both wire paths carry the same bytes; the decoder never panics; the paths agree on whether a frame decodes; close and control frames never reach the codec |
| `server_frame_structured` | a well-formed server frame drawn from the bytes (setup, content with audio/text/transcription parts, tool calls, cancellations, GoAway, resumption, VAD, usage, unknown keys) | the frame is classified by its top-level key; audio bytes survive base64 unchanged; tool calls and cancellation ids are kept; an unknown key never fails the frame |

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
cd crates/gemini-genai-rs/fuzz
cargo +nightly fuzz run decode_server_message -- -max_total_time=600
cargo +nightly fuzz run server_frame_structured -- -max_total_time=600
cargo +nightly fuzz cmin decode_server_message   # minimise before committing seeds
```

CI runs each target for one minute from the committed corpus (`just fuzz`
does the same locally).

## Corpus

`corpus/<target>/` holds the seeds: the golden wire fixtures from
`tests/fixtures/wire`, a few hand-written frames, and the 200 smallest
inputs of each target's minimised plateau corpus. The full plateau corpus
from the first ten-minute campaigns (2026-11-06, revision of this file's
first commit) was 3 700 and 1 783 inputs, 22 MB in all, at 4 577 and
4 068 coverage edges; it is not committed because of its size, and a
ten-minute run from the seeds reaches the plateau again.
