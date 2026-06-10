//! `specslice propose` — generate a per-business-module evidence pack
//! from the indexed graph, ready for an AI to turn into
//! `business_logic.yaml` candidates (P9 review loop).
//!
//! This is the productised front-half of "build business documentation
//! from code": fast, deterministic, non-invasive (read-only on the
//! graph). It replaces the manual graph trawling that `connect propose`
//! could not do at scale.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_engine::business_pack::{
    propose_business_pack, BusinessPack, BusinessPackOptions, ModuleDependency, ModuleEvidence,
};

/// Cap on edges drawn in the module-dependency flowchart. Real repos have
/// hundreds of cross-module calls; a business overview only needs the
/// strongest ones (the full list lives in each module's「依赖模块」line).
const MAX_MERMAID_EDGES: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposeFormat {
    Json,
    Markdown,
    Text,
}

#[derive(Debug, Clone)]
pub struct ProposeRunArgs {
    pub repo_root: PathBuf,
    pub format: ProposeFormat,
    pub out: Option<PathBuf>,
    pub pretty: bool,
    pub max_modules: usize,
    pub max_entry_points: usize,
}

pub fn run(args: ProposeRunArgs) -> Result<()> {
    let pack = propose_business_pack(BusinessPackOptions {
        repo_root: args.repo_root.clone(),
        max_modules: args.max_modules,
        max_entry_points: args.max_entry_points,
        max_signal_samples: 10,
    })
    .context("building business evidence pack")?;

    let rendered = match args.format {
        ProposeFormat::Json => {
            if args.pretty {
                serde_json::to_string_pretty(&pack).context("serialising pack to JSON")?
            } else {
                serde_json::to_string(&pack).context("serialising pack to JSON")?
            }
        }
        ProposeFormat::Markdown => render_markdown(&pack),
        ProposeFormat::Text => render_text(&pack),
    };

    match args.out.as_deref() {
        Some(path) => {
            write_to(path, &rendered)?;
            eprintln!("已写入业务证据包: {}", path.display());
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

// ---------------------------------------------------------------------------
// Markdown — a copy-paste-ready business documentation draft.
// ---------------------------------------------------------------------------

fn render_markdown(pack: &BusinessPack) -> String {
    let mut s = String::new();
    s.push_str("# 业务模块证据包（SpecSlice propose）\n\n");
    s.push_str("> 由 `specslice propose` 基于代码/文档/测试事实自动生成（非侵入、只读图）。\n");
    s.push_str("> 以下为供 AI 提炼业务逻辑的**证据**，不是已确认 requirement；确认请走 `specslice candidate review`。\n\n");
    s.push_str(&format!(
        "> 业务模块 {} 个（共识别 {}）· 代码符号 {} · 文档 {} · 测试 {}\n\n",
        pack.stats.modules_reported,
        pack.stats.total_modules,
        pack.stats.total_symbols,
        pack.stats.total_docs,
        pack.stats.total_tests,
    ));

    // module dependency flowchart — only the strongest edges, so the
    // overview stays legible on real repos.
    s.push_str("## 业务模块依赖图\n\n");
    s.push_str(&format!(
        "> 仅展示权重最高的前 {} 条跨模块依赖；完整依赖见各模块「依赖模块」。\n\n",
        MAX_MERMAID_EDGES
    ));
    s.push_str("```mermaid\nflowchart LR\n");
    for m in &pack.modules {
        s.push_str(&format!(
            "  {}[\"{}\"]\n",
            mermaid_id(&m.id),
            escape_mermaid(&m.name)
        ));
    }
    let reported: std::collections::HashSet<&str> =
        pack.modules.iter().map(|m| m.id.as_str()).collect();
    // `module_dependencies` is already weight-sorted (engine), so keep the
    // first MAX_MERMAID_EDGES whose endpoints are both reported modules.
    let drawable: Vec<&ModuleDependency> = pack
        .module_dependencies
        .iter()
        .filter(|d| reported.contains(d.from.as_str()) && reported.contains(d.to.as_str()))
        .collect();
    if drawable.is_empty() {
        s.push_str("  %% （未发现跨模块依赖边 — 可能尚无 calls/imports 精确层）\n");
    }
    for d in drawable.iter().take(MAX_MERMAID_EDGES) {
        s.push_str(&format!(
            "  {} --> {}\n",
            mermaid_id(&d.from),
            mermaid_id(&d.to)
        ));
    }
    if drawable.len() > MAX_MERMAID_EDGES {
        s.push_str(&format!(
            "  %% 省略 {} 条较弱依赖\n",
            drawable.len() - MAX_MERMAID_EDGES
        ));
    }
    s.push_str("```\n\n");

    // product-level docs that no single module claims
    if !pack.key_docs.is_empty() {
        s.push_str("## 全局关键文档（产品级，未归属单一模块）\n\n");
        for d in &pack.key_docs {
            s.push_str(&format!("- `{}` — {}\n", d.path, d.name));
        }
        s.push('\n');
    }

    // module evidence sections
    s.push_str("## 模块证据\n\n");
    for m in &pack.modules {
        render_module_md(&mut s, m);
    }

    // candidate skeleton table
    s.push_str("## 候选骨架（待 AI 填写描述/置信度）\n\n");
    s.push_str("| 模块 id | 模块 | 入口符号数 | 路由 | Provider | 存储 | 测试 | 文档 |\n");
    s.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for m in &pack.modules {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} |\n",
            m.id,
            escape_pipe(&m.name),
            m.entry_points.len(),
            m.routes.len(),
            m.providers.len(),
            m.storage.len(),
            m.test_count,
            m.docs.len(),
        ));
    }
    s.push('\n');

    // the AI prompt
    s.push_str("## 喂给 AI 的提示词\n\n");
    s.push_str("把本文件（或 `--format json` 的证据包）连同下面的提示词交给你信任的模型，让它产出 `.specslice/candidates/business_logic.yaml`：\n\n");
    s.push_str("```text\n");
    s.push_str(pack.prompt.trim_end());
    s.push_str("\n```\n");

    s
}

fn render_module_md(s: &mut String, m: &ModuleEvidence) {
    s.push_str(&format!(
        "### {} (`{}`) — 信号分 {}\n\n",
        m.name, m.id, m.signal_score
    ));
    s.push_str(&format!("- 路径: `{}`\n", m.path_prefix));
    s.push_str(&format!(
        "- 规模: {} 文件 · {} 符号 · {} 测试 · 内聚 {:.0}%（{}）\n",
        m.file_count,
        m.symbol_count,
        m.test_count,
        m.cohesion * 100.0,
        cohesion_hint(m.cohesion),
    ));
    if !m.framework_roles.is_empty() {
        s.push_str(&format!("- 框架角色: {}\n", m.framework_roles.join(", ")));
    }
    if !m.depends_on.is_empty() {
        s.push_str(&format!("- 依赖模块: {}\n", m.depends_on.join(", ")));
    }
    if !m.routes.is_empty() {
        s.push_str(&format!("- 路由 (navigates_to): {}\n", m.routes.join(", ")));
    }
    if !m.providers.is_empty() {
        s.push_str(&format!(
            "- Provider (reads_provider): {}\n",
            m.providers.join(", ")
        ));
    }
    if !m.storage.is_empty() {
        s.push_str(&format!("- 存储 (persists_to): {}\n", m.storage.join(", ")));
    }
    if m.stream_subscriptions > 0 {
        s.push_str(&format!(
            "- 流订阅 (subscribes_stream): {}\n",
            m.stream_subscriptions
        ));
    }
    if !m.entry_points.is_empty() {
        s.push_str("- 入口符号:\n");
        for ep in &m.entry_points {
            let roles = if ep.roles.is_empty() {
                String::new()
            } else {
                format!(" [{}]", ep.roles.join(","))
            };
            s.push_str(&format!(
                "  - `{}` ({}){} — `{}`\n",
                ep.name, ep.kind, roles, ep.path
            ));
        }
    }
    if !m.docs.is_empty() {
        s.push_str("- 文档:\n");
        for d in &m.docs {
            s.push_str(&format!("  - `{}`\n", d.path));
        }
    }
    if !m.tests.is_empty() {
        s.push_str("- 测试:\n");
        for t in m.tests.iter().take(5) {
            s.push_str(&format!("  - `{}`\n", t.path));
        }
    }
    if !m.evidence.is_empty() {
        s.push_str("- 候选证据 id（粘到 business_logic.yaml 的 `evidence:`）:\n");
        for ev in &m.evidence {
            s.push_str(&format!("  - `{}`\n", ev));
        }
    }
    s.push('\n');
}

// ---------------------------------------------------------------------------
// Text — terse human summary.
// ---------------------------------------------------------------------------

fn render_text(pack: &BusinessPack) -> String {
    let mut s = String::new();
    s.push_str("SpecSlice propose · 业务模块证据包\n");
    s.push_str(&format!(
        "模块 {}/{} · 符号 {} (已归属 {}) · 文档 {} · 测试 {}\n\n",
        pack.stats.modules_reported,
        pack.stats.total_modules,
        pack.stats.total_symbols,
        pack.stats.assigned_symbols,
        pack.stats.total_docs,
        pack.stats.total_tests,
    ));
    if pack.modules.is_empty() {
        s.push_str("(图太稀疏 — 先 `specslice index`，或确认 code roots 配置)\n");
        return s;
    }
    for m in &pack.modules {
        s.push_str(&format!(
            "== {} (`{}`) · 分 {} ==\n",
            m.name, m.id, m.signal_score
        ));
        s.push_str(&format!(
            "   {} 文件 · {} 符号 · {} 测试 · {} 文档 · 入口 {} · 内聚 {:.0}%\n",
            m.file_count,
            m.symbol_count,
            m.test_count,
            m.docs.len(),
            m.entry_points.len(),
            m.cohesion * 100.0,
        ));
        if !m.routes.is_empty() {
            s.push_str(&format!("   路由: {}\n", m.routes.join(", ")));
        }
        if !m.providers.is_empty() {
            s.push_str(&format!("   Provider: {}\n", m.providers.join(", ")));
        }
        if !m.storage.is_empty() {
            s.push_str(&format!("   存储: {}\n", m.storage.join(", ")));
        }
        if !m.depends_on.is_empty() {
            s.push_str(&format!("   依赖: {}\n", m.depends_on.join(", ")));
        }
    }
    if !pack.key_docs.is_empty() {
        s.push_str(&format!(
            "\n全局关键文档 ({}): {}\n",
            pack.key_docs.len(),
            pack.key_docs
                .iter()
                .map(|d| d.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    s.push_str("\n下一步: `specslice propose --format markdown --out .specslice/export/business-pack.md` 再喂给 AI 生成 business_logic.yaml。\n");
    s
}

// ---------------------------------------------------------------------------
// mermaid / markdown escaping
// ---------------------------------------------------------------------------

/// A plain-language read of the graph cohesion: how self-contained the
/// module is. High = a clean business boundary; low = entangled with the
/// rest of the codebase (a refactor smell worth flagging to the AI).
fn cohesion_hint(cohesion: f64) -> &'static str {
    if cohesion >= 0.7 {
        "高内聚，边界清晰"
    } else if cohesion >= 0.4 {
        "中等内聚"
    } else {
        "低内聚，与其他模块耦合较多"
    }
}

fn mermaid_id(slug: &str) -> String {
    // mermaid node ids must be identifier-ish; slugs are already
    // `[a-z0-9_-]` but `-` is safest replaced with `_`.
    slug.replace('-', "_")
}

fn escape_mermaid(s: &str) -> String {
    s.replace('"', "'")
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::MAX_MERMAID_EDGES;
    use super::*;
    use specslice_engine::business_pack::{
        BusinessPack, BusinessPackStats, EvidenceRef, EvidenceSymbol, ModuleDependency,
        ModuleEvidence, BUSINESS_PACK_SCHEMA_VERSION,
    };

    fn sample_pack() -> BusinessPack {
        BusinessPack {
            schema_version: BUSINESS_PACK_SCHEMA_VERSION,
            repo_root: ".".into(),
            stats: BusinessPackStats {
                total_modules: 2,
                modules_reported: 2,
                total_symbols: 10,
                assigned_symbols: 9,
                unassigned_symbols: 1,
                total_docs: 1,
                total_tests: 1,
            },
            modules: vec![
                ModuleEvidence {
                    id: "auth".into(),
                    name: "Auth".into(),
                    path_prefix: "lib/features/auth".into(),
                    file_count: 3,
                    symbol_count: 5,
                    test_count: 1,
                    signal_score: 42,
                    cohesion: 0.82,
                    entry_points: vec![EvidenceSymbol {
                        id: "dart_class::lib/features/auth/auth_bloc.dart#AuthBloc".into(),
                        kind: "dart_class".into(),
                        name: "AuthBloc".into(),
                        path: "lib/features/auth/auth_bloc.dart".into(),
                        line_range: Some((1, 9)),
                        roles: vec![],
                    }],
                    routes: vec!["/login".into()],
                    providers: vec!["authProvider".into()],
                    storage: vec![],
                    stream_subscriptions: 0,
                    framework_roles: vec![],
                    docs: vec![EvidenceRef {
                        id: "doc_section::docs/auth.md#Auth".into(),
                        path: "docs/auth.md".into(),
                        name: "Auth".into(),
                    }],
                    tests: vec![],
                    depends_on: vec![],
                    evidence: vec!["dart_class::lib/features/auth/auth_bloc.dart#AuthBloc".into()],
                },
                ModuleEvidence {
                    id: "products".into(),
                    name: "Products".into(),
                    path_prefix: "lib/features/products".into(),
                    file_count: 2,
                    symbol_count: 4,
                    test_count: 0,
                    signal_score: 30,
                    cohesion: 0.35,
                    entry_points: vec![],
                    routes: vec![],
                    providers: vec![],
                    storage: vec![],
                    stream_subscriptions: 0,
                    framework_roles: vec![],
                    docs: vec![],
                    tests: vec![],
                    depends_on: vec!["auth".into()],
                    evidence: vec![],
                },
            ],
            module_dependencies: vec![ModuleDependency {
                from: "products".into(),
                to: "auth".into(),
                weight: 3,
            }],
            key_docs: vec![EvidenceRef {
                id: "doc_section::README.md#Overview".into(),
                path: "README.md".into(),
                name: "Overview".into(),
            }],
            prompt: "你是 SpecSlice 的业务逻辑提炼器。输出 business_logic.yaml。".into(),
        }
    }

    #[test]
    fn markdown_has_mermaid_modules_table_and_prompt() {
        let md = render_markdown(&sample_pack());
        assert!(md.contains("flowchart LR"));
        assert!(md.contains("auth[\"Auth\"]"));
        assert!(md.contains("products --> auth"), "dependency edge rendered");
        assert!(md.contains("### Auth (`auth`)"));
        assert!(md.contains("路由 (navigates_to): /login"));
        assert!(md.contains("候选骨架"));
        assert!(md.contains("business_logic.yaml"), "prompt embedded");
        assert!(
            md.contains("全局关键文档") && md.contains("`README.md`"),
            "pack-level key docs rendered"
        );
        assert!(
            md.contains("dart_class::lib/features/auth/auth_bloc.dart#AuthBloc"),
            "evidence id surfaced for copy-paste"
        );
    }

    #[test]
    fn text_summary_lists_modules_and_signals() {
        let txt = render_text(&sample_pack());
        assert!(txt.contains("Auth (`auth`)"));
        assert!(txt.contains("路由: /login"));
        assert!(txt.contains("依赖: auth"));
    }

    fn dense_pack(modules: usize, edges: usize) -> BusinessPack {
        let module_evs: Vec<ModuleEvidence> = (0..modules)
            .map(|i| ModuleEvidence {
                id: format!("m{i}"),
                name: format!("M{i}"),
                path_prefix: format!("lib/features/m{i}"),
                file_count: 1,
                symbol_count: 1,
                test_count: 0,
                signal_score: u32::try_from(modules - i).unwrap(),
                cohesion: 0.5,
                entry_points: vec![],
                routes: vec![],
                providers: vec![],
                storage: vec![],
                stream_subscriptions: 0,
                framework_roles: vec![],
                docs: vec![],
                tests: vec![],
                depends_on: vec![],
                evidence: vec![],
            })
            .collect();
        // build `edges` distinct directed pairs i -> j (i != j), weight desc
        let mut deps = Vec::new();
        let mut weight = 1000usize;
        'outer: for i in 0..modules {
            for j in 0..modules {
                if i == j {
                    continue;
                }
                deps.push(ModuleDependency {
                    from: format!("m{i}"),
                    to: format!("m{j}"),
                    weight,
                });
                weight = weight.saturating_sub(1);
                if deps.len() >= edges {
                    break 'outer;
                }
            }
        }
        BusinessPack {
            schema_version: BUSINESS_PACK_SCHEMA_VERSION,
            repo_root: ".".into(),
            stats: BusinessPackStats {
                total_modules: modules,
                modules_reported: modules,
                total_symbols: modules,
                assigned_symbols: modules,
                unassigned_symbols: 0,
                total_docs: 0,
                total_tests: 0,
            },
            modules: module_evs,
            module_dependencies: deps,
            key_docs: vec![],
            prompt: "prompt".into(),
        }
    }

    #[test]
    fn markdown_mermaid_caps_edges() {
        let md = render_markdown(&dense_pack(10, 60));
        let mermaid = md
            .split("```mermaid")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .expect("mermaid block");
        let edge_lines = mermaid.lines().filter(|l| l.contains("-->")).count();
        assert!(
            edge_lines <= MAX_MERMAID_EDGES,
            "mermaid edges capped at {MAX_MERMAID_EDGES}, got {edge_lines}"
        );
        assert!(
            mermaid.contains("省略"),
            "truncation note present when edges are dropped"
        );
        // strongest edge (highest weight) must survive the cap
        assert!(mermaid.contains("m0 --> m1"), "highest-weight edge kept");
    }
}
