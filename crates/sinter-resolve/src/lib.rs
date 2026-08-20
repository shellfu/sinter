mod resolver;
mod scip;

pub use resolver::{
    Binding, Index, ResolutionStats, dynamic_edges, qualified_of, resolve, resolve_boundary,
};
pub use scip::{
    ScipError, ScipResolution, load_index, merge_index_files, prefix_index_paths,
    resolve_with_index,
};
