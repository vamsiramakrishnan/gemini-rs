//! Agent optimization framework — iteratively improve agent prompts.
//!
//! Mirrors ADK-Python's `optimization` module. Provides traits for
//! sampling evaluation examples and optimizing agent instructions
//! using LLM-driven prompt rewriting.

mod optimizer;
mod sampler;
mod simple_prompt;

pub use optimizer::{AgentOptimizer, OptimizerError, OptimizerResult};
pub use sampler::{EvalSample, Sampler};
pub use simple_prompt::{SimplePromptOptimizer, SimplePromptOptimizerConfig};
