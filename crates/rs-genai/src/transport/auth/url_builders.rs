//! Internal URL builder helpers shared by auth implementations.

use super::ServiceEndpoint;
use crate::protocol::types::GeminiModel;

/// Build a Google AI REST URL with API key as query parameter.
pub(crate) fn build_google_ai_rest_url(
    base: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
    api_key: &str,
) -> String {
    let path = build_rest_path(endpoint, model);
    format!("{base}/{path}?key={api_key}")
}

/// Build a Google AI REST URL without an API key (for token-based auth).
pub(crate) fn build_google_ai_rest_url_no_key(
    base: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
) -> String {
    let path = build_rest_path(endpoint, model);
    format!("{base}/{path}")
}

/// Build a Vertex AI REST URL.
pub(crate) fn build_vertex_rest_url(
    host: &str,
    project: &str,
    location: &str,
    endpoint: ServiceEndpoint,
    model: Option<&GeminiModel>,
) -> String {
    let base = format!("https://{host}/v1beta1/projects/{project}/locations/{location}",);
    match endpoint {
        ServiceEndpoint::LiveWs => {
            // LiveWs should use ws_url(), not rest_url()
            panic!("Use ws_url() for LiveWs endpoints")
        }
        ServiceEndpoint::Files => {
            // Vertex AI files are at project/location level
            format!("{base}/files")
        }
        ServiceEndpoint::CachedContents => {
            format!("{base}/cachedContents")
        }
        ServiceEndpoint::TuningJobs => {
            format!("{base}/tuningJobs")
        }
        ServiceEndpoint::BatchJobs => {
            format!("{base}/batchPredictionJobs")
        }
        ServiceEndpoint::ListModels => {
            format!("{base}/publishers/google/models")
        }
        endpoint => {
            // Model-scoped endpoints
            let model_id = model
                .map(|m| m.to_string().trim_start_matches("models/").to_string())
                .unwrap_or_default();
            let publisher_model = format!("publishers/google/models/{model_id}");
            if let Some(method) = endpoint.model_method() {
                format!("{base}/{publisher_model}:{method}")
            } else {
                format!("{base}/{publisher_model}")
            }
        }
    }
}

/// Build the REST path segment for Google AI (mldev) endpoints.
pub(crate) fn build_rest_path(endpoint: ServiceEndpoint, model: Option<&GeminiModel>) -> String {
    match endpoint {
        ServiceEndpoint::LiveWs => {
            panic!("Use ws_url() for LiveWs endpoints")
        }
        ServiceEndpoint::Files => "files".to_string(),
        ServiceEndpoint::CachedContents => "cachedContents".to_string(),
        ServiceEndpoint::TuningJobs => "tunedModels".to_string(),
        ServiceEndpoint::BatchJobs => "batchJobs".to_string(),
        ServiceEndpoint::ListModels => "models".to_string(),
        endpoint => {
            let raw = model.map(|m| m.to_string()).unwrap_or_default();
            let model_str = if raw.starts_with("models/") {
                raw
            } else {
                format!("models/{raw}")
            };
            if let Some(method) = endpoint.model_method() {
                format!("{model_str}:{method}")
            } else {
                model_str
            }
        }
    }
}
