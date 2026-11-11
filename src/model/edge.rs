use super::node::SourceSpan;
use super::uri::SymbolUri;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Categorization of relationships between code entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// Stable numeric code used by binary persistence.
    ///
    /// These values are part of the persistence wire format and must never be
    /// reassigned; add new variants with new codes instead.
    pub fn stable_code(&self) -> u8 {
        match self {
            EdgeKind::Contains => 0,
            EdgeKind::MemberOf => 1,
            EdgeKind::Calls => 2,
            EdgeKind::Instantiates => 3,
            EdgeKind::Reads => 4,
            EdgeKind::Writes => 5,
            EdgeKind::Implements => 6,
            EdgeKind::Extends => 7,
            EdgeKind::ReturnsType => 8,
            EdgeKind::AcceptsType => 9,
            EdgeKind::TypedAs => 10,
            EdgeKind::Imports => 11,
            EdgeKind::ReExports => 12,
            EdgeKind::DependsOn => 13,
            EdgeKind::CoChangedWith => 14,
        }
    }

    /// Reconstruct a variant from its stable persistence code.
    pub fn from_stable_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => EdgeKind::Contains,
            1 => EdgeKind::MemberOf,
            2 => EdgeKind::Calls,
            3 => EdgeKind::Instantiates,
            4 => EdgeKind::Reads,
            5 => EdgeKind::Writes,
            6 => EdgeKind::Implements,
            7 => EdgeKind::Extends,
            8 => EdgeKind::ReturnsType,
            9 => EdgeKind::AcceptsType,
            10 => EdgeKind::TypedAs,
            11 => EdgeKind::Imports,
            12 => EdgeKind::ReExports,
            13 => EdgeKind::DependsOn,
            14 => EdgeKind::CoChangedWith,
            _ => return None,
        })
    }

    /// Reconstruct a variant from its stable string code.
    pub fn from_wire_str(value: &str) -> Option<Self> {
        Some(match value {
            "CONTAINS" => EdgeKind::Contains,
            "MEMBER_OF" => EdgeKind::MemberOf,
            "CALLS" => EdgeKind::Calls,
            "INSTANTIATES" => EdgeKind::Instantiates,
            "READS" => EdgeKind::Reads,
            "WRITES" => EdgeKind::Writes,
            "IMPLEMENTS" => EdgeKind::Implements,
            "EXTENDS" => EdgeKind::Extends,
            "RETURNS_TYPE" => EdgeKind::ReturnsType,
            "ACCEPTS_TYPE" => EdgeKind::AcceptsType,
            "TYPED_AS" => EdgeKind::TypedAs,
            "IMPORTS" => EdgeKind::Imports,
            "RE_EXPORTS" => EdgeKind::ReExports,
            "DEPENDS_ON" => EdgeKind::DependsOn,
            "CO_CHANGED_WITH" => EdgeKind::CoChangedWith,
            _ => return None,
        })
    }
}

impl Serialize for EdgeKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(self.as_str())
        } else {
            serializer.serialize_u8(self.stable_code())
        }
    }
}

impl<'de> Deserialize<'de> for EdgeKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let raw = String::deserialize(deserializer)?;
            Self::from_wire_str(&raw)
                .ok_or_else(|| D::Error::custom(format!("unknown EdgeKind '{raw}'")))
        } else {
            let code = u8::deserialize(deserializer)?;
            Self::from_stable_code(code)
                .ok_or_else(|| D::Error::custom(format!("unknown EdgeKind code {code}")))
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
    fn test_edge_record_roundtrip() {
        let src = SymbolUri::new("rusty", "src/lib.rs", None, "lib", "file").unwrap();
        let dst = SymbolUri::new("rusty", "src/add.rs", None, "add", "file").unwrap();
        let edge = CodeEdge::new(src, dst, EdgeKind::DependsOn);

        let bytes = crate::persistence::encode_record(&edge).unwrap();
        let decoded: CodeEdge = crate::persistence::decode_record(&bytes).unwrap();
        assert_eq!(edge, decoded);
    }
}
