//! Pair 1: lowering a ~9 KB text, one character at a time.
//!
//! `plus_to_string` reallocates and re-copies the whole prefix per character,
//! `push` grows one amortized buffer, `reuse_buffer` writes into a buffer that
//! is kept across iterations.
//!
//! Run just this pair with: cargo bench -- string_build

use compare::{lower_into, lower_plus_to_string, lower_push};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_string_build(c: &mut Criterion) {
    let text = support::sample_text();
    let mut group = c.benchmark_group("string_build");

    group.bench_function("plus_to_string", |b| {
        b.iter(|| black_box(lower_plus_to_string(black_box(text.as_str()))));
    });

    group.bench_function("push", |b| {
        b.iter(|| black_box(lower_push(black_box(text.as_str()))));
    });

    let mut scratch = String::with_capacity(text.len() + 8);
    group.bench_function("reuse_buffer", |b| {
        b.iter(|| {
            lower_into(black_box(text.as_str()), &mut scratch);
            black_box(scratch.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_string_build);
criterion_main!(benches);
