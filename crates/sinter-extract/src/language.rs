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
/// - `@import.alias`   local rebinding (`as` clauses, Go dot imports).
/// - `@import.star`    glob semantics: with `@import.module` (Python `*`)
///   or alongside a plain `@import` (bash `source`), every top-level name
///   of the module binds.
/// - `@local` (+ `@local.type`) — shadowing bindings, optionally typed.
/// - `@embed`          embedded/promoted type members (Go).
/// - `@trait` + `@trait.impl` — an impl block (`@trait.impl`) naming the
///   trait it implements (`@trait`): dynamic-dispatch pairing input.
/// - `@doc`            a node whose text IS the definition's doc (Python
///   docstrings): attached to the smallest containing definition,
///   overriding any sibling-comment doc.
///
/// Standard tree-sitter text predicates (`#eq?`, `#any-of?`, ...) are
/// evaluated by the tree-sitter crate itself and may be used freely
/// (bash isolates `source` from ordinary commands this way).
/// A package manifest declares the mapping between a package's *name*
/// (what imports say) and its *directory* (what module paths say) — a
/// naming root the file tree alone cannot reveal. Reading it is
/// evidence, exactly like reading an import statement. Pure data; the
/// engine never branches on language.
pub struct ManifestSpec {
    /// Manifest file basename ("Cargo.toml", "go.mod").
    pub filename: &'static str,
    /// Key whose value is the package name ("name" for `name = "x"`,
    /// "module" for `module x`).
    pub name_key: &'static str,
    /// Path-head aliases meaning "this package's root" ("crate").
    pub self_names: &'static [&'static str],
    /// Normalizes the declared name to reference form (dashes to
    /// underscores for Rust).
    pub normalize: fn(&str) -> String,
}

/// A discovered package root: files under `dir` belong to package `name`
/// for language `language`.
#[derive(Debug, Clone)]
pub struct ModuleRoot {
    pub name: String,
    /// Repo-relative directory of the manifest ("" for repo root).
    pub dir: String,
    pub language: &'static str,
}

/// Parse one candidate file into a module root, if its basename matches
/// a language's manifest spec. `rel_path` is repo-relative.
pub fn manifest_root(rel_path: &str, content: &str) -> Option<ModuleRoot> {
    let base = rel_path.rsplit('/').next()?;
    let spec = LANGUAGES
        .iter()
        .find(|l| l.manifest.is_some_and(|m| m.filename == base))?;
    let m = spec.manifest?;
    let name = content.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(m.name_key)?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
        let name = rest.trim_matches('"').trim();
        (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
    })?;
    let dir = rel_path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    Some(ModuleRoot {
        name: (m.normalize)(&name),
        dir,
        language: spec.name,
    })
}

/// A secondary grammar for languages whose spec splits container and
/// content parses (tree-sitter markdown's block/inline split). The engine
/// re-parses the byte ranges of the named container nodes with this
/// grammar and runs its query through the same capture contract; spans
/// stay file-absolute via included-range parsing. Pure data.
pub struct InlineSpec {
    pub grammar: fn() -> Language,
    pub query_source: &'static str,
    /// Node kinds in the primary tree whose ranges the inline grammar
    /// parses.
    pub container_kinds: &'static [&'static str],
}

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
    /// Node kinds the doc-comment walk may step over (max 2) between a
    /// definition and its doc — e.g. Unreal's `UCLASS(...)` line, which
    /// parses as an expression_statement between comment and class.
    pub doc_skip_kinds: &'static [&'static str],
    /// Package manifest shape, when the language has one that names
    /// module roots (see ManifestSpec).
    pub manifest: Option<&'static ManifestSpec>,
    /// Secondary inline grammar applied to designated container-node
    /// ranges, whose captures merge into the file's facts (markdown's
    /// block/inline split).
    pub inline: Option<&'static InlineSpec>,
    /// References in this language are document paths naming corpus
    /// files (`[text](docs/guide.md#setup)`), not symbol paths: the
    /// resolver binds them by exact file path — with or without the
    /// language's extensions, `#fragment` to the target file's unique
    /// def whose name slugifies to the fragment — and never through the
    /// symbol tiers. Evidence or nothing: a dead link stays unresolved.
    pub file_refs: bool,
}

fn rust_normalize(name: &str) -> String {
    name.replace('-', "_")
}

static RUST_MANIFEST: ManifestSpec = ManifestSpec {
    filename: "Cargo.toml",
    name_key: "name",
    self_names: &["crate"],
    normalize: rust_normalize,
};

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

/// `acme/core/v1/action_event.proto` -> ["acme","core","v1","action_event"]:
/// the file extension must not become a module segment, or the import key
/// never suffix-matches the file's own module path and same-package bare
/// references stay unresolved.
fn proto_absolutize(path: &str, _file: &str) -> Vec<String> {
    split_all(path.strip_suffix(".proto").unwrap_or(path), &["/", "."])
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

fn bash_grammar() -> Language {
    tree_sitter_bash::LANGUAGE.into()
}

/// Bash has no module system: a file is its path. `lib/util.sh` ->
/// ["lib", "util"].
fn bash_module_path(file: &str) -> Vec<String> {
    let trimmed = file
        .strip_suffix(".sh")
        .or_else(|| file.strip_suffix(".bash"))
        .unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `source` paths: strip `$(dirname "$0")/` and `./` style prefixes and
/// resolve against the sourcing file's directory; bare paths pass through.
fn bash_absolutize(path: &str, file: &str) -> Vec<String> {
    let trimmed = path.trim();
    let dir_relative = [
        "$(dirname \"$0\")/",
        "$(dirname $0)/",
        "${BASH_SOURCE%/*}/",
        "./",
    ]
    .iter()
    .find_map(|p| trimmed.strip_prefix(p));
    let stripped = |s: &str| {
        s.strip_suffix(".sh")
            .or_else(|| s.strip_suffix(".bash"))
            .unwrap_or(s)
            .to_string()
    };
    match dir_relative {
        Some(rest) => {
            let mut base = dirname_segments(file);
            base.extend(rest.split('/').filter(|s| !s.is_empty()).map(stripped));
            base
        }
        None => trimmed
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(stripped)
            .collect(),
    }
}

fn cpp_grammar() -> Language {
    tree_sitter_cpp::LANGUAGE.into()
}

/// `player/character.h` and `player/character.cpp` share the module
/// ["player", "character"] — header/impl pairs resolve into one another.
fn cpp_module_path(file: &str) -> Vec<String> {
    let trimmed = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Quoted include paths, extension-stripped, `./` resolved against the
/// including file's directory; `<system>` includes pass through (and stay
/// external unless a matching module exists in the corpus).
fn cpp_absolutize(path: &str, file: &str) -> Vec<String> {
    let trimmed = path.trim().trim_matches(['<', '>']);
    let no_ext = trimmed.rsplit_once('.').map_or(trimmed, |(stem, ext)| {
        if matches!(
            ext,
            "h" | "hh" | "hpp" | "hxx" | "cpp" | "cc" | "cxx" | "inl"
        ) {
            stem
        } else {
            trimmed
        }
    });
    if let Some(rest) = no_ext.strip_prefix("./") {
        let mut base = dirname_segments(file);
        base.extend(
            rest.split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
        return base;
    }
    // Member access (`c.jump`, `this->jump`) must split so the resolver's
    // receiver/typed-local tiers see a prefix (fixture: cpp-header-impl).
    no_ext
        .replace("->", ".")
        .split(['/', ':', '.'])
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn proto_grammar() -> Language {
    tree_sitter_proto::LANGUAGE.into()
}

/// `contracts/payments.proto` -> ["contracts", "payments"]. Generated-stub
/// imports name proto symbols through package paths; module keys are the
/// file path, packages resolve via the same suffix matching as Go.
fn proto_module_path(file: &str) -> Vec<String> {
    let trimmed = file.strip_suffix(".proto").unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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

fn javascript_grammar() -> Language {
    tree_sitter_javascript::LANGUAGE.into()
}

fn c_grammar() -> Language {
    tree_sitter_c::LANGUAGE.into()
}

fn java_grammar() -> Language {
    tree_sitter_java::LANGUAGE.into()
}

fn csharp_grammar() -> Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

/// C# module identity is the file's directory, Go-style: `using Acme.Util;`
/// imports a namespace (never a type), and namespaces conventionally mirror
/// directories. `Acme/Util/TextHelper.cs` -> ["Acme", "Util"], so the using
/// directive's segments suffix-match the directory key. Path-derived only:
/// namespace declarations that diverge from layout are out of scope
/// (stated boundary of the csharp pack, see queries/csharp.scm).
fn csharp_module_path(file: &str) -> Vec<String> {
    let mut segments: Vec<String> = file.split('/').map(str::to_string).collect();
    segments.pop(); // file name; namespaces are directories
    segments
}

fn markdown_grammar() -> Language {
    tree_sitter_md::LANGUAGE.into()
}

fn markdown_inline_grammar() -> Language {
    tree_sitter_md::INLINE_LANGUAGE.into()
}

/// Inline content (`[text](target)`) is a second grammar in tree-sitter's
/// markdown split; the engine parses the block tree's `inline` ranges
/// with it (see InlineSpec).
static MARKDOWN_INLINE: InlineSpec = InlineSpec {
    grammar: markdown_inline_grammar,
    query_source: include_str!("../queries/markdown-inline.scm"),
    container_kinds: &["inline"],
};

/// Link destinations resolve against the linking file's directory —
/// `./x`, `../x`, and bare `x` alike (doc-link convention); the `.md`
/// extension is not a module segment.
fn markdown_absolutize(path: &str, file: &str) -> Vec<String> {
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
    let rest = rest
        .strip_suffix(".md")
        .or_else(|| rest.strip_suffix(".markdown"))
        .unwrap_or(rest);
    base.extend(
        rest.split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    base
}

/// A document is its path: `docs/team.md` -> ["docs", "team"].
fn markdown_module_path(file: &str) -> Vec<String> {
    let trimmed = file
        .strip_suffix(".md")
        .or_else(|| file.strip_suffix(".markdown"))
        .unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn sql_grammar() -> Language {
    tree_sitter_sequel::LANGUAGE.into()
}

fn javascript_module_path(file: &str) -> Vec<String> {
    let trimmed = file
        .strip_suffix(".jsx")
        .or_else(|| file.strip_suffix(".mjs"))
        .or_else(|| file.strip_suffix(".cjs"))
        .or_else(|| file.strip_suffix(".js"))
        .unwrap_or(file);
    trimmed
        .split('/')
        .filter(|s| !matches!(*s, "index" | ""))
        .map(str::to_string)
        .collect()
}

fn javascript_absolutize(path: &str, file: &str) -> Vec<String> {
    typescript_absolutize(path, file)
}

fn c_module_path(file: &str) -> Vec<String> {
    cpp_module_path(file)
}

fn c_absolutize(path: &str, file: &str) -> Vec<String> {
    cpp_absolutize(path, file)
}

/// Java packages are directories: `com/acme/util/Text.java` ->
/// ["com", "acme", "util"], matching a path-aligned `package com.acme.util;`.
/// The class contributes its own segment as a definition, so the FQN
/// `com.acme.util.Text` resolves as module + top-level def.
// ponytail: assumes package declarations align with directories; parse the
// package_declaration into module identity if misaligned repos matter.
fn java_module_path(file: &str) -> Vec<String> {
    dirname_segments(file)
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
}

/// Java qualified references arrive as full invocation text
/// (`Text.trim(s)`): everything from the first `(` on is arguments, not
/// path. Imports are already-absolute dotted FQNs.
fn java_absolutize(path: &str, _file: &str) -> Vec<String> {
    let head = path.split('(').next().unwrap_or(path);
    head.split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// All .sql files in a directory share one namespace (a database schema
/// has no per-file scoping), so the module key is the directory alone:
/// `db/schema.sql` and `db/queries.sql` both map to ["db"].
fn sql_module_path(file: &str) -> Vec<String> {
    let dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
    dir.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Dotted absolute imports (C#/SQL style): already-absolute FQNs split
/// on dots; no relative forms exist.
fn dotted_absolutize(path: &str, _file: &str) -> Vec<String> {
    path.trim()
        .split('.')
        .filter(|s| !s.is_empty())
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
        doc_skip_kinds: &[],
        manifest: Some(&RUST_MANIFEST),
        inline: None,
        file_refs: false,
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
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
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
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
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
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "bash",
        extensions: &["sh", "bash"],
        grammar: bash_grammar,
        query_source: include_str!("../queries/bash.scm"),
        comment_kinds: &["comment"],
        module_path: bash_module_path,
        path_separators: &["/"],
        absolutize: bash_absolutize,
        receivers: &[],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "proto",
        extensions: &["proto"],
        grammar: proto_grammar,
        query_source: include_str!("../queries/proto.scm"),
        comment_kinds: &["comment"],
        module_path: proto_module_path,
        path_separators: &["/", "."],
        absolutize: proto_absolutize,
        receivers: &[],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "h"],
        grammar: cpp_grammar,
        query_source: include_str!("../queries/cpp.scm"),
        comment_kinds: &["comment"],
        module_path: cpp_module_path,
        path_separators: &["/", "::"],
        absolutize: cpp_absolutize,
        receivers: &["this"],
        doc_skip_kinds: &["expression_statement"],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        grammar: javascript_grammar,
        query_source: include_str!("../queries/javascript.scm"),
        comment_kinds: &["comment"],
        module_path: javascript_module_path,
        path_separators: &["/", "."],
        absolutize: javascript_absolutize,
        receivers: &["this"],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "c",
        extensions: &["c"],
        grammar: c_grammar,
        query_source: include_str!("../queries/c.scm"),
        comment_kinds: &["comment"],
        module_path: c_module_path,
        path_separators: &["/"],
        absolutize: c_absolutize,
        receivers: &[],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "java",
        extensions: &["java"],
        grammar: java_grammar,
        query_source: include_str!("../queries/java.scm"),
        comment_kinds: &["line_comment", "block_comment"],
        module_path: java_module_path,
        path_separators: &["."],
        absolutize: java_absolutize,
        receivers: &["this"],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "csharp",
        extensions: &["cs"],
        grammar: csharp_grammar,
        query_source: include_str!("../queries/csharp.scm"),
        comment_kinds: &["comment"],
        module_path: csharp_module_path,
        path_separators: &["."],
        absolutize: dotted_absolutize,
        receivers: &["this", "base"],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "sql",
        extensions: &["sql"],
        grammar: sql_grammar,
        query_source: include_str!("../queries/sql.scm"),
        comment_kinds: &["comment", "marginalia"],
        module_path: sql_module_path,
        path_separators: &["."],
        absolutize: dotted_absolutize,
        receivers: &[],
        doc_skip_kinds: &[],
        manifest: None,
        inline: None,
        file_refs: false,
    },
    LanguageSpec {
        name: "markdown",
        extensions: &["md", "markdown"],
        grammar: markdown_grammar,
        query_source: include_str!("../queries/markdown.scm"),
        comment_kinds: &[],
        module_path: markdown_module_path,
        path_separators: &["/"],
        absolutize: markdown_absolutize,
        receivers: &[],
        doc_skip_kinds: &[],
        manifest: None,
        inline: Some(&MARKDOWN_INLINE),
        file_refs: true,
    },
];

/// Spec for a file, by extension.
pub fn spec_for_path(path: &str) -> Option<&'static LanguageSpec> {
    let ext = path.rsplit('.').next()?;
    LANGUAGES.iter().find(|spec| spec.extensions.contains(&ext))
}
