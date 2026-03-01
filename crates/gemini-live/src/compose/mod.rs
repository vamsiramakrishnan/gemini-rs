//! Composition modules — S, C, P, M, T.
//!
//! Five namespaces for composing different aspects of agent configuration:
//!
//! | Module | Namespace | Operator | Purpose                        |
//! |--------|-----------|----------|--------------------------------|
//! | S      | `S::`     | `>>`     | State transforms               |
//! | C      | `C::`     | `+`      | Context engineering             |
//! | P      | `P::`     | `+`      | Prompt composition              |
//! | M      | `M::`     | `\|`     | Middleware composition           |
//! | T      | `T::`     | `\|`     | Tool composition                |

pub mod context;
pub mod middleware;
pub mod prompt;
pub mod state;
pub mod tools;

pub use context::C;
pub use middleware::M;
pub use prompt::P;
pub use state::S;
pub use tools::T;
