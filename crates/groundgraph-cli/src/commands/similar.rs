//! `groundgraph similar` — P18 tier 1 + tier 2 duplicate report.
//!
//! Tier 1 (`exact_ast`): function / method bodies that collapse to
//! identical normalized token streams after stripping identifiers,
//! literals and comments.
//!
//! Tier 2 (`near_token`, SimHash): pairs whose SimHash over k-shingles
//! has small Hamming distance — catches "copy + rename a few fields,
//! add or remove a couple of statements".
//!
//! Output is always a candidate list — never an auto-merge instruction.
//!
//! ```text
//! groundgraph similar
//! groundgraph similar --mode exact
//! groundgraph similar --mode near --min-score 0.8
//! groundgraph similar --node python::app/foo.py::bar
//! groundgraph similar --format json
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use groundgraph_engine::similarity::{
    analyze_similarity, SimilarityCluster, SimilarityMode, SimilarityOptions, SimilarityReport,
};

#[derive(Debug, Clone)]
pub struct SimilarRunArgs {
    pub repo_root: PathBuf,
    pub focus_symbol_id: Option<String>,
    pub min_tokens: usize,
    pub min_cluster_size: usize,
    pub mode: String,
    pub min_similarity: f32,
    pub shingle_k: usize,
    pub max_pairwise: usize,
    pub format: String,
}

pub fn run(args: SimilarRunArgs) -> Result<()> {
    let mode = parse_mode(&args.mode)?;
    let report = analyze_similarity(SimilarityOptions {
        repo_root: args.repo_root,
        min_tokens: args.min_tokens,
        min_cluster_size: args.min_cluster_size,
        focus_symbol_id: args.focus_symbol_id,
        mode,
        min_similarity: args.min_similarity,
        shingle_k: args.shingle_k,
        max_pairwise_symbols: args.max_pairwise,
    })
    .context("running similarity analysis")?;
    match args.format.as_str() {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).context("serialising similarity report")?
            );
        }
        "text" | "" => print_text(&report),
        other => anyhow::bail!("unsupported --format `{other}` (expected text|json)"),
    }
    Ok(())
}

fn parse_mode(raw: &str) -> Result<SimilarityMode> {
    match raw {
        "exact" => Ok(SimilarityMode::Exact),
        "near" => Ok(SimilarityMode::Near),
        "all" | "" => Ok(SimilarityMode::All),
        other => anyhow::bail!("unsupported --mode `{other}` (expected exact|near|all)"),
    }
}

fn print_text(report: &SimilarityReport) {
    println!("GroundGraph similar (tier 1 结构指纹 + tier 2 SimHash 近似)");
    println!(
        "扫描函数 {} · 跳过 {} · 输出簇 {} (exact {} · near {})",
        report.stats.symbols_scanned,
        report.stats.symbols_skipped,
        report.stats.clusters_reported,
        report.stats.exact_clusters,
        report.stats.near_clusters,
    );
    if report.stats.near_pairwise_skipped {
        println!(
            "⚠ near tier 已跳过 (uncovered 符号超过 --max-pairwise 上限)。请缩小 code-roots 或显式提高上限后重试。"
        );
    }
    println!();
    if report.clusters.is_empty() {
        println!("(没有发现满足阈值的相似代码簇)");
        println!(
            "提示: tier 3 业务重复 (graph 邻域) 仍在后续迭代；先用 `groundgraph search` 与 `groundgraph graph --focus <id>` 复核单点上下文。"
        );
        return;
    }
    for (i, cluster) in report.clusters.iter().enumerate() {
        print_cluster(i, cluster);
    }
    println!();
    println!(
        "提示: 报告仅作为候选列表 — 请用 `groundgraph graph --focus <id>` 或 `groundgraph search` 查看上下文后再决定是否合并 / 删除。"
    );
}

fn print_cluster(index: usize, cluster: &SimilarityCluster) {
    let score_suffix = match cluster.similarity_score {
        Some(s) => format!(" · 相似度 {:.2}", s),
        None => String::new(),
    };
    println!(
        "== 簇 #{} · {} · {} 个成员 · {} tokens · 指纹 {}{} ==",
        index + 1,
        cluster.duplicate_type,
        cluster.members.len(),
        cluster.normalized_token_count,
        cluster.fingerprint,
        score_suffix,
    );
    println!("建议: {}", cluster.recommendation);
    for member in &cluster.members {
        let range = member
            .line_range
            .map(|(s, e)| format!(":{s}-{e}"))
            .unwrap_or_default();
        println!(
            "  - {label}  ({kind})\n      id:   {id}\n      路径: {path}{range}",
            label = member.label,
            kind = member.kind,
            id = member.id,
            path = member.path,
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use groundgraph_engine::similarity::{
        SimilarityCluster, SimilarityMember, SimilarityReport, SimilarityStats,
        SIMILARITY_SCHEMA_VERSION,
    };

    fn exact_cluster() -> SimilarityCluster {
        SimilarityCluster {
            fingerprint: "deadbeefcafebabe".into(),
            duplicate_type: "exact_ast".into(),
            members: vec![
                SimilarityMember {
                    id: "python::app/a.py::fa".into(),
                    kind: "python_function".into(),
                    label: "fa".into(),
                    path: "app/a.py".into(),
                    line_range: Some((1, 5)),
                },
                SimilarityMember {
                    id: "python::app/b.py::fb".into(),
                    kind: "python_function".into(),
                    label: "fb".into(),
                    path: "app/b.py".into(),
                    line_range: Some((1, 5)),
                },
            ],
            normalized_token_count: 24,
            recommendation: "review".into(),
            similarity_score: None,
        }
    }

    fn near_cluster() -> SimilarityCluster {
        SimilarityCluster {
            fingerprint: "1234567812345678".into(),
            duplicate_type: "near_token".into(),
            members: vec![
                SimilarityMember {
                    id: "python::app/c.py::fc".into(),
                    kind: "python_function".into(),
                    label: "fc".into(),
                    path: "app/c.py".into(),
                    line_range: Some((1, 8)),
                },
                SimilarityMember {
                    id: "python::app/d.py::fd".into(),
                    kind: "python_function".into(),
                    label: "fd".into(),
                    path: "app/d.py".into(),
                    line_range: Some((1, 9)),
                },
            ],
            normalized_token_count: 40,
            recommendation: "review".into(),
            similarity_score: Some(0.875),
        }
    }

    #[test]
    fn report_serialisation_distinguishes_exact_and_near_clusters() {
        let report = SimilarityReport {
            schema_version: SIMILARITY_SCHEMA_VERSION,
            stats: SimilarityStats {
                symbols_scanned: 8,
                symbols_skipped: 2,
                clusters_reported: 2,
                exact_clusters: 1,
                near_clusters: 1,
                near_pairwise_skipped: false,
            },
            clusters: vec![exact_cluster(), near_cluster()],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"duplicate_type\":\"exact_ast\""));
        assert!(json.contains("\"duplicate_type\":\"near_token\""));
        // Exact cluster omits similarity_score; near cluster carries it.
        assert!(!json.contains("\"similarity_score\":null"));
        assert!(json.contains("\"similarity_score\":0.875"));
        assert!(json.contains("\"exact_clusters\":1"));
        assert!(json.contains("\"near_clusters\":1"));
    }
}
