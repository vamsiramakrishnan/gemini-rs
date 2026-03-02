//! Files API — upload, download, list, delete files.
//!
//! Feature-gated behind `files`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::transport::auth::ServiceEndpoint;

/// State of a file in the Files API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileState {
    Processing,
    Active,
    Failed,
}

/// Source type for file registration (Vertex AI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSource {
    /// GCS URI of the file (e.g., `gs://bucket/path`).
    pub file_uri: String,
    /// MIME type of the file.
    pub mime_type: String,
}

/// A file resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    /// Resource name (e.g., `files/abc123`).
    #[serde(default)]
    pub name: String,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// MIME type.
    #[serde(default)]
    pub mime_type: String,
    /// Size in bytes.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// State of the file.
    #[serde(default)]
    pub state: Option<FileState>,
    /// URI for downloading the file.
    #[serde(default)]
    pub uri: Option<String>,
    /// SHA256 hash of the file.
    #[serde(default)]
    pub sha256_hash: Option<String>,
    /// Error details if state is Failed.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// Configuration for file upload.
#[derive(Debug, Clone)]
pub struct UploadFileConfig {
    /// Display name for the uploaded file.
    pub display_name: Option<String>,
    /// MIME type of the file.
    pub mime_type: String,
}

/// Response from listFiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilesResponse {
    /// List of files.
    #[serde(default)]
    pub files: Vec<File>,
    /// Pagination token for the next page.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Errors from the Files API.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl Client {
    /// List files.
    pub async fn list_files(&self) -> Result<ListFilesResponse, FilesError> {
        let url = self.rest_url(ServiceEndpoint::Files);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        // Handle empty response (no files)
        if json.is_null() {
            return Ok(ListFilesResponse {
                files: vec![],
                next_page_token: None,
            });
        }
        Ok(serde_json::from_value(json)?)
    }

    /// Get a file by name.
    pub async fn get_file(&self, name: &str) -> Result<File, FilesError> {
        let base_url = self.rest_url(ServiceEndpoint::Files);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Delete a file by name.
    pub async fn delete_file(&self, name: &str) -> Result<(), FilesError> {
        let base_url = self.rest_url(ServiceEndpoint::Files);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;
        self.http_client().delete(&url, headers).await?;
        Ok(())
    }

    /// Upload a file from a byte buffer.
    pub async fn upload_file(
        &self,
        data: Vec<u8>,
        config: UploadFileConfig,
    ) -> Result<File, FilesError> {
        let url = self.rest_url(ServiceEndpoint::Files);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;

        let mut body = serde_json::json!({
            "file": {
                "mimeType": config.mime_type,
            }
        });
        if let Some(name) = config.display_name {
            body["file"]["displayName"] = serde_json::Value::String(name);
        }

        // For upload, we POST metadata + inline data
        body["file"]["inlineData"] = serde_json::json!({
            "mimeType": config.mime_type,
            "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data),
        });

        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Download a file's content by name.
    pub async fn download_file(&self, name: &str) -> Result<Vec<u8>, FilesError> {
        let base_url = self.rest_url(ServiceEndpoint::Files);
        let url = format!("{base_url}/{name}:download");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;

        // The response contains base64-encoded data
        if let Some(data) = json.get("data").and_then(|v| v.as_str()) {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                .map_err(|e| FilesError::Auth(format!("Base64 decode error: {e}")))?;
            Ok(bytes)
        } else {
            // Return raw JSON as bytes if no data field
            Ok(json.to_string().into_bytes())
        }
    }

    /// Register external files by URI (Vertex AI only).
    pub async fn register_files(
        &self,
        sources: Vec<FileSource>,
    ) -> Result<Vec<File>, FilesError> {
        let url = self.rest_url(ServiceEndpoint::Files);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| FilesError::Auth(e.to_string()))?;

        let mut files = Vec::new();
        for source in sources {
            let body = serde_json::json!({
                "file": {
                    "uri": source.file_uri,
                    "mimeType": source.mime_type,
                }
            });
            let json = self.http_client().post_json(&url, headers.clone(), &body).await?;
            files.push(serde_json::from_value(json)?);
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file() {
        let json = serde_json::json!({
            "name": "files/abc123",
            "displayName": "test.txt",
            "mimeType": "text/plain",
            "sizeBytes": 1024,
            "state": "ACTIVE",
            "uri": "https://example.com/file"
        });
        let file: File = serde_json::from_value(json).unwrap();
        assert_eq!(file.name, "files/abc123");
        assert_eq!(file.display_name, "test.txt");
        assert_eq!(file.mime_type, "text/plain");
        assert_eq!(file.size_bytes, Some(1024));
        assert_eq!(file.state, Some(FileState::Active));
    }

    #[test]
    fn parse_list_files_response() {
        let json = serde_json::json!({
            "files": [
                {
                    "name": "files/a",
                    "displayName": "a.txt",
                    "mimeType": "text/plain"
                },
                {
                    "name": "files/b",
                    "displayName": "b.pdf",
                    "mimeType": "application/pdf"
                }
            ],
            "nextPageToken": "page2"
        });
        let resp: ListFilesResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.files.len(), 2);
        assert_eq!(resp.next_page_token, Some("page2".to_string()));
    }

    #[test]
    fn file_state_serialization() {
        assert_eq!(
            serde_json::to_value(FileState::Processing).unwrap(),
            "PROCESSING"
        );
        assert_eq!(
            serde_json::to_value(FileState::Active).unwrap(),
            "ACTIVE"
        );
        assert_eq!(
            serde_json::to_value(FileState::Failed).unwrap(),
            "FAILED"
        );
    }

    #[test]
    fn file_state_deserialization() {
        let state: FileState = serde_json::from_str("\"ACTIVE\"").unwrap();
        assert_eq!(state, FileState::Active);
    }

    #[test]
    fn file_source_serialization() {
        let source = FileSource {
            file_uri: "gs://bucket/file.txt".to_string(),
            mime_type: "text/plain".to_string(),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["fileUri"], "gs://bucket/file.txt");
        assert_eq!(json["mimeType"], "text/plain");
    }

    #[test]
    fn empty_list_response() {
        let json = serde_json::json!({"files": []});
        let resp: ListFilesResponse = serde_json::from_value(json).unwrap();
        assert!(resp.files.is_empty());
        assert!(resp.next_page_token.is_none());
    }

    #[test]
    fn file_with_error() {
        let json = serde_json::json!({
            "name": "files/bad",
            "displayName": "bad.txt",
            "mimeType": "text/plain",
            "state": "FAILED",
            "error": {"code": 400, "message": "Invalid file"}
        });
        let file: File = serde_json::from_value(json).unwrap();
        assert_eq!(file.state, Some(FileState::Failed));
        assert!(file.error.is_some());
    }
}
