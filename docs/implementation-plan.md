# SpecSlice MVP 落地方案、测试体系与验收指标

## 目标

SpecSlice MVP 先证明一个非侵入式闭环：

```text
REQ 文档 / Dart 事实 / 测试事实 -> AI 候选关联 -> 人工确认 -> .specslice/links.yaml -> PR Impact -> Agent Context Pack
```

第一阶段不做 Dart analyzer sidecar、不做 GraphRAG、不做 MCP。MVP 的价值判断看两件事：AI 能否基于事实生成高质量候选关联；确认后的 links manifest 能否在不改业务代码/业务文档的前提下稳定查询和反查。

## 非侵入式约束

- 业务代码、业务测试、业务文档默认只读扫描。
- 不支持在业务代码/业务测试中加入工具专用注解。
- 不支持在业务文档中加入工具专用关系段落。
- 已有 Markdown frontmatter 可以兼容读取，但不能作为接入前置条件。
- SpecSlice 只能写 `.specslice.yaml`、`.specslice/links.yaml`、`.specslice/graph.db`、`.specslice/export/`，以及后续 `.specslice/requirements.yaml`、`.specslice/candidates/`。
- LLM 只能生成候选、问题和解释，不能写回业务代码、业务测试或业务文档。
- 业务逻辑文档与代码/测试的关联不能由人工标注产生，也不能由规则匹配产生。
- 规则只负责解析事实、校验 AI 候选引用、维护已确认外置关系。
- 人工负责确认、编辑、拒绝 AI 候选；确认结果写入 `.specslice/links.yaml`。

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

### MVP-1：Markdown Requirement 索引

**目标：** 从 `docs / specs / adr` 中索引 Requirement、ADR、DocSection。

**实现范围：**

- 提取 Markdown File / DocSection。
- 兼容读取已有 frontmatter 中的 `id / type / title / status`。
- 不要求业务文档新增 frontmatter。
- Requirement 可由已有 frontmatter、`.specslice/links.yaml` 或后续 `.specslice/requirements.yaml` 创建。
- 建立 `DocSection --documents--> Requirement`。
- 不解析业务文档中的工具专用关系标记；需求到实现/测试的关系由 `.specslice/links.yaml` 声明。

**TDD 起点：**

- Fixture：`docs/watermark.md`。
- 先断言 `specslice index docs` 输出 `Requirements: 1`、`DocSections: 1`。
- 再断言数据库里有 requirement node、doc section node 和 documents edge。
- 再写 broken doc reference 测试。

### MVP-2：Dart Lightweight Adapter 与外置关系声明

**目标：** 不依赖 Dart analyzer，只用 Rust lightweight scanner 提取 Dart 文件、符号和测试；关系声明只来自 `.specslice/links.yaml`。

**实现范围：**

- 扫描 `lib / test`。
- 提取 file、class、method、function、constructor、import、`test(...)`、`group(...)`。
- 输出 `LanguageIndexBatch`，由 Core 统一入库。
- 建立 symbol range 和 parent-child hierarchy。
- 索引 `.specslice/links.yaml`，建立 `Documents`、`DeclaresImplementation`、`DeclaresVerification` 边。

**TDD 起点：**

- Adapter unit tests 覆盖 class、method、constructor、top-level function。
- Test extractor 覆盖 `test('name', ...)` 和 `group('name', ...)`。
- Links manifest 测试覆盖无业务注解时仍能连接 requirement、implementation、test。
- Range mapper 覆盖 method 改动可映射到 method，并通过 manifest 声明的 parent class 回溯到 Requirement。

### MVP-3：Feature Slice

**目标：** 从一个 Requirement 找到文档、实现和测试。

**实现范围：**

- `specslice slice REQ-ID`。
- 只走 confirmed/declared 高可信边。
- 默认不走 imports、calls、references、candidate edges。
- 输出 Docs、Linked Implementation、Linked Tests、Risks。

**TDD 起点：**

- 使用 fixture 中的 `REQ-WATERMARK-001`。
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

**目标：** 在不侵入业务仓库的前提下，由 AI 生成业务文档与代码/测试的候选关联，人工确认后进入 confirmed graph；同时识别“关系存在但业务逻辑未验证”、“业务文档缺失”、“文档与代码/测试信号可能不一致”的风险。

**实现范围：**

- 新增 `specslice connect`：
  - 输入 docs/code/tests 的事实节点和 evidence pack。
  - AI 生成 candidate links。
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
- 新增可选 `.specslice/requirements.yaml`，用于没有业务文档时保存外置需求描述。
- 新增 `specslice ask`，根据 evidence 生成需要用户回答的问题。
- AI 只生成 candidate / questions，不直接写 confirmed graph。
- 禁止用规则匹配生成业务关联。

**TDD 起点：**

- 有业务文档、实现和测试但无 links 时，AI candidate 经确认后写入 `.specslice/links.yaml`。
- 无业务文档但有实现/测试时，输出 `missing_doc` 和澄清问题。
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
   - 覆盖 frontmatter、REQ/AC/ADR 识别、doc section line range。

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

- fixture 中 `docs/watermark.md` 能生成 1 个 Requirement 和 1 个 DocSection。
- `DocSection --documents--> Requirement` edge 存在。
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
- 确认 Markdown frontmatter 只是兼容输入，不是接入要求。
- fixture 增加“无 frontmatter + links manifest”的端到端用例。
- 验收：全文搜索无工具注解/业务 Related 语义残留；`cargo test --workspace` 通过。

### P1：AI 候选关联与人工确认

- 增加 `specslice connect`。
- 生成 evidence pack：docs sections、symbols、tests、paths、line ranges、hash。
- 调用 AI 生成 candidate links 和澄清问题。
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

## 后续验收方式

你开发后，我会按以下顺序验收：

1. 查看 Git diff，确认改动是否落在当前 MVP 范围内。
2. 查测试是否先覆盖目标行为，尤其是 parser、range、impact 传播。
3. 运行格式化、clippy、workspace tests。
4. 运行 fixture CLI e2e。
5. 对照本文件的阶段验收指标逐项打勾。
6. 如无阻塞问题，再按中文提交信息提交或确认可合并。
