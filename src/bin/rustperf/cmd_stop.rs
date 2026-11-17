//! The `stop` command: asks a run to end.
//!
//! Two speeds, and the difference is who decides when to stop:
//!
//! - Plain `stop` writes a request the AGENT reads at its next verdict. The
//!   experiment under way finishes and is scored, its KEEP or DISCARD is
//!   applied, and only then does the loop exit. Nothing is thrown away.
//! - `stop --force` additionally signals the running `eval` to abandon the
//!   experiment now, for when the human cannot wait for a long benchmark to
//!   finish. It reports what state that leaves the repository in and drops
//!   nothing itself — dropping an in-flight commit is the human's call.
//!
//! Both leave the repository on the run branch with every kept commit
//! intact.
//!
//! On unix, `--force` sends a real `SIGTERM` to a verified-LIVE `eval`
//! process (see `rustperf_core::state::eval_running`, which tells a live
//! claim from a pid file a crashed eval left behind) and waits `FORCE_GRACE`
//! for it to exit on its own — `eval`'s own signal handler sets
//! `state::CANCEL_REQUESTED`, which the pipeline and `Runner::cargo` check
//! between gates and between measurement rounds, so the process gets a
//! chance to record `ABORTED`/`stop_forced` and tear down its own benchmark
//! subprocess cleanly.
//!
//! If the claim is still held after that grace period, `--force` escalates
//! instead of giving up — see [`escalate`]. A polite `SIGTERM` only helps an
//! `eval` that reaches a poll tick; one wedged inside a syscall it cannot
//! leave (a stuck `cargo` waiting on a lock, a benchmark spinning in a
//! build script) would otherwise leave its benchmark running for as long as
//! the human leaves the machine alone. `eval`'s benchmark child is its own
//! process-group leader, and `Runner::cargo` records that pid in a temp file
//! for exactly this case, so the escalation SIGKILLs the whole recorded
//! group first (taking the grandchild with it), then SIGKILLs `eval` itself,
//! then clears the claim a `SIGKILL`ed process can no longer release. That
//! experiment is then gone, not scored: an escalated stop throws the
//! measurement away, which is what the human asked for.
//!
//! Windows has no such polite half — see [`terminate`] — so there `--force`
//! ends the process outright from the start, and the killed `eval` never
//! gets to record what it abandoned.

use crate::args::Args;
use rustperf_core::{gitx, state};
use std::path::Path;
use std::time::Duration;

/// How long `--force` waits for a signalled `eval` to release its claim
/// before escalating to `SIGKILL` — first its recorded benchmark process
/// group, then `eval` itself; see [`escalate`]. Long enough for a cooperative
/// `eval` to finish the abort it was asked for (record the abandoned
/// experiment, kill its own benchmark, drop the claim), short enough that a
/// human who cannot wait is not left watching a frozen process.
const FORCE_GRACE: Duration = Duration::from_secs(10);
const FORCE_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(
        argv,
        &[
            ("C", true),
            ("tag", true),
            ("clear", false),
            ("force", false),
        ],
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };
    if args.flag("clear") && args.flag("force") {
        eprintln!("rustperf stop: --clear and --force are opposites; pass one or neither");
        return rustperf_core::EXIT_USAGE;
    }
    match stop(&args) {
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

/// Resolves which run to signal, matching `status`'s resolution: an explicit
/// `--tag`, or the tag the current branch encodes.
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

fn stop(args: &Args) -> Result<String, String> {
    let root = gitx::root(&args.dir())?;
    let tag = resolve_tag(&root, args.value("tag"))?;
    let dir = state::state_dir(&root, &tag)?;

    if args.flag("clear") {
        state::clear_stop(&dir)?;
        return Ok(format!(
            "stop request cleared for run {tag:?}\nthe agent will keep experimenting; run \
             `rustperf stop` again to stop it\n"
        ));
    }

    state::request_stop(&dir)?;
    let mut out = format!(
        "stop requested for run {tag:?}\nthe current experiment will finish and be scored; the \
         agent will exit the loop after it\n"
    );
    if !args.flag("force") {
        out.push_str(
            "to cancel:      rustperf stop --clear\nto stop sooner: rustperf \
             stop --force\n",
        );
        return Ok(out);
    }

    out.push('\n');
    out.push_str(&force_signal(&dir));
    out.push_str(&repo_state(&root)?);
    Ok(out)
}

/// Signals a live `eval`, if one holds the run's claim, and reports what
/// happened. See [`terminate`] for the unix/Windows split and [`escalate`]
/// for what happens when the polite signal is not enough.
fn force_signal(dir: &Path) -> String {
    let live = match state::eval_running(dir) {
        Ok(p) => p,
        Err(e) => return format!("could not tell whether an eval is running: {e}\n"),
    };
    let Some(pid) = live else {
        // `eval_running` returning `None` can mean either that no pid file
        // exists at all, or that one does but nobody holds its lock — a
        // leftover from an `eval` that died without releasing its claim (a
        // `SIGKILL`, a panic, a crash). Clear it here, as the Go original
        // does, to keep the recorded state honest rather than leaving a
        // known-stale pid file sitting on disk indefinitely. Clearing a
        // file that is not there is not an error.
        let _ = state::clear_eval_pid(dir);
        return "no eval running — nothing to signal\n".to_string();
    };

    let mut out = format!("signalling eval (pid {pid})...\n");
    if !terminate(pid) {
        out.push_str("failed to signal the process; it may have already exited\n");
        return out;
    }

    if cfg!(unix) {
        out.push_str("sent SIGTERM; waiting for it to finish aborting...\n");
        if wait_for_release(dir, FORCE_GRACE) {
            out.push_str("eval exited\n");
        } else {
            out.push_str(&escalate(pid, dir));
        }
    } else {
        // TerminateProcess is immediate and gives the target no chance to
        // run its own cleanup, so the claim it held has to be cleared here
        // instead — `eval` never gets back to `EvalClaim::drop` to do it
        // itself.
        out.push_str("eval terminated (no graceful shutdown on this platform)\n");
        let _ = state::clear_eval_pid(dir);
    }
    out
}

/// What `--force` does when the polite half has run out of time: kill the
/// benchmark group `eval` recorded, kill `eval`, and clear the claim the
/// killed process can no longer release itself.
///
/// The order matters. The benchmark goes first, because the record naming
/// its group is this command's only handle on that tree, and it is used
/// before anything else can change state. `SIGKILL` is the only signal that
/// reaches a process stuck somewhere it cannot leave; it also cannot be
/// caught, so `eval` records nothing about the experiment — which is why the
/// report says so out loud rather than implying a verdict exists.
fn escalate(pid: u32, dir: &Path) -> String {
    let mut out = format!(
        "eval did not release its claim within {:?}; escalating to SIGKILL\n",
        FORCE_GRACE
    );

    match rustperf_core::runner::kill_recorded_tree(pid) {
        Ok(true) => out.push_str(
            "killed the benchmark process group eval was running — the benchmark is gone\n",
        ),
        Ok(false) => out.push_str(
            "no benchmark pid was recorded for that eval (it had no command running, or the \
             record was already cleared) — nothing extra to kill\n",
        ),
        Err(e) => out.push_str(&format!(
            "could not kill the recorded benchmark tree: {e}\n"
        )),
    }

    if kill_forced(pid) {
        out.push_str(&format!("killed eval (pid {pid}) with SIGKILL\n"));
    } else {
        out.push_str(&format!(
            "could not SIGKILL eval (pid {pid}); it may have exited between the check and the \
             signal\n"
        ));
    }

    // A process killed by SIGKILL runs no destructor, so `EvalClaim::drop` —
    // the only thing that unlocks and removes `eval.pid` — never runs. Left
    // in place, that file would keep claiming a live eval, and the next
    // `eval` would refuse to start because of it.
    match state::clear_eval_pid(dir) {
        Ok(()) => out.push_str("cleared the eval claim it could not release itself\n"),
        Err(e) => out.push_str(&format!("could not clear the eval claim: {e}\n")),
    }

    out.push_str("the experiment was killed, not scored: nothing about it was recorded\n");
    out
}

/// Polls until no live process holds the run's claim, or `grace` runs out.
///
/// Polls [`state::eval_running`] rather than the pid directly: a signalled
/// process that has died but not yet been reaped by its parent is a
/// ZOMBIE, and a liveness probe against the bare pid can still say yes for
/// one. `eval_running`'s claim (a kernel `flock` on unix, an exclusive
/// file-sharing handle on Windows) is released the moment the process
/// exits by any means, zombie or not, so it is the honest signal.
fn wait_for_release(dir: &Path, grace: Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        match state::eval_running(dir) {
            Ok(None) | Err(_) => return true,
            Ok(Some(_)) => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(FORCE_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn terminate(pid: u32) -> bool {
    // SAFETY: sends a real signal to a pid `state::eval_running` just
    // confirmed holds this run's live claim.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
}

/// The last resort on unix: `SIGKILL` cannot be caught, so the target gets no
/// chance to record anything or to release its claim — see [`escalate`],
/// which clears that claim afterward.
#[cfg(unix)]
fn kill_forced(pid: u32) -> bool {
    // SAFETY: sends a real signal to a pid `state::eval_running` confirmed
    // held this run's live claim, after the whole grace period for it to
    // exit on its own has already elapsed.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) == 0 }
}

/// Nothing to escalate to on platforms that have no graceful half: `--force`
/// there already ends the process outright on the first try (see
/// [`terminate`]).
#[cfg(not(unix))]
fn kill_forced(_pid: u32) -> bool {
    false
}

/// Windows has no `SIGTERM` a process can catch and act on mid-benchmark, so
/// there is no polite half here: this ends the process outright, and it
/// never gets to record what it abandoned or clean up its own claim (see
/// [`force_signal`], which clears it afterward instead).
///
/// UNVERIFIED on this platform: developed without access to a Windows
/// machine. Uses only `Win32_System_Threading` and `Win32_Foundation`,
/// already enabled in `Cargo.toml` for `procgroup_windows.rs`'s job-object
/// machinery — no new dependency or feature. CI covers Windows.
#[cfg(windows)]
fn terminate(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate(_pid: u32) -> bool {
    false
}

/// Tells the human what a forced stop leaves behind. Never changes the
/// repository: dropping a commit is the human's call, and an experiment
/// abandoned mid-flight may still be worth keeping by hand.
fn repo_state(root: &Path) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("repository state:\n");
    let branch = gitx::current_branch(root)?;
    out.push_str(&format!("  {:<10} {branch}\n", "branch"));
    if let Ok(commit) = gitx::head_commit(root) {
        out.push_str(&format!("  {:<10} {commit}\n", "HEAD"));
    }
    out.push_str(
        "\nif the agent had already committed the experiment it was running, that commit \
         carries no verdict. To drop it:  git reset --hard HEAD~1\nto resume this run later: \
         rustperf stop --clear\n",
    );
    Ok(out)
}
