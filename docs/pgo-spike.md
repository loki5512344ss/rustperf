# PGO spike — 2026-11-17

Throwaway measurement to decide whether a PGO mode belongs in rustperf. Machine:
Xeon E5-2680 v3, rustc/cargo 1.100.0 (LLVM 23.1.5), llvm-profgen 23.1.5.
Workload: `testdata/demo` (`count_words` on a 9.2 KB input, 1800 words),
criterion bench binary. All numbers from commands run on this machine.

## Sample-based PGO (`-Cprofile-sample-use`)

Pipeline: train build with `CARGO_PROFILE_BENCH_DEBUG=2` → `perf record -b -F 999`
→ `llvm-profgen --perfdata --binary --output` → rebuild with
`-Cprofile-sample-use=pgo.prof`.

Two things bite:

- `perf record` fails at `perf_event_paranoid=2` ("Failure to open any events");
  it worked only after temporarily lowering it to 1 (restored to 2 afterwards).
  `samply` refuses at 2 as well.
- `llvm-profgen` needs branch stacks (`perf record -b`); without `-b` it rejects
  the file as "Invalid perf script input".
- Cargo strips debuginfo by default even with `-Cdebuginfo=2` in `RUSTFLAGS`;
  the training build needs `CARGO_PROFILE_BENCH_DEBUG=2`, or llvm-profgen reports
  "no binary functions".

Profile: 91.25 % of samples mapped to functions, `demo::count_words` got
993 111 samples.

## Instrumented PGO (`-Cprofile-generate` / `-Cprofile-use`)

Used as a cross-check, to rule out a profile-mapping artifact: exact counters,
no perf, no address mapping.

## Results

Interleaved ABBA, 6 rounds × 4 runs = 12 samples per side, criterion default
sample size, median estimate per run:

| binary | median | min | max |
|---|---|---|---|
| plain (same flags, no PGO) | **115.30 µs** | 114.33 | 116.11 |
| sample PGO | **158.40 µs** | 157.56 | 161.16 |

**+37.38 %** (slower), distributions disjoint. Instrumented PGO reproduces it:
159.4 µs vs 114.7 µs in a single interleaved check.

Why: `perf stat` over a fixed 2 s window gives similar totals, so per call the
PGO binary executes ~1.37× more instructions (115 µs vs 158 µs at the same
instructions/s), and ~5.8× more branch misses. The hot function grows from
1876 bytes to 2380 (sample PGO) / 2997 (instrumented PGO): guided inlining pulls
more formatting/allocation machinery into `count_words`, which is already
allocation-bound. Not a measurement artifact — both PGO kinds do it.

## Allocations in the same workload (separate probe)

Counting `GlobalAlloc` wrapper around `System`, 100 calls after warm-up:

| variant | allocations/call | bytes/call | time/call |
|---|---|---|---|
| `count_words` as shipped | 1803.0 | 15 372 | 137.3 µs |
| `word.push(c)` instead of `word = word + &c.to_string()` | 1803.0 | 15 372 | 136.2 µs (1.01×) |
| one reused `String` + borrowed `HashMap::get_mut` | **13.0** | 1 014 | **88.7 µs (1.55×)** |

Two facts worth carrying into the C1/C2 decision:

- The demo has ~1 allocation per word (1803 per call) and **zero reallocations**;
  LLVM already removes the per-character temporaries at `-O3`, so the "quadratic"
  comment in the demo is stale. The cost is the per-word `String`, not `realloc`.
- Cutting allocations 139× buys 1.55×, not 100×: the rest is scanning and hashing.
  Allocation count is a strong *lead*, not the whole model.

## Decision input

- Sample-based PGO works mechanically on this machine, with two non-obvious
  prerequisites (`-b`, `CARGO_PROFILE_BENCH_DEBUG=2`) and a temporary kernel
  setting. But on this allocation-bound benchmark it is a **consistent 37 %
  regression**, so it cannot be a default, and in rustperf it would have to be a
  human-gated, separately measured experiment that a DISCARD can reject.
- Allocation telemetry (C1) has a clearer case: 1803 allocations per call are
  invisible to the current harness, and the 1.55× win from removing them is
  exactly the kind of lead `leads` was built to point at.

## Reproduce it

`benchmarks/pgo/run.sh` replays this experiment (training build, `perf record -b`,
`llvm-profgen`, rebuild with `-Cprofile-sample-use`, interleaved ABBA) and prints
the medians. It refuses to run at `perf_event_paranoid > 1` and never calls
`sudo`; see `benchmarks/README.md` for the three traps it encodes.
