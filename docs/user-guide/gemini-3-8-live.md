# Gemini 3.8 Live

Gemini 3.8 Live (`gemini-3.8-live`, GA on Vertex AI since 2026-09-24) is the
current real-time model. Compared with Gemini 2.5 Flash Live Native Audio it
adds Live Avatar video output, custom transcription vocabulary, blocking tool
calls that the server cancels when the user speaks again, and polite handling
of `INTERRUPT`-scheduled tool responses. Affective dialogue and proactive
audio are always on, and thinking is not supported.

Select it with `ModelId::LIVE_3_8`. The platform default model is unchanged,
so existing sessions keep their current model until you opt in.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let handle = Live::builder()
    .model(ModelId::LIVE_3_8)
    .instruction(
        "You are an enterprise concierge. When a tool returns no results, tell \
         the user before calling it again. Never issue more than two \
         consecutive tool calls without speaking.",
    )
    .custom_vocabulary(["ORD-8472", "Express Shipping"])
    .transcription()
    .connect_from_env()
    .await?;
```

## What the SDK does for you

The setup message is shaped per model by `LiveModelProfile`, keyed on the
model name. For Gemini 3.8 Live:

| Setting | On the wire | Why |
|---|---|---|
| `.thinking(..)`, `.include_thoughts()` | left off | The model does not support thinking. |
| `.affective_dialog()` | left off | Always on. The guide says not to send `enable_affective_dialog`. |
| `.proactive_audio()` | left off | Always on. The guide says not to send `proactivity`. |
| Tool `behavior` (`BLOCKING` / `NON_BLOCKING`) | sent, including on Vertex AI | Earlier Vertex AI Live models reject it, so it is still stripped for them. |
| Tool response `scheduling` | sent, including on Vertex AI | Same as `behavior`. |

Anything left off is listed by `SessionConfig::ignored_settings()`, and
connecting logs it once as a warning. A setting without effect is visible, not
silent. Models the SDK has no profile for get every field passed through.

## Live Avatar

`.avatar(..)` makes the model answer with synchronized 24 FPS video and sets
the response modality to `VIDEO`, as the API requires. Video arrives on
`on_media` (L2), `EventCallbacks::on_media` (L1), `LiveEvent::Media`, or
`SessionEvent::Media` (L0), as `InlineMedia { mime_type, data }` chunks
(`video/mp4`). Speech still arrives on `on_audio`. Both are suppressed after a
barge-in.

```rust,ignore
Live::builder()
    .model(ModelId::LIVE_3_8)
    .voice(Voice::Puck)
    .avatar(AvatarConfig::prebuilt("Ben"))
    .on_media(|chunk| renderer.push(chunk.data.clone()))
    .on_audio(|pcm| speaker.push(pcm.clone()));
```

`AvatarConfig::custom(image, "png")` builds an avatar from a reference image
instead. The API wants a PNG portrait of at least 704×1280, under 5 MB. Custom
avatars and `replicated_voice(..)` (a voice cloned from a sample) are
allow-listed features. You are responsible for the consents and rights needed
to process a likeness or a voice.

Before this release, every inline part from the model was treated as audio. An
avatar stream would have been played through the speaker as noise.

## Transcription

```rust,ignore
Live::builder()
    .input_transcription_config(
        AudioTranscriptionConfig::default()
            .language_codes(["en-US", "es-US"])
            .custom_vocabulary(["QwikPay", "Xylotek"]),
    )
    .output_transcription();
```

Language hints (`language_codes`, BCP-47) reduce misdetected languages on
short utterances. `custom_vocabulary` biases recognition toward product names,
SKUs and proper nouns.

## Tools: informative responses

Gemini 3.8 Live retries a tool with varied arguments when a response does not
say why it came back empty. That can mean two to five sweeps before it speaks.
Return a status the model can act on. Never return an empty object or `null`:

```json
{
  "results": [],
  "status": "invalid_argument",
  "retryable": false,
  "message": "There is no provider named 'Mr. Johnson'. Ask the caller to confirm the name.",
  "valid_options": ["Dr. Johnson", "Dr. Chen"]
}
```

Pair this with a system-instruction retry policy, as in the example above.

A tool declared `FunctionCallingBehavior::Blocking` pauses the model until the
response arrives. If the user speaks meanwhile, the server cancels the
pending call. A response scheduled `INTERRUPT` while the user is speaking now
waits until they finish.

## Session lifecycle

- `.transparent_resumption()` makes each resumption update name the last
  client message the server consumed (`ResumeInfo::last_consumed_index`). A
  resumed session knows what to send again.
- `.history_in_client_content()` sets
  `historyConfig.initialHistoryInClientContent`. Gemini 3.8 Live requires it
  before it accepts history seeded with client content: send the turns after
  `setupComplete`, with `turnComplete: true` on the last.
- `.explicit_vad_signal()` asks the server for `voiceActivity` events at the
  edges of user speech. It is Vertex AI only and is left off the wire on
  Google AI.

## Audio formats

Unchanged: 16 kHz, 16-bit, little-endian mono PCM in (20–100 ms chunks), and
24 kHz mono PCM out.

## References

- [Developer's guide to Gemini 3.8 Live](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/guides/gemini-3-8-live)
- [Migrate from Gemini 2.5 Flash Live API to Gemini 3.8 Live](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/live-api/migrate-from-gemini-2-5-to-gemini-3-8-live)
- [Configure live avatars](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/live-api/configure-live-avatars)
