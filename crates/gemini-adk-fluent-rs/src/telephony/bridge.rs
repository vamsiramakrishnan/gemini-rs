//! Vendor-neutral call-bridge components.
//!
//! Every contact-center connector — Twilio Media Streams, a raw SIP/RTP leg,
//! a platform's gRPC virtual-agent slot — reduces to the same duties: move
//! audio frames both ways through [`voice::pump`](crate::voice::pump), land
//! caller keypresses and call identity in session state where flow guards
//! read them, and keep the caller's ear busy while slow work runs. This
//! module holds those duties as small, connector-agnostic components, so a
//! new connector composes them instead of re-inventing them.
//!
//! Nothing here owns a socket. Connectors own transport; these components
//! own semantics.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use gemini_adk_rs::State;
use gemini_adk_rs::live::{LiveEvent, LiveHandle};

use crate::voice::Playback;

// ── Session-state vocabulary ─────────────────────────────────────────────────
//
// One key set for every connector, so a flow guard like
// `Guard::eq("telephony:dtmf", "1")` works identically behind Twilio, SIP,
// or any future transport.

/// State key holding the most recent DTMF digit pressed by the caller.
pub const KEY_DTMF: &str = "telephony:dtmf";
/// State key holding every DTMF digit pressed so far, concatenated in order.
pub const KEY_DTMF_HISTORY: &str = "telephony:dtmf_history";
/// State key holding the transport's call identifier once known.
pub const KEY_CALL_SID: &str = "telephony:call_sid";
/// State key holding the transport's media-stream identifier once known.
pub const KEY_STREAM_SID: &str = "telephony:stream_sid";
/// State key holding the caller identity the transport presented
/// (SIP `From`, a platform's ANI field, …).
pub const KEY_CALLER: &str = "telephony:caller";

/// Record one DTMF keypress into session state under the shared keys.
///
/// Sets [`KEY_DTMF`] to the digit and appends it to [`KEY_DTMF_HISTORY`] —
/// the exact writes every connector must make, factored to one place.
pub fn record_dtmf(state: &State, digit: char) {
    let _ = state.set(KEY_DTMF, digit.to_string());
    let _ = state.modify(KEY_DTMF_HISTORY, String::new(), |mut history| {
        history.push(digit);
        history
    });
}

/// Deduplicates RFC 4733 end-of-event packets.
///
/// A telephone-event keypress ends with its final packet conventionally
/// retransmitted three times, all sharing one RTP timestamp. Feed every
/// end-marked event through [`accept`](Self::accept); only the first per
/// timestamp comes back `true`.
#[derive(Debug, Default)]
pub struct DtmfDeduper {
    last_end_timestamp: Option<u32>,
}

impl DtmfDeduper {
    /// `true` exactly once per keypress: for the first end-marked packet
    /// carrying a given RTP timestamp.
    pub fn accept(&mut self, end: bool, rtp_timestamp: u32) -> bool {
        if !end {
            return false;
        }
        if self.last_end_timestamp == Some(rtp_timestamp) {
            return false;
        }
        self.last_end_timestamp = Some(rtp_timestamp);
        true
    }
}

// ── Latency filler ───────────────────────────────────────────────────────────

/// Configuration for [`spawn_latency_filler`].
#[derive(Clone)]
pub struct FillerConfig {
    /// The filler clip: mono PCM16 at the connector's playback sample rate
    /// (the `speaker_hz` given to [`voice::pump`](crate::voice::pump)) —
    /// e.g. a pre-synthesized "one moment, let me check that".
    pub clip: Arc<Vec<i16>>,
    /// Silence to tolerate after the caller stops speaking before playing
    /// the clip. Below ~1.5 s the filler fires on normal model latency and
    /// talks over the answer's first syllables.
    pub delay: Duration,
    /// At most one filler per this interval, so a long tool call gets one
    /// reassurance, not a loop of them.
    pub min_interval: Duration,
}

impl FillerConfig {
    /// A filler clip with the conventional pacing: 2 s of tolerated silence,
    /// at most one filler per 10 s.
    pub fn new(clip: Vec<i16>) -> Self {
        Self {
            clip: Arc::new(clip),
            delay: Duration::from_secs(2),
            min_interval: Duration::from_secs(10),
        }
    }

    /// Override the tolerated-silence delay.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Override the per-filler minimum interval.
    pub fn min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }
}

/// Keep the caller's ear busy while the model is slow: when the caller stops
/// speaking ([`LiveEvent::VadEnd`]) and no model audio arrives within
/// `config.delay`, inject the configured clip into the connector's playback
/// channel. Model audio or an interruption disarms the timer; a
/// [`Playback::Flush`] from a barge-in clears any queued filler exactly like
/// any other queued audio.
///
/// Masking is not a substitute for reducing latency — it buys tolerance for
/// the tail, and because it is driven by the same event stream as the
/// telemetry lane, the silences it papers over remain visible in the
/// latency metrics.
///
/// The task ends when the session's event stream closes; abort the handle to
/// stop it sooner.
pub fn spawn_latency_filler(
    handle: &LiveHandle,
    speaker: mpsc::Sender<Playback>,
    config: FillerConfig,
) -> JoinHandle<()> {
    let events = handle.events();
    tokio::spawn(filler_task(events, speaker, config))
}

/// The filler loop itself, taking the event stream directly — the seam tests
/// drive without a session.
pub(crate) async fn filler_task(
    mut events: broadcast::Receiver<LiveEvent>,
    speaker: mpsc::Sender<Playback>,
    config: FillerConfig,
) {
    let mut armed_at: Option<tokio::time::Instant> = None;
    let mut last_filler: Option<tokio::time::Instant> = None;
    loop {
        let deadline = armed_at.map(|at| at + config.delay);
        tokio::select! {
            event = events.recv() => match event {
                Ok(LiveEvent::VadEnd) => armed_at = Some(tokio::time::Instant::now()),
                // Model audio (or the user cutting in) means silence ended.
                Ok(LiveEvent::Audio(_)) | Ok(LiveEvent::VadStart) | Ok(LiveEvent::Interrupted) => {
                    armed_at = None;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            () = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {
                armed_at = None;
                let recently = last_filler
                    .is_some_and(|at| at.elapsed() < config.min_interval);
                if !recently {
                    last_filler = Some(tokio::time::Instant::now());
                    let _ = speaker
                        .send(Playback::Chunk(config.clip.as_ref().clone()))
                        .await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtmf_dedup_accepts_one_end_per_timestamp() {
        let mut dedup = DtmfDeduper::default();
        assert!(!dedup.accept(false, 100), "non-end packets never emit");
        assert!(dedup.accept(true, 100), "first end emits");
        assert!(!dedup.accept(true, 100), "retransmitted end is dropped");
        assert!(!dedup.accept(true, 100));
        assert!(dedup.accept(true, 900), "next keypress emits again");
    }

    #[test]
    fn record_dtmf_writes_the_shared_keys() {
        let state = State::new();
        record_dtmf(&state, '4');
        record_dtmf(&state, '#');
        assert_eq!(state.get::<String>(KEY_DTMF), Some("#".into()));
        assert_eq!(state.get::<String>(KEY_DTMF_HISTORY), Some("4#".into()));
    }

    #[tokio::test(start_paused = true)]
    async fn filler_fires_after_silence_and_respects_min_interval() {
        let (event_tx, event_rx) = broadcast::channel(16);
        let (speaker_tx, mut speaker_rx) = mpsc::channel(4);
        let config = FillerConfig::new(vec![7i16; 80])
            .delay(Duration::from_secs(2))
            .min_interval(Duration::from_secs(10));
        let task = tokio::spawn(filler_task(event_rx, speaker_tx, config));

        // Caller stops speaking; nothing for 2 s → filler plays.
        event_tx.send(LiveEvent::VadEnd).unwrap();
        tokio::time::sleep(Duration::from_millis(2100)).await;
        match speaker_rx.recv().await {
            Some(Playback::Chunk(samples)) => assert_eq!(samples, vec![7i16; 80]),
            other => panic!("expected filler chunk, got {other:?}"),
        }

        // A second silence inside min_interval stays quiet.
        event_tx.send(LiveEvent::VadEnd).unwrap();
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(
            speaker_rx.try_recv().is_err(),
            "min_interval suppresses a second filler"
        );

        drop(event_tx);
        let _ = task.await;
    }

    #[tokio::test(start_paused = true)]
    async fn model_audio_disarms_the_filler() {
        let (event_tx, event_rx) = broadcast::channel(16);
        let (speaker_tx, mut speaker_rx) = mpsc::channel(4);
        let task = tokio::spawn(filler_task(
            event_rx,
            speaker_tx,
            FillerConfig::new(vec![1i16]).delay(Duration::from_secs(2)),
        ));

        event_tx.send(LiveEvent::VadEnd).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        // The model answers within the window — no filler.
        event_tx
            .send(LiveEvent::Audio(bytes::Bytes::from_static(&[0, 0])))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(speaker_rx.try_recv().is_err(), "audio disarmed the filler");

        drop(event_tx);
        let _ = task.await;
    }
}
