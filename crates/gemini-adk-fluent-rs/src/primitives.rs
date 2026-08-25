//! # The L2 contract — authoring
//!
//! This module is the *engineered surface* of the authoring layer: the closed
//! set of primitives L2 exposes to applications, curated and named. L0 moves
//! frames; L1 gives them meaning and enforces it; this layer makes a whole
//! application sayable — in one fluent expression, or in one JSON document.
//!
//! **L2 promises:** two equivalent ways to state a session. In code: the
//! [`Live`] builder and the [`AgentBuilder`] combinators, composed through
//! eight one-letter algebras — [`S`]tate `>>`, [`C`]ontext `+`, [`T`]ools `|`,
//! [`P`]rompt `+`, [`M`]iddleware `|`, [`A`]rtifacts `+`, [`E`]valuation `|`,
//! [`G`]uards `|`. As data: [`SessionSpec`], the same session as one
//! serializable document, with load-time validation and offline tests. What
//! serializes, runs; what can fail, fails before connect
//! ([`check_contracts`], [`SessionSpec::validate`]).
//!
//! **L2 never:** invents runtime semantics. Every builder method lowers to an
//! L1 primitive; every spec field lowers to a builder method. This layer adds
//! *phrasing*, not *behavior* — which is why the JSON document and the fluent
//! chain stay equivalent.
//!
//! The primitives, by concern:
//!
//! | Concern | Primitives |
//! |---|---|
//! | Voice session | [`Live`] (·builder → connect → [`LiveHandle`](gemini_adk_rs::live::LiveHandle)) |
//! | Text agents | [`AgentBuilder`], [`Pipeline`] `>>`, [`FanOut`] `\|`, `*` loops, [`until`], `/` fallback |
//! | The algebra | [`S`], [`C`], [`T`], [`P`], [`M`], [`A`], [`E`], [`G`] |
//! | Session as data | [`SessionSpec`], [`SpecResources`], [`SpecTest`], [`run_tests`] |
//! | Proof before connect | [`check_contracts`], [`ContractViolation`] |
//! | Telephony | [`TwilioCall`] (·attach — a phone call on the same pump as a microphone) |
//! | Ergonomics | [`let_clone!`](crate::let_clone) |
//!
//! Five lines to a governed voice application (add the `voice-io` feature for
//! `voice::Talk::talk`):
//!
//! ```ignore
//! let session = Live::builder()
//!     .instruction("You are a helpful concierge.")
//!     .greeting("Greet the caller.")
//!     .govern(flow)
//!     .connect_from_env().await?;
//! session.talk().await?;      // microphone in, speakers out, barge-in handled
//! ```

pub use crate::builder::AgentBuilder;
pub use crate::live::Live;

pub use crate::operators::{until, FanOut, Pipeline};

pub use crate::compose::artifacts::A;
pub use crate::compose::context::C;
pub use crate::compose::eval::E;
pub use crate::compose::guards::G;
pub use crate::compose::middleware::M;
pub use crate::compose::prompt::P;
pub use crate::compose::state::S;
pub use crate::compose::tools::T;

pub use crate::spec::{run_tests, SessionSpec, SpecResources, SpecTest};

pub use crate::telephony::TwilioCall;
pub use crate::testing::{check_contracts, ContractViolation};

#[cfg(test)]
mod contract {
    //! The drift guard: every primitive the module docs name must exist and
    //! be reachable from this path.
    #[test]
    fn every_named_primitive_is_reachable() {
        use super::*;
        fn is_type<T: ?Sized>() {}
        is_type::<Live>();
        is_type::<TwilioCall>();
        is_type::<AgentBuilder>();
        is_type::<Pipeline>();
        is_type::<FanOut>();
        is_type::<S>();
        is_type::<C>();
        is_type::<T>();
        is_type::<P>();
        is_type::<M>();
        is_type::<A>();
        is_type::<E>();
        is_type::<G>();
        is_type::<SessionSpec>();
        is_type::<SpecResources>();
        is_type::<SpecTest>();
        is_type::<ContractViolation>();
        let _ = until(|_| true);
        let _ = run_tests;
        let _ = check_contracts;
    }
}
