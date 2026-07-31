//! Speaking to the assistant, rather than typing at it.
//!
//! Every Live test in this crate so far calls `send_text`, which is convenient
//! and quietly skips the half of the system a voice product actually runs on.
//! Text arrives whole, punctuated, correctly spelled and with turn boundaries
//! the caller chose. Speech arrives as a stream of PCM frames, gets segmented
//! by the server's voice-activity detector, and reaches the model as an ASR
//! transcript that may say "cortado" or "quarter dough".
//!
//! Memory retrieval is *downstream of that transcript*. A recall that works on
//! `"what's my usual coffee order"` and fails on what the recogniser actually
//! produced is a recall that does not work, and no amount of text-driven
//! testing would say so.
//!
//! So this synthesises the user's side with Gemini's TTS models and feeds the
//! audio in as a real microphone would.
//!
//! # Format
//!
//! TTS returns `audio/l16` — 24 kHz, mono, signed 16-bit little-endian PCM. The
//! Live API expects **16 kHz** input, so everything is resampled on the way
//! through. That ratio is exactly 3:2, and [`resample_24k_to_16k`] takes every
//! other input sample pair to two output samples with linear interpolation,
//! which is mild enough low-pass to keep an ASR happy without a real filter
//! design.
//!
//! Audio is cached on disk by content hash, because synthesis is a paid call
//! and the same sentence is spoken on every run.

#![allow(dead_code)]

use std::path::PathBuf;

use gemini_memory_rs::core::stable_hash;

/// The TTS model. `flash` rather than `pro`: this is a test fixture speaking,
/// not a product voice, and it is a third of the latency.
const TTS_MODEL: &str = "gemini-2.5-flash-preview-tts";

/// What the Live API wants on the way in.
pub const LIVE_INPUT_HZ: u32 = 16_000;
/// What the TTS models emit.
pub const TTS_OUTPUT_HZ: u32 = 24_000;

/// How much audio to send per frame, in milliseconds.
///
/// Twenty is what a browser's `ScriptProcessor` or a phone's mic callback
/// typically delivers, so sending in that shape exercises the same buffering
/// the real client hits rather than one giant write the server would never see.
pub const FRAME_MS: usize = 20;

fn api_key() -> Option<String> {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
}

fn cache_path(text: &str, voice: &str) -> PathBuf {
    let key = stable_hash(&format!("{TTS_MODEL}|{voice}|{text}"));
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("voice-{key}.pcm"))
}

/// Synthesise `text` and return 16 kHz mono PCM, ready for `send_audio`.
///
/// Returns `None` without an API key, so a caller can skip rather than fail.
pub async fn speak(text: &str, voice: &str) -> Option<Vec<u8>> {
    let path = cache_path(text, voice);
    if let Ok(cached) = std::fs::read(&path) {
        if !cached.is_empty() {
            return Some(cached);
        }
    }
    let key = api_key()?;

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": TTS_MODEL,
        "input": text,
        "response_format": { "type": "audio" },
        // Sampling left at the model's default throughout this crate.
        "generation_config": { "speech_config": [{ "voice": voice }] },
    });

    let mut backoff = std::time::Duration::from_millis(500);
    for attempt in 0..4 {
        let response = client
            .post("https://generativelanguage.googleapis.com/v1beta/interactions")
            .header("x-goog-api-key", &key)
            .json(&body)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let json: serde_json::Value = response.json().await.ok()?;
                let encoded = json["steps"][0]["content"][0]["data"].as_str()?;
                let raw = base64_decode(encoded)?;
                let pcm = resample_24k_to_16k(&raw);
                let _ = std::fs::write(&path, &pcm);
                return Some(pcm);
            }
            Ok(response) if attempt == 3 => {
                eprintln!(
                    "  TTS failed: {} — giving up on {text:?}",
                    response.status()
                );
            }
            _ => {}
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    None
}

/// 24 kHz to 16 kHz, linear interpolation over 16-bit little-endian samples.
///
/// Three input samples become two output samples. Nearest-neighbour decimation
/// would alias audibly at this ratio and ASR quality would drop for a reason
/// that had nothing to do with the system under test.
pub fn resample_24k_to_16k(input: &[u8]) -> Vec<u8> {
    let samples: Vec<i16> = input
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    if samples.is_empty() {
        return Vec::new();
    }
    let out_len = samples.len() * 2 / 3;
    let mut out = Vec::with_capacity(out_len * 2);
    for i in 0..out_len {
        // Source position for output sample i, at a 3:2 ratio.
        let position = i as f32 * 1.5;
        let left = position.floor() as usize;
        let right = (left + 1).min(samples.len() - 1);
        let fraction = position - left as f32;
        let value = samples[left] as f32 * (1.0 - fraction) + samples[right] as f32 * fraction;
        out.extend_from_slice(&(value as i16).to_le_bytes());
    }
    out
}

/// Split PCM into [`FRAME_MS`] frames, as a microphone would deliver it.
pub fn frames(pcm: &[u8]) -> Vec<Vec<u8>> {
    // 16 kHz × 2 bytes × 20 ms = 640 bytes.
    let frame = (LIVE_INPUT_HZ as usize / 1000) * 2 * FRAME_MS;
    pcm.chunks(frame).map(<[u8]>::to_vec).collect()
}

/// How much silence to stream after the utterance, in milliseconds.
///
/// Not a pause — actual zero-valued frames, sent at the same pace.
///
/// Server-side voice activity detection decides an utterance has ended by
/// *hearing* a stretch of silence, not by noticing that packets stopped
/// arriving. A client that sends two seconds of speech and then goes quiet on
/// the socket looks like a client whose network stalled mid-word, so the server
/// waits. A real microphone never stops delivering frames; it delivers frames
/// of near-zero amplitude. This reproduces that.
pub const TRAILING_SILENCE_MS: usize = 700;

/// Send `pcm` to a live session in real time, followed by trailing silence.
///
/// Paced rather than dumped, for two reasons. The obvious one is that a real
/// microphone delivers 20 ms at a time. The load-bearing one is that VAD
/// segments on what it hears over time, so delivering the whole utterance in
/// one write puts it inside a single window and moves the turn boundary — or
/// removes it.
pub async fn say(
    handle: &gemini_adk_rs::live::LiveHandle,
    pcm: &[u8],
) -> Result<(), gemini_adk_rs::error::AgentError> {
    let frame_bytes = (LIVE_INPUT_HZ as usize / 1000) * 2 * FRAME_MS;
    let mut all = pcm.to_vec();
    all.resize(
        all.len() + frame_bytes * (TRAILING_SILENCE_MS / FRAME_MS),
        0u8,
    );

    for frame in frames(&all) {
        handle.send_audio(frame).await?;
        tokio::time::sleep(std::time::Duration::from_millis(FRAME_MS as u64)).await;
    }
    Ok(())
}

/// Minimal standard-alphabet base64 decoder.
///
/// Hand-rolled to avoid adding a dependency to the dev tree for one call site.
fn base64_decode(encoded: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => (byte - b'A') as u32,
            b'a'..=b'z' => (byte - b'a') as u32 + 26,
            b'0'..=b'9' => (byte - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let cleaned: Vec<u8> = encoded
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    for chunk in cleaned.chunks(4) {
        let mut packed = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            packed |= value(*byte)? << (18 - 6 * i);
        }
        let bytes = [(packed >> 16) as u8, (packed >> 8) as u8, packed as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_preserves_duration_at_the_new_rate() {
        // One second at 24 kHz, 16-bit mono.
        let input = vec![0u8; 24_000 * 2];
        let output = resample_24k_to_16k(&input);
        assert_eq!(
            output.len(),
            16_000 * 2,
            "one second in must be one second out"
        );
    }

    #[test]
    fn resampling_a_ramp_stays_monotonic() {
        // A rising ramp must not develop reversals; that would be an indexing
        // bug producing audible artefacts an ASR would stumble on.
        let samples: Vec<u8> = (0..3_000i16).flat_map(|i| (i * 10).to_le_bytes()).collect();
        let output = resample_24k_to_16k(&samples);
        let values: Vec<i16> = output
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert!(
            values.windows(2).all(|w| w[1] >= w[0]),
            "resampling a monotonic ramp produced a reversal"
        );
    }

    #[test]
    fn frames_are_twenty_milliseconds_of_sixteen_kilohertz_audio() {
        let pcm = vec![0u8; 640 * 5];
        let framed = frames(&pcm);
        assert_eq!(framed.len(), 5);
        assert!(framed.iter().all(|f| f.len() == 640));
    }

    #[test]
    fn base64_round_trips_the_bytes_it_is_given() {
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("YQ==").as_deref(), Some(&b"a"[..]));
        assert_eq!(base64_decode("YWI=").as_deref(), Some(&b"ab"[..]));
    }
}
