//! Quick-start convenience functions for connecting to the Gemini Multimodal Live API.
//!
//! These are thin wrappers over [`SessionConfig`] + [`connect()`] that provide
//! sensible defaults for the common case. For advanced configuration (custom
//! transport, codec, modalities, etc.), use [`SessionConfig`] directly.
//!
//! # Google AI (API key)
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), gemini_live_wire::session::SessionError> {
//! use gemini_live_wire::prelude::*;
//!
//! let session = gemini_live_wire::quick_connect("API_KEY", "gemini-2.0-flash-live-001").await?;
//! session.send_text("What is the speed of light?").await?;
//! let mut events = session.subscribe();
//! while let Ok(event) = events.recv().await {
//!     if let SessionEvent::TextDelta(ref text) = event { print!("{text}"); }
//!     if let SessionEvent::TurnComplete = event { break; }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Vertex AI
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), gemini_live_wire::session::SessionError> {
//! use gemini_live_wire::prelude::*;
//!
//! let session = gemini_live_wire::quick_connect_vertex(
//!     "ya29.ACCESS_TOKEN",
//!     "my-project",
//!     "us-central1",
//!     "gemini-2.0-flash-live-001",
//! ).await?;
//! # Ok(())
//! # }
//! ```

use crate::protocol::types::{GeminiModel, SessionConfig};
use crate::session::{SessionError, SessionHandle};
use crate::transport::{connect, TransportConfig};

/// Connect to Gemini Live with minimal configuration.
///
/// Uses sensible defaults: [`TransportConfig::default()`], audio output modality.
/// For advanced configuration, use [`SessionConfig`] + [`connect()`] directly.
pub async fn quick_connect(
    api_key: &str,
    model: &str,
) -> Result<SessionHandle, SessionError> {
    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Custom(model.to_string()));
    connect(config, TransportConfig::default()).await
}

/// Connect via Vertex AI with minimal configuration.
///
/// Uses sensible defaults: [`TransportConfig::default()`], audio output modality.
/// For advanced configuration, use [`SessionConfig::from_vertex()`] + [`connect()`] directly.
pub async fn quick_connect_vertex(
    access_token: &str,
    project: &str,
    location: &str,
    model: &str,
) -> Result<SessionHandle, SessionError> {
    let config = SessionConfig::from_vertex(project, location, access_token)
        .model(GeminiModel::Custom(model.to_string()));
    connect(config, TransportConfig::default()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ws::MockTransport;
    use crate::transport::codec::JsonCodec;
    use crate::transport::connect_with;
    use crate::session::SessionPhase;

    /// Verify that `quick_connect` builds a valid SessionConfig internally.
    ///
    /// We can't call `quick_connect` directly because it opens a real WebSocket.
    /// Instead we replicate its config construction and verify it works with a mock.
    #[tokio::test]
    async fn quick_connect_creates_valid_config() {
        let config = SessionConfig::new("test-api-key")
            .model(GeminiModel::Custom("gemini-2.0-flash-live-001".to_string()));

        // Verify the config fields
        assert_eq!(config.model, GeminiModel::Custom("gemini-2.0-flash-live-001".to_string()));

        // Verify it connects successfully with a mock transport
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let transport_config = TransportConfig {
            max_reconnect_attempts: 0,
            ..TransportConfig::default()
        };

        let handle = connect_with(config, transport_config, transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;
        assert_eq!(handle.phase(), SessionPhase::Active);
    }

    /// Verify that `quick_connect_vertex` builds a valid Vertex AI config.
    #[tokio::test]
    async fn quick_connect_vertex_creates_valid_config() {
        let config = SessionConfig::from_vertex("my-project", "us-central1", "ya29.TOKEN")
            .model(GeminiModel::Custom("gemini-2.0-flash-live-001".to_string()));

        // Verify model
        assert_eq!(config.model, GeminiModel::Custom("gemini-2.0-flash-live-001".to_string()));

        // Verify Vertex AI endpoint
        assert!(config.is_vertex(), "should target Vertex AI");
        let url = config.ws_url();
        assert!(url.contains("aiplatform.googleapis.com"), "URL should use Vertex AI endpoint");

        // Verify model URI contains project and location
        let model_uri = config.model_uri();
        assert!(model_uri.contains("my-project"), "model URI should contain project ID");
        assert!(model_uri.contains("us-central1"), "model URI should contain location");

        // Verify bearer token is set
        assert_eq!(config.bearer_token(), Some("ya29.TOKEN"));

        // Verify it connects successfully with a mock transport
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let transport_config = TransportConfig {
            max_reconnect_attempts: 0,
            ..TransportConfig::default()
        };

        let handle = connect_with(config, transport_config, transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;
        assert_eq!(handle.phase(), SessionPhase::Active);
    }
}
