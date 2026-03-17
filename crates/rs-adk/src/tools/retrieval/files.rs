//! Files retrieval tool — retrieve context from local files.
//!
//! Mirrors ADK-Python's `files_retrieval` tool. Provides simple
//! substring-based retrieval from a collection of text files.

use std::path::PathBuf;

use async_trait::async_trait;

use super::base::{BaseRetrievalTool, RetrievalResult};
use crate::error::ToolError;

/// Retrieval tool that searches through local text files.
///
/// Performs simple substring matching over file contents and returns
/// relevant file chunks as retrieval results.
#[derive(Debug, Clone)]
pub struct FilesRetrievalTool {
    /// Paths to the files to search.
    files: Vec<PathBuf>,
    /// Chunk size in characters for splitting files.
    chunk_size: usize,
    /// Overlap between chunks in characters.
    chunk_overlap: usize,
}

impl FilesRetrievalTool {
    /// Create a new files retrieval tool.
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            chunk_size: 1000,
            chunk_overlap: 200,
        }
    }

    /// Set the chunk size for splitting files.
    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = size;
        self
    }

    /// Set the overlap between chunks.
    pub fn with_chunk_overlap(mut self, overlap: usize) -> Self {
        self.chunk_overlap = overlap;
        self
    }

    /// Split text into overlapping chunks.
    fn chunk_text(&self, text: &str) -> Vec<String> {
        if text.len() <= self.chunk_size {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut start = 0;
        while start < text.len() {
            let end = (start + self.chunk_size).min(text.len());
            chunks.push(text[start..end].to_string());
            if end >= text.len() {
                break;
            }
            start += self.chunk_size - self.chunk_overlap;
        }
        chunks
    }
}

#[async_trait]
impl BaseRetrievalTool for FilesRetrievalTool {
    fn name(&self) -> &str {
        "files_retrieval"
    }

    async fn retrieve(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<RetrievalResult>, ToolError> {
        let query_lower = query.to_lowercase();
        let mut all_results = Vec::new();

        for path in &self.files {
            let content = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read {}: {e}", path.display())))?;

            let chunks = self.chunk_text(&content);
            let source = path.display().to_string();

            for chunk in &chunks {
                let chunk_lower = chunk.to_lowercase();
                // Simple relevance scoring: count query term occurrences
                let words: Vec<&str> = query_lower.split_whitespace().collect();
                let matches = words
                    .iter()
                    .filter(|w| chunk_lower.contains(*w))
                    .count();

                if matches > 0 {
                    let score = matches as f64 / words.len().max(1) as f64;
                    all_results.push(RetrievalResult {
                        content: chunk.clone(),
                        source: source.clone(),
                        score,
                        metadata: serde_json::Value::Null,
                    });
                }
            }
        }

        // Sort by score descending and take top_k
        all_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        all_results.truncate(top_k);

        Ok(all_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_short_text() {
        let tool = FilesRetrievalTool::new(vec![]);
        let chunks = tool.chunk_text("short text");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short text");
    }

    #[test]
    fn chunk_long_text() {
        let tool = FilesRetrievalTool::new(vec![])
            .with_chunk_size(10)
            .with_chunk_overlap(3);
        let text = "abcdefghijklmnopqrstuvwxyz";
        let chunks = tool.chunk_text(text);
        assert!(chunks.len() > 1);
        // First chunk should be 10 chars
        assert_eq!(chunks[0].len(), 10);
    }

    #[tokio::test]
    async fn retrieve_from_nonexistent_file() {
        let tool = FilesRetrievalTool::new(vec![PathBuf::from("/nonexistent/file.txt")]);
        let result = tool.retrieve("test", 5).await;
        assert!(result.is_err());
    }
}
