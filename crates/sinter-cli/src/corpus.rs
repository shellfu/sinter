//! Repository corpus policy shared by source extraction, SCIP freshness,
//! and coverage reporting. These directories are products of analysis
//! tools, so indexing them feeds Sinter's own output back into results.

pub const DERIVED_ROOTS: &[&str] = &["graphify-out", "memory", ".memory"];

pub fn excluded(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or(rel);
    DERIVED_ROOTS.contains(&first)
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_top_level_derived_roots_are_excluded() {
        assert!(super::excluded("graphify-out/graph.json"));
        assert!(super::excluded("memory/notes.md"));
        assert!(!super::excluded("src/memory/store.rs"));
    }
}
