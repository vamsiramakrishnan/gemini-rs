//! Concurrent-session capacity harness.
//!
//! Opens `N` Live sessions in one process, each running the real L1 control
//! plane (router, fast lane, control lane, telemetry lane) over a
//! [`ScriptedTransport`] that answers every user turn with a fixed model
//! reply after a configurable delay. No model is called and no socket is
//! opened, so the numbers isolate what the runtime itself costs:
//!
//! - **memory**: resident set before, after connecting all sessions, at peak,
//!   and after disconnecting, from `/proc/self/status` (Linux; elsewhere the
//!   RSS fields are reported as unavailable);
//! - **turn latency**: wall clock from `send_text` to the first `TextDelta`
//!   and to `TurnComplete`, as seen by the application through
//!   [`LiveHandle::events`], minus nothing — the scripted delay is part of it
//!   by design, so a run with `response_delay = 0` measures pure runtime
//!   overhead;
//! - **conformance**: the runtime's own [`SessionTelemetry`] must have counted
//!   the same turns the harness drove.
//!
//! The harness is model-free in the sense AGENTS.md means it: the transport
//! is substituted, everything above it is production code.
//!
//! [`SessionTelemetry`]: gemini_adk_rs::live::SessionTelemetry

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use gemini_adk_rs::live::{LiveEvent, LiveHandle, LiveSessionBuilder, attach_session};
use gemini_genai_rs::prelude::{ModelId, SessionConfig};
use gemini_genai_rs::transport::{ConnectBuilder, Transport, TransportConfig};

// ---------------------------------------------------------------------------
// Scripted transport
// ---------------------------------------------------------------------------

/// Error type of [`ScriptedTransport`].
#[derive(Debug)]
pub enum ScriptedError {
    /// Operation attempted while not connected.
    NotConnected,
}

impl std::fmt::Display for ScriptedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str("not connected"),
        }
    }
}

impl std::error::Error for ScriptedError {}

/// A [`Transport`] that plays the server side of a Live session from a script:
/// `setupComplete` on connect, and one text `modelTurn` with `turnComplete`
/// in reply to every `clientContent` the application sends, `response_delay`
/// after the send.
///
/// Unlike [`ReplayTransport`](gemini_genai_rs::transport::ReplayTransport),
/// which streams a fixed recording, this transport reacts to what the session
/// sends, so a turn's latency is measured end to end: application send → L0
/// encode → transport → scripted reply → L0 decode → L1 lanes → application
/// event.
pub struct ScriptedTransport {
    inbound_tx: mpsc::UnboundedSender<Vec<u8>>,
    inbound_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    response_delay: Duration,
    reply: Arc<[u8]>,
    connected: bool,
}

impl ScriptedTransport {
    /// Reply to every user turn with `reply_text`, `response_delay` after it
    /// was sent.
    pub fn new(reply_text: &str, response_delay: Duration) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let frame = serde_json::json!({
            "serverContent": {
                "modelTurn": {"parts": [{"text": reply_text}]},
                "turnComplete": true
            }
        });
        Self {
            inbound_tx,
            inbound_rx,
            response_delay,
            reply: serde_json::to_vec(&frame)
                .expect("static JSON encodes")
                .into(),
            connected: false,
        }
    }
}

#[async_trait]
impl Transport for ScriptedTransport {
    type Error = ScriptedError;

    async fn connect(
        &mut self,
        _url: &str,
        _headers: Vec<(String, String)>,
    ) -> Result<(), Self::Error> {
        self.connected = true;
        // The handshake reply. The session loop sends the setup frame right
        // after connect; answering unconditionally keeps the transport free of
        // any dependency on the setup encoding.
        let _ = self.inbound_tx.send(br#"{"setupComplete":{}}"#.to_vec());
        Ok(())
    }

    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
        if !self.connected {
            return Err(ScriptedError::NotConnected);
        }
        // Only user turns get a reply; the setup frame, tool responses and
        // realtime input are accepted and dropped, as a quiet server would.
        if memmem(&data, b"\"clientContent\"") {
            let tx = self.inbound_tx.clone();
            let reply = self.reply.clone();
            let delay = self.response_delay;
            tokio::spawn(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let _ = tx.send(reply.to_vec());
            });
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        if !self.connected {
            return Err(ScriptedError::NotConnected);
        }
        // Pends while nothing is scripted, like `MockTransport`; the sender
        // half lives in `self`, so the channel never closes while connected.
        Ok(self.inbound_rx.recv().await)
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.connected = false;
        Ok(())
    }
}

fn memmem(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Configuration and report
// ---------------------------------------------------------------------------

/// What to run.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Number of sessions to hold open concurrently.
    pub sessions: usize,
    /// Text turns driven through each session.
    pub turns: usize,
    /// Pause between one session's turns (a caller thinking).
    pub think: Duration,
    /// Scripted model latency: delay between the user turn and the reply.
    pub response_delay: Duration,
    /// How long to hold every session open, idle, after all have connected
    /// and before the first turn. Exposes idle cost and slow leaks.
    pub hold: Duration,
    /// Give up on a turn that produces no `TurnComplete` within this long.
    pub turn_timeout: Duration,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            sessions: 10,
            turns: 5,
            think: Duration::from_millis(50),
            response_delay: Duration::from_millis(20),
            hold: Duration::ZERO,
            turn_timeout: Duration::from_secs(10),
        }
    }
}

/// Percentiles of a sample set, in milliseconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Percentiles {
    /// Sample count.
    pub count: usize,
    /// Median.
    pub p50_ms: f64,
    /// 90th percentile.
    pub p90_ms: f64,
    /// 99th percentile.
    pub p99_ms: f64,
    /// Largest sample.
    pub max_ms: f64,
    /// Mean.
    pub mean_ms: f64,
}

impl Percentiles {
    /// Nearest-rank percentiles over `samples` (any order).
    pub fn of(mut samples: Vec<f64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN latencies"));
        let count = samples.len();
        // Microsecond resolution: readable JSON, and nothing below the timer
        // granularity pretends to be signal.
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        let rank = |p: f64| {
            let idx = ((p / 100.0) * count as f64).ceil() as usize;
            round(samples[idx.clamp(1, count) - 1])
        };
        Self {
            count,
            p50_ms: rank(50.0),
            p90_ms: rank(90.0),
            p99_ms: rank(99.0),
            max_ms: round(samples[count - 1]),
            mean_ms: round(samples.iter().sum::<f64>() / count as f64),
        }
    }
}

/// Resident-set samples around the run, in kilobytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RssReport {
    /// Whether the platform exposed RSS at all (`/proc/self/status`).
    pub available: bool,
    /// Before the first session is built.
    pub baseline_kb: u64,
    /// Once every session is connected and idle.
    pub after_connect_kb: u64,
    /// Highest sample seen at any point in the run.
    pub peak_kb: u64,
    /// After every session has disconnected and lanes have joined.
    pub after_disconnect_kb: u64,
    /// `(after_connect - baseline) / sessions`: the marginal cost of one idle
    /// session, including its share of allocator slack.
    pub per_session_kb: u64,
}

/// One run's result. Serialized as the JSON the CLI writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    /// Sessions held concurrently.
    pub sessions: usize,
    /// Turns driven per session.
    pub turns_per_session: usize,
    /// Turns that reached `TurnComplete` within the timeout, over all sessions.
    pub turns_completed: usize,
    /// Turns that timed out or whose session failed.
    pub turns_failed: usize,
    /// Scripted model delay, so a reader can subtract it.
    pub response_delay_ms: u64,
    /// Time for `connect` + `attach_session`, per session.
    pub connect_ms: Percentiles,
    /// `send_text` → first `TextDelta` seen by the application.
    pub first_text_ms: Percentiles,
    /// `send_text` → `TurnComplete` seen by the application.
    pub turn_ms: Percentiles,
    /// `send_text` → first model text, as the runtime's own telemetry saw it,
    /// summed over sessions. Must equal `turns_completed` for the run to be
    /// trusted.
    pub telemetry_response_count: u64,
    /// Memory.
    pub rss: RssReport,
    /// Whole run, from baseline RSS sample to the last disconnect.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Resident set size in kB from `/proc/self/status`, or `None` off Linux.
pub fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

struct Peak {
    max_kb: AtomicU64,
}

impl Peak {
    fn observe(&self) {
        if let Some(kb) = rss_kb() {
            self.max_kb.fetch_max(kb, Ordering::Relaxed);
        }
    }
}

struct SessionOutcome {
    connect_ms: f64,
    first_text_ms: Vec<f64>,
    turn_ms: Vec<f64>,
    failed_turns: usize,
    telemetry_responses: u64,
}

async fn open_session(
    response_delay: Duration,
) -> Result<LiveHandle, gemini_adk_rs::error::AgentError> {
    let config = SessionConfig::new("session-bench").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
    let transport_config = TransportConfig {
        max_reconnect_attempts: 0,
        connect_timeout_secs: 5,
        setup_timeout_secs: 5,
        ..TransportConfig::default()
    };
    let transport = ScriptedTransport::new("Understood.", response_delay);
    let session = ConnectBuilder::new(config.clone())
        .transport_config(transport_config)
        .transport(transport)
        .connect()
        .await
        .map_err(gemini_adk_rs::error::AgentError::Session)?;
    attach_session(LiveSessionBuilder::new(config), session).await
}

/// Drive `turns` text turns through one connected session.
async fn drive(handle: &LiveHandle, cfg: &BenchConfig, out: &mut SessionOutcome) {
    let mut events = handle.events();
    for i in 0..cfg.turns {
        let started = Instant::now();
        if handle.send_text(format!("turn {i}")).await.is_err() {
            out.failed_turns += 1;
            continue;
        }
        // A turn is over when BOTH lanes have delivered it: `TextComplete`
        // from the fast lane and `TurnComplete` from the control lane. The two
        // are ordered within a lane, not across lanes, so waiting for either
        // alone lets the next `send_text` race the previous turn's text and
        // mis-attributes the first delta (and the runtime's own text latency)
        // to the wrong turn.
        let mut first_text: Option<f64> = None;
        let mut text_done = false;
        let mut turn_done = false;
        let mut turn_ms: Option<f64> = None;
        let deadline = started + cfg.turn_timeout;
        let completed = loop {
            if text_done && turn_done {
                break true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break false;
            }
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Ok(LiveEvent::TextDelta(_))) => {
                    first_text.get_or_insert_with(|| ms_since(started));
                }
                Ok(Ok(LiveEvent::TextComplete(_))) => {
                    first_text.get_or_insert_with(|| ms_since(started));
                    text_done = true;
                }
                Ok(Ok(LiveEvent::TurnComplete)) => {
                    turn_ms.get_or_insert_with(|| ms_since(started));
                    turn_done = true;
                }
                Ok(Ok(LiveEvent::Disconnected { .. } | LiveEvent::Error(_))) => break false,
                Ok(Ok(_)) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break false,
                Err(_) => break false,
            }
        };
        if completed {
            out.turn_ms
                .push(turn_ms.unwrap_or_else(|| ms_since(started)));
            if let Some(ms) = first_text {
                out.first_text_ms.push(ms);
            }
        } else {
            out.failed_turns += 1;
        }
        if !cfg.think.is_zero() {
            tokio::time::sleep(cfg.think).await;
        }
    }
    out.telemetry_responses = handle.telemetry().latency().count;
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Run the harness once and return its report.
pub async fn run(cfg: BenchConfig) -> Report {
    let run_started = Instant::now();
    let rss_available = rss_kb().is_some();
    let baseline_kb = rss_kb().unwrap_or(0);

    let peak = Arc::new(Peak {
        max_kb: AtomicU64::new(baseline_kb),
    });
    let sampler = {
        let peak = peak.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tick.tick().await;
                peak.observe();
            }
        })
    };

    // Connect everything concurrently; each session is one real L1 stack.
    let mut connects = JoinSet::new();
    for _ in 0..cfg.sessions {
        let delay = cfg.response_delay;
        connects.spawn(async move {
            let started = Instant::now();
            let handle = open_session(delay).await;
            (ms_since(started), handle)
        });
    }
    let mut handles = Vec::with_capacity(cfg.sessions);
    let mut connect_ms = Vec::with_capacity(cfg.sessions);
    let mut failed_sessions = 0usize;
    while let Some(joined) = connects.join_next().await {
        match joined {
            Ok((ms, Ok(handle))) => {
                connect_ms.push(ms);
                handles.push(handle);
            }
            _ => failed_sessions += 1,
        }
    }

    peak.observe();
    let after_connect_kb = rss_kb().unwrap_or(0);

    if !cfg.hold.is_zero() {
        tokio::time::sleep(cfg.hold).await;
        peak.observe();
    }

    // Drive every session's turns concurrently.
    let mut drives = JoinSet::new();
    for (handle, connect_ms) in handles.iter().cloned().zip(connect_ms) {
        let cfg = cfg.clone();
        drives.spawn(async move {
            let mut out = SessionOutcome {
                connect_ms,
                first_text_ms: Vec::new(),
                turn_ms: Vec::new(),
                failed_turns: 0,
                telemetry_responses: 0,
            };
            drive(&handle, &cfg, &mut out).await;
            out
        });
    }
    let mut outcomes = Vec::with_capacity(handles.len());
    while let Some(joined) = drives.join_next().await {
        if let Ok(out) = joined {
            outcomes.push(out);
        }
    }
    peak.observe();

    for handle in &handles {
        let _ = handle.disconnect().await;
    }
    drop(handles);
    tokio::task::yield_now().await;
    peak.observe();
    let after_disconnect_kb = rss_kb().unwrap_or(0);
    sampler.abort();

    let turns_completed: usize = outcomes.iter().map(|o| o.turn_ms.len()).sum();
    let turns_failed: usize =
        outcomes.iter().map(|o| o.failed_turns).sum::<usize>() + failed_sessions * cfg.turns;
    let per_session_kb = if cfg.sessions == 0 {
        0
    } else {
        after_connect_kb.saturating_sub(baseline_kb) / cfg.sessions as u64
    };

    Report {
        sessions: cfg.sessions,
        turns_per_session: cfg.turns,
        turns_completed,
        turns_failed,
        response_delay_ms: cfg.response_delay.as_millis() as u64,
        connect_ms: Percentiles::of(outcomes.iter().map(|o| o.connect_ms).collect()),
        first_text_ms: Percentiles::of(
            outcomes
                .iter()
                .flat_map(|o| o.first_text_ms.iter().copied())
                .collect(),
        ),
        turn_ms: Percentiles::of(
            outcomes
                .iter()
                .flat_map(|o| o.turn_ms.iter().copied())
                .collect(),
        ),
        telemetry_response_count: outcomes.iter().map(|o| o.telemetry_responses).sum(),
        rss: RssReport {
            available: rss_available,
            baseline_kb,
            after_connect_kb,
            peak_kb: peak.max_kb.load(Ordering::Relaxed),
            after_disconnect_kb,
            per_session_kb,
        },
        elapsed_ms: run_started.elapsed().as_millis() as u64,
    }
}

impl Report {
    /// A few lines for a terminal.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "sessions {}  turns/session {}  completed {}  failed {}  telemetry counted {}\n",
            self.sessions,
            self.turns_per_session,
            self.turns_completed,
            self.turns_failed,
            self.telemetry_response_count
        ));
        s.push_str(&format!(
            "connect     p50 {:.1} ms  p90 {:.1}  p99 {:.1}  max {:.1}\n",
            self.connect_ms.p50_ms,
            self.connect_ms.p90_ms,
            self.connect_ms.p99_ms,
            self.connect_ms.max_ms
        ));
        s.push_str(&format!(
            "first text  p50 {:.1} ms  p90 {:.1}  p99 {:.1}  max {:.1}  (scripted delay {} ms)\n",
            self.first_text_ms.p50_ms,
            self.first_text_ms.p90_ms,
            self.first_text_ms.p99_ms,
            self.first_text_ms.max_ms,
            self.response_delay_ms
        ));
        s.push_str(&format!(
            "turn        p50 {:.1} ms  p90 {:.1}  p99 {:.1}  max {:.1}\n",
            self.turn_ms.p50_ms, self.turn_ms.p90_ms, self.turn_ms.p99_ms, self.turn_ms.max_ms
        ));
        if self.rss.available {
            s.push_str(&format!(
                "rss         baseline {} kB  connected {} kB  peak {} kB  after {} kB  per session {} kB\n",
                self.rss.baseline_kb,
                self.rss.after_connect_kb,
                self.rss.peak_kb,
                self.rss.after_disconnect_kb,
                self.rss.per_session_kb
            ));
        } else {
            s.push_str("rss         unavailable on this platform\n");
        }
        s.push_str(&format!("elapsed     {} ms\n", self.elapsed_ms));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_nearest_rank() {
        let p = Percentiles::of((1..=100).map(f64::from).collect());
        assert_eq!(p.count, 100);
        assert_eq!(p.p50_ms, 50.0);
        assert_eq!(p.p90_ms, 90.0);
        assert_eq!(p.p99_ms, 99.0);
        assert_eq!(p.max_ms, 100.0);
        assert_eq!(p.mean_ms, 50.5);
        assert_eq!(Percentiles::of(vec![]), Percentiles::default());
    }

    #[tokio::test]
    async fn scripted_transport_answers_client_content_only() {
        let mut t = ScriptedTransport::new("hi", Duration::ZERO);
        t.connect("ws://scripted", vec![]).await.unwrap();
        let setup = t.recv().await.unwrap().unwrap();
        assert!(memmem(&setup, b"setupComplete"));

        t.send(br#"{"setup":{}}"#.to_vec()).await.unwrap();
        t.send(br#"{"clientContent":{"turns":[]}}"#.to_vec())
            .await
            .unwrap();
        let reply = t.recv().await.unwrap().unwrap();
        assert!(memmem(&reply, b"\"turnComplete\":true"));
        assert!(memmem(&reply, b"hi"));

        // Only one reply was scripted: the setup frame produced none.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), t.recv())
                .await
                .is_err()
        );
    }
}
