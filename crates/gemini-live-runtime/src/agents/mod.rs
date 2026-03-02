//! Agent composition primitives — Sequential, Parallel, Loop.
//!
//! These implement the Agent trait and compose sub-agents in different patterns.
//! They work at the InvocationContext level, passing the context to sub-agents.

pub mod loop_agent;
pub mod parallel;
pub mod sequential;

pub use loop_agent::LoopAgent;
pub use parallel::ParallelAgent;
pub use sequential::SequentialAgent;
