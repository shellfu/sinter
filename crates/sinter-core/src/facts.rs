use serde::{Deserialize, Serialize};

use crate::edge::Edge;
use crate::node::{CorpusScope, Node, NodeId};
use crate::reference::{Embed, FieldBinding, LocalBinding, Reference, TraitImpl};

/// Everything extraction produces for one file. Content-addressed: the hash
/// is the incrementality key — same bytes, same facts. The persisted set of
/// `FileFacts` is the source of truth every derived table rebuilds from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFacts {
    /// Repo-relative path.
    pub file: String,
    /// blake3 of the source bytes, hex.
    pub content_hash: String,
    /// Tree-sitter reported syntax errors; facts are best-effort partial.
    pub has_syntax_errors: bool,
    /// File node plus every definition, content-bearing.
    pub nodes: Vec<Node>,
    /// Structural containment edges (Certain).
    pub contains: Vec<Edge>,
    /// Reference sites: calls, imports. Resolution input.
    pub references: Vec<Reference>,
    /// Local bindings that shadow outer names. Resolution suppression input.
    pub locals: Vec<LocalBinding>,
    /// Declared field types for field-receiver resolution.
    pub fields: Vec<FieldBinding>,
    /// Type embeddings (promoted members). Resolution lookup input.
    pub embeds: Vec<Embed>,
    /// Trait-impl blocks. Dynamic-dispatch edge input.
    pub trait_impls: Vec<TraitImpl>,
    /// Node-level scope overrides (sparse): nodes whose role differs from
    /// their file's path-derived scope, e.g. a Rust `#[cfg(test)]` module's
    /// members or a `@generated` header's file node.
    pub scopes: Vec<(NodeId, CorpusScope)>,
    /// Body-only identifier words per function/method (sparse): words the
    /// body uses that the name, signature, and doc do not. Ranking evidence
    /// for concept-phrased questions.
    pub body_terms: Vec<(NodeId, Vec<String>)>,
}
