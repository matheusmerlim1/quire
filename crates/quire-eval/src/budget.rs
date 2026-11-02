//! Limits on how much work and memory one evaluation may take, so that any document (they
//! arrive in links) gives an error rather than hanging or exhausting memory.
//!
//! The step count is per thread: an [`crate::Interp`] sets its limit while it evaluates, and
//! the matrix code, which has no interpreter at hand, charges its work against that limit.

use std::cell::Cell;

use crate::error::{EvalError, Result};

/// Steps one interpreter may take: a call, a loop iteration, or a share of a large matrix
/// operation (see [`charge`]).
pub(crate) const MAX_STEPS: u64 = 20_000_000;

/// Largest matrix (or range) built, in elements. A value takes some 64 bytes, so this keeps
/// one matrix to tens of megabytes.
pub(crate) const MAX_ELEMENTS: usize = 1_000_000;

thread_local! {
    /// Steps taken on this thread, ever; limits are absolute values of it.
    static SPENT: Cell<u64> = const { Cell::new(0) };
    /// The limit of the evaluation in progress (none outside one).
    static LIMIT: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// The limit for an evaluation starting now.
pub(crate) fn limit_from_now() -> u64 {
    SPENT.with(Cell::get).saturating_add(MAX_STEPS)
}

/// Runs `f` with `limit` as the step limit of anything it charges.
pub(crate) fn within<T>(limit: u64, f: impl FnOnce() -> T) -> T {
    let outer = LIMIT.with(|l| l.replace(limit));
    let r = f();
    LIMIT.with(|l| l.set(outer));
    r
}

/// Takes `n` steps; an error once the evaluation's limit is passed.
pub(crate) fn tick(n: u64, limit: u64) -> Result<()> {
    let spent = SPENT.with(|s| {
        let v = s.get().saturating_add(n);
        s.set(v);
        v
    });
    if spent > limit { Err(stopped()) } else { Ok(()) }
}

/// Charges work done outside the interpreter (matrix products, eliminations), counted in
/// multiply-adds: `per_step` of them make a step. Called before the work starts, so a product
/// too large for the budget fails at once instead of after running for minutes.
pub(crate) fn charge(multiply_adds: u64, per_step: u64) -> Result<()> {
    tick(multiply_adds / per_step.max(1), LIMIT.with(Cell::get))
}

#[cold]
fn stopped() -> EvalError {
    EvalError::new(format!(
        "evaluation stopped after {MAX_STEPS} steps: this takes too long (an endless loop, or a matrix too large?)"
    ))
}

/// Multiply-adds of a least-squares fit of `rows` points to `cols` functions.
pub(crate) fn least_squares_work(rows: usize, cols: usize) -> u64 {
    (rows as u64).saturating_mul(cols as u64).saturating_mul(cols as u64)
}

/// Refuses a `rows`×`cols` matrix larger than [`MAX_ELEMENTS`], before it is allocated.
pub(crate) fn check_size(rows: usize, cols: usize) -> Result<()> {
    match rows.checked_mul(cols) {
        Some(n) if n <= MAX_ELEMENTS => Ok(()),
        _ => Err(EvalError::new(format!(
            "a {rows}×{cols} matrix is too large (at most {MAX_ELEMENTS} elements)"
        ))),
    }
}

/// Every limit above, and the others an untrusted document can reach (nesting, unit
/// exponents, symbolic work), seen from a worksheet: an error, quickly, and never a crash.
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::{DisplaySettings, DisplaySpec, MathInput, TableInput, Worksheet};

    /// Runs `lines` (a line ending in ` →` is symbolic) on a thread with the web version's
    /// stack, 1 MB; unoptimised builds use several times more stack per call, so they get 8 MB.
    /// Returns what each line shows, or `!` and its error. Fails if it takes over `secs`.
    fn run_in(secs: u64, marked: bool, lines: &[&str]) -> Vec<String> {
        let regions: Vec<MathInput> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| MathInput {
                id: i.to_string(),
                expr: l.strip_suffix(" →").unwrap_or(l).to_string(),
                show: true,
                display: DisplaySpec::default(),
                table: None,
                plot: None,
                symbolic: l.ends_with(" →"),
            })
            .collect();
        let size = if cfg!(debug_assertions) { 8 << 20 } else { 1 << 20 };
        let start = Instant::now();
        let run = std::thread::Builder::new().stack_size(size).spawn(move || {
            let settings = DisplaySettings { marked_units: marked, ..Default::default() };
            Worksheet::new(settings).run(&regions)
        });
        let Ok(Ok(out)) = run.map(|h| h.join()) else { std::process::abort() };
        let took = start.elapsed();
        assert!(took < Duration::from_secs(secs), "took {took:?}");
        out.into_iter()
            .map(|o| match (o.error, o.shown) {
                (Some(e), _) => format!("!{e}"),
                (None, s) => s.unwrap_or_default(),
            })
            .collect()
    }

    fn run(lines: &[&str]) -> Vec<String> {
        run_in(3, false, lines)
    }

    fn nested(open: &str, close: &str, n: usize) -> String {
        format!("{}x{}", open.repeat(n), close.repeat(n))
    }

    fn fails_with(out: &[String], i: usize, words: &str) -> bool {
        out.get(i).is_some_and(|s| s.starts_with('!') && s.contains(words))
    }

    #[test]
    fn expressions_nested_up_to_the_parser_limit_evaluate() {
        let n = quire_ast::MAX_DEPTH - 1;
        let sum = vec!["x"; n + 1].join(" + ");
        let lines = [
            "x := 2".to_string(),
            nested("(", ")", n),
            nested("abs(", ")", n),
            format!("{}x", "-".repeat(n - 1)),
            sum.clone(),
            format!("{} →", nested("(", ")", n)),
            format!("{sum} →"),
            format!("{} →", nested("sin(", ")", n)),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let out = run(&lines);
        assert_eq!(out[1..5], ["2", "2", "2", "400"]);
        // → keeps x a symbol only when it has no value; here it is worked out
        assert!(out[5..].iter().all(|s| !s.starts_with('!')), "{out:?}");
    }

    #[test]
    fn deeper_nesting_and_long_chains_are_errors() {
        let lines =
            [nested("(", ")", 3000), vec!["1"; 30_000].join("+"), format!("{} →", nested("(", ")", 3000))];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let out = run(&lines);
        assert!((0..3).all(|i| fails_with(&out, i, "too deeply nested")), "{out:?}");
    }

    #[test]
    fn recursion_works_to_its_limit_and_stops_there() {
        let out = run(&["g(n) := if(n < 1, 0, 1 + g(n - 1))", "g(250)", "g(300)"]);
        assert_eq!(out[1], "250");
        assert!(fails_with(&out, 2, "calls itself too deeply"), "{out:?}");
    }

    #[test]
    fn deep_function_bodies_calling_each_other_stop_cleanly() {
        // each call nests 150 brackets, so a few calls deep the evaluation is too deep
        let body = format!("h(n) := if(n < 1, 0, {})", nested("(", ")", 150).replace('x', "h(n - 1)"));
        let out = run(&[&body, "h(100)", &format!("{} →", nested("(", ")", 150).replace('x', "h(3)"))]);
        assert!(fails_with(&out, 1, "nests too deeply"), "{out:?}");
        assert!(out[2].starts_with('!'), "{out:?}");
    }

    #[test]
    fn unit_exponents_that_do_not_fit_are_errors() {
        let roots = nested("sqrt(", ")", 34).replace('x', "'m");
        let out =
            run(&[&roots, "('m^1000000)^1000000", "sqrt(sqrt('m^4))", "('m^1000000)^2000*('m^1000000)^2000"]);
        for i in [0, 1, 3] {
            assert!(fails_with(&out, i, "too large to keep exactly"), "{out:?}");
        }
        assert_eq!(out[2], "1 'm");
    }

    #[test]
    fn huge_matrix_results_are_refused_before_they_are_built() {
        let out = run(&[
            "(1..200000)*transpose(1..200000)",
            "identity(1500)",
            "v := 1..1000000",
            "stack(v, v, v)",
            "range(1, 2000000)",
        ]);
        assert!(fails_with(&out, 0, "too large"), "{out:?}");
        assert!(fails_with(&out, 1, "between 1 and 1000"), "{out:?}");
        assert!(fails_with(&out, 3, "too large"), "{out:?}");
        assert!(fails_with(&out, 4, "too many elements"), "{out:?}");
    }

    #[test]
    fn long_matrix_work_runs_out_of_steps_at_once() {
        let out = run(&[
            "identity(1000)*identity(1000)",
            "det(identity(1000))",
            "inverse(identity(1000))",
            "det(identity(50))",
        ]);
        for i in 0..3 {
            assert!(fails_with(&out, i, "stopped after"), "{out:?}");
        }
        assert_eq!(out[3], "1");
    }

    #[test]
    fn spreadsheet_ranges_stop_at_the_table() {
        let start = Instant::now();
        let t = TableInput {
            rows: vec![vec!["5".into(), "=SUM(A2:ZZZ999999) + COUNT(A1:A999999)".into()]],
            ..Default::default()
        };
        let input = MathInput {
            id: "t".into(),
            expr: String::new(),
            show: true,
            display: DisplaySpec::default(),
            table: Some(t),
            plot: None,
            symbolic: false,
        };
        let out = Worksheet::new(DisplaySettings::default()).run(&[input]);
        let cell = out.first().and_then(|o| o.cells.first()).and_then(|r| r.get(1)).cloned();
        assert_eq!(cell.and_then(|c| c.shown).as_deref(), Some("1"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn symbolic_work_that_explodes_stops_with_an_error() {
        let out = run(&["diff(x^x^x^x^x^x, x, 6) →", "diff(x, x, 1000000000) →", "expand((a + b)^12) →"]);
        assert!(fails_with(&out, 0, "too much to work out"), "{out:?}");
        assert!(out[1].starts_with('!'), "{out:?}");
        assert!(out[2].starts_with("a^12 + 12*a^11*b"), "{out:?}");
    }

    #[test]
    fn formulas_built_in_a_loop_stop_growing() {
        // SMath mode: names with no value are symbols, so these formulas grow each time round
        let out = run_in(3, true, &["for(i, 1..1000, y := x[y])", "for(i, 1..1000, z := sin(z))"]);
        assert!(fails_with(&out, 0, "grew too large"), "{out:?}");
        assert!(fails_with(&out, 1, "grew too large"), "{out:?}");
    }
}
