//! # adk-rs-fluent
//!
//! Fluent DX for Gemini — builder API, operator algebra, composition modules.
//! The highest-level crate in the rs-genai workspace.

pub mod builder;
pub mod compose;
pub mod operators;
pub mod patterns;
pub mod testing;

pub use rs_adk;
pub use rs_genai;

pub mod prelude {
    pub use crate::builder::*;
    pub use crate::compose::{A, C, M, P, S, T};
    pub use crate::operators::*;
    pub use crate::patterns::*;
    pub use crate::testing::*;
    pub use rs_adk::agent::*;
    pub use rs_adk::agent_session::*;
    pub use rs_adk::state::State;
    pub use rs_genai::prelude::*;
}
