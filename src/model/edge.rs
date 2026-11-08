use super::node::SourceSpan;
use super::uri::SymbolUri;
use serde::{Deserialize, Serialize};

/// Categorization of relationships between code entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    // Structural / Containment
    Contains,
    MemberOf,

    // Invocation & Execution
    Calls,
    Instantiates,

    // Data Access
    Reads,
    Writes,

    // Type Semantics
    Implements,
    Extends,
    ReturnsType,
    AcceptsType,
    TypedAs,

    // Module / Dependency Flow
    Imports,
    ReExports,
    DependsOn,

    // VCS Temporal Association
    CoChangedWith,
}

impl EdgeKind {
    /// True if this edge represents hierarchical syntactic containment.
    pub fn is_structural(&self) -> bool {
        matches!(self, EdgeKind::Contains | EdgeKind::MemberOf)
    }

    /// True if this edge represents a function or method call or construction.
    pub fn is_invocation(&self) -> bool {
        matches!(self, EdgeKind::Calls | EdgeKind::Instantiates)
    }

    /// True if this edge connects an entity to a type.
    pub fn is_type_relation(&self) -> bool {
        matches!(
            self,
            EdgeKind::Implements
                | EdgeKind::Extends
                | EdgeKind::ReturnsType
                | EdgeKind::AcceptsType
                | EdgeKind::TypedAs
        )
    }

    /// Canonical string identifier for serializers.
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::MemberOf => "MEMBER_OF",
            EdgeKind::Calls => "CALLS",
            EdgeKind::Instantiates => "INSTANTIATES",
            EdgeKind::Reads => "READS",
            EdgeKind::Writes => "WRITES",
            EdgeKind::Implements => "IMPLEMENTS",
            EdgeKind::Extends => "EXTENDS",
            EdgeKind::ReturnsType => "RETURNS_TYPE",
            EdgeKind::AcceptsType => "ACCEPTS_TYPE",
            EdgeKind::TypedAs => "TYPED_AS",
            EdgeKind::Imports => "IMPORTS",
            EdgeKind::ReExports => "RE_EXPORTS",
            EdgeKind::DependsOn => "DEPENDS_ON",
            EdgeKind::CoChangedWith => "CO_CHANGED_WITH",
        }
    }
}

/// A directed edge connecting two entities in the knowledge graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEdge {
    pub source_uri: SymbolUri,
    pub target_uri: SymbolUri,
    pub kind: EdgeKind,
    pub confidence: f32,
    pub call_site: Option<SourceSpan>,
}

impl CodeEdge {
    /// Create a new edge with default confidence (1.0) and no specific call site.
    pub fn new(source_uri: SymbolUri, target_uri: SymbolUri, kind: EdgeKind) -> Self {
        Self {
            source_uri,
            target_uri,
            kind,
            confidence: 1.0,
            call_site: None,
        }
    }

    /// Builder: assign confidence score (between 0.0 and 1.0).
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// Builder: attach the source span where this relationship occurred.
    pub fn with_call_site(mut self, span: SourceSpan) -> Self {
        self.call_site = Some(span);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_creation_and_properties() {
        let src = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        let dst = SymbolUri::new("rusty", "src/index.rs", Some("Index"), "load", "fn").unwrap();

        let edge = CodeEdge::new(src.clone(), dst.clone(), EdgeKind::Calls)
            .with_confidence(0.9)
            .with_call_site(SourceSpan::new(12, 18, 12, 33, 250, 265));

        assert_eq!(edge.source_uri, src);
        assert_eq!(edge.target_uri, dst);
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert!(edge.kind.is_invocation());
        assert!(!edge.kind.is_structural());
        assert_eq!(edge.confidence, 0.9);
        assert!(edge.call_site.is_some());
    }

    #[test]
    fn test_edge_serde_json() {
        let src = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        let dst = SymbolUri::new("rusty", "src/index.rs", None, "Index", "struct").unwrap();
        let edge = CodeEdge::new(src, dst, EdgeKind::TypedAs);

        let json = serde_json::to_string(&edge).unwrap();
        let decoded: CodeEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(edge, decoded);
    }

    #[test]
    fn test_edge_bincode_roundtrip() {
        let src = SymbolUri::new("rusty", "src/lib.rs", None, "lib", "file").unwrap();
        let dst = SymbolUri::new("rusty", "src/add.rs", None, "add", "file").unwrap();
        let edge = CodeEdge::new(src, dst, EdgeKind::DependsOn);

        let bytes = bincode::serialize(&edge).unwrap();
        let decoded: CodeEdge = bincode::deserialize(&bytes).unwrap();
        assert_eq!(edge, decoded);
    }
}
