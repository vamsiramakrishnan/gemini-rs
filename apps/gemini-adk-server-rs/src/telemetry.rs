//! Process-wide tracing setup shared by `adk-runtime` and the web UI.

use gemini_genai_rs::telemetry::{TelemetryConfig, TelemetryGuard};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Install the global tracing subscriber: logs, `extra` (a layer of the
/// caller's own, such as the web UI's span stream; pass
/// `tracing_subscriber::layer::Identity::new()` for none), and trace and
/// metric export when the environment asks for it
/// (`OTEL_EXPORTER_OTLP_ENDPOINT` with `otel-otlp`, `ADK_TELEMETRY=gcp` with
/// `otel-gcp`). The returned guard flushes the exporters when dropped.
///
/// Panics if a global subscriber is already installed.
#[cfg_attr(
    not(feature = "otel-gcp"),
    allow(clippy::unused_async, reason = "Cloud Trace setup is async")
)]
pub async fn init_telemetry<L>(extra: L) -> TelemetryGuard
where
    L: Layer<Registry> + Send + Sync + 'static,
{
    let config = TelemetryConfig::from_env();
    let filter = EnvFilter::try_new(&config.log_filter).unwrap_or_else(|_| "info".into());
    let registry = tracing_subscriber::registry()
        .with(extra)
        .with(filter)
        .with(tracing_subscriber::fmt::layer());

    #[cfg(any(feature = "otel-otlp", feature = "otel-gcp"))]
    {
        let exporters: Result<_, Box<dyn std::error::Error + Send + Sync>> = if config.wants_gcp() {
            #[cfg(feature = "otel-gcp")]
            {
                config.build_gcp().await
            }
            #[cfg(not(feature = "otel-gcp"))]
            {
                Err("ADK_TELEMETRY=gcp needs the otel-gcp feature".into())
            }
        } else if config.otel_traces || config.otel_metrics {
            #[cfg(feature = "otel-otlp")]
            {
                config.build_otlp()
            }
            #[cfg(not(feature = "otel-otlp"))]
            {
                Err("OTEL_EXPORTER_OTLP_ENDPOINT needs the otel-otlp feature".into())
            }
        } else {
            Ok(gemini_genai_rs::telemetry::Exporters::default())
        };
        match exporters {
            Ok(exporters) => {
                registry.with(exporters.layer()).init();
                return exporters.guard;
            }
            Err(e) => eprintln!("telemetry export disabled: {e}"),
        }
    }

    registry.init();
    TelemetryGuard::default()
}
