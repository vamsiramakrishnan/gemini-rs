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
    Auth(#[from] crate::session::AuthError),

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

    /// POST JSON to a URL and stream the Server-Sent Events it answers with,
    /// each event's `data` parsed as JSON.
    ///
    /// Transient failures are retried until the response starts, as for
    /// [`post_json`](Self::post_json); after that the stream is not retried.
    /// The client's timeout covers the whole stream.
    pub async fn post_sse(
        &self,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: &impl serde::Serialize,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<serde_json::Value, HttpError>>,
        HttpError,
    > {
        use futures_util::StreamExt;

        let response = self
            .send_with_retry("POST", url, auth_headers, Some(body))
            .await?;
        let state = (
            response,
            SseDecoder::default(),
            std::collections::VecDeque::<String>::new(),
        );
        let events = futures_util::stream::unfold(
            state,
            |(mut response, mut decoder, mut ready)| async move {
                loop {
                    if let Some(data) = ready.pop_front() {
                        let parsed = serde_json::from_str::<serde_json::Value>(&data)
                            .map_err(HttpError::from);
                        return Some((parsed, (response, decoder, ready)));
                    }
                    match response.chunk().await {
                        Ok(Some(bytes)) => ready.extend(decoder.push(&bytes)),
                        Ok(None) => {
                            ready.extend(decoder.finish());
                            if ready.is_empty() {
                                return None;
                            }
                        }
                        Err(e) => {
                            return Some((Err(HttpError::Request(e)), (response, decoder, ready)));
                        }
                    }
                }
            },
        );
        Ok(events.boxed())
    }

    /// Execute an HTTP request with exponential backoff retry on transient errors.
    async fn request_with_retry<B: serde::Serialize>(
        &self,
        method: &str,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: Option<&B>,
    ) -> Result<serde_json::Value, HttpError> {
        let response = self
            .send_with_retry(method, url, auth_headers, body)
            .await?;
        let body = response.text().await?;
        if body.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_str(&body)?)
    }

    /// Send a request, retrying transient failures (5xx, 429, network) with
    /// exponential backoff, and return the first successful response.
    async fn send_with_retry<B: serde::Serialize>(
        &self,
        method: &str,
        url: &str,
        auth_headers: Vec<(String, String)>,
        body: Option<&B>,
    ) -> Result<reqwest::Response, HttpError> {
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
                        return Ok(response);
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
/// Splits a Server-Sent Events byte stream into the `data` of each event.
///
/// Bytes are buffered until an event is complete (a blank line), so an event
/// or a UTF-8 character split across network chunks is decoded whole. Lines
/// other than `data:` (`event:`, `id:`, comments) are ignored; the lines of a
/// multi-line `data` field are joined with `\n`.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    /// Feed received bytes; returns the `data` of every event they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((end, separator)) = find_event_end(&self.buffer) {
            let event: Vec<u8> = self.buffer.drain(..end + separator).collect();
            if let Some(data) = event_data(&event[..end]) {
                events.push(data);
            }
        }
        events
    }

    /// The stream ended: return the last event if it was not terminated.
    pub fn finish(&mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.buffer);
        event_data(&rest).into_iter().collect()
    }
}

/// Where the first complete event ends, and the length of its separator.
fn find_event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    (0..buffer.len()).find_map(|i| {
        if buffer[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else if buffer[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else {
            None
        }
    })
}

/// The joined `data:` lines of one event, if it has any.
fn event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let data: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|value| value.strip_prefix(' ').unwrap_or(value))
        .collect();
    (!data.is_empty()).then(|| data.join("\n"))
}

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
    fn sse_events_survive_arbitrary_chunk_boundaries() {
        let wire = "data: {\"a\":1}\n\n: comment\nevent: x\ndata: {\"b\":\"é\"}\r\n\r\ndata: [1,\ndata: 2]\n\n";
        let bytes = wire.as_bytes();
        // Every split point, including one inside the two-byte `é`.
        for split in 0..=bytes.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&bytes[..split]);
            events.extend(decoder.push(&bytes[split..]));
            events.extend(decoder.finish());
            assert_eq!(
                events,
                ["{\"a\":1}", "{\"b\":\"é\"}", "[1,\n2]"],
                "split at {split}"
            );
        }
    }

    /// Serve one canned HTTP response, written in the given pieces.
    async fn serve_once(pieces: Vec<&'static str>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/stream", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = socket.read(&mut request).await;
            for piece in pieces {
                socket.write_all(piece.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        url
    }

    fn client() -> HttpClient {
        HttpClient::new(HttpConfig {
            max_retries: 0,
            ..HttpConfig::default()
        })
    }

    #[tokio::test]
    async fn post_sse_yields_each_event_as_it_arrives() {
        use futures_util::StreamExt;
        let url = serve_once(vec![
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            "data: {\"n\":1}\n",
            "\ndata: {\"n\"",
            ":2}\n\n",
        ])
        .await;
        let events: Vec<_> = client()
            .post_sse(&url, vec![], &serde_json::json!({}))
            .await
            .unwrap()
            .collect()
            .await;
        let values: Vec<serde_json::Value> = events.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            values,
            [serde_json::json!({"n": 1}), serde_json::json!({"n": 2})]
        );
    }

    #[tokio::test]
    async fn post_sse_reports_an_error_status_before_streaming() {
        let url = serve_once(vec![
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
            "{\"error\":{\"message\":\"bad schema\"}}",
        ])
        .await;
        match client()
            .post_sse(&url, vec![], &serde_json::json!({}))
            .await
        {
            Err(HttpError::ApiError {
                status, message, ..
            }) => {
                assert_eq!(status, 400);
                assert_eq!(message, "bad schema");
            }
            Err(other) => panic!("expected ApiError, got {other}"),
            Ok(_) => panic!("expected ApiError, got a stream"),
        }
    }

    #[test]
    fn an_unterminated_last_event_is_kept() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {}").is_empty());
        assert_eq!(decoder.finish(), ["{}"]);
    }

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
