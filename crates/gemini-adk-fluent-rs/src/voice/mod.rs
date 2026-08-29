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
//! - `Talk::talk` *(feature `voice-io`)* — the whole loop on the system's
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

#[cfg(feature = "denoise")]
mod denoise;

#[cfg(feature = "denoise")]
pub use denoise::Denoiser;

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
/// (`Talk::talk` does exactly that), a WebSocket bridge, a test harness —
/// anything that can fill and drain a channel.
pub fn pump(
    handle: &LiveHandle,
    mic: mpsc::Receiver<Vec<i16>>,
    mic_hz: u32,
    speaker: mpsc::Sender<Playback>,
    speaker_hz: u32,
) -> VoicePump {
    pump_processed(handle, mic, mic_hz, Vec::new(), speaker, speaker_hz)
}

/// [`pump`], with a chain of [`MicProcessor`]s applied to each microphone
/// frame before resampling — the insertion point for denoisers and
/// client-side voice-activity gates. Processors run in order at the mic's
/// native rate; an emptied frame (all-zero) still flows, so the session's
/// own VAD sees continuous audio.
pub fn pump_processed(
    handle: &LiveHandle,
    mut mic: mpsc::Receiver<Vec<i16>>,
    mic_hz: u32,
    mut processors: Vec<Box<dyn MicProcessor>>,
    speaker: mpsc::Sender<Playback>,
    speaker_hz: u32,
) -> VoicePump {
    let uplink_handle = handle.clone();
    let uplink = tokio::spawn(async move {
        while let Some(mut frame) = mic.recv().await {
            for processor in &mut processors {
                processor.process(&mut frame);
            }
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

/// A stage in the microphone chain: denoisers, gates, meters. Each frame is
/// mono PCM16 at the microphone's native rate; mutate it in place.
///
/// This is the seam third-party audio front-ends plug into — a
/// DeepFilterNet-style denoiser, a Silero-style VAD gate, a proprietary
/// vendor SDK — each as one `impl` with no changes to the pump. Evaluate
/// candidates with the same recorded call set on both transcription
/// accuracy *and* added latency: a stage that cleans the audio but spends
/// 200 ms per frame defeats the point.
pub trait MicProcessor: Send + 'static {
    /// Process one microphone frame in place.
    fn process(&mut self, frame: &mut Vec<i16>);
}

/// A reference [`MicProcessor`]: an energy gate that silences frames whose
/// RMS falls below a threshold, with a hangover so word tails are not
/// chopped. A floor, not a denoiser — it removes constant low-level room
/// noise between utterances and nothing more.
pub struct NoiseGate {
    threshold_rms: f64,
    hang_frames: u32,
    open_for: u32,
}

impl NoiseGate {
    /// `threshold_rms` in sample units (i16 full scale is 32767; telephone
    /// speech typically sits well above 1000 RMS). `hang_frames` keeps the
    /// gate open that many quiet frames after the last loud one.
    pub fn new(threshold_rms: f64, hang_frames: u32) -> Self {
        Self {
            threshold_rms,
            hang_frames,
            open_for: 0,
        }
    }
}

impl gemini_adk_rs::live::InputAudioProcessor for NoiseGate {
    fn process_frame(&mut self, frame: &mut Vec<i16>) {
        MicProcessor::process(self, frame);
    }
}

impl MicProcessor for NoiseGate {
    fn process(&mut self, frame: &mut Vec<i16>) {
        if frame.is_empty() {
            return;
        }
        let energy: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let rms = (energy / frame.len() as f64).sqrt();
        if rms >= self.threshold_rms {
            self.open_for = self.hang_frames + 1;
        }
        if self.open_for > 0 {
            self.open_for -= 1;
        } else {
            frame.fill(0);
        }
    }
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

    #[test]
    fn noise_gate_silences_quiet_frames_with_hangover() {
        let mut gate = NoiseGate::new(1_000.0, 1);
        let loud = vec![8_000i16; 160];
        let quiet = vec![50i16; 160];

        let mut frame = loud.clone();
        gate.process(&mut frame);
        assert_eq!(frame, loud, "speech passes untouched");

        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, quiet, "hangover keeps the tail of an utterance");

        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, vec![0i16; 160], "sustained quiet is gated");

        let mut frame = loud.clone();
        gate.process(&mut frame);
        assert_eq!(frame, loud, "the gate reopens on speech");
    }

    #[test]
    fn resample_single_sample_upsampling() {
        // Single sample upsampled should not panic and should produce clamped output
        let result = resample(&[100i16], 8_000, 24_000);
        assert!(
            !result.is_empty(),
            "upsampling single sample should produce output"
        );
        assert_eq!(
            result[0], 100,
            "single sample upsampled should preserve value"
        );
    }

    #[test]
    fn resample_single_sample_downsampling() {
        // Downsampling single sample to lower rate should work
        let result = resample(&[100i16], 24_000, 8_000);
        // A single 24kHz sample downsampled to 8kHz may produce 0-1 samples depending on ratio
        // floor(1 / (24000/8000)) = floor(1 / 3) = 0, but let's verify the function handles it
        assert!(
            result.len() <= 1,
            "downsampling single sample should produce at most 1 sample"
        );
    }

    #[test]
    fn resample_extreme_upsampling_ratio() {
        // 1 sample at 100 Hz to 48000 Hz (480x ratio)
        let result = resample(&[1000i16], 100, 48_000);
        assert!(
            !result.is_empty(),
            "extreme upsampling should produce output"
        );
        // With 1 sample input at low rate upsampled to high rate, we should get many samples
        assert!(
            result.len() >= 400,
            "extreme upsampling should produce proportional output"
        );
    }

    #[test]
    fn resample_maintains_edge_indices_without_panic() {
        // Verify that resampling with various edge lengths doesn't panic on index access
        for len in &[1, 2, 3, 159, 160, 161, 479, 480, 481] {
            let input = vec![100i16; *len];
            let down = resample(&input, 48_000, 16_000);
            let up = resample(&input, 16_000, 48_000);
            assert!(
                !down.is_empty() || *len <= 4,
                "downsampling should produce some output or be very short"
            );
            // Up should always produce non-empty unless input is empty
            assert!(
                !up.is_empty(),
                "upsampling non-empty input should produce output"
            );
        }
    }

    #[test]
    fn resample_boundary_value_clamping() {
        // Extreme values should not overflow or produce NaN
        let extreme = vec![i16::MIN, 0, i16::MAX];
        let result = resample(&extreme, 16_000, 24_000);
        for &sample in &result {
            assert!(
                sample >= i16::MIN && sample <= i16::MAX,
                "resampled sample in valid i16 range"
            );
        }
    }

    #[test]
    fn noise_gate_accumulates_open_duration() {
        // Test that open_for counter works correctly over multiple frames
        let mut gate = NoiseGate::new(1_000.0, 3);
        let loud = vec![8_000i16; 160];
        let quiet = vec![100i16; 160];

        // Loud frame: open_for = 4 (hang_frames + 1), then -= 1 → 3
        let mut frame = loud.clone();
        gate.process(&mut frame);
        assert_eq!(frame, loud);

        // Quiet frame 1: open_for = 3, -= 1 → 2, gate open
        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, quiet);

        // Quiet frame 2: open_for = 2, -= 1 → 1, gate open
        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, quiet);

        // Quiet frame 3: open_for = 1, -= 1 → 0, gate open
        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, quiet);

        // Quiet frame 4: open_for = 0, gate closed
        let mut frame = quiet.clone();
        gate.process(&mut frame);
        assert_eq!(frame, vec![0i16; 160]);
    }
}
