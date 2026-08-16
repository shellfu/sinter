use redb::{ReadableDatabase, ReadableTableMetadata};

#[test]
#[ignore = "scratch: per-table size report for a real db (SINTER_DB env)"]
fn table_sizes() {
    let path = std::env::var("SINTER_DB").expect("set SINTER_DB");
    let db = redb::Database::open(&path).unwrap();
    let txn = db.begin_read().unwrap();
    let mut rows = Vec::new();
    for name in [
        "nodes",
        "out_edges",
        "in_edges",
        "unresolved",
        "file_facts",
        "file_hash",
        "name_refs",
        "name_nodes",
        "trigrams",
        "tokens_words",
        "imports",
    ] {
        let stats = if let Ok(t) =
            txn.open_multimap_table(redb::MultimapTableDefinition::<&str, &str>::new(name))
        {
            t.stats().ok()
        } else if let Ok(t) =
            txn.open_multimap_table(redb::MultimapTableDefinition::<&str, &[u8]>::new(name))
        {
            t.stats().ok()
        } else if let Ok(t) = txn.open_table(redb::TableDefinition::<&str, &[u8]>::new(name)) {
            t.stats().ok()
        } else if let Ok(t) = txn.open_table(redb::TableDefinition::<&str, &str>::new(name)) {
            t.stats().ok()
        } else if let Ok(t) =
            txn.open_multimap_table(redb::MultimapTableDefinition::<&str, u32>::new(name))
        {
            t.stats().ok()
        } else if let Ok(t) = txn.open_table(redb::TableDefinition::<u32, &str>::new(name)) {
            t.stats().ok()
        } else if let Ok(t) = txn.open_table(redb::TableDefinition::<&str, u32>::new(name)) {
            t.stats().ok()
        } else {
            None
        };
        if let Some(s) = stats {
            let bytes = s.stored_bytes();
            rows.push((name, bytes));
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    let total: u64 = rows.iter().map(|r| r.1).sum();
    for (name, bytes) in rows {
        println!(
            "{name:>12}: {:>8.1} MB ({:>4.1}%)",
            bytes as f64 / 1e6,
            bytes as f64 * 100.0 / total as f64
        );
    }
    println!(
        "{:>12}: {:>8.1} MB stored (file may be larger: pages/fragmentation)",
        "TOTAL",
        total as f64 / 1e6
    );
}
