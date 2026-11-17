//! Parses the allocation telemetry an opt-in benchmark prints.
//!
//! Why this is a protocol and not a measurement: criterion and divan both
//! report a per-iteration estimate but neither prints the iteration count it
//! arrived at, and a `GlobalAlloc` wrapper outside the benchmark can only see
//! a total, not a total per operation (see `docs/pgo-spike.md`). So the only
//! honest way to get `allocs/op` out of someone else's benchmark is to have
//! that benchmark count its own allocations inside its own loop and report
//! both numbers together:
//!
//! ```text
//! RUSTPERF_ALLOCS benchmark=count_words allocs=1300 bytes=101400 iters=100
//! ```
//!
//! All four fields are required and all numbers are decimal `u64`. The line
//! goes to **stderr**, so a benchmark's own stdout stays whatever criterion
//! prints there; a harness reading stderr therefore sees only these lines plus
//! cargo's diagnostics, everything else being ignored by [`parse_line`].
//! `allocs/op = allocs / iters`, `bytes/op = bytes / iters`, and `iters` must
//! be greater than zero — a zero would make the division undefined rather
//! than infinite, so such a line is treated as malformed.
//!
//! The protocol is documented for humans, with a runnable example, in
//! `benchmarks/allocs/README.md`.

/// One benchmark's allocation counters, as the benchmark reported them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocStat {
    /// The benchmark id, exactly as it appears in `RUSTPERF_ALLOCS`.
    pub benchmark: String,
    /// Total allocations counted across the measured iterations.
    pub allocs: u64,
    /// Total bytes requested across those allocations.
    pub bytes: u64,
    /// Iterations the benchmark actually ran in its counted loop.
    pub iters: u64,
}

impl AllocStat {
    /// Allocations per iteration, or `None` when no iteration was counted.
    ///
    /// [`parse_line`] already refuses `iters == 0`, so this can only be
    /// `None` for a stat constructed by hand; returning `None` rather than
    /// `NaN` keeps a nonsense ratio from silently flowing into a report.
    pub fn allocs_per_op(&self) -> Option<f64> {
        (self.iters > 0).then(|| self.allocs as f64 / self.iters as f64)
    }

    /// Requested bytes per iteration, or `None` when no iteration was
    /// counted. See [`AllocStat::allocs_per_op`].
    pub fn bytes_per_op(&self) -> Option<f64> {
        (self.iters > 0).then(|| self.bytes as f64 / self.iters as f64)
    }
}

/// The prefix that makes a stderr line telemetry rather than output.
const MARKER: &str = "RUSTPERF_ALLOCS";

/// Parses one stderr line, or `None` if it is not a well-formed protocol line.
///
/// Leading and trailing whitespace and any run of whitespace between fields
/// are tolerated. A line with the marker but a missing field, a non-numeric
/// number, or `iters=0` is malformed and yields `None` rather than a partial
/// stat: reporting "0 allocations" for a line that said nothing about
/// allocations would be a fabricated measurement.
pub fn parse_line(line: &str) -> Option<AllocStat> {
    let rest = line.trim().strip_prefix(MARKER)?;
    // The marker must be its own word. `RUSTPERF_ALLOCS_DEBUG=1` is somebody
    // else's variable, not a stat line, and matching it by prefix alone would
    // then fail on the missing fields — which happens to be the same answer,
    // but only by accident, so say it explicitly.
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }

    let mut benchmark = None;
    let mut allocs = None;
    let mut bytes = None;
    let mut iters = None;
    for field in rest.split_whitespace() {
        // A token that is not `key=value` means the line is not the shape
        // this protocol promises; refuse the whole line instead of guessing
        // which parts were meant.
        let (key, value) = field.split_once('=')?;
        match key {
            "benchmark" if !value.is_empty() => benchmark = Some(value.to_string()),
            "allocs" => allocs = Some(value.parse::<u64>().ok()?),
            "bytes" => bytes = Some(value.parse::<u64>().ok()?),
            "iters" => iters = Some(value.parse::<u64>().ok()?),
            // Unknown keys are ignored rather than refused: a later version
            // of the protocol may add fields, and a harness that cannot read
            // the three it needs should not reject the line over one it does
            // not.
            _ => {}
        }
    }

    let stat = AllocStat {
        benchmark: benchmark?,
        allocs: allocs?,
        bytes: bytes?,
        iters: iters?,
    };
    // `iters == 0` makes `allocs/op` undefined. A benchmark that counted no
    // iterations did not measure anything, so it has nothing to report.
    (stat.iters > 0).then_some(stat)
}

/// Parses every protocol line in `stderr`, keeping the **first** occurrence of
/// each benchmark.
///
/// First-wins is deliberate: a real criterion binary may run its group in
/// several batches (warm-up, measurement, `--profile-time`), and a later
/// batch is a smaller or differently shaped sample of the same benchmark.
/// Keeping the first line makes the reported ratio depend on the order the
/// benchmark itself printed, not on which line happened to parse last, and it
/// keeps duplicate lines from inflating a count.
pub fn parse_output(stderr: &str) -> Vec<AllocStat> {
    let mut out: Vec<AllocStat> = Vec::new();
    for line in stderr.lines() {
        let Some(stat) = parse_line(line) else {
            continue;
        };
        if !out.iter().any(|s| s.benchmark == stat.benchmark) {
            out.push(stat);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_line_yields_a_stat_with_its_ratios() {
        let stat =
            parse_line("RUSTPERF_ALLOCS benchmark=count_words allocs=1300 bytes=101400 iters=100")
                .expect("a well-formed line must parse");
        assert_eq!(stat.benchmark, "count_words");
        assert_eq!(stat.allocs, 1300);
        assert_eq!(stat.bytes, 101_400);
        assert_eq!(stat.iters, 100);
        assert_eq!(stat.allocs_per_op(), Some(13.0));
        assert_eq!(stat.bytes_per_op(), Some(1014.0));
    }

    // The line is printed by a benchmark the human provisioned, so a stray
    // indentation or a tab-separated rewrite must still be understood.
    #[test]
    fn tabs_and_extra_spaces_are_tolerated() {
        let stat = parse_line(
            "\t RUSTPERF_ALLOCS\tbenchmark=demo/alloc   allocs=3    bytes=48\titers=2  ",
        )
        .expect("whitespace variation must parse");
        assert_eq!(stat.benchmark, "demo/alloc");
        assert_eq!(stat.allocs_per_op(), Some(1.5));
        assert_eq!(stat.bytes_per_op(), Some(24.0));
    }

    #[test]
    fn a_missing_field_makes_the_line_malformed() {
        for line in [
            "RUSTPERF_ALLOCS allocs=1 bytes=2 iters=3",
            "RUSTPERF_ALLOCS benchmark=x bytes=2 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=1 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=1 bytes=2",
            "RUSTPERF_ALLOCS benchmark=x",
        ] {
            assert!(parse_line(line).is_none(), "{line:?} must be refused");
        }
    }

    #[test]
    fn a_bad_number_makes_the_line_malformed() {
        for line in [
            "RUSTPERF_ALLOCS benchmark=x allocs=one bytes=2 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=-1 bytes=2 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=1.5 bytes=2 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=1 bytes=99999999999999999999999 iters=3",
            "RUSTPERF_ALLOCS benchmark=x allocs=1 bytes=2 iters=",
        ] {
            assert!(parse_line(line).is_none(), "{line:?} must be refused");
        }
    }

    // `allocs/op` is `allocs / iters`; with no iterations there is no
    // denominator, and reporting zero allocations would be a made-up number.
    #[test]
    fn zero_iters_is_malformed() {
        assert!(
            parse_line("RUSTPERF_ALLOCS benchmark=x allocs=0 bytes=0 iters=0").is_none(),
            "iters=0 has no defined per-op ratio"
        );
        let stat = AllocStat {
            benchmark: "x".into(),
            allocs: 0,
            bytes: 0,
            iters: 0,
        };
        assert_eq!(stat.allocs_per_op(), None);
        assert_eq!(stat.bytes_per_op(), None);
    }

    #[test]
    fn lines_without_the_marker_are_ignored() {
        for line in [
            "running 2 tests",
            "Benchmarking count_words: Warming up for 3.0000 s",
            "warning: unused variable: `x`",
            "RUSTPERF_ALLOCS_DEBUG benchmark=x allocs=1 bytes=2 iters=3",
            "the line mentions RUSTPERF_ALLOCS mid-sentence",
            "",
        ] {
            assert!(parse_line(line).is_none(), "{line:?} must not parse");
        }
    }

    // A binary may print the same benchmark more than once (several criterion
    // batches, a rerun, a duplicated `eprintln!`). The first line is the one
    // the benchmark itself chose to print first, so it wins; later lines must
    // not overwrite or add to it.
    #[test]
    fn a_duplicate_benchmark_keeps_the_first_line() {
        let stderr = "\
RUSTPERF_ALLOCS benchmark=a allocs=10 bytes=100 iters=10
RUSTPERF_ALLOCS benchmark=a allocs=999 bytes=9999 iters=1
";
        let stats = parse_output(stderr);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].allocs, 10);
        assert_eq!(stats[0].allocs_per_op(), Some(1.0));
    }

    #[test]
    fn several_benchmarks_are_reported_in_the_order_they_appear() {
        let stderr = "\
cargo bench is chatty
RUSTPERF_ALLOCS benchmark=a/one allocs=2 bytes=16 iters=2
RUSTPERF_ALLOCS benchmark=b/two allocs=0 bytes=0 iters=5
not a stat line
";
        let stats = parse_output(stderr);
        let names: Vec<&str> = stats.iter().map(|s| s.benchmark.as_str()).collect();
        assert_eq!(names, ["a/one", "b/two"]);
        assert_eq!(stats[0].allocs_per_op(), Some(1.0));
        assert_eq!(stats[1].allocs_per_op(), Some(0.0));
        assert_eq!(stats[1].bytes_per_op(), Some(0.0));
    }
}
