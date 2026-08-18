//! Golden-corpus accuracy metric (R7). Precision/recall of extraction
//! against hand-verified expectations; any change that moves the metric
//! fails here with the delta (missing/extra) printed.

use std::collections::BTreeSet;
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

fn extract_fixture(root: &Path) -> (BTreeSet<Tuple>, BTreeSet<Tuple>, BTreeSet<Tuple>) {
    let mut files = Vec::new();
    source_files(root, &mut files);
    let (mut nodes, mut contains, mut references) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
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
            nodes.insert(vec![
                n.kind.as_str().to_string(),
                qualified_of(n.id.as_str()),
                n.file.clone(),
                n.doc
                    .as_deref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
            ]);
        }
        for e in &facts.contains {
            contains.insert(vec![
                qualified_of(e.src.as_str()),
                qualified_of(e.dst.as_str()),
            ]);
        }
        for r in &facts.references {
            references.insert(vec![r.relation.as_str().to_string(), r.name.clone()]);
        }
    }
    (nodes, contains, references)
}

/// Expected tuples may be shorter than found tuples (legacy 3-element node
/// rows vs doc-bearing 4-element): prefix match, like the resolution runner.
fn tuple_matches(expected: &Tuple, found: &Tuple) -> bool {
    found.len() >= expected.len() && found[..expected.len()] == expected[..]
}

fn deltas<'a>(
    found: &'a BTreeSet<Tuple>,
    expected: &'a BTreeSet<Tuple>,
) -> (Vec<&'a Tuple>, Vec<&'a Tuple>, f64, f64) {
    let missing: Vec<&Tuple> = expected
        .iter()
        .filter(|e| !found.iter().any(|f| tuple_matches(e, f)))
        .collect();
    let extra: Vec<&Tuple> = found
        .iter()
        .filter(|f| !expected.iter().any(|e| tuple_matches(e, f)))
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
        ("nodes", &nodes, &expected.nodes.iter().cloned().collect()),
        (
            "contains",
            &contains,
            &expected.contains.iter().cloned().collect(),
        ),
        (
            "references",
            &references,
            &expected.references.iter().cloned().collect(),
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
