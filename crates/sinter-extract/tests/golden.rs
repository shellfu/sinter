//! Golden-corpus accuracy metric (R7). Precision/recall of extraction
//! against hand-verified expectations; any change that moves the metric
//! fails here with the delta (missing/extra) printed.

use std::path::{Path, PathBuf};

use sinter_extract::{Extractor, spec_for_path};

type Tuple = Vec<String>;

#[derive(serde::Deserialize)]
struct Expected {
    nodes: Vec<Tuple>,
    contains: Vec<Tuple>,
    references: Vec<Tuple>,
}

/// `{file}#{qualified}@{start}` -> qualified; plain file id -> the path.
fn qualified_of(id: &str) -> String {
    match id.split_once('#') {
        Some((_, rest)) => rest.rsplit_once('@').map_or(rest, |(q, _)| q).to_string(),
        None => id.to_string(),
    }
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.file_name().is_some_and(|n| n != "expected.json") {
            out.push(path);
        }
    }
}

fn extract_fixture(root: &Path) -> (Vec<Tuple>, Vec<Tuple>, Vec<Tuple>) {
    let mut files = Vec::new();
    source_files(root, &mut files);
    let (mut nodes, mut contains, mut references) = (Vec::new(), Vec::new(), Vec::new());
    for path in files {
        let rel = sinter_core::rel_display(path.strip_prefix(root).unwrap());
        assert!(!rel.contains('\\'), "path separator leaked into {rel}");
        let Some(spec) = spec_for_path(&rel) else {
            continue; // non-source fixture support file (go.mod, ...)
        };
        let source = std::fs::read_to_string(&path).unwrap();
        let facts = Extractor::new(spec)
            .unwrap()
            .extract(&rel, &source)
            .unwrap();
        // Misparse-resilience fixtures (macro-heavy C++) intentionally
        // contain grammar ERROR nodes; everything else must parse clean.
        const SYNTAX_ERRORS_EXPECTED: &[&str] = &["character.h"];
        assert!(
            !facts.has_syntax_errors || SYNTAX_ERRORS_EXPECTED.contains(&rel.as_str()),
            "fixture {rel} has syntax errors"
        );
        for n in &facts.nodes {
            nodes.push(vec![
                n.kind.as_str().to_string(),
                qualified_of(n.id.as_str()),
                n.file.clone(),
                n.doc
                    .as_deref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                n.span.start.to_string(),
                n.span.end.to_string(),
            ]);
        }
        for e in &facts.contains {
            contains.push(vec![
                qualified_of(e.src.as_str()),
                qualified_of(e.dst.as_str()),
            ]);
        }
        for r in &facts.references {
            references.push(vec![
                r.relation.as_str().to_string(),
                r.name.clone(),
                r.file.clone(),
                r.enclosing
                    .as_ref()
                    .map(|id| qualified_of(id.as_str()))
                    .unwrap_or_default(),
                r.path.clone().unwrap_or_default(),
                r.alias.clone().unwrap_or_default(),
                r.span.start.to_string(),
                r.span.end.to_string(),
            ]);
        }
    }
    nodes.sort();
    contains.sort();
    references.sort();
    (nodes, contains, references)
}

/// Expected tuples may be shorter than found tuples (legacy 3-element node
/// rows vs doc-bearing 4-element): prefix match, like the resolution runner.
fn tuple_matches(expected: &Tuple, found: &Tuple) -> bool {
    found.len() >= expected.len() && found[..expected.len()] == expected[..]
}

fn deltas<'a>(
    found: &'a [Tuple],
    expected: &'a [Tuple],
) -> (Vec<&'a Tuple>, Vec<&'a Tuple>, f64, f64) {
    // Multiset matching: duplicate references and edges are observable
    // regressions, not facts a set may silently collapse.
    let mut used = vec![false; found.len()];
    let mut missing = Vec::new();
    for expected_row in expected {
        if let Some((index, _)) = found
            .iter()
            .enumerate()
            .find(|(index, found_row)| !used[*index] && tuple_matches(expected_row, found_row))
        {
            used[index] = true;
        } else {
            missing.push(expected_row);
        }
    }
    let extra: Vec<&Tuple> = found
        .iter()
        .enumerate()
        .filter(|(index, _)| !used[*index])
        .map(|(_, row)| row)
        .collect();
    let p = if found.is_empty() {
        1.0
    } else {
        (found.len() - extra.len()) as f64 / found.len() as f64
    };
    let r = if expected.is_empty() {
        1.0
    } else {
        (expected.len() - missing.len()) as f64 / expected.len() as f64
    };
    (missing, extra, p, r)
}

/// Fixtures with known engine gaps. CI gates on everything else; a listed
/// fixture that STARTS passing also fails, so this list only ever shrinks.
const KNOWN_FAIL: &[&str] = &[];

fn check(fixture: &str) {
    let expected_fail = KNOWN_FAIL.contains(&fixture);
    let result = std::panic::catch_unwind(|| check_inner(fixture));
    match (result.is_ok(), expected_fail) {
        (true, false) => {}
        (false, false) => panic!("{fixture}: precision/recall below 1.0 — deltas above"),
        (false, true) => println!("{fixture}: known-fail (allowlisted engine gap)"),
        (true, true) => panic!("{fixture}: now PASSES — remove it from KNOWN_FAIL"),
    }
}

fn check_inner(fixture: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/golden/fixtures")
        .join(fixture);
    let expected: Expected =
        serde_json::from_str(&std::fs::read_to_string(root.join("expected.json")).unwrap())
            .unwrap();
    let (nodes, contains, references) = extract_fixture(&root);

    let mut ok = true;
    for (category, found, expected) in [
        ("nodes", nodes.as_slice(), expected.nodes.as_slice()),
        (
            "contains",
            contains.as_slice(),
            expected.contains.as_slice(),
        ),
        (
            "references",
            references.as_slice(),
            expected.references.as_slice(),
        ),
    ] {
        let (missing, extra, p, r) = deltas(found, expected);
        println!("{fixture}/{category}: precision {p:.3} recall {r:.3}");
        for m in missing {
            println!("  MISSING  {m:?}");
            ok = false;
        }
        for e in extra {
            println!("  EXTRA    {e:?}");
            ok = false;
        }
    }
    assert!(ok, "{fixture}: precision/recall below 1.0 — deltas above");
}

#[test]
fn golden_rust_basic() {
    check("rust-basic");
}

#[test]
fn golden_go_basic() {
    check("go-basic");
}

#[test]
fn golden_python_basic() {
    check("python-basic");
}

#[test]
fn golden_typescript_basic() {
    check("typescript-basic");
}

#[test]
fn golden_python_relative_import() {
    check("python-relative-import");
}

#[test]
fn golden_python_alias_import() {
    check("python-alias-import");
}

#[test]
fn golden_python_init_reexport() {
    check("python-init-reexport");
}

#[test]
fn golden_python_star_import() {
    check("python-star-import");
}

#[test]
fn golden_python_decorator() {
    check("python-decorator");
}

#[test]
fn golden_python_nested_function() {
    check("python-nested-function");
}

#[test]
fn golden_python_shadowed_param() {
    check("python-shadowed-param");
}

#[test]
fn golden_python_same_name_disambig() {
    check("python-same-name-disambig");
}

#[test]
fn golden_python_method_vs_function() {
    check("python-method-vs-function");
}

#[test]
fn golden_python_untyped_receiver() {
    check("python-untyped-receiver");
}

// TypeScript idiom fixtures mined from the prototype changelog.
#[test]
fn golden_typescript_loop_var_shadow() {
    check("typescript-loop-var-shadow");
}

#[test]
fn golden_typescript_catch_shadow() {
    check("typescript-catch-shadow");
}

#[test]
fn golden_typescript_arrow_param_shadow() {
    check("typescript-arrow-param-shadow");
}

#[test]
fn golden_typescript_nested_function() {
    check("typescript-nested-function");
}

#[test]
fn golden_typescript_arrow_const() {
    check("typescript-arrow-const");
}

#[test]
fn golden_typescript_class_arrow_property() {
    check("typescript-class-arrow-property");
}

#[test]
fn golden_typescript_default_export() {
    check("typescript-default-export");
}

#[test]
fn golden_typescript_barrel_reexport() {
    check("typescript-barrel-reexport");
}

#[test]
fn golden_typescript_method_collision() {
    check("typescript-method-collision");
}

#[test]
fn golden_typescript_aliased_import() {
    check("typescript-aliased-import");
}

#[test]
fn golden_typescript_dynamic_import() {
    check("typescript-dynamic-import");
}

#[test]
fn golden_go_same_package_xfile() {
    check("go-same-package-xfile");
}

#[test]
fn golden_go_aliased_import() {
    check("go-aliased-import");
}

#[test]
fn golden_go_dot_import() {
    check("go-dot-import");
}

#[test]
fn golden_go_shadowed_pkg_name() {
    check("go-shadowed-pkg-name");
}

#[test]
fn golden_go_case_distinct() {
    check("go-case-distinct");
}

#[test]
fn golden_go_builtin_calls() {
    check("go-builtin-calls");
}

#[test]
fn golden_go_method_receivers() {
    check("go-method-receivers");
}

#[test]
fn golden_go_embedded_struct() {
    check("go-embedded-struct");
}

#[test]
fn golden_go_qualified_type() {
    check("go-qualified-type");
}

#[test]
fn golden_rust_use_alias() {
    check("rust-use-alias");
}

#[test]
fn golden_rust_pub_use_reexport() {
    check("rust-pub-use-reexport");
}

#[test]
fn golden_rust_trait_vs_inherent() {
    check("rust-trait-vs-inherent");
}

#[test]
fn golden_rust_mod_hierarchy() {
    check("rust-mod-hierarchy");
}

#[test]
fn golden_rust_relative_paths() {
    check("rust-relative-paths");
}

#[test]
fn golden_rust_shadowing_let() {
    check("rust-shadowing-let");
}

#[test]
fn golden_rust_struct_fn_same_name() {
    check("rust-struct-fn-same-name");
}

#[test]
fn golden_rust_multi_impl() {
    check("rust-multi-impl");
}
#[test]
fn golden_rust_self_method_call() {
    check("rust-self-method-call");
}

#[test]
fn golden_rust_macro_generated() {
    check("rust-macro-generated");
}

#[test]
fn golden_rust_same_name_modules() {
    check("rust-same-name-modules");
}

#[test]
fn golden_bash_basic() {
    check("bash-basic");
}

#[test]
fn golden_bash_dirname_source() {
    check("bash-dirname-source");
}

#[test]
fn golden_cpp_basic() {
    check("cpp-basic");
}

#[test]
fn golden_cpp_header_impl() {
    check("cpp-header-impl");
}

#[test]
fn golden_cpp_unreal_macros() {
    check("cpp-unreal-macros");
}

#[test]
fn golden_python_docstring() {
    check("python-docstring");
}

/// Every fixture directory must be registered as a test — an unregistered
/// fixture is silent coverage loss (D16 audit follow-up).
#[test]
fn all_fixtures_registered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/golden/fixtures");
    let source = include_str!("golden.rs");
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            source.contains(&format!("check(\"{name}\")")),
            "fixture {name} has no golden_* test registered"
        );
    }
}

#[test]
fn golden_proto_basic() {
    check("proto-basic");
}

/// tonic convention: proto rpc binds to the hand-written `impl S for T`
/// method by `declared` evidence, in both directions.
#[test]
fn golden_proto_tonic_service() {
    check("proto-tonic-service");
}

/// Mined from a real proto corpus: imports are include-root-relative
/// ("acme/v1/money.proto" under schema/proto/), and sibling files of one
/// package reference each other's messages bare, via oneof branches and
/// map value types.
#[test]
fn golden_proto_include_root() {
    check("proto-include-root");
}

/// Mined from a real Rust workspace: Cargo.toml declares the crate name
/// (`acme-util` -> `acme_util`) that cross-crate `use` paths say, while
/// the directory says `crates/util` — a naming root only the manifest
/// reveals. Also pins `crate::` self-alias translation.
#[test]
fn golden_rust_workspace_crates() {
    check("rust-workspace-crates");
}

/// Associated items through a crate-qualified path: `acme_util::Config::new()`
/// is path-to-type then member, not module-to-leaf.
#[test]
fn golden_rust_crate_assoc_items() {
    check("rust-crate-assoc-items");
}

/// Cross-crate re-export: lib.rs `pub use masks::apply_mask` (uniform
/// path, relative to the crate root) makes `acme_util::apply_mask`
/// public; the consumer's call must chain through it.
#[test]
fn golden_rust_crate_reexport() {
    check("rust-crate-reexport");
}

#[test]
fn golden_rust_mod_sibling_call() {
    check("rust-mod-sibling-call");
}

/// Dynamic dispatch: a trait with two impls and a caller through the
/// trait. Extraction records the trait-impl headers as `uses` references.
#[test]
fn golden_rust_dyn_dispatch() {
    check("rust-dyn-dispatch");
}

/// C basics: functions (definitions and a prototype), struct/enum/union/
/// typedef, function-like and object-like macros, quoted vs angle
/// includes, and a cross-file call through a shared header. The header
/// itself parses under the cpp pack — the .c extension split is
/// deliberate.
#[test]
fn golden_c_basic() {
    check("c-basic");
}

/// Two translation units each define a `static` function with the same
/// name; each caller must see its own file's static, never the sibling's.
#[test]
fn golden_c_static_scope() {
    check("c-static-scope");
}

#[test]
fn golden_javascript_basic() {
    check("javascript-basic");
}

/// CommonJS interop: destructured require binds the item, whole-module
/// require aliases the module for member calls.
#[test]
fn golden_javascript_cjs() {
    check("javascript-cjs");
}

/// JSX: <Button/> in a consumer registers a `uses` reference to the
/// component; lowercase host elements are ignored.
#[test]
fn golden_javascript_jsx() {
    check("javascript-jsx");
}

/// SQL DDL/DML: tables and a foreign-key use; the query file's FROM /
/// JOIN / INSERT / UPDATE targets reference the schema's tables, plus
/// one table that exists nowhere (audit_log).
#[test]
fn golden_sql_basic() {
    check("sql-basic");
}

/// View chain: a view over a table, a query over the view.
#[test]
fn golden_sql_view_chain() {
    check("sql-view-chain");
}

/// C# pack: path-derived, namespace-aligned module identity (directories
/// mirror namespaces; `using Ns;` is a glob import of that directory).
#[test]
fn golden_csharp_basic() {
    check("csharp-basic");
}

#[test]
fn golden_csharp_cross_namespace() {
    check("csharp-cross-namespace");
}

#[test]
fn golden_csharp_static_vs_instance() {
    check("csharp-static-vs-instance");
}

#[test]
fn golden_java_basic() {
    check("java-basic");
}

#[test]
fn golden_java_cross_package() {
    check("java-cross-package");
}

#[test]
fn golden_java_interface_impl() {
    check("java-interface-impl");
}

#[test]
fn golden_java_inheritance() {
    check("java-inheritance");
}

#[test]
fn golden_csharp_inheritance() {
    check("csharp-inheritance");
}

#[test]
fn golden_markdown_headings() {
    check("markdown-headings");
}

/// Every block under a heading (paragraph, list, table, fenced code) is
/// the section body, joined with blank lines; golden rows only see the
/// first line, so the full body is asserted here.
#[test]
fn golden_markdown_table_list() {
    check("markdown-table-list");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/golden/fixtures/markdown-table-list/docs/arch.md");
    let source = std::fs::read_to_string(&path).unwrap();
    let facts = Extractor::new(spec_for_path("docs/arch.md").unwrap())
        .unwrap()
        .extract("docs/arch.md", &source)
        .unwrap();
    let doc = |name: &str| {
        facts
            .nodes
            .iter()
            .find(|n| n.name == name)
            .and_then(|n| n.doc.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        doc("Key Traits"),
        "Some intro paragraph.\n\n- Harness: adjudicates events\n- PolicyEngine: compiles Cedar\n\n| a | b |\n|---|---|\n| 1 | 2 |"
    );
    assert_eq!(doc("Example"), "```rust\nfn main() {}\n```");
    // Parent section's doc stops at its first subheading.
    assert_eq!(doc("Architecture"), "");
}

/// Inline links (secondary inline grammar): destinations become `uses`
/// references; external URLs produce nothing at all.
#[test]
fn golden_markdown_links() {
    check("markdown-links");
}

/// `implements`/interface-`extends` heritage: interface method
/// signatures are symbols; heritage clauses reference the supertype.
#[test]
fn golden_typescript_implements() {
    check("typescript-implements");
}

/// Class bases: the base reference, and methods overriding base methods.
#[test]
fn golden_python_inheritance() {
    check("python-inheritance");
}

/// Implicit interface satisfaction plus the `var _ I = (*T)(nil)`
/// assertion idiom; interface method specs are symbols.
#[test]
fn golden_go_interface() {
    check("go-interface");
}

/// Base-class specifier: reference + virtual-override pairing.
#[test]
fn golden_cpp_inheritance() {
    check("cpp-inheritance");
}

/// go.mod-declared module path anchors full-path imports
/// (`example.com/acme` -> the module root package).
#[test]
fn golden_go_module_import() {
    check("go-module-import");
}

#[test]
fn golden_rust_typed_local_receiver() {
    check("rust-typed-local-receiver");
}

#[test]
fn golden_rust_field_receiver() {
    check("rust-field-receiver");
}

#[test]
fn golden_rust_async_trait_cross_crate() {
    check("rust-async-trait-cross-crate");
}
