//! Error taxonomy for the memory engine.
//!
//! Memory failure is never fatal to a voice session: every error type here is
//! something a caller can degrade past (empty context, deferred commit) rather
//! than something that terminates the conversation.

use thiserror::Error;

use super::ids::MemoryId;

/// Anything that can go wrong inside the memory engine.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// A malformed OKF document.
    #[error("malformed OKF record at {path}: {message}")]
    MalformedRecord {
        /// Where the record came from.
        path: String,
        /// What was wrong with it.
        message: String,
    },

    /// A record was expected but not found.
    #[error("memory {0} not found")]
    NotFound(MemoryId),

    /// The caller's revision no longer matches the repository's.
    #[error("write conflict: expected revision {expected}, found {actual}")]
    RevisionConflict {
        /// The revision the caller read.
        expected: u64,
        /// The revision the repository is actually at.
        actual: u64,
    },

    /// A structured-output extraction failed or returned an unusable shape.
    #[error("extraction failed: {0}")]
    Extraction(String),

    /// A retrieval backend failed or timed out.
    #[error("retrieval failed: {0}")]
    Retrieval(String),

    /// Consolidation or reconciliation failed and should be retried.
    #[error("reconciliation failed: {0}")]
    Reconciliation(String),

    /// A durable event could not be appended.
    #[error("event log unavailable: {0}")]
    EventLog(String),

    /// The operation was refused for policy reasons.
    #[error("refused by policy: {0}")]
    PolicyRefused(String),

    /// An underlying storage or filesystem failure.
    #[error("storage error: {0}")]
    Storage(String),

    /// A deadline elapsed before the operation completed.
    #[error("{operation} exceeded its {budget_ms}ms budget")]
    DeadlineExceeded {
        /// What was being attempted.
        operation: &'static str,
        /// The budget that elapsed.
        budget_ms: u64,
    },
}

impl MemoryError {
    /// Whether retrying the same operation could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Extraction(_)
                | Self::Retrieval(_)
                | Self::Reconciliation(_)
                | Self::EventLog(_)
                | Self::Storage(_)
                | Self::DeadlineExceeded { .. }
                | Self::RevisionConflict { .. }
        )
    }

    /// Whether the voice path should silently degrade rather than surface this.
    ///
    /// Everything except an explicit policy refusal degrades: the user asked a
    /// question, and an internal memory failure is not their problem.
    pub fn should_degrade_silently(&self) -> bool {
        !matches!(self, Self::PolicyRefused(_))
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(value: std::io::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_retryable_but_policy_refusals_are_not() {
        assert!(MemoryError::Retrieval("timeout".into()).is_retryable());
        assert!(!MemoryError::PolicyRefused("sensitive".into()).is_retryable());
    }

    #[test]
    fn only_policy_refusals_surface_to_the_caller() {
        assert!(MemoryError::EventLog("down".into()).should_degrade_silently());
        assert!(!MemoryError::PolicyRefused("restricted".into()).should_degrade_silently());
    }
}
