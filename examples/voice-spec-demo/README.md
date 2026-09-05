# voice-spec-demo — a whole phone call from one JSON document

`spec.json` is the restaurant cookbook from the Flow Studio gallery, edited by
hand: audio modality, a named voice, memory section removed. That is the point —
the document the Studio edits is just JSON you can edit too, and this binary
runs the whole call from it with no hand-written agent code.

What it does, in order:

1. loads and validates the spec, then replays its embedded tests offline;
2. applies it to a Live session (`SessionSpec::apply`) — governed flow, mock
   tools, computed state, watchers and runtime tuning all come from the document;
3. plays the *caller* with Gemini TTS (`generateContent`, `AUDIO` modality);
4. bridges the caller's audio into the session through `voice::pump` — the
   same device-independent duplex core `talk()` and the telephony examples use;
5. records both sides to `voice-spec-demo.wav` and prints the transcript with
   the flow's live state after every turn.

## Run

```bash
export GEMINI_API_KEY=...
cargo run -p example-voice-spec-demo
```

No microphone or speaker is involved: the caller is synthesised and the
output is a file, so this runs unattended — it is the recording linked from
the docs. Two models are used, both pinned at the top of `src/main.rs`: a
native-audio Live model for the agent and a TTS model for the caller.

## Change the call

Edit `spec.json` — or open it in Flow Studio (`just run-studio`, then **Open**),
change the flow, download, and run again. The embedded tests replay before the
call, so a broken edit fails in milliseconds rather than after a minute of audio.
