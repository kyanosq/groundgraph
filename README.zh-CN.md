<div align="center">

# SpecSlice

**面向 AI 编程的非侵入式「意图层」。**

SpecSlice 为代码库构建一张**带证据**的图——把需求、文档、测试与代码连起来——让 AI（和人）拿到**有依据**的上下文，而不是靠猜。它从不改动你的源码：所有状态都只写在 `.specslice/` 下。

[![CI](https://github.com/specslice/specslice/actions/workflows/ci.yml/badge.svg)](https://github.com/specslice/specslice/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.96-orange.svg)](rust-toolchain.toml)

[English](README.md) · **简体中文**

</div>

---

## SpecSlice 是什么？

大多数「代码智能」工具回答的是*「这个符号在哪」*。SpecSlice 还回答*「这段代码是**为了什么**、有什么能证明它」*。

它把仓库索引成一张 SQLite 图：**节点**（符号、文件、文档、需求、测试、路由、数据库表……）和**边**（调用、引用、实现、验证、持久化……），且每条边都带**证据**。在这张图之上，提供代码检索、影响分析、死代码检测、行为事实抽取，以及一套「AI 提候选 → 人确认」的业务逻辑沉淀流程。

- **非侵入（零写回）。** SpecSlice 绝不编辑、注解或提交你的代码。所有状态都是可重建的缓存，只在 `.specslice/` 下。
- **证据优先于断言。** 边由具体事实支撑（一个调用点、一处文档链接、一条测试引用），并带置信度——不是黑盒启发式。
- **AI 提候选，人确认。** 业务逻辑候选由代码/文档/测试事实生成，必须经人工审阅后才成为权威。
- **分层、多语言。** 进程内 tree-sitter 后端覆盖广度（Rust、TypeScript、Python、Go、Java、C、C++、Swift、C#、Ruby、PHP、Kotlin）外加 Dart 分析器 sidecar；**可选**的 SCIP/LSP 叠加层在你需要的地方补上精确的调用/引用边。

> SpecSlice **不是**更快的 grep。它是检索之上的一层：意图对齐、可追溯、文档/代码漂移。它能自举——SpecSlice 索引自己的 Rust 源码。

## 核心能力

- 🔎 **`search`** — 混合检索：结构打分（id/名称/路径/证据/邻接）**叠加 BM25 全文内容层**（代码正文、文档注释、markdown 正文），中英双语（CJK 二元组分词），每个命中附带一行定位片段。像 `byte boundary panic`、`错位竞争` 这类"词不在标识符里"的概念查询也能命中。
- 📋 **`check`** — 文档→代码漂移检测：文档引用了已不存在的路径/符号（`doc_stale_code_ref`）、孤儿需求自动给出**图上疑似实现**（`requirement_implementation_hint`）、声明链接断裂、缺验证测试。
- 🧭 **`trace`** — 端点 → 完整下游链路（controller → service → impl → SQL → 表）。
- 💥 **`impact`** — 一次 git diff 影响了哪些需求、文档、测试。
- 🪦 **`dead-code`** — 从任何入口都不可达的符号，带中文理由与置信度（绝不自动删除）。
- 🧪 **`facts` / `purity` / `constants` / `contract`** — 重构/移植用的确定性行为事实。
- 🧠 **`propose` / `candidate` / `logic`** — AI 业务逻辑证据包与人工审阅流程。
- 🔁 **`port-coverage` / `graph-equiv`** — 对照源图跟踪并证明一次重写/移植。
- 📊 **`dashboard`** — 单文件离线 HTML 管理面板：概览 / 业务模块 / 功能簇 / 检查 / 死代码 / 待澄清 / 纯度一页聚合，浏览器直接打开（`file://`），无服务、无 CDN。
- 🔌 **MCP 服务** — 通过 Model Context Protocol 把图暴露给 AI 智能体。

在多语言大型代码库上实战验证：Redis（C，约 20 万行）索引约 11 秒；TypeScript 编译器仓库（2 万+ 文件）约 28 秒——并行解析 + 单文件解析预算，能扛住带故意语法错误的 fixture 语料；Django（Python）、gin（Go）、gson（Java/Maven）端到端验证。SCIP 富集支持增量——源码未变时直接复用上次 `.scip`，免去重跑类型检查器；搜索排序对 tests/tools/examples 降权，issue 式查询优先命中生产代码（用 Redis 真实 issues 验证）。

## 安装

SpecSlice 是一个 Rust workspace。从源码构建（`rust-toolchain.toml` 已钉死工具链版本）：

```bash
git clone https://github.com/specslice/specslice.git
cd specslice

# 安装 CLI（`specslice`）与 MCP 服务（`specslice-mcp`）。
# `--locked` 会遵循已提交的 Cargo.lock，使构建可复现。
cargo install --locked --path crates/specslice-cli
cargo install --locked --path crates/specslice-mcp   # 可选，供 AI 智能体使用

# …或直接把二进制构建到 target/release/
cargo build --release
```

## 快速上手

```bash
cd /path/to/your/repo

specslice init                      # 生成 .specslice.yaml + .specslice/graph.db
specslice index                     # 把文档 + 代码索引进图

specslice search "parse sql tables" # 带证据的命中排序
specslice dead-code                 # 不可达符号 + 理由
specslice trace UserController      # 某端点的完整下游链路
specslice propose                   # AI 业务逻辑证据包（+ 中文提示词）
specslice dashboard                 # 单文件离线 HTML 管理面板
```

所有产物都只在 `.specslice/` 下。删掉该目录即可从头开始——你的源码永不被改动。若不想提交缓存，把 `.specslice/` 加进 `.gitignore`。

## 命令速查

完整且权威的列表请运行 `specslice --help`（或 `specslice <命令> --help`）。最常用的：

| 分类 | 命令 | 作用 |
| --- | --- | --- |
| **初始化** | `init`、`index` | 创建工作区；把文档 + 代码索引进图 |
| **导航** | `search`、`trace`、`graph`、`context`、`slice` | 找代码、追链路、渲染图、生成上下文包 |
| **总览** | `dashboard`、`features`、`stats` | 离线 HTML 管理面板；功能区聚类；命令账本 |
| **变更影响** | `impact`、`graph-diff`、`select-tests` | diff 影响面；图快照比对；该跑哪些测试 |
| **质量** | `dead-code`、`similar`、`check`、`questions` | 死代码、重复簇、一致性检查、待澄清问题 |
| **行为事实** | `facts`、`purity`、`constants`、`contract` | 分支/返回/空值、纯度普查、字面量目录、数据契约 |
| **业务意图** | `propose`、`candidate`、`logic`、`business-doc`、`connect` | 生成/审阅业务候选；渲染已确认文档 |
| **移植** | `port-coverage`、`route-coverage`、`graph-equiv`、`feature-pack`、`schema-index` | 对照源图跟踪重写并证明等价 |

> 只读命令绝不改动源码。`dead-code`、`similar`、`select-tests` 等只**报告**——绝不替你删除或执行任何东西。

## 语言支持

| 层级 | 机制 | 语言 |
| --- | --- | --- |
| 广度（默认） | 进程内 **tree-sitter** | Rust、TypeScript、Python、Go、Java、C、C++、Swift、C#、Ruby、PHP、Kotlin |
| Dart | 内置**分析器 sidecar**（领域感知：Riverpod / Hive / 导航 / 内购） | Dart |
| 文档 | Markdown / RST / AsciiDoc / 需求 / ADR | `.md`、`.mdx`、`.rst`、`.adoc` |

在 `.specslice.yaml` 的统一 `languages:` 选择器里选语言，再跑 `specslice index`。

### 可选精确层（SCIP）

为得到精确的 `Calls`/`References` 边，`index` 时 SpecSlice 会按语言自动调用已安装的 SCIP indexer 并摄取结果。这是**可选**的——没有它你仍得到完整的结构图。

| 语言 | indexer | 安装 |
| --- | --- | --- |
| Rust | `rust-analyzer scip` | `rustup component add rust-analyzer` |
| Go | `scip-go` | `go install github.com/sourcegraph/scip-go/cmd/scip-go@latest` |
| TypeScript | `scip-typescript` | `npm i -g @sourcegraph/scip-typescript` |
| Python | `scip-python` | `npm i -g @sourcegraph/scip-python` |

indexer 缺失或失败时只是一条清晰、非致命的「仅结构图」提示——绝不报错。可用 `SPECSLICE_SCIP_<LANG>_BIN`（如 `SPECSLICE_SCIP_RUST_BIN`）指定具体二进制。

> **对钉了工具链的 Rust 仓库的提示：** `rust-analyzer` 的 rustup 代理会按仓库的 `rust-toolchain.toml` 解析；若该工具链没有该组件，运行 `rustup component add rust-analyzer`（针对该工具链）或设置 `SPECSLICE_SCIP_RUST_BIN`。

## MCP 集成

`specslice-mcp` 是一个 [Model Context Protocol](https://modelcontextprotocol.io) 服务，把图（search、subgraph、dead-code……）暴露给 AI 智能体。让支持 MCP 的客户端指向该二进制：

```jsonc
{
  "mcpServers": {
    "specslice": {
      "command": "specslice-mcp",
      "args": ["--repo-root", "/path/to/your/repo"]
    }
  }
}
```

## 配置

`specslice init` 会写出可编辑的 `.specslice.yaml`，关键段：

```yaml
storage:
  path: .specslice/graph.db   # 图缓存（可重建）
docs:
  paths: [docs, specs, adr]   # 文档/需求所在目录
  include: ["**/*.md", "**/*.mdx", "**/*.rst", "**/*.adoc"]
languages:                    # 统一、canonical 的语言选择器
  - id: rust
    paths: [crates]           # 该语言要扫描的根目录
enrichment:
  scip: true                  # 存在时自动调用 SCIP indexer
  analyzer: true              # Dart 分析器 sidecar（配置了 Dart 时）
```

> 顶层 `languages:` 列表才是 canonical 选择器。旧写法 `treesitter.languages:
> [rust]` 仍作为**向后兼容别名**生效，但仅当 `languages:` 缺省时——不要两者同时
> 设置（normalisation 时存在的 `languages:` 会清空该别名）。

## 工作原理

```
crates/
├── specslice-core      # 图领域模型：节点、边、证据、id
├── specslice-store     # SQLite 存储 + 迁移（.specslice/graph.db）
├── specslice-engine    # 索引器、扫描器、检索、分析（大脑）
├── specslice-lang-dart # Dart 语言支持
├── specslice-cli       # `specslice` 二进制
└── specslice-mcp       # `specslice-mcp` MCP 服务
```

`index` 先跑结构化 pass（tree-sitter / Dart），再由可选的 SCIP 叠加层把精确边绑定到已存在的符号上。读命令打开存储后查询图——开库时会幂等地兜底建好性能索引，因此即便刚升级完二进制，查询依然很快。

## 开发

```bash
cargo fmt --all                                          # 格式化
cargo clippy --workspace --all-targets -- -D warnings    # 静态检查（零警告策略）
cargo test --workspace                                   # 1000+ 测试
```

- 工具链钉在 [`rust-toolchain.toml`](rust-toolchain.toml)；CI（`.github/workflows/ci.yml`）在每次 push 上强制 fmt + clippy（`-D warnings`）+ 测试。
- **测试驱动：** 新行为先写失败测试，再写让它通过的最小实现。
- 手写扫描器由 `proptest` 全域性测试守护（任意 UTF-8 → 不 panic、确定性）。
- 验收以**真实命令输出**为准，不以口头结论替代。

## 贡献

欢迎贡献——见 [CONTRIBUTING.md](CONTRIBUTING.md)。请保持零警告策略，并先写失败测试。

## 许可证

采用以下任一许可（二选一）：

- Apache License 2.0（[LICENSE-APACHE](LICENSE-APACHE)）
- MIT 许可（[LICENSE-MIT](LICENSE-MIT)）

除非你明确声明，否则你有意提交并被纳入本作品的任何贡献（按 Apache-2.0 定义），都将按上述双许可授权，不附加任何额外条款。
