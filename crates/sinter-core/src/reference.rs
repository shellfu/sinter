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

/// A declared field and its written type. Resolution uses this fact for
/// receiver chains such as `self.harness.check()` without pretending that
/// the field access itself is a symbol definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FieldBinding {
    /// Type/class/struct that declares the field.
    pub owner: NodeId,
    pub name: String,
    /// Type exactly as written (`Arc<dyn Harness>`, `&Dog`, ...).
    pub type_name: String,
}

/// Why a reference remained outside the graph. These are deliberately
/// outcome descriptions, not guesses at a compiler diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedReason {
    /// Source evidence pointed into the corpus, but did not identify one
    /// target (missing member, ambiguity, or incomplete type facts).
    SyntaxAnchoredMiss,
    /// Source extraction had no corpus anchor and no compiler index was
    /// available to distinguish an external/builtin from a missed edge.
    SyntaxOnly,
    /// A compiler index was present but supplied no in-corpus target.
    CompilerUnresolved,
}

impl UnresolvedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntaxAnchoredMiss => "syntax_anchored_miss",
            Self::SyntaxOnly => "syntax_only",
            Self::CompilerUnresolved => "compiler_unresolved",
        }
    }
}

/// Persisted unresolved outcome: the reference plus the coverage context
/// that produced the miss.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnresolvedReference {
    pub reference: Reference,
    pub reason: UnresolvedReason,
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
