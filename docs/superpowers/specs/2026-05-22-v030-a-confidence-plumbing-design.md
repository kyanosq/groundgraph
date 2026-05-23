# v0.3.0-A — 把 evidence_quality 接到 dead-code 和 search

- **Status:** Draft (待 user review)
- **Author / drafter:** SpecSlice agent (Cursor / Claude Opus 4.7)
- **Date:** 2026-05-22
- **Sub-project of:** v0.3.0 主题"图可信度 / 死代码误报收敛 / 候选证据评分 / 搜索排序 / MCP·Skill 闭环"
- **Depends on:** v0.2.0 已上线的 `edge_confidence` 模块
- **Unblocks:** v0.3.0-B (TS/Java framework decorator)、C (候选证据评分)、D (搜索排序扩展)

## 1. 问题陈述

v0.2.0 已经把每条边算成 `EdgeConfidence::High | Medium | Low`
（`crates/specslice-engine/src/edge_confidence.rs:102-181`），
并把它作为 `evidence_quality` 字段挂在 `GraphEdge` 上
（`crates/specslice-engine/src/graph.rs:600-627`），但**下游消费者一个都没用**：

- `search.rs:48-66` 定义了 `SCORE_EDGE_EVIDENCE = 30` 和
  `SCORE_NEIGHBOR = 20` 两个常量，但代码里**从不 reference**——
  ranking 完全靠 token match。
- `dead_code.rs:416-427` BFS 把 `python_ast` 的 `Calls` 边和
  `python_lsp` 的 `Calls` 边一视同仁；bucket 决策从不参考边质量。
- 结果：操作者拿到的 dead-code high-bucket 候选无法区分"完全没人
  调用"和"只有 AST fallback 给出的低置信入边"；搜索结果无法
  区分"有真实证据出边的核心符号"和"只在某个测试里被命名一次"。

## 2. 目标

把已有的 `evidence_quality` 信号接到 dead-code 的**桶分配 / 原因
字符串**（不动 BFS reach 集合）和 search 的**ranking 加权**
（点亮两个 dormant 常量），并抽出一层 `confidence_view` 共享
helper 给后续 B/C/D 复用。

具体可观测变化：

- `specslice dead-code --json` 的 high-bucket entry 在"仅有低置信
  入边"时多一行 reason，操作者能看出证据强度。
- `specslice search "<token>"` 的命中排序在以下两种情况里被推前：
  - 命中节点本身有高置信出边（语义"这是一个有强证据的核心符号"）。
  - 命中节点的 1-hop 邻居里有另一条命中（语义"功能域内多点共现"）。
- `search` 的 JSON / MCP 输出新增结构化 `warnings: Vec<String>`，
  engine 层不再直接写 stderr。

## 3. 非目标

- ❌ 不改 SQLite schema（`EdgeAssertion.confidence: f32` 仍然
  默认 1.0；本 sub-project 走的是 query-time `evidence_quality`，
  不动持久化字段）。
- ❌ 不改 `dead_code` BFS reach 集合 —— 任何 inbound usage edge
  仍然能把目标节点拉成"活"，绝不引入 FN 风险。
- ❌ 不改 `BusinessCandidate.confidence` 语义（YAML 自由浮点，
  由 v0.3.0-C 处理）。
- ❌ 不改 MCP tool 输出字段类型；`dead_code` entry 的 reason 列表
  只是追加新行；`search` hit 字段保持向后兼容。
- ❌ 本 sub-project 不引入 `imports`-based ranking boost；imports
  作为"软结构信号"留给后续（B/C/D 任意一个决定怎么用）。
- ❌ 本 sub-project 不上 `benches/`。理由：这是
  ranking/explainability 项目，不是性能优化项目；bench 维护成本
  高、信号弱。改成"unit + 4-repo 真实扫描报告"组合（见 § 7）。

## 4. 架构

```
        ┌──────────────────────────────────────────────┐
        │ confidence_view.rs (新增，本 sub-project)      │
        │                                              │
        │  pub enum EdgeQualityScope {                 │
        │      Usage,        // dead-code 用           │
        │      SearchRanking // search 用              │
        │  }                                           │
        │                                              │
        │  pub fn inbound_edge_quality(                │
        │      store, node_id, scope                   │
        │  ) -> Result<EdgeQualitySummary>             │
        │                                              │
        │  pub fn outbound_edge_quality(               │
        │      store, node_id, scope                   │
        │  ) -> Result<EdgeQualitySummary>             │
        │                                              │
        │  EdgeQualitySummary { high, medium, low }    │
        │      .dominant() -> Option<EdgeConfidence>   │
        │      .is_empty() -> bool                     │
        │                                              │
        │  Scope 在内部决定允许哪些 EdgeKind ──────────┐  │
        └─────────────────────────────────────────┬─┘ │  │
                                                  ▼ │ │  │
                                       Excluded:    │ │  │
                                       Contains (结构) ◄┘  │
                                       DerivesFrom (候选合成) ◄┘
                                       Imports (留给后续)
        ┌─────────────────────────────────────────┐
        │  Usage scope 允许 kinds:                  │
        │    Calls / References / ReadsProvider /   │
        │    PersistsTo / NavigatesTo /             │
        │    SubscribesStream / DeclaresVerification│
        │                                           │
        │  SearchRanking scope 允许 kinds:         │
        │    与 Usage 相同（本 sub-project 不上     │
        │    Imports；若 B/C/D 需要再扩 scope）       │
        └─────────────────────────────────────────┘

                            │
       ┌────────────────────┼─────────────────────┐
       ▼                    ▼                     ▼
   dead_code            search                 (B/C/D 之后)
   bucket reason +一行   ranking +0..2 boost    复用 scope，不分叉
```

为什么有 `EdgeQualityScope` 而不是固定一种？因为 B/C/D 的语义
不同：dead-code 关心"是否被业务使用"，search 关心"证据强度"，
candidate 评分关心"被引用的符号自身的证据完整度"。一个固定的
kind 列表会在 v0.3.0-C 上线时立刻分叉，提前让 scope 参数化是
更小的总改动量。本轮只暴露 `Usage` 和 `SearchRanking` 两个，
其余将来按需扩。

## 5. API 详细

### 5.1 `confidence_view.rs`（新增）

```rust
//! 按 node 计算入/出边的 evidence_quality 分布。
//!
//! 这一层不持有 store 状态，每次调用都是一次 sqlite query；
//! 调用方可以自由 batch。语义由 EdgeQualityScope 决定，避免
//! dead-code / search / 后续 B/C/D 各自重复"哪些 kind 算
//! usage 证据"这个判断。

use crate::edge_confidence::{confidence_for_edge, EdgeConfidence};
use specslice_core::edge::EdgeKind;
use specslice_store::Store;
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeQualityScope {
    /// 用于 dead-code：只算"被业务使用"的边。
    /// 排除 Contains（结构）、Imports（软结构）、DerivesFrom
    /// （候选合成）。
    Usage,
    /// 用于 search ranking：当前等同于 Usage。预留这个枚举
    /// 是为了让后续 B/C/D 单独定制 search 的允许集，而不需要
    /// 改 inbound_edge_quality / outbound_edge_quality 的签名。
    SearchRanking,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EdgeQualitySummary {
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

impl EdgeQualitySummary {
    pub fn total(&self) -> u32 { /* sum */ }
    pub fn is_empty(&self) -> bool { /* total == 0 */ }
    /// 返回桶里 count 最多的 tier；并列时按 high > medium > low。
    pub fn dominant(&self) -> Option<EdgeConfidence> { /* … */ }
    /// 是否"只有 low-tier 证据"（用于 dead-code reason）。
    pub fn is_only_low(&self) -> bool {
        self.high == 0 && self.medium == 0 && self.low > 0
    }
}

pub fn inbound_edge_quality(
    store: &Store,
    node_id: &str,
    scope: EdgeQualityScope,
) -> Result<EdgeQualitySummary>;

pub fn outbound_edge_quality(
    store: &Store,
    node_id: &str,
    scope: EdgeQualityScope,
) -> Result<EdgeQualitySummary>;

/// 1-hop 邻接（双向、按对端 id 去重、字典序、限 `cap`）。
/// search 用作 SCORE_NEIGHBOR 的相交基；v0.3.0-C 候选证据评分
/// 也会复用。**不**过 EdgeQualityScope —— 邻接是结构问题，与
/// "哪条边算 usage 证据"无关。
pub fn neighbors_of(
    store: &Store,
    node_id: &str,
    cap: usize,
) -> Result<Vec<NeighborInfo>>;

pub struct NeighborInfo { pub id: String, pub name: String, pub kind: String }
```

实现要点：

- 单条 SQL：`SELECT kind, source, certainty, status, indexer
  FROM edge_assertions WHERE to_node = ? AND status != 'deprecated'`
  （outbound 改成 `from_node`）。已有 index `idx_edges_to_node` /
  `idx_edges_from_node` 覆盖。
- 行级解码 → 构造 `EdgeAssertion`-shaped tuple →
  `confidence_for_edge(...)` 得 tier → scope 过滤 EdgeKind → 计数。
- scope 过滤通过 `scope.allows(kind: EdgeKind) -> bool` 私有 helper
  实现；矩阵测试钉死每个 EdgeKind 在每个 scope 下的允许 / 排除态，
  防止后续新增 EdgeKind 时遗漏归类。

### 5.2 `dead_code.rs` 变更

- BFS 不动；reach 集合不变。
- 在 `assign_bucket(...)`（或等价位置）之后，**追加** reason 字符串：
  - 若 `inbound_edge_quality(node, Usage).is_only_low()` 且 node 已被
    判为 high/medium 桶 → 追加一行：
    `"仅有 N 条 low-tier 入边（来自低置信 indexer / AST fallback / lightweight resolver），证据较弱"`
  - 若 `inbound_edge_quality.is_empty()`：现有 reason 已经覆盖（"无任何 calls / references / declares_verification 入边"），不重复添加。
- 不动现有的 medium-bucket "构造器" / "公共可见" / "lifecycle" 这些
  mitigating-factor reason；它们继续按原样写出来。
- 所有现存 `dead_code::tests` 必须不动也仍绿（reason 列表追加新行
  而不替换旧行）。

### 5.3 `search.rs` 变更

第一遍 token-match score 不动；新增**两个后处理 pass**：

**Pass A — Edge evidence boost (per hit, independent)**

```rust
for hit in &mut hits {
    let oq = confidence_view::outbound_edge_quality(
        store, &hit.id, EdgeQualityScope::SearchRanking
    )?;
    if oq.high >= 1 {
        hit.score += SCORE_EDGE_EVIDENCE;          // = 30
        hit.match_reasons.push(format!(
            "出边 evidence_quality=high ({} 条)，符号有强证据支撑",
            oq.high
        ));
    }
}
```

**Pass B — Neighbor boost (capped, symmetric, deterministic)**

需要一个新 helper `neighbors_of(store, node_id, cap) -> Result<Vec<NeighborInfo>>`，
co-locate 在 `confidence_view.rs`（因为它和"按 node 看相邻边"是一类语义；
对外可供 search 以及 v0.3.0-C 候选证据评分复用）。语义：单条 SQL
取 `from_node = ? OR to_node = ?` 的边，按对端 node id 去重，按 id
字典序排序，取前 `cap` 个，每个携带对端 `id + name + kind` 三元组。

```rust
let hit_ids: HashSet<_> = hits.iter().map(|h| h.id.clone()).collect();
for hit in &mut hits {
    let neighbors = confidence_view::neighbors_of(store, &hit.id, 8)?;
    // 全集：邻居中与 hit-set 相交者
    let matches: Vec<_> = neighbors.iter()
        .filter(|n| n.id != hit.id && hit_ids.contains(&n.id))
        .collect();
    let matches_total = matches.len();
    let reason_names: Vec<_> = matches.iter().take(2)
        .map(|n| n.name.clone())
        .collect();
    if matches_total > 0 {
        hit.score += SCORE_NEIGHBOR;        // = 20，**每个 hit 最多 +1 次**
        hit.match_reasons.push(format!(
            "邻接其他命中（{}{}）",
            reason_names.join("、"),
            if matches_total > 2 { " 等" } else { "" }
        ));
    }
}
```

为什么 cap：原始设计"每对邻居各加一次"在 cluster 里会把 5 个弱命中
互相 +80 分顶到 exact ID match (100) 前面，破坏排序的可解释性。改成
**每个 hit 最多 +1 次 SCORE_NEIGHBOR**，匹配邻居总数大于 2 时
reason 后缀加"等"，让它做 tie-breaker 而不是主排序信号。

**Engine 层 warnings 改造（响应 user feedback #4）**

`SearchResult` 与 `DeadCodeReport` 都新增 `warnings`，沿用工程里
已有的同型字段（参见 `impact::ImpactReport` line 139、
`logic_confidence::LogicConfidenceReport` line 132）：

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub warnings: Vec<String>,
```

- 任何在 Pass A / Pass B 中 sqlite 出错的节点：跳过该节点 + 把
  `format!("warn: 节点 {id} 的边质量查询失败：{err}")` 推进
  `SearchResult.warnings`。
- 任何在 `dead_code` 调 `inbound_edge_quality` 时出错：reason
  列表不追加新行，把 `format!("warn: 节点 {id} 的入边质量查询失败：{err}")`
  推进 `DeadCodeReport.warnings`。
- engine 层**不再写 stderr**。
- 序列化用 `skip_serializing_if = "Vec::is_empty"`，老 JSON
  消费者完全向后兼容（字段为空时连键都不出现）。
- CLI human renderer 在末尾打印 warnings；JSON 输出走结构化
  `warnings` 字段；MCP `search_graph` / `dead_code` 透传同字段。

## 6. Data flow

```
search "login"
  ├── 现有 token-match collect: Vec<SearchMatch>，每个有 base score
  ├── Pass A — 对每个 hit：
  │     outbound_edge_quality(hit, SearchRanking)
  │       └── confidence_view 内部：SQL on edge_assertions
  │           where from_node = hit.id and kind allowed by scope
  │             → for each row: confidence_for_edge → 累计
  │       └── 若 high ≥ 1 → hit.score += 30, push reason
  ├── Pass B — 对每个 hit：
  │     neighbors_of(hit, cap=8)
  │       └── 内部 SQL（已有 query 或新增）取 1-hop 双向邻居
  │     与 hit-set 相交 → cap +1 次 SCORE_NEIGHBOR + 前 2 reason
  ├── re-sort by score desc, stable
  └── 返回 SearchResult { hits, warnings, … }

dead_code --json
  ├── 现有 BFS reach 集合 — 不变
  ├── 现有 bucket 决策 — 不变
  └── 每个 dead candidate：
        inbound_edge_quality(candidate.id, Usage)
          └── 若 is_only_low() → 追加 reason 字符串
```

## 7. Testing 矩阵（"尽可能全面"，砍掉 bench）

### 7.1 Unit（红绿门槛 — TDD 进 implementation 前先写）

`confidence_view::tests`：

- `inbound_edge_quality_counts_by_tier` — 混合 high/medium/low 边，断言计数对。
- `outbound_edge_quality_counts_by_tier` — 同上。
- `usage_scope_excludes_contains` — 喂一条 `Contains`，summary 为空。
- `usage_scope_excludes_derives_from` — 同上。
- `usage_scope_excludes_imports` — 同上。
- `search_ranking_scope_excludes_same_set_as_usage_in_v030_a` —
  锁定本轮的 scope 等价；将来扩 scope 时人为改这个 test 提醒自己。
- `dominant_breaks_ties_in_favor_of_high`.
- `is_only_low_true_when_only_low_evidence`.
- `missing_node_returns_empty`.
- `deprecated_edges_excluded`.
- **EdgeKind 矩阵测试**：`ALL_EDGE_KINDS` 数组在 `Usage` 和
  `SearchRanking` 两个 scope 下的允许 / 排除态显式断言；新增
  EdgeKind 时编译器+矩阵 test 强制更新。
- `neighbors_of_dedup_and_sorts_and_caps` — 喂双向重复 + 超过 `cap`
  条边，断言去重 + 字典序 + 限长。
- `neighbors_of_excludes_self_loops` — 防御性。

`dead_code::tests` 新增：

- `high_bucket_reason_mentions_low_tier_inbound_when_only_low_evidence_exists`
- `existing_reason_strings_are_preserved_when_only_high_tier_inbound`
  （回归保护：现有"无任何 calls / references / declares_verification 入边"
  字符串原样仍打印）
- `bfs_reach_set_unchanged_when_low_tier_edges_present`
  （强等价 — 用一个 fixture 跑老代码 reach + 新代码 reach，断言相等）

`search::tests` 新增：

- `score_match_includes_edge_evidence_boost_when_high_tier_outbound_exists`
- `score_match_does_not_include_edge_evidence_boost_when_only_medium_low_outbound`
- `neighbor_boost_caps_at_single_increment_per_hit`
- `neighbor_boost_is_symmetric_between_two_adjacent_hits`
- `neighbor_boost_zero_when_no_hit_is_adjacent_to_another_hit`
- `neighbor_reason_lists_at_most_two_neighbor_names_and_appends_等`
- `engine_layer_does_not_write_to_stderr` — 用 stderr capture 跑
  一次 `search` 在故意写入的 sqlite 错节点上，断言 stderr 空、
  `result.warnings` 非空。
- `mcp_search_graph_passes_through_warnings` — MCP 集成 test。

### 7.2 4-repo 真实扫描对比（empirical 收口）

复用 `scripts/release_scan.sh` 重扫四个 vetted 仓库
（pixcraft-app / pixcraft-landing / atagent / vub），用一个新工具
`scripts/diff_release_scans.sh <before-dir> <after-dir>` 输出：

```
==> dead-code reason diff (per repo, per bucket)
  pixcraft-app/high:
    +0 entries, 0 removed
    3 entries gained suffix '仅有 low-tier 入边'
  pixcraft-app/medium: ...

==> search top-10 diff (固定 queries: login, watermark, purchase,
                          scan, evidence, greeter, store, save)
  watermark:
    before #1: WatermarkApp                       score=80
    after  #1: WatermarkApp                       score=110  (+EDGE_EVIDENCE)
    before #3: WatermarkController                score=60
    after  #2: WatermarkController                score=80   (+NEIGHBOR)
  ...
```

报告写到 `reports/release/v0.3.0-A.md`，commit 一起入库；4 仓的
完整 scratch 产物按 `.gitignore` 规则不入库（与 v0.2.0 同模式）。

### 7.3 Quality gates（不变）

`cargo fmt --all -- --check`、`cargo clippy --workspace
--all-targets -- -D warnings`、`cargo test --workspace`、
`dart test` 在 `tool/specslice_dart_analyzer/` 仍是必过门槛；
`cargo test -p specslice-engine --test lsp_indexers --
--include-ignored` 不在本轮的 scope 改动里（不动 LSP 路径）。

## 8. Error handling

- `confidence_view` 的两个查询返回 `Result<EdgeQualitySummary>`；
  sqlite 错就向上抛。
- 节点不存在 / 无边 → 返回 `EdgeQualitySummary::default()`（全 0），
  调用方自然按"无入边"分支处理。
- `search` Pass A / B 在单节点失败时**不向 stderr 写任何东西**；
  把 `format!("warn: ...")` 推进 `SearchResult.warnings`，跳过
  该节点的本次 boost。CLI human 渲染器在末尾打印 warnings；
  JSON / MCP 透传字段。
- `dead_code` 调 `inbound_edge_quality` 失败 → 现有 reason 列表
  正常输出，**不**追加新行；把一条 warning 推进
  `DeadCodeReport.warnings`（**新增字段**，按
  `impact::ImpactReport.warnings` / `logic_confidence::
  LogicConfidenceReport.warnings` 的同型范式：`#[serde(default,
  skip_serializing_if = "Vec::is_empty")]`，老 JSON 消费者
  100% 向后兼容）。

## 9. 开放问题 / 未来工作

- **Imports 软信号**：B 处理 TS/Java framework decorator 时大概率
  需要"imports 路径里包含 'framework' 关键字"这类启发，到时再决定
  是扩 `EdgeQualityScope::SearchRanking` 还是另起 `ImportsHint`。
- **Neighbor pass 性能**：当前实现是 hits×8 次邻接查询；4-repo
  实测延迟 < 100ms 都可接受。若 vub（3000+ Java 文件）的某个
  high-frequency query 出现明显回归（>20% 延迟），加 cap=8 即可，
  必要时改成 hit-set ∩ adj 的单 SQL JOIN。性能调优不在本轮
  scope。
- **Edge weights**：当前 SCORE_EDGE_EVIDENCE=30、SCORE_NEIGHBOR=20
  来自 search.rs 现存常量声明；若 4-repo 对比报告显示比例失衡，
  在本 sub-project 收尾时调一次，不留给下一个 sub-project。

## 10. 验收清单（对 user）

完成本 sub-project 时，user 应能在 review 阶段看到：

- [ ] `crates/specslice-engine/src/confidence_view.rs` 新增 + ~12 个
  单测全绿（含 EdgeKind 矩阵 + neighbors_of 行为）
- [ ] `dead_code.rs` 改动 + 3 个新单测 + 现有单测全部不动也仍绿
- [ ] `search.rs` 改动 + 8 个新单测 + 现有单测全部仍绿
- [ ] `SearchResult.warnings` 与 `DeadCodeReport.warnings` 两字段
  补上，按现有 `impact::ImpactReport` 范式（`skip_serializing_if`
  确保旧 JSON 消费者兼容）；MCP / JSON / CLI 三路打通
- [ ] `cargo fmt --check` / `cargo clippy -- -D warnings` /
  `cargo test --workspace` 全绿
- [ ] `reports/release/v0.3.0-A.md` 入库，包含 4 仓 dead-code
  reason diff + search top-K diff
- [ ] `docs/implementation-plan.md` 新增 v0.3.0 章节
  （v0.2.0 之后），登记本 sub-project 的范围与验收

## 14. Implementation addendum — 实施过程中确认/微调的设计

为了与最终落地的代码保持一致，把开发中确认的几处实现细节回写进
本 spec，避免 spec ↔ 代码漂移。

### 14.1 `confidence_view` 暴露第三个纯函数 `summarize_edges`

设计稿原本只列了 `inbound_edge_quality / outbound_edge_quality`
两个 store-aware API。实际开发时把"如何把 edges 切成 high/medium/low
桶"抽成一个**纯函数** `summarize_edges`，签名：

```rust
pub fn summarize_edges<'a, I>(edges: I, scope: EdgeQualityScope) -> EdgeQualitySummary
where
    I: IntoIterator<Item = &'a EdgeAssertion>;
```

这样：
- `inbound_edge_quality` / `outbound_edge_quality` 内部都走
  `store.list_edges_*` → `summarize_edges(_, scope)`，零重复逻辑。
- `dead_code::classify(...)` 复用已经在内存里的
  `&[&EdgeAssertion]`（避免对 sqlite 做第二次查询），直接调
  `summarize_edges(inbound_usage.iter().map(|e| **e), scope)`。
- 纯函数完全可单测，无需 fixtures。

### 14.2 `EdgeQualitySummary::is_only_low` 替代外部判定

设计稿"dead-code only-low-tier reason"的判定逻辑被收敛进
`EdgeQualitySummary::is_only_low(&self) -> bool` 方法
（`self.high == 0 && self.medium == 0 && self.low > 0`）。
这样未来 spec / 调用方修改阈值定义时只需改一个地方。

同时为了 future-proof，summary 还附带 `total()` / `is_empty()` /
`dominant() -> Option<EdgeConfidence>` 三个观察方法，供 v0.3.0-B 复用。

### 14.3 Phase 5 真实仓库覆盖率回填

完成 Phase 1-4 后，在 v0.2.0 阶段沉淀好的四份
`release-scans/_scratch/*/graph.db`（pixcraft-app /
atagent / pixcraft-landing / vub）上跑出真实数据：

- dead-code only-low-tier-inbound reason 触发率为 0 / 0 / 0 / 0：
  edge_confidence.rs 把 `*_ast` indexer 边都判到 Medium，Low 只
  留给 AI-derive / override / ignored 三类罕见情况，因此 reason
  现在加得保守不会误报；待 v0.3.0-B / C 接 AI derive 后会自然激活。
- search Pass A 在 pixcraft-app `--kind dart_method` "build"
  上 75/100 触发，最高 score 100 → 130；样例 `_EditorScreenState.build`
  含 9 条 high-tier 出边。
- search Pass B 在 vub/"service" 30/30 cluster 触发；vub/"save"
  0 触发（命中分散在多个 package），证明 boost 只在真实邻接时给
  tie-break 信号，不会无差别加分。

报告在 `reports/release-v0.3.0a/README.md`，复现脚本在
`scripts/release_scan_v030a_metrics.py`。

### 14.4 不在 v0.3.0-A 范围内的遗留 bug

- `specslice-cli/src/commands/search.rs::parse_kind` 的 P20 补丁
  只覆盖 Dart / Swift / Go / Python 别名，**TypeScript / Java**
  NodeKind 别名缺失。`--kind typescript_function` / `--kind
  java_method` 在 CLI 本地解析器就被拒绝，尽管 engine 已经
  把它们列为 valid。Phase 5 报告里以"不带 --kind"绕过。
  → 留给 v0.3.0-B 或 P20 follow-up 补别名表。
