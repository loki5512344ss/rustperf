# `compare` — visible A/B micro-benchmarks

A small standalone crate with pairs of implementations of the same task. Both
sides of a pair are ordinary, readable Rust: no macros hiding the work, no
generated code, no harness magic. You are meant to read side A, read side B,
understand why they might differ, and then run them on your own machine.

This crate deliberately contains **no result table**. Numbers belong to the
machine, the allocator and the compiler version that produced them; anything
printed here would be stale the moment you read it. Run it yourself.

The guarantee that makes the comparison meaningful: every pair returns the same
value. `cargo test` asserts that for empty input, a single element, many
elements and repeated keys, so a difference in the benchmark is a difference in
*how* the same work is done.

## Layout

```
benchmarks/compare/
├── Cargo.toml
├── src/lib.rs          # the pairs + the equality tests
└── benches/
    ├── support.rs      # deterministic input generators (shared module)
    ├── string_build.rs
    ├── vec_growth.rs
    ├── clone_vs_borrow.rs
    ├── map_collect.rs
    ├── format_loop.rs
    ├── entry_vs_insert.rs
    ├── hashmap_hasher.rs
    ├── dyn_vs_generic.rs
    ├── short_string.rs
    └── arena.rs
```

One benchmark group per pair. `benches/support.rs` is a module, not a benchmark
target (`autobenches = false` in `Cargo.toml`).

## Running

From `benchmarks/compare/`:

```sh
cargo test          # prove both sides of every pair agree
cargo bench --no-run  # compile the benchmarks without running them
cargo bench         # run every group (takes a few minutes)
```

Run a single group by passing its name as a filter:

```sh
cargo bench -- string_build
cargo bench -- hashmap_hasher
cargo bench -- arena
```

A quick pass over everything, useful while editing the crate rather than for
drawing conclusions:

```sh
cargo bench -- --sample-size 10 --measurement-time 1 --warm-up-time 1
```

Criterion writes its raw data and HTML reports to `target/criterion/`
(git-ignored). `cargo bench` builds the `bench` profile; do not mix it with
numbers from a `--release` build.

## Reading the output

Criterion prints one line per benchmark:

```
string_build/push       time:   [low  median  high]
```

- The middle number is the **median**; the outer two are the confidence
  interval. Compare medians, not single runs.
- A difference of **less than a few percent is noise**. Two implementations
  within a few percent of each other are, for practical purposes, the same
  code, and you should pick the one that is easier to read and maintain.
- Run on a quiet machine. A browser tab, a container neighbour or a laptop on
  battery can move a result by tens of percent. Fix the CPU governor if you can
  and do not benchmark while compiling.
- What matters is the **ratio** between the two sides of one pair measured in
  the same session on the same machine. Criterion's `change:` line is only
  meaningful for the same group on the same machine as a previous run.
- Absolute times here are small. At tens of nanoseconds per iteration, timer
  granularity, alignment and inlining luck can dominate; prefer pairs where one
  side is clearly and repeatably ahead.

## What to look for, pair by pair

Group names are exactly the `cargo bench -- <name>` filters.

### 1. `string_build` — building a lowercased copy of a ~9 KB text

`plus_to_string` (`word = word + &c.to_string()`), `push`, `reuse_buffer`
(caller keeps the `String`).

Look for: the gap between `plus_to_string` and the other two should be huge and
should grow superlinearly with the input size — that is a quadratic copy, not a
constant factor. `push` pays for growing a buffer from zero every call;
`reuse_buffer` keeps the capacity, so it should be the fastest and the
difference between it and `push` is the price of allocation plus growth.
If you increase the text size, `plus_to_string` explodes while the others scale
roughly linearly.

### 2. `vec_growth` — turning 1000 `u64`s into a `Vec<u64>`

`new_push`, `with_capacity`, `extend` from an exact-size iterator, `collect`.

Look for: `new_push` should lose to the other three because of reallocations and
element copies; the interesting question is how much, which depends on the
allocator and on `n`. `with_capacity`, `extend` and `collect` all reserve once,
so they should be close — if they are not (for example `with_capacity` + `push`
ends up slower than `extend`), that is a codegen difference worth looking at in
`cargo asm` rather than a property of `Vec`. This pair is a good reminder that
"obviously equivalent" shapes can still compile differently.

### 3. `clone_vs_borrow` — summing name lengths over 1000 records

`clone_owned` (`fn f(&self) -> String`) vs `borrow` (`fn f(&self) -> &str`).

Look for: the owned side allocates and frees once per record per pass, so it is
normally one to two orders of magnitude slower purely on allocation traffic.
The lesson is not "never clone" — it is "when the callee only reads, the
signature should say so". Note that the borrowed side's work becomes a few
loads and adds, so the ratio is limited by how little real work is left.

### 4. `map_collect` — summing `mix(x)` over 4096 values

`collect_then_sum` (builds a `Vec`) vs `fused_sum` (`.sum()`) vs `fused_for`.

Look for: the collected version allocates and writes `n` values to the heap and
reads them back; the fused versions do neither, so they should win clearly on a
large `n`. `fused_sum` and `fused_for` should be within noise of each other —
when one is much faster than the other, it is usually an inlining difference,
not "iterators are slow" or "loops are fast".

### 5. `format_loop` — joining 512 numbers as `"v,v,v,"`

`format_in_loop`, `write_macro` (`write!` into the destination), `manual_digits`.

Look for: `format_in_loop` allocates twice per number and copies the growing
prefix, so it is quadratic in the output size — it should be last and it should
get worse faster than the others as you add values. `write_macro` removes the
temporary allocation but still runs the full `Formatter` path. `manual_digits`
skips formatting entirely and should be fastest; the question is whether the gap
is worth hand-written digit code that only works for `u32`. Read all three
results together: the ordering is the lesson, the exact ratio is not.

### 6. `entry_vs_insert` — counting 4096 words from a 32-word vocabulary

`contains_key_then_insert` vs `entry_or_insert`.

Look for: the `contains_key` version hashes the key three times per hit and
allocates a new `String` only on a miss; `entry` hashes once but builds the key
with `to_string()` **before** the call, on hits too. So the pair is a real
trade-off rather than a free win: two probes against one allocation per
iteration, and the winner depends on key length and hit rate. Because the
vocabulary repeats, most iterations are hits; try a vocabulary larger than the
input to make misses dominate, and watch the ordering move. Read the result as
"which shape does my access pattern reward", not "entry is always better".

### 7. `hashmap_hasher` — a 4096-entry `HashMap<u64, u64>`

`insert_siphash`, `insert_fnv1a`, `lookup_siphash`, `lookup_fnv1a`.

Look for: SipHash-1-3 (via `RandomState`) is keyed and multi-round; the local
FNV-1a is one xor + one multiply per byte. Both the insert and the lookup sides
should favour FNV, more so for lookups where hashing is the whole cost. The
honest conclusion is "the default hasher buys DoS resistance, and for trusted
`u64` keys you can trade some of it away" — not "always use FNV". With
attacker-controlled keys an FNV table is trivially flooded.

### 8. `dyn_vs_generic` — driving a tiny `Sink` trait over 1024 values

`dynamic` (`&mut dyn Sink`) vs `generic` (`impl S: Sink`).

Look for: the dynamic side makes one indirect call per element and cannot inline
the (deliberately tiny) body, so the accumulator cannot stay in a register across
calls. Expect the generic side to win by roughly a small constant factor. If the
two are nearly equal, check that `add` is not already being called often enough
to hide the vtable — widen the input. The point is the shape: one indirect call
per element is cheap in absolute terms and expensive relative to a three
instruction body.

### 9. `short_string` — 1000 values that all fit in 24 bytes

`inline_array` (`[u8; 24]` + length), `vec_u8`, `string`.

Look for: `inline_array` makes one allocation for the whole vector; `vec_u8` and
`string` make one allocation *per value*. The gap is allocator traffic, and it
should be large and stable. `vec_u8` and `string` are the same layout under the
hood, so they should land within noise of each other — if they do not, something
else changed. Also notice the cost of the inline representation: 25 bytes per
value even for a 2-byte string, which only pays off when allocation is the
bottleneck.

### 10. `arena` — building and summing a 1023-node binary tree

`index_arena` (`Vec<Node>` + `u32` indices) vs `rc_refcell`
(`Rc<RefCell<Node>>` per node).

Look for: the arena does one allocation and a flat traversal; the pointer
version does 1023 allocations, refcount increments on every clone, a `RefCell`
borrow check on every access, and a recursive drop that walks the refcounts.
The arena should be much faster and the gap should grow with the node count.
The trade-off is real: `Rc<RefCell<_>>` gives you shared mutation and
structural sharing, which an index arena does not — you are choosing between
those features and this cost.

## Caveats

- These are microbenchmarks. They answer "does this shape cost measurably more"
  for inputs that fit in cache, not "will my service be faster".
- `black_box` is used on inputs and outputs so the optimizer cannot delete the
  work or constant-fold the result. It is not a proof of fairness: inlining
  decisions still change between compiler versions, so re-run when you upgrade.
- The `bench` profile differs from `release` (no debug assertions, but also no
  LTO by default). Do not copy numbers between profiles.
- Only `criterion` is a dependency. Input generators in `benches/support.rs` are
  seeded by hand so a run is repeatable without `rand`.

## Adding a pair

1. Add both implementations to `src/lib.rs` with doc comments saying what
   differs and why one might be faster.
2. Add a `#[cfg(test)]` test section asserting equality on empty input, one
   element, many elements and repeated keys.
3. Add `benches/<pair>.rs` with a `criterion_group!`/`criterion_main!` pair, a
   `benchmark_group("<pair>")`, and `black_box` around inputs and outputs.
4. Declare it in `Cargo.toml` under `[[bench]]` with `harness = false`.
5. Describe what to look for here — without numbers.
