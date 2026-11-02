//! Quire numeric kernel and worksheet evaluator.
//!
//! - [`Value`]: numbers carry SI magnitude + [`quire_units::Dim`]; matrices and strings.
//! - [`Interp`]: evaluates one expression in a [`Scope`].
//! - [`Worksheet`]: SMath/Mathcad document semantics — regions run in reading order,
//!   a definition is visible below it, `≡` global definitions are visible everywhere.

mod budget;
mod builtins;
mod constants;
mod error;
mod format;
mod interp;
mod linalg;
mod numeric;
mod ops;
mod plot;
mod sheet;
mod solvers;
mod stats;
mod symbolic;
mod value;
mod worksheet;

pub use constants::{CONSTANTS, Constant, constant};
pub use error::{EvalError, Result};
pub use format::{
    DisplaySettings, DisplaySpec, Notation, WorkingUnit, format_number, format_shown, format_value,
    format_value_in, parse_unit_spec, working_unit,
};
pub use interp::{Binding, Env, Interp, Scope};
pub use plot::{
    Animate, Axis, DEFAULT_FPS, Marker, PlotInput, PlotOutcome, SAMPLES, SeriesInput, SeriesOutcome,
};
pub use sheet::{CellOutcome, TableInput, cell_label, column_name, parse_ref};
pub use value::{Matrix, Quantity, Value};
pub use worksheet::{Fix, MathInput, Outcome, Worksheet};
