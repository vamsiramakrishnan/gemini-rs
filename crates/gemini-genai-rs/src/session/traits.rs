//! Session traits for testability and middleware injection.
//!
//! [`SessionWriter`] — write-side: send commands without owning the full handle.
//! [`SessionReader`] — read-side: subscribe to events and observe phase.

use super::errors::SessionError;
use super::events::SessionEvent;
use super::state::SessionPhase;
use crate::protocol::{Content, FunctionResponse};
use async_trait::async_trait;
use tokio::sync::broadcast;

/// Write-side of a session — send commands without owning the full handle.
#[async_trait]
pub trait SessionWriter: Send + Sync + 'static {
    /// Send raw PCM16 audio bytes.
    async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError>;
    /// Send a text message.
    async fn send_text(&self, text: String) -> Result<(), SessionError>;
    /// Send tool/function call responses back to the model.
    async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError>;
    /// Send client content (conversation history or context).
    async fn send_client_content(
        &self,
        turns: Vec<Content>,
        turn_complete: bool,
    ) -> Result<(), SessionError>;
    /// Send a video/image frame (raw JPEG bytes).
    async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError>;
    /// Update the system instruction mid-session.
    async fn update_instruction(&self, instruction: String) -> Result<(), SessionError>;
    /// Signal that user speech activity has started.
    async fn signal_activity_start(&self) -> Result<(), SessionError>;
    /// Signal that user speech activity has ended.
    async fn signal_activity_end(&self) -> Result<(), SessionError>;
    /// Gracefully disconnect the session.
    async fn disconnect(&self) -> Result<(), SessionError>;
}

/// Read-side of a session — subscribe to events and observe phase.
pub trait SessionReader: Send + Sync + 'static {
    /// Subscribe to the session event broadcast stream.
    fn subscribe(&self) -> broadcast::Receiver<SessionEvent>;
    /// Returns the current session phase.
    fn phase(&self) -> SessionPhase;
    /// Returns the unique session ID.
    fn session_id(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::super::handle::SessionHandle;
    use super::*;

    #[test]
    fn session_handle_implements_session_writer() {
        fn assert_impl<T: SessionWriter>() {}
        assert_impl::<SessionHandle>();
    }

    #[test]
    fn session_handle_implements_session_reader() {
        fn assert_impl<T: SessionReader>() {}
        assert_impl::<SessionHandle>();
    }

    #[test]
    fn session_writer_is_object_safe() {
        fn _assert(_: &dyn SessionWriter) {}
    }

    #[test]
    fn session_reader_is_object_safe() {
        fn _assert(_: &dyn SessionReader) {}
    }
}
