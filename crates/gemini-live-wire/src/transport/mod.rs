//! WebSocket transport layer — connection, full-duplex messaging, flow control.

pub mod connection;
pub mod flow;

pub use connection::connect;
pub use flow::{FlowConfig, TokenBucket};

/// Configuration for the transport layer.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Depth of the command send queue.
    pub send_queue_depth: usize,
    /// Capacity of the event broadcast channel.
    pub event_channel_capacity: usize,
    /// WebSocket connect timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Setup handshake timeout in seconds.
    pub setup_timeout_secs: u64,
    /// Maximum reconnection attempts before giving up.
    pub max_reconnect_attempts: u32,
    /// Base delay for exponential backoff (ms).
    pub reconnect_base_delay_ms: u32,
    /// Maximum delay for exponential backoff (ms).
    pub reconnect_max_delay_ms: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            send_queue_depth: 256,
            event_channel_capacity: 512,
            connect_timeout_secs: 10,
            setup_timeout_secs: 15,
            max_reconnect_attempts: 5,
            reconnect_base_delay_ms: 1000,
            reconnect_max_delay_ms: 30_000,
        }
    }
}
