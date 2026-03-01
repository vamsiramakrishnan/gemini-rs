//! Agent-level observability — OpenTelemetry tracing, structured logging, Prometheus metrics.
//!
//! Mirrors the wire crate's telemetry layer but for agent lifecycle operations.
//! All components are feature-gated for zero overhead when disabled:
//! - `tracing-support`: OTel spans and structured logging
//! - `metrics`: Prometheus metric definitions

pub mod logging;
pub mod metrics;
pub mod spans;
