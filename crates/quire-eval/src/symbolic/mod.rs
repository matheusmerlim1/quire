//! Symbolic math, shown with `→` (SMath's symbolic evaluation).
//!
//! Names stay symbols: `q*x*(L - x)/2 →` keeps `q` and `L`, and the document's functions
//! are expanded into their bodies. On top of simplification (like terms collected,
//! numbers folded exactly, powers combined, fractions cancelled) it knows `diff`, `int`
//! (indefinite and definite), `expand`, `factor`, `simplify`, `solve` (polynomials,
//! equations with fractions, simple transcendental ones and linear systems), `series`
//! (`taylor`), `lim` and `subs`.
//!
//! Expressions are kept in a canonical form built by the smart constructors [`add`],
//! [`mul`], [`pow`] and [`func`], so two equal expressions have equal keys. Names are
//! taken to be positive, as lengths, areas and stiffnesses are: `sqrt(h^2) → h`.

use std::collections::HashMap;

use quire_ast::{BinaryOp, CmpOp, Expr, UnaryOp};

pub(crate) mod bridge;
mod integrate;
mod limit;
mod poly;
mod simplify;
mod solve;
#[cfg(test)]
mod tests;
mod work;

use integrate::integrate;
use simplify::{simplify, tidy};

// ---------------------------------------------------------------------------
// Numbers: exact rationals, falling back to floats for decimals and overflow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Rat(i128, i128),
    Float(f64),
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn lcm(a: i128, b: i128) -> Option<i128> {
    (a / gcd(a, b)).checked_mul(b).map(i128::abs)
}

/// `n = a^q * r` with `a` as large as possible: 12 → (2, 3) for square roots.
fn perfect_part(n: i128, q: u32) -> (i128, i128) {
    let (mut a, mut r) = (1i128, n);
    let mut p = 2i128;
    while p.checked_pow(q).is_some_and(|pq| pq <= r) && p <= 1_000_000 {
        let pq = p.pow(q);
        if r % pq == 0 {
            r /= pq;
            a *= p;
        } else {
            p += 1;
        }
    }
    (a, r)
}

impl Num {
    fn int(n: i128) -> Num {
        Num::Rat(n, 1)
    }

    fn rat(n: i128, d: i128) -> Num {
        if d == 0 {
            return Num::Float(f64::NAN);
        }
        let g = gcd(n, d) * d.signum();
        Num::Rat(n / g, d / g)
    }

    fn f(self) -> f64 {
        match self {
            Num::Rat(n, d) => n as f64 / d as f64,
            Num::Float(x) => x,
        }
    }

    fn is(self, n: i128) -> bool {
        matches!(self, Num::Rat(a, 1) if a == n)
    }

    fn is_zero(self) -> bool {
        self.f() == 0.0
    }

    fn integer(self) -> Option<i128> {
        match self {
            Num::Rat(n, 1) => Some(n),
            _ => None,
        }
    }

    /// As an exact fraction; a typed decimal like 0.25 counts as 1/4.
    fn ratio(self) -> Option<(i128, i128)> {
        match self {
            Num::Rat(..) => Some(self),
            Num::Float(x) if x.is_finite() && x.abs() < 1e12 => (0..=9).find_map(|k| {
                let d = 10f64.powi(k);
                let n = (x * d).round();
                ((n - x * d).abs() < 1e-9 * d).then(|| Num::rat(n as i128, 10i128.pow(k as u32)))
            }),
            _ => None,
        }
        .and_then(|r| match r {
            Num::Rat(n, d) => Some((n, d)),
            Num::Float(_) => None,
        })
    }

    fn add(self, o: Num) -> Num {
        match (self, o) {
            (Num::Rat(a, b), Num::Rat(c, d)) => {
                match (a.checked_mul(d), c.checked_mul(b), b.checked_mul(d)) {
                    (Some(x), Some(y), Some(z)) if x.checked_add(y).is_some() => Num::rat(x + y, z),
                    _ => Num::Float(self.f() + o.f()),
                }
            }
            _ => Num::Float(self.f() + o.f()),
        }
    }

    fn mul(self, o: Num) -> Num {
        match (self, o) {
            (Num::Rat(a, b), Num::Rat(c, d)) => match (a.checked_mul(c), b.checked_mul(d)) {
                (Some(x), Some(y)) => Num::rat(x, y),
                _ => Num::Float(self.f() * o.f()),
            },
            _ => Num::Float(self.f() * o.f()),
        }
    }

    fn neg(self) -> Num {
        self.mul(Num::int(-1))
    }

    fn recip(self) -> Num {
        match self {
            Num::Rat(n, d) => Num::rat(d, n),
            Num::Float(x) => Num::Float(1.0 / x),
        }
    }

    /// `self^e`, exactly where possible (integer powers, perfect roots).
    fn pow(self, e: Num) -> Option<Num> {
        if let Some(k) = e.integer() {
            let k32 = i32::try_from(k).ok()?;
            return Some(match self {
                Num::Rat(n, d) if k32.unsigned_abs() <= 64 => {
                    let (n, d) = if k32 < 0 { (d, n) } else { (n, d) };
                    match (n.checked_pow(k32.unsigned_abs()), d.checked_pow(k32.unsigned_abs())) {
                        (Some(a), Some(b)) => Num::rat(a, b),
                        _ => Num::Float(self.f().powi(k32)),
                    }
                }
                _ => Num::Float(self.f().powi(k32)),
            });
        }
        match (self, e) {
            // perfect roots: 4^(1/2) = 2, 27^(1/3) = 3
            (Num::Rat(n, d), Num::Rat(p, q)) if n >= 0 && q > 1 && q <= 4 => {
                let root = |x: i128| {
                    let r = (x as f64).powf(1.0 / q as f64).round() as i128;
                    (r.checked_pow(q as u32) == Some(x)).then_some(r)
                };
                let (rn, rd) = (root(n)?, root(d)?);
                Num::rat(rn, rd).pow(Num::int(p))
            }
            (Num::Float(x), _) => Some(Num::Float(x.powf(e.f()))),
            (_, Num::Float(y)) => Some(Num::Float(self.f().powf(y))),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum S {
    N(Num),
    Sym(String),
    Unit(String),
    Add(Vec<S>),
    Mul(Vec<S>),
    Pow(Box<S>, Box<S>),
    Fn(String, Vec<S>),
    /// Several results (`solve` with two roots): shown as a column vector.
    Vec(Vec<S>),
}

pub(crate) type Res<T> = std::result::Result<T, String>;

/// The `i`-th argument of a call. `call` checks the argument count first.
fn ex(args: &[Expr], i: usize) -> Res<&Expr> {
    args.get(i).ok_or_else(|| "missing argument".to_string())
}

const ZERO: S = S::N(Num::Rat(0, 1));
const ONE: S = S::N(Num::Rat(1, 1));

fn n(k: i128) -> S {
    S::N(Num::int(k))
}

fn num_of(s: &S) -> Option<Num> {
    match s {
        S::N(x) => Some(*x),
        _ => None,
    }
}

impl S {
    /// A canonical text key: equal expressions have equal keys.
    fn key(&self) -> String {
        // keys are built over and over as terms are collected: the work grows with them
        if !work::spend(1) {
            return String::new();
        }
        match self {
            S::N(Num::Rat(a, 1)) => a.to_string(),
            S::N(Num::Rat(a, b)) => format!("{a}/{b}"),
            S::N(Num::Float(x)) => format!("{x:?}"),
            S::Sym(s) => s.clone(),
            S::Unit(u) => format!("'{u}"),
            S::Add(ts) => format!("({})", ts.iter().map(S::key).collect::<Vec<_>>().join("+")),
            S::Mul(fs) => format!("({})", fs.iter().map(S::key).collect::<Vec<_>>().join("*")),
            S::Pow(b, e) => format!("{}^{}", b.key(), e.key()),
            S::Fn(f, xs) => format!("{f}({})", xs.iter().map(S::key).collect::<Vec<_>>().join(",")),
            S::Vec(xs) => format!("[{}]", xs.iter().map(S::key).collect::<Vec<_>>().join(";")),
        }
    }

    fn depends(&self, x: &str) -> bool {
        if !work::spend(1) {
            return false;
        }
        match self {
            S::Sym(s) => s == x,
            S::N(_) | S::Unit(_) => false,
            S::Add(xs) | S::Mul(xs) | S::Fn(_, xs) | S::Vec(xs) => xs.iter().any(|t| t.depends(x)),
            S::Pow(b, e) => b.depends(x) || e.depends(x),
        }
    }

    /// Total degree, for ordering terms: x^2 before x before constants.
    fn degree(&self) -> f64 {
        match self {
            S::N(_) | S::Unit(_) => 0.0,
            S::Sym(_) | S::Fn(..) => 1.0,
            S::Pow(b, e) => b.degree() * num_of(e).map_or(1.0, Num::f),
            S::Mul(fs) => fs.iter().map(S::degree).sum(),
            S::Add(ts) => ts.iter().map(S::degree).fold(0.0, f64::max),
            S::Vec(_) => 0.0,
        }
    }
}

/// Split a term into its numeric coefficient and the rest: `3*x*y` → (3, x*y).
fn coef(t: &S) -> (Num, S) {
    match t {
        S::N(c) => (*c, ONE),
        S::Mul(fs) => match fs.first() {
            Some(S::N(c)) => {
                let rest: Vec<S> = fs.iter().skip(1).cloned().collect();
                (
                    *c,
                    match <[S; 1]>::try_from(rest) {
                        Ok([only]) => only,
                        Err(rest) => S::Mul(rest),
                    },
                )
            }
            _ => (Num::int(1), t.clone()),
        },
        _ => (Num::int(1), t.clone()),
    }
}

/// Split a factor into base and exponent: `x^3` → (x, 3), `y` → (y, 1).
fn base_exp(f: &S) -> (S, S) {
    match f {
        S::Pow(b, e) => ((**b).clone(), (**e).clone()),
        _ => (f.clone(), ONE),
    }
}

pub(crate) fn add(terms: Vec<S>) -> S {
    if !work::spend(terms.len()) {
        return ZERO;
    }
    let mut flat = Vec::new();
    for t in terms {
        match t {
            S::Add(ts) => flat.extend(ts),
            // a number times a sum is spread over it, so (a + b) - (a - b) cancels
            S::Mul(fs) if matches!(fs.as_slice(), [S::N(_), S::Add(_)]) => {
                if let [S::N(c), S::Add(ts)] = fs.as_slice() {
                    flat.extend(ts.iter().map(|t| mul(vec![S::N(*c), t.clone()])));
                }
            }
            t => flat.push(t),
        }
    }
    if work::too_many(flat.len()) {
        return ZERO;
    }
    let mut constant = Num::int(0);
    // like terms: rest key → (rest, summed coefficient), in order of first appearance
    let mut order: Vec<String> = Vec::new();
    let mut like: HashMap<String, (S, Num)> = HashMap::new();
    for t in flat {
        if let S::N(c) = t {
            constant = constant.add(c);
            continue;
        }
        let (c, rest) = coef(&t);
        let k = rest.key();
        match like.get_mut(&k) {
            Some((_, sum)) => *sum = sum.add(c),
            None => {
                order.push(k.clone());
                like.insert(k, (rest, c));
            }
        }
    }
    let mut out: Vec<S> = order
        .into_iter()
        .filter_map(|k| {
            let (rest, c) = like.remove(&k)?;
            (!c.is_zero()).then(|| mul(vec![S::N(c), rest]))
        })
        .collect();
    out.sort_by(term_order);
    if !constant.is_zero() {
        out.push(S::N(constant));
    }
    match out.pop() {
        None => ZERO,
        Some(only) if out.is_empty() => only,
        Some(last) => {
            out.push(last);
            S::Add(out)
        }
    }
}

/// A term's powers of each name, if it is a monomial (`3*a^2*b` → {a: 2, b: 1}).
fn monomial(t: &S) -> Option<Vec<(String, f64)>> {
    let factors = match coef(t).1 {
        S::Mul(fs) => fs,
        S::N(_) => vec![],
        f => vec![f],
    };
    let mut out = Vec::new();
    for f in factors {
        let (b, e) = base_exp(&f);
        let name = match b {
            S::Sym(s) => s.to_lowercase(),
            S::Unit(u) => format!("~{u}"),
            _ => return None,
        };
        out.push((name, num_of(&e)?.f()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// The order terms are written in, as in textbooks: higher degree first; among equal
/// degrees, polynomial terms before others (-b + sqrt(…)), positive before negative
/// (b^2 - 4*a*c), then higher powers of the first name (a^3 + 3*a^2*b + 3*a*b^2 + b^3).
fn term_order(a: &S, b: &S) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let by_powers = || match (monomial(a), monomial(b)) {
        (Some(ma), Some(mb)) => {
            let mut names: Vec<&String> = ma.iter().chain(&mb).map(|(n, _)| n).collect();
            names.sort();
            names.dedup();
            let power =
                |m: &[(String, f64)], n: &String| m.iter().find(|(k, _)| k == n).map_or(0.0, |(_, e)| *e);
            names
                .into_iter()
                .map(|n| power(&mb, n).total_cmp(&power(&ma, n)))
                .find(|o| *o != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let sign = |t: &S| coef(t).0.f() < 0.0;
    let poly = |t: &S| monomial(t).is_none();
    b.degree()
        .total_cmp(&a.degree())
        .then_with(|| poly(a).cmp(&poly(b)))
        .then_with(|| sign(a).cmp(&sign(b)))
        .then_with(by_powers)
        .then_with(|| coef(a).1.key().to_lowercase().cmp(&coef(b).1.key().to_lowercase()))
}

pub(crate) fn mul(factors: Vec<S>) -> S {
    if !work::spend(factors.len()) {
        return ZERO;
    }
    let mut flat = Vec::new();
    for f in factors {
        match f {
            S::Mul(fs) => flat.extend(fs),
            f => flat.push(f),
        }
    }
    if work::too_many(flat.len()) {
        return ZERO;
    }
    let mut c = Num::int(1);
    let mut order: Vec<String> = Vec::new();
    let mut bases: HashMap<String, (S, Vec<S>)> = HashMap::new();
    for f in flat {
        if let S::N(x) = f {
            c = c.mul(x);
            continue;
        }
        let (b, e) = base_exp(&f);
        let k = b.key();
        match bases.get_mut(&k) {
            Some((_, es)) => es.push(e),
            None => {
                order.push(k.clone());
                bases.insert(k, (b, vec![e]));
            }
        }
    }
    if c.is_zero() {
        return ZERO;
    }
    let mut out: Vec<S> = Vec::new();
    for k in order {
        let Some((b, es)) = bases.remove(&k) else { continue };
        match pow(b, add(es)) {
            S::N(x) => c = c.mul(x),
            S::Mul(fs) => {
                for f in fs {
                    match f {
                        S::N(x) => c = c.mul(x),
                        f => out.push(f),
                    }
                }
            }
            p => out.push(p),
        }
    }
    // symbols first, then functions, units last (5*x*'m reads "5 x m")
    let rank = |f: &S| match base_exp(f).0 {
        S::Unit(_) => 3,
        S::Fn(..) => 1,
        S::Add(_) => 2,
        _ => 0,
    };
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| base_exp(a).0.key().cmp(&base_exp(b).0.key())));
    if !c.is(1) || out.is_empty() {
        out.insert(0, S::N(c));
    }
    match out.pop() {
        Some(only) if out.is_empty() => only,
        Some(last) => {
            out.push(last);
            S::Mul(out)
        }
        None => S::Mul(out),
    }
}

pub(crate) fn pow(base: S, e: S) -> S {
    if !work::spend(1) {
        return ZERO;
    }
    if let Some(k) = num_of(&e) {
        if k.is_zero() {
            return ONE;
        }
        if k.is(1) {
            return base;
        }
        if let Some(b) = num_of(&base) {
            if b.is_zero() && k.f() > 0.0 {
                return ZERO;
            }
            if let Some(v) = b.pow(k) {
                return S::N(v);
            }
            // roots pulled out where they can be: sqrt(12) = 2*sqrt(3), sqrt(1/2) = 1/sqrt(2)
            if let (Num::Rat(bn, bd), Num::Rat(p @ (1 | -1), q @ 2..=4)) = (b, k)
                && bn > 0
            {
                let ((an, rn), (ad, rd)) = (perfect_part(bn, q as u32), perfect_part(bd, q as u32));
                if an > 1 || ad > 1 || rd > 1 {
                    let surd = |r: i128, s: i128| match r {
                        1 => ONE,
                        r => S::Pow(Box::new(n(r)), Box::new(S::N(Num::rat(s, q)))),
                    };
                    return mul(vec![
                        S::N(Num::rat(an, ad).pow(Num::int(p)).unwrap_or(Num::int(1))),
                        surd(rn, p),
                        surd(rd, -p),
                    ]);
                }
            }
        }
    }
    if let Some(b) = num_of(&base)
        && b.is(1)
    {
        return ONE;
    }
    let int_exp = num_of(&e).and_then(Num::integer).is_some();
    match base {
        // (x^a)^b = x^(a*b) when b is a whole number
        S::Pow(b, a) if int_exp => pow(*b, mul(vec![*a, e])),
        S::Mul(fs) if int_exp => mul(fs.into_iter().map(|f| pow(f, e.clone())).collect()),
        base => S::Pow(Box::new(base), Box::new(e)),
    }
}

fn sym(name: &str) -> S {
    S::Sym(name.to_string())
}

pub(crate) fn func(name: &str, args: Vec<S>) -> S {
    if !work::spend(1 + args.len()) {
        return ZERO;
    }
    let one = args.len() == 1;
    let a = args.first();
    let zero = a.is_some_and(|x| *x == ZERO);
    if one
        && let Some(x) = a
        && let Some(v) = exact_value(name, x)
    {
        return v;
    }
    // exp(ln(x)) = x, ln(exp(x)) = x, ln(e^k) = k
    if let (true, Some(x)) = (one, a) {
        match (name, x) {
            ("ln", S::Pow(b, k)) if **b == sym("e") => return (**k).clone(),
            ("exp", S::Fn(f, inner)) if f == "ln" => {
                if let Some(v) = inner.first() {
                    return v.clone();
                }
            }
            ("ln", S::Fn(f, inner)) if f == "exp" => {
                if let Some(v) = inner.first() {
                    return v.clone();
                }
            }
            _ => {}
        }
    }
    // |n| for a number
    if let (true, Some(x), "abs") = (one, a, name)
        && let Some(n) = num_of(x)
    {
        return match n {
            Num::Rat(p, q) => S::N(Num::rat(p.abs(), q)),
            Num::Float(f) => S::N(Num::Float(f.abs())),
        };
    }
    match name {
        "sin" | "tan" | "asin" | "atan" | "sinh" | "tanh" if one && zero => ZERO,
        "cos" | "cosh" | "exp" if one && zero => ONE,
        "exp" if one && a == Some(&ONE) => sym("e"),
        "ln" if one && a == Some(&ONE) => ZERO,
        "ln" if one && a == Some(&sym("e")) => ONE,
        // odd and even functions: sin(-x) = -sin(x), cos(-x) = cos(x)
        "sin" | "tan" | "sinh" | "tanh" | "asin" | "atan" if one && a.is_some_and(negative) => {
            neg(func(name, vec![neg(first_arg(a))]))
        }
        "cos" | "cosh" if one && a.is_some_and(negative) => func(name, vec![neg(first_arg(a))]),
        _ => S::Fn(name.to_string(), args),
    }
}

/// The argument of a one-argument call; the callers check there is one.
fn first_arg(a: Option<&S>) -> S {
    a.cloned().unwrap_or(ZERO)
}

/// Values a table gives exactly: sin(π/6) = 1/2, cos(π) = -1, atan(1) = π/4.
fn exact_value(name: &str, a: &S) -> Option<S> {
    let half = |k: i128| S::N(Num::rat(k, 2));
    let root = |r: i128| pow(n(r), half(1));
    let pi_times = |c: Num| mul(vec![S::N(c), sym("π")]);
    match name {
        "asin" if *a == ONE => return Some(pi_times(Num::rat(1, 2))),
        "acos" if *a == ONE => return Some(ZERO),
        "acos" if *a == ZERO => return Some(pi_times(Num::rat(1, 2))),
        "atan" if *a == ONE => return Some(pi_times(Num::rat(1, 4))),
        _ => {}
    }
    // a rational multiple of π, in twelfths of a turn
    let (c, rest) = coef(a);
    if rest != sym("π") && *a != sym("π") {
        return None;
    }
    let c = if *a == sym("π") { Num::int(1) } else { c };
    let Num::Rat(p, q) = c else { return None };
    if 12 % q != 0 {
        return None;
    }
    let k = (p * (12 / q)).rem_euclid(24); // angle in units of π/12
    // sin of k·π/12 for k = 0, 2, 3, 4, 6 (0°, 30°, 45°, 60°, 90°), by symmetry elsewhere
    let sin_k = |k: i128| -> Option<S> {
        let (k, sign) = if k >= 12 { (k - 12, -1) } else { (k, 1) };
        let k = if k > 6 { 12 - k } else { k };
        let v = match k {
            0 => ZERO,
            2 => half(1),
            3 => mul(vec![half(1), root(2)]),
            4 => mul(vec![half(1), root(3)]),
            6 => ONE,
            _ => return None,
        };
        Some(mul(vec![n(sign), v]))
    };
    match name {
        "sin" => sin_k(k),
        "cos" => sin_k((k + 6) % 24),
        "tan" => {
            let (s, c) = (sin_k(k)?, sin_k((k + 6) % 24)?);
            (c != ZERO).then(|| div(s, c))
        }
        _ => None,
    }
}

fn neg(s: S) -> S {
    mul(vec![n(-1), s])
}

fn div(a: S, b: S) -> S {
    mul(vec![a, pow(b, n(-1))])
}

// ---------------------------------------------------------------------------
// From the AST, with the document's functions expanded
// ---------------------------------------------------------------------------

/// The document's functions, by name and arity: parameters and body.
pub(crate) type Functions<'a> = &'a dyn Fn(&str, usize) -> Option<(Vec<String>, Expr)>;

pub(crate) struct Ctx<'a> {
    pub functions: Functions<'a>,
    /// Parameters bound while a function body is expanded.
    bound: HashMap<String, S>,
    depth: usize,
    /// Calls of [`from_expr`] in progress.
    nesting: usize,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(functions: Functions<'a>) -> Self {
        Ctx { functions, bound: HashMap::new(), depth: 0, nesting: 0 }
    }
}

fn parse_num(text: &str) -> S {
    if let Ok(i) = text.parse::<i128>() {
        return n(i);
    }
    S::N(Num::Float(text.parse().unwrap_or(f64::NAN)))
}

fn var_name(e: &Expr, what: &str) -> Res<String> {
    match e {
        Expr::Ident(v) => Ok(v.clone()),
        _ => Err(format!("{what} should be a name, like x")),
    }
}

pub(crate) fn from_expr(e: &Expr, cx: &mut Ctx) -> Res<S> {
    // the document's functions expand into each other, so this can go deeper than the
    // expression as parsed
    if cx.nesting >= work::MAX_NESTING {
        return Err(work::TOO_DEEP.into());
    }
    cx.nesting += 1;
    let r = build(e, cx);
    cx.nesting -= 1;
    r
}

fn build(e: &Expr, cx: &mut Ctx) -> Res<S> {
    Ok(match e {
        Expr::Num(t) => parse_num(t),
        Expr::Ident(name) => match cx.bound.get(name) {
            Some(v) => v.clone(),
            None if name == "pi" => sym("π"),
            None => sym(name),
        },
        Expr::Unit(u) => S::Unit(u.clone()),
        Expr::Paren(x) => from_expr(x, cx)?,
        Expr::Unary(UnaryOp::Neg, x) => neg(from_expr(x, cx)?),
        Expr::Unary(UnaryOp::Pos, x) => from_expr(x, cx)?,
        Expr::Binary(op, a, b) => {
            let (a, b) = (from_expr(a, cx)?, from_expr(b, cx)?);
            match op {
                BinaryOp::Add => add(vec![a, b]),
                BinaryOp::Sub => add(vec![a, neg(b)]),
                BinaryOp::Mul | BinaryOp::Juxt => mul(vec![a, b]),
                BinaryOp::Div => div(a, b),
                BinaryOp::Pow => pow(a, b),
                _ => return Err(format!("→ does not handle the operator {}", op.symbol().trim())),
            }
        }
        Expr::Call(name, args) => call(name, args, cx)?,
        Expr::Compare(..) => {
            return Err("→ works on expressions; to solve an equation write solve(a == b, x)".into());
        }
        Expr::Matrix(_) | Expr::Index(..) | Expr::Range(..) => {
            return Err("→ works on single expressions, not matrices or ranges".into());
        }
        Expr::Str(_) => return Err("→ works on math, not text".into()),
        Expr::Factorial(_) | Expr::Unary(..) => return Err("→ does not handle this operator".into()),
        Expr::Placeholder => return Err("fill in the empty slot (?) first".into()),
        Expr::Define { .. } => return Err("→ shows the right-hand side of a definition".into()),
    })
}

/// `a == b` as `a - b` (to be zero); a plain expression is taken as `… == 0`.
fn equation(e: &Expr, cx: &mut Ctx) -> Res<S> {
    match e {
        Expr::Compare(a, rest) => match rest.as_slice() {
            [(CmpOp::Eq, b)] => Ok(add(vec![from_expr(a, cx)?, neg(from_expr(b, cx)?)])),
            _ => Err("solve needs an equation with ==, like solve(a*x == b, x)".into()),
        },
        e => from_expr(e, cx),
    }
}

/// `series(f, x, x0, n)`: the Taylor polynomial's terms, lowest power first.
fn series_call(name: &str, args: &[Expr], cx: &mut Ctx) -> Res<Vec<S>> {
    let x = var_name(ex(args, 1)?, &format!("the variable of {name}"))?;
    let order = match ex(args, 3)? {
        Expr::Num(t) => t.parse::<usize>().ok().filter(|k| *k <= 20),
        _ => None,
    }
    .ok_or_else(|| format!("{name}'s order should be a whole number up to 20"))?;
    let f = from_expr(ex(args, 0)?, cx)?;
    limit::series(&f, &x, &from_expr(ex(args, 2)?, cx)?, order)
}

/// An expression as the user would type it, for messages.
fn show(e: &S) -> String {
    let e = tidy(e);
    if work::shown_depth(&e) > work::MAX_SHOWN_DEPTH {
        return "this expression".into();
    }
    print(&to_expr(&e))
}

fn call(name: &str, args: &[Expr], cx: &mut Ctx) -> Res<S> {
    let arg = |i: usize, cx: &mut Ctx| from_expr(ex(args, i)?, cx);
    match (name, args.len()) {
        ("diff", 2 | 3) => {
            let x = var_name(ex(args, 1)?, "the variable of diff")?;
            let times = match args.get(2) {
                Some(Expr::Num(t)) => {
                    t.parse::<usize>().map_err(|_| "diff's order should be a whole number")?
                }
                Some(_) => return Err("diff's order should be a whole number".into()),
                None => 1,
            };
            let mut f = arg(0, cx)?;
            for _ in 0..times {
                if work::exhausted() {
                    break;
                }
                f = diff(&f, &x);
            }
            Ok(f)
        }
        ("int", 2) => {
            let x = var_name(ex(args, 1)?, "the variable of int")?;
            integrate(&arg(0, cx)?, &x)
        }
        ("int", 4) => {
            let x = var_name(ex(args, 1)?, "the variable of int")?;
            let f = integrate(&arg(0, cx)?, &x)?;
            let (a, b) = (arg(2, cx)?, arg(3, cx)?);
            // the antiderivative at the limits; a limit where it cannot simply be put in
            let at = |p: &S| {
                limit::limit(&f, &x, p).map_err(|e| format!("the integral at {x} = {}: {e}", show(p)))
            };
            Ok(add(vec![at(&b)?, neg(at(&a)?)]))
        }
        ("expand", 1) => Ok(expand(&arg(0, cx)?)),
        ("simplify", 1) => Ok(simplify(&arg(0, cx)?)),
        ("factor", 1) => Ok(poly::factor(&arg(0, cx)?)),
        ("subs" | "substitute", 3) => {
            let x = var_name(ex(args, 1)?, &format!("the second argument of {name}"))?;
            Ok(subs(&arg(0, cx)?, &x, &arg(2, cx)?))
        }
        ("series" | "taylor", 4) => Ok(add(series_call(name, args, cx)?)),
        ("series" | "taylor", _) => {
            Err(format!("write {name}(f, x, x0, n): f around x = x0, up to (x - x0)^n"))
        }
        ("lim" | "limit", 3) => {
            let x = var_name(ex(args, 1)?, "the variable of lim")?;
            limit::limit(&arg(0, cx)?, &x, &arg(2, cx)?)
        }
        ("solve", 2) => match (ex(args, 0)?, ex(args, 1)?) {
            (Expr::Matrix(eqs), Expr::Matrix(xs)) => {
                let eqs: Vec<S> = eqs.iter().flatten().map(|e| equation(e, cx)).collect::<Res<_>>()?;
                let xs: Vec<String> =
                    xs.iter().flatten().map(|v| var_name(v, "each unknown of solve")).collect::<Res<_>>()?;
                solve::solve_system(&eqs, &xs)
            }
            (Expr::Matrix(_), _) => {
                Err("list the unknowns too: solve([x + y == 3; x - y == 1], [x; y])".into())
            }
            (eq, v) => {
                let x = var_name(v, "the unknown of solve")?;
                solve::solve(&equation(eq, cx)?, &x)
            }
        },
        ("sqrt", 1) => Ok(pow(arg(0, cx)?, S::N(Num::rat(1, 2)))),
        ("root", 2) => {
            let k = arg(1, cx)?;
            Ok(pow(arg(0, cx)?, pow(k, n(-1))))
        }
        ("exp", 1) if args.first() == Some(&Expr::Num("1".into())) => Ok(sym("e")),
        _ => {
            if cx.depth < 32
                && let Some((params, body)) = (cx.functions)(name, args.len())
            {
                // the document's own function: its body, with the arguments put in
                let values: Vec<S> = args.iter().map(|a| from_expr(a, cx)).collect::<Res<_>>()?;
                let saved = std::mem::take(&mut cx.bound);
                cx.bound = params.into_iter().zip(values).collect();
                cx.depth += 1;
                let out = from_expr(&body, cx);
                cx.depth -= 1;
                cx.bound = saved;
                // a body with the arguments put in is as deep as both together
                let out = out?;
                work::check_depth(&out)?;
                return Ok(out);
            }
            let xs: Vec<S> = args.iter().map(|a| from_expr(a, cx)).collect::<Res<_>>()?;
            Ok(func(name, xs))
        }
    }
}

// ---------------------------------------------------------------------------
// Calculus and algebra
// ---------------------------------------------------------------------------

pub(crate) fn subs(e: &S, x: &str, v: &S) -> S {
    let s = |t: &S| subs(t, x, v);
    match e {
        S::Sym(name) if name == x => v.clone(),
        S::N(_) | S::Sym(_) | S::Unit(_) => e.clone(),
        S::Add(ts) => add(ts.iter().map(s).collect()),
        S::Mul(fs) => mul(fs.iter().map(s).collect()),
        S::Pow(b, p) => pow(s(b), s(p)),
        S::Fn(f, xs) => func(f, xs.iter().map(s).collect()),
        S::Vec(xs) => S::Vec(xs.iter().map(s).collect()),
    }
}

pub(crate) fn diff(e: &S, x: &str) -> S {
    if !work::spend(1) || !e.depends(x) {
        return ZERO;
    }
    match e {
        S::Sym(_) => ONE,
        S::Add(ts) => add(ts.iter().map(|t| diff(t, x)).collect()),
        S::Mul(fs) => add(fs
            .iter()
            .enumerate()
            .filter(|(_, f)| f.depends(x))
            .map(|(i, _)| {
                mul(fs.iter().enumerate().map(|(j, g)| if j == i { diff(g, x) } else { g.clone() }).collect())
            })
            .collect()),
        S::Pow(b, p) if !p.depends(x) => {
            mul(vec![(**p).clone(), pow((**b).clone(), add(vec![(**p).clone(), n(-1)])), diff(b, x)])
        }
        // a^b = exp(b*ln(a))
        S::Pow(b, p) => mul(vec![
            e.clone(),
            add(vec![
                mul(vec![diff(p, x), func("ln", vec![(**b).clone()])]),
                mul(vec![(**p).clone(), diff(b, x), pow((**b).clone(), n(-1))]),
            ]),
        ]),
        S::Fn(f, args) if args.len() == 1 => {
            // the guard above checks there is exactly one argument
            let u = args.first().cloned().unwrap_or(ZERO);
            let du = diff(&u, x);
            let outer = match f.as_str() {
                "sin" => func("cos", vec![u]),
                "cos" => neg(func("sin", vec![u])),
                "tan" => pow(func("cos", vec![u]), n(-2)),
                "exp" => e.clone(),
                "ln" => pow(u, n(-1)),
                "log" => pow(mul(vec![u, func("ln", vec![n(10)])]), n(-1)),
                "sinh" => func("cosh", vec![u]),
                "cosh" => func("sinh", vec![u]),
                "tanh" => pow(func("cosh", vec![u]), n(-2)),
                "asin" => pow(add(vec![ONE, neg(pow(u, n(2)))]), S::N(Num::rat(-1, 2))),
                "acos" => neg(pow(add(vec![ONE, neg(pow(u, n(2)))]), S::N(Num::rat(-1, 2)))),
                "atan" => pow(add(vec![ONE, pow(u, n(2))]), n(-1)),
                "abs" => func("sign", vec![u]),
                _ => return S::Fn("diff".into(), vec![e.clone(), sym(x)]),
            };
            mul(vec![outer, du])
        }
        _ => S::Fn("diff".into(), vec![e.clone(), sym(x)]),
    }
}

/// Products of sums multiplied out, whole powers of sums expanded.
pub(crate) fn expand(e: &S) -> S {
    match e {
        S::Add(ts) => add(ts.iter().map(expand).collect()),
        S::Mul(fs) => fs.iter().map(expand).fold(ONE, |acc, f| distribute(&acc, &f)),
        S::Pow(b, p) => {
            let b = expand(b);
            match (num_of(p).and_then(Num::integer), &b) {
                (Some(k), S::Add(_)) if (2..=12).contains(&k) => {
                    (0..k).fold(ONE, |acc, _| distribute(&acc, &b))
                }
                _ => pow(b, (**p).clone()),
            }
        }
        S::Fn(f, xs) => func(f, xs.iter().map(expand).collect()),
        _ => e.clone(),
    }
}

fn terms(e: &S) -> Vec<S> {
    match e {
        S::Add(ts) => ts.clone(),
        t => vec![t.clone()],
    }
}

fn distribute(a: &S, b: &S) -> S {
    let mut out = Vec::new();
    for x in terms(a) {
        for y in terms(b) {
            out.push(mul(vec![x.clone(), y]));
        }
    }
    add(out)
}

// ---------------------------------------------------------------------------
// Back to the AST, the way people write it
// ---------------------------------------------------------------------------

fn num_expr(x: Num) -> Expr {
    match x {
        Num::Rat(a, 1) if a < 0 => Expr::Unary(UnaryOp::Neg, Box::new(Expr::Num((-a).to_string()))),
        Num::Rat(a, 1) => Expr::Num(a.to_string()),
        Num::Rat(a, b) => {
            // -1/2 rather than -(1/2)
            let top = Expr::Num(a.abs().to_string());
            let top = if a < 0 { Expr::Unary(UnaryOp::Neg, Box::new(top)) } else { top };
            Expr::Binary(BinaryOp::Div, Box::new(top), Box::new(Expr::Num(b.to_string())))
        }
        Num::Float(f) if f < 0.0 => Expr::Unary(UnaryOp::Neg, Box::new(Expr::Num(format_float(-f)))),
        Num::Float(f) => Expr::Num(format_float(f)),
    }
}

fn format_float(f: f64) -> String {
    let s = format!("{}", (f * 1e12).round() / 1e12);
    if s.contains('e') { format!("{f}") } else { s }
}

fn product(fs: Vec<Expr>) -> Expr {
    let mut it = fs.into_iter();
    let first = it.next().unwrap_or(Expr::Num("1".into()));
    it.fold(first, |acc, f| Expr::Binary(BinaryOp::Mul, Box::new(acc), Box::new(f)))
}

/// Is this expression negative as written (a leading minus)?
fn negative(e: &S) -> bool {
    coef(e).0.f() < 0.0
}

/// `-2*x` rather than `-(2*x)`: the minus goes on the first factor.
fn negate_leading(e: Expr) -> Expr {
    match e {
        Expr::Binary(op @ (BinaryOp::Mul | BinaryOp::Div), a, b) => {
            Expr::Binary(op, Box::new(negate_leading(*a)), b)
        }
        e => Expr::Unary(UnaryOp::Neg, Box::new(e)),
    }
}

/// Terms written one after another, in the order given: `a - b + c`.
fn sum_expr(ts: &[S]) -> Expr {
    let term = |t: &S| match to_expr(t) {
        // a bare sum as a term of another, as in a series in (x - 1): keep its brackets
        e @ Expr::Binary(BinaryOp::Add | BinaryOp::Sub, ..) => Expr::Paren(Box::new(e)),
        e => e,
    };
    // 1 - sqrt(5) rather than -sqrt(5) + 1
    let mut ts = ts.to_vec();
    if ts.len() > 1
        && ts.first().is_some_and(negative)
        && ts.last().is_some_and(|t| matches!(t, S::N(_)) && !negative(t))
    {
        ts.rotate_right(1);
    }
    let mut it = ts.iter();
    let Some(first) = it.next() else { return Expr::Num("0".into()) };
    let mut out = term(first);
    for t in it {
        out = if negative(t) {
            Expr::Binary(BinaryOp::Sub, Box::new(out), Box::new(term(&neg(t.clone()))))
        } else {
            Expr::Binary(BinaryOp::Add, Box::new(out), Box::new(term(t)))
        };
    }
    out
}

pub(crate) fn to_expr(e: &S) -> Expr {
    match e {
        S::N(x) => num_expr(*x),
        S::Sym(s) => Expr::Ident(s.clone()),
        S::Unit(u) => Expr::Unit(u.clone()),
        S::Add(ts) => sum_expr(ts),
        S::Mul(_) => {
            let (c, rest) = coef(e);
            if c.f() < 0.0 {
                return negate_leading(to_expr(&mul(vec![S::N(c.neg()), rest])));
            }
            let factors = match rest {
                S::Mul(fs) => fs,
                f => vec![f],
            };
            let (mut top, mut bottom) = (Vec::new(), Vec::new());
            if let Num::Rat(a, b) = c {
                if a != 1 {
                    top.push(Expr::Num(a.to_string()));
                }
                if b != 1 {
                    bottom.push(Expr::Num(b.to_string()));
                }
            } else {
                top.push(num_expr(c));
            }
            for f in factors {
                let (b, p) = base_exp(&f);
                match num_of(&p) {
                    Some(k) if k.f() < 0.0 => bottom.push(to_expr(&pow(b, S::N(k.neg())))),
                    _ => top.push(to_expr(&f)),
                }
            }
            let top = product(top);
            if bottom.is_empty() {
                top
            } else {
                Expr::Binary(BinaryOp::Div, Box::new(top), Box::new(product(bottom)))
            }
        }
        S::Pow(b, p) => match num_of(p) {
            Some(Num::Rat(1, 2)) => Expr::Call("sqrt".into(), vec![to_expr(b)]),
            Some(k) if k.f() < 0.0 => Expr::Binary(
                BinaryOp::Div,
                Box::new(Expr::Num("1".into())),
                Box::new(to_expr(&pow((**b).clone(), S::N(k.neg())))),
            ),
            _ => Expr::Binary(BinaryOp::Pow, Box::new(to_expr(b)), Box::new(to_expr(p))),
        },
        S::Fn(f, xs) => Expr::Call(f.clone(), xs.iter().map(to_expr).collect()),
        S::Vec(xs) => Expr::Matrix(xs.iter().map(|x| vec![to_expr(x)]).collect()),
    }
}

fn print(e: &Expr) -> String {
    quire_ast::print(e)
}

/// `expr →`: the expression worked out symbolically, as linear text.
pub(crate) fn evaluate(e: &Expr, functions: Functions) -> Res<String> {
    work::budgeted(|| evaluate_within(e, functions))
}

fn evaluate_within(e: &Expr, functions: Functions) -> Res<String> {
    let mut cx = Ctx::new(functions);
    // a series reads from the lowest power up, so it is written out term by term
    if let Expr::Call(f, args) = e
        && matches!(f.as_str(), "series" | "taylor")
        && args.len() == 4
    {
        let terms: Vec<S> = series_call(f, args, &mut cx)?.iter().map(tidy).collect();
        let deepest = terms.iter().map(work::shown_depth).max().unwrap_or(0);
        if work::exhausted() || terms.len() + deepest > work::MAX_SHOWN_DEPTH {
            return Err(work::TOO_MUCH_WORK.into());
        }
        return Ok(print(&sum_expr(&terms)));
    }
    let s = from_expr(e, &mut cx)?;
    work::check_depth(&s)?;
    // expand() and factor() ask for a form; anything else is simplified and shown
    // factored where it can be
    let shown = match e {
        Expr::Call(f, _) if f == "expand" || f == "factor" => s,
        _ => tidy(&simplify(&s)),
    };
    // what ran out of work collapsed to 0: the result must not be shown
    if work::exhausted() || work::shown_depth(&shown) > work::MAX_SHOWN_DEPTH {
        return Err(work::TOO_MUCH_WORK.into());
    }
    Ok(print(&to_expr(&shown)))
}

/// The value of `e` as a float, given values for its names (units count as 1).
pub(crate) fn approx(e: &S, value: &dyn Fn(&str) -> Option<f64>) -> Option<f64> {
    let v = |t: &S| approx(t, value);
    Some(match e {
        S::N(x) => x.f(),
        S::Sym(s) if s == "π" => std::f64::consts::PI,
        S::Sym(s) if s == "e" => std::f64::consts::E,
        S::Sym(s) if s == "∞" => f64::INFINITY,
        S::Sym(s) => value(s)?,
        S::Unit(_) => 1.0,
        S::Add(ts) => ts.iter().map(v).sum::<Option<f64>>()?,
        S::Mul(fs) => fs.iter().map(v).product::<Option<f64>>()?,
        S::Pow(b, p) => v(b)?.powf(v(p)?),
        S::Fn(f, xs) if xs.len() == 1 => {
            let a = v(xs.first()?)?;
            match f.as_str() {
                "sin" => a.sin(),
                "cos" => a.cos(),
                "tan" => a.tan(),
                "asin" => a.asin(),
                "acos" => a.acos(),
                "atan" => a.atan(),
                "sinh" => a.sinh(),
                "cosh" => a.cosh(),
                "tanh" => a.tanh(),
                "asinh" => a.asinh(),
                "exp" => a.exp(),
                "ln" => a.ln(),
                "log" => a.log10(),
                "abs" => a.abs(),
                "sign" => a.signum(),
                _ => return None,
            }
        }
        _ => return None,
    })
    .filter(|x| !x.is_nan())
}
