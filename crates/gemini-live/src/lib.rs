#![warn(missing_docs)]
//! # gemini-live
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

#[cfg(feature = "batches")]
pub mod batches;
pub mod buffer;
#[cfg(feature = "caches")]
pub mod caches;
#[cfg(feature = "chats")]
pub mod chats;
pub mod client;
#[cfg(feature = "embed")]
pub mod embed;
#[cfg(feature = "files")]
pub mod files;
pub mod flow;
#[cfg(feature = "generate")]
pub mod generate;
#[cfg(feature = "models")]
pub mod models;
pub mod protocol;
pub mod quick;
pub mod session;
pub mod telemetry;
#[cfg(feature = "tokens")]
pub mod tokens;
pub mod transport;
#[cfg(feature = "tunings")]
pub mod tunings;
#[cfg(feature = "vad")]
pub mod vad;

// Top-level re-exports for convenience.
pub use client::Client;
pub use quick::{quick_connect, quick_connect_vertex};

/// Convenient re-exports for wire-level usage.
pub mod prelude {
    // Protocol types
    pub use crate::protocol::messages::*;
    pub use crate::protocol::types::*;
    pub use crate::protocol::Platform;

    // Transport
    pub use crate::transport::auth::{
        AuthProvider, GoogleAIAuth, GoogleAITokenAuth, ServiceEndpoint, VertexAIAuth,
    };
    pub use crate::transport::ws::{MockTransport, Transport, TungsteniteTransport};
    pub use crate::transport::{
        connect, connect_with, Codec, CodecError, ConnectBuilder, JsonCodec, TransportConfig,
    };

    // Session
    pub use crate::session::{
        recv_event, AuthError, ResumeInfo, SessionCommand, SessionError, SessionEvent,
        SessionHandle, SessionPhase, SessionReader, SessionWriter, SetupError, WebSocketError,
    };

    // Buffers
    pub use crate::buffer::{bytes_to_i16, i16_to_bytes, into_shared};
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};

    // VAD
    #[cfg(feature = "vad")]
    pub use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};

    // Flow
    pub use crate::flow::{
        BargeInAction, BargeInConfig, BargeInDetector, TurnDetectionConfig, TurnDetectionEvent,
        TurnDetector,
    };

    // Telemetry
    pub use crate::telemetry::TelemetryConfig;

    // Safety types (shared across all APIs)
    pub use crate::protocol::types::{
        CitationMetadata, CitationSource, FileData, FinishReason, HarmBlockThreshold, HarmCategory,
        HarmProbability, SafetyRating, SafetySetting,
    };

    // Client
    #[cfg(feature = "http")]
    pub use crate::client::http::{HttpClient, HttpConfig, HttpError};
    pub use crate::client::Client;

    // Generate API
    #[cfg(feature = "generate")]
    pub use crate::generate::{
        Candidate, GenerateContentConfig, GenerateContentResponse, GenerateError,
    };

    // Tokens API
    #[cfg(feature = "tokens")]
    pub use crate::tokens::{CountTokensResponse, TokensError};

    // Models API
    #[cfg(feature = "models")]
    pub use crate::models::{ListModelsResponse, ModelInfo, ModelsError};

    // Embed API
    #[cfg(feature = "embed")]
    pub use crate::embed::{
        ContentEmbedding, EmbedContentConfig, EmbedContentResponse, EmbedError, TaskType,
    };

    // Files API
    #[cfg(feature = "files")]
    pub use crate::files::{
        File, FileSource, FileState, FilesError, ListFilesResponse, UploadFileConfig,
    };

    // Caches API
    #[cfg(feature = "caches")]
    pub use crate::caches::{
        CachedContent, CachedContentUsageMetadata, CachesError, CreateCachedContentConfig,
        ListCachedContentsResponse, UpdateCachedContentRequest,
    };

    // Tunings API
    #[cfg(feature = "tunings")]
    pub use crate::tunings::{
        CreateTuningJobConfig, ListTuningJobsResponse, SupervisedTuningSpec, TuningHyperParameters,
        TuningJob, TuningJobState, TuningsError,
    };

    // Batches API
    #[cfg(feature = "batches")]
    pub use crate::batches::{
        BatchJob, BatchJobDestination, BatchJobSource, BatchJobState, BatchesError,
        CreateBatchJobConfig, ListBatchJobsResponse,
    };

    // Chat API
    #[cfg(feature = "chats")]
    pub use crate::chats::ChatSession;

    // Quick-start
    pub use crate::quick::{quick_connect, quick_connect_vertex};
}
