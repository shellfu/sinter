use serde::{Deserialize, Serialize};

/// Repository role of a source file and every graph node declared in it.
///
/// Scope is persisted per file by the store rather than repeated in every
/// node blob. The path classifier is deliberately conservative: repositories
/// can override an exceptional path, while an uncertain path remains
/// production instead of disappearing from the default agent corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusScope {
    Production,
    Test,
    Fixture,
    Example,
    Generated,
    Vendor,
    Docs,
}

impl CorpusScope {
    pub const ALL: [Self; 7] = [
        Self::Production,
        Self::Test,
        Self::Fixture,
        Self::Example,
        Self::Generated,
        Self::Vendor,
        Self::Docs,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
            Self::Fixture => "fixture",
            Self::Example => "example",
            Self::Generated => "generated",
            Self::Vendor => "vendor",
            Self::Docs => "docs",
        }
    }

    pub fn from_str_opt(value: &str) -> Option<Self> {
        Some(match value {
            "production" | "prod" => Self::Production,
            "test" | "tests" => Self::Test,
            "fixture" | "fixtures" => Self::Fixture,
            "example" | "examples" => Self::Example,
            "generated" => Self::Generated,
            "vendor" | "vendored" => Self::Vendor,
            "docs" | "documentation" => Self::Docs,
            _ => return None,
        })
    }

    /// Conservative path-only classification used when a repository has no
    /// explicit override. Rules operate on complete path components and
    /// well-known generated suffixes to avoid hiding ordinary source whose
    /// name merely contains words such as `test` or `example`.
    ///
    /// Test-infrastructure directories (`harness`, `bench`, `benches`,
    /// `benchmark`, `benchmarks`, `eval`, `evals`, `e2e`) count only as the
    /// top-level component: a crate or module literally named `eval` under
    /// `crates/` or `src/` stays production. Nested `fixtures`, `golden`,
    /// `testdata`, `examples`, and `tests` components still match anywhere.
    ///
    /// Rust convention: `tests.rs`, `*_tests.rs`, and `test_*.rs` basenames
    /// are test files wherever they sit under `src/`.
    pub fn classify_path(file: &str) -> Self {
        if file.starts_with("dep:") {
            return Self::Vendor;
        }
        let lower = file.replace('\\', "/").to_ascii_lowercase();
        let components = lower.split('/').collect::<Vec<_>>();
        let basename = components.last().copied().unwrap_or(&lower);

        if components.iter().any(|component| {
            matches!(
                *component,
                "vendor" | "vendored" | "third_party" | "third-party" | "node_modules"
            )
        }) {
            return Self::Vendor;
        }
        if components.iter().any(|component| {
            matches!(
                *component,
                "generated" | "autogen" | "auto-generated" | "generated-src"
            )
        }) || basename.contains(".generated.")
            || basename.contains("_generated.")
            || basename.ends_with(".g.rs")
            || basename.ends_with(".pb.go")
            || basename.ends_with(".designer.cs")
        {
            return Self::Generated;
        }
        if components.iter().any(|component| {
            matches!(
                *component,
                "fixture"
                    | "fixtures"
                    | "golden"
                    | "testdata"
                    | "test-data"
                    | "snapshot"
                    | "snapshots"
                    | "__snapshots__"
            )
        }) {
            return Self::Fixture;
        }
        if components.iter().any(|component| {
            matches!(
                *component,
                "example" | "examples" | "sample" | "samples" | "demo" | "demos"
            )
        }) {
            return Self::Example;
        }
        if components
            .iter()
            .any(|component| matches!(*component, "test" | "tests" | "spec" | "specs"))
            || components.first().is_some_and(|component| {
                matches!(
                    *component,
                    "harness"
                        | "bench"
                        | "benches"
                        | "benchmark"
                        | "benchmarks"
                        | "eval"
                        | "evals"
                        | "e2e"
                )
            })
            || basename.starts_with("test_")
            || basename == "tests.rs"
            || basename.ends_with("_tests.rs")
            || basename.contains("_test.")
            || basename.contains(".test.")
            || basename.contains("_spec.")
            || basename.contains(".spec.")
        {
            return Self::Test;
        }
        if components
            .first()
            .is_some_and(|component| matches!(*component, "docs" | "doc" | "documentation"))
            || matches!(
                basename,
                "readme"
                    | "readme.md"
                    | "readme.mdx"
                    | "changelog.md"
                    | "contributing.md"
                    | "architecture.md"
            )
            || [".md", ".mdx", ".rst", ".adoc", ".asciidoc"]
                .iter()
                .any(|extension| basename.ends_with(extension))
        {
            return Self::Docs;
        }
        Self::Production
    }
}

impl std::fmt::Display for CorpusScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CorpusScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_str_opt(value).ok_or_else(|| {
            format!(
                "unknown scope `{value}` (expected production, test, fixture, example, generated, vendor, or docs)"
            )
        })
    }
}

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

    /// The qualified declaration path encoded in this snapshot-local id.
    /// File-node ids contain no `#` and therefore qualify as their path.
    pub fn qualified(&self) -> &str {
        match self.0.split_once('#') {
            Some((_, rest)) => rest
                .rsplit_once('@')
                .map_or(rest, |(qualified, _)| qualified),
            None => &self.0,
        }
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A declaration handle that is independent of byte offsets.
///
/// The encoded form is `symbol:<kind>:<file-byte-length>:<file>#<qualified>`.
/// The length prefix keeps parsing unambiguous even when a path contains `#`.
/// A key is deliberately not a unique id: overloads or duplicate declarations
/// with the same kind and qualified path share it, and callers must handle the
/// resulting candidate set rather than guessing a binding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolKey(String);

impl SymbolKey {
    pub const PREFIX: &'static str = "symbol:";

    pub fn new(kind: SymbolKind, file: &str, qualified: &str) -> Self {
        Self(format!(
            "{}{}:{}:{file}#{qualified}",
            Self::PREFIX,
            kind.as_str(),
            file.len()
        ))
    }

    pub fn parse(encoded: impl Into<String>) -> Option<Self> {
        let key = Self(encoded.into());
        key.parts()?;
        Some(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parts(&self) -> Option<(SymbolKind, &str, &str)> {
        let rest = self.0.strip_prefix(Self::PREFIX)?;
        let (kind, rest) = rest.split_once(':')?;
        let (file_len, rest) = rest.split_once(':')?;
        let file_len: usize = file_len.parse().ok()?;
        let file = rest.get(..file_len)?;
        let qualified = rest.get(file_len..)?.strip_prefix('#')?;
        if file.is_empty() || qualified.is_empty() {
            return None;
        }
        Some((SymbolKind::from_str_opt(kind)?, file, qualified))
    }
}

impl std::fmt::Display for SymbolKey {
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
    /// A prose document section (markdown heading); appended for postcard
    /// wire compatibility — never reorder.
    Section,
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
            Self::Section => "section",
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
            "section" => Self::Section,
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

impl Node {
    /// Stable semantic handle for this declaration. It survives unrelated
    /// text inserted before the declaration; unlike [`NodeId`], it does not
    /// claim uniqueness among overloads or duplicate declarations.
    pub fn symbol_key(&self) -> SymbolKey {
        SymbolKey::new(self.kind, &self.file, self.id.qualified())
    }
}

#[cfg(test)]
mod tests {
    use super::{CorpusScope, Node, NodeId, Span, SymbolKey, SymbolKind};

    fn node(id: &str, signature: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind: SymbolKind::Function,
            name: "run".to_string(),
            file: "src/lib.rs".to_string(),
            span: Span { start: 10, end: 20 },
            signature: signature.to_string(),
            doc: None,
        }
    }

    #[test]
    fn symbol_key_survives_offset_changes() {
        let before = node("src/lib.rs#Runner::run@10", "fn run()");
        let after = node("src/lib.rs#Runner::run@200", "fn run()");
        assert_ne!(before.id, after.id);
        assert_eq!(before.symbol_key(), after.symbol_key());
        assert_eq!(
            before.symbol_key().parts(),
            Some((SymbolKind::Function, "src/lib.rs", "Runner::run"))
        );
        assert_eq!(
            SymbolKey::parse(before.symbol_key().to_string()),
            Some(before.symbol_key())
        );
    }

    #[test]
    fn overloads_share_a_key_without_claiming_uniqueness() {
        let one = node("src/lib.rs#run@10", "fn run(u8)");
        let two = node("src/lib.rs#run@30", "fn run(u16)");
        assert_eq!(one.symbol_key(), two.symbol_key());
    }

    #[test]
    fn corpus_scope_uses_conservative_path_roles() {
        let cases = [
            ("src/lib.rs", CorpusScope::Production),
            ("tests/integration.rs", CorpusScope::Test),
            ("harness/golden/example/main.rs", CorpusScope::Fixture),
            ("examples/client.rs", CorpusScope::Example),
            ("src/generated/schema.pb.go", CorpusScope::Generated),
            ("third_party/parser.c", CorpusScope::Vendor),
            ("docs/architecture.md", CorpusScope::Docs),
            ("src/contest.rs", CorpusScope::Production),
            ("harness/eval/runner/scoring.rs", CorpusScope::Test),
            (
                "harness/eval/fixtures/agent-flow/main.rs",
                CorpusScope::Fixture,
            ),
            (
                "harness/golden/fixtures/go-basic/main.go",
                CorpusScope::Fixture,
            ),
            ("benches/ask.rs", CorpusScope::Test),
            ("e2e/smoke.ts", CorpusScope::Test),
            ("crates/sinter-cli/tests/cli.rs", CorpusScope::Test),
            ("crates/eval/src/lib.rs", CorpusScope::Production),
            ("src/eval/mod.rs", CorpusScope::Production),
            ("src/tests.rs", CorpusScope::Test),
            ("crates/foo/src/bar/tests.rs", CorpusScope::Test),
            ("src/bar_tests.rs", CorpusScope::Test),
            ("src/test_bar.rs", CorpusScope::Test),
            ("crates/foo/src/bar/tests/cases.rs", CorpusScope::Test),
            ("src/contests.rs", CorpusScope::Production),
        ];
        for (path, expected) in cases {
            assert_eq!(CorpusScope::classify_path(path), expected, "{path}");
        }
    }
}
