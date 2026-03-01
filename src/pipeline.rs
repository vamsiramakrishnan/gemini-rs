//! Audio pipeline orchestrator — unifies VAD, barge-in, turn detection, and jitter buffer.
//!
//! The [`AudioPipeline`] connects [`AudioSource`] and [`AudioSink`] to a
//! [`SessionHandle`], automatically running VAD, barge-in detection, turn detection,
//! and jitter-buffered playout as two coordinated background tasks.
//!
//! # Architecture
//!
//! ```text
//! AudioSource → [VAD → BargeIn → TurnDetector] → SessionHandle::send_audio()
//!                                                         ↕
//! AudioSink ← [JitterBuffer ← pull] ← SessionEvent::AudioData
//! ```

use std::future::Future;
use std::pin::Pin;

use crate::app::PipelineConfig;
use crate::buffer::AudioJitterBuffer;
use crate::flow::{BargeInAction, BargeInDetector, TurnDetectionEvent, TurnDetector};
use crate::session::{SessionEvent, SessionHandle};

#[cfg(feature = "vad")]
use crate::vad::VoiceActivityDetector;

use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Audio I/O traits
// ---------------------------------------------------------------------------

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Audio input source — microphone, WebSocket, RTP, file, etc.
///
/// Implementations produce PCM16 frames at a configured sample rate.
/// Return `None` from [`read_frame`](AudioSource::read_frame) to signal end-of-stream.
pub trait AudioSource: Send + 'static {
    /// Read the next audio frame (PCM16 samples).
    fn read_frame(&mut self) -> BoxFuture<Option<Vec<i16>>>;

    /// Sample rate of this source in Hz.
    fn sample_rate(&self) -> u32;

    /// Frame duration in milliseconds (default: 30ms).
    fn frame_duration_ms(&self) -> u32 {
        30
    }
}

/// Audio output sink — speaker, WebSocket, RTP, file, etc.
///
/// Implementations receive PCM16 frames from the jitter buffer.
pub trait AudioSink: Send + 'static {
    /// Write an audio frame for playout.
    fn write_frame(
        &mut self,
        samples: &[i16],
    ) -> BoxFuture<Result<(), Box<dyn std::error::Error + Send>>>;

    /// Sample rate expected by this sink in Hz.
    fn sample_rate(&self) -> u32;

    /// Frame duration in milliseconds (default: 30ms).
    fn frame_duration_ms(&self) -> u32 {
        30
    }
}

// ---------------------------------------------------------------------------
// Audio pipeline
// ---------------------------------------------------------------------------

/// Orchestrates the full audio processing chain between an [`AudioSource`],
/// the Gemini Live session, and an [`AudioSink`].
///
/// Spawns two coordinated background tasks:
/// - **Ingest**: source → VAD → barge-in → turn detection → send_audio
/// - **Playout**: AudioData events → jitter buffer → sink
pub struct AudioPipeline {
    ingest_handle: Option<tokio::task::JoinHandle<()>>,
    playout_handle: Option<tokio::task::JoinHandle<()>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl AudioPipeline {
    /// Start the audio pipeline. Spawns ingest and playout tasks.
    pub fn start(
        handle: SessionHandle,
        config: PipelineConfig,
        source: Box<dyn AudioSource>,
        sink: Box<dyn AudioSink>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (barge_in_tx, barge_in_rx) = mpsc::channel::<()>(4);

        let ingest_handle = {
            let handle = handle.clone();
            let config = config.clone();
            tokio::spawn(Self::ingest_loop(
                handle,
                config,
                source,
                barge_in_tx,
                shutdown_rx,
            ))
        };

        let playout_handle = {
            let handle = handle.clone();
            tokio::spawn(Self::playout_loop(handle, config, sink, barge_in_rx))
        };

        Self {
            ingest_handle: Some(ingest_handle),
            playout_handle: Some(playout_handle),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Ingest loop: source → VAD → barge-in → turn detection → send_audio.
    async fn ingest_loop(
        handle: SessionHandle,
        config: PipelineConfig,
        mut source: Box<dyn AudioSource>,
        barge_in_tx: mpsc::Sender<()>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        #[cfg(feature = "vad")]
        let mut vad = config.vad_config.map(VoiceActivityDetector::new);
        let mut barge_in = BargeInDetector::new(config.barge_in_config);
        let mut turn_detector = TurnDetector::new(config.turn_detection_config);

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                frame = source.read_frame() => {
                    let Some(samples) = frame else { break };

                    // Step 1: VAD processing
                    #[cfg(feature = "vad")]
                    let vad_is_speaking = if let Some(ref mut v) = vad {
                        use crate::vad::VadEvent;
                        match v.process_frame(&samples) {
                            Some(VadEvent::SpeechStart) => {
                                let _ = handle.signal_activity_start().await;
                                true
                            }
                            Some(VadEvent::SpeechEnd) => false,
                            None => v.is_speaking(),
                        }
                    } else {
                        true
                    };
                    #[cfg(not(feature = "vad"))]
                    let vad_is_speaking = true;

                    // Step 2: Barge-in check
                    let action = barge_in.check(handle.phase(), vad_is_speaking);
                    if action == BargeInAction::Interrupt {
                        let _ = barge_in_tx.send(()).await;
                        let _ = handle.signal_activity_start().await;
                    }

                    // Step 3: Turn detection
                    if let Some(TurnDetectionEvent::TurnEnded) = turn_detector.update(vad_is_speaking) {
                        let _ = handle.signal_activity_end().await;
                    }

                    // Step 4: Send audio (if speaking or no VAD)
                    #[cfg(feature = "vad")]
                    let should_send = vad_is_speaking || vad.is_none();
                    #[cfg(not(feature = "vad"))]
                    let should_send = true;

                    if should_send {
                        let bytes: Vec<u8> = samples
                            .iter()
                            .flat_map(|s| s.to_le_bytes())
                            .collect();
                        let _ = handle.send_audio(bytes).await;
                    }
                }
            }
        }
    }

    /// Playout loop: AudioData events → jitter buffer → sink.
    async fn playout_loop(
        handle: SessionHandle,
        config: PipelineConfig,
        mut sink: Box<dyn AudioSink>,
        mut barge_in_rx: mpsc::Receiver<()>,
    ) {
        let mut jitter = AudioJitterBuffer::new(config.jitter_config);
        let mut events = handle.subscribe();
        let frame_size =
            (sink.sample_rate() * sink.frame_duration_ms() / 1000) as usize;
        let frame_interval =
            std::time::Duration::from_millis(sink.frame_duration_ms() as u64);
        let mut playout_interval = tokio::time::interval(frame_interval);

        loop {
            tokio::select! {
                // Receive audio from Gemini
                event = events.recv() => {
                    match event {
                        Ok(SessionEvent::AudioData(bytes)) => {
                            let samples: Vec<i16> = bytes
                                .chunks_exact(2)
                                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            jitter.push(&samples);
                        }
                        Ok(SessionEvent::Disconnected(_)) | Err(_) => break,
                        _ => {}
                    }
                }

                // Barge-in signal: flush jitter buffer for instant silence
                _ = barge_in_rx.recv() => {
                    jitter.flush();
                }

                // Regular playout tick
                _ = playout_interval.tick() => {
                    let mut frame = vec![0i16; frame_size];
                    jitter.pull(&mut frame);
                    let _ = sink.write_frame(&frame).await;
                }
            }
        }
    }

    /// Stop the pipeline gracefully.
    pub async fn stop(mut self) {
        // Signal shutdown — dropping the sender also works if this fails
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.ingest_handle.take() {
            let _ = h.await;
        }
        if let Some(h) = self.playout_handle.take() {
            let _ = h.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSource {
        frames: Vec<Vec<i16>>,
        index: usize,
    }

    impl AudioSource for MockSource {
        fn read_frame(&mut self) -> BoxFuture<Option<Vec<i16>>> {
            let frame = if self.index < self.frames.len() {
                let f = self.frames[self.index].clone();
                self.index += 1;
                Some(f)
            } else {
                None
            };
            Box::pin(async move { frame })
        }
        fn sample_rate(&self) -> u32 {
            16_000
        }
    }

    struct MockSink {
        received: std::sync::Arc<std::sync::Mutex<Vec<Vec<i16>>>>,
    }

    impl AudioSink for MockSink {
        fn write_frame(
            &mut self,
            samples: &[i16],
        ) -> BoxFuture<Result<(), Box<dyn std::error::Error + Send>>> {
            self.received
                .lock()
                .unwrap()
                .push(samples.to_vec());
            Box::pin(async { Ok(()) })
        }
        fn sample_rate(&self) -> u32 {
            24_000
        }
    }

    #[test]
    fn mock_source_produces_frames() {
        let mut source = MockSource {
            frames: vec![vec![1, 2, 3], vec![4, 5, 6]],
            index: 0,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let f1 = rt.block_on(source.read_frame());
        assert_eq!(f1, Some(vec![1, 2, 3]));
        let f2 = rt.block_on(source.read_frame());
        assert_eq!(f2, Some(vec![4, 5, 6]));
        let f3 = rt.block_on(source.read_frame());
        assert_eq!(f3, None);
    }

    #[test]
    fn mock_sink_receives_frames() {
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut sink = MockSink {
            received: received.clone(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(sink.write_frame(&[10, 20, 30])).unwrap();
        assert_eq!(received.lock().unwrap().len(), 1);
        assert_eq!(received.lock().unwrap()[0], vec![10, 20, 30]);
    }
}
