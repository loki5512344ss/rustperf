//! The harness-owned queue of "requests to the human".
//!
//! Some useful optimizations cannot be introduced by the agent at all: a new
//! dependency (`Cargo.toml`/`Cargo.lock` are frozen), a global allocator or an
//! arena (`unsafe impl Allocator`/`GlobalAlloc` trips the unsafe gate),
//! compiler flags, profile settings. There is no way around those gates, and
//! deliberately so — a harness whose gates the agent can talk itself past is
//! no harness. What was missing was the other half: an explicit way for the
//! agent to RECORD "the next experiment needs an arena/dependency/unsafe
//! block" instead of spending a whole experiment on a guaranteed FAIL.
//!
//! **A request lifts no gate.** It is a note to a human, nothing more: the
//! pipeline never reads this file, so writing one cannot change a verdict.
//! Provisioning stays the human's call, made before the next `baseline` (or,
//! for a single unsafe block, by a human setting `allow_unsafe: true`).
//!
//! The file is append-only JSONL inside the run's out-of-tree state directory,
//! next to `noise.jsonl`, and is advisory in exactly the same way: a missing
//! file is an empty queue, and a malformed line is skipped rather than failing
//! the read. Losing the whole queue to one bad byte would make `status` and
//! `report` refuse to describe a run over a note nobody has acted on yet.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// The file, inside a run's state directory, holding one request per line.
pub const REQUESTS_FILE: &str = "requests.jsonl";

/// The kinds a request may declare, and the order the CLI lists them in.
pub const KINDS: &[&str] = &["allocator", "dependency", "unsafe", "flags", "other"];

/// The longest text stored verbatim, in **characters**. A human reads this
/// file; 500 characters is generous for "what I need and why", and the cap
/// keeps one pasted transcript from making every future read expensive.
pub const MAX_TEXT_LEN: usize = 500;

/// One request to the human.
///
/// A `granted` flag is absent on purpose: whether a request was granted is recorded
/// by the human provisioning the thing (or not), not by a flag the agent can
/// set itself. `experiment` is the experiment number the request was written
/// before — the log's row count plus one — so a human reading the queue later
/// can tell which attempt it was blocking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub kind: String,
    pub text: String,
    pub commit: String,
    pub experiment: usize,
}

/// Flattens tabs and newlines, trims, and caps at [`MAX_TEXT_LEN`] characters,
/// appending `...` when it had to cut.
///
/// Counting characters rather than bytes means a multi-byte character is
/// never split in half into invalid UTF-8. The same treatment as an
/// experiment description (see `results::append`), for the same reason: the
/// file is one line per record and is read by a human.
pub fn sanitize(text: &str) -> String {
    let flat = text.replace(['\t', '\r', '\n'], " ");
    let flat = flat.trim();
    if flat.chars().count() <= MAX_TEXT_LEN {
        return flat.to_string();
    }
    let kept: String = flat.chars().take(MAX_TEXT_LEN).collect();
    format!("{kept}...")
}

/// Appends one request, creating the state directory and file when needed.
///
/// No `sync_all`: unlike a result row, a request is not a measurement. Losing
/// the tail of this file to a power cut costs one note, and the agent can
/// write it again; paying for an fsync on every note is not worth that.
pub fn append(state_dir: &Path, request: &Request) -> Result<(), String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("create {}: {e}", state_dir.display()))?;
    let path = state_dir.join(REQUESTS_FILE);
    let record = Request {
        kind: request.kind.clone(),
        text: sanitize(&request.text),
        commit: request.commit.clone(),
        experiment: request.experiment,
    };
    let mut line = serde_json::to_string(&record).map_err(|e| format!("encode request: {e}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    // One `write_all` of a whole line: an appended record is never half of
    // one JSON object.
    file.write_all(line.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// Reads every stored request, oldest first.
///
/// A missing file is an empty queue, not an error. Malformed lines are
/// skipped: this file is advisory, and a queue nobody has acted on yet must
/// not be able to stop `status` or `report` from describing the run.
pub fn load(state_dir: &Path) -> Result<Vec<Request>, String> {
    let path = state_dir.join(REQUESTS_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(request) = serde_json::from_str::<Request>(line) {
            out.push(request);
        }
    }
    Ok(out)
}

/// One request as a single line a human reads: `[kind] text (experiment N,
/// commit abc1234)`. No indentation — `report` and `request --list` own their
/// layout.
///
/// The commit half is dropped when it is empty: a request written while git
/// could not answer has no commit to name, and `commit ` with nothing after
/// it would read as a bug rather than as missing context.
pub fn describe(request: &Request) -> String {
    let mut out = format!("[{}] {}", request.kind, request.text);
    if request.commit.is_empty() {
        out.push_str(&format!(" (experiment {})", request.experiment));
    } else {
        out.push_str(&format!(
            " (experiment {}, commit {})",
            request.experiment, request.commit
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: &str, text: &str, experiment: usize) -> Request {
        Request {
            kind: kind.to_string(),
            text: text.to_string(),
            commit: "abc1234".to_string(),
            experiment,
        }
    }

    #[test]
    fn requests_round_trip_in_order() {
        let d = tempfile::tempdir().unwrap();
        append(
            d.path(),
            &request("allocator", "bumpalo for the parser arena", 1),
        )
        .unwrap();
        append(
            d.path(),
            &request("dependency", "smallvec for the token buffer", 2),
        )
        .unwrap();

        let loaded = load(d.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].kind, "allocator");
        assert_eq!(loaded[0].text, "bumpalo for the parser arena");
        assert_eq!(loaded[0].commit, "abc1234");
        assert_eq!(loaded[0].experiment, 1);
        assert_eq!(loaded[1].kind, "dependency");
        // Several requests keep their write order, which is the order a human
        // reads them in.
        assert_eq!(loaded[1].experiment, 2);
    }

    #[test]
    fn a_missing_file_is_an_empty_queue_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).unwrap().is_empty());
    }

    #[test]
    fn append_creates_the_state_directory() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("nested/state");
        append(&dir, &request("other", "x", 1)).unwrap();
        assert!(dir.join(REQUESTS_FILE).is_file());
    }

    // Advisory by design: one torn write must not hide every request behind
    // it.
    #[test]
    fn a_malformed_line_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &request("flags", "codegen-units=1", 1)).unwrap();
        let path = d.path().join(REQUESTS_FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not a json object\n");
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        append(d.path(), &request("unsafe", "one unsafe copy loop", 2)).unwrap();

        let loaded = load(d.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].kind, "flags");
        assert_eq!(loaded[1].kind, "unsafe");
    }

    #[test]
    fn sanitize_flattens_tabs_and_newlines_and_trims() {
        assert_eq!(sanitize("  one\ntwo\tthree\r\n"), "one two three");
    }

    #[test]
    fn sanitize_of_whitespace_is_empty() {
        assert_eq!(sanitize(" \n\t "), "");
    }

    #[test]
    fn sanitize_keeps_a_text_at_the_cap_unchanged() {
        let text = "x".repeat(MAX_TEXT_LEN);
        assert_eq!(sanitize(&text), text);
    }

    #[test]
    fn sanitize_caps_long_text_and_says_it_cut() {
        let text = "x".repeat(MAX_TEXT_LEN + 50);
        let out = sanitize(&text);
        assert_eq!(out.chars().count(), MAX_TEXT_LEN + 3);
        assert!(out.ends_with("..."), "{out}");
        assert!(out.starts_with(&"x".repeat(MAX_TEXT_LEN - 1)), "{out}");
    }

    // Cut on a character boundary, never a byte one: the file must stay
    // valid UTF-8 and readable.
    #[test]
    fn sanitize_does_not_split_a_multibyte_character() {
        let text = "é".repeat(MAX_TEXT_LEN + 50);
        let out = sanitize(&text);
        assert_eq!(out.chars().count(), MAX_TEXT_LEN + 3);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().filter(|c| *c == 'é').count(), MAX_TEXT_LEN);
    }

    // The write side owns the guarantee: whatever a caller passes, one record
    // in the file is one line.
    #[test]
    fn append_stores_the_text_sanitized() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &request("other", "line one\nline\ttwo\r\n", 1)).unwrap();
        let text = std::fs::read_to_string(d.path().join(REQUESTS_FILE)).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert_eq!(load(d.path()).unwrap()[0].text, "line one line two");
    }

    #[test]
    fn describe_names_kind_text_experiment_and_commit() {
        assert_eq!(
            describe(&request("allocator", "bumpalo", 3)),
            "[allocator] bumpalo (experiment 3, commit abc1234)"
        );
    }

    // A request written while git could not name a commit has none to show;
    // printing "commit )" would read as a bug rather than as missing context.
    #[test]
    fn describe_drops_an_empty_commit() {
        let mut r = request("other", "x", 0);
        r.commit = String::new();
        assert_eq!(describe(&r), "[other] x (experiment 0)");
    }
}
