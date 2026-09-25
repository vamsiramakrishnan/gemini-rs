# gemini-adk-fluent-rs

Build Gemini agents in Rust: ask, hold a streamed conversation, get typed
answers, give the model tools, govern a conversation, and talk to it by voice.
This is the crate to add to an application; it sits on the runtime
(`gemini-adk-rs`) and the wire layer (`gemini-genai-rs`) of the
[gemini-rs](https://github.com/vamsiramakrishnan/gemini-rs) workspace.

## Quick Start

```toml
[dependencies]
gemini-adk-fluent-rs = "3.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Export `GEMINI_API_KEY`, then:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("You are a concise assistant.")
        .build(GeminiLlm::from_env()?)?;

    // One question.
    println!("{}", agent.ask("Say hello in one sentence.").await?);

    // A conversation, streamed as the model writes.
    let mut chat = agent.chat();
    let mut reply = chat.send_stream("Now say it in French.");
    while let Some(event) = reply.next().await {
        if let RunEvent::TextDelta(text) = event? {
            print!("{text}");
        }
    }
    Ok(())
}
```

Typed answers (`agent.ask_as::<T>(..)`), tools (a documented `async fn` marked
`#[tool]`), and model-free tests (`testing::MockLlm`) are one step further;
the workspace README's [text agent](https://github.com/vamsiramakrishnan/gemini-rs#text-agent)
section walks through each, and every program there is compiled in CI as
[`examples/quickstart`](https://github.com/vamsiramakrishnan/gemini-rs/tree/main/examples/quickstart).

Voice needs the `voice-io` feature:

```rust,ignore
Live::builder()
    .instruction("You are a helpful concierge.")
    .greeting("Greet the caller.")
    .connect_from_env()
    .await?
    .talk()
    .await?;
```

## Features

| Feature | Default | Enables |
|---|---|---|
| `gemini-llm` | on | text generation through `GeminiLlm` (pure Rust) |
| `tls-native` | on | the TLS backend (`tls-rustls` is the alternative) |
| `voice-io` | off | the `talk()` microphone/speaker loop — without it there is no `talk()` method on the handle (Linux needs `libasound2-dev`) |
| `voice` | off | bundle: `voice-io` + `denoise` + `dsp` + `vad-wavekat`, everything a microphone application wants |
| `full` | off | bundle: `voice` + `sip` + `http-tools` + `templates` + `otel-otlp`, with the default TLS backend |
| `denoise`, `dsp`, `sip`, `http-tools`, `templates` | off | RNNoise stage, DSP chain, SIP agent, spec HTTP tools, Jinja instructions |

The prelude (`gemini_adk_fluent_rs::prelude::*`) carries the names a typical
application uses; everything else has a focused module one import away
(`live`, `text`, `tools`, `state`, `flow`, `conversation`, `testing`, `wire`, …).

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk-fluent-rs) · [The book](https://vamsiramakrishnan.github.io/gemini-rs/)

## See Also

- [Cookbook examples](../../examples/cookbook) — end-to-end runnable examples
  using the fluent API.

## License

MIT
