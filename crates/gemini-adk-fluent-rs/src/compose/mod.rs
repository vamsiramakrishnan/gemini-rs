//! Composition modules -- S, C, P, M, T, A, E, G.
//!
//! Eight namespaces for composing different aspects of agent configuration.
//! Two operators combine them, and each means one thing everywhere:
//!
//! - **`a >> b` — then.** Order matters: `b` applies after `a`, to what `a`
//!   produced. State transforms, context rewrites, middleware layers, and
//!   agent pipelines.
//! - **`a + b` — together.** Both apply; neither sees the other's result.
//!   Prompt sections, tools, guards (every guard must pass), evaluation
//!   criteria, artifact declarations.
//!
//! Agents alone also take `|` (run in parallel), `*` (loop) and `/`
//! (fallback); see [`operators`](crate::operators).
//!
//! | Module | Namespace | Operator | Purpose                    | Consumed by |
//! |--------|-----------|----------|----------------------------|-------------|
//! | S      | `S::`     | `>>`     | State transforms           | pipeline steps (`agent >> S::pick(..) >> agent`), loop and branch predicates |
//! | C      | `C::`     | `>>`     | Context rewrites           | `AgentBuilder::context` |
//! | M      | `M::`     | `>>`     | Middleware layers          | `.middleware(..)` |
//! | P      | `P::`     | `+`      | Prompt sections            | `.instruction(..)` |
//! | T      | `T::`     | `+`      | Tools                      | `.tools(..)` |
//! | G      | `G::`     | `+`      | Output guards (all must pass) | `AgentBuilder::guard` |
//! | E      | `E::`     | `+`      | Evaluation criteria        | `E::suite(..)` |
//! | A      | `A::`     | `+`      | Artifact declarations      | `AgentBuilder::artifacts`, contract checks |
//!
//! # Quick Reference
//!
//! ```rust
//! use gemini_adk_fluent_rs::compose::{S, C, P, T, A, E, G};
//! use serde_json::json;
//! use gemini_genai_rs::prelude::Content;
//!
//! // S: State transforms — pick, rename, chain with >>
//! let transform = S::pick(&["a", "b"]) >> S::rename(&[("a", "x")]);
//!
//! // C: Context rewrites — applied in order with >>
//! let context = C::window(10) >> C::user_only();
//!
//! // P: Prompt sections — role, task, format, together with +
//! let prompt = P::role("analyst") + P::task("analyze data") + P::format("JSON");
//!
//! // T: Tools — built-ins and custom, together with +
//! let tools = T::google_search() + T::code_execution();
//!
//! // A: Artifact declarations — inputs and outputs, together with +
//! let artifacts = A::json_output("report", "Analysis report")
//!     + A::text_input("source", "Source document");
//!
//! // E: Evaluation — deterministic criteria together with + (LLM-judge
//! // criteria like E::safety(llm) take a judge model).
//! let eval = E::response_match() + E::contains_match();
//!
//! // G: Guards — output validation, all must pass, together with +
//! let guards = G::length(1, 1000) + G::json();
//! ```

pub mod artifacts;
pub mod context;
pub mod ctx;
pub mod eval;
pub mod guards;
pub mod judge;
pub mod middleware;
pub mod prompt;
pub mod state;
pub mod tools;

pub use artifacts::{A, ArtifactOp};
pub use context::C;
pub use ctx::Ctx;
pub use eval::E;
pub use guards::G;
pub use middleware::M;
pub use prompt::P;
pub use state::S;
pub use tools::T;
