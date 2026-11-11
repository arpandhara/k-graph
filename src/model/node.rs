use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::uri::{SymbolUri, UriParseError};

/// Categorization of all recognized source code and architectural entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// Stable numeric code used by binary persistence.
    ///
    /// These values are part of the persistence wire format and must never be
    /// reassigned; add new variants with new codes instead.
    pub fn stable_code(&self) -> u8 {
        match self {
            NodeKind::Repository => 0,
            NodeKind::Directory => 1,
            NodeKind::File => 2,
            NodeKind::Module => 3,
            NodeKind::Class => 4,
            NodeKind::Struct => 5,
            NodeKind::Enum => 6,
            NodeKind::EnumVariant => 7,
            NodeKind::Interface => 8,
            NodeKind::Trait => 9,
            NodeKind::TypeAlias => 10,
            NodeKind::Function => 11,
            NodeKind::Method => 12,
            NodeKind::Constructor => 13,
            NodeKind::Closure => 14,
            NodeKind::Macro => 15,
            NodeKind::Field => 16,
            NodeKind::Property => 17,
            NodeKind::GlobalVariable => 18,
            NodeKind::Constant => 19,
        }
    }

    /// Reconstruct a variant from its stable persistence code.
    pub fn from_stable_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => NodeKind::Repository,
            1 => NodeKind::Directory,
            2 => NodeKind::File,
            3 => NodeKind::Module,
            4 => NodeKind::Class,
            5 => NodeKind::Struct,
            6 => NodeKind::Enum,
            7 => NodeKind::EnumVariant,
            8 => NodeKind::Interface,
            9 => NodeKind::Trait,
            10 => NodeKind::TypeAlias,
            11 => NodeKind::Function,
            12 => NodeKind::Method,
            13 => NodeKind::Constructor,
            14 => NodeKind::Closure,
            15 => NodeKind::Macro,
            16 => NodeKind::Field,
            17 => NodeKind::Property,
            18 => NodeKind::GlobalVariable,
            19 => NodeKind::Constant,
            _ => return None,
        })
    }

    /// Stable string code used by human-readable persistence.
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Repository => "repository",
            NodeKind::Directory => "directory",
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Class => "class",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            NodeKind::EnumVariant => "enum_variant",
            NodeKind::Interface => "interface",
            NodeKind::Trait => "trait",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Constructor => "constructor",
            NodeKind::Closure => "closure",
            NodeKind::Macro => "macro",
            NodeKind::Field => "field",
            NodeKind::Property => "property",
            NodeKind::GlobalVariable => "global_variable",
            NodeKind::Constant => "constant",
        }
    }

    /// Reconstruct a variant from its stable string code.
    pub fn from_wire_str(value: &str) -> Option<Self> {
        Some(match value {
            "repository" => NodeKind::Repository,
            "directory" => NodeKind::Directory,
            "file" => NodeKind::File,
            "module" => NodeKind::Module,
            "class" => NodeKind::Class,
            "struct" => NodeKind::Struct,
            "enum" => NodeKind::Enum,
            "enum_variant" => NodeKind::EnumVariant,
            "interface" => NodeKind::Interface,
            "trait" => NodeKind::Trait,
            "type_alias" => NodeKind::TypeAlias,
            "function" => NodeKind::Function,
            "method" => NodeKind::Method,
            "constructor" => NodeKind::Constructor,
            "closure" => NodeKind::Closure,
            "macro" => NodeKind::Macro,
            "field" => NodeKind::Field,
            "property" => NodeKind::Property,
            "global_variable" => NodeKind::GlobalVariable,
            "constant" => NodeKind::Constant,
            _ => return None,
        })
    }
}

impl Serialize for NodeKind {
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

impl<'de> Deserialize<'de> for NodeKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let raw = String::deserialize(deserializer)?;
            Self::from_wire_str(&raw)
                .ok_or_else(|| D::Error::custom(format!("unknown NodeKind '{raw}'")))
        } else {
            let code = u8::deserialize(deserializer)?;
            Self::from_stable_code(code)
                .ok_or_else(|| D::Error::custom(format!("unknown NodeKind code {code}")))
        }
    }
}

/// The visibility or access modifier of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Visibility {
    #[default]
    Public,
    Crate,
    ModulePrivate,
    Private,
    Protected,
    Internal,
}

impl Visibility {
    /// Stable numeric code used by binary persistence.
    pub fn stable_code(&self) -> u8 {
        match self {
            Visibility::Public => 0,
            Visibility::Crate => 1,
            Visibility::ModulePrivate => 2,
            Visibility::Private => 3,
            Visibility::Protected => 4,
            Visibility::Internal => 5,
        }
    }

    /// Reconstruct a variant from its stable persistence code.
    pub fn from_stable_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Visibility::Public,
            1 => Visibility::Crate,
            2 => Visibility::ModulePrivate,
            3 => Visibility::Private,
            4 => Visibility::Protected,
            5 => Visibility::Internal,
            _ => return None,
        })
    }

    /// Stable string code used by human-readable persistence.
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Crate => "crate",
            Visibility::ModulePrivate => "module_private",
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Internal => "internal",
        }
    }

    /// Reconstruct a variant from its stable string code.
    pub fn from_wire_str(value: &str) -> Option<Self> {
        Some(match value {
            "public" => Visibility::Public,
            "crate" => Visibility::Crate,
            "module_private" => Visibility::ModulePrivate,
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            "internal" => Visibility::Internal,
            _ => return None,
        })
    }
}

impl Serialize for Visibility {
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

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let raw = String::deserialize(deserializer)?;
            Self::from_wire_str(&raw)
                .ok_or_else(|| D::Error::custom(format!("unknown Visibility '{raw}'")))
        } else {
            let code = u8::deserialize(deserializer)?;
            Self::from_stable_code(code)
                .ok_or_else(|| D::Error::custom(format!("unknown Visibility code {code}")))
        }
    }
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
    fn test_node_record_roundtrip() {
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

        let bytes = crate::persistence::encode_record(&node).expect("record encoding failed");
        let deserialized: CodeEntityNode =
            crate::persistence::decode_record(&bytes).expect("record decoding failed");

        assert_eq!(node, deserialized);
    }
}
