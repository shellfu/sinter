//! Deterministic identity of one committed graph snapshot.

use redb::{ReadableDatabase, ReadableMultimapTable, ReadableTable};

use crate::error::StoreError;
use crate::store::{FILE_HASH, FILE_SCOPE, FileStamp, META, NODES, OUT_EDGES, RESOLVE_META, Store};

impl Store {
    /// Stable token for the committed graph snapshot served by this store.
    ///
    /// Normal repository stores hash the schema, source content hashes, and
    /// non-source resolution fingerprints. That makes harmless reads and stat
    /// changes stable while any indexed source or compiler-input change moves
    /// the token. Hand-built stores used by tests/export have no file hashes,
    /// so they fall back to hashing their node and edge rows.
    pub fn snapshot_token(&self) -> Result<String, StoreError> {
        let txn = self.db.begin_read()?;
        let mut fingerprint = SnapshotFingerprint::new();
        let schema = match txn.open_table(META) {
            Ok(table) => table.get("schema")?.map(|guard| guard.value()).unwrap_or(0),
            Err(redb::TableError::TableDoesNotExist(_)) => 0,
            Err(error) => return Err(error.into()),
        };
        fingerprint.field(b"schema");
        fingerprint.field(&schema.to_le_bytes());

        let hashes = txn.open_table(FILE_HASH)?;
        let mut file_count = 0usize;
        for entry in hashes.iter()? {
            let (file, stamp) = entry?;
            fingerprint.field(file.value().as_bytes());
            fingerprint.field(FileStamp::decode(stamp.value()).hash.as_bytes());
            file_count += 1;
        }
        drop(hashes);

        let scopes = txn.open_table(FILE_SCOPE)?;
        for entry in scopes.iter()? {
            let (file, scope) = entry?;
            fingerprint.field(file.value().as_bytes());
            fingerprint.field(scope.value().as_bytes());
        }
        drop(scopes);

        let resolution = txn.open_table(RESOLVE_META)?;
        for entry in resolution.iter()? {
            let (kind, value) = entry?;
            fingerprint.field(kind.value().as_bytes());
            fingerprint.field(value.value().as_bytes());
        }
        drop(resolution);

        if file_count == 0 {
            let nodes = txn.open_table(NODES)?;
            for entry in nodes.iter()? {
                let (id, bytes) = entry?;
                fingerprint.field(id.value().as_bytes());
                fingerprint.field(bytes.value());
            }
            drop(nodes);
            let edges = txn.open_multimap_table(OUT_EDGES)?;
            for entry in edges.iter()? {
                let (id, values) = entry?;
                fingerprint.field(id.value().as_bytes());
                for value in values {
                    fingerprint.field(value?.value());
                }
            }
        }

        Ok(format!(
            "graph-v{schema}-{:016x}{:016x}",
            fingerprint.left, fingerprint.right
        ))
    }
}

/// Two independent FNV-1a streams are sufficient for a deterministic
/// precondition token without adding a hashing dependency to the store.
/// Length framing prevents concatenation ambiguity; this is an identity
/// checksum, not an adversarial cryptographic commitment.
struct SnapshotFingerprint {
    left: u64,
    right: u64,
}

impl SnapshotFingerprint {
    fn new() -> Self {
        Self {
            left: 0xcbf29ce484222325,
            right: 0x6c62272e07bb0142,
        }
    }

    fn field(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.left ^= u64::from(*byte);
            self.left = self.left.wrapping_mul(0x0000_0100_0000_01b3);
            self.right ^= u64::from(*byte).rotate_left(1);
            self.right = self.right.wrapping_mul(0x9e37_79b1_85eb_ca87);
        }
    }
}
