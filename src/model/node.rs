use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::uri::{SymbolUri, UriParseError};

/// Categorization of all recognized source code and architectural entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    // Structural / Organizational
    Repository,
    Directory,
    File,
    Module,

    // Type Definitions
    Class,
    Struct,
    Enum,
    EnumVariant,
    Interface,
    Trait,
    TypeAlias,

    // Executable Code Entities
    Function,
    Method,
    Constructor,
    Closure,
    Macro,

    // Variables & Attributes
    Field,
    Property,
    GlobalVariable,
    Constant,
}

impl NodeKind {
    /// Returns true if this entity represents a callable / executable block.
    pub fn is_callable(&self) -> bool {
        matches!(
            self,
            NodeKind::Function
                | NodeKind::Method
                | NodeKind::Constructor
                | NodeKind::Closure
                | NodeKind::Macro
        )
    }

    /// Returns true if this entity represents a type definition.
    pub fn is_type_definition(&self) -> bool {
        matches!(
            self,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::TypeAlias
        )
    }

    /// Returns true if this entity is a container that naturally hosts other definitions.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            NodeKind::Repository
                | NodeKind::Directory
                | NodeKind::File
                | NodeKind::Module
                | NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Trait
                | NodeKind::Interface
        )
    }

    /// Returns a short canonical suffix used in SymbolUri generation (e.g., "fn", "struct").
    pub fn default_uri_suffix(&self) -> &'static str {
        match self {
            NodeKind::Repository => "repo",
            NodeKind::Directory => "dir",
            NodeKind::File => "file",
            NodeKind::Module => "mod",
            NodeKind::Class => "class",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            NodeKind::EnumVariant => "var",
            NodeKind::Interface => "iface",
            NodeKind::Trait => "trait",
            NodeKind::TypeAlias => "type",
            NodeKind::Function => "fn",
            NodeKind::Method => "fn",
            NodeKind::Constructor => "ctor",
            NodeKind::Closure => "closure",
            NodeKind::Macro => "macro",
            NodeKind::Field => "field",
            NodeKind::Property => "prop",
            NodeKind::GlobalVariable => "var",
            NodeKind::Constant => "const",
        }
    }
}

/// The visibility or access modifier of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Visibility {
    #[default]
    Public,
    Crate,
    ModulePrivate,
    Private,
    Protected,
    Internal,
}

/// Source code location span (line and byte boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub byte_start: usize,
    pub byte_end: usize,
}

impl SourceSpan {
    pub fn new(
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
        byte_start: usize,
        byte_end: usize,
    ) -> Self {
        Self {
            start_line,
            start_col,
            end_line,
            end_col,
            byte_start,
            byte_end,
        }
    }

    /// Point span for whole-file or synthetic nodes.
    pub fn zero() -> Self {
        Self {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 0,
            byte_start: 0,
            byte_end: 0,
        }
    }

    /// Number of lines spanned by this construct.
    pub fn line_count(&self) -> u32 {
        self.end_line
            .saturating_sub(self.start_line)
            .saturating_add(1)
    }
}

/// Historical VCS and commit overlay data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VcsMetadata {
    pub created_in_commit: Option<String>,
    pub last_modified_commit: Option<String>,
    pub last_modified_by: Option<String>,
    pub churn_count: u32,
    pub last_modified_timestamp: Option<i64>,
}

/// Errors raised when a node's redundant identity fields disagree with its URI.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NodeError {
    #[error(transparent)]
    InvalidUri(#[from] UriParseError),
    #[error("node name '{actual}' does not match URI name '{expected}'")]
    NameMismatch { expected: String, actual: String },
    #[error("node file path '{actual}' does not match URI path '{expected}'")]
    FilePathMismatch { expected: String, actual: String },
    #[error("node kind '{kind:?}' does not match URI kind suffix '{suffix}'")]
    KindMismatch { kind: NodeKind, suffix: String },
}

/// The central node representing an entity in the code knowledge graph.
///
/// Identity fields (`uri`, `name`, `file_path`, and `kind`) are private and can
/// only be populated through `try_new` or Serde deserialization, both of which
/// verify that the redundant fields agree with the canonical `SymbolUri`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CodeEntityNodeWire")]
pub struct CodeEntityNode {
    uri: SymbolUri,
    name: String,
    kind: NodeKind,
    file_path: String,
    language: String,
    visibility: Visibility,
    span: SourceSpan,
    signature: Option<String>,
    docstring: Option<String>,
    vcs: Option<VcsMetadata>,
    complexity: Option<u32>,
}

impl CodeEntityNode {
    /// Construct a new `CodeEntityNode` after validating URI identity consistency.
    pub fn try_new(
        uri: SymbolUri,
        name: impl Into<String>,
        kind: NodeKind,
        file_path: impl Into<String>,
        language: impl Into<String>,
        span: SourceSpan,
    ) -> Result<Self, NodeError> {
        let node = Self {
            uri,
            name: name.into(),
            kind,
            file_path: file_path.into(),
            language: language.into(),
            visibility: Visibility::default(),
            span,
            signature: None,
            docstring: None,
            vcs: None,
            complexity: None,
        };

        node.validate()?;
        Ok(node)
    }

    /// Verify that the URI and the redundant identity fields are consistent.
    pub fn validate(&self) -> Result<(), NodeError> {
        let parsed = self.uri.parse()?;

        if parsed.name != self.name {
            return Err(NodeError::NameMismatch {
                expected: parsed.name,
                actual: self.name.clone(),
            });
        }

        if parsed.file_path != self.file_path {
            return Err(NodeError::FilePathMismatch {
                expected: parsed.file_path,
                actual: self.file_path.clone(),
            });
        }

        if parsed.kind_suffix != self.kind.default_uri_suffix() {
            return Err(NodeError::KindMismatch {
                kind: self.kind,
                suffix: parsed.kind_suffix,
            });
        }

        Ok(())
    }

    pub fn uri(&self) -> &SymbolUri {
        &self.uri
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    pub fn vcs(&self) -> Option<&VcsMetadata> {
        self.vcs.as_ref()
    }

    pub fn complexity(&self) -> Option<u32> {
        self.complexity
    }

    /// Builder: attach signature.
    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    /// Builder: attach docstring.
    pub fn with_docstring(mut self, doc: impl Into<String>) -> Self {
        self.docstring = Some(doc.into());
        self
    }

    /// Builder: set visibility.
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Builder: attach VCS metadata.
    pub fn with_vcs(mut self, vcs: VcsMetadata) -> Self {
        self.vcs = Some(vcs);
        self
    }

    /// Builder: attach complexity.
    pub fn with_complexity(mut self, complexity: u32) -> Self {
        self.complexity = Some(complexity);
        self
    }
}

/// Wire representation used by Serde. Validation happens in `TryFrom` below so
/// deserialization cannot bypass `CodeEntityNode::validate`.
#[derive(Deserialize)]
struct CodeEntityNodeWire {
    uri: SymbolUri,
    name: String,
    kind: NodeKind,
    file_path: String,
    language: String,
    visibility: Visibility,
    span: SourceSpan,
    signature: Option<String>,
    docstring: Option<String>,
    vcs: Option<VcsMetadata>,
    complexity: Option<u32>,
}

impl TryFrom<CodeEntityNodeWire> for CodeEntityNode {
    type Error = NodeError;

    fn try_from(wire: CodeEntityNodeWire) -> Result<Self, Self::Error> {
        let mut node = Self::try_new(
            wire.uri,
            wire.name,
            wire.kind,
            wire.file_path,
            wire.language,
            wire.span,
        )?;

        node.visibility = wire.visibility;
        node.signature = wire.signature;
        node.docstring = wire.docstring;
        node.vcs = wire.vcs;
        node.complexity = wire.complexity;

        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_creation_and_builders() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        let span = SourceSpan::new(10, 0, 25, 1, 200, 500);

        let node = CodeEntityNode::try_new(
            uri.clone(),
            "add_file",
            NodeKind::Function,
            "src/add.rs",
            "rust",
            span,
        )
        .unwrap()
        .with_signature("pub fn add_file(path: &Path, repo_path: &Path) -> Result<()>")
        .with_visibility(Visibility::Public)
        .with_complexity(4);

        assert_eq!(node.uri(), &uri);
        assert_eq!(node.name(), "add_file");
        assert_eq!(node.kind(), NodeKind::Function);
        assert!(node.kind().is_callable());
        assert_eq!(node.span().line_count(), 16);
        assert_eq!(node.complexity(), Some(4));
        assert_eq!(node.visibility(), Visibility::Public);
    }

    #[test]
    fn test_node_rejects_identity_mismatch() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();

        assert!(
            CodeEntityNode::try_new(
                uri.clone(),
                "different_name",
                NodeKind::Function,
                "src/add.rs",
                "rust",
                SourceSpan::zero(),
            )
            .is_err()
        );

        assert!(
            CodeEntityNode::try_new(
                uri.clone(),
                "add_file",
                NodeKind::Function,
                "src/other.rs",
                "rust",
                SourceSpan::zero(),
            )
            .is_err()
        );

        assert!(
            CodeEntityNode::try_new(
                uri,
                "add_file",
                NodeKind::Struct,
                "src/add.rs",
                "rust",
                SourceSpan::zero(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_node_serde_json_roundtrip() {
        let uri = SymbolUri::new("rusty", "src/index.rs", Some("Index"), "load", "fn").unwrap();
        let span = SourceSpan::new(45, 4, 60, 5, 1200, 1600);
        let node =
            CodeEntityNode::try_new(uri, "load", NodeKind::Method, "src/index.rs", "rust", span)
                .unwrap()
                .with_signature("pub fn load(repo_path: &Path) -> Result<Self>")
                .with_docstring("Load the repository staging index from disk.");

        let serialized = serde_json::to_string(&node).expect("JSON serialization failed");
        let deserialized: CodeEntityNode =
            serde_json::from_str(&serialized).expect("JSON deserialization failed");

        assert_eq!(node, deserialized);
    }

    #[test]
    fn test_node_deserialization_rejects_identity_mismatch() {
        let uri = SymbolUri::new("rusty", "src/index.rs", None, "load", "fn").unwrap();
        let node = CodeEntityNode::try_new(
            uri,
            "load",
            NodeKind::Function,
            "src/index.rs",
            "rust",
            SourceSpan::zero(),
        )
        .unwrap();

        let mut value = serde_json::to_value(&node).unwrap();
        value["name"] = serde_json::Value::String("other".to_string());

        assert!(serde_json::from_value::<CodeEntityNode>(value).is_err());
    }

    #[test]
    fn test_node_bincode_roundtrip() {
        let uri = SymbolUri::new("rusty", "src/commit.rs", None, "Commit", "struct").unwrap();
        let span = SourceSpan::new(1, 0, 20, 1, 0, 450);
        let node = CodeEntityNode::try_new(
            uri,
            "Commit",
            NodeKind::Struct,
            "src/commit.rs",
            "rust",
            span,
        )
        .unwrap();

        let bytes = bincode::serialize(&node).expect("bincode serialization failed");
        let deserialized: CodeEntityNode =
            bincode::deserialize(&bytes).expect("bincode deserialization failed");

        assert_eq!(node, deserialized);
    }
}
