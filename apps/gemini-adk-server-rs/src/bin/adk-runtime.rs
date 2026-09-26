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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match gemini_adk_server_rs::runtime::run_from_env().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("adk-runtime: {e}");
            ExitCode::FAILURE
        }
    }
}
