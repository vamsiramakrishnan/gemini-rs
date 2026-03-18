# AudioWorklet + OTel + Streamlined Devtools Design

**Date**: 2026-03-03
**Status**: Approved

## Problem

1. **Audio**: The Web UI uses the deprecated `ScriptProcessorNode` for mic
   capture. It runs on the main thread, causes GC pauses and UI jank under
   sustained audio streaming, and Chrome has signaled future removal.

2. **Telemetry**: The crates define 24 Prometheus-style metrics, 9 tracing
   spans, and a full `TelemetryConfig` with atomic counters — none of which
   reach standard observability tooling. The browser devtools has a custom
   telemetry tab that duplicates what Jaeger/Cloud Trace does better.

3. **UI**: The devtools telemetry tab is a hand-rolled stats grid. Standard OTel
   tooling (Cloud Trace, Cloud Monitoring) provides richer visualization with
   no custom code.

## Decision

Three workstreams, shipped independently:

- **A**: Replace `ScriptProcessorNode` with `AudioWorkletProcessor` for both
  capture and playback. Zero main-thread audio processing.
- **B**: Wire `tracing-opentelemetry` into existing spans and export via OTLP
  to Google Cloud Trace + Cloud Monitoring.
- **C**: Remove the custom telemetry tab from devtools. Polish remaining tabs
  (State, Events, Playbook, Evaluator). Add a session status bar.

---

## Workstream A: High-Performance AudioWorklet

### Architecture

```
Microphone
    |
    v
MediaStreamSource --> CaptureWorkletProcessor (audio thread)
                        |  - 16kHz mono resample
                        |  - Float32 -> PCM16 conversion
                        |  - Ring buffer accumulation (4096 samples)
                        v
                   MessagePort --> Main thread --> base64 --> WebSocket
                   (Transferable ArrayBuffer, zero-copy)


WebSocket --> Main thread --> MessagePort
                                   |
                                   v
                          PlaybackWorkletProcessor (audio thread)
                              |  - Ring buffer (24kHz, ~200ms capacity)
                              |  - Int16 -> Float32 conversion
                              |  - Underrun detection (silence fill)
                              v
                          AudioDestination (speakers)
```

### Files

| File | Purpose |
|------|---------|
| `static/worklets/capture-processor.js` | AudioWorkletProcessor for mic capture |
| `static/worklets/playback-processor.js` | AudioWorkletProcessor for speaker playback |
| `static/audio.js` | Rewritten AudioManager, loads worklets, ScriptProcessorNode fallback |

### CaptureWorkletProcessor

- Runs on audio rendering thread at native sample rate
- Accumulates Float32 samples in a ring buffer
- When buffer reaches 4096 samples: converts Float32 -> PCM16 (Int16Array),
  posts via `MessagePort` with `Transferable` ownership (zero-copy)
- No resampling in worklet — AudioContext is created with `sampleRate: 16000`
  to let the browser handle resampling natively

### PlaybackWorkletProcessor

- Ring buffer sized for ~200ms at 24kHz (~4800 samples)
- Main thread posts Int16 PCM chunks via `MessagePort`
- Worklet converts Int16 -> Float32, writes to ring buffer
- `process()` reads from ring buffer each quantum (128 samples)
- On underrun: fills silence, posts `underrun` event to main thread
- On `flush` message: clears ring buffer (for interruption handling)

### AudioManager Rewrite

```javascript
class AudioManager {
  // Feature detection
  static get workletSupported() {
    return typeof AudioWorkletNode !== 'undefined';
  }

  async initPlayback() {
    this.playbackCtx = new AudioContext({ sampleRate: 24000 });
    if (AudioManager.workletSupported) {
      await this.playbackCtx.addModule('/static/worklets/playback-processor.js');
      this.playbackNode = new AudioWorkletNode(this.playbackCtx, 'playback-processor');
      this.playbackNode.connect(this.playbackCtx.destination);
    }
    // else: fallback to AudioBufferSourceNode scheduling (current approach)
  }

  async startRecording() {
    this.recordCtx = new AudioContext({ sampleRate: 16000 });
    const stream = await navigator.mediaDevices.getUserMedia({ audio: ... });
    const source = this.recordCtx.createMediaStreamSource(stream);

    if (AudioManager.workletSupported) {
      await this.recordCtx.addModule('/static/worklets/capture-processor.js');
      this.captureNode = new AudioWorkletNode(this.recordCtx, 'capture-processor');
      this.captureNode.port.onmessage = (e) => {
        // e.data is Int16Array (transferred, zero-copy)
        this.onAudioData(base64Encode(e.data.buffer));
      };
      source.connect(this.captureNode);
    } else {
      // fallback: ScriptProcessorNode (current code)
    }
  }

  playAudio(base64) {
    const pcm = base64Decode(base64);
    if (this.playbackNode) {
      // Post to worklet ring buffer (Transferable)
      this.playbackNode.port.postMessage(pcm, [pcm.buffer]);
    } else {
      // fallback: schedule AudioBufferSourceNode
    }
  }

  clearQueue() {
    if (this.playbackNode) {
      this.playbackNode.port.postMessage({ type: 'flush' });
    }
  }
}
```

### Performance Properties

- Zero audio processing on main thread (UI stays at 60fps)
- `postMessage` with `Transferable` ArrayBuffers avoids structured cloning
- Ring buffer in playback worklet absorbs network jitter (no clicks/gaps)
- Graceful fallback for older browsers (Firefox ESR, Safari < 14.1)

---

## Workstream B: Google Cloud OTel Export

### Architecture

```
tracing spans (already defined)          metrics counters (already defined)
    |                                         |
    v                                         v
tracing-opentelemetry layer              opentelemetry-sdk MeterProvider
    |                                         |
    v                                         v
opentelemetry-otlp exporter              opentelemetry-otlp exporter
    |                                         |
    v                                         v
Google Cloud Trace                       Google Cloud Monitoring
```

### Integration Point

Enhance `TelemetryConfig::init()` in `rs-genai/src/telemetry/mod.rs` to
optionally attach the OTel tracing layer and metrics exporter.

### Configuration (env vars, standard OTel SDK config)

```env
OTEL_EXPORTER_OTLP_ENDPOINT=https://monitoring.googleapis.com
OTEL_SERVICE_NAME=gemini-rs
OTEL_TRACES_EXPORTER=otlp
OTEL_METRICS_EXPORTER=otlp
```

### New Dependencies (feature-gated behind `otel`)

```toml
# rs-genai Cargo.toml
[features]
otel = [
  "dep:opentelemetry",
  "dep:opentelemetry_sdk",
  "dep:opentelemetry-otlp",
  "dep:tracing-opentelemetry",
  "dep:opentelemetry-semantic-conventions",
  "tracing-support",
]

[dependencies]
opentelemetry = { version = "0.28", optional = true }
opentelemetry_sdk = { version = "0.28", features = ["rt-tokio"], optional = true }
opentelemetry-otlp = { version = "0.28", features = ["grpc-tonic"], optional = true }
tracing-opentelemetry = { version = "0.29", optional = true }
opentelemetry-semantic-conventions = { version = "0.28", optional = true }
```

### TelemetryConfig Enhancement

```rust
pub struct TelemetryConfig {
    // Existing fields
    pub logging_enabled: bool,
    pub log_filter: String,
    pub json_logs: bool,
    pub metrics_enabled: bool,
    pub metrics_addr: Option<String>,

    // New OTel fields
    pub otel_traces: bool,       // Enable OTLP trace export
    pub otel_metrics: bool,      // Enable OTLP metrics export
    pub otel_service_name: String,
}

impl TelemetryConfig {
    pub fn init(&self) -> Result<TelemetryGuard, Box<dyn std::error::Error>> {
        let mut layers = Vec::new();

        // Existing: tracing-subscriber with env filter
        if self.logging_enabled { ... }

        // New: OpenTelemetry tracing layer
        #[cfg(feature = "otel")]
        if self.otel_traces {
            let tracer = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .build()?;
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_batch_exporter(tracer)
                .with_resource(resource())
                .build();
            let otel_layer = tracing_opentelemetry::layer()
                .with_tracer(provider.tracer(self.otel_service_name.clone()));
            layers.push(otel_layer);
        }

        // New: OpenTelemetry metrics
        #[cfg(feature = "otel")]
        if self.otel_metrics {
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .build()?;
            let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
                .build();
            let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_reader(reader)
                .with_resource(resource())
                .build();
            opentelemetry::global::set_meter_provider(provider);
        }

        // Compose layers into tracing subscriber
        ...
    }
}
```

### Span Mapping

Existing spans map directly to OTel:

| Tracing Span | OTel Span Name | Key Attributes |
|---|---|---|
| `rs_genai.session` | `rs_genai.session` | `session.id` |
| `rs_genai.connect` | `rs_genai.connect` | `net.peer.name` |
| `rs_genai.tool_call` | `rs_genai.tool_call` | `rpc.method` |
| `gemini.agent.run` | `gemini.agent.run` | `agent.name` |
| `gemini.agent.transfer` | `gemini.agent.transfer` | `agent.from`, `agent.to` |

No changes needed to span definitions — `tracing-opentelemetry` bridges
them automatically.

---

## Workstream C: Streamlined Devtools

### Replace Telemetry Tab with NFR Metrics

The full telemetry dashboard is replaced by two components:

1. **Session status bar** (always visible, above tabs)
2. **NFR Metrics tab** (replaces old Telemetry tab, focused on performance)

Heavy observability (distributed traces, metric time series, span
hierarchies) goes to Google Cloud Trace / Monitoring. The devtools keeps
only the NFR metrics that matter during live development and testing.

### Keep and Polish

| Tab | Content | Changes |
|---|---|---|
| **State** | Grouped key-value table with flash animation | No changes |
| **Events** | Timeline log of all server messages | No changes |
| **Playbook** | Phase timeline waterfall | No changes |
| **Evaluator** | Violations + evaluation scores | No changes |
| **NFR** | Focused performance metrics (replaces Telemetry) | New — see below |

### Session Status Bar

A compact bar at the top of devtools (above tabs), always visible:

```
[Connected 2m 34s] | Phase: identify_caller | Turns: 5
```

- Session uptime (ticking via `requestAnimationFrame`)
- Current phase badge
- Turn count

### NFR Metrics Tab

Focused on the performance metrics that matter during development:

```
┌─────────────────────────────────────────┐
│  TTFB (Time to First Byte)              │
│  ┌──────┐  ┌──────┐  ┌──────┐          │
│  │ Last │  │ Avg  │  │ P95* │          │
│  │ 342ms│  │ 289ms│  │ 410ms│          │
│  └──────┘  └──────┘  └──────┘          │
│  Min: 180ms          Max: 520ms         │
│  ▓▓▓▓▓▓▓▓▓░░░░░░░░░░ (range bar)       │
│                                         │
│  Per-Turn Duration                      │
│  ┌──────┐  ┌──────┐                     │
│  │ Avg  │  │ Count│                     │
│  │ 4.2s │  │  5   │                     │
│  └──────┘  └──────┘                     │
│                                         │
│  Interruptions: 2                       │
│  Audio: 1.2 MB | 48 KB/s | 2m 34s      │
└─────────────────────────────────────────┘
```

*P95 is not currently tracked by SessionTelemetry (only min/avg/max).
We display min/max range instead.

**Data source**: All fields come from `SessionTelemetry::snapshot()` which
is already sent every 2 seconds via `ServerMessage::Telemetry`:

| UI Field | `snapshot()` key | Description |
|---|---|---|
| TTFB Last | `last_response_latency_ms` | VAD end -> first audio byte |
| TTFB Avg | `avg_response_latency_ms` | Running average across turns |
| TTFB Min | `min_response_latency_ms` | Best-case latency |
| TTFB Max | `max_response_latency_ms` | Worst-case latency |
| Turn Avg | `avg_turn_duration_ms` | Avg turn-start to turn-complete |
| Turn Count | `response_count` | Number of measured responses |
| Interruptions | `interruptions` | Barge-in count |
| Audio KB | `audio_kbytes_out` | Total audio sent |
| Audio KB/s | `audio_throughput_kbps` | Throughput over session |
| Uptime | `uptime_secs` | Session duration |

**Color coding for TTFB**:
- Green: < 300ms (good)
- Orange: 300-600ms (acceptable)
- Red: > 600ms (needs investigation)

### Tab Availability Logic (updated)

```javascript
// Always shown
tabs.push('state', 'events', 'nfr');

// Shown for apps with phase-machine or advanced/showcase category
if (hasPhases) tabs.push('playbook');

// Shown for apps with guardrails/evaluation features
if (hasEvaluation) tabs.push('evaluator');
```

---

## File Change Summary

### New Files

| File | Workstream |
|------|-----------|
| `apps/adk-web/static/worklets/capture-processor.js` | A |
| `apps/adk-web/static/worklets/playback-processor.js` | A |

### Modified Files

| File | Workstream | Changes |
|------|-----------|---------|
| `apps/adk-web/static/audio.js` | A | Rewrite: AudioWorklet + fallback |
| `apps/adk-web/static/devtools.js` | C | Remove telemetry tab, add status bar |
| `apps/adk-web/static/devtools.css` | C | Remove telemetry styles, add status bar styles |
| `apps/adk-web/static/app.html` | C | Add status bar container |
| `crates/rs-genai/Cargo.toml` | B | Add optional otel dependencies |
| `crates/rs-genai/src/telemetry/mod.rs` | B | OTel layer init in TelemetryConfig |
| `crates/rs-adk/Cargo.toml` | B | Forward otel feature flag |

### Unchanged

- All Rust app files (`call_screening.rs`, `clinic.rs`, etc.) — no changes
  needed. They already send telemetry via `ServerMessage::Telemetry` which
  the devtools status bar consumes for uptime/phase/turns.
- Existing tracing span definitions — bridged automatically
- Existing Prometheus metric definitions — bridged to OTel metrics

---

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| AudioWorklet not supported (old browsers) | Feature detection + ScriptProcessorNode fallback |
| OTel dependency weight | Feature-gated behind `otel` flag, not in default features |
| Google Cloud auth for OTel export | Reuses existing Vertex AI credentials (same project) |
| Breaking existing telemetry consumers | `SessionTelemetry::snapshot()` unchanged; only UI tab removed |
