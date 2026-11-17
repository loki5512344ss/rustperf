//! Pair 3: summing name lengths over 1000 records, cloning each name or
//! borrowing it.
//!
//! `owned` allocates and copies a `String` per record per pass; `borrowed`
//! hands out `&str`. Same sum, very different allocation traffic.
//!
//! Run just this pair with: cargo bench -- clone_vs_borrow

use compare::{sum_name_lens_borrowed, sum_name_lens_owned};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_clone_vs_borrow(c: &mut Criterion) {
    let records = support::records(1000);
    let mut group = c.benchmark_group("clone_vs_borrow");

    group.bench_function("clone_owned", |b| {
        b.iter(|| black_box(sum_name_lens_owned(black_box(records.as_slice()))));
    });

    group.bench_function("borrow", |b| {
        b.iter(|| black_box(sum_name_lens_borrowed(black_box(records.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_clone_vs_borrow);
criterion_main!(benches);
