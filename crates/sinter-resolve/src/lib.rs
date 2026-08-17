mod resolver;
mod scip;

pub use resolver::{Binding, ResolutionStats, qualified_of, resolve, resolve_boundary};
pub use scip::{ScipError, load_index, merge_index_files, resolve_with_index};
