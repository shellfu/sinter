//! MCP tool declarations for repository and workspace server scopes.
//!
//! This is the discoverable agent surface. Runtime validation and output
//! envelopes remain owned by `agent_protocol`; execution remains in the
//! corresponding repository/workspace tool module.

use serde_json::{Value, json};

fn traversal_filters() -> Value {
    json!({
        "evidence": {"type": "array", "items": {"type": "string",
            "enum": ["structural", "scope", "import", "scip", "declared", "dynamic"]},
            "description": "restrict to these evidence kinds"},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only"},
        "relations": {"type": "array", "items": {"type": "string",
            "enum": ["calls", "uses", "imports", "implements", "extends"]},
            "description": "follow only these relations (e.g. drop file-level imports)"},
    })
}

fn snapshot_precondition() -> Value {
    json!({
        "type": "string",
        "description": "optional graph snapshot token from a prior response; stale tokens fail instead of resolving against changed graph state"
    })
}

fn scope_filter(default: &[&str]) -> Value {
    json!({
        "type": "array",
        "items": {"type": "string", "enum": [
            "production", "test", "fixture", "example", "generated", "vendor", "docs", "all"
        ]},
        "default": default,
        "description": "corpus roles to return or traverse; `all` must be used alone"
    })
}

pub(crate) fn repository() -> Value {
    let filters = traversal_filters();
    let mut list = json!({"tools": [
        {
            "name": "map",
            "description": "One-screen orientation card for the repository: node/edge totals, the module tree with per-directory symbol counts, the most depended-on hub symbols, and doc entry points. Call this first in an unfamiliar repo.",
            "inputSchema": {"type": "object", "properties": {
                "scope": scope_filter(&["production", "docs"]),
            }},
        },
        {
            "name": "ask",
            "description": "Answer a vague or conceptual codebase question with explicit per-topic ranked hits and agent-safety metadata. `ranking_margin` is only a score gap; `confidence.calibration` reports the named holdout sample and measured precision. Obey each topic's `status`, `verify_required`, and `advice`: abstain means refine the query, verify means inspect evidence before acting. `limit` is a strict global hit budget across topics.",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": {"type": "integer"},
                "scope": scope_filter(&["production", "docs"]),
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": "Orient on one symbol: signature, doc, file, plus every incoming and outgoing edge with relation, evidence, and call site (`site`: file:line of the reference).",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": "Find symbols by exact name, qualified name, or fuzzy match. Results carry signature, doc comment, file, and byte span.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "limit": {"type": "integer"},
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Reverse blast radius: everything transitively depending on a symbol, cross-file. Summary-first: total, by_file (top files by dependent count), then dependents capped at `limit` (default 50; `truncated` reports how many were omitted). Terse dependent keys: s=qualified symbol, k=kind, f=file, e=relation/evidence, c=certain/possible, d=depth, site=file:line of the referencing site when known. Pass detail:true for full nodes within the limit. Pass `symbols` (array) to batch many symbols in one call — response is {results:[...]}, per-symbol errors inline. Every result carries snapshot plus coverage completeness, active filters, evidence availability, certain/possible counts, and unresolved gaps. Even a non-empty result is not runtime-exhaustive.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "symbols": {"type": "array", "items": {"type": "string"},
                    "description": "batch: blast radius for each; overrides `symbol`"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer", "description": "max dependents returned (default 50)"},
                "detail": {"type": "boolean", "description": "full node objects instead of terse entries"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "deps",
            "description": "Forward blast radius: everything a symbol transitively depends on (calls, uses, imports), cross-file. Summary-first: total, by_file, then dependencies capped at `limit` (default 50; `truncated` reports how many were omitted). Terse keys: s=qualified symbol, k=kind, f=file, e=relation/evidence, c=certain/possible, d=depth, site=file:line of the referencing site when known. Every result carries snapshot plus coverage completeness, active filters, evidence availability, certain/possible counts, and unresolved gaps. Even a non-empty result is not runtime-exhaustive.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer", "description": "max dependencies returned (default 50)"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path from one symbol to another, with relation, evidence, confidence (certain/possible), and call site (`site`: file:line) for every step. Hits and misses both carry snapshot plus coverage completeness, active filters, evidence availability, and unresolved gaps. A static path is not proof of runtime reachability; `found:false` is never absence proof.",
            "inputSchema": {"type": "object", "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["from", "to"]},
        },
        {
            "name": "unresolved",
            "description": "Where the graph is blind: references extraction saw but resolution never bound, each with file:line, enclosing symbol, and reason. Check this before treating an empty affected/deps/path result as a negative proof. Filter by `file` (repo-relative path) and/or `name` (referenced identifier).",
            "inputSchema": {"type": "object", "properties": {
                "file": {"type": "string"},
                "name": {"type": "string"},
                "limit": {"type": "integer", "description": "max references returned (default 50)"},
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, and affected tests for a git rev range (e.g. HEAD~1..HEAD, main...branch). A single rev (`HEAD`) covers uncommitted edits to tracked files in the working tree; untracked files are not included.",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": {"type": "string"},
            }, "required": ["rev_range"]},
        },
        {
            "name": "overlap",
            "description": "Rank pairwise merge risk between several in-flight changes (git rev ranges, e.g. open PRs). Tiers: direct = both touch the same symbol (textual or semantic collision); radius = one touches a symbol the other's touched code depends on (merges clean, breaks semantically); file = same file, disjoint symbols. Ranges accept `label=range` (e.g. pr-12=main...branch).",
            "inputSchema": {"type": "object", "properties": {
                "ranges": {"type": "array", "items": {"type": "string"}, "minItems": 2,
                    "description": "two or more rev-ranges, optionally labeled `label=range`"},
            }, "required": ["ranges"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    list
}

pub(crate) fn workspace() -> Value {
    let filters = traversal_filters();
    let addressing = "Symbols accept `member:Symbol` (member from the workspace manifest) or any bare name that resolves uniquely across members.";
    let mut list = json!({"tools": [
        {
            "name": "ask",
            "description": "Answer a vague or conceptual question across every workspace member with the same calibrated per-topic contract as repository ask. Hits are merge-ranked and tagged with member before confidence is assessed. Obey each topic's `status`, `verify_required`, and `advice`; `limit` is a strict global hit budget.",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": {"type": "integer"},
                "scope": scope_filter(&["production", "docs"]),
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": format!("Orient on one symbol: signature, doc, file, every incoming and outgoing edge inside its member (with relation, evidence, and call site), plus boundary links into and out of the other members. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "member-qualified stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": format!("Resolve a symbol across every workspace member. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "member-qualified stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": format!("Cross-repository blast radius: everything transitively depending on a symbol across all workspace members, boundary links included. Summary-first: total, by_file, then dependents capped at `limit` (default 50; `truncated` reports omissions). Terse dependent keys: s=member:qualified symbol, k=kind, f=file, e=relation/evidence, c=certain/possible, p=parent. Pass detail:true for full nodes within the limit. Every result carries a workspace snapshot and member-attributed coverage gaps; unresolved_refs_matching_name > 0 means the list may be incomplete. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string", "description": "member-qualified stable symbol_key (preferred), name, qualified suffix, name@file-suffix, or snapshot-local node id"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer", "description": "max dependents returned (default 50)"},
                "detail": {"type": "boolean", "description": "full node objects instead of terse entries"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": format!("Shortest dependency path between two symbols, crossing repository boundaries through import and declared links. Every step states certain/possible confidence, and hits and misses both carry a workspace snapshot plus member-attributed coverage gaps. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["from", "to"]},
        },
        {
            "name": "unresolved",
            "description": "Where the graph is blind across the workspace: references extraction saw but resolution never bound, each tagged with its member (`member`, file as `member:path`). Check before treating an empty affected/deps/path result as a negative proof. Filter by `member`, `file` (member-relative path), and/or `name`.",
            "inputSchema": {"type": "object", "properties": {
                "member": {"type": "string"},
                "file": {"type": "string"},
                "name": {"type": "string"},
                "limit": {"type": "integer", "description": "max references returned (default 50)"},
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, and affected tests for a git rev range (e.g. HEAD~1..HEAD) in one member, with the radius continued across boundary links into the other members (cross-member entries carry a `member:` file prefix).",
            "inputSchema": {"type": "object", "properties": {
                "member": {"type": "string", "description": "workspace member the rev range applies to"},
                "rev_range": {"type": "string"},
            }, "required": ["member", "rev_range"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    list
}

#[cfg(test)]
mod tests {
    use super::{repository, workspace};

    fn names(catalog: &serde_json::Value) -> Vec<&str> {
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn scope_catalogs_advertise_only_executable_tools() {
        let repository = repository();
        let workspace = workspace();
        let repository_names = names(&repository);
        let workspace_names = names(&workspace);
        assert!(repository_names.contains(&"deps"));
        assert!(repository_names.contains(&"map"));
        assert!(!workspace_names.contains(&"deps"));
        assert!(!workspace_names.contains(&"map"));
        for tool in repository["tools"]
            .as_array()
            .unwrap()
            .iter()
            .chain(workspace["tools"].as_array().unwrap())
        {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["outputSchema"].is_object());
        }
    }
}
