//! Repository corpus policy: derived-state exclusions, durable file roles,
//! repository overrides, and query-scope selection.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use sinter_core::CorpusScope;

pub const DERIVED_ROOTS: &[&str] = &["graphify-out", "memory", ".memory"];

pub fn excluded(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or(rel);
    DERIVED_ROOTS.contains(&first)
}

/// Which persisted file roles a graph operation may return.
#[derive(Debug, Clone)]
pub struct ScopeSelection {
    scopes: BTreeSet<CorpusScope>,
}

impl ScopeSelection {
    pub fn agent_default() -> Self {
        Self {
            scopes: BTreeSet::from([CorpusScope::Production, CorpusScope::Docs]),
        }
    }

    pub fn all() -> Self {
        Self {
            scopes: CorpusScope::ALL.into_iter().collect(),
        }
    }

    pub fn parse(values: &[String], default: Self) -> Result<Self> {
        if values.is_empty() {
            return Ok(default);
        }
        if values.iter().any(|value| value == "all") {
            if values.len() != 1 {
                bail!("scope `all` cannot be combined with individual scopes");
            }
            return Ok(Self::all());
        }
        let mut scopes = BTreeSet::new();
        for value in values {
            scopes.insert(value.parse::<CorpusScope>().map_err(anyhow::Error::msg)?);
        }
        if scopes.is_empty() {
            bail!("scope selection cannot be empty");
        }
        Ok(Self { scopes })
    }

    pub fn from_json(args: &Value, default: Self) -> Result<Self> {
        let values = args
            .get("scope")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow::anyhow!("scope entries must be strings"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Self::parse(&values, default)
    }

    pub fn contains(&self, scope: CorpusScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Keep the in-scope nodes, unless none are in scope: then the
    /// out-of-scope matches are the answer (e.g. `dep:` pseudo-files).
    pub fn narrow(&self, nodes: &mut Vec<sinter_core::Node>, scopes: &sinter_store::ScopeIndex) {
        if nodes
            .iter()
            .any(|node| self.contains(scopes.scope_of(node)))
        {
            nodes.retain(|node| self.contains(scopes.scope_of(node)));
        }
    }

    pub fn labels(&self) -> Vec<&'static str> {
        self.scopes.iter().map(|scope| scope.as_str()).collect()
    }

    pub fn as_set(&self) -> BTreeSet<CorpusScope> {
        self.scopes.clone()
    }

    pub fn is_all(&self) -> bool {
        self.scopes.len() == CorpusScope::ALL.len()
    }

    pub fn json(&self) -> Value {
        json!(self.labels())
    }
}

#[derive(Default, Deserialize)]
struct RepositoryConfig {
    #[serde(default)]
    scope: ScopeConfig,
}

#[derive(Default, Deserialize)]
struct ScopeConfig {
    #[serde(default, rename = "override")]
    overrides: Vec<ScopeOverride>,
}

#[derive(Deserialize)]
struct ScopeOverride {
    pattern: String,
    scope: CorpusScope,
}

struct ScopeRule {
    scope: CorpusScope,
    matcher: Gitignore,
}

/// Conservative path rules plus ordered repository overrides from
/// `.sinter.toml`:
///
/// ```toml
/// [[scope.override]]
/// pattern = "tools/golden-production/**"
/// scope = "production"
/// ```
///
/// Patterns use gitignore syntax and later matching entries win.
pub struct ScopePolicy {
    rules: Vec<ScopeRule>,
}

impl ScopePolicy {
    pub fn load(repo: &Path) -> Result<Self> {
        let path = repo.join(".sinter.toml");
        if !path.exists() {
            return Ok(Self { rules: Vec::new() });
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let config: RepositoryConfig =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let mut rules = Vec::with_capacity(config.scope.overrides.len());
        for entry in config.scope.overrides {
            if entry.pattern.starts_with('!') {
                bail!(
                    "{}: scope override patterns cannot start with `!`; ordered overrides already provide precedence",
                    path.display()
                );
            }
            let mut builder = GitignoreBuilder::new(repo);
            builder
                .add_line(Some(path.clone()), &entry.pattern)
                .with_context(|| format!("scope override pattern {:?}", entry.pattern))?;
            rules.push(ScopeRule {
                scope: entry.scope,
                matcher: builder.build()?,
            });
        }
        Ok(Self { rules })
    }

    pub fn classify(&self, rel: &str) -> CorpusScope {
        let mut scope = CorpusScope::classify_path(rel);
        for rule in &self.rules {
            if rule
                .matcher
                .matched_path_or_any_parents(Path::new(rel), false)
                .is_ignore()
            {
                scope = rule.scope;
            }
        }
        scope
    }
}

#[cfg(test)]
mod tests {
    use sinter_core::CorpusScope;

    #[test]
    fn only_top_level_derived_roots_are_excluded() {
        assert!(super::excluded("graphify-out/graph.json"));
        assert!(super::excluded("memory/notes.md"));
        assert!(!super::excluded("src/memory/store.rs"));
    }

    #[test]
    fn ordered_repository_overrides_replace_path_defaults() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join(".sinter.toml"),
            r#"
[[scope.override]]
pattern = "harness/golden/**"
scope = "production"

[[scope.override]]
pattern = "harness/golden/vendor-case/**"
scope = "fixture"
"#,
        )
        .unwrap();
        let policy = super::ScopePolicy::load(repo.path()).unwrap();
        assert_eq!(
            policy.classify("harness/golden/check.rs"),
            CorpusScope::Production
        );
        assert_eq!(
            policy.classify("harness/golden/vendor-case/check.rs"),
            CorpusScope::Fixture
        );
    }
}
