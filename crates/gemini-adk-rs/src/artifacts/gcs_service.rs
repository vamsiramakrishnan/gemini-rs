//! GCS-backed artifact service.
//!
//! Feature-gated behind `gcs-artifacts`. Implements [`ArtifactService`] against
//! Google Cloud Storage using the JSON HTTP API
//! (`https://storage.googleapis.com/storage/v1/...` for metadata operations and
//! `https://storage.googleapis.com/upload/...` for uploads). Authentication is
//! via an OAuth2 bearer access token, supplied the same way as the rest of this
//! crate's REST-backed services (a static token or a refresher closure).
//!
//! # Blob naming scheme
//!
//! This mirrors the ADK Python `GcsArtifactService`. The blob name depends on
//! whether the filename carries a user namespace:
//!
//! - User-scoped (filename starts with `"user:"`):
//!   `{app_name}/{user_id}/user/{filename}/{version}`
//! - Session-scoped (regular filenames):
//!   `{app_name}/{user_id}/{session_id}/{filename}/{version}`
//!
//! Versions are sequential integers per artifact. The first version is `0`
//! (matching ADK on the wire); the next version is `max(existing) + 1`.
//!
//! # Trait mapping
//!
//! The crate's [`ArtifactService`] trait is session-scoped and does not carry a
//! `user_id` argument, so a fixed `user_id` is held on the service (defaults to
//! `"user"`, override with [`GcsArtifactService::user_id`]). The trait exposes
//! 1-based version numbers (consistent with the in-memory and file services),
//! while the GCS wire layout follows ADK's 0-based numbering. The two are
//! mapped at the trait boundary: wire version `v` is surfaced as `v + 1`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Artifact, ArtifactError, ArtifactMetadata, ArtifactService, now_secs};

const STORAGE_BASE: &str = "https://storage.googleapis.com/storage/v1";
const UPLOAD_BASE: &str = "https://storage.googleapis.com/upload/storage/v1";

// ──────────────────────────────────────────────────────────────────────────────
// Auth provider (mirrors session::vertex_ai::TokenProvider)
// ──────────────────────────────────────────────────────────────────────────────

/// How to supply a bearer token for GCS requests.
enum TokenProvider {
    /// No token configured — requests will fail with a clear message.
    None,
    /// A static, pre-fetched bearer token string.
    Static(String),
    /// A dynamic refresher: called before every request.
    Refresher(Arc<dyn Fn() -> String + Send + Sync>),
}

impl TokenProvider {
    /// Retrieve the current token, or return an error if none is configured.
    fn get(&self) -> Result<String, ArtifactError> {
        match self {
            TokenProvider::None => Err(ArtifactError::Storage(
                "missing auth token: call .with_token() or .with_token_refresher()".into(),
            )),
            TokenProvider::Static(t) => Ok(t.clone()),
            TokenProvider::Refresher(f) => Ok(f()),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// DTO types — mirror GCS JSON shapes
// ──────────────────────────────────────────────────────────────────────────────

/// A GCS object resource (subset of fields we use).
#[derive(Debug, Deserialize)]
struct GcsObject {
    /// Object name (the full blob path within the bucket).
    name: String,
}

/// Response envelope for `objects.list`.
#[derive(Debug, Default, Deserialize)]
struct ListObjectsResponse {
    #[serde(default)]
    items: Vec<GcsObject>,
    #[serde(rename = "nextPageToken", default)]
    next_page_token: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Pure helpers (blob naming) — mirror ADK's _get_blob_prefix / _get_blob_name
// ──────────────────────────────────────────────────────────────────────────────

/// Whether the filename carries a user namespace (starts with `"user:"`).
fn file_has_user_namespace(filename: &str) -> bool {
    filename.starts_with("user:")
}

/// Construct the blob name prefix (everything up to but excluding `/{version}`).
fn blob_prefix(app_name: &str, user_id: &str, session_id: &str, filename: &str) -> String {
    if file_has_user_namespace(filename) {
        format!("{app_name}/{user_id}/user/{filename}")
    } else {
        format!("{app_name}/{user_id}/{session_id}/{filename}")
    }
}

/// Construct the full blob name including the version suffix.
fn blob_name(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    filename: &str,
    version: u64,
) -> String {
    format!(
        "{}/{}",
        blob_prefix(app_name, user_id, session_id, filename),
        version
    )
}

/// Parse the trailing `/{version}` integer from a blob name, if present.
fn version_from_blob_name(name: &str) -> Option<u64> {
    name.rsplit('/')
        .next()
        .and_then(|seg| seg.parse::<u64>().ok())
}

// ──────────────────────────────────────────────────────────────────────────────
// Service struct
// ──────────────────────────────────────────────────────────────────────────────

/// GCS-backed artifact service.
///
/// Path format (on the wire, mirroring ADK):
/// `{app_name}/{user_id}/{session_id}/{filename}/{version}` for session-scoped
/// artifacts, and `{app_name}/{user_id}/user/{filename}/{version}` for
/// user-namespaced filenames (those starting with `"user:"`).
///
/// # Quick start
///
/// ```rust,no_run
/// # use gemini_adk_rs::artifacts::GcsArtifactService;
/// let svc = GcsArtifactService::new("my-bucket", "my-app")
///     .with_token("ya29.my-access-token")
///     .user_id("alice");
/// ```
pub struct GcsArtifactService {
    bucket: String,
    app_name: String,
    user_id: String,
    client: reqwest::Client,
    token_provider: TokenProvider,
}

impl GcsArtifactService {
    /// Create a new GCS artifact service targeting the given bucket.
    ///
    /// No auth token is configured yet — call [`with_token`](Self::with_token)
    /// or [`with_token_refresher`](Self::with_token_refresher) before issuing
    /// requests, otherwise they will return
    /// `ArtifactError::Storage("missing auth token")`.
    pub fn new(bucket: impl Into<String>, app_name: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            app_name: app_name.into(),
            user_id: "user".to_string(),
            client: reqwest::Client::new(),
            token_provider: TokenProvider::None,
        }
    }

    /// Set a static bearer token (OAuth2 access token) for all requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token_provider = TokenProvider::Static(token.into());
        self
    }

    /// Set a dynamic token refresher closure.
    ///
    /// The closure is invoked before every HTTP request, allowing the caller to
    /// supply a freshly-refreshed token each time.
    pub fn with_token_refresher(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.token_provider = TokenProvider::Refresher(Arc::new(f));
        self
    }

    /// Override the user ID used in blob paths (defaults to `"user"`).
    ///
    /// The crate's [`ArtifactService`] trait is session-scoped and carries no
    /// `user_id`, so it is fixed on the service to complete ADK's
    /// `{app_name}/{user_id}/{session_id}/...` blob layout.
    pub fn user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    /// The bucket this service targets.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The application name prefix used in object paths.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    // ── Internal HTTP helpers ──────────────────────────────────────────────

    fn token(&self) -> Result<String, ArtifactError> {
        self.token_provider.get()
    }

    /// List the wire versions (0-based integers) of an artifact, in ascending
    /// order. Mirrors ADK's `_list_versions`.
    async fn list_wire_versions(
        &self,
        session_id: &str,
        filename: &str,
    ) -> Result<Vec<u64>, ArtifactError> {
        let prefix = format!(
            "{}/",
            blob_prefix(&self.app_name, &self.user_id, session_id, filename)
        );
        let mut versions: Vec<u64> = self
            .list_object_names(&prefix)
            .await?
            .into_iter()
            .filter_map(|name| version_from_blob_name(&name))
            .collect();
        versions.sort_unstable();
        Ok(versions)
    }

    /// List all object names under a prefix, paginating through results.
    async fn list_object_names(&self, prefix: &str) -> Result<Vec<String>, ArtifactError> {
        let token = self.token()?;
        let base = format!("{STORAGE_BASE}/b/{}/o", urlencode(&self.bucket));
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get(&base)
                .header("Authorization", format!("Bearer {token}"))
                .query(&[("prefix", prefix)]);
            if let Some(tok) = &page_token {
                req = req.query(&[("pageToken", tok.as_str())]);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| ArtifactError::Storage(format!("HTTP request failed: {e}")))?;
            let parsed: ListObjectsResponse = parse_json(resp).await?.unwrap_or_default();
            names.extend(parsed.items.into_iter().map(|o| o.name));

            match parsed.next_page_token {
                Some(t) if !t.is_empty() => page_token = Some(t),
                _ => break,
            }
        }
        Ok(names)
    }

    /// Upload a payload to the given blob name with the given content type.
    async fn upload_blob(
        &self,
        blob: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), ArtifactError> {
        let token = self.token()?;
        let url = format!("{UPLOAD_BASE}/b/{}/o", urlencode(&self.bucket));
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", content_type)
            .query(&[("uploadType", "media"), ("name", blob)])
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| ArtifactError::Storage(format!("HTTP request failed: {e}")))?;
        check_success(resp).await
    }

    /// Download a blob's payload and content type. Returns `Ok(None)` on 404.
    async fn download_blob(&self, blob: &str) -> Result<Option<(Vec<u8>, String)>, ArtifactError> {
        let token = self.token()?;
        let url = format!(
            "{STORAGE_BASE}/b/{}/o/{}",
            urlencode(&self.bucket),
            urlencode(blob)
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .query(&[("alt", "media")])
            .send()
            .await
            .map_err(|e| ArtifactError::Storage(format!("HTTP request failed: {e}")))?;

        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
            return Err(ArtifactError::Storage(format!(
                "GCS request failed [{status}]: {body}"
            )));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ArtifactError::Storage(format!("failed to read body: {e}")))?;
        Ok(Some((bytes.to_vec(), content_type)))
    }

    /// Delete a single blob. A 404 is treated as success (idempotent delete).
    async fn delete_blob(&self, blob: &str) -> Result<(), ArtifactError> {
        let token = self.token()?;
        let url = format!(
            "{STORAGE_BASE}/b/{}/o/{}",
            urlencode(&self.bucket),
            urlencode(blob)
        );
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| ArtifactError::Storage(format!("HTTP request failed: {e}")))?;
        let status = resp.status().as_u16();
        if status == 404 || (200..300).contains(&status) {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
        Err(ArtifactError::Storage(format!(
            "GCS request failed [{status}]: {body}"
        )))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HTTP response helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Parse a JSON response, mapping 404 to `Ok(None)` and non-2xx to an error.
async fn parse_json<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<Option<T>, ArtifactError> {
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if (200..300).contains(&status) {
        let parsed: T = resp
            .json()
            .await
            .map_err(|e| ArtifactError::Storage(format!("failed to parse response: {e}")))?;
        return Ok(Some(parsed));
    }
    let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
    Err(ArtifactError::Storage(format!(
        "GCS request failed [{status}]: {body}"
    )))
}

/// Verify a response was 2xx (for void operations like upload).
async fn check_success(resp: reqwest::Response) -> Result<(), ArtifactError> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
    Err(ArtifactError::Storage(format!(
        "GCS request failed [{status}]: {body}"
    )))
}

/// Percent-encode a path segment for use in GCS object URLs.
///
/// GCS object names may contain `/`, which must be encoded as `%2F` when the
/// object name appears as a single path segment (e.g. `objects.get`).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// ArtifactService implementation
// ──────────────────────────────────────────────────────────────────────────────

#[async_trait]
impl ArtifactService for GcsArtifactService {
    async fn save(
        &self,
        session_id: &str,
        artifact: Artifact,
    ) -> Result<ArtifactMetadata, ArtifactError> {
        let filename = &artifact.metadata.name;

        // Next wire version = max(existing) + 1, or 0 if none. (ADK semantics.)
        let versions = self.list_wire_versions(session_id, filename).await?;
        let wire_version = versions.iter().copied().max().map(|v| v + 1).unwrap_or(0);

        let blob = blob_name(
            &self.app_name,
            &self.user_id,
            session_id,
            filename,
            wire_version,
        );
        self.upload_blob(&blob, &artifact.data, &artifact.metadata.mime_type)
            .await?;

        let mut metadata = artifact.metadata;
        // Surface a 1-based version at the trait boundary for consistency with
        // the in-memory and file services.
        metadata.version = (wire_version + 1) as u32;
        metadata.updated_at = now_secs();
        if wire_version == 0 {
            metadata.created_at = metadata.updated_at;
        }
        Ok(metadata)
    }

    async fn load(&self, session_id: &str, name: &str) -> Result<Option<Artifact>, ArtifactError> {
        let versions = self.list_wire_versions(session_id, name).await?;
        let Some(latest) = versions.iter().copied().max() else {
            return Ok(None);
        };
        self.load_version(session_id, name, (latest + 1) as u32)
            .await
    }

    async fn load_version(
        &self,
        session_id: &str,
        name: &str,
        version: u32,
    ) -> Result<Option<Artifact>, ArtifactError> {
        if version == 0 {
            return Ok(None);
        }
        // Trait versions are 1-based; the wire layout is 0-based.
        let wire_version = (version - 1) as u64;
        let blob = blob_name(
            &self.app_name,
            &self.user_id,
            session_id,
            name,
            wire_version,
        );

        let Some((data, content_type)) = self.download_blob(&blob).await? else {
            return Ok(None);
        };

        let now = now_secs();
        let size = data.len();
        Ok(Some(Artifact {
            metadata: ArtifactMetadata {
                name: name.to_string(),
                mime_type: content_type,
                version,
                size,
                created_at: now,
                updated_at: now,
            },
            data,
        }))
    }

    async fn list(&self, session_id: &str) -> Result<Vec<ArtifactMetadata>, ArtifactError> {
        use std::collections::BTreeSet;

        // Session-scoped filenames.
        let session_prefix = format!("{}/{}/{}/", self.app_name, self.user_id, session_id);
        // User-namespaced filenames (no session_id).
        let user_prefix = format!("{}/{}/user/", self.app_name, self.user_id);

        let mut filenames: BTreeSet<String> = BTreeSet::new();

        for name in self.list_object_names(&session_prefix).await? {
            if let Some(rest) = name.strip_prefix(&session_prefix) {
                // rest is `{filename}/{version}` (filename may contain slashes).
                if let Some(idx) = rest.rfind('/') {
                    filenames.insert(rest[..idx].to_string());
                }
            }
        }
        for name in self.list_object_names(&user_prefix).await? {
            if let Some(rest) = name.strip_prefix(&user_prefix)
                && let Some(idx) = rest.rfind('/')
            {
                // Re-attach the `user:` prefix so callers can round-trip the
                // returned name back into load/load_version. The stored blob
                // path keeps the literal `user:` filename segment.
                filenames.insert(rest[..idx].to_string());
            }
        }

        // Resolve latest-version metadata for each filename.
        let mut result = Vec::with_capacity(filenames.len());
        for filename in filenames {
            if let Some(artifact) = self.load(session_id, &filename).await? {
                result.push(artifact.metadata);
            }
        }
        Ok(result)
    }

    async fn delete(&self, session_id: &str, name: &str) -> Result<(), ArtifactError> {
        let versions = self.list_wire_versions(session_id, name).await?;
        for wire_version in versions {
            let blob = blob_name(
                &self.app_name,
                &self.user_id,
                session_id,
                name,
                wire_version,
            );
            self.delete_blob(&blob).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_construct() {
        let svc = GcsArtifactService::new("my-bucket", "my-app");
        assert_eq!(svc.bucket(), "my-bucket");
        assert_eq!(svc.app_name(), "my-app");
    }

    #[test]
    fn default_user_id_is_user() {
        let svc = GcsArtifactService::new("b", "a");
        assert_eq!(svc.user_id, "user");
    }

    #[test]
    fn user_id_override() {
        let svc = GcsArtifactService::new("b", "a").user_id("alice");
        assert_eq!(svc.user_id, "alice");
    }

    #[test]
    fn session_scoped_blob_name() {
        assert_eq!(
            blob_name("app", "user", "sess1", "file.bin", 3),
            "app/user/sess1/file.bin/3"
        );
    }

    #[test]
    fn user_namespaced_blob_name_ignores_session() {
        // ADK: user-namespaced files use `/user/` and ignore session_id.
        assert_eq!(
            blob_name("app", "alice", "sess1", "user:prefs.json", 0),
            "app/alice/user/user:prefs.json/0"
        );
    }

    #[test]
    fn blob_prefix_session_scoped() {
        assert_eq!(blob_prefix("app", "u", "s", "doc.txt"), "app/u/s/doc.txt");
    }

    #[test]
    fn blob_prefix_user_scoped() {
        assert_eq!(
            blob_prefix("app", "u", "s", "user:doc.txt"),
            "app/u/user/user:doc.txt"
        );
    }

    #[test]
    fn detects_user_namespace() {
        assert!(file_has_user_namespace("user:settings"));
        assert!(!file_has_user_namespace("settings"));
    }

    #[test]
    fn parses_version_suffix() {
        assert_eq!(version_from_blob_name("app/u/s/file/7"), Some(7));
        assert_eq!(version_from_blob_name("app/u/s/file/notanum"), None);
        assert_eq!(version_from_blob_name("0"), Some(0));
    }

    #[test]
    fn urlencode_encodes_slashes() {
        assert_eq!(urlencode("a/b/c"), "a%2Fb%2Fc");
        assert_eq!(urlencode("user:f.json"), "user%3Af.json");
        assert_eq!(urlencode("plain-name_1.txt"), "plain-name_1.txt");
    }

    #[test]
    fn missing_token_returns_storage_error() {
        let svc = GcsArtifactService::new("b", "a");
        let err = svc.token().unwrap_err();
        assert!(matches!(err, ArtifactError::Storage(_)));
        assert!(err.to_string().contains("missing auth token"));
    }

    #[test]
    fn with_token_sets_provider() {
        let svc = GcsArtifactService::new("b", "a").with_token("tok123");
        assert_eq!(svc.token().unwrap(), "tok123");
    }

    #[test]
    fn with_token_refresher_calls_closure() {
        let svc =
            GcsArtifactService::new("b", "a").with_token_refresher(|| "dynamic-token".to_string());
        assert_eq!(svc.token().unwrap(), "dynamic-token");
    }

    #[test]
    fn implements_artifact_service_trait() {
        fn _assert_trait(_: &dyn ArtifactService) {}
        let svc = GcsArtifactService::new("b", "a");
        _assert_trait(&svc);
    }
}
