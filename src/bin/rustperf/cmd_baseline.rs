use crate::args::Args;
use rustperf_core::{config, discover, freeze, gitx, results, runner, scope, state, stats};
use std::path::Path;

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(argv, &[("C", true), ("tag", true), ("force", false)]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };
    match baseline(&args) {
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

fn baseline(args: &Args) -> Result<String, String> {
    let tag = args
        .value("tag")
        .ok_or("--tag is required, e.g. --tag sep8")?;
    state::valid_tag(tag)?;

    let root = gitx::root(&args.dir())?;
    let cfg_path = root.join(config::CONFIG_PATH);
    if !cfg_path.exists() {
        return Err(format!(
            "no {} — run 'rustperf init' first",
            config::CONFIG_PATH
        ));
    }
    let cfg = config::Config::load(&cfg_path)?;

    // A baseline pinned against what is on disk rather than what is in git
    // would not be reproducible, so the tree must be committed first.
    if !gitx::is_clean(&root)? {
        return Err(
            "the working tree has uncommitted changes — commit them first:\n    \
             git add -A && git commit -m \"rustperf init\"\n\nA baseline pinned \
             against what is on disk rather than what is in git would not be reproducible."
                .to_string(),
        );
    }

    let dir = state::state_dir(&root, tag)?;
    if dir.join(state::BASELINE_FILE).exists() {
        return Err(format!(
            "tag {tag:?} already has a baseline at {} — pick a new tag, or delete that \
             directory to start over",
            dir.display()
        ));
    }
    // A results.tsv holding rows from an earlier run would make `report`
    // mix two runs' experiments into one cumulative figure.
    let results_path = root.join(results::RESULTS_PATH);
    if !results::load(&results_path)?.is_empty() && !args.flag("force") {
        return Err(format!(
            "{} already holds experiment rows from an earlier run — move it aside, or pass \
             --force to start a new run against it",
            results::RESULTS_PATH
        ));
    }

    let branch = state::branch_name(tag);
    if gitx::branch_exists(&root, &branch)? {
        return Err(format!("branch {branch} already exists — pick a new tag"));
    }

    // Nothing above this line has mutated the repository or written into the
    // state directory, so nothing above needs a rollback. Everything below
    // does, and `baseline`'s whole contract is doing it exactly once: a
    // failure partway through must not leave wreckage that a retry misreads
    // as a naming collision (the branch or the state directory already
    // existing) rather than the real problem.
    let original_branch = gitx::current_branch(&root)?;
    gitx::create_and_checkout_branch(&root, &branch)?;

    freeze_and_pin(&root, &dir, &cfg, &cfg_path, tag, &branch)
        .map_err(|e| rollback(&root, &dir, &branch, &original_branch, e))
}

/// Everything `baseline` does once the run branch exists: freezing the
/// tests that live in their own files, hashing the inline tests that
/// cannot be, pinning the measurement worktree, and recording the baseline.
///
/// Split out from [`baseline`] so its errors can be rolled back as a single
/// unit by the caller — this function itself never cleans up after its own
/// failure.
fn freeze_and_pin(
    root: &Path,
    dir: &Path,
    cfg: &config::Config,
    cfg_path: &Path,
    tag: &str,
    branch: &str,
) -> Result<String, String> {
    let commit = gitx::head_commit(root)?;

    // Freeze the tests that live in their own files.
    let frozen = discover::frozen_test_files(root, &cfg.unfreeze)?;
    let store = dir.join(freeze::STORE_DIR);
    let mut manifest = freeze::snapshot(root, &store, &frozen).map_err(|e| e.to_string())?;

    // Hash the tests that cannot be: inline #[cfg(test)] modules and doctests
    // inside the source files the agent is allowed to edit. `inline_hashes_at`
    // (not the pure-string `inline_hashes`) is what lets a `#[cfg(test)] mod
    // tests;` declared without a body resolve to its own file.
    let matcher = scope::Matcher::new(&cfg.scope);
    for rel in discover::in_scope_sources(root, &matcher)? {
        let hashes = freeze::inline_hashes_at(root, &rel).map_err(|e| e.to_string())?;
        manifest.inline.insert(rel.clone(), hashes);
    }
    manifest.save(&dir.join(freeze::MANIFEST_PATH))?;

    // Hash every locked file present on disk, independent of git: `eval`
    // gate 2 stats these paths directly, which is what catches a locked file
    // that a repo's own (or the agent's own) `.gitignore` hides from
    // `git diff`/`git status`.
    let mut locked_files = std::collections::BTreeMap::new();
    for rel in discover::locked_files(root)? {
        locked_files.insert(rel.clone(), freeze::sha256_file(&root.join(&rel))?);
    }

    // Hash gitignored sources outside the scope: git cannot report edits to
    // them, so `eval` compares hashes of what is on disk instead.
    let mut ignored_sources = std::collections::BTreeMap::new();
    for rel in discover::ignored_sources_outside_scope(root, &matcher)? {
        ignored_sources.insert(rel.clone(), freeze::sha256_file(&root.join(&rel))?);
    }

    // Pin the measurement worktree.
    let worktree = dir.join(state::WORKTREE_NAME);
    gitx::add_worktree(root, &worktree, &commit)?;

    // Warm the worktree with the release build `eval` would otherwise pay
    // for on its very first measurement. Environment trouble here must not
    // sink `baseline`: `eval` reports a build that does not compile far more
    // precisely (and without rolling back a perfectly good baseline), so a
    // failed warm-up only adds a warning to the output below.
    let warm = warm_note(&warm_worktree(&worktree, cfg)?);

    let record = state::Baseline {
        tag: tag.to_string(),
        branch: branch.to_string(),
        commit: commit.clone(),
        measure_commit: commit.clone(),
        created_at: now_utc(),
        benchmarks: cfg.benchmarks.clone(),
        bench_targets: cfg.bench_targets.clone(),
        config_sha256: freeze::sha256_file(cfg_path)?,
        locked_files,
        ignored_sources: Some(ignored_sources),
    };
    record.save(&dir.join(state::BASELINE_FILE))?;

    let warning = keep_reachability_warning(cfg.count, record.benchmarks.len())
        .map(|w| format!("\nwarning        {w}\n"))
        .unwrap_or_default();

    Ok(format!(
        "run tag        {tag}\nbranch         {branch}  (checked out)\nbaseline       \
         {commit}\nfrozen         {} test file(s), {} source file(s) hashed for inline \
         tests\nworktree       {}{warm}\nbenchmarks     {}\n{warning}\nStart your agent in this \
         repository and point it at program.md.\n",
        manifest.files.len(),
        manifest.inline.len(),
        worktree.display(),
        record.benchmarks.join(", ")
    ))
}

/// Lines of a failed warm-up build's output `baseline` repeats. The real
/// cause is at the bottom of cargo's diagnostics; a whole failed build log is
/// not something anyone wants echoed back at them.
const WARM_TAIL_LINES: usize = 5;

/// The 15 columns `baseline`'s `label value` lines spend on their label, so
/// the lines of a multi-line warning start under its body rather than under
/// its label.
const WARNING_INDENT: &str = "               ";

/// Runs, inside the freshly pinned worktree, the release build `eval` would
/// otherwise pay for on its first measurement.
///
/// A warm-up can fail in three ways — cargo exits non-zero, the config's
/// timeout expires, or cargo cannot be started at all — and [`warm_note`]
/// reports all three identically. The last one has no streams of its own, so
/// it is folded into an [`runner::Output`] with the error where cargo's
/// diagnostics would have been, keeping the formatter total and testable
/// without ever invoking cargo.
fn warm_worktree(worktree: &Path, cfg: &config::Config) -> Result<runner::Output, String> {
    const ARGS: [&str; 4] = ["build", "--release", "--all-targets", "--workspace"];
    let r = runner::Runner::new(worktree, cfg.timeout_duration()?);
    match r.cargo(&ARGS, None) {
        Ok(out) => Ok(out),
        Err(e) => Ok(runner::Output {
            args: ARGS.iter().map(|a| a.to_string()).collect(),
            stdout: Vec::new(),
            stderr: e.into_bytes(),
            // `cargo` reports -1 for a child that never produced an exit
            // status; the formatter treats any negative code as such.
            exit_code: -1,
            timed_out: false,
            cancelled: false,
            duration: std::time::Duration::ZERO,
        }),
    }
}

/// What the `worktree` line says about the warm-up build: `  (warm: release
/// build ok)` appended to the line itself on success, or — after any failure
/// — a `warning` block appended below it, naming the cold first `eval` and
/// the last few lines of the build's own output.
///
/// A failed warm-up is deliberately not an error: `baseline` must not refuse
/// to pin a run because the build environment is broken, and `eval` already
/// reports a build that does not compile. The returned text is written
/// verbatim after the worktree path, so a failure block starts with a newline
/// (separating it from that path) and ends with one (separating it from the
/// `benchmarks` line that follows).
fn warm_note(out: &runner::Output) -> String {
    if out.ok() && !out.cancelled {
        return "  (warm: release build ok)".to_string();
    }
    let how = if out.cancelled {
        "was cancelled".to_string()
    } else if out.timed_out {
        format!("timed out after {}s", out.duration.as_secs())
    } else if out.exit_code < 0 {
        "did not exit cleanly".to_string()
    } else {
        format!("exited with status {}", out.exit_code)
    };
    let mut text = format!(
        "\nwarning        the baseline worktree was not warmed: cargo build --release \
         --all-targets --workspace {how} — the first eval will pay for a cold release build\n"
    );
    for line in out.tail(WARM_TAIL_LINES).lines() {
        // Blank diagnostic lines (cargo emits them between notes) must not
        // turn into 15 columns of trailing whitespace.
        let line = line.trim_end();
        if line.is_empty() {
            text.push('\n');
        } else {
            text.push_str(WARNING_INDENT);
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

/// Raw significance level `eval` tests every benchmark at before correcting.
const ALPHA: f64 = 0.05;

/// Says so when this run could never produce a KEEP.
///
/// `eval` corrects the threshold to `alpha / k` for `k` benchmarks, and the
/// U test cannot return a p-value below a floor set by the rounds per side.
/// `count` and `k` are fixed by the baseline's config hash, so the run would
/// discard every experiment for the whole night; the verdict already warns
/// about it, but only after a measurement was spent to find out.
fn keep_reachability_warning(count: usize, k: usize) -> Option<String> {
    let k = k.max(1);
    let corrected = ALPHA / k as f64;
    if stats::min_achievable_p(count, count) < corrected {
        return None;
    }
    let advice = match stats::count_for_alpha(corrected) {
        Some(need) => format!("raise count to at least {need} in {}", config::CONFIG_PATH),
        None => "measure fewer benchmarks (list fewer in the config)".to_string(),
    };
    Some(format!(
        "no KEEP can be reached: {k} benchmark(s) correct the threshold to {corrected:.5}, \
         but count = {count} cannot produce a p-value below {:.5}. {advice}, then start a \
         new run.",
        stats::min_achievable_p(count, count)
    ))
}

/// Undoes everything [`freeze_and_pin`] may have partially done: checks the
/// repository back out onto `original_branch`, force-deletes the abandoned
/// run branch, and removes the partial state directory. Returns
/// `original_err`, with any trouble the rollback itself hits appended as
/// additional context.
///
/// The rollback's own failure must never REPLACE `original_err` — whatever
/// actually broke the run is what the person retrying needs to see first,
/// not a secondary complaint about cleanup.
fn rollback(
    root: &Path,
    dir: &Path,
    branch: &str,
    original_branch: &str,
    original_err: String,
) -> String {
    let mut msg = original_err;
    if let Err(e) = gitx::checkout_branch(root, original_branch) {
        msg.push_str(&format!(
            "\n\nadditionally, failed to check the repository back out onto {original_branch:?} \
             after this failure: {e}\n    to recover by hand: git checkout {original_branch}"
        ));
    }
    if let Err(e) = gitx::delete_branch(root, branch) {
        msg.push_str(&format!(
            "\n\nadditionally, failed to delete the abandoned run branch {branch:?}: {e}\n    \
             to recover by hand: git branch -D {branch}"
        ));
    }
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            msg.push_str(&format!(
                "\n\nadditionally, failed to remove the partial state directory {}: {e}\n    \
                 to recover by hand: rm -rf {}",
                dir.display(),
                dir.display()
            ));
        }
    }
    msg
}

/// An RFC3339 UTC timestamp, without pulling in a date library for one field.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days, Howard Hinnant's algorithm.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reachable_threshold_gives_no_warning() {
        assert_eq!(keep_reachability_warning(10, 1), None);
    }

    #[test]
    fn too_many_benchmarks_for_the_count_is_reported_with_a_fix() {
        let w = keep_reachability_warning(5, 30).expect("unreachable");
        assert!(w.contains("30 benchmark"), "{w}");
        assert!(w.contains("no KEEP can be reached"), "{w}");
    }

    fn build(stderr: &str, exit_code: i32, timed_out: bool) -> runner::Output {
        runner::Output {
            args: vec!["build".to_string(), "--release".to_string()],
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            exit_code,
            timed_out,
            cancelled: false,
            duration: std::time::Duration::from_secs(7),
        }
    }

    #[test]
    fn a_warm_worktree_says_so_on_the_worktree_line() {
        assert_eq!(
            warm_note(&build("", 0, false)),
            "  (warm: release build ok)"
        );
    }

    #[test]
    fn a_failed_warm_build_warns_with_the_stderr_tail() {
        let note = warm_note(&build(
            "   Compiling demo v0.1.0\nerror[E0432]: unresolved import `nope`\n\
             error: could not compile `demo` (lib) due to 1 previous error\n",
            101,
            false,
        ));
        assert!(note.starts_with("\nwarning        "), "{note}");
        assert!(
            note.contains("first eval will pay for a cold release build"),
            "{note}"
        );
        assert!(note.contains("status 101"), "{note}");
        assert!(note.contains("unresolved import `nope`"), "{note}");
        // The block starts on its own line, after the worktree path, and ends
        // with one so the following `benchmarks` line stays separated.
        assert!(note.ends_with('\n'), "{note}");
    }

    #[test]
    fn a_timed_out_warm_build_is_a_warning_not_an_error() {
        let note = warm_note(&build("", -1, true));
        assert!(note.contains("timed out after 7s"), "{note}");
        assert!(
            note.contains("first eval will pay for a cold release build"),
            "{note}"
        );
    }

    #[test]
    fn a_warm_build_that_could_not_be_started_is_reported_the_same_way() {
        let note = warm_note(&build(
            "run cargo build: No such file or directory",
            -1,
            false,
        ));
        assert!(note.contains("cold release build"), "{note}");
        assert!(note.contains("No such file or directory"), "{note}");
    }

    #[test]
    fn only_the_tail_of_a_failed_build_is_repeated() {
        // Ten stderr lines, one of them blank like cargo's own diagnostics,
        // of which only the last five belong in the warning.
        let mut stderr = String::new();
        for i in 1..=9 {
            if i == 7 {
                stderr.push('\n');
            }
            stderr.push_str(&format!("error {i}\n"));
        }
        let note = warm_note(&build(&stderr, 101, false));
        assert!(!note.contains("error 5"), "{note}");
        assert!(note.contains("error 9"), "{note}");
        // The blank diagnostic line is echoed as an empty line, not as a run
        // of trailing whitespace.
        assert!(note.lines().all(|l| !l.ends_with(' ')), "{note:?}");
    }
}
