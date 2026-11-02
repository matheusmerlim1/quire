//! Tables: each cell is text, a value (`12 kN`, `1.35`, `15%`) or a spreadsheet formula
//! (`=B2*C2`, `=SUM(D2:D4)`, `=IF(E5<=F5, "OK", "FAIL")`).
//!
//! Formulas follow spreadsheet conventions (A1 references, ranges, `=`/`<>` comparisons,
//! `&` joins text, `,` or `;` between arguments, function names in any case) and reach
//! into the worksheet: document variables (`=M.max`), units (`=B2 + 5 kN`), your own
//! functions and Quire's built-ins. A named table is a matrix variable for the math below
//! it, and other tables' cells are `name!B2`.

mod formula;
mod functions;

use std::collections::BTreeSet;

use quire_ast::{BinaryOp, CmpOp, Expr, parse};
use quire_units::UnitRegistry;

use crate::error::{EvalError, Result, bail};
use crate::format::{DisplaySettings, DisplaySpec, WorkingUnit, format_value_in};
use crate::interp::{Binding, Interp, Scope};
use crate::ops::{arith, compare, negate};
use crate::value::{Matrix, Quantity, Value};

/// A table region, in document order among the math regions.
#[derive(Debug, Clone, Default)]
pub struct TableInput {
    /// The variable the table defines for the math below it.
    pub name: Option<String>,
    /// Raw cell text, row-major; short rows count as blank cells.
    pub rows: Vec<Vec<String>>,
    /// Per-column display (unit, decimals); overrides `display`.
    pub columns: Vec<DisplaySpec>,
    pub display: DisplaySpec,
}

/// What one cell shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CellOutcome {
    /// The value in linear syntax (`16.2 'kN`, `"OK"`); `None` for text and blank cells.
    pub shown: Option<String>,
    pub error: Option<String>,
    /// The cell holds a formula (its text starts with `=`).
    pub formula: bool,
    /// The number behind `shown`, for showing it again in other units.
    pub value: Option<Value>,
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

/// `B12` / `$B$12` → (row, col), 0-based.
pub fn parse_ref(s: &str) -> Option<(usize, usize)> {
    let s = s.replace('$', "");
    let letters = s.chars().take_while(|c| c.is_ascii_uppercase()).count();
    if letters == 0 || letters > 3 || letters == s.len() {
        return None;
    }
    let (col, row) = s.split_at(letters);
    if !row.chars().all(|c| c.is_ascii_digit()) || row.starts_with('0') {
        return None;
    }
    let col = col.chars().fold(0usize, |n, c| n * 26 + (c as usize - 'A' as usize + 1));
    Some((row.parse::<usize>().ok()? - 1, col - 1))
}

/// 0-based column → `A`, `B`, … `AA`.
pub fn column_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push((b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.iter().rev().collect()
}

/// `B3` for row 2, column 1 (0-based).
pub fn cell_label(r: usize, c: usize) -> String {
    format!("{}{}", column_name(c), r + 1)
}

use formula::*;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A cell's value as formulas see it.
#[derive(Debug, Clone, PartialEq)]
enum CV {
    Blank,
    Bool(bool),
    V(Value),
}

impl CV {
    fn value(&self) -> Value {
        match self {
            CV::Blank => Value::num(0.0),
            CV::Bool(b) => Value::bool(*b),
            CV::V(v) => v.clone(),
        }
    }

    fn truthy(&self) -> Result<bool> {
        match self {
            CV::Blank => Ok(false),
            CV::Bool(b) => Ok(*b),
            CV::V(Value::Str(s)) => match s.to_ascii_uppercase().as_str() {
                "TRUE" => Ok(true),
                "FALSE" | "" => Ok(false),
                _ => bail!("\"{s}\" is not true or false"),
            },
            CV::V(v) => v.truthy(),
        }
    }

    fn text(&self, settings: &DisplaySettings, units: &UnitRegistry) -> String {
        match self {
            CV::Blank => String::new(),
            CV::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            CV::V(Value::Str(s)) => s.clone(),
            CV::V(v) => format_value_in(v, &DisplaySpec::default(), settings, units, &[])
                .unwrap_or_default()
                .replace('\'', "")
                .replace('*', "·"),
        }
    }
}

/// Every unit name in a value (`12 kN`, `9.81 m/s^2`).
fn unit_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Unit(n) | Expr::Ident(n) => out.push(n.clone()),
        Expr::Paren(x) | Expr::Unary(_, x) => unit_names(x, out),
        Expr::Binary(_, a, b) => {
            unit_names(a, out);
            unit_names(b, out);
        }
        _ => {}
    }
}

/// A plain cell that reads as a value: `12`, `-1,5`, `1.35`, `12 kN`, `9.81 m/s^2`, `15%`.
fn literal(text: &str, units: &UnitRegistry) -> Option<Expr> {
    let t = text.trim();
    if t.is_empty() || !t.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')) {
        return None;
    }
    // decimal comma (12,5 kN), as typed where it is the norm
    let t = match t.find(',') {
        Some(i)
            if t.get(..i).and_then(|head| head.chars().last()).is_some_and(|c| c.is_ascii_digit())
                && t.get(i + 1..).is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_digit()))
                && t.matches(',').count() == 1 =>
        {
            format!("{}.{}", t.get(..i).unwrap_or_default(), t.get(i + 1..).unwrap_or_default())
        }
        _ => t.to_string(),
    };
    if let Some(p) = t.strip_suffix('%') {
        let e = parse(p.trim()).ok().filter(plain_number)?;
        return Some(Expr::Binary(BinaryOp::Div, Box::new(e), Box::new(Expr::Num("100".into()))));
    }
    let e = parse(&t).ok()?;
    quantity_literal(&e, units).then_some(e)
}

fn plain_number(e: &Expr) -> bool {
    match e {
        Expr::Num(_) => true,
        Expr::Unary(_, x) => plain_number(x),
        _ => false,
    }
}

fn quantity_literal(e: &Expr, units: &UnitRegistry) -> bool {
    fn unit_part(e: &Expr, units: &UnitRegistry) -> bool {
        match e {
            Expr::Unit(_) => true,
            Expr::Ident(n) => units.lookup(n).is_some(),
            Expr::Paren(x) => unit_part(x, units),
            Expr::Binary(BinaryOp::Mul | BinaryOp::Div | BinaryOp::Juxt, a, b) => {
                unit_part(a, units) && unit_part(b, units)
            }
            Expr::Binary(BinaryOp::Pow, a, b) => unit_part(a, units) && plain_number(b),
            _ => false,
        }
    }
    match e {
        Expr::Num(_) => true,
        Expr::Unary(_, x) => quantity_literal(x, units),
        Expr::Binary(BinaryOp::Juxt | BinaryOp::Mul | BinaryOp::Div, a, b) => {
            quantity_literal(a, units) && unit_part(b, units)
        }
        _ => false,
    }
}

enum Cell {
    Blank,
    Text(String),
    Value(Expr),
    Formula(std::result::Result<F, String>),
}

#[derive(Clone)]
enum State {
    Todo,
    Busy,
    Done(std::result::Result<CV, String>),
}

/// One table being evaluated.
pub(crate) struct Sheet<'a, 'u, 's> {
    cells: Vec<Vec<Cell>>,
    state: Vec<Vec<State>>,
    interp: &'a mut Interp<'u>,
    scope: &'a mut Scope<'s>,
    settings: &'a DisplaySettings,
}

/// Evaluate a table: per-cell outcomes, the matrix it defines and the units it is written in
/// (in order, repeats included).
pub(crate) fn evaluate(
    t: &TableInput,
    interp: &mut Interp,
    scope: &mut Scope,
    settings: &DisplaySettings,
) -> (Vec<Vec<CellOutcome>>, Value, Vec<String>) {
    let nrows = t.rows.len();
    let ncols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut written = Vec::new();
    let cells: Vec<Vec<Cell>> = (0..nrows)
        .map(|r| {
            (0..ncols)
                .map(|c| {
                    let raw = t.rows.get(r).and_then(|row| row.get(c)).map(String::as_str).unwrap_or("");
                    let trimmed = raw.trim();
                    if let Some(f) = trimmed.strip_prefix('=') {
                        let parsed = parse_formula(f, (nrows, ncols), interp.units).map_err(|e| e.message);
                        if let Ok(f) = &parsed {
                            formula_units(f, &mut written);
                        }
                        Cell::Formula(parsed)
                    } else if trimmed.is_empty() {
                        Cell::Blank
                    } else if let Some(e) = literal(trimmed, interp.units) {
                        unit_names(&e, &mut written);
                        Cell::Value(e)
                    } else {
                        Cell::Text(raw.to_string())
                    }
                })
                .collect()
        })
        .collect();
    let mut sheet = Sheet { cells, state: vec![vec![State::Todo; ncols]; nrows], interp, scope, settings };
    let mut outcomes = Vec::with_capacity(nrows);
    let mut data = Vec::with_capacity(nrows * ncols);
    let vocab: Vec<WorkingUnit> = {
        let mut seen = BTreeSet::new();
        written
            .iter()
            .filter(|n| seen.insert(n.as_str()))
            .filter_map(|n| {
                let unit = sheet.interp.units.lookup(n)?;
                (unit.offset == 0.0 && !unit.dim.is_dimensionless())
                    .then(|| WorkingUnit { name: n.clone(), unit })
            })
            .collect()
    };
    for r in 0..nrows {
        let mut row = Vec::with_capacity(ncols);
        for c in 0..ncols {
            let formula = matches!(sheet.cells.get(r).and_then(|row| row.get(c)), Some(Cell::Formula(_)));
            let spec = cell_spec(t, c);
            let mut out = CellOutcome { formula, ..Default::default() };
            match sheet.cell(r, c) {
                Ok(v) => {
                    match &v {
                        CV::Blank => data.push(Value::Str(String::new())),
                        CV::Bool(b) => data.push(Value::bool(*b)),
                        CV::V(v) => data.push(v.clone()),
                    }
                    let text_cell = matches!(
                        sheet.cells.get(r).and_then(|row| row.get(c)),
                        Some(Cell::Text(_) | Cell::Blank)
                    );
                    if !text_cell {
                        out.shown = match &v {
                            CV::Blank => None,
                            CV::Bool(b) => Some(if *b { "\"TRUE\"" } else { "\"FALSE\"" }.into()),
                            CV::V(v) => {
                                // a value is shown in the units it was typed in, as a
                                // definition is; formulas follow the document's result units
                                let typed = typed_units(t, r, c, sheet.interp.units);
                                let as_typed = DisplaySettings { working_units: true, ..settings.clone() };
                                let (settings, vocab) = match &typed {
                                    Some(units) => (&as_typed, units.as_slice()),
                                    None => (settings, vocab.as_slice()),
                                };
                                // a unit set on the column that doesn't fit (a text column, a
                                // count) is ignored rather than turning the cell into an error
                                let shown = format_value_in(v, &spec, settings, sheet.interp.units, vocab)
                                    .or_else(|_| {
                                        let plain = DisplaySpec { unit: None, ..spec.clone() };
                                        format_value_in(v, &plain, settings, sheet.interp.units, vocab)
                                    });
                                out.value = Some(v.clone());
                                match shown {
                                    Ok(s) => Some(s),
                                    Err(e) => {
                                        out.error = Some(e.message);
                                        None
                                    }
                                }
                            }
                        };
                    }
                }
                Err(e) => {
                    data.push(Value::Str("#ERROR".into()));
                    out.error = Some(e);
                }
            }
            row.push(out);
        }
        outcomes.push(row);
    }
    (outcomes, Value::Matrix(Matrix::new(nrows, ncols, data)), written)
}

/// The units cell (r, c) was typed in when it holds a value (`4.5 kN/m` → kN, m);
/// `None` for formulas, text and blanks.
fn typed_units(t: &TableInput, r: usize, c: usize, units: &UnitRegistry) -> Option<Vec<WorkingUnit>> {
    let e = literal(t.rows.get(r)?.get(c)?, units)?;
    let mut names = Vec::new();
    unit_names(&e, &mut names);
    let mut out: Vec<WorkingUnit> = Vec::new();
    for name in names {
        if out.iter().any(|w| w.name == name) {
            continue;
        }
        if let Some(unit) = units.lookup(&name).filter(|u| u.offset == 0.0 && !u.dim.is_dimensionless()) {
            out.push(WorkingUnit { name, unit });
        }
    }
    Some(out)
}

/// How column `c` is shown: its own unit and decimals, else the table's.
fn cell_spec(t: &TableInput, c: usize) -> DisplaySpec {
    DisplaySpec {
        unit: t.columns.get(c).and_then(|s| s.unit.clone()).or_else(|| t.display.unit.clone()),
        decimals: t.columns.get(c).and_then(|s| s.decimals).or(t.display.decimals),
    }
}

/// Show the numbers again with a fuller list of preferred units (the document's).
pub(crate) fn reformat(
    t: &TableInput,
    cells: &mut [Vec<CellOutcome>],
    settings: &DisplaySettings,
    units: &UnitRegistry,
    vocab: &[WorkingUnit],
) {
    for (r, row) in cells.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            let spec = cell_spec(t, c);
            // values stay as typed
            if spec.unit.is_some() || cell.error.is_some() || typed_units(t, r, c, units).is_some() {
                continue;
            }
            if let Some(v @ Value::Num(_)) = &cell.value
                && let Ok(s) = format_value_in(v, &spec, settings, units, vocab)
            {
                cell.shown = Some(s);
            }
        }
    }
}

/// The worksheet names a table's formulas read (`=M.max`, `=σ/f.y`).
pub(crate) fn reads(t: &TableInput, units: &UnitRegistry) -> Vec<String> {
    fn walk(f: &F, out: &mut Vec<String>) {
        match f {
            F::Quire(Expr::Ident(n)) => out.push(n.clone()),
            F::Ext(n, ..) => out.push(n.clone()),
            F::Neg(x) | F::Percent(x) => walk(x, out),
            F::Bin(_, a, b) => {
                walk(a, out);
                walk(b, out);
            }
            F::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
            _ => {}
        }
    }
    let size = (t.rows.len(), t.rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut out = Vec::new();
    for cell in t.rows.iter().flatten() {
        if let Some(src) = cell.trim().strip_prefix('=')
            && let Ok(f) = parse_formula(src, size, units)
        {
            walk(&f, &mut out);
        }
    }
    out
}

/// The rows and columns of the range from `a` to `b` that lie inside a table of `size`
/// (rows, columns). Cells past the table are blank, so `SUM(A2:ZZZ999999)` reads only the
/// table instead of walking (and allocating) a billion blanks. `None` when nothing is inside.
fn clip(
    a: (usize, usize),
    b: (usize, usize),
    size: (usize, usize),
) -> Option<(std::ops::RangeInclusive<usize>, std::ops::RangeInclusive<usize>)> {
    let (r0, r1) = (a.0.min(b.0), a.0.max(b.0).min(size.0.checked_sub(1)?));
    let (c0, c1) = (a.1.min(b.1), a.1.max(b.1).min(size.1.checked_sub(1)?));
    (r0 <= r1 && c0 <= c1).then_some((r0..=r1, c0..=c1))
}

impl Sheet<'_, '_, '_> {
    fn cell(&mut self, r: usize, c: usize) -> std::result::Result<CV, String> {
        if r >= self.cells.len() || c >= self.cells.first().map_or(0, Vec::len) {
            return Ok(CV::Blank);
        }
        match self.state.get(r).and_then(|row| row.get(c)) {
            Some(State::Done(v)) => return v.clone(),
            Some(State::Busy) => return Err(format!("circular reference through {}", cell_label(r, c))),
            Some(State::Todo) => {}
            None => return Err("internal error: a cell outside the sheet".into()),
        }
        *self.state_mut(r, c)? = State::Busy;
        let v = match self.cells.get(r).and_then(|row| row.get(c)) {
            None | Some(Cell::Blank) => Ok(CV::Blank),
            Some(Cell::Text(t)) => Ok(CV::V(Value::Str(t.clone()))),
            Some(Cell::Value(e)) => {
                let e = e.clone();
                self.interp.eval(self.scope, &e).map(CV::V).map_err(|e| e.message)
            }
            Some(Cell::Formula(Err(e))) => Err(e.clone()),
            Some(Cell::Formula(Ok(f))) => {
                let f = f.clone();
                self.eval(&f).map_err(|e| e.message)
            }
        };
        *self.state_mut(r, c)? = State::Done(v.clone());
        v
    }

    /// The state of cell (r, c), for writing. The sheet's grids have the same shape.
    fn state_mut(&mut self, r: usize, c: usize) -> std::result::Result<&mut State, String> {
        self.state
            .get_mut(r)
            .and_then(|row| row.get_mut(c))
            .ok_or_else(|| "internal error: a cell outside the sheet".to_string())
    }

    /// A referenced cell: its error becomes the reader's, named.
    fn get(&mut self, r: usize, c: usize) -> Result<CV> {
        self.cell(r, c).map_err(|e| {
            if e.starts_with("circular reference") {
                EvalError::new(e)
            } else {
                EvalError::new(format!("{} has an error", cell_label(r, c)))
            }
        })
    }

    fn range(&mut self, a: (usize, usize), b: (usize, usize)) -> Result<Vec<Vec<CV>>> {
        let size = (self.cells.len(), self.cells.first().map_or(0, Vec::len));
        let Some((rows, cols)) = clip(a, b, size) else { return Ok(Vec::new()) };
        rows.map(|r| cols.clone().map(|c| self.get(r, c)).collect()).collect()
    }

    fn external(&mut self, table: &str, a: (usize, usize), b: Option<(usize, usize)>) -> Result<F> {
        let m = match self.scope.get(table) {
            Some(Binding::Val(Value::Matrix(m))) => m.clone(),
            Some(Binding::Val(_)) => bail!("{table} is not a table"),
            Some(Binding::Failed(_)) => bail!("{table} has an error"),
            _ => bail!("there is no table named {table} above this one"),
        };
        let at = |r: usize, c: usize| -> CV {
            if r >= m.rows || c >= m.cols {
                return CV::Blank;
            }
            let Some(v) = m.get(r, c) else { return CV::Blank };
            match v {
                Value::Str(s) if s.is_empty() => CV::Blank,
                Value::Str(s) if s == "#ERROR" => CV::V(Value::Str(s.clone())),
                v => CV::V(v.clone()),
            }
        };
        let b = b.unwrap_or(a);
        let rows: Vec<Vec<CV>> = match clip(a, b, (m.rows, m.cols)) {
            Some((rows, cols)) => rows.map(|r| cols.clone().map(|c| at(r, c)).collect()).collect(),
            // a single cell outside the table reads as blank, as inside it
            None if a == b => vec![vec![CV::Blank]],
            None => Vec::new(),
        };
        Ok(F::Lit(rows))
    }

    fn eval(&mut self, f: &F) -> Result<CV> {
        match self.eval_any(f)? {
            Got::One(v) => Ok(v),
            Got::Many(rows) => {
                if let [row] = rows.as_slice()
                    && let [v] = row.as_slice()
                {
                    return Ok(v.clone());
                }
                bail!("a cell holds one value; use SUM, MAX, … to combine a range")
            }
        }
    }

    fn eval_any(&mut self, f: &F) -> Result<Got> {
        Ok(match f {
            F::Num(x) => Got::One(CV::V(Value::num(*x))),
            F::Str(s) if s.is_empty() => Got::One(CV::Blank),
            F::Str(s) => Got::One(CV::V(Value::Str(s.clone()))),
            F::Bool(b) => Got::One(CV::Bool(*b)),
            F::Quire(e) => Got::One(CV::V(self.interp.eval(self.scope, e)?)),
            F::Cell(r, c) => Got::One(self.get(*r, *c)?),
            F::Range(a, b) => Got::Many(self.range(*a, *b)?),
            F::Ext(t, a, b) => {
                let read = self.external(t, *a, *b)?;
                return self.eval_any(&read);
            }
            F::Lit(rows) => Got::Many(rows.clone()),
            F::Neg(x) => Got::One(match self.eval(x)? {
                CV::Blank => CV::Blank,
                v => CV::V(negate(&v.value())?),
            }),
            F::Percent(x) => {
                let v = self.eval(x)?.value();
                Got::One(CV::V(arith(BinaryOp::Div, &v, &Value::num(100.0))?))
            }
            F::Bin(op, a, b) => {
                let x = self.eval(a)?;
                let y = self.eval(b)?;
                Got::One(self.binary(*op, x, y)?)
            }
            F::Call(name, args) => Got::One(self.call(name, args)?),
        })
    }

    fn binary(&mut self, op: Op, x: CV, y: CV) -> Result<CV> {
        match op {
            Op::Concat => Ok(CV::V(Value::Str(
                x.text(self.settings, self.interp.units) + &y.text(self.settings, self.interp.units),
            ))),
            Op::Cmp(op) => Ok(CV::Bool(cmp(op, &x, &y)?)),
            Op::Arith(op) => {
                // a blank cell is nothing to add, and zero otherwise (with the other side's units)
                match (op, &x, &y) {
                    (BinaryOp::Add, CV::Blank, v) | (BinaryOp::Add | BinaryOp::Sub, v, CV::Blank) => {
                        return Ok(v.clone());
                    }
                    (BinaryOp::Sub, CV::Blank, v) => return Ok(CV::V(negate(&v.value())?)),
                    _ => {}
                }
                for v in [&x, &y] {
                    if let CV::V(Value::Str(s)) = v {
                        bail!("\"{s}\" is text, not a number");
                    }
                }
                Ok(CV::V(arith(op, &x.value(), &y.value())?))
            }
        }
    }
}

enum Got {
    One(CV),
    Many(Vec<Vec<CV>>),
}

fn cmp(op: CmpOp, x: &CV, y: &CV) -> Result<bool> {
    let as_text = |v: &CV| match v {
        CV::V(Value::Str(s)) => Some(s.to_lowercase()),
        CV::Blank => Some(String::new()),
        _ => None,
    };
    // text compares without regard to case; a blank equals "" and 0
    if let (Some(a), Some(b)) = (as_text(x), as_text(y))
        && (matches!(x, CV::V(Value::Str(_))) || matches!(y, CV::V(Value::Str(_))))
    {
        return Ok(match op {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
        });
    }
    let num = |v: &CV| -> Result<Value> {
        match v {
            CV::V(Value::Str(s)) => bail!("cannot compare the text \"{s}\" with a number"),
            CV::Blank => Ok(Value::num(0.0)),
            other => Ok(other.value()),
        }
    };
    let (a, b) = (num(x)?, num(y)?);
    // a blank is zero in whatever units the other side has
    let zero_like = |z: &CV, other: &Value| match (z, other) {
        (CV::Blank, Value::Num(q)) => Value::Num(Quantity::new(0.0, q.dim)),
        _ => num(z).unwrap_or(Value::num(0.0)),
    };
    let a = if matches!(x, CV::Blank) { zero_like(x, &b) } else { a };
    let b = if matches!(y, CV::Blank) { zero_like(y, &a) } else { b };
    compare(op, &a, &b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Env;

    fn table(rows: &[&[&str]]) -> Vec<Vec<CellOutcome>> {
        let units = UnitRegistry::new();
        let env = Env::new();
        let mut interp = Interp::new(&units);
        let mut scope = Scope::new(&env);
        let t = TableInput {
            rows: rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect(),
            ..Default::default()
        };
        let settings = DisplaySettings { working_units: true, ..Default::default() };
        evaluate(&t, &mut interp, &mut scope, &settings).0
    }

    fn shown(rows: &[&[&str]]) -> Vec<Vec<String>> {
        table(rows)
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|c| c.error.map(|e| format!("!{e}")).or(c.shown).unwrap_or_default())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn references() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("$B$12"), Some((11, 1)));
        assert_eq!(parse_ref("AA3"), Some((2, 26)));
        assert_eq!(parse_ref("A0"), None);
        assert_eq!(parse_ref("a1"), None);
        assert_eq!(parse_ref("ABCD1"), None);
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
    }

    #[test]
    fn values_and_formulas() {
        let r = shown(&[
            &["Load", "Value", "Factor", "Design"],
            &["Dead", "12 kN", "1.35", "=B2*C2"],
            &["Live", "8 kN", "1,5", "=B3*C3"],
            &["Total", "=SUM(B2:B3)", "", "=sum(D2:D3)"],
            &["Check", "=IF(D4<=40 kN, \"OK\", \"FAIL\")", "=D4>B4", "=ROUND(C2*10%, 3)"],
        ]);
        assert_eq!(r[0], ["", "", "", ""], "text cells show their text, not a value");
        assert_eq!(r[1], ["", "12 'kN", "1.35", "16.2 'kN"]);
        assert_eq!(r[2][2], "1.5");
        assert_eq!(r[3][1], "20 'kN");
        assert_eq!(r[3][3], "28.2 'kN");
        assert_eq!(r[4][1], "\"OK\"");
        assert_eq!(r[4][2], "\"TRUE\"");
        assert_eq!(r[4][3], "0.135");
    }

    #[test]
    fn spreadsheet_functions() {
        let r = shown(&[
            &["1", "2", "3", "=AVERAGE(A1:C1)", "=MAX(A1:C1; 10)", "=COUNT(A1:C2)"],
            &["x", "", "4", "=COUNTA(A2:C2)", "=SUMIF(A1:C1, \">1\")", "=COUNTIF(A1:C2, \"x\")"],
            &[
                "=VLOOKUP(2, A1:C1, 1, FALSE)",
                "=INDEX(A1:C1, 3)",
                "=MATCH(3, A1:C1, 0)",
                "=AND(A1, C2>3)",
                "=IFERROR(1/0, \"n/a\")",
                "=\"a\" & 1+1",
            ],
            &[
                "=SUMPRODUCT(A1:C1, A1:C1)",
                "=MEDIAN(A1:C1, 7)",
                "=CEILING(2.1, 0.5)",
                "=MOD(7, 3)",
                "=SQRT(16)",
                "=CHOOSE(2, \"a\", \"b\")",
            ],
        ]);
        assert_eq!(r[0][3..], ["2", "10", "4"]);
        assert_eq!(r[1][3..], ["2", "5", "1"]);
        assert_eq!(r[2][0], "!VLOOKUP: 2 is not in the table");
        assert_eq!(r[2][1..], ["3", "3", "\"TRUE\"", "\"n/a\"", "\"a2\""]);
        assert_eq!(r[3], ["14", "2.5", "2.5", "1", "4", "\"b\""]);
    }

    #[test]
    fn errors() {
        let r = shown(&[&["=A1+1", "=B1", "=A2+1"], &["=SUM(", "=1+\"x\"", "=nosuch(1)"]]);
        assert_eq!(r[0][0], "!circular reference through A1");
        assert_eq!(r[0][2], "!A2 has an error");
        assert!(r[1][0].starts_with('!'));
        assert_eq!(r[1][1], "!\"x\" is text, not a number");
        assert_eq!(r[1][2], "!nosuch is not a known function");
    }
}
