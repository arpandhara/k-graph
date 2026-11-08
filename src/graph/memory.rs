use dashmap::DashMap;
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::error::GraphError;
use crate::model::{CodeEdge, CodeEntityNode, SymbolUri};

/// Statistical overview of the in-memory graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub total_files: usize,
    pub nodes_by_kind: HashMap<String, usize>,
    pub edges_by_kind: HashMap<String, usize>,
    pub total_callables: usize,
    pub total_types: usize,
}

/// The core in-memory graph engine.
///
/// Combines a `StableDiGraph` with fast $O(1)$ secondary indices for URIs, file paths, and symbol names.
/// Using `StableDiGraph` ensures that removing nodes does not invalidate indices of other nodes.
///
/// All fields are private. Mutations must go through this module so the three
/// indices and the redundant identity fields on `CodeEntityNode` stay consistent.
#[derive(Clone)]
pub struct MemoryGraph {
    /// Underlying directed graph with stable indices.
    graph: StableDiGraph<CodeEntityNode, CodeEdge>,
    /// URI -> NodeIndex index
    uri_to_node: DashMap<SymbolUri, NodeIndex>,
    /// File path -> NodeIndices in that file
    file_to_nodes: DashMap<String, Vec<NodeIndex>>,
    /// Unqualified symbol name -> NodeIndices matching that name
    name_index: DashMap<String, Vec<NodeIndex>>,
}

impl Default for MemoryGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryGraph {
    /// Create a new, empty in-memory knowledge graph.
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            uri_to_node: DashMap::new(),
            file_to_nodes: DashMap::new(),
            name_index: DashMap::new(),
        }
    }

    /// Number of active nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of active edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Read-only access to the underlying graph.
    pub fn graph(&self) -> &StableDiGraph<CodeEntityNode, CodeEdge> {
        &self.graph
    }

    /// Add a node to the graph or update its metadata if the URI already exists.
    ///
    /// The node's identity fields are validated against its URI. Re-adding an
    /// existing URI is allowed only when the name and file path still match the
    /// existing node, so the secondary indices never become stale.
    pub fn add_node(&mut self, node: CodeEntityNode) -> Result<NodeIndex, GraphError> {
        node.validate()?;

        let uri = node.uri().clone();
        let name = node.name().to_string();
        let file_path = node.file_path().to_string();

        if let Some(existing_idx) = self.uri_to_node.get(&uri).map(|entry| *entry) {
            let existing_name = {
                let existing = self
                    .graph
                    .node_weight(existing_idx)
                    .ok_or(GraphError::MissingNode(existing_idx))?;
                existing.name().to_string()
            };
            let existing_file = {
                let existing = self
                    .graph
                    .node_weight(existing_idx)
                    .ok_or(GraphError::MissingNode(existing_idx))?;
                existing.file_path().to_string()
            };

            if existing_name != name || existing_file != file_path {
                return Err(GraphError::IdentityMismatch);
            }

            let slot = self
                .graph
                .node_weight_mut(existing_idx)
                .ok_or(GraphError::MissingNode(existing_idx))?;
            *slot = node;
            return Ok(existing_idx);
        }

        let idx = self.graph.add_node(node);
        self.uri_to_node.insert(uri, idx);

        self.file_to_nodes.entry(file_path).or_default().push(idx);

        self.name_index.entry(name).or_default().push(idx);

        Ok(idx)
    }

    /// Retrieve a node by its graph index.
    pub fn get_node(&self, idx: NodeIndex) -> Option<&CodeEntityNode> {
        self.graph.node_weight(idx)
    }

    /// Find a node by its canonical `SymbolUri`.
    pub fn get_node_by_uri(&self, uri: &SymbolUri) -> Option<&CodeEntityNode> {
        self.uri_to_node
            .get(uri)
            .and_then(|idx| self.graph.node_weight(*idx))
    }

    /// Lookup the `NodeIndex` for a `SymbolUri`.
    pub fn get_node_index(&self, uri: &SymbolUri) -> Option<NodeIndex> {
        self.uri_to_node.get(uri).map(|entry| *entry)
    }

    /// Add an edge between two entities using their URIs.
    ///
    /// Returns `Some(EdgeIndex)` if both nodes exist in the graph, or `None` otherwise.
    pub fn add_edge(&mut self, edge: CodeEdge) -> Option<EdgeIndex> {
        let from_idx = self.get_node_index(&edge.source_uri)?;
        let to_idx = self.get_node_index(&edge.target_uri)?;
        Some(self.graph.add_edge(from_idx, to_idx, edge))
    }

    /// Add an edge between two entities directly by `NodeIndex`.
    pub fn add_edge_by_index(
        &mut self,
        from: NodeIndex,
        to: NodeIndex,
        edge: CodeEdge,
    ) -> EdgeIndex {
        self.graph.add_edge(from, to, edge)
    }

    /// Retrieve an edge by its `EdgeIndex`.
    pub fn get_edge(&self, idx: EdgeIndex) -> Option<&CodeEdge> {
        self.graph.edge_weight(idx)
    }

    /// Remove a node and all its incoming and outgoing edges using its `SymbolUri`.
    pub fn remove_node_by_uri(&mut self, uri: &SymbolUri) -> Option<CodeEntityNode> {
        let idx = match self.uri_to_node.remove(uri) {
            Some((_, idx)) => idx,
            None => return None,
        };

        if let Some(node) = self.graph.node_weight(idx) {
            let file_path = node.file_path().to_string();
            let name = node.name().to_string();

            Self::remove_index_entry(&self.file_to_nodes, &file_path, idx);
            Self::remove_index_entry(&self.name_index, &name, idx);
        }

        self.graph.remove_node(idx)
    }

    /// Invalidate and remove all nodes associated with a specific file.
    ///
    /// Automatically detaches all connected edges from the graph.
    pub fn remove_nodes_by_file(&mut self, file_path: &str) -> Vec<CodeEntityNode> {
        let indices = match self.file_to_nodes.remove(file_path) {
            Some((_, indices)) => indices,
            None => return Vec::new(),
        };

        let mut removed_nodes = Vec::with_capacity(indices.len());

        for idx in indices {
            if let Some(node) = self.graph.node_weight(idx) {
                let uri = node.uri().clone();
                let name = node.name().to_string();

                self.uri_to_node.remove(&uri);
                Self::remove_index_entry(&self.name_index, &name, idx);
            }
            if let Some(removed) = self.graph.remove_node(idx) {
                removed_nodes.push(removed);
            }
        }

        removed_nodes
    }

    /// Remove a single index entry and delete the bucket when it becomes empty.
    fn remove_index_entry(map: &DashMap<String, Vec<NodeIndex>>, key: &str, idx: NodeIndex) {
        if let Some(mut nodes) = map.get_mut(key) {
            nodes.retain(|&i| i != idx);
            let empty = nodes.is_empty();
            drop(nodes);
            if empty {
                map.remove(key);
            }
        }
    }

    /// Find all nodes matching an unqualified symbol name.
    pub fn find_nodes_by_name(&self, name: &str) -> Vec<NodeIndex> {
        self.name_index
            .get(name)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Find all nodes originating from a specific file.
    pub fn find_nodes_by_file(&self, file_path: &str) -> Vec<NodeIndex> {
        self.file_to_nodes
            .get(file_path)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Retrieve all outgoing edges from a given node index.
    pub fn outgoing_edges(&self, idx: NodeIndex) -> Vec<(&CodeEntityNode, &CodeEdge)> {
        let mut results = Vec::new();
        for edge_ref in self.graph.edges_directed(idx, Direction::Outgoing) {
            let target_idx = edge_ref.target();
            if let Some(target_node) = self.graph.node_weight(target_idx) {
                results.push((target_node, edge_ref.weight()));
            }
        }
        results
    }

    /// Retrieve all incoming edges towards a given node index.
    pub fn incoming_edges(&self, idx: NodeIndex) -> Vec<(&CodeEntityNode, &CodeEdge)> {
        let mut results = Vec::new();
        for edge_ref in self.graph.edges_directed(idx, Direction::Incoming) {
            let source_idx = edge_ref.source();
            if let Some(source_node) = self.graph.node_weight(source_idx) {
                results.push((source_node, edge_ref.weight()));
            }
        }
        results
    }

    /// Compute statistics of the graph.
    pub fn stats(&self) -> GraphStats {
        let mut stats = GraphStats {
            total_nodes: self.graph.node_count(),
            total_edges: self.graph.edge_count(),
            total_files: self
                .file_to_nodes
                .iter()
                .filter(|entry| !entry.value().is_empty())
                .count(),
            ..Default::default()
        };

        for node in self.graph.node_weights() {
            let kind_str = format!("{:?}", node.kind());
            *stats.nodes_by_kind.entry(kind_str).or_default() += 1;

            if node.kind().is_callable() {
                stats.total_callables += 1;
            }
            if node.kind().is_type_definition() {
                stats.total_types += 1;
            }
        }

        for edge in self.graph.edge_weights() {
            let kind_str = edge.kind.as_str().to_string();
            *stats.edges_by_kind.entry(kind_str).or_default() += 1;
        }

        stats
    }

    /// Clear all nodes, edges, and secondary indices.
    pub fn clear(&mut self) {
        self.graph.clear();
        self.uri_to_node.clear();
        self.file_to_nodes.clear();
        self.name_index.clear();
    }

    /// Validate index consistency against the underlying graph.
    pub fn validate_integrity(&self) -> Result<(), String> {
        let active_nodes = self.graph.node_count();
        let uri_count = self.uri_to_node.len();

        if active_nodes != uri_count {
            return Err(format!(
                "Integrity violation: node_count ({}) != uri_to_node count ({})",
                active_nodes, uri_count
            ));
        }

        let mut uri_seen = HashSet::with_capacity(active_nodes);

        for entry in self.uri_to_node.iter() {
            let uri = entry.key();
            let idx = *entry.value();

            if !uri_seen.insert(idx) {
                return Err(format!(
                    "Integrity violation: duplicate URI index for NodeIndex {:?}",
                    idx
                ));
            }

            let node = self.graph.node_weight(idx).ok_or_else(|| {
                format!(
                    "Integrity violation: URI index '{}' points to missing node at idx {:?}",
                    uri, idx
                )
            })?;

            if node.uri() != uri {
                return Err(format!(
                    "Integrity violation: index URI '{}' does not match node URI '{}'",
                    uri,
                    node.uri()
                ));
            }

            let parsed = uri.parse().map_err(|err| {
                format!(
                    "Integrity violation: index URI '{}' is not parseable: {}",
                    uri, err
                )
            })?;

            if node.name() != parsed.name {
                return Err(format!(
                    "Integrity violation: node name '{}' does not match URI name '{}'",
                    node.name(),
                    parsed.name
                ));
            }

            if node.file_path() != parsed.file_path {
                return Err(format!(
                    "Integrity violation: node path '{}' does not match URI path '{}'",
                    node.file_path(),
                    parsed.file_path
                ));
            }

            if node.kind().default_uri_suffix() != parsed.kind_suffix {
                return Err(format!(
                    "Integrity violation: node kind '{:?}' does not match URI suffix '{}'",
                    node.kind(),
                    parsed.kind_suffix
                ));
            }
        }

        if uri_seen.len() != active_nodes {
            return Err(format!(
                "Integrity violation: {} active nodes but {} unique URI indices",
                active_nodes,
                uri_seen.len()
            ));
        }

        let mut file_seen = HashSet::with_capacity(active_nodes);

        for entry in self.file_to_nodes.iter() {
            let file_path = entry.key();
            let indices = entry.value();

            if indices.is_empty() {
                return Err(format!(
                    "Integrity violation: empty file index bucket for '{}'",
                    file_path
                ));
            }

            for &idx in indices.iter() {
                let node = self.graph.node_weight(idx).ok_or_else(|| {
                    format!(
                        "Integrity violation: file index '{}' points to missing node at idx {:?}",
                        file_path, idx
                    )
                })?;

                if node.file_path() != file_path {
                    return Err(format!(
                        "Integrity violation: file index '{}' contains node with path '{}'",
                        file_path,
                        node.file_path()
                    ));
                }

                if !file_seen.insert(idx) {
                    return Err(format!(
                        "Integrity violation: NodeIndex {:?} appears in multiple file buckets",
                        idx
                    ));
                }
            }
        }

        if file_seen != uri_seen {
            return Err(
                "Integrity violation: file index does not contain exactly the active nodes"
                    .to_string(),
            );
        }

        let mut name_seen = HashSet::with_capacity(active_nodes);

        for entry in self.name_index.iter() {
            let name = entry.key();
            let indices = entry.value();

            if indices.is_empty() {
                return Err(format!(
                    "Integrity violation: empty name index bucket for '{}'",
                    name
                ));
            }

            for &idx in indices.iter() {
                let node = self.graph.node_weight(idx).ok_or_else(|| {
                    format!(
                        "Integrity violation: name index '{}' points to missing node at idx {:?}",
                        name, idx
                    )
                })?;

                if node.name() != name {
                    return Err(format!(
                        "Integrity violation: name index '{}' contains node with name '{}'",
                        name,
                        node.name()
                    ));
                }

                if !name_seen.insert(idx) {
                    return Err(format!(
                        "Integrity violation: NodeIndex {:?} appears in multiple name buckets",
                        idx
                    ));
                }
            }
        }

        if name_seen != uri_seen {
            return Err(
                "Integrity violation: name index does not contain exactly the active nodes"
                    .to_string(),
            );
        }

        for edge_idx in self.graph.edge_indices() {
            let (source_idx, target_idx) =
                self.graph.edge_endpoints(edge_idx).ok_or_else(|| {
                    format!("Integrity violation: edge {:?} has no endpoints", edge_idx)
                })?;

            let source = self.graph.node_weight(source_idx).ok_or_else(|| {
                format!(
                    "Integrity violation: edge {:?} has missing source node {:?}",
                    edge_idx, source_idx
                )
            })?;

            let target = self.graph.node_weight(target_idx).ok_or_else(|| {
                format!(
                    "Integrity violation: edge {:?} has missing target node {:?}",
                    edge_idx, target_idx
                )
            })?;

            let edge = self
                .graph
                .edge_weight(edge_idx)
                .ok_or_else(|| format!("Integrity violation: edge {:?} has no weight", edge_idx))?;

            if edge.source_uri != *source.uri() {
                return Err(format!(
                    "Integrity violation: edge {:?} source URI '{}' does not match node URI '{}'",
                    edge_idx,
                    edge.source_uri,
                    source.uri()
                ));
            }

            if edge.target_uri != *target.uri() {
                return Err(format!(
                    "Integrity violation: edge {:?} target URI '{}' does not match node URI '{}'",
                    edge_idx,
                    edge.target_uri,
                    target.uri()
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, NodeKind, SourceSpan};

    fn sample_node(repo: &str, file: &str, name: &str, kind: NodeKind) -> CodeEntityNode {
        let uri = SymbolUri::new(repo, file, None, name, kind.default_uri_suffix()).unwrap();
        CodeEntityNode::try_new(uri, name, kind, file, "rust", SourceSpan::zero()).unwrap()
    }

    #[test]
    fn test_memory_graph_add_and_get() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let uri_a = node_a.uri().clone();

        let idx_a = mg.add_node(node_a.clone()).unwrap();
        assert_eq!(mg.node_count(), 1);
        assert_eq!(mg.get_node_index(&uri_a), Some(idx_a));

        let retrieved = mg.get_node_by_uri(&uri_a).unwrap();
        assert_eq!(retrieved.name(), "add_file");
        assert_eq!(retrieved.file_path(), "src/add.rs");

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_update_existing_node_keeps_indexes_consistent() {
        let mut mg = MemoryGraph::new();
        let node = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let idx = mg.add_node(node).unwrap();

        let updated = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function)
            .with_signature("pub fn add_file(path: &Path) -> Result<()>");

        let updated_idx = mg.add_node(updated).unwrap();
        assert_eq!(idx, updated_idx);
        assert_eq!(mg.node_count(), 1);
        assert_eq!(
            mg.get_node(idx).unwrap().signature(),
            Some("pub fn add_file(path: &Path) -> Result<()>")
        );
        assert_eq!(mg.find_nodes_by_name("add_file").len(), 1);
        assert_eq!(mg.find_nodes_by_file("src/add.rs").len(), 1);

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_edges() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let node_b = sample_node("rusty", "src/index.rs", "load", NodeKind::Method);
        let uri_a = node_a.uri().clone();
        let uri_b = node_b.uri().clone();

        let idx_a = mg.add_node(node_a).unwrap();
        let idx_b = mg.add_node(node_b).unwrap();

        let edge = CodeEdge::new(uri_a.clone(), uri_b.clone(), EdgeKind::Calls);
        let edge_idx = mg.add_edge(edge).expect("Failed to add edge");

        assert_eq!(mg.edge_count(), 1);
        assert_eq!(mg.get_edge(edge_idx).unwrap().kind, EdgeKind::Calls);

        let out = mg.outgoing_edges(idx_a);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.name(), "load");

        let inc = mg.incoming_edges(idx_b);
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].0.name(), "add_file");

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_remove_node_by_uri() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let node_b = sample_node("rusty", "src/index.rs", "load", NodeKind::Method);
        let uri_a = node_a.uri().clone();
        let uri_b = node_b.uri().clone();

        mg.add_node(node_a).unwrap();
        mg.add_node(node_b).unwrap();
        mg.add_edge(CodeEdge::new(uri_a.clone(), uri_b.clone(), EdgeKind::Calls));
        assert_eq!(mg.node_count(), 2);
        assert_eq!(mg.edge_count(), 1);

        // Remove node A
        let removed = mg.remove_node_by_uri(&uri_a);
        assert!(removed.is_some());
        assert_eq!(mg.node_count(), 1);
        // Removing node A must automatically remove its outgoing edge
        assert_eq!(mg.edge_count(), 0);
        assert!(mg.get_node_by_uri(&uri_a).is_none());
        assert!(mg.get_node_by_uri(&uri_b).is_some());
        assert_eq!(mg.stats().total_files, 1);
        assert!(!mg.file_to_nodes.contains_key("src/add.rs"));
        assert!(!mg.name_index.contains_key("add_file"));

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_remove_nodes_by_file() {
        let mut mg = MemoryGraph::new();
        let node_1 = sample_node("rusty", "src/merge.rs", "merge", NodeKind::Function);
        let node_2 = sample_node("rusty", "src/merge.rs", "MergeState", NodeKind::Struct);
        let node_3 = sample_node(
            "rusty",
            "src/commit.rs",
            "create_commit",
            NodeKind::Function,
        );

        mg.add_node(node_1).unwrap();
        mg.add_node(node_2).unwrap();
        mg.add_node(node_3).unwrap();
        assert_eq!(mg.node_count(), 3);

        let removed = mg.remove_nodes_by_file("src/merge.rs");
        assert_eq!(removed.len(), 2);
        assert_eq!(mg.node_count(), 1);
        assert_eq!(mg.find_nodes_by_file("src/merge.rs").len(), 0);
        assert_eq!(mg.find_nodes_by_file("src/commit.rs").len(), 1);
        assert!(!mg.name_index.contains_key("merge"));
        assert!(!mg.name_index.contains_key("MergeState"));

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_stats() {
        let mut mg = MemoryGraph::new();
        mg.add_node(sample_node("rusty", "src/a.rs", "foo", NodeKind::Function))
            .unwrap();
        mg.add_node(sample_node("rusty", "src/a.rs", "Bar", NodeKind::Struct))
            .unwrap();
        mg.add_node(sample_node("rusty", "src/b.rs", "baz", NodeKind::Function))
            .unwrap();

        let stats = mg.stats();
        assert_eq!(stats.total_nodes, 3);
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_callables, 2);
        assert_eq!(stats.total_types, 1);
    }

    #[test]
    fn test_memory_graph_integrity_detects_stale_indexes() {
        let mut mg = MemoryGraph::new();
        let node = sample_node("rusty", "src/a.rs", "foo", NodeKind::Function);
        let uri = node.uri().clone();
        mg.add_node(node).unwrap();

        // Simulate a legacy corrupt bucket: empty but still present.
        mg.name_index.get_mut("foo").unwrap().clear();
        assert!(mg.validate_integrity().is_err());

        mg.clear();
        assert!(mg.get_node_by_uri(&uri).is_none());
        assert!(mg.validate_integrity().is_ok());
    }
}
