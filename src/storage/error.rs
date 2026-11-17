use thiserror::Error;

use crate::graph::GraphError;
use crate::model::EdgeError;
use crate::persistence::CodecError;

/// Errors raised by the persistent storage layer.
///
/// The variants separate failures from the `redb` backend, record decoding, and
/// graph-level integrity so callers can decide whether a failure is retryable,
/// fatal, or the result of corrupt on-disk data.
///
/// Backend errors are boxed because `redb`'s transaction error is large enough
/// to make every `Result<_, StorageError>` expensive to return on the happy
/// path; the indirection is paid only when an error is actually constructed.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("failed to open database: {0}")]
    Open(#[source] Box<redb::DatabaseError>),

    #[error("failed to start or use a transaction: {0}")]
    Transaction(#[source] Box<redb::TransactionError>),

    #[error("failed to open or manage a table: {0}")]
    Table(#[source] Box<redb::TableError>),

    #[error("failed to commit a transaction: {0}")]
    Commit(#[source] Box<redb::CommitError>),

    #[error("storage backend operation failed: {0}")]
    Backend(#[source] Box<redb::StorageError>),

    #[error("database compaction failed: {0}")]
    Compaction(#[source] Box<redb::CompactionError>),

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error(transparent)]
    Edge(#[from] EdgeError),

    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "database schema version {found} is newer than the supported version {supported}; upgrade kg"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("no migration is registered from database schema version {from}")]
    MissingMigration { from: u32 },

    #[error("metadata '{key}' is corrupt: {reason}")]
    CorruptMetadata { key: String, reason: String },

    #[error("edge '{source_uri}' -> '{target_uri}' references a node that is not stored")]
    DanglingEdge {
        source_uri: crate::model::SymbolUri,
        target_uri: crate::model::SymbolUri,
    },

    #[error("storage integrity violation: {0}")]
    Integrity(String),
}

impl From<redb::DatabaseError> for StorageError {
    fn from(error: redb::DatabaseError) -> Self {
        Self::Open(Box::new(error))
    }
}

impl From<redb::TransactionError> for StorageError {
    fn from(error: redb::TransactionError) -> Self {
        Self::Transaction(Box::new(error))
    }
}

impl From<redb::TableError> for StorageError {
    fn from(error: redb::TableError) -> Self {
        Self::Table(Box::new(error))
    }
}

impl From<redb::CommitError> for StorageError {
    fn from(error: redb::CommitError) -> Self {
        Self::Commit(Box::new(error))
    }
}

impl From<redb::StorageError> for StorageError {
    fn from(error: redb::StorageError) -> Self {
        Self::Backend(Box::new(error))
    }
}

impl From<redb::CompactionError> for StorageError {
    fn from(error: redb::CompactionError) -> Self {
        Self::Compaction(Box::new(error))
    }
}
