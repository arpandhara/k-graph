//! # Code Knowledge Graph (`kg`)
//!
//! An ultra-fast, zero-daemon, deterministic code intelligence engine designed to
//! parse, index, and query multi-language codebases at structural and semantic levels.

pub mod graph;
pub mod model;
pub mod parser;
pub mod persistence;

// Re-export core abstractions
pub use graph::{GraphEdge, GraphError, GraphStats, MemoryGraph};
pub use model::{
    CodeEdge, CodeEntityNode, Confidence, EdgeError, EdgeKind, NodeError, NodeKind,
    ParsedSymbolUri, SourceSpan, SpanError, SymbolUri, UriParseError, VcsMetadata, Visibility,
};
pub use parser::{
    GoAdapter, ImportRecord, LanguageAdapter, LanguageRegistry, LanguageSyntax, ParserError,
    PendingCallSite, PythonAdapter, QuerySet, RawExtractedFile, RustAdapter, TreeSitterAdapter,
    TypeScriptAdapter,
};
pub use persistence::{
    CodecError, MAX_RECORD_BYTES, SCHEMA_VERSION, ValidateRecord, decode_record, encode_record,
};
