//! Persistent graph storage on top of `redb`.
//!
//! [`GraphStore`] owns the database and exposes atomic batched writes, snapshot
//! reads, file-index lookup, metadata helpers, compaction, and integrity
//! verification. It deliberately does not hold a [`crate::graph::MemoryGraph`];
//! callers load a snapshot, mutate it, and persist a batch.
//!
//! ```no_run
//! use kg::storage::GraphStore;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = GraphStore::open(".rusty/graph.redb")?;
//! let graph = store.load_graph()?;
//! store.save_graph(&graph)?;
//! # Ok(())
//! # }
//! ```

mod db;
mod error;
pub mod schema;
mod transaction;

pub use db::{GraphStore, StoreReport};
pub use error::StorageError;
pub use schema::{
    FileIndex, META_LAST_COMMIT, META_REPO_NAME, META_SCHEMA_VERSION, STORAGE_SCHEMA_VERSION,
    edge_key,
};
pub use transaction::{WriteBatch, WriteReport};
