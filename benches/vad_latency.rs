//! Benchmark: VAD processing latency per frame.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

#[cfg(feature = "vad")]
fn bench_vad_silence(c: &mut Criterion) {
    use gemini_genai_rs::vad::{VadConfig, VoiceActivityDetector};

    let config = VadConfig {
        sample_rate: 16000,
        frame_duration_ms: 30,
        ..Default::default()
    };
    let mut vad = VoiceActivityDetector::new(config);
    let silence = vec![0i16; 480]; // 30ms @ 16kHz

    c.bench_function("vad_process_silence_30ms", |b| {
        b.iter(|| {
            vad.process_frame(black_box(&silence));
        })
    });
}

#[cfg(feature = "vad")]
fn bench_vad_speech(c: &mut Criterion) {
    use gemini_genai_rs::vad::{VadConfig, VoiceActivityDetector};

    let config = VadConfig {
        sample_rate: 16000,
        frame_duration_ms: 30,
        ..Default::default()
    };
    let mut vad = VoiceActivityDetector::new(config);
    // Synthetic speech-like signal
    let speech: Vec<i16> = (0..480)
        .map(|i| ((i as f64 * 0.1).sin() * 10000.0) as i16)
        .collect();

    c.bench_function("vad_process_speech_30ms", |b| {
        b.iter(|| {
            vad.process_frame(black_box(&speech));
        })
    });
}

#[cfg(feature = "vad")]
criterion_group!(benches, bench_vad_silence, bench_vad_speech);

#[cfg(not(feature = "vad"))]
fn bench_placeholder(c: &mut Criterion) {
    c.bench_function("vad_disabled", |b| b.iter(|| {}));
}

#[cfg(not(feature = "vad"))]
criterion_group!(benches, bench_placeholder);

criterion_main!(benches);
