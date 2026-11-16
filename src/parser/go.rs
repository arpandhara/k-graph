use tree_sitter::Node;

use crate::model::{NodeKind, Visibility};

use super::error::ParserError;
use super::language::{ImportRecord, LanguageAdapter, RawExtractedFile};
use super::tree_sitter_adapter::{
    LanguageSyntax, TreeSitterAdapter, collapse_whitespace, node_text,
};

const EXTENSIONS: &[&str] = &["go"];

/// Go ingestion backend.
///
/// Go differs from the other languages in two structural ways:
/// - the package clause is a sibling of the declarations it scopes, so it is
///   supplied as a file-wide [`LanguageSyntax::base_scope`];
/// - a method declares its receiver type as a parameter, so the receiver is
///   supplied by [`LanguageSyntax::definition_scope`] instead of the parent
///   chain.
pub struct GoAdapter {
    inner: TreeSitterAdapter,
}

impl GoAdapter {
    /// Compile the Go grammar and query set.
    pub fn new() -> Result<Self, ParserError> {
        Ok(Self {
            inner: TreeSitterAdapter::new(
                tree_sitter_go::LANGUAGE.into(),
                include_str!("../../queries/go/symbols.scm"),
                include_str!("../../queries/go/calls.scm"),
                EXTENSIONS,
                Box::new(GoSyntax),
            )?,
        })
    }
}

impl LanguageAdapter for GoAdapter {
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

struct GoSyntax;

impl LanguageSyntax for GoSyntax {
    fn language_name(&self) -> &'static str {
        "go"
    }

    fn refine_kind(&self, kind: NodeKind, _name: &str, node: Node<'_>, _source: &str) -> NodeKind {
        // `type_spec` covers structs, interfaces, and aliases. The captured
        // node is the spec itself, so its `type` field decides the real kind.
        if kind == NodeKind::TypeAlias
            && node.kind() == "type_spec"
            && let Some(type_node) = node.child_by_field_name("type")
        {
            return match type_node.kind() {
                "struct_type" => NodeKind::Struct,
                "interface_type" => NodeKind::Interface,
                _ => NodeKind::TypeAlias,
            };
        }
        kind
    }

    fn visibility(
        &self,
        _kind: NodeKind,
        name: &str,
        _node: Node<'_>,
        _source: &str,
    ) -> Visibility {
        // Go exports identifiers that begin with an uppercase letter. The
        // unexported remainder is package-private, which maps to
        // `ModulePrivate`.
        if name.chars().next().is_some_and(char::is_uppercase) {
            Visibility::Public
        } else {
            Visibility::ModulePrivate
        }
    }

    fn docstring(&self, node: Node<'_>, source: &str) -> Option<String> {
        // `type_spec`, `const_spec`, and `var_spec` are nested inside their
        // declaration node, so a comment above the declaration is a sibling of
        // the parent. Prefer a directly attached comment, then the parent.
        if let Some(doc) = super::tree_sitter_adapter::default_docstring(node, source) {
            return Some(doc);
        }
        let parent = node.parent()?;
        if matches!(
            parent.kind(),
            "type_declaration" | "const_declaration" | "var_declaration"
        ) {
            return super::tree_sitter_adapter::default_docstring(parent, source);
        }
        None
    }

    fn imports(&self, declaration: Node<'_>, source: &str) -> Vec<ImportRecord> {
        let Ok(text) = node_text(declaration, source) else {
            return Vec::new();
        };
        parse_import_spec(text)
    }

    fn base_scope(&self, root: Node<'_>, source: &str) -> Option<String> {
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() != "package_clause" {
                continue;
            }
            let mut inner = child.walk();
            for identifier in child.named_children(&mut inner) {
                if identifier.kind() == "package_identifier" {
                    let name = node_text(identifier, source).ok()?.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
        None
    }

    fn definition_scope(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() != "method_declaration" {
            return None;
        }

        let receiver = node.child_by_field_name("receiver")?;
        let mut cursor = receiver.walk();
        let parameter = receiver.named_children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            )
        })?;
        let type_node = parameter.child_by_field_name("type")?;
        let raw = node_text(type_node, source).ok()?;
        normalize_receiver_type(raw)
    }
}

/// Reduce a receiver type expression to its local type name:
/// `*Server` becomes `Server`, `*Box[T]` becomes `Box`, and `pkg.Server`
/// becomes `Server`.
fn normalize_receiver_type(raw: &str) -> Option<String> {
    let mut text = collapse_whitespace(raw);
    while let Some(rest) = text.strip_prefix('*') {
        text = rest.trim_start().to_string();
    }

    let base = text
        .split(['[', '(', ')', '{', '}'])
        .next()
        .unwrap_or(&text)
        .trim();
    let last = base.rsplit('.').next().unwrap_or(base).trim();
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

fn parse_import_spec(text: &str) -> Vec<ImportRecord> {
    let Some((path, path_start)) = extract_quoted(text) else {
        return Vec::new();
    };

    let alias = text[..path_start].trim();
    if path.is_empty() {
        return Vec::new();
    }

    let (imported_symbol, alias) = match alias {
        "" => (default_package_name(path), None),
        "." => ("*".to_string(), None),
        "_" => ("_".to_string(), None),
        other => (default_package_name(path), Some(other.to_string())),
    };

    vec![ImportRecord {
        source_path: path.to_string(),
        imported_symbol,
        alias,
    }]
}

/// Extract the quoted import path and its byte offset.
///
/// Go permits both interpreted strings (`"net/http"`) and raw strings
/// (`` `net/http` ``).
fn extract_quoted(text: &str) -> Option<(&str, usize)> {
    let (start, quote) = text
        .char_indices()
        .find(|(_, ch)| matches!(ch, '"' | '`'))?;
    let rest = &text[start + quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some((&rest[..end], start))
}

fn default_package_name(path: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or(path);
    // Version suffixes such as `gopkg.in/yaml.v2` bind the package name
    // without the major-version suffix.
    last.split('.').next().unwrap_or(last).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;

    const SAMPLE: &str = r#"
// Package server exposes the HTTP API.
package server

import (
    "fmt"
    h "net/http"
    . "math"
    _ "embed"
)

// MaxRetries is the retry limit.
const MaxRetries = 3

var DefaultClient = h.Client{}

var Alpha, Beta = 1, 2

// Server handles requests.
type Server struct {
    // Addr is the listen address.
    Addr string
    hidden int
}

type Handler interface {
    Serve(req string) error
}

type ID = int

const (
    A = 1
    B = 2
)

const X, Y = 1, 2

// Start starts the server.
func (s *Server) Start() error {
    fmt.Println("start")
    return s.serve()
}

// NewServer builds a server.
func NewServer() *Server { return &Server{} }

func helper() {
    local := 1
    var shadowed = local
    const LocalConst = 1
    fmt.Println(MaxRetries, shadowed, LocalConst)
}
"#;

    #[test]
    fn test_go_parses_package_scopes_and_definitions() {
        let adapter = GoAdapter::new().unwrap();
        let extraction = adapter
            .parse_file("repo", "server/http.go", SAMPLE)
            .unwrap();

        let uris: Vec<&str> = extraction
            .nodes
            .iter()
            .map(|node| node.uri().as_str())
            .collect();
        assert!(uris.contains(&"sym://repo/server/http.go#file:file"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::MaxRetries:const"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::DefaultClient:var"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Alpha:var"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Beta:var"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Server:struct"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Server::Addr:field"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Server::hidden:field"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Handler:iface"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Handler::Serve:fn"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::ID:type"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::A:const"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::B:const"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::X:const"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Y:const"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::Server::Start:fn"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::NewServer:fn"));
        assert!(uris.contains(&"sym://repo/server/http.go#server::helper:fn"));
        // Local declarations are not graph nodes.
        assert!(!uris.iter().any(|uri| uri.contains("local")));
        assert!(!uris.iter().any(|uri| uri.contains("shadowed")));
        assert!(!uris.iter().any(|uri| uri.contains("LocalConst")));

        let start = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#server::Server::Start:fn"))
            .unwrap();
        assert_eq!(start.kind(), NodeKind::Method);
        assert_eq!(start.visibility(), Visibility::Public);
        assert_eq!(start.docstring(), Some("Start starts the server."));
        assert!(
            start
                .signature()
                .unwrap()
                .contains("func (s *Server) Start")
        );

        let hidden = extraction
            .nodes
            .iter()
            .find(|node| {
                node.uri()
                    .as_str()
                    .contains("#server::Server::hidden:field")
            })
            .unwrap();
        assert_eq!(hidden.visibility(), Visibility::ModulePrivate);

        let server = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().ends_with("#server::Server:struct"))
            .unwrap();
        assert_eq!(server.docstring(), Some("Server handles requests."));
    }

    #[test]
    fn test_go_containment_and_method_receiver_edges() {
        let adapter = GoAdapter::new().unwrap();
        let extraction = adapter
            .parse_file("repo", "server/http.go", SAMPLE)
            .unwrap();

        assert!(extraction.local_edges.iter().any(|edge| {
            edge.kind() == EdgeKind::MemberOf
                && edge
                    .source_uri()
                    .as_str()
                    .ends_with("#server::Server:struct")
                && edge
                    .target_uri()
                    .as_str()
                    .ends_with("#server::Server::Start:fn")
        }));
        assert!(extraction.local_edges.iter().any(|edge| {
            edge.kind() == EdgeKind::MemberOf
                && edge
                    .source_uri()
                    .as_str()
                    .ends_with("#server::Handler:iface")
                && edge
                    .target_uri()
                    .as_str()
                    .ends_with("#server::Handler::Serve:fn")
        }));
        assert!(extraction.local_edges.iter().any(|edge| {
            edge.kind() == EdgeKind::Contains
                && edge.source_uri().as_str().ends_with("#file:file")
                && edge
                    .target_uri()
                    .as_str()
                    .ends_with("#server::NewServer:fn")
        }));
    }

    #[test]
    fn test_go_calls_and_imports() {
        let adapter = GoAdapter::new().unwrap();
        let extraction = adapter
            .parse_file("repo", "server/http.go", SAMPLE)
            .unwrap();

        let calls: Vec<&str> = extraction
            .pending_calls
            .iter()
            .map(|call| call.call_expr.as_str())
            .collect();
        assert!(calls.contains(&"fmt.Println"));
        assert!(calls.contains(&"s.serve"));
        assert!(extraction.pending_calls.iter().all(|call| {
            call.caller_uri.contains("#server::Server::Start:fn")
                || call.caller_uri.contains("#server::NewServer:fn")
                || call.caller_uri.contains("#server::helper:fn")
        }));

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
        assert!(imports.contains(&("fmt".to_string(), "fmt".to_string(), None)));
        assert!(imports.contains(&(
            "net/http".to_string(),
            "http".to_string(),
            Some("h".to_string())
        )));
        assert!(imports.contains(&("math".to_string(), "*".to_string(), None)));
        assert!(imports.contains(&("embed".to_string(), "_".to_string(), None)));
    }

    #[test]
    fn test_go_receiver_and_import_normalization() {
        assert_eq!(
            normalize_receiver_type("*Server").as_deref(),
            Some("Server")
        );
        assert_eq!(normalize_receiver_type("*Box[T]").as_deref(), Some("Box"));
        assert_eq!(
            normalize_receiver_type("pkg.Server").as_deref(),
            Some("Server")
        );
        assert_eq!(
            parse_import_spec("h \"net/http\""),
            vec![ImportRecord {
                source_path: "net/http".to_string(),
                imported_symbol: "http".to_string(),
                alias: Some("h".to_string()),
            }]
        );
        assert_eq!(default_package_name("gopkg.in/yaml.v2"), "yaml".to_string());
    }

    #[test]
    fn test_go_deterministic_output() {
        let adapter = GoAdapter::new().unwrap();
        let first = adapter
            .parse_file("repo", "server/http.go", SAMPLE)
            .unwrap();
        let second = adapter
            .parse_file("repo", "server/http.go", SAMPLE)
            .unwrap();
        assert_eq!(first, second);
    }
}
