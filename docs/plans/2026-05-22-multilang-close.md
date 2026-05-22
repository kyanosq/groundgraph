# 2026-05-22 — P18 / P19 收口前的硬门槛 + TS/Java 端到端

> **状态**：执行中。每完成一个 Phase 在下方打 ✓ 并贴真实命令输出。
> **守则**：每个新增行为必须先写失败测试 → 看到失败原因正确 → 写最小实现 → 跑测试至绿 → 必要时 refactor。

## 背景

外部审查（用户）在 P18/P19 收口前指出 5 个 P1/P2 缺陷：

1. **[P1] TypeScript / Java 没有实际纳入。** `EngineConfig` / `IndexResult` /
   `NodeKind` 只到 Python；没有任何 TS / Java 适配器代码。先前在 SKILL.md /
   implementation-plan.md 暗示 "Python + TypeScript + Java" 已支持是过度承诺。
2. **[P1] Python opt-in LSP smoke 失败。** `python_lsp_available` 只校验
   `binary_on_path` (`path.is_file()`)，不真启动；本机 `pylsp` 文件存在但
   shebang 指向已删除的 Anaconda 解释器，导致 probe 返回可用、`run_profile`
   启动失败、`assert_eq!(result.resolver_used, "python_lsp")` 失败。
3. **[P1] `specslice questions` 读不到真实 pending candidates。** `questions.rs`
   只扫 `Store` 里的 `BusinessCandidate` 节点；实际候选来自
   `.specslice/candidates/business_logic.yaml`，从未持久写入 store。结果是真实
   仓库的 pending 候选永远不在报告里。
4. **[P2] `graph-diff` 注释承诺了 candidate diff，但实现没做。**
   `GraphDiffOptions` 只接 `base_db / head_db`，不读仓库根；输出也没有
   candidates 字段。
5. **[P2] 跨语言一致性漂移。** `questions.rs` 自己复制 `is_code_symbol`，遗漏
   `SwiftInitializer / SwiftEnum / SwiftProtocol / GoInterface / PythonModule`。
   `search / graph / dead-code / similarity / select-tests / MCP kind parsing`
   各自一组 `match kind`，没有公共源头。

## 范围决定（用户 2026-05-22 拍板）

- (b) **本轮一并做完 TS + Java 完整支持**（NodeKind + EngineConfig +
  IndexResult + LSP 适配器 + AST 补强 + framework 识别 + fixture + opt-in
  smoke + 矩阵测试）。允许跨多个会话。
- (D-1) **`graph-diff` 实现 candidate diff**：`GraphDiffOptions` 扩展
  `base_repo_root: Option<PathBuf>` / `head_repo_root: Option<PathBuf>`，传入则
  加载两份 YAML 做 candidate `added / removed / status_changed`；不传则保持现
  状（兼容 `--base-db` 单独使用）。

## 执行阶段

### Phase A — `language_traits` 公共谓词层

文件：`crates/specslice-core/src/language_traits.rs`

API（全部以 `NodeKind` 为输入，纯函数）：

```rust
pub enum Language { Dart, Swift, Go, Python, Typescript, Java, Doc, Markup, Unknown }

pub fn language_of(kind: NodeKind) -> Language;
pub fn is_code_symbol(kind: NodeKind) -> bool;          // 任何"代码内被书写的可命名实体"
pub fn is_callable(kind: NodeKind) -> bool;              // function / method / constructor
pub fn is_type(kind: NodeKind) -> bool;                  // class / struct / enum / interface / protocol
pub fn is_module_or_file(kind: NodeKind) -> bool;        // File / *Module / *Package
pub fn is_test(kind: NodeKind) -> bool;                  // TestCase / TestGroup
pub fn default_dead_code_reason(kind: NodeKind) -> &'static str;
pub fn search_aliases(kind: NodeKind) -> &'static [&'static str];
pub fn graph_column(kind: NodeKind) -> GraphColumn;      // 与 specslice-engine 的 GraphColumn 复用
pub fn similarity_supported(kind: NodeKind) -> bool;     // tier 1 / tier 2 是否扫该 kind
```

> `GraphColumn` 在 `specslice-engine`，为避免循环依赖，`graph_column` 留在
> `specslice-engine::graph` 里再调 `language_traits` 的若干小谓词；而不是把
> `GraphColumn` 下沉到 core。

矩阵测试：用 `NodeKind` 的所有变体跑一次，断言每个谓词都有定义、且
`is_code_symbol` ⊇ `is_callable ∪ is_type ∪ is_module_or_file`，
`is_test` 与 `is_code_symbol` 互斥（除非 framework metadata 另说）。
回归测试要明确把 `questions.rs` 遗漏的那批 (`SwiftInitializer / SwiftEnum /
SwiftProtocol / GoInterface / PythonModule`) 全部覆盖到。

### Phase B — 迁移现有调用方

| 调用方 | 当前实现 | 迁移目标 |
|---|---|---|
| `questions.rs::is_code_symbol` | 局部 `match`，漏 5 个 kind | `language_traits::is_code_symbol` |
| `search.rs` 别名扩展 | `match` 中分散写 | `language_traits::search_aliases` |
| `graph.rs::column_for_node` | 长 `match`（已大致完整） | 用 `language_traits::language_of` 派生 |
| `dead_code.rs` reason | 局部 `match` | `language_traits::default_dead_code_reason` |
| `similarity.rs` | 检测 Python / Dart 才参与 | `language_traits::similarity_supported`，自动覆盖新语言 |
| `test_selection.rs::enclosing_module_id` | 注释写过这事 | 用 `language_traits::is_module_or_file` |
| MCP `kind` 解析 | `serde` 自带 | 用 `language_of` 校验，给"未知 kind"返回错误 |

每个迁移点要有"用 trait 后能识别原本漏掉的 kind"的回归测试。

### Phase C — `questions` 加载 YAML candidates

`QuestionsOptions` 已经持有 `repo_root: PathBuf`（如果没有，先加上）。
分支 `pending_candidate` 改为：

1. `load_business_candidates(repo_root)` → `BusinessCandidatesDocument`
2. 对每个候选：当 `review_status()` ∈ `{None, NeedsChanges, Pending}` 时计入
   pending 类别（与 `list_for_review` 的语义一致）。
3. `artifact_id` 用 `candidate_artifact_id(&c.id)`，`path` 指向 candidates YAML。

测试：构造临时仓含两 pending + 一 accepted 的 YAML → `analyze_questions` →
断言 `by_category["pending_candidate"] == 2` 且 prompt 含 candidate name。

### Phase D — `graph-diff` candidate diff

`GraphDiffOptions` 增字段：

```rust
pub base_repo_root: Option<PathBuf>,
pub head_repo_root: Option<PathBuf>,
```

`GraphDiff` 增字段（向后兼容，缺省 `Vec::new()`，stats 默认 0）：

```rust
pub candidates_added: Vec<DiffCandidate>,
pub candidates_removed: Vec<DiffCandidate>,
pub candidates_status_changed: Vec<DiffCandidateStatusChange>,
```

`DiffCandidate { id, name, status, path }` / `DiffCandidateStatusChange { id,
from_status, to_status }`。两边都传 repo_root 才比较；任一缺失则三个 vec 留空且
stats 不计入。

CLI：`specslice graph-diff` 增 `--base-root <path>` / `--head-root <path>`。

### Phase E — Python LSP 真启动 probe

`ProbeOutcome::from_options` 末端的"resolved a command"位置，新增一步：
`verify_python_lsp_runs(cmd)`：

- `std::process::Command::new(cmd).arg("--help").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()`
- 异步等 1.5s。`wait_timeout` 不在标准库 → 自己 spin sleep + try_wait（小循环）
  以避免引入新 crate。
- 退出码非 0 且 stderr 含 `bad interpreter` / `No module named` / `cannot
  execute` → 返回 `command=None`, `skip_reason="..."`。
- 退出码非 0 但其它原因 → 仍接受（部分 LSP 在 `--help` 后返回 1 是历史 bug），
  但记 warn。
- 启动失败（Err）→ 拒绝。

测试：在 tmpdir 写一个 `bad_pylsp` shell 脚本 `#!/path/that/does/not/exist`，
chmod +x，PATH 加进去 → `python_lsp_available` 必须返回 false。

opt-in smoke 测试：跑完 `index_python` 后若 `resolver_used != "python_lsp"`，
打印 `eprintln!("soft-skip: pylsp probe ok but adapter fell back to AST")` 并
`return`，不要再 `assert_eq!`。

### Phase F — TypeScript + Java 端到端

#### F1 NodeKind 扩展

```rust
TypescriptModule,           // .ts / .tsx 文件级 (file id::<rel> 之外的语义节点)
TypescriptClass,
TypescriptInterface,
TypescriptFunction,
TypescriptMethod,
TypescriptEnum,
JavaPackage,
JavaClass,
JavaInterface,
JavaMethod,
JavaConstructor,
```

#### F2 配置

```rust
EngineConfig {
    // ...
    pub typescript: LanguageAdapterConfig, // 与 swift/go/python 同款
    pub java: LanguageAdapterConfig,
}

IndexResult {
    // ...
    pub typescript: Option<TypescriptIndexResult>,
    pub java: Option<JavaIndexResult>,
}
```

#### F3 `typescript_indexer.rs`

- LSP：`typescript-language-server --stdio`（npm 包：
  `typescript-language-server` 依赖 `typescript`），env 覆盖
  `SPECSLICE_TYPESCRIPT_LSP_BIN`。
- AST 补强：`scan_typescript` 用 `tree-sitter-typescript` 解 imports +
  `describe/it`（jest/vitest）。
- 框架：Express (`app.get('/x', handler)`), Hono, Fastify, Next API routes
  (`pages/api/foo.ts` 默认导出函数)。最少两条规则就够开口。

#### F4 `java_indexer.rs`

- LSP：`jdtls`（Eclipse JDT Language Server）。配置复杂、启动慢 → smoke 测试只
  在 `SPECSLICE_JAVA_LSP_BIN` 显式设置时跑。
- AST 补强：`scan_java` 用 `tree-sitter-java`，提 package 声明 +
  `@Test`（JUnit 4/5）。
- 框架：Spring Boot (`@RestController` + `@GetMapping/@PostMapping`)。

#### F5 Fixtures

- `crates/specslice-engine/tests/fixtures/typescript_hello/`：含
  `src/greeter.ts`, `src/api.ts` (Express route), `tests/greeter.test.ts`
  (jest)。
- `crates/specslice-engine/tests/fixtures/java_hello/`：含
  `src/main/java/com/example/Greeter.java`, `src/main/java/com/example/api/HelloController.java`
  (Spring), `src/test/java/com/example/GreeterTest.java` (JUnit5)。

#### F6 测试矩阵

- 默认（无 LSP）：AST 扫描，产 file/imports/test_case；
- opt-in LSP smoke：与 python_indexer 同款；
- 跨语言一致性：`language_traits` 矩阵测试覆盖所有新 kind；`similarity` /
  `search` / `dead-code` / `questions` / `select-tests` 跨语言回归测试。

### Phase G — 文档诚实回归

- `docs/implementation-plan.md`：补本次审查纪要；列出实际落地语言；删除任何"已
  支持 TS / Java"的暗示。
- `packaging/skills/specslice/SKILL.md`：同上，并补 TS / Java 的输出 schema 与
  框架识别清单。
- 本计划文档的"完成"勾选与命令输出。

### Phase H — 真实复跑验证

- `atagent`（Python）：`index / similar / questions / graph-diff /
  select-tests` 全部跑一遍。
- 新建一个 TS demo 仓（或挑现成开源仓）+ 一个 Java demo 仓，跑全套。
- 命令输出贴回本计划。

## 验收门槛

收口前必须全部为真：

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`（含 TS / Java 默认测试）
- [ ] `cargo test -p specslice-engine --test lsp_indexers -- --include-ignored`
      要么 Python/TS/Java 三个 smoke 都过，要么按上述 soft-skip 协议跳过且打印
      可读原因，绝不留断言失败。
- [ ] `dart test`（保持当前 6 通过）。
- [ ] atagent / TS 仓 / Java 仓真实输出贴在本计划末尾。
