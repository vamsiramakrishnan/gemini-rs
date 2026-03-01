//! # gemini-live-wire
//!
//! Raw wire protocol and transport for the Gemini Multimodal Live API.
//! This crate provides zero-abstraction access to the Gemini Live WebSocket API.
//!
//! ## Layers
//!
//! - **Protocol**: Wire-format types mapping 1:1 to the API (`protocol/`)
//! - **Transport**: WebSocket connection with reconnection and flow control (`transport/`)
//! - **Session**: Session handle with command/event channels and phase FSM (`session/`)
//! - **Buffer**: Lock-free SPSC ring buffer and adaptive jitter buffer (`buffer/`)
//! - **VAD**: Voice activity detection with adaptive noise floor (`vad/`)
//! - **Flow**: Barge-in detection and turn detection (`flow/`)
//! - **Telemetry**: OTel spans, structured logging, Prometheus metrics (`telemetry/`)

pub mod protocol;
pub mod transport;
pub mod buffer;
#[cfg(feature = "vad")]
pub mod vad;
pub mod session;
pub mod flow;
pub mod telemetry;

/// Convenient re-exports for wire-level usage.
pub mod prelude {
    // Protocol types
    pub use crate::protocol::types::*;
    pub use crate::protocol::messages::*;

    // Transport
    pub use crate::transport::{connect, connect_with, Codec, CodecError, JsonCodec, TransportConfig};
    pub use crate::transport::ws::{Transport, TungsteniteTransport, MockTransport};

    // Session
    pub use crate::session::{
        AuthError, SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase,
        SessionReader, SessionWriter, SetupError, WebSocketError,
    };

    // Buffers
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};

    // VAD
    #[cfg(feature = "vad")]
    pub use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};

    // Flow
    pub use crate::flow::{
        BargeInAction, BargeInConfig, BargeInDetector,
        TurnDetectionConfig, TurnDetectionEvent, TurnDetector,
    };

    // Telemetry
    pub use crate::telemetry::TelemetryConfig;
}
