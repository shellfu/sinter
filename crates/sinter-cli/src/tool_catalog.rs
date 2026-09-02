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
`grep` is a regex over file *text*. Over MCP `within` is required and bounds the search to
what a traversal reached: `within: [affected(SYM)]`, `[deps(SYM)]`, or `[file(PATH)]`,
repeatable and unioned (the CLI `sinter grep` runs unbounded without `--within`); rows are
f=file l=line t=text and `total` counts every match above `limit`. `show` takes `body: true`
(with `context_lines`; 0 = whole span) for a source excerpt in `excerpt`: whole when short,
else up to the byte budget; `symbol: \"X@file:line\"` shows the enclosing symbol. `affected`
and `deps` are transitive to `max_depth` (default 10); pass `max_depth: 1` for direct rows.
`impact` takes `expect: [SYM]` and answers the unfinished-refactor question: per symbol, the
direct dependents this diff changed and the ones it still owes (`expect[].untouched`); names
resolve at the base rev too, and a body-only change reports its callers unaffected.
`context` returns `next_actions` as tool calls `{tool, args}` ready to send back, plus
`literals` and `mirrors` (string-literal and hand-maintained-copy hits for the task words).

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
`unresolved` lists user gaps (references in indexed code the graph could not bind; external,
resolver-gap and unsupported-syntax refs are counted, not listed); check it before reading
an empty affected/deps/path result as absence.
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
`main...branch`); `overlap` takes two or more, optionally `label=range`, and a
`relations` filter for its radius tier (default calls,uses).

## Filters
`relations` (calls, uses, imports, implements, extends, reads, writes, creates, alters,
drops; any `relations` filter drops file-level import noise) and `scope` (corpus roles:
production, test, fixture, example, generated, vendor, docs; `all` must be used alone).
A `not_proven` outcome with `reason: filter_excluded` means the filter emptied the result,
not the graph. `if_snapshot`: pass a prior `snapshot` token to fail instead of answering
from a changed graph. The evidence-tier filters (`--evidence`, `--min-confidence`) and
`grep --within` depth stay CLI flags; `coverage.evidence` still reports the tiers a
result rests on.
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

/// Enum-valued array property. `items` carries the vocabulary and no
/// redundant `"type": "string"`: the enum already names every legal value,
/// and one duplicated word per filter is a dozen lines of standing tax.
fn enum_array(values: &[&str], description: &str) -> Value {
    json!({
        "type": "array",
        "items": {"enum": values},
        "description": description,
    })
}

/// Edge relations to follow. `evidence` and `min_confidence` stay CLI
/// flags: `relations` is the filter that pays for its schema bytes.
/// Evidence tiers a traversal may follow. Restoring this to the MCP
/// surface costs bytes; an agent that cannot ask for compiler-grade-only
/// answers cannot make a defensible negative claim.
fn evidence() -> Value {
    json!({
        "type": "array",
        "items": {"enum": ["structural", "scope", "import", "scip", "declared", "dynamic"]},
        "description": "evidence tiers to follow",
    })
}

fn min_confidence() -> Value {
    json!({
        "type": "string",
        "enum": ["certain", "inferred"],
        "description": "lowest edge confidence",
    })
}

fn relations() -> Value {
    enum_array(&RELATIONS, "relations to follow")
}

fn snapshot_precondition() -> Value {
    json!({"type": "string", "description": "fail if snapshot changed"})
}

fn symbol() -> Value {
    json!({"type": "string", "description": "symbol_key, name, or name@file"})
}

fn symbols() -> Value {
    json!({
        "type": "array", "items": {"type": "string"},
        "description": "batch: one result each",
    })
}

fn scope_filter(default: &[&str]) -> Value {
    let mut value = enum_array(&SCOPES, "corpus roles; \"all\" alone");
    if default == ASK_SCOPE {
        // Only the odd one out is worth a `default` line; the rest is the
        // CLI corpus the guide states once.
        value["default"] = json!(default);
    }
    value
}

/// Per-tool defaults are listed once in the guide, not restated on
/// every schema.
fn limit() -> Value {
    json!({"type": "integer", "description": "max rows; 0 = all"})
}

fn include_tests() -> Value {
    json!({"type": "boolean", "description": "list test-scope rows"})
}

fn max_depth() -> Value {
    json!({"type": "integer", "minimum": 0, "description": "depth; 0 = seed (10)"})
}

fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn boolean(description: &str) -> Value {
    json!({"type": "boolean", "description": description})
}

/// The MCP default corpus: what the same CLI verb searches.
const DEFAULT_SCOPE: [&str; 3] = ["production", "test", "docs"];
const ASK_SCOPE: [&str; 2] = ["production", "docs"];

/// Tools whose payload carries a `coverage` block. Everywhere else
/// `include_coverage` would be a knob with nothing to turn, so the
/// injected argument is taken back off the schema.
const REPOSITORY_COVERAGE: [&str; 5] = ["affected", "deps", "path", "grep", "context"];
const WORKSPACE_COVERAGE: [&str; 4] = ["affected", "deps", "path", "context"];

/// Strip `include_coverage` from tools that never emit one.
fn drop_unused_coverage_flag(list: &mut Value, emitters: &[&str]) {
    for tool in list["tools"].as_array_mut().into_iter().flatten() {
        if emitters.contains(&tool["name"].as_str().unwrap_or_default()) {
            continue;
        }
        if let Some(props) = tool["inputSchema"]["properties"].as_object_mut() {
            props.remove("include_coverage");
        }
    }
}

pub(crate) fn repository() -> Value {
    let mut list = json!({"tools": [
        {
            "name": "map",
            "description": "Repo inventory: modules, hubs, doc entry points.",
            "inputSchema": {"type": "object", "properties": {
                "scope": scope_filter(&DEFAULT_SCOPE),
            }},
        },
        {
            "name": "ask",
            "description": "Concept search: ranked hits; abstain = refine.",
            "inputSchema": {"type": "object", "properties": {
                "question": string("question or concept"),
                "limit": limit(),
                "scope": scope_filter(&ASK_SCOPE),
                "explain": boolean("add ranking detail"),
            }, "required": ["question"]},
        },
        {
            "name": "context",
            "description": "Task packet: symbols, callers, tests, gaps, next steps.",
            "inputSchema": {"type": "object", "properties": {
                "task": string("task text; name symbols"),
            }, "required": ["task"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, edges, optional body.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "limit": {"type": "integer",
                    "description": "rows per relation; 0 = all (20)"},
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
                "body": boolean("add a source excerpt"),
                "context_lines": {"type": "integer",
                    "description": "lines around excerpt"},
            }},
        },
        {
            "name": "query",
            "description": "Find symbols by exact, qualified, or fuzzy name.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": limit(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Transitive dependents of a symbol, by file.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "max_depth": max_depth(),
                "include_tests": include_tests(),
                "through_hubs": boolean("keep going past hubs"),
                "limit": limit(),
                "detail": boolean("full nodes, not rows"),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "deps",
            "description": "What a symbol depends on (depth 1), by file.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "symbols": symbols(),
                "max_depth": {"type": "integer", "minimum": 0,
                    "description": "depth; 0 = seed (1)"},
                "include_tests": include_tests(),
                "limit": limit(),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "grep",
            "description": "Regex over file text, bounded by within[].",
            "inputSchema": {"type": "object", "properties": {
                "pattern": string("regex over file text"),
                "within": {"type": "array", "items": {"type": "string"},
                    "description": "affected(SYM)|deps(SYM)|file(PATH)"},
                "no_tests": boolean("skip test-scoped files"),
                "max_depth": max_depth(),
                "limit": limit(),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
            }, "required": ["pattern"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path A->B with evidence.",
            "inputSchema": {"type": "object", "properties": {
                "from": string("source symbol"),
                "to": string("target symbol"),
                "pairs": {"type": "array", "items": {"type": "array", "items": {"type": "string"}},
                    "description": "batch: [[from, to], ...]"},
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }},
        },
        {
            "name": "unresolved",
            "description": "Unbound references: check before claiming absence.",
            "inputSchema": {"type": "object", "properties": {
                "file": string("only refs in this file"),
                "name": string("only refs to this name"),
                "limit": limit(),
            }},
        },
        {
            "name": "impact",
            "description": "Blast radius and tests for a git range.",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": string("git range, e.g. HEAD~1..HEAD"),
                "limit": {"type": "integer",
                    "description": "per collection; 0 = all (20)"},
                "expect": {"type": "array", "items": {"type": "string"},
                    "description": "symbols the diff should cover"},
            }, "required": ["rev_range"]},
        },
        {
            "name": "overlap",
            "description": "Pairwise merge risk between change ranges.",
            "inputSchema": {"type": "object", "properties": {
                "ranges": {"type": "array", "items": {"type": "string"}, "minItems": 2,
                    "description": "git ranges; label=range"},
                "relations": enum_array(&RELATIONS, "radius relations (default calls,uses)"),
            }, "required": ["ranges"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    drop_unused_coverage_flag(&mut list, &REPOSITORY_COVERAGE);
    list
}

pub(crate) fn workspace() -> Value {
    let mut list = json!({"tools": [
        {
            "name": "context",
            "description": "Task packet across members: callers, tests, gaps.",
            "inputSchema": {"type": "object", "properties": {
                "task": string("task text; name symbols"),
            }, "required": ["task"]},
        },
        {
            "name": "ask",
            "description": "Concept search across every member.",
            "inputSchema": {"type": "object", "properties": {
                "question": string("question or concept"),
                "limit": limit(),
                "scope": scope_filter(&ASK_SCOPE),
                "explain": boolean("add ranking detail"),
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": "One symbol: signature, doc, cross-member edges.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "limit": {"type": "integer",
                    "description": "rows per relation; 0 = all (20)"},
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
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
            "description": "Transitive dependents across members, by file.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": max_depth(),
                "limit": limit(),
                "detail": boolean("full nodes, not rows"),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "deps",
            "description": "What a symbol depends on across members.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": symbol(),
                "max_depth": max_depth(),
                "limit": limit(),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path A->B across members.",
            "inputSchema": {"type": "object", "properties": {
                "from": string("source symbol (member:Symbol)"),
                "to": string("target symbol (member:Symbol)"),
                "relations": relations(),
                "evidence": evidence(),
                "min_confidence": min_confidence(),
                "scope": scope_filter(&DEFAULT_SCOPE),
                "if_snapshot": snapshot_precondition(),
            }, "required": ["from", "to"]},
        },
        {
            "name": "unresolved",
            "description": "Unbound references across members.",
            "inputSchema": {"type": "object", "properties": {
                "member": string("only this member"),
                "file": string("only refs in this file"),
                "name": string("only refs to this name"),
                "limit": limit(),
            }},
        },
        {
            "name": "impact",
            "description": "Blast radius and tests for a member's range.",
            "inputSchema": {"type": "object", "properties": {
                "member": string("member owning the range"),
                "rev_range": string("git range, e.g. HEAD~1..HEAD"),
                "limit": {"type": "integer",
                    "description": "per collection; 0 = all (20)"},
            }, "required": ["member", "rev_range"]},
        },
    ]});
    crate::agent_protocol::complete_tool_schemas(&mut list);
    drop_unused_coverage_flag(&mut list, &WORKSPACE_COVERAGE);
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

    /// Bytes of one scope's `tools/list` catalog, schemas included. This
    /// is the standing context tax: every session pays it before asking
    /// anything. Repository scope compact-serializes to 9_832 bytes here
    /// and measures 10_575 on the wire (`sum(len(json.dumps(tool)))`
    /// over a live `tools/list`, which pads every separator).
    const BUDGET: usize = 12_000;

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
                assert!(description.len() <= 60, "{}: {description}", tool["name"]);
                assert!(!description.contains("e.g."), "{}", tool["name"]);
                assert!(tool.get("outputSchema").is_none(), "{}", tool["name"]);
                for (key, prop) in tool["inputSchema"]["properties"].as_object().unwrap() {
                    let d = prop["description"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{}.{key} has no description", tool["name"]));
                    assert!(d.len() <= 40, "{}.{key}: {d}", tool["name"]);
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
        // `include_coverage` is injected for every tool and taken back off
        // the ones whose payload never carries a coverage block.
        for (catalog, emitters) in [
            (&repository, &super::REPOSITORY_COVERAGE[..]),
            (&workspace, &super::WORKSPACE_COVERAGE[..]),
        ] {
            for tool in catalog["tools"].as_array().unwrap() {
                let name = tool["name"].as_str().unwrap();
                let advertised = tool["inputSchema"]["properties"]
                    .get("include_coverage")
                    .is_some();
                assert_eq!(advertised, emitters.contains(&name), "{name}");
            }
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
            let impact = catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "impact")
                .unwrap();
            assert_eq!(
                impact["inputSchema"]["properties"]["limit"]["type"],
                "integer"
            );
        }
        assert!(super::input_schema("map", true).is_none());
        // A client that reads only the schema still learns the vocabulary.
        assert_eq!(
            super::input_schema("affected", false).unwrap()["properties"]["scope"]["items"]["enum"],
            serde_json::json!(super::SCOPES)
        );
        // `overlap` filters its radius tier like the CLI verb does.
        assert_eq!(
            super::input_schema("overlap", false).unwrap()["properties"]["relations"]["items"]["enum"],
            serde_json::json!(super::RELATIONS)
        );
    }
}
