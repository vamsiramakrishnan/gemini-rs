//! Transport abstraction — bidirectional message transport.
//!
//! The [`Transport`] trait defines a pluggable transport layer for sending and
//! receiving raw bytes. The default implementation [`TungsteniteTransport`] wraps
//! `tokio-tungstenite` for WebSocket connectivity. [`MockTransport`] enables
//! deterministic unit testing without a network.

use async_trait::async_trait;

/// A bidirectional message transport.
///
/// The default is WebSocket ([`TungsteniteTransport`]); [`MockTransport`] enables
/// unit testing without a real server.
#[async_trait]
pub trait Transport: Send + 'static {
    /// The error type produced by this transport.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Connect to the given URL with optional headers.
    async fn connect(
        &mut self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<(), Self::Error>;

    /// Send raw bytes.
    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error>;

    /// Receive raw bytes. Returns `None` when the connection is closed.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Close the transport.
    async fn close(&mut self) -> Result<(), Self::Error>;
}

// ---------------------------------------------------------------------------
// TungsteniteTransport — WebSocket transport using tokio-tungstenite
// ---------------------------------------------------------------------------

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// WebSocket transport using `tokio-tungstenite`.
pub struct TungsteniteTransport {
    ws_write: Option<futures_util::stream::SplitSink<WsStream, Message>>,
    ws_read: Option<futures_util::stream::SplitStream<WsStream>>,
}

impl TungsteniteTransport {
    /// Create a new, disconnected transport.
    pub fn new() -> Self {
        Self {
            ws_write: None,
            ws_read: None,
        }
    }
}

impl Default for TungsteniteTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from the [`TungsteniteTransport`].
#[derive(Debug, thiserror::Error)]
pub enum TungsteniteError {
    /// The transport is not connected.
    #[error("Not connected")]
    NotConnected,

    /// WebSocket protocol error from tungstenite.
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// Failed to construct the HTTP request (e.g. bad URL or header).
    #[error("Request error: {0}")]
    Request(String),
}

#[async_trait]
impl Transport for TungsteniteTransport {
    type Error = TungsteniteError;

    async fn connect(
        &mut self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<(), Self::Error> {
        let mut request = url
            .into_client_request()
            .map_err(|e| TungsteniteError::Request(e.to_string()))?;

        for (name, value) in headers {
            let header_name: tokio_tungstenite::tungstenite::http::HeaderName =
                name.parse().map_err(
                    |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderName| {
                        TungsteniteError::Request(format!("invalid header name: {e}"))
                    },
                )?;
            let header_value: tokio_tungstenite::tungstenite::http::HeaderValue =
                value.parse().map_err(
                    |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                        TungsteniteError::Request(format!("invalid header value: {e}"))
                    },
                )?;
            request.headers_mut().insert(header_name, header_value);
        }

        let (ws_stream, _response) = tokio_tungstenite::connect_async(request).await?;
        let (ws_write, ws_read) = ws_stream.split();
        self.ws_write = Some(ws_write);
        self.ws_read = Some(ws_read);
        Ok(())
    }

    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
        let ws_write = self
            .ws_write
            .as_mut()
            .ok_or(TungsteniteError::NotConnected)?;
        // Convert bytes to a UTF-8 text frame. The wire protocol sends JSON as text.
        let text = String::from_utf8(data)
            .map_err(|e| TungsteniteError::Request(format!("invalid UTF-8: {e}")))?;
        ws_write.send(Message::Text(text)).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        let ws_read = self
            .ws_read
            .as_mut()
            .ok_or(TungsteniteError::NotConnected)?;
        loop {
            match ws_read.next().await {
                Some(Ok(Message::Text(t))) => return Ok(Some(t.into_bytes())),
                // IMPORTANT: Vertex AI sends JSON in Binary frames.
                Some(Ok(Message::Binary(b))) => return Ok(Some(b)),
                Some(Ok(Message::Close(_))) => return Ok(None),
                // Ping/Pong are handled internally by tungstenite; skip them.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                // Frame is a low-level variant; skip.
                Some(Ok(Message::Frame(_))) => continue,
                Some(Err(e)) => return Err(TungsteniteError::WebSocket(e)),
                None => return Ok(None),
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(ref mut ws_write) = self.ws_write {
            ws_write.send(Message::Close(None)).await?;
        }
        self.ws_write = None;
        self.ws_read = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockTransport — for unit testing
// ---------------------------------------------------------------------------

/// Mock transport for unit testing.
///
/// Records sent data and replays scripted responses from a queue. When the
/// queue is empty and the transport is connected, [`recv`](Transport::recv)
/// will pend indefinitely — simulating a connected-but-idle transport.
/// Call [`close`](Transport::close) to signal connection closure (returns `None`).
pub struct MockTransport {
    sent: Vec<Vec<u8>>,
    recv_queue: std::collections::VecDeque<Vec<u8>>,
    /// Whether connect() has been called (and close() has not).
    connected: bool,
}

impl MockTransport {
    /// Create a new, disconnected mock transport.
    pub fn new() -> Self {
        Self {
            sent: Vec::new(),
            recv_queue: std::collections::VecDeque::new(),
            connected: false,
        }
    }

    /// Queue a message to be returned by [`Transport::recv`].
    pub fn script_recv(&mut self, data: Vec<u8>) {
        self.recv_queue.push_back(data);
    }

    /// Take all sent data (for assertions). Drains the internal buffer.
    pub fn take_sent(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.sent)
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from the [`MockTransport`].
#[derive(Debug, thiserror::Error)]
pub enum MockTransportError {
    /// Operation attempted while not connected.
    #[error("Not connected")]
    NotConnected,

    /// A custom error injected for testing.
    #[error("Mock error: {0}")]
    Custom(String),
}

#[async_trait]
impl Transport for MockTransport {
    type Error = MockTransportError;

    async fn connect(
        &mut self,
        _url: &str,
        _headers: Vec<(String, String)>,
    ) -> Result<(), Self::Error> {
        self.connected = true;
        Ok(())
    }

    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
        if !self.connected {
            return Err(MockTransportError::NotConnected);
        }
        self.sent.push(data);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        if !self.connected {
            return Err(MockTransportError::NotConnected);
        }
        // Yield to the scheduler so tests can observe intermediate states
        // (phase transitions, events) before the next message is processed.
        tokio::task::yield_now().await;

        if let Some(data) = self.recv_queue.pop_front() {
            return Ok(Some(data));
        }

        // Queue is empty: pend indefinitely, simulating a connected-but-idle
        // transport waiting for the next message from the server.
        // The connection loop uses `tokio::select!` so this future is dropped
        // when a command (e.g., Disconnect) arrives on the command channel.
        std::future::pending().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.connected = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_transport_round_trip() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        transport
            .connect("wss://example.com", vec![])
            .await
            .unwrap();
        transport.send(b"hello".to_vec()).await.unwrap();
        let data = transport.recv().await.unwrap();
        assert!(data.is_some());
        let text = String::from_utf8(data.unwrap()).unwrap();
        assert!(text.contains("setupComplete"));
    }

    #[tokio::test]
    async fn mock_transport_records_sent() {
        let mut transport = MockTransport::new();
        transport
            .connect("wss://example.com", vec![])
            .await
            .unwrap();
        transport.send(b"msg1".to_vec()).await.unwrap();
        transport.send(b"msg2".to_vec()).await.unwrap();
        let sent = transport.take_sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], b"msg1");
    }

    #[tokio::test]
    async fn mock_transport_recv_pends_when_queue_empty() {
        let mut transport = MockTransport::new();
        transport
            .connect("wss://example.com", vec![])
            .await
            .unwrap();
        // recv() should pend when queue is empty (simulating idle transport)
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), transport.recv()).await;
        assert!(result.is_err(), "recv should pend when queue is empty");
    }

    #[tokio::test]
    async fn mock_transport_recv_errors_when_not_connected() {
        let mut transport = MockTransport::new();
        // Not connected yet — recv should error
        let result = transport.recv().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_transport_not_connected_error() {
        let mut transport = MockTransport::new();
        let result = transport.send(b"data".to_vec()).await;
        assert!(result.is_err());
    }

    #[test]
    fn transport_trait_is_object_safe_check() {
        // Transport has an associated type, so it's not directly object-safe
        // but can be used as generic bounds. This test just verifies compilation.
        fn _assert_transport<T: Transport>() {}
        _assert_transport::<MockTransport>();
    }
}
