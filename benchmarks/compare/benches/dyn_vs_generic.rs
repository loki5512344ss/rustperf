//! Pair 8: driving a tiny `Sink` trait over 1024 values.
//!
//! `dynamic` passes `&mut dyn Sink` (indirect call, no inlining); `generic`
//! passes a concrete type (monomorphized, inlined). The sink is recreated per
//! iteration on both sides so only dispatch differs.
//!
//! Run just this pair with: cargo bench -- dyn_vs_generic

use compare::{drive_dyn, drive_generic, MixSink};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_dyn_vs_generic(c: &mut Criterion) {
    let values = support::numbers(1024);
    let mut group = c.benchmark_group("dyn_vs_generic");

    group.bench_function("dynamic", |b| {
        b.iter(|| {
            let mut sink = MixSink::new();
            black_box(drive_dyn(&mut sink, black_box(values.as_slice())))
        });
    });

    group.bench_function("generic", |b| {
        b.iter(|| {
            let mut sink = MixSink::new();
            black_box(drive_generic(&mut sink, black_box(values.as_slice())))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_dyn_vs_generic);
criterion_main!(benches);
