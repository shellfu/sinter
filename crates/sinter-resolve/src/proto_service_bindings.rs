//! Convention-based bindings between a `.proto` `service S { rpc R }` and
//! the Rust code tonic/prost generate for it. The generated trait and
//! client live in OUT_DIR, never in the repo, so the useful target is the
//! hand-written `impl S for T { fn r }`. Evidence is `declared`: the
//! convention is the contract, nothing in the corpus writes the link.

use std::collections::HashMap;

use sinter_core::{Edge, Evidence, Node, Relation, SymbolKind, TraitImpl};

/// Edges for every proto rpc whose tonic counterpart exists in the corpus:
/// rpc `calls` the impl method, impl method `implements` the rpc; a
/// generated `SClient::r` / `SServer::r` method `calls` the rpc; a trait
/// named `S` under a `s_server`/`s_client` module `implements` service `S`.
pub fn proto_service_edges(nodes: &[Node], trait_impls: &[TraitImpl]) -> Vec<Edge> {
    // (service, snake(rpc)) -> rpc node; service -> service node.
    let mut rpcs: HashMap<(&str, String), &Node> = HashMap::new();
    let mut services: HashMap<&str, &Node> = HashMap::new();
    for n in nodes.iter().filter(|n| n.file.ends_with(".proto")) {
        let q = n.id.qualified();
        match (n.kind, q.split_once("::")) {
            (SymbolKind::Method, Some((service, rpc))) => {
                rpcs.insert((service, snake_case(rpc)), n);
            }
            (SymbolKind::Interface, None) => {
                services.insert(q, n);
            }
            _ => {}
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
    use super::snake_case;

    #[test]
    fn snake_case_follows_tonic() {
        assert_eq!(snake_case("Adjudicates"), "adjudicates");
        assert_eq!(snake_case("GetUserByID"), "get_user_by_id");
        assert_eq!(snake_case("HTTPServer"), "http_server");
        assert_eq!(snake_case("ListV2"), "list_v2");
    }
}
