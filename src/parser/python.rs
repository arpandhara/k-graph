use tree_sitter::Node;

use crate::model::{NodeKind, Visibility};

use super::error::ParserError;
use super::language::{ImportRecord, LanguageAdapter, RawExtractedFile};
use super::tree_sitter_adapter::{
    LanguageSyntax, TreeSitterAdapter, collapse_whitespace, node_text,
};

const EXTENSIONS: &[&str] = &["py", "pyi"];

/// Python ingestion backend.
pub struct PythonAdapter {
    inner: TreeSitterAdapter,
}

impl PythonAdapter {
    /// Compile the Python grammar and query set.
    pub fn new() -> Result<Self, ParserError> {
        Ok(Self {
            inner: TreeSitterAdapter::new(
                tree_sitter_python::LANGUAGE.into(),
                include_str!("../../queries/python/symbols.scm"),
                include_str!("../../queries/python/calls.scm"),
                EXTENSIONS,
                Box::new(PythonSyntax),
            )?,
        })
    }
}

impl LanguageAdapter for PythonAdapter {
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

struct PythonSyntax;

impl LanguageSyntax for PythonSyntax {
    fn language_name(&self) -> &'static str {
        "python"
    }

    fn refine_kind(&self, kind: NodeKind, name: &str, _node: Node<'_>, _source: &str) -> NodeKind {
        match kind {
            NodeKind::Method if matches!(name, "__init__" | "__new__") => NodeKind::Constructor,
            NodeKind::GlobalVariable if is_constant_name(name) => NodeKind::Constant,
            _ => kind,
        }
    }

    fn visibility(
        &self,
        _kind: NodeKind,
        name: &str,
        _node: Node<'_>,
        _source: &str,
    ) -> Visibility {
        let private = name.starts_with('_') && !(name.starts_with("__") && name.ends_with("__"));
        if private {
            Visibility::Private
        } else {
            Visibility::Public
        }
    }

    fn docstring(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Python's docstring is the first string literal in the body, not a
        // preceding comment. Fall back to leading comments when absent.
        if let Some(body) = node.child_by_field_name("body")
            && let Some(statement) = body.named_child(0)
            && statement.kind() == "expression_statement"
            && let Some(expression) = statement.named_child(0)
            && matches!(expression.kind(), "string" | "concatenated_string")
            && let Ok(raw) = node_text(expression, source)
            && let Some(text) = strip_python_string(raw)
        {
            return Some(truncate_doc(&text));
        }

        super::tree_sitter_adapter::default_docstring(node, source)
    }

    fn imports(&self, declaration: Node<'_>, source: &str) -> Vec<ImportRecord> {
        let Ok(text) = node_text(declaration, source) else {
            return Vec::new();
        };
        parse_import_statement(text)
    }
}

fn is_constant_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().any(|ch| ch.is_ascii_alphabetic())
        && name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn truncate_doc(text: &str) -> String {
    let max = 4_096usize;
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut truncated: String = text.chars().take(max).collect();
        truncated.push('…');
        truncated
    }
}

fn strip_python_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let quote_offset = trimmed.find(['\'', '"'])?;
    let literal = &trimmed[quote_offset..];
    let quote = literal.chars().next()?;
    let triple = format!("{quote}{quote}{quote}");
    let inner = if literal.starts_with(&triple) {
        literal.strip_prefix(&triple)?.strip_suffix(&triple)?
    } else {
        literal.strip_prefix(quote)?.strip_suffix(quote)?
    };
    Some(inner.trim().to_string())
}

fn parse_import_statement(text: &str) -> Vec<ImportRecord> {
    let cleaned = collapse_whitespace(&text.replace('\\', " "));
    if let Some(rest) = cleaned.strip_prefix("import ") {
        return parse_plain_imports(rest);
    }
    if let Some(rest) = cleaned.strip_prefix("from ")
        && let Some((module, names)) = rest.split_once(" import ")
    {
        let module = module.trim();
        let names = names.trim().trim_start_matches('(').trim_end_matches(')');
        let mut records = Vec::new();
        for name in split_top_level(names) {
            let (symbol, alias) = split_alias(name);
            if symbol.is_empty() {
                continue;
            }
            records.push(ImportRecord {
                source_path: module.to_string(),
                imported_symbol: symbol,
                alias,
            });
        }
        return records;
    }
    Vec::new()
}

fn parse_plain_imports(text: &str) -> Vec<ImportRecord> {
    let mut records = Vec::new();
    for item in split_top_level(text) {
        let (path, alias) = split_alias(item);
        if path.is_empty() {
            continue;
        }
        match path.rsplit_once('.') {
            Some((source_path, symbol)) => records.push(ImportRecord {
                source_path: source_path.to_string(),
                imported_symbol: symbol.to_string(),
                alias,
            }),
            None => records.push(ImportRecord {
                source_path: String::new(),
                imported_symbol: path,
                alias,
            }),
        }
    }
    records
}

fn split_alias(item: &str) -> (String, Option<String>) {
    match item.rsplit_once(" as ") {
        Some((path, alias)) => {
            let alias = alias.trim();
            (
                path.trim().to_string(),
                if alias.is_empty() {
                    None
                } else {
                    Some(alias.to_string())
                },
            )
        }
        None => (item.trim().to_string(), None),
    }
}

fn split_top_level(text: &str) -> Vec<&str> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;

    const SAMPLE: &str = r#"
"""Module documentation."""
from storage import save as persist
import os.path

MAX_SIZE = 10

class Agent:
    """Agent documentation."""

    def __init__(self):
        self.ready = True

    def execute(self, query):
        return persist(query)

def run():
    agent = Agent()
    agent.execute("x")
    return MAX_SIZE
"#;

    #[test]
    fn test_python_parses_classes_methods_and_calls() {
        let adapter = PythonAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "agent/core.py", SAMPLE).unwrap();

        let uris: Vec<&str> = extraction
            .nodes
            .iter()
            .map(|node| node.uri().as_str())
            .collect();
        assert!(uris.contains(&"sym://repo/agent/core.py#Agent:class"));
        assert!(uris.contains(&"sym://repo/agent/core.py#Agent::__init__:ctor"));
        assert!(uris.contains(&"sym://repo/agent/core.py#Agent::execute:fn"));
        assert!(uris.contains(&"sym://repo/agent/core.py#run:fn"));
        assert!(uris.contains(&"sym://repo/agent/core.py#MAX_SIZE:const"));

        let execute = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Agent::execute:fn"))
            .unwrap();
        assert_eq!(execute.kind(), NodeKind::Method);
        assert_eq!(execute.visibility(), Visibility::Public);
        assert!(execute.signature().unwrap().contains("def execute"));

        let agent = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Agent:class"))
            .unwrap();
        assert_eq!(agent.docstring(), Some("Agent documentation."));

        assert!(
            extraction
                .local_edges
                .iter()
                .any(|edge| edge.kind() == EdgeKind::MemberOf
                    && edge.source_uri().as_str().contains("#Agent:class")
                    && edge.target_uri().as_str().contains("#Agent::execute:fn"))
        );

        let calls: Vec<&str> = extraction
            .pending_calls
            .iter()
            .map(|call| call.call_expr.as_str())
            .collect();
        assert!(calls.contains(&"persist"));
        assert!(calls.contains(&"Agent"));
        assert!(calls.contains(&"agent.execute"));

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
            "storage".to_string(),
            "save".to_string(),
            Some("persist".to_string())
        )));
        assert!(imports.contains(&("os".to_string(), "path".to_string(), None)));
    }

    #[test]
    fn test_python_private_visibility_and_nested_functions() {
        let source = r#"
class Store:
    def _load(self):
        def helper():
            return 1
        return helper()
"#;
        let adapter = PythonAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "store.py", source).unwrap();

        let load = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Store::_load:fn"))
            .unwrap();
        assert_eq!(load.visibility(), Visibility::Private);

        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.uri().as_str().ends_with("#Store::_load::helper:fn"))
        );
    }

    #[test]
    fn test_python_import_parser() {
        assert_eq!(
            parse_import_statement("from a.b import (x, y as z, *)"),
            vec![
                ImportRecord {
                    source_path: "a.b".to_string(),
                    imported_symbol: "x".to_string(),
                    alias: None,
                },
                ImportRecord {
                    source_path: "a.b".to_string(),
                    imported_symbol: "y".to_string(),
                    alias: Some("z".to_string()),
                },
                ImportRecord {
                    source_path: "a.b".to_string(),
                    imported_symbol: "*".to_string(),
                    alias: None,
                },
            ]
        );
        assert_eq!(
            parse_import_statement("import os.path as p, sys"),
            vec![
                ImportRecord {
                    source_path: "os".to_string(),
                    imported_symbol: "path".to_string(),
                    alias: Some("p".to_string()),
                },
                ImportRecord {
                    source_path: String::new(),
                    imported_symbol: "sys".to_string(),
                    alias: None,
                },
            ]
        );
    }

    #[test]
    fn test_python_string_literal_stripping() {
        assert_eq!(
            strip_python_string("\"\"\"hello\nworld\"\"\"").as_deref(),
            Some("hello\nworld")
        );
        assert_eq!(strip_python_string("r'raw'").as_deref(), Some("raw"));
    }
}
