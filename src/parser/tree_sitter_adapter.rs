use std::collections::{HashMap, HashSet};
use std::path::Path;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

use crate::model::{
    CodeEdge, CodeEntityNode, EdgeKind, NodeKind, SourceSpan, SymbolUri, Visibility,
};

use super::error::ParserError;
use super::language::{ImportRecord, LanguageAdapter, PendingCallSite, RawExtractedFile};

/// Maximum length (in characters) of a stored signature or call expression.
const MAX_TEXT_CHARS: usize = 512;

/// Maximum length (in characters) of a stored leading-comment docstring.
const MAX_DOC_CHARS: usize = 4_096;

/// Language-specific behaviour used by the generic Tree-Sitter executor.
///
/// Definitions and references are found declaratively by the `.scm` queries.
/// This trait supplies the parts that cannot be expressed as a Tree-Sitter
/// pattern: visibility rules, signature/docstring formatting, import parsing,
/// and how an `impl` target is named.
pub trait LanguageSyntax: Send + Sync {
    /// Canonical language name recorded on every node.
    fn language_name(&self) -> &'static str;

    /// Adjust a definition kind after scope analysis (for example
    /// `__init__` becomes a constructor).
    fn refine_kind(&self, kind: NodeKind, _name: &str, _node: Node<'_>, _source: &str) -> NodeKind {
        kind
    }

    /// Visibility of a definition. The default is module-private.
    fn visibility(
        &self,
        _kind: NodeKind,
        _name: &str,
        _node: Node<'_>,
        _source: &str,
    ) -> Visibility {
        Visibility::ModulePrivate
    }

    /// Declaration text without the body. The default implementation is
    /// language-agnostic and works for every grammar in this crate.
    fn signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        default_signature(node, source)
    }

    /// Documentation attached to a definition.
    fn docstring(&self, node: Node<'_>, source: &str) -> Option<String> {
        default_docstring(node, source)
    }

    /// Normalize an import declaration into [`ImportRecord`] values.
    fn imports(&self, _declaration: Node<'_>, _source: &str) -> Vec<ImportRecord> {
        Vec::new()
    }

    /// Name of a Rust-style `impl` scope provider. Unused by languages whose
    /// classes are ordinary definitions.
    fn scope_name(&self, _scope_node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    /// File-wide scope prepended to every definition.
    ///
    /// Go package clauses are siblings of the declarations they scope, not
    /// ancestors, so the generic parent-chain walk cannot discover them.
    /// Languages without a file-wide namespace return `None`.
    fn base_scope(&self, _root: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Extra scope component derived from the definition node itself.
    ///
    /// Go methods declare their receiver type as a parameter rather than
    /// nesting inside the type, so the receiver name cannot come from the
    /// parent chain. Other languages return `None`.
    fn definition_scope(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }
}

/// Compiled declarative queries for one grammar.
///
/// Query compilation happens once per adapter. `Query` and `Language` are both
/// `Send + Sync` in Tree-Sitter 0.24, so an adapter can be shared across Rayon
/// workers without locking.
pub struct QuerySet {
    language: Language,
    language_name: &'static str,
    symbols: Query,
    references: Query,
}

impl QuerySet {
    /// Compile the symbol and reference queries for a grammar.
    pub fn new(
        language_name: &'static str,
        language: Language,
        symbols_source: &str,
        references_source: &str,
    ) -> Result<Self, ParserError> {
        let symbols = Query::new(&language, symbols_source).map_err(|err| ParserError::Query {
            language: language_name,
            query: "symbols",
            message: err.to_string(),
        })?;
        let references =
            Query::new(&language, references_source).map_err(|err| ParserError::Query {
                language: language_name,
                query: "references",
                message: err.to_string(),
            })?;

        Ok(Self {
            language,
            language_name,
            symbols,
            references,
        })
    }

    /// Parse one file with the supplied language semantics.
    pub fn parse(
        &self,
        syntax: &dyn LanguageSyntax,
        repo_name: &str,
        file_path: &str,
        source: &str,
    ) -> Result<RawExtractedFile, ParserError> {
        parse_source(self, syntax, repo_name, file_path, source)
    }
}

/// Generic Tree-Sitter backend for one grammar.
pub struct TreeSitterAdapter {
    queries: QuerySet,
    syntax: Box<dyn LanguageSyntax>,
    extensions: &'static [&'static str],
}

impl TreeSitterAdapter {
    /// Build an adapter from a grammar, query sources, and language semantics.
    pub fn new(
        language: Language,
        symbols_source: &str,
        references_source: &str,
        extensions: &'static [&'static str],
        syntax: Box<dyn LanguageSyntax>,
    ) -> Result<Self, ParserError> {
        let queries = QuerySet::new(
            syntax.language_name(),
            language,
            symbols_source,
            references_source,
        )?;
        Ok(Self {
            queries,
            syntax,
            extensions,
        })
    }

    /// Access the compiled query set.
    pub fn queries(&self) -> &QuerySet {
        &self.queries
    }
}

impl LanguageAdapter for TreeSitterAdapter {
    fn language_name(&self) -> &'static str {
        self.syntax.language_name()
    }

    fn can_handle(&self, extension: &str) -> bool {
        let normalized = extension.trim_start_matches('.').to_ascii_lowercase();
        self.extensions.contains(&normalized.as_str())
    }

    fn parse_file(
        &self,
        repo_name: &str,
        file_path: &str,
        source: &str,
    ) -> Result<RawExtractedFile, ParserError> {
        self.queries
            .parse(self.syntax.as_ref(), repo_name, file_path, source)
    }
}

#[derive(Debug, Clone)]
struct ScopeProvider {
    name: String,
    /// `None` means a Rust `impl` block: it contributes a scope name but is not
    /// itself a graph node.
    kind: Option<NodeKind>,
}

struct UnbuiltDefinition<'tree> {
    node: Node<'tree>,
    kind: NodeKind,
    name: String,
}

struct BuiltDefinition {
    node: CodeEntityNode,
    scope_parts: Vec<String>,
    /// Definition-local scope component (currently a Go method receiver).
    extra_scope: Option<String>,
    nearest_definition_id: Option<usize>,
    nearest_provider: Option<ScopeProvider>,
}

fn parse_source(
    queries: &QuerySet,
    syntax: &dyn LanguageSyntax,
    repo_name: &str,
    file_path: &str,
    source: &str,
) -> Result<RawExtractedFile, ParserError> {
    // The file URI validates the repository and path once, before any source
    // token is turned into a node. A path that cannot be represented (absolute,
    // traversal-bearing, or containing a URI delimiter) fails the whole file.
    let file_uri = SymbolUri::for_file(repo_name, file_path)?;

    let mut parser = Parser::new();
    parser
        .set_language(&queries.language)
        .map_err(|err| ParserError::Language {
            language: queries.language_name,
            message: err.to_string(),
        })?;

    let tree = parser
        .parse(source.as_bytes(), None)
        .ok_or_else(|| ParserError::ParseFailed {
            path: file_path.to_string(),
        })?;

    let root = tree.root_node();
    let bytes = source.as_bytes();

    // Pass 1: collect raw definitions and Rust-style impl scope providers.
    let mut raw_definitions: Vec<UnbuiltDefinition<'_>> = Vec::new();
    let mut impl_scopes: Vec<(Node<'_>, String)> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&queries.symbols, root, bytes);
        while let Some(query_match) = matches.next() {
            let mut definition: Option<(Node<'_>, NodeKind)> = None;
            let mut name_node: Option<Node<'_>> = None;
            let mut impl_scope_node: Option<Node<'_>> = None;
            let mut impl_target_node: Option<Node<'_>> = None;

            for capture in query_match.captures {
                let capture_name = queries.symbols.capture_names()[capture.index as usize];
                if let Some(kind_name) = capture_name.strip_prefix("def.")
                    && let Some(kind) = NodeKind::from_wire_str(kind_name)
                {
                    definition = Some((capture.node, kind));
                } else if capture_name == "name" {
                    name_node = Some(capture.node);
                } else if capture_name == "impl.scope" {
                    impl_scope_node = Some(capture.node);
                } else if capture_name == "impl.target" {
                    impl_target_node = Some(capture.node);
                }
            }

            if let (Some((node, kind)), Some(name_node)) = (definition, name_node) {
                let raw_name = node_text(name_node, source)?;
                if let Some(name) = sanitize_name(raw_name) {
                    raw_definitions.push(UnbuiltDefinition { node, kind, name });
                }
            } else if let (Some(scope_node), Some(target_node)) =
                (impl_scope_node, impl_target_node)
                && let Some(name) = syntax
                    .scope_name(target_node, source)
                    .and_then(|name| sanitize_name(&name))
            {
                impl_scopes.push((scope_node, name));
            }
        }
    }

    // Source order makes every downstream list deterministic.
    raw_definitions.sort_by(|a, b| {
        a.node
            .start_byte()
            .cmp(&b.node.start_byte())
            .then_with(|| a.name.cmp(&b.name))
    });

    // Scope providers include every named definition plus each `impl` target.
    let mut scope_providers: HashMap<usize, ScopeProvider> = HashMap::new();
    for definition in &raw_definitions {
        scope_providers.insert(
            definition.node.id(),
            ScopeProvider {
                name: definition.name.clone(),
                kind: Some(definition.kind),
            },
        );
    }
    for (scope_node, name) in &impl_scopes {
        scope_providers.insert(
            scope_node.id(),
            ScopeProvider {
                name: name.clone(),
                kind: None,
            },
        );
    }

    // The file node is always first so even an empty file is addressable.
    let file_span = span_for(root)?;
    let file_node = CodeEntityNode::try_new(
        file_uri.clone(),
        "file",
        NodeKind::File,
        file_path,
        queries.language_name,
        file_span,
    )?
    .with_visibility(Visibility::Public);

    let mut nodes = vec![file_node];
    let mut built: Vec<BuiltDefinition> = Vec::new();
    let mut seen_uris: HashSet<SymbolUri> = HashSet::new();
    let mut def_index_by_node_id: HashMap<usize, usize> = HashMap::new();

    // A file-wide namespace (Go package) is not an ancestor of the
    // declarations it scopes, so it is resolved once and prefixed explicitly.
    let base_scope = syntax.base_scope(root, source);

    for definition in &raw_definitions {
        let mut ancestor_parts = Vec::new();
        let mut nearest_definition_id = None;
        let mut nearest_provider = None;
        let mut ancestor = definition.node.parent();

        while let Some(parent) = ancestor {
            if let Some(provider) = scope_providers.get(&parent.id()) {
                if nearest_provider.is_none() {
                    nearest_provider = Some(provider.clone());
                }
                ancestor_parts.push(provider.name.clone());
                if nearest_definition_id.is_none() && provider.kind.is_some() {
                    nearest_definition_id = Some(parent.id());
                }
            }
            ancestor = parent.parent();
        }
        ancestor_parts.reverse();

        let mut scope_parts = Vec::new();
        if let Some(base) = &base_scope {
            scope_parts.push(base.clone());
        }
        scope_parts.extend(ancestor_parts);

        // A Go receiver contributes a type scope after the package name.
        let extra_scope = syntax
            .definition_scope(definition.node, source)
            .and_then(|scope| sanitize_name(&scope));
        if let Some(extra) = &extra_scope {
            scope_parts.push(extra.clone());
        }

        // A free function directly inside a type or impl is a method. A nested
        // function inside another function keeps `Function`.
        let mut kind = definition.kind;
        if kind == NodeKind::Function
            && nearest_provider.as_ref().is_some_and(|provider| {
                provider
                    .kind
                    .is_none_or(|provider_kind| provider_kind.is_type_definition())
            })
        {
            kind = NodeKind::Method;
        }
        kind = syntax.refine_kind(kind, &definition.name, definition.node, source);

        let scope = if scope_parts.is_empty() {
            None
        } else {
            Some(scope_parts.join("::"))
        };

        let uri = match SymbolUri::new(
            repo_name,
            file_path,
            scope.as_deref(),
            &definition.name,
            kind.default_uri_suffix(),
        ) {
            Ok(uri) => uri,
            Err(err) => {
                tracing::debug!(
                    path = file_path,
                    name = definition.name,
                    error = %err,
                    "skipping definition with an unrepresentable symbol name"
                );
                continue;
            }
        };

        if !seen_uris.insert(uri.clone()) {
            tracing::debug!(
                path = file_path,
                uri = %uri,
                "skipping duplicate definition URI"
            );
            continue;
        }

        let span = match span_for(definition.node) {
            Ok(span) => span,
            Err(err) => {
                tracing::debug!(path = file_path, error = %err, "skipping invalid definition span");
                continue;
            }
        };

        let mut node = match CodeEntityNode::try_new(
            uri,
            definition.name.clone(),
            kind,
            file_path,
            queries.language_name,
            span,
        ) {
            Ok(node) => node,
            Err(err) => {
                tracing::debug!(path = file_path, error = %err, "skipping invalid definition node");
                continue;
            }
        };

        node = node.with_visibility(syntax.visibility(
            kind,
            &definition.name,
            definition.node,
            source,
        ));
        if let Some(signature) = syntax.signature(definition.node, source) {
            node = node.with_signature(signature);
        }
        if let Some(docstring) = syntax.docstring(definition.node, source) {
            node = node.with_docstring(docstring);
        }

        def_index_by_node_id.insert(definition.node.id(), built.len());
        built.push(BuiltDefinition {
            node,
            scope_parts,
            extra_scope,
            nearest_definition_id,
            nearest_provider,
        });
    }

    for definition in &built {
        nodes.push(definition.node.clone());
    }

    let mut local_edges = Vec::new();
    let mut edge_keys: HashSet<(SymbolUri, SymbolUri, EdgeKind)> = HashSet::new();

    // Index type definitions by (parent scope, name) so methods inside a Rust
    // `impl` block can attach to the type instead of the enclosing module.
    let mut type_index: HashMap<(String, String), SymbolUri> = HashMap::new();
    let mut built_index_by_uri: HashMap<SymbolUri, usize> = HashMap::new();
    for (index, definition) in built.iter().enumerate() {
        built_index_by_uri.insert(definition.node.uri().clone(), index);
        if definition.node.kind().is_type_definition() {
            let prefix = definition.scope_parts.join("::");
            type_index.insert(
                (prefix, definition.node.name().to_string()),
                definition.node.uri().clone(),
            );
        }
    }

    for definition in &built {
        let mut parent_idx = None;
        let mut parent_is_type = false;

        if definition
            .nearest_provider
            .as_ref()
            .is_some_and(|provider| provider.kind.is_none())
            && let Some((target, prefix)) = definition.scope_parts.split_last()
        {
            let key = (prefix.join("::"), target.clone());
            if let Some(uri) = type_index.get(&key)
                && let Some(index) = built_index_by_uri.get(uri)
            {
                parent_idx = Some(*index);
                parent_is_type = true;
            }
        }

        // A Go method names its receiver type instead of nesting inside it.
        // Attach to the local type when that type is defined in this file.
        if parent_idx.is_none()
            && let Some(receiver) = &definition.extra_scope
            && let Some((_, prefix)) = definition.scope_parts.split_last()
            && let Some(uri) = type_index.get(&(prefix.join("::"), receiver.clone()))
            && let Some(index) = built_index_by_uri.get(uri)
        {
            parent_idx = Some(*index);
            parent_is_type = true;
        }

        if parent_idx.is_none()
            && let Some(id) = definition.nearest_definition_id
            && let Some(index) = def_index_by_node_id.get(&id)
        {
            parent_idx = Some(*index);
            parent_is_type = built[*index].node.kind().is_type_definition();
        }

        let (source_uri, edge_kind) = match parent_idx {
            Some(index) => {
                let parent = &built[index];
                let member = parent_is_type
                    && (definition.node.kind().is_callable()
                        || matches!(
                            definition.node.kind(),
                            NodeKind::Field
                                | NodeKind::Property
                                | NodeKind::EnumVariant
                                | NodeKind::GlobalVariable
                                | NodeKind::Constant
                        ));
                (
                    parent.node.uri().clone(),
                    if member {
                        EdgeKind::MemberOf
                    } else {
                        EdgeKind::Contains
                    },
                )
            }
            None => (file_uri.clone(), EdgeKind::Contains),
        };

        let target_uri = definition.node.uri().clone();
        if source_uri == target_uri {
            continue;
        }

        if !edge_keys.insert((source_uri.clone(), target_uri.clone(), edge_kind)) {
            continue;
        }

        let edge =
            CodeEdge::new(source_uri, target_uri, edge_kind).with_call_site(definition.node.span());
        local_edges.push(edge);
    }

    // Pass 2: calls and imports. Calls are attached to the nearest enclosing
    // callable definition; top-level calls without a caller are ignored.
    let mut callable_uri_by_node_id: HashMap<usize, (SymbolUri, NodeKind)> = HashMap::new();
    for (id, index) in &def_index_by_node_id {
        let definition = &built[*index];
        callable_uri_by_node_id
            .insert(*id, (definition.node.uri().clone(), definition.node.kind()));
    }

    let mut call_nodes: Vec<Node<'_>> = Vec::new();
    let mut import_nodes: Vec<Node<'_>> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&queries.references, root, bytes);
        while let Some(query_match) = matches.next() {
            for capture in query_match.captures {
                let capture_name = queries.references.capture_names()[capture.index as usize];
                if capture_name == "call.target" {
                    call_nodes.push(capture.node);
                } else if capture_name == "import.statement" {
                    import_nodes.push(capture.node);
                }
            }
        }
    }

    let mut pending_calls: Vec<PendingCallSite> = Vec::new();
    let mut call_keys: HashSet<(String, String, u32, u32)> = HashSet::new();
    for call_node in call_nodes {
        let mut caller = None;
        let mut ancestor = call_node.parent();
        while let Some(parent) = ancestor {
            if let Some((uri, kind)) = callable_uri_by_node_id.get(&parent.id())
                && kind.is_callable()
            {
                caller = Some(uri.clone());
                break;
            }
            ancestor = parent.parent();
        }

        let Some(caller_uri) = caller else {
            continue;
        };

        let raw = node_text(call_node, source)?;
        let call_expr = truncate_chars(&collapse_whitespace(raw), MAX_TEXT_CHARS);
        if call_expr.is_empty() {
            continue;
        }

        let position = call_node.start_position();
        let line = to_u32(position.row).saturating_add(1);
        let col = to_u32(position.column);
        let uri_text = caller_uri.to_string();

        if call_keys.insert((uri_text.clone(), call_expr.clone(), line, col)) {
            pending_calls.push(PendingCallSite {
                caller_uri: uri_text,
                call_expr,
                line,
                col,
            });
        }
    }
    pending_calls.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| a.col.cmp(&b.col))
            .then_with(|| a.call_expr.cmp(&b.call_expr))
            .then_with(|| a.caller_uri.cmp(&b.caller_uri))
    });

    let mut imports: Vec<ImportRecord> = Vec::new();
    let mut import_keys: HashSet<(String, String, Option<String>)> = HashSet::new();
    let mut seen_import_nodes: HashSet<usize> = HashSet::new();
    for import_node in import_nodes {
        if !seen_import_nodes.insert(import_node.id()) {
            continue;
        }
        for record in syntax.imports(import_node, source) {
            let key = (
                record.source_path.clone(),
                record.imported_symbol.clone(),
                record.alias.clone(),
            );
            if import_keys.insert(key) {
                imports.push(record);
            }
        }
    }
    imports.sort_by(|a, b| {
        a.source_path
            .cmp(&b.source_path)
            .then_with(|| a.imported_symbol.cmp(&b.imported_symbol))
            .then_with(|| a.alias.cmp(&b.alias))
    });

    Ok(RawExtractedFile {
        nodes,
        local_edges,
        pending_calls,
        imports,
    })
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn span_for(node: Node<'_>) -> Result<SourceSpan, ParserError> {
    let start = node.start_position();
    let end = node.end_position();
    Ok(SourceSpan::new(
        to_u32(start.row).saturating_add(1),
        to_u32(start.column),
        to_u32(end.row).saturating_add(1),
        to_u32(end.column),
        node.start_byte(),
        node.end_byte(),
    )?)
}

pub(crate) fn node_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str, ParserError> {
    node.utf8_text(source.as_bytes())
        .map_err(|_| ParserError::InvalidUtf8 {
            start: node.start_byte(),
            end: node.end_byte(),
        })
}

fn sanitize_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Rust raw identifiers (`r#type`) and TypeScript private names (`#field`)
    // contain a `#`, which is a reserved URI delimiter.
    let stripped = trimmed.strip_prefix("r#").unwrap_or(trimmed);
    let stripped = stripped.strip_prefix('#').unwrap_or(stripped);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

pub(crate) fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut result: String = input.chars().take(max_chars).collect();
    result.push('…');
    result
}

fn default_signature(node: Node<'_>, source: &str) -> Option<String> {
    let end_byte = node
        .child_by_field_name("body")
        .map(|body| body.start_byte())
        .unwrap_or_else(|| {
            node_text(node, source)
                .ok()
                .and_then(|text| text.find('\n').map(|offset| node.start_byte() + offset))
                .unwrap_or(node.end_byte())
        });

    let raw = source.get(node.start_byte()..end_byte)?;
    let collapsed = collapse_whitespace(raw);
    let trimmed = collapsed
        .trim()
        .trim_end_matches([',', ';', ':'])
        .trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_chars(trimmed, MAX_TEXT_CHARS))
    }
}

pub(crate) fn default_docstring(node: Node<'_>, source: &str) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    collect_leading_comments(node, source, &mut comments);

    if comments.is_empty()
        && let Some(parent) = node.parent()
        && matches!(parent.kind(), "decorated_definition" | "export_statement")
    {
        collect_leading_comments(parent, source, &mut comments);
    }

    comments.reverse();

    let mut cleaned: Vec<String> = Vec::new();
    for comment in comments {
        let text = clean_comment(&comment);
        if !text.is_empty() {
            cleaned.push(text);
        }
    }

    if cleaned.is_empty() {
        None
    } else {
        Some(truncate_chars(&cleaned.join("\n"), MAX_DOC_CHARS))
    }
}

fn collect_leading_comments(node: Node<'_>, source: &str, comments: &mut Vec<String>) {
    let mut current = node.prev_named_sibling();
    while let Some(previous) = current {
        let kind = previous.kind();
        if is_comment_kind(kind) {
            if let Ok(text) = node_text(previous, source) {
                comments.push(text.to_string());
            }
        } else if is_metadata_kind(kind) {
            // Attributes/decorators sit between comments and the definition.
        } else {
            break;
        }
        current = previous.prev_named_sibling();
    }
}

fn is_comment_kind(kind: &str) -> bool {
    kind == "comment" || kind.ends_with("_comment")
}

fn is_metadata_kind(kind: &str) -> bool {
    matches!(
        kind,
        "attribute_item" | "inner_attribute_item" | "decorator" | "decorator_list"
    )
}

fn clean_comment(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let mut trimmed = line.trim();
        if trimmed.starts_with("<!--") || trimmed.starts_with("/*!") {
            trimmed = trimmed
                .trim_start_matches("<!--")
                .trim_start_matches("/*!")
                .trim_end_matches("-->")
                .trim_end_matches("*/");
        } else if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            trimmed = &trimmed[3..];
        } else if trimmed.starts_with("//") {
            trimmed = &trimmed[2..];
        } else if trimmed.starts_with("/**") {
            trimmed = &trimmed[3..];
        } else if trimmed.starts_with("/*") {
            trimmed = &trimmed[2..];
        } else if trimmed.starts_with('*') {
            trimmed = trimmed.trim_start_matches('*');
        } else if trimmed.starts_with('#') {
            trimmed = trimmed.trim_start_matches('#');
        }

        let trimmed = trimmed.trim_end_matches("*/").trim();
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }

    lines.join("\n")
}

/// Return the extension of a path in lowercase, without a leading dot.
pub(crate) fn extension_of(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collapse_and_truncate() {
        assert_eq!(collapse_whitespace("  a\n b\tc "), "a b c");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        assert_eq!(truncate_chars("abc", 3), "abc");
    }

    #[test]
    fn test_clean_comment_markers() {
        assert_eq!(clean_comment("/// hello\n/// world"), "hello\nworld");
        assert_eq!(clean_comment("/* hello */"), "hello");
        assert_eq!(clean_comment("# hello"), "hello");
    }

    #[test]
    fn test_extension_of() {
        assert_eq!(extension_of("src/main.RS").as_deref(), Some("rs"));
        assert_eq!(extension_of("Makefile"), None);
    }
}
