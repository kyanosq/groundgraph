# ADR-0001：SCIP 互通与 tree-sitter-stack-graphs 跨文件符号解析

- 状态：已接受（Accepted）——**记录决策与路线；默认延后实现**（按需以单语言 spike 启动）。stack-graphs 进程内方向已于 2026-05-31 完成**真实 cargo 可行性验证**，结论为「当前被上游版本锁死、维持延后」，见 §8 书面例外。
- 日期：2026-05-31（§8 验证补记于同日）
- 关联：P23 收敛与成熟化 Epic（诉求 #2「去多套实现 / 摆脱外部 LSP 脆弱性」、#3「SCIP / stack-graphs 是否自研」）
- 上游事实：`docs/codegraph-benchmark-and-roadmap.md` §5.1（三层后端）已将 SCIP/stack-graphs 记为「未来 Tier 2.5/Tier 3 精度增强候选」；本 ADR 把它从「记在案」升级为「有明确接口、绑定策略与验收门槛的决策」。

## 1. 背景与问题

SpecSlice 当前的结构来源已收敛为**唯一的 tree-sitter 通用驱动**（`crates/specslice-engine/src/treesitter.rs`，P22/P23），LSP / Dart analyzer 退化为可选 Tier-3 语义富化（按符号 id 叠加 `Calls`/`References`）。这套架构解决了「语言广度」和「结构确定性」，但仍有两个结构性缺口：

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

1. **采纳 SCIP 作为 SpecSlice 的精度层标准互通边界（导出 + 摄入），但默认延后实现**：
   - **摄入优先**：对已有成熟 SCIP indexer 的语言（TS/Java/Python/Rust），将「读取离线 `.scip`」定位为 **Tier-3 的首选精度来源**，逐步把实时 LSP 降为「无 SCIP 时的回退」。这是对诉求 #2 的正面回答。
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
| 维持「实时 LSP sidecar」为唯一精度层 | 高 | 高（长驻进程/PATH） | 低（已实现） | 差（诉求 #2 痛点） | 保留为回退，不作首选 |
| **SCIP 摄入**（离线 indexer → `.scip` → overlay） | 高 | 中（构建期跑 indexer，无运行期长驻） | 中 | 好 | **采纳（首选精度路径）** |
| SCIP 导出（我们的图 → `.scip`） | n/a | 无 | 低–中 | 好 | 采纳（互操作） |
| **tree-sitter-stack-graphs**（进程内 TSG 解析） | 中–高 | 无（链入） | 高（每语言规则） | 最佳 | spike 验证，默认关闭 |
| 自研跨文件类型解析器 | 取决投入 | 无 | 极高 | —— | 不采纳（重复造轮子） |
| 厂商 SDK（TS Compiler API / JDT / Roslyn 直集成） | 高 | 高（重型 SDK） | 中–高 | 差（毁掉单二进制分发） | 不采纳 |

一句话取舍：**精度层从「实时 LSP」迁移到「离线 SCIP 摄入」以根治诉求 #2；stack-graphs 作为「连 SCIP indexer 都不想依赖」的进程内终极解，spike 先行。**

## 6. 落地路线（默认延后；每步独立 TDD + 非侵入式）

- **R1 SCIP 摄入 MVP（单语言 TypeScript）**：`scip` crate 读 `.specslice/scip/index.scip` → `scip_overlay` 按 `symbol_ranges` 范围匹配 → `Calls`/`References`/`Defines` 边（`indexer="scip"`）。验收：与同仓 `scip-typescript` 产物对照，精度 ≥ 现 TS LSP；零运行期进程；重索引幂等。
- **R2 推广摄入到 Java/Python/Rust**：复用 R1 overlay，仅新增「如何获得各语言 `.scip`」的文档与可选脚手架（不强制）。把对应语言的实时 LSP 调整为「无 `.scip` 时回退」。
- **R3 SCIP 导出**：`specslice export --format scip`，最小可被 Sourcegraph 摄入的子集（definitions + occurrences）。验收：往返一致性（导出→第三方读取无误）。
- **R4 stack-graphs spike（Python 或 TS）**：特性开关 + 单语言；验收见 §4.2。**当前受阻**：上游锁 `tree-sitter <0.25`，与本仓 0.26 存在 cargo `links` 硬冲突（实测见 §8），待「上游支持 `tree-sitter >=0.26`」触发条件满足后重启。
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

### 8.5 书面例外与结论

- **R4（stack-graphs 单语言 spike）维持默认延后**——并非缺乏意愿，而是**上游 `tree-sitter-stack-graphs`/`tree-sitter-graph` 仍锁在 `tree-sitter <0.25`**，与本仓 0.26 语法栈存在 cargo `links` 级硬冲突。
- **已拒绝的绕过方案**：
  1. 将全仓语法栈回退到 0.24 —— 违背「保持解析器与上游同步」，会回退 P21/P22/P23 的语法覆盖与修复，代价不可接受；
  2. 维护并行的 0.24 解析栈专供 stack-graphs —— 违背 D2（单一结构来源）与 D3（单二进制简洁）。
- **复活触发条件（明确、可机检）**：当 `tree-sitter-stack-graphs`（及其传递依赖 `tree-sitter-graph`）发布**要求 `tree-sitter >=0.26`** 的版本时，重启 R4 spike（首选 Python/TS，特性开关、默认关闭）。在此之前，精度层以 **R1 SCIP 摄入**为唯一推进路径（SCIP indexer 离线产物不与我们的 grammar 版本耦合，无此冲突）。
- **路线状态**：§6 的 R1–R5 不变；R4 标注为「受阻于上游版本锁，待触发条件满足」。本节即 §4.2 所要求的书面例外。

## 9. 参考

- SCIP 协议与 `scip` Rust crate（Sourcegraph）。
- SCIP indexers：`scip-typescript`、`scip-java`、`scip-python`、`rust-analyzer scip`。
- `tree-sitter-stack-graphs` / `stack-graphs` / `tree-sitter-graph`（GitHub）。
- 本仓：`docs/codegraph-benchmark-and-roadmap.md` §5.1；`crates/specslice-engine/src/treesitter.rs`（`symbol_ranges`）；`crates/specslice-engine/src/dart_sidecar.rs`（Tier-3 overlay 范式）。
