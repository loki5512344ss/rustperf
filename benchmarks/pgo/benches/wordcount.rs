use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pgo_demo::count_words;

fn bench_count_words(c: &mut Criterion) {
    // ~9.2 KB, 1800 words — the input from the demo crate and docs/pgo-spike.md.
    let input = "The Quick, Brown Fox! jumps over 2 lazy dogs. ".repeat(200);
    c.bench_function("count_words", |b| {
        b.iter(|| {
            // Consume the result so the optimizer cannot elide the call.
            let counts = count_words(black_box(&input));
            assert!(!counts.is_empty());
        })
    });
}

criterion_group!(benches, bench_count_words);
criterion_main!(benches);
