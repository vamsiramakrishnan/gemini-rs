//! One error type for application code.
//!
//! Each layer keeps its own precise error: [`AgentError`] for agents and
//! Live sessions, [`ConversationError`] for the conversation compiler,
//! [`PersistenceError`] for snapshots, and so on. An application that uses
//! several of them in one function wants one error to `?` into. [`Error`]
//! converts from each of them, keeps the original inside so nothing is
//! lost, and answers the questions a caller usually has
//! ([`is_retryable`](Error::is_retryable), [`llm`](Error::llm)).
//!
//! ```no_run
//! use gemini_adk::prelude::*;
//! use gemini_adk::conversation::Conversation;
//!
//! async fn run() -> gemini_adk::Result<()> {
//!     let convo = Conversation::new("booking")
//!         .stage("collect").collect(["party_size"])
//!         .stage("done").after("collect").terminal()
//!         .compile()?; // a ConversationError
//!
//!     let agent = AgentBuilder::new("assistant")
//!         .instruction("Answer in one sentence.")
//!         .build(GeminiLlm::from_env()?)?; // an LlmError, then an AgentError
//!     println!("{}", agent.ask("Hello").await?);
//!
//!     let spec = std::fs::read_to_string("booking.json")?; // an io::Error
//!     let _ = (convo, spec);
//!     Ok(())
//! }
//! ```

use gemini_adk_fluent_rs::conversation::ConversationError;
use gemini_adk_fluent_rs::gemini_adk_rs::error::{AgentError, ConfigError, ToolError};
use gemini_adk_fluent_rs::gemini_adk_rs::flow::FlowErrors;
use gemini_adk_fluent_rs::gemini_adk_rs::live::PersistenceError;
use gemini_adk_fluent_rs::gemini_adk_rs::llm::LlmError;
use gemini_adk_fluent_rs::gemini_adk_rs::state::StateError;
use gemini_adk_fluent_rs::gemini_genai_rs::session::SessionError;
use gemini_adk_fluent_rs::gemini_genai_rs::transport::WireLogError;

/// Any error the SDK returns. See the [module docs](self).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An agent, a Live session, a tool, a model call or state.
    Agent(AgentError),
    /// A conversation spec failed to compile.
    Conversation(ConversationError),
    /// A flow failed to compile.
    Flow(FlowErrors),
    /// A session snapshot could not be saved or loaded.
    Persistence(PersistenceError),
    /// A wire log could not be read.
    WireLog(WireLogError),
    /// Contextual memory failed (feature `memory`).
    #[cfg(feature = "memory")]
    Memory(gemini_memory_rs::core::MemoryError),
    /// A file or stream failed.
    Io(std::io::Error),
    /// JSON could not be read or written.
    Json(serde_json::Error),
}

/// `Result` with [`Error`] as the default error.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// The model call's error, when the failure was a model call.
    pub fn llm(&self) -> Option<&LlmError> {
        match self {
            Error::Agent(AgentError::Llm(e)) => Some(e),
            _ => None,
        }
    }

    /// Whether trying again later could succeed: a rate limit, a transient
    /// provider or transport failure, a timeout. A configuration, spec or
    /// input error is not.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Agent(AgentError::Llm(e)) => e.is_retryable(),
            Error::Agent(AgentError::Timeout) => true,
            Error::Agent(AgentError::Tool(ToolError::Timeout(_))) => true,
            Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Agent(e) => e.fmt(f),
            Error::Conversation(e) => e.fmt(f),
            Error::Flow(e) => e.fmt(f),
            Error::Persistence(e) => e.fmt(f),
            Error::WireLog(e) => e.fmt(f),
            #[cfg(feature = "memory")]
            Error::Memory(e) => e.fmt(f),
            Error::Io(e) => e.fmt(f),
            Error::Json(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Agent(e) => Some(e),
            Error::Conversation(e) => Some(e),
            Error::Flow(e) => Some(e),
            Error::Persistence(e) => Some(e),
            Error::WireLog(e) => Some(e),
            #[cfg(feature = "memory")]
            Error::Memory(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
        }
    }
}

macro_rules! from {
    ($($ty:ty => $variant:ident),* $(,)?) => {$(
        impl From<$ty> for Error {
            fn from(e: $ty) -> Self {
                Error::$variant(e)
            }
        }
    )*};
}

from! {
    AgentError => Agent,
    ConversationError => Conversation,
    FlowErrors => Flow,
    PersistenceError => Persistence,
    WireLogError => WireLog,
    std::io::Error => Io,
    serde_json::Error => Json,
}

#[cfg(feature = "memory")]
from! { gemini_memory_rs::core::MemoryError => Memory }

// The errors `AgentError` already wraps convert through it, so `?` works on
// them directly too.
macro_rules! via_agent {
    ($($ty:ty),* $(,)?) => {$(
        impl From<$ty> for Error {
            fn from(e: $ty) -> Self {
                Error::Agent(AgentError::from(e))
            }
        }
    )*};
}

via_agent!(SessionError, ToolError, LlmError, StateError, ConfigError);

#[cfg(test)]
mod tests {
    use super::*;

    fn io() -> Result<()> {
        std::fs::read_to_string("/definitely/not/here")?;
        Ok(())
    }

    fn model() -> Result<()> {
        Err(LlmError::RateLimited)?
    }

    fn spec() -> Result<()> {
        gemini_adk_fluent_rs::conversation::Conversation::new("empty").compile()?;
        Ok(())
    }

    #[test]
    fn every_layer_converts_and_keeps_its_cause() {
        let e = io().unwrap_err();
        assert!(matches!(e, Error::Io(_)));

        let e = model().unwrap_err();
        assert!(e.is_retryable());
        assert!(matches!(e.llm(), Some(LlmError::RateLimited)));
        assert!(std::error::Error::source(&e).is_some());

        let e = spec().unwrap_err();
        assert!(matches!(e, Error::Conversation(_)));
        assert!(!e.is_retryable());
    }
}
