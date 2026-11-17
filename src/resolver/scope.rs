use std::collections::HashMap;

use crate::graph::MemoryGraph;
use crate::model::{CodeEntityNode, NodeKind, SymbolUri, UriParseError, Visibility};

/// The lexical scope components of a symbol, from outermost to innermost.
///
/// The components use the same `::` separator as [`SymbolUri`], so a node with
/// URI `sym://repo/src/db.rs#Store::load:fn` has the scope `["Store"]` and a
/// nested helper `#Store::load::helper:fn` has `["Store", "load"]`.
///
/// Phase 3 uses this chain for the first (local) resolution pass: a definition
/// is visible to a call when its scope is a prefix of the caller's scope. That
/// covers ordinary nested functions, closures, and methods on the same type
/// without requiring a full type system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ScopeChain {
    components: Vec<String>,
}

impl ScopeChain {
    /// Build a scope chain from already-split components.
    pub fn new(components: Vec<String>) -> Self {
        Self { components }
    }

    /// Build a scope chain from the `scope` component of a parsed URI.
    pub fn from_scope(scope: Option<&str>) -> Self {
        let components = scope
            .map(|scope| {
                scope
                    .split("::")
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self { components }
    }

    /// Build the scope chain of an existing symbol URI.
    pub fn from_uri(uri: &SymbolUri) -> Result<Self, UriParseError> {
        let parsed = uri.parse()?;
        Ok(Self::from_scope(parsed.scope.as_deref()))
    }

    /// Borrow the ordered components.
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// True when the symbol has no enclosing scope.
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// Number of lexical scope components.
    pub fn len(&self) -> usize {
        self.components.len()
    }

    /// Innermost scope component, if any.
    pub fn last(&self) -> Option<&str> {
        self.components.last().map(String::as_str)
    }

    /// True when every component of `self` is also the corresponding leading
    /// component of `other`. A chain is a prefix of itself.
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        self.components.len() <= other.components.len()
            && self
                .components
                .iter()
                .zip(other.components.iter())
                .all(|(left, right)| left == right)
    }

    /// Number of leading components shared with another chain.
    pub fn common_prefix_len(&self, other: &Self) -> usize {
        self.components
            .iter()
            .zip(other.components.iter())
            .take_while(|(left, right)| left == right)
            .count()
    }

    /// Drop the innermost component.
    pub fn parent(&self) -> Option<Self> {
        if self.components.is_empty() {
            return None;
        }
        let mut components = self.components.clone();
        components.pop();
        Some(Self { components })
    }

    /// Append one component.
    pub fn child(&self, name: &str) -> Self {
        let mut components = self.components.clone();
        components.push(name.to_string());
        Self { components }
    }

    /// Canonical `a::b` text, empty for the root scope.
    pub fn join(&self) -> String {
        self.components.join("::")
    }
}

/// The separator used by a call expression to attach a receiver or qualifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallSeparator {
    /// A bare name such as `save`.
    None,
    /// A member access such as `record.save` or `fmt.Println`.
    Dot,
    /// A path-qualified name such as `Index::load`.
    Path,
}

/// A structured view of a [`crate::parser::PendingCallSite::call_expr`].
///
/// The parser records the callee text verbatim (with whitespace collapsed).
/// The resolver needs the final name plus an optional qualifier. Generic
/// arguments and trailing array/index expressions are removed so the same
/// shape can be matched against graph names:
///
/// - `save` -> name `save`
/// - `agent.execute` -> qualifier `agent`, name `execute`
/// - `Index::load` -> qualifier `Index`, name `load`
/// - `Vec::<String>::new` -> qualifier `Vec`, name `new`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallShape {
    qualifier: Option<String>,
    name: String,
    separator: CallSeparator,
}

impl CallShape {
    /// Parse a call expression, returning `None` when it has no usable name.
    pub fn parse(expression: &str) -> Option<Self> {
        let compact: String = expression
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let mut text = remove_generics(&compact);

        // Macro calls and optional/handled call syntax are not identifiers.
        while text.ends_with(['!', '?', ';']) {
            text.pop();
        }

        // The parser normally stores only the callee, but tolerate `foo(...)`.
        if text.ends_with(')')
            && let Some(open) = matching_open_paren(&text)
        {
            text.truncate(open);
        }

        let text = text.trim().trim_start_matches("::");
        let mut text = text.trim_end_matches("::").to_string();
        if text.is_empty() {
            return None;
        }

        // A trailing index/array expression (`handlers[i]`) is not part of a
        // symbol name. Drop it before splitting the qualifier.
        if text.ends_with(']')
            && let Some(open) = text.rfind('[')
        {
            text.truncate(open);
        }
        if text.is_empty() {
            return None;
        }

        let path_index = text.rfind("::");
        let dot_index = text.rfind('.');
        let (separator, index) = match (path_index, dot_index) {
            (Some(path), Some(dot)) if path > dot => (CallSeparator::Path, path),
            (Some(_), Some(dot)) => (CallSeparator::Dot, dot),
            (Some(path), None) => (CallSeparator::Path, path),
            (None, Some(dot)) => (CallSeparator::Dot, dot),
            (None, None) => (CallSeparator::None, 0),
        };

        let (qualifier, name) = match separator {
            CallSeparator::None => (None, text.as_str()),
            CallSeparator::Dot => {
                let qualifier = text[..index].trim_matches('.');
                let name = &text[index + 1..];
                (Some(qualifier), name)
            }
            CallSeparator::Path => {
                let qualifier = text[..index].trim_matches(':');
                let name = &text[index + 2..];
                (Some(qualifier), name)
            }
        };

        let name = name.trim();
        if !is_valid_name(name) {
            return None;
        }

        let qualifier = qualifier
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string);

        Some(Self {
            qualifier,
            name: name.to_string(),
            separator,
        })
    }

    /// Receiver or path qualifier, when present.
    pub fn qualifier(&self) -> Option<&str> {
        self.qualifier.as_deref()
    }

    /// Final symbol name being called.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How the qualifier was attached.
    pub fn separator(&self) -> CallSeparator {
        self.separator
    }

    /// True when the expression is a bare name with no qualifier.
    pub fn is_bare(&self) -> bool {
        self.qualifier.is_none()
    }
}

/// A graph-node snapshot used by resolution passes.
///
/// Resolution runs against immutable snapshots so the graph can be mutated to
/// add edges without invalidating outstanding borrows. The snapshot is also
/// the unit ranked by [`super::heuristics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub uri: SymbolUri,
    pub name: String,
    pub file_path: String,
    pub scope: ScopeChain,
    pub kind: NodeKind,
    pub visibility: Visibility,
    pub language: String,
}

impl Candidate {
    /// Snapshot the identity and ranking-relevant fields of a graph node.
    pub fn from_node(node: &CodeEntityNode) -> Result<Self, UriParseError> {
        let parsed = node.uri().parse()?;
        Ok(Self {
            uri: node.uri().clone(),
            name: node.name().to_string(),
            file_path: node.file_path().to_string(),
            scope: ScopeChain::from_scope(parsed.scope.as_deref()),
            kind: node.kind(),
            visibility: node.visibility(),
            language: node.language().to_string(),
        })
    }

    /// True when the node can be the target of a `Calls` edge.
    pub fn is_callable(&self) -> bool {
        self.kind.is_callable()
    }

    /// True when the node names a type.
    pub fn is_type_definition(&self) -> bool {
        self.kind.is_type_definition()
    }

    /// Scope a member of this candidate would have (`scope::name`).
    pub fn qualified_scope(&self) -> ScopeChain {
        self.scope.child(&self.name)
    }

    /// Innermost scope component, which is the receiver type for a method.
    pub fn receiver_type(&self) -> Option<&str> {
        self.scope.last()
    }
}

#[derive(Debug, Clone, Default)]
struct FileScope {
    candidates: Vec<Candidate>,
    by_name: HashMap<String, Vec<usize>>,
    by_path: HashMap<String, Vec<usize>>,
}

impl FileScope {
    fn insert(&mut self, candidate: Candidate) {
        let index = self.candidates.len();
        self.by_name
            .entry(candidate.name.clone())
            .or_default()
            .push(index);
        self.by_path
            .entry(qualified_key(&candidate.scope, &candidate.name))
            .or_default()
            .push(index);
        self.candidates.push(candidate);
    }

    fn named(&self, name: &str) -> Vec<Candidate> {
        self.by_name
            .get(name)
            .map(|indices| {
                indices
                    .iter()
                    .map(|&index| self.candidates[index].clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A per-file lexical scope index built once per resolution run.
///
/// This is the first resolution pass. It answers "what is defined in this file
/// under this name?" without scanning the global graph for every call site and
/// without needing a full type checker. The second pass ([`super::linker`])
/// falls back to imports and the global name index when the local pass is not
/// conclusive.
#[derive(Debug, Clone, Default)]
pub struct ScopeIndex {
    by_file: HashMap<String, FileScope>,
}

impl ScopeIndex {
    /// Build the index from every definition node in the graph.
    ///
    /// Container-only nodes (`Repository`, `Directory`, `File`) are skipped;
    /// they are addressable through the graph but are never call targets.
    pub fn build(graph: &MemoryGraph) -> Self {
        let mut raw: HashMap<String, Vec<Candidate>> = HashMap::new();

        for node in graph.graph().node_weights() {
            if matches!(
                node.kind(),
                NodeKind::Repository | NodeKind::Directory | NodeKind::File
            ) {
                continue;
            }
            if let Ok(candidate) = Candidate::from_node(node) {
                raw.entry(candidate.file_path.clone())
                    .or_default()
                    .push(candidate);
            }
        }

        let mut by_file = HashMap::with_capacity(raw.len());
        for (file_path, mut candidates) in raw {
            candidates.sort_by(|left, right| left.uri.cmp(&right.uri));
            candidates.dedup_by(|left, right| left.uri == right.uri);

            let mut file_scope = FileScope::default();
            for candidate in candidates {
                file_scope.insert(candidate);
            }
            by_file.insert(file_path, file_scope);
        }

        Self { by_file }
    }

    /// Number of indexed files.
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Total number of indexed candidates.
    pub fn candidate_count(&self) -> usize {
        self.by_file
            .values()
            .map(|file_scope| file_scope.candidates.len())
            .sum()
    }

    /// All definitions in a file with the given unqualified name.
    pub fn candidates_in_file(&self, file_path: &str, name: &str) -> Vec<Candidate> {
        self.by_file
            .get(file_path)
            .map(|file_scope| file_scope.named(name))
            .unwrap_or_default()
    }

    /// Callable definitions in a file with the given unqualified name.
    pub fn callables_in_file(&self, file_path: &str, name: &str) -> Vec<Candidate> {
        self.candidates_in_file(file_path, name)
            .into_iter()
            .filter(Candidate::is_callable)
            .collect()
    }

    /// Type definitions in a file with the given unqualified name.
    pub fn types_in_file(&self, file_path: &str, name: &str) -> Vec<Candidate> {
        self.candidates_in_file(file_path, name)
            .into_iter()
            .filter(Candidate::is_type_definition)
            .collect()
    }

    /// Find type definitions that lexically enclose `scope` in `file_path`.
    ///
    /// Results are ordered from the deepest enclosing scope to the shallowest.
    /// For a method inside `Store::load::helper`, this yields the types named
    /// by the prefixes `Store::load::helper`, `Store::load`, and `Store`.
    pub fn enclosing_types(&self, file_path: &str, scope: &ScopeChain) -> Vec<Candidate> {
        let Some(file_scope) = self.by_file.get(file_path) else {
            return Vec::new();
        };

        let mut result = Vec::new();
        for depth in (1..=scope.len()).rev() {
            let components = &scope.components()[..depth];
            let Some(name) = components.last() else {
                continue;
            };
            let parent = ScopeChain::new(components[..depth - 1].to_vec());
            let key = qualified_key(&parent, name);

            if let Some(indices) = file_scope.by_path.get(&key) {
                for &index in indices {
                    let candidate = &file_scope.candidates[index];
                    if candidate.is_type_definition() {
                        result.push(candidate.clone());
                    }
                }
            }
        }
        result
    }
}

fn qualified_key(scope: &ScopeChain, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", scope.join(), name)
    }
}

/// Remove balanced `<...>` generic argument lists.
fn remove_generics(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut depth = 0usize;

    for ch in input.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => output.push(ch),
            _ => {}
        }
    }

    output
}

/// Find the opening parenthesis that matches a trailing `)`.
fn matching_open_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, ch) in text.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|ch| {
            ch.is_control() || matches!(ch, ':' | '.' | '(' | ')' | '[' | ']' | '{' | '}')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_chain_prefix_and_nesting() {
        let root = ScopeChain::from_scope(None);
        let store = ScopeChain::from_scope(Some("Store"));
        let load = ScopeChain::from_scope(Some("Store::load"));
        let nested = ScopeChain::from_scope(Some("Store::load::helper"));

        assert!(root.is_prefix_of(&store));
        assert!(store.is_prefix_of(&nested));
        assert!(nested.is_prefix_of(&nested));
        assert!(!load.is_prefix_of(&store));
        assert_eq!(store.common_prefix_len(&nested), 1);
        assert_eq!(nested.common_prefix_len(&store), 1);
        assert_eq!(load.parent().unwrap(), store);
        assert_eq!(store.child("load"), load);
        assert_eq!(nested.join(), "Store::load::helper");
        assert_eq!(nested.last(), Some("helper"));
    }

    #[test]
    fn test_call_shape_parsing() {
        let bare = CallShape::parse("save").unwrap();
        assert_eq!(bare.name(), "save");
        assert_eq!(bare.qualifier(), None);
        assert_eq!(bare.separator(), CallSeparator::None);

        let receiver = CallShape::parse("agent.execute").unwrap();
        assert_eq!(receiver.name(), "execute");
        assert_eq!(receiver.qualifier(), Some("agent"));
        assert_eq!(receiver.separator(), CallSeparator::Dot);

        let path = CallShape::parse("Index::load").unwrap();
        assert_eq!(path.name(), "load");
        assert_eq!(path.qualifier(), Some("Index"));
        assert_eq!(path.separator(), CallSeparator::Path);

        let nested = CallShape::parse("a.b.c").unwrap();
        assert_eq!(nested.name(), "c");
        assert_eq!(nested.qualifier(), Some("a.b"));

        let generic = CallShape::parse("Vec::<String>::new").unwrap();
        assert_eq!(generic.name(), "new");
        assert_eq!(generic.qualifier(), Some("Vec"));

        let spaced = CallShape::parse(" fmt :: Println ( ) ").unwrap();
        assert_eq!(spaced.name(), "Println");
        assert_eq!(spaced.qualifier(), Some("fmt"));
        assert_eq!(spaced.separator(), CallSeparator::Path);
    }

    #[test]
    fn test_call_shape_rejects_empty_or_expression_names() {
        assert!(CallShape::parse("").is_none());
        assert!(CallShape::parse("   ").is_none());
        assert!(CallShape::parse("::").is_none());
        assert!(CallShape::parse("(|| {})()").is_none());
    }

    #[test]
    fn test_scope_index_builds_local_candidates_and_enclosing_types() {
        let mut graph = MemoryGraph::new();
        let file = SymbolUri::for_file("repo", "src/store.rs").unwrap();
        let store = SymbolUri::new("repo", "src/store.rs", None, "Store", "struct").unwrap();
        let method = SymbolUri::new("repo", "src/store.rs", Some("Store"), "save", "fn").unwrap();
        let helper_uri =
            SymbolUri::new("repo", "src/store.rs", Some("Store::save"), "helper", "fn").unwrap();

        for (uri, name, kind, span) in [
            (file, "file", NodeKind::File, SourceSpan::zero()),
            (store, "Store", NodeKind::Struct, SourceSpan::zero()),
            (
                method.clone(),
                "save",
                NodeKind::Method,
                SourceSpan::new(1, 0, 2, 0, 0, 10).unwrap(),
            ),
            (
                helper_uri,
                "helper",
                NodeKind::Function,
                SourceSpan::new(2, 0, 3, 0, 10, 20).unwrap(),
            ),
        ] {
            let node =
                CodeEntityNode::try_new(uri, name, kind, "src/store.rs", "rust", span).unwrap();
            graph.add_node(node).unwrap();
        }

        let index = ScopeIndex::build(&graph);
        assert_eq!(index.file_count(), 1);
        assert_eq!(index.candidate_count(), 3);

        let callables = index.callables_in_file("src/store.rs", "save");
        assert_eq!(callables.len(), 1);
        assert_eq!(callables[0].uri, method);
        assert_eq!(callables[0].receiver_type(), Some("Store"));

        let scope = ScopeChain::from_scope(Some("Store::save::helper"));
        let enclosing = index.enclosing_types("src/store.rs", &scope);
        assert_eq!(enclosing.len(), 1);
        assert_eq!(enclosing[0].name, "Store");
    }

    use crate::model::SourceSpan;
}
