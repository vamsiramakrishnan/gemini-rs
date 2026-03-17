//! Composition modules -- S, C, P, M, T, A, E, G.
//!
//! Eight namespaces for composing different aspects of agent configuration:
//!
//! | Module | Namespace | Operator | Purpose                        |
//! |--------|-----------|----------|--------------------------------|
//! | S      | `S::`     | `>>`     | State transforms               |
//! | C      | `C::`     | `+`      | Context engineering             |
//! | P      | `P::`     | `+`      | Prompt composition              |
//! | M      | `M::`     | `\|`     | Middleware composition           |
//! | T      | `T::`     | `\|`     | Tool composition                |
//! | A      | `A::`     | `+`      | Artifact schemas                |
//! | E      | `E::`     | `\|`     | Evaluation criteria             |
//! | G      | `G::`     | `\|`     | Guard composition               |
//!
//! # Quick Reference
//!
//! ```rust
//! use adk_rs_fluent::compose::{S, C, P, T, A, E, G};
//! use serde_json::json;
//! use rs_genai::prelude::Content;
//!
//! // S: State transforms — pick, rename, chain with >>
//! let transform = S::pick(&["a", "b"]) >> S::rename(&[("a", "x")]);
//!
//! // C: Context policies — window, filter, chain with +
//! let context = C::window(10) + C::user_only();
//!
//! // P: Prompt sections — role, task, format, chain with +
//! let prompt = P::role("analyst") + P::task("analyze data") + P::format("JSON");
//!
//! // T: Tool composition — built-ins and custom, chain with |
//! let tools = T::google_search() | T::code_execution();
//!
//! // A: Artifact schemas — inputs and outputs, chain with +
//! let artifacts = A::json_output("report", "Analysis report")
//!     + A::text_input("source", "Source document");
//!
//! // E: Evaluation — criteria composition with |
//! let eval = E::response_match() | E::safety();
//!
//! // G: Guards — output validation with |
//! let guards = G::length(1, 1000) | G::json();
//! ```

pub mod artifacts;
pub mod context;
pub mod ctx;
pub mod eval;
pub mod guards;
#[doc(hidden)]
pub mod middleware;
pub mod prompt;
pub mod state;
pub mod tools;

pub use artifacts::A;
pub use context::C;
pub use ctx::Ctx;
pub use eval::E;
pub use guards::G;
pub use middleware::M;
pub use prompt::P;
pub use state::S;
pub use tools::T;
