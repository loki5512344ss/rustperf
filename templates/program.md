# program.md

This file is your instructions, coding agent. Read it fully before doing
anything. It is the only thing a human edits to steer this run — everything
else (the metric, the gates, the verdict) is a compiled binary you cannot
change and should not try to.

## Setup

Before starting the loop, do this once:

1. Agree a run tag with the human if one was not already given to you (a
   short slug like `sep4` — today's date or similar is fine).
2. Confirm `rustperf baseline --tag <tag>` has already been run for
   this tag. If it has not, stop and ask the human to run it, or run it
   yourself if you have been told you may. `baseline` creates the run
   branch, freezes the current test files as the golden copies, and records
   the commit this run measures against. Everything downstream depends on
   this step having happened exactly once.
3. Read the repository. Skim the package(s) named in `scope` in
   `.rustperf/config.yaml`. Run the declared benchmarks yourself once
   (`rustperf profile`) so you know what you are starting from
   before changing anything.

## Experimentation

What you MAY do:

- Edit any Rust source file that falls under one of the `scope` patterns in
  `.rustperf/config.yaml`.
- Add new files inside `scope`, as long as they are not new files under
  `tests/**` or `benches/**` that duplicate or replace existing tests.
- Add a *new* benchmark inside `scope` — it will not count toward the score
  (the score only covers the declared `benchmarks` list) but it can help you
  reason about a hot path.
- Run any read-only diagnostic command (`cargo check`, `cargo clippy`,
  `rustperf profile`) as often as you like between experiments.

What you MUST NOT do:

- Edit anything under `tests/**` or `benches/**`. These are snapshotted at
  baseline and restored byte for byte before every `eval`, so an edit there
  is silently discarded — worse, it wastes an experiment slot for nothing.
  If a test looks wrong, say so in your `--desc` for that experiment and move
  on; do not try to route around it.
- Edit a `#[cfg(test)] mod tests` block or a doctest inside a source file you
  are otherwise allowed to change. These live inside a file you may edit, so
  they cannot be restored for you the way a whole test file can — restoring
  the file would erase your optimization too. Instead they are hashed at
  baseline, and any change to one fails the experiment with reason
  `inline_test_modified`. Revert the test and re-run.
- Add or change `unsafe` code: an `unsafe { }` block, an `unsafe fn`, an
  `unsafe impl`, an `#[unsafe(..)]` attribute, or anything inside one that
  already exists. The tests and the benchmark both pass on undefined
  behaviour, so the harness cannot tell a sound `get_unchecked` from an
  unsound one and accepts neither: the experiment fails with reason
  `unsafe_added` before anything is built. Removing `unsafe`, or moving an
  existing region without changing it, is fine. Get the speed in safe code —
  iterators, `chunks_exact`, a length check hoisted out of the loop.
- Edit `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `.cargo/config`,
  `rust-toolchain.toml` or `rust-toolchain`, ever. These are rejected outright
  regardless of what `scope` says — both spellings of the cargo-config and
  toolchain files are locked, because cargo honours the extensionless form
  too and locking only the `.toml` spelling would be trivially bypassed by a
  rename. Changing a dependency or a toolchain is a supply-chain decision a
  human makes, not something an unattended loop decides, and
  `.cargo/config.toml` in particular can set `RUSTFLAGS` — adding
  `-C target-cpu=native` produces a real, reproducible speedup with no logic
  change at all, which is not what this run is measuring.
- Edit `.rustperf/config.yaml`. It is not covered by the scope gate —
  the gate only sees ordinary source files, and this file is gitignored so
  it is invisible to it either way. Instead, its hash is recorded at
  `baseline` time; if it has changed by `eval` time, the run fails with
  reason `config_changed`, not `scope_violation`.
- Edit an ordinary source file outside `scope`. This *is* what the scope
  gate itself rejects, failing the experiment before it is even measured.
- Weaken, delete, or skip a test to make it pass. A test failure means your
  change is wrong; fix the change, not the test.
- Try to weaken, disable, or reinterpret the verdict. `eval`'s exit code and
  `--json` output are the only truth. If a result looks wrong, say so in
  your `--desc`; do not try to make the harness agree with you by other
  means.
- Batch multiple unrelated changes into one experiment. One idea per
  experiment keeps every result attributable and every discard cheap.
- Reach for `unsafe` as a shortcut to a faster number. It is gated in this
  fork: `eval` fails with `unsafe_added` before the build, so the experiment
  costs a slot and changes nothing. See the note at the end of the idea bank.

## Output format

`rustperf eval` is the only command whose result decides anything.
It exits with one of four codes, and program.md's loop below branches on
exactly these:

| Exit code | Meaning | Verdict status |
|---|---|---|
| `0` | KEEP — the change is a real, safe improvement | `KEEP` |
| `1` | DISCARD — no significant improvement, or it lost the coin flip against noise | `DISCARD` |
| `2` | FAIL — a gate rejected the change: scope violation, config edit, an edited inline test, a new/missing test file, or a test failure | `FAIL` |
| `3` | CRASH — the build failed outright or a phase timed out | `CRASH` |

Any status the harness cannot classify is reported as exit code `2`
(FAIL) rather than a silent success — treat an unrecognized `--json` status
the same way you would treat FAIL. `ABORTED` (see "Stopping") also arrives
as exit code `2` for exactly that reason: it is not a verdict, and treating
it like FAIL is the right thing to do with it.

**A `KEEP` requires a real, not just a technically significant, improvement.**
`eval` only returns `KEEP` when `score` clears `1 - min_effect_pct/100` (default
1%, i.e. score < 0.99) AND at least one benchmark improved past a
Bonferroni-corrected significance bar. A change that shaves off a fraction of a
percent will be `DISCARD`ed by design, even if the numbers technically moved in
the right direction — do not spend a night chasing sub-1% wins, the harness will
not bank them. And even a `KEEP` is evidence, not proof: any significance
threshold admits some false positives, so treat one `KEEP` as a good sign worth
building on, not as a guarantee that the change actually helped.

### Reading `eval --json`

With `--json`, `eval` prints one JSON object to stdout — and nothing else —
with (at least) these fields:

```json
{
  "status": "KEEP",
  "reason": "improved",
  "score": 0.9123,
  "message": "score 0.9123 (-8.77%)",
  "regressions": [],
  "warnings": [],
  "benchmarks": [
    {
      "name": "count_words/large",
      "base_ns": 44702.0,
      "cand_ns": 38636.0,
      "pct_change": -13.57,
      "ci_low": -15.0,
      "ci_high": -12.2,
      "p": 0.00001,
      "significant": true,
      "keep_threshold_p": 0.025,
      "n_base": 10,
      "n_cand": 10
    }
  ],
  "leads": [
    {
      "kind": "format_in_loop",
      "file": "src/parse.rs",
      "line": 42,
      "function": "parse_line",
      "message": "format! allocates once per iteration; write into a reused String with write! (std::fmt::Write), or hoist the formatting out of the loop"
    }
  ],
  "stop_requested": false,
  "run": {
    "tag": "sep4",
    "branch": "rustperf/sep4",
    "baseline_commit": "a3f1c2d",
    "measure_commit": "9b7e410",
    "worktree": "/Users/you/Library/Caches/rustperf/1a2b3c4d/sep4/baseline-worktree",
    "experiment": 5
  }
}
```

`status` is one of `KEEP`, `DISCARD`, `FAIL`, `CRASH` — or `ABORTED`, which is
not a verdict at all but an interrupted experiment; see "Stopping" below.
`reason` is a stable, machine-readable code, e.g. `improved`,
`no_significant_improvement`, `improvement_below_min_effect`,
`guard_regression`, `scope_violation`, `config_changed`,
`inline_test_modified`, `unsafe_added`, `new_test_file`, `missing_test_file`,
`symlink_swap`, `frozen_store_tampered`, `baseline_tampered`, `build_failed`,
`tests_failed`, `timeout`, `stop_forced`.

`benchmarks` has one row per measured benchmark and is present whenever the
experiment got as far as measurement — on `KEEP` and on `DISCARD`. Read it
before choosing the next experiment: `pct_change` is the change in the median
(negative is faster), `p` is that benchmark's own p-value, and
`keep_threshold_p` is the bar at least one benchmark has to clear for a
`KEEP`. One benchmark at -8% with the rest flat is a different situation from
every benchmark at -1%, and the single `score` cannot tell you which you have.
`ci_low`/`ci_high` are a percentile bootstrap interval for `pct_change`, in
the same percent units: 2000 resamples of the median ratio, 2.5 %/97.5 %.
It says how wide the effect still is at this number of rounds, so `-13.57 %
[-15.0, -12.2]` and `-13.57 % [-13.7, -13.4]` are different claims even
though the point estimate is identical. (The numbers in the example above are
illustrative; the case study's real ranges are in `docs/case-study.md`.) With few rounds the interval is wide
by construction; that is the honest reading, not a bug. The keys are omitted
when the interval could not be computed.

The key is absent on `FAIL`, `CRASH` and `ABORTED`, where nothing was
measured, and `regressions` and `warnings` are omitted when empty — do not
require any of the three.

`leads` is a fork addition: syntactic hints about lines THIS candidate
introduced. They are found by scanning the changed in-scope sources and
subtracting patterns the baseline already had, so a repository that always had
a `format!` in a loop does not nag on every experiment. Kinds are stable
strings — `format_in_loop`, `to_string_in_loop`, `clone_in_loop`,
`collect_in_loop`, `map_contains_then_insert`, `collect_then_iterate` — and
each row names the file, line and enclosing function. A lead never changes a
verdict, is omitted when empty, and `leads_error` appears instead when the scan
itself failed. Treat a lead as a question, not a finding: `clone()` in a loop
is sometimes the right call, and an allocating closure inside an iterator chain
is not detected at all. Before rewriting code, check that `rustperf profile`
names the function the lead points at.

Two of those discard reasons mean genuinely different things, and the
difference should change what you do next. `no_significant_improvement`
means nothing measurably moved — the idea did not work, drop it.
`improvement_below_min_effect` means it DID work and the harness measured a
real speedup, just a smaller one than `min_effect_pct` will bank. That is a
signal the direction is right: a variation on the same idea with a larger
effect may well clear the bar, whereas the same idea applied to a hotter
path almost certainly would. Do not read it as failure.

`score` is the geometric mean of `candidate_ns/baseline_ns` across the
declared benchmarks — below 1 is faster. `message` is a human-readable
one-liner suitable for a log row. `regressions`, when present, lists which
benchmarks tripped the regression guard and by how much.

`warnings`, when present, says the measurement is too weak to carry the
verdict printed beside it — the same lines appear as `WARNING:` in the human
output, just above the `VERDICT:` line. They never change the decision; they
tell you not to over-read it. Two you may see:

- **too few rounds for a confidence interval** — the numbers are still the
  measured medians, but the interval around them is unbounded. Raise `count`.
- **no KEEP was reachable** — the significance threshold is corrected for the
  number of benchmarks compared (`alpha/k`), and with the configured `count`
  the test cannot produce a p-value that small however large the improvement
  is. Every experiment will `DISCARD` until `count` is raised, so treat this
  as a broken configuration and stop rather than continuing to burn the night
  on experiments that cannot be banked.

**What `baseline_ns` means changes as the run progresses.** It is NOT always
the commit `baseline` recorded (`run.baseline_commit`, frozen) — it is
whatever the harness's measurement baseline currently points to
(`run.measure_commit`, advancing), and a KEEP moves that pointer to the
commit you just kept (see the loop below). So `score` always answers "did
THIS experiment help, compared to the last thing that was kept" — never "is
the tree better than when the run started." Two consequences: (1) after a
KEEP, running `eval` again with nothing new committed measures your last
commit against itself and correctly `DISCARD`s — it is not a bug or a fluke;
(2) a long, honest run's total progress is not any single `score` — it is the
product of every kept `score`, which is what `rustperf report`'s
"cumulative speedup" computes for the human.

`run` is the context for the experiment just recorded: which run tag and
branch you are on, the frozen `baseline_commit` the run started from, the
advancing `measure_commit` this experiment was scored against, the pinned
baseline worktree, and `experiment`, the 1-based number of the row just
written to `results.tsv`. None of it changes what you do — it is there so you
can tell the human where the run is, since `--json` is the only channel they
can see through you.

`stop_requested` is the one field that does change what you do. See
"Stopping" below.

## Stopping

The loop does not end on its own. It ends in one of two ways, and only one of
them is yours to act on. Both arrive riding along with an `eval` you already
ran — you never poll for a stop request separately, and `status` has no
`--json` form or `stop_requested` field of its own; it only ever prints plain
text with the word `requested` or `not requested` in it, for a human to read.

**A graceful stop.** The human runs `rustperf stop`. That does not
touch you or the experiment you are running; it writes a request that the
next `eval` reports back to you as `"stop_requested": true`, alongside a
verdict that is still fully valid. When you see it:

1. Apply this verdict exactly as you would have anyway — KEEP leaves the
   commit, anything else is `git reset --hard HEAD~1`. A stop must never
   leave a commit on the branch that nothing decided on.
2. Do NOT start another experiment.
3. Run `rustperf report` and summarize, in a few lines: what you
   tried, what was kept, and what you would try next if the run resumed.
4. Exit the loop and say you have stopped because the human asked.

`stop_requested` never changes the exit code, so read it as a separate
question from the verdict — it says whether to continue, not what this
experiment was worth.

**An interrupt.** The human presses Ctrl+C, or runs `rustperf stop
--force` because they could not wait for a long benchmark to finish. Either
one cancels `eval` mid-experiment. You will see `"status": "ABORTED"` with
`"reason": "stop_forced"`, exit code `2`, and no `results.tsv` row — nothing
was measured, so nothing was recorded. Treat the commit the way you would
treat any FAIL (`git reset --hard HEAD~1`), then stop as above.

If the human wants the run to continue after all, they clear the request with
`rustperf stop --clear`. That is their decision, not something to
wait for or ask about.

## The noise floor

`rustperf status` (up to three rows, then `and N more`) and `rustperf report`
(all of them) show a `noise floor` per benchmark once four measured
experiments have covered it: how far the median the verdict compares can move
between measurements at this round count, as a percentage — the robust
round-to-round sigma scaled by `1.2533/sqrt(n)`. It answers "could this run even
see the change I am asking for". When `min_effect_pct` is below that floor,
the line says so: a run whose median moves ±2 % cannot honestly bank a 1 %
improvement. Raising `count` lowers the floor with `1/sqrt(n)`; raising
`min_effect_pct` stops banking wins this run cannot resolve. The floor is
advisory: it changes no verdict and no threshold.

## Logging

`results.tsv` is HARNESS-OWNED. `eval` appends exactly one row to it,
automatically, on every invocation, KEEP or not. You must never create,
append to, or edit `results.tsv` yourself — any manual write is either
redundant (the row already exists) or corrupts a file the harness parses
strictly, which fails the human's morning `rustperf report`, or a
future `rustperf baseline --force`, on the whole file, not just your
line.

The columns the human sees are:

```
commit	score	best_bench_delta	status	description	version
```

(The last column is the `rustperf` version that wrote the row, filled in by
the harness — the human needs it when a log spans a tool upgrade. There is
no allocation column here — `eval` measures wall-clock time only. Allocation
counts are a separate, opt-in diagnostic (`rustperf allocs`, see below), and
they never enter this file or a verdict.)

`commit`, `score`, `best_bench_delta`, and `status` are filled in by the
harness from the verdict — you do not control them. The one column that is
yours is `description`, and you set it by passing `--desc` to `eval`:

```
rustperf eval --json --desc "preallocate the map"
```

Always pass `--desc`, on every invocation, KEEP or not — it is the only
record of what you were trying. Keep it terse — a short slug, e.g.
`preallocate-vec` — and never dishonest: describe what you actually tried,
including for a DISCARD, FAIL, or CRASH. This file is the human's morning
read. A long trail of honest discards is more useful to the human than a
short trail that hides them.

`eval` already splits its own output for you: the verdict — one compact
JSON object — is written to stdout, and the full, noisy
build/test/benchmark transcript is written automatically to `run.log` in the
repository root. `run.log` is the harness's own file, opened by `eval`
itself before it does anything else — never redirect or tee `eval`'s own
stdout into it yourself (`eval --json > run.log 2>&1` or similar). Doing so
opens a second, independent file descriptor on a path the harness already
has open for the transcript; if the two ever fall out of step, whichever one
writes second overwrites the other from byte 0, destroying exactly the
diagnostic transcript you need when a step FAILs or CRASHes and you want to
see why. Run `eval --json` bare and read the verdict straight from its
stdout; open `run.log` only to *read* it, and only when you are diagnosing a
FAIL or a CRASH — not as a matter of course on every experiment, since the
verdict itself already tells you what you need for a KEEP or an ordinary
DISCARD.

## Rust optimization idea bank

Search rather than flail. Start from `rustperf profile`, not from a guess.

- **Allocation in a hot loop.** `String`/`Vec` growth without `with_capacity`;
  building a `String` a character at a time; `format!` where `write!` into a
  reused buffer would do.
- **`clone()` on a hot path.** Often a borrow, a `Cow<'_, str>`, or moving
  instead of copying.
- **`collect()` into an intermediate collection** that is immediately consumed.
  Iterator chains usually fuse without it.
- **`&str` vs `String` parameters.** Taking `String` forces a caller to
  allocate; taking `&str` does not.
- **Bounds checks.** Indexing in a loop where iterators, `chunks_exact`, or a
  single up-front slice would let the compiler drop them.
- **Dynamic dispatch.** `Box<dyn Trait>` in a hot call; a generic parameter
  monomorphizes and inlines.
- **Hasher choice.** `HashMap`'s default is SipHash, chosen for DoS resistance;
  a faster hasher is appropriate for internal, non-adversarial keys.
- **`#[inline]` on small cross-crate functions**, which are not inlined across
  a crate boundary without it or LTO.
- **Autovectorization.** `chunks_exact` plus a fixed-size inner loop often
  vectorizes where a general loop does not.
- **Algorithmic replacement.** The largest wins are usually not micro-tuning:
  a quadratic string build, a linear scan that could be a lookup, work repeated
  per iteration that could be hoisted.
- **Newer std APIs that remove work** — only when the repository's declared
  MSRV and CI toolchain already allow Rust 1.100; check `rust-version` before
  proposing one, or the experiment buys speed by breaking the supported
  compiler:
  - `str::trim_prefix` / `str::trim_suffix` (and the same on `[T]`, plus
    `Path::trim_prefix`) replace `strip_prefix(..).map(..).unwrap_or(..)`
    chains with one call and no intermediate `Option`.
  - `Vec::from_fn(n, |i| ..)` builds the vector in one pass instead of
    `vec![placeholder; n]` followed by overwriting every element.
  - `bool::toggle()` reads better than `flag = !flag` in a state machine;
    it is not a speed trick.
  - `Box::map` / `Arc::map` / `Rc::map` and `Box::take` transform or move the
    inner value without rebuilding the pointer. Whether the allocation is
    actually saved depends on the layouts; confirm it with the clock rather
    than assuming.
  - `Result<T, !>` on an internal API says the error branch cannot happen;
    the compiler also drops it from codegen.
- **Arena or custom allocator for one hot phase** (`std::alloc::Allocator`
  with `Vec::with_capacity_in` / `Box::new_in`, stable in Rust 1.100). A bump
  arena turns thousands of allocations into one and frees them all at once.
  Only worth it when the borrow checker can prove the arena outlives every
  use and the phase ends where the arena does; the clock has to show the win,
  because an arena that outlives its phase is a leak with a nicer name.

Two notes specific to this harness. Allocation counts are **not automatic** —
they exist only for a benchmark the human provisioned to report them
(`rustperf allocs`), so an allocation hypothesis still has to be confirmed by
the clock unless that report is available. And **`unsafe` is gated** against
new or changed regions: `get_unchecked` will genuinely be faster and will
genuinely pass the tests, but `eval` fails the experiment with
`unsafe_added` before it is built, and the speed would be bought with
undefined behaviour on any input the tests do not cover. Do not reach for it.

## When the fix needs a human decision

Some of the largest wins in the idea bank cannot be made by you, by design:

- **`Cargo.toml` / `Cargo.lock` are locked.** A new crate (`bumpalo`,
  `mimalloc`, `smallvec`, `phf`, …) fails the experiment before it is built.
- **`unsafe` is gated** against new or changed regions: a hand-written
  `Allocator`/`GlobalAlloc`, raw pointers, `set_len` — `FAIL(unsafe_added)`.
- **Compiler flags and the toolchain are not yours**: `RUSTFLAGS`,
  `-C target-cpu`, PGO profiles.

Do not attempt them "just to see", and never edit a locked file. Record the
need instead:

```
rustperf request "bump arena for the parse phase: ~40k allocations per parse" --kind allocator
```

Kinds are `allocator`, `dependency`, `unsafe`, `flags`, `other`;
`rustperf request --list` shows what is already pending. The request is
appended to the harness-owned queue in the run's state directory;
`rustperf status` counts it and `rustperf report` prints it for the human. It
lifts nothing: the human either provisions the dependency or allocator before
the next `baseline` (the preferred route — then it is baseline code and you
may use it safely), or sets `allow_unsafe: true` for the run (their call,
off by default). Until then, keep going with the largest improvement that
fits inside the gates; a queued request is not a reason to stop the loop.

If the human has already provisioned a benchmark to report its own counters
(see `benchmarks/allocs/README.md`), `rustperf allocs` prints `allocs/op` and
`bytes/op` for the declared benchmarks. Use it to aim the next experiment, not
to conclude one: an unprovisioned row says "not provisioned", and that is not
permission to guess a number.

## What this harness cannot see, so don't rely on it

- **No automatic allocation counts.** Not in `eval`'s result, not in
  `profile`. They exist only when the human provisioned a benchmark to print
  `RUSTPERF_ALLOCS` lines, and `rustperf allocs` reads those; an unprovisioned
  repository gets "not provisioned", never a guessed number. Even when
  present, a count is a lead, not evidence a verdict may rest on.
- **No lint gate.** Clippy is not run as a gate here — it is far too
  opinionated to force on an arbitrary repository. Passing or failing clippy
  has no bearing on any verdict.
- **No data-race detector, and that is not a gap.** Safe Rust cannot have a
  data race in the first place; there is nothing here for a `-race`-style tool
  to catch that the compiler didn't already refuse to compile.

## The experiment loop

LOOP FOREVER:

0. Print one context line, so the human watching knows where the run is
   without having to read the JSON:
   `[exp <n> | <branch> | vs <measure_commit> | stop: rustperf stop]`
   Take the numbers from the previous experiment's `run` object; on the first
   pass, from `rustperf status`.
1. Check git state: confirm you are on the run branch.
2. If you have no strong hypothesis, run `rustperf profile` and read
   the hot spots.
3. Change ONE thing in the in-scope Rust source. One idea per experiment.
4. `git add -A && git commit -m "<idea>"`
5. `rustperf eval --json --desc "<idea>"` (no redirect — do NOT tee
   or send stdout to `run.log`; either floods your context or clobbers the
   transcript, see "Logging" above). `--desc` is what lands in
   `results.tsv`'s `description` column for this experiment — always pass it.
6. Read the verdict directly from stdout: it is one compact JSON object,
   so parse or `grep '"status"'` it straight from the command's own output.
7. KEEP -> leave the commit in place, the branch advances, AND the
   harness's measurement baseline advances to this commit too — your next
   experiment is measured against what you just kept, not against where
   the run started.
   Anything else -> `git reset --hard HEAD~1`
8. If the verdict carried `"stop_requested": true`, or its status was
   `ABORTED`, the human has asked you to stop. Do not start another
   experiment — follow "Stopping" above and leave the loop.
9. Go to 0.

**NEVER STOP ON YOUR OWN**: Do not pause to ask whether to continue. The
human may be asleep. You are autonomous. If you run out of ideas, re-read the
profile output, re-read the idea bank, combine previous near-misses, or try a
more radical change.

Only the human ends this loop, and only in the two ways "Stopping" describes:
a stop request (`"stop_requested": true`) or an interrupt (`"status":
"ABORTED"`). Running out of ideas is not one of them, and neither is a long
string of DISCARDs.
