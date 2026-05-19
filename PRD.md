# SpecSlice Rust Core + Dart Adapter 分阶段 MVP 与后续演进方案 v0.5

## 0. 文档定位

本文档用于重新收敛 SpecSlice 的开发范围，并明确一个关键边界：

> **SpecSlice 本体不是 Dart 库，而是 Rust 构建的 CLI / Engine / Library；Dart 只是 MVP 阶段优先支持的目标语言 Adapter。**

当前 MVP 不追求“自动理解整个仓库”，而是先跑通一个对 AI 编程最有价值的显式 trace 闭环：

```text
REQ / DocSection
      ↓ explicit trace
Dart Symbol
      ↓ explicit trace
TestCase
      ↓ git diff impact
Affected Requirements / Docs / Tests
      ↓
AI Context Pack
```

完整愿景仍然保留：

```text
Evidence Graph
Candidate Layer
LLM Semantic Inference
Human Review
Feature Slice
PR Impact
GraphRAG
MCP
SCIP / Multi-language
```

但这些不会一次性进入 MVP。

---

## 1. 当前需求重新定义

当前阶段真正需要解决的是：

```text
1. 我有一些文档 / 需求 / 纲要。
2. 我希望这些文档能和 Dart/Flutter 代码建立关联。
3. 我希望某个需求能找到对应实现代码和测试。
4. 我希望某段代码能反查它服务哪个需求。
5. 我希望 PR 改动时能提示影响哪些需求、文档和测试。
6. 我希望给 AI Agent 一个更准确的上下文包。
7. 我希望核心引擎是 Rust，可后续扩展到其他语言，而不是绑定 Dart 生态。
```

当前阶段不解决：

```text
1. 自动理解整个无文档仓库。
2. 自动生成完整需求文档。
3. 自动从代码反推全部功能。
4. 复杂 GraphRAG。
5. 多语言完整代码智能。
6. 精准 call graph。
7. Web UI。
8. MCP 完整服务。
9. Dart resolved AST / analysis server 集成。
```

MVP 的准确定位：

> **显式 trace 驱动的 AI 编程上下文治理工具。**

不是：

> **自动理解整个代码库的 AI 架构大脑。**

---

## 2. MVP 总原则

MVP 的本体实现语言是 Rust。

```text
Rust Core:
- CLI
- Config
- SQLite Store
- Artifact Graph
- Evidence / EdgeAssertion
- Markdown Indexer
- Git Diff / Impact Engine
- Feature Slice Engine
- Context Pack

Language Adapter:
- Dart Adapter 是第一个目标语言适配器
- 后续可扩展 Python / TypeScript / Rust / Go / Java 等
```

MVP 只做一条主线：

```text
REQ 文档
  -> Dart 实现代码
  -> Dart 测试
  -> PR Impact
  -> AI Context Pack
```

MVP 核心原则：

```text
1. 显式 trace 优先。
2. 不依赖 LLM。
3. 不做复杂 call graph。
4. 不做全自动文档关联。
5. 不做大图可视化。
6. 不做复杂 Review Workflow。
7. 不做多语言。
8. Rust Core 先支持 Dart/Flutter Adapter。
9. SQLite 本地存储。
10. PR Impact 是第一阶段 killer feature。
```

---

## 2.1 Dart Adapter 技术边界

Rust Core 不能直接像调用 Rust crate 一样调用 Dart analyzer。

因此 Dart 支持分为两个阶段：

```text
MVP:
  Rust native lightweight Dart adapter
  只做语法级提取：
  - file
  - class
  - method
  - function
  - constructor
  - test(...)
  - group(...)
  - import
  - doc comment trace
  - symbol_ranges

Phase 3:
  Dart analyzer sidecar
  通过外部 Dart helper 进程使用 Dart analyzer
  输出 JSONL / JSON / protobuf 给 Rust Core
```

MVP 不承诺：

```text
1. Dart resolved AST。
2. 精准 call target。
3. Provider / Riverpod / Bloc 语义关系。
4. Flutter widget tree。
5. route graph。
```

Dart Adapter 只负责把 Dart/Flutter 源码转成 SpecSlice 统一中间模型，不直接写数据库。

---

## 3. MVP 与完整架构的边界

### 3.1 MVP 只保留

```text
Rust Core:
- ArtifactNode
- EdgeAssertion
- Evidence
- SQLite Graph Store
- Markdown Requirement Parser
- Git Diff Parser
- Feature Slice Engine
- PR Impact Engine
- Basic Checks
- JSON Context Pack
- JSONL Export

Dart Language Adapter:
- Dart file scanner
- Dart symbol extractor
- Dart test extractor
- Dart trace comment extractor
- Symbol range mapper
- Parent symbol hierarchy
```

### 3.2 MVP 暂缓

```text
CandidateFeature
CandidateTraceEdge
LLM Semantic Inference
infer-features
link-docs
accept-feature
Human Review Layer
GraphRAG
MCP Server
SCIP Adapter
Web UI
Dart analyzer sidecar
```

### 3.3 为什么这样砍

因为当前最重要的是验证：

```text
需求能否稳定关联到代码？
代码改动能否稳定反查需求？
这个结果对 AI 编程是否有帮助？
```

如果这条链路跑不通，引入 LLM / GraphRAG / MCP 只会放大复杂度。

---

## 4. MVP 阶段目标

## MVP-0：项目骨架与存储

### 目标

建立最小可运行 Rust CLI 与 SQLite 图存储。

### 范围

```text
1. specslice init
2. .specslice.yaml
3. .specslice/graph.db
4. ArtifactNode
5. EdgeAssertion
6. Evidence
7. SQLite migrations
8. JSONL export
```

### 验收标准

运行：

```bash
specslice init
specslice export --format jsonl
```

应能生成：

```text
.specslice/
  graph.db
  export/
.specslice.yaml
```

---

## MVP-1：文档 / 需求索引

### 目标

从 Markdown 文档中提取 Requirement、ADR、DocSection。

### 范围

```text
1. 扫描 docs / specs / adr。
2. 解析 frontmatter。
3. 识别 REQ / AC / ADR ID。
4. 建立 Requirement 节点。
5. 建立 DocSection 节点。
6. 建立 DocSection --documents--> Requirement。
7. 支持 Related 中的 symbol/test 字符串，但暂时只作为 unresolved reference。
```

### 文档示例

```md
---
id: REQ-WATERMARK-001
type: requirement
title: Auto watermark placement
status: active
---

# 自动水印放置

用户导入图片后，系统应自动避开人脸区域放置水印。

## Related

- symbol://lib/domain/watermark/auto_placement_service.dart#AutoPlacementService
- test://test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region
```

### 验收标准

运行：

```bash
specslice index docs
specslice check --docs
```

应能输出：

```text
Requirements: 1
DocSections: 1
Broken doc references: 0 / N
```

---

## MVP-2：Dart Adapter 代码索引与显式 Trace

### 目标

在 Rust Core 中通过 Dart language adapter，从 Dart/Flutter 代码中提取文件、类、方法、函数、测试，并解析 `@implements` / `@verifies`。

### 重要边界

MVP 阶段的 Dart Adapter 使用 **Rust native lightweight parser / scanner**，不直接依赖 Dart analyzer。

Dart analyzer sidecar 放到后续 Phase 3。

### 范围

```text
1. 扫描 lib / test。
2. 使用 Dart lightweight adapter 提取：
   - File
   - ClassDeclaration
   - MethodDeclaration
   - FunctionDeclaration
   - ConstructorDeclaration
   - ImportDirective
   - test(...)
   - group(...)
   - Doc comments
3. 建立 contains / imports 边。
4. 解析 doc comment 中的 trace 标签：
   - @implements
   - @verifies
   - @related
5. 建立 declared trace：
   - CodeSymbol --declaresImplementation--> Requirement
   - TestCase --declaresVerification--> Requirement
6. 建立 symbol_ranges。
7. 建立 parent-child symbol hierarchy，用于 PR Impact 传播。
```

### 暂不做

```text
1. 精准 calls。
2. references。
3. provider/riverpod/bloc 关系。
4. widget tree。
5. route graph。
6. resolved AST。
```

### 代码示例

```dart
/// @implements REQ-WATERMARK-001
class AutoPlacementService {
  PlacementResult placeWatermark(...) {
    ...
  }
}
```

测试示例：

```dart
/// @verifies REQ-WATERMARK-001
test('places watermark outside face region', () {
  ...
});
```

### 验收标准

运行：

```bash
specslice index .
specslice check --broken-trace
```

应能识别：

```text
Dart files: N
Symbols: N
TestCases: N
Declared implementations: N
Declared verifications: N
Broken trace links: 0 / N
```

---

## MVP-3：Feature Slice

### 目标

从某个 Requirement 出发，找到相关文档、实现代码和测试。

### 范围

```text
1. specslice slice REQ-ID
2. Requirement Slice Policy
3. 只走 confirmed/declared 高可信边。
4. 不默认走 imports。
5. 不走 LLM candidate。
6. 输出 Docs / Implementation / Tests / Risks。
```

### Traversal Policy

MVP 允许边：

```text
Requirement <-documents- DocSection
CodeSymbol -declaresImplementation-> Requirement
TestCase -declaresVerification-> Requirement
File -contains-> CodeSymbol
ClassSymbol -contains-> MethodSymbol
```

MVP 默认不走：

```text
imports
calls
references
mentions
candidate edges
```

### 输出示例

```text
Feature Slice: REQ-WATERMARK-001 Auto watermark placement

Docs:
- docs/watermark.md#auto-watermark-placement

Declared Implementation:
- lib/domain/watermark/auto_placement_service.dart#AutoPlacementService

Linked Tests:
- test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region

Risks:
- No coverage data imported. Test verification is declared, not proven.
```

### 验收标准

运行：

```bash
specslice slice REQ-WATERMARK-001
```

应能稳定输出：

```text
1. 需求文档位置。
2. 声明实现代码。
3. 声明测试。
4. 断链和缺失项。
```

---

## MVP-4：PR Impact

### 目标

根据 Git diff 找到 changed symbols，并反查受影响需求、文档和测试。

这是 MVP 的关键价值点。

### 范围

```text
1. 读取 git diff --unified=0 base...HEAD。
2. 找 changed files 和 changed line ranges。
3. impact 前检查 changed file hash。
4. 如 hash 不匹配，增量索引 changed files。
5. 用 symbol_ranges 找 changed symbols。
6. 从 changed symbols 反查 declaresImplementation。
7. 支持 changed method -> parent class -> requirement 的影响传播。
8. 支持 changed doc section -> requirement -> implementation/tests 的反向影响。
9. 找 affected requirements。
10. 找 related docs。
11. 找 linked tests。
12. 输出 ImpactReport。
```

### Impact Resolution Policy

PR Impact 不能只查直接 trace。

如果 changed symbol 没有直接关联需求，需要沿父级结构向上查找：

```text
changed method
  -> containing class
  -> class declaresImplementation REQ

changed constructor
  -> containing class
  -> class declaresImplementation REQ

changed private helper
  -> containing class/file
  -> nearest declared implementation REQ
```

MVP 最小传播规则：

```text
1. direct symbol trace
2. parent class trace
3. containing file trace
4. test file relation
5. changed doc section relation
```

### Doc Impact

PR Impact 必须同时支持文档改动。

```text
changed doc section
  -> Requirement
  -> declared implementation
  -> linked tests
```

例如修改：

```text
docs/watermark.md#REQ-WATERMARK-001
```

应输出：

```text
Affected requirement:
- REQ-WATERMARK-001

Related implementation:
- AutoPlacementService

Linked tests:
- auto_placement_service_test.dart
```

### 暂不做

```text
1. 语义级 stale doc 判断。
2. LLM drift check。
3. 精准行为变化分析。
4. 自动判断测试是否真的覆盖。
```

### 输出示例

```text
SpecSlice Impact Report

Changed symbols:
- lib/domain/watermark/auto_placement_service.dart#AutoPlacementService.placeWatermark

Affected requirements:
- REQ-WATERMARK-001 Auto watermark placement

Affected docs:
- docs/watermark.md#auto-watermark-placement

Linked tests:
- test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region

Warnings:
- Affected requirement has linked test, but no related test changed in this PR.

Info:
- Related doc section was not changed. Review whether docs are still accurate.
```

### 验收标准

在 fixture 中修改实现代码后运行：

```bash
specslice impact --base origin/main
```

应能输出：

```text
1. changed symbol
2. affected requirement
3. affected doc
4. linked test
5. warning/info
```

在 fixture 中修改需求文档后运行：

```bash
specslice impact --base origin/main
```

应能输出：

```text
1. changed doc section
2. affected requirement
3. related implementation
4. linked test
```

---

## MVP-5：Basic Checks 与 Agent Context Pack

### 目标

把图谱结果转化为工程检查和 AI 可用上下文。

### Basic Checks

```text
1. Broken Trace Check
   - @implements 指向不存在的 REQ
   - @verifies 指向不存在的 REQ / AC
   - Related 指向不存在的 symbol/test

2. Missing Linked Test Check
   - Requirement 有 declared implementation，但没有 declared verification。

3. Orphan Requirement Check
   - Requirement 没有 declared implementation。

4. Impact Review Check
   - PR 改了 requirement implementation，但相关 test/doc 未变化。
```

### Check 分级

```text
Error:
- broken trace
- missing referenced requirement
- missing referenced symbol in confirmed link

Warning:
- requirement has implementation but no linked test
- changed implementation without related test change

Info:
- related doc not changed
- doc/code may need review
```

### Agent Context Pack

命令：

```bash
specslice context REQ-WATERMARK-001 --json
```

输出：

```json
{
  "feature": {
    "id": "REQ-WATERMARK-001",
    "title": "Auto watermark placement"
  },
  "docs": [
    "docs/watermark.md#auto-watermark-placement"
  ],
  "implementation": [
    "lib/domain/watermark/auto_placement_service.dart#AutoPlacementService"
  ],
  "linked_tests": [
    "test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region"
  ],
  "risks": [
    "Verification is declared, not proven by coverage."
  ],
  "files_to_read": [
    "docs/watermark.md",
    "lib/domain/watermark/auto_placement_service.dart",
    "test/watermark/auto_placement_service_test.dart"
  ],
  "tests_to_run": [
    "test/watermark/auto_placement_service_test.dart"
  ]
}
```

### 验收标准

运行：

```bash
specslice check
specslice context REQ-WATERMARK-001 --json
```

应能输出可用于 AI Agent 的最小上下文。

---

## 5. MVP 最小数据库 Schema

MVP 仍然保留 Evidence-based 架构，但只实现必要表。

### 5.1 nodes

```sql
CREATE TABLE IF NOT EXISTS nodes (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  path TEXT,
  name TEXT,
  start_line INTEGER,
  end_line INTEGER,
  content_hash TEXT,
  stable_key TEXT,
  source_file TEXT,
  source_hash TEXT,
  indexer TEXT,
  index_generation INTEGER,
  metadata_json TEXT
);
```

### 5.2 edge_assertions

```sql
CREATE TABLE IF NOT EXISTS edge_assertions (
  id TEXT PRIMARY KEY,
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  source TEXT NOT NULL,
  certainty TEXT NOT NULL,
  status TEXT NOT NULL,
  confidence REAL NOT NULL,
  evidence_json TEXT,
  source_file TEXT,
  source_hash TEXT,
  indexer TEXT,
  index_generation INTEGER,
  metadata_json TEXT
);
```

### 5.3 evidence

```sql
CREATE TABLE IF NOT EXISTS evidence (
  id TEXT PRIMARY KEY,
  artifact_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  path TEXT,
  start_line INTEGER,
  end_line INTEGER,
  snippet TEXT,
  hash TEXT,
  metadata_json TEXT
);
```

### 5.4 symbol_ranges

```sql
CREATE TABLE IF NOT EXISTS symbol_ranges (
  file_path TEXT NOT NULL,
  symbol_id TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  symbol_kind TEXT,
  qualified_name TEXT,
  parent_symbol_id TEXT
);
```

### 5.5 file_index

```sql
CREATE TABLE IF NOT EXISTS file_index (
  path TEXT PRIMARY KEY,
  hash TEXT NOT NULL,
  kind TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  index_generation INTEGER NOT NULL
);
```

### 5.6 slice_cache

```sql
CREATE TABLE IF NOT EXISTS slice_cache (
  root_id TEXT PRIMARY KEY,
  input_hash TEXT NOT NULL,
  index_generation INTEGER NOT NULL,
  slice_json TEXT NOT NULL,
  generated_at TEXT NOT NULL
);
```

### 5.7 MVP 暂不创建

```text
candidate_features
candidate_edges
review_sessions
review_items
embedding_index
llm_runs
```

这些放到后续演进阶段。

---

## 6. Language Adapter 契约

Language Adapter 是 SpecSlice 后续扩展到多语言的关键边界。

### 6.1 核心原则

```text
1. Adapter 不直接写 SQLite。
2. Adapter 不生成 Feature Slice。
3. Adapter 不判断需求语义。
4. Adapter 只输出统一的 LanguageIndexBatch。
5. Rust Core 负责 ID 规范、Evidence、EdgeAssertion、入库、slice、impact、check。
```

### 6.2 Trait 设计

```rust
pub trait LanguageAdapter {
    fn language_id(&self) -> &'static str;

    fn supports_path(&self, path: &std::path::Path) -> bool;

    fn index_files(
        &self,
        repo_root: &std::path::Path,
        files: &[std::path::PathBuf],
        config: &LanguageConfig,
    ) -> anyhow::Result<LanguageIndexBatch>;
}
```

### 6.3 LanguageIndexBatch

```rust
pub struct LanguageIndexBatch {
    pub language: String,
    pub files: Vec<FileArtifact>,
    pub symbols: Vec<SymbolArtifact>,
    pub tests: Vec<TestArtifact>,
    pub imports: Vec<ImportEdge>,
    pub trace_links: Vec<DeclaredTrace>,
    pub symbol_ranges: Vec<SymbolRange>,
    pub diagnostics: Vec<AdapterDiagnostic>,
}
```

### 6.4 Adapter 输出，Core 转换

```text
Dart Adapter 输出：
- FileArtifact
- SymbolArtifact
- TestArtifact
- ImportEdge
- DeclaredTrace
- SymbolRange

Rust Core 转换为：
- ArtifactNode
- EdgeAssertion
- Evidence
- symbol_ranges
- file_index
```

---

## 7. MVP 目录结构

MVP 阶段不要过度拆 crate。

推荐先使用 5 个 crate：

```text
specslice/
  Cargo.toml
  crates/
    specslice-core/
      src/
        artifact_id.rs
        node.rs
        edge_assertion.rs
        evidence.rs
        result_types.rs
        language_batch.rs

    specslice-store/
      src/
        sqlite.rs
        migrations.rs
        repositories.rs

    specslice-lang-dart/
      src/
        dart_adapter.rs
        lightweight_parser.rs
        dart_symbol_extractor.rs
        dart_test_extractor.rs
        dart_trace_comment_extractor.rs
        symbol_range_mapper.rs

    specslice-engine/
      src/
        config.rs
        docs_indexer.rs
        git_diff.rs
        slice.rs
        impact.rs
        checks.rs
        context_pack.rs
        export.rs

    specslice-cli/
      src/
        main.rs
        commands/
          init.rs
          index.rs
          slice.rs
          impact.rs
          check.rs
          context.rs
          export.rs
```

后续稳定后再拆出：

```text
specslice-docs
specslice-git
specslice-slice
specslice-impact
specslice-checks
specslice-context
specslice-mcp
```

---

## 8. MVP 配置文件

`.specslice.yaml`：

```yaml
repo:
  root: .
  default_branch: main

storage:
  path: .specslice/graph.db

docs:
  paths:
    - docs
    - specs
    - adr
  include:
    - "**/*.md"
    - "**/*.mdx"
  requirement_patterns:
    - "REQ-[A-Z]+-[0-9]+"
    - "AC-[A-Z]+-[0-9]+-[0-9]+"
  adr_patterns:
    - "ADR-[0-9]+"

code:
  language: dart
  paths:
    - lib
    - test
  adapter:
    backend: lightweight
  exclude:
    - .dart_tool
    - build
    - generated
    - "**/*.g.dart"
    - "**/*.freezed.dart"

trace:
  explicit_tags:
    implements: "@implements"
    verifies: "@verifies"
    related: "@related"

slice:
  max_depth: 3
  max_nodes: 120
  min_score: 0.35
  include_imports: false
  include_candidates: false

impact:
  auto_reindex_changed_files: true
  propagate_to_parent_symbol: true
  include_doc_changes: true
  stale_doc_level: info
  missing_test_change_level: warning

checks:
  broken_trace_level: error
  missing_linked_test_level: warning
  orphan_requirement_level: warning
```

`code.adapter.backend` 后续可扩展为：

```text
lightweight       MVP：Rust 原生轻量解析
analyzer_sidecar  后续：Dart helper 进程使用 Dart analyzer
analysis_server   后续：接 Dart analysis server / LSP
```

---

## 9. MVP 数据模型收敛

以下枚举和结构体应定义在 Rust Core 中；Dart 只是 language adapter 输出这些统一模型。

### 9.1 EdgeKind MVP

```rust
pub enum EdgeKind {
    Contains,
    Imports,
    Documents,
    DeclaresImplementation,
    DeclaresVerification,
    RelatedTo,
}
```

后续再扩展：

```text
References
Calls
Covers
CandidateImplements
CandidateDocuments
CandidateVerifies
CoChangedWith
```

### 9.2 EdgeSource MVP

```rust
pub enum EdgeSource {
    Filesystem,
    LanguageAdapter,
    Markdown,
    ExplicitTrace,
    GitDiff,
}
```

后续再扩展：

```text
GitHistory
Coverage
SearchCandidate
Clustering
LlmSuggested
HumanConfirmed
```

### 9.3 EdgeCertainty MVP

```rust
pub enum EdgeCertainty {
    Fact,
    Declared,
}
```

后续再扩展：

```text
Observed
Candidate
```

### 9.4 EdgeStatus MVP

```rust
pub enum EdgeStatus {
    Confirmed,
    Deprecated,
}
```

后续再扩展：

```text
PendingReview
Rejected
```

---

## 10. MVP 验收用例

### Fixture 项目

```text
example/flutter_watermark_app/
  docs/
    watermark.md
  lib/
    domain/
      watermark/
        auto_placement_service.dart
        placement_candidate.dart
  test/
    watermark/
      auto_placement_service_test.dart
```

### docs/watermark.md

```md
---
id: REQ-WATERMARK-001
type: requirement
title: Auto watermark placement
---

# 自动水印放置

系统应自动避开人脸区域放置水印。
```

### auto_placement_service.dart

```dart
/// @implements REQ-WATERMARK-001
class AutoPlacementService {
  PlacementResult placeWatermark(...) {
    ...
  }

  double scoreCandidate(...) {
    ...
  }
}
```

### auto_placement_service_test.dart

```dart
/// @verifies REQ-WATERMARK-001
test('places watermark outside face region', () {
  ...
});
```

### 验收命令

```bash
specslice init
specslice index .
specslice slice REQ-WATERMARK-001
specslice impact --base origin/main
specslice check
specslice context REQ-WATERMARK-001 --json
```

### 验收结果

必须能证明：

```text
1. 能从 REQ 找到文档。
2. 能从 REQ 找到实现类。
3. 能从 REQ 找到测试。
4. 改实现类后，impact 能反查 REQ。
5. 改实现类中的 method 后，impact 能通过 parent class 反查 REQ。
6. 改需求文档后，impact 能反查相关实现代码和测试。
7. impact 能提示 linked test/doc。
8. context pack 能给 AI Agent 最小上下文。
```

### MVP 不覆盖的项目状态

MVP 不负责自动理解无 trace 仓库。

```text
无 REQ 文档
无 @implements
无 @verifies
```

这种项目需要后续 Phase 1 的 Candidate Layer。

---

## 11. 库调用时返回的核心产物

SpecSlice 内部维护完整 Artifact Graph，但外部不应默认返回完整图。

外部返回任务相关产物：

```text
IndexResult
FeatureSlice
ImpactReport
CheckReport
AgentContextPack
ExportBundle
```

这些类型定义在 Rust Core 中，并应支持：

```rust
serde::Serialize
serde::Deserialize
Debug
Clone
```

### 11.1 示例调用

```rust
let engine = SpecSliceEngine::open(".")?;

let index_result = engine.index(IndexOptions::default())?;

let slice = engine.slice_requirement("REQ-WATERMARK-001")?;

let impact = engine.impact(ImpactOptions {
    base_ref: "origin/main".into(),
    head_ref: "HEAD".into(),
})?;

let context = engine.build_agent_context("REQ-WATERMARK-001")?;
```

### 11.2 产物说明

```text
FeatureSlice:
  这个需求由哪些文档、代码、测试组成？

ImpactReport:
  这次改动影响了哪些需求、文档、测试？

CheckReport:
  当前 trace 是否断链、缺测试、缺实现？

AgentContextPack:
  AI Agent 修改这个功能前应该读哪些内容、跑哪些测试？
```

---

## 12. 后续演进路线

## Phase 1：LLM Candidate Layer

### 目标

解决“有文档但无显式 trace”或“老仓库无文档”的问题。

### 新增能力

```text
1. CandidateFeature
2. CandidateTraceEdge
3. candidate_features 表
4. candidate_edges 表
5. specslice link-docs
6. specslice infer-features
7. specslice accept-feature
```

### 原则

```text
1. LLM 只生成 candidate。
2. Candidate 必须有 evidence。
3. Candidate 默认 pendingReview。
4. CI 不信任 candidate。
```

---

## Phase 2：Review Workflow

### 目标

让候选关系可以低成本确认。

### 新增能力

```text
1. ReviewSession
2. ReviewItem
3. ReviewDecision
4. specslice review --interactive
5. accept / reject / merge candidate
6. 批量确认 feature cluster
```

---

## Phase 3：更强 Dart 语义关系

### 目标

增强 Dart/Flutter 语义索引。

### 新增能力

```text
1. Dart analyzer sidecar
2. resolved AST references
3. simple calls
4. constructor usages
5. route hints
6. Flutter Page / Screen 识别
7. Provider / Riverpod / Bloc 轻量规则
8. coverage import
```

注意：不要追求完整 call graph。

目标是提升 Feature Slice 的上下文质量，而不是做 Sourcegraph 替代品。

---

## Phase 4：Agent Integration

### 目标

把 SpecSlice 接入 AI 编程工具。

### 新增能力

```text
1. specslice mcp
2. get_feature_slice
3. get_pr_impact
4. find_docs_for_symbol
5. find_tests_for_requirement
6. explain_symbol_context
```

---

## Phase 5：GraphRAG / Semantic Query

### 目标

支持自然语言查询和语义摘要。

### 新增能力

```text
1. semantic search
2. graph-aware retrieval
3. feature summary
4. PR semantic drift candidate
5. doc-code mismatch candidate
```

原则：

```text
1. GraphRAG 不作为事实源。
2. 只用于查询、摘要、候选发现。
3. 输出必须带 evidence。
```

---

## Phase 6：SCIP / 多语言 / 高性能图存储

### 目标

在 Rust Core 已经存在的基础上，从“Rust Core + Dart Adapter”升级为高性能、多语言基础库。

### 新增能力

```text
1. compact graph store
2. SCIP adapter
3. Tree-sitter fallback
4. GitNexus adapter
5. multi-repo support
6. faster PR impact
7. additional language adapters
```

### 保持兼容

Rust MVP 必须保留：

```text
1. JSONL export
2. stable node IDs
3. edge assertion schema
4. .specslice.yaml
```

这样未来多语言版本、SCIP 版本、GraphRAG 版本都可以读取 MVP 阶段产物。

---

## 13. 开发优先级总表

| 阶段 | 目标 | 是否 MVP 必需 |
|---|---|---|
| MVP-0 | Rust CLI + SQLite | 必需 |
| MVP-1 | 文档/REQ 索引 | 必需 |
| MVP-2 | Dart lightweight adapter + trace | 必需 |
| MVP-3 | Feature Slice | 必需 |
| MVP-4 | PR Impact | 必需 |
| MVP-5 | Checks + Context Pack | 必需 |
| Phase 1 | LLM Candidate Layer | 后续 |
| Phase 2 | Review Workflow | 后续 |
| Phase 3 | Dart analyzer sidecar | 后续 |
| Phase 4 | MCP / Agent 集成 | 后续 |
| Phase 5 | GraphRAG / Semantic Query | 后续 |
| Phase 6 | SCIP / 多语言 / 高性能图存储 | 后续 |

---

## 14. MVP 不变的核心架构原则

```text
1. Graph is not truth. Evidence is truth.
2. LLM suggests. Human confirms.
3. CI trusts only deterministic or confirmed edges.
4. Feature Slice is a derived view.
5. PR Impact is the main engineering value.
6. Prefer fewer high-confidence links over many noisy links.
7. Do not build a giant graph visualization first.
8. Do not use imports as default feature traversal.
9. Do not claim @verifies proves behavior.
10. Keep the protocol migration-friendly.
11. Adapter outputs facts; Core owns storage and semantics.
```

---

## 15. 最终结论

SpecSlice 的完整愿景是：

> **面向 AI 编程的代码库意图治理层。**

但当前 MVP 只需要证明一个闭环：

```text
REQ 文档
  -> Dart 实现代码
  -> Dart 测试
  -> PR Impact
  -> AI Context Pack
```

MVP 的正确形态是：

```text
Rust CLI + SQLite + Markdown REQ + Dart lightweight adapter + explicit trace + Feature Slice + PR Impact + Context Pack
```

它不承诺：

```text
Dart semantic analyzer
call graph
LLM
GraphRAG
MCP
无文档自动理解
多语言
```

只要这条链路跑通，就已经能补充当前 AI 编写代码的关键短板：

```text
1. AI 不知道需求。
2. AI 不知道代码为什么存在。
3. AI 不知道该读哪些测试。
4. AI 不知道改动影响哪些文档。
5. AI 不知道 PR 的功能影响面。
```

因此第一阶段不要追求“自动理解整个仓库”。

正确路线是：

```text
先用 Rust Core + Dart lightweight adapter + 显式 trace 跑通 PR Impact。
再用 LLM Candidate Layer 解决老仓库和无文档问题。
最后再演进到 MCP / GraphRAG / SCIP / 多语言。
```
