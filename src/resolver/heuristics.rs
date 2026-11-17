use std::collections::BTreeSet;

use super::scope::{Candidate, ScopeChain};

/// How strongly an import path identifies a candidate file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModuleMatch {
    /// No component of the import path appears in the candidate path.
    None,
    /// Only the file stem matches (`utils` for `src/utils.ts`).
    Stem,
    /// A trailing part of the path matches (`db` for `src/db.rs`).
    Suffix,
    /// The full module path matches (`pkg/db` for `pkg/db.py`).
    Exact,
}

impl ModuleMatch {
    /// Ranking contribution of this match.
    pub fn score(self) -> i32 {
        match self {
            ModuleMatch::None => 0,
            ModuleMatch::Stem => 150,
            ModuleMatch::Suffix => 400,
            ModuleMatch::Exact => 600,
        }
    }
}

/// A candidate paired with the heuristic score assigned to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedCandidate {
    pub candidate: Candidate,
    pub score: i32,
}

/// Directory portion of a repository-relative path.
pub fn directory_of(file_path: &str) -> &str {
    file_path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .unwrap_or("")
}

/// Path-proximity score between the caller's file and a candidate's file.
///
/// Resolution is deliberately lexical before it is semantic: code that lives in
/// the same file or directory is a better guess than code in an unrelated
/// subtree. The gaps between tiers are large so a scope match cannot outweigh a
/// same-file match.
pub fn proximity_score(caller_file: &str, candidate_file: &str) -> i32 {
    if caller_file == candidate_file {
        return 1_000;
    }

    let caller_directory = directory_of(caller_file);
    let candidate_directory = directory_of(candidate_file);
    if caller_directory == candidate_directory {
        return 500;
    }

    let caller_parts: Vec<&str> = caller_directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let candidate_parts: Vec<&str> = candidate_directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let shared = caller_parts
        .iter()
        .zip(candidate_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();

    100 + (shared as i32) * 25
}

/// Lexical-scope affinity between the caller and a candidate.
///
/// A definition in an enclosing scope is visible to the caller, so shared scope
/// components are a strong signal. An exact scope match gets an additional
/// bonus to prefer siblings over shallower definitions.
pub fn scope_score(caller_scope: &ScopeChain, candidate_scope: &ScopeChain) -> i32 {
    let shared = caller_scope.common_prefix_len(candidate_scope) as i32;
    let exact = i32::from(caller_scope == candidate_scope) * 60;
    shared * 40 + exact
}

/// Match an import path against a candidate file path.
///
/// The import path is normalized to a `/`-separated module path. Both the file
/// without its extension and its directory are considered, which covers
/// `crate::db` -> `src/db.rs`, Python `pkg.db` -> `pkg/db.py`, TypeScript
/// `./util` -> `src/util.ts`, and Go `net/http` -> `net/http.go`.
pub fn module_match(import_path: &str, file_path: &str) -> ModuleMatch {
    let import = normalize_module_path(import_path);
    if import.is_empty() {
        return ModuleMatch::None;
    }

    let keys = FileKeys::new(file_path);

    if keys.module_path == import || keys.directory == import {
        return ModuleMatch::Exact;
    }

    if ends_with_segment(&keys.module_path, &import)
        || ends_with_segment(&keys.directory, &import)
        || ends_with_segment(&import, &keys.module_path)
    {
        return ModuleMatch::Suffix;
    }

    if keys.stem == import || ends_with_segment(&import, &keys.stem) {
        return ModuleMatch::Stem;
    }

    ModuleMatch::None
}

/// Score contribution of a module match.
pub fn module_match_score(import_path: &str, file_path: &str) -> i32 {
    module_match(import_path, file_path).score()
}

/// Rank candidates by proximity, local scope, and optional import module match.
///
/// Ties are broken by canonical URI so the result is independent of graph
/// insertion order. Callers should use [`unique_best`] when a tied top score
/// means the link is genuinely ambiguous.
pub fn rank_candidates(
    caller_file: &str,
    caller_scope: &ScopeChain,
    candidates: &[Candidate],
) -> Vec<RankedCandidate> {
    rank_candidates_with_modules(caller_file, caller_scope, candidates, &[])
}

/// Rank candidates with an additional bonus for each matching import module.
pub fn rank_candidates_with_modules(
    caller_file: &str,
    caller_scope: &ScopeChain,
    candidates: &[Candidate],
    module_paths: &[String],
) -> Vec<RankedCandidate> {
    let mut seen = BTreeSet::new();
    let mut ranked = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        if !seen.insert(candidate.uri.clone()) {
            continue;
        }

        let mut score = proximity_score(caller_file, &candidate.file_path)
            + scope_score(caller_scope, &candidate.scope);

        if !module_paths.is_empty() {
            score += module_paths
                .iter()
                .map(|path| module_match_score(path, &candidate.file_path))
                .max()
                .unwrap_or(0);
        }

        ranked.push(RankedCandidate {
            candidate: candidate.clone(),
            score,
        });
    }

    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.candidate.uri.cmp(&right.candidate.uri))
    });
    ranked
}

/// Return the top candidate only when its score is strictly greater than the
/// runner-up's. A tie means the resolver should report an ambiguity instead of
/// inventing an edge.
pub fn unique_best(ranked: &[RankedCandidate]) -> Option<&RankedCandidate> {
    let best = ranked.first()?;
    if ranked
        .get(1)
        .is_some_and(|runner_up| runner_up.score == best.score)
    {
        None
    } else {
        Some(best)
    }
}

struct FileKeys {
    module_path: String,
    directory: String,
    stem: String,
}

impl FileKeys {
    fn new(file_path: &str) -> Self {
        let module_path = strip_source_extension(file_path);
        let (directory, stem) = module_path
            .rsplit_once('/')
            .map(|(directory, stem)| (directory.to_string(), stem.to_string()))
            .unwrap_or_else(|| (String::new(), module_path.clone()));

        Self {
            module_path,
            directory,
            stem,
        }
    }
}

/// Normalize a language-specific import path into a `/`-separated module path.
fn normalize_module_path(import_path: &str) -> String {
    let mut text = import_path
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '`'))
        .to_string();
    text = strip_source_extension(&text);

    loop {
        let stripped = text
            .strip_prefix("./")
            .or_else(|| text.strip_prefix("../"))
            .or_else(|| text.strip_prefix("crate::"))
            .or_else(|| text.strip_prefix("self::"))
            .or_else(|| text.strip_prefix("super::"));
        match stripped {
            Some(rest) => text = rest.to_string(),
            None => break,
        }
    }

    let text = text.trim_start_matches("::");
    let replaced = text.replace("::", "/").replace('.', "/");
    let mut parts = Vec::new();
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn strip_source_extension(path: &str) -> String {
    const EXTENSIONS: &[&str] = &[
        "rs", "py", "pyi", "ts", "tsx", "mts", "cts", "js", "jsx", "go",
    ];

    match path.rsplit_once('.') {
        Some((stem, extension)) if EXTENSIONS.contains(&extension) => stem.to_string(),
        _ => path.to_string(),
    }
}

fn ends_with_segment(path: &str, suffix: &str) -> bool {
    if path.is_empty() || suffix.is_empty() {
        return false;
    }
    path.ends_with(&format!("/{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, SymbolUri, Visibility};
    use crate::resolver::scope::ScopeChain;

    fn candidate(name: &str, file_path: &str, scope: Option<&str>) -> Candidate {
        Candidate {
            uri: SymbolUri::new("repo", file_path, scope, name, "fn").unwrap(),
            name: name.to_string(),
            file_path: file_path.to_string(),
            scope: ScopeChain::from_scope(scope),
            kind: NodeKind::Function,
            visibility: Visibility::Public,
            language: "rust".to_string(),
        }
    }

    #[test]
    fn test_proximity_orders_same_file_then_same_directory() {
        let same_file = proximity_score("src/a/main.rs", "src/a/main.rs");
        let same_directory = proximity_score("src/a/main.rs", "src/a/util.rs");
        let nearby = proximity_score("src/a/main.rs", "src/a/deep/util.rs");
        let distant = proximity_score("src/a/main.rs", "other/util.rs");

        assert!(same_file > same_directory);
        assert!(same_directory > nearby);
        assert!(nearby > distant);
    }

    #[test]
    fn test_module_matching_across_languages() {
        assert_eq!(module_match("crate::db", "src/db.rs"), ModuleMatch::Suffix);
        assert_eq!(module_match("pkg.db", "pkg/db.py"), ModuleMatch::Exact);
        assert_eq!(module_match("./util", "src/util.ts"), ModuleMatch::Suffix);
        assert_eq!(module_match("net/http", "net/http.go"), ModuleMatch::Exact);
        assert_eq!(module_match("server", "server/http.go"), ModuleMatch::Exact);
        assert_eq!(
            module_match("crate::db", "src/db/mod.rs"),
            ModuleMatch::Suffix
        );
        assert_eq!(
            module_match("completely/other", "src/db.rs"),
            ModuleMatch::None
        );
        assert_eq!(module_match("", "src/db.rs"), ModuleMatch::None);
    }

    #[test]
    fn test_ranking_is_deterministic_and_scope_aware() {
        let caller_scope = ScopeChain::from_scope(Some("Store::load"));
        let mut candidates = vec![
            candidate("save", "src/other.rs", Some("Other")),
            candidate("save", "src/store.rs", Some("Store")),
            candidate("save", "src/store.rs", Some("Store::load")),
        ];
        // Shuffle insertion order; ranking must not depend on it.
        candidates.reverse();

        let ranked = rank_candidates("src/store.rs", &caller_scope, &candidates);
        assert_eq!(ranked[0].candidate.scope.join(), "Store::load");
        assert_eq!(ranked[1].candidate.scope.join(), "Store");
        assert_eq!(ranked[2].candidate.file_path, "src/other.rs");
    }

    #[test]
    fn test_unique_best_rejects_ties() {
        let candidates = vec![
            candidate("save", "src/a.rs", None),
            candidate("save", "src/b.rs", None),
        ];
        let ranked = rank_candidates("root.rs", &ScopeChain::default(), &candidates);
        assert!(unique_best(&ranked).is_none());

        let candidates = vec![
            candidate("save", "src/a.rs", None),
            candidate("save", "src/sub/b.rs", None),
        ];
        let ranked = rank_candidates("src/a.rs", &ScopeChain::default(), &candidates);
        assert!(unique_best(&ranked).is_some());
    }
}
