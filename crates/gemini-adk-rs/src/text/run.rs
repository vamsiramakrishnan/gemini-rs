//! One run of a text agent: what goes in ([`RunRequest`]), what comes out
//! ([`RunResult`]), and a conversation that remembers ([`Chat`]).

use gemini_genai_rs::prelude::{Content, Part, Role};
use serde::de::DeserializeOwned;

use super::TextAgent;
use crate::error::{AgentError, ToolError};
use crate::llm::TokenUsage;
use crate::state::State;

/// What to run: the new user turn, the conversation before it, and optionally
/// the JSON shape the reply must take.
///
/// ```
/// use gemini_adk_rs::text::RunRequest;
///
/// let request = RunRequest::new("And in Celsius?")
///     .history(vec![/* earlier turns, e.g. from `RunResult::messages` */]);
/// assert_eq!(request.input_text(), "And in Celsius?");
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunRequest {
    /// The new user turn.
    pub input: Content,
    /// Earlier turns, oldest first, sent before `input`.
    pub history: Vec<Content>,
    /// A JSON Schema the reply must match; sent as the response schema by
    /// agents that call a model.
    pub response_schema: Option<serde_json::Value>,
}

impl RunRequest {
    /// A request whose new turn is `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self::from_content(Content::user(text.into()))
    }

    /// A request whose new turn is `input` — text with images, audio or files.
    pub fn from_content(input: Content) -> Self {
        Self {
            input,
            history: Vec::new(),
            response_schema: None,
        }
    }

    /// Send `history` before the new turn.
    pub fn history(mut self, history: Vec<Content>) -> Self {
        self.history = history;
        self
    }

    /// Require the reply to be JSON matching `schema`.
    pub fn response_schema(mut self, schema: serde_json::Value) -> Self {
        self.response_schema = Some(schema);
        self
    }

    /// The text of the new turn, its text parts joined.
    pub fn input_text(&self) -> String {
        text_of(&self.input)
    }
}

/// A tool call the model made during a run, and what it returned.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToolCallRecord {
    /// The tool's name.
    pub name: String,
    /// The arguments the model supplied.
    pub args: serde_json::Value,
    /// What the tool returned, or why it did not run.
    pub outcome: Result<serde_json::Value, ToolError>,
}

impl ToolCallRecord {
    /// Record one call.
    pub fn new(
        name: impl Into<String>,
        args: serde_json::Value,
        outcome: Result<serde_json::Value, ToolError>,
    ) -> Self {
        Self {
            name: name.into(),
            args,
            outcome,
        }
    }
}

/// The outcome of a run: the final text, and what it took to get there.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunResult {
    /// The final reply.
    pub text: String,
    /// The turns this run added, oldest first: the request's input, every
    /// model turn and every tool response. Append them to the history to
    /// continue the conversation.
    pub messages: Vec<Content>,
    /// Tokens used by this agent's own model calls. Composite agents
    /// (pipelines, fan-outs) do not add up their children's usage.
    pub usage: TokenUsage,
    /// Every tool call, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// How many times the model was called.
    pub model_calls: u32,
}

impl RunResult {
    /// A result carrying only its final text.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Parse the reply as JSON into `T`.
    ///
    /// A reply wrapped in a Markdown code fence is unwrapped first. Fails with
    /// [`AgentError::InvalidOutput`], which names the type and carries the
    /// reply.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, AgentError> {
        serde_json::from_str(strip_code_fence(&self.text)).map_err(|e| AgentError::InvalidOutput {
            expected: std::any::type_name::<T>(),
            reason: e.to_string(),
            text: self.text.clone(),
        })
    }
}

/// A conversation with an agent: every [`send`](Self::send) carries the turns
/// before it, and the conversation keeps its own [`State`].
///
/// ```
/// use gemini_adk_rs::llm::{LlmResponse, MockLlm};
/// use gemini_adk_rs::text::{LlmTextAgent, TextAgent};
///
/// # tokio_test::block_on(async {
/// let llm = MockLlm::script([
///     LlmResponse::from_text("Nice to meet you, Ada."),
///     LlmResponse::from_text("Your name is Ada."),
/// ]);
/// let agent = LlmTextAgent::new("assistant", llm.clone());
///
/// let mut chat = agent.chat();
/// chat.send("Hi, I'm Ada.").await.unwrap();
/// assert_eq!(chat.send("What's my name?").await.unwrap(), "Your name is Ada.");
///
/// // The second request carried the whole conversation.
/// assert_eq!(llm.last_request().unwrap().contents.len(), 3);
/// assert_eq!(chat.history().len(), 4);
/// # });
/// ```
///
/// `agent.chat()` borrows the agent; `Chat::new(agent)` takes it, for a
/// conversation that outlives the scope that built the agent.
pub struct Chat<A> {
    agent: A,
    history: Vec<Content>,
    state: State,
    usage: TokenUsage,
}

impl<A: TextAgent> Chat<A> {
    /// A new conversation with `agent`, with empty history and fresh state.
    pub fn new(agent: A) -> Self {
        Self::with_state(agent, State::new())
    }

    /// A new conversation that reads and writes `state`.
    pub fn with_state(agent: A, state: State) -> Self {
        Self {
            agent,
            history: Vec::new(),
            state,
            usage: TokenUsage::default(),
        }
    }

    /// Send a message and get the reply. The turn is added to the history
    /// only when it succeeds.
    pub async fn send(&mut self, message: impl Into<String>) -> Result<String, AgentError> {
        Ok(self.send_request(RunRequest::new(message)).await?.text)
    }

    /// Send a request (media, a response schema) and get the full result.
    /// The request's own history is replaced by the conversation's.
    pub async fn send_request(&mut self, request: RunRequest) -> Result<RunResult, AgentError> {
        let request = request.history(self.history.clone());
        let result = self.agent.run_with(request, &self.state).await?;
        self.history.extend(result.messages.iter().cloned());
        self.usage += result.usage;
        Ok(result)
    }

    /// Every turn so far, oldest first.
    pub fn history(&self) -> &[Content] {
        &self.history
    }

    /// Tokens used by the conversation so far.
    pub fn usage(&self) -> TokenUsage {
        self.usage
    }

    /// The conversation's state.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Forget the history (the state is kept).
    pub fn clear(&mut self) {
        self.history.clear();
    }
}

/// The text parts of a turn, joined.
pub(crate) fn text_of(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A model turn saying `text`.
pub(crate) fn model_turn(text: impl Into<String>) -> Content {
    Content {
        role: Some(Role::Model),
        parts: vec![Part::Text { text: text.into() }],
    }
}

/// `text` without a surrounding Markdown code fence (```` ```json … ``` ````).
fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(body) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let body = body.split_once('\n').map_or("", |(_, rest)| rest);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_plain_and_fenced_json() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct City {
            name: String,
        }
        let plain = RunResult::from_text(r#"{"name":"Paris"}"#);
        assert_eq!(plain.parse::<City>().unwrap().name, "Paris");
        let fenced = RunResult::from_text("```json\n{\"name\": \"Lyon\"}\n```");
        assert_eq!(fenced.parse::<City>().unwrap().name, "Lyon");
        let err = RunResult::from_text("Paris").parse::<City>().unwrap_err();
        assert!(
            matches!(&err, AgentError::InvalidOutput { text, expected, .. }
                if text == "Paris" && expected.ends_with("City")),
            "{err}"
        );
    }

    #[test]
    fn request_text_joins_text_parts() {
        let request = RunRequest::from_content(Content {
            role: Some(Role::User),
            parts: vec![
                Part::Text { text: "a".into() },
                Part::inline_data("image/png", "AAAA"),
                Part::Text { text: "b".into() },
            ],
        });
        assert_eq!(request.input_text(), "ab");
    }
}
