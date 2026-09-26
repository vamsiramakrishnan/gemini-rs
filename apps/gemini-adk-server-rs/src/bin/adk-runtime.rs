//! `adk-runtime`: serve session-spec bundles from a bundle store.
//!
//! Configured by environment; see `gemini_adk_server_rs::runtime` and
//! docs/user-guide/deploy.md. The minimum:
//!
//! ```text
//! ADK_BUNDLES=gs://my-bucket/bundles ADK_SERVE=booking:prod \
//! ADK_RUNTIME_TOKENS=... adk-runtime
//! ```

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let _telemetry = init_telemetry().await;
    if let Err(e) = gemini_adk_server_rs::runtime::install_sdk_metrics() {
        tracing::warn!("SDK metrics unavailable on /metrics: {e}");
    }
    match gemini_adk_server_rs::runtime::run_from_env().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("adk-runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Logs, plus trace and metric export when the environment asks for it
/// (`OTEL_EXPORTER_OTLP_ENDPOINT` with `otel-otlp`, `ADK_TELEMETRY=gcp` with
/// `otel-gcp`). The guard flushes the exporters when dropped.
#[cfg_attr(
    not(feature = "otel-gcp"),
    allow(clippy::unused_async, reason = "Cloud Trace setup is async")
)]
async fn init_telemetry() -> gemini_genai_rs::telemetry::TelemetryGuard {
    use tracing_subscriber::prelude::*;

    let config = gemini_genai_rs::telemetry::TelemetryConfig::from_env();
    let filter = tracing_subscriber::EnvFilter::try_new(&config.log_filter)
        .unwrap_or_else(|_| "info".into());
    let registry = tracing_subscriber::registry()
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
    gemini_genai_rs::telemetry::TelemetryGuard::default()
}
