use thiserror::Error;

use crate::graph::GraphError;
use crate::model::{EdgeError, SpanError};

/// Errors raised by the symbol resolution engine.
///
/// Most resolution failures are *not* errors: a call that cannot be linked is
/// reported as an [`super::UnresolvedCall`]. This type only covers failures
/// that make the graph itself inconsistent, such as adding an edge whose
/// endpoints disappeared or building an invalid call-site span.
#[derive(Debug, Error)]
pub enum ResolverError {
    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error(transparent)]
    Edge(#[from] EdgeError),

    #[error(transparent)]
    Span(#[from] SpanError),
}
