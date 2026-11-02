// LaTeX-named symbols and equation structures, for `\name` shortcuts and the galleries.

export interface Sym {
  /** LaTeX command without the backslash. */
  name: string;
  /** What gets inserted: a character, or a template where `?` marks the slots. */
  insert: string;
  /** Shown in galleries (defaults to `insert`). */
  label?: string;
  /** Not understood by the calculator: fine in text, an error in math. */
  textOnly?: boolean;
  /** Shown instead of `\name` (unit suggestions show the converted result). */
  note?: string;
  /** A `\` command (`\text`, `\ref`) instead of something to insert: the app command it runs. */
  command?: string;
}

/** Where the `\` menu is open: in math being typed, or in text. */
export type Where = "math" | "text";

/** `\` commands, listed before symbols where they apply. */
const COMMANDS: (Sym & { where: Where })[] = [
  { name: "text", insert: "", label: "¶", note: "\\text  write text instead", command: "to_text", where: "math" },
  { name: "plot", insert: "", label: "∿", note: "\\plot  plot this function", command: "insert_plot", where: "math" },
  { name: "table", insert: "", label: "▦", note: "\\table  insert a table", command: "insert_table", where: "math" },
  {
    name: "math",
    insert: "",
    label: "x²",
    note: "\\math  live math in the sentence",
    command: "insert_math",
    where: "text",
  },
  { name: "ref", insert: "", label: "§", note: "\\ref  cross-reference…", command: "insert_ref", where: "text" },
];

export const commandsFor = (where: Where) => COMMANDS.filter((c) => c.where === where);

export interface Category {
  id: string;
  title: string;
  items: Sym[];
}

const s = (name: string, insert: string, textOnly = false): Sym => ({ name, insert, textOnly });

const greekLower: Sym[] = [
  s("alpha", "α"),
  s("beta", "β"),
  s("gamma", "γ"),
  s("delta", "δ"),
  s("epsilon", "ϵ"),
  s("varepsilon", "ε"),
  s("zeta", "ζ"),
  s("eta", "η"),
  s("theta", "θ"),
  s("vartheta", "ϑ"),
  s("iota", "ι"),
  s("kappa", "κ"),
  s("lambda", "λ"),
  s("mu", "μ"),
  s("nu", "ν"),
  s("xi", "ξ"),
  s("omicron", "ο"),
  s("pi", "π"),
  s("varpi", "ϖ"),
  s("rho", "ρ"),
  s("varrho", "ϱ"),
  s("sigma", "σ"),
  s("varsigma", "ς"),
  s("tau", "τ"),
  s("upsilon", "υ"),
  s("phi", "ϕ"),
  s("varphi", "φ"),
  s("chi", "χ"),
  s("psi", "ψ"),
  s("omega", "ω"),
];

const greekUpper: Sym[] = [
  s("Gamma", "Γ"),
  s("Delta", "Δ"),
  s("Theta", "Θ"),
  s("Lambda", "Λ"),
  s("Xi", "Ξ"),
  s("Pi", "Π"),
  s("Sigma", "Σ"),
  s("Upsilon", "Υ"),
  s("Phi", "Φ"),
  s("Psi", "Ψ"),
  s("Omega", "Ω"),
];

const operators: Sym[] = [
  s("pm", "±"),
  s("times", "×"),
  s("cdot", "·"),
  s("div", "÷"),
  s("coloneq", "≔"),
  s("equiv", "≡"),
  s("le", "≤"),
  s("leq", "≤"),
  s("ge", "≥"),
  s("geq", "≥"),
  s("ne", "≠"),
  s("neq", "≠"),
  s("infty", "∞"),
  s("hbar", "ħ"),
  s("approx", "≈", true),
  s("sim", "∼", true),
  s("propto", "∝", true),
  s("mp", "∓", true),
  s("partial", "∂", true),
  s("nabla", "∇", true),
  s("ll", "≪", true),
  s("gg", "≫", true),
];

const arrows: Sym[] = [
  s("to", "→", true),
  s("rightarrow", "→", true),
  s("leftarrow", "←", true),
  s("leftrightarrow", "↔", true),
  s("Rightarrow", "⇒", true),
  s("Leftarrow", "⇐", true),
  s("Leftrightarrow", "⇔", true),
  s("uparrow", "↑", true),
  s("downarrow", "↓", true),
  s("mapsto", "↦", true),
  s("implies", "⇒", true),
  s("iff", "⇔", true),
];

const logic: Sym[] = [
  s("therefore", "∴", true),
  s("because", "∵", true),
  s("forall", "∀", true),
  s("exists", "∃", true),
  s("in", "∈", true),
  s("notin", "∉", true),
  s("subset", "⊂", true),
  s("subseteq", "⊆", true),
  s("cup", "∪", true),
  s("cap", "∩", true),
  s("emptyset", "∅", true),
  s("neg", "¬", true),
  s("land", "∧", true),
  s("lor", "∨", true),
];

const misc: Sym[] = [
  s("degree", "°", true),
  s("prime", "′", true),
  s("dprime", "″", true),
  s("ell", "ℓ"),
  s("angle", "∠", true),
  s("perp", "⊥", true),
  s("parallel", "∥", true),
  s("triangle", "△", true),
  s("square", "□", true),
  s("dots", "…", true),
  s("cdots", "⋯", true),
  s("circ", "∘", true),
  s("bullet", "•", true),
  s("permil", "‰", true),
  s("checkmark", "✓", true),
  s("dagger", "†", true),
];

const t = (name: string, insert: string, label: string): Sym => ({ name, insert, label });

/** Equation structures (Word's "Structures"), as linear-syntax templates. */
export const STRUCTURES: Category[] = [
  {
    id: "fraction",
    title: "Fraction",
    items: [t("frac", "(?)/(?)", "Fraction"), t("dfrac", "?/?", "Linear fraction")],
  },
  {
    id: "script",
    title: "Script",
    items: [
      t("sup", "?^?", "Superscript"),
      t("sub", "?[?]", "Index"),
      t("sq", "?^2", "Square"),
      t("inv", "?^-1", "Inverse"),
    ],
  },
  {
    id: "radical",
    title: "Radical",
    items: [
      t("sqrt", "sqrt(?)", "Square root"),
      t("cbrt", "root(?, 3)", "Cube root"),
      t("root", "root(?, ?)", "n-th root"),
    ],
  },
  {
    id: "integral",
    title: "Integral",
    items: [t("int", "int(?, x, ?, ?)", "Definite integral")],
  },
  {
    id: "largeop",
    title: "Large operator",
    items: [t("sum", "sum(?, i, 1, n)", "Sum"), t("prod", "prod(?, i, 1, n)", "Product")],
  },
  {
    id: "calculus",
    title: "Calculus",
    items: [t("diff", "diff(?, x)", "Derivative"), t("ddiff", "diff(?, x, 2)", "Second derivative")],
  },
  {
    id: "solve",
    title: "Solve",
    items: [
      t("solve", "solve(? == ?, x, ?, ?)", "Solve an equation in a range"),
      t("solvex", "solve(? == ?, x)", "Solve from the current value"),
      t("rootf", "root(?, x, ?, ?)", "Root of an expression"),
      t("solvesys", "solve([? == ?; ? == ?], [x; y])", "System of equations from the current x, y"),
      t("lsolve", "lsolve(?, ?)", "Linear system A·x = b"),
      t("rkfixed", "rkfixed(?, 0, ?, 100, D)", "ODE y′ = D(t, y), fixed steps"),
      t("Rkadapt", "Rkadapt(?, 0, ?, 100, D)", "ODE y′ = D(t, y), adaptive steps"),
    ],
  },
  {
    id: "data",
    title: "Data",
    items: [
      t("linterp", "linterp(?, ?, ?)", "Interpolate in a table"),
      t("cinterp", "cinterp(?, ?, ?)", "Cubic spline through a table, at a point"),
      t("cspline", "interp(cspline(?, ?), ?, ?, ?)", "Cubic spline through a table"),
      t("slope", "slope(?, ?)", "Slope of the best-fit line"),
      t("intercept", "intercept(?, ?)", "Intercept of the best-fit line"),
      t("corr", "corr(?, ?)", "Correlation coefficient"),
      t("polyfit", "polyfit(?, ?, 2)", "Polynomial fit [a0; a1; a2]"),
      t("linfit", "linfit(?, ?, F)", "Least squares on the functions F(x)"),
      t("median", "median(?)", "Median"),
      t("Stdev", "Stdev(?)", "Standard deviation of a sample (n − 1)"),
      t("stdev", "stdev(?)", "Standard deviation of a population (n)"),
      t("percentile", "percentile(?, 0.9)", "Percentile (p from 0 to 1)"),
      t("Ceil", "Ceil(?, ?)", "Round up to a multiple"),
    ],
  },
  {
    id: "matrix",
    title: "Matrix",
    items: [
      t("row", "[?, ?]", "Row vector"),
      t("col", "[?; ?]", "Column vector"),
      t("matrix", "[?, ?; ?, ?]", "2 × 2"),
      t("matrix3", "[?, ?, ?; ?, ?, ?; ?, ?, ?]", "3 × 3"),
      t("csort", "csort(?, ?)", "Sort rows by a column"),
      t("rsort", "rsort(?, ?)", "Sort columns by a row"),
      t("tr", "tr(?)", "Trace"),
      t("submatrix", "submatrix(?, ?, ?, ?, ?)", "Rows r1 to r2, columns c1 to c2"),
      t("diag", "diag(?)", "Diagonal matrix from a vector"),
      t("minor", "minor(?, ?, ?)", "Minor M(i, j)"),
      t("rank", "rank(?)", "Rank"),
      t("findrows", "findrows(?, ?, ?)", "Rows whose column j equals x"),
      t("polyroots", "polyroots(?)", "Roots of a polynomial [a0; a1; a2]"),
    ],
  },
  {
    id: "vector",
    title: "Vector",
    items: [
      t("vsum", "sum(?)", "Sum of elements"),
      t("mean", "mean(?)", "Mean"),
      t("sort", "sort(?)", "Sort"),
      t("norm", "norm(?)", "Length"),
      t("dotp", "dot(?, ?)", "Dot product"),
      t("cross", "cross(?, ?)", "Cross product"),
    ],
  },
  {
    id: "function",
    title: "Function",
    items: [
      t("sin", "sin(?)", "Sine"),
      t("cos", "cos(?)", "Cosine"),
      t("tan", "tan(?)", "Tangent"),
      t("ln", "ln(?)", "Natural log"),
      t("log", "log(?, 10)", "Logarithm"),
      t("exp", "exp(?)", "Exponential"),
      t("abs", "abs(?)", "Absolute value"),
      t("max", "max(?, ?)", "Maximum"),
      t("min", "min(?, ?)", "Minimum"),
      t("if", "if(?, ?, ?)", "Condition"),
    ],
  },
];

export const SYMBOLS: Category[] = [
  { id: "greek", title: "Greek", items: greekLower },
  { id: "Greek", title: "Greek capitals", items: greekUpper },
  { id: "operators", title: "Operators", items: operators },
  { id: "arrows", title: "Arrows", items: arrows },
  { id: "logic", title: "Logic & sets", items: logic },
  { id: "misc", title: "Other", items: misc },
];

const ALL: Sym[] = [...SYMBOLS.flatMap((c) => c.items), ...STRUCTURES.flatMap((c) => c.items)];
const BY_NAME = new Map(ALL.map((x) => [x.name, x]));

export const isTemplate = (x: Sym) => x.insert.includes("?");

export function exact(name: string, where?: Where): Sym | undefined {
  return (where && commandsFor(where).find((c) => c.name === name)) || BY_NAME.get(name);
}

/** Symbols (and, with `where`, commands) starting with `prefix`; exact match first, then shorter names. */
export function completions(prefix: string, limit = 8, where?: Where): Sym[] {
  const lower = prefix.toLowerCase();
  const commands = where ? commandsFor(where).filter((c) => c.name.startsWith(lower)) : [];
  const starts = ALL.filter((x) => x.name.startsWith(prefix));
  const loose = ALL.filter((x) => !x.name.startsWith(prefix) && x.name.toLowerCase().startsWith(lower));
  const symbols = [...starts, ...loose].sort(
    (a, b) => Number(b.name === prefix) - Number(a.name === prefix) || a.name.length - b.name.length,
  );
  return [...commands, ...symbols].slice(0, limit + commands.length);
}
