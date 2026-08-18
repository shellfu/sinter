use sinter_core::GraphError;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("codec error: {0}")]
    Codec(#[from] postcard::Error),
    #[error("stored graph violates an invariant: {0}")]
    Invariant(#[from] GraphError),
    #[error("cannot reset outdated database: {0}")]
    Reset(std::io::Error),
    #[error(
        "graph was built by a newer sinter (schema v{stored}, this binary writes v{supported}) — upgrade sinter, or delete .sinter/graph.redb to rebuild at v{supported}"
    )]
    NewerSchema { stored: u32, supported: u32 },
    #[error("facts compression error: {0}")]
    Compress(std::io::Error),
    #[error("compaction error: {0}")]
    Compaction(#[from] redb::CompactionError),
}
