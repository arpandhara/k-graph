use crate::model::{CodeEdge, CodeEntityNode};

use super::error::ParserError;
use super::go::GoAdapter;
use super::python::PythonAdapter;
use super::rust::RustAdapter;
use super::typescript::TypeScriptAdapter;

/// Everything a language parser extracts from one source file.
///
/// The parser deliberately stops at *syntactic* facts. Calls and imports are
/// recorded as unresolved tokens; the Phase 3 resolver turns them into
/// cross-file edges. `local_edges` therefore contains only relationships whose
/// both endpoints are defined in the same file (file/type containment).
#[derive(Debug, Clone, PartialEq)]
pub struct RawExtractedFile {
    pub nodes: Vec<CodeEntityNode>,
    pub local_edges: Vec<CodeEdge>,
    pub pending_calls: Vec<PendingCallSite>,
    pub imports: Vec<ImportRecord>,
}

impl RawExtractedFile {
    /// Create an empty extraction result.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            local_edges: Vec::new(),
            pending_calls: Vec::new(),
            imports: Vec::new(),
        }
    }

    /// True when the file produced neither nodes nor relationships.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.local_edges.is_empty()
            && self.pending_calls.is_empty()
            && self.imports.is_empty()
    }
}

impl Default for RawExtractedFile {
    fn default() -> Self {
        Self::new()
    }
}

/// A call expression observed in a callable body, waiting for symbol resolution.
///
/// `call_expr` is the callee text with whitespace collapsed: a bare name
/// (`save`), a receiver chain (`record.save`), or a scoped path
/// (`Index::load`). Arguments are not included. `line` is 1-based and `col` is
/// the 0-based byte column reported by Tree-Sitter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingCallSite {
    pub caller_uri: String,
    pub call_expr: String,
    pub line: u32,
    pub col: u32,
}

/// A symbol imported or re-exported by a file.
///
/// Language-specific import syntax is normalized into this shape:
/// - `source_path` is the module path as written (`crate::db`, `storage`,
///   `./utils`), not a resolved filesystem path;
/// - `imported_symbol` is the imported name (`save`) or `*` for a module/glob;
/// - `alias` is the local binding when one exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportRecord {
    pub source_path: String,
    pub imported_symbol: String,
    pub alias: Option<String>,
}

/// Contract implemented by every language ingestion backend.
pub trait LanguageAdapter: Send + Sync {
    /// Canonical language name recorded on `CodeEntityNode::language`.
    fn language_name(&self) -> &'static str;

    /// Whether this adapter handles a file extension (with or without a dot).
    fn can_handle(&self, extension: &str) -> bool;

    /// Parse one file into local nodes, containment edges, calls, and imports.
    fn parse_file(
        &self,
        repo_name: &str,
        file_path: &str,
        source: &str,
    ) -> Result<RawExtractedFile, ParserError>;
}

/// Registry of the built-in language adapters.
///
/// Constructing the registry compiles the declarative Tree-Sitter queries once;
/// the resulting adapters are immutable and safe to share across Rayon workers.
pub struct LanguageRegistry {
    adapters: Vec<Box<dyn LanguageAdapter>>,
}

impl LanguageRegistry {
    /// Build the default registry (Rust, Python, TypeScript/TSX, Go).
    pub fn new() -> Result<Self, ParserError> {
        Ok(Self {
            adapters: vec![
                Box::new(RustAdapter::new()?),
                Box::new(PythonAdapter::new()?),
                Box::new(TypeScriptAdapter::new()?),
                Box::new(GoAdapter::new()?),
            ],
        })
    }

    /// All registered adapters.
    pub fn adapters(&self) -> &[Box<dyn LanguageAdapter>] {
        &self.adapters
    }

    /// Look up an adapter by file extension (with or without a leading dot).
    pub fn adapter_for_extension(&self, extension: &str) -> Option<&dyn LanguageAdapter> {
        self.adapters
            .iter()
            .map(Box::as_ref)
            .find(|adapter| adapter.can_handle(extension))
    }

    /// Look up an adapter by file path.
    pub fn adapter_for_path(&self, path: &str) -> Option<&dyn LanguageAdapter> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())?;
        self.adapter_for_extension(extension)
    }

    /// Look up an adapter by canonical language name.
    pub fn adapter_for_language(&self, language: &str) -> Option<&dyn LanguageAdapter> {
        self.adapters
            .iter()
            .map(Box::as_ref)
            .find(|adapter| adapter.language_name() == language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;

    #[test]
    fn test_registry_dispatches_by_extension_and_language() {
        let registry = LanguageRegistry::new().unwrap();

        assert_eq!(
            registry
                .adapter_for_extension(".RS")
                .map(|a| a.language_name()),
            Some("rust")
        );
        assert_eq!(
            registry
                .adapter_for_path("src/tool.PY")
                .map(|a| a.language_name()),
            Some("python")
        );
        assert_eq!(
            registry
                .adapter_for_path("web/App.tsx")
                .map(|a| a.language_name()),
            Some("typescript")
        );
        assert_eq!(
            registry
                .adapter_for_language("typescript")
                .map(|a| a.language_name()),
            Some("typescript")
        );
        assert_eq!(
            registry
                .adapter_for_extension("go")
                .map(|a| a.language_name()),
            Some("go")
        );
        assert_eq!(
            registry
                .adapter_for_path("cmd/server/main.GO")
                .map(|a| a.language_name()),
            Some("go")
        );
        assert!(registry.adapter_for_path("Makefile").is_none());
    }

    #[test]
    fn test_empty_file_still_produces_a_file_node() {
        let registry = LanguageRegistry::new().unwrap();
        let adapter = registry.adapter_for_extension("rs").unwrap();
        let extraction = adapter.parse_file("repo", "src/empty.rs", "").unwrap();

        assert_eq!(extraction.nodes.len(), 1);
        assert_eq!(extraction.nodes[0].kind(), NodeKind::File);
        assert_eq!(
            extraction.nodes[0].uri().as_str(),
            "sym://repo/src/empty.rs#file:file"
        );
        assert!(extraction.local_edges.is_empty());
        assert!(extraction.pending_calls.is_empty());
        assert!(extraction.imports.is_empty());
    }

    #[test]
    fn test_unrepresentable_file_path_fails_the_whole_file() {
        let registry = LanguageRegistry::new().unwrap();
        let adapter = registry.adapter_for_extension("py").unwrap();

        assert!(matches!(
            adapter.parse_file("repo", "../escape.py", "x = 1"),
            Err(ParserError::InvalidUri(_))
        ));
    }
}
