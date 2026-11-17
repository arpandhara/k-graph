//! Two-pass symbol resolution for the code knowledge graph.
//!
//! Parsing stops at syntactic facts: a file yields validated nodes, same-file
//! containment edges, unresolved [`crate::parser::PendingCallSite`] values, and
//! normalized [`crate::parser::ImportRecord`] values. This module turns those
//! pending calls into cross-file `Calls` edges without introducing Tree-Sitter
//! types or mutating node identity.
//!
//! # Passes
//!
//! 1. **Local scope.** [`ScopeIndex`] snapshots every definition by file and
//!    lexical scope. A call is first checked against definitions in the
//!    caller's own file, preferring the deepest enclosing scope. This resolves
//!    nested functions, closures, and same-file helpers without repository-wide
//!    lookups.
//! 2. **Cross-file linking.** Remaining calls are resolved through explicit
//!    import bindings, receiver-type method lookup, then the global name index
//!    with deterministic proximity ranking from [`heuristics`].
//!
//! Calls that cannot be linked are reported in
//! [`ResolutionReport::unresolved`] with a typed
//! [`UnresolvedReason`]; no speculative edges are added.
//!
//! # Determinism
//!
//! Pending calls are sorted before resolution, candidates are sorted by
//! canonical URI, tied heuristic scores are reported as ambiguous instead of
//! being broken arbitrarily, and repeated runs upsert the same graph edges.
//! Two runs over the same graph and parser output therefore produce the same
//! report and the same edge set.

mod error;
pub mod heuristics;
pub mod linker;
pub mod scope;

pub use error::ResolverError;
pub use heuristics::{ModuleMatch, RankedCandidate};
pub use linker::{
    Linker, ResolutionConfig, ResolutionReport, ResolutionStrategy, ResolvedCall, UnresolvedCall,
    UnresolvedReason,
};
pub use scope::{CallSeparator, CallShape, Candidate, ScopeChain, ScopeIndex};
