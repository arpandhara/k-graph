//! On-disk table definitions and record formats.
//!
//! The storage layer uses redb's borrowed key/value types (`&str` and `&[u8]`)
//! so reads can expose pages without first allocating an intermediate `String`
//! or `Vec<u8>`. Record bodies are still encoded through the validated codec in
//! [`crate::persistence`]; the borrowed bytes are the codec's versioned
//! envelope, not raw Serde output.
//!
//! Four tables make up a store:
//!
//! | Table | Key | Value |
//! |---|---|---|
//! | `TABLE_NODES` | canonical `SymbolUri` text | encoded [`crate::model::CodeEntityNode`] |
//! | `TABLE_EDGES` | [`edge_key`] | encoded [`crate::model::CodeEdge`] |
//! | `TABLE_FILE_INDEX` | repository-relative file path | encoded [`FileIndex`] |
//! | `TABLE_META` | metadata key | UTF-8 metadata value |
//!
//! Keys are canonical and deterministic. Nodes are keyed by their URI; edges by
//! a NUL-separated `(source, target, kind)` tuple. `SymbolUri` rejects control
//! characters, so a NUL separator cannot occur inside a URI and the edge key is
//! unambiguous without length prefixes.

use redb::TableDefinition;
use serde::{Deserialize, Serialize};

use crate::model::{CodeEdge, CodeEntityNode, EdgeKind, SymbolUri};
use crate::persistence::{CodecError, ValidateRecord, decode_record, encode_record};

/// Version of the physical table layout.
///
/// This is intentionally separate from the record [`crate::persistence::SCHEMA_VERSION`]:
/// the record envelope describes one value, while this version describes the
/// set of tables and their metadata. Bump it when a migration is required.
pub const STORAGE_SCHEMA_VERSION: u32 = 1;

/// Metadata key holding the physical schema version.
pub const META_SCHEMA_VERSION: &str = "schema_version";
/// Metadata key holding the repository name that produced the graph.
pub const META_REPO_NAME: &str = "repo_name";
/// Metadata key holding the last indexed VCS commit, maintained by Phase 5.
pub const META_LAST_COMMIT: &str = "last_commit";

pub(crate) const TABLE_NODES: TableDefinition<&str, &[u8]> = TableDefinition::new("kg.nodes");
pub(crate) const TABLE_EDGES: TableDefinition<&str, &[u8]> = TableDefinition::new("kg.edges");
pub(crate) const TABLE_FILE_INDEX: TableDefinition<&str, &[u8]> = TableDefinition::new("kg.files");
pub(crate) const TABLE_META: TableDefinition<&str, &[u8]> = TableDefinition::new("kg.meta");

/// Build the deterministic primary key for an edge.
///
/// The separator is `NUL`, which [`SymbolUri`] forbids in every component.
/// `EdgeKind::as_str` is the stable uppercase wire code, not `Debug` output, so
/// the key survives enum formatting changes.
pub fn edge_key(source: &SymbolUri, target: &SymbolUri, kind: EdgeKind) -> String {
    format!("{}\u{0}{}\u{0}{}", source, target, kind.as_str())
}

/// The nodes stored for one repository-relative file.
///
/// The file index makes "remove this file" a bounded lookup instead of a full
/// node-table scan. It is stored as a validated record so a corrupt index is
/// rejected by the same trust boundary as nodes and edges.
///
/// `FileIndex::new` sorts and de-duplicates the node URIs. [`ValidateRecord`]
/// additionally requires a non-empty, strictly ordered list and that every URI
/// belongs to `file_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIndex {
    file_path: String,
    nodes: Vec<SymbolUri>,
}

impl FileIndex {
    /// Create an index with sorted, de-duplicated node URIs.
    pub fn new(file_path: impl Into<String>, mut nodes: Vec<SymbolUri>) -> Self {
        nodes.sort();
        nodes.dedup();
        Self {
            file_path: file_path.into(),
            nodes,
        }
    }

    /// Repository-relative file path this index describes.
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Sorted node URIs belonging to the file.
    pub fn nodes(&self) -> &[SymbolUri] {
        &self.nodes
    }

    /// Number of indexed nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when the index names no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl ValidateRecord for FileIndex {
    fn validate_record(&self) -> Result<(), String> {
        if self.nodes.is_empty() {
            return Err("file index must contain at least one node".to_string());
        }

        let mut previous: Option<&SymbolUri> = None;
        for uri in &self.nodes {
            let parsed = uri
                .parse()
                .map_err(|error| format!("invalid node URI in file index: {error}"))?;

            if parsed.file_path != self.file_path {
                return Err(format!(
                    "file index for '{}' contains a node from '{}'",
                    self.file_path, parsed.file_path
                ));
            }

            if let Some(previous) = previous
                && previous >= uri
            {
                return Err("file index node URIs must be strictly sorted and unique".to_string());
            }
            previous = Some(uri);
        }

        Ok(())
    }
}

pub(crate) fn encode_node(node: &CodeEntityNode) -> Result<Vec<u8>, CodecError> {
    encode_record(node)
}

pub(crate) fn decode_node(bytes: &[u8]) -> Result<CodeEntityNode, CodecError> {
    decode_record(bytes)
}

pub(crate) fn encode_edge(edge: &CodeEdge) -> Result<Vec<u8>, CodecError> {
    encode_record(edge)
}

pub(crate) fn decode_edge(bytes: &[u8]) -> Result<CodeEdge, CodecError> {
    decode_record(bytes)
}

pub(crate) fn encode_file_index(index: &FileIndex) -> Result<Vec<u8>, CodecError> {
    encode_record(index)
}

pub(crate) fn decode_file_index(bytes: &[u8]) -> Result<FileIndex, CodecError> {
    decode_record(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, SourceSpan};

    fn uri(file_path: &str, name: &str) -> SymbolUri {
        SymbolUri::new("repo", file_path, None, name, "fn").unwrap()
    }

    #[test]
    fn test_edge_key_uses_stable_kind_and_cannot_collide() {
        let source = SymbolUri::new("repo", "a.rs", None, "a", "fn").unwrap();
        let target = SymbolUri::new("repo", "b.rs", None, "b", "fn").unwrap();

        let calls = edge_key(&source, &target, EdgeKind::Calls);
        let imports = edge_key(&source, &target, EdgeKind::Imports);
        assert_ne!(calls, imports);
        assert!(calls.contains("\u{0}"));
        assert!(calls.ends_with("CALLS"));
        assert_eq!(calls, format!("{source}\u{0}{target}\u{0}CALLS"));
    }

    #[test]
    fn test_file_index_sorts_and_deduplicates() {
        let index = FileIndex::new(
            "src/a.rs",
            vec![
                uri("src/a.rs", "b"),
                uri("src/a.rs", "a"),
                uri("src/a.rs", "b"),
            ],
        );
        assert_eq!(index.len(), 2);
        assert!(!index.is_empty());
        assert!(index.validate_record().is_ok());
        assert!(index.nodes()[0].as_str().ends_with("#a:fn"));
        assert!(index.nodes()[1].as_str().ends_with("#b:fn"));
    }

    #[test]
    fn test_file_index_rejects_empty_duplicate_and_foreign_nodes() {
        let empty = FileIndex {
            file_path: "src/a.rs".to_string(),
            nodes: Vec::new(),
        };
        assert!(empty.validate_record().is_err());

        let duplicate = FileIndex {
            file_path: "src/a.rs".to_string(),
            nodes: vec![uri("src/a.rs", "a"), uri("src/a.rs", "a")],
        };
        assert!(duplicate.validate_record().is_err());

        let foreign = FileIndex {
            file_path: "src/a.rs".to_string(),
            nodes: vec![uri("src/b.rs", "a")],
        };
        assert!(foreign.validate_record().is_err());
    }

    #[test]
    fn test_file_index_roundtrips_through_codec() {
        let index = FileIndex::new("src/a.rs", vec![uri("src/a.rs", "load")]);
        let bytes = encode_file_index(&index).unwrap();
        let decoded = decode_file_index(&bytes).unwrap();
        assert_eq!(index, decoded);
    }

    #[test]
    fn test_node_record_roundtrips_through_schema() {
        let node = CodeEntityNode::try_new(
            uri("src/a.rs", "load"),
            "load",
            NodeKind::Function,
            "src/a.rs",
            "rust",
            SourceSpan::zero(),
        )
        .unwrap();
        let bytes = encode_node(&node).unwrap();
        assert_eq!(decode_node(&bytes).unwrap(), node);
    }
}
