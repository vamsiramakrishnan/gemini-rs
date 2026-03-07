//! Reconnection delay and disconnect reason types.

use std::time::Duration;

use crate::transport::TransportConfig;

/// Reason for session disconnect.
pub(super) enum DisconnectReason {
    Graceful,
    GoAway(Option<String>),
    Error(String),
    CommandChannelClosed,
}

/// Calculate reconnection delay with exponential backoff and jitter.
pub(super) fn reconnect_delay(attempt: u32, config: &TransportConfig) -> Duration {
    let base_ms = config.reconnect_base_delay_ms as u64;
    let max_ms = config.reconnect_max_delay_ms as u64;
    let delay_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1))).min(max_ms);
    // Add ~25% jitter
    let jitter = delay_ms / 4;
    Duration::from_millis(delay_ms + jitter)
}
