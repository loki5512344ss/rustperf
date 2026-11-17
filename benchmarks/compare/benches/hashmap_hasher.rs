//! Pair 7: a 4096-entry `HashMap<u64, u64>` with two hash functions.
//!
//! `insert_*` measures building the map (both sides reserve exactly, so the
//! difference is mostly hashing), `lookup_*` measures 2048 probes, half of them
//! hits and half misses.
//!
//! Run just this pair with: cargo bench -- hashmap_hasher

use compare::{index_default, index_fnv, lookup_sum_default, lookup_sum_fnv};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_hashmap_hasher(c: &mut Criterion) {
    let keys = support::keys(4096);
    let probes = support::probes(&keys);
    let default_map = index_default(&keys);
    let fnv_map = index_fnv(&keys);

    let mut group = c.benchmark_group("hashmap_hasher");

    group.bench_function("insert_siphash", |b| {
        b.iter(|| black_box(index_default(black_box(keys.as_slice()))));
    });

    group.bench_function("insert_fnv1a", |b| {
        b.iter(|| black_box(index_fnv(black_box(keys.as_slice()))));
    });

    group.bench_function("lookup_siphash", |b| {
        b.iter(|| {
            black_box(lookup_sum_default(
                black_box(&default_map),
                black_box(probes.as_slice()),
            ))
        });
    });

    group.bench_function("lookup_fnv1a", |b| {
        b.iter(|| {
            black_box(lookup_sum_fnv(
                black_box(&fnv_map),
                black_box(probes.as_slice()),
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_hashmap_hasher);
criterion_main!(benches);
