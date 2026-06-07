# ADR-0001：SCIP 互通与 tree-sitter-stack-graphs 跨文件符号解析

- 状态：已接受（Accepted）并**持续落地**——**R1 SCIP 摄入已实现**（首发语言 Rust，本仓 dogfood 验证，见 §6/§8.7）；**R2 已推进**：`specslice index` 自动调用 PATH 上的 SCIP indexer，确立「**SCIP 权威 + 启发式补空**」精度模型，并据此**退役 go/python/ts/java 的实时 LSP**（仅 Swift 因缺成熟 SCIP indexer 保留 sourcekit-lsp，见 §8.8）；R4 stack-graphs 进程内方向因上游 `github/stack-graphs` **已归档**（2026-06-07 复核：`archived:true`、末次提交 2025-09-09）而**永久退役**，SCIP 摄入成为唯一精度推进路径，见 §8 书面例外。
- 日期：2026-05-31（初稿，§8.1–8.5 验证）；2026-06-07（R1 落地 + §8.6 归档复核与 R4 退役）；2026-06-04 复核与 R2 推进（§8.8：自动调用、权威补空、LSP 退役、Python 上游限制）
- 关联：P23 收敛与成熟化 Epic（诉求 #2「去多套实现 / 摆脱外部 LSP 脆弱性」、#3「SCIP / stack-graphs 是否自研」）
- 上游事实：`docs/codegraph-benchmark-and-roadmap.md` §5.1（三层后端）已将 SCIP/stack-graphs 记为「未来 Tier 2.5/Tier 3 精度增强候选」；本 ADR 把它从「记在案」升级为「有明确接口、绑定策略与验收门槛的决策」。

## 1. 背景与问题

SpecSlice 当前的结构来源已收敛为**唯一的 tree-sitter 通用驱动**（`crates/specslice-engine/src/treesitter.rs`，P22/P23）。精度层（按符号 id 叠加 `Calls`/`References`）则在 R1/R2（2026-06，§8.7/§8.8）收敛为「**SCIP 摄入为权威 + tree-sitter 启发式补空**」，实时 LSP 仅 Swift 例外保留、Dart analyzer 保留为 Dart 的 overlay。本 ADR 撰写时（2026-05-31）这两个缺口尚未解决，以下背景即据此而立：

1. **跨文件精确符号解析**：tree-sitter 只懂语法，不懂「这个 `foo()` 到底解析到哪个定义」。跨文件、跨包、考虑可见性/导入别名/泛型的「定义→引用」精度，tree-sitter 天生做不到。
2. **Tier-3 依赖外部 LSP 进程**（诉求 #2 的痛点）：实测 Dart 22.9s、Swift ~80s 且大仓超时；要求外部二进制在 PATH、环境脆、CI 难复现。把「精度层」绑死在「实时 LSP 子进程」上，是当前最大的工程债。

对标项目 CodeGraph 的边模型里 `provenance` 已是 `'tree-sitter' | 'scip' | 'heuristic'` 三态（见 `docs/sourcecode/codegraph/src/types.ts`）——即**它已经把 SCIP 当作精度边的一等来源**。要「替代并增强 CodeGraph」，我们必须给出对等或更优的精度层方案。

本 ADR 回答三个问题：
- SCIP 应该**导出**（让别人消费我们的图）还是**摄入**（消费别人的精确索引）？还是都做？
- stack-graphs 能否让我们**在进程内、无外部 LSP** 地做到跨文件精确解析（即诉求 #2 的「终极解」）？
- 现在要不要实现？以什么顺序、什么验收门槛？

## 2. 决策驱动因素

- **D1 非侵入式不变量**：任何方案都只能向 `.specslice/` 写入；绝不在目标仓库落地索引产物或注解。
- **D2 单一结构来源**：精度层只能**叠加语义边**到既有 tree-sitter 结构节点上，绝不引入第二套结构/第二套 id（P23 的核心教训）。
- **D3 分发优势**：保持「零运行期外部依赖 + 单静态二进制」。任何「要求用户先装一个重型 SDK / 守护进程」的方案都要降权。
- **D4 确定性与可信度分级**：每条边都带 `indexer` 标记与 evidence；精度来源（SCIP/stack-graphs/LSP）天然比启发式高一档。
- **D5 生态成熟度**：优先复用社区已成熟的索引器，避免「每语言自写 stack-graph 规则」的长尾工程。

## 3. 关键技术结论

### 3.1 SCIP 是数据格式，不是解析器

SCIP（Sourcegraph Code Intelligence Protocol）是一份 **protobuf 定义的「符号 + 出现位置（occurrences）+ 关系」索引格式**，是 LSIF 的后继。它本身不解析代码，而是由各语言的 **SCIP indexer** 产出：

- `scip-typescript`（TS/JS，基于 TS Compiler API，成熟）
- `scip-java`（Java/Scala/Kotlin，基于 SemanticDB，成熟）
- `scip-python`（Python，基于 Pyright，较成熟）
- `rust-analyzer scip`（Rust，rust-analyzer 内置 `scip` 子命令，成熟）
- 其它（Go: `scip-go`；Ruby: `scip-ruby` 等，成熟度不一）

对 SpecSlice 的意义：**SCIP 摄入 = 用「离线生成的精确索引文件」替代「实时 LSP 子进程」**，直接缓解 D3/诉求 #2——索引器可在 CI 的独立步骤一次性产出 `index.scip`，主流程只读文件、零长驻进程。

Rust 侧有官方 `scip` crate（protobuf 绑定），读写 `.scip` 成本低。

### 3.2 绑定策略：用 `symbol_ranges` 做「按范围叠加」，零 id 翻译

这是让 SCIP 摄入「不引入第二套 id」(D2) 的关键，也是本 ADR 最重要的设计判断：

> SCIP 的每个 occurrence 都带 `range`（文件内行列范围）和 `symbol`（结构化字符串）。我们**已经**在 tree-sitter 驱动里为每个符号产出 `SymbolRange`（P23.0）。因此 SCIP 摄入只需把「occurrence.range」与「我们已有的 symbol_ranges」做**范围归属匹配**，即可把 SCIP 的「definition/reference」翻译成「我们既有节点之间的 `Calls`/`References`/`Defines` 边」——**完全复用 Dart analyzer overlay 的「按 id 绑定既有结构」范式**，不需要把 SCIP 符号字符串映射成我们的 ArtifactId。

落点：新增 `scip_overlay`（与 `dart_sidecar` 同层的 Tier-3 富化器），输入 `.specslice/scip/*.scip` + 既有 `symbol_ranges`，输出 `EdgeAssertion`（`indexer="scip"`，高可信度）。无范围命中的 occurrence 安全丢弃（永不 panic、永不造节点）。

### 3.3 stack-graphs 是「无 LSP 跨文件解析」的终极解，但工程长尾

`tree-sitter-stack-graphs`（GitHub 开源，Rust crate `stack-graphs` + `tree-sitter-stack-graphs`）让我们在 **tree-sitter AST 之上写一套 TSG（tree-sitter-graph DSL）规则**，把作用域/导入/可见性建模成「栈图」，从而**在进程内**做到跨文件「定义→引用」解析——既精确又无需外部 LSP，完美契合 D2/D3/诉求 #2。

代价与现实：
- **每语言要写/移植一套 stack-graph 规则**（D5 的长尾）。社区已有 `tree-sitter-stack-graphs-python` / `-typescript` / `-java` 等现成规则，可直接复用；但覆盖面与质量参差，自研语言需要可观投入。
- 需要与我们链入的 `tree-sitter 0.26` 语法版本对齐（stack-graphs 语言包各自钉了 grammar 版本，存在版本错配风险，必须 spike 验证）。
- 解析质量上限通常略低于厂商 LSP/SemanticDB，但**胜在零进程、确定性、可链入单二进制**。

## 4. 决策

1. **采纳 SCIP 作为 SpecSlice 的精度层标准互通边界（导出 + 摄入）**：
   - **摄入优先**：对已有成熟 SCIP indexer 的语言（TS/Java/Python/Rust/Go），将「读取离线 `.scip`」定位为 **Tier-3 的首选精度来源**。这是对诉求 #2 的正面回答。
   - **精度模型「SCIP 权威 + 启发式补空」（2026-06 收敛，见 §8.8）**：实时 LSP sidecar 不再作为 go/python/ts/java 的精度回退——它被**彻底退役**，取而代之的是 tree-sitter 驱动内置的**启发式 `Calls`/`References`**作为基线。SCIP 覆盖到的文件，其同名启发式精度边被**压制**（覆盖区单一真相）；SCIP 未覆盖的文件（如 indexer 缺失/上游损坏）由启发式**补空**，保证无精度断崖。**Swift 是唯一例外**：因缺乏成熟 SCIP indexer，继续以 sourcekit-lsp 作为其 Tier-3 overlay。
   - **导出其次**：提供 `specslice export --format scip`，把我们的图导出为 `.scip`，便于被 Sourcegraph 等生态消费（互操作性，护城河外延）。
   - **绑定方式**：§3.2 的「按 `symbol_ranges` 范围叠加」，零 id 翻译、零第二套结构（满足 D2）。
   - **非侵入式**：indexer 产物与导出文件一律写 `.specslice/scip/`（满足 D1）。

2. **tree-sitter-stack-graphs 列为「无 LSP 精度」的目标方向，但仅以单语言 spike 启动、特性开关隔离、默认关闭**：
   - 首选 spike 语言：**Python 或 TypeScript**（社区规则成熟、收益大）。
   - spike 目标：在我们链入的 grammar 版本上跑通 `tree-sitter-stack-graphs-<lang>`，用真实仓库验证「跨文件 calls/references 精度 ≥ 当前 LSP 且无外部进程」，并复用 §3.2 范围绑定入库。
   - 通过后再决定是否推广；不通过则写书面例外，继续以 SCIP 摄入为主精度路径。

3. **实现顺序与本 Epic 的关系**：本 ADR 不在 P23 内落地代码。P23 只产出本决策文档。具体实现进入后续阶段（见 §6 路线），与 `codegraph-benchmark-and-roadmap.md` 的 Phase B/C 对齐。

## 5. 备选方案与取舍

| 方案 | 跨文件精度 | 外部依赖 | 工程量 | 与 D2/D3 契合 | 结论 |
|---|---|---|---|---|---|
| 维持「实时 LSP sidecar」为唯一精度层 | 高 | 高（长驻进程/PATH） | 低（已实现） | 差（诉求 #2 痛点） | **已退役**（go/py/ts/java）；仅 Swift 例外保留 |
| **SCIP 摄入**（离线 indexer → `.scip` → overlay） | 高 | 中（构建期跑 indexer，无运行期长驻） | 中 | 好 | **采纳（首选精度路径）** |
| SCIP 导出（我们的图 → `.scip`） | n/a | 无 | 低–中 | 好 | 采纳（互操作） |
| **tree-sitter-stack-graphs**（进程内 TSG 解析） | 中–高 | 无（链入） | 高（每语言规则） | 最佳 | spike 验证，默认关闭 |
| 自研跨文件类型解析器 | 取决投入 | 无 | 极高 | —— | 不采纳（重复造轮子） |
| 厂商 SDK（TS Compiler API / JDT / Roslyn 直集成） | 高 | 高（重型 SDK） | 中–高 | 差（毁掉单二进制分发） | 不采纳 |

一句话取舍：**精度层从「实时 LSP」迁移到「离线 SCIP 摄入（权威）+ 进程内启发式（补空）」以根治诉求 #2 并保留无 indexer 时的兜底；go/py/ts/java 的实时 LSP 据此退役，仅 Swift 例外保留；stack-graphs 因上游归档永久退役。**

## 6. 落地路线（默认延后；每步独立 TDD + 非侵入式）

- **R1 SCIP 摄入 MVP**：✅ **已完成（2026-06-07，首发语言 Rust）**。`scip` crate 读 `.specslice/scip/*.scip` → `scip_overlay` 按 `symbol_ranges` 范围匹配 → `Calls`/`References` 边（`indexer="scip"`，高可信）。落点：`crates/specslice-engine/src/scip_overlay.rs`（绑定见 §3.2）、`config.rs`（`enrichment.scip` 默认开）、`index.rs`（结构层之后最后叠加）、`edge_confidence.rs`（SCIP→High）、CLI `index` 打印「SCIP overlay」段。验收实测见 §8.6：本仓 `rust-analyzer scip .` 产物经 overlay 注入 **9684 条高可信边**（启发式 `rust_treesitter` 仅 ~580 calls，≈8× calls 提升）；零运行期长驻进程；重索引幂等（计数稳定）。
  - 备注：原计划首发 TypeScript，实际改为 **Rust 先行**（本仓自举 dogfood、`rust-analyzer scip` 内置成熟、无需额外安装 indexer），符合 §4.1「对已有成熟 indexer 的语言优先摄入」。
- **R2 推广摄入到 TypeScript/Java/Python/Go**：✅ **已推进（2026-06-04，见 §8.8）**。复用 R1 overlay（已语言无关），并新增 `scip_runner`——`specslice index` **自动调用** PATH 上已安装的 SCIP indexer（`rust-analyzer scip` / `scip-go` / `scip-typescript` / `scip-python`…）产出 `.specslice/scip/*.scip`，无 indexer 时静默降级到结构+启发式。确立「**SCIP 权威 + 启发式补空**」模型：覆盖文件压制启发式精度边、未覆盖文件由启发式兜底。据此**退役 go/python/ts/java 的实时 LSP**（删除 `lsp_command`/probe/overlay 接线），**Swift 例外保留** sourcekit-lsp。实测：Rust✓ / Go✓ / TS✓；**Python✗（`scip-python` 上游产空索引，暂由启发式独撑，见 §8.8）**。
- **R3 SCIP 导出**：`specslice export --format scip`，最小可被 Sourcegraph 摄入的子集（definitions + occurrences）。验收：往返一致性（导出→第三方读取无误）。
- **R4 stack-graphs spike（Python 或 TS）**：❌ **永久退役（2026-06-07）**。除原 `tree-sitter <0.25` 的 cargo `links` 硬冲突外，上游 `github/stack-graphs` 已归档（§8.6），触发条件不再可能满足。精度层以 R1 SCIP 摄入为唯一推进路径。
- **R5 决策回填**：依据 R1–R4 证据更新本 ADR 状态与 `codegraph-benchmark-and-roadmap.md` §5.1/§6。

## 7. 后果

- **正向**：根治诉求 #2（精度层去长驻进程化）；与 CodeGraph 的 SCIP 能力对等并以「按范围零翻译绑定」更干净；保住单二进制分发；为生态互通（导出）留出接口。
- **负向**：摄入引入「构建期跑 indexer」的前置步骤（虽非运行期依赖，但仍是环境要求）；stack-graphs 每语言规则是长尾投入与 grammar 版本风险。
- **中性**：精度层从此有「SCIP 摄入 / stack-graphs / 实时 LSP」三条可切换通路，均按 §3.2 统一绑定、统一 evidence 分级——这与 P23「单一结构来源 + 可选 Tier-3 富化」的总架构完全自洽。

## 8. stack-graphs 进程内 spike 可行性验证（2026-05-31，书面例外）

§3.3 要求「必须 spike 验证 grammar 版本对齐」，§4.2 规定「不通过则写书面例外」。本节为该门槛的执行结果，以**真实命令输出**为准（非口头结论）。

### 8.1 方法（非侵入式）

不在工作区添加任何 stack-graphs 依赖（避免污染单二进制分发与既有锁文件）。在 `/tmp` 建一个隔离探针 crate，声明与本仓相同的 `tree-sitter = "0.26"` 约束并叠加 `tree-sitter-stack-graphs = "0.10"`，仅跑 `cargo update`（只做版本求解，不编译），验证后即删除探针。

### 8.2 上游版本约束（cargo 稀疏索引实测）

| crate | 最新版 | 对 `tree-sitter` 的要求 | 备注 |
|---|---|---|---|
| `tree-sitter-stack-graphs` | 0.10.0 | `^0.24`（即 `>=0.24, <0.25`） | 同时要求 `tree-sitter-graph ^0.12` |
| `tree-sitter-graph` | 0.12.0 | `^0.24` | stack-graphs 的传递约束 |
| 本仓 `tree-sitter` | **0.26.9** | —— | P21/P22/P23 全语法栈所链版本 |

### 8.3 cargo 求解结果：硬冲突（`links` 唯一性）

`cargo update` 直接失败，关键输出：

```
error: failed to select a version for `tree-sitter`.
    ... required by package `tree-sitter-stack-graphs v0.10.0`
versions that meet the requirements `^0.24` are: 0.24.7 ... 0.24.0
the package `tree-sitter` links to the native library `tree-sitter`, but it
conflicts with a previous package which links to `tree-sitter` as well:
package `tree-sitter v0.26.2`
Only one package in the dependency graph may specify the same links value.
```

`tree-sitter` 在其 `Cargo.toml` 中声明了 `links = "tree-sitter"`。Cargo 的 `links` 唯一性规则**禁止同一依赖图里出现两个 tree-sitter 版本**——这不是「两版本可共存、只是类型不互通」的软问题，而是**编译期硬禁止**。因此在保留本仓 0.26 语法栈的前提下，无法引入需要 `<0.25` 的 stack-graphs。

### 8.4 叠加阻塞：缺 Rust 规则包

stack-graphs 官方语言规则仅有 `tree-sitter-stack-graphs-python / -javascript / -typescript / -java`，**没有 Rust**。本仓首要 dogfood 语言是 Rust，即便版本可对齐，Rust 仍需从零手写 TSG 规则（大长尾），不符合 D5（优先复用成熟生态）。

### 8.5 书面例外与结论（2026-05-31 初判：受阻待触发）

- **R4（stack-graphs 单语言 spike）受阻**——并非缺乏意愿，而是**上游 `tree-sitter-stack-graphs`/`tree-sitter-graph` 仍锁在 `tree-sitter <0.25`**，与本仓 0.26 语法栈存在 cargo `links` 级硬冲突。
- **已拒绝的绕过方案**：
  1. 将全仓语法栈回退到 0.24 —— 违背「保持解析器与上游同步」，会回退 P21/P22/P23 的语法覆盖与修复，代价不可接受；
  2. 维护并行的 0.24 解析栈专供 stack-graphs —— 违背 D2（单一结构来源）与 D3（单二进制简洁）。
- **复活触发条件（2026-05-31 设定）**：当 `tree-sitter-stack-graphs`（及其传递依赖 `tree-sitter-graph`）发布**要求 `tree-sitter >=0.26`** 的版本时，重启 R4 spike。
- 该触发条件已于 2026-06-07 被 §8.6 证否（上游归档），R4 据此由「受阻待触发」转为「永久退役」。

### 8.6 复核与 R4 永久退役（2026-06-07，真实 API 输出）

§8.5 的复活触发条件依赖「上游继续演进并放宽版本约束」。2026-06-07 复核该前提，结论是它**已不可能成立**：

- **上游仓库已归档**（GitHub API `repos/github/stack-graphs`）：

  ```
  archived: True
  disabled: False
  pushed_at: 2025-09-09T22:02:09Z   # 末次提交
  ```

  归档仓库不再接受提交/合并，§8.5 设定的「发布要求 `tree-sitter >=0.26` 的新版本」这一触发动作**永远不会发生**。

- **crates.io 版本约束维持不变**（仍是 §8.2 所记的旧约束，自 2024-12 起未动）：

  | crate | 最新版 | 发布时间 | 对 `tree-sitter` 的要求 |
  |---|---|---|---|
  | `tree-sitter-stack-graphs` | 0.10.0 | 2024-12-13 | `^0.24`（`>=0.24,<0.25`） |
  | `tree-sitter-graph` | 0.12.0 | 2024-12-11 | `^0.24` |

  与本仓 `tree-sitter 0.26.x` 的 `links` 硬冲突（§8.3）因此**不再有上游修复的可能**。

- **结论**：R4（stack-graphs 进程内 spike）**永久退役**，从 §6 路线的「待触发」状态移除。精度层以 **R1 SCIP 摄入**为**唯一**推进路径——SCIP indexer 的离线 `.scip` 产物不与我们链入的 grammar 版本耦合，从根上没有该冲突，且 R1 已落地验证（§8.7）。
- 若未来出现「积极维护、支持 `tree-sitter >=0.26`」的**全新 stack-graphs 替代实现**，再以新 ADR 评估，不复用本已关闭的 R4。

### 8.7 R1 SCIP 摄入落地验证（2026-06-07，本仓 dogfood，真实命令输出）

- **产物来源**：`rust-analyzer scip .` 在本仓生成 `index.scip`（12.5 MB），置于 `.specslice/scip/index.scip`（非侵入式，仅落 `.specslice/`，满足 D1）。
- **`specslice index` 摘要（真实输出节选）**：

  ```
  Rust index:
    Rust files: 166
    Symbols: 2049
    Resolver: rust_treesitter
  SCIP overlay:
    Files: 1
    Documents: 166
    Edges: 9684
  ```

  即 SCIP overlay 在既有 tree-sitter 结构节点上叠加了 **9684 条**高可信 `Calls`/`References` 边（对比启发式 `rust_treesitter` 仅约 580 条 calls，≈8× calls 提升），全部标 `indexer="scip"` → `EdgeConfidence::High`（§D4）。
- **幂等**：连续两次 `index` 计数稳定（overlay 先 `clear_indexer_outputs("scip")` 再叠加），满足 D4 确定性。
- **零运行期长驻进程**：摄入只读磁盘 `.scip`，无 LSP 子进程，正面兑现诉求 #2。
- **回归**：`specslice-engine` 与 `specslice-cli` 全测通过（CLI 新增 `render_includes_scip_section_when_overlay_ingested_files` / `render_omits_scip_section_when_no_scip_file_on_disk` 覆盖打印分支）。

### 8.8 R2 推进：自动调用 + SCIP 权威补空 + LSP 退役（2026-06-04，真实命令输出）

R1（§8.7）只验证了「手工产出 `.scip` → overlay 摄入」。R2 把它工程化为「**零手工、自动调用、权威补空**」，并据此退役实时 LSP。以**真实 `specslice index` 输出**为准：

**(a) 自动调用 SCIP indexer（`scip_runner`）。** `specslice index` 在结构层之后探测 PATH 上已安装的 SCIP indexer 并自动调用，产物落 `.specslice/scip/<lang>.scip`（非侵入式，满足 D1）；无 indexer 时静默降级，不报错。本仓实测（rust-analyzer 在 PATH）：

```
SCIP indexers:
  rust: generated
SCIP overlay:
  Files: 1
  Documents: 167
  Edges: 9619
  Suppressed (heuristic): 560
```

即无需操作者手敲 `rust-analyzer scip .`——索引一步到位。Go（`scip-go`）/ TS（`scip-typescript`）同机验证可自动产出并摄入；spec 见 `crates/specslice-engine/src/scip_runner.rs`。

**(b) SCIP 权威 + 启发式补空（消重 → 覆盖区单一真相）。** 上方 `Suppressed (heuristic): 560` 即证据：SCIP 覆盖到的 167 个 Rust 文件，其 tree-sitter 启发式 `Calls`/`References`（约 580 条）被**压制 560 条**，由 9619 条 `indexer="scip"` 高可信边取代——覆盖区只保留一种真相，杜绝「同一调用既有启发式边又有 SCIP 边」的重复。机制：`scip_overlay` 收集本次有 occurrence 的 `covered_files`，调用 `repositories.rs::delete_precision_edges_for_files_except(covered_files, keep="scip")` 删除这些文件上的非 SCIP 精度边；**未被 SCIP 覆盖的文件其启发式边原样保留**（补空，无精度断崖）。TDD 覆盖：`repositories.rs::scip_suppression_clears_only_nonscip_precision_on_covered_files`、`scip_overlay.rs::ingest_suppresses_heuristic_edges_only_on_scip_covered_files`。

**(c) 据此退役 go/python/ts/java 的实时 LSP（commit `refactor(lsp)`）。** 既然「SCIP 权威 + 启发式补空」已能在覆盖区给高可信、未覆盖区给兜底，实时 LSP sidecar 对这四种语言不再有独立价值，遂**整体删除**其 `lsp_command` / probe / overlay / `*_lsp_available` / `*_LSP_COMMAND_ENV` 接线（adapter 收敛为「tree-sitter 结构 + 启发式」薄壳）。CLI 这四种语言改显示 `References (heuristic)`，不再有 `References (LSP)` / `LSP skipped`。**Swift 是唯一例外**：缺乏成熟 SCIP indexer，继续保留 sourcekit-lsp 作为其 Tier-3 overlay（`References (LSP)` / `LSP skipped` 仅 Swift 保留）。

**(d) Python 上游限制（书面记录）。** `scip-python`（实测 0.6.6）对样例仓库产出**空索引（0 documents）**，无法提供精度边。因此 Python 当前**由启发式独撑**（符合上方「补空」语义——SCIP 未覆盖即启发式兜底），待上游修复或换用可用 indexer 后自动恢复 SCIP 权威，无需改代码。

**(e) 回归。** LSP 退役后全量测试绿：`specslice-engine` lib 511、`specslice-cli` bins 45、集成测试（`lsp_indexers.rs` 重写为「Swift LSP 契约 + go/py/ts/java 结构契约」）全部通过；`index` / overlay 重跑计数稳定（幂等）。

## 9. 参考

- SCIP 协议与 `scip` Rust crate（Sourcegraph）。
- SCIP indexers：`scip-typescript`、`scip-java`、`scip-python`、`rust-analyzer scip`。
- `tree-sitter-stack-graphs` / `stack-graphs` / `tree-sitter-graph`（GitHub）。
- 本仓：`docs/codegraph-benchmark-and-roadmap.md` §5.1；`crates/specslice-engine/src/treesitter.rs`（`symbol_ranges`）；`crates/specslice-engine/src/dart_sidecar.rs`（Tier-3 overlay 范式）。
