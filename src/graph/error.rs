use petgraph::stable_graph::NodeIndex;
use thiserror::Error;

use crate::model::{NodeError, SymbolUri};

/// Errors raised by graph mutation operations.
#[derive(Debug, Error)]
pub enum GraphError {
    #[error(transparent)]
    InvalidNode(#[from] NodeError),
    #[error("node index {0:?} is not present in the graph")]
    MissingNode(NodeIndex),
    #[error("no node exists for URI '{0}'")]
    UnknownNode(SymbolUri),
    #[error("edge endpoints do not match the edge's source/target URIs")]
    EdgeEndpointMismatch,
    #[error("cannot update node in place: its identity fields differ from the existing URI")]
    IdentityMismatch,
}
