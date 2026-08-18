//! Evidence-based reference resolution. Tiers, strongest local knowledge
//! first: receiver binding, typed-local binding, shadow suppression,
//! same-file/same-module scope, then import evidence (aliases, globs,
//! re-export chains, relative paths). Exactly one candidate or nothing —
//! ambiguity is unresolved, never a guess.

use std::collections::HashMap;

use sinter_core::{
    Edge, Embed, Evidence, LocalBinding, Node, NodeId, Reference, Relation, SymbolKind, TraitImpl,
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
    /// Evidence pointed into the corpus but binding failed (ambiguity,
    /// member missing on a known module/type). The accuracy gauge.
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
}

impl ResolutionStats {
    pub fn resolved(&self) -> usize {
        self.scope + self.import + self.scip
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

    /// Internal-unresolved over corpus-anchored references — the number
    /// that measures resolver accuracy rather than corpus openness.
    pub fn internal_unresolved_rate(&self) -> f64 {
        let total = self.resolved() + self.unresolved_internal;
        if total == 0 {
            0.0
        } else {
            self.unresolved_internal as f64 / total as f64
        }
    }
}

/// Per-reference resolution verdict.
enum Res {
    Bound(Binding),
    Internal,
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
    prefix: String,
    /// Every ancestor on the prefix is function-like, so the name is
    /// lexically visible bare inside them (nested fns yes, methods no).
    functionish: bool,
}

struct Index<'a> {
    /// (file, plain name) -> defs with visibility info.
    by_file_name: HashMap<(&'a str, &'a str), Vec<FileDef<'a>>>,
    /// (file, qualified) -> def, receiver/type lookups.
    by_file_qualified: HashMap<(&'a str, &'a str), &'a Node>,
    /// exact file path -> file node (includes naming a literal repo file).
    file_nodes: HashMap<&'a str, &'a Node>,
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
    /// owner node id -> embedded type names.
    embeds: HashMap<&'a str, Vec<&'a str>>,
    /// Discovered package roots (manifest-declared name <-> directory).
    roots: Vec<ModuleRoot>,
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
    match key.first() {
        Some(head) if manifest.self_names.contains(&head.as_str()) => {
            key[0] = root.name.clone();
        }
        _ => key.insert(0, root.name.clone()),
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

fn build_index<'a>(
    nodes: &'a [Node],
    all_imports: &'a [Reference],
    locals: &'a [LocalBinding],
    embeds: &'a [Embed],
    roots: &[ModuleRoot],
) -> Index<'a> {
    let mut index = Index {
        by_file_name: HashMap::new(),
        by_file_qualified: HashMap::new(),
        file_nodes: HashMap::new(),
        by_name: HashMap::new(),
        by_module_tail: HashMap::new(),
        files_of_module: HashMap::new(),
        module_defs: HashMap::new(),
        imports: HashMap::new(),
        locals: HashMap::new(),
        embeds: HashMap::new(),
        roots: roots.to_vec(),
    };
    // Pass 1: qualified -> kind per file, for ancestor-kind checks.
    let mut kind_of: HashMap<(&str, &str), SymbolKind> = HashMap::new();
    for node in nodes {
        kind_of.insert(
            (node.file.as_str(), qualified_of(node.id.as_str())),
            node.kind,
        );
    }
    for node in nodes {
        let Some(spec) = spec_for_path(&node.file) else {
            continue;
        };
        let file_module = key_of(spec, roots, &node.file);
        if node.kind == SymbolKind::File {
            index.file_nodes.insert(node.file.as_str(), node);
            if let Some(tail) = file_module.last() {
                index
                    .by_module_tail
                    .entry(tail.clone())
                    .or_default()
                    .push((file_module.clone(), node));
            }
            if let Some(tail) = file_module.last() {
                let entries = index.files_of_module.entry(tail.clone()).or_default();
                match entries.iter_mut().find(|m| m.key == file_module) {
                    Some(m) => m.files.push(&node.file),
                    None => entries.push(ModuleFiles {
                        key: file_module.clone(),
                        files: vec![&node.file],
                    }),
                }
            }
            continue;
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
        index
            .by_file_qualified
            .insert((node.file.as_str(), qualified), node);
        index
            .by_file_name
            .entry((node.file.as_str(), node.name.as_str()))
            .or_default()
            .push(FileDef {
                node,
                prefix: prefix.to_string(),
                functionish: prefix.is_empty() || functionish.is_some(),
            });
        let mut module = file_module.clone();
        if !prefix.is_empty() {
            module.extend(prefix.split("::").map(str::to_string));
        }
        index
            .by_name
            .entry(node.name.as_str())
            .or_default()
            .push((module, node));
        if prefix.is_empty() {
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

pub fn resolve(
    nodes: &[Node],
    references: &[Reference],
    locals: &[LocalBinding],
    all_imports: &[Reference],
    embeds: &[Embed],
    roots: &[ModuleRoot],
) -> (Vec<Binding>, ResolutionStats, Vec<usize>) {
    let t = std::time::Instant::now();
    let index = build_index(nodes, all_imports, locals, embeds, roots);
    if std::env::var_os("SINTER_TIMING").is_some() {
        eprintln!("index build: {:?}", t.elapsed());
    }
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
            let (target, evidence, internal) = resolve_one(&index, spec, r, &file_module, imports);
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
                            confidence: evidence.confidence(),
                        },
                        reference: i,
                    })
                }
                _ if internal => Res::Internal,
                _ => Res::External,
            }
        })
        .collect();
    let mut bindings = Vec::new();
    let mut stats = ResolutionStats::default();
    let mut internal_indices = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Res::Bound(binding) => {
                match binding.edge.evidence {
                    Evidence::Scope => stats.scope += 1,
                    _ => stats.import += 1,
                }
                bindings.push(binding);
            }
            Res::Internal => {
                stats.unresolved_internal += 1;
                internal_indices.push(i);
            }
            Res::External => stats.unresolved_external += 1,
        }
    }
    (bindings, stats, internal_indices)
}

fn resolve_one<'a>(
    index: &Index<'a>,
    spec: &sinter_extract::LanguageSpec,
    r: &Reference,
    file_module: &[String],
    imports: Option<&Vec<Import>>,
) -> (Option<&'a Node>, Evidence, bool) {
    if r.relation == Relation::Imports {
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
        return (target, Evidence::Import, internal);
    }

    if let Some(path) = &r.path {
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
        if spec.receivers.contains(&prefix.as_str())
            && let Some(enclosing) = &r.enclosing
            && let Some((type_prefix, _)) = qualified_of(enclosing.as_str()).rsplit_once("::")
            && let Some(ty) = index.by_file_qualified.get(&(r.file.as_str(), type_prefix))
        {
            // Receiver type is in the corpus: any miss is internal.
            return (index.member_of(ty, &r.name, 4), Evidence::Scope, true);
        }
        match index.local_at(&r.file, &prefix, r.span.start) {
            Some(Some(type_name)) => {
                let ty = index.type_def(&r.file, file_module, type_name);
                let target = ty.and_then(|ty| index.member_of(ty, &r.name, 4));
                // Known corpus type but missing member -> internal.
                return (target, Evidence::Scope, ty.is_some());
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
        return match candidates.as_slice() {
            [node] => (Some(node), Evidence::Import, true),
            _ => (None, Evidence::Import, internal),
        };
    }

    // Bare name.
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
    (target, Evidence::Import, internal)
}

/// Dynamic-dispatch fan-out edges: for every impl block naming a trait the
/// corpus defines, `trait_method -> impl_method` (Calls, Dynamic) for each
/// method the impl defines under a same-named trait method. Conservative
/// over-approximation — every impl is assumed reachable through the trait —
/// which is exactly why the edges carry the distinct Dynamic evidence.
/// Pairing rule: the impl block names the trait (same file/module, or a
/// named import) and the method names match.
pub fn dynamic_edges(
    nodes: &[Node],
    trait_impls: &[TraitImpl],
    all_imports: &[Reference],
    roots: &[ModuleRoot],
) -> Vec<Edge> {
    if trait_impls.is_empty() {
        return Vec::new();
    }
    let index = build_index(nodes, all_imports, &[], &[], roots);
    let mut by_file: HashMap<&str, Vec<&Node>> = HashMap::new();
    for n in nodes {
        if is_callable(n.kind) {
            by_file.entry(n.file.as_str()).or_default().push(n);
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
    let mut edges = Vec::new();
    for ti in trait_impls {
        let Some(spec) = spec_for_path(&ti.file) else {
            continue;
        };
        let file_module = key_of(spec, roots, &ti.file);
        let trait_node = index
            .type_def(&ti.file, &file_module, &ti.trait_name)
            .filter(|n| is_trait(n))
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
                    [node] => Some(node),
                    _ => None,
                }
            });
        let Some(trait_node) = trait_node else {
            continue; // external trait: nothing in the corpus to fan into
        };
        for method in by_file.get(ti.file.as_str()).into_iter().flatten() {
            if !(ti.span.start <= method.span.start && method.span.end <= ti.span.end) {
                continue;
            }
            if let Some(trait_method) = index.member_of(trait_node, &method.name, 1)
                && trait_method.id != method.id
            {
                edges.push(Edge {
                    src: trait_method.id.clone(),
                    dst: method.id.clone(),
                    relation: Relation::Calls,
                    evidence: Evidence::Dynamic,
                    confidence: Evidence::Dynamic.confidence(),
                });
            }
        }
    }
    edges.sort();
    edges.dedup();
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
    let index = build_index(foreign_nodes, owner_imports, &[], &[], &[]);
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
