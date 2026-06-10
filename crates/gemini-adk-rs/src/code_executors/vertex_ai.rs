//! Vertex AI code executor — runs code via the Vertex AI Code Interpreter Extension.
//!
//! Mirrors ADK-Python's `vertex_ai_code_executor`. The Python SDK wraps the
//! Vertex AI **Extension** service (`vertexai.preview.extensions.Extension`):
//! it loads an existing code-interpreter extension (or imports one from the
//! prebuilt hub) and then calls its `execute` operation. This Rust port talks
//! directly to the underlying Vertex AI Extension Execution REST API:
//!
//! * Import from hub:
//!   `POST https://{loc}-aiplatform.googleapis.com/v1beta1/projects/{p}/locations/{loc}/extensions:import`
//! * Execute:
//!   `POST https://{loc}-aiplatform.googleapis.com/v1beta1/{resource_name}:execute`
//!   with body `{"operationId": "execute", "operationParams": {...}}`.
//!
//! The execute response carries a `content` payload holding `execution_result`
//! (stdout), `execution_error` (stderr) and `output_files` (a list of
//! `{name, contents}`), exactly as ADK consumes them.
//!
//! The HTTP integration is gated behind the `vertex-ai-code-executor` feature
//! (which pulls in `reqwest`). Without it, the executor and its types still
//! compile as a drop-in, but `execute_code` returns an error explaining the
//! feature is required — so the request/response helpers are only exercised
//! when the feature is enabled.
#![cfg_attr(
    not(feature = "vertex-ai-code-executor"),
    allow(dead_code, unused_imports)
)]

use std::sync::Arc;

use async_trait::async_trait;

use super::base::{CodeExecutor, CodeExecutorError};
use super::types::{CodeExecutionInput, CodeExecutionResult, CodeFile};

/// Library preamble injected ahead of user code, mirroring ADK's
/// `_IMPORTED_LIBRARIES`. Makes the common data-science stack and the
/// `explore_df` helper available to executed snippets.
const IMPORTED_LIBRARIES: &str = r#"
import io
import math
import re

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import scipy

def crop(s: str, max_chars: int = 64) -> str:
  """Crops a string to max_chars characters."""
  return s[: max_chars - 3] + '...' if len(s) > max_chars else s


def explore_df(df: pd.DataFrame) -> None:
  """Prints some information about a pandas DataFrame."""

  with pd.option_context(
      'display.max_columns', None, 'display.expand_frame_repr', False
  ):
    # Print the column names to never encounter KeyError when selecting one.
    df_dtypes = df.dtypes

    # Obtain information about data types and missing values.
    df_nulls = (len(df) - df.isnull().sum()).apply(
        lambda x: f'{x} / {df.shape[0]} non-null'
    )

    # Explore unique total values in columns using `.unique()`.
    df_unique_count = df.apply(lambda x: len(x.unique()))

    # Explore unique values in columns using `.unique()`.
    df_unique = df.apply(lambda x: crop(str(list(x.unique()))))

    df_info = pd.concat(
        (
            df_dtypes.rename('Dtype'),
            df_nulls.rename('Non-Null Count'),
            df_unique_count.rename('Unique Values Count'),
            df_unique.rename('Unique Values'),
        ),
        axis=1,
    )
    df_info.index.name = 'Columns'
    print(f"""Total rows: {df.shape[0]}
Total columns: {df.shape[1]}

{df_info}""")
"#;

/// Image extensions that map to `image/{ext}` MIME types (mirrors ADK's
/// `_SUPPORTED_IMAGE_TYPES`).
const SUPPORTED_IMAGE_TYPES: &[&str] = &["png", "jpg", "jpeg"];

/// Data-file extensions that map to `text/{ext}` MIME types (mirrors ADK's
/// `_SUPPORTED_DATA_FILE_TYPES`).
const SUPPORTED_DATA_FILE_TYPES: &[&str] = &["csv"];

/// Configuration for Vertex AI code execution.
#[derive(Debug, Clone)]
pub struct VertexAiCodeExecutorConfig {
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location (e.g., "us-central1").
    pub location: String,
    /// Execution timeout in seconds.
    pub timeout_secs: u64,
    /// If set, the full resource name of an existing code interpreter
    /// extension to reuse instead of importing a new one. Format:
    /// `projects/123/locations/us-central1/extensions/456`.
    pub resource_name: Option<String>,
}

impl VertexAiCodeExecutorConfig {
    /// Create a new config with the default timeout (no preexisting extension).
    pub fn new(project: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            location: location.into(),
            timeout_secs: 60,
            resource_name: None,
        }
    }

    /// Reuse an existing code interpreter extension resource name.
    ///
    /// Format: `projects/123/locations/us-central1/extensions/456`.
    pub fn resource_name(mut self, name: impl Into<String>) -> Self {
        self.resource_name = Some(name.into());
        self
    }

    /// Override the execution timeout in seconds.
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Base Vertex AI API URL for this project/location.
    fn api_base(&self) -> String {
        format!(
            "https://{location}-aiplatform.googleapis.com/v1beta1",
            location = self.location,
        )
    }

    /// Parent resource for the `extensions:import` call.
    fn extensions_parent(&self) -> String {
        format!(
            "{}/projects/{}/locations/{}",
            self.api_base(),
            self.project,
            self.location,
        )
    }
}

/// How to supply a bearer token for Vertex AI requests. Mirrors the token
/// pattern used by [`crate::session::VertexAiSessionService`].
enum TokenProvider {
    /// No token configured — requests fail with a clear message.
    None,
    /// A static, pre-fetched bearer token string.
    Static(String),
    /// A dynamic refresher: called before every request.
    Refresher(Arc<dyn Fn() -> String + Send + Sync>),
}

impl std::fmt::Debug for TokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenProvider::None => f.write_str("TokenProvider::None"),
            TokenProvider::Static(_) => f.write_str("TokenProvider::Static(..)"),
            TokenProvider::Refresher(_) => f.write_str("TokenProvider::Refresher(..)"),
        }
    }
}

impl TokenProvider {
    fn get(&self) -> Result<String, CodeExecutorError> {
        match self {
            TokenProvider::None => Err(CodeExecutorError::Other(
                "missing auth token: call .with_token() or .with_token_refresher()".into(),
            )),
            TokenProvider::Static(t) => Ok(t.clone()),
            TokenProvider::Refresher(f) => Ok(f()),
        }
    }
}

/// Code executor that runs code via the Vertex AI Code Interpreter Extension.
///
/// Uses the Vertex AI Extension Execution REST API to run Python code in a
/// Google-managed sandboxed environment, mirroring ADK-Python's
/// `VertexAiCodeExecutor`.
///
/// # Quick start
///
/// ```rust,no_run
/// # use gemini_adk_rs::code_executors::{VertexAiCodeExecutor, VertexAiCodeExecutorConfig};
/// let executor = VertexAiCodeExecutor::new(
///     VertexAiCodeExecutorConfig::new("my-project", "us-central1"),
/// )
/// .with_token("ya29.my-access-token");
/// ```
#[derive(Debug)]
pub struct VertexAiCodeExecutor {
    config: VertexAiCodeExecutorConfig,
    token_provider: TokenProvider,
    #[cfg(feature = "vertex-ai-code-executor")]
    client: reqwest::Client,
    /// Lazily-resolved extension resource name (cached after first import).
    extension_name: parking_lot::Mutex<Option<String>>,
}

impl VertexAiCodeExecutor {
    /// Create a new Vertex AI code executor.
    ///
    /// No auth token is configured yet — call [`with_token`](Self::with_token)
    /// or [`with_token_refresher`](Self::with_token_refresher) before issuing
    /// requests.
    pub fn new(config: VertexAiCodeExecutorConfig) -> Self {
        let extension_name = parking_lot::Mutex::new(config.resource_name.clone());
        Self {
            config,
            token_provider: TokenProvider::None,
            #[cfg(feature = "vertex-ai-code-executor")]
            client: reqwest::Client::new(),
            extension_name,
        }
    }

    /// Set a static bearer token for all requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token_provider = TokenProvider::Static(token.into());
        self
    }

    /// Set a dynamic token refresher closure, invoked before every request.
    pub fn with_token_refresher(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.token_provider = TokenProvider::Refresher(Arc::new(f));
        self
    }

    /// Returns the configured project ID.
    pub fn project(&self) -> &str {
        &self.config.project
    }

    /// Returns the configured location.
    pub fn location(&self) -> &str {
        &self.config.location
    }

    /// Build the code string with the standard ADK library preamble.
    fn code_with_imports(code: &str) -> String {
        format!("\n{IMPORTED_LIBRARIES}\n\n{code}\n")
    }

    /// Map a code-interpreter output file into a [`CodeFile`], guessing a MIME
    /// type from the extension exactly as ADK does.
    fn map_output_file(name: String, contents: String) -> CodeFile {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let mime_type = if SUPPORTED_IMAGE_TYPES.contains(&ext.as_str()) {
            format!("image/{ext}")
        } else if SUPPORTED_DATA_FILE_TYPES.contains(&ext.as_str()) {
            format!("text/{ext}")
        } else {
            guess_mime_type(&name)
        };
        CodeFile {
            name,
            content: contents,
            mime_type,
        }
    }
}

/// Code-interpreter output file as returned by the extension.
#[cfg(feature = "vertex-ai-code-executor")]
#[derive(Debug, serde::Deserialize)]
struct OutputFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    contents: String,
}

/// The decoded `content` payload of an `:execute` response.
#[cfg(feature = "vertex-ai-code-executor")]
#[derive(Debug, Default, serde::Deserialize)]
struct ExecuteContent {
    #[serde(default)]
    execution_result: String,
    #[serde(default)]
    execution_error: String,
    #[serde(default)]
    output_files: Vec<OutputFile>,
}

#[cfg(feature = "vertex-ai-code-executor")]
impl VertexAiCodeExecutor {
    /// Resolve (loading or importing) the code interpreter extension resource
    /// name, caching it for subsequent calls.
    async fn ensure_extension(&self, token: &str) -> Result<String, CodeExecutorError> {
        if let Some(name) = self.extension_name.lock().clone() {
            return Ok(name);
        }

        // Import the prebuilt "code_interpreter" extension from the hub.
        // Mirrors ADK's `Extension.from_hub('code_interpreter')`.
        let url = format!("{}/extensions:import", self.config.extensions_parent());
        let body = serde_json::json!({
            "displayName": "Code Interpreter",
            "description": "This extension generates and executes code in the specified language",
            "manifest": {
                "name": "code_interpreter_tool",
                "description": "Google Code Interpreter Extension",
                "apiSpec": {
                    "openApiGcsUri": "gs://vertex-extension-public/code_interpreter.yaml"
                },
                "authConfig": {
                    "authType": "GOOGLE_SERVICE_ACCOUNT_AUTH",
                    "googleServiceAccountConfig": {}
                }
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| CodeExecutorError::Other(format!("extension import failed: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| CodeExecutorError::Other(format!("reading import response: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(CodeExecutorError::Other(format!(
                "Vertex AI extension import failed [{status}]: {text}"
            )));
        }

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CodeExecutorError::Other(format!("parsing import response: {e}")))?;
        // The import op may return the extension directly (`name`) or wrap it in
        // an LRO with the resource under `response.name`.
        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|n| n.contains("/extensions/"))
            .or_else(|| json.pointer("/response/name").and_then(|v| v.as_str()))
            .map(String::from)
            .ok_or_else(|| {
                CodeExecutorError::Other(format!(
                    "extension import returned no resource name: {text}"
                ))
            })?;

        *self.extension_name.lock() = Some(name.clone());
        Ok(name)
    }

    /// Call the extension's `execute` operation and decode its content payload.
    async fn execute_code_interpreter(
        &self,
        code: &str,
        input_files: &[CodeFile],
        session_id: Option<&str>,
    ) -> Result<ExecuteContent, CodeExecutorError> {
        let token = self.token_provider.get()?;
        let extension = self.ensure_extension(&token).await?;

        let mut operation_params = serde_json::json!({ "code": code });
        if !input_files.is_empty() {
            operation_params["files"] = serde_json::Value::Array(
                input_files
                    .iter()
                    .map(|f| serde_json::json!({ "name": f.name, "contents": f.content }))
                    .collect(),
            );
        }
        if let Some(sid) = session_id {
            operation_params["session_id"] = serde_json::Value::String(sid.to_string());
        }

        let url = format!("{}/{extension}:execute", self.config.api_base());
        let body = serde_json::json!({
            "operationId": "execute",
            "operationParams": operation_params,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                CodeExecutorError::ExecutionFailed(format!("execute request failed: {e}"))
            })?;

        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| {
            CodeExecutorError::ExecutionFailed(format!("reading execute response: {e}"))
        })?;
        if !(200..300).contains(&status) {
            return Err(CodeExecutorError::ExecutionFailed(format!(
                "Vertex AI code execution failed [{status}]: {text}"
            )));
        }

        parse_execute_content(&text)
    }
}

/// Parse the body of an `:execute` response into an [`ExecuteContent`].
///
/// The extension wraps its payload in a `content` field, which may be either a
/// JSON object or a JSON-encoded string. Both shapes are handled, as is a
/// top-level payload with no `content` wrapper.
#[cfg(feature = "vertex-ai-code-executor")]
fn parse_execute_content(text: &str) -> Result<ExecuteContent, CodeExecutorError> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        CodeExecutorError::ExecutionFailed(format!("parsing execute response: {e}"))
    })?;

    let content = json.get("content").unwrap_or(&json);
    let value = match content {
        // `content` delivered as a JSON-encoded string.
        serde_json::Value::String(s) => serde_json::from_str(s).map_err(|e| {
            CodeExecutorError::ExecutionFailed(format!("parsing execute content string: {e}"))
        })?,
        other => other.clone(),
    };

    serde_json::from_value(value).map_err(|e| {
        CodeExecutorError::ExecutionFailed(format!("decoding execute content fields: {e}"))
    })
}

/// Best-effort MIME type guess from a file extension. Covers the common types
/// produced by the code interpreter; falls back to `text/plain`.
fn guess_mime_type(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "txt" | "log" => "text/plain",
        "pdf" => "application/pdf",
        "xml" => "application/xml",
        _ => "text/plain",
    }
    .to_string()
}

#[async_trait]
impl CodeExecutor for VertexAiCodeExecutor {
    async fn execute_code(
        &self,
        input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError> {
        #[cfg(not(feature = "vertex-ai-code-executor"))]
        {
            let _ = &input;
            return Err(CodeExecutorError::Other(
                "VertexAiCodeExecutor requires the `vertex-ai-code-executor` feature".into(),
            ));
        }

        #[cfg(feature = "vertex-ai-code-executor")]
        {
            let code = Self::code_with_imports(&input.code);
            let content = self
                .execute_code_interpreter(&code, &input.input_files, input.execution_id.as_deref())
                .await?;

            let output_files = content
                .output_files
                .into_iter()
                .map(|f| Self::map_output_file(f.name, f.contents))
                .collect();

            Ok(CodeExecutionResult {
                stdout: content.execution_result,
                stderr: content.execution_error,
                output_files,
            })
        }
    }

    fn stateful(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VertexAiCodeExecutorConfig {
        VertexAiCodeExecutorConfig::new("test-project", "us-central1")
    }

    #[test]
    fn executor_metadata() {
        let exec = VertexAiCodeExecutor::new(test_config());
        assert_eq!(exec.project(), "test-project");
        assert_eq!(exec.location(), "us-central1");
        assert!(exec.stateful());
    }

    #[test]
    fn config_builder() {
        let cfg = VertexAiCodeExecutorConfig::new("p", "us-central1")
            .resource_name("projects/p/locations/us-central1/extensions/42")
            .timeout_secs(120);
        assert_eq!(cfg.timeout_secs, 120);
        assert_eq!(
            cfg.resource_name.as_deref(),
            Some("projects/p/locations/us-central1/extensions/42")
        );
    }

    #[test]
    fn api_url_construction() {
        let cfg = test_config();
        assert_eq!(
            cfg.api_base(),
            "https://us-central1-aiplatform.googleapis.com/v1beta1"
        );
        assert!(cfg
            .extensions_parent()
            .ends_with("/projects/test-project/locations/us-central1"));
    }

    #[test]
    fn preexisting_resource_name_is_cached() {
        let cfg = test_config().resource_name("projects/p/locations/l/extensions/9");
        let exec = VertexAiCodeExecutor::new(cfg);
        assert_eq!(
            exec.extension_name.lock().as_deref(),
            Some("projects/p/locations/l/extensions/9")
        );
    }

    #[test]
    fn code_with_imports_includes_preamble() {
        let code = VertexAiCodeExecutor::code_with_imports("print(1)");
        assert!(code.contains("import pandas as pd"));
        assert!(code.contains("def explore_df"));
        assert!(code.contains("print(1)"));
    }

    #[test]
    fn map_output_file_image_mime() {
        let f = VertexAiCodeExecutor::map_output_file("chart.png".into(), "AAAA".into());
        assert_eq!(f.mime_type, "image/png");
        let f = VertexAiCodeExecutor::map_output_file("photo.JPG".into(), "AAAA".into());
        assert_eq!(f.mime_type, "image/jpg");
    }

    #[test]
    fn map_output_file_data_mime() {
        let f = VertexAiCodeExecutor::map_output_file("out.csv".into(), "a,b".into());
        assert_eq!(f.mime_type, "text/csv");
    }

    #[test]
    fn map_output_file_fallback_mime() {
        let f = VertexAiCodeExecutor::map_output_file("notes.txt".into(), "hi".into());
        assert_eq!(f.mime_type, "text/plain");
        let f = VertexAiCodeExecutor::map_output_file("data.json".into(), "{}".into());
        assert_eq!(f.mime_type, "application/json");
    }

    #[test]
    fn with_token_sets_provider() {
        let exec = VertexAiCodeExecutor::new(test_config()).with_token("tok123");
        assert_eq!(exec.token_provider.get().unwrap(), "tok123");
    }

    #[test]
    fn missing_token_errors() {
        let exec = VertexAiCodeExecutor::new(test_config());
        assert!(exec.token_provider.get().is_err());
    }

    #[cfg(feature = "vertex-ai-code-executor")]
    #[test]
    fn parse_execute_content_object() {
        let body = serde_json::json!({
            "content": {
                "execution_result": "42\n",
                "execution_error": "",
                "output_files": [{"name": "chart.png", "contents": "AAAA"}],
            }
        })
        .to_string();
        let content = parse_execute_content(&body).unwrap();
        assert_eq!(content.execution_result, "42\n");
        assert_eq!(content.output_files.len(), 1);
        assert_eq!(content.output_files[0].name, "chart.png");
    }

    #[cfg(feature = "vertex-ai-code-executor")]
    #[test]
    fn parse_execute_content_stringified() {
        let inner = serde_json::json!({
            "execution_result": "ok",
            "execution_error": "boom",
            "output_files": [],
        })
        .to_string();
        let body = serde_json::json!({ "content": inner }).to_string();
        let content = parse_execute_content(&body).unwrap();
        assert_eq!(content.execution_result, "ok");
        assert_eq!(content.execution_error, "boom");
    }

    #[cfg(feature = "vertex-ai-code-executor")]
    #[test]
    fn parse_execute_content_no_wrapper() {
        let body = serde_json::json!({
            "execution_result": "hi",
            "execution_error": "",
            "output_files": [],
        })
        .to_string();
        let content = parse_execute_content(&body).unwrap();
        assert_eq!(content.execution_result, "hi");
    }

    #[cfg(not(feature = "vertex-ai-code-executor"))]
    #[tokio::test]
    async fn execute_without_feature_errors() {
        let exec = VertexAiCodeExecutor::new(test_config()).with_token("t");
        let input = CodeExecutionInput {
            code: "print(42)".into(),
            input_files: vec![],
            execution_id: None,
        };
        assert!(exec.execute_code(input).await.is_err());
    }
}
