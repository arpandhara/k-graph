use tree_sitter::Node;

use crate::model::{NodeKind, Visibility};

use super::error::ParserError;
use super::language::{ImportRecord, LanguageAdapter, RawExtractedFile};
use super::tree_sitter_adapter::{
    LanguageSyntax, TreeSitterAdapter, collapse_whitespace, extension_of, node_text,
};

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts"];

/// TypeScript / TSX ingestion backend.
///
/// TypeScript and TSX need separate grammars but share one query and one set of
/// syntax rules. The adapter selects the grammar from the file extension.
pub struct TypeScriptAdapter {
    typescript: TreeSitterAdapter,
    tsx: TreeSitterAdapter,
}

impl TypeScriptAdapter {
    /// Compile both TypeScript grammars and the shared query set.
    pub fn new() -> Result<Self, ParserError> {
        let symbols = include_str!("../../queries/typescript/symbols.scm");
        let references = include_str!("../../queries/typescript/calls.scm");

        Ok(Self {
            typescript: TreeSitterAdapter::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                symbols,
                references,
                EXTENSIONS,
                Box::new(TypeScriptSyntax),
            )?,
            tsx: TreeSitterAdapter::new(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                symbols,
                references,
                EXTENSIONS,
                Box::new(TypeScriptSyntax),
            )?,
        })
    }

    fn backend(&self, file_path: &str) -> &TreeSitterAdapter {
        match extension_of(file_path).as_deref() {
            Some("tsx") => &self.tsx,
            _ => &self.typescript,
        }
    }
}

impl LanguageAdapter for TypeScriptAdapter {
    fn language_name(&self) -> &'static str {
        "typescript"
    }

    fn can_handle(&self, extension: &str) -> bool {
        self.typescript.can_handle(extension)
    }

    fn parse_file(
        &self,
        repo_name: &str,
        file_path: &str,
        source: &str,
    ) -> Result<RawExtractedFile, ParserError> {
        self.backend(file_path)
            .parse_file(repo_name, file_path, source)
    }
}

struct TypeScriptSyntax;

impl LanguageSyntax for TypeScriptSyntax {
    fn language_name(&self) -> &'static str {
        "typescript"
    }

    fn refine_kind(&self, kind: NodeKind, name: &str, _node: Node<'_>, _source: &str) -> NodeKind {
        if kind == NodeKind::Method && name == "constructor" {
            NodeKind::Constructor
        } else {
            kind
        }
    }

    fn visibility(&self, kind: NodeKind, name: &str, node: Node<'_>, source: &str) -> Visibility {
        if let Some(modifier) = accessibility_modifier(node, source) {
            return match modifier.as_str() {
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                "public" => Visibility::Public,
                _ => Visibility::Public,
            };
        }

        // `#name` is a runtime-private class member.
        if name.starts_with('#') || node_text(node, source).unwrap_or_default().starts_with('#') {
            return Visibility::Private;
        }

        // Exported declarations are public; interface members are public by
        // definition; other class members default to public in TypeScript.
        if has_ancestor(node, "export_statement") {
            return Visibility::Public;
        }
        if has_ancestor(node, "interface_body") || matches!(kind, NodeKind::Property) {
            return Visibility::Public;
        }
        if has_ancestor(node, "class_body") {
            return Visibility::Public;
        }

        Visibility::ModulePrivate
    }

    fn imports(&self, declaration: Node<'_>, source: &str) -> Vec<ImportRecord> {
        let Ok(text) = node_text(declaration, source) else {
            return Vec::new();
        };
        parse_import_statement(text)
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

fn accessibility_modifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "accessibility_modifier" {
            return node_text(child, source)
                .ok()
                .map(|text| text.trim().to_string());
        }
    }
    None
}

fn parse_import_statement(text: &str) -> Vec<ImportRecord> {
    let collapsed = collapse_whitespace(text);
    let Some(rest) = collapsed.strip_prefix("import ") else {
        return Vec::new();
    };

    // Side-effect import: `import "./styles.css";`
    let (clause, source) = match rest.rsplit_once(" from ") {
        Some((clause, source)) => {
            let Some(path) = extract_quoted(source) else {
                return Vec::new();
            };
            (clause.trim(), path)
        }
        None => {
            let Some(path) = extract_quoted(rest) else {
                return Vec::new();
            };
            ("", path)
        }
    };

    let clause = clause.trim().trim_start_matches("type ").trim();
    if clause.is_empty() {
        return vec![ImportRecord {
            source_path: source,
            imported_symbol: "*".to_string(),
            alias: None,
        }];
    }

    let mut records = Vec::new();

    // `import * as ns from "..."`.
    if let Some(alias) = clause.strip_prefix("* as ") {
        records.push(ImportRecord {
            source_path: source,
            imported_symbol: "*".to_string(),
            alias: Some(alias.trim().to_string()),
        });
        return records;
    }

    // `import { a, b as c } from "..."`.
    if clause.starts_with('{') {
        records.extend(parse_named_imports(&source, clause));
        return records;
    }

    // `import Default, { named } from "..."` or a bare default import.
    let (default_name, remainder) = match clause.split_once(',') {
        Some((default_name, remainder)) => (default_name.trim(), remainder.trim()),
        None => (clause, ""),
    };

    if !default_name.is_empty() {
        records.push(ImportRecord {
            source_path: source.clone(),
            imported_symbol: "default".to_string(),
            alias: Some(default_name.to_string()),
        });
    }

    if remainder.starts_with('{') {
        records.extend(parse_named_imports(&source, remainder));
    } else if let Some(alias) = remainder.strip_prefix("* as ") {
        records.push(ImportRecord {
            source_path: source,
            imported_symbol: "*".to_string(),
            alias: Some(alias.trim().to_string()),
        });
    }

    records
}

fn parse_named_imports(source: &str, clause: &str) -> Vec<ImportRecord> {
    let inner = clause
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();

    inner
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim().trim_start_matches("type ").trim();
            if entry.is_empty() {
                return None;
            }
            let (symbol, alias) = match entry.rsplit_once(" as ") {
                Some((symbol, alias)) => (symbol.trim(), Some(alias.trim().to_string())),
                None => (entry, None),
            };
            if symbol.is_empty() {
                return None;
            }
            Some(ImportRecord {
                source_path: source.to_string(),
                imported_symbol: symbol.to_string(),
                alias,
            })
        })
        .collect()
}

fn extract_quoted(text: &str) -> Option<String> {
    let start = text.find(['\'', '"'])?;
    let quote = text[start..].chars().next()?;
    let rest = &text[start + quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;

    const SAMPLE: &str = r#"
import { save as persist, load } from "./storage";
import * as utils from "./utils";

/** Persists values. */
export interface Saver {
  save(value: string): void;
}

export class Store implements Saver {
  private count = 0;

  save(value: string): void {
    helper(value);
  }
}

function helper(value: string) {
  console.log(value);
  persist(value);
}

const arrow = (n: number) => n + 1;
"#;

    #[test]
    fn test_typescript_parses_definitions_and_calls() {
        let adapter = TypeScriptAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/store.ts", SAMPLE).unwrap();

        let uris: Vec<&str> = extraction
            .nodes
            .iter()
            .map(|node| node.uri().as_str())
            .collect();
        assert!(uris.contains(&"sym://repo/src/store.ts#Saver:iface"));
        assert!(uris.contains(&"sym://repo/src/store.ts#Saver::save:fn"));
        assert!(uris.contains(&"sym://repo/src/store.ts#Store:class"));
        assert!(uris.contains(&"sym://repo/src/store.ts#Store::save:fn"));
        assert!(uris.contains(&"sym://repo/src/store.ts#Store::count:field"));
        assert!(uris.contains(&"sym://repo/src/store.ts#helper:fn"));
        assert!(uris.contains(&"sym://repo/src/store.ts#arrow:fn"));

        let store = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Store:class"))
            .unwrap();
        assert_eq!(store.visibility(), Visibility::Public);

        let private_field = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Store::count:field"))
            .unwrap();
        assert_eq!(private_field.visibility(), Visibility::Private);

        let saver = extraction
            .nodes
            .iter()
            .find(|node| node.uri().as_str().contains("#Saver:iface"))
            .unwrap();
        assert_eq!(saver.docstring(), Some("Persists values."));

        assert!(
            extraction
                .local_edges
                .iter()
                .any(|edge| edge.kind() == EdgeKind::MemberOf
                    && edge.source_uri().as_str().contains("#Store:class")
                    && edge.target_uri().as_str().contains("#Store::save:fn"))
        );

        let calls: Vec<&str> = extraction
            .pending_calls
            .iter()
            .map(|call| call.call_expr.as_str())
            .collect();
        assert!(calls.contains(&"helper"));
        assert!(calls.contains(&"console.log"));
        assert!(calls.contains(&"persist"));
    }

    #[test]
    fn test_typescript_import_parser() {
        assert_eq!(
            parse_import_statement("import { save as persist, load } from './storage';"),
            vec![
                ImportRecord {
                    source_path: "./storage".to_string(),
                    imported_symbol: "save".to_string(),
                    alias: Some("persist".to_string()),
                },
                ImportRecord {
                    source_path: "./storage".to_string(),
                    imported_symbol: "load".to_string(),
                    alias: None,
                },
            ]
        );
        assert_eq!(
            parse_import_statement("import * as utils from \"./utils\";"),
            vec![ImportRecord {
                source_path: "./utils".to_string(),
                imported_symbol: "*".to_string(),
                alias: Some("utils".to_string()),
            }]
        );
        assert_eq!(
            parse_import_statement("import Default, { named } from \"../mod\";"),
            vec![
                ImportRecord {
                    source_path: "../mod".to_string(),
                    imported_symbol: "default".to_string(),
                    alias: Some("Default".to_string()),
                },
                ImportRecord {
                    source_path: "../mod".to_string(),
                    imported_symbol: "named".to_string(),
                    alias: None,
                },
            ]
        );
        assert_eq!(
            parse_import_statement("import \"./side-effect\";"),
            vec![ImportRecord {
                source_path: "./side-effect".to_string(),
                imported_symbol: "*".to_string(),
                alias: None,
            }]
        );
    }

    #[test]
    fn test_tsx_grammar_is_selected_by_extension() {
        let source = r#"
import React from "react";

export function App() {
  return <button onClick={() => helper()}>Go</button>;
}

function helper() {}
"#;
        let adapter = TypeScriptAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/App.tsx", source).unwrap();
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.uri().as_str().ends_with("#App:fn"))
        );
        assert!(
            extraction
                .pending_calls
                .iter()
                .any(|call| call.call_expr == "helper")
        );
    }
}
