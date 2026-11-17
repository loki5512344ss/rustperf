//! Side-by-side A/B implementations of small, everyday Rust tasks.
//!
//! Every pair in this crate answers the same question twice: once the "obvious"
//! way and once the way the allocator, the optimizer or the type system likes
//! better. Both sides of a pair always produce exactly the same value - the
//! unit tests at the bottom of this file assert that - so whatever `cargo
//! bench` reports is the cost of the *shape* of the code and not the cost of
//! doing different work.
//!
//! The benchmarks that drive these functions live in `benches/`. See
//! `README.md` for how to run them and what to look for.
//!
//! Nothing here is a recommendation by itself: the numbers depend on the input
//! size, the allocator, the CPU and the compiler version. The point is that you
//! can read both sides, run them on your machine and decide.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// 1. Building a string, one character at a time
// ---------------------------------------------------------------------------

/// Pair 1, side A: `word = word + &c.to_lowercase().to_string()`.
///
/// Lowercases `input` one character at a time. Each iteration allocates a
/// temporary `String` for the character's lowercase form and then allocates a
/// *brand new* `String` for the concatenation, copying the whole prefix into
/// it. The copy length grows with every character, so the loop is quadratic in
/// the output length: this is the classic accidental O(n^2) string build.
///
/// Side B is [`lower_push`]; [`lower_into`] is the buffer-reuse variant.
pub fn lower_plus_to_string(input: &str) -> String {
    let mut word = String::new();
    for c in input.chars() {
        word = word + &c.to_lowercase().to_string();
    }
    word
}

/// Pair 1, side B: `word.push(c)` on one growing `String`.
///
/// The string owns an amortically growing buffer, so appending is an
/// occasional reallocation plus a byte copy instead of a full re-copy per
/// character. Expect this to be dramatically faster than
/// [`lower_plus_to_string`] and slower than [`lower_into`], because the buffer
/// still grows from zero every call.
pub fn lower_push(input: &str) -> String {
    let mut word = String::new();
    for c in input.chars() {
        for lower in c.to_lowercase() {
            word.push(lower);
        }
    }
    word
}

/// Pair 1, side C: reuse a caller-owned buffer.
///
/// Same push loop, but `out` is supplied by the caller: `clear()` keeps the
/// existing capacity, so a caller that lowercases many strings pays the
/// allocation cost once. The trade-off is visible in the signature - the
/// result borrows from `out` and cannot outlive the next call, which is why
/// this shape only pays off inside a loop.
pub fn lower_into(input: &str, out: &mut String) {
    out.clear();
    for c in input.chars() {
        for lower in c.to_lowercase() {
            out.push(lower);
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Growing a Vec
// ---------------------------------------------------------------------------

#[inline]
fn expand(x: u64) -> u64 {
    x.wrapping_mul(3).wrapping_add(1)
}

/// Pair 2, side A: `Vec::new()` then `push` in a loop.
///
/// Growth is amortized doubling. For 1000 elements that is ~10 reallocations
/// and ~2n element copies, all of which can disappear from the profile at
/// large sizes and dominate at small ones.
pub fn expand_new_push(input: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    for &x in input {
        out.push(expand(x));
    }
    out
}

/// Pair 2, side B: `Vec::with_capacity(n)` then `push`.
///
/// One allocation, zero reallocations, zero element copies. The cost is that
/// the capacity must be known up front and over-reserving wastes memory - so
/// this is the right shape when the length is exact and known.
pub fn expand_with_capacity(input: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(input.len());
    for &x in input {
        out.push(expand(x));
    }
    out
}

/// Pair 2, side C: `extend` from an exactly sized iterator.
///
/// `Map<slice::Iter>` is an `ExactSizeIterator`, and `Extend`'s default
/// implementation reserves `size_hint().0` up front - so this gets the same
/// single allocation as [`expand_with_capacity`] without the explicit length
/// argument. It is usually the most readable way to say "turn this slice into
/// a Vec".
pub fn expand_extend(input: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    out.extend(input.iter().map(|&x| expand(x)));
    out
}

/// Pair 2, side D: `map(..).collect()`.
///
/// The idiomatic baseline. Its cost is identical to [`expand_extend`] because
/// `collect` uses the same exact-size reservation path; it is included so you
/// can see that "collect allocates a lot" is not generally true.
pub fn expand_collect(input: &[u64]) -> Vec<u64> {
    input.iter().map(|&x| expand(x)).collect()
}

// ---------------------------------------------------------------------------
// 3. Returning owned data vs borrowing it
// ---------------------------------------------------------------------------

/// A record with an owned name, like a row read out of a database or a config
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub value: u64,
}

impl Record {
    pub fn new(name: impl Into<String>, value: u64) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

/// Pair 3, side A: `fn f(&self) -> String`, i.e. clone to own.
///
/// The caller gets an independent value it can store, mutate or send to
/// another thread, but every call allocates and copies the name. Across many
/// calls that is one malloc/free per record per pass, which shows up as
/// allocation traffic rather than as ALU work.
pub fn record_name_owned(record: &Record) -> String {
    record.name.clone()
}

/// Pair 3, side B: `fn f(&self) -> &str`, i.e. borrow.
///
/// No allocation, no copy - just a pointer and a length out of the `Record`.
/// The price is a lifetime tie: the result cannot outlive `record`, so it
/// cannot be returned from a function that drops the record or stored next to
/// it in a struct without a lifetime parameter. When all you need is to read
/// the bytes, this is strictly cheaper.
pub fn record_name_borrowed(record: &Record) -> &str {
    &record.name
}

/// Pair 3, measured: `sum` of name lengths, cloning every name on the way.
pub fn sum_name_lens_owned(records: &[Record]) -> usize {
    records
        .iter()
        .map(|record| record_name_owned(record).len())
        .sum()
}

/// Pair 3, measured: `sum` of name lengths on borrowed names.
///
/// Identical result to [`sum_name_lens_owned`], no allocation at all.
pub fn sum_name_lens_borrowed(records: &[Record]) -> usize {
    records
        .iter()
        .map(|record| record_name_borrowed(record).len())
        .sum()
}

// ---------------------------------------------------------------------------
// 4. collect() then iterate vs a fused iterator
// ---------------------------------------------------------------------------

#[inline]
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    // Masked to 48 bits so that plain `+` / `.sum()` cannot overflow even in a
    // debug build at the sizes used here (4096 * 2^48 < 2^64).
    (z ^ (z >> 31)) & 0x0000_FFFF_FFFF_FFFF
}

/// Pair 4, side A: `map(..).collect::<Vec<_>>()` and then iterate.
///
/// Building an intermediate `Vec` allocates, writes `n` values to the heap and
/// reads them back. That is real work, and it is thrown away immediately.
/// Sometimes the intermediate is genuinely wanted (debugging, reuse, sorting);
/// when it is not, it is pure overhead.
pub fn sum_mixed_collected(input: &[u64]) -> u64 {
    let mapped: Vec<u64> = input.iter().map(|&x| mix(x)).collect();
    let mut total = 0u64;
    for value in mapped {
        total = total.wrapping_add(value);
    }
    total
}

/// Pair 4, side B: one fused iterator, `.sum()`.
///
/// `Sum` folds the adapter chain without ever materializing it, so the values
/// are produced straight into the accumulator and stay in registers. Same
/// result, no heap allocation, no memory traffic.
pub fn sum_mixed_fused(input: &[u64]) -> u64 {
    input.iter().map(|&x| mix(x)).sum()
}

/// Pair 4, side C: the same fused loop written with `for`.
///
/// Included because `for` and `.sum()` lower to very similar code; a large gap
/// between them usually means one side is not inlining, not that iterators are
/// slow.
pub fn sum_mixed_for(input: &[u64]) -> u64 {
    let mut total = 0u64;
    for &x in input {
        total = total.wrapping_add(mix(x));
    }
    total
}

// ---------------------------------------------------------------------------
// 5. Formatting numbers in a loop
// ---------------------------------------------------------------------------

/// Pair 5, side A: `format!` inside the loop.
///
/// `format!("{v},")` allocates a fresh `String` per element, and `out = out +
/// ...` allocates yet another one for the concatenation. Two allocations and a
/// full prefix copy per number: quadratic in the output length, plus the
/// formatting machinery itself.
pub fn join_numbers_format(values: &[u32]) -> String {
    let mut out = String::new();
    for &value in values {
        out = out + &format!("{value},");
    }
    out
}

/// Pair 5, side B: `write!` into the one `String` being built.
///
/// `write!` formats straight into the destination buffer through `fmt::Write`
/// - no temporary allocation. It still pays for the integer formatting code
/// (digit extraction, padding logic, a `Formatter`), but only once per number
/// and with no copying of the prefix.
pub fn join_numbers_write(values: &[u32]) -> String {
    let mut out = String::new();
    for &value in values {
        write!(&mut out, "{value},").expect("writing into a String cannot fail");
    }
    out
}

/// Pair 5, side C: extract the decimal digits by hand.
///
/// Division and remainder by 10 into a small stack buffer, then push ASCII
/// bytes. No `Formatter`, no trait objects, no padding/alignment branches. The
/// trade-off is obvious: this only handles `u32` (no widths, signs, radix,
/// floats) and it is more code to get right.
pub fn join_numbers_manual(values: &[u32]) -> String {
    let mut out = String::new();
    for &value in values {
        push_u32_decimal(&mut out, value);
        out.push(',');
    }
    out
}

fn push_u32_decimal(out: &mut String, mut value: u32) {
    if value == 0 {
        out.push('0');
        return;
    }
    let mut digits = [0u8; 10];
    let mut cursor = digits.len();
    while value > 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for &digit in &digits[cursor..] {
        out.push(digit as char);
    }
}

// ---------------------------------------------------------------------------
// 6. contains_key + insert vs entry
// ---------------------------------------------------------------------------

/// Pair 6, side A: `contains_key` to test, then `get_mut` or `insert`.
///
/// The key is hashed and probed three times on a hit (`contains_key`,
/// `get_mut`) and twice on a miss (`contains_key`, `insert`). Hashing a
/// `String` walks all its bytes, so on short keys the repeated probes are a
/// large share of the loop.
pub fn word_counts_contains_key(words: &[&str]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in words {
        if counts.contains_key(*word) {
            *counts.get_mut(*word).expect("just checked") += 1;
        } else {
            counts.insert((*word).to_string(), 1);
        }
    }
    counts
}

/// Pair 6, side B: `entry(word.to_string()).or_insert(0)`.
///
/// One hash, one probe, one insertion point (a `VacantEntry` or `OccupiedEntry`)
/// instead of three lookups on a hit. The price is that the key is built
/// **before** the call: `to_string()` runs on every iteration, hits included,
/// whereas side A allocates only on a miss. So this is not a free win — it
/// trades two probes for one allocation per call, and which one dominates
/// depends on the key length and the hit rate. That trade-off is the point of
/// the pair; there is no window where the key is known to exist but the code
/// still has to look it up again.
pub fn word_counts_entry(words: &[&str]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in words {
        *counts.entry((*word).to_string()).or_insert(0) += 1;
    }
    counts
}

// ---------------------------------------------------------------------------
// 7. SipHash vs a cheap hasher for trusted u64 keys
// ---------------------------------------------------------------------------

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A minimal FNV-1a hasher.
///
/// FNV-1a is one xor and one multiply per byte: it is a few instructions for an
/// 8-byte key, where `RandomState`/SipHash-1-3 runs a keyed, multi-round mix.
/// SipHash exists to make collision attacks expensive, which is exactly the
/// protection you give up here: with attacker-chosen keys an FNV table is easy
/// to flood. Only use a hasher like this when the keys are trusted and you have
/// measured that hashing is hot.
#[derive(Clone, Copy, Debug)]
pub struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(FNV_OFFSET_BASIS)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        self.0 = hash;
    }
}

/// `BuildHasher` for [`Fnv1a`], usable as the third type parameter of a map.
pub type FnvBuildHasher = BuildHasherDefault<Fnv1a>;

/// `HashMap<u64, u64>` keyed by trusted values with the cheap hasher.
pub type FnvIndex = HashMap<u64, u64, FnvBuildHasher>;

/// Pair 7, side A: the default `HashMap`, i.e. `RandomState` + SipHash-1-3.
///
/// DoS resistant and seeded per process, which is the right default for
/// anything reachable from untrusted input. The cost is more work per key:
/// SipHash processes 8-byte blocks with several rounds of mixing.
pub fn index_default(keys: &[u64]) -> HashMap<u64, u64> {
    let mut map = HashMap::with_capacity(keys.len());
    for (slot, &key) in keys.iter().enumerate() {
        map.insert(key, slot as u64);
    }
    map
}

/// Pair 7, side B: the same inserts into an [`FnvIndex`].
///
/// Identical contents, cheaper hash function. The gap you see is the price of
/// collision resistance, and it only exists because the keys here are
/// generated locally (sequence numbers, ids, hashes of trusted data).
pub fn index_fnv(keys: &[u64]) -> FnvIndex {
    let mut map = FnvIndex::with_capacity_and_hasher(keys.len(), FnvBuildHasher::default());
    for (slot, &key) in keys.iter().enumerate() {
        map.insert(key, slot as u64);
    }
    map
}

/// Pair 7, measured: look every probe up in a default-hasher map.
pub fn lookup_sum_default(map: &HashMap<u64, u64>, probes: &[u64]) -> u64 {
    probes
        .iter()
        .map(|key| map.get(key).copied().unwrap_or(0))
        .sum()
}

/// Pair 7, measured: the same probes against an [`FnvIndex`].
pub fn lookup_sum_fnv(map: &FnvIndex, probes: &[u64]) -> u64 {
    probes
        .iter()
        .map(|key| map.get(key).copied().unwrap_or(0))
        .sum()
}

// ---------------------------------------------------------------------------
// 8. dyn dispatch vs a generic parameter
// ---------------------------------------------------------------------------

/// The interface used by pair 8. Deliberately tiny so that dispatch overhead
/// is not hidden behind the body.
pub trait Sink {
    fn add(&mut self, value: u64);
    fn total(&self) -> u64;
}

/// A concrete sink that folds values into one word.
#[derive(Debug, Default, Clone, Copy)]
pub struct MixSink {
    state: u64,
}

impl MixSink {
    pub fn new() -> Self {
        Self {
            state: 0x243F_6A88_85A3_08D3,
        }
    }
}

impl Sink for MixSink {
    #[inline]
    fn add(&mut self, value: u64) {
        self.state = (self.state ^ value)
            .wrapping_mul(0x0000_0100_0000_01b3)
            .rotate_left(5);
    }

    #[inline]
    fn total(&self) -> u64 {
        self.state
    }
}

/// Pair 8, side A: `&mut dyn Sink`.
///
/// Every `add` goes through a vtable: load the function pointer, indirect call,
/// and the body is opaque to the optimizer, so it cannot inline, vectorize or
/// keep `state` in a register across the call. In exchange the sink type can be
/// chosen at run time and different sinks can live in one `Vec<Box<dyn Sink>>`.
pub fn drive_dyn(sink: &mut dyn Sink, values: &[u64]) -> u64 {
    for &value in values {
        sink.add(value);
    }
    sink.total()
}

/// Pair 8, side B: `S: Sink`, monomorphized.
///
/// The compiler sees the concrete type, inlines `add` into the loop and can
/// often keep the accumulator in a register for the whole run. The cost is
/// compile time and code size: one copy of the loop per concrete sink type.
pub fn drive_generic<S: Sink>(sink: &mut S, values: &[u64]) -> u64 {
    for &value in values {
        sink.add(value);
    }
    sink.total()
}

// ---------------------------------------------------------------------------
// 9. Short strings: inline bytes vs Vec<u8> vs String
// ---------------------------------------------------------------------------

/// Capacity of [`InlineStr`]. Every value used with the `short_*` helpers must
/// fit, otherwise the helper panics.
pub const INLINE_CAP: usize = 24;

/// Pair 9, side A: length + fixed inline buffer, no heap at all.
///
/// The bytes live inside the value, so a `Vec<InlineStr>` for 1000 short
/// strings is one allocation instead of 1000 small ones (plus the per-prefix
/// `Vec` growth). The cost: a fixed 25-byte footprint per value even for a
/// 2-byte string, and the length has to be tracked by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineStr {
    len: u8,
    buf: [u8; INLINE_CAP],
}

impl InlineStr {
    /// Copies `value` into the inline buffer, or returns `None` if it is too
    /// long for [`INLINE_CAP`].
    pub fn new(value: &str) -> Option<Self> {
        if value.len() > INLINE_CAP {
            return None;
        }
        let mut buf = [0u8; INLINE_CAP];
        buf[..value.len()].copy_from_slice(value.as_bytes());
        Some(Self {
            len: value.len() as u8,
            buf,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    pub fn as_str(&self) -> &str {
        // Safe by construction: `new` only copies `str` bytes, so the prefix is
        // valid UTF-8 at a char boundary.
        std::str::from_utf8(self.as_bytes()).expect("inline bytes are always valid UTF-8")
    }
}

/// Pair 9, side B: `Vec<u8>` for every value.
///
/// One heap allocation per value, plus three words of `Vec` header per element
/// in the outer vector. Flexible (any length, freely mutable), but the
/// allocator is on the hot path once per item.
pub fn short_checksum_vec(values: &[&str]) -> u64 {
    let stored: Vec<Vec<u8>> = values
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect();
    checksum(stored.iter().map(|bytes| bytes.as_slice()))
}

/// Pair 9, side C: `String` for every value.
///
/// A `String` is a `Vec<u8>` with a UTF-8 invariant, so this has the same
/// allocation behaviour as [`short_checksum_vec`] and the two should land
/// within noise of each other. It is in the pair to show that `String` is not
/// magically slower - what costs is allocating per element.
pub fn short_checksum_string(values: &[&str]) -> u64 {
    let stored: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
    checksum(stored.iter().map(|value| value.as_bytes()))
}

/// Pair 9, measured: inline storage, one allocation for the whole vector.
pub fn short_checksum_inline(values: &[&str]) -> u64 {
    let stored: Vec<InlineStr> = values
        .iter()
        .map(|value| InlineStr::new(value).expect("value does not fit in INLINE_CAP bytes"))
        .collect();
    checksum(stored.iter().map(InlineStr::as_bytes))
}

fn checksum<'a>(items: impl Iterator<Item = &'a [u8]>) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for bytes in items {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

// ---------------------------------------------------------------------------
// 10. Index arena vs Rc<RefCell<_>> for a small tree
// ---------------------------------------------------------------------------

/// One node of the index-based tree: values plus child *indices*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaNode {
    pub value: u64,
    pub left: Option<u32>,
    pub right: Option<u32>,
}

/// A flattened tree: all nodes in one `Vec`, children referenced by `u32`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Arena {
    pub nodes: Vec<ArenaNode>,
}

/// One node of the pointer-based tree.
#[derive(Debug)]
pub struct RcNode {
    pub value: u64,
    pub left: Option<Rc<RefCell<RcNode>>>,
    pub right: Option<Rc<RefCell<RcNode>>>,
}

/// Handle to the pointer-based tree.
pub type RcTree = Rc<RefCell<RcNode>>;

/// Pair 10, side A: `Vec<Node>` with `u32` child indices.
///
/// Builds a complete binary tree of `n` nodes in one allocation: child of `i`
/// are `2i+1` and `2i+2`, exactly like a binary heap. Traversal is an explicit
/// stack of indices, so the nodes stay contiguous and cache friendly, there is
/// no per-node header, and nothing is refcounted.
pub fn arena_tree_sum(n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut arena = Arena {
        nodes: Vec::with_capacity(n),
    };
    for i in 0..n {
        let left = 2 * i + 1;
        let right = 2 * i + 2;
        arena.nodes.push(ArenaNode {
            value: i as u64,
            left: (left < n).then_some(left as u32),
            right: (right < n).then_some(right as u32),
        });
    }
    let mut total = 0u64;
    let mut stack: Vec<u32> = Vec::with_capacity(64);
    stack.push(0);
    while let Some(index) = stack.pop() {
        let node = arena.nodes[index as usize];
        total = total.wrapping_add(node.value);
        if let Some(left) = node.left {
            stack.push(left);
        }
        if let Some(right) = node.right {
            stack.push(right);
        }
    }
    total
}

/// Pair 10, side B: `Rc<RefCell<RcNode>>` children.
///
/// The same tree shape with owning pointers. Each node is a separate heap
/// allocation with a strong/weak refcount pair, and every access goes through a
/// `RefCell` borrow check (a counter increment plus a runtime panic path) before
/// dereferencing. Build and drop both walk the tree of refcounts. You get shared
/// mutation and cheap structural sharing, which may well be worth it - but for a
/// tree that is built once and read many times, an arena is usually much faster.
pub fn rc_tree_sum(n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    let root = build_rc_tree(0, n);
    walk_rc_tree(&root)
}

fn build_rc_tree(index: usize, n: usize) -> RcTree {
    let left = 2 * index + 1;
    let right = 2 * index + 2;
    let node = RcNode {
        value: index as u64,
        left: (left < n).then(|| build_rc_tree(left, n)),
        right: (right < n).then(|| build_rc_tree(right, n)),
    };
    Rc::new(RefCell::new(node))
}

fn walk_rc_tree(node: &RcTree) -> u64 {
    let borrowed = node.borrow();
    let mut total = borrowed.value;
    if let Some(left) = &borrowed.left {
        total = total.wrapping_add(walk_rc_tree(left));
    }
    if let Some(right) = &borrowed.right {
        total = total.wrapping_add(walk_rc_tree(right));
    }
    total
}

// ---------------------------------------------------------------------------
// Tests: every pair must agree on every input shape we care about.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_lower_agrees(input: &str) {
        let plus = lower_plus_to_string(input);
        let push = lower_push(input);
        let mut scratch = String::with_capacity(input.len() + 8);
        lower_into(input, &mut scratch);
        assert_eq!(plus, push, "input {input:?}");
        assert_eq!(plus, scratch, "input {input:?}");
        // Twice, to prove the scratch buffer really is reusable.
        lower_into(input, &mut scratch);
        assert_eq!(plus, scratch, "input {input:?} (second use)");
    }

    #[test]
    fn string_build_empty() {
        assert_lower_agrees("");
    }

    #[test]
    fn string_build_one_element() {
        assert_lower_agrees("Q");
    }

    #[test]
    fn string_build_many_elements() {
        assert_lower_agrees("Hello HunTERS of Rust: LOWER me please, and count MY words!");
        assert_lower_agrees(&"aBcDeF gHiJkL ".repeat(200));
    }

    #[test]
    fn string_build_repeated_keys() {
        assert_lower_agrees("aaaa bbbb aaaa bbbb aaaa");
        // Non-ASCII: `to_lowercase` can yield several chars for one input char,
        // so both sides must handle the same expansion.
        assert_lower_agrees("ÄÖÜ ẞ İ");
    }

    fn assert_expand_agrees(input: &[u64]) {
        let new_push = expand_new_push(input);
        assert_eq!(new_push, expand_with_capacity(input));
        assert_eq!(new_push, expand_extend(input));
        assert_eq!(new_push, expand_collect(input));
    }

    #[test]
    fn vec_growth_empty() {
        assert_expand_agrees(&[]);
    }

    #[test]
    fn vec_growth_one_element() {
        assert_expand_agrees(&[0]);
        assert_expand_agrees(&[u64::MAX]);
    }

    #[test]
    fn vec_growth_many_elements() {
        let many: Vec<u64> = (0..1000).collect();
        assert_expand_agrees(&many);
    }

    #[test]
    fn vec_growth_repeated_keys() {
        assert_expand_agrees(&[7, 7, 7, 7]);
    }

    fn records(names: &[&str]) -> Vec<Record> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| Record::new(*name, i as u64))
            .collect()
    }

    fn assert_name_lens_agree(names: &[&str]) {
        let records = records(names);
        let owned = sum_name_lens_owned(&records);
        let borrowed = sum_name_lens_borrowed(&records);
        assert_eq!(owned, borrowed);
        // And per call, including the value comparison the caller would do.
        for record in &records {
            let owned = record_name_owned(record);
            assert_eq!(owned, record_name_borrowed(record));
            assert_eq!(owned.as_bytes(), record_name_borrowed(record).as_bytes());
        }
    }

    #[test]
    fn clone_vs_borrow_empty() {
        assert_name_lens_agree(&[]);
    }

    #[test]
    fn clone_vs_borrow_one_element() {
        assert_name_lens_agree(&["only"]);
    }

    #[test]
    fn clone_vs_borrow_many_elements() {
        let names: Vec<&str> = vec!["alpha"; 500];
        assert_name_lens_agree(&names);
        let mixed: Vec<&str> = (0..300)
            .map(|i| ["", "a", "medium-name", "a-considerably-longer-name"][i % 4])
            .collect();
        assert_name_lens_agree(&mixed);
    }

    #[test]
    fn clone_vs_borrow_repeated_keys() {
        assert_name_lens_agree(&["dup", "dup", "dup"]);
    }

    fn assert_mix_sums_agree(input: &[u64]) {
        let collected = sum_mixed_collected(input);
        assert_eq!(collected, sum_mixed_fused(input));
        assert_eq!(collected, sum_mixed_for(input));
    }

    #[test]
    fn map_collect_empty() {
        assert_mix_sums_agree(&[]);
    }

    #[test]
    fn map_collect_one_element() {
        assert_mix_sums_agree(&[1]);
        assert_mix_sums_agree(&[u64::MAX]);
    }

    #[test]
    fn map_collect_many_elements() {
        let many: Vec<u64> = (0..4096).collect();
        assert_mix_sums_agree(&many);
    }

    #[test]
    fn map_collect_repeated_keys() {
        assert_mix_sums_agree(&[5, 5, 5, 5]);
    }

    fn assert_number_join_agrees(values: &[u32]) {
        let formatted = join_numbers_format(values);
        assert_eq!(formatted, join_numbers_write(values));
        assert_eq!(formatted, join_numbers_manual(values));
    }

    #[test]
    fn format_loop_empty() {
        assert_number_join_agrees(&[]);
    }

    #[test]
    fn format_loop_one_element() {
        assert_number_join_agrees(&[0]);
        assert_number_join_agrees(&[7]);
        assert_number_join_agrees(&[u32::MAX]);
    }

    #[test]
    fn format_loop_many_elements() {
        let many: Vec<u32> = (0..1000).collect();
        assert_number_join_agrees(&many);
        assert_number_join_agrees(&[9, 10, 99, 100, 123_456_789, 4_294_967_295]);
    }

    #[test]
    fn format_loop_repeated_keys() {
        assert_number_join_agrees(&[5, 5, 5]);
    }

    fn assert_word_counts_agree(words: &[&str]) {
        let contains = word_counts_contains_key(words);
        let entry = word_counts_entry(words);
        assert_eq!(contains, entry);
        assert_eq!(contains.len(), entry.len());
        assert_eq!(contains.values().sum::<usize>(), words.len());
    }

    #[test]
    fn entry_vs_insert_empty() {
        assert_word_counts_agree(&[]);
    }

    #[test]
    fn entry_vs_insert_one_element() {
        assert_word_counts_agree(&["solo"]);
    }

    #[test]
    fn entry_vs_insert_many_elements() {
        let words: Vec<&str> = (0..1000)
            .map(|i| ["red", "green", "blue", "cyan"][i % 4])
            .collect();
        assert_word_counts_agree(&words);
    }

    #[test]
    fn entry_vs_insert_repeated_keys() {
        assert_word_counts_agree(&["same", "same", "same", "same"]);
    }

    fn assert_indexes_agree(keys: &[u64], probes: &[u64]) {
        let default = index_default(keys);
        let fnv = index_fnv(keys);
        assert_eq!(default.len(), fnv.len());
        for (key, value) in &default {
            assert_eq!(fnv.get(key), Some(value));
        }
        assert_eq!(
            lookup_sum_default(&default, probes),
            lookup_sum_fnv(&fnv, probes)
        );
    }

    #[test]
    fn hashmap_hasher_empty() {
        assert_indexes_agree(&[], &[]);
        assert_indexes_agree(&[], &[1, 2, 3]);
    }

    #[test]
    fn hashmap_hasher_one_element() {
        assert_indexes_agree(&[42], &[42]);
    }

    #[test]
    fn hashmap_hasher_many_elements() {
        let keys: Vec<u64> = (0..4096u64).map(|i| i.wrapping_mul(0x9E37_79B9)).collect();
        let probes: Vec<u64> = (0..4096u64)
            .map(|i| (i * 2).wrapping_mul(0x9E37_79B9))
            .collect();
        assert_indexes_agree(&keys, &probes);
    }

    #[test]
    fn hashmap_hasher_repeated_keys() {
        // Duplicates collapse to one entry with the *last* slot, on both sides.
        assert_indexes_agree(&[7, 7, 7, 7], &[7, 7, 8]);
    }

    fn assert_sinks_agree(values: &[u64]) {
        let mut boxed: Box<dyn Sink> = Box::new(MixSink::new());
        let dynamic = drive_dyn(boxed.as_mut(), values);

        let mut concrete = MixSink::new();
        let generic = drive_generic(&mut concrete, values);
        assert_eq!(dynamic, generic);

        let mut borrowed_dyn = MixSink::new();
        assert_eq!(dynamic, drive_dyn(&mut borrowed_dyn, values));
    }

    #[test]
    fn dyn_vs_generic_empty() {
        assert_sinks_agree(&[]);
    }

    #[test]
    fn dyn_vs_generic_one_element() {
        assert_sinks_agree(&[1]);
    }

    #[test]
    fn dyn_vs_generic_many_elements() {
        let many: Vec<u64> = (0..1024).collect();
        assert_sinks_agree(&many);
    }

    #[test]
    fn dyn_vs_generic_repeated_keys() {
        assert_sinks_agree(&[9, 9, 9, 9]);
    }

    fn assert_short_checksums_agree(values: &[&str]) {
        let inline = short_checksum_inline(values);
        assert_eq!(inline, short_checksum_vec(values));
        assert_eq!(inline, short_checksum_string(values));
    }

    #[test]
    fn short_string_empty() {
        assert_short_checksums_agree(&[]);
    }

    #[test]
    fn short_string_one_element() {
        assert_short_checksums_agree(&["x"]);
        assert_short_checksums_agree(&["abcdefghijklmnopqrstuvwx"]);
    }

    #[test]
    fn short_string_many_elements() {
        let many: Vec<&str> = (0..1000)
            .map(|i| ["id", "name", "value", "unit"][i % 4])
            .collect();
        assert_short_checksums_agree(&many);
    }

    #[test]
    fn short_string_repeated_keys() {
        assert_short_checksums_agree(&["dup", "dup", "dup"]);
    }

    #[test]
    fn short_string_over_capacity_is_rejected() {
        assert!(InlineStr::new(&"x".repeat(INLINE_CAP + 1)).is_none());
        let at_cap = "y".repeat(INLINE_CAP);
        let inline = InlineStr::new(&at_cap).expect("fits exactly");
        assert_eq!(inline.as_str(), at_cap.as_str());
    }

    fn assert_trees_agree(n: usize) {
        let arena = arena_tree_sum(n);
        assert_eq!(arena, rc_tree_sum(n), "n = {n}");
        if n > 0 {
            // Values are 0..n, so the sum is known independently of both impls.
            assert_eq!(arena, (0..n as u64).sum::<u64>(), "n = {n}");
        } else {
            assert_eq!(arena, 0);
        }
    }

    #[test]
    fn arena_empty() {
        assert_trees_agree(0);
    }

    #[test]
    fn arena_one_element() {
        assert_trees_agree(1);
    }

    #[test]
    fn arena_many_elements() {
        assert_trees_agree(2);
        assert_trees_agree(7);
        assert_trees_agree(100);
        assert_trees_agree(1023);
    }

    #[test]
    fn arena_repeated_structure() {
        // Repeated calls must build and free the same shape without leaking or
        // leaving refcounts behind.
        for _ in 0..3 {
            assert_trees_agree(31);
        }
    }
}
