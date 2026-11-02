//! Formulas as values (`Value::Sym`): the bridge between the evaluator and the symbolic
//! module. In SMath, a name with no value is a symbol, so `f := x[1]^2 + 1` with x undefined
//! is a formula, and a function built from it works out numbers once x is given a value.
//!
//! An indexed name such as `x[2]` is one symbol inside the symbolic module (named `x[2]`),
//! and is turned back into an index expression on the way out. Every formula is simplified
//! before it is returned, and one larger than [`MAX_NODES`] (or nested deeper than the
//! parser accepts) is an error rather than a hang. Each call works within a budget
//! ([`super::work`]), so a formula that explodes stops quickly with an error.

use std::f64::consts::{E, PI};

use quire_ast::{BinaryOp, Expr, UnaryOp, parse, print};

use super::{Ctx, S, add, approx, diff, from_expr, func, mul, neg, pow, subs, to_expr, work};
use crate::error::{EvalError, Result, bail};
use crate::value::{Matrix, Value};

/// The largest formula kept, in nodes of the symbolic tree.
const MAX_NODES: usize = 4000;

/// Whether a value is or holds a formula (a matrix with a formula inside counts).
pub(crate) fn has_sym(v: &Value) -> bool {
    match v {
        Value::Sym(_) => true,
        Value::Matrix(m) => m.data.iter().any(has_sym),
        Value::Num(_) | Value::Str(_) => false,
    }
}

/// The first name left in a formula: the one to report when the formula is shown with `=`,
/// which asks for a number (SMath says the variable is not defined).
pub(crate) fn free_name(v: &Value) -> Option<String> {
    let found = std::cell::RefCell::new(None);
    let e = expr_of(v).ok()?;
    rewrite(&e, &|x| {
        if let Expr::Ident(n) = x {
            found.borrow_mut().get_or_insert_with(|| n.clone());
        }
        None
    });
    found.into_inner()
}

/// The name of the element `base[idx]` as a symbol.
pub(crate) fn element_name(base: &str, idx: &[Expr]) -> String {
    print(&Expr::Index(Box::new(Expr::Ident(base.to_string())), idx.to_vec()))
}

/// A whole number as an expression: `3`, or `-3` for a negative number.
fn literal(x: f64) -> Result<Expr> {
    if !x.is_finite() {
        bail!("a formula cannot hold {x}");
    }
    let magnitude = Expr::Num(format!("{}", x.abs()));
    Ok(if x < 0.0 { Expr::Unary(UnaryOp::Neg, Box::new(magnitude)) } else { magnitude })
}

/// An index as an expression: a whole number, or a formula that is itself an index.
pub(crate) fn index_expr(v: &Value) -> Result<Expr> {
    match v {
        Value::Num(_) => literal(v.as_int("an index")? as f64),
        Value::Sym(e) => Ok(e.clone()),
        other => bail!("an index must be a whole number, not {}", other.kind()),
    }
}

/// `x[i]` for a formula `x` that has no value: the element as a symbol.
pub(crate) fn element(base: &Expr, ix: &[Value]) -> Result<Value> {
    let Expr::Ident(name) = base else { bail!("only a named formula can be indexed") };
    let idx = ix.iter().map(index_expr).collect::<Result<Vec<_>>>()?;
    bounded(Expr::Index(Box::new(Expr::Ident(name.clone())), idx))
}

/// A formula as a value. Values are walked recursively (evaluated, printed, put into other
/// formulas), so they are kept within the depth the parser accepts: `x[x[x[…]]]` built in a
/// loop stops there.
fn bounded(e: Expr) -> Result<Value> {
    if e.depth() > quire_ast::MAX_DEPTH {
        bail!("the expression grew too large");
    }
    Ok(Value::Sym(e))
}

/// `e` rebuilt bottom-up: where `f` gives a replacement for a node, that replaces the node.
pub(crate) fn rewrite(e: &Expr, f: &dyn Fn(&Expr) -> Option<Expr>) -> Expr {
    if let Some(r) = f(e) {
        return r;
    }
    let go = |x: &Expr| rewrite(x, f);
    match e {
        Expr::Paren(x) => Expr::Paren(Box::new(go(x))),
        Expr::Unary(op, x) => Expr::Unary(*op, Box::new(go(x))),
        Expr::Factorial(x) => Expr::Factorial(Box::new(go(x))),
        Expr::Binary(op, a, b) => Expr::Binary(*op, Box::new(go(a)), Box::new(go(b))),
        Expr::Range(a, b) => Expr::Range(Box::new(go(a)), Box::new(go(b))),
        Expr::Compare(a, rest) => {
            Expr::Compare(Box::new(go(a)), rest.iter().map(|(op, x)| (*op, go(x))).collect())
        }
        Expr::Call(name, xs) => Expr::Call(name.clone(), xs.iter().map(&go).collect()),
        Expr::Index(b, xs) => Expr::Index(Box::new(go(b)), xs.iter().map(&go).collect()),
        Expr::Matrix(rows) => Expr::Matrix(rows.iter().map(|r| r.iter().map(&go).collect()).collect()),
        Expr::Define { lhs, rhs, global } => {
            Expr::Define { lhs: Box::new(go(lhs)), rhs: Box::new(go(rhs)), global: *global }
        }
        Expr::Num(_) | Expr::Ident(_) | Expr::Unit(_) | Expr::Str(_) | Expr::Placeholder => e.clone(),
    }
}

/// The value as a formula expression, for writing into a function body. Matrices become
/// matrix literals; a number that is exactly π or e stays the constant.
pub(crate) fn expr_of(v: &Value) -> Result<Expr> {
    match v {
        Value::Sym(e) => Ok(e.clone()),
        Value::Num(q) => {
            if !q.dim.is_dimensionless() {
                bail!("symbols with units are not supported yet");
            }
            if q.v.to_bits() == E.to_bits() {
                return Ok(Expr::Ident("e".into()));
            }
            if q.v.to_bits() == PI.to_bits() {
                return Ok(Expr::Ident("pi".into()));
            }
            literal(q.v)
        }
        Value::Matrix(m) => {
            if m.rows == 0 || m.cols == 0 {
                bail!("an empty matrix cannot be part of a formula");
            }
            let rows = m
                .data
                .chunks(m.cols)
                .map(|row| row.iter().map(expr_of).collect::<Result<Vec<_>>>())
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Matrix(rows))
        }
        Value::Str(_) => bail!("text cannot be part of a formula"),
    }
}

/// A number as a symbolic constant: π and e stay symbols, whole numbers stay exact.
fn num_s(x: f64) -> S {
    if x.to_bits() == E.to_bits() {
        return S::Sym("e".into());
    }
    if x.to_bits() == PI.to_bits() {
        return S::Sym("π".into());
    }
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return super::n(x as i128);
    }
    S::N(super::Num::Float(x))
}

/// A value as a symbolic expression. Only plain numbers and formulas can take part.
pub(crate) fn to_s(v: &Value) -> Result<S> {
    match v {
        Value::Num(q) => {
            if !q.dim.is_dimensionless() {
                bail!("symbols with units are not supported yet");
            }
            Ok(num_s(q.v))
        }
        Value::Sym(e) => {
            let none = |_: &str, _: usize| -> Option<(Vec<String>, Expr)> { None };
            let mut cx = Ctx::new(&none);
            let s = from_expr(&mangle(e), &mut cx).map_err(EvalError::new)?;
            check_size(&s)?;
            Ok(s)
        }
        Value::Matrix(_) => bail!("a matrix cannot be used inside a formula"),
        Value::Str(_) => bail!("text cannot be part of a formula"),
    }
}

/// Indexed names are symbols with the index in their name (`x[2]`) inside the symbolic
/// module. Other index nodes cannot be read there, so they are rejected by `from_expr`.
fn mangle(e: &Expr) -> Expr {
    rewrite(e, &|node| match node {
        Expr::Index(b, idx) => match b.as_ref() {
            Expr::Ident(name) => Some(Expr::Ident(element_name(name, idx))),
            _ => None,
        },
        _ => None,
    })
}

/// Undoes [`mangle`]: a symbol named `x[2]` becomes the index node `x[2]`.
fn remangle(e: Expr) -> Expr {
    rewrite(&e, &|node| match node {
        Expr::Ident(name) if name.contains('[') => parse(name).ok(),
        _ => None,
    })
}

/// Refuses a formula that has grown past [`MAX_NODES`] or too deep to walk.
pub(crate) fn check_size(s: &S) -> Result<()> {
    let (depth, nodes) = work::measure(s);
    if nodes > MAX_NODES || depth > work::MAX_DEPTH {
        bail!("the expression grew too large");
    }
    Ok(())
}

/// Symbolic work within a budget: an error, not a hang, when it explodes.
fn budgeted<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    work::budgeted(|| f().map_err(|e| e.message)).map_err(EvalError::new)
}

/// A symbolic result as a value: a number when nothing is left unknown, else a formula
/// (simplified, and with a negative product bracketed as SMath writes it: `-(2*x)`).
pub(crate) fn from_s(s: &S) -> Result<Value> {
    check_size(s)?;
    let shown = super::tidy(&super::simplify(s));
    if work::shown_depth(&shown) > work::MAX_SHOWN_DEPTH {
        bail!("the expression grew too large");
    }
    if let Some(x) = approx(&shown, &|_: &str| None).filter(|x| x.is_finite()) {
        return Ok(Value::num(x));
    }
    // -x and -sin(x) stay as they are; a negative product is bracketed: -(2*x)
    let positive = neg(shown.clone());
    let e = match (&shown, &positive) {
        (S::Mul(_), S::Mul(_)) if super::negative(&shown) => {
            Expr::Unary(UnaryOp::Neg, Box::new(Expr::Paren(Box::new(to_expr(&positive)))))
        }
        _ => to_expr(&shown),
    };
    bounded(remangle(e))
}

/// `a op b` where at least one side is a formula.
pub(crate) fn arith_sym(op: BinaryOp, a: &Value, b: &Value) -> Result<Value> {
    budgeted(|| arith_within(op, a, b))
}

fn arith_within(op: BinaryOp, a: &Value, b: &Value) -> Result<Value> {
    let (x, y) = (to_s(a)?, to_s(b)?);
    let s = match op {
        BinaryOp::Add => add(vec![x, y]),
        BinaryOp::Sub => add(vec![x, neg(y)]),
        BinaryOp::Mul | BinaryOp::Juxt => mul(vec![x, y]),
        BinaryOp::Div => {
            if matches!(&y, S::N(q) if q.is_zero()) {
                bail!("division by zero");
            }
            super::div(x, y)
        }
        BinaryOp::Pow => pow(x, y),
        other => bail!("'{}' is not defined for a formula", other.symbol().trim()),
    };
    from_s(&s)
}

/// Elementary functions of one formula: `sin(x)`, `exp(x)`, `sqrt(x)`. `None` when the call
/// is not one of these with a formula argument, so the usual builtins take it.
pub(crate) fn call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let [a @ Value::Sym(_)] = args else { return None };
    let elementary = matches!(
        name,
        "sqrt"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "exp"
            | "ln"
            | "abs"
    );
    if !elementary {
        return None;
    }
    Some(budgeted(|| {
        let s = to_s(a)?;
        let built = if name == "sqrt" { pow(s, S::N(super::Num::rat(1, 2))) } else { func(name, vec![s]) };
        from_s(&built)
    }))
}

/// The derivative of a value with respect to the symbol `var`, `order` times. A matrix is
/// differentiated element by element.
pub(crate) fn diff_value(v: &Value, var: &str, order: u32) -> Result<Value> {
    match v {
        Value::Matrix(m) => {
            let data = m.data.iter().map(|e| diff_value(e, var, order)).collect::<Result<Vec<_>>>()?;
            Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
        }
        _ => budgeted(|| {
            let mut s = to_s(v)?;
            for _ in 0..order {
                check_size(&s)?;
                s = diff(&s, var);
            }
            from_s(&s)
        }),
    }
}

/// The value with the symbol `var` replaced by the number `x`.
pub(crate) fn at_point(v: &Value, var: &str, x: f64) -> Result<Value> {
    match v {
        Value::Matrix(m) => {
            let data = m.data.iter().map(|e| at_point(e, var, x)).collect::<Result<Vec<_>>>()?;
            Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
        }
        _ => budgeted(|| from_s(&subs(&to_s(v)?, var, &num_s(x)))),
    }
}

/// Writes the formulas of globals into a function body, so that each call works them out
/// again with its own arguments. `lookup` gives the replacement for a name, if any.
pub(crate) fn inline(body: &Expr, lookup: &dyn Fn(&str) -> Option<Expr>) -> Expr {
    rewrite(body, &|node| match node {
        Expr::Ident(name) => lookup(name),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Quantity;
    use quire_units::Dim;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn sym(text: &str) -> Result<Value> {
        Ok(Value::Sym(parse(text)?))
    }

    fn text(v: &Value) -> String {
        match v {
            Value::Sym(e) => print(e),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn indexed_names_round_trip_through_the_symbolic_module() -> TestResult {
        let v = sym("2*x[2]^3 + x[1]")?;
        let s = to_s(&v)?;
        assert_eq!(text(&from_s(&s)?), "2*x[2]^3 + x[1]");
        Ok(())
    }

    #[test]
    fn a_formula_that_cancels_is_a_number() -> TestResult {
        let two = S::N(super::super::Num::int(2));
        let v = from_s(&add(vec![two.clone(), neg(two)]))?;
        assert_eq!(v, Value::num(0.0));
        Ok(())
    }

    #[test]
    fn arithmetic_collects_like_terms() -> TestResult {
        let x = sym("x")?;
        let two_x = arith_sym(BinaryOp::Mul, &Value::num(2.0), &x)?;
        let total = arith_sym(BinaryOp::Add, &two_x, &x)?;
        assert_eq!(text(&total), "3*x");
        Ok(())
    }

    #[test]
    fn a_negative_product_is_written_with_brackets() -> TestResult {
        let x = sym("x")?;
        let v = arith_sym(BinaryOp::Mul, &Value::num(-24.0), &x)?;
        assert_eq!(text(&v), "-(24*x)");
        Ok(())
    }

    #[test]
    fn derivatives_are_taken_with_respect_to_an_indexed_name() -> TestResult {
        let f = sym("x[1]^2*x[2]^3 - 5*x[1] + x[2]")?;
        let d = diff_value(&f, "x[1]", 1)?;
        assert_eq!(text(&d), "2*x[1]*x[2]^3 - 5");
        let dd = diff_value(&d, "x[2]", 1)?;
        assert_eq!(text(&dd), "6*x[1]*x[2]^2");
        Ok(())
    }

    #[test]
    fn a_derivative_without_symbols_is_a_number() -> TestResult {
        let f = sym("x^3")?;
        let d = diff_value(&f, "x", 3)?;
        assert_eq!(d, Value::num(6.0));
        Ok(())
    }

    #[test]
    fn a_value_is_put_into_a_formula_at_a_point() -> TestResult {
        let f = sym("x^2 + 1")?;
        assert_eq!(at_point(&f, "x", 3.0)?, Value::num(10.0));
        Ok(())
    }

    #[test]
    fn euler_number_stays_a_symbol() -> TestResult {
        let v = arith_sym(BinaryOp::Pow, &Value::num(E), &sym("z")?)?;
        assert_eq!(text(&v), "e^z");
        Ok(())
    }

    #[test]
    fn elementary_functions_of_a_formula_stay_symbolic() -> TestResult {
        let x = sym("x")?;
        let r = call("sin", std::slice::from_ref(&x)).ok_or("no result")??;
        assert_eq!(text(&r), "sin(x)");
        assert!(call("cot", std::slice::from_ref(&x)).is_none());
        Ok(())
    }

    #[test]
    fn units_are_refused() {
        let length = Quantity::new(2.0, Dim::from_ints([1, 0, 0, 0, 0, 0, 0]));
        let v = Value::Num(length);
        assert!(arith_sym(BinaryOp::Add, &v, &Value::Sym(Expr::Ident("x".into()))).is_err());
    }

    #[test]
    fn a_formula_that_grows_too_large_is_an_error() -> TestResult {
        // each factor has a new name, so nothing collects and the tree grows without bound
        let mut s = to_s(&sym("x")?)?;
        let mut failed = false;
        for k in 0..MAX_NODES {
            let factor = add(vec![S::Sym(format!("y{k}")), super::super::n(1)]);
            s = mul(vec![s, factor]);
            if check_size(&s).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        Ok(())
    }
}
