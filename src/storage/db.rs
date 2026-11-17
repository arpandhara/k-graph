//! `redb`-backed persistent graph storage.
//!
//! [`GraphStore`] owns one redb [`Database`] with four tables defined in
//! [`super::schema`]. Writes always flow through a [`WriteBatch`] and commit in
//! one transaction with immediate durability; reads always use a redb read
//! transaction, which provides a consistent snapshot of nodes and edges.
//!
//! The store never decodes a record without the codec's size, version, and
//! validation checks, and it never writes an edge whose endpoints are absent
//! from the post-write node set.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use redb::{Database, ReadTransaction, ReadableTable, ReadableTableMetadata, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::graph::MemoryGraph;
use crate::model::{CodeEdge, CodeEntityNode, EdgeKind, SymbolUri};

use super::error::StorageError;
use super::schema::{
    self, FileIndex, META_LAST_COMMIT, META_REPO_NAME, META_SCHEMA_VERSION, STORAGE_SCHEMA_VERSION,
    TABLE_EDGES, TABLE_FILE_INDEX, TABLE_META, TABLE_NODES,
};
use super::transaction::{WriteBatch, WriteReport};

/// Summary returned by [`GraphStore::verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StoreReport {
    /// Number of node records in the store.
    pub nodes: usize,
    /// Number of edge records in the store.
    pub edges: usize,
    /// Number of distinct file index records.
    pub files: usize,
}

/// A persistent code knowledge graph.
///
/// The in-memory graph is not held open by the store. Callers load with
/// [`GraphStore::load_graph`] or [`GraphStore::load_all`], mutate in memory, and
/// persist with [`GraphStore::write_batch`] or [`GraphStore::save_batch`].
/// Because redb permits only one writer at a time, concurrent callers serialize
/// on [`GraphStore::write_batch`] without corrupting the store.
pub struct GraphStore {
    db: Database,
}

impl fmt::Debug for GraphStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphStore")
            .field("schema_version", &STORAGE_SCHEMA_VERSION)
            .finish_non_exhaustive()
    }
}

impl GraphStore {
    /// Open an existing store or create a new one at `path`.
    ///
    /// The parent directory is created when missing. Tables are opened inside a
    /// write transaction, which creates them if needed, and the physical schema
    /// version is stamped into `TABLE_META`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let db = Database::create(path)?;
        let transaction = db.begin_write()?;

        // Opening every table creates it if missing. redb creates tables lazily,
        // so an empty store would otherwise have no edge/file tables to read.
        {
            let _ = transaction.open_table(TABLE_NODES)?;
            let _ = transaction.open_table(TABLE_EDGES)?;
            let _ = transaction.open_table(TABLE_FILE_INDEX)?;
            let _ = transaction.open_table(TABLE_META)?;
        }

        let has_nodes = {
            let nodes = transaction.open_table(TABLE_NODES)?;
            nodes.len()? > 0
        };

        let stored_version = {
            let meta = transaction.open_table(TABLE_META)?;
            read_stored_version(&meta)?
        };

        match stored_version {
            None if has_nodes => {
                return Err(StorageError::CorruptMetadata {
                    key: META_SCHEMA_VERSION.to_string(),
                    reason: "nodes exist but no schema version is recorded".to_string(),
                });
            }
            None => {}
            Some(found) if found > STORAGE_SCHEMA_VERSION => {
                return Err(StorageError::SchemaTooNew {
                    found,
                    supported: STORAGE_SCHEMA_VERSION,
                });
            }
            Some(found) if found < STORAGE_SCHEMA_VERSION => {
                migrate(&transaction, found)?;
            }
            Some(_) => {}
        }

        {
            let mut meta = transaction.open_table(TABLE_META)?;
            meta.insert(
                META_SCHEMA_VERSION,
                STORAGE_SCHEMA_VERSION.to_string().as_bytes(),
            )?;
        }

        transaction.commit()?;
        Ok(Self { db })
    }

    /// Read the physical schema version recorded in the store.
    pub fn schema_version(&self) -> Result<u32, StorageError> {
        let raw =
            self.get_meta(META_SCHEMA_VERSION)?
                .ok_or_else(|| StorageError::CorruptMetadata {
                    key: META_SCHEMA_VERSION.to_string(),
                    reason: "metadata key is missing".to_string(),
                })?;
        raw.parse::<u32>()
            .map_err(|error| StorageError::CorruptMetadata {
                key: META_SCHEMA_VERSION.to_string(),
                reason: error.to_string(),
            })
    }

    /// Persist a collection of nodes and edges as one atomic batch.
    ///
    /// The file index is maintained automatically. Edges whose endpoints are
    /// not present after the batch are rejected and the whole batch is aborted.
    pub fn save_batch(
        &self,
        nodes: &[CodeEntityNode],
        edges: &[CodeEdge],
    ) -> Result<WriteReport, StorageError> {
        let mut batch = WriteBatch::new();
        for node in nodes {
            batch.put_node(node.clone());
        }
        for edge in edges {
            batch.put_edge(edge.clone());
        }
        self.write_batch(&batch)
    }

    /// Persist every node and edge currently in a [`MemoryGraph`].
    ///
    /// This is an upsert, not a replacement: rows that are absent from the
    /// graph remain in the store. Use [`WriteBatch::remove_file`] or
    /// [`WriteBatch::remove_node`] to delete records.
    pub fn save_graph(&self, graph: &MemoryGraph) -> Result<WriteReport, StorageError> {
        let mut batch = WriteBatch::new();
        for node in graph.graph().node_weights() {
            batch.put_node(node.clone());
        }

        for edge_index in graph.graph().edge_indices() {
            let (source_index, target_index) =
                graph.graph().edge_endpoints(edge_index).ok_or_else(|| {
                    StorageError::Integrity(format!("graph edge {edge_index:?} has no endpoints"))
                })?;
            let source = graph.get_node(source_index).ok_or_else(|| {
                StorageError::Integrity(format!(
                    "graph edge {edge_index:?} has a missing source node"
                ))
            })?;
            let target = graph.get_node(target_index).ok_or_else(|| {
                StorageError::Integrity(format!(
                    "graph edge {edge_index:?} has a missing target node"
                ))
            })?;
            let weight = graph.get_edge(edge_index).ok_or_else(|| {
                StorageError::Integrity(format!("graph edge {edge_index:?} has no weight"))
            })?;

            let mut edge = CodeEdge::new(source.uri().clone(), target.uri().clone(), weight.kind())
                .with_confidence(weight.confidence().value())?;
            if let Some(span) = weight.call_site() {
                edge = edge.with_call_site(span);
            }
            batch.put_edge(edge);
        }

        self.write_batch(&batch)
    }

    /// Apply a [`WriteBatch`] atomically.
    ///
    /// Removals are resolved and applied before inserts. Edge endpoints are
    /// checked against the post-write node set, and any failure aborts the whole
    /// batch.
    pub fn write_batch(&self, batch: &WriteBatch) -> Result<WriteReport, StorageError> {
        super::transaction::apply(&self.db, batch)
    }

    /// Load all nodes and edges from one consistent snapshot.
    pub fn load_all(&self) -> Result<(Vec<CodeEntityNode>, Vec<CodeEdge>), StorageError> {
        let transaction = self.db.begin_read()?;
        Ok((read_nodes(&transaction)?, read_edges(&transaction)?))
    }

    /// Load a fresh [`MemoryGraph`] from one consistent snapshot.
    pub fn load_graph(&self) -> Result<MemoryGraph, StorageError> {
        let transaction = self.db.begin_read()?;
        let nodes = read_nodes(&transaction)?;
        let edges = read_edges(&transaction)?;

        let mut graph = MemoryGraph::new();
        for node in nodes {
            graph.add_node(node)?;
        }
        for edge in edges {
            graph.add_edge(edge)?;
        }
        Ok(graph)
    }

    /// Read one node by canonical URI.
    pub fn get_node(&self, uri: &SymbolUri) -> Result<Option<CodeEntityNode>, StorageError> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(TABLE_NODES)?;
        match table.get(uri.as_str())? {
            Some(value) => Ok(Some(schema::decode_node(value.value())?)),
            None => Ok(None),
        }
    }

    /// Read one edge by its identity.
    pub fn get_edge(
        &self,
        source: &SymbolUri,
        target: &SymbolUri,
        kind: EdgeKind,
    ) -> Result<Option<CodeEdge>, StorageError> {
        let key = schema::edge_key(source, target, kind);
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(TABLE_EDGES)?;
        match table.get(key.as_str())? {
            Some(value) => Ok(Some(schema::decode_edge(value.value())?)),
            None => Ok(None),
        }
    }

    /// Read the file index for one file.
    pub fn file_index(&self, file_path: &str) -> Result<Option<FileIndex>, StorageError> {
        let transaction = self.db.begin_read()?;
        read_file_index(&transaction, file_path)
    }

    /// Read all nodes for one file, ordered by canonical URI.
    pub fn nodes_for_file(&self, file_path: &str) -> Result<Vec<CodeEntityNode>, StorageError> {
        let transaction = self.db.begin_read()?;
        let Some(index) = read_file_index(&transaction, file_path)? else {
            return Ok(Vec::new());
        };

        let table = transaction.open_table(TABLE_NODES)?;
        let mut nodes = Vec::with_capacity(index.len());
        for uri in index.nodes() {
            match table.get(uri.as_str())? {
                Some(value) => nodes.push(schema::decode_node(value.value())?),
                None => {
                    return Err(StorageError::Integrity(format!(
                        "file index for '{file_path}' references missing node '{uri}'"
                    )));
                }
            }
        }
        Ok(nodes)
    }

    /// Read the node URIs indexed for one file.
    pub fn node_uris_for_file(&self, file_path: &str) -> Result<Vec<SymbolUri>, StorageError> {
        let transaction = self.db.begin_read()?;
        Ok(read_file_index(&transaction, file_path)?
            .map(|index| index.nodes().to_vec())
            .unwrap_or_default())
    }

    /// Number of persisted nodes.
    pub fn node_count(&self) -> Result<usize, StorageError> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(TABLE_NODES)?;
        Ok(table.len()? as usize)
    }

    /// Number of persisted edges.
    pub fn edge_count(&self) -> Result<usize, StorageError> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(TABLE_EDGES)?;
        Ok(table.len()? as usize)
    }

    /// Read a UTF-8 metadata value.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, StorageError> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(TABLE_META)?;
        match table.get(key)? {
            Some(value) => std::str::from_utf8(value.value())
                .map(|text| Some(text.to_string()))
                .map_err(|error| StorageError::CorruptMetadata {
                    key: key.to_string(),
                    reason: error.to_string(),
                }),
            None => Ok(None),
        }
    }

    /// Set a UTF-8 metadata value in its own immediate transaction.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let mut transaction = self.db.begin_write()?;
        transaction.set_durability(redb::Durability::Immediate);
        {
            let mut table = transaction.open_table(TABLE_META)?;
            table.insert(key, value.as_bytes())?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Record the last indexed VCS commit (used by Phase 5).
    pub fn set_last_commit(&self, commit: &str) -> Result<(), StorageError> {
        self.set_meta(META_LAST_COMMIT, commit)
    }

    /// Read the last indexed VCS commit, if one has been recorded.
    pub fn last_commit(&self) -> Result<Option<String>, StorageError> {
        self.get_meta(META_LAST_COMMIT)
    }

    /// Record the repository name associated with this store.
    pub fn set_repo_name(&self, name: &str) -> Result<(), StorageError> {
        self.set_meta(META_REPO_NAME, name)
    }

    /// Read the repository name associated with this store.
    pub fn repo_name(&self) -> Result<Option<String>, StorageError> {
        self.get_meta(META_REPO_NAME)
    }

    /// Compact the underlying database file.
    ///
    /// Returns `true` when compaction changed the file. Compaction requires an
    /// exclusive `&mut self` because redb rewrites the backing file.
    pub fn compact(&mut self) -> Result<bool, StorageError> {
        self.db.compact().map_err(StorageError::from)
    }

    /// Validate table contents and cross-table references.
    ///
    /// Checks that every record decodes, every primary key matches its record
    /// identity, every edge endpoint exists, every node appears in exactly one
    /// file index, and every index entry points back to a node in the same file.
    pub fn verify(&self) -> Result<StoreReport, StorageError> {
        let transaction = self.db.begin_read()?;

        let node_table = transaction.open_table(TABLE_NODES)?;
        let mut node_files = BTreeMap::new();
        let mut node_count = 0usize;
        for entry in node_table.iter()? {
            let (key, value) = entry?;
            let node = schema::decode_node(value.value())?;
            if key.value() != node.uri().as_str() {
                return Err(StorageError::Integrity(format!(
                    "node key '{}' does not match record URI '{}'",
                    key.value(),
                    node.uri()
                )));
            }
            if node_files
                .insert(node.uri().clone(), node.file_path().to_string())
                .is_some()
            {
                return Err(StorageError::Integrity(format!(
                    "node '{}' is stored more than once",
                    node.uri()
                )));
            }
            node_count += 1;
        }

        let edge_table = transaction.open_table(TABLE_EDGES)?;
        let mut edge_count = 0usize;
        for entry in edge_table.iter()? {
            let (key, value) = entry?;
            let edge = schema::decode_edge(value.value())?;

            let expected = schema::edge_key(edge.source_uri(), edge.target_uri(), edge.kind());
            if key.value() != expected {
                return Err(StorageError::Integrity(format!(
                    "edge key '{}' does not match its stored identity",
                    key.value()
                )));
            }

            if !node_files.contains_key(edge.source_uri()) {
                return Err(StorageError::Integrity(format!(
                    "edge source '{}' is not stored",
                    edge.source_uri()
                )));
            }
            if !node_files.contains_key(edge.target_uri()) {
                return Err(StorageError::Integrity(format!(
                    "edge target '{}' is not stored",
                    edge.target_uri()
                )));
            }
            edge_count += 1;
        }

        let file_table = transaction.open_table(TABLE_FILE_INDEX)?;
        let mut indexed = BTreeSet::new();
        let mut files = 0usize;

        for entry in file_table.iter()? {
            let (key, value) = entry?;
            let index = schema::decode_file_index(value.value())?;
            if index.file_path() != key.value() {
                return Err(StorageError::Integrity(format!(
                    "file index key '{}' does not match record path '{}'",
                    key.value(),
                    index.file_path()
                )));
            }

            files += 1;
            for uri in index.nodes() {
                if !indexed.insert(uri.clone()) {
                    return Err(StorageError::Integrity(format!(
                        "node '{uri}' appears in more than one file index"
                    )));
                }
                match node_files.get(uri) {
                    Some(file_path) if file_path == index.file_path() => {}
                    Some(file_path) => {
                        return Err(StorageError::Integrity(format!(
                            "node '{uri}' is indexed under '{}' but stored under '{file_path}'",
                            index.file_path()
                        )));
                    }
                    None => {
                        return Err(StorageError::Integrity(format!(
                            "file index '{}' references missing node '{uri}'",
                            index.file_path()
                        )));
                    }
                }
            }
        }

        if indexed.len() != node_files.len() || !node_files.keys().all(|uri| indexed.contains(uri))
        {
            let missing: Vec<_> = node_files
                .keys()
                .filter(|uri| !indexed.contains(*uri))
                .map(ToString::to_string)
                .collect();
            return Err(StorageError::Integrity(format!(
                "{} stored nodes are absent from the file index: {}",
                missing.len(),
                missing.join(", ")
            )));
        }

        Ok(StoreReport {
            nodes: node_count,
            edges: edge_count,
            files,
        })
    }
}

fn read_nodes(transaction: &ReadTransaction) -> Result<Vec<CodeEntityNode>, StorageError> {
    let table = transaction.open_table(TABLE_NODES)?;
    let mut nodes = Vec::with_capacity(table.len()? as usize);
    for entry in table.iter()? {
        let (_, value) = entry?;
        nodes.push(schema::decode_node(value.value())?);
    }
    Ok(nodes)
}

fn read_edges(transaction: &ReadTransaction) -> Result<Vec<CodeEdge>, StorageError> {
    let table = transaction.open_table(TABLE_EDGES)?;
    let mut edges = Vec::with_capacity(table.len()? as usize);
    for entry in table.iter()? {
        let (_, value) = entry?;
        edges.push(schema::decode_edge(value.value())?);
    }
    Ok(edges)
}

fn read_file_index(
    transaction: &ReadTransaction,
    file_path: &str,
) -> Result<Option<FileIndex>, StorageError> {
    let table = transaction.open_table(TABLE_FILE_INDEX)?;
    match table.get(file_path)? {
        Some(value) => Ok(Some(schema::decode_file_index(value.value())?)),
        None => Ok(None),
    }
}

fn read_stored_version(table: &redb::Table<'_, &str, &[u8]>) -> Result<Option<u32>, StorageError> {
    match table.get(META_SCHEMA_VERSION)? {
        Some(value) => {
            let text = std::str::from_utf8(value.value()).map_err(|error| {
                StorageError::CorruptMetadata {
                    key: META_SCHEMA_VERSION.to_string(),
                    reason: error.to_string(),
                }
            })?;
            let version = text
                .parse::<u32>()
                .map_err(|error| StorageError::CorruptMetadata {
                    key: META_SCHEMA_VERSION.to_string(),
                    reason: error.to_string(),
                })?;
            Ok(Some(version))
        }
        None => Ok(None),
    }
}

/// Apply physical-schema migrations in order.
///
/// Version `0` is the implicit layout written before metadata existed; it is
/// upgraded to `1` without touching table contents. Future versions add a
/// contiguous arm here. A missing arm is a typed error rather than a silent
/// partial upgrade.
fn migrate(_transaction: &WriteTransaction, from: u32) -> Result<(), StorageError> {
    let mut version = from;
    while version < STORAGE_SCHEMA_VERSION {
        match version {
            0 => version = 1,
            _ => return Err(StorageError::MissingMigration { from: version }),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, SourceSpan};
    use tempfile::tempdir;

    fn node(file_path: &str, name: &str, kind: NodeKind) -> CodeEntityNode {
        let uri = SymbolUri::new("repo", file_path, None, name, kind.default_uri_suffix()).unwrap();
        CodeEntityNode::try_new(
            uri,
            name,
            kind,
            file_path,
            "rust",
            SourceSpan::new(1, 0, 2, 0, 0, 10).unwrap(),
        )
        .unwrap()
    }

    fn edge(source: &CodeEntityNode, target: &CodeEntityNode) -> CodeEdge {
        CodeEdge::new(source.uri().clone(), target.uri().clone(), EdgeKind::Calls)
            .with_confidence(0.9)
            .unwrap()
            .with_call_site(SourceSpan::new(1, 4, 1, 12, 4, 12).unwrap())
    }

    fn store() -> (tempfile::TempDir, GraphStore) {
        let directory = tempdir().unwrap();
        let store = GraphStore::open(directory.path().join("graph.redb")).unwrap();
        (directory, store)
    }

    #[test]
    fn test_open_stamps_schema_and_reopens() {
        let (directory, store) = store();
        assert_eq!(store.schema_version().unwrap(), STORAGE_SCHEMA_VERSION);
        assert_eq!(store.node_count().unwrap(), 0);
        assert_eq!(store.edge_count().unwrap(), 0);
        assert_eq!(store.verify().unwrap(), StoreReport::default());
        drop(store);

        let reopened = GraphStore::open(directory.path().join("graph.redb")).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), STORAGE_SCHEMA_VERSION);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let (_directory, store) = store();
        let source = node("src/a.rs", "run", NodeKind::Function);
        let target = node("src/b.rs", "load", NodeKind::Function);
        let call = edge(&source, &target);

        let report = store
            .save_batch(
                &[source.clone(), target.clone()],
                std::slice::from_ref(&call),
            )
            .unwrap();
        assert_eq!(report.nodes_written, 2);
        assert_eq!(report.edges_written, 1);
        assert_eq!(report.files_indexed, 2);
        assert_eq!(store.node_count().unwrap(), 2);
        assert_eq!(store.edge_count().unwrap(), 1);

        assert_eq!(store.get_node(source.uri()).unwrap(), Some(source.clone()));
        assert_eq!(
            store
                .get_edge(source.uri(), target.uri(), EdgeKind::Calls)
                .unwrap(),
            Some(call.clone())
        );

        let (nodes, edges) = store.load_all().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(edges, vec![call]);
        assert_eq!(store.verify().unwrap().nodes, 2);
    }

    #[test]
    fn test_file_index_tracks_nodes_and_removal() {
        let (_directory, store) = store();
        let first = node("src/a.rs", "first", NodeKind::Function);
        let second = node("src/a.rs", "second", NodeKind::Function);
        let other = node("src/b.rs", "other", NodeKind::Function);

        store
            .save_batch(&[first.clone(), second.clone(), other.clone()], &[])
            .unwrap();

        assert_eq!(store.node_uris_for_file("src/a.rs").unwrap().len(), 2);
        assert_eq!(store.nodes_for_file("src/a.rs").unwrap().len(), 2);
        assert!(store.nodes_for_file("src/missing.rs").unwrap().is_empty());

        store
            .save_batch(&[first.clone(), other.clone()], &[])
            .unwrap();
        assert_eq!(store.node_uris_for_file("src/a.rs").unwrap().len(), 2);
    }

    #[test]
    fn test_save_is_idempotent_and_updates_metadata() {
        let (_directory, store) = store();
        let source = node("src/a.rs", "run", NodeKind::Function);
        let mut updated = source.clone().with_signature("fn run() -> u32");

        let first = store
            .save_batch(std::slice::from_ref(&source), &[])
            .unwrap();
        let second = store
            .save_batch(std::slice::from_ref(&source), &[])
            .unwrap();
        assert_eq!(first.nodes_written, second.nodes_written);
        assert_eq!(store.node_count().unwrap(), 1);

        updated = updated.with_visibility(crate::model::Visibility::Public);
        store
            .save_batch(std::slice::from_ref(&updated), &[])
            .unwrap();
        let loaded = store.get_node(source.uri()).unwrap().unwrap();
        assert_eq!(loaded.signature(), Some("fn run() -> u32"));
        assert_eq!(loaded.visibility(), crate::model::Visibility::Public);
        assert_eq!(store.node_count().unwrap(), 1);
    }

    #[test]
    fn test_remove_file_removes_nodes_edges_and_index() {
        let (_directory, store) = store();
        let source = node("src/a.rs", "run", NodeKind::Function);
        let target = node("src/b.rs", "load", NodeKind::Function);
        let call = edge(&source, &target);
        store
            .save_batch(&[source.clone(), target.clone()], &[call])
            .unwrap();

        let mut batch = WriteBatch::new();
        batch.remove_file("src/a.rs");
        let report = store.write_batch(&batch).unwrap();

        assert_eq!(report.nodes_removed, 1);
        assert_eq!(report.edges_removed, 1);
        assert!(store.get_node(source.uri()).unwrap().is_none());
        assert!(store.get_node(target.uri()).unwrap().is_some());
        assert_eq!(store.node_count().unwrap(), 1);
        assert_eq!(store.edge_count().unwrap(), 0);
        assert!(store.file_index("src/a.rs").unwrap().is_none());
        assert_eq!(store.file_index("src/b.rs").unwrap().unwrap().len(), 1);
        assert_eq!(store.verify().unwrap().nodes, 1);
    }

    #[test]
    fn test_remove_node_updates_index() {
        let (_directory, store) = store();
        let first = node("src/a.rs", "first", NodeKind::Function);
        let second = node("src/a.rs", "second", NodeKind::Function);
        store
            .save_batch(&[first.clone(), second.clone()], &[])
            .unwrap();

        let mut batch = WriteBatch::new();
        batch.remove_node(first.uri());
        store.write_batch(&batch).unwrap();

        let uris = store.node_uris_for_file("src/a.rs").unwrap();
        assert_eq!(uris, vec![second.uri().clone()]);
        assert_eq!(store.verify().unwrap().nodes, 1);
    }

    #[test]
    fn test_dangling_edge_aborts_the_whole_batch() {
        let (_directory, store) = store();
        let source = node("src/a.rs", "run", NodeKind::Function);
        let missing = node("src/missing.rs", "missing", NodeKind::Function);
        let bad = edge(&source, &missing);

        let error = store
            .save_batch(std::slice::from_ref(&source), std::slice::from_ref(&bad))
            .unwrap_err();
        assert!(matches!(error, StorageError::DanglingEdge { .. }));
        assert_eq!(store.node_count().unwrap(), 0);
        assert_eq!(store.edge_count().unwrap(), 0);
        assert!(store.file_index("src/a.rs").unwrap().is_none());
    }

    #[test]
    fn test_save_graph_roundtrip_preserves_edge_metadata() {
        let (_directory, store) = store();
        let source = node("src/a.rs", "run", NodeKind::Function);
        let target = node("src/b.rs", "load", NodeKind::Function);
        let call = edge(&source, &target);

        let mut graph = MemoryGraph::new();
        graph.add_node(source.clone()).unwrap();
        graph.add_node(target.clone()).unwrap();
        graph.add_edge(call.clone()).unwrap();

        store.save_graph(&graph).unwrap();
        let loaded = store.load_graph().unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);

        let source_index = loaded.get_node_index(source.uri()).unwrap();
        let stored = loaded
            .outgoing_edges(source_index)
            .find(|(node, _)| node.uri() == target.uri())
            .map(|(_, edge)| edge)
            .unwrap();
        assert_eq!(stored.kind(), EdgeKind::Calls);
        assert_eq!(stored.confidence().value(), 0.9);
        assert_eq!(
            stored.call_site().unwrap(),
            SourceSpan::new(1, 4, 1, 12, 4, 12).unwrap()
        );
    }

    #[test]
    fn test_newer_schema_version_is_rejected() {
        let (directory, store) = store();
        store.set_meta(META_SCHEMA_VERSION, "999").unwrap();
        drop(store);

        let error = GraphStore::open(directory.path().join("graph.redb")).unwrap_err();
        assert!(matches!(
            error,
            StorageError::SchemaTooNew { found: 999, .. }
        ));
    }

    #[test]
    fn test_metadata_helpers_and_compaction() {
        let (_directory, mut store) = store();
        assert_eq!(store.last_commit().unwrap(), None);
        store.set_last_commit("abc123").unwrap();
        store.set_repo_name("rusty").unwrap();
        assert_eq!(store.last_commit().unwrap().as_deref(), Some("abc123"));
        assert_eq!(store.repo_name().unwrap().as_deref(), Some("rusty"));
        assert!(store.compact().is_ok());
    }

    #[test]
    fn test_save_order_does_not_change_load_order() {
        let (_directory, store) = store();
        let first = node("src/a.rs", "first", NodeKind::Function);
        let second = node("src/b.rs", "second", NodeKind::Function);

        store
            .save_batch(&[second.clone(), first.clone()], &[])
            .unwrap();
        let (nodes, _) = store.load_all().unwrap();
        assert_eq!(nodes, vec![first, second]);
    }

    #[test]
    fn test_verify_rejects_corrupt_record() {
        let (_directory, store) = store();
        {
            let transaction = store.db.begin_write().unwrap();
            {
                let mut table = transaction.open_table(TABLE_NODES).unwrap();
                table.insert("corrupt", b"not a record".as_slice()).unwrap();
            }
            transaction.commit().unwrap();
        }

        assert!(matches!(store.verify(), Err(StorageError::Codec(_))));
    }

    #[test]
    fn test_verify_rejects_mismatched_primary_key() {
        let (_directory, store) = store();
        let node = node("src/a.rs", "run", NodeKind::Function);
        let bytes = schema::encode_node(&node).unwrap();

        {
            let transaction = store.db.begin_write().unwrap();
            {
                let mut table = transaction.open_table(TABLE_NODES).unwrap();
                table.insert("wrong-key", bytes.as_slice()).unwrap();
            }
            transaction.commit().unwrap();
        }

        assert!(matches!(store.verify(), Err(StorageError::Integrity(_))));
    }

    #[test]
    fn test_remove_file_and_reinsert_in_one_batch() {
        let (_directory, store) = store();
        let old = node("src/a.rs", "old", NodeKind::Function);
        let target = node("src/b.rs", "target", NodeKind::Function);
        let old_call = edge(&old, &target);
        store
            .save_batch(&[old.clone(), target.clone()], &[old_call])
            .unwrap();

        let replacement = node("src/a.rs", "new", NodeKind::Function);
        let new_call = edge(&replacement, &target);

        let mut batch = WriteBatch::new();
        batch.put_node(replacement.clone());
        batch.put_edge(new_call.clone());
        batch.remove_file("src/a.rs");
        let report = store.write_batch(&batch).unwrap();

        assert_eq!(report.nodes_removed, 1);
        assert_eq!(report.edges_removed, 1);
        assert_eq!(report.nodes_written, 1);
        assert_eq!(report.edges_written, 1);
        assert_eq!(report.files_indexed, 1);

        assert!(store.get_node(old.uri()).unwrap().is_none());
        assert_eq!(
            store.get_node(replacement.uri()).unwrap(),
            Some(replacement.clone())
        );
        assert!(
            store
                .get_edge(old.uri(), target.uri(), EdgeKind::Calls)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .get_edge(replacement.uri(), target.uri(), EdgeKind::Calls)
                .unwrap(),
            Some(new_call)
        );
        assert_eq!(
            store.node_uris_for_file("src/a.rs").unwrap(),
            vec![replacement.uri().clone()]
        );
        assert_eq!(store.verify().unwrap().nodes, 2);
    }

    #[test]
    fn test_write_batch_last_write_wins() {
        let (_directory, store) = store();
        let original = node("src/a.rs", "run", NodeKind::Function);
        let updated = original.clone().with_signature("fn run() -> u32");

        let mut batch = WriteBatch::new();
        batch.put_node(original);
        batch.put_node(updated);
        store.write_batch(&batch).unwrap();

        assert_eq!(store.node_count().unwrap(), 1);
        let path = "src/a.rs";
        assert_eq!(
            store.nodes_for_file(path).unwrap()[0].signature(),
            Some("fn run() -> u32")
        );
    }
}
