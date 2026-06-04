//! `specslice business-doc` — render the human-confirmed business
//! candidates into a reader-facing business document.
//!
//! This is the back half of "build business documentation from code":
//! `propose` produced the evidence pack, an AI turned it into
//! `business_logic.yaml`, a human accepted the good claims, and this
//! command renders those accepted claims — with their real code/doc/test
//! evidence resolved from the graph — into Markdown (or JSON / text).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_engine::business_doc::{
    build_business_doc, BusinessDoc, BusinessDocEntry, BusinessDocOptions, DocEvidence,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusinessDocFormat {
    Markdown,
    Json,
    Text,
}

#[derive(Debug, Clone)]
pub struct BusinessDocRunArgs {
    pub repo_root: PathBuf,
    pub format: BusinessDocFormat,
    pub out: Option<PathBuf>,
    pub include_proposed: bool,
    pub include_rejected: bool,
    pub pretty: bool,
}

pub fn run(args: BusinessDocRunArgs) -> Result<()> {
    let doc = build_business_doc(BusinessDocOptions {
        repo_root: args.repo_root.clone(),
        include_proposed: args.include_proposed,
        include_rejected: args.include_rejected,
    })
    .context("building business document")?;

    for w in &doc.warnings {
        eprintln!("specslice: 警告：{w}");
    }

    let rendered = match args.format {
        BusinessDocFormat::Json => {
            if args.pretty {
                serde_json::to_string_pretty(&doc).context("serialising doc to JSON")?
            } else {
                serde_json::to_string(&doc).context("serialising doc to JSON")?
            }
        }
        BusinessDocFormat::Markdown => render_markdown(&doc),
        BusinessDocFormat::Text => render_text(&doc),
    };

    match args.out.as_deref() {
        Some(path) => {
            write_to(path, &rendered)?;
            eprintln!("已写入业务文档: {}", path.display());
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

fn write_to(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent of {}", path.display()))?;
        }
    }
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn status_cn(key: &str) -> &'static str {
    match key {
        "accepted" => "已确认",
        "proposed" => "AI 提议（未确认）",
        "needs_changes" => "需修改",
        "pending" => "待定",
        "rejected" => "已拒绝",
        _ => "未知",
    }
}

fn confidence_str(c: Option<f32>) -> String {
    c.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".into())
}

// ---------------------------------------------------------------------------
// Markdown — the deliverable business document.
// ---------------------------------------------------------------------------

fn render_markdown(doc: &BusinessDoc) -> String {
    let mut s = String::new();
    s.push_str("# 业务逻辑文档（SpecSlice）\n\n");
    s.push_str("> 由 `specslice business-doc` 基于**已确认**的业务候选（`.specslice/candidates/business_logic.yaml`）生成。\n");
    s.push_str(
        "> 每条业务能力都引用代码图中的真实证据（代码 / 文档 / 测试 / 框架信号），可逐条审计。\n\n",
    );
    let st = &doc.stats;
    s.push_str(&format!(
        "> 已确认 {} · 草稿(未确认) {} · 已拒绝 {} · 证据漂移 {}（候选共 {}）\n\n",
        st.accepted,
        st.proposed + st.needs_changes + st.pending,
        st.rejected,
        st.unresolved_evidence,
        st.total_candidates,
    ));

    if doc.entries.is_empty() {
        s.push_str("## 尚无可导出的业务能力\n\n");
        s.push_str("当前没有**已确认**的业务候选。请先：\n\n");
        s.push_str("1. `specslice propose --format markdown --out .specslice/export/business-pack.md` 生成证据包；\n");
        s.push_str("2. 交给 AI 产出 `.specslice/candidates/business_logic.yaml`；\n");
        s.push_str("3. `specslice candidate review` 确认；\n");
        s.push_str("4. 重新运行本命令。或加 `--include-proposed` 预览未确认草稿。\n");
        return s;
    }

    // overview table
    s.push_str("## 能力总览\n\n");
    s.push_str("| 业务能力 | id | 置信度 | 状态 | 代码 | 文档 | 测试 | 信号 |\n");
    s.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for e in &doc.entries {
        s.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} | {} |\n",
            escape_pipe(&e.name),
            e.id,
            confidence_str(e.confidence),
            status_cn(&e.status),
            e.code_evidence.len(),
            e.doc_evidence.len(),
            e.test_evidence.len(),
            e.signal_evidence.len(),
        ));
    }
    s.push('\n');

    // per-capability sections
    for e in &doc.entries {
        render_entry_md(&mut s, e);
    }
    s
}

fn render_entry_md(s: &mut String, e: &BusinessDocEntry) {
    s.push_str(&format!("## {}\n\n", e.name));
    let mut meta = format!(
        "**状态**: {} · **置信度**: {} · `{}`",
        status_cn(&e.status),
        confidence_str(e.confidence),
        e.id
    );
    if let Some(reviewer) = &e.reviewer {
        meta.push_str(&format!(" · 审阅: {reviewer}"));
        if let Some(at) = &e.reviewed_at {
            meta.push_str(&format!(" @ {at}"));
        }
    }
    s.push_str(&meta);
    s.push_str("\n\n");

    if !e.description.is_empty() {
        s.push_str(e.description.trim());
        s.push_str("\n\n");
    }

    if let Some(note) = &e.review_note {
        if !note.trim().is_empty() {
            s.push_str(&format!("> 审阅备注：{}\n\n", note.trim()));
        }
    }

    s.push_str("**业务证据**\n\n");
    render_evidence_group(s, "代码", &e.code_evidence);
    render_evidence_group(s, "文档", &e.doc_evidence);
    render_evidence_group(s, "测试", &e.test_evidence);
    render_evidence_group(s, "信号", &e.signal_evidence);
    if e.code_evidence.is_empty()
        && e.doc_evidence.is_empty()
        && e.test_evidence.is_empty()
        && e.signal_evidence.is_empty()
    {
        s.push_str("- （无解析到的证据）\n");
    }
    s.push('\n');

    if !e.open_questions.is_empty() {
        s.push_str("**开放问题**\n\n");
        for q in &e.open_questions {
            s.push_str(&format!("- {q}\n"));
        }
        s.push('\n');
    }
    if !e.risks.is_empty() {
        s.push_str("**风险**\n\n");
        for r in &e.risks {
            s.push_str(&format!("- {r}\n"));
        }
        s.push('\n');
    }
    if let Some(rec) = &e.recommendation {
        if !rec.trim().is_empty() {
            s.push_str(&format!("**建议**: {}\n\n", rec.trim()));
        }
    }
    if !e.unresolved_evidence.is_empty() {
        s.push_str(
            "> ⚠ **证据漂移**：以下被引用的符号已不在代码图中（可能已重命名/删除），请复核：\n>\n",
        );
        for id in &e.unresolved_evidence {
            s.push_str(&format!("> - `{id}`\n"));
        }
        s.push('\n');
    }
}

fn render_evidence_group(s: &mut String, label: &str, items: &[DocEvidence]) {
    if items.is_empty() {
        return;
    }
    s.push_str(&format!("- {label}:\n"));
    for it in items {
        s.push_str(&format!("  - {}\n", evidence_line(it)));
    }
}

fn evidence_line(it: &DocEvidence) -> String {
    let loc = match (&it.path, it.line_range) {
        (Some(p), Some((a, b))) => format!(" — `{p}:{a}-{b}`"),
        (Some(p), None) => format!(" — `{p}`"),
        (None, _) => String::new(),
    };
    format!("`{}` ({}){}", it.name, it.kind, loc)
}

// ---------------------------------------------------------------------------
// Text — terse human summary.
// ---------------------------------------------------------------------------

fn render_text(doc: &BusinessDoc) -> String {
    let mut s = String::new();
    s.push_str("SpecSlice 业务文档\n");
    let st = &doc.stats;
    s.push_str(&format!(
        "已确认 {} · 草稿 {} · 漂移 {} · 导出 {}（候选共 {}）\n\n",
        st.accepted,
        st.proposed + st.needs_changes + st.pending,
        st.unresolved_evidence,
        st.included,
        st.total_candidates,
    ));
    if doc.entries.is_empty() {
        s.push_str(
            "（无已确认业务能力；先 `specslice candidate review`，或加 --include-proposed 预览）\n",
        );
        return s;
    }
    for e in &doc.entries {
        s.push_str(&format!(
            "== {} (`{}`) · {} · 置信度 {} ==\n",
            e.name,
            e.id,
            status_cn(&e.status),
            confidence_str(e.confidence),
        ));
        s.push_str(&format!(
            "   证据: 代码 {} · 文档 {} · 测试 {} · 信号 {}{}\n",
            e.code_evidence.len(),
            e.doc_evidence.len(),
            e.test_evidence.len(),
            e.signal_evidence.len(),
            if e.unresolved_evidence.is_empty() {
                String::new()
            } else {
                format!(" · 漂移 {}", e.unresolved_evidence.len())
            },
        ));
    }
    s
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_engine::business_doc::{
        BusinessDoc, BusinessDocStats, BUSINESS_DOC_SCHEMA_VERSION,
    };

    fn sample_doc() -> BusinessDoc {
        BusinessDoc {
            schema_version: BUSINESS_DOC_SCHEMA_VERSION,
            repo_root: ".".into(),
            stats: BusinessDocStats {
                total_candidates: 2,
                included: 1,
                accepted: 1,
                proposed: 1,
                needs_changes: 0,
                pending: 0,
                rejected: 0,
                unresolved_evidence: 1,
            },
            entries: vec![BusinessDocEntry {
                id: "cart".into(),
                name: "购物车下单".into(),
                description: "用户把商品加入购物车并提交订单。".into(),
                status: "accepted".into(),
                confidence: Some(0.82),
                recommendation: Some("建议接受".into()),
                reviewer: Some("qjs".into()),
                reviewed_at: Some("2026-06-04T00:00:00Z".into()),
                review_note: Some("与产品意图一致".into()),
                code_evidence: vec![DocEvidence {
                    id: "dart_class::lib/features/cart/cart_bloc.dart#CartBloc".into(),
                    kind: "dart_class".into(),
                    name: "CartBloc".into(),
                    path: Some("lib/features/cart/cart_bloc.dart".into()),
                    line_range: Some((10, 42)),
                }],
                doc_evidence: vec![],
                test_evidence: vec![],
                signal_evidence: vec![DocEvidence {
                    id: "route::/cart".into(),
                    kind: "route".into(),
                    name: "/cart".into(),
                    path: None,
                    line_range: None,
                }],
                unresolved_evidence: vec!["dart_class::lib/gone.dart#Gone".into()],
                open_questions: vec!["库存校验是否在服务端？".into()],
                risks: vec!["缺少并发下单测试".into()],
            }],
            warnings: vec![],
        }
    }

    #[test]
    fn markdown_renders_overview_evidence_and_drift() {
        let md = render_markdown(&sample_doc());
        assert!(md.contains("# 业务逻辑文档"));
        assert!(md.contains("## 能力总览"));
        assert!(md.contains("## 购物车下单"));
        assert!(md.contains("**状态**: 已确认"));
        assert!(md.contains("用户把商品加入购物车并提交订单。"));
        assert!(
            md.contains("`lib/features/cart/cart_bloc.dart:10-42`"),
            "code evidence cites path + line range"
        );
        assert!(md.contains("证据漂移"), "drift section present");
        assert!(
            md.contains("库存校验是否在服务端？"),
            "open question rendered"
        );
        assert!(md.contains("缺少并发下单测试"), "risk rendered");
    }

    #[test]
    fn markdown_empty_gives_actionable_guidance() {
        let mut doc = sample_doc();
        doc.entries.clear();
        let md = render_markdown(&doc);
        assert!(md.contains("尚无可导出的业务能力"));
        assert!(md.contains("--include-proposed"));
    }

    #[test]
    fn text_summary_lists_capabilities() {
        let txt = render_text(&sample_doc());
        assert!(txt.contains("购物车下单 (`cart`)"));
        assert!(txt.contains("已确认"));
        assert!(txt.contains("漂移 1"));
    }
}
