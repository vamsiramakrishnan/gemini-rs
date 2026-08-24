//! The `cpal` device adapter behind [`Talk`] — default microphone in, default
//! speakers out, barge-in flush, drain signaling. Feature `voice-io`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use gemini_adk_rs::live::LiveHandle;
use tokio::sync::mpsc;

use super::{downmix, pump, Playback};

/// Errors from the device layer.
#[derive(Debug, thiserror::Error)]
pub enum VoiceIoError {
    /// No default input (microphone) device.
    #[error("no default input device")]
    NoInputDevice,
    /// No default output (speaker) device.
    #[error("no default output device")]
    NoOutputDevice,
    /// The audio backend refused a stream.
    #[error("audio backend: {0}")]
    Backend(String),
}

/// Run a full-duplex voice conversation on the system's default audio
/// devices.
///
/// `talk()` bridges the default microphone and speakers into the session via
/// [`pump`](super::pump): capture is down-mixed to mono and resampled to the
/// session's input rate; model speech is resampled to the device rate and
/// buffered; an interruption flushes the buffer instantly (barge-in); the
/// session's voice reactor is told when playback drains. Returns when the
/// session ends or on Ctrl-C.
#[allow(async_fn_in_trait)]
pub trait Talk {
    /// See the trait docs. The five-line voice app:
    ///
    /// ```ignore
    /// Live::builder()
    ///     .instruction("You are a helpful concierge.")
    ///     .greeting("Greet the caller.")
    ///     .connect_from_env().await?
    ///     .talk().await?;
    /// ```
    async fn talk(&self) -> Result<(), VoiceIoError>;
}

impl Talk for LiveHandle {
    async fn talk(&self) -> Result<(), VoiceIoError> {
        let host = cpal::default_host();
        let input = host
            .default_input_device()
            .ok_or(VoiceIoError::NoInputDevice)?;
        let output = host
            .default_output_device()
            .ok_or(VoiceIoError::NoOutputDevice)?;
        let input_config = input
            .default_input_config()
            .map_err(|e| VoiceIoError::Backend(e.to_string()))?;
        let output_config = output
            .default_output_config()
            .map_err(|e| VoiceIoError::Backend(e.to_string()))?;

        let mic_hz = input_config.sample_rate().0;
        let mic_channels = input_config.channels();
        let speaker_hz = output_config.sample_rate().0;
        let speaker_channels = output_config.channels();

        // Microphone → pump. Bounded; a saturated channel drops the frame
        // (mic loss under pressure beats blocking the audio callback).
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
        let capture = move |mono: Vec<i16>| {
            let _ = mic_tx.try_send(mono);
        };
        let input_stream = build_input_stream(&input, &input_config, mic_channels, capture)?;

        // Pump → speaker ring. The cpal output callback drains the ring;
        // `Flush` clears it — barge-in silences the device within one buffer.
        let ring: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let was_playing = Arc::new(AtomicBool::new(false));
        let (spk_tx, mut spk_rx) = mpsc::channel::<Playback>(64);
        {
            let ring = ring.clone();
            let was_playing = was_playing.clone();
            tokio::spawn(async move {
                while let Some(playback) = spk_rx.recv().await {
                    let mut ring = ring.lock().expect("playback ring poisoned");
                    match playback {
                        Playback::Chunk(samples) => {
                            ring.extend(samples);
                            was_playing.store(true, Ordering::Relaxed);
                        }
                        Playback::Flush => ring.clear(),
                    }
                }
            });
        }
        let output_stream =
            build_output_stream(&output, &output_config, speaker_channels, ring.clone())?;

        input_stream
            .play()
            .map_err(|e| VoiceIoError::Backend(e.to_string()))?;
        output_stream
            .play()
            .map_err(|e| VoiceIoError::Backend(e.to_string()))?;

        let running = pump(self, mic_rx, mic_hz, spk_tx, speaker_hz);

        // Tell the voice reactor when the speaker goes quiet, so prompt
        // gating and barge-in accounting see real playback state.
        let drain_handle = self.clone();
        let drain_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(50));
            loop {
                interval.tick().await;
                let empty = ring.lock().expect("playback ring poisoned").is_empty();
                if empty && was_playing.swap(false, Ordering::Relaxed) {
                    let _ = drain_handle.playback_drained().await;
                }
            }
        });

        // Converse until the session ends or the user hits Ctrl-C.
        tokio::select! {
            _ = running.join() => {}
            _ = tokio::signal::ctrl_c() => {
                let _ = self.disconnect().await;
            }
        }
        drain_task.abort();
        drop(input_stream);
        drop(output_stream);
        Ok(())
    }
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: u16,
    on_mono: impl Fn(Vec<i16>) + Send + 'static,
) -> Result<cpal::Stream, VoiceIoError> {
    let stream_config: cpal::StreamConfig = config.config();
    let err = |e: cpal::BuildStreamError| VoiceIoError::Backend(e.to_string());
    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _| on_mono(downmix(data, channels)),
                |e| tracing::warn!("input stream error: {e}"),
                None,
            )
            .map_err(err)?,
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    let pcm: Vec<i16> = data
                        .iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    on_mono(downmix(&pcm, channels));
                },
                |e| tracing::warn!("input stream error: {e}"),
                None,
            )
            .map_err(err)?,
        other => {
            return Err(VoiceIoError::Backend(format!(
                "unsupported input sample format {other:?}"
            )))
        }
    };
    Ok(stream)
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: u16,
    ring: Arc<Mutex<VecDeque<i16>>>,
) -> Result<cpal::Stream, VoiceIoError> {
    let stream_config: cpal::StreamConfig = config.config();
    let err = |e: cpal::BuildStreamError| VoiceIoError::Backend(e.to_string());
    let channels = channels as usize;
    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    let mut ring = ring.lock().expect("playback ring poisoned");
                    for frame in data.chunks_mut(channels) {
                        let sample = ring.pop_front().unwrap_or(0);
                        frame.fill(sample);
                    }
                },
                |e| tracing::warn!("output stream error: {e}"),
                None,
            )
            .map_err(err)?,
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    let mut ring = ring.lock().expect("playback ring poisoned");
                    for frame in data.chunks_mut(channels) {
                        let sample = ring.pop_front().unwrap_or(0) as f32 / i16::MAX as f32;
                        frame.fill(sample);
                    }
                },
                |e| tracing::warn!("output stream error: {e}"),
                None,
            )
            .map_err(err)?,
        other => {
            return Err(VoiceIoError::Backend(format!(
                "unsupported output sample format {other:?}"
            )))
        }
    };
    Ok(stream)
}
