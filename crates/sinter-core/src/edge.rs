use serde::{Deserialize, Serialize};

use crate::node::{NodeId, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Relation {
    Calls,
    Uses,
    Imports,
    Contains,
    Implements,
    Extends,
    /// The source reads rows from the destination relation.
    Reads,
    /// The source can insert, update, or delete rows in the destination table.
    Writes,
    /// The source declares creation of the destination database object.
    Creates,
    /// The source changes the destination database object.
    Alters,
    /// The source declares removal of the destination database object.
    Drops,
}

impl Relation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Uses => "uses",
            Self::Imports => "imports",
            Self::Contains => "contains",
            Self::Implements => "implements",
            Self::Extends => "extends",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Creates => "creates",
            Self::Alters => "alters",
            Self::Drops => "drops",
        }
    }
}

/// What binds this edge to its target (R2: evidence or nothing).
/// Global name uniqueness is not evidence and has no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Evidence {
    /// Syntactic containment seen directly in the parse tree.
    Structural,
    /// Name visible in the reference's own file scope.
    Scope,
    /// An import statement binds the reference's path to the target.
    Import,
    /// A compiler-produced SCIP index binds reference to definition.
    Scip,
    /// An operator-declared binding from a workspace manifest (runtime
    /// coupling like queue topics/HTTP routes that no static analysis can
    /// see). Auditable in the manifest; never inferred.
    Declared,
    /// Dynamic-dispatch fan-out: a trait/interface method is assumed to
    /// reach every implementation in the corpus. Conservative
    /// over-approximation, deliberately excludable (`--certain`,
    /// `--evidence` without "dynamic").
    Dynamic,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Scope => "scope",
            Self::Import => "import",
            Self::Scip => "scip",
            Self::Declared => "declared",
            Self::Dynamic => "dynamic",
        }
    }

    /// Compiler-grade evidence is certain; heuristic-free but indirect
    /// evidence (scope/import matching) is inferred.
    pub fn confidence(self) -> Confidence {
        match self {
            Self::Structural | Self::Scip | Self::Declared => Confidence::Certain,
            Self::Scope | Self::Import | Self::Dynamic => Confidence::Inferred,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Confidence {
    Certain,
    Inferred,
}

/// Directed edge `src -> dst`. The graph is a multigraph: parallel edges
/// that differ in relation, evidence, or confidence coexist.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
    pub relation: Relation,
    pub evidence: Evidence,
    pub confidence: Confidence,
    /// Byte span of the binding reference in the src node's file (the file
    /// is derivable from `src`). None when no single site exists:
    /// containment, dynamic fan-out, implements/extends pairing, declared
    /// links. When several sites bind the same (src, dst, relation,
    /// evidence), this is the representative (smallest) one — the field is
    /// after identity so identity orders first.
    pub site: Option<Span>,
    /// Further sites binding the same identity, ascending, capped so that
    /// `1 + extra_sites.len() <= MAX_SITES`. Empty for a single-site edge.
    pub extra_sites: Vec<Span>,
    /// Distinct sites observed for this identity, including the ones the
    /// cap dropped. 0 when the edge has no site at all.
    pub sites_total: u32,
}

/// Sites kept per edge. A hub edge can be called dozens of times; the
/// answer stays bounded ("3 of 12 shown") instead of growing with fan-in.
pub const MAX_SITES: usize = 8;

impl Edge {
    /// Identity without the site: two edges equal here are the same
    /// dependency fact observed at (possibly) different call sites.
    /// One edge with a single site: the shape extraction and resolution
    /// produce, before storage merges same-identity sites together.
    pub fn single(
        src: NodeId,
        dst: NodeId,
        relation: Relation,
        evidence: Evidence,
        confidence: Confidence,
        site: Option<Span>,
    ) -> Self {
        Self {
            src,
            dst,
            relation,
            evidence,
            confidence,
            site,
            extra_sites: Vec::new(),
            sites_total: u32::from(site.is_some()),
        }
    }

    /// Every kept site, ascending (representative first). Empty when the
    /// edge has none.
    pub fn sites(&self) -> impl Iterator<Item = Span> + '_ {
        self.site
            .into_iter()
            .chain(self.extra_sites.iter().copied())
    }

    /// Sites this edge has beyond the ones it kept.
    pub fn sites_omitted(&self) -> u32 {
        self.sites_total.saturating_sub(self.sites().count() as u32)
    }

    pub fn identity(&self) -> (&NodeId, &NodeId, Relation, Evidence, Confidence) {
        (
            &self.src,
            &self.dst,
            self.relation,
            self.evidence,
            self.confidence,
        )
    }
}
