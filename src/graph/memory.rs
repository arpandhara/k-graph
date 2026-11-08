use std::collections::HashMap;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

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
#[derive(Clone)]
pub struct MemoryGraph {
    /// Underlying directed graph with stable indices.
    pub graph: StableDiGraph<CodeEntityNode, CodeEdge>,
    /// URI -> NodeIndex index
    pub uri_to_node: DashMap<SymbolUri, NodeIndex>,
    /// File path -> NodeIndices in that file
    pub file_to_nodes: DashMap<String, Vec<NodeIndex>>,
    /// Unqualified symbol name -> NodeIndices matching that name
    pub name_index: DashMap<String, Vec<NodeIndex>>,
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

    /// Add a node to the graph or update its metadata if the URI already exists.
    pub fn add_node(&mut self, node: CodeEntityNode) -> NodeIndex {
        let uri = node.uri.clone();
        let name = node.name.clone();
        let file_path = node.file_path.clone();

        if let Some(existing_idx) = self.uri_to_node.get(&uri) {
            let idx = *existing_idx;
            if let Some(weight) = self.graph.node_weight_mut(idx) {
                *weight = node;
            }
            return idx;
        }

        let idx = self.graph.add_node(node);
        self.uri_to_node.insert(uri, idx);

        self.file_to_nodes
            .entry(file_path)
            .or_default()
            .push(idx);

        self.name_index
            .entry(name)
            .or_default()
            .push(idx);

        idx
    }

    /// Retrieve a node by its graph index.
    pub fn get_node(&self, idx: NodeIndex) -> Option<&CodeEntityNode> {
        self.graph.node_weight(idx)
    }

    /// Retrieve a mutable reference to a node by its graph index.
    pub fn get_node_mut(&mut self, idx: NodeIndex) -> Option<&mut CodeEntityNode> {
        self.graph.node_weight_mut(idx)
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
    pub fn add_edge_by_index(&mut self, from: NodeIndex, to: NodeIndex, edge: CodeEdge) -> EdgeIndex {
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
            let file_path = node.file_path.clone();
            let name = node.name.clone();

            if let Some(mut file_nodes) = self.file_to_nodes.get_mut(&file_path) {
                file_nodes.retain(|&i| i != idx);
            }
            if let Some(mut name_nodes) = self.name_index.get_mut(&name) {
                name_nodes.retain(|&i| i != idx);
            }
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
                self.uri_to_node.remove(&node.uri);
                let name = node.name.clone();
                if let Some(mut name_nodes) = self.name_index.get_mut(&name) {
                    name_nodes.retain(|&i| i != idx);
                }
            }
            if let Some(removed) = self.graph.remove_node(idx) {
                removed_nodes.push(removed);
            }
        }

        removed_nodes
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
            total_files: self.file_to_nodes.len(),
            ..Default::default()
        };

        for node in self.graph.node_weights() {
            let kind_str = format!("{:?}", node.kind);
            *stats.nodes_by_kind.entry(kind_str).or_default() += 1;

            if node.kind.is_callable() {
                stats.total_callables += 1;
            }
            if node.kind.is_type_definition() {
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
        let actual_nodes = self.graph.node_count();
        let uri_count = self.uri_to_node.len();

        if actual_nodes != uri_count {
            return Err(format!(
                "Integrity violation: node_count ({}) != uri_to_node count ({})",
                actual_nodes, uri_count
            ));
        }

        for entry in self.uri_to_node.iter() {
            let uri = entry.key();
            let idx = *entry.value();
            match self.graph.node_weight(idx) {
                Some(node) => {
                    if node.uri != *uri {
                        return Err(format!(
                            "Integrity violation: index URI '{}' does not match node URI '{}'",
                            uri, node.uri
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "Integrity violation: index points to missing node at idx {:?}",
                        idx
                    ));
                }
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
        let uri = SymbolUri::new(repo, file, None, name, kind.default_uri_suffix());
        CodeEntityNode::new(uri, name, kind, file, "rust", SourceSpan::zero())
    }

    #[test]
    fn test_memory_graph_add_and_get() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let uri_a = node_a.uri.clone();

        let idx_a = mg.add_node(node_a.clone());
        assert_eq!(mg.node_count(), 1);
        assert_eq!(mg.get_node_index(&uri_a), Some(idx_a));

        let retrieved = mg.get_node_by_uri(&uri_a).unwrap();
        assert_eq!(retrieved.name, "add_file");
        assert_eq!(retrieved.file_path, "src/add.rs");

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_edges() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let node_b = sample_node("rusty", "src/index.rs", "load", NodeKind::Method);
        let uri_a = node_a.uri.clone();
        let uri_b = node_b.uri.clone();

        let idx_a = mg.add_node(node_a);
        let idx_b = mg.add_node(node_b);

        let edge = CodeEdge::new(uri_a.clone(), uri_b.clone(), EdgeKind::Calls);
        let edge_idx = mg.add_edge(edge).expect("Failed to add edge");

        assert_eq!(mg.edge_count(), 1);
        assert_eq!(mg.get_edge(edge_idx).unwrap().kind, EdgeKind::Calls);

        let out = mg.outgoing_edges(idx_a);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.name, "load");

        let inc = mg.incoming_edges(idx_b);
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].0.name, "add_file");

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_remove_node_by_uri() {
        let mut mg = MemoryGraph::new();
        let node_a = sample_node("rusty", "src/add.rs", "add_file", NodeKind::Function);
        let node_b = sample_node("rusty", "src/index.rs", "load", NodeKind::Method);
        let uri_a = node_a.uri.clone();
        let uri_b = node_b.uri.clone();

        mg.add_node(node_a);
        mg.add_node(node_b);
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

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_remove_nodes_by_file() {
        let mut mg = MemoryGraph::new();
        let node_1 = sample_node("rusty", "src/merge.rs", "merge", NodeKind::Function);
        let node_2 = sample_node("rusty", "src/merge.rs", "MergeState", NodeKind::Struct);
        let node_3 = sample_node("rusty", "src/commit.rs", "create_commit", NodeKind::Function);

        mg.add_node(node_1);
        mg.add_node(node_2);
        mg.add_node(node_3);
        assert_eq!(mg.node_count(), 3);

        let removed = mg.remove_nodes_by_file("src/merge.rs");
        assert_eq!(removed.len(), 2);
        assert_eq!(mg.node_count(), 1);
        assert_eq!(mg.find_nodes_by_file("src/merge.rs").len(), 0);
        assert_eq!(mg.find_nodes_by_file("src/commit.rs").len(), 1);

        mg.validate_integrity().expect("Graph integrity failed");
    }

    #[test]
    fn test_memory_graph_stats() {
        let mut mg = MemoryGraph::new();
        mg.add_node(sample_node("rusty", "src/a.rs", "foo", NodeKind::Function));
        mg.add_node(sample_node("rusty", "src/a.rs", "Bar", NodeKind::Struct));
        mg.add_node(sample_node("rusty", "src/b.rs", "baz", NodeKind::Function));

        let stats = mg.stats();
        assert_eq!(stats.total_nodes, 3);
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_callables, 2);
        assert_eq!(stats.total_types, 1);
    }
}
