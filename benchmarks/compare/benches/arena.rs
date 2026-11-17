//! Pair 10: building and summing a 1023-node complete binary tree.
//!
//! `index_arena` uses one `Vec<Node>` and `u32` child indices; `rc_refcell`
//! uses one `Rc<RefCell<Node>>` allocation per node. Both build the same tree
//! of values `0..n` and return the same sum.
//!
//! Run just this pair with: cargo bench -- arena

use compare::{arena_tree_sum, rc_tree_sum};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

fn bench_arena(c: &mut Criterion) {
    const NODES: usize = 1023;
    let mut group = c.benchmark_group("arena");

    group.bench_function("index_arena", |b| {
        b.iter(|| black_box(arena_tree_sum(black_box(NODES))));
    });

    group.bench_function("rc_refcell", |b| {
        b.iter(|| black_box(rc_tree_sum(black_box(NODES))));
    });

    group.finish();
}

criterion_group!(benches, bench_arena);
criterion_main!(benches);
