use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Errors that can occur when constructing or parsing a SymbolUri.
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
    #[error("Invalid repository segment in URI: '{0}'")]
    InvalidRepo(String),
    #[error("Invalid relative file path in URI: '{0}'")]
    InvalidPath(String),
    #[error("Invalid symbol name in URI: '{0}'")]
    InvalidName(String),
    #[error("Invalid scope in URI: '{0}'")]
    InvalidScope(String),
    #[error("Invalid kind suffix in URI: '{0}'")]
    InvalidKind(String),
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
///
/// The inner string is private and every construction path validates the
/// grammar, normalizes the kind suffix to lowercase, and rejects path traversal.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct SymbolUri(String);

impl SymbolUri {
    /// Create a new `SymbolUri` from individual symbol components.
    ///
    /// Returns an error when a component contains a reserved delimiter,
    /// control character, an absolute path, or a `..` path segment.
    pub fn new(
        repo: &str,
        file_path: &str,
        scope: Option<&str>,
        name: &str,
        kind_suffix: &str,
    ) -> Result<Self, UriParseError> {
        let clean_repo = repo.trim();
        let clean_path = file_path.trim();
        let clean_name = name.trim();
        let clean_kind = validate_kind(kind_suffix.trim())?;
        let clean_scope = scope.map(str::trim).filter(|s| !s.is_empty());

        validate_repo(clean_repo, repo)?;
        validate_file_path(clean_path, file_path)?;
        validate_name(clean_name)?;
        if let Some(scope) = clean_scope {
            validate_scope(scope)?;
        }

        let fragment = match clean_scope {
            Some(scope) => format!("{}::{}:{}", scope, clean_name, clean_kind),
            None => format!("{}:{}", clean_name, clean_kind),
        };

        Ok(Self(format!(
            "sym://{}/{}#{}",
            clean_repo, clean_path, fragment
        )))
    }

    /// Helper to construct a URI representing a whole file.
    pub fn for_file(repo: &str, file_path: &str) -> Result<Self, UriParseError> {
        Self::new(repo, file_path, None, "file", "file")
    }

    /// Helper to construct a URI representing a module.
    pub fn for_module(
        repo: &str,
        file_path: &str,
        module_name: &str,
    ) -> Result<Self, UriParseError> {
        Self::new(repo, file_path, None, module_name, "mod")
    }

    /// Access the raw string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse the components out of the URI string.
    pub fn parse(&self) -> Result<ParsedSymbolUri, UriParseError> {
        Self::parse_inner(&self.0)
    }

    /// Return the file path portion of the URI.
    ///
    /// The URI is guaranteed valid at construction time, so an error here
    /// indicates an internal invariant violation rather than user input.
    pub fn file_path(&self) -> Result<String, UriParseError> {
        Ok(self.parse()?.file_path)
    }

    /// Return the repo portion of the URI.
    pub fn repo(&self) -> Result<String, UriParseError> {
        Ok(self.parse()?.repo)
    }

    /// Return the symbol name portion of the URI.
    pub fn name(&self) -> Result<String, UriParseError> {
        Ok(self.parse()?.name)
    }

    fn parse_inner(raw: &str) -> Result<ParsedSymbolUri, UriParseError> {
        let stripped = raw
            .strip_prefix("sym://")
            .ok_or_else(|| UriParseError::MissingScheme(raw.to_string()))?;

        let (path_part, fragment_part) = stripped
            .split_once('#')
            .ok_or_else(|| UriParseError::MissingFragment(raw.to_string()))?;

        // A second '#' makes the component split ambiguous.
        if fragment_part.contains('#') {
            return Err(UriParseError::InvalidFragment(raw.to_string()));
        }

        let (repo, file_path) = path_part
            .split_once('/')
            .ok_or_else(|| UriParseError::MissingPath(raw.to_string()))?;

        validate_repo(repo, raw)?;
        validate_file_path(file_path, raw)?;

        // Fragment part: [scope::]name:kind
        let (symbol_part, kind_raw) = fragment_part
            .rsplit_once(':')
            .ok_or_else(|| UriParseError::InvalidFragment(raw.to_string()))?;

        let kind_suffix = validate_kind(kind_raw)?;

        let parts: Vec<&str> = symbol_part.split("::").collect();
        if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
            return Err(UriParseError::InvalidFragment(raw.to_string()));
        }

        let name = parts[parts.len() - 1].to_string();
        validate_name(&name)?;

        let scope = if parts.len() > 1 {
            let scope = parts[..parts.len() - 1].join("::");
            validate_scope(&scope)?;
            Some(scope)
        } else {
            None
        };

        Ok(ParsedSymbolUri {
            repo: repo.to_string(),
            file_path: file_path.to_string(),
            scope,
            name,
            kind_suffix,
        })
    }
}

fn validate_repo(repo: &str, raw: &str) -> Result<(), UriParseError> {
    if repo.is_empty() {
        return Err(UriParseError::MissingPath(raw.to_string()));
    }

    if repo.trim() != repo
        || repo
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '#' | ':' | '\\'))
    {
        return Err(UriParseError::InvalidRepo(repo.to_string()));
    }

    Ok(())
}

fn validate_file_path(path: &str, raw: &str) -> Result<(), UriParseError> {
    if path.is_empty() {
        return Err(UriParseError::MissingPath(raw.to_string()));
    }

    if path.trim() != path
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '#' | ':'))
    {
        return Err(UriParseError::InvalidPath(path.to_string()));
    }

    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(UriParseError::InvalidPath(path.to_string()));
        }
    }

    Ok(())
}

fn validate_name(name: &str) -> Result<(), UriParseError> {
    if name.is_empty()
        || name.trim() != name
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '#' | ':'))
    {
        return Err(UriParseError::InvalidName(name.to_string()));
    }

    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), UriParseError> {
    if scope.trim() != scope {
        return Err(UriParseError::InvalidScope(scope.to_string()));
    }

    for part in scope.split("::") {
        if part.is_empty()
            || part.trim() != part
            || part
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '#' | ':'))
        {
            return Err(UriParseError::InvalidScope(scope.to_string()));
        }
    }

    Ok(())
}

fn validate_kind(kind: &str) -> Result<String, UriParseError> {
    if kind.is_empty() || kind.trim() != kind {
        return Err(UriParseError::InvalidKind(kind.to_string()));
    }

    let canonical = kind.to_ascii_lowercase();
    if !canonical
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(UriParseError::InvalidKind(kind.to_string()));
    }

    Ok(canonical)
}

impl<'de> Deserialize<'de> for SymbolUri {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
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
        // Validate, then rebuild the canonical form. This normalizes the kind
        // suffix so all construction paths produce the same identity.
        let parsed = Self::parse_inner(s)?;

        let fragment = match parsed.scope.as_deref() {
            Some(scope) => format!("{}::{}:{}", scope, parsed.name, parsed.kind_suffix),
            None => format!("{}:{}", parsed.name, parsed.kind_suffix),
        };

        Ok(Self(format!(
            "sym://{}/{}#{}",
            parsed.repo, parsed.file_path, fragment
        )))
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
        let uri = SymbolUri::new("rusty", "src/index.rs", Some("Index"), "load", "fn").unwrap();
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
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
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
        let file_uri = SymbolUri::for_file("rusty", "src/lib.rs").unwrap();
        assert_eq!(file_uri.as_str(), "sym://rusty/src/lib.rs#file:file");

        let mod_uri = SymbolUri::for_module("rusty", "src/engine/mod.rs", "engine").unwrap();
        assert_eq!(mod_uri.as_str(), "sym://rusty/src/engine/mod.rs#engine:mod");
    }

    #[test]
    fn test_uri_nested_scopes() {
        let uri =
            SymbolUri::new("rusty", "src/tui/app.rs", Some("App::State"), "draw", "fn").unwrap();
        assert_eq!(
            uri.as_str(),
            "sym://rusty/src/tui/app.rs#App::State::draw:fn"
        );

        let parsed = uri.parse().unwrap();
        assert_eq!(parsed.scope.as_deref(), Some("App::State"));
        assert_eq!(parsed.name, "draw");
    }

    #[test]
    fn test_uri_kind_is_canonicalized() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "FN").unwrap();
        assert_eq!(uri.as_str(), "sym://rusty/src/add.rs#add_file:fn");

        let parsed: SymbolUri = "sym://rusty/src/add.rs#add_file:Fn".parse().unwrap();
        assert_eq!(parsed.as_str(), "sym://rusty/src/add.rs#add_file:fn");
    }

    #[test]
    fn test_uri_invalid_parsing() {
        assert!(SymbolUri("http://foo.bar".to_string()).parse().is_err());
        assert!(SymbolUri("sym://repo".to_string()).parse().is_err());
        assert!(SymbolUri("sym://repo/file.rs".to_string()).parse().is_err());
        assert!(
            SymbolUri("sym://repo/file.rs#symbol".to_string())
                .parse()
                .is_err()
        );
    }

    #[test]
    fn test_uri_rejects_ambiguous_and_traversal_components() {
        assert!(SymbolUri::new("rusty", "src/a#b.rs", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "../etc/passwd", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "/etc/passwd", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src/./a.rs", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src//a.rs", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src/a.rs\u{0}", None, "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src/a.rs", Some("S::"), "f", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src/a.rs", None, "a:b", "fn").is_err());
        assert!(SymbolUri::new("rusty", "src/a.rs", None, "f", "f n").is_err());
        assert!(SymbolUri::new("ru/sty", "src/a.rs", None, "f", "fn").is_err());

        assert!(SymbolUri::from_str("sym://rusty//etc/passwd#f:fn").is_err());
        assert!(SymbolUri::from_str("sym://rusty/src/../a.rs#f:fn").is_err());
        assert!(SymbolUri::from_str("sym://rusty/src/a.rs#a::b::c:fn").is_ok());
    }

    #[test]
    fn test_uri_getters_report_errors_and_values() {
        let uri = SymbolUri::new("rusty", "src/add.rs", Some("Repo"), "add_file", "fn").unwrap();
        assert_eq!(uri.repo().unwrap(), "rusty");
        assert_eq!(uri.file_path().unwrap(), "src/add.rs");
        assert_eq!(uri.name().unwrap(), "add_file");

        let broken = SymbolUri("not-a-uri".to_string());
        assert!(broken.repo().is_err());
        assert!(broken.file_path().is_err());
        assert!(broken.name().is_err());
    }

    #[test]
    fn test_uri_serde_roundtrip() {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        let json = serde_json::to_string(&uri).unwrap();
        assert_eq!(json, "\"sym://rusty/src/add.rs#add_file:fn\"");

        let deserialized: SymbolUri = serde_json::from_str(&json).unwrap();
        assert_eq!(uri, deserialized);
    }

    #[test]
    fn test_uri_deserialization_rejects_invalid_values() {
        assert!(serde_json::from_str::<SymbolUri>("\"not-a-uri\"").is_err());
        assert!(serde_json::from_str::<SymbolUri>("\"sym://r/../etc#f:fn\"").is_err());
        assert!(serde_json::from_str::<SymbolUri>("\"sym://r/a#f:FN\"").is_ok());
    }
}
