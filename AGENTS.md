# Working on gemini-rs

These instructions apply to any coding agent working in this repository.
Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development and release gates.
Use [the API working reference](docs/agent-reference.md) for imports and
examples; it is reference material, not a script to execute in full.

## Choose the implementation layer

- Application authoring: `crates/gemini-adk-fluent-rs`.
- Session state, tool dispatch, phases and callbacks: `crates/gemini-adk-rs`.
- Protocol, authentication and transport: `crates/gemini-genai-rs`.
- Independent contextual memory: `crates/gemini-memory-rs`.

Start application code with the fluent prelude, then import specialized APIs
from their named submodules. `Agent` is the runtime trait; `AgentBuilder` is
the fluent builder. See [migration](docs/user-guide/migration.md).

## Preserve behavior

Use atomic `State::modify` for read-modify-write operations. Choose strict
state access when a type mismatch must fail. Keep fast-lane callbacks short
and synchronous; move I/O to the control lane or a bounded worker.

Tool admission, confirmation, cancellation, and retries need separate tests.
Do not use an allow-all confirmation provider to make a production path pass.
Record/replay only substitutes the transport. Attached tools can still execute
external effects. Use controlled implementations in offline tests.

Do not infer a data-retention guarantee from transcript redaction. Check
fragment boundaries, telemetry, raw deltas, recordings, and handoff collection.
See [hardening](docs/user-guide/hardening.md).

## Build and validate

```bash
cargo check --workspace --locked
cargo test --workspace --lib --locked
cargo fmt --check
```

These model-free checks need no provider credentials. For the full pre-push
matrix, use `just ci` as described in CONTRIBUTING. Use the affected crate's
tests while iterating. Live provider and audio-device checks require their
own configuration and should be reported separately.

For documentation, edit `docs/src/` and `docs/user-guide/`. The website is
generated from those sources; `docs/src/SUMMARY.md` controls navigation.
Run `npm --prefix apps/docs run build`. Rustdoc checks are documented in
CONTRIBUTING. Do not edit generated agent code or generated website pages.

## Release

Release work uses `just release-preview`, `just release-dry <version>`, and
`just release <version>`. The last command creates a release branch and tag,
pushes, and opens a PR. Run it only for an authorized release. Version and
release-note rules are in the [working reference](docs/agent-reference.md#release-process).
