//! Pair 2: building a 1000-element `Vec<u64>`.
//!
//! `new_push` starts empty and relies on amortized doubling, `with_capacity`
//! reserves exactly, `extend` lets `Extend` read the exact size hint, and
//! `collect` is the idiomatic baseline that takes the same reservation path as
//! `extend`.
//!
//! Run just this pair with: cargo bench -- vec_growth

use compare::{expand_collect, expand_extend, expand_new_push, expand_with_capacity};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_vec_growth(c: &mut Criterion) {
    let input = support::numbers(1000);
    let mut group = c.benchmark_group("vec_growth");

    group.bench_function("new_push", |b| {
        b.iter(|| black_box(expand_new_push(black_box(input.as_slice()))));
    });

    group.bench_function("with_capacity", |b| {
        b.iter(|| black_box(expand_with_capacity(black_box(input.as_slice()))));
    });

    group.bench_function("extend", |b| {
        b.iter(|| black_box(expand_extend(black_box(input.as_slice()))));
    });

    group.bench_function("collect", |b| {
        b.iter(|| black_box(expand_collect(black_box(input.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_vec_growth);
criterion_main!(benches);
