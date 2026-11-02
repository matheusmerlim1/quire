use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use quire_ast::{BinaryOp, Expr, UnaryOp};
use quire_units::{Dim, Unit, UnitRegistry};

use crate::budget;
use crate::builtins::{self, dim_pow};
use crate::constants::constant;
use crate::error::{EvalError, Result, bail, internal};
use crate::ops::{self, arith, compare, same_dim};
use crate::value::{Matrix, Quantity, Value};

#[derive(Debug, Clone)]
pub enum Binding {
    Val(Value),
    Func(Rc<Func>),
    /// The definition exists but failed; uses report it instead of "not defined".
    Failed(String),
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
    /// Values visible where the function was defined (Mathcad/SMath semantics).
    pub env: Rc<Env>,
}

pub type Env = HashMap<String, Binding>;

/// The parameters of a call to `f`, and `f` itself (so it can call itself). Out of line: this
/// is on the stack once per call of a recursion, and the map's code would make that frame big.
#[inline(never)]
fn call_frame(f: &Rc<Func>, vals: Vec<Value>) -> HashMap<String, Binding> {
    let mut frame = HashMap::new();
    frame.insert(func_key(&f.name, f.params.len()), Binding::Func(Rc::clone(f)));
    for (p, v) in f.params.iter().zip(vals) {
        frame.insert(p.clone(), Binding::Val(v));
    }
    frame
}

/// Functions are keyed by name and arity, so `f(x)` and `f(x, y)` can coexist.
pub fn func_key(name: &str, arity: usize) -> String {
    format!("{name}#{arity}")
}

/// The `i`-th argument of a call. The lazy forms are chosen by argument count first, so this
/// only fails if a caller asks for an argument it never checked.
pub(crate) fn arg(args: &[Expr], i: usize) -> Result<&Expr> {
    args.get(i).ok_or_else(|| EvalError::new(format!("missing argument {}", i + 1)))
}

/// Lookup chain: local frames (innermost last), then a shared base environment.
pub struct Scope<'e> {
    base: &'e Env,
    frames: Vec<HashMap<String, Binding>>,
}

impl<'e> Scope<'e> {
    pub fn new(base: &'e Env) -> Self {
        Scope { base, frames: vec![HashMap::new()] }
    }

    pub fn get(&self, name: &str) -> Option<&Binding> {
        self.frames.iter().rev().find_map(|f| f.get(name)).or_else(|| self.base.get(name))
    }

    /// Bound in this evaluation (a function's parameter, a loop or plot variable) rather
    /// than defined in the document.
    fn is_local(&self, name: &str) -> bool {
        self.frames.iter().any(|f| f.contains_key(name))
    }

    fn set(&mut self, name: &str, b: Binding) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name.to_string(), b);
        }
    }

    /// Bind a loop/summation variable, returning what it shadowed in the frame.
    pub(crate) fn bind(&mut self, name: &str, v: Value) -> Option<Binding> {
        let frame = self.frames.last_mut()?;
        frame.insert(name.to_string(), Binding::Val(v))
    }

    pub(crate) fn restore(&mut self, name: &str, prev: Option<Binding>) {
        if let Some(frame) = self.frames.last_mut() {
            match prev {
                Some(b) => frame.insert(name.to_string(), b),
                None => frame.remove(name),
            };
        }
    }

    /// The bindings assigned in the outermost frame during evaluation: the names a
    /// worksheet's top-level expression wrote (`while`, `for`, `if`, `block` assignments).
    pub(crate) fn into_writes(self) -> HashMap<String, Binding> {
        self.frames.into_iter().next().unwrap_or_default()
    }
}

/// Deepest chain of user-function calls (`g(n) := … g(n - 1)`).
const MAX_DEPTH: usize = 256;
/// Deepest nesting of [`Interp::eval`]: a parsed expression is at most
/// [`quire_ast::MAX_DEPTH`] deep, but function bodies stack up when functions call each other.
/// This keeps evaluation within a 1 MB stack (the web version's).
const MAX_NESTING: usize = 1000;

#[cold]
#[inline(never)]
fn too_deep() -> EvalError {
    EvalError::new(format!(
        "the calculation nests too deeply (more than {MAX_NESTING} levels of calls and operators)"
    ))
}

fn placeholder() -> EvalError {
    EvalError::new("the expression is incomplete: fill in the empty placeholder (?)")
}

pub struct Interp<'u> {
    pub units: &'u UnitRegistry,
    depth: usize,
    nesting: usize,
    /// The step limit of this interpreter, on the thread's count (see [`budget`]).
    limit: u64,
    /// Plain names that resolved to units (`2 m`): the editor colours them as units.
    pub unit_idents: BTreeSet<String>,
    /// Plain names that resolved to built-in constants (`g`, `c`, `R`).
    pub constant_idents: BTreeSet<String>,
    /// Your variables whose names are also a built-in constant (they win, but it's worth saying).
    pub shadowing: BTreeSet<String>,
    /// Your variables read as a unit because they follow a number (`10 m` with `m := 25 kg`).
    pub units_over_variables: BTreeSet<String>,
    /// SMath documents (`settings.units.marked`): a bare name is never a unit or a constant,
    /// so only the predefined constants (π, e, ∞) resolve without a definition.
    pub marked_units: bool,
}

/// `2 m`, `9.81 m/s`, `64 kgf*m`: an expression that reads as a number with units.
fn quantity_like(e: &Expr) -> bool {
    match e {
        Expr::Binary(BinaryOp::Juxt, ..) | Expr::Unit(_) => true,
        Expr::Binary(BinaryOp::Mul | BinaryOp::Div, l, _) => quantity_like(l),
        _ => false,
    }
}

// Kept out of line, as `number` below: what a recursive frame inlines makes every level bigger.
#[inline(never)]
fn apply_unary(op: UnaryOp, v: Value) -> Result<Value> {
    match op {
        UnaryOp::Neg => ops::negate(&v),
        UnaryOp::Pos => Ok(v),
        UnaryOp::Not => Ok(Value::bool(!v.truthy()?)),
        UnaryOp::PlusMinus => bail!("± gives two results; this is not supported yet"),
    }
}

#[inline(never)]
fn number(t: &str) -> Result<Value> {
    parse_num(t).map(Value::num)
}

fn parse_num(t: &str) -> Result<f64> {
    let v = t.parse::<f64>().map_err(|_| EvalError::new(format!("'{t}' is not a valid number")))?;
    if v.is_infinite() {
        bail!("{t} is too large: numbers go up to about 1.8e308");
    }
    Ok(v)
}

/// A plain number `v` read on the temperature scale `u`: `-5 '°C` is 268.15 K, and a matrix
/// of plain numbers is read element by element. Next to other units (`'W/('m*'°C)`) only the
/// size of the degree counts, as for `'K`.
fn on_scale(v: Value, u: Unit) -> Result<Value> {
    let plain = |e: &Value| matches!(e, Value::Num(q) if q.dim.is_dimensionless());
    match v {
        Value::Num(q) if q.dim.is_dimensionless() => {
            Ok(Value::Num(Quantity::new(q.v * u.factor + u.offset, u.dim)))
        }
        Value::Matrix(m) if m.data.iter().all(plain) => {
            let data = m.data.into_iter().map(|e| on_scale(e, u)).collect::<Result<Vec<_>>>()?;
            Ok(Value::Matrix(Matrix::new(m.rows, m.cols, data)))
        }
        Value::Sym(_) => {
            bail!("a temperature in °C or °F needs a number in front of the unit, not a formula")
        }
        other => arith(BinaryOp::Mul, &other, &Value::Num(Quantity::new(u.factor, u.dim))),
    }
}

fn ident<'a>(e: &'a Expr, what: &str) -> Result<&'a str> {
    match e {
        Expr::Ident(n) => Ok(n),
        _ => bail!("{what} must be a variable name"),
    }
}

impl<'u> Interp<'u> {
    pub fn new(units: &'u UnitRegistry) -> Self {
        Interp {
            units,
            depth: 0,
            nesting: 0,
            limit: budget::limit_from_now(),
            unit_idents: BTreeSet::new(),
            constant_idents: BTreeSet::new(),
            shadowing: BTreeSet::new(),
            units_over_variables: BTreeSet::new(),
            marked_units: false,
        }
    }

    /// Turn on marked-units mode (see [`Interp::marked_units`]).
    pub fn with_marked_units(mut self, on: bool) -> Self {
        self.marked_units = on;
        self
    }

    /// The unit a bare name stands for, if any. Marked documents never read a bare name as a unit.
    fn unit_named(&self, name: &str) -> Option<Unit> {
        if self.marked_units { None } else { self.units.lookup(name) }
    }

    pub(crate) fn tick(&mut self) -> Result<()> {
        budget::tick(1, self.limit)
    }

    pub fn unit(&self, name: &str) -> Result<Unit> {
        self.units.lookup(name).ok_or_else(|| EvalError::new(format!("unknown unit '{name}")))
    }

    pub fn eval(&mut self, s: &mut Scope, e: &Expr) -> Result<Value> {
        // Every walk down the tree and every call passes here, so this bounds the stack the
        // evaluation can use. The arms call out to keep this frame small, as it is on the
        // stack once per level; the helpers on that path are `#[inline(never)]` so the
        // optimiser doesn't fold their locals back into it.
        if self.nesting >= MAX_NESTING {
            return Err(too_deep());
        }
        if self.nesting == 0 {
            // the matrix code charges its work against this interpreter's limit
            let limit = self.limit;
            return budget::within(limit, || {
                self.nesting += 1;
                let r = self.eval(s, e);
                self.nesting -= 1;
                r
            });
        }
        self.nesting += 1;
        let r = match e {
            Expr::Num(t) => number(t),
            Expr::Ident(name) => self.lookup(s, name),
            Expr::Unit(name) => self.unit_value(name),
            Expr::Str(t) => Ok(Value::Str(t.clone())),
            Expr::Placeholder => Err(placeholder()),
            Expr::Paren(x) => self.eval(s, x),
            Expr::Unary(op, x) => self.unary(s, *op, x),
            Expr::Factorial(x) => self.factorial(s, x),
            Expr::Binary(op, a, b) => self.binary(s, *op, a, b),
            Expr::Compare(first, rest) => self.compare_chain(s, first, rest),
            Expr::Call(name, args) => self.call(s, name, args),
            Expr::Index(base, idx) => self.element(s, base, idx),
            Expr::Matrix(rows) => self.matrix(s, rows),
            Expr::Range(start, end) => self.range(s, start, end),
            Expr::Define { lhs, rhs, .. } => self.define_local(s, lhs, rhs),
        };
        self.nesting -= 1;
        r
    }

    #[inline(never)]
    fn unit_value(&self, name: &str) -> Result<Value> {
        let u = self.unit(name)?;
        Ok(Value::Num(Quantity::new(u.factor, u.dim)))
    }

    #[inline(never)]
    fn unary(&mut self, s: &mut Scope, op: UnaryOp, x: &Expr) -> Result<Value> {
        if op == UnaryOp::Neg
            && let Some(v) = self.below_zero(s, x)?
        {
            return Ok(v);
        }
        let v = self.eval(s, x)?;
        apply_unary(op, v)
    }

    /// `-5 '°C` is five degrees below zero on the scale, not minus 278.15 K. `None` when `x`
    /// is not a value on a temperature scale.
    #[inline(never)]
    fn below_zero(&mut self, s: &mut Scope, x: &Expr) -> Result<Option<Value>> {
        let Expr::Binary(times, a, b) = x else { return Ok(None) };
        let Some(u) = self.offset_unit(s, *times, b)? else { return Ok(None) };
        let v = ops::negate(&self.eval(s, a)?)?;
        on_scale(v, u).map(Some)
    }

    #[inline(never)]
    fn factorial(&mut self, s: &mut Scope, x: &Expr) -> Result<Value> {
        let n = self.eval(s, x)?.as_int("the argument of !")?;
        if !(0..=170).contains(&n) {
            bail!("{n}! is out of range");
        }
        Ok(Value::num((1..=n).map(|k| k as f64).product()))
    }

    #[inline(never)]
    fn compare_chain(
        &mut self,
        s: &mut Scope,
        first: &Expr,
        rest: &[(quire_ast::CmpOp, Expr)],
    ) -> Result<Value> {
        let mut left = self.eval(s, first)?;
        let mut holds = true;
        for (op, x) in rest {
            let right = self.eval(s, x)?;
            holds &= compare(*op, &left, &right)?;
            left = right;
        }
        Ok(Value::bool(holds))
    }

    #[inline(never)]
    fn element(&mut self, s: &mut Scope, base: &Expr, idx: &[Expr]) -> Result<Value> {
        let b = self.eval(s, base)?;
        let ix = idx.iter().map(|i| self.eval(s, i)).collect::<Result<Vec<_>>>()?;
        // a formula with an index is a formula: x[2] for x with no value
        if matches!(b, Value::Sym(_)) {
            return crate::symbolic::bridge::element(base, &ix);
        }
        builtins::index(&b, &ix)
    }

    #[inline(never)]
    fn matrix(&mut self, s: &mut Scope, rows: &[Vec<Expr>]) -> Result<Value> {
        let cols = rows.first().map_or(0, |r| r.len());
        let mut data = Vec::with_capacity(rows.len() * cols);
        for cell in rows.iter().flatten() {
            data.push(self.eval(s, cell)?);
        }
        Ok(Value::Matrix(Matrix::new(rows.len(), cols, data)))
    }

    #[inline(never)]
    fn range(&mut self, s: &mut Scope, start: &Expr, end: &Expr) -> Result<Value> {
        let a = self.eval(s, start)?.as_num("the range start")?;
        let b = self.eval(s, end)?.as_num("the range end")?;
        let step = self.range_step(s, start, end, a, b)?;
        builtins::range(a, b, step)
    }

    #[inline(never)]
    fn define_local(&mut self, s: &mut Scope, lhs: &Expr, rhs: &Expr) -> Result<Value> {
        let v = self.eval(s, rhs)?;
        self.assign_local(s, lhs, v.clone())?;
        Ok(v)
    }

    /// Assignment inside an expression (program blocks, loops): writes the innermost frame.
    fn assign_local(&mut self, s: &mut Scope, lhs: &Expr, v: Value) -> Result<()> {
        match lhs {
            Expr::Ident(name) => s.set(name, Binding::Val(v)),
            Expr::Index(base, idx) => {
                let name = ident(base, "the indexed target")?;
                let ix =
                    idx.iter().map(|i| self.eval(s, i)?.as_int("an index")).collect::<Result<Vec<_>>>()?;
                let current = match s.get(name) {
                    Some(Binding::Val(v)) => Some(v.clone()),
                    _ => None,
                };
                s.set(name, Binding::Val(set_element(current, &ix, v)?));
            }
            _ => bail!("cannot assign to this expression"),
        }
        Ok(())
    }

    #[inline(never)]
    fn binary(&mut self, s: &mut Scope, op: BinaryOp, a: &Expr, b: &Expr) -> Result<Value> {
        match op {
            BinaryOp::And => {
                let left = self.eval(s, a)?.truthy()?;
                return Ok(Value::bool(left && self.eval(s, b)?.truthy()?));
            }
            BinaryOp::Or => {
                let left = self.eval(s, a)?.truthy()?;
                return Ok(Value::bool(left || self.eval(s, b)?.truthy()?));
            }
            _ => {}
        }
        // `20 '°C`, `T '°C`, `[10; 20] degC`: absolute temperatures, with the scale's offset
        if let Some(u) = self.offset_unit(s, op, b)? {
            let x = self.eval(s, a)?;
            return on_scale(x, u);
        }
        let x = self.eval(s, a)?;
        // right after a number or unit, a plain name is a unit: `2 g` grams, `100 km/h` hours,
        // and `10 m` metres even when you have a variable m (`10*m` multiplies by it)
        let y = if op == BinaryOp::Juxt || (op == BinaryOp::Div && quantity_like(a)) {
            self.eval_unit_position(s, b)?
        } else {
            self.eval(s, b)?
        };
        arith(op, &x, &y)
    }

    /// The step of `start..end` with units: one of the unit written on its ends, so
    /// `0 'mm..10 'mm` steps by 1 mm. `None` (the range then asks for a step) when no unit is
    /// written on either end, as in `a..b` for two lengths; an error when the ends are written
    /// in different units.
    #[inline(never)]
    fn range_step(
        &mut self,
        s: &mut Scope,
        start: &Expr,
        end: &Expr,
        a: Quantity,
        b: Quantity,
    ) -> Result<Option<Quantity>> {
        if a.dim.is_dimensionless() && b.dim.is_dimensionless() {
            return Ok(None);
        }
        same_dim(a.dim, b.dim, "make a range from")?;
        let unit = match (self.written_unit(s, start)?, self.written_unit(s, end)?) {
            (Some(x), Some(y)) if x != y => bail!(
                "the ends of this range are written in different units, so its step is unclear: give it with range(start, end, step)"
            ),
            (Some(u), _) | (None, Some(u)) => u,
            (None, None) => return Ok(None),
        };
        let sign = if b.v >= a.v { 1.0 } else { -1.0 };
        Ok(Some(Quantity::new(sign * unit.v, unit.dim)))
    }

    /// The unit written after the number of a range end: `'mm` in `0 'mm`, `km/h` in `100 km/h`.
    fn written_unit(&mut self, s: &mut Scope, e: &Expr) -> Result<Option<Quantity>> {
        match e {
            Expr::Paren(x) | Expr::Unary(UnaryOp::Neg | UnaryOp::Pos, x) => self.written_unit(s, x),
            Expr::Binary(BinaryOp::Juxt, n, u)
                if matches!(**n, Expr::Num(_)) && self.is_unit_term(s, u, true) =>
            {
                Ok(Some(self.eval_unit_position(s, u)?.as_num("a unit")?))
            }
            Expr::Binary(op @ (BinaryOp::Mul | BinaryOp::Div), l, r)
                if quantity_like(l) && self.is_unit_term(s, r, *op == BinaryOp::Div) =>
            {
                let Some(left) = self.written_unit(s, l)? else { return Ok(None) };
                let right =
                    if *op == BinaryOp::Div { self.eval_unit_position(s, r)? } else { self.eval(s, r)? };
                Ok(Some(ops::arith(*op, &Value::Num(left), &right)?.as_num("a unit")?))
            }
            _ => Ok(None),
        }
    }

    /// `e` is a unit as written (`'mm`, `kN`, `m^2`). In unit position (after a number, or
    /// after `/` in a quantity) a unit name wins over a variable; elsewhere a variable wins.
    fn is_unit_term(&self, s: &Scope, e: &Expr, unit_position: bool) -> bool {
        match e {
            Expr::Unit(_) => true,
            Expr::Ident(n) => {
                let unit = !s.is_local(n) && self.unit_named(n).is_some();
                unit && (unit_position || (s.get(n).is_none() && constant(n).is_none()))
            }
            Expr::Binary(BinaryOp::Pow, base, _) | Expr::Paren(base) => {
                self.is_unit_term(s, base, unit_position)
            }
            _ => false,
        }
    }

    /// The temperature scale (°C, °F: a unit with an offset) that `b` names when a value is
    /// multiplied by it, as in `20 '°C` or `T*degC`. A plain name counts only where it would be
    /// read as a unit.
    #[inline(never)]
    fn offset_unit(&mut self, s: &Scope, op: BinaryOp, b: &Expr) -> Result<Option<Unit>> {
        if !matches!(op, BinaryOp::Juxt | BinaryOp::Mul) {
            return Ok(None);
        }
        let unit = match b {
            Expr::Unit(name) => Some(self.unit(name)?),
            Expr::Ident(name) if !s.is_local(name) && (op == BinaryOp::Juxt || s.get(name).is_none()) => {
                self.unit_named(name)
            }
            _ => None,
        };
        let Some(u) = unit.filter(|u| u.offset != 0.0) else { return Ok(None) };
        if let Expr::Ident(name) = b {
            self.unit_idents.insert(name.clone());
        }
        Ok(Some(u))
    }

    fn eval_unit_position(&mut self, s: &mut Scope, e: &Expr) -> Result<Value> {
        match e {
            // a function's own parameter stays itself: `v(t) := 9.81 t`
            Expr::Ident(name) if !s.is_local(name) => match self.unit_named(name) {
                Some(u) => {
                    self.unit_idents.insert(name.clone());
                    if matches!(s.get(name), Some(Binding::Val(_))) {
                        self.units_over_variables.insert(name.clone());
                    }
                    Ok(Value::Num(Quantity::new(u.factor, u.dim)))
                }
                None => self.eval(s, e),
            },
            Expr::Binary(BinaryOp::Pow, base, exp) if matches!(**base, Expr::Ident(_)) => {
                let b = self.eval_unit_position(s, base)?;
                let x = self.eval(s, exp)?;
                arith(BinaryOp::Pow, &b, &x)
            }
            Expr::Paren(x) => self.eval_unit_position(s, x),
            _ => self.eval(s, e),
        }
    }

    #[inline(never)]
    fn call(&mut self, s: &mut Scope, name: &str, args: &[Expr]) -> Result<Value> {
        self.tick()?;
        match s.get(&func_key(name, args.len())) {
            Some(Binding::Func(f)) => {
                let f = Rc::clone(f);
                return self.call_user(s, &f, args);
            }
            Some(Binding::Failed(_)) => {
                bail!("{name}(…) has no value (its definition has an error)")
            }
            _ => {}
        }
        if let Some(r) = self.lazy(s, name, args) {
            return r;
        }
        self.call_builtin(s, name, args)
    }

    /// One of the document's functions, with its arguments worked out.
    fn call_user(&mut self, s: &mut Scope, f: &Rc<Func>, args: &[Expr]) -> Result<Value> {
        let vals = args.iter().map(|a| self.eval(s, a)).collect::<Result<Vec<_>>>()?;
        // SMath mode: the body sees the worksheet as it is now, not as it was when the
        // function was defined (a constant defined further down is still its value)
        if self.marked_units {
            return self.apply_in(f, vals, s.base);
        }
        self.apply(f, vals)
    }

    // Kept out of `call`, whose frame is on the stack for every call a recursion makes.
    #[inline(never)]
    fn call_builtin(&mut self, s: &mut Scope, name: &str, args: &[Expr]) -> Result<Value> {
        let vals = args.iter().map(|a| self.eval(s, a)).collect::<Result<Vec<_>>>()?;
        if let Some(r) = crate::symbolic::bridge::call(name, &vals) {
            return r;
        }
        match builtins::call(name, &vals) {
            Some(r) => r,
            None => match crate::special::unsupported(name) {
                Some(why) => Err(EvalError::new(why)),
                None => bail!("{name} is not a known function"),
            },
        }
    }

    pub(crate) fn apply(&mut self, f: &Rc<Func>, vals: Vec<Value>) -> Result<Value> {
        let env = Rc::clone(&f.env);
        self.apply_in(f, vals, &env)
    }

    /// Calls `f` with the names of `base` visible besides its parameters.
    fn apply_in(&mut self, f: &Rc<Func>, vals: Vec<Value>, base: &Env) -> Result<Value> {
        if self.depth >= MAX_DEPTH {
            bail!("{} calls itself too deeply", f.name);
        }
        let frame = call_frame(f, vals);
        let base = if self.marked_units { base } else { &*f.env };
        let mut scope = Scope { base, frames: vec![frame] };
        self.depth += 1;
        let r = self.eval(&mut scope, &f.body);
        self.depth -= 1;
        r
    }

    /// Constructs that control evaluation of their own arguments. `None` = not lazy.
    /// `if` and `block` recurse through here, so the larger constructs are kept out of line
    /// (`#[inline(never)]`) to keep this frame small.
    #[inline(never)]
    fn lazy(&mut self, s: &mut Scope, name: &str, args: &[Expr]) -> Option<Result<Value>> {
        Some(match (name, args.len()) {
            ("if", 3) => self.if_(s, args),
            ("block", _) => self.block(s, args),
            ("for", 3) => self.for_in(s, args),
            ("for", 4) => self.for_c(s, args),
            ("while", 2) => self.while_(s, args),
            ("sum", 4) => self.fold(s, args, BinaryOp::Add),
            ("prod" | "product", 4) => self.fold(s, args, BinaryOp::Mul),
            ("int", 4) => self.integrate(s, args),
            ("int", 2) => {
                Err(EvalError::new("an indefinite integral is symbolic: end the line with → instead of ="))
            }
            ("diff", 2 | 3) => self.differentiate(s, args),
            ("root" | "solve", 4) => self.solve_in(s, args),
            ("solve", 2 | 3) if Self::is_system(s, args) => self.solve_system(s, args),
            ("solve", 2 | 3) => self.solve_from(s, args),
            ("rkfixed", 5) => self.rkfixed(s, args),
            ("Rkadapt", 5) => self.rkadapt(s, args),
            ("linfit", 3) => self.linfit(s, args),
            ("rkfixed" | "Rkadapt", _) => {
                Err(EvalError::new(format!("{name} takes 5 arguments: {name}(y0, t0, t1, n, D)")))
            }
            ("linfit", _) => Err(EvalError::new("linfit takes 3 arguments: linfit(vx, vy, F)")),
            ("lim", _) => Err(EvalError::new("a limit is symbolic: end the line with → instead of =")),
            ("IsDefined", 1) => self.is_defined(s, args),
            ("IsDefined", _) => Err(EvalError::new("IsDefined takes 1 argument: IsDefined(x)")),
            _ => return None,
        })
    }

    /// `IsDefined(x)`: 1 when the name x has a value here (a variable above, or a constant),
    /// 0 when it has none or its definition failed. The name itself is not evaluated.
    fn is_defined(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let name = ident(arg(args, 0)?, "the argument of IsDefined")?;
        let defined = match s.get(name) {
            Some(Binding::Val(_)) => true,
            Some(Binding::Func(_) | Binding::Failed(_)) => false,
            // in SMath mode a name with no value is a symbol, also when it names a constant
            None => {
                matches!(name, "π" | "pi" | "e" | "∞") || (!self.marked_units && constant(name).is_some())
            }
        };
        Ok(Value::bool(defined))
    }

    fn if_(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let c = self.eval(s, arg(args, 0)?)?.truthy()?;
        self.eval(s, arg(args, if c { 1 } else { 2 })?)
    }

    #[inline(never)]
    fn block(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let mut last = Value::num(0.0);
        for a in args {
            last = self.eval(s, a)?;
        }
        Ok(last)
    }

    #[inline(never)]
    fn for_in(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let var = ident(arg(args, 0)?, "the loop variable")?;
        let items = match self.eval(s, arg(args, 1)?)? {
            Value::Matrix(m) => m.data,
            v => vec![v],
        };
        let mut last = Value::num(0.0);
        for item in items {
            self.tick()?;
            s.set(var, Binding::Val(item));
            last = self.eval(s, arg(args, 2)?)?;
        }
        Ok(last)
    }

    #[inline(never)]
    fn for_c(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        self.eval(s, arg(args, 0)?)?;
        let mut last = Value::num(0.0);
        while self.eval(s, arg(args, 1)?)?.truthy()? {
            self.tick()?;
            last = self.eval(s, arg(args, 3)?)?;
            self.eval(s, arg(args, 2)?)?;
        }
        Ok(last)
    }

    #[inline(never)]
    fn while_(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let mut last = Value::num(0.0);
        while self.eval(s, arg(args, 0)?)?.truthy()? {
            self.tick()?;
            last = self.eval(s, arg(args, 1)?)?;
        }
        Ok(last)
    }

    /// `sum(expr, i, a, b)` / `prod(expr, i, a, b)` over whole numbers a..b.
    #[inline(never)]
    fn fold(&mut self, s: &mut Scope, args: &[Expr], op: BinaryOp) -> Result<Value> {
        let var = ident(arg(args, 1)?, "the summation variable")?;
        let a = self.eval(s, arg(args, 2)?)?.as_int("the lower limit")?;
        let b = self.eval(s, arg(args, 3)?)?.as_int("the upper limit")?;
        let mut acc: Option<Value> = None;
        let prev = s.get(var).cloned();
        let mut result = Ok(());
        for k in a..=b {
            if let Err(e) = self.tick() {
                result = Err(e);
                break;
            }
            s.bind(var, Value::num(k as f64));
            match self.eval(s, arg(args, 0)?).and_then(|v| match &acc {
                None => Ok(v),
                Some(x) => arith(op, x, &v),
            }) {
                Ok(v) => acc = Some(v),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        s.restore(var, prev);
        result?;
        Ok(acc.unwrap_or_else(|| Value::num(if op == BinaryOp::Mul { 1.0 } else { 0.0 })))
    }

    fn sample(
        &mut self,
        s: &mut Scope,
        f: &Expr,
        var: &str,
        x: Quantity,
        fdim: &mut Option<Dim>,
    ) -> Result<f64> {
        s.bind(var, Value::Num(x));
        let y = self.eval(s, f)?.as_num("the integrand")?;
        match fdim {
            None => *fdim = Some(y.dim),
            Some(d) => same_dim(*d, y.dim, "integrate a function that mixes")?,
        }
        Ok(y.v)
    }

    /// Adaptive Simpson quadrature of `int(f, x, a, b)`.
    #[inline(never)]
    fn integrate(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let var = ident(arg(args, 1)?, "the integration variable")?;
        let mut a = self.eval(s, arg(args, 2)?)?.as_num("the lower limit")?;
        let mut b = self.eval(s, arg(args, 3)?)?.as_num("the upper limit")?;
        // a plain 0 limit takes the other limit's units: `int(q(x), x, 0, L)`
        if a.v == 0.0 {
            a.dim = b.dim;
        } else if b.v == 0.0 {
            b.dim = a.dim;
        }
        same_dim(a.dim, b.dim, "integrate between")?;
        if !a.v.is_finite() || !b.v.is_finite() {
            bail!("integrals with infinite limits are not supported yet");
        }
        let prev = s.get(var).cloned();
        let mut fdim = None;
        let r: Result<f64> = (|| {
            let at = |t: f64| Quantity::new(t, a.dim);
            let m = 0.5 * (a.v + b.v);
            let fa = self.sample(s, arg(args, 0)?, var, at(a.v), &mut fdim)?;
            let fm = self.sample(s, arg(args, 0)?, var, at(m), &mut fdim)?;
            let fb = self.sample(s, arg(args, 0)?, var, at(b.v), &mut fdim)?;
            let whole = (b.v - a.v) / 6.0 * (fa + 4.0 * fm + fb);
            let eps = 1e-10 * whole.abs().max(1e-300);
            self.simpson(s, arg(args, 0)?, var, a.dim, (a.v, b.v), (fa, fm, fb), whole, eps, 48, &mut fdim)
        })();
        s.restore(var, prev);
        let v = r?;
        Ok(Value::Num(Quantity::new(v, builtins::dim_mul(fdim.unwrap_or(Dim::NONE), a.dim)?)))
    }

    #[allow(clippy::too_many_arguments)]
    fn simpson(
        &mut self,
        s: &mut Scope,
        f: &Expr,
        var: &str,
        xdim: Dim,
        (a, b): (f64, f64),
        (fa, fm, fb): (f64, f64, f64),
        whole: f64,
        eps: f64,
        depth: u32,
        fdim: &mut Option<Dim>,
    ) -> Result<f64> {
        self.tick()?;
        let m = 0.5 * (a + b);
        let (lm, rm) = (0.5 * (a + m), 0.5 * (m + b));
        let flm = self.sample(s, f, var, Quantity::new(lm, xdim), fdim)?;
        let frm = self.sample(s, f, var, Quantity::new(rm, xdim), fdim)?;
        let left = (m - a) / 6.0 * (fa + 4.0 * flm + fm);
        let right = (b - m) / 6.0 * (fm + 4.0 * frm + fb);
        let delta = left + right - whole;
        if depth == 0 || delta.abs() <= 15.0 * eps {
            return Ok(left + right + delta / 15.0);
        }
        Ok(self.simpson(s, f, var, xdim, (a, m), (fa, flm, fm), left, eps / 2.0, depth - 1, fdim)?
            + self.simpson(s, f, var, xdim, (m, b), (fm, frm, fb), right, eps / 2.0, depth - 1, fdim)?)
    }

    /// `f(x)` as written, or `lhs - rhs` for an equation `lhs == rhs`.
    pub(crate) fn residual(e: &Expr) -> Expr {
        match e {
            Expr::Compare(l, rest) => match rest.as_slice() {
                [(quire_ast::CmpOp::Eq, r)] => Expr::Binary(BinaryOp::Sub, l.clone(), Box::new(r.clone())),
                _ => e.clone(),
            },
            other => other.clone(),
        }
    }

    /// `root(f(x), x, a, b)` / `solve(x^2 == 2, x, a, b)`: a root between a and b.
    /// Without a sign change at the ends, the range is scanned for one.
    #[inline(never)]
    fn solve_in(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let var = ident(arg(args, 1)?, "the unknown")?;
        let a = self.eval(s, arg(args, 2)?)?.as_num("the lower limit")?;
        let b = self.eval(s, arg(args, 3)?)?.as_num("the upper limit")?;
        let dim = if a.v == 0.0 { b.dim } else { a.dim };
        same_dim(dim, if b.v == 0.0 { dim } else { b.dim }, "search between")?;
        let f = Self::residual(arg(args, 0)?);
        let prev = s.get(var).cloned();
        let mut fdim = None;
        let r: Result<f64> = (|| {
            let at = |t: f64| Quantity::new(t, dim);
            let (mut lo, mut hi) = (a.v.min(b.v), a.v.max(b.v));
            let mut flo = self.sample(s, &f, var, at(lo), &mut fdim)?;
            let mut fhi = self.sample(s, &f, var, at(hi), &mut fdim)?;
            if flo == 0.0 {
                return Ok(lo);
            }
            if fhi == 0.0 {
                return Ok(hi);
            }
            if flo.signum() == fhi.signum() {
                // look for the first sign change from the lower end
                const STEPS: usize = 200;
                let step = (hi - lo) / STEPS as f64;
                let mut found = None;
                let mut x0 = lo;
                for k in 1..=STEPS {
                    let x1 = lo + step * k as f64;
                    let f1 = self.sample(s, &f, var, at(x1), &mut fdim)?;
                    if f1 == 0.0 {
                        return Ok(x1);
                    }
                    if f1.signum() != flo.signum() {
                        found = Some((x0, x1, f1));
                        break;
                    }
                    (x0, flo) = (x1, f1);
                }
                let Some((x0, x1, f1)) = found else {
                    bail!("no root between the limits: the expression doesn't change sign there")
                };
                (lo, hi, fhi) = (x0, x1, f1);
            }
            let (width, scale) = (hi - lo, flo.abs().max(fhi.abs()));
            for _ in 0..200 {
                self.tick()?;
                let mid = 0.5 * (lo + hi);
                if mid == lo || mid == hi {
                    break;
                }
                let fm = self.sample(s, &f, var, at(mid), &mut fdim)?;
                if fm == 0.0 {
                    return Ok(mid);
                }
                if fm.signum() == flo.signum() {
                    (lo, flo) = (mid, fm);
                } else {
                    (hi, fhi) = (mid, fm);
                }
            }
            // A sign change is not a root when the expression jumps across zero (floor(x) - 1.5)
            // or through a pole (tan(x)): there it stays far from zero however narrow the
            // bracket gets. Near a root it shrinks with the bracket: allow 1e-6 of the values
            // the bisection started from, or what a straight line would leave in the bracket
            // when it stopped (a range too wide for 200 halvings).
            let residual = flo.abs().min(fhi.abs());
            if residual > scale * (1e-6 + 64.0 * (hi - lo) / width) {
                let at_x = self.show_quantity(at(if flo.abs() <= fhi.abs() { lo } else { hi }));
                if residual > scale {
                    bail!(
                        "no root between the limits: the expression changes sign at {var} = {at_x} by going through infinity (a pole), not through zero"
                    );
                }
                bail!(
                    "no root between the limits: the expression jumps across zero at {var} = {at_x} without reaching it"
                );
            }
            Ok(0.5 * (lo + hi))
        })();
        s.restore(var, prev);
        Ok(Value::Num(Quantity::new(r?, dim)))
    }

    /// A quantity as a message shows it: `1.5708`, `0.25 'm`.
    fn show_quantity(&self, q: Quantity) -> String {
        let (spec, settings) =
            (crate::format::DisplaySpec::default(), crate::format::DisplaySettings::default());
        crate::format::format_value(&Value::Num(q), &spec, &settings, self.units)
            .unwrap_or_else(|_| q.v.to_string())
    }

    /// `solve(f(x), x)`: a root near the current value of x (secant steps), or near a
    /// starting value given as `solve(f(x), x, x0)`.
    #[inline(never)]
    fn solve_from(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let var = ident(arg(args, 1)?, "the unknown")?;
        let x0 = match (args.get(2), s.get(var)) {
            (Some(g), _) => self.eval(s, g)?.as_num("the starting value")?,
            (None, Some(Binding::Val(Value::Num(q)))) => *q,
            _ => bail!("give {var} a starting value first (or search a range: solve(…, {var}, a, b))"),
        };
        let f = Self::residual(arg(args, 0)?);
        let prev = s.get(var).cloned();
        let mut fdim = None;
        let r: Result<f64> = (|| {
            let at = |t: f64| Quantity::new(t, x0.dim);
            let mut a = x0.v;
            let mut b = if a == 0.0 { 1e-4 } else { a * (1.0 + 1e-4) };
            let mut fa = self.sample(s, &f, var, at(a), &mut fdim)?;
            let mut fb = self.sample(s, &f, var, at(b), &mut fdim)?;
            for _ in 0..100 {
                self.tick()?;
                if fb == 0.0 || (b - a).abs() <= 1e-14 * b.abs().max(1e-300) {
                    return Ok(b);
                }
                if fb == fa {
                    break;
                }
                let c = b - fb * (b - a) / (fb - fa);
                if !c.is_finite() {
                    break;
                }
                (a, fa) = (b, fb);
                b = c;
                fb = self.sample(s, &f, var, at(b), &mut fdim)?;
            }
            bail!("no root found near {var} = {}; try a range: solve(…, {var}, a, b)", x0.v)
        })();
        s.restore(var, prev);
        Ok(Value::Num(Quantity::new(r?, x0.dim)))
    }

    /// `diff(f, x[, n])`: a numeric derivative at the current value of x, or in SMath mode,
    /// with x unbound, the derivative as a formula.
    #[inline(never)]
    fn differentiate(&mut self, s: &mut Scope, args: &[Expr]) -> Result<Value> {
        let order = match args.get(2) {
            Some(n) => self.eval(s, n)?.as_int("the derivative order")?,
            None => 1,
        };
        if order < 0 {
            bail!("a derivative order cannot be negative, but this one is {order}");
        }
        // the 0th derivative is the expression itself
        if order == 0 {
            return self.eval(s, arg(args, 0)?);
        }
        if let Expr::Ident(var) = arg(args, 1)?
            && let Some(Binding::Val(Value::Num(x0))) = s.get(var)
        {
            let (var, x0) = (var.clone(), *x0);
            if (1..=2).contains(&order) {
                return self.numeric_derivative(s, args, &var, order, x0);
            }
            return self.derivative_at(s, args, order, x0);
        }
        if !self.marked_units {
            let var = ident(arg(args, 1)?, "the differentiation variable")?;
            bail!(
                "{var} has no value here; give it one for a numeric derivative, or end the line with → for the derivative as a formula"
            );
        }
        let (_, d) = self.symbolic_derivative(s, args, order)?;
        Ok(d)
    }

    /// First or second numeric derivative of `args[0]` at `x0`, by central differences.
    fn numeric_derivative(
        &mut self,
        s: &mut Scope,
        args: &[Expr],
        var: &str,
        order: i64,
        x0: Quantity,
    ) -> Result<Value> {
        let prev = s.get(var).cloned();
        let mut fdim = None;
        let scale = x0.v.abs().max(1.0);
        let r: Result<f64> = (|| {
            let at = |t: f64| Quantity::new(t, x0.dim);
            if order == 1 {
                let h = 6e-6 * scale;
                let fp = self.sample(s, arg(args, 0)?, var, at(x0.v + h), &mut fdim)?;
                let fm = self.sample(s, arg(args, 0)?, var, at(x0.v - h), &mut fdim)?;
                Ok((fp - fm) / (2.0 * h))
            } else {
                let h = 1e-4 * scale;
                let fp = self.sample(s, arg(args, 0)?, var, at(x0.v + h), &mut fdim)?;
                let f0 = self.sample(s, arg(args, 0)?, var, at(x0.v), &mut fdim)?;
                let fm = self.sample(s, arg(args, 0)?, var, at(x0.v - h), &mut fdim)?;
                Ok((fp - 2.0 * f0 + fm) / (h * h))
            }
        })();
        s.restore(var, prev);
        let v = r?;
        Ok(Value::Num(Quantity::new(
            v,
            builtins::dim_div(fdim.unwrap_or(Dim::NONE), dim_pow(x0.dim, order as i32)?)?,
        )))
    }

    /// A derivative of order above two at a point: worked out as a formula, then evaluated
    /// there. Values with units are not supported this way yet.
    fn derivative_at(&mut self, s: &mut Scope, args: &[Expr], order: i64, x0: Quantity) -> Result<Value> {
        if order < 1 {
            bail!("only first and second numeric derivatives are supported");
        }
        if !x0.dim.is_dimensionless() {
            bail!("a derivative above the second order is not supported yet for a value with units");
        }
        let (var, d) = self.symbolic_derivative(s, args, order)?;
        crate::symbolic::bridge::at_point(&d, &var, x0.v)
    }

    /// `diff` worked out as a formula. The variable is a symbol while the expression is
    /// evaluated; a numbered element `x[k]` is a symbol of its own name. Returns the symbol
    /// and the derivative.
    fn symbolic_derivative(&mut self, s: &mut Scope, args: &[Expr], order: i64) -> Result<(String, Value)> {
        const MAX_ORDER: i64 = 12;
        if !(1..=MAX_ORDER).contains(&order) {
            bail!("a derivative of order {order} is not supported; the order must be from 1 to {MAX_ORDER}");
        }
        let (var, bound) = match arg(args, 1)? {
            Expr::Ident(name) => (name.clone(), true),
            Expr::Index(base, idx) => {
                let Expr::Ident(name) = base.as_ref() else {
                    bail!("the differentiation variable must be a name, like x")
                };
                let ix = idx
                    .iter()
                    .map(|i| self.eval(s, i).and_then(|v| crate::symbolic::bridge::index_expr(&v)))
                    .collect::<Result<Vec<_>>>()?;
                (crate::symbolic::bridge::element_name(name, &ix), false)
            }
            _ => bail!("the differentiation variable must be a name, like x"),
        };
        let prev = if bound { Some(s.get(&var).cloned()) } else { None };
        if bound {
            s.bind(&var, Value::Sym(Expr::Ident(var.clone())));
        }
        let v = self.eval(s, arg(args, 0)?);
        if let Some(prev) = prev {
            s.restore(&var, prev);
        }
        let d = crate::symbolic::bridge::diff_value(&v?, &var, order as u32)?;
        Ok((var, d))
    }
}

impl Interp<'_> {
    /// Variables first, then constants, then units: a plain `m` means metres unless
    /// a variable `m` is defined at this point (Mathcad's rule).
    #[inline(never)]
    fn lookup(&mut self, s: &Scope, name: &str) -> Result<Value> {
        match s.get(name) {
            Some(Binding::Val(v)) => {
                // naming a variable like a unit (m, s) is normal; hiding a constant is worth a note
                if constant(name).is_some() && !matches!(name, "π" | "pi" | "e" | "∞") {
                    self.shadowing.insert(name.to_string());
                }
                Ok(v.clone())
            }
            Some(Binding::Failed(_)) => bail!("{name} has no value (its definition has an error)"),
            Some(Binding::Func(_)) => bail!("{name} is a function; give it its arguments"),
            None => {
                let predefined = matches!(name, "π" | "pi" | "e" | "∞");
                // SMath: a name with no value is a symbol, worked out with later
                if self.marked_units && !predefined {
                    return Ok(Value::Sym(Expr::Ident(name.to_string())));
                }
                if let Some(c) = constant(name) {
                    if !predefined {
                        self.constant_idents.insert(name.to_string());
                    }
                    return Ok(Value::Num(Quantity::new(c.value, c.dim())));
                }
                match self.unit_named(name) {
                    Some(u) => {
                        self.unit_idents.insert(name.to_string());
                        Ok(Value::Num(Quantity::new(u.factor, u.dim)))
                    }
                    None => bail!("{name} is not defined"),
                }
            }
        }
    }
}

/// `v[i] := x` / `M[i, j] := x`, growing the matrix (filled with 0) as needed.
pub fn set_element(current: Option<Value>, idx: &[i64], v: Value) -> Result<Value> {
    let (r, c) = match *idx {
        [i] => (i, 1),
        [i, j] => (i, j),
        _ => bail!("assignment needs one or two indices"),
    };
    if r < 1 || c < 1 || r > 100_000 || c > 100_000 {
        bail!("index ({r}, {c}) is out of range (indices start at 1)");
    }
    let (r, c) = (r as usize, c as usize);
    let mut m = match current {
        Some(Value::Matrix(m)) => m,
        Some(other) => Matrix::new(1, 1, vec![other]),
        None => Matrix::new(0, 0, vec![]),
    };
    if r > m.rows || c > m.cols {
        let (rows, cols) = (r.max(m.rows), c.max(m.cols));
        // `M[1000, 1000] := x` is fine
        budget::check_size(rows, cols)?;
        if cols == m.cols || m.rows == 0 {
            // new rows only: append (amortized, so filling a column in a loop stays linear)
            let mut data = std::mem::take(&mut m.data);
            data.resize(rows * cols, Value::num(0.0));
            m = Matrix::new(rows, cols, data);
        } else {
            let mut data = vec![Value::num(0.0); rows * cols];
            for i in 0..m.rows {
                for j in 0..m.cols {
                    *data.get_mut(i * cols + j).ok_or_else(|| internal("a matrix element"))? =
                        m.at(i, j)?.clone();
                }
            }
            m = Matrix::new(rows, cols, data);
        }
    }
    let cols = m.cols;
    *m.data.get_mut((r - 1) * cols + (c - 1)).ok_or_else(|| internal("a matrix element"))? = v;
    Ok(Value::Matrix(m))
}
