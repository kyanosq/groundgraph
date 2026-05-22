# specslice 0.2.0 — 真实仓库扫描验收报告

本目录记录 `specslice 0.2.0` 在四个真实仓库上的扫描结果，作为 P20（TypeScript / Java + 多语言一致性）正式收口的发布证据。

## 非侵入性约定

为了让目标仓库保持 0 副作用（连 `graph.db` 都不能写到用户的代码库里），所有扫描走 **shadow scratch** 模式：

1. `scripts/release_scan.sh` 用 `rsync` 把源仓的源码同步到 `release-scans/_scratch/<name>/`，自动剔除 `.git / node_modules / .venv / target / build / .dart_tool / dist / Pods` 等本地工具产物；
2. 在 scratch 副本里生成 `.specslice.yaml`（开启对应语言的 adapter），跑 `specslice init / index / check / graph / dead-code`；
3. 摘要写到 `reports/release/<name>/report.md`，二进制图 / HTML 留在仓库内部并 gitignore；
4. **目标仓库自始至终没有任何文件被创建、修改或删除**。本目录的所有副作用都发生在 specslice 自己的工作区里。

证据：扫描完成后 `ls -la <target>/.specslice` 全部命中本次扫描之前的旧时间戳（或目录根本不存在）；`pixcraft-landing` 与 `vub` 的目标仓 **当前** 仍没有 `.specslice/` 子目录。

## 扫描总览（specslice 0.2.0）

| 仓库 | 语言 | 文件 | 符号 | 测试 | Imports | 节点 | 边 | Resolver |
|------|------|------|------|------|---------|------|----|----------|
| [pixcraft-app](./pixcraft-app/report.md) | Dart (Flutter) | 151 | 6964 | 366 | n/a | 7653 | 8869 | `dart_analyzer` |
| [pixcraft-landing](./pixcraft-landing/report.md) | TypeScript (React) | 22 | 102 | 12 | 71 | 136 | 180 | `typescript_lsp` |
| [atagent](./atagent/report.md) | Python (FastAPI) | 165 | 1224 | 272 | 665 | 1807 | 2054 | `python_ast`（soft-skip） |
| [vub](./vub/report.md) | Java (Maven, 多模块) | 3111 | 16099 | 0 | 25194 | 18295 | 40239 | `java_ast`（soft-skip） |

`Resolver` 为 `*_ast` 表示当前本机没有装对应的 LSP（pyright/basedpyright/pylsp 或 jdtls），新加固后的 probe 在启动 smoke 失败时 soft-skip 并打印原因，AST 通路无缝接管。

## 节点类型 top-8（验证 0.2.0 的多语言 NodeKind 真的落地）

```
pixcraft-app: dart_method(6371) test_case(366) dart_class(225) dart_function(199) dart_constructor(156) file(153) test_group(86) module(67)
pixcraft-landing: typescript_function(33) typescript_method(28) file(23) typescript_module(22) typescript_interface(12) test_case(8) module(4) test_group(4)
atagent: python_method(535) python_function(459) python_class(230) test_case(218) file(174) doc_section(91) test_group(54) module(46)
vub: java_method(11423) file(3112) java_class(2452) java_interface(635) java_constructor(430) module(137) java_package(93) java_enum(12)
```

亮点：
- `vub` 命中 `java_enum: 12`，正是本次小批次新增的 `JavaEnum` NodeKind，证明 P20 收口补丁里的 enum 语义在真实大型 Java 仓里有效（vub 是多模块 Maven 工程）。
- `pixcraft-landing` 走的是真实 `typescript-language-server --stdio`（`Resolver: typescript_lsp`），输出 `typescript_module / typescript_interface / typescript_function / typescript_method` 全套节点，并把 vitest / jest 的 `describe/it` 翻译成 `test_group / test_case`。
- `atagent` 的 `python_ast` fallback 在没有 pyright/pylsp 的情况下依旧识别出 165 个 module、272 个 test case、45 个 framework entrypoint（FastAPI 路由 / pydantic 验证器），证明 P17 的 framework classifier 在真实 FastAPI 仓上工作。
- `pixcraft-app` 走 Dart sidecar (`Resolver: dart_analyzer`)，6964 个 symbol、366 个 test case、8869 条边，并且 `dead-code (high)` 给出了 6 个真实候选（前 30 行附在 `pixcraft-app/report.md` 里），全部带 `confidence: high` + 中文 reason，每条都明确说不是「自动删除」。

## 复现方式

```bash
cargo build -p specslice-cli --release
bash scripts/release_scan.sh pixcraft-app    /Users/qjs/Code/My/bean/pixcraft-app     dart
bash scripts/release_scan.sh pixcraft-landing /Users/qjs/Code/My/bean/pixcraft-landing typescript
bash scripts/release_scan.sh atagent          /Users/qjs/Code/Projects/atagent          python
bash scripts/release_scan.sh vub              /Users/qjs/Code/Demo/vub                  java
```

每个 `report.md` 自带：
- 完整的 `specslice index` 输出（含 LSP / AST resolver 信息）
- `specslice check` 摘要
- Graph code-view 规模
- `dead-code --json --min-confidence high` 的前 30 行

具体的 JSON / SQLite 产物（`graph-code.json` / `graph-business.json` / `dead-code-high.json` / 中间态 `.specslice/graph.db`）保留在本目录里供二次分析，但都已 gitignore，不会进入 release tarball。
