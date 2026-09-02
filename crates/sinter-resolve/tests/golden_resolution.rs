//! Resolution accuracy metric (R7): resolved bindings and unresolved rate
//! against hand-verified expectations. A change that moves the metric fails
//! here with the delta printed.

use std::path::{Path, PathBuf};

use sinter_core::{Embed, FieldBinding, LocalBinding, Node, Reference, Relation, TraitImpl};
use sinter_extract::{Extractor, spec_for_path};
use sinter_resolve::{dynamic_edges, qualified_of, resolve};

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

type Extracted = (
    Vec<Node>,
    Vec<Reference>,
    Vec<LocalBinding>,
    Vec<FieldBinding>,
    Vec<Embed>,
    Vec<TraitImpl>,
);

fn extract_all(root: &Path) -> Extracted {
    let mut files = Vec::new();
    source_files(root, &mut files);
    let (mut nodes, mut references, mut locals, mut fields, mut embeds, mut trait_impls) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
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
        fields.extend(facts.fields);
        embeds.extend(facts.embeds);
        trait_impls.extend(facts.trait_impls);
    }
    (nodes, references, locals, fields, embeds, trait_impls)
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

/// Collect package manifests the same way the pipeline does (fixtures
/// are tiny; plain recursion, no ignore rules needed).
fn walk_manifests(root: &Path) -> Vec<sinter_extract::ModuleRoot> {
    fn walk(dir: &Path, top: &Path, out: &mut Vec<sinter_extract::ModuleRoot>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, top, out);
            } else if let (Ok(rel), Ok(content)) =
                (path.strip_prefix(top), std::fs::read_to_string(&path))
            {
                let rel = rel.to_string_lossy().replace('\\', "/");
                out.extend(sinter_extract::manifest_root(&rel, &content));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| (&a.dir, &a.name).cmp(&(&b.dir, &b.name)));
    out
}

fn check_inner(fixture: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/golden/fixtures")
        .join(fixture);
    let expected: Expected =
        serde_json::from_str(&std::fs::read_to_string(root.join("expected.json")).unwrap())
            .unwrap();
    let (nodes, references, locals, fields, embeds, trait_impls) = extract_all(&root);
    let all_imports: Vec<Reference> = references
        .iter()
        .filter(|r| r.relation == Relation::Imports)
        .cloned()
        .collect();
    let roots: Vec<sinter_extract::ModuleRoot> = walk_manifests(&root);
    let index =
        sinter_resolve::Index::build(&nodes, &all_imports, &locals, &fields, &embeds, &roots);
    let (bindings, stats, _, _) = resolve(&index, &references);
    let dynamic = dynamic_edges(&index, &nodes, &trait_impls);

    // Every resolved binding carries its call site — the span of the
    // reference it bound (the "at file:line" answer for query verbs).
    for b in &bindings {
        assert_eq!(
            b.edge.site,
            Some(references[b.reference].span),
            "{fixture}: binding {} -> {} lost its call site",
            b.edge.src,
            b.edge.dst,
        );
    }

    // Found tuples are fully qualified: [evidence, relation, src, dst,
    // src_file, dst_file, site_start, site_end]. Expected tuples may use
    // a shorter prefix, but the full 8-element form distinguishes both
    // same-named symbols and repeated call sites (D16).
    let file_of = |id: &str| id.split_once('#').map_or(id, |(f, _)| f).to_string();
    let mut found: Vec<Tuple> = bindings
        .iter()
        .map(|b| &b.edge)
        .chain(dynamic.iter())
        .map(|edge| {
            vec![
                edge.evidence.as_str().to_string(),
                edge.relation.as_str().to_string(),
                qualified_of(edge.src.as_str()).to_string(),
                qualified_of(edge.dst.as_str()).to_string(),
                file_of(edge.src.as_str()),
                file_of(edge.dst.as_str()),
                edge.site
                    .map(|site| site.start.to_string())
                    .unwrap_or_default(),
                edge.site
                    .map(|site| site.end.to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    found.sort();
    let mut expected_rows = expected.resolved;
    expected_rows.sort();
    let matches = |e: &Tuple, f: &Tuple| f.len() >= e.len() && f[..e.len()] == e[..];

    let mut used = vec![false; found.len()];
    let mut missing = Vec::new();
    for expected_row in &expected_rows {
        if let Some((index, _)) = found
            .iter()
            .enumerate()
            .find(|(index, found_row)| !used[*index] && matches(expected_row, found_row))
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
    let r = if expected_rows.is_empty() {
        1.0
    } else {
        (expected_rows.len() - missing.len()) as f64 / expected_rows.len() as f64
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
fn resolution_typescript_class_arrow_property() {
    check("typescript-class-arrow-property");
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
fn resolution_rust_self_method_call() {
    check("rust-self-method-call");
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

#[test]
fn resolution_proto_basic() {
    check("proto-basic");
}

/// tonic convention: proto rpc binds to the hand-written `impl S for T`
/// method by `declared` evidence, in both directions.
#[test]
fn resolution_proto_tonic_service() {
    check("proto-tonic-service");
}

/// Mined from a real proto corpus: imports are include-root-relative
/// ("acme/v1/money.proto" under schema/proto/), and sibling files of one
/// package reference each other's messages bare, via oneof branches and
/// map value types.
#[test]
fn resolution_proto_include_root() {
    check("proto-include-root");
}

/// Mined from a real Rust workspace: Cargo.toml declares the crate name
/// (`acme-util` -> `acme_util`) that cross-crate `use` paths say, while
/// the directory says `crates/util` — a naming root only the manifest
/// reveals. Also pins `crate::` self-alias translation.
#[test]
fn resolution_rust_workspace_crates() {
    check("rust-workspace-crates");
}

/// Associated items through a crate-qualified path: `acme_util::Config::new()`
/// is path-to-type then member, not module-to-leaf.
#[test]
fn resolution_rust_crate_assoc_items() {
    check("rust-crate-assoc-items");
}

/// Cross-crate re-export: lib.rs `pub use masks::apply_mask` (uniform
/// path, relative to the crate root) makes `acme_util::apply_mask`
/// public; the consumer's call must chain through it.
#[test]
fn resolution_rust_crate_reexport() {
    check("rust-crate-reexport");
}

#[test]
fn resolution_rust_mod_sibling_call() {
    check("rust-mod-sibling-call");
}

/// Dynamic dispatch: the call binds to the trait method, and dynamic
/// fan-out edges `trait_method -> impl_method` carry `dynamic` evidence
/// for every impl — the conservative blast-radius bridge.
#[test]
fn resolution_rust_dyn_dispatch() {
    check("rust-dyn-dispatch");
}

/// Cross-pack resolution: foo.c's call binds through `#include "bar.h"`
/// (bar.h and bar.c share module path ["bar"]) to the definition in
/// bar.c; the angle include stays an unresolved external.
#[test]
fn resolution_c_basic() {
    check("c-basic");
}

/// Same-named statics in two files: each call resolves within its own
/// file's scope.
#[test]
fn resolution_c_static_scope() {
    check("c-static-scope");
}

#[test]
fn resolution_javascript_basic() {
    check("javascript-basic");
}

/// CommonJS interop: destructured require binds the item, whole-module
/// require aliases the module for member calls.
#[test]
fn resolution_javascript_cjs() {
    check("javascript-cjs");
}

/// JSX: <Button/> in a consumer registers a `uses` reference to the
/// component; lowercase host elements are ignored.
#[test]
fn resolution_javascript_jsx() {
    check("javascript-jsx");
}

/// SQL: all .sql files in a directory share one namespace (sql_module_path
/// keys on the directory), so queries.sql table refs bind to schema.sql
/// definitions with scope evidence; audit_log stays unresolved.
#[test]
fn resolution_sql_basic() {
    check("sql-basic");
}

/// View chain: view -> table and query -> view both bind by scope.
#[test]
fn resolution_sql_view_chain() {
    check("sql-view-chain");
}

/// Database-root namespace: migrations/ defines, queries/ reads — both
/// strip to the same root, so the read binds across directories.
#[test]
fn resolution_sql_cross_dir() {
    check("sql-cross-dir");
}

/// Two database roots each define `users`: reads inside a root bind to
/// that root's table; a read outside any root hits the repo-wide fallback,
/// finds two candidates, and stays unresolved (never a guess).
#[test]
fn resolution_sql_two_roots() {
    check("sql-two-roots");
}

/// C# pack: path-derived, namespace-aligned module identity (directories
/// mirror namespaces; `using Ns;` is a glob import of that directory).
#[test]
fn resolution_csharp_basic() {
    check("csharp-basic");
}

#[test]
fn resolution_csharp_cross_namespace() {
    check("csharp-cross-namespace");
}

/// Static call and `this.` receiver resolve; an untyped instance receiver
/// (`var` local) is pinned unresolved — never a wrong bind.
#[test]
fn resolution_csharp_static_vs_instance() {
    check("csharp-static-vs-instance");
}

#[test]
fn resolution_java_basic() {
    check("java-basic");
}

#[test]
fn resolution_java_cross_package() {
    check("java-cross-package");
}

#[test]
fn resolution_java_interface_impl() {
    check("java-interface-impl");
}

#[test]
fn resolution_java_inheritance() {
    check("java-inheritance");
}

#[test]
fn resolution_csharp_inheritance() {
    check("csharp-inheritance");
}

#[test]
fn resolution_markdown_headings() {
    check("markdown-headings");
}

#[test]
fn resolution_markdown_table_list() {
    check("markdown-table-list");
}

/// Link edges: section -> file (sibling, subdir, extensionless),
/// section -> section (`#fragment`, same- and cross-file via heading
/// slugs); a dead link is the one unresolved ref, an external URL is
/// nothing at all.
#[test]
fn resolution_markdown_links() {
    check("markdown-links");
}

/// Implements/extends edges and dynamic fan-out through a TS interface.
#[test]
fn resolution_typescript_implements() {
    check("typescript-implements");
}

/// Extends edge and dynamic fan-out from base method to override.
#[test]
fn resolution_python_inheritance() {
    check("python-inheritance");
}

/// Structural method-set satisfaction: dynamic fan-out and an
/// implements edge with no naming syntax at all.
#[test]
fn resolution_go_interface() {
    check("go-interface");
}

/// Extends edge and virtual-override fan-out through an #include.
#[test]
fn resolution_cpp_inheritance() {
    check("cpp-inheritance");
}

/// Module-path import binds through the go.mod manifest root.
#[test]
fn resolution_go_module_import() {
    check("go-module-import");
}

#[test]
fn resolution_rust_typed_local_receiver() {
    check("rust-typed-local-receiver");
}

#[test]
fn resolution_rust_field_receiver() {
    check("rust-field-receiver");
}

#[test]
fn resolution_rust_async_trait_cross_crate() {
    check("rust-async-trait-cross-crate");
}
