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
`content[0].text` is a one-line summary only. `data` is the CLI `--json` payload minus the
MCP trim: the `symbol` echo keeps {symbol_key, qualified, kind, file, line}, a dependent's
`site` becomes `l` (line) when it is in the row's own file, and `coverage` is omitted unless
`include_coverage: true`. `outcome.status` is the one verdict to check before acting:
`complete`, `partial` (coverage gaps, a tie-break, an abstaining ranking, or a cut list),
`not_found`, or `not_proven` (graph is blind here; never a negative proof). `outcome.reason`
says why when known: filter_excluded (max_depth 0 or a filter removed every edge),
no_scip (no compiler index; run `sinter scip`), abstain, tie_break (see `warnings`),
limit_reached (page with `cursor`).

## Errors
A failure the caller can fix arrives as a tool result with `isError: true` and
`structuredContent` = {outcome: {status}, error: {code, message, candidates, retryable}}:
no_match, ambiguous_symbol, relocated_handle, stale_snapshot [retryable], invalid_arguments
(names the field and the expected type), execution_error. `candidates` are `Name@file[:line]`
selectors to paste back as `symbol`. Only protocol faults (unknown tool, arguments not an
object) are JSON-RPC errors. Batched entries carry the same `error` object plus `status`.

## Symbol addressing
`symbol` accepts a stable `symbol_key` (preferred, from any prior result), a bare name, a
qualified suffix (`Store::in_edges`), `name@file-suffix`, `name@file:line`, or a
snapshot-local node id. Workspace scope adds `member:Symbol`.

## Terse row keys (affected/deps dependents)
s=qualified symbol  k=kind  f=file  e=relation/evidence  c=certain|possible
d=depth (repo)  p=parent (workspace)  l=line of the referencing site in f
site=file:line when the site is in another file.
List-bearing responses carry a `legend` field on the first page and when truncated.
Pass `detail:true` for full node objects.

## Bounded text search, excerpts, refactor checks
`grep` is a regex over file *text* restricted to what a traversal reached, so a search never
leaves the graph: `within: [affected(SYM)]`, `[deps(SYM)]`, or `[file(PATH)]`, repeatable
and unioned; rows are f=file l=line t=text and `total` counts every match above `limit`.
`show` takes `body: true` (with `context_lines`) for a bounded source excerpt in `excerpt`.
`impact` takes `expect: [SYM]` and answers the unfinished-refactor question: per symbol, the
direct dependents this diff changed and the ones it still owes (`expect[].untouched`).
`context` returns `next_actions` as tool calls `{tool, args}` ready to send back.

## Universe and coverage
Every tool searches the CLI default corpus `production,test,docs` unless `scope` says
otherwise (`ask` searches `production,docs`); `scope: [\"all\"]` adds fixture, example,
generated and vendor code. With `include_coverage: true`:
`coverage.status` found|not_proven; `coverage.completeness` complete|partial;
`coverage.conclusive` is always false: a static graph is never runtime-exhaustive.
`coverage.universe` names the canonical repository root or every declared workspace member
searched; repositories absent from it were not searched. `coverage.filters` appears only
when a filter narrowed the traversal.
`coverage.compiler_index` is {state: fresh|stale|missing, stale_inputs, missing_index_for:
[languages]}; run `sinter scip` to refresh. Full per-project detail: CLI `--json` or `doctor`.
`unresolved` lists references the graph could not bind; check it before reading an empty
affected/deps/path result as absence.
Repository-wide coverage fields are sent once per session and then replaced by
`coverage.ref` (a fingerprint) plus `ref_note`; a changed fingerprint means they are sent
in full again. Read resource `sinter://coverage` for the referenced block.

## Budget and paging
`limit` caps list entries per call (defaults: ask 5, query 10, show 20 per relation,
impact 20 per collection, grep 100, others 50); `limit: 0` means unlimited on every tool.
Results are bounded to `budget_bytes` (default 8000, 0 = unlimited): text fields are
capped first, then diagnostics collapsed, then trailing list entries dropped. A budget too
small for the minimal answer returns that answer with `budget_truncated: true`.
`next_cursor` is present whenever rows remain beyond the page, whether `limit` or the byte
budget cut them; resume with `cursor: next_cursor` (the page is taken from the whole
result). `totals` names the full size; a `cursor` past the end is `invalid_arguments`.

## Batching
Repository `show`, `affected`, and `deps` accept `symbols: [...]`; `path` accepts
`pairs: [[from, to], ...]`. Each returns `{status, results: [...]}` with per-entry
`status` and inline errors. `impact` takes a git rev range (`HEAD`, `HEAD~1..HEAD`,
`main...branch`); `overlap` takes two or more, optionally `label=range`.

## Filters
`evidence` (structural, scope, import, scip, declared, dynamic), `min_confidence`
(`certain` = compiler-grade edges only), `relations` (calls, uses, imports, implements,
extends, reads, writes, creates, alters, drops; `calls,uses,reads,writes,creates,alters,drops`
drops file-level import noise), `scope` (corpus roles: production, test,
fixture, example, generated, vendor, docs; `all` must be used alone).
`if_snapshot`: pass a prior `snapshot` token to fail instead of answering from a changed graph.
";

const RELATIONS: [&str; 10] = [
    "calls",
    "uses",
    "imports",
    "implements",
    "extends",
    "reads",
    "writes",
    "creates",
    "alters",
    "drops",
];
const EVIDENCE: [&str; 6] = [
    "structural",
    "scope",
    "import",
    "scip",
    "declared",
    "dynamic",
];
const SCOPES: [&str; 8] = [
    "production",
    "test",
    "fixture",
    "example",
    "generated",
    "vendor",
    "docs",
    "all",
];

fn traversal_filters() -> Value {
    json!({
        "evidence": {
            "type": "array", "items": {"type": "string", "enum": EVIDENCE},
            "description": "edge evidence kinds to keep (default: all)",
        },
        "min_confidence": {
            "type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only",
        },
        "relations": {
            "type": "array", "items": {"type": "string", "enum": RELATIONS},
            "description": "edge relations to follow (default: all)",
        },
    })
}

fn snapshot_precondition() -> Value {
    json!({"type": "string", "description": "fail if the graph snapshot changed"})
}

fn symbol() -> Value {
    json!({"type": "string", "description": "symbol_key, name, or name@file-suffix"})
}

fn symbols() -> Value {
    json!({
        "type": "array", "items": {"type": "string"},
        "description": "batch: one result per symbol",
    })
}

fn scope_filter(default: &[&str]) -> Value {
    json!({
        "type": "array",
        "items": {"type": "string", "enum": SCOPES},
        "default": default,
        "description": "corpus roles searched; [\"all\"] alone widens",
    })
}

fn limit(default: usize) -> Value {
    json!({
        "type": "integer", "minimum": 0, "default": default,
        "description": "max rows; 0 = unlimited",
    })
}

fn max_depth() -> Value {
    json!({
        "type": "integer", "minimum": 0, "default": 10,
        "description": "traversal depth; 0 = seed only",
    })
}

fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn boolean(description: &str) -> Value {
    json!({"type": "boolean", "default": false, "description": description})
}

/// The MCP default corpus: what the same CLI verb searches.
const DEFAULT_SCOPE: [&str; 3] = ["production", "test", "docs"];
const ASK_SCOPE: [&str; 2] = ["production", "docs"];

pub(crate) fn repository() -> Value {
    let filters = traversal_filters();
    let mut list = json!({"tools": [
        {
            "name": "map",
            "description": "Repo inventory: totals, modules, dependency hubs, doc entry points.",
            "inputSchema": {"type": "object", "properties": {
                "scope": scope_filter(&DEFAULT_SCOPE),
            }},
        },
        {
            "name": "ask",
            "description": "Concept search: ranked hits per topic with status/advice; abstain = refine.",
            "inputSchema": {"type": "object", "properties": {
                "question": string("natural-language question or concept"),
                "limit": limit(5),
                "scope": scope_filter(&ASK_SCOPE),
                "explain": boolean("add ranking diagnostics per hit"),
            }, "required": ["question"]},
        },
        {
            "name": "context",
            "description": "Evidence packet for a task: symbols, deps, callers, tests, gaps, next steps.",
            "inputSchema": {"type": "object", "properties": {
                "task": string("task text; name real symbols and files"),
            }, "required": ["task"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, file, in/out edges with site; optional body.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "rows per relation; 0 = unlimited"},
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
                "body": boolean("add a source excerpt"),
                "context_lines": {"type": "integer", "minimum": 0,
                    "description": "lines around the body excerpt"},
            }},
        },
        {
            "name": "query",
            "description": "Find symbols by exact, qualified, or fuzzy name: signature, file, span.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": limit(10),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Transitive dependents of a symbol, by file; batch via symbols[].",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "max_depth": max_depth(),
                "limit": limit(50),
                "detail": boolean("full node objects instead of terse rows"),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "deps",
            "description": "What a symbol transitively depends on, by file; batch via symbols[].",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "max_depth": max_depth(),
                "limit": limit(50),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "grep",
            "description": "Regex over file text bounded to a traversal (within[]); rows f/l/t.",
            "inputSchema": {"type": "object", "properties": {
                "pattern": string("regex over file text"),
                "within": {"type": "array", "items": {"type": "string"}, "minItems": 1,
                    "description": "affected(SYM)|deps(SYM)|file(PATH)"},
                "max_depth": max_depth(),
                "limit": limit(100),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
            }, "required": ["pattern", "within"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path A->B with evidence per step; not found != absence.",
            "inputSchema": {"type": "object", "properties": {
                "from": string("source symbol"),
                "to": string("target symbol"),
                "pairs": {"type": "array", "items": {"type": "array", "items": {"type": "string"}},
                    "description": "batch: [[from, to], ...], one result each"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "unresolved",
            "description": "Unbound references (file:line, reason): check before trusting empty results.",
            "inputSchema": {"type": "object", "properties": {
                "file": string("only references in this file"),
                "name": string("only references to this name"),
                "limit": limit(50),
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, tests for a git range; HEAD = working tree.",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": string("git range: HEAD, HEAD~1..HEAD, main...branch"),
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "per collection; 0 = all"},
                "expect": {"type": "array", "items": {"type": "string"},
                    "description": "symbols the diff should cover"},
            }, "required": ["rev_range"]},
        },
        {
            "name": "overlap",
            "description": "Pairwise merge risk between in-flight change ranges (direct/radius/file).",
            "inputSchema": {"type": "object", "properties": {
                "ranges": {"type": "array", "items": {"type": "string"}, "minItems": 2,
                    "description": "two or more git ranges, optionally label=range"},
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
            "name": "context",
            "description": "Task packet across members: symbols, callers, tests, gaps, searched universe.",
            "inputSchema": {"type": "object", "properties": {
                "task": string("task text; name real symbols and files"),
            }, "required": ["task"]},
        },
        {
            "name": "ask",
            "description": "Concept search across all members: ranked hits per topic with status/advice.",
            "inputSchema": {"type": "object", "properties": {
                "question": string("natural-language question or concept"),
                "limit": limit(5),
                "scope": scope_filter(&ASK_SCOPE),
                "explain": boolean("add ranking diagnostics per hit"),
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, member edges, boundary links to other members.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "rows per relation; 0 = unlimited"},
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": "Resolve a symbol (member:Symbol) across every member.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Transitive dependents across all members, by file.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": max_depth(),
                "limit": limit(50),
                "detail": boolean("full node objects instead of terse rows"),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "deps",
            "description": "What a symbol transitively depends on across members, by file.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": max_depth(),
                "limit": limit(50),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path A->B across member boundaries.",
            "inputSchema": {"type": "object", "properties": {
                "from": string("source symbol (member:Symbol)"),
                "to": string("target symbol (member:Symbol)"),
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["from", "to"]},
        },
        {
            "name": "unresolved",
            "description": "Unbound references across members, tagged member:path.",
            "inputSchema": {"type": "object", "properties": {
                "member": string("only this workspace member"),
                "file": string("only references in this file"),
                "name": string("only references to this name"),
                "limit": limit(50),
            }},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, tests for a git range in one member, and beyond.",
            "inputSchema": {"type": "object", "properties": {
                "member": string("workspace member the range belongs to"),
                "rev_range": string("git range: HEAD, HEAD~1..HEAD, main...branch"),
                "limit": {"type": "integer", "minimum": 0, "default": 20,
                    "description": "per collection; 0 = all"},
            }, "required": ["member", "rev_range"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    list
}

/// The advertised `inputSchema` of one tool in one scope, or `None` when
/// the scope does not serve it. Runtime validation reads this so the
/// catalog is the single contract.
pub(crate) fn input_schema(name: &str, workspace_scope: bool) -> Option<Value> {
    let catalog = if workspace_scope {
        workspace()
    } else {
        repository()
    };
    catalog["tools"]
        .as_array()?
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| tool["inputSchema"].clone())
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

    /// Bytes of one scope's `tools/list` catalog, schemas included.
    const BUDGET: usize = 14_500;

    #[test]
    fn catalog_stays_within_the_context_tax_budget() {
        for catalog in [repository(), workspace()] {
            // One new verb is one new tax line. Tight on purpose — raise it
            // only for a verb, never for prose. Examples and long-form
            // guidance live in the `sinter://guide` resource, not here.
            let size = serde_json::to_string(&catalog).unwrap().len();
            assert!(size <= BUDGET, "catalog is {size} bytes");
            for tool in catalog["tools"].as_array().unwrap() {
                let description = tool["description"].as_str().unwrap();
                assert!(description.len() <= 80, "{}: {description}", tool["name"]);
                assert!(!description.contains("e.g."), "{}", tool["name"]);
                assert!(tool.get("outputSchema").is_none(), "{}", tool["name"]);
                for (key, prop) in tool["inputSchema"]["properties"].as_object().unwrap() {
                    let d = prop["description"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{}.{key} has no description", tool["name"]));
                    assert!(d.len() <= 60, "{}.{key}: {d}", tool["name"]);
                    assert!(prop.get("examples").is_none(), "{}.{key}", tool["name"]);
                    if matches!(key.as_str(), "relations" | "evidence" | "scope") {
                        assert!(prop["items"]["enum"].is_array(), "{}.{key}", tool["name"]);
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
        assert!(workspace_names.contains(&"deps"));
        assert!(!workspace_names.contains(&"map"));
        for tool in repository["tools"]
            .as_array()
            .unwrap()
            .iter()
            .chain(workspace["tools"].as_array().unwrap())
        {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
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
        assert!(super::input_schema("map", true).is_none());
        assert_eq!(
            super::input_schema("affected", false).unwrap()["properties"]["scope"]["default"],
            serde_json::json!(["production", "test", "docs"])
        );
    }
}
