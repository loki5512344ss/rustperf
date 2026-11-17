//! The `allocs` command against a fixture whose bench targets only print the
//! protocol line — no criterion, no real measurement.
//!
//! The fixture matters more than usual here: `allocs` shells out to `cargo
//! bench`, so the test has to own a real crate with real `[[bench]]` targets.
//! Making them criterion targets would pull criterion and its ~30 transitive
//! dependencies into every test run for no benefit — the command reads stderr
//! lines, it does not care what produced them. Each target's `main` therefore
//! prints (or deliberately does not print) a single line and exits.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway crate with two bench targets, a config, a committed git history
/// and a run branch, plus the out-of-tree state directory a real `baseline`
/// would have written.
struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state_home: tempfile::TempDir,
}

impl Project {
    /// Builds the fixture with the given declared benchmark ids and
    /// `(package, target)` bench targets.
    ///
    /// `targets` may be empty: that is the "a config with no `bench_targets`"
    /// case, which must be reported and not failed.
    fn new(tag: &str, benchmarks: &[&str], targets: &[(&str, &str)]) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state_home = tempfile::tempdir().unwrap();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("benches")).unwrap();
        std::fs::create_dir_all(root.join(".rustperf")).unwrap();

        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\n\
             name = \"fakebench\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\n\
             [[bench]]\n\
             name = \"fake\"\n\
             harness = false\n\n\
             [[bench]]\n\
             name = \"silent\"\n\
             harness = false\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();

        // A provisioned benchmark: the malformed line first, to prove the
        // parser ignores it rather than choking on the whole stream, then the
        // real one. `cargo` itself also writes progress lines to stderr, so
        // "ignore everything else" is exercised for free.
        std::fs::write(
            root.join("benches/fake.rs"),
            "fn main() {\n\
             \x20   println!(\"Benchmarking demo/alloc: Warming up\");\n\
             \x20   eprintln!(\"RUSTPERF_ALLOCS benchmark=demo/alloc allocs=not-a-number\");\n\
             \x20   eprintln!(\n\
             \x20       \"RUSTPERF_ALLOCS benchmark=demo/alloc allocs=1300 bytes=101400 iters=100\"\n\
             \x20   );\n\
             }\n",
        )
        .unwrap();

        // A benchmark that runs fine and reports nothing: the "not
        // provisioned" case, which must not be an error.
        std::fs::write(root.join("benches/silent.rs"), "fn main() {}\n").unwrap();

        std::fs::write(
            root.join(".rustperf/config.yaml"),
            config_yaml(benchmarks, targets),
        )
        .unwrap();

        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["checkout", "-q", "-b", &format!("rustperf/{tag}")]);

        let project = Project {
            _dir: dir,
            root,
            state_home,
        };
        project.write_baseline(tag, benchmarks, targets);
        project
    }

    /// Writes the `baseline.json` a real `baseline` run would have left in the
    /// run's out-of-tree state directory.
    fn write_baseline(&self, tag: &str, benchmarks: &[&str], targets: &[(&str, &str)]) {
        let bench_targets: Vec<Value> = targets
            .iter()
            .map(|(package, target)| serde_json::json!({"package": package, "target": target}))
            .collect();
        let baseline = serde_json::json!({
            "tag": tag,
            "branch": format!("rustperf/{tag}"),
            "commit": "0000000",
            "created_at": "2026-11-17T00:00:00Z",
            "benchmarks": benchmarks,
            "bench_targets": bench_targets,
            "config_sha256": "0".repeat(64),
        });
        let dir = state_dir(self.state_home.path(), &self.root, tag);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("baseline.json"),
            serde_json::to_string_pretty(&baseline).unwrap(),
        )
        .unwrap();
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rustperf"))
            .args(args)
            .arg("-C")
            .arg(&self.root)
            .env("RUSTPERF_STATE_HOME", self.state_home.path())
            // Keep the fixture's build output inside its own tempdir instead
            // of inheriting whatever target directory the test run uses.
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .output()
            .expect("run rustperf")
    }
}

/// Runs a git command in `root`, panicking on failure. Fixture setup only.
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn config_yaml(benchmarks: &[&str], targets: &[(&str, &str)]) -> String {
    let mut yaml = String::from("benchmarks:\n");
    for name in benchmarks {
        yaml.push_str(&format!("  - {name}\n"));
    }
    if !targets.is_empty() {
        yaml.push_str("bench_targets:\n");
        for (package, target) in targets {
            yaml.push_str(&format!("  - package: {package}\n    target: {target}\n"));
        }
    }
    yaml
}

/// The state directory `state::state_dir` derives, spelled out here because
/// that function reads `RUSTPERF_STATE_HOME` from the *current* process while
/// the fixture only ever sets it for the child it spawns.
fn state_dir(home: &Path, root: &Path, tag: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    let abs = root.canonicalize().unwrap();
    let mut hasher = Sha256::new();
    hasher.update(abs.to_string_lossy().as_bytes());
    let key = format!("{:x}", hasher.finalize());
    home.join(&key[..16]).join(tag)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn allocs_prints_a_row_per_benchmark_and_marks_the_unprovisioned_ones() {
    let project = Project::new(
        "t1",
        &["demo/alloc", "silent/thing"],
        &[("fakebench", "fake"), ("fakebench", "silent")],
    );

    let out = project.cli(&["allocs"]);
    assert!(out.status.success(), "allocs: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("demo/alloc"), "{text}");
    // 1300 allocations / 100 iterations, 101400 bytes / 100 iterations.
    assert!(text.contains("13.0"), "{text}");
    assert!(text.contains("1014.0"), "{text}");
    assert!(text.contains("silent/thing"), "{text}");
    assert!(
        text.contains("not provisioned") && text.contains("benchmarks/allocs/README.md"),
        "an unprovisioned benchmark must point at the protocol:\n{text}"
    );
    assert!(text.contains("1 of 2 benchmarks provisioned"), "{text}");

    // The command is diagnostic only: it must not create the run's results.
    assert!(
        !project.root.join("results.tsv").exists(),
        "allocs must never write results.tsv"
    );
}

#[test]
fn allocs_json_reports_null_for_a_benchmark_that_reported_nothing() {
    let project = Project::new(
        "t1",
        &["demo/alloc", "silent/thing"],
        &[("fakebench", "fake"), ("fakebench", "silent")],
    );

    let out = project.cli(&["allocs", "--json"]);
    assert!(out.status.success(), "allocs --json: {}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).expect("one JSON object");

    assert_eq!(v["provisioned"], 1);
    assert_eq!(v["total"], 2);
    let rows = v["benchmarks"].as_array().expect("benchmarks array");
    assert_eq!(rows.len(), 2);

    assert_eq!(rows[0]["name"], "demo/alloc");
    assert_eq!(rows[0]["provisioned"], true);
    assert_eq!(rows[0]["allocs_per_op"], 13.0);
    assert_eq!(rows[0]["bytes_per_op"], 1014.0);

    assert_eq!(rows[1]["name"], "silent/thing");
    assert_eq!(rows[1]["provisioned"], false);
    assert!(
        rows[1]["allocs_per_op"].is_null(),
        "a missing number must be null, not 0: {}",
        rows[1]
    );
    assert!(rows[1]["bytes_per_op"].is_null(), "{}", rows[1]);
}

#[test]
fn a_baseline_without_bench_targets_is_explained_and_not_failed() {
    let project = Project::new("t1", &["demo/alloc"], &[]);

    let out = project.cli(&["allocs"]);
    assert!(out.status.success(), "allocs: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no bench targets recorded"), "{text}");
    assert!(text.contains("re-run baseline"), "{text}");
}

#[test]
fn allocs_outside_a_run_branch_fails_before_running_anything() {
    let project = Project::new("t1", &["demo/alloc"], &[("fakebench", "fake")]);
    git(&project.root, &["checkout", "-q", "main"]);

    let out = project.cli(&["allocs"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("baseline --tag"), "{err}");
}
