#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
//! Build Gemini agents in Rust, from one crate.
//!
//! ```toml
//! [dependencies]
//! gemini-adk = "3"
//! tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
//! ```
//!
//! Each step below adds one idea to the one before.
//!
//! **Ask.** A model from the environment (`GEMINI_API_KEY`, or Vertex AI via
//! `GOOGLE_GENAI_USE_VERTEXAI` and `GOOGLE_CLOUD_PROJECT`), an agent, a
//! question:
//!
//! ```no_run
//! use gemini_adk::prelude::*;
//!
//! # async fn run() -> Result<(), AgentError> {
//! let agent = AgentBuilder::new("assistant")
//!     .instruction("Answer in one sentence.")
//!     .build(GeminiLlm::from_env()?)?;
//!
//! println!("{}", agent.ask("What is the Gemini Live API?").await?);
//! # Ok(()) }
//! ```
//!
//! **Get a typed answer.** The type is the schema. Typed answers and tool
//! argument types derive `serde::Deserialize` and `schemars::JsonSchema`, so
//! add `serde = { version = "1", features = ["derive"] }` and
//! `schemars = "0.8"` to use them.
//!
//! ```no_run
//! # use gemini_adk::prelude::*;
//! #[derive(serde::Deserialize, schemars::JsonSchema)]
//! struct City {
//!     name: String,
//!     country: String,
//! }
//!
//! # async fn run(agent: impl TextAgent) -> Result<(), AgentError> {
//! let city: City = agent.ask_as("Which city hosts the Louvre?").await?;
//! # Ok(()) }
//! ```
//!
//! **Hold a conversation, and stream it.**
//!
//! ```no_run
//! # use gemini_adk::prelude::*;
//! # async fn run(agent: impl TextAgent) -> Result<(), AgentError> {
//! let mut chat = agent.chat();
//! chat.send("Hi, I'm Ada.").await?;
//! let mut reply = chat.send_stream("What's my name?");
//! while let Some(event) = reply.next().await {
//!     if let RunEvent::TextDelta(text) = event? {
//!         print!("{text}");
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! **Give it a tool.** A documented `async fn` is a tool; the doc comment is
//! what the model reads:
//!
//! ```no_run
//! # use gemini_adk::prelude::*;
//! /// Get the current temperature for a city, in Celsius.
//! ///
//! /// # Arguments
//! ///
//! /// * `city` - The city name, e.g. "Paris".
//! #[tool]
//! async fn temperature(city: String) -> f64 {
//!     # let _ = city;
//!     21.5
//! }
//!
//! # fn run() -> Result<(), AgentError> {
//! let agent = AgentBuilder::new("weather")
//!     .tool(temperature())
//!     .build(GeminiLlm::from_env()?)?;
//! # Ok(()) }
//! ```
//!
//! **Test it without a model.** [`testing::MockLlm`] replies from a script
//! and records what the agent sent.
//!
//! From there: live voice sessions ([`live`], `Live::builder()`), governed
//! conversations that enforce which tools a step admits ([`conversation`],
//! [`flow`]), and model-free conversation tests ([`simulation`]).
//!
//! This crate re-exports [`gemini-adk-fluent-rs`](gemini_adk_fluent_rs); its
//! features have the same names here, plus `memory`, which adds
//! `gemini_adk::memory` (contextual memory for Live sessions).

#[doc(inline)]
pub use gemini_adk_fluent_rs::*;

/// Contextual memory for Live sessions (feature `memory`).
#[cfg(feature = "memory")]
#[doc(inline)]
pub use gemini_memory_rs as memory;
