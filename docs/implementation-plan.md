# SpecSlice MVP 落地方案、测试体系与验收指标

## 目标

SpecSlice MVP 先证明一个确定性闭环：

```text
REQ 文档 -> Dart 显式 trace -> Dart 测试 -> PR Impact -> Agent Context Pack
```

第一阶段不做自动理解无 trace 仓库、不做 LLM Candidate、不做 Dart analyzer sidecar、不做 GraphRAG、不做 MCP。MVP 的价值判断只看显式 trace 能否稳定建立、查询和反查。

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

- 解析 Markdown frontmatter。
- 提取 `id / type / title / status`。
- 识别 `REQ-*`、`AC-*`、`ADR-*`。
- 建立 `DocSection --documents--> Requirement`。
- `Related` 中的 `symbol://`、`test://` 先作为 unresolved reference 记录，不做语义推断。

**TDD 起点：**

- Fixture：`docs/watermark.md`。
- 先断言 `specslice index docs` 输出 `Requirements: 1`、`DocSections: 1`。
- 再断言数据库里有 requirement node、doc section node 和 documents edge。
- 再写 broken doc reference 测试。

### MVP-2：Dart Lightweight Adapter 与显式 Trace

**目标：** 不依赖 Dart analyzer，只用 Rust lightweight scanner 提取 Dart 文件、符号、测试和 trace。

**实现范围：**

- 扫描 `lib / test`。
- 提取 file、class、method、function、constructor、import、`test(...)`、`group(...)`。
- 从 doc comment 解析 `@implements`、`@verifies`、`@related`。
- 输出 `LanguageIndexBatch`，由 Core 统一入库。
- 建立 symbol range 和 parent-child hierarchy。

**TDD 起点：**

- Adapter unit tests 覆盖 class、method、constructor、top-level function。
- Test extractor 覆盖 `test('name', ...)` 和 `group('name', ...)`。
- Trace extractor 覆盖 class 上的 `@implements` 和 test 上的 `@verifies`。
- Range mapper 覆盖 method 改动可映射到 method，method 无直接 trace 时可回溯到 parent class。

### MVP-3：Feature Slice

**目标：** 从一个 Requirement 找到文档、实现和测试。

**实现范围：**

- `specslice slice REQ-ID`。
- 只走 confirmed/declared 高可信边。
- 默认不走 imports、calls、references、candidate edges。
- 输出 Docs、Declared Implementation、Linked Tests、Risks。

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
- 支持 direct symbol trace、parent class trace、containing file trace、test file relation、changed doc section relation。

**TDD 起点：**

- 临时 fixture Git 仓库：先提交 baseline，再修改实现类方法。
- 断言 `specslice impact --base main` 输出 changed symbol、affected requirement、affected doc、linked test。
- 再修改 requirement 文档，断言 impact 能输出 changed doc section、related implementation、linked test。

### MVP-5：Basic Checks 与 Agent Context Pack

**目标：** 把图谱结果转换为工程检查和 AI 可用上下文。

**实现范围：**

- Broken Trace Check。
- Missing Linked Test Check。
- Orphan Requirement Check。
- Impact Review Check。
- `specslice context REQ-ID --json` 输出 docs、implementation、linked_tests、risks、files_to_read、tests_to_run。

**TDD 起点：**

- Broken trace 指向不存在 REQ 时返回 error。
- Requirement 有 implementation 但无 verification 时返回 warning。
- Context JSON 必须可反序列化，并包含最小文件集合。

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
   - 覆盖 frontmatter、REQ/AC/ADR 识别、doc section line range、Related unresolved reference。

4. **Dart adapter tests**
   - 使用内联 Dart 源码和 fixture 文件。
   - 覆盖 class/method/function/test/group/import/doc comment trace。
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
- broken related reference 能被 check 报出。
- 文档 line range 能定位到对应 section。

### MVP-2 验收

- fixture 中 Dart files、Symbols、TestCases、Declared implementations、Declared verifications 计数正确。
- `@implements REQ-WATERMARK-001` 能连到 Requirement。
- `@verifies REQ-WATERMARK-001` 能连到 Requirement。
- method 改动能映射到 method symbol。
- method 无直接 trace 时能通过 parent class 回溯到 Requirement。

### MVP-3 验收

- `specslice slice REQ-WATERMARK-001` 能稳定输出需求文档、实现类、测试。
- 无 linked test 时输出 warning/risk。
- 默认不把 imports 纳入 feature slice。

### MVP-4 验收

- 修改实现类后，`specslice impact --base <baseline>` 输出 changed symbol、affected requirement、affected doc、linked test。
- 修改实现类 method 后，impact 能通过 parent class 反查 requirement。
- 修改 requirement 文档后，impact 能输出 changed doc section、related implementation、linked test。
- 未改相关测试时输出 warning，不声称测试覆盖真实行为。

### MVP-5 验收

- `specslice check` 能区分 error、warning、info。
- broken trace 是 error。
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

## 后续验收方式

你开发后，我会按以下顺序验收：

1. 查看 Git diff，确认改动是否落在当前 MVP 范围内。
2. 查测试是否先覆盖目标行为，尤其是 parser、range、impact 传播。
3. 运行格式化、clippy、workspace tests。
4. 运行 fixture CLI e2e。
5. 对照本文件的阶段验收指标逐项打勾。
6. 如无阻塞问题，再按中文提交信息提交或确认可合并。
