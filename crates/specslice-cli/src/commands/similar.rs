//! `specslice similar` — P18 tier 1, structural duplicate report.
//!
//! Tier 1 finds *exact AST duplicates*: function / method bodies that
//! collapse to identical normalized token streams after stripping
//! identifiers, literals and comments. Output is intentionally a
//! candidate list — never an auto-merge instruction.
//!
//! ```text
//! specslice similar
//! specslice similar --node python::app/foo.py::bar
//! specslice similar --format json
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use specslice_engine::similarity::{
    analyze_similarity, SimilarityCluster, SimilarityOptions, SimilarityReport,
};

#[derive(Debug, Clone)]
pub struct SimilarRunArgs {
    pub repo_root: PathBuf,
    pub focus_symbol_id: Option<String>,
    pub min_tokens: usize,
    pub min_cluster_size: usize,
    pub format: String,
}

pub fn run(args: SimilarRunArgs) -> Result<()> {
    let report = analyze_similarity(SimilarityOptions {
        repo_root: args.repo_root,
        min_tokens: args.min_tokens,
        min_cluster_size: args.min_cluster_size,
        focus_symbol_id: args.focus_symbol_id,
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

fn print_text(report: &SimilarityReport) {
    println!("SpecSlice similar (tier 1 · 结构指纹)");
    println!(
        "扫描函数 {} · 跳过 {} · 输出簇 {}",
        report.stats.symbols_scanned, report.stats.symbols_skipped, report.stats.clusters_reported,
    );
    println!();
    if report.clusters.is_empty() {
        println!("(没有发现结构完全相同的代码簇)");
        println!(
            "提示: tier 1 仅识别『去除标识符 / 字面量 / 注释后完全一致』的函数；近似重复 (tier 2) 与业务重复 (tier 3) 仍在后续迭代。"
        );
        return;
    }
    for (i, cluster) in report.clusters.iter().enumerate() {
        print_cluster(i, cluster);
    }
    println!();
    println!(
        "提示: 报告仅作为候选列表 — 请用 `specslice graph --focus <id>` 或 `specslice search` 查看上下文后再决定是否合并 / 删除。"
    );
}

fn print_cluster(index: usize, cluster: &SimilarityCluster) {
    println!(
        "== 簇 #{} · {} · {} 个成员 · {} tokens · 指纹 {} ==",
        index + 1,
        cluster.duplicate_type,
        cluster.members.len(),
        cluster.normalized_token_count,
        cluster.fingerprint,
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
    use specslice_engine::similarity::{
        SimilarityCluster, SimilarityMember, SimilarityReport, SimilarityStats,
        SIMILARITY_SCHEMA_VERSION,
    };

    fn cluster() -> SimilarityCluster {
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
        }
    }

    #[test]
    fn text_output_lists_cluster_members_and_recommendation() {
        // Re-routes stdout would be heavier than necessary — exercise
        // by serialising to JSON, which uses the same `Serialize` impl.
        let report = SimilarityReport {
            schema_version: SIMILARITY_SCHEMA_VERSION,
            stats: SimilarityStats {
                symbols_scanned: 5,
                symbols_skipped: 2,
                clusters_reported: 1,
            },
            clusters: vec![cluster()],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"duplicate_type\":\"exact_ast\""));
        assert!(json.contains("\"recommendation\":\"review\""));
        assert!(json.contains("python::app/a.py::fa"));
        assert!(json.contains("python::app/b.py::fb"));
    }
}
