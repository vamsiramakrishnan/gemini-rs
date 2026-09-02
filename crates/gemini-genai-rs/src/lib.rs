#![warn(unreachable_pub)]
#![warn(missing_docs)]
// The README is the crate doc, so its code blocks are doctests: the
// quickstart cannot drift from the API without failing `cargo test`.
#![doc = include_str!("../README.md")]

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
#[cfg(feature = "generate")]
pub mod generate;
#[cfg(feature = "models")]
pub mod models;
pub mod primitives;
pub mod protocol;
pub mod session;
pub mod telemetry;
#[cfg(feature = "tokens")]
pub mod tokens;
pub mod transport;
#[cfg(feature = "tunings")]
pub mod tunings;
pub mod turn;
#[cfg(feature = "vad")]
pub mod vad;

// Top-level re-exports for convenience.
pub use client::Client;
pub use transport::{ConnectBuilder, connect};

/// Convenient re-exports for wire-level usage.
pub mod prelude {
    // Protocol types — the API vocabulary an application names. The wire
    // envelopes (`SetupMessage`, `RealtimeInputPayload`, `ServerMessageWrapper`,
    // …) stay public at `protocol::messages` for anyone writing a codec, but
    // they are not something a glob import should hand every caller.
    #[allow(deprecated)]
    pub use crate::protocol::types::GeminiModel;
    pub use crate::protocol::types::{
        AccessToken, ActivityHandling, ApiEndpoint, AudioFormat, AutomaticActivityDetection, Blob,
        CodeExecutionResult, Content, ContextWindowCompressionConfig, EndpointEnvError,
        ExecutableCode, FunctionCall, FunctionCallingBehavior, FunctionCallingConfig,
        FunctionCallingMode, FunctionDeclaration, FunctionResponse, FunctionResponseScheduling,
        GenerationConfig, GoogleSearch, GoogleSearchRetrieval, GroundingMetadata,
        InputAudioTranscription, LIVE_INPUT_SAMPLE_RATE, MediaResolution, Modality,
        ModalityTokenCount, ModelId, OutputAudioTranscription, Part, PrebuiltVoiceConfig,
        ProactivityConfig, RealtimeInputConfig, Role, Sensitivity, SessionConfig,
        SessionResumptionConfig, SlidingWindow, SpeechConfig, ThinkingConfig, Tool,
        ToolCodeExecution, ToolConfig, ToolProvider, TurnCoverage, UrlContext, UrlContextMetadata,
        UsageMetadata, VertexConfig, Voice, VoiceConfig,
    };
    // The decoded server message is the one envelope applications do match on.
    pub use crate::protocol::messages::ServerMessage;

    // Transport
    pub use crate::transport::auth::{
        AuthProvider, GoogleAIAuth, GoogleAITokenAuth, ServiceEndpoint, VertexAIAuth,
    };
    // Wire recording and replay are debugging/test tooling; they live at
    // `transport::recording` and `transport::replay` rather than in the prelude.
    pub use crate::transport::ws::{
        MockTransport, Transport, TungsteniteError, TungsteniteTransport,
    };
    pub use crate::transport::{
        Codec, CodecError, ConnectBuilder, JsonCodec, TransportConfig, connect,
    };

    // Session
    pub use crate::session::{
        AuthError, ResumeInfo, SessionCommand, SessionError, SessionEvent, SessionHandle,
        SessionPhase, SessionReader, SessionWriter, SetupError, WebSocketError, recv_event,
    };

    // Buffers
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};
    pub use crate::buffer::{bytes_to_i16, i16_to_bytes, into_shared};

    // VAD
    #[cfg(feature = "vad")]
    pub use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};

    // Flow
    pub use crate::turn::{
        BargeInAction, BargeInConfig, BargeInDetector, TurnDetectionConfig, TurnDetectionEvent,
        TurnDetector,
    };

    // `telemetry::TelemetryConfig` installs a process-global subscriber; that
    // is an application's once-per-binary decision, not prelude material.

    // Safety types (shared across all APIs)
    pub use crate::protocol::types::{
        CitationMetadata, CitationSource, FileData, FinishReason, HarmBlockThreshold, HarmCategory,
        HarmProbability, SafetyRating, SafetySetting,
    };

    // `Client` (the REST client) is at the crate root; a name that generic does
    // not belong in a glob.
    #[cfg(feature = "http")]
    pub use crate::client::http::{HttpClient, HttpConfig, HttpError};

    // Generate API
    #[cfg(feature = "generate")]
    pub use crate::generate::{GenerateContentConfig, GenerateContentResponse, GenerateError};

    // Tokens API
    #[cfg(feature = "tokens")]
    pub use crate::tokens::{CountTokensResponse, TokensError};

    // Models API
    #[cfg(feature = "models")]
    pub use crate::models::{ListModelsResponse, ModelsError};

    // Embed API
    #[cfg(feature = "embed")]
    pub use crate::embed::{
        ContentEmbedding, EmbedContentConfig, EmbedContentResponse, EmbedError,
    };

    // The Files API is at `crate::files`; `File` next to `std::fs::File` in a
    // glob import is an ambiguity error waiting to happen.

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
}
