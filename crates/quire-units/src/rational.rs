use std::fmt;
use std::ops::Neg;

/// Small exact rational used for dimension exponents. Always reduced, `d > 0`, and both
/// parts within `±i32::MAX`, so negating never overflows.
///
/// Arithmetic is checked: a result that doesn't fit (`('m^1000000)^1000000`, or the
/// denominators of many nested square roots) is `None`, never a wrapped or rounded value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    n: i32,
    d: i32,
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

impl Default for Rational {
    fn default() -> Self {
        Rational::ZERO
    }
}

impl Rational {
    pub const ZERO: Rational = Rational { n: 0, d: 1 };
    pub const ONE: Rational = Rational { n: 1, d: 1 };

    /// `n/d` reduced; `None` when `d` is zero or the reduced parts don't fit.
    pub fn new(n: i32, d: i32) -> Option<Rational> {
        Rational::reduce(n.into(), d.into())
    }

    /// Exact arithmetic is done in i64 (products of two i32 parts fit), then reduced.
    fn reduce(n: i64, d: i64) -> Option<Rational> {
        if d == 0 {
            return None;
        }
        let g = gcd(n, d).max(1);
        let s = if d < 0 { -1 } else { 1 };
        let part = |x: i64| i32::try_from(s * x / g).ok().filter(|x| *x != i32::MIN);
        Some(Rational { n: part(n)?, d: part(d)? })
    }

    pub const fn int(n: i32) -> Rational {
        Rational { n, d: 1 }
    }

    /// The nearest rational with denominator ≤ 12, if `x` is (numerically) one.
    pub fn approx(x: f64) -> Option<Rational> {
        if !x.is_finite() || x.abs() > 1e6 {
            return None;
        }
        (1..=12).find_map(|d| {
            let n = (x * d as f64).round();
            ((n / d as f64 - x).abs() < 1e-9).then(|| Rational::new(n as i32, d)).flatten()
        })
    }

    pub fn checked_add(self, o: Rational) -> Option<Rational> {
        let (a, b, c, d) = (i64::from(self.n), i64::from(self.d), i64::from(o.n), i64::from(o.d));
        Rational::reduce(a.checked_mul(d)?.checked_add(c.checked_mul(b)?)?, b.checked_mul(d)?)
    }

    pub fn checked_sub(self, o: Rational) -> Option<Rational> {
        self.checked_add(-o)
    }

    pub fn checked_mul(self, o: Rational) -> Option<Rational> {
        let (a, b, c, d) = (i64::from(self.n), i64::from(self.d), i64::from(o.n), i64::from(o.d));
        Rational::reduce(a.checked_mul(c)?, b.checked_mul(d)?)
    }

    pub fn numer(self) -> i32 {
        self.n
    }

    pub fn denom(self) -> i32 {
        self.d
    }

    pub fn is_zero(self) -> bool {
        self.n == 0
    }

    pub fn is_integer(self) -> bool {
        self.d == 1
    }

    pub fn to_f64(self) -> f64 {
        self.n as f64 / self.d as f64
    }

    pub fn abs(self) -> Rational {
        Rational { n: self.n.abs(), d: self.d }
    }
}

impl Neg for Rational {
    type Output = Rational;
    fn neg(self) -> Rational {
        Rational { n: -self.n, d: self.d }
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.d == 1 { write!(f, "{}", self.n) } else { write!(f, "{}/{}", self.n, self.d) }
    }
}

#[cfg(test)]
mod tests {
    use super::Rational;

    fn r(n: i32, d: i32) -> Option<Rational> {
        Rational::new(n, d)
    }

    #[test]
    fn arithmetic_reduces() {
        assert_eq!(r(2, 4), r(1, 2));
        assert_eq!(r(1, -2), r(-1, 2));
        let (half, third) = (Rational::new(1, 2), Rational::new(1, 3));
        assert_eq!(half.zip(third).and_then(|(a, b)| a.checked_add(b)), r(5, 6));
        assert_eq!(half.and_then(|h| Rational::int(2).checked_mul(h)), Some(Rational::ONE));
        assert_eq!(Rational::approx(0.5), r(1, 2));
        assert_eq!(Rational::approx(1.0 / 3.0), r(1, 3));
        assert_eq!(Rational::approx(std::f64::consts::PI), None);
    }

    #[test]
    fn results_that_do_not_fit_are_none() {
        assert_eq!(r(1, 0), None);
        assert_eq!(r(i32::MIN, 1), None);
        let big = Rational::int(1_000_000);
        assert_eq!(big.checked_mul(big), None);
        assert_eq!(Rational::int(i32::MAX).checked_add(Rational::ONE), None);
        assert_eq!(Rational::int(-i32::MAX).checked_sub(Rational::ONE), None);
        // the denominator of 33 nested square roots is 2^33
        let mut e = Some(Rational::ONE);
        for _ in 0..33 {
            e = e.zip(r(1, 2)).and_then(|(e, h)| e.checked_mul(h));
        }
        assert_eq!(e, None);
        // a sum whose parts reduce back into range still works
        let x = r(1, 1 << 30);
        assert_eq!(x.zip(x).and_then(|(a, b)| a.checked_add(b)), r(1, 1 << 29));
    }
}
