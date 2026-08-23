use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use protobuf::Message;
use scip::types::Index;
use sinter_core::{Edge, Node, NodeId, Reference, Relation, Span, SymbolKind};

use crate::resolver::Binding;

/// Everything one index pass yields: binds to in-corpus definitions plus
/// the synthesized dependency surface (D29) for references whose symbol
/// has no definition occurrence anywhere in the corpus.
pub struct ScipResolution {
    /// References bound to in-corpus definition nodes.
    pub bindings: Vec<Binding>,
    /// References bound to synthesized dep nodes (edge dst is a node in
    /// `external_nodes`). The caller decides which survive — a ref already
    /// bound by internal evidence keeps its internal edge.
    pub external: Vec<Binding>,
    /// Distinct synthesized dependency-surface nodes, keyed by id via
    /// `external` edges; unreferenced ones must not be installed.
    pub external_nodes: Vec<Node>,
    /// Distinct external symbols that overlapped a reference but did not
    /// parse as a `<scheme> <manager> <package> <version> <descriptors>`
    /// moniker (skipped silently, counted here).
    pub external_skipped: usize,
    /// Edges from SCIP reference occurrences that overlap NO extracted
    /// reference (macro token trees: `format!`, `assert_eq!`, ...) but sit
    /// inside one of our nodes and point at an in-corpus definition. Only
    /// produced for files in `scope`; src is the enclosing node.
    pub unanchored: Vec<Edge>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScipError {
    #[error("cannot read SCIP index {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot parse SCIP index {path}: {source}")]
    Parse {
        path: String,
        source: protobuf::Error,
    },
}

pub fn load_index(path: &Path) -> Result<Index, ScipError> {
    let bytes = std::fs::read(path).map_err(|source| ScipError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Index::parse_from_bytes(&bytes).map_err(|source| ScipError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Rewrite document paths from an index produced in a nested project so
/// they remain repo-relative after indexes from several project roots are
/// merged. Indexers generally emit paths relative to their working
/// directory, while Sinter's nodes are always relative to the repository.
pub fn prefix_index_paths(path: &Path, prefix: &str) -> Result<(), ScipError> {
    if prefix.is_empty() {
        return Ok(());
    }
    let mut index = load_index(path)?;
    let prefix = prefix.trim_end_matches('/');
    for document in &mut index.documents {
        let relative = document.relative_path.replace('\\', "/");
        if !Path::new(&relative).is_absolute()
            && relative != prefix
            && !relative.starts_with(&format!("{prefix}/"))
        {
            document.relative_path = format!("{prefix}/{relative}");
        } else {
            document.relative_path = relative;
        }
    }
    let bytes = index.write_to_bytes().map_err(|source| ScipError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(path, bytes).map_err(|source| ScipError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Merge several on-disk indexes into one file: documents and external
/// symbols concatenate, metadata comes from the first. Per-language
/// indexers cover disjoint files, so concatenation is the whole merge.
pub fn merge_index_files(paths: &[&Path], out: &Path) -> Result<(), ScipError> {
    let (first, rest) = paths.split_first().expect("at least one index");
    let mut merged = load_index(first)?;
    for path in rest {
        let mut index = load_index(path)?;
        merged.documents.append(&mut index.documents);
        merged.external_symbols.append(&mut index.external_symbols);
    }
    let bytes = merged.write_to_bytes().map_err(|source| ScipError::Parse {
        path: out.display().to_string(),
        source,
    })?;
    std::fs::write(out, bytes).map_err(|source| ScipError::Io {
        path: out.display().to_string(),
        source,
    })
}

/// Bind our extracted references using a compiler-produced SCIP index —
/// the highest evidence tier. A reference binds when a SCIP reference
/// occurrence overlaps its span and the symbol's definition occurrence
/// falls inside one of our nodes. `read_source` maps a repo-relative path
/// to its content (for line→byte conversion); return None to skip a file.
/// `scope` is the set of files being (re)resolved: occurrences there that
/// anchor to no extracted reference still yield `unanchored` edges, so
/// calls the extractor cannot see (inside macro token trees) are not lost.
pub fn resolve_with_index(
    index: &Index,
    nodes: &[Node],
    references: &[Reference],
    scope: &BTreeSet<String>,
    mut read_source: impl FnMut(&str) -> Option<String>,
) -> ScipResolution {
    let mut line_starts: HashMap<String, Vec<u64>> = HashMap::new();
    for document in &index.documents {
        if let Some(source) = read_source(&document.relative_path) {
            let mut starts = vec![0u64];
            for (i, b) in source.bytes().enumerate() {
                if b == b'\n' {
                    starts.push(i as u64 + 1);
                }
            }
            line_starts.insert(document.relative_path.clone(), starts);
        }
    }
    let to_byte = |file: &str, line: i32, col: i32| -> Option<u64> {
        let starts = line_starts.get(file)?;
        Some(starts.get(line as usize)? + col as u64)
    };

    // Pass 1: symbol -> our node containing its definition occurrence.
    let mut nodes_by_file: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes {
        nodes_by_file
            .entry(node.file.as_str())
            .or_default()
            .push(node);
    }
    // Indexers can emit the SAME moniker for distinct definitions (e.g.
    // rust-analyzer gives every test binary's `fn sinter()` helper one
    // symbol string). Binding through such a moniker cross-file attaches
    // refs to the wrong file's definition, so track ambiguity and keep a
    // per-file map: an ambiguous symbol may only bind within the file
    // that defines it.
    let mut def_of_symbol: HashMap<&str, &Node> = HashMap::new();
    let mut ambiguous: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut same_file_def: HashMap<(&str, &str), &Node> = HashMap::new();
    for document in &index.documents {
        for occ in &document.occurrences {
            if occ.symbol_roles & scip::types::SymbolRole::Definition as i32 == 0 {
                continue;
            }
            // `local N` symbols are document-scoped; a global map would
            // bind them across files.
            if occ.symbol.starts_with("local ") {
                continue;
            }
            let Some(pos) = occ
                .range
                .first()
                .zip(occ.range.get(1))
                .and_then(|(l, c)| to_byte(&document.relative_path, *l, *c))
            else {
                continue;
            };
            let target = nodes_by_file
                .get(document.relative_path.as_str())
                .into_iter()
                .flatten()
                .filter(|n| span_contains(n.span, pos))
                .min_by_key(|n| n.span.end - n.span.start);
            if let Some(node) = target {
                same_file_def.insert((&occ.symbol, &document.relative_path), node);
                match def_of_symbol.entry(&occ.symbol) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(node);
                    }
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if e.get().file != node.file {
                            ambiguous.insert(&occ.symbol);
                        }
                    }
                }
            }
        }
    }

    // Pass 2: SCIP reference occurrences overlapping our reference spans.
    let mut refs_by_file: HashMap<&str, Vec<(usize, &Reference)>> = HashMap::new();
    for (i, r) in references.iter().enumerate() {
        refs_by_file
            .entry(r.file.as_str())
            .or_default()
            .push((i, r));
    }
    // Per reference keep the rightmost contained occurrence: a span like
    // `util::helper` overlaps both the module and the item occurrence, and
    // the reference's meaning is the item — the last path segment.
    // Internal (def in corpus) and external (dep surface, D29) occurrences
    // track separately; the caller prefers internal.
    let mut best: HashMap<usize, (u64, Edge)> = HashMap::new();
    let mut best_ext: HashMap<usize, (u64, Edge)> = HashMap::new();
    // symbol -> parsed dep node (None: unparseable, skip and count once).
    let mut dep_cache: HashMap<String, Option<Node>> = HashMap::new();
    let mut skipped: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unanchored: Vec<Edge> = Vec::new();
    for document in &index.documents {
        let file = document.relative_path.as_str();
        let in_scope = scope.contains(file);
        let file_refs = refs_by_file.get(file).map_or(&[][..], Vec::as_slice);
        if file_refs.is_empty() && !in_scope {
            continue;
        }
        for occ in &document.occurrences {
            if occ.symbol_roles & scip::types::SymbolRole::Definition as i32 != 0
                || occ.symbol.starts_with("local ")
            {
                continue;
            }
            let target = if ambiguous.contains(occ.symbol.as_str()) {
                same_file_def.get(&(occ.symbol.as_str(), document.relative_path.as_str()))
            } else {
                def_of_symbol.get(occ.symbol.as_str())
            };
            let Some(pos) = occ
                .range
                .first()
                .zip(occ.range.get(1))
                .and_then(|(l, c)| to_byte(&document.relative_path, *l, *c))
            else {
                continue;
            };
            let mut anchored = false;
            for (i, r) in file_refs {
                if !span_contains(r.span, pos) {
                    continue;
                }
                anchored = true;
                let distance = pos - r.span.start;
                let make_edge = |dst: NodeId| Edge {
                    src: r
                        .enclosing
                        .clone()
                        .unwrap_or_else(|| NodeId::new(r.file.clone())),
                    dst,
                    relation: r.relation,
                    evidence: sinter_core::Evidence::Scip,
                    confidence: sinter_core::Evidence::Scip.confidence(),
                    site: Some(r.span),
                };
                match target {
                    Some(target) => {
                        if best.get(i).is_none_or(|(d, _)| distance > *d) {
                            best.insert(*i, (distance, make_edge(target.id.clone())));
                        }
                    }
                    // No in-corpus definition anywhere: the compiler
                    // resolved this into a dependency — synthesize the
                    // dep-surface node instead of discarding its answer.
                    None => {
                        let node = dep_cache
                            .entry(occ.symbol.clone())
                            .or_insert_with(|| dep_node(&occ.symbol));
                        match node {
                            Some(node) => {
                                if best_ext.get(i).is_none_or(|(d, _)| distance > *d) {
                                    best_ext.insert(*i, (distance, make_edge(node.id.clone())));
                                }
                            }
                            None => {
                                skipped.insert(occ.symbol.clone());
                            }
                        }
                    }
                }
            }
            if anchored || !in_scope {
                continue;
            }
            // No extracted reference covers this occurrence (macro token
            // tree): the compiler still proved the call, so keep it.
            let (Some(target), Some(src)) = (
                target,
                nodes_by_file
                    .get(file)
                    .into_iter()
                    .flatten()
                    .filter(|n| span_contains(n.span, pos))
                    .min_by_key(|n| n.span.end - n.span.start),
            ) else {
                continue;
            };
            let end = match occ.range.as_slice() {
                [_, _, c] => to_byte(file, occ.range[0], *c),
                [_, _, l, c] => to_byte(file, *l, *c),
                _ => None,
            }
            .unwrap_or(pos);
            unanchored.push(Edge {
                src: src.id.clone(),
                dst: target.id.clone(),
                relation: match target.kind {
                    SymbolKind::Function | SymbolKind::Method => Relation::Calls,
                    _ => Relation::Uses,
                },
                evidence: sinter_core::Evidence::Scip,
                confidence: sinter_core::Evidence::Scip.confidence(),
                site: Some(Span { start: pos, end }),
            });
        }
    }
    let mut external_nodes: HashMap<String, Node> = HashMap::new();
    for node in dep_cache.into_values().flatten() {
        external_nodes.insert(node.id.as_str().to_string(), node);
    }
    ScipResolution {
        bindings: best
            .into_iter()
            .map(|(reference, (_, edge))| Binding { edge, reference })
            .collect(),
        external: best_ext
            .into_iter()
            .map(|(reference, (_, edge))| Binding { edge, reference })
            .collect(),
        external_nodes: external_nodes.into_values().collect(),
        external_skipped: skipped.len(),
        unanchored,
    }
}

/// Parse an external SCIP moniker into a dependency-surface node (D29):
/// `<scheme> <manager> <package> <version> <descriptors...>` (`.` fields
/// mean absent — no package identity, no node). Descriptors join into a
/// `::` qualified path rooted at the package name (dashes normalized to
/// underscores, matching language path heads); the final descriptor marker
/// picks the kind:
///   `().` -> Function (Method when the previous segment is a type `#`)
///   `#`   -> Struct (honest default for SCIP "type"; the moniker cannot
///            distinguish struct/enum/trait)
///   `!`   -> Macro
///   `/`   -> Module
///   `.`   -> Constant (SCIP "term": consts, statics, fields)
/// Node id follows the `{file}#{qualified}@{offset}` convention at offset
/// 0 with span 0..0 — dep pseudo-files have no source.
fn dep_node(symbol: &str) -> Option<Node> {
    let mut parts = symbol.splitn(5, ' ');
    let _scheme = parts.next()?;
    let manager = parts.next()?;
    let package = parts.next()?;
    let version = parts.next()?;
    let descriptors = parts.next()?;
    if manager == "."
        || package == "."
        || package.is_empty()
        || version == "."
        || version.is_empty()
    {
        return None;
    }
    let (path, kind) = parse_descriptors(descriptors)?;
    let file = format!("dep:{package}@{version}");
    let head = package.replace('-', "_");
    let name = path.last().cloned().unwrap_or_else(|| head.clone());
    let qualified = std::iter::once(head)
        .chain(path)
        .collect::<Vec<_>>()
        .join("::");
    Some(Node {
        id: NodeId::new(format!("{file}#{qualified}@0")),
        kind,
        name,
        file,
        span: Span { start: 0, end: 0 },
        signature: String::new(),
        doc: None,
    })
}

/// Split a SCIP descriptor chain into path segments plus the kind its
/// final marker implies. Backtick-escaped names unescape; parameter
/// descriptors `(x)`, type parameters `[x]`, and malformed shapes yield
/// None (the caller counts them skipped).
fn parse_descriptors(desc: &str) -> Option<(Vec<String>, SymbolKind)> {
    // (segment, marker): marker is the descriptor suffix, with 'm' for
    // the method shape `name().`.
    let mut segs: Vec<(String, char)> = Vec::new();
    let mut cur = String::new();
    let mut chars = desc.chars();
    while let Some(c) = chars.next() {
        match c {
            '`' => loop {
                match chars.next()? {
                    '`' => break,
                    escaped => cur.push(escaped),
                }
            },
            '/' | '#' | '.' | '!' | ':' => {
                if cur.is_empty() {
                    return None;
                }
                segs.push((std::mem::take(&mut cur), c));
            }
            '(' => {
                if cur.is_empty() {
                    // `(param)` parameter descriptor — not a surface item.
                    return None;
                }
                loop {
                    if chars.next()? == ')' {
                        break;
                    }
                }
                if chars.next() != Some('.') {
                    return None;
                }
                segs.push((std::mem::take(&mut cur), 'm'));
            }
            '[' => return None,
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() || segs.is_empty() {
        return None;
    }
    let kind = match segs.last()?.1 {
        'm' if segs.len() >= 2 && segs[segs.len() - 2].1 == '#' => SymbolKind::Method,
        'm' => SymbolKind::Function,
        '#' => SymbolKind::Struct,
        '/' => SymbolKind::Module,
        '!' => SymbolKind::Macro,
        '.' => SymbolKind::Constant,
        // meta descriptor tail — nothing an agent asks "what breaks" about.
        _ => return None,
    };
    Some((segs.into_iter().map(|(s, _)| s).collect(), kind))
}

fn span_contains(span: Span, pos: u64) -> bool {
    span.start <= pos && pos < span.end
}
