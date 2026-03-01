//! # gemini-live-rs
//!
//! Ultra-low-latency Rust library for the Gemini Multimodal Live API.
//!
//! `gemini-live-rs` treats the Gemini Live API as a first-class, unified
//! speech-to-speech system — eliminating cascaded STT→LLM→TTS pipelines
//! and operating at the wire level where milliseconds are reclaimed.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use gemini_live_rs::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = SessionConfig::new("YOUR_API_KEY")
//!         .model(GeminiModel::Gemini2_0FlashLive)
//!         .voice(Voice::Kore)
//!         .system_instruction("You are a helpful voice assistant.");
//!
//!     let session = connect(config, TransportConfig::default()).await?;
//!     session.wait_for_phase(SessionPhase::Active).await;
//!
//!     session.send_text("Hello!").await?;
//!
//!     let mut events = session.subscribe();
//!     while let Ok(event) = events.recv().await {
//!         match event {
//!             SessionEvent::TextDelta(text) => print!("{}", text),
//!             SessionEvent::TurnComplete => println!("\n--- Turn complete ---"),
//!             SessionEvent::Disconnected(_) => break,
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```

pub mod protocol;
pub mod transport;
pub mod buffer;
#[cfg(feature = "vad")]
pub mod vad;
pub mod session;
pub mod agent;
pub mod flow;
pub mod telemetry;
pub mod app;
pub mod pipeline;
pub mod call;
pub mod client;
pub mod context;
pub mod prompt;
pub mod state;

/// Convenient re-exports for common usage.
pub mod prelude {
    pub use crate::protocol::{
        ApiEndpoint, AudioFormat, Blob, Content, FunctionCall, FunctionDeclaration,
        FunctionResponse, GeminiModel, Modality, Part, SessionConfig, ToolConfig,
        ToolDeclaration, VertexConfig, Voice,
    };
    pub use crate::transport::{connect, TransportConfig};
    pub use crate::session::{
        SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase,
    };
    pub use crate::agent::FunctionRegistry;
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};
    pub use crate::telemetry::TelemetryConfig;

    #[cfg(feature = "vad")]
    pub use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};

    // Sugar layer
    pub use crate::app::{GeminiAgent, GeminiAgentBuilder, PipelineConfig};
    pub use crate::pipeline::{AudioPipeline, AudioSink, AudioSource};
    pub use crate::call::{CallMetrics, CallPhase, CallSession};
    pub use crate::client::{AudioNegotiation, ClientEvent, ServerEvent};

    // Engineering layers
    pub use crate::context::{
        ContextBudget, ContextInjection, ContextManager, ContextPolicy, ContextSnapshot,
        InjectionTrigger, MemoryStrategy, TurnSummary,
    };
    pub use crate::prompt::{
        PromptStrategy, SectionKind, SystemPrompt, SystemPromptBuilder,
    };
    pub use crate::state::{
        ConversationState, EventTrigger, GuardPoint, StateGuard, StateManager, StatePolicy,
        StateScope, StateTransform,
    };
}
