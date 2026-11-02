use std::fmt;

use crate::Rational;

/// SI base units, in the order of [`Dim`]'s exponents: L, M, T, I, Θ, N, J.
/// Angles are dimensionless (rad = 1), as in SMath and Mathcad.
pub const BASE_UNITS: [&str; 7] = ["m", "kg", "s", "A", "K", "mol", "cd"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Dim(pub [Rational; 7]);

impl Dim {
    pub const NONE: Dim = Dim([Rational::ZERO; 7]);

    pub fn from_ints(e: [i8; 7]) -> Dim {
        Dim(e.map(|x| Rational::int(x as i32)))
    }

    pub fn is_dimensionless(&self) -> bool {
        self.0.iter().all(|r| r.is_zero())
    }

    /// Each exponent times `r`; `None` when one no longer fits (see [`Rational`]).
    pub fn pow(self, r: Rational) -> Option<Dim> {
        let mut out = self.0;
        for e in &mut out {
            *e = e.checked_mul(r)?;
        }
        Some(Dim(out))
    }

    /// The dimension of a product; `None` when an exponent no longer fits.
    pub fn checked_mul(self, o: Dim) -> Option<Dim> {
        zip_with(self, o, Rational::checked_add)
    }

    /// The dimension of a quotient; `None` when an exponent no longer fits.
    pub fn checked_div(self, o: Dim) -> Option<Dim> {
        zip_with(self, o, Rational::checked_sub)
    }

    /// `(base unit, exponent)` for each non-zero exponent.
    pub fn parts(&self) -> Vec<(&'static str, Rational)> {
        BASE_UNITS.iter().zip(self.0).filter(|(_, e)| !e.is_zero()).map(|(n, e)| (*n, e)).collect()
    }
}

/// Combines the exponents of two dimensions component by component.
fn zip_with(a: Dim, b: Dim, f: fn(Rational, Rational) -> Option<Rational>) -> Option<Dim> {
    let mut out = a.0;
    for (x, y) in out.iter_mut().zip(b.0) {
        *x = f(*x, y)?;
    }
    Some(Dim(out))
}

impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = format_product(&self.parts(), false);
        f.write_str(if s.is_empty() { "1 (dimensionless)" } else { &s })
    }
}

fn power(name: &str, e: Rational, quote: bool) -> String {
    let q = if quote { "'" } else { "" };
    if e == Rational::ONE {
        format!("{q}{name}")
    } else if e.is_integer() {
        format!("{q}{name}^{e}")
    } else {
        format!("{q}{name}^({e})")
    }
}

/// Product of unit powers as linear syntax: `kg*m/s^2`, `1/s`, `m/(s^2*A)`.
/// With `quote`, every unit gets its `'` marker. Empty input gives "".
pub fn format_product(parts: &[(&str, Rational)], quote: bool) -> String {
    let num: Vec<String> =
        parts.iter().filter(|(_, e)| e.numer() > 0).map(|(n, e)| power(n, *e, quote)).collect();
    let den: Vec<String> =
        parts.iter().filter(|(_, e)| e.numer() < 0).map(|(n, e)| power(n, e.abs(), quote)).collect();
    let mut s = if num.is_empty() && !den.is_empty() { "1".to_string() } else { num.join("*") };
    match den.as_slice() {
        [] => {}
        [only] => {
            s.push('/');
            s.push_str(only);
        }
        _ => {
            s.push_str("/(");
            s.push_str(&den.join("*"));
            s.push(')');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(Dim::from_ints([1, 1, -2, 0, 0, 0, 0]).to_string(), "m*kg/s^2");
        assert_eq!(Dim::from_ints([0, 0, -1, 0, 0, 0, 0]).to_string(), "1/s");
        assert_eq!(Dim::from_ints([1, 0, -2, -1, 0, 0, 0]).to_string(), "m/(s^2*A)");
        let quarter = Rational::new(1, 4).and_then(|r| Dim::from_ints([2, 0, 0, 0, 0, 0, 0]).pow(r));
        assert_eq!(quarter.map(|d| d.to_string()).as_deref(), Some("m^(1/2)"));
        assert_eq!(Dim::NONE.to_string(), "1 (dimensionless)");
    }
}
