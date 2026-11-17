//! The `allocs` command: allocation telemetry for benchmarks that opt in.
//!
//! Absent this command, allocations are invisible to the harness: a change
//! that removes 139× of the allocations in a workload and buys 1.55× of wall
//! time looks like an ordinary 1.55× win, and a change that quietly adds an
//! allocation per item looks like nothing at all until it shows up somewhere
//! else (see `docs/pgo-spike.md`). Allocation count is a strong *lead*, which
//! is exactly what this reports — a lead for the agent and the human to
//! investigate, not a measurement the verdict is allowed to depend on.
//!
//! It cannot be taken automatically from someone else's criterion benchmark:
//! neither criterion nor divan prints the number of iterations it measured, so
//! an external `GlobalAlloc` wrapper can only produce allocations per second.
//! A per-operation number therefore requires the benchmark itself to count
//! inside its own loop and report both numbers — the `RUSTPERF_ALLOCS`
//! protocol parsed by [`rustperf_core::allocstats`], with a runnable example
//! in `benchmarks/allocs/`.
//!
//! Consequences that shape this file:
//!
//! - **Nothing here writes.** No `results.tsv` row, no run log, no noise
//!   history: a command whose numbers cannot affect a verdict must not be able
//!   to influence one indirectly either.
//! - **A missing line is not an error.** Most repositories have not been
//!   provisioned, and refusing to run at all would make the first look at a
//!   new repository fail for a reason the human has not been told about yet.
//!   Each unprovisioned benchmark gets its own row pointing at the README, and
//!   the command exits 0 as long as it ran.
//! - **A benchmark that could not run IS an error.** `cargo bench` failing or
//!   timing out means the target could not be exercised, which is different
//!   from a target that ran and printed nothing; reporting the former as "not
//!   provisioned" would send the human looking for a missing `eprintln!`
//!   instead of a broken build.

use crate::args::Args;
use rustperf_core::{allocstats, config, gitx, measure, runner, state};
use std::path::Path;

/// Where the human is sent when a benchmark has not been provisioned.
const PROTOCOL_HINT: &str = "benchmarks/allocs/README.md";

/// How long criterion's own measurement phase runs for, in seconds.
///
/// `--profile-time` is criterion's "run the body over and over with no
/// statistical machinery and no saved estimates" mode. It is the right shape
/// here for two reasons: this command wants the benchmark's own counters, not
/// a criterion estimate, and criterion's normal sampling would multiply the
/// cost of every declared benchmark by a full warm-up and measurement cycle
/// without adding anything the counters do not already say.
const PROFILE_TIME_SECS: &str = "1";

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(argv, &[("C", true), ("json", false)]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };
    match allocs(&args) {
        Ok(msg) => {
            print!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn allocs(args: &Args) -> Result<String, String> {
    let root = gitx::root(&args.dir())?;
    let branch = gitx::current_branch(&root)?;
    let tag = state::tag_from_branch(&branch).ok_or_else(|| {
        format!(
            "current branch {branch:?} was not created by 'rustperf baseline --tag <tag>' — check \
             out the run branch first"
        )
    })?;
    let dir = state::state_dir(&root, &tag)?;
    let baseline = state::Baseline::load(&dir.join(state::BASELINE_FILE))?;
    let cfg = config::Config::load(&root.join(config::CONFIG_PATH))?;

    // A baseline recorded from a config that declared no bench targets has
    // nothing to invoke. That is a configuration gap, not a failure of this
    // command, and the fix is a human's `baseline` run — so say exactly that
    // and exit 0 rather than spending a non-zero code on it.
    if baseline.bench_targets.is_empty() {
        return Ok(format!(
            "no bench targets recorded — re-run baseline with a config that declares them\n\
             (the config's `bench_targets` list is what names the criterion targets to invoke; \
             see {PROTOCOL_HINT})\n"
        ));
    }

    let stats = collect(&root, &baseline, cfg.timeout_duration()?)?;
    let names = reported_names(&baseline, &stats);
    if names.is_empty() {
        return Ok(
            "no benchmarks declared — re-run baseline with a config that declares `benchmarks`\n"
                .to_string(),
        );
    }

    if args.flag("json") {
        return json(&names, &stats);
    }
    Ok(table(&names, &stats))
}

/// Runs every declared benchmark against every declared bench target and
/// returns the first stat reported for each benchmark id.
///
/// One cargo invocation per **target**, with all declared ids in the filter:
/// criterion runs whichever of them that target owns, and the id in the printed
/// line — not the filter that was passed — decides attribution, so a target
/// that ignores its filter and reports all its benchmarks still lands each
/// number on its own row. Invoking once per `(target, id)` instead would pay
/// cargo's freshness check and criterion's warm-up N times for the same data.
fn collect(
    root: &Path,
    baseline: &state::Baseline,
    timeout: std::time::Duration,
) -> Result<Vec<allocstats::AllocStat>, String> {
    let runner = runner::Runner::new(root, timeout);
    // No bench id declared means "every benchmark the target has", which is
    // also how `measure` spells the same thing.
    let filters: Vec<String> = if baseline.benchmarks.is_empty() {
        vec![".".to_string()]
    } else {
        baseline.benchmarks.clone()
    };
    let filter = measure::bench_filter(&filters);
    let declared = if baseline.benchmarks.is_empty() {
        "every benchmark the targets declare".to_string()
    } else {
        baseline.benchmarks.join(", ")
    };

    let mut stats: Vec<allocstats::AllocStat> = Vec::new();
    for target in &baseline.bench_targets {
        let args = [
            "bench",
            "-p",
            target.package.as_str(),
            "--bench",
            target.target.as_str(),
            "--",
            filter.as_str(),
            "--profile-time",
            PROFILE_TIME_SECS,
        ];
        // `None` for the log on purpose: `allocs` must not write a run log,
        // and a write that cannot happen cannot corrupt one.
        let out = runner.cargo(&args, None)?;
        if out.timed_out {
            return Err(format!(
                "benchmark(s) {declared} (package {}, target {}) timed out after {timeout:?}",
                target.package, target.target
            ));
        }
        if !out.ok() {
            return Err(format!(
                "benchmark(s) {declared} (package {}, target {}) could not run: cargo exited {}\n{}",
                target.package,
                target.target,
                out.exit_code,
                out.tail(20)
            ));
        }
        for stat in allocstats::parse_output(&String::from_utf8_lossy(&out.stderr)) {
            // First occurrence wins across the whole command, not just within
            // one invocation: with several targets, the same benchmark may be
            // attempted more than once, and re-adding it would make the table
            // depend on iteration order.
            if !stats.iter().any(|s| s.benchmark == stat.benchmark) {
                stats.push(stat);
            }
        }
    }
    Ok(stats)
}

/// The benchmark ids to print, in the order the baseline declared them.
///
/// A baseline that declared none (which `measure` reads as "all of them") is
/// answered with whatever the targets actually reported, so an unconfigured
/// repository still gets the telemetry its benchmarks chose to print.
fn reported_names(baseline: &state::Baseline, stats: &[allocstats::AllocStat]) -> Vec<String> {
    if !baseline.benchmarks.is_empty() {
        return baseline.benchmarks.clone();
    }
    stats.iter().map(|s| s.benchmark.clone()).collect()
}

fn lookup<'a>(stats: &'a [allocstats::AllocStat], name: &str) -> Option<&'a allocstats::AllocStat> {
    stats.iter().find(|s| s.benchmark == name)
}

/// The text form: one row per benchmark, then how many were provisioned.
fn table(names: &[String], stats: &[allocstats::AllocStat]) -> String {
    let not_provisioned = format!("not provisioned — see {PROTOCOL_HINT}");
    let width = names
        .iter()
        .map(|n| n.len())
        .chain(std::iter::once("benchmark".len()))
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<width$}  {:>10}  {:>10}\n",
        "benchmark", "allocs/op", "bytes/op"
    ));
    let mut provisioned = 0usize;
    for name in names {
        match lookup(stats, name) {
            Some(stat) => {
                provisioned += 1;
                // One decimal is the precision these counters support: the
                // ratio is exact over a whole number of iterations, and a
                // human comparing 13.0 with 1014.0 needs no more digits than
                // that to see which change is worth pursuing.
                out.push_str(&format!(
                    "{name:<width$}  {:>10.1}  {:>10.1}\n",
                    stat.allocs_per_op().unwrap_or(f64::NAN),
                    stat.bytes_per_op().unwrap_or(f64::NAN),
                ));
            }
            None => out.push_str(&format!("{name:<width$}  {not_provisioned}\n")),
        }
    }
    out.push_str(&format!(
        "\n{provisioned} of {} benchmarks provisioned\n",
        names.len()
    ));
    if provisioned < names.len() {
        out.push_str(&format!(
            "to provision one, make it count its own allocations and print the RUSTPERF_ALLOCS \
             line — see {PROTOCOL_HINT}\n"
        ));
    }
    out
}

/// The `--json` form: one object, the same facts as [`table`], `null` where a
/// benchmark reported nothing.
fn json(names: &[String], stats: &[allocstats::AllocStat]) -> Result<String, String> {
    let mut rows = Vec::with_capacity(names.len());
    let mut provisioned = 0usize;
    for name in names {
        let stat = lookup(stats, name);
        if stat.is_some() {
            provisioned += 1;
        }
        rows.push(serde_json::json!({
            "name": name,
            "allocs_per_op": stat.and_then(|s| s.allocs_per_op()),
            "bytes_per_op": stat.and_then(|s| s.bytes_per_op()),
            "provisioned": stat.is_some(),
        }));
    }
    let v = serde_json::json!({
        "benchmarks": rows,
        "provisioned": provisioned,
        "total": names.len(),
    });
    let mut text = serde_json::to_string_pretty(&v).map_err(|e| format!("encode allocs: {e}"))?;
    text.push('\n');
    Ok(text)
}
