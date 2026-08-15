use tree_sitter::Language;

/// A language is data: a grammar, a capture query, and comment node kinds.
/// The engine consumes only this struct — adding a language adds a row here
/// and a `.scm` file, never engine code.
///
/// Query capture contract (the universal primitives):
/// - `@def.<kind>`   whole definition node; `<kind>` must parse via
///   `SymbolKind::from_str_opt`. A definition also scopes what it contains.
/// - `@name`         the definition's (or scope's) name node, same match.
/// - `@qualifier`    optional extra scope prefix from the same match
///   (e.g. a Go method receiver type).
/// - `@scope`        a node that scopes names but is not itself a symbol
///   (e.g. a Rust `impl` block); pairs with `@name`.
/// - `@ref.<rel>`    a reference site; `<rel>` in {call, use} maps to the
///   relation an eventual binding would carry.
/// - `@import`       an imported path; quotes are stripped.
/// - `@import.module` + `@import.name` — from-style imports
///   (`from util import helper`, `import { helper } from "./util"`): the
///   engine joins them with the language's first path separator so the
///   import binds the item, not just the module.
pub struct LanguageSpec {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub grammar: fn() -> Language,
    pub query_source: &'static str,
    pub comment_kinds: &'static [&'static str],
    /// Maps a repo-relative file path to its module/package path segments —
    /// what import statements are matched against. Pure data transform;
    /// the resolver stays language-blind.
    pub module_path: fn(&str) -> Vec<String>,
    /// Splits an import/reference path into segments (`::` vs `/` vs `.`).
    pub path_separators: &'static [&'static str],
    /// Turns a raw import/reference path into absolute module segments,
    /// resolving language-relative forms (`super::`, leading dots, `./`)
    /// against the path's own file. Data transform; resolver stays blind.
    pub absolutize: fn(path: &str, file: &str) -> Vec<String>,
    /// Receiver keywords (`self`, `this`): a qualified reference through one
    /// binds within the reference's enclosing type.
    pub receivers: &'static [&'static str],
}

fn split_all(path: &str, separators: &[&str]) -> Vec<String> {
    let mut segments = vec![path.to_string()];
    for sep in separators {
        segments = segments
            .iter()
            .flat_map(|s| s.split(sep).map(str::to_string))
            .collect();
    }
    segments.into_iter().filter(|s| !s.is_empty()).collect()
}

fn dirname_segments(file: &str) -> Vec<String> {
    let mut segments: Vec<String> = file.split('/').map(str::to_string).collect();
    segments.pop();
    segments
}

/// `crate::x` stays absolute; `super::x`/`self::x` resolve against the
/// file's own module path.
fn rust_absolutize(path: &str, file: &str) -> Vec<String> {
    let mut module = rust_module_path(file);
    let mut rest = path;
    if let Some(r) = rest.strip_prefix("self::") {
        rest = r;
    } else {
        while let Some(r) = rest.strip_prefix("super::") {
            module.pop();
            rest = r;
        }
        if rest.len() == path.len() {
            if path.starts_with("crate::") {
                return split_all(path, &["::", "."]);
            }
            // Bare paths are relative to the file's own module
            // (`internal::helper` inside lib.rs means crate::internal::...).
            module.extend(split_all(path, &["::", "."]));
            return module;
        }
    }
    module.extend(split_all(rest, &["::", "."]));
    module
}

fn go_absolutize(path: &str, _file: &str) -> Vec<String> {
    split_all(path, &["/", "."])
}

/// Leading dots are package-relative: one dot is the file's own package,
/// each further dot one package up.
fn python_absolutize(path: &str, file: &str) -> Vec<String> {
    let dots = path.len() - path.trim_start_matches('.').len();
    if dots == 0 {
        return split_all(path, &["."]);
    }
    let mut base = dirname_segments(file);
    for _ in 1..dots {
        base.pop();
    }
    base.extend(split_all(&path[dots..], &["."]));
    base
}

/// `./x` and `../x` resolve against the file's directory.
fn typescript_absolutize(path: &str, file: &str) -> Vec<String> {
    if !path.starts_with('.') {
        return split_all(path, &["/", "."]);
    }
    let mut base = dirname_segments(file);
    let mut rest = path;
    loop {
        if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("../") {
            base.pop();
            rest = r;
        } else {
            break;
        }
    }
    base.extend(split_all(rest, &["/"]));
    base
}

fn rust_grammar() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn go_grammar() -> Language {
    tree_sitter_go::LANGUAGE.into()
}

/// `src/util.rs` -> ["crate", "util"]; `src/foo/mod.rs` -> ["crate", "foo"].
// ponytail: single-crate view; multi-crate workspaces collide on "crate" and
// resolve only unambiguous suffixes. Crate-name mapping lands in Phase 7.
fn rust_module_path(file: &str) -> Vec<String> {
    let trimmed = file.strip_suffix(".rs").unwrap_or(file);
    let after_src = trimmed.rsplit_once("src/").map_or(trimmed, |(_, r)| r);
    let mut segments = vec!["crate".to_string()];
    for seg in after_src.split('/') {
        if !matches!(seg, "lib" | "main" | "mod" | "") {
            segments.push(seg.to_string());
        }
    }
    segments
}

/// `pkg/util/util.go` -> ["pkg", "util"]; root files -> [].
fn go_module_path(file: &str) -> Vec<String> {
    let mut segments: Vec<String> = file.split('/').map(str::to_string).collect();
    segments.pop(); // file name; Go packages are directories
    segments
}

fn python_grammar() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

fn typescript_grammar() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

/// `pkg/mod.py` -> ["pkg", "mod"]; `pkg/__init__.py` -> ["pkg"].
fn python_module_path(file: &str) -> Vec<String> {
    let trimmed = file.strip_suffix(".py").unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !matches!(*s, "__init__" | ""))
        .map(str::to_string)
        .collect()
}

/// `src/util.ts` -> ["src", "util"]; `src/index.ts` -> ["src"].
fn typescript_module_path(file: &str) -> Vec<String> {
    let trimmed = file
        .strip_suffix(".tsx")
        .or_else(|| file.strip_suffix(".ts"))
        .unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !matches!(*s, "index" | ""))
        .map(str::to_string)
        .collect()
}

pub static LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        name: "rust",
        extensions: &["rs"],
        grammar: rust_grammar,
        query_source: include_str!("../queries/rust.scm"),
        comment_kinds: &["line_comment", "block_comment"],
        module_path: rust_module_path,
        path_separators: &["::", "."],
        absolutize: rust_absolutize,
        receivers: &["self", "Self"],
    },
    LanguageSpec {
        name: "go",
        extensions: &["go"],
        grammar: go_grammar,
        query_source: include_str!("../queries/go.scm"),
        comment_kinds: &["comment"],
        module_path: go_module_path,
        path_separators: &["/", "."],
        absolutize: go_absolutize,
        receivers: &[],
    },
    LanguageSpec {
        name: "python",
        extensions: &["py"],
        grammar: python_grammar,
        query_source: include_str!("../queries/python.scm"),
        comment_kinds: &["comment"],
        module_path: python_module_path,
        path_separators: &["."],
        absolutize: python_absolutize,
        receivers: &["self", "cls"],
    },
    LanguageSpec {
        name: "typescript",
        extensions: &["ts", "tsx"],
        grammar: typescript_grammar,
        query_source: include_str!("../queries/typescript.scm"),
        comment_kinds: &["comment"],
        module_path: typescript_module_path,
        path_separators: &["/", "."],
        absolutize: typescript_absolutize,
        receivers: &["this"],
    },
];

/// Spec for a file, by extension.
pub fn spec_for_path(path: &str) -> Option<&'static LanguageSpec> {
    let ext = path.rsplit('.').next()?;
    LANGUAGES.iter().find(|spec| spec.extensions.contains(&ext))
}
