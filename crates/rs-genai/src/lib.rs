//! # rs-genai
//!
//! Full Rust equivalent of Google's `@google/genai` SDK.
//! Wire protocol, transport, types, auth, plus REST API modules (feature-gated).
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
pub mod quick;

// Top-level re-exports for convenience.
pub use quick::{quick_connect, quick_connect_vertex};

/// Convenient re-exports for wire-level usage.
pub mod prelude {
    // Protocol types
    pub use crate::protocol::types::*;
    pub use crate::protocol::messages::*;
    pub use crate::protocol::Platform;

    // Transport
    pub use crate::transport::{connect, connect_with, Codec, CodecError, ConnectBuilder, JsonCodec, TransportConfig};
    pub use crate::transport::auth::{AuthProvider, GoogleAIAuth, GoogleAITokenAuth, VertexAIAuth};
    pub use crate::transport::ws::{Transport, TungsteniteTransport, MockTransport};

    // Session
    pub use crate::session::{
        recv_event, AuthError, SessionCommand, SessionError, SessionEvent, SessionHandle,
        SessionPhase, SessionReader, SessionWriter, SetupError, WebSocketError,
    };

    // Buffers
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};
    pub use crate::buffer::{bytes_to_i16, i16_to_bytes, into_shared};

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

    // Quick-start
    pub use crate::quick::{quick_connect, quick_connect_vertex};
}
