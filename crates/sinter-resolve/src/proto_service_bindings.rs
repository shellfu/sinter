//! Convention-based bindings between a `.proto` `service S { rpc R }` and
//! the Rust code tonic/prost generate for it. The generated trait and
//! client live in OUT_DIR, never in the repo, so the useful target is the
//! hand-written `impl S for T { fn r }`. Evidence is `declared`: the
//! convention is the contract, nothing in the corpus writes the link.

use std::collections::HashMap;

use sinter_core::{Edge, Evidence, Node, Relation, SymbolKind, TraitImpl};

/// `(service, snake(rpc))` for a proto rpc node, None for anything else.
fn rpc_key(n: &Node) -> Option<(&str, String)> {
    if n.kind != SymbolKind::Method || !n.file.ends_with(".proto") {
        return None;
    }
    let (service, rpc) = n.id.qualified().split_once("::")?;
    Some((service, snake_case(rpc)))
}

/// Proto rpcs keyed for client-side call binding: `client.adjudicates(req)`
/// where `client: HarnessServiceClient<_>` is generated code in OUT_DIR,
/// so the call can only bind to the rpc declaration itself.
#[derive(Default)]
pub struct ProtoRpcs<'a> {
    by_key: HashMap<(&'a str, String), &'a Node>,
    /// snake(rpc) -> every service declaring it; corpus-wide uniqueness
    /// is the fallback evidence when the receiver type is unknown.
    by_method: HashMap<String, Vec<&'a Node>>,
}

impl<'a> ProtoRpcs<'a> {
    pub fn build(nodes: &'a [Node]) -> Self {
        let mut out = Self::default();
        for n in nodes {
            if let Some((service, method)) = rpc_key(n) {
                out.by_method.entry(method.clone()).or_default().push(n);
                out.by_key.insert((service, method), n);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// The rpc an unresolved `<recv>.<method>(..)` call targets. With a
    /// declared receiver type `SClient` / `s_client::SClient<..>`, the
    /// service is known. Without one, the method must name exactly one
    /// rpc corpus-wide and the file must import a `*_client` module or a
    /// `*Client` name (`file_tokens`: import segments and bindings).
    pub fn client_call<'t>(
        &self,
        method: &str,
        receiver_type: Option<&str>,
        file_tokens: impl IntoIterator<Item = &'t str>,
    ) -> Option<&'a Node> {
        if let Some(ty) = receiver_type {
            let leaf = ty.split('<').next().unwrap_or(ty).trim();
            let leaf = leaf.rsplit("::").next().unwrap_or(leaf);
            let service = leaf.strip_suffix("Client")?;
            return self.by_key.get(&(service, method.to_string())).copied();
        }
        let [rpc] = self.by_method.get(method)?.as_slice() else {
            return None;
        };
        let service = rpc.id.qualified().split_once("::")?.0;
        let module = format!("{}_client", snake_case(service));
        let client = format!("{service}Client");
        file_tokens
            .into_iter()
            .any(|t| t == module || t == client)
            .then_some(rpc)
    }
}

/// Edges for every proto rpc whose tonic counterpart exists in the corpus:
/// rpc `calls` the impl method, impl method `implements` the rpc; a
/// generated `SClient::r` / `SServer::r` method `calls` the rpc; a trait
/// named `S` under a `s_server`/`s_client` module `implements` service `S`.
pub fn proto_service_edges(nodes: &[Node], trait_impls: &[TraitImpl]) -> Vec<Edge> {
    // (service, snake(rpc)) -> rpc node; service -> service node.
    let mut rpcs: HashMap<(&str, String), &Node> = HashMap::new();
    let mut services: HashMap<&str, &Node> = HashMap::new();
    for n in nodes.iter().filter(|n| n.file.ends_with(".proto")) {
        if let Some(key) = rpc_key(n) {
            rpcs.insert(key, n);
        } else if n.kind == SymbolKind::Interface && !n.id.qualified().contains("::") {
            services.insert(n.id.qualified(), n);
        }
    }
    if rpcs.is_empty() {
        return Vec::new();
    }
    let declared = |src: &Node, dst: &Node, relation| Edge {
        src: src.id.clone(),
        dst: dst.id.clone(),
        relation,
        evidence: Evidence::Declared,
        confidence: Evidence::Declared.confidence(),
        site: None,
        extra_sites: Vec::new(),
        sites_total: 0,
    };
    let mut edges = Vec::new();
    for n in nodes.iter().filter(|n| n.file.ends_with(".rs")) {
        let q = n.id.qualified();
        let (owner, leaf) = q.rsplit_once("::").unwrap_or(("", q));
        match n.kind {
            SymbolKind::Function | SymbolKind::Method => {
                // Hand-written `impl S for T { fn r }`.
                let impl_trait = trait_impls.iter().find(|ti| {
                    ti.file == n.file && ti.span.start <= n.span.start && n.span.end <= ti.span.end
                });
                if let Some(rpc) =
                    impl_trait.and_then(|ti| rpcs.get(&(ti.trait_name.as_str(), leaf.to_string())))
                {
                    edges.push(declared(rpc, n, Relation::Calls));
                    edges.push(declared(n, rpc, Relation::Implements));
                }
                // Generated `SClient::r` / `SServer::r` checked into the tree.
                let owner_leaf = owner.rsplit("::").next().unwrap_or(owner);
                if let Some(service) = owner_leaf
                    .strip_suffix("Client")
                    .or_else(|| owner_leaf.strip_suffix("Server"))
                    && let Some(rpc) = rpcs.get(&(service, leaf.to_string()))
                {
                    edges.push(declared(n, rpc, Relation::Calls));
                }
            }
            SymbolKind::Trait => {
                let module = snake_case(leaf);
                let generated = [format!("{module}_server"), format!("{module}_client")];
                let in_generated_module = generated
                    .iter()
                    .any(|m| owner.split("::").any(|seg| seg == m) || n.file.contains(m));
                if in_generated_module && let Some(service) = services.get(leaf) {
                    edges.push(declared(n, service, Relation::Implements));
                }
            }
            _ => {}
        }
    }
    edges
}

/// tonic/prost naming: `GetUserByID` -> `get_user_by_id`.
fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars.get(i + 1).is_some_and(|n| n.is_lowercase()))
        {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ProtoRpcs, snake_case};
    use sinter_core::{Node, NodeId, Span, SymbolKind};

    fn rpc(q: &str) -> Node {
        Node {
            id: NodeId::new(format!("proto/h.proto#{q}")),
            name: q.rsplit("::").next().unwrap().to_string(),
            kind: SymbolKind::Method,
            file: "proto/h.proto".into(),
            span: Span { start: 0, end: 1 },
            signature: String::new(),
            doc: None,
        }
    }

    #[test]
    fn client_call_binds_by_receiver_type_or_unique_import_evidence() {
        let nodes = vec![
            rpc("HarnessService::Adjudicates"),
            rpc("Other::Ping"),
            rpc("Third::Ping"),
        ];
        let rpcs = ProtoRpcs::build(&nodes);
        let hit = |m: &str, ty: Option<&str>, tokens: &[&str]| {
            rpcs.client_call(m, ty, tokens.iter().copied())
                .map(|n| n.id.qualified().to_string())
        };
        assert_eq!(
            hit("adjudicates", Some("HarnessServiceClient<Channel>"), &[]).as_deref(),
            Some("HarnessService::Adjudicates")
        );
        assert_eq!(hit("adjudicates", Some("OtherClient"), &[]), None);
        assert_eq!(
            hit("adjudicates", None, &["harness_service_client"]).as_deref(),
            Some("HarnessService::Adjudicates")
        );
        assert_eq!(hit("adjudicates", None, &["tonic"]), None);
        assert_eq!(hit("ping", None, &["other_client"]), None); // ambiguous
    }

    #[test]
    fn snake_case_follows_tonic() {
        assert_eq!(snake_case("Adjudicates"), "adjudicates");
        assert_eq!(snake_case("GetUserByID"), "get_user_by_id");
        assert_eq!(snake_case("HTTPServer"), "http_server");
        assert_eq!(snake_case("ListV2"), "list_v2");
    }
}
