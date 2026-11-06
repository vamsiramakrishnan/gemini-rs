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
    let _telemetry =
        gemini_adk_server_rs::telemetry::init_telemetry(tracing_subscriber::layer::Identity::new())
            .await;
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
