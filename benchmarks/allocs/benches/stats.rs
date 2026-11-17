//! The allocation-counting benchmark harness.
//!
//! This file is the pattern a human copies into a real repository once, before
//! `rustperf baseline`. It does two things a normal criterion bench does not:
//!
//! 1. A `#[global_allocator]` wrapper counts every allocation and every
//!    requested byte, process-wide.
//! 2. `main` is hand-written (not `criterion_main!`) so that after criterion
//!    finishes it can print the `RUSTPERF_ALLOCS` protocol lines `rustperf
//!    allocs` reads from **stderr**.
//!
//! The counting is scoped to the inside of `b.iter_custom(|iters| ...)`: the
//! counters are snapshotted immediately before the loop and read immediately
//! after, and only that difference is accumulated. Everything criterion does
//! around the loop — allocating sample buffers, formatting estimates, building
//! the report — therefore stays out of the numbers. A wrapper that counted the
//! whole process would report criterion's own allocations as if they were the
//! benchmark's.
//!
//! Why there is an iteration count at all: neither criterion nor divan prints
//! how many times it ran the body, and without that denominator a global
//! counter can only yield allocations per second. `iter_custom` hands us the
//! iteration count, so the harness accumulates `(allocs, bytes, iters)` across
//! every batch criterion runs and prints the three totals.
//!
//! `--profile-time` (which `rustperf allocs` passes) is criterion's
//! "run the body, skip the statistics" mode: it keeps this cheap and stops
//! criterion from saving estimates nobody asked for.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use criterion::Criterion;

// ---------------------------------------------------------------------------
// The counting allocator.
// ---------------------------------------------------------------------------

/// Every allocation request the process makes, and the bytes it asked for.
///
/// `Relaxed` is enough: nothing here synchronizes any data, it only counts,
/// and criterion calls the measured body from a single thread. A reallocation
/// counts as one allocation of the new size, which is what the caller asked
/// the allocator for.
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: `layout` comes from the caller and is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: same as `alloc` above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: `ptr`, `layout` and `new_size` come from the caller and are
        // forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Frees are not counted: `allocs/op` answers "how many allocations
        // does one operation make", and every counted allocation is freed
        // exactly once anyway.
        // SAFETY: `ptr` and `layout` come from the caller and are forwarded
        // unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// ---------------------------------------------------------------------------
// Per-benchmark totals, accumulated inside the measured loop.
// ---------------------------------------------------------------------------

/// The totals for one benchmark: three `u64`s behind atomics so `iter_custom`'s
/// closure needs no lock and, more importantly, allocates nothing itself.
struct Totals {
    allocs: AtomicU64,
    bytes: AtomicU64,
    iters: AtomicU64,
}

impl Totals {
    const fn new() -> Totals {
        Totals {
            allocs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            iters: AtomicU64::new(0),
        }
    }

    fn add(&self, allocs: u64, bytes: u64, iters: u64) {
        self.allocs.fetch_add(allocs, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.iters.fetch_add(iters, Ordering::Relaxed);
    }
}

/// The benchmark ids printed in the protocol lines, in [`TOTALS`] order. They
/// match the criterion ids so a reader can join `rustperf allocs` rows to
/// criterion output by eye.
const NAMES: [&str; 2] = ["allocs/allocating", "allocs/reusing"];

/// One entry per benchmark, in the same order as [`NAMES`].
static TOTALS: [Totals; 2] = [Totals::new(), Totals::new()];

/// Runs one `iter_custom` batch under the counters, then returns the duration
/// criterion asked for. Split out so both benchmarks share exactly one counting
/// path — the whole point of the file is that this path is auditable.
fn counted(index: usize, iters: u64, body: impl FnOnce()) -> Duration {
    let allocs_before = ALLOC_COUNT.load(Ordering::Relaxed);
    let bytes_before = ALLOC_BYTES.load(Ordering::Relaxed);

    let start = Instant::now();
    body();
    let elapsed = start.elapsed();

    TOTALS[index].add(
        ALLOC_COUNT.load(Ordering::Relaxed) - allocs_before,
        ALLOC_BYTES.load(Ordering::Relaxed) - bytes_before,
        iters,
    );
    elapsed
}

// ---------------------------------------------------------------------------
// The benchmarks.
// ---------------------------------------------------------------------------

const ITEM_NAMES: [&str; 8] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
];

fn benches(c: &mut Criterion) {
    // A fixed input, built once, outside every measured loop. Its own
    // allocations happen before any counter snapshot and cost nothing.
    let items: Vec<(&str, u64)> = (0..32)
        .map(|i| (ITEM_NAMES[i % ITEM_NAMES.len()], i as u64))
        .collect();
    // Reused across every iteration AND every batch, which is the whole point
    // of the comparison: after its first growth this buffer stops allocating,
    // so its counts should be far smaller.
    //
    // It is declared HERE, not inside the `|b| ...` closure below. Criterion
    // re-invokes that closure once per measured batch (`Routine::bench` calls
    // it per sample), so a buffer created inside it would be rebuilt — and
    // re-grown through ~8 allocations — for every batch, and those rebuilds
    // would be counted as if they were the benchmark's own work. Hoisting is
    // the difference between 0 allocations per op and a few dozen per batch.
    let mut buf = String::new();

    c.bench_function(NAMES[0], |b| {
        b.iter_custom(|iters| {
            counted(0, iters, || {
                for _ in 0..iters {
                    black_box(allocs_example::render_allocating(&items));
                }
            })
        })
    });

    c.bench_function(NAMES[1], |b| {
        b.iter_custom(|iters| {
            counted(1, iters, || {
                for _ in 0..iters {
                    allocs_example::render_reusing(&mut buf, &items);
                    black_box(&buf);
                }
            })
        })
    });
}

// ---------------------------------------------------------------------------
// Custom main: criterion first, then the protocol lines.
// ---------------------------------------------------------------------------

fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    benches(&mut criterion);
    criterion.final_summary();

    // Only benchmarks criterion actually ran have a denominator. A filtered
    // run (`cargo bench -- allocs/reusing`) must print only the one line it
    // measured, not a zero for the one it skipped.
    for (index, name) in NAMES.iter().enumerate() {
        let iters = TOTALS[index].iters.load(Ordering::Relaxed);
        if iters == 0 {
            continue;
        }
        eprintln!(
            "RUSTPERF_ALLOCS benchmark={name} allocs={} bytes={} iters={iters}",
            TOTALS[index].allocs.load(Ordering::Relaxed),
            TOTALS[index].bytes.load(Ordering::Relaxed),
        );
    }
}
