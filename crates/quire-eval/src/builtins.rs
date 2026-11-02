//! Built-in functions whose arguments are evaluated first. Lazy constructs
//! (`if`, `for`, `sum`, `int`, …) live in `interp.rs`.

use quire_ast::BinaryOp;
use quire_units::{Dim, Rational};

use crate::budget::MAX_ELEMENTS;
use crate::error::{EvalError, Result, bail, internal};
use crate::format::{DisplaySettings, describe_dim, format_number};
use crate::ops::{self, arith, same_dim, uniform};
use crate::value::{Matrix, Quantity, Value};

pub(crate) fn arity(name: &str, args: &[Value], ok: impl Fn(usize) -> bool, expected: &str) -> Result<()> {
    if !ok(args.len()) {
        bail!("{name} takes {expected}, but got {}", args.len());
    }
    Ok(())
}

/// The `i`-th argument. Callers check the argument count first.
pub(crate) fn arg(args: &[Value], i: usize) -> Result<&Value> {
    args.get(i).ok_or_else(|| internal("a builtin argument"))
}

/// The `i`-th number of a list. Callers check the list's length first.
pub(crate) fn nth_value(v: &[f64], i: usize) -> Result<f64> {
    v.get(i).copied().ok_or_else(|| internal("a list value"))
}

/// The `i`-th number of a list, for writing. Callers build the list to a known length.
pub(crate) fn slot(v: &mut [f64], i: usize) -> Result<&mut f64> {
    v.get_mut(i).ok_or_else(|| internal("a list value"))
}

fn real(name: &str, f: fn(f64) -> f64, args: &[Value]) -> Result<Value> {
    arity(name, args, |n| n == 1, "1 argument")?;
    let x = arg(args, 0)?.as_scalar(&format!("the argument of {name}"))?;
    let v = f(x);
    if v.is_nan() && !x.is_nan() {
        bail!("{name}({x}) has no real result");
    }
    // ∞ in, ∞ out is fine (exp(∞)); from a finite x it is overflow or a pole
    if v.is_infinite() && x.is_finite() {
        if matches!(name, "exp" | "sinh" | "cosh") {
            bail!("{name}({x}) is too large: results go up to about 1.8e308");
        }
        bail!("{name}({x}) is infinite");
    }
    Ok(Value::num(v))
}

fn matrix_arg<'a>(name: &str, v: &'a Value) -> Result<&'a Matrix> {
    match v {
        Value::Matrix(m) => Ok(m),
        other => bail!("{name} needs a matrix, not {}", other.kind()),
    }
}

/// Numbers from every argument, matrices flattened.
pub(crate) fn flatten(args: &[Value]) -> Vec<&Value> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Value::Matrix(m) => out.extend(m.data.iter()),
            v => out.push(v),
        }
    }
    out
}

fn extremum(name: &str, args: &[Value], pick_max: bool) -> Result<Value> {
    let items = flatten(args);
    let Some(first) = items.first() else { bail!("{name} needs at least one value") };
    let mut best = first.as_num(&format!("an argument of {name}"))?;
    for v in items.iter().skip(1) {
        let q = v.as_num(&format!("an argument of {name}"))?;
        same_dim(best.dim, q.dim, &format!("take {name} of"))?;
        if (pick_max && q.v > best.v) || (!pick_max && q.v < best.v) {
            best = q;
        }
    }
    Ok(Value::Num(best))
}

fn element(m: &Matrix, idx: &[i64]) -> Result<Value> {
    let (r, c) = match *idx {
        [i] if m.cols == 1 => (i, 1),
        [i] if m.rows == 1 => (1, i),
        [_] => bail!("a {}×{} matrix needs two indices", m.rows, m.cols),
        [i, j] => (i, j),
        _ => bail!("too many indices"),
    };
    if r < 1 || c < 1 || r as usize > m.rows || c as usize > m.cols {
        bail!("index ({r}, {c}) is outside the {}×{} matrix (indices start at 1)", m.rows, m.cols);
    }
    Ok(m.at(r as usize - 1, c as usize - 1)?.clone())
}

pub fn index(base: &Value, idx: &[Value]) -> Result<Value> {
    let m = matrix_arg("indexing", base)?;
    let idx = idx.iter().map(|v| v.as_int("an index")).collect::<Result<Vec<_>>>()?;
    element(m, &idx)
}

fn concat_blocks(name: &str, args: &[Value], vertical: bool) -> Result<Value> {
    // the size of the result, before any block is copied
    let total = args.iter().try_fold(0usize, |n, v| {
        n.checked_add(match v {
            Value::Matrix(m) => m.data.len(),
            _ => 1,
        })
    });
    if total.is_none_or(|n| n > crate::budget::MAX_ELEMENTS) {
        bail!("{name}: the result is too large (at most {} elements)", crate::budget::MAX_ELEMENTS);
    }
    let blocks: Vec<Matrix> = args
        .iter()
        .map(|v| match v {
            Value::Matrix(m) => m.clone(),
            other => Matrix::new(1, 1, vec![other.clone()]),
        })
        .collect();
    let Some(first) = blocks.first() else { bail!("{name} needs at least one argument") };
    if vertical {
        let cols = first.cols;
        if blocks.iter().any(|b| b.cols != cols) {
            bail!("{name}: all blocks need the same number of columns");
        }
        let rows = blocks.iter().map(|b| b.rows).sum();
        let data = blocks.into_iter().flat_map(|b| b.data).collect();
        return Ok(Value::Matrix(Matrix::new(rows, cols, data)));
    }
    if blocks.iter().any(|b| b.rows != first.rows) {
        bail!("{name}: all blocks need the same number of rows");
    }
    let cols = blocks.iter().map(|b| b.cols).sum();
    let mut data = Vec::with_capacity(first.rows * cols);
    for r in 0..first.rows {
        for b in &blocks {
            data.extend((0..b.cols).map(|c| b.at(r, c).cloned()).collect::<Result<Vec<_>>>()?);
        }
    }
    Ok(Value::Matrix(Matrix::new(first.rows, cols, data)))
}

/// `a..b` steps by ±1. With units there is no natural "1" (1 m would make `0 'mm..10 'mm` a
/// single element), so the caller passes the step: one of the unit written on the ends.
pub fn range(a: Quantity, b: Quantity, step: Option<Quantity>) -> Result<Value> {
    same_dim(a.dim, b.dim, "make a range from")?;
    if step.is_none() && !a.dim.is_dimensionless() {
        bail!(
            "a range of {} needs a step: write the unit on its ends (0 'mm..10 'mm steps by 1 mm) or give the step with range(start, end, step)",
            describe_dim(a.dim)
        );
    }
    let step = step.unwrap_or(Quantity::new(if b.v >= a.v { 1.0 } else { -1.0 }, a.dim));
    same_dim(a.dim, step.dim, "use a step for")?;
    if step.v == 0.0 || (b.v - a.v) * step.v < 0.0 {
        bail!("the range step never reaches the end value");
    }
    let count = ((b.v - a.v) / step.v + 1e-9).floor() + 1.0;
    if !count.is_finite() || count > MAX_ELEMENTS as f64 {
        bail!("the range has too many elements ({count:.3e}; at most {MAX_ELEMENTS})");
    }
    let n = count as usize;
    let data = (0..n).map(|k| Value::Num(Quantity::new(a.v + k as f64 * step.v, a.dim))).collect();
    Ok(Value::Matrix(Matrix::column(data)))
}

/// `None` when `name` is not a built-in.
pub fn call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let r = match name {
        "sqrt" => sqrt(args),
        "root" => root(args),
        "abs" => abs(args),
        "exp" => real(name, f64::exp, args),
        "ln" => real(name, f64::ln, args),
        "log10" | "lg" => real(name, f64::log10, args),
        "log" => log(args),
        "sin" => real(name, f64::sin, args),
        "cos" => real(name, f64::cos, args),
        "tan" => real(name, f64::tan, args),
        "cot" => real(name, |x| 1.0 / x.tan(), args),
        "sec" => real(name, |x| 1.0 / x.cos(), args),
        "csc" => real(name, |x| 1.0 / x.sin(), args),
        "asin" => real(name, f64::asin, args),
        "acos" => real(name, f64::acos, args),
        "atan" => real(name, f64::atan, args),
        "acot" => real(name, |x| (1.0 / x).atan(), args),
        "asec" => real(name, |x| (1.0 / x).acos(), args),
        "acsc" => real(name, |x| (1.0 / x).asin(), args),
        "sinh" => real(name, f64::sinh, args),
        "cosh" => real(name, f64::cosh, args),
        "tanh" => real(name, f64::tanh, args),
        "coth" => real(name, |x| 1.0 / x.tanh(), args),
        "asinh" => real(name, f64::asinh, args),
        "acosh" => real(name, f64::acosh, args),
        "atanh" => real(name, f64::atanh, args),
        "atan2" => atan2(args),
        "floor" => real(name, f64::floor, args),
        "ceil" => real(name, f64::ceil, args),
        "trunc" => real(name, f64::trunc, args),
        "round" => round(args),
        "sign" => sign(args),
        "mod" => mod_(args),
        "max" => extremum(name, args, true),
        "min" => extremum(name, args, false),
        "sum" if args.len() == 1 => sum_all(args),
        "mean" => mean(args),
        "sort" => sort(args),
        "norm" => norm(args),
        "dot" => dot(args),
        "cross" => cross(args),
        "lsolve" => lsolve(args),
        "linterp" => linterp(args),
        "rows" | "cols" | "length" => size(name, args),
        "el" => el(args),
        "transpose" => transpose(args),
        "det" => one_matrix(name, args).and_then(ops::det),
        "inverse" => one_matrix(name, args).and_then(ops::inverse),
        "identity" => identity(args),
        "stack" => concat_blocks(name, args, true),
        "augment" => concat_blocks(name, args, false),
        "mat" => mat(args),
        "range" => range_fn(args),
        "num2str" => num2str(args),
        "concat" => concat(args),
        "eval" => arity(name, args, |n| n == 1, "1 argument").and_then(|_| Ok(arg(args, 0)?.clone())),
        "sys" => crate::plot::sys(args),
        _ => return crate::linalg::call(name, args).or_else(|| crate::stats::call(name, args)),
    };
    Some(r)
}

fn sqrt(args: &[Value]) -> Result<Value> {
    arity("sqrt", args, |n| n == 1, "1 argument")?;
    let x = arg(args, 0)?.as_num("the argument of sqrt")?;
    if x.v < 0.0 {
        bail!("sqrt of a negative number (complex numbers are not supported yet)");
    }
    Ok(Value::Num(ops::pow(x, Quantity::scalar(0.5))?))
}

fn root(args: &[Value]) -> Result<Value> {
    arity("root", args, |n| n == 2, "2 arguments: root(x, n)")?;
    let x = arg(args, 0)?.as_num("the radicand")?;
    let n = arg(args, 1)?.as_int("the root degree")?;
    if n == 0 {
        bail!("the root degree cannot be zero");
    }
    if x.v < 0.0 && n % 2 != 0 {
        let pos = ops::pow(Quantity::new(-x.v, x.dim), Quantity::scalar(1.0 / n as f64))?;
        return Ok(Value::Num(Quantity::new(-pos.v, pos.dim)));
    }
    Ok(Value::Num(ops::pow(x, Quantity::new(1.0 / n as f64, Dim::NONE))?))
}

fn abs(args: &[Value]) -> Result<Value> {
    arity("abs", args, |n| n == 1, "1 argument")?;
    match arg(args, 0)? {
        Value::Num(q) => Ok(Value::Num(Quantity::new(q.v.abs(), q.dim))),
        Value::Matrix(m) if m.is_vector() => {
            let (vals, dim) = uniform(m, "a vector element")?;
            Ok(Value::Num(Quantity::new(vals.iter().map(|v| v * v).sum::<f64>().sqrt(), dim)))
        }
        Value::Matrix(m) => ops::det(m),
        Value::Str(_) => bail!("abs needs a number"),
        Value::Sym(_) => bail!("abs needs a number, not a formula"),
    }
}

fn log(args: &[Value]) -> Result<Value> {
    arity("log", args, |n| n == 1 || n == 2, "1 or 2 arguments: log(x) or log(x, base)")?;
    let x = arg(args, 0)?.as_scalar("the argument of log")?;
    let base = match args.get(1) {
        Some(b) => b.as_scalar("the base of log")?,
        None => 10.0,
    };
    if x <= 0.0 || base <= 0.0 || base == 1.0 {
        bail!("log is only defined for positive x and a positive base ≠ 1");
    }
    Ok(Value::num(x.ln() / base.ln()))
}

fn atan2(args: &[Value]) -> Result<Value> {
    arity("atan2", args, |n| n == 2, "2 arguments: atan2(y, x)")?;
    let y = arg(args, 0)?.as_num("y")?;
    let x = arg(args, 1)?.as_num("x")?;
    same_dim(y.dim, x.dim, "take atan2 of")?;
    Ok(Value::num(y.v.atan2(x.v)))
}

fn round(args: &[Value]) -> Result<Value> {
    arity("round", args, |n| n == 1 || n == 2, "1 or 2 arguments")?;
    let q = arg(args, 0)?.as_num("the argument of round")?;
    // 10^±308 is as far as a decimal scale goes before it overflows to ∞ or 0
    const MAX_DECIMALS: i64 = 308;
    let digits = match args.get(1) {
        Some(d) => d.as_int("the number of decimals")?,
        None => 0,
    };
    if !(-MAX_DECIMALS..=MAX_DECIMALS).contains(&digits) {
        bail!(
            "round: the number of decimals must be between -{MAX_DECIMALS} and {MAX_DECIMALS}, not {digits}"
        );
    }
    let rounded = if digits >= 0 {
        let scale = 10f64.powi(digits as i32);
        let scaled = q.v * scale;
        // a number too large to scale has no digits that far right: it is already round
        if scaled.is_finite() { scaled.round() / scale } else { q.v }
    } else {
        let scale = 10f64.powi(-digits as i32);
        (q.v / scale).round() * scale
    };
    if rounded.is_infinite() && q.v.is_finite() {
        bail!("round: rounding {} to {digits} decimals is too large", q.v);
    }
    Ok(Value::Num(Quantity::new(rounded, q.dim)))
}

fn sign(args: &[Value]) -> Result<Value> {
    arity("sign", args, |n| n == 1, "1 argument")?;
    let q = arg(args, 0)?.as_num("the argument of sign")?;
    Ok(Value::num(if q.v > 0.0 {
        1.0
    } else if q.v < 0.0 {
        -1.0
    } else {
        0.0
    }))
}

fn mod_(args: &[Value]) -> Result<Value> {
    arity("mod", args, |n| n == 2, "2 arguments")?;
    arith(BinaryOp::Mod, arg(args, 0)?, arg(args, 1)?)
}

fn sum_all(args: &[Value]) -> Result<Value> {
    let items = flatten(args);
    let mut acc = Value::num(0.0);
    for (k, v) in items.into_iter().enumerate() {
        acc = if k == 0 { v.clone() } else { arith(BinaryOp::Add, &acc, v)? };
    }
    Ok(acc)
}

fn mean(args: &[Value]) -> Result<Value> {
    let n = flatten(args).len();
    if n == 0 {
        bail!("mean needs at least one value");
    }
    arith(BinaryOp::Div, &sum_all(args)?, &Value::num(n as f64))
}

/// A vector's numbers and their common units.
pub(crate) type Vector = (Vec<f64>, Dim);

/// The numbers of a vector (row or column), all with the same units.
pub(crate) fn vector(name: &str, v: &Value) -> Result<Vector> {
    let m = matrix_arg(name, v)?;
    if m.rows != 1 && m.cols != 1 {
        bail!("{name} needs a vector, not a {}×{} matrix", m.rows, m.cols);
    }
    let mut dim = None;
    let mut out = Vec::with_capacity(m.data.len());
    for x in &m.data {
        let q = x.as_num(&format!("an element of the {name} vector"))?;
        match dim {
            None => dim = Some(q.dim),
            Some(d) => same_dim(d, q.dim, &format!("{name} a vector that mixes"))?,
        }
        out.push(q.v);
    }
    Ok((out, dim.unwrap_or(Dim::NONE)))
}

pub(crate) fn column(values: Vec<f64>, dim: Dim) -> Value {
    let n = values.len();
    Value::Matrix(Matrix::new(n, 1, values.into_iter().map(|v| Value::Num(Quantity::new(v, dim))).collect()))
}

fn sort(args: &[Value]) -> Result<Value> {
    arity("sort", args, |n| n == 1, "1 argument")?;
    let (mut v, dim) = vector("sort", arg(args, 0)?)?;
    v.sort_by(f64::total_cmp);
    Ok(column(v, dim))
}

fn norm(args: &[Value]) -> Result<Value> {
    arity("norm", args, |n| n == 1, "1 argument")?;
    let (v, dim) = vector("norm", arg(args, 0)?)?;
    Ok(Value::Num(Quantity::new(v.iter().map(|x| x * x).sum::<f64>().sqrt(), dim)))
}

fn two_vectors(name: &str, args: &[Value]) -> Result<(Vector, Vector)> {
    arity(name, args, |n| n == 2, "2 arguments")?;
    let a = vector(name, arg(args, 0)?)?;
    let b = vector(name, arg(args, 1)?)?;
    if a.0.len() != b.0.len() {
        bail!("{name} needs two vectors of the same length, not {} and {}", a.0.len(), b.0.len());
    }
    Ok((a, b))
}

fn dot(args: &[Value]) -> Result<Value> {
    let ((a, da), (b, db)) = two_vectors("dot", args)?;
    Ok(Value::Num(Quantity::new(a.iter().zip(&b).map(|(x, y)| x * y).sum(), dim_mul(da, db)?)))
}

fn cross(args: &[Value]) -> Result<Value> {
    let ((a, da), (b, db)) = two_vectors("cross", args)?;
    let c = match (a.as_slice(), b.as_slice()) {
        ([a0, a1, a2], [b0, b1, b2]) => vec![a1 * b2 - a2 * b1, a2 * b0 - a0 * b2, a0 * b1 - a1 * b0],
        _ => bail!("cross needs two vectors of 3 elements"),
    };
    Ok(column(c, dim_mul(da, db)?))
}

/// `lsolve(A, b)`: the x that makes A·x = b.
fn lsolve(args: &[Value]) -> Result<Value> {
    arity("lsolve", args, |n| n == 2, "2 arguments")?;
    let a = matrix_arg("lsolve", arg(args, 0)?)?;
    if a.rows != a.cols {
        bail!("lsolve needs a square matrix, not {}×{}", a.rows, a.cols);
    }
    let rows = match arg(args, 1)? {
        Value::Matrix(m) => m.rows,
        _ => 1,
    };
    if rows != a.rows {
        bail!("lsolve: b needs {} rows to match the matrix, not {rows}", a.rows);
    }
    let inv = match ops::inverse(a) {
        Err(e) if e.message == "the matrix is singular" => bail!("lsolve: the matrix is singular"),
        r => r?,
    };
    arith(BinaryOp::Mul, &inv, arg(args, 1)?)
}

/// `linterp(xs, ys, x)`: straight lines between table points, extended past the ends.
fn linterp(args: &[Value]) -> Result<Value> {
    arity("linterp", args, |n| n == 3, "3 arguments")?;
    let (xs, xd) = vector("linterp", arg(args, 0)?)?;
    let (ys, yd) = vector("linterp", arg(args, 1)?)?;
    if xs.len() != ys.len() || xs.len() < 2 {
        bail!("linterp needs two vectors of the same length, with at least 2 points");
    }
    if xs.iter().zip(xs.iter().skip(1)).any(|(a, b)| b <= a) {
        bail!("linterp needs the x values in increasing order");
    }
    let x = arg(args, 2)?.as_num("the point to interpolate at")?;
    same_dim(xd, x.dim, "interpolate between")?;
    let i = xs.partition_point(|&v| v <= x.v).clamp(1, xs.len().saturating_sub(1).max(1));
    let (x0, x1) = (nth_value(&xs, i - 1)?, nth_value(&xs, i)?);
    let (y0, y1) = (nth_value(&ys, i - 1)?, nth_value(&ys, i)?);
    let t = (x.v - x0) / (x1 - x0);
    Ok(Value::Num(Quantity::new(y0 + t * (y1 - y0), yd)))
}

fn one_matrix<'a>(name: &str, args: &'a [Value]) -> Result<&'a Matrix> {
    arity(name, args, |n| n == 1, "1 argument")?;
    matrix_arg(name, arg(args, 0)?)
}

fn size(name: &str, args: &[Value]) -> Result<Value> {
    arity(name, args, |n| n == 1, "1 argument")?;
    let (r, c) = match arg(args, 0)? {
        Value::Matrix(m) => (m.rows, m.cols),
        _ => (1, 1),
    };
    Ok(Value::num(match name {
        "rows" => r,
        "cols" => c,
        _ => r * c,
    } as f64))
}

fn el(args: &[Value]) -> Result<Value> {
    arity("el", args, |n| n == 2 || n == 3, "2 or 3 arguments")?;
    let [base, idx @ ..] = args else { bail!("el takes 2 or 3 arguments") };
    index(base, idx)
}

fn transpose(args: &[Value]) -> Result<Value> {
    let m = one_matrix("transpose", args)?;
    let data = (0..m.cols)
        .flat_map(|c| (0..m.rows).map(move |r| m.at(r, c).cloned()))
        .collect::<Result<Vec<_>>>()?;
    Ok(Value::Matrix(Matrix::new(m.cols, m.rows, data)))
}

fn identity(args: &[Value]) -> Result<Value> {
    arity("identity", args, |n| n == 1, "1 argument")?;
    let n = arg(args, 0)?.as_int("the size")?;
    // n×n elements, at most MAX_ELEMENTS
    if !(1..=1000).contains(&n) {
        bail!("identity size must be between 1 and 1000");
    }
    Ok(Value::Matrix(ops::identity(n as usize)))
}

/// SMath's `mat(a11, a12, …, rows, cols)`.
fn mat(args: &[Value]) -> Result<Value> {
    if args.len() < 3 {
        bail!("mat takes the elements followed by rows and cols");
    }
    let (cells, dims) = args.split_at(args.len() - 2);
    let [rows_v, cols_v] = dims else { bail!("mat takes the elements followed by rows and cols") };
    let (Ok(r), Ok(c)) = (usize::try_from(rows_v.as_int("rows")?), usize::try_from(cols_v.as_int("cols")?))
    else {
        bail!("mat: rows and cols must be positive");
    };
    if r == 0 || c == 0 {
        bail!("mat: rows and cols must be positive");
    }
    if r.checked_mul(c) != Some(cells.len()) {
        bail!("mat: {r}×{c} needs {} elements, got {}", r.saturating_mul(c), cells.len());
    }
    Ok(Value::Matrix(Matrix::new(r, c, cells.to_vec())))
}

fn range_fn(args: &[Value]) -> Result<Value> {
    arity("range", args, |n| n == 2 || n == 3, "2 or 3 arguments: range(start, end[, step])")?;
    let a = arg(args, 0)?.as_num("the start")?;
    let b = arg(args, 1)?.as_num("the end")?;
    let step = args.get(2).map(|s| s.as_num("the step")).transpose()?;
    range(a, b, step)
}

fn num2str(args: &[Value]) -> Result<Value> {
    arity("num2str", args, |n| n == 1, "1 argument")?;
    let x = arg(args, 0)?.as_scalar("the argument of num2str")?;
    Ok(Value::Str(format_number(x, 4, &DisplaySettings::default())))
}

fn concat(args: &[Value]) -> Result<Value> {
    let mut s = String::new();
    for a in args {
        match a {
            Value::Str(t) => s.push_str(t),
            other => bail!("concat joins strings, not {}", other.kind()),
        }
    }
    Ok(Value::Str(s))
}

/// Dimension of `x^n`, as of derivatives and integrals.
pub fn dim_pow(d: Dim, n: i32) -> Result<Dim> {
    dim_pow_r(d, Rational::int(n))
}

/// Dimension of `x^r`: `('m^2)^(1/2)` is m.
pub(crate) fn dim_pow_r(d: Dim, r: Rational) -> Result<Dim> {
    d.pow(r).ok_or_else(exponents_too_large)
}

/// Dimension of a product.
pub(crate) fn dim_mul(a: Dim, b: Dim) -> Result<Dim> {
    a.checked_mul(b).ok_or_else(exponents_too_large)
}

/// Dimension of a quotient.
pub(crate) fn dim_div(a: Dim, b: Dim) -> Result<Dim> {
    a.checked_div(b).ok_or_else(exponents_too_large)
}

/// Exponents are exact fractions of 32-bit whole numbers; a power such as
/// `('m^1000000)^1000000`, or many nested square roots, goes past them.
fn exponents_too_large() -> EvalError {
    EvalError::new("the powers of the units in this result are too large to keep exactly")
}
