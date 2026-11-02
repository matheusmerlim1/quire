//! Limits on symbolic work, so that a formula that explodes (`diff(x^x^x^x^x^x, x, 6) →`)
//! gives an error quickly instead of running for ever or using all memory.
//!
//! The smart constructors and the canonical keys charge a work budget as they go. They
//! cannot fail, so once the budget is spent they return `0` at once: what is left of the
//! computation collapses quickly, and the entry point ([`budgeted`]) reports the error.
//! Trees are also kept shallow enough to walk recursively on a small stack.

use std::cell::Cell;

use super::{Res, S};

/// Units of work one symbolic evaluation may use: roughly a node built or looked at.
/// The largest results the tests work out take under a fifth of this; past it the cost of
/// each unit grows quickly (copies of ever larger trees), so it is kept no larger.
const MAX_WORK: u64 = 500_000;

/// Most terms in one sum, or factors in one product. A sum is shown as a chain
/// (`a + b + c` is `(a + b) + c`), so this also bounds the depth of what is printed.
pub(super) const MAX_TERMS: usize = 1000;

/// Deepest symbolic tree kept: twice what the parser accepts, so expanding the document's
/// functions into each other has room, while every recursive walk stays on a small stack.
pub(super) const MAX_DEPTH: usize = 2 * quire_ast::MAX_DEPTH;

/// Deepest expression shown as a result of `→`.
pub(super) const MAX_SHOWN_DEPTH: usize = 1000;

/// Deepest nesting of `from_expr`, the walk from the parsed expression into the engine.
pub(super) const MAX_NESTING: usize = 300;

pub(super) const TOO_MUCH_WORK: &str =
    "this is too much to work out symbolically: the expression grows too large";
pub(super) const TOO_DEEP: &str = "this expression nests too deeply to work out symbolically";

thread_local! {
    /// Work left in the evaluation in progress; `u64::MAX` outside one.
    static LEFT: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// Spends `n` units; `false` once the budget is used up (and from then on).
pub(super) fn spend(n: usize) -> bool {
    LEFT.with(|left| {
        let have = left.get();
        let n = n as u64;
        // outside a budgeted call (unit tests of the parts) nothing is counted
        if have == u64::MAX {
            return true;
        }
        if have <= n {
            left.set(0);
            false
        } else {
            left.set(have - n);
            true
        }
    })
}

/// Whether a sum or product of `n` parts is past [`MAX_TERMS`]; it then spends the whole
/// budget, so the evaluation fails. Outside a budgeted call there is no limit.
pub(super) fn too_many(n: usize) -> bool {
    let over = n > MAX_TERMS && LEFT.with(Cell::get) != u64::MAX;
    if over {
        LEFT.with(|left| left.set(0));
    }
    over
}

pub(super) fn exhausted() -> bool {
    LEFT.with(Cell::get) == 0
}

/// Runs `f` with a fresh budget, and fails if it ran out. Inside another budgeted call, `f`
/// shares that call's budget.
pub(crate) fn budgeted<T>(f: impl FnOnce() -> Res<T>) -> Res<T> {
    if LEFT.with(Cell::get) != u64::MAX {
        return f();
    }
    LEFT.with(|left| left.set(MAX_WORK));
    let r = f();
    let out = exhausted();
    LEFT.with(|left| left.set(u64::MAX));
    if out { Err(TOO_MUCH_WORK.into()) } else { r }
}

/// The children of a node.
fn children(e: &S) -> &[S] {
    match e {
        S::Add(xs) | S::Mul(xs) | S::Fn(_, xs) | S::Vec(xs) => xs,
        S::N(_) | S::Sym(_) | S::Unit(_) | S::Pow(..) => &[],
    }
}

/// Levels of nesting of `e` (a leaf is 1), and its number of nodes, worked out without
/// recursion so it is safe on any tree.
pub(super) fn measure(e: &S) -> (usize, usize) {
    let (mut deepest, mut nodes) = (0, 0);
    let mut stack = vec![(e, 1)];
    while let Some((e, d)) = stack.pop() {
        deepest = deepest.max(d);
        nodes += 1;
        if let S::Pow(b, p) = e {
            stack.push((b, d + 1));
            stack.push((p, d + 1));
        }
        stack.extend(children(e).iter().map(|x| (x, d + 1)));
    }
    (deepest, nodes)
}

/// Refuses a tree deeper than [`MAX_DEPTH`].
pub(super) fn check_depth(e: &S) -> Res<()> {
    if measure(e).0 > MAX_DEPTH { Err(TOO_DEEP.into()) } else { Ok(()) }
}

/// An upper bound on the depth of `to_expr(e)`: a sum or product of n parts becomes a chain
/// n deep, and a few nodes (a sign, a fraction bar) may wrap each part. Worked out without
/// recursion, so it can be asked before building an expression that would be too deep.
pub(super) fn shown_depth(e: &S) -> usize {
    enum Step<'a> {
        Visit(&'a S),
        /// Combine the last `count` results, adding `extra` levels.
        Join(usize, usize),
    }
    let mut steps = vec![Step::Visit(e)];
    let mut done: Vec<usize> = Vec::new();
    while let Some(step) = steps.pop() {
        match step {
            Step::Visit(e) => {
                let (extra, parts): (usize, Vec<&S>) = match e {
                    S::N(_) | S::Sym(_) | S::Unit(_) => (3, Vec::new()),
                    S::Add(xs) | S::Mul(xs) => (xs.len() + 3, xs.iter().collect()),
                    S::Fn(_, xs) | S::Vec(xs) => (3, xs.iter().collect()),
                    S::Pow(b, p) => (3, vec![&**b, &**p]),
                };
                steps.push(Step::Join(extra, parts.len()));
                steps.extend(parts.into_iter().map(Step::Visit));
            }
            Step::Join(extra, count) => {
                let deepest = done.split_off(done.len().saturating_sub(count)).into_iter().max();
                done.push(deepest.unwrap_or(0).saturating_add(extra));
            }
        }
    }
    done.pop().unwrap_or(0)
}
