//! Numeric tools that call your functions or re-evaluate your equations: ODE solvers
//! (`rkfixed`, `Rkadapt`), least squares on basis functions (`linfit`) and systems of
//! equations (`solve` with a vector of unknowns).

use std::rc::Rc;

use quire_ast::Expr;
use quire_units::Dim;

use crate::error::{Result, bail, internal};
use crate::format::describe_dim;
use crate::interp::{Binding, Func, Interp, Scope, arg, func_key};
use crate::numeric;
use crate::ops::same_dim;
use crate::value::{Matrix, Quantity, Value};

/// An initial value problem as the solvers see it: SI magnitudes plus the units of t and y.
struct Ode {
    f: Rc<Func>,
    tdim: Dim,
    dims: Vec<Dim>,
    /// y0 was a number, so D(t, y) gets a number rather than a 1-element vector.
    scalar: bool,
}

const MAX_ODE_POINTS: usize = 1_000_000;

/// A plain 0 fits any units (`[y[2]; 0]` as a rate), as it does for integration limits.
fn fits(q: Quantity, dim: Dim) -> bool {
    q.dim == dim || (q.v == 0.0 && q.dim.is_dimensionless())
}

/// The numbers of a vector value (or of one number), each with its own units.
fn quantities(v: &Value, what: &str) -> Result<Vec<Quantity>> {
    match v {
        Value::Num(q) => Ok(vec![*q]),
        Value::Matrix(m) if m.is_vector() => m.data.iter().map(|x| x.as_num(what)).collect(),
        Value::Matrix(m) => bail!("{what} must be a vector, not a {}×{} matrix", m.rows, m.cols),
        Value::Str(_) => bail!("{what} must be a number or a vector, not a string"),
        Value::Sym(_) => bail!("{what} must be a number or a vector, not a formula"),
    }
}

fn column_of(values: &[f64], dims: &[Dim]) -> Value {
    Value::Matrix(Matrix::column(
        values.iter().zip(dims).map(|(v, d)| Value::Num(Quantity::new(*v, *d))).collect(),
    ))
}

impl Interp<'_> {
    /// The function an argument names: `D`, or `D(t, y)` written out as SMath does.
    fn function_arg(&self, s: &Scope, e: &Expr, params: &[&str], who: &str) -> Result<Rc<Func>> {
        let sig = params.join(", ");
        let name = match e {
            Expr::Ident(n) => n,
            Expr::Call(n, xs)
                if xs.len() == params.len() && xs.iter().all(|x| matches!(x, Expr::Ident(_))) =>
            {
                n
            }
            _ => bail!(
                "{who} needs the name of a function of ({sig}) here: define F({sig}) := … above and pass F"
            ),
        };
        match s.get(&func_key(name, params.len())) {
            Some(Binding::Func(f)) => Ok(Rc::clone(f)),
            Some(Binding::Failed(_)) => bail!("{name}(…) has no value (its definition has an error)"),
            _ => bail!(
                "{name}({sig}) is not defined; {who} calls it with ({sig}), so define {name}({sig}) := … above"
            ),
        }
    }

    /// `rkfixed(y0, t0, t1, n, D)` / `Rkadapt(…)`: y0, the interval and the step count.
    fn ode_problem(
        &mut self,
        s: &mut Scope,
        args: &[Expr],
        who: &str,
    ) -> Result<(Ode, Vec<f64>, f64, f64, usize)> {
        let y0 = self.eval(s, arg(args, 0)?)?;
        let scalar = matches!(y0, Value::Num(_));
        let y0 = quantities(&y0, "the initial value y0")?;
        if y0.is_empty() {
            bail!("{who} needs at least one initial value");
        }
        let mut t0 = self.eval(s, arg(args, 1)?)?.as_num("the start t0")?;
        let mut t1 = self.eval(s, arg(args, 2)?)?.as_num("the end t1")?;
        // a plain 0 start takes the end's units: `rkfixed(y0, 0, 10 s, …)`
        if t0.v == 0.0 && t0.dim.is_dimensionless() {
            t0.dim = t1.dim;
        } else if t1.v == 0.0 && t1.dim.is_dimensionless() {
            t1.dim = t0.dim;
        }
        same_dim(t0.dim, t1.dim, "integrate between")?;
        if t0.v == t1.v || !t0.v.is_finite() || !t1.v.is_finite() {
            bail!("{who} needs a start t0 and a different, finite end t1");
        }
        let n = self.eval(s, arg(args, 3)?)?.as_int("the number of steps")?;
        if n < 1 || n as usize * (y0.len() + 1) > MAX_ODE_POINTS {
            bail!(
                "{who} needs from 1 to {} steps for this many equations, not {n}",
                MAX_ODE_POINTS / (y0.len() + 1)
            );
        }
        let f = self.function_arg(s, arg(args, 4)?, &["t", "y"], who)?;
        let ode = Ode { f, tdim: t0.dim, dims: y0.iter().map(|q| q.dim).collect(), scalar };
        Ok((ode, y0.iter().map(|q| q.v).collect(), t0.v, t1.v, n as usize))
    }

    /// D(t, y), checked to be the rate of change of each y.
    fn rate(&mut self, ode: &Ode, t: f64, y: &[f64]) -> Result<Vec<f64>> {
        self.tick()?;
        let yv = match (ode.scalar, y.first(), ode.dims.first()) {
            (true, Some(&yi), Some(&d)) => Value::Num(Quantity::new(yi, d)),
            _ => column_of(y, &ode.dims),
        };
        let r = self.apply(&ode.f, vec![Value::Num(Quantity::new(t, ode.tdim)), yv])?;
        let name = &ode.f.name;
        let r = quantities(&r, &format!("the result of {name}(t, y)"))?;
        if r.len() != y.len() {
            bail!(
                "{name}(t, y) returned {} values, but y has {}: it needs one rate for each",
                r.len(),
                y.len()
            );
        }
        for (j, (q, d)) in r.iter().zip(&ode.dims).enumerate() {
            let want = crate::builtins::dim_div(*d, ode.tdim)?;
            if !fits(*q, want) {
                let which = if y.len() == 1 { String::new() } else { format!(" (element {})", j + 1) };
                bail!(
                    "{name}(t, y) must return the rate of change of y{which}: {}, but it is {}",
                    describe_dim(want),
                    describe_dim(q.dim)
                );
            }
            if !q.v.is_finite() {
                bail!("{name}(t, y) is not finite at t = {t} (the solution may blow up there)");
            }
        }
        Ok(r.iter().map(|q| q.v).collect())
    }

    /// The (n+1)×(k+1) table [t, y1, …, yk] that both ODE solvers return.
    fn ode_table(ode: &Ode, rows: Vec<(f64, Vec<f64>)>) -> Value {
        let cols = ode.dims.len() + 1;
        let mut data = Vec::with_capacity(rows.len() * cols);
        for (t, y) in &rows {
            data.push(Value::Num(Quantity::new(*t, ode.tdim)));
            data.extend(y.iter().zip(&ode.dims).map(|(v, d)| Value::Num(Quantity::new(*v, *d))));
        }
        Value::Matrix(Matrix::new(rows.len(), cols, data))
    }

    /// `rkfixed(y0, t0, t1, n, D)`: n classical Runge–Kutta steps from t0 to t1.
    #[inline(never)]
    pub(crate) fn rkfixed(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let (ode, mut y, t0, t1, n) = self.ode_problem(s, args, "rkfixed")?;
        let h = (t1 - t0) / n as f64;
        let mut rows = Vec::with_capacity(n + 1);
        rows.push((t0, y.clone()));
        let step =
            |y: &[f64], k: &[f64], f: f64| -> Vec<f64> { y.iter().zip(k).map(|(a, b)| a + f * b).collect() };
        for i in 0..n {
            let t = t0 + i as f64 * h;
            let k1 = self.rate(&ode, t, &y)?;
            let k2 = self.rate(&ode, t + h / 2.0, &step(&y, &k1, h / 2.0))?;
            let k3 = self.rate(&ode, t + h / 2.0, &step(&y, &k2, h / 2.0))?;
            let k4 = self.rate(&ode, t + h, &step(&y, &k3, h))?;
            for (yj, (((a, b), c), d)) in y.iter_mut().zip(k1.iter().zip(&k2).zip(&k3).zip(&k4)) {
                *yj += h / 6.0 * (a + 2.0 * b + 2.0 * c + d);
            }
            rows.push((if i + 1 == n { t1 } else { t0 + (i + 1) as f64 * h }, y.clone()));
        }
        Ok(Self::ode_table(&ode, rows))
    }

    /// `Rkadapt(y0, t0, t1, n, D)`: the same table as `rkfixed`, but each of the n intervals
    /// is crossed in as many Dormand–Prince 5(4) steps as an accuracy of about 1e-9 needs.
    #[inline(never)]
    pub(crate) fn rkadapt(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        const C: [f64; 6] = [1.0 / 5.0, 3.0 / 10.0, 4.0 / 5.0, 8.0 / 9.0, 1.0, 1.0];
        const A: [&[f64]; 6] = [
            &[1.0 / 5.0],
            &[3.0 / 40.0, 9.0 / 40.0],
            &[44.0 / 45.0, -56.0 / 15.0, 32.0 / 9.0],
            &[19372.0 / 6561.0, -25360.0 / 2187.0, 64448.0 / 6561.0, -212.0 / 729.0],
            &[9017.0 / 3168.0, -355.0 / 33.0, 46732.0 / 5247.0, 49.0 / 176.0, -5103.0 / 18656.0],
            &[35.0 / 384.0, 0.0, 500.0 / 1113.0, 125.0 / 192.0, -2187.0 / 6784.0, 11.0 / 84.0],
        ];
        // fifth-order weights minus the embedded fourth-order ones
        const E: [f64; 7] = [
            71.0 / 57600.0,
            0.0,
            -71.0 / 16695.0,
            71.0 / 1920.0,
            -17253.0 / 339200.0,
            22.0 / 525.0,
            -1.0 / 40.0,
        ];
        const RTOL: f64 = 1e-9;
        const MAX_STEPS: usize = 200_000;

        let (ode, mut y, t0, t1, n) = self.ode_problem(s, args, "Rkadapt")?;
        let k = y.len();
        let mut scale: Vec<f64> = y.iter().map(|v| v.abs()).collect();
        let mut rows = Vec::with_capacity(n + 1);
        rows.push((t0, y.clone()));
        let mut h = (t1 - t0) / n as f64;
        let mut t = t0;
        let mut k1 = self.rate(&ode, t, &y)?;
        let mut steps = 0;
        for i in 1..=n {
            let target = if i == n { t1 } else { t0 + (t1 - t0) * i as f64 / n as f64 };
            while (target - t) * h.signum() > 0.0 {
                steps += 1;
                if steps > MAX_STEPS {
                    bail!("Rkadapt needed more than {MAX_STEPS} steps; the equations may be stiff");
                }
                let last = (target - t).abs() <= h.abs();
                let hs = if last { target - t } else { h };
                let mut ks = vec![k1.clone()];
                let mut y5 = y.clone();
                for (stage, (a, c)) in A.iter().zip(C).enumerate() {
                    let sums = weighted_sum(a, &ks, k);
                    let yi: Vec<f64> = y.iter().zip(&sums).map(|(yj, s)| yj + hs * s).collect();
                    ks.push(self.rate(&ode, t + c * hs, &yi)?);
                    if stage == 5 {
                        y5 = yi;
                    }
                }
                let mut err: f64 = 0.0;
                let e_sums = weighted_sum(&E, &ks, k);
                for (((es, yj), y5j), sj) in e_sums.iter().zip(&y).zip(&y5).zip(&scale) {
                    let e = hs * es;
                    let sc = RTOL * (sj.max(yj.abs()).max(y5j.abs()));
                    let r = if e == 0.0 { 0.0 } else { e.abs() / sc.max(f64::MIN_POSITIVE) };
                    err = err.max(r);
                }
                if err <= 1.0 {
                    t = if last { target } else { t + hs };
                    y = y5;
                    k1 = ks.pop().ok_or_else(|| internal("the Runge-Kutta stages"))?;
                    for (sc, yj) in scale.iter_mut().zip(&y) {
                        *sc = sc.max(yj.abs());
                    }
                }
                let grow = if err == 0.0 { 5.0 } else { (0.9 * err.powf(-0.2)).clamp(0.2, 5.0) };
                if !(last && err <= 1.0) {
                    h = hs * grow;
                }
                if h.abs() <= 1e-14 * t.abs().max((t1 - t0).abs()) {
                    bail!(
                        "Rkadapt could not keep the error small near t = {t}: the solution may blow up there"
                    );
                }
            }
            rows.push((target, y.clone()));
        }
        Ok(Self::ode_table(&ode, rows))
    }

    /// `linfit(vx, vy, F)`: the coefficients c minimising Σ(vy − c·F(vx))², where F(x)
    /// returns the vector of basis functions (Mathcad's linfit).
    #[inline(never)]
    pub(crate) fn linfit(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let xs = quantities(&self.eval(s, arg(args, 0)?)?, "vx")?;
        let ys = quantities(&self.eval(s, arg(args, 1)?)?, "vy")?;
        let f = self.function_arg(s, arg(args, 2)?, &["x"], "linfit")?;
        if xs.len() != ys.len() {
            bail!("linfit needs vx and vy of the same length, not {} and {}", xs.len(), ys.len());
        }
        let ydim = ys.first().map_or(Dim::NONE, |q| q.dim);
        for q in &ys {
            same_dim(ydim, q.dim, "fit values that mix")?;
        }
        let mut basis_dims: Option<Vec<Dim>> = None;
        let mut a = Vec::new();
        for x in &xs {
            self.tick()?;
            let row =
                quantities(&self.apply(&f, vec![Value::Num(*x)])?, &format!("the result of {}(x)", f.name))?;
            match &basis_dims {
                None => basis_dims = Some(row.iter().map(|q| q.dim).collect()),
                Some(d) if d.len() != row.len() => {
                    bail!("{}(x) returns a different number of values for different x", f.name)
                }
                Some(d) => {
                    for (q, d) in row.iter().zip(d) {
                        if !fits(*q, *d) {
                            same_dim(*d, q.dim, &format!("fit with an {}(x) whose element mixes", f.name))?;
                        }
                    }
                }
            }
            a.extend(row.iter().map(|q| q.v));
        }
        let dims = basis_dims.unwrap_or_default();
        let m = dims.len();
        if m == 0 || xs.len() < m {
            bail!("linfit needs at least as many points ({}) as basis functions ({m})", xs.len());
        }
        crate::budget::charge(crate::budget::least_squares_work(xs.len(), m), 64)?;
        let Some(c) = numeric::least_squares(a, ys.iter().map(|q| q.v).collect(), m) else {
            bail!("linfit: the functions in {}(x) are not independent at these x values", f.name)
        };
        let data = c
            .iter()
            .zip(&dims)
            .map(|(c, d)| Ok(Value::Num(Quantity::new(*c, crate::builtins::dim_div(ydim, *d)?))))
            .collect::<Result<_>>()?;
        Ok(Value::Matrix(Matrix::column(data)))
    }
}

/// The unknowns of a system: `[x; y]` (each a number), or one vector variable `v`.
enum Unknowns {
    Names(Vec<String>),
    Vector(String),
}

impl Interp<'_> {
    /// `solve` with a vector of unknowns: `[x; y]`, or a variable whose value is a vector.
    pub(crate) fn is_system(s: &Scope, args: &[Expr]) -> bool {
        match args.get(1) {
            Some(Expr::Matrix(_)) => true,
            Some(Expr::Ident(n)) => {
                matches!(s.get(n), Some(Binding::Val(Value::Matrix(_))))
                    || matches!(args.get(2), Some(Expr::Matrix(_)))
            }
            _ => false,
        }
    }

    /// `solve([f1 == g1; f2 == g2], [x; y])`: the x, y that satisfy every equation, found by
    /// damped Newton steps from their current values (or from `solve(…, [x; y], [x0; y0])`).
    #[inline(never)]
    pub(crate) fn solve_system(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let unknowns = match arg(args, 1)? {
            Expr::Matrix(rows) => {
                if rows.len() != 1 && rows.first().is_none_or(|r| r.len() != 1) {
                    bail!("solve needs the unknowns as a vector, like [x; y]");
                }
                let mut names: Vec<String> = Vec::new();
                for cell in rows.iter().flatten() {
                    let Expr::Ident(n) = cell else { bail!("the unknowns [x; y] must be variable names") };
                    if names.contains(n) {
                        bail!("{n} appears twice among the unknowns");
                    }
                    names.push(n.clone());
                }
                Unknowns::Names(names)
            }
            Expr::Ident(n) => Unknowns::Vector(n.clone()),
            _ => bail!("the unknowns must be variable names, like [x; y]"),
        };
        let start: Vec<Quantity> = match (args.get(2), &unknowns) {
            (Some(g), _) => quantities(&self.eval(s, g)?, "the starting values")?,
            (None, Unknowns::Names(names)) => {
                let mut v = Vec::new();
                for n in names {
                    match s.get(n) {
                        Some(Binding::Val(Value::Num(q))) => v.push(*q),
                        _ => bail!(
                            "give {n} a starting value first, or pass them all: solve(…, [{}], [guesses])",
                            names.join("; ")
                        ),
                    }
                }
                v
            }
            (None, Unknowns::Vector(n)) => match s.get(n) {
                Some(Binding::Val(v)) => quantities(&v.clone(), "the starting value")?,
                _ => bail!("give {n} a starting value first"),
            },
        };
        let count = match &unknowns {
            Unknowns::Names(n) => n.len(),
            Unknowns::Vector(_) => start.len(),
        };
        if start.len() != count {
            bail!("solve needs {count} starting values, one for each unknown, not {}", start.len());
        }
        let eqs = match arg(args, 0)? {
            Expr::Matrix(rows) => {
                Expr::Matrix(rows.iter().map(|r| r.iter().map(Self::residual).collect()).collect())
            }
            e => Self::residual(e),
        };
        let names: Vec<String> = match &unknowns {
            Unknowns::Names(n) => n.clone(),
            Unknowns::Vector(n) => vec![n.clone()],
        };
        let prev: Vec<_> = names.iter().map(|n| s.get(n).cloned()).collect();
        let dims: Vec<Dim> = start.iter().map(|q| q.dim).collect();
        let x0: Vec<f64> = start.iter().map(|q| q.v).collect();
        let r = self.newton(s, &eqs, &unknowns, &dims, x0);
        for (n, p) in names.iter().zip(prev) {
            s.restore(n, p);
        }
        Ok(column_of(&r?, &dims))
    }

    /// The equations' residuals with the unknowns set to x.
    fn residuals(
        &mut self,
        s: &mut Scope,
        eqs: &Expr,
        unknowns: &Unknowns,
        dims: &[Dim],
        x: &[f64],
        rdims: &mut Option<Vec<Dim>>,
    ) -> Result<Vec<f64>> {
        self.tick()?;
        match unknowns {
            Unknowns::Names(names) => {
                for ((n, v), d) in names.iter().zip(x).zip(dims) {
                    s.bind(n, Value::Num(Quantity::new(*v, *d)));
                }
            }
            Unknowns::Vector(n) => {
                s.bind(n, column_of(x, dims));
            }
        }
        let r = quantities(&self.eval(s, eqs)?, "the equations")?;
        if r.len() != x.len() {
            bail!(
                "solve needs as many equations as unknowns, not {} equations for {} unknowns",
                r.len(),
                x.len()
            );
        }
        match rdims {
            None => *rdims = Some(r.iter().map(|q| q.dim).collect()),
            Some(d) => {
                for (q, d) in r.iter().zip(d.iter()) {
                    if !fits(*q, *d) {
                        same_dim(*d, q.dim, "solve an equation that mixes")?;
                    }
                }
            }
        }
        if r.iter().any(|q| !q.v.is_finite()) {
            bail!(
                "the equations have no finite value at these values of the unknowns; try other starting values"
            );
        }
        Ok(r.iter().map(|q| q.v).collect())
    }

    /// Newton's method with a numerical Jacobian, halving the step until the (scaled)
    /// residuals shrink.
    fn newton(
        &mut self,
        s: &mut Scope,
        eqs: &Expr,
        unknowns: &Unknowns,
        dims: &[Dim],
        mut x: Vec<f64>,
    ) -> Result<Vec<f64>> {
        let n = x.len();
        let mut rdims = None;
        let typical: Vec<f64> = x.iter().map(|v| v.abs().max(1e-3)).collect();
        let mut f = self.residuals(s, eqs, unknowns, dims, &x, &mut rdims)?;
        let mut weights: Option<Vec<f64>> = None;
        let merit = |f: &[f64], w: &[f64]| f.iter().zip(w).map(|(a, b)| (a * b).powi(2)).sum::<f64>();
        for _ in 0..100 {
            let mut jac = vec![0.0; n * n];
            for (j, (xj, tj)) in x.iter().zip(&typical).enumerate() {
                let h = 1e-7 * xj.abs().max(*tj);
                let xh: Vec<f64> =
                    x.iter().enumerate().map(|(i, v)| if i == j { v + h } else { *v }).collect();
                let fh = self.residuals(s, eqs, unknowns, dims, &xh, &mut rdims)?;
                for (i, (fhi, fi)) in fh.iter().zip(&f).enumerate() {
                    *numeric::el_mut(&mut jac, n, i, j).ok_or_else(|| internal("the Jacobian"))? =
                        (fhi - fi) / h;
                }
            }
            // each equation measured against the size of its own terms, so one in kN and
            // one in mm count alike
            let w = weights.get_or_insert_with(|| {
                jac.chunks_exact(n.max(1))
                    .zip(&f)
                    .map(|(row, fi)| {
                        let size =
                            row.iter().zip(&typical).map(|(a, t)| (a * t).abs()).fold(fi.abs(), f64::max);
                        if size > 0.0 { 1.0 / size } else { 1.0 }
                    })
                    .collect()
            });
            let phi = merit(&f, w);
            if phi.sqrt() <= 1e-13 {
                return Ok(x);
            }
            crate::budget::charge((n as u64).saturating_pow(3), 64)?;
            let Some(dx) = numeric::solve(jac, f.iter().map(|v| -v).collect()) else {
                bail!(
                    "the equations don't determine the unknowns here (their Jacobian is singular); try other starting values"
                );
            };
            let mut lambda = 1.0;
            loop {
                let xt: Vec<f64> = x.iter().zip(&dx).map(|(a, b)| a + lambda * b).collect();
                let ft = self.residuals(s, eqs, unknowns, dims, &xt, &mut rdims);
                if let Ok(ft) = ft
                    && (merit(&ft, w) <= (1.0 - 1e-4 * lambda) * phi || lambda < 1e-3)
                {
                    let small =
                        dx.iter().zip(&xt).all(|(d, v)| (lambda * d).abs() <= 1e-14 * v.abs().max(1e-300));
                    x = xt;
                    f = ft;
                    if small {
                        return if merit(&f, w).sqrt() <= 1e-8 { Ok(x) } else { Err(no_solution()) };
                    }
                    break;
                }
                lambda /= 2.0;
                if lambda < 1e-3 / 2.0 {
                    return Err(no_solution());
                }
            }
        }
        match weights {
            Some(w) if merit(&f, &w).sqrt() <= 1e-8 => Ok(x),
            _ => Err(no_solution()),
        }
    }
}

fn no_solution() -> crate::error::EvalError {
    crate::error::EvalError::new("no solution found near the starting values; try other starting values")
}

/// For each component i, Σₛ coefₛ·ks[s][i] with the stages added in order: the Runge–Kutta
/// combinations. `len` is the number of components.
fn weighted_sum(coef: &[f64], ks: &[Vec<f64>], len: usize) -> Vec<f64> {
    let mut sums = vec![-0.0; len];
    for (c, k) in coef.iter().zip(ks) {
        for (acc, kj) in sums.iter_mut().zip(k) {
            *acc += c * kj;
        }
    }
    sums
}

#[cfg(test)]
mod tests {
    use crate::stats::tests::shown_with;

    fn shown(lines: &[&str]) -> Vec<String> {
        shown_with(6, lines)
    }

    #[test]
    fn exponential_decay() {
        let r = shown(&[
            "τ := 2 's",
            "D(t, y) := -y/τ",
            "Y := rkfixed(10 'mm, 0, 4 's, 40, D)",
            "[rows(Y); cols(Y)]",
            "Y[41, 1]",
            "Y[41, 2]/(10 'mm*exp(-2))",
            "Z := Rkadapt(10 'mm, 0, 4 's, 4, D(t, y))",
            "Z[5, 2]/(10 'mm*exp(-2))",
            "Z[3, 1]",
        ]);
        assert_eq!(r[3], "[41; 2]");
        assert_eq!(r[4], "4 's");
        assert_eq!(r[5], "1");
        assert_eq!(r[7], "1");
        assert_eq!(r[8], "2 's");
    }

    #[test]
    fn harmonic_oscillator_keeps_its_energy() {
        // ω = sqrt(k/m) = 5 rad/s; x = 0.1 m·cos(ωt), E = k·x0²/2 = 0.25 J
        let r = shown(&[
            "m := 2 'kg",
            "k := 50 'N/'m",
            "D(t, y) := [y[2]; -k/m*y[1]]",
            "Y := rkfixed([0.1 'm; 0 'm/'s], 0 's, 2 's, 400, D)",
            "Y[401, 2]",
            "k*Y[401, 2]^2/2 + m*Y[401, 3]^2/2",
            "A := Rkadapt([0.1 'm; 0 'm/'s], 0, 2 's, 10, D)",
            "A[11, 2]",
            "A[6, 3]",
            "k*A[11, 2]^2/2 + m*A[11, 3]^2/2",
        ]);
        assert_eq!(r[4], "-0.083907 'm");
        assert_eq!(r[5], "0.25 'J");
        assert_eq!(r[7], "-0.083907 'm");
        // v = −0.5 m/s·sin(5 rad/s · 1 s)
        assert_eq!(r[8], "0.479462 'm/'s");
        assert_eq!(r[9], "0.25 'J");
    }

    #[test]
    fn ode_mistakes_explain_themselves() {
        let r = shown(&[
            "S(t, y) := y[1]",
            "D(t, y) := y",
            "rkfixed(1 'm, 0, 1 's, 10, D)",
            "E(t, y) := [y[2]; 0]",
            "rkfixed([1 'm; 0 'm/'s], 0, 1 's, 10, E)",
            "rkfixed([1; 2], 0, 1, 10, S(t, y))",
            "rkfixed(1, 0, 1, 10, F)",
            "rkfixed(1, 0, 1, 0, D)",
            "rkfixed(1, 0, 1, 10)",
            "rkfixed(1 'm, 0 's, 1 'kg, 10, D)",
            "G(t, y) := [y[2]; -y[1]/'s]",
            "rkfixed([1 'm; 0 'm/'s], 0, 1 's, 10, G)",
            "rkfixed(1, 0, 1, 10, 2)",
        ]);
        assert_eq!(
            r[2],
            "D(t, y) must return the rate of change of y: a speed (m/s), but it is a length (m)"
        );
        assert!(r[4].starts_with('['), "a plain 0 fits any rate: {}", r[4]);
        assert_eq!(r[5], "S(t, y) returned 1 values, but y has 2: it needs one rate for each");
        assert_eq!(
            r[6],
            "F(t, y) is not defined; rkfixed calls it with (t, y), so define F(t, y) := … above"
        );
        assert!(r[7].starts_with("rkfixed needs from 1 to "), "{}", r[7]);
        assert_eq!(r[8], "rkfixed takes 5 arguments: rkfixed(y0, t0, t1, n, D)");
        assert_eq!(r[9], "cannot integrate between a time (s) and a mass (kg)");
        assert_eq!(
            r[11],
            "G(t, y) must return the rate of change of y (element 2): an acceleration (m/s^2), but it is a speed (m/s)"
        );
        assert!(r[12].starts_with("rkfixed needs the name of a function of (t, y)"), "{}", r[12]);
    }

    #[test]
    fn linfit_on_basis_functions() {
        let r = shown(&[
            "F(x) := [1; x; x^2]",
            "linfit([0; 1; 2; 3; 4], [2; 5.5; 10; 15.5; 22], F)",
            "x := [0; 1; 2; 3] 'm",
            "c := linfit(x, [1; 3; 5; 7] 'kN, F)",
            "c[2]",
            "G(x) := [1; 2]",
            "linfit([0; 1; 2], [1; 2; 3], G)",
            "linfit([0; 1], [1; 2], F)",
            "linfit([0; 1; 2], [1; 2; 3], H)",
        ]);
        // y = 2 + 3x + x²/2
        assert_eq!(r[1], "[2; 3; 0.5]");
        assert_eq!(r[4], "2000 'N/'m");
        assert_eq!(r[6], "linfit: the functions in G(x) are not independent at these x values");
        assert_eq!(r[7], "linfit needs at least as many points (2) as basis functions (3)");
        assert_eq!(r[8], "H(x) is not defined; linfit calls it with (x), so define H(x) := … above");
    }

    #[test]
    fn systems_of_equations() {
        let r = shown(&[
            "x := 2 'm",
            "y := 0.5 'm",
            "solve([x^2 + y^2 == 4 'm^2; x*y == 1 'm^2], [x; y])",
            "solve([a^2 == 4 'm^2; a*F == 10 'kN*'m], [a; F], [1 'm; 1 'kN])",
            "v := [1; 1]",
            "G(v) := [v[1]^2 + v[2]^2 - 4; v[1] - v[2]]",
            "solve(G(v), v)",
            "solve(cos(z) == z, z, 1)",
            "solve([x + y == 1 'm; 2*x + 2*y == 2 'm], [x; y])",
            "solve([x + y == 1 'm], [x; y])",
            "solve([p + q == 1; p - q == 0], [p; q])",
            "solve([u^2 + 1 == 0; w == 1], [u; w], [1; 0])",
            "solve([x + y == 1 'm; x - y == 0], [x; y])",
            "solve([2*x + y == 4 'm; x - y == -1 'm], [x; x])",
        ]);
        // (x + y)² = 6 and (x − y)² = 2
        assert_eq!(r[2], "[1.931852 'm; 0.517638 'm]");
        assert_eq!(r[3], "[2 'm; 5000 'N]");
        assert_eq!(r[6], "[1.414214; 1.414214]");
        assert_eq!(r[7], "0.739085");
        assert!(r[8].starts_with("the equations don't determine the unknowns here"), "{}", r[8]);
        assert_eq!(r[9], "solve needs as many equations as unknowns, not 1 equations for 2 unknowns");
        assert!(r[10].starts_with("give p a starting value first"), "{}", r[10]);
        assert!(r[11].starts_with("no solution found near the starting values"), "{}", r[11]);
        assert_eq!(r[12], "cannot subtract a length (m) and a plain number");
        assert_eq!(r[13], "x appears twice among the unknowns");
    }
}
