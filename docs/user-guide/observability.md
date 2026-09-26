# Observability

A Live session produces three kinds of signal:

- **Traces.** Each session is one `live_session` span carrying
  `gen_ai.conversation.id` (the persistence session id when set, else the
  transport's). Every turn is a `turn` span beneath it, and each tool call,
  VAD edge, interruption and transcript is logged inside its turn.
- **Metrics**, all prefixed `gemini_genai_rs_`:

  | Metric | Kind | What |
  |---|---|---|
  | `connections_total`, `sessions_active` | counter, gauge | Live sessions connected, open now |
  | `reconnections_total` | counter | reconnects (GoAway or a dropped connection) |
  | `ws_bytes_sent_total`, `ws_bytes_received_total` | counter | Live wire traffic |
  | `response_latency_ms` | histogram | end of user speech (or text) to the model's first output |
  | `tool_calls_total`, `tool_call_duration_ms` | counter, histogram | by `function` |
  | `tokens_total` | counter | by `direction` (`prompt`, `response`) and `modality` (`TEXT`, `AUDIO`, `IMAGE`, `VIDEO`), per turn |
  | `http_requests_total`, `http_request_duration_ms` | counter, histogram | REST calls |

- **In-process telemetry.** `SessionTelemetry` counts the same things for
  one session. Its `snapshot()` includes `tokens_by_modality`. The web UI's
  DevTools panels show it.

## Turning export on

Configuration comes from the environment through
`TelemetryConfig::from_env()`:

| Variable | Effect | Feature |
|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | export traces and metrics over OTLP/gRPC to that collector | `otel-otlp` |
| `ADK_TELEMETRY=gcp` | export to Cloud Trace and Cloud Monitoring (project from `GOOGLE_CLOUD_PROJECT`, else detected) | `otel-gcp` |
| `OTEL_SERVICE_NAME` | the service name (default `gemini-live`) | |
| `ADK_METRICS_ADDR` | serve Prometheus metrics at this address, e.g. `0.0.0.0:9464` | `metrics` |
| `RUST_LOG` | log filter (default `info`) | |

An application that owns its subscriber can call `TelemetryConfig::init()`
(or `init_gcp()`). One that builds its own subscriber, for example to add a
layer, builds the exporters and attaches their layer:

```rust,ignore
use gemini_genai_rs::telemetry::TelemetryConfig;
use tracing_subscriber::prelude::*;

let config = TelemetryConfig::from_env();
config.install_metrics()?;               // Prometheus, if ADK_METRICS_ADDR is set
let exporters = config.build_otlp()?;    // or build_gcp().await
tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer())
    .with(my_layer)
    .with(exporters.layer())
    .init();
let _guard = exporters.guard;            // hold for the life of the process
```

The web UI does exactly this when it is built with `--features otel-otlp`
or `--features otel-gcp`:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
  cargo run -p gemini-adk-web-rs --features otel-otlp
```

Metrics are recorded through the `metrics` facade. They go nowhere until a
recorder is installed, such as the Prometheus endpoint above.

## What is not exported

Transcript text, tool arguments and results appear in logs at `debug` and
`info`. They are not span attributes, so a trace backend doesn't receive
them unless you ship logs there too. See [hardening](hardening.md) for what
redaction does and does not cover.
