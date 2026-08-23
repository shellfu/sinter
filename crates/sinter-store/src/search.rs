//! Symbol search: exact name index plus lowercased-trigram fuzzy index.
//! Both are maintained incrementally by `update.rs`.

use std::collections::{BTreeSet, HashMap};

use redb::ReadableDatabase;
use sinter_core::Node;

use crate::error::StoreError;
use crate::store::{BODY_TERMS, INTERN, NAME_NODES, NODES, Store, TOKENS_WORDS};

/// Lowercased character trigrams of a name; names shorter than 3 chars
/// index as one whole-name gram.
pub(crate) fn trigrams(name: &str) -> Vec<String> {
    let lower: Vec<char> = name.to_lowercase().chars().collect();
    if lower.len() < 3 {
        return vec![lower.iter().collect()];
    }
    let mut grams: Vec<String> = lower.windows(3).map(|w| w.iter().collect()).collect();
    grams.sort();
    grams.dedup();
    grams
}

/// Distinct lowercase words a node is findable by: name, signature, doc
/// text, and file-path segments. Identifiers split on non-alphanumerics
/// (snake_case, path separators) and camelCase boundaries, including
/// acronym-word ("HTTPServer" -> http, server); each full identifier is
/// also indexed whole ("PlayerCharacterV2" -> "playercharacterv2") so
/// exact-name lookup stays one keyed read. Subwords shorter than 2 chars
/// are dropped. This is a RECALL filter, not the scorer: the consumer
/// re-scores candidates with its own substring logic, so over-inclusion is
/// fine. Recall is subword-boundary based — substrings crossing subword
/// boundaries ("rchar") are not indexed (accepted limitation, design §4).
pub(crate) fn node_tokens(node: &Node) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    for text in [
        node.name.as_str(),
        node.signature.as_str(),
        node.file.as_str(),
        node.doc.as_deref().unwrap_or(""),
    ] {
        for ident in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
        {
            for sub in camel_split(ident) {
                if sub.chars().count() >= 2 {
                    words.insert(sub);
                }
            }
            let whole = ident.to_lowercase();
            if whole.chars().count() >= 2 {
                words.insert(whole);
            }
        }
    }
    words
}

/// Lowercased camelCase subwords: a boundary before an uppercase char that
/// follows a non-uppercase one (aB, 2B) or that starts a lowercase run
/// after an acronym (the S in "HTTPServer").
fn camel_split(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_uppercase()
            && (!chars[i - 1].is_uppercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if boundary && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

impl Store {
    /// Nodes whose name matches exactly (case-sensitive).
    pub fn nodes_named(&self, name: &str) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(NAME_NODES)?;
        let mut interned = BTreeSet::new();
        for guard in table.get(name)? {
            interned.insert(guard?.value());
        }
        drop(table);
        drop(txn);
        self.decode_ids(interned)
    }

    /// Nodes indexed under this exact lowercase token (see `node_tokens`).
    pub fn nodes_with_token(&self, word: &str) -> Result<Vec<Node>, StoreError> {
        self.decode_ids(self.token_ids([word].into_iter())?)
    }

    /// Functions whose body (not header) uses this lowercase word, capped
    /// at `limit` in id order.
    pub fn nodes_with_body_term(&self, word: &str, limit: usize) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(BODY_TERMS)?;
        let mut ids = BTreeSet::new();
        for guard in table.get(word)?.take(limit) {
            ids.insert(guard?.value());
        }
        drop(table);
        drop(txn);
        self.decode_ids(ids)
    }

    /// Every node id carrying `word` as a body term, in interned order.
    /// Cheap (no node decode): membership evidence for ranking.
    pub fn body_term_ids(&self, word: &str) -> Result<Vec<String>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(BODY_TERMS)?;
        let intern = txn.open_table(INTERN)?;
        let mut ids = Vec::new();
        for guard in table.get(word)? {
            if let Some(id) = intern.get(guard?.value())? {
                ids.push(id.value().to_string());
            }
        }
        Ok(ids)
    }

    /// Document frequency: how many nodes carry `word` as a body term.
    pub fn body_term_df(&self, word: &str) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_multimap_table(BODY_TERMS)?.get(word)?.len())
    }

    /// Recall candidates for query terms: union over each term as an exact
    /// token plus its trailing-`s` singular variant. Deduped by node id,
    /// sorted by id — deterministic. The consumer re-scores.
    pub fn candidates_for_terms(&self, terms: &[String]) -> Result<Vec<Node>, StoreError> {
        let variants = terms
            .iter()
            .map(|term| {
                let mut words = vec![term.clone()];
                if let Some(singular) = term
                    .strip_suffix('s')
                    .filter(|singular| !singular.is_empty())
                {
                    words.push(singular.to_owned());
                }
                words
            })
            .collect::<Vec<_>>();
        self.candidates_for_term_variants(&variants)
    }

    /// Recall candidates for already-normalized query-term variants.
    /// Each inner vector represents one semantic term (for example,
    /// `["parsed", "parse"]`). All variants are unioned by node id.
    pub fn candidates_for_term_variants(
        &self,
        variants: &[Vec<String>],
    ) -> Result<Vec<Node>, StoreError> {
        let words = variants.iter().flatten().map(String::as_str);
        self.decode_ids(self.token_ids(words)?)
    }

    fn token_ids<'a>(
        &self,
        words: impl Iterator<Item = &'a str>,
    ) -> Result<BTreeSet<u32>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(TOKENS_WORDS)?;
        let mut ids = BTreeSet::new();
        for word in words {
            for guard in table.get(word)? {
                ids.insert(guard?.value());
            }
        }
        Ok(ids)
    }

    /// Interned ids -> nodes, in id order (deterministic).
    fn decode_ids(&self, interned: BTreeSet<u32>) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let intern = txn.open_table(INTERN)?;
        let mut ids = Vec::new();
        for i in interned {
            if let Some(guard) = intern.get(i)? {
                ids.push(guard.value().to_string());
            }
        }
        drop(intern);
        ids.sort();
        let table = txn.open_table(NODES)?;
        let mut nodes = Vec::new();
        for id in ids {
            if let Some(guard) = table.get(id.as_str())? {
                nodes.push(postcard::from_bytes(guard.value())?);
            }
        }
        Ok(nodes)
    }

    /// Fuzzy candidates: nodes sharing the most trigrams with the query,
    /// best first, capped at `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(crate::store::TRIGRAMS)?;
        let mut hits: HashMap<u32, usize> = HashMap::new();
        let query_grams = trigrams(query);
        for gram in &query_grams {
            for guard in table.get(gram.as_str())? {
                *hits.entry(guard?.value()).or_default() += 1;
            }
        }
        drop(table);
        // Rank by shared grams, tie-broken by resolved id string for
        // deterministic output.
        let intern = txn.open_table(INTERN)?;
        let mut ranked: Vec<(String, usize)> = Vec::new();
        for (interned, shared) in hits {
            if let Some(guard) = intern.get(interned)? {
                ranked.push((guard.value().to_string(), shared));
            }
        }
        drop(intern);
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let table = txn.open_table(NODES)?;
        let mut nodes = Vec::new();
        for (id, shared) in ranked.into_iter().take(limit.max(1)) {
            // Require a majority of query trigrams to appear in the name.
            if shared * 2 >= query_grams.len()
                && let Some(guard) = table.get(id.as_str())?
            {
                nodes.push(postcard::from_bytes(guard.value())?);
            }
        }
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use sinter_core::{Node, NodeId, Span, SymbolKind};

    use super::node_tokens;

    fn node(name: &str, file: &str, signature: &str, doc: Option<&str>) -> Node {
        Node {
            id: NodeId::new(format!("{file}#{name}@0")),
            kind: SymbolKind::Function,
            name: name.to_string(),
            file: file.to_string(),
            span: Span { start: 0, end: 1 },
            signature: signature.to_string(),
            doc: doc.map(str::to_string),
        }
    }

    fn has(words: &std::collections::BTreeSet<String>, expect: &[&str]) {
        for w in expect {
            assert!(words.contains(*w), "missing {w:?} in {words:?}");
        }
    }

    #[test]
    fn camel_case_splits_and_keeps_whole_identifier() {
        let words = node_tokens(&node("PlayerCharacterV2", "a.rs", "", None));
        has(&words, &["player", "character", "v2", "playercharacterv2"]);
    }

    #[test]
    fn acronym_word_boundary() {
        let words = node_tokens(&node("HTTPServer", "a.rs", "", None));
        has(&words, &["http", "server", "httpserver"]);
    }

    #[test]
    fn snake_case_and_signature_and_doc() {
        let words = node_tokens(&node(
            "climb_state",
            "a.rs",
            "fn climb_state(input: MoveInput)",
            Some("Main traversal controller."),
        ));
        has(
            &words,
            &[
                // No "climb_state" whole token: query terms are split on
                // non-alphanumerics too, so an underscore token is unreachable.
                "climb",
                "state",
                "move",
                "input",
                "traversal",
                "controller",
                "fn",
            ],
        );
    }

    #[test]
    fn path_segments_indexed() {
        let words = node_tokens(&node("f", "src/player/ClimbComponent.test.ts", "", None));
        has(
            &words,
            &[
                "src",
                "player",
                "climb",
                "component",
                "climbcomponent",
                "test",
                "ts",
            ],
        );
    }

    #[test]
    fn short_subwords_dropped_but_short_wholes_kept_at_two_chars() {
        let words = node_tokens(&node("aB", "x.rs", "", None));
        assert!(words.contains("ab"), "{words:?}");
        assert!(!words.contains("a") && !words.contains("b"), "{words:?}");
    }
}
