//! Arithmetic on values with dimension checking.

use quire_ast::{BinaryOp, CmpOp};
use quire_units::{Dim, Rational};

use crate::error::{EvalError, Result, bail};
use crate::format::describe_dim;
use crate::value::{Matrix, Quantity, Value};

pub fn same_dim(a: Dim, b: Dim, verb: &str) -> Result<()> {
    if a != b {
        bail!("cannot {verb} {} and {}", describe_dim(a), describe_dim(b));
    }
    Ok(())
}

fn check(v: f64, what: &str) -> Result<f64> {
    if v.is_nan() {
        bail!("{what} has no real result (complex numbers are not supported yet)");
    }
    Ok(v)
}

/// The largest magnitude a result can have, for messages.
const LIMIT: &str = "about 1.8e308";

/// `v`, the result of an operation on `inputs`, when it is a number. Finite inputs never give
/// ∞ (overflow is an error, as division by zero is); ∞ given as an input (the constant ∞) may
/// carry through, but never to an undefined ∞ − ∞ or 0·∞.
fn finite(v: f64, inputs: [f64; 2], what: &str) -> Result<f64> {
    if v.is_nan() {
        bail!("{what} is undefined (as ∞ − ∞, 0·∞ and ∞/∞ are)");
    }
    if v.is_infinite() && inputs.iter().all(|x| x.is_finite()) {
        bail!("{what} is too large: results go up to {LIMIT}");
    }
    Ok(v)
}

pub fn pow(x: Quantity, y: Quantity) -> Result<Quantity> {
    if !y.dim.is_dimensionless() {
        bail!("an exponent must be a plain number, but this one is {}", describe_dim(y.dim));
    }
    let dim = if x.dim.is_dimensionless() {
        Dim::NONE
    } else {
        match Rational::approx(y.v) {
            Some(r) => crate::builtins::dim_pow_r(x.dim, r)?,
            None => bail!("cannot raise {} to the power {}", describe_dim(x.dim), y.v),
        }
    };
    // 0^-1 is 1/0
    if x.v == 0.0 && y.v < 0.0 {
        bail!("division by zero (0 to a negative power)");
    }
    let v = if y.v.fract() == 0.0 && y.v.abs() < 1e9 { x.v.powi(y.v as i32) } else { x.v.powf(y.v) };
    Ok(Quantity::new(finite(check(v, "this power")?, [x.v, y.v], "this power")?, dim))
}

fn num_op(op: BinaryOp, x: Quantity, y: Quantity) -> Result<Quantity> {
    let inputs = [x.v, y.v];
    Ok(match op {
        BinaryOp::Add => {
            same_dim(x.dim, y.dim, "add")?;
            Quantity::new(finite(x.v + y.v, inputs, "this sum")?, x.dim)
        }
        BinaryOp::Sub => {
            same_dim(x.dim, y.dim, "subtract")?;
            Quantity::new(finite(x.v - y.v, inputs, "this difference")?, x.dim)
        }
        BinaryOp::Mul | BinaryOp::Juxt => {
            Quantity::new(finite(x.v * y.v, inputs, "this product")?, crate::builtins::dim_mul(x.dim, y.dim)?)
        }
        BinaryOp::Div => {
            if y.v == 0.0 {
                bail!("division by zero");
            }
            Quantity::new(
                finite(x.v / y.v, inputs, "this quotient")?,
                crate::builtins::dim_div(x.dim, y.dim)?,
            )
        }
        BinaryOp::Mod => {
            same_dim(x.dim, y.dim, "take the remainder of")?;
            if y.v == 0.0 {
                bail!("remainder by zero");
            }
            Quantity::new(finite(x.v % y.v, inputs, "this remainder")?, x.dim)
        }
        BinaryOp::Pow => pow(x, y)?,
        BinaryOp::PlusMinus => bail!("± gives two results; this is not supported yet"),
        BinaryOp::And | BinaryOp::Or => bail!("logical operators are evaluated lazily"),
    })
}

pub(crate) fn map(m: &Matrix, f: impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    let data = m.data.iter().map(f).collect::<Result<Vec<_>>>()?;
    Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
}

pub fn arith(op: BinaryOp, a: &Value, b: &Value) -> Result<Value> {
    use Value::{Matrix as M, Num};
    let op = if op == BinaryOp::Juxt { BinaryOp::Mul } else { op };
    match (a, b) {
        (Num(x), Num(y)) => Ok(Num(num_op(op, *x, *y)?)),
        (M(m), M(n)) => match op {
            BinaryOp::Add | BinaryOp::Sub => {
                if (m.rows, m.cols) != (n.rows, n.cols) {
                    bail!("cannot add a {}×{} matrix and a {}×{} matrix", m.rows, m.cols, n.rows, n.cols);
                }
                let data = m.data.iter().zip(&n.data).map(|(x, y)| arith(op, x, y)).collect::<Result<_>>()?;
                Ok(M(Matrix::new(m.rows, m.cols, data)))
            }
            BinaryOp::Mul => matmul(m, n),
            _ => bail!("'{}' is not defined between two matrices", op.symbol()),
        },
        (M(m), Num(y)) => match op {
            BinaryOp::Pow => matrix_pow(m, *y),
            _ => map(m, |e| arith(op, e, b)),
        },
        (Num(_), M(m)) => match op {
            BinaryOp::Mul | BinaryOp::Add | BinaryOp::Sub => map(m, |e| arith(op, a, e)),
            _ => bail!("'{}' is not defined for a number and a matrix", op.symbol()),
        },
        // a formula with a matrix works element by element (a matrix power needs numbers)
        (M(m), Value::Sym(_)) if op != BinaryOp::Pow => map(m, |e| arith(op, e, b)),
        (Value::Sym(_), M(m)) if op != BinaryOp::Pow => map(m, |e| arith(op, a, e)),
        (Num(_) | Value::Sym(_), Num(_) | Value::Sym(_)) => crate::symbolic::bridge::arith_sym(op, a, b),
        _ => bail!("'{}' cannot combine {} and {}", op.symbol(), a.kind(), b.kind()),
    }
}

pub fn negate(v: &Value) -> Result<Value> {
    match v {
        Value::Num(q) => Ok(Value::Num(Quantity::new(-q.v, q.dim))),
        Value::Matrix(m) => map(m, negate),
        Value::Sym(_) => crate::symbolic::bridge::arith_sym(BinaryOp::Mul, &Value::num(-1.0), v),
        Value::Str(_) => bail!("cannot negate a string"),
    }
}

pub fn compare(op: CmpOp, a: &Value, b: &Value) -> Result<bool> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => {
            same_dim(x.dim, y.dim, "compare")?;
            // the tolerance is relative, so it must not make every number equal to ∞
            let close =
                x.v.is_finite() && y.v.is_finite() && (x.v - y.v).abs() <= 1e-12 * x.v.abs().max(y.v.abs());
            let eq = x.v == y.v || close;
            Ok(match op {
                CmpOp::Eq => eq,
                CmpOp::Ne => !eq,
                CmpOp::Lt => x.v < y.v && !eq,
                CmpOp::Le => x.v < y.v || eq,
                CmpOp::Gt => x.v > y.v && !eq,
                CmpOp::Ge => x.v > y.v || eq,
            })
        }
        (Value::Str(x), Value::Str(y)) => match op {
            CmpOp::Eq => Ok(x == y),
            CmpOp::Ne => Ok(x != y),
            _ => bail!("strings can only be compared with == and ≠"),
        },
        (Value::Matrix(m), Value::Matrix(n)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
            let mut eq = (m.rows, m.cols) == (n.rows, n.cols);
            if eq {
                for (x, y) in m.data.iter().zip(&n.data) {
                    eq &= compare(CmpOp::Eq, x, y)?;
                }
            }
            Ok(eq == (op == CmpOp::Eq))
        }
        // a number is never equal to a matrix: SMath's ≡ answers false there (`if(path ≡ 0, …)`
        // once path has become a list), it does not fail
        (Value::Matrix(_), Value::Num(_)) | (Value::Num(_), Value::Matrix(_))
            if matches!(op, CmpOp::Eq | CmpOp::Ne) =>
        {
            Ok(op == CmpOp::Ne)
        }
        _ => bail!("cannot compare {} with {}", a.kind(), b.kind()),
    }
}

pub fn matmul(m: &Matrix, n: &Matrix) -> Result<Value> {
    Ok(Value::Matrix(mat_mul(m, n)?))
}

fn mat_mul(m: &Matrix, n: &Matrix) -> Result<Matrix> {
    if m.cols != n.rows {
        bail!("cannot multiply a {}×{} matrix by a {}×{} matrix", m.rows, m.cols, n.rows, n.cols);
    }
    crate::budget::check_size(m.rows, n.cols)?;
    // element arithmetic is slow next to plain floats: a step per few multiply-adds
    let work = (m.rows as u64).saturating_mul(n.cols as u64).saturating_mul(m.cols as u64);
    crate::budget::charge(work, 8)?;
    let mut data = Vec::with_capacity(m.rows * n.cols);
    for i in 0..m.rows {
        for j in 0..n.cols {
            let mut acc = arith(BinaryOp::Mul, m.at(i, 0)?, n.at(0, j)?)?;
            for k in 1..m.cols {
                acc = arith(BinaryOp::Add, &acc, &arith(BinaryOp::Mul, m.at(i, k)?, n.at(k, j)?)?)?;
            }
            data.push(acc);
        }
    }
    Ok(Matrix::new(m.rows, n.cols, data))
}

/// All elements numeric with one common dimension → (values, dim).
pub fn uniform(m: &Matrix, what: &str) -> Result<(Vec<f64>, Dim)> {
    let mut dim = None;
    let mut vals = Vec::with_capacity(m.data.len());
    for e in &m.data {
        let q = e.as_num(what)?;
        match dim {
            None => dim = Some(q.dim),
            Some(d) => same_dim(d, q.dim, "mix")?,
        }
        vals.push(q.v);
    }
    Ok((vals, dim.unwrap_or(Dim::NONE)))
}

pub fn identity(n: usize) -> Matrix {
    let data = (0..n * n).map(|k| Value::num(if k / n == k % n { 1.0 } else { 0.0 })).collect();
    Matrix::new(n, n, data)
}

/// Element `i` of a row-major buffer; the callers stay inside the square it was checked to be.
fn at(v: &[f64], i: usize) -> f64 {
    v.get(i).copied().unwrap_or(f64::NAN)
}

/// Gauss-Jordan elimination with partial pivoting; returns (determinant, inverse?, dimension
/// of the entries). A pivot counts as zero when it is tiny next to its own row: a row of
/// small SI magnitudes is fine, while `[1 2; 2 4]` in rounding noise is singular.
fn lu(m: &Matrix, want_inverse: bool) -> Result<(f64, Option<Vec<f64>>, Dim)> {
    if m.rows != m.cols {
        bail!("the matrix must be square, but is {}×{}", m.rows, m.cols);
    }
    let n = m.rows;
    // Gauss-Jordan on floats: about n³ multiply-adds for the matrix and as many for the inverse
    crate::budget::charge((n as u64).saturating_pow(3).saturating_mul(2), 64)?;
    let (mut a, dim) = uniform(m, "a matrix element")?;
    if a.len() != n * n {
        bail!("the matrix has {} elements, but a {n}×{n} matrix needs {}", a.len(), n * n);
    }
    let mut scale: Vec<f64> =
        a.chunks(n.max(1)).map(|row| row.iter().fold(0.0_f64, |s, x| s.max(x.abs()))).collect();
    let mut inv: Vec<f64> = (0..n * n).map(|k| if k / n == k % n { 1.0 } else { 0.0 }).collect();
    let mut det = 1.0;
    for c in 0..n {
        let p =
            (c + 1..n).fold(c, |p, i| if at(&a, i * n + c).abs() > at(&a, p * n + c).abs() { i } else { p });
        if at(&a, p * n + c).abs() <= f64::EPSILON * n as f64 * at(&scale, p) {
            return Ok((0.0, None, dim));
        }
        if p != c {
            for k in 0..n {
                a.swap(p * n + k, c * n + k);
                inv.swap(p * n + k, c * n + k);
            }
            scale.swap(p, c);
            det = -det;
        }
        let piv = at(&a, c * n + c);
        det *= piv;
        for k in 0..n {
            for buf in [&mut a, &mut inv] {
                if let Some(x) = buf.get_mut(c * n + k) {
                    *x /= piv;
                }
            }
        }
        for r in 0..n {
            let f = if r == c { 0.0 } else { at(&a, r * n + c) };
            if f == 0.0 {
                continue;
            }
            for k in 0..n {
                let (ac, ic) = (at(&a, c * n + k), at(&inv, c * n + k));
                if let Some(x) = a.get_mut(r * n + k) {
                    *x -= f * ac;
                }
                if let Some(x) = inv.get_mut(r * n + k) {
                    *x -= f * ic;
                }
            }
        }
    }
    Ok((det, want_inverse.then_some(inv), dim))
}

pub fn det(m: &Matrix) -> Result<Value> {
    if m.data.iter().any(|v| matches!(v, Value::Sym(_))) {
        return det_by_cofactors(m);
    }
    let (d, _, dim) = lu(m, false)?;
    let rows = i32::try_from(m.rows).map_err(|_| EvalError::new("the matrix is too large"))?;
    Ok(Value::Num(Quantity::new(d, crate::builtins::dim_pow(dim, rows)?)))
}

/// The determinant of a matrix holding formulas, by expansion along the first row. Elimination
/// needs comparisons between entries, which formulas do not have, so this is used instead; it
/// is only for small matrices (the work grows as n!).
fn det_by_cofactors(m: &Matrix) -> Result<Value> {
    if m.rows != m.cols {
        bail!("the matrix must be square, but is {}×{}", m.rows, m.cols);
    }
    let n = m.rows;
    if n > 6 {
        bail!("a determinant of a {n}×{n} matrix with formulas is too large");
    }
    match m.data.first() {
        None => Ok(Value::num(1.0)),
        Some(only) if n == 1 => Ok(only.clone()),
        Some(_) => {
            let mut acc = Value::num(0.0);
            for (j, a) in m.data.iter().take(n).enumerate() {
                let minor = Matrix::new(n - 1, n - 1, minor_without_first_row(m, j));
                let term = arith(BinaryOp::Mul, a, &det(&minor)?)?;
                acc = arith(if j % 2 == 0 { BinaryOp::Add } else { BinaryOp::Sub }, &acc, &term)?;
            }
            Ok(acc)
        }
    }
}

/// The entries below the first row, without column `col`, row by row.
fn minor_without_first_row(m: &Matrix, col: usize) -> Vec<Value> {
    m.data
        .chunks(m.cols)
        .skip(1)
        .flat_map(|row| row.iter().enumerate().filter(|(c, _)| *c != col).map(|(_, v)| v.clone()))
        .collect()
}

pub fn inverse(m: &Matrix) -> Result<Value> {
    Ok(Value::Matrix(inverse_of(m)?))
}

/// The inverse of a square matrix of one dimension `D`: its entries have dimension `1/D`.
fn inverse_of(m: &Matrix) -> Result<Matrix> {
    let (_, inv, dim) = lu(m, true)?;
    let Some(inv) = inv else { bail!("the matrix is singular") };
    let inv_dim = crate::builtins::dim_div(Dim::NONE, dim)?;
    let data = inv.into_iter().map(|v| Value::Num(Quantity::new(v, inv_dim))).collect();
    Ok(Matrix::new(m.rows, m.cols, data))
}

/// `A^k` for a whole number `k`: `A^0` is the identity, `A^-k` is `(A^-1)^k`. Squaring
/// keeps the work logarithmic in `k`. The elements must share one dimension `D`, so that
/// `A^k` has dimension `D^k` (and `A^-k` has `1/D^k`) in every entry.
fn matrix_pow(m: &Matrix, y: Quantity) -> Result<Value> {
    let k = Value::Num(y).as_int("a matrix exponent")?;
    if m.rows != m.cols {
        bail!("only square matrices can be raised to a power");
    }
    uniform(m, "a matrix element")?;
    let base = if k < 0 { inverse_of(m)? } else { m.clone() };
    let mut result = identity(m.rows);
    let mut square = base;
    let mut e = k.unsigned_abs();
    while e > 0 {
        if e & 1 == 1 {
            result = mat_mul(&result, &square)?;
        }
        e >>= 1;
        if e > 0 {
            square = mat_mul(&square, &square)?;
        }
    }
    let finite = result.data.iter().all(|v| matches!(v, Value::Num(q) if q.v.is_finite()));
    if !finite {
        bail!("this power is too large for the matrix's values");
    }
    Ok(Value::Matrix(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn mat(rows: usize, cols: usize, vals: &[f64], dim: Dim) -> Matrix {
        Matrix::new(rows, cols, vals.iter().map(|&v| Value::Num(Quantity::new(v, dim))).collect())
    }

    /// The elements of a matrix value as plain numbers, row by row.
    fn values(v: &Value) -> std::result::Result<Vec<f64>, String> {
        match v {
            Value::Matrix(m) => m
                .data
                .iter()
                .map(|e| match e {
                    Value::Num(q) => Ok(q.v),
                    _ => Err("not a number".to_string()),
                })
                .collect(),
            _ => Err("not a matrix".to_string()),
        }
    }

    fn close(got: &[f64], want: &[f64]) -> bool {
        got.len() == want.len() && got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-9)
    }

    fn power(m: &Matrix, k: f64) -> Result<Value> {
        matrix_pow(m, Quantity::new(k, Dim::NONE))
    }

    #[test]
    fn inverse_of_a_dimensionless_matrix() -> TestResult {
        // [2 1; 1 3]^-1 = [3 -1; -1 2] / 5
        let a = mat(2, 2, &[2.0, 1.0, 1.0, 3.0], Dim::NONE);
        let inv = values(&power(&a, -1.0)?)?;
        assert!(close(&inv, &[0.6, -0.2, -0.2, 0.4]), "{inv:?}");
        Ok(())
    }

    #[test]
    fn inverse_needs_a_row_swap() -> TestResult {
        // the first pivot is zero, so partial pivoting must swap rows
        let a = mat(2, 2, &[0.0, 1.0, 2.0, 0.0], Dim::NONE);
        let inv = values(&power(&a, -1.0)?)?;
        assert!(close(&inv, &[0.0, 0.5, 1.0, 0.0]), "{inv:?}");
        Ok(())
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let exact = mat(2, 2, &[1.0, 2.0, 2.0, 4.0], Dim::NONE);
        let err = power(&exact, -1.0).err().map(|e| e.message);
        assert_eq!(err.as_deref(), Some("the matrix is singular"));
        // rounding leaves a tiny non-zero pivot; the matrix is still singular
        let rounded = mat(2, 2, &[0.1, 0.2, 0.3, 0.6], Dim::NONE);
        let err = power(&rounded, -1.0).err().map(|e| e.message);
        assert_eq!(err.as_deref(), Some("the matrix is singular"));
        // and a singular matrix with a zero row
        let zero_row = mat(2, 2, &[0.0, 0.0, 1.0, 2.0], Dim::NONE);
        assert!(power(&zero_row, -2.0).is_err());
    }

    #[test]
    fn tiny_si_magnitudes_are_not_singular() -> TestResult {
        // a well-conditioned matrix with entries around 1e-9 (say mm^2 in SI)
        let a = mat(2, 2, &[2e-9, 0.0, 0.0, 4e-9], Dim::NONE);
        let inv = values(&power(&a, -1.0)?)?;
        assert!(close(&inv.iter().map(|v| v * 1e-9).collect::<Vec<_>>(), &[0.5, 0.0, 0.0, 0.25]), "{inv:?}");
        Ok(())
    }

    #[test]
    fn non_negative_powers() -> TestResult {
        let a = mat(2, 2, &[1.0, 1.0, 0.0, 1.0], Dim::NONE);
        assert!(close(&values(&power(&a, 0.0)?)?, &[1.0, 0.0, 0.0, 1.0]));
        assert!(close(&values(&power(&a, 1.0)?)?, &[1.0, 1.0, 0.0, 1.0]));
        // [1 1; 0 1]^5 = [1 5; 0 1]
        assert!(close(&values(&power(&a, 5.0)?)?, &[1.0, 5.0, 0.0, 1.0]));
        Ok(())
    }

    #[test]
    fn negative_power_is_inverse_to_the_absolute_power() -> TestResult {
        let a = mat(2, 2, &[2.0, 1.0, 1.0, 3.0], Dim::NONE);
        let inv = power(&a, -1.0)?;
        let Value::Matrix(inv_m) = &inv else { return Err("not a matrix".into()) };
        let direct = values(&power(inv_m, 3.0)?)?;
        assert!(close(&values(&power(&a, -3.0)?)?, &direct));
        // A^-1 * A is the identity
        let product = arith(BinaryOp::Mul, &inv, &Value::Matrix(a.clone()))?;
        assert!(close(&values(&product)?, &[1.0, 0.0, 0.0, 1.0]));
        Ok(())
    }

    #[test]
    fn inverse_times_a_column_solves_the_system() -> TestResult {
        // A^-1 * d for A = [2 1; 1 3], d = [1; 2]: x = [1/5; 3/5]
        let a = Value::Matrix(mat(2, 2, &[2.0, 1.0, 1.0, 3.0], Dim::NONE));
        let d = Value::Matrix(mat(2, 1, &[1.0, 2.0], Dim::NONE));
        let x = arith(BinaryOp::Mul, &power(&mat(2, 2, &[2.0, 1.0, 1.0, 3.0], Dim::NONE), -1.0)?, &d)?;
        assert!(close(&values(&x)?, &[0.2, 0.6]));
        assert!(arith(BinaryOp::Mul, &a, &d).is_ok());
        Ok(())
    }

    #[test]
    fn matrix_with_units_has_inverse_dimension() -> TestResult {
        let length = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let a = mat(2, 2, &[2.0, 0.0, 0.0, 4.0], length);
        let Value::Matrix(inv) = power(&a, -1.0)? else { return Err("not a matrix".into()) };
        for e in &inv.data {
            let Value::Num(q) = e else { return Err("not a number".into()) };
            assert_eq!(Some(q.dim), Dim::NONE.checked_div(length));
        }
        let Value::Matrix(sq) = power(&a, 2.0)? else { return Err("not a matrix".into()) };
        let Some(Value::Num(q)) = sq.get(0, 0) else { return Err("not a number".into()) };
        assert_eq!(Some(q.dim), length.pow(Rational::int(2)));
        Ok(())
    }

    #[test]
    fn mixed_units_in_a_power_are_an_error() {
        let length = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let data =
            vec![Value::Num(Quantity::new(1.0, length)), Value::num(0.0), Value::num(0.0), Value::num(1.0)];
        let m = Matrix::new(2, 2, data);
        assert!(power(&m, 2.0).is_err());
        assert!(power(&m, -1.0).is_err());
    }

    #[test]
    fn non_square_and_fractional_powers_are_errors() {
        let rect = mat(2, 3, &[1.0; 6], Dim::NONE);
        assert!(power(&rect, 2.0).is_err());
        let a = mat(2, 2, &[1.0, 0.0, 0.0, 1.0], Dim::NONE);
        assert!(power(&a, 0.5).is_err());
    }
}
