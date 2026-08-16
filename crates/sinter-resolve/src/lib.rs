mod resolver;
mod scip;

pub use resolver::{Binding, ResolutionStats, qualified_of, resolve, resolve_boundary};
pub use scip::{ScipError, load_index, resolve_with_index};
