//! Evidence-based reference resolution. Tiers, strongest local knowledge
//! first: receiver binding, typed-local binding, shadow suppression,
//! same-file/same-module scope, then import evidence (aliases, globs,
//! re-export chains, relative paths). Exactly one candidate or nothing —
//! ambiguity is unresolved, never a guess.

use std::collections::HashMap;

use sinter_core::{
    Confidence, Edge, Embed, Evidence, FieldBinding, LocalBinding, Node, NodeId, Reference,
    Relation, SymbolKind, TraitImpl,
};
use sinter_extract::{LanguageSpec, ModuleRoot, spec_for_path};

pub struct Binding {
    pub edge: Edge,
    /// Index into the references slice passed to [`resolve`].
    pub reference: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionStats {
    pub scope: usize,
    pub import: usize,
    pub scip: usize,
    /// Corpus-anchored misses subsequently resolved by compiler evidence.
    /// This is a subset of `scip`/`scip_external`, retained so the anchored
    /// miss denominator does not absorb compiler hits the heuristic had
    /// classified as external.
    pub compiler_rescued_internal: usize,
    /// Evidence pointed into the corpus but binding failed (ambiguity,
    /// member missing on a known module/type). This is an anchored miss,
    /// not a complete accuracy measure: the anchoring heuristic can still
    /// classify a compiler-resolvable corpus reference as external.
    pub unresolved_internal: usize,
    /// No corpus-anchored evidence: external imports, builtins, and
    /// value-receiver calls without type evidence. Dependency-index (SCIP)
    /// territory, not a resolver defect.
    pub unresolved_external: usize,
    /// References bound by both internal evidence and SCIP, split by
    /// whether the two agreed on the target — the measured trust level
    /// of non-scip edges.
    pub scip_agree: usize,
    pub scip_disagree: usize,
    /// Refs bound to synthesized dependency-surface nodes (D29). Counted
    /// apart from `scip` and excluded from the cross-check and recall
    /// denominators: internal evidence can never find a symbol with no
    /// in-corpus definition, so mixing these in would fake a regression.
    pub scip_external: usize,
    /// Edges from SCIP occurrences no extracted reference anchors (macro
    /// token trees). Not references, so outside every rate denominator.
    pub scip_unanchored: usize,
}

impl ResolutionStats {
    pub fn resolved(&self) -> usize {
        self.scope + self.import + self.scip + self.scip_external
    }

    pub fn unresolved(&self) -> usize {
        self.unresolved_internal + self.unresolved_external
    }

    pub fn unresolved_rate(&self) -> f64 {
        let total = self.resolved() + self.unresolved();
        if total == 0 {
            0.0
        } else {
            self.unresolved() as f64 / total as f64
        }
    }

    /// Anchored unresolved references over references the heuristic itself
    /// classified as corpus-anchored. This is useful without a compiler
    /// index, but it is not recall: compiler evidence can prove that some
    /// references classified as external were actually internal.
    ///
    /// `None` means no references were resolved in this pass. Reporting
    /// that state as 0% would make a no-op build look perfectly accurate.
    pub fn anchored_unresolved_rate(&self) -> Option<f64> {
        let total =
            self.scope + self.import + self.compiler_rescued_internal + self.unresolved_internal;
        if total == 0 {
            None
        } else {
            Some(self.unresolved_internal as f64 / total as f64)
        }
    }
}

/// Per-reference resolution verdict.
enum Res {
    Bound(Binding),
    /// Anchored in the corpus, unbound; `dangling` when the path's module
    /// exists but has no such member.
    Internal {
        dangling: bool,
    },
    External,
}

/// `{file}#{qualified}@{start}` -> qualified; plain file ids map to themselves.
pub fn qualified_of(id: &str) -> &str {
    match id.split_once('#') {
        Some((_, rest)) => rest.rsplit_once('@').map_or(rest, |(q, _)| q),
        None => id,
    }
}

/// Kinds a "call" landing on means conversion/use, and namespace_pick
/// prefers for Uses. Class is deliberately absent: instantiation really is
/// a call (D14).
fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
            | SymbolKind::TypeAlias
            | SymbolKind::Table
            | SymbolKind::View
    )
}

/// Kinds that can own members for typed-local/receiver lookup — Class
/// included here (a C++ local typed as a class binds its methods;
/// fixture: cpp-header-impl).
fn is_member_scope(kind: SymbolKind) -> bool {
    is_type_kind(kind) || kind == SymbolKind::Class
}

fn is_callable(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro | SymbolKind::Class
    )
}

struct ModuleFiles<'a> {
    key: Vec<String>,
    files: Vec<&'a str>,
}

struct LocalRange<'a> {
    start: u64,
    scope_end: u64,
    type_name: Option<&'a str>,
}

struct Import {
    segments: Vec<String>,
    /// Locally bound name: alias, or the last path segment.
    binding: String,
    /// Dot/star import: binds every top-level name of the module.
    glob: bool,
}

struct FileDef<'a> {
    node: &'a Node,
    /// Qualified prefix ("Server" for Server::run; "" for top level).
    prefix: &'a str,
    /// Every ancestor on the prefix is function-like, so the name is
    /// lexically visible bare inside them (nested fns yes, methods no).
    functionish: bool,
}

/// Prebuilt lookup structures over one corpus snapshot. Built once per
/// resolution pass and shared by [`resolve`] and [`dynamic_edges`] — the
/// build walks every node and is the most expensive part of a pass.
pub struct Index<'a> {
    /// (file, plain name) -> defs with visibility info.
    by_file_name: HashMap<(&'a str, &'a str), Vec<FileDef<'a>>>,
    /// (file, qualified) -> def, receiver/type lookups.
    by_file_qualified: HashMap<(&'a str, &'a str), &'a Node>,
    /// exact file path -> file node (includes naming a literal repo file).
    file_nodes: HashMap<&'a str, &'a Node>,
    /// file -> its non-file defs, for fragment-slug lookup (file_refs).
    defs_by_file: HashMap<&'a str, Vec<&'a Node>>,
    /// name -> (absolute module segments, def).
    by_name: HashMap<&'a str, Vec<(Vec<String>, &'a Node)>>,
    /// last module segment -> (module segments, file node).
    by_module_tail: HashMap<String, Vec<(Vec<String>, &'a Node)>>,
    /// last module segment -> (module segments, files in it) — re-export
    /// chain walking must never scan every module.
    files_of_module: HashMap<String, Vec<ModuleFiles<'a>>>,
    /// module segments -> top-level def name -> defs.
    module_defs: HashMap<Vec<String>, HashMap<&'a str, Vec<&'a Node>>>,
    /// file -> absolutized imports.
    imports: HashMap<&'a str, Vec<Import>>,
    /// (file, name) -> local bindings.
    locals: HashMap<(&'a str, &'a str), Vec<LocalRange<'a>>>,
    /// declaring type node id -> fields with written types.
    fields: HashMap<&'a str, Vec<&'a FieldBinding>>,
    /// owner node id -> embedded type names.
    embeds: HashMap<&'a str, Vec<&'a str>>,
    /// Discovered package roots (manifest-declared name <-> directory).
    roots: Vec<ModuleRoot>,
    /// Proto rpcs, for binding calls on generated (OUT_DIR) clients.
    proto_rpcs: crate::proto_service_bindings::ProtoRpcs<'a>,
}

/// Module key of a file, manifest-aware: under a discovered package
/// root, the key is rooted at the *declared package name* (with the
/// language's self-alias, e.g. Rust's "crate", replaced by it) so that
/// cross-package imports naming the package match. Outside any root the
/// plain module_path applies — single-package repos are unchanged.
fn key_of(spec: &LanguageSpec, roots: &[ModuleRoot], file: &str) -> Vec<String> {
    let Some((manifest, root)) = spec.manifest.zip(root_of(spec, roots, file)) else {
        return (spec.module_path)(file);
    };
    let rel = if root.dir.is_empty() {
        file
    } else {
        &file[root.dir.len() + 1..]
    };
    let mut key = (spec.module_path)(rel);
    // A declared name may span several segments in reference form
    // (Go's `module example.com/proj` vs Rust's single-segment crate
    // name): split it the same way reference paths split.
    let mut name_segments = vec![root.name.clone()];
    for sep in spec.path_separators {
        name_segments = name_segments
            .iter()
            .flat_map(|s| s.split(sep).map(str::to_string))
            .collect();
    }
    name_segments.retain(|s| !s.is_empty());
    match key.first() {
        Some(head) if manifest.self_names.contains(&head.as_str()) => {
            key.splice(0..1, name_segments);
        }
        _ => {
            key.splice(0..0, name_segments);
        }
    }
    key
}

/// Deepest package root containing `file` for this language.
fn root_of<'r>(spec: &LanguageSpec, roots: &'r [ModuleRoot], file: &str) -> Option<&'r ModuleRoot> {
    roots
        .iter()
        .filter(|r| r.language == spec.name)
        .filter(|r| r.dir.is_empty() || file.starts_with(&format!("{}/", r.dir)))
        .max_by_key(|r| r.dir.len())
}

/// Rewrite a reference path's self-alias head ("crate::x") to the
/// enclosing package's declared name, so it matches manifest-aware keys.
fn expand(
    spec: &LanguageSpec,
    roots: &[ModuleRoot],
    file: &str,
    mut segments: Vec<String>,
) -> Vec<String> {
    if let Some(manifest) = spec.manifest
        && let Some(head) = segments.first()
        && manifest.self_names.contains(&head.as_str())
        && let Some(root) = root_of(spec, roots, file)
    {
        segments[0] = root.name.clone();
    }
    segments
}

fn module_of(node: &Node, roots: &[ModuleRoot]) -> Vec<String> {
    let mut module = spec_for_path(&node.file)
        .map(|s| key_of(s, roots, &node.file))
        .unwrap_or_default();
    let qualified = qualified_of(node.id.as_str());
    if let Some((prefix, _)) = qualified.rsplit_once("::") {
        module.extend(prefix.split("::").map(str::to_string));
    }
    module
}

/// Per-node data whose computation is independent of every other node —
/// the expensive half of the index build, computed in parallel.
struct Prep<'a> {
    file_module: Vec<String>,
    qualified: &'a str,
    prefix: &'a str,
    functionish: bool,
    /// file_module + prefix segments, the `by_name` key module.
    module: Vec<String>,
}

fn build_index<'a>(
    nodes: &'a [Node],
    all_imports: &'a [Reference],
    locals: &'a [LocalBinding],
    fields: &'a [FieldBinding],
    embeds: &'a [Embed],
    roots: &[ModuleRoot],
) -> Index<'a> {
    use rayon::prelude::*;
    let mut index = Index {
        by_file_name: HashMap::new(),
        by_file_qualified: HashMap::new(),
        file_nodes: HashMap::new(),
        defs_by_file: HashMap::new(),
        by_name: HashMap::new(),
        by_module_tail: HashMap::new(),
        files_of_module: HashMap::new(),
        module_defs: HashMap::new(),
        imports: HashMap::new(),
        locals: HashMap::new(),
        fields: HashMap::new(),
        embeds: HashMap::new(),
        roots: roots.to_vec(),
        proto_rpcs: crate::proto_service_bindings::ProtoRpcs::build(nodes),
    };
    // Pass 1: qualified -> kind per file, for ancestor-kind checks.
    let mut kind_of: HashMap<(&str, &str), SymbolKind> = HashMap::new();
    for node in nodes {
        kind_of.insert(
            (node.file.as_str(), qualified_of(node.id.as_str())),
            node.kind,
        );
    }
    // Pass 2a, parallel: everything derivable from one node alone —
    // module keys, qualified prefix, lexical visibility — is the hot
    // part of the build (measured on 1.6M-node corpora). Map insertion
    // stays serial below, in node order, so the index is byte-identical
    // to a serial build.
    let preps: Vec<Option<Prep<'a>>> = nodes
        .par_iter()
        .map(|node| {
            let spec = spec_for_path(&node.file)?;
            let file_module = key_of(spec, roots, &node.file);
            if node.kind == SymbolKind::File {
                return Some(Prep {
                    file_module,
                    qualified: "",
                    prefix: "",
                    functionish: false,
                    module: Vec::new(),
                });
            }
            let qualified = qualified_of(node.id.as_str());
            let prefix = qualified.rsplit_once("::").map_or("", |(p, _)| p);
            let functionish =
                prefix
                    .split("::")
                    .filter(|s| !s.is_empty())
                    .try_fold(String::new(), |acc, seg| {
                        let q = if acc.is_empty() {
                            seg.to_string()
                        } else {
                            format!("{acc}::{seg}")
                        };
                        let kind = kind_of.get(&(node.file.as_str(), q.as_str()));
                        match kind {
                            Some(k) if is_callable(*k) && *k != SymbolKind::Class => Some(q),
                            None => None, // impl/receiver scope: not lexically callable
                            Some(_) => None,
                        }
                    });
            let mut module = file_module.clone();
            if !prefix.is_empty() {
                module.extend(prefix.split("::").map(str::to_string));
            }
            Some(Prep {
                file_module,
                qualified,
                prefix,
                functionish: prefix.is_empty() || functionish.is_some(),
                module,
            })
        })
        .collect();
    // Pass 2b, serial: insert in node order.
    for (node, prep) in nodes.iter().zip(preps) {
        let Some(prep) = prep else {
            continue;
        };
        let file_module = prep.file_module;
        if node.kind == SymbolKind::File {
            index.file_nodes.insert(node.file.as_str(), node);
            if let Some(tail) = file_module.last() {
                index
                    .by_module_tail
                    .entry(tail.clone())
                    .or_default()
                    .push((file_module.clone(), node));
                let entries = index.files_of_module.entry(tail.clone()).or_default();
                match entries.iter_mut().find(|m| m.key == file_module) {
                    Some(m) => m.files.push(&node.file),
                    None => entries.push(ModuleFiles {
                        key: file_module,
                        files: vec![&node.file],
                    }),
                }
            }
            continue;
        }
        index
            .by_file_qualified
            .insert((node.file.as_str(), prep.qualified), node);
        index
            .defs_by_file
            .entry(node.file.as_str())
            .or_default()
            .push(node);
        index
            .by_file_name
            .entry((node.file.as_str(), node.name.as_str()))
            .or_default()
            .push(FileDef {
                node,
                prefix: prep.prefix,
                functionish: prep.functionish,
            });
        index
            .by_name
            .entry(node.name.as_str())
            .or_default()
            .push((prep.module, node));
        if prep.prefix.is_empty() {
            index
                .module_defs
                .entry(file_module)
                .or_default()
                .entry(node.name.as_str())
                .or_default()
                .push(node);
        }
    }
    for r in all_imports {
        let Some(spec) = spec_for_path(&r.file) else {
            continue;
        };
        let glob = matches!(r.alias.as_deref(), Some("*") | Some("."));
        let raw = strip_glob(&r.name);
        let segments = expand(spec, roots, &r.file, (spec.absolutize)(raw, &r.file));
        let binding = match (&r.alias, glob) {
            (Some(alias), false) => alias.clone(),
            _ => segments.last().cloned().unwrap_or_default(),
        };
        index
            .imports
            .entry(r.file.as_str())
            .or_default()
            .push(Import {
                segments,
                binding,
                glob,
            });
    }
    for l in locals {
        index
            .locals
            .entry((l.file.as_str(), l.name.as_str()))
            .or_default()
            .push(LocalRange {
                start: l.span.start,
                scope_end: l.scope_end,
                type_name: l.type_name.as_deref(),
            });
    }
    for field in fields {
        index
            .fields
            .entry(field.owner.as_str())
            .or_default()
            .push(field);
    }
    for e in embeds {
        index
            .embeds
            .entry(e.owner.as_str())
            .or_default()
            .push(&e.type_name);
    }
    index
}

fn strip_glob(name: &str) -> &str {
    name.strip_suffix('*')
        .map(|s| s.trim_end_matches(['.', ':', '/']))
        .unwrap_or(name)
}

impl<'a> Index<'a> {
    /// Build the lookup index once; [`resolve`] and [`dynamic_edges`]
    /// both borrow it, so one pass never builds it twice.
    pub fn build(
        nodes: &'a [Node],
        all_imports: &'a [Reference],
        locals: &'a [LocalBinding],
        fields: &'a [FieldBinding],
        embeds: &'a [Embed],
        roots: &[ModuleRoot],
    ) -> Index<'a> {
        let t = std::time::Instant::now();
        let index = build_index(nodes, all_imports, locals, fields, embeds, roots);
        if std::env::var_os("SINTER_TIMING").is_some() {
            eprintln!("index build: {:?}", t.elapsed());
        }
        index
    }

    /// Local binding in scope at `at`, returning its declared type if any.
    fn local_at(&self, file: &str, name: &str, at: u64) -> Option<Option<&'a str>> {
        self.locals
            .get(&(file, name))
            .into_iter()
            .flatten()
            .filter(|l| l.start <= at && at < l.scope_end)
            .map(|l| l.type_name)
            .next_back()
    }

    /// A type definition visible from `file`: same file, then same module.
    fn type_def(&self, file: &str, module: &[String], name: &str) -> Option<&'a Node> {
        let same_file: Vec<&Node> = self
            .by_file_name
            .get(&(file, name))
            .into_iter()
            .flatten()
            .filter(|d| is_member_scope(d.node.kind))
            .map(|d| d.node)
            .collect();
        if let [node] = same_file.as_slice() {
            return Some(node);
        }
        let in_module: Vec<&Node> = self
            .module_defs
            .get(module)
            .and_then(|m| m.get(name))
            .into_iter()
            .flatten()
            .filter(|n| is_member_scope(n.kind))
            .copied()
            .collect();
        match in_module.as_slice() {
            [node] => Some(node),
            _ => None,
        }
    }

    /// Resolve a written type through wrappers and named imports. The
    /// extractor intentionally preserves source spelling; this tier turns
    /// `&Dog` and `Arc<dyn Harness>` into corpus type candidates without
    /// claiming that arbitrary expressions have known types.
    fn visible_types(&self, file: &str, module: &[String], written: &str) -> Vec<&'a Node> {
        let mut found = Vec::new();
        for candidate in type_candidates(written) {
            if let Some(node) = self.type_def(file, module, candidate) {
                found.push(node);
                continue;
            }
            let imported: Vec<&Node> = self
                .imports
                .get(file)
                .into_iter()
                .flatten()
                .filter(|imp| !imp.glob && imp.binding == candidate)
                .filter_map(|imp| self.resolve_path_defs(&imp.segments, 4))
                .filter(|n| is_member_scope(n.kind))
                .collect();
            if let [node] = imported.as_slice() {
                found.push(*node);
            }
        }
        found.sort_by_key(|node| node.id.as_str());
        found.dedup_by_key(|node| node.id.as_str());
        found
    }

    /// Resolve a member through a written receiver type. Multi-trait
    /// objects (`dyn Read + Seek`) bind only when exactly one visible trait
    /// owns the member; ambiguity remains unresolved.
    fn member_of_written_type(
        &self,
        file: &str,
        module: &[String],
        written: &str,
        member: &str,
    ) -> (Option<&'a Node>, bool) {
        let types = self.visible_types(file, module, written);
        let mut members: Vec<&Node> = types
            .iter()
            .filter_map(|ty| self.member_of(ty, member, 4))
            .collect();
        members.sort_by_key(|node| node.id.as_str());
        members.dedup_by_key(|node| node.id.as_str());
        let target = match members.as_slice() {
            [member] => Some(*member),
            _ => None,
        };
        (target, !types.is_empty())
    }

    fn field(&self, owner: &Node, name: &str) -> Option<&'a FieldBinding> {
        let matching: Vec<&FieldBinding> = self
            .fields
            .get(owner.id.as_str())
            .into_iter()
            .flatten()
            .filter(|f| f.name == name)
            .copied()
            .collect();
        match matching.as_slice() {
            [field] => Some(*field),
            _ => None,
        }
    }

    /// Member `name` of type `ty`, following embedded types.
    fn member_of(&self, ty: &'a Node, name: &str, depth: usize) -> Option<&'a Node> {
        if depth == 0 {
            return None;
        }
        let mut module = module_of(ty, &self.roots);
        module.extend(
            qualified_of(ty.id.as_str())
                .rsplit("::")
                .next()
                .map(str::to_string),
        );
        let direct: Vec<&Node> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(m, _)| *m == module)
            .map(|(_, n)| *n)
            .collect();
        if let [node] = direct.as_slice() {
            return Some(node);
        }
        // Header/impl pairs declare and define the same member in one
        // module: the declaration inside the type's own file IS the
        // entity (fixture: cpp-header-impl).
        let in_type_file: Vec<&Node> = direct
            .iter()
            .filter(|n| n.file == ty.file)
            .copied()
            .collect();
        if let [node] = in_type_file.as_slice() {
            return Some(node);
        }
        let spec = spec_for_path(&ty.file)?;
        let file_module = key_of(spec, &self.roots, &ty.file);
        for embedded in self.embeds.get(ty.id.as_str()).into_iter().flatten() {
            if let Some(embedded_ty) = self.type_def(&ty.file, &file_module, embedded)
                && let Some(node) = self.member_of(embedded_ty, name, depth - 1)
            {
                return Some(node);
            }
        }
        None
    }

    /// Does this path point at anything in the corpus (module suffix
    /// match or a same-named module part), regardless of unique binding?
    fn anchored(&self, segments: &[String]) -> bool {
        let module_hit = |segs: &[String]| {
            segs.last().is_some_and(|tail| {
                self.files_of_module
                    .get(tail.as_str())
                    .into_iter()
                    .flatten()
                    .any(|m| suffix_len(&m.key, segs).is_some())
                    || self
                        .by_module_tail
                        .get(tail.as_str())
                        .into_iter()
                        .flatten()
                        .any(|(key, _)| suffix_len(key, segs).is_some())
            })
        };
        if module_hit(segments) {
            return true;
        }
        match segments.split_last() {
            Some((_, module)) if !module.is_empty() => module_hit(module),
            _ => false,
        }
    }

    /// Is this absolute path dangling inside the corpus: its module part
    /// is exactly a corpus module whose files neither define nor import
    /// (nor glob-import anything that could supply) the leaf name? The
    /// shape a rename or deletion leaves behind at untouched call sites.
    // ponytail: exact module-key match only; relative `config::get()`
    // paths stay `internal`, widen through the import tier if needed.
    fn missing_internal_target(&self, segments: &[String]) -> bool {
        let Some((name, module)) = segments.split_last() else {
            return false;
        };
        let Some(tail) = module.last() else {
            return false;
        };
        let Some(files) = self
            .files_of_module
            .get(tail.as_str())
            .into_iter()
            .flatten()
            .find(|m| m.key == module)
        else {
            return false;
        };
        if self
            .module_defs
            .get(module)
            .is_some_and(|defs| defs.contains_key(name.as_str()))
        {
            return false;
        }
        !files.files.iter().any(|file| {
            self.imports
                .get(*file)
                .into_iter()
                .flatten()
                .any(|import| import.glob || import.binding == *name)
        })
    }

    /// File node for an import path, matching either containment
    /// direction: Go-style (long import, short module key) or
    /// include-root style (protoc, C headers) where the import resolves
    /// against roots the graph can't see and the file's repo path ends
    /// with it. Import-evidence sites only — a bare qualified reference
    /// must never bind this loosely. Unique or nothing.
    fn import_file(&self, segments: &[String]) -> Option<&'a Node> {
        unique_best(
            self.by_module_tail
                .get(segments.last()?.as_str())
                .into_iter()
                .flatten()
                .filter_map(|(key, node)| {
                    let len = suffix_len(key, segments).or_else(|| suffix_len(segments, key))?;
                    Some((len, *node))
                }),
        )
    }

    /// Resolve absolute segments to a definition or module file node,
    /// following re-export chains up to a small depth.
    fn resolve_path(&self, segments: &[String], depth: usize) -> Option<&'a Node> {
        self.resolve_path_defs(segments, depth).or_else(|| {
            // Module/package: bind to its file node.
            // ponytail: single-file packages only; multi-file packages stay
            // unresolved here — bind-to-all-files when a consumer needs it.
            let files = self
                .by_module_tail
                .get(segments.last()?.as_str())
                .into_iter()
                .flatten()
                .filter_map(|(key, node)| Some((suffix_len(key, segments)?, *node)));
            unique_best(files)
        })
    }

    /// Like [`resolve_path`] but definitions only — a qualified call or
    /// use must never bind to an unrelated module *file* through the
    /// loose tail fallback (a Rust `hooks::install()` once bound to a
    /// bash `install.sh` this way); the file fallback is import-context
    /// evidence.
    fn resolve_path_defs(&self, segments: &[String], depth: usize) -> Option<&'a Node> {
        if segments.is_empty() || depth == 0 {
            return None;
        }
        if let Some((name, module)) = segments.split_last() {
            let defs = self
                .by_name
                .get(name.as_str())
                .into_iter()
                .flatten()
                .filter_map(|(key, node)| Some((suffix_len(key, module)?, *node)));
            if let Some(node) = unique_best(defs) {
                return Some(node);
            }
            // Re-export chain: the module part names files that re-export
            // this name — follow their imports.
            if !module.is_empty() {
                let mut chained: Vec<&Node> = Vec::new();
                let tail = module.last().map(String::as_str).unwrap_or("");
                for m in self.files_of_module.get(tail).into_iter().flatten() {
                    if suffix_len(&m.key, module).is_none() {
                        continue;
                    }
                    for file in &m.files {
                        for import in self.imports.get(*file).into_iter().flatten() {
                            if import.binding == *name && !import.glob {
                                chained.extend(self.resolve_path(&import.segments, depth - 1));
                            } else if import.glob {
                                let mut deeper = import.segments.clone();
                                deeper.push(name.clone());
                                chained.extend(self.resolve_path(&deeper, depth - 1));
                            }
                        }
                    }
                }
                chained.sort_by_key(|n| n.id.as_str().to_string());
                chained.dedup_by_key(|n| n.id.as_str().to_string());
                if let [node] = chained.as_slice() {
                    return Some(node);
                }
            }
        }
        None
    }
}

/// Plausible type identifiers, inner-most first. Resolution still requires
/// a unique corpus definition, so a generic with several type arguments
/// remains unresolved unless exactly one candidate owns the requested
/// member.
const TYPE_KEYWORDS: &[&str] = &[
    "dyn", "impl", "mut", "const", "ref", "crate", "self", "super", "std", "core", "alloc",
];

fn type_tokens(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|token| {
            !token.is_empty()
                && !token.chars().next().is_some_and(char::is_numeric)
                && !TYPE_KEYWORDS.contains(token)
        })
}

fn type_candidates(written: &str) -> Vec<&str> {
    // These wrappers implement transparent receiver dereference. Containers
    // such as Option/Result/Vec/Mutex deliberately stay outer types: binding
    // their method calls to the element type would create false edges.
    const DEREF_WRAPPERS: &[&str] = &["Box", "Arc", "Rc", "Pin", "Cow"];
    let (head_text, arguments) = written
        .split_once('<')
        .map_or((written, None), |(head, rest)| (head, Some(rest)));
    let head = type_tokens(head_text).next_back();
    if let Some(head) = head
        && !DEREF_WRAPPERS.contains(&head)
    {
        return vec![head];
    }
    let mut out = Vec::new();
    for token in type_tokens(arguments.unwrap_or(written)).rev() {
        if DEREF_WRAPPERS.contains(&token) {
            continue;
        }
        if !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// Pick among same-name candidates: a call prefers callables, a use prefers
/// types (value vs type namespace). Applied only on ambiguity.
fn namespace_pick(candidates: Vec<&Node>, relation: Relation) -> Option<&Node> {
    match candidates.as_slice() {
        [node] => Some(node),
        [] => None,
        _ => {
            let preferred: Vec<&Node> = candidates
                .iter()
                .filter(|n| match relation {
                    Relation::Calls => is_callable(n.kind),
                    Relation::Uses => is_type_kind(n.kind),
                    Relation::Reads | Relation::Writes => {
                        matches!(n.kind, SymbolKind::Table | SymbolKind::View)
                    }
                    Relation::Creates | Relation::Alters | Relation::Drops => matches!(
                        n.kind,
                        SymbolKind::Table | SymbolKind::View | SymbolKind::Index
                    ),
                    _ => true,
                })
                .copied()
                .collect();
            match preferred.as_slice() {
                [node] => Some(node),
                _ => None,
            }
        }
    }
}

/// Bindings, pass statistics, indices of references the heuristic anchored
/// in the corpus but could not bind, and the subset of those whose absolute
/// path names a corpus module that has no such member (dangling).
pub fn resolve(
    index: &Index<'_>,
    references: &[Reference],
) -> (Vec<Binding>, ResolutionStats, Vec<usize>, Vec<usize>) {
    use rayon::prelude::*;
    let results: Vec<Res> = references
        .par_iter()
        .enumerate()
        .map(|(i, r)| {
            let Some(spec) = spec_for_path(&r.file) else {
                return Res::External;
            };
            let src = r
                .enclosing
                .clone()
                .unwrap_or_else(|| NodeId::new(r.file.clone()));
            let file_module = key_of(spec, &index.roots, &r.file);
            let imports = index.imports.get(r.file.as_str());
            let (target, evidence, internal) = resolve_one(index, spec, r, &file_module, imports);
            match target {
                Some(node) if node.id != src => {
                    // A "call" landing on a type is a conversion or
                    // instantiation of a non-callable kind: it is a use.
                    let relation = if r.relation == Relation::Calls && is_type_kind(node.kind) {
                        Relation::Uses
                    } else {
                        r.relation
                    };
                    Res::Bound(Binding {
                        edge: Edge {
                            src,
                            dst: node.id.clone(),
                            relation,
                            evidence,
                            // Convention-bound (proto client) references
                            // are declared but never compiler-checked.
                            confidence: if evidence == Evidence::Declared {
                                Confidence::Inferred
                            } else {
                                evidence.confidence()
                            },
                            site: Some(r.span),
                        },
                        reference: i,
                    })
                }
                _ if internal => Res::Internal {
                    dangling: r.path.as_deref().is_some_and(|path| {
                        !spec.file_refs
                            && r.relation != Relation::Imports
                            && is_dangling_path(index, spec, r, path)
                    }),
                },
                _ => Res::External,
            }
        })
        .collect();
    let mut bindings = Vec::new();
    let mut stats = ResolutionStats::default();
    let mut internal_indices = Vec::new();
    let mut dangling_indices = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Res::Bound(binding) => {
                match binding.edge.evidence {
                    Evidence::Scope => stats.scope += 1,
                    _ => stats.import += 1,
                }
                bindings.push(binding);
            }
            Res::Internal { dangling } => {
                stats.unresolved_internal += 1;
                internal_indices.push(i);
                if dangling {
                    dangling_indices.push(i);
                }
            }
            Res::External => stats.unresolved_external += 1,
        }
    }
    (bindings, stats, internal_indices, dangling_indices)
}

/// A written path that absolutizes without an implicit module prefix
/// (`crate::util::gone`, not `super::x`, `self::x`, a bare relative path, or
/// a value receiver) and names a corpus module lacking the leaf. Relative
/// forms are excluded because absolutize cannot see inline modules, so
/// `super::` inside `mod tests` would look dangling at the wrong module.
fn is_dangling_path(index: &Index<'_>, spec: &LanguageSpec, r: &Reference, path: &str) -> bool {
    let absolute = (spec.absolutize)(path, &r.file);
    let written_head = path
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("");
    absolute.first().is_some_and(|head| head == written_head)
        && index.missing_internal_target(&expand(spec, &index.roots, &r.file, absolute))
}

fn resolve_one<'a>(
    index: &Index<'a>,
    spec: &sinter_extract::LanguageSpec,
    r: &Reference,
    file_module: &[String],
    imports: Option<&Vec<Import>>,
) -> (Option<&'a Node>, Evidence, bool) {
    if r.relation == Relation::Imports {
        return resolve_import_reference(index, spec, r);
    }

    if let Some(path) = &r.path {
        let (target, evidence, internal) =
            resolve_qualified_reference(index, spec, r, file_module, imports, path);
        if target.is_none()
            && r.relation == Relation::Calls
            && !index.proto_rpcs.is_empty()
            && let Some(rpc) = proto_client_call(index, spec, r, file_module, imports, path)
        {
            return (Some(rpc), Evidence::Declared, true);
        }
        return (target, evidence, internal);
    }

    resolve_bare_reference(index, spec, r, file_module, imports)
}

/// Method call on a tonic-generated client (`client.adjudicates(req)`):
/// the client type never exists in the corpus, so the call binds to the
/// proto rpc by convention. Receiver type comes from a typed local or a
/// declared field; otherwise the file's imports must name the client.
fn proto_client_call<'a>(
    index: &Index<'a>,
    spec: &LanguageSpec,
    r: &Reference,
    file_module: &[String],
    imports: Option<&Vec<Import>>,
    path: &str,
) -> Option<&'a Node> {
    let segments = expand(
        spec,
        &index.roots,
        &r.file,
        (spec.absolutize)(path, &r.file),
    );
    let prefix = segments.get(segments.len().checked_sub(2)?)?;
    let field_type = || {
        let enclosing = r.enclosing.as_ref()?;
        let (type_prefix, _) = qualified_of(enclosing.as_str()).rsplit_once("::")?;
        let name = type_prefix.rsplit("::").next().unwrap_or(type_prefix);
        let owner = index
            .by_file_qualified
            .get(&(r.file.as_str(), type_prefix))
            .copied()
            .or_else(|| index.type_def(&r.file, file_module, name))?;
        Some(index.field(owner, prefix)?.type_name.as_str())
    };
    let receiver_type = if segments.len() >= 3
        && spec
            .receivers
            .contains(&segments[segments.len() - 3].as_str())
    {
        field_type()
    } else {
        index.local_at(&r.file, prefix, r.span.start).flatten()
    };
    let tokens = imports.into_iter().flatten().flat_map(|imp| {
        imp.segments
            .iter()
            .map(String::as_str)
            .chain([imp.binding.as_str()])
    });
    index.proto_rpcs.client_call(&r.name, receiver_type, tokens)
}

/// Import declarations resolve through exact files first, then absolute
/// module/definition paths. A corpus-anchored miss remains internal.
fn resolve_import_reference<'a>(
    index: &Index<'a>,
    spec: &LanguageSpec,
    r: &Reference,
) -> (Option<&'a Node>, Evidence, bool) {
    let glob = matches!(r.alias.as_deref(), Some("*") | Some("."));
    let raw = strip_glob(&r.name);
    // An import naming a literal repo file binds it exactly — this is
    // how `#include "player/character.h"` stays unambiguous even though
    // header and impl share one module (fixture: cpp-header-impl).
    if let Some(node) = index
        .file_nodes
        .get(raw.trim().trim_matches(['<', '>', '"']))
    {
        return (Some(node), Evidence::Import, true);
    }
    let segments = expand(spec, &index.roots, &r.file, (spec.absolutize)(raw, &r.file));
    let target = if glob {
        index.import_file(&segments)
    } else {
        index.resolve_path(&segments, 4)
    };
    let internal = target.is_some() || index.anchored(&segments);
    (target, Evidence::Import, internal)
}

/// Qualified references resolve receiver and type evidence before absolute
/// paths and named imports. The tier ordering is part of the binding contract.
fn resolve_qualified_reference<'a>(
    index: &Index<'a>,
    spec: &LanguageSpec,
    r: &Reference,
    file_module: &[String],
    imports: Option<&Vec<Import>>,
    path: &str,
) -> (Option<&'a Node>, Evidence, bool) {
    // Document-path languages (spec.file_refs): the path names a
    // corpus file, never a symbol — dedicated tier, no fallthrough.
    if spec.file_refs {
        return resolve_file_ref(index, spec, r, path);
    }
    // Qualified reference: receiver, typed local, shadow, absolute
    // path, then imports — strongest local knowledge first.
    let segments = expand(
        spec,
        &index.roots,
        &r.file,
        (spec.absolutize)(path, &r.file),
    );
    let prefix = segments
        .len()
        .checked_sub(2)
        .and_then(|p| segments.get(p))
        .cloned();
    let Some(prefix) = prefix else {
        return (None, Evidence::Import, false);
    };
    // Field receiver: `self.harness.check()`. The ordinary receiver
    // tier sees `harness` as the prefix, so it cannot use the enclosing
    // impl type. A declared field type provides the missing link.
    if segments.len() >= 3
        && spec
            .receivers
            .contains(&segments[segments.len() - 3].as_str())
        && let Some(enclosing) = &r.enclosing
        && let Some((type_prefix, _)) = qualified_of(enclosing.as_str()).rsplit_once("::")
    {
        let owner = index
            .by_file_qualified
            .get(&(r.file.as_str(), type_prefix))
            .copied()
            .or_else(|| {
                let name = type_prefix.rsplit("::").next().unwrap_or(type_prefix);
                index.type_def(&r.file, file_module, name)
            });
        if let Some(owner) = owner
            && let Some(field) = index.field(owner, &segments[segments.len() - 2])
        {
            let field_spec = spec_for_path(&owner.file).unwrap_or(spec);
            let field_module = key_of(field_spec, &index.roots, &owner.file);
            let (target, anchored) =
                index.member_of_written_type(&owner.file, &field_module, &field.type_name, &r.name);
            return (target, Evidence::Scope, anchored);
        }
    }
    if spec.receivers.contains(&prefix.as_str())
        && let Some(enclosing) = &r.enclosing
        && let Some((type_prefix, _)) = qualified_of(enclosing.as_str()).rsplit_once("::")
    {
        // Sibling method in the same impl block's file: `self.m()`
        // inside `impl T` binds `T::m` without needing T's definition
        // in this file (struct in types.rs, impl in lib.rs).
        let sibling = format!("{type_prefix}::{}", r.name);
        if let Some(node) = index
            .by_file_qualified
            .get(&(r.file.as_str(), sibling.as_str()))
        {
            return (Some(node), Evidence::Scope, true);
        }
        if let Some(ty) = index.by_file_qualified.get(&(r.file.as_str(), type_prefix)) {
            // Receiver type is in the corpus: any miss is internal.
            return (index.member_of(ty, &r.name, 4), Evidence::Scope, true);
        }
    }
    match index.local_at(&r.file, &prefix, r.span.start) {
        Some(Some(type_name)) => {
            let (target, anchored) =
                index.member_of_written_type(&r.file, file_module, type_name, &r.name);
            // Known corpus type but missing member -> internal.
            return (target, Evidence::Scope, anchored);
        }
        Some(None) => return (None, Evidence::Scope, false), // shadowed: correctly no edge
        None => {}
    }
    // Same-scope type qualifier (Counter::new in the type's own file).
    if let Some(ty) = index.type_def(&r.file, file_module, &prefix)
        && let Some(node) = index.member_of(ty, &r.name, 4)
    {
        return (Some(node), Evidence::Scope, true);
    }
    if let Some(node) = index.resolve_path_defs(&segments, 4) {
        return (Some(node), Evidence::Import, true);
    }
    // Associated item through a path: the second-to-last segment is a
    // *type*, not a module (`some_crate::Config::new`,
    // `ns::Class::method`). Resolve the prefix as a path — re-export
    // chains included — then look the leaf up as a member. Path
    // shape, not language shape: active for every language.
    if let Some((leaf, type_path)) = segments.split_last()
        && type_path.len() >= 2
        && let Some(ty) = index.resolve_path_defs(type_path, 4)
        && let Some(node) = index.member_of(ty, leaf, 4)
    {
        return (Some(node), Evidence::Import, true);
    }
    let matching: Vec<&Import> = imports
        .into_iter()
        .flatten()
        .filter(|imp| !imp.glob && imp.binding == prefix)
        .collect();
    let candidates: Vec<&Node> = matching
        .iter()
        .filter_map(|imp| {
            let mut full = imp.segments.clone();
            full.push(r.name.clone());
            index.resolve_path(&full, 4)
        })
        .collect();
    let internal = candidates.len() > 1
        || index.anchored(&segments)
        || matching.iter().any(|imp| index.anchored(&imp.segments));
    match candidates.as_slice() {
        [node] => (Some(node), Evidence::Import, true),
        _ => (None, Evidence::Import, internal),
    }
}

/// Bare names resolve lexical scope and module scope before named and glob
/// imports. Shadowing and every ambiguity remain evidence-or-nothing.
fn resolve_bare_reference<'a>(
    index: &Index<'a>,
    spec: &LanguageSpec,
    r: &Reference,
    file_module: &[String],
    imports: Option<&Vec<Import>>,
) -> (Option<&'a Node>, Evidence, bool) {
    if index.local_at(&r.file, &r.name, r.span.start).is_some() {
        return (None, Evidence::Scope, false); // shadowed: correctly no edge
    }
    let enclosing_q = r
        .enclosing
        .as_ref()
        .map(|e| qualified_of(e.as_str()))
        .unwrap_or("");
    let visible: Vec<&Node> = index
        .by_file_name
        .get(&(r.file.as_str(), r.name.as_str()))
        .into_iter()
        .flatten()
        .filter(|d| {
            d.prefix.is_empty()
                || (d.functionish
                    && (enclosing_q == d.prefix
                        || enclosing_q.starts_with(&format!("{}::", d.prefix))))
        })
        .map(|d| d.node)
        .collect();
    if !visible.is_empty() {
        // Candidates exist in scope: a miss here is ambiguity — internal.
        return (namespace_pick(visible, r.relation), Evidence::Scope, true);
    }
    if let Some(defs) = index
        .module_defs
        .get(file_module)
        .and_then(|m| m.get(r.name.as_str()))
    {
        return (
            namespace_pick(defs.clone(), r.relation),
            Evidence::Scope,
            true,
        );
    }
    let named: Vec<&Node> = imports
        .into_iter()
        .flatten()
        .filter(|imp| !imp.glob && imp.binding == r.name)
        .filter_map(|imp| index.resolve_path(&imp.segments, 4))
        .collect();
    let (target, internal) = match named.as_slice() {
        [node] => (Some(*node), true),
        [] => {
            let globbed: Vec<&Node> = imports
                .into_iter()
                .flatten()
                .filter(|imp| imp.glob)
                .filter_map(|imp| {
                    let mut full = imp.segments.clone();
                    full.push(r.name.clone());
                    index.resolve_path(&full, 4).or_else(|| {
                        // Include-root import: bind via the imported
                        // file's own top-level definitions.
                        let file = index.import_file(&imp.segments)?;
                        index
                            .by_file_name
                            .get(&(file.file.as_str(), r.name.as_str()))
                            .into_iter()
                            .flatten()
                            .find(|d| d.prefix.is_empty())
                            .map(|d| d.node)
                    })
                })
                .collect();
            let name_imports_anchored = imports
                .into_iter()
                .flatten()
                .filter(|imp| !imp.glob && imp.binding == r.name)
                .any(|imp| index.anchored(&imp.segments));
            match globbed.as_slice() {
                [node] => (Some(*node), true),
                [] => (None, name_imports_anchored),
                _ => (None, true), // glob ambiguity across corpus modules
            }
        }
        _ => (None, true), // ambiguous named imports
    };
    if target.is_none() && (spec.name == "sql" || is_data_relation(r.relation)) {
        // Data relations from a non-SQL file are embedded SQL (sqlx/diesel
        // string literals): the host language's module key can never match
        // a SQL namespace, so the repo-wide unique-table tier is the only
        // binding chance.
        return sql_repo_fallback(index, r);
    }
    (target, Evidence::Import, internal)
}

/// Relations only SQL emits — table data flow, whether from a .sql file
/// or a SQL literal embedded in host-language code.
fn is_data_relation(relation: Relation) -> bool {
    matches!(
        relation,
        Relation::Reads | Relation::Writes | Relation::Creates | Relation::Alters | Relation::Drops
    )
}

/// Repo-wide SQL fallback: a table/view name that misses its database-root
/// namespace binds only when exactly one table or view in the whole corpus
/// carries that name. Two or more candidates is ambiguity — unresolved is
/// the answer, never a guess.
fn sql_repo_fallback<'a>(index: &Index<'a>, r: &Reference) -> (Option<&'a Node>, Evidence, bool) {
    let candidates: Vec<&Node> = index
        .by_name
        .get(r.name.as_str())
        .into_iter()
        .flatten()
        .map(|(_, node)| *node)
        .filter(|n| matches!(n.kind, SymbolKind::Table | SymbolKind::View))
        .collect();
    match candidates.as_slice() {
        [node] => (Some(node), Evidence::Scope, true),
        [] => (None, Evidence::Scope, false),
        _ => (None, Evidence::Scope, true), // ambiguous across roots: conservative
    }
}

/// Document-path reference (spec.file_refs, e.g. a markdown link): the
/// path resolves to a corpus file — the same exact-file evidence imports
/// carry — with the language's extensions optional and `#fragment`
/// binding the target file's unique def whose name slugifies to the
/// fragment (`#quality-gate` -> the "Quality Gate" section). A path that
/// names no corpus file is a dead or external link and stays unresolved:
/// evidence or nothing, never a guess.
fn resolve_file_ref<'a>(
    index: &Index<'a>,
    spec: &LanguageSpec,
    r: &Reference,
    path: &str,
) -> (Option<&'a Node>, Evidence, bool) {
    let (head, frag) = match path.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (path, None),
    };
    let file = if head.is_empty() {
        // `#fragment` alone: the linking file itself.
        index.file_nodes.get(r.file.as_str()).copied()
    } else {
        let joined = (spec.absolutize)(head, &r.file).join("/");
        index.file_nodes.get(joined.as_str()).copied().or_else(|| {
            spec.extensions.iter().find_map(|ext| {
                index
                    .file_nodes
                    .get(format!("{joined}.{ext}").as_str())
                    .copied()
            })
        })
    };
    match (file, frag) {
        (Some(file), None) => (Some(file), Evidence::Import, true),
        (Some(file), Some(frag)) => {
            let matching: Vec<&Node> = index
                .defs_by_file
                .get(file.file.as_str())
                .into_iter()
                .flatten()
                .filter(|n| slugify(&n.name) == frag)
                .copied()
                .collect();
            // The file is corpus evidence: a fragment miss (or a
            // duplicate slug) is internal, and unique-or-nothing holds.
            match matching.as_slice() {
                [node] => (Some(node), Evidence::Import, true),
                _ => (None, Evidence::Import, true),
            }
        }
        (None, _) => (None, Evidence::Import, false),
    }
}

/// GitHub-style heading slug: lowercase, spaces become dashes, `-`/`_`
/// survive, other punctuation drops.
fn slugify(name: &str) -> String {
    name.chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// Dynamic-dispatch fan-out edges: for every impl block naming a trait the
/// corpus defines, `trait_method -> impl_method` (Calls, Dynamic) for each
/// method the impl defines under a same-named trait method. Conservative
/// over-approximation — every impl is assumed reachable through the trait —
/// which is exactly why the edges carry the distinct Dynamic evidence.
/// Pairing rule: the impl block names the trait (same file/module, or a
/// named import) and the method names match.
pub fn dynamic_edges(index: &Index<'_>, nodes: &[Node], trait_impls: &[TraitImpl]) -> Vec<Edge> {
    // Proto service conventions ride the same post-resolution slot: they
    // need nodes and impl blocks, nothing from reference resolution.
    let mut edges = crate::proto_service_bindings::proto_service_edges(nodes, trait_impls);
    let implicit = nodes
        .iter()
        .any(|n| spec_for_path(&n.file).is_some_and(|s| s.implicit_interfaces));
    if trait_impls.is_empty() && !implicit {
        return edges;
    }
    let roots = &index.roots;
    let mut by_file: HashMap<&str, Vec<&Node>> = HashMap::new();
    let mut types_by_file: HashMap<&str, Vec<&Node>> = HashMap::new();
    for n in nodes {
        if is_callable(n.kind) {
            by_file.entry(n.file.as_str()).or_default().push(n);
        }
        if is_member_scope(n.kind) {
            types_by_file.entry(n.file.as_str()).or_default().push(n);
        }
    }
    // Class included: C# captures base classes as @trait because its
    // virtual dispatch flows through them; Rust/Java only ever emit
    // @trait on real traits/interfaces, so they are unaffected.
    let is_trait = |n: &Node| {
        matches!(
            n.kind,
            SymbolKind::Trait | SymbolKind::Interface | SymbolKind::Class
        )
    };
    for ti in trait_impls {
        let Some(spec) = spec_for_path(&ti.file) else {
            continue;
        };
        let file_module = key_of(spec, roots, &ti.file);
        let trait_node = index
            .type_def(&ti.file, &file_module, &ti.trait_name)
            .filter(|n| is_trait(n))
            .map(|n| (n, Evidence::Scope))
            .or_else(|| {
                // Trait bound through a named import; unique or nothing.
                let named: Vec<&Node> = index
                    .imports
                    .get(ti.file.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|imp| !imp.glob && imp.binding == ti.trait_name)
                    .filter_map(|imp| index.resolve_path_defs(&imp.segments, 4))
                    .filter(|n| is_trait(n))
                    .collect();
                match named.as_slice() {
                    [node] => Some((node, Evidence::Import)),
                    _ => None,
                }
            })
            .or_else(|| {
                // Glob imports (C++ #include, C# using): the trait is one
                // of the module's top-level names; unique or nothing.
                let globbed: Vec<&Node> = index
                    .imports
                    .get(ti.file.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|imp| imp.glob)
                    .filter_map(|imp| {
                        let mut full = imp.segments.clone();
                        full.push(ti.trait_name.clone());
                        index.resolve_path_defs(&full, 4)
                    })
                    .filter(|n| is_trait(n))
                    .collect();
                match globbed.as_slice() {
                    [node] => Some((node, Evidence::Import)),
                    _ => None,
                }
            });
        let Some((trait_node, pair_evidence)) = trait_node else {
            continue; // external trait: nothing in the corpus to fan into
        };
        let mut impl_methods: Vec<&Node> = Vec::new();
        for method in by_file.get(ti.file.as_str()).into_iter().flatten() {
            if !(ti.span.start <= method.span.start && method.span.end <= ti.span.end) {
                continue;
            }
            impl_methods.push(method);
            if let Some(trait_method) = index.member_of(trait_node, &method.name, 1)
                && trait_method.id != method.id
            {
                edges.push(Edge {
                    src: trait_method.id.clone(),
                    dst: method.id.clone(),
                    relation: Relation::Calls,
                    evidence: Evidence::Dynamic,
                    confidence: Evidence::Dynamic.confidence(),
                    // Fan-out is assumed, not written anywhere: no site.
                    site: None,
                });
            }
        }
        // Persistent supertype edge, impl type -> trait/base. The block
        // either IS the implementing type's declaration (class languages)
        // or contains its methods (Rust impl blocks) — the method prefix
        // then names the type. Same kinds mean inheritance (class : class,
        // interface extends interface); differing kinds mean an interface
        // contract. Evidence mirrors how the pairing was bound.
        let impl_type = types_by_file
            .get(ti.file.as_str())
            .into_iter()
            .flatten()
            .find(|n| n.span == ti.span)
            .copied()
            .or_else(|| {
                let prefix = impl_methods.iter().find_map(|m| {
                    let q = qualified_of(m.id.as_str());
                    q.rsplit_once("::")
                        .map(|(p, _)| p.rsplit("::").next().unwrap_or(p))
                })?;
                index.type_def(&ti.file, &file_module, prefix)
            });
        if let Some(impl_type) = impl_type
            && impl_type.id != trait_node.id
        {
            let relation = if impl_type.kind == trait_node.kind {
                Relation::Extends
            } else {
                Relation::Implements
            };
            edges.push(Edge {
                src: impl_type.id.clone(),
                dst: trait_node.id.clone(),
                relation,
                evidence: pair_evidence,
                confidence: pair_evidence.confidence(),
                // The impl block's span lives in ti.file, which may not be
                // the impl type's file — a site here could point into the
                // wrong file, so none is carried.
                site: None,
            });
        }
    }
    if implicit {
        edges.extend(implicit_interface_edges(nodes, roots));
    }
    edges.sort();
    edges.dedup();
    edges
}

/// Structural interface satisfaction for languages where no syntax names
/// the interface at the implementing type (spec.implicit_interfaces — Go):
/// within one package, a type T satisfies interface I when T's method
/// names cover all of I's declared methods. Name-only matching
/// over-approximates signatures, so every edge carries Dynamic evidence
/// (Inferred, excludable). Package scope keeps precision high: matching
/// the whole corpus would pair unrelated same-shaped types.
/// ponytail: cross-package satisfaction (io.Writer style) not inferred;
/// widen to module scope if a real repo shows the recall gap.
fn implicit_interface_edges(nodes: &[Node], roots: &[ModuleRoot]) -> Vec<Edge> {
    // (package key, type name) -> type nodes; (package key, type name) ->
    // methods declared/received under that name.
    let mut types: HashMap<(Vec<String>, &str), Vec<&Node>> = HashMap::new();
    let mut types_by_key: HashMap<Vec<String>, Vec<&Node>> = HashMap::new();
    let mut methods: HashMap<(Vec<String>, &str), Vec<&Node>> = HashMap::new();
    for n in nodes {
        let Some(spec) = spec_for_path(&n.file) else {
            continue;
        };
        if !spec.implicit_interfaces {
            continue;
        }
        let key = key_of(spec, roots, &n.file);
        match n.kind {
            SymbolKind::Interface | SymbolKind::Struct | SymbolKind::TypeAlias => {
                types
                    .entry((key.clone(), n.name.as_str()))
                    .or_default()
                    .push(n);
                types_by_key.entry(key).or_default().push(n);
            }
            SymbolKind::Method => {
                let q = qualified_of(n.id.as_str());
                if let Some((owner, _)) = q.rsplit_once("::")
                    && !owner.contains("::")
                {
                    methods.entry((key, owner)).or_default().push(n);
                }
            }
            _ => {}
        }
    }
    let mut edges = Vec::new();
    for ((key, name), candidates) in &types {
        // Unique or nothing: a same-named sibling makes ownership ambiguous.
        let [iface] = candidates.as_slice() else {
            continue;
        };
        if iface.kind != SymbolKind::Interface {
            continue;
        }
        let Some(iface_methods) = methods.get(&(key.clone(), *name)) else {
            continue; // empty interface: everything satisfies it — emit nothing
        };
        for ty in types_by_key.get(key).into_iter().flatten() {
            if ty.kind == SymbolKind::Interface {
                continue;
            }
            let ty_methods = methods.get(&(key.clone(), ty.name.as_str()));
            let covers = |m: &Node| ty_methods.into_iter().flatten().any(|tm| tm.name == m.name);
            if !iface_methods.iter().all(|m| covers(m)) {
                continue;
            }
            for im in iface_methods {
                for tm in ty_methods.into_iter().flatten() {
                    if tm.name == im.name {
                        edges.push(Edge {
                            src: im.id.clone(),
                            dst: tm.id.clone(),
                            relation: Relation::Calls,
                            evidence: Evidence::Dynamic,
                            confidence: Evidence::Dynamic.confidence(),
                            site: None,
                        });
                    }
                }
            }
            edges.push(Edge {
                src: ty.id.clone(),
                dst: iface.id.clone(),
                relation: Relation::Implements,
                evidence: Evidence::Dynamic,
                confidence: Evidence::Dynamic.confidence(),
                site: None,
            });
        }
    }
    edges
}

/// Resolve references against FOREIGN definitions using import evidence
/// only — the cross-repo boundary pass. Same-file/module/receiver/local
/// tiers are intra-repo by definition and deliberately excluded, which
/// also prevents false bindings between identically-named files in
/// different members. `refs` and `owner_imports` come from one member;
/// `foreign_nodes` from the others.
pub fn resolve_boundary(
    foreign_nodes: &[Node],
    references: &[Reference],
    owner_imports: &[Reference],
) -> Vec<Binding> {
    let index = build_index(foreign_nodes, owner_imports, &[], &[], &[], &[]);
    let mut bindings = Vec::new();
    for (i, r) in references.iter().enumerate() {
        let Some(spec) = spec_for_path(&r.file) else {
            continue;
        };
        let src = r
            .enclosing
            .clone()
            .unwrap_or_else(|| NodeId::new(r.file.clone()));
        let imports = index.imports.get(r.file.as_str());
        let target = if r.relation == Relation::Imports {
            let glob = matches!(r.alias.as_deref(), Some("*") | Some("."));
            let segments = (spec.absolutize)(strip_glob(&r.name), &r.file);
            if glob {
                index.import_file(&segments)
            } else {
                index.resolve_path(&segments, 4)
            }
        } else if let Some(path) = &r.path {
            let segments = (spec.absolutize)(path, &r.file);
            let direct = index.resolve_path(&segments, 4);
            direct.or_else(|| {
                let prefix = segments
                    .len()
                    .checked_sub(2)
                    .and_then(|p| segments.get(p))?;
                let candidates: Vec<&Node> = imports
                    .into_iter()
                    .flatten()
                    .filter(|imp| !imp.glob && imp.binding == *prefix)
                    .filter_map(|imp| {
                        let mut full = imp.segments.clone();
                        full.push(r.name.clone());
                        index.resolve_path(&full, 4)
                    })
                    .collect();
                match candidates.as_slice() {
                    [node] => Some(node),
                    _ => None,
                }
            })
        } else {
            // Bare name: only through this member's own imports.
            let named: Vec<&Node> = imports
                .into_iter()
                .flatten()
                .filter(|imp| !imp.glob && imp.binding == r.name)
                .filter_map(|imp| index.resolve_path(&imp.segments, 4))
                .collect();
            match named.as_slice() {
                [node] => Some(*node),
                _ => None,
            }
        };
        if let Some(node) = target
            && node.id != src
        {
            let relation = if r.relation == Relation::Calls && is_type_kind(node.kind) {
                Relation::Uses
            } else {
                r.relation
            };
            bindings.push(Binding {
                edge: Edge {
                    src,
                    dst: node.id.clone(),
                    relation,
                    evidence: Evidence::Import,
                    confidence: Evidence::Import.confidence(),
                    site: Some(r.span),
                },
                reference: i,
            });
        }
    }
    bindings
}

/// Segment count of `key` if it is a non-empty suffix of `path`.
fn suffix_len(key: &[String], path: &[String]) -> Option<usize> {
    (!key.is_empty() && path.len() >= key.len() && path[path.len() - key.len()..] == key[..])
        .then_some(key.len())
}

/// Of the longest-key candidates, the single node — or None on ambiguity.
fn unique_best<'a>(candidates: impl Iterator<Item = (usize, &'a Node)>) -> Option<&'a Node> {
    let mut best: Option<(usize, Vec<&Node>)> = None;
    for (len, node) in candidates {
        match &mut best {
            Some((best_len, nodes)) if len == *best_len => nodes.push(node),
            Some((best_len, nodes)) if len > *best_len => {
                *best_len = len;
                nodes.clear();
                nodes.push(node);
            }
            None => best = Some((len, vec![node])),
            _ => {}
        }
    }
    match best {
        Some((_, nodes)) if nodes.len() == 1 => Some(nodes[0]),
        _ => None,
    }
}

#[cfg(test)]
mod resolution_stats_tests {
    use super::{ResolutionStats, type_candidates};

    #[test]
    fn anchored_rate_is_absent_when_the_pass_measured_nothing() {
        assert_eq!(ResolutionStats::default().anchored_unresolved_rate(), None);
    }

    #[test]
    fn anchored_rate_excludes_external_references() {
        let stats = ResolutionStats {
            scope: 4,
            import: 3,
            scip: 42,
            compiler_rescued_internal: 2,
            unresolved_internal: 1,
            unresolved_external: 90,
            ..ResolutionStats::default()
        };

        assert_eq!(stats.anchored_unresolved_rate(), Some(0.1));
    }

    #[test]
    fn written_type_unwraps_only_receiver_transparent_wrappers() {
        assert_eq!(type_candidates("&Dog"), ["Dog"]);
        assert_eq!(type_candidates("std::sync::Arc<dyn Harness>"), ["Harness"]);
        assert_eq!(type_candidates("Option<Dog>"), ["Option"]);
        assert_eq!(type_candidates("Result<Dog, Error>"), ["Result"]);
    }
}
