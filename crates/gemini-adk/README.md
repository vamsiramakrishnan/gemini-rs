# gemini-adk

Build Gemini agents in Rust from one crate: text, streaming, typed output,
tools, governed conversations and live voice.

```rust,no_run
use gemini_adk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    let agent = AgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .build(GeminiLlm::from_env()?)?;

    println!("{}", agent.ask("What is the Gemini Live API?").await?);
    Ok(())
}
```

See the [crate documentation](https://docs.rs/gemini-adk) for the next steps:
typed answers, conversations, streaming, tools and testing without a model.
