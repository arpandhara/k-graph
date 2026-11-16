use tree_sitter::Node;

use crate::model::{NodeKind, Visibility};

use super::error::ParserError;
use super::language::{ImportRecord, LanguageAdapter, RawExtractedFile};
use super::tree_sitter_adapter::{
    LanguageSyntax, TreeSitterAdapter, collapse_whitespace, node_text,
};

const EXTENSIONS: &[&str] = &["rs"];

/// Rust ingestion backend.
pub struct RustAdapter {
    inner: TreeSitterAdapter,
}

impl RustAdapter {
    /// Compile the Rust grammar and query set.
    pub fn new() -> Result<Self, ParserError> {
        Ok(Self {
            inner: TreeSitterAdapter::new(
                tree_sitter_rust::LANGUAGE.into(),
                include_str!("../../queries/rust/symbols.scm"),
                include_str!("../../queries/rust/calls.scm"),
                EXTENSIONS,
                Box::new(RustSyntax),
            )?,
        })
    }
}

impl LanguageAdapter for RustAdapter {
    fn language_name(&self) -> &'static str {
        self.inner.language_name()
    }

    fn can_handle(&self, extension: &str) -> bool {
        self.inner.can_handle(extension)
    }

    fn parse_file(
        &self,
        repo_name: &str,
        file_path: &str,
        source: &str,
    ) -> Result<RawExtractedFile, ParserError> {
        self.inner.parse_file(repo_name, file_path, source)
    }
}

struct RustSyntax;

impl LanguageSyntax for RustSyntax {
    fn language_name(&self) -> &'static str {
        "rust"
    }

    fn visibility(&self, kind: NodeKind, _name: &str, node: Node<'_>, source: &str) -> Visibility {
        // Trait items are public by virtue of being in the trait, even when the
        // `pub` keyword is omitted.
        if has_ancestor(node, "trait_item") {
            return Visibility::Public;
        }

        let text = node_text(node, source).unwrap_or_default().trim_start();
        if let Some(rest) = text.strip_prefix("pub") {
            if rest.starts_with("(crate)") {
                return Visibility::Crate;
            }
            if rest.starts_with("(super)") || rest.starts_with("(in ") {
                return Visibility::ModulePrivate;
            }
            if rest.starts_with("(self)") {
                return Visibility::Private;
            }
            if rest.starts_with(char::is_whitespace) {
                return Visibility::Public;
            }
        }

        match kind {
            NodeKind::Field | NodeKind::EnumVariant => Visibility::Private,
            _ => Visibility::ModulePrivate,
        }
    }

    fn imports(&self, declaration: Node<'_>, source: &str) -> Vec<ImportRecord> {
        let Ok(text) = node_text(declaration, source) else {
            return Vec::new();
        };
        parse_use_declaration(text)
    }

    fn scope_name(&self, scope_node: Node<'_>, source: &str) -> Option<String> {
        let text = node_text(scope_node, source).ok()?;
        normalize_impl_target(text)
    }
}

fn has_ancestor(node: Node<'_>, kind: &str) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == kind {
            return true;
        }
        ancestor = parent.parent();
    }
    false
}

/// Reduce an `impl` type expression to its last path segment so the scope is a
/// stable type name: `&mut foo::Index<T>` becomes `Index`.
fn normalize_impl_target(raw: &str) -> Option<String> {
    let mut text = collapse_whitespace(raw);
    while let Some(rest) = text.strip_prefix('&') {
        text = rest.trim_start().to_string();
    }
    for prefix in ["mut ", "dyn "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim_start().to_string();
        }
    }

    let base = text.split(['<', '(', '[']).next().unwrap_or(&text).trim();
    let last = base.rsplit("::").next().unwrap_or(base);
    let last = last.trim_matches(|c: char| c.is_whitespace() || c == '\'');
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

fn parse_use_declaration(text: &str) -> Vec<ImportRecord> {
    let Some(position) = text.find("use ") else {
        return Vec::new();
    };
    let body = text[position + 4..].trim().trim_end_matches(';').trim();
    let mut records = Vec::new();
    parse_use_tree(body, None, &mut records);
    records
}

fn parse_use_tree(tree: &str, prefix: Option<&str>, out: &mut Vec<ImportRecord>) {
    let tree = tree.trim();
    if tree.is_empty() {
        return;
    }

    if let Some(open) = find_top_level_open_brace(tree)
        && let Some(close) = tree.rfind('}')
        && close > open
    {
        let head = tree[..open].trim().trim_end_matches("::").trim();
        let next_prefix = join_prefix(prefix, head);
        for part in split_top_level(&tree[open + 1..close], ',') {
            parse_use_tree(part, Some(next_prefix.as_str()), out);
        }
        return;
    }

    let leaf = tree.trim().trim_start_matches("::").trim();
    if leaf.is_empty() {
        return;
    }

    let (path, alias) = split_alias(leaf);
    let full = if path == "self" {
        prefix.map(str::to_string).unwrap_or_default()
    } else {
        join_prefix(prefix, &path)
    };

    if full.is_empty() {
        return;
    }

    if let Some(base) = full.strip_suffix("::*") {
        out.push(ImportRecord {
            source_path: base.to_string(),
            imported_symbol: "*".to_string(),
            alias,
        });
        return;
    }

    if full == "*" {
        out.push(ImportRecord {
            source_path: prefix.unwrap_or_default().to_string(),
            imported_symbol: "*".to_string(),
            alias,
        });
        return;
    }

    match full.rsplit_once("::") {
        Some((source_path, symbol)) => out.push(ImportRecord {
            source_path: source_path.to_string(),
            imported_symbol: symbol.to_string(),
            alias,
        }),
        None => out.push(ImportRecord {
            source_path: String::new(),
            imported_symbol: full,
            alias,
        }),
    }
}

fn join_prefix(prefix: Option<&str>, head: &str) -> String {
    match (prefix, head.is_empty()) {
        (Some(prefix), false) => format!("{prefix}::{head}"),
        (Some(prefix), true) => prefix.to_string(),
        (None, false) => head.to_string(),
        (None, true) => String::new(),
    }
}

fn split_alias(leaf: &str) -> (String, Option<String>) {
    match leaf.rsplit_once(" as ") {
        Some((path, alias)) => {
            let alias = alias.trim();
            (
                path.trim().to_string(),
                if alias.is_empty() || alias == "_" {
                    None
                } else {
                    Some(alias.to_string())
                },
            )
        }
        None => (leaf.trim().to_string(), None),
    }
}

fn find_top_level_open_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in text.char_indices() {
        match ch {
            '{' if depth == 0 => return Some(offset),
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (offset, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if ch == delimiter && depth == 0 => {
                parts.push(&text[start..offset]);
                start = offset + ch.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
use crate::db::{save as persist, load, storage::*};

/// Storage index.
pub struct Index {
    pub entries: u32,
    hidden: bool,
}

impl Index {
    /// Load the index.
    pub fn load(&self) -> u32 {
        helper(self.entries)
    }
}

fn helper(value: u32) -> u32 {
    persist(value);
    load(value)
}
"#;

    #[test]
    fn test_rust_parses_definitions_scopes_and_calls() {
        let adapter = RustAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/index.rs", SAMPLE).unwrap();

        let uris: Vec<&str> = extraction
            .nodes
            .iter()
            .map(|node| node.uri().as_str())
            .collect();
        assert!(uris.contains(&"sym://repo/src/index.rs#file:file"));
        assert!(uris.contains(&"sym://repo/src/index.rs#Index:struct"));
        assert!(uris.contains(&"sym://repo/src/index.rs#Index::entries:field"));
        assert!(uris.contains(&"sym://repo/src/index.rs#Index::load:fn"));
        assert!(uris.contains(&"sym://repo/src/index.rs#helper:fn"));

        let load = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Index::load:fn"))
            .unwrap();
        assert_eq!(load.kind(), NodeKind::Method);
        assert_eq!(load.visibility(), Visibility::Public);
        assert_eq!(load.docstring(), Some("Load the index."));
        assert!(load.signature().unwrap().contains("pub fn load"));

        assert!(
            extraction
                .local_edges
                .iter()
                .any(|edge| edge.kind() == crate::model::EdgeKind::MemberOf
                    && edge.source_uri().as_str().contains("#Index:struct"))
        );
        assert!(
            extraction
                .local_edges
                .iter()
                .any(|edge| edge.kind() == crate::model::EdgeKind::Contains
                    && edge.source_uri().as_str().contains("#file:file"))
        );

        let calls: Vec<&str> = extraction
            .pending_calls
            .iter()
            .map(|call| call.call_expr.as_str())
            .collect();
        assert!(calls.contains(&"helper"));
        assert!(calls.contains(&"persist"));
        assert!(calls.contains(&"load"));
        assert!(
            extraction
                .pending_calls
                .iter()
                .all(|call| call.caller_uri.contains("#Index::load:fn")
                    || call.caller_uri.contains("#helper:fn"))
        );

        let imports: Vec<(String, String, Option<String>)> = extraction
            .imports
            .iter()
            .map(|record| {
                (
                    record.source_path.clone(),
                    record.imported_symbol.clone(),
                    record.alias.clone(),
                )
            })
            .collect();
        assert!(imports.contains(&(
            "crate::db".to_string(),
            "save".to_string(),
            Some("persist".to_string())
        )));
        assert!(imports.contains(&("crate::db".to_string(), "load".to_string(), None)));
        assert!(imports.contains(&("crate::db::storage".to_string(), "*".to_string(), None)));
    }

    #[test]
    fn test_rust_raw_identifier_and_nested_modules() {
        let source = r#"
mod inner {
    pub fn r#type() {}
}
"#;
        let adapter = RustAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/lib.rs", source).unwrap();
        // Raw identifiers keep the name without the `r#` prefix.
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.uri().as_str().ends_with("#inner::type:fn"))
        );
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.uri().as_str().ends_with("#inner:mod"))
        );
    }

    #[test]
    fn test_rust_use_tree_parser() {
        assert_eq!(
            parse_use_declaration("use crate::db::{save as persist, load};"),
            vec![
                ImportRecord {
                    source_path: "crate::db".to_string(),
                    imported_symbol: "save".to_string(),
                    alias: Some("persist".to_string()),
                },
                ImportRecord {
                    source_path: "crate::db".to_string(),
                    imported_symbol: "load".to_string(),
                    alias: None,
                },
            ]
        );
        assert_eq!(
            parse_use_declaration("use std::collections::HashMap;"),
            vec![ImportRecord {
                source_path: "std::collections".to_string(),
                imported_symbol: "HashMap".to_string(),
                alias: None,
            }]
        );
        assert_eq!(
            parse_use_declaration("use foo::{self, bar::*};"),
            vec![
                ImportRecord {
                    source_path: String::new(),
                    imported_symbol: "foo".to_string(),
                    alias: None,
                },
                ImportRecord {
                    source_path: "foo::bar".to_string(),
                    imported_symbol: "*".to_string(),
                    alias: None,
                },
            ]
        );
    }

    #[test]
    fn test_rust_impl_target_normalization() {
        assert_eq!(
            normalize_impl_target("&mut foo::Index<T>").as_deref(),
            Some("Index")
        );
        assert_eq!(
            normalize_impl_target("dyn Display").as_deref(),
            Some("Display")
        );
        assert_eq!(normalize_impl_target("[u8]"), None);
    }

    #[test]
    fn test_rust_impl_in_nested_module_attaches_to_local_type() {
        let source = r#"
mod db {
    pub struct Index {
        pub entries: u32,
    }

    impl Index {
        pub fn load(&self) -> u32 {
            self.entries
        }
    }
}
"#;
        let adapter = RustAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/lib.rs", source).unwrap();

        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.uri().as_str().ends_with("#db::Index::load:fn"))
        );
        assert!(extraction.local_edges.iter().any(|edge| {
            edge.kind() == crate::model::EdgeKind::MemberOf
                && edge.source_uri().as_str().ends_with("#db::Index:struct")
                && edge.target_uri().as_str().ends_with("#db::Index::load:fn")
        }));
    }

    #[test]
    fn test_rust_deterministic_output() {
        let adapter = RustAdapter::new().unwrap();
        let first = adapter.parse_file("repo", "src/a.rs", SAMPLE).unwrap();
        let second = adapter.parse_file("repo", "src/a.rs", SAMPLE).unwrap();
        assert_eq!(first, second);
    }
}
