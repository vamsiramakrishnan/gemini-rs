//! Shared test helpers for gemini-adk-rs tests.

use async_trait::async_trait;
use gemini_genai_rs::prelude::FunctionResponse;
use gemini_genai_rs::session::{SessionError, SessionWriter};

use crate::agent::Agent;
use crate::context::InvocationContext;
use crate::error::AgentError;

/// Agent that completes immediately.
pub(crate) struct NoopAgent {
    pub(crate) name: String,
}

#[async_trait]
impl Agent for NoopAgent {
    fn name(&self) -> &str {
        &self.name
    }
    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        Ok(())
    }
}

/// Mock writer that accepts all commands without error.
pub(crate) struct MockWriter;

#[async_trait]
impl SessionWriter for MockWriter {
    async fn send_audio(&self, _data: bytes::Bytes) -> Result<(), SessionError> {
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
        _turns: Vec<gemini_genai_rs::prelude::Content>,
        _turn_complete: bool,
    ) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_video(&self, _jpeg_data: bytes::Bytes) -> Result<(), SessionError> {
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
