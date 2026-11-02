//! Result display: numbers rounded per the document/region settings, units either
//! forced by the region (SMath's "contract") or chosen from coherent SI.
//! Output is linear syntax, so it can be parsed back (`results.json`).

use quire_ast::{Expr, parse, print, quote_str};
use quire_units::{BASE_UNITS, Dim, Rational, Unit, UnitRegistry, format_product};

use crate::error::{Result, bail};
use crate::interp::{Env, Interp, Scope};
use crate::value::{Quantity, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySettings {
    pub decimals: u32,
    /// Magnitudes ≥ 10^threshold are shown in scientific notation.
    pub exponential_threshold: i32,
    pub trailing_zeros: bool,
    /// Show results in the units the document is written in (`54 kN*m`) rather than
    /// coherent SI (`54000 J`). `"resultUnits": "working"` in a document.
    pub working_units: bool,
    /// How large and small numbers are written.
    pub notation: Notation,
    /// Round to this many significant figures instead of a number of decimals.
    pub sig_figs: Option<u32>,
    /// SMath documents: units are always written quoted (`'m`), so a bare name is never a
    /// unit or a constant, only a variable. `"units": {"marked": true}` in a document.
    pub marked_units: bool,
}

/// `"notation"` in a document's display settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Notation {
    /// Plain decimals, scientific beyond the threshold: `12500`, `1.25*10^7`.
    #[default]
    Auto,
    /// Powers of ten in steps of three: `12.5*10^3`.
    Engineering,
    /// The unit's SI prefix keeps the number between 1 and 1000: `12.5 kN`, `4 mm`.
    Prefix,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        DisplaySettings {
            decimals: 4,
            exponential_threshold: 5,
            trailing_zeros: false,
            working_units: false,
            notation: Notation::Auto,
            sig_figs: None,
            marked_units: false,
        }
    }
}

/// A unit the document's author typed, in order of preference (most used first).
#[derive(Debug, Clone)]
pub struct WorkingUnit {
    pub name: String,
    pub unit: Unit,
}

/// The simplest way to write `dim` with the author's units: one unit (`MPa`), one unit
/// to a power (`m^2`, `cm^4`) or two units (`kN*m`, `kN/m`). Returns (factor, linear text).
pub fn working_unit(dim: Dim, vocab: &[WorkingUnit]) -> Option<(f64, String)> {
    const POWERS: [i32; 8] = [1, 2, 3, 4, -1, -2, -3, -4];
    // a power whose exponents don't fit is never the dimension sought
    let pow = |u: &WorkingUnit, k: i32| u.unit.dim.pow(Rational::int(k));
    // ((complexity, preference rank), [(unit index, power)]): lower scores are better
    type Candidate = ((i32, usize), Vec<(usize, i32)>);
    let mut found: Vec<Candidate> = Vec::new();
    for (i, a) in vocab.iter().enumerate() {
        for ka in POWERS {
            if pow(a, ka) == Some(dim) {
                found.push(((ka.abs(), i), vec![(i, ka)]));
            }
        }
        for (j, b) in vocab.iter().enumerate().skip(i + 1) {
            if a.unit.dim == b.unit.dim {
                continue; // `kN/N` is not a way to write anything
            }
            for ka in POWERS {
                for kb in POWERS {
                    if pow(a, ka).zip(pow(b, kb)).and_then(|(x, y)| x.checked_mul(y)) == Some(dim) {
                        found.push(((ka.abs() + kb.abs(), i + j), vec![(i, ka), (j, kb)]));
                    }
                }
            }
        }
    }
    let (_, mut best) = found.into_iter().min_by_key(|(score, _)| *score)?;
    // `kN*m`, `kN/m`, `MPa*m`: lengths go after forces, stresses and the rest
    let is_length =
        |i: usize| vocab.get(i).is_some_and(|u| u.unit.dim == Dim::from_ints([1, 0, 0, 0, 0, 0, 0]));
    best.sort_by_key(|&(i, _)| is_length(i));
    let factor: f64 =
        best.iter().map(|&(i, k)| Some(vocab.get(i)?.unit.factor.powi(k))).product::<Option<f64>>()?;
    let parts: Vec<(&str, Rational)> = best
        .iter()
        .map(|&(i, k)| Some((vocab.get(i)?.name.as_str(), Rational::int(k))))
        .collect::<Option<_>>()?;
    Some((factor, format_product(&parts, true)))
}

/// Per-region overrides (`"display": { "unit": "kgf", "decimals": 2 }`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplaySpec {
    pub unit: Option<String>,
    pub decimals: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnitSpec {
    pub factor: f64,
    pub dim: Dim,
    pub offset: f64,
    /// Linear syntax with unit markers, e.g. `'kgf/'m^2`; empty for plain numbers.
    pub text: String,
}

fn as_units(e: Expr) -> Expr {
    let b = |x: Box<Expr>| Box::new(as_units(*x));
    match e {
        Expr::Ident(n) => Expr::Unit(n),
        Expr::Paren(x) => Expr::Paren(b(x)),
        Expr::Unary(op, x) => Expr::Unary(op, b(x)),
        Expr::Binary(op, l, r) if op == quire_ast::BinaryOp::Pow => Expr::Binary(op, b(l), r),
        Expr::Binary(op, l, r) => Expr::Binary(op, b(l), b(r)),
        other => other,
    }
}

/// Parse a display unit written without markers: `kgf`, `kgf/m^2`, `cm^2`, `°C`.
pub fn parse_unit_spec(spec: &str, units: &UnitRegistry) -> Result<UnitSpec> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "1" {
        return Ok(UnitSpec { factor: 1.0, dim: Dim::NONE, offset: 0.0, text: String::new() });
    }
    let expr = match units.lookup(spec) {
        Some(_) => Expr::Unit(spec.to_string()),
        None => as_units(parse(spec)?),
    };
    let env = Env::new();
    let q = Interp::new(units).eval(&mut Scope::new(&env), &expr)?.as_num("a display unit")?;
    let offset = match &expr {
        Expr::Unit(n) => units.lookup(n).map_or(0.0, |u| u.offset),
        _ => 0.0,
    };
    Ok(UnitSpec { factor: q.v, dim: q.dim, offset, text: print(&expr) })
}

fn trim_zeros(s: String) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

pub fn format_number(x: f64, decimals: u32, settings: &DisplaySettings) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "∞" } else { "-∞" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    // significant figures: as many decimals as the magnitude leaves room for
    let decimals_for = |v: f64| match settings.sig_figs {
        Some(n) => (n.max(1) as i32 - 1 - v.abs().log10().floor() as i32).clamp(0, 15) as u32,
        None => decimals,
    };
    if settings.notation != Notation::Auto {
        // powers of ten in steps of three; 1 ≤ |mantissa| < 1000
        let mut e = (x.abs().log10() / 3.0).floor() as i32 * 3;
        let mut m = x / 10f64.powi(e);
        let d = decimals_for(m);
        let scale = 10f64.powi(d as i32);
        if ((m.abs() * scale).round() / scale) >= 1000.0 {
            e += 3;
            m = x / 10f64.powi(e);
        }
        let plain =
            DisplaySettings { notation: Notation::Auto, exponential_threshold: 16, ..settings.clone() };
        let text = format_number(m, decimals_for(m), &DisplaySettings { sig_figs: None, ..plain });
        return if e == 0 { text } else { format!("{text}*10^{e}") };
    }
    let d = decimals_for(x).min(15) as usize;
    let fixed = |v: f64| {
        let s = format!("{v:.d$}");
        if settings.trailing_zeros { s } else { trim_zeros(s) }
    };
    let ax = x.abs();
    let scale = 10f64.powi(d as i32);
    let rounded = (ax * scale).round() / scale;
    if rounded >= 10f64.powi(settings.exponential_threshold)
        || rounded == 0.0
        || ax < 10f64.powi(-settings.exponential_threshold)
    {
        let mut e = ax.log10().floor() as i32;
        let mut m = fixed(x / 10f64.powi(e));
        if m.trim_start_matches('-').starts_with("10") {
            e += 1;
            m = fixed(x / 10f64.powi(e));
        }
        return format!("{m}*10^{e}");
    }
    let s = fixed(x);
    if s == "-0" { "0".into() } else { s }
}

fn join(num: String, unit: &str) -> String {
    if unit.is_empty() {
        num
    } else if let Some(rest) = unit.strip_prefix("1/") {
        format!("{num}/{rest}")
    } else if num.contains("*10^") {
        format!("{num}*{unit}") // `1.2*10^5 'm` would parse as 10^(5 m)
    } else {
        format!("{num} {unit}")
    }
}

/// Coherent SI spelling for a dimension: a named unit (N, Pa, J…), a named unit
/// times a power of metres (N*m, N/m), or base units.
pub(crate) fn si_unit(dim: Dim) -> String {
    let named = UnitRegistry::named_si();
    if let Some((n, _)) = named.iter().find(|(_, d)| *d == dim) {
        return format!("'{n}");
    }
    let base = dim.parts();
    // mechanics (mass and time involved): prefer N/m, N*m, Pa*m… over kg/s^2
    let mechanical = !dim.0[1].is_zero() && !dim.0[2].is_zero();
    if mechanical {
        let metre = Dim::from_ints([1, 0, 0, 0, 0, 0, 0]);
        for (n, d) in named {
            for k in [1, -1, 2, -2, 3, -3] {
                if metre.pow(Rational::int(k)).and_then(|m| d.checked_mul(m)) == Some(dim) {
                    return format_product(&[(n, Rational::ONE), (BASE_UNITS[0], Rational::int(k))], true);
                }
            }
        }
    }
    format_product(&base, true)
}

/// A dimension in words for messages: "a length (m)", "a stress (Pa)", "a quantity in kg*m".
pub fn describe_dim(dim: Dim) -> String {
    if dim.is_dimensionless() {
        return "a plain number".to_string();
    }
    const NAMES: &[([i8; 7], &str)] = &[
        ([1, 0, 0, 0, 0, 0, 0], "a length"),
        ([2, 0, 0, 0, 0, 0, 0], "an area"),
        ([3, 0, 0, 0, 0, 0, 0], "a volume"),
        ([4, 0, 0, 0, 0, 0, 0], "a second moment of area"),
        ([0, 1, 0, 0, 0, 0, 0], "a mass"),
        ([0, 0, 1, 0, 0, 0, 0], "a time"),
        ([0, 0, -1, 0, 0, 0, 0], "a frequency"),
        ([1, 0, -1, 0, 0, 0, 0], "a speed"),
        ([1, 0, -2, 0, 0, 0, 0], "an acceleration"),
        ([1, 1, -2, 0, 0, 0, 0], "a force"),
        ([0, 1, -2, 0, 0, 0, 0], "a force per length"),
        ([-1, 1, -2, 0, 0, 0, 0], "a pressure or stress"),
        ([-2, 1, -2, 0, 0, 0, 0], "a unit weight"),
        ([2, 1, -2, 0, 0, 0, 0], "an energy or moment"),
        ([2, 1, -3, 0, 0, 0, 0], "a power"),
        ([-3, 1, 0, 0, 0, 0, 0], "a density"),
        ([0, 0, 0, 1, 0, 0, 0], "an electric current"),
        ([0, 0, 0, 0, 1, 0, 0], "a temperature"),
    ];
    let unit = si_unit(dim).replace('\'', "");
    match NAMES.iter().find(|(d, _)| Dim::from_ints(*d) == dim) {
        Some((_, name)) => format!("{name} ({unit})"),
        None => format!("a quantity in {unit}"),
    }
}

pub fn format_value(
    v: &Value,
    spec: &DisplaySpec,
    settings: &DisplaySettings,
    units: &UnitRegistry,
) -> Result<String> {
    format_value_in(v, spec, settings, units, &[])
}

/// [`format_value`], preferring the author's units when `settings.working_units` is on.
pub fn format_value_in(
    v: &Value,
    spec: &DisplaySpec,
    settings: &DisplaySettings,
    units: &UnitRegistry,
    vocab: &[WorkingUnit],
) -> Result<String> {
    match v {
        Value::Num(q) => format_quantity(*q, spec, settings, units, vocab),
        Value::Str(s) => Ok(quote_str(s)),
        Value::Sym(e) => Ok(quire_ast::print(e)),
        Value::Matrix(m) => {
            let mut rows = Vec::with_capacity(m.rows);
            for r in 0..m.rows {
                let cells = (0..m.cols)
                    .map(|c| format_value_in(m.at(r, c)?, spec, settings, units, vocab))
                    .collect::<Result<Vec<_>>>()?;
                rows.push(cells.join(", "));
            }
            Ok(format!("[{}]", rows.join("; ")))
        }
    }
}

/// A result shown in the unit its region asks for. A unit of another kind keeps the rest
/// in SI, as SMath does: a force shown in kg reads `9.81 kg*m/s^2`.
pub fn format_shown(
    v: &Value,
    spec: &DisplaySpec,
    settings: &DisplaySettings,
    units: &UnitRegistry,
) -> Result<String> {
    if let (Value::Num(q), Some(u)) = (v, spec.unit.as_deref()) {
        let us = parse_unit_spec(u, units)?;
        if us.dim != q.dim && us.offset == 0.0 && !us.text.is_empty() {
            let d = spec.decimals.unwrap_or(settings.decimals);
            let rest = si_unit(crate::builtins::dim_div(q.dim, us.dim)?);
            let text = match rest.strip_prefix("1/") {
                Some(denominator) => format!("{}/{denominator}", us.text),
                None if rest.is_empty() => us.text.clone(),
                None => format!("{}*{rest}", us.text),
            };
            return Ok(join(format_number(q.v / us.factor, d, settings), &text));
        }
    }
    format_value(v, spec, settings, units)
}

fn format_quantity(
    q: Quantity,
    spec: &DisplaySpec,
    settings: &DisplaySettings,
    units: &UnitRegistry,
    vocab: &[WorkingUnit],
) -> Result<String> {
    let d = spec.decimals.unwrap_or(settings.decimals);
    if let Some(u) = spec.unit.as_deref() {
        let us = parse_unit_spec(u, units)?;
        if us.dim != q.dim {
            bail!("the display unit {u} doesn't fit this result, which is {}", describe_dim(q.dim));
        }
        // the unit asked for is kept, and with prefixes on, so is a plain number
        let plain = DisplaySettings { notation: Notation::Auto, ..settings.clone() };
        let s = if settings.notation == Notation::Prefix { &plain } else { settings };
        return Ok(join(format_number((q.v - us.offset) / us.factor, d, s), &us.text));
    }
    if q.dim.is_dimensionless() {
        return Ok(format_number(q.v, d, settings));
    }
    let (value, text) = match settings.working_units.then(|| working_unit(q.dim, vocab)).flatten() {
        Some((factor, text)) => (q.v / factor, text),
        None => (q.v, si_unit(q.dim)),
    };
    // with a prefix chosen the number stays plain (2500 kg, not 2.5*10^3 kg); compound
    // units fall back to powers of ten in steps of three
    if settings.notation == Notation::Prefix
        && let Some((value, text)) = reprefix(value, &text, units)
    {
        let plain = DisplaySettings { notation: Notation::Auto, ..settings.clone() };
        return Ok(join(format_number(value, d, &plain), &text));
    }
    Ok(join(format_number(value, d, settings), &text))
}

/// SI prefixes a unit may take when results are written with prefixes, by base unit.
fn prefixes_for(base: &str) -> &'static [(&'static str, i32)] {
    const ALL: &[(&str, i32)] =
        &[("p", -12), ("n", -9), ("µ", -6), ("m", -3), ("", 0), ("k", 3), ("M", 6), ("G", 9), ("T", 12)];
    match base {
        // lengths: centimetres too, but nothing above kilometres
        "m" => &[("n", -9), ("µ", -6), ("m", -3), ("c", -2), ("", 0), ("k", 3)],
        // masses: grams up to kilograms (tonnes are a unit of their own)
        "g" => &[("µ", -6), ("m", -3), ("", 0), ("k", 3)],
        // times: only below a second
        "s" => &[("p", -12), ("n", -9), ("µ", -6), ("m", -3), ("", 0)],
        _ => ALL,
    }
}

const PREFIXABLE: &[&str] = &["m", "g", "s", "N", "Pa", "J", "W", "V", "A", "Hz", "C", "F", "Ω"];

/// `12500 'N` → `12.5 'kN`, `0.004 'm` → `4 'mm`: the prefix that keeps the number
/// between 1 and 1000 (centimetres between 1 and 100). Only a single unit is re-prefixed.
fn reprefix(value: f64, text: &str, units: &UnitRegistry) -> Option<(f64, String)> {
    if value == 0.0 || !value.is_finite() {
        return None;
    }
    let name = text.strip_prefix('\'')?;
    if name.contains(['*', '/', '^', '\'']) {
        return None;
    }
    // split off the prefix the unit already has: kN → (k, N)
    let (base, exp) = PREFIXABLE.iter().find_map(|b| {
        let p = name.strip_suffix(b)?;
        prefixes_for(b).iter().find(|(q, _)| *q == p).map(|(_, e)| (*b, *e))
    })?;
    let in_base = value * 10f64.powi(exp);
    let fits = |&&(p, e): &&(&str, i32)| {
        let v = (in_base / 10f64.powi(e)).abs();
        if p == "c" { (1.0..100.0).contains(&v) } else { (1.0..1000.0).contains(&v) }
    };
    let list = prefixes_for(base);
    // the largest prefix that still leaves the number at least 1
    let chosen = list
        .iter()
        .rev()
        .find(fits)
        .or_else(|| if in_base.abs() < 1.0 { list.first() } else { list.last() })?;
    let unit = format!("{}{base}", chosen.0);
    units.lookup(&unit)?;
    Some((in_base / 10f64.powi(chosen.1), format!("'{unit}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn numbers() {
        let s = DisplaySettings::default();
        assert_eq!(format_number(25.5, 2, &s), "25.5");
        assert_eq!(format_number(2.0, 4, &s), "2");
        assert_eq!(format_number(2.5612496949731396, 2, &s), "2.56");
        assert_eq!(format_number(856.9, 0, &s), "857");
        assert_eq!(format_number(-0.0001, 2, &s), "-1*10^-4");
        assert_eq!(format_number(123456.0, 2, &s), "1.23*10^5");
        assert_eq!(format_number(99999.996, 2, &s), "1*10^5");
        let tz = DisplaySettings { trailing_zeros: true, ..s.clone() };
        assert_eq!(format_number(2.0, 2, &tz), "2.00");
        let sig = DisplaySettings { sig_figs: Some(3), ..s.clone() };
        assert_eq!(format_number(12345.0, 2, &sig), "12345");
        assert_eq!(format_number(0.0123456, 2, &sig), "0.0123");
        assert_eq!(format_number(146.875, 2, &sig), "147");
        let eng = DisplaySettings { notation: Notation::Engineering, ..s.clone() };
        assert_eq!(format_number(12500.0, 2, &eng), "12.5*10^3");
        assert_eq!(format_number(0.00042, 2, &eng), "420*10^-6");
        assert_eq!(format_number(47.587, 2, &eng), "47.59");
        assert_eq!(format_number(999999.0, 2, &eng), "1*10^6");
    }

    #[test]
    fn si_prefixes() -> TestResult {
        let r = UnitRegistry::new();
        let s = DisplaySettings { notation: Notation::Prefix, ..DisplaySettings::default() };
        let q = |v: f64, d: [i8; 7]| Value::Num(Quantity::new(v, Dim::from_ints(d)));
        let shown = |v: Value| format_value(&v, &DisplaySpec::default(), &s, &r);
        assert_eq!(shown(q(12500.0, [1, 1, -2, 0, 0, 0, 0]))?, "12.5 'kN");
        assert_eq!(shown(q(0.004, [1, 0, 0, 0, 0, 0, 0]))?, "4 'mm");
        assert_eq!(shown(q(0.3, [1, 0, 0, 0, 0, 0, 0]))?, "30 'cm");
        assert_eq!(shown(q(1.469e8, [-1, 1, -2, 0, 0, 0, 0]))?, "146.9 'MPa");
        assert_eq!(shown(q(2500.0, [0, 1, 0, 0, 0, 0, 0]))?, "2500 'kg");
        // compound units and plain numbers: engineering notation instead
        assert_eq!(shown(q(5000.0, [0, 1, -2, 0, 0, 0, 0]))?, "5*10^3*'N/'m");
        assert_eq!(shown(q(12500.0, [0; 7]))?, "12.5*10^3");
        // a unit the author asked for stays as asked
        let kn = DisplaySpec { unit: Some("kN".into()), decimals: None };
        assert_eq!(format_value(&q(500.0, [1, 1, -2, 0, 0, 0, 0]), &kn, &s, &r)?, "0.5 'kN");
        Ok(())
    }

    #[test]
    fn units() -> TestResult {
        let r = UnitRegistry::new();
        let spec = |u: &str| DisplaySpec { unit: Some(u.into()), decimals: Some(2) };
        let force = Value::Num(Quantity::new(9.80665 * 1800.0, Dim::from_ints([1, 1, -2, 0, 0, 0, 0])));
        let s = DisplaySettings::default();
        assert_eq!(format_value(&force, &spec("kgf"), &s, &r)?, "1800 'kgf");
        assert_eq!(format_value(&force, &DisplaySpec::default(), &s, &r)?, "17651.97 'N");
        let moment = Value::Num(Quantity::new(10.0, Dim::from_ints([2, 1, -2, 0, 0, 0, 0])));
        assert_eq!(format_value(&moment, &spec("kgf*m"), &s, &r)?, "1.02 'kgf*'m");
        let line = Value::Num(Quantity::new(5.0, Dim::from_ints([0, 1, -2, 0, 0, 0, 0])));
        assert_eq!(format_value(&line, &DisplaySpec::default(), &s, &r)?, "5 'N/'m");
        let hz = Value::Num(Quantity::new(5.0, Dim::from_ints([0, 0, -1, 0, 0, 0, 0])));
        assert_eq!(format_value(&hz, &DisplaySpec::default(), &s, &r)?, "5/'s");
        let temp = Value::Num(Quantity::new(293.15, Dim::from_ints([0, 0, 0, 0, 1, 0, 0])));
        assert_eq!(format_value(&temp, &spec("°C"), &s, &r)?, "20 '°C");
        assert!(format_value(&temp, &spec("m"), &s, &r).is_err());
        // shown in a unit of another kind, the rest stays in SI (SMath)
        assert_eq!(format_shown(&force, &spec("kg"), &s, &r)?, "17651.97 'kg*'m/'s^2");
        assert_eq!(format_shown(&moment, &spec("kN"), &s, &r)?, "0.01 'kN*'m");
        assert_eq!(format_shown(&line, &spec("kN"), &s, &r)?, "0.01 'kN/'m");
        assert!(
            format_shown(&force, &spec("°C"), &s, &r).is_err(),
            "a scale with an offset can't carry other units"
        );
        Ok(())
    }
}
