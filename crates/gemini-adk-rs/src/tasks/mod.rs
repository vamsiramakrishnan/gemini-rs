//! Independent capability activations inside one speaking session.
//!
//! [`TaskRuntime`] is a synchronous owner of task state, governance and approval.
//! Live and offline drivers execute the [`OwnedInvocation`] it returns, then
//! return the completion to the same runtime. Workers never select a foreground
//! task or deliver speech. State snapshots and tool factories isolate accepted
//! task state from cancelled or superseded workers. Trusted custom tools must
//! not retain unrelated mutable state outside the supplied context/factory.
//!
//! Snapshots are observations, not durable recovery checkpoints. Commit adapters
//! must honor their idempotency argument and support external reconciliation;
//! cancelling a future cannot establish whether an external effect happened.
mod runtime;
mod services;
mod types;
pub use runtime::{InvocationCompletion, OwnedInvocation, TaskRuntime};
pub use services::{
    AcceptedTaskEffect, OwnedTaskServiceWork, TaskEffect, TaskEffects, TaskMemoryService,
    TaskObservation, TaskPattern, TaskServiceCompletion, TaskServices, TaskWatcher,
};
pub use types::*;
#[cfg(test)]
mod tests;
