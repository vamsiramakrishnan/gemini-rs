# quickstart — the README's programs, compiled

The two programs in the root README's Quickstart live here as real binaries:

| README section | Binary        | Needs                         |
|----------------|---------------|-------------------------------|
| 2. Say hello   | `hello-text`  | `GEMINI_API_KEY`              |
| 3. Talk to it  | `hello-voice` | a key, a mic, `libasound2-dev` on Linux |

```bash
export GEMINI_API_KEY=...
cargo run -p example-quickstart --bin hello-text
cargo run -p example-quickstart --bin hello-voice --features voice
```

## Why this crate exists

The README is the first thing a reader compiles, and a snippet that has
drifted from the API is the worst first impression an SDK can make. So the
snippets are not copied into the README — the README *is* checked against
these files:

- `tests/readme_snippets.rs` fails if a README code block and the
  corresponding binary here ever disagree (exact text, trailing whitespace aside),
- it also fails if the README's `Cargo.toml` block names a dependency version
  that would not accept the version this workspace ships, or if the
  `**vX.Y**` line under the badges falls behind a release.

Change the README and this crate together, or CI says no.
