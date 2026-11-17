//! The `status` command: shows where a run is without touching anything.
//!
//! It exists because `eval`'s own output cannot answer "where is this run,
//! and how do I stop it": `eval --json`'s contract is one JSON object and
//! nothing else, and the human watching may not even be reading the agent's
//! transcript. This is the command they run in their own shell, at any
//! moment, from any branch.
//!
//! **It opens nothing for writing.** Checking on a run must never be able to
//! change it — see `tests/cmd_status_stop.rs`, which snapshots the tree
//! either side of the call to prove it. Plain text by default; `--json`
//! prints the same facts as one object for a caller that would otherwise parse
//! the text. The agent's own stop signal is still `stop_requested` on
//! `eval --json`: `status --json` is for a human's tooling, not the loop.

use crate::args::Args;
use rustperf_core::{gitx, noise, requests, results, state};
use std::path::Path;

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(argv, &[("C", true), ("tag", true), ("json", false)]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };
    match status(&args) {
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

/// Resolves which run to describe: an explicit `--tag`, or — when the
/// current branch is itself a run branch — the tag it encodes. Refuses to
/// guess from anywhere else: a human standing on an unrelated branch with no
/// `--tag` has given this command nothing to infer from.
fn resolve_tag(root: &Path, tag_flag: Option<&str>) -> Result<String, String> {
    if let Some(t) = tag_flag {
        state::valid_tag(t)?;
        return Ok(t.to_string());
    }
    let branch = gitx::current_branch(root)?;
    state::tag_from_branch(&branch).ok_or_else(|| {
        format!(
            "current branch {branch:?} was not created by 'rustperf baseline --tag \
             <tag>' — pass --tag <tag>, or check out the run branch first"
        )
    })
}

fn status(args: &Args) -> Result<String, String> {
    let root = gitx::root(&args.dir())?;
    let tag = resolve_tag(&root, args.value("tag"))?;
    let dir = state::state_dir(&root, &tag)?;
    let baseline = state::Baseline::load(&dir.join(state::BASELINE_FILE))?;
    let run_branch = state::branch_name(&tag);
    let current_branch = gitx::current_branch(&root)?;

    if args.flag("json") {
        return status_json(&root, &dir, &tag, &baseline, &current_branch);
    }

    let mut out = String::new();
    out.push_str(&format!("{:<14} {tag}\n", "run tag"));
    if current_branch == run_branch {
        out.push_str(&format!("{:<14} {run_branch}  (checked out)\n", "branch"));
    } else {
        out.push_str(&format!(
            "{:<14} {run_branch}  (NOT checked out — you are on {current_branch})\n",
            "branch"
        ));
    }
    out.push_str(&format!(
        "{:<14} {}  (run started here)\n",
        "baseline", baseline.commit
    ));
    if baseline.measure_commit == baseline.commit {
        out.push_str(&format!(
            "{:<14} {}  (still the baseline — nothing kept yet)\n",
            "measuring vs", baseline.measure_commit
        ));
    } else {
        out.push_str(&format!(
            "{:<14} {}  (advanced past the baseline by earlier KEEPs)\n",
            "measuring vs", baseline.measure_commit
        ));
    }
    let worktree = dir.join(state::WORKTREE_NAME);
    if worktree.is_dir() {
        out.push_str(&format!("{:<14} {}\n", "worktree", worktree.display()));
    } else {
        // `baseline --tag {tag} --force` cannot fix this: its tag-collision
        // check is unconditional (see `cmd_baseline.rs`) and refuses to run
        // again for a tag that already has a baseline, --force or not —
        // --force only ever gates the separate results.tsv check. The
        // worktree is the only thing missing here (baseline.json, just
        // loaded above, is fine), so the real fix is recreating just it, by
        // hand, at the commit this run is actually measuring against.
        out.push_str(&format!(
            "{:<14} {}  (MISSING — recreate it by hand:\n{:<16}git -C {} worktree add \
             --detach -f {} {}\n{:<16}'baseline --tag {tag} --force' will NOT do this: a tag \
             that already has a baseline is always refused, --force or not)\n",
            "worktree",
            worktree.display(),
            "",
            root.display(),
            worktree.display(),
            baseline.measure_commit,
            "",
        ));
    }
    out.push_str(&experiments_line(&root));
    out.push_str(&noise_lines(&root, &dir));
    out.push_str(&requests_line(&dir));
    out.push_str(&stop_lines(&dir));
    Ok(out)
}

/// Counts of each status in `rows`: keep, discard, fail, crash.
fn status_counts(rows: &[results::Row]) -> [usize; 4] {
    let n = |s: &str| rows.iter().filter(|r| r.status == s).count();
    [n("KEEP"), n("DISCARD"), n("FAIL"), n("CRASH")]
}

/// `status --json`: the facts of the text form as one object. A `results.tsv`
/// that cannot be read is `experiments: null` plus `experiments_error`, never
/// a zero count, for the same reason the text form says "unavailable".
fn status_json(
    root: &Path,
    dir: &Path,
    tag: &str,
    baseline: &state::Baseline,
    current_branch: &str,
) -> Result<String, String> {
    let run_branch = state::branch_name(tag);
    let worktree = dir.join(state::WORKTREE_NAME);
    let mut v = serde_json::json!({
        "tag": tag,
        "branch": run_branch,
        "branch_checked_out": current_branch == run_branch,
        "current_branch": current_branch,
        "baseline_commit": baseline.commit,
        "measure_commit": baseline.measure_commit,
        "worktree": worktree.display().to_string(),
        "worktree_exists": worktree.is_dir(),
        "stop_requested": state::stop_requested(dir),
    });
    match results::load(&root.join(results::RESULTS_PATH)) {
        Ok(rows) => {
            let [keep, discard, fail, crash] = status_counts(&rows);
            v["experiments"] = serde_json::json!({
                "total": rows.len(),
                "keep": keep,
                "discard": discard,
                "fail": fail,
                "crash": crash,
            });
        }
        Err(e) => {
            v["experiments"] = serde_json::Value::Null;
            v["experiments_error"] = serde_json::json!(e);
        }
    }
    // Omitted entirely while there is no data: a run with no `noise.jsonl`
    // (or one too short to aggregate) has nothing to report, and an empty
    // array would read as "measured, found no noise".
    if let Ok(floors) = noise::floors(dir) {
        if !floors.is_empty() {
            v["noise"] = noise::rows_json(&floors, noise::min_effect_pct(root));
        }
    }
    // Same rule as `noise`: omitted while there is nothing pending, because
    // `"requests": 0` would read as "the queue was read and is empty" even
    // when the file could not be read at all.
    if let Ok(pending) = requests::load(dir) {
        if !pending.is_empty() {
            v["requests"] = serde_json::json!(pending.len());
        }
    }
    let mut text = serde_json::to_string_pretty(&v).map_err(|e| format!("encode status: {e}"))?;
    text.push('\n');
    Ok(text)
}

/// "How far into the loop is it", from `results.tsv` — the same file
/// `report` reads. A missing or unreadable file is reported as such rather
/// than as zero experiments: silently claiming a run has done nothing would
/// be worse than admitting the count is unavailable.
fn experiments_line(root: &Path) -> String {
    let rows = match results::load(&root.join(results::RESULTS_PATH)) {
        Ok(r) => r,
        Err(e) => return format!("{:<14} unavailable ({e})\n", "experiments"),
    };
    if rows.is_empty() {
        return format!("{:<14} none yet\n", "experiments");
    }
    let [keep, discard, fail, crash] = status_counts(&rows);
    format!(
        "{:<14} {} run  ({keep} keep, {discard} discard, {fail} fail, {crash} crash)  — next is \
         #{}\n",
        "experiments",
        rows.len(),
        rows.len() + 1
    )
}

/// The per-benchmark noise floors, once at least four experiments have
/// measured them. Read-only like every other line here: `noise::floors` only
/// reads the JSONL file, and a file that is not there is simply no floors.
///
/// An unreadable file is reported as such rather than swallowed, matching
/// `experiments_line`: claiming nothing is known about the noise would be
/// worse than admitting the file could not be read.
fn noise_lines(root: &Path, dir: &Path) -> String {
    match noise::floors(dir) {
        Ok(floors) => noise::status_block(&floors, noise::min_effect_pct(root)),
        Err(e) => format!("{:<14} unavailable ({e})\n", "noise floor"),
    }
}

/// How many requests to the human are waiting, read-only — the file is only
/// ever opened for reading here, like the noise history above.
///
/// An unreadable file is reported as such rather than hidden, matching
/// `experiments_line` and `noise_lines`: a human glancing at `status` must not
/// be told the queue is empty when it could not be read. No line at all is
/// printed while the queue is empty, which is the normal case.
fn requests_line(dir: &Path) -> String {
    match requests::load(dir) {
        Ok(pending) if pending.is_empty() => String::new(),
        Ok(pending) => format!(
            "{:<14} {} pending — see 'rustperf report'\n",
            "requests",
            pending.len()
        ),
        Err(e) => format!("{:<14} unavailable ({e})\n", "requests"),
    }
}

fn stop_lines(dir: &Path) -> String {
    if state::stop_requested(dir) {
        format!(
            "{:<14} requested — the agent will exit the loop at its next verdict\n\nto cancel \
             the stop:  rustperf stop --clear\nto stop sooner:      rustperf \
             stop --force\n",
            "stop"
        )
    } else {
        format!(
            "{:<14} not requested\n\nto stop after the current experiment:  rustperf \
             stop\nto stop now, abandoning it:            rustperf stop --force\n",
            "stop"
        )
    }
}
