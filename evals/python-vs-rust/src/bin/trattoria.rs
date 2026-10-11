//! The trattoria booking agent, built from its spec, taking calls on N phone
//! lines against `trattoria_mock`.
//!
//! Each line takes calls back to back until the run ends. A call is the
//! agent from `crates/gemini-adk-fluent-rs/tests/fixtures/live/trattoria.json`
//! as `SessionSpec::apply` builds it (stages, tool gate, commit gate,
//! extractors, verbatim check, repair, the handoff overlay, `contextUpdate`
//! steering), connected over a real WebSocket, with its extraction calls going
//! through `GeminiLlm` to the stand-in's REST endpoint. The caller's mic is a
//! 20 ms clock of 16 kHz audio that runs for the whole call; frames carry the
//! time they were captured. Model audio goes to an audio callback that only
//! notes how late each frame arrived. The caller hangs up a second after the
//! turn that follows the booking.
//!
//! Prints one JSON line. `python/trattoria.py` is the same agent in Python.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_rs::State;
use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRegistry};
use gemini_genai_rs::prelude::*;
use serde_json::{Value, json};
use tokio::sync::Notify;

const SPEC: &str =
    include_str!("../../../../crates/gemini-adk-fluent-rs/tests/fixtures/live/trattoria.json");
/// 20 ms of 16 kHz mono PCM16.
const MIC_FRAME: Duration = Duration::from_millis(20);
const MIC_FRAME_BYTES: usize = 640;

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Process user+system CPU seconds (100 Hz ticks).
fn cpu_s() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|r| r.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0)
}

/// A WebSocket to the stand-in, whatever URL the session asks for.
struct LocalWs {
    inner: TungsteniteTransport,
    url: String,
}

#[async_trait::async_trait]
impl Transport for LocalWs {
    type Error = TungsteniteError;
    async fn connect(&mut self, _url: &str, _h: Vec<(String, String)>) -> Result<(), Self::Error> {
        self.inner.connect(&self.url, Vec::new()).await
    }
    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
        self.inner.send(data).await
    }
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        self.inner.recv().await
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inner.close().await
    }
    fn close_reason(&self) -> Option<String> {
        self.inner.close_reason()
    }
}

#[derive(Default)]
struct Totals {
    calls: AtomicU64,
    booked: AtomicU64,
    failed: AtomicU64,
    audio_frames: AtomicU64,
    /// How late model audio reached the callback, in microseconds.
    audio_late: parking_lot::Mutex<Vec<u32>>,
    errors: parking_lot::Mutex<Vec<String>>,
}

struct Shared {
    spec: SessionSpec,
    resources: SpecResources,
    url: String,
    totals: Totals,
}

/// One call: connect, stream the mic, hang up after the booking.
async fn call(shared: Arc<Shared>) -> Result<bool, String> {
    let state = State::new();
    let turn_done = Arc::new(Notify::new());
    let gone = Arc::new(Notify::new());
    let late = Arc::new(parking_lot::Mutex::new(Vec::<u32>::with_capacity(512)));

    let live = shared
        .spec
        .apply(Live::builder(), &state, &shared.resources)?
        .model(ModelId::new("models/gemini-3.8-live".to_string()))
        .transcription();
    let (t, g, l) = (turn_done.clone(), gone.clone(), late.clone());
    let live = live
        .on_audio(move |bytes| {
            if let Some(head) = bytes.get(..8) {
                let sent = u64::from_le_bytes(head.try_into().unwrap_or_default());
                l.lock().push(now_us().saturating_sub(sent) as u32);
            }
        })
        .on_turn_complete(move || {
            let t = t.clone();
            async move { t.notify_one() }
        })
        .on_disconnected(move |_| {
            let g = g.clone();
            async move { g.notify_one() }
        });
    let handle = live
        .connect_with_transport(LocalWs {
            inner: TungsteniteTransport::new(),
            url: shared.url.clone(),
        })
        .await
        .map_err(|e| format!("connect: {e}"))?;

    // The caller's mic: a frame every 20 ms, stamped with its capture time.
    // A late tick sends every frame it owes, as an audio device's buffer would.
    let mic_handle = handle.clone();
    let mic = tokio::spawn(async move {
        let mut tick = tokio::time::interval(MIC_FRAME);
        let start_us = now_us();
        let mut n = 0u64;
        let mut frame = vec![0u8; MIC_FRAME_BYTES];
        for (i, b) in frame.iter_mut().enumerate().skip(8) {
            *b = (i % 13) as u8;
        }
        loop {
            tick.tick().await;
            let captured = start_us + n * MIC_FRAME.as_micros() as u64;
            n += 1;
            frame[..8].copy_from_slice(&captured.to_le_bytes());
            if mic_handle.send_audio(frame.clone()).await.is_err() {
                break;
            }
        }
    });

    let outcome = async {
        loop {
            tokio::select! {
                _ = turn_done.notified() => {
                    if state.contains("booking") {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        return Ok(true);
                    }
                }
                _ = gone.notified() => return Ok(false),
                _ = tokio::time::sleep(Duration::from_secs(120)) => {
                    return Err("call timed out".to_string());
                }
            }
        }
    }
    .await;
    mic.abort();
    let _ = handle.disconnect().await;
    let late = std::mem::take(&mut *late.lock());
    shared
        .totals
        .audio_frames
        .fetch_add(late.len() as u64, Ordering::Relaxed);
    shared.totals.audio_late.lock().extend(late);
    outcome
}

async fn line(shared: Arc<Shared>, start_after: Duration, until: Instant) {
    tokio::time::sleep(start_after).await;
    while Instant::now() < until {
        shared.totals.calls.fetch_add(1, Ordering::Relaxed);
        match call(shared.clone()).await {
            Ok(true) => {
                shared.totals.booked.fetch_add(1, Ordering::Relaxed);
            }
            Ok(false) => {
                shared.totals.failed.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                shared.totals.failed.fetch_add(1, Ordering::Relaxed);
                shared.totals.errors.lock().push(e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn quantiles_ms(v: &mut [u32]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_unstable();
    let q = |p: f64| {
        let us = v[((p * v.len() as f64) as usize).min(v.len() - 1)];
        (us as f64 / 100.0).round() / 10.0
    };
    json!({ "n": v.len(), "p50": q(0.5), "p99": q(0.99), "p999": q(0.999), "max": q(1.0) })
}

fn main() {
    let workers = std::env::var("TOKIO_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut b = tokio::runtime::Builder::new_multi_thread();
    if workers > 0 {
        b.worker_threads(workers);
    }
    b.enable_all()
        .build()
        .expect("runtime")
        .block_on(amain(workers));
}

async fn amain(workers: usize) {
    let rss_start = rss_kb();
    let (mut lines, mut secs, mut port, mut ramp) = (10usize, 60u64, 8766u16, 30u64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = || args.next().expect("value").parse::<u64>().expect("number");
        match a.as_str() {
            "--lines" => lines = num() as usize,
            "--secs" => secs = num(),
            "--port" => port = num() as u16,
            "--ramp-secs" => ramp = num(),
            other => panic!("unknown argument {other}"),
        }
    }

    // Extraction goes through GeminiLlm to the stand-in, one client for
    // every call, as an application would share it.
    let extraction: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
        model: Some("gemini-3.5-flash-lite".into()),
        api_key: Some("bench".into()),
        vertexai: Some(false),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        ..Default::default()
    }));
    let mut registry = LlmRegistry::new();
    registry.register("", move |_| extraction.clone());
    let resources = SpecResources {
        models: Some(Arc::new(registry)),
        ..Default::default()
    };
    let doc: Value = serde_json::from_str(SPEC).expect("spec JSON");
    let spec = SessionSpec::from_value(doc).expect("spec");
    let shared = Arc::new(Shared {
        spec,
        resources,
        url: format!("ws://127.0.0.1:{port}/ws/live"),
        totals: Totals::default(),
    });

    let peak = Arc::new(AtomicU64::new(rss_start));
    let p = peak.clone();
    let sampler = tokio::spawn(async move {
        loop {
            p.fetch_max(rss_kb(), Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let t0 = Instant::now();
    let c0 = cpu_s();
    let until = t0 + Duration::from_secs(secs);
    let tasks: Vec<_> = (0..lines)
        .map(|i| {
            let offset = Duration::from_millis(ramp * 1000 * i as u64 / lines.max(1) as u64);
            tokio::spawn(line(shared.clone(), offset, until))
        })
        .collect();
    for t in tasks {
        let _ = t.await;
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu = cpu_s() - c0;
    sampler.abort();

    let totals = &shared.totals;
    let mut late = std::mem::take(&mut *totals.audio_late.lock());
    let errors = totals.errors.lock().clone();
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    for e in errors {
        *kinds.entry(e).or_default() += 1;
    }
    let peak = peak.load(Ordering::Relaxed);
    println!(
        "{}",
        json!({
            "impl": "rust (gemini-adk-fluent-rs spec)",
            "lines": lines,
            "secs": secs,
            "tokio_workers": workers,
            "calls": totals.calls.load(Ordering::Relaxed),
            "booked": totals.booked.load(Ordering::Relaxed),
            "failed": totals.failed.load(Ordering::Relaxed),
            "errors": kinds,
            "wall_s": (wall * 100.0).round() / 100.0,
            "cpu_s": (cpu * 100.0).round() / 100.0,
            "cpu_pct_of_one_core": (1000.0 * cpu / wall).round() / 10.0,
            "rss_start_kb": rss_start,
            "rss_peak_kb": peak,
            "per_line_kb": ((peak - rss_start) as f64 / lines as f64 * 10.0).round() / 10.0,
            "audio_frames": totals.audio_frames.load(Ordering::Relaxed),
            "audio_late_ms": quantiles_ms(&mut late),
        })
    );
}
