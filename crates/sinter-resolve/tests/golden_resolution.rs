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
        let rel = sinter_core::rel_display(path.strip_prefix(root).unwrap());
        assert!(!rel.contains('\\'), "path separator leaked into {rel}");
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

    // Found tuples are fully qualified: [evidence, relation, src, dst,
    // src_file, dst_file]. Expected tuples may be the 4-element legacy form
    // (prefix match) or the full 6-element form — same-named symbols in
    // different files need the latter to be distinguishable (D16).
    let file_of = |id: &str| id.split_once('#').map_or(id, |(f, _)| f).to_string();
    let found: BTreeSet<Tuple> = bindings
        .iter()
        .map(|b| {
            vec![
                b.edge.evidence.as_str().to_string(),
                b.edge.relation.as_str().to_string(),
                qualified_of(b.edge.src.as_str()).to_string(),
                qualified_of(b.edge.dst.as_str()).to_string(),
                file_of(b.edge.src.as_str()),
                file_of(b.edge.dst.as_str()),
            ]
        })
        .collect();
    let expected_set: BTreeSet<Tuple> = expected.resolved.into_iter().collect();
    let matches = |e: &Tuple, f: &Tuple| f.len() >= e.len() && f[..e.len()] == e[..];

    let missing: Vec<&Tuple> = expected_set
        .iter()
        .filter(|e| !found.iter().any(|f| matches(e, f)))
        .collect();
    let extra: Vec<&Tuple> = found
        .iter()
        .filter(|f| !expected_set.iter().any(|e| matches(e, f)))
        .collect();
    let p = if found.is_empty() {
        1.0
    } else {
        (found.len() - extra.len()) as f64 / found.len() as f64
    };
    let r = if expected_set.is_empty() {
        1.0
    } else {
        (expected_set.len() - missing.len()) as f64 / expected_set.len() as f64
    };
    println!(
        "{fixture}/resolved: precision {p:.3} recall {r:.3}, unresolved {} (expected {}), rate {:.1}%",
        stats.unresolved(),
        expected.unresolved_count,
        stats.unresolved_rate() * 100.0
    );
    let mut ok = true;
    for m in &missing {
        println!("  MISSING  {m:?}");
        ok = false;
    }
    for e in &extra {
        println!("  EXTRA    {e:?}");
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

// TypeScript idiom fixtures mined from the prototype changelog.
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

#[test]
fn resolution_bash_basic() {
    check("bash-basic");
}

#[test]
fn resolution_bash_dirname_source() {
    check("bash-dirname-source");
}

#[test]
fn resolution_cpp_basic() {
    check("cpp-basic");
}

#[test]
fn resolution_cpp_header_impl() {
    check("cpp-header-impl");
}

#[test]
fn resolution_cpp_unreal_macros() {
    check("cpp-unreal-macros");
}

#[test]
fn resolution_python_docstring() {
    check("python-docstring");
}

/// Every fixture directory must be registered as a test — an unregistered
/// fixture is silent coverage loss (D16 audit follow-up).
#[test]
fn all_fixtures_registered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/golden/fixtures");
    let source = include_str!("golden_resolution.rs");
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            source.contains(&format!("check(\"{name}\")")),
            "fixture {name} has no resolution_* test registered"
        );
    }
}
