use crate::node::NodeId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("duplicate node id `{0}` (ids are case-sensitive; collision is an error, not a merge)")]
    DuplicateNode(NodeId),
    #[error("edge endpoint `{0}` does not exist in the graph")]
    MissingEndpoint(NodeId),
    #[error("node `{id}` has an empty {field}")]
    EmptyField { id: NodeId, field: &'static str },
    #[error("node `{id}` has an invalid span {start}..{end}")]
    InvalidSpan { id: NodeId, start: u64, end: u64 },
}
