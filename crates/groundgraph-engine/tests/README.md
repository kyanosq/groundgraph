# GroundGraph 引擎测试体系

本目录是引擎的集成测试层。整个仓库的测试按 6 层组织，新增测试请先
判断它属于哪一层，再按对应约定落位。

## 分层

| 层 | 位置 | 职责 | 运行 |
| --- | --- | --- | --- |
| L0 单元 | 各 `src/*.rs` 的 `#[cfg(test)]` | 单函数/单结构的行为与边界 | `cargo test -p <crate> --lib` |
| L1 性质（property） | `p23_totality_proptest.rs`、`p25_scanner_totality_proptest.rs`、`store/tests/proptest_roundtrip.rs` | 手写扫描器/分词器对任意 UTF-8 **全域不 panic、确定性**；存储 roundtrip | `cargo test --test 'p2*'` |
| L2 金标准（golden） | `p4_pixcraft_golden.rs`、`p5_search_golden.rs`、`p7_dead_code_golden.rs`、`p8_semantic_edges.rs`、`p9_business_candidates.rs` | 对固定 fixture 仓库的端到端输出钉死（图形状、检索命中、死代码、语义边、候选） | `cargo test --test 'p[4-9]*'` |
| L3 能力回归 | `search_content_layer.rs`、`check_doc_drift.rs`、`requirements_md.rs`、`checks_and_context.rs`、`links_manifest.rs` 等 | 一个用户可感知能力 = 一个域名文件；文件头注释必须写清"钉住什么行为、为什么" | `cargo test --test <名字>` |
| L4 自举（self-host） | `p21_rust_self_host.rs` | GroundGraph 索引自身工作区必须产出有意义的图 | `cargo test --test p21_rust_self_host` |
| L5 端到端 | `crates/groundgraph-cli/tests/*`、`crates/groundgraph-mcp/tests/protocol.rs` | CLI 人类输出 / JSON 合同 / MCP 协议 | `cargo test -p groundgraph-cli -p groundgraph-mcp` |

## 命名约定

- **新文件一律按"域"命名**：`search_content_layer.rs`、`check_doc_drift.rs`。
- `pN_` 前缀是历史阶段编号（对应 `docs/implementation-plan.md` 的 P4/P5/…），
  保留以维持计划可追溯性；不要给新文件继续编号。
- 每个测试名描述**行为**而非实现：`stale_doc_refs_are_reported_with_precision`，
  而不是 `test_extract_refs_2`。

## 约定

1. **TDD**：先写失败测试（red），再实现（green）。提交里测试与实现同行。
2. **真实输出验收**：金标准/能力回归断言真实命令产物，不 mock 引擎内部。
3. **精度优先**：凡是会"报告问题"的能力（checks、dead-code），必须同时
   测"该报的报了"与"不该报的没报"（误报守卫）。
4. **扫描器全域性**：任何新的手写文本扫描器（路由、SQL、常量、分词）
   必须在 L1 的 proptest 套件里登记，token 池中加入触发它的片段。
5. **外部依赖跳过**：依赖 Dart SDK / LSP / SCIP 二进制的测试必须探测
   可用性并优雅 skip，CI 不应因环境缺工具而红。

## 检索质量金标准

检索能力的"对/错"在 `search_content_layer.rs`（内容层）与
`p5_search_golden.rs`（结构层）。给排序/分词/权重做任何改动时：

- 概念查询（词只在注释/文档正文）：`byte boundary panic`、`错位竞争`
  必须命中正确节点（内容层）。
- 符号查询：`ensure query indexes` 类 name-token 命中必须排第一（结构层）。
- 权重位次：`SCORE_CONTENT_ALL` 介于 `SCORE_PATH_SEGMENT` 与
  `SCORE_NAME_TOKEN` 之间；改权重先改这两个文件里的断言再改实现。
