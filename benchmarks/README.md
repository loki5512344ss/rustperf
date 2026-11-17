# Benchmarks and reproductions

Small, self-contained crates that compare two ways of doing the same thing — and
one that reproduces a PGO experiment end to end. They are here so that a person
can read the code, run it, and check a claim instead of trusting a number in a
document. **None of this is part of the harness's score**; a benchmark in this
directory never becomes a `results.tsv` row or a verdict.

Each subdirectory is an independent Cargo package, not a workspace member, and
has its own `target/`. Run it from inside that directory.

## `compare/` — A/B comparisons of techniques

Pairs of implementations of one task: building a string with `+` vs `push` vs a
reused buffer, `Vec::new`+push vs `with_capacity`, clone vs borrow, `collect`
vs a fused chain, `format!` vs `write!`, `contains_key`+`insert` vs `entry`,
SipHash vs a small custom hasher, `Box<dyn Trait>` vs a generic, a stack buffer
vs `Vec`, and an index arena vs `Rc<RefCell<_>>`. Every pair has a test that
both sides return the same result — a fast benchmark of the wrong answer is
worse than no benchmark.

```bash
cd benchmarks/compare
cargo test          # both sides agree
cargo bench         # all groups
cargo bench -- string_build   # one group
```

There is deliberately no results table: medians on an idle machine are the only
ones worth reading, and a difference below a few percent is noise. Compare the
two rows for a pair, not two different runs of the whole suite.

## `pgo/` — reproduce the sample-based PGO result

The experiment documented in [`../docs/pgo-spike.md`](../docs/pgo-spike.md):
train with `perf record -b`, turn the samples into an LLVM profile with
`llvm-profgen`, rebuild a criterion bench with `-Cprofile-sample-use`, and
compare plain vs PGO in an interleaved ABBA loop.

On the machine where this was written (Xeon E5-2680 v3, rustc 1.100.0 / LLVM
23.1.5), the PGO build was **37.4 % slower** (115.30 µs → 158.40 µs median,
12 samples per side, disjoint ranges), because guided inlining grew the hot
allocation-bound function and roughly 1.37× more instructions ran per call.
That result is *not* a claim that PGO is bad in general; it is a claim that PGO
must be measured, not assumed.

```bash
cd benchmarks/pgo
./run.sh            # refuses at perf_event_paranoid > 1 and prints the exact command
```

The script never calls `sudo`. To run it you have to lower
`/proc/sys/kernel/perf_event_paranoid` to 1 yourself and **set it back to 2**;
the README in that directory explains why (`perf record` and `samply` both
refuse at 2 on this kind of kernel).

Three traps the script encodes, all found the hard way:

- `perf record` needs `-b` (branch stacks); without it `llvm-profgen` rejects the
  file as "Invalid perf script input".
- Cargo strips debuginfo by default even with `-Cdebuginfo=2` in `RUSTFLAGS`;
  the training build needs `CARGO_PROFILE_BENCH_DEBUG=2`, or `llvm-profgen`
  reports "no binary functions".
- The profile only maps if the training and rebuilt binaries come from the same
  source; the script copies the training binary before rebuilding.

## `allocs/` — how allocation stats are provisioned

The harness cannot count allocations in someone else's criterion benchmark by
itself: a counting `#[global_allocator]` has to live in the benchmark binary,
and neither criterion nor divan prints an iteration count for an outside tool
to divide by. So allocation stats are **opt-in and human-provisioned**: a
benchmark prints one line per benchmark to stderr,

```
RUSTPERF_ALLOCS benchmark=count_words allocs=1803 bytes=15372 iters=100
```

and `rustperf allocs` runs the declared benchmarks, parses those lines, and
prints `allocs/op` and `bytes/op`. Nothing is gated on it and it never changes
a verdict. `benchmarks/allocs/` is a complete example to copy from, including
the counting allocator and the custom criterion `main` that attributes
allocations to the measured closure rather than to criterion itself.

```bash
cd benchmarks/allocs
cargo bench        # prints RUSTPERF_ALLOCS lines for its two benchmarks
```

See the README in that directory for the few lines to add to a real repository.
Provision it once, before `baseline`; an agent cannot add a dependency or an
`unsafe` allocator itself (it writes a `rustperf request` instead).
