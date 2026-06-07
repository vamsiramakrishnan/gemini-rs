//! Retrieval tools — provide RAG (Retrieval-Augmented Generation) capabilities.
//!
//! Mirrors ADK-Python's `tools/retrieval` module. Provides base traits
//! and implementations for retrieving relevant documents to augment
//! LLM context.

mod base;
mod files;
#[cfg(feature = "vertex-ai-rag")]
mod vertex_ai_rag;

pub use base::{BaseRetrievalTool, RetrievalResult};
pub use files::FilesRetrievalTool;
#[cfg(feature = "vertex-ai-rag")]
pub use vertex_ai_rag::{VertexAiRagConfig, VertexAiRagRetrievalTool};
