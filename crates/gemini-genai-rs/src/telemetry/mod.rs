//! Observability layer — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: Console logging via tracing-subscriber
//! - `metrics`: Prometheus metric definitions and export
//! - `otel-otlp`: OTLP trace and metric export to any OTel collector
//! - `otel-gcp`: Google Cloud-native trace and metric export (Cloud Trace + Cloud Monitoring)

pub mod logging;
pub mod metrics;
pub mod spans;

/// Telemetry configuration.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Enable structured logging.
    pub logging_enabled: bool,
    /// Log level filter (e.g., "info", "debug", "gemini_genai_rs=debug").
    pub log_filter: String,
    /// Use JSON format for logs (production). If false, uses pretty format (development).
    pub json_logs: bool,
    /// Enable Prometheus metrics endpoint.
    pub metrics_enabled: bool,
    /// Prometheus listen address (e.g., "0.0.0.0:9090").
    pub metrics_addr: Option<String>,
    /// Enable OTel trace export (requires `otel-otlp` or `otel-gcp` feature).
    pub otel_traces: bool,
    /// Enable OTel metrics export (requires `otel-otlp` or `otel-gcp` feature).
    pub otel_metrics: bool,
    /// OTel service name for resource identification.
    pub otel_service_name: String,
    /// Google Cloud project ID for GCP-native OTel export.
    /// If None, auto-detects from ADC or environment.
    pub otel_gcp_project: Option<String>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            logging_enabled: true,
            log_filter: "info".to_string(),
            json_logs: false,
            metrics_enabled: false,
            metrics_addr: None,
            otel_traces: false,
            otel_metrics: false,
            otel_service_name: "gemini-live".to_string(),
            otel_gcp_project: None,
        }
    }
}

/// Guard that keeps telemetry systems alive while held.
/// Drop this to flush and shutdown OTel exporters.
#[derive(Default)]
pub struct TelemetryGuard {
    #[cfg(feature = "otel-base")]
    _tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel-base")]
    _meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    #[cfg(not(feature = "otel-base"))]
    _private: (),
}

impl TelemetryConfig {
    /// Initialize telemetry subsystems based on configuration.
    ///
    /// When `otel-otlp` is enabled and `otel_traces`/`otel_metrics` are set,
    /// this configures OTLP exporters that send data to whatever endpoint is set
    /// via the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var (defaults to
    /// `http://localhost:4317` for gRPC).
    ///
    /// When `otel-gcp` is enabled, use `init_gcp()` to set up Google Cloud-native
    /// exporters, or configure providers manually and call `init_with_tracer()`.
    ///
    /// The returned `TelemetryGuard` must be held alive for the duration of the
    /// application. Dropping it triggers a flush and shutdown of all exporters.
    pub fn init(&self) -> Result<TelemetryGuard, Box<dyn std::error::Error>> {
        #[allow(unused_mut)]
        let mut guard = TelemetryGuard::default();

        // --- OTel OTLP providers (must be created before tracing subscriber) ---
        #[cfg(feature = "otel-otlp")]
        let otel_tracer = if self.otel_traces {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .build()?;
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(self.otel_resource())
                .build();
            let tracer = opentelemetry::trace::TracerProvider::tracer(
                &provider,
                self.otel_service_name.clone(),
            );
            guard._tracer_provider = Some(provider);
            Some(tracer)
        } else {
            None
        };

        #[cfg(feature = "otel-otlp")]
        if self.otel_metrics {
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .build()?;
            let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_periodic_exporter(exporter)
                .with_resource(self.otel_resource())
                .build();
            opentelemetry::global::set_meter_provider(provider.clone());
            guard._meter_provider = Some(provider);
        }

        // --- Tracing subscriber ---
        #[cfg(feature = "tracing-support")]
        if self.logging_enabled {
            #[cfg(feature = "otel-otlp")]
            {
                self.init_tracing_subscriber_with_tracer(otel_tracer)
                    .map_err(|e| -> Box<dyn std::error::Error> { e })?;
            }
            #[cfg(not(feature = "otel-otlp"))]
            {
                self.init_tracing_subscriber()
                    .map_err(|e| -> Box<dyn std::error::Error> { e })?;
            }
        }

        Ok(guard)
    }

    /// Initialize telemetry with Google Cloud-native exporters (Cloud Trace + Cloud Monitoring).
    ///
    /// This is the GCP counterpart to `init()`. It uses `opentelemetry-gcloud-trace` for
    /// span export and `opentelemetry_gcloud_monitoring_exporter` for metrics.
    ///
    /// If `otel_gcp_project` is set, it is used as the GCP project ID. Otherwise the
    /// project ID is auto-detected from ADC or the environment.
    ///
    /// The returned `TelemetryGuard` must be held alive for the duration of the
    /// application. Dropping it triggers a flush and shutdown of all exporters.
    #[cfg(feature = "otel-gcp")]
    pub async fn init_gcp(
        &self,
    ) -> Result<TelemetryGuard, Box<dyn std::error::Error + Send + Sync>> {
        use opentelemetry_gcloud_trace::GcpCloudTraceExporterBuilder;

        let mut guard = TelemetryGuard::default();

        // --- GCP Cloud Trace provider ---
        let otel_tracer = if self.otel_traces {
            let gcp_trace_builder = if let Some(ref project_id) = self.otel_gcp_project {
                GcpCloudTraceExporterBuilder::new(project_id.clone())
                    .with_resource(self.otel_resource())
            } else {
                GcpCloudTraceExporterBuilder::for_default_project_id()
                    .await?
                    .with_resource(self.otel_resource())
            };

            let tracer_provider = gcp_trace_builder.create_provider().await?;
            let tracer = gcp_trace_builder.install(&tracer_provider).await?;
            opentelemetry::global::set_tracer_provider(tracer_provider.clone());
            guard._tracer_provider = Some(tracer_provider);
            Some(tracer)
        } else {
            None
        };

        // --- GCP Cloud Monitoring metrics ---
        if self.otel_metrics {
            use opentelemetry_gcloud_monitoring_exporter::{
                GCPMetricsExporter, GCPMetricsExporterConfig,
            };

            let mut metrics_cfg = GCPMetricsExporterConfig::default();
            metrics_cfg.prefix = format!("custom.googleapis.com/{}", self.otel_service_name);
            if let Some(ref project_id) = self.otel_gcp_project {
                metrics_cfg.project_id = Some(project_id.clone());
            }
            let metrics_exporter = GCPMetricsExporter::init(metrics_cfg).await?;

            use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
            let reader =
                PeriodicReader::builder(metrics_exporter, opentelemetry_sdk::runtime::Tokio)
                    .build();

            let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_resource(self.otel_resource())
                .with_reader(reader)
                .build();
            opentelemetry::global::set_meter_provider(meter_provider.clone());
            guard._meter_provider = Some(meter_provider);
        }

        // --- Tracing subscriber ---
        #[cfg(feature = "tracing-support")]
        if self.logging_enabled {
            self.init_tracing_subscriber_with_tracer(otel_tracer)?;
        }

        Ok(guard)
    }

    /// Set up the tracing subscriber with no OTel tracer layer (plain logging mode).
    ///
    /// Used when `tracing-support` is on but neither `otel-otlp` nor `otel-gcp`
    /// provides a tracer, or when called from `init()` without OTLP.
    #[cfg(feature = "tracing-support")]
    #[allow(dead_code)]
    fn init_tracing_subscriber(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.init_tracing_subscriber_with_tracer(None)
    }

    /// Set up the tracing subscriber, optionally wiring in an OTel tracer layer.
    ///
    /// Shared implementation used by both `init()` (OTLP path) and `init_gcp()`.
    #[cfg(feature = "tracing-support")]
    fn init_tracing_subscriber_with_tracer(
        &self,
        #[cfg(feature = "otel-base")] otel_tracer: Option<opentelemetry_sdk::trace::Tracer>,
        #[cfg(not(feature = "otel-base"))] _otel_tracer: Option<()>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::EnvFilter;

        let filter =
            EnvFilter::try_new(&self.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));

        let fmt_layer = if self.json_logs {
            tracing_subscriber::fmt::layer().json().boxed()
        } else {
            tracing_subscriber::fmt::layer().boxed()
        };

        let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

        #[cfg(feature = "otel-base")]
        {
            if let Some(tracer) = otel_tracer {
                let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
                let subscriber = registry.with(otel_layer);
                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            } else {
                tracing::subscriber::set_global_default(registry)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            }
        }

        #[cfg(not(feature = "otel-base"))]
        {
            tracing::subscriber::set_global_default(registry)
                .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
        }

        Ok(())
    }

    /// Build an OTel resource with the configured service name.
    #[cfg(feature = "otel-base")]
    pub(crate) fn otel_resource(&self) -> opentelemetry_sdk::Resource {
        use opentelemetry::KeyValue;
        opentelemetry_sdk::Resource::builder_empty()
            .with_attributes([KeyValue::new(
                "service.name",
                self.otel_service_name.clone(),
            )])
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = TelemetryConfig::default();
        assert!(config.logging_enabled);
        assert_eq!(config.log_filter, "info");
        assert!(!config.json_logs);
        assert!(!config.metrics_enabled);
        assert!(config.metrics_addr.is_none());
        assert!(!config.otel_traces);
        assert!(!config.otel_metrics);
        assert_eq!(config.otel_service_name, "gemini-live");
        assert!(config.otel_gcp_project.is_none());
    }

    #[test]
    fn config_builder_pattern() {
        let config = TelemetryConfig {
            logging_enabled: false,
            log_filter: "debug".to_string(),
            json_logs: true,
            metrics_enabled: true,
            metrics_addr: Some("0.0.0.0:9090".to_string()),
            otel_traces: true,
            otel_metrics: true,
            otel_service_name: "my-service".to_string(),
            otel_gcp_project: Some("my-project".to_string()),
        };
        assert!(!config.logging_enabled);
        assert_eq!(config.log_filter, "debug");
        assert!(config.json_logs);
        assert!(config.metrics_enabled);
        assert_eq!(config.metrics_addr.as_deref(), Some("0.0.0.0:9090"));
        assert!(config.otel_traces);
        assert!(config.otel_metrics);
        assert_eq!(config.otel_service_name, "my-service");
        assert_eq!(config.otel_gcp_project.as_deref(), Some("my-project"));
    }

    #[test]
    fn telemetry_guard_default() {
        let _guard = TelemetryGuard::default();
        // Verifies that TelemetryGuard::default() compiles and doesn't panic.
    }
}
