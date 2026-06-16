# CLAUDE.md

> 本文件给 Claude Code（或任何 AI pair-programmer）提供 SpecSlice 项目的协作约定与上下文。新 session 开始时优先读此文件。

## 项目概况

**SpecSlice** 是一个非侵入式的代码上下文层工具：在目标仓库的 `.specslice/` 目录下构建代码图谱（nodes/edges/evidence），为 AI coding agent 提供搜索、影响分析、依赖追踪、业务候选生成等能力。

- **语言**：Rust（workspace 6 crates）+ Dart sidecar + webui（vanilla JS + 3d-force-graph）
- **核心约束**：禁止 `unsafe`、零 clippy 警告、非侵入式（只写 `.specslice/`）、测试驱动（TDD）、commit 用中文
- **MSRV**：1.89（`File::lock`）；pinned toolchain 1.96（见 `rust-toolchain.toml`）
- **规模**：约 91,750 行 Rust，支持 12 门 tree-sitter 语言 + Dart/SCIP overlay

## 协作约定

### 代码审查工作流

SpecSlice 采用**多轮并行 agent 审查**模式，由用户驱动每轮的"找新问题→复核→归档"循环：

1. **派 agent**：用户说"找新的 N 个"，主审查派 3-5 个并行 agent（按模块或角度分工），每个 agent 拿到前几批的概要清单避免重复
2. **交叉去重**：主审查汇总各 agent 候选，去除与前批重复的条目，挑选达到目标数量
3. **写入 issues 文件**：新发现追加到对应批次的 issues 文件
4. **用户处理**：用户在代码层处理（TDD 修复 / 标按设计 / 标误报 / 延后专项）
5. **复核归档**：用户说"已处理 你复核"，主审查验证代码层落实情况，把已闭环项移到 archive 文件，issues 主文件只留活跃项

### issues 文件组织

| 文件 | 角色 | 内容 |
|---|---|---|
| `issues.md` 唯一审查文件（活跃 #63-#240 + 归档附录 #1-#80）** | 所有未处理问题 #63–#240（150 个），按编号排序 |
| `issues.md` 归档附录部分 |
| `CLAUDE.md` | 协作约定 | 本文件——审查方法论、约束基线、常用命令 |

**活跃总计**：150 个（截至 2026-06-13）。已闭环归档总计：103 个（30 + 30 + 18 + 12 + 13）。

### 编号规则

- 每个问题一个**全局唯一编号**，从 #1 开始连续递增，不重用
- 跨批次连续编号（第一批 1–30，第二批 31–60，… 第六批 181–210）
- 已处理项**保留编号**（归档不释放编号），便于跨文件引用

### 严重度分级

| 级别 | 标准 |
|---|---|
| **High** | 生产可触发，影响数据正确性或安全 |
| **Medium** | 边界条件、显著性能/设计缺陷 |
| **Low** | 性能微优化、潜在隐患、重构脆弱性 |

### 每个问题的标准格式

```markdown
### N. 简短标题

- **位置**：`crates/.../foo.rs:line_start-line_end`
- **问题**：具体描述（含触发场景）
- **建议**：修复方向
- （可选）简短代码片段
```

### 复核时的判定类别

| 判定 | 含义 |
|---|---|
| **成立，已 TDD 修复** | 代码已改，有失败测试先行 |
| **按设计** | 现状是有意为之（如前瞻性变体、性能权衡） |
| **误报** | 前提算术错、不可达分支、与代码实际行为不符 |
| **延后专项** | 成立但需独立 PR（如 serde_yaml 迁移涉 30 文件） |
| **已被先前修复覆盖** | 与另一已修项同源 |

## 关键约束基线（审查时据此判断违规）

- **禁止 `unsafe`**（grep `unsafe` 应只在 `target/` 命中）
- **零 clippy 警告**（`cargo clippy --workspace --all-targets -- -D warnings`）
- **非侵入式**：SpecSlice 必须只在目标仓库的 `.specslice/` 下写入。任何 `std::fs::write` / `Command::new` 路径要重点审查
- **测试驱动**：新增行为必须先写失败测试。bug 修复必须先写复现测试
- **手写扫描器必须 total**：任意 UTF-8 输入不 panic 且确定性（见 `crates/specslice-engine/tests/p25_scanner_totality_proptest.rs`）
- **多语言覆盖**：12 门 tree-sitter 语言（rust/typescript/python/go/java/swift/c/cpp/csharp/ruby/php/kotlin）+ Dart/SCIP overlay。新增语言必须接入 `call_idents_of` 与 `every_language_spec_opts_into_the_call_resolver` 守门测试

## 常用命令

```bash
# 全量门检（CI 同款）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 大型仓库性能基准（spring-framework / django / TypeScript compiler）
# 见 docs/codegraph-benchmark-and-roadmap.md

# 单测特定层
cargo test --workspace --test p23_totality_proptest   # 扫描器 totality
cargo test --workspace --test p25_scanner_totality_proptest
cargo test --workspace --test repositories           # store 解码矩阵
```

## 已知活跃问题高优先级 Top 10（跨所有批次）

| 优先 | 编号 | 文件 | 一句话 |
|---|---|---|---|
| 1 | #81 | issues.md | release_scan rsync 实际已同步 `OPENAI_API_KEY` 进 scratch，需立即清理 |
| 2 | #82 | issues.md | 🟢 已收口：macOS codesign/notarytool/stapler 脚本框架就位，待发布者填 Developer ID 证书（env 门控，非代码阻断） |
| 3 | #133 | issues.md | dist README 指导用户配置已退役 LSP |
| 4 | #138 | issues.md | ✅ 已闭环：rebuild 后 `node_fts(optimize)` + 回归测试，BM25 不再随段累积退化 |
| 5 | #139 | issues.md | 缺 `PRAGMA foreign_keys=ON`（延后，需 table-rebuild） |
| 6 | #181 | issues.md | git_diff u32 溢出，一行恶意 diff header 即 panic |
| 7 | #184 | issues.md | `time 0.3.41` CVE-2026-25727，`cargo update` 零改动 |
| 8 | #187 | issues.md | `.specslice.yaml` `*_command` 字段无白名单 = 配置 RCE |
| 9 | #186 | issues.md | docs/lsp/treesitter 多处 `read_to_string` 无大小上限 |
| 10 | #70 | issues.md | ✅ 已闭环：迁移 `serde_yml 0.0.13`（noyalib 后端），`serde_yaml`/`unsafe-libyaml` 已从 Cargo.lock 移除 |

## 不要做的事

- 不要在 `issues*.md` 主文件追加已处理项的细节——已闭环项的 verdict 写入对应 `issues*-archive.md`
- 不要重用已归档的编号
- 不要跳过 clippy（即便"只是测试代码"）
- 不要给 `.specslice/` 之外的路径写文件（除非用户显式 `--output` 授权）
- 不要给 commit 用英文（项目约定中文 commit message）

## 更新此文件

当审查工作流、文件组织、或约束基线发生变化时，更新此 CLAUDE.md。新 session 开始时此文件会被自动加载。
