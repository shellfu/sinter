//! Resolution accuracy metric (R7): resolved bindings and unresolved rate
//! against hand-verified expectations. A change that moves the metric fails
//! here with the delta printed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sinter_core::{Embed, LocalBinding, Node, Reference, Relation};
use sinter_extract::{Extractor, spec_for_path};
use sinter_resolve::{qualified_of, resolve};

type Tuple = Vec<String>;

#[derive(serde::Deserialize)]
struct Expected {
    resolved: Vec<Tuple>,
    unresolved_count: usize,
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn extract_all(root: &Path) -> (Vec<Node>, Vec<Reference>, Vec<LocalBinding>, Vec<Embed>) {
    let mut files = Vec::new();
    source_files(root, &mut files);
    let (mut nodes, mut references, mut locals, mut embeds) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for path in files {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Some(spec) = spec_for_path(&rel) else {
            continue;
        };
        let source = std::fs::read_to_string(&path).unwrap();
        let facts = Extractor::new(spec)
            .unwrap()
            .extract(&rel, &source)
            .unwrap();
        nodes.extend(facts.nodes);
        references.extend(facts.references);
        locals.extend(facts.locals);
        embeds.extend(facts.embeds);
    }
    (nodes, references, locals, embeds)
}

/// Fixtures with known engine gaps. CI gates on everything else; a listed
/// fixture that STARTS passing also fails, so this list only ever shrinks.
const KNOWN_FAIL: &[&str] = &[];

fn check(fixture: &str) {
    let expected_fail = KNOWN_FAIL.contains(&fixture);
    let result = std::panic::catch_unwind(|| check_inner(fixture));
    match (result.is_ok(), expected_fail) {
        (true, false) => {}
        (false, false) => panic!("{fixture}: resolution metric moved — deltas above"),
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
    let (nodes, references, locals, embeds) = extract_all(&root);
    let all_imports: Vec<Reference> = references
        .iter()
        .filter(|r| r.relation == Relation::Imports)
        .cloned()
        .collect();
    let (bindings, stats, _) = resolve(&nodes, &references, &locals, &all_imports, &embeds);

    let found: BTreeSet<Tuple> = bindings
        .iter()
        .map(|b| {
            vec![
                b.edge.evidence.as_str().to_string(),
                b.edge.relation.as_str().to_string(),
                qualified_of(b.edge.src.as_str()).to_string(),
                qualified_of(b.edge.dst.as_str()).to_string(),
            ]
        })
        .collect();
    let expected_set: BTreeSet<Tuple> = expected.resolved.into_iter().collect();

    let hit = found.intersection(&expected_set).count() as f64;
    let p = if found.is_empty() {
        1.0
    } else {
        hit / found.len() as f64
    };
    let r = if expected_set.is_empty() {
        1.0
    } else {
        hit / expected_set.len() as f64
    };
    println!(
        "{fixture}/resolved: precision {p:.3} recall {r:.3}, unresolved {} (expected {}), rate {:.1}%",
        stats.unresolved(),
        expected.unresolved_count,
        stats.unresolved_rate() * 100.0
    );
    let mut ok = true;
    for missing in expected_set.difference(&found) {
        println!("  MISSING  {missing:?}");
        ok = false;
    }
    for extra in found.difference(&expected_set) {
        println!("  EXTRA    {extra:?}");
        ok = false;
    }
    assert!(ok, "{fixture}: resolution metric moved — deltas above");
    assert_eq!(
        stats.unresolved(),
        expected.unresolved_count,
        "{fixture}: unresolved count moved"
    );
}

#[test]
fn resolution_rust_basic() {
    check("rust-basic");
}

#[test]
fn resolution_go_basic() {
    check("go-basic");
}

#[test]
fn resolution_python_basic() {
    check("python-basic");
}

#[test]
fn resolution_typescript_basic() {
    check("typescript-basic");
}

#[test]
fn resolution_python_relative_import() {
    check("python-relative-import");
}

#[test]
fn resolution_python_alias_import() {
    check("python-alias-import");
}

#[test]
fn resolution_python_init_reexport() {
    check("python-init-reexport");
}

#[test]
fn resolution_python_star_import() {
    check("python-star-import");
}

#[test]
fn resolution_python_decorator() {
    check("python-decorator");
}

#[test]
fn resolution_python_nested_function() {
    check("python-nested-function");
}

#[test]
fn resolution_python_shadowed_param() {
    check("python-shadowed-param");
}

#[test]
fn resolution_python_same_name_disambig() {
    check("python-same-name-disambig");
}

#[test]
fn resolution_python_method_vs_function() {
    check("python-method-vs-function");
}

#[test]
fn resolution_python_untyped_receiver() {
    check("python-untyped-receiver");
}

// TypeScript idiom fixtures mined from the the prototype prototype changelog.
#[test]
fn resolution_typescript_loop_var_shadow() {
    check("typescript-loop-var-shadow");
}

#[test]
fn resolution_typescript_catch_shadow() {
    check("typescript-catch-shadow");
}

#[test]
fn resolution_typescript_arrow_param_shadow() {
    check("typescript-arrow-param-shadow");
}

#[test]
fn resolution_typescript_nested_function() {
    check("typescript-nested-function");
}

#[test]
fn resolution_typescript_arrow_const() {
    check("typescript-arrow-const");
}

#[test]
fn resolution_typescript_default_export() {
    check("typescript-default-export");
}

#[test]
fn resolution_typescript_barrel_reexport() {
    check("typescript-barrel-reexport");
}

#[test]
fn resolution_typescript_method_collision() {
    check("typescript-method-collision");
}

#[test]
fn resolution_typescript_aliased_import() {
    check("typescript-aliased-import");
}

#[test]
fn resolution_typescript_dynamic_import() {
    check("typescript-dynamic-import");
}

#[test]
fn resolution_go_same_package_xfile() {
    check("go-same-package-xfile");
}

#[test]
fn resolution_go_aliased_import() {
    check("go-aliased-import");
}

#[test]
fn resolution_go_dot_import() {
    check("go-dot-import");
}

#[test]
fn resolution_go_shadowed_pkg_name() {
    check("go-shadowed-pkg-name");
}

#[test]
fn resolution_go_case_distinct() {
    check("go-case-distinct");
}

#[test]
fn resolution_go_builtin_calls() {
    check("go-builtin-calls");
}

#[test]
fn resolution_go_method_receivers() {
    check("go-method-receivers");
}

#[test]
fn resolution_go_embedded_struct() {
    check("go-embedded-struct");
}

#[test]
fn resolution_go_qualified_type() {
    check("go-qualified-type");
}

#[test]
fn resolution_rust_use_alias() {
    check("rust-use-alias");
}

#[test]
fn resolution_rust_pub_use_reexport() {
    check("rust-pub-use-reexport");
}

#[test]
fn resolution_rust_trait_vs_inherent() {
    check("rust-trait-vs-inherent");
}

#[test]
fn resolution_rust_mod_hierarchy() {
    check("rust-mod-hierarchy");
}

#[test]
fn resolution_rust_relative_paths() {
    check("rust-relative-paths");
}

#[test]
fn resolution_rust_shadowing_let() {
    check("rust-shadowing-let");
}

#[test]
fn resolution_rust_struct_fn_same_name() {
    check("rust-struct-fn-same-name");
}

#[test]
fn resolution_rust_multi_impl() {
    check("rust-multi-impl");
}

#[test]
fn resolution_rust_macro_generated() {
    check("rust-macro-generated");
}

#[test]
fn resolution_rust_same_name_modules() {
    check("rust-same-name-modules");
}
