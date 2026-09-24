# quickstart — the README's programs, compiled

The programs in the root README's text-agent and voice sections live here as
real binaries, and its testing example as a real test:

| README step | Target | Shows |
|---|---|---|
| Ask a question | `hello-text` | `GeminiLlm::from_env`, `ask` |
| Hold a conversation | `chat` | `chat()`, `send_stream`, `RunEvent` |
| Get a typed answer | `typed` | `ask_as::<T>` |
| Give it a tool | `tool` | a documented `#[tool]` fn |
| Test without a model | `tests/agent_test.rs` | `MockLlm` |
| Talk to it | `hello-voice` | `Live::builder()…talk()` |

```bash
export GEMINI_API_KEY=...
cargo run -p example-quickstart --bin hello-text        # chat, typed, tool likewise
cargo test -p example-quickstart                        # no key needed
cargo run -p example-quickstart --bin hello-voice --features voice
```

`hello-voice` needs a microphone and, on Linux, `libasound2-dev`.

## Why this crate exists

The README is the first thing a reader compiles, and a snippet that has
drifted from the API is the worst first impression an SDK can make. So the
snippets are not copied into the README — the README *is* checked against
these files:

- `tests/readme_snippets.rs` fails if a README code block and the
  corresponding file here ever disagree (exact text, trailing whitespace aside),
- it also fails if the README's `Cargo.toml` block names a dependency version
  that would not accept the version this workspace ships, or if the
  `**vX.Y**` line under the badges falls behind a release.

Change the README and this crate together, or CI says no.
