//! OTLP metric export to Google Cloud (feature `otel-gcp`).
//!
//! Metrics go to the Telemetry API (`telemetry.googleapis.com`) over OTLP
//! gRPC, authenticated with Application Default Credentials, and land in
//! Cloud Monitoring as `prometheus.googleapis.com/...` metrics on the
//! `prometheus_target` resource. That resource needs a project, a location
//! (a real region or zone; `global` is rejected) and an instance, so the
//! exporter's resource carries `gcp.project_id`, `location` or
//! `cloud.region`, and `service.instance.id`. See
//! <https://docs.cloud.google.com/stackdriver/docs/reference/telemetry/v1.metrics>.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use gcloud_sdk::tonic;
use gcloud_sdk::{GoogleAuthHeaders, GoogleEnvironment, HeaderMap};
use opentelemetry::{Key, KeyValue};
use opentelemetry_sdk::Resource;

/// The Telemetry API's global endpoint.
pub(crate) const TELEMETRY_ENDPOINT: &str = "https://telemetry.googleapis.com";

/// How often the cached auth headers are re-read from the credentials. The
/// credentials refresh the token themselves, minutes before it expires; this
/// only picks up the new one, so it costs no request while the token holds.
const AUTH_POLL: Duration = Duration::from_secs(30);

/// How long to wait for the metadata server before treating the process as
/// running outside Google Cloud.
const METADATA_TIMEOUT: Duration = Duration::from_secs(2);

/// Resource attributes Cloud Monitoring reads the `location` label from, in
/// its order of preference.
const LOCATION_KEYS: [&str; 3] = ["location", "cloud.availability_zone", "cloud.region"];

/// Resource attributes Cloud Monitoring reads the `instance` label from.
const INSTANCE_KEYS: [&str; 5] = [
    "instance",
    "service.instance.id",
    "faas.instance",
    "k8s.pod.name",
    "host.id",
];

/// Why Google Cloud metric export could not be set up.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GcpExportError {
    /// No project: none configured, none in the environment, the ADC file
    /// or the metadata server.
    #[error(
        "no Google Cloud project for metric export: set `otel_gcp_project` \
         or GOOGLE_CLOUD_PROJECT"
    )]
    NoProject,
    /// No region or zone: Cloud Monitoring stores OTLP metrics by location
    /// and rejects `global`.
    #[error(
        "no Google Cloud region for metric export: set \
         OTEL_RESOURCE_ATTRIBUTES=cloud.region=<region> (for example \
         us-central1); on Cloud Run, GCE and GKE it is read from the \
         metadata server"
    )]
    NoLocation,
    /// The Application Default Credentials could not be loaded.
    #[error("Google Cloud credentials for metric export: {0}")]
    Credentials(String),
    /// The channel to the Telemetry API could not be opened.
    #[error("Telemetry API channel: {0}")]
    Channel(String),
}

/// A tonic interceptor that adds the current ADC headers (`authorization`,
/// and `x-goog-user-project` when the credentials carry a quota project) to
/// every export request. Interceptors are synchronous, so the headers are
/// read from a cache that [`AuthRefresh`] keeps current.
#[derive(Clone)]
pub(crate) struct AuthInterceptor {
    headers: Arc<RwLock<Arc<HeaderMap>>>,
}

impl AuthInterceptor {
    fn new(headers: Arc<HeaderMap>) -> Self {
        Self {
            headers: Arc::new(RwLock::new(headers)),
        }
    }

    fn set(&self, headers: Arc<HeaderMap>) {
        *self.headers.write().unwrap_or_else(PoisonError::into_inner) = headers;
    }
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let auth = Arc::clone(&self.headers.read().unwrap_or_else(PoisonError::into_inner));
        let mut headers = std::mem::take(request.metadata_mut()).into_headers();
        for (name, value) in auth.iter() {
            headers.insert(name.clone(), value.clone());
        }
        *request.metadata_mut() = tonic::metadata::MetadataMap::from_headers(headers);
        Ok(request)
    }
}

/// Stops the header refresh task when the telemetry guard is dropped.
pub(crate) struct AuthRefresh(tokio::task::AbortHandle);

impl Drop for AuthRefresh {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Load ADC, read its headers once (so a missing credential fails here,
/// not at the first export), and start the task that keeps them current.
pub(crate) async fn auth() -> Result<(AuthInterceptor, AuthRefresh), GcpExportError> {
    let credentials = GoogleAuthHeaders::from_adc()
        .await
        .map_err(|e| GcpExportError::Credentials(e.to_string()))?;
    let first = credentials
        .headers()
        .await
        .map_err(|e| GcpExportError::Credentials(e.to_string()))?;
    let interceptor = AuthInterceptor::new(first);
    let cache = interceptor.clone();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(AUTH_POLL);
        tick.tick().await;
        loop {
            tick.tick().await;
            match credentials.headers().await {
                Ok(headers) => cache.set(headers),
                // Keep the last headers; the next poll retries, and an
                // expired token shows up as a failed export.
                Err(e) => tracing::warn!(error = %e, "refreshing Google Cloud credentials failed"),
            }
        }
    });
    Ok((interceptor, AuthRefresh(task.abort_handle())))
}

/// A TLS channel to the Telemetry API.
pub(crate) async fn channel() -> Result<tonic::transport::Channel, GcpExportError> {
    GoogleEnvironment::init_google_services_channel(TELEMETRY_ENDPOINT)
        .await
        .map_err(|e| GcpExportError::Channel(e.to_string()))
}

/// The configured project, else the one the environment names.
pub(crate) async fn project(configured: Option<&str>) -> Result<String, GcpExportError> {
    match configured {
        Some(project) => Ok(project.to_string()),
        None => GoogleEnvironment::detect_google_project_id()
            .await
            .ok_or(GcpExportError::NoProject),
    }
}

/// The region (Cloud Run) or zone (GCE, GKE) the metadata server reports,
/// or `None` outside Google Cloud.
pub(crate) async fn metadata_location() -> Option<String> {
    let host = std::env::var("GCE_METADATA_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "metadata.google.internal".to_string());
    let client = reqwest::Client::builder()
        .timeout(METADATA_TIMEOUT)
        .build()
        .ok()?;
    for path in ["instance/region", "instance/zone"] {
        let url = format!("http://{host}/computeMetadata/v1/{path}");
        let Ok(response) = client
            .get(&url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
        else {
            // Unreachable: not on Google Cloud, so the zone will not answer either.
            return None;
        };
        if !response.status().is_success() {
            continue;
        }
        if let Some(location) = response.text().await.ok().as_deref().and_then(last_segment) {
            return Some(location);
        }
    }
    None
}

/// `projects/123/regions/us-central1` → `us-central1`.
fn last_segment(path: &str) -> Option<String> {
    path.trim()
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn has_any(resource: &Resource, keys: &[&'static str]) -> bool {
    keys.iter().any(|k| {
        resource
            .get(&Key::from_static_str(k))
            .is_some_and(|v| !v.as_str().trim().is_empty())
    })
}

/// Whether `resource` already names a location Cloud Monitoring accepts.
pub(crate) fn has_location(resource: &Resource) -> bool {
    has_any(resource, &LOCATION_KEYS)
}

/// The metric exporter's resource: the attributes from
/// `OTEL_RESOURCE_ATTRIBUTES` (`env`), the service name, the project, and
/// defaults for the labels Cloud Monitoring requires. Attributes the
/// environment already sets are kept.
pub(crate) fn metrics_resource(
    env: &Resource,
    service_name: &str,
    project: &str,
    location: Option<String>,
) -> Result<Resource, GcpExportError> {
    let mut defaults = vec![KeyValue::new("gcp.project_id", project.to_string())];
    if !has_location(env) {
        let location = location.ok_or(GcpExportError::NoLocation)?;
        defaults.push(KeyValue::new("cloud.region", location));
    }
    if !has_any(env, &INSTANCE_KEYS) {
        defaults.push(KeyValue::new(
            "service.instance.id",
            uuid::Uuid::new_v4().to_string(),
        ));
    }
    // A blank attribute names nothing, so it must not hide a default.
    let explicit = env
        .iter()
        .filter(|(_, v)| !v.as_str().trim().is_empty())
        .map(|(k, v)| KeyValue::new(k.clone(), v.clone()));
    Ok(Resource::builder_empty()
        .with_attributes(defaults)
        .with_attributes(explicit)
        .with_service_name(service_name.to_string())
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::service::Interceptor as _;

    fn resource(attrs: &[(&'static str, &'static str)]) -> Resource {
        Resource::builder_empty()
            .with_attributes(attrs.iter().map(|(k, v)| KeyValue::new(*k, *v)))
            .build()
    }

    fn get(resource: &Resource, key: &'static str) -> Option<String> {
        resource
            .get(&Key::from_static_str(key))
            .map(|v| v.as_str().into_owned())
    }

    #[test]
    fn resource_fills_the_labels_cloud_monitoring_requires() {
        let r = metrics_resource(&resource(&[]), "svc", "proj", Some("us-central1".into()))
            .expect("resource");
        assert_eq!(get(&r, "gcp.project_id").as_deref(), Some("proj"));
        assert_eq!(get(&r, "cloud.region").as_deref(), Some("us-central1"));
        assert_eq!(get(&r, "service.name").as_deref(), Some("svc"));
        let instance = get(&r, "service.instance.id").expect("instance id");
        assert!(uuid::Uuid::parse_str(&instance).is_ok(), "{instance}");
    }

    #[test]
    fn resource_keeps_what_the_environment_sets() {
        let env = resource(&[
            ("location", "europe-west1-b"),
            ("k8s.pod.name", "pod-7"),
            ("gcp.project_id", "from-env"),
        ]);
        let r = metrics_resource(&env, "svc", "proj", None).expect("resource");
        assert_eq!(get(&r, "location").as_deref(), Some("europe-west1-b"));
        assert_eq!(
            get(&r, "cloud.region"),
            None,
            "no default over an explicit location"
        );
        assert_eq!(
            get(&r, "service.instance.id"),
            None,
            "the pod name is the instance"
        );
        assert_eq!(get(&r, "gcp.project_id").as_deref(), Some("from-env"));
    }

    #[test]
    fn resource_without_any_location_is_an_error() {
        let err = metrics_resource(&resource(&[]), "svc", "proj", None).unwrap_err();
        assert!(matches!(err, GcpExportError::NoLocation), "{err}");
        assert!(err.to_string().contains("cloud.region"), "{err}");
    }

    #[test]
    fn blank_location_does_not_count() {
        let env = resource(&[("cloud.region", " ")]);
        assert!(!has_location(&env));
        let r =
            metrics_resource(&env, "svc", "proj", Some("asia-south1".into())).expect("resource");
        assert_eq!(get(&r, "cloud.region").as_deref(), Some("asia-south1"));
    }

    #[test]
    fn metadata_paths_reduce_to_their_last_segment() {
        assert_eq!(
            last_segment("projects/123/regions/us-central1\n").as_deref(),
            Some("us-central1")
        );
        assert_eq!(
            last_segment("projects/123/zones/us-east1-b").as_deref(),
            Some("us-east1-b")
        );
        assert_eq!(last_segment("projects/123/zones/"), None);
    }

    #[test]
    fn interceptor_adds_the_current_headers_and_keeps_the_request_metadata() {
        let headers = |token: &str| {
            let mut h = HeaderMap::new();
            h.insert(
                "authorization",
                format!("Bearer {token}").parse().expect("value"),
            );
            h.insert("x-goog-user-project", "quota".parse().expect("value"));
            Arc::new(h)
        };
        let mut interceptor = AuthInterceptor::new(headers("one"));
        let mut request = tonic::Request::new(());
        request
            .metadata_mut()
            .insert("x-custom", "kept".parse().expect("value"));
        let out = interceptor.call(request).expect("intercepted");
        let md = out.metadata();
        assert_eq!(
            md.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer one")
        );
        assert_eq!(
            md.get("x-goog-user-project").and_then(|v| v.to_str().ok()),
            Some("quota")
        );
        assert_eq!(
            md.get("x-custom").and_then(|v| v.to_str().ok()),
            Some("kept")
        );

        // A refreshed token replaces the old one rather than adding a second.
        interceptor.set(headers("two"));
        let out = interceptor
            .call(tonic::Request::new(()))
            .expect("intercepted");
        let values: Vec<_> = out
            .metadata()
            .get_all("authorization")
            .iter()
            .map(|v| v.to_str().unwrap_or("<not ascii>").to_string())
            .collect();
        assert_eq!(values, vec!["Bearer two".to_string()]);
    }
}
