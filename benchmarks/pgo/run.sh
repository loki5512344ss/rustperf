#!/usr/bin/env bash
# Sample-based PGO reproduction for an allocation-bound word counter.
#
# Pipeline: train build with CARGO_PROFILE_BENCH_DEBUG=2 -> perf record -b
# -> llvm-profgen -> rebuild with -Cprofile-sample-use -> interleaved ABBA
# comparison of plain vs PGO.
#
# The measured result and the three traps are documented in docs/pgo-spike.md.
# This script never calls sudo. It refuses to run at perf_event_paranoid > 1.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

ROUNDS="${ROUNDS:-4}"               # ABBA rounds; one round = 4 runs (2 plain, 2 PGO)
WARMUP_TIME="${WARMUP_TIME:-1}"     # seconds, criterion --warm-up-time
MEASURE_TIME="${MEASURE_TIME:-3}"   # seconds, criterion --measurement-time
STAT_SECS="${STAT_SECS:-2}"         # seconds, criterion --profile-time for perf stat

die() {
    printf 'error: %s\n' "$*" >&2
    exit 2
}

# --- preconditions ---------------------------------------------------------

[ "$(uname -s)" = "Linux" ] ||
    die "this script needs Linux (perf and /proc/sys); got $(uname -s) on $(uname -m)"
[ "$(uname -m)" = "x86_64" ] ||
    die "this script needs x86-64 (perf -b / llvm-profgen sample mapping); got $(uname -m)"

for tool in cargo python3 perf llvm-profgen llvm-profdata; do
    command -v "$tool" >/dev/null 2>&1 ||
        die "missing required tool '$tool' (need rustc/cargo, python3, linux perf, LLVM llvm-profgen + llvm-profdata)"
done

paranoid_file=/proc/sys/kernel/perf_event_paranoid
[ -r "$paranoid_file" ] || die "cannot read $paranoid_file"
paranoid=$(cat "$paranoid_file")
case "$paranoid" in
'' | - | *[!0-9-]*) die "unexpected value in $paranoid_file: '$paranoid'" ;;
esac

if [ "$paranoid" -gt 1 ]; then
    {
        echo "error: perf_event_paranoid is $paranoid; perf record and perf stat need it at 1 or lower."
        echo
        echo "Lower it yourself (this script never calls sudo), then re-run ./run.sh:"
        echo
        echo "    echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid"
        echo
        echo "It must be restored to 2 afterwards:"
        echo
        echo "    echo 2 | sudo tee /proc/sys/kernel/perf_event_paranoid"
    } >&2
    [ "$paranoid" = "2" ] ||
        echo "note: the current value is $paranoid; restore that instead of 2." >&2
    exit 2
fi

case "$ROUNDS" in
'' | *[!0-9]*) die "ROUNDS must be a positive integer, got '$ROUNDS'" ;;
esac
[ "$ROUNDS" -ge 1 ] || die "ROUNDS must be at least 1, got $ROUNDS"

# --- helpers ---------------------------------------------------------------

# Finds the executable of the `wordcount` bench artifact in cargo's JSON
# message stream. On this Cargo (1.100.0) bench executables live under
# target/release/build/<pkg>/<hash>/out/, not target/release/deps/.
extract_bench_exe() {
    python3 - "$1" <<'PY'
import json
import sys

path = None
with open(sys.argv[1], errors="replace") as fh:
    for line in fh:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        target = msg.get("target") or {}
        if target.get("name") != "wordcount":
            continue
        if "bench" not in (target.get("kind") or []):
            continue
        exe = msg.get("executable")
        if exe:
            path = exe
print(path or "")
PY
}

build_bench() {
    # $1 = json log file; stdout carries the JSON stream, stderr the progress.
    cargo bench --no-run --message-format=json >"$1"
}

# Runs one criterion bench binary and records its median estimate.
# $1 = label (plain|pgo), $2 = executable, $3 = round, $4 = run index
run_one() {
    local label=$1 bin=$2 round=$3 seq=$4
    local log="out/run-${label}-r${round}-${seq}.log"
    "$bin" --bench --warm-up-time "$WARMUP_TIME" --measurement-time "$MEASURE_TIME" \
        >"$log" 2>&1
    python3 - "$log" "$label" "$round" "$seq" <<'PY'
import re
import sys

log, label, rnd, seq = sys.argv[1:5]
text = open(log, errors="replace").read()
text = re.sub(r"\x1b\[[0-9;]*m", "", text)  # strip ANSI in case of a tty
match = re.search(r"time:\s*\[([^\]]+)\]", text)
if not match:
    sys.exit(f"could not find a criterion 'time: [...]' line in {log}")
found = re.findall(r"([0-9.]+)\s*(ns|µs|us|ms|s)\b", match.group(1))
if len(found) < 3:
    sys.exit(f"unexpected criterion time line in {log}: {match.group(1)!r}")
scale = {"ns": 1e-3, "µs": 1.0, "us": 1.0, "ms": 1e3, "s": 1e6}
lo, est, hi = (float(value) * scale[unit] for value, unit in found[:3])
with open("out/samples.tsv", "a") as fh:
    fh.write(f"{label}\t{rnd}\t{seq}\t{lo:.4f}\t{est:.4f}\t{hi:.4f}\n")
print(f"  {label:<5} round {int(rnd):>2} run {seq}   est {est:8.2f} µs"
      f"   [lo {lo:8.2f}   hi {hi:8.2f}]")
PY
}

perf_stat_one() {
    # $1 = label, $2 = executable. Returns non-zero if perf stat cannot run.
    # Criterion's profiling mode loops the routine for a fixed wall-clock
    # window, so the two totals cover the same amount of work.
    local label=$1 bin=$2
    local log="out/perf-stat-${label}.log"
    perf stat -e instructions:u,cycles:u,branch-misses:u \
        -- "$bin" --bench --profile-time "$STAT_SECS" >"$log" 2>&1
}

# --- build, record, profile, rebuild ---------------------------------------

mkdir -p out
export CARGO_PROFILE_BENCH_DEBUG=2

echo "==> [1/6] training build (CARGO_PROFILE_BENCH_DEBUG=2)"
build_bench out/build-plain.json
plain_exe=$(extract_bench_exe out/build-plain.json)
[ -n "$plain_exe" ] ||
    die "could not find the 'wordcount' bench executable in out/build-plain.json"
cp "$plain_exe" out/bench-plain
echo "    $plain_exe -> out/bench-plain"

echo "==> [2/6] perf record -b -F 999 (branch stacks are mandatory)"
perf record -b -F 999 -o out/perf.data -- \
    out/bench-plain --bench --profile-time 5 >out/perf-record.log 2>&1 || {
    sed 's/^/    | /' out/perf-record.log >&2
    die "perf record failed (see out/perf-record.log)"
}
echo "    out/perf.data"

echo "==> [3/6] llvm-profgen"
llvm-profgen --perfdata=out/perf.data --binary=out/bench-plain \
    --output=out/pgo.prof >out/llvm-profgen.log 2>&1 || {
    sed 's/^/    | /' out/llvm-profgen.log >&2
    die "llvm-profgen failed (see out/llvm-profgen.log)"
}
prof_bytes=$(stat -c %s out/pgo.prof 2>/dev/null || echo 0)
if [ "$prof_bytes" -lt 1024 ] ||
    grep -Eqi 'empty profile|no binary functions|no samples|invalid perf script input' \
        out/llvm-profgen.log; then
    sed 's/^/    | /' out/llvm-profgen.log >&2
    {
        echo "error: llvm-profgen produced an unusable profile (out/pgo.prof is $prof_bytes bytes)."
        echo "Two likely causes, both from docs/pgo-spike.md:"
        echo "  1. perf was recorded without branch stacks. '-b' on 'perf record' is mandatory;"
        echo "     without it llvm-profgen rejects the file as \"Invalid perf script input\"."
        echo "  2. the training binary has no debuginfo. Cargo strips debuginfo by default even"
        echo "     with -Cdebuginfo=2 in RUSTFLAGS, so the training build needs"
        echo "     CARGO_PROFILE_BENCH_DEBUG=2, or llvm-profgen reports \"no binary functions\"."
    } >&2
    exit 2
fi
echo "    out/pgo.prof ($prof_bytes bytes)"

echo "==> [4/6] rebuild with -Cprofile-sample-use=out/pgo.prof"
RUSTFLAGS="-Cprofile-sample-use=$PWD/out/pgo.prof" build_bench out/build-pgo.json
pgo_exe=$(extract_bench_exe out/build-pgo.json)
[ -n "$pgo_exe" ] ||
    die "could not find the 'wordcount' bench executable in out/build-pgo.json"
cp "$pgo_exe" out/bench-pgo
echo "    $pgo_exe -> out/bench-pgo"

# --- interleaved ABBA comparison ------------------------------------------

echo
echo "==> [5/6] ABBA comparison: $ROUNDS rounds x 4 runs (plain, pgo, pgo, plain)"
echo "    criterion: --warm-up-time $WARMUP_TIME --measurement-time $MEASURE_TIME"
: >out/samples.tsv
round=1
while [ "$round" -le "$ROUNDS" ]; do
    seq=1
    for label in plain pgo pgo plain; do
        if [ "$label" = "plain" ]; then
            bin=out/bench-plain
        else
            bin=out/bench-pgo
        fi
        run_one "$label" "$bin" "$round" "$seq"
        seq=$((seq + 1))
    done
    round=$((round + 1))
done

echo
echo "    medians over the criterion median estimate (out/samples.tsv):"
python3 - <<'PY'
import statistics

samples = {}
with open("out/samples.tsv") as fh:
    for line in fh:
        label, _rnd, _seq, _lo, est, _hi = line.split()
        samples.setdefault(label, []).append(float(est))

missing = [label for label in ("plain", "pgo") if label not in samples]
if missing:
    raise SystemExit("no samples for: " + ", ".join(missing))

medians = {label: statistics.median(vals) for label, vals in samples.items()}
for label in ("plain", "pgo"):
    vals = samples[label]
    print(f"      {label:<5} n={len(vals):>3}  median {medians[label]:8.2f} µs"
          f"  min {min(vals):8.2f}  max {max(vals):8.2f}")

delta = (medians["pgo"] - medians["plain"]) / medians["plain"] * 100.0
print(f"      delta (pgo - plain) on the median: {delta:+.2f} %"
      f"  ({'slower' if delta > 0 else 'faster'} with PGO)")
PY

echo
echo "==> [6/6] perf stat counters (one short run per binary)"
if perf_stat_one plain out/bench-plain; then
    grep -E 'instructions:u|cycles:u|branch-misses:u' out/perf-stat-plain.log |
        sed 's/^/      plain  /'
    if perf_stat_one pgo out/bench-pgo; then
        grep -E 'instructions:u|cycles:u|branch-misses:u' out/perf-stat-pgo.log |
            sed 's/^/      pgo    /'
    else
        echo "      perf stat failed for the PGO binary (see out/perf-stat-pgo.log) — counters skipped"
    fi
    echo "      A fixed ${STAT_SECS}s window counts work done, not cost per call: a slower binary"
    echo "      fits fewer calls into it, and CPU contention shortens the window (check 'time"
    echo "      elapsed' in out/perf-stat-*.log). Compare these on an idle machine only."
else
    echo "      perf stat failed (see out/perf-stat-plain.log) — counters skipped"
fi

echo
echo "Numbers are machine-specific: this CPU, this toolchain, this kernel."
echo "This script reproduces docs/pgo-spike.md; it is a reproduction, not a gate."
