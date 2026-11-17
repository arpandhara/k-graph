use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use crate::graph::MemoryGraph;
use crate::model::{CodeEdge, Confidence, EdgeKind, SourceSpan, SymbolUri};
use crate::parser::{ImportRecord, PendingCallSite, RawExtractedFile};

use super::error::ResolverError;
use super::heuristics::{self, ModuleMatch};
use super::scope::{CallSeparator, CallShape, Candidate, ScopeChain, ScopeIndex};

/// Confidence assigned to each resolution strategy.
///
/// The values encode *how* a call was linked, not how likely the callee is to
/// execute at runtime. A lexical definition is stronger evidence than a unique
/// global name; a proximity guess is deliberately low so consumers can filter
/// it without discarding the rest of the graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolutionConfig {
    /// Definition in the caller's file and an enclosing lexical scope.
    pub local_scope_confidence: Confidence,
    /// Only definition with this name in the caller's file.
    pub local_file_confidence: Confidence,
    /// Target reached through an explicit import binding.
    pub import_confidence: Confidence,
    /// Method on a resolvable receiver type.
    pub receiver_confidence: Confidence,
    /// Exactly one callable with this name in the repository.
    pub unique_global_confidence: Confidence,
    /// Several global candidates narrowed by path/scope proximity.
    pub proximity_confidence: Confidence,
    /// Receiver chain narrowed without a known receiver type.
    pub heuristic_confidence: Confidence,
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            local_scope_confidence: Confidence::new(0.99).expect("0.99 is a valid confidence"),
            local_file_confidence: Confidence::new(0.95).expect("0.95 is a valid confidence"),
            import_confidence: Confidence::new(0.95).expect("0.95 is a valid confidence"),
            receiver_confidence: Confidence::new(0.90).expect("0.90 is a valid confidence"),
            unique_global_confidence: Confidence::new(0.85).expect("0.85 is a valid confidence"),
            proximity_confidence: Confidence::new(0.65).expect("0.65 is a valid confidence"),
            heuristic_confidence: Confidence::new(0.55).expect("0.55 is a valid confidence"),
        }
    }
}

/// How a pending call was linked to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionStrategy {
    /// Definition in the caller's file and an enclosing lexical scope.
    LocalScope,
    /// Only definition with this name in the caller's file.
    LocalFile,
    /// Explicit import binding.
    Import,
    /// Method on a receiver type resolved from the call qualifier.
    ReceiverType,
    /// Exactly one callable with this name in the graph.
    UniqueGlobal,
    /// Several candidates narrowed by path/scope proximity.
    Proximity,
    /// Qualified call narrowed without knowing the receiver's type.
    Heuristic,
}

impl ResolutionStrategy {
    /// Stable string used by reports and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionStrategy::LocalScope => "local_scope",
            ResolutionStrategy::LocalFile => "local_file",
            ResolutionStrategy::Import => "import",
            ResolutionStrategy::ReceiverType => "receiver_type",
            ResolutionStrategy::UniqueGlobal => "unique_global",
            ResolutionStrategy::Proximity => "proximity",
            ResolutionStrategy::Heuristic => "heuristic",
        }
    }
}

/// A call site that was linked to a target.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCall {
    pub caller_uri: SymbolUri,
    pub target_uri: SymbolUri,
    pub call_expr: String,
    pub line: u32,
    pub col: u32,
    pub confidence: Confidence,
    pub strategy: ResolutionStrategy,
}

/// Why a call site could not be linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// The parser emitted a caller URI that is not valid.
    InvalidCallerUri(String),
    /// The caller URI is valid but its node is not in the graph.
    UnknownCaller,
    /// The callee expression has no usable identifier.
    EmptyCallExpression,
    /// No callable candidate matched the call.
    NoCandidate,
    /// More than one candidate had the same best score.
    Ambiguous { candidates: usize },
    /// `self`/`this` was used outside a resolvable type.
    NoEnclosingType,
}

impl UnresolvedReason {
    /// Stable string used by reports and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            UnresolvedReason::InvalidCallerUri(_) => "invalid_caller_uri",
            UnresolvedReason::UnknownCaller => "unknown_caller",
            UnresolvedReason::EmptyCallExpression => "empty_call_expression",
            UnresolvedReason::NoCandidate => "no_candidate",
            UnresolvedReason::Ambiguous { .. } => "ambiguous",
            UnresolvedReason::NoEnclosingType => "no_enclosing_type",
        }
    }
}

/// A call site that could not be linked.
///
/// Unresolved calls are expected for external APIs (`fmt.Println`, `std::fs`),
/// dynamically dispatched calls, and macro-generated code. They are reported
/// rather than turned into speculative edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCall {
    pub caller_uri: String,
    pub call_expr: String,
    pub line: u32,
    pub col: u32,
    pub reason: UnresolvedReason,
}

/// Result of one linking run.
///
/// Both vectors are sorted deterministically. `resolved` may contain several
/// call sites that collapse to the same graph edge; [`Self::edges_linked`]
/// reports the number of distinct `(caller, target)` `Calls` edges.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolutionReport {
    pub resolved: Vec<ResolvedCall>,
    pub unresolved: Vec<UnresolvedCall>,
}

impl ResolutionReport {
    /// Number of call sites that were linked.
    pub fn resolved_count(&self) -> usize {
        self.resolved.len()
    }

    /// Number of call sites that remain unresolved.
    pub fn unresolved_count(&self) -> usize {
        self.unresolved.len()
    }

    /// Number of distinct graph edges produced by the run.
    pub fn edges_linked(&self) -> usize {
        let mut edges = std::collections::BTreeSet::new();
        for call in &self.resolved {
            edges.insert((&call.caller_uri, &call.target_uri));
        }
        edges.len()
    }
}

/// Two-pass symbol resolution engine.
///
/// Pass 1 uses [`ScopeIndex`] to resolve calls against definitions in the
/// caller's own file and enclosing lexical scopes. Pass 2 falls back to import
/// bindings, receiver-type method lookup, then the global name index with
/// proximity ranking. The graph is only mutated after every call has been
/// resolved, so resolution never observes a partially updated edge set.
#[derive(Debug, Clone, Default)]
pub struct Linker {
    config: ResolutionConfig,
}

impl Linker {
    /// Build a linker with an explicit confidence policy.
    pub fn new(config: ResolutionConfig) -> Self {
        Self { config }
    }

    /// Access the active confidence policy.
    pub fn config(&self) -> &ResolutionConfig {
        &self.config
    }

    /// Resolve call sites and add `Calls` edges to the graph.
    ///
    /// `imports_by_file` is keyed by the repository-relative file path that
    /// appears in [`SymbolUri`] (for example `src/main.rs`). Calls without a
    /// target are returned in [`ResolutionReport::unresolved`] and do not
    /// mutate the graph.
    pub fn link(
        &self,
        graph: &mut MemoryGraph,
        pending: &[PendingCallSite],
        imports_by_file: &HashMap<String, Vec<ImportRecord>>,
    ) -> Result<ResolutionReport, ResolverError> {
        let scope_index = ScopeIndex::build(graph);

        let mut sites: Vec<&PendingCallSite> = pending.iter().collect();
        sites.sort_by(|left, right| {
            left.caller_uri
                .as_str()
                .cmp(right.caller_uri.as_str())
                .then(left.line.cmp(&right.line))
                .then(left.col.cmp(&right.col))
                .then(left.call_expr.as_str().cmp(right.call_expr.as_str()))
        });

        let mut resolved = Vec::new();
        let mut unresolved = Vec::new();

        for site in sites {
            match self.resolve_site(graph, &scope_index, site, imports_by_file) {
                SiteOutcome::Resolved(call) => resolved.push(call),
                SiteOutcome::Unresolved(call) => unresolved.push(call),
            }
        }

        resolved.sort_by(compare_resolved);
        resolved.dedup_by(|left, right| {
            left.caller_uri == right.caller_uri
                && left.target_uri == right.target_uri
                && left.call_expr == right.call_expr
                && left.line == right.line
                && left.col == right.col
        });
        unresolved.sort_by(|left, right| {
            left.caller_uri
                .cmp(&right.caller_uri)
                .then(left.line.cmp(&right.line))
                .then(left.col.cmp(&right.col))
                .then(left.call_expr.cmp(&right.call_expr))
        });

        self.insert_edges(graph, &resolved)?;

        Ok(ResolutionReport {
            resolved,
            unresolved,
        })
    }

    /// Convenience wrapper that collects calls and imports from parser output.
    ///
    /// Each tuple is the repository-relative file path followed by the
    /// extraction for that file. Callers are still responsible for adding the
    /// extraction's nodes and local edges to the graph before linking.
    pub fn link_extractions(
        &self,
        graph: &mut MemoryGraph,
        extractions: &[(String, RawExtractedFile)],
    ) -> Result<ResolutionReport, ResolverError> {
        let mut pending = Vec::new();
        let mut imports_by_file: HashMap<String, Vec<ImportRecord>> = HashMap::new();

        for (file_path, extraction) in extractions {
            pending.extend(extraction.pending_calls.iter().cloned());
            if !extraction.imports.is_empty() {
                imports_by_file.insert(file_path.clone(), extraction.imports.clone());
            }
        }

        self.link(graph, &pending, &imports_by_file)
    }

    fn insert_edges(
        &self,
        graph: &mut MemoryGraph,
        resolved: &[ResolvedCall],
    ) -> Result<(), ResolverError> {
        // One graph edge per (caller, target). Several call sites can collapse
        // to the same edge; keep the strongest, then the earliest, call site so
        // the stored span is deterministic.
        let mut preferred_by_edge: BTreeMap<(SymbolUri, SymbolUri), usize> = BTreeMap::new();
        for (index, call) in resolved.iter().enumerate() {
            let key = (call.caller_uri.clone(), call.target_uri.clone());
            match preferred_by_edge.get(&key) {
                Some(&existing) if !prefer_call(call, &resolved[existing]) => {}
                _ => {
                    preferred_by_edge.insert(key, index);
                }
            }
        }

        for index in preferred_by_edge.into_values() {
            let call = &resolved[index];
            let edge = CodeEdge::new(
                call.caller_uri.clone(),
                call.target_uri.clone(),
                EdgeKind::Calls,
            )
            .with_confidence(call.confidence.value())?
            .with_call_site(call_site_span(call)?);

            graph.add_edge(edge)?;
        }

        Ok(())
    }

    fn resolve_site(
        &self,
        graph: &MemoryGraph,
        scope_index: &ScopeIndex,
        site: &PendingCallSite,
        imports_by_file: &HashMap<String, Vec<ImportRecord>>,
    ) -> SiteOutcome {
        let caller = match SymbolUri::from_str(&site.caller_uri) {
            Ok(uri) => uri,
            Err(error) => {
                return SiteOutcome::Unresolved(UnresolvedCall {
                    caller_uri: site.caller_uri.clone(),
                    call_expr: site.call_expr.clone(),
                    line: site.line,
                    col: site.col,
                    reason: UnresolvedReason::InvalidCallerUri(error.to_string()),
                });
            }
        };

        let parsed = match caller.parse() {
            Ok(parsed) => parsed,
            Err(error) => {
                return SiteOutcome::Unresolved(UnresolvedCall {
                    caller_uri: site.caller_uri.clone(),
                    call_expr: site.call_expr.clone(),
                    line: site.line,
                    col: site.col,
                    reason: UnresolvedReason::InvalidCallerUri(error.to_string()),
                });
            }
        };

        if graph.get_node_index(&caller).is_none() {
            return SiteOutcome::Unresolved(UnresolvedCall {
                caller_uri: site.caller_uri.clone(),
                call_expr: site.call_expr.clone(),
                line: site.line,
                col: site.col,
                reason: UnresolvedReason::UnknownCaller,
            });
        }

        let shape = match CallShape::parse(&site.call_expr) {
            Some(shape) => shape,
            None => {
                return SiteOutcome::Unresolved(UnresolvedCall {
                    caller_uri: site.caller_uri.clone(),
                    call_expr: site.call_expr.clone(),
                    line: site.line,
                    col: site.col,
                    reason: UnresolvedReason::EmptyCallExpression,
                });
            }
        };

        let caller_scope = ScopeChain::from_scope(parsed.scope.as_deref());
        let caller_file = parsed.file_path;
        let imports = imports_by_file
            .get(&caller_file)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let lookup = match shape.qualifier() {
            None => self.resolve_bare(
                graph,
                scope_index,
                &caller_file,
                &caller_scope,
                shape.name(),
                imports,
            ),
            Some(qualifier) => self.resolve_qualified(
                graph,
                scope_index,
                &caller_file,
                &caller_scope,
                qualifier,
                shape.name(),
                shape.separator(),
                imports,
            ),
        };

        match lookup {
            Lookup::Found {
                target,
                confidence,
                strategy,
            } => SiteOutcome::Resolved(ResolvedCall {
                caller_uri: caller,
                target_uri: target,
                call_expr: site.call_expr.clone(),
                line: site.line,
                col: site.col,
                confidence,
                strategy,
            }),
            Lookup::Miss(reason) => SiteOutcome::Unresolved(UnresolvedCall {
                caller_uri: site.caller_uri.clone(),
                call_expr: site.call_expr.clone(),
                line: site.line,
                col: site.col,
                reason,
            }),
        }
    }

    fn resolve_bare(
        &self,
        graph: &MemoryGraph,
        scope_index: &ScopeIndex,
        caller_file: &str,
        caller_scope: &ScopeChain,
        name: &str,
        imports: &[ImportRecord],
    ) -> Lookup {
        // Pass 1: same file and enclosing lexical scopes. A same-file callable
        // that is not lexically visible shadows cross-file resolution: the name
        // exists locally, so importing it would require name-resolution rules
        // Phase 3 does not model. The call is reported rather than guessed.
        let local = scope_index.callables_in_file(caller_file, name);
        if !local.is_empty() {
            return self.choose_local(local, caller_scope);
        }

        // Pass 2a: explicit import binding.
        if let Some(lookup) =
            self.resolve_imported_symbol(graph, caller_file, caller_scope, name, imports)
        {
            return lookup;
        }

        // Pass 2b: unique global name, then proximity.
        self.resolve_global(graph, caller_file, caller_scope, name)
    }

    fn choose_local(&self, candidates: Vec<Candidate>, caller_scope: &ScopeChain) -> Lookup {
        let mut visible: Vec<Candidate> = candidates
            .into_iter()
            .filter(|candidate| candidate.scope.is_prefix_of(caller_scope))
            .collect();

        if visible.is_empty() {
            // A definition nested deeper than the caller is not normally
            // visible, so do not guess at a same-file match.
            return Lookup::Miss(UnresolvedReason::NoCandidate);
        }

        let deepest = visible
            .iter()
            .map(|candidate| candidate.scope.len())
            .max()
            .unwrap_or(0);
        visible.retain(|candidate| candidate.scope.len() == deepest);

        if visible.len() > 1 {
            return Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: visible.len(),
            });
        }

        let candidate = visible.pop().expect("one candidate");
        let (confidence, strategy) = if candidate.scope.is_empty() {
            (
                self.config.local_file_confidence,
                ResolutionStrategy::LocalFile,
            )
        } else {
            (
                self.config.local_scope_confidence,
                ResolutionStrategy::LocalScope,
            )
        };

        Lookup::found(candidate, confidence, strategy)
    }

    fn resolve_global(
        &self,
        graph: &MemoryGraph,
        caller_file: &str,
        caller_scope: &ScopeChain,
        name: &str,
    ) -> Lookup {
        let candidates = callable_candidates_named(graph, name);
        if candidates.is_empty() {
            return Lookup::Miss(UnresolvedReason::NoCandidate);
        }

        if candidates.len() == 1 {
            return Lookup::found(
                candidates.into_iter().next().expect("one candidate"),
                self.config.unique_global_confidence,
                ResolutionStrategy::UniqueGlobal,
            );
        }

        let ranked = heuristics::rank_candidates(caller_file, caller_scope, &candidates);
        match heuristics::unique_best(&ranked) {
            Some(best) => Lookup::found(
                best.candidate.clone(),
                self.config.proximity_confidence,
                ResolutionStrategy::Proximity,
            ),
            None => Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: candidates.len(),
            }),
        }
    }

    fn resolve_imported_symbol(
        &self,
        graph: &MemoryGraph,
        caller_file: &str,
        caller_scope: &ScopeChain,
        local_name: &str,
        imports: &[ImportRecord],
    ) -> Option<Lookup> {
        let mut best: Vec<(i32, Candidate)> = Vec::new();
        let mut ambiguous = false;

        for import in imports {
            let Some(target_name) = bare_import_target(import, local_name) else {
                continue;
            };

            let module_paths = import_module_paths(import);
            let mut candidates = callable_candidates_named(graph, &target_name);
            if candidates.is_empty() && target_name != local_name {
                candidates = callable_candidates_named(graph, local_name);
            }
            if candidates.is_empty() {
                continue;
            }

            if let Some(filtered) = module_filtered(&candidates, &module_paths) {
                candidates = filtered;
            } else {
                continue;
            }

            let ranked = heuristics::rank_candidates_with_modules(
                caller_file,
                caller_scope,
                &candidates,
                &module_paths,
            );

            match heuristics::unique_best(&ranked) {
                Some(ranked) => best.push((ranked.score, ranked.candidate.clone())),
                None => ambiguous = true,
            }
        }

        if ambiguous {
            return Some(Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: best.len().max(1),
            }));
        }

        if best.is_empty() {
            return None;
        }

        best.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.uri.cmp(&right.1.uri))
        });
        if best.len() > 1 && best[0].0 == best[1].0 && best[0].1.uri != best[1].1.uri {
            return Some(Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: best.len(),
            }));
        }

        let (_, candidate) = best.remove(0);
        Some(Lookup::found(
            candidate,
            self.config.import_confidence,
            ResolutionStrategy::Import,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_qualified(
        &self,
        graph: &MemoryGraph,
        scope_index: &ScopeIndex,
        caller_file: &str,
        caller_scope: &ScopeChain,
        qualifier: &str,
        name: &str,
        separator: CallSeparator,
        imports: &[ImportRecord],
    ) -> Lookup {
        if matches!(qualifier, "self" | "this" | "cls") {
            return self.resolve_enclosing(graph, scope_index, caller_file, caller_scope, name);
        }

        // A qualified receiver that names a type is the strongest signal for a
        // method call (`Index::load`, `Server.Start`, `Foo.bar`).
        if let Some(lookup) = self.resolve_typed_receiver(
            graph,
            caller_file,
            caller_scope,
            qualifier,
            name,
            separator,
            imports,
        ) {
            return lookup;
        }

        // A qualifier can also be a module namespace imported under an alias
        // (`import * as ns`, Go package imports, Python module aliases).
        if let Some(lookup) = self.resolve_import_module_member(
            graph,
            caller_file,
            caller_scope,
            qualifier,
            name,
            imports,
        ) {
            return lookup;
        }

        let import_bound = imports
            .iter()
            .any(|import| import_binds_qualifier(import, qualifier));
        if import_bound {
            return Lookup::Miss(UnresolvedReason::NoCandidate);
        }

        // Last resort for dynamic receivers (`agent.execute`): narrow by the
        // method name, then by file/scope proximity. Confidence is deliberately
        // low because the receiver's type is unknown.
        let candidates = callable_candidates_named(graph, name);
        if candidates.is_empty() {
            return Lookup::Miss(UnresolvedReason::NoCandidate);
        }

        let methods: Vec<Candidate> = candidates
            .iter()
            .filter(|candidate| candidate.receiver_type().is_some())
            .cloned()
            .collect();
        let pool = if methods.is_empty() {
            candidates
        } else {
            methods
        };

        let ranked = heuristics::rank_candidates(caller_file, caller_scope, &pool);
        match heuristics::unique_best(&ranked) {
            Some(best) => Lookup::found(
                best.candidate.clone(),
                self.config.heuristic_confidence,
                ResolutionStrategy::Heuristic,
            ),
            None => Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: pool.len(),
            }),
        }
    }

    fn resolve_enclosing(
        &self,
        graph: &MemoryGraph,
        scope_index: &ScopeIndex,
        caller_file: &str,
        caller_scope: &ScopeChain,
        name: &str,
    ) -> Lookup {
        let enclosing_types = scope_index.enclosing_types(caller_file, caller_scope);
        if enclosing_types.is_empty() {
            return Lookup::Miss(UnresolvedReason::NoEnclosingType);
        }

        let mut candidates = Vec::new();
        for type_candidate in &enclosing_types {
            let methods = methods_for_type(graph, &type_candidate.name, name);
            if !methods.is_empty() {
                candidates = methods;
                break;
            }
        }

        if candidates.is_empty() {
            return Lookup::Miss(UnresolvedReason::NoCandidate);
        }

        let same_file: Vec<Candidate> = candidates
            .iter()
            .filter(|candidate| candidate.file_path == caller_file)
            .cloned()
            .collect();
        let pool = if same_file.is_empty() {
            candidates
        } else {
            same_file
        };

        let ranked = heuristics::rank_candidates(caller_file, caller_scope, &pool);
        match heuristics::unique_best(&ranked) {
            Some(best) => Lookup::found(
                best.candidate.clone(),
                self.config.receiver_confidence,
                ResolutionStrategy::ReceiverType,
            ),
            None => Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: pool.len(),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_typed_receiver(
        &self,
        graph: &MemoryGraph,
        caller_file: &str,
        caller_scope: &ScopeChain,
        type_name: &str,
        method_name: &str,
        separator: CallSeparator,
        imports: &[ImportRecord],
    ) -> Option<Lookup> {
        let types = type_candidates_named(graph, type_name);
        if types.is_empty() {
            return None;
        }

        let module_paths = dedup_paths(
            imports
                .iter()
                .filter(|import| import_binds_qualifier(import, type_name))
                .flat_map(import_module_paths)
                .collect(),
        );

        let mut types = types;
        if !module_paths.is_empty() {
            let filtered: Vec<Candidate> = types
                .iter()
                .filter(|candidate| matches_modules(&candidate.file_path, &module_paths))
                .cloned()
                .collect();
            if !filtered.is_empty() {
                types = filtered;
            }
        }

        let ranked_types = heuristics::rank_candidates_with_modules(
            caller_file,
            caller_scope,
            &types,
            &module_paths,
        );
        let top_score = ranked_types.first()?.score;
        let top_types: Vec<&Candidate> = ranked_types
            .iter()
            .filter(|ranked| ranked.score == top_score)
            .map(|ranked| &ranked.candidate)
            .collect();

        let mut methods = Vec::new();
        for type_candidate in top_types {
            methods.extend(methods_for_type(graph, &type_candidate.name, method_name));
        }
        let methods = sorted_unique(methods);

        if methods.is_empty() {
            // A capitalized/path-qualified name that matched a type but not one
            // of its methods should not fall through to unrelated functions.
            return if matches!(separator, CallSeparator::Path) || is_type_like(type_name) {
                Some(Lookup::Miss(UnresolvedReason::NoCandidate))
            } else {
                None
            };
        }

        let ranked = heuristics::rank_candidates_with_modules(
            caller_file,
            caller_scope,
            &methods,
            &module_paths,
        );
        match heuristics::unique_best(&ranked) {
            Some(best) => Some(Lookup::found(
                best.candidate.clone(),
                self.config.receiver_confidence,
                ResolutionStrategy::ReceiverType,
            )),
            None => Some(Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: methods.len(),
            })),
        }
    }

    fn resolve_import_module_member(
        &self,
        graph: &MemoryGraph,
        caller_file: &str,
        caller_scope: &ScopeChain,
        qualifier: &str,
        name: &str,
        imports: &[ImportRecord],
    ) -> Option<Lookup> {
        let mut matches: Vec<(i32, Candidate)> = Vec::new();
        let mut ambiguous = false;

        for import in imports
            .iter()
            .filter(|import| import_binds_qualifier(import, qualifier))
        {
            let module_paths = import_module_paths(import);
            let candidates = callable_candidates_named(graph, name);
            if candidates.is_empty() {
                continue;
            }

            let Some(candidates) = module_filtered(&candidates, &module_paths) else {
                continue;
            };

            let ranked = heuristics::rank_candidates_with_modules(
                caller_file,
                caller_scope,
                &candidates,
                &module_paths,
            );
            match heuristics::unique_best(&ranked) {
                Some(ranked) => matches.push((ranked.score, ranked.candidate.clone())),
                None => ambiguous = true,
            }
        }

        if ambiguous {
            return Some(Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: matches.len().max(1),
            }));
        }

        if matches.is_empty() {
            return None;
        }

        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.uri.cmp(&right.1.uri))
        });
        if matches.len() > 1 && matches[0].0 == matches[1].0 && matches[0].1.uri != matches[1].1.uri
        {
            return Some(Lookup::Miss(UnresolvedReason::Ambiguous {
                candidates: matches.len(),
            }));
        }

        let (_, candidate) = matches.remove(0);
        Some(Lookup::found(
            candidate,
            self.config.import_confidence,
            ResolutionStrategy::Import,
        ))
    }
}

#[derive(Debug)]
enum SiteOutcome {
    Resolved(ResolvedCall),
    Unresolved(UnresolvedCall),
}

#[derive(Debug)]
enum Lookup {
    Found {
        target: SymbolUri,
        confidence: Confidence,
        strategy: ResolutionStrategy,
    },
    Miss(UnresolvedReason),
}

impl Lookup {
    fn found(candidate: Candidate, confidence: Confidence, strategy: ResolutionStrategy) -> Self {
        Self::Found {
            target: candidate.uri,
            confidence,
            strategy,
        }
    }
}

fn callable_candidates_named(graph: &MemoryGraph, name: &str) -> Vec<Candidate> {
    let indices = graph.find_nodes_by_name(name);
    let mut candidates = Vec::new();
    for index in indices.iter() {
        if let Some(node) = graph.get_node(*index)
            && node.kind().is_callable()
            && let Ok(candidate) = Candidate::from_node(node)
        {
            candidates.push(candidate);
        }
    }
    sorted_unique(candidates)
}

fn type_candidates_named(graph: &MemoryGraph, name: &str) -> Vec<Candidate> {
    let indices = graph.find_nodes_by_name(name);
    let mut candidates = Vec::new();
    for index in indices.iter() {
        if let Some(node) = graph.get_node(*index)
            && node.kind().is_type_definition()
            && let Ok(candidate) = Candidate::from_node(node)
        {
            candidates.push(candidate);
        }
    }
    sorted_unique(candidates)
}

fn methods_for_type(graph: &MemoryGraph, type_name: &str, method_name: &str) -> Vec<Candidate> {
    callable_candidates_named(graph, method_name)
        .into_iter()
        .filter(|candidate| candidate.receiver_type() == Some(type_name))
        .collect()
}

fn sorted_unique(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|left, right| left.uri.cmp(&right.uri));
    candidates.dedup_by(|left, right| left.uri == right.uri);
    candidates
}

/// Resolve a bare call through an import binding.
fn bare_import_target(import: &ImportRecord, local_name: &str) -> Option<String> {
    let binding = import
        .alias
        .as_deref()
        .unwrap_or(import.imported_symbol.as_str());
    if binding != local_name {
        return None;
    }

    match import.imported_symbol.as_str() {
        "*" | "_" => None,
        "default" => Some(local_name.to_string()),
        symbol => Some(symbol.to_string()),
    }
}

/// Module paths that an import declaration can refer to.
///
/// The same record shape covers `use crate::db::save`, Python
/// `import os.path as p`, TypeScript `import * as ns from "./util"`, and Go
/// `import h "net/http"`. Trying both the declared source and the
/// source-plus-symbol form keeps module matching purely textual.
fn import_module_paths(import: &ImportRecord) -> Vec<String> {
    let source = import.source_path.trim();
    if source.is_empty() {
        return Vec::new();
    }

    let mut paths = vec![source.to_string()];
    let symbol = import.imported_symbol.trim();
    if !matches!(symbol, "" | "*" | "_" | "default") {
        paths.push(format!("{source}::{symbol}"));
    }
    dedup_paths(paths)
}

fn dedup_paths(paths: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for path in paths {
        if !path.is_empty() && !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

/// True when `qualifier` is a local binding introduced by an import.
fn import_binds_qualifier(import: &ImportRecord, qualifier: &str) -> bool {
    if let Some(alias) = import.alias.as_deref()
        && alias == qualifier
    {
        return true;
    }

    if import.alias.is_none()
        && import.imported_symbol != "*"
        && import.imported_symbol == qualifier
    {
        return true;
    }

    // Python's `import os.path` binds `os` even though the parser records
    // source `os` and symbol `path`. Only apply this to bare dotted module
    // paths; Rust/TS module paths are explicitly qualified or relative.
    let source = import.source_path.trim();
    let python_like = !source.is_empty()
        && !source.contains("::")
        && !source.contains('/')
        && !source.starts_with('.');
    if python_like {
        let first = source.split('.').next().unwrap_or(source);
        if first == qualifier {
            return true;
        }
    }

    false
}

fn matches_modules(file_path: &str, module_paths: &[String]) -> bool {
    module_paths
        .iter()
        .any(|module| heuristics::module_match(module, file_path) != ModuleMatch::None)
}

fn module_filtered(candidates: &[Candidate], module_paths: &[String]) -> Option<Vec<Candidate>> {
    if module_paths.is_empty() {
        return Some(candidates.to_vec());
    }

    let filtered: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| matches_modules(&candidate.file_path, module_paths))
        .cloned()
        .collect();

    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

fn is_type_like(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn compare_resolved(left: &ResolvedCall, right: &ResolvedCall) -> std::cmp::Ordering {
    left.caller_uri
        .cmp(&right.caller_uri)
        .then(left.line.cmp(&right.line))
        .then(left.col.cmp(&right.col))
        .then(left.call_expr.cmp(&right.call_expr))
        .then(left.target_uri.cmp(&right.target_uri))
}

fn prefer_call(candidate: &ResolvedCall, existing: &ResolvedCall) -> bool {
    if candidate.confidence.value() != existing.confidence.value() {
        return candidate.confidence.value() > existing.confidence.value();
    }
    (candidate.line, candidate.col, &candidate.call_expr)
        < (existing.line, existing.col, &existing.call_expr)
}

fn call_site_span(call: &ResolvedCall) -> Result<SourceSpan, crate::model::SpanError> {
    let length = u32::try_from(call.call_expr.chars().count()).unwrap_or(u32::MAX);
    SourceSpan::new(
        call.line,
        call.col,
        call.line,
        call.col.saturating_add(length),
        0,
        0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CodeEntityNode, NodeKind, SourceSpan};
    use crate::parser::LanguageAdapter;
    use crate::resolver::scope::CallShape;

    fn add_extraction(graph: &mut MemoryGraph, extraction: &RawExtractedFile) {
        for node in &extraction.nodes {
            graph.add_node(node.clone()).expect("node added");
        }
        for edge in &extraction.local_edges {
            graph.add_edge(edge.clone()).expect("local edge added");
        }
    }

    fn imports(entries: &[(&str, RawExtractedFile)]) -> HashMap<String, Vec<ImportRecord>> {
        let mut by_file = HashMap::new();
        for (file, extraction) in entries {
            if !extraction.imports.is_empty() {
                by_file.insert(file.to_string(), extraction.imports.clone());
            }
        }
        by_file
    }

    fn link(graph: &mut MemoryGraph, extractions: &[(&str, RawExtractedFile)]) -> ResolutionReport {
        let owned: Vec<(String, RawExtractedFile)> = extractions
            .iter()
            .map(|(file, extraction)| (file.to_string(), extraction.clone()))
            .collect();
        Linker::default()
            .link_extractions(graph, &owned)
            .expect("link succeeds")
    }

    fn calls_edge_count(graph: &MemoryGraph) -> usize {
        graph
            .graph()
            .edge_weights()
            .filter(|edge| edge.kind() == EdgeKind::Calls)
            .count()
    }

    #[test]
    fn test_call_shape_is_reused_by_the_linker() {
        let shape = CallShape::parse("fmt.Println").unwrap();
        assert_eq!(shape.qualifier(), Some("fmt"));
        assert_eq!(shape.name(), "Println");
    }

    #[test]
    fn test_rust_local_scope_prefers_nested_definition() {
        let source = r#"
fn helper() {}
fn outer() {
    fn helper() {}
    fn inner() {
        helper();
    }
}
"#;
        let adapter = crate::parser::RustAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/nested.rs", source).unwrap();
        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &extraction);

        let report = link(&mut graph, &[("src/nested.rs", extraction)]);
        assert_eq!(report.resolved_count(), 1);
        let call = &report.resolved[0];
        assert!(call.caller_uri.as_str().contains("#outer::inner:fn"));
        assert!(call.target_uri.as_str().contains("#outer::helper:fn"));
        assert_eq!(call.strategy, ResolutionStrategy::LocalScope);
        assert_eq!(call.confidence.value(), 0.99);
    }

    #[test]
    fn test_rust_import_alias_resolves_across_files() {
        let db = r#"
pub fn save() {}
pub fn load() {}
"#;
        let main = r#"
use crate::db::{save as persist, load};

fn helper() {}
fn run() {
    persist();
    load();
    helper();
}
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let db_extraction = rust.parse_file("repo", "src/db.rs", db).unwrap();
        let main_extraction = rust.parse_file("repo", "src/main.rs", main).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &db_extraction);
        add_extraction(&mut graph, &main_extraction);

        let report = link(
            &mut graph,
            &[
                ("src/db.rs", db_extraction),
                ("src/main.rs", main_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 3);
        assert_eq!(report.unresolved_count(), 0);

        let persist = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "persist")
            .unwrap();
        assert!(persist.target_uri.as_str().ends_with("#save:fn"));
        assert_eq!(persist.strategy, ResolutionStrategy::Import);

        let load = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "load")
            .unwrap();
        assert!(load.target_uri.as_str().ends_with("#load:fn"));

        let helper = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "helper")
            .unwrap();
        assert_eq!(helper.strategy, ResolutionStrategy::LocalFile);
        assert_eq!(calls_edge_count(&graph), 3);
    }

    #[test]
    fn test_rust_receiver_and_self_calls_resolve_methods() {
        let index = r#"
struct Index;

impl Index {
    fn load(&self) {}
    fn refresh(&self) {
        self.load();
    }
}
"#;
        let app = r#"
fn main() {
    Index::load();
    let index = Index;
    index.load();
}
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let index_extraction = rust.parse_file("repo", "src/index.rs", index).unwrap();
        let app_extraction = rust.parse_file("repo", "src/app.rs", app).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &index_extraction);
        add_extraction(&mut graph, &app_extraction);

        let report = link(
            &mut graph,
            &[
                ("src/index.rs", index_extraction),
                ("src/app.rs", app_extraction),
            ],
        );

        let self_call = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "self.load")
            .unwrap();
        assert_eq!(self_call.strategy, ResolutionStrategy::ReceiverType);
        assert!(self_call.target_uri.as_str().ends_with("#Index::load:fn"));

        let path_call = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "Index::load")
            .unwrap();
        assert_eq!(path_call.strategy, ResolutionStrategy::ReceiverType);

        let dynamic = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "index.load")
            .unwrap();
        assert_eq!(dynamic.strategy, ResolutionStrategy::Heuristic);
        assert_eq!(dynamic.confidence.value(), 0.55);
    }

    #[test]
    fn test_proximity_breaks_global_ambiguity() {
        let caller = r#"
fn run() {
    helper();
}
"#;
        let near = "pub fn helper() {}";
        let far = "pub fn helper() {}";

        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let caller_extraction = rust.parse_file("repo", "src/a/main.rs", caller).unwrap();
        let near_extraction = rust.parse_file("repo", "src/a/util.rs", near).unwrap();
        let far_extraction = rust.parse_file("repo", "src/b/util.rs", far).unwrap();

        let mut graph = MemoryGraph::new();
        for extraction in [&caller_extraction, &near_extraction, &far_extraction] {
            add_extraction(&mut graph, extraction);
        }

        let report = link(
            &mut graph,
            &[
                ("src/a/main.rs", caller_extraction),
                ("src/a/util.rs", near_extraction),
                ("src/b/util.rs", far_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 1);
        let call = &report.resolved[0];
        assert_eq!(call.strategy, ResolutionStrategy::Proximity);
        assert!(call.target_uri.as_str().contains("src/a/util.rs"));
        assert_eq!(call.confidence.value(), 0.65);
    }

    #[test]
    fn test_python_import_alias_resolves_across_files() {
        let db = r#"
def save():
    pass

def load():
    pass
"#;
        let main = r#"
from pkg.db import save as persist

def run():
    persist()
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let python = registry.adapter_for_extension("py").unwrap();
        let db_extraction = python.parse_file("repo", "pkg/db.py", db).unwrap();
        let main_extraction = python.parse_file("repo", "pkg/main.py", main).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &db_extraction);
        add_extraction(&mut graph, &main_extraction);

        let report = link(
            &mut graph,
            &[
                ("pkg/db.py", db_extraction),
                ("pkg/main.py", main_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 1);
        assert_eq!(report.resolved[0].strategy, ResolutionStrategy::Import);
        assert!(report.resolved[0].target_uri.as_str().ends_with("#save:fn"));
    }

    #[test]
    fn test_typescript_import_alias_resolves_across_files() {
        let util = r#"
export function save() {}
export function load() {}
"#;
        let app = r#"
import { save as persist } from "./util";

export function run() {
  persist();
}
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let typescript = registry.adapter_for_extension("ts").unwrap();
        let util_extraction = typescript.parse_file("repo", "src/util.ts", util).unwrap();
        let app_extraction = typescript.parse_file("repo", "src/app.ts", app).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &util_extraction);
        add_extraction(&mut graph, &app_extraction);

        let report = link(
            &mut graph,
            &[
                ("src/util.ts", util_extraction),
                ("src/app.ts", app_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 1);
        assert_eq!(report.resolved[0].strategy, ResolutionStrategy::Import);
        assert!(report.resolved[0].target_uri.as_str().ends_with("#save:fn"));
    }

    #[test]
    fn test_go_package_import_and_receiver_call() {
        let http = r#"
package server

func StartServer() {}

type Server struct{}

func (s *Server) Start() {}
"#;
        let main = r#"
package main

import "server"

func main() {
    server.StartServer()
    var s Server
    s.Start()
}
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let go = registry.adapter_for_extension("go").unwrap();
        let http_extraction = go.parse_file("repo", "server/http.go", http).unwrap();
        let main_extraction = go.parse_file("repo", "cmd/main.go", main).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &http_extraction);
        add_extraction(&mut graph, &main_extraction);

        let report = link(
            &mut graph,
            &[
                ("server/http.go", http_extraction),
                ("cmd/main.go", main_extraction),
            ],
        );

        let start_server = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "server.StartServer")
            .unwrap();
        assert_eq!(start_server.strategy, ResolutionStrategy::Import);

        let receiver = report
            .resolved
            .iter()
            .find(|call| call.call_expr == "s.Start")
            .unwrap();
        assert_eq!(receiver.strategy, ResolutionStrategy::Heuristic);
    }

    #[test]
    fn test_unresolved_external_and_invalid_calls_are_reported() {
        let source = r#"
fn run() {
    missing();
}
"#;
        let adapter = crate::parser::RustAdapter::new().unwrap();
        let extraction = adapter.parse_file("repo", "src/main.rs", source).unwrap();
        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &extraction);

        let pending = vec![
            PendingCallSite {
                caller_uri: "not-a-uri".to_string(),
                call_expr: "save".to_string(),
                line: 1,
                col: 0,
            },
            PendingCallSite {
                caller_uri: "sym://repo/src/missing.rs#run:fn".to_string(),
                call_expr: "save".to_string(),
                line: 2,
                col: 0,
            },
            PendingCallSite {
                caller_uri: "sym://repo/src/main.rs#run:fn".to_string(),
                call_expr: "   ".to_string(),
                line: 3,
                col: 0,
            },
        ];

        let report = Linker::default()
            .link(&mut graph, &pending, &HashMap::new())
            .unwrap();

        assert_eq!(report.resolved_count(), 0);
        assert_eq!(report.unresolved_count(), 3);
        assert_eq!(
            report
                .unresolved
                .iter()
                .find(|call| call.caller_uri == "not-a-uri")
                .unwrap()
                .reason
                .as_str(),
            "invalid_caller_uri"
        );
        assert_eq!(
            report
                .unresolved
                .iter()
                .find(|call| call.caller_uri.contains("src/missing.rs"))
                .unwrap()
                .reason
                .as_str(),
            "unknown_caller"
        );
        assert_eq!(
            report
                .unresolved
                .iter()
                .find(|call| call.call_expr.trim().is_empty())
                .unwrap()
                .reason
                .as_str(),
            "empty_call_expression"
        );
    }

    #[test]
    fn test_linking_is_idempotent_and_deterministic() {
        let db = "pub fn save() {}";
        let main = r#"
use crate::db::save;
fn run() {
    save();
}
"#;
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let db_extraction = rust.parse_file("repo", "src/db.rs", db).unwrap();
        let main_extraction = rust.parse_file("repo", "src/main.rs", main).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &db_extraction);
        add_extraction(&mut graph, &main_extraction);

        let mut pending = main_extraction.pending_calls.clone();
        pending.reverse();
        let imports = imports(&[
            ("src/db.rs", db_extraction.clone()),
            ("src/main.rs", main_extraction.clone()),
        ]);

        let linker = Linker::default();
        let first = linker.link(&mut graph, &pending, &imports).unwrap();
        let edges_after_first = graph.edge_count();
        let second = linker.link(&mut graph, &pending, &imports).unwrap();

        assert_eq!(first, second);
        assert_eq!(graph.edge_count(), edges_after_first);
        assert_eq!(first.edges_linked(), 1);
    }

    #[test]
    fn test_ambiguous_global_call_stays_unresolved() {
        let caller = "fn run() { helper(); }";
        let left = "pub fn helper() {}";
        let right = "pub fn helper() {}";
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let caller_extraction = rust.parse_file("repo", "left/main.rs", caller).unwrap();
        let left_extraction = rust.parse_file("repo", "left/a.rs", left).unwrap();
        let right_extraction = rust.parse_file("repo", "left/b.rs", right).unwrap();

        let mut graph = MemoryGraph::new();
        for extraction in [&caller_extraction, &left_extraction, &right_extraction] {
            add_extraction(&mut graph, extraction);
        }

        // Same directory, same scope, and the same visibility: both candidates
        // score identically, so no edge is invented.
        let report = link(
            &mut graph,
            &[
                ("left/main.rs", caller_extraction),
                ("left/a.rs", left_extraction),
                ("left/b.rs", right_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 0);
        assert_eq!(report.unresolved[0].reason.as_str(), "ambiguous");
        assert_eq!(calls_edge_count(&graph), 0);
    }

    #[test]
    fn test_confidence_configuration_is_honoured() {
        let source = "fn run() { helper(); }";
        let helper = "pub fn helper() {}";
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let source_extraction = rust.parse_file("repo", "src/a.rs", source).unwrap();
        let helper_extraction = rust.parse_file("repo", "src/b.rs", helper).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &source_extraction);
        add_extraction(&mut graph, &helper_extraction);

        let config = ResolutionConfig {
            unique_global_confidence: Confidence::new(0.42).unwrap(),
            ..ResolutionConfig::default()
        };
        let report = Linker::new(config)
            .link(
                &mut graph,
                &source_extraction.pending_calls,
                &HashMap::new(),
            )
            .unwrap();

        assert_eq!(report.resolved_count(), 1);
        assert_eq!(report.resolved[0].confidence.value(), 0.42);
    }

    #[test]
    fn test_duplicate_call_sites_upsert_one_edge_with_earliest_span() {
        let source = "fn run() { helper(); helper(); }";
        let helper = "pub fn helper() {}";
        let registry = crate::parser::LanguageRegistry::new().unwrap();
        let rust = registry.adapter_for_extension("rs").unwrap();
        let source_extraction = rust.parse_file("repo", "src/a.rs", source).unwrap();
        let helper_extraction = rust.parse_file("repo", "src/b.rs", helper).unwrap();

        let mut graph = MemoryGraph::new();
        add_extraction(&mut graph, &source_extraction);
        add_extraction(&mut graph, &helper_extraction);

        let report = link(
            &mut graph,
            &[
                ("src/a.rs", source_extraction),
                ("src/b.rs", helper_extraction),
            ],
        );

        assert_eq!(report.resolved_count(), 2);
        assert_eq!(report.edges_linked(), 1);
        assert_eq!(calls_edge_count(&graph), 1);

        let edge_index = graph
            .find_nodes_by_name("run")
            .iter()
            .next()
            .copied()
            .unwrap();
        let edge = graph
            .outgoing_edges(edge_index)
            .find(|(_, edge)| edge.kind() == EdgeKind::Calls)
            .map(|(_, edge)| edge)
            .unwrap();
        let span = edge.call_site().unwrap();
        assert_eq!(span.start_line(), 1);
        assert_eq!(span.start_col(), 11);
    }

    #[test]
    fn test_file_less_caller_uri_is_not_silently_mutated() {
        // Guard the boundary: resolver never fabricates a URI from invalid
        // input. This mirrors the parser's whole-file failure policy.
        let mut graph = MemoryGraph::new();
        let pending = vec![PendingCallSite {
            caller_uri: "sym://repo/src/a.rs#file:file".to_string(),
            call_expr: "save".to_string(),
            line: 1,
            col: 0,
        }];
        let report = Linker::default()
            .link(&mut graph, &pending, &HashMap::new())
            .unwrap();
        assert_eq!(report.unresolved_count(), 1);
        assert_eq!(report.unresolved[0].reason.as_str(), "unknown_caller");
    }

    #[test]
    fn test_scope_index_snapshot_ignores_container_nodes() {
        let mut graph = MemoryGraph::new();
        let uri = SymbolUri::for_file("repo", "src/a.rs").unwrap();
        graph
            .add_node(
                CodeEntityNode::try_new(
                    uri,
                    "file",
                    NodeKind::File,
                    "src/a.rs",
                    "rust",
                    SourceSpan::zero(),
                )
                .unwrap(),
            )
            .unwrap();

        let index = ScopeIndex::build(&graph);
        assert_eq!(index.file_count(), 0);
        assert_eq!(index.candidate_count(), 0);
    }
}
