//! 3D plots (SMath's 3D graph): a surface z(x, y) over two ranges, a parametric curve
//! x(t), y(t), z(t) over one, a parametric surface x(u, v), y(u, v), z(u, v) over two, and
//! data — a matrix of heights is a surface over its column and row numbers, three vectors a
//! line through points. Like 2D plots, they are sampled where they sit in reading order.

use quire_ast::{Expr, parse};
use quire_units::{Dim, UnitRegistry};

use crate::budget::MAX_ELEMENTS;
use crate::error::{Result, bail};
use crate::interp::{Interp, Scope};
use crate::plot::in_unit;
use crate::value::{Quantity, Value};

/// Steps across each range of a surface (41 × 41 points).
pub const GRID: usize = 40;
/// Steps along a curve (241 points, as a 2D plot).
pub const CURVE: usize = crate::plot::SAMPLES;

// the sampled grids are well inside the engine's element cap
const _: () = assert!((GRID + 1) * (GRID + 1) <= MAX_ELEMENTS && CURVE < MAX_ELEMENTS);

#[derive(Debug, Clone, Default)]
pub struct Series3Input {
    /// A height `z(x, y)` over `xrange` × `yrange`, or a matrix of heights.
    pub surface: Option<String>,
    /// `[x, y, z]`: of `t` over `range` (a curve), of `u` and `v` over `urange` × `vrange`
    /// (a surface), or three vectors of points.
    pub parametric: Option<[String; 3]>,
    /// `a..b` for a surface's x and y.
    pub xrange: Option<String>,
    pub yrange: Option<String>,
    /// `a..b` for a curve's `t`.
    pub range: Option<String>,
    /// `a..b` for a parametric surface's `u` and `v`.
    pub urange: Option<String>,
    pub vrange: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Plot3Input {
    pub series: Vec<Series3Input>,
    /// Axis units for x, y and z (`m`, `kN`); otherwise the units the document works in.
    pub units: [Option<String>; 3],
}

/// What a series came out as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    /// A grid of points, row by row, `cols` to a row; neighbours make the faces.
    #[default]
    Surface,
    /// Points joined in order.
    Curve,
}

#[derive(Debug, Clone, Default)]
pub struct Series3Outcome {
    pub shape: Shape,
    /// Points in a row of a surface (0 for a curve).
    pub cols: usize,
    /// One entry per point, in the axis units; `NaN` where a point could not be computed.
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    pub zs: Vec<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Plot3Outcome {
    /// The x, y and z axis units (`m`; empty for plain numbers).
    pub units: [String; 3],
    pub series: Vec<Series3Outcome>,
}

/// One series' samples in SI, kept until the axis units are chosen.
#[derive(Debug, Clone)]
pub(crate) struct Raw {
    shape: Shape,
    cols: usize,
    xs: Vec<Quantity>,
    ys: Vec<Quantity>,
    zs: Vec<Quantity>,
}

fn part(text: &str, what: &str) -> Result<Expr> {
    if text.trim().is_empty() {
        bail!("{what} is empty");
    }
    Ok(parse(text)?)
}

fn given(o: &Option<String>) -> Option<&str> {
    o.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// `a..b` evaluated, as `steps + 1` evenly spaced values.
fn range(
    interp: &mut Interp,
    scope: &mut Scope,
    src: &str,
    what: &str,
    steps: usize,
) -> Result<Vec<Quantity>> {
    let Expr::Range(a, b) = part(src, what)? else { bail!("write {what} as start..end, like -2..2") };
    let a = interp.eval(scope, &a)?.as_num(what)?;
    let b = interp.eval(scope, &b)?.as_num(what)?;
    if a.dim != b.dim {
        bail!("the two ends of {what} have different units");
    }
    if !(a.v.is_finite() && b.v.is_finite()) || a.v == b.v {
        bail!("{what} needs two different, finite ends");
    }
    Ok((0..=steps).map(|k| Quantity::new(a.v + (b.v - a.v) * k as f64 / steps as f64, a.dim)).collect())
}

/// Evaluate `e` at every point (names bound to values). A point that fails is a gap (NaN);
/// when every point fails, the first error is the series' error.
fn sample_at(
    interp: &mut Interp,
    scope: &mut Scope,
    e: &Expr,
    points: &[Vec<(&str, Quantity)>],
) -> Result<Vec<Quantity>> {
    let mut out = Vec::with_capacity(points.len());
    let mut first = None;
    for binds in points {
        interp.tick()?;
        let prev: Vec<_> = binds.iter().map(|(n, v)| scope.bind(n, Value::Num(*v))).collect();
        let r = interp.eval(scope, e).and_then(|v| v.as_num("a plotted value"));
        for ((n, _), p) in binds.iter().zip(prev).rev() {
            scope.restore(n, p);
        }
        match r {
            Ok(q) => out.push(q),
            Err(err) => {
                first.get_or_insert(err);
                out.push(Quantity::new(f64::NAN, Dim::NONE));
            }
        }
    }
    if out.iter().all(|q| q.v.is_nan())
        && let Some(e) = first
    {
        return Err(e);
    }
    Ok(out)
}

/// The points' common dimension; a gap takes it too.
fn one_dim(mut values: Vec<Quantity>, what: &str) -> Result<Vec<Quantity>> {
    let dim = values.iter().find(|q| !q.v.is_nan()).map_or(Dim::NONE, |q| q.dim);
    for q in &mut values {
        if q.v.is_nan() {
            q.dim = dim;
        } else if q.dim != dim {
            bail!("{what} change units across the plot");
        }
    }
    Ok(values)
}

/// The numbers `e` evaluates to, row by row, with its rows and columns. Each number is a step
/// of the evaluation's budget, and there are at most [`MAX_ELEMENTS`] of them.
fn numbers(
    interp: &mut Interp,
    scope: &mut Scope,
    e: &Expr,
    what: &str,
) -> Result<(Vec<Quantity>, usize, usize)> {
    match interp.eval(scope, e)? {
        Value::Num(q) => Ok((vec![q], 1, 1)),
        Value::Matrix(m) => {
            let n = m.data.len();
            if n > MAX_ELEMENTS {
                bail!("{what}: {n} points are too many to plot (at most {MAX_ELEMENTS})");
            }
            interp.tick_n(n as u64)?;
            let qs = m.data.iter().map(|v| v.as_num(what)).collect::<Result<Vec<_>>>()?;
            Ok((qs, m.rows, m.cols))
        }
        Value::Str(_) => bail!("{what} should be numbers, not text"),
        Value::Sym(_) => bail!("{what} should be numbers, not a symbolic expression"),
    }
}

/// The grid of a surface: every (a, b) with `a` along a row, `b` down the rows.
fn grid<'n>(an: &'n str, a: &[Quantity], bn: &'n str, b: &[Quantity]) -> Vec<Vec<(&'n str, Quantity)>> {
    b.iter().flat_map(|bv| a.iter().map(move |av| vec![(an, *av), (bn, *bv)])).collect()
}

pub(crate) fn sample(s: &Series3Input, interp: &mut Interp, scope: &mut Scope) -> Result<Raw> {
    if let Some([xe, ye, ze]) = &s.parametric {
        let [xe, ye, ze] = [part(xe, "x")?, part(ye, "y")?, part(ze, "z")?];
        let points = match (given(&s.range), given(&s.urange), given(&s.vrange)) {
            (_, Some(ur), Some(vr)) => {
                let us = range(interp, scope, ur, "the u range", GRID)?;
                let vs = range(interp, scope, vr, "the v range", GRID)?;
                Some((Shape::Surface, us.len(), grid("u", &us, "v", &vs)))
            }
            (_, Some(_), None) | (_, None, Some(_)) => {
                bail!("a parametric surface needs both a u range and a v range")
            }
            (Some(tr), ..) => {
                let ts = range(interp, scope, tr, "the t range", CURVE)?;
                Some((Shape::Curve, 0, ts.into_iter().map(|t| vec![("t", t)]).collect()))
            }
            _ => None,
        };
        if let Some((shape, cols, points)) = points {
            let xs = one_dim(sample_at(interp, scope, &xe, &points)?, "the x values")?;
            let ys = one_dim(sample_at(interp, scope, &ye, &points)?, "the y values")?;
            let zs = one_dim(sample_at(interp, scope, &ze, &points)?, "the z values")?;
            return Ok(Raw { shape, cols, xs, ys, zs });
        }
        // data: three vectors of points
        let (xs, ..) = numbers(interp, scope, &xe, "the x values")?;
        let (ys, ..) = numbers(interp, scope, &ye, "the y values")?;
        let (zs, ..) = numbers(interp, scope, &ze, "the z values")?;
        if xs.len() != zs.len() || ys.len() != zs.len() {
            bail!("{} x, {} y and {} z values: they should match", xs.len(), ys.len(), zs.len());
        }
        return Ok(Raw {
            shape: Shape::Curve,
            cols: 0,
            xs: one_dim(xs, "the x values")?,
            ys: one_dim(ys, "the y values")?,
            zs: one_dim(zs, "the z values")?,
        });
    }
    let Some(surface) = given(&s.surface) else {
        bail!("plot a surface z(x, y), or x, y and z of a parameter")
    };
    let z = part(surface, "the plotted expression")?;
    if let (Some(xr), Some(yr)) = (given(&s.xrange), given(&s.yrange)) {
        let (xv, yv) = ("x", "y");
        let xs = range(interp, scope, xr, "the x range", GRID)?;
        let ys = range(interp, scope, yr, "the y range", GRID)?;
        let points = grid(xv, &xs, yv, &ys);
        let zs = one_dim(sample_at(interp, scope, &z, &points)?, "the heights")?;
        let (cols, rows) = (xs.len(), ys.len());
        let xs = (0..rows).flat_map(|_| xs.iter().copied()).collect();
        let ys = ys.iter().flat_map(|y| std::iter::repeat_n(*y, cols)).collect();
        return Ok(Raw { shape: Shape::Surface, cols, xs, ys, zs });
    }
    // SMath: a matrix is a surface over its column (x) and row (y) numbers
    let (zs, rows, cols) = numbers(interp, scope, &z, "the plotted values")?;
    if rows < 2 || cols < 2 {
        bail!("plot a surface with x and y ranges, or a matrix of heights with two or more rows and columns");
    }
    let xs = (0..rows).flat_map(|_| (1..=cols).map(|i| Quantity::scalar(i as f64))).collect();
    let ys = (1..=rows).flat_map(|j| std::iter::repeat_n(Quantity::scalar(j as f64), cols)).collect();
    Ok(Raw { shape: Shape::Surface, cols, xs, ys, zs: one_dim(zs, "the heights")? })
}

/// Each axis's dimension: that of the first point any series computed.
pub(crate) fn dims(raws: &[std::result::Result<Raw, String>]) -> [Option<Dim>; 3] {
    std::array::from_fn(|a| {
        raws.iter().filter_map(|r| r.as_ref().ok()).find_map(|r| {
            let values = match a {
                0 => &r.xs,
                1 => &r.ys,
                _ => &r.zs,
            };
            values.iter().find(|q| !q.v.is_nan()).map(|q| q.dim)
        })
    })
}

/// Put every series in the axis units.
pub(crate) fn finish(
    raws: &[std::result::Result<Raw, String>],
    units: [String; 3],
    registry: &UnitRegistry,
) -> Plot3Outcome {
    let convert = |r: &Raw| -> std::result::Result<Series3Outcome, String> {
        let axis = |values: &[Quantity], unit: &str, name: &str| {
            in_unit(values, unit, registry).map_err(|e| format!("{name} axis: {}", e.message))
        };
        let [ux, uy, uz] = &units;
        Ok(Series3Outcome {
            shape: r.shape,
            cols: r.cols,
            xs: axis(&r.xs, ux, "x")?,
            ys: axis(&r.ys, uy, "y")?,
            zs: axis(&r.zs, uz, "z")?,
            error: None,
        })
    };
    let series = raws
        .iter()
        .map(|r| {
            r.as_ref()
                .map_err(Clone::clone)
                .and_then(convert)
                .unwrap_or_else(|e| Series3Outcome { error: Some(e), ..Default::default() })
        })
        .collect();
    Plot3Outcome { units, series }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{DisplaySettings, DisplaySpec};
    use crate::plot::PlotInput;
    use crate::worksheet::{MathInput, Outcome, Worksheet};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn math(id: &str, expr: &str) -> MathInput {
        MathInput {
            id: id.into(),
            expr: expr.into(),
            show: false,
            display: DisplaySpec::default(),
            table: None,
            plot: None,
            symbolic: false,
        }
    }

    fn plot3(id: &str, series: Vec<Series3Input>, units: [Option<String>; 3]) -> MathInput {
        let three = Plot3Input { series, units };
        MathInput { plot: Some(PlotInput { three: Some(three), ..Default::default() }), ..math(id, "") }
    }

    fn run(regions: &[MathInput]) -> Vec<Outcome> {
        Worksheet::new(DisplaySettings::default()).run(regions)
    }

    fn outcome(o: &Outcome) -> std::result::Result<&Plot3Outcome, Box<dyn std::error::Error>> {
        Ok(o.plot.as_ref().and_then(|p| p.three.as_ref()).ok_or("no 3D plot")?)
    }

    fn surface(z: &str) -> Series3Input {
        Series3Input { surface: Some(z.into()), ..Default::default() }
    }

    fn close(a: Option<&f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn surfaces_curves_and_matrices() -> TestResult {
        let surf =
            Series3Input { xrange: Some("-1..1".into()), yrange: Some("0..2".into()), ..surface("x^2 + y") };
        let curve = Series3Input {
            parametric: Some(["cos(t)".into(), "sin(t)".into(), "t/(2*π)".into()]),
            range: Some("0..2*π".into()),
            ..Default::default()
        };
        let sphere = Series3Input {
            parametric: Some(["cos(u)*cos(v)".into(), "sin(u)*cos(v)".into(), "sin(v)".into()]),
            urange: Some("0..2*π".into()),
            vrange: Some("-π/2..π/2".into()),
            ..Default::default()
        };
        let out = run(&[
            math("m", "M := [1, 2, 3; 4, 5, 6]"),
            plot3("p", vec![surf, curve, surface("M"), sphere], Default::default()),
        ]);
        assert_eq!(out[1].error, None);
        let p = outcome(&out[1])?;
        let [a, b, c, d] = &p.series[..] else { return Err("four series".into()) };
        let n = (GRID + 1) * (GRID + 1);
        assert_eq!(
            (a.shape, a.cols, a.xs.len(), a.ys.len(), a.zs.len()),
            (Shape::Surface, GRID + 1, n, n, n)
        );
        assert!(close(a.zs.first(), 1.0), "z(-1, 0) = 1");
        assert!(close(a.zs.last(), 3.0), "z(1, 2) = 3");
        assert!(close(a.ys.get(GRID + 1), 2.0 / GRID as f64), "the second row is the next y");
        assert_eq!((b.shape, b.zs.len()), (Shape::Curve, CURVE + 1));
        assert!(close(b.zs.last(), 1.0));
        assert_eq!((c.shape, c.cols), (Shape::Surface, 3));
        assert_eq!(c.xs, vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0]);
        assert_eq!(c.ys, vec![1.0, 1.0, 1.0, 2.0, 2.0, 2.0]);
        assert_eq!(c.zs, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!((d.shape, d.cols, d.zs.len()), (Shape::Surface, GRID + 1, n));
        assert!(close(d.zs.first(), -1.0), "the sphere's south pole");
        Ok(())
    }

    #[test]
    fn units_gaps_and_errors() -> TestResult {
        let surf = Series3Input {
            xrange: Some("0 m..2 m".into()),
            yrange: Some("0 m..1 m".into()),
            ..surface("x*y")
        };
        let out = run(&[plot3("p", vec![surf], [Some("mm".into()), None, None])]);
        let p = outcome(&out[0])?;
        assert_eq!(p.units, ["mm".to_string(), "m".to_string(), "m^2".to_string()]);
        assert!(close(p.series[0].xs.last(), 2000.0));
        let gaps =
            Series3Input { xrange: Some("-1..1".into()), yrange: Some("-1..1".into()), ..surface("sqrt(x)") };
        let curve =
            Series3Input { parametric: Some(["t".into(), "".into(), "t".into()]), ..Default::default() };
        let out = run(&[plot3("p", vec![gaps, surface("[1, 2]"), curve], Default::default())]);
        let p = outcome(&out[0])?;
        assert!(
            p.series[0].error.is_none() && p.series[0].zs.first().is_some_and(|z| z.is_nan()),
            "a gap where sqrt fails"
        );
        assert!(p.series[1].error.as_deref().is_some_and(|e| e.contains("matrix of heights")));
        assert!(p.series[2].error.as_deref().is_some_and(|e| e.contains("y is empty")));
        let half = Series3Input {
            parametric: Some(["u".into(), "v".into(), "u*v".into()]),
            urange: Some("0..1".into()),
            ..Default::default()
        };
        let half_out = run(&[plot3("h", vec![half], Default::default())]);
        let half_plot = outcome(&half_out[0])?;
        assert_eq!(
            half_plot.series[0].error.as_deref(),
            Some("a parametric surface needs both a u range and a v range")
        );
        assert_eq!(
            out[0].error.as_deref(),
            Some(
                "series 2: plot a surface with x and y ranges, or a matrix of heights with two or more rows and columns"
            )
        );
        Ok(())
    }

    #[test]
    fn sampling_stops_at_the_step_budget() -> TestResult {
        // every point loops 20 000 times: 41 × 41 points of it is past the budget
        let slow = Series3Input {
            xrange: Some("0..1".into()),
            yrange: Some("0..1".into()),
            ..surface("for(i, 1..20000, i) + x")
        };
        let out = run(&[plot3("p", vec![slow], Default::default())]);
        let p = outcome(&out[0])?;
        let e = p.series[0].error.as_deref().unwrap_or_default();
        assert!(e.contains("stopped after"), "{e}");
        Ok(())
    }

    #[test]
    fn axes_take_the_units_the_document_works_in() -> TestResult {
        let surf =
            Series3Input { xrange: Some("0 m..L".into()), yrange: Some("0..1".into()), ..surface("q*x*y") };
        let regions =
            [math("q", "q := 10 kN/m"), math("l", "L := 6 m"), plot3("p", vec![surf], Default::default())];
        let out = run(&regions);
        assert_eq!(outcome(&out[2])?.units, ["m".to_string(), String::new(), "N".to_string()]);
        let settings = DisplaySettings { working_units: true, ..Default::default() };
        let out = Worksheet::new(settings).run(&regions);
        let p = outcome(&out[2])?;
        assert_eq!(p.units, ["m".to_string(), String::new(), "kN".to_string()]);
        assert!(close(p.series[0].zs.last(), 60.0));
        assert_eq!(out[2].error, None);
        Ok(())
    }
}
