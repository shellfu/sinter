use sinter_core::{Edge, Evidence, Node, NodeId, Reference, Relation, Span, SymbolKind};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

use sinter_core::FileFacts;

use crate::language::LanguageSpec;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("bad grammar or query for {language}: {message}")]
    Query {
        language: &'static str,
        message: String,
    },
    #[error("parser returned no tree for {0}")]
    Parse(String),
}

/// One reusable extractor per (language, thread): parser and compiled query
/// are pooled here, never rebuilt per file.
pub struct Extractor {
    spec: &'static LanguageSpec,
    parser: Parser,
    query: Query,
}

/// A definition or scope-only entry, pre-qualification.
struct RawEntry {
    start: usize,
    end: usize,
    name: String,
    /// None for scope-only entries (e.g. impl blocks).
    kind: Option<SymbolKind>,
    /// Extra scope prefix from the same match (e.g. Go receiver type).
    qualifier: Option<String>,
    signature: String,
    doc: Option<String>,
}

/// A reference site, pre-enclosure.
struct RawRef {
    start: usize,
    end: usize,
    name: String,
    path: Option<String>,
    alias: Option<String>,
    relation: Relation,
}

/// A local binding site, pre-scoping.
struct RawLocal {
    start: usize,
    end: usize,
    name: String,
    type_name: Option<String>,
}

/// Everything collect() gathers besides definitions.
#[derive(Default)]
struct Collected {
    refs: Vec<RawRef>,
    locals: Vec<RawLocal>,
    /// (span, embedded type name) — owner resolved after entries exist.
    embeds: Vec<(usize, usize, String)>,
    /// Import-alias name spans: identical local captures are the import
    /// binding itself, not a shadow.
    alias_spans: Vec<(usize, usize)>,
}

impl Extractor {
    pub fn new(spec: &'static LanguageSpec) -> Result<Self, ExtractError> {
        let language = (spec.grammar)();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| ExtractError::Query {
                language: spec.name,
                message: e.to_string(),
            })?;
        let query = Query::new(&language, spec.query_source).map_err(|e| ExtractError::Query {
            language: spec.name,
            message: e.to_string(),
        })?;
        Ok(Self {
            spec,
            parser,
            query,
        })
    }

    /// Extract facts from one file. `file` is the repo-relative path.
    pub fn extract(&mut self, file: &str, source: &str) -> Result<FileFacts, ExtractError> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or_else(|| ExtractError::Parse(file.to_string()))?;
        let root = tree.root_node();

        let (mut entries, collected) = self.collect(root, source);
        entries.sort_by_key(|e| (e.start, usize::MAX - e.end));
        // Two patterns may claim the same node (e.g. `const f = () => ...`
        // as variable and function): the more specific, non-variable kind
        // wins; sort puts identical spans adjacent.
        entries.dedup_by(|b, a| {
            let same = a.start == b.start && a.end == b.end && a.name == b.name;
            if same && a.kind == Some(SymbolKind::Variable) && b.kind.is_some() {
                a.kind = b.kind;
            }
            same
        });

        let file_id = NodeId::new(file);
        let mut nodes = vec![Node {
            id: file_id.clone(),
            kind: SymbolKind::File,
            name: file.to_string(),
            file: file.to_string(),
            span: Span {
                start: 0,
                end: source.len().max(1) as u64,
            },
            signature: String::new(),
            doc: None,
        }];
        let mut contains = Vec::new();

        // Containment stack: (end, scope name, node id if a real definition).
        let mut stack: Vec<(usize, String, Option<NodeId>)> = Vec::new();
        // (start, end, id) of each definition, for enclosing-ref lookup.
        let mut def_spans: Vec<(usize, usize, NodeId)> = Vec::new();

        for entry in &entries {
            while stack.last().is_some_and(|(end, _, _)| *end <= entry.start) {
                stack.pop();
            }
            let mut path: Vec<&str> = stack.iter().map(|(_, name, _)| name.as_str()).collect();
            if let Some(q) = &entry.qualifier {
                path.push(q);
            }
            path.push(&entry.name);
            let qualified = path.join("::");
            // Children nest under the entry's qualified segment.
            let scope_segment = entry
                .qualifier
                .as_ref()
                .map_or(entry.name.clone(), |q| format!("{q}::{}", entry.name));

            let id = if let Some(kind) = entry.kind {
                let id = NodeId::new(format!("{file}#{qualified}@{}", entry.start));
                let parent = stack
                    .iter()
                    .rev()
                    .find_map(|(_, _, id)| id.clone())
                    .unwrap_or_else(|| file_id.clone());
                nodes.push(Node {
                    id: id.clone(),
                    kind,
                    name: entry.name.clone(),
                    file: file.to_string(),
                    span: Span {
                        start: entry.start as u64,
                        end: entry.end as u64,
                    },
                    signature: entry.signature.clone(),
                    doc: entry.doc.clone(),
                });
                contains.push(Edge {
                    src: parent,
                    dst: id.clone(),
                    relation: Relation::Contains,
                    evidence: Evidence::Structural,
                    confidence: Evidence::Structural.confidence(),
                });
                def_spans.push((entry.start, entry.end, id.clone()));
                Some(id)
            } else {
                None
            };
            stack.push((entry.end, scope_segment, id));
        }

        let references = collected
            .refs
            .into_iter()
            .map(|r| {
                let enclosing = def_spans
                    .iter()
                    .filter(|(s, e, _)| *s <= r.start && r.end <= *e)
                    .min_by_key(|(s, e, _)| e - s)
                    .map(|(_, _, id)| id.clone());
                Reference {
                    file: file.to_string(),
                    name: r.name,
                    path: r.path,
                    relation: r.relation,
                    span: Span {
                        start: r.start as u64,
                        end: r.end as u64,
                    },
                    enclosing,
                    alias: r.alias,
                }
            })
            .collect();

        // A local shadows from its introduction to the end of the innermost
        // definition containing it (file end at top level). A "local" whose
        // span is an import alias IS the import binding, not a shadow.
        let alias_spans = collected.alias_spans;
        let embeds = collected
            .embeds
            .iter()
            .filter_map(|(start, end, type_name)| {
                let owner = def_spans
                    .iter()
                    .filter(|(s, e, _)| s <= start && end <= e)
                    .min_by_key(|(s, e, _)| e - s)
                    .map(|(_, _, id)| id.clone())?;
                Some(sinter_core::Embed {
                    owner,
                    type_name: type_name.clone(),
                })
            })
            .collect();
        let locals = collected
            .locals
            .into_iter()
            .filter(|l| !alias_spans.contains(&(l.start, l.end)))
            .map(|l| {
                let scope_end = def_spans
                    .iter()
                    .filter(|(s, e, _)| *s <= l.start && l.end <= *e)
                    .min_by_key(|(s, e, _)| e - s)
                    .map_or(source.len() as u64, |(_, e, _)| *e as u64);
                sinter_core::LocalBinding {
                    file: file.to_string(),
                    name: l.name,
                    span: Span {
                        start: l.start as u64,
                        end: l.end as u64,
                    },
                    scope_end,
                    type_name: l.type_name,
                }
            })
            .collect();

        Ok(FileFacts {
            file: file.to_string(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            has_syntax_errors: root.has_error(),
            nodes,
            contains,
            references,
            locals,
            embeds,
        })
    }

    /// Run the query; group captures per match by the universal contract.
    fn collect(&mut self, root: TsNode<'_>, source: &str) -> (Vec<RawEntry>, Collected) {
        let mut entries = Vec::new();
        let mut out = Collected::default();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, root, source.as_bytes());
        while let Some(m) = matches.next() {
            let mut def: Option<(TsNode, SymbolKind)> = None;
            let mut scope: Option<TsNode> = None;
            let mut name: Option<TsNode> = None;
            let mut qualifier: Option<TsNode> = None;
            let mut reference: Option<(TsNode, Relation)> = None;
            let mut refpath: Option<TsNode> = None;
            let mut import_path: Option<TsNode> = None;
            let mut import_module: Option<TsNode> = None;
            let mut import_name: Option<TsNode> = None;
            let mut import_alias: Option<TsNode> = None;
            let mut import_star = false;
            let mut match_locals: Vec<TsNode> = Vec::new();
            let mut local_type: Option<TsNode> = None;
            for cap in m.captures {
                let cap_name = &self.query.capture_names()[cap.index as usize];
                if let Some(kind_str) = cap_name.strip_prefix("def.") {
                    if let Some(kind) = SymbolKind::from_str_opt(kind_str) {
                        def = Some((cap.node, kind));
                    }
                } else if let Some(rel) = cap_name.strip_prefix("ref.") {
                    let relation = match rel {
                        "use" => Relation::Uses,
                        _ => Relation::Calls,
                    };
                    reference = Some((cap.node, relation));
                } else {
                    match *cap_name {
                        "scope" => scope = Some(cap.node),
                        "name" => name = Some(cap.node),
                        "qualifier" => qualifier = Some(cap.node),
                        "refpath" => refpath = Some(cap.node),
                        "import" => import_path = Some(cap.node),
                        "import.module" => import_module = Some(cap.node),
                        "import.name" => import_name = Some(cap.node),
                        "import.alias" => import_alias = Some(cap.node),
                        "import.star" => import_star = true,
                        "local" => match_locals.push(cap.node),
                        "local.type" => local_type = Some(cap.node),
                        "embed" => out.embeds.push((
                            cap.node.start_byte(),
                            cap.node.end_byte(),
                            text(cap.node, source).to_string(),
                        )),
                        _ => {}
                    }
                }
            }
            let sep = self.spec.path_separators.first().copied().unwrap_or(".");
            if let Some(a) = import_alias {
                out.alias_spans.push((a.start_byte(), a.end_byte()));
            }
            let alias = import_alias.map(|a| text(a, source).to_string());
            for l in &match_locals {
                out.locals.push(RawLocal {
                    start: l.start_byte(),
                    end: l.end_byte(),
                    name: text(*l, source).to_string(),
                    type_name: local_type.map(|t| text(t, source).to_string()),
                });
            }
            if let Some(path_node) = import_path {
                // Whole-path import (`use a::b`, `import "pkg"`), possibly
                // with an alias or Go's dot form.
                out.refs.push(RawRef {
                    start: path_node.start_byte(),
                    end: path_node.end_byte(),
                    name: text(path_node, source)
                        .trim_matches(['"', '\'', '`'])
                        .to_string(),
                    path: None,
                    alias,
                    relation: Relation::Imports,
                });
            } else if let (Some(module), Some(item)) = (import_module, import_name) {
                // From-style import: module and item joined so the import
                // binds the item itself. Alias renames the local binding.
                let module_text = text(module, source).trim_matches(['"', '\'', '`']);
                out.refs.push(RawRef {
                    start: module.start_byte().min(item.start_byte()),
                    end: item.end_byte().max(module.end_byte()),
                    name: format!("{module_text}{sep}{}", text(item, source)),
                    path: None,
                    alias,
                    relation: Relation::Imports,
                });
            } else if let (Some(module), true) = (import_module, import_star) {
                // Glob import: every top-level name of the module is bound.
                let module_text = text(module, source).trim_matches(['"', '\'', '`']);
                out.refs.push(RawRef {
                    start: module.start_byte(),
                    end: module.end_byte(),
                    name: format!("{module_text}{sep}*"),
                    path: None,
                    alias: Some("*".to_string()),
                    relation: Relation::Imports,
                });
            }
            if let Some((node, relation)) = reference {
                out.refs.push(RawRef {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    name: text(node, source).to_string(),
                    path: refpath.map(|p| text(p, source).to_string()),
                    alias: None,
                    relation,
                });
            }
            let container = def.map(|(n, _)| n).or(scope);
            if let (Some(container), Some(name_node)) = (container, name) {
                entries.push(RawEntry {
                    start: container.start_byte(),
                    end: container.end_byte(),
                    name: text(name_node, source).to_string(),
                    kind: def.map(|(_, k)| k),
                    qualifier: qualifier.map(|q| text(q, source).to_string()),
                    signature: signature(container, source),
                    doc: doc_comment(container, source, self.spec.comment_kinds),
                });
            }
        }
        (entries, out)
    }
}

fn text<'a>(node: TsNode<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// Declaration text up to the body. Brace languages cut at the first `{`;
/// a first line ending in `:` (Python-style) is the whole signature.
fn signature(node: TsNode<'_>, source: &str) -> String {
    let t = text(node, source);
    let first_line = t.lines().next().unwrap_or(t);
    let head = if first_line.trim_end().ends_with(':') || first_line.contains('{') {
        first_line.split('{').next().unwrap_or(first_line)
    } else {
        let up_to_brace = t.split('{').next().unwrap_or(t);
        if up_to_brace.len() == t.len() {
            first_line
        } else {
            up_to_brace
        }
    };
    head.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Contiguous comment siblings immediately above the definition (or its
/// parent declaration), stripped of comment markers. Generic across
/// languages: comment node kinds come from the spec.
fn doc_comment(node: TsNode<'_>, source: &str, comment_kinds: &[&str]) -> Option<String> {
    let comments = preceding_comments(node, comment_kinds).or_else(|| {
        node.parent()
            .and_then(|p| preceding_comments(p, comment_kinds))
    })?;
    let mut lines = Vec::new();
    for c in comments {
        for line in text(c, source).lines() {
            let mut l = line.trim();
            for marker in ["///", "//!", "//", "/**", "/*", "*/"] {
                if let Some(stripped) = l.strip_prefix(marker) {
                    l = stripped;
                    break;
                }
            }
            l = l.strip_suffix("*/").unwrap_or(l);
            lines.push(l.trim());
        }
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn preceding_comments<'t>(node: TsNode<'t>, comment_kinds: &[&str]) -> Option<Vec<TsNode<'t>>> {
    let mut comments = Vec::new();
    let mut cur = node.prev_named_sibling();
    while let Some(sib) = cur {
        if !comment_kinds.contains(&sib.kind()) {
            break;
        }
        comments.push(sib);
        cur = sib.prev_named_sibling();
    }
    comments.reverse();
    if comments.is_empty() {
        None
    } else {
        Some(comments)
    }
}
