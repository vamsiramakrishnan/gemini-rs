# API Reference

Two copies of the rustdoc exist, and they differ in one way that matters.

| Crate | Layer | This site (`main`, every feature) | docs.rs (the published version) |
|-------|-------|-----------------------------------|----------------------------------|
| `gemini-adk-fluent-rs` | L2 — the builder API most apps use | [gemini_adk_fluent_rs](./api/gemini_adk_fluent_rs/index.html) | [docs.rs](https://docs.rs/gemini-adk-fluent-rs) |
| `gemini-adk-rs` | L1 — agent runtime | [gemini_adk_rs](./api/gemini_adk_rs/index.html) | [docs.rs](https://docs.rs/gemini-adk-rs) |
| `gemini-genai-rs` | L0 — wire protocol | [gemini_genai_rs](./api/gemini_genai_rs/index.html) | [docs.rs](https://docs.rs/gemini-genai-rs) |
| `gemini-memory-rs` | contextual memory engine | [gemini_memory_rs](./api/gemini_memory_rs/index.html) | [docs.rs](https://docs.rs/gemini-memory-rs) |
| `gemini-adk-macros-rs` | the `#[tool]` attribute | [gemini_adk_macros_rs](./api/gemini_adk_macros_rs/index.html) | [docs.rs](https://docs.rs/gemini-adk-macros-rs) |

**Which to read.** docs.rs is versioned — it documents exactly the release you
have in `Cargo.lock`, and every feature-gated item carries an *available on
crate feature …* badge. This site tracks `main` and is built with **every**
feature, including `voice-io` (`talk()`, the microphone/speaker duplex), which
docs.rs cannot build because the `cpal` backend needs ALSA headers its builder
lacks. So: docs.rs for the API you depend on; this site for what is coming and
for the voice surface.

Both are built with `RUSTDOCFLAGS="-D warnings"`: a broken intra-doc link fails
the build, so a link on any of these pages goes somewhere.
