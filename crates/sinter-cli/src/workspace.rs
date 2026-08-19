//! Cross-repo workspaces: federation, not merger.
//! Each member repo keeps its own `.sinter/` store; a small link store next
//! to the manifest holds ONLY boundary edges. Boundary resolution consumes
//! each member's already-persisted unresolved references and binds them
//! against the other members' symbols with import evidence only; runtime
//! coupling comes exclusively from operator-declared manifest links.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use redb::{Database, MultimapTableDefinition, ReadableDatabase, TableDefinition};
use serde::Deserialize;
use sinter_core::{Confidence, Evidence, Node, NodeId, Relation};
use sinter_store::Store;

use crate::pipeline;

// ---------------------------------------------------------------- manifest

#[derive(Deserialize)]
pub struct Manifest {
    pub workspace: WorkspaceMeta,
    /// member name -> repo path
    pub members: BTreeMap<String, String>,
    #[serde(default)]
    pub links: Vec<DeclaredLink>,
}

#[derive(Deserialize)]
pub struct WorkspaceMeta {
    pub name: String,
}

/// Operator-declared runtime coupling — the only non-static evidence
/// allowed across repos, and it carries its own evidence kind.
#[derive(Deserialize)]
pub struct DeclaredLink {
    pub from_member: String,
    pub from_symbol: String,
    pub to_member: String,
    pub to_symbol: String,
    /// Human note ("topic payments.settled"); recorded, not interpreted.
    #[serde(default)]
    pub via: String,
}

pub struct Workspace {
    pub manifest: Manifest,
    pub manifest_dir: PathBuf,
    pub members: BTreeMap<String, PathBuf>,
}

pub fn load(manifest_path: &Path) -> Result<Workspace> {
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("workspace manifest {}", manifest_path.display()))?;
    let text = std::fs::read_to_string(&manifest_path)?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
    let dir = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut members = BTreeMap::new();
    for (name, path) in &manifest.members {
        let repo = dir
            .join(path)
            .canonicalize()
            .or_else(|_| PathBuf::from(shellexpand(path)).canonicalize());
        let repo = repo.with_context(|| format!("member `{name}` path {path}"))?;
        members.insert(name.clone(), repo);
    }
    Ok(Workspace {
        manifest,
        manifest_dir: dir,
        members,
    })
}

fn shellexpand(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
        _ => path.to_string(),
    }
}

// --------------------------------------------------------------- link store

/// A boundary edge between members.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct Link {
    pub src_member: String,
    pub src_id: String,
    pub dst_member: String,
    pub dst_id: String,
    pub relation: Relation,
    pub evidence: Evidence,
    pub confidence: Confidence,
    /// Declared links carry their manifest note.
    pub via: String,
}

const LINKS_OUT: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("links_out");
const LINKS_IN: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("links_in");
const MEMBER_FP: TableDefinition<&str, &str> = TableDefinition::new("member_fp");

fn link_key(member: &str, id: &str) -> String {
    format!("{member}\u{1f}{id}")
}

pub struct LinkStore {
    db: Database,
}

impl LinkStore {
    pub fn path(ws: &Workspace) -> PathBuf {
        ws.manifest_dir.join(".sinter-workspace").join("links.redb")
    }

    pub fn open(ws: &Workspace) -> Result<Self> {
        let path = Self::path(ws);
        std::fs::create_dir_all(path.parent().unwrap())?;
        // Same contention policy as the repository store: parallel
        // workspace queries all open this file.
        let db = sinter_store::create_database(&path)?;
        let txn = db.begin_write()?;
        {
            txn.open_multimap_table(LINKS_OUT)?;
            txn.open_multimap_table(LINKS_IN)?;
            txn.open_table(MEMBER_FP)?;
        }
        txn.commit()?;
        Ok(Self { db })
    }

    /// Replace all links in one transaction; record member fingerprints.
    pub fn replace(&self, links: &[Link], fingerprints: &BTreeMap<String, String>) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            txn.delete_multimap_table(LINKS_OUT)?;
            txn.delete_multimap_table(LINKS_IN)?;
            let mut out = txn.open_multimap_table(LINKS_OUT)?;
            let mut inn = txn.open_multimap_table(LINKS_IN)?;
            for link in links {
                let bytes = postcard::to_allocvec(link)?;
                out.insert(
                    link_key(&link.src_member, &link.src_id).as_str(),
                    bytes.as_slice(),
                )?;
                inn.insert(
                    link_key(&link.dst_member, &link.dst_id).as_str(),
                    bytes.as_slice(),
                )?;
            }
            let mut fp = txn.open_table(MEMBER_FP)?;
            for (name, print) in fingerprints {
                fp.insert(name.as_str(), print.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    fn links(
        &self,
        table: MultimapTableDefinition<&str, &[u8]>,
        member: &str,
        id: &str,
    ) -> Result<Vec<Link>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(table)?;
        let mut links = Vec::new();
        for guard in table.get(link_key(member, id).as_str())? {
            links.push(postcard::from_bytes(guard?.value())?);
        }
        Ok(links)
    }

    pub fn out_links(&self, member: &str, id: &str) -> Result<Vec<Link>> {
        self.links(LINKS_OUT, member, id)
    }

    pub fn in_links(&self, member: &str, id: &str) -> Result<Vec<Link>> {
        self.links(LINKS_IN, member, id)
    }

    pub fn count(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let txn = self.db.begin_read()?;
        Ok(txn.open_multimap_table(LINKS_OUT)?.len()?)
    }

    pub fn fingerprints(&self) -> Result<BTreeMap<String, String>> {
        use redb::ReadableTable;
        let txn = self.db.begin_read()?;
        let table = txn.open_table(MEMBER_FP)?;
        let mut out = BTreeMap::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            out.insert(k.value().to_string(), v.value().to_string());
        }
        Ok(out)
    }
}

/// Cheap member staleness fingerprint: db file length + mtime.
pub fn member_fingerprint(repo: &Path) -> String {
    match std::fs::metadata(pipeline::db_path(repo)) {
        Ok(meta) => format!(
            "{}:{}",
            meta.len(),
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ),
        Err(_) => "missing".to_string(),
    }
}

// ------------------------------------------------------------------ refresh

/// Rebuild boundary links from members' persisted state. Cost is
/// proportional to unresolved-externals, never corpus size.
pub fn refresh(ws: &Workspace) -> Result<usize> {
    let mut stores = BTreeMap::new();
    for (name, repo) in &ws.members {
        if !pipeline::db_path(repo).exists() {
            bail!(
                "member `{name}` has no graph — run `sinter workspace {}` or `sinter build {}`",
                ws.manifest.workspace.name,
                repo.display()
            );
        }
        stores.insert(name.clone(), Store::open(pipeline::db_path(repo))?);
    }

    // Foreign symbol pool per member = all OTHER members' nodes, with the
    // owning member recorded so bindings can be attributed.
    let mut nodes_by_member: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for (name, store) in &stores {
        nodes_by_member.insert(name.clone(), store.all_nodes()?);
    }
    let mut owner_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, nodes) in &nodes_by_member {
        for node in nodes {
            owner_of.insert(node.id.as_str(), name.as_str());
        }
    }

    let mut links: Vec<Link> = Vec::new();
    for (name, store) in &stores {
        let refs = store.all_unresolved()?;
        if refs.is_empty() {
            continue;
        }
        let imports = store.all_imports()?;
        let foreign: Vec<Node> = nodes_by_member
            .iter()
            .filter(|(other, _)| *other != name)
            .flat_map(|(_, nodes)| nodes.iter().cloned())
            .collect();
        for binding in sinter_resolve::resolve_boundary(&foreign, &refs, &imports) {
            let Some(dst_member) = owner_of.get(binding.edge.dst.as_str()) else {
                continue;
            };
            links.push(Link {
                src_member: name.clone(),
                src_id: binding.edge.src.as_str().to_string(),
                dst_member: dst_member.to_string(),
                dst_id: binding.edge.dst.as_str().to_string(),
                relation: binding.edge.relation,
                evidence: binding.edge.evidence,
                confidence: binding.edge.confidence,
                via: String::new(),
            });
        }
    }

    // Declared links: manifest-asserted runtime coupling. Symbols resolve
    // by unique name within their member; ambiguity is an error, not a
    // guess — the manifest must qualify.
    for declared in &ws.manifest.links {
        let src = declared_symbol(&stores, &declared.from_member, &declared.from_symbol)?;
        let dst = declared_symbol(&stores, &declared.to_member, &declared.to_symbol)?;
        links.push(Link {
            src_member: declared.from_member.clone(),
            src_id: src.id.as_str().to_string(),
            dst_member: declared.to_member.clone(),
            dst_id: dst.id.as_str().to_string(),
            relation: Relation::Uses,
            evidence: Evidence::Declared,
            confidence: Evidence::Declared.confidence(),
            via: declared.via.clone(),
        });
    }

    links.sort_by(|a, b| {
        (&a.src_member, &a.src_id, &b.dst_member).cmp(&(&b.src_member, &b.src_id, &a.dst_member))
    });

    let mut fps = BTreeMap::new();
    for (name, repo) in &ws.members {
        fps.insert(name.clone(), member_fingerprint(repo));
    }
    let store = LinkStore::open(ws)?;
    store.replace(&links, &fps)?;
    Ok(links.len())
}

fn declared_symbol(stores: &BTreeMap<String, Store>, member: &str, symbol: &str) -> Result<Node> {
    let store = stores
        .get(member)
        .with_context(|| format!("declared link names unknown member `{member}`"))?;
    let matches = store.nodes_named(symbol.rsplit("::").next().unwrap_or(symbol))?;
    let filtered: Vec<Node> = matches
        .into_iter()
        .filter(|n| {
            let q = sinter_resolve::qualified_of(n.id.as_str());
            q == symbol || q.ends_with(&format!("::{symbol}"))
        })
        .collect();
    match filtered.as_slice() {
        [node] => Ok(node.clone()),
        [] => bail!("declared link symbol `{symbol}` not found in member `{member}`"),
        _ => {
            bail!("declared link symbol `{symbol}` is ambiguous in member `{member}` — qualify it")
        }
    }
}

/// Members whose stores changed since the last link refresh.
pub fn stale_members(ws: &Workspace) -> Result<Vec<String>> {
    let store = LinkStore::open(ws)?;
    let recorded = store.fingerprints()?;
    let mut stale = Vec::new();
    for (name, repo) in &ws.members {
        if recorded.get(name) != Some(&member_fingerprint(repo)) {
            stale.push(name.clone());
        }
    }
    Ok(stale)
}

// ---------------------------------------------------------------- traversal

/// Cross-workspace reverse blast radius: BFS over member in-edges plus
/// boundary in-links, nodes identified as (member, id).
pub struct WsReached {
    pub member: String,
    pub node: Node,
    pub relation: Relation,
    pub evidence: Evidence,
    /// (member, node id) this dependent was reached from — the tree
    /// parent for rendering; BFS order alone misattributes children.
    pub parent: (String, String),
}

pub fn dependents(
    ws: &Workspace,
    start_member: &str,
    start: &NodeId,
    filter: &sinter_store::EdgeFilter,
    max_depth: usize,
) -> Result<Vec<WsReached>> {
    let links = LinkStore::open(ws)?;
    let mut stores = BTreeMap::new();
    for (name, repo) in &ws.members {
        stores.insert(name.clone(), Store::open(pipeline::db_path(repo))?);
    }
    let mut seen: HashSet<(String, String)> =
        HashSet::from([(start_member.to_string(), start.as_str().to_string())]);
    let mut queue: VecDeque<(String, String, usize)> =
        VecDeque::from([(start_member.to_string(), start.as_str().to_string(), 0)]);
    let mut out = Vec::new();
    while let Some((member, id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let store = &stores[&member];
        let node_id = NodeId::new(id.clone());
        for edge in store.in_edges(&node_id)? {
            if !filter.admits(&edge) {
                continue;
            }
            let key = (member.clone(), edge.src.as_str().to_string());
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(node) = store.node(&edge.src)? {
                out.push(WsReached {
                    member: member.clone(),
                    node,
                    relation: edge.relation,
                    evidence: edge.evidence,
                    parent: (member.clone(), id.clone()),
                });
                queue.push_back((key.0, key.1, depth + 1));
            }
        }
        for link in links.in_links(&member, &id)? {
            let admit = filter
                .evidence
                .as_ref()
                .is_none_or(|allowed| allowed.contains(&link.evidence))
                && filter
                    .relations
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(&link.relation));
            if !admit {
                continue;
            }
            let key = (link.src_member.clone(), link.src_id.clone());
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(node) = stores[&link.src_member].node(&NodeId::new(link.src_id.clone()))? {
                out.push(WsReached {
                    member: link.src_member.clone(),
                    node,
                    relation: link.relation,
                    evidence: link.evidence,
                    parent: (member.clone(), id.clone()),
                });
                queue.push_back((key.0, key.1, depth + 1));
            }
        }
    }
    Ok(out)
}

/// One step of a cross-workspace path: source (member, node id), the edge's
/// relation and evidence, destination (member, node id).
pub type PathStep = (String, String, Relation, Evidence, String, String);

/// Shortest cross-workspace path over out-edges + out-links.
pub fn shortest_path(
    ws: &Workspace,
    from: (&str, &NodeId),
    to: (&str, &NodeId),
    filter: &sinter_store::EdgeFilter,
) -> Result<Option<Vec<PathStep>>> {
    let links = LinkStore::open(ws)?;
    let mut stores = BTreeMap::new();
    for (name, repo) in &ws.members {
        stores.insert(name.clone(), Store::open(pipeline::db_path(repo))?);
    }
    type Key = (String, String);
    let start: Key = (from.0.to_string(), from.1.as_str().to_string());
    let goal: Key = (to.0.to_string(), to.1.as_str().to_string());
    let mut prev: BTreeMap<Key, (Key, Relation, Evidence)> = BTreeMap::new();
    let mut seen: HashSet<Key> = HashSet::from([start.clone()]);
    let mut queue: VecDeque<Key> = VecDeque::from([start.clone()]);
    while let Some(current) = queue.pop_front() {
        if current == goal {
            let mut path = Vec::new();
            let mut at = goal.clone();
            while at != start {
                let (parent, rel, evid) = prev[&at].clone();
                path.push((
                    parent.0.clone(),
                    parent.1.clone(),
                    rel,
                    evid,
                    at.0.clone(),
                    at.1.clone(),
                ));
                at = parent;
            }
            path.reverse();
            return Ok(Some(path));
        }
        let (member, id) = &current;
        let store = &stores[member];
        let mut nexts: Vec<(Key, Relation, Evidence)> = Vec::new();
        for edge in store.out_edges(&NodeId::new(id.clone()))? {
            if filter.admits(&edge) {
                nexts.push((
                    (member.clone(), edge.dst.as_str().to_string()),
                    edge.relation,
                    edge.evidence,
                ));
            }
        }
        for link in links.out_links(member, id)? {
            let admit = filter
                .evidence
                .as_ref()
                .is_none_or(|allowed| allowed.contains(&link.evidence))
                && filter
                    .relations
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(&link.relation));
            if admit {
                nexts.push((
                    (link.dst_member.clone(), link.dst_id.clone()),
                    link.relation,
                    link.evidence,
                ));
            }
        }
        for (next, rel, evid) in nexts {
            if seen.insert(next.clone()) {
                prev.insert(next.clone(), (current.clone(), rel, evid));
                queue.push_back(next);
            }
        }
    }
    Ok(None)
}

// -------------------------------------------------------------------- verb

/// `sinter workspace <manifest>`: build every member incrementally, then
/// refresh boundary links, then summarize.
pub fn run(manifest_path: &Path) -> Result<()> {
    let ws = load(manifest_path)?;
    println!(
        "workspace `{}` ({} members)",
        ws.manifest.workspace.name,
        ws.members.len()
    );
    for (name, repo) in &ws.members {
        let report = pipeline::build(repo, None)?;
        println!(
            "  {name}: {} scanned, {} changed, {} nodes",
            report.scanned, report.changed, report.total_nodes
        );
    }
    let count = refresh(&ws)?;
    println!("boundary links: {count}");
    Ok(())
}

// ------------------------------------------------------------ symbol lookup

/// Resolve a symbol argument across members. `member:symbol` addresses one
/// member explicitly; a bare symbol must be unique across the workspace.
pub fn find_symbol(ws: &Workspace, symbol: &str) -> Result<(String, Node)> {
    if let Some((member, rest)) = symbol.split_once(':')
        && ws.members.contains_key(member)
    {
        let store = Store::open(pipeline::db_path(&ws.members[member]))?;
        let node = crate::lookup::unique_symbol(&store, rest)?;
        return Ok((member.to_string(), node));
    }
    let mut matches: Vec<(String, Node)> = Vec::new();
    for (name, repo) in &ws.members {
        let store = Store::open(pipeline::db_path(repo))?;
        if let Ok(node) = crate::lookup::unique_symbol(&store, symbol) {
            matches.push((name.clone(), node));
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!("no member resolves `{symbol}` uniquely — try `member:symbol`"),
        _ => {
            let list: Vec<String> = matches
                .iter()
                .map(|(m, n)| format!("  {m}:{}", sinter_resolve::qualified_of(n.id.as_str())))
                .collect();
            bail!(
                "`{symbol}` matches in multiple members:\n{}",
                list.join("\n")
            )
        }
    }
}
