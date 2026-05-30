# gemini-adk-fluent-rs

Fluent developer experience for Gemini Live — builder API, operator algebra, and composition modules. This is the L2 (DX) crate, the highest-level entry point in the gemini-rs workspace.

## Features

- **`AgentBuilder`** — copy-on-write immutable builder for declarative agent configuration
- **S-C-T-P-M-A operators** — composable algebra for state, context, tools, phases, middleware, and agents
- **`Live` session** — callback-driven full-duplex voice/text event handling
- **Pre-built patterns** — common agent compositions ready to use
- **Full re-exports** — `gemini_adk_fluent_rs::prelude::*` re-exports all three
  layers (L0 `gemini_genai_rs`, L1 `gemini_adk_rs`, and L2 itself), so a
  single `use` statement is enough for most applications

## Quick Start

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let agent = AgentBuilder::new("assistant")
    .model(GeminiModel::Gemini2_0Flash)
    .instruction("You are a helpful assistant.")
    .build();
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk-fluent-rs)

## See Also

- [Cookbook examples](../../examples/cookbook) — end-to-end runnable examples
  using the fluent API.

## License

Apache-2.0
