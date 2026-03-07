//! Client-side Voice Activity Detection (VAD).
//!
//! Dual-threshold energy detector with zero-crossing rate (ZCR) confirmation
//! and adaptive noise floor estimation. Complements Gemini's server-side VAD:
//!
//! - **Bandwidth savings**: Don't send silence over the network
//! - **Latency reduction**: Signal `activityStart` before server detects it
//! - **Barge-in pre-emption**: Flush jitter buffer locally before server confirms

/// VAD configuration parameters.
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Frame duration in milliseconds (typically 10–30ms).
    pub frame_duration_ms: u32,
    /// Energy threshold (dBFS) above noise floor to trigger speech start.
    pub start_threshold_db: f64,
    /// Energy threshold (dBFS) above noise floor to end speech.
    pub stop_threshold_db: f64,
    /// Minimum speech duration in frames before confirming speech.
    pub min_speech_frames: u32,
    /// Hangover duration in frames — keeps "speaking" state after energy drops.
    pub hangover_frames: u32,
    /// ZCR range for speech confirmation (low, high).
    pub speech_zcr_range: (f64, f64),
    /// Initial noise floor estimate (dBFS).
    pub initial_noise_floor_db: f64,
    /// Number of pre-speech frames to buffer.
    pub pre_speech_frames: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            frame_duration_ms: 30,
            start_threshold_db: 15.0,
            stop_threshold_db: 10.0,
            min_speech_frames: 3,
            hangover_frames: 10,
            speech_zcr_range: (0.02, 0.5),
            initial_noise_floor_db: -60.0,
            pre_speech_frames: 3,
        }
    }
}

impl VadConfig {
    /// Number of samples per frame.
    pub fn frame_size(&self) -> usize {
        (self.sample_rate * self.frame_duration_ms / 1000) as usize
    }
}

/// VAD state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// No speech detected.
    Silence,
    /// Energy exceeded threshold but min duration not yet met.
    PendingSpeech,
    /// Speech confirmed.
    Speech,
    /// Energy dropped but still in hangover period.
    Hangover,
}

/// Events emitted by the VAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// Speech onset detected.
    SpeechStart,
    /// Speech ended (after hangover).
    SpeechEnd,
}

/// Voice Activity Detector with adaptive noise floor.
pub struct VoiceActivityDetector {
    config: VadConfig,
    state: VadState,
    /// Adaptive noise floor estimate (dBFS).
    noise_floor_db: f64,
    /// Frames spent in current state.
    state_frames: u32,
    /// Number of frames used for noise adaptation.
    noise_adapt_frames: u64,
    /// Circular buffer of pre-speech frames.
    pre_speech_buf: Vec<Vec<i16>>,
    pre_speech_idx: usize,
}

impl VoiceActivityDetector {
    /// Create a new VAD with the given configuration.
    pub fn new(config: VadConfig) -> Self {
        let frame_size = config.frame_size();
        let pre_speech_buf: Vec<Vec<i16>> = (0..config.pre_speech_frames)
            .map(|_| vec![0i16; frame_size])
            .collect();
        Self {
            noise_floor_db: config.initial_noise_floor_db,
            state: VadState::Silence,
            state_frames: 0,
            noise_adapt_frames: 0,
            pre_speech_buf,
            pre_speech_idx: 0,
            config,
        }
    }

    /// Current VAD state.
    pub fn state(&self) -> VadState {
        self.state
    }

    /// Whether speech is currently detected (Speech or Hangover state).
    pub fn is_speaking(&self) -> bool {
        matches!(self.state, VadState::Speech | VadState::Hangover)
    }

    /// Current noise floor estimate (dBFS).
    pub fn noise_floor_db(&self) -> f64 {
        self.noise_floor_db
    }

    /// Get pre-speech frames (the frames captured just before speech onset).
    pub fn drain_pre_speech(&mut self) -> Vec<Vec<i16>> {
        let frame_size = self.config.frame_size();
        let mut fresh: Vec<Vec<i16>> = (0..self.config.pre_speech_frames)
            .map(|_| vec![0i16; frame_size])
            .collect();
        std::mem::swap(&mut self.pre_speech_buf, &mut fresh);
        self.pre_speech_idx = 0;
        fresh
    }

    /// Process a single audio frame and return any state-change event.
    pub fn process_frame(&mut self, samples: &[i16]) -> Option<VadEvent> {
        let energy_db = compute_energy_db(samples);
        let zcr = compute_zcr(samples);
        let energy_above_noise = energy_db - self.noise_floor_db;

        let is_speech_like = energy_above_noise > self.config.start_threshold_db
            && zcr >= self.config.speech_zcr_range.0
            && zcr <= self.config.speech_zcr_range.1;

        let is_above_stop = energy_above_noise > self.config.stop_threshold_db;

        match self.state {
            VadState::Silence => {
                // Update noise floor during confirmed silence
                self.update_noise_floor(energy_db);

                // Store pre-speech frame (copy into pre-allocated slot, zero-alloc)
                if self.config.pre_speech_frames > 0 && !self.pre_speech_buf.is_empty() {
                    let idx = self.pre_speech_idx % self.config.pre_speech_frames;
                    let buf = &mut self.pre_speech_buf[idx];
                    buf.resize(samples.len(), 0);
                    buf.copy_from_slice(samples);
                    self.pre_speech_idx += 1;
                }

                if is_speech_like {
                    self.state = VadState::PendingSpeech;
                    self.state_frames = 1;
                }
                None
            }

            VadState::PendingSpeech => {
                if is_speech_like {
                    self.state_frames += 1;
                    if self.state_frames >= self.config.min_speech_frames {
                        self.state = VadState::Speech;
                        self.state_frames = 0;
                        Some(VadEvent::SpeechStart)
                    } else {
                        None
                    }
                } else {
                    // False alarm — go back to silence
                    self.state = VadState::Silence;
                    self.state_frames = 0;
                    None
                }
            }

            VadState::Speech => {
                if !is_above_stop {
                    self.state = VadState::Hangover;
                    self.state_frames = 1;
                }
                None
            }

            VadState::Hangover => {
                if is_above_stop {
                    // Speech resumed — back to Speech
                    self.state = VadState::Speech;
                    self.state_frames = 0;
                    None
                } else {
                    self.state_frames += 1;
                    if self.state_frames >= self.config.hangover_frames {
                        self.state = VadState::Silence;
                        self.state_frames = 0;
                        for buf in &mut self.pre_speech_buf {
                            buf.iter_mut().for_each(|s| *s = 0);
                        }
                        self.pre_speech_idx = 0;
                        Some(VadEvent::SpeechEnd)
                    } else {
                        None
                    }
                }
            }
        }
    }

    /// Update the adaptive noise floor using EWMA.
    fn update_noise_floor(&mut self, energy_db: f64) {
        self.noise_adapt_frames += 1;
        // Alpha decreases over time: fast initial adaptation, slow drift
        let alpha = 0.01_f64.min(1.0 / self.noise_adapt_frames as f64);
        self.noise_floor_db = self.noise_floor_db * (1.0 - alpha) + energy_db * alpha;
    }

    /// Reset the VAD to its initial state.
    pub fn reset(&mut self) {
        self.state = VadState::Silence;
        self.state_frames = 0;
        self.noise_adapt_frames = 0;
        self.noise_floor_db = self.config.initial_noise_floor_db;
        for buf in &mut self.pre_speech_buf {
            buf.iter_mut().for_each(|s| *s = 0);
        }
        self.pre_speech_idx = 0;
    }
}

/// Compute RMS energy in dBFS for a frame of PCM16 samples.
fn compute_energy_db(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return -96.0;
    }

    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / samples.len() as f64).sqrt();
    let db = 20.0 * (rms / 32767.0).log10();
    db.max(-96.0) // Floor at -96 dBFS
}

/// Compute zero-crossing rate for a frame of PCM16 samples.
fn compute_zcr(samples: &[i16]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }

    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] >= 0) != (w[1] >= 0))
        .count();

    crossings as f64 / (samples.len() - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vad() -> VoiceActivityDetector {
        VoiceActivityDetector::new(VadConfig {
            sample_rate: 16000,
            frame_duration_ms: 20,
            start_threshold_db: 15.0,
            stop_threshold_db: 10.0,
            min_speech_frames: 2,
            hangover_frames: 3,
            speech_zcr_range: (0.01, 0.9),
            initial_noise_floor_db: -60.0,
            pre_speech_frames: 2,
        })
    }

    fn silence_frame(len: usize) -> Vec<i16> {
        vec![0i16; len]
    }

    fn speech_frame(len: usize, amplitude: i16) -> Vec<i16> {
        // Generate a simple alternating signal that has both energy and ZCR
        (0..len)
            .map(|i| if i % 4 < 2 { amplitude } else { -amplitude })
            .collect()
    }

    #[test]
    fn starts_silent() {
        let vad = make_vad();
        assert_eq!(vad.state(), VadState::Silence);
        assert!(!vad.is_speaking());
    }

    #[test]
    fn silence_stays_silent() {
        let mut vad = make_vad();
        let frame = silence_frame(320);
        for _ in 0..10 {
            let event = vad.process_frame(&frame);
            assert!(event.is_none());
        }
        assert_eq!(vad.state(), VadState::Silence);
    }

    #[test]
    fn speech_detected_after_min_frames() {
        let mut vad = make_vad();
        let frame = speech_frame(320, 10000);

        // Frame 1: PendingSpeech
        let e1 = vad.process_frame(&frame);
        assert!(e1.is_none());
        assert_eq!(vad.state(), VadState::PendingSpeech);

        // Frame 2: min_speech_frames = 2 → SpeechStart
        let e2 = vad.process_frame(&frame);
        assert_eq!(e2, Some(VadEvent::SpeechStart));
        assert_eq!(vad.state(), VadState::Speech);
        assert!(vad.is_speaking());
    }

    #[test]
    fn speech_end_after_hangover() {
        let mut vad = make_vad();
        let speech = speech_frame(320, 10000);
        let silence = silence_frame(320);

        // Trigger speech
        vad.process_frame(&speech);
        vad.process_frame(&speech);
        assert_eq!(vad.state(), VadState::Speech);

        // Drop energy → hangover
        vad.process_frame(&silence);
        assert_eq!(vad.state(), VadState::Hangover);

        // Hangover frames 2 and 3
        vad.process_frame(&silence);
        let e = vad.process_frame(&silence);
        assert_eq!(e, Some(VadEvent::SpeechEnd));
        assert_eq!(vad.state(), VadState::Silence);
    }

    #[test]
    fn speech_resumes_during_hangover() {
        let mut vad = make_vad();
        let speech = speech_frame(320, 10000);
        let silence = silence_frame(320);

        // Trigger speech
        vad.process_frame(&speech);
        vad.process_frame(&speech);
        assert_eq!(vad.state(), VadState::Speech);

        // Brief silence → hangover
        vad.process_frame(&silence);
        assert_eq!(vad.state(), VadState::Hangover);

        // Speech resumes
        let e = vad.process_frame(&speech);
        assert!(e.is_none()); // No event, just resumes
        assert_eq!(vad.state(), VadState::Speech);
    }

    #[test]
    fn false_alarm_returns_to_silence() {
        let mut vad = make_vad();
        let speech = speech_frame(320, 10000);
        let silence = silence_frame(320);

        // 1 speech frame → PendingSpeech
        vad.process_frame(&speech);
        assert_eq!(vad.state(), VadState::PendingSpeech);

        // Then silence → back to Silence (false alarm)
        vad.process_frame(&silence);
        assert_eq!(vad.state(), VadState::Silence);
    }

    #[test]
    fn energy_db_calculation() {
        // Full-scale sine approximation
        let full_scale: Vec<i16> = (0..320).map(|_| i16::MAX).collect();
        let db = compute_energy_db(&full_scale);
        assert!(db > -1.0); // Should be near 0 dBFS

        let silence = vec![0i16; 320];
        let db_silence = compute_energy_db(&silence);
        assert_eq!(db_silence, -96.0);
    }

    #[test]
    fn zcr_calculation() {
        // Alternating signal → high ZCR
        let alternating: Vec<i16> = (0..100)
            .map(|i| if i % 2 == 0 { 1000 } else { -1000 })
            .collect();
        let zcr = compute_zcr(&alternating);
        assert!(zcr > 0.9);

        // Constant signal → zero ZCR
        let constant = vec![1000i16; 100];
        let zcr_const = compute_zcr(&constant);
        assert_eq!(zcr_const, 0.0);
    }

    #[test]
    fn noise_floor_adapts() {
        let mut vad = make_vad();
        // Feed low-energy frames — noise floor should move toward them
        let low_noise: Vec<i16> = vec![10; 320]; // Very quiet
        for _ in 0..100 {
            vad.process_frame(&low_noise);
        }
        // Noise floor should have adapted upward from -60 dBFS
        assert!(vad.noise_floor_db() > -96.0);
    }

    #[test]
    fn reset_clears_state() {
        let mut vad = make_vad();
        let speech = speech_frame(320, 10000);
        vad.process_frame(&speech);
        vad.process_frame(&speech);
        assert_eq!(vad.state(), VadState::Speech);

        vad.reset();
        assert_eq!(vad.state(), VadState::Silence);
        assert_eq!(vad.noise_floor_db(), -60.0);
    }
}
