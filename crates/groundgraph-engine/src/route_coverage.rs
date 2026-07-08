//! P26 — HTTP route porting coverage (port-coverage's sibling for the API
//! surface).
//!
//! `port_coverage` answers "which *symbols* have a counterpart in the
//! rewrite". For a client/server rewrite the other half of "no omissions" is
//! the **wire surface**: of every backend route the client actually consumes,
//! how many does the rewritten server already serve, and which are still
//! to-do? Both graphs already carry `http_route` nodes (the client graph from
//! Dart/JS API-constant mining, the server graph from route definitions), so
//! this module diffs the two by **normalized route path** and produces the
//! same ported / missing / extra ledger, plus a per-service breakdown.
//!
//! Matching is by a normalized **suffix key** rather than the verbatim path,
//! because the two sides view the same endpoint through different prefixes: a
//! client calls through an API gateway (`/style/app/style-info/getStyleHome`)
//! while the rewritten monolith mounts the handler under its own prefix
//! (`/app/style-info/getStyleHome`, or the bare `/style-info/getStyleHome`).
//! The stable identity is the trailing `controller/action` pair, so the key is
//! the last `suffix_segments` path segments (default 2), lowercased, with
//! `{param}` / `:param` / `*` placeholder segments dropped. The full source
//! path is always kept in the report so the to-do list stays actionable, and
//! `suffix_segments` is tunable when the default over- or under-matches.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use groundgraph_core::{Node, NodeKind};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};

pub const ROUTE_COVERAGE_SCHEMA_VERSION: u32 = 1;

/// Default number of trailing path segments used as the cross-graph match key.
/// 2 = `controller/action`, which survives gateway-prefix variance without
/// collapsing distinct endpoints down to a bare (collision-prone) action name.
pub const DEFAULT_SUFFIX_SEGMENTS: usize = 2;

// ---------------------------------------------------------------------------
// Data contract
// ---------------------------------------------------------------------------

/// A consumed route that has no server counterpart (the porting to-do list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRef {
    /// Full route path as seen on its own side (client gateway path, or the
    /// server mount path for `extra`).
    pub path: String,
    /// The normalized suffix key used for matching.
    pub key: String,
    /// Action name (route node leaf name), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A consumed route that the rewritten server already serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortedRoute {
    pub path: String,
    pub key: String,
    /// Server route path(s) that matched this key (sorted, de-duplicated).
    pub targets: Vec<String>,
}

/// Per-service (first path segment) coverage, least-covered first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCoverage {
    pub service: String,
    pub total: usize,
    pub ported: usize,
    pub coverage: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCoverageStats {
    /// Distinct consumed (source) route paths.
    pub source_routes: usize,
    /// Distinct consumed-route match keys (endpoints).
    pub source_distinct_keys: usize,
    /// Distinct served (target) route paths.
    pub target_routes: usize,
    /// Distinct served-route match keys.
    pub target_distinct_keys: usize,
    /// Source keys with a served counterpart.
    pub ported_keys: usize,
    /// Source keys with no served counterpart.
    pub missing_keys: usize,
    /// Served keys with no consumer (server-only).
    pub extra_keys: usize,
    /// `ported_keys / source_distinct_keys` (0.0 when no source routes).
    pub coverage: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCoverageReport {
    pub schema_version: u32,
    /// Suffix length used for the match key (echoed for reproducibility).
    pub suffix_segments: usize,
    pub stats: RouteCoverageStats,
    pub missing: Vec<RouteRef>,
    pub ported: Vec<PortedRoute>,
    pub by_service: Vec<ServiceCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<RouteRef>,
}

#[derive(Debug, Clone)]
pub struct RouteCoverageOptions {
    /// Consumer graph (client): the routes that *must* be served.
    pub source_db: PathBuf,
    /// Server graph (rewrite): the routes actually served.
    pub target_db: PathBuf,
    /// Trailing path segments used as the match key (0 = whole path).
    pub suffix_segments: usize,
    /// Populate `extra` with server-only routes (default `false`).
    pub include_extra: bool,
    /// Route-path globs to drop on both sides (e.g. `/token/**`).
    pub exclude: Vec<String>,
    /// Cap `missing` / `ported` / `extra` list lengths (0 = unlimited).
    pub max_items: usize,
}

impl Default for RouteCoverageOptions {
    fn default() -> Self {
        Self {
            source_db: PathBuf::new(),
            target_db: PathBuf::new(),
            suffix_segments: DEFAULT_SUFFIX_SEGMENTS,
            include_extra: false,
            exclude: Vec::new(),
            max_items: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn analyze_route_coverage(options: RouteCoverageOptions) -> Result<RouteCoverageReport> {
    let source = Store::open(&options.source_db)
        .with_context(|| format!("opening source graph at {}", options.source_db.display()))?;
    let target = Store::open(&options.target_db)
        .with_context(|| format!("opening target graph at {}", options.target_db.display()))?;
    analyze_route_coverage_with_stores(&source, &target, &options)
}

pub fn analyze_route_coverage_with_stores(
    source: &Store,
    target: &Store,
    options: &RouteCoverageOptions,
) -> Result<RouteCoverageReport> {
    let exclude =
        build_globset(&options.exclude).context("compiling route-coverage exclude globs")?;
    let suffix = options.suffix_segments;

    let route_paths = |store: &Store| -> Result<Vec<(String, Option<String>)>> {
        let mut out: Vec<(String, Option<String>)> = store
            .list_all_nodes()?
            .into_iter()
            .filter(|n| n.kind == NodeKind::HttpRoute)
            .filter_map(|n| route_path(&n).map(|p| (p, n.name.clone())))
            .filter(|(p, _)| !exclude.is_match(p))
            .collect();
        // Stable, de-duplicated by full path.
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        Ok(out)
    };

    let source_routes = route_paths(source)?;
    let target_routes = route_paths(target)?;

    // target key -> set of server paths.
    let mut target_by_key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, _name) in &target_routes {
        target_by_key
            .entry(route_key(path, suffix))
            .or_default()
            .insert(path.clone());
    }

    let mut source_keys: BTreeSet<String> = BTreeSet::new();
    let mut ported_keys: BTreeSet<String> = BTreeSet::new();
    let mut emitted_ported: BTreeSet<String> = BTreeSet::new();
    let mut emitted_missing: BTreeSet<String> = BTreeSet::new();
    let mut ported: Vec<PortedRoute> = Vec::new();
    let mut missing: Vec<RouteRef> = Vec::new();
    // service -> (total source routes, ported source routes).
    let mut service_totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for (path, name) in &source_routes {
        let key = route_key(path, suffix);
        source_keys.insert(key.clone());
        let matched = target_by_key.get(&key);
        let is_ported = matched.is_some();

        let service = route_service(path);
        let entry = service_totals.entry(service).or_insert((0, 0));
        entry.0 += 1;
        if is_ported {
            entry.1 += 1;
        }

        match matched {
            Some(targets) => {
                ported_keys.insert(key.clone());
                if emitted_ported.insert(key.clone()) {
                    ported.push(PortedRoute {
                        path: path.clone(),
                        key,
                        targets: targets.iter().cloned().collect(),
                    });
                }
            }
            None => {
                if emitted_missing.insert(key.clone()) {
                    missing.push(RouteRef {
                        path: path.clone(),
                        key,
                        name: name.clone(),
                    });
                }
            }
        }
    }

    let mut by_service: Vec<ServiceCoverage> = service_totals
        .into_iter()
        .map(|(service, (total, ported))| ServiceCoverage {
            service,
            total,
            ported,
            coverage: ratio(ported, total),
        })
        .collect();
    // Least-covered first, then larger services, then name — that's the work queue.
    by_service.sort_by(|a, b| {
        a.coverage
            .partial_cmp(&b.coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.total.cmp(&a.total))
            .then(a.service.cmp(&b.service))
    });

    let extra: Vec<RouteRef> = if options.include_extra {
        target_by_key
            .iter()
            .filter(|(key, _)| !source_keys.contains(*key))
            .map(|(key, paths)| RouteRef {
                path: paths.iter().next().cloned().unwrap_or_default(),
                key: key.clone(),
                name: None,
            })
            .collect()
    } else {
        Vec::new()
    };

    let stats = RouteCoverageStats {
        source_routes: source_routes.len(),
        source_distinct_keys: source_keys.len(),
        target_routes: target_routes.len(),
        target_distinct_keys: target_by_key.len(),
        ported_keys: ported_keys.len(),
        missing_keys: source_keys.len() - ported_keys.len(),
        extra_keys: extra.len(),
        coverage: ratio(ported_keys.len(), source_keys.len()),
    };

    if options.max_items > 0 {
        missing.truncate(options.max_items);
        ported.truncate(options.max_items);
    }
    let extra = if options.max_items > 0 && extra.len() > options.max_items {
        extra[..options.max_items].to_vec()
    } else {
        extra
    };

    Ok(RouteCoverageReport {
        schema_version: ROUTE_COVERAGE_SCHEMA_VERSION,
        suffix_segments: suffix,
        stats,
        missing,
        ported,
        by_service,
        extra,
    })
}

// ---------------------------------------------------------------------------
// Path normalization
// ---------------------------------------------------------------------------

/// The route URL of an `http_route` node. Route nodes carry the URL in `path`;
/// fall back to `name` if a producer only set the leaf.
fn route_path(n: &Node) -> Option<String> {
    let raw = n
        .path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .or(n.name.as_deref())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Whether a path segment is a placeholder (path parameter / wildcard) that
/// must not participate in matching: `{id}`, `:id`, `*`, `<id>` / `<int:id>`,
/// and raw template interpolation (`${id}`, `$id`) that escaped upstream
/// normalization (issues2.md #31/#44).
fn is_param_segment(seg: &str) -> bool {
    (seg.starts_with('{') && seg.ends_with('}'))
        || (seg.starts_with('<') && seg.ends_with('>'))
        || seg.starts_with(':')
        || seg.starts_with('*')
        || seg.starts_with('$')
}

/// Concrete (non-placeholder) path segments, lowercased.
fn concrete_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty() && !is_param_segment(s))
        .map(|s| s.to_lowercase())
        .collect()
}

/// Normalized match key: the last `suffix` concrete segments (all of them when
/// `suffix == 0` or the path is shorter), joined by `/`.
///
/// # Warning
/// `suffix == 1` keys on the action segment alone, so same-named actions under
/// different controllers (`/foo/bar/select` vs `/craft/craftMandatory/select`)
/// collapse to the same key and are treated as the same route. The default is
/// `2` for exactly this reason; pass `1` only when matching on action name
/// across a controller rename is the explicit intent (#93).
pub fn route_key(path: &str, suffix: usize) -> String {
    let segs = concrete_segments(path);
    if segs.is_empty() {
        return String::new();
    }
    let start = if suffix == 0 || suffix >= segs.len() {
        0
    } else {
        segs.len() - suffix
    };
    segs[start..].join("/")
}

/// The service a route belongs to — its first concrete segment (the gateway /
/// microservice name on the client side). Empty when the path has none.
fn route_service(path: &str) -> String {
    concrete_segments(path)
        .into_iter()
        .next()
        .unwrap_or_default()
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
        part as f32 / whole as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundgraph_core::ArtifactId;

    fn store_with(routes: &[(&str, &str)]) -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        for (i, (name, path)) in routes.iter().enumerate() {
            let mut n = Node::new(
                ArtifactId::new(format!("route::{path}#{i}")),
                NodeKind::HttpRoute,
            );
            n.name = Some((*name).to_string());
            n.path = Some((*path).to_string());
            store.upsert_node(&n).unwrap();
        }
        (store, dir)
    }

    #[test]
    fn route_key_uses_last_two_segments_and_strips_params() {
        // Gateway prefix differs; controller/action suffix is the identity.
        assert_eq!(
            route_key("/style/app/style-info/getStyleHome", 2),
            "style-info/getstylehome"
        );
        assert_eq!(
            route_key("/app/style-info/getStyleHome", 2),
            "style-info/getstylehome"
        );
        // Trailing path param is dropped before taking the suffix.
        assert_eq!(
            route_key("/craft/craft-conflict/selectConflictByCraftIds/{ids}", 2),
            "craft-conflict/selectconflictbycraftids"
        );
        // :param and <param> forms too.
        assert_eq!(route_key("/orders/:id/items", 2), "orders/items");
        // Template-literal interpolation that escaped normalization
        // (`${id}`, `$id`) must also drop out (issues2.md #31/#44).
        assert_eq!(route_key("/orders/${id}/items", 2), "orders/items");
        assert_eq!(route_key("/orders/$id/items", 2), "orders/items");
        // Flask/FastAPI typed converters: `<int:id>`.
        assert_eq!(route_key("/orders/<int:id>/items", 2), "orders/items");
        // suffix=0 keeps the whole concrete path.
        assert_eq!(route_key("/a/b/c", 0), "a/b/c", "suffix 0 => whole path");
        // Short path: fewer segments than the suffix → keep all.
        assert_eq!(route_key("/ping", 2), "ping");
    }

    #[test]
    fn aligns_consumed_routes_to_served_routes_by_suffix() {
        // Client consumes through a gateway prefix; server mounts under its own.
        let (client, _c) = store_with(&[
            ("getStyleHome", "/style/app/style-info/getStyleHome"),
            ("getAdjustmentInfo", "/craft/dict-basic/getAdjustmentInfo"),
            ("submitOrder", "/order/app/order/submitOrder"),
        ]);
        let (server, _s) = store_with(&[
            ("getStyleHome", "/app/style-info/getStyleHome"),
            ("getAdjustmentInfo", "/dict-basic/getAdjustmentInfo"),
            // server-only helper
            ("health", "/internal/health"),
        ]);

        let report = analyze_route_coverage_with_stores(
            &client,
            &server,
            &RouteCoverageOptions {
                include_extra: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(report.stats.source_routes, 3);
        assert_eq!(report.stats.ported_keys, 2);
        assert_eq!(report.stats.missing_keys, 1);
        assert!((report.stats.coverage - 2.0 / 3.0).abs() < 1e-6);

        let missing: Vec<&str> = report.missing.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(missing, vec!["/order/app/order/submitOrder"]);

        // ported entry carries the matched server path.
        let style = report
            .ported
            .iter()
            .find(|p| p.key == "style-info/getstylehome")
            .unwrap();
        assert_eq!(
            style.targets,
            vec!["/app/style-info/getStyleHome".to_string()]
        );

        // server-only route surfaces as extra.
        let extra: Vec<&str> = report.extra.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(extra, vec!["internal/health"]);
    }

    /// issues2.md #31/#44 epic, end to end: a Spring server declaring
    /// `{id}` params and a Dart client consuming with `${id}` interpolation
    /// are indexed into two separate stores by the real schema indexer and
    /// must align under route-coverage — the exact cross-graph link the
    /// route pipeline exists for.
    #[test]
    fn spring_server_and_dart_client_align_end_to_end() {
        let server_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            server_dir.path().join("OrderController.java"),
            r#"
@RestController
@RequestMapping("/orders")
public class OrderController {
    @GetMapping("/{id}/items")
    public List<Item> items(@PathVariable Long id) { return null; }
}
"#,
        )
        .unwrap();
        let mut server = Store::open(server_dir.path().join("graph.db")).unwrap();
        server.migrate().unwrap();
        let s_stats =
            crate::schema_indexer::index_schema_into(&mut server, server_dir.path()).unwrap();
        assert_eq!(s_stats.http_routes, 1, "server route indexed");

        let client_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            client_dir.path().join("api.dart"),
            r#"
class OrderApi {
  Future<void> items(int id) async {
    final r = await _dio.get<Map<String, dynamic>>('/orders/${id}/items');
  }
}
"#,
        )
        .unwrap();
        let mut client = Store::open(client_dir.path().join("graph.db")).unwrap();
        client.migrate().unwrap();
        let c_stats =
            crate::schema_indexer::index_schema_into(&mut client, client_dir.path()).unwrap();
        assert_eq!(c_stats.consumed_routes, 1, "consumed route indexed");

        let report =
            analyze_route_coverage_with_stores(&client, &server, &RouteCoverageOptions::default())
                .unwrap();
        assert_eq!(report.stats.source_routes, 1);
        assert_eq!(
            report.stats.ported_keys, 1,
            "client `${{id}}` and server `{{id}}` must meet at one key; missing: {:?}",
            report.missing
        );
        assert!(report.missing.is_empty());
    }

    #[test]
    fn by_service_orders_least_covered_first() {
        let (client, _c) = store_with(&[
            ("a", "/craft/x/a"),
            ("b", "/craft/x/b"),
            ("c", "/style/y/c"),
        ]);
        // Only craft/x/a is served → craft 1/2 (0.5), style 0/1 (0.0).
        let (server, _s) = store_with(&[("a", "/x/a")]);
        let report =
            analyze_route_coverage_with_stores(&client, &server, &RouteCoverageOptions::default())
                .unwrap();
        assert_eq!(report.by_service[0].service, "style");
        assert!((report.by_service[0].coverage - 0.0).abs() < 1e-6);
        assert_eq!(report.by_service[1].service, "craft");
        assert!((report.by_service[1].coverage - 0.5).abs() < 1e-6);
        assert_eq!(report.by_service[1].total, 2);
        assert_eq!(report.by_service[1].ported, 1);
    }

    #[test]
    fn exclude_glob_drops_routes_on_both_sides() {
        let (client, _c) = store_with(&[
            ("token", "/token/oauth/token"),
            ("home", "/member/member/homeInfo"),
        ]);
        let (server, _s) = store_with(&[("homeInfo", "/member/homeInfo")]);
        let report = analyze_route_coverage_with_stores(
            &client,
            &server,
            &RouteCoverageOptions {
                exclude: vec!["/token/**".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        // token route excluded → only member/homeInfo remains, and it is ported.
        assert_eq!(report.stats.source_routes, 1);
        assert_eq!(report.stats.ported_keys, 1);
        assert!(report.missing.is_empty(), "missing: {:?}", report.missing);
    }

    #[test]
    fn empty_source_is_zero_coverage_not_nan() {
        let (client, _c) = store_with(&[]);
        let (server, _s) = store_with(&[("x", "/a/x")]);
        let report =
            analyze_route_coverage_with_stores(&client, &server, &RouteCoverageOptions::default())
                .unwrap();
        assert_eq!(report.stats.coverage, 0.0);
        assert_eq!(report.stats.ported_keys, 0);
        assert_eq!(report.stats.source_routes, 0);
    }

    #[test]
    fn suffix_one_matches_on_action_only() {
        // With suffix=1 the controller is ignored; a bare action name matches.
        let (client, _c) = store_with(&[("select", "/craft/craftMandatory/select")]);
        let (server, _s) = store_with(&[("select", "/foo/bar/select")]);
        let report = analyze_route_coverage_with_stores(
            &client,
            &server,
            &RouteCoverageOptions {
                suffix_segments: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.stats.ported_keys, 1);
    }
}
