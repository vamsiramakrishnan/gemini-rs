# Gemini 3.8 Live

Gemini 3.8 Live (`gemini-3.8-live`, GA since 2026-09-24) is the current
real-time model, on both Vertex AI and Google AI (Gemini API key). Compared
with Gemini 2.5 Flash Live Native Audio it adds custom transcription
vocabulary, blocking tool calls that the server cancels when the user speaks
again, polite handling of `INTERRUPT`-scheduled tool responses, and, on Vertex
AI, Live Avatar video. Affective dialogue and proactive audio are always on,
and thinking is not supported. A separate model,
`gemini-3.8-live-extended-thinking`, does think (see below).

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

The setup message is shaped per model (`LiveModelProfile`, keyed on the model
name) and per platform. Everything below was measured against the live
Google AI endpoint on 2026-09-25. A "1007" is the close code the server sends
when it refuses a setup or a message.

| Setting | Gemini 3.8 Live on the wire | Why |
|---|---|---|
| `.thinking(..)`, `.include_thoughts()`, `.thinking_level(..)` | left off | The model has no thinking. A `thinkingLevel` is refused (1007), and a budget has no effect. |
| `.affective_dialog()` | left off | Always on. Sending `enableAffectiveDialog` closes the session on the first input (1007). |
| `.proactive_audio()` | left off | Always on, and Google AI has no `proactivity` field for any model (1007 at setup). |
| Tool `behavior` (`BLOCKING` / `NON_BLOCKING`) and response `scheduling` | sent | Earlier Vertex AI Live models reject both, so they are still stripped there. |

Google AI also has no `explicitVadSignal`, no `sessionResumption.transparent`,
and no `avatarConfig.avatarName` / `customizedAvatar`. Each of these is
refused at setup, so the SDK leaves them off on Google AI.

Anything left off is listed by `SessionConfig::ignored_settings()`, and
connecting logs it once as a warning. A setting without effect is visible, not
silent. Models the SDK has no profile for get every field passed through.

## Changing instructions mid-session

`update_instruction(..)`, which the default `InstructionUpdate` steering mode
uses on phase transitions, is sent differently per platform:

- **Vertex AI:** a `system`-role client content turn, as Vertex AI documents.
- **Google AI:** a user-role turn that says it replaces the instructions, with
  `turnComplete: false`. A `system` role closes the session on Google AI
  (1007); this was measured on Gemini 2.5, 3.1 and 3.8 Live. Gemini 3.1 and
  3.8 follow the update on the next turn. Gemini 2.5 accepts it without
  following it: for a persona change on 2.5, use `ContextInjection` steering
  or start a new session.

Context injected mid-session as client content with `turnComplete: false`
(`ContextInjection` steering, per-turn modifiers) is accepted by 3.8 without
triggering speech, and the model uses it. So is a tool response scheduled
`SILENT`: it adds its result to the context without the model speaking.

## Live Avatar (Vertex AI)

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

Avatars are a Vertex AI feature. On Google AI, `gemini-3.8-live` refuses the
`VIDEO` modality ("The requested combination of response modalities (VIDEO) is
not supported by the model"), and its avatar config has only the bitrate
fields.

`AvatarConfig::custom(image, "png")` builds an avatar from a reference image
instead. The API wants a PNG portrait of at least 704×1280, under 5 MB. Custom
avatars and `replicated_voice(..)` (a voice cloned from a sample) are
allow-listed features. You are responsible for the consents and rights needed
to process a likeness or a voice.

Before this release, every inline part from the model was treated as audio. An
avatar stream would have been played through the speaker as noise.

## Extended thinking

`ModelId::LIVE_3_8_EXTENDED_THINKING` (`gemini-3.8-live-extended-thinking`,
Google AI) thinks before answering, and requires a thinking level: a setup
without one is refused ("Thinking level must be specified for this model").

```rust,ignore
Live::builder()
    .model(ModelId::LIVE_3_8_EXTENDED_THINKING)
    .thinking_level(ThinkingLevel::Low);
```

A question that needs thought is answered in two turns. First comes a spoken
holding line ("Let me calculate that for you."), with
`SessionEvent::InteractionStatus("IN_PROGRESS")`, and the turn completes.
The answer then arrives on its own as the next turn, with no further input.
Code that treats the first `TurnComplete` as "the answer is in" must wait for
the next turn.

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
SKUs and proper nouns. Both are accepted on Google AI and Vertex AI.

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

- `.history_in_client_content()` sets
  `historyConfig.initialHistoryInClientContent`. Send the seed turns after
  `setupComplete`, with `turnComplete: true` on the last. With it, the model
  takes the turns in as history. Without it, it answers the seed as if the
  user had just spoken.
- `.transparent_resumption()` (Vertex AI) makes each resumption update name
  the last client message the server consumed
  (`ResumeInfo::last_consumed_index`). A resumed session then knows what to
  send again.
- `.explicit_vad_signal()` (Vertex AI) asks the server for `voiceActivity`
  events at the edges of user speech.

## Audio formats

Unchanged: 16 kHz, 16-bit, little-endian mono PCM in (20–100 ms chunks), and
24 kHz mono PCM out.

## References

- [Developer's guide to Gemini 3.8 Live](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/guides/gemini-3-8-live)
- [Migrate from Gemini 2.5 Flash Live API to Gemini 3.8 Live](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/live-api/migrate-from-gemini-2-5-to-gemini-3-8-live)
- [Configure live avatars](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/live-api/configure-live-avatars)
