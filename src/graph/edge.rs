use petgraph::stable_graph::NodeIndex;

use crate::model::{CodeEdge, Confidence, EdgeKind, SourceSpan};

/// In-memory edge weight stored in [`super::MemoryGraph`].
///
/// The domain type [`CodeEdge`] carries two full `SymbolUri` values because it
/// is also the persistence record. Inside the graph those URIs are redundant:
/// petgraph already stores the endpoints, and their identities are available
/// from the vertex weights. Dropping the URI strings here is the memory fix for
/// audit finding KG-06.
///
/// The endpoint indices are kept as well so an edge can be inspected without
/// asking petgraph for its endpoints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphEdge {
    source: NodeIndex,
    target: NodeIndex,
    kind: EdgeKind,
    confidence: Confidence,
    call_site: Option<SourceSpan>,
}

impl GraphEdge {
    /// Build the graph weight for a domain edge at the resolved endpoints.
    pub(crate) fn from_code_edge(edge: &CodeEdge, source: NodeIndex, target: NodeIndex) -> Self {
        Self {
            source,
            target,
            kind: edge.kind(),
            confidence: edge.confidence(),
            call_site: edge.call_site(),
        }
    }

    pub fn source(&self) -> NodeIndex {
        self.source
    }

    pub fn target(&self) -> NodeIndex {
        self.target
    }

    pub fn kind(&self) -> EdgeKind {
        self.kind
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn call_site(&self) -> Option<SourceSpan> {
        self.call_site
    }
}
