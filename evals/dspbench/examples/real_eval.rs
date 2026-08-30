//! Score a real recording through a chain variant, decision-level.
//!
//! Usage:
//!   real_eval <variant> <in.wav> <truth> [out.wav]
//!
//! * `variant` — `raw` | `hpf` | `hpf_denoise` | `full`
//!   (`full` = hpf → RNNoise denoise → AGC → limiter, the non-echo chain)
//! * `in.wav` — mono 16 kHz PCM16 mixture
//! * `truth`  — ground-truth speech intervals as `start:end,start:end,…`
//!   in sample indices of the input timeline
//! * `out.wav` — optional processed-audio dump
//!
//! Prints one JSON object to stdout: the same `DecisionScore` the bench
//! computes (shipped energy VAD, `noisy_street` profile, 120 ms tolerance,
//! truth shifted by the chain's declared latency) plus latency and level.
//! Framing is 20 ms through the production `InputAudioProcessor` path.

#[path = "../src/metrics.rs"]
#[allow(dead_code)]
mod metrics;

use gemini_adk_fluent_rs::voice::dsp::{
    stages::{Agc, HighPass, Limiter},
    DspChain, IntStage,
};
use gemini_adk_fluent_rs::voice::Denoiser;
use gemini_adk_rs::live::InputAudioProcessor;
use gemini_genai_rs::vad::VadConfig;

const SR: u32 = 16_000;
const FRAME: usize = 320; // 20 ms

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: real_eval <raw|hpf|hpf_denoise|full> <in.wav> <truth> [out.wav]");
        std::process::exit(2);
    }
    let variant = args[0].as_str();
    let samples = read_wav_mono_16k(&args[1]);
    let truth: Vec<(usize, usize)> = args[2]
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (a, b) = pair.split_once(':').expect("truth format start:end");
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();

    let mut chain = DspChain::new(SR);
    match variant {
        "raw" => {}
        "hpf" => chain = chain.stage(HighPass::speech(SR)),
        "hpf_denoise" => {
            chain = chain
                .stage(HighPass::speech(SR))
                .stage(IntStage::named(Denoiser::new(SR), "denoise").with_latency(160));
        }
        "full" => {
            chain = chain
                .stage(HighPass::speech(SR))
                .stage(IntStage::named(Denoiser::new(SR), "denoise").with_latency(160))
                .stage(Agc::speech_default(SR))
                .stage(Limiter::speech_default(SR));
        }
        other => {
            eprintln!("unknown variant {other}");
            std::process::exit(2);
        }
    }

    let mut processed: Vec<f32> = Vec::with_capacity(samples.len());
    let mut frame: Vec<i16> = Vec::with_capacity(FRAME);
    for block in samples.chunks(FRAME) {
        frame.clear();
        frame.extend_from_slice(block);
        chain.process_frame(&mut frame);
        processed.extend(frame.iter().map(|&s| f32::from(s) / 32768.0));
    }

    let latency = chain.total_latency_samples();
    let decision = metrics::score_vad(
        &processed,
        SR,
        &truth,
        VadConfig::noisy_street(),
        latency,
        120,
    );
    let rms = (processed.iter().map(|&s| s * s).sum::<f32>() / processed.len().max(1) as f32)
        .sqrt()
        .max(1e-9);

    if let Some(out) = args.get(3) {
        write_wav(out, &processed);
    }

    println!(
        "{}",
        serde_json::json!({
            "variant": variant,
            "decision": decision,
            "latency_samples": latency,
            "rms_dbfs": 20.0 * rms.log10(),
        })
    );
}

/// Minimal RIFF reader: mono 16 kHz PCM16 only, walks chunks to `data`.
fn read_wav_mono_16k(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(
        bytes.len() > 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{path}: not a RIFF/WAVE file"
    );
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);

    let mut pos = 12;
    let mut data: Option<(usize, usize)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            let format = u16_at(body);
            let channels = u16_at(body + 2);
            let rate = u32_at(body + 4);
            let bits = u16_at(body + 14);
            assert!(
                format == 1 && channels == 1 && rate == SR && bits == 16,
                "{path}: need mono 16 kHz PCM16, got fmt {format} ch {channels} {rate} Hz {bits} bit"
            );
        } else if id == b"data" {
            data = Some((body, len.min(bytes.len() - body)));
        }
        pos = body + len + (len & 1);
    }
    let (start, len) = data.unwrap_or_else(|| panic!("{path}: no data chunk"));
    bytes[start..start + len]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Minimal RIFF/PCM16 mono writer at the chain rate.
fn write_wav(path: &str, samples: &[f32]) {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SR.to_le_bytes());
    bytes.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let q = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        bytes.extend_from_slice(&q.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
}
