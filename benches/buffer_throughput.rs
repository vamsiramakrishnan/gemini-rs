//! Benchmark: SPSC ring buffer throughput.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gemini_genai_rs::buffer::SpscRing;

fn bench_spsc_write(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 1600]; // 100ms @ 16kHz

    c.bench_function("spsc_write_100ms_16kHz", |b| {
        b.iter(|| {
            ring.write(black_box(&data));
            ring.clear();
        })
    });
}

fn bench_spsc_read(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 1600];
    let mut out = vec![0i16; 1600];

    c.bench_function("spsc_write_read_100ms_16kHz", |b| {
        b.iter(|| {
            ring.write(black_box(&data));
            ring.read(black_box(&mut out));
        })
    });
}

fn bench_spsc_write_small(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 160]; // 10ms @ 16kHz

    c.bench_function("spsc_write_10ms_16kHz", |b| {
        b.iter(|| {
            ring.write(black_box(&data));
            ring.clear();
        })
    });
}

fn bench_spsc_write_24k(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 2400]; // 100ms @ 24kHz

    c.bench_function("spsc_write_100ms_24kHz", |b| {
        b.iter(|| {
            ring.write(black_box(&data));
            ring.clear();
        })
    });
}

criterion_group!(
    benches,
    bench_spsc_write,
    bench_spsc_read,
    bench_spsc_write_small,
    bench_spsc_write_24k,
);
criterion_main!(benches);
