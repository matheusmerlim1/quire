//! Precedence-climbing parser. Precedence, loosest first:
//!
//! `:=`/`≡` (right) · `or` · `and` · `not` · comparison chains · `..` · `+ - ±` ·
//! `* / %` · unary `- + ±` · `^` (right) · postfix `!` `[…]` · primary.
//!
//! A number directly followed by a unit (`25.5 'm^2`) is a single primary, so it
//! binds tighter than any operator; so is a name or parentheses followed by a quoted
//! unit (`T '°C`, `(-20) '°C`).
//!
//! Trees deeper than [`MAX_DEPTH`] are refused while they are built, so nothing that
//! walks a parsed tree (evaluation, printing, the symbolic engine, dropping it) can run
//! out of stack. One loop handles every binary level, which keeps the parser's own
//! recursion to a few calls per bracket.

use std::fmt;

use crate::ast::{BinaryOp, CmpOp, Expr, UnaryOp};
use crate::lexer::{Tok, Token, tokenize};

/// The deepest expression accepted, a leaf being depth 1: the SMath importer's limit too.
/// A long chain is deep as well (`a + b + c` is `(a + b) + c`), so this also bounds sums
/// and products of many terms.
pub const MAX_DEPTH: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    /// Character offset into the source.
    pub pos: usize,
}

impl ParseError {
    pub(crate) fn new(message: impl Into<String>, pos: usize) -> Self {
        ParseError { message: message.into(), pos }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.pos + 1)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// An expression and its depth.
type Tree = (Expr, usize);

pub fn parse(src: &str) -> Result<Expr> {
    let mut p = Parser { tokens: tokenize(src)?, i: 0, nest: 0 };
    let (e, _) = p.expr(DEFINE)?;
    match p.peek() {
        Tok::Eof => Ok(e),
        t => Err(p.error(format!("unexpected {}", describe(t)))),
    }
}

fn not_a_value(t: &Tok, pos: usize) -> ParseError {
    ParseError::new(format!("expected a value but found {}", describe(t)), pos)
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Num(n) => format!("number {n}"),
        Tok::Ident(n) => format!("name {n}"),
        Tok::Unit(u) => format!("unit '{u}"),
        Tok::Str(_) => "string".to_string(),
        Tok::Sym(s) => format!("'{s}'"),
        Tok::Eof => "end of expression".to_string(),
    }
}

// Binding levels, loosest first. An operator of level L takes a left operand of level L or
// tighter, and parses its right operand at the level above (at its own when right-associative).
const DEFINE: u8 = 1;
const OR: u8 = 2;
const AND: u8 = 3;
const NOT: u8 = 4;
const COMPARE: u8 = 5;
const RANGE: u8 = 6;
const ADD: u8 = 7;
const MUL: u8 = 8;
const UNARY: u8 = 9;
/// Powers, postfix operators and primaries.
const ATOM: u8 = 10;

fn compare_op(t: &Tok) -> Option<CmpOp> {
    Some(match t {
        Tok::Sym("==") => CmpOp::Eq,
        Tok::Sym("≠") => CmpOp::Ne,
        Tok::Sym("<") => CmpOp::Lt,
        Tok::Sym("≤") => CmpOp::Le,
        Tok::Sym(">") => CmpOp::Gt,
        Tok::Sym("≥") => CmpOp::Ge,
        _ => return None,
    })
}

/// Returned when the token list is exhausted. `tokenize` always ends with `Eof`,
/// so this is only a fallback that keeps the accessors total.
static EOF_TOK: Tok = Tok::Eof;

struct Parser {
    tokens: Vec<Token>,
    i: usize,
    /// Calls of [`Parser::expr`] in progress. Each one builds at least one level of the
    /// tree, so this stays within [`MAX_DEPTH`] for any tree that is accepted.
    nest: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.tokens.get(self.i).map_or(&EOF_TOK, |t| &t.tok)
    }

    fn peek2(&self) -> &Tok {
        self.tokens.get(self.i + 1).or_else(|| self.tokens.last()).map_or(&EOF_TOK, |t| &t.tok)
    }

    /// Character offset of the current token.
    fn pos(&self) -> usize {
        self.tokens.get(self.i).map_or(0, |t| t.pos)
    }

    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        if self.i + 1 < self.tokens.len() {
            self.i += 1;
        }
        t
    }

    fn eat(&mut self, sym: &str) -> bool {
        if matches!(self.peek(), Tok::Sym(s) if *s == sym) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, sym: &str) -> Result<()> {
        if self.eat(sym) {
            Ok(())
        } else {
            Err(self.error(format!("expected '{sym}' but found {}", describe(self.peek()))))
        }
    }

    fn error(&self, message: String) -> ParseError {
        ParseError::new(message, self.pos())
    }

    #[cold]
    #[inline(never)]
    fn too_deep(&self) -> ParseError {
        self.error(format!(
            "this expression is too long or too deeply nested (more than {MAX_DEPTH} levels of operators and brackets); split it into smaller definitions"
        ))
    }

    /// `e` as a node over children at most `below` deep.
    fn grow(&self, e: Expr, below: usize) -> Result<Tree> {
        let depth = below + 1;
        if depth > MAX_DEPTH {
            return Err(self.too_deep());
        }
        Ok((e, depth))
    }

    fn bin(&self, op: BinaryOp, (a, da): Tree, (b, db): Tree) -> Result<Tree> {
        self.grow(Expr::Binary(op, Box::new(a), Box::new(b)), da.max(db))
    }

    /// An expression whose operators all bind at `min` or tighter.
    fn expr(&mut self, min: u8) -> Result<Tree> {
        if self.nest >= MAX_DEPTH {
            return Err(self.too_deep());
        }
        self.nest += 1;
        let r = self.climb(min);
        self.nest -= 1;
        r
    }

    // The loop only picks the operator; building each node happens in a function of its own,
    // which keeps this frame, on the stack once per level of nesting, small.
    fn climb(&mut self, min: u8) -> Result<Tree> {
        let (mut lhs, mut level) = self.prefix(min)?;
        loop {
            let t = self.peek();
            let at = if compare_op(t).is_some() {
                COMPARE
            } else {
                match t {
                    Tok::Sym(":=" | "≡") => DEFINE,
                    Tok::Sym("or") => OR,
                    Tok::Sym("and") => AND,
                    Tok::Sym("..") => RANGE,
                    Tok::Sym("+" | "-" | "±") => ADD,
                    Tok::Sym("*" | "/" | "%") => MUL,
                    _ => break,
                }
            };
            // comparisons chain and ranges don't associate, so they take a tighter left side
            let takes = match at {
                COMPARE | RANGE => level > at,
                _ => level >= at,
            };
            if min > at || !takes {
                break;
            }
            lhs = match at {
                DEFINE => self.define(lhs)?,
                COMPARE => self.chain(lhs)?,
                RANGE => self.range(lhs)?,
                _ => self.binary(at, lhs)?,
            };
            level = at;
        }
        Ok(lhs)
    }

    /// `lhs := rhs`, right-associative: `a := b := c`.
    #[inline(never)]
    fn define(&mut self, (lhs, dl): Tree) -> Result<Tree> {
        let global = self.bump() == Tok::Sym("≡");
        let (rhs, dr) = self.expr(DEFINE)?;
        self.grow(Expr::Define { lhs: Box::new(lhs), rhs: Box::new(rhs), global }, dl.max(dr))
    }

    /// A comparison chain, `a < b ≤ c`, of operands that are ranges or tighter.
    #[inline(never)]
    fn chain(&mut self, (first, mut depth): Tree) -> Result<Tree> {
        let mut rest = Vec::new();
        while let Some(op) = compare_op(self.peek()) {
            self.bump();
            let (x, d) = self.expr(RANGE)?;
            depth = depth.max(d);
            rest.push((op, x));
        }
        self.grow(Expr::Compare(Box::new(first), rest), depth)
    }

    /// `a..b`: not associative, so `a..b..c` is an error.
    #[inline(never)]
    fn range(&mut self, (a, da): Tree) -> Result<Tree> {
        self.bump();
        let (b, db) = self.expr(ADD)?;
        self.grow(Expr::Range(Box::new(a), Box::new(b)), da.max(db))
    }

    /// A left-associative operator of level `at`: its right operand binds tighter
    /// (`and` takes `not b`, `*` takes `-b`).
    #[inline(never)]
    fn binary(&mut self, at: u8, lhs: Tree) -> Result<Tree> {
        let op = match self.bump() {
            Tok::Sym("or") => BinaryOp::Or,
            Tok::Sym("and") => BinaryOp::And,
            Tok::Sym("-") => BinaryOp::Sub,
            Tok::Sym("±") => BinaryOp::PlusMinus,
            Tok::Sym("*") => BinaryOp::Mul,
            Tok::Sym("/") => BinaryOp::Div,
            Tok::Sym("%") => BinaryOp::Mod,
            _ => BinaryOp::Add,
        };
        let rhs = self.expr(at + 1)?;
        self.bin(op, lhs, rhs)
    }

    /// A prefix operator and its operand, or a power. Returns the level it binds at.
    fn prefix(&mut self, min: u8) -> Result<(Tree, u8)> {
        let op = match self.peek() {
            // `not` binds looser than comparisons: `not a < b` is `not (a < b)`
            Tok::Sym("not") if min <= NOT => {
                self.bump();
                let (x, d) = self.expr(NOT)?;
                return Ok((self.grow(Expr::Unary(UnaryOp::Not, Box::new(x)), d)?, NOT));
            }
            Tok::Sym("-") => UnaryOp::Neg,
            Tok::Sym("+") => UnaryOp::Pos,
            Tok::Sym("±") => UnaryOp::PlusMinus,
            _ => return Ok((self.power()?, ATOM)),
        };
        self.bump();
        let (x, d) = self.expr(UNARY)?;
        Ok((self.grow(Expr::Unary(op, Box::new(x)), d)?, UNARY))
    }

    fn power(&mut self) -> Result<Tree> {
        let base = self.postfix()?;
        if self.eat("^") {
            // right-associative, and the exponent may have a sign: `2^-1`, `a^b^c`
            let exponent = self.expr(UNARY)?;
            return self.bin(BinaryOp::Pow, base, exponent);
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Tree> {
        let (mut e, mut depth) = self.primary()?;
        loop {
            if self.eat("!") {
                (e, depth) = self.grow(Expr::Factorial(Box::new(e)), depth)?;
            } else if self.eat("[") {
                let (idx, d) = self.list("]")?;
                (e, depth) = self.grow(Expr::Index(Box::new(e), idx), depth.max(d))?;
            } else {
                return Ok((e, depth));
            }
        }
    }

    fn primary(&mut self) -> Result<Tree> {
        let pos = self.pos();
        match self.bump() {
            Tok::Num(n) => self.number(n),
            Tok::Ident(name) if self.eat("(") => self.call(name),
            Tok::Ident(name) => self.quoted_unit_after((Expr::Ident(name), 1)),
            Tok::Unit(u) => Ok((Expr::Unit(u), 1)),
            Tok::Str(s) => Ok((Expr::Str(s), 1)),
            Tok::Sym("(") => self.paren(),
            Tok::Sym("[") => self.bracket(pos),
            Tok::Sym("?") => Ok((Expr::Placeholder, 1)),
            t => Err(not_a_value(&t, pos)),
        }
    }

    /// `2 m`, `5.1 'm`: a number followed by a unit or a plain name multiplies it.
    #[inline(never)]
    fn number(&mut self, n: String) -> Result<Tree> {
        let num = (Expr::Num(n), 1);
        if matches!(self.peek(), Tok::Unit(_) | Tok::Ident(_)) {
            let unit = self.unit_term()?;
            return self.bin(BinaryOp::Juxt, num, unit);
        }
        Ok(num)
    }

    #[inline(never)]
    fn call(&mut self, name: String) -> Result<Tree> {
        let (args, d) = self.list(")")?;
        self.grow(Expr::Call(name, args), d)
    }

    #[inline(never)]
    fn paren(&mut self) -> Result<Tree> {
        let (e, d) = self.expr(DEFINE)?;
        self.expect(")")?;
        let paren = self.grow(Expr::Paren(Box::new(e)), d)?;
        self.quoted_unit_after(paren)
    }

    /// A vector or matrix; `[0; 1; 2] s`: a unit after it applies to every element.
    #[inline(never)]
    fn bracket(&mut self, pos: usize) -> Result<Tree> {
        let m = self.matrix(pos)?;
        let unit = matches!(self.peek(), Tok::Unit(_) | Tok::Ident(_))
            && !matches!(self.peek2(), Tok::Sym("(" | "["));
        if unit {
            let u = self.unit_term()?;
            return self.bin(BinaryOp::Juxt, m, u);
        }
        Ok(m)
    }

    /// `x '°C`, `(-20) '°C`: a quoted unit after a name or parentheses applies to it, as after a
    /// number. Only a quoted unit: a plain name there (`x m`) would read as two variables.
    #[inline(never)]
    fn quoted_unit_after(&mut self, e: Tree) -> Result<Tree> {
        if matches!(self.peek(), Tok::Unit(_)) {
            let unit = self.unit_term()?;
            return self.bin(BinaryOp::Juxt, e, unit);
        }
        Ok(e)
    }

    #[inline(never)]
    fn unit_term(&mut self) -> Result<Tree> {
        let pos = self.pos();
        let unit = match self.peek() {
            // `2 sin(x)`, `3 v[i]`
            Tok::Ident(_) if matches!(self.peek2(), Tok::Sym("(" | "[")) => self.postfix()?,
            _ => match self.bump() {
                Tok::Unit(u) => (Expr::Unit(u), 1),
                Tok::Ident(n) => (Expr::Ident(n), 1),
                t => return Err(ParseError::new(format!("expected a unit but found {}", describe(&t)), pos)),
            },
        };
        if self.eat("^") {
            let exponent = self.expr(UNARY)?;
            return self.bin(BinaryOp::Pow, unit, exponent);
        }
        Ok(unit)
    }

    /// Comma-separated expressions up to `close`, and the depth of the deepest.
    #[inline(never)]
    fn list(&mut self, close: &str) -> Result<(Vec<Expr>, usize)> {
        let mut items = Vec::new();
        let mut depth = 0;
        if self.eat(close) {
            return Ok((items, depth));
        }
        loop {
            let (e, d) = self.expr(DEFINE)?;
            items.push(e);
            depth = depth.max(d);
            if !self.eat(",") {
                self.expect(close)?;
                return Ok((items, depth));
            }
        }
    }

    #[inline(never)]
    fn matrix(&mut self, pos: usize) -> Result<Tree> {
        if self.eat("]") {
            return Err(ParseError::new("empty matrix", pos));
        }
        let mut rows: Vec<Vec<Expr>> = Vec::new();
        let mut row = Vec::new();
        let mut depth = 0;
        loop {
            let (e, d) = self.expr(DEFINE)?;
            row.push(e);
            depth = depth.max(d);
            if self.eat(",") {
                continue;
            }
            if self.eat(";") {
                rows.push(std::mem::take(&mut row));
                continue;
            }
            self.expect("]")?;
            break;
        }
        rows.push(row);
        let width = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|r| r.len() != width) {
            return Err(ParseError::new("matrix rows have different lengths", pos));
        }
        self.grow(Expr::Matrix(rows), depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    /// Runs `f` on a thread with the stack of the web version (WebAssembly's default 1 MB);
    /// debug builds get more, as their frames are several times larger than optimised ones.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let size = if cfg!(debug_assertions) { 8 << 20 } else { 1 << 20 };
        let run = std::thread::Builder::new().stack_size(size).spawn(f);
        match run.map(|h| h.join()) {
            Ok(Ok(v)) => v,
            _ => std::process::abort(),
        }
    }

    fn nested(open: &str, close: &str, n: usize) -> String {
        format!("{}1{}", open.repeat(n), close.repeat(n))
    }

    fn is_too_deep(r: Result<Expr>) -> bool {
        r.is_err_and(|e| e.message.contains("too deeply nested"))
    }

    #[test]
    fn nesting_up_to_the_limit_parses_prints_and_drops() {
        on_small_stack(|| {
            // each exactly MAX_DEPTH deep: n brackets or prefixes around a leaf, or n + 1 terms
            let n = MAX_DEPTH - 1;
            for src in [
                nested("(", ")", n),
                nested("abs(", ")", n),
                nested("[", "]", n),
                format!("{}1", "-".repeat(n)),
                format!("{}1", "not ".repeat(n)),
                vec!["2"; n + 1].join("^"),
                vec!["x"; n + 1].join(" + "),
            ] {
                let e = parse(&src);
                assert!(e.as_ref().is_ok_and(|e| e.depth() == MAX_DEPTH), "{}", &src[..20]);
                assert!(e.is_ok_and(|e| print(&e) == src));
            }
        });
    }

    #[test]
    fn nesting_past_the_limit_is_an_error() {
        on_small_stack(|| {
            for src in [
                nested("(", ")", 3000),
                nested("abs(", ")", MAX_DEPTH),
                nested("[", "]", 100_000),
                format!("{}1", "-".repeat(30_000)),
                format!("{}1", "not ".repeat(30_000)),
                format!("x{}", "!".repeat(30_000)),
                format!("x{}", "[1]".repeat(30_000)),
                vec!["2"; 30_000].join("^"),
                vec!["x"; 30_000].join("+"),
                vec!["x"; 30_000].join("*"),
                vec!["x"; 30_000].join(" and "),
                vec!["x"; 30_000].join(" := "),
            ] {
                assert!(is_too_deep(parse(&src)), "{}", &src[..20]);
            }
        });
    }

    #[test]
    fn operators_keep_their_precedence() -> TestResult {
        for (src, grouped) in [
            ("a or b and not c < d..e + f*-g^h!", "a or (b and (not (c < (d..(e + (f*(-(g^(h!)))))))))"),
            ("not a and b", "(not a) and b"),
            ("-a^b*c", "((-(a^b)))*c"),
            ("a - b - c", "(a - b) - c"),
            ("a^b^c", "a^(b^c)"),
            ("a := b := c + 1", "a := (b := (c + 1))"),
            ("a..b < c..d ≤ e", "(a..b) < (c..d) ≤ e"),
            ("2^-3*4", "(2^(-3))*4"),
        ] {
            assert_eq!(parse(src)?, parse(grouped).map(strip)?, "{src}");
        }
        Ok(())
    }

    #[test]
    fn misplaced_operators_are_errors() {
        for src in ["a..b..c", "a < b..c..d", "a + not b", "(a..b..c)", "a := ", "not"] {
            assert!(parse(src).is_err(), "{src}");
        }
    }

    /// `e` without the parentheses written to show its grouping.
    fn strip(e: Expr) -> Expr {
        let s = |x: Box<Expr>| Box::new(strip(*x));
        match e {
            Expr::Paren(x) => strip(*x),
            Expr::Unary(op, x) => Expr::Unary(op, s(x)),
            Expr::Factorial(x) => Expr::Factorial(s(x)),
            Expr::Binary(op, a, b) => Expr::Binary(op, s(a), s(b)),
            Expr::Range(a, b) => Expr::Range(s(a), s(b)),
            Expr::Compare(a, rest) => {
                Expr::Compare(s(a), rest.into_iter().map(|(op, x)| (op, strip(x))).collect())
            }
            Expr::Define { lhs, rhs, global } => Expr::Define { lhs: s(lhs), rhs: s(rhs), global },
            other => other,
        }
    }
}
