//! `groundgraph features` — P19 functional area map.

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::feature_map::{
    analyze_feature_map, FeatureCluster, FeatureMap, FeatureMapOptions,
};

use super::output::TextJsonFormat;

#[derive(Debug, Clone)]
pub struct FeaturesRunArgs {
    pub repo_root: PathBuf,
    pub max_clusters: usize,
    pub max_propagation_depth: usize,
    pub min_cluster_size: usize,
    pub format: TextJsonFormat,
}

pub fn run(args: FeaturesRunArgs) -> Result<()> {
    let report = analyze_feature_map(FeatureMapOptions {
        repo_root: args.repo_root,
        max_clusters: args.max_clusters,
        max_propagation_depth: args.max_propagation_depth,
        min_cluster_size: args.min_cluster_size,
    })
    .context("building feature map")?;
    match args.format {
        TextJsonFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialising feature map")?
        ),
        TextJsonFormat::Text => print_text(&report),
    }
    Ok(())
}

fn print_text(report: &FeatureMap) {
    println!("GroundGraph features (P19 · 功能区聚类)");
    println!(
        "种子 {} 个 · 输出簇 {} · 已归属节点 {} · 未归属代码节点 {}",
        report.stats.seeds_considered,
        report.stats.clusters_reported,
        report.stats.nodes_assigned,
        report.stats.nodes_unassigned,
    );
    println!();
    if report.clusters.is_empty() {
        println!("(代码图太稀疏 — 提高索引完整度后再试，或调小 --min-cluster-size)");
        return;
    }
    for (i, cluster) in report.clusters.iter().enumerate() {
        print_cluster(i, cluster);
    }
    println!();
    println!(
        "提示: 这个聚类是『启发式』结果 — 不要把它当作权威功能划分。新增 LSP / 框架事实后聚类质量会显著提升。"
    );
}

fn print_cluster(i: usize, c: &FeatureCluster) {
    let roles = if c.roles.is_empty() {
        String::from("(no framework hints)")
    } else {
        c.roles.join(", ")
    };
    println!(
        "== 簇 #{} · {} · {} 个节点 · 种子分 {} · roles: {} ==",
        i + 1,
        c.name,
        c.node_count,
        c.seed_score,
        roles,
    );
    println!("    seed:  {}", c.seed_path);
    println!("    id:    {}", c.id);
    if c.representative_symbols.is_empty() {
        println!("    (no representative symbols)");
    } else {
        println!("    成员（按距种子距离排序）:");
        for m in &c.representative_symbols {
            println!(
                "      d={d}  {kind:18} {label}\n        path: {path}",
                d = m.distance_from_seed,
                kind = m.kind,
                label = m.label,
                path = m.path,
            );
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use groundgraph_engine::feature_map::{
        FeatureCluster, FeatureClusterMember, FeatureMap, FeatureMapStats,
        FEATURE_MAP_SCHEMA_VERSION,
    };

    #[test]
    fn feature_map_serialises_clusters_with_roles_and_members() {
        let report = FeatureMap {
            schema_version: FEATURE_MAP_SCHEMA_VERSION,
            stats: FeatureMapStats {
                seeds_considered: 5,
                clusters_reported: 1,
                nodes_assigned: 12,
                nodes_unassigned: 4,
            },
            clusters: vec![FeatureCluster {
                id: "feature::python_module::auth".into(),
                name: "app · auth".into(),
                seed_path: "backend/app/auth.py".into(),
                seed_score: 25,
                representative_symbols: vec![FeatureClusterMember {
                    id: "python::backend/app/auth.py::login".into(),
                    kind: "python_function".into(),
                    label: "login".into(),
                    path: "backend/app/auth.py".into(),
                    distance_from_seed: 1,
                }],
                node_count: 12,
                roles: vec!["fastapi_route".into()],
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"name\":\"app · auth\""));
        assert!(json.contains("\"roles\":[\"fastapi_route\"]"));
        assert!(json.contains("\"distance_from_seed\":1"));
    }
}
