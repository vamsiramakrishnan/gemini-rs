//! Retrieval tools — provide RAG (Retrieval-Augmented Generation) capabilities.
//!
//! Mirrors ADK-Python's `tools/retrieval` module. Provides base traits
//! and implementations for retrieving relevant documents to augment
//! LLM context.

mod base;
mod files;
mod vertex_ai_rag;

pub use base::{BaseRetrievalTool, RetrievalResult};
pub use files::FilesRetrievalTool;
pub use vertex_ai_rag::VertexAiRagRetrievalTool;
