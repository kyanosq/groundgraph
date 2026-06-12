# SpecSlice 代码审查报告

**审查时间**：2026-06-12
**审查范围**：crates/* 全部 src 文件（约 91,680 行 Rust 代码）
**审查方法**：5 个并行 agent 按模块分工（core+store / engine 算法 / engine 数据流 / cli / dart+mcp），主审查交叉验证关键发现
**项目约束基线**：AGENTS.md、CONTRIBUTING.md（不允许 unsafe、非侵入式只写 `.specslice/`、零 clippy 警告、测试驱动）

共记录 30 个问题。严重度分级：**High**（生产可触发，影响数据/安全）、**Medium**（边界条件或显著性能/设计缺陷）、**Low**（性能微优化或潜在隐患）。

---

## 处理结果（2026-06-12 复核）

每个问题先经源码复核，确认存在的按 TDD（先失败测试、后修复）处理；全量 `cargo test --workspace` 与 `cargo clippy --all-targets` 通过。

| # | 结论 | 处理 |
|---|------|------|
| 1 | 确认 | `sanitize_for_script` / `sanitize_json_for_script` 改为 UTF-8 安全的 `replace("</", "<\\/")`，新增中文/emoji 回归测试 |
| 2 | 确认 | `repaintEdgeDetail` / 解析错误兜底 / 头部统计全部改为 DOM 构建 + `textContent`，新增"innerHTML 禁止拼接"不变量测试 |
| 3 | 部分成立 | `escapeText` 原本已覆盖，属防御性问题；补 `escapeAttr(e.layer)`、`escapeAttr(classes)`、统计行 DOM 化 |
| 4 | 确认 | `clear_indexer_outputs` 增加孤立 `node_fts` 行清理（先测后修） |
| 5 | 确认 | `update_depth` / `strip_strings_and_comments` 改为带 `escaped` 状态机，正确处理 `\\` 与 `\"` |
| 6 | 确认 | `parse_import` / `extract_call_arg` 改用 `find_unescaped` 找未转义引号 |
| 7 | 确认 | `scan_identifiers` 改为 `char_indices()`，杜绝多字节切片 panic |
| 8 | 误报 | MCP stdio 传输规范即为换行分隔 JSON-RPC，无需 `Content-Length` 帧（LSP 才需要） |
| 9 | 确认 | `get_subgraph` BFS 增加 `depth≤16`、节点 2000、边 8000 预算；超限时响应带 `"truncated": true`，新增密集图测试 |
| 10 | 确认 | Java 实体解析改用 `brace_counts_outside_strings`（跳过字符串/字符字面量与 `//` 注释） |
| 11 | 确认 | `balanced_parens` 增加字符串字面量与转义跟踪 |
| 12 | 确认 | 簇分数不再依赖"已发现的对"，对簇内全部成员重算 pairwise simhash，报告真实 worst-case |
| 13 | 按设计 | `normalize` 对未闭合注释/字符串静默 EOF 是对损坏输入的容错启发式，不改 |
| 14 | 确认 | `scip_runner` 改为 `run_with_capped_stderr`：stdout 重定向 null，stderr 只保留 64 KiB 前缀并持续排水防死锁 |
| 15 | 政策澄清 | 显式 `--output` 是用户意图的逃生门，不算非侵入违规；已在 CLI 帮助文本写明（省略时只写 `.specslice/`） |
| 16 | 确认 | `escape_label` 改为 Mermaid HTML 实体（`#quot;`/`#lt;`/`#gt;`/`#124;`，`#35;` 最先替换） |
| 17 | 确认 | 新增共享 `commands/output.rs::write_atomic`（同目录临时文件 + rename），7 处输出全部接入 |
| 18 | 确认 | `ALL_KINDS` 改用 `NodeKind::ALL` 单一事实来源，断言总数 82 |
| 19 | 确认 | 批量 upsert 改 `values_placeholders` + `execute_chunk`（满块 `prepare_cached`、尾块 `prepare`） |
| 20 | 确认 | `EvidenceKind`/`EdgeKind`/`EdgeSource`/`EdgeCertainty`/`EdgeStatus` 增加 `ALL` + `from_str`，store 解析全部改用 |
| 21 | 确认 | 三个批量 upsert 包进 `with_write_tx` 事务 |
| 22 | 确认 | `search_aliases` 按 `is_method()`/`is_free_function()` 动态派生，覆盖全部语言 |
| 23 | 重新评估 | PRAGMA 全部改为逐条 best-effort 执行（受限环境下任一失败不影响其余） |
| 24 | 确认 | 兜底转义提取为 `escape_json_string`（引号/反斜杠/控制字符），保证单行有效 JSON |
| 25 | 确认 | `shutdown()` 在 child 已被 force-kill 后直接成功返回，不再向死管道写入 |
| 26 | 确认 | `backfill_referenced_symbols` 改 `HashMap` 索引，O(R×S) → O(R+S) |
| 27 | 确认 | 注释修正：预算实为 15s（为 sourcekit-lsp 冷启动留量），并删除不存在的常量引用 |
| 28 | 确认 | `docs_indexer` 的 `visited` 改 `BTreeSet`，三处线性查找消除 |
| 29 | 确认 | 8 个读路径改 `prepare_cached` |
| 30 | 误报 | `append_stat` 已有目录存在性守卫，不会在缺失 `--repo-root` 时写盘 |

---

## High（4 个）

### 1. HTML 报告 `sanitize_for_script` 按字节处理 UTF-8，多字节字符被损坏

- **位置**：`crates/specslice-cli/src/commands/search_html.rs:39-54`，同样问题在 `crates/specslice-cli/src/commands/graph_html.rs:44-55`
- **问题**：函数逐字节遍历 JSON 字符串，对每个字节执行 `out.push(b as char)`。对 UTF-8 多字节字符（中文 symbol 名、中文注释、中文路径），每个续字节（0x80–0xBF）会被零扩展为 U+0080–U+00FF 的 Latin-1 字符，再被 `String::push` 重新编码为 2 字节 UTF-8。结果：一个 3 字节的中文（如 `中` = `E4 B8 AD`）会被错误展开为 6 字节的乱码 `Ã¤Â¸­`，前端 `JSON.parse` 得到错误字符串，搜索/graph 报告在含任何非 ASCII 字符时即损坏。
- **触发场景**：任何含中文/日文/韩文/emoji 的 symbol 名、文档片段、路径出现在 HTML payload 中——SpecSlice 主要面向中文用户场景，触发概率极高。
- **建议**：在字节层只替换 `</` → `<\/`，其余应原样 `push_str` 整段非 ASCII 切片（或直接用 `str::replace("</", "<\\/")`）。
```rust
// search_html.rs:42-53
let bytes = raw.as_bytes();
let mut i = 0;
while i < bytes.len() {
    let b = bytes[i];
    if b == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
        out.push_str("<\\/");
        i += 2;
        continue;
    }
    out.push(b as char);  // ← 对 0x80+ 字节会损坏
    i += 1;
}
```

### 2. `search_html.rs` 客户端 JS 使用 `innerHTML` 直接拼接 graph 数据

- **位置**：`crates/specslice-cli/src/commands/search_html.rs:985-986`
- **问题**：`repaintEdgeDetail` 用 `innerHTML` 拼接 `row.edge_kind` 和 `row.neighbor_kind`，这些值来自后端 graph 数据（symbol 名 / 边类型 / 节点 label），未在前端做 HTML 转义。SpecSlice 索引源代码注释、文档片段、MyBatis SQL 文本时，若这些字符串包含 `<img onerror=...>` 等 HTML 标签，会经 JSON payload 传到前端后被 `innerHTML` 渲染执行。
- **触发场景**：被索引的代码注释或文档中包含 HTML 片段（在企业仓库、含示例 HTML 的 README、含 SQL 字符串的 mapper.xml 中常见）。
- **建议**：所有动态文本走 `textContent`，或经统一的 `escapeHtml()` 注入。
```javascript
t.innerHTML = '<b>' + row.edge_kind + '</b> · ' + row.neighbor_kind;  // ← 两字段均未转义
```

### 3. `graph_html.rs` `edgeRow` 同样的 `innerHTML` 注入

- **位置**：`crates/specslice-cli/src/commands/graph_html.rs:686-687`
- **问题**：与 #2 同类问题。`escapeText` 只用在 `e.kind` 上，`otherLabel` 虽经 `escapeText` 但整体拼接进入含 `<span>` 的 `innerHTML` 字符串，模板里只要有任一字段遗漏 `escapeText` 即引入注入点。
- **触发场景**：同 #2。
```javascript
li.innerHTML = '<span class="arrow">' + (dir === 'in' ? '◀' : '▶') + '</span> ' +
  escapeText(e.kind) + ' — ' + escapeText(otherLabel);
```

### 4. `clear_indexer_outputs` 不清理孤立 FTS 行，搜索会返回幽灵节点

- **位置**：`crates/specslice-store/src/repositories.rs:420-441`
- **问题**：函数在事务中删除给定 indexer 的节点、边、孤立证据、孤立符号范围，**但未删除 `node_fts` 中引用已删除节点的全文行**。`node_fts` 表的 `node_id` 字段是 `UNINDEXED` 且无外键约束，因此已删除节点在 FTS 表中留下幽灵条目。下一次 `fulltext_match` 搜索可能命中已不存在的节点 id，下游 `find_node` 调用返回 `None`，导致搜索结果出现"命中但无法展开"的破损引用，或在 JOIN 后静默丢弃。
- **触发场景**：增量重索引（`specslice index` 在文件已变更后重跑）。若工作流跳过全文重建（例如只重索引子集），幽灵条目永久驻留。
- **建议**：在事务内追加 `DELETE FROM node_fts WHERE node_id NOT IN (SELECT id FROM nodes)`，或改用 FTS `external-content` 表 + 触发器同步。
```rust
// repositories.rs:420-441
pub fn clear_indexer_outputs(&mut self, indexer: &str) -> StoreResult<()> {
    self.with_write_tx(|tx| {
        tx.execute("DELETE FROM nodes WHERE indexer = ?1", params![indexer])?;
        tx.execute("DELETE FROM edge_assertions WHERE indexer = ?1", params![indexer])?;
        tx.execute("DELETE FROM evidence WHERE artifact_id NOT IN (SELECT id FROM nodes)")?;
        tx.execute("DELETE FROM symbol_ranges WHERE symbol_id NOT IN (SELECT id FROM nodes)")?;
        // ← 缺：DELETE FROM node_fts WHERE node_id NOT IN (SELECT id FROM nodes)
        Ok(())
    })
}
```

---

## Medium（20 个）

### 5. Dart `update_depth` 对 `"\\"` 误判为未转义引号

- **位置**：`crates/specslice-lang-dart/src/parser.rs:675-699`
- **问题**：判断引号是否被转义仅看前一字符 `prev != '\\'`。对包含单个反斜杠的字符串字面量 `"\\"`，第二个 `"` 的 `prev` 是 `\`，于是被认为"仍在字符串内"，但实际字符串已结束。其后的 `{` / `}` 被错误计入类/方法深度计数，破坏 Dart 类作用域跟踪。
- **触发场景**：任何 Dart 文件中包含 `"\\"` 后跟大括号，例如 `var s = "\\"; if (x) { ... }` —— 该 `{` 被错误计数，类边界漂移。
- **建议**：正确扫描转义（连续反斜杠成对消除）。
```rust
// parser.rs:681-683
if ch == quote_char && prev != '\\' {
    in_string = false;
}
```
- **关联**：`crates/specslice-lang-dart/src/references.rs:369-370` 的 `strip_strings_and_comments` 有相同 bug。

### 6. Dart `parse_import` / `extract_call_arg` 不处理转义引号

- **位置**：`crates/specslice-lang-dart/src/parser.rs:744-745`，`parser.rs:772`
- **问题**：`let end = rest[1..].find(quote)?` 在第一个匹配的引号字符处停止，即使被 `\` 转义。`import 'it\'s.dart';` 会得到截断的导入路径 `it\`；`test("it\'s working", () {});` 会得到错误的测试名。
- **触发场景**：Dart 文件 import 路径或测试名包含转义引号。
```rust
// parser.rs:744-745
let end = rest[1..].find(quote)?;
Some(rest[1..1 + end].to_string())
```

### 7. Dart `scan_identifiers` 用 `bytes[i] as char` 处理多字节 UTF-8

- **位置**：`crates/specslice-lang-dart/src/references.rs:325-329`
- **问题**：`bytes[i] as char` 将每个字节零扩展为 `u32` 并解释为 Unicode 码点。对 ≥ 0x80 的 UTF-8 续字节，会产生不正确的 `char`。当前因 `is_ident_start` / `is_ident_continue` 只匹配 ASCII，不会创建虚假标识符，但若将来扩展为接受 Unicode 字母（Dart 实际允许 Unicode 标识符），会产生错误的字符匹配。
- **触发场景**：源码含中文注释紧邻标识符时，扫描器会执行 `bytes[i] as char`。
```rust
// references.rs:325-329
let c = bytes[i] as char;
if is_ident_start(c) {
    let start = i;
    while i < bytes.len() && is_ident_continue(bytes[i] as char) {
```

### 8. MCP `pump` 用 `read_line` 而非 MCP stdio 规范的 `Content-Length` 帧

- **位置**：`crates/specslice-mcp/src/server.rs:35-57`
- **问题**：MCP stdio 传输官方使用 `Content-Length` 头帧（类似 LSP）。本实现按换行分隔 JSON。当前主流客户端（Cursor、Claude Desktop）也接受换行分隔 JSON，但严格合规的 MCP 客户端发送 `Content-Length: 123\r\n\r\n{...}` 不带尾随换行——`read_line` 永远看不到完整行，服务器挂起。此外，若 JSON 体本身包含嵌入换行（在 JSON 字符串值中合法），解析器会在换行处分割并收到无效 JSON。
- **触发场景**：严格实现 MCP 规范帧的客户端连接。
```rust
// server.rs:41-56
loop {
    line.clear();
    let n = reader.read_line(&mut line)?;
```

### 9. MCP `get_subgraph` 无内存上限的 BFS

- **位置**：`crates/specslice-mcp/src/tools/get_subgraph.rs:109-160`
- **问题**：while 循环从起始节点扩展，仅受 `depth` 参数限制；`depth` 无上限校验。调用方可传 `depth: 1000000`，或即使 `depth` 适中，密集连接的图也能产生百万级节点。每次迭代分配 JSON 值并推入 `nodes_out` / `edges_out`，无任何容量限制。
- **触发场景**：MCP 客户端在大型仓库上调用 `get_subgraph` with `depth: 50`。
```rust
// get_subgraph.rs:109-110
while let Some((id, hop)) = queue.pop_front() {
    if hop >= depth { continue; }
```

### 10. `schema_indexer.rs` Java 实体解析不识别字符串字面量中的大括号

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:1615-1674`
- **问题**：`depth` 计算仅用 `line.matches('{').count() - line.matches('}').count()`。当代码行包含字符串字面量（如 `String json = "{\"key\": \"value\"}";`），字符串中的大括号也被计入深度。深度漂移后，原本位于 `depth == 1`（类体）的字段被误判为 `depth == 2`（方法体）而跳过，导致 ORM 表缺少列；或反之。
- **触发场景**：Java 实体类字段声明中包含含大括号的字符串字面量。
```rust
// schema_indexer.rs:1617-1618
let opens = i32::try_from(line.matches('{').count()).unwrap_or(i32::MAX);
let closes = i32::try_from(line.matches('}').count()).unwrap_or(i32::MAX);
// ...
depth += opens - closes;
```

### 11. `schema_indexer.rs` `balanced_parens` 不跟踪字符串字面量中的括号

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:3436-3454`
- **问题**：函数仅计算 `(` 和 `)` 的深度，不考虑它们是否在字符串字面量中。`"some (text)"` 的括号被错误计入深度，导致提前匹配或匹配失败。在路由解析中，参数常含字符串字面量（如 `@GetMapping("/users({id})")` 或 Go `mux.HandleFunc("GET /path", handler)`），不正确的括号匹配会截断参数解析或跳过有效路由。
- **触发场景**：路由注解或 Go 路由注册中，参数列表内包含含括号的字符串字面量。
```rust
// schema_indexer.rs:3436-3454
fn balanced_parens(b: &[u8], open: usize) -> Option<(&str, usize)> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => { depth -= 1; /* ... */ }
            _ => {}  // ← 字符串字面量未跟踪
        }
```

### 12. `similarity.rs` tier-2 集群缺失 pair 时 `min_sim` 保持 1.0，置信度虚高

- **位置**：`crates/specslice-engine/src/similarity.rs:384-395`
- **问题**：UnionFind 通过传递性将 A-B、B-C 合并为一个集群（A 与 B 相似，B 与 C 相似，但 A 与 C 可能 Hamming 距离超阈值未直接比较）。后续计算 `min_sim` 时，`pair_scores.get(&(lo, hi))` 对未比较过的 A-C 对返回 `None`，`min_sim` 保持为初始值 1.0。结果：集群的 `similarity_score` 可能被报告为 1.0（完全相同），即使集群中实际存在不相似成员对——这是误导性的置信度。
- **触发场景**：三个函数 A、B、C，A-B 相似度 0.86，B-C 0.87，A-C 0.70（低于 0.85 阈值）。UnionFind 合并三者，`pair_scores` 无 A-C 条目，`min_sim` 保持 1.0。
```rust
// similarity.rs:384-395
let mut min_sim: f32 = 1.0;
for a in 0..indices.len() {
    for b in (a + 1)..indices.len() {
        // ...
        if let Some(s) = pair_scores.get(&(lo, hi)) {
            if *s < min_sim { min_sim = *s; }
        }
        // 缺失 pair 时 min_sim 不变
    }
}
```

### 13. `similarity.rs` `normalize` 对未关闭的 `/*` 注释静默吞 EOF

- **位置**：`crates/specslice-engine/src/similarity.rs:614-623`
- **问题**：若源文件包含未关闭的块注释（合并冲突残余、编辑器截断），`for next in chars.by_ref()` 会消耗输入直到 EOF 而不报错。该注释后的所有代码都被当作注释跳过，规范化输出严重失真（几乎为空），指纹不匹配，导致重复代码漏检或误判。
- **触发场景**：源文件含未关闭 `/*`（合并冲突、不完整文件、半截粘贴）。
```rust
// similarity.rs:614-623
Some('*') => {
    chars.next();
    let mut prev = '\0';
    for next in chars.by_ref() {
        if prev == '*' && next == '/' { break; }
        prev = next;
    }
    continue;
}
```
- **关联**：同文件 `consume_string_literal`（697-708）对未关闭字符串字面量有相同问题。

### 14. `scip_runner.rs` `execute` 用 `cmd.output()` 缓冲全部 stdout/stderr

- **位置**：`crates/specslice-engine/src/scip_runner.rs:543-565`
- **问题**：`Command::output()` 会将子进程的 stdout/stderr 全部读入 `Vec<u8>`。SCIP 索引器（如 `scip-python`、`rust-analyzer`）在大型仓库上可能产出数百 MB 的 stderr 日志。与 `lsp_client.rs` 的管道式读取不同，这里无背压机制，大型仓库索引时可能瞬时占用数 GB 内存。
- **触发场景**：在大型仓库（django、spring-framework）上运行 `specslice index`，SCIP indexer 输出大量 stderr 时。
```rust
// scip_runner.rs:551-553
let mut cmd = Command::new(program);
cmd.args(args).current_dir(cwd);
match cmd.output() {  // ← stdout + stderr 全部读入 Vec<u8>
```

### 15. 多个命令的 `--output` 路径无 `.specslice/` 限制（非侵入式字面违规）

- **位置**：`crates/specslice-cli/src/commands/search.rs:187-205`、`dashboard.rs:31-42`、`business_doc.rs:59-61`、`propose.rs:60-63`、`impact.rs:39-51`
- **问题**：用户可通过 `--output /etc/cron.d/x` 或 `--output ../../evil.html` 将报告写到 `.specslice/` 之外的任意位置。`resolve_html_output` 对绝对路径直接放行（`if p.is_absolute() { return Ok(p.clone()); }`），对相对路径以 `repo_root` 为前缀也无法阻止 `../` 穿越。CONTRIBUTING.md 明确要求"SpecSlice must never write outside `.specslice/` in a target repo"。
- **触发场景**：`specslice search --format html --output /tmp/evil` 或 `--output ../../evil.html`。
- **建议**：要么显式文档化 `--output` 是用户授权的越界写（user-intended），要么强制路径必须在 `.specslice/export/` 内。
```rust
// search.rs:192-196
if let Some(p) = requested {
    if p.is_absolute() {
        return Ok(p.clone());  // ← 直接放行绝对路径
    }
    return Ok(repo_root.join(p));  // ← 不阻止 ../ 穿越
}
```

### 16. `graph_mermaid.rs` Mermaid label 转义不充分

- **位置**：`crates/specslice-cli/src/commands/graph_mermaid.rs:144-146`
- **问题**：`escape_label` 只处理 `"` 和换行，但 Mermaid 语法中 `[ ] ( ) { } | > < #` 等字符有特殊含义。若 symbol 名包含这些字符（如 C++ `operator==`、`fn<T>`、`array[i]`），Mermaid 图表渲染断裂或语法错误。
- **触发场景**：索引包含 C++ operator、泛型、数组操作的代码后执行 `specslice search --format mermaid`。
```rust
// graph_mermaid.rs:144-146
fn escape_label(text: &str) -> String {
    text.replace('"', "\\\"").replace('\n', " ")
}
```

### 17. 多个命令的 `write_to` 无原子性，写入失败留下半成品

- **位置**：`crates/specslice-cli/src/commands/graph.rs:199-209`、`search.rs:93-94`、`business_doc.rs:68-76`、`propose.rs:70-78`、`connect.rs:54-63`
- **问题**：所有 `write_to` 函数都用 `std::fs::write` 直接覆盖目标文件。若写入过程中磁盘满或权限错误，目标文件被截断为空或部分内容。下次打开看到空白。该 `write_to` 被至少 5 个命令共享。
- **触发场景**：磁盘空间不足时 `specslice graph --format html` 生成 0 字节的 `graph.html`。
- **建议**：write-to-temp-then-rename 模式（同目录临时文件 + 持久化 rename）。

### 18. `language_traits::ALL_KINDS` 测试矩阵未覆盖 25 种新 NodeKind

- **位置**：`crates/specslice-core/src/language_traits.rs:451-509`（测试常量）vs `crates/specslice-core/src/node.rs:192-275`（`NodeKind::ALL`）
- **问题**：测试中硬编码的 `ALL_KINDS` 切片在 `CppMethod` 处停止（共 57 项），断言 `ALL_KINDS.len() == 57`。但 `NodeKind` 枚举有 82 个变体（含 `CSharp*`、`Ruby*`、`Php*`、`Kotlin*`、`DbTable`、`SqlMapperStmt`、`HttpRoute`）。这 25 种新类型**绕过**了 `language_traits` 中的所有矩阵测试。生产函数本身正确（match 分支齐全），但测试不验证；未来若有人从 `language_of` 误删 `CSharpMethod` 分支，测试仍通过。
- **触发场景**：违反 CONTRIBUTING.md 的"测试驱动"约定，未来回归无防护。
- **建议**：测试直接遍历 `NodeKind::ALL`（已存在的 single source of truth），删除手维护的 `ALL_KINDS` 切片。
```rust
// language_traits.rs:522-529
#[test]
fn matrix_total_count_matches_known_kinds() {
    assert_eq!(
        ALL_KINDS.len(),
        57,  // ← 硬编码 57，但 NodeKind::ALL 有 82 项
        "ALL_KINDS missing a NodeKind variant..."
    );
}
```

### 19. `repositories.rs` 批量 upsert 用变化的 SQL 字符串调用 `prepare_cached`，缓存永不命中

- **位置**：`crates/specslice-store/src/repositories.rs:125-165`（`upsert_nodes_bulk`）、`168-206`（`upsert_edges_bulk`）、`300-336`（`upsert_symbol_ranges_bulk`）
- **问题**：三个批量 upsert 方法为每个 chunk 生成 SQL，`VALUES (?,..), (?,..), ...` 重复次数等于 chunk 长度。`chunks(512)` 在尾部产生 1–511 行的短块，SQL 文本因 chunk 大小而异。将不同 SQL 字符串传给 `prepare_cached`，缓存条目从不重用——每个不同的 chunk 大小都获得新的缓存条目。缓存容量 64（`lib.rs:105`），最多 512 种 SQL 形状，缓存抖动，命中率近零，抵消 `prepare_cached` 优势。
- **触发场景**：批量插入 512 行以上（spring-framework 有 84k 个符号）。尾部短块始终唯一。
- **建议**：固定 chunk 大小（短块用 NULL 填充到 512），或改用单行 prepared statement 在事务内重复 execute。

### 20. `repositories.rs` `evidence_from_row` 手写 match 而非 `EvidenceKind::from_str()`

- **位置**：`crates/specslice-store/src/repositories.rs:631-653`
- **问题**：函数用硬编码 match 将字符串映射到 `EvidenceKind` 变体。与 `node_from_row` 使用 `NodeKind::from_str()`（单一真相源）不同，证据解码器无对应 `from_str`。若向 `EvidenceKind` 添加新变体并更新 `as_str()` 但忘记更新此 match，新类型的证据行会静默解码失败，抛出"unknown evidence kind"错误。
- **触发场景**：向 `EvidenceKind` 添加新变体（如 `AiAnnotation`）后读写不一致。
```rust
// repositories.rs:633-641
let kind = match kind_str.as_str() {
    "doc_section" => EvidenceKind::DocSection,
    // ...
    other => return Err(decode_error(2, format!("unknown evidence kind {other}"))),
};
```
- **建议**：在 `specslice-core::evidence` 上实现 `FromStr`，所有解码统一引用。

### 21. `repositories.rs` 批量 upsert 内部无事务，部分失败导致不可回滚的部分提交

- **位置**：`crates/specslice-store/src/repositories.rs:125-165`、`168-206`
- **问题**：批量方法直接在 `self.conn` 上运行，依赖调用方包在 `begin_bulk`/`commit_bulk` 中。若调用方忘记，每个 chunk 在自动提交模式下独立提交。5000 节点的批量插入若在第 3 块失败，前 1024 行已提交，调用方的错误处理无法回滚已提交块，数据库留下部分批量插入。
- **触发场景**：调用方未先调用 `begin_bulk` 即批量插入。
- **建议**：方法内部检测是否已在事务中，否则自动包装。

### 22. `search_aliases` 缺少 C# / Kotlin method & function 别名

- **位置**：`crates/specslice-core/src/language_traits.rs:416-443`
- **问题**：`search_aliases` 包含 `DartMethod`、`SwiftMethod`、`GoMethod`、`PythonMethod`、`TypescriptMethod`、`JavaMethod`、`RustMethod`、`CppMethod`，但缺少 `CSharpMethod`/`CSharpFunction`。其他语言（Ruby/PHP/Kotlin）的方法和函数变体在第 429-443 行同样缺失（或仅部分）。用户搜索 `["method", "fn"]` 别名时，C# 方法节点被排除。
- **触发场景**：用户搜索 "method" 期望找到 C# 方法。
```rust
// language_traits.rs:423-430
NodeKind::DartMethod
| NodeKind::SwiftMethod
| NodeKind::GoMethod
| NodeKind::PythonMethod
| NodeKind::TypescriptMethod
| NodeKind::JavaMethod
| NodeKind::RustMethod
| NodeKind::CppMethod => &["method", "fn"],
// ← CSharpMethod 缺失；RubyMethod/PhpMethod/KotlinMethod 同样
```

### 23. `Store::open` 的 PRAGMA 批处理非原子，部分失败后连接处于次优状态

- **位置**：`crates/specslice-store/src/lib.rs:93-96`
- **问题**：`execute_batch` 将 5 个 PRAGMA 作为单个批处理运行。若其中 `mmap_size` 在 mmap 受限环境（容器、某些 macOS 配置）失败，整个批处理中止，但 `journal_mode=WAL` 和 `synchronous=NORMAL` 可能已应用，`busy_timeout`/`cache_size`/`mmap_size` 未应用。错误传播（良好），但 SQLite 连接保持打开且处于次优状态——调用方无法知道哪些 PRAGMA 已设。
- **触发场景**：在 mmap 受限环境中打开 store。
```rust
// lib.rs:93-96
conn.execute_batch(
    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;\
     PRAGMA cache_size=-65536; PRAGMA mmap_size=268435456;",
)
```
- **建议**：拆为独立 `PRAGMA` 语句，逐条执行并记录失败。

### 24. MCP `serialize` 备用 JSON 转义不完整，可能产生非法 JSON

- **位置**：`crates/specslice-mcp/src/server.rs:163-172`
- **问题**：当 `serde_json::to_string` 失败时，备用方案仅 `.replace('"', "\\\"")`。若错误消息包含 `\n`、`\r`、`\t`、`\\`，生成的字符串包含原始控制字符，违反 RFC 8259，破坏客户端 JSON 解析器。
- **触发场景**：`serde_json::to_string` 在含换行的错误消息上失败（极低概率，但可能来自引擎 I/O 错误）。
```rust
// server.rs:167-170
format!(
    r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"failed to serialise response: {}"}}}}"#,
    e.to_string().replace('"', "\\\"")  // ← 不处理 \n \r \t \\
)
```

---

## Low（6 个）

### 25. `lsp_client.rs` `shutdown()` 在 force_kill 后仍尝试写 stdin 通知

- **位置**：`crates/specslice-engine/src/lsp_client.rs:409-422`
- **问题**：`shutdown()` 先发 shutdown 请求，超时则 `read_response_for` 内部调 `force_kill`（child 已 None），随后 `shutdown()` 仍尝试 `notify("exit")` 写到已关闭的 stdin 返回 Err，再对 None child 调 `wait()`。最终 `shutdown_result` 错误被 `.context()` 包裹但被上游丢弃（`lsp_indexer.rs:298` 只取 skip reason）。逻辑不致命但冗余。
- **触发场景**：LSP 服务器卡住导致 shutdown 超时。

### 26. `dart_indexer.rs` `backfill_referenced_symbols` 对每个 reference 做线性搜索

- **位置**：`crates/specslice-engine/src/dart_indexer.rs:228-264`
- **问题**：对 `batch.references` 中每个 `from`/`to` endpoint，若不在 `present` 集合中，对 `overlay_symbols` 做 `.iter().find()` 线性搜索。reference 边可达万级，overlay_symbols 数千，O(R*S) 可达数百万次比较。
- **触发场景**：大型 Dart 项目（Flutter 电商应用）analyzer sidecar 索引。
- **建议**：构造 `HashMap<ArtifactId, &Symbol>` 索引。

### 27. `lsp_indexer.rs` warmup 总预算 15s，CI 冷启动易超时

- **位置**：`crates/specslice-engine/src/lsp_indexer.rs:630, 636`
- **问题**：`WARMUP_TOTAL_BUDGET = 15s`，`WARMUP_SLEEP = 250ms`。若 sourcekit-lsp 的 IndexStoreDB 冷启动未就绪，第一个探测文件就会消耗整个 15s 预算（外层循环 `continue` 在收到非空结果时）。
- **触发场景**：Swift 项目 CI 中首次 `specslice index`。

### 28. `docs_indexer.rs` 使用 `Vec` 做去重，大文档仓库 O(N²)

- **位置**：`crates/specslice-engine/src/docs_indexer.rs:104, 158, 185`
- **问题**：`visited: Vec<String>`，每次 `visited.iter().any(|v| v == &rel)` 是 O(N)。三次 walk 累计，文档密集型项目（如 spring-framework 470 个 `.adoc` + `*.md` + `*.rst`）总去重成本可达 O(N²)。
- **建议**：换 `HashSet<String>`。
```rust
// docs_indexer.rs:104, 158, 185
let mut visited = Vec::new();
// ...
if visited.iter().any(|v| v == &rel) { continue; }  // O(N) per file
visited.push(rel.clone());
```

### 29. `repositories.rs` 读取路径未用 `prepare_cached`，重复查询重新解析 SQL

- **位置**：`crates/specslice-store/src/repositories.rs:88-89`、`101-103`、`111-113`、`251-257`、`287-291`、`361-363`、`377-379`
- **问题**：`find_node`、`list_nodes_by_kind`、`list_all_nodes`、`query_edges`、`list_evidence_for_artifact`、`list_symbol_ranges_for_file`、`find_symbols_intersecting` 都用 `self.conn.prepare(&sql)` 而非 `prepare_cached`。每次调用重新解析 SQL。`search` 命中扇出场景下 `list_edges_from/to` 可能被调上千次。
- **建议**：读取路径同样使用 `prepare_cached`。
```rust
// repositories.rs:88-89
pub fn find_node(&self, id: &ArtifactId) -> StoreResult<Option<Node>> {
    let sql = format!("SELECT {SELECT_NODE_COLS} FROM nodes WHERE id = ?1");
    let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;  // ← 非 cached
```

### 30. `main.rs` `run()` 在不存在的 `--repo-root` 仍写入 stats

- **位置**：`crates/specslice-cli/src/main.rs:1023-1035`
- **问题**：`--repo-root` 默认 `.`，但 `run()` 不验证路径存在。`stats::append_stat` 在 `run()` 末尾无条件执行，会创建 `.specslice/stats.jsonl`——若用户打错 `--repo-root /tm`，stats 文件被写到 `/tm/.specslice/stats.jsonl`。
- **触发场景**：`specslice --repo-root /tm search "foo"` 命令失败后。
```rust
// main.rs:1023-1035 附近
let _ = specslice_engine::stats::append_stat(&repo_root.join(".specslice"), &stat);
```

---

## 审查统计

| 严重度 | 数量 |
|---|---|
| High | 4 |
| Medium | 20 |
| Low | 6 |
| **合计** | **30** |

按模块分布：

| 模块 | 问题数 |
|---|---|
| specslice-cli（HTML/输出/命令） | 7 |
| specslice-engine（schema_indexer / similarity / 数据流） | 9 |
| specslice-store（repositories / lib） | 6 |
| specslice-lang-dart | 3 |
| specslice-mcp | 3 |
| specslice-core | 2 |

**核心结论**：
- 项目工程质量整体很高（禁 unsafe、anyhow 错误传播、子进程有超时清理），未发现 Critical 级别数据损坏或远程代码执行漏洞。
- 最值得优先修复的 4 个 High 问题集中在 **HTML 渲染管道**（UTF-8 损坏 + innerHTML 注入）和 **FTS 索引一致性**（幽灵节点）——前两个影响所有中文用户的搜索/graph HTML 报告，后者影响增量重索引的搜索正确性。
- 中等问题集中在 **手写扫描器的字符串/注释 totality**（Dart parser、Java entity parser、balanced_parens、similarity normalize）和 **路径/输出安全**（--output 越界、Mermaid 转义）。
- 测试矩阵问题（#20、#24）虽不直接影响生产，但违反 CONTRIBUTING.md 的测试驱动约定，未来回归无防护。
