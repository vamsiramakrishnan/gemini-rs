//! ConnectBuilder — ergonomic builder for advanced transport/codec configuration.

use crate::protocol::types::SessionConfig;
use crate::session::{SessionError, SessionHandle};
use crate::transport::codec::{Codec, JsonCodec};
use crate::transport::connection::connect_with;
use crate::transport::ws::{Transport, TungsteniteTransport};
use crate::transport::TransportConfig;

/// Builder for advanced connection configuration.
///
/// Allows customizing the transport and codec used for the connection.
/// Defaults to TungsteniteTransport + JsonCodec.
///
/// # Example
/// ```rust,no_run
/// use rs_genai::prelude::*;
///
/// # async fn example() {
/// let config = SessionConfig::new("key");
/// let handle = ConnectBuilder::new(config)
///     .transport_config(TransportConfig { connect_timeout_secs: 30, ..Default::default() })
///     .build()
///     .await
///     .unwrap();
/// # }
/// ```
pub struct ConnectBuilder<T = TungsteniteTransport, C = JsonCodec> {
    config: SessionConfig,
    transport_config: TransportConfig,
    transport: T,
    codec: C,
}

impl ConnectBuilder {
    /// Create a new builder with default transport and codec.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            transport_config: TransportConfig::default(),
            transport: TungsteniteTransport::new(),
            codec: JsonCodec,
        }
    }
}

impl<T: Transport, C: Codec> ConnectBuilder<T, C> {
    /// Set the transport configuration.
    pub fn transport_config(mut self, tc: TransportConfig) -> Self {
        self.transport_config = tc;
        self
    }

    /// Use a custom transport implementation.
    pub fn transport<T2: Transport>(self, transport: T2) -> ConnectBuilder<T2, C> {
        ConnectBuilder {
            config: self.config,
            transport_config: self.transport_config,
            transport,
            codec: self.codec,
        }
    }

    /// Use a custom codec implementation.
    pub fn codec<C2: Codec>(self, codec: C2) -> ConnectBuilder<T, C2> {
        ConnectBuilder {
            config: self.config,
            transport_config: self.transport_config,
            transport: self.transport,
            codec,
        }
    }

    /// Build the connection and return a SessionHandle.
    pub async fn build(self) -> Result<SessionHandle, SessionError> {
        connect_with(self.config, self.transport_config, self.transport, self.codec).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::*;
    use crate::transport::ws::MockTransport;

    #[test]
    fn builder_compiles_with_defaults() {
        let config = SessionConfig::new("key").model(GeminiModel::Gemini2_0FlashLive);
        let _builder = ConnectBuilder::new(config);
    }

    #[test]
    fn builder_with_custom_transport_config() {
        let config = SessionConfig::new("key");
        let _builder = ConnectBuilder::new(config).transport_config(TransportConfig {
            connect_timeout_secs: 30,
            ..Default::default()
        });
    }

    #[test]
    fn builder_with_mock_transport() {
        let config = SessionConfig::new("key");
        let mock = MockTransport::new();
        let _builder = ConnectBuilder::new(config).transport(mock);
    }

    #[test]
    fn builder_with_custom_codec() {
        let config = SessionConfig::new("key");
        let _builder = ConnectBuilder::new(config).codec(JsonCodec);
    }

    #[tokio::test]
    async fn builder_with_mock_builds() {
        let mut mock = MockTransport::new();
        mock.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let config = SessionConfig::new("key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = ConnectBuilder::new(config)
            .transport(mock)
            .build()
            .await
            .unwrap();

        handle
            .wait_for_phase(crate::session::SessionPhase::Active)
            .await;
        assert_eq!(handle.phase(), crate::session::SessionPhase::Active);
    }
}
