//! P24 — porting coverage ledger (gap #4).
//!
//! When rewriting project A (source) into project B (target), the single
//! question that matters for "no omissions" is: *which source symbols have
//! a counterpart in the target, and which are still missing?* The code
//! graph already knows every symbol on both sides; this module diffs two
//! graph databases by **symbol name** and produces a ledger:
//!
//! - `ported` — source symbols whose name also exists in the target,
//! - `missing` — source symbols with no target counterpart (the to-do list),
//! - `extra` — target-only names (new scaffolding / helpers),
//! - per-file coverage so you can see which source files are fully ported.
//!
//! Matching is by leaf name across languages (a Dart `class Shift` and a
//! Swift `struct Shift` both have the name `Shift`), because a rewrite
//! deliberately changes the node *kind*. Name matching can over-credit
//! ubiquitous names (`build`, `new`); the per-file breakdown and the
//! explicit `missing` list keep the result honest, and the JSON form lets
//! an agent re-check any match against `facts` / `constants`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use specslice_core::language_traits::{is_callable, is_type};
use specslice_core::Node;
use specslice_store::Store;

use crate::path_class::{is_generated_path, is_test_path};

pub const PORT_COVERAGE_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortedSymbol {
    pub name: String,
    pub source_kind: String,
    pub source_path: Option<String>,
    /// Distinct target node kinds that matched this name (sorted).
    pub target_kinds: Vec<String>,
    pub target_count: usize,
    /// Set when the match came from a port-map alias rather than an exact
    /// name match: the target identifier the source name was mapped onto
    /// (e.g. Dart `nameInputUnits` → Swift `inputUnits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingSymbol {
    pub name: String,
    pub kind: String,
    pub path: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileCoverage {
    pub path: String,
    pub total: usize,
    pub ported: usize,
    pub coverage: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortCoverageStats {
    pub source_symbols: usize,
    pub source_distinct_names: usize,
    pub target_symbols: usize,
    pub target_distinct_names: usize,
    pub ported_names: usize,
    pub missing_names: usize,
    pub extra_names: usize,
    /// `ported_names / source_distinct_names` (0.0 when no source symbols).
    pub coverage: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortCoverageReport {
    pub schema_version: u32,
    pub stats: PortCoverageStats,
    pub missing: Vec<MissingSymbol>,
    pub ported: Vec<PortedSymbol>,
    pub by_file: Vec<FileCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PortCoverageOptions {
    pub source_db: PathBuf,
    pub target_db: PathBuf,
    /// Match type containers in addition to callables (default `true`).
    pub include_types: bool,
    /// Populate the `extra` list of target-only names (default `false`).
    pub include_extra: bool,
    /// Drop machine-generated codegen files (freezed/.g.dart/l10n/…). Without
    /// this the to-port universe is dominated by plumbing the rewrite never
    /// reproduces (default `true`).
    pub skip_generated: bool,
    /// Drop test/spec scaffolding (default `true`).
    pub skip_tests: bool,
    /// Drop synthetic / anonymous names like `<default>` constructors that are
    /// not portable identifiers (default `true`).
    pub skip_synthetic_names: bool,
    /// Match on a normalized identifier (strip leading `_` privacy prefixes) so
    /// a Dart `_foo` matches a Swift `foo`. Off by default — it can over-credit
    /// unrelated names that collide after normalization (default `false`).
    pub normalize_names: bool,
    /// Case-insensitive name matching. A Java→Go port capitalises exported
    /// identifiers (`selectCraftTree` → `SelectCraftTree`); C#/Pascal targets
    /// do the same. Off by default — like `normalize_names` it can over-credit
    /// names that collide only after case folding (default `false`).
    pub ignore_case: bool,
    /// Explicit source-name → target-name aliases for ports that legitimately
    /// rename a symbol to the target language's idiom (Dart `nameInputUnits`
    /// → Swift `inputUnits`). A source name with no exact/normalized match is
    /// credited as ported if its alias target exists. This keeps the coverage
    /// ledger honest when a faithful port is not a verbatim rename.
    pub aliases: BTreeMap<String, String>,
    /// Source leaf names to drop entirely (exact match) — host-framework
    /// scaffolding / language idioms with no portable counterpart.
    pub ignore_names: BTreeSet<String>,
    /// Source leaf-name prefixes to drop (e.g. `_build` for Flutter `build*`
    /// view helpers). Applied after exact `ignore_names`.
    pub ignore_name_prefixes: Vec<String>,
    /// Extra user globs applied to source *and* target node paths.
    pub exclude: Vec<String>,
    /// Source-only include scope: when non-empty, a source symbol counts toward
    /// coverage only if its path matches one of these globs (paths with no path
    /// are excluded). This scopes the *denominator* to one slice of the source
    /// — e.g. `**/rcmtm-cloud-craft/**` to measure just the craft microservice's
    /// port progress out of a big monolith. Does not affect the target side.
    pub source_include: Vec<String>,
    /// Source-only exclude globs, applied after `source_include`. Unlike
    /// `exclude` (both sides), this drops paths from the source denominator only
    /// — e.g. exclude a not-yet-in-scope sibling service without hiding any
    /// target symbol from the `extra` list.
    pub source_exclude: Vec<String>,
    /// Cap `missing` / `ported` / `extra` list lengths (0 = unlimited).
    pub max_items: usize,
}

impl Default for PortCoverageOptions {
    fn default() -> Self {
        Self {
            source_db: PathBuf::new(),
            target_db: PathBuf::new(),
            include_types: true,
            include_extra: false,
            skip_generated: true,
            skip_tests: true,
            skip_synthetic_names: true,
            normalize_names: false,
            ignore_case: false,
            aliases: BTreeMap::new(),
            ignore_names: BTreeSet::new(),
            ignore_name_prefixes: Vec::new(),
            exclude: Vec::new(),
            source_include: Vec::new(),
            source_exclude: Vec::new(),
            max_items: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Port-map file (`--port-map`)
// ---------------------------------------------------------------------------

/// On-disk port-map: a YAML file declaring how a rewrite maps onto the target
/// language. Two concerns:
///
/// - `aliases` credit idiomatic renames (Dart `nameInputUnits` → Swift
///   `inputUnits`) that an exact-name diff would miss;
/// - `ignore_names` / `ignore_name_prefixes` drop source symbols that have no
///   portable counterpart *by design* — typically host-framework scaffolding
///   (Flutter `createState`/`initState`/`dispose`/`build*`) or language
///   idioms the target expresses differently (`props`, `toString`). Without
///   this the coverage number is dominated by code the rewrite never
///   reproduces, making it useless as a progress signal.
///
/// ```yaml
/// aliases:
///   nameInputUnits: inputUnits
///   fromJson: fromJSON
/// ignore_names:
///   - createState
///   - initState
///   - dispose
///   - props
/// ignore_name_prefixes:
///   - _build
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
pub struct PortMapFile {
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub ignore_names: Vec<String>,
    #[serde(default)]
    pub ignore_name_prefixes: Vec<String>,
}

/// Load and parse a YAML port-map file.
pub fn load_port_map(path: &std::path::Path) -> Result<PortMapFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading port-map {}", path.display()))?;
    let parsed: PortMapFile = serde_yml::from_str(&text)
        .with_context(|| format!("parsing port-map {}", path.display()))?;
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_port_coverage(options: PortCoverageOptions) -> Result<PortCoverageReport> {
    let source = Store::open(&options.source_db)
        .with_context(|| format!("opening source graph at {}", options.source_db.display()))?;
    let target = Store::open(&options.target_db)
        .with_context(|| format!("opening target graph at {}", options.target_db.display()))?;
    analyze_port_coverage_with_stores(&source, &target, &options)
}

pub fn analyze_port_coverage_with_stores(
    source: &Store,
    target: &Store,
    options: &PortCoverageOptions,
) -> Result<PortCoverageReport> {
    let user_globs =
        build_globset(&options.exclude).context("compiling port-coverage exclude globs")?;
    let source_include_globs = build_globset(&options.source_include)
        .context("compiling port-coverage --source-include globs")?;
    let source_exclude_globs = build_globset(&options.source_exclude)
        .context("compiling port-coverage --source-exclude globs")?;

    let path_excluded = |path: Option<&str>| -> bool {
        let Some(p) = path else { return false };
        if options.skip_generated && is_generated_path(p) {
            return true;
        }
        if options.skip_tests && is_test_path(p) {
            return true;
        }
        user_globs.is_match(p)
    };
    let name_excluded = |name: &str| -> bool {
        if options.skip_synthetic_names && (name.starts_with('<') || name.is_empty()) {
            return true;
        }
        if options.ignore_names.contains(name) {
            return true;
        }
        options
            .ignore_name_prefixes
            .iter()
            .any(|p| !p.is_empty() && name.starts_with(p.as_str()))
    };
    let eligible = |n: &Node| -> bool {
        if !(is_callable(n.kind) || (options.include_types && is_type(n.kind))) {
            return false;
        }
        match &n.name {
            None => false,
            Some(name) => !name_excluded(name) && !path_excluded(n.path.as_deref()),
        }
    };

    // Source-only scope: shrink the coverage *denominator* to one slice without
    // touching the target side. `source_include` (when set) is an allow-list;
    // `source_exclude` then removes from what remains. A node with no path is
    // kept only when no include scope is configured.
    let has_source_include = !options.source_include.is_empty();
    let in_source_scope = |path: Option<&str>| -> bool {
        match path {
            None => !has_source_include,
            Some(p) => {
                if has_source_include && !source_include_globs.is_match(p) {
                    return false;
                }
                !source_exclude_globs.is_match(p)
            }
        }
    };

    let source_nodes: Vec<Node> = source
        .list_all_nodes()?
        .into_iter()
        .filter(|n| eligible(n) && in_source_scope(n.path.as_deref()))
        .collect();
    let target_nodes: Vec<Node> = target
        .list_all_nodes()?
        .into_iter()
        .filter(|n| eligible(n))
        .collect();

    // Matching key: optionally normalized (strip leading `_`) so privacy
    // prefixes do not block a match. Lists still show original names.
    let key_of = |name: &str| -> String {
        let base = if options.normalize_names {
            normalize_ident(name)
        } else {
            name.to_string()
        };
        if options.ignore_case {
            base.to_lowercase()
        } else {
            base
        }
    };

    // target matching-key -> set of kinds
    let mut target_by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in &target_nodes {
        if let Some(name) = &n.name {
            target_by_name
                .entry(key_of(name))
                .or_default()
                .insert(n.kind.as_str().to_string());
        }
    }

    // source distinct names + a representative node per name (smallest id for
    // determinism) for the `ported` list.
    let mut source_names: BTreeSet<String> = BTreeSet::new();
    let mut ported: Vec<PortedSymbol> = Vec::new();
    let mut missing: Vec<MissingSymbol> = Vec::new();
    let mut ported_names: BTreeSet<String> = BTreeSet::new();
    let mut emitted_ported: BTreeSet<String> = BTreeSet::new();

    // file -> (total, ported)
    let mut file_totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Target keys consumed via an alias — excluded from `extra` so an aliased
    // port is not double-counted as both "ported" and "target-only".
    let mut alias_consumed_keys: BTreeSet<String> = BTreeSet::new();

    // Resolve a source name to a matching target key: exact (normalized) first,
    // then a port-map alias. Returns the matched target key and the alias used.
    let resolve_match = |name: &str, key: &str| -> Option<(String, Option<String>)> {
        if target_by_name.contains_key(key) {
            return Some((key.to_string(), None));
        }
        let alias = options.aliases.get(name)?;
        let akey = key_of(alias);
        if target_by_name.contains_key(&akey) {
            Some((akey, Some(alias.clone())))
        } else {
            None
        }
    };

    // Stable ordering: sort source nodes by (name, id).
    let mut sorted_source = source_nodes.clone();
    sorted_source.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.id.to_string().cmp(&b.id.to_string()))
    });

    for n in &sorted_source {
        // Defensive: nameless nodes are filtered upstream, but DB drift or a
        // weakened filter must skip rather than panic mid-analysis.
        let Some(name) = n.name.clone() else { continue };
        let key = key_of(&name);
        source_names.insert(key.clone());
        let matched = resolve_match(&name, &key);
        let is_ported = matched.is_some();
        if let Some(path) = &n.path {
            let entry = file_totals.entry(path.clone()).or_insert((0, 0));
            entry.0 += 1;
            if is_ported {
                entry.1 += 1;
            }
        }
        if let Some((target_key, via_alias)) = matched {
            ported_names.insert(key.clone());
            if via_alias.is_some() {
                alias_consumed_keys.insert(target_key.clone());
            }
            if emitted_ported.insert(key.clone()) {
                let kinds = target_by_name.get(&target_key).cloned().unwrap_or_default();
                ported.push(PortedSymbol {
                    name: name.clone(),
                    source_kind: n.kind.as_str().to_string(),
                    source_path: n.path.clone(),
                    target_count: kinds.len(),
                    target_kinds: kinds.into_iter().collect(),
                    via_alias,
                });
            }
        } else {
            missing.push(MissingSymbol {
                name,
                kind: n.kind.as_str().to_string(),
                path: n.path.clone(),
                line: n.start_line,
            });
        }
    }

    let mut by_file: Vec<FileCoverage> = file_totals
        .into_iter()
        .map(|(path, (total, ported))| FileCoverage {
            path,
            total,
            ported,
            coverage: ratio(ported, total),
        })
        .collect();
    // Least-covered files first — those are where work remains.
    by_file.sort_by(|a, b| {
        a.coverage
            .partial_cmp(&b.coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.total.cmp(&a.total))
            .then(a.path.cmp(&b.path))
    });

    let extra: Vec<String> = if options.include_extra {
        target_by_name
            .keys()
            .filter(|name| !source_names.contains(*name) && !alias_consumed_keys.contains(*name))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    let stats = PortCoverageStats {
        source_symbols: source_nodes.len(),
        source_distinct_names: source_names.len(),
        target_symbols: target_nodes.len(),
        target_distinct_names: target_by_name.len(),
        ported_names: ported_names.len(),
        missing_names: source_names.len() - ported_names.len(),
        extra_names: extra.len(),
        coverage: ratio(ported_names.len(), source_names.len()),
    };

    // missing is already in (name,id) order; ported too. Apply caps.
    if options.max_items > 0 {
        missing.truncate(options.max_items);
        ported.truncate(options.max_items);
    }
    let extra = if options.max_items > 0 && extra.len() > options.max_items {
        extra[..options.max_items].to_vec()
    } else {
        extra
    };

    Ok(PortCoverageReport {
        schema_version: PORT_COVERAGE_SCHEMA_VERSION,
        stats,
        missing,
        ported,
        by_file,
        extra,
    })
}

/// Normalize an identifier for cross-language matching: strip leading `_`
/// privacy prefixes (`_foo` → `foo`, `__x` → `x`). A name that is *only*
/// underscores is left intact so it never collapses to the empty string.
fn normalize_ident(name: &str) -> String {
    let trimmed = name.trim_start_matches('_');
    if trimmed.is_empty() {
        name.to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid glob `{p}`"))?);
    }
    builder.build().context("building globset")
}

fn ratio(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        0.0
    } else {
        // counts are tiny relative to f32 precision; lossless in practice.
        part as f32 / whole as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_core::{ArtifactId, NodeKind};

    fn store_with(symbols: &[(&str, NodeKind, &str)]) -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        for (i, (name, kind, path)) in symbols.iter().enumerate() {
            let mut n = Node::new(ArtifactId::new(format!("{path}#{name}#{i}")), *kind);
            n.name = Some((*name).to_string());
            n.path = Some((*path).to_string());
            n.start_line = Some(u32::try_from(i + 1).unwrap());
            store.upsert_node(&n).unwrap();
        }
        (store, dir)
    }

    #[test]
    fn coverage_by_name_across_languages() {
        // Source: Dart. Target: Swift. Names that match count as ported even
        // though the kinds differ.
        let (source, _s) = store_with(&[
            ("Shift", NodeKind::DartClass, "lib/models/shift.dart"),
            ("fromJson", NodeKind::DartMethod, "lib/models/shift.dart"),
            (
                "computePayroll",
                NodeKind::DartFunction,
                "lib/logic/pay.dart",
            ),
        ]);
        let (target, _t) = store_with(&[
            ("Shift", NodeKind::SwiftStruct, "Sources/Shift.swift"),
            ("fromJson", NodeKind::SwiftMethod, "Sources/Shift.swift"),
            (
                "ExtraHelper",
                NodeKind::SwiftFunction,
                "Sources/Extra.swift",
            ),
        ]);

        let report = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                include_extra: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(report.stats.source_distinct_names, 3);
        assert_eq!(report.stats.ported_names, 2);
        assert_eq!(report.stats.missing_names, 1);
        assert!((report.stats.coverage - 2.0 / 3.0).abs() < 1e-6);

        let missing_names: Vec<&str> = report.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing_names, vec!["computePayroll"]);

        // Shift matched a swift_struct.
        let shift = report.ported.iter().find(|p| p.name == "Shift").unwrap();
        assert_eq!(shift.target_kinds, vec!["swift_struct".to_string()]);

        // ExtraHelper is target-only.
        assert_eq!(report.extra, vec!["ExtraHelper".to_string()]);
    }

    #[test]
    fn per_file_coverage_orders_least_covered_first() {
        let (source, _s) = store_with(&[
            ("A", NodeKind::DartFunction, "lib/a.dart"),
            ("B", NodeKind::DartFunction, "lib/a.dart"),
            ("C", NodeKind::DartFunction, "lib/b.dart"),
        ]);
        let (target, _t) = store_with(&[("A", NodeKind::SwiftFunction, "x.swift")]);
        let report =
            analyze_port_coverage_with_stores(&source, &target, &PortCoverageOptions::default())
                .unwrap();
        // lib/b.dart has 0/1 ported (0.0); lib/a.dart has 1/2 (0.5) -> b first.
        assert_eq!(report.by_file[0].path, "lib/b.dart");
        assert!((report.by_file[0].coverage - 0.0).abs() < 1e-6);
        assert_eq!(report.by_file[1].path, "lib/a.dart");
        assert!((report.by_file[1].coverage - 0.5).abs() < 1e-6);
    }

    #[test]
    fn excludes_generated_tests_and_synthetic_by_default() {
        // Real-world noise: freezed/g.dart codegen, l10n, a test file, and a
        // `<default>` constructor must not pollute the to-port universe.
        let (source, _s) = store_with(&[
            (
                "computePayroll",
                NodeKind::DartFunction,
                "lib/logic/pay.dart",
            ),
            ("Shift", NodeKind::DartClass, "lib/models/shift.dart"),
            // codegen — must be dropped
            (
                "$ShiftCopyWith",
                NodeKind::DartClass,
                "lib/models/shift.freezed.dart",
            ),
            ("fromJson", NodeKind::DartMethod, "lib/models/shift.g.dart"),
            (
                "greeting",
                NodeKind::DartMethod,
                "lib/l10n/app_localizations_en.dart",
            ),
            // tests — must be dropped
            ("itComputes", NodeKind::DartFunction, "test/pay_test.dart"),
            // synthetic name — must be dropped
            (
                "<default>",
                NodeKind::DartConstructor,
                "lib/models/shift.dart",
            ),
        ]);
        let (target, _t) = store_with(&[("Shift", NodeKind::SwiftStruct, "Sources/Shift.swift")]);

        let report =
            analyze_port_coverage_with_stores(&source, &target, &PortCoverageOptions::default())
                .unwrap();

        // Only computePayroll + Shift survive filtering.
        assert_eq!(report.stats.source_distinct_names, 2);
        assert_eq!(report.stats.ported_names, 1); // Shift
        let missing: Vec<&str> = report.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing, vec!["computePayroll"]);
        // The generated freezed file must not appear in per-file coverage.
        assert!(report
            .by_file
            .iter()
            .all(|f| !f.path.contains("freezed") && !f.path.contains(".g.dart")));
    }

    #[test]
    fn normalize_names_matches_across_privacy_prefix() {
        // Dart privates carry a leading `_`; the Swift port uses the `private`
        // keyword and drops the prefix. With normalization on, `_compute`
        // (Dart) matches `compute` (Swift).
        let (source, _s) = store_with(&[
            ("_compute", NodeKind::DartFunction, "lib/a.dart"),
            ("keepMissing", NodeKind::DartFunction, "lib/a.dart"),
        ]);
        let (target, _t) = store_with(&[("compute", NodeKind::SwiftFunction, "x.swift")]);

        // Off (default): `_compute` is missing.
        let off =
            analyze_port_coverage_with_stores(&source, &target, &PortCoverageOptions::default())
                .unwrap();
        assert_eq!(off.stats.ported_names, 0);

        // On: `_compute` ↔ `compute`.
        let on = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                normalize_names: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(on.stats.ported_names, 1);
        let missing: Vec<&str> = on.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing, vec!["keepMissing"]);
    }

    #[test]
    fn port_map_aliases_credit_idiomatic_renames() {
        // A faithful port renames `nameInputUnits` (Dart) to `inputUnits`
        // (Swift idiom). Without an alias it reads as missing; with the
        // port-map it is credited as ported (via_alias), and the aliased
        // target is not double-counted as `extra`.
        let (source, _s) = store_with(&[
            (
                "nameInputUnits",
                NodeKind::DartFunction,
                "lib/utils/name_limits.dart",
            ),
            (
                "stillMissing",
                NodeKind::DartFunction,
                "lib/utils/name_limits.dart",
            ),
        ]);
        let (target, _t) = store_with(&[(
            "inputUnits",
            NodeKind::SwiftMethod,
            "Sources/NameLimits.swift",
        )]);

        // No alias: missing.
        let plain = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                include_extra: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plain.stats.ported_names, 0);
        assert_eq!(plain.extra, vec!["inputUnits".to_string()]);

        // With alias: nameInputUnits ↔ inputUnits.
        let mut aliases = BTreeMap::new();
        aliases.insert("nameInputUnits".to_string(), "inputUnits".to_string());
        let mapped = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                include_extra: true,
                aliases,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(mapped.stats.ported_names, 1);
        let ported = mapped
            .ported
            .iter()
            .find(|p| p.name == "nameInputUnits")
            .unwrap();
        assert_eq!(ported.via_alias.as_deref(), Some("inputUnits"));
        assert_eq!(ported.target_kinds, vec!["swift_method".to_string()]);
        // aliased target consumed → not reported as extra.
        assert!(mapped.extra.is_empty(), "extra was {:?}", mapped.extra);
        let missing: Vec<&str> = mapped.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing, vec!["stillMissing"]);
    }

    #[test]
    fn user_exclude_glob_drops_matching_source_files() {
        let (source, _s) = store_with(&[
            ("keep", NodeKind::DartFunction, "lib/logic/a.dart"),
            ("drop", NodeKind::DartFunction, "lib/vendor/b.dart"),
        ]);
        let (target, _t) = store_with(&[]);
        let report = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                exclude: vec!["**/vendor/**".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        let missing: Vec<&str> = report.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing, vec!["keep"]);
    }

    #[test]
    fn source_include_scopes_denominator_to_one_slice() {
        // Source (Java) spans two microservices; the target (Go) ported only the
        // craft slice. Unscoped coverage is 2/4; scoping the SOURCE to the craft
        // service makes the denominator 2 and coverage 100% — this is how you
        // measure progress on a single-service slice of a big monolith port.
        let (source, _s) = store_with(&[
            (
                "selectCraftTree",
                NodeKind::JavaMethod,
                "rcmtm-cloud-craft/src/CraftController.java",
            ),
            (
                "getDictSystem",
                NodeKind::JavaMethod,
                "rcmtm-cloud-craft/src/DictController.java",
            ),
            (
                "createOrder",
                NodeKind::JavaMethod,
                "rcmtm-cloud-order/src/OrderController.java",
            ),
            (
                "cancelOrder",
                NodeKind::JavaMethod,
                "rcmtm-cloud-order/src/OrderController.java",
            ),
        ]);
        let (target, _t) = store_with(&[
            (
                "SelectCraftTree",
                NodeKind::GoFunction,
                "internal/craft/handler.go",
            ),
            (
                "GetDictSystem",
                NodeKind::GoFunction,
                "internal/craft/handler.go",
            ),
        ]);

        // Unscoped baseline: 2 of 4 ported (ignore_case maps Java->Go casing).
        let base = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                ignore_case: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(base.stats.source_distinct_names, 4);
        assert_eq!(base.stats.ported_names, 2);

        // Scoped to the craft service: denominator 2, coverage 100%.
        let scoped = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                ignore_case: true,
                source_include: vec!["**/rcmtm-cloud-craft/**".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(scoped.stats.source_distinct_names, 2);
        assert_eq!(scoped.stats.ported_names, 2);
        assert!((scoped.stats.coverage - 1.0).abs() < 1e-9);
        assert!(
            scoped.missing.is_empty(),
            "scoped missing must be empty: {:?}",
            scoped.missing
        );
    }

    #[test]
    fn source_exclude_drops_slice_from_source_only() {
        // source_exclude removes the order slice from the SOURCE denominator
        // without touching the target (unlike `exclude`, which applies to both).
        let (source, _s) = store_with(&[
            (
                "selectCraftTree",
                NodeKind::JavaMethod,
                "rcmtm-cloud-craft/Craft.java",
            ),
            (
                "createOrder",
                NodeKind::JavaMethod,
                "rcmtm-cloud-order/Order.java",
            ),
        ]);
        let (target, _t) =
            store_with(&[("selectCraftTree", NodeKind::GoFunction, "internal/craft.go")]);
        let report = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                source_exclude: vec!["**/rcmtm-cloud-order/**".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.stats.source_distinct_names, 1);
        assert!(
            report.missing.is_empty(),
            "order excluded from source, craft ported -> no missing: {:?}",
            report.missing
        );
    }

    #[test]
    fn load_port_map_parses_aliases_and_ignores_yaml() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("port-map.yaml");
        std::fs::write(
            &path,
            "aliases:\n  nameInputUnits: inputUnits\n  limitNameInput: limitInput\n\
             ignore_names:\n  - createState\n  - dispose\n\
             ignore_name_prefixes:\n  - _build\n",
        )
        .unwrap();
        let map = load_port_map(&path).unwrap();
        assert_eq!(
            map.aliases.get("nameInputUnits").map(String::as_str),
            Some("inputUnits")
        );
        assert_eq!(map.aliases.len(), 2);
        assert_eq!(
            map.ignore_names,
            vec!["createState".to_string(), "dispose".to_string()]
        );
        assert_eq!(map.ignore_name_prefixes, vec!["_build".to_string()]);
    }

    #[test]
    fn ignore_names_and_prefixes_drop_framework_scaffolding() {
        // Flutter lifecycle (`createState`, `dispose`) and `build*` view
        // helpers have no by-name SwiftUI counterpart; ignoring them keeps the
        // coverage number about portable logic, not host-framework plumbing.
        let (source, _s) = store_with(&[
            (
                "computePayroll",
                NodeKind::DartFunction,
                "lib/logic/pay.dart",
            ),
            (
                "createState",
                NodeKind::DartMethod,
                "lib/home/home_page.dart",
            ),
            ("dispose", NodeKind::DartMethod, "lib/home/home_page.dart"),
            (
                "_buildHeader",
                NodeKind::DartMethod,
                "lib/home/home_page.dart",
            ),
        ]);
        let (target, _t) = store_with(&[(
            "computePayroll",
            NodeKind::SwiftFunction,
            "Sources/Pay.swift",
        )]);

        let mut ignore_names = BTreeSet::new();
        ignore_names.insert("createState".to_string());
        ignore_names.insert("dispose".to_string());
        let report = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                ignore_names,
                ignore_name_prefixes: vec!["_build".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        // Only computePayroll remains in the universe, and it is ported → 100%.
        assert_eq!(report.stats.source_distinct_names, 1);
        assert_eq!(report.stats.ported_names, 1);
        assert!(report.missing.is_empty(), "missing: {:?}", report.missing);
        assert!((report.stats.coverage - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ignore_case_matches_java_camel_to_go_pascal() {
        // A Java→Go port capitalises exported identifiers:
        // selectTrademarkConflictListTreeByCloth ↔ SelectTrademarkConflictListTreeByCloth.
        let (source, _s) = store_with(&[
            (
                "selectTrademarkConflictListTreeByCloth",
                NodeKind::JavaMethod,
                "C.java",
            ),
            ("keepMissing", NodeKind::JavaMethod, "C.java"),
        ]);
        let (target, _t) = store_with(&[(
            "SelectTrademarkConflictListTreeByCloth",
            NodeKind::GoFunction,
            "c.go",
        )]);

        // Off (default): case mismatch → missing.
        let off =
            analyze_port_coverage_with_stores(&source, &target, &PortCoverageOptions::default())
                .unwrap();
        assert_eq!(off.stats.ported_names, 0);

        // On: matches case-insensitively.
        let on = analyze_port_coverage_with_stores(
            &source,
            &target,
            &PortCoverageOptions {
                ignore_case: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(on.stats.ported_names, 1);
        let missing: Vec<&str> = on.missing.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(missing, vec!["keepMissing"]);
    }

    #[test]
    fn empty_source_is_zero_coverage_not_nan() {
        let (source, _s) = store_with(&[]);
        let (target, _t) = store_with(&[("X", NodeKind::SwiftFunction, "x.swift")]);
        let report =
            analyze_port_coverage_with_stores(&source, &target, &PortCoverageOptions::default())
                .unwrap();
        assert_eq!(report.stats.coverage, 0.0);
        assert_eq!(report.stats.ported_names, 0);
    }
}
