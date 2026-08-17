//! The whole build pipeline, incremental by construction (R4): hash-diff the
//! corpus, re-extract only changed files, re-resolve only what the change
//! invalidates. Orchestration lives here in the binary only (R6).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use sinter_core::{FileFacts, Graph, Reference};
use sinter_extract::{Extractor, LanguageSpec, ModuleRoot, manifest_root, spec_for_path};
use sinter_store::Store;

pub struct BuildReport {
    pub scanned: usize,
    pub changed: usize,
    pub removed: usize,
    pub reresolved_files: usize,
    pub syntax_error_files: usize,
    pub failures: Vec<(String, String)>,
    pub stats: sinter_resolve::ResolutionStats,
    pub total_nodes: u64,
    pub total_edges: u64,
    pub total_unresolved: u64,
    pub elapsed: std::time::Duration,
}

pub fn db_path(repo: &Path) -> PathBuf {
    repo.join(".sinter").join("graph.redb")
}

/// The SCIP index to ingest, if any: `sinter scip` writes .sinter/index.scip;
/// a repo-root index.scip (hand-generated) still counts.
pub fn scip_index_path(repo: &Path) -> Option<PathBuf> {
    [
        repo.join(".sinter").join("index.scip"),
        repo.join("index.scip"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// Human-readable size of the graph database file — real allocated blocks
/// where available (redb files are sparse; apparent length overstates).
pub fn db_size(repo: &Path) -> String {
    match std::fs::metadata(db_path(repo)) {
        Ok(meta) => {
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::fs::MetadataExt;
                meta.blocks() * 512
            };
            #[cfg(not(unix))]
            let bytes = meta.len();
            if bytes >= 1 << 30 {
                format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
            } else if bytes >= 1 << 20 {
                format!("{:.1}M", bytes as f64 / (1u64 << 20) as f64)
            } else {
                format!("{}K", bytes >> 10)
            }
        }
        Err(_) => "?".to_string(),
    }
}

/// Walk the repo and hash every language-matched file. `stored` supplies
/// fallback hashes for transiently unreadable files.
pub fn scan_hashes(repo: &Path, stored: &HashMap<String, String>) -> Result<Vec<(String, String)>> {
    Ok(scan(repo, stored)?.hashes)
}

/// One walk, two harvests: language files to hash, and package manifests
/// (Cargo.toml, ...) whose declared names become module roots for the
/// resolver. Piggybacked so incremental builds never walk twice.
/// (file, blake3) rows plus the manifest-declared module roots.
pub struct Scan {
    pub hashes: Vec<(String, String)>,
    pub roots: Vec<ModuleRoot>,
}

pub fn scan(repo: &Path, stored: &HashMap<String, String>) -> Result<Scan> {
    let mut current: Vec<String> = Vec::new();
    let mut roots: Vec<ModuleRoot> = Vec::new();
    for entry in ignore::WalkBuilder::new(repo).build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(repo).unwrap_or(entry.path()));
        if rel.starts_with(".sinter/") {
            continue;
        }
        if spec_for_path(&rel).is_none() {
            let base = rel.rsplit('/').next().unwrap_or(&rel);
            if sinter_extract::LANGUAGES
                .iter()
                .any(|l| l.manifest.is_some_and(|m| m.filename == base))
                && let Ok(content) = std::fs::read_to_string(entry.path())
                && let Some(root) = manifest_root(&rel, &content)
            {
                roots.push(root);
            }
            continue;
        }
        current.push(rel);
    }
    roots.sort_by(|a, b| (&a.dir, &a.name).cmp(&(&b.dir, &b.name)));
    let hashes = current
        .par_iter()
        .filter_map(|rel| match std::fs::read(repo.join(rel)) {
            Ok(bytes) if bytes.is_empty() => None,
            Ok(bytes) => Some((rel.clone(), blake3::hash(&bytes).to_hex().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            // Transient read error (permissions, editor race): keep the
            // stored state instead of tearing the file down as removed.
            Err(_) => stored.get(rel).map(|h| (rel.clone(), h.clone())),
        })
        .collect();
    Ok(Scan { hashes, roots })
}

/// One incremental build pass. `only` narrows the scan to an explicit
/// changed set (watcher/hook fast path); None scans the whole corpus.
pub fn build(repo: &Path, only: Option<&[PathBuf]>) -> Result<BuildReport> {
    let started = Instant::now();
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    let out_dir = repo.join(".sinter");
    std::fs::create_dir_all(&out_dir)?;
    let mut store = Store::create(db_path(&repo))?;

    // Current corpus: (file, hash) for every language-matched file in scope.
    let scoped: Option<HashSet<String>> = only.map(|paths| {
        paths
            .iter()
            .filter_map(|p| {
                p.strip_prefix(&repo)
                    .ok()
                    .or(Some(p.as_path()))
                    .map(sinter_core::rel_display)
            })
            .collect()
    });
    let stored: HashMap<String, String> = store.file_hashes()?.into_iter().collect();
    let Scan {
        hashes,
        roots: module_roots,
    } = scan(&repo, &stored)?;
    let current_set: HashSet<&str> = hashes.iter().map(|(f, _)| f.as_str()).collect();
    // Scope entries match exactly or as directory prefixes: a directory
    // rename arrives as one event on the dir, covering its whole subtree.
    let in_scope = |file: &str| {
        scoped.as_ref().is_none_or(|s| {
            s.contains(file)
                || s.iter().any(|p| {
                    file.strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('/'))
                })
        })
    };
    let changed_files: Vec<&str> = hashes
        .iter()
        .filter(|(f, h)| stored.get(f) != Some(h) && in_scope(f))
        .map(|(f, _)| f.as_str())
        .collect();
    let removed: Vec<String> = stored
        .keys()
        .filter(|f| !current_set.contains(f.as_str()) && in_scope(f))
        .cloned()
        .collect();

    // Extract changed files in parallel, extractors pooled per language.
    let mut by_lang: Vec<(&'static LanguageSpec, Vec<&str>)> = Vec::new();
    for rel in &changed_files {
        let spec = spec_for_path(rel).expect("filtered above");
        match by_lang.iter_mut().find(|(s, _)| s.name == spec.name) {
            Some((_, files)) => files.push(rel),
            None => by_lang.push((spec, vec![rel])),
        }
    }
    let mut changed_facts: Vec<FileFacts> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut syntax_error_files = 0usize;
    for (spec, files) in &by_lang {
        let results: Vec<(String, Result<FileFacts, String>)> = files
            .par_iter()
            .map_init(
                || Extractor::new(spec),
                |extractor, rel| {
                    let result = extractor
                        .as_mut()
                        .map_err(|e| e.to_string())
                        .and_then(|ex| {
                            let source = std::fs::read_to_string(repo.join(rel))
                                .map_err(|e| e.to_string())?;
                            ex.extract(rel, &source).map_err(|e| e.to_string())
                        });
                    (rel.to_string(), result)
                },
            )
            .collect();
        for (rel, result) in results {
            match result {
                Ok(facts) => {
                    if facts.has_syntax_errors {
                        syntax_error_files += 1;
                    }
                    changed_facts.push(facts);
                }
                Err(message) => failures.push((rel, message)),
            }
        }
    }

    // Validate invariants per changed file before anything persists.
    for facts in &changed_facts {
        let mut g = Graph::new();
        for node in &facts.nodes {
            g.add_node(node.clone())
                .with_context(|| format!("invalid facts for {}", facts.file))?;
        }
        for edge in &facts.contains {
            g.add_edge(edge.clone())
                .with_context(|| format!("invalid facts for {}", facts.file))?;
        }
    }

    // Derive: replace per-file state, then re-resolve the invalidated set.
    let delta = store.update_files(&changed_facts, &removed)?;
    let mut affected: BTreeSet<String> = store.ref_files(&delta.def_names)?;
    affected.extend(delta.dependent_files.iter().cloned());
    for facts in &changed_facts {
        affected.insert(facts.file.clone());
    }
    for file in &removed {
        affected.remove(file);
    }
    // A changed SCIP index moves bindings in files whose source did not
    // change: re-resolve the whole corpus, but never re-extract (facts are
    // content-addressed and untouched). len:mtime fingerprint, not a content
    // hash — indexes run to hundreds of MB and this runs on every build.
    let scip_fingerprint = scip_index_path(&repo).and_then(|p| {
        let meta = std::fs::metadata(&p).ok()?;
        let mtime = meta.modified().ok()?;
        let nanos = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(format!("{}:{}", meta.len(), nanos))
    });
    if store.scip_fingerprint()? != scip_fingerprint {
        affected.extend(hashes.iter().map(|(f, _)| f.clone()));
        for file in &removed {
            affected.remove(file);
        }
    }

    let mut stats = sinter_resolve::ResolutionStats::default();
    if !affected.is_empty() {
        store.remove_resolution_edges(&affected)?;
        let nodes = store.all_nodes()?;
        let all_imports = store.all_imports()?;
        let mut refs: Vec<Reference> = Vec::new();
        let mut locals: Vec<sinter_core::LocalBinding> = Vec::new();
        let mut embeds: Vec<sinter_core::Embed> = Vec::new();
        for file in &affected {
            if let Some(facts) = store.facts(file)? {
                refs.extend(facts.references);
                locals.extend(facts.locals);
                embeds.extend(facts.embeds);
            }
        }

        // Internal evidence first; SCIP then binds what is left, moving
        // each hit out of its unresolved bucket.
        let (bindings, resolved_stats, internal_indices) =
            sinter_resolve::resolve(&nodes, &refs, &locals, &all_imports, &embeds, &module_roots);
        stats = resolved_stats;
        let internal_set: HashSet<usize> = internal_indices.into_iter().collect();
        let mut resolved_idx: HashSet<usize> = HashSet::new();
        let mut internal_dst: HashMap<usize, sinter_core::NodeId> = HashMap::new();
        let mut edges = Vec::new();
        for binding in bindings {
            resolved_idx.insert(binding.reference);
            internal_dst.insert(binding.reference, binding.edge.dst.clone());
            edges.push(binding.edge);
        }
        if let Some(scip_path) = scip_index_path(&repo) {
            let index = sinter_resolve::load_index(&scip_path)?;
            for binding in sinter_resolve::resolve_with_index(&index, &nodes, &refs, |rel| {
                std::fs::read_to_string(repo.join(rel)).ok()
            }) {
                if resolved_idx.insert(binding.reference) {
                    stats.scip += 1;
                    if internal_set.contains(&binding.reference) {
                        stats.unresolved_internal -= 1;
                    } else {
                        stats.unresolved_external -= 1;
                    }
                    edges.push(binding.edge);
                } else if let Some(dst) = internal_dst.get(&binding.reference) {
                    // Both tiers bound this ref: score internal evidence
                    // against the compiler's answer.
                    if *dst == binding.edge.dst {
                        stats.scip_agree += 1;
                    } else {
                        stats.scip_disagree += 1;
                    }
                }
            }
        }
        let unresolved: Vec<Reference> = refs
            .iter()
            .enumerate()
            .filter(|(i, _)| !resolved_idx.contains(i))
            .map(|(_, r)| r.clone())
            .collect();
        store.insert_edges(&edges)?;
        store.replace_unresolved(&affected, &unresolved)?;
    }

    if !failures.is_empty() && changed_facts.is_empty() && changed_files.len() == failures.len() {
        bail!(
            "every changed file failed extraction; first: {:?}",
            failures[0]
        );
    }

    // Hashes commit only now: every derived table is consistent, so a crash
    // anywhere above re-runs these files as changed on the next build.
    store.commit_hashes(&changed_facts)?;
    store.set_scip_fingerprint(scip_fingerprint.as_deref())?;

    // Reclaim free pages after bulk (re)builds; never on incremental
    // updates — compaction rewrites the file and would blow the <1s
    // one-file-edit budget. Half the redb file was page slack on the
    // benchmark corpus before this.
    if changed_facts.len() * 2 >= hashes.len() && !changed_facts.is_empty() {
        store.compact()?;
    }

    Ok(BuildReport {
        scanned: hashes.len(),
        changed: changed_facts.len(),
        removed: removed.len(),
        reresolved_files: affected.len(),
        syntax_error_files,
        failures,
        stats,
        total_nodes: store.node_count()?,
        total_edges: store.edge_count()?,
        total_unresolved: store.unresolved_count()?,
        elapsed: started.elapsed(),
    })
}

pub fn print_report(report: &BuildReport) {
    println!(
        "sinter build: {} scanned, {} changed, {} removed, {} files re-resolved, {} syntax-error files, {} failed, {:.1?}",
        report.scanned,
        report.changed,
        report.removed,
        report.reresolved_files,
        report.syntax_error_files,
        report.failures.len(),
        report.elapsed,
    );
    println!(
        "  resolution (this pass): {} resolved (scip {}, import {}, scope {}), {} unresolved ({} internal, {} external)",
        report.stats.resolved(),
        report.stats.scip,
        report.stats.import,
        report.stats.scope,
        report.stats.unresolved(),
        report.stats.unresolved_internal,
        report.stats.unresolved_external,
    );
    println!(
        "  accuracy gauge: {:.1}% internal-unresolved (external refs need dependency indexes, not resolver fixes)",
        report.stats.internal_unresolved_rate() * 100.0,
    );
    let both = report.stats.scip_agree + report.stats.scip_disagree;
    if both > 0 {
        println!(
            "  scip cross-check: {:.1}% of internally-bound refs match the compiler ({}/{} agree)",
            report.stats.scip_agree as f64 / both as f64 * 100.0,
            report.stats.scip_agree,
            both,
        );
    }
    println!(
        "  totals: {} nodes, {} edges, {} unresolved refs",
        report.total_nodes, report.total_edges, report.total_unresolved,
    );
    for (rel, message) in &report.failures {
        eprintln!("  FAILED {rel}: {message}");
    }
}
