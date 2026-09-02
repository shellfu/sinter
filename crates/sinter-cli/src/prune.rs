//! What a traversal answer leaves out on purpose: test rows, file-level
//! import noise under a relations filter, and everything behind a hub.
//! A dropped row takes its subtree with it, so no orphan rows appear;
//! the counts of what left stay so the answer never reads as complete.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use sinter_core::{CorpusScope, Node, NodeId, SymbolKind};
use sinter_store::{EdgeFilter, Reached, Store};

/// A hub is a node more than this many rows were reached through.
pub const HUB_FAN_IN: usize = 100;

pub struct Rules<'a> {
    pub keep_tests: bool,
    /// Drop `k:"file"` rows: set when a relations filter is given, since
    /// file-level `uses`/`imports` rows are import noise there.
    pub drop_file_rows: bool,
    /// `None` never stops at a hub.
    pub hub_fan_in: Option<usize>,
    pub is_test: &'a dyn Fn(&Node) -> bool,
}

#[derive(Default)]
pub struct Pruned {
    pub rows: Vec<Reached>,
    /// Test rows hidden (or, with `keep_tests`, kept) — direct and
    /// transitive, before subtree removal.
    pub tests: usize,
    /// Hubs the answer stopped at: `(qualified name, fan-in)`.
    pub hubs: Vec<(String, usize)>,
}

/// `parent` is the node a row was reached from: `via.dst` for dependents,
/// `via.src` for dependencies. Rows must arrive parents-first (BFS order).
pub fn prune(reached: Vec<Reached>, parent: fn(&Reached) -> &NodeId, rules: &Rules) -> Pruned {
    let mut fan_in: HashMap<String, usize> = HashMap::new();
    for row in &reached {
        *fan_in.entry(parent(row).as_str().to_string()).or_default() += 1;
    }
    let is_hub = |id: &str| {
        rules
            .hub_fan_in
            .is_some_and(|max| fan_in.get(id) > Some(&max))
    };
    let mut cut: HashSet<String> = HashSet::new();
    let mut out = Pruned::default();
    for row in reached {
        if cut.contains(parent(&row).as_str()) {
            cut.insert(row.node.id.as_str().to_string());
            continue;
        }
        let test = (rules.is_test)(&row.node);
        out.tests += usize::from(test);
        // A file row is import noise only when its edge is an import or a
        // file-level `uses`; a file that *calls* the seed at top level (a TS
        // test file, a script) is a caller and stays.
        let noise_file_row = rules.drop_file_rows
            && row.node.kind == SymbolKind::File
            && matches!(
                row.via.relation,
                sinter_core::Relation::Imports | sinter_core::Relation::Uses
            );
        if (test && !rules.keep_tests) || noise_file_row {
            cut.insert(row.node.id.as_str().to_string());
            continue;
        }
        if is_hub(row.node.id.as_str()) {
            let id = row.node.id.as_str();
            out.hubs
                .push((sinter_resolve::qualified_of(id).to_string(), fan_in[id]));
            cut.insert(id.to_string());
        }
        out.rows.push(row);
    }
    // Production rows rank before test rows everywhere; stable, so BFS
    // order (parents first) survives within each class.
    out.rows.sort_by_key(|row| (rules.is_test)(&row.node));
    out
}

pub fn is_test_scope(scope: CorpusScope) -> bool {
    scope == CorpusScope::Test
}

/// Whether an empty answer is the filter's doing: depth 0 admits nothing,
/// and a restricted relation/evidence/confidence filter is blamed when the
/// same traversal without it finds a direct edge.
pub fn filter_excluded(
    store: &Store,
    id: &NodeId,
    filter: &EdgeFilter,
    max_depth: usize,
    forward: bool,
) -> Result<bool> {
    if max_depth == 0 {
        return Ok(true);
    }
    if filter.relations.is_none() && filter.evidence.is_none() && filter.min_confidence.is_none() {
        return Ok(false);
    }
    let open = EdgeFilter {
        scopes: filter.scopes.clone(),
        ..EdgeFilter::default()
    };
    let reached = if forward {
        store.dependencies(id, &open, 1)?
    } else {
        store.dependents(id, &open, 1)?
    };
    Ok(!reached.is_empty())
}

#[cfg(test)]
mod tests {
    use sinter_core::{Confidence, Edge, Evidence, Node, NodeId, Relation, Span, SymbolKind};
    use sinter_store::Reached;

    use super::{Pruned, Rules, prune};

    fn node(id: &str, kind: SymbolKind) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            name: id.rsplit('#').next().unwrap_or(id).to_string(),
            file: id.split('#').next().unwrap_or(id).to_string(),
            span: Span { start: 0, end: 1 },
            signature: String::new(),
            doc: None,
        }
    }

    fn dependent(id: &str, from: &str, depth: usize, kind: SymbolKind) -> Reached {
        Reached {
            node: node(id, kind),
            depth,
            via: Edge {
                src: NodeId::new(id),
                dst: NodeId::new(from),
                relation: Relation::Calls,
                evidence: Evidence::Scope,
                confidence: Confidence::Inferred,
                site: None,
                extra_sites: Vec::new(),
                sites_total: 0,
            },
        }
    }

    fn parent(r: &Reached) -> &NodeId {
        &r.via.dst
    }

    fn ids(pruned: &Pruned) -> Vec<&str> {
        pruned.rows.iter().map(|r| r.node.id.as_str()).collect()
    }

    #[test]
    fn a_hidden_test_row_takes_its_subtree_and_is_counted() {
        let rows = vec![
            dependent("a.rs#x", "seed", 1, SymbolKind::Function),
            dependent("tests/t.rs#test_x", "seed", 1, SymbolKind::Function),
            dependent(
                "tests/t.rs#helper",
                "tests/t.rs#test_x",
                2,
                SymbolKind::Function,
            ),
            dependent("a.rs#y", "a.rs#x", 2, SymbolKind::Function),
        ];
        let is_test = |n: &Node| n.file.starts_with("tests/");
        let pruned = prune(
            rows,
            parent,
            &Rules {
                keep_tests: false,
                drop_file_rows: false,
                hub_fan_in: None,
                is_test: &is_test,
            },
        );
        assert_eq!(ids(&pruned), vec!["a.rs#x", "a.rs#y"]);
        // Only the row itself is a test count; its subtree left with it.
        assert_eq!(pruned.tests, 1);
    }

    #[test]
    fn kept_test_rows_rank_after_production_rows() {
        let rows = vec![
            dependent("tests/t.rs#test_x", "seed", 1, SymbolKind::Function),
            dependent("a.rs#x", "seed", 1, SymbolKind::Function),
        ];
        let is_test = |n: &Node| n.file.starts_with("tests/");
        let pruned = prune(
            rows,
            parent,
            &Rules {
                keep_tests: true,
                drop_file_rows: false,
                hub_fan_in: None,
                is_test: &is_test,
            },
        );
        assert_eq!(ids(&pruned), vec!["a.rs#x", "tests/t.rs#test_x"]);
        assert_eq!(pruned.tests, 1);
    }

    #[test]
    fn file_rows_leave_under_a_relations_filter() {
        let mut import_row = dependent("a.rs", "seed", 1, SymbolKind::File);
        import_row.via.relation = Relation::Imports;
        let rows = vec![
            import_row,
            dependent("b.rs#imports_a", "a.rs", 2, SymbolKind::Function),
            dependent("a.rs#x", "seed", 1, SymbolKind::Function),
        ];
        let never = |_: &Node| false;
        let pruned = prune(
            rows,
            parent,
            &Rules {
                keep_tests: true,
                drop_file_rows: true,
                hub_fan_in: None,
                is_test: &never,
            },
        );
        assert_eq!(ids(&pruned), vec!["a.rs#x"]);
    }

    #[test]
    fn a_file_that_calls_the_seed_is_a_caller_not_noise() {
        let rows = vec![dependent("t.test.ts", "seed", 1, SymbolKind::File)];
        let never = |_: &Node| false;
        let pruned = prune(
            rows,
            parent,
            &Rules {
                keep_tests: true,
                drop_file_rows: true,
                hub_fan_in: None,
                is_test: &never,
            },
        );
        assert_eq!(ids(&pruned), vec!["t.test.ts"]);
    }

    #[test]
    fn the_answer_stops_at_a_hub_and_names_it() {
        let mut rows = vec![dependent("hub.rs#hub", "seed", 1, SymbolKind::Function)];
        for i in 0..3 {
            rows.push(dependent(
                &format!("c.rs#c{i}"),
                "hub.rs#hub",
                2,
                SymbolKind::Function,
            ));
        }
        rows.push(dependent("d.rs#d", "c.rs#c0", 3, SymbolKind::Function));
        let never = |_: &Node| false;
        let pruned = prune(
            rows,
            parent,
            &Rules {
                keep_tests: true,
                drop_file_rows: false,
                hub_fan_in: Some(2),
                is_test: &never,
            },
        );
        assert_eq!(ids(&pruned), vec!["hub.rs#hub"]);
        assert_eq!(pruned.hubs, vec![("hub".to_string(), 3)]);
    }
}
