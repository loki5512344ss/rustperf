//! The `report` command: summarizes `results.tsv`.

use crate::args::Args;
use rustperf_core::gitx;
use rustperf_core::results::{self, Row};

pub fn run(argv: &[String]) -> i32 {
    let args = match Args::parse(argv, &[("C", true), ("json", false)]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return rustperf_core::EXIT_USAGE;
        }
    };
    match report(&args) {
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

fn report(args: &Args) -> Result<String, String> {
    let root = gitx::root(&args.dir())?;
    let rows = results::load(&root.join(results::RESULTS_PATH))?;
    if args.flag("json") {
        let mut text = serde_json::to_string_pretty(&report_json(&rows))
            .map_err(|e| format!("encode report: {e}"))?;
        text.push('\n');
        return Ok(text);
    }
    Ok(format_report(&rows))
}

/// The same summary as [`format_report`], as one JSON object, for a caller
/// that would otherwise parse the text. Same arithmetic: the cumulative score
/// is the product of every kept row's score.
fn report_json(rows: &[Row]) -> serde_json::Value {
    let count = |status: &str| rows.iter().filter(|r| r.status == status).count();
    let kept: Vec<&Row> = rows.iter().filter(|r| r.status == "KEEP").collect();
    let cumulative: f64 = kept.iter().map(|r| r.score).product();
    let mut wins = kept.clone();
    wins.sort_by(|a, b| {
        a.best_bench_delta
            .partial_cmp(&b.best_bench_delta)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let wins: Vec<serde_json::Value> = wins
        .iter()
        .take(5)
        .map(|r| {
            serde_json::json!({
                "commit": r.commit,
                "description": r.description,
                "score": r.score,
                "best_bench_delta": r.best_bench_delta,
            })
        })
        .collect();
    let mut out = serde_json::json!({
        "experiments": rows.len(),
        "keep": count("KEEP"),
        "discard": count("DISCARD"),
        "fail": count("FAIL"),
        "crash": count("CRASH"),
        "largest_wins": wins,
    });
    if !kept.is_empty() {
        out["cumulative_score"] = serde_json::json!(cumulative);
        out["cumulative_speedup_pct"] = serde_json::json!((1.0 - cumulative) * 100.0);
        if cumulative > 0.0 {
            out["speedup_multiplier"] = serde_json::json!(1.0 / cumulative);
        }
    }
    out
}

/// Renders the summary. The cumulative speedup is the PRODUCT of every kept
/// row's score, not the latest.
///
/// Each kept score is only that experiment's own incremental contribution,
/// because `measure_commit` advances after every KEEP (see
/// `rustperf_core::pipeline::advance_measurement_baseline`): every `eval`
/// measures the candidate against the immediately preceding ACCEPTED state,
/// never against the run's original commit. So the run-level figure has to
/// be composed by multiplying every kept score together, the same way
/// compounding successive percentage changes works — taking the latest
/// alone would under-report every earlier kept experiment.
fn format_report(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "rustperf report: no experiments recorded\n".to_string();
    }

    let mut keep = 0;
    let mut discard = 0;
    let mut fail = 0;
    let mut crash = 0;
    for r in rows {
        match r.status.as_str() {
            "KEEP" => keep += 1,
            "DISCARD" => discard += 1,
            "FAIL" => fail += 1,
            "CRASH" => crash += 1,
            _ => {}
        }
    }

    let mut cumulative = 1.0f64;
    let mut kept_rows: Vec<&Row> = Vec::new();
    for r in rows {
        if r.status == "KEEP" {
            cumulative *= r.score;
            kept_rows.push(r);
        }
    }

    let mut out = format!(
        "rustperf report: {} experiment(s)  ({keep} keep, {discard} discard, {fail} \
         fail, {crash} crash)\n\n",
        rows.len()
    );

    if kept_rows.is_empty() {
        out.push_str("cumulative speedup: no experiments were kept\n");
    } else {
        let speedup_pct = (1.0 - cumulative) * 100.0;
        let multiplier = if cumulative > 0.0 {
            1.0 / cumulative
        } else {
            f64::INFINITY
        };
        out.push_str(&format!(
            "cumulative speedup: {speedup_pct:.1}%  (score {cumulative:.2}, {multiplier:.2}x \
             faster overall)\n"
        ));

        // Largest individual wins: every kept row, sorted by its own
        // best-benchmark delta (most negative — the biggest improvement —
        // first).
        let mut sorted = kept_rows.clone();
        sorted.sort_by(|a, b| {
            a.best_bench_delta
                .partial_cmp(&b.best_bench_delta)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.push_str("\nlargest wins:\n");
        for (i, r) in sorted.iter().take(5).enumerate() {
            let desc = if r.description.is_empty() {
                "(no description)"
            } else {
                &r.description
            };
            out.push_str(&format!(
                "  {}. {desc} (time: {:+.1}%)\n",
                i + 1,
                r.best_bench_delta
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, score: f64, best_bench_delta: f64, desc: &str) -> Row {
        Row {
            commit: "abc1234".into(),
            score,
            best_bench_delta,
            status: status.into(),
            description: desc.into(),
            version: "0.3.0".into(),
        }
    }

    #[test]
    fn an_empty_log_says_so() {
        assert!(format_report(&[]).contains("no experiments recorded"));
    }

    #[test]
    fn counts_and_the_product_of_kept_scores_are_reported() {
        let rows = vec![
            row("KEEP", 0.8, -20.0, "a"),
            row("KEEP", 0.5, -50.0, "b"),
            row("DISCARD", 1.0, 0.0, "c"),
            row("FAIL", 0.0, 0.0, "d"),
        ];
        let text = format_report(&rows);
        assert!(text.contains("2 keep"), "{text}");
        assert!(text.contains("1 discard"), "{text}");
        assert!(text.contains("1 fail"), "{text}");
        // 0.8 * 0.5 = 0.40, the PRODUCT rather than the latest kept score
        // (0.5) alone.
        assert!(text.contains("0.40"), "{text}");
    }

    #[test]
    fn json_report_matches_the_text_arithmetic() {
        let rows = vec![
            row("KEEP", 0.8, -20.0, "a"),
            row("KEEP", 0.5, -50.0, "b"),
            row("DISCARD", 1.0, 0.0, "c"),
        ];
        let v = report_json(&rows);
        assert_eq!(v["experiments"], 3);
        assert_eq!(v["keep"], 2);
        assert_eq!(v["discard"], 1);
        assert!((v["cumulative_score"].as_f64().unwrap() - 0.4).abs() < 1e-12);
        assert!((v["speedup_multiplier"].as_f64().unwrap() - 2.5).abs() < 1e-12);
        assert_eq!(v["largest_wins"][0]["description"], "b");
    }

    #[test]
    fn json_report_with_nothing_kept_omits_the_cumulative_fields() {
        let v = report_json(&[row("DISCARD", 1.0, 0.0, "c")]);
        assert!(v.get("cumulative_score").is_none());
        assert_eq!(v["keep"], 0);
    }

    #[test]
    fn no_kept_rows_says_so_rather_than_zero() {
        let rows = vec![row("DISCARD", 1.0, 0.0, "c")];
        let text = format_report(&rows);
        assert!(!text.contains("0.0%"), "{text}");
        assert!(
            text.to_lowercase().contains("no experiments were kept"),
            "{text}"
        );
    }
}
