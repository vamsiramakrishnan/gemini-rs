use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeFile {
    pub name: String,
    pub content: String,
    pub mime_type: String,
}

#[derive(Debug, Clone)]
pub struct CodeExecutionInput {
    pub code: String,
    pub input_files: Vec<CodeFile>,
    pub execution_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodeExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub output_files: Vec<CodeFile>,
}

impl CodeExecutionResult {
    pub fn empty() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            output_files: Vec::new(),
        }
    }
}
