use std::fmt;
use std::str::FromStr;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur when parsing a SymbolUri.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum UriParseError {
    #[error("URI must begin with 'sym://', got: '{0}'")]
    MissingScheme(String),
    #[error("URI missing repository or file path segment: '{0}'")]
    MissingPath(String),
    #[error("URI missing fragment indicator '#' in: '{0}'")]
    MissingFragment(String),
    #[error("Invalid fragment format in URI: '{0}'")]
    InvalidFragment(String),
}

/// A parsed representation of the components inside a SymbolUri.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbolUri {
    pub repo: String,
    pub file_path: String,
    pub scope: Option<String>,
    pub name: String,
    pub kind_suffix: String,
}

/// A canonical, immutable, globally unique identifier for code symbols across repositories and files.
///
/// Syntax:
/// `sym://<repo>/<path>#<scope>::<name>:<kind>`
/// or without scope:
/// `sym://<repo>/<path>#<name>:<kind>`
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolUri(String);

impl SymbolUri {
    /// Create a new `SymbolUri` from individual symbol components.
    pub fn new(repo: &str, file_path: &str, scope: Option<&str>, name: &str, kind_suffix: &str) -> Self {
        let clean_repo = repo.trim_matches('/');
        let clean_path = file_path.trim_matches('/');
        let clean_name = name.trim();
        let clean_kind = kind_suffix.trim().to_ascii_lowercase();

        let fragment = match scope {
            Some(s) if !s.trim().is_empty() => {
                format!("{}::{}:{}", s.trim(), clean_name, clean_kind)
            }
            _ => format!("{}:{}", clean_name, clean_kind),
        };

        Self(format!("sym://{}/{}#{}", clean_repo, clean_path, fragment))
    }

    /// Helper to construct a URI representing a whole file.
    pub fn for_file(repo: &str, file_path: &str) -> Self {
        let clean_repo = repo.trim_matches('/');
        let clean_path = file_path.trim_matches('/');
        Self(format!("sym://{}/{}#file:file", clean_repo, clean_path))
    }

    /// Helper to construct a URI representing a module.
    pub fn for_module(repo: &str, file_path: &str, module_name: &str) -> Self {
        let clean_repo = repo.trim_matches('/');
        let clean_path = file_path.trim_matches('/');
        Self(format!("sym://{}/{}#{}:mod", clean_repo, clean_path, module_name.trim()))
    }

    /// Access the raw string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse the components out of the URI string.
    pub fn parse(&self) -> Result<ParsedSymbolUri, UriParseError> {
        let raw = &self.0;
        let stripped = raw
            .strip_prefix("sym://")
            .ok_or_else(|| UriParseError::MissingScheme(raw.clone()))?;

        let (path_part, fragment_part) = stripped
            .split_once('#')
            .ok_or_else(|| UriParseError::MissingFragment(raw.clone()))?;

        let (repo, file_path) = path_part
            .split_once('/')
            .ok_or_else(|| UriParseError::MissingPath(raw.clone()))?;

        if repo.is_empty() || file_path.is_empty() {
            return Err(UriParseError::MissingPath(raw.clone()));
        }

        // Fragment part: [scope::]name:kind
        let (symbol_part, kind_suffix) = fragment_part
            .rsplit_once(':')
            .ok_or_else(|| UriParseError::InvalidFragment(raw.clone()))?;

        let (scope, name) = if let Some((s, n)) = symbol_part.rsplit_once("::") {
            (Some(s.to_string()), n.to_string())
        } else {
            (None, symbol_part.to_string())
        };

        Ok(ParsedSymbolUri {
            repo: repo.to_string(),
            file_path: file_path.to_string(),
            scope,
            name,
            kind_suffix: kind_suffix.to_string(),
        })
    }

    /// Return the file path portion of the URI.
    pub fn file_path(&self) -> String {
        self.parse()
            .map(|p| p.file_path)
            .unwrap_or_else(|_| "".to_string())
    }

    /// Return the repo portion of the URI.
    pub fn repo(&self) -> String {
        self.parse()
            .map(|p| p.repo)
            .unwrap_or_else(|_| "".to_string())
    }

    /// Return the symbol name portion of the URI.
    pub fn name(&self) -> String {
        self.parse()
            .map(|p| p.name)
            .unwrap_or_else(|_| "".to_string())
    }
}

impl fmt::Display for SymbolUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for SymbolUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SymbolUri(\"{}\")", self.0)
    }
}

impl FromStr for SymbolUri {
    type Err = UriParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uri = Self(s.to_string());
        // Validate by parsing
        uri.parse()?;
        Ok(uri)
    }
}

impl std::ops::Deref for SymbolUri {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for SymbolUri {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_creation_and_parse_with_scope() {
        let uri = SymbolUri::new("rusty", "src/index.rs", Some("Index"), "load", "fn");
        assert_eq!(uri.as_str(), "sym://rusty/src/index.rs#Index::load:fn");

        let parsed = uri.parse().expect("Failed to parse URI");
        assert_eq!(parsed.repo, "rusty");
        assert_eq!(parsed.file_path, "src/index.rs");
        assert_eq!(parsed.scope.as_deref(), Some("Index"));
        assert_eq!(parsed.name, "load");
        assert_eq!(parsed.kind_suffix, "fn");
    }

    #[test]
    fn test_uri_creation_without_scope() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn");
        assert_eq!(uri.as_str(), "sym://rusty/src/add.rs#add_file:fn");

        let parsed = uri.parse().expect("Failed to parse URI");
        assert_eq!(parsed.repo, "rusty");
        assert_eq!(parsed.file_path, "src/add.rs");
        assert_eq!(parsed.scope, None);
        assert_eq!(parsed.name, "add_file");
        assert_eq!(parsed.kind_suffix, "fn");
    }

    #[test]
    fn test_uri_file_and_module_helpers() {
        let file_uri = SymbolUri::for_file("rusty", "src/lib.rs");
        assert_eq!(file_uri.as_str(), "sym://rusty/src/lib.rs#file:file");

        let mod_uri = SymbolUri::for_module("rusty", "src/engine/mod.rs", "engine");
        assert_eq!(mod_uri.as_str(), "sym://rusty/src/engine/mod.rs#engine:mod");
    }

    #[test]
    fn test_uri_nested_scopes() {
        let uri = SymbolUri::new("rusty", "src/tui/app.rs", Some("App::State"), "draw", "fn");
        assert_eq!(uri.as_str(), "sym://rusty/src/tui/app.rs#App::State::draw:fn");

        let parsed = uri.parse().unwrap();
        assert_eq!(parsed.scope.as_deref(), Some("App::State"));
        assert_eq!(parsed.name, "draw");
    }

    #[test]
    fn test_uri_invalid_parsing() {
        assert!(SymbolUri("http://foo.bar".to_string()).parse().is_err());
        assert!(SymbolUri("sym://repo".to_string()).parse().is_err());
        assert!(SymbolUri("sym://repo/file.rs".to_string()).parse().is_err());
        assert!(SymbolUri("sym://repo/file.rs#symbol".to_string()).parse().is_err());
    }

    #[test]
    fn test_uri_serde_roundtrip() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn");
        let json = serde_json::to_string(&uri).unwrap();
        assert_eq!(json, "\"sym://rusty/src/add.rs#add_file:fn\"");

        let deserialized: SymbolUri = serde_json::from_str(&json).unwrap();
        assert_eq!(uri, deserialized);
    }
}
