mod common;
use common::TestRepo;

fn ready(tag: &str) -> TestRepo {
    let repo = TestRepo::demo();
    common::run_cli(&repo, &["init"]);
    common::git(repo.path(), &["add", "-A"]);
    common::git(repo.path(), &["commit", "-qm", "init"]);
    common::run_cli(&repo, &["baseline", "--tag", tag]);
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
fn status_reports_the_run_and_writes_nothing() {
    let repo = ready("t1");
    let before = common::tree_snapshot(repo.path());
    let out = common::run_cli(&repo, &["status"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "run tag",
        "t1",
        "rustperf/t1",
        "baseline",
        "worktree",
        "stop",
    ] {
        assert!(
            text.contains(expected),
            "status must mention {expected:?}:\n{text}"
        );
    }
    // The snapshot skips directories and `target/`, so guard against it
    // silently degrading into a comparison of two empty lists — a test that
    // cannot fail is worse than no test.
    assert!(
        before.len() > 10,
        "tree_snapshot should see the repo's real files, saw {}",
        before.len()
    );
    // Checking on a run must not be able to change it.
    assert_eq!(common::tree_snapshot(repo.path()), before);
}

#[test]
fn status_works_from_another_branch_with_an_explicit_tag() {
    let repo = ready("t1");
    common::git(repo.path(), &["checkout", "-q", "main"]);
    assert!(
        common::run_cli(&repo, &["status", "--tag", "t1"])
            .status
            .success()
    );
    // Without the tag there is nothing to infer from a non-run branch.
    assert!(!common::run_cli(&repo, &["status"]).status.success());
}

#[test]
fn stop_requests_are_visible_to_status_and_can_be_cleared() {
    let repo = ready("t1");
    assert!(common::run_cli(&repo, &["stop"]).status.success());
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(text.contains("requested"), "{text}");

    assert!(
        common::run_cli(&repo, &["stop", "--clear"])
            .status
            .success()
    );
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(text.contains("not requested"), "{text}");
}

#[test]
fn stop_tells_the_user_what_it_did_and_what_happens_next() {
    let repo = ready("t1");
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["stop"]).stdout).into_owned();
    assert!(text.contains("current experiment"), "{text}");
}

// I5: `status` used to tell a human to run `baseline --tag <tag> --force`
// to recreate a missing worktree — a command `baseline` always refuses,
// --force or not, because its tag-collision check is unconditional. The
// advice must both (a) not recommend that dead end and (b) name a command
// that actually works.
#[test]
fn status_advises_a_real_fix_for_a_missing_worktree_not_a_dead_end() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    std::fs::remove_dir_all(dir.join("baseline-worktree")).unwrap();

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(text.contains("MISSING"), "{text}");
    assert!(
        !text.contains("re-run `rustperf baseline --tag t1 --force`"),
        "must not recommend a command baseline always refuses:\n{text}"
    );
    assert!(
        text.contains("git") && text.contains("worktree add"),
        "must name the real fix:\n{text}"
    );

    // Prove the old advice really was a dead end: baseline --force still
    // refuses outright for a tag that already has one.
    assert!(
        !common::run_cli(&repo, &["baseline", "--tag", "t1", "--force"])
            .status
            .success(),
        "a tag collision must be refused even with --force"
    );
}

// M7: a stale `eval.pid` — left by an `eval` that died without releasing
// its claim — must be cleared by `stop --force` even when there is nothing
// live to signal, matching the Go original's "keep status honest" behavior.
#[test]
fn stop_force_clears_a_stale_eval_pid_with_nothing_to_signal() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    std::fs::write(dir.join("eval.pid"), "999999999\n").unwrap();
    assert!(dir.join("eval.pid").exists());

    let out = common::run_cli(&repo, &["stop", "--force"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("nothing to signal"), "{text}");
    assert!(
        !dir.join("eval.pid").exists(),
        "a stale pid file must be cleared, not left behind"
    );
}

#[test]
fn status_json_reports_the_run_as_one_object_and_writes_nothing() {
    let repo = ready("t1");
    let before = common::tree_snapshot(repo.path());
    let out = common::run_cli(&repo, &["status", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["tag"], "t1");
    assert_eq!(v["branch"], "rustperf/t1");
    assert_eq!(v["branch_checked_out"], true);
    assert_eq!(v["stop_requested"], false);
    assert_eq!(v["worktree_exists"], true);
    assert_eq!(v["baseline_commit"], v["measure_commit"]);
    assert_eq!(common::tree_snapshot(repo.path()), before);

    assert!(common::run_cli(&repo, &["stop"]).status.success());
    let out = common::run_cli(&repo, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["stop_requested"], true);
}

// H1: `status` reports the per-benchmark noise floor after the experiments
// line, with the `min_effect_pct` comparison, and the same facts in `--json`.
#[test]
fn status_reports_the_noise_floor_and_flags_min_effect_below_it() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    write_noise(&dir, "count_words", &[1.0, 2.0, 3.0, 4.0]);

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    // Median of [1,2,3,4], not the mean: 2.50 %.
    assert!(
        text.contains("noise floor    count_words ±2.50 % over 4 evals"),
        "{text}"
    );
    assert!(
        text.contains("min_effect_pct 1.0 % is below it"),
        "a 2.5 % floor is above the default 1 % threshold:\n{text}"
    );
    // The noise line sits after the experiments line, not floating free.
    let experiments = text.find("experiments").expect("an experiments line");
    let noise = text.find("noise floor").expect("a noise line");
    assert!(experiments < noise, "{text}");

    let out = common::run_cli(&repo, &["status", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["noise"][0]["benchmark"], "count_words");
    assert_eq!(v["noise"][0]["experiments"], 4);
    assert_eq!(v["noise"][0]["min_effect_below"], true);
}

// No data yet is not an empty array: the key is left out entirely, so a
// parser cannot read "nothing measured" as "measured, found no noise".
#[test]
fn status_json_omits_the_noise_key_without_data() {
    let repo = ready("t1");
    let out = common::run_cli(&repo, &["status", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert!(v.get("noise").is_none(), "{v}");

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(!text.contains("noise floor"), "{text}");
}

// Reading the noise history must not write anything, like every other line
// `status` prints.
#[test]
fn status_with_a_noise_history_still_writes_nothing() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");
    write_noise(&dir, "count_words", &[1.0, 2.0, 3.0, 4.0]);

    let before = common::tree_snapshot(repo.path());
    let noise_before = std::fs::read(dir.join("noise.jsonl")).unwrap();
    assert!(common::run_cli(&repo, &["status"]).status.success());
    assert!(
        common::run_cli(&repo, &["status", "--json"])
            .status
            .success()
    );
    assert_eq!(common::tree_snapshot(repo.path()), before);
    assert_eq!(
        std::fs::read(dir.join("noise.jsonl")).unwrap(),
        noise_before,
        "status must only ever read the noise history"
    );
}

/// Writes `requests.jsonl` into the run's state directory as `rustperf
/// request` would.
fn write_requests(dir: &std::path::Path, count: usize) {
    let mut text = String::new();
    for i in 0..count {
        text.push_str(&format!(
            "{{\"kind\":\"allocator\",\"text\":\"need an arena {i}\",\
             \"commit\":\"abc1234\",\"experiment\":{}}}\n",
            i + 1
        ));
    }
    std::fs::write(dir.join("requests.jsonl"), text).expect("write requests");
}

// I2: pending requests to a human are counted after the noise line, and only
// while there are some. `status` reads the queue and nothing else.
#[test]
fn status_counts_pending_requests_and_only_reads_them() {
    let repo = ready("t1");
    let dir = common::state_dir(&repo, "t1");

    // A run with an empty queue says nothing about requests, and `--json` has
    // no key — not `0`, which would read as "read it, none pending".
    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(!text.contains("pending"), "{text}");
    let out = common::run_cli(&repo, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert!(v.get("requests").is_none(), "{v}");

    write_requests(&dir, 2);
    let queue_before = std::fs::read(dir.join("requests.jsonl")).unwrap();
    let before = common::tree_snapshot(repo.path());

    let text = String::from_utf8_lossy(&common::run_cli(&repo, &["status"]).stdout).into_owned();
    assert!(
        text.contains("requests       2 pending — see 'rustperf report'"),
        "{text}"
    );
    let experiments = text.find("experiments").expect("an experiments line");
    let requests = text.find("requests").expect("a requests line");
    assert!(experiments < requests, "{text}");

    let out = common::run_cli(&repo, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    assert_eq!(v["requests"], 2);

    assert_eq!(common::tree_snapshot(repo.path()), before);
    assert_eq!(
        std::fs::read(dir.join("requests.jsonl")).unwrap(),
        queue_before,
        "status must only ever read the request queue"
    );
}
