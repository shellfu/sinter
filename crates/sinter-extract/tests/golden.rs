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
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Some(spec) = spec_for_path(&rel) else {
            continue; // non-source fixture support file (go.mod, ...)
        };
        let source = std::fs::read_to_string(&path).unwrap();
        let facts = Extractor::new(spec)
            .unwrap()
            .extract(&rel, &source)
            .unwrap();
        assert!(!facts.has_syntax_errors, "fixture {rel} has syntax errors");
        for n in &facts.nodes {
            nodes.insert(vec![
                n.kind.as_str().to_string(),
                qualified_of(n.id.as_str()),
                n.file.clone(),
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

fn precision_recall(found: &BTreeSet<Tuple>, expected: &BTreeSet<Tuple>) -> (f64, f64) {
    let hit = found.intersection(expected).count() as f64;
    let p = if found.is_empty() {
        1.0
    } else {
        hit / found.len() as f64
    };
    let r = if expected.is_empty() {
        1.0
    } else {
        hit / expected.len() as f64
    };
    (p, r)
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
        let (p, r) = precision_recall(found, expected);
        println!("{fixture}/{category}: precision {p:.3} recall {r:.3}");
        for missing in expected.difference(found) {
            println!("  MISSING  {missing:?}");
            ok = false;
        }
        for extra in found.difference(expected) {
            println!("  EXTRA    {extra:?}");
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

// TypeScript idiom fixtures mined from the the prototype prototype changelog.
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
