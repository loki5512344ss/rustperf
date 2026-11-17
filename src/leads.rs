//! Syntax-level leads for the agent: cheap, honest hints about what to try next.
//!
//! A lead is never a verdict and never touches scoring. It answers the
//! question the agent otherwise answers by rereading its own diff: "which
//! lines I just wrote are the usual causes of a speedup I did not get?" The
//! patterns are deliberately syntactic, so they can be wrong about intent —
//! `clone()` in a loop is sometimes the right call — and every message names
//! the alternative to weigh, not an instruction to follow.
//!
//! Only *new* occurrences count. Baseline and candidate are scanned separately
//! and their findings subtracted, so a repository that already had a `format!`
//! in a loop does not produce a lead on every experiment. Comparison is by
//! `(kind, function)` counts rather than by line: an agent that edits above a
//! finding moves every line below it, and a lead that appears merely because
//! lines shifted would be noise. The cost of that choice is a missed lead when
//! an agent moves a smell into an already-smelly function — acceptable for a
//! hint, not for a gate.
//!
//! What is deliberately not scanned: closures. `xs.iter().map(|x| x.to_string())`
//! allocates exactly like a loop, but attributing it needs type knowledge this
//! module does not have, and a false "loop" on every iterator chain would drown
//! the real leads.

use quote::ToTokens;
use std::collections::BTreeMap;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, Stmt};

use crate::{discover, freeze, gitx, scope};

/// One hint, tied to a place the candidate introduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lead {
    /// Stable machine-readable kind, e.g. `format_in_loop`.
    pub kind: &'static str,
    /// Repository-relative path. Empty until [`for_changes`] fills it in.
    pub file: String,
    /// 1-based line in the candidate.
    pub line: usize,
    /// Innermost enclosing function; empty at module scope.
    pub function: String,
    /// What to weigh instead. Prose for the agent, not a verdict.
    pub message: String,
}

/// Findings in one source text, without a file name and without baseline
/// subtraction. Public for tests and for tools that already hold the text.
pub fn scan(source: &str) -> Vec<Lead> {
    // A file that does not parse is not an error here: the build gate reports
    // syntax errors precisely, and a hint generator must not pre-empt it.
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut scanner = Scanner {
        leads: Vec::new(),
        function: String::new(),
        loop_depth: 0,
    };
    scanner.visit_file(&file);
    let mut leads = scanner.leads;
    leads.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then(a.kind.cmp(b.kind))
            .then(a.function.cmp(&b.function))
    });
    leads
}

/// Findings the candidate introduced, i.e. those not already present in the
/// baseline text. Comparison is by `(kind, function)` counts, so an extra
/// occurrence in a function that already had one is a lead, while the
/// pre-existing one is not.
pub fn find(baseline: Option<&str>, candidate: &str) -> Vec<Lead> {
    let mut remaining: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    for lead in scan(baseline.unwrap_or("")) {
        *remaining.entry((lead.kind, lead.function)).or_default() += 1;
    }
    scan(candidate)
        .into_iter()
        .filter(|lead| {
            let key = (lead.kind, lead.function.clone());
            match remaining.get_mut(&key) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            }
        })
        .collect()
}

/// Leads for every in-scope Rust file that differs from `commit`.
///
/// `commit` must be the ADVANCING measurement anchor
/// (`state::Baseline::measure_commit`), not the frozen run anchor: a smell the
/// agent introduced and the run kept is part of what the next experiment is
/// measured against, so re-reporting it every experiment after would be noise
/// that grows with the run.
///
/// Test and benchmark files are skipped: they are frozen, so nothing the agent
/// writes there can be measured, and a lead pointing at one would invite a
/// `FAIL(new_test_file)`. An unreadable file is skipped too — a deleted file
/// has nothing to hint at, and the scope gate owns a path that should not
/// have been touched.
pub fn for_changes(
    root: &Path,
    commit: &str,
    matcher: &scope::Matcher,
) -> Result<Vec<Lead>, String> {
    let mut out = Vec::new();
    for rel in gitx::changed_since(root, commit)? {
        let rel = rel.replace('\\', "/");
        // A file whose stem is exactly `tests` is the split-module form of
        // inline test material — `#[cfg(test)] mod tests;` in `src/foo.rs`
        // naming `src/foo/tests.rs` — which `is_test_or_bench_path` does not
        // see because it is not under a `tests/` directory. That material is
        // frozen by `freeze::inline_hashes_at`, exactly like an inline
        // `#[cfg(test)] mod tests { .. }`, so it is skipped here too.
        // Deliberately conservative: a production module that happens to be
        // named `tests` loses its leads, which is cheaper than a lead that
        // can only cost an experiment.
        let is_split_test_module = Path::new(&rel)
            .file_stem()
            .is_some_and(|stem| stem == "tests");
        if !rel.ends_with(".rs")
            || discover::is_test_or_bench_path(&rel)
            || is_split_test_module
            || !matcher.matches(&rel)
        {
            continue;
        }
        let Ok(candidate) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let baseline = gitx::file_at(root, commit, &rel)?;
        for mut lead in find(baseline.as_deref(), &candidate) {
            lead.file = rel.clone();
            out.push(lead);
        }
    }
    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.kind.cmp(b.kind))
    });
    Ok(out)
}

/// One method call found while looking for a pair of operations on the same
/// receiver.
struct MethodHit {
    /// The receiver's tokens, stringified only to compare two calls on "the
    /// same" expression; it is never shown to a reader.
    receiver: String,
    line: usize,
}

struct Scanner {
    leads: Vec<Lead>,
    /// Innermost function name; methods are named by their own identifier.
    function: String,
    /// How many `for`/`while`/`loop` bodies enclose the current node. A
    /// closure inside a loop keeps the count of its enclosing loop; an item
    /// declared inside one does not, because it is compiled once.
    loop_depth: usize,
}

impl Scanner {
    fn push(&mut self, kind: &'static str, line: usize, message: impl Into<String>) {
        self.leads.push(Lead {
            kind,
            file: String::new(),
            line,
            function: self.function.clone(),
            message: message.into(),
        });
    }
}

/// The attributes on an impl item kind that can carry `#[cfg(test)]`.
///
/// `syn::ImplItem` has no attrs accessor shared across variants either, so
/// this matches explicitly — with the same fail-safe reasoning as
/// [`freeze::item_attrs`]: an unlisted kind is never mistaken for test
/// material, and only the four kinds that can carry an attribute are here.
fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(i) => &i.attrs,
        syn::ImplItem::Fn(i) => &i.attrs,
        syn::ImplItem::Type(i) => &i.attrs,
        syn::ImplItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

impl<'ast> Visit<'ast> for Scanner {
    /// Items are where both boundaries live.
    ///
    /// Frozen test material is dropped: a change inside `#[cfg(test)]` is
    /// `FAIL(inline_test_modified)`, so a lead pointing there can only cost
    /// the experiment. [`freeze::is_cfg_test`] is the same structural
    /// predicate the gate uses, so `#[cfg(not(test))]` — ordinary production
    /// code — is still scanned, where substring-matching `test` would have
    /// skipped it.
    ///
    /// The loop context is reset for the same reason: an item declared inside
    /// a loop body is compiled once, however many times the loop runs, so
    /// nothing inside it is per-iteration.
    fn visit_item(&mut self, node: &'ast syn::Item) {
        if freeze::item_attrs(node).iter().any(freeze::is_cfg_test) {
            return;
        }
        let loop_depth = std::mem::take(&mut self.loop_depth);
        syn::visit::visit_item(self, node);
        self.loop_depth = loop_depth;
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let function = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        let loop_depth = std::mem::take(&mut self.loop_depth);
        syn::visit::visit_item_fn(self, node);
        self.loop_depth = loop_depth;
        self.function = function;
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        if impl_item_attrs(node).iter().any(freeze::is_cfg_test) {
            return;
        }
        syn::visit::visit_impl_item(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let function = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        let loop_depth = std::mem::take(&mut self.loop_depth);
        syn::visit::visit_impl_item_fn(self, node);
        self.loop_depth = loop_depth;
        self.function = function;
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.loop_depth += 1;
        syn::visit::visit_expr_for_loop(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.loop_depth += 1;
        syn::visit::visit_expr_while(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.loop_depth += 1;
        syn::visit::visit_expr_loop(self, node);
        self.loop_depth -= 1;
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        if self.loop_depth > 0 && path_ends_with(&node.mac.path, "format") {
            self.push(
                "format_in_loop",
                node.mac.path.span().start().line,
                "format! allocates once per iteration; write into a reused String with write! \
                 (std::fmt::Write), or hoist the formatting out of the loop",
            );
        }
        syn::visit::visit_expr_macro(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if self.loop_depth > 0 {
            let line = node.method.span().start().line;
            match node.method.to_string().as_str() {
                "to_string" => self.push(
                    "to_string_in_loop",
                    line,
                    "to_string() allocates once per iteration; push into an existing String, or \
                     borrow the &str it came from",
                ),
                "clone" => self.push(
                    "clone_in_loop",
                    line,
                    "clone() once per iteration; weigh a borrow, Cow, or moving the value instead",
                ),
                "collect" => self.push(
                    "collect_in_loop",
                    line,
                    "collect() once per iteration; keep the iterator lazy, or drain into a reused \
                     buffer",
                ),
                _ => {}
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.inspect_statements(&node.stmts);
        syn::visit::visit_block(self, node);
    }
}

impl Scanner {
    /// Patterns that only make sense across statements: a `collect` whose
    /// result is only iterated, and the `if` shape below.
    fn inspect_statements(&mut self, stmts: &[Stmt]) {
        for (index, stmt) in stmts.iter().enumerate() {
            self.inspect_if_with_map_insert(stmt);
            self.inspect_collect_then_iterate(stmt, &stmts[index + 1..]);
        }
    }

    /// `if !m.contains_key(k) { m.insert(k, v); }` — the shape the entry API
    /// exists to replace.
    ///
    /// The negation is load-bearing, not cosmetic: `or_insert(..)` inserts
    /// when the key is ABSENT, which is what `!contains_key` means. A plain
    /// `if m.contains_key(k) { m.insert(k, 1); }` means "update only if
    /// present", so the advice would change the program; that shape gets no
    /// lead. A `contains_key` and an `insert` in two separate statements is
    /// likewise not reported: without the `if` there is no way to see that
    /// they are one conditional write rather than two independent calls, and
    /// guessing costs the agent a false hint.
    fn inspect_if_with_map_insert(&mut self, stmt: &Stmt) {
        let Stmt::Expr(Expr::If(node), _) = stmt else {
            return;
        };
        let Expr::Unary(unary) = &*node.cond else {
            return;
        };
        if !matches!(unary.op, syn::UnOp::Not(_)) {
            return;
        }
        let Some(contains) = first_method(&unary.expr, "contains_key") else {
            return;
        };
        let then = &node.then_branch;
        let Some(insert) = then.stmts.iter().find_map(|s| {
            first_method_in_statement(s, "insert").filter(|i| i.receiver == contains.receiver)
        }) else {
            return;
        };
        self.push(
            "map_contains_then_insert",
            insert.line,
            "contains_key then insert hashes the key twice; entry(k).or_insert(..), or \
             or_insert_with(..) for an expensive value, hashes once",
        );
    }

    /// `let v = ..collect();` immediately followed by `for x in v` — the
    /// intermediate collection buys nothing the chain does not.
    fn inspect_collect_then_iterate(&mut self, stmt: &Stmt, rest: &[Stmt]) {
        let Stmt::Local(local) = stmt else {
            return;
        };
        let Some(init) = &local.init else {
            return;
        };
        if !expr_has_method(&init.expr, "collect") {
            return;
        }
        let Some(name) = pat_ident(&local.pat) else {
            return;
        };
        // The collected value's first use decides. Only `for x in v` counts:
        // `v.len()`, `v[0]` or a second pass means the collection is doing
        // real work, and a lead there would be wrong.
        for next in rest {
            if let Some(is_for_loop) = first_use(next, &name) {
                if is_for_loop {
                    self.push(
                        "collect_then_iterate",
                        init.expr.span().start().line,
                        "collect() into a Vec whose first use is one `for` loop; the chain \
                         usually fuses without the intermediate collection",
                    );
                }
                return;
            }
        }
    }
}

/// Whether the last segment of a macro path is `name`, e.g. `str::format` and
/// `format` alike.
fn path_ends_with(path: &syn::Path, name: &str) -> bool {
    path.segments
        .last()
        .map(|s| s.ident == name)
        .unwrap_or(false)
}

/// Tokens of an expression, for comparing "the same receiver" across two
/// calls. Round-trips to the same string for the same written expression.
fn tokens(expr: &Expr) -> String {
    expr.to_token_stream().to_string()
}

/// First call to method `name` anywhere inside `expr`.
fn first_method(expr: &Expr, name: &str) -> Option<MethodHit> {
    let mut finder = MethodFinder { name, hit: None };
    finder.visit_expr(expr);
    finder.hit
}

/// First call to method `name` anywhere inside one statement.
fn first_method_in_statement(stmt: &Stmt, name: &str) -> Option<MethodHit> {
    let mut finder = MethodFinder { name, hit: None };
    finder.visit_stmt(stmt);
    finder.hit
}

/// Whether any call to method `name` occurs inside `expr`.
fn expr_has_method(expr: &Expr, name: &str) -> bool {
    first_method(expr, name).is_some()
}

/// The name bound by a local pattern, when it is a plain identifier.
fn pat_ident(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(ident) => Some(ident.ident.to_string()),
        // `let v: Vec<u32> = ..` binds through a type ascription.
        syn::Pat::Type(typed) => pat_ident(&typed.pat),
        syn::Pat::Paren(paren) => pat_ident(&paren.pat),
        _ => None,
    }
}

/// Whether a statement mentions `name` at all, and if so whether its own
/// expression is a `for` loop over exactly `name`.
fn first_use(stmt: &Stmt, name: &str) -> Option<bool> {
    if let Stmt::Expr(Expr::ForLoop(loop_), _) = stmt {
        if expr_is_ident(&loop_.expr, name) {
            return Some(true);
        }
    }
    let mut finder = IdentFinder { name, found: false };
    finder.visit_stmt(stmt);
    finder.found.then_some(false)
}

/// Whether an expression is exactly the path `name`.
fn expr_is_ident(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Path(path) => path.path.get_ident().map(|i| i == name).unwrap_or(false),
        Expr::Group(group) => expr_is_ident(&group.expr, name),
        _ => false,
    }
}

/// Whether any identifier in the visited tree is `name`.
struct IdentFinder<'a> {
    name: &'a str,
    found: bool,
}

impl<'ast> Visit<'ast> for IdentFinder<'_> {
    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        if node == self.name {
            self.found = true;
        }
    }
}

/// Stops at the first call to one method name and keeps its receiver.
struct MethodFinder<'a> {
    name: &'a str,
    hit: Option<MethodHit>,
}

impl<'ast> Visit<'ast> for MethodFinder<'_> {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if self.hit.is_some() {
            return;
        }
        if node.method == self.name {
            self.hit = Some(MethodHit {
                receiver: tokens(&node.receiver),
                line: node.method.span().start().line,
            });
            return;
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<&'static str> {
        scan(source).into_iter().map(|l| l.kind).collect()
    }

    fn only(source: &str) -> Lead {
        let leads = scan(source);
        assert_eq!(leads.len(), 1, "{leads:?}");
        leads.into_iter().next().unwrap()
    }

    // --- format! ------------------------------------------------------------

    #[test]
    fn format_inside_a_loop_is_a_lead() {
        let src = "fn f(xs: &[u32]) -> usize {\n    let mut n = 0;\n    \
                   for x in xs {\n        n += format!(\"{x}\").len();\n    }\n    n\n}\n";
        let lead = only(src);
        assert_eq!(lead.kind, "format_in_loop");
        assert_eq!(lead.function, "f");
        assert_eq!(lead.line, 4);
        assert!(lead.message.contains("write!"));
    }

    #[test]
    fn format_outside_a_loop_is_not_a_lead() {
        assert!(kinds("fn f(x: u32) -> String { format!(\"{x}\") }").is_empty());
    }

    #[test]
    fn a_nested_loop_is_still_a_loop() {
        let src = "fn f(xs: &[u32]) {\n    while true {\n        for x in xs {\n            \
                   let _ = format!(\"{x}\");\n        }\n    }\n}\n";
        assert_eq!(kinds(src), ["format_in_loop"]);
    }

    #[test]
    fn a_closure_inside_a_loop_keeps_the_loop_context() {
        // No type knowledge is needed to see that this allocates per iteration.
        let src = "fn f(xs: &[u32]) {\n    for x in xs {\n        let _ = || format!(\"{x}\");\n    }\n}\n";
        assert_eq!(kinds(src), ["format_in_loop"]);
    }

    #[test]
    fn other_macros_are_not_format_leads() {
        assert!(kinds("fn f(xs: &[u32]) { for x in xs { print!(\"{x}\"); } }").is_empty());
    }

    // --- per-iteration methods ---------------------------------------------

    #[test]
    fn to_string_clone_and_collect_in_a_loop_are_leads() {
        let src = "fn f(xs: &[u32]) {\n    for x in xs {\n        let a = x.to_string();\n        \
                   let b = a.clone();\n        let c: Vec<char> = b.chars().collect();\n        \
                   let _ = (b, c);\n    }\n}\n";
        assert_eq!(
            kinds(src),
            ["to_string_in_loop", "clone_in_loop", "collect_in_loop"]
        );
    }

    #[test]
    fn the_same_calls_outside_a_loop_are_not_leads() {
        let src = "fn f(x: u32) {\n    let a = x.to_string();\n    let b = a.clone();\n    \
                   let c: Vec<char> = b.chars().collect();\n    let _ = (b, c);\n}\n";
        assert!(kinds(src).is_empty());
    }

    #[test]
    fn a_lead_is_attributed_to_the_innermost_function() {
        let src = "fn outer() {\n    fn inner(xs: &[u32]) {\n        for x in xs {\n            \
                   let _ = x.to_string();\n        }\n    }\n    inner(&[]);\n}\n";
        assert_eq!(only(src).function, "inner");
    }

    #[test]
    fn a_method_is_named_by_its_own_identifier() {
        let src = "struct S;\nimpl S {\n    fn go(&self, xs: &[u32]) {\n        for x in xs {\n            \
                   let _ = x.to_string();\n        }\n    }\n}\n";
        assert_eq!(only(src).function, "go");
    }

    // A nested item is compiled once however many times the enclosing loop
    // runs, so its body must not inherit the loop context — otherwise
    // `format!` inside a helper declared in a loop body reads as
    // `format_in_loop` when it is nothing of the sort.
    #[test]
    fn a_nested_item_inside_a_loop_is_not_a_per_iteration_lead() {
        let src = "fn outer() {\n    for _ in 0..3 {\n        fn helper() {\n            \
                   let _ = format!(\"{}\", 1);\n        }\n        helper();\n    }\n}\n";
        assert!(kinds(src).is_empty());
    }

    #[test]
    fn an_impl_nested_in_a_loop_is_not_a_per_iteration_lead() {
        let src = "fn outer() {\n    for _ in 0..3 {\n        struct S;\n        \
                   impl S {\n            fn helper() {\n                \
                   let _ = format!(\"{}\", 1);\n            }\n        }\n    }\n}\n";
        assert!(kinds(src).is_empty());
    }

    // --- frozen test material ----------------------------------------------
    // A change inside `#[cfg(test)]` is `FAIL(inline_test_modified)`, so a
    // lead pointing there can only cost the experiment. The structural
    // predicate from `freeze` is reused rather than substring-matching
    // "test", which would also skip `#[cfg(not(test))]` — production code.
    #[test]
    fn an_inline_test_module_is_not_scanned() {
        let src = "fn clean() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       #[test]\n\
                       fn t(xs: &[u32]) {\n\
                           for x in xs {\n\
                               let _ = format!(\"{x}\");\n\
                           }\n\
                       }\n\
                   }\n";
        assert!(kinds(src).is_empty());
    }

    #[test]
    fn a_cfg_not_test_item_is_still_scanned() {
        let src = "#[cfg(not(test))]\n\
                   fn t(xs: &[u32]) {\n    for x in xs {\n        \
                   let _ = format!(\"{x}\");\n    }\n}\n";
        assert_eq!(kinds(src), ["format_in_loop"]);
    }

    #[test]
    fn the_same_pattern_in_a_normal_function_is_still_a_lead() {
        let src = "fn t(xs: &[u32]) {\n    for x in xs {\n        \
                   let _ = format!(\"{x}\");\n    }\n}\n";
        assert_eq!(kinds(src), ["format_in_loop"]);
    }

    // --- map patterns -------------------------------------------------------

    #[test]
    fn contains_key_then_insert_in_one_if_is_a_lead() {
        let src = "use std::collections::HashMap;\n\
                   fn f(m: &mut HashMap<u32, u32>, k: u32) {\n    \
                   if !m.contains_key(&k) {\n        m.insert(k, 1);\n    }\n}\n";
        let lead = only(src);
        assert_eq!(lead.kind, "map_contains_then_insert");
        assert_eq!(lead.line, 4);
        assert!(lead.message.contains("entry"));
    }

    // The entry-API advice inserts when the key is absent, which is only
    // what the code does when the condition is negated. A plain
    // `if m.contains_key(k)` means "update only if present" — `or_insert`
    // would insert in the absent case, changing the program. Reporting it
    // would be worse than missing it.
    #[test]
    fn non_negated_contains_key_then_insert_is_not_a_lead() {
        let src = "use std::collections::HashMap;\n\
                   fn f(m: &mut HashMap<u32, u32>, k: u32) {\n    \
                   if m.contains_key(&k) {\n        m.insert(k, 1);\n    }\n}\n";
        assert!(kinds(src).is_empty());
    }

    #[test]
    fn a_negated_check_and_an_insert_on_different_maps_are_not_a_pair() {
        let src = "use std::collections::HashMap;\n\
                   fn f(a: &HashMap<u32, u32>, b: &mut HashMap<u32, u32>, k: u32) {\n    \
                   if !a.contains_key(&k) {\n        b.insert(k, 1);\n    }\n}\n";
        assert!(kinds(src).is_empty());
    }

    #[test]
    fn a_negated_contains_key_without_an_insert_is_not_a_lead() {
        let src = "use std::collections::HashMap;\n\
                   fn f(m: &HashMap<u32, u32>, k: u32) {\n    \
                   if !m.contains_key(&k) {\n        let _ = k;\n    }\n}\n";
        assert!(kinds(src).is_empty());
    }

    // --- collect then iterate ----------------------------------------------

    #[test]
    fn collect_then_iterate_is_a_lead() {
        let src = "fn f(xs: &[u32]) -> u32 {\n    let v: Vec<u32> = xs.iter().map(|x| x * 2).collect();\n    \
                   let mut n = 0;\n    for x in v {\n        n += x;\n    }\n    n\n}\n";
        let lead = only(src);
        assert_eq!(lead.kind, "collect_then_iterate");
        assert_eq!(lead.line, 2);
        assert_eq!(lead.function, "f");
    }

    #[test]
    fn collect_that_is_stored_or_returned_is_not_a_lead() {
        let src = "fn f(xs: &[u32]) -> Vec<u32> {\n    let v: Vec<u32> = xs.iter().copied().collect();\n    v\n}\n";
        assert!(kinds(src).is_empty());
    }

    // --- baseline subtraction ----------------------------------------------

    #[test]
    fn a_pattern_the_baseline_already_had_is_not_new() {
        let baseline = "fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }";
        assert!(find(Some(baseline), baseline).is_empty());
    }

    #[test]
    fn an_extra_occurrence_in_an_already_smelly_function_is_a_lead() {
        let baseline = "fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }";
        let candidate = "fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); \
                         let _ = format!(\"{x}\"); } }";
        assert_eq!(find(Some(baseline), candidate).len(), 1);
    }

    #[test]
    fn a_lead_in_a_new_function_is_reported_even_beside_an_old_one() {
        let baseline = "fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }";
        let candidate = "fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n\
                         fn g(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }";
        let leads = find(Some(baseline), candidate);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].function, "g");
    }

    #[test]
    fn a_new_file_is_compared_against_nothing() {
        let candidate = "fn f(xs: &[u32]) { for x in xs { let _ = x.to_string(); } }";
        assert_eq!(find(None, candidate).len(), 1);
    }

    #[test]
    fn an_unparsable_candidate_yields_no_leads_rather_than_an_error() {
        assert!(scan("fn f( {").is_empty());
    }

    // --- the git-backed entry point ----------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The wiring that matters in practice: only in-scope `.rs` sources, only
    // patterns the candidate introduced, and never a frozen test or bench
    // file — a lead pointing there would invite a FAIL.
    #[test]
    fn for_changes_reports_only_new_smells_in_in_scope_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::create_dir_all(dir.path().join("tests")).expect("tests");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n",
        )
        .expect("baseline src");
        std::fs::write(
            dir.path().join("tests/it.rs"),
            "fn t(xs: &[u32]) { for x in xs { let _ = x.to_string(); } }\n",
        )
        .expect("baseline test");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);
        let commit = gitx::head_commit(dir.path()).expect("head");

        // One more smell in the source, a new smell in the test file.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n\
             pub fn g(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n",
        )
        .expect("candidate src");
        std::fs::write(
            dir.path().join("tests/it.rs"),
            "fn t(xs: &[u32]) { for x in xs { let _ = x.to_string(); let _ = x.to_string(); } }\n",
        )
        .expect("candidate test");

        let matcher = scope::Matcher::new(&["**".to_string()]);
        let found = for_changes(dir.path(), &commit, &matcher).expect("leads");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].file, "src/lib.rs");
        assert_eq!(found[0].kind, "format_in_loop");
        assert_eq!(found[0].function, "g");
    }

    // The advice is subtracted against the ADVANCING measurement baseline,
    // never the frozen run anchor: a smell introduced and KEPT is part of
    // what the next experiment is measured against, so re-reporting it on
    // every later experiment would be noise that grows with the run. `eval`
    // therefore passes `baseline.measure_commit`; this pins the semantic by
    // asking the same question against the second commit.
    #[test]
    fn a_kept_smell_is_not_re_reported_against_the_advancing_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f(xs: &[u32]) -> u32 { xs.len() as u32 }\n",
        )
        .expect("first commit");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "first"]);
        let first = gitx::head_commit(dir.path()).expect("head");

        // The candidate introduces the smell, and the run KEEPs it.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f(xs: &[u32]) -> u32 {\n    let mut n = 0;\n    for x in xs {\n        \
             n += format!(\"{x}\").len();\n    }\n    n as u32\n}\n",
        )
        .expect("second commit");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "second"]);
        let second = gitx::head_commit(dir.path()).expect("head");

        let matcher = scope::Matcher::new(&["**".to_string()]);
        let against_first = for_changes(dir.path(), &first, &matcher).expect("leads");
        assert_eq!(against_first.len(), 1, "{against_first:?}");
        assert_eq!(against_first[0].kind, "format_in_loop");
        assert_eq!(against_first[0].function, "f");

        // Still measuring against the kept second commit, so the same smell
        // is old news even though the file has an uncommitted edit.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f(xs: &[u32]) -> u32 {\n    let mut n = 0;\n    for x in xs {\n        \
             n += format!(\"{x}\").len();\n    }\n    n as u32\n}\n\npub fn g() -> u32 { 1 }\n",
        )
        .expect("uncommitted edit");
        let against_second = for_changes(dir.path(), &second, &matcher).expect("leads");
        assert!(against_second.is_empty(), "{against_second:?}");
    }

    // I5: both skip branches the plan claims coverage for, in one place. An
    // out-of-scope source is not the agent's to fix, a file whose stem is
    // `tests` is a split-out `#[cfg(test)] mod tests;` target and so frozen,
    // and a deleted file has nothing to hint at. None of them may turn into
    // an `Err` or a lead.
    #[test]
    fn for_changes_ignores_out_of_scope_sources_and_deleted_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        std::fs::create_dir_all(dir.path().join("src/foo")).expect("src/foo");
        std::fs::create_dir_all(dir.path().join("xtask/src")).expect("xtask");
        std::fs::write(dir.path().join("src/in.rs"), "pub fn f() -> u32 { 1 }\n").expect("in");
        std::fs::write(
            dir.path().join("src/gone.rs"),
            "pub fn gone() -> u32 { 2 }\n",
        )
        .expect("gone");
        std::fs::write(dir.path().join("src/foo/tests.rs"), "fn t() -> u32 { 3 }\n")
            .expect("split test module");
        std::fs::write(
            dir.path().join("xtask/src/helper.rs"),
            "pub fn h() -> u32 { 4 }\n",
        )
        .expect("out of scope");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);
        let commit = gitx::head_commit(dir.path()).expect("head");

        std::fs::write(
            dir.path().join("src/in.rs"),
            "pub fn f() -> u32 { 1 }\n\
             pub fn g(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n",
        )
        .expect("candidate");
        std::fs::write(
            dir.path().join("src/foo/tests.rs"),
            "fn t(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n",
        )
        .expect("candidate split test module");
        std::fs::write(
            dir.path().join("xtask/src/helper.rs"),
            "pub fn h(xs: &[u32]) { for x in xs { let _ = format!(\"{x}\"); } }\n",
        )
        .expect("candidate out of scope");
        std::fs::remove_file(dir.path().join("src/gone.rs")).expect("delete");

        let matcher = scope::Matcher::new(&["src/**".to_string()]);
        let found = for_changes(dir.path(), &commit, &matcher).expect("leads");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].file, "src/in.rs");
        assert_eq!(found[0].function, "g");
    }
}
