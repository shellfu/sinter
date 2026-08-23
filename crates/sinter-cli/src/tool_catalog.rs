//! MCP tool declarations for repository and workspace server scopes.
//!
//! This is the discoverable agent surface. Runtime validation and output
//! envelopes remain owned by `agent_protocol`; execution remains in the
//! corresponding repository/workspace tool module.

use serde_json::{Value, json};

/// URI of the one long-form guidance resource; tool descriptions stay terse
/// and point here once via the server `instructions`.
pub(crate) const GUIDE_URI: &str = "sinter://guide";

/// Long-form guidance served through `resources/read`. Everything that used
/// to be repeated per tool description lives here once.
pub(crate) const GUIDE: &str = "\
# sinter MCP guide

## Reading results
Every tools/call result carries `structuredContent` = {protocol, operation, outcome, data};
`content[0].text` is a one-line summary only. `data` is byte-identical to the CLI `--json`
payload. Check `outcome.status` before acting: `complete`, `partial` (coverage gaps, see
`data.coverage`), `not_found`, or `not_proven` (graph is blind here; never a negative proof).
Errors arrive as JSON-RPC `error.data` with `error.code` (no_match, ambiguous_symbol,
stale_snapshot [retryable], invalid_arguments, ...) and `error.candidates`.

## Symbol addressing
`symbol` accepts a stable `symbol_key` (preferred, from any prior result), a bare name, a
qualified suffix (`Store::in_edges`), `name@file-suffix`, or a snapshot-local node id.
Workspace scope adds `member:Symbol`. Ambiguous names fail with candidates; pick one.

## Terse row keys (affected/deps dependents)
s=qualified symbol  k=kind  f=file  e=relation/evidence  c=certain|possible
d=depth (repo)  p=parent (workspace)  site=file:line of the referencing site.
List-bearing responses carry a `legend` field on the first page and when truncated.
Pass `detail:true` for full node objects.

## Coverage semantics
`coverage.status` found|not_proven; `coverage.completeness` complete|partial;
`coverage.conclusive` is always false: a static graph is never runtime-exhaustive.
`coverage.compiler_index` is {state: fresh|stale|missing, stale_inputs, missing_index_for:
[languages]}; run `sinter scip` to refresh. Full per-project detail: CLI `--json` or `doctor`.
`unresolved` lists references the graph could not bind; check it before reading an empty
affected/deps/path result as absence.

## Budget and paging
`limit` caps list entries per call (default 50; `impact` 20 per collection, 0 = all).
Results are bounded to `budget_bytes` (default 8000, 0 = unlimited): text fields are
capped first, then diagnostics collapsed, then trailing list entries dropped with
`truncated`, `totals`, and `next_cursor` set. Resume with `cursor: next_cursor`.

## Batching
Repository `affected` accepts `symbols: [...]` and returns `{results: [...]}` with
per-symbol errors inline. `impact` takes a git rev range (`HEAD`, `HEAD~1..HEAD`,
`main...branch`); `overlap` takes two or more, optionally `label=range`.

## Filters
`evidence` (structural, scope, import, scip, declared, dynamic), `min_confidence`
(`certain` = compiler-grade edges only), `relations` (calls, uses, imports, implements,
extends; `calls,uses` drops file-level import noise), `scope` (corpus roles: production, test,
fixture, example, generated, vendor, docs; `all` must be used alone).
`if_snapshot`: pass a prior `snapshot` token to fail instead of answering from a changed graph.
";

fn traversal_filters() -> Value {
    json!({
        "evidence": {"type": "array", "items": {"type": "string"}},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"]},
        "relations": {"type": "array", "items": {"type": "string",
            "enum": ["calls", "uses", "imports", "implements", "extends"]}},
    })
}

fn snapshot_precondition() -> Value {
    json!({"type": "string"})
}

fn symbol() -> Value {
    json!({"type": "string", "description": "symbol_key, name, or name@file-suffix"})
}

fn scope_filter(default: &[&str]) -> Value {
    json!({
        "type": "array",
        "items": {"type": "string"},
        "default": default,
    })
}

fn limit() -> Value {
    json!({"type": "integer"})
}

pub(crate) fn repository() -> Value {
    let filters = traversal_filters();
    let mut list = json!({"tools": [
        {
            "name": "map",
            "description": "Repo inventory: totals, modules, dependency hubs, doc entry points. Start here in an unfamiliar repo. e.g. {}",
            "inputSchema": {"type": "object", "properties": {
                "scope": scope_filter(&["production", "docs"]),
            }},
        },
        {
            "name": "ask",
            "description": "Concept search: ranked hits per topic with status/advice (abstain = refine). e.g. {question:\"where is retry backoff\"}",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": limit(),
                "scope": scope_filter(&["production", "docs"]),
                "explain": {"type": "boolean", "default": false},
            }, "required": ["question"]},
        },
        {
            "name": "context",
            "description": "Evidence packet for a coding task: top symbols, deps, callers, tests, gaps, next commands. e.g. {task:\"add a retry to the fetcher\"}",
            "inputSchema": {"type": "object", "properties": {
                "task": {"type": "string"},
            }, "required": ["task"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, file, incoming/outgoing edges with site, capped per relation. e.g. {symbol:\"Store::in_edges\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": {"type": "integer", "description": "rows per relation (default 20)"},
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": "Find symbols by exact, qualified, or fuzzy name; returns signature, file, span. e.g. {symbol:\"in_edges\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": limit(),
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "What breaks if a symbol changes: transitive dependents, by file. Batch via symbols:[]. e.g. {symbol:\"Store::in_edges\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": {"type": "array", "items": {"type": "string"}},
                "max_depth": {"type": "integer"},
                "limit": limit(),
                "detail": {"type": "boolean"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "deps",
            "description": "What a symbol transitively calls/uses/imports, by file. e.g. {symbol:\"serve::run\",relations:[\"calls\"]}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": {"type": "integer"},
                "limit": limit(),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path between two symbols, with evidence per step; found:false is not absence. e.g. {from:\"main\",to:\"Store::open\"}",
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
            "description": "Where the graph is blind: unbound references with file:line and reason. Check before trusting an empty result. e.g. {name:\"open\"}",
            "inputSchema": {"type": "object", "properties": {
                "file": {"type": "string"},
                "name": {"type": "string"},
                "limit": limit(),
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, affected tests for a git range; HEAD = working tree. e.g. {rev_range:\"HEAD~1..HEAD\"}",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": {"type": "string"},
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "per collection; 0 = all"},
            }, "required": ["rev_range"]},
        },
        {
            "name": "overlap",
            "description": "Pairwise merge risk between in-flight changes (direct/radius/file tiers). e.g. {ranges:[\"pr-1=main...a\",\"pr-2=main...b\"]}",
            "inputSchema": {"type": "object", "properties": {
                "ranges": {"type": "array", "items": {"type": "string"}, "minItems": 2},
            }, "required": ["ranges"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    list
}

pub(crate) fn workspace() -> Value {
    let filters = traversal_filters();
    let mut list = json!({"tools": [
        {
            "name": "ask",
            "description": "Concept search across all members: ranked hits per topic with status/advice. e.g. {question:\"retry backoff\"}",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": limit(),
                "scope": scope_filter(&["production", "docs"]),
                "explain": {"type": "boolean", "default": false},
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, edges in its member, boundary links to other members. e.g. {symbol:\"common:Backoff\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": {"type": "integer", "description": "rows per relation (default 20)"},
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": "Resolve a symbol across every member. e.g. {symbol:\"common:Backoff\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "What breaks across all members if a symbol changes: transitive dependents, by file. e.g. {symbol:\"common:Backoff\"}",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": {"type": "integer"},
                "limit": limit(),
                "detail": {"type": "boolean"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&["all"]),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path between two symbols across member boundaries. e.g. {from:\"auth:login\",to:\"common:Backoff\"}",
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
            "description": "Where the graph is blind across members: unbound references tagged member:path. e.g. {member:\"auth\"}",
            "inputSchema": {"type": "object", "properties": {
                "member": {"type": "string"},
                "file": {"type": "string"},
                "name": {"type": "string"},
                "limit": limit(),
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, affected tests for a git range in one member, continued across members. e.g. {member:\"common\",rev_range:\"HEAD\"}",
            "inputSchema": {"type": "object", "properties": {
                "member": {"type": "string"},
                "rev_range": {"type": "string"},
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "per collection; 0 = all"},
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
    fn catalog_stays_within_the_context_tax_budget() {
        for catalog in [repository(), workspace()] {
            assert!(serde_json::to_string(&catalog).unwrap().len() <= 9000);
            for tool in catalog["tools"].as_array().unwrap() {
                let description = tool["description"].as_str().unwrap();
                assert!(description.len() <= 160, "{}: {description}", tool["name"]);
                assert!(description.contains("e.g. {"), "{}", tool["name"]);
                for (key, prop) in tool["inputSchema"]["properties"].as_object().unwrap() {
                    if let Some(d) = prop["description"].as_str() {
                        assert!(d.len() <= 40, "{}.{key}: {d}", tool["name"]);
                    }
                }
            }
        }
        assert!(super::GUIDE.contains("s=qualified symbol"));
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
        for catalog in [&repository, &workspace] {
            let ask = catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "ask")
                .unwrap();
            assert_eq!(
                ask["inputSchema"]["properties"]["explain"]["type"],
                "boolean"
            );
            assert_eq!(
                ask["inputSchema"]["properties"]["explain"]["default"],
                false
            );
            let impact = catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "impact")
                .unwrap();
            assert_eq!(impact["inputSchema"]["properties"]["limit"]["minimum"], 0);
            assert_eq!(impact["inputSchema"]["properties"]["limit"]["default"], 20);
        }
    }
}
