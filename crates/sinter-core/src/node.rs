use serde::{Deserialize, Serialize};

/// Identifier of a graph node.
///
/// Comparison is byte-exact and case-sensitive: `Config` and `config` are
/// distinct ids. A collision on insert is an error, never a merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Byte range of a symbol in its source file. `end` is exclusive; a valid
/// span has `end > start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Variant,
    Trait,
    Interface,
    TypeAlias,
    Constant,
    Static,
    Variable,
    Field,
    Module,
    Macro,
    File,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Variant => "variant",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::TypeAlias => "typealias",
            Self::Constant => "constant",
            Self::Static => "static",
            Self::Variable => "variable",
            Self::Field => "field",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::File => "file",
        }
    }

    /// Inverse of [`SymbolKind::as_str`]; the mapping extraction query
    /// captures (`@def.<kind>`) resolve through.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        Some(match s {
            "function" => Self::Function,
            "method" => Self::Method,
            "struct" => Self::Struct,
            "class" => Self::Class,
            "enum" => Self::Enum,
            "variant" => Self::Variant,
            "trait" => Self::Trait,
            "interface" => Self::Interface,
            "typealias" => Self::TypeAlias,
            "constant" => Self::Constant,
            "static" => Self::Static,
            "variable" => Self::Variable,
            "field" => Self::Field,
            "module" => Self::Module,
            "macro" => Self::Macro,
            "file" => Self::File,
            _ => return None,
        })
    }
}

/// A symbol with enough content that a query result saves the consumer a
/// file read: signature, doc comment, and exact byte span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: SymbolKind,
    pub name: String,
    /// Repo-relative source file path.
    pub file: String,
    pub span: Span,
    /// Declaration text; may be empty for kinds with no meaningful signature.
    pub signature: String,
    pub doc: Option<String>,
}
