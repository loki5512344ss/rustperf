//! The `request` command: an agent records that the next experiment needs a
//! human to provision something, instead of spending the experiment on a
//! change the gates refuse by construction.
//!
//! Only the two tests that exercise a recorded request need a baseline; the
//! usage-error and wrong-branch tests are deliberately cheap and stop before
//! the run is resolved.

mod common;
use common::TestRepo;

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

/// The stored queue, one parsed JSON object per line.
fn stored(repo: &TestRepo, tag: &str) -> Vec<serde_json::Value> {
    let path = common::state_dir(repo, tag).join("requests.jsonl");
    let text = std::fs::read_to_string(&path).expect("requests.jsonl");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect()
}

#[test]
fn request_records_kind_text_commit_and_experiment() {
    let repo = ready("t1");

    let out = common::run_cli(&repo, &["request", "bumpalo for the parser arena"]);
    assert!(
        out.status.success(),
        "request: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let confirmation = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(confirmation.contains("kind other"), "{confirmation}");
    assert!(
        confirmation.contains("requests.jsonl"),
        "must name the file it wrote:\n{confirmation}"
    );
    assert!(
        confirmation.contains("does not lift any gate"),
        "must say provisioning is still the human's call:\n{confirmation}"
    );

    let out = common::run_cli(
        &repo,
        &["request", "one unsafe copy loop", "--kind", "unsafe"],
    );
    assert!(
        out.status.success(),
        "request --kind: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows = stored(&repo, "t1");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["kind"], "other");
    assert_eq!(rows[0]["text"], "bumpalo for the parser arena");
    // No experiment has run: the request is filed against the one that comes
    // next.
    assert_eq!(rows[0]["experiment"], 1);
    assert!(
        !rows[0]["commit"].as_str().unwrap_or_default().is_empty(),
        "the request must name the commit it was written at: {}",
        rows[0]
    );
    assert_eq!(rows[1]["kind"], "unsafe");
    assert_eq!(rows[1]["text"], "one unsafe copy loop");
    assert_eq!(rows[1]["experiment"], 1);
}

#[test]
fn request_list_shows_the_queue_and_says_so_when_empty() {
    let repo = ready("t1");

    let out = common::run_cli(&repo, &["request", "--list"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no requests yet"),
        "{out:?}"
    );

    assert!(
        common::run_cli(&repo, &["request", "codegen-units=1", "--kind", "flags"])
            .status
            .success()
    );
    let out = common::run_cli(&repo, &["request", "--list"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("[flags] codegen-units=1"), "{text}");
    assert!(text.contains("experiment 1"), "{text}");
    assert!(text.contains("commit "), "{text}");
}

#[test]
fn request_refuses_an_unknown_kind_as_a_usage_error() {
    let repo = TestRepo::demo();
    let out = common::run_cli(&repo, &["request", "a thing", "--kind", "moonshot"]);
    assert_eq!(out.status.code(), Some(64));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("moonshot"), "{err}");
    for kind in rustperf_core::requests::KINDS {
        assert!(err.contains(kind), "must list the allowed kinds: {err}");
    }
}

#[test]
fn request_needs_a_non_empty_text() {
    let repo = TestRepo::demo();
    assert_eq!(
        common::run_cli(&repo, &["request"]).status.code(),
        Some(64),
        "a missing <text> is a usage error"
    );
    assert_eq!(
        common::run_cli(&repo, &["request", "   "]).status.code(),
        Some(64),
        "a whitespace-only <text> is a usage error"
    );
}

#[test]
fn request_refuses_a_branch_that_is_not_a_run() {
    let repo = TestRepo::demo();
    let out = common::run_cli(&repo, &["request", "bumpalo"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("baseline --tag"), "{err}");
    // Refusing must not leave anything behind: the request was never filed.
    assert_eq!(
        std::fs::read_dir(repo.state_home())
            .expect("state home")
            .count(),
        0,
        "a refused request must not create a state directory"
    );
}
