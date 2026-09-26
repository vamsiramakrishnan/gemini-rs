//! Mic-to-wire audio jitter: how evenly the runtime forwards microphone audio.
//!
//! A voice agent's microphone produces a chunk every 20 ms. The runtime must
//! put each one on the wire promptly and at the same cadence; a chunk that
//! waits behind a busy lane arrives late at the model, and a burst of late
//! chunks reads to the model's voice activity detection as a gap.
//!
//! [`run_jitter`] opens `N` sessions over a [`ScriptedTransport`]. Each has a
//! paced "microphone" task that stamps a sequence number into the first
//! bytes of its PCM and calls [`LiveHandle::send_audio`]. The transport reads
//! the number back out of every `realtimeInput` frame it is handed and
//! records when. Three distributions come out:
//!
//! - **mic → wire**: from `send_audio` to the frame reaching the transport,
//!   through L1 and the L0 encoder. This is the runtime's added latency.
//! - **wire jitter**: how far each frame's spacing on the wire strays from
//!   the chunk period.
//! - **source jitter**: the same for the microphone task's own timer. It is
//!   the harness's noise floor: wire jitter below it is not measurable here.
//!
//! No model or socket is involved, so the numbers are the runtime's alone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use gemini_adk_rs::live::{LiveHandle, LiveSessionBuilder, attach_session};
use gemini_genai_rs::prelude::{ModelId, SessionConfig};
use gemini_genai_rs::transport::{ConnectBuilder, TransportConfig};

use crate::{Percentiles, ScriptedTransport};

/// What to run.
#[derive(Debug, Clone)]
pub struct JitterConfig {
    /// Sessions streaming audio at once.
    pub sessions: usize,
    /// How long each streams.
    pub duration: Duration,
    /// The microphone's chunk period.
    pub chunk: Duration,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            sessions: 10,
            duration: Duration::from_secs(10),
            chunk: Duration::from_millis(20),
        }
    }
}

/// One jitter run's result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JitterReport {
    /// Sessions streaming at once.
    pub sessions: usize,
    /// Chunk period in milliseconds.
    pub chunk_ms: u64,
    /// Chunks the microphones produced, over all sessions.
    pub chunks_sent: usize,
    /// Chunks that reached the wire.
    pub chunks_wired: usize,
    /// Chunks that reached the wire after a later chunk of the same session.
    pub chunks_reordered: usize,
    /// `send_audio` → frame on the wire.
    pub mic_to_wire_ms: Percentiles,
    /// |spacing between consecutive frames on the wire − chunk period|.
    pub wire_jitter_ms: Percentiles,
    /// |spacing between consecutive microphone ticks − chunk period|: the
    /// harness's own timer noise.
    pub source_jitter_ms: Percentiles,
    /// Whole run.
    pub elapsed_ms: u64,
}

impl JitterReport {
    /// A few lines for a terminal.
    pub fn summary(&self) -> String {
        let line = |name: &str, p: &Percentiles| {
            format!(
                "{name:<13} p50 {:.3} ms  p90 {:.3}  p99 {:.3}  max {:.3}\n",
                p.p50_ms, p.p90_ms, p.p99_ms, p.max_ms
            )
        };
        let mut s = format!(
            "sessions {}  chunk {} ms  sent {}  on the wire {}  reordered {}\n",
            self.sessions,
            self.chunk_ms,
            self.chunks_sent,
            self.chunks_wired,
            self.chunks_reordered
        );
        s.push_str(&line("mic → wire", &self.mic_to_wire_ms));
        s.push_str(&line("wire jitter", &self.wire_jitter_ms));
        s.push_str(&line("source jitter", &self.source_jitter_ms));
        s.push_str(&format!("elapsed       {} ms\n", self.elapsed_ms));
        s
    }
}

/// When each chunk of one session was captured and when it was wired.
#[derive(Default)]
pub(crate) struct AudioProbe {
    captured: Mutex<Vec<Instant>>,
    wired: Mutex<Vec<(u64, Instant)>>,
}

impl AudioProbe {
    /// Called by the transport with each frame it is handed.
    pub(crate) fn observe(&self, frame: &[u8]) {
        let now = Instant::now();
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(frame) else {
            return;
        };
        let Some(data) = message
            .pointer("/realtimeInput/audio/data")
            .and_then(|d| d.as_str())
        else {
            return;
        };
        let Ok(pcm) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return;
        };
        if let Some(seq) = pcm.get(..8) {
            let seq = u64::from_le_bytes(seq.try_into().expect("eight bytes"));
            self.wired.lock().push((seq, now));
        }
    }
}

/// 20 ms of 16 kHz PCM16 carrying `seq` in its first eight bytes, over a
/// quiet tone so no silence-specific path is taken.
fn chunk(seq: u64, samples: usize) -> Vec<u8> {
    let mut pcm: Vec<u8> = (0..samples)
        .flat_map(|i| (if i % 32 < 16 { 800i16 } else { -800 }).to_le_bytes())
        .collect();
    pcm[..8].copy_from_slice(&seq.to_le_bytes());
    pcm
}

async fn open_session(probe: Arc<AudioProbe>) -> Option<LiveHandle> {
    let config = SessionConfig::new("session-bench").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
    let transport_config = TransportConfig {
        max_reconnect_attempts: 0,
        connect_timeout_secs: 5,
        setup_timeout_secs: 5,
        ..TransportConfig::default()
    };
    let transport = ScriptedTransport::new("Understood.", Duration::ZERO).with_audio_probe(probe);
    let session = ConnectBuilder::new(config.clone())
        .transport_config(transport_config)
        .transport(transport)
        .connect()
        .await
        .ok()?;
    attach_session(LiveSessionBuilder::new(config), session)
        .await
        .ok()
}

fn deviations(times: &[Instant], period: Duration) -> Vec<f64> {
    let period = period.as_secs_f64() * 1000.0;
    times
        .windows(2)
        .map(|w| ((w[1] - w[0]).as_secs_f64() * 1000.0 - period).abs())
        .collect()
}

/// Run the jitter harness once.
pub async fn run_jitter(cfg: JitterConfig) -> JitterReport {
    let started = Instant::now();
    let samples = (16_000 * cfg.chunk.as_micros() / 1_000_000) as usize;

    let mut opens = JoinSet::new();
    for _ in 0..cfg.sessions {
        opens.spawn(async move {
            let probe = Arc::new(AudioProbe::default());
            open_session(probe.clone()).await.map(|h| (h, probe))
        });
    }
    let mut sessions = Vec::new();
    while let Some(joined) = opens.join_next().await {
        if let Ok(Some(session)) = joined {
            sessions.push(session);
        }
    }

    let mut mics = JoinSet::new();
    for (handle, probe) in sessions.iter().cloned() {
        let cfg = cfg.clone();
        mics.spawn(async move {
            let mut tick = tokio::time::interval(cfg.chunk);
            let end = Instant::now() + cfg.duration;
            let mut seq = 0u64;
            while Instant::now() < end {
                tick.tick().await;
                let pcm = chunk(seq, samples);
                probe.captured.lock().push(Instant::now());
                if handle.send_audio(pcm).await.is_err() {
                    break;
                }
                seq += 1;
            }
        });
    }
    while mics.join_next().await.is_some() {}
    // Let the last chunks drain through the lanes.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (mut sent, mut wired, mut reordered) = (0, 0, 0);
    let (mut latency, mut wire_jitter, mut source_jitter) = (Vec::new(), Vec::new(), Vec::new());
    for (handle, probe) in &sessions {
        let _ = handle.disconnect().await;
        let captured = probe.captured.lock();
        let on_wire = probe.wired.lock();
        sent += captured.len();
        wired += on_wire.len();
        reordered += on_wire.windows(2).filter(|w| w[1].0 < w[0].0).count();
        for (seq, at) in on_wire.iter() {
            if let Some(from) = captured.get(*seq as usize) {
                latency.push((*at - *from).as_secs_f64() * 1000.0);
            }
        }
        let wire_times: Vec<Instant> = on_wire.iter().map(|(_, at)| *at).collect();
        wire_jitter.extend(deviations(&wire_times, cfg.chunk));
        source_jitter.extend(deviations(&captured, cfg.chunk));
    }

    JitterReport {
        sessions: sessions.len(),
        chunk_ms: cfg.chunk.as_millis() as u64,
        chunks_sent: sent,
        chunks_wired: wired,
        chunks_reordered: reordered,
        mic_to_wire_ms: Percentiles::of(latency),
        wire_jitter_ms: Percentiles::of(wire_jitter),
        source_jitter_ms: Percentiles::of(source_jitter),
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_carries_its_sequence_number_to_the_probe() {
        let probe = AudioProbe::default();
        let pcm = chunk(42, 320);
        assert_eq!(pcm.len(), 640);
        let frame = serde_json::json!({
            "realtimeInput": { "audio": {
                "mimeType": "audio/pcm;rate=16000",
                "data": base64::engine::general_purpose::STANDARD.encode(&pcm),
            } }
        });
        probe.observe(frame.to_string().as_bytes());
        probe.observe(br#"{"clientContent":{}}"#);
        let wired = probe.wired.lock();
        assert_eq!(wired.len(), 1);
        assert_eq!(wired[0].0, 42);
    }
}
