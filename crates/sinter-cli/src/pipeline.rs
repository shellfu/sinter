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
use sinter_store::{FileStamp, Store};

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

/// Resolve the graph root for a path the way git resolves `.git`: the
/// path itself when it already has `.sinter`, else the nearest ancestor
/// that does. A path with no graph anywhere resolves to itself (a first
/// `sinter build` creates the graph right there).
pub fn discover_root(path: &Path) -> PathBuf {
    let Ok(canon) = path.canonicalize() else {
        return path.to_path_buf();
    };
    let mut current = canon.as_path();
    loop {
        if current.join(".sinter").is_dir() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return canon,
        }
    }
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
/// stat stamps for hash reuse and fallback hashes for transiently
/// unreadable files.
pub fn scan_hashes(
    repo: &Path,
    stored: &HashMap<String, FileStamp>,
) -> Result<Vec<(String, String)>> {
    Ok(scan(repo, stored)?
        .hashes
        .into_iter()
        .map(|(f, s)| (f, s.hash))
        .collect())
}

/// One walk, two harvests: language files to hash, and package manifests
/// (Cargo.toml, ...) whose declared names become module roots for the
/// resolver. Piggybacked so incremental builds never walk twice.
/// (file, stamp) rows plus the manifest-declared module roots.
pub struct Scan {
    pub hashes: Vec<(String, FileStamp)>,
    pub roots: Vec<ModuleRoot>,
}

/// mtime-gated hashing (the `make` trick): a file whose stat (mtime, len)
/// matches its stored stamp reuses the stored hash without being read, so
/// a clean scan is O(stat), not O(corpus bytes). Standard make caveat: a
/// rewrite that preserves both mtime and length with different content is
/// invisible. Set SINTER_FULL_SCAN=1 to force content hashing (escape
/// hatch for filesystems with untrustworthy mtimes).
pub fn scan(repo: &Path, stored: &HashMap<String, FileStamp>) -> Result<Scan> {
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
    let full_scan = std::env::var_os("SINTER_FULL_SCAN").is_some();
    let hashes = current
        .par_iter()
        .filter_map(|rel| {
            let path = repo.join(rel);
            // Stat identity of this file right now; (0, 0) when the stat
            // fails or the mtime predates the epoch — matches no stored
            // stamp (empty files are never stamped), so the file is read.
            let (mtime_nanos, len) = std::fs::metadata(&path)
                .ok()
                .map(|m| {
                    let nanos = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos());
                    (nanos, m.len())
                })
                .unwrap_or((0, 0));
            if !full_scan
                && len > 0
                && let Some(stamp) = stored.get(rel)
                && stamp.mtime_nanos == mtime_nanos
                && stamp.len == len
            {
                return Some((rel.clone(), stamp.clone()));
            }
            match std::fs::read(&path) {
                Ok(bytes) if bytes.is_empty() => None,
                Ok(bytes) => Some((
                    rel.clone(),
                    FileStamp {
                        hash: blake3::hash(&bytes).to_hex().to_string(),
                        mtime_nanos,
                        len: bytes.len() as u64,
                    },
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                // Transient read error (permissions, editor race): keep the
                // stored state instead of tearing the file down as removed.
                Err(_) => stored.get(rel).map(|s| (rel.clone(), s.clone())),
            }
        })
        .collect();
    Ok(Scan { hashes, roots })
}

/// One incremental build pass. `only` narrows the scan to an explicit
/// changed set (watcher/hook fast path); None scans the whole corpus.
pub fn build(repo: &Path, only: Option<&[PathBuf]>) -> Result<BuildReport> {
    let started = Instant::now();
    let repo = discover_root(repo);
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    let out_dir = repo.join(".sinter");
    std::fs::create_dir_all(&out_dir)?;
    // Current-schema databases open plain (no ensure-tables write
    // transaction); create — with its wipe-on-mismatch — runs only on
    // first build or schema change. Keeps a clean build write-free and
    // pays a single Database::open per build.
    let db = db_path(&repo);
    let mut store = match db
        .exists()
        .then(|| Store::open(&db))
        .transpose()?
        .filter(|s| s.schema().ok().flatten() == Some(Store::CURRENT_SCHEMA))
    {
        Some(store) => store,
        // Drops any mismatched handle first: create must reopen to wipe.
        None => Store::create(&db)?,
    };

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
    let stored: HashMap<String, FileStamp> = store.file_hashes()?.into_iter().collect();
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
        .filter(|(f, s)| stored.get(f).map(|st| &st.hash) != Some(&s.hash) && in_scope(f))
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
    // Non-source resolution inputs can move bindings in files whose source
    // did not change: a new/regenerated SCIP index, or a manifest edit that
    // renames a module root (package rename with imports untouched). Either
    // fingerprint changing re-resolves the whole corpus, but never
    // re-extracts (facts are content-addressed and untouched). The index
    // uses len:mtime, not a content hash — indexes run to hundreds of MB
    // and this runs on every build.
    let scip_fingerprint = scip_index_path(&repo).and_then(|p| {
        let meta = std::fs::metadata(&p).ok()?;
        let mtime = meta.modified().ok()?;
        let nanos = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        Some(format!("{}:{}", meta.len(), nanos))
    });
    let roots_fingerprint = {
        let mut hasher = blake3::Hasher::new();
        for root in &module_roots {
            hasher.update(root.name.as_bytes());
            hasher.update(b"\0");
            hasher.update(root.dir.as_bytes());
            hasher.update(b"\0");
            hasher.update(root.language.as_bytes());
            hasher.update(b"\n");
        }
        Some(hasher.finalize().to_hex().to_string())
    };
    if store.resolve_fingerprint("scip")? != scip_fingerprint
        || store.resolve_fingerprint("module_roots")? != roots_fingerprint
    {
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
                    // `crate::module` refs: internal binds the `mod x;`
                    // declaration node, SCIP binds the file node — the
                    // same module under two identities, not a conflict.
                    let internal_name = dst
                        .as_str()
                        .split_once('#')
                        .and_then(|(_, rest)| rest.split('@').next());
                    let scip_file_stem = (!binding.edge.dst.as_str().contains('#'))
                        .then(|| binding.edge.dst.as_str())
                        .and_then(|p| p.rsplit('/').next())
                        .and_then(|f| f.split('.').next());
                    if *dst == binding.edge.dst
                        || (internal_name.is_some() && internal_name == scip_file_stem)
                    {
                        stats.scip_agree += 1;
                    } else {
                        stats.scip_disagree += 1;
                        // Each disagreement is a defect with an unknown
                        // owner (resolver or cross-check); dump enough to
                        // classify it.
                        if std::env::var_os("SINTER_SCIP_DIFF").is_some() {
                            let r = &refs[binding.reference];
                            eprintln!(
                                "SCIP-DIFF {}:{}..{} `{}` internal={} scip={}",
                                r.file, r.span.start, r.span.end, r.name, dst, binding.edge.dst,
                            );
                        }
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
    // Two kinds of rows: freshly derived files, and touched-but-unchanged
    // files (same hash, new mtime/len) whose stamp must refresh or every
    // future scan would re-hash them forever. One write per real touch;
    // a fully clean build commits nothing and opens no write transaction.
    let derived: HashSet<&str> = changed_facts.iter().map(|f| f.file.as_str()).collect();
    let stamp_rows: Vec<(String, FileStamp)> = hashes
        .iter()
        .filter(|(f, s)| {
            derived.contains(f.as_str())
                || stored.get(f).is_some_and(|st| {
                    st.hash == s.hash && (st.mtime_nanos, st.len) != (s.mtime_nanos, s.len)
                })
        })
        .cloned()
        .collect();
    store.commit_stamps(&stamp_rows)?;
    store.set_resolve_fingerprint("scip", scip_fingerprint.as_deref())?;
    store.set_resolve_fingerprint("module_roots", roots_fingerprint.as_deref())?;

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
