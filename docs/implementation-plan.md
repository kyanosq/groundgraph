# SpecSlice 落地方案、测试体系与验收指标

## 目标

SpecSlice 的核心目标是证明一个非侵入式闭环：

```text
文档事实 / Dart 事实 / 测试事实 -> AI 业务逻辑候选与关联候选 -> 人工确认 -> confirmed graph -> PR Impact / Agent Context Pack / Graph 浏览
```

MVP-0 ~ MVP-5 已完成；P6 ~ P9 已把只读图浏览、代码事实边、Dart analyzer sidecar、Flutter/Riverpod 语义边和 AI 业务候选层落到主线。P10 落地 `specslice dead-code`，P11 把 MCP 工具层与可展开/可过滤的搜索阅读器并入主线，P12 通过 LSP sidecar 加入 Swift / Go 的结构事实图。当前阶段仍不做 GraphRAG、不把 LLM 输出直接写进 confirmed graph，也不在 Swift / Go 代码里加任何注解。价值判断看三件事：

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
- 不引入新的事实通路：Swift / Go 沿用 `EdgeKind::Contains`（File → Symbol → Symbol），后续 `callHierarchy` / `references` 会作为新 PR 单独跟进，不会回头改既有 Dart 路径。

**P12 复核修复（已落地）：**

- **LSP 运行期失败一律降级**：`run_profile` 现在把 `spawn / initialize / didOpen / documentSymbol` 的所有错误捕获并写入 `LspIndexOutcome::Skipped` 或 `Indexed { stats.skip_reason }`，不再让 `index_repository` 因 sourcekit-lsp 沙箱权限、`gopls` cache 缺失等环境问题整体失败。`run_profile_downgrades_runtime_lsp_failure_to_skipped` 用 `/usr/bin/true` 冒充 LSP 复现这条契约。
- **read 超时真正生效**：`LspClient` 把 stdout 读取放到后台线程并通过 `mpsc::Receiver::recv_timeout` 等待应答，`set_response_timeout` 到期会立刻 `force_kill` 子进程；新增 `request_times_out_when_lsp_server_never_writes` 用 `sleep 30` 复现「LSP 吃掉请求但不回包」的死锁场景，断言 150ms 超时内 bail。
- **CLI 输出 Swift / Go 段**：`specslice index` 的 `print_result` 拆出 `format_result`，在配置启用 `swift.enabled` / `go.enabled` 时分别打印 files / symbols / resolver_used / skip_reason；五条新 `format_result` 单测同时覆盖「未启用」「Indexed」「Skipped 含 PATH 提示」三种渲染分支。
- 现阶段 Swift / Go 仍只覆盖结构事实（files + symbols + contains），调用/引用边会作为后续 PR 通过 `callHierarchy` / `references` 单独引入。

## 后续验收方式

你开发后，我会按以下顺序验收：

1. 查看 Git diff，确认改动是否落在当前 MVP 范围内。
2. 查测试是否先覆盖目标行为，尤其是 parser、range、impact 传播。
3. 运行格式化、clippy、workspace tests。
4. 运行 fixture CLI e2e。
5. 对照本文件的阶段验收指标逐项打勾。
6. 如无阻塞问题，再按中文提交信息提交或确认可合并。
