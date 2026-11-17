# `pgo/` — reproduce the sample-based PGO result

A self-contained reproduction of the experiment in
[`docs/pgo-spike.md`](../../docs/pgo-spike.md): build an allocation-heavy
criterion bench, train it with `perf record`, turn the samples into an LLVM
profile with `llvm-profgen`, rebuild with `-Cprofile-sample-use`, and compare
plain vs PGO with an interleaved ABBA loop.

The result measured on the author's machine is in
[`docs/pgo-spike.md`](../../docs/pgo-spike.md). It is **not** repeated here as a
promise: run `./run.sh` and read your own numbers. That document also records
why the numbers went the way they did (guided inlining grew the hot function on
an allocation-bound workload).

## Prerequisites

- Linux on x86-64. `perf` (any recent build), `llvm-profgen` and
  `llvm-profdata` from LLVM (23.x here), `python3`, and a Rust toolchain
  (`cargo`).
- `/proc/sys/kernel/perf_event_paranoid` at `1` or lower. At `2` — the common
  default, and the value on the machine this was written on — `perf record`
  fails with "Failure to open any events" and `samply` refuses as well.

  Lower it yourself; **the script never calls `sudo`**:

  ```bash
  echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid
  ```

  and **restore it to 2 afterwards**:

  ```bash
  echo 2 | sudo tee /proc/sys/kernel/perf_event_paranoid
  ```

  `./run.sh` reads the file, refuses at a value above 1, prints exactly those
  two commands and exits with status 2. This is a deliberate friction: a
  benchmark should not silently widen kernel permissions.

- `perf record` works per task at `paranoid=1`; only system-wide (`-a`) and
  kernel (`:k`) profiling need more.

## Run it

```bash
cd benchmarks/pgo
./run.sh
```

Useful knobs (environment variables):

```bash
ROUNDS=6 ./run.sh              # ABBA rounds (default 4 -> 16 measured runs)
WARMUP_TIME=3 MEASURE_TIME=5 ./run.sh   # closer to criterion's own defaults
```

The script works entirely inside this directory: `target/` for the builds,
`out/` for the training binary, `perf.data`, `pgo.prof`, per-run logs and
`out/samples.tsv`. Both are git-ignored.

Cargo 1.100.0 also prints a `package.homepage is redundant with
package.repository` warning about the repository root manifest while building
this nested package. It comes from the parent package, not from this crate, and
it changes nothing about the build.

## What it does, step by step

1. **Training build** — `CARGO_PROFILE_BENCH_DEBUG=2 cargo bench --no-run
   --message-format=json`, parse the JSON stream with `python3`, copy the
   `wordcount` executable to `out/bench-plain`.
2. **Record** — `perf record -b -F 999 -o out/perf.data -- out/bench-plain
   --bench --profile-time 5`.
3. **Profile** — `llvm-profgen --perfdata=out/perf.data --binary=out/bench-plain
   --output=out/pgo.prof`. An empty profile (under ~1 KB, or an "empty profile"
   / "no binary functions" / "Invalid perf script input" warning) aborts with
   the two likely causes.
4. **PGO rebuild** — `CARGO_PROFILE_BENCH_DEBUG=2
   RUSTFLAGS="-Cprofile-sample-use=$PWD/out/pgo.prof" cargo bench --no-run`,
   copy to `out/bench-pgo`.
5. **ABBA** — per round `plain, pgo, pgo, plain`, parse each run's criterion
   `time: [lo est hi]` line, print every sample, then medians per side, the
   delta in percent, and `perf stat -e instructions:u,cycles:u,branch-misses:u`
   over a fixed 2 s window (`STAT_SECS`) for each binary, skipped with a note
   if `perf stat` fails.

## Why the three non-obvious flags

- **`-b` on `perf record`.** Sample-based PGO needs *branch stacks* to attribute
  samples to call paths; `llvm-profgen` rejects a plain `perf.data` as
  "Invalid perf script input".
- **`CARGO_PROFILE_BENCH_DEBUG=2`.** Cargo strips debuginfo by default, even
  when `-Cdebuginfo=2` is in `RUSTFLAGS`. Without symbols the samples cannot be
  mapped and `llvm-profgen` reports "no binary functions". This is an
  environment variable rather than a `RUSTFLAGS` entry on purpose.
- **`$PWD/out/pgo.prof`, absolute.** The flag is passed through `RUSTFLAGS` to
  a rustc invocation that Cargo starts in its own working directory, so pinning
  the profile to an absolute path keeps it unambiguous. The training binary is
  also copied to `out/bench-plain` before the rebuild, so the profile always
  belongs to a binary that still exists on disk to read `perf.data` against.

## How to read the output

```
  plain round  1 run 1   est <est> µs   [lo <lo>   hi <hi>]
  pgo   round  1 run 2   est <est> µs   [lo <lo>   hi <hi>]
  ...
      plain n=<n>  median <est> µs  min <min>  max <max>
      pgo   n=<n>  median <est> µs  min <min>  max <max>
      delta (pgo - plain) on the median: <±d.dd> %  (<faster|slower> with PGO)
```

That is the shape of the output, not a result; the actual numbers depend on the
machine. `docs/pgo-spike.md` has one measured run.

- Compare the two **medians**, and check that the `min`/`max` ranges do not
  overlap. Overlapping ranges at a few percent mean noise; go and get an idle
  machine.
- The ABBA order cancels slow drift (thermal, page-cache, another build in the
  background) between the two binaries; it cannot cancel noise *within* a run,
  so keep the machine quiet.
- A positive delta means sample PGO made this workload **slower**. That is a
  legitimate result: PGO optimizes for what the profile saw, and on an
  allocation-bound loop the guided inlining it chose can cost more than it
  saves.
- The `perf stat` block is a rough cross-check, not a per-call instruction
  count. The window is wall-clock, so it counts *work completed*: a slower
  binary fits fewer calls into the same 2 s. Read it together with
  `time elapsed` and `user` in `out/perf-stat-*.log` — if the two runs did not
  get about the same CPU time, the machine was busy and the totals are not
  comparable. Branch misses are the most stable signal in this workload.
- Absolute times differ from `docs/pgo-spike.md` on any other CPU. Only the
  delta, and the ranges, are meant to be comparable.

## Scope

This directory is a reproduction, not a gate. It never feeds a `results.tsv`
row or a verdict, and nothing in the harness depends on it. Treat a run as
evidence for *this* workload on *this* machine, and a DISCARD as a perfectly
good outcome.
