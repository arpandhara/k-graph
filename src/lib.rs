//! # Code Knowledge Graph (`kg`)
//!
//! An ultra-fast, zero-daemon, deterministic code intelligence engine designed to
//! parse, index, and query multi-language codebases at structural and semantic levels.

pub mod graph;
pub mod model;

// Re-export core abstractions
pub use graph::{GraphStats, MemoryGraph};
pub use model::{
    CodeEdge, CodeEntityNode, EdgeKind, NodeKind, ParsedSymbolUri, SourceSpan, SymbolUri,
    UriParseError, VcsMetadata, Visibility,
};
