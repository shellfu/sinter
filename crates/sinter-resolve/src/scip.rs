use std::collections::HashMap;
use std::path::Path;

use protobuf::Message;
use scip::types::Index;
use sinter_core::{Edge, Node, NodeId, Reference, Span};

use crate::resolver::Binding;

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
pub fn resolve_with_index(
    index: &Index,
    nodes: &[Node],
    references: &[Reference],
    mut read_source: impl FnMut(&str) -> Option<String>,
) -> Vec<Binding> {
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
    let mut best: HashMap<usize, (u64, Edge)> = HashMap::new();
    for document in &index.documents {
        let Some(file_refs) = refs_by_file.get(document.relative_path.as_str()) else {
            continue;
        };
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
            let Some(target) = target else {
                continue;
            };
            let Some(pos) = occ
                .range
                .first()
                .zip(occ.range.get(1))
                .and_then(|(l, c)| to_byte(&document.relative_path, *l, *c))
            else {
                continue;
            };
            for (i, r) in file_refs {
                if !span_contains(r.span, pos) {
                    continue;
                }
                let distance = pos - r.span.start;
                if best.get(i).is_none_or(|(d, _)| distance > *d) {
                    best.insert(
                        *i,
                        (
                            distance,
                            Edge {
                                src: r
                                    .enclosing
                                    .clone()
                                    .unwrap_or_else(|| NodeId::new(r.file.clone())),
                                dst: target.id.clone(),
                                relation: r.relation,
                                evidence: sinter_core::Evidence::Scip,
                                confidence: sinter_core::Evidence::Scip.confidence(),
                            },
                        ),
                    );
                }
            }
        }
    }
    best.into_iter()
        .map(|(reference, (_, edge))| Binding { edge, reference })
        .collect()
}

fn span_contains(span: Span, pos: u64) -> bool {
    span.start <= pos && pos < span.end
}
