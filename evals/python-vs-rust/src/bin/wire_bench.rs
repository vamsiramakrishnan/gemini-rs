//! N concurrent sessions against `python/mock_server.py` over real WebSockets.
//! `--layer l0`: gemini-genai-rs SessionHandle only (same scope as google-genai).
//! `--layer l1`: plus the gemini-adk-rs control plane (attach_session).
//!
//! The Python counterpart is `python/wire_bench.py`; both print one JSON line.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gemini_adk_rs::live::{LiveEvent, LiveSessionBuilder, attach_session};
use gemini_genai_rs::prelude::*;

/// Process user+system CPU seconds from /proc/self/stat (100 Hz ticks).
fn cpu_s() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
}

static EVENT_CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GAPS: std::sync::Mutex<Vec<f64>> = std::sync::Mutex::new(Vec::new());

fn gap(last: &mut Option<Instant>, gaps: &mut Vec<f64>) {
    let now = Instant::now();
    if let Some(prev) = last.replace(now) {
        gaps.push(now.duration_since(prev).as_secs_f64() * 1000.0);
    }
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

enum Held {
    L0(SessionHandle),
    L1(gemini_adk_rs::live::LiveHandle),
}

async fn open(url: String, l1: bool, prompt: String, work_ms: u64) -> (Held, f64) {
    // Naive inline app work, the same as the Python harness: spins on a worker.
    let worker = (work_ms > 0).then(|| {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(work_ms) {
                    std::hint::spin_loop();
                }
            }
        })
    });
    let out = open_inner(url, l1, prompt).await;
    if let Some(w) = worker {
        w.abort();
    }
    out
}

async fn open_inner(url: String, l1: bool, prompt: String) -> (Held, f64) {
    let config =
        SessionConfig::new("bench").model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
    let tc = TransportConfig {
        max_reconnect_attempts: 0,
        connect_timeout_secs: 60,
        setup_timeout_secs: 60,
        ..TransportConfig::default()
    };
    let session = ConnectBuilder::new(config.clone())
        .transport_config(tc)
        .transport(LocalWs {
            inner: TungsteniteTransport::new(),
            url,
        })
        .connect()
        .await
        .expect("connect");
    if l1 {
        let mut b = LiveSessionBuilder::new(config);
        let cap = EVENT_CAP.load(Ordering::Relaxed);
        if cap > 0 {
            b = b.event_capacity(cap);
        }
        let live = attach_session(b, session).await.expect("attach");
        let mut events = live.events();
        let (mut last, mut gaps) = (None, Vec::new());
        let t0 = Instant::now();
        live.send_text(prompt).await.expect("send");
        loop {
            match tokio::time::timeout(Duration::from_secs(60), events.recv()).await {
                Ok(Ok(LiveEvent::TurnComplete)) => break,
                Ok(Ok(LiveEvent::Audio(_))) => {
                    FRAMES.fetch_add(1, Ordering::Relaxed);
                    gap(&mut last, &mut gaps);
                }
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                other => panic!("turn failed: {other:?}"),
            }
        }
        GAPS.lock().unwrap().extend(gaps);
        (Held::L1(live), t0.elapsed().as_secs_f64() * 1000.0)
    } else {
        session.wait_for_phase(SessionPhase::Active).await;
        let mut events = session.subscribe();
        let (mut last, mut gaps) = (None, Vec::new());
        let t0 = Instant::now();
        session.send_text(prompt).await.expect("send");
        loop {
            match tokio::time::timeout(Duration::from_secs(60), events.recv()).await {
                Ok(Ok(SessionEvent::TurnComplete)) => break,
                Ok(Ok(SessionEvent::AudioData(_))) => {
                    FRAMES.fetch_add(1, Ordering::Relaxed);
                    gap(&mut last, &mut gaps);
                }
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                other => panic!("turn failed: {other:?}"),
            }
        }
        GAPS.lock().unwrap().extend(gaps);
        (Held::L0(session), t0.elapsed().as_secs_f64() * 1000.0)
    }
}

fn env_usize(k: &str) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn main() {
    let workers = env_usize("RSBENCH_WORKERS");
    let mut b = tokio::runtime::Builder::new_multi_thread();
    if workers > 0 {
        b.worker_threads(workers);
    }
    b.enable_all().build().unwrap().block_on(amain());
}

async fn amain() {
    let rss_start = rss_kb();
    let mut sessions = 100usize;
    let mut port = 8765u16;
    let mut l1 = false;
    let mut audio_secs = 0f64;
    let mut work_ms = 0u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sessions" => sessions = args.next().unwrap().parse().unwrap(),
            "--port" => port = args.next().unwrap().parse().unwrap(),
            "--layer" => l1 = args.next().unwrap() == "l1",
            "--event-capacity" => {
                EVENT_CAP.store(args.next().unwrap().parse().unwrap(), Ordering::Relaxed)
            }
            "--work-ms" => work_ms = args.next().unwrap().parse().unwrap(),
            "--audio-secs" => audio_secs = args.next().unwrap().parse().unwrap(),
            _ => panic!("unknown arg {a}"),
        }
    }
    let peak = Arc::new(AtomicU64::new(rss_start));
    let p = peak.clone();
    let sampler = tokio::spawn(async move {
        loop {
            p.fetch_max(rss_kb(), Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let url = format!("ws://127.0.0.1:{port}/");
    let prompt = if audio_secs > 0.0 {
        format!("stream:{audio_secs}")
    } else {
        "hi".to_string()
    };
    let t0 = Instant::now();
    let c0 = cpu_s();
    let shards = env_usize("RSBENCH_SHARDS");
    let mut held = Vec::with_capacity(sessions);
    let mut turn_ms = Vec::with_capacity(sessions);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut joins = Vec::new();
    if shards == 0 {
        let tasks: Vec<_> = (0..sessions)
            .map(|_| tokio::spawn(open(url.clone(), l1, prompt.clone(), work_ms)))
            .collect();
        for t in tasks {
            let (h, ms) = t.await.unwrap();
            held.push(h);
            turn_ms.push(ms);
        }
    } else {
        // Thread-per-core: one current-thread runtime per shard, sessions
        // round-robin, so a session's tasks never hop threads.
        let (tx, rx) = std::sync::mpsc::channel::<f64>();
        for k in 0..shards {
            let n = sessions / shards + usize::from(k < sessions % shards);
            let (tx, url, prompt, mut stop) =
                (tx.clone(), url.clone(), prompt.clone(), stop_rx.clone());
            joins.push(std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let tasks: Vec<_> = (0..n)
                        .map(|_| tokio::spawn(open(url.clone(), l1, prompt.clone(), work_ms)))
                        .collect();
                    let mut held = Vec::with_capacity(n);
                    for t in tasks {
                        let (h, ms) = t.await.unwrap();
                        held.push(h);
                        tx.send(ms).unwrap();
                    }
                    let _ = stop.wait_for(|v| *v).await;
                    drop(held);
                });
            }));
        }
        drop(tx);
        turn_ms = tokio::task::spawn_blocking(move || rx.iter().take(sessions).collect())
            .await
            .unwrap();
    }
    let ready_s = t0.elapsed().as_secs_f64();
    let cpu = cpu_s() - c0;
    let idle_secs = env_usize("RSBENCH_IDLE_SECS");
    let mut idle_cpu = 0.0;
    if idle_secs > 0 {
        let c1 = cpu_s();
        tokio::time::sleep(Duration::from_secs(idle_secs as u64)).await;
        idle_cpu = cpu_s() - c1;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let idle = rss_kb();
    sampler.abort();
    turn_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut g = std::mem::take(&mut *GAPS.lock().unwrap());
    g.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let gq = |q: f64| {
        if g.is_empty() {
            0.0
        } else {
            (g[((q * g.len() as f64) as usize).min(g.len() - 1)] * 10.0).round() / 10.0
        }
    };
    let late = if g.is_empty() {
        0.0
    } else {
        (10000.0 * g.iter().filter(|x| **x > 60.0).count() as f64 / g.len() as f64).round() / 100.0
    };
    let pct = |q: f64| turn_ms[((q * turn_ms.len() as f64) as usize).min(turn_ms.len() - 1)];
    println!(
        "{}",
        serde_json::json!({
            "impl": if l1 { "rust L1 (genai-rs + adk-rs control plane)" } else { "rust L0 (gemini-genai-rs)" },
            "sessions": sessions,
            "workers": env_usize("RSBENCH_WORKERS"),
            "shards": shards,
            "idle_cpu_pct_of_one_core": (1000.0 * idle_cpu / idle_secs.max(1) as f64).round() / 10.0,
            "event_capacity": EVENT_CAP.load(Ordering::Relaxed),
            "rss_start_kb": rss_start,
            "rss_idle_kb": idle,
            "rss_peak_kb": peak.load(Ordering::Relaxed),
            "per_session_kb": ((idle - rss_start) as f64 / sessions as f64 * 10.0).round() / 10.0,
            "all_connected_s": (ready_s * 100.0).round() / 100.0,
            "audio_secs": audio_secs,
            "work_ms": work_ms,
            "audio_frames": FRAMES.load(Ordering::Relaxed),
            "cpu_s": (cpu * 100.0).round() / 100.0,
            "gap_p50_ms": gq(0.5),
            "gap_p99_ms": gq(0.99),
            "gap_p999_ms": gq(0.999),
            "gap_max_ms": g.last().map(|x| (x * 10.0).round() / 10.0).unwrap_or(0.0),
            "late_over_60ms_pct": late,
            "cpu_pct_of_one_core": (1000.0 * cpu / ready_s).round() / 10.0,
            "turn_ms_p50": (pct(0.5) * 10.0).round() / 10.0,
            "turn_ms_p99": (pct(0.99) * 10.0).round() / 10.0,
        })
    );
    let _ = stop_tx.send(true);
    for j in joins {
        let _ = j.join();
    }
    for h in held {
        match h {
            Held::L0(s) => {
                let _ = s.disconnect().await;
            }
            Held::L1(l) => {
                let _ = l.disconnect().await;
            }
        }
    }
}
