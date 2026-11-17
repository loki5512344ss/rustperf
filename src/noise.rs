//! The per-benchmark noise floor: how far the median the verdict compares can
//! move between measurements at this round count.
//!
//! The dispersion is measured within one eval, on the BASELINE side only — the
//! pinned worktree measured several times in the same session under the same
//! conditions. That is the right yardstick for "an effect smaller than this is
//! indistinguishable from measurement jitter". Comparing the baseline centres
//! of DIFFERENT evals would not be noise: after a KEEP the baseline code
//! itself changes, and that shift is real.
//!
//! The verdict compares the MEDIAN of `count` rounds, not a single round, so
//! the reported floor is that median's standard error: the robust round-to
//! round sigma divided by the median, scaled by `1.2533 / sqrt(n)` for the `n`
//! rounds actually recorded. Raising `count` therefore lowers this number with
//! `1/sqrt(n)` — the documented remedy does move it, unlike a bare
//! single-round dispersion.
//!
//! Every measured experiment appends one line to `<state_dir>/noise.jsonl`.
//! The aggregate a human reads is the median of a benchmark's per-eval floors
//! once it has appeared in at least [`MIN_EXPERIMENTS`] experiments: one noisy
//! eval should not define the floor.
//!
//! This module is advisory throughout. The floor never enters a verdict and
//! never changes a threshold; it answers "was this run able to see a change
//! this small".

use crate::bench;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

/// The file, inside a run's state directory, holding one JSON record per
/// measured experiment.
pub const NOISE_FILE: &str = "noise.jsonl";

/// How many experiments must have measured a benchmark before its floor is
/// reported. Below this a single unlucky eval would define the number.
pub const MIN_EXPERIMENTS: usize = 4;

/// Rows `status` prints before collapsing the rest into "and N more".
pub const STATUS_ROWS: usize = 3;

/// The constant making the MAD a consistent estimator of the standard
/// deviation for normally distributed data.
pub const MAD_TO_SIGMA: f64 = 1.4826;

/// The factor turning a sample's standard deviation into the standard error of
/// its median: `sqrt(pi / 2)`, divided by `sqrt(n)` where `n` is the sample
/// size. This is the quantity the verdict's median comparison actually moves
/// by, so it is what a floor must be expressed in.
pub const MEDIAN_SE_FACTOR: f64 = 1.2533;

/// One benchmark's noise in one experiment.
///
/// `base_ns` and `cand_ns` are the two medians of that experiment, recorded so
/// a later reader can see what was being measured without re-running anything.
/// `n` is the number of measured rounds behind `base_ns`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub name: String,
    /// How far `base_ns` can move as a median of `n` rounds, in percent:
    /// `1.4826 * MAD(base rounds) / median(base rounds) * 100 * 1.2533 / sqrt(n)`.
    pub noise_pct: f64,
    /// Defaulted so a record written before the field existed still loads.
    #[serde(default)]
    pub base_ns: f64,
    #[serde(default)]
    pub cand_ns: f64,
    #[serde(default)]
    pub n: usize,
}

/// The one line appended per measured experiment.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    benchmarks: Vec<Sample>,
}

/// A benchmark's floor, aggregated across experiments.
#[derive(Clone, Debug, PartialEq)]
pub struct Floor {
    pub name: String,
    /// How many experiments measured this benchmark — the sample size behind
    /// `noise_pct`, never the total number of experiments in the run.
    pub experiments: usize,
    pub noise_pct: f64,
}

/// The median absolute deviation of `values`. `0.0` for an empty slice.
pub fn mad(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let center = median(values);
    let deviations: Vec<f64> = values.iter().map(|v| (v - center).abs()).collect();
    median(&deviations)
}

/// The standard error of the median of `values`, as a percentage of that
/// median: `1.4826 * MAD / median * 100 * 1.2533 / sqrt(n)`.
///
/// This is the quantity the verdict's median-of-`n`-rounds comparison can
/// resolve, not the dispersion of a single round. More rounds shrink it with
/// `1/sqrt(n)`, so raising `count` genuinely lowers the floor.
///
/// `None` when there are fewer than two rounds (dispersion is undefined) or
/// the median is zero or non-finite (no denominator).
pub fn noise_pct(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let center = median(values);
    if !center.is_finite() || center == 0.0 {
        return None;
    }
    let n = values.len() as f64;
    let pct = MAD_TO_SIGMA * mad(values) / center * 100.0 * MEDIAN_SE_FACTOR / n.sqrt();
    if pct.is_finite() { Some(pct) } else { None }
}

/// The median of `values`, reordering a copy. `NAN` for an empty slice.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    // `total_cmp` rather than `partial_cmp`: a NaN that reached this far must
    // still order deterministically instead of collapsing into an arbitrary
    // tie position.
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// One sample per delta, using only the baseline's own rounds.
pub fn samples(base: &bench::Set, deltas: &[bench::Delta]) -> Vec<Sample> {
    let mut out = Vec::with_capacity(deltas.len());
    for d in deltas {
        // The baseline side only: after a KEEP the baseline code itself is
        // different, so a centre shift between experiments is a real change,
        // not noise. The candidate's own rounds are never consulted here.
        let Some(values) = base.values(&d.name) else {
            continue;
        };
        let Some(noise_pct) = noise_pct(&values) else {
            continue;
        };
        out.push(Sample {
            name: d.name.clone(),
            noise_pct,
            base_ns: d.base_center,
            cand_ns: d.cand_center,
            n: values.len(),
        });
    }
    out
}

/// Appends one record to `<state_dir>/noise.jsonl`, one `write_all` per record.
///
/// Only one experiment writes at a time (the run's `eval` claim is held for
/// the whole measurement), and each record goes out through a single
/// `write_all` so two records are never spliced together. `write_all` may loop
/// internally and there is no `sync_all` here, so a concurrent reader can still
/// catch a partial LAST line; [`floors`] skips a line it cannot parse.
pub fn append(state_dir: &Path, samples: &[Sample]) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("create {}: {e}", state_dir.display()))?;
    let path = state_dir.join(NOISE_FILE);
    let mut line = serde_json::to_string(&Record {
        benchmarks: samples.to_vec(),
    })
    .map_err(|e| format!("encode noise record: {e}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// Samples the baseline side of a finished experiment and appends it.
///
/// A no-op when there is nothing to record.
pub fn record(state_dir: &Path, base: &bench::Set, deltas: &[bench::Delta]) -> Result<(), String> {
    append(state_dir, &samples(base, deltas))
}

/// Every benchmark's aggregated floor, noisiest first.
///
/// A missing file is no floors, not an error: a run that has not measured
/// anything yet has nothing to say here. Malformed lines are skipped.
pub fn floors(state_dir: &Path) -> Result<Vec<Floor>, String> {
    let path = state_dir.join(NOISE_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };

    let mut by_name: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A malformed line is skipped, not fatal: the file is append-only and
        // written by an older or a interrupted harness, and losing the whole
        // history to one bad byte would be worse than losing one experiment.
        let Ok(record) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        // One benchmark counts once per record, however many times the record
        // mentions it: `experiments` must stay a count of experiments.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for sample in record.benchmarks {
            if !sample.noise_pct.is_finite() || sample.noise_pct < 0.0 {
                continue;
            }
            if !seen.insert(sample.name.clone()) {
                continue;
            }
            by_name
                .entry(sample.name)
                .or_default()
                .push(sample.noise_pct);
        }
    }

    let mut out: Vec<Floor> = by_name
        .into_iter()
        .filter_map(|(name, values)| {
            // At least MIN_EXPERIMENTS experiments, counted per benchmark —
            // not the number of lines in the file, which is HIGHER whenever
            // one experiment measured only part of the benchmark set.
            if values.len() < MIN_EXPERIMENTS {
                return None;
            }
            Some(Floor {
                name,
                experiments: values.len(),
                noise_pct: median(&values),
            })
        })
        .collect();
    out.sort_by(|a, b| b.noise_pct.total_cmp(&a.noise_pct));
    Ok(out)
}

/// Whether `min_effect_pct` is below `floor` — that is, whether the KEEP
/// threshold asks for a change this run cannot reliably see.
pub fn min_effect_below(floor: &Floor, min_effect_pct: f64) -> bool {
    floor.noise_pct > min_effect_pct
}

/// One row of facts: `count_words ±1.83 % over 6 evals — min_effect_pct ...`,
/// without any label column or indent — the caller owns the layout.
pub fn describe(floor: &Floor, min_effect_pct: Option<f64>) -> String {
    let mut out = format!(
        "{} ±{:.2} % over {} evals",
        floor.name, floor.noise_pct, floor.experiments
    );
    if let Some(min_effect) = min_effect_pct {
        let position = if min_effect_below(floor, min_effect) {
            "below"
        } else {
            "above"
        };
        out.push_str(&format!(
            " — min_effect_pct {min_effect:.1} % is {position} it"
        ));
    }
    out
}

/// The `status` block: at most [`STATUS_ROWS`] rows, then `and N more`.
pub fn status_block(floors: &[Floor], min_effect_pct: Option<f64>) -> String {
    if floors.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, floor) in floors.iter().take(STATUS_ROWS).enumerate() {
        // The label column only on the first row; `status`'s other lines pad
        // theirs to 14 columns, and every continuation aligns with them.
        let label = if i == 0 { "noise floor" } else { "" };
        out.push_str(&format!(
            "{label:<14} {}\n",
            describe(floor, min_effect_pct)
        ));
    }
    if floors.len() > STATUS_ROWS {
        out.push_str(&format!(
            "{:<14} and {} more\n",
            "",
            floors.len() - STATUS_ROWS
        ));
    }
    out
}

/// The `report` block: a section header and every row.
///
/// Not capped: a report is read deliberately, not glanced at in a shell.
pub fn report_block(floors: &[Floor], min_effect_pct: Option<f64>) -> String {
    if floors.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nnoise floor:\n");
    for floor in floors {
        out.push_str(&format!("  {}\n", describe(floor, min_effect_pct)));
    }
    out
}

/// The same floors as JSON rows, for `status --json` and `report --json`.
///
/// `min_effect_below` is omitted when the config could not be read: the
/// comparison was not made, so there is no boolean to report.
pub fn rows_json(floors: &[Floor], min_effect_pct: Option<f64>) -> serde_json::Value {
    serde_json::Value::Array(
        floors
            .iter()
            .map(|floor| {
                let mut row = serde_json::json!({
                    "benchmark": floor.name,
                    "experiments": floor.experiments,
                    "noise_pct": floor.noise_pct,
                });
                if let Some(min_effect) = min_effect_pct {
                    row["min_effect_below"] =
                        serde_json::json!(min_effect_below(floor, min_effect));
                }
                row
            })
            .collect(),
    )
}

/// `min_effect_pct` from `<root>/.rustperf/config.yaml`, or `None` when that
/// file is missing or unusable.
///
/// Advisory only: a broken config already fails `eval` with a clear message,
/// and `status`/`report` must keep describing the run rather than refuse to.
pub fn min_effect_pct(root: &Path) -> Option<f64> {
    crate::config::Config::load(&root.join(crate::config::CONFIG_PATH))
        .ok()
        .map(|cfg| cfg.min_effect_pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(name: &str, values: &[f64]) -> bench::Set {
        let mut s = bench::Set::new();
        for &v in values {
            s.record(name, v);
        }
        s
    }

    fn delta(name: &str, base: f64, cand: f64) -> bench::Delta {
        bench::Delta {
            name: name.to_string(),
            base_center: base,
            cand_center: cand,
            ratio: cand / base,
            pct_change: (cand / base - 1.0) * 100.0,
            ci_low: f64::NAN,
            ci_high: f64::NAN,
            p: 0.5,
            alpha: 0.05,
            significant: false,
            n_base: 0,
            n_cand: 0,
            warnings: Vec::new(),
        }
    }

    fn sample(name: &str, noise_pct: f64) -> Sample {
        Sample {
            name: name.to_string(),
            noise_pct,
            base_ns: 100.0,
            cand_ns: 90.0,
            n: 10,
        }
    }

    /// Writes one record per value, in order.
    fn write_evals(dir: &Path, name: &str, values: &[f64]) {
        for &v in values {
            append(dir, &[sample(name, v)]).expect("append a noise record");
        }
    }

    #[test]
    fn mad_is_the_median_of_the_absolute_deviations() {
        // median of [1,2,3,4,5] is 3; deviations [2,1,0,1,2] sort to
        // [0,1,1,2,2] whose median is 1.
        assert!((mad(&[1.0, 2.0, 3.0, 4.0, 5.0]) - 1.0).abs() < 1e-12);
        // Even length: median of [1,2,3,4] is 2.5, deviations
        // [1.5,0.5,0.5,1.5] sort to [0.5,0.5,1.5,1.5] -> 1.0.
        assert!((mad(&[1.0, 2.0, 3.0, 4.0]) - 1.0).abs() < 1e-12);
        assert_eq!(mad(&[]), 0.0);
    }

    #[test]
    fn noise_pct_is_the_median_standard_error_over_the_median() {
        let values = [100.0, 101.0, 102.0, 103.0, 104.0];
        // The rounds' robust sigma is 1.4826 * 1; the median of five rounds
        // has standard error sigma * 1.2533 / sqrt(5), and that is what the
        // verdict's median comparison can resolve.
        let sigma = MAD_TO_SIGMA * 1.0;
        let expected = sigma / 102.0 * 100.0 * MEDIAN_SE_FACTOR / 5f64.sqrt();
        let got = noise_pct(&values).expect("five rounds give a floor");
        assert!((got - expected).abs() < 1e-12, "got {got}, want {expected}");
        // Spelled out with the literal factor, so the formula cannot drift
        // silently: 1.4826 * 1 / 102 * 100 * 1.2533 / sqrt(5).
        assert!((got - 0.81469).abs() < 1e-4, "got {got}");
    }

    // The documented remedy for a floor too high is "raise count". That only
    // works if the reported number divides by sqrt(n): the same round-to-round
    // dispersion measured with more rounds is a more precise median.
    #[test]
    fn more_rounds_of_the_same_dispersion_lower_the_floor() {
        let five = [100.0, 101.0, 102.0, 103.0, 104.0];
        let ten: Vec<f64> = five.iter().chain(five.iter()).copied().collect();
        assert_eq!(mad(&ten), mad(&five), "same dispersion by construction");
        let f5 = noise_pct(&five).expect("floor at five rounds");
        let f10 = noise_pct(&ten).expect("floor at ten rounds");
        assert!(f10 < f5, "ten rounds {f10} should be below five {f5}");
        let ratio = f5 / f10;
        assert!((ratio - 2f64.sqrt()).abs() < 1e-12, "ratio {ratio}");
    }

    #[test]
    fn identical_rounds_have_a_zero_mad_and_zero_noise() {
        assert_eq!(mad(&[7.0; 6]), 0.0);
        assert_eq!(
            noise_pct(&[7.0; 6]),
            Some(0.0),
            "zero dispersion is a real floor, not a missing one"
        );
    }

    #[test]
    fn an_empty_or_single_round_series_has_no_floor() {
        assert_eq!(noise_pct(&[]), None);
        assert_eq!(
            noise_pct(&[42.0]),
            None,
            "dispersion needs at least two rounds"
        );
        assert_eq!(
            noise_pct(&[0.0, 0.0, 0.0]),
            None,
            "a zero median leaves no denominator"
        );
        assert_eq!(
            noise_pct(&[1.0, f64::NAN]),
            None,
            "a NaN median leaves no denominator"
        );
        assert_eq!(
            noise_pct(&[f64::INFINITY, f64::INFINITY]),
            None,
            "an infinite median leaves no denominator"
        );
    }

    #[test]
    fn a_sample_carries_the_rounds_and_both_centers() {
        let base = set_of("count_words", &[100.0, 101.0, 102.0, 103.0]);
        let d = delta("count_words", 101.5, 80.0);
        let s = samples(&base, std::slice::from_ref(&d));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "count_words");
        assert_eq!(s[0].n, 4);
        assert_eq!(s[0].base_ns, 101.5);
        assert_eq!(s[0].cand_ns, 80.0);
        let expected = noise_pct(&[100.0, 101.0, 102.0, 103.0]).expect("floor");
        assert!((s[0].noise_pct - expected).abs() < 1e-12);
    }

    // The whole point of sampling only the baseline side: after a KEEP the
    // baseline code changes, and a centre shift between experiments is that
    // change, not noise. A candidate that implodes mid-run must not inflate
    // the floor either.
    #[test]
    fn the_noise_comes_from_the_baseline_side_alone() {
        let base = set_of("b", &[100.0, 100.0, 100.0, 100.0]);
        let mut cand = bench::Set::new();
        for v in [1.0, 50.0, 400.0, 9000.0] {
            cand.record("b", v);
        }
        let deltas = bench::compare_all(&base, &cand).expect("both sides have rounds");
        let s = samples(&base, &deltas);
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].noise_pct, 0.0,
            "a wildly dispersed candidate is not baseline noise"
        );
    }

    #[test]
    fn a_record_is_one_json_line_with_the_documented_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Sample {
            name: "count_words".into(),
            noise_pct: 1.8,
            base_ns: 44702.0,
            cand_ns: 38636.0,
            n: 10,
        };
        append(dir.path(), std::slice::from_ref(&s)).expect("append");

        let raw = std::fs::read_to_string(dir.path().join(NOISE_FILE)).expect("read back");
        assert_eq!(raw.lines().count(), 1, "{raw}");
        let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("one JSON object");
        assert_eq!(v.as_object().expect("object").len(), 1, "one key: {v}");
        assert_eq!(v["benchmarks"][0]["name"], "count_words");
        assert_eq!(v["benchmarks"][0]["n"], 10);
        assert!((v["benchmarks"][0]["noise_pct"].as_f64().expect("number") - 1.8).abs() < 1e-12);
    }

    #[test]
    fn each_measured_experiment_appends_exactly_one_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        record(
            dir.path(),
            &set_of("b", &[100.0, 101.0, 102.0, 103.0]),
            &[delta("b", 101.5, 100.0)],
        )
        .expect("first record");
        record(
            dir.path(),
            &set_of("b", &[100.0, 101.0, 102.0, 103.0]),
            &[delta("b", 101.5, 100.0)],
        )
        .expect("second record");
        let raw = std::fs::read_to_string(dir.path().join(NOISE_FILE)).expect("read back");
        assert_eq!(raw.lines().count(), 2, "{raw}");
    }

    #[test]
    fn jsonl_round_trips_through_floors() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_evals(dir.path(), "count_words", &[2.5; MIN_EXPERIMENTS]);
        let floors = floors(dir.path()).expect("floors");
        assert_eq!(floors.len(), 1);
        assert_eq!(floors[0].name, "count_words");
        assert_eq!(floors[0].experiments, MIN_EXPERIMENTS);
        assert!((floors[0].noise_pct - 2.5).abs() < 1e-12);
    }

    #[test]
    fn a_missing_file_is_no_floors_rather_than_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(floors(dir.path()).expect("missing file is fine").is_empty());
    }

    #[test]
    fn a_malformed_line_is_skipped_rather_than_failing_the_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_evals(dir.path(), "b", &[1.5; MIN_EXPERIMENTS]);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(NOISE_FILE))
            .expect("open for append");
        f.write_all(b"{not json\n").expect("write");
        f.write_all(b"\n").expect("write");
        // Well-formed JSON, but not a record: a missing field is malformed too.
        f.write_all(b"{\"benchmarks\":[{\"name\":\"b\"}]}\n")
            .expect("write");
        f.flush().expect("flush");

        let floors = floors(dir.path()).expect("bad lines are skipped, not fatal");
        assert_eq!(floors.len(), 1);
        assert_eq!(floors[0].experiments, MIN_EXPERIMENTS);
    }

    #[test]
    fn a_record_missing_the_later_fields_still_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A record written before `base_ns`/`cand_ns`/`n` existed. The
        // `#[serde(default)]`s must keep it readable instead of discarding
        // the line (or, worse, resetting the whole floor).
        let mut raw = String::new();
        for _ in 0..MIN_EXPERIMENTS {
            raw.push_str("{\"benchmarks\":[{\"name\":\"b\",\"noise_pct\":1.5}]}\n");
        }
        std::fs::write(dir.path().join(NOISE_FILE), raw).expect("write");

        let floors = floors(dir.path()).expect("floors");
        assert_eq!(floors.len(), 1);
        assert_eq!(floors[0].experiments, MIN_EXPERIMENTS);
        assert!((floors[0].noise_pct - 1.5).abs() < 1e-12);
    }

    #[test]
    fn a_benchmark_needs_four_experiments_before_its_floor_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_evals(dir.path(), "b", &[1.0; MIN_EXPERIMENTS - 1]);
        assert!(
            floors(dir.path()).expect("floors").is_empty(),
            "{} experiments is one short",
            MIN_EXPERIMENTS - 1
        );
        write_evals(dir.path(), "b", &[1.0]);
        assert_eq!(floors(dir.path()).expect("floors").len(), 1);
    }

    #[test]
    fn the_floor_is_the_median_across_experiments_not_the_mean() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_evals(dir.path(), "b", &[1.0, 2.0, 3.0, 100.0]);
        let floors = floors(dir.path()).expect("floors");
        assert!(
            (floors[0].noise_pct - 2.5).abs() < 1e-12,
            "median of [1,2,3,100], got {}",
            floors[0].noise_pct
        );
        assert_eq!(floors[0].experiments, 4);
    }

    // `experiments` counts RECORDS, not samples: a record that repeats a
    // benchmark (a hand-edited or double-flushed line) must not reach the
    // four-experiment threshold on its own.
    #[test]
    fn a_duplicate_benchmark_in_one_record_counts_as_one_experiment() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two ordinary experiments, plus one record that repeats the same
        // benchmark four times: three experiments, not six.
        write_evals(dir.path(), "b", &[1.0; MIN_EXPERIMENTS - 2]);
        append(
            dir.path(),
            &[
                sample("b", 1.0),
                sample("b", 1.0),
                sample("b", 1.0),
                sample("b", 1.0),
            ],
        )
        .expect("one record with duplicates");
        assert!(
            floors(dir.path()).expect("floors").is_empty(),
            "{} experiments is one short",
            MIN_EXPERIMENTS - 1
        );
    }

    #[test]
    fn floors_are_sorted_noisiest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_evals(dir.path(), "quiet", &[0.5; MIN_EXPERIMENTS]);
        write_evals(dir.path(), "noisy", &[9.0; MIN_EXPERIMENTS]);
        write_evals(dir.path(), "middling", &[3.0; MIN_EXPERIMENTS]);
        let names: Vec<String> = floors(dir.path())
            .expect("floors")
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, vec!["noisy", "middling", "quiet"]);
    }

    #[test]
    fn min_effect_below_the_floor_is_flagged_and_above_is_not() {
        let f = Floor {
            name: "count_words".into(),
            experiments: 6,
            noise_pct: 1.83,
        };
        assert!(
            min_effect_below(&f, 1.0),
            "1.0 % cannot see 1.83 % of noise"
        );
        assert!(!min_effect_below(&f, 2.0), "2.0 % is a visible effect");
        assert!(!min_effect_below(&f, 1.83), "equal is not below");
    }

    #[test]
    fn describe_renders_the_facts_and_the_comparison() {
        let f = Floor {
            name: "count_words".into(),
            experiments: 6,
            noise_pct: 1.83,
        };
        let s = describe(&f, Some(1.0));
        assert!(s.contains("count_words ±1.83 % over 6 evals"), "{s}");
        assert!(s.ends_with("min_effect_pct 1.0 % is below it"), "{s}");

        let s = describe(&f, Some(2.0));
        assert!(s.ends_with("min_effect_pct 2.0 % is above it"), "{s}");
    }

    #[test]
    fn describe_without_a_config_omits_the_comparison() {
        let f = Floor {
            name: "count_words".into(),
            experiments: 6,
            noise_pct: 1.83,
        };
        assert_eq!(describe(&f, None), "count_words ±1.83 % over 6 evals");
    }

    fn floors_of(n: usize) -> Vec<Floor> {
        (0..n)
            .map(|i| Floor {
                name: format!("bench{i}"),
                experiments: 4,
                noise_pct: 3.0 - i as f64 * 0.5,
            })
            .collect()
    }

    #[test]
    fn status_caps_the_list_at_three_rows_and_counts_the_rest() {
        let text = status_block(&floors_of(5), Some(1.0));
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(text.starts_with("noise floor    bench0"), "{text}");
        assert!(text.contains("and 2 more"), "{text}");
        assert!(
            text.ends_with('\n'),
            "the block ends its own line: {text:?}"
        );
    }

    #[test]
    fn status_says_nothing_when_there_are_no_floors() {
        assert!(status_block(&[], Some(1.0)).is_empty());
        assert!(report_block(&[], Some(1.0)).is_empty());
    }

    #[test]
    fn report_lists_every_row_under_a_header() {
        let text = report_block(&floors_of(5), Some(1.0));
        assert!(
            text.starts_with("\nnoise floor:\n"),
            "a separate section: {text}"
        );
        assert_eq!(
            text.lines().filter(|l| !l.is_empty()).count(),
            6,
            "header plus five rows: {text}"
        );
        assert!(
            text.contains("bench4"),
            "all rows, not the capped three: {text}"
        );
        assert!(!text.contains("and 2 more"), "{text}");
    }

    #[test]
    fn json_rows_carry_the_documented_keys() {
        let f = floors_of(1);
        let v = rows_json(&f, Some(1.0));
        assert_eq!(v[0]["benchmark"], "bench0");
        assert_eq!(v[0]["experiments"], 4);
        assert!((v[0]["noise_pct"].as_f64().expect("number") - 3.0).abs() < 1e-12);
        assert_eq!(v[0]["min_effect_below"], true);

        let v = rows_json(&f, Some(9.0));
        assert_eq!(v[0]["min_effect_below"], false);
    }

    #[test]
    fn json_rows_omit_the_comparison_when_the_config_is_unreadable() {
        let v = rows_json(&floors_of(1), None);
        assert!(
            v[0].get("min_effect_below").is_none(),
            "no comparison was made: {v}"
        );
        assert_eq!(v[0]["benchmark"], "bench0");
    }

    #[test]
    fn a_broken_config_leaves_the_comparison_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(min_effect_pct(dir.path()), None, "no config at all");

        std::fs::create_dir_all(dir.path().join(".rustperf")).expect("mkdir");
        std::fs::write(dir.path().join(crate::config::CONFIG_PATH), "count: nope\n")
            .expect("write a broken config");
        assert_eq!(
            min_effect_pct(dir.path()),
            None,
            "a config that will not parse"
        );

        std::fs::write(
            dir.path().join(crate::config::CONFIG_PATH),
            "min_effect_pct: 2.5\n",
        )
        .expect("write a good config");
        assert_eq!(min_effect_pct(dir.path()), Some(2.5));
    }
}
