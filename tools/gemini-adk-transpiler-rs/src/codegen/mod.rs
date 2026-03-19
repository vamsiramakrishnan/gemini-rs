//! Code generation from extracted schemas to Rust source.

pub mod adk;
pub mod genai;
pub mod rest;

// Re-export the main entry points.
pub use adk::{generate, generate_compilable, generate_compilable_with_genai};
pub use genai::generate_genai_modules;
pub use rest::generate_rest_modules;
