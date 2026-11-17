//! Pair 6: counting word frequencies over 4096 words from a 32-word vocabulary.
//!
//! `contains_key_then_insert` hashes each key up to three times per hit;
//! `entry_or_insert` hashes once. The vocabulary repeats on purpose, so most
//! iterations take the "already present" path.
//!
//! Run just this pair with: cargo bench -- entry_vs_insert

use compare::{word_counts_contains_key, word_counts_entry};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

mod support;

fn bench_entry_vs_insert(c: &mut Criterion) {
    let words = support::words(4096);
    let mut group = c.benchmark_group("entry_vs_insert");

    group.bench_function("contains_key_then_insert", |b| {
        b.iter(|| black_box(word_counts_contains_key(black_box(words.as_slice()))));
    });

    group.bench_function("entry_or_insert", |b| {
        b.iter(|| black_box(word_counts_entry(black_box(words.as_slice()))));
    });

    group.finish();
}

criterion_group!(benches, bench_entry_vs_insert);
criterion_main!(benches);
