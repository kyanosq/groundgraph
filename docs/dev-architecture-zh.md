# GroundGraph 开发文档（自举生成）

> **本文档由 GroundGraph 用自身的能力对自身仓库分析后生成**（dogfood / 自举）。
> 所有结构数字都来自真实命令输出与 `.groundgraph/graph.db`，不是人工估算。
>
> - 生成时间：2026-06-15
> - 工具版本：`groundgraph 0.2.0`（本地 `cargo build --release` 产物 `./target/release/groundgraph`）
> - 解析后端：`rust_treesitter`（本仓为纯 Rust 工作区，`enrichment.lsp/analyzer = false`）+ Rust SCIP overlay
> - 复现方式见文末「附录 A：一键复现」

---

## 1. 这份文档怎么来的

GroundGraph 是一个**非侵入式代码上下文层**：它把仓库事实读进外部图谱 `.groundgraph/graph.db`，只写 `.groundgraph/`，不碰任何源代码。本文档就是先对本仓 `index`，再用 `search / facts / purity / dead-code / features / logic / contract / graph` 等命令把图谱事实抽出来汇总而成。

新贡献者上手顺序建议：先读第 3 节（分层）→ 第 5 节（功能区）→ 第 9 节（开发工作流），需要定位代码时用第 6 节的 `search` 代替 `grep`。

---

## 2. 项目规模（真实计数）

| 指标 | 数值 | 来源 |
|---|---|---|
| 源码行数（`crates/*/src`，不含 tests 目录） | **85,330** | `find crates/*/src -name '*.rs' \| cat \| wc -l` |
| 含测试目录总行数 | **99,272** | 同上含 `tests/` |
| 已索引 Rust 文件 | 189 | `index` 报告 |
| Rust 符号节点 | 2,479 | `index` 报告 |
| import 边 | 676 | `index` 报告 |
| 文档文件 / DocSection | 22 / 389 | `index` 报告（docs/ 目录）|
| 全文检索内容节点 | 4,135 | `index` 报告（fulltext 层）|
| SCIP overlay 边（调用/引用富化） | 12,290 | `index` 报告 |
| 测试用例节点（`test_case`） | **1,268** | `graph.db` |

**观察**：`test_case`(1,268) 与 `rust_function`(1,558) 接近 1:1.2，反映了项目的 TDD 文化（CLAUDE.md/AGENTS.md 明确要求新增行为先写失败测试）。

---

## 3. 架构分层（6 个 crate）

按 Rust 符号数与源码行数排序（符号数来自 `graph.db`，行数为净计数）：

| crate | 符号 | 源码行 | 文件 | 职责（一句话） |
|---|---|---|---|---|
| **groundgraph-engine** | 1,795 | 63,959 | 75 | 索引与分析引擎：12 门 tree-sitter + SCIP overlay + Dart sidecar、checks/dead_code/impact/features/constants/purity/data_contract/port_coverage/graph_equiv/trace |
| **groundgraph-cli** | 363 | 10,966 | 38 | `clap` 命令行：每个子命令一个 `commands/*.rs`，负责参数解析与人读/JSON 输出 |
| **groundgraph-mcp** | 100 | 3,169 | 13 | MCP（Model Context Protocol）JSON-RPC server，把图谱能力暴露给 AI agent |
| **groundgraph-core** | 87 | 2,442 | 7 | 无 IO 的纯数据/契约层：`ArtifactId`、`EdgeAssertion`、`NodeKind`、`Language` traits、`LanguageIndexBatch` |
| **groundgraph-store** | 80 | 2,077 | 3 | SQLite 持久化：`migrations`（迁移）+ `repositories`（图谱读写），`graph.db` 的唯一入口 |
| **groundgraph-lang-dart** | 53 | 2,717 | 3 | Dart 语言支持（tree-sitter + analyzer sidecar 胶水）|

依赖方向（自底向上）：`core` ← `store` ← `engine` ← {`cli`, `mcp`}；`lang-dart` 被 `engine` 调用。`core` 不依赖任何兄弟 crate（是契约根）。

**engine 是绝对核心**：占 75% 源码行、72% 符号。绝大部分改动会落在这里。

---

## 4. 图谱事实（节点与边的分布）

### 节点种类（`graph.db`，全部 indexer 合并后）

| kind | 数量 |
|---|---|
| rust_function | 1,558 |
| test_case | 1,268 |
| doc_section | 389 |
| rust_struct | 373 |
| rust_module | 259 |
| rust_method | 215 |
| file | 211 |
| rust_enum | 72 |
| db_table | 24 |
| requirement | 18 |
| http_route | 2 |
| rust_trait | 1 |

**观察**：`rust_trait` 只有 1 个，`rust_struct`(373)+`rust_enum`(72) 远多于 trait——这是一个**数据导向 / 函数式内核**的代码库（用结构体+枚举+自由函数，而非 trait 抽象层叠）。这与第 7 节「67% 纯函数」互相印证。

### 边种类（`edge_assertions` 表）

| kind | 数量 | 含义 |
|---|---|---|
| references | 6,253 | 符号引用（SCIP overlay 富化）|
| calls | 6,037 | 调用关系 |
| contains | 4,135 | 包含（文件/模块→符号）|
| imports | 580 | 导入 |
| persists_to | 43 | 内联 SQL → 表（数据层）|
| declares_implementation | 27 | 接口→实现 |
| declares_verification | 21 | 需求→验证测试 |
| documents | 18 | 文档→需求 |

`calls`+`references` 合计 1.2 万余条，主要由 Rust SCIP overlay 提供（精度高于纯 tree-sitter 启发式）。

---

## 5. 模块功能区（`features` 启发式聚类）

`features` 命令对图谱做功能区聚类：**157 个种子 → 17 个簇**，已归属 1,229 个节点（未归属 1,450，多为叶子工具函数）。前若干簇即 engine 的主功能边界：

| 簇 | 种子文件 | 节点 | 这一区在做什么 |
|---|---|---|---|
| #1 | `dart_indexer.rs` | 276 | Dart 索引 + sidecar（`dart_sidecar.rs`/`dart_treesitter.rs`/`lang-dart`）|
| #2 | `checks.rs` | 152 | 图一致性检查（drift / 断链 / 缺测试），含 `impact.rs` |
| #3 | `business_pack.rs` | 130 | 业务包/模块切分（`feature_cluster.rs`）|
| #4 | `business_candidates.rs` | 116 | AI 业务逻辑候选的解析/评审状态机 |
| #5 | `business_doc.rs` | 111 | 已确认候选 → 业务文档渲染 |
| #6 | `dead_code.rs` | 92 | 死代码候选分类（high/medium/low）|
| #7 | `connect.rs` | 51 | 证据包（evidence pack）/ propose-apply |
| #8 | `data_contract.rs` | 43 | CREATE TABLE schema + `obj["key"]`/`.get()` 契约挖掘 |
| #12 | `artifact_id.rs` | 34 | 稳定 ID 生成（fnv1a64 + 各语言命名）|
| #13 | `language_traits.rs` | 30 | 语言无关 trait（`SymbolFamily`/`is_callable`…）|
| #14 | `constants.rs` | 30 | 字面量/魔法值抽取 |
| #15 | `edge.rs` | 27 | 边模型（`EdgeKind`/`sanitize_confidence`）|
| #16 | `context_pack.rs` | 18 | 上下文包（给 AI 的自包含片段）|
| #17 | `git_diff.rs` | 9 | git diff 解析（含 `hostile_hunk_header_does_not_overflow` 防溢出测试）|

> 注意：聚类是**启发式**结果，不是权威功能划分；接入 LSP/框架事实后质量会提升。

---

## 6. 关键入口与调用链（用 `search` 代替 grep）

定位代码请优先用图谱搜索。例如核心索引入口：

```bash
groundgraph search index_repository
```

实测结果（节选）：

```
[1] index_repository (rust_function)  分数=185
    路径: crates/groundgraph-engine/src/index.rs:199-525
    片段: L199: pub fn index_repository(options: IndexOptions) -> Result<IndexResult>
    出边 evidence_quality=high (55 条)
[2] run (rust_function)
    路径: crates/groundgraph-cli/src/commands/index.rs:7-94
    片段: L13: let result = index_repository(options)?;
```

即：CLI `index::run`（`crates/groundgraph-cli/src/commands/index.rs`）→ 引擎 `index_repository`（`crates/groundgraph-engine/src/index.rs:199`）是整个索引流水线的主入口。搜索结果里的「内容层分词: index, repository」说明全文层把 `index_repository` 拆成了 `index`/`repository` 子词参与匹配。

要看一个端点/函数的**完整下游链**（而非 1 跳邻居），用 `trace`：

```bash
groundgraph trace index_repository --depth 14
```

---

## 7. 代码健康度

| 维度 | 结果 | 命令 |
|---|---|---|
| **纯度** | 分析 1,773 个可调用符号：**纯 1,195（67%）/ 有副作用 578 / 未知 0** | `purity` |
| **死代码** | 总符号 2,478 · 入口 1,329 · 可达 2,388 · **可能死 0 · high 候选 0** | `dead-code --min-confidence high` |
| **图一致性** | **0 error / 0 warning** | `check` |
| **逻辑可信度** | 需求已确认 18 · 缺测试 0 · 其余 0 | `logic --only-risks` |

> 「分析 1,773」少于「符号 2,479」是正常的——`purity`/`facts` 只分析有函数体的可调用符号，不含类型/模块/文件节点。

`check` 当前为零告警；需求映射中的 `REQ-P18-SIMILARITY` 与
`REQ-P19-SELECT-TESTS` 已链接到各自模块内的验证测试。

`facts` 可看任意函数的行为骨架（分支/循环/return/比较/空值/抛出/await + 决策证据行 + 纯度），移植/重构时用来补「图里没有的行为」。

---

## 8. 数据层

本仓自身用 SQLite 存图谱。`schema-index` 从迁移 SQL 抽出 **24 个 `db_table` 节点 / 87 列**；核心表见 `crates/groundgraph-store/src/migrations_sql/`：`nodes`、`edge_assertions`、`evidence`、`symbol_ranges`、`file_index`、`node_fts*`（FTS5 全文）、`schema_version`、`slice_cache`。

`contract` 命令扫描 186 个源文件，识别 21 张表 / 134 个去重 JSON 键 / 328 处键引用——其中多数表名来自 `schema_indexer.rs` 内**内嵌的测试夹具 SQL**（这正是引擎用来验证自己 SQL 解析的样本），真实持久化表是 `migrations.rs` 里的 `schema_version` 等。

---

## 9. 开发工作流

### 构建与运行

```bash
cargo build --release                 # 产物 ./target/release/groundgraph
# 或安装到 PATH（canonical）：
cargo install --path crates/groundgraph-cli --force
```

### 全量门禁（CI 同款，提交前必过）

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # 零警告硬门禁
cargo test --workspace
```

### 硬约束基线（来自 CLAUDE.md / AGENTS.md）

- **禁止 `unsafe`**（workspace `unsafe_code = "forbid"`）
- **零 clippy 警告**（即使是测试代码）
- **非侵入式**：只允许写目标仓的 `.groundgraph/`；任何 `std::fs::write` / `Command::new` 到其它路径需重点审查
- **测试驱动**：新增行为先写失败测试，bug 修复先写复现测试
- **手写扫描器必须 total**：任意 UTF-8 输入不 panic 且确定性（见 `tests/p25_scanner_totality_proptest.rs`）
- **多语言守门**：新增 tree-sitter 语言必须接入 `call_idents_of` 与 `every_language_spec_opts_into_the_call_resolver` 守门测试
- **Commit 用中文**

### MSRV / 工具链

- MSRV 1.89（`File::lock`）；pinned toolchain 见 `rust-toolchain.toml`

---

## 10. 已知风险 / 待办

| 项 | 来源 | 说明 |
|---|---|---|
| 多语言 fixture 配置漂移 | `index` | `.groundgraph.yaml` 显式覆盖本仓 fixture、WebUI 与脚本语言；`release-scans/` 外部快照在自动检测中跳过 |
| 发布签名 #82 | issues.md | macOS codesign/notarytool 脚本框架已就位，待填 Apple Developer ID 证书 |

更细的活跃问题清单见 `issues.md`，协作约定见 `CLAUDE.md`。

---

## 附录 A：一键复现

在仓库根目录执行（需先 `cargo build --release`）：

```bash
SS=./target/release/groundgraph

$SS index                         # 建图（写 .groundgraph/graph.db）
$SS check                         # 图一致性
$SS stats                         # 命令统计账本
$SS purity                        # 纯度普查
$SS dead-code --min-confidence high
$SS features                      # 功能区聚类
$SS logic --only-risks            # 逻辑可信度
$SS contract                      # 数据契约
$SS search index_repository       # 图谱搜索（grep 替代）
$SS facts --max 5                 # 行为事实采样
$SS graph --format html --view overview   # → .groundgraph/export/graph.html

# 结构分布（直接查 graph.db）
sqlite3 .groundgraph/graph.db "SELECT kind, COUNT(*) FROM nodes GROUP BY kind ORDER BY 2 DESC;"
sqlite3 .groundgraph/graph.db "SELECT kind, COUNT(*) FROM edge_assertions GROUP BY kind ORDER BY 2 DESC;"
```

## 附录 B：可视化产物

| 文件 | 说明 |
|---|---|
| `.groundgraph/export/graph.html` | WebGL 力导向「星座图」全图拓扑（10 MB，浏览器打开）|
| `.groundgraph/export/dashboard.html` | 仪表盘视图 |
| `.groundgraph/export/nodes.jsonl` / `edge_assertions.jsonl` | 图谱快照（CI artefact / diff 用）|
