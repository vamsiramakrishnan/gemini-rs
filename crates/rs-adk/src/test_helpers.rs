//! Shared test helpers for rs-adk tests.

use async_trait::async_trait;
use rs_genai::prelude::FunctionResponse;
use rs_genai::session::{SessionError, SessionWriter};

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
        _turns: Vec<rs_genai::prelude::Content>,
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
