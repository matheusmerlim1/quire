#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(tag = "t", content = "c"))]
pub enum Expr {
    /// Numeric literal, kept exactly as typed (`5.0` stays `5.0`).
    Num(String),
    /// Variable or constant; `.` starts a literal subscript (`A.net`).
    Ident(String),
    /// Unit reference, written `'m` in linear syntax.
    Unit(String),
    Str(String),
    /// An empty slot, written `?`: the editor's placeholder for something not yet typed.
    Placeholder,
    /// Parentheses the user typed. Kept so text round-trips exactly.
    Paren(Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Factorial(Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// Comparison chain: `a < b ≤ c` holds when every adjacent pair holds.
    Compare(Box<Expr>, Vec<(CmpOp, Expr)>),
    Call(String, Vec<Expr>),
    /// 1-based element access: `v[i]`, `M[i, j]`.
    Index(Box<Expr>, Vec<Expr>),
    /// Row-major cells; all rows have the same length.
    Matrix(Vec<Vec<Expr>>),
    Range(Box<Expr>, Box<Expr>),
    /// `lhs := rhs`, or `lhs ≡ rhs` (visible to the whole document) when `global`.
    Define {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        global: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum UnaryOp {
    Neg,
    Pos,
    PlusMinus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum BinaryOp {
    Or,
    And,
    Add,
    Sub,
    PlusMinus,
    Mul,
    Div,
    Mod,
    Pow,
    /// Implicit multiplication by juxtaposition: `2 m`, `5.1 'm`, `9.81 m/s^2`.
    Juxt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Pos => "+",
            UnaryOp::PlusMinus => "±",
            UnaryOp::Not => "not ",
        }
    }
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Or => "or",
            BinaryOp::And => "and",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::PlusMinus => "±",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Pow => "^",
            BinaryOp::Juxt => " ",
        }
    }
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "≠",
            CmpOp::Lt => "<",
            CmpOp::Le => "≤",
            CmpOp::Gt => ">",
            CmpOp::Ge => "≥",
        }
    }
}

impl Expr {
    /// A number with a juxtaposed unit or name: `5.1 'm`, `2 m`, `25.5 m^2`, or a name or
    /// parentheses with a quoted unit: `T '°C`, `(-20) '°C`.
    pub fn is_quantity(&self) -> bool {
        matches!(self, Expr::Binary(BinaryOp::Juxt, ..))
    }

    /// Levels of nesting, a leaf being 1. Worked out without recursion, so it is safe on a
    /// tree of any depth: callers that build trees check it against [`crate::MAX_DEPTH`].
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack: Vec<(&Expr, usize)> = vec![(self, 1)];
        while let Some((e, d)) = stack.pop() {
            deepest = deepest.max(d);
            let mut push = |x| stack.push((x, d + 1));
            match e {
                Expr::Num(_) | Expr::Ident(_) | Expr::Unit(_) | Expr::Str(_) | Expr::Placeholder => {}
                Expr::Paren(x) | Expr::Unary(_, x) | Expr::Factorial(x) => push(x),
                Expr::Binary(_, a, b) | Expr::Range(a, b) | Expr::Define { lhs: a, rhs: b, .. } => {
                    push(a);
                    push(b);
                }
                Expr::Compare(a, rest) => {
                    push(a);
                    rest.iter().for_each(|(_, x)| push(x));
                }
                Expr::Call(_, xs) => xs.iter().for_each(push),
                Expr::Index(b, xs) => {
                    push(b);
                    xs.iter().for_each(push);
                }
                Expr::Matrix(rows) => rows.iter().flatten().for_each(push),
            }
        }
        deepest
    }
}
