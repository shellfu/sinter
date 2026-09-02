mod proto_service_bindings;
mod resolver;
mod scip;

pub use proto_service_bindings::proto_service_edges;
pub use resolver::{
    Binding, Index, Resolution, ResolutionStats, dynamic_edges, qualified_of, resolve,
    resolve_boundary,
};
pub use scip::{
    ScipError, ScipResolution, load_index, merge_index_files, prefix_index_paths,
    resolve_with_index,
};
