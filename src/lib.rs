//! # Code Knowledge Graph (`kg`)
//!
//! An ultra-fast, zero-daemon, deterministic code intelligence engine designed to
//! parse, index, and query multi-language codebases at structural and semantic levels.

pub mod graph;
pub mod model;
pub mod parser;
pub mod persistence;
pub mod resolver;
pub mod storage;

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
pub use resolver::{
    CallSeparator, CallShape, Candidate, Linker, ModuleMatch, RankedCandidate, ResolutionConfig,
    ResolutionReport, ResolutionStrategy, ResolvedCall, ResolverError, ScopeChain, ScopeIndex,
    UnresolvedCall, UnresolvedReason,
};
pub use storage::{
    FileIndex, GraphStore, META_LAST_COMMIT, META_REPO_NAME, META_SCHEMA_VERSION,
    STORAGE_SCHEMA_VERSION, StorageError, StoreReport, WriteBatch, WriteReport, edge_key,
};
