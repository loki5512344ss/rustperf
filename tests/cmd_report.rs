mod common;
use common::TestRepo;
use rustperf_core::results::{self, Row};

fn row(status: &str, score: f64) -> Row {
    Row {
        commit: "abc1234".into(),
        score,
        best_bench_delta: -10.0,
        status: status.into(),
        description: "x".into(),
        version: "0.3.0".into(),
    }
}

/// A repository with an initialized run, so the current branch names a run
/// tag and `report` can find the run's state directory.
fn ready(tag: &str) -> TestRepo {
    let repo = TestRepo::demo();
    common::run_cli(&repo, &["init"]);
    common::git(repo.path(), &["add", "-A"]);
    common::git(repo.path(), &["commit", "-qm", "init"]);
    let out = common::run_cli(&repo, &["baseline", "--tag", tag]);
    assert!(
        out.status.success(),
        "baseline: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

/// Writes `noise.jsonl` into the run's state directory as the harness would,
/// one record per value.
fn write_noise(dir: &std::path::Path, name: &str, pcts: &[f64]) {
    let mut text = String::new();
    for &pct in pcts {
        text.push_str(&format!(
            "{{\"benchmarks\":[{{\"name\":\"{name}\",\"noise_pct\":{pct},\
             \"base_ns\":44702.0,\"cand_ns\":38636.0,\"n\":10}}]}}\n"
        ));
    }
    std::fs::write(dir.join("noise.jsonl"), text).expect("write noise history");
}

#[test]
fn report_counts_by_status_and_multiplies_kept_scores() {
    let repo = TestRepo::demo();
    let p = repo.path().join(results::RESULTS_PATH);
    results::append(&p, &row("KEEP", 0.8)).unwrap();
    results::append(&p, &row("KEEP", 0.5)).unwrap();
    results::append(&p, &row("DISCARD", 1.0)).unwrap();
    results::append(&p, &row("FAIL", 0.0)).unwrap();

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(
        text.contains("2 keep") || text.contains("keep    2"),
        "{text}"
    );
    // 0.8 * 0.5 = 0.40 cumulative, i.e. a 2.5x speedup — the PRODUCT, not the
    // latest kept score, because measure_commit advances after every KEEP.
    assert!(
        text.contains("0.40") || text.contains("2.5"),
        "cumulative must compound:\n{text}"
    );
}

#[test]
fn report_with_no_kept_experiments_says_so_rather_than_printing_zero() {
    let repo = TestRepo::demo();
    let p = repo.path().join(results::RESULTS_PATH);
    results::append(&p, &row("DISCARD", 1.0)).unwrap();
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(!text.contains("0.0%"), "{text}");
    assert!(
        text.to_lowercase().contains("no experiments were kept"),
        "{text}"
    );
}

#[test]
fn report_on_an_empty_log_is_not_an_error() {
    let repo = TestRepo::demo();
    assert!(common::run_cli(&repo, &["report"]).status.success());
}

#[test]
fn doctor_always_exits_zero_and_prints_findings() {
    let repo = TestRepo::demo();
    let out = common::run_cli(&repo, &["doctor"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "doctor is informational and always exits 0"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).is_empty());
}

#[test]
fn version_reports_a_build() {
    let repo = TestRepo::demo();
    let out = common::run_cli(&repo, &["version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

// samply is not installed on CI, and profile must degrade rather than fail.
#[test]
fn profile_without_samply_explains_how_to_install_it_and_exits_zero() {
    let repo = TestRepo::demo();
    common::run_cli(&repo, &["init"]);
    let out = common::run_cli(&repo, &["profile"]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if !text.contains("samply") {
        return; // samply IS installed here; the happy path is covered elsewhere
    }
    assert_eq!(
        out.status.code(),
        Some(0),
        "profile is advisory and must not fail a run"
    );
    assert!(text.contains("cargo install samply"), "{text}");
}

#[test]
fn report_json_is_one_parseable_object_with_the_same_numbers() {
    let repo = TestRepo::demo();
    let p = repo.path().join(results::RESULTS_PATH);
    results::append(&p, &row("KEEP", 0.8)).unwrap();
    results::append(&p, &row("KEEP", 0.5)).unwrap();
    results::append(&p, &row("DISCARD", 1.0)).unwrap();

    let out = common::run_cli(&repo, &["report", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["experiments"], 3);
    assert_eq!(v["keep"], 2);
    assert!((v["cumulative_score"].as_f64().unwrap() - 0.4).abs() < 1e-9);
}

// H1: the report carries the per-benchmark noise floor once at least four
// experiments have measured it, and says when `min_effect_pct` sits below it.
#[test]
fn report_shows_the_noise_floor_and_flags_min_effect_below_it() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    write_noise(&dir, "count_words", &[1.0, 2.0, 3.0, 4.0]);

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(text.contains("noise floor"), "{text}");
    // Median of [1,2,3,4], not the mean: 2.50 %.
    assert!(text.contains("count_words ±2.50 % over 4 evals"), "{text}");
    assert!(
        text.contains("min_effect_pct 1.0 % is below it"),
        "the default 1 % threshold is under a 2.5 % floor:\n{text}"
    );

    let out = common::run_cli(&repo, &["report", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["noise"][0]["benchmark"], "count_words");
    assert_eq!(v["noise"][0]["experiments"], 4);
    assert_eq!(v["noise"][0]["min_effect_below"], true);
}

// Fewer than four experiments is not a floor yet: one unlucky eval must not
// define what the run can see.
#[test]
fn report_omits_a_benchmark_measured_too_few_times() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    write_noise(&dir, "count_words", &[1.0, 2.0, 3.0]);

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(!text.contains("noise floor"), "{text}");

    let out = common::run_cli(&repo, &["report", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert!(v.get("noise").is_none(), "{v}");
}

// The tag can only come from a run branch. On any other branch the noise
// section is omitted; the report itself must still work.
#[test]
fn report_omits_the_noise_section_off_a_run_branch() {
    let repo = TestRepo::demo();
    let p = repo.path().join(results::RESULTS_PATH);
    results::append(&p, &row("KEEP", 0.9)).unwrap();

    let out = common::run_cli(&repo, &["report"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("1 keep"), "{text}");
    assert!(!text.contains("noise floor"), "{text}");
}

/// Writes `requests.jsonl` into the run's state directory as `rustperf
/// request` would.
fn write_requests(dir: &std::path::Path, requests: &[(&str, &str, usize)]) {
    let mut text = String::new();
    for (kind, what, experiment) in requests {
        text.push_str(&format!(
            "{{\"kind\":\"{kind}\",\"text\":\"{what}\",\"commit\":\"abc1234\",\
             \"experiment\":{experiment}}}\n"
        ));
    }
    std::fs::write(dir.join("requests.jsonl"), text).expect("write requests");
}

// I2: the report lists what the agent asked a human to provision, in text and
// in `--json`; with an empty queue there is no section and no key at all, and
// a queue that cannot be read never fails the report.
#[test]
fn report_lists_requests_for_a_human() {
    let repo = ready("t1");

    // Nothing pending: no section, no key.
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(!text.contains("requests for a human"), "{text}");
    let out = common::run_cli(&repo, &["report", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert!(v.get("requests").is_none(), "{v}");

    let dir = common::state_dir(&repo, "t1");
    write_requests(
        &dir,
        &[
            ("allocator", "bumpalo for the parser arena", 3),
            ("flags", "codegen-units=1", 4),
        ],
    );

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["report"]).stdout).into_owned();
    assert!(text.contains("requests for a human"), "{text}");
    assert!(
        text.contains("  [allocator] bumpalo for the parser arena (experiment 3, commit abc1234)"),
        "{text}"
    );
    assert!(
        text.contains("  [flags] codegen-units=1 (experiment 4, commit abc1234)"),
        "{text}"
    );

    let out = common::run_cli(&repo, &["report", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["requests"][0]["kind"], "allocator");
    assert_eq!(v["requests"][0]["text"], "bumpalo for the parser arena");
    assert_eq!(v["requests"][0]["commit"], "abc1234");
    assert_eq!(v["requests"][0]["experiment"], 3);
    assert_eq!(v["requests"][1]["kind"], "flags");

    // Advisory like the noise history: an unreadable queue is omitted, not an
    // error that turns a useful report into a failure.
    std::fs::remove_file(dir.join("requests.jsonl")).unwrap();
    std::fs::create_dir(dir.join("requests.jsonl")).unwrap();
    let out = common::run_cli(&repo, &["report"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!text.contains("requests for a human"), "{text}");
}
