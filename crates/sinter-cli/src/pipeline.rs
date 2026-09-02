//! The whole build pipeline, incremental by construction (R4): hash-diff the
//! corpus, re-extract only changed files, re-resolve only what the change
//! invalidates. Orchestration lives here in the binary only (R6).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use sinter_core::{
    CorpusScope, FileFacts, Graph, Reference, UnresolvedReason, UnresolvedReference,
};
use sinter_extract::{Extractor, LanguageSpec, ModuleRoot, manifest_root, spec_for_path};
use sinter_store::{FileStamp, Store};

pub struct BuildReport {
    pub scanned: usize,
    pub changed: usize,
    pub removed: usize,
    pub reresolved_files: usize,
    pub syntax_error_files: Vec<String>,
    /// Symbols extracted from those files before/around the errors.
    pub syntax_error_symbols: usize,
    pub failures: Vec<(String, String)>,
    pub scip_disagreements: Vec<ScipDisagreement>,
    pub stats: sinter_resolve::ResolutionStats,
    /// Distinct dependency-surface symbols bound this pass (D29).
    pub dep_symbols: usize,
    /// Distinct packages those symbols belong to.
    pub dep_packages: usize,
    pub total_nodes: u64,
    pub total_edges: u64,
    pub total_unresolved: u64,
    pub elapsed: std::time::Duration,
    /// A SCIP index was ingested that predates the newest source file:
    /// its bindings are compiler evidence for code that has since moved.
    pub scip_stale: bool,
    /// FILE_SCOPE rows whose classification changed without the file changing.
    pub scope_rows_restamped: usize,
}

/// Build progress, reported as it happens. The graph is complete and
/// correct at `Ready`; `Compacting` is optional maintenance that follows
/// it, so a build interrupted there loses nothing but page slack.
pub enum Phase {
    Scanning,
    Scanned {
        files: usize,
        changed: usize,
        removed: usize,
    },
    Extracting {
        files: usize,
    },
    Resolving {
        files: usize,
    },
    /// The ingested index is older than the corpus it binds.
    ScipStale,
    Ready {
        nodes: u64,
        edges: u64,
        elapsed: std::time::Duration,
    },
    Compacting {
        before: u64,
    },
    Compacted {
        before: u64,
        after: u64,
    },
}

/// One reference for which internal evidence and compiler evidence chose
/// different targets. Kept in the report so disagreements are actionable
/// without rerunning the build under a diagnostic environment variable.
pub struct ScipDisagreement {
    pub file: String,
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub internal: sinter_core::NodeId,
    pub scip: sinter_core::NodeId,
}

pub fn db_path(repo: &Path) -> PathBuf {
    repo.join(".sinter").join("graph.redb")
}

/// Resolve the graph root for a path without crossing repository ownership.
/// Inside Git, the nearest `.sinter` on the way to the repository boundary
/// wins; otherwise the `.git` directory or worktree file is the root for a
/// first build. Outside Git, the nearest ancestor `.sinter` owns the graph.
pub fn discover_root(path: &Path) -> PathBuf {
    let Ok(canon) = path.canonicalize() else {
        return path.to_path_buf();
    };
    let git_root = canon.ancestors().find(|dir| dir.join(".git").exists());
    nearest_graph_root(&canon, git_root)
}

/// Select a graph root under an already-resolved repository boundary. Keeping
/// boundary detection outside this function makes the non-Git contract
/// testable without inheriting `.git` markers from the test host.
fn nearest_graph_root(canon: &Path, git_root: Option<&Path>) -> PathBuf {
    for current in canon.ancestors() {
        if current.join(".sinter").is_dir() {
            return current.to_path_buf();
        }
        if git_root == Some(current) {
            return current.to_path_buf();
        }
    }
    canon.to_path_buf()
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
            human_bytes(bytes)
        }
        Err(_) => "?".to_string(),
    }
}

/// Byte counts as humans read them (`139.2M`). Shared by the on-disk
/// report and the compaction phase lines.
pub fn human_bytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1}M", bytes as f64 / (1u64 << 20) as f64)
    } else {
        format!("{}K", bytes >> 10)
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
    pub scopes: Vec<(String, CorpusScope)>,
    /// Newest source mtime seen this walk (0 when the corpus is empty).
    /// Harvested here because the walk already stats every file — SCIP
    /// staleness would otherwise cost a second traversal per build.
    pub newest_source_nanos: u128,
}

/// Stat-gated hashing (the `make` trick): a file whose identity and length
/// match its stored stamp reuses the stored hash without being read, so a
/// clean scan is O(stat), not O(corpus bytes). Unix identity includes ctime
/// as well as mtime, catching rewrites that restore mtime; other platforms
/// use mtime. Set SINTER_FULL_SCAN=1 to force content hashing.
pub fn scan(repo: &Path, stored: &HashMap<String, FileStamp>) -> Result<Scan> {
    let scope_policy = crate::corpus::ScopePolicy::load(repo)?;
    let mut current: Vec<String> = Vec::new();
    let mut roots: Vec<ModuleRoot> = Vec::new();
    let mut walker = ignore::WalkBuilder::new(repo);
    walker.add_custom_ignore_filename(".sinterignore");
    for entry in walker.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(repo).unwrap_or(entry.path()));
        if rel.starts_with(".sinter/") || crate::corpus::excluded(&rel) {
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
    let scan_one = |rel: &String| scan_file(repo, stored, rel, full_scan);
    // Starting Rayon is a net loss for the small repositories that dominate
    // interactive queries. Large corpora still parallelize stat/read work.
    const PARALLEL_SCAN_THRESHOLD: usize = 512;
    let scanned: Vec<(String, FileStamp, u128)> = if current.len() < PARALLEL_SCAN_THRESHOLD {
        current.iter().filter_map(scan_one).collect()
    } else {
        current.par_iter().filter_map(scan_one).collect()
    };
    let newest_source_nanos = scanned.iter().map(|(_, _, m)| *m).max().unwrap_or(0);
    let hashes: Vec<(String, FileStamp)> = scanned
        .into_iter()
        .map(|(file, stamp, _)| (file, stamp))
        .collect();
    let scopes = hashes
        .iter()
        .map(|(file, _)| (file.clone(), scope_policy.classify(file)))
        .collect();
    Ok(Scan {
        hashes,
        roots,
        scopes,
        newest_source_nanos,
    })
}

fn modified_nanos(metadata: &std::fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn metadata_identity(metadata: &std::fs::Metadata) -> u128 {
    let modified = modified_nanos(metadata);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let changed = u128::try_from(metadata.ctime())
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(u128::try_from(metadata.ctime_nsec()).unwrap_or(0));
        ((modified & u64::MAX as u128) << 64) | (changed & u64::MAX as u128)
    }
    #[cfg(not(unix))]
    {
        modified
    }
}

fn scan_file(
    repo: &Path,
    stored: &HashMap<String, FileStamp>,
    rel: &String,
    full_scan: bool,
) -> Option<(String, FileStamp, u128)> {
    let path = repo.join(rel);
    // (0, 0) on stat failure never matches a stored non-empty stamp.
    let metadata = std::fs::metadata(&path).ok();
    let modified = metadata.as_ref().map_or(0, modified_nanos);
    let (identity_nanos, len) = metadata
        .as_ref()
        .map(|m| (metadata_identity(m), m.len()))
        .unwrap_or((0, 0));
    if !full_scan
        && len > 0
        && let Some(stamp) = stored.get(rel)
        && stamp.identity_nanos == identity_nanos
        && stamp.len == len
    {
        return Some((rel.clone(), stamp.clone(), modified));
    }
    match std::fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => None,
        Ok(bytes) => Some((
            rel.clone(),
            FileStamp {
                hash: blake3::hash(&bytes).to_hex().to_string(),
                identity_nanos,
                len: bytes.len() as u64,
            },
            modified,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Transient read error (permissions, editor race): keep the stored
        // state instead of tearing the file down as removed.
        Err(_) => stored.get(rel).map(|s| (rel.clone(), s.clone(), modified)),
    }
}

fn read_repo_source(repo: &Path, rel: &str) -> Option<String> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    std::fs::read_to_string(repo.join(path)).ok()
}

/// One incremental build pass. `only` narrows the scan to an explicit
/// changed set (watcher/hook fast path); None scans the whole corpus.
/// Silent build: the query, hook, and MCP path. Every caller that wants
/// to show its work goes through `build_with`.
pub fn build(repo: &Path, only: Option<&[PathBuf]>) -> Result<BuildReport> {
    build_with(repo, only, &mut |_| {})
}

pub fn build_with(
    repo: &Path,
    only: Option<&[PathBuf]>,
    on: &mut dyn FnMut(Phase),
) -> Result<BuildReport> {
    let started = Instant::now();
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
    // Read-only first: redb's writable handle stamps the file header on
    // open and flushes allocator state on close, so merely opening one
    // rewrites the graph file even when the build turns out to be a no-op.
    // The handle upgrades to writable below, once there is real work.
    let mut store = match db
        .exists()
        .then(|| Store::open_read_only(&db).or_else(|_| Store::open(&db)))
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
    on(Phase::Scanning);
    let Scan {
        hashes,
        roots: module_roots,
        scopes,
        newest_source_nanos,
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
    // Facts are content-addressed against the *extractor* that produced
    // them: a new binary may extract different references from identical
    // bytes (query or classifier changes). Different binary → every file is
    // changed, so stale facts never survive an upgrade silently.
    let binary_fingerprint = Some(env!("CARGO_PKG_VERSION").to_string());
    let extractor_changed = store.resolve_fingerprint("binary")? != binary_fingerprint;
    let changed_files: Vec<&str> = hashes
        .iter()
        .filter(|(f, s)| {
            (extractor_changed || stored.get(f).map(|st| &st.hash) != Some(&s.hash)) && in_scope(f)
        })
        .map(|(f, _)| f.as_str())
        .collect();
    let removed: Vec<String> = stored
        .keys()
        .filter(|f| !current_set.contains(f.as_str()) && in_scope(f))
        .cloned()
        .collect();
    on(Phase::Scanned {
        files: hashes.len(),
        changed: changed_files.len(),
        removed: removed.len(),
    });
    // Non-source resolution inputs can move bindings in files whose source
    // did not change: a new/regenerated SCIP index, or a manifest edit that
    // renames a module root (package rename with imports untouched). Either
    // fingerprint changing re-resolves the whole corpus, but never
    // re-extracts (facts are content-addressed and untouched). The index
    // uses len:mtime, not a content hash — indexes run to hundreds of MB
    // and this runs on every build.
    // Index mtime serves twice: the resolve fingerprint, and the
    // staleness signal below. Ingestion is unconditional when the file
    // exists, so an index older than the corpus binds references the
    // compiler last saw somewhere else — say so instead of ingesting it
    // silently.
    let scip_mtime = scip_index_path(&repo).and_then(|p| {
        let meta = std::fs::metadata(&p).ok()?;
        let nanos = modified_nanos(&meta);
        Some((meta.len(), nanos))
    });
    let scip_fingerprint = scip_mtime.map(|(len, nanos)| format!("{len}:{nanos}"));
    let scip_stale = scip_mtime.is_some_and(|(_, nanos)| newest_source_nanos > nanos);
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
    let full_reresolve = store.resolve_fingerprint("scip")? != scip_fingerprint
        || store.resolve_fingerprint("module_roots")? != roots_fingerprint;

    // A no-op sync must be observably no-op: no write transaction, and no
    // writable redb handle either. Every write this build could perform is
    // already idempotent, so the whole build reduces to these predicates.
    let residue_pending = store.pending_delta()?;
    let stored_scopes = store.file_scopes()?;
    let stamp_refresh_due = hashes.iter().any(|(f, s)| {
        stored.get(f).is_some_and(|st| {
            st.hash == s.hash && (st.identity_nanos, st.len) != (s.identity_nanos, s.len)
        })
    });
    let idle = changed_files.is_empty()
        && removed.is_empty()
        && !full_reresolve
        && !stamp_refresh_due
        && residue_pending.def_names.is_empty()
        && residue_pending.dependent_files.is_empty()
        && scopes
            .iter()
            .all(|(file, scope)| stored_scopes.get(file) == Some(scope));
    if idle {
        let (total_nodes, total_edges, total_unresolved) = (
            store.node_count()?,
            store.edge_count()?,
            store.unresolved_count()?,
        );
        on(Phase::Ready {
            nodes: total_nodes,
            edges: total_edges,
            elapsed: started.elapsed(),
        });
        return Ok(BuildReport {
            scanned: hashes.len(),
            changed: 0,
            removed: 0,
            reresolved_files: 0,
            syntax_error_files: Vec::new(),
            syntax_error_symbols: 0,
            failures: Vec::new(),
            scip_disagreements: Vec::new(),
            stats: sinter_resolve::ResolutionStats::default(),
            dep_symbols: 0,
            dep_packages: 0,
            total_nodes,
            total_edges,
            total_unresolved,
            elapsed: started.elapsed(),
            scip_stale,
            scope_rows_restamped: 0,
        });
    }
    // Real work ahead: take the writable handle (and its exclusive lock).
    if store.is_read_only() {
        // Drop the shared lock before asking for the exclusive one.
        drop(store);
        store = Store::open(&db)?;
    }

    // Extract changed files in parallel, extractors pooled per language.
    let mut by_lang: Vec<(&'static LanguageSpec, Vec<&str>)> = Vec::new();
    for rel in &changed_files {
        let spec = spec_for_path(rel).expect("filtered above");
        match by_lang.iter_mut().find(|(s, _)| s.name == spec.name) {
            Some((_, files)) => files.push(rel),
            None => by_lang.push((spec, vec![rel])),
        }
    }
    if !changed_files.is_empty() {
        on(Phase::Extracting {
            files: changed_files.len(),
        });
    }
    let mut changed_facts: Vec<FileFacts> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut syntax_error_files = Vec::new();
    let mut syntax_error_symbols = 0usize;
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
                        syntax_error_files.push(rel.clone());
                        syntax_error_symbols += facts.nodes.len();
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
    // Classification is independent of source bytes. Persist it on every
    // build so editing only `.sinter.toml` immediately changes agent views.
    let scope_rows_restamped = store.set_file_scopes(&scopes)?;
    // Only file names are needed past this point; the full facts (gigabytes
    // on large corpora) drop here instead of living to end of build.
    let changed_names: Vec<String> = changed_facts.iter().map(|f| f.file.clone()).collect();
    drop(changed_facts);
    // Crash residue: deltas from a build that died before its resolution
    // pass committed. Their dependent files' in-edges are already gone and
    // cannot be recomputed this pass — replaying the persisted set is the
    // only way those bindings come back without a forced full rebuild.
    let residue = store.pending_delta()?;
    let mut invalidated_names = delta.def_names;
    invalidated_names.extend(residue.def_names);
    let mut affected: BTreeSet<String> = store.ref_files(&invalidated_names)?;
    affected.extend(delta.dependent_files.iter().cloned());
    affected.extend(residue.dependent_files);
    affected.extend(changed_names.iter().cloned());
    for file in &removed {
        affected.remove(file);
    }
    if full_reresolve {
        affected.extend(hashes.iter().map(|(f, _)| f.clone()));
        for file in &removed {
            affected.remove(file);
        }
    }

    let mut stats = sinter_resolve::ResolutionStats::default();
    let mut scip_disagreements = Vec::new();
    let mut dep_symbols = 0usize;
    let mut dep_packages = 0usize;
    if !affected.is_empty() {
        on(Phase::Resolving {
            files: affected.len(),
        });
        if scip_stale {
            on(Phase::ScipStale);
        }
        // Dynamic fan-out edges are re-derived from their dst files' facts;
        // any such file losing edges must join the set (its unchanged refs
        // re-resolve to identical edges — a set-semantics no-op). Read-only
        // lookahead: the actual edge teardown commits atomically with the
        // re-derived edges in apply_resolution below, so no crash window
        // exists between losing old bindings and gaining new ones.
        let teardown_files = affected.clone();
        let dynamic_dst = store.dynamic_edge_dst_files(&affected)?;
        affected.extend(dynamic_dst);
        // Stored dep-surface nodes (D29) are scip output, never internal-
        // evidence input: feeding them back would let scope/import bind to
        // synthesized nodes and contaminate the cross-check. Split them off
        // for lifecycle bookkeeping below.
        let (dep_nodes, nodes): (Vec<sinter_core::Node>, Vec<sinter_core::Node>) = store
            .all_nodes()?
            .into_iter()
            .partition(|n| n.file.starts_with("dep:"));
        let mut dep_stored: HashMap<String, Vec<sinter_core::Node>> = HashMap::new();
        for node in dep_nodes {
            dep_stored.entry(node.file.clone()).or_default().push(node);
        }
        let all_imports = store.all_imports()?;
        let mut refs: Vec<Reference> = Vec::new();
        let mut locals: Vec<sinter_core::LocalBinding> = Vec::new();
        let mut fields: Vec<sinter_core::FieldBinding> = Vec::new();
        let mut embeds: Vec<sinter_core::Embed> = Vec::new();
        let mut trait_impls: Vec<sinter_core::TraitImpl> = Vec::new();
        for file in &affected {
            if let Some(facts) = store.facts(file)? {
                refs.extend(facts.references);
                locals.extend(facts.locals);
                fields.extend(facts.fields);
                embeds.extend(facts.embeds);
                trait_impls.extend(facts.trait_impls);
            }
        }

        // Internal evidence first; SCIP then binds what is left, moving
        // each hit out of its unresolved bucket. One index serves both
        // the reference pass and the dynamic fan-out pass.
        let resolve_index = sinter_resolve::Index::build(
            &nodes,
            &all_imports,
            &locals,
            &fields,
            &embeds,
            &module_roots,
        );
        let (bindings, resolved_stats, internal_indices, dangling_indices) =
            sinter_resolve::resolve(&resolve_index, &refs);
        stats = resolved_stats;
        let internal_set: HashSet<usize> = internal_indices.into_iter().collect();
        let dangling_set: HashSet<usize> = dangling_indices.into_iter().collect();
        let mut resolved_idx: HashSet<usize> = HashSet::new();
        let mut internal_dst: HashMap<usize, sinter_core::NodeId> = HashMap::new();
        let mut edges = Vec::new();
        for binding in bindings {
            resolved_idx.insert(binding.reference);
            internal_dst.insert(binding.reference, binding.edge.dst.clone());
            edges.push(binding.edge);
        }
        edges.extend(sinter_resolve::dynamic_edges(
            &resolve_index,
            &nodes,
            &trait_impls,
        ));
        // Dep-surface facts derived this pass: pseudo-file -> its nodes.
        let mut dep_derived: HashMap<String, Vec<sinter_core::Node>> = HashMap::new();
        if let Some(scip_path) = scip_index_path(&repo) {
            let index = sinter_resolve::load_index(&scip_path)?;
            // A file edited since the index was built has moved: its
            // occurrence positions no longer name what the compiler saw,
            // so its evidence is withheld (the file already counts in the
            // stale-index notice) and import/scope evidence stands.
            let index_nanos = scip_mtime.map_or(0, |(_, nanos)| nanos);
            let resolution =
                sinter_resolve::resolve_with_index(&index, &nodes, &refs, &affected, |rel| {
                    read_repo_source(&repo, rel).filter(|_| {
                        std::fs::metadata(repo.join(rel))
                            .is_ok_and(|meta| modified_nanos(&meta) <= index_nanos)
                    })
                });
            for binding in resolution.bindings {
                if resolved_idx.insert(binding.reference) {
                    stats.scip += 1;
                    if internal_set.contains(&binding.reference) {
                        stats.compiler_rescued_internal += 1;
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
                        let r = &refs[binding.reference];
                        scip_disagreements.push(ScipDisagreement {
                            file: r.file.clone(),
                            start: r.span.start,
                            end: r.span.end,
                            name: r.name.clone(),
                            internal: dst.clone(),
                            scip: binding.edge.dst.clone(),
                        });
                    }
                }
            }
            // Dependency surface (D29): refs the compiler resolved into a
            // package bind to synthesized dep nodes — but never over an
            // existing bind (internal evidence or in-corpus scip wins).
            let mut used_dep_ids: HashSet<String> = HashSet::new();
            for binding in resolution.external {
                if resolved_idx.insert(binding.reference) {
                    stats.scip_external += 1;
                    if internal_set.contains(&binding.reference) {
                        stats.compiler_rescued_internal += 1;
                        stats.unresolved_internal -= 1;
                    } else {
                        stats.unresolved_external -= 1;
                    }
                    used_dep_ids.insert(binding.edge.dst.as_str().to_string());
                    edges.push(binding.edge);
                }
            }
            for node in resolution.external_nodes {
                if used_dep_ids.contains(node.id.as_str()) {
                    dep_derived.entry(node.file.clone()).or_default().push(node);
                }
            }
            // Occurrences no extracted reference anchors (macro token
            // trees): compiler-proven, so they become edges too.
            stats.scip_unanchored = resolution.unanchored.len();
            edges.extend(resolution.unanchored);
            dep_symbols = used_dep_ids.len();
            dep_packages = dep_derived.len();
        }

        // Dep pseudo-file lifecycle: the per-file replace machinery owns
        // it. A full re-resolve derived the complete surface, so replace
        // everything and drop packages that vanished. A partial pass only
        // re-derived the affected files' binds: merge stored nodes in so
        // untouched files' dep targets survive, and skip files whose facts
        // come out identical (an unchanged rebuild must not tear down and
        // rebuild the surface).
        if !full_reresolve {
            for (file, stored_nodes) in &dep_stored {
                if let Some(new) = dep_derived.get_mut(file) {
                    let have: HashSet<String> =
                        new.iter().map(|n| n.id.as_str().to_string()).collect();
                    new.extend(
                        stored_nodes
                            .iter()
                            .filter(|n| !have.contains(n.id.as_str()))
                            .cloned(),
                    );
                }
            }
        }
        let mut dep_changed: Vec<FileFacts> = Vec::new();
        let mut dep_present: HashSet<String> = HashSet::new();
        for (file, mut dep_nodes) in dep_derived {
            dep_present.insert(file.clone());
            dep_nodes.sort_by(|a, b| a.id.cmp(&b.id));
            if dep_stored.get(&file).is_some_and(|stored_nodes| {
                let mut stored_sorted = stored_nodes.clone();
                stored_sorted.sort_by(|a, b| a.id.cmp(&b.id));
                stored_sorted == dep_nodes
            }) {
                continue;
            }
            dep_changed.push(FileFacts {
                file,
                // Never committed to FILE_HASH: dep pseudo-files are not on
                // disk, so the scan neither hashes nor removes them — the
                // dep: removal exemption falls out of that.
                content_hash: String::new(),
                has_syntax_errors: false,
                nodes: dep_nodes,
                contains: Vec::new(),
                references: Vec::new(),
                locals: Vec::new(),
                fields: Vec::new(),
                embeds: Vec::new(),
                trait_impls: Vec::new(),
                scopes: Vec::new(),
                body_terms: Vec::new(),
            });
        }
        let dep_removed: Vec<String> = if full_reresolve {
            dep_stored
                .keys()
                .filter(|f| !dep_present.contains(f.as_str()))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if !dep_changed.is_empty() || !dep_removed.is_empty() {
            // update_files tears down every in-edge of a replaced file's
            // nodes, including binds from files outside this pass. Snapshot
            // those and re-insert them below; affected files' binds are
            // torn down inside apply_resolution and re-derived above.
            for facts in &dep_changed {
                for node in dep_stored.get(&facts.file).into_iter().flatten() {
                    for edge in store.in_edges(&node.id)? {
                        let src_file = edge
                            .src
                            .as_str()
                            .split_once('#')
                            .map_or(edge.src.as_str(), |(f, _)| f);
                        if !affected.contains(src_file) {
                            edges.push(edge);
                        }
                    }
                }
            }
            store.update_files(&dep_changed, &dep_removed)?;
        }
        let compiler_indexed = scip_fingerprint.is_some();
        let unresolved: Vec<UnresolvedReference> = refs
            .iter()
            .enumerate()
            .filter(|(i, _)| !resolved_idx.contains(i))
            .map(|(i, r)| UnresolvedReference {
                reference: r.clone(),
                reason: if dangling_set.contains(&i) {
                    UnresolvedReason::MissingInternalTarget
                } else if compiler_indexed {
                    UnresolvedReason::CompilerUnresolved
                } else if internal_set.contains(&i) {
                    UnresolvedReason::SyntaxAnchoredMiss
                } else {
                    UnresolvedReason::SyntaxOnly
                },
            })
            .collect();
        // One transaction: resolution-edge teardown + re-derived edge
        // insert + unresolved replace. The lethal crash window (teardown
        // committed, edges not) is gone by construction.
        store.apply_resolution(&teardown_files, &edges, &affected, &unresolved)?;
    }

    if !failures.is_empty() && changed_names.is_empty() && changed_files.len() == failures.len() {
        bail!(
            "every changed file failed extraction; first: {:?}",
            failures[0]
        );
    }

    // Hashes commit only now: every derived table is consistent, so a crash
    // anywhere above re-runs these files as changed on the next build.
    // Two kinds of rows: freshly derived files, and touched-but-unchanged
    // files (same hash, new stat identity) whose stamp must refresh or every
    // future scan would re-hash them forever. One write per real touch;
    // a fully clean build commits nothing and opens no write transaction.
    let derived: HashSet<&str> = changed_names.iter().map(String::as_str).collect();
    let stamp_rows: Vec<(String, FileStamp)> = hashes
        .iter()
        .filter(|(f, s)| {
            derived.contains(f.as_str())
                || stored.get(f).is_some_and(|st| {
                    st.hash == s.hash && (st.identity_nanos, st.len) != (s.identity_nanos, s.len)
                })
        })
        .cloned()
        .collect();
    store.commit_stamps(&stamp_rows)?;
    store.set_resolve_fingerprint("scip", scip_fingerprint.as_deref())?;
    store.set_resolve_fingerprint("module_roots", roots_fingerprint.as_deref())?;
    store.set_resolve_fingerprint("binary", binary_fingerprint.as_deref())?;
    // Everything above is committed; the crash-recovery intent has served
    // its purpose. (A crash between the stamps and here replays some files
    // redundantly next build — idempotent, same edges.)
    store.clear_pending_delta()?;

    crate::coverage::record_health(
        &repo,
        &changed_files,
        &removed,
        &syntax_error_files,
        &failures,
    )?;

    // The graph is complete and durable here. Report it BEFORE any
    // maintenance: compaction below rewrites a multi-hundred-megabyte
    // file and used to be the last thing between a finished build and
    // its first line of output, which reads as a hang.
    let (total_nodes, total_edges, total_unresolved) = (
        store.node_count()?,
        store.edge_count()?,
        store.unresolved_count()?,
    );
    on(Phase::Ready {
        nodes: total_nodes,
        edges: total_edges,
        elapsed: started.elapsed(),
    });

    // Reclaim free pages after bulk (re)builds; never on incremental
    // updates — compaction rewrites the file and would blow the <1s
    // one-file-edit budget. Half the redb file was page slack on the
    // benchmark corpus before this. Iterative and synchronous: the phase
    // is named so a long one is visibly maintenance, not a stall.
    if changed_names.len() * 2 >= hashes.len() && !changed_names.is_empty() {
        let before = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        on(Phase::Compacting { before });
        store.compact()?;
        let after = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        on(Phase::Compacted { before, after });
    }

    Ok(BuildReport {
        scanned: hashes.len(),
        changed: changed_names.len(),
        removed: removed.len(),
        reresolved_files: affected.len(),
        syntax_error_files,
        syntax_error_symbols,
        failures,
        scip_disagreements,
        stats,
        dep_symbols,
        dep_packages,
        total_nodes,
        total_edges,
        total_unresolved,
        elapsed: started.elapsed(),
        scip_stale,
        scope_rows_restamped,
    })
}

/// The onboarding view of a build: what the graph now contains, no
/// resolver diagnostics. `print_report` keeps those for `sinter build`,
/// where the audience already knows what an anchored miss rate is.
pub fn print_summary(repo: &Path, report: &BuildReport) {
    let resolved = report.stats.resolved();
    let total = resolved + report.stats.unresolved();
    let rate = if total > 0 {
        format!(
            " ({:.0}% of references bound)",
            resolved as f64 / total as f64 * 100.0
        )
    } else {
        String::new()
    };
    // Deliberately does not restate the symbol/edge counts: the `Ready`
    // phase line already carries those, and one number printed twice
    // reads as two different numbers.
    let restamped = match report.scope_rows_restamped {
        0 => String::new(),
        n => format!(", {n} scope rows re-stamped"),
    };
    println!(
        "  {} indexed{rate}{restamped}, {} on disk",
        crate::render::count(report.scanned, "file"),
        db_size(repo)
    );
    if !report.failures.is_empty() {
        println!(
            "  {} failed extraction — `sinter build` names them",
            crate::render::count(report.failures.len(), "file")
        );
    }
}

pub fn print_report(report: &BuildReport) {
    println!(
        "sinter build: {} scanned, {} changed, {} removed, {} files re-resolved, {} syntax-error files, {} failed, {:.1?}",
        report.scanned,
        report.changed,
        report.removed,
        report.reresolved_files,
        report.syntax_error_files.len(),
        report.failures.len(),
        report.elapsed,
    );
    if report.scope_rows_restamped > 0 {
        println!("  {} scope rows re-stamped", report.scope_rows_restamped);
    }
    println!(
        "  resolution (this pass): {} resolved (scip {}, import {}, scope {}), {} unresolved ({} internal, {} external), {} unanchored scip edges",
        report.stats.resolved(),
        report.stats.scip,
        report.stats.import,
        report.stats.scope,
        report.stats.unresolved(),
        report.stats.unresolved_internal,
        report.stats.unresolved_external,
        report.stats.scip_unanchored,
    );
    match report.stats.anchored_unresolved_rate() {
        Some(rate) => println!(
            "  anchored miss rate (this pass): {:.1}% (heuristic classification, not compiler-relative recall)",
            rate * 100.0,
        ),
        None => {
            println!("  anchored miss rate (this pass): not measured (no references re-resolved)")
        }
    }
    if report.scip_stale {
        println!(
            "  SCIP index is older than the newest source file — its bindings may be stale; rerun `sinter scip`"
        );
    }
    let both = report.stats.scip_agree + report.stats.scip_disagree;
    if both > 0 {
        println!(
            "  scip cross-check: {:.1}% of internally-bound refs match the compiler ({}/{} agree)",
            report.stats.scip_agree as f64 / both as f64 * 100.0,
            report.stats.scip_agree,
            both,
        );
    }
    // Recall against the compiler: scip binds only corpus-internal defs,
    // so every scip-only bind is a ref internal evidence could have found
    // and didn't. Precision (above) without this number flatters.
    let compiler_bound = both + report.stats.scip;
    if compiler_bound > 0 {
        println!(
            "  internal recall vs compiler: {:.1}% ({} of {} compiler-bound refs found without scip)",
            both as f64 / compiler_bound as f64 * 100.0,
            both,
            compiler_bound,
        );
    }
    if report.stats.scip_external > 0 {
        println!(
            "  dependency surface: {} refs bound to {} external symbols across {} packages",
            report.stats.scip_external, report.dep_symbols, report.dep_packages,
        );
    }
    println!(
        "  totals: {} nodes, {} edges, {} unresolved refs",
        report.total_nodes, report.total_edges, report.total_unresolved,
    );
    // One line, not one per file: the list belongs to `doctor --verbose`.
    if !report.syntax_error_files.is_empty() {
        eprintln!(
            "  {} parsed partially ({} in them; statements after the first syntax error are absent; `sinter doctor --verbose` lists files)",
            crate::render::count(report.syntax_error_files.len(), "file"),
            crate::render::count(report.syntax_error_symbols, "symbol"),
        );
    }
    const DEFAULT_DIFF_LIMIT: usize = 10;
    let diff_limit = if std::env::var_os("SINTER_SCIP_DIFF").is_some() {
        usize::MAX
    } else {
        DEFAULT_DIFF_LIMIT
    };
    for diff in report.scip_disagreements.iter().take(diff_limit) {
        eprintln!(
            "  SCIP-DIFF {}:{}..{} `{}` internal={} scip={}",
            diff.file, diff.start, diff.end, diff.name, diff.internal, diff.scip,
        );
    }
    if report.scip_disagreements.len() > diff_limit {
        eprintln!(
            "  SCIP-DIFF {} more disagreement(s); set SINTER_SCIP_DIFF=1 to print all",
            report.scip_disagreements.len() - diff_limit,
        );
    }
    for (rel, message) in &report.failures {
        eprintln!("  FAILED {rel}: {message}");
    }
}

#[cfg(test)]
mod source_path_tests {
    use std::collections::HashMap;

    use super::{read_repo_source, scan_hashes};

    #[test]
    fn scip_source_reads_stay_inside_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inside.rs"), "inside").unwrap();
        assert_eq!(
            read_repo_source(dir.path(), "inside.rs").as_deref(),
            Some("inside")
        );
        assert_eq!(read_repo_source(dir.path(), "../outside.rs"), None);
        assert_eq!(read_repo_source(dir.path(), "/etc/passwd"), None);
    }

    #[test]
    fn repository_sinterignore_excludes_deliberate_corpus_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("fixtures")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join(".sinterignore"), "fixtures/**\n").unwrap();
        std::fs::write(dir.path().join("fixtures/ignored.rs"), "fn ignored() {}\n").unwrap();
        std::fs::write(dir.path().join("src/indexed.rs"), "fn indexed() {}\n").unwrap();

        let files = scan_hashes(dir.path(), &HashMap::new())
            .unwrap()
            .into_iter()
            .map(|(file, _)| file)
            .collect::<Vec<_>>();
        assert_eq!(files, vec!["src/indexed.rs"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_discovery_stops_at_git_boundaries() {
        for git_is_file in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(parent.path().join(".sinter")).unwrap();
            let repo = parent.path().join(if git_is_file {
                "worktree-repo"
            } else {
                "ordinary-repo"
            });
            let nested = repo.join("src/deep");
            std::fs::create_dir_all(&nested).unwrap();
            if git_is_file {
                std::fs::write(repo.join(".git"), "gitdir: ../worktrees/repo\n").unwrap();
            } else {
                std::fs::create_dir_all(repo.join(".git")).unwrap();
            }

            assert_eq!(
                discover_root(&nested),
                repo.canonicalize().unwrap(),
                "discovery crossed a {} .git boundary into the parent graph",
                if git_is_file { "file" } else { "directory" }
            );
        }
    }

    #[test]
    fn graph_discovery_still_finds_the_current_repository_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(discover_root(&nested), repo.canonicalize().unwrap());
    }

    #[test]
    fn graph_discovery_finds_a_non_git_graph_without_host_git_state() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let nested = repo.join("src/deep");
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            nearest_graph_root(&nested.canonicalize().unwrap(), None),
            repo.canonicalize().unwrap()
        );
    }

    /// Compaction is maintenance, not construction: the graph must be
    /// reported complete before it starts. Reporting after it is what
    /// made a bulk build look hung for the length of a multi-hundred-
    /// megabyte file rewrite.
    #[test]
    fn graph_is_reported_ready_before_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("a.rs"), "pub fn f() { g(); }\npub fn g() {}\n").unwrap();

        let mut seen: Vec<&'static str> = Vec::new();
        build_with(repo, None, &mut |phase| {
            seen.push(match phase {
                Phase::Scanning => "scanning",
                Phase::Scanned { .. } => "scanned",
                Phase::Extracting { .. } => "extracting",
                Phase::Resolving { .. } => "resolving",
                Phase::ScipStale => "scip_stale",
                Phase::Ready { .. } => "ready",
                Phase::Compacting { .. } => "compacting",
                Phase::Compacted { .. } => "compacted",
            });
        })
        .unwrap();

        let ready = seen.iter().position(|p| *p == "ready").expect("ready");
        assert_eq!(seen.first(), Some(&"scanning"), "{seen:?}");
        assert!(seen.contains(&"extracting"), "{seen:?}");
        assert!(seen.contains(&"resolving"), "{seen:?}");
        // A first build rewrites the whole corpus, so compaction runs —
        // and every compaction phase must follow the ready line.
        assert!(seen.contains(&"compacting"), "{seen:?}");
        for (i, phase) in seen.iter().enumerate() {
            if phase.starts_with("compact") {
                assert!(i > ready, "{phase} preceded ready: {seen:?}");
            }
        }
    }

    /// Ingestion is unconditional whenever the index file exists, so an
    /// index older than the corpus binds references the compiler last saw
    /// elsewhere. The report has to carry that, not swallow it.
    #[test]
    fn stale_index_is_flagged_on_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
        // Empty but valid SCIP: binds nothing, ingests cleanly.
        let index = repo.join(".sinter/index.scip");
        std::fs::write(&index, b"").unwrap();

        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&index)
            .unwrap()
            .set_modified(past)
            .unwrap();
        assert!(build(repo, None).unwrap().scip_stale, "index predates a.rs");

        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&index)
            .unwrap()
            .set_modified(future)
            .unwrap();
        assert!(!build(repo, None).unwrap().scip_stale);
    }
}
