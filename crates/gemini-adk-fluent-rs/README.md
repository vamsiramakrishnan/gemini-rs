# gemini-adk-fluent-rs

Fluent developer experience for Gemini Live — builder API, operator algebra, and composition modules. This is the L2 (DX) crate, the highest-level entry point in the [gemini-rs](https://github.com/vamsiramakrishnan/gemini-rs) workspace and the one to add to your application.

## Features

- **`AgentBuilder`** — copy-on-write immutable builder for declarative agent configuration
- **S-C-T-P-M-A operators** — composable algebra for state, context, tools, phases, middleware, and agents
- **`Live` session** — callback-driven full-duplex voice/text event handling
- **Pre-built patterns** — common agent compositions ready to use
- **A kernel prelude** — `gemini_adk_fluent_rs::prelude::*` re-exports the ~40
  types a typical application touches (builders, the algebra, `Live`, `State`,
  core tools/flow/errors, the L0 wire prelude); everything else has a focused
  home one import away (`live`, `text`, `tools`, `state`, `flow`, `agents`,
  `llm`, `conversation`, `wire`, …)

## Quick Start

```toml
[dependencies]
gemini-adk-fluent-rs = { version = "1.0", features = ["voice-io"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | Default | Enables |
|---|---|---|
| `gemini-llm` | on | text generation through `GeminiLlm` (pure Rust) |
| `tls-native` | on | the TLS backend (`tls-rustls` is the alternative) |
| `voice-io` | off | the `talk()` microphone/speaker loop — without it there is no `talk()` method on the handle (Linux needs `libasound2-dev`) |
| `denoise`, `dsp`, `sip`, `http-tools`, `templates` | off | RNNoise stage, DSP chain, SIP agent, spec HTTP tools, Jinja instructions |

Export `GEMINI_API_KEY`, then:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Text: ask one question.
    let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));
    let agent = AgentBuilder::new("assistant")
        .instruction("You are a concise assistant.")
        .build(llm)?;
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
