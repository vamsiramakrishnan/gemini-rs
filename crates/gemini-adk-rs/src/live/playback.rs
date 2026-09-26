//! What the listener heard of the model's speech.
//!
//! The model streams its audio faster than it plays, and its transcript runs
//! further ahead still. Measured on the Live API, output transcription
//! arrives up to twice as far into the answer as the audio delivered with
//! it. When the listener barges in, the model's transcript therefore holds
//! words the listener never heard: a confirmation cut off halfway, or a
//! verbatim disclosure that was never finished.
//!
//! Whatever plays the audio reports to the session's [`PlaybackClock`]
//! ([`LiveHandle::playback`](super::LiveHandle::playback)). It reports the
//! audio it queues and the moment it flushes on barge-in. The voice pump,
//! and so every telephony bridge built on it, reports automatically. When
//! the model is interrupted, the runtime reads the clock and cuts the
//! model's side of the turn to what was heard. That cut applies to the
//! transcript buffer, the final `on_output_transcript` callback and the
//! verbatim check. Heard audio becomes text through the session's speaking
//! rate, which is calibrated on its uninterrupted turns (16 characters a
//! second until then, which matches English speech on current Live models).
//! The cut ends at the last whole word.
//!
//! A session with no playback reporter cuts to the audio it received, which
//! is an upper bound on what could have been heard. A turn with no audio,
//! as in a text session, is not cut.
//!
//! The model's own context is not changed. The Live API keeps what it sent,
//! so the model may still believe it finished its sentence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::clock::SharedClock;

/// English speech on current Live models, in transcript characters per
/// second of audio, used until a session has calibrated its own.
const DEFAULT_CHARS_PER_SEC: f64 = 16.0;
/// Uninterrupted speech a session needs before its own rate is trusted.
const CALIBRATION_MIN: Duration = Duration::from_secs(2);
/// Live API output: 24 kHz mono PCM16.
const OUTPUT_BYTES_PER_SEC: f64 = 48_000.0;

/// Playback progress of the model's audio, reported by whatever plays it.
/// See the [module docs](self).
///
/// It assumes queued audio plays in real time after audio queued before it,
/// as a phone line or a sound card does. Cloning shares the clock.
#[derive(Clone)]
pub struct PlaybackClock {
    inner: Arc<Mutex<Queue>>,
    clock: SharedClock,
}

impl std::fmt::Debug for PlaybackClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let q = self.inner.lock();
        f.debug_struct("PlaybackClock")
            .field("reporting", &q.reporting)
            .field("scheduled", &q.scheduled)
            .finish()
    }
}

#[derive(Debug, Default)]
struct Queue {
    reporting: bool,
    /// Audio that has played or will play: everything queued, less what a
    /// flush dropped.
    scheduled: Duration,
    /// When the queued audio finishes playing.
    drains_at: Option<Instant>,
}

impl Queue {
    fn remaining(&self, now: Instant) -> Duration {
        self.drains_at
            .map_or(Duration::ZERO, |end| end.saturating_duration_since(now))
    }
}

impl PlaybackClock {
    /// A clock on `clock`'s time (the session's, so replays stay
    /// deterministic).
    pub fn new(clock: SharedClock) -> Self {
        Self {
            inner: Arc::default(),
            clock,
        }
    }

    /// `audio` of the model's speech was handed to the speaker just now. It
    /// plays after the audio already queued.
    pub fn queued(&self, audio: Duration) {
        self.queued_at(self.clock.now(), audio);
    }

    /// Playback was cut (barge-in): the queued audio not yet played is
    /// dropped.
    pub fn flushed(&self) {
        self.flushed_at(self.clock.now());
    }

    /// All of the model's audio the listener has heard this session, or
    /// `None` if no playback has been reported.
    pub fn heard(&self) -> Option<Duration> {
        self.heard_at(self.clock.now())
    }

    pub(crate) fn queued_at(&self, now: Instant, audio: Duration) {
        let mut q = self.inner.lock();
        q.reporting = true;
        let start = q.drains_at.filter(|&end| end > now).unwrap_or(now);
        q.drains_at = Some(start + audio);
        q.scheduled += audio;
    }

    pub(crate) fn flushed_at(&self, now: Instant) {
        let mut q = self.inner.lock();
        let dropped = q.remaining(now);
        q.scheduled = q.scheduled.saturating_sub(dropped);
        q.drains_at = Some(now);
    }

    pub(crate) fn heard_at(&self, now: Instant) -> Option<Duration> {
        let q = self.inner.lock();
        q.reporting
            .then(|| q.scheduled.saturating_sub(q.remaining(now)))
    }

    /// Everything queued so far, played or not: where the next audio starts.
    fn scheduled(&self) -> Duration {
        self.inner.lock().scheduled
    }
}

/// The router's account of the model's current turn, for cutting its
/// transcript to what was heard.
#[derive(Debug, Default)]
pub(crate) struct TurnSpeech {
    /// Model audio received this turn.
    audio: Duration,
    /// Output transcript characters received this turn.
    chars: usize,
    /// Where this turn's audio starts on the playback clock.
    starts_at: Option<Duration>,
    /// The turn was interrupted; what follows it is not counted.
    cut: bool,
    /// Calibration over uninterrupted turns.
    calibrated_chars: usize,
    calibrated_audio: Duration,
}

impl TurnSpeech {
    pub(crate) fn on_audio(&mut self, bytes: usize, playback: &PlaybackClock) {
        if self.cut {
            return;
        }
        if self.starts_at.is_none() {
            self.starts_at = Some(playback.scheduled());
        }
        self.audio += Duration::from_secs_f64(bytes as f64 / OUTPUT_BYTES_PER_SEC);
    }

    pub(crate) fn on_text(&mut self, text: &str) {
        if !self.cut {
            self.chars += text.chars().count();
        }
    }

    /// The model was interrupted: how many of this turn's transcript
    /// characters were heard, or `None` to leave the transcript whole.
    pub(crate) fn on_interrupted(&mut self, playback: &PlaybackClock) -> Option<usize> {
        self.on_interrupted_at(playback, playback.clock.now())
    }

    pub(crate) fn on_interrupted_at(
        &mut self,
        playback: &PlaybackClock,
        now: Instant,
    ) -> Option<usize> {
        if self.cut {
            return None;
        }
        self.cut = true;
        if self.audio.is_zero() || self.chars == 0 {
            return None;
        }
        let heard = match (playback.heard_at(now), self.starts_at) {
            (Some(total), Some(start)) => total.saturating_sub(start).min(self.audio),
            _ => self.audio,
        };
        let chars = (heard.as_secs_f64() * self.chars_per_sec()).floor() as usize;
        Some(chars.min(self.chars))
    }

    pub(crate) fn on_turn_complete(&mut self) {
        if !self.cut && self.chars > 0 && self.audio >= Duration::from_millis(500) {
            self.calibrated_chars += self.chars;
            self.calibrated_audio += self.audio;
        }
        self.audio = Duration::ZERO;
        self.chars = 0;
        self.starts_at = None;
        self.cut = false;
    }

    fn chars_per_sec(&self) -> f64 {
        if self.calibrated_audio >= CALIBRATION_MIN {
            self.calibrated_chars as f64 / self.calibrated_audio.as_secs_f64()
        } else {
            DEFAULT_CHARS_PER_SEC
        }
    }
}

/// The byte length of the heard prefix of `text`: at most `chars`
/// characters, ending at the last whole word, without trailing space.
pub(crate) fn heard_prefix(text: &str, chars: usize) -> usize {
    let Some((cut, next)) = text.char_indices().nth(chars) else {
        return text.len();
    };
    let head = &text[..cut];
    // Mid-word: the partly heard word goes.
    let head = if next.is_whitespace() || head.ends_with(char::is_whitespace) {
        head
    } else {
        head.rfind(char::is_whitespace).map_or("", |i| &head[..i])
    };
    head.trim_end().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::system_clock;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Bytes of 24 kHz PCM16 for `n` milliseconds.
    fn audio_bytes(n: u64) -> usize {
        (n * 48) as usize
    }

    #[test]
    fn heard_follows_real_time_and_flush_drops_the_rest() {
        let clock = PlaybackClock::new(system_clock());
        let t0 = Instant::now();
        assert_eq!(clock.heard_at(t0), None);
        // 3 s of audio arrives at once: it plays until t0 + 3 s.
        clock.queued_at(t0, ms(3_000));
        assert_eq!(clock.heard_at(t0 + ms(1_200)), Some(ms(1_200)));
        // More audio queues behind it.
        clock.queued_at(t0 + ms(1_500), ms(1_000));
        assert_eq!(clock.heard_at(t0 + ms(3_500)), Some(ms(3_500)));
        // Barge-in at 3.6 s: the last 400 ms never play.
        clock.flushed_at(t0 + ms(3_600));
        assert_eq!(clock.heard_at(t0 + ms(9_000)), Some(ms(3_600)));
        // After an idle gap, new audio starts when it arrives.
        clock.queued_at(t0 + ms(10_000), ms(500));
        assert_eq!(clock.heard_at(t0 + ms(10_200)), Some(ms(3_800)));
    }

    #[test]
    fn an_interrupted_turn_is_cut_to_the_audio_heard() {
        let playback = PlaybackClock::new(system_clock());
        let mut speech = TurnSpeech::default();
        let t0 = Instant::now();
        // A first, uninterrupted turn calibrates the rate: 40 chars in 2 s.
        speech.on_audio(audio_bytes(2_000), &playback);
        playback.queued_at(t0, ms(2_000));
        speech.on_text(&"x".repeat(40));
        speech.on_turn_complete();
        assert_eq!(speech.chars_per_sec(), 20.0);

        // The next turn starts at 10 s. The model streams 5 s of audio and
        // 150 characters within a second; the caller cuts in 1.5 s into
        // playback.
        let start = t0 + ms(10_000);
        speech.on_audio(audio_bytes(5_000), &playback);
        playback.queued_at(start, ms(5_000));
        speech.on_text(&"y".repeat(150));
        let heard = speech.on_interrupted_at(&playback, start + ms(1_500));
        assert_eq!(heard, Some(30)); // 1.5 s at 20 chars/s
        // What follows the interruption in the same turn is not counted,
        // and an interrupted turn does not calibrate.
        speech.on_text("zzz");
        speech.on_turn_complete();
        assert_eq!(speech.chars_per_sec(), 20.0);
    }

    #[test]
    fn without_a_reporter_the_cut_is_the_audio_received() {
        let playback = PlaybackClock::new(system_clock());
        let mut speech = TurnSpeech::default();
        speech.on_audio(audio_bytes(2_000), &playback);
        speech.on_text(&"w ".repeat(100));
        // 2 s at the default rate.
        assert_eq!(
            speech.on_interrupted_at(&playback, Instant::now()),
            Some(32)
        );
    }

    #[test]
    fn a_turn_without_audio_or_text_is_left_whole() {
        let playback = PlaybackClock::new(system_clock());
        let mut text_only = TurnSpeech::default();
        text_only.on_text("a text session's reply");
        assert_eq!(text_only.on_interrupted(&playback), None);

        let mut untranscribed = TurnSpeech::default();
        untranscribed.on_audio(audio_bytes(1_000), &playback);
        assert_eq!(untranscribed.on_interrupted(&playback), None);
    }

    #[test]
    fn the_heard_prefix_ends_at_a_whole_word() {
        let text = "Your table is booked for Friday at eight.";
        assert_eq!(&text[..heard_prefix(text, 17)], "Your table is"); // mid-word
        assert_eq!(&text[..heard_prefix(text, 13)], "Your table is"); // at a space
        assert_eq!(&text[..heard_prefix(text, 14)], "Your table is"); // after one
        assert_eq!(&text[..heard_prefix(text, 3)], ""); // not one whole word
        assert_eq!(heard_prefix(text, 400), text.len());
        // Multi-byte text is cut on character boundaries.
        let accented = "Réservation confirmée à huit heures";
        assert_eq!(
            &accented[..heard_prefix(accented, 24)],
            "Réservation confirmée à"
        );
    }
}
