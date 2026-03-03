use serde::{Deserialize, Serialize};

/// A file used in code execution (input or output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeFile {
    /// File name.
    pub name: String,
    /// File content as a string.
    pub content: String,
    /// MIME type of the file.
    pub mime_type: String,
}

/// Input for a code execution request.
#[derive(Debug, Clone)]
pub struct CodeExecutionInput {
    /// The source code to execute.
    pub code: String,
    /// Input files available to the code.
    pub input_files: Vec<CodeFile>,
    /// Optional execution identifier for tracking.
    pub execution_id: Option<String>,
}

/// Result of a code execution.
#[derive(Debug, Clone)]
pub struct CodeExecutionResult {
    /// Standard output from execution.
    pub stdout: String,
    /// Standard error from execution.
    pub stderr: String,
    /// Files produced by the execution.
    pub output_files: Vec<CodeFile>,
}

impl CodeExecutionResult {
    /// Create an empty result with no output.
    pub fn empty() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            output_files: Vec::new(),
        }
    }
}
