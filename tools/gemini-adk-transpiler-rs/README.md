# gemini-adk-transpiler-rs

A maintainer tool: it transpiles the ADK-JS, js-genai and adk-fluent type
definitions to Rust. Its one consumer in this repository is
`crates/gemini-adk-rs/src/agents/generated.rs`, regenerated with:

```bash
cargo run -p gemini-adk-transpiler-rs -- transpile --source <path> --output crates/gemini-adk-rs/src/agents/generated.rs
```

Do not edit `generated.rs` by hand; change the transpiler or the source
definitions and regenerate (see CONTRIBUTING, "Hand-Written vs Generated
Code").

## What it generates on purpose

The ADK-JS agents it transpiles have no Live path, so the generated
`run_live` bodies are `todo!("<Agent>::run_live")` stubs, and callback
types it cannot express carry a `// TODO: Implement Rust callback type`
comment. Those strings are the transpiler's output, not unfinished work in
this crate; its tests assert them. A generated agent that is used on the
Live path has to implement `run_live` by hand.

## Status

Not published. Compiled and unit-tested by the workspace CI jobs; the
generated file it maintains is checked in, so a transpiler change shows up
as a diff to `generated.rs`.
