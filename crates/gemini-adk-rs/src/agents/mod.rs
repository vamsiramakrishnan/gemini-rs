//! Agent composition primitives — Sequential, Parallel, Loop.
//!
//! These implement the Agent trait and compose sub-agents in different patterns.
//! They work at the InvocationContext level, passing the context to sub-agents.

pub mod loop_agent;
pub mod parallel;
pub mod sequential;

// Auto-generated agent definitions from ADK-JS transpiler.
// Run `cargo run -p gemini-adk-transpiler-rs -- transpile --source <path> --output crates/gemini-adk-rs/src/agents/generated.rs`
// to regenerate.
// Crate-private: these are transpiler shadow types, not public API.
#[path = "generated.rs"]
// Generated code is exempt from clippy (it mirrors ADK-JS naming verbatim).
#[allow(clippy::all, missing_docs, rustdoc::bare_urls, unreachable_pub)]
pub(crate) mod generated;

pub use loop_agent::LoopAgent;
pub use parallel::ParallelAgent;
pub use sequential::SequentialAgent;
