use serde::{Deserialize, Serialize};

use crate::edge::Relation;
use crate::node::{NodeId, Span};

/// A use of a name that extraction saw but has not bound to a definition.
///
/// Unresolved is a first-class outcome: references are stored and countable,
/// never guessed into edges. Phase 3 resolution consumes these and promotes
/// each to an evidence-backed edge — or leaves it here, counted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Reference {
    /// Repo-relative file the reference appears in.
    pub file: String,
    /// Referenced name as written (call target, import path, ...).
    pub name: String,
    /// Full path text at the reference site when the name was qualified
    /// (`fmt.Println`, `util::double`) — import-evidence input.
    pub path: Option<String>,
    /// Relation an eventual binding would carry.
    pub relation: Relation,
    pub span: Span,
    /// Innermost definition containing the reference site, if any —
    /// the `src` of the edge resolution would create.
    pub enclosing: Option<NodeId>,
    /// Local rebinding of the imported/referenced name: `as` clauses
    /// (`use x as y`, `import a as b`), Go's dot import (`.`), and glob
    /// imports (`*`). The `name` field always keeps the original path.
    pub alias: Option<String>,
}

/// A local binding (parameter, let/const, loop variable, catch variable)
/// that shadows outer names. Not a symbol — recorded so resolution never
/// binds a shadowed reference to the outer definition it no longer means.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LocalBinding {
    pub file: String,
    pub name: String,
    /// Where the binding is introduced.
    pub span: Span,
    /// End of the innermost definition containing it — the binding shadows
    /// references from `span.start` to here.
    pub scope_end: u64,
    /// Declared/constructed type when the language chose to expose it
    /// (`c *Counter`, `c := Counter{}`): local type evidence for method
    /// binding. None = shadow-only binding.
    pub type_name: Option<String>,
}

/// A type embedding another (Go embedded struct field): member lookup on
/// the owner falls through to the embedded type. Lookup fact, not an edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Embed {
    pub owner: NodeId,
    pub type_name: String,
}

/// An impl block naming the trait it implements (`impl Runner for Widget`).
/// Pairing fact for dynamic-dispatch edges: methods defined inside `span`
/// implement the same-named methods of the trait `trait_name` resolves to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TraitImpl {
    pub file: String,
    /// Trait name as written (leaf segment for qualified paths).
    pub trait_name: String,
    /// Span of the whole impl block.
    pub span: Span,
}
