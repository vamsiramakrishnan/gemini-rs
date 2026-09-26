//! Versioned, labelled storage for session specs ("bundles").
//!
//! A bundle is a session spec under a name. Every push stores an immutable
//! **version**, identified by the SHA-256 of the spec's canonical JSON, so
//! pushing the same spec twice yields the same version. **Labels** such as
//! `prod` or `staging` are movable pointers to a version. A runtime loads
//! `booking:prod`, and promoting or rolling back is moving a label; nothing
//! is rebuilt or redeployed.
//!
//! ```text
//! booking            the latest version
//! booking@3f2a9c1d   a version, by id or unique prefix
//! booking:prod       the version a label points to
//! ```
//!
//! [`open_store`] picks a backend from a URI: a directory path (or
//! `file://`) for local work and tests, or `gs://bucket/prefix` for Cloud
//! Storage (feature `gcs-store`). Other backends implement [`BundleObjects`],
//! a three-method object interface, and get versions and labels from
//! [`ObjectBundleStore`].
//!
//! Only valid specs are stored: a push that fails
//! [`validate`](super::SessionSpec::validate) is refused, so whatever a
//! runtime loads has already passed validation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::SessionSpec;

/// One stored version of a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleVersion {
    /// The bundle's name.
    pub name: String,
    /// The version id: the first 12 hex digits of [`digest`](Self::digest).
    pub version: String,
    /// SHA-256 of the spec's canonical JSON, in hex.
    pub digest: String,
    /// When this version was first pushed (RFC 3339, UTC).
    pub created_at: String,
    /// What changed, as given at push time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Which version of a bundle to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleRef {
    /// The most recently pushed version.
    Latest,
    /// A version id, or a unique prefix of one (at least 4 characters).
    Version(String),
    /// The version a label points to.
    Label(String),
}

impl BundleRef {
    /// Split `name`, `name@version` or `name:label` into a name and a
    /// reference.
    pub fn parse(reference: &str) -> (String, BundleRef) {
        if let Some((name, version)) = reference.split_once('@') {
            (name.to_string(), BundleRef::Version(version.to_string()))
        } else if let Some((name, label)) = reference.split_once(':') {
            (name.to_string(), BundleRef::Label(label.to_string()))
        } else {
            (reference.to_string(), BundleRef::Latest)
        }
    }
}

impl std::fmt::Display for BundleRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleRef::Latest => f.write_str("latest"),
            BundleRef::Version(v) => write!(f, "@{v}"),
            BundleRef::Label(l) => write!(f, ":{l}"),
        }
    }
}

/// Why a bundle operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// No such bundle, version or label.
    NotFound(String),
    /// The spec failed validation, or a name was not allowed.
    Invalid(Vec<String>),
    /// The backend failed.
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound(what) => write!(f, "not found: {what}"),
            StoreError::Invalid(errors) => write!(f, "invalid: {}", errors.join("; ")),
            StoreError::Backend(message) => write!(f, "bundle store: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Versioned, labelled storage for session specs. See the module docs.
#[async_trait]
pub trait BundleStore: Send + Sync {
    /// Store `spec` as a version of `name`. Pushing a spec that is already
    /// stored returns the existing version unchanged.
    async fn push(
        &self,
        name: &str,
        spec: &SessionSpec,
        message: Option<&str>,
    ) -> Result<BundleVersion, StoreError>;

    /// Load a version of `name`.
    async fn get(
        &self,
        name: &str,
        reference: &BundleRef,
    ) -> Result<(BundleVersion, SessionSpec), StoreError>;

    /// Every version of `name`, oldest first.
    async fn versions(&self, name: &str) -> Result<Vec<BundleVersion>, StoreError>;

    /// Every bundle name, sorted.
    async fn names(&self) -> Result<Vec<String>, StoreError>;

    /// Point `label` of `name` at `version` (an id or unique prefix).
    /// Returns the full version id.
    async fn set_label(&self, name: &str, label: &str, version: &str)
    -> Result<String, StoreError>;

    /// The labels of `name` and the versions they point to.
    async fn labels(&self, name: &str) -> Result<BTreeMap<String, String>, StoreError>;
}

/// The object interface a bundle backend implements. Keys are relative
/// paths with `/` separators, such as `booking/versions/3f2a9c1d4e5f.json`.
#[async_trait]
pub trait BundleObjects: Send + Sync {
    /// The object's bytes, or `None` when it does not exist.
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    /// Create or replace an object, atomically.
    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError>;
    /// Every key that starts with `prefix`.
    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError>;
}

/// A [`BundleStore`] over any [`BundleObjects`] backend.
///
/// Layout, per bundle: `<name>/versions/<id>.json` (the spec),
/// `<name>/versions/<id>.meta.json` (its [`BundleVersion`], written last so
/// a version is only listed once complete) and `<name>/labels/<label>` (a
/// version id).
pub struct ObjectBundleStore<O> {
    objects: O,
}

impl<O: BundleObjects> ObjectBundleStore<O> {
    /// A store over `objects`.
    pub fn new(objects: O) -> Self {
        Self { objects }
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        match self.objects.read(key).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StoreError::Backend(format!("{key}: {e}"))),
            None => Ok(None),
        }
    }

    async fn resolve(&self, name: &str, reference: &BundleRef) -> Result<String, StoreError> {
        match reference {
            BundleRef::Latest => self
                .versions(name)
                .await?
                .pop()
                .map(|v| v.version)
                .ok_or_else(|| StoreError::NotFound(format!("bundle '{name}'"))),
            BundleRef::Label(label) => {
                check_name("label", label)?;
                let key = format!("{name}/labels/{label}");
                match self.objects.read(&key).await? {
                    Some(bytes) => Ok(String::from_utf8_lossy(&bytes).trim().to_string()),
                    None => Err(StoreError::NotFound(format!("{name}:{label}"))),
                }
            }
            BundleRef::Version(prefix) => {
                if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(StoreError::Invalid(vec![format!(
                        "version '{prefix}': use at least 4 hex digits"
                    )]));
                }
                let matches: Vec<String> = self
                    .versions(name)
                    .await?
                    .into_iter()
                    .map(|v| v.version)
                    .filter(|v| v.starts_with(prefix.as_str()))
                    .collect();
                match matches.as_slice() {
                    [one] => Ok(one.clone()),
                    [] => Err(StoreError::NotFound(format!("{name}@{prefix}"))),
                    _ => Err(StoreError::Invalid(vec![format!(
                        "{name}@{prefix} matches {} versions; give more digits",
                        matches.len()
                    )])),
                }
            }
        }
    }
}

#[async_trait]
impl<O: BundleObjects> BundleStore for ObjectBundleStore<O> {
    async fn push(
        &self,
        name: &str,
        spec: &SessionSpec,
        message: Option<&str>,
    ) -> Result<BundleVersion, StoreError> {
        check_name("bundle", name)?;
        let validation = spec.validate();
        if !validation.valid {
            return Err(StoreError::Invalid(validation.errors));
        }
        let value = serde_json::to_value(spec).map_err(|e| StoreError::Backend(e.to_string()))?;
        let digest = hex(&Sha256::digest(canonical(&value).as_bytes()));
        let version = digest[..12].to_string();
        let meta_key = format!("{name}/versions/{version}.meta.json");
        if let Some(existing) = self.read_json::<BundleVersion>(&meta_key).await? {
            return Ok(existing);
        }
        let mut document =
            serde_json::to_vec_pretty(&value).map_err(|e| StoreError::Backend(e.to_string()))?;
        document.push(b'\n');
        self.objects
            .write(&format!("{name}/versions/{version}.json"), document)
            .await?;
        let meta = BundleVersion {
            name: name.to_string(),
            version,
            digest,
            created_at: now_rfc3339(),
            message: message.map(str::to_string).filter(|m| !m.trim().is_empty()),
        };
        let bytes =
            serde_json::to_vec_pretty(&meta).map_err(|e| StoreError::Backend(e.to_string()))?;
        self.objects.write(&meta_key, bytes).await?;
        Ok(meta)
    }

    async fn get(
        &self,
        name: &str,
        reference: &BundleRef,
    ) -> Result<(BundleVersion, SessionSpec), StoreError> {
        check_name("bundle", name)?;
        let version = self.resolve(name, reference).await?;
        let missing = || StoreError::NotFound(format!("{name}@{version}"));
        let meta: BundleVersion = self
            .read_json(&format!("{name}/versions/{version}.meta.json"))
            .await?
            .ok_or_else(missing)?;
        let value: Value = self
            .read_json(&format!("{name}/versions/{version}.json"))
            .await?
            .ok_or_else(missing)?;
        let spec = SessionSpec::from_value(value).map_err(StoreError::Backend)?;
        Ok((meta, spec))
    }

    async fn versions(&self, name: &str) -> Result<Vec<BundleVersion>, StoreError> {
        check_name("bundle", name)?;
        let mut versions = Vec::new();
        for key in self.objects.list(&format!("{name}/versions/")).await? {
            if key.ends_with(".meta.json")
                && let Some(meta) = self.read_json::<BundleVersion>(&key).await?
            {
                versions.push(meta);
            }
        }
        versions.sort_by(|a, b| {
            (a.created_at.as_str(), a.version.as_str())
                .cmp(&(b.created_at.as_str(), b.version.as_str()))
        });
        Ok(versions)
    }

    async fn names(&self) -> Result<Vec<String>, StoreError> {
        let mut names: Vec<String> = self
            .objects
            .list("")
            .await?
            .iter()
            .filter(|key| key.ends_with(".meta.json"))
            .filter_map(|key| {
                key.split_once("/versions/")
                    .map(|(name, _)| name.to_string())
            })
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    async fn set_label(
        &self,
        name: &str,
        label: &str,
        version: &str,
    ) -> Result<String, StoreError> {
        check_name("bundle", name)?;
        check_name("label", label)?;
        let version = self
            .resolve(name, &BundleRef::Version(version.to_string()))
            .await?;
        self.objects
            .write(
                &format!("{name}/labels/{label}"),
                version.clone().into_bytes(),
            )
            .await?;
        Ok(version)
    }

    async fn labels(&self, name: &str) -> Result<BTreeMap<String, String>, StoreError> {
        check_name("bundle", name)?;
        let prefix = format!("{name}/labels/");
        let mut labels = BTreeMap::new();
        for key in self.objects.list(&prefix).await? {
            if let Some(bytes) = self.objects.read(&key).await? {
                let label = key.trim_start_matches(prefix.as_str()).to_string();
                labels.insert(label, String::from_utf8_lossy(&bytes).trim().to_string());
            }
        }
        Ok(labels)
    }
}

/// Open the store a URI names: a directory path or `file://` URI, or
/// `gs://bucket/prefix` (feature `gcs-store`, credentials from
/// [`GoogleAccessToken::from_env`](gemini_genai_rs::transport::auth::GoogleAccessToken::from_env)).
pub fn open_store(uri: &str) -> Result<Arc<dyn BundleStore>, StoreError> {
    if let Some(rest) = uri.strip_prefix("gs://") {
        #[cfg(feature = "gcs-store")]
        {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            if bucket.is_empty() {
                return Err(StoreError::Invalid(vec![format!(
                    "'{uri}' names no bucket"
                )]));
            }
            return Ok(Arc::new(ObjectBundleStore::new(GcsObjects::new(
                bucket,
                prefix,
                Arc::new(gemini_genai_rs::transport::auth::GoogleAccessToken::from_env()),
            ))));
        }
        #[cfg(not(feature = "gcs-store"))]
        {
            let _ = rest;
            return Err(StoreError::Invalid(vec![format!(
                "'{uri}' needs the `gcs-store` feature"
            )]));
        }
    }
    if uri.contains("://") && !uri.starts_with("file://") {
        return Err(StoreError::Invalid(vec![format!(
            "'{uri}': use a directory path, file://, or gs://"
        )]));
    }
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    Ok(Arc::new(ObjectBundleStore::new(DirObjects::new(path))))
}

// ── Directory backend ───────────────────────────────────────────────────────

/// Bundles in a local directory.
pub struct DirObjects {
    root: PathBuf,
}

impl DirObjects {
    /// Objects under `root`, created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, key: &str) -> Result<PathBuf, StoreError> {
        if key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(StoreError::Invalid(vec![format!("object key '{key}'")]));
        }
        Ok(self.root.join(key))
    }
}

#[async_trait]
impl BundleObjects for DirObjects {
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match tokio::fs::read(self.path(key)?).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Backend(format!("{key}: {e}"))),
        }
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let path = self.path(key)?;
        let backend = |e: std::io::Error| StoreError::Backend(format!("{key}: {e}"));
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(backend)?;
        }
        // Write aside, then rename: readers never see a partial object.
        let staging = path.with_extension(format!("tmp-{}", std::process::id()));
        tokio::fs::write(&staging, bytes).await.map_err(backend)?;
        tokio::fs::rename(&staging, &path).await.map_err(backend)
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let mut keys = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(dir) = pending.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::Backend(format!("{}: {e}", dir.display()))),
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| StoreError::Backend(e.to_string()))?
            {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(relative) = path.strip_prefix(&self.root) {
                    let key = relative
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    if key.starts_with(prefix) && !key.contains(".tmp-") {
                        keys.push(key);
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}

// ── Cloud Storage backend ───────────────────────────────────────────────────

/// Bundles in a Cloud Storage bucket, through the JSON API.
///
/// `STORAGE_EMULATOR_HOST` (for example `http://localhost:4443`) points it
/// at an emulator instead of `https://storage.googleapis.com`.
#[cfg(feature = "gcs-store")]
pub struct GcsObjects {
    bucket: String,
    prefix: String,
    endpoint: String,
    token: Arc<gemini_genai_rs::transport::auth::GoogleAccessToken>,
    client: reqwest::Client,
}

#[cfg(feature = "gcs-store")]
impl GcsObjects {
    /// Objects in `bucket` under `prefix`, authorized with `token`.
    pub fn new(
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        token: Arc<gemini_genai_rs::transport::auth::GoogleAccessToken>,
    ) -> Self {
        let prefix = prefix.into().trim_matches('/').to_string();
        Self {
            bucket: bucket.into(),
            prefix: if prefix.is_empty() {
                prefix
            } else {
                format!("{prefix}/")
            },
            endpoint: std::env::var("STORAGE_EMULATOR_HOST")
                .ok()
                .filter(|h| !h.trim().is_empty())
                .map(|h| h.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "https://storage.googleapis.com".to_string()),
            token,
            client: reqwest::Client::new(),
        }
    }

    /// Use `endpoint` instead of the default (or `STORAGE_EMULATOR_HOST`).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into().trim_end_matches('/').to_string();
        self
    }

    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<url::Url, StoreError> {
        let mut url =
            url::Url::parse(&self.endpoint).map_err(|e| StoreError::Backend(e.to_string()))?;
        url.path_segments_mut()
            .map_err(|()| StoreError::Backend(format!("endpoint {}", self.endpoint)))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, StoreError> {
        let token = self
            .token
            .token()
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        let response = request
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.token.invalidate().await;
        }
        Ok(response)
    }

    async fn failure(what: &str, response: reqwest::Response) -> StoreError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        StoreError::Backend(format!("{what}: HTTP {status}: {}", body.trim()))
    }
}

#[cfg(feature = "gcs-store")]
#[async_trait]
impl BundleObjects for GcsObjects {
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let object = format!("{}{key}", self.prefix);
        let url = self.url(
            &["storage", "v1", "b", &self.bucket, "o", &object],
            &[("alt", "media")],
        )?;
        let response = self.send(self.client.get(url)).await?;
        match response.status() {
            s if s.is_success() => response
                .bytes()
                .await
                .map(|b| Some(b.to_vec()))
                .map_err(|e| StoreError::Backend(e.to_string())),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => Err(Self::failure(&object, response).await),
        }
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let object = format!("{}{key}", self.prefix);
        let url = self.url(
            &["upload", "storage", "v1", "b", &self.bucket, "o"],
            &[("uploadType", "media"), ("name", &object)],
        )?;
        let request = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .body(bytes);
        let response = self.send(request).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::failure(&object, response).await)
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            items: Vec<Item>,
            #[serde(default, rename = "nextPageToken")]
            next_page_token: Option<String>,
        }
        #[derive(Deserialize)]
        struct Item {
            name: String,
        }
        let full = format!("{}{prefix}", self.prefix);
        let mut keys = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                ("prefix", full.as_str()),
                ("fields", "items(name),nextPageToken"),
            ];
            if let Some(token) = &page_token {
                query.push(("pageToken", token.as_str()));
            }
            let url = self.url(&["storage", "v1", "b", &self.bucket, "o"], &query)?;
            let response = self.send(self.client.get(url)).await?;
            if !response.status().is_success() {
                return Err(Self::failure(&full, response).await);
            }
            let page: Page = response
                .json()
                .await
                .map_err(|e| StoreError::Backend(e.to_string()))?;
            keys.extend(page.items.into_iter().filter_map(|i| {
                i.name
                    .strip_prefix(self.prefix.as_str())
                    .map(str::to_string)
            }));
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }
        keys.sort();
        Ok(keys)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Names and labels: lowercase letters, digits, `-`, `_` and `.`, starting
/// with a letter or digit, at most 63 characters. They become object keys.
fn check_name(what: &str, name: &str) -> Result<(), StoreError> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && !name.contains("..");
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(vec![format!(
            "{what} name '{name}': use lowercase letters, digits, '-', '_' and '.' (at most 63)"
        )]))
    }
}

/// JSON with object keys sorted at every level, so equal specs hash equally
/// whatever the map implementation.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            format!(
                "[{}]",
                items.iter().map(canonical).collect::<Vec<_>>().join(",")
            )
        }
        other => other.to_string(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The current time as RFC 3339 in UTC, to the millisecond.
fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(instruction: &str) -> SessionSpec {
        SessionSpec::from_value(json!({
            "name": "booking",
            "instruction": instruction,
            "flow": { "steps": [{ "id": "s", "terminal": true }] }
        }))
        .unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bundles-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    async fn exercise(store: &dyn BundleStore) {
        let v1 = store
            .push("booking", &spec("one"), Some("first"))
            .await
            .unwrap();
        assert_eq!(v1.version.len(), 12);
        // Pushing the same spec again is the same version.
        let again = store
            .push("booking", &spec("one"), Some("dup"))
            .await
            .unwrap();
        assert_eq!(again, v1);
        // Sleep past the millisecond so versions order by time.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let v2 = store.push("booking", &spec("two"), None).await.unwrap();
        assert_ne!(v2.version, v1.version);

        let versions = store.versions("booking").await.unwrap();
        assert_eq!(
            versions
                .iter()
                .map(|v| v.version.as_str())
                .collect::<Vec<_>>(),
            [v1.version.as_str(), v2.version.as_str()]
        );
        assert_eq!(store.names().await.unwrap(), ["booking"]);

        // Latest, by prefix, by label.
        let (latest, loaded) = store.get("booking", &BundleRef::Latest).await.unwrap();
        assert_eq!(latest.version, v2.version);
        assert_eq!(loaded.instruction, "two");
        let (by_prefix, _) = store
            .get("booking", &BundleRef::Version(v1.version[..6].to_string()))
            .await
            .unwrap();
        assert_eq!(by_prefix.version, v1.version);
        assert_eq!(
            store
                .set_label("booking", "prod", &v1.version[..8])
                .await
                .unwrap(),
            v1.version
        );
        let (prod, loaded) = store
            .get("booking", &BundleRef::Label("prod".into()))
            .await
            .unwrap();
        assert_eq!(prod.version, v1.version);
        assert_eq!(loaded.instruction, "one");
        // Promotion is moving the label.
        store
            .set_label("booking", "prod", &v2.version)
            .await
            .unwrap();
        assert_eq!(store.labels("booking").await.unwrap()["prod"], v2.version);

        // Refusals.
        assert!(matches!(
            store
                .get("booking", &BundleRef::Label("staging".into()))
                .await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.set_label("booking", "prod", "ffffffff").await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.push("../etc", &spec("x"), None).await,
            Err(StoreError::Invalid(_))
        ));
        let invalid = SessionSpec::from_value(json!({
            "name": "bad",
            "flow": { "steps": [{ "id": "s", "allow": ["nope"] }] }
        }))
        .unwrap();
        assert!(matches!(
            store.push("bad", &invalid, None).await,
            Err(StoreError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn a_directory_store_versions_and_labels_bundles() {
        let dir = temp_dir("dir");
        let store = open_store(dir.to_str().unwrap()).unwrap();
        exercise(store.as_ref()).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn references_parse() {
        assert_eq!(
            BundleRef::parse("booking"),
            ("booking".into(), BundleRef::Latest)
        );
        assert_eq!(
            BundleRef::parse("booking@3f2a"),
            ("booking".into(), BundleRef::Version("3f2a".into()))
        );
        assert_eq!(
            BundleRef::parse("booking:prod"),
            ("booking".into(), BundleRef::Label("prod".into()))
        );
    }

    #[test]
    fn equal_specs_hash_equally_whatever_the_key_order() {
        let a = json!({ "b": 1, "a": { "y": [1, { "q": 2, "p": 3 }], "x": null } });
        let b = json!({ "a": { "x": null, "y": [1, { "p": 3, "q": 2 }] }, "b": 1 });
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(
            canonical(&a),
            r#"{"a":{"x":null,"y":[1,{"p":3,"q":2}]},"b":1}"#
        );
    }

    #[test]
    fn timestamps_are_rfc3339() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 24, "{now}");
        assert!(now.starts_with("20") && now.ends_with('Z'), "{now}");
    }

    /// A minimal Cloud Storage JSON API: media upload, media download and
    /// prefix listing, over an in-memory bucket.
    #[cfg(feature = "gcs-store")]
    async fn fake_gcs() -> String {
        use std::sync::Mutex;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let objects = objects.clone();
                tokio::spawn(async move {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 8192];
                    let (head, body) = loop {
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        data.extend_from_slice(&buf[..n]);
                        let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&data[..end]).to_string();
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break (head, data[end + 4..end + 4 + length].to_vec());
                        }
                    };
                    assert!(
                        head.to_ascii_lowercase()
                            .contains("authorization: bearer test-token")
                    );
                    let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let url = url::Url::parse(&format!("http://x{target}")).unwrap();
                    let query: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
                    let segments: Vec<String> =
                        url.path_segments().unwrap().map(percent_decode).collect();
                    let (status, reply) = match segments.as_slice() {
                        [u, ..] if u == "upload" => {
                            objects.lock().unwrap().insert(query["name"].clone(), body);
                            ("200 OK", b"{}".to_vec())
                        }
                        [_, _, _, _, o, name] if o == "o" => {
                            match objects.lock().unwrap().get(name) {
                                Some(bytes) => ("200 OK", bytes.clone()),
                                None => ("404 Not Found", b"{}".to_vec()),
                            }
                        }
                        _ => {
                            let prefix = query.get("prefix").cloned().unwrap_or_default();
                            let items: Vec<Value> = objects
                                .lock()
                                .unwrap()
                                .keys()
                                .filter(|k| k.starts_with(&prefix))
                                .map(|k| json!({ "name": k }))
                                .collect();
                            ("200 OK", json!({ "items": items }).to_string().into_bytes())
                        }
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        reply.len()
                    )
                    .into_bytes();
                    response.extend(reply);
                    let _ = socket.write_all(&response).await;
                });
            }
        });
        endpoint
    }

    #[cfg(feature = "gcs-store")]
    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let escaped = (bytes[i] == b'%' && i + 2 < bytes.len())
                .then(|| u8::from_str_radix(&s[i + 1..i + 3], 16).ok())
                .flatten();
            match escaped {
                Some(b) => {
                    out.push(b);
                    i += 3;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[cfg(feature = "gcs-store")]
    #[tokio::test]
    async fn a_cloud_storage_store_versions_and_labels_bundles() {
        let endpoint = fake_gcs().await;
        let token = Arc::new(gemini_genai_rs::transport::auth::GoogleAccessToken::fixed(
            "test-token",
        ));
        let store = ObjectBundleStore::new(
            GcsObjects::new("bucket", "team/bundles/", token).with_endpoint(endpoint),
        );
        exercise(&store).await;
    }
}
