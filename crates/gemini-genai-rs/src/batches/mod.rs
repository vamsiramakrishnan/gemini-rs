//! Batches API — create, list, get, cancel, delete batch prediction jobs.
//!
//! Feature-gated behind `batches`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::transport::auth::ServiceEndpoint;

/// State of a batch job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchJobState {
    StateUnspecified,
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
}

/// Source configuration for a batch job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchJobSource {
    /// GCS URI of the input file (JSONL).
    pub gcs_uri: Option<String>,
    /// BigQuery input table.
    pub bigquery_source: Option<String>,
    /// Format of the input (e.g., "bigquery", "jsonl").
    #[serde(default)]
    pub format: Option<String>,
}

/// Destination configuration for a batch job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchJobDestination {
    /// GCS URI prefix for output.
    pub gcs_uri: Option<String>,
    /// BigQuery output table.
    pub bigquery_destination: Option<String>,
}

/// A batch prediction job resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchJob {
    /// Resource name.
    #[serde(default)]
    pub name: String,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Model used for batch prediction.
    #[serde(default)]
    pub model: Option<String>,
    /// State of the batch job.
    #[serde(default)]
    pub state: Option<BatchJobState>,
    /// Input source.
    #[serde(default)]
    pub source: Option<BatchJobSource>,
    /// Output destination.
    #[serde(default)]
    pub destination: Option<BatchJobDestination>,
    /// Creation time (RFC3339).
    #[serde(default)]
    pub create_time: Option<String>,
    /// Update time (RFC3339).
    #[serde(default)]
    pub update_time: Option<String>,
    /// Completion time (RFC3339).
    #[serde(default)]
    pub completion_time: Option<String>,
    /// Error details if state is Failed.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// Configuration for creating a batch job.
#[derive(Debug, Clone)]
pub struct CreateBatchJobConfig {
    /// Model for batch prediction.
    pub model: String,
    /// Display name.
    pub display_name: Option<String>,
    /// Input source configuration.
    pub source: BatchJobSource,
    /// Output destination configuration.
    pub destination: BatchJobDestination,
}

/// Response from listBatchJobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBatchJobsResponse {
    /// List of batch jobs.
    #[serde(default)]
    pub batch_jobs: Vec<BatchJob>,
    /// Pagination token for the next page.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Errors from the Batches API.
#[derive(Debug, thiserror::Error)]
pub enum BatchesError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// List batch jobs.
    pub async fn list_batch_jobs(&self) -> Result<ListBatchJobsResponse, BatchesError> {
        let url = self.rest_url(ServiceEndpoint::BatchJobs);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| BatchesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        if json.is_null() {
            return Ok(ListBatchJobsResponse {
                batch_jobs: vec![],
                next_page_token: None,
            });
        }
        Ok(serde_json::from_value(json)?)
    }

    /// Create a batch prediction job.
    pub async fn create_batch_job(
        &self,
        config: CreateBatchJobConfig,
    ) -> Result<BatchJob, BatchesError> {
        let url = self.rest_url(ServiceEndpoint::BatchJobs);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| BatchesError::Auth(e.to_string()))?;

        let mut body = serde_json::json!({
            "model": config.model,
            "source": config.source,
            "destination": config.destination,
        });

        if let Some(name) = config.display_name {
            body["displayName"] = serde_json::Value::String(name);
        }

        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Create a batch embeddings job.
    pub async fn create_batch_embeddings(
        &self,
        config: CreateBatchJobConfig,
    ) -> Result<BatchJob, BatchesError> {
        // Same endpoint — the model determines the operation type
        self.create_batch_job(config).await
    }

    /// Get a batch job by name.
    pub async fn get_batch_job(&self, name: &str) -> Result<BatchJob, BatchesError> {
        let base_url = self.rest_url(ServiceEndpoint::BatchJobs);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| BatchesError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Cancel a batch job by name.
    pub async fn cancel_batch_job(&self, name: &str) -> Result<(), BatchesError> {
        let base_url = self.rest_url(ServiceEndpoint::BatchJobs);
        let url = format!("{base_url}/{name}:cancel");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| BatchesError::Auth(e.to_string()))?;
        self.http_client()
            .post_json(&url, headers, &serde_json::json!({}))
            .await?;
        Ok(())
    }

    /// Delete a batch job by name.
    pub async fn delete_batch_job(&self, name: &str) -> Result<(), BatchesError> {
        let base_url = self.rest_url(ServiceEndpoint::BatchJobs);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| BatchesError::Auth(e.to_string()))?;
        self.http_client().delete(&url, headers).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_batch_job() {
        let json = serde_json::json!({
            "name": "batchJobs/123",
            "displayName": "My Batch",
            "model": "models/gemini-1.5-flash",
            "state": "RUNNING",
            "createTime": "2026-03-01T00:00:00Z"
        });
        let job: BatchJob = serde_json::from_value(json).unwrap();
        assert_eq!(job.name, "batchJobs/123");
        assert_eq!(job.state, Some(BatchJobState::Running));
    }

    #[test]
    fn parse_list_batch_jobs_response() {
        let json = serde_json::json!({
            "batchJobs": [
                {"name": "batchJobs/1", "state": "PENDING"},
                {"name": "batchJobs/2", "state": "SUCCEEDED"}
            ],
            "nextPageToken": "page2"
        });
        let resp: ListBatchJobsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.batch_jobs.len(), 2);
        assert_eq!(resp.next_page_token, Some("page2".to_string()));
    }

    #[test]
    fn batch_job_state_serialization() {
        assert_eq!(
            serde_json::to_value(BatchJobState::Running).unwrap(),
            "RUNNING"
        );
        assert_eq!(
            serde_json::to_value(BatchJobState::Succeeded).unwrap(),
            "SUCCEEDED"
        );
        assert_eq!(
            serde_json::to_value(BatchJobState::Cancelled).unwrap(),
            "CANCELLED"
        );
    }

    #[test]
    fn batch_source_serialization() {
        let source = BatchJobSource {
            gcs_uri: Some("gs://bucket/input.jsonl".to_string()),
            bigquery_source: None,
            format: Some("jsonl".to_string()),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["gcsUri"], "gs://bucket/input.jsonl");
    }

    #[test]
    fn batch_destination_serialization() {
        let dest = BatchJobDestination {
            gcs_uri: Some("gs://bucket/output/".to_string()),
            bigquery_destination: None,
        };
        let json = serde_json::to_value(&dest).unwrap();
        assert_eq!(json["gcsUri"], "gs://bucket/output/");
    }

    #[test]
    fn empty_list_response() {
        let json = serde_json::json!({"batchJobs": []});
        let resp: ListBatchJobsResponse = serde_json::from_value(json).unwrap();
        assert!(resp.batch_jobs.is_empty());
    }

    #[test]
    fn batch_job_with_error() {
        let json = serde_json::json!({
            "name": "batchJobs/bad",
            "state": "FAILED",
            "error": {"code": 400, "message": "Invalid input"}
        });
        let job: BatchJob = serde_json::from_value(json).unwrap();
        assert_eq!(job.state, Some(BatchJobState::Failed));
        assert!(job.error.is_some());
    }
}
