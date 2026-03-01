//! Observability layer — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: OTel spans and structured logging
//! - `metrics`: Prometheus metric definitions and export

pub mod logging;
pub mod metrics;
pub mod spans;

/// Telemetry configuration.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Enable structured logging.
    pub logging_enabled: bool,
    /// Log level filter (e.g., "info", "debug", "gemini_live_rs=debug").
    pub log_filter: String,
    /// Use JSON format for logs (production). If false, uses pretty format (development).
    pub json_logs: bool,
    /// Enable Prometheus metrics endpoint.
    pub metrics_enabled: bool,
    /// Prometheus listen address (e.g., "0.0.0.0:9090").
    pub metrics_addr: Option<String>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            logging_enabled: true,
            log_filter: "info".to_string(),
            json_logs: false,
            metrics_enabled: false,
            metrics_addr: None,
        }
    }
}

/// Guard that keeps telemetry systems alive while held.
#[derive(Default)]
pub struct TelemetryGuard {
    _private: (),
}

impl TelemetryConfig {
    /// Initialize telemetry subsystems based on configuration.
    pub fn init(&self) -> Result<TelemetryGuard, Box<dyn std::error::Error>> {
        #[cfg(feature = "tracing-support")]
        if self.logging_enabled {
            use tracing_subscriber::EnvFilter;

            let filter = EnvFilter::try_new(&self.log_filter)
                .unwrap_or_else(|_| EnvFilter::new("info"));

            if self.json_logs {
                let subscriber = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .json()
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            } else {
                let subscriber = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            }
        }

        Ok(TelemetryGuard::default())
    }
}
