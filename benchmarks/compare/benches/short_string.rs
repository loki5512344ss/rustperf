//! Pair 9: 1000 short values (all under 24 bytes) stored three ways.
//!
//! `inline` keeps the bytes in the value, `vec_u8` allocates once per value,
//! `string` is the same thing with UTF-8 guidance. Each side builds the
//! collection and then folds every byte into a checksum.
//!
//! Run just this pair with: cargo bench -- short_string

use compare::{short_checksum_inline, short_checksum_string, short_checksum_vec};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_short_string(c: &mut Criterion) {
    let values = support::short_strings(1000);
    let mut group = c.benchmark_group("short_string");

    group.bench_function("inline_array", |b| {
        b.iter(|| black_box(short_checksum_inline(black_box(values.as_slice()))));
    });

    group.bench_function("vec_u8", |b| {
        b.iter(|| black_box(short_checksum_vec(black_box(values.as_slice()))));
    });

    group.bench_function("string", |b| {
        b.iter(|| black_box(short_checksum_string(black_box(values.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_short_string);
criterion_main!(benches);
