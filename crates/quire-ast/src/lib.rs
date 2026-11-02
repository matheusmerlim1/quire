//! Quire expression AST and its linear text syntax (docs/02-file-format.md §4.3).
//!
//! The AST is the one expression representation shared by the parser, the printer,
//! the evaluator and (later) the structural 2D editor. `print(parse(s))` reproduces
//! canonical text exactly, so documents round-trip without diff noise.

mod ast;
mod lexer;
mod parser;
mod printer;

pub use ast::{BinaryOp, CmpOp, Expr, UnaryOp};
pub use parser::{MAX_DEPTH, ParseError, parse};
pub use printer::{print, quote_str};
