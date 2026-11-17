//! Pair 4: summing a mapped 4096-element slice.
//!
//! `collect_then_sum` materializes an intermediate `Vec`; `fused_sum` and
//! `fused_for` fold the same computation with no intermediate storage.
//!
//! Run just this pair with: cargo bench -- map_collect

use compare::{sum_mixed_collected, sum_mixed_for, sum_mixed_fused};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_map_collect(c: &mut Criterion) {
    let input = support::numbers(4096);
    let mut group = c.benchmark_group("map_collect");

    group.bench_function("collect_then_sum", |b| {
        b.iter(|| black_box(sum_mixed_collected(black_box(input.as_slice()))));
    });

    group.bench_function("fused_sum", |b| {
        b.iter(|| black_box(sum_mixed_fused(black_box(input.as_slice()))));
    });

    group.bench_function("fused_for", |b| {
        b.iter(|| black_box(sum_mixed_for(black_box(input.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_map_collect);
criterion_main!(benches);
