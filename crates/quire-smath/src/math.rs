//! SMath RPN token stream -> AST -> Quire linear syntax.

use crate::{ImportError, Result};

/// Precedence of the unary prefix operators (`-x`).
const UNARY_PREC: u8 = 6;
/// Precedence of the postfix operators (`n!`).
const POSTFIX_PREC: u8 = 8;
/// Precedence of atoms: operands, parenthesised groups, calls.
const ATOM_PREC: u8 = 10;
/// Largest matrix dimension accepted, so a malformed `mat` cannot exhaust memory.
const MAX_DIM: usize = 1 << 20;
/// Deepest AST accepted (a leaf is depth 1). Printing, precedence, unit checks and drop
/// all recurse over the tree, so this keeps them within a small stack on hostile input.
const MAX_DEPTH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Num,
    Id,
    Unit,
    Str,
    Paren,
    Unary,
    Binary,
    Postfix,
    Call,
}

/// One AST node, as `Node` in the Python importer: `value` is the operand text,
/// operator or function name; `args` its operands.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Node {
    pub kind: Kind,
    pub value: String,
    pub args: Vec<Node>,
}

/// One `<e>` element of an `<input>`/`<result>`/`<contract>` container.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    /// `type` attribute: operand, operator, function, bracket.
    pub typ: String,
    /// Element text ("" when empty).
    pub text: String,
    /// `style` attribute (unit, string), if any.
    pub style: Option<String>,
    /// `args` attribute, 0 when absent or unparsable.
    pub args: usize,
}

/// The `<e>` children of a container element.
pub(crate) fn tokens(container: roxmltree::Node) -> Vec<Token> {
    container
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "e")
        .map(|e| Token {
            typ: e.attribute("type").unwrap_or("").to_string(),
            text: e.text().unwrap_or("").to_string(),
            style: e.attribute("style").map(str::to_string),
            args: e.attribute("args").and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0),
        })
        .collect()
}

fn node(kind: Kind, value: impl Into<String>, args: Vec<Node>) -> Node {
    Node { kind, value: value.into(), args }
}

fn malformed(msg: impl std::fmt::Display) -> ImportError {
    ImportError::Math(format!("malformed RPN ({msg})"))
}

/// Python's `e.get("type")`/`e.text` reading of a number: `[0-9]+(\.[0-9]*)?([eE][+-]?[0-9]+)?`
/// matched against the whole string.
fn is_number(s: &str) -> bool {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
    }
    let (mantissa, exponent) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (s, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => digits(int) && frac.chars().all(|c| c.is_ascii_digit()),
        None => digits(mantissa),
    };
    let exponent_ok = match exponent {
        None => true,
        Some(e) => digits(e.strip_prefix(['+', '-']).unwrap_or(e)),
    };
    mantissa_ok && exponent_ok
}

fn operand(e: &Token) -> Node {
    let text = e.text.as_str();
    match e.style.as_deref() {
        Some("unit") if text.is_empty() => empty_unit(),
        Some("unit") => node(Kind::Unit, text, Vec::new()),
        Some("string") => node(Kind::Str, text, Vec::new()),
        _ if is_number(text) => node(Kind::Num, text, Vec::new()),
        _ if text == "." => node(Kind::Id, "\u{2026}", Vec::new()), // SMath filler dot in illustrative matrices
        _ => node(Kind::Id, text, Vec::new()),
    }
}

/// The SMath empty unit `''`: a unit node with an empty value.
fn empty_unit() -> Node {
    node(Kind::Unit, "", Vec::new())
}

fn is_empty_unit(n: &Node) -> bool {
    n.kind == Kind::Unit && n.value.is_empty()
}

/// Depth of a node built over children of the given depths (a leaf is depth 1).
/// Rejects any node deeper than `MAX_DEPTH`, which bounds every recursive walk over the tree.
fn nested_depth(children: impl Iterator<Item = usize>) -> Result<usize> {
    let depth = children.max().unwrap_or(0).saturating_add(1);
    if depth > MAX_DEPTH {
        return Err(ImportError::Math("expression nested too deeply".into()));
    }
    Ok(depth)
}

pub(crate) fn rpn_to_ast(tokens: &[Token]) -> Result<Node> {
    // Each entry carries its own depth so the check is O(1) per token.
    let mut stack: Vec<(Node, usize)> = Vec::new();
    for e in tokens {
        match e.typ.as_str() {
            "operand" => stack.push((operand(e), 1)),
            "bracket" => {
                let (inner, depth) =
                    stack.pop().ok_or_else(|| malformed("bracket with nothing to enclose"))?;
                let depth = nested_depth(std::iter::once(depth))?;
                stack.push((node(Kind::Paren, "", vec![inner]), depth));
            }
            typ @ ("operator" | "function") => {
                let n = e.args;
                let start = stack.len().checked_sub(n).ok_or_else(|| {
                    malformed(format!("{:?} needs {n} operands, {} on the stack", e.text, stack.len()))
                })?;
                let operands = stack.split_off(start);
                let depth = nested_depth(operands.iter().map(|(_, d)| *d))?;
                let args: Vec<Node> = operands.into_iter().map(|(a, _)| a).collect();
                let kind = if typ == "operator" {
                    match n {
                        1 if e.text == "!" => Kind::Postfix,
                        1 => Kind::Unary,
                        2 => Kind::Binary,
                        _ => Kind::Call,
                    }
                } else {
                    Kind::Call
                };
                stack.push((node(kind, e.text.as_str(), args), depth));
            }
            other => return Err(ImportError::Math(format!("unknown token type '{other}'"))),
        }
    }
    match stack.len() {
        1 => stack.pop().map(|(n, _)| n).ok_or_else(|| malformed("empty stack")),
        n => Err(malformed(format!("{n} items left"))),
    }
}

/// Precedence and printed operator of a binary operator. Unknown operators keep their
/// text at precedence 5, as the Python `BINARY.get(value, (value, 5))` default.
fn binary_op(value: &str) -> (&str, u8) {
    match value {
        ":" => (":=", 0),
        // SMath's ≡ is an equality test (`if(path ≡ 0, …)`); Quire's ≡ defines globally
        "≡" => ("==", 3),
        "|" => ("or", 1),
        "&" => ("and", 2),
        "=" => ("==", 3),
        // SMath's approximately-equal test; Quire's == already allows for rounding
        "≈" => ("==", 3),
        "≠" => ("≠", 3),
        "<" => ("<", 3),
        ">" => (">", 3),
        "≤" => ("≤", 3),
        "≥" => ("≥", 3),
        "+" => ("+", 4),
        "-" => ("-", 4),
        "±" => ("±", 4),
        "*" => ("*", 5),
        "/" => ("/", 5),
        "%" => ("%", 5),
        "^" => ("^", 7),
        _ => (value, 5),
    }
}

/// SMath (Russian-tradition) function names -> canonical Quire names.
fn rename(name: &str) -> &str {
    match name {
        "tg" => "tan",
        "ctg" => "cot",
        "sh" => "sinh",
        "ch" => "cosh",
        "th" => "tanh",
        "cth" => "coth",
        "arctg" => "atan",
        "arcctg" => "acot",
        "arcsin" => "asin",
        "arccos" => "acos",
        "arcsec" => "asec",
        "arccosec" => "acsc",
        "cosec" => "csc",
        "arsh" => "asinh",
        "arch" => "acosh",
        "arth" => "atanh",
        "arcth" => "acoth",
        "cosech" => "csch",
        "lg" => "log10",
        "nthroot" => "root",
        _ => name,
    }
}

fn prec(n: &Node) -> u8 {
    match n.kind {
        Kind::Binary => {
            if n.value == "*" && is_quantity(n) {
                return 5;
            }
            binary_op(&n.value).1
        }
        Kind::Unary => UNARY_PREC,
        Kind::Postfix => POSTFIX_PREC,
        Kind::Num if n.value.starts_with('-') => UNARY_PREC,
        _ => ATOM_PREC,
    }
}

fn is_unit_expr(n: &Node) -> bool {
    match n.kind {
        Kind::Unit => true,
        Kind::Binary if n.value == "*" || n.value == "/" => n.args.iter().all(is_unit_expr),
        Kind::Binary if n.value == "^" => match n.args.as_slice() {
            [base, exp, ..] => is_unit_expr(base) && matches!(exp.kind, Kind::Num | Kind::Unary),
            _ => false,
        },
        _ => false,
    }
}

/// `5.1 'm`, `64 'kgf*'m` -> printed as juxtaposition.
fn is_quantity(n: &Node) -> bool {
    n.kind == Kind::Binary
        && n.value == "*"
        && n.args.first().is_some_and(|a| a.kind == Kind::Num)
        && n.args.get(1).is_some_and(is_unit_expr)
}

/// SMath names may have primes (σ', τ'.xy, f''); in Quire ' starts a unit, so they are ′.
fn fmt_id(name: &str) -> String {
    name.replace('\'', "′")
}

/// Python `json.dumps(s, ensure_ascii=False)`.
fn fmt_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python `str.strip()` whitespace (includes the 0x1C-0x1F separators).
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn arg(n: &Node, i: usize) -> Result<&Node> {
    n.args.get(i).ok_or_else(|| ImportError::Math(format!("missing operand of '{}'", n.value)))
}

fn matrix_dim(s: &str) -> Result<usize> {
    let v: usize =
        s.parse().map_err(|_| ImportError::Math(format!("matrix dimension {s:?} is not an integer")))?;
    if v > MAX_DIM {
        return Err(ImportError::Math(format!("matrix dimension {v} too large")));
    }
    Ok(v)
}

/// Print AST as Quire linear syntax. `units_bare`: print `kgf` instead of `'kgf`.
/// A whole statement. At the top, SMath's `≡` is a global definition (`A ≡ [..]`), as in Quire;
/// inside an expression it is an equality test (`if(path ≡ 0, …)`, see `binary_op`).
pub(crate) fn statement_to_linear(n: &Node, units_bare: bool) -> Result<String> {
    if n.kind == Kind::Binary && n.value == "≡" {
        return Ok(format!(
            "{} ≡ {}",
            to_linear(arg(n, 0)?, units_bare)?,
            to_linear(arg(n, 1)?, units_bare)?
        ));
    }
    to_linear(n, units_bare)
}

pub(crate) fn to_linear(n: &Node, units_bare: bool) -> Result<String> {
    Ok(match n.kind {
        Kind::Num => n.value.clone(),
        Kind::Id => fmt_id(&n.value),
        Kind::Str => fmt_str(&n.value),
        Kind::Unit if units_bare => n.value.clone(),
        Kind::Unit => format!("'{}", n.value),
        Kind::Paren => format!("({})", to_linear(arg(n, 0)?, units_bare)?),
        Kind::Unary => {
            let a = arg(n, 0)?;
            let mut s = to_linear(a, units_bare)?;
            if prec(a) < UNARY_PREC {
                s = format!("({s})");
            }
            format!("{}{s}", n.value)
        }
        Kind::Postfix => {
            let a = arg(n, 0)?;
            let mut s = to_linear(a, units_bare)?;
            if prec(a) < POSTFIX_PREC {
                s = format!("({s})");
            }
            format!("{s}{}", n.value)
        }
        Kind::Binary => binary_to_linear(n, units_bare)?,
        Kind::Call => call_to_linear(n, units_bare)?,
    })
}

fn binary_to_linear(n: &Node, units_bare: bool) -> Result<String> {
    let (a, b) = match n.args.as_slice() {
        [a, b] => (a, b),
        _ => return Err(malformed(format!("'{}' needs 2 operands", n.value))),
    };
    // the SMath empty unit `''` is a dimensionless marker: `1 ''` -> `1`
    if n.value == "*" && is_empty_unit(b) {
        return to_linear(a, units_bare);
    }
    if n.value == "*" && is_empty_unit(a) {
        return to_linear(b, units_bare);
    }
    let (op, p) = binary_op(&n.value);
    let right_assoc = op == "^";
    let mut sa = to_linear(a, units_bare)?;
    let mut sb = to_linear(b, units_bare)?;
    if prec(a) < p || (right_assoc && prec(a) == p) {
        sa = format!("({sa})");
    }
    if prec(b) < p || (!right_assoc && prec(b) == p && op != "*" && op != "+") {
        sb = format!("({sb})");
    }
    if is_quantity(n) {
        return Ok(format!("{sa} {sb}"));
    }
    if matches!(op, "*" | "/" | "^") { Ok(format!("{sa}{op}{sb}")) } else { Ok(format!("{sa} {op} {sb}")) }
}

fn call_to_linear(n: &Node, units_bare: bool) -> Result<String> {
    let name = n.value.as_str();
    let args = n.args.as_slice();

    // mat(cells..., rows, cols)
    if let (true, [cell_args @ .., rows_node, cols_node]) = (name == "mat", args)
        && rows_node.kind == Kind::Num
        && cols_node.kind == Kind::Num
    {
        let r = matrix_dim(&rows_node.value)?;
        let c = matrix_dim(&cols_node.value)?;
        let cells = cell_args.iter().map(|a| to_linear(a, units_bare)).collect::<Result<Vec<_>>>()?;
        let rows: Vec<String> = (0..r)
            .map(|i| {
                let lo = i.saturating_mul(c).min(cells.len());
                let hi = i.saturating_add(1).saturating_mul(c).min(cells.len());
                format!("  {}", cells.get(lo..hi).unwrap_or(&[]).join(", "))
            })
            .collect();
        if r.checked_mul(c).is_some_and(|cells_total| cells_total <= 6) {
            let parts: Vec<&str> = rows.iter().map(|x| x.trim_matches(is_py_space)).collect();
            return Ok(format!("[{}]", parts.join("; ")));
        }
        return Ok(format!("[\n{}\n]", rows.join(";\n")));
    }

    // line(statements…, rows, cols): SMath's block of statements. Its last two arguments give
    // the block's layout, not a value, so the block is worth its last statement.
    if let ("line", [stmts @ .., rows_node, cols_node]) = (name, args)
        && rows_node.kind == Kind::Num
        && cols_node.kind == Kind::Num
        && !stmts.is_empty()
    {
        let parts = stmts.iter().map(|a| to_linear(a, units_bare)).collect::<Result<Vec<_>>>()?;
        return Ok(format!("block({})", parts.join(", ")));
    }

    // range(a, b, s): SMath's third argument is the second element, Quire's is the step
    if let ("range", [a, b, s]) = (name, args) {
        return range_call(a, b, s, units_bare);
    }

    // el(base, indices...)
    if name == "el" && args.len() >= 2 {
        let base_node = arg(n, 0)?;
        let mut base = to_linear(base_node, units_bare)?;
        if prec(base_node) < ATOM_PREC {
            base = format!("({base})");
        }
        let indices = args.iter().skip(1).map(|a| to_linear(a, units_bare)).collect::<Result<Vec<_>>>()?;
        return Ok(format!("{base}[{}]", indices.join(", ")));
    }

    let parts = args.iter().map(|a| to_linear(a, units_bare)).collect::<Result<Vec<_>>>()?;
    Ok(format!("{}({})", fmt_id(rename(name)), parts.join(", ")))
}

/// SMath `range(a, b, s)` runs from `a` through `b` with `s` as the second element, so the
/// step is `s - a` (`range(1, 10, 3)` is 1, 3, 5, 7, 9). Two whole numbers fold to a literal;
/// otherwise the subtraction is printed, parenthesised only where precedence needs it.
fn range_call(a: &Node, b: &Node, s: &Node, units_bare: bool) -> Result<String> {
    let folded = match (a.kind, s.kind) {
        (Kind::Num, Kind::Num) => match (a.value.parse::<i64>(), s.value.parse::<i64>()) {
            (Ok(av), Ok(sv)) => sv.checked_sub(av),
            _ => None,
        },
        _ => None,
    };
    let step = match folded {
        Some(d) => Node { kind: Kind::Num, value: d.to_string(), args: Vec::new() },
        None => Node { kind: Kind::Binary, value: "-".to_string(), args: vec![s.clone(), a.clone()] },
    };
    Ok(format!(
        "range({}, {}, {})",
        to_linear(a, units_bare)?,
        to_linear(b, units_bare)?,
        to_linear(&step, units_bare)?
    ))
}

/// Whether an SMath result holds only numbers in a `mat`. SMath labels `→` results
/// symbolic even when all it shows is a numeric matrix; Quire's symbolic engine cannot
/// work with matrices, so such a result is shown as its value.
pub(crate) fn is_numeric_matrix(container: roxmltree::Node) -> bool {
    let mut has_mat = false;
    for t in tokens(container) {
        match t.typ.as_str() {
            "operand" if t.style.is_none() && is_number(&t.text) => {}
            "operator" => {}
            "function" if t.text == "mat" => has_mat = true,
            _ => return false,
        }
    }
    has_mat
}

/// `None` when the container is absent or has no `<e>` tokens.
pub(crate) fn math_expr(container: Option<roxmltree::Node>) -> Result<Option<Node>> {
    let Some(c) = container else { return Ok(None) };
    let toks = tokens(c);
    if toks.is_empty() {
        return Ok(None);
    }
    rpn_to_ast(&toks).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(typ: &str, text: &str, style: Option<&str>, args: usize) -> Token {
        Token { typ: typ.to_string(), text: text.to_string(), style: style.map(str::to_string), args }
    }

    fn operand(text: &str) -> Token {
        tok("operand", text, None, 0)
    }

    fn unit(text: &str) -> Token {
        tok("operand", text, Some("unit"), 0)
    }

    fn op(text: &str, args: usize) -> Token {
        tok("operator", text, None, args)
    }

    fn func(text: &str, args: usize) -> Token {
        tok("function", text, None, args)
    }

    fn render(tokens: &[Token]) -> Result<String> {
        to_linear(&rpn_to_ast(tokens)?, false)
    }

    #[test]
    fn definition_with_unit() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // L := 3 'm
        let toks = [operand("L"), operand("3"), unit("m"), op("*", 2), op(":", 2)];
        assert_eq!(render(&toks)?, "L := 3 'm");
        assert_eq!(to_linear(&rpn_to_ast(&toks)?, true)?, "L := 3 m");
        Ok(())
    }

    #[test]
    fn primes_in_names_are_kept() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // σ' := τ'.xy + f''(x)
        let toks = [operand("σ'"), operand("τ'.xy"), operand("x"), func("f''", 1), op("+", 2), op(":", 2)];
        assert_eq!(render(&toks)?, "σ′ := τ′.xy + f′′(x)");
        Ok(())
    }

    #[test]
    fn precedence_parenthesises_sum() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // (a+b)*c
        let toks =
            [operand("a"), operand("b"), op("+", 2), tok("bracket", "", None, 0), operand("c"), op("*", 2)];
        assert_eq!(render(&toks)?, "(a + b)*c");
        // a*(b+c): the sum has lower precedence than `*`, so it is parenthesised
        let toks = [operand("a"), operand("b"), operand("c"), op("+", 2), op("*", 2)];
        assert_eq!(render(&toks)?, "a*(b + c)");
        Ok(())
    }

    #[test]
    fn unary_minus() -> std::result::Result<(), Box<dyn std::error::Error>> {
        assert_eq!(render(&[operand("x"), op("-", 1)])?, "-x");
        // -(x+y)*z: the unary operand is a sum, so it gets parentheses; `*` does not
        let toks = [operand("x"), operand("y"), op("+", 2), op("-", 1), operand("z"), op("*", 2)];
        assert_eq!(render(&toks)?, "-(x + y)*z");
        Ok(())
    }

    #[test]
    fn power_is_right_associative() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // a^(b^c)
        let toks = [operand("a"), operand("b"), operand("c"), op("^", 2), op("^", 2)];
        assert_eq!(render(&toks)?, "a^b^c");
        // (a^b)^c needs the parentheses
        let toks = [operand("a"), operand("b"), op("^", 2), operand("c"), op("^", 2)];
        assert_eq!(render(&toks)?, "(a^b)^c");
        Ok(())
    }

    #[test]
    fn function_rename() -> std::result::Result<(), Box<dyn std::error::Error>> {
        assert_eq!(render(&[operand("x"), func("tg", 1)])?, "tan(x)");
        assert_eq!(render(&[operand("x"), func("lg", 1)])?, "log10(x)");
        // SMath's hyperbolic inverses and cosecant, as the European names it stores
        assert_eq!(render(&[operand("x"), func("arsh", 1)])?, "asinh(x)");
        assert_eq!(render(&[operand("x"), func("arch", 1)])?, "acosh(x)");
        assert_eq!(render(&[operand("x"), func("arth", 1)])?, "atanh(x)");
        assert_eq!(render(&[operand("x"), func("arcth", 1)])?, "acoth(x)");
        assert_eq!(render(&[operand("x"), func("cosech", 1)])?, "csch(x)");
        Ok(())
    }

    #[test]
    fn matrix_two_by_two() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let toks = [
            operand("1"),
            operand("2"),
            operand("3"),
            operand("4"),
            operand("2"),
            operand("2"),
            func("mat", 6),
        ];
        assert_eq!(render(&toks)?, "[1, 2; 3, 4]");
        Ok(())
    }

    #[test]
    fn element_indexing() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // v[2]
        let toks = [operand("v"), operand("2"), func("el", 2)];
        assert_eq!(render(&toks)?, "v[2]");
        Ok(())
    }

    #[test]
    fn string_operand_is_escaped() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let toks = [tok("operand", "a\"b\n", Some("string"), 0)];
        assert_eq!(render(&toks)?, r#""a\"b\n""#);
        Ok(())
    }

    #[test]
    fn number_pattern() {
        assert!(is_number("3"));
        assert!(is_number("3."));
        assert!(is_number("1.5e-3"));
        assert!(is_number("2E+10"));
        assert!(!is_number(""));
        assert!(!is_number("."));
        assert!(!is_number("1e"));
        assert!(!is_number("1.2.3"));
        assert!(!is_number("L.A"));
    }

    #[test]
    fn malformed_rpn_is_an_error() {
        // operator with too few operands
        assert!(rpn_to_ast(&[operand("a"), op("+", 2)]).is_err());
        // operator with no operands at all
        assert!(rpn_to_ast(&[op("-", 1)]).is_err());
        // empty stack
        assert!(rpn_to_ast(&[]).is_err());
        // bracket around nothing
        assert!(rpn_to_ast(&[tok("bracket", "", None, 0)]).is_err());
        // leftover operands
        assert!(rpn_to_ast(&[operand("a"), operand("b")]).is_err());
        // unknown token type
        assert!(rpn_to_ast(&[tok("weird", "a", None, 0)]).is_err());
    }

    /// Runs `f` on a thread with a `stack_kib` KiB stack, proving it needs no deep stack.
    fn on_stack<T: Send + 'static>(
        stack_kib: usize,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> std::result::Result<T, Box<dyn std::error::Error>> {
        let handle = std::thread::Builder::new().stack_size(stack_kib * 1024).spawn(f)?;
        handle.join().map_err(|_| "small-stack test thread panicked".into())
    }

    /// `x` followed by `depth` unary minus operators: a tree `depth + 1` levels deep.
    fn unary_chain(depth: usize) -> Vec<Token> {
        let mut toks = vec![operand("x")];
        toks.extend(std::iter::repeat_with(|| op("-", 1)).take(depth));
        toks
    }

    #[test]
    fn deep_unary_chain_is_rejected_without_deep_stack() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let rejected = on_stack(256, || {
            matches!(
                rpn_to_ast(&unary_chain(100_000)),
                Err(ImportError::Math(ref m)) if m.contains("nested too deeply")
            )
        })?;
        assert!(rejected);
        Ok(())
    }

    #[test]
    fn deep_left_sum_chain_is_rejected_without_deep_stack()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let rejected = on_stack(256, || {
            // a+a+a+... is left-deep: each `+` nests the previous sum one level further
            let mut toks = vec![operand("a")];
            for _ in 0..100_000 {
                toks.push(operand("a"));
                toks.push(op("+", 2));
            }
            matches!(
                rpn_to_ast(&toks),
                Err(ImportError::Math(ref m)) if m.contains("nested too deeply")
            )
        })?;
        assert!(rejected);
        Ok(())
    }

    #[test]
    fn chain_at_depth_limit_converts() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // 199 minus signs over an operand is a 200-deep tree: the largest accepted
        // a debug-build conversion of 200 levels needs roughly 256-512 KiB of stack
        let out = on_stack(512, || rpn_to_ast(&unary_chain(199)).and_then(|n| to_linear(&n, false)).ok())?;
        assert_eq!(out, Some(format!("{}x", "-".repeat(199))));
        // one more level is rejected
        let over = on_stack(512, || rpn_to_ast(&unary_chain(200)).is_err())?;
        assert!(over);
        Ok(())
    }

    /// `range(a, b, s)` as SMath writes it, with `a`, `b`, `s` given as token lists.
    fn range_of(a: &[Token], b: &[Token], s: &[Token]) -> Result<String> {
        let mut toks = Vec::new();
        for part in [a, b, s] {
            toks.extend_from_slice(part);
        }
        toks.push(func("range", 3));
        render(&toks)
    }

    #[test]
    fn range_third_argument_is_the_second_element() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // range(1, 10, 3): step 2, since the sequence is 1, 3, 5, 7, 9
        assert_eq!(range_of(&[operand("1")], &[operand("10")], &[operand("3")])?, "range(1, 10, 2)");
        // counting down: range(n-1, 1, n-2) steps by -1
        let (n_1, n_2) = ([operand("n"), operand("1"), op("-", 2)], [operand("n"), operand("2"), op("-", 2)]);
        assert_eq!(range_of(&n_1, &[operand("1")], &n_2)?, "range(n - 1, 1, n - 2 - (n - 1))");
        // a symbolic second element is subtracted, with no parentheses needed
        assert_eq!(range_of(&[operand("2")], &[operand("n")], &[operand("k")])?, "range(2, n, k - 2)");
        // a sum as the second element keeps the sum's precedence
        let s_sum = [operand("k"), operand("1"), op("+", 2)];
        assert_eq!(range_of(&[operand("2")], &[operand("n")], &s_sum)?, "range(2, n, k + 1 - 2)");
        Ok(())
    }

    /// Whether `is_numeric_matrix` accepts the `<result>` built from `body`.
    fn numeric_matrix(body: &str) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        let xml = format!("<result>{body}</result>");
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        Ok(is_numeric_matrix(doc.root_element()))
    }

    #[test]
    fn numeric_matrix_result_is_a_value() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // [4/5; 1/5] written as fractions of numbers, as SMath shows it
        let fractions = r#"<e type="operand">4</e><e type="operand">5</e><e type="operator" args="2">/</e><e type="operand">1</e><e type="operand">5</e><e type="operator" args="2">/</e><e type="operand">2</e><e type="function" args="2">mat</e>"#;
        assert!(numeric_matrix(fractions)?);
        // a symbolic result keeps its symbols
        assert!(!numeric_matrix(r#"<e type="operand">x</e><e type="function" args="1">mat</e>"#)?);
        // a unit is not a plain number
        assert!(!numeric_matrix(
            r#"<e type="operand" style="unit">m</e><e type="function" args="1">mat</e>"#
        )?);
        // no matrix, no change
        assert!(!numeric_matrix(r#"<e type="operand">4</e>"#)?);
        Ok(())
    }
}
