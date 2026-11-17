//! Deterministic input generators shared by the benchmark binaries.
//!
//! Each bench file pulls this in with `mod support;`. Inputs are generated
//! from a fixed seed so a run is repeatable on any machine, and every generator
//! avoids the randomness of `rand` so the crate has no dependencies beyond
//! criterion.

#![allow(dead_code)]

use compare::Record;

/// Tiny xorshift64 generator: good enough for benchmark inputs, and stable
/// across Rust versions, unlike `std`'s hash seeds.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const WORDS: [&str; 16] = [
    "Rust",
    "borrow",
    "checker",
    "Fast",
    "code",
    "String",
    "Vec",
    "Allocation",
    "benchmark",
    "Cache",
    "Miss",
    "optimizer",
    "Inline",
    "arena",
    "Lifetime",
    "registers",
];

/// ~9 KB of space separated, mixed case words with plenty of repeats.
pub fn sample_text() -> String {
    const TARGET: usize = 9 * 1024;
    let mut rng = Rng::new(0x5EED_1234);
    let mut text = String::with_capacity(TARGET + 32);
    while text.len() < TARGET {
        if !text.is_empty() {
            text.push(' ');
        }
        let word = WORDS[(rng.next_u64() % WORDS.len() as u64) as usize];
        text.push_str(word);
    }
    text
}

/// `n` pseudo-random u64 values.
pub fn numbers(n: usize) -> Vec<u64> {
    let mut rng = Rng::new(0xC0FF_EE00);
    (0..n).map(|_| rng.next_u64()).collect()
}

/// `n` map keys derived from a multiplicative sequence, so they are unique but
/// far apart (a realistic id space rather than 0, 1, 2, ...).
pub fn keys(n: usize) -> Vec<u64> {
    (0..n as u64)
        .map(|i| (i.wrapping_mul(0x9E37_79B9_7F4A_7C15)) >> 4)
        .collect()
}

/// Half the probes hit `keys`, half of them miss.
pub fn probes(keys: &[u64]) -> Vec<u64> {
    keys.iter()
        .step_by(2)
        .copied()
        .chain(keys.iter().step_by(2).map(|key| key ^ 0x5555_5555))
        .collect()
}

/// `n` words drawn from a 32-word vocabulary, so keys repeat a lot and the
/// entry/insert pair exercises both the hit and the miss path.
pub fn words(n: usize) -> Vec<&'static str> {
    const VOCAB: [&str; 32] = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi",
        "chi", "psi", "omega", "red", "green", "blue", "cyan", "magenta", "yellow", "black",
        "white",
    ];
    let mut rng = Rng::new(0x1234_5678);
    (0..n)
        .map(|_| VOCAB[(rng.next_u64() % VOCAB.len() as u64) as usize])
        .collect()
}

/// `n` records with names of varying length, for the clone-vs-borrow pair.
pub fn records(n: usize) -> Vec<Record> {
    let names = [
        "",
        "id",
        "name",
        "service-name",
        "a-much-longer-record-name",
    ];
    (0..n)
        .map(|i| Record::new(names[i % names.len()], i as u64))
        .collect()
}

/// `n` values that all fit in `compare::INLINE_CAP` bytes.
pub fn short_strings(n: usize) -> Vec<&'static str> {
    const VALUES: [&str; 8] = [
        "id",
        "name",
        "value",
        "unit",
        "project",
        "module",
        "profile",
        "target-name",
    ];
    (0..n).map(|i| VALUES[i % VALUES.len()]).collect()
}
