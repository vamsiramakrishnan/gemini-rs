//! # gemini-live
//!
//! Fluent DX for Gemini Live — builder API, operator algebra, composition modules.
//! The highest-level crate in the gemini-live-rs workspace.

pub mod builder;
pub mod operators;

pub use gemini_live_runtime;
pub use gemini_live_wire;

pub mod prelude {
    pub use crate::builder::*;
    pub use crate::operators::*;
    pub use gemini_live_runtime::agent::*;
    pub use gemini_live_runtime::agent_session::*;
    pub use gemini_live_runtime::state::State;
    pub use gemini_live_wire::prelude::*;
}
