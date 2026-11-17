//! Pair 5: joining 512 `u32` values as `"v,v,v,"`.
//!
//! `format_in_loop` allocates a temporary `String` per value and a new output
//! `String` per concatenation; `write_macro` formats into the output buffer;
//! `manual_digits` extracts decimal digits by hand.
//!
//! Run just this pair with: cargo bench -- format_loop

use compare::{join_numbers_format, join_numbers_manual, join_numbers_write};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_format_loop(c: &mut Criterion) {
    // A mix of small, medium and ten-digit values: a uniform u32 range would
    // hide the per-digit cost behind always-long numbers.
    let values: Vec<u32> = support::numbers(512)
        .into_iter()
        .enumerate()
        .map(|(i, value)| (value as u32) >> (i % 3))
        .collect();

    let mut group = c.benchmark_group("format_loop");

    group.bench_function("format_in_loop", |b| {
        b.iter(|| black_box(join_numbers_format(black_box(values.as_slice()))));
    });

    group.bench_function("write_macro", |b| {
        b.iter(|| black_box(join_numbers_write(black_box(values.as_slice()))));
    });

    group.bench_function("manual_digits", |b| {
        b.iter(|| black_box(join_numbers_manual(black_box(values.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_format_loop);
criterion_main!(benches);
