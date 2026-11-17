//! Puts each command in its own process group, so killing it kills the whole
//! tree it started.
//!
//! `cargo bench` runs the compiled benchmark as a grandchild. Killing only the
//! direct child would leave that benchmark running — burning CPU and
//! corrupting every later measurement on the machine.
//!
//! Inside one process [`kill_tree`] is enough for that: the child is its own
//! group leader, so signalling the negative pid reaches every descendant it
//! spawned. Across processes it is not, and `rustperf stop --force` runs in a
//! different process from the `eval` it is stopping. So each `Runner::cargo`
//! also records the pid of the child it just spawned in
//! `<temp_dir>/rustperf-eval-<eval_pid>.child` — one decimal pid and a
//! newline, where `<eval_pid>` is the harness's own `std::process::id()` —
//! and [`kill_recorded_tree`] reads that file back, letting a `stop --force`
//! whose `eval` is wedged SIGKILL the benchmark's group from outside.
//!
//! Lifetime of that file. It is written immediately after `spawn` and removed
//! on every exit path of `Runner::cargo` — a normal wait, a timeout, a
//! cancel, or a `try_wait` error — so it names a running command or nothing.
//! [`kill_recorded_tree`] removes it as soon as it has been used, whether or
//! not the signal succeeded.
//!
//! The file can outlive the command in one case:
//!
//! - The `eval` process was killed hard (a `SIGKILL` from outside, the OOM
//!   killer, a crash, a power cut) and ran none of its cleanup, so the file
//!   is stale: it names a group that is probably gone, or a pid number the
//!   OS has since reused. It is only ever read for an `eval` that a live
//!   claim (the `flock` in `state::eval_running`) says is still running, and
//!   a group that no longer exists fails with `ESRCH`, which is reported as
//!   "nothing to kill" rather than as an error. The residual window is a new
//!   `eval` that has reclaimed a recycled pid and not yet spawned anything:
//!   until its first spawn, a stale record from the previous owner of that
//!   pid is still on disk.
//!
//! Nothing on this platform prevents that leftover, but the next spawn in any
//! `eval` process holding that pid overwrites it (create/truncate), so a stale
//! record never survives the next command even when it survives the crash.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Makes the child the leader of a new process group.
pub fn configure(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setpgid is async-signal-safe and is exactly what pre_exec is
    // for; it touches no memory shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Kills the child and everything it started, by signalling the group.
pub fn kill_tree(child: &mut Child) {
    let pid = child.id() as i32;
    // Negative pid addresses the whole group. SIGKILL rather than SIGTERM: by
    // the time this runs the command has already overrun its timeout.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
}

/// Where an `eval` process records the pid of the command it is running.
///
/// Keyed by the harness pid rather than by tag or repository, because it is
/// written and read from opposite sides of a process boundary that knows
/// only the `eval` pid it captured in `eval.pid` — see `state::eval_pid`. In
/// the temp directory rather than the run's state directory so the two
/// processes find it without agreeing on `RUSTPERF_STATE_HOME`, and so it is
/// not part of the run state a human might copy, migrate or delete while a
/// command is running.
pub fn child_file(eval_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("rustperf-eval-{eval_pid}.child"))
}

/// Reads the decimal pid a child record holds, refusing anything else.
///
/// `0` and `1` are refused on purpose: `kill(-1, ...)` addresses EVERY
/// process the caller may signal and `kill(0, ...)` the caller's own group,
/// so a corrupt or truncated record containing one of those must never reach
/// libc. A recorded cargo child is always at least pid 2.
pub fn parse_child_pid(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match trimmed.parse::<u32>().ok()? {
        pid if pid > 1 => Some(pid),
        _ => None,
    }
}

/// Records the pid of the command this process just spawned, replacing any
/// previous record — one command runs at a time, so previous content is a
/// stale leftover, not data to preserve.
pub fn record_child(pid: u32) -> Result<(), String> {
    write_child_pid(&child_file(std::process::id()), pid)
}

/// Removes this process's child record. Removing one that is not there is
/// not an error, so every exit path can call this unconditionally.
pub fn clear_child() -> Result<(), String> {
    remove_child_file(&child_file(std::process::id()))
}

/// SIGKILLs the process group the command of `eval_pid` was leading, then
/// removes the record. `Ok(true)` means a group was signalled, `Ok(false)`
/// that there was no record or the group was already gone.
///
/// Used by `stop --force` after its grace period, precisely because a
/// `SIGKILL`ed `eval` cannot tear its own benchmark child down: the child is
/// a group leader, so the negative pid takes the whole tree.
pub fn kill_recorded_tree(eval_pid: u32) -> Result<bool, String> {
    kill_tree_at(&child_file(eval_pid))
}

/// Lower-level half of [`record_child`], taking the path explicitly so tests
/// can round-trip a record in a temp directory instead of overwriting the
/// process-wide one a concurrently running test may be observing.
fn write_child_pid(path: &Path, pid: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        // The path is predictable and lives in a directory other users may
        // write to, so refuse to follow a symlink planted there: at worst
        // recording fails and the `stop --force` escalation is unavailable,
        // which is a warning, never a run failure.
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("record child pid in {}: {e}", path.display()))?;
    writeln!(file, "{pid}").map_err(|e| format!("record child pid in {}: {e}", path.display()))
}

/// Lower-level half of [`clear_child`]; see [`write_child_pid`] on the split.
fn remove_child_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear child record {}: {e}", path.display())),
    }
}

/// Lower-level half of [`kill_recorded_tree`], taking the path explicitly so
/// tests can exercise the reading paths without ever aiming a signal at a
/// pid that exists.
fn kill_tree_at(path: &Path) -> Result<bool, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("read child record {}: {e}", path.display())),
    };
    let Some(child) = parse_child_pid(&raw) else {
        let _ = std::fs::remove_file(path);
        return Err(format!(
            "{} does not hold a decimal pid (got {:?})",
            path.display(),
            raw.trim()
        ));
    };
    let Ok(leader) = i32::try_from(child) else {
        let _ = std::fs::remove_file(path);
        return Err(format!("recorded pid {child} does not fit a unix pid"));
    };
    // Negative pid addresses the whole group the recorded child leads, which
    // is what takes the benchmark grandchild with it. `leader` is at least 2
    // (see `parse_child_pid`), so the negation cannot overflow.
    // SAFETY: no pointer is passed and no memory is touched; the pid came
    // from a record this process's own `Runner::cargo` wrote, and the signal
    // is only sent for an `eval` whose live claim `stop --force` verified.
    let signalled = unsafe { libc::kill(-leader, libc::SIGKILL) } == 0;
    let failure = if signalled {
        None
    } else {
        Some(std::io::Error::last_os_error())
    };
    // Removed either way: it names the command that was just signalled, and
    // keeping it can only aim a later SIGKILL at a pid this run no longer
    // owns.
    let _ = std::fs::remove_file(path);
    match failure {
        None => Ok(true),
        // ESRCH: the group is already gone — the command had exited, or the
        // record is a leftover from a crash. Nothing was killed, which is
        // not a failure.
        Some(e) if e.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        Some(e) => Err(format!("kill process group {child}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nothing here signals a live pid: the reading half of the protocol is
    // exercised through `kill_tree_at` with a private path, so a test can
    // never aim a SIGKILL at whatever process the CI machine happens to run.
    #[test]
    fn the_child_record_path_is_in_the_temp_dir_and_named_for_the_eval_pid() {
        let p = child_file(4242);
        assert_eq!(p.parent(), Some(std::env::temp_dir().as_path()));
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some("rustperf-eval-4242.child")
        );
        assert_ne!(child_file(4242), child_file(4243), "one file per eval");
    }

    #[test]
    fn only_a_decimal_pid_of_a_real_process_group_leader_is_accepted() {
        assert_eq!(parse_child_pid("12345\n"), Some(12345));
        assert_eq!(parse_child_pid("  7  "), Some(7));
        // 0 is not a pid at all, and 1 would turn `kill(-1, SIGKILL)` into
        // "signal every process this user may signal" — both are refused
        // here rather than passed on to libc.
        for bad in [
            "",
            " ",
            "\n",
            "abc",
            "12a",
            "-5",
            "0",
            "1",
            "1 2",
            "4294967296",
        ] {
            assert_eq!(parse_child_pid(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_missing_record_reports_that_there_was_nothing_to_kill() {
        let d = tempfile::tempdir().expect("tempdir");
        assert!(!kill_tree_at(&d.path().join("absent.child")).expect("must not error"));
    }

    // The public entry point, not just its path-taking half: an eval with no
    // record (it had no command running, or already cleared it) must read as
    // "nothing to kill", never as an error. Nothing is signalled here — that
    // is the point of the missing file.
    #[test]
    fn a_pid_with_no_record_reports_that_there_was_nothing_to_kill() {
        let eval_pid = 3_999_999;
        assert!(
            !child_file(eval_pid).exists(),
            "precondition: no record may exist for {eval_pid}"
        );
        assert!(!kill_recorded_tree(eval_pid).expect("must not error"));
    }

    #[test]
    fn an_unusable_record_is_reported_and_removed() {
        let d = tempfile::tempdir().expect("tempdir");
        let path = d.path().join("junk.child");
        std::fs::write(&path, "this is not a pid\n").expect("write");
        let err = kill_tree_at(&path).expect_err("junk must be reported");
        assert!(err.contains("pid"), "{err}");
        assert!(!path.exists(), "an unusable record must not be left behind");
    }

    #[test]
    fn recording_and_clearing_a_child_pid_round_trips_through_the_file() {
        let d = tempfile::tempdir().expect("tempdir");
        let path = d.path().join("child");
        write_child_pid(&path, 4321).expect("record");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert_eq!(parse_child_pid(&raw), Some(4321));
        // The next spawn overwrites the record instead of appending to it.
        write_child_pid(&path, 4322).expect("overwrite");
        assert_eq!(
            parse_child_pid(&std::fs::read_to_string(&path).expect("read")),
            Some(4322)
        );
        remove_child_file(&path).expect("clear");
        assert!(!path.exists());
        // Clearing a record that is not there is not an error.
        remove_child_file(&path).expect("clear absent");
    }
}
