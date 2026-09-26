//! Stereo call recording for compliance and quality review.
//!
//! [`CallRecorder`] writes a 16-bit PCM WAV with the caller on the left
//! channel and the agent on the right, aligned to the call's own clock:
//!
//! - Caller audio arrives in real time and is placed at the moment it
//!   arrived. A gap in delivery becomes silence.
//! - Agent audio arrives faster than it plays, since the model streams ahead
//!   of the phone line. Each chunk is placed where playback reaches it: after
//!   the audio already queued, or now if the queue is empty.
//! - On barge-in, [`flush`](CallRecorder::flush) cuts the agent audio that
//!   was queued but not yet played, so the recording holds what the caller
//!   heard, not what the model generated. It returns how much was cut.
//!
//! Samples are written to disk as soon as they can no longer change (up to
//! the current moment), so memory stays flat for a call of any length. Keypad
//! masking applies upstream: while [`KeypadGuard`](super::bridge::KeypadGuard)
//! silences the caller, the recorder receives the silence too.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// A stereo WAV recording of one call. See the module docs.
pub struct CallRecorder {
    inner: Mutex<Inner>,
}

struct Inner {
    writer: Option<BufWriter<File>>,
    rate: u32,
    start: Instant,
    /// Frames already written to disk.
    written: u64,
    /// Caller samples from position `written` on.
    caller: VecDeque<i16>,
    /// Agent samples from position `written` on (silence where none played).
    agent: VecDeque<i16>,
}

impl Inner {
    fn now_frames(&self, at: Duration) -> u64 {
        (at.as_secs_f64() * f64::from(self.rate)) as u64
    }

    fn caller_end(&self) -> u64 {
        self.written + self.caller.len() as u64
    }

    fn agent_end(&self) -> u64 {
        self.written + self.agent.len() as u64
    }

    /// Write every frame before `upto` (caller and agent both final there).
    fn drain_to(&mut self, upto: u64) -> std::io::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        while self.written < upto {
            let left = self.caller.pop_front().unwrap_or(0);
            let right = self.agent.pop_front().unwrap_or(0);
            writer.write_all(&left.to_le_bytes())?;
            writer.write_all(&right.to_le_bytes())?;
            self.written += 1;
        }
        Ok(())
    }
}

impl CallRecorder {
    /// Start recording to `path` (created or truncated) at `sample_rate`,
    /// the call's rate (8 kHz on the phone network).
    pub fn create(path: impl AsRef<Path>, sample_rate: u32) -> std::io::Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(&wav_header(sample_rate, 0))?;
        Ok(Self {
            inner: Mutex::new(Inner {
                writer: Some(writer),
                rate: sample_rate,
                start: Instant::now(),
                written: 0,
                caller: VecDeque::new(),
                agent: VecDeque::new(),
            }),
        })
    }

    /// Record caller audio that just arrived.
    pub fn caller(&self, samples: &[i16]) {
        let at = self.inner.lock().start.elapsed();
        self.caller_at(at, samples);
    }

    /// Record agent audio just sent toward the caller.
    pub fn agent(&self, samples: &[i16]) {
        let at = self.inner.lock().start.elapsed();
        self.agent_at(at, samples);
    }

    /// The caller barged in: drop agent audio not yet played. Returns how
    /// much was dropped.
    pub fn flush(&self) -> Duration {
        let at = self.inner.lock().start.elapsed();
        self.flush_at(at)
    }

    /// Finish the file: write what remains and fix up the header. Returns
    /// the recording's length.
    pub fn finish(&self) -> std::io::Result<Duration> {
        let mut inner = self.inner.lock();
        let at = inner.start.elapsed();
        let now = inner.now_frames(at);
        // Agent audio still queued past now was never played.
        let end = inner.caller_end().max(inner.agent_end().min(now));
        inner.drain_to(end)?;
        let frames = inner.written;
        let rate = inner.rate;
        if let Some(mut writer) = inner.writer.take() {
            writer.flush()?;
            let mut file = writer.into_inner().map_err(std::io::IntoInnerError::into_error)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&wav_header(rate, frames))?;
            file.sync_all()?;
        }
        Ok(Duration::from_secs_f64(frames as f64 / f64::from(rate)))
    }

    pub(crate) fn caller_at(&self, at: Duration, samples: &[i16]) {
        let mut inner = self.inner.lock();
        let arrived = inner.now_frames(at);
        // The chunk ends now; anything between the last chunk and its start
        // is a gap in delivery.
        let begins = arrived.saturating_sub(samples.len() as u64);
        let pad = begins.saturating_sub(inner.caller_end());
        inner.caller.extend(std::iter::repeat_n(0, pad as usize));
        inner.caller.extend(samples);
        let safe = inner.caller_end().min(arrived);
        if let Err(e) = inner.drain_to(safe) {
            tracing::warn!("call recording stopped: {e}");
            inner.writer = None;
        }
    }

    pub(crate) fn agent_at(&self, at: Duration, samples: &[i16]) {
        let mut inner = self.inner.lock();
        let now = inner.now_frames(at);
        let pad = now.saturating_sub(inner.agent_end());
        inner.agent.extend(std::iter::repeat_n(0, pad as usize));
        inner.agent.extend(samples);
    }

    pub(crate) fn flush_at(&self, at: Duration) -> Duration {
        let mut inner = self.inner.lock();
        let now = inner.now_frames(at).max(inner.written);
        let keep = (now - inner.written) as usize;
        let dropped = inner.agent.len().saturating_sub(keep);
        inner.agent.truncate(keep);
        Duration::from_secs_f64(dropped as f64 / f64::from(inner.rate))
    }
}

fn wav_header(rate: u32, frames: u64) -> [u8; 44] {
    let data = u32::try_from(frames * 4).unwrap_or(u32::MAX - 36);
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // stereo
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&4u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn read(path: &Path) -> (u32, Vec<(i16, i16)>) {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2);
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), 44 + len);
        let frames = bytes[44..]
            .chunks_exact(4)
            .map(|f| {
                (
                    i16::from_le_bytes([f[0], f[1]]),
                    i16::from_le_bytes([f[2], f[3]]),
                )
            })
            .collect();
        (rate, frames)
    }

    #[test]
    fn caller_and_agent_are_aligned_on_the_call_clock() {
        let path = std::env::temp_dir().join(format!("rec-{}.wav", std::process::id()));
        let rec = CallRecorder::create(&path, 8_000).unwrap();
        // 20 ms of caller audio arrives at 20 ms, then nothing until 60 ms.
        rec.caller_at(ms(20), &[1; 160]);
        // The agent streams 100 ms of audio at once at 30 ms: it plays
        // from 30 ms to 130 ms.
        rec.agent_at(ms(30), &[2; 800]);
        rec.caller_at(ms(60), &[3; 160]);
        // The caller barges in at 80 ms: 50 ms of agent audio never played.
        let cut = rec.flush_at(ms(80));
        assert_eq!(cut, ms(50));
        rec.caller_at(ms(100), &[4; 160]);
        rec.finish().unwrap();

        let (rate, frames) = read(&path);
        assert_eq!(rate, 8_000);
        assert_eq!(frames.len(), 800); // 100 ms
        let at = |t_ms: usize| frames[t_ms * 8];
        assert_eq!(at(10), (1, 0));
        assert_eq!(at(30), (0, 2)); // delivery gap on the left, agent on the right
        assert_eq!(at(50), (3, 2));
        assert_eq!(at(85), (4, 0)); // agent cut at 80 ms
        let _ = std::fs::remove_file(path);
    }
}
