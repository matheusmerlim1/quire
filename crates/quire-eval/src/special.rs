//! SMath's remaining built-ins that only need their arguments' values: the Gamma and error
//! functions, the real and imaginary parts of a (real) number, strings (`strlen`,
//! `str2num`, `IsString`), units (`UnitsOf`) and `error`. Also the messages for SMath
//! names that Quire does not support, or that are not SMath built-ins at all.

use std::f64::consts::PI;

use crate::builtins::{arg, arity};
use crate::error::{EvalError, Result, bail};
use crate::ops::{map, uniform};
use crate::value::{Quantity, Value};

/// `None` when `name` is not one of these.
pub fn call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let r = match name {
        "Gamma" => one_scalar(name, args).and_then(gamma),
        "erf" => one_scalar(name, args).map(|x| Value::num(erf(x))),
        "Re" => one(name, args).and_then(|v| map_parts(v, |q| q)),
        "Im" => one(name, args).and_then(|v| map_parts(v, |q| Quantity::new(0.0, q.dim))),
        "arg" => {
            one(name, args).and_then(|v| map_parts(v, |q| Quantity::scalar(if q.v < 0.0 { PI } else { 0.0 })))
        }
        "strlen" => one_string(name, args).map(|s| Value::num(s.chars().count() as f64)),
        "str2num" => one_string(name, args).and_then(str2num),
        "IsString" => one(name, args).map(|v| Value::bool(matches!(v, Value::Str(_)))),
        "UnitsOf" => one(name, args).and_then(units_of),
        "error" => one(name, args).and_then(raise),
        _ => return None,
    };
    Some(r)
}

/// Why `name` is not a function in Quire, when that deserves more than "not a known function".
pub fn unsupported(name: &str) -> Option<String> {
    let why = match name {
        "random" => "random is not supported: a worksheet gives the same results every time it is calculated",
        "eigenvals" | "numden" => return Some(format!("{name} is not supported yet")),
        "wfile" | "rfile" | "dfile" | "importData" | "ImportData" => {
            return Some(format!(
                "{name} reads or writes files, which worksheets in Quire don't do: for data, use Insert ▸ Table from CSV…"
            ));
        }
        "gcd" | "lcm" => {
            return Some(format!(
                "{name} is not an SMath built-in (SMath has it only through its Maxima plugin): define it in the worksheet"
            ));
        }
        "last" => "last is not an SMath built-in: write v[length(v)] for the last element",
        "cumsum" => {
            "cumsum is not an SMath built-in: write sum(v[k], k, 1, i) for the sum of the first i elements"
        }
        "lookup" => "lookup is not an SMath built-in: findrows(M, x, j) picks the rows whose column j is x",
        "SetUnits" => "SetUnits is not an SMath built-in: multiply by the unit instead (x*1 m)",
        _ => return None,
    };
    Some(why.to_string())
}

fn one<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value> {
    arity(name, args, |n| n == 1, "1 argument")?;
    arg(args, 0)
}

fn one_scalar(name: &str, args: &[Value]) -> Result<f64> {
    one(name, args)?.as_scalar(&format!("the argument of {name}"))
}

fn one_string<'a>(name: &str, args: &'a [Value]) -> Result<&'a str> {
    match one(name, args)? {
        Value::Str(s) => Ok(s),
        other => bail!("{name} needs a string, not {}", other.kind()),
    }
}

/// `Re`, `Im`, `arg` of a number, or of each element of a matrix. Numbers in Quire are real,
/// so these only matter for worksheets written with complex results in mind.
fn map_parts(v: &Value, f: fn(Quantity) -> Quantity) -> Result<Value> {
    match v {
        Value::Num(q) => Ok(Value::Num(f(*q))),
        Value::Matrix(m) => map(m, |e| map_parts(e, f)),
        other => bail!("this needs a number, not {}", other.kind()),
    }
}

/// Lanczos coefficients for g = 7, n = 9: about 15 significant digits for x ≥ 0.5.
const LANCZOS: [f64; 9] = [
    0.999_999_999_999_809_9,
    676.520_368_121_885_1,
    -1_259.139_216_722_402_8,
    771.323_428_777_653_1,
    -176.615_029_162_140_6,
    12.507_343_278_686_905,
    -0.138_571_095_265_720_12,
    9.984_369_578_019_572e-6,
    1.505_632_735_149_311_6e-7,
];

/// `Gamma(x)`: Γ(x), with Γ(n) = (n − 1)! for whole n. Below 1/2 by the reflection
/// Γ(x)·Γ(1 − x) = π / sin(πx); the non-positive whole numbers are its poles.
fn gamma(x: f64) -> Result<Value> {
    if x <= 0.0 && x.fract() == 0.0 {
        bail!("Gamma({x}) is infinite: Gamma has poles at 0, −1, −2, …");
    }
    let g = gamma_of(x);
    if !g.is_finite() {
        bail!("Gamma({x}) is too large: results go up to about 1.8e308");
    }
    Ok(Value::num(g))
}

fn gamma_of(x: f64) -> f64 {
    if x < 0.5 {
        return PI / ((PI * x).sin() * gamma_of(1.0 - x));
    }
    // whole numbers up to 20 exactly, as products
    if x.fract() == 0.0 && x <= 21.0 {
        return (1..x as u64).fold(1.0, |p, k| p * k as f64);
    }
    let x = x - 1.0;
    let mut sum = LANCZOS[0];
    for (k, c) in LANCZOS.iter().enumerate().skip(1) {
        sum += c / (x + k as f64);
    }
    let t = x + 7.5;
    // t^(x + 0.5) split in two halves so that it overflows only when Γ itself does
    let half = t.powf((x + 0.5) / 2.0);
    (2.0 * PI).sqrt() * half * (half * (-t).exp()) * sum
}

/// `erf(x)` = 2/√π ∫₀ˣ e^(−t²) dt. Below |x| = 2 by the series
/// 2/√π · e^(−x²) · Σ 2ⁿ x^(2n+1) / (1·3·…·(2n+1)), whose terms are all positive (no
/// cancellation); above, as 1 − erfc(x) with erfc from its continued fraction
/// e^(−x²)/√π · 1/(x + ½/(x + 1/(x + 3⁄2/(x + …)))). Past |x| = 6, erf is ±1 to double precision.
pub(crate) fn erf(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let a = x.abs();
    if a >= 6.0 {
        return x.signum();
    }
    let x2 = x * x;
    if a >= 2.0 {
        let mut f = a;
        for n in (1..=120).rev() {
            f = a + (n as f64 / 2.0) / f;
        }
        let erfc = (-x2).exp() / PI.sqrt() / f;
        return x.signum() * (1.0 - erfc);
    }
    let mut term = x;
    let mut sum = x;
    for n in 1..200 {
        term *= 2.0 * x2 / (2 * n + 1) as f64;
        sum += term;
        if term.abs() <= 1e-17 * sum.abs() {
            break;
        }
    }
    2.0 / PI.sqrt() * (-x2).exp() * sum
}

/// `str2num("12.5")`: the number written in a string (a decimal comma works too).
fn str2num(s: &str) -> Result<Value> {
    let t = s.trim();
    let normal = if t.contains('.') { t.to_string() } else { t.replace(',', ".") };
    match normal.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(Value::num(v)),
        _ => bail!("str2num: \"{s}\" is not a number"),
    }
}

/// `UnitsOf(x)`: 1 in the units of x (`UnitsOf(5 kN)` is 1 N, in SI); 1 for a plain number.
fn units_of(v: &Value) -> Result<Value> {
    let dim = match v {
        Value::Num(q) => q.dim,
        Value::Matrix(m) => uniform(m, "an element of UnitsOf's matrix")?.1,
        other => bail!("UnitsOf needs a number, not {}", other.kind()),
    };
    Ok(Value::Num(Quantity::new(1.0, dim)))
}

/// `error("message")`: stops with the given message, as SMath shows it under the line.
fn raise(v: &Value) -> Result<Value> {
    match v {
        Value::Str(s) => Err(EvalError::new(s.clone())),
        Value::Num(q) => Err(EvalError::new(crate::format::format_number(q.v, 4, &Default::default()))),
        other => bail!("error needs a message (a string), not {}", other.kind()),
    }
}

#[cfg(test)]
mod tests {
    use quire_units::Dim;

    use super::{call, erf, unsupported};
    use crate::value::{Matrix, Quantity, Value};

    fn run(name: &str, args: &[Value]) -> Value {
        match call(name, args) {
            Some(Ok(v)) => v,
            other => Value::Str(format!("{name} failed: {other:?}")),
        }
    }

    fn error(name: &str, args: &[Value]) -> String {
        match call(name, args) {
            Some(Err(e)) => e.message,
            _ => String::from("no error"),
        }
    }

    fn number(v: &Value) -> f64 {
        v.as_num("x").map_or(f64::NAN, |q| q.v)
    }

    fn close(got: f64, want: f64, rel: f64) -> bool {
        (got - want).abs() <= rel * want.abs().max(1e-300)
    }

    #[test]
    fn gamma_matches_factorials_and_known_values() {
        // Γ(n) = (n − 1)!
        assert_eq!(number(&run("Gamma", &[Value::num(5.0)])), 24.0);
        assert_eq!(number(&run("Gamma", &[Value::num(1.0)])), 1.0);
        // Γ(1/2) = √π; Γ(−1/2) = −2√π; Γ(3/2) = √π/2
        let rp = std::f64::consts::PI.sqrt();
        for (x, want) in [(0.5, rp), (-0.5, -2.0 * rp), (1.5, rp / 2.0)] {
            let got = number(&run("Gamma", &[Value::num(x)]));
            assert!(close(got, want, 1e-14), "Gamma({x}) = {got}, want {want}");
        }
        // scipy.special.gamma
        for (x, want) in
            [(4.7, 15.431_411_600_047_438), (0.1, 9.513_507_698_668_732), (-2.3, -1.447_107_394_255_918_3)]
        {
            let got = number(&run("Gamma", &[Value::num(x)]));
            assert!(close(got, want, 1e-13), "Gamma({x}) = {got}, want {want}");
        }
        // near the top of the range (scipy: 5.562092414559999e305); Γ(172) overflows
        let got = number(&run("Gamma", &[Value::num(170.5)]));
        assert!(close(got, 5.562_092_414_559_999e305, 1e-12), "{got}");
        assert!(error("Gamma", &[Value::num(172.0)]).contains("too large"));
        assert!(error("Gamma", &[Value::num(0.0)]).contains("poles"));
        assert!(error("Gamma", &[Value::num(-3.0)]).contains("poles"));
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        assert!(error("Gamma", &[Value::Num(Quantity::new(2.0, metre))]).contains("plain number"));
    }

    #[test]
    fn erf_matches_reference_values() {
        // scipy.special.erf
        for (x, want) in [
            (0.0, 0.0),
            (0.1, 0.112_462_916_018_284_9),
            (0.5, 0.520_499_877_813_046_5),
            (1.0, 0.842_700_792_949_714_9),
            (-1.5, -0.966_105_146_475_310_7),
            (2.0, 0.995_322_265_018_952_7),
            (3.5, 0.999_999_256_901_627_7),
            (5.9, 0.999_999_999_999_999_9),
            (10.0, 1.0),
        ] {
            let got = erf(x);
            assert!((got - want).abs() <= 1e-15, "erf({x}) = {got}, want {want}");
        }
        assert!(error("erf", &[Value::num(1.0), Value::num(2.0)]).contains("1 argument"));
    }

    #[test]
    fn parts_of_real_numbers() {
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        let x = Value::Num(Quantity::new(-2.5, metre));
        assert_eq!(run("Re", std::slice::from_ref(&x)), x);
        assert_eq!(run("Im", std::slice::from_ref(&x)), Value::Num(Quantity::new(0.0, metre)));
        assert_eq!(number(&run("arg", &[x])), std::f64::consts::PI);
        assert_eq!(number(&run("arg", &[Value::num(3.0)])), 0.0);
        let v = Value::Matrix(Matrix::column(vec![Value::num(1.0), Value::num(-1.0)]));
        let im = Value::Matrix(Matrix::column(vec![Value::num(0.0), Value::num(0.0)]));
        assert_eq!(run("Im", &[v]), im);
    }

    #[test]
    fn strings() {
        let s = |t: &str| Value::Str(t.to_string());
        assert_eq!(run("strlen", &[s("Ação")]), Value::num(4.0));
        assert_eq!(run("strlen", &[s("")]), Value::num(0.0));
        assert_eq!(run("str2num", &[s(" 12.5 ")]), Value::num(12.5));
        assert_eq!(run("str2num", &[s("1,5")]), Value::num(1.5));
        assert_eq!(run("str2num", &[s("-3e2")]), Value::num(-300.0));
        assert!(error("str2num", &[s("abc")]).contains("\"abc\" is not a number"));
        assert!(error("strlen", &[Value::num(3.0)]).contains("needs a string"));
        assert_eq!(run("IsString", &[s("a")]), Value::num(1.0));
        assert_eq!(run("IsString", &[Value::num(1.0)]), Value::num(0.0));
    }

    #[test]
    fn units_of_and_error() {
        let newton = Dim::from_ints([1, 1, -2, 0, 0, 0, 0]);
        assert_eq!(
            run("UnitsOf", &[Value::Num(Quantity::new(5000.0, newton))]),
            Value::Num(Quantity::new(1.0, newton))
        );
        assert_eq!(run("UnitsOf", &[Value::num(7.0)]), Value::num(1.0));
        assert_eq!(error("error", &[Value::Str("span too long".into())]), "span too long");
        assert!(
            error("error", &[Value::Matrix(Matrix::column(vec![Value::num(1.0)]))]).contains("a message")
        );
    }

    #[test]
    fn unsupported_names_say_why() {
        assert!(unsupported("random").is_some_and(|m| m.contains("same results")));
        assert!(unsupported("gcd").is_some_and(|m| m.contains("Maxima")));
        assert!(unsupported("last").is_some_and(|m| m.contains("v[length(v)]")));
        assert!(unsupported("wfile").is_some_and(|m| m.contains("files")));
        assert!(unsupported("sin").is_none());
    }

    #[test]
    fn on_a_worksheet() {
        let r = crate::stats::tests::shown_with(
            6,
            &[
                "IsDefined(x)",
                "x := 2",
                "IsDefined(x)",
                "IsDefined(π)",
                "sech(0)",
                "csch(1)",
                "acoth(2)",
                "acoth(0.5)",
                "error(\"span too long\")",
                "random(1)",
                "UnitsOf(5 kN)",
                "strlen(\"abc\") + str2num(\"2.5\")",
                "Gamma(5)",
            ],
        );
        let at = |i: usize| r.get(i).map_or("", String::as_str);
        assert_eq!(at(0), "0");
        assert_eq!(at(2), "1");
        assert_eq!(at(3), "1");
        assert_eq!(at(4), "1");
        // 1/sinh(1), atanh(1/2), worked out with Python's math module
        assert_eq!(at(5), "0.850918");
        assert_eq!(at(6), "0.549306");
        assert!(at(7).contains("no real result"), "{r:?}");
        assert_eq!(at(8), "span too long");
        assert!(at(9).contains("same results"), "{r:?}");
        assert_eq!(at(10), "1 'N");
        assert_eq!(at(11), "5.5");
        assert_eq!(at(12), "24");
    }
}
