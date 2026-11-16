//! Tree-Sitter language ingestion.
//!
//! The parser turns one source file into a [`RawExtractedFile`]: validated
//! [`crate::model::CodeEntityNode`] definitions, same-file containment edges,
//! unresolved call sites, and normalized import records. Cross-file resolution
//! is the job of a later phase; this module only records syntactic facts.
//!
//! Language support is split into three layers:
//!
//! - [`language`] defines the adapter contract and the registry of built-in
//!   languages;
//! - [`tree_sitter_adapter`] is the shared, grammar-agnostic Tree-Sitter
//!   executor driven by the declarative queries under `queries/`;
//! - [`rust`], [`python`], [`typescript`], and [`go`] supply the small amount
//!   of language-specific behaviour that queries cannot express.

pub mod error;
pub mod go;
pub mod language;
pub mod python;
pub mod rust;
pub mod tree_sitter_adapter;
pub mod typescript;

pub use error::ParserError;
pub use go::GoAdapter;
pub use language::{
    ImportRecord, LanguageAdapter, LanguageRegistry, PendingCallSite, RawExtractedFile,
};
pub use python::PythonAdapter;
pub use rust::RustAdapter;
pub use tree_sitter_adapter::{LanguageSyntax, QuerySet, TreeSitterAdapter};
pub use typescript::TypeScriptAdapter;
