//! Ctx — Declarative state-to-narrative context composition.
//!
//! Builds a [`ContextBuilder`] that renders session state into a
//! natural-language summary appended to the phase instruction.
//!
//! # Example
//!
//! ```ignore
//! use gemini_adk_fluent_rs::prelude::*;
//!
//! let ctx = Ctx::builder()
//!     .section("Caller")
//!         .field("caller_name", "Name")
//!         .flag("is_known_contact", "Known contact")
//!     .section("Call")
//!         .field("call_purpose", "Purpose")
//!         .sentiment("caller_sentiment")
//!     .build();
//!
//! // Use with phase_defaults:
//! Live::builder()
//!     .phase_defaults(|d| d.context(ctx))
//! ```

pub use gemini_adk_rs::live::context_builder::{ContextBuilder, SectionBuilder};

/// The `Ctx` namespace — factory methods for declarative context builders.
pub struct Ctx;

impl Ctx {
    /// Start building a new context with the first section.
    ///
    /// Returns a [`SectionBuilder`] — add fields, flags, sentiments,
    /// then call `.build()` to get a [`ContextBuilder`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = Ctx::builder()
    ///     .section("Caller")
    ///         .field("caller_name", "Name")
    ///         .field("caller_organization", "Organization")
    ///         .flag("is_known_contact", "Known contact")
    ///     .section("Call")
    ///         .field("call_purpose", "Purpose")
    ///         .sentiment("caller_sentiment")
    ///     .build();
    /// ```
    pub fn builder() -> SectionBuilder {
        ContextBuilder::new()
    }
}
