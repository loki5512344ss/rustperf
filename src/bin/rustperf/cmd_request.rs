//! The `request` command: records that an experiment needs a human.
//!
//! The agent cannot introduce a dependency (`Cargo.toml`/`Cargo.lock` are
//! frozen), a global allocator, compiler flags or an `unsafe` block — the
//! harness gates each of those, and the gates are the point. What it can do
//! is say so in one place a human reads, instead of spending an experiment on
//! a change that will FAIL by construction.
//!
//! **Writing a request lifts no gate.** Nothing in the pipeline reads the
//! queue, so this command cannot change a verdict. Provisioning stays the
//! human's call: before the next `baseline`, or — for a single `unsafe` block
//! — by a human setting `allow_unsafe: true` in `.rustperf/config.yaml`
//! (human-only).
//!
//! The queue is append-only and lives in the run's own state directory, so a
//! request names the run it belongs to: resolving the tag is exactly what
//! `status`/`stop` do, and for the same reason — a note about "the next
//! experiment" is meaningless without a run.

use crate::args::Args;
use rustperf_core::{gitx, requests, results, state};
use std::path::PathBuf;

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(argv, &[("C", true), ("kind", true), ("list", false)]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };

    if args.flag("list") {
        if !args.positional.is_empty() {
            eprintln!("rustperf request: --list takes no <text>\n\n{}", usage());
            return rustperf_core::EXIT_USAGE;
        }
        return match list(&args) {
            Ok(msg) => {
                print!("{msg}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                2
            }
        };
    }

    // Usage mistakes are refused before anything is read from the repository:
    // a bad kind or an empty text cannot depend on which branch is checked
    // out, and exit 64 is what a caller (or a script) expects for them.
    let kind = match kind_flag(&args) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}\n\n{}", usage());
            return rustperf_core::EXIT_USAGE;
        }
    };
    let text = match text_arg(&args.positional) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}\n\n{}", usage());
            return rustperf_core::EXIT_USAGE;
        }
    };
    match record(&args, &kind, &text) {
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

fn usage() -> &'static str {
    "usage: rustperf request \"<text>\" [--kind K] | rustperf request --list\n\
     kinds: allocator, dependency, unsafe, flags, other (default: other)"
}

/// `--kind`, defaulting to `other`. An unknown kind is refused rather than
/// stored: the kind is what a human triages the queue by.
fn kind_flag(args: &Args) -> Result<String, String> {
    let kind = args.value("kind").unwrap_or("other");
    if requests::KINDS.contains(&kind) {
        Ok(kind.to_string())
    } else {
        Err(format!(
            "rustperf request: unknown --kind {kind:?}; allowed kinds: {}",
            requests::KINDS.join(", ")
        ))
    }
}

/// The `<text>` positional, sanitized. Unquoted words are joined with single
/// spaces so `rustperf request need an arena` says what it looks like; the
/// file still gets one trimmed, single-line record.
fn text_arg(positional: &[String]) -> Result<String, String> {
    let text = requests::sanitize(&positional.join(" "));
    if text.is_empty() {
        return Err(
            "rustperf request: needs a non-empty <text> — what should be provisioned, and why"
                .to_string(),
        );
    }
    Ok(text)
}

/// Resolves the run the way `status` does, but only from the current branch:
/// a request belongs to the run being worked on right now, and an explicit
/// `--tag` would let one be filed against a run nobody is touching.
fn run_state_dir(args: &Args) -> Result<(PathBuf, PathBuf), String> {
    let root = gitx::root(&args.dir())?;
    let branch = gitx::current_branch(&root)?;
    let tag = state::tag_from_branch(&branch).ok_or_else(|| {
        format!(
            "current branch {branch:?} was not created by 'rustperf baseline --tag <tag>' — a \
             request belongs to one run, so check out the run branch first"
        )
    })?;
    let dir = state::state_dir(&root, &tag)?;
    Ok((root, dir))
}

fn record(args: &Args, kind: &str, text: &str) -> Result<String, String> {
    let (root, dir) = run_state_dir(args)?;
    // `experiment` is the number the next `eval` would get, or 0 when the log
    // cannot be read: a request exists so that a human can act, and must
    // never fail because of a file it only needs for context.
    let experiment = results::load(&root.join(results::RESULTS_PATH))
        .map(|rows| rows.len() + 1)
        .unwrap_or(0);
    let request = requests::Request {
        kind: kind.to_string(),
        text: text.to_string(),
        commit: gitx::head_commit(&root).unwrap_or_default(),
        experiment,
    };
    requests::append(&dir, &request)?;
    Ok(format!(
        "request recorded: kind {kind}\n  {}\n\nthis does not lift any gate: provisioning (the \
         dependency, allocator, flags or unsafe block) is the human's call before the next \
         `rustperf baseline` — or, for a single unsafe block, by a human setting \
         `allow_unsafe: true` in `.rustperf/config.yaml`. Do not build an experiment on it \
         until then.\n",
        dir.join(requests::REQUESTS_FILE).display()
    ))
}

fn list(args: &Args) -> Result<String, String> {
    let (_, dir) = run_state_dir(args)?;
    let stored = requests::load(&dir)?;
    if stored.is_empty() {
        return Ok("no requests yet\n".to_string());
    }
    let mut out = format!("{} request(s) for a human:\n", stored.len());
    for request in &stored {
        out.push_str(&format!("  {}\n", requests::describe(request)));
    }
    Ok(out)
}
