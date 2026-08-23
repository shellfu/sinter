//! Body-identifier evidence: the distinct words a definition's body uses
//! that its name, signature, and doc do not. Lets a concept-phrased
//! question ("stat-only walk") reach a function whose body calls
//! `metadata()` even though nothing in its header says so.
//!
//! Language-agnostic by design: the body span's text is tokenized
//! directly, so identifiers, field names, comment words, and string
//! literals all land without per-grammar queries. Keywords fall to the
//! stopword list; rarer keywords are harmless (high df, ~0 IDF).

use std::collections::BTreeSet;

use sinter_core::{CorpusScope, Node, NodeId, SymbolKind};

/// Hard cap per node: a giant function is not more about every word.
const MAX_TERMS: usize = 64;
const MIN_CHARS: usize = 3;

/// Language keywords, common type/trait names, and the generic
/// variable names that carry no topic (measured top-df on this repo).
const STOPWORDS: &[&str] = &[
    // question stopwords (mirror ask/query.rs)
    "and",
    "are",
    "been",
    "can",
    "could",
    "does",
    "find",
    "for",
    "from",
    "how",
    "into",
    "its",
    "located",
    "may",
    "might",
    "must",
    "only",
    "our",
    "shall",
    "should",
    "show",
    "that",
    "the",
    "these",
    "this",
    "those",
    "was",
    "were",
    "what",
    "where",
    "which",
    "who",
    "whom",
    "will",
    "with",
    "would",
    "you",
    "your",
    // keywords across the supported grammars
    "abstract",
    "async",
    "await",
    "bool",
    "boolean",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "crate",
    "def",
    "default",
    "defer",
    "del",
    "dyn",
    "elif",
    "else",
    "enum",
    "except",
    "extends",
    "extern",
    "false",
    "final",
    "finally",
    "float",
    "func",
    "function",
    "impl",
    "implements",
    "int",
    "interface",
    "lambda",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "namespace",
    "new",
    "nil",
    "none",
    "not",
    "null",
    "override",
    "package",
    "pass",
    "private",
    "protected",
    "pub",
    "public",
    "raise",
    "ref",
    "return",
    "self",
    "static",
    "str",
    "string",
    "struct",
    "super",
    "switch",
    "then",
    "throw",
    "throws",
    "trait",
    "true",
    "try",
    "type",
    "typeof",
    "unsafe",
    "unsigned",
    "use",
    "var",
    "void",
    "where",
    "while",
    "with",
    "yield",
    "u32",
    "u64",
    "i64",
    "usize",
    "f64",
    "undefined",
    "instanceof",
    "readonly",
    "declare",
    "virtual",
    "template",
    "sizeof",
    "goto",
    "chan",
    "range",
    "global",
    "nonlocal",
    "assert",
    "std",
    // measured top-df on this repo: glue vocabulary, not topics
    "contains",
    "empty",
    "matches",
    "owned",
    "take",
    "once",
    "lossy",
    "utf8",
    "bytes",
    "current",
    "begin",
    "starts",
    "array",
    "total",
    "join",
    "dir",
    "tempfile",
    "tempdir",
    "success",
    "stdout",
    "stderr",
    "exe",
    "bin",
    "bail",
    "src",
    "lib",
    "serde",
    "cargo",
    "env",
    "display",
    "missing",
    "strip",
    "trim",
    "prefix",
    "txn",
    "anyhow",
    "context",
    "ensure",
    "with_context",
    "flatten",
    "slice",
    "extend",
    "rsplit",
    "min",
    "max",
    "found",
    // generic names / std vocabulary
    "ok",
    "err",
    "some",
    "result",
    "option",
    "value",
    "values",
    "item",
    "items",
    "len",
    "iter",
    "into",
    "map",
    "get",
    "set",
    "unwrap",
    "expect",
    "clone",
    "push",
    "vec",
    "box",
    "string",
    "format",
    "println",
    "eprintln",
    "collect",
    "from",
    "str",
    "fn",
    "as_str",
    "to_string",
    "as_ref",
    "as_deref",
    "unwrap_or",
    "entry",
    "key",
    "out",
    "res",
    "ret",
    "tmp",
    "idx",
    "obj",
    "arg",
    "args",
    "ctx",
    "data",
    "name",
    "names",
    "node",
    "nodes",
    "file",
    "files",
    "path",
    "line",
    "lines",
    "else",
    "first",
    "last",
    "next",
    "text",
    "end",
    "start",
    "count",
    "list",
    "err",
    "error",
    "errors",
    "msg",
    "message",
    "type",
    "kind",
    "new",
    "other",
    "one",
    "two",
    "all",
    "any",
    "each",
    "are",
    "iter",
    "mut",
];

fn is_stop(word: &str) -> bool {
    STOPWORDS.contains(&word)
}

/// Lowercased subwords of one identifier, split on camelCase boundaries
/// (an uppercase after a non-uppercase, or the start of a lowercase run
/// after an acronym).
fn camel_split(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_uppercase()
            && (!chars[i - 1].is_uppercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if boundary && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Distinct lowercased subwords of `text`, in first-seen order.
pub(crate) fn words(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for ident in text.split(|c: char| !c.is_alphanumeric()) {
        for sub in camel_split(ident) {
            if sub.chars().count() >= MIN_CHARS
                && sub.chars().any(|c| c.is_alphabetic())
                && seen.insert(sub.clone())
            {
                out.push(sub);
            }
        }
    }
    out
}

/// Sparse `(node, terms)` for every production function/method in
/// `nodes`. Test bodies are skipped: the default ask scope never ranks
/// them and their fixture vocabulary would dominate document frequency.
pub fn body_terms(
    source: &str,
    nodes: &[Node],
    scopes: &[(NodeId, CorpusScope)],
) -> Vec<(NodeId, Vec<String>)> {
    let file_is_test = nodes
        .first()
        .is_some_and(|n| CorpusScope::classify_path(&n.file) == CorpusScope::Test);
    if file_is_test {
        return Vec::new();
    }
    nodes
        .iter()
        .filter(|n| matches!(n.kind, SymbolKind::Function | SymbolKind::Method))
        .filter(|n| {
            !scopes
                .iter()
                .any(|(id, s)| *s == CorpusScope::Test && id == &n.id)
        })
        .filter_map(|n| {
            let body = source.get(n.span.start as usize..n.span.end as usize)?;
            let header: BTreeSet<String> = words(&n.name)
                .into_iter()
                .chain(words(&n.signature))
                .chain(words(n.doc.as_deref().unwrap_or("")))
                .collect();
            let terms: Vec<String> = words(body)
                .into_iter()
                .filter(|w| !header.contains(w) && !is_stop(w))
                .take(MAX_TERMS)
                .collect();
            (!terms.is_empty()).then(|| (n.id.clone(), terms))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{Extractor, spec_for_path};

    #[test]
    fn body_words_exclude_header_and_keywords() {
        let src = "/// Walk the tree.\npub fn scan_file(path: &Path) -> bool {\n    // stat only, never read\n    let meta = std::fs::metadata(path).unwrap();\n    let fooBar = \"string literal here\";\n    meta.isFile()\n}\n";
        let facts = Extractor::new(spec_for_path("src/x.rs").unwrap())
            .unwrap()
            .extract("src/x.rs", src)
            .unwrap();
        let (id, terms) = &facts.body_terms[0];
        assert_eq!(id.qualified(), "scan_file");
        for w in [
            "stat", "never", "read", "meta", "metadata", "foo", "bar", "literal", "is",
        ] {
            assert_eq!(terms.iter().any(|t| t == w), w != "is", "{w}");
        }
        for w in [
            "scan", "file", "path", "bool", "walk", "let", "std", "unwrap", "tree",
        ] {
            assert!(!terms.iter().any(|t| t == w), "{w} should be excluded");
        }
    }
}
