# SpecSlice 代码审查报告（第二批）

**审查时间**：2026-06-12
**审查范围**：crates/* 全部 src 文件（约 91,750 行 Rust 代码）
**审查方法**：5 个并行 agent 按模块分工（core+store / engine 算法 / engine 数据流 / dart+mcp / cli），主审查交叉去重并比对 issues.md 第一批 30 个问题
**与 [issues.md](issues.md) 的关系**：本文件**仅记录新发现**，所有条目均与第一批 30 个问题比对去重；编号从 31 开始。

共记录 30 个新问题。严重度分级与第一批一致：**High**（生产可触发，影响数据/安全）、**Medium**（边界条件或显著性能/设计缺陷）、**Low**（性能微优化或潜在隐患）。

> **2026-06-12 复核完毕** — 处理结果表见文末「处理结果」一节：确认修复 22、按设计 4、误报 3、已被先前修复覆盖 2。

---

## High（13 个）

### 31. 服务端路由节点与客户端 consumed 路由节点 ID 永不匹配（跨图断链）

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:962-988`、`3105-3111`
- **问题**：服务端 `http_route_node` 用原始路径 `r.path`（如 Spring `GET /api/users/{id}` → `http_route::path::GET /api/users/{id}`）。而客户端 `consumed_route_node` 用 `normalize_consumed_route_path` 把 `${id}` 折叠成 `:param`（如 `/api/users/:param`）。**两套 ID 方案不统一**——同样的语义路径（参数占位符）生成不同的 `ArtifactId`，跨图链接（`link_inline_consumed_routes`）永远命中不到，HTTP 服务端到客户端的 Calls 边缺失。
- **触发场景**：Spring `@GetMapping("/users/{id}")` + Dart 客户端 `'/users/${userId}'` —— 期望链接 `HttpRoute←Calls←DartMethod`，实际两条记录 ID 不同永远不连。
- **建议**：让 `http_route_node` 也走同一个参数占位符规范化（`{x}` / `:x` / `<x>` / `${x}` / `<int:x>` 全部折叠为 `:param`），复用 `normalize_consumed_route_path` 实现。
```rust
// schema_indexer.rs:963 当前
let id = ArtifactId::new(format!("http_route::{rel_path}::{} {}", r.verb, r.path));
// 应改为
let canon = normalize_consumed_route_path(&r.path);
let id = ArtifactId::new(format!("http_route::{rel_path}::{} {}", r.verb, canon));
```

### 32. `lsp_client::read_message` 的 `Content-Length` 无上限，可触发巨型分配 / OOM

- **位置**：`crates/specslice-engine/src/lsp_client.rs:841-875`
- **问题**：`content_length: Option<usize>` 直接 `.parse::<usize>()` 后 `vec![0u8; length]`，无任何上限校验。行为异常或恶意 LSP 服务器声明 `Content-Length: 99999999999` 会立即触发多 GB 内存分配并 OOM 杀死 `specslice` 进程（甚至整台 CI 节点）。`Content-Length: 0` 时 `read_exact(&mut [])` 返回 Ok 但 `serde_json::from_slice(&[])` 报错。
- **触发场景**：服务器在 `initialize` 时发回异常帧（部分 `gopls`/`sourcekit-lsp` 旧版本在边界情况下会发空帧或重发 `Content-Length`），导致整个索引崩溃。
- **建议**：硬上限校验。
```rust
const MAX_LSP_FRAME: usize = 64 * 1024 * 1024; // 64 MiB
let length = content_length.ok_or_else(|| anyhow!("missing Content-Length"))?;
if length == 0 || length > MAX_LSP_FRAME {
    bail!("implausible Content-Length {length}");
}
```

### 33. `scip_runner::inputs_digest` 用 mtime+length 做指纹，内容变更但 mtime/length 相同时静默跳过索引

- **位置**：`crates/specslice-engine/src/scip_runner.rs:480-523`
- **问题**：摘要 = 排序的 `(rel_path, len, mtime)` 元组。`is_skipped_module_scan_dir` 仅剪 `node_modules` / 隐藏目录；**Rust `target/` 不在 `ALWAYS_SKIP_DIRS`**，build script 生成的 `.rs` 文件污染摘要。更严重的是：构建工具链（`cargo` / `bazel` / `webpack`）常常把内容改写后保留 size 与整秒级 mtime（HFS+/FAT32 精度 1s/2s），两次 `specslice index` 之间实际源码已变但摘要相同 → **跳过重新生成** → SCIP 覆盖层基于陈旧索引，Calls/References 边缺失。
- **触发场景**：CI 上 `cargo build` 后立即 `specslice index`，触发了 build-script 重写 `OUT_DIR/foo.rs` 但 mtime 截断；下一次 `specslice index` 看到 `(target/.../foo.rs, 1234, t)` 与上次相同 → 跳过。
- **建议**：(a) 把 `target` / `build` / `dist` / `out` 加入 `ALWAYS_SKIP_DIRS`；(b) 对小文件（< 4 KiB）改读内容做哈希，否则至少用 `mtime` 的纳秒部分。

### 34. `index_repository` 全有或全无：单个 indexer 失败回滚之前所有成功的 indexer 工作

- **位置**：`crates/specslice-engine/src/index.rs:148-461`
- **问题**：`index_repository` 在第 164 行 `store.begin_bulk()` 开启单个大事务，然后顺序执行 docs → dart → swift → go → python → ts → java → rust → treesitter → scip → fulltext，每步先 `clear_indexer_outputs(indexer)` 删除旧数据再写入新数据。如果**任何一步**通过 `?` 早期返回（典型场景：`scip_runner` 子进程超时、tree-sitter parser panic、磁盘满），函数返回 `Err`，`commit_bulk()` 永不执行，连接 drop 时 SQLite 回滚整个事务——**前 7 个 indexer 成功写入的 8 万个节点全部回滚**。用户必须完整重跑（含 Dart analyzer sidecar 冷启动、SCIP 子进程、tree-sitter 全量解析）。
- **触发场景**：在大型仓库（spring-framework 18.6s、typescript 16s）上 `specslice index`，单个 indexer（如 `scip_runner`）失败；或 LSP 服务器卡住超时；或某文件因权限被拒读取。
- **建议**：每个 indexer 用独立的子事务提交自己的工作单元，单个 indexer 失败只回滚该 indexer。或捕获每个 indexer 的错误并收集到 `result.warnings`，让其他 indexer 继续执行。

### 35. `stats::append_stat` 无文件锁，并发 CLI 调用导致 jsonl 行交错损坏

- **位置**：`crates/specslice-engine/src/stats.rs:101-111`
- **问题**：`append_stat` 用 `OpenOptions::new().append(true).open()` 写入一行 JSON。POSIX `O_APPEND` 仅保证**单次 `write()` 调用**且 size ≤ `PIPE_BUF`（macOS/Linux 通常 4096）原子。`CommandStat` 含大 `BTreeMap<String, i64>` metrics 时单行轻易 > 4 KB，`f.write_all` 可能拆成多个底层 `write()` 系统调用。多个并发 `specslice` 进程（CI 流水线、watcher、用户多终端）追加同一 `.specslice/stats.jsonl` 时，两行字节交错产生畸形 JSON。`load_stats` 第 128 行 `if let Ok(stat) = ...` 静默丢弃整条记录——**统计丢失无任何告警**。
- **触发场景**：CI 并发执行 `specslice index` + `specslice search` + `specslice impact` 同时写入同一仓库的 stats.jsonl。
- **建议**：用 `fs2::FileExt::lock_exclusive` 文件锁；或写入 `.tmp` 文件后 rename（但 append 语义会丢失）；或保证 `line.len() < PIPE_BUF`。
```rust
// stats.rs:106-110
let mut f = OpenOptions::new().create(true).append(true).open(path)?;  // ← 无锁
f.write_all(line.as_bytes())  // ← 多次 write 可能交错
```

### 36. Dart 解析器不识别 `enum` / `mixin` / `extension` / `typedef` / `factory`，体被错误消费

- **位置**：`crates/specslice-lang-dart/src/parser.rs:10-12, 157-245, 710-735, 818-841`
- **问题**：模块 doc 明示 "Non-goals: typedef, mixins, enums, extension methods"，但仅是"不识别"——这些声明不会被注册为符号，**但它们带有的 `{` 仍会进入 `update_depth` 的深度计数**。后果：
  1. `enum Foo { A, B, C }` 内部的 `{` 让 `depth` 递增；enum 在 Dart 中可声明成员方法，但 `class_for_decl` 因 `class_stack` 为空时进入 `else if depth == 0` 分支会过滤掉这些方法。
  2. `factory Foo.fromJson(...) { ... }` 被 `parse_constructor` 拒绝（前缀 `factory ` 不匹配类名 `Foo`），然后落入 `parse_method`：`cleaned = "Foo"`（`take_while` 在 `.` 处停止），**同一类里多个 factory 全部坍缩到名为 `Foo` 的 method**，符号冲突。
  3. `typedef IntList = List<int>;` 中的 `=` 让 `parse_field` 误判为字段，type_token 取 `IntList`，name_token 取 `List`。
- **触发场景**：任何含 enum、mixin、extension、factory、typedef 的 Dart 文件——Flutter 项目常见结构（每个 State 类都伴随 factory，每个 model 都有 enum）。
- **建议**：在 `parse_class_header` 同级增加 enum/mixin/extension/typedef header 解析，或至少在 `update_depth` 之前用 `starts_with("enum ")` 等过滤；在 `parse_constructor` 中先 `trim_start_matches("factory ")`；在 `parse_field` 中显式拒绝以 `typedef` / `enum` / `mixin` / `extension` / `factory` 开头的行。

### 37. Dart 解析器不处理三引号字符串与字符串插值，跨行/插值 `}` 让深度计数严重失真

- **位置**：`crates/specslice-lang-dart/src/parser.rs:675-702`（`update_depth`）
- **问题**：`update_depth` 是**逐行**扫描（`for (idx, raw_line) in source.lines().enumerate()`），其内部状态 `in_string`、`quote_char` 在每行结束后**丢弃**。两个相关问题：
  1. **三引号跨行**：Dart `'''...'''` / `"""..."""` 允许跨多行，第一行进入字符串状态后行结束、状态丢失，后续两行的 `{` 与 `}` 被当作代码大括号处理，`depth` 漂移。
  2. **字符串插值 `${...}`**：Dart 单/双引号字符串支持插值 `"$var"` 与 `"${expr}"`，扫描器把整个 `"${...}"` 视为字符串跳过其中所有 `}`。但 Dart 单引号字符串内的 `(`、`{`、`;` 字面量在 `parse_method` 等函数中无预处理剥离，`var s = "foo(bar)"` 会被误判为方法声明。
- **触发场景**：Dart 源码含多行字符串字面量（JSON 模板、SQL 模板、Markdown 模板）或字符串插值——Flutter 项目中常见模式。
- **建议**：在 `parse_dart` 顶层维护跨行字符串状态机，把 `update_depth` 从"每行独立"改为"全文件 char 流扫描"；或在 `parse_method` / `parse_field` 入口先做 `strip_strings_and_comments` 预处理。
```rust
// parser.rs:675-702 现状：每行独立扫描
fn update_depth(line: &str, depth: &mut usize) {
    let mut in_string = false;  // ← 状态在每行重置
    // …
}
```

### 38. MCP `dispatch` 把"缺少 `id` 的请求"误判为 notification，违反 JSON-RPC 2.0

- **位置**：`crates/specslice-mcp/src/server.rs:61-80`，`crates/specslice-mcp/src/protocol.rs:36-53`
- **问题**：JSON-RPC 2.0 §4.1/§4.2 区分 Request（必须有 `id`）与 Notification（必须没有 `id`）。当前实现把"缺少 `id`"等价于"notification"，对二者**都不返回响应**。但若客户端发送一个意图为请求但忘记带 `id` 的消息（`{ "jsonrpc": "2.0", "method": "tools/list" }`），服务器静默丢弃，客户端永远等不到响应，连接挂起。

  更严重的合规问题：JSON-RPC 规范规定 `id: null` 时仍视为 Request，但 `Request::id` 用 `Option<Value>`，`Some(Value::Null)` 与缺失 `id` 通过 `serde(default)` 都映射到 `None`——`"id": null` 也被当作 notification 处理。同时**未校验 `jsonrpc == "2.0"`**，客户端发 `"jsonrpc": "1.0"` 也能被正常处理。
- **触发场景**：严格 JSON-RPC 客户端、用 `null` 作为 id 的客户端、客户端代码生成器忘记 emit `id` 字段时。
- **建议**：(1) 用 serde deserializer 区分"字段缺失"与"`null`"；(2) 对 `method` 非 notification 类型但 `id` 缺失返回 `INVALID_REQUEST`；(3) 在 `dispatch` 入口校验 `request.jsonrpc == "2.0"`。
```rust
// protocol.rs:36-53 现状
#[serde(default)]
pub id: Option<Value>,  // ← 字段缺失与 null 不可区分
pub fn is_notification(&self) -> bool {
    self.id.is_none()  // ← "id: null" 与缺 id 同样返回 true
}
```

### 39. `graph_html.rs` 多字段未转义注入 `innerHTML`（与第一批 #3 不同字段）

- **位置**：`crates/specslice-cli/src/commands/graph_html.rs:528, 551`
- **问题**：第一批 #3 记录了 `edgeRow` 的 `otherLabel` 未充分转义，但**同一文件其他渲染路径**仍未转义。`n.kind`（节点类型）、`e.layer`（边层级）等字段直接拼接进 `innerHTML`。SpecSlice 索引源代码注释、文档片段、MyBatis SQL 文本时，若这些字符串包含 `<img onerror=...>` 等 HTML 标签，会经 JSON payload 传到前端后被 `innerHTML` 渲染执行。
- **触发场景**：被索引的代码注释或文档中包含 HTML 片段（企业仓库、含示例 HTML 的 README、含 SQL 字符串的 mapper.xml）。
- **建议**：所有动态文本统一走 `textContent` 或 `escapeHtml()` 注入，建立"不允许任何 innerHTML 拼接未转义字段"的 lint 规则。

### 40. `dashboard` HTML 导出文件泄露宿主机绝对路径

- **位置**：`crates/specslice-cli/src/commands/dashboard.rs:99` 附近
- **问题**：导出的 HTML dashboard 把宿主机的绝对仓库路径（如 `/Users/qjs/Code/Projects/specslice/`）嵌入到 HTML payload 中（可能是 stats 引用、文件链接、源代码片段）。用户分享 dashboard（贴 issue、上传 CI artifact、发送给同事）即**无意泄露内部目录结构、用户名、组织结构**。这违反了 CONTRIBUTING.md "SpecSlice must never write outside `.specslice/`" 的非侵入精神外延——非侵入不仅指写入，也应包括"不外泄宿主信息"。
- **触发场景**：CI artifact 上传 dashboard HTML 到公开链；用户贴 dashboard 截图/源码到公开 issue。
- **建议**：在所有面向 HTML 的输出中只使用相对路径或仓库根 relative 形式（`./src/foo.rs`）；提供 `--redact-paths` 选项。

### 41. `clear_indexer_outputs` 不清理 `indexer IS NULL` 的节点（与第一批 #4 不同维度）

- **位置**：`crates/specslice-store/src/repositories.rs:444-478`
- **问题**：第一批 #4 记录的是 FTS 表的幽灵行；本条关注 nodes/edges 表的 `NULL` indexer 语义。删除条件 `DELETE FROM nodes WHERE indexer = ?1` 在 SQL 中遇到 `NULL` indexer 时永远不匹配——`NULL = 'dart_lightweight'` 求值为 `NULL`（不是 TRUE）。然而 `EdgeAssertion::declared` / `fact` 工厂默认 `indexer: None`，写入数据库即 `NULL`。结果：通过工厂构建但未显式设 indexer 的节点/边**永远无法被 `clear_indexer_outputs` 清除**。重索引时这些行残留，与"重置某 indexer 的输出"的语义相违。
- **触发场景**：任何用 `Node::new` / `EdgeAssertion::declared` 工厂构建但未显式设 indexer 的写入路径（包括 dart_lightweight 等历史 adapter），加上一次 `clear_indexer_outputs(same_name)` 后期望清理——结果残留所有未指派 indexer 的节点。
- **建议**：要么将 `indexer` 列改为 `NOT NULL DEFAULT '__unmanaged__'`；要么删除条件改为 `WHERE indexer IS ?1` 或 `WHERE indexer = ?1 OR indexer IS NULL`；或语义上明确"NULL indexer 表示未托管 → 应被任何 clear 清除"。
```rust
// repositories.rs:449
tx.execute("DELETE FROM nodes WHERE indexer = ?1", params![indexer])
//   ← indexer IS NULL 的行永远不被删除（NULL = ?1 → NULL ≠ TRUE）

// edge.rs:204-205 工厂创建的节点：
indexer: None,  // ← 写入即 NULL
```

### 42. `NodeKind::language()` 前缀匹配脆弱，且与 `language_of()` 对 `db_table`/`http_route` 给出不一致结果

- **位置**：`crates/specslice-core/src/node.rs:288-315` vs `crates/specslice-core/src/language_traits.rs:246`
- **问题**：函数把 `as_str()` 拆为语言前缀 + `_` + rest。但 `csharp_*` 前缀中含子串 `c`——目前因为列表里 `csharp` 在 `c` 之前而侥幸正确。任何新增 `cobra_*` / `c_factory_*` 这种字母前缀相互包含的语言都会被静默错配。更直接的是：`db_table` / `sql_mapper_stmt` / `http_route` 在 `NodeKind::language()` 返回 `None`，但 `language_traits.rs::language_of()` 中 `HttpRoute => Language::Synthetic`，`DbTable / SqlMapperStmt => Language::Generic`。**两个 API 对同一 NodeKind 给出语义不同结果**，调用方一旦混用即出现"看到 None 误判为非代码节点"的逻辑分叉。
- **触发场景**：UI 图例 / 搜索路由调用 `kind.language()`（方法）期待返回 `Some("http")` 或 `Some("sql")`，结果返回 `None`；而 `language_of(kind)`（自由函数）认为它们是 `Synthetic` / `Generic`。两个并存 API 必然被新代码误用。
- **建议**：要么删除 `NodeKind::language()` 让所有调用走 `language_of`（消除二义性）；要么补齐 `db_table` / `sql_mapper_stmt` / `http_route` 分支让两个 API 一致。前缀匹配改为 `(lang, "_")` 锚定完整 token。
```rust
// node.rs:309-314
.find(|lang| {
    s.strip_prefix(lang)
        .and_then(|r| r.strip_prefix('_'))  // ← 仅靠顺序保证 csharp≠c，脆弱
        .is_some()
})
// language_traits.rs:246
NodeKind::Route | NodeKind::Storage | NodeKind::HttpRoute => Language::Synthetic,
// ↑ NodeKind::HttpRoute.language() = None，但 language_of() = Synthetic — 不一致
```

### 43. `expand_subgraph` 无 depth 上限，用户 `--depth` 可触发无界图遍历（CLI 入口）

- **位置**：`crates/specslice-engine/src/search.rs:1738-1786`，配合 `crates/specslice-cli/src/commands/search.rs:67`（`depth: args.depth`）
- **问题**：第一批 #9 记录的是 MCP `get_subgraph` 的无界 BFS；本条关注**更广触发面**的 CLI 入口。`expand_subgraph` 用 `for _ in 0..depth` 做 BFS 扩展，每跳对 frontier 中每个 id 调用 `store.list_edges_from` 和 `store.list_edges_to`（两次 DB 往返）。`depth` 完全由 CLI 参数 `--depth` 决定，无任何上限校验。在密集图（spring-framework 84k 节点）上 `--depth 5` 即可访问百万级节点 × 2 次 DB 查询，产生分钟级延迟和 GB 级 `kept_edges: Vec` 内存增长。
- **触发场景**：`specslice search "foo" --depth 10` 在大型仓库上执行——比 MCP 调用更易触发（任何 CLI 用户即可）。
- **建议**：硬上限 `depth.min(MAX_DEPTH)`（如 3）；或为 `node_ids` / `kept_edges` 设硬容量上限，超出后停止扩展并 push 一条 `warnings`。
```rust
// search.rs:1750
for _ in 0..depth {  // ← depth 用户可控，无上限
    let mut next: BTreeSet<String> = BTreeSet::new();
    for id in &frontier {
        // 每个节点两次 DB 往返，所有边 push 进 kept_edges（无去重容量）
```

---

## Medium（16 个）

### 44. 路径参数形式（`{id}` / `:id` / `<id:int>` / `${id}` / `%s`）未做统一规范化

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:3334-3349`（`normalize_route`）、`3353-3360`（`route_search_name`）、`3105-3111`（`normalize_consumed_route_path`）
- **问题**：Spring `/users/{id}`、Gin `/users/:id`、ASP.NET `/users/{id:int}`、Flask `/users/<int:id>`、FastAPI `/users/{id:int}`、TS `${id}`、C printf `%s` 各自保留原形式。`route_search_name` 只跳过 `{var}` 开头段，对 `:id` / `<id>` 返回 `:id` 作为节点 `name`——搜索 "getUserById" 无法匹配。多语言混合仓库里同一接口的两个客户端调用可能产生两个互不合并的节点。
- **触发场景**：一个仓库 Spring 端 `GET /u/{id}` + Gin 端转发 `GET /u/:id`：两次索引产出两个 `HttpRoute` 节点，`name` 分别为 `{id}` 与 `:id`。
- **建议**：在 `normalize_route` 末尾或专用函数里把每个段里的参数占位符折叠为 `:param`。

### 45. FastAPI / Flask 的 `APIRouter(prefix=...)` / `Blueprint(url_prefix=...)` 前缀完全未传播

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:2152-2192`（`parse_python_routes`）
- **问题**：仅 Java 类级 `@RequestMapping` 与 Go `Gin.Group()` 解析前缀，Python 的 `APIRouter(prefix="/api/v1", ...)` / `Blueprint(url_prefix=...)` / `app = FastAPI(root_path=...)` 都被忽略。`@router.get("/users")` 直接索引为 `/users`，而实际服务路径是 `/api/v1/users`。
- **触发场景**：FastAPI 项目 `r = APIRouter(prefix="/api/v1"); @r.get("/users")` → 索引路径 `/users`，与客户端 `/api/v1/users` 不匹配，跨图链接失败。
- **建议**：仿照 `collect_gin_group_prefixes` 写一个 `collect_python_router_prefixes`，扫描 `APIRouter(prefix=...)` / `Blueprint(url_prefix=...)` 赋值。

### 46. `skip_python_string` 三引号关闭条件 `i + 2 < b.len()` 在文件末尾少读一字节

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:2228-2244`
- **问题**：第 2230 行检测开三引号用 `start + 2 < b.len()`（严格小于），第 2238 行检测关三引号同样 `i + 2 < b.len()`。当关闭的 `"""` 正好位于文件末尾（即 `i == b.len() - 3`）时，`i + 2 < b.len()` 为假，**关闭符不被识别**，循环跑到 `b.len()` 后返回，`python_decorator_offsets` 把 docstring 后面的代码当作仍在字符串里跳过 —— 一段代码末尾的 docstring 后所有 `@decorator` 都被丢弃，**漏索引真实路由**。
- **触发场景**：Python 文件以三引号字符串结尾（如模块级 `"""..."""`），或文件末尾的 docstring 之后还有 import 但没 `@`。
- **建议**：把严格 `<` 改为 `<=`（即 `i + 3 <= b.len()`），并加边界检查。
```rust
if b[i] == q
    && i + 2 < b.len()              // 旧：少一字节
    && b[i + 1] == q && b[i + 2] == q {
// 改为：
if i + 3 <= b.len() && b[i] == q && b[i + 1] == q && b[i + 2] == q {
```

### 47. `similarity::normalize` 未折叠 Unicode 标识符，破坏结构指纹一致性

- **位置**：`crates/specslice-engine/src/similarity.rs:651-668, 720-731`（`consume_identifier`）
- **问题**：`consume_identifier` 只接受 ASCII alphanumeric + `_`。遇到 Rust/Swift/Kotlin/Dart 的 Unicode XID 标识符（如 `用户_count`、`α_β`、`count用户`）时，`用`/`户` 走 fallback `out.push(c.to_string())`，每个非 ASCII 字符成为独立单字符 token。结果：`def 用户():` 标准化为 `[def, 用, 户, (, )]`，与 `def 其他():` 结构相同（因为 `用`/`户`/`其`/`他` 都是独立 token）；`用户_id` 与 `_id`（无 Unicode）标准化的 token 数不同，结构对比失真。**手写扫描器必须 total 且确定性的基线被违反**。
- **触发场景**：源码含中文/日文/韩文/希腊字母标识符——SpecSlice 中文用户场景常见。
- **建议**：把 `consume_identifier` 的字符判据改为 `c.is_alphanumeric() || c == '_'`（Unicode-aware），然后像 ASCII 标识符一样折叠为 `ID`。

### 48. `dart_sidecar::try_run` 用 `wait_with_output` 全量缓冲 stdout，无超时；解析单条记录失败则丢弃整批

- **位置**：`crates/specslice-engine/src/dart_sidecar.rs:138-187`（`try_run`）、`304-355`（`parse_response`）
- **问题**：
  1. `child.wait_with_output()` 一次性把 sidecar 全部 stdout 读入 `Vec<u8>`，大型 Flutter 仓库（如 Flutter gallery，50k+ 符号）可能产出数百 MB JSON，内存峰值极高。
  2. **无超时控制**：sidecar 死锁（如 Dart analyzer 等待 stdin EOF 但 specslice 已 `drop(stdin)` 后又出错）会让 `wait_with_output` 永远阻塞，整条 `specslice index` 卡死。
  3. `parse_response` 在循环中遇到单条记录反序列化失败（如 sidecar 升级后字段不兼容）立即返回 `Skipped` —— 已成功解析的 9999 条记录被全部丢弃，违反部分恢复原则。
- **触发场景**：sidecar 版本不匹配，第 5 条 symbol 字段新增 `deprecated` 字段，前 4 条已 push 到 batch，第 5 条 `from_value` 报错 → 整批丢弃 → Dart 索引"全部缺失"而非"95% 缺失"。
- **建议**：(a) 用带超时的 `wait` + 流式读 stdout；(b) `parse_response` 对单条失败计入 warning 计数并 `continue`。
```rust
for v in response.symbols {
    match serde_json::from_value(v) {
        Ok(s) => batch.symbols.push(s),
        Err(_) => { invalid_count += 1; continue; } // 不再整体返回
    }
}
```

### 49. `concat_string_literals` 对 Go 字符串转义 `\xNN`/`\uXXXX`/`\n` 误还原为原始字符

- **位置**：`crates/specslice-engine/src/schema_indexer.rs:2419-2450`
- **问题**：第 2428-2431 行：遇到 `\` 就把"下一个字节"作为字符 push。对于 Go 路由 `mux.HandleFunc("GET /api\\n", h)`（路径里包含字面反斜杠+n）会被还原成 `/apin`，丢失反斜杠。对 `中`（中文字面）只取 `u` 字符。`\x2f`（斜杠转义）只取 `x`。结果：路由路径被篡改，节点 ID 与实际不匹配，跨图链接失败。
- **触发场景**：Go 测试 fixture 路由 `"GET /v\\x2f1"` → 实际路径 `/v/x1`，索引成 `/v x1`（取 `\` 后的 `x`）。
- **建议**：要么如实保留反斜杠+下一字节，要么真正解码 Go 字符串转义；简化方案是不做转义还原，遇到 `\` 直接跳两字节不 push。
```rust
b'\\' => { j += 2; continue; } // 跳过转义序列但不污染输出
```

### 50. `simple_glob_match` 递归回溯，对 `**/**/foo/**` 类病态模式呈指数级耗时

- **位置**：`crates/specslice-engine/src/lsp_indexer.rs:915-971`（被 `treesitter.rs:1946-1950` 的 `discover_files` exclude 过滤调用）
- **问题**：`simple_glob_match` 在遇到 `**` 时对 `txt[ti..]` 的**每个后缀**递归调用 `glob_match_rec`。当模式含多个连续 `**`（如用户在 `.specslice.yaml` 的 `code.exclude` 写 `**/**/foo/**`），每个 `**` 都枚举所有切点并递归，组合爆炸——对一条长路径（如 `a/b/c/d/e/f/g.swift`）匹配耗时可达指数级。`discover_files` 对**每个发现的源文件** × **每条 exclude glob** 调用此函数，一条病态 glob 即可让 `specslice index` 在 84k 文件仓库上耗时数十分钟甚至卡死。
- **触发场景**：用户在 `.specslice.yaml` 配置含多个 `**` 的 exclude glob（合法且常见，如 `**/generated/**`）。
- **建议**：改用 `globset` crate（项目已用，见 `port_coverage.rs:25`、`dead_code.rs:37`）替代手写匹配器；或对连续 `*` 折叠为单个 `*`/`**`；或将 `**` 转换为 NFA 而非回溯。
```rust
// lsp_indexer.rs:934-938
for j in ti..=txt.len() {
    if glob_match_rec(pat, next, txt, j) {  // ← 对每个 j 全递归
        return true;
    }
}
```

### 51. `attach_snippets` 直接 `repo_root.join(&m.path)` 读文件，无路径遍历校验、无文件大小上限

- **位置**：`crates/specslice-engine/src/search.rs:576-625`（具体在 590-594）
- **问题**：`attach_snippets` 把数据库 `nodes.path` 字段直接拼到 `repo_root`，然后 `std::fs::read_to_string`。三种风险：
  1. **非 UTF-8 文件**：`read_to_string` 在 Latin-1/GBK 源文件上返回 `Err`，整个 match 的 snippet 静默丢失（`let Some(lines) = lines else { continue }`）。
  2. **路径遍历**：若数据库被人工编辑或 indexer bug 导致 `path = "../../../../etc/passwd"`，`repo_root.join` 不阻止 `..`，会读到仓库外的文件（虽然只读，但违反非侵入精神，snippet 内容会泄露到搜索结果）。
  3. **超大文件**：`read_to_string` 把整个文件读入内存。一个被索引的 50MB 自动生成 `.g.dart` 文件会让单次 `attach_snippets` 分配 50MB `String` + `Vec<String>`（每行一个 String）。
- **触发场景**：仓库包含自动生成的大文件（`*.g.dart`、`*.pb.go`），或非 UTF-8 源文件。
- **建议**：先 `metadata` 校验大小（< 1MB 才读）；用 `fs::read` + `String::from_utf8_lossy` 容忍非 UTF-8；校验规范化后的路径仍在 `repo_root` 之下（`canonicalize` + `starts_with`）。
```rust
// search.rs:590-594
let lines = cache.entry(path.clone()).or_insert_with(|| {
    std::fs::read_to_string(repo_root.join(&path))  // ← 无大小检查、无路径校验
        .ok()
        .map(|c| c.lines().map(String::from).collect())
});
```

### 52. `tokenise_keywords` 用 `is_alphanumeric()` 分词，CJK 标识符产 huge tokens，结构层中文搜索严重受限

- **位置**：`crates/specslice-engine/src/search.rs:1261-1274`
- **问题**：`tokenise_keywords` 按 `!c.is_alphanumeric() && c != '_'` 分割。Unicode `is_alphanumeric` 包括所有 CJK 字符，所以中文查询 `"用户登录服务"` 被当作**单个 token** 而非四个词。后续 `keyword_matches` → `score_node` 用该 token 对 node.name/id 做 `contains` 子串匹配。意味着只有名字里**完整连续**包含 `用户登录服务` 的节点才命中；用户输入 `登录 服务` 反而匹配不到 `用户登录服务模块` 节点。FTS 内容层用 `fts_query_tokens`（CJK bigram）补救了正文匹配，但**结构层**（node 名字/id/path）的中文搜索严重受限。
- **触发场景**：用户用中文关键词搜索中文项目（SpecSlice 主要面向中文用户）。
- **建议**：`tokenise_keywords` 在 ASCII 字符处分割后，对 CJK run 进一步用 bigram 切分（复用 `fts_text::fts_tokens` 或独立 CJK 分词）。

### 53. `slugify` 对全非 ASCII 字符串返回 `"section"`，导致中文/日文章节 ID 全部冲突

- **位置**：`crates/specslice-core/src/artifact_id.rs:93-113`
- **问题**：当输入文本不含任何 ASCII 字母数字（例如纯中文标题 `"自动水印放置"`），输出回退为 `"section"`。多个中文 doc section 的 slug 全部塌缩为 `"section"`，于是 `doc_section_id("docs/a.md", "section")` 对所有中文小节返回相同 ID `docsec::docs/a.md#section`。这破坏了 ArtifactId 的"确定性 + 唯一性"约定：所有中文小节在 `nodes` 表里互相覆盖（`ON CONFLICT(id) DO UPDATE`），FTS 搜索只能找到最后一个。
- **触发场景**：任何纯中文 / 纯日文 / 纯韩文 / 纯 emoji 标题——SpecSlice 主打中文用户场景，触发概率极高。
- **建议**：fallback 不要用固定字符串，而是用输入的 hash（如 FNV-1a 64 位 hex）或保留原字符的 NFC 形式。
```rust
// artifact_id.rs:108-112
if out.is_empty() {
    "section".to_string()  // ← 多个不同中文标题都映射到此，ID 冲突
} else {
    out
}
```

### 54. `rebuild_fulltext` 用未缓存 `prepare` + 一次一行 INSERT，大型 ingest 数秒浪费

- **位置**：`crates/specslice-store/src/repositories.rs:530-548`
- **问题**：(1) `tx.prepare("INSERT INTO node_fts ...")` 在事务内手动准备，与同模块其他写入路径明确强调的 `prepare_cached` 基线相违。(2) 循环中一次一行 `execute`，而非 multi-row VALUES；84k 行等于 84k 次 VDBE dispatch。对比 `upsert_nodes_bulk` 已用 512 行 chunk，此处是显著的退化点。
- **触发场景**：每次 `specslice index` 全量重建 FTS——对 django (96k symbols) / spring (84k) 这类大仓，单次 index 多花数秒在 dispatch 上。
- **建议**：用 `prepare_cached` + 512-行 VALUES chunk，与 `upsert_nodes_bulk` 对齐。
```rust
// repositories.rs:535-545
let mut stmt = tx
    .prepare("INSERT INTO node_fts (node_id, body) VALUES (?1, ?2)")  // ← 非 cached
    .map_err(StoreError::sqlite)?;
for row in rows {
    if row.body.trim().is_empty() { continue; }
    stmt.execute(params![row.node_id, row.body])  // ← 一次一行，84k+ dispatch
        .map_err(StoreError::sqlite)?;
}
```

### 55. `commit_bulk` 后未恢复 `wal_autocheckpoint` 的失败语义：异常路径残留 0

- **位置**：`crates/specslice-store/src/lib.rs:176-185`
- **问题**：`commit_bulk` 执行 `COMMIT; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=1000;` 一连串语句在单个 `execute_batch` 中。如果 `wal_checkpoint(TRUNCATE)` 因任何原因失败（如只读文件系统、磁盘满），`execute_batch` 整体返回错误，但 `COMMIT` 已经执行（SQLite `execute_batch` 是顺序执行，COMMIT 不可撤销）。后果：事务已提交但 `wal_autocheckpoint=1000` 没有恢复（停留在 `begin_bulk` 设的 `0`），后续连接的所有 WAL 累积直到下次显式 checkpoint，**WAL 文件无界增长**。
- **触发场景**：CI 上 WAL 体积监控 / 长跑 daemon / 磁盘空间紧张时 checkpoint 失败但 commit 已完成——非常隐蔽，WAL 静默膨胀。
- **建议**：把 `COMMIT` 单独 `execute_batch`，再单独执行 PRAGMA，PRAGMA 失败时仅记日志不影响 commit 结果；或在 commit 前先把 `wal_autocheckpoint` 重置为 1000，再 commit，再 checkpoint。
```rust
// lib.rs:179-182
self.conn.execute_batch(
    "COMMIT; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=1000;",
    // ← 若 checkpoint 失败：commit 已生效但 autocheckpoint 永久为 0
)
```

### 56. Dart `strip_strings_and_comments` 不处理块注释 `/* ... */`，块注释内标识符被当真实引用产生假边

- **位置**：`crates/specslice-lang-dart/src/references.rs:361-394`
- **问题**：函数只识别**行注释** `//`（line 379-381），完全不处理 **块注释** `/* ... */`。结果：`/* IapProductIds is used here */` 中的 `IapProductIds` 不被剥离，`scan_identifiers` 把它当作真实引用、查到符号索引中存在的 `IapProductIds` 类后，**发出一条虚假的 `references` 边**。`compute_references` 在所有方法体上跑——任何含块注释提及类名/方法名的源文件都会产生假边。
- **触发场景**：源码方法体中含块注释提及类名（很常见——版权头、解释性注释、被注释掉的代码片段）。
- **建议**：在 `strip_strings_and_comments` 中跟踪 `in_block_comment` 状态（同时处理块注释跨多行的情况——需要在外层维护状态）。
```rust
// references.rs:379-381 现状
if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'//' {
    break;  // ← 仅处理 //，未处理 /*
}
```

### 57. MCP 不支持 JSON-RPC 批量请求 `[ {...}, {...} ]`，违反规范

- **位置**：`crates/specslice-mcp/src/server.rs:35-57, 61-80`
- **问题**：JSON-RPC 2.0 规范 §6 明确允许**批量请求**——客户端可发送一个 JSON 数组 `[{...}, {...}]`，服务器应答一个数组。当前 `dispatch` 用 `serde_json::from_str::<Request>(raw)` 解析，遇到数组会直接失败并返回 PARSE_ERROR（id 为 null）——客户端永远收不到批量内任何子请求的响应。即使主流 MCP 客户端（Cursor、Claude Desktop）当前不使用批量，规范合规性仍是 SpecSlice 这种"提供标准 MCP 服务"的工具应当满足的；支持批量请求的客户端连接后整个会话无法启动。
- **触发场景**：合规的 JSON-RPC 客户端、或并发优化客户端使用批量提交多个 `tools/call`。
- **建议**：在 `pump` 中先尝试解析为 `Vec<Request>`，回退到单 `Request`；批量输入按"对每个子项调用 `dispatch`，聚合非 None 结果为数组"。

### 58. `--json` 隐式 flag 静默覆盖 `--format`，对运维脚本是常见陷阱

- **位置**：`crates/specslice-cli/src/main.rs:1103-1108, 1227-1248`
- **问题**：分发逻辑执行 `let format = if args.json { SearchFormat::Json } else { args.format.into_command_format() };`——因此 `specslice search foo --format html --json` 会静默切换到 JSON 输出（忽略 HTML 请求）。`--json` 和 `--format` 都有 clap 文档字符串，但都没声明排他性，clap group 验证未连接。`--format text --json` 也会静默生成 JSON。同样的模式在 `ImpactArgs`（`main.rs:1103-1108`）重复。运维人员传 `--json` 期望"详细模式"却得到机器输出，反之亦然——CI 脚本静默错误。
- **触发场景**：CI 脚本逐步演化，先加 `--format text`，后来加 `--json` 调试，二者同时存在即出错。
- **建议**：用 `#[arg(conflicts_with = "format")]` 设置互斥；或保留 `--json` 兼容但发出 deprecation 警告。
```rust
if args.json && !matches!(args.format, SearchFormatArg::Json) {
    eprintln!("warn: --json overrides --format {}; dropping --json in a future release", ...);
}
```

### 59. `propose` Markdown 输出 `mermaid_id` 未充分 sanitize，模块 id 含 `/`/`.` 会破坏 Mermaid 语法

- **位置**：`crates/specslice-cli/src/commands/propose.rs:106-139, 176-180`
- **问题**：`mermaid_id(&m.id)` 当前实现（如有）可能未充分 sanitize，若 `m.id` 来自目录路径含 `/`（如 `"lib_v2/auth"`）、`.`、空格或特殊字符，斜杠会原封不动落入 Mermaid 节点 id `lib_v2/auth`，破坏 Mermaid 解析。同时，AI 提示词被包裹在 ` ```text ` / ` ``` ` 中（第 178-180 行），若 `pack.prompt` 本身包含 ` ``` `（自定义提示词或多行字符串），Markdown 解析器会过早关闭外部代码块并破坏文档其余部分。引擎通过模板构建 `pack.prompt`，目前安全，但**未强制不变性**。
- **触发场景**：业务模块 id 来源于目录路径（如 `lib_v2/auth`）；或扩展 pack.prompt 模板时引入反引号。
- **建议**：把 `mermaid_id` 严格 sanitize 为 `[A-Za-z0-9_]+`；用四反引号围栏 ` ```` ` 作为外部块以容忍内部三反引号。
```rust
fn mermaid_id(slug: &str) -> String {
    let mut out = String::new();
    for ch in slug.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' { out.push(ch); } else { out.push('_'); }
    }
    out
}
```

---

## Low（1 个）

### 60. Dart `disambiguate_duplicate_test_ids` 是 O(N²)，大型 test 套件性能退化

- **位置**：`crates/specslice-lang-dart/src/parser.rs:572-594`
- **问题**：函数对 `tests` 数组中的每个元素，再次遍历整个 `tests` 数组做去重计数（`tests.iter().filter(... candidate ...).count()`）。`tests.len()` 在大型 Flutter 测试套件中可达数千（一个 `*_test.dart` 文件常含 200+ `test(...)`，单批 index 处理上百个测试文件 → 数万 test），总复杂度 O(N²) 可达 10⁸–10⁹ 级。此外每个 idx 的 `slug` 都被重复计算（外层 `slugify` 与内层 `slugify(&candidate.name)` 多次计算同一字符串）。
- **触发场景**：批量索引一个含数百个测试文件的大型 Flutter 仓库。
- **建议**：预计算一次 `HashMap<(TestKind, path, slug), count>`，再单次遍历做重命名。
```rust
// parser.rs:573-582
for idx in 0..tests.len() {
    let slug = slugify(&tests[idx].name);
    let duplicate_count = tests
        .iter()
        .filter(|candidate| {  // ← O(N) 内嵌在 O(N) 循环里
            // …
        })
        .count();
```

---

## 审查统计

| 严重度 | 数量 |
|---|---|
| High | 13 |
| Medium | 16 |
| Low | 1 |
| **合计** | **30** |

按模块分布：

| 模块 | 问题数 |
|---|---|
| specslice-engine（schema_indexer / 路由 / scip_runner / lsp_client / search / dart_sidecar / index / stats） | 16 |
| specslice-store（repositories / lib / artifact_id） | 5 |
| specslice-lang-dart（parser / references） | 4 |
| specslice-cli（commands / main） | 4 |
| specslice-mcp（server / protocol） | 2 |
| specslice-core（node / language_traits） | 2 |

按主题聚类：

| 主题 | 涉及条目 |
|---|---|
| **路由/HTTP 跨图链接正确性**（ID 规范化、参数折叠、前缀传播） | #31、#44、#45、#49 |
| **手写扫描器的字符串/注释边界**（三引号、插值、块注释、Unicode 标识符） | #37、#46、#47、#56 |
| **资源耗尽 / DoS**（Content-Length、glob 回溯、attach_snippets、depth BFS） | #32、#43、#50、#51 |
| **事务/索引一致性**（NULL indexer、checkpoint、FTS 重建、全有或全无） | #33、#34、#41、#54、#55 |
| **协议合规**（JSON-RPC id/batch、MCP） | #38、#57 |
| **中文用户场景**（slugify 塌缩、CJK 分词、Unicode 标识符） | #47、#52、#53 |
| **CLI/UX 陷阱**（--json 覆盖、mermaid_id、dashboard 泄露、stats 并发） | #35、#39、#40、#58、#59 |

**核心结论（与第一批 issues.md 互补）**：

- **最值得优先修复**的 5 个：#31（路由跨图断链，破坏 P0 索引价值）、#34（全有或全无事务，单 indexer 失败导致分钟级回滚 + 全量重跑）、#35（stats 并发损坏，CI 流水线数据完整性）、#36（Dart enum/mixin/factory 不识别，Flutter 项目几乎必触发）、#38（MCP 协议合规，严格客户端无法启动会话）。

- **路由处理是系统性的弱点**：第一批的 #10/#11（schema_indexer Java entity / balanced_parens 字符串字面量）+ 本批 #31/#44/#45/#49 揭示了**整个跨语言 HTTP 路由索引管道**存在多个独立缺陷——服务端 ID 与客户端 ID 不匹配（#31）、参数占位符 5 种形式未规范化（#44）、Python router 前缀未传播（#45）、Go 字符串转义误还原（#49）。这 4 个问题任意一个都会让 HTTP 端到端 Calls 边断裂，建议作为一个 epic 统一修复并加 e2e 测试。

- **手写扫描器的 totality 是项目基线**（CONTRIBUTING.md 明确要求），本批 #37/#46/#47/#56 加上第一批 #5/#6/#7/#13 显示**至少 8 处独立扫描器存在字符串/注释/Unicode 边界缺陷**。建议建立一个集中的 `scanner_proptest` 套件，对所有手写扫描器统一跑"任意 UTF-8 输入不 panic + 确定性"property test。

- **中文用户场景**：第一批的 #1（HTML UTF-8 损坏）+ 本批 #47（Unicode 标识符指纹失真）、#52（CJK 分词）、#53（slug 塌缩）显示**面向中文用户的核心管道仍有多处缺陷**——SpecSlice 文档主打中文场景，这些应是 P1 修复优先级。

- **协议合规**：第一批的 #8（MCP 帧格式）+ 本批 #38（id 处理）、#57（批量请求）显示 **MCP 实现距离规范合规还有明显距离**。如果 SpecSlice 想被严格 MCP 客户端（不只是 Cursor/Claude Desktop）使用，建议对照 MCP spec 系统性补齐。


---

## 处理结果（2026-06-12 复核）

每个问题先经源码复核，确认存在的按 TDD（先失败测试、后修复）处理；全量 `cargo test --workspace` 与 `cargo clippy --all-targets` 通过。

| # | 结论 | 处理 |
|---|------|------|
| 31 | 确认 | `parse_dart_route_constants` 输出统一过 `normalize_consumed_route_path`；`is_param_segment` 识别 `$`/`<` 形参段，服务端/客户端 `route_key` 对齐 |
| 32 | 确认 | `read_message` 增加 `MAX_FRAME_BYTES`（256 MB）上限，超限 `bail!` 不分配，新增不分配回归测试 |
| 33 | 按设计 | mtime+length 是经典 make 式权衡；doc 注释明确两类边界（touch 无害重跑 / 同长同 mtime 可能陈旧）与 `--force` 逃生门 |
| 34 | 按设计 | 全有或全无优先保证图的"同代一致性"；`index_repository` doc 注释写明取舍（混代图比重跑更危险） |
| 35 | 确认 | `append_stat` 增加 `File::lock` 排它锁（写完随句柄释放）；新增 8 线程 × 64 KB 行并发测试；MSRV 1.87→1.89 |
| 36 | 确认 | `parse_constructor` 剥离 `factory` 前缀，`parse_method` 排除 `factory `；工厂构造器注册为独立构造器节点 |
| 37 | 确认 | 新增 `ScanState` 跨行追踪三引号字符串；`update_depth` 在多行字符串内不计大括号，字符串内"声明"不再被解析 |
| 38 | 部分成立 | 增加 `jsonrpc != "2.0"` → `INVALID_REQUEST` 校验；"缺 id 即 notification"本就符合 JSON-RPC §4.1（任何 method 都可为通知）；`id:null` 因 serde 无法与缺失区分、按通知处理，已在注释声明限制 |
| 39 | 已覆盖 | 复核确认第一批 #3 的修复已覆盖全部 `innerHTML` 注入点（`escapeAttr(e.layer)`、`escapeAttr(classes)`、`escapeText` 详情/统计行） |
| 40 | 确认 | dashboard `meta` 只嵌仓库名（`repo`），不再嵌绝对路径；`section()` 折叠错误时把 `repo_root` 前缀替换为 `<repo>`，防错误链泄露宿主路径 |
| 41 | 按设计 | `indexer IS NULL` 即"无主数据"（人工确认链、外部导入），任何 indexer 重跑都不得删除；doc 注释写明契约 |
| 42 | 误报 | `language()` 前缀匹配以 `_` 锚定（`dart_` 不会误配 `dartx_`）；`language()` 与 `language_of()` 语义本不同（节点种类语言 vs 路径推断） |
| 43 | 已修复 | 与"继续审查"阶段发现重合：`expand_subgraph` 已有 `SUBGRAPH_NODE_BUDGET` 预算 + `truncated` 标志，CLI `--depth` 无法触发无界遍历 |
| 44 | 确认 | `{id}` / `:id` / `<int:id>` / `${id}` / `$id` 统一规范化为 `:param`；`route_key` 折叠全部形参段 |
| 45 | 确认 | 新增 `collect_python_router_prefixes`：`APIRouter(prefix=...)` / `Blueprint(url_prefix=...)` 前缀传播到所属装饰器路由 |
| 46 | 误报 | `i + 2 < b.len()` 是"闭合后还有内容才继续扫"的正确条件；新增 EOF 紧邻 docstring 的回归测试钉死行为 |
| 47 | 确认 | `normalize` / `consume_identifier` 改 `is_alphanumeric()`，CJK/Unicode 标识符与 ASCII 一样折叠为 `ID` token |
| 48 | 确认 | `try_run` 改双线程排水 + `try_wait` 轮询 + 墙钟预算（默认 600 s，`SPECSLICE_DART_ANALYZER_TIMEOUT_SECS` 可调），超时 kill；`parse_response` 改逐行恢复，坏行计数进 diagnostics 不再丢整批 |
| 49 | 确认 | `concat_string_literals` 改按 `char` 迭代（UTF-8 安全）；仅解码 `\"`/`\\`，其余转义原样保留（grep 可达性优先于"还原"） |
| 50 | 确认 | `simple_glob_match` 增加失败状态 memoization，病态 `**/**/…` 模式从指数级降为多项式，新增性能断言测试 |
| 51 | 确认 | 新增 `read_snippet_lines`：拒绝绝对路径/`..` 穿越、2 MB 大小上限、`from_utf8_lossy` 容错非 UTF-8 |
| 52 | 确认 | `tokenise_keywords` 对非 ASCII 串改用重叠 bigram（与 FTS 层一致），中文关键词可命中结构层名称/ID |
| 53 | 确认 | `slugify` 空槽时回退 `s<fnv1a64>` 内容哈希，纯中文/日文标题 ID 不再全部塌缩为 `section` |
| 54 | 大体误报 | 单事务一次重建本就正确；顺手把 INSERT 改 `prepare_cached` |
| 55 | 确认 | `COMMIT` 单独执行；`wal_checkpoint` / `wal_autocheckpoint=1000` 改为 best-effort（失败不再连累已成功的事务），新增状态恢复测试 |
| 56 | 确认 | 新增 `StripState` 跨行追踪块注释（含嵌套）与三引号字符串；块注释里的标识符不再产生假边 |
| 57 | 确认 | `dispatch` 对 `[` 开头的批量数组直接返回 `INVALID_REQUEST` + 明确错误文案（MCP 规范已移除 batch），不再抛笼统 parse error |
| 58 | 确认 | `search` / `impact` / `candidate show` 三处 `--json` 加 `conflicts_with = "format"`：显式同传硬报错，单独使用保持兼容 |
| 59 | 确认 | `mermaid_id` 严格 sanitize 为 `[A-Za-z0-9_]`（有损折叠时附 FNV 短哈希防 `lib/auth` 与 `lib.auth` 合并）；AI 提示词围栏改四反引号容忍内部 ``` |
| 60 | 确认 | `disambiguate_duplicate_test_ids` 改两遍 O(N)：先 `HashMap` 计数 slug，再单遍重命名 |

**统计**：确认修复 22，按设计/政策澄清 4（#33 #34 #41 +#54 顺手优化），误报 3（#42 #46 #54），已被先前修复覆盖 2（#39 #43）。
