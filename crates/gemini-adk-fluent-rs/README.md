# gemini-adk-fluent-rs

Fluent developer experience for Gemini Live — builder API, operator algebra, and composition modules. This is the L2 (DX) crate, the highest-level entry point in the [gemini-rs](https://github.com/vamsiramakrishnan/gemini-rs) workspace and the one to add to your application.

## Features

- **`AgentBuilder`** — copy-on-write immutable builder for declarative agent configuration
- **S-C-T-P-M-A operators** — composable algebra for state, context, tools, phases, middleware, and agents
- **`Live` session** — callback-driven full-duplex voice/text event handling
- **Pre-built patterns** — common agent compositions ready to use
- **Full re-exports** — `gemini_adk_fluent_rs::prelude::*` re-exports all three
  layers (L0 `gemini_genai_rs`, L1 `gemini_adk_rs`, and L2 itself), so a
  single `use` statement is enough for most applications

## Quick Start

```toml
[dependencies]
gemini-adk-fluent-rs = { version = "1.0", features = ["gemini-llm", "voice-io"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

`gemini-llm` enables text generation through `GeminiLlm` (off by default);
`voice-io` enables the `talk()` microphone/speaker loop (Linux needs
`libasound2-dev`). Export `GEMINI_API_KEY`, then:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Text: ask one question.
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));
    let agent = AgentBuilder::new("assistant")
        .instruction("You are a concise assistant.")
        .build(llm);
    let state = State::new();
    state.set("input", "Say hello in one sentence.")?;
    println!("{}", agent.run(&state).await?);

    // Voice: microphone in, speakers out, barge-in handled.
    Live::builder()
        .instruction("You are a helpful concierge.")
        .greeting("Greet the caller.")
        .connect_from_env()
        .await?
        .talk()
        .await?;
    Ok(())
}
```

The same two programs are compiled in CI as
[`examples/quickstart`](https://github.com/vamsiramakrishnan/gemini-rs/tree/main/examples/quickstart),
and the workspace README's [Quickstart](https://github.com/vamsiramakrishnan/gemini-rs#quickstart)
walks through them line by line.

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk-fluent-rs) · [The book](https://vamsiramakrishnan.github.io/gemini-rs/)

## See Also

- [Cookbook examples](../../examples/cookbook) — end-to-end runnable examples
  using the fluent API.

## License

MIT
