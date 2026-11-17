//! Batched, atomic graph writes.
//!
//! A [`WriteBatch`] collects all node, edge, removal, and metadata mutations
//! first; [`crate::storage::GraphStore::write_batch`] then applies the whole
//! batch inside one `redb` write transaction with immediate durability. A
//! failure aborts the transaction, so the store never contains a partially
//! written batch.
//!
//! The batch is keyed by canonical identity, not insertion order:
//!
//! - nodes by [`crate::model::SymbolUri`];
//! - edges by the NUL-separated storage key from
//!   [`crate::storage::schema::edge_key`];
//! - removals and metadata by sorted keys.
//!
//! Repeating a batch is therefore idempotent: the same table keys are written
//! with the same values.

use std::collections::{BTreeMap, BTreeSet};

use redb::{Database, Durability, ReadableTable};

use crate::model::{CodeEdge, CodeEntityNode, SymbolUri};

use super::error::StorageError;
use super::schema::{
    self, FileIndex, TABLE_EDGES, TABLE_FILE_INDEX, TABLE_META, TABLE_NODES, edge_key,
};

/// A set of graph mutations committed as one atomic transaction.
#[derive(Debug, Clone, Default)]
pub struct WriteBatch {
    nodes: BTreeMap<SymbolUri, CodeEntityNode>,
    edges: BTreeMap<String, CodeEdge>,
    removed_nodes: BTreeSet<SymbolUri>,
    removed_files: BTreeSet<String>,
    meta: BTreeMap<String, String>,
}

impl WriteBatch {
    /// Create an empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a node. A pending removal of the same URI is cancelled.
    pub fn put_node(&mut self, node: CodeEntityNode) {
        self.removed_nodes.remove(node.uri());
        self.nodes.insert(node.uri().clone(), node);
    }

    /// Insert or replace an edge. The key is deterministic, so the last value
    /// for the same `(source, target, kind)` wins.
    pub fn put_edge(&mut self, edge: CodeEdge) {
        let key = edge_key(edge.source_uri(), edge.target_uri(), edge.kind());
        self.edges.insert(key, edge);
    }

    /// Remove one node. A pending insert of the same URI is discarded.
    pub fn remove_node(&mut self, uri: &SymbolUri) {
        self.nodes.remove(uri);
        self.removed_nodes.insert(uri.clone());
    }

    /// Remove every node belonging to a file. The file index is used during
    /// commit to discover the URIs; incident edges are removed as well.
    ///
    /// Removals are applied before inserts regardless of builder call order, so
    /// a node staged with [`Self::put_node`] survives a `remove_file` for the
    /// same path and is written as part of the replacement.
    pub fn remove_file(&mut self, file_path: impl Into<String>) {
        self.removed_files.insert(file_path.into());
    }

    /// Set a metadata key to a UTF-8 value.
    pub fn set_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.meta.insert(key.into(), value.into());
    }

    /// Number of pending node writes.
    pub fn node_writes(&self) -> usize {
        self.nodes.len()
    }

    /// Number of pending edge writes.
    pub fn edge_writes(&self) -> usize {
        self.edges.len()
    }

    /// Number of explicitly removed nodes.
    pub fn node_removals(&self) -> usize {
        self.removed_nodes.len()
    }

    /// Number of files marked for removal.
    pub fn file_removals(&self) -> usize {
        self.removed_files.len()
    }

    /// True when the batch would not change the store.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.edges.is_empty()
            && self.removed_nodes.is_empty()
            && self.removed_files.is_empty()
            && self.meta.is_empty()
    }
}

/// Counts describing the effect of one committed batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteReport {
    /// Node records encoded and written, including replacements.
    pub nodes_written: usize,
    /// Edge records encoded and written, including replacements.
    pub edges_written: usize,
    /// Existing node records deleted by explicit or file removal.
    pub nodes_removed: usize,
    /// Existing edge records deleted because an endpoint was removed.
    pub edges_removed: usize,
    /// File indexes written or rewritten by the batch.
    pub files_indexed: usize,
}

/// Apply a batch in one immediate-durability transaction.
pub(crate) fn apply(db: &Database, batch: &WriteBatch) -> Result<WriteReport, StorageError> {
    let mut transaction = db.begin_write()?;
    transaction.set_durability(Durability::Immediate);

    let mut report = WriteReport::default();

    // Resolve removed files to their indexed nodes before deleting anything.
    let mut removed_uris = batch.removed_nodes.clone();
    {
        let file_table = transaction.open_table(TABLE_FILE_INDEX)?;
        for file_path in &batch.removed_files {
            if let Some(value) = file_table.get(file_path.as_str())? {
                let index = schema::decode_file_index(value.value())?;
                removed_uris.extend(index.nodes().iter().cloned());
            }
        }
    }

    // Delete node records first so any failure leaves the transaction uncommitted.
    {
        let mut node_table = transaction.open_table(TABLE_NODES)?;
        for uri in &removed_uris {
            if node_table.remove(uri.as_str())?.is_some() {
                report.nodes_removed += 1;
            }
        }
    }

    // Every file touched by a removal or insertion needs its index rebuilt.
    let mut affected_files = batch.removed_files.clone();
    for uri in &removed_uris {
        if let Ok(parsed) = uri.parse() {
            affected_files.insert(parsed.file_path);
        }
    }
    for node in batch.nodes.values() {
        affected_files.insert(node.file_path().to_string());
    }

    {
        let mut node_table = transaction.open_table(TABLE_NODES)?;
        for node in batch.nodes.values() {
            let bytes = schema::encode_node(node)?;
            node_table.insert(node.uri().as_str(), bytes.as_slice())?;
            report.nodes_written += 1;
        }
    }

    // Remove edges incident to deleted nodes. Edge keys are not ordered by
    // target, so this is a scan; batches that remove files are expected to be
    // rare and the scan is still bounded by the store size.
    if !removed_uris.is_empty() {
        let mut edge_table = transaction.open_table(TABLE_EDGES)?;
        let mut stale = Vec::new();
        for entry in edge_table.iter()? {
            let (key, value) = entry?;
            let edge = schema::decode_edge(value.value())?;
            if removed_uris.contains(edge.source_uri()) || removed_uris.contains(edge.target_uri())
            {
                stale.push(key.value().to_string());
            }
        }

        for key in stale {
            if edge_table.remove(key.as_str())?.is_some() {
                report.edges_removed += 1;
            }
        }
    }

    // Validate endpoints against the post-write node set before inserting.
    {
        let node_table = transaction.open_table(TABLE_NODES)?;
        let mut edge_table = transaction.open_table(TABLE_EDGES)?;

        for (key, edge) in &batch.edges {
            if node_table.get(edge.source_uri().as_str())?.is_none() {
                return Err(StorageError::DanglingEdge {
                    source_uri: edge.source_uri().clone(),
                    target_uri: edge.target_uri().clone(),
                });
            }
            if node_table.get(edge.target_uri().as_str())?.is_none() {
                return Err(StorageError::DanglingEdge {
                    source_uri: edge.source_uri().clone(),
                    target_uri: edge.target_uri().clone(),
                });
            }

            let bytes = schema::encode_edge(edge)?;
            edge_table.insert(key.as_str(), bytes.as_slice())?;
            report.edges_written += 1;
        }
    }

    // Rebuild affected file indexes from the surviving nodes plus the batch.
    {
        let mut file_table = transaction.open_table(TABLE_FILE_INDEX)?;
        for file_path in &affected_files {
            let mut nodes: BTreeSet<SymbolUri> = if batch.removed_files.contains(file_path) {
                BTreeSet::new()
            } else {
                match file_table.get(file_path.as_str())? {
                    Some(value) => schema::decode_file_index(value.value())?
                        .nodes()
                        .iter()
                        .cloned()
                        .collect(),
                    None => BTreeSet::new(),
                }
            };

            nodes.retain(|uri| !removed_uris.contains(uri));
            nodes.extend(
                batch
                    .nodes
                    .values()
                    .filter(|node| node.file_path() == file_path)
                    .map(|node| node.uri().clone()),
            );

            if nodes.is_empty() {
                file_table.remove(file_path.as_str())?;
            } else {
                let index = FileIndex::new(file_path.clone(), nodes.into_iter().collect());
                let bytes = schema::encode_file_index(&index)?;
                file_table.insert(file_path.as_str(), bytes.as_slice())?;
                report.files_indexed += 1;
            }
        }
    }

    {
        let mut meta_table = transaction.open_table(TABLE_META)?;
        for (key, value) in &batch.meta {
            meta_table.insert(key.as_str(), value.as_bytes())?;
        }
    }

    transaction.commit()?;
    Ok(report)
}
