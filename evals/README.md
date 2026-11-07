# evals

Measurement harnesses. None is built or run by CI: each talks to a live
model, needs its own toolchain, or runs for longer than a CI job should.
Their numbers are reported with the revision and method that produced them.

| Directory | What it measures | Why it is outside the workspace |
|---|---|---|
| `code-mode` | Code mode against direct function calls on live Gemini | Live calls; needs a newer toolchain than the workspace (own `rust-toolchain.toml`) |
| `dspbench` | Whether the microphone DSP chain improves VAD decisions | Own workspace and lockfile; long runs over recorded audio |
| `python-vs-rust` | CPU, memory and audio timing of a Live client in Python (`google-genai`) and Rust | Own lockfile; a load harness that never runs in CI |
| `turnbench` | Turn-taking (end-of-turn prediction) quality; Python plus a `predictor` crate | Live calls and datasets |

Each directory is listed in the root `Cargo.toml` `[workspace].exclude`.
