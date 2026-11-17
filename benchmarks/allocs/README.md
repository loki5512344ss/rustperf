# Allocation telemetry: the `RUSTPERF_ALLOCS` protocol

`rustperf allocs` answers one question the harness cannot otherwise answer:
**how many allocations does one operation make?** Wall-clock and instruction
counts both miss this. A change that removes 139× of the allocations in a
workload and buys 1.55× of time looks like an ordinary 1.55× win, and a change
that adds one allocation per item can stay invisible until it costs real time
somewhere else (see `docs/pgo-spike.md`).

It cannot be measured from the outside. Neither criterion nor divan prints how
many iterations it ran, so a `GlobalAlloc` wrapper around someone else's
benchmark sees only a total — allocations per *second*, not per operation. The
only honest route is to let the benchmark count its own allocations inside its
own loop and report both numbers together. That is what this crate demonstrates.

## What is in here

| Path | What it is |
|---|---|
| `src/lib.rs` | Two implementations of the same job: `render_allocating` (a temporary `String` per item) and `render_reusing` (a caller-owned buffer), with equality tests. |
| `benches/stats.rs` | A criterion bench with a counting `#[global_allocator]` around `System`, and a hand-written `main` that prints the protocol lines after criterion finishes. |

`benches/stats.rs` counts inside `b.iter_custom(|iters| ...)`: the counters are
snapshotted immediately before the measured loop and read immediately after,
so criterion's own allocations (sample buffers, report formatting) are outside
the delta. The iteration count criterion passes into `iter_custom` is
accumulated with the counts, which is what makes `allocs / iters` possible at
all.

## Run it

```sh
cd benchmarks/allocs
cargo bench
```

Criterion's own report goes to stdout; the protocol lines go to **stderr**, one
per benchmark, after the report. To read only them:

```sh
cargo bench 2>&1 >/dev/null | grep RUSTPERF_ALLOCS
```

A single benchmark:

```sh
cargo bench -- allocs/reusing
```

The bench targets are outside this repository's workspace, so `cargo bench`
here builds only this crate and criterion.

## The protocol

One line per benchmark, on stderr:

```
RUSTPERF_ALLOCS benchmark=<id> allocs=<n> bytes=<m> iters=<k>
```

- `benchmark` — the same id criterion prints, e.g. `allocs/allocating`.
- `allocs` — allocations counted across the measured iterations.
- `bytes` — bytes requested across those allocations.
- `iters` — iterations actually run in the counted loop.

All four fields are required; all numbers are decimal `u64`; `iters` must be
greater than zero. `rustperf allocs` computes `allocs/op = allocs / iters` and
`bytes/op = bytes / iters`, keeps the first line it sees for a given benchmark,
and ignores every other line on stderr. Extra whitespace is fine.

## Provisioning a real repository

This is a **human** step, done **once**, before `rustperf baseline` records the
run — the same category as allowing a dependency or an `unsafe` block. The
harness never edits a benchmark: boilerplate that changed the measured code
would make the measurement itself part of the experiment.

1. Copy the two pieces from `benches/stats.rs` into your own bench target:
   the `#[global_allocator]` wrapper (with its two counters) and the custom
   `main` that prints the lines. Replace `criterion_main!` with
   `Criterion::default().configure_from_args()`, run your benchmark functions,
   call `final_summary()`, then print.
2. Wrap each benchmark body you want counted in `iter_custom`, snapshotted the
   same way `counted` does it. Plain `b.iter` gives no iteration count, so it
   cannot produce `allocs/op`.
3. Make sure `.rustperf/config.yaml` lists the benchmark ids you report and the
   bench target that prints them
   (`bench_targets: [{package: ..., target: ...}]`).
4. Run `rustperf allocs` and check the table. Benchmarks that print nothing are
   listed as `not provisioned`, which is not an error — it just means that
   particular one has not been instrumented.

`rustperf allocs` runs each declared benchmark with
`cargo bench -p <package> --bench <target> -- <id> --profile-time 1`, so
criterion's statistical machinery is skipped and only the counters matter. It
never writes `results.tsv`, never writes a run log, and never affects a verdict:
allocation counts are a **lead** for the agent and the human, not a score.
