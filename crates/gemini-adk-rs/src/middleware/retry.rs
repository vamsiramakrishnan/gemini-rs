//! Advisory retry middleware for agent error tracking.

use async_trait::async_trait;

use super::Middleware;
use crate::error::AgentError;

/// Advisory middleware that tracks errors and provides retry guidance.
///
/// The `Middleware` trait hooks are lifecycle callbacks, not control-flow points.
/// `RetryMiddleware` counts errors via [`Middleware::on_error`] and exposes a
/// [`RetryMiddleware::should_retry`] method the caller can query to decide
/// whether to re-invoke the agent.
///
/// # Examples
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use gemini_adk_rs::middleware::RetryMiddleware;
///
/// let retry = Arc::new(RetryMiddleware::new(3));
/// // ... run agent, on_error is called by the middleware chain ...
/// while retry.should_retry() {
///     retry.record_attempt();
///     // re-run the agent
/// }
/// ```
pub struct RetryMiddleware {
    max_retries: u32,
    pub(crate) error_count: std::sync::atomic::AtomicU32,
    pub(crate) attempt: std::sync::atomic::AtomicU32,
}

impl RetryMiddleware {
    /// Create a new retry middleware with the given maximum retry count.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            error_count: std::sync::atomic::AtomicU32::new(0),
            attempt: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Returns `true` if the number of attempts is below `max_retries`
    /// and at least one error has been recorded since the last reset.
    pub fn should_retry(&self) -> bool {
        let attempts = self.attempt.load(std::sync::atomic::Ordering::SeqCst);
        let errors = self.error_count.load(std::sync::atomic::Ordering::SeqCst);
        errors > 0 && attempts < self.max_retries
    }

    /// Record that a retry attempt is being made.
    /// Call this before re-invoking the agent.
    pub fn record_attempt(&self) {
        self.attempt
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Reset the error flag so we wait for a new error before retrying again.
        self.error_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns the current attempt count (0-based).
    pub fn attempts(&self) -> u32 {
        self.attempt.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the configured maximum number of retries.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Reset all counters, allowing the middleware to be reused.
    pub fn reset(&self) {
        self.error_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.attempt.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl Middleware for RetryMiddleware {
    fn name(&self) -> &str {
        "retry"
    }

    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> {
        self.error_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
