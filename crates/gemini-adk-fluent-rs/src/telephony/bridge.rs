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

/// Set this state key to `true` to mask the keypad: while it is set, the
/// caller's audio reaches neither the model nor the recording (it is
/// replaced with silence), and keypresses go to [`KEY_KEYPAD_ENTRY`] instead
/// of [`KEY_DTMF`]. This is the "pause and resume" pattern for taking card
/// numbers and PINs by keypad. Set it back to `false` to resume.
pub const KEY_KEYPAD_MASK: &str = "telephony:keypad_mask";
/// Digits keyed while the keypad was masked, in order. The key is marked
/// sensitive ([`State::redact_keys`]): tools read the real value, while
/// journals, exports and telemetry see it redacted.
pub const KEY_KEYPAD_ENTRY: &str = "telephony:keypad_entry";
/// How many digits [`KEY_KEYPAD_ENTRY`] holds. Not sensitive, so a flow can
/// guard on it.
pub const KEY_KEYPAD_LEN: &str = "telephony:keypad_len";
/// Set to `true` when the caller presses `#` while the keypad is masked.
pub const KEY_KEYPAD_DONE: &str = "telephony:keypad_done";

/// Whether the keypad is masked (see [`KEY_KEYPAD_MASK`]).
pub fn keypad_masked(state: &State) -> bool {
    state.get::<bool>(KEY_KEYPAD_MASK).unwrap_or(false)
}

/// Record one DTMF keypress into session state under the shared keys.
///
/// Sets [`KEY_DTMF`] to the digit and appends it to [`KEY_DTMF_HISTORY`] —
/// the exact writes every connector must make, factored to one place. While
/// the keypad is masked ([`KEY_KEYPAD_MASK`]) the digit goes to the
/// sensitive [`KEY_KEYPAD_ENTRY`] instead, and `#` ends the entry.
pub fn record_dtmf(state: &State, digit: char) {
    if keypad_masked(state) {
        state.redact_keys([KEY_KEYPAD_ENTRY]);
        if digit == '#' {
            let _ = state.set(KEY_KEYPAD_DONE, true);
            return;
        }
        let mut len = 0usize;
        let _ = state.modify(KEY_KEYPAD_ENTRY, String::new(), |mut entry| {
            entry.push(digit);
            len = entry.len();
            entry
        });
        let _ = state.set(KEY_KEYPAD_LEN, len);
        return;
    }
    let _ = state.set(KEY_DTMF, digit.to_string());
    let _ = state.modify(KEY_DTMF_HISTORY, String::new(), |mut history| {
        history.push(digit);
        history
    });
}

/// Keeps keypad tones and masked keypad entry away from the model.
///
/// An [`InputAudioProcessor`](crate::voice::InputAudioProcessor) for the
/// caller's audio at the connector's rate:
///
/// - Frames carrying DTMF tones are silenced (from the first detection, for
///   60 ms after), so the model neither hears nor transcribes keypresses.
///   Up to one detection block (26 ms) of a tone's onset can pass before it
///   is recognized; that is too short to be decoded.
/// - While [`KEY_KEYPAD_MASK`] is set, every frame is silenced.
/// - With [`record_in_band`](Self::record_in_band), digits found in the audio
///   are recorded with [`record_dtmf`]. Use it for transports without
///   out-of-band DTMF; where the platform reports keypresses itself (Twilio
///   `dtmf` events, RFC 4733), leave it off so digits aren't counted twice.
pub struct KeypadGuard {
    state: State,
    detector: super::dtmf::DtmfDetector,
    record_in_band: bool,
    hold_samples: usize,
    held: usize,
}

impl KeypadGuard {
    /// A guard for caller audio at `sample_rate`, writing to `state`.
    pub fn new(state: State, sample_rate: u32) -> Self {
        Self {
            state,
            detector: super::dtmf::DtmfDetector::new(sample_rate),
            record_in_band: false,
            hold_samples: (sample_rate as usize * 60) / 1000,
            held: 0,
        }
    }

    /// Also record digits detected in the audio (see the type docs).
    pub fn record_in_band(mut self, enabled: bool) -> Self {
        self.record_in_band = enabled;
        self
    }
}

impl crate::voice::InputAudioProcessor for KeypadGuard {
    fn process_frame(&mut self, frame: &mut Vec<i16>) {
        for digit in self.detector.feed(frame) {
            if self.record_in_band {
                record_dtmf(&self.state, digit);
            }
        }
        let tone = self.detector.tone_present();
        if tone {
            self.held = self.hold_samples;
        }
        let silence = tone || self.held > 0 || keypad_masked(&self.state);
        self.held = self.held.saturating_sub(frame.len());
        if silence {
            frame.fill(0);
        }
    }
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

    #[test]
    fn a_masked_keypad_keeps_digits_out_of_the_shared_keys() {
        let state = State::new();
        record_dtmf(&state, '1');
        let _ = state.set(KEY_KEYPAD_MASK, true);
        for digit in "4242#".chars() {
            record_dtmf(&state, digit);
        }
        let _ = state.set(KEY_KEYPAD_MASK, false);
        // The shared keys never saw the masked digits.
        assert_eq!(state.get::<String>(KEY_DTMF_HISTORY), Some("1".into()));
        assert_eq!(state.get::<String>(KEY_KEYPAD_ENTRY), Some("4242".into()));
        assert_eq!(state.get::<usize>(KEY_KEYPAD_LEN), Some(4));
        assert_eq!(state.get::<bool>(KEY_KEYPAD_DONE), Some(true));
        // The entry is sensitive wherever state leaves the process.
        assert!(state.redacted_keys().contains(KEY_KEYPAD_ENTRY));
    }

    #[test]
    fn the_keypad_guard_silences_tones_and_masked_speech() {
        use crate::voice::InputAudioProcessor as _;
        let state = State::new();
        let mut guard = KeypadGuard::new(state.clone(), 8_000).record_in_band(true);
        let tones = crate::telephony::dtmf::tests::keypresses("73", 80, 60, 6_000.0);
        let mut heard = Vec::new();
        for chunk in tones.chunks(160) {
            let mut frame = chunk.to_vec();
            guard.process_frame(&mut frame);
            heard.extend(frame);
        }
        assert_eq!(state.get::<String>(KEY_DTMF_HISTORY), Some("73".into()));
        // At most one detection block of each tone's onset gets through.
        let leaked = heard.iter().filter(|&&s| s != 0).count();
        assert!(leaked <= 2 * 205, "{leaked} tone samples reached the model");

        // Speech passes, until the keypad is masked.
        let speech: Vec<i16> = (0..160).map(|n| ((n % 40) as i16 - 20) * 300).collect();
        let mut frame = speech.clone();
        guard.process_frame(&mut frame);
        assert_eq!(frame, speech);
        let _ = state.set(KEY_KEYPAD_MASK, true);
        let mut frame = speech.clone();
        guard.process_frame(&mut frame);
        assert!(frame.iter().all(|&s| s == 0));
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
