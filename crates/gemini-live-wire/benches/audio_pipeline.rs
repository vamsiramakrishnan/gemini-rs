//! Performance benchmarks for the audio pipeline and core operations.
//!
//! Run with: `cargo bench -p gemini-live-wire`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use gemini_live_wire::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};
use gemini_live_wire::protocol::types::{GeminiModel, SessionConfig, Voice};
use gemini_live_wire::session::SessionCommand;
use gemini_live_wire::transport::{Codec, JsonCodec};

// ---------------------------------------------------------------------------
// SPSC Ring Buffer benchmarks
// ---------------------------------------------------------------------------

fn bench_spsc_write_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("spsc_ring");

    // Benchmark different chunk sizes
    for chunk_size in [256, 512, 1024, 2048] {
        let ring = SpscRing::<i16>::new(4096);
        let data: Vec<i16> = (0..chunk_size).map(|i| i as i16).collect();
        let mut out = vec![0i16; chunk_size];

        group.throughput(Throughput::Elements(chunk_size as u64));
        group.bench_with_input(
            BenchmarkId::new("write_read", chunk_size),
            &chunk_size,
            |b, _| {
                b.iter(|| {
                    let written = ring.write(black_box(&data));
                    let read = ring.read(black_box(&mut out));
                    black_box((written, read));
                })
            },
        );
    }

    group.finish();
}

fn bench_spsc_write_only(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(8192);
    let data: Vec<i16> = (0..1024).map(|i| i as i16).collect();

    c.bench_function("spsc_write_1024_samples", |b| {
        b.iter(|| {
            // Drain to make room each iteration
            let mut drain = vec![0i16; 1024];
            ring.read(&mut drain);
            let written = ring.write(black_box(&data));
            black_box(written);
        })
    });
}

fn bench_spsc_contention(c: &mut Criterion) {
    // Benchmark rapid alternating write/read to simulate real-time audio streaming
    let ring = SpscRing::<i16>::new(4096);
    let chunk: Vec<i16> = (0..160).map(|i| i as i16).collect(); // 10ms at 16kHz
    let mut out = vec![0i16; 160];

    c.bench_function("spsc_10ms_write_read_cycle", |b| {
        b.iter(|| {
            ring.write(black_box(&chunk));
            ring.read(black_box(&mut out));
            black_box(&out);
        })
    });
}

// ---------------------------------------------------------------------------
// AudioJitterBuffer benchmarks
// ---------------------------------------------------------------------------

fn bench_jitter_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("jitter_buffer");

    // Different chunk sizes representing typical network packet sizes
    for (label, chunk_samples) in [
        ("32ms_16kHz", 512),   // 32ms at 16kHz
        ("64ms_16kHz", 1024),  // 64ms at 16kHz
        ("100ms_16kHz", 1600), // 100ms at 16kHz
        ("100ms_24kHz", 2400), // 100ms at 24kHz (Gemini output rate)
    ] {
        let chunk: Vec<i16> = (0..chunk_samples).map(|i| i as i16).collect();

        group.throughput(Throughput::Elements(chunk_samples as u64));
        group.bench_with_input(BenchmarkId::new("push", label), &chunk, |b, chunk| {
            let mut buf = AudioJitterBuffer::new(JitterConfig {
                sample_rate: 16000,
                min_depth_samples: 1600,
                max_depth_samples: 48000, // large to avoid overflow path
                jitter_alpha: 0.125,
                target_jitter_multiple: 2.0,
            });
            b.iter(|| {
                buf.push(black_box(chunk));
                // Periodically drain to prevent overflow
                if buf.depth() > 32000 {
                    let mut drain = vec![0i16; 16000];
                    buf.pull(&mut drain);
                }
            })
        });
    }

    group.finish();
}

fn bench_jitter_push_pull_cycle(c: &mut Criterion) {
    // Simulate the real-world pattern: push a network packet, pull a playback frame
    let push_chunk: Vec<i16> = (0..2400).map(|i| i as i16).collect(); // 100ms at 24kHz
    let mut pull_buf = vec![0i16; 480]; // 20ms playback frame at 24kHz

    c.bench_function("jitter_push_100ms_pull_20ms", |b| {
        let mut buf = AudioJitterBuffer::new(JitterConfig::for_sample_rate(24000));
        // Pre-fill to get into Playing state
        buf.push(&vec![0i16; 4800]);

        b.iter(|| {
            buf.push(black_box(&push_chunk));
            // Pull 5 playback frames (= 100ms consumed for 100ms pushed)
            for _ in 0..5 {
                let real = buf.pull(black_box(&mut pull_buf));
                black_box(real);
            }
        })
    });
}

fn bench_jitter_flush(c: &mut Criterion) {
    c.bench_function("jitter_flush_barge_in", |b| {
        let mut buf = AudioJitterBuffer::new(JitterConfig::for_sample_rate(24000));
        b.iter(|| {
            // Fill the buffer
            buf.push(&vec![42i16; 4800]);
            // Simulate barge-in flush
            buf.flush();
            black_box(buf.depth());
        })
    });
}

// ---------------------------------------------------------------------------
// JsonCodec benchmarks
// ---------------------------------------------------------------------------

fn bench_codec_encode_audio(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_encode");
    let codec = JsonCodec;
    let config = SessionConfig::new("test-key")
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck);

    // Benchmark different audio chunk sizes
    for (label, size) in [
        ("32ms_16kHz_16bit", 1024),  // 32ms of 16kHz 16-bit mono
        ("64ms_16kHz_16bit", 2048),  // 64ms
        ("100ms_16kHz_16bit", 3200), // 100ms
    ] {
        let audio_data = vec![0u8; size];
        let cmd = SessionCommand::SendAudio(audio_data);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("audio", label), &cmd, |b, cmd| {
            b.iter(|| {
                let result = codec.encode_command(black_box(cmd), &config).unwrap();
                black_box(result);
            })
        });
    }

    group.finish();
}

fn bench_codec_encode_text(c: &mut Criterion) {
    let codec = JsonCodec;
    let config = SessionConfig::new("test-key")
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck);
    let cmd = SessionCommand::SendText("Hello, how are you doing today?".to_string());

    c.bench_function("codec_encode_text", |b| {
        b.iter(|| {
            let result = codec.encode_command(black_box(&cmd), &config).unwrap();
            black_box(result);
        })
    });
}

fn bench_codec_encode_setup(c: &mut Criterion) {
    let codec = JsonCodec;
    let config = SessionConfig::new("test-key")
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck)
        .system_instruction("You are a helpful assistant.");

    c.bench_function("codec_encode_setup", |b| {
        b.iter(|| {
            let result = codec.encode_setup(black_box(&config)).unwrap();
            black_box(result);
        })
    });
}

fn bench_codec_decode_text(c: &mut Criterion) {
    let codec = JsonCodec;
    let msg = br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello world"}]},"turnComplete":true}}"#;

    c.bench_function("codec_decode_text_response", |b| {
        b.iter(|| {
            let result = codec.decode_message(black_box(msg)).unwrap();
            black_box(result);
        })
    });
}

fn bench_codec_decode_audio(c: &mut Criterion) {
    let codec = JsonCodec;
    // Simulate a server audio response with base64-encoded audio data.
    // Pre-compute a base64 string representing 3200 zero bytes.
    // base64 of all-zero bytes: each 3 zero bytes -> "AAAA". 3200/3 = 1066r2.
    let base64_audio = {
        let mut encoded = String::with_capacity(4268);
        for _ in 0..(3200 / 3) {
            encoded.push_str("AAAA");
        }
        encoded.push_str("AAA="); // 2 remaining bytes of zeros
        encoded
    };
    let msg = format!(
        r#"{{"serverContent":{{"modelTurn":{{"parts":[{{"inlineData":{{"mimeType":"audio/pcm","data":"{}"}}}}]}}}}}}"#,
        base64_audio
    );
    let msg_bytes = msg.into_bytes();

    c.bench_function("codec_decode_audio_response", |b| {
        b.iter(|| {
            let result = codec.decode_message(black_box(&msg_bytes)).unwrap();
            black_box(result);
        })
    });
}

fn bench_codec_decode_tool_call(c: &mut Criterion) {
    let codec = JsonCodec;
    let msg = br#"{"toolCall":{"functionCalls":[{"name":"get_weather","args":{"city":"London","units":"celsius"},"id":"call-abc123"}]}}"#;

    c.bench_function("codec_decode_tool_call", |b| {
        b.iter(|| {
            let result = codec.decode_message(black_box(msg)).unwrap();
            black_box(result);
        })
    });
}

fn bench_codec_decode_setup_complete(c: &mut Criterion) {
    let codec = JsonCodec;
    let msg = br#"{"setupComplete":{"sessionResumption":{"handle":"sess-handle-12345"}}}"#;

    c.bench_function("codec_decode_setup_complete", |b| {
        b.iter(|| {
            let result = codec.decode_message(black_box(msg)).unwrap();
            black_box(result);
        })
    });
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group!(
    spsc_benches,
    bench_spsc_write_read,
    bench_spsc_write_only,
    bench_spsc_contention,
);

criterion_group!(
    jitter_benches,
    bench_jitter_push,
    bench_jitter_push_pull_cycle,
    bench_jitter_flush,
);

criterion_group!(
    codec_benches,
    bench_codec_encode_audio,
    bench_codec_encode_text,
    bench_codec_encode_setup,
    bench_codec_decode_text,
    bench_codec_decode_audio,
    bench_codec_decode_tool_call,
    bench_codec_decode_setup_complete,
);

criterion_main!(spsc_benches, jitter_benches, codec_benches);
