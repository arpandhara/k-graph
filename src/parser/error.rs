use thiserror::Error;

use crate::model::{NodeError, SpanError, UriParseError};

/// Errors raised by the Tree-Sitter ingestion layer.
///
/// The parser is a library boundary, so failures are typed rather than routed
/// through `anyhow`. A malformed *definition* inside a source file is skipped
/// (Tree-Sitter is error tolerant), but failures that prevent the file from
/// being indexed at all (bad grammar, bad query, invalid repository/file path)
/// are reported here.
#[derive(Debug, Error)]
pub enum ParserError {
    #[error("failed to load the {language} Tree-Sitter grammar: {message}")]
    Language {
        language: &'static str,
        message: String,
    },

    #[error("failed to compile the {language} {query} query: {message}")]
    Query {
        language: &'static str,
        query: &'static str,
        message: String,
    },

    #[error("Tree-Sitter could not parse '{path}'")]
    ParseFailed { path: String },

    #[error("capture '{capture}' is missing from the {language} query")]
    MissingCapture {
        language: &'static str,
        capture: &'static str,
    },

    #[error("source text in bytes {start}..{end} is not valid UTF-8")]
    InvalidUtf8 { start: usize, end: usize },

    #[error(transparent)]
    InvalidUri(#[from] UriParseError),

    #[error(transparent)]
    InvalidNode(#[from] NodeError),

    #[error(transparent)]
    InvalidSpan(#[from] SpanError),
}
