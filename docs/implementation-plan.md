# SpecSlice 落地方案、测试体系与验收指标

## 目标

SpecSlice 的核心目标是证明一个非侵入式闭环：

```text
文档事实 / Dart 事实 / 测试事实 -> AI 业务逻辑候选与关联候选 -> 人工确认 -> confirmed graph -> PR Impact / Agent Context Pack / Graph 浏览
```

MVP-0 ~ MVP-5 已完成；P6 ~ P9 已把只读图浏览、代码事实边、Dart analyzer sidecar、Flutter/Riverpod 语义边和 AI 业务候选层落到主线。P10 落地 `specslice dead-code`，P11 把 MCP 工具层与可展开/可过滤的搜索阅读器并入主线，P12 通过 LSP sidecar 加入 Swift / Go 的结构事实图，P13 在同一 LSP 通路上补全 `callHierarchy` + `references`，让 Swift / Go 的调用 / 引用边和 Dart analyzer 保持同一可信链路。P14 把 `Calls` / `References` 真正接入 `impact` 反向 BFS、`slice` 正向 fanout、MCP `get_subgraph` 的 `resolvers` 过滤，并为 `search` / `impact` / `candidate show` 提供局部 Mermaid 导出，让 PR / 设计文档可以嵌入小型可视化。P15 把 LSP 集成测试与沙箱 CI 解耦（真实 LSP 测试默认 `#[ignore]`，`--include-ignored` 才跑），把 Swift / Go `Calls` 边的 evidence 改为 `outgoingCalls[].fromRanges` 报告的 caller 调用点而非 callee 声明位置，并在 `ImpactReport` 增加 `impact_edges: Vec<ImpactEdge>` 真实边轨迹，让 `impact --format mermaid` 不再合成近似 cross-product。P16 引入 Python 适配器（`python.enabled: true`），同一套 `LspProfile` 通路驱动 `pyright-langserver` / `basedpyright-langserver` / `pylsp`，按 `SPECSLICE_PYTHON_LSP_BIN → .venv → PATH` 顺序自动发现，结构事实和 `Calls` / `References` 来源标记 `indexer = python_lsp`；同一索引会再叠一层纯 Rust AST 扫描，无论 LSP 是否可用都补齐 `Imports` 边与 pytest `TestCase` / `TestGroup`，无 LSP 时也能产出 `python_class / python_method / python_function` 结构，AST 输出统一打 `indexer = python_ast` 以区分证据来源。当前阶段仍不做 GraphRAG、不把 LLM 输出直接写进 confirmed graph，也不在 Swift / Go 代码里加任何注解。价值判断看三件事：

- AI 能否基于文档/代码/测试事实生成高质量业务逻辑候选和候选关联。
- 人工确认后的外置 graph 能否在不改业务代码/业务文档的前提下稳定查询、反查和审阅。
- 图浏览能否帮助用户快速理解目标仓库的真实代码逻辑，并明确区分事实、候选、确认关系和风险。

## 产品工作流

SpecSlice 的产品体验不是让用户手写业务逻辑与代码的映射，也不是用规则从文件名、标题、注释或命名约定里猜业务含义。产品闭环应当是：

1. **代码图生成事实。** SpecSlice 只用确定性索引器生成文档段落、Dart 符号、测试、调用、Provider、路由、持久化、Stream 等事实节点和事实边。
2. **AI 生成业务候选。** AI 读取代码图、文档事实和测试事实，把多个事实组织成中文自然语言业务逻辑描述，并附上 evidence、可信度和未能证明的问题。
3. **人工确认业务含义。** 用户看到的是自然语言确认稿，而不是原始 artifact id 或 YAML。用户可以逐条选择确认、拒绝、补充或暂缓。
4. **确认后进入 confirmed graph。** 只有人工确认后的候选才会写入 `.specslice/requirements.yaml` 或 `.specslice/links.yaml`，并进入 confirmed graph。AI 候选默认不参与 `slice`、`context`、`impact` 的可信链路。
5. **问题转为测试或澄清。** 如果候选缺少测试、缺少业务文档、无法判断产品意图，系统应以“需要补充的问题”呈现，而不是把候选当作错误或事实。

确认界面的核心输出必须是中文自然语言，例如：

```text
编辑器会从项目库加载项目，编辑像素或图层后进入防抖保存；撤销/重做使用最多 100 条历史快照。
建议：可以确认。真实 App 生命周期暂停/恢复仍建议补测试。
```

原始 evidence 只作为可展开的依据展示，用于审计 AI 为什么得出这条业务描述；它不应成为用户确认业务含义的主要交互方式。

### 人工确认结果的产品语义

人工确认不是“用户手写链接”，而是用户对 AI 根据代码图生成的业务描述做产品判断。确认结果应支持四类闭环：

- `accepted`：业务描述符合产品意图，可以进入 confirmed graph。
- `rejected`：业务描述不符合产品意图，保留为被拒绝候选，避免下次重复提出。
- `needs_changes`：业务方向成立，但需要补测试、补产品边界或补实现；不能进入 confirmed graph。
- `pending`：用户需要更多解释，或还有外部配置、商店后台、设备行为等代码图无法证明的信息。

AI candidate 代理可以做的事：

- 根据代码图和测试图生成中文业务描述、证据、可信度、风险和待确认问题。
- 根据用户自然语言反馈更新候选审阅状态，例如“这项没问题”“需要补测试”“这里不是三类而是两类”。
- 把“需要补测试”的项转成 TDD 任务，测试通过后再建议用户确认。

AI candidate 代理不能做的事：

- 不能把 `proposed` 候选直接标成 confirmed business rule。
- 不能要求用户在业务代码、业务文档或测试中加入 SpecSlice 注解。
- 不能把文件名、标题、注释或命名相似度当作业务关联真相；这些只能作为 AI 解释候选时的弱信号。

## 非侵入式约束

- 业务代码、业务测试、业务文档默认只读扫描。
- 不支持在业务代码/业务测试中加入工具专用注解。
- 不支持在业务文档中加入工具专用关系段落。
- Markdown frontmatter 只能作为普通文档内容的结构边界处理，不能被规则解释为业务需求、ADR 或验收标准。
- SpecSlice 只能写 `.specslice.yaml`、`.specslice/links.yaml`、`.specslice/graph.db`、`.specslice/export/`，以及后续 `.specslice/requirements.yaml`、`.specslice/candidates/`。
- LLM 只能生成候选、问题和解释，不能写回业务代码、业务测试或业务文档。
- 业务逻辑单元的抽取、业务文档与代码/测试的关联候选，不能由人工初始标注产生，也不能由规则匹配产生。
- 规则只负责解析物理事实、校验 AI 候选引用、维护已确认外置关系。
- AI 负责从事实中生成业务逻辑候选、关联候选、可信度和澄清问题。
- 人工负责确认、编辑、拒绝 AI 候选；确认结果写入 SpecSlice 自有目录。

## 工程落地顺序

### MVP-0：Rust CLI、配置与 SQLite 存储

**目标：** 建立可运行的仓库骨架和图存储。

**实现范围：**

- Rust workspace：`specslice-core`、`specslice-store`、`specslice-lang-dart`、`specslice-engine`、`specslice-cli`。
- `specslice init` 生成 `.specslice.yaml` 和 `.specslice/graph.db`。
- SQLite migration 创建 `nodes`、`edge_assertions`、`evidence`、`symbol_ranges`、`file_index`、`slice_cache`。
- `specslice export --format jsonl` 导出当前图数据。

**TDD 起点：**

- 先写 CLI e2e 测试：空目录执行 `specslice init` 后能看到配置和数据库。
- 再写 store integration 测试：migration 后表存在，重复执行 migration 不报错。
- 最后写 export 测试：空图也能输出合法 JSONL 文件。

### MVP-1：Markdown 文档事实索引

**目标：** 从 `docs / specs / adr` 中索引 Markdown 文件和文档段落事实，不做业务语义判断。

**实现范围：**

- 提取 Markdown File / DocSection。
- 不要求业务文档新增 frontmatter。
- 不把 frontmatter、标题、编号规则解释为 Requirement、ADR 或业务逻辑。
- 不建立 `DocSection --documents--> Requirement`。
- 只建立 `File --contains--> DocSection` 事实边。
- 后续 AI 基于 DocSection 文本、代码符号和测试事实生成业务逻辑候选。

**TDD 起点：**

- Fixture：`docs/watermark.md`。
- 先断言 `specslice index --docs-only` 输出 `Requirements: 0`、`DocSections: 1`。
- 再断言数据库里只有 file/doc section 和 contains edge，没有 requirement node 和 documents edge。
- 再写 frontmatter 文档回归测试，确认规则不会从 frontmatter 推导业务语义。

### MVP-2：Dart Adapter 与外置关系声明

**目标：** 用统一 `LanguageIndexBatch` 提取 Dart 文件、符号和测试；关系声明只来自 `.specslice/links.yaml`。Rust lightweight scanner 是默认 fallback；P7 之后可通过 Dart analyzer sidecar 获得 resolved AST 精度。

**实现范围：**

- 扫描配置中的 code paths，默认覆盖 `lib / test`。
- 提取 file、class、method、function、constructor、import、`test(...)`、`group(...)`。
- 可选 sidecar 通过 `SPECSLICE_DART_ANALYZER=1` 启用，输出同一批量协议；失败时回退到 lightweight，不阻断索引。
- 输出 `LanguageIndexBatch`，由 Core 统一入库。
- 建立 symbol range 和 parent-child hierarchy。
- 索引 `.specslice/links.yaml`，建立 `Documents`、`DeclaresImplementation`、`DeclaresVerification` 边。

**TDD 起点：**

- Adapter unit tests 覆盖 class、method、constructor、top-level function。
- Test extractor 覆盖 `test('name', ...)` 和 `group('name', ...)`。
- Links manifest 测试覆盖无业务注解时仍能连接 requirement、implementation、test。
- Range mapper 覆盖 method 改动可映射到 method，并通过 manifest 声明的 parent class 回溯到 Requirement。

### MVP-3：Feature Slice

**目标：** 从一个已确认业务逻辑 ID 找到文档、实现和测试。

**实现范围：**

- `specslice slice <confirmed-business-logic-id>`。
- 只走 confirmed/declared 高可信边。
- 默认不走 imports、calls、references、semantic code facts、candidate edges。代码事实边用于图浏览、候选生成和解释，不自动提升为 confirmed slice。
- 输出 Docs、Linked Implementation、Linked Tests、Risks。

**TDD 起点：**

- 使用 fixture 中已确认的 `REQ-WATERMARK-001`。
- 断言 slice 包含 `docs/watermark.md`、`AutoPlacementService`、`auto_placement_service_test.dart`。
- 断言无测试时给出 missing linked test risk。

### MVP-4：PR Impact

**目标：** 根据 Git diff 定位 changed symbols，并反查受影响需求、文档和测试。

**实现范围：**

- 读取 `git diff --unified=0 base...HEAD`。
- 解析 changed file 和 changed line ranges。
- impact 前检查 file hash，必要时增量索引 changed files。
- 通过 `symbol_ranges` 找 changed symbols。
- 支持 direct symbol link、parent class link、containing file link、test file relation、changed doc section relation。

**TDD 起点：**

- 临时 fixture Git 仓库：先提交 baseline，再修改实现类方法。
- 断言 `specslice impact --base main` 输出 changed symbol、affected requirement、affected doc、linked test。
- 再修改 requirement 文档，断言 impact 能输出 changed doc section、linked implementation、linked test。

### MVP-5：Basic Checks 与 Agent Context Pack

**目标：** 把图谱结果转换为工程检查和 AI 可用上下文。

**实现范围：**

- Broken Link Check。
- Missing Linked Test Check。
- Orphan Requirement Check。
- Impact Review Check。
- `specslice context REQ-ID --json` 输出 docs、implementation、linked_tests、risks、files_to_read、tests_to_run。

**TDD 起点：**

- Links manifest 指向不存在节点时返回 error。
- Requirement 有 implementation 但无 verification 时返回 warning。
- Context JSON 必须可反序列化，并包含最小文件集合。

### Phase-1：AI 候选关联、逻辑可信度与澄清候选

**目标：** 在不侵入业务仓库的前提下，由 AI 从文档事实、代码事实和测试事实中生成业务逻辑候选与候选关联，人工确认后进入 confirmed graph；同时识别“关系存在但业务逻辑未验证”、“业务文档缺失”、“文档与代码/测试信号可能不一致”的风险。

**实现范围：**

- 新增 `specslice connect`：
  - 输入 docs/code/tests 的事实节点和 evidence pack。
  - AI 生成 business logic candidates、candidate links、clarifying questions。
  - 系统校验 candidate 引用是否存在、唯一、可定位。
  - 人工确认、编辑或拒绝。
  - 确认后写入 `.specslice/links.yaml`。
- 新增 LogicConfidence report：
  - `confirmed_link`：外置关系已确认且节点可解析。
  - `stale_link`：关联文件 hash 变化后未复核。
  - `missing_doc`：实现/测试存在，但没有可读业务逻辑文档。
  - `missing_link`：存在文档或代码信号，但没有外置关系声明。
  - `mismatch_candidate`：文档描述与代码/测试信号可能冲突。
  - `unknown`：证据不足。
- 新增 candidate 输出到 `.specslice/candidates/`。
- 新增可选 `.specslice/requirements.yaml`，用于保存人工确认后的 AI 业务逻辑草案，尤其是没有业务文档时的外置需求描述。
- 新增 `specslice ask`，根据 evidence 生成需要用户回答的问题。
- AI 只生成 candidate / questions，不直接写 confirmed graph。
- 禁止用规则匹配生成业务关联。

**TDD 起点：**

- 有业务文档、实现和测试但无 links 时，AI 从 DocSection 与代码/测试事实生成 candidate，经确认后写入 `.specslice/links.yaml`。
- 无业务文档但有实现/测试时，AI 可生成低可信业务逻辑草案，同时输出 `missing_doc` 和澄清问题。
- links 指向的文件 hash 变化后，输出 `stale_link`。
- AI 判断文档与代码/测试信号可能冲突时，只输出 `mismatch_candidate`，不作为 error。
- 用户确认 candidate 后，只更新 `.specslice/links.yaml` 或 `.specslice/requirements.yaml`。

## 测试体系

### 测试分层

1. **Core unit tests**
   - 覆盖 stable ID、node/edge/evidence 类型、result types、枚举序列化。
   - 不访问文件系统和数据库。

2. **Store integration tests**
   - 使用临时目录和 SQLite 文件。
   - 覆盖 migration 幂等性、upsert、edge 查询、symbol range 查询、file index hash 更新。

3. **Docs indexer tests**
   - 使用小型 Markdown fixture。
   - 覆盖 frontmatter 不产生语义节点、doc section line range、File contains DocSection。

4. **Dart adapter tests**
   - 使用内联 Dart 源码和 fixture 文件。
   - 覆盖 class/method/function/test/group/import。
   - 重点覆盖 line range 和 parent symbol，因为 PR Impact 依赖它们。

5. **Engine behavior tests**
   - 覆盖 index、slice、impact、check、context 的业务结果。
   - 这些测试不关心 CLI 文案，只断言结构化 result type。

6. **CLI e2e tests**
   - 使用 `assert_cmd` 和临时 fixture 仓库执行真实 `specslice` 命令。
   - 覆盖用户实际会运行的命令：`init`、`index`、`slice`、`impact`、`check`、`context`、`export`。

7. **Golden/snapshot tests**
   - 对 `slice`、`impact`、`context --json` 做快照。
   - 快照只用于稳定输出格式，不替代行为断言。

### 推荐测试依赖

Rust dev-dependencies 建议：

```toml
assert_cmd = "2"
predicates = "3"
assert_fs = "1"
tempfile = "3"
insta = { version = "1", features = ["json"] }
serde_json = "1"
```

生产依赖后续按实现选择，但建议优先保持简单：

```toml
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
rusqlite = { version = "0.32", features = ["bundled"] }
walkdir = "2"
globset = "0.4"
sha2 = "0.10"
time = { version = "0.3", features = ["formatting"] }
```

## TDD 工作规则

每个行为按红绿重构推进：

1. 写一个最小失败测试。
2. 运行测试，确认失败原因是目标行为缺失，而不是测试写错。
3. 写最小实现。
4. 运行同一个测试，确认通过。
5. 运行相关测试集合，确认没有回归。
6. 重构，保持测试绿。

开发记录中需要保留可验证证据：

- 新行为对应的测试文件。
- RED 阶段失败输出。
- GREEN 阶段通过输出。
- 相关命令最终通过输出。

## 验收指标

### 每次提交前必须通过

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

如果当前阶段尚未引入某个工具，提交说明里必须明确说明原因和替代验证命令。

### MVP-0 验收

- `specslice init` 在空目录成功生成 `.specslice.yaml`、`.specslice/graph.db`。
- 重复运行 `specslice init` 不破坏已有配置。
- SQLite migration 幂等。
- `specslice export --format jsonl` 对空图也能输出合法文件。

### MVP-1 验收

- fixture 中 `docs/watermark.md` 能生成 1 个 File 和至少 1 个 DocSection。
- 即使文档含 frontmatter，Markdown 索引也不生成 Requirement、ADR 或 AcceptanceCriterion。
- Markdown 索引不生成 `DocSection --documents--> Requirement` edge。
- 文档 line range 能定位到对应 section。

### MVP-2 验收

- fixture 中 Dart files、Symbols、TestCases 计数正确。
- `.specslice/links.yaml` 能把 `REQ-WATERMARK-001` 连到实现类和测试。
- method 改动能映射到 method symbol。
- method 改动能通过 parent class 和 links manifest 回溯到 Requirement。

### MVP-3 验收

- `specslice slice REQ-WATERMARK-001` 能稳定输出需求文档、实现类、测试。
- 无 linked test 时输出 warning/risk。
- 默认不把 imports 纳入 feature slice。

### MVP-4 验收

- 修改实现类后，`specslice impact --base <baseline>` 输出 changed symbol、affected requirement、affected doc、linked test。
- 修改实现类 method 后，impact 能通过 parent class 反查 requirement。
- 修改 requirement 文档后，impact 能输出 changed doc section、linked implementation、linked test。
- 未改相关测试时输出 warning，不声称测试覆盖真实行为。

### MVP-5 验收

- `specslice check` 能区分 error、warning、info。
- broken link 是 error。
- orphan requirement 和 missing linked test 是 warning。
- `specslice context REQ-WATERMARK-001 --json` 输出可反序列化 JSON。
- context 中 `files_to_read` 只包含相关 docs、implementation、tests。
- context 中 `tests_to_run` 只包含 linked tests。

## 总体验收门槛

MVP 完成时必须满足：

- 所有验收 fixture 都在 `examples/flutter_watermark_app` 或 `tests/fixtures/flutter_watermark_app` 中固化。
- `cargo test --workspace` 通过。
- CLI e2e 覆盖 PRD 中 6 条命令：
  - `specslice init`
  - `specslice index .`
  - `specslice slice REQ-WATERMARK-001`
  - `specslice impact --base <baseline>`
  - `specslice check`
  - `specslice context REQ-WATERMARK-001 --json`
- 行为覆盖优先于行覆盖。进入 MVP-2 后再引入 `cargo llvm-cov`，目标是核心 crate 行覆盖率不低于 75%。
- 所有对外 result type 支持 `Serialize`、`Deserialize`、`Debug`、`Clone`。
- JSONL export、stable node ID、edge assertion schema、`.specslice.yaml` 保持向后兼容。

## 下一步任务规划

### P0：非侵入式边界固化

- 清理所有代码、测试、CLI 文案、PRD 中的工具专用关系残留。
- 确认 Markdown frontmatter 不再被规则解释成业务需求。
- fixture 增加“无 frontmatter + links manifest”的端到端用例。
- 验收：全文搜索无工具注解/业务 Related 语义残留；`cargo test --workspace` 通过。

### P1：AI 候选关联与人工确认

- 增加 `specslice connect`。
- 生成 evidence pack：docs sections、symbols、tests、paths、line ranges、hash。
- 调用 AI 生成 business logic candidates、candidate links 和澄清问题。
- 系统只校验 candidate 引用是否存在、唯一、可定位，不用规则匹配生成候选。
- 用户确认、编辑或拒绝 candidate。
- 确认后写入 `.specslice/links.yaml`。
- 验收：不手写 YAML、不靠规则匹配，也能完成 fixture 关系建立。

### P2：关系维护工具

- 增加 `specslice link list`。
- 增加 `specslice link edit`。
- 增加 `specslice link remove`。
- 增加 `specslice link check`，只检查 `.specslice/links.yaml` 解析和节点可达性。
- 验收：人工可以修正已确认关系，但不是主要建链入口。

### P3：外置 Requirement Registry

- 增加 `.specslice/requirements.yaml`。
- 支持没有业务需求文档时，用外置 registry 保存 id、title、description、status。
- `slice/context/impact/check` 同等支持 docs-backed 和 registry-backed Requirement。
- 验收：无业务 docs 的 fixture 仍可得到 context pack，但报告 `missing_doc` 风险。

### P4：逻辑可信度

- 增加 LogicConfidence 结构化报告。
- 基于 links、file hash、doc presence、test presence 输出 `confirmed_link / stale_link / missing_doc / missing_link / unknown`。
- AI 可以生成 `mismatch_candidate` 和澄清问题，但不能声称业务正确。
- 验收：修改已链接文件后出现 `stale_link`；缺文档时出现 `missing_doc`。

### P5：无文档澄清与候选需求

- 增加 `specslice ask`，输出带 evidence 的澄清问题。
- 增加 `.specslice/candidates/` 保存候选关系和候选需求。
- 增加 `specslice accept-candidate`，用户确认后写入 `.specslice/links.yaml` 或 `.specslice/requirements.yaml`。
- 验收：LLM/candidate 不进入 confirmed graph；CI 默认不信任 candidate。

### P6：可视化与审阅界面 ✅ 落地

详细设计见 [`docs/visualization-design.md`](visualization-design.md)。

落地内容（CLI 子命令 `specslice graph`）：

- `--format json`：输出稳定契约 `GraphViewModel`（当前 schema_version=2），含 view/stats/nodes/edges/findings 与 generated_at。
- `--format mermaid`：输出 `flowchart LR`，使用 `n0/n1/…` 别名，不暴露原始 artifact id；layer→箭头/形状映射（confirmed→实线圆角、candidate→虚线、risk→菱形）。
- `--format html`：生成 `.specslice/export/graph.html`，自包含 HTML+CSS+JSON+JS，零远程依赖（无 `https://`/`http://`/CDN）。lane 列布局 Documents/Business/Code/Tests/Risks；layer 分别用 `layer-fact`/`layer-confirmed`/`layer-candidate`/`layer-risk` CSS 类区分；支持搜索、focus、图层开关、节点/边详情面板；SVG 贝塞尔在 resize/filter 后重算。
- 通用旗标：`--focus <id>`（支持 `REQ-*` 稳定键、module path 或完整 artifact id）、`--max-nodes N`（截断时附 `graph_truncated` finding，确保保留 focus 与 confirmed 节点）、`--include-risks`（基于 `compute_checks` 注入 risk findings）、`--include-candidates`（读取 `.specslice/candidates/business_logic.yaml` 的候选层）。

节点分层映射：

- DocSection / AcceptanceCriterion / Adr → `documents` lane, layer=`fact`。
- Requirement → `business` lane, layer=`confirmed`。
- File / Dart {class,method,function,constructor} → `code` lane, layer=`fact`。
- TestCase / TestGroup → `tests` lane, layer=`fact`。
- Findings（来自 checks）→ `risks` lane, layer=`risk`，仅在 `--include-risks` 时出现。

边分层映射：

- `EdgeSource::ExternalManifest` → layer=`confirmed`（来自 `.specslice/links.yaml`）。
- `EdgeCertainty::Fact`（filesystem/parser 提供）→ layer=`fact`。
- 候选：来自 `.specslice/candidates/business_logic.yaml`，作为 `GraphLayer::Candidate` 展示，不参与 confirmed slice。
- risk：来自 checks / candidate 解析问题 / 后续 LogicConfidence。

验收（已通过，2026-05-19）：

- 引擎层 9 个测试覆盖 view model 结构、focus 邻域、未命中 focus、max_nodes 截断、risk findings 与候选空占位。
- CLI 层 7 个 e2e 测试覆盖 JSON/HTML/Mermaid 输出、`--out` 路径、`--focus`、`--max-nodes`，外加 5 个单元测试覆盖 mermaid 别名转义与 HTML payload 中 `</script>` 注入防御。
- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --quiet`、`git diff --check` 全部通过。
- 真实冷启动：`cargo run -- graph --format html` 在 `tests/fixtures/flutter_watermark_app` 上 30s 内产出 26KB HTML，离线 grep 无任何远程 URL。

### P6.1：可视化重构为代码图浏览器

P6 的 lane 平铺布局在 ≥1k 符号的真实仓库（pixcraft 1786 符号 / 334 测试）上会退化成事实堆。P6.1 把可视化重做成「分层、可展开、可聚焦的代码图浏览器」。

**视图模式：**

- `overview`（默认）：顶层 module（按目录聚合），可展开到子 module、文件、符号。
- `code`：与 overview 同构，强调代码结构图样式（模块 box、imports 边）。
- `business`：仅渲染 requirement + 其单跳邻居（docs/impl/test），无 requirement 时显式空状态提示 `specslice connect propose`。
- `focus`：单节点邻域图，复用现有 focus 解析。

CLI：

```bash
specslice graph --format html --view overview      # 默认
specslice graph --format html --view code --focus lib/features/editor
specslice graph --format html --view business
```

**聚合节点：**

- `module`：合成节点，按文件父目录递归聚合，形成 `lib → lib/features → lib/features/editor` 的多层 hierarchy。
- `file` / `dart_class` / `dart_method` / `dart_function` / `dart_constructor` / `test_case` / `test_group` / `doc_section` / `requirement`：保留原有 NodeKind，并通过 `parent_id` 链上溯到 module。
- 每个聚合节点带 `child_count`，UI 折叠态显示 "12 files / 87 symbols"。

**默认规模硬规则：**

- HTML 默认 `--max-nodes` = 80（CLI 在 format=html 且未显式指定时填默认）。
- 引擎按 view 设置 `default_visible`：overview 只对顶层 module 置 true；business 对 requirement + 1-hop neighbours 置 true；focus 对 focus + 1-hop 置 true；code 与 overview 相同。
- 超出 max_nodes 仍走原 priority_order 截断逻辑 + `graph_truncated` finding。

**前端重构：**

- 三栏：左侧可折叠树（按 column 分组）、中央 SVG 画布、右侧详情面板。
- 节点点击=展开/折叠 children；canvas 双击=聚焦邻域。
- 边聚合：渲染前对 hidden 端点沿 parent_id 上溯到可见祖先并去重。
- 搜索同时过滤树与画布。
- 不引入 cytoscape/d3 等外部库；保持自包含、离线、无 CDN。

**验收（必须达成）：**

- pixcraft 冷启动：HTML 打开后默认看到顶层 module 列表，而非 2000+ 节点平铺。
- 点击 `lib/features/editor` 树节点可展开其子 module 或文件。
- 点击 Dart class 节点，右侧详情面板显示 path:line、incoming/outgoing 边、关联测试。
- 业务视图在 pixcraft 上显式空状态（"no business logic yet"），不退化成代码堆。
- `cargo fmt`/`clippy -D warnings`/`cargo test --workspace` 全绿。
- HTML 自包含、零远程 URL（沿用 P6 离线断言）。

### P6.2 / P6.3：代码事实图与证据 ✅ 落地

**目标：** 让图浏览不只是目录树，而能看到真实代码事实。

**已落地：**

- Dart `calls` / `references` 事实边入库并在 graph JSON/HTML 中可见。
- 每条代码事实边携带 source file、line、snippet、resolver。
- 默认降噪：过滤常见 framework noise calls，保留 references 和语义边。
- 无 confirmed business 时，graph 仍可用于代码理解，但不声称业务逻辑已确认。

**边界：**

- `calls` / `references` 是 deterministic code facts，不是业务确认关系。
- 它们可以作为 AI candidate evidence，但不能直接进入 confirmed graph。

### P7：Dart analyzer sidecar ✅ 落地

**目标：** 在可用时用 `package:analyzer` resolved AST 替代 lightweight 启发式解析，提高调用、引用和语义边准确率。

**已落地：**

- `SPECSLICE_DART_ANALYZER=1` 启用 sidecar；`SPECSLICE_DART_ANALYZER_BIN` 可指定入口。
- sidecar 输出 `LanguageIndexBatch` 兼容结构；失败时 fallback 到 lightweight。
- 代码事实边 resolver 标记为 `dart_analyzer`。

**必须持续满足：**

- sidecar 必须与 lightweight 输出同等类别的数据：files、symbols、tests、imports、ranges、references、synthetic nodes。
- sidecar 不执行目标代码、不改目标仓库、不依赖业务注解。

### P8：Flutter / Riverpod 语义边 ✅ 落地

**目标：** 在不依赖业务标注的前提下，把常见 Flutter 业务路径提升成可读事实边。

**已落地语义边：**

- `reads_provider`：Riverpod `ref.read/watch/listen(provider)`。
- `navigates_to`：Flutter / GoRouter / Navigator 路由跳转。
- `persists_to`：Hive / SharedPreferences 持久化目标。
- `subscribes_stream`：Stream subscription。

**边界：**

- 语义边仍然是代码事实，不是 confirmed business link。
- 语义边可用于图浏览、AI 候选证据和澄清问题。

### P9：AI 业务候选层 ✅ 落地

**目标：** AI 可以把文档/代码/测试事实组织成中文自然语言业务逻辑候选，但必须保持候选态，等待人工确认。

**已落地：**

- `.specslice/candidates/business_logic.yaml` 作为候选输入。
- Graph 将候选节点和 `derives_from` evidence 边渲染为 Candidate layer。
- 候选引用不存在时输出 warning finding。

**产品确认流：**

- 输入：代码图中的事实节点和事实边，包括 `calls`、`references`、`reads_provider`、`persists_to`、`navigates_to`、`subscribes_stream`、测试节点和 DocSection。
- AI 输出：面向用户的中文业务描述、建议动作（确认 / 拒绝 / 补充 / 暂缓）、可信度、open questions、evidence。
- 用户确认：用户只需要判断自然语言描述是否符合真实产品设计，不需要手写 `.specslice/links.yaml`，也不需要人工初始标注代码关系。
- 系统写入：被用户确认的候选才能转成外置 confirmed requirement/link；未确认候选继续停留在 `.specslice/candidates/`。
- 风险呈现：测试不足、产品边界不清、外部配置不可见、设备/商店行为不可证明等，应进入补充问题或风险提示，而不是自动失败。

**边界：**

- 候选不进入 confirmed graph。
- `slice` / `context` / `impact` 默认不信任 candidate。
- 人工确认后才写入 `.specslice/links.yaml` 或 `.specslice/requirements.yaml`。
- `status: proposed` 表示 AI 生成且 evidence 可解析；`status: confirmed` 只能表示人工已确认，不允许用“AI/Codex 已审阅”冒充人工确认。

## 当前收口状态

本轮已补齐：

- analyzer sidecar 输出 `test(...)` / `group(...)` 测试事实，避免启用 P7 后测试节点从图里消失。
- focused graph 对 listener / handler 补一跳高信号语义上下文，能保留 caller 上的 stream/provider/storage/route 事实边。

仍然是重大产品边界：

- 对真实业务仓库的“业务逻辑图”仍依赖 AI candidate 输入和人工确认；没有候选文件时只能输出代码事实图，不能声称已经理解业务需求。

### P10：死代码检查 ✅ 落地

**目标：** 提供独立命令 `specslice dead-code`，把代码图变成可用的“死代码可信度报告”而不是删除建议。报告必须给出置信度和理由，不替操作员决策。

**已落地：**

- 新 CLI：`specslice dead-code [--json] [--min-confidence high|medium|low] [--include-tests]`。
- 引擎模块 `dead_code`：基于 store 做三层判断 —— 可达性 BFS（入口点 = `main()` / Route / DartProvider / TestCase / TestGroup / Flutter lifecycle / `public_api_roots`）；按入边 usage（calls / references / declares_verification / reads_provider / persists_to / navigates_to / subscribes_stream）+ 公共可见性 + 路径 ignore glob 分到 high / medium / low；low 专门对应「dead island」。
- 中文 reasons：每条候选都解释「为什么被标可能死」、入边为什么不足以让它活着，以及是否落入 `public_api_roots` / 公共名 / lifecycle 名等减分项。
- `.specslice.yaml` 新增 `dead_code` 配置（默认 `lib/main.dart` 入口、`**/*.g.dart` / `**/*.freezed.dart` / `**/generated/**` / Flutter `app_localizations*.dart` 等 ignore、可选 `public_api_roots`）。
- `--include-tests` 只把无 `declares_verification` 的 `TestCase` / `TestGroup` 作为低可信孤儿测试候选；`test/` 下的普通 Dart helper 函数不作为生产死代码候选。
- `test/**#main` 被视为测试入口点，它调用到的生产符号会参与可达性，避免把“有测试覆盖的生产方法”误报为 dead island。
- Dart 构造器默认不进入 high 置信桶；即使轻量/sidecar 索引把默认构造器命名成 `_default`，也按 medium 处理，因为构造器可能经由类实例化、const 构造或框架创建触发。
- 测试：engine 单元测覆盖 high/medium/low/ignore/include-tests/min-confidence、测试 helper 噪声过滤、测试入口可达性；CLI 集成测覆盖 json schema + 排序 + 文本头；PixCraft golden 验证真实 sidecar 索引下的 schema_version、stats、排序，并断言 `*.g.dart` 被默认 ignore 过滤、`public_api_roots = lib/**` 会把 high 降到 medium 或更低。

**非侵入式约束：**

- 不要求代码或文档加 `@used` / `@business` 注解。
- 不要求用户为了压低误报而在测试里加标记；测试可达性来自代码图与测试事实。
- 任何分析边界（入口点、ignore、public API）都只通过 `.specslice.yaml` 表达。
- 输出可信度而非删除指令，并在文末显式提示运营要先用 `graph --focus` / `search` 复核。

### P11：Agent 工具层与画布交互（MCP + 阅读器升级）✅ 落地

**目标：** 让 AI Agent 不再解析 CLI 文本，让人能围绕搜索目标在画布上展开 / 收起 / 过滤局部业务图。

**已落地（A — MCP 工具化）：**

- 新 crate `specslice-mcp`，提供 `specslice-mcp` 二进制：stdio 上的换行分隔 JSON-RPC 2.0，MCP 协议版本 `2024-11-05`。
- 启动顺序：`--repo-root <path>` → `SPECSLICE_REPO_ROOT` → 当前目录；每条 `tools/call` 还可在 `arguments` 里覆写 `repo_root`，便于 Agent 在多仓库间复用同一进程。
- 协议面：`initialize` 返回 `protocolVersion / serverInfo / capabilities.tools`；`notifications/initialized` 静默接受；`tools/list` 返回带 JSON Schema 的工具清单；`tools/call` 把工具结果包成单个 `text` content block，结构化 JSON 在 `text` 字段内。
- 6 个一线工具：`search_graph(query/code/file+line)` · `get_subgraph(node_id, depth, edge_kinds)` · `explain_symbol(symbol_id)` · `impact(base, head)` · `dead_code(min_confidence, include_tests)` · `context_pack(requirement_id|candidate_id|symbol_id)`，全部复用 engine 现成 API、不引入新的事实通路。
- 工具错误（缺工作区、参数缺失、引擎抛错）走 MCP 约定的 `isError: true` content，传输级错误（未知方法 / 未知工具 / 缺 `name`）才回 JSON-RPC error envelope，不会让 Agent 误把业务失败当成协议崩溃。
- 测试：`dispatcher_*` 直接覆盖 `initialize / tools.list / 未知方法 / 未知工具 / 工具失败`；`end_to_end_initialize_list_search_against_watermark_fixture` 通过真实 `specslice-mcp` 二进制 + watermark fixture 走通 `initialize → tools/list → tools/call(search_graph) → tools/call(dead_code)`，断言每一步的 JSON-RPC 信封、tool 内嵌 JSON 与 `isError` 标志。

**已落地（B — 搜索阅读器再迭代）：**

- `SearchHtmlPayload` schema bump 到 2：新增 `full_subgraph`（所有命中 1-hop 并集）与 `edge_kinds`（按显示优先级排序的边类型目录 `{ kind, count, priority }`）。Schema 2 同时向后兼容：缺字段时通过 `#[serde(default)]` 视为 0 节点 / 空目录。
- HTML 阅读器顶栏新增「按边过滤」chip 行（calls / persists_to / navigates_to / reads_provider / declares_verification / derives_from / contains 等），点击即时隐藏 / 显示对应边并连带过滤右侧上下游列表。
- 画布支持「点击节点展开 1-hop 邻居」：节点右上角实时显示 `+N` badge，点击即从 `full_subgraph` 抽出该节点未上画布的邻居加入视野；展开过的节点画虚框，点一下回收。
- 右侧 Inspector 增加「可展开邻居」段，按起点节点列出隐藏邻居数与类型枚举，配「展开」按钮供没有命中 badge 的场景直接操作。
- 测试：engine `html_payload_includes_one_focus_card_per_match_with_canvas_under_budget` 与 P5 golden 都断言 `schema_version == 2`、`full_subgraph` 非空、`edge_kinds` 按 priority 严格下降；CLI `search_html_writes_self_contained_reader_to_default_path` 额外断言 HTML 中存在 `edge-filter-host` 容器、`按边过滤` 中文标签、`full_subgraph` / `edge_kinds` payload。

**非侵入式约束（同 P10 一致）：**

- 不在代码中加任何注解；MCP 工具与 HTML 阅读器都只读 `.specslice.yaml` + graph.db + `business_logic.yaml`。
- 不主动联网、不依赖任何外部 LLM；Agent 端拿到结构化 JSON 后自由扩词，CLI 保持确定性。
- 阅读器 HTML 单文件可离线打开，不引入 CDN / WebFont / WebGL。

### P12：Swift / Go 多语言 sidecar（LSP 驱动）✅ 落地

**目标：** 把 SpecSlice 的事实图从 Dart 扩出 Swift / Go 两条边，按规划用上游 LSP（`sourcekit-lsp`、`gopls`）当 sidecar，不自己写 parser，统一图模型由语言侧 Adapter 产出节点和边。

**已落地：**

- `specslice-core` 新增 11 个语言节点：`SwiftClass / SwiftStruct / SwiftEnum / SwiftProtocol / SwiftMethod / SwiftFunction / SwiftInitializer` 和 `GoStruct / GoInterface / GoMethod / GoFunction`，全部沿用 `language_prefix_kind` 约定，`as_str` / serde 序列化与 store 解码同步扩。
- 新增通用 `lsp_client`：同步 stdio LSP 客户端，自实现 `Content-Length` 帧、`initialize → initialized → request → exit` 生命周期、`request id` 关联与服务器侧请求自答（避免 gopls 阻塞），并兼容旧 `SymbolInformation[]` 平铺响应通过 `containerName` 重建层级。
- 新增通用 `lsp_indexer`：根据 `LspProfile { language, language_id, file_extensions, skip_dirs, default_command, command_env_var, map_kind, qualify }` 走 `discover_files → didOpen → documentSymbol → didClose → shutdown`，把符号树映射进 `LanguageIndexBatch`；缺二进制 / 无源文件 / 进程异常都返回 `Skipped { reason, language }`，永不让 `index_repository` 失败。
- 新增 `swift_indexer` + `go_indexer`：分别映射 sourcekit-lsp / gopls 的 `documentSymbol` 到 SpecSlice 节点类型；qualified name 采用 `<file>::Type.Member` 形式，跨语言保持稳定。`SPECSLICE_SWIFT_LSP_BIN` / `SPECSLICE_GO_LSP_BIN` 可覆盖二进制路径，`.specslice.yaml` 的 `swift / go.lsp_command` 同样生效。
- `dart_indexer` 抽出 `ingest_language_batch_minimal`（files + symbols + tests + imports + references + symbol_ranges 的通用子集），Swift / Go 全部复用同一入仓路径，不需要为新语言改 store 代码。
- 配置面：`EngineConfig` 新增 `swift / go: LanguageAdapterConfig { enabled, paths, exclude, lsp_command }`，两者默认关闭。`index_repository` 仅在 `enabled == true` 时调度对应 indexer，并在二进制缺失时把跳过原因写进结果，不让现有 Dart-only 仓库感知到任何变化。
- CLI / MCP 适配：`specslice search --kind swift_method,go_struct,...` 直接生效；MCP `search_graph` / `get_subgraph` 的 `kinds` 数组支持全部新 kind 别名；`graph` 列分组与「业务噪声排序」也同步把 swift/go 的方法纳入降权名单（`build / dispose / toString` 等）。
- Dead-code 入口点：Go 自动把 `main / init / Test*/Benchmark*/Example*` 视作入口；Swift 自动把 `main / test* / UIKit/SwiftUI 生命周期回调` 视作入口；`reason_unreached` 针对新 NodeKind 输出对应语言的解释，避免误判反射调用。
- 测试覆盖：
  - 单测：LSP 帧读写 / `SymbolInformation` 重建层级 / `path_to_file_uri` UTF-8 / `simple_glob_match` 的 `*` vs `**` 语义 / Swift & Go `map_kind` & `qualify` / `LspIndexOutcome::Skipped` 路径。
  - 集成测：`tests/fixtures/swift_hello`（含 `Package.swift` / `Sources/Greeter` / `Tests/GreeterTests`）与 `tests/fixtures/go_hello`（含 `go.mod` / `internal/api` / `cmd/server`）。`lsp_indexers.rs` 在无 LSP 二进制时仅验证 `Skipped` 文案；当 `sourcekit-lsp` / `gopls` 真在 PATH 上时，会启服务器、跑 `documentSymbol`、断言节点名与 NodeKind 与 fixture 对得上。
  - 配置 schema：`p11_swift_and_go_sections_parse_with_paths_and_lsp_command` / `index_repository_skips_swift_adapter_when_disabled` / `index_repository_runs_swift_adapter_when_enabled_and_skips_when_lsp_missing` 锁住默认关闭与 enabled-but-LSP-missing 两种行为。

**非侵入式约束（与 P11 一致）：**

- 不写自家 parser；语言事实由上游官方 LSP 服务器（`sourcekit-lsp` / `gopls`）产出，SpecSlice 只做结构化吸纳。
- 不在 Swift / Go 代码中加注解、不依赖运行时反射 / 字符串约定；缺少 `Package.swift` 或 `go.mod` 时优雅退化为「跳过」并保留可读原因。
- 不联网；LSP 完全本地 stdio 通信，CLI / MCP 的搜索 / 死代码 / Context Pack 路径在 Swift / Go 启用后保持同一可信链路。
- 不引入新的事实通路：Swift / Go 沿用 `EdgeKind::Contains`（File → Symbol → Symbol），调用 / 引用边在 P13 通过 `callHierarchy` / `references` 单独引入，仍不会回头改既有 Dart 路径。

**P12 复核修复（已落地）：**

- **LSP 运行期失败一律降级**：`run_profile` 现在把 `spawn / initialize / didOpen / documentSymbol` 的所有错误捕获并写入 `LspIndexOutcome::Skipped` 或 `Indexed { stats.skip_reason }`，不再让 `index_repository` 因 sourcekit-lsp 沙箱权限、`gopls` cache 缺失等环境问题整体失败。`run_profile_downgrades_runtime_lsp_failure_to_skipped` 用 `/usr/bin/true` 冒充 LSP 复现这条契约。
- **read 超时真正生效**：`LspClient` 把 stdout 读取放到后台线程并通过 `mpsc::Receiver::recv_timeout` 等待应答，`set_response_timeout` 到期会立刻 `force_kill` 子进程；新增 `request_times_out_when_lsp_server_never_writes` 用 `sleep 30` 复现「LSP 吃掉请求但不回包」的死锁场景，断言 150ms 超时内 bail。
- **CLI 输出 Swift / Go 段**：`specslice index` 的 `print_result` 拆出 `format_result`，在配置启用 `swift.enabled` / `go.enabled` 时分别打印 files / symbols / resolver_used / skip_reason；五条新 `format_result` 单测同时覆盖「未启用」「Indexed」「Skipped 含 PATH 提示」三种渲染分支。

## P13 Swift / Go callHierarchy + references（已落地）

P12 留下的最大短板是 Swift / Go 只有结构边、没有调用 / 引用关系，导致 `slice` / `impact` / `dead-code` 在多语言仓库里只能停在文件级。P13 在 LSP sidecar 内补上 `callHierarchy` 与 `textDocument/references`，把这两个图谱通路拉齐到 Dart analyzer 的事实级别。

**实现要点：**

- `LspClient` 扩展三组同步 RPC：`prepare_call_hierarchy(uri, line, character)`、`outgoing_calls(item)`、`references(uri, line, character)`。新增 `LspCallHierarchyItem` 与 `LspLocation` 类型，**完整保留服务器返回的 `data` 字段**（sourcekit-lsp 在 `data.usr` 里塞 USR，没回传会被服务器视为不存在的 item，导致 `outgoingCalls` 永远返回空）。
- `initialize` 客户端能力声明里追加 `textDocument.callHierarchy` 和 `textDocument.references`，让上游显式启用 `callHierarchyProvider`，否则 sourcekit-lsp 不会广告对应 provider。
- `LspDocumentSymbol` 额外携带 `selection_line` / `selection_character`（取自 `selectionRange.start`），确保 `prepareCallHierarchy` 拿到的是标识符光标位置而非整条声明开头。
- `lsp_indexer` 在结构事实写完之后做一遍 best-effort 探测：
  - `SymbolResolver` 用 `repo_root` + per-file SymbolRange 表把 LSP 回来的 `Location.uri + line` 反向解析成已索引的 `ArtifactId`；外部 stdlib / 第三方调用解析不到时直接丢弃，**不会自动合成 stub 节点**，保持「非侵入式」契约。
  - `warmup_call_hierarchy` 给每个有 callable 的文件做一轮 `prepareCallHierarchy` 轮询（每 250ms 一次，整体上限 15s）以等 sourcekit-lsp 的 `IndexStoreDB` 异步装入；轮询失败就放弃该探针，不阻塞其他文件。
  - 每个 callable 走完 `callHierarchy/outgoingCalls`（产出 `EdgeKind::Calls`，方向 `from sym → to callee`）与 `textDocument/references`（产出 `EdgeKind::References`，方向 `from caller → to sym`）。两类边都进 `LanguageIndexBatch.references`，最终由 `ingest_language_batch_minimal` 写入 store，**沿用 Dart 的允许集合**，不会引入新的 EdgeKind。
- 跨文件 URI 解析特别处理 macOS：`SymbolResolver::build` 对 `repo_root` 做 `canonicalize`，并对每个 LSP URI 也做 `canonicalize`，避开 `/var/folders/...` ↔ `/private/var/folders/...` symlink 不匹配的死角。
- 关键 bugfix：`code_roots = ["."]` 时 `walkdir` 会保留中段的 `./`，让 `gopls` 报 `no package metadata`；`discover_files` 现在直接用 `repo_root.join(rel)` 重建绝对路径再去算 URI。

**测试覆盖：**

- 单测：`parse_call_hierarchy_items_normalises_kind_selection_and_preserves_data`（断言 `data.usr` 原样保留）、`parse_locations_collects_line_character_for_references`、`file_uri_to_path_round_trips_through_path_to_file_uri` / `file_uri_to_path_handles_lenient_forms`。
- 集成测：`swift_indexer_emits_class_struct_protocol_method_nodes_when_lsp_present` 在 `sourcekit-lsp` 可用且 `swift build` 成功时**强制要求** `EdgeKind::Calls` 至少有一条，且每条目标必须落在已索引节点上；`go_indexer_emits_struct_interface_method_function_nodes_when_lsp_present` 对 `gopls` 做同样的断言（无需预 build，gopls 在初始化阶段就准备好了）。
- 当 `swift build` 缺失或失败时，Swift 集成测试会回落到只验证结构事实并打印一条日志，保证流水线在无 Swift 工具链环境（如纯 Linux CI）下不会假阳性。

**非侵入式约束（沿用 P11 / P12）：**

- LSP 调用仍然是 best-effort：任何一条 `prepareCallHierarchy` / `outgoingCalls` / `references` 错误都不会让 `specslice index` 失败，只会进 `stats.skip_reason` 让 CLI 渲染出来。
- 不联网、不写第三方 IDE 配置、不向用户代码注入注解；新增的 `Calls` / `References` 边纯粹由上游官方 LSP 服务器算出。
- 不引入新的 EdgeKind / NodeKind；callHierarchy / references 落回 Dart adapter 已有的允许集合，所以 `dead-code` / `slice` / `impact` / MCP / HTML / 搜索全链路自动看到 Swift / Go 的调用关系，无需再修一行下游代码。

## P14 Calls / References 通路打通 + 三种局部 Mermaid 导出（已落地）

P13 把 Swift / Go 的 `Calls` / `References` 事实边写进 store，但下游消费仍停在「相同边、相同允许集合」的层面：`impact` / `slice` 实际并没有沿 `Calls` / `References` 跑 BFS，只用了 manifest 边；`MCP get_subgraph` 的边过滤也只接受预定义的 `edge_kinds`，无法按 `swift_lsp` / `go_lsp` 这种 provenance 做隔离审计。P14 闭环这条链路，并在三处局部场景下提供 Mermaid 导出，让 PR 描述 / 设计文档 / candidate 评审稿能直接嵌入小型可视化。

**实现要点：**

- `dead-code` 已经按 kind 接入了 `EdgeKind::Calls` / `EdgeKind::References`（P10 时即如此），P14 仅追加回归测 `swift_lsp_calls_participate_in_dead_code_reachability`：用 `indexer = "swift_lsp"` 的 Swift 调用边验证 `test*` 入口 + Calls BFS 必须把私有 callee 保留为可达，避免未来重构按 indexer 名做误过滤。
- `impact` 新增 `ImpactPropagation { call_depth, max_propagated_symbols }`，默认 `call_depth = 1`、`max_propagated_symbols = 256`。`compute_impact_with_policy` 在 manifest 传播之后调用 `propagate_via_calls_and_references`：从 `changed_symbols` 用 `list_edges_to` 反向 BFS（即「谁调用 / 引用了改动的符号」），新节点写进 `ImpactReport.propagated_symbols`；其中 `TestCase` / `TestGroup` 会被自动并入 `linked_tests`，让「应该跑哪些测试」的答案不再依赖人工的 `declares_verification`。集成测 `impact_propagates_via_calls_and_references_to_callers_and_tests` 同时覆盖 depth=0/depth=2 两条分支。
- `slice` 新增 `SliceFanoutOptions { call_depth }`（默认 `1`）+ `slice_from_store_with_options` / CLI `--call-depth`。沿 `EdgeKind::Calls` / `EdgeKind::References` 正向 BFS（从 declared implementation 出发的 callee），结果写进 `FeatureSlice.code_fanout`，保留 `#[serde(default, skip_serializing_if = "Vec::is_empty")]` 以保证旧 JSON 消费者不破坏。两类传播都加了 256 节点上限并在触达时把截断原因写进 `info` / `risks`，避免噪声图谱炸裂。
- MCP `get_subgraph` 新增 `resolvers: Vec<String>` 入参（schema 同步更新），运行时按 `edge.indexer` 过滤。同时单测 `resolver_allowed_passes_through_when_filter_empty_and_filters_when_set` 锁定语义：空集合 = 全放行；非空集合 = 命中 indexer 才放行，且 `indexer = None` 的旧边在过滤生效时被排除（防止 manifest 边渗漏到 `--resolvers swift_lsp` 这种调试调用）。
- 三类局部 Mermaid 导出共用 `commands/graph_mermaid.rs::render_parts(MermaidNode, MermaidEdge, notes)`：原 `render_mermaid(&GraphViewModel)` 现在是它的薄包装；search / impact / candidate 各写一个 `render_*_mermaid` 把自己的报告映射到 `(id, label, layer, path)` + `(from, to, kind, layer)`，避免反复重复构造完整 `GraphViewModel`。`search --format mermaid --output` 把命中节点渲染成 Confirmed 圆角、扩展邻居渲染成 Fact 矩形；`impact --format mermaid --output` 渲染 `changed_files → changed_symbols → affected_requirements / linked_tests / affected_confirmed_candidates / propagated_symbols`；`candidate show --format mermaid` 把候选自身按 `review_status` 映射成 Confirmed (accepted) 或 Candidate (其他) 形状，每条 evidence id 解析成 `symbol [kind]` 的 Fact 矩形。

**测试覆盖：**

- `crates/specslice-engine/tests/impact.rs::impact_propagates_via_calls_and_references_to_callers_and_tests` — depth=0 baseline + depth=2 双跳，反向 BFS 覆盖 `TestCase` 升级到 `linked_tests`。
- `crates/specslice-engine/tests/slice.rs::slicing_fans_out_via_calls_and_references_from_implementations` — depth=0 退回 manifest-only / depth=1 单跳 / depth=2 双跳三条分支。
- `crates/specslice-engine/src/dead_code.rs::tests::swift_lsp_calls_participate_in_dead_code_reachability` — 回归测，锁定按 kind 接入而非按 indexer 白名单。
- `crates/specslice-mcp/src/tools/get_subgraph.rs::tests` — `parse_resolvers` 合法 / 非法形状、`resolver_allowed` 空集合 vs 非空集合的过滤语义。
- `crates/specslice-cli/src/commands/graph_mermaid.rs` 新增 `render_parts_renders_minimal_diagram_and_drops_dangling_edges` / `render_parts_emits_empty_subgraph_comment_when_no_nodes`，锁定核心渲染器的 dangling edge 容错与空图退化。
- `commands/search.rs::tests::search_mermaid_highlights_matches_as_confirmed_nodes_and_uses_aliases`、`commands/impact.rs::tests::impact_mermaid_renders_changed_files_requirements_tests_and_candidates`、`commands/candidate.rs::mermaid_tests::candidate_mermaid_renders_evidence_as_fact_rectangles_with_accepted_shape` / `candidate_mermaid_uses_candidate_shape_when_not_accepted` 验证三类导出对节点形状、箭头、注释、artifact id 隐藏的契约。

**非侵入式约束（沿用前几期）：**

- `Calls` / `References` 传播按 kind 接入，没有按 indexer 名做特化；任何新接入语言只要写到 `EdgeKind::Calls` / `EdgeKind::References` 就自动获得 `impact` / `slice` / `dead-code` / `MCP get_subgraph` 覆盖，无需再回头改下游。
- 所有传播都带显式深度上限与节点数上限，避免噪声图谱让 `slice` / `impact` JSON 爆炸；触达上限会通过 `risks` / `info` 字段告知调用方。
- Mermaid 导出只读图，不写回 store，也不会把局部子图的"业务推断"反向写入 confirmed graph。Mermaid 文件可以直接贴 PR / 设计文档，但确认逻辑仍走 `candidate review` 显式审阅。

## P15 LSP CI 解耦 + Calls evidence 校准 + Impact 真实边轨迹（已落地）

P14 把 Calls / References 接入了 impact / slice / MCP / Mermaid，但还有三处可信度短板：(1) 真实 LSP 集成测试不稳定，沙箱 CI 跑 `cargo test --workspace --quiet` 会卡在 `sourcekit-lsp` cache 权限或 `gopls` 没产出 Calls 边的 fixture；(2) Swift / Go `Calls` 边的 evidence 写的是 callee 的 `selection_line`，指向"被调用函数声明"而非真正的调用点；(3) `impact --format mermaid` 会把 propagated symbols 全部挂到第一个 changed_symbol，并把 changed_symbols × requirements 做 cross-product，作为摘要图能看，但若用户当真就是误导。P15 闭环这三点。

**实现要点：**

- `crates/specslice-engine/tests/lsp_indexers.rs` 里两条「spawn 真实 LSP」的集成测试改成 `#[ignore = "..."]`：默认 `cargo test --workspace` 只跑「LSP 不存在时优雅退化」的轻量测试，沙箱也能直接绿。开发者要复跑真实 LSP 用 `cargo test -p specslice-engine --test lsp_indexers -- --include-ignored`。Swift / Go 的 `Calls` 硬断言软化为「为空则 eprintln 提示 warmup/proxy 没就绪」，避免 sourcekit / gopls 在不同机器上的非确定性 warmup 把 opt-in 测试搞成假阳性；但每条非空边仍硬断言 `to_node` 必须落在已索引节点里，确保 callHierarchy 不会写脏数据。
- `lsp_client.rs` 新增 `LspOutgoingCall { to, from_ranges }` 与纯解析函数 `parse_outgoing_calls`，覆盖 LSP 规范的 `callHierarchy/outgoingCalls[].fromRanges`（caller 端调用点 range 列表）。`LspClient::outgoing_calls` 现在返回这个新结构，老的「只取 callee」实现被替换。新增单测 `parse_outgoing_calls_returns_from_ranges_alongside_callee` 锁定缺失 `to`、缺失 `fromRanges`、混合 garbage entry 三种边界。
- `lsp_indexer.rs::probe_call_hierarchy_and_references` 改成：解析到的 `outgoing.to` 仅用于 `resolver.resolve` 找 SpecSlice 侧的 callee artifact，`ReferenceEdge` 的 `source_file` / `line` 改写为 caller 自身的 `sym.file_rel` + `outgoing.from_ranges[0].line`（如果 fromRanges 为空则退到 `sym.selection_line`，仍然在 caller 文件而非 callee 声明位置）。`(from, to, kind)` 的 dedup 集合保证多次调用同一 callee 只生成一条边，第一处调用点成为权威 evidence。
- 真实 LSP smoke 测试 `swift_indexer_emits_class_struct_protocol_method_nodes_when_lsp_present` 追加 caller-side evidence 检查：当 sourcekit-lsp 在本机实际产出 `EdgeKind::Calls` 时，至少一条 Calls 边的 `source_file` 必须与 caller node 的 `path` 后缀匹配；如果 Calls 为空则只打印 warmup 提示，避免把 sourcekit / gopls 的非确定性初始化误判成代码失败。
- `ImpactReport` 新增 `impact_edges: Vec<ImpactEdge { from, to, kind }>`，`compute_impact_with_policy` 在三处把真实穿越的边写进一个 `BTreeSet`：(a) `changed_file → changed_symbol` 的结构性 contains 边；(b) 父级 walk 中找到的 `DeclaresImplementation` 边（关键：写的是 *实际声明者* 的 id，而不是把 cross-product 挂到改动叶子上）；(c) `propagate_via_calls_and_references` BFS 走过的真 `Calls` / `References` 边；(d) requirement → docs / tests / impls 的反向真实边。`ImpactEdge` 用 `#[serde(default, skip_serializing_if = "Vec::is_empty")]`，老消费者不受影响。
- `commands/impact.rs::render_impact_mermaid` 改写：所有实质性边都从 `report.impact_edges` 取，`changed_files` 路径会被翻译成 Mermaid 里的 `file::{path}` 伪节点 id。`affected_confirmed_candidates` 来自候选 YAML 而不是 store，因此仍按 first-anchor 合成 `evidence` 边，但箭头标签 `evidence` 区别于 `declares_implementation` / `calls`，读者能看出"manifest link"。注释行额外打印 `impact_edges=<n>`，让审阅者知道这张图基于多少条真实边。

**测试覆盖：**

- `crates/specslice-engine/src/lsp_client.rs::tests::parse_outgoing_calls_returns_from_ranges_alongside_callee` — 纯解析层锁定 `fromRanges`。
- `crates/specslice-engine/tests/lsp_indexers.rs::swift_indexer_emits_*`（`#[ignore]`，`--include-ignored` 才跑）— 真实 sourcekit-lsp 路径下，如果产出 Swift `Calls` 边，则 evidence 必须落在 caller 文件；Calls 为空会作为 opt-in smoke 的软提示输出。
- `crates/specslice-engine/tests/impact.rs::impact_records_real_edges_for_calls_and_requirement_anchors` — `impact_edges` 必须包含 `(class → req, declares_implementation)`（而非 cross-product 到改动叶子）、`(test → req, declares_verification)`、`(caller → callee, calls)`，且 BFS 双跳能补出 `(t → b)` 与 `(b → a)`。
- `commands/impact.rs::tests::impact_mermaid_renders_changed_files_requirements_tests_and_candidates` — 注入真实 `impact_edges` 后 Mermaid 必须出现 `---|calls|`、`---|contains|`、`-->|declares_implementation|`、`-->|declares_verification|`，并且**不再**出现合成标签 `calls/refs`；注释行带 `impact_edges=4`。

**非侵入式约束（沿用前几期）：**

- `ImpactEdge` 仅描述事实图边的轨迹；不引入新的 `EdgeKind`、不修改 store、也不会把 Mermaid 视图反向写回 confirmed graph。
- 真实 LSP 集成测试转为 opt-in 后，沙箱 CI 默认零 LSP 依赖；任何机器没有 `sourcekit-lsp` / `gopls` 都能跑全量 `cargo test --workspace`，与「`specslice index` 的 LSP 缺失即跳过」的运行时承诺一致。
- Calls evidence 改用 `fromRanges` 后，所有上游消费（图详情 / Mermaid / Impact / 审计快照）的 file:line 都指向调用点。LSP 端如果未来还报 `to.uri` 与 callee 不一致的 bug，也不会污染 SpecSlice 自己的 evidence 列。

## P16 Python 适配（LSP-first + AST 补强）（已落地）

P15 闭环 Swift / Go 后，下一道短板是 Python 这条 SpecSlice 用户最常碰到的语言。Python 的动态特性使得「纯 LSP 索引」既无法覆盖 imports / pytest，也无法把 `Calls` 当成强事实，因此 P16 直接把 LSP 与 AST 拆成两条互补通路并强制全部跑过一遍，避免在 CI / 沙箱里只有 LSP 时再静默吞掉 import / 测试事实。

**实现要点：**

- `specslice-core::NodeKind` 新增 `PythonModule / PythonClass / PythonFunction / PythonMethod`，并接入 `column_for / kind_rank / is_code_kind / is_implementation_kind / collect_orphan_symbols / search.default_search_kinds / search.kind_to_zh / MCP & CLI 的 --kind 解析 / store 字符串↔kind 映射`。CLI / MCP 全部接受 `python_function / py_function / pyfunc` 等别名，方便 Agent 与人手都能写。
- `crates/specslice-engine/src/python_ast.rs` 是纯 Rust、无依赖的 Python AST 扫描器（indentation + 行级 token）：识别 `class`、`def`、`async def`、`import` / `from ... import ...`、`@pytest.fixture`、`@pytest.mark.parametrize`、`Test*` 类、`test_*` 函数；同时跟踪三引号 docstring 块避免误把 docstring 里的 `def` 当成符号。
- `crates/specslice-engine/src/python_indexer.rs` 是入口适配器：复用 `LspProfile` 通路驱动 `pyright-langserver` / `basedpyright-langserver` / `pylsp`，按 `SPECSLICE_PYTHON_LSP_BIN → python.lsp_command → <repo>/.venv/bin/{basedpyright,pyright,pylsp}-langserver → PATH` 顺序自动发现，并强制带 `--stdio`。env / `python.lsp_command` 是「权威覆盖」：操作者点名某个 binary 但不存在时直接降级到 AST，不再静默回退到 PATH 上的同名工具。
- LSP 跑完后 *再* 跑一遍 AST 扫描：永远补齐 `Imports` 边与 pytest `TestCase` / `TestGroup`；如果 LSP 失败或跳过，AST 还会把 `python_class / python_method / python_function` 这些结构补齐，确保「没装任何工具链」时仍然有可用图。LSP 端节点打 `indexer = python_lsp`，AST 端节点打 `indexer = python_ast`，可以分别 clear / 调试。
- `EngineConfig` 新增 `python: LanguageAdapterConfig`，默认 `enabled: false`，shape 与 swift / go 对齐。CLI `specslice index` 输出新增 `Python index:` 段，列出 files / Symbols / TestCases / Imports / Resolver / LSP skipped reason，方便操作者一眼看清 LSP 是否真的接上。
- `dead-code` 入口规则补 `is_python_entry_name`：`main / __main__ / app / cli / create_app / run` 与 `__dunder__ / test_*` 模式视为框架/反射入口，避免把 pytest 测试和 Flask app instance 当成 high 置信死代码。`reason_unreached` 文案同步加 Python 分支。

**测试覆盖：**

- `python_ast::tests::*` — 5 个单测覆盖结构识别（含 `async def`）、imports（含 `from ... import ... as ...` 与相对导入）、pytest fixture / parametrize / `Test*` 类、docstring 内的 `def` 不应被识别、`end_line` 在 outdent 后正确收口。
- `python_indexer::tests::ast_pass_emits_imports_pytest_tests_and_structural_symbols_without_lsp` — 内置 fixture，强制 LSP 命令为不存在的二进制并禁用 venv 发现，验证 AST fallback 能产出 `python_class / python_method / python_function / TestCase / TestGroup`，并产出 `tests/test_greeter.py → app/greeter.py` 的 `Imports` 边。
- `python_indexer::tests::python_lsp_available_respects_options_override` 与 `python_qualify_uses_dot_for_methods / resolve_python_import_handles_packages_and_relative_imports` — 锁定操作者 override 的"authoritative"语义、qualified-name 拼接、相对导入跨包解析。
- `tests/fixtures/python_hello` + `tests/lsp_indexers.rs::python_indexer_ast_pass_runs_against_python_hello_fixture_without_lsp` — 全链路：从 fixture 拷贝到 tmpdir、跑 `index_python`、直接读 store 校验节点 / 边、确认 `Resolver=python_ast` 与 `LSP skipped` 文案。
- `python_indexer_emits_class_function_method_nodes_when_lsp_present` —`#[ignore]` 的真实 LSP smoke，需要本机有 `pyright/basedpyright/pylsp` 才跑，与 Swift / Go 一样靠 `--include-ignored` 触发。
- `commands/index.rs::tests::render_includes_python_section_with_*` — CLI 输出格式锁定，操作者必须能从 `Python index:` 段同时看到 LSP 接通 (`python_lsp`) 与 AST 降级 (`python_ast`) 两种状态。

**非侵入式约束（沿用前几期）：**

- Python `Calls` 边仍然只来源于 LSP `callHierarchy/outgoingCalls`，AST 路径**不会**伪造 `Calls`。文档里明确把 Python `Calls` 归为「线索而非强事实」。
- AST 扫描器不依赖第三方解析器：纯 Rust 实现避免引入 `rustpython-parser` / `tree-sitter` 体量；扫描结果只用于补 Imports / pytest，结构语义保留 LSP 的优先权。
- 配置 `python.enabled: false` 时，整条 Python 通路不开启，沙箱与 Dart-only 工作流的行为零变化。

### P16 真实仓库回归（atagent / 165 py 文件）

P16 落地后我们在 `/Users/qjs/Code/Projects/atagent`（典型 FastAPI 后端，~165 py 文件，无 `__init__.py` 的 `backend/` src layout）做了非侵入式验证，只在仓库根写入临时 `.specslice/` 工作区并在结束时清理，未触碰任何业务源码。验证结论：

- AST 通路在没有可用 LSP 时仍能产出可用图：files 162 / Symbols 1216 / TestCases 272 / Imports 662 / Resolver `python_ast`。其中 LSP 是因为 PATH 上的 `pylsp` shebang 指向已删除的 Anaconda 解释器导致 `execve(2)` 失败，被 `python_indexer` 优雅降级，CLI 输出明确写出 `LSP skipped: 无法启动 python LSP …`。
- 暴露并修掉一个 src-layout 解析 bug：未引入 `discover_python_src_roots` 之前，`from app.core.config import settings` 这类导入只匹配纯扁平布局，仅 34 / 244 `from app.*` 命中；现在自动从 `__init__.py` 链反推 `backend/` 等 src 根目录，命中数提升到 662（≈20×），覆盖 atagent 几乎全部包内导入。
- `dead-code --json` 在 LSP 缺席时仍按设计返回较高 candidate 数（1066/1216）。这不是 bug：Python AST 通路不会伪造 `Calls / References`，因此可达性主要依赖 file→Imports→file 链路。当前结果与"AST-only 模式仅适合作为线索 / 后续 P17 框架装饰器入口收敛"的契约一致；同时 entry 集合已经覆盖 pytest test_*、dunder、`main / app / cli / run / create_app` 等典型 Python 框架触发点，避免把框架反射调用误报为高置信死代码。
- 其余 CLI 流程（`check / logic / candidate list / search --format mermaid`）在该仓库上零异常，搜索 `Middleware` 子图能正确返回 `python_class`、`python_function` 节点与 `Contains` 边，证明 P16 与既有 search / mermaid 通路完全打通。
- 已新增 `python_indexer::tests::resolve_python_import_handles_src_layout_via_discovered_roots / discover_python_src_roots_includes_repo_root_for_flat_layout`，把 atagent 这类 src-layout 直接锁回归。

## P17 Python 框架装饰器识别（FastAPI / Flask / Celery / Click / Pydantic …）（已落地）

P16 之后 Python 通路已能稳定产出 imports + pytest + 结构符号，但在 LSP 缺席的 AST-only 模式下，FastAPI / Celery / Click 这类「框架反射调用入口」仍然会被 dead-code 误报为可疑死代码。P17 直接消除这一短板：把 Python 装饰器升格为「框架事实」，让 dead-code 知道哪些符号其实由框架触发。

**实现要点：**

- `crates/specslice-engine/src/python_ast.rs` 在扫描 `def` / `async def` / `class` 时，把已积累的 `@decorator` 文本（去掉前导 `@`）整体迁移到 `PythonSymbol::decorators: Vec<String>`，保留括号 / 关键字参数原样以便分类器做精确匹配；docstring 内的 `def` 仍然不会被识别。
- 新模块 `crates/specslice-engine/src/python_frameworks.rs` 提供 `classify_decorators(&[String]) -> Option<FrameworkRole>`，覆盖以下 9 种角色：FastAPI 路由（`@router.get/post/...`、`@app.api_route`、`@app.websocket`，仅当对象名匹配 `app/router/api/blueprint/bp/*Router/*_router/*_app` 时；天然过滤 `httpx.get` 等同名干扰）；Flask 路由（`@app.route` / `@bp.route`，并提取 `methods=[...]`）；Django 视图装饰器（`@login_required` / `@require_http_methods` 等）；Celery / RQ / Dramatiq 任务（含 `queue=` kwarg 透传）；Click / Typer CLI；FastAPI lifecycle（`on_event`）；FastAPI exception_handler / middleware（即 ASGI infrastructure，在 atagent 上正是这两类导致 4 个真实入口被误判）；SQLAlchemy `event.listens_for`；Pydantic `validator / field_validator / model_validator`；以及 `dataclass / attrs.define`（仅元数据，不视为入口）。
- `specslice-core::SymbolArtifact` 新增 `metadata_json: Option<String>`，沿用 `serde(skip_serializing_if = "Option::is_none")` 保持现有序列化最小化；`ingest_language_batch_minimal` 把它写入 `Node::metadata_json`，与之前的 `indexer / index_generation / source_file` 同等流转。所有现有 SymbolArtifact 构造（dart parser / dart references / lsp_indexer / dart_indexer 测试 / lang-dart 测试）都同步补齐 `metadata_json: None`，保持向后兼容。
- `python_indexer.rs` 在 AST pass 推送结构符号时调用 `classify_decorators`：命中入口角色（`FastapiRoute / FlaskRoute / DjangoView / BackgroundTask / CliCommand / EventHandler / AsgiInfrastructure / SqlAlchemyEvent / PydanticValidator`）就把序列化后的 JSON 写到 `SymbolArtifact::metadata_json`，并把 `outcome.framework_entrypoints` 累加。`PythonIndexResult` 暴露 `framework_entrypoints: usize`，CLI `Python index:` 段固定打印 `Framework entrypoints:`（即便 0 也打印，方便操作者诊断分类器是否生效）。
- `dead_code.rs` 在 entry 种子阶段新增 `is_python_framework_entrypoint_metadata`：对每个 `is_code_kind` 节点解析 `metadata_json`，若反序列化成 `FrameworkRole` 且 `is_framework_entrypoint() == true` 就纳入 entry 集合。这意味着 `@router.get(...)` 包裹的 handler、`@app.task` 包裹的 Celery 任务、`@validator(...)` 包裹的 Pydantic 校验器都自动逃出"未引用 → high confidence 死代码"的判定。`data_class / attrs.define` 之类纯元数据角色保持非入口语义，不会污染可达图。
- `search.rs` 在 `make_match` 里读 `metadata_json` 并通过新增的 `framework_family` 提取 `family()`（`fastapi_route / background_task / pydantic_validator / ...`），写入 `SearchMatch::framework_role`。CLI 文本输出新增 `框架角色:` 一行；JSON / MCP / HTML 自动透传，AI Agent 不必再去重新解析 metadata_json。

**测试覆盖：**

- `python_ast::tests::captures_decorators_on_classes_and_functions` — 锁 raw decorator 文本对 `router.get("/items", tags=[...])` / `router.post / app.task / dataclass / pytest.fixture` 等典型形态都保留原貌（含括号 / kwarg）。
- `python_frameworks::tests::*` — 9 个独立分类用例覆盖：FastAPI verb 白名单 + `httpx.get / os.get` 反例、Flask methods kwarg、Celery `shared_task / app.task(queue=...)` 和 RQ `job(queue=...)`、Click / Typer 区分、FastAPI lifecycle、ASGI exception_handler / middleware、Pydantic vs dataclass 入口语义、pytest fixture 不会被误判、`classify_decorators` 多装饰器场景按外层框架取胜。
- `python_indexer::tests::framework_decorated_symbols_get_metadata_and_entry_status` — 端到端：写一个含 FastAPI route / Celery task / Click command 的 fixture，跑 `index_python`，反序列化 Node `metadata_json` 回 `FrameworkRole`，验证 `family()` 与 `is_framework_entrypoint()` 在落库后仍然准确。
- `dead_code::tests::python_framework_decorated_symbols_are_treated_as_entrypoints` — 直接给 Store 注入三种 Python 节点（route / dataclass / 无 metadata helper），跑 `analyze_dead_code_with_store`：route 必须不在 dead 列表，dataclass 与 plain helper 仍然在 dead 列表，锁定 "framework metadata = 入口" 与 "data_class 不是入口" 的双向保证。
- `search::tests::framework_family_extracts_role_label_from_metadata_json` + `search::tests::search_match_carries_framework_role_for_decorated_symbols` — 上行验证 `framework_family` 对合法 / 不合法 JSON 的健壮性，下行确保 `SearchMatch::framework_role` 在 keyword search 命中路径上真正被填充。
- CLI 端 `render_includes_python_section_with_lsp_resolver_when_indexed` 锁 `Framework entrypoints: 4` 文案。

**atagent 真实仓库验证（沿用 P16 验证方法，不修改任何业务代码）：**

- Python files / Symbols / TestCases / Imports：162 / 1216 / 272 / 662（保持 P16 水平）。
- 新增 `Framework entrypoints: 45`：22 个 FastAPI route（`router.get/post/patch/delete`、`app.get` 等）+ 19 个 Pydantic validator + 4 个 ASGI infrastructure（exception_handler / middleware）。`dataclass`（17 处）按设计仅记录元数据，不进入入口。
- `dead-code --json` 在加入 P17 entry 规则后从 `possibly_dead: 1066` 降到 `1026`（-40，剩余 5 个属于 main.py 配置入口与 framework decorator 重合的去重）；同时 `python::backend/app/api/v1/endpoints/blocks.py::list_blocks` 等 22 个 FastAPI handler 彻底退出死代码列表，避免误删生产路由。
- `specslice search "list_blocks"` 在 CLI 文本输出里直接显示 `框架角色: fastapi_route`，AI Agent 与人工读图都能立刻看清"这是被 FastAPI 反射调用的 route"。
- 验证完成后已清理 `/Users/qjs/Code/Projects/atagent/.specslice` 工作区，目标仓库代码零修改。

**非侵入式约束（沿用 P16）：**

- 仍然不依赖业务代码注解、不写入业务源文件、不强制装第三方 Python 工具。AST 分类器纯 Rust 实现，不引入 `rustpython-parser` / `tree-sitter`。
- Python `Calls` 边仍然只来源于 LSP；P17 不试图从 AST 推断 call graph，只在装饰器层提取「外部触发入口」事实。
- `metadata_json` 字段对老配置完全向后兼容：旧 Node 行 `metadata_json IS NULL` 走原 dead-code 规则，新行才参与框架入口判定。

## P18 相似代码候选 — tier 1 结构指纹（已落地）

P18 的整体目标是把"两个函数其实是同一段逻辑的不同副本"作为候选报告呈现，但不自动合并、不自动删除——把判断权留给人和 AI。tier 1 只做"结构完全相同"这一最确定的子集；近似重复（tier 2，token shingles / SimHash）与业务重复（tier 3，图邻域）会在后续迭代里以同一份归一化 token 流为基础叠加。

**实现要点：**

- 新模块 `crates/specslice-engine/src/similarity.rs` 提供 `Language::{Python, Dart}` 共用的归一化器：剥掉标识符（→ `ID`）、字符串 / 数字字面量（→ `STR` / `NUM`）、Python `#` 注释 / Dart `// /* */` 注释、Python 三引号 docstring、所有空白；同时保留 `if / elif / else / for / while / return / yield / def / class / import / from / try / except / async / await / and / or / not / in / is / None / True / False / ...` 等结构性关键字与多字符运算符（`==`、`!=`、`->`、`=>`、`//`、`**`、`::`、`..` 等），从而让 `+` 与 `-`、`==` 与 `=` 这类语义关键差异不会被折叠掉。
- 归一化结果通过 FNV-1a 哈希得到 64 bit 结构指纹；`analyze_similarity_with_store` 扫描所有 `python_function / python_method / dart_function / dart_method / dart_constructor` 节点，按指纹分桶。`min_tokens`（默认 12）过滤掉 `return None` / `pass` 这类只会污染报告的 trivial body；`min_cluster_size`（默认 2）控制最小报告粒度；`focus_symbol_id` 在 `--node SYMBOL_ID` 模式下只返回包含该节点的簇。
- 报告字段保持 AI 友好且只含候选语义：`schema_version: 1`、`stats { symbols_scanned, symbols_skipped, clusters_reported }`、`clusters[]: { fingerprint, duplicate_type: "exact_ast", recommendation: "review", normalized_token_count, members: [{ id, kind, label, path, line_range }] }`。tier 1 永远写 `recommendation: "review"`，不会输出 `consolidate / keep_separate`——那些应交给 tier 3 + 人工。
- 新增 CLI `specslice similar [--node SYMBOL_ID] [--min-tokens N] [--min-cluster-size N] [--format text|json]`。文本输出按 normalized_token_count 降序排列簇，方便操作者先看「最大段重复」；JSON 通路供 MCP / 上层 Agent 直接消费。

**测试覆盖：**

- `similarity::tests::normalize_strips_identifiers_literals_and_comments` — Python 同结构、不同字段名 / 字面量 / 注释的两个函数归一化后 token 流必须完全一致。
- `similarity::tests::normalize_drops_python_docstrings` — 三引号 docstring 必须被剔除，避免 copyright header 污染指纹。
- `similarity::tests::normalize_dart_handles_line_and_block_comments` — Dart `//` 与 `/* */` 注释都被剔除。
- `similarity::tests::fingerprints_differ_when_structure_differs` — `return a + b` 与 `return a - b` 必须产出不同指纹，否则会把语义不同的代码误聚成簇。
- `similarity::tests::analyze_returns_cluster_for_two_structurally_identical_python_functions` — 端到端：写两个仅参数名 / 局部变量名 / 字符串字面量不同的 Python 函数，跑 `analyze_similarity_with_store` 必须输出 1 个 cluster、两个成员、`duplicate_type = exact_ast`、`recommendation = review`。
- `similarity::tests::analyze_drops_clusters_below_min_tokens` — 两个 `def f(): pass` 在 `min_tokens=10` 下必须被 skip，不会成为簇。
- `similarity::tests::analyze_filters_to_focus_symbol_when_requested` — `focus_symbol_id` 指向单成员符号时返回空；指向多成员簇时只返回该簇。
- CLI 端 `commands::similar::tests::text_output_lists_cluster_members_and_recommendation` — 锁 JSON 序列化包含 `duplicate_type / recommendation / members` 关键字段。

**atagent 真实仓库验证（沿用 P16 / P17 验证方法，不修改任何业务代码）：**

- 索引规模与 P17 一致：1367 Python 符号、235 Python 文件、668 imports、45 framework entrypoints。
- `specslice similar` 扫描 1043 函数 / 方法（跳过 63 个 body < 12 tokens 的 trivial），输出 **107 个结构重复簇**；过滤掉 `vscode-copilot-chat/...` 第三方 fixture 后仍有 **96 个 backend/ 内部纯净簇**，全部为真实重构候选。
- 最大簇（187 tokens, 2 成员）：`UIFactory.create_design_failure_blocks` vs `UIFactory.create_edit_image_failure_blocks` — 同为「header MD block → empty gallery block → 2 个 retry action」结构，仅 `UITextKeys` / `ActionID` 字面量不同；是教科书级别的"复制 handler 改字段名"重复。
- 第二大簇（139 tokens）：`RunEventWriter._ensure_pool` vs `RunEventStreamer._ensure_query_pool` — 数据库连接池初始化逻辑被复制到两个不同 writer。
- 第三大簇（136 tokens）：`stylist_agent._request_design_image_logic` vs `stylist_agent._request_person_image_logic` — 同 agent 内部两条 request 路径结构一致，参数 / 服务调用名不同。
- 簇大小分布：2 成员 76 簇 / 3 成员 18 簇 / 4 成员 8 簇 / 5 成员 3 簇 / 6 成员 2 簇，整体形状符合预期（多数为成对复制，少数为基类 + 派生类的镜像实现）。
- `specslice similar --node ... --format json` 模式输出符合 schema_version=1 的 JSON，可直接灌入 MCP / 自动化流水线。
- 验证完成后已清理 `/Users/qjs/Code/Projects/atagent/.specslice` 工作区，目标仓库代码零修改。

**显式延期（不在 tier 1 内）：**

- tier 2 近似重复（70%~95% 相似）：基于已有 token 流叠加 shingle + SimHash / MinHash，单独迭代。
- tier 3 行为重复：基于代码图邻域（共调用、共测试、共 route / storage）判定语义等价。需要 P19 的 confidence / evidence 升级落地后再开工，避免没有 evidence 就把"看似相似"的两段代码推荐为可合并。
- HTML / Mermaid 报告渲染：与 P14 search/impact/candidate Mermaid 同一通路，等 tier 2/3 落地后一次性接入。
- MCP `find_similar` 工具：CLI 接口先稳定一两个版本，确认 schema 后再接入 MCP 工具描述符。
- `--changed-only --base origin/main`：与 P19 `Graph Diff` 子项绑定，统一在那里实现 diff-aware 子图。

## P18 tier 2 SimHash 近似重复（已落地）

承接 tier 1 的归一化 token 流，叠加 k-shingle + SimHash + Hamming 距离，覆盖「70%~95% 相似」这一段最常见的"复制后改字段名 / 改错误文案 / 加一两行"重复。仍然只是候选报告，不会自动合并 / 自动删除。

- 算法：默认 shingle 宽度 k=5，每个 shingle 用与 tier 1 同族的 FNV-1a 计算 64-bit hash；每个 bit 按出现次数累加投票得到 SimHash。两个 SimHash 的 Hamming 距离 / 64 即"距离"，反推相似度 `1 - h/64`。
- 调度：对未被 tier 1 锁定的符号做 O(N²) 配对；`--max-pairwise` (默认 20000) 触发后 near tier 跳过并在 `stats.near_pairwise_skipped = true` 上写入警告。
- 簇生成：union-find（路径压缩 + rank 合并）按"配对距离低于阈值"合并；簇的 `similarity_score` 取簇内最小 pairwise 分数（保守下界），`normalized_token_count` 取成员中位数。
- 新增 CLI flags：`--mode exact|near|all` (默认 all)、`--min-score 0.85` (默认)、`--shingle-k 5`、`--max-pairwise 20000`。
- 报告 schema 保持 v1 向后兼容：新字段 `similarity_score: Option<f32>`，exact 簇序列化时跳过；`stats` 多出 `exact_clusters / near_clusters / near_pairwise_skipped`。

**atagent 真实仓库验证：**

- 修正 `.specslice.yaml`：Python 索引器读 `python.paths`，不读 `roots.code`。确认后只索引 backend/（165 文件，1224 符号），不再混入 vscode-copilot-chat fixture。
- `specslice similar --mode all` 输出 **96 exact + 55 near = 151 簇**。
- 最大 near 簇：`measurement_agent_node` ≡ `stylist_agent_node`（1240 tokens, 相似度 0.859）— 两个 agent node 函数 86% 结构相同，是 P18 tier 2 的"招牌发现"，tier 1 完全捕获不到。
- 中型 near 簇：`UIFactory._build_measurement_basic_form_definition` ≡ `_build_measurement_photo_form_definition`（272 tokens, 0.844）；`form_service._create_with_retry` ≡ `task_timeout_sweeper._sweep_once`（262 tokens, 0.859）— 后者是跨服务的 retry 逻辑复用。

## P19 base — 每条边 `evidence_quality` 派生（已落地）

`crates/specslice-engine/src/edge_confidence.rs` 把 `(EdgeKind, EdgeSource, EdgeCertainty, EdgeStatus, indexer)` 五元组折叠成 `EdgeConfidence::{High, Medium, Low}`。`GraphEdge` 序列化时多写一个 `evidence_quality: "high"|"medium"|"low"` 字段，consumers（人 / AI / MCP）不需要重新解析 provenance 元组就能筛选。

判定规则（保守优先，未知组合一律降级 Medium）：

- `EdgeStatus::Deprecated` → Low（哪怕 LSP 边也降级）。
- `DerivesFrom` → Low（AI 业务候选证据）。
- `EdgeSource::GitDiff` → Low（provisional）。
- `Contains` → High（任意 source，AST 父子关系是确定事实）。
- `Documents` (Markdown) → High。
- `DeclaresImplementation` / `DeclaresVerification` (ExternalManifest) → High。
- `Imports` → High（即使 Python 动态语言，import 解析仍是词法确定的）。
- `Calls` / `References` / `ReadsProvider` / `NavigatesTo` / `PersistsTo` / `SubscribesStream`：indexer 名以 `_lsp` 结尾或 `dart_analyzer` → High；`_ast` 结尾 → Medium；其它 → Medium。

atagent 验证：2046 条边全部判定为 high（1579 contains + 467 imports），完全符合"AST-only 模式只有结构 fact"的预期。

## P19 — `specslice select-tests`（已落地）

`crates/specslice-engine/src/test_selection.rs` 复用 `compute_impact_with_policy` 取得 changed_files / changed_symbols / propagated_symbols，再按三条规则选测试：

1. `test_file_directly_changed` — 测试文件本身在 diff 里。confidence high。
2. `references_changed_symbol` — Calls / References 指向变更符号；沿 Contains 链回溯到最近的 TestCase / TestGroup。confidence high。
3. `imports_changed_module` — 测试模块 Imports 任一变更符号的祖先模块。confidence medium。

`--include-deps` 才会启用 impact 的反向 BFS（传播 max-depth 默认 2）；否则保守只看前两条规则。

CLI：`specslice select-tests --base main [--head HEAD] [--include-deps] [--max-depth N] [--format text|json]`。

atagent HEAD~1..HEAD 验证：6 改动文件 → 6 改动符号 → 选出 85 个测试（2 high + 83 medium）。

## P19 — `specslice features`（已落地）

`crates/specslice-engine/src/feature_map.rs` — 两阶段启发式聚类：

1. **种子打分**：File / PythonModule / DartClass / SwiftClass 节点作为种子；descendants 中每识别一个框架装饰器（FastapiRoute / PydanticValidator / AsgiInfrastructure 等）+5 分，每个 TestCase +1（≤20）。
2. **标签传播**：从 top-N 种子按 Contains（双向）/ Imports / Calls / References 做 max_depth（默认 3）的 BFS；同一 ArtifactId 由更高分种子接管，距离同 BFS 一次写入避免后续 per-member BFS（atagent 5s vs 55s）。

CLI：`specslice features [--max-clusters 20] [--max-depth 3] [--min-cluster-size 3] [--format text|json]`。

atagent 验证：18 个功能区，包括 `app · core · config`（89 nodes）、`app · api · v1 · endpoints · conversations`（179 nodes, fastapi_route）、`app · main`（160 nodes, asgi + fastapi）等，与人工 mental model 高度匹配。

## P19 — `specslice graph-diff`（已落地）

`crates/specslice-engine/src/graph_diff.rs` 比对两份 `.specslice/graph.db` 快照（典型用法：CI 把 base / head 的 graph.db 都缓存下来），输出：

- `nodes_added` / `nodes_removed` / `nodes_kind_changed`
- `edges_added` / `edges_removed` / `edges_status_changed`（捕获 confirmed → deprecated 等转变）

CLI：`specslice graph-diff --base-db <path> --head-db <path> [--format text|json]`。

显式延期：driver 还不会自动 reindex 历史 commit（需要 worktree 隔离 + 历史索引器 cache）；当前 CLI 期望调用方自己保存好两份 graph.db。下一轮迭代再做"一行命令 reindex base..head"。

## P19 — `specslice questions`（已落地）

`crates/specslice-engine/src/questions.rs` 把"代码图里需要人 / Agent 确认的事实"做成可读的问题列表。MVP 四个类别：

- `orphan_symbol`（info）— 没有任何 Calls/References/Imports 入边且没有框架装饰器的代码符号。
- `pending_candidate`（warn）— AI 业务候选还没有被 `DeclaresImplementation` 边确认进入 confirmed graph。
- `test_without_references`（info）— TestCase / TestGroup 节点没有任何到代码符号的 Calls/References。
- `dangling_import`（info）— 测试文件 Imports 了一个图里不存在的目标（外部依赖 / 跨语言 / 被排除目录）。

每条问题都附带 `artifact_id` 与 `path`，AI 可以直接喂给 `specslice graph --focus <id>` 取上下文。

CLI：`specslice questions [--max-per-category 20] [--format text|json]`。

atagent 验证：5 orphan_symbol + 5 test_without_references（受 --max-per-category 5 限制）。orphan 示例：`backend/alembic/env.py::do_run_migrations` — Alembic 框架反射调用的 entrypoint，AI 一问即可确认。

## P20 — TypeScript / Java + 多语言一致性收口（已落地）

P16/P17/P18 把 Python 通路打通到框架装饰器与相似聚类后，仍然遗留五道"声明已收口但用户跑起来会立刻碰壁"的真实门：(1) TypeScript / Java 实际还没有进图；(2) Python opt-in LSP smoke 在 shebang 损坏的 `pylsp` 上不会软跳过；(3) `specslice questions` 的 pending candidate 检测从 `graph.db` 节点而不是 `.specslice/candidates/business_logic.yaml` 读，真实候选完全不会出现在报告里；(4) `graph-diff` 文档承诺了 candidate diff，实现里只有 nodes / edges；(5) `is_code_symbol` 在 `questions.rs` 被复制了一份不完整版本（漏 `SwiftInitializer / SwiftEnum / SwiftProtocol / GoInterface / PythonModule`），证明跨语言一致性已经在漂移。P20 同时解掉这五道。

**实现要点：**

- **统一语言能力层**：新模块 `crates/specslice-core/src/language_traits.rs` 提供 `language_of(kind) / family_of(kind) / is_code_symbol / is_callable / is_type / is_module_or_file / is_test / similarity_supported / default_dead_code_reason / search_aliases`。`questions.rs` / `dead_code.rs` / `feature_map.rs` / `slice.rs` / `search.rs` / `specslice-mcp::tools::parse_node_kind` / `specslice-store::repositories::node_from_row` 全部改为调用这个模块，不再各自维护 `match` 分支。`language_traits::tests` 包含 41 项 `ALL_KINDS` 全枚举矩阵测试，新加语言时漏掉任何一个 family/predicate 都会立刻爆红。
- **NodeKind 扩 11 类**：`TypescriptModule / TypescriptClass / TypescriptInterface / TypescriptEnum / TypescriptFunction / TypescriptMethod` 与 `JavaPackage / JavaClass / JavaInterface / JavaMethod / JavaConstructor`，串联 `as_str` / serde / store 解码 / search 默认 kind 集合 / MCP `parse_node_kind` 别名（`ts_class / java_method` 等）。
- **TypeScript adapter**（`typescript_indexer.rs` + `typescript_ast.rs`）：
  - LSP-first via `typescript-language-server --stdio`（自动嗅 `node_modules/.bin/typescript-language-server` → PATH，`SPECSLICE_TYPESCRIPT_LSP_BIN` 覆盖），失败原因写进 `sidecar_skip_reason`。
  - AST 补强始终运行：识别 ESM `import` / 重导出 / 副作用 import / `class / interface / enum / function`（含 `async / abstract / declare` 修饰）/ 类方法 / 装饰器 / `vitest / jest` 的 `describe / it / test`；用字符串引号 & brace stack 跟踪保护避免误识别字符串里的 `class`。
  - Module-level `TypescriptModule` 节点稳定 id（`ts_module::<rel>`），相对 import 沿 `./foo / ./foo/index.ts(x)` 解析回仓内文件 id；包名 / `node:` 等 bare specifier 透传为 `to_path` 供 dangling import 问题报告。
- **Java adapter**（`java_indexer.rs` + `java_ast.rs`）：
  - LSP-first via `jdtls`（`SPECSLICE_JAVA_LSP_BIN` 覆盖），AST 补强始终运行。
  - 识别 `package x.y.z;` / `import [static] x.y.Z;` / `class / interface / enum / record / @interface`（含修饰符栈 `public / private / protected / static / final / abstract / default / synchronized / native / transient / volatile / strictfp`）/ 方法 / 构造器；保留 raw 注解串（`Test / GetMapping("/api")` 等）便于框架分类器后续接入。
  - 类型限定按 package 拼接（`com.example.Greeter`），同一 package 的多份文件只发一个 `JavaPackage` 节点；JUnit `@Test / @ParameterizedTest / @RepeatedTest / @TestFactory / @TestTemplate / @Theory` 注解的方法直接升格为 `TestCase`。
- **`specslice questions` 改为 YAML 优先**：pending candidate 检测从 `.specslice/candidates/business_logic.yaml`（经 `load_business_candidates`）加载，按 `review_status ∈ { pending, needs_changes, missing }` 报 warn 级问题；并保留对 `BusinessCandidate` store 节点的回退路径以兼容老仓库。`questions.rs` 内部不再维护 `is_code_symbol` 副本，全部走 `language_traits::is_code_symbol`。
- **`specslice graph-diff` 补齐 candidate diff**：`GraphDiffOptions` 增加 `base_repo_root / head_repo_root`；提供时 `diff_candidates_into` 从两侧仓库加载 candidate YAML 后输出 `candidates_added / candidates_removed / candidates_status_changed`，并体现在 `GraphDiffStats` 与 CLI 文本 / JSON 报告。CLI 新增 `--base-root / --head-root` flag。
- **Python LSP probe 真启动校验**：`python_indexer::ProbeOutcome::from_options` 在选定二进制后用 `--help + 3s timeout` 做一次"轻量 smoke launch"，shebang 损坏 / 解释器不存在等失败直接降为 AST fallback，不再让 `python_lsp_available()` 谎报"可用"。`tests/lsp_indexers.rs::python_indexer_emits_*` opt-in smoke 改为：probe 判定可用但 adapter 实际 fallback 时 soft-skip（写 eprintln 解释 fallback 原因）而不是 hard fail，避免本机 LSP 状态把 CI 染红。

**测试覆盖：**

- `language_traits::tests::{every_kind_has_a_language_and_family / matrix_total_count_matches_known_kinds / is_code_symbol_covers_swift_initializer_enum_protocol_go_interface_python_module / families_are_disjoint / similarity_only_targets_callables / dead_code_reason_is_non_empty / typescript_and_java_are_routed}` — 41 项 NodeKind 全枚举矩阵，新加 kind 漏掉 family/语言/dead-code reason/similarity 都立刻爆红。
- `typescript_ast::tests::*`（7 项）— ESM import / 重导出 / 类 + 方法 + 构造器 / 顶层函数 / 接口 + 枚举 / 装饰器堆叠 / 字符串里 `class` 不能被误识别。
- `typescript_indexer::tests::*`（3 项）— AST-only 路径跑 fixture 必须出 `TypescriptModule + Class + Function`、`Greeter` 类落地、相对 import 解析成功。
- `java_ast::tests::*`（7 项）— package + 静态 / 普通 import 双轨、类 + 构造器 + 方法、JUnit `@Test / @ParameterizedTest` 升格、嵌套类、注解串保留、`package` 单行文件、字符串里 `class` 不被误识别。
- `java_indexer::tests::*`（2 项）— AST-only 跑 fixture 必须出 `JavaPackage + JavaClass + TestCase`，符号 id 必须按 package 限定（`java::com.example.Greeter`）。
- `tests/lsp_indexers.rs::typescript_indexer_skips_when_tsserver_unavailable_but_still_runs_ast` + `java_indexer_skips_when_jdtls_unavailable_but_still_runs_ast` — 默认 cargo test 路径下，二进制不存在仍能跑 AST、出 `TestCase` / `JavaPackage` / `TypescriptModule`，并校验 `resolver_used` ∈ `{<lang>_ast, ""}`。
- `tests/lsp_indexers.rs::typescript_indexer_emits_*` + `java_indexer_emits_*` — `#[ignore]` 的真实 LSP smoke，需要 `typescript-language-server` / `jdtls` 才跑，与 Swift / Go / Python 一样走 `--include-ignored`，probe pass 但 adapter fallback 时 soft-skip。
- `questions::tests::pending_business_candidate_is_loaded_from_yaml` + `orphan_detection_uses_language_traits_for_every_code_kind` — pending candidate 必须从 YAML 而非 store 加载，且 orphan 判定走 `language_traits::is_code_symbol`（含 Swift / Go / Python / TS / Java 全量）。
- `graph_diff::tests::diff_candidates_reports_added_removed_and_status_change` — base + head 两份 candidate YAML 必须分别报 added / removed / status_changed。
- `python_indexer::tests::python_lsp_available_rejects_binary_with_broken_shebang` — `pylsp` 文件存在但 shebang 指向不存在的解释器时，probe 必须返回 `command: None` 而不是误报可用。

**新增 fixtures：**

- `tests/fixtures/typescript_hello/`：`package.json + tsconfig.json + src/{index,greeter,utils}.ts + tests/greeter.test.ts`，AST 与（可选）`typescript-language-server` 共享。
- `tests/fixtures/java_hello/`：`pom.xml + src/main/java/com/example/hello/{Greeter,HelloController}.java + src/test/java/com/example/hello/GreeterTest.java`，含 Spring-flavoured（`@RestController / @GetMapping`）但不依赖 Spring classpath 的最小注解 stub，AST 与（可选）`jdtls` 共享。

**非侵入式约束（沿用 P16 / P17）：**

- AST 扫描器全部纯 Rust、不引入 tree-sitter / TS Compiler API / JDT 二进制；只在 LSP 缺席时退化，不挪走 LSP 该有的精确度。
- TypeScript / Java `Calls` 边仍然只来源于 LSP `callHierarchy/outgoingCalls`，AST 路径**不会**伪造 `Calls`，文档把 AST-only 模式的 TS / Java `Calls` 归为"缺失但不污染"。
- 配置 `typescript.enabled / java.enabled` 默认 `false`，沙箱 / Dart-only / Python-only 工作流的行为零变化。
- `metadata_json` 字段对老配置完全向后兼容；TS / Java 节点初版不写 framework metadata，等后续迭代再叠加 Express / NestJS / Spring 装饰器分类器。

## P20 收口补丁（小批次）

在主 P20 落地后又跑了一轮小收口修复，让“正式收口前”的最后几条尾巴落地：

1. **统一 LSP probe / soft-skip 策略**：`swift_indexer_emits_*` 和 `go_indexer_emits_*` 在 probe 通过、但 adapter 实际回退（`result.resolver_used != "<lang>_lsp"`）时改为 soft-skip + `eprintln!`，与 Python / TypeScript / Java 的判断对齐。本机 `sourcekit-lsp` / `gopls` / 任何一种 LSP 的 stdio 启动失败都不会让 `cargo test … --include-ignored` 报红。
2. **`specslice index` 输出补 TypeScript / Java**：`crates/specslice-cli/src/commands/index.rs::format_result` 在 Python 之后新增 `TypeScript index:` / `Java index:` 两段（files / symbols / TestCases / Imports / Resolver / `LSP skipped` 原因），并补了 4 个 unit 测试（`render_includes_typescript_section_with_lsp_resolver_when_indexed`、`render_includes_typescript_section_with_ast_fallback_when_lsp_missing`、`render_includes_java_section_with_lsp_resolver_when_indexed`、`render_includes_java_section_with_ast_fallback_when_lsp_missing`），原 `render_omits_swift_section_when_adapter_is_disabled` 也扩展为同时验证 TS / Java 段在 adapter 未启用时不出现。
3. **`JavaEnum` 独立语义**：`crates/specslice-core/src/node.rs` 新增 `NodeKind::JavaEnum`（`java_enum`），`language_traits` 矩阵从 41 增至 42，`family_of(JavaEnum) = SymbolFamily::Type`、`language_of(JavaEnum) = Language::Java`。AST 扫描器 `java_ast.rs` 的 `enum Foo {…}` 不再退化为 `JavaClass`，并通过 `is_type_scope`（class / interface / enum 共用作用域规则）保证 `enum` 内方法仍能正确 parent。`record` 暂留 `JavaClass`（行为更接近不可变 POJO）。LSP 通路 `java_map_kind` 也补上 `LspSymbolKind::Enum → JavaEnum`。store / MCP / search / store round-trip 测试同步覆盖。新增测试 `java_ast::tests::enum_declares_distinct_kind_and_parents_methods`。
4. **fixture 卫生**：`tests/fixtures/java_hello/target/` 被清理；`.gitignore` 显式补 `tests/fixtures/java_hello/target/`、`tests/fixtures/typescript_hello/node_modules/`、`dist/`、`.turbo/`、`**/*.class`，避免操作者本地 `mvn` / `npm install` / `tsc` 之后把产物混入打包。

### 收口补丁的验收

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：581 passed / 0 failed / 5 opt-in LSP smokes ignored
- `cargo test -p specslice-engine --test lsp_indexers -- --include-ignored`：10 passed / 0 failed（本机 Swift + Go + TS 真启动，Python + Java soft-skip）
- `cargo test -p specslice-cli --bin specslice commands::index`：11 passed（含 4 个新 TS/Java 渲染测试）
- `dart test`（`tool/specslice_dart_analyzer/`）：6 passed

## P20 收口补丁（第二批：统一 LSP probe）

第一批收口走完后还有两个尾巴：

- 复核者跑完整命令 `cargo test -p specslice-engine --test lsp_indexers -- --include-ignored` 时 Swift LSP smoke 仍 hard fail，原因是 `sourcekit-lsp` 在他的机器上「PATH 上可见但 stdio 启动后立刻 `SOURCEKITD FATAL ERROR: Service is invalid`」，而第一批的 soft-skip 只覆盖 `resolver_used != "swift_lsp"`，没有覆盖 `index_swift` 直接返回 `Err` 的崩溃路径。
- 第一批中只有 Python 改成了「真启动一次 `--help` 才算可用」，Swift / TypeScript / Java / Go 仍然只检查二进制是否在 PATH 上，导致同样的失败模式还会在别的语言上复发。

第二批一次性把 LSP probe 拉到统一层。

1. **新增 `crates/specslice-engine/src/lsp_probe.rs`**：提供 `probe_lsp_command(command, args, timeout) -> ProbeReport`。`Runnable` 当且仅当进程在 `timeout`（默认 1500ms）内退出 0 且 stderr 不命中 `broken stub` 标记（`bad interpreter` / `no such file or directory` / `no module named` / `cannot execute` / `command not found` / `SOURCEKITD FATAL ERROR` / `could not load` / `no java runtime` / `JAVA_HOME is not set` / `node: command not found`）。模块内带 8 个 TDD 单测覆盖 ok / missing / broken-shebang / timeout / non-zero-exit / sourcekitd / 缺 JVM / 干净帮助文本。
2. **Python adapter 改为薄壳**：`crates/specslice-engine/src/python_indexer.rs::runs_ok` 从 60 行手写 spawn-and-timeout 缩成 8 行，转调 `crate::lsp_probe::probe_lsp_command(…)`。`python_lsp_available_rejects_binary_with_broken_shebang` 等 3 个回归测试仍绿。
3. **Swift / Go / TS / Java 全部走真 smoke launch**：`swift_lsp_available` / `go_lsp_available` / `typescript_binary_runnable`（TS `ProbeOutcome` 里所有 `binary_on_path` 调用点）/ `java_binary_runnable` 全部追加 `crate::lsp_probe::probe_lsp_command(…).is_runnable()` 这一步，保证 PATH 上的二进制能真启动才算「可用」。TS / Java 的 skip-reason 文案从「未找到对应可执行文件」改成「smoke launch 未通过」。
4. **opt-in 测试再加一层 Err → soft-skip**：`crates/specslice-engine/tests/lsp_indexers.rs` 中 Swift / Python / Go / TypeScript / Java 五个 opt-in smoke 把 `index_<lang>(&mut store, &opts).expect(...)` 改成 `match … { Err(e) => { eprintln!("soft-skip … `index_<lang>` returned Err ({e}); …"); return; } }`。即便 probe 误判可用、stdio 真启动后立刻崩溃，测试也会 soft-skip 而不是 panic。
5. **skip 文案统一**：5 个 opt-in smoke 的「跳过原因」全部改写成「did not pass the shared `lsp_probe` smoke launch（…）」，把可能的失败维度（PATH 缺失 / env 未设 / 非零退出 / broken-stub stderr / SOURCEKITD FATAL ERROR / 缺 JRE / broken node shebang）写在括号里，操作者一看就能定位。

### 第二批的验收（含模拟复核者失败模式）

- `cargo test -p specslice-engine --lib lsp_` → 36 passed（含 8 个 `lsp_probe` 单测）
- `cargo test -p specslice-engine --test lsp_indexers -- --include-ignored` → 10 passed / 0 failed（本机健康路径）
- `SPECSLICE_SWIFT_LSP_BIN=/tmp/specslice-broken-sourcekit.sh cargo test … swift_indexer_emits_class_struct_protocol_method_nodes_when_lsp_present -- --include-ignored` → 模拟「sourcekit-lsp 可见但崩溃」失败模式，soft-skip 触发，输出 `skipping … — `sourcekit-lsp` did not pass the shared `lsp_probe` smoke launch (… or LSP returned a `SOURCEKITD FATAL ERROR` / non-zero exit)`，**不再 hard fail**
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`（**589 passed / 0 failed / 5 ignored**）/ `dart test`（6 passed）

新增 / 修改：

- 新增：`crates/specslice-engine/src/lsp_probe.rs`
- 修改：`crates/specslice-engine/src/{lib.rs, python_indexer.rs, swift_indexer.rs, go_indexer.rs, typescript_indexer.rs, java_indexer.rs}`、`crates/specslice-engine/tests/lsp_indexers.rs`

## v0.2.0 正式收口（release artifact + 真实仓库扫描）

P20 + 小批次收口完成后正式打 0.2.0：

- `workspace.package.version = "0.2.0"`，`tool/specslice_dart_analyzer/pubspec.yaml::version = 0.2.0`，CLI `--version` 自检：`specslice 0.2.0`。
- `bash scripts/release_macos_universal.sh`：生成 `dist/specslice-0.2.0-macos-universal.tar.gz`（含 arm64 + x86_64 lipo 通用二进制 + Dart sidecar 源码 + AI Skill），`bash scripts/validate_macos_package.sh dist/specslice-0.2.0-macos-universal.tar.gz` 通过；
- `packaging/macos/README.md`：安装命令同步至 0.2.0；新增「Supported languages」表，列出 Dart / Swift / Go / Python / TypeScript / Java 各自的 LSP 与 AST fallback 现状；
- `packaging/skills/specslice/SKILL.md`：补 `java_enum` 独立语义说明。

### 真实仓库扫描（非侵入式 shadow-scan）

为了让目标仓不出现「除 yaml 之外的侵入」（连 `graph.db` / export 都不能落到用户代码库里），新增 `scripts/release_scan.sh`：把源仓 `rsync` 到 `release-scans/_scratch/<name>/`（自动剔除 `.git / node_modules / .venv / target / build / .dart_tool / dist` 等本地工具产物）→ 在 scratch 副本里跑 `init / index / check / graph / dead-code` → 摘要写到 `reports/release/<name>/report.md`。**目标仓自始至终零写入。**

四个真实仓库：

| 仓库 | 语言 | 文件 | 符号 | 测试 | Imports | 节点 | 边 | Resolver |
|------|------|------|------|------|---------|------|----|----------|
| pixcraft-app | Dart (Flutter) | 151 | 6964 | 366 | n/a | 7653 | 8869 | `dart_analyzer` |
| pixcraft-landing | TypeScript (React) | 22 | 102 | 12 | 71 | 136 | 180 | `typescript_lsp`（真启动） |
| atagent | Python (FastAPI) | 165 | 1224 | 272 | 665 | 1807 | 2054 | `python_ast`（soft-skip） |
| vub | Java (Maven 多模块) | 3111 | 16099 | 0 | 25194 | 18295 | 40239 | `java_ast`（soft-skip） |

亮点：
- vub 命中 12 个 `java_enum` 节点，证明本轮新加的 `JavaEnum` NodeKind 在真实大型 Java 仓里有效。
- pixcraft-landing 全程跑真 `typescript-language-server --stdio`，输出 22 个 module + 28 个 method + 12 个 interface，vitest `describe/it` 翻译成 `test_group/test_case` 完整。
- atagent 在没有 pyright/pylsp 的情况下，新加固的 probe smoke-launch 检测到 LSP 不可用并 soft-skip 到 `python_ast`，依旧识别 165 个 module、272 个 test case、45 个 framework entrypoint（FastAPI 路由 / pydantic 验证器）。
- pixcraft-app 走 Dart sidecar (`dart_analyzer`)，输出 6964 个 symbol、8869 条边；`dead-code --min-confidence high` 给出 6 个真实候选，每条都带中文 reason，明确说不是「自动删除」。

详见 `reports/release/README.md` 与各仓的 `reports/release/<name>/report.md`。

### 收口验收

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：581 passed / 0 failed / 5 opt-in LSP smokes ignored
- `dart test`（`tool/specslice_dart_analyzer/`）：6 passed
- `bash scripts/validate_macos_package.sh dist/specslice-0.2.0-macos-universal.tar.gz`：通过
- 四个仓 scratch-scan 全部生成 `report.md`；目标仓 `.specslice/` 时间戳全部早于本次扫描（pixcraft-landing 与 vub 根本没有 `.specslice/`），确认非侵入。

## v0.3.0-A 置信度贯通（已落地，未发版）

v0.2.0 收口后，进入 v0.3.0 主题。第一个子项目 v0.3.0-A 把
**已有的 `edge_confidence` 信号实际接入 dead-code reason 与
search ranking**，并打通 engine → CLI / MCP / JSON 的结构化
`warnings` 通道。这是纯 ranking / 解释性收口，不动 schema、
不动 BFS 可达性、不动 MCP 输出字段集（warnings 字段
`skip_serializing_if = "Vec::is_empty"` 保持完全向后兼容）。

- 设计与决策：`docs/superpowers/specs/2026-05-22-v030-a-confidence-plumbing-design.md`
- 6 段 TDD 实施计划：`docs/plans/2026-05-22-v030-a-implementation-plan.md`
- 真实仓库行为对照：`reports/release-v0.3.0a/README.md`

### 落地范围

| Phase | 主体改动 | 关键新代码 / 测试 |
|-------|----------|---------------------|
| 1 | 新模块 `confidence_view`：`EdgeQualityScope` (`Usage` / `SearchRanking`) + `EdgeQualitySummary` + 纯函数 `summarize_edges` + store 查询 `inbound_edge_quality` / `outbound_edge_quality` + 邻接收集 `neighbors_of(cap)` | `crates/specslice-engine/src/confidence_view.rs`（20 单测） |
| 2 | dead-code 在 `classify(...)` 末尾给"非 Low 桶但 inbound usage 边全是 low-tier"的候选追加 reason；`DeadCodeReport` 引入 `warnings: Vec<String>`（`skip_serializing_if = "Vec::is_empty"`） | `crates/specslice-engine/src/dead_code.rs`（+4 单测） |
| 3 | search 在 `matches.sort_by` 之前插入 Pass A（出边 evidence_quality=high → +30 score）+ Pass B（邻接 cluster → 最多 +20 score，reason 最多列 2 个名字 + "等"）；`SearchResult` 引入同型 `warnings` 字段，engine 端**任何 stderr 都改 push warnings** | `crates/specslice-engine/src/search.rs`（+7 单测、回归修 `tests/p5_search_golden.rs::p5_file_line_input_resolves_to_enclosing_symbol`） |
| 4 | CLI 渲染：`specslice search` / `specslice dead-code` 人类输出末尾增加 `== Warnings ==` 段（空列表完全静默）；MCP `search_graph` / `dead_code` tool 的 JSON-RPC 输出自然透传 warnings（通过 round-trip 测试锁死） | `crates/specslice-cli/src/commands/{search,dead_code}.rs`、`crates/specslice-mcp/src/tools/{search_graph,dead_code}.rs`（+4 CLI 单测、+4 MCP 单测） |
| 5 | 用 v0.2.0 阶段的 4 份 `release-scans/_scratch/*/graph.db` 与新二进制对照跑 dead-code 与 search，把 Phase 2 / Phase 3 的真实触发率与样例落库 | `reports/release-v0.3.0a/README.md`、`scripts/release_scan_v030a_metrics.py` |

### Phase 5 真实证据要点

- **Pass A（evidence boost）** 在 pixcraft-app `--kind dart_method` "build"
  查询上 75/100 命中触发，最高 score 从 100 涨到 130。
  样例 `_EditorScreenState.build` 含 9 条 high-tier 出边。
- **Pass B（neighbor boost）** 在 pixcraft-app/build 42/100 触发，
  在 vub/"service" 30/30 触发（同 package cluster）；vub/save 0
  触发，证明它只在真实邻接时给 tie-break。
- **dead-code only-low-tier-inbound reason** 在四个仓 0/0/0/0 命中，
  因为 `*_ast` indexer 出的边目前都是 Medium，Low 留给 AI-derive /
  override / ignored 三类罕见情况。Phase 2 reason 加得保守，
  不会误报，AI derive 接进来后会自然变得有意义。

### 已知遗留 bug（不属于 v0.3.0-A 引入）

- ~~`specslice-cli/src/commands/search.rs::parse_kind` 的 P20 补丁
  只覆盖 Dart / Swift / Go / Python 别名，**TypeScript / Java
  NodeKind 别名缺失**，所以 `--kind typescript_function` /
  `--kind java_method` 会被 CLI 本地解析器以 `unknown --kind` 拒绝，
  尽管 engine 的 `default_search_kinds()` 已经列为 valid。~~
  → **已修复（2026-05-31，CodeGraph 对标实测期间）**：`match_kind`
  补齐 TS（`typescript_*` + `ts_*` 短别名 + `tsfunc`）与 Java
  （`java_package/class/interface/enum/method/constructor`）全部别名；
  回归测试 `commands::search::tests::match_kind_accepts_typescript_and_java_aliases`。

### 已知遗留 bug（2026-05-31 CodeGraph 对标实测新发现并修复）

- ~~LSP 调用链大面积失败时（如 sourcekit-lsp 在大型 Swift 仓上
  callHierarchy/references 超时 + Broken pipe），`lsp_indexer.rs::
  push_partial_warning` 把**每个符号**的失败用「；」无上限拼接进
  `skip_reason`，最终 `specslice index` 把单行 **185 KB** 直接 `print!`
  到 stdout，污染管道 / MCP 载荷。~~
  → **已修复**：明细封顶 `MAX_PARTIAL_WARNING_DETAILS = 8` + 折叠计数
  （`…（另有 N 条 LSP 警告已折叠）`）；实测 Panelly 索引 stdout
  从 185 KB → 1 371 字节。回归测试
  `lsp_indexer::tests::push_partial_warning_caps_runaway_failures`。

### 收口验收（v0.3.0-A 仍以分支态，不发版）

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace -- --test-threads=4`：workspace 全绿（engine lib 272 测试、CLI 30 测试、MCP 6 测试，5 个 opt-in LSP smoke ignored）
- `dart test`（`tool/specslice_dart_analyzer/`）：6 passed
- 四个 scratch 副本 + raw JSON 已 gitignore，落库的只有人类可读的 `reports/release-v0.3.0a/README.md`

## P21 — Rust tree-sitter 广度后端 + 自举（已落地，未发版）

CodeGraph 对标实测暴露了一条**根本路径错误**：SpecSlice 的多语言全部押在
LSP-first 上，结果（1）慢 4–50 倍（Morse Dart 22.9s vs CG 5.6s；Panelly Swift
~80s vs CG 1.6s）、（2）强依赖外部二进制、（3）大仓 callHierarchy 超时、
（4）**连自己的 Rust 工作区都索引不了**。CodeGraph 用进程内 tree-sitter 在广度
与速度上完胜。P21 不再背历史包袱，引入 SpecSlice 的第一个 **tree-sitter 广度
后端**，先把"能索引自身"这条最难堪的缺口堵上。

**实现要点：**

- **依赖**：`tree-sitter 0.26` + `tree-sitter-rust 0.24` 直接链入 `specslice-engine`
  （C 语法在 build 期编译，运行期零外部依赖、完全确定性）。
- **`crates/specslice-engine/src/rust_treesitter.rs`**：纯函数 `scan(&str) -> RustScan`
  walk `source_file`，识别 `struct / union → RustStruct`、`enum → RustEnum`、
  `trait → RustTrait`、`mod → RustModule`、自由 `fn → RustFunction`、
  `impl`/`trait` 体内 `fn / function_signature → RustMethod`、`use → import`。
  `impl Trait for Foo` 的方法挂到**被实现类型 `Foo`** 而非 trait 上；模块/类型
  嵌套用 `outer::inner::name` 限定。语法树天然规避正则扫描器的字符串/注释误识别
  （`string_literal_is_not_a_symbol` 钉死）。递归带 `MAX_NESTING_DEPTH=256` 上限，
  对抗恶意深嵌套不爆栈。
- **`crates/specslice-engine/src/rust_indexer.rs`**：`index_rust` 走 walkdir 发现
  `.rs`（跳过 `target/.git/out/.idea`），逐文件 `scan` → `LanguageIndexBatch`
  → `ingest_language_batch_minimal`。符号 id 用文件相对路径限定
  （`rust::<file-rel>::<qualified>`），父链只在**同文件内确有该父符号**时才建，
  杜绝 dangling `contains` 边。无 LSP 层。
- **接线全链路**：`NodeKind` 追加 6 类（`rust_module/struct/enum/trait/function/method`）；
  `language_traits` 矩阵从 42 增至 48（`Language::Rust` + family/predicate/alias 全分支）；
  `store::node_from_row`、`search::default_search_kinds`、CLI `match_kind`（`rust_*` +
  `rs_*` 短别名）、MCP `parse_node_kind`、CLI `index` 渲染（`Rust index:` 段）、
  `config.rust`（`LanguageAdapterConfig`，默认 `enabled:false`，`init` 序列化默认配置
  时自动出现）、`index_repository`（`rust.enabled` gate）全部打通。

**SQLite 级测试标准（本轮起步）：**

- **属性测试**（`proptest`，`rust_treesitter::property_tests`）：对任意输入 `.*`
  ——（1）`scan` 永不 panic/abort/hang（全函数契约）；（2）`scan` 确定性
  （`scan(s)==scan(s)`，重索引幂等的根基）；（3）每个符号良构（name 非空、
  `1<=start_line<=end_line`、`qualified_name` 末段恒等于 `name`）；（4）import 恒被
  trim 且不留 `;`。
- **鲁棒性**：`pathologically_deep_nesting_does_not_overflow`（5000 层嵌套 mod 不爆栈）、
  `empty_and_garbage_inputs_are_safe`（空/残缺/Unicode/emoji）。
- **自举集成测试** `tests/p21_rust_self_host.rs`：索引**本仓库真实 `crates/` 树**，
  断言 `files>=20`、`symbols>=300`、`resolver_used==rust_treesitter`，且命中
  P21 自身定义的符号（`index_rust` / `scan` / `RustScan` / `RustIndexResult`）与各
  family 下限。**自举回归即刻爆红。**
- 单元/索引器测试：扫描器 7 项 + 索引器 3 项（端到端 + 重索引幂等 + `target/` 跳过）
  + CLI `match_kind_accepts_rust_aliases` + CLI `render_includes_rust_section_*`。

**实测自举（live dogfood，非侵入）：** 临时 `init`（仅生成 untracked 文件）→
`rust.enabled=true, paths:[crates]` → `specslice index` →
**125 个 .rs 文件 / 2023 符号 / 748 imports，`rust_treesitter`，整机 ~8s**，
随后删除生成物恢复整洁工作树。`search RustIndexResult --kind rust_struct` 与
`search index_rust --kind rs_fn` 均精确命中（带 file:line 与 `file→symbol`
contains 子图）——这正是 LSP-first 从来做不到的自举检索。

**非侵入式约束：** 不写入目标源码；tree-sitter 为 build 期依赖、运行期零外部
进程；`rust.enabled` 默认 false，非 Rust 工作流零变化。

### P21 验收

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets`：通过（含两处
  `usize as u32` 截断告警以 `line_no` 安全转换消除）
- `cargo test --workspace`：全绿（`specslice-engine` lib **288 passed**，含 15 项 Rust
  单元/属性测试；`p21_rust_self_host` 1 passed；core 28 passed；CLI / MCP / store
  全绿；5 个 opt-in LSP smoke ignored）
- live dogfood：本仓自索引 125 文件 / 2023 符号，检索端到端命中后恢复整洁工作树

## P22 — 通用 tree-sitter 广度后端（Rust/TS/Python/Go/C/C++ 六语言）（已落地，未发版）

P21 用单语言（Rust）验证了「进程内 tree-sitter = Tier 2 广度后端」这条路可行，但实现是 Rust 专用的手写扫描器 + 索引器。如果每加一门语言都复制一份「scanner + indexer + 接线」，就是六份近乎相同的文件、六套要各自维护的递归遍历。P22 把这条路**收敛成一个数据驱动的通用驱动**：遍历器与索引器只写一次、测一次、所有语言共享；每门语言坍缩成一份静态 `LangSpec` 配置。一次性把 Tier 2 从 1 门语言推广到 **6 门**（Rust / TypeScript / Python / Go / C / C++），直接回应 §5.1 / §6 Phase B 的「把广度层推广到 TS/Go/Python/C/C++」目标。

**架构：一个驱动，多种语言（`crates/specslice-engine/src/treesitter.rs`）**

- **`LangSpec` 数据驱动**：每门语言只提供 `language_id` + `grammar`（编译期链入的 tree-sitter 语法）+ `extensions` / `skip_dirs` / `separator` + `func_kind` / `method_kind`，再加一把小函数指针钩子，把该语法的 node kind 映射到 SpecSlice `NodeKind`。用 `fn` 指针而非 trait object，`LangSpec` 是零分配的 `static`。
- **通用 `extract(spec, source) -> Scan` + `walk(...)`**：嵌套限定名（`Outer::Inner::method`）从**真实 AST 祖先链**推导，跨语言统一，无需每语言维护「容器名单」。`walk` 的五步分派顺序固定：① import（一个节点可产出多个目标）→ ② impl-like 块（自身不是符号，体内 callable 挂到被实现类型）→ ③ type/module 容器 → ④ callable（函数/方法，按所在容器是否为 Type 决定 func/method）→ ⑤ transparent 包装（`export_statement` / `decorated_definition` 等，下钻但不发符号）。
- **全函数、panic-free、深度受限**：parser 任何失败都返回空 `Scan` 而非中断整轮索引；递归带 `MAX_NESTING_DEPTH = 256` 上限，恶意深嵌套只会停止下钻，绝不爆栈。
- **两处真正的语法不规则被隔离到钩子里**，主路径保持简单：Rust `impl` 块（方法挂到别处声明的类型）走 `impl_type_of`；Go 方法接收者（`func (r *T) m()`）走 `receiver_type_of`；其余语言这两个钩子返回 `None`。
- **导入钩子升级为 `ImportsFn`（返回 `Vec<String>`）**：覆盖 Python `import a, b` / TS 多名导入这类「一条语句多个目标」。
- **共享 helper**：`node_text` / `name_from_field` / `body_from_field` / `no_text` / `no_imports` / `no_container` / `never` / `simple_type_name`（`Vec<T>` → `Vec`、`crate::a::Foo<'x>` → `Foo`、`*T` → `T`）/ `normalise_ws`（折叠空白 + 去尾 `;`）/ `strip_quotes`（`"foo.h"` / `<stdio.h>` / `'pkg'` 各剥一层）/ `declarator_name`（沿 C/C++ `declarator` 指针/括号/引用链下钻到最内层标识符，`qualified_identifier` 取末段，64 步上限防畸形死循环）。
- **通用索引器 `index_repo_with_spec`**：`discover_files`（walkdir 按扩展名 + `skip_dirs` + exclude glob 收集，相对路径去重排序）→ 逐文件 `extract` → 组 `LanguageIndexBatch` → 复用 P12 起就稳定的 `ingest_language_batch_minimal` 入库。符号 id 用 `<language_id>::<file-rel>::<qualified>` 文件作用域限定（同名跨文件不撞），父链只在**本文件内确有该父符号**时才建，杜绝 dangling `contains` 边。indexer 名统一 `<language_id>_treesitter`。
- **语言注册表**：`SUPPORTED_LANGUAGES = [rust, typescript, python, go, c, cpp]`（即规范渲染/迭代顺序）；`spec_for_language(id)` 带常见别名（`rs / ts / py / golang / c++ / cxx`），未知 id 返回 `None` 让引擎跳过而非中断。

**六门语言的 `LangSpec`（每门一个文件，纯配置 + 该语言的不规则钩子）**

- **Rust（`rust_treesitter.rs`，`RUST_SPEC`）**：P21 的手写扫描器**重构为委托** `treesitter::extract(&RUST_SPEC, ...)`，`RustScan` / `RustImport` / `RustSymbol` 降为通用类型别名。`struct/union → RustStruct`、`enum → RustEnum`、`trait → RustTrait`、`mod → RustModule`、自由 `fn → RustFunction`、`impl`/trait 体内 `fn → RustMethod`；`impl Trait for Foo` 方法挂到 `Foo`。P21 的全部单元/属性/自举测试保持绿。
- **TypeScript（`typescript_treesitter.rs`，`TYPESCRIPT_SPEC`）**：`tree-sitter-typescript`，扩展 `ts/mts/cts`；容器 class/interface/enum/module，callable 函数+方法，`import_of` 取 `source` 字段；`export_statement` / `ambient_declaration` / `expression_statement` 标 transparent（`namespace Geo {}` 实为 `expression_statement` 包 `internal_module`，必须下钻）。
- **Python（`python_treesitter.rs`，`PYTHON_SPEC`）**：`tree-sitter-python`，扩展 `py/pyi`；容器 class、callable function、`import` / `from ... import ...`（多名展开）；`decorated_definition` 标 transparent。
- **Go（`go_treesitter.rs`，`GO_SPEC`）**：`tree-sitter-go`；容器从 `type_spec` 取 struct/interface，callable function/method，`receiver_type_of` 抽接收者类型让方法挂到对应类型；`type_declaration` / `import_declaration` / `import_spec_list` 标 transparent。
- **C（`c_treesitter.rs`，`C_SPEC`）**：`tree-sitter-c`，扩展 `c/h`；容器 struct/union/enum（带 body），callable function_definition，`name_of` 用 `declarator_name`，`import_of` 取 preproc include；`declaration` / `type_definition` / `linkage_specification` / `preproc_if*` 标 transparent。
- **C++（`cpp_treesitter.rs`，`CPP_SPEC`）**：`tree-sitter-cpp`，扩展 `cpp/cc/cxx/hpp/hh/hxx/ipp`；容器具名 namespace/class/struct/union/enum（带 body），callable function_definition，`name_of` 用 `declarator_name`；匿名 `namespace_definition` / `declaration_list` / `template_declaration` / `declaration` / `type_definition` / `linkage_specification` / `export_declaration` / `preproc_if*` 标 transparent。

**接线全链路（统一配置 + 引擎 pass + CLI/MCP）**

- **`specslice-core`**：`NodeKind` 追加 9 类 C/C++ 节点（`CFunction / CStruct / CEnum`、`CppNamespace / CppClass / CppStruct / CppEnum / CppFunction / CppMethod`），沿 `language_prefix_kind` 约定打通 `as_str` / serde；`language_traits` 矩阵加 `Language::C` / `Language::Cpp`，`ALL_KINDS` 全枚举矩阵从 48 增至 **57**，`matrix_total_count_matches_known_kinds` 钉死总数——漏改任一 family/predicate/alias 立即爆红。
- **`specslice-store`**：`node_from_row` 增补全部 C/C++ kind 字符串↔NodeKind 映射；round-trip 测试覆盖。
- **配置（`config.rs`）**：新增 `treesitter: TreeSitterConfig { enabled, languages, paths, exclude, roots() }`，默认关闭。识别 id：`rust / typescript / python / go / c / cpp`（+ 别名）。这是「一个开关索引多门语言」的单一入口。
- **引擎 pass（`index.rs`）**：`IndexResult` 增 `treesitter: Vec<TreeSitterLangResult>`（`skip_serializing_if = "Vec::is_empty"` 向后兼容）。`config.treesitter.enabled` 时遍历 `languages`，`spec_for_language` 解析（未知跳过、按 `language_id` 去重），**若 `rust.enabled` 旧开关本轮已索引 Rust 则跳过**避免双重索引；每语言先 `clear_indexer_outputs(<lang>_treesitter)` 再 `index_repo_with_spec`，幂等重索引。
- **CLI / MCP / search**：`specslice index` 输出新增 `Tree-sitter index:` 段，逐语言打印 files / symbols / imports / resolver；CLI `match_kind` 与 MCP `parse_node_kind` 补齐 `c_*` / `cpp_*` 别名（`cfn` / `cpp_ns` 等）；`search::default_search_kinds` 纳入 C/C++ 变体。
- **依赖（`Cargo.toml`）**：`tree-sitter-go` / `tree-sitter-typescript` / `tree-sitter-python` / `tree-sitter-c` / `tree-sitter-cpp` 直接链入 engine（全部编译期语法、运行期零外部进程、确定性）。

**SQLite 级测试标准（逐 spec + 端到端）：**

- **逐语言属性测试（`proptest`）**：对任意输入 `.*`，每门语言的 `scan` 都满足——① 永不 panic/abort/hang（全函数契约）；② 确定性（`scan(s)==scan(s)`，重索引幂等根基）；③ 每个符号良构（name 非空、`1<=start<=end`、`qualified_name` 末段恒等于 `name`）；④ import 恒 trim 且不留 `;`。这是「对标 SQLite——任意输入不崩、可重放」的落点。
- **逐语言单元/fixture 测试**：每门语言验证 class/struct/enum/namespace/method/function/import 各旗舰 kind 落地，且字符串/注释里的关键字不被误识别（语法树天然规避正则扫描器的误判）。
- **端到端多语言集成测试 `tests/p22_treesitter_multilang.rs`**：用**真实引擎 pass** `index_repository` 跑一个含六语言源文件的临时仓，仅靠单个 `treesitter:` 配置开关，断言六门语言都产出结构图（每门 `files>=1 && symbols>=2`、`resolver_used == <lang>_treesitter`），再开库逐一断言旗舰节点（`RustStruct/RustMethod`、`TypescriptClass/Function`、`PythonClass/Method`、`GoStruct/Method`、`CStruct/Function`、`CppNamespace/Class/Method`）确实入库；`unknown_languages_are_skipped_not_fatal` 锁住未知语言优雅跳过。
- **Rust 自举回归 `tests/p21_rust_self_host.rs`**：因 `RustScan` 降为类型别名，断言改为命中实际结构体（`LangSpec` / `RustIndexResult`），自举链路保持绿。

**实测 dogfood（live，非侵入）：** 用通用驱动直接对本仓 `crates/` 跑全部 `SUPPORTED_LANGUAGES`：Rust **133 文件 / 2129 符号 / 801 imports，约 2.8s**（其余语言 0，因本仓纯 Rust，符合预期），证明通用驱动在真实代码库上的吞吐与单语言手写版持平。

**非侵入式约束（沿用前几期）：**

- tree-sitter 全部是编译期依赖、运行期零外部进程、完全确定性；不写入目标源码。
- `treesitter.enabled` 默认 false；非 tree-sitter 工作流（Dart-only / LSP 语言）行为零变化。Rust 双开关（legacy `rust:` 与新 `treesitter:`）显式去重，永不双索引。
- 沿用现有 `EdgeKind`（`Contains` / `Imports`），不引入新事实通路；Tier 2 产出的是结构事实（结构 + import），高可信调用链仍留给 Tier 3 LSP。

### P22 验收

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets`：通过（修掉 C/C++ spec 的 `needless_borrow`、`treesitter.rs` 的 `doc_lazy_continuation`）。
- `cargo test --workspace`：全绿——`specslice-core` 30 passed；`specslice-engine` lib **313 passed**（含六语言单元 + 逐 spec 属性测试）；`p22_treesitter_multilang` 2 passed、`p21_rust_self_host` 1 passed；CLI / MCP / store 全绿；5 个 opt-in LSP smoke ignored。
- `dart test`（`tool/specslice_dart_analyzer/`）：6 passed。
- live dogfood：通用驱动对本仓 `crates/` 自索引 Rust 133 文件 / 2129 符号 / 801 imports（~2.8s）。

## P23 — 收敛与成熟化 Epic（tree-sitter 成为唯一结构来源；已落地，未发版）

P22 让通用 tree-sitter 驱动覆盖六语言，但仓库里仍**并存两条结构通路**：每门 LSP 语言既有手写 `*_ast.rs`（LSP 驱动）又有 `*_treesitter.rs`（通用驱动），且 **id 方案不同**。这违背「单一结构来源」，也是诉求 #2（去多套实现）的核心债。P23 回应五项诉求——#1 更严苛测试（对标 SQLite）、#2 去多套实现、#3 SCIP/stack-graphs 决策、#4 Java/Dart、#5 Markdown 需求映射（中文 dogfood）——确立**目标架构：tree-sitter 通用驱动是唯一结构来源；LSP / Dart analyzer 仅作可选 Tier-3 富化（按符号 id 叠加 `Calls`/`References`/语义边）**。

**P23.0 通用驱动能力补齐（不删任何东西）：** 在 `LangSpec` 增可选 hook 并在 `index_repo_with_spec` 落地，把手写 AST「多做的四件事」补进通用驱动——① `symbol_ranges`（impact 依赖）；② `metadata_of(symbol)`（写 `SymbolArtifact.metadata_json`，承载框架语义）；③ `resolve_import` 索引后置 pass（模块名→仓库相对路径，外部依赖回退/丢弃）；④ `tests_of` 同时支持声明式（Python `test_*`/`Test*`、Go `TestXxx`、Java `@Test`）与调用式（JS `describe/it/test`、Dart `test()/group()`，靠 `recurse_callables` 下钻 callable 体）。现有语言全回归 + 新增 proptest 守护总性。

**P23.1–P23.6 逐语言收敛（每语言独立 TDD、保持全绿）：**
- **Python（P23.1，模板）**：pytest/装饰器分类/src-root import 迁入 `PYTHON_SPEC`；LSP 降为 Tier-3；统一 id（一符号一节点）；新增差分测试（tree-sitter 结构 vs LSP 结构集合一致）+ 重索引幂等；删除 `python_ast.rs`。
- **TypeScript（P23.2）**：vitest/jest 测试 + ESM import 解析进 `TYPESCRIPT_SPEC`；删除 `typescript_ast.rs`。
- **Java（P23.3，#4）**：新增 `tree-sitter-java`，新建 `java_treesitter.rs`（JUnit `@Test`、package→路径 import）；删除 `java_ast.rs`。
- **Go（P23.4）**：补 `TestXxx`/import；`go_indexer.rs` 结构部分降级 Tier-3。
- **Swift（P23.5）**：验证 `tree-sitter-swift` 兼容→新建 `swift_treesitter.rs`，`swift_indexer.rs` 降 Tier-3。
- **Dart（P23.6，#4，最高风险）**：新增 `tree-sitter-dart`，`dart_treesitter.rs` 作结构来源；analyzer/sidecar 降为 Tier-3 语义/calls（`reads_provider`/`navigates_to`/… + Calls + `DartProvider` 框架节点），沿用 `dart_*::` 旧 id 方案使 overlay 零翻译绑定；退役 `specslice-lang-dart` 结构职责；**pixcraft 等 golden 保持全绿**。

**P23.7 统一配置 + 单一 index pass + CLI/MCP 去重：** `config.rs` 引入统一 `languages:` + 可选 `enrichment:`(lsp/analyzer)，旧 per-language 键作**弃用别名**；`EngineConfig::normalized()` 把 `languages` 折叠为**唯一权威来源**（未列语言一律排除，Dart 含在内）。`index.rs` 合为单一结构 pass。CLI/MCP 的 NodeKind 别名解析与渲染统一收敛到 `NodeKind::from_str`（消除漂移）。

**P23.8 SQLite 级测试（#1）：** ① store `proptest` round-trip——对**自由文本列**（id/path/hash/indexer/generation/JSON/confidence）灌任意安全文本，证明 write→read 无损、`upsert` 幂等（顺带修了一处潜伏 bug：`repositories.rs` 的 `node_from_row`/`symbol_range_from_row` 手写 match 不全，非 Dart/Swift/Go/Python 的 NodeKind 能写不能读，改用 `NodeKind::from_str` 根治）；② 全语言总性/确定性 proptest（截断 UTF-8、超大、深嵌套、任意字节均不 panic 且确定性）；③ 全流水线重索引幂等（统一配置跑两次 `index_repository`，图快照逐字节一致）；④ 语料总性——对自身仓库（Rust）与外部 Dart 仓（Morse/Penlly，`#[ignore]` 重测）两次索引图一致，全部写入临时 store（非侵入）。

**P23.9 Markdown 需求格式（#5a）：** 新增 `requirements_md_indexer.rs` 解析 `.specslice/requirements/*.md`——每需求 H1 `<编号> <中文标题>` + 描述 + `## 文档/## 实现/## 测试`（中英标签皆可）的 `路径#片段` 引用列表 → `Requirement` 节点 + `Documents`/`DeclaresImplementation`/`DeclaresVerification` 边（复用与 links.yaml 相同的边类型，故 `slice`/`impact`/`checks`/`context` 与 MCP `context_pack` 自动 surfacing，无需改这些路径）。解析器**跳过围栏代码块**（```/~~~ 内的 `#`/`-` 不解析），索引器**跳过 `README.md`**（保留为格式说明文档）。引用解析语言无关：按「path + 片段」对既有节点做 exact-name / stable-key / slug / `类型.方法` 末段匹配；命不中时回退到 file 边并计入 `unresolved`，供 `checks` 标记。`init` 脚手架在 `.specslice/requirements/README.md` 写入中文格式说明（围栏示例、不污染图）；`links.yaml` 仍兼容。

**P23.10 中文 dogfood 需求映射（#5b，含 dogfood 发现的真实 bug 修复）：** 为本仓写 `.specslice.yaml`（统一 `languages: [rust]`）+ 三份中文需求文件（18 条需求，覆盖 P0 非侵入式 / MVP-1..5 / P6 图浏览器 / P8 语义边 / P9 候选 / P10 死代码 / P11 MCP / P16 Python / P18 相似 / P19 选测 / P21 自举 / P22 通用驱动 / P23 收敛 + Markdown 需求）。实测 `specslice index`：18 需求、18 docs、27 implementations、21 tests、**0 unresolved**；`slice` 三类齐全（含 CJK 标题 section 级命中）；`impact`（跨 `index.rs` 改动的历史区间）正确命中 `REQ-P23-CONFIG` 并附「有链接测试但本次无测试改动」warning；`graph --view business` mermaid 渲染需求子图；`check` 仅报 2 条**故意留空**的 missing_linked_test。**dogfood 暴露并修复了一处 P23.7 缺陷**：`discover_files` 把空 `code_roots` 当作 `["."]` 扫全仓，导致「未列 Dart」时仍索引了 174 个 fixture Dart 文件；改为「`code.paths` 为空 = 显式排除，Dart pass 整体跳过且清理旧 Dart 行」（保留 legacy 默认 `[lib,test]` 行为），并补 `p23_unified_languages_excludes_unlisted_dart` 回归。

**P23.11 SCIP / stack-graphs 决策（#3）：** 新增 `docs/adr/0001-scip-and-stack-graphs.md`。结论：**SCIP 是数据格式**，采纳「摄入优先 + 导出其次」——对有成熟 indexer 的语言（TS/Java/Python/Rust）以**读取离线 `.scip`** 替代实时 LSP 作首选精度层（正面回应诉求 #2），绑定方式复用 P23.0 的 `symbol_ranges` **按范围零翻译叠加**（与 Dart overlay 同范式，不引入第二套 id）；**stack-graphs** 是「无外部 LSP 的进程内跨文件解析」终极方向，以单语言（Python/TS）spike、特性开关隔离、默认关闭。**实际实现默认延后**，给出 R1–R5 路线与验收门槛；产物一律写 `.specslice/`（非侵入）。

**P23.12 文档：** 本节即 P23 入档；并同步 `docs/codegraph-benchmark-and-roadmap.md`（§5.1 收敛事实 + §6 路线打勾 + ADR 链接）。

**P23.13 第二轮 dogfood（逐命令自检 + TDD 修复 + stack-graphs 真实验证）：** 对本仓逐一跑 `search/logic/features/dead-code/similar/questions/context/candidate/export` 等命令，暴露并以 TDD 修复四处真实缺陷（均先写失败/更新测试，再以真实命令输出验收）：

1. **Rust import 全部悬空（最严重，影响图正确性）：** `RUST_SPEC.resolve_import = keep_raw_import` 把每条 `use`（含 `std::`/外部 crate/未拆分的 `{..}` 组/未解析的 `crate::`/`super::`）原样落成 `file::<raw>` 悬空边——实测 **845 条 import 边几乎全是悬空**，污染 `questions`（20 条 dangling_import）与整图。新增 `rust_resolve_import` + `rust_src_roots`：按 crate 源根做**模块路径→文件**解析（`crate::`/`self::`/`super::super::`、按 crate 名跨 crate、最长模块前缀命中 `foo.rs`/`foo/mod.rs`、crate 根 re-export 回退 `lib.rs`/`main.rs`），外部/std 一律**丢弃**且不产生自环边。结果：**845→422 条 import 边，全部命中真实文件（0 悬空）**，`questions` 的 dangling_import **20→0**；新增 7 项单测 + 重写 `rust_indexer` 端到端断言（`use std::…` 被丢、`use crate::util::Thing` 解析到 `src/util.rs`）。
2. **`features` 性能 75.7s→~0.1s：** 聚类的种子打分与 BFS 标签传播对**每个种子的每个节点**都发 SQLite `list_edges_from/to` 查询（O(种子×节点) 往返）。改为一次 `list_all_nodes`+`list_all_edges` 建内存邻接表与节点索引，邻居查询降为 O(1)；算法与（顺序无关的）输出不变，新增「跑两次 diff 一致」确定性核验。
3. **`similar` 在 Rust 仓扫描 0 函数：** `Language` 枚举只有 Python/Dart，`node_language` 对 Rust 返回 None → 静默跳过。新增 `Language::Rust`（C 风格 `//`、`/* */` 注释 + Rust 关键字集），`RustFunction/RustMethod` 纳入扫描。结果：**扫描 0→1712 函数**，找到真实克隆簇（如 `index_go`≈`index_swift`、`setup_indexed_repo` 跨 3 个测试文件复制）；新增归一化/识别单测。
4. **`dead-code` 在 Rust 仓的「全员死代码」假阳 + 静默：** 默认入口点是 Dart 化的 `lib/main.dart`，本仓 0 命中 → 2223 符号全标「可能死」却无任何提示。新增 Rust `fn main` 入口识别；并在「0 入口点」或「无 calls/references 精确层边」时输出**诚实告警**（建议配置入口 / 当前结果仅为候选，需 Tier-3 富化）。`DeclaresVerification` 不计入「精确层存在」判断（避免少量需求边掩盖代码无解析调用的事实）；新增 2 项告警单测。

**stack-graphs 进程内 spike — 真实 cargo 可行性验证（回应诉求 #2 的「终极方向」）：** 在隔离临时 crate（非侵入，不动工作区依赖）实测：`tree-sitter-stack-graphs 0.10` 要求 `tree-sitter ^0.24`（`<0.25`）+ `tree-sitter-graph ^0.12`，与本仓 `tree-sitter 0.26.9` 因 `links = "tree-sitter"` 唯一性规则**硬冲突**（`cargo update` 直接失败）；且官方无 Rust 规则包。结论：R4 维持默认延后，记为**书面例外**（触发条件：上游支持 `tree-sitter >=0.26`），精度层以 SCIP 摄入（R1）为唯一推进路径。详见 `docs/adr/0001-scip-and-stack-graphs.md` §8（含完整命令输出与拒绝的绕过方案）。

**P23.14 第三轮 dogfood（补跑命令矩阵 + 同类缺陷批量收口）：** 补齐上一轮未充分驱动的命令（`graph` 的 json/mermaid/overview/business/focus、`slice`、`impact`、`select-tests`、`graph-diff`、`connect propose`、`candidate list`），并把前两轮只在 Rust 上修过的「同类 bug」推广到全语言。四处真实缺陷均先写失败/更新测试再以真实命令输出验收：

1. **doc 索引吞掉 `node_modules`（图体积虚高 2.4×）：** `index_docs` 的 `walkdir` **无目录剪枝**，会下钻 `node_modules`/`.git`/`target` 等——本仓 `docs/` 下 vendored 的 CodeGraph `node_modules` 让 DocSection 虚增到 **1518**（绝大多数是第三方 README）。新增 `is_pruned_dir`（对齐 tree-sitter 驱动的 `skip_dirs`：`node_modules`/`.git`/`target`/`dist`/`.venv`/`.specslice`/… 一律不下钻），DocSection **1518→620、node_modules 段 0**；新增 `docs_walk_prunes_vendor_directories` 回归。
2. **`graph --format mermaid` 无视 `--view`（business 视图 8395 行、0 需求可见）：** `apply_view` 只切换每节点 `default_visible`，不删节点；HTML 端遵循该标志，但 `render_mermaid` 把**全部节点**直接铺出 → mermaid 把整图（含上面那些 vendored 文档）一次性 dump。改为只渲染 `default_visible` 的可见面及其之间的边：business **8395→142 行**（以需求节点为中心）、overview 收敛为顶层模块。新增 `render_mermaid_only_emits_default_visible_nodes_and_their_edges` 与对应 CLI 集成断言。
3. **`similar` 仅覆盖 3 语言（同类静默跳过）：** 上一轮只给 Rust 补了 `Language` 臂，Go/Swift/TypeScript/Java/C/C++ 的函数/方法节点仍返回 None。一次补齐九语言（共享归一化器 + 各自结构关键字集 + C 系 `//`、`/* */` 注释），`node_language` 覆盖全部已产出函数/方法的语言；新增 Go/TS 归一化与全语言 `node_language` 识别单测。
4. **C/C++ 仍用 `keep_raw_import`（同类悬空边）：** 仅剩 C/C++ 沿用原样落边，`#include <sys.h>` 必然悬空。新增共享 `resolve_c_include`（按存在性解析：相对 include 文件目录 → 仓库根相对 → 唯一后缀匹配 `-I` 根；系统头/歧义一律**丢弃**不留悬空），与 Rust/TS 同一正确性标准；新增相对/根/唯一后缀解析与歧义丢弃单测。

**P23.15 第四轮 dogfood（Dart 大仓真实扫描 + 死代码假阳收口；analyzer 自洽化）：** 用发版二进制 + AOT 编译的 analyzer sidecar 对多个真实 Flutter 仓做非侵入扫描（图写入各仓 `.specslice/`，临时仓用毕即清理），以 `dead-code` 的 **high 置信候选数**为假阳指标，逐一 TDD 修复：

1. **Dart extension 成员在 tree-sitter 误解析时沦为 phantom `dart_fn`（turing：16 条 high 假阳）：** turing 的 `game_screen_editor.dart` 含 `tree-sitter-dart` 无法消化的 Dart 3 语法，整文件解析失败（**103 个 ERROR/MISSING 节点**，根 `ERROR@1` 吞掉全文），`extension _GameScreenEditor on _GameScreenState { … }` 的私有成员（`_showSnackBar` 等）退化为顶层 `dart_fn::<file>#_showSnackBar`；而 analyzer overlay 的 ~20 条 `calls` 边正确指向 `dart_method::<file>#_GameScreenState._showSnackBar`，于是**全部悬空**、成员被判 high 死代码。根因：`walker.dart` 的 `visitMethodDeclaration` 对 `_inExtension` 成员**只登记 Element→id 映射、不产出 symbol 行**（假设 tree-sitter 必然产出该节点）。修复采用 **analyzer 自洽化**——extension 成员也产出 `dart_method` symbol 行：tree-sitter 正常解析时该行与结构节点 id 相同，`reconcile_misparsed_callables`/`backfill_referenced_symbols` 都按「id 已存在」跳过（零重复、不 clobber）；tree-sitter 误解析时 reconcile 据此**丢弃 phantom `dart_fn`、改挂 analyzer 的 `dart_method`**（合成父 `dart_extension::…` 不存在时 re-home 到 file，零悬空 `contains`）。新增回归 `dart_treesitter.rs::private_extension_on_private_type_attaches_members`、`dart_indexer.rs::reconcile_tests::drops_phantom_fn_for_extension_member_with_synthetic_parent`、`walker_test.dart` 的 extension 成员 symbol 断言。实测 turing：`_showSnackBar` 由 `dart_fn`→`dart_method`（20 条入边连通）、phantom 清零、**high 16→0**。
2. **AOT 编译的 sidecar 找不到 Dart SDK（生产快路径不可用）：** 文档允许把 `SPECSLICE_DART_ANALYZER_BIN` 指向**编译后的二进制**（大仓比 `dart run` 显著快），但 analyzer 7.7.1 的 `getSdkPath()=dirname(dirname(Platform.resolvedExecutable))` 对 AOT exe 解析成 exe 自身目录（`/tmp/lib/_internal/…`），抛 `PathNotFoundException`，sidecar 整体 skip 回退 lightweight。修复：`walker.dart` 新增 `resolveSdkPath()`——`dart run` 下默认有效则返回 `null`（行为不变）；AOT 下依次用 `SPECSLICE_DART_SDK`/`DART_SDK`、再在 `PATH` 上定位 `dart` 推导 SDK（兼容纯 Dart SDK 的 `<sdk>/bin/dart` 与 Flutter 的 `<flutter>/bin/cache/dart-sdk`），以 `sdk_library_metadata/libraries.dart` 存在性校验。`resolvedExecutable`/`environment` 注入便于测试；新增 5 项 `resolveSdkPath` 单测（含「默认 bogus 时经 PATH 复原」「`SPECSLICE_DART_SDK` 覆盖优先」）。实测 AOT exe 对 turing：`ok:true`、20 456 symbols、stderr 空。

并对齐前序轮次的 analyzer overlay 修复：sidecar 用**临时空 `analysis_options.yaml`** 覆盖目标仓的 `analyzer: exclude:`，使生成代码（如 `lib/l10n/generated/**`、`*.g.dart`）也产出语义边——图覆盖跟随 code roots 而非项目 lint 作用域（dogfood：hama `_AppLocalizationsDelegate`）；构造器边统一 `dart_ctor::path#Class.<default>` id；`DartConstructor` 与构造器形 `dart_method` phantom 默认从 high 降级，可达构造器反向点亮其 owning class。

**真实扫描验收（非侵入；high =「可安全建议删除」假阳指标）：**

| 仓 | dart 文件 | code 符号 | reachable | high 假阳 |
|---|---|---|---|---|
| turing | 543 | 19 969 | 19 730 | **16→0** |
| tailorx（extension dogfood 源头，954+ lib） | 1 443 | 24 759 | 24 702 | **0** |
| morse | 89 | 1 267 | 1 222 | **0** |
| NeuronLab | 43 | 735 | 715 | **0** |
| hama / Shift（前序轮次） | — | — | — | **0** |

剩余 medium 候选以**具名/私有构造器**与**框架 override**（如 `_TimeoutHttpClient.close/send`）为主，符合保守预期；构造器形 phantom（如 `AppButton.AppButton`）正确停留 medium。

### P23 验收

- `cargo fmt --all -- --check`：通过。
- `cargo clippy --workspace --all-targets`：通过（含 `requirements_md_indexer` 行号 `u32::try_from` 收口）。
- `cargo test --workspace`：全绿——**738 passed / 0 failed**；engine lib **352 passed**（在 P23.13 的 346 上 +6：docs 剪枝 1、similarity Go/TS/全语言识别 3、C 头解析 2）。
- `cargo test -p specslice-cli`：全绿（init/index/slice/impact/graph/check + graph mermaid overview/business 集成断言）。
- live dogfood（非侵入，写临时 `.specslice/`）：`index` 0 unresolved；`slice`/`impact`/`graph`/`check` 全部连通验证（见 P23.10）。
- 第二轮 dogfood（P23.13，真实命令输出）：import 边 845→422（**0 悬空**，全部命中真实文件）；`questions` dangling_import 20→0；`features` 75.7s→~0.1s（两次 diff 一致）；`similar` 扫描 0→1712 函数；`dead-code` 输出诚实告警；stack-graphs 进程内方向经真实 cargo 验证记为书面例外（ADR §8）。
- 第三轮 dogfood（P23.14，真实命令输出）：补跑 `graph(json/mermaid/focus)`/`slice`/`impact`/`select-tests`/`graph-diff`/`connect propose`/`candidate list` 全部连通；DocSection **1518→620**（node_modules 段 0）；business mermaid **8395→142 行**；`similar` 覆盖 **9 语言**；C/C++ 改用 `resolve_c_include`（系统头不再悬空）。集成后再次自索引：DocSection 620 / Imports 475 / **0 悬空**。
- 第四轮 dogfood（P23.15，真实命令输出）：`cargo fmt`/`clippy` 通过；`cargo test --workspace` 全绿（**0 failed**，engine lib **389 passed**，含 extension/SDK 两类新增回归）；Dart sidecar `dart analyze` 无问题 + `dart test` **19 passed**（含 extension 成员 symbol 断言与 5 项 `resolveSdkPath`）。真实 Flutter 仓扫描 high 假阳：turing **16→0**、tailorx/morse/NeuronLab/hama/Shift 均 **0**；AOT sidecar 经 SDK 解析后对 turing 输出 `ok:true`、20 456 symbols（快路径可用）。

## 后续验收方式

你开发后，我会按以下顺序验收：

1. 查看 Git diff，确认改动是否落在当前 MVP 范围内。
2. 查测试是否先覆盖目标行为，尤其是 parser、range、impact 传播。
3. 运行格式化、clippy、workspace tests。
4. 运行 fixture CLI e2e。
5. 对照本文件的阶段验收指标逐项打勾。
6. 如无阻塞问题，再按中文提交信息提交或确认可合并。
