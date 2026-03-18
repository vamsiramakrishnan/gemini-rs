# OTel OTLP Export Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire `tracing-opentelemetry` into existing tracing spans and export traces + metrics via OTLP to Google Cloud Trace / Cloud Monitoring, feature-gated behind `otel`.

**Architecture:** The existing `tracing` spans (9 in L0, 5 in L1) and `metrics` counters/histograms are already defined and feature-gated. This plan adds a new `otel` feature flag that layers `tracing-opentelemetry` onto the existing `tracing-subscriber` pipeline, and configures `opentelemetry-otlp` exporters for both traces and metrics. The `TelemetryConfig::init()` method is rewritten to use `tracing_subscriber::Registry` with composable layers instead of the current `fmt().finish()` approach, enabling both console logging AND OTel export simultaneously. `TelemetryGuard` is enhanced to hold the `SdkTracerProvider` and `SdkMeterProvider` for clean shutdown.

**Tech Stack:** `opentelemetry 0.31`, `opentelemetry_sdk 0.31`, `opentelemetry-otlp 0.31`, `tracing-opentelemetry 0.32`, Rust feature flags.

---

### Task 1: Add OTel dependencies to rs-genai Cargo.toml

**Files:**
- Modify: `crates/rs-genai/Cargo.toml`

**Step 1: Add optional OTel dependencies and feature flag**

In the `[dependencies]` section, after the existing metrics dependencies (line 76), add:

```toml
# OpenTelemetry (optional, feature-gated behind `otel`)
opentelemetry = { version = "0.31", optional = true }
opentelemetry_sdk = { version = "0.31", features = ["rt-tokio"], optional = true }
opentelemetry-otlp = { version = "0.31", features = ["grpc-tonic", "trace", "metrics"], optional = true }
tracing-opentelemetry = { version = "0.32", optional = true }
```

In the `[features]` section, after `metrics = [...]` (line 30), add:

```toml
otel = [
  "dep:opentelemetry",
  "dep:opentelemetry_sdk",
  "dep:opentelemetry-otlp",
  "dep:tracing-opentelemetry",
  "tracing-support",
]
```

**Step 2: Verify it compiles without the feature**

Run: `cargo check -p rs-genai`
Expected: Compiles with no new warnings (otel deps are optional, not pulled in).

**Step 3: Verify it compiles with the feature**

Run: `cargo check -p rs-genai --features otel`
Expected: Compiles. The OTel crates are pulled in but not yet used.

**Step 4: Commit**

```bash
git add crates/rs-genai/Cargo.toml
git commit -m "feat(rs-genai): add optional otel dependencies behind feature flag"
```

---

### Task 2: Rewrite TelemetryConfig to support layered subscriber + OTel

**Files:**
- Modify: `crates/rs-genai/src/telemetry/mod.rs`

**Step 1: Add OTel fields to TelemetryConfig and rewrite init()**

Replace the entire file with the new implementation that:
1. Adds `otel_traces: bool` and `otel_metrics: bool` fields to `TelemetryConfig`
2. Adds `otel_service_name: String` for the OTel resource service name
3. Rewrites `init()` to use `tracing_subscriber::Registry` with composable layers
4. Under `#[cfg(feature = "otel")]`, creates the OTel trace pipeline and metrics provider
5. Holds `SdkTracerProvider` and `SdkMeterProvider` in `TelemetryGuard` for shutdown

```rust
//! Observability layer — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: Console logging via tracing-subscriber
//! - `metrics`: Prometheus metric definitions and export
//! - `otel`: OTLP trace and metric export to Google Cloud or any OTel collector

pub mod logging;
pub mod metrics;
pub mod spans;

/// Telemetry configuration.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Enable structured logging.
    pub logging_enabled: bool,
    /// Log level filter (e.g., "info", "debug", "rs_genai=debug").
    pub log_filter: String,
    /// Use JSON format for logs (production). If false, uses pretty format (development).
    pub json_logs: bool,
    /// Enable Prometheus metrics endpoint.
    pub metrics_enabled: bool,
    /// Prometheus listen address (e.g., "0.0.0.0:9090").
    pub metrics_addr: Option<String>,
    /// Enable OTLP trace export (requires `otel` feature).
    pub otel_traces: bool,
    /// Enable OTLP metrics export (requires `otel` feature).
    pub otel_metrics: bool,
    /// OTel service name for resource identification.
    pub otel_service_name: String,
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
        }
    }
}

/// Guard that keeps telemetry systems alive while held.
/// Drop this to flush and shutdown OTel exporters.
pub struct TelemetryGuard {
    #[cfg(feature = "otel")]
    _tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel")]
    _meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    #[cfg(not(feature = "otel"))]
    _private: (),
}

impl Default for TelemetryGuard {
    fn default() -> Self {
        Self {
            #[cfg(feature = "otel")]
            _tracer_provider: None,
            #[cfg(feature = "otel")]
            _meter_provider: None,
            #[cfg(not(feature = "otel"))]
            _private: (),
        }
    }
}

impl TelemetryConfig {
    /// Initialize telemetry subsystems based on configuration.
    ///
    /// When the `otel` feature is enabled and `otel_traces`/`otel_metrics` are set,
    /// this configures OTLP exporters that send data to whatever endpoint is set
    /// via the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var (defaults to
    /// `http://localhost:4317` for gRPC).
    ///
    /// The returned `TelemetryGuard` must be held alive for the duration of the
    /// application. Dropping it triggers a flush and shutdown of all exporters.
    pub fn init(&self) -> Result<TelemetryGuard, Box<dyn std::error::Error>> {
        let mut guard = TelemetryGuard::default();

        // --- OTel providers (must be created before tracing subscriber) ---
        #[cfg(feature = "otel")]
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

        #[cfg(feature = "otel")]
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
            self.init_tracing_subscriber(
                #[cfg(feature = "otel")]
                otel_tracer,
            )?;
        }

        Ok(guard)
    }

    #[cfg(feature = "tracing-support")]
    fn init_tracing_subscriber(
        &self,
        #[cfg(feature = "otel")] otel_tracer: Option<
            opentelemetry_sdk::trace::Tracer,
        >,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::EnvFilter;

        let filter = EnvFilter::try_new(&self.log_filter)
            .unwrap_or_else(|_| EnvFilter::new("info"));

        let fmt_layer = if self.json_logs {
            tracing_subscriber::fmt::layer()
                .json()
                .boxed()
        } else {
            tracing_subscriber::fmt::layer()
                .boxed()
        };

        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer);

        #[cfg(feature = "otel")]
        {
            if let Some(tracer) = otel_tracer {
                let otel_layer = tracing_opentelemetry::layer()
                    .with_tracer(tracer);
                let subscriber = registry.with(otel_layer);
                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            } else {
                tracing::subscriber::set_global_default(registry)
                    .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
            }
        }

        #[cfg(not(feature = "otel"))]
        {
            tracing::subscriber::set_global_default(registry)
                .map_err(|e| format!("Failed to set tracing subscriber: {e}"))?;
        }

        Ok(())
    }

    #[cfg(feature = "otel")]
    fn otel_resource(&self) -> opentelemetry_sdk::Resource {
        use opentelemetry::KeyValue;
        opentelemetry_sdk::Resource::builder_empty()
            .with_attributes([
                KeyValue::new(
                    "service.name",
                    self.otel_service_name.clone(),
                ),
            ])
            .build()
    }
}
```

**Step 2: Verify it compiles without otel feature**

Run: `cargo check -p rs-genai`
Expected: Compiles. The new fields exist but OTel code paths are `#[cfg]`-gated away.

**Step 3: Verify it compiles with otel feature**

Run: `cargo check -p rs-genai --features otel`
Expected: Compiles with the OTel integration code included.

**Step 4: Run tests**

Run: `cargo test -p rs-genai`
Expected: All existing tests pass. No test changes needed — `TelemetryConfig::default()` has `otel_traces: false` and `otel_metrics: false`.

**Step 5: Commit**

```bash
git add crates/rs-genai/src/telemetry/mod.rs
git commit -m "feat(rs-genai): add OTel OTLP trace and metrics export to TelemetryConfig"
```

---

### Task 3: Forward otel feature flag through rs-adk and adk-rs-fluent

**Files:**
- Modify: `crates/rs-adk/Cargo.toml`
- Modify: `crates/adk-rs-fluent/Cargo.toml`

**Step 1: Add otel feature to rs-adk**

In `crates/rs-adk/Cargo.toml`, add after the `metrics` feature (line 40):

```toml
otel = ["rs-genai/otel", "tracing-support"]
```

**Step 2: Add otel feature to adk-rs-fluent**

In `crates/adk-rs-fluent/Cargo.toml`, add after `gemini-llm` feature (line 29):

```toml
otel = ["rs-adk/otel", "rs-genai/otel"]
```

**Step 3: Verify workspace compiles**

Run: `cargo check --workspace`
Expected: Compiles.

**Step 4: Commit**

```bash
git add crates/rs-adk/Cargo.toml crates/adk-rs-fluent/Cargo.toml
git commit -m "feat(adk): forward otel feature flag through rs-adk and adk-rs-fluent"
```

---

### Task 4: Add otel feature to Web UI and document usage

**Files:**
- Modify: `apps/adk-web/Cargo.toml`
- Modify: `apps/adk-web/README.md`

**Step 1: Add optional otel feature to Web UI**

In `apps/adk-web/Cargo.toml`, add a `[features]` section at the end:

```toml
[features]
default = []
otel = ["rs-genai/otel", "rs-adk/otel"]
```

**Step 2: Update README with OTel instructions**

In `apps/adk-web/README.md`, add a new section after the "Model Routing" section documenting:

```markdown
## OpenTelemetry Export (Optional)

The demo supports exporting traces and metrics to any OTLP-compatible backend (Google Cloud Trace, Jaeger, etc.) via the `otel` feature flag.

### Build with OTel

```bash
cargo run -p rs-genai-ui --features otel
```

### Configure

Set standard OTel environment variables in `.env`:

```env
# OTLP endpoint (default: http://localhost:4317 for gRPC)
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317

# Service name for trace/metric resource identification
OTEL_SERVICE_NAME=gemini-rs
```

### Google Cloud Trace

To export to Google Cloud Trace, run a local OTel Collector with the
[Google Cloud exporter](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/exporter/googlecloudexporter),
or use the `OTEL_EXPORTER_OTLP_ENDPOINT` pointing to Cloud Trace's OTLP ingestion endpoint.

### Jaeger (local development)

```bash
# Start Jaeger with OTLP support
docker run -d --name jaeger \
  -p 4317:4317 \
  -p 16686:16686 \
  jaegertracing/jaeger:latest

# Run with OTel enabled
cargo run -p rs-genai-ui --features otel

# View traces at http://localhost:16686
```
```

**Step 3: Verify it compiles with and without the feature**

Run: `cargo check -p rs-genai-ui && cargo check -p rs-genai-ui --features otel`
Expected: Both compile.

**Step 4: Commit**

```bash
git add apps/adk-web/Cargo.toml apps/adk-web/README.md
git commit -m "feat(ui): add optional otel feature flag with documentation"
```

---

### Task 5: Wire OTel init into Web UI main.rs

**Files:**
- Modify: `apps/adk-web/src/main.rs`

**Step 1: Read current main.rs to understand init flow**

Look at how `TelemetryConfig` is (or isn't) currently used in `main.rs`. If it's not used yet, we need to add the init call. If it is, we need to enhance it with the OTel fields.

The key change: detect whether the `otel` feature is active and, if the `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set, enable `otel_traces` and `otel_metrics` in the config.

**Step 2: Add TelemetryConfig initialization**

In `main.rs`, near the top of `main()` (after `dotenvy::dotenv().ok()`), add:

```rust
// Initialize telemetry (tracing + optional OTel)
let otel_enabled = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok();
let _telemetry_guard = rs_genai::telemetry::TelemetryConfig {
    logging_enabled: true,
    log_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
    json_logs: false,
    otel_traces: otel_enabled,
    otel_metrics: otel_enabled,
    otel_service_name: std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| "gemini-rs".to_string()),
    ..Default::default()
}
.init()
.expect("Failed to initialize telemetry");
```

Remove any existing `tracing_subscriber::fmt::init()` or similar calls that would conflict.

**Step 3: Verify compilation**

Run: `cargo check -p rs-genai-ui`
Expected: Compiles. Without `otel` feature, `otel_traces` is ignored inside `init()`.

Run: `cargo check -p rs-genai-ui --features otel`
Expected: Compiles with OTel support active.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass (149 UI tests + all crate tests).

**Step 5: Commit**

```bash
git add apps/adk-web/src/main.rs
git commit -m "feat(ui): wire TelemetryConfig with optional OTel into main.rs"
```

---

### Task 6: Integration test — verify OTel feature compiles and existing tests pass

**Step 1: Full workspace build with otel**

Run: `cargo build --workspace --features rs-genai/otel`
Expected: Entire workspace builds. The `otel` feature propagates correctly.

**Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: All tests pass (no regressions from the TelemetryConfig rewrite).

**Step 3: Feature-specific test**

Run: `cargo test -p rs-genai --features otel`
Expected: All rs-genai tests pass with the otel feature enabled.

**Step 4: Verify no otel deps leak into default build**

Run: `cargo tree -p rs-genai -e features | grep opentelemetry`
Expected: No output (opentelemetry not in default dependency tree).

Run: `cargo tree -p rs-genai -e features --features otel | grep opentelemetry`
Expected: Shows opentelemetry, opentelemetry_sdk, opentelemetry-otlp, tracing-opentelemetry.

**Step 5: Commit any fixes**

If any issues found, fix and commit.
