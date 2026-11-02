//! Matrix helpers that SMath spells as functions: a column or row of a matrix, sorting
//! its lines by a key, the trace, the roots of a polynomial, building and cutting matrices
//! (`matrix`, `diag`, `submatrix`, `reverse`), minors and cofactors, rank, norms, and
//! finding rows. Indices are 1-based, as in SMath and in `el`; every error names the
//! function and says what was expected.

use std::ops::{Add, Div, Mul, Sub};

use quire_ast::BinaryOp;
use quire_units::Dim;

use crate::builtins::column;
use crate::error::{EvalError, Result, bail};
use crate::ops::{arith, same_dim};
use crate::value::{Matrix, Quantity, Value};

/// `None` when `name` is not one of these.
pub fn call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let r = match name {
        "col" => column_of(args),
        "row" => row_of(args),
        "csort" => sort_lines(name, args, true),
        "rsort" => sort_lines(name, args, false),
        "tr" => trace(args),
        "polyroots" => polyroots(args),
        "matrix" => zeros(args),
        "submatrix" => submatrix(args),
        "diag" => diag(args),
        "reverse" => reverse(args),
        "rank" => rank(args),
        // vminor is SMath's way of showing a minor; its value is the minor's
        "minor" | "vminor" => minor(name, args, false),
        "alg" | "cofactor" => minor(name, args, true),
        "norm1" => matrix_norm(name, args, Norm::Columns),
        "normi" => matrix_norm(name, args, Norm::Rows),
        "norme" => matrix_norm(name, args, Norm::Euclidean),
        "invert" => invert(args),
        "findrows" => findrows(args),
        _ => return None,
    };
    Some(r)
}

/// `matrix(r, c)`: an r×c matrix of zeros.
fn zeros(args: &[Value]) -> Result<Value> {
    let [r, c] = args else { bail!("matrix takes 2 arguments: matrix(rows, cols)") };
    let size = |v: &Value, noun: &str| -> Result<usize> {
        let n = v.as_int(&format!("the number of {noun}"))?;
        match usize::try_from(n) {
            Ok(n) if n >= 1 => Ok(n),
            _ => bail!("matrix: the number of {noun} must be at least 1, not {n}"),
        }
    };
    let (rows, cols) = (size(r, "rows")?, size(c, "columns")?);
    crate::budget::check_size(rows, cols)?;
    Ok(Value::Matrix(Matrix::new(rows, cols, vec![Value::num(0.0); rows * cols])))
}

/// `submatrix(M, r1, r2, c1, c2)`: rows r1 to r2 and columns c1 to c2 of M, ends included.
fn submatrix(args: &[Value]) -> Result<Value> {
    let [src, r1, r2, c1, c2] = args else {
        bail!("submatrix takes 5 arguments: submatrix(M, first row, last row, first column, last column)")
    };
    let m = matrix("submatrix", src)?;
    let (r1, r2) =
        (line_number("submatrix", "row", r1, m.rows, m)?, line_number("submatrix", "row", r2, m.rows, m)?);
    let (c1, c2) = (
        line_number("submatrix", "column", c1, m.cols, m)?,
        line_number("submatrix", "column", c2, m.cols, m)?,
    );
    if r2 < r1 || c2 < c1 {
        bail!("submatrix: the last row and column must not come before the first");
    }
    let mut data = Vec::with_capacity((r2 - r1 + 1) * (c2 - c1 + 1));
    for r in r1..=r2 {
        for c in c1..=c2 {
            data.push(cell(m, r, c)?);
        }
    }
    Ok(Value::Matrix(Matrix::new(r2 - r1 + 1, c2 - c1 + 1, data)))
}

/// `diag(v)`: the square matrix with v on its diagonal. The zeros take v's units, so the
/// result is a matrix of one unit like v (and `det`, `inverse` work on it).
fn diag(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("diag takes 1 argument: diag(v)") };
    let m = matrix("diag", src)?;
    if !m.is_vector() {
        bail!("diag needs a vector, not a {}×{} matrix", m.rows, m.cols);
    }
    let (values, dim) = crate::ops::uniform(m, "an element of diag's vector")?;
    let n = values.len();
    crate::budget::check_size(n, n)?;
    let mut data = vec![Value::Num(Quantity::new(0.0, dim)); n * n];
    for (k, v) in values.into_iter().enumerate() {
        if let Some(slot) = data.get_mut(k * n + k) {
            *slot = Value::Num(Quantity::new(v, dim));
        }
    }
    Ok(Value::Matrix(Matrix::new(n, n, data)))
}

/// `reverse(M)`: the rows of M in reverse order; a row vector's elements in reverse order.
fn reverse(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("reverse takes 1 argument: reverse(M)") };
    let m = matrix("reverse", src)?;
    if m.rows == 1 {
        let data = m.data.iter().rev().cloned().collect();
        return Ok(Value::Matrix(Matrix::new(1, m.cols, data)));
    }
    let data = m.data.chunks(m.cols.max(1)).rev().flatten().cloned().collect();
    Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
}

/// Entries this small next to the largest of their row (after elimination) count as zero.
const RANK_TOLERANCE: f64 = 1e-10;

/// `rank(M)`: the number of linearly independent rows, by elimination with full pivoting.
/// Each row is first scaled to its largest entry, so rows in different units (a row of
/// forces next to a row of lengths) are compared fairly.
fn rank(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("rank takes 1 argument: rank(M)") };
    let m = matrix("rank", src)?;
    let (rows, cols) = (m.rows, m.cols);
    let work = (rows as u64).saturating_mul(cols as u64).saturating_mul(rows.min(cols) as u64);
    crate::budget::charge(work, 64)?;
    let mut a = Vec::with_capacity(m.data.len());
    for v in &m.data {
        a.push(v.as_num("an element of rank's matrix")?.v);
    }
    for row in a.chunks_mut(cols.max(1)) {
        let big = row.iter().fold(0.0_f64, |s, x| s.max(x.abs()));
        if big > 0.0 {
            row.iter_mut().for_each(|x| *x /= big);
        }
    }
    let at = |a: &[f64], r: usize, c: usize| a.get(r * cols + c).copied().unwrap_or(0.0);
    let mut rank = 0;
    let mut live_cols: Vec<usize> = (0..cols).collect();
    for r in 0..rows {
        // the largest remaining entry in rows r.. and the columns not yet used
        let mut best = (0.0, r, 0);
        for i in r..rows {
            for (k, &c) in live_cols.iter().enumerate() {
                let x = at(&a, i, c).abs();
                if x > best.0 {
                    best = (x, i, k);
                }
            }
        }
        if best.0 <= RANK_TOLERANCE {
            break;
        }
        let (_, p, k) = best;
        let pc = live_cols.swap_remove(k);
        for c in 0..cols {
            a.swap(p * cols + c, r * cols + c);
        }
        let piv = at(&a, r, pc);
        for i in r + 1..rows {
            let f = at(&a, i, pc) / piv;
            if f != 0.0 {
                for c in 0..cols {
                    let x = at(&a, r, c);
                    if let Some(y) = a.get_mut(i * cols + c) {
                        *y -= f * x;
                    }
                }
            }
        }
        rank += 1;
    }
    Ok(Value::num(rank as f64))
}

/// `minor(M, i, j)`: the determinant of M without row i and column j; with `signed`, the
/// cofactor `alg(M, i, j)` = (−1)^(i+j)·minor.
fn minor(name: &str, args: &[Value], signed: bool) -> Result<Value> {
    let [src, i, j] = args else { bail!("{name} takes 3 arguments: {name}(M, row, column)") };
    let m = matrix(name, src)?;
    if m.rows != m.cols || m.rows < 2 {
        bail!("{name} needs a square matrix of at least 2×2, not {}×{}", m.rows, m.cols);
    }
    let i = line_number(name, "row", i, m.rows, m)?;
    let j = line_number(name, "column", j, m.cols, m)?;
    let n = m.rows - 1;
    let mut data = Vec::with_capacity(n * n);
    for r in (0..m.rows).filter(|&r| r != i) {
        for c in (0..m.cols).filter(|&c| c != j) {
            data.push(cell(m, r, c)?);
        }
    }
    let d = crate::ops::det(&Matrix::new(n, n, data))?;
    if signed && (i + j) % 2 == 1 { crate::ops::negate(&d) } else { Ok(d) }
}

enum Norm {
    /// The largest column sum of |a|: the L1 norm.
    Columns,
    /// The largest row sum of |a|: the uniform (∞) norm.
    Rows,
    /// The square root of the sum of squares (Frobenius).
    Euclidean,
}

/// `norm1`, `normi`, `norme` of a matrix or vector, in the units of its elements.
fn matrix_norm(name: &str, args: &[Value], kind: Norm) -> Result<Value> {
    let [src] = args else { bail!("{name} takes 1 argument: {name}(M)") };
    let m = matrix(name, src)?;
    let (a, dim) = crate::ops::uniform(m, &format!("an element of {name}'s matrix"))?;
    let at = |r: usize, c: usize| a.get(r * m.cols + c).map_or(0.0, |x| x.abs());
    let v = match kind {
        Norm::Columns => (0..m.cols).map(|c| (0..m.rows).map(|r| at(r, c)).sum()).fold(0.0, f64::max),
        Norm::Rows => (0..m.rows).map(|r| (0..m.cols).map(|c| at(r, c)).sum()).fold(0.0, f64::max),
        Norm::Euclidean => a.iter().map(|x| x * x).sum::<f64>().sqrt(),
    };
    Ok(Value::Num(Quantity::new(v, dim)))
}

/// `invert(x)`: the inverse of a square matrix, or 1/x of a number.
fn invert(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("invert takes 1 argument: invert(M)") };
    match src {
        Value::Matrix(m) => crate::ops::inverse(m),
        Value::Num(_) => arith(BinaryOp::Div, &Value::num(1.0), src),
        other => bail!("invert needs a matrix or a number, not {}", other.kind()),
    }
}

/// `findrows(M, x, j)`: the rows of M whose element in column j equals x, as a matrix, or
/// 0 when there are none (as SMath does). Strings match strings; numbers match within the
/// usual rounding tolerance, and must have the units of the column.
fn findrows(args: &[Value]) -> Result<Value> {
    let [src, want, j] = args else { bail!("findrows takes 3 arguments: findrows(M, value, column)") };
    let m = matrix("findrows", src)?;
    let j = line_number("findrows", "column", j, m.cols, m)?;
    let mut data = Vec::new();
    let mut found = 0;
    for r in 0..m.rows {
        let x = cell(m, r, j)?;
        let hit = match (&x, want) {
            (Value::Num(_), Value::Num(_)) | (Value::Str(_), Value::Str(_)) => {
                crate::ops::compare(quire_ast::CmpOp::Eq, &x, want)?
            }
            (Value::Num(_) | Value::Str(_), Value::Num(_) | Value::Str(_)) => false,
            _ => bail!("findrows compares numbers and strings, not {} with {}", x.kind(), want.kind()),
        };
        if hit {
            data.extend((0..m.cols).map(|c| cell(m, r, c)).collect::<Result<Vec<_>>>()?);
            found += 1;
        }
    }
    if found == 0 {
        return Ok(Value::num(0.0));
    }
    Ok(Value::Matrix(Matrix::new(found, m.cols, data)))
}

fn matrix<'a>(name: &str, v: &'a Value) -> Result<&'a Matrix> {
    match v {
        Value::Matrix(m) => Ok(m),
        other => bail!("{name} needs a matrix, not {}", other.kind()),
    }
}

/// The element at 0-based (`r`, `c`); a short data vector is an error, not a panic.
fn cell(m: &Matrix, r: usize, c: usize) -> Result<Value> {
    m.data
        .get(r * m.cols + c)
        .cloned()
        .ok_or_else(|| EvalError::new("the matrix does not have as many elements as its size"))
}

/// A 1-based line number from `v`, which must lie in `1..=count`.
fn line_number(name: &str, noun: &str, v: &Value, count: usize, m: &Matrix) -> Result<usize> {
    let k = v.as_int(&format!("the {noun} number"))?;
    match usize::try_from(k) {
        Ok(k) if (1..=count).contains(&k) => Ok(k - 1),
        _ => bail!("{name}: {noun} {k} is outside the {}×{} matrix (counting starts at 1)", m.rows, m.cols),
    }
}

/// `col(M, j)`: column j of M, as a column vector.
fn column_of(args: &[Value]) -> Result<Value> {
    let [src, j] = args else { bail!("col takes 2 arguments: col(M, j)") };
    let m = matrix("col", src)?;
    let j = line_number("col", "column", j, m.cols, m)?;
    let data = (0..m.rows).map(|r| cell(m, r, j)).collect::<Result<Vec<_>>>()?;
    Ok(Value::Matrix(Matrix::column(data)))
}

/// `row(M, i)`: row i of M, as a row vector.
fn row_of(args: &[Value]) -> Result<Value> {
    let [src, i] = args else { bail!("row takes 2 arguments: row(M, i)") };
    let m = matrix("row", src)?;
    let i = line_number("row", "row", i, m.rows, m)?;
    let data = (0..m.cols).map(|c| cell(m, i, c)).collect::<Result<Vec<_>>>()?;
    Ok(Value::Matrix(Matrix::new(1, m.cols, data)))
}

/// `csort(M, j)` sorts the rows by column j, `rsort(M, i)` sorts the columns by row i,
/// both ascending. The key line must hold numbers of one unit; the sort is stable.
fn sort_lines(name: &str, args: &[Value], by_rows: bool) -> Result<Value> {
    let [src, key_arg] = args else { bail!("{name} takes 2 arguments") };
    let m = matrix(name, src)?;
    let (noun, count, lines) = if by_rows { ("column", m.cols, m.rows) } else { ("row", m.rows, m.cols) };
    let key = line_number(name, noun, key_arg, count, m)?;
    let mut keys: Vec<(f64, usize)> = Vec::with_capacity(lines);
    let mut unit: Option<Dim> = None;
    for line in 0..lines {
        let v = if by_rows { cell(m, line, key)? } else { cell(m, key, line)? };
        let q = v.as_num(&format!("an element of the {noun} that {name} sorts by"))?;
        match unit {
            None => unit = Some(q.dim),
            Some(d) => same_dim(d, q.dim, &format!("{name} sort by a {noun} that mixes"))?,
        }
        keys.push((q.v, line));
    }
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut data = Vec::with_capacity(m.data.len());
    if by_rows {
        for &(_, r) in &keys {
            for c in 0..m.cols {
                data.push(cell(m, r, c)?);
            }
        }
    } else {
        for r in 0..m.rows {
            for &(_, c) in &keys {
                data.push(cell(m, r, c)?);
            }
        }
    }
    Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
}

/// `tr(M)`: the sum of the diagonal of a square matrix, in its units.
fn trace(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("tr takes 1 argument") };
    let m = matrix("tr", src)?;
    if m.rows != m.cols {
        bail!("tr needs a square matrix, not {}×{}", m.rows, m.cols);
    }
    if m.rows == 0 {
        bail!("tr needs a matrix with at least one element");
    }
    let mut acc = cell(m, 0, 0)?;
    for k in 1..m.rows {
        acc = arith(BinaryOp::Add, &acc, &cell(m, k, k)?)?;
    }
    Ok(acc)
}

/// Degrees above this are refused: Durand–Kerner costs about n² per sweep.
const MAX_DEGREE: usize = 50;
/// Imaginary parts this small (relative to the root) count as zero, which absorbs the
/// error a double root leaves behind.
const REAL_TOLERANCE: f64 = 1e-7;

/// `polyroots(v)`: the roots of v[1] + v[2]·x + v[3]·x² + …, lowest power first, as a
/// column vector ordered by size, a negative root before its positive twin, which matches
/// SMath's order in the example worksheets. Coefficients may carry units; the roots then carry
/// the unit that makes every term the same.
fn polyroots(args: &[Value]) -> Result<Value> {
    let [src] = args else { bail!("polyroots takes 1 argument: polyroots(v)") };
    let m = matrix("polyroots", src)?;
    if !m.is_vector() {
        bail!("polyroots needs a vector of coefficients, not a {}×{} matrix", m.rows, m.cols);
    }
    let coeffs =
        m.data.iter().map(|v| v.as_num("a coefficient of polyroots")).collect::<Result<Vec<Quantity>>>()?;
    let (Some(top), Some(low)) =
        (coeffs.iter().rposition(|q| q.v != 0.0), coeffs.iter().position(|q| q.v != 0.0))
    else {
        bail!("polyroots needs at least one nonzero coefficient");
    };
    if top == 0 {
        bail!("polyroots needs a polynomial of degree 1 or more");
    }
    // The zero roots of x^low · p(x) are exact, so they come first and the rest is solved.
    let p = coeffs.get(low..=top).unwrap_or_default();
    let mut roots = vec![0.0; low];
    let mut unit = Dim::NONE;
    if let (Some(p0), Some(p1)) = (p.first(), p.get(1)) {
        unit = crate::builtins::dim_div(p0.dim, p1.dim)?;
        for (k, q) in p.iter().enumerate() {
            let want = crate::builtins::dim_mul(p0.dim, crate::builtins::dim_pow(unit, -(k as i32))?)?;
            if q.dim != want {
                bail!("polyroots needs coefficients whose units fit one polynomial in x");
            }
        }
    }
    roots.extend(solve_real(p)?);
    roots.sort_by(|a, b| by_size(*a, *b));
    Ok(column(roots, unit))
}

/// Order by absolute value, then negative before positive. Values whose sizes agree to
/// a relative 1e-9 count as equal, so the pair ±r sorts the same way whatever the rounding.
fn by_size(a: f64, b: f64) -> std::cmp::Ordering {
    let scale = a.abs().max(b.abs()).max(1.0);
    if (a.abs() - b.abs()).abs() <= 1e-9 * scale { a.total_cmp(&b) } else { a.abs().total_cmp(&b.abs()) }
}

/// The real roots of the nonzero polynomial with coefficients `p` (lowest power first).
fn solve_real(p: &[Quantity]) -> Result<Vec<f64>> {
    let degree = p.len().saturating_sub(1);
    if degree > MAX_DEGREE {
        bail!("polyroots handles polynomials up to degree {MAX_DEGREE}, not {degree}");
    }
    let Some(lead) = p.last().map(|q| q.v) else { return Ok(Vec::new()) };
    if degree == 0 {
        return Ok(Vec::new());
    }
    // Monic: x^n + low[n-1]·x^(n-1) + … + low[0]
    let low: Vec<f64> = p.iter().take(degree).map(|q| q.v / lead).collect();
    let z = durand_kerner(&low)?;
    let mut out = Vec::with_capacity(degree);
    for r in z {
        if r.im.abs() > REAL_TOLERANCE * r.re.abs().max(1.0) {
            bail!("polyroots: complex roots are not supported yet");
        }
        out.push(r.re);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    const ONE: Complex = Complex { re: 1.0, im: 0.0 };

    fn real(re: f64) -> Complex {
        Complex { re, im: 0.0 }
    }

    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    fn is_finite(self) -> bool {
        self.re.is_finite() && self.im.is_finite()
    }
}

impl Add for Complex {
    type Output = Complex;
    fn add(self, o: Complex) -> Complex {
        Complex { re: self.re + o.re, im: self.im + o.im }
    }
}

impl Sub for Complex {
    type Output = Complex;
    fn sub(self, o: Complex) -> Complex {
        Complex { re: self.re - o.re, im: self.im - o.im }
    }
}

impl Mul for Complex {
    type Output = Complex;
    fn mul(self, o: Complex) -> Complex {
        Complex { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}

impl Div for Complex {
    type Output = Complex;
    fn div(self, o: Complex) -> Complex {
        let d = o.re * o.re + o.im * o.im;
        Complex { re: (self.re * o.re + self.im * o.im) / d, im: (self.im * o.re - self.re * o.im) / d }
    }
}

/// All roots of the monic polynomial whose lower coefficients are `low` (length n, for
/// x^n + low[n-1]·x^(n-1) + … + low[0]), by simultaneous Durand–Kerner iteration.
fn durand_kerner(low: &[f64]) -> Result<Vec<Complex>> {
    let n = low.len();
    let not_converged =
        || EvalError::new("polyroots did not converge; the polynomial may have repeated roots");
    // Starting points on a spiral: distinct powers of a point off the axes.
    let seed = Complex { re: 0.4, im: 0.9 };
    let mut z = Vec::with_capacity(n);
    let mut power = Complex::ONE;
    for _ in 0..n {
        z.push(power);
        power = power * seed;
    }
    let scale_of = |z: &[Complex]| z.iter().fold(1.0f64, |m, r| m.max(r.abs()));
    for _ in 0..1000 {
        let next: Vec<Complex> = z
            .iter()
            .enumerate()
            .map(|(k, &zk)| {
                // p(zk), by Horner from the leading 1 down to low[0]
                let value = low.iter().rev().fold(Complex::ONE, |acc, &c| acc * zk + Complex::real(c));
                let denom = z
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != k)
                    .fold(Complex::ONE, |acc, (_, &zj)| acc * (zk - zj));
                if denom.abs() == 0.0 { zk } else { zk - value / denom }
            })
            .collect();
        if next.iter().any(|r| !r.is_finite()) {
            return Err(not_converged());
        }
        let change = z.iter().zip(&next).fold(0.0f64, |m, (&a, &b)| m.max((b - a).abs()));
        z = next;
        if change <= 1e-13 * scale_of(&z) {
            return Ok(z);
        }
    }
    Err(not_converged())
}

#[cfg(test)]
mod tests {
    use quire_units::Dim;

    use super::call;
    use crate::stats::tests::shown_with;
    use crate::value::{Matrix, Quantity, Value};

    /// The shown result of each line, at 6 decimals.
    fn shown(lines: &[&str]) -> Vec<String> {
        shown_with(6, lines)
    }

    /// Line `i` of a shown worksheet ("" past the end).
    fn at(r: &[String], i: usize) -> &str {
        r.get(i).map_or("", String::as_str)
    }

    /// The result of a built-in; a failure comes back as a value no assertion accepts.
    fn run(name: &str, args: &[Value]) -> Value {
        match call(name, args) {
            Some(Ok(v)) => v,
            _ => Value::Str(format!("{name} failed")),
        }
    }

    /// The message of a built-in's error, or "no error".
    fn error(name: &str, args: &[Value]) -> String {
        match call(name, args) {
            Some(Err(e)) => e.message,
            _ => String::from("no error"),
        }
    }

    fn mat(rows: usize, cols: usize, v: &[f64]) -> Value {
        Value::Matrix(Matrix::new(rows, cols, v.iter().map(|&x| Value::num(x)).collect()))
    }

    /// The SI magnitudes of a matrix's elements, in order.
    fn numbers(v: &Value) -> Vec<f64> {
        match v {
            Value::Matrix(m) => m.data.iter().map(|x| x.as_num("x").map_or(f64::NAN, |q| q.v)).collect(),
            _ => Vec::new(),
        }
    }

    fn assert_close(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-9, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn col_and_row_pick_lines_one_based() {
        let m = mat(2, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(numbers(&run("col", &[m.clone(), Value::num(2.0)])), vec![2.0, 5.0]);
        assert_eq!(numbers(&run("row", &[m.clone(), Value::num(2.0)])), vec![4.0, 5.0, 6.0]);
        let first_row = run("row", &[m.clone(), Value::num(1.0)]);
        assert!(matches!(&first_row, Value::Matrix(r) if r.rows == 1 && r.cols == 3));
        let first_col = run("col", &[m.clone(), Value::num(1.0)]);
        assert!(matches!(&first_col, Value::Matrix(c) if c.rows == 2 && c.cols == 1));
        assert!(error("col", &[m.clone(), Value::num(0.0)]).contains("column 0 is outside the 2×3 matrix"));
        assert!(error("col", &[m.clone(), Value::num(4.0)]).contains("column 4 is outside"));
        assert!(error("row", &[m.clone(), Value::num(3.0)]).contains("row 3 is outside"));
        assert!(error("row", &[m.clone(), Value::num(1.5)]).contains("whole number"));
        assert!(error("col", &[Value::num(1.0), Value::num(1.0)]).contains("needs a matrix"));
    }

    #[test]
    fn col_and_row_keep_units() {
        let length = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let m = Value::Matrix(Matrix::new(
            1,
            2,
            vec![Value::Num(Quantity::new(2.0, length)), Value::Num(Quantity::new(3.0, length))],
        ));
        let got = match run("row", &[m, Value::num(1.0)]) {
            Value::Matrix(r) => r.get(0, 1).and_then(|v| v.as_num("x").ok()).map(|q| (q.v, q.dim)),
            _ => None,
        };
        assert_eq!(got, Some((3.0, length)));
    }

    #[test]
    fn csort_orders_rows_by_a_column() {
        let m = mat(3, 2, &[3.0, 30.0, 1.0, 10.0, 2.0, 20.0]);
        let sorted = vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0];
        assert_eq!(numbers(&run("csort", &[m.clone(), Value::num(1.0)])), sorted);
        assert_eq!(numbers(&run("csort", &[m.clone(), Value::num(2.0)])), sorted);
        assert!(error("csort", &[m.clone(), Value::num(3.0)]).contains("column 3 is outside"));
        assert!(error("csort", &[m.clone(), Value::num(0.0)]).contains("column 0"));
    }

    #[test]
    fn rsort_orders_columns_by_a_row() {
        let r = mat(2, 3, &[2.0, 1.0, 3.0, 5.0, 4.0, 6.0]);
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(numbers(&run("rsort", &[r.clone(), Value::num(1.0)])), sorted);
        assert_eq!(numbers(&run("rsort", &[r.clone(), Value::num(2.0)])), sorted);
        assert!(error("rsort", &[r, Value::num(3.0)]).contains("row 3 is outside"));
    }

    #[test]
    fn csort_refuses_a_key_with_mixed_units() {
        let kg = Dim::from_ints([0, 1, 0, 0, 0, 0, 0]);
        let m = Value::Matrix(Matrix::new(2, 1, vec![Value::Num(Quantity::new(1.0, kg)), Value::num(2.0)]));
        assert!(error("csort", &[m, Value::num(1.0)]).contains("mixes"));
    }

    #[test]
    fn trace_sums_the_diagonal() {
        let m = mat(3, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(run("tr", &[m]), Value::num(15.0));
        assert_eq!(run("tr", &[mat(1, 1, &[7.0])]), Value::num(7.0));
        assert!(error("tr", &[mat(2, 3, &[1.0; 6])]).contains("square"));
        assert!(error("tr", &[Value::num(1.0)]).contains("needs a matrix"));
    }

    #[test]
    fn roots_of_a_quadratic_and_a_cubic() {
        // x² - 5x + 6 = (x - 2)(x - 3)
        assert_close(&numbers(&run("polyroots", &[mat(3, 1, &[6.0, -5.0, 1.0])])), &[2.0, 3.0]);
        // (x + 1)(x - 2)(x - 4) = x³ - 5x² + 2x + 8, lowest power first
        assert_close(&numbers(&run("polyroots", &[mat(4, 1, &[8.0, 2.0, -5.0, 1.0])])), &[-1.0, 2.0, 4.0]);
    }

    #[test]
    fn zero_roots_are_exact_and_come_out_in_order() {
        // x³ - 4x = x(x - 2)(x + 2), with a zero constant term
        let r = numbers(&run("polyroots", &[mat(4, 1, &[0.0, -4.0, 0.0, 1.0])]));
        assert_eq!(r, vec![0.0, -2.0, 2.0]);
        // x²·(x - 1)
        let r = numbers(&run("polyroots", &[mat(4, 1, &[0.0, 0.0, -1.0, 1.0])]));
        assert_eq!(r, vec![0.0, 0.0, 1.0]);
    }

    #[test]
    fn roots_of_a_hermite_polynomial() {
        // D(0) of the Hermite example: 15x - 10x³ + x⁵ (lowest power first)
        let r = numbers(&run("polyroots", &[mat(6, 1, &[0.0, 15.0, 0.0, -10.0, 0.0, 1.0])]));
        let outer = (5.0 + 10f64.sqrt()).sqrt();
        let inner = (5.0 - 10f64.sqrt()).sqrt();
        assert_close(&r, &[0.0, -inner, inner, -outer, outer]);
    }

    #[test]
    fn complex_roots_are_refused() {
        // x² + 1
        assert!(
            error("polyroots", &[mat(3, 1, &[1.0, 0.0, 1.0])])
                .contains("complex roots are not supported yet")
        );
    }

    #[test]
    fn polyroots_refuses_bad_input() {
        assert!(error("polyroots", &[mat(2, 1, &[0.0, 0.0])]).contains("nonzero coefficient"));
        assert!(error("polyroots", &[mat(1, 1, &[3.0])]).contains("degree 1 or more"));
        assert!(error("polyroots", &[mat(2, 2, &[1.0; 4])]).contains("vector of coefficients"));
        assert!(error("polyroots", &[Value::num(1.0)]).contains("needs a matrix"));
        let mut long = vec![1.0; 60];
        long.push(1.0);
        let n = long.len();
        assert!(error("polyroots", &[mat(n, 1, &long)]).contains("up to degree 50"));
    }

    #[test]
    fn polyroots_gives_units_of_the_variable() {
        let newton = Dim::from_ints([1, 1, -2, 0, 0, 0, 0]);
        let per_metre = Dim::from_ints([0, 1, -2, 0, 0, 0, 0]);
        // 2 kN − 4 kN/m · x = 0 at x = 0.5 m
        let c = Value::Matrix(Matrix::column(vec![
            Value::Num(Quantity::new(2000.0, newton)),
            Value::Num(Quantity::new(-4000.0, per_metre)),
        ]));
        let root = match run("polyroots", &[c]) {
            Value::Matrix(r) => r.get(0, 0).and_then(|v| v.as_num("root").ok()).map(|q| (q.v, q.dim)),
            _ => None,
        };
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(root, Some((v, d)) if (v - 0.5).abs() < 1e-12 && d == metre), "{root:?}");
    }

    #[test]
    fn matrix_makes_zeros() {
        let z = run("matrix", &[Value::num(2.0), Value::num(3.0)]);
        assert!(matches!(&z, Value::Matrix(m) if m.rows == 2 && m.cols == 3));
        assert_eq!(numbers(&z), vec![0.0; 6]);
        assert!(error("matrix", &[Value::num(0.0), Value::num(3.0)]).contains("at least 1"));
        assert!(error("matrix", &[Value::num(2000.0), Value::num(2000.0)]).contains("too large"));
        assert!(error("matrix", &[Value::num(2.0)]).contains("2 arguments"));
    }

    #[test]
    fn submatrix_cuts_a_block_ends_included() {
        let m = mat(3, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        let one = |v: f64| Value::num(v);
        let b = run("submatrix", &[m.clone(), one(2.0), one(3.0), one(1.0), one(2.0)]);
        assert!(matches!(&b, Value::Matrix(x) if x.rows == 2 && x.cols == 2));
        assert_eq!(numbers(&b), vec![4.0, 5.0, 7.0, 8.0]);
        assert_eq!(
            numbers(&run("submatrix", &[m.clone(), one(1.0), one(1.0), one(3.0), one(3.0)])),
            vec![3.0]
        );
        assert!(
            error("submatrix", &[m.clone(), one(3.0), one(2.0), one(1.0), one(1.0)])
                .contains("must not come before")
        );
        assert!(
            error("submatrix", &[m.clone(), one(1.0), one(4.0), one(1.0), one(1.0)])
                .contains("row 4 is outside")
        );
        assert!(error("submatrix", &[m, one(1.0), one(1.0)]).contains("5 arguments"));
    }

    #[test]
    fn diag_puts_a_vector_on_the_diagonal_with_its_units() {
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let v = Value::Matrix(Matrix::column(vec![
            Value::Num(Quantity::new(1.0, metre)),
            Value::Num(Quantity::new(2.0, metre)),
        ]));
        let d = run("diag", &[v]);
        assert_eq!(numbers(&d), vec![1.0, 0.0, 0.0, 2.0]);
        assert!(
            matches!(&d, Value::Matrix(m) if m.data.iter().all(|x| matches!(x, Value::Num(q) if q.dim == metre)))
        );
        assert_eq!(numbers(&run("diag", &[mat(1, 3, &[1.0, 2.0, 3.0])])).len(), 9);
        assert!(error("diag", &[mat(2, 2, &[1.0; 4])]).contains("needs a vector"));
    }

    #[test]
    fn reverse_flips_rows_or_a_row_vector() {
        assert_eq!(numbers(&run("reverse", &[mat(3, 1, &[1.0, 2.0, 3.0])])), vec![3.0, 2.0, 1.0]);
        assert_eq!(numbers(&run("reverse", &[mat(1, 3, &[1.0, 2.0, 3.0])])), vec![3.0, 2.0, 1.0]);
        assert_eq!(numbers(&run("reverse", &[mat(2, 2, &[1.0, 2.0, 3.0, 4.0])])), vec![3.0, 4.0, 1.0, 2.0]);
        assert!(error("reverse", &[Value::num(1.0)]).contains("needs a matrix"));
    }

    #[test]
    fn rank_counts_independent_rows() {
        let r = |m: Value| numbers(&Value::Matrix(Matrix::column(vec![run("rank", &[m])])));
        assert_eq!(r(mat(2, 2, &[1.0, 2.0, 2.0, 4.0])), vec![1.0]);
        assert_eq!(r(mat(3, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])), vec![2.0]);
        assert_eq!(r(mat(3, 3, &[2.0, -1.0, 0.0, -1.0, 2.0, -1.0, 0.0, -1.0, 2.0])), vec![3.0]);
        assert_eq!(r(mat(2, 3, &[0.0; 6])), vec![0.0]);
        assert_eq!(r(mat(3, 2, &[1.0, 0.0, 0.0, 1.0, 1.0, 1.0])), vec![2.0]);
        // a row of kN next to a row of m: [1 kN, 2 kN; 3 mm, 6 mm] has rank 1
        let newton = Dim::from_ints([1, 1, -2, 0, 0, 0, 0]);
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let q = |v: f64, d: Dim| Value::Num(Quantity::new(v, d));
        let mixed = Value::Matrix(Matrix::new(
            2,
            2,
            vec![q(1000.0, newton), q(2000.0, newton), q(0.003, metre), q(0.006, metre)],
        ));
        assert_eq!(r(mixed), vec![1.0]);
    }

    #[test]
    fn minors_and_cofactors() {
        // M = [2 −1 0; −1 2 −1; 0 −1 2], worked by hand
        let m = mat(3, 3, &[2.0, -1.0, 0.0, -1.0, 2.0, -1.0, 0.0, -1.0, 2.0]);
        let ij = |i: f64, j: f64| [m.clone(), Value::num(i), Value::num(j)];
        assert_eq!(run("minor", &ij(1.0, 1.0)), Value::num(3.0));
        assert_eq!(run("minor", &ij(1.0, 2.0)), Value::num(-2.0));
        assert_eq!(run("alg", &ij(1.0, 2.0)), Value::num(2.0));
        assert_eq!(run("alg", &ij(2.0, 2.0)), Value::num(4.0));
        assert_eq!(run("cofactor", &ij(3.0, 2.0)), Value::num(2.0));
        assert_eq!(run("vminor", &ij(1.0, 1.0)), Value::num(3.0));
        // expanding along the first row gives the determinant, 4
        let det: f64 = (1..=3)
            .map(|j| {
                let a = [2.0, -1.0, 0.0][j - 1];
                a * numbers(&Value::Matrix(Matrix::column(vec![run("alg", &ij(1.0, j as f64))])))[0]
            })
            .sum();
        assert_eq!(det, 4.0);
        assert!(error("minor", &ij(4.0, 1.0)).contains("row 4 is outside"));
        assert!(error("minor", &[mat(2, 3, &[1.0; 6]), Value::num(1.0), Value::num(1.0)]).contains("square"));
    }

    #[test]
    fn matrix_norms() {
        // A = [1 −2; 3 4]: column sums 4, 6; row sums 3, 7; √(1 + 4 + 9 + 16) = √30
        let a = mat(2, 2, &[1.0, -2.0, 3.0, 4.0]);
        assert_eq!(run("norm1", std::slice::from_ref(&a)), Value::num(6.0));
        assert_eq!(run("normi", std::slice::from_ref(&a)), Value::num(7.0));
        assert_eq!(run("norme", &[a]), Value::num(30f64.sqrt()));
        // a column vector: Σ|v|, max |v|, length
        let v = mat(3, 1, &[3.0, -4.0, 0.0]);
        assert_eq!(run("norm1", std::slice::from_ref(&v)), Value::num(7.0));
        assert_eq!(run("normi", std::slice::from_ref(&v)), Value::num(4.0));
        assert_eq!(run("norme", &[v]), Value::num(5.0));
        let kg = Dim::from_ints([0, 1, 0, 0, 0, 0, 0]);
        let mixed =
            Value::Matrix(Matrix::new(2, 1, vec![Value::Num(Quantity::new(1.0, kg)), Value::num(2.0)]));
        assert!(error("norme", &[mixed]).contains("mix"));
    }

    #[test]
    fn invert_a_matrix_or_a_number() {
        let inv = run("invert", &[mat(2, 2, &[2.0, 0.0, 0.0, 4.0])]);
        assert_eq!(numbers(&inv), vec![0.5, 0.0, 0.0, 0.25]);
        assert_eq!(run("invert", &[Value::num(4.0)]), Value::num(0.25));
        assert!(error("invert", &[mat(2, 2, &[1.0, 2.0, 2.0, 4.0])]).contains("singular"));
    }

    #[test]
    fn findrows_picks_matching_rows_or_gives_zero() {
        let mut data = Vec::new();
        for (k, s) in [(1.0, "a"), (2.0, "b"), (1.0, "c")] {
            data.push(Value::num(k));
            data.push(Value::Str(s.to_string()));
        }
        let m = Value::Matrix(Matrix::new(3, 2, data));
        let ones = run("findrows", &[m.clone(), Value::num(1.0), Value::num(1.0)]);
        let want = Value::Matrix(Matrix::new(
            2,
            2,
            vec![Value::num(1.0), Value::Str("a".into()), Value::num(1.0), Value::Str("c".into())],
        ));
        assert_eq!(ones, want);
        let b = run("findrows", &[m.clone(), Value::Str("b".into()), Value::num(2.0)]);
        assert_eq!(b, Value::Matrix(Matrix::new(1, 2, vec![Value::num(2.0), Value::Str("b".into())])));
        assert_eq!(run("findrows", &[m.clone(), Value::num(5.0), Value::num(1.0)]), Value::num(0.0));
        // a string never equals a number
        assert_eq!(run("findrows", &[m.clone(), Value::Str("1".into()), Value::num(1.0)]), Value::num(0.0));
        assert!(error("findrows", &[m, Value::num(1.0), Value::num(3.0)]).contains("column 3 is outside"));
    }

    #[test]
    fn new_functions_on_a_worksheet() {
        let r = shown(&[
            "M := [1 m, 2 m; 3 m, 4 m]",
            "minor(M, 1, 1)",
            "alg(M, 1, 2)",
            "norme([3 kN; 4 kN])",
            "submatrix(M, 1, 2, 2, 2)",
            "det(diag([2; 3]))",
            "rank(M)",
        ]);
        assert_eq!(at(&r, 1), "4 'm");
        assert_eq!(at(&r, 2), "-3 'm");
        assert_eq!(at(&r, 3), "5000 'N");
        assert_eq!(at(&r, 4), "[2 'm; 4 'm]");
        assert_eq!(at(&r, 5), "6");
        assert_eq!(at(&r, 6), "2");
    }

    #[test]
    fn worksheet_lines_use_the_functions() {
        let r = shown(&[
            "M := [1, 2, 3; 4, 5, 6]",
            "col(M, 2)",
            "row(M, 2)",
            "tr([1, 2; 3, 4])",
            "csort([3, 30; 1, 10], 1)",
            "col(M, 9)",
        ]);
        assert_eq!(at(&r, 1), "[2; 5]");
        assert_eq!(at(&r, 2), "[4, 5, 6]");
        assert_eq!(at(&r, 3), "5");
        assert_eq!(at(&r, 4), "[1, 10; 3, 30]");
        assert!(at(&r, 5).contains("column 9 is outside"), "{r:?}");
    }
}
