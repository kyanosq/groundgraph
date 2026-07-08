# v0.3.0-A 实现计划

- **Spec:** `docs/superpowers/specs/2026-05-22-v030-a-confidence-plumbing-design.md`
- **Workflow:** TDD（先红后绿）每个阶段独立可绿；阶段间不引入未使用的 API。
- **Quality gates 每阶段都过:** `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`
- **Spec 微调（在实现中发现，会同步进 spec）:** confidence_view 暴露第三个纯函数 `summarize_edges(edges: &[EdgeAssertion], scope) -> EdgeQualitySummary`；`inbound_edge_quality` / `outbound_edge_quality` 内部走 `store.list_edges_*` → `summarize_edges`，dead-code 复用已在内存的 EdgeAssertions 直接调 `summarize_edges` 避免重复 sqlite 查询。

## Phase 1 — confidence_view 模块（含 EdgeKind 矩阵）

**目标:** 新建 `crates/groundgraph-engine/src/confidence_view.rs`，导出
`EdgeQualityScope` / `EdgeQualitySummary` / `summarize_edges` /
`inbound_edge_quality` / `outbound_edge_quality` /
`NeighborInfo` / `neighbors_of`。

**TDD 顺序:**

1. **🔴 写测试，先编译失败 (模块不存在)**
   - `usage_scope_excludes_contains_imports_derives_from`
   - `usage_scope_allows_calls_references_reads_provider_persists_to_navigates_to_subscribes_stream_declares_verification`
   - `search_ranking_scope_excludes_same_set_as_usage_in_v030_a`
   - `summarize_edges_counts_by_tier`
   - `dominant_breaks_ties_in_favor_of_high_then_medium_then_low`
   - `is_only_low_true_when_only_low_evidence`
   - `is_empty_true_when_no_allowed_edges`
   - **EdgeKind 矩阵:** `all_edge_kinds_have_explicit_scope_decision` —
     遍历 `EdgeKind` 所有变体，对 `Usage` 和 `SearchRanking` 显式断言
     允许 / 排除。
2. **🟢 实现枚举 + 结构 + `scope.allows(kind)` 私有函数 + 纯函数 `summarize_edges`**
3. **🔴 store 查询测试**：建临时 store，喂 high/medium/low 三档边，断言：
   - `inbound_edge_quality_counts_by_tier`
   - `outbound_edge_quality_counts_by_tier`
   - `inbound_edge_quality_excludes_deprecated_edges`
   - `missing_node_returns_empty`
4. **🟢 实现 `inbound_edge_quality` = `store.list_edges_to(aid)` → `summarize_edges`；outbound 对称**
5. **🔴 neighbors_of 测试**：
   - `neighbors_of_dedup_by_other_endpoint`
   - `neighbors_of_sorts_alphabetically_and_caps`
   - `neighbors_of_excludes_self_loops`
6. **🟢 实现 `neighbors_of`：合并 `list_edges_from + list_edges_to` →
   按对端 id BTreeMap 去重 → 取前 cap → `Vec<NeighborInfo>`**
7. **🟢 验收门槛**：fmt / clippy / `cargo test -p groundgraph-engine confidence_view`

**预计产物:**
- 新增：`crates/groundgraph-engine/src/confidence_view.rs`（~250 行 + 测试）
- 修改：`crates/groundgraph-engine/src/lib.rs`（`pub mod confidence_view;`）

**完成判据:** 矩阵 + 计数 + 邻接 共 ~12 单测全绿；workspace 仍全绿。

---

## Phase 2 — dead_code 接 evidence_quality（只动 reason，不动 BFS）

**目标:** 在 `classify(...)` 末尾，对 high/medium 桶的 candidate
追加一行 "仅有低置信入边" reason；`DeadCodeReport.warnings` 字段
落地，按 `impact::ImpactReport` 同型范式。

**TDD 顺序:**

1. **🔴 测试**：
   - `high_bucket_reason_mentions_low_tier_inbound_when_only_low_evidence_exists`
     建一个节点 + 一条 `python_ast Calls` 入边（→ low tier），断言
     reasons 里有 "仅有 N 条 low-tier 入边" 子串。
   - `existing_reason_strings_are_preserved_when_only_high_tier_inbound`
     建节点 + 一条 `python_lsp Calls` 入边（→ high tier），断言
     reasons 里**不含**新行，老 reasons 原样保留。
   - `bfs_reach_set_unchanged_when_low_tier_edges_present`
     用同一个 fixture 跑两遍：旧路径 reach 集合 = 新路径 reach 集合。
     （直接调 BFS helper；本测试是回归护栏，不验证 reason 字符串。）
   - `dead_code_report_warnings_field_serializes_with_skip_if_empty`
     空 warnings 时 JSON 不出现 `"warnings"` 键，非空时出现。
2. **🟢 实现**：
   - 给 `DeadCodeReport` 加 `pub warnings: Vec<String>` +
     `#[serde(default, skip_serializing_if = "Vec::is_empty")]`。
   - 在 `classify(...)` 之后（或就在 inbound_sources 构造区域附近）
     调 `summarize_edges(inbound_for, EdgeQualityScope::Usage)` →
     若 `is_only_low()` 且 candidate confidence != Low → push reason。
   - warnings 当前 plumbing：dead-code 不会触发 sqlite 错（用的是
     in-memory edges），所以 warnings 字段先**保留为空 Vec**，只锁
     字段存在性 + 向后兼容序列化；将来若做按需 sqlite 查询再激活。
3. **🟢 验收**：fmt / clippy / `cargo test -p groundgraph-engine dead_code`

**完成判据:** 4 新单测绿 + 既有 dead_code 测试 100% 不动也仍绿。

---

## Phase 3 — search 接 evidence_quality + neighbor boost + warnings

**目标:** 在 `run_search_with_store` 内、`matches.sort_by` 之前，
插入 Pass A（edge evidence boost）+ Pass B（neighbor boost, capped）；
`SearchResult.warnings` 落地。

**TDD 顺序:**

1. **🔴 测试**（在 `crates/groundgraph-engine/src/search.rs::tests`）：
   - `score_match_includes_edge_evidence_boost_when_high_tier_outbound_exists`
   - `score_match_does_not_include_edge_evidence_boost_when_only_medium_low_outbound`
   - `neighbor_boost_caps_at_single_increment_per_hit`
   - `neighbor_boost_is_symmetric_between_two_adjacent_hits`
   - `neighbor_boost_zero_when_no_hit_is_adjacent_to_another_hit`
   - `neighbor_reason_lists_at_most_two_neighbor_names`
   - `neighbor_reason_appends_等_when_more_than_two_matches`
   - `search_result_warnings_field_serializes_with_skip_if_empty`
2. **🟢 实现**：
   - 给 `SearchResult` 加 `pub warnings: Vec<String>` + 同型 serde attr。
   - 在 `run_search_with_store` 内 `matches.sort_by` 之前：
     - **Pass A:** 对每个 hit 调
       `confidence_view::outbound_edge_quality(store, &hit.id, SearchRanking)`；
       若 `summary.high >= 1` → `hit.score += SCORE_EDGE_EVIDENCE` +
       push reason。出错就 push warning，跳过 boost。
     - **Pass B:** 先构造 `hit_ids: HashSet<String>`，对每个 hit 调
       `neighbors_of(store, &hit.id, 8)`：取 matched (filter hit_ids ∩
       neighbors)，`matches_total = matched.len()`，最多 +1 次
       SCORE_NEIGHBOR，reason 只列前 2 个 name，>2 加 "等"。
   - **engine 层任何 stderr 都禁止** — 改 push 进 `result.warnings`。
3. **🟢 验收**：fmt / clippy / `cargo test -p groundgraph-engine search`

**完成判据:** 8 新单测绿 + 既有 search 测试全绿。

---

## Phase 4 — JSON / MCP / CLI 透传 warnings

**目标:** 三路输出渠道都把 `SearchResult.warnings` /
`DeadCodeReport.warnings` 透传给操作者。

**TDD 顺序:**

1. **🔴 测试**：
   - `cli_dead_code_human_renders_warnings_when_present`
     （在 `crates/groundgraph-cli`）
   - `cli_search_human_renders_warnings_when_present`
   - `mcp_search_graph_passes_through_warnings`
     （在 `crates/groundgraph-mcp` 的 tools 测试中）
   - `mcp_dead_code_passes_through_warnings`
2. **🟢 实现**：
   - CLI human renderer：在 dead-code / search 的人类输出末尾，
     若 warnings 非空，打印 `## Warnings` 段。
   - MCP：tool 输出 JSON 自然透传（serde derives 自动覆盖）；只需
     测试断言。
3. **🟢 验收**：fmt / clippy / `cargo test --workspace`

**完成判据:** 4 新集成测试绿 + workspace 全绿。

---

## Phase 5 — empirical 收口：4-repo release-scan diff

**目标:** 用 `scripts/release_scan.sh` 跑前/后两次 4-repo 扫描，
`scripts/diff_release_scans.sh` 输出 dead-code reason diff +
search top-10 diff，写到 `reports/release/v0.3.0-A.md`。

**步骤:**

1. **基线 (before)**：`git stash` 待实现的改动，跑
   `bash scripts/release_scan.sh` 把 4 仓扫到 `reports/release-before/`。
2. **新建 `scripts/diff_release_scans.sh`** — 输入 before / after 两个
   报告目录，输出：
   - 每仓 dead-code 桶差异（+entries / -entries / +suffix 'low-tier 入边'）
   - 8 个固定 query 的 top-10 排序差异（标注 +EDGE_EVIDENCE / +NEIGHBOR）
3. **恢复改动 (after)**：跑 `bash scripts/release_scan.sh` 扫到
   `reports/release-after/`，再跑 diff 工具。
4. **报告**：`reports/release/v0.3.0-A.md` 入库；scratch 不入库。

**完成判据:** 报告文件存在；至少展示
- 1 个 candidate 多了 "low-tier 入边" reason（pixcraft-app 上下文）
- 1 个 search 命中因为 EDGE_EVIDENCE 上升 ≥ 30 分
- 1 个 search 命中因为 NEIGHBOR 上升 = 20 分

---

## Phase 6 — 文档同步与 sub-project 收尾

1. `docs/implementation-plan.md` 增 v0.3.0-A 章节，链接 spec + 本文件
   + 报告。
2. `packaging/skills/groundgraph/SKILL.md` 增"evidence_quality 现已影响
   dead-code reason 与 search ranking"段。
3. `docs/superpowers/specs/.../design.md` 添加一个 "Implementation
   addendum: summarize_edges helper" 小段，把第三个 API 入档（避免
   spec 与代码漂移）。
4. 最终验收命令一次性跑：
   ```
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   dart test  # 在 tool/groundgraph_dart_analyzer/
   ```
5. 中文 commit 信息分阶段落（每个 Phase 一个 commit，保持 history
   可二分查找）。

---

## Risk register

- **R1 — `list_edges_from` / `list_edges_to` 内部已对 deprecated
  做过滤？** 需要 Phase 1 step 3 用 deprecated 边核实；若没过滤，
  `summarize_edges` 内部做 `if status == Deprecated continue`。
- **R2 — search 端到 4 仓的 neighbor pass 性能？** 4 仓最大的是
  vub（~3000 Java 文件）；neighbor pass 是 hits × `list_edges_*`
  两次查询；hits 上限 25 个，每次查询 < 1ms 量级，总耗 < 200ms 可接受。
  若 phase 5 实测超 1s，加 cap=8 已经写在 spec 里，必要时降到 cap=4。
- **R3 — 已有 dead_code 测试里有断言 `reasons.len() == N` 的吗？**
  Phase 2 step 1 要先 grep；若有，要么改成 `>= N`，要么调整产生
  条件不让新 reason 触发；本规划假设可以追加。
- **R4 — MCP 输出 schema 是否锁版本？** 若 MCP tools 文档写死了
  `dead_code` / `search_graph` 的字段集，新增 warnings 是向后兼容
  扩展（skip_if_empty 保证旧字段集仍是 superset），但 phase 4
  test 要明确覆盖。

每个 Phase 完成时把这一节的 risk 项打勾或更新。
