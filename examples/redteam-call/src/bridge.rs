//! The cross-connect: one session's mouth wired to the other's ear.
//!
//! # Why this is not just `a.on_audio(|b| b.send_audio(b))`
//!
//! Three things stand between the two sessions, and skipping any of them
//! produces a call that looks plumbed and behaves nothing like one.
//!
//! **Rate.** The Live API emits 24 kHz and accepts 16 kHz. Feeding output
//! straight back in plays every utterance 1.5× fast and pitched up, which the
//! recogniser on the far side transcribes as approximately nothing.
//!
//! **Pacing.** The model can emit a five-second utterance in two seconds.
//! Forwarding it as fast as it arrives hands the far side a burst its
//! voice-activity detector reads as one short blurt, and turn segmentation
//! collapses. So the bridge is a jitter buffer drained on a 20 ms wall-clock
//! tick: audio leaves at the rate speech is spoken, however fast it arrived.
//!
//! **Silence.** When a speaker stops, server VAD needs to *hear* the gap to
//! close the utterance. Sending nothing is not silence — it is absence, and the
//! far side simply waits. So the pump emits silent frames when the buffer is
//! empty. The line is open for the whole call, exactly like a phone: both ends
//! are always receiving, which is also what makes barge-in possible at all.
//!
//! # Barge-in
//!
//! When a session is interrupted, everything still queued for the far side is
//! speech that was never finished, and playing it on is the one unforgivable
//! sin of a voice UI. So [`Line::flush`] does not set a flag for the pump to
//! notice later: it puts a marker *in the queue*. A flag is read at the next
//! tick, and audio from the replacement generation can arrive inside that
//! window — the pump would then clear the new utterance along with the old one,
//! swallowing its opening. The marker is ordered against the audio around it,
//! so exactly the frames queued before the interruption are dropped.
//!
//! # Deliberately full duplex
//!
//! Nothing here stops both sides talking at once. Half-duplex floor control —
//! forward only from whoever has the token — would produce tidier transcripts
//! and would also erase the most interesting failure the example can show: two
//! VAD-driven agents deadlocking on politeness, or talking over each other for
//! thirty seconds. If they collide, that is a finding about the pair, not an
//! artefact of the harness, and the stereo recording is where you hear it.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use gemini_adk_fluent_rs::voice::{SESSION_INPUT_HZ, SESSION_OUTPUT_HZ, resample};
use gemini_adk_rs::live::LiveHandle;
use gemini_genai_rs::prelude::{bytes_to_i16, i16_to_bytes};
use parking_lot::Mutex;
use tokio::sync::mpsc;

/// Wall-clock period of one forwarded frame.
const FRAME_MS: u64 = 20;
/// Samples in one frame at the session input rate.
const FRAME_SAMPLES: usize = (SESSION_INPUT_HZ as usize / 1000) * FRAME_MS as usize;

/// How much undelivered speech the jitter buffer will hold, in seconds.
///
/// Reached only if a model sustains generation faster than real time for this
/// long, which in practice means something has gone wrong upstream. The cap
/// stops one runaway session growing the buffer for the length of the call;
/// the drop is counted so the report can say it happened rather than leaving a
/// mysterious gap in the audio.
const BUFFER_CAP_SECS: usize = 30;
const BUFFER_CAP: usize = SESSION_INPUT_HZ as usize * BUFFER_CAP_SECS;

/// Capacity of the fast-lane hand-off queue, in chunks.
///
/// `on_audio` runs on the event-dispatch hot path and must not block, so it
/// `try_send`s and gives up rather than waiting. A drop here is a real gap in
/// the far side's audio, so it is counted too.
const HANDOFF_CHUNKS: usize = 256;

/// What travels down a line: speech, or the boundary that ends it.
enum Chunk {
    /// A chunk of the speaker's 24 kHz output.
    Audio(Vec<u8>),
    /// Everything queued before this point was interrupted and must be dropped.
    Flush,
}

/// One direction of the call.
#[derive(Clone)]
pub struct Line {
    tx: mpsc::Sender<Chunk>,
    dropped: Arc<AtomicUsize>,
}

impl Line {
    /// Hand a chunk of the speaker's 24 kHz output to the far side.
    ///
    /// Safe to call from a fast-lane `on_audio` callback: it never blocks,
    /// allocates only the copy the channel needs, and drops rather than waits
    /// if the pump has fallen behind.
    pub fn feed(&self, pcm24: &[u8]) {
        if self.tx.try_send(Chunk::Audio(pcm24.to_vec())).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drop everything queued: the speaker was interrupted and the rest of that
    /// utterance is never going to be said.
    ///
    /// Awaits rather than trying, because this runs on the control lane where
    /// blocking is allowed, and a barge-in dropped because the queue was
    /// momentarily full is the failure this exists to prevent.
    pub async fn flush(&self) {
        let _ = self.tx.send(Chunk::Flush).await;
    }

    /// Chunks lost because the pump could not keep up.
    pub fn dropped_chunks(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The audio actually put on the wire, one mono track per direction.
///
/// Recorded at the point of transmission rather than at the point of
/// generation, so it is the paced, resampled, post-flush stream — what the far
/// side really heard, including the silences. Both tracks advance on their own
/// 20 ms tick, which makes them close enough to aligned to mix into a stereo
/// file where crosstalk is audible as both channels talking at once.
#[derive(Default)]
pub struct Tape {
    samples: Mutex<Vec<i16>>,
}

impl Tape {
    fn write(&self, frame: &[i16]) {
        self.samples.lock().extend_from_slice(frame);
    }

    /// Everything recorded, at [`SESSION_INPUT_HZ`].
    pub fn take(&self) -> Vec<i16> {
        std::mem::take(&mut *self.samples.lock())
    }
}

/// Start pumping one direction, and return the handle the speaker feeds.
///
/// The returned [`Line`] is cheap to clone and safe to hold in a fast-lane
/// callback. The task runs until `stop` is set, then returns.
pub fn spawn(
    peer: LiveHandle,
    tape: Arc<Tape>,
    stop: Arc<AtomicBool>,
) -> (Line, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Chunk>(HANDOFF_CHUNKS);
    let dropped = Arc::new(AtomicUsize::new(0));

    let line = Line {
        tx,
        dropped: dropped.clone(),
    };

    let task = tokio::spawn(async move {
        let mut buf: VecDeque<i16> = VecDeque::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(FRAME_MS));
        // The pump is a clock, not a catch-up loop: a tick missed because the
        // socket blocked should be skipped, not repaid as a burst that undoes
        // the pacing this whole task exists to provide.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut open = true;
        let mut frame = vec![0i16; FRAME_SAMPLES];

        loop {
            tokio::select! {
                // Ingest before emitting, so audio that arrived during this
                // tick goes out on it rather than waiting for the next.
                biased;

                chunk = rx.recv(), if open => match chunk {
                    Some(Chunk::Audio(chunk)) => {
                        let Some(pcm24) = bytes_to_i16(&chunk) else { continue };
                        buf.extend(resample(pcm24, SESSION_OUTPUT_HZ, SESSION_INPUT_HZ));
                        if buf.len() > BUFFER_CAP {
                            let excess = buf.len() - BUFFER_CAP;
                            buf.drain(..excess);
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // Ordered against the audio around it: everything queued
                    // before the interruption goes, everything after stays.
                    Some(Chunk::Flush) => buf.clear(),
                    None => open = false,
                },

                _ = ticker.tick() => {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    for slot in frame.iter_mut() {
                        *slot = buf.pop_front().unwrap_or(0);
                    }
                    tape.write(&frame);
                    if peer.send_audio(i16_to_bytes(&frame).to_vec()).await.is_err() {
                        // The far side is gone. Nothing left to pump into.
                        return;
                    }
                }
            }
        }
    });

    (line, task)
}

/// Interleave the two tracks into a stereo WAV: collector left, caller right.
///
/// Separated rather than mixed down on purpose. A mono mix of two people
/// talking over each other is just noise; in stereo you can hear which one
/// started it.
pub fn stereo_wav(left: &[i16], right: &[i16]) -> Vec<u8> {
    let frames = left.len().max(right.len());
    let mut pcm = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        pcm.push(left.get(i).copied().unwrap_or(0));
        pcm.push(right.get(i).copied().unwrap_or(0));
    }

    let data = i16_to_bytes(&pcm);
    let channels: u16 = 2;
    let bits: u16 = 16;
    let byte_rate = SESSION_INPUT_HZ * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;

    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&SESSION_INPUT_HZ.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(data);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_twenty_milliseconds_at_the_session_input_rate() {
        assert_eq!(FRAME_SAMPLES, 320);
    }

    #[test]
    fn stereo_wav_has_a_correct_riff_header_and_interleaves_both_tracks() {
        let wav = stereo_wav(&[1, 2, 3], &[4, 5, 6]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");

        let declared = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(declared, wav.len() - 44, "data size must match the payload");
        assert_eq!(declared, 3 * 2 * 2, "three stereo frames of 16-bit samples");

        let pcm = bytes_to_i16(&wav[44..]).expect("even byte count");
        assert_eq!(pcm, [1, 4, 2, 5, 3, 6], "left and right must interleave");
    }

    /// A call where one side stops talking first leaves one track shorter.
    /// Truncating to the shorter one would cut the tail off the conversation.
    #[test]
    fn stereo_wav_pads_the_shorter_track_rather_than_truncating() {
        let wav = stereo_wav(&[1, 2, 3, 4], &[9]);
        let pcm = bytes_to_i16(&wav[44..]).expect("even byte count");
        assert_eq!(pcm, [1, 9, 2, 0, 3, 0, 4, 0]);
    }
}
