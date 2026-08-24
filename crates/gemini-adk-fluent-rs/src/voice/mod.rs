//! # Voice I/O — a talking application in five lines
//!
//! The Live API speaks PCM16: 16 kHz in, 24 kHz out. Everything between a
//! microphone and that contract — resampling, channel down-mix, playback
//! buffering, and *barge-in* (the user speaks over the model; buffered speech
//! must vanish now) — is plumbing every voice application needs and none
//! should write. This module is that plumbing, engineered as two primitives:
//!
//! - [`pump`] — the device-independent duplex core. Feed it microphone frames
//!   on a channel at any sample rate; receive playback frames on another at
//!   any sample rate. It resamples both directions, forwards
//!   [`LiveEvent::Audio`](gemini_adk_rs::live::LiveEvent) to your speaker
//!   channel, and turns an interruption into an explicit [`Playback::Flush`]
//!   so stale audio is dropped, not played. Works with any audio backend —
//!   or none (tests drive it with plain channels).
//! - [`Talk::talk`] *(feature `voice-io`)* — the whole loop on the system's
//!   default microphone and speakers via `cpal`, with drain signaling wired
//!   back into the session's voice reactor. Ctrl-C or session end stops it.
//!
//! ```ignore
//! let session = Live::builder()
//!     .instruction("You are a helpful concierge.")
//!     .greeting("Greet the caller.")
//!     .connect_from_env().await?;
//! session.talk().await?;
//! ```

#[cfg(feature = "voice-io")]
mod devices;

#[cfg(feature = "voice-io")]
pub use devices::{Talk, VoiceIoError};

use gemini_adk_rs::live::{LiveEvent, LiveHandle};
use gemini_genai_rs::prelude::{bytes_to_i16, i16_to_bytes};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The sample rate the Live API expects on its input stream.
pub const SESSION_INPUT_HZ: u32 = 16_000;
/// The sample rate the Live API produces on its output stream.
pub const SESSION_OUTPUT_HZ: u32 = 24_000;

/// One playback instruction to the speaker side.
#[derive(Debug, Clone, PartialEq)]
pub enum Playback {
    /// PCM16 mono samples at the sink rate requested from [`pump`].
    Chunk(Vec<i16>),
    /// Barge-in: the model was interrupted — drop every buffered sample
    /// immediately. Playing on is the one unforgivable sin of a voice UI.
    Flush,
}

/// The two halves of a running duplex pump. Ends on its own when the session
/// closes or either channel hangs up; [`abort`](VoicePump::abort) ends it early.
pub struct VoicePump {
    uplink: JoinHandle<()>,
    downlink: JoinHandle<()>,
}

impl VoicePump {
    /// Wait for both directions to finish (session closed or channels dropped).
    pub async fn join(self) {
        let _ = self.uplink.await;
        let _ = self.downlink.await;
    }

    /// Stop both directions immediately.
    pub fn abort(&self) {
        self.uplink.abort();
        self.downlink.abort();
    }
}

/// Run the device-independent duplex loop between audio channels and a
/// session.
///
/// - `mic`: mono PCM16 frames at `mic_hz` — resampled to
///   [`SESSION_INPUT_HZ`] and written to the session.
/// - `speaker`: receives [`Playback`] instructions; chunks are mono PCM16 at
///   `speaker_hz`, resampled from [`SESSION_OUTPUT_HZ`]. An interruption
///   arrives as [`Playback::Flush`].
///
/// The pump owns no devices: pair it with `cpal` streams
/// ([`Talk::talk`] does exactly that), a WebSocket bridge, a test harness —
/// anything that can fill and drain a channel.
pub fn pump(
    handle: &LiveHandle,
    mut mic: mpsc::Receiver<Vec<i16>>,
    mic_hz: u32,
    speaker: mpsc::Sender<Playback>,
    speaker_hz: u32,
) -> VoicePump {
    let uplink_handle = handle.clone();
    let uplink = tokio::spawn(async move {
        while let Some(frame) = mic.recv().await {
            let samples = resample(&frame, mic_hz, SESSION_INPUT_HZ);
            if uplink_handle
                .send_audio(i16_to_bytes(&samples).to_vec())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let mut events = handle.events();
    let downlink = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => match playback_of(&event, speaker_hz) {
                    Some(playback) => {
                        if speaker.send(playback).await.is_err() {
                            break;
                        }
                    }
                    None => continue,
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    VoicePump { uplink, downlink }
}

/// Map one session event to a playback instruction, if it carries any.
/// Pure — this is the whole downlink policy, testable without a session.
pub(crate) fn playback_of(event: &LiveEvent, speaker_hz: u32) -> Option<Playback> {
    match event {
        LiveEvent::Audio(bytes) => {
            let samples = bytes_to_i16(bytes)?;
            Some(Playback::Chunk(resample(
                samples,
                SESSION_OUTPUT_HZ,
                speaker_hz,
            )))
        }
        LiveEvent::Interrupted => Some(Playback::Flush),
        _ => None,
    }
}

/// Linear-interpolation resampling for mono PCM16.
///
/// Deliberately simple: conversational speech through a linear resampler is
/// transparent for this use, and zero dependencies keep the core buildable
/// everywhere. Same-rate input is returned unchanged.
pub fn resample(input: &[i16], from_hz: u32, to_hz: u32) -> Vec<i16> {
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_hz as f64 / to_hz as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        let frac = pos - idx as f64;
        let a = input[idx] as f64;
        let b = input[(idx + 1).min(input.len() - 1)] as f64;
        out.push((a + (b - a) * frac).round() as i16);
    }
    out
}

/// Down-mix interleaved multi-channel PCM16 to mono by averaging.
pub fn downmix(interleaved: &[i16], channels: u16) -> Vec<i16> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let channels = channels as usize;
    interleaved
        .chunks_exact(channels)
        .map(|frame| (frame.iter().map(|&s| s as i32).sum::<i32>() / channels as i32) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn resample_preserves_duration() {
        // 480 samples at 48k = 10ms → 160 samples at 16k.
        let input = vec![1000i16; 480];
        assert_eq!(resample(&input, 48_000, 16_000).len(), 160);
        // 240 samples at 24k = 10ms → 480 samples at 48k.
        let output = vec![1000i16; 240];
        assert_eq!(resample(&output, 24_000, 48_000).len(), 480);
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let input = vec![1, -2, 3, -4];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn resample_interpolates_between_samples() {
        // Doubling the rate of [0, 100] lands a midpoint near 50.
        let out = resample(&[0, 100], 1, 2);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 50);
    }

    #[test]
    fn downmix_averages_channels() {
        // Stereo frames (L, R): (100, 300) → 200; (-50, 50) → 0.
        assert_eq!(downmix(&[100, 300, -50, 50], 2), vec![200, 0]);
        // Mono passes through.
        assert_eq!(downmix(&[7, 8], 1), vec![7, 8]);
    }

    #[test]
    fn audio_events_become_resampled_chunks() {
        // 240 samples at the session's 24k output = 10ms → 480 at 48k.
        let samples = vec![500i16; 240];
        let event = LiveEvent::Audio(Bytes::copy_from_slice(i16_to_bytes(&samples)));
        match playback_of(&event, 48_000) {
            Some(Playback::Chunk(chunk)) => assert_eq!(chunk.len(), 480),
            other => panic!("expected a chunk, got {other:?}"),
        }
    }

    #[test]
    fn interruption_becomes_flush() {
        assert_eq!(
            playback_of(&LiveEvent::Interrupted, 48_000),
            Some(Playback::Flush)
        );
    }

    #[test]
    fn unrelated_events_produce_no_playback() {
        assert_eq!(playback_of(&LiveEvent::TurnComplete, 48_000), None);
    }
}
