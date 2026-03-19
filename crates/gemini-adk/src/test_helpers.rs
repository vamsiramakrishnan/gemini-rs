//! Shared test helpers for gemini-adk tests.

use async_trait::async_trait;
use gemini_live::prelude::FunctionResponse;
use gemini_live::session::{SessionError, SessionWriter};

/// Mock writer that accepts all commands without error.
pub struct MockWriter;

#[async_trait]
impl SessionWriter for MockWriter {
    async fn send_audio(&self, _data: Vec<u8>) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_text(&self, _text: String) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_tool_response(
        &self,
        _responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_client_content(
        &self,
        _turns: Vec<gemini_live::prelude::Content>,
        _turn_complete: bool,
    ) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_video(&self, _jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        Ok(())
    }
    async fn update_instruction(&self, _instruction: String) -> Result<(), SessionError> {
        Ok(())
    }
    async fn signal_activity_start(&self) -> Result<(), SessionError> {
        Ok(())
    }
    async fn signal_activity_end(&self) -> Result<(), SessionError> {
        Ok(())
    }
    async fn disconnect(&self) -> Result<(), SessionError> {
        Ok(())
    }
}
