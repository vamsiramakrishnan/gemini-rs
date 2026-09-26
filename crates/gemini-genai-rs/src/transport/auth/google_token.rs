//! OAuth2 access tokens for Google Cloud APIs, from wherever the process runs.
//!
//! [`GoogleAccessToken::from_env`] finds a token the way Google's client
//! libraries do for a service: an explicit `GOOGLE_ACCESS_TOKEN`, else the
//! metadata server that Cloud Run, GKE and Compute Engine provide, else the
//! `gcloud` CLI on a developer machine. Tokens are cached until shortly
//! before they expire, so calling [`token`](GoogleAccessToken::token) per
//! request is cheap.

use std::time::{Duration, Instant};

use crate::session::AuthError;

/// Refresh this long before a token's reported expiry.
const EARLY: Duration = Duration::from_secs(300);
/// How long a `gcloud` token is trusted (it does not report its expiry;
/// user tokens last an hour).
const GCLOUD_LIFETIME: Duration = Duration::from_secs(45 * 60);

#[derive(Debug, Clone)]
enum Source {
    Fixed(String),
    Metadata(String),
    Gcloud,
    /// Metadata server if one answers, else `gcloud`.
    Auto(String),
}

/// A cached source of Google OAuth2 access tokens. See the module docs.
#[derive(Debug)]
pub struct GoogleAccessToken {
    source: Source,
    client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl GoogleAccessToken {
    /// `GOOGLE_ACCESS_TOKEN` when set; otherwise the metadata server when one
    /// answers (Cloud Run, GKE, Compute Engine), else `gcloud auth
    /// print-access-token`. `GCE_METADATA_HOST` overrides the metadata host.
    pub fn from_env() -> Self {
        match std::env::var("GOOGLE_ACCESS_TOKEN") {
            Ok(token) if !token.trim().is_empty() => Self::fixed(token.trim()),
            _ => Self::with_source(Source::Auto(metadata_host())),
        }
    }

    /// Always this token (it is not refreshed).
    pub fn fixed(token: impl Into<String>) -> Self {
        Self::with_source(Source::Fixed(token.into()))
    }

    /// Tokens for the attached service account, from the metadata server at
    /// `host` (normally `metadata.google.internal`).
    pub fn metadata_server(host: impl Into<String>) -> Self {
        Self::with_source(Source::Metadata(host.into()))
    }

    /// Tokens from `gcloud auth print-access-token`.
    pub fn gcloud() -> Self {
        Self::with_source(Source::Gcloud)
    }

    fn with_source(source: Source) -> Self {
        Self {
            source,
            client: reqwest::Client::new(),
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// A valid access token, fetching a new one when the cached one is
    /// about to expire.
    pub async fn token(&self) -> Result<String, AuthError> {
        let mut cached = self.cached.lock().await;
        if let Some((token, valid_until)) = cached.as_ref()
            && Instant::now() < *valid_until
        {
            return Ok(token.clone());
        }
        let (token, lifetime) = match &self.source {
            Source::Fixed(token) => return Ok(token.clone()),
            Source::Metadata(host) => self.fetch_metadata(host).await?,
            Source::Gcloud => (gcloud_token().await?, GCLOUD_LIFETIME),
            Source::Auto(host) => match self.fetch_metadata(host).await {
                Ok(fetched) => fetched,
                Err(metadata) => (
                    gcloud_token().await.map_err(|gcloud| {
                        AuthError::TokenFetchFailed(format!(
                            "no Google credentials: set GOOGLE_ACCESS_TOKEN, run on Google \
                             Cloud, or install the gcloud CLI ({metadata}; {gcloud})"
                        ))
                    })?,
                    GCLOUD_LIFETIME,
                ),
            },
        };
        *cached = Some((
            token.clone(),
            Instant::now() + lifetime.saturating_sub(EARLY),
        ));
        Ok(token)
    }

    /// Forget the cached token, e.g. after a request was rejected with 401.
    pub async fn invalidate(&self) {
        *self.cached.lock().await = None;
    }

    async fn fetch_metadata(&self, host: &str) -> Result<(String, Duration), AuthError> {
        #[derive(serde::Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let url =
            format!("http://{host}/computeMetadata/v1/instance/service-accounts/default/token");
        let response = self
            .client
            .get(&url)
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| AuthError::TokenFetchFailed(format!("metadata server: {e}")))?;
        if !response.status().is_success() {
            return Err(AuthError::TokenFetchFailed(format!(
                "metadata server: HTTP {}",
                response.status()
            )));
        }
        let token: Token = response
            .json()
            .await
            .map_err(|e| AuthError::TokenFetchFailed(format!("metadata server: {e}")))?;
        Ok((token.access_token, Duration::from_secs(token.expires_in)))
    }
}

fn metadata_host() -> String {
    std::env::var("GCE_METADATA_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "metadata.google.internal".to_string())
}

async fn gcloud_token() -> Result<String, AuthError> {
    let output = tokio::task::spawn_blocking(|| {
        std::process::Command::new("gcloud")
            .args(["auth", "print-access-token"])
            .output()
    })
    .await
    .map_err(|e| AuthError::TokenFetchFailed(format!("gcloud: {e}")))?
    .map_err(|e| AuthError::TokenFetchFailed(format!("gcloud: {e}")))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.is_empty() {
        return Err(AuthError::TokenFetchFailed(format!(
            "gcloud: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A metadata server that counts its requests and checks the header.
    async fn fake_metadata(
        expires_in: u64,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = if request.contains("metadata-flavor: google") {
                    let body = format!(
                        "{{\"access_token\":\"tok-{n}\",\"expires_in\":{expires_in},\"token_type\":\"Bearer\"}}"
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .into()
                };
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (host, hits)
    }

    #[tokio::test]
    async fn a_metadata_token_is_cached_until_it_nears_expiry() {
        let (host, hits) = fake_metadata(3600).await;
        let source = GoogleAccessToken::metadata_server(host);
        assert_eq!(source.token().await.unwrap(), "tok-0");
        assert_eq!(source.token().await.unwrap(), "tok-0");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        source.invalidate().await;
        assert_eq!(source.token().await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn a_token_about_to_expire_is_refetched() {
        // Expires inside the early-refresh window, so it is never reused.
        let (host, _) = fake_metadata(60).await;
        let source = GoogleAccessToken::metadata_server(host);
        assert_eq!(source.token().await.unwrap(), "tok-0");
        assert_eq!(source.token().await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn a_fixed_token_is_returned_as_is() {
        assert_eq!(
            GoogleAccessToken::fixed("abc").token().await.unwrap(),
            "abc"
        );
    }
}
