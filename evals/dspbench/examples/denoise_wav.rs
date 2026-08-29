//! Run a real recording through the production `hpf+denoise` chain.
//!
//! Usage: `denoise_wav <in.wav> <out.wav>` — input must be mono 16 kHz
//! PCM16. The file is framed at 20 ms and pushed through the same
//! `InputAudioProcessor` path a live session uses, so the output is
//! exactly what the model would hear.

use gemini_adk_fluent_rs::voice::dsp::{stages::HighPass, DspChain, IntStage};
use gemini_adk_fluent_rs::voice::Denoiser;
use gemini_adk_rs::live::InputAudioProcessor;

const SR: u32 = 16_000;
const FRAME: usize = 320; // 20 ms

fn main() {
    let mut args = std::env::args().skip(1);
    let (input, output) = match (args.next(), args.next()) {
        (Some(i), Some(o)) => (i, o),
        _ => {
            eprintln!("usage: denoise_wav <in.wav> <out.wav>");
            std::process::exit(2);
        }
    };

    let mut samples = read_wav_mono_16k(&input);
    let mut chain = DspChain::new(SR)
        .stage(HighPass::speech(SR))
        .stage(IntStage::named(Denoiser::new(SR), "denoise").with_latency(160));

    let mut out: Vec<i16> = Vec::with_capacity(samples.len());
    let mut frame: Vec<i16> = Vec::with_capacity(FRAME);
    for block in samples.chunks(FRAME) {
        frame.clear();
        frame.extend_from_slice(block);
        chain.process_frame(&mut frame);
        out.extend_from_slice(&frame);
    }
    samples.clear();

    write_wav(&output, &out);
    eprintln!(
        "{input} -> {output}  ({} samples, chain latency {} ms)",
        out.len(),
        chain.total_latency_samples() * 1000 / SR as usize
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
fn write_wav(path: &str, samples: &[i16]) {
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
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
}
