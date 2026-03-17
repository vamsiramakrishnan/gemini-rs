//! OTLP exporter setup helpers for agent-level telemetry.
//!
//! Provides a convenience [`TelemetrySetup`] builder that configures tracing-subscriber
//! with optional OpenTelemetry exporters. This is a higher-level wrapper around the
//! L0 [`rs_genai::telemetry::TelemetryConfig`] tailored for agent applications.
//!
//! # Feature flags
//!
//! - `tracing-support`: Enables tracing-subscriber with env-filter and fmt layer.
//! - `otel-otlp`: Adds OTLP trace export via `opentelemetry-otlp`.
//! - `otel-gcp`: Adds Google Cloud Trace export via `opentelemetry-gcloud-trace`.
//!
//! Without any of these features, [`TelemetrySetup::init`] is a no-op that returns `Ok(())`.

/// Configuration for telemetry export.
///
/// Use the builder methods to configure the desired exporters, then call [`init`](TelemetrySetup::init)
/// to set up the global tracing subscriber.
///
/// # Examples
///
/// ```rust,no_run
/// use rs_adk::telemetry::setup::TelemetrySetup;
///
/// // Basic setup with console logging only (requires `tracing-support` feature)
/// TelemetrySetup::new("my-agent-service").init().unwrap();
///
/// // With OTLP export (requires `otel-otlp` feature)
/// TelemetrySetup::new("my-agent-service")
///     .with_otlp("http://localhost:4317")
///     .with_content_capture(true)
///     .init()
///     .unwrap();
///
/// // With Google Cloud Trace (requires `otel-gcp` feature)
/// TelemetrySetup::new("my-agent-service")
///     .with_cloud_trace()
///     .init()
///     .unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct TelemetrySetup {
    /// Service name for OTel resource identification.
    pub service_name: String,
    /// OTLP gRPC endpoint (e.g., `http://localhost:4317`).
    /// When set, enables OTLP trace export (requires `otel-otlp` feature).
    pub otlp_endpoint: Option<String>,
    /// Enable Google Cloud Trace export (requires `otel-gcp` feature).
    pub cloud_trace: bool,
    /// Whether to capture prompt/completion content in spans.
    /// Defaults to `false` to avoid logging sensitive data.
    pub capture_content: bool,
}

impl TelemetrySetup {
    /// Create a new telemetry setup with the given service name.
    ///
    /// Defaults to console logging only, no OTLP or Cloud Trace export,
    /// and content capture disabled.
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            otlp_endpoint: None,
            cloud_trace: false,
            capture_content: false,
        }
    }

    /// Set the OTLP gRPC endpoint for trace export.
    ///
    /// Requires the `otel-otlp` feature. If the feature is not enabled,
    /// this value is ignored during [`init`](TelemetrySetup::init).
    pub fn with_otlp(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// Enable Google Cloud Trace export.
    ///
    /// Requires the `otel-gcp` feature. If the feature is not enabled,
    /// this value is ignored during [`init`](TelemetrySetup::init).
    pub fn with_cloud_trace(mut self) -> Self {
        self.cloud_trace = true;
        self
    }

    /// Set whether to capture prompt/completion content in trace spans.
    ///
    /// Defaults to `false`. When enabled, LLM request and response content
    /// may be recorded in span attributes, which is useful for debugging
    /// but should be disabled in production to avoid logging sensitive data.
    pub fn with_content_capture(mut self, capture: bool) -> Self {
        self.capture_content = capture;
        self
    }

    /// Initialize the tracing subscriber with the configured exporters.
    ///
    /// This is a convenience function that sets up:
    /// - `tracing-subscriber` with `EnvFilter` (reads `RUST_LOG` env var, defaults to `info`)
    /// - OpenTelemetry tracer (if `otlp_endpoint` is set and `otel-otlp` feature is enabled)
    /// - Google Cloud Trace (if `cloud_trace` is set and `otel-gcp` feature is enabled)
    /// - Pretty log format for development
    ///
    /// # Feature behavior
    ///
    /// | Features enabled | Behavior |
    /// |-----------------|----------|
    /// | (none) | No-op, returns `Ok(())` |
    /// | `tracing-support` | Console logging with env-filter |
    /// | `tracing-support` + `otel-otlp` | Console + OTLP trace export |
    /// | `tracing-support` + `otel-gcp` | Console + Cloud Trace export |
    ///
    /// # Errors
    ///
    /// Returns an error if the tracing subscriber cannot be set (e.g., if one
    /// is already registered globally), or if OTel exporter initialization fails.
    pub fn init(self) -> Result<(), Box<dyn std::error::Error>> {
        #[cfg(feature = "tracing-support")]
        {
            let config = rs_genai::telemetry::TelemetryConfig {
                logging_enabled: true,
                log_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
                json_logs: false,
                metrics_enabled: false,
                metrics_addr: None,
                otel_traces: self.otlp_endpoint.is_some() || self.cloud_trace,
                otel_metrics: false,
                otel_service_name: self.service_name,
                otel_gcp_project: None,
            };

            // Set OTLP endpoint env var if provided (the OTLP exporter reads it).
            #[cfg(feature = "otel-otlp")]
            if let Some(ref endpoint) = self.otlp_endpoint {
                std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint);
            }

            let _guard = config.init()?;
            // Note: The guard is intentionally leaked here so the providers stay alive
            // for the process lifetime. For finer control, use TelemetryConfig::init()
            // directly and hold the guard.
            std::mem::forget(_guard);
        }

        #[cfg(not(feature = "tracing-support"))]
        {
            let _ = self; // suppress unused warning
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_setup_defaults() {
        let setup = TelemetrySetup::new("test-service");
        assert_eq!(setup.service_name, "test-service");
        assert!(setup.otlp_endpoint.is_none());
        assert!(!setup.cloud_trace);
        assert!(!setup.capture_content);
    }

    #[test]
    fn telemetry_setup_builder_chain() {
        let setup = TelemetrySetup::new("my-service")
            .with_otlp("http://localhost:4317")
            .with_cloud_trace()
            .with_content_capture(true);

        assert_eq!(setup.service_name, "my-service");
        assert_eq!(
            setup.otlp_endpoint.as_deref(),
            Some("http://localhost:4317")
        );
        assert!(setup.cloud_trace);
        assert!(setup.capture_content);
    }

    #[test]
    fn telemetry_setup_clone() {
        let setup = TelemetrySetup::new("svc").with_otlp("http://otel:4317");
        let cloned = setup.clone();
        assert_eq!(cloned.service_name, "svc");
        assert_eq!(cloned.otlp_endpoint.as_deref(), Some("http://otel:4317"));
    }
}
