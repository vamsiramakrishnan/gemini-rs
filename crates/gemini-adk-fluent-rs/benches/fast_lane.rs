//! End-to-end cost of one fast-lane audio frame, through the public API.
//!
//! A scripted server streams audio frames over an in-memory transport into a
//! `Live` session whose `on_audio` callback counts them. The number reported
//! is the per-frame wall time from the transport to the callback, which
//! covers the codec, the router, the fast-lane channel and the dispatch. The
//! lane's own budget (under one millisecond per callback) is asserted by
//! `fast_lane_budget_tests` in `gemini-adk-rs`; this benchmark tracks the
//! whole path so a regression anywhere on it shows up as a number.
//!
//! `cargo bench -p gemini-adk-fluent-rs --bench fast_lane`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::json;

/// 40 ms of 24 kHz PCM16: 1 920 bytes, the size the model streams.
const FRAME_BYTES: usize = 1_920;

fn script(frames: usize) -> ScriptedServer {
    let data = base64::engine::general_purpose::STANDARD.encode(vec![0u8; FRAME_BYTES]);
    let mut server = ScriptedServer::new().settle_after(Duration::from_millis(20));
    for _ in 0..frames {
        server = server.frame(json!({
            "serverContent": { "modelTurn": { "parts": [
                { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": data } }
            ] } }
        }));
    }
    server.turn_complete()
}

fn audio_frames(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut group = c.benchmark_group("fast_lane");
    for &frames in &[250usize, 1_000] {
        group.throughput(Throughput::Elements(frames as u64));
        group.bench_function(format!("audio_frames/{frames}"), |b| {
            b.iter(|| {
                runtime.block_on(async {
                    let seen = Arc::new(AtomicUsize::new(0));
                    let seen_cb = seen.clone();
                    let run = script(frames)
                        .play(Live::builder().instruction("bench").on_audio(move |_| {
                            seen_cb.fetch_add(1, Ordering::Relaxed);
                        }))
                        .await
                        .expect("script plays");
                    assert_eq!(
                        seen.load(Ordering::Relaxed),
                        frames,
                        "every frame reached the callback"
                    );
                    run.disconnect().await;
                });
            });
        });
    }
    group.finish();
}

criterion_group!(benches, audio_frames);
criterion_main!(benches);
