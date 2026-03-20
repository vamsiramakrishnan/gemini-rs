//! Tunings API — create, list, get, cancel tuning jobs.
//!
//! Feature-gated behind `tunings`.

use serde::{Deserialize, Serialize};

use crate::client::http::HttpError;
use crate::client::Client;
use crate::transport::auth::ServiceEndpoint;

/// State of a tuning job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TuningJobState {
    StateUnspecified,
    Creating,
    Active,
    Failed,
}

/// Supervised tuning specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisedTuningSpec {
    /// Training dataset configuration.
    pub training_dataset_uri: Option<String>,
    /// Validation dataset URI.
    #[serde(default)]
    pub validation_dataset_uri: Option<String>,
    /// Hyperparameters.
    #[serde(default)]
    pub hyper_parameters: Option<TuningHyperParameters>,
}

/// Tuning hyperparameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuningHyperParameters {
    /// Number of training epochs.
    #[serde(default)]
    pub epoch_count: Option<u32>,
    /// Batch size.
    #[serde(default)]
    pub batch_size: Option<u32>,
    /// Learning rate.
    #[serde(default)]
    pub learning_rate: Option<f64>,
}

/// A tuning job resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuningJob {
    /// Resource name.
    #[serde(default)]
    pub name: String,
    /// Base model being tuned.
    #[serde(default)]
    pub base_model: Option<String>,
    /// Tuned model name (output).
    #[serde(default)]
    pub tuned_model: Option<String>,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// State of the tuning job.
    #[serde(default)]
    pub state: Option<TuningJobState>,
    /// Supervised tuning spec.
    #[serde(default)]
    pub supervised_tuning_spec: Option<SupervisedTuningSpec>,
    /// Creation time (RFC3339).
    #[serde(default)]
    pub create_time: Option<String>,
    /// Update time (RFC3339).
    #[serde(default)]
    pub update_time: Option<String>,
    /// Error details if state is Failed.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// Configuration for creating a tuning job.
#[derive(Debug, Clone)]
pub struct CreateTuningJobConfig {
    /// Base model to tune.
    pub base_model: String,
    /// Display name.
    pub display_name: Option<String>,
    /// Supervised tuning spec.
    pub supervised_tuning_spec: SupervisedTuningSpec,
}

/// Response from listTuningJobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTuningJobsResponse {
    /// List of tuning jobs.
    #[serde(default)]
    pub tuning_jobs: Vec<TuningJob>,
    /// Pagination token for the next page.
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Errors from the Tunings API.
#[derive(Debug, thiserror::Error)]
pub enum TuningsError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Client {
    /// List tuning jobs.
    pub async fn list_tuning_jobs(&self) -> Result<ListTuningJobsResponse, TuningsError> {
        let url = self.rest_url(ServiceEndpoint::TuningJobs);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| TuningsError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        if json.is_null() {
            return Ok(ListTuningJobsResponse {
                tuning_jobs: vec![],
                next_page_token: None,
            });
        }
        Ok(serde_json::from_value(json)?)
    }

    /// Get a tuning job by name.
    pub async fn get_tuning_job(&self, name: &str) -> Result<TuningJob, TuningsError> {
        let base_url = self.rest_url(ServiceEndpoint::TuningJobs);
        let url = format!("{base_url}/{name}");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| TuningsError::Auth(e.to_string()))?;
        let json = self.http_client().get_json(&url, headers).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Create a new tuning job.
    pub async fn create_tuning_job(
        &self,
        config: CreateTuningJobConfig,
    ) -> Result<TuningJob, TuningsError> {
        let url = self.rest_url(ServiceEndpoint::TuningJobs);
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| TuningsError::Auth(e.to_string()))?;

        let mut body = serde_json::json!({
            "baseModel": config.base_model,
            "supervisedTuningSpec": config.supervised_tuning_spec,
        });

        if let Some(name) = config.display_name {
            body["displayName"] = serde_json::Value::String(name);
        }

        let json = self.http_client().post_json(&url, headers, &body).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Cancel a tuning job by name.
    pub async fn cancel_tuning_job(&self, name: &str) -> Result<(), TuningsError> {
        let base_url = self.rest_url(ServiceEndpoint::TuningJobs);
        let url = format!("{base_url}/{name}:cancel");
        let headers = self
            .auth_headers()
            .await
            .map_err(|e| TuningsError::Auth(e.to_string()))?;
        self.http_client()
            .post_json(&url, headers, &serde_json::json!({}))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tuning_job() {
        let json = serde_json::json!({
            "name": "tuningJobs/123",
            "baseModel": "models/gemini-1.5-flash",
            "tunedModel": "tunedModels/my-model",
            "displayName": "My Tuning",
            "state": "ACTIVE",
            "createTime": "2026-03-01T00:00:00Z"
        });
        let job: TuningJob = serde_json::from_value(json).unwrap();
        assert_eq!(job.name, "tuningJobs/123");
        assert_eq!(job.state, Some(TuningJobState::Active));
        assert_eq!(job.tuned_model, Some("tunedModels/my-model".to_string()));
    }

    #[test]
    fn parse_list_tuning_jobs_response() {
        let json = serde_json::json!({
            "tuningJobs": [
                {"name": "tuningJobs/1", "state": "CREATING"},
                {"name": "tuningJobs/2", "state": "ACTIVE"}
            ]
        });
        let resp: ListTuningJobsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.tuning_jobs.len(), 2);
        assert_eq!(resp.tuning_jobs[0].state, Some(TuningJobState::Creating));
    }

    #[test]
    fn tuning_job_state_serialization() {
        assert_eq!(
            serde_json::to_value(TuningJobState::Active).unwrap(),
            "ACTIVE"
        );
        assert_eq!(
            serde_json::to_value(TuningJobState::Creating).unwrap(),
            "CREATING"
        );
        assert_eq!(
            serde_json::to_value(TuningJobState::Failed).unwrap(),
            "FAILED"
        );
    }

    #[test]
    fn supervised_tuning_spec_serialization() {
        let spec = SupervisedTuningSpec {
            training_dataset_uri: Some("gs://bucket/train.jsonl".to_string()),
            validation_dataset_uri: None,
            hyper_parameters: Some(TuningHyperParameters {
                epoch_count: Some(5),
                batch_size: Some(32),
                learning_rate: Some(0.001),
            }),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["trainingDatasetUri"], "gs://bucket/train.jsonl");
        assert_eq!(json["hyperParameters"]["epochCount"], 5);
    }

    #[test]
    fn empty_list_response() {
        let json = serde_json::json!({"tuningJobs": []});
        let resp: ListTuningJobsResponse = serde_json::from_value(json).unwrap();
        assert!(resp.tuning_jobs.is_empty());
    }
}
