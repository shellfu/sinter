use serde::{Deserialize, Serialize};

use crate::node::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Relation {
    Calls,
    Uses,
    Imports,
    Contains,
    Implements,
    Extends,
}

impl Relation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Uses => "uses",
            Self::Imports => "imports",
            Self::Contains => "contains",
            Self::Implements => "implements",
            Self::Extends => "extends",
        }
    }
}

/// What binds this edge to its target (R2: evidence or nothing).
/// Global name uniqueness is not evidence and has no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Evidence {
    /// Syntactic containment seen directly in the parse tree.
    Structural,
    /// Name visible in the reference's own file scope.
    Scope,
    /// An import statement binds the reference's path to the target.
    Import,
    /// A compiler-produced SCIP index binds reference to definition.
    Scip,
    /// An operator-declared binding from a workspace manifest (runtime
    /// coupling like queue topics/HTTP routes that no static analysis can
    /// see). Auditable in the manifest; never inferred.
    Declared,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Scope => "scope",
            Self::Import => "import",
            Self::Scip => "scip",
            Self::Declared => "declared",
        }
    }

    /// Compiler-grade evidence is certain; heuristic-free but indirect
    /// evidence (scope/import matching) is inferred.
    pub fn confidence(self) -> Confidence {
        match self {
            Self::Structural | Self::Scip | Self::Declared => Confidence::Certain,
            Self::Scope | Self::Import => Confidence::Inferred,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Confidence {
    Certain,
    Inferred,
}

/// Directed edge `src -> dst`. The graph is a multigraph: parallel edges
/// that differ in relation, evidence, or confidence coexist.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
    pub relation: Relation,
    pub evidence: Evidence,
    pub confidence: Confidence,
}
