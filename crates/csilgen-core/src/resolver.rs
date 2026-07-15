//! Import resolution system for CSIL files

use crate::ast::*;
use crate::parser::parse_csil_file;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A rule's true source: the canonical path of the file that originally *defined* it
/// (via `=`/`/=`, not via an `include`/`from` that merely re-exported it), plus its
/// name in that file before any alias prefix was applied. Two rules merged into the
/// same spec under the same final name are the same logical rule — and therefore a
/// diamond, not a collision — iff their `Origin`s are equal. Tracking the *true*
/// origin (rather than the immediate file an `include`/`from` statement named) is
/// what lets diamond detection see through an arbitrary chain of re-exports: e.g.
/// `entry` includes `mid` (which includes `common`) and also includes `common`
/// directly — `common`'s rules reach `entry` labeled with `common`'s path both times,
/// even though the first time they arrived bundled inside `mid`'s resolved spec.
type Origin = (PathBuf, String);

/// Handles resolution of import statements in CSIL files
pub struct ImportResolver {
    /// Directories to search for imported files
    search_paths: Vec<PathBuf>,
    /// Cache of resolved specifications to avoid re-parsing, paired with the `Origin`
    /// of every rule in that spec (see `Origin`) so a spec pulled from cache can still
    /// participate correctly in a consuming spec's own diamond-dedup guard.
    resolved_cache: HashMap<PathBuf, (CsilSpec, HashMap<String, Origin>)>,
    /// Set of files currently being resolved (for circular dependency detection)
    resolving: HashSet<PathBuf>,
}

impl ImportResolver {
    /// Create a new import resolver with current directory as default search path
    pub fn new() -> Self {
        Self {
            search_paths: vec![PathBuf::from(".")],
            resolved_cache: HashMap::new(),
            resolving: HashSet::new(),
        }
    }

    /// Add a directory to the search paths for import resolution
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.search_paths.push(path);
    }

    /// Resolve all imports in a specification recursively. This is the outward-facing
    /// entry point: after assembling the full include graph it also finalizes any
    /// `/=` rule that never found a same-name `=` base into its own `TypeDef` (see
    /// `merge_type_choice_extensions`'s `collapse_orphans` doc). Internal recursion for
    /// each included file goes through `resolve_imports_uncollapsed` instead, which
    /// skips that finalization — a leaf file resolved on its own can't yet tell "no
    /// base anywhere" from "the base is in whichever file includes me".
    pub fn resolve_imports(&mut self, spec: &mut CsilSpec, base_path: &Path) -> Result<()> {
        self.resolve_imports_uncollapsed(spec, base_path)?;
        crate::ast::merge_type_choice_extensions(&mut spec.rules, true);
        Ok(())
    }

    /// Shared implementation behind `resolve_imports`: walks and merges in this file's
    /// imports, without finalizing orphaned `/=` rules (see `resolve_imports`'s doc).
    ///
    /// Returns this spec's own `Origin` map (final rule name -> true defining file),
    /// so a caller assembling a *different* spec that merges this one in (e.g. this
    /// file was reached via `include`/`from`) can attribute the right true origin to
    /// each rule it re-merges, rather than crediting the immediate file it came from.
    fn resolve_imports_uncollapsed(
        &mut self,
        spec: &mut CsilSpec,
        base_path: &Path,
    ) -> Result<HashMap<String, Origin>> {
        let base_dir = base_path.parent().unwrap_or(Path::new("."));

        // Register this file as "currently resolving" so a cycle that loops back to it
        // (directly, or transitively through an included file) is caught by the same guard
        // `load_and_resolve_file` applies to files reached via an include statement. Without
        // this the top-level entry file was never marked, so a cycle back to *it* specifically
        // went undetected.
        let canonical_base = base_path
            .canonicalize()
            .unwrap_or_else(|_| base_path.to_path_buf());
        let newly_resolving = self.resolving.insert(canonical_base.clone());

        // Seed the origin map with this file's own natively-defined rules (present in
        // `spec.rules` before any merge below) — each is its own true origin.
        let mut origins: HashMap<String, Origin> = spec
            .rules
            .iter()
            .map(|r| (r.name.clone(), (canonical_base.clone(), r.name.clone())))
            .collect();

        // Per-spec diamond guard: (final rule name, true origin) already merged into
        // `spec` during *this* call. Deliberately a local, not a struct field —
        // resolving a sibling/nested spec (e.g. a `from`-imported file's own includes)
        // must start from a clean guard, since that's a wholly different target spec
        // being assembled. A resolver-global guard here was Finding 1: it made a file
        // merged into one spec silently skip re-merging into an unrelated spec that
        // needed its own independent copy of the same source file's rules.
        let mut merged: HashSet<(String, Origin)> = HashSet::new();

        // Clone the imports to avoid borrowing conflicts
        let imports = spec.imports.clone();

        for import in &imports {
            match import {
                ImportStatement::Include { path, alias, .. } => {
                    self.resolve_include(
                        spec,
                        path,
                        alias.as_deref(),
                        base_dir,
                        &mut origins,
                        &mut merged,
                    )?;
                }
                ImportStatement::SelectiveImport { path, items, .. } => {
                    self.resolve_selective_import(
                        spec,
                        path,
                        items,
                        base_dir,
                        &mut origins,
                        &mut merged,
                    )?;
                }
            }
        }

        if newly_resolving {
            self.resolving.remove(&canonical_base);
        }

        // Clear imports after resolution since they've been merged
        spec.imports.clear();

        // Re-run the `/=` merge over the now-complete rule set: `Parser::parse` already
        // folded same-file extensions, but a base `=` rule and its `/=` extension can
        // live in different files (either one included, either order), and that pairing
        // only exists once the included files' rules have been appended above.
        // `collapse_orphans: false` — this may itself be a leaf file included by a
        // parent that still hasn't been merged in.
        crate::ast::merge_type_choice_extensions(&mut spec.rules, false);

        Ok(origins)
    }

    /// Resolve an include statement (brings in all rules, optionally with namespace).
    ///
    /// `origins` and `merged` are the caller's per-spec bookkeeping (see
    /// `resolve_imports_uncollapsed`): every rule actually pushed into `spec` is
    /// recorded in both, keyed by its *true* origin rather than this file's path, so a
    /// later `include`/`from` of the same underlying file — reached directly or via yet
    /// another re-export — is recognized as the same rule instead of duplicated
    /// (Finding 1), and so a plain `include` and a `from ... include` of the same file
    /// under the same (absent) alias merge the overlapping rule at most once
    /// (Finding 2).
    #[allow(clippy::too_many_arguments)]
    fn resolve_include(
        &mut self,
        spec: &mut CsilSpec,
        path: &str,
        alias: Option<&str>,
        base_dir: &Path,
        origins: &mut HashMap<String, Origin>,
        merged: &mut HashSet<(String, Origin)>,
    ) -> Result<()> {
        let resolved_path = self.resolve_file_path(path, base_dir)?;
        let (imported_spec, imported_origins) = self.load_and_resolve_file(path, base_dir)?;

        // Merge rules with optional namespace prefix
        for rule in imported_spec.rules {
            let origin = imported_origins
                .get(&rule.name)
                .cloned()
                .unwrap_or_else(|| (resolved_path.clone(), rule.name.clone()));
            let final_name = match alias {
                Some(alias) => format!("{alias}.{}", rule.name),
                None => rule.name.clone(),
            };

            if !merged.insert((final_name.clone(), origin.clone())) {
                // Same true origin already merged into `spec` under this exact final
                // name — a diamond (or an overlapping plain-include/selective-import
                // pair), not a genuine collision. Two rules that merely *end up* with
                // the same final name but different origins are a real collision and
                // are intentionally left in place for `validate_spec` to reject.
                continue;
            }

            let mut rule = rule;
            rule.name = final_name.clone();
            spec.rules.push(rule);
            origins.insert(final_name, origin);
        }

        Ok(())
    }

    /// Resolve a selective import statement (brings in only specified rules).
    ///
    /// See `resolve_include`'s doc for what `origins`/`merged` do. Selective imports
    /// have no per-item rename syntax (`from f include X` always keeps the name `X` —
    /// the grammar doesn't support `as` here), so unlike `resolve_include` there is no
    /// alias to fold into the final name.
    #[allow(clippy::too_many_arguments)]
    fn resolve_selective_import(
        &mut self,
        spec: &mut CsilSpec,
        path: &str,
        items: &[String],
        base_dir: &Path,
        origins: &mut HashMap<String, Origin>,
        merged: &mut HashSet<(String, Origin)>,
    ) -> Result<()> {
        let resolved_path = self.resolve_file_path(path, base_dir)?;
        let (imported_spec, imported_origins) = self.load_and_resolve_file(path, base_dir)?;

        // Only import specified items
        for item_name in items {
            let Some(rule) = imported_spec.rules.iter().find(|r| &r.name == item_name) else {
                bail!("Item '{}' not found in '{}'", item_name, path);
            };
            let origin = imported_origins
                .get(item_name)
                .cloned()
                .unwrap_or_else(|| (resolved_path.clone(), item_name.clone()));

            if !merged.insert((item_name.clone(), origin.clone())) {
                // Same true origin already reached `spec` under this name — either a
                // diamond `from` re-import, or this exact item already arrived via a
                // plain `include` of the same file (Finding 2's mixed-guard case).
                continue;
            }

            spec.rules.push(rule.clone());
            origins.insert(item_name.clone(), origin);
        }

        Ok(())
    }

    /// Load a file and resolve its imports recursively. Returns the fully-materialized
    /// spec together with its `Origin` map (see `resolve_imports_uncollapsed`); both
    /// are cached together so a spec served from cache is always complete and can still
    /// feed a consuming spec's diamond-dedup guard correctly (Finding 1(c)).
    fn load_and_resolve_file(
        &mut self,
        path: &str,
        base_dir: &Path,
    ) -> Result<(CsilSpec, HashMap<String, Origin>)> {
        let resolved_path = self.resolve_file_path(path, base_dir)?;

        // Check for circular dependencies
        if self.resolving.contains(&resolved_path) {
            bail!("Circular dependency detected: {}", resolved_path.display());
        }

        // Check cache first
        if let Some(cached) = self.resolved_cache.get(&resolved_path) {
            return Ok(cached.clone());
        }

        // Mark as resolving
        self.resolving.insert(resolved_path.clone());

        // Load and resolve
        let mut spec = parse_csil_file(&resolved_path).with_context(|| {
            format!("Failed to parse imported file: {}", resolved_path.display())
        })?;

        // Recurse without finalizing orphaned `/=` rules: this file may itself be
        // included by something else that supplies the base (see `resolve_imports`'s
        // doc), and the cached spec below may be reused from a different root. This
        // call gets its own fresh, local `origins`/`merged` state (see
        // `resolve_imports_uncollapsed`) — this spec is assembled independently of
        // whatever spec is including it.
        let origins = self.resolve_imports_uncollapsed(&mut spec, &resolved_path)?;

        // Cache and return
        self.resolving.remove(&resolved_path);
        self.resolved_cache
            .insert(resolved_path, (spec.clone(), origins.clone()));

        Ok((spec, origins))
    }

    /// Resolve a file path using search paths
    fn resolve_file_path(&self, path: &str, base_dir: &Path) -> Result<PathBuf> {
        let path_buf = PathBuf::from(path);

        // Try relative to base directory first
        let relative_path = base_dir.join(&path_buf);
        if relative_path.exists() {
            return Ok(relative_path.canonicalize()?);
        }

        // Try search paths
        for search_path in &self.search_paths {
            let candidate = search_path.join(&path_buf);
            if candidate.exists() {
                return Ok(candidate.canonicalize()?);
            }
        }

        bail!("Could not resolve import path: {}", path);
    }
}

impl Default for ImportResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A standalone `/=` (no `=` base anywhere) is left tagged `TypeChoice` by a bare
    /// `parse_csil` (see the parser tests), but the real pipeline always runs
    /// `resolve_imports` even for a file with zero `include` statements — at that point
    /// there is nowhere else a base could come from, so it must finalize into exactly
    /// the same `TypeDef(Choice(..))` shape `Status = "a" / "b" / "c"` would produce.
    #[test]
    fn test_standalone_type_choice_collapses_via_resolve_imports() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("status.csil"),
            r#"Status /= "a" / "b" / "c""#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("status.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("status.csil"))
            .unwrap();

        assert_eq!(spec.rules.len(), 1);
        match &spec.rules[0].rule_type {
            RuleType::TypeDef(TypeExpression::Choice(arms)) => {
                let literals: Vec<&str> = arms
                    .iter()
                    .map(|arm| match arm {
                        TypeExpression::Literal(LiteralValue::Text(s)) => s.as_str(),
                        other => panic!("expected text literal arm, got {other:?}"),
                    })
                    .collect();
                assert_eq!(literals, vec!["a", "b", "c"]);
            }
            other => panic!("expected TypeDef(Choice(..)), got {other:?}"),
        }

        crate::validate_spec(&spec).expect("standalone /= should validate cleanly");
    }

    /// A single-arm standalone `/=` (no `/` at all) collapses to a plain `TypeDef` once
    /// finalized, matching what `Name = <that type>` would have parsed to, rather than
    /// leaving a one-arm `Choice` behind.
    #[test]
    fn test_standalone_type_choice_single_arm_collapses_via_resolve_imports() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("alias.csil"), "Alias /= text").unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("alias.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("alias.csil"))
            .unwrap();

        assert_eq!(spec.rules.len(), 1);
        assert!(matches!(
            &spec.rules[0].rule_type,
            RuleType::TypeDef(TypeExpression::Builtin(b)) if b == "text"
        ));
    }

    #[test]
    fn test_simple_include() {
        let temp_dir = TempDir::new().unwrap();

        // Create base.csil
        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "types.csil"
        
        service TestService {
            test: Request -> Response
        }
        "#,
        )
        .unwrap();

        // Create types.csil
        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have 3 rules: Request, Response, TestService
        assert_eq!(spec.rules.len(), 3);
        assert!(spec.imports.is_empty()); // Imports should be cleared after resolution
    }

    #[test]
    fn test_selective_import() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        from "types.csil" include Request
        
        service TestService {
            test: Request -> { success: bool }
        }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        Internal = { secret: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have 2 rules: Request and TestService (not Response or Internal)
        assert_eq!(spec.rules.len(), 2);
        assert!(spec.rules.iter().any(|r| r.name == "Request"));
        assert!(!spec.rules.iter().any(|r| r.name == "Response"));
        assert!(!spec.rules.iter().any(|r| r.name == "Internal"));
    }

    #[test]
    fn test_namespace_alias() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "user/types.csil" as user
        
        service TestService {
            test: user.Request -> user.Response
        }
        "#,
        )
        .unwrap();

        fs::create_dir(temp_dir.path().join("user")).unwrap();
        fs::write(
            temp_dir.path().join("user/types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have namespaced names
        assert!(spec.rules.iter().any(|r| r.name == "user.Request"));
        assert!(spec.rules.iter().any(|r| r.name == "user.Response"));
    }

    #[test]
    fn test_circular_dependency_detection() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("a.csil"),
            r#"
        include "b.csil"
        TypeA = { field: int }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "a.csil"
        TypeB = { field: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("a.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("a.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Circular dependency")
        );
    }

    #[test]
    fn test_nested_imports() {
        let temp_dir = TempDir::new().unwrap();

        // Create main.csil -> common.csil -> base.csil
        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "common.csil"
        
        MainType = { data: CommonType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("common.csil"),
            r#"
        include "base.csil"
        
        CommonType = { base: BaseType, extra: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        BaseType = { id: int, name: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .unwrap();

        // Should have all types from the chain
        assert_eq!(spec.rules.len(), 3);
        assert!(spec.rules.iter().any(|r| r.name == "BaseType"));
        assert!(spec.rules.iter().any(|r| r.name == "CommonType"));
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
    }

    #[test]
    fn test_missing_import_file() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "nonexistent.csil"
        MyType = { field: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("base.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Could not resolve import path")
        );
    }

    #[test]
    fn test_missing_selective_import_item() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        from "types.csil" include Request, NonExistent
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("base.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Item 'NonExistent' not found")
        );
    }

    #[test]
    fn test_search_paths() {
        let temp_dir = TempDir::new().unwrap();
        let lib_dir = temp_dir.path().join("lib");
        fs::create_dir(&lib_dir).unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "shared.csil"
        MainType = { field: text }
        "#,
        )
        .unwrap();

        fs::write(
            lib_dir.join("shared.csil"),
            r#"
        SharedType = { id: int }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        resolver.add_search_path(lib_dir);

        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .unwrap();

        // Should find shared.csil in the lib directory
        assert_eq!(spec.rules.len(), 2);
        assert!(spec.rules.iter().any(|r| r.name == "SharedType"));
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
    }

    /// Diamond include: entry.csil includes mid.csil (which includes common.csil) AND
    /// includes common.csil directly. common.csil's rules must appear exactly once.
    #[test]
    fn test_diamond_include_deduplicates_rules() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "mid.csil"
        include "common.csil"

        EntryType = { data: MidType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("mid.csil"),
            r#"
        include "common.csil"

        MidType = { common: CommonType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("common.csil"),
            r#"
        CommonType = { id: int }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        // CommonType must be merged exactly once despite being reachable via two paths.
        let common_count = spec.rules.iter().filter(|r| r.name == "CommonType").count();
        assert_eq!(
            common_count, 1,
            "CommonType should be included exactly once"
        );
        assert_eq!(spec.rules.len(), 3);
        assert!(spec.rules.iter().any(|r| r.name == "MidType"));
        assert!(spec.rules.iter().any(|r| r.name == "EntryType"));

        // The deduplicated spec must also validate cleanly (no false DuplicateRule error)
        // via both the standard and CLI-facing optimized validation paths.
        crate::validate_spec(&spec).expect("diamond include should validate cleanly");
        crate::validate_spec_optimized(&spec)
            .expect("diamond include should validate cleanly via the optimized path");
    }

    /// Same as above but reversed statement order: direct include first, transitive second.
    /// Guards against the fix only working for one ordering of the diamond.
    #[test]
    fn test_diamond_include_deduplicates_regardless_of_order() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "common.csil"
        include "mid.csil"

        EntryType = { data: MidType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("mid.csil"),
            r#"
        include "common.csil"

        MidType = { common: CommonType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("common.csil"),
            r#"
        CommonType = { id: int }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let common_count = spec.rules.iter().filter(|r| r.name == "CommonType").count();
        assert_eq!(
            common_count, 1,
            "CommonType should be included exactly once"
        );
        crate::validate_spec(&spec).expect("diamond include should validate cleanly");
        crate::validate_spec_optimized(&spec)
            .expect("diamond include should validate cleanly via the optimized path");
    }

    /// Two genuinely different files defining the same rule name is a real collision, not a
    /// diamond, and must still surface as a validation error after the merge.
    #[test]
    fn test_distinct_files_same_rule_name_still_errors() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "one.csil"
        include "two.csil"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("one.csil"),
            r#"
        Shared = { id: int }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("two.csil"),
            r#"
        Shared = { name: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        // Resolution itself doesn't reject this (it just merges rules); validation must catch
        // the genuine name collision between two unrelated files.
        assert_eq!(spec.rules.len(), 2);
        let err = crate::validate_spec(&spec).unwrap_err();
        assert!(err.to_string().contains("Duplicate rule name 'Shared'"));

        // The CLI's `generate`/`validate` commands run the optimized path, which must catch
        // this collision too — it previously skipped the rule-name-uniqueness check entirely.
        let optimized_err = crate::validate_spec_optimized(&spec).unwrap_err();
        assert!(
            optimized_err
                .to_string()
                .contains("Duplicate rule name 'Shared'")
        );
    }

    /// Diamond via a selective (`from ... include`) import reaching the same item twice.
    #[test]
    fn test_diamond_selective_import_deduplicates() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "mid.csil"
        from "common.csil" include CommonType

        EntryType = { data: MidType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("mid.csil"),
            r#"
        from "common.csil" include CommonType

        MidType = { common: CommonType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("common.csil"),
            r#"
        CommonType = { id: int }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let common_count = spec.rules.iter().filter(|r| r.name == "CommonType").count();
        assert_eq!(
            common_count, 1,
            "CommonType should be selected exactly once"
        );
        crate::validate_spec(&spec).expect("diamond selective import should validate");
    }

    /// `Status = "a"` in the base file and `Status /= "b" / "c"` in an included file
    /// (RFC 8610 socket-extension semantics) must merge into a single rule, not
    /// collide as a duplicate — the merge only exists once the include is resolved.
    #[test]
    fn test_type_choice_extension_across_include_merges() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "extra.csil"

        Status = "a"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("extra.csil"),
            r#"
        Status /= "b" / "c"
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let status_rules: Vec<_> = spec.rules.iter().filter(|r| r.name == "Status").collect();
        assert_eq!(
            status_rules.len(),
            1,
            "extension across an include must merge, not duplicate"
        );
        match &status_rules[0].rule_type {
            RuleType::TypeDef(TypeExpression::Choice(arms)) => {
                let literals: Vec<&str> = arms
                    .iter()
                    .map(|arm| match arm {
                        TypeExpression::Literal(LiteralValue::Text(s)) => s.as_str(),
                        other => panic!("expected text literal arm, got {other:?}"),
                    })
                    .collect();
                assert_eq!(literals, vec!["a", "b", "c"]);
            }
            other => panic!("expected TypeDef(Choice(..)), got {other:?}"),
        }

        crate::validate_spec(&spec).expect("cross-include /= extension should validate cleanly");
        crate::validate_spec_optimized(&spec)
            .expect("cross-include /= extension should validate cleanly via optimized path");
    }

    /// Same as above but with the base rule and its extension swapped between files, so
    /// the merge doesn't accidentally depend on the base file always defining `=` first.
    #[test]
    fn test_type_choice_extension_across_include_merges_regardless_of_which_file_has_base() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "base.csil"

        Status /= "b" / "c"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        Status = "a"
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let status_rules: Vec<_> = spec.rules.iter().filter(|r| r.name == "Status").collect();
        assert_eq!(status_rules.len(), 1);
        match &status_rules[0].rule_type {
            RuleType::TypeDef(TypeExpression::Choice(arms)) => {
                assert_eq!(arms.len(), 3);
            }
            other => panic!("expected TypeDef(Choice(..)), got {other:?}"),
        }
        crate::validate_spec(&spec).expect("cross-include /= extension should validate cleanly");
    }

    /// A genuine duplicate `=` rule across two included files must still error even
    /// once `/=` merging is in play elsewhere in the same spec — merging must not
    /// paper over real name collisions.
    #[test]
    fn test_duplicate_rule_across_includes_still_errors_alongside_type_choice_merge() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "one.csil"
        include "two.csil"

        Mode = "a"
        Mode /= "b"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("one.csil"),
            r#"
        Status = "a"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("two.csil"),
            r#"
        Status = "z"
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        // The unrelated `Mode` extension must have merged cleanly...
        let mode_rules: Vec<_> = spec.rules.iter().filter(|r| r.name == "Mode").collect();
        assert_eq!(mode_rules.len(), 1, "Mode's /= extension should merge");

        // ...while the genuine `Status` collision between the two included files
        // is still reported.
        let err = crate::validate_spec(&spec).unwrap_err();
        assert!(err.to_string().contains("Duplicate rule name 'Status'"));
    }

    /// Finding 1(a): `main` both plain-includes `d` directly *and* selectively imports
    /// an item from `b`, where `b` itself plain-includes `d`. Before the per-spec
    /// scoping fix, `main`'s direct `include d` registered `d` as globally "already
    /// included", so when `b` was resolved (to satisfy the selective import) its own
    /// `include d` was silently skipped — `b`'s resolved spec then lacked `Item`, and
    /// resolution hard-errored "Item 'Item' not found in 'b.csil'". This must now
    /// resolve cleanly with `Item` present exactly once.
    #[test]
    fn test_finding1a_plain_include_then_selective_import_of_diamond_transitive_item() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "d.csil"
        from "b.csil" include Item

        MainType = { x: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "d.csil"

        BThing = { y: text }
        "#,
        )
        .unwrap();

        fs::write(temp_dir.path().join("d.csil"), "Item = { id: int }").unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .expect("b's own include of d must still resolve even though main separately included d directly");

        let item_count = spec.rules.iter().filter(|r| r.name == "Item").count();
        assert_eq!(item_count, 1, "Item should appear exactly once");
        // `BThing` was never requested (main only selectively imported `Item` from b),
        // so a full `b` merge must not have happened as a side effect.
        assert!(!spec.rules.iter().any(|r| r.name == "BThing"));
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
        assert_eq!(spec.rules.len(), 2);

        crate::validate_spec(&spec).expect("should validate cleanly");
        crate::validate_spec_optimized(&spec).expect("should validate cleanly via optimized path");
    }

    /// Finding 1(b): `main` plain-includes `d` directly *and* plain-includes `b` under
    /// an alias, where `b` itself plain-includes `d`. Before the fix, `b`'s resolved
    /// spec silently lacked `d`'s rules (same root cause as 1(a)), so `ns.Item` never
    /// existed and any reference to it would fail. `ns.Item` must now exist alongside
    /// the unaliased `Item` from main's own direct include.
    #[test]
    fn test_finding1b_plain_include_then_aliased_include_of_diamond_transitive_item() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "d.csil"
        include "b.csil" as ns

        MainType = { x: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "d.csil"

        BThing = { y: text }
        "#,
        )
        .unwrap();

        fs::write(temp_dir.path().join("d.csil"), "Item = { id: int }").unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .expect("aliased include of b must carry d's rules along with it");

        // The unaliased direct include of d.
        assert!(spec.rules.iter().any(|r| r.name == "Item"));
        // b's own rules, namespaced...
        assert!(spec.rules.iter().any(|r| r.name == "ns.BThing"));
        // ...including the ones b itself pulled in transitively from d. This is the
        // rule that was missing entirely before the fix.
        assert!(
            spec.rules.iter().any(|r| r.name == "ns.Item"),
            "ns.Item must exist: b's own include of d must have been fully resolved"
        );
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
        assert_eq!(spec.rules.len(), 4);

        crate::validate_spec(&spec).expect("should validate cleanly");
    }

    /// Finding 1(c): resolving `main` (which nests `b`'s resolution behind a selective
    /// import) must leave a *complete* `b` spec in the resolver's cache, not an
    /// incomplete one poisoned by `main`'s own unrelated direct include of `d`. Proven
    /// by re-resolving `b.csil` directly, through the *same* resolver instance
    /// afterwards, and checking the cache-served spec still has everything `b` is
    /// supposed to have.
    #[test]
    fn test_finding1c_cached_spec_not_poisoned_by_sibling_resolution() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "d.csil"
        from "b.csil" include Item

        MainType = { x: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "d.csil"

        BThing = { y: text }
        "#,
        )
        .unwrap();

        fs::write(temp_dir.path().join("d.csil"), "Item = { id: int }").unwrap();

        let mut resolver = ImportResolver::new();

        // Resolve main first; this resolves and caches b.csil as a side effect of the
        // selective import.
        let mut main_spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut main_spec, &temp_dir.path().join("main.csil"))
            .unwrap();

        // Now resolve b.csil directly, as its own top-level spec, through the SAME
        // resolver instance — this must hit the cache, and the cached spec must be
        // complete (BThing + Item), not the incomplete pre-fix version (BThing only).
        let mut b_spec = parse_csil_file(temp_dir.path().join("b.csil")).unwrap();
        resolver
            .resolve_imports(&mut b_spec, &temp_dir.path().join("b.csil"))
            .unwrap();

        assert!(b_spec.rules.iter().any(|r| r.name == "BThing"));
        assert!(
            b_spec.rules.iter().any(|r| r.name == "Item"),
            "b.csil's cached spec must still include Item from its own `include d.csil`"
        );
        assert_eq!(b_spec.rules.len(), 2);
    }

    /// Complements 1(c): the same fixture, but `b.csil` is resolved standalone
    /// *first*, populating the cache, and `main.csil` (whose selective import triggers
    /// a cache *hit* rather than a fresh resolution) is resolved second. The
    /// cache-hit path must serve the same complete spec as a fresh resolution would.
    #[test]
    fn test_cache_hit_serves_complete_spec_to_selective_import() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "d.csil"
        from "b.csil" include Item

        MainType = { x: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "d.csil"

        BThing = { y: text }
        "#,
        )
        .unwrap();

        fs::write(temp_dir.path().join("d.csil"), "Item = { id: int }").unwrap();

        let mut resolver = ImportResolver::new();

        let mut b_spec = parse_csil_file(temp_dir.path().join("b.csil")).unwrap();
        resolver
            .resolve_imports(&mut b_spec, &temp_dir.path().join("b.csil"))
            .unwrap();
        assert_eq!(b_spec.rules.len(), 2);

        let mut main_spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut main_spec, &temp_dir.path().join("main.csil"))
            .expect("selective import of Item from the already-cached b.csil must succeed");

        let item_count = main_spec.rules.iter().filter(|r| r.name == "Item").count();
        assert_eq!(item_count, 1);
    }

    /// Finding 2: a plain `include f` and a selective `from f include X` of the *same*
    /// unaliased item must merge `X` exactly once, not twice — mixing the two guards
    /// used to be disjoint, so this hard-errored "Duplicate rule name" once
    /// `validate_spec_optimized` started enforcing rule-name uniqueness. Order 1:
    /// plain include first.
    #[test]
    fn test_finding2_plain_include_then_selective_import_same_item_no_duplicate() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "f.csil"
        from "f.csil" include X
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("f.csil"),
            r#"
        X = { a: int }
        Y = { b: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let x_count = spec.rules.iter().filter(|r| r.name == "X").count();
        assert_eq!(x_count, 1, "X must merge exactly once");
        assert_eq!(spec.rules.len(), 2);
        crate::validate_spec_optimized(&spec)
            .expect("mixed plain+selective import of the same item must not be a duplicate");
    }

    /// Same as above with the two import statements swapped, guarding against the fix
    /// only working for one ordering.
    #[test]
    fn test_finding2_selective_import_then_plain_include_same_item_no_duplicate() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        from "f.csil" include X
        include "f.csil"
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("f.csil"),
            r#"
        X = { a: int }
        Y = { b: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        let x_count = spec.rules.iter().filter(|r| r.name == "X").count();
        assert_eq!(x_count, 1, "X must merge exactly once");
        assert_eq!(spec.rules.len(), 2);
        crate::validate_spec_optimized(&spec)
            .expect("mixed selective+plain import of the same item must not be a duplicate");
    }

    /// Mixing an *aliased* full include with an unaliased selective import of the same
    /// file is not a diamond: `ns.X` and `X` are different final rule names, so both
    /// are intentionally kept — the selective import isn't a redundant re-statement of
    /// something already reachable as `ns.X`, it's how the caller opts into the
    /// unprefixed name too. (The grammar has no `from f include X as y` to rename a
    /// selective import, so aliasing is only ever a whole-file `include ... as` thing.)
    #[test]
    fn test_finding2_aliased_include_plus_unaliased_selective_import_both_kept() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        include "f.csil" as ns
        from "f.csil" include X
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("f.csil"),
            r#"
        X = { a: int }
        Y = { b: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        assert!(spec.rules.iter().any(|r| r.name == "ns.X"));
        assert!(spec.rules.iter().any(|r| r.name == "ns.Y"));
        assert!(spec.rules.iter().any(|r| r.name == "X"));
        assert_eq!(spec.rules.len(), 3);
        crate::validate_spec_optimized(&spec)
            .expect("ns.X and X are distinct rule names and must both validate cleanly");
    }

    /// Same intentional-both-kept mix, reversed order (selective import first, aliased
    /// include second).
    #[test]
    fn test_finding2_unaliased_selective_import_plus_aliased_include_both_kept() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("entry.csil"),
            r#"
        from "f.csil" include X
        include "f.csil" as ns
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("f.csil"),
            r#"
        X = { a: int }
        Y = { b: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("entry.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("entry.csil"))
            .unwrap();

        assert!(spec.rules.iter().any(|r| r.name == "ns.X"));
        assert!(spec.rules.iter().any(|r| r.name == "ns.Y"));
        assert!(spec.rules.iter().any(|r| r.name == "X"));
        assert_eq!(spec.rules.len(), 3);
        crate::validate_spec_optimized(&spec)
            .expect("ns.X and X are distinct rule names and must both validate cleanly");
    }
}
