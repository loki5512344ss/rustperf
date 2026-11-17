//! Two implementations of the same small formatting job, one allocating per
//! item and one reusing a caller-owned buffer.
//!
//! This is the workload `benches/stats.rs` measures. It is deliberately tiny
//! and dependency-free: the point of the crate is the allocation-counting
//! harness around it, not the work itself, and a reader should be able to
//! follow the whole thing in one sitting.

/// Renders `name=count;` for every item into a **fresh** `String`, allocating
/// a temporary `String` per item along the way.
pub fn render_allocating(items: &[(&str, u64)]) -> String {
    let mut out = String::new();
    for (name, count) in items {
        out.push_str(&format!("{name}={count};"));
    }
    out
}

/// Renders exactly what [`render_allocating`] renders, into a buffer the
/// caller owns and reuses.
///
/// The buffer is cleared, not freed, so a caller that keeps it across calls
/// pays for at most the growth of one allocation; `write!` formats straight
/// into it instead of building a temporary `String` per item.
pub fn render_reusing(buf: &mut String, items: &[(&str, u64)]) {
    use std::fmt::Write;

    buf.clear();
    for (name, count) in items {
        // Writing into a `String` cannot fail, so the result carries no
        // information a caller could act on.
        let _ = write!(buf, "{name}={count};");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(&'static str, u64)> {
        vec![("alpha", 3), ("beta", 0), ("gamma", 1_000_000)]
    }

    #[test]
    fn the_two_implementations_render_the_same_text() {
        let items = sample();
        let owned = render_allocating(&items);
        let mut buf = String::new();
        render_reusing(&mut buf, &items);
        assert_eq!(owned, buf);
        assert_eq!(owned, "alpha=3;beta=0;gamma=1000000;");
    }

    #[test]
    fn reusing_a_buffer_replaces_its_contents_rather_than_appending() {
        let items = sample();
        let mut buf = String::from("leftover text that must not survive");
        render_reusing(&mut buf, &items);
        assert_eq!(buf, render_allocating(&items));
        // Second call over the same buffer must produce the same output too.
        render_reusing(&mut buf, &[("only", 1)]);
        assert_eq!(buf, "only=1;");
    }

    #[test]
    fn an_empty_input_renders_nothing() {
        assert_eq!(render_allocating(&[]), "");
        let mut buf = String::from("stale");
        render_reusing(&mut buf, &[]);
        assert_eq!(buf, "");
    }
}
