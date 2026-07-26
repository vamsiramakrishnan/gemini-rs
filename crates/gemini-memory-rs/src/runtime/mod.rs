//! Wiring the memory engine into a Gemini Live session.
//!
//! Three pieces: [`keys`] names the shared state, [`events`] is the
//! sub-millisecond bridge from Live's fast lane, and [`control`] is the single
//! task that does everything the fast lane must not.
//!
//! ```no_run
//! use std::sync::Arc;
//! use gemini_memory_rs::prelude::*;
//! use gemini_memory_rs::runtime::{events::channel, control::run_memory_control_loop, tools};
//!
//! # async fn wire() -> Result<(), MemoryError> {
//! # let state = gemini_adk_rs::state::State::new();
//! let engine = MemoryEngine::in_memory(UserId::new("usr_72ab"));
//! engine.compile_index().await?;
//!
//! let session = Arc::new(engine.begin_session(SessionId::new("ses_01")));
//! let (sender, receiver) = channel(256);
//! tokio::spawn(run_memory_control_loop(receiver, session.clone(), state));
//!
//! // In `Live::builder()`:
//! //   .on_input_transcript({
//! //       let sender = sender.clone();
//! //       move |text, is_final| { sender.input_transcript(turn, text, is_final); }
//! //   })
//! //   .with_tools(tools::recall_context_tool(session.clone()))
//! let _ = (sender, tools::recall_context_tool(session));
//! # Ok(())
//! # }
//! ```

pub mod control;
pub mod events;
pub mod keys;
pub mod tools;

pub use control::{run_memory_control_loop, snapshot_for_turn};
pub use events::{channel, MemoryEventSender, MemoryRuntimeEvent, DEFAULT_CHANNEL_DEPTH};
pub use tools::{manage_memory_tool, recall_context_tool, MANAGE_TOOL, RECALL_TOOL};
