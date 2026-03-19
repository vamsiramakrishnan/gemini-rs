//! HTTP client for Gemini REST APIs.
//!
//! Wraps `reqwest` with retry logic, telemetry, and typed errors.
//! Feature-gated behind `http`.

use std::time::Duration;

use crate::telemetry;

/// Configuration for the HTTP client.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Request timeout.
    pub timeout: Duration,
    /// Maximum number of retries on transient errors (5xx, network).
    pub max_retries: u32,
    /// Base delay for exponential backoff between retries.
    pub retry_base_delay: Duration,
    /// Maximum delay between retries.
    pub retry_max_delay: Duration,
    /// User-Agent header value.
    pub user_agent: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            retry_max_delay: Duration::from_secs(30),
            user_agent: format!("gemini-live/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Errors from HTTP client operations.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// HTTP request failed.
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// Server returned an error status.
    #[error("API error {status}: {message}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// Error message from the API.
        message: String,
        /// Optional response body.
        body: Option<serde_json::Value>,
    },

    /// Authentication error.
    #[error("Auth error: {0}")]
    Auth(String),

    /// JSON deserialization error.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// All retries exhausted.
    #[error("All {attempts} retries exhausted: {last_error}")]
    RetriesExhausted {
        /// Number of retry attempts made.
        attempts: u32,
        /// Error message from the last attempt.
        last_error: String,
    },
}

/// HTTP client wrapping reqwest with retry and telemetry.
pub struct HttpClient {
    inner: reqwest::Client,
    config: HttpConfig,
}

impl HttpClient {
    /// Create a new HTTP client with the given configuration.
    pub fn new(config: HttpConfig) -> Self {
        let inner = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .build()
            .expect("Failed to build reqwest client");
        Self { inner, config }
    }

    /// POST JSON to a URL and return the parsed response.
    pub async fn post_json(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, HttpError> {
        self.request_with_retry("POST", url, auth_headers, Some(body))
            .await
    }

    /// PATCH JSON to a URL and return the parsed response.
    pub async fn patch_json(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, HttpError> {
        self.request_with_retry("PATCH", url, auth_headers, Some(body))
            .await
    }

    /// PUT JSON to a URL and return the parsed response.
    pub async fn put_json(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: &impl serde::Serialize,
    ) -> Result<serde_json::Value, HttpError> {
        self.request_with_retry("PUT", url, auth_headers, Some(body))
            .await
    }

    /// GET a URL and return the parsed response.
    pub async fn get_json(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
    ) -> Result<serde_json::Value, HttpError> {
        self.request_with_retry::<()>("GET", url, auth_headers, None)
            .await
    }

    /// DELETE a URL and return the parsed response.
    pub async fn delete(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
    ) -> Result<serde_json::Value, HttpError> {
        self.request_with_retry::<()>("DELETE", url, auth_headers, None)
            .await
    }

    /// Execute an HTTP request with exponential backoff retry on transient errors.
    async fn request_with_retry<B: serde::Serialize>(
        &self,
        method: &str,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: Option<&B>,
    ) -> Result<serde_json::Value, HttpError> {
        let mut last_error = String::new();

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay = self.backoff_delay(attempt);
                telemetry::logging::log_http_retry(url, attempt, delay.as_millis() as u64);
                tokio::time::sleep(delay).await;
            }

            telemetry::logging::log_http_request(method, url);
            let _span = telemetry::spans::http_request_span(method, url);

            let start = std::time::Instant::now();
            match self.execute_request(method, url, &auth_headers, body).await {
                Ok(response) => {
                    let status = response.status();
                    let duration_ms = start.elapsed().as_millis() as f64;
                    telemetry::metrics::record_http_request(method, status.as_u16(), duration_ms);
                    telemetry::logging::log_http_response(status.as_u16(), duration_ms);

                    if status.is_success() {
                        let body = response.text().await?;
                        if body.is_empty() {
                            return Ok(serde_json::Value::Null);
                        }
                        return Ok(serde_json::from_str(&body)?);
                    }

                    let status_code = status.as_u16();
                    let body_text = response.text().await.unwrap_or_default();
                    let body_json: Option<serde_json::Value> =
                        serde_json::from_str(&body_text).ok();

                    // Extract error message
                    let message = body_json
                        .as_ref()
                        .and_then(|v| v.get("error"))
                        .and_then(|v| v.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&body_text)
                        .to_string();

                    // Retry on 5xx and 429 (rate limit)
                    if is_retryable_status(status_code) && attempt < self.config.max_retries {
                        last_error = format!("HTTP {status_code}: {message}");
                        continue;
                    }

                    return Err(HttpError::ApiError {
                        status: status_code,
                        message,
                        body: body_json,
                    });
                }
                Err(e) => {
                    let duration_ms = start.elapsed().as_millis() as f64;
                    telemetry::metrics::record_http_request(method, 0, duration_ms);

                    if is_retryable_error(&e) && attempt < self.config.max_retries {
                        last_error = e.to_string();
                        continue;
                    }
                    return Err(HttpError::Request(e));
                }
            }
        }

        Err(HttpError::RetriesExhausted {
            attempts: self.config.max_retries + 1,
            last_error,
        })
    }

    /// Execute a single HTTP request (no retry).
    async fn execute_request<B: serde::Serialize>(
        &self,
        method: &str,
        url: &str,
        auth_headers: &[(String, String)],
        body: Option<&B>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut builder = match method {
            "POST" => self.inner.post(url),
            "GET" => self.inner.get(url),
            "DELETE" => self.inner.delete(url),
            "PATCH" => self.inner.patch(url),
            "PUT" => self.inner.put(url),
            _ => self
                .inner
                .request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), url),
        };

        for (key, value) in auth_headers {
            builder = builder.header(key, value);
        }

        if let Some(body) = body {
            builder = builder.json(body);
        }

        builder.send().await
    }

    /// Calculate exponential backoff delay.
    fn backoff_delay(&self, attempt: u32) -> Duration {
        let delay = self.config.retry_base_delay * 2u32.saturating_pow(attempt.saturating_sub(1));
        std::cmp::min(delay, self.config.retry_max_delay)
    }
}

/// Whether an HTTP status code is retryable.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// Whether a reqwest error is retryable (network, timeout).
fn is_retryable_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = HttpConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert_eq!(config.max_retries, 3);
        assert!(config.user_agent.starts_with("gemini-live/"));
    }

    #[test]
    fn backoff_delay_calculation() {
        let client = HttpClient::new(HttpConfig {
            retry_base_delay: Duration::from_millis(100),
            retry_max_delay: Duration::from_secs(5),
            ..HttpConfig::default()
        });
        assert_eq!(client.backoff_delay(1), Duration::from_millis(100));
        assert_eq!(client.backoff_delay(2), Duration::from_millis(200));
        assert_eq!(client.backoff_delay(3), Duration::from_millis(400));
    }

    #[test]
    fn backoff_delay_capped() {
        let client = HttpClient::new(HttpConfig {
            retry_base_delay: Duration::from_secs(1),
            retry_max_delay: Duration::from_secs(5),
            ..HttpConfig::default()
        });
        // 2^9 = 512 seconds, should be capped at 5 seconds
        assert_eq!(client.backoff_delay(10), Duration::from_secs(5));
    }

    #[test]
    fn retryable_status_codes() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(599));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }
}
