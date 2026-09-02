//! Question → deterministic query model. Owns term normalization, the
//! action-verb vocabulary, code synonyms, and ordered phrases. Ranking
//! consumes a `Query`; it never re-parses the question.

use std::collections::HashSet;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "by", "can", "could", "do", "does", "find",
    "for", "from", "how", "i", "in", "include", "inside", "into", "is", "it", "its", "like",
    "located", "make", "may", "me", "might", "must", "my", "of", "on", "only", "or", "our",
    "shall", "should", "show", "that", "the", "these", "this", "those", "to", "up", "was", "we",
    "were", "what", "where", "which", "who", "whom", "will", "with", "would", "you", "your",
];

const SOFT_STOPWORDS: &[&str] = &[
    "code",
    "going",
    "happen",
    "happens",
    "stuff",
    "thing",
    "things",
    "use",
    "used",
    "uses",
    "using",
    "work",
    "working",
    "works",
    "compared",
    "comparison",
    "comparisons",
    "describe",
    "described",
    "describes",
    "docs",
    "documentation",
    "documented",
    "explain",
    "explained",
    "explains",
    "overview",
    "related",
];

/// Verbs that name what code does. A term whose stem is here marks the
/// question as asking for behavior, which shifts ranking toward callables
/// and away from the types that merely own them.
const ACTION_VERBS: &[&str] = &[
    "abort",
    "accept",
    "acquire",
    "add",
    "advance",
    "aggregate",
    "allocate",
    "append",
    "apply",
    "assemble",
    "assert",
    "associate",
    "authenticate",
    "authorize",
    "begin",
    "bind",
    "boot",
    "bootstrap",
    "build",
    "cache",
    "calculate",
    "call",
    "cancel",
    "check",
    "clamp",
    "clear",
    "clone",
    "close",
    "collect",
    "commit",
    "compare",
    "compile",
    "complete",
    "compose",
    "compress",
    "compute",
    "configure",
    "confirm",
    "connect",
    "construct",
    "consume",
    "convert",
    "copy",
    "count",
    "create",
    "debounce",
    "decode",
    "decompress",
    "decrement",
    "decrypt",
    "defer",
    "define",
    "delay",
    "delete",
    "deploy",
    "derive",
    "deserialize",
    "detect",
    "dispatch",
    "display",
    "drain",
    "draw",
    "drop",
    "emit",
    "enable",
    "encode",
    "encrypt",
    "end",
    "enforce",
    "escape",
    "evaluate",
    "exclude",
    "exec",
    "execute",
    "exit",
    "expand",
    "extract",
    "fail",
    "fetch",
    "filter",
    "finalize",
    "find",
    "finish",
    "flatten",
    "flush",
    "focus",
    "format",
    "free",
    "generate",
    "get",
    "group",
    "handle",
    "hash",
    "increment",
    "initialize",
    "insert",
    "inspect",
    "install",
    "intercept",
    "interpolate",
    "invoke",
    "iterate",
    "join",
    "kill",
    "launch",
    "list",
    "listen",
    "load",
    "localize",
    "lock",
    "log",
    "look",
    "lookup",
    "make",
    "mark",
    "marshal",
    "match",
    "measure",
    "merge",
    "migrate",
    "mount",
    "navigate",
    "normalize",
    "notify",
    "open",
    "pack",
    "paint",
    "parse",
    "pause",
    "peek",
    "pick",
    "poll",
    "pop",
    "print",
    "process",
    "profile",
    "prompt",
    "publish",
    "push",
    "query",
    "quit",
    "quote",
    "raise",
    "read",
    "receive",
    "record",
    "redirect",
    "refresh",
    "register",
    "reject",
    "release",
    "reload",
    "remove",
    "render",
    "replace",
    "report",
    "reserve",
    "reset",
    "resize",
    "resolve",
    "respond",
    "restart",
    "resume",
    "retry",
    "return",
    "reverse",
    "rollback",
    "rotate",
    "round",
    "route",
    "run",
    "sanitize",
    "save",
    "scan",
    "schedule",
    "scroll",
    "search",
    "select",
    "send",
    "serialize",
    "serve",
    "set",
    "setup",
    "shuffle",
    "shut",
    "shutdown",
    "sign",
    "skip",
    "sleep",
    "sort",
    "spawn",
    "spin",
    "split",
    "start",
    "stop",
    "store",
    "stream",
    "stringify",
    "strip",
    "submit",
    "subscribe",
    "substitute",
    "sum",
    "suspend",
    "swap",
    "sync",
    "tear",
    "teardown",
    "terminate",
    "throttle",
    "throw",
    "toggle",
    "tokenize",
    "trace",
    "track",
    "transform",
    "translate",
    "traverse",
    "trim",
    "truncate",
    "unlock",
    "unmarshal",
    "unpack",
    "unwrap",
    "update",
    "upgrade",
    "validate",
    "verify",
    "visit",
    "wait",
    "walk",
    "warn",
    "wire",
    "wrap",
    "write",
    "yield",
];

#[derive(Clone, Debug)]
pub(super) struct QueryTerm {
    surface: String,
    /// Surface form plus morphological stems and code synonyms.
    variants: Vec<String>,
    /// Surface form plus morphological stems only: what the user literally
    /// said, for signals that must not fire on a loose synonym.
    core: Vec<String>,
    is_action: bool,
}

impl QueryTerm {
    pub(super) fn surface(&self) -> &str {
        &self.surface
    }

    pub(super) fn variants(&self) -> &[String] {
        &self.variants
    }

    pub(super) fn is_action(&self) -> bool {
        self.is_action
    }

    /// True when `token` is exactly a literal form of this term (no synonym).
    pub(super) fn is_core_token(&self, token: &str) -> bool {
        self.core.iter().any(|core| core == token)
    }

    /// True when `haystack` (lowercased) contains a literal form of this
    /// term (surface or stem, no synonym).
    pub(super) fn core_occurs_in(&self, haystack: &str) -> bool {
        self.core.iter().any(|core| haystack.contains(core))
    }

    /// True when `haystack` (lowercased) contains any variant.
    pub(super) fn occurs_in(&self, haystack: &str) -> bool {
        self.variants
            .iter()
            .any(|variant| haystack.contains(variant))
    }

    /// True when `token` is one identifier token for this term: equal to a
    /// variant, a longer word that starts with a variant of at least four
    /// characters (`matcher` for `match`, `args` for `arg`), or a short
    /// prefix abbreviation of a long literal form (`gen` for `generate`,
    /// `init` for `initialize`).
    pub(super) fn matches_token(&self, token: &str) -> bool {
        self.variants.iter().any(|variant| {
            token == variant || (variant.len() >= 4 && token.starts_with(variant.as_str()))
        }) || self.abbreviates(token)
    }

    pub(super) fn abbreviates(&self, token: &str) -> bool {
        token.len() >= 3
            && self
                .core
                .iter()
                .any(|core| core.len() >= 6 && core.starts_with(token))
    }
}

/// Words that mark a question as reaching for prose (install guide,
/// publishing steps) rather than the code that implements it.
const DOC_MARKERS: &[&str] = &[
    "how do i",
    "how to",
    "install",
    "publish",
    "guide",
    "tutorial",
    "readme",
    "documentation",
    "docs",
    "changelog",
    "license",
    "getting started",
    "contributing",
];

/// Words that only an engineer asks with: the question wants a symbol.
const CODE_WORDS: &[&str] = &[
    "fn",
    "function",
    "functions",
    "struct",
    "structs",
    "impl",
    "trait",
    "enum",
    "type",
    "types",
    "method",
    "methods",
    "class",
    "module",
    "crate",
    "test",
    "tests",
    "call",
    "calls",
    "caller",
    "callers",
    "callee",
    "symbol",
    "symbols",
    "import",
    "imports",
    "return",
    "returns",
    "where",
];

/// The text reaches for prose (install guide, publishing steps) rather
/// than the code that implements it.
pub(crate) fn wants_docs(text: &str) -> bool {
    let lower = text.to_lowercase();
    DOC_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// `parse_args`, `Index::build`, `src/ask.rs`, `fooBar`, `run(`.
fn looks_like_identifier(word: &str) -> bool {
    word.contains('_')
        || word.contains("::")
        || word.contains('/')
        || word.contains('(')
        || word.contains(".rs")
        || (word.chars().any(char::is_lowercase) && word.chars().skip(1).any(char::is_uppercase))
}

/// Parsed question. `phrases` pairs indexes into `terms` that were
/// adjacent in the question ("command line" in "command line arguments"),
/// so ranking can reward identifiers that keep the same order.
#[derive(Clone, Debug)]
pub(super) struct Query {
    terms: Vec<QueryTerm>,
    phrases: Vec<(usize, usize)>,
    action: bool,
    wants_docs: bool,
    engineering: bool,
}

impl Query {
    pub(super) fn parse(question: &str) -> Self {
        let lower = question.to_lowercase();
        let mut seen = HashSet::new();
        // (position in question, term) for every retained word.
        let mut positioned: Vec<(usize, QueryTerm)> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .enumerate()
            .filter(|(_, word)| !STOPWORDS.contains(word))
            .filter(|(_, word)| seen.insert((*word).to_owned()))
            .map(|(position, word)| (position, normalize_term(word)))
            .collect();
        let has_hard = positioned
            .iter()
            .any(|(_, term)| !SOFT_STOPWORDS.contains(&term.surface.as_str()));
        if has_hard {
            positioned.retain(|(_, term)| !SOFT_STOPWORDS.contains(&term.surface.as_str()));
        }
        if lower.contains("command line interface")
            && let Some((_, interface)) = positioned
                .iter_mut()
                .find(|(_, term)| term.surface == "interface")
        {
            interface.variants.push("cli".into());
        }
        let phrases = positioned
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[1].0 == pair[0].0 + 1)
            .map(|(index, _)| (index, index + 1))
            .collect();
        let terms: Vec<QueryTerm> = positioned.into_iter().map(|(_, term)| term).collect();
        let action = terms.iter().any(QueryTerm::is_action);
        let wants_docs = wants_docs(&lower);
        let words = lower.split_whitespace().collect::<Vec<_>>();
        let engineering = !wants_docs
            && (action
                || question.split_whitespace().any(looks_like_identifier)
                || words.iter().any(|word| CODE_WORDS.contains(word)));
        Self {
            terms,
            phrases,
            action,
            wants_docs,
            engineering,
        }
    }

    pub(super) fn terms(&self) -> &[QueryTerm] {
        &self.terms
    }

    pub(super) fn phrases(&self) -> &[(usize, usize)] {
        &self.phrases
    }

    /// The question asks what code does (has an action verb), not only
    /// what a thing is.
    pub(super) fn is_action(&self) -> bool {
        self.action
    }

    /// The question reaches for prose: a section may outrank code.
    pub(super) fn wants_docs(&self) -> bool {
        self.wants_docs
    }

    /// The question names code (identifier, path, code word, or action
    /// verb): a section describing the code must not outrank the code.
    pub(super) fn is_engineering(&self) -> bool {
        self.engineering
    }

    pub(super) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.terms.len()
    }

    pub(super) fn surface_text(&self, separator: &str) -> String {
        self.terms
            .iter()
            .map(QueryTerm::surface)
            .collect::<Vec<_>>()
            .join(separator)
    }
}

/// Split a question into independent topics on `,`, `;`, and " or ".
pub(super) fn clauses_of(question: &str) -> Vec<(String, Query)> {
    let lower = question.to_lowercase();
    let mut seen = HashSet::new();
    lower
        .split([',', ';'])
        .flat_map(|segment| segment.split(" or "))
        .filter_map(|clause| {
            let query = Query::parse(clause);
            let label = query.surface_text(" ");
            (!query.is_empty()).then_some((label, query))
        })
        .filter(|(label, _)| seen.insert(label.clone()))
        .collect()
}

fn normalize_term(surface: &str) -> QueryTerm {
    let mut variants = vec![surface.to_owned()];
    match surface {
        "built" => variants.push("build".into()),
        "ran" => variants.push("run".into()),
        "sent" => variants.push("send".into()),
        "torn" => variants.push("teardown".into()),
        "thrown" => variants.push("throw".into()),
        "written" | "wrote" => variants.push("write".into()),
        "found" => variants.push("find".into()),
        "made" => variants.push("make".into()),
        "chosen" => variants.push("choose".into()),
        _ => {}
    }
    if let Some(stem) = surface.strip_suffix("ied") {
        variants.push(format!("{stem}y"));
    }
    if let Some(stem) = surface.strip_suffix("ing") {
        add_verb_stems(&mut variants, stem);
    }
    if let Some(stem) = surface.strip_suffix("ed") {
        add_verb_stems(&mut variants, stem);
    }
    if let Some(singular) = surface
        .strip_suffix('s')
        .filter(|singular| !singular.is_empty() && !surface.ends_with("ss"))
    {
        variants.push(singular.to_owned());
    }
    let is_action = variants
        .iter()
        .any(|variant| ACTION_VERBS.contains(&variant.as_str()));
    let mut core = variants.clone();
    core.sort();
    core.dedup();
    add_query_synonyms(&mut variants);
    variants.sort();
    variants.dedup();
    QueryTerm {
        surface: surface.to_owned(),
        variants,
        core,
        is_action,
    }
}

fn add_query_synonyms(variants: &mut Vec<String>) {
    let originals = variants.clone();
    for variant in originals {
        let synonyms: &[&str] = match variant.as_str() {
            "application" => &["app"],
            "argument" => &["arg", "args"],
            "authentication" => &["auth"],
            "calculate" => &["get", "compute"],
            "check" => &["validate"],
            "cap" => &["limit", "budget", "truncate", "max"],
            "limit" => &["cap", "budget", "max"],
            "budget" => &["limit", "cap"],
            "truncate" => &["cap", "limit", "budget"],
            "size" => &["bytes", "length", "len"],
            "bytes" => &["size", "length"],
            "caller" => &["dependent", "user"],
            "dependent" => &["caller", "user"],
            "user" => &["caller", "dependent"],
            "setting" => &["config", "option", "cfg"],
            "option" => &["config", "setting"],
            "failure" => &["error", "err"],
            "err" => &["error", "failure"],
            "result" => &["output", "response"],
            "delete" => &["remove"],
            "remove" => &["delete"],
            "new" => &["create", "init"],
            "init" => &["create", "new"],
            "read" => &["load", "fetch"],
            "fetch" => &["read", "load"],
            "write" => &["save", "store"],
            "save" => &["write", "store"],
            "store" => &["write", "save"],
            "compute" => &["calculate"],
            "configuration" => &["config"],
            "create" => &["new", "make", "init"],
            "deserialize" => &["fromjson", "read", "decode"],
            "exactly" => &["exact"],
            "flag" => &["arg", "args"],
            "look" => &["lookup", "find", "get"],
            "lookup" => &["find", "get"],
            "load" => &["open", "read", "fetch"],
            "extract" => &["get"],
            "run" => &["execute", "exec"],
            "execute" => &["run", "exec"],
            "raise" => &["throw"],
            "throw" => &["raise"],
            "template" => &["tmpl"],
            "command" => &["cmd"],
            "config" => &["cfg", "settings", "options"],
            "context" => &["ctx"],
            "message" => &["msg"],
            "request" => &["req"],
            "response" => &["res", "resp", "output"],
            "error" => &["err", "failure"],
            "function" => &["fn", "func"],
            "initialize" => &["init"],
            "directory" => &["dir"],
            "number" => &["num"],
            "string" => &["str"],
            "buffer" => &["buf"],
            "parameter" => &["param"],
            "object" => &["obj"],
            "index" => &["idx"],
            "iterator" => &["iter"],
            "attribute" => &["attr"],
            "value" => &["val"],
            "source" => &["src"],
            "destination" => &["dst", "dest"],
            "implementation" => &["impl"],
            "specification" => &["spec"],
            "environment" => &["env"],
            "variable" => &["var"],
            "reference" => &["ref"],
            "pointer" => &["ptr"],
            "length" => &["len", "size", "bytes"],
            "maximum" => &["max"],
            "minimum" => &["min"],
            "previous" => &["prev"],
            "current" => &["cur", "curr"],
            "temporary" => &["tmp", "temp"],
            "output" => &["sink", "response"],
            "register" => &["add"],
            "route" => &["rule"],
            "serialize" => &["tojson", "write", "encode"],
            "start" => &["run", "launch", "serve"],
            "subcommand" => &["command", "traverse"],
            "validate" => &["check"],
            _ => &[],
        };
        variants.extend(synonyms.iter().map(|synonym| (*synonym).to_owned()));
    }
}

fn add_verb_stems(variants: &mut Vec<String>, stem: &str) {
    let mut candidates = vec![stem.to_owned(), format!("{stem}e")];
    let chars = stem.chars().collect::<Vec<_>>();
    if chars.len() >= 2 && chars[chars.len() - 1] == chars[chars.len() - 2] {
        candidates.push(chars[..chars.len() - 1].iter().collect());
    }
    variants.extend(
        candidates
            .into_iter()
            .filter(|candidate| ACTION_VERBS.contains(&candidate.as_str())),
    );
}

/// English filler with no chance of naming a symbol worth resolving.
/// `context` reuses the question parser's vocabulary instead of keeping a
/// second copy of it.
pub(crate) fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word) || SOFT_STOPWORDS.contains(&word)
}

/// Identifier → lowercase word tokens: `parse_low` → [parse, low],
/// `GenZshCompletion` → [gen, zsh, completion], `HTTPException` →
/// [http, exception].
pub(super) fn identifier_tokens(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            flush(&mut tokens, &mut current);
            continue;
        }
        let prev_lower = index > 0 && chars[index - 1].is_lowercase();
        let next_lower = chars.get(index + 1).is_some_and(|n| n.is_lowercase());
        let prev_upper = index > 0 && chars[index - 1].is_uppercase();
        if c.is_uppercase() && (prev_lower || (prev_upper && next_lower)) {
            flush(&mut tokens, &mut current);
        }
        current.extend(c.to_lowercase());
    }
    flush(&mut tokens, &mut current);
    tokens
}

fn flush(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

#[cfg(test)]
mod tests {
    use super::{Query, clauses_of, identifier_tokens, normalize_term};

    #[test]
    fn conservative_morphology_normalizes_code_actions() {
        assert!(normalize_term("parsed").variants.contains(&"parse".into()));
        assert!(
            normalize_term("matching")
                .variants
                .contains(&"match".into())
        );
        assert!(normalize_term("built").variants.contains(&"build".into()));
        assert_eq!(normalize_term("matcher").variants, vec!["matcher"]);
    }

    #[test]
    fn code_vocabulary_expands_directionally() {
        assert!(
            normalize_term("registered")
                .variants
                .contains(&"add".into())
        );
        assert!(
            normalize_term("arguments")
                .variants
                .contains(&"args".into())
        );
        assert!(normalize_term("route").variants.contains(&"rule".into()));
    }

    #[test]
    fn command_line_interface_adds_initialism_variant() {
        let query = Query::parse("where does the command line interface load the application");
        let interface = query
            .terms()
            .iter()
            .find(|term| term.surface == "interface")
            .unwrap();
        assert!(interface.variants.contains(&"cli".into()));
    }

    #[test]
    fn weak_terms_drop_when_specific_terms_remain() {
        let query = Query::parse("where does this code work for parsed arguments");
        assert_eq!(query.surface_text(" "), "parsed arguments");
        assert!(query.is_action());
    }

    #[test]
    fn phrases_keep_adjacent_words_only() {
        let query = Query::parse("where is the default error handler for a command");
        assert_eq!(query.surface_text(" "), "default error handler command");
        // "default error", "error handler" adjacent; "handler ... command" not.
        assert_eq!(query.phrases(), &[(0, 1), (1, 2)]);
    }

    #[test]
    fn engineering_and_docs_intent_are_cheap_surface_checks() {
        assert!(Query::parse("where is the trigram search").is_engineering());
        assert!(Query::parse("Index::build callers").is_engineering());
        assert!(Query::parse("tests proving impact selects affected tests").is_engineering());
        let install = Query::parse("how do I install sinter");
        assert!(install.wants_docs());
        assert!(!install.is_engineering());
        assert!(!Query::parse("sinter skill card hooks").is_engineering());
    }

    #[test]
    fn clauses_are_deduplicated_after_normalization() {
        let clauses = clauses_of("parser, parser or matcher");
        assert_eq!(clauses.len(), 2);
        assert_eq!(clauses[0].0, "parser");
        assert_eq!(clauses[1].0, "matcher");
    }

    #[test]
    fn short_identifier_tokens_abbreviate_long_terms() {
        let generated = normalize_term("generated");
        assert!(generated.matches_token("gen"));
        assert!(generated.matches_token("generate"));
        assert!(!generated.matches_token("ge"));
        assert!(!normalize_term("get").matches_token("gen"));
    }

    #[test]
    fn identifier_tokens_split_case_and_separators() {
        assert_eq!(identifier_tokens("parse_low"), ["parse", "low"]);
        assert_eq!(
            identifier_tokens("GenZshCompletion"),
            ["gen", "zsh", "completion"]
        );
        assert_eq!(identifier_tokens("HTTPException"), ["http", "exception"]);
        assert_eq!(identifier_tokens("fromJson"), ["from", "json"]);
    }
}
