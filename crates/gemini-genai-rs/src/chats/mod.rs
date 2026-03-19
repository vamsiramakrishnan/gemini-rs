//! Stateful chat API — multi-turn conversation over generateContent.
//!
//! Feature-gated behind `chats` (depends on `generate`).
//!
//! Wraps the generateContent endpoint with automatic conversation history
//! management, similar to js-genai's `ChatSession`.

use crate::client::Client;
use crate::generate::{GenerateContentConfig, GenerateContentResponse, GenerateError};
use crate::protocol::types::{Content, GeminiModel};

/// A stateful chat session that tracks conversation history.
///
/// Each call to `send_message` appends the user message and model response
/// to the history, so subsequent calls include full conversation context.
pub struct ChatSession<'a> {
    client: &'a Client,
    model: GeminiModel,
    history: Vec<Content>,
    system_instruction: Option<String>,
}

impl<'a> ChatSession<'a> {
    /// Send a text message and get the model's response.
    ///
    /// The message and response are appended to conversation history.
    pub async fn send_message(
        &mut self,
        text: impl Into<String>,
    ) -> Result<GenerateContentResponse, GenerateError> {
        let user_content = Content::user(text);
        self.history.push(user_content);

        let mut config = GenerateContentConfig::from_contents(self.history.clone());
        if let Some(ref si) = self.system_instruction {
            config = config.system_instruction(si.clone());
        }

        let response = self
            .client
            .generate_content_with(config, Some(&self.model))
            .await?;

        // Append model response to history
        if let Some(candidate) = response.candidates.first() {
            if let Some(content) = &candidate.content {
                self.history.push(content.clone());
            }
        }

        Ok(response)
    }

    /// Get the current conversation history.
    pub fn history(&self) -> &[Content] {
        &self.history
    }

    /// Get the number of turns in the conversation.
    pub fn turn_count(&self) -> usize {
        self.history.len()
    }
}

impl Client {
    /// Start a new chat session with the default model.
    pub fn chat(&self) -> ChatSessionBuilder<'_> {
        ChatSessionBuilder {
            client: self,
            model: self.default_model().clone(),
            history: vec![],
            system_instruction: None,
        }
    }
}

/// Builder for configuring a [`ChatSession`].
pub struct ChatSessionBuilder<'a> {
    client: &'a Client,
    model: GeminiModel,
    history: Vec<Content>,
    system_instruction: Option<String>,
}

impl<'a> ChatSessionBuilder<'a> {
    /// Set the model for this chat.
    pub fn model(mut self, model: GeminiModel) -> Self {
        self.model = model;
        self
    }

    /// Set initial conversation history (for resuming).
    pub fn history(mut self, history: Vec<Content>) -> Self {
        self.history = history;
        self
    }

    /// Set system instruction.
    pub fn system_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.system_instruction = Some(instruction.into());
        self
    }

    /// Build the chat session.
    pub fn build(self) -> ChatSession<'a> {
        ChatSession {
            client: self.client,
            model: self.model,
            history: self.history,
            system_instruction: self.system_instruction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_builder() {
        let client = Client::from_api_key("key");
        let chat = client
            .chat()
            .model(GeminiModel::Gemini2_0FlashLive)
            .system_instruction("You are helpful")
            .build();
        assert_eq!(chat.turn_count(), 0);
        assert!(chat.history().is_empty());
    }

    #[test]
    fn chat_with_initial_history() {
        use crate::protocol::types::{Part, Role};
        let client = Client::from_api_key("key");
        let history = vec![
            Content::user("Hello"),
            Content {
                role: Some(Role::Model),
                parts: vec![Part::text("Hi there!")],
            },
        ];
        let chat = client.chat().history(history).build();
        assert_eq!(chat.turn_count(), 2);
    }
}
